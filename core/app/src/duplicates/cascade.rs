//! Оркестрація каскаду дублікатів 1→2→3 зі статусами preliminary/confirmed (T-061).
//!
//! architecture.md §4: щаблі 1–2 дають **попередню** цифру; UI маркує
//! «уточнюється» (`refining`), поки щабель 3 не підтвердить.

use std::collections::HashMap;

use trashradar_domain::candidate::{CandidateId, FileRecord};
use trashradar_domain::duplicates::{
    ContentHashGroup, DuplicateConfidence, DuplicatesCategoryState, PartialHashGroup,
};

use crate::duplicates::stage1::{run_size_stage, SizeStageResult};
use crate::duplicates::stage2::{
    hash_targets_from_records, run_partial_hash_stage, HashTarget, PartialHashStageResult,
};
use crate::duplicates::stage3::{default_file_workers, run_full_hash_stage, FullHashStageResult};
use crate::ports::Hasher;
use crate::workers::{CancellationToken, JobHandle, JobPriority, WorkerPool};

/// Повний результат каскаду + фінальний знімок для UI.
#[derive(Debug, Clone)]
pub struct CascadeResult {
    pub state: DuplicatesCategoryState,
    pub size: SizeStageResult,
    pub partial: PartialHashStageResult,
    pub full: Option<FullHashStageResult>,
    /// Підтверджені групи (порожньо, якщо cancel / 0 після stage 2).
    pub confirmed_groups: Vec<ContentHashGroup>,
}

/// Прогін каскаду size → partial → full.
///
/// `on_update` викликається:
/// 1. після stage 2 — **попередня** цифра, `refining=true` (якщо є групи);
/// 2. після stage 3 — **підтверджена** цифра, `refining=false`.
///
/// DoD T-061: категорія бачить preliminary після stage 2 і маркер
/// «уточнюється» до stage 3.
pub fn run_duplicate_cascade(
    records: &[FileRecord],
    hasher: &dyn Hasher,
    cancel: &CancellationToken,
    file_workers: usize,
    mut on_update: impl FnMut(&DuplicatesCategoryState),
) -> CascadeResult {
    // --- Stage 1 ---
    let size = run_size_stage(records);
    if cancel.is_cancelled() {
        return cancelled_result(size, empty_partial(), None);
    }

    let targets = hash_targets_from_records(records);

    // --- Stage 2 ---
    let partial = run_partial_hash_stage(&size.groups, &targets, hasher, cancel);
    let mut state = DuplicatesCategoryState::preliminary_from_partial(&partial.stats);
    if cancel.is_cancelled() {
        state.cancelled = true;
        state.phase = trashradar_domain::duplicates::CascadePhase::Cancelled;
        state.refining = false;
        on_update(&state);
        return CascadeResult {
            state,
            size,
            partial,
            full: None,
            confirmed_groups: vec![],
        };
    }
    on_update(&state);

    // Немає кандидатів на full hash — preliminary і є фіналом (без confirmed groups).
    if partial.groups.is_empty() {
        state.refining = false;
        state.phase = trashradar_domain::duplicates::CascadePhase::Complete;
        // Довіра лишається Preliminary (нічого не підтверджували), але refining знято.
        on_update(&state);
        return CascadeResult {
            state,
            size,
            partial,
            full: None,
            confirmed_groups: vec![],
        };
    }

    // --- Stage 3 ---
    let workers = if file_workers == 0 {
        default_file_workers()
    } else {
        file_workers
    };
    let full = run_full_hash_stage(&partial.groups, &targets, hasher, cancel, workers);
    state = DuplicatesCategoryState::confirmed_from_full(&full.stats);
    on_update(&state);

    let confirmed_groups = full.groups.clone();
    CascadeResult {
        state,
        size,
        partial,
        full: Some(full),
        confirmed_groups,
    }
}

/// Зручний вхід: лише records + hasher (дефолтні workers).
pub fn run_duplicate_cascade_default(
    records: &[FileRecord],
    hasher: &dyn Hasher,
    cancel: &CancellationToken,
    on_update: impl FnMut(&DuplicatesCategoryState),
) -> CascadeResult {
    run_duplicate_cascade(records, hasher, cancel, 0, on_update)
}

fn empty_partial() -> PartialHashStageResult {
    PartialHashStageResult {
        groups: vec![],
        stats: Default::default(),
    }
}

fn cancelled_result(
    size: SizeStageResult,
    partial: PartialHashStageResult,
    full: Option<FullHashStageResult>,
) -> CascadeResult {
    CascadeResult {
        state: DuplicatesCategoryState {
            phase: trashradar_domain::duplicates::CascadePhase::Cancelled,
            confidence: DuplicateConfidence::Preliminary,
            refining: false,
            reclaimable_bytes: 0,
            group_count: 0,
            files_in_groups: 0,
            cancelled: true,
        },
        size,
        partial,
        full,
        confirmed_groups: vec![],
    }
}

/// Поставити каскад у [`WorkerPool`].
pub fn spawn_duplicate_cascade<H>(
    pool: &WorkerPool,
    priority: JobPriority,
    records: Vec<FileRecord>,
    hasher: std::sync::Arc<H>,
    file_workers: usize,
    on_update: impl FnMut(&DuplicatesCategoryState) + Send + 'static,
    on_done: impl FnOnce(CascadeResult) + Send + 'static,
) -> JobHandle
where
    H: Hasher + 'static,
{
    let mut on_update = on_update;
    pool.submit(priority, move |cancel| {
        let result = run_duplicate_cascade(
            &records,
            hasher.as_ref(),
            &cancel,
            file_workers,
            &mut on_update,
        );
        on_done(result);
    })
}

/// Зберегти path-map для повторного stage 3 (майбутній resume).
pub fn targets_for_partial_groups(
    groups: &[PartialHashGroup],
    all_targets: &HashMap<CandidateId, HashTarget>,
) -> HashMap<CandidateId, HashTarget> {
    let mut out = HashMap::new();
    for g in groups {
        for id in &g.members {
            if let Some(t) = all_targets.get(id) {
                out.insert(*id, t.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duplicates::stage2::MapHasher;
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, SafetyLevel,
    };
    use trashradar_domain::category::CategoryId;
    use trashradar_domain::duplicates::{
        CascadePhase, ContentHash, DuplicateConfidence, PartialHash,
    };
    use trashradar_domain::PARTIAL_HASH_MAX_READ_BYTES;

    fn rec(id: u64, path: &str, size: u64) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: path.into(),
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

    fn ph(b: u8) -> PartialHash {
        let mut a = [0u8; 32];
        a[0] = b;
        PartialHash(a)
    }

    fn ch(b: u8) -> ContentHash {
        let mut a = [0u8; 32];
        a[0] = b;
        ContentHash(a)
    }

    /// DoD: після stage 2 — preliminary + refining; після stage 3 — confirmed.
    #[test]
    fn cascade_emits_preliminary_then_confirmed() {
        let records = [
            rec(1, "a", 1000),
            rec(2, "b", 1000),
            rec(3, "c", 1000),
            rec(4, "d", 1000),
            rec(5, "unique", 999), // відсіється на stage 1
        ];
        // same size → one size group; partial splits into two pairs; full confirms both.
        let hasher = MapHasher {
            map: [
                ("a".into(), ph(1)),
                ("b".into(), ph(1)),
                ("c".into(), ph(2)),
                ("d".into(), ph(2)),
                ("unique".into(), ph(9)),
            ]
            .into_iter()
            .collect(),
            full: [
                ("a".into(), ch(1)),
                ("b".into(), ch(1)),
                ("c".into(), ch(2)),
                ("d".into(), ch(2)),
            ]
            .into_iter()
            .collect(),
            fail: HashMap::new(),
            fail_full: HashMap::new(),
        };

        let mut updates: Vec<DuplicatesCategoryState> = Vec::new();
        let result = run_duplicate_cascade(&records, &hasher, &CancellationToken::new(), 1, |s| {
            updates.push(s.clone())
        });

        assert!(
            updates.len() >= 2,
            "expected preliminary + confirmed, got {}",
            updates.len()
        );
        let prelim = &updates[0];
        assert_eq!(prelim.confidence, DuplicateConfidence::Preliminary);
        assert!(prelim.refining, "DoD: refining until stage 3");
        assert_eq!(prelim.reclaimable_bytes, 2000); // two groups × 1000×1
        assert_eq!(prelim.group_count, 2);
        assert_eq!(prelim.phase, CascadePhase::FullHashing);

        let conf = updates.last().unwrap();
        assert_eq!(conf.confidence, DuplicateConfidence::Confirmed);
        assert!(!conf.refining);
        assert_eq!(conf.reclaimable_bytes, 2000);
        assert_eq!(result.confirmed_groups.len(), 2);
        assert_eq!(result.state.confidence, DuplicateConfidence::Confirmed);
        assert!(result.full.is_some());
        let _ = PARTIAL_HASH_MAX_READ_BYTES; // keep import path stable if used later
    }

    #[test]
    fn full_hash_can_shrink_preliminary_estimate() {
        // Partial thinks a,b,c same; full splits c out → reclaim drops.
        let records = [rec(1, "a", 500), rec(2, "b", 500), rec(3, "c", 500)];
        let hasher = MapHasher {
            map: [
                ("a".into(), ph(1)),
                ("b".into(), ph(1)),
                ("c".into(), ph(1)),
            ]
            .into_iter()
            .collect(),
            full: [
                ("a".into(), ch(9)),
                ("b".into(), ch(9)),
                ("c".into(), ch(7)), // not a true duplicate
            ]
            .into_iter()
            .collect(),
            fail: HashMap::new(),
            fail_full: HashMap::new(),
        };
        let mut updates = Vec::new();
        let result = run_duplicate_cascade(&records, &hasher, &CancellationToken::new(), 2, |s| {
            updates.push(s.clone())
        });
        assert_eq!(updates[0].reclaimable_bytes, 1000); // 500*(3-1)
        assert!(updates[0].refining);
        assert_eq!(result.state.reclaimable_bytes, 500); // only a,b
        assert_eq!(result.state.group_count, 1);
        assert!(!result.state.refining);
        assert_eq!(result.state.confidence, DuplicateConfidence::Confirmed);
    }

    #[test]
    fn no_size_groups_skips_hashing_without_refining() {
        let records = [rec(1, "a", 1), rec(2, "b", 2)];
        let hasher = MapHasher::default();
        let mut updates = Vec::new();
        let result = run_duplicate_cascade(&records, &hasher, &CancellationToken::new(), 1, |s| {
            updates.push(s.clone())
        });
        assert!(!updates.is_empty());
        assert!(!result.state.refining);
        assert_eq!(result.state.reclaimable_bytes, 0);
        assert!(result.full.is_none());
        assert!(result.confirmed_groups.is_empty());
    }

    #[test]
    fn cancel_before_cascade() {
        let records = [rec(1, "a", 10), rec(2, "b", 10)];
        let hasher = MapHasher {
            map: [("a".into(), ph(1)), ("b".into(), ph(1))]
                .into_iter()
                .collect(),
            full: [("a".into(), ch(1)), ("b".into(), ch(1))]
                .into_iter()
                .collect(),
            fail: HashMap::new(),
            fail_full: HashMap::new(),
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = run_duplicate_cascade(&records, &hasher, &cancel, 1, |_| {});
        assert!(result.state.cancelled);
        assert_eq!(result.state.phase, CascadePhase::Cancelled);
        assert!(!result.state.refining);
    }
}
