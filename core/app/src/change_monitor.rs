//! Change Monitor: живе оновлення індексу з USN, поки застосунок відкритий (T-032).
//!
//! architecture.md §13: легкий фоновий модуль — слухає USN Journal і тримає
//! індекс актуальним. DoD: видалення у Explorer відбивається в індексі
//! ≤ 5 с (poll_interval за замовчуванням 1 с, стеля 5 с).
//!
//! Оркестрація чиста: [`monitor_tick`] + [`ChangeMonitor`] (фоновий цикл
//! з [`CancellationToken`]). I/O — через [`ChangeSource`] / probe.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use trashradar_domain::error::CoreError;
use trashradar_domain::scan::UsnCursor;

use crate::ports::{ChangeSource, HotIndex, IndexStore, UsnReadOutcome};
use crate::usn_apply::{FileProbe, FrnPathCache, UsnApplyStats};
use crate::usn_fallback::{process_usn_sync, FullRescanRequest, UsnSyncResult};
use crate::workers::CancellationToken;

/// Максимальний інтервал опитування для DoD T-032 (≤ 5 с).
pub const MAX_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Дефолт: 1 с — з запасом під 5-секундний DoD.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Конфігурація Change Monitor.
#[derive(Debug, Clone)]
pub struct ChangeMonitorConfig {
    /// Інтервал між тиками. Обрізається до [`MAX_POLL_INTERVAL`].
    pub poll_interval: Duration,
    /// Томи для спостереження (літери).
    pub volumes: Vec<char>,
}

impl Default for ChangeMonitorConfig {
    fn default() -> Self {
        Self {
            poll_interval: DEFAULT_POLL_INTERVAL,
            volumes: Vec::new(),
        }
    }
}

impl ChangeMonitorConfig {
    pub fn effective_poll_interval(&self) -> Duration {
        if self.poll_interval.is_zero() {
            DEFAULT_POLL_INTERVAL
        } else {
            self.poll_interval.min(MAX_POLL_INTERVAL)
        }
    }
}

/// Подія одного тика монітора (для listener / UI).
#[derive(Debug, Clone)]
pub enum MonitorEvent {
    /// Дельта застосована до індексу.
    Applied { volume: char, stats: UsnApplyStats },
    /// Потрібен повний рескан (T-031).
    FullRescanRequired(FullRescanRequest),
    /// Немає збереженого курсора — чекаємо повного скану (T-033).
    NoCursor { volume: char },
    /// Помилка читання/застосування (том пропускається до наступного тика).
    Error { volume: char, message: String },
}

/// Підсумок одного [`monitor_tick`].
#[derive(Debug, Default, Clone)]
pub struct MonitorTickReport {
    pub events: Vec<MonitorEvent>,
}

/// Один цикл опитування всіх томів (T-032, unit-тестований без sleep).
///
/// Для кожного тому з курсором: `read_delta` → `process_usn_sync`.
/// `probe(volume, path)` — метадані файла (розмір) для create/modify.
pub fn monitor_tick(
    volumes: &[char],
    source: &impl ChangeSource,
    index: &impl HotIndex,
    store: &impl IndexStore,
    caches: &mut HashMap<char, FrnPathCache>,
    mut probe: impl FnMut(char, &str) -> Option<FileProbe>,
) -> MonitorTickReport {
    let mut report = MonitorTickReport::default();

    for &volume in volumes {
        let volume = volume.to_ascii_uppercase();
        let cache = caches.entry(volume).or_insert_with(|| {
            let mut c = FrnPathCache::new();
            c.seed_volume_root(volume);
            c
        });

        let cursor = match store.get_usn_cursor(volume) {
            Ok(Some(c)) => c,
            Ok(None) => {
                report.events.push(MonitorEvent::NoCursor { volume });
                continue;
            }
            Err(e) => {
                report.events.push(MonitorEvent::Error {
                    volume,
                    message: e.to_string(),
                });
                continue;
            }
        };

        let outcome = match source.read_delta(volume, cursor) {
            Ok(o) => o,
            Err(e) => {
                report.events.push(MonitorEvent::Error {
                    volume,
                    message: e.to_string(),
                });
                continue;
            }
        };

        // Порожня дельта без змін — тихо.
        if let UsnReadOutcome::Changes {
            ref changes,
            next_cursor,
        } = outcome
        {
            if changes.is_empty() {
                // Все одно просуваємо курсор, якщо next змінився.
                if next_cursor != cursor {
                    let _ = store.set_usn_cursor(volume, next_cursor);
                }
                continue;
            }
        }

        let mut vol_probe = |path: &str| probe(volume, path);
        match process_usn_sync(outcome, volume, index, store, cache, &mut vol_probe) {
            Ok(UsnSyncResult::Applied(stats)) => {
                if stats.created + stats.modified + stats.deleted + stats.renamed > 0 {
                    report.events.push(MonitorEvent::Applied { volume, stats });
                }
            }
            Ok(UsnSyncResult::FullRescanRequired(req)) => {
                report.events.push(MonitorEvent::FullRescanRequired(req));
            }
            Err(e) => {
                report.events.push(MonitorEvent::Error {
                    volume,
                    message: e.to_string(),
                });
            }
        }
    }

    report
}

/// Слухач подій монітора (shell емітить IPC).
pub trait ChangeMonitorListener: Send {
    fn on_event(&self, event: &MonitorEvent);
}

/// No-op listener.
pub struct NopListener;
impl ChangeMonitorListener for NopListener {
    fn on_event(&self, _: &MonitorEvent) {}
}

/// Фоновий Change Monitor (окремий потік, кооперативна відміна).
pub struct ChangeMonitor {
    cancel: CancellationToken,
    /// true після stop/join.
    stopped: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl ChangeMonitor {
    /// Запускає монітор у фоновому потоці.
    ///
    /// `store` і `source` переїжджають у потік (Send). `index` — `Arc` (Sync).
    pub fn start<I, S, C, L, P>(
        config: ChangeMonitorConfig,
        index: Arc<I>,
        store: S,
        source: C,
        listener: L,
        probe: P,
    ) -> Self
    where
        I: HotIndex + Send + Sync + 'static,
        S: IndexStore + Send + 'static,
        C: ChangeSource + Send + 'static,
        L: ChangeMonitorListener + 'static,
        P: Fn(char, &str) -> Option<FileProbe> + Send + 'static,
    {
        let cancel = CancellationToken::new();
        let cancel_thread = cancel.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_thread = Arc::clone(&stopped);
        let interval = config.effective_poll_interval();
        let volumes = config
            .volumes
            .into_iter()
            .map(|v| v.to_ascii_uppercase())
            .collect::<Vec<_>>();

        let join = thread::Builder::new()
            .name("trashradar-change-monitor".into())
            .spawn(move || {
                let mut caches: HashMap<char, FrnPathCache> = HashMap::new();
                while !cancel_thread.is_cancelled() {
                    let report = monitor_tick(
                        &volumes,
                        &source,
                        index.as_ref(),
                        &store,
                        &mut caches,
                        |vol, path| probe(vol, path),
                    );
                    for ev in &report.events {
                        listener.on_event(ev);
                    }
                    // Sleep у коротких квантах — швидша відміна.
                    sleep_cancellable(&cancel_thread, interval);
                }
                stopped_thread.store(true, Ordering::Release);
            })
            .expect("spawn change monitor");

        Self {
            cancel,
            stopped,
            join: Some(join),
        }
    }

    /// Запит на зупинку (кооперативно).
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    /// Зупинити і дочекатись завершення потоку.
    pub fn stop(mut self) {
        self.cancel.cancel();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for ChangeMonitor {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn sleep_cancellable(cancel: &CancellationToken, total: Duration) {
    let slice = Duration::from_millis(50);
    let mut left = total;
    while !left.is_zero() && !cancel.is_cancelled() {
        let step = left.min(slice);
        thread::sleep(step);
        left = left.saturating_sub(step);
    }
}

/// Знімок для події `index.updated` (T-032 → UI / майбутня цифра T-055).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexUpdatedNotice {
    pub volume: char,
    pub created: u64,
    pub modified: u64,
    pub deleted: u64,
    pub renamed: u64,
}

impl IndexUpdatedNotice {
    pub fn from_applied(volume: char, stats: &UsnApplyStats) -> Self {
        Self {
            volume,
            created: stats.created,
            modified: stats.modified,
            deleted: stats.deleted,
            renamed: stats.renamed,
        }
    }

    pub fn volume_label(&self) -> String {
        format!("{}:", self.volume.to_ascii_uppercase())
    }

    pub fn has_changes(&self) -> bool {
        self.created + self.modified + self.deleted + self.renamed > 0
    }
}

/// Спільний кеш FRN між тиками (експорт для shell wiring).
pub type VolumePathCaches = HashMap<char, FrnPathCache>;

/// Зручність: початковий курсор після full scan уже в store — монітор
/// підхопить на наступному тику. Ця функція лише перевіряє інваріант.
pub fn volume_is_watched(store: &impl IndexStore, volume: char) -> Result<bool, CoreError> {
    Ok(store.get_usn_cursor(volume)?.is_some())
}

/// Тестовий double: заздалегідь задані outcomes на кожен read_delta.
#[derive(Default)]
pub struct ScriptedChangeSource {
    /// Черга outcomes (FIFO) на кожен виклик read_delta.
    pub outcomes: Mutex<Vec<Result<UsnReadOutcome, CoreError>>>,
    pub journals: Mutex<HashMap<char, trashradar_domain::scan::UsnJournalInfo>>,
}

impl ScriptedChangeSource {
    pub fn with_outcomes(outcomes: Vec<Result<UsnReadOutcome, CoreError>>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes),
            journals: Mutex::new(HashMap::new()),
        }
    }
}

impl ChangeSource for ScriptedChangeSource {
    fn query_journal(
        &self,
        volume: char,
    ) -> Result<trashradar_domain::scan::UsnJournalInfo, CoreError> {
        self.journals
            .lock()
            .unwrap()
            .get(&volume.to_ascii_uppercase())
            .copied()
            .ok_or_else(|| CoreError::internal("no scripted journal"))
    }

    fn read_delta(&self, _volume: char, _from: UsnCursor) -> Result<UsnReadOutcome, CoreError> {
        let mut q = self.outcomes.lock().unwrap();
        if q.is_empty() {
            // Порожня дельта за замовчуванням.
            return Ok(UsnReadOutcome::Changes {
                changes: vec![],
                next_cursor: _from,
            });
        }
        q.remove(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::HotIndex;
    use crate::usn_apply::UsnApplyStats;
    use std::sync::Mutex;
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
        FileRecordSort, SafetyLevel,
    };
    use trashradar_domain::category::CategoryId;
    use trashradar_domain::scan::{usn_reason, UsnChange, UsnCursor, UsnJournalInfo};

    struct MemStore {
        cursor: Mutex<Option<UsnCursor>>,
    }

    impl MemStore {
        fn with_cursor(c: UsnCursor) -> Self {
            Self {
                cursor: Mutex::new(Some(c)),
            }
        }
    }

    impl IndexStore for MemStore {
        fn read_file_records_window(
            &self,
            _: CategoryId,
            _: FileRecordSort,
            _: u64,
            _: u64,
        ) -> Result<Vec<FileRecord>, CoreError> {
            Ok(vec![])
        }
        fn read_all_file_records(&self) -> Result<Vec<FileRecord>, CoreError> {
            Ok(vec![])
        }
        fn get_usn_cursor(&self, _: char) -> Result<Option<UsnCursor>, CoreError> {
            Ok(*self.cursor.lock().unwrap())
        }
        fn set_usn_cursor(&self, _: char, c: UsnCursor) -> Result<(), CoreError> {
            *self.cursor.lock().unwrap() = Some(c);
            Ok(())
        }
        fn clear_usn_cursor(&self, _: char) -> Result<(), CoreError> {
            *self.cursor.lock().unwrap() = None;
            Ok(())
        }
        fn delete_file_records_by_path(&self, _: &str) -> Result<u64, CoreError> {
            Ok(0)
        }
    }

    struct MemIndex {
        records: Mutex<Vec<FileRecord>>,
    }

    impl MemIndex {
        fn new() -> Self {
            Self {
                records: Mutex::new(Vec::new()),
            }
        }
        fn paths(&self) -> Vec<String> {
            self.records
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.path.clone())
                .collect()
        }
    }

    impl HotIndex for MemIndex {
        fn insert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError> {
            self.records.lock().unwrap().extend(records);
            Ok(())
        }
        fn finish_indexing(&self) -> Result<(), CoreError> {
            Ok(())
        }
        fn len(&self) -> Result<usize, CoreError> {
            Ok(self.records.lock().unwrap().len())
        }
        fn is_empty(&self) -> Result<bool, CoreError> {
            Ok(self.records.lock().unwrap().is_empty())
        }
        fn clear(&self) -> Result<(), CoreError> {
            self.records.lock().unwrap().clear();
            Ok(())
        }
        fn get_all(&self) -> Result<Vec<FileRecord>, CoreError> {
            Ok(self.records.lock().unwrap().clone())
        }
        fn search_file_records(&self, _: &str, _: usize) -> Result<Vec<FileRecord>, CoreError> {
            Ok(vec![])
        }
        fn remove_paths(&self, paths: &[String]) -> Result<usize, CoreError> {
            let mut recs = self.records.lock().unwrap();
            let before = recs.len();
            recs.retain(|r| !paths.iter().any(|p| r.path.eq_ignore_ascii_case(p)));
            Ok(before - recs.len())
        }
        fn upsert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError> {
            let mut recs = self.records.lock().unwrap();
            for record in records {
                if let Some(i) = recs
                    .iter()
                    .position(|r| r.path.eq_ignore_ascii_case(&record.path))
                {
                    recs[i] = record;
                } else {
                    recs.push(record);
                }
            }
            Ok(())
        }
    }

    fn change(file_ref: u64, parent: u64, reason: u32, name: &str) -> UsnChange {
        UsnChange {
            usn: 1,
            file_ref,
            parent_ref: parent,
            reason,
            name: name.to_string(),
            is_directory: false,
            timestamp: None,
        }
    }

    #[test]
    fn poll_interval_capped_at_five_seconds() {
        let cfg = ChangeMonitorConfig {
            poll_interval: Duration::from_secs(30),
            volumes: vec!['C'],
        };
        assert_eq!(cfg.effective_poll_interval(), MAX_POLL_INTERVAL);
        assert!(cfg.effective_poll_interval() <= Duration::from_secs(5));
    }

    #[test]
    fn tick_applies_delete_to_index_within_one_poll() {
        // DoD: видалення відбивається без дій користувача на наступному тику
        // (тик ≤ 5 с за конфігом).
        let index = MemIndex::new();
        index
            .insert_batch(vec![FileRecord {
                candidate_id: CandidateId(1),
                path: "C:\\data\\gone.txt".into(),
                size: ByteSize(10),
                created_at: None,
                modified_at: None,
                accessed_at: None,
                kind: FileKind::Document,
                unit: CandidateUnit::File,
                category: CategoryId::Uncategorized,
                safety: SafetyLevel::ReviewRecommended,
                decision: Decision::Undecided,
                detector_id: String::new(),
                explanation: String::new(),
                attributes: FileAttributes::default(),
            }])
            .unwrap();

        let cursor = UsnCursor {
            journal_id: 1,
            next_usn: 100,
        };
        let store = MemStore::with_cursor(cursor);

        let mut cache = FrnPathCache::new();
        cache.seed_volume_root('C');
        cache.insert(10, "C:\\data");
        cache.insert(20, "C:\\data\\gone.txt");

        let delete = change(20, 10, usn_reason::FILE_DELETE, "gone.txt");
        let source = ScriptedChangeSource::with_outcomes(vec![Ok(UsnReadOutcome::Changes {
            changes: vec![delete],
            next_cursor: UsnCursor {
                journal_id: 1,
                next_usn: 101,
            },
        })]);

        let mut caches = HashMap::new();
        caches.insert('C', cache);

        let report = monitor_tick(&['C'], &source, &index, &store, &mut caches, |_, _| None);

        assert!(
            report.events.iter().any(|e| matches!(
                e,
                MonitorEvent::Applied {
                    stats: UsnApplyStats { deleted: 1, .. },
                    ..
                }
            )),
            "expected Applied delete, got {:?}",
            report.events
        );
        assert!(
            index.paths().is_empty(),
            "file should be gone from index: {:?}",
            index.paths()
        );
        assert_eq!(store.get_usn_cursor('C').unwrap().unwrap().next_usn, 101);
    }

    #[test]
    fn tick_without_cursor_emits_no_cursor() {
        let index = MemIndex::new();
        let store = MemStore {
            cursor: Mutex::new(None),
        };
        let source = ScriptedChangeSource::default();
        let mut caches = HashMap::new();
        let report = monitor_tick(&['C'], &source, &index, &store, &mut caches, |_, _| None);
        assert!(matches!(
            report.events.first(),
            Some(MonitorEvent::NoCursor { volume: 'C' })
        ));
    }

    #[test]
    fn tick_stale_journal_requests_full_rescan() {
        let index = MemIndex::new();
        let store = MemStore::with_cursor(UsnCursor {
            journal_id: 1,
            next_usn: 10,
        });
        let info = UsnJournalInfo {
            journal_id: 99,
            lowest_valid_usn: 0,
            next_usn: 1000,
            first_usn: 0,
        };
        let source = ScriptedChangeSource::with_outcomes(vec![Ok(UsnReadOutcome::JournalStale {
            info,
            reason: "journal_id_changed",
        })]);
        let mut caches = HashMap::new();
        let report = monitor_tick(&['C'], &source, &index, &store, &mut caches, |_, _| None);
        assert!(matches!(
            report.events.first(),
            Some(MonitorEvent::FullRescanRequired(_))
        ));
        assert!(store.get_usn_cursor('C').unwrap().is_none());
    }

    #[test]
    fn index_updated_notice_from_stats() {
        let n = IndexUpdatedNotice::from_applied(
            'e',
            &UsnApplyStats {
                deleted: 2,
                created: 1,
                ..UsnApplyStats::default()
            },
        );
        assert_eq!(n.volume_label(), "E:");
        assert!(n.has_changes());
        assert_eq!(n.deleted, 2);
    }

    #[test]
    fn background_monitor_stops_on_cancel() {
        let index = Arc::new(MemIndex::new());
        let store = MemStore {
            cursor: Mutex::new(None),
        };
        let source = ScriptedChangeSource::default();
        let mon = ChangeMonitor::start(
            ChangeMonitorConfig {
                poll_interval: Duration::from_millis(100),
                volumes: vec!['C'],
            },
            index,
            store,
            source,
            NopListener,
            |_, _| None,
        );
        thread::sleep(Duration::from_millis(50));
        assert!(!mon.is_stopped());
        mon.stop();
        // stop joins — якщо зависне, тест не завершиться.
    }
}
