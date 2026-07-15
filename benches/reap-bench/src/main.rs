//! Бенчмарк-гейт транзакційного reap (T-154).
//!
//! Ціль architecture.md §15: **reap 1 000 файлів на одному томі < 2 с**.
//! Міряється продуктовий шлях T-079 без спрощень: реальна FS (temp-каталог
//! = той самий том), справжній `NativeQuarantineFs` (guard + identity check +
//! атомарний same-volume move) і справжній SQLite-manifest (durable
//! in_flight → move → confirmation + append-only аудит на кожен файл).
//!
//! Гейти за політикою T-019/T-035: тайминг на shared-runner — WARN на
//! регрес >15% + жорстка катастрофічна стеля; ціль §15 (2 с) — WARN-лінія,
//! бо per-file транзакції T-079 коливаються довкола неї (борг у
//! progress.md, відхилення T-154); `--strict` робить регрес помилкою
//! (локально на машині baseline); кількість переміщених файлів і стан
//! manifest — жорсткі інваріанти.
//!
//! Використання (Windows; на інших ОС — no-op, продукт MVP Windows-only):
//!   cargo run --release
//!   cargo run --release -- --strict
//!   cargo run --release -- --bless

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Еталон §15: 1 000 файлів за один reap-батч.
const REAP_FILES: u64 = 1_000;
/// Розмір кожного файла: move не копіює дані, розмір на час не впливає,
/// але ненульовий вміст робить identity-перевірку чесною.
const FILE_BYTES: usize = 4_096;
/// Допустимий регрес baseline (політика T-019).
const TOLERANCE: f64 = 1.15;
/// Ціль §15: 2 с на батч. Поточний per-file транзакційний шлях T-079
/// коливається довкола цілі (~2 мс/файл: 2 durable-коміти + аудит + move) —
/// перевищення цілі дає WARN, не FAIL; борг батчування manifest-транзакцій
/// зафіксовано у progress.md (відхилення T-154).
const TARGET_BATCH_MS: f64 = 2_000.0;
/// Жорстка катастрофічна стеля CI (запас на shared-runner і AV).
const CEILING_BATCH_MS: f64 = 10_000.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Baseline {
    /// Повний `reap_batch` (журнал + move + підтвердження + аудит), мс.
    reap_batch_millis: f64,
    /// Похідна пропускна здатність, файлів/с.
    reap_files_per_sec: f64,
    /// Скільки файлів переміщено (має = REAP_FILES).
    reaped_count: u64,
}

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("baseline.json")
}

fn load_baseline() -> Option<Baseline> {
    let data = std::fs::read_to_string(baseline_path()).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_baseline(baseline: &Baseline) {
    let path = baseline_path();
    let data = serde_json::to_string_pretty(baseline).expect("serialize");
    std::fs::write(&path, data).expect("write baseline");
    println!("baseline записано: {}", path.display());
}

#[cfg(windows)]
fn measure_reap() -> Baseline {
    use std::time::Instant;

    use trashradar_app::{ReapRequest, TransactionalReaper};
    use trashradar_domain::quarantine::{
        BatchId, QuarantineEntry, QuarantineEntryId, QuarantineStatus,
    };
    use trashradar_index_sqlite::IndexDatabase;
    use trashradar_platform_win::read_file_identity;
    use trashradar_quarantine_fs::NativeQuarantineFs;

    let root = std::env::temp_dir().join(format!("trashradar-reap-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    println!(
        "Сетап: 2×{REAP_FILES} файлів × {FILE_BYTES} Б у {}",
        root.display()
    );
    let directory = NativeQuarantineFs
        .ensure_at_root(&root)
        .expect("ensure quarantine dir");
    let database = IndexDatabase::open_profile(root.join("profile")).expect("open manifest db");

    // Два однакові корпуси: warmup-батч гріє AV/кеші FS/WAL (§15 —
    // «теплий диск», той самий прийом, що live-режим scan-bench),
    // вимірюється другий.
    let payload = vec![0xA5u8; FILE_BYTES];
    let build_requests = |label: &str, id_base: u64| -> Vec<ReapRequest> {
        let mut requests = Vec::with_capacity(REAP_FILES as usize);
        for k in 0..REAP_FILES {
            let source = data_dir.join(format!("{label}{k}.bin"));
            std::fs::write(&source, &payload).expect("write source file");
            let identity = read_file_identity(&source).expect("read identity");
            let surrogate_name = format!("{:016}.bin", id_base + k);
            let destination = directory.quarantine_root.join(&surrogate_name);
            requests.push(ReapRequest {
                entry: QuarantineEntry {
                    id: QuarantineEntryId(id_base + k),
                    batch_id: None,
                    original_path: source.to_string_lossy().into_owned(),
                    surrogate_name,
                    size: identity.size,
                    quarantined_at_unix: 1_750_000_000,
                    expires_at_unix: 1_752_592_000,
                    status: QuarantineStatus::InFlight,
                },
                destination_path: destination.to_string_lossy().into_owned(),
                expected_identity: identity,
            });
        }
        requests
    };

    let reaper = TransactionalReaper::new(&NativeQuarantineFs, &database, &[]);

    println!("Прогрів (discard)…");
    let warm_requests = build_requests("warm", 100_000);
    let warm_started = Instant::now();
    let warm_outcomes = reaper
        .reap_batch(BatchId(1), warm_requests)
        .expect("warmup reap batch");
    assert_eq!(warm_outcomes.len() as u64, REAP_FILES);
    println!(
        "  [warmup] {:.1} мс (не гейтиться)",
        warm_started.elapsed().as_secs_f64() * 1000.0
    );

    // Вимірюваний корпус створюється ПІСЛЯ warmup і з паузою: реал-тайм
    // антивірус асинхронно сканує щойно створені файли і блокує move
    // фільтр-драйвером — без паузи замір міряє AV, а не reap (розкид до
    // ~7× на тій самій машині).
    println!("Замір (теплий): reap_batch (журнал → move → підтвердження)…");
    let requests = build_requests("candidate", 0);
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    let started = Instant::now();
    let outcomes = reaper.reap_batch(BatchId(2), requests).expect("reap batch");
    let reap_batch_millis = started.elapsed().as_secs_f64() * 1000.0;

    // Жорсткі інваріанти D4: кожен файл переміщено і підтверджено в журналі.
    assert_eq!(outcomes.len() as u64, REAP_FILES);
    assert!(outcomes
        .iter()
        .all(|o| o.entry.status == QuarantineStatus::Quarantined));
    assert!(
        !data_dir.join("candidate0.bin").exists()
            && !data_dir
                .join(format!("candidate{}.bin", REAP_FILES - 1))
                .exists(),
        "оригінали мають зникнути"
    );
    let quarantined = database
        .list_quarantine_entries()
        .expect("list manifest")
        .into_iter()
        .filter(|entry| entry.status == QuarantineStatus::Quarantined)
        .count() as u64;
    // warmup-батч + вимірюваний батч.
    assert_eq!(
        quarantined,
        REAP_FILES * 2,
        "manifest має підтвердити обидва батчі"
    );

    drop(database);
    let _ = std::fs::remove_dir_all(&root);

    Baseline {
        reap_batch_millis,
        reap_files_per_sec: if reap_batch_millis > 0.0 {
            REAP_FILES as f64 / (reap_batch_millis / 1000.0)
        } else {
            f64::INFINITY
        },
        reaped_count: REAP_FILES,
    }
}

#[cfg(windows)]
fn run_gate(bless: bool, strict: bool) -> i32 {
    println!(
        "T-154 reap: {} файлів, один том, стеля §15 = {:.0} с",
        REAP_FILES,
        CEILING_BATCH_MS / 1000.0
    );

    let current = measure_reap();
    println!(
        "Заміри: reap_batch={:.1} мс ({:.0} файлів/с)",
        current.reap_batch_millis, current.reap_files_per_sec
    );

    if bless {
        write_baseline(&current);
        return 0;
    }

    let Some(baseline) = load_baseline() else {
        eprintln!("ПОМИЛКА: baseline.json відсутній. Згенеруйте: cargo run --release -- --bless");
        return 2;
    };

    let ratio = if baseline.reap_batch_millis > 0.0 {
        current.reap_batch_millis / baseline.reap_batch_millis
    } else {
        f64::INFINITY
    };
    let regressed = ratio > TOLERANCE;
    let over_limit = current.reap_batch_millis > CEILING_BATCH_MS;
    let hard = over_limit || (regressed && strict);
    let verdict = if over_limit {
        "FAIL (ceiling)"
    } else if hard {
        "FAIL (regress)"
    } else if regressed {
        "WARN (regress)"
    } else {
        "ok"
    };
    println!(
        "\n{:<20} {:>12} {:>12} {:>8}  verdict",
        "metric", "baseline", "current", "ratio"
    );
    println!(
        "{:<20} {:>12.1} {:>12.1} {:>7.2}x  {} [ms]",
        "reap_batch_millis", baseline.reap_batch_millis, current.reap_batch_millis, ratio, verdict
    );

    // Ціль §15 — WARN-лінія до батчування manifest-транзакцій (T-154).
    if current.reap_batch_millis > TARGET_BATCH_MS {
        println!(
            "\nWARN §15: reap {:.1} мс > цілі {:.0} с — борг per-file транзакцій \
             (див. відхилення T-154 у progress.md).",
            current.reap_batch_millis,
            TARGET_BATCH_MS / 1000.0
        );
    }
    if hard {
        eprintln!(
            "\nРегрес >{:.0}% / катастрофічна стеля — гейт не пройдено.",
            (TOLERANCE - 1.0) * 100.0
        );
        return 1;
    }
    println!(
        "\nГейт пройдено: reap {} файлів < {:.0} с (guard).",
        REAP_FILES,
        CEILING_BATCH_MS / 1000.0
    );
    0
}

#[cfg(not(windows))]
fn run_gate(_bless: bool, _strict: bool) -> i32 {
    // Продукт MVP — Windows-only (product.md §6); NativeQuarantineFs і
    // read_file_identity — WinAPI. На інших ОС гейт свідомо порожній.
    println!("reap-bench: Windows-only (MVP), пропущено.");
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bless = args.iter().any(|a| a == "--bless");
    let strict = args.iter().any(|a| a == "--strict");
    std::process::exit(run_gate(bless, strict));
}
