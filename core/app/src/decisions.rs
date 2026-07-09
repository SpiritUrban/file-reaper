//! Keep / Mark рішення на файлі → усі категорії (T-057).
//!
//! architecture.md §6.3: рішення застосовується до файла і миттєво
//! відбивається в усіх категоріях. У моделі індексу — одне поле
//! [`Decision`] на [`FileRecord`]; multi-hit (T-054) ділить лише
//! категорії, не окремі decision-рядки.
//!
//! DoD: Keep ховає файл у всіх категоріях; рішення переживає перезапуск
//! (upsert у HotIndex + persistent IndexStore, коли є).

use crate::ports::HotIndex;
use trashradar_domain::candidate::{CandidateId, Decision, FileRecord};
use trashradar_domain::error::CoreError;

/// Ціль застосування рішення (один або кілька файлів).
#[derive(Debug, Clone, Default)]
pub struct DecisionSelector {
    pub candidate_ids: Vec<CandidateId>,
    /// Повні шляхи; порівняння регістронезалежне (Windows).
    pub paths: Vec<String>,
}

impl DecisionSelector {
    pub fn by_id(id: CandidateId) -> Self {
        Self {
            candidate_ids: vec![id],
            paths: vec![],
        }
    }

    pub fn by_path(path: impl Into<String>) -> Self {
        Self {
            candidate_ids: vec![],
            paths: vec![path.into()],
        }
    }

    pub fn is_empty(&self) -> bool {
        self.candidate_ids.is_empty() && self.paths.is_empty()
    }

    fn matches(&self, record: &FileRecord) -> bool {
        if self
            .candidate_ids
            .iter()
            .any(|id| id.0 == record.candidate_id.0)
        {
            return true;
        }
        let path = normalize_path_key(&record.path);
        self.paths
            .iter()
            .any(|p| path.eq_ignore_ascii_case(&normalize_path_key(p)))
    }
}

/// Результат застосування рішення.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyDecisionResult {
    pub updated: Vec<FileRecord>,
    pub decision: Decision,
}

impl ApplyDecisionResult {
    pub fn count(&self) -> usize {
        self.updated.len()
    }
}

/// Застосувати `decision` до matching-записів у зрізі (чиста функція).
pub fn apply_decision_to_records(
    records: &mut [FileRecord],
    selector: &DecisionSelector,
    decision: Decision,
) -> Vec<FileRecord> {
    let mut updated = Vec::new();
    for r in records.iter_mut() {
        if !selector.matches(r) {
            continue;
        }
        if r.decision == decision {
            // Усе одно повертаємо клон — ідемпотентність для upsert/UI.
            updated.push(r.clone());
            continue;
        }
        r.decision = decision;
        updated.push(r.clone());
    }
    updated
}

/// Застосувати рішення в hot-індексі (get_all → mutate → upsert).
pub fn apply_decision_hot(
    index: &dyn HotIndex,
    selector: &DecisionSelector,
    decision: Decision,
) -> Result<ApplyDecisionResult, CoreError> {
    if selector.is_empty() {
        return Err(CoreError::invalid_argument(
            "Потрібен candidateId або path для keep/mark.",
        ));
    }
    let mut all = index.get_all()?;
    let updated = apply_decision_to_records(&mut all, selector, decision);
    if !updated.is_empty() {
        index.upsert_batch(updated.clone())?;
    }
    Ok(ApplyDecisionResult { updated, decision })
}

/// HotIndex + optional persist (SQLite upsert) — рішення переживає перезапуск.
pub fn apply_decision_hot_and_persist<F>(
    index: &dyn HotIndex,
    selector: &DecisionSelector,
    decision: Decision,
    mut persist: F,
) -> Result<ApplyDecisionResult, CoreError>
where
    F: FnMut(&[FileRecord]) -> Result<(), CoreError>,
{
    let result = apply_decision_hot(index, selector, decision)?;
    if !result.updated.is_empty() {
        persist(&result.updated)?;
    }
    Ok(result)
}

/// Keep: приховати з кандидатів.
pub fn keep_hot(
    index: &dyn HotIndex,
    selector: &DecisionSelector,
) -> Result<ApplyDecisionResult, CoreError> {
    apply_decision_hot(index, selector, Decision::Keep)
}

/// Mark / unmark для Reap Bar.
pub fn mark_hot(
    index: &dyn HotIndex,
    selector: &DecisionSelector,
    marked: bool,
) -> Result<ApplyDecisionResult, CoreError> {
    let d = if marked {
        Decision::Marked
    } else {
        Decision::Undecided
    };
    apply_decision_hot(index, selector, d)
}

/// Зняти Keep → Undecided (знову видимий).
pub fn unkeep_hot(
    index: &dyn HotIndex,
    selector: &DecisionSelector,
) -> Result<ApplyDecisionResult, CoreError> {
    apply_decision_hot(index, selector, Decision::Undecided)
}

fn normalize_path_key(path: &str) -> String {
    path.replace('/', "\\")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::{Aggregator, LiveTotals};
    use crate::detectors::{DetectorHit, DetectorId};
    use crate::ports::HotIndex;
    use std::sync::Mutex;
    use trashradar_domain::candidate::{
        ByteSize, CandidateUnit, FileAttributes, FileKind, SafetyLevel, Verdict,
    };
    use trashradar_domain::category::CategoryId;

    struct MemIndex {
        rows: Mutex<Vec<FileRecord>>,
    }

    impl MemIndex {
        fn new(rows: Vec<FileRecord>) -> Self {
            Self {
                rows: Mutex::new(rows),
            }
        }
    }

    impl HotIndex for MemIndex {
        fn insert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError> {
            self.rows.lock().unwrap().extend(records);
            Ok(())
        }
        fn finish_indexing(&self) -> Result<(), CoreError> {
            Ok(())
        }
        fn len(&self) -> Result<usize, CoreError> {
            Ok(self.rows.lock().unwrap().len())
        }
        fn is_empty(&self) -> Result<bool, CoreError> {
            Ok(self.rows.lock().unwrap().is_empty())
        }
        fn clear(&self) -> Result<(), CoreError> {
            self.rows.lock().unwrap().clear();
            Ok(())
        }
        fn get_all(&self) -> Result<Vec<FileRecord>, CoreError> {
            Ok(self.rows.lock().unwrap().clone())
        }
        fn search_file_records(&self, _: &str, _: usize) -> Result<Vec<FileRecord>, CoreError> {
            Ok(vec![])
        }
        fn remove_paths(&self, _: &[String]) -> Result<usize, CoreError> {
            Ok(0)
        }
        fn upsert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError> {
            let mut rows = self.rows.lock().unwrap();
            for rec in records {
                if let Some(i) = rows.iter().position(|r| r.candidate_id == rec.candidate_id) {
                    rows[i] = rec;
                } else {
                    rows.push(rec);
                }
            }
            Ok(())
        }
    }

    fn rec(id: u64, path: &str, size: u64, cat: CategoryId) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: path.into(),
            size: ByteSize(size),
            created_at: None,
            modified_at: None,
            accessed_at: None,
            kind: FileKind::Other,
            unit: CandidateUnit::File,
            category: cat,
            safety: SafetyLevel::ReviewRecommended,
            decision: Decision::Undecided,
            detector_id: "t".into(),
            explanation: "x".into(),
            attributes: FileAttributes::default(),
        }
    }

    #[test]
    fn keep_hides_file_from_all_category_totals() {
        // DoD T-057: Keep у «одній» категорії → файл зникає з усіх (unique + by_cat).
        let size = 1000u64;
        let records = [rec(1, r"C:\a\big.bin", size, CategoryId::LargeFiles)];
        let hits = [
            DetectorHit {
                candidate_id: CandidateId(1),
                detector_id: DetectorId::new("large_files"),
                verdict: Verdict::new(
                    CategoryId::LargeFiles,
                    "large",
                    SafetyLevel::ReviewRecommended,
                ),
            },
            DetectorHit {
                candidate_id: CandidateId(1),
                detector_id: DetectorId::new("old_files"),
                verdict: Verdict::new(CategoryId::OldFiles, "old", SafetyLevel::ReviewRecommended),
            },
            DetectorHit {
                candidate_id: CandidateId(1),
                detector_id: DetectorId::new("archives"),
                verdict: Verdict::new(CategoryId::Archives, "arch", SafetyLevel::ReviewRecommended),
            },
        ];

        let before = Aggregator::from_hits(&hits, &records);
        assert_eq!(before.unique_bytes.0, size);
        assert_eq!(before.category_sum_bytes.0, size * 3);

        let mut kept = records;
        kept[0].decision = Decision::Keep;
        let after = Aggregator::from_hits(&hits, &kept);
        assert_eq!(after.unique_bytes.0, 0);
        assert_eq!(after.unique_files, 0);
        assert_eq!(after.category_sum_bytes.0, 0);
        for cat in CategoryId::ALL {
            assert_eq!(after.category(cat).unwrap().bytes, 0);
        }
    }

    #[test]
    fn keep_on_hot_index_by_path() {
        let index = MemIndex::new(vec![
            rec(1, r"C:\a\f.bin", 10, CategoryId::TempFiles),
            rec(2, r"C:\b\g.bin", 20, CategoryId::TempFiles),
        ]);
        let res = keep_hot(&index, &DecisionSelector::by_path(r"c:\a\f.bin")).unwrap();
        assert_eq!(res.count(), 1);
        assert_eq!(res.decision, Decision::Keep);
        let all = index.get_all().unwrap();
        assert_eq!(
            all.iter()
                .find(|r| r.candidate_id == CandidateId(1))
                .unwrap()
                .decision,
            Decision::Keep
        );
        assert_eq!(
            all.iter()
                .find(|r| r.candidate_id == CandidateId(2))
                .unwrap()
                .decision,
            Decision::Undecided
        );
    }

    #[test]
    fn mark_and_unmark() {
        let index = MemIndex::new(vec![rec(5, r"D:\x", 50, CategoryId::Archives)]);
        mark_hot(&index, &DecisionSelector::by_id(CandidateId(5)), true).unwrap();
        assert!(index.get_all().unwrap()[0].decision.is_marked_for_reap());
        mark_hot(&index, &DecisionSelector::by_id(CandidateId(5)), false).unwrap();
        assert_eq!(index.get_all().unwrap()[0].decision, Decision::Undecided);
    }

    #[test]
    fn decision_survives_get_all_roundtrip_hot() {
        // «Переживає перезапуск» на рівні hot snapshot: upsert → get_all.
        let index = MemIndex::new(vec![rec(1, r"C:\k", 1, CategoryId::OldFiles)]);
        keep_hot(&index, &DecisionSelector::by_id(CandidateId(1))).unwrap();
        // імітація «перезапуску»: новий MemIndex з тих самих рядків (як load from DB)
        let snapshot = index.get_all().unwrap();
        assert_eq!(snapshot[0].decision, Decision::Keep);
        let restored = MemIndex::new(snapshot);
        assert_eq!(restored.get_all().unwrap()[0].decision, Decision::Keep);
        // Keep лишається прихованим після «рестарту»
        let s = Aggregator::from_primary_records(&restored.get_all().unwrap());
        assert_eq!(s.unique_bytes.0, 0);
    }

    #[test]
    fn live_totals_respect_keep_after_decision() {
        let mut live = LiveTotals::new();
        let mut r = rec(1, r"C:\z", 100, CategoryId::LargeFiles);
        live.ingest_primary(&[r.clone()]);
        assert_eq!(live.summary().unique_bytes.0, 100);

        r.decision = Decision::Keep;
        live.set_decision(CandidateId(1), Decision::Keep);
        assert_eq!(live.summary().unique_bytes.0, 0);
    }

    #[test]
    fn empty_selector_is_invalid() {
        let index = MemIndex::new(vec![]);
        let err = keep_hot(&index, &DecisionSelector::default()).unwrap_err();
        assert_eq!(err.code.as_str(), "invalid_argument");
    }

    #[test]
    fn persist_callback_receives_updated_rows() {
        let index = MemIndex::new(vec![rec(1, r"C:\p", 1, CategoryId::TempFiles)]);
        let mut persisted = Vec::new();
        apply_decision_hot_and_persist(
            &index,
            &DecisionSelector::by_id(CandidateId(1)),
            Decision::Keep,
            |rows| {
                persisted = rows.to_vec();
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].decision, Decision::Keep);
    }
}
