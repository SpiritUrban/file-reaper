//! Каскад пошуку дублікатів (architecture.md §4, T-058…).
//!
//! - Щабель 1 — метадані індексу (0 I/O).
//! - Щабель 2 — partial hash head+tail 64 КіБ (≤ 128 КіБ I/O).

mod stage1;
mod stage2;

pub use stage1::{run_size_stage, run_size_stage_from_index, SizeStageResult};
pub use stage2::{
    estimated_partial_read, hash_targets_from_records, run_partial_hash_stage,
    spawn_partial_hash_stage, HashTarget, MapHasher, PartialHashStageResult,
};
pub use trashradar_domain::duplicates::{
    group_by_exact_size, group_by_partial_hash, ExactSizeGroup, PartialHash, PartialHashGroup,
    PartialHashKey, PartialHashStageStats, SizeKey, SizeStageStats, PARTIAL_HASH_CHUNK_BYTES,
    PARTIAL_HASH_MAX_READ_BYTES,
};
