//! Aggregator: чесні цифри «можна звільнити» з перетинами категорій (T-054).
//!
//! architecture.md §6.3 / repository.md §4: загальна цифра — сума
//! **унікальних** файлів; категорії можуть перетинатися.
//!
//! Вхід: hits оркестратора (T-037) + записи індексу (розмір / decision).
//! Вихід: [`FreeableSummary`] (domain).

use std::collections::HashMap;

use crate::detectors::DetectorHit;
use trashradar_domain::aggregate::{summarize_unique, CandidateContribution, FreeableSummary};
use trashradar_domain::candidate::{CandidateId, Decision, FileRecord};
use trashradar_domain::category::CategoryId;

/// Application-оркестратор агрегованих цифр (T-054; live-події — T-055).
#[derive(Debug, Default)]
pub struct Aggregator;

impl Aggregator {
    pub fn new() -> Self {
        Self
    }

    /// Зібрати summary з **усіх hits** (перетини) + метаданих записів.
    ///
    /// - hits без відповідного запису в `records` — пропускаються;
    /// - Keep — не в «можна звільнити»;
    /// - один файл у N категоріях → `unique_bytes` += size **один раз**.
    pub fn from_hits(hits: &[DetectorHit], records: &[FileRecord]) -> FreeableSummary {
        let by_id = index_records(records);
        let contributions = contributions_from_hits(hits, &by_id);
        summarize_unique(contributions)
    }

    /// Fallback: лише primary `FileRecord.category` (без multi-hit).
    ///
    /// Корисний, коли hits недоступні (індекс уже з primary category).
    /// Перетини **не** видно — unique == category sum (якщо немає Keep).
    pub fn from_primary_records(records: &[FileRecord]) -> FreeableSummary {
        let contributions = records.iter().filter_map(|r| {
            if r.category == CategoryId::Uncategorized {
                return None;
            }
            Some(CandidateContribution::new(
                r.candidate_id,
                r.size,
                r.decision,
                [r.category],
            ))
        });
        summarize_unique(contributions)
    }

    /// З готових внесків (тести / майбутній multi-category store).
    pub fn from_contributions(
        contributions: impl IntoIterator<Item = CandidateContribution>,
    ) -> FreeableSummary {
        summarize_unique(contributions)
    }
}

fn index_records(records: &[FileRecord]) -> HashMap<CandidateId, &FileRecord> {
    let mut map = HashMap::with_capacity(records.len());
    for r in records {
        map.insert(r.candidate_id, r);
    }
    map
}

/// Hits → один [`CandidateContribution`] на candidate_id (усі категорії hits).
fn contributions_from_hits(
    hits: &[DetectorHit],
    by_id: &HashMap<CandidateId, &FileRecord>,
) -> Vec<CandidateContribution> {
    // candidate_id → (size, decision, categories)
    let mut acc: HashMap<CandidateId, (u64, Decision, Vec<CategoryId>)> = HashMap::new();

    for hit in hits {
        let Some(record) = by_id.get(&hit.candidate_id) else {
            continue;
        };
        let cat = hit.verdict.category;
        if cat == CategoryId::Uncategorized {
            continue;
        }
        let entry =
            acc.entry(hit.candidate_id)
                .or_insert((record.size.0, record.decision, Vec::new()));
        // Prefer record size/decision; categories accumulate.
        entry.0 = record.size.0;
        entry.1 = record.decision;
        if !entry.2.contains(&cat) {
            entry.2.push(cat);
        }
    }

    acc.into_iter()
        .map(|(id, (size, decision, categories))| {
            CandidateContribution::new(
                id,
                trashradar_domain::candidate::ByteSize(size),
                decision,
                categories,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::{
        Detector, DetectorHit, DetectorId, DetectorOrchestrator, DetectorRegistry,
    };
    use trashradar_domain::candidate::{
        ByteSize, CandidateUnit, FileAttributes, FileKind, SafetyLevel, Verdict,
    };

    fn rec(id: u64, size: u64) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: format!(r"C:\f\{id}.bin"),
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

    struct AlwaysHit {
        id: DetectorId,
        category: CategoryId,
    }

    impl Detector for AlwaysHit {
        fn id(&self) -> DetectorId {
            self.id
        }
        fn category(&self) -> CategoryId {
            self.category
        }
        fn evaluate(&self, _: &FileRecord) -> Option<Verdict> {
            Some(Verdict::new(
                self.category,
                "hit",
                SafetyLevel::ReviewRecommended,
            ))
        }
    }

    #[test]
    fn dod_three_categories_unique_once() {
        // DoD T-054: файл у 3 категоріях рахується в «можна звільнити» один раз.
        let size = 250 * 1024 * 1024u64;
        let records = [rec(1, size)];
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
                verdict: Verdict::new(
                    CategoryId::Archives,
                    "archive",
                    SafetyLevel::ReviewRecommended,
                ),
            },
        ];

        let summary = Aggregator::from_hits(&hits, &records);
        assert_eq!(summary.unique_bytes.0, size, "unique = 1× size");
        assert_eq!(summary.unique_files, 1);
        assert_eq!(summary.category_sum_bytes.0, size * 3);
        assert!(summary.is_honest());
        assert_eq!(
            summary.category(CategoryId::LargeFiles).unwrap().bytes,
            size
        );
        assert_eq!(summary.category(CategoryId::OldFiles).unwrap().bytes, size);
        assert_eq!(summary.category(CategoryId::Archives).unwrap().bytes, size);
    }

    #[test]
    fn orchestrator_hits_feed_honest_total() {
        let mut reg = DetectorRegistry::new();
        reg.register(AlwaysHit {
            id: DetectorId::new("a"),
            category: CategoryId::LargeFiles,
        });
        reg.register(AlwaysHit {
            id: DetectorId::new("b"),
            category: CategoryId::TempFiles,
        });
        let orch = DetectorOrchestrator::new(&reg);
        let records = [rec(10, 1000), rec(11, 2000)];
        let out = orch.categorize_batch(&records);

        // 2 files × 2 categories each = 4 hits; unique = 3000
        assert_eq!(out.hits.len(), 4);
        let summary = Aggregator::from_hits(&out.hits, &records);
        assert_eq!(summary.unique_bytes.0, 3000);
        assert_eq!(summary.unique_files, 2);
        assert_eq!(summary.category_sum_bytes.0, 6000); // each cat has both files
        assert_eq!(
            summary.category(CategoryId::LargeFiles).unwrap().bytes,
            3000
        );
        assert_eq!(summary.category(CategoryId::TempFiles).unwrap().bytes, 3000);
    }

    #[test]
    fn keep_file_not_in_unique_total() {
        let mut records = [rec(1, 500), rec(2, 100)];
        records[0].decision = Decision::Keep;
        let hits = [
            DetectorHit {
                candidate_id: CandidateId(1),
                detector_id: DetectorId::new("t"),
                verdict: Verdict::new(CategoryId::TempFiles, "t", SafetyLevel::SafeToBulk),
            },
            DetectorHit {
                candidate_id: CandidateId(2),
                detector_id: DetectorId::new("t"),
                verdict: Verdict::new(CategoryId::TempFiles, "t", SafetyLevel::SafeToBulk),
            },
        ];
        let summary = Aggregator::from_hits(&hits, &records);
        assert_eq!(summary.unique_bytes.0, 100);
        assert_eq!(summary.unique_files, 1);
    }

    #[test]
    fn from_primary_records_no_overlap() {
        let mut a = rec(1, 10);
        a.category = CategoryId::Archives;
        let mut b = rec(2, 20);
        b.category = CategoryId::Archives;
        let summary = Aggregator::from_primary_records(&[a, b]);
        assert_eq!(summary.unique_bytes.0, 30);
        assert_eq!(summary.category_sum_bytes.0, 30);
    }
}
