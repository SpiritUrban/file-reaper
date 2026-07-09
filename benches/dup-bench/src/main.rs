//! Бенчмарк-гейт каскаду дублікатів (T-066).
//!
//! Еталонна «медіатека» — синтетичний корпус метаданих (без реального диску):
//! MapHasher емулює partial/full. Дві сесії:
//!
//! 1. **Перша** — холодний каскад size→partial→full (без кешу).
//! 2. **Повторна** — той самий корпус + MemoryHashCache після session 1
//!    (T-062): disk_reads мають бути 0.
//!
//! Політика як T-019/T-035: baseline.json; регрес >15% для детермінованих
//! метрик і абсолютні стелі для таймингів; `--strict` / `--bless`.
//!
//! ```sh
//! cargo run --release --manifest-path benches/dup-bench/Cargo.toml
//! cargo run --release --manifest-path benches/dup-bench/Cargo.toml -- --strict
//! cargo run --release --manifest-path benches/dup-bench/Cargo.toml -- --bless
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use trashradar_app::duplicates::{
    mark_confirmed_groups, run_duplicate_cascade_with_cache, CountingHasher, MapHasher,
    MemoryHashCache,
};
use trashradar_app::workers::CancellationToken;
use trashradar_domain::candidate::{
    ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
    FsTimestamp, SafetyLevel,
};
use trashradar_domain::category::CategoryId;
use trashradar_domain::duplicates::{ContentHash, KeepPolicy, PartialHash};

/// Розмір еталонної «медіатеки» (метадані).
const CORPUS_FILES: u64 = 50_000;
/// Скільки size-груп-дублікатів (по 3 файли = reclaim 2×size).
const DUP_GROUPS: u64 = 2_000;
/// Файлів у кожній dup-групі.
const DUP_GROUP_SIZE: u64 = 3;
/// Допустимий регрес baseline.
const TOLERANCE: f64 = 1.15;
/// Стеля першої сесії (мс) — щедрий запас для shared-runner.
const CEILING_FIRST_MS: f64 = 8_000.0;
/// Стеля повторної сесії (мс) — має бути суттєво швидшою.
const CEILING_SECOND_MS: f64 = 2_000.0;
/// Повторна: обов'язково 0 disk reads (T-062).
const SECOND_DISK_READS_MUST_BE: u64 = 0;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Baseline {
    first_session_millis: f64,
    second_session_millis: f64,
    /// Детерміновано: 0 на повторній.
    second_disk_reads: u64,
    confirmed_groups: u64,
    reclaimable_bytes: u64,
    /// partial+full disk reads першої сесії (орієнтир).
    first_disk_reads: u64,
}

struct MetricSpec {
    name: &'static str,
    current: f64,
    baseline: f64,
    higher_is_worse: bool,
    deterministic: bool,
    ceiling: f64,
    unit: &'static str,
}

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("baseline.json")
}

/// Синтетична медіатека: dup-групи однакового size + unique files.
fn build_corpus() -> (Vec<FileRecord>, MapHasher) {
    let mut records = Vec::with_capacity(CORPUS_FILES as usize);
    let mut partial_map = HashMap::new();
    let mut full_map = HashMap::new();

    let mut id = 0u64;
    // Дублікати: group g має size = (g+1)*1_000_000, 3 файли, спільний partial+full.
    for g in 0..DUP_GROUPS {
        let size = (g + 1) * 1_000_000;
        let ph_byte = (g % 250 + 1) as u8;
        let fh_byte = (g % 200 + 1) as u8;
        let mut ph = [0u8; 32];
        ph[0] = ph_byte;
        ph[1] = (g >> 8) as u8;
        let mut fh = [0u8; 32];
        fh[0] = fh_byte;
        fh[2] = (g >> 8) as u8;
        for k in 0..DUP_GROUP_SIZE {
            let path = format!(r"C:\Media\lib_{g}\clip_{k}.mp4");
            partial_map.insert(path.clone(), PartialHash(ph));
            full_map.insert(path.clone(), ContentHash(fh));
            records.push(FileRecord {
                candidate_id: CandidateId(id),
                path,
                size: ByteSize(size),
                created_at: Some(FsTimestamp(1000 + g as i64)),
                modified_at: Some(FsTimestamp(2000 + (g * 10 + k) as i64)),
                accessed_at: None,
                kind: FileKind::Video,
                unit: CandidateUnit::File,
                category: CategoryId::Uncategorized,
                safety: SafetyLevel::ReviewRecommended,
                decision: Decision::Undecided,
                detector_id: String::new(),
                explanation: String::new(),
                attributes: FileAttributes::default(),
            });
            id += 1;
        }
    }

    // Унікальні «медіа»-файли.
    while id < CORPUS_FILES {
        let path = format!(r"C:\Media\unique\u_{id}.mp4");
        let size = 50_000 + id * 17; // unique sizes
        partial_map.insert(path.clone(), {
            let mut a = [0u8; 32];
            a[0] = 0xFE;
            a[1] = (id % 255) as u8;
            PartialHash(a)
        });
        full_map.insert(path.clone(), {
            let mut a = [0u8; 32];
            a[0] = 0xFD;
            a[1] = (id % 255) as u8;
            ContentHash(a)
        });
        records.push(FileRecord {
            candidate_id: CandidateId(id),
            path,
            size: ByteSize(size),
            created_at: Some(FsTimestamp(id as i64)),
            modified_at: Some(FsTimestamp(id as i64 + 1)),
            accessed_at: None,
            kind: FileKind::Video,
            unit: CandidateUnit::File,
            category: CategoryId::Uncategorized,
            safety: SafetyLevel::ReviewRecommended,
            decision: Decision::Undecided,
            detector_id: String::new(),
            explanation: String::new(),
            attributes: FileAttributes::default(),
        });
        id += 1;
    }

    let hasher = MapHasher {
        map: partial_map,
        full: full_map,
        fail: HashMap::new(),
        fail_full: HashMap::new(),
    };
    (records, hasher)
}

fn expected_reclaim_bytes() -> u64 {
    // Кожна dup-група: size * (3-1) = 2 * size; size = (g+1)*1e6
    (0..DUP_GROUPS).map(|g| 2 * (g + 1) * 1_000_000).sum()
}

fn measure_sessions() -> Baseline {
    let (records, map_hasher) = build_corpus();
    let hasher = CountingHasher::new(map_hasher);
    let cache = MemoryHashCache::new();
    let cancel = CancellationToken::new();

    // --- Session 1: cold ---
    hasher.reset_counts();
    let t0 = Instant::now();
    let r1 = run_duplicate_cascade_with_cache(&records, &hasher, &cancel, 4, Some(&cache), |_| {});
    let first_session_millis = t0.elapsed().as_secs_f64() * 1000.0;
    let first_disk_reads = hasher.partial_reads() + hasher.full_reads();
    let confirmed_groups = r1.confirmed_groups.len() as u64;
    let reclaimable_bytes = r1.state.reclaimable_bytes;

    // Розмітка Keep (T-065) — у гейт не входить, але прогріває path.
    let _marked = mark_confirmed_groups(
        &r1.confirmed_groups,
        &records,
        KeepPolicy::PreferOldestModified,
    );

    // --- Session 2: warm cache (DoD T-062/T-066: 0 disk reads) ---
    hasher.reset_counts();
    let t1 = Instant::now();
    let r2 = run_duplicate_cascade_with_cache(&records, &hasher, &cancel, 4, Some(&cache), |_| {});
    let second_session_millis = t1.elapsed().as_secs_f64() * 1000.0;
    // CountingHasher — джерело правди: cache hit не викликає Hasher.
    let second_disk_reads = hasher.partial_reads() + hasher.full_reads();
    debug_assert_eq!(r2.partial.disk_reads, 0);
    debug_assert_eq!(r2.full.as_ref().map(|f| f.disk_reads), Some(0));
    // Результат сесії 2 має збігатись із підтвердженою цифрою сесії 1.
    assert_eq!(r2.confirmed_groups.len() as u64, confirmed_groups);
    assert_eq!(r2.state.reclaimable_bytes, reclaimable_bytes);

    Baseline {
        first_session_millis,
        second_session_millis,
        second_disk_reads,
        confirmed_groups,
        reclaimable_bytes,
        first_disk_reads,
    }
}

fn load_baseline() -> Option<Baseline> {
    let data = std::fs::read_to_string(baseline_path()).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_baseline(b: &Baseline) {
    let path = baseline_path();
    let data = serde_json::to_string_pretty(b).expect("serialize");
    std::fs::write(&path, data).expect("write baseline");
    println!("baseline записано: {}", path.display());
}

fn run_gate(bless: bool, strict: bool) -> i32 {
    println!(
        "T-066 dup-bench: corpus={} files, dup_groups={}×{}",
        CORPUS_FILES, DUP_GROUPS, DUP_GROUP_SIZE
    );
    println!("session1 = cold cascade; session2 = warm HashCache (T-062)");

    let current = measure_sessions();
    let expected_reclaim = expected_reclaim_bytes();

    println!(
        "session1: {:.1} мс, disk_reads={}",
        current.first_session_millis, current.first_disk_reads
    );
    println!(
        "session2: {:.1} мс, disk_reads={} (ціль 0)",
        current.second_session_millis, current.second_disk_reads
    );
    println!(
        "groups={}, reclaim={} B (expected {})",
        current.confirmed_groups, current.reclaimable_bytes, expected_reclaim
    );

    // Інваріанти DoD (незалежно від baseline).
    let mut failed = false;
    if current.second_disk_reads != SECOND_DISK_READS_MUST_BE {
        eprintln!(
            "DoD T-062/T-066: second_disk_reads={} (очікувалось {})",
            current.second_disk_reads, SECOND_DISK_READS_MUST_BE
        );
        failed = true;
    }
    if current.confirmed_groups != DUP_GROUPS {
        eprintln!(
            "DoD: confirmed_groups={} (очікувалось {})",
            current.confirmed_groups, DUP_GROUPS
        );
        failed = true;
    }
    if current.reclaimable_bytes != expected_reclaim {
        eprintln!(
            "DoD: reclaimable_bytes={} (очікувалось {})",
            current.reclaimable_bytes, expected_reclaim
        );
        failed = true;
    }
    if current.first_session_millis > CEILING_FIRST_MS {
        eprintln!(
            "Стеля session1: {:.1} > {:.0} мс",
            current.first_session_millis, CEILING_FIRST_MS
        );
        failed = true;
    }
    if current.second_session_millis > CEILING_SECOND_MS {
        eprintln!(
            "Стеля session2: {:.1} > {:.0} мс",
            current.second_session_millis, CEILING_SECOND_MS
        );
        failed = true;
    }

    if bless {
        write_baseline(&current);
        return if failed { 1 } else { 0 };
    }

    let Some(baseline) = load_baseline() else {
        eprintln!("ПОМИЛКА: baseline.json відсутній. Згенеруйте: cargo run --release -- --bless");
        return 2;
    };

    let specs = [
        MetricSpec {
            name: "first_session_millis",
            current: current.first_session_millis,
            baseline: baseline.first_session_millis,
            higher_is_worse: true,
            deterministic: false,
            ceiling: CEILING_FIRST_MS,
            unit: "ms",
        },
        MetricSpec {
            name: "second_session_millis",
            current: current.second_session_millis,
            baseline: baseline.second_session_millis,
            higher_is_worse: true,
            deterministic: false,
            ceiling: CEILING_SECOND_MS,
            unit: "ms",
        },
        MetricSpec {
            name: "second_disk_reads",
            current: current.second_disk_reads as f64,
            baseline: baseline.second_disk_reads as f64,
            higher_is_worse: true,
            deterministic: true,
            ceiling: 0.0,
            unit: "reads",
        },
        MetricSpec {
            name: "confirmed_groups",
            current: current.confirmed_groups as f64,
            baseline: baseline.confirmed_groups as f64,
            higher_is_worse: false, // must equal — checked as absolute
            deterministic: true,
            ceiling: DUP_GROUPS as f64,
            unit: "groups",
        },
        MetricSpec {
            name: "reclaimable_bytes",
            current: current.reclaimable_bytes as f64,
            baseline: baseline.reclaimable_bytes as f64,
            higher_is_worse: false,
            deterministic: true,
            ceiling: expected_reclaim as f64,
            unit: "B",
        },
    ];

    println!(
        "\n{:<24} {:>12} {:>12} {:>8}  verdict",
        "metric", "baseline", "current", "ratio"
    );
    for spec in &specs {
        // Детерміновані інваріанти: рівність очікуваному (ceiling = expected).
        if matches!(
            spec.name,
            "confirmed_groups" | "reclaimable_bytes" | "second_disk_reads"
        ) {
            let equal_ok = if spec.name == "second_disk_reads" {
                spec.current == 0.0
            } else {
                (spec.current - spec.ceiling).abs() < 0.5
            };
            let verdict = if equal_ok { "ok" } else { "FAIL" };
            if !equal_ok {
                failed = true;
            }
            println!(
                "{:<24} {:>12.0} {:>12.0} {:>7}  {} [{}]",
                spec.name, spec.baseline, spec.current, "—", verdict, spec.unit
            );
            continue;
        }

        let (ratio, regressed, over) = if spec.higher_is_worse {
            let ratio = if spec.baseline > 0.0 {
                spec.current / spec.baseline
            } else {
                f64::INFINITY
            };
            (ratio, ratio > TOLERANCE, spec.current > spec.ceiling)
        } else {
            let ratio = if spec.current > 0.0 {
                spec.baseline / spec.current
            } else {
                f64::INFINITY
            };
            (ratio, ratio > TOLERANCE, spec.current < spec.ceiling)
        };
        let hard = over || (regressed && (spec.deterministic || strict));
        let verdict = if over {
            if spec.higher_is_worse {
                "FAIL (ceiling)"
            } else {
                "FAIL (floor)"
            }
        } else if regressed && hard {
            "FAIL (regress)"
        } else if regressed {
            "WARN (regress)"
        } else {
            "ok"
        };
        if hard {
            failed = true;
        }
        println!(
            "{:<24} {:>12.1} {:>12.1} {:>7.2}x  {} [{}]",
            spec.name, spec.baseline, spec.current, ratio, verdict, spec.unit
        );
    }

    if failed {
        eprintln!("\nT-066 гейт не пройдено.");
        return 1;
    }
    println!(
        "\nГейт пройдено: session1 < {:.0} с, session2 < {:.0} с, cache hits (0 disk reads).",
        CEILING_FIRST_MS / 1000.0,
        CEILING_SECOND_MS / 1000.0
    );
    0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bless = args.iter().any(|a| a == "--bless");
    let strict = args.iter().any(|a| a == "--strict");
    let code = run_gate(bless, strict);
    std::process::exit(code);
}
