//! Каскад пошуку дублікатів (architecture.md §4, T-058…).
//!
//! - Щабель 1 — метадані індексу (0 I/O).
//! - Щабель 2 — partial hash head+tail 64 КіБ (≤ 128 КіБ I/O).
//! - Щабель 3 — повний потоковий BLAKE3, багатопотоково по файлах.
//! - Оркестрація — preliminary / confirmed + refining (T-061).
//! - Кеш хешів size+mtime (T-062).

mod cascade;
mod hash_cache;
mod stage1;
mod stage2;
mod stage3;

pub use cascade::{
    run_duplicate_cascade, run_duplicate_cascade_default, run_duplicate_cascade_with_cache,
    spawn_duplicate_cascade, targets_for_partial_groups, CascadeResult,
};
pub use hash_cache::{cache_store_content, cache_store_partial, CountingHasher, MemoryHashCache};
pub use stage1::{run_size_stage, run_size_stage_from_index, SizeStageResult};
pub use stage2::{
    estimated_partial_read, hash_targets_from_records, run_partial_hash_stage,
    run_partial_hash_stage_cached, spawn_partial_hash_stage, HashTarget, MapHasher,
    PartialHashStageResult,
};
pub use stage3::{
    default_file_workers, run_full_hash_stage, run_full_hash_stage_cached, spawn_full_hash_stage,
    FullHashStageResult,
};
pub use trashradar_domain::duplicates::{
    group_by_content_hash, group_by_exact_size, group_by_partial_hash, normalize_hash_cache_path,
    CascadePhase, ContentHash, ContentHashGroup, ContentHashKey, ContentHashStageStats,
    DuplicateConfidence, DuplicatesCategoryState, ExactSizeGroup, FileHashCacheEntry, PartialHash,
    PartialHashGroup, PartialHashKey, PartialHashStageStats, SizeKey, SizeStageStats,
    PARTIAL_HASH_CHUNK_BYTES, PARTIAL_HASH_MAX_READ_BYTES,
};
