//! Ліміт одночасних читань **на фізичний том** (T-063, architecture.md §4).
//!
//! Два томи (C: і D:) хешуються паралельно; на одному томі — не більше
//! [`DEFAULT_MAX_CONCURRENT_READS_PER_VOLUME`] (стелі).

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use trashradar_domain::error::CoreError;

use crate::workers::CancellationToken;

/// Дефолтна стеля одночасних disk-read на один том (SSD/HDD shared bus).
pub const DEFAULT_MAX_CONCURRENT_READS_PER_VOLUME: usize = 2;

/// Невідомий / мережевий / UNC шлях — окремий «віртуальний» слот.
pub const UNKNOWN_VOLUME: char = '?';

/// Витягти літеру тому з Windows-шляху.
///
/// - `C:\Users\…` → `C`
/// - `\\?\C:\…` → `C`
/// - `\\server\share` / relative → [`UNKNOWN_VOLUME`]
pub fn volume_from_path(path: &str) -> char {
    let p = path.trim();
    // \\?\C:\...
    if let Some(rest) = p.strip_prefix(r"\\?\") {
        return volume_from_drive_prefix(rest);
    }
    // \\.\C:\...
    if let Some(rest) = p.strip_prefix(r"\\.\") {
        return volume_from_drive_prefix(rest);
    }
    volume_from_drive_prefix(p)
}

fn volume_from_drive_prefix(p: &str) -> char {
    let bytes = p.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        (bytes[0] as char).to_ascii_uppercase()
    } else {
        UNKNOWN_VOLUME
    }
}

#[derive(Debug)]
struct VolumeState {
    active: usize,
    peak: usize,
}

/// Семафор «на том»: acquire блокує, поки active < max на цій літері.
#[derive(Debug)]
pub struct VolumeIoGate {
    max_per_volume: usize,
    inner: Mutex<HashMap<char, VolumeState>>,
    cvar: Condvar,
}

/// RAII-дозвіл: Drop зменшує active і будить очікувачів.
#[derive(Debug)]
pub struct VolumeIoPermit {
    gate: Arc<VolumeIoGate>,
    volume: char,
}

impl VolumeIoGate {
    pub fn new(max_per_volume: usize) -> Arc<Self> {
        Arc::new(Self {
            max_per_volume: max_per_volume.max(1),
            inner: Mutex::new(HashMap::new()),
            cvar: Condvar::new(),
        })
    }

    pub fn with_default_limit() -> Arc<Self> {
        Self::new(DEFAULT_MAX_CONCURRENT_READS_PER_VOLUME)
    }

    pub fn max_per_volume(&self) -> usize {
        self.max_per_volume
    }

    /// Захопити слот для `volume`. Скасування → `CoreError::cancelled`.
    pub fn acquire(
        self: &Arc<Self>,
        volume: char,
        cancel: &CancellationToken,
    ) -> Result<VolumeIoPermit, CoreError> {
        let mut guard = self.inner.lock().expect("volume gate mutex");
        loop {
            if cancel.is_cancelled() {
                return Err(CoreError::cancelled("volume_io_acquire"));
            }
            let state = guard
                .entry(volume)
                .or_insert(VolumeState { active: 0, peak: 0 });
            if state.active < self.max_per_volume {
                state.active += 1;
                if state.active > state.peak {
                    state.peak = state.active;
                }
                return Ok(VolumeIoPermit {
                    gate: Arc::clone(self),
                    volume,
                });
            }
            // Чекаємо звільнення; poll cancel через короткий timeout.
            let (g, _) = self
                .cvar
                .wait_timeout(guard, Duration::from_millis(50))
                .expect("volume gate condvar");
            guard = g;
        }
    }

    fn release(&self, volume: char) {
        let mut guard = self.inner.lock().expect("volume gate mutex");
        if let Some(state) = guard.get_mut(&volume) {
            state.active = state.active.saturating_sub(1);
        }
        drop(guard);
        self.cvar.notify_all();
    }

    /// Пікове active для тому (тести / метрики).
    pub fn peak_for(&self, volume: char) -> usize {
        self.inner
            .lock()
            .expect("volume gate mutex")
            .get(&volume)
            .map(|s| s.peak)
            .unwrap_or(0)
    }

    /// Поточне active (тести).
    pub fn active_for(&self, volume: char) -> usize {
        self.inner
            .lock()
            .expect("volume gate mutex")
            .get(&volume)
            .map(|s| s.active)
            .unwrap_or(0)
    }

    /// Скинути peak/active (тести).
    pub fn reset_stats(&self) {
        let mut guard = self.inner.lock().expect("volume gate mutex");
        for state in guard.values_mut() {
            state.peak = state.active;
        }
    }
}

impl Drop for VolumeIoPermit {
    fn drop(&mut self) {
        self.gate.release(self.volume);
    }
}

/// Захопити gate за шляхом файла (volume з path).
pub fn acquire_for_path(
    gate: &Arc<VolumeIoGate>,
    path: &str,
    cancel: &CancellationToken,
) -> Result<VolumeIoPermit, CoreError> {
    gate.acquire(volume_from_path(path), cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Instant;

    #[test]
    fn volume_from_path_drive_letters() {
        assert_eq!(volume_from_path(r"C:\Users\a"), 'C');
        assert_eq!(volume_from_path(r"d:/x"), 'D');
        assert_eq!(volume_from_path(r"\\?\E:\foo"), 'E');
        assert_eq!(volume_from_path(r"\\server\share"), UNKNOWN_VOLUME);
        assert_eq!(volume_from_path("relative"), UNKNOWN_VOLUME);
    }

    #[test]
    fn same_volume_respects_ceiling() {
        let gate = VolumeIoGate::new(2);
        let cancel = CancellationToken::new();
        let max_seen = Arc::new(AtomicUsize::new(0));
        let started = Instant::now();

        thread::scope(|s| {
            for _ in 0..6 {
                let g = Arc::clone(&gate);
                let m = Arc::clone(&max_seen);
                let c = cancel.clone();
                s.spawn(move || {
                    let _p = g.acquire('C', &c).unwrap();
                    let cur = g.active_for('C');
                    m.fetch_max(cur, Ordering::Relaxed);
                    thread::sleep(Duration::from_millis(30));
                });
            }
        });

        assert!(
            started.elapsed() >= Duration::from_millis(80),
            "with limit 2 and 6 jobs, wall time should stack"
        );
        assert!(
            max_seen.load(Ordering::Relaxed) <= 2,
            "peak concurrent on C: must be ≤ 2"
        );
        assert_eq!(gate.peak_for('C'), 2);
        assert_eq!(gate.active_for('C'), 0);
    }

    #[test]
    fn two_volumes_run_in_parallel() {
        // Limit 1 per volume → C and D each hold 1 at once = 2 total concurrent.
        let gate = VolumeIoGate::new(1);
        let cancel = CancellationToken::new();
        let both_held = Arc::new(AtomicUsize::new(0));
        let barrier_c = Arc::new(AtomicUsize::new(0));
        let barrier_d = Arc::new(AtomicUsize::new(0));

        thread::scope(|s| {
            let g = Arc::clone(&gate);
            let both = Arc::clone(&both_held);
            let bc = Arc::clone(&barrier_c);
            let bd = Arc::clone(&barrier_d);
            let c1 = cancel.clone();
            s.spawn(move || {
                let _p = g.acquire('C', &c1).unwrap();
                bc.store(1, Ordering::Release);
                // Wait until D also holds.
                while bd.load(Ordering::Acquire) == 0 {
                    thread::sleep(Duration::from_millis(1));
                }
                both.fetch_add(1, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(20));
            });
            let g = Arc::clone(&gate);
            let both = Arc::clone(&both_held);
            let bc = Arc::clone(&barrier_c);
            let bd = Arc::clone(&barrier_d);
            let c2 = cancel.clone();
            s.spawn(move || {
                let _p = g.acquire('D', &c2).unwrap();
                bd.store(1, Ordering::Release);
                while bc.load(Ordering::Acquire) == 0 {
                    thread::sleep(Duration::from_millis(1));
                }
                both.fetch_add(1, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(20));
            });
        });

        // Обидва томи одночасно утримували permit.
        assert_eq!(both_held.load(Ordering::Relaxed), 2);
        assert_eq!(gate.peak_for('C'), 1);
        assert_eq!(gate.peak_for('D'), 1);
    }

    #[test]
    fn cancel_while_waiting() {
        let gate = VolumeIoGate::new(1);
        let cancel = CancellationToken::new();
        let _hold = gate.acquire('F', &cancel).unwrap();
        let cancel2 = CancellationToken::new();
        cancel2.cancel();
        let err = gate.acquire('F', &cancel2).unwrap_err();
        assert_eq!(err.code, trashradar_domain::error::ErrorCode::Cancelled);
    }
}
