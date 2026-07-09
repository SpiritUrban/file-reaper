//! Щабель 1: групування за точним розміром з індексу (T-058).

use trashradar_domain::candidate::{CandidateUnit, Decision, FileRecord};
use trashradar_domain::duplicates::{group_by_exact_size, ExactSizeGroup, SizeKey, SizeStageStats};
use trashradar_domain::error::CoreError;

use crate::ports::HotIndex;

/// Результат щабля 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizeStageResult {
    pub groups: Vec<ExactSizeGroup>,
    pub stats: SizeStageStats,
}

/// Прогін щабля 1 над записами індексу.
///
/// - пропускає [`Decision::Keep`];
/// - size==0 відкидає domain-функція;
/// - лише [`CandidateUnit::File`] (папки-одиниці не дублікати).
pub fn run_size_stage<'a>(records: impl IntoIterator<Item = &'a FileRecord>) -> SizeStageResult {
    let mut files_seen = 0u64;
    let groups = group_by_exact_size(records.into_iter().filter_map(|r| {
        if r.unit != CandidateUnit::File || r.decision == Decision::Keep {
            return None;
        }
        files_seen += 1;
        Some(SizeKey {
            candidate_id: r.candidate_id,
            size: r.size,
        })
    }));
    let stats = SizeStageStats::from_groups(files_seen, &groups);
    SizeStageResult { groups, stats }
}

/// Щабель 1 поверх [`HotIndex`] (DoD «з індексу»).
pub fn run_size_stage_from_index(index: &dyn HotIndex) -> Result<SizeStageResult, CoreError> {
    let records = index.get_all()?;
    Ok(run_size_stage(&records))
}

#[cfg(test)]
mod tests {
    use super::*;
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, FileAttributes, FileKind, SafetyLevel,
    };
    use trashradar_domain::category::CategoryId;

    fn rec(id: u64, size: u64) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: format!(r"C:\f\{id}"),
            size: ByteSize(size),
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

    #[test]
    fn run_skips_keep_and_unique() {
        let mut keep = rec(1, 100);
        keep.decision = Decision::Keep;
        let records = [keep, rec(2, 100), rec(3, 100), rec(4, 999)];
        let out = run_size_stage(&records);
        // Keep не в seen → seen=3, group size 100 with 2+3
        assert_eq!(out.stats.files_seen, 3);
        assert_eq!(out.groups.len(), 1);
        assert_eq!(out.groups[0].members.len(), 2);
        assert_eq!(out.stats.files_unique_size, 1); // 999
    }

    #[test]
    fn run_skips_folder_units() {
        let mut folder = rec(10, 500);
        folder.unit = CandidateUnit::Folder;
        let records = [folder, rec(11, 500), rec(12, 500)];
        let out = run_size_stage(&records);
        assert_eq!(out.stats.files_seen, 2);
        assert_eq!(out.groups.len(), 1);
        assert_eq!(out.groups[0].members.len(), 2);
    }

    struct MemIndex {
        records: std::sync::Mutex<Vec<FileRecord>>,
    }

    impl MemIndex {
        fn with(records: Vec<FileRecord>) -> Self {
            Self {
                records: std::sync::Mutex::new(records),
            }
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
        fn search_file_records(
            &self,
            _query: &str,
            _limit: usize,
        ) -> Result<Vec<FileRecord>, CoreError> {
            Ok(vec![])
        }
        fn remove_paths(&self, _paths: &[String]) -> Result<usize, CoreError> {
            Ok(0)
        }
        fn upsert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError> {
            self.insert_batch(records)
        }
    }

    #[test]
    fn from_index_groups_matching_sizes() {
        let index = MemIndex::with(vec![rec(1, 42), rec(2, 42), rec(3, 7)]);
        let out = run_size_stage_from_index(&index).unwrap();
        assert_eq!(out.stats.files_seen, 3);
        assert_eq!(out.groups.len(), 1);
        assert_eq!(out.groups[0].size.0, 42);
        assert_eq!(out.stats.files_unique_size, 1);
    }

    /// DoD T-058: 1 млн записів < 1 с (release; debug ~2×).
    ///
    /// ```text
    /// cargo test -p trashradar-app one_million_records_under_one_second --release -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "perf gate: cargo test -p trashradar-app one_million_records_under_one_second --release -- --ignored --nocapture"]
    fn one_million_records_under_one_second() {
        const N: u64 = 1_000_000;
        // 50k розмірів × 2 файли = 100k у групах; решта унікальні.
        let records: Vec<FileRecord> = (0..N)
            .map(|i| {
                let size = if i < 100_000 {
                    (i / 2) + 1
                } else {
                    1_000_000 + i
                };
                rec(i, size)
            })
            .collect();

        let start = std::time::Instant::now();
        let out = run_size_stage(&records);
        let elapsed = start.elapsed();

        assert_eq!(out.stats.files_seen, N);
        assert_eq!(out.stats.group_count, 50_000);
        assert_eq!(out.stats.files_in_groups, 100_000);
        assert!(
            elapsed.as_secs_f64() < 1.0,
            "1M size-group took {elapsed:?} (DoD < 1s)"
        );
        eprintln!("T-058 run_size_stage 1M: {elapsed:?}");
    }
}
