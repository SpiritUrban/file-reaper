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
    /// Живе оновлення індексу від Change Monitor (T-032).
    pub const INDEX_UPDATED: &str = "index.updated";
    /// Прогрес сесії скану по томах (T-033).
    pub const SCAN_PROGRESS: &str = "scan.progress";
    /// Жива головна цифра + розбивка (T-055).
    pub const CLEANUP_TOTAL_UPDATED: &str = "cleanup.total_updated";
    /// Оновлення однієї категорії (T-055).
    pub const CATEGORY_UPDATED: &str = "category.updated";
    /// Каскад дублікатів: preliminary / confirmed + refining (T-061).
    pub const DUPLICATES_CASCADE_UPDATED: &str = "duplicates.cascade_updated";
    /// Файл відновлено з карантину; `usedSuffix=true` = попередження про
    /// зайнятий оригінальний шлях (T-080). Живить тост T-132.
    pub const QUARANTINE_RESTORED: &str = "quarantine.restored";
    #[allow(dead_code)] // lifecycle wiring starts with Quarantine screen T-131
    pub const QUARANTINE_CHANGED: &str = "quarantine.changed";
    #[allow(dead_code)] // lifecycle wiring starts with Quarantine screen T-131
    pub const QUARANTINE_ENTRY_EXPIRED: &str = "quarantine.entry_expired";
    pub const SETTINGS_CHANGED: &str = "settings.changed";
    /// Мініатюра плитки готова після фонової P1-генерації (T-067/T-120).
    pub const PREVIEW_READY: &str = "preview.ready";
}

/// Payload `preview.ready` (T-120/T-124): превью, згенероване в фоні, готове
/// до показу. `path` — ключ доставки (той самий, яким запитували
/// `preview.thumbnail`/`preview.large`); `dataUrl` — вже закодований PNG
/// (base64). `kind`: `"thumbnail"` (T-120) або `"large_sharp"` (T-124 —
/// підміна Draft на різке після P0-генерації); скраб-кадри доставляються
/// синхронно відповіддю команди, не подією.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewReadyEvent {
    pub path: String,
    pub kind: String,
    pub data_url: String,
}

/// Payload `index.updated` (T-032): дельта після USN-тика.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // emit: Change Monitor listener / T-033 wiring
pub struct IndexUpdatedEvent {
    pub volume: String,
    pub created: u64,
    pub modified: u64,
    pub deleted: u64,
    pub renamed: u64,
}

impl IndexUpdatedEvent {
    #[allow(dead_code)]
    pub fn from_notice(n: &trashradar_app::change_monitor::IndexUpdatedNotice) -> Self {
        Self {
            volume: n.volume_label(),
            created: n.created,
            modified: n.modified,
            deleted: n.deleted,
            renamed: n.renamed,
        }
    }
}

/// Емісія live-оновлення індексу (T-032).
#[allow(dead_code)] // wired when ChangeMonitor runs in shell (T-033 lifecycle)
pub fn emit_index_updated<R: Runtime>(
    app: &AppHandle<R>,
    notice: &trashradar_app::change_monitor::IndexUpdatedNotice,
) {
    if !notice.has_changes() {
        return;
    }
    emit(
        app,
        topic::INDEX_UPDATED,
        &IndexUpdatedEvent::from_notice(notice),
    );
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

/// Payload `quarantine.restored` (T-080, підключено `quarantine.restore_batch` T-132).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineRestoredEvent {
    pub entry_id: u64,
    pub original_path: String,
    /// Фактичний шлях після відновлення (може відрізнятись суфіксом).
    pub restored_path: String,
    /// Оригінальний шлях був зайнятий → застосовано суфікс (DoD T-080).
    pub used_suffix: bool,
    /// Попередження для UI — лише коли used_suffix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl QuarantineRestoredEvent {
    pub fn from_outcome(outcome: &trashradar_app::RestoreOutcome) -> Self {
        let message = outcome.used_suffix.then(|| {
            format!(
                "Оригінальний шлях «{}» був зайнятий — файл відновлено як «{}».",
                outcome.entry.original_path, outcome.restored_path
            )
        });
        Self {
            entry_id: outcome.entry.id.0,
            original_path: outcome.entry.original_path.clone(),
            restored_path: outcome.restored_path.clone(),
            used_suffix: outcome.used_suffix,
            message,
        }
    }
}

/// Емісія результату відновлення (T-080 DoD: подія-попередження при суфіксі).
pub fn emit_quarantine_restored<R: Runtime>(
    app: &AppHandle<R>,
    outcome: &trashradar_app::RestoreOutcome,
) {
    let payload = QuarantineRestoredEvent::from_outcome(outcome);
    if let Some(message) = &payload.message {
        tracing::warn!(
            entry_id = payload.entry_id,
            restored_path = %payload.restored_path,
            "{message}"
        );
    }
    emit(app, topic::QUARANTINE_RESTORED, &payload);
}

/// Payload `quarantine.changed`: тримання Quarantine змінилось — TTL-sweeper
/// (T-082, `emit_quarantine_sweep`, ще не підключений до lifecycle) або
/// ручний `quarantine.purge` (T-133, підключено).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineChangedEvent {
    pub purged_count: u64,
    pub purged_bytes: u64,
    pub held_bytes: u64,
    pub threshold_exceeded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // emitted by scheduled shell lifecycle once T-131 wires Quarantine UI
pub struct QuarantineEntryExpiredEvent {
    pub entry_id: u64,
    pub original_path: String,
    pub size_bytes: u64,
}

#[allow(dead_code)] // shell lifecycle wiring follows with T-131
pub fn emit_quarantine_sweep<R: Runtime>(app: &AppHandle<R>, result: &trashradar_app::SweepResult) {
    for entry in &result.purged {
        emit(
            app,
            topic::QUARANTINE_ENTRY_EXPIRED,
            &QuarantineEntryExpiredEvent {
                entry_id: entry.id.0,
                original_path: entry.original_path.clone(),
                size_bytes: entry.size.0,
            },
        );
    }
    let message = result.threshold_exceeded.then(|| {
        format!(
            "Quarantine утримує {} байт — перевищено налаштований поріг.",
            result.held_bytes
        )
    });
    emit(
        app,
        topic::QUARANTINE_CHANGED,
        &QuarantineChangedEvent {
            purged_count: result.purged.len() as u64,
            purged_bytes: result.purged_bytes,
            held_bytes: result.held_bytes,
            threshold_exceeded: result.threshold_exceeded,
            message,
        },
    );
}

/// Реєстр реалізованих scan/index/aggregate-подій (health / smoke).
pub fn scan_event_topics() -> &'static [&'static str] {
    &[
        topic::SCAN_JOURNAL_STALE,
        topic::INDEX_UPDATED,
        topic::SCAN_PROGRESS,
        topic::CLEANUP_TOTAL_UPDATED,
        topic::CATEGORY_UPDATED,
        topic::DUPLICATES_CASCADE_UPDATED,
    ]
}

// --- T-055: живі агрегати ----------------------------------------------------

/// Payload `category.updated` / елемент `cleanup.total_updated.categories`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategorySummaryEvent {
    /// snake_case CategoryId (`large_files`, …).
    pub id: String,
    pub total_bytes: u64,
    pub item_count: u64,
    pub safety: String,
    /// MVP: `files` (groups/folders — пізніше).
    pub count_unit: String,
}

/// Payload `cleanup.total_updated` (дзеркало UI CleanupTotal + uniqueFiles).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupTotalEvent {
    /// Чесна цифра унікальних кандидатів (T-054).
    pub reclaimable_bytes: u64,
    pub unique_files: u64,
    pub categories: Vec<CategorySummaryEvent>,
}

impl CleanupTotalEvent {
    pub fn from_summary(summary: &trashradar_domain::aggregate::FreeableSummary) -> Self {
        use trashradar_domain::candidate::SafetyLevel;
        use trashradar_domain::category::CategoryId;

        let mut categories = Vec::new();
        for cat in CategoryId::ALL {
            let Some(roll) = summary.category(cat) else {
                continue;
            };
            if roll.files == 0 && roll.bytes == 0 {
                continue;
            }
            categories.push(CategorySummaryEvent {
                id: category_id_wire(cat).into(),
                total_bytes: roll.bytes,
                item_count: roll.files,
                // Без per-file safety map — review як безпечний дефолт Sidebar.
                safety: safety_wire(SafetyLevel::ReviewRecommended).into(),
                count_unit: "files".into(),
            });
        }
        // Sidebar: важчі категорії зверху.
        categories.sort_by(|a, b| {
            b.total_bytes
                .cmp(&a.total_bytes)
                .then_with(|| a.id.cmp(&b.id))
        });
        Self {
            reclaimable_bytes: summary.unique_bytes.0,
            unique_files: summary.unique_files,
            categories,
        }
    }
}

fn category_id_wire(c: trashradar_domain::category::CategoryId) -> &'static str {
    use trashradar_domain::category::CategoryId::*;
    match c {
        LargeFiles => "large_files",
        OldFiles => "old_files",
        ForgottenVideos => "forgotten_videos",
        Duplicates => "duplicates",
        Archives => "archives",
        Installers => "installers",
        TempFiles => "temp_files",
        AppCaches => "app_caches",
        DevArtifacts => "dev_artifacts",
        EmptyFolders => "empty_folders",
        SparseFolders => "sparse_folders",
        Uncategorized => "uncategorized",
    }
}

fn safety_wire(s: trashradar_domain::candidate::SafetyLevel) -> &'static str {
    match s {
        trashradar_domain::candidate::SafetyLevel::SafeToBulk => "safe_to_bulk",
        trashradar_domain::candidate::SafetyLevel::ReviewRecommended => "review_recommended",
    }
}

/// Емісія повної цифри + per-category подій (T-055).
pub fn emit_cleanup_totals<R: Runtime>(
    app: &AppHandle<R>,
    summary: &trashradar_domain::aggregate::FreeableSummary,
) {
    let total = CleanupTotalEvent::from_summary(summary);
    emit(app, topic::CLEANUP_TOTAL_UPDATED, &total);
    for cat in &total.categories {
        emit(app, topic::CATEGORY_UPDATED, cat);
    }
}

// --- T-061/T-126: каскад дублікатів -------------------------------------------
// Емісія зі scan_runtime post-scan (T-126); те саме DTO — тіло duplicates.groups.

/// Payload `duplicates.cascade_updated` (дзеркало domain `DuplicatesCategoryState`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicatesCascadeEvent {
    pub phase: String,
    pub confidence: String,
    pub refining: bool,
    pub reclaimable_bytes: u64,
    pub group_count: u64,
    pub files_in_groups: u64,
    pub cancelled: bool,
}

impl DuplicatesCascadeEvent {
    pub fn from_state(state: &trashradar_domain::DuplicatesCategoryState) -> Self {
        use trashradar_domain::{CascadePhase, DuplicateConfidence};
        Self {
            phase: match state.phase {
                CascadePhase::Idle => "idle",
                CascadePhase::SizeGrouping => "size_grouping",
                CascadePhase::PartialHashing => "partial_hashing",
                CascadePhase::FullHashing => "full_hashing",
                CascadePhase::Complete => "complete",
                CascadePhase::Cancelled => "cancelled",
            }
            .into(),
            confidence: match state.confidence {
                DuplicateConfidence::Preliminary => "preliminary",
                DuplicateConfidence::Confirmed => "confirmed",
            }
            .into(),
            refining: state.refining,
            reclaimable_bytes: state.reclaimable_bytes,
            group_count: state.group_count,
            files_in_groups: state.files_in_groups,
            cancelled: state.cancelled,
        }
    }
}

/// Емісія прогресу каскаду дублікатів (T-061), викликається з
/// `scan_runtime::run_duplicates_cascade` (T-126) на preliminary/confirmed.
pub fn emit_duplicates_cascade<R: Runtime>(
    app: &AppHandle<R>,
    state: &trashradar_domain::DuplicatesCategoryState,
) {
    emit(
        app,
        topic::DUPLICATES_CASCADE_UPDATED,
        &DuplicatesCascadeEvent::from_state(state),
    );
}

/// Тротлінг snapshot-ів агрегатів ≤10/с (T-006 / T-055).
#[derive(Debug, Clone)]
pub struct AggregateThrottle {
    min_interval: Duration,
    last_emit_at: Instant,
    pending: Option<trashradar_domain::aggregate::FreeableSummary>,
}

impl AggregateThrottle {
    pub fn new_at(min_interval: Duration, started_at: Instant) -> Self {
        Self {
            min_interval,
            last_emit_at: started_at,
            pending: None,
        }
    }

    /// Запам'ятати останній summary; емітити якщо минув інтервал.
    pub fn observe(
        &mut self,
        now: Instant,
        summary: trashradar_domain::aggregate::FreeableSummary,
    ) -> Option<trashradar_domain::aggregate::FreeableSummary> {
        self.pending = Some(summary);
        if now.duration_since(self.last_emit_at) >= self.min_interval {
            return self.flush(now);
        }
        None
    }

    /// Примусовий flush (кінець скану) — без втрати останнього snapshot.
    pub fn flush(&mut self, now: Instant) -> Option<trashradar_domain::aggregate::FreeableSummary> {
        let s = self.pending.take()?;
        self.last_emit_at = now;
        Some(s)
    }
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
        assert_eq!(topic::INDEX_UPDATED, "index.updated");
        assert!(scan_event_topics().contains(&topic::INDEX_UPDATED));
    }
}

#[cfg(test)]
mod quarantine_restored_tests {
    use super::*;
    use trashradar_app::RestoreOutcome;
    use trashradar_domain::candidate::ByteSize;
    use trashradar_domain::quarantine::{QuarantineEntry, QuarantineEntryId, QuarantineStatus};

    fn outcome(restored_path: &str, used_suffix: bool) -> RestoreOutcome {
        RestoreOutcome {
            entry: QuarantineEntry {
                id: QuarantineEntryId(7),
                batch_id: None,
                original_path: r"C:\Users\Ada\Videos\clip.mp4".into(),
                surrogate_name: "00000007.bin".into(),
                size: ByteSize(4096),
                quarantined_at_unix: 1_750_000_000,
                expires_at_unix: 1_752_592_000,
                status: QuarantineStatus::Restored,
            },
            restored_path: restored_path.into(),
            used_suffix,
        }
    }

    /// DoD T-080: суфікс → подія-попередження з обома шляхами.
    #[test]
    fn suffix_restore_carries_warning_message() {
        let ev = QuarantineRestoredEvent::from_outcome(&outcome(
            r"C:\Users\Ada\Videos\clip (відновлено).mp4",
            true,
        ));
        assert_eq!(ev.entry_id, 7);
        assert!(ev.used_suffix);
        let message = ev.message.expect("попередження при суфіксі");
        assert!(message.contains(r"C:\Users\Ada\Videos\clip.mp4"));
        assert!(message.contains("(відновлено)"));
        assert_eq!(topic::QUARANTINE_RESTORED, "quarantine.restored");
        assert_eq!(wire_name(topic::QUARANTINE_RESTORED), "quarantine:restored");
    }

    #[test]
    fn clean_restore_has_no_warning() {
        let ev =
            QuarantineRestoredEvent::from_outcome(&outcome(r"C:\Users\Ada\Videos\clip.mp4", false));
        assert!(!ev.used_suffix);
        assert!(ev.message.is_none());
        assert_eq!(ev.restored_path, ev.original_path);
    }
}

#[cfg(test)]
mod aggregate_events_tests {
    use super::*;
    use trashradar_domain::aggregate::{summarize_unique, CandidateContribution};
    use trashradar_domain::candidate::{ByteSize, CandidateId, Decision};
    use trashradar_domain::category::CategoryId;

    #[test]
    fn cleanup_total_from_summary_dod_shape() {
        let summary = summarize_unique([CandidateContribution::new(
            CandidateId(1),
            ByteSize(1024 * 1024 * 1024),
            Decision::Undecided,
            [CategoryId::LargeFiles, CategoryId::Archives],
        )]);
        let ev = CleanupTotalEvent::from_summary(&summary);
        assert_eq!(ev.reclaimable_bytes, 1024 * 1024 * 1024);
        assert_eq!(ev.unique_files, 1);
        assert_eq!(ev.categories.len(), 2);
        assert!(ev.categories.iter().any(|c| c.id == "large_files"));
        assert!(ev.categories.iter().any(|c| c.id == "archives"));
        // важчі / рівні — стабільний порядок
        assert_eq!(topic::CLEANUP_TOTAL_UPDATED, "cleanup.total_updated");
        assert_eq!(topic::CATEGORY_UPDATED, "category.updated");
        assert!(scan_event_topics().contains(&topic::CLEANUP_TOTAL_UPDATED));
    }

    #[test]
    fn duplicates_cascade_event_from_preliminary_state() {
        use trashradar_domain::duplicates::{
            CascadePhase, DuplicateConfidence, DuplicatesCategoryState,
        };
        let state = DuplicatesCategoryState {
            phase: CascadePhase::FullHashing,
            confidence: DuplicateConfidence::Preliminary,
            refining: true,
            reclaimable_bytes: 27 * 1024 * 1024 * 1024,
            group_count: 12,
            files_in_groups: 40,
            cancelled: false,
        };
        let ev = DuplicatesCascadeEvent::from_state(&state);
        assert_eq!(ev.phase, "full_hashing");
        assert_eq!(ev.confidence, "preliminary");
        assert!(ev.refining);
        assert_eq!(ev.reclaimable_bytes, 27 * 1024 * 1024 * 1024);
        assert_eq!(
            topic::DUPLICATES_CASCADE_UPDATED,
            "duplicates.cascade_updated"
        );
        assert_eq!(
            wire_name(topic::DUPLICATES_CASCADE_UPDATED),
            "duplicates:cascade_updated"
        );
        assert!(scan_event_topics().contains(&topic::DUPLICATES_CASCADE_UPDATED));
    }

    #[test]
    fn aggregate_throttle_caps_to_ten_per_second_and_flush() {
        let started = Instant::now();
        let mut th = AggregateThrottle::new_at(Duration::from_millis(100), started);
        let s = summarize_unique([CandidateContribution::new(
            CandidateId(1),
            ByteSize(10),
            Decision::Undecided,
            [CategoryId::TempFiles],
        )]);
        let mut emits = 0u32;
        for i in 0..50 {
            let at = started + Duration::from_millis(i * 10);
            if th.observe(at, s.clone()).is_some() {
                emits += 1;
            }
        }
        // 50 samples over 500ms @ 100ms interval → ~5 + flush
        assert!(emits <= 10, "emits={emits}");
        assert!(th.flush(started + Duration::from_secs(1)).is_some() || emits > 0);
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
