//! Щабель 2: частковий хеш head+tail 64 КіБ (T-059).
//!
//! I/O — лише через [`Hasher`]; domain групує готові fingerprint-и.

use std::collections::HashMap;

use trashradar_domain::candidate::{ByteSize, CandidateId, FileRecord};
use trashradar_domain::duplicates::{
    group_by_partial_hash, ContentHash, ExactSizeGroup, PartialHash, PartialHashGroup,
    PartialHashKey, PartialHashStageStats,
};
use trashradar_domain::error::CoreError;

use crate::ports::Hasher;
use crate::workers::{CancellationToken, JobHandle, JobPriority, WorkerPool};

/// Результат щабля 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialHashStageResult {
    pub groups: Vec<PartialHashGroup>,
    pub stats: PartialHashStageStats,
}

/// Шлях + розмір для члена size-групи.
#[derive(Debug, Clone)]
pub struct HashTarget {
    pub candidate_id: CandidateId,
    pub path: String,
    pub size: ByteSize,
}

/// Прогін щабля 2: хешує членів size-груп, розбиває за partial-хешем.
///
/// - кооперативна відміна між файлами (T-008);
/// - помилка одного файла → skip + `files_failed`, решта триває;
/// - `bytes_read` оцінюється як min(size, 128 KiB) на успішний файл
///   (фактичне обмеження — в адаптері Hasher).
pub fn run_partial_hash_stage(
    size_groups: &[ExactSizeGroup],
    targets: &HashMap<CandidateId, HashTarget>,
    hasher: &dyn Hasher,
    cancel: &CancellationToken,
) -> PartialHashStageResult {
    let mut keys: Vec<PartialHashKey> = Vec::new();
    let mut files_hashed = 0u64;
    let mut files_failed = 0u64;
    let mut bytes_read = 0u64;
    let mut cancelled = false;

    'outer: for group in size_groups {
        for &id in &group.members {
            if cancel.is_cancelled() {
                cancelled = true;
                break 'outer;
            }
            let Some(target) = targets.get(&id) else {
                files_failed += 1;
                continue;
            };
            match hasher.partial_hash(&target.path, target.size) {
                Ok(partial_hash) => {
                    files_hashed += 1;
                    bytes_read += estimated_partial_read(target.size.0);
                    keys.push(PartialHashKey {
                        candidate_id: id,
                        size: target.size,
                        partial_hash,
                    });
                }
                Err(_) => {
                    files_failed += 1;
                }
            }
        }
    }

    let groups = group_by_partial_hash(keys);
    let stats = PartialHashStageStats::from_groups(
        files_hashed,
        files_failed,
        bytes_read,
        cancelled,
        &groups,
    );
    PartialHashStageResult { groups, stats }
}

/// Зібрати targets з індексних записів (path + size).
pub fn hash_targets_from_records<'a>(
    records: impl IntoIterator<Item = &'a FileRecord>,
) -> HashMap<CandidateId, HashTarget> {
    records
        .into_iter()
        .map(|r| {
            (
                r.candidate_id,
                HashTarget {
                    candidate_id: r.candidate_id,
                    path: r.path.clone(),
                    size: r.size,
                },
            )
        })
        .collect()
}

/// Оцінка байтів читання (узгоджено з infra: ≤ 128 КіБ).
pub fn estimated_partial_read(size: u64) -> u64 {
    use trashradar_domain::PARTIAL_HASH_MAX_READ_BYTES;
    size.min(PARTIAL_HASH_MAX_READ_BYTES)
}

/// Поставити щабель 2 у [`WorkerPool`] (фон після size-stage).
pub fn spawn_partial_hash_stage<H>(
    pool: &WorkerPool,
    priority: JobPriority,
    size_groups: Vec<ExactSizeGroup>,
    targets: HashMap<CandidateId, HashTarget>,
    hasher: std::sync::Arc<H>,
    on_done: impl FnOnce(PartialHashStageResult) + Send + 'static,
) -> JobHandle
where
    H: Hasher + 'static,
{
    pool.submit(priority, move |cancel| {
        let result = run_partial_hash_stage(&size_groups, &targets, hasher.as_ref(), &cancel);
        on_done(result);
    })
}

/// Тестовий Hasher: map path → partial / full (0 I/O).
#[derive(Debug, Default)]
pub struct MapHasher {
    pub map: HashMap<String, PartialHash>,
    pub full: HashMap<String, ContentHash>,
    pub fail: HashMap<String, bool>,
    pub fail_full: HashMap<String, bool>,
}

impl Hasher for MapHasher {
    fn partial_hash(&self, path: &str, _size: ByteSize) -> Result<PartialHash, CoreError> {
        if self.fail.get(path).copied().unwrap_or(false) {
            return Err(CoreError::io(format!("mock fail: {path}")));
        }
        self.map
            .get(path)
            .copied()
            .ok_or_else(|| CoreError::io(format!("mock missing: {path}")))
    }

    fn full_hash(
        &self,
        path: &str,
        _size: ByteSize,
        cancel: &CancellationToken,
    ) -> Result<ContentHash, CoreError> {
        if cancel.is_cancelled() {
            return Err(CoreError::cancelled("full_hash"));
        }
        if self.fail_full.get(path).copied().unwrap_or(false) {
            return Err(CoreError::io(format!("mock full fail: {path}")));
        }
        self.full
            .get(path)
            .copied()
            .ok_or_else(|| CoreError::io(format!("mock full missing: {path}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trashradar_domain::candidate::ByteSize;
    use trashradar_domain::PARTIAL_HASH_MAX_READ_BYTES;

    fn target(id: u64, path: &str, size: u64) -> (CandidateId, HashTarget) {
        let cid = CandidateId(id);
        (
            cid,
            HashTarget {
                candidate_id: cid,
                path: path.into(),
                size: ByteSize(size),
            },
        )
    }

    fn ph(b: u8) -> PartialHash {
        let mut a = [0u8; 32];
        a[0] = b;
        PartialHash(a)
    }

    #[test]
    fn different_partial_splits_size_group() {
        // DoD: size-group → split by partial hash.
        let size_groups = vec![ExactSizeGroup {
            size: ByteSize(10_000),
            members: vec![
                CandidateId(1),
                CandidateId(2),
                CandidateId(3),
                CandidateId(4),
            ],
        }];
        let targets: HashMap<_, _> = [
            target(1, r"C:\a.bin", 10_000),
            target(2, r"C:\b.bin", 10_000),
            target(3, r"C:\c.bin", 10_000),
            target(4, r"C:\d.bin", 10_000),
        ]
        .into_iter()
        .collect();
        let hasher = MapHasher {
            map: [
                (r"C:\a.bin".into(), ph(1)),
                (r"C:\b.bin".into(), ph(1)), // pair with a
                (r"C:\c.bin".into(), ph(2)),
                (r"C:\d.bin".into(), ph(2)), // pair with c
            ]
            .into_iter()
            .collect(),
            full: HashMap::new(),
            fail: HashMap::new(),
            fail_full: HashMap::new(),
        };
        let out =
            run_partial_hash_stage(&size_groups, &targets, &hasher, &CancellationToken::new());
        assert_eq!(out.groups.len(), 2);
        assert_eq!(out.stats.files_hashed, 4);
        assert_eq!(out.stats.files_failed, 0);
        assert_eq!(out.stats.files_in_groups, 4);
        assert!(out.stats.bytes_read <= 4 * PARTIAL_HASH_MAX_READ_BYTES);
        assert!(out.stats.bytes_read > 0);
    }

    #[test]
    fn unique_partial_after_split_discarded() {
        let size_groups = vec![ExactSizeGroup {
            size: ByteSize(100),
            members: vec![CandidateId(1), CandidateId(2), CandidateId(3)],
        }];
        let targets: HashMap<_, _> = [
            target(1, "a", 100),
            target(2, "b", 100),
            target(3, "c", 100),
        ]
        .into_iter()
        .collect();
        let hasher = MapHasher {
            map: [
                ("a".into(), ph(9)),
                ("b".into(), ph(9)),
                ("c".into(), ph(7)), // alone
            ]
            .into_iter()
            .collect(),
            full: HashMap::new(),
            fail: HashMap::new(),
            fail_full: HashMap::new(),
        };
        let out =
            run_partial_hash_stage(&size_groups, &targets, &hasher, &CancellationToken::new());
        assert_eq!(out.groups.len(), 1);
        assert_eq!(out.groups[0].members.len(), 2);
        assert_eq!(out.stats.files_unique_partial, 1);
    }

    #[test]
    fn estimated_read_capped_at_128kib() {
        assert_eq!(estimated_partial_read(50), 50);
        assert_eq!(
            estimated_partial_read(PARTIAL_HASH_MAX_READ_BYTES),
            PARTIAL_HASH_MAX_READ_BYTES
        );
        assert_eq!(
            estimated_partial_read(PARTIAL_HASH_MAX_READ_BYTES + 1),
            PARTIAL_HASH_MAX_READ_BYTES
        );
    }

    #[test]
    fn cancel_stops_between_files() {
        let size_groups = vec![ExactSizeGroup {
            size: ByteSize(10),
            members: vec![CandidateId(1), CandidateId(2), CandidateId(3)],
        }];
        let targets: HashMap<_, _> = [target(1, "a", 10), target(2, "b", 10), target(3, "c", 10)]
            .into_iter()
            .collect();
        let hasher = MapHasher {
            map: [
                ("a".into(), ph(1)),
                ("b".into(), ph(1)),
                ("c".into(), ph(1)),
            ]
            .into_iter()
            .collect(),
            full: HashMap::new(),
            fail: HashMap::new(),
            fail_full: HashMap::new(),
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let out = run_partial_hash_stage(&size_groups, &targets, &hasher, &cancel);
        assert!(out.stats.cancelled);
        assert_eq!(out.stats.files_hashed, 0);
        assert!(out.groups.is_empty());
    }

    #[test]
    fn hash_failure_skips_file() {
        let size_groups = vec![ExactSizeGroup {
            size: ByteSize(10),
            members: vec![CandidateId(1), CandidateId(2), CandidateId(3)],
        }];
        let targets: HashMap<_, _> = [target(1, "a", 10), target(2, "b", 10), target(3, "c", 10)]
            .into_iter()
            .collect();
        let hasher = MapHasher {
            map: [("a".into(), ph(1)), ("c".into(), ph(1))]
                .into_iter()
                .collect(),
            full: HashMap::new(),
            fail: [("b".into(), true)].into_iter().collect(),
            fail_full: HashMap::new(),
        };
        let out =
            run_partial_hash_stage(&size_groups, &targets, &hasher, &CancellationToken::new());
        assert_eq!(out.stats.files_failed, 1);
        assert_eq!(out.stats.files_hashed, 2);
        assert_eq!(out.groups.len(), 1);
    }

    #[test]
    fn spawn_on_worker_pool_completes() {
        use std::sync::{Arc, Mutex};

        use crate::workers::WorkerPoolConfig;

        let pool = WorkerPool::new(WorkerPoolConfig { workers: 1 });
        let size_groups = vec![ExactSizeGroup {
            size: ByteSize(8),
            members: vec![CandidateId(1), CandidateId(2)],
        }];
        let targets: HashMap<_, _> = [target(1, "x", 8), target(2, "y", 8)].into_iter().collect();
        let hasher = Arc::new(MapHasher {
            map: [("x".into(), ph(3)), ("y".into(), ph(3))]
                .into_iter()
                .collect(),
            full: HashMap::new(),
            fail: HashMap::new(),
            fail_full: HashMap::new(),
        });
        let slot = Arc::new(Mutex::new(None));
        let slot2 = Arc::clone(&slot);
        let handle = spawn_partial_hash_stage(
            &pool,
            JobPriority::Background,
            size_groups,
            targets,
            hasher,
            move |r| {
                *slot2.lock().unwrap() = Some(r);
            },
        );
        handle.wait();
        let r = slot.lock().unwrap().take().expect("result");
        assert_eq!(r.groups.len(), 1);
        assert_eq!(r.stats.files_hashed, 2);
        drop(pool);
    }
}
