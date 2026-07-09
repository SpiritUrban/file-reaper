//! Каскад пошуку дублікатів — щабель 1: точний розмір (T-058).
//!
//! architecture.md §4:
//! ```text
//! УСІ ФАЙЛИ (індекс, 0 I/O)
//!   │  щабель 1: групування за точним розміром
//!   ▼     унікальний розмір → НЕ дублікат (~95%)
//! ГРУПИ ОДНАКОВОГО РОЗМІРУ → щабель 2…
//! ```
//!
//! DoD T-058: файли з унікальним розміром відкинуті; 1 млн записів < 1 с.

use serde::{Deserialize, Serialize};

use crate::candidate::{ByteSize, CandidateId};

/// Вхід щабля 1: ідентичність + розмір (без шляху — економія).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeKey {
    pub candidate_id: CandidateId,
    pub size: ByteSize,
}

/// Група файлів **однакового** розміру (≥ 2 члени) — кандидати на щабель 2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactSizeGroup {
    pub size: ByteSize,
    pub members: Vec<CandidateId>,
}

impl ExactSizeGroup {
    /// Скільки можна звільнити, залишивши 1 екземпляр: `size × (n − 1)`.
    pub fn potential_reclaim_bytes(&self) -> u64 {
        let n = self.members.len() as u64;
        self.size.0.saturating_mul(n.saturating_sub(1))
    }

    pub fn member_count(&self) -> usize {
        self.members.len()
    }
}

/// Згрупувати за точним розміром; відкинути унікальні розміри та size==0.
///
/// - **0 I/O** — лише метадані;
/// - порядок груп: більший потенціал звільнення спочатку (підготовка T-064);
/// - усередині групи — стабільний порядок `candidate_id`.
pub fn group_by_exact_size(files: impl IntoIterator<Item = SizeKey>) -> Vec<ExactSizeGroup> {
    use std::collections::HashMap;

    // size → member ids (один прохід, 0 I/O)
    let mut buckets: HashMap<u64, Vec<CandidateId>> = HashMap::new();
    for f in files {
        if f.size.0 == 0 {
            continue;
        }
        buckets.entry(f.size.0).or_default().push(f.candidate_id);
    }

    let mut groups: Vec<ExactSizeGroup> = buckets
        .into_iter()
        .filter_map(|(size, mut members)| {
            if members.len() < 2 {
                return None;
            }
            members.sort_unstable_by_key(|id| id.0);
            // дедуп id (на випадок дубль-рядків індексу)
            members.dedup();
            if members.len() < 2 {
                return None;
            }
            Some(ExactSizeGroup {
                size: ByteSize(size),
                members,
            })
        })
        .collect();

    groups.sort_unstable_by(|a, b| {
        b.potential_reclaim_bytes()
            .cmp(&a.potential_reclaim_bytes())
            .then_with(|| b.size.0.cmp(&a.size.0))
            .then_with(|| a.members[0].0.cmp(&b.members[0].0))
    });
    groups
}

/// Підсумок щабля 1 (для UI «попередня цифра» / метрик).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeStageStats {
    pub files_seen: u64,
    pub files_unique_size: u64,
    pub files_in_groups: u64,
    pub group_count: u64,
    /// Σ size×(n−1) по групах.
    pub potential_reclaim_bytes: u64,
}

impl SizeStageStats {
    pub fn from_groups(files_seen: u64, groups: &[ExactSizeGroup]) -> Self {
        let files_in_groups: u64 = groups.iter().map(|g| g.member_count() as u64).sum();
        let potential_reclaim_bytes = groups.iter().map(|g| g.potential_reclaim_bytes()).sum();
        Self {
            files_seen,
            files_unique_size: files_seen.saturating_sub(files_in_groups),
            files_in_groups,
            group_count: groups.len() as u64,
            potential_reclaim_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: u64, size: u64) -> SizeKey {
        SizeKey {
            candidate_id: CandidateId(id),
            size: ByteSize(size),
        }
    }

    #[test]
    fn unique_sizes_discarded() {
        // DoD: унікальний розмір → не в групах.
        let groups = group_by_exact_size([
            key(1, 100),
            key(2, 200),
            key(3, 300),
            key(4, 100), // пара з 1
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].size.0, 100);
        assert_eq!(groups[0].members.len(), 2);
        assert_eq!(groups[0].members[0].0, 1);
        assert_eq!(groups[0].members[1].0, 4);
    }

    #[test]
    fn zero_size_ignored() {
        let groups = group_by_exact_size([key(1, 0), key(2, 0), key(3, 10), key(4, 10)]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].size.0, 10);
    }

    #[test]
    fn potential_reclaim_leaves_one_copy() {
        let g = ExactSizeGroup {
            size: ByteSize(50),
            members: vec![CandidateId(1), CandidateId(2), CandidateId(3)],
        };
        assert_eq!(g.potential_reclaim_bytes(), 100); // 50 * 2
    }

    #[test]
    fn larger_reclaim_sorted_first() {
        let groups = group_by_exact_size([
            key(1, 10),
            key(2, 10), // reclaim 10
            key(3, 1000),
            key(4, 1000),
            key(5, 1000), // reclaim 2000
        ]);
        assert_eq!(groups[0].size.0, 1000);
        assert_eq!(groups[0].potential_reclaim_bytes(), 2000);
        assert_eq!(groups[1].size.0, 10);
    }

    #[test]
    fn stats_count_unique_vs_grouped() {
        let keys = [
            key(1, 1),
            key(2, 2),
            key(3, 2),
            key(4, 3),
            key(5, 3),
            key(6, 3),
        ];
        let groups = group_by_exact_size(keys);
        let stats = SizeStageStats::from_groups(6, &groups);
        assert_eq!(stats.group_count, 2);
        assert_eq!(stats.files_in_groups, 5); // 2+3
        assert_eq!(stats.files_unique_size, 1); // id=1
        assert_eq!(stats.potential_reclaim_bytes, 2 + 3 * 2); // size2:1 + size3:2
    }

    /// DoD T-058: 1 млн записів < 1 с (release; debug ~2× повільніший).
    ///
    /// ```text
    /// cargo test -p trashradar-domain group_one_million --release -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "perf gate: cargo test -p trashradar-domain group_one_million --release -- --ignored --nocapture"]
    fn group_one_million_records_under_one_second() {
        const N: u64 = 1_000_000;
        let keys: Vec<SizeKey> = (0..N)
            .map(|i| {
                let size = if i < 100_000 {
                    (i / 2) + 1 // 50_000 груп по 2
                } else {
                    1_000_000 + i // унікальні
                };
                key(i, size)
            })
            .collect();

        let start = std::time::Instant::now();
        let groups = group_by_exact_size(keys.iter().copied());
        let elapsed = start.elapsed();

        assert_eq!(groups.len(), 50_000);
        let in_groups: usize = groups.iter().map(|g| g.member_count()).sum();
        assert_eq!(in_groups, 100_000);
        assert!(
            elapsed.as_secs_f64() < 1.0,
            "1M exact-size group took {elapsed:?} (DoD < 1s)"
        );
        eprintln!("T-058 group_by_exact_size 1M: {elapsed:?}");
    }
}
