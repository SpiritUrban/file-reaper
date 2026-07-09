//! Щабель 3: повний потоковий BLAKE3 (T-060).
//!
//! Багатопотоковість — паралель **файлів** (диск/CPU); пам'ять на файл
//! константна (буфер у адаптері Hasher). Ліміт на том — T-063.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;

use trashradar_domain::candidate::CandidateId;
use trashradar_domain::duplicates::{
    group_by_content_hash, ContentHashGroup, ContentHashKey, ContentHashStageStats,
    PartialHashGroup,
};

use crate::duplicates::stage2::HashTarget;
use crate::ports::Hasher;
use crate::workers::{CancellationToken, JobHandle, JobPriority, WorkerPool};

/// Результат щабля 3 — підтверджені групи дублікатів.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullHashStageResult {
    pub groups: Vec<ContentHashGroup>,
    pub stats: ContentHashStageStats,
}

/// Скільки потоків файлів за замовчуванням (диск-bound: ≤ CPU count).
pub fn default_file_workers() -> usize {
    thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, 8)
}

/// Прогін щабля 3 над групами після partial-хешу.
///
/// - `file_workers == 1` — послідовно;
/// - `file_workers > 1` — work-stealing по файлах (спільний лічильник);
/// - cancel між файлами / всередині `full_hash` (адаптер);
/// - помилка одного файла → skip, решта триває.
pub fn run_full_hash_stage(
    partial_groups: &[PartialHashGroup],
    targets: &HashMap<CandidateId, HashTarget>,
    hasher: &dyn Hasher,
    cancel: &CancellationToken,
    file_workers: usize,
) -> FullHashStageResult {
    let workers = file_workers.max(1);

    // Плоский список задач: (id, size з групи — узгоджений з partial).
    let mut jobs: Vec<(CandidateId, trashradar_domain::candidate::ByteSize)> = Vec::new();
    for g in partial_groups {
        for &id in &g.members {
            jobs.push((id, g.size));
        }
    }

    if jobs.is_empty() {
        return FullHashStageResult {
            groups: vec![],
            stats: ContentHashStageStats::from_groups(
                0,
                0,
                0,
                cancel.is_cancelled(),
                workers as u32,
                &[],
            ),
        };
    }

    if workers == 1 || jobs.len() == 1 {
        return run_full_hash_sequential(&jobs, targets, hasher, cancel, workers as u32);
    }

    run_full_hash_parallel(&jobs, targets, hasher, cancel, workers)
}

fn run_full_hash_sequential(
    jobs: &[(CandidateId, trashradar_domain::candidate::ByteSize)],
    targets: &HashMap<CandidateId, HashTarget>,
    hasher: &dyn Hasher,
    cancel: &CancellationToken,
    file_workers: u32,
) -> FullHashStageResult {
    let mut keys = Vec::new();
    let mut files_hashed = 0u64;
    let mut files_failed = 0u64;
    let mut bytes_read = 0u64;
    let mut cancelled = false;

    for &(id, size) in jobs {
        if cancel.is_cancelled() {
            cancelled = true;
            break;
        }
        let Some(target) = targets.get(&id) else {
            files_failed += 1;
            continue;
        };
        // size з групи (метадані індексу); path з target
        match hasher.full_hash(&target.path, size, cancel) {
            Ok(content_hash) => {
                files_hashed += 1;
                bytes_read += size.0;
                keys.push(ContentHashKey {
                    candidate_id: id,
                    size,
                    content_hash,
                });
            }
            Err(e) if e.code == trashradar_domain::error::ErrorCode::Cancelled => {
                cancelled = true;
                break;
            }
            Err(_) => {
                files_failed += 1;
            }
        }
    }

    let groups = group_by_content_hash(keys);
    let stats = ContentHashStageStats::from_groups(
        files_hashed,
        files_failed,
        bytes_read,
        cancelled,
        file_workers,
        &groups,
    );
    FullHashStageResult { groups, stats }
}

fn run_full_hash_parallel(
    jobs: &[(CandidateId, trashradar_domain::candidate::ByteSize)],
    targets: &HashMap<CandidateId, HashTarget>,
    hasher: &dyn Hasher,
    cancel: &CancellationToken,
    workers: usize,
) -> FullHashStageResult {
    let next = AtomicUsize::new(0);
    let files_hashed = AtomicU64::new(0);
    let files_failed = AtomicU64::new(0);
    let bytes_read = AtomicU64::new(0);
    let cancelled_flag = AtomicBool::new(false);
    let keys: Mutex<Vec<ContentHashKey>> = Mutex::new(Vec::new());

    // hasher & targets & jobs shared read-only across threads.
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if cancel.is_cancelled() || cancelled_flag.load(Ordering::Acquire) {
                    cancelled_flag.store(true, Ordering::Release);
                    break;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= jobs.len() {
                    break;
                }
                let (id, size) = jobs[i];
                let Some(target) = targets.get(&id) else {
                    files_failed.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                match hasher.full_hash(&target.path, size, cancel) {
                    Ok(content_hash) => {
                        files_hashed.fetch_add(1, Ordering::Relaxed);
                        bytes_read.fetch_add(size.0, Ordering::Relaxed);
                        keys.lock().expect("keys mutex").push(ContentHashKey {
                            candidate_id: id,
                            size,
                            content_hash,
                        });
                    }
                    Err(e) if e.code == trashradar_domain::error::ErrorCode::Cancelled => {
                        cancelled_flag.store(true, Ordering::Release);
                        break;
                    }
                    Err(_) => {
                        files_failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });

    let keys = keys.into_inner().expect("keys mutex");
    let groups = group_by_content_hash(keys);
    let stats = ContentHashStageStats::from_groups(
        files_hashed.load(Ordering::Relaxed),
        files_failed.load(Ordering::Relaxed),
        bytes_read.load(Ordering::Relaxed),
        cancelled_flag.load(Ordering::Relaxed) || cancel.is_cancelled(),
        workers as u32,
        &groups,
    );
    FullHashStageResult { groups, stats }
}

/// Поставити щабель 3 у [`WorkerPool`].
pub fn spawn_full_hash_stage<H>(
    pool: &WorkerPool,
    priority: JobPriority,
    partial_groups: Vec<PartialHashGroup>,
    targets: HashMap<CandidateId, HashTarget>,
    hasher: std::sync::Arc<H>,
    file_workers: usize,
    on_done: impl FnOnce(FullHashStageResult) + Send + 'static,
) -> JobHandle
where
    H: Hasher + 'static,
{
    pool.submit(priority, move |cancel| {
        let result = run_full_hash_stage(
            &partial_groups,
            &targets,
            hasher.as_ref(),
            &cancel,
            file_workers,
        );
        on_done(result);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duplicates::stage2::{HashTarget, MapHasher};
    use trashradar_domain::candidate::ByteSize;
    use trashradar_domain::duplicates::{ContentHash, PartialHash};

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

    fn ch(b: u8) -> ContentHash {
        let mut a = [0u8; 32];
        a[0] = b;
        ContentHash(a)
    }

    fn partial_group(size: u64, ids: &[u64]) -> PartialHashGroup {
        PartialHashGroup {
            size: ByteSize(size),
            partial_hash: PartialHash([0u8; 32]),
            members: ids.iter().map(|&i| CandidateId(i)).collect(),
        }
    }

    #[test]
    fn different_full_hashes_split_partial_group() {
        let groups = [partial_group(1000, &[1, 2, 3, 4])];
        let targets: HashMap<_, _> = [
            target(1, "a", 1000),
            target(2, "b", 1000),
            target(3, "c", 1000),
            target(4, "d", 1000),
        ]
        .into_iter()
        .collect();
        let hasher = MapHasher {
            map: HashMap::new(),
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
        let out = run_full_hash_stage(&groups, &targets, &hasher, &CancellationToken::new(), 1);
        assert_eq!(out.groups.len(), 2);
        assert_eq!(out.stats.files_hashed, 4);
        assert_eq!(out.stats.bytes_read, 4000);
        assert_eq!(out.stats.file_workers, 1);
    }

    #[test]
    fn parallel_matches_sequential_grouping() {
        let groups = [partial_group(64, &[1, 2, 3, 4, 5, 6])];
        let targets: HashMap<_, _> = (1..=6).map(|i| target(i, &format!("f{i}"), 64)).collect();
        let hasher = MapHasher {
            map: HashMap::new(),
            full: [
                ("f1".into(), ch(9)),
                ("f2".into(), ch(9)),
                ("f3".into(), ch(8)),
                ("f4".into(), ch(8)),
                ("f5".into(), ch(7)), // alone
                ("f6".into(), ch(9)),
            ]
            .into_iter()
            .collect(),
            fail: HashMap::new(),
            fail_full: HashMap::new(),
        };
        let seq = run_full_hash_stage(&groups, &targets, &hasher, &CancellationToken::new(), 1);
        let par = run_full_hash_stage(&groups, &targets, &hasher, &CancellationToken::new(), 4);
        assert_eq!(seq.groups.len(), par.groups.len());
        assert_eq!(seq.stats.files_in_groups, par.stats.files_in_groups);
        assert_eq!(par.stats.file_workers, 4);
        // 9×3 + 8×2 = groups 2; unique content for f5
        assert_eq!(par.groups.len(), 2);
        assert_eq!(par.stats.files_unique_content, 1);
    }

    #[test]
    fn cancel_before_start() {
        let groups = [partial_group(10, &[1, 2])];
        let targets: HashMap<_, _> = [target(1, "a", 10), target(2, "b", 10)]
            .into_iter()
            .collect();
        let hasher = MapHasher {
            map: HashMap::new(),
            full: [("a".into(), ch(1)), ("b".into(), ch(1))]
                .into_iter()
                .collect(),
            fail: HashMap::new(),
            fail_full: HashMap::new(),
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let out = run_full_hash_stage(&groups, &targets, &hasher, &cancel, 1);
        assert!(out.stats.cancelled);
        assert_eq!(out.stats.files_hashed, 0);
    }

    #[test]
    fn spawn_on_pool() {
        use crate::workers::WorkerPoolConfig;
        use std::sync::{Arc, Mutex};

        let pool = WorkerPool::new(WorkerPoolConfig { workers: 1 });
        let groups = vec![partial_group(8, &[1, 2])];
        let targets: HashMap<_, _> = [target(1, "x", 8), target(2, "y", 8)].into_iter().collect();
        let hasher = Arc::new(MapHasher {
            map: HashMap::new(),
            full: [("x".into(), ch(3)), ("y".into(), ch(3))]
                .into_iter()
                .collect(),
            fail: HashMap::new(),
            fail_full: HashMap::new(),
        });
        let slot = Arc::new(Mutex::new(None));
        let slot2 = Arc::clone(&slot);
        let handle = spawn_full_hash_stage(
            &pool,
            JobPriority::Background,
            groups,
            targets,
            hasher,
            2,
            move |r| {
                *slot2.lock().unwrap() = Some(r);
            },
        );
        handle.wait();
        let r = slot.lock().unwrap().take().expect("result");
        assert_eq!(r.groups.len(), 1);
        drop(pool);
    }
}
