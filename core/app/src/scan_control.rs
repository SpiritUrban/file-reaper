//! Керування сесією скану: старт / стоп / прогрес (T-033).
//!
//! architecture.md §1.2 / §14: команда лише підтверджує прийняття;
//! прогрес і завершення — подіями. Скасування кооперативне
//! ([`CancellationToken`]); DoD: стоп ≤ 500 мс, частковий індекс валідний.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use trashradar_domain::candidate::FileRecord;
use trashradar_domain::error::CoreError;
use trashradar_domain::scan::ScanStrategy;

use crate::workers::CancellationToken;

/// DoD T-033: скасування має завершитись не пізніше цього інтервалу.
pub const CANCEL_DEADLINE: Duration = Duration::from_millis(500);

/// Підсумок скану одного тому.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeScanOutcome {
    pub files_indexed: u64,
    pub cancelled: bool,
}

/// Прогрес для події `scan.progress`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanProgress {
    pub volume: char,
    pub strategy: ScanStrategy,
    pub phase: ScanProgressPhase,
    pub files_indexed: u64,
    pub volume_index: u32,
    pub volume_count: u32,
    pub done: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanProgressPhase {
    /// Початок тому.
    VolumeStarted,
    /// Проміжне оновлення (батчі).
    VolumeProgress,
    /// Том завершено (або скасовано на ньому).
    VolumeFinished,
    /// Уся сесія завершена.
    SessionFinished,
}

impl ScanProgressPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            ScanProgressPhase::VolumeStarted => "volume_started",
            ScanProgressPhase::VolumeProgress => "volume_progress",
            ScanProgressPhase::VolumeFinished => "volume_finished",
            ScanProgressPhase::SessionFinished => "session_finished",
        }
    }
}

/// Сканер одного тому з кооперативною відміною (реалізації: MFT/walk/mock).
pub trait CancellableVolumeScanner: Send {
    fn scan_volume(
        &self,
        volume: char,
        cancel: &CancellationToken,
        on_batch: &mut dyn FnMut(Vec<FileRecord>) -> Result<(), CoreError>,
    ) -> Result<VolumeScanOutcome, CoreError>;
}

/// Стан сесії скану (потокобезпечний контролер).
#[derive(Debug, Default)]
pub struct ScanController {
    running: AtomicBool,
    token: Mutex<Option<CancellationToken>>,
}

impl ScanController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Почати сесію: повертає токен для фонового воркера.
    /// Якщо вже running — `invalid_argument`.
    pub fn begin(&self) -> Result<CancellationToken, CoreError> {
        if self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(CoreError::invalid_argument(
                "Сканування вже виконується. Зупиніть поточне або зачекайте завершення.",
            ));
        }
        let token = CancellationToken::new();
        *self.token.lock().expect("scan controller mutex poisoned") = Some(token.clone());
        Ok(token)
    }

    /// Запит скасування (кооперативний).
    pub fn request_cancel(&self) -> bool {
        let guard = self.token.lock().expect("scan controller mutex poisoned");
        if let Some(t) = guard.as_ref() {
            t.cancel();
            true
        } else {
            false
        }
    }

    /// Завершити сесію (після join воркера).
    pub fn end(&self) {
        *self.token.lock().expect("scan controller mutex poisoned") = None;
        self.running.store(false, Ordering::Release);
    }
}

/// Підсумок усієї сесії.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSessionSummary {
    pub volumes_completed: u32,
    pub files_indexed: u64,
    pub cancelled: bool,
}

/// Проганяє томи послідовно; між батчами/томами перевіряє cancel.
///
/// `on_progress` — для емісії `scan.progress`.
/// `on_batch` — запис у HotIndex (частковий результат лишається при cancel).
pub fn run_scan_session(
    volumes: &[char],
    strategies: &[ScanStrategy],
    scanner: &impl CancellableVolumeScanner,
    cancel: &CancellationToken,
    mut on_progress: impl FnMut(ScanProgress),
    mut on_batch: impl FnMut(char, Vec<FileRecord>) -> Result<(), CoreError>,
) -> Result<ScanSessionSummary, CoreError> {
    assert_eq!(
        volumes.len(),
        strategies.len(),
        "volumes and strategies length mismatch"
    );
    let volume_count = volumes.len() as u32;
    let mut total_files = 0u64;
    let mut volumes_completed = 0u32;
    let mut session_cancelled = false;

    for (i, (&volume, &strategy)) in volumes.iter().zip(strategies.iter()).enumerate() {
        if cancel.is_cancelled() {
            session_cancelled = true;
            break;
        }
        let volume_index = i as u32;
        on_progress(ScanProgress {
            volume,
            strategy,
            phase: ScanProgressPhase::VolumeStarted,
            files_indexed: 0,
            volume_index,
            volume_count,
            done: false,
            cancelled: false,
        });

        let mut volume_files = 0u64;
        let mut batch_sink = |batch: Vec<FileRecord>| -> Result<(), CoreError> {
            let n = batch.len() as u64;
            volume_files = volume_files.saturating_add(n);
            total_files = total_files.saturating_add(n);
            on_batch(volume, batch)?;
            on_progress(ScanProgress {
                volume,
                strategy,
                phase: ScanProgressPhase::VolumeProgress,
                files_indexed: volume_files,
                volume_index,
                volume_count,
                done: false,
                cancelled: cancel.is_cancelled(),
            });
            Ok(())
        };

        let outcome = scanner.scan_volume(volume, cancel, &mut batch_sink)?;
        // Якщо сканер порахував файли без батчів — узгодити total.
        if outcome.files_indexed > volume_files {
            total_files = total_files
                .saturating_sub(volume_files)
                .saturating_add(outcome.files_indexed);
            volume_files = outcome.files_indexed;
        }

        let cancelled_here = outcome.cancelled || cancel.is_cancelled();
        on_progress(ScanProgress {
            volume,
            strategy,
            phase: ScanProgressPhase::VolumeFinished,
            files_indexed: volume_files,
            volume_index,
            volume_count,
            done: false,
            cancelled: cancelled_here,
        });

        if cancelled_here {
            session_cancelled = true;
            break;
        }
        volumes_completed = volumes_completed.saturating_add(1);
    }

    on_progress(ScanProgress {
        volume: volumes.first().copied().unwrap_or('?'),
        strategy: strategies
            .first()
            .copied()
            .unwrap_or(ScanStrategy::DirectoryWalk),
        phase: ScanProgressPhase::SessionFinished,
        files_indexed: total_files,
        volume_index: volume_count.saturating_sub(1),
        volume_count,
        done: true,
        cancelled: session_cancelled,
    });

    Ok(ScanSessionSummary {
        volumes_completed,
        files_indexed: total_files,
        cancelled: session_cancelled,
    })
}

/// Тестовий сканер: кілька кроків із затримкою; cancel між кроками (DoD ≤500 мс).
pub struct SteppedTestScanner {
    pub steps: usize,
    pub step_delay: Duration,
    pub files_per_step: usize,
}

impl Default for SteppedTestScanner {
    fn default() -> Self {
        Self {
            steps: 20,
            step_delay: Duration::from_millis(100),
            files_per_step: 10,
        }
    }
}

impl CancellableVolumeScanner for SteppedTestScanner {
    fn scan_volume(
        &self,
        volume: char,
        cancel: &CancellationToken,
        on_batch: &mut dyn FnMut(Vec<FileRecord>) -> Result<(), CoreError>,
    ) -> Result<VolumeScanOutcome, CoreError> {
        let mut files_indexed = 0u64;
        for step in 0..self.steps {
            if cancel.is_cancelled() {
                return Ok(VolumeScanOutcome {
                    files_indexed,
                    cancelled: true,
                });
            }
            let mut batch = Vec::with_capacity(self.files_per_step);
            for i in 0..self.files_per_step {
                let id = files_indexed + i as u64;
                batch.push(test_record(id, volume, step, i));
            }
            files_indexed += batch.len() as u64;
            on_batch(batch)?;
            if !self.step_delay.is_zero() {
                std::thread::sleep(self.step_delay);
            }
        }
        Ok(VolumeScanOutcome {
            files_indexed,
            cancelled: false,
        })
    }
}

fn test_record(id: u64, volume: char, step: usize, i: usize) -> FileRecord {
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, SafetyLevel,
    };
    use trashradar_domain::category::CategoryId;
    FileRecord {
        candidate_id: CandidateId(id),
        path: format!("{}:\\scan\\s{step}\\f{i}.bin", volume.to_ascii_uppercase()),
        size: ByteSize(id.saturating_add(1) * 1024),
        created_at: None,
        modified_at: None,
        accessed_at: None,
        kind: FileKind::Other,
        unit: CandidateUnit::File,
        category: CategoryId::Uncategorized,
        safety: SafetyLevel::ReviewRecommended,
        decision: Decision::Undecided,
        detector_id: String::new(),
        explanation: String::new(),
        attributes: FileAttributes::default(),
    }
}

/// Вимірює час від `request_cancel` до виходу `scan_volume` (для тестів DoD).
pub fn measure_cancel_latency(
    scanner: &impl CancellableVolumeScanner,
    cancel_after: Duration,
) -> Result<(Duration, VolumeScanOutcome), CoreError> {
    let token = CancellationToken::new();
    let token_cancel = token.clone();
    let start_flag = Arc::new(AtomicBool::new(false));
    let start_flag2 = Arc::clone(&start_flag);

    let join = std::thread::spawn(move || {
        while !start_flag2.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        std::thread::sleep(cancel_after);
        let t0 = Instant::now();
        token_cancel.cancel();
        t0
    });

    start_flag.store(true, Ordering::Release);
    let mut sink = |_: Vec<FileRecord>| Ok(());
    let outcome = scanner.scan_volume('T', &token, &mut sink)?;
    let t0 = join.join().expect("cancel thread");
    let latency = t0.elapsed();
    Ok((latency, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn controller_begin_twice_fails() {
        let c = ScanController::new();
        assert!(c.begin().is_ok());
        assert!(c.is_running());
        assert!(c.begin().is_err());
        c.request_cancel();
        c.end();
        assert!(!c.is_running());
        assert!(c.begin().is_ok());
        c.end();
    }

    #[test]
    fn cancel_stops_within_deadline() {
        let scanner = SteppedTestScanner {
            steps: 50,
            step_delay: Duration::from_millis(50),
            files_per_step: 5,
        };
        let (latency, outcome) =
            measure_cancel_latency(&scanner, Duration::from_millis(30)).expect("run");
        assert!(outcome.cancelled, "scan must observe cancel");
        assert!(
            latency <= CANCEL_DEADLINE,
            "cancel latency {latency:?} > {CANCEL_DEADLINE:?}"
        );
        // Частковий результат: щось уже проіндексовано до cancel.
        assert!(outcome.files_indexed > 0);
    }

    #[test]
    fn session_keeps_partial_batches_on_cancel() {
        let scanner = SteppedTestScanner {
            steps: 10,
            step_delay: Duration::from_millis(40),
            files_per_step: 3,
        };
        let token = CancellationToken::new();
        let token2 = token.clone();
        let stored: Arc<Mutex<Vec<FileRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let stored2 = Arc::clone(&stored);

        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(70));
            token2.cancel();
        });

        let summary = run_scan_session(
            &['C'],
            &[ScanStrategy::DirectoryWalk],
            &scanner,
            &token,
            |_| {},
            |_, batch| {
                stored2.lock().unwrap().extend(batch);
                Ok(())
            },
        )
        .expect("session");

        assert!(summary.cancelled);
        let n = stored.lock().unwrap().len();
        assert!(n > 0, "partial batches must remain");
        assert_eq!(summary.files_indexed as usize, n);
    }

    #[test]
    fn full_session_without_cancel() {
        let scanner = SteppedTestScanner {
            steps: 2,
            step_delay: Duration::ZERO,
            files_per_step: 4,
        };
        let token = CancellationToken::new();
        let mut progress_phases = Vec::new();
        let summary = run_scan_session(
            &['C', 'D'],
            &[ScanStrategy::Mft, ScanStrategy::DirectoryWalk],
            &scanner,
            &token,
            |p| progress_phases.push(p.phase),
            |_, _| Ok(()),
        )
        .expect("session");
        assert!(!summary.cancelled);
        assert_eq!(summary.volumes_completed, 2);
        assert_eq!(summary.files_indexed, 16); // 2 volumes * 2 steps * 4
        assert!(progress_phases.contains(&ScanProgressPhase::SessionFinished));
    }
}
