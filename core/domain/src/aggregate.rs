//! Унікалізація файла між категоріями — чесна цифра «можна звільнити» (T-054).
//!
//! architecture.md §6.3: файл може бути у кількох категоріях; **загальна**
//! цифра — сума **унікальних** файлів, не сума категорій.
//!
//! product.md: категорії можуть перетинатися (файл видаляється один раз).

use crate::candidate::{ByteSize, CandidateId, Decision};
use crate::category::CategoryId;

/// Накопичувач однієї MVP-категорії (може перетинатися з іншими).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CategoryRollup {
    pub bytes: u64,
    pub files: u64,
}

impl CategoryRollup {
    pub fn add(&mut self, size: u64) {
        self.bytes = self.bytes.saturating_add(size);
        self.files = self.files.saturating_add(1);
    }
}

/// Підсумок «можна звільнити» + розбивка по категоріях.
///
/// Інваріант: `unique_bytes` ≤ `category_sum_bytes` (рівність лише без перетинів).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeableSummary {
    /// Σ розмірів **унікальних** кандидатів (файл у N категоріях — 1×).
    pub unique_bytes: ByteSize,
    pub unique_files: u64,
    /// Наївна сума категорій (Σ by_category.bytes) — для UI/діагностики.
    pub category_sum_bytes: ByteSize,
    /// Порядок = [`CategoryId::ALL`].
    pub by_category: [CategoryRollup; 9],
}

impl Default for FreeableSummary {
    fn default() -> Self {
        Self {
            unique_bytes: ByteSize(0),
            unique_files: 0,
            category_sum_bytes: ByteSize(0),
            by_category: [CategoryRollup::default(); 9],
        }
    }
}

impl FreeableSummary {
    /// Rollup для категорії; `None` для [`CategoryId::Uncategorized`].
    pub fn category(&self, id: CategoryId) -> Option<&CategoryRollup> {
        id.mvp_index().map(|i| &self.by_category[i])
    }

    /// Чи виконується інваріант унікалізації.
    pub fn is_honest(&self) -> bool {
        self.unique_bytes.0 <= self.category_sum_bytes.0
    }
}

/// Внесок одного файла: розмір + множина категорій (перетин).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateContribution {
    pub candidate_id: CandidateId,
    pub size: ByteSize,
    pub decision: Decision,
    /// Категорії, у яких файл заявлений (без Uncategorized / дублікатів id).
    pub categories: Vec<CategoryId>,
}

impl CandidateContribution {
    pub fn new(
        candidate_id: CandidateId,
        size: ByteSize,
        decision: Decision,
        categories: impl IntoIterator<Item = CategoryId>,
    ) -> Self {
        let mut cats = Vec::new();
        for c in categories {
            if c == CategoryId::Uncategorized {
                continue;
            }
            if !cats.contains(&c) {
                cats.push(c);
            }
        }
        Self {
            candidate_id,
            size,
            decision,
            categories: cats,
        }
    }

    /// Чи враховується в «можна звільнити» (не Keep, є ≥1 категорія).
    pub fn is_reclaimable(&self) -> bool {
        self.decision != Decision::Keep && !self.categories.is_empty()
    }
}

/// Звести внески у [`FreeableSummary`].
///
/// - **Keep** — повністю ігнорується;
/// - без категорій — ігнорується;
/// - один `candidate_id` кілька разів — береться **перший** reclaimable
///   (викликач має дедуплікувати; якщо ні — `seen` захищає unique).
pub fn summarize_unique(
    contributions: impl IntoIterator<Item = CandidateContribution>,
) -> FreeableSummary {
    use std::collections::HashSet;

    let mut summary = FreeableSummary::default();
    let mut seen: HashSet<u64> = HashSet::new();

    for c in contributions {
        if !c.is_reclaimable() {
            continue;
        }
        let size = c.size.0;

        // Unique: один раз на candidate_id.
        if seen.insert(c.candidate_id.0) {
            summary.unique_bytes.0 = summary.unique_bytes.0.saturating_add(size);
            summary.unique_files = summary.unique_files.saturating_add(1);
        }

        // Per-category: size у кожну категорію (перетин навмисний).
        for cat in &c.categories {
            if let Some(i) = cat.mvp_index() {
                summary.by_category[i].add(size);
            }
        }
    }

    summary.category_sum_bytes.0 = summary.by_category.iter().map(|r| r.bytes).sum();
    debug_assert!(summary.is_honest());
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contrib(id: u64, size: u64, cats: &[CategoryId]) -> CandidateContribution {
        CandidateContribution::new(
            CandidateId(id),
            ByteSize(size),
            Decision::Undecided,
            cats.iter().copied(),
        )
    }

    #[test]
    fn file_in_three_categories_counted_once_in_unique_total() {
        // DoD T-054: файл у 3 категоріях → «можна звільнити» 1×.
        let size = 100 * 1024 * 1024u64;
        let summary = summarize_unique([contrib(
            1,
            size,
            &[
                CategoryId::LargeFiles,
                CategoryId::OldFiles,
                CategoryId::Archives,
            ],
        )]);

        assert_eq!(summary.unique_bytes.0, size);
        assert_eq!(summary.unique_files, 1);
        assert_eq!(summary.category_sum_bytes.0, size * 3);
        assert!(summary.is_honest());

        assert_eq!(
            summary.category(CategoryId::LargeFiles).unwrap().bytes,
            size
        );
        assert_eq!(summary.category(CategoryId::OldFiles).unwrap().bytes, size);
        assert_eq!(summary.category(CategoryId::Archives).unwrap().bytes, size);
        assert_eq!(summary.category(CategoryId::TempFiles).unwrap().bytes, 0);
    }

    #[test]
    fn two_files_one_overlap_honest_total() {
        // A: Large+Old (50), B: Old only (30) → unique 80, Old=80, Large=50
        let summary = summarize_unique([
            contrib(1, 50, &[CategoryId::LargeFiles, CategoryId::OldFiles]),
            contrib(2, 30, &[CategoryId::OldFiles]),
        ]);
        assert_eq!(summary.unique_bytes.0, 80);
        assert_eq!(summary.unique_files, 2);
        assert_eq!(summary.category(CategoryId::LargeFiles).unwrap().bytes, 50);
        assert_eq!(summary.category(CategoryId::OldFiles).unwrap().bytes, 80);
        assert_eq!(summary.category_sum_bytes.0, 50 + 80);
    }

    #[test]
    fn keep_excluded_from_all_totals() {
        let mut keep = contrib(1, 999, &[CategoryId::TempFiles]);
        keep.decision = Decision::Keep;
        let summary = summarize_unique([keep, contrib(2, 10, &[CategoryId::TempFiles])]);
        assert_eq!(summary.unique_bytes.0, 10);
        assert_eq!(summary.unique_files, 1);
        assert_eq!(summary.category(CategoryId::TempFiles).unwrap().files, 1);
    }

    #[test]
    fn empty_categories_ignored() {
        let summary = summarize_unique([contrib(1, 100, &[])]);
        assert_eq!(summary.unique_bytes.0, 0);
        assert_eq!(summary.unique_files, 0);
    }

    #[test]
    fn duplicate_candidate_id_not_double_unique() {
        // Захист: той самий id двічі в потоці — unique 1×.
        let summary = summarize_unique([
            contrib(7, 40, &[CategoryId::Archives]),
            contrib(7, 40, &[CategoryId::Archives, CategoryId::LargeFiles]),
        ]);
        assert_eq!(summary.unique_bytes.0, 40);
        assert_eq!(summary.unique_files, 1);
        // Але per-category add може спрацювати двічі — викликач має дедуп.
        // Тут другий прохід додає Archives ще раз + LargeFiles.
        assert_eq!(summary.category(CategoryId::Archives).unwrap().files, 2);
    }

    #[test]
    fn mvp_index_covers_all() {
        for (i, cat) in CategoryId::ALL.iter().enumerate() {
            assert_eq!(cat.mvp_index(), Some(i), "{cat:?}");
        }
        assert_eq!(CategoryId::Uncategorized.mvp_index(), None);
    }
}
