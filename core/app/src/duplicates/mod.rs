//! Каскад пошуку дублікатів (architecture.md §4, T-058…).
//!
//! - Щабель 1 — метадані індексу (0 I/O).
//! - Щабель 2 — partial hash head+tail 64 КіБ (≤ 128 КіБ I/O).
//! - Щабель 3 — повний потоковий BLAKE3, багатопотоково по файлах.
//! - Оркестрація — preliminary / confirmed + refining (T-061).
//! - Кеш хешів size+mtime (T-062).
//! - Ліміт I/O на том (T-063).
//! - Пріоритет груп за potential reclaim (T-064).
//! - Політика Keep/Reap ✓/╳ (T-065).

mod cascade;
mod hash_cache;
mod keep_policy;
mod stage1;
mod stage2;
mod stage3;
mod volume_limit;

pub use cascade::{
    run_duplicate_cascade, run_duplicate_cascade_default, run_duplicate_cascade_with_cache,
    spawn_duplicate_cascade, targets_for_partial_groups, CascadeResult,
};
pub use hash_cache::{cache_store_content, cache_store_partial, CountingHasher, MemoryHashCache};
pub use keep_policy::{
    mark_confirmed_groups, mark_group_with_records, member_refs_from_records, record_index_by_id,
};
pub use stage1::{run_size_stage, run_size_stage_from_index, SizeStageResult};
pub use stage2::{
    estimated_partial_read, hash_targets_from_records, run_partial_hash_stage,
    run_partial_hash_stage_cached, run_partial_hash_stage_gated, spawn_partial_hash_stage,
    HashTarget, MapHasher, PartialHashStageResult,
};
pub use stage3::{
    default_file_workers, run_full_hash_stage, run_full_hash_stage_cached,
    run_full_hash_stage_gated, spawn_full_hash_stage, FullHashStageResult,
};
pub use trashradar_domain::duplicates::{
    choose_keep_member, group_by_content_hash, group_by_exact_size, group_by_partial_hash,
    is_downloads_like_path, mark_content_hash_group, mark_duplicate_members,
    normalize_hash_cache_path, prioritize_exact_size_groups, prioritize_partial_groups,
    reclaim_order_of_partial_groups, CascadePhase, ContentHash, ContentHashGroup, ContentHashKey,
    ContentHashStageStats, DuplicateConfidence, DuplicateMemberRef, DuplicateRole,
    DuplicatesCategoryState, ExactSizeGroup, FileHashCacheEntry, KeepPolicy, MarkedDuplicateGroup,
    MarkedDuplicateMember, PartialHash, PartialHashGroup, PartialHashKey, PartialHashStageStats,
    SizeKey, SizeStageStats, PARTIAL_HASH_CHUNK_BYTES, PARTIAL_HASH_MAX_READ_BYTES,
};
pub use volume_limit::{
    acquire_for_path, volume_from_path, VolumeIoGate, VolumeIoPermit,
    DEFAULT_MAX_CONCURRENT_READS_PER_VOLUME, UNKNOWN_VOLUME,
};
