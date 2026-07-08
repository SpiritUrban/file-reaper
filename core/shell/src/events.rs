//! Канал подій Core→UI (T-005).
//!
//! Правила:
//! - топіки — лише з реєстру [`topic`] (дзеркало contracts/ →
//!   ui/src/ipc/types.ts EventName); довільні рядки заборонені;
//! - Core надсилає події через [`emit`] — єдину точку з логуванням;
//!   збій доставки не валить викликача (UI міг ще не піднятися);
//! - підписки й відписки — на боці UI (ui/src/ipc/client.ts
//!   subscribe → UnlistenFn); Rust-підписники використовуються
//!   у тестах для верифікації шини.
//!
//! Тротлінг агрегованих подій — окрема задача T-006.

use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Runtime};

static EVENTS_EMITTED: AtomicU64 = AtomicU64::new(0);
static EVENT_ERRORS: AtomicU64 = AtomicU64::new(0);

/// Реєстр топіків подій (канонічні контрактні імена). Новий топік =
/// константа тут + запис у contracts/ipc-contract.json + EventName
/// в ui/src/ipc/types.ts.
pub mod topic {
    /// Діагностичний потік (команда `app.test_stream`).
    pub const APP_TEST: &str = "app.test";
    pub const APP_TEST_COUNTER: &str = "app.test_counter";
    /// USN journal застарів → авто-фолбек на повний скан (T-031).
    /// Використовується оркестратором скану (T-033) через [`emit_journal_stale`].
    pub const SCAN_JOURNAL_STALE: &str = "scan.journal_stale";
}

/// Payload події `scan.journal_stale` (T-031).
///
/// Оркестратор T-033 викликає [`emit_journal_stale`]; до того API
/// тримається публічним для тестів і майбутнього wiring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // emit path: T-033 scan orchestrator
pub struct JournalStaleEvent {
    /// Напр. `"C:"`.
    pub volume: String,
    /// Machine code: `journal_id_changed`, `usn_below_lowest_valid`, …
    pub reason: String,
    /// Людське пояснення (українською).
    pub message: String,
    /// Завжди true для цієї події — UI може показати «повне сканування…».
    pub full_rescan: bool,
}

impl JournalStaleEvent {
    #[allow(dead_code)] // emit path: T-033
    pub fn from_request(req: &trashradar_app::usn_fallback::FullRescanRequest) -> Self {
        Self {
            volume: req.event_volume_label(),
            reason: req.reason_code.to_string(),
            message: req.message.clone(),
            full_rescan: true,
        }
    }
}

/// Емісія пояснення про фолбек на повний скан (T-031 DoD: подія-пояснення).
///
/// Викликається оркестратором після [`trashradar_app::usn_fallback::process_usn_sync`]
/// → `FullRescanRequired` (підключення в T-033).
#[allow(dead_code)] // wired by T-033 scan orchestrator
pub fn emit_journal_stale<R: Runtime>(
    app: &AppHandle<R>,
    req: &trashradar_app::usn_fallback::FullRescanRequest,
) {
    let payload = JournalStaleEvent::from_request(req);
    emit(app, topic::SCAN_JOURNAL_STALE, &payload);
    tracing::warn!(
        volume = %payload.volume,
        reason = %payload.reason,
        message = %payload.message,
        "USN journal stale — full rescan required"
    );
}

/// Реєстр реалізованих scan-подій (для health / smoke; тримає API «живим» до T-033).
pub fn scan_event_topics() -> &'static [&'static str] {
    &[topic::SCAN_JOURNAL_STALE]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventMetrics {
    pub emitted: u64,
    pub failed: u64,
}

pub fn metrics() -> EventMetrics {
    EventMetrics {
        emitted: EVENTS_EMITTED.load(Ordering::Relaxed),
        failed: EVENT_ERRORS.load(Ordering::Relaxed),
    }
}

/// Aggregated counter payload for throttled Core -> UI updates.
///
/// `delta` is the accumulated change since the previous emission. `total` is
/// the full counter value observed by Core, so throttling does not lose the
/// summary even when many low-level events are collapsed into one UI event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CounterSnapshot {
    pub delta: u64,
    pub total: u64,
}

/// Coalesces high-frequency counter deltas before they reach the event bus.
#[derive(Debug, Clone)]
pub struct CounterThrottle {
    min_interval: Duration,
    last_emit_at: Instant,
    pending_delta: u64,
    total: u64,
}

impl CounterThrottle {
    pub fn new_at(min_interval: Duration, started_at: Instant) -> Self {
        Self {
            min_interval,
            last_emit_at: started_at,
            pending_delta: 0,
            total: 0,
        }
    }

    pub fn observe(&mut self, now: Instant, delta: u64) -> Option<CounterSnapshot> {
        self.pending_delta = self.pending_delta.saturating_add(delta);
        self.total = self.total.saturating_add(delta);

        if self.pending_delta > 0 && now.duration_since(self.last_emit_at) >= self.min_interval {
            return self.take_snapshot(now);
        }

        None
    }

    pub fn flush(&mut self, now: Instant) -> Option<CounterSnapshot> {
        if self.pending_delta == 0 {
            return None;
        }

        self.take_snapshot(now)
    }

    fn take_snapshot(&mut self, now: Instant) -> Option<CounterSnapshot> {
        let delta = std::mem::take(&mut self.pending_delta);
        self.last_emit_at = now;
        Some(CounterSnapshot {
            delta,
            total: self.total,
        })
    }
}

/// Контрактне ім'я → дротове ім'я шини Tauri.
///
/// Tauri забороняє крапки в іменах подій (IllegalEventName), тому на
/// дроті `app.test` стає `app:test`. Дзеркальне перетворення — в
/// ui/src/ipc/client.ts (subscribe). Скрізь у коді й контракті
/// фігурують лише канонічні імена з крапками.
pub fn wire_name(topic: &str) -> String {
    topic.replace('.', ":")
}

/// Надсилає подію всім підписникам топіка.
///
/// Помилка доставки логується і НЕ повертається: емісія подій —
/// fire-and-forget за контрактом (architecture.md §1.2), відправник
/// не має залежати від стану webview.
pub fn emit<R: Runtime, T: Serialize + Clone>(app: &AppHandle<R>, topic: &str, payload: &T) {
    match app.emit(&wire_name(topic), payload.clone()) {
        Ok(()) => {
            EVENTS_EMITTED.fetch_add(1, Ordering::Relaxed);
            tracing::trace!(topic, "подію надіслано")
        }
        Err(error) => {
            EVENT_ERRORS.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(topic, %error, "не вдалося надіслати подію")
        }
    }
}

#[cfg(test)]
mod journal_stale_tests {
    use super::*;
    use trashradar_app::usn_fallback::FullRescanRequest;
    use trashradar_domain::scan::{FullRescanReason, UsnJournalInfo};

    #[test]
    fn journal_stale_event_from_request() {
        let req = FullRescanRequest {
            volume: 'D',
            reason: FullRescanReason::JournalIdChanged,
            reason_code: "journal_id_changed",
            message: "test message".into(),
            journal: UsnJournalInfo {
                journal_id: 1,
                lowest_valid_usn: 0,
                next_usn: 10,
                first_usn: 0,
            },
        };
        let ev = JournalStaleEvent::from_request(&req);
        assert_eq!(ev.volume, "D:");
        assert_eq!(ev.reason, "journal_id_changed");
        assert!(ev.full_rescan);
        assert_eq!(ev.message, "test message");
        assert_eq!(topic::SCAN_JOURNAL_STALE, "scan.journal_stale");
        assert_eq!(wire_name(topic::SCAN_JOURNAL_STALE), "scan:journal_stale");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_throttle_limits_hot_stream_to_ten_updates_per_second() {
        let started = Instant::now();
        let mut throttle = CounterThrottle::new_at(Duration::from_millis(100), started);
        let mut emitted = Vec::new();

        for event_index in 0..50_000 {
            let at = started + Duration::from_micros(event_index * 20);
            if let Some(snapshot) = throttle.observe(at, 1) {
                emitted.push(snapshot);
            }
        }

        if let Some(snapshot) = throttle.flush(started + Duration::from_secs(1)) {
            emitted.push(snapshot);
        }

        assert!(
            emitted.len() <= 10,
            "50 000 events/s must be aggregated to <= 10 UI updates/s, got {}",
            emitted.len()
        );
        assert_eq!(emitted.last().map(|snapshot| snapshot.total), Some(50_000));
        assert_eq!(
            emitted.iter().map(|snapshot| snapshot.delta).sum::<u64>(),
            50_000
        );
    }

    #[test]
    fn counter_throttle_flushes_sparse_pending_delta() {
        let started = Instant::now();
        let mut throttle = CounterThrottle::new_at(Duration::from_millis(100), started);

        assert_eq!(
            throttle.observe(started + Duration::from_millis(10), 7),
            None
        );
        assert_eq!(
            throttle.flush(started + Duration::from_millis(20)),
            Some(CounterSnapshot { delta: 7, total: 7 })
        );
        assert_eq!(throttle.flush(started + Duration::from_millis(30)), None);
    }
}
