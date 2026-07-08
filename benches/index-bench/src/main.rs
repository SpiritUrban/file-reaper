//! Бенчмарк-гейт in-memory індексу (T-019).
//!
//! Фіксує пам'ять і швидкість `InMemoryIndex` на детермінованому еталонному
//! наборі та звіряє їх із закоміченим `baseline.json`. Політика беклогу
//! (docs/repository.md §9): регрес метрики понад поріг блокує merge.
//!
//! Метрика пам'яті детермінована (оцінка через `capacity()` однакова на
//! однаковому таргеті) — вона гейтиться жорстко на 15% і реально валить CI.
//! Тайминги залежать від заліза shared-runner'а: за замовчуванням їхній регрес
//! лише попереджає, але кожен має абсолютну стелю зі здоровим запасом. Прапорець
//! `--strict` перетворює регрес будь-якої метрики на помилку — для локального
//! запуску на машині, де знято baseline.
//!
//! Використання:
//!   cargo run --release            # check-режим (гейт для CI)
//!   cargo run --release -- --strict  # жорсткий гейт усіх метрик (локально)
//!   cargo run --release -- --bless   # перезаписати baseline поточними замірами

use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use trashradar_app::ports::HotIndex;
use trashradar_domain::candidate::{
    ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
    FsTimestamp, SafetyLevel,
};
use trashradar_domain::category::CategoryId;
use trashradar_index_memory::InMemoryIndex;

/// Розмір еталонного набору. Тримаємо помірним, щоб гейт у CI укладався в
/// секунди, але достатнім, щоб заміри були стабільні.
const REFERENCE_RECORDS: u64 = 1_000_000;
const BATCH: usize = 100_000;
/// Кількість унікальних директорій (кожна вміщає ~5 записів).
const DIR_BUCKETS: u64 = 200_000;
/// Скільки разів повторюємо швидкі заміри; беремо мінімум (найменш зашумлений).
const TIMING_SAMPLES: usize = 5;
/// Допустимий регрес — 15% (docs/tasks.md T-019).
const TOLERANCE: f64 = 1.15;

/// Записаний зріз метрик еталонного прогону.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Baseline {
    /// Оцінка heap-пам'яті індексу після `finish_indexing`, байти.
    index_memory_bytes: u64,
    /// Час наповнення індексу батчами + `finish_indexing`, мс.
    build_millis: f64,
    /// Пошук рідкісного підрядка (повний прохід усіх записів), мс.
    search_rare_millis: f64,
    /// Пошук частого підрядка з лімітом (рання зупинка), мс.
    search_frequent_millis: f64,
}

/// Опис однієї метрики для звірки з baseline.
struct MetricSpec {
    name: &'static str,
    current: f64,
    baseline: f64,
    /// Чи детермінована метрика (регрес завжди жорсткий).
    deterministic: bool,
    /// Абсолютна стеля: перевищення — завжди помилка.
    ceiling: f64,
    unit: &'static str,
}

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("baseline.json")
}

fn make_record(i: u64) -> FileRecord {
    let dir_idx = i % DIR_BUCKETS;
    FileRecord {
        candidate_id: CandidateId(i),
        path: format!("C:\\Users\\User\\Folder_{dir_idx}\\file_name_{i}.dat"),
        size: ByteSize(i.wrapping_mul(123)),
        created_at: Some(FsTimestamp(130_000_000_000_000_000 + i as i64 * 10_000_000)),
        modified_at: Some(FsTimestamp(140_000_000_000_000_000 + i as i64 * 10_000_000)),
        accessed_at: Some(FsTimestamp(150_000_000_000_000_000 + i as i64 * 10_000_000)),
        kind: FileKind::Video,
        unit: CandidateUnit::File,
        category: CategoryId::ForgottenVideos,
        safety: SafetyLevel::SafeToBulk,
        decision: Decision::Undecided,
        detector_id: String::new(),
        explanation: String::new(),
        attributes: FileAttributes::default(),
    }
}

/// Наповнює індекс еталонним набором; повертає індекс і час наповнення в мс.
fn build_reference_index() -> (InMemoryIndex, f64) {
    let index = InMemoryIndex::new();
    let started = Instant::now();
    let mut batch: Vec<FileRecord> = Vec::with_capacity(BATCH);
    for i in 0..REFERENCE_RECORDS {
        batch.push(make_record(i));
        if batch.len() == BATCH {
            index.insert_batch(std::mem::take(&mut batch)).unwrap();
            batch = Vec::with_capacity(BATCH);
        }
    }
    if !batch.is_empty() {
        index.insert_batch(batch).unwrap();
    }
    index.finish_indexing();
    let build_millis = started.elapsed().as_secs_f64() * 1000.0;
    (index, build_millis)
}

/// Повертає мінімальний час (мс) серед `TIMING_SAMPLES` прогонів `f`.
fn min_millis<F: Fn()>(f: F) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..TIMING_SAMPLES {
        let started = Instant::now();
        f();
        best = best.min(started.elapsed().as_secs_f64() * 1000.0);
    }
    best
}

fn measure() -> Baseline {
    let (index, build_millis) = build_reference_index();
    assert_eq!(
        InMemoryIndex::len(&index) as u64,
        REFERENCE_RECORDS,
        "індекс наповнено не повністю"
    );

    let index_memory_bytes = index.memory_usage() as u64;

    // Рідкісний підрядок: єдиний збіг, повний прохід усіх записів.
    let rare_hits = index.search("file_name_999999", 200).len();
    assert_eq!(rare_hits, 1, "очікувався рівно 1 збіг рідкісного підрядка");
    let search_rare_millis = min_millis(|| {
        std::hint::black_box(index.search("file_name_999999", 200));
    });

    // Частий підрядок: багато збігів, ліміт вмикає ранню зупинку.
    let frequent_hits = index.search("folder_1999", 200).len();
    assert!(frequent_hits > 0, "частий підрядок мусить давати збіги");
    let search_frequent_millis = min_millis(|| {
        std::hint::black_box(index.search("folder_1999", 200));
    });

    Baseline {
        index_memory_bytes,
        build_millis,
        search_rare_millis,
        search_frequent_millis,
    }
}

fn load_baseline() -> Option<Baseline> {
    let path = baseline_path();
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_baseline(baseline: &Baseline) {
    let path = baseline_path();
    let data = serde_json::to_string_pretty(baseline).expect("serialize baseline");
    std::fs::write(&path, data).expect("write baseline");
    println!("baseline записано: {}", path.display());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bless = args.iter().any(|a| a == "--bless");
    let strict = args.iter().any(|a| a == "--strict");

    println!(
        "Еталонний набір: {} записів, {} директорій, {} прогонів на замір",
        REFERENCE_RECORDS, DIR_BUCKETS, TIMING_SAMPLES
    );
    let current = measure();
    println!(
        "Заміри: memory={:.2} МБ, build={:.1} мс, search_rare={:.2} мс, search_frequent={:.2} мс",
        current.index_memory_bytes as f64 / 1024.0 / 1024.0,
        current.build_millis,
        current.search_rare_millis,
        current.search_frequent_millis,
    );

    if bless {
        write_baseline(&current);
        return;
    }

    let Some(baseline) = load_baseline() else {
        eprintln!(
            "ПОМИЛКА: baseline.json відсутній або нечитабельний. \
             Згенеруйте його: cargo run --release -- --bless"
        );
        std::process::exit(2);
    };

    // Абсолютні стелі: щедрий запас, щоб не блимати на повільному CI-раннері,
    // але ловити катастрофічний регрес. Заміри T-018 на 1 млн: пошук ~55 мс.
    let specs = [
        MetricSpec {
            name: "index_memory_bytes",
            current: current.index_memory_bytes as f64,
            baseline: baseline.index_memory_bytes as f64,
            deterministic: true,
            ceiling: 200.0 * 1024.0 * 1024.0,
            unit: "B",
        },
        MetricSpec {
            name: "build_millis",
            current: current.build_millis,
            baseline: baseline.build_millis,
            deterministic: false,
            ceiling: 20_000.0,
            unit: "ms",
        },
        MetricSpec {
            name: "search_rare_millis",
            current: current.search_rare_millis,
            baseline: baseline.search_rare_millis,
            deterministic: false,
            ceiling: 400.0,
            unit: "ms",
        },
        MetricSpec {
            name: "search_frequent_millis",
            current: current.search_frequent_millis,
            baseline: baseline.search_frequent_millis,
            deterministic: false,
            ceiling: 400.0,
            unit: "ms",
        },
    ];

    let mut failed = false;
    println!(
        "\n{:<24} {:>12} {:>12} {:>8}  verdict",
        "metric", "baseline", "current", "ratio"
    );
    for spec in &specs {
        let ratio = if spec.baseline > 0.0 {
            spec.current / spec.baseline
        } else {
            f64::INFINITY
        };
        let regressed = ratio > TOLERANCE;
        let over_ceiling = spec.current > spec.ceiling;

        let hard = over_ceiling || (regressed && (spec.deterministic || strict));
        let verdict = if over_ceiling {
            "FAIL (ceiling)"
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
        eprintln!(
            "\nРегрес перевищив поріг {:.0}% або абсолютну стелю — гейт не пройдено.",
            (TOLERANCE - 1.0) * 100.0
        );
        std::process::exit(1);
    }
    println!(
        "\nГейт пройдено: регресів понад {:.0}% немає.",
        (TOLERANCE - 1.0) * 100.0
    );
}
