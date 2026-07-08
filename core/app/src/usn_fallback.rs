//! Фолбек на повний скан при застарілому USN Journal (T-031).
//!
//! architecture.md §2.3: якщо журнал перезаписано — авто-фолбек на повний
//! скан. Тут: розпізнавання stale → очищення курсора → рішення
//! [`UsnSyncResult::FullRescanRequired`] з поясненням для події UI.
//! Запуск самого скану — оркестратор T-033; подія `scan.journal_stale` —
//! shell.

use trashradar_domain::error::CoreError;
use trashradar_domain::scan::{FullRescanReason, UsnJournalInfo};

use crate::ports::{HotIndex, IndexStore, UsnReadOutcome};
use crate::usn_apply::{
    apply_mutations_to_hot_index, next_candidate_id_from_index, plan_usn_mutations, FileProbe,
    FrnPathCache, UsnApplyStats,
};

/// Результат спроби інкрементального оновлення тому (T-030 + T-031).
#[derive(Debug, Clone)]
pub enum UsnSyncResult {
    /// Дельта застосована; курсор просунуто.
    Applied(UsnApplyStats),
    /// Потрібен повний рескан; курсор скинуто; є пояснення для UI.
    FullRescanRequired(FullRescanRequest),
}

/// Запит на автоматичний повний рескан після stale journal (T-031).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullRescanRequest {
    pub volume: char,
    pub reason: FullRescanReason,
    /// Стабільний machine code (`journal_id_changed`, …).
    pub reason_code: &'static str,
    /// Людський текст для тоста / health / логу.
    pub message: String,
    /// Актуальний стан журналу (новий id / межі), якщо відомий.
    pub journal: UsnJournalInfo,
}

impl FullRescanRequest {
    pub fn from_stale(volume: char, reason: &'static str, journal: UsnJournalInfo) -> Self {
        let parsed = FullRescanReason::from_stale_reason(reason);
        Self {
            volume,
            reason: parsed,
            reason_code: parsed.as_str(),
            message: parsed.user_message(volume),
            journal,
        }
    }

    /// Payload для події `scan.journal_stale` (camelCase у shell через serde).
    pub fn event_volume_label(&self) -> String {
        format!("{}:", self.volume.to_ascii_uppercase())
    }
}

/// Обробити результат `read_delta`: застосувати зміни або ініціювати фолбек.
///
/// При [`UsnReadOutcome::JournalStale`]:
/// 1. очищає USN-курсор тому (щоб не чіплятись до застарілої позиції);
/// 2. повертає [`UsnSyncResult::FullRescanRequired`] з поясненням —
///    оркестратор (T-033) запускає повний скан і емітить подію.
pub fn process_usn_sync(
    outcome: UsnReadOutcome,
    volume: char,
    index: &impl HotIndex,
    store: &impl IndexStore,
    cache: &mut FrnPathCache,
    probe: impl FnMut(&str) -> Option<FileProbe>,
) -> Result<UsnSyncResult, CoreError> {
    match outcome {
        UsnReadOutcome::JournalStale { info, reason } => {
            let request = prepare_full_rescan_after_stale(volume, reason, info, store, cache)?;
            Ok(UsnSyncResult::FullRescanRequired(request))
        }
        UsnReadOutcome::Changes {
            changes,
            next_cursor,
        } => {
            let mut next_id = next_candidate_id_from_index(index)?;
            let (mutations, stats) = plan_usn_mutations(&changes, cache, probe, &mut next_id);
            apply_mutations_to_hot_index(index, &mutations)?;
            for m in &mutations {
                if let crate::usn_apply::IndexMutation::Remove { path } = m {
                    let _ = store.delete_file_records_by_path(path)?;
                }
            }
            store.set_usn_cursor(volume, next_cursor)?;
            Ok(UsnSyncResult::Applied(stats))
        }
    }
}

/// Підготовка до повного рескану після stale: скинути курсор і FRN-кеш.
pub fn prepare_full_rescan_after_stale(
    volume: char,
    reason: &'static str,
    journal: UsnJournalInfo,
    store: &impl IndexStore,
    cache: &mut FrnPathCache,
) -> Result<FullRescanRequest, CoreError> {
    store.clear_usn_cursor(volume)?;
    // Кеш шляхів за FRN більше ненадійний — батьківські ланцюжки з іншого
    // знімка журналу; після повного скану засіється наново.
    *cache = FrnPathCache::new();
    cache.seed_volume_root(volume);

    Ok(FullRescanRequest::from_stale(volume, reason, journal))
}

/// Чи результат вимагає повного рескану (зручність для оркестратора).
pub fn requires_full_rescan(result: &UsnSyncResult) -> bool {
    matches!(result, UsnSyncResult::FullRescanRequired(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::HotIndex;
    use crate::usn_apply::FrnPathCache;
    use std::sync::Mutex;
    use trashradar_domain::candidate::FileRecord;
    use trashradar_domain::scan::{UsnCursor, UsnJournalInfo};

    struct MemStore {
        cursor: Mutex<Option<UsnCursor>>,
        cleared: Mutex<bool>,
    }

    impl MemStore {
        fn new(cursor: Option<UsnCursor>) -> Self {
            Self {
                cursor: Mutex::new(cursor),
                cleared: Mutex::new(false),
            }
        }
    }

    impl IndexStore for MemStore {
        fn read_file_records_window(
            &self,
            _: trashradar_domain::category::CategoryId,
            _: trashradar_domain::candidate::FileRecordSort,
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
            *self.cleared.lock().unwrap() = true;
            Ok(())
        }
        fn delete_file_records_by_path(&self, _: &str) -> Result<u64, CoreError> {
            Ok(0)
        }
    }

    struct EmptyIndex;
    impl HotIndex for EmptyIndex {
        fn insert_batch(&self, _: Vec<FileRecord>) -> Result<(), CoreError> {
            Ok(())
        }
        fn finish_indexing(&self) -> Result<(), CoreError> {
            Ok(())
        }
        fn len(&self) -> Result<usize, CoreError> {
            Ok(0)
        }
        fn is_empty(&self) -> Result<bool, CoreError> {
            Ok(true)
        }
        fn clear(&self) -> Result<(), CoreError> {
            Ok(())
        }
        fn get_all(&self) -> Result<Vec<FileRecord>, CoreError> {
            Ok(vec![])
        }
        fn search_file_records(&self, _: &str, _: usize) -> Result<Vec<FileRecord>, CoreError> {
            Ok(vec![])
        }
        fn remove_paths(&self, _: &[String]) -> Result<usize, CoreError> {
            Ok(0)
        }
        fn upsert_batch(&self, _: Vec<FileRecord>) -> Result<(), CoreError> {
            Ok(())
        }
    }

    fn sample_journal() -> UsnJournalInfo {
        UsnJournalInfo {
            journal_id: 99,
            lowest_valid_usn: 1000,
            next_usn: 2000,
            first_usn: 1000,
        }
    }

    #[test]
    fn stale_journal_id_requests_full_rescan_and_clears_cursor() {
        let store = MemStore::new(Some(UsnCursor {
            journal_id: 1,
            next_usn: 10,
        }));
        let mut cache = FrnPathCache::new();
        cache.insert(42, "C:\\old");

        let outcome = UsnReadOutcome::JournalStale {
            info: sample_journal(),
            reason: "journal_id_changed",
        };
        let result = process_usn_sync(outcome, 'C', &EmptyIndex, &store, &mut cache, |_| None)
            .expect("sync");

        assert!(requires_full_rescan(&result));
        match result {
            UsnSyncResult::FullRescanRequired(req) => {
                assert_eq!(req.volume, 'C');
                assert_eq!(req.reason, FullRescanReason::JournalIdChanged);
                assert_eq!(req.reason_code, "journal_id_changed");
                assert!(req.message.contains("C:"));
                assert!(req.message.contains("повне сканування") || req.message.contains("Повне"));
                assert_eq!(req.journal.journal_id, 99);
                assert_eq!(req.event_volume_label(), "C:");
            }
            UsnSyncResult::Applied(_) => panic!("expected full rescan"),
        }
        assert!(*store.cleared.lock().unwrap());
        assert_eq!(store.get_usn_cursor('C').unwrap(), None);
        // Кеш скинуто (лише seed root).
        assert!(cache.get(42).is_none());
        assert!(cache.get(5).is_some());
    }

    #[test]
    fn stale_usn_below_lowest_has_matching_message() {
        let store = MemStore::new(None);
        let mut cache = FrnPathCache::new();
        let outcome = UsnReadOutcome::JournalStale {
            info: sample_journal(),
            reason: "usn_below_lowest_valid",
        };
        let result =
            process_usn_sync(outcome, 'F', &EmptyIndex, &store, &mut cache, |_| None).unwrap();
        match result {
            UsnSyncResult::FullRescanRequired(req) => {
                assert_eq!(req.reason, FullRescanReason::UsnBelowLowestValid);
                assert!(req.message.contains("F:"));
                assert!(req.message.contains("застаріла") || req.message.contains("повне"));
            }
            _ => panic!("expected full rescan"),
        }
    }

    #[test]
    fn applied_delta_is_not_full_rescan() {
        let store = MemStore::new(None);
        let mut cache = FrnPathCache::new();
        cache.seed_volume_root('C');
        let outcome = UsnReadOutcome::Changes {
            changes: vec![],
            next_cursor: UsnCursor {
                journal_id: 1,
                next_usn: 50,
            },
        };
        let result =
            process_usn_sync(outcome, 'C', &EmptyIndex, &store, &mut cache, |_| None).unwrap();
        assert!(!requires_full_rescan(&result));
        match result {
            UsnSyncResult::Applied(s) => {
                assert_eq!(s.created, 0);
            }
            _ => panic!("expected applied"),
        }
        assert_eq!(
            store.get_usn_cursor('C').unwrap(),
            Some(UsnCursor {
                journal_id: 1,
                next_usn: 50
            })
        );
    }

    #[test]
    fn from_stale_reason_covers_known_codes() {
        assert_eq!(
            FullRescanReason::from_stale_reason("journal_entry_deleted"),
            FullRescanReason::JournalEntryDeleted
        );
        assert_eq!(
            FullRescanReason::from_stale_reason("journal_id_changed_during_read"),
            FullRescanReason::JournalIdChangedDuringRead
        );
        assert_eq!(
            FullRescanReason::from_stale_reason("weird"),
            FullRescanReason::Other
        );
    }
}
