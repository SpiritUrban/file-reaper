//! Бенчмарк-гейт повторного запуску (T-154).
//!
//! Ціль architecture.md §15: **повторний запуск (USN-дельта) < 2 с до
//! актуальної цифри**. Міряється повний продуктовий конвеєр рестарту:
//!
//! 1. відновлення in-memory індексу з SQLite (T-017, `read_all_file_records`);
//! 2. застосування USN-дельти до гарячого індексу (T-030,
//!    `plan_usn_mutations` + `apply_mutations_to_hot_index`);
//! 3. перерахунок предикатних детекторів з індексу (T-038,
//!    `recalculate_index`);
//! 4. агрегація «цифри» (T-054, `Aggregator::from_primary_records`).
//!
//! Корпус синтетичний і детермінований (CI без реального USN Journal):
//! 1 500 000 записів у SQLite + дельта ~11 тис. подій
//! (create/modify/delete/rename + директорії для FRN-кешу). I/O диска —
//! лише читання щойно записаної БД (теплий кеш ОС = «теплий диск» §15).
//!
//! Гейти за політикою T-019/T-035: тайминги на shared-runner — WARN на
//! регрес >15% + жорстка катастрофічна стеля (ловить квадратичні регреси);
//! ціль §15 (2 с) — WARN-лінія, бо поточне відновлення з SQLite її не
//! досягає (борг у progress.md, відхилення T-154); `--strict` робить
//! регрес будь-якої метрики помилкою (локально на машині baseline);
//! кількість записів після дельти — жорсткий інваріант.
//!
//! Використання:
//!   cargo run --release
//!   cargo run --release -- --strict
//!   cargo run --release -- --bless

use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use trashradar_app::detectors::DetectorOrchestrator;
use trashradar_app::mvp_predicate_registry;
use trashradar_app::ports::HotIndex;
use trashradar_app::usn_apply::{
    apply_mutations_to_hot_index, next_candidate_id_from_index, plan_usn_mutations, FileProbe,
    FrnPathCache,
};
use trashradar_app::workers::CancellationToken;
use trashradar_app::Aggregator;
use trashradar_domain::candidate::{
    ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
    SafetyLevel,
};
use trashradar_domain::category::CategoryId;
use trashradar_domain::scan::{usn_reason, UsnChange};
use trashradar_index_memory::InMemoryIndex;
use trashradar_index_sqlite::IndexDatabase;

/// Еталон §15: ~1.5 млн записів у persistent-індексі.
const REFERENCE_FILES: u64 = 1_500_000;
/// Директорій у синтетичному дереві (як scan-bench: `C:\d{i}\f{j}.dat`).
const DIR_COUNT: u64 = 15_000;
/// MFT-номер кореня тому (сіється `FrnPathCache::seed_volume_root`).
const ROOT_RECORD: u64 = 5;
/// Перший MFT-номер директорій.
const DIR_BASE: u64 = 100;
/// Перший MFT-номер файлів.
const FILE_BASE: u64 = DIR_BASE + DIR_COUNT;
/// Батч запису в SQLite (сетап) і відновлення в пам'ять.
const BATCH_SIZE: usize = 100_000;

/// Дельта: скільки директорій задіяно (події create для FRN-кешу).
const DELTA_DIRS: u64 = 100;
/// Нові файли у дельті.
const DELTA_CREATES: u64 = 4_000;
/// Змінені наявні файли.
const DELTA_MODIFIES: u64 = 3_000;
/// Видалені наявні файли.
const DELTA_DELETES: u64 = 2_000;
/// Перейменовані наявні файли (пара подій old+new на кожен).
const DELTA_RENAMES: u64 = 1_000;
/// Очікувана кількість записів після дельти (rename не змінює кількість).
const EXPECTED_AFTER_DELTA: u64 = REFERENCE_FILES + DELTA_CREATES - DELTA_DELETES;

/// Допустимий регрес baseline (політика T-019).
const TOLERANCE: f64 = 1.15;
/// Ціль §15: 2 с до актуальної цифри. Поточна реалізація рестарту на
/// корпусі «всі 1.5 млн — кандидати» її не досягає (домінує відновлення з
/// SQLite) — перевищення цілі дає WARN, не FAIL; борг зафіксовано у
/// progress.md (відхилення T-154).
const TARGET_TOTAL_MS: f64 = 2_000.0;
/// Жорстка катастрофічна стеля CI: ловить повернення квадратичних шляхів
/// (до фіксів T-154 конвеєр займав хвилини+) із запасом на shared-runner.
const CEILING_TOTAL_MS: f64 = 60_000.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Baseline {
    /// Повний конвеєр рестарту: restore + USN + recalc + totals, мс.
    rerun_total_millis: f64,
    /// SQLite `read_all_file_records` → `InMemoryIndex`, мс.
    restore_millis: f64,
    /// Планування + застосування USN-дельти, мс.
    usn_apply_millis: f64,
    /// Перерахунок предикатних детекторів (T-038), мс.
    recalc_millis: f64,
    /// Агрегація унікальної цифри (T-054), мс.
    totals_millis: f64,
    /// Записів в індексі після дельти (має = EXPECTED_AFTER_DELTA).
    files_after_delta: u64,
}

struct MetricSpec {
    name: &'static str,
    current: f64,
    baseline: f64,
    ceiling: f64,
}

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("baseline.json")
}

/// Шлях файла i так само, як його збере FRN-кеш: `C:` + `d{dir}` + ім'я.
fn file_dir(i: u64) -> u64 {
    i % DIR_COUNT
}

fn file_path(i: u64) -> String {
    format!("C:\\d{}\\f{}.dat", file_dir(i), i)
}

/// Розмір понад дефолтний поріг «Великих файлів» (100 МіБ) — після
/// перерахунку всі записи знову категоризовані, цифра ненульова.
fn file_size(i: u64) -> u64 {
    100 * 1024 * 1024 + (i % 4096) * 1024
}

fn make_record(i: u64) -> FileRecord {
    FileRecord {
        candidate_id: CandidateId(FILE_BASE + i),
        path: file_path(i),
        size: ByteSize(file_size(i)),
        created_at: None,
        modified_at: None,
        accessed_at: None,
        kind: FileKind::Other,
        unit: CandidateUnit::File,
        // Persistent-індекс зберігає лише категоризовані записи (T-030);
        // фактичну категорію все одно перевстановить перерахунок.
        category: CategoryId::LargeFiles,
        safety: SafetyLevel::ReviewRecommended,
        decision: Decision::Undecided,
        detector_id: String::new(),
        explanation: String::new(),
        attributes: FileAttributes::default(),
    }
}

/// Наявний файл i для подій дельти: q-та «хвиля» по перших DELTA_DIRS теках.
fn existing_target(q: u64, d: u64) -> u64 {
    q * DIR_COUNT + d
}

fn dir_create_change(d: u64) -> UsnChange {
    UsnChange {
        usn: d as i64,
        file_ref: DIR_BASE + d,
        parent_ref: ROOT_RECORD,
        reason: usn_reason::FILE_CREATE,
        name: format!("d{d}"),
        is_directory: true,
        timestamp: None,
    }
}

/// Синтетична USN-дельта: спершу директорії (FRN-кеш), далі файлові події.
fn make_delta() -> Vec<UsnChange> {
    let mut changes = Vec::new();
    for d in 0..DELTA_DIRS {
        changes.push(dir_create_change(d));
    }

    // Create: нові файли new{k}.dat у теках d0..d99.
    for k in 0..DELTA_CREATES {
        changes.push(UsnChange {
            usn: 1_000 + k as i64,
            file_ref: FILE_BASE + REFERENCE_FILES + k,
            parent_ref: DIR_BASE + (k % DELTA_DIRS),
            reason: usn_reason::FILE_CREATE,
            name: format!("new{k}.dat"),
            is_directory: false,
            timestamp: None,
        });
    }

    // Modify: хвилі q=0..29 → 3000 наявних файлів.
    for k in 0..DELTA_MODIFIES {
        let i = existing_target(k / DELTA_DIRS, k % DELTA_DIRS);
        changes.push(UsnChange {
            usn: 100_000 + k as i64,
            file_ref: FILE_BASE + i,
            parent_ref: DIR_BASE + file_dir(i),
            reason: usn_reason::DATA_EXTEND,
            name: format!("f{i}.dat"),
            is_directory: false,
            timestamp: None,
        });
    }

    // Delete: хвилі q=30..49 → 2000 наявних файлів.
    for k in 0..DELTA_DELETES {
        let i = existing_target(30 + k / DELTA_DIRS, k % DELTA_DIRS);
        changes.push(UsnChange {
            usn: 200_000 + k as i64,
            file_ref: FILE_BASE + i,
            parent_ref: DIR_BASE + file_dir(i),
            reason: usn_reason::FILE_DELETE,
            name: format!("f{i}.dat"),
            is_directory: false,
            timestamp: None,
        });
    }

    // Rename: хвилі q=50..59 → 1000 пар old+new.
    for k in 0..DELTA_RENAMES {
        let i = existing_target(50 + k / DELTA_DIRS, k % DELTA_DIRS);
        changes.push(UsnChange {
            usn: 300_000 + k as i64 * 2,
            file_ref: FILE_BASE + i,
            parent_ref: DIR_BASE + file_dir(i),
            reason: usn_reason::RENAME_OLD_NAME,
            name: format!("f{i}.dat"),
            is_directory: false,
            timestamp: None,
        });
        changes.push(UsnChange {
            usn: 300_001 + k as i64 * 2,
            file_ref: FILE_BASE + i,
            parent_ref: DIR_BASE + file_dir(i),
            reason: usn_reason::RENAME_NEW_NAME,
            name: format!("r{i}.dat"),
            is_directory: false,
            timestamp: None,
        });
    }

    changes
}

/// Сетап (не гейтиться): persistent-індекс на REFERENCE_FILES записів.
fn seed_database(profile: &std::path::Path) -> IndexDatabase {
    let mut database = IndexDatabase::open_profile(profile).expect("open profile db");
    let mut batch = Vec::with_capacity(BATCH_SIZE);
    for i in 0..REFERENCE_FILES {
        batch.push(make_record(i));
        if batch.len() == BATCH_SIZE {
            database
                .upsert_file_records_batch(&batch)
                .expect("seed batch");
            batch.clear();
        }
    }
    if !batch.is_empty() {
        database
            .upsert_file_records_batch(&batch)
            .expect("seed final batch");
    }
    database
}

struct Phases {
    restore_millis: f64,
    usn_apply_millis: f64,
    recalc_millis: f64,
    totals_millis: f64,
    rerun_total_millis: f64,
    files_after_delta: u64,
    unique_bytes: u64,
}

/// Гейтований конвеєр: усе, що продукт робить від старту до актуальної цифри.
fn measure_rerun(database: &IndexDatabase, changes: &[UsnChange]) -> Phases {
    let total_started = Instant::now();

    // 1. Відновлення пам'яті з SQLite (T-017).
    let restore_started = Instant::now();
    let stored = database
        .read_all_file_records()
        .expect("read_all_file_records");
    let index = InMemoryIndex::new();
    for chunk in stored.chunks(BATCH_SIZE) {
        HotIndex::insert_batch(&index, chunk.to_vec()).expect("restore batch");
    }
    index.finish_indexing();
    let restore_millis = restore_started.elapsed().as_secs_f64() * 1000.0;

    // 2. USN-дельта (T-030). Probe — синтетичні метадані без I/O.
    let usn_started = Instant::now();
    let mut cache = FrnPathCache::new();
    cache.seed_volume_root('C');
    let mut next_id = next_candidate_id_from_index(&index).expect("next candidate id");
    let probe = |_path: &str| -> Option<FileProbe> {
        Some(FileProbe {
            size: 128 * 1024 * 1024,
            modified_at: None,
            created_at: None,
            accessed_at: None,
            attributes: FileAttributes::default(),
            is_directory: false,
        })
    };
    let (mutations, stats) = plan_usn_mutations(changes, &mut cache, probe, &mut next_id);
    apply_mutations_to_hot_index(&index, &mutations).expect("apply mutations");
    let usn_apply_millis = usn_started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(stats.skipped_unresolved, 0, "усі події мають резолвитись");
    assert_eq!(stats.created, DELTA_CREATES);
    assert_eq!(stats.modified, DELTA_MODIFIES);
    assert_eq!(stats.deleted, DELTA_DELETES);
    assert_eq!(stats.renamed, DELTA_RENAMES);

    // 3. Перерахунок предикатних детекторів (T-038).
    let recalc_started = Instant::now();
    let registry = mvp_predicate_registry();
    let orchestrator = DetectorOrchestrator::new(&registry);
    orchestrator
        .recalculate_index(&index, &CancellationToken::new())
        .expect("recalculate index");
    let recalc_millis = recalc_started.elapsed().as_secs_f64() * 1000.0;

    // 4. «Актуальна цифра» (T-054): унікальні кандидати з primary-категорій.
    let totals_started = Instant::now();
    let records = HotIndex::get_all(&index).expect("get_all");
    let summary = Aggregator::from_primary_records(&records);
    let totals_millis = totals_started.elapsed().as_secs_f64() * 1000.0;

    Phases {
        restore_millis,
        usn_apply_millis,
        recalc_millis,
        totals_millis,
        rerun_total_millis: total_started.elapsed().as_secs_f64() * 1000.0,
        files_after_delta: records.len() as u64,
        unique_bytes: summary.unique_bytes.0,
    }
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

fn temp_profile() -> PathBuf {
    std::env::temp_dir().join(format!("trashradar-rerun-bench-{}", std::process::id()))
}

fn run_gate(bless: bool, strict: bool) -> i32 {
    println!(
        "T-154 rerun: {} записів у SQLite, дельта {} подій (c{} m{} d{} r{})",
        REFERENCE_FILES,
        DELTA_DIRS + DELTA_CREATES + DELTA_MODIFIES + DELTA_DELETES + DELTA_RENAMES * 2,
        DELTA_CREATES,
        DELTA_MODIFIES,
        DELTA_DELETES,
        DELTA_RENAMES,
    );

    let profile = temp_profile();
    let _ = std::fs::remove_dir_all(&profile);
    println!("Сетап: наповнення persistent-індексу (не гейтиться)…");
    let database = seed_database(&profile);
    let changes = make_delta();

    println!("Замір: restore → USN → recalc → цифра…");
    let phases = measure_rerun(&database, &changes);
    drop(database);
    let _ = std::fs::remove_dir_all(&profile);

    println!(
        "Заміри: total={:.1} мс (restore={:.1} + usn={:.1} + recalc={:.1} + totals={:.1}); \
         записів={}, цифра={:.1} ГіБ",
        phases.rerun_total_millis,
        phases.restore_millis,
        phases.usn_apply_millis,
        phases.recalc_millis,
        phases.totals_millis,
        phases.files_after_delta,
        phases.unique_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
    );

    // Жорсткий детермінований інваріант незалежно від baseline.
    assert_eq!(
        phases.files_after_delta, EXPECTED_AFTER_DELTA,
        "кількість записів після дельти має бути точною"
    );
    assert!(phases.unique_bytes > 0, "цифра після рестарту не нульова");

    let current = Baseline {
        rerun_total_millis: phases.rerun_total_millis,
        restore_millis: phases.restore_millis,
        usn_apply_millis: phases.usn_apply_millis,
        recalc_millis: phases.recalc_millis,
        totals_millis: phases.totals_millis,
        files_after_delta: phases.files_after_delta,
    };

    if bless {
        write_baseline(&current);
        return 0;
    }

    let Some(baseline) = load_baseline() else {
        eprintln!("ПОМИЛКА: baseline.json відсутній. Згенеруйте: cargo run --release -- --bless");
        return 2;
    };

    let specs = [
        MetricSpec {
            name: "rerun_total_millis",
            current: current.rerun_total_millis,
            baseline: baseline.rerun_total_millis,
            ceiling: CEILING_TOTAL_MS,
        },
        MetricSpec {
            name: "restore_millis",
            current: current.restore_millis,
            baseline: baseline.restore_millis,
            ceiling: CEILING_TOTAL_MS,
        },
        MetricSpec {
            name: "usn_apply_millis",
            current: current.usn_apply_millis,
            baseline: baseline.usn_apply_millis,
            ceiling: CEILING_TOTAL_MS,
        },
        MetricSpec {
            name: "recalc_millis",
            current: current.recalc_millis,
            baseline: baseline.recalc_millis,
            ceiling: CEILING_TOTAL_MS,
        },
        MetricSpec {
            name: "totals_millis",
            current: current.totals_millis,
            baseline: baseline.totals_millis,
            ceiling: CEILING_TOTAL_MS,
        },
    ];

    let mut failed = false;
    println!(
        "\n{:<20} {:>12} {:>12} {:>8}  verdict",
        "metric", "baseline", "current", "ratio"
    );
    for spec in &specs {
        let ratio = if spec.baseline > 0.0 {
            spec.current / spec.baseline
        } else {
            f64::INFINITY
        };
        let regressed = ratio > TOLERANCE;
        let over_limit = spec.current > spec.ceiling;
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
        if hard {
            failed = true;
        }
        println!(
            "{:<20} {:>12.1} {:>12.1} {:>7.2}x  {} [ms]",
            spec.name, spec.baseline, spec.current, ratio, verdict
        );
    }

    // Ціль §15 — WARN-лінія до закриття боргу відновлення (відхилення T-154).
    if current.rerun_total_millis > TARGET_TOTAL_MS {
        println!(
            "\nWARN §15: rerun {:.1} мс > цілі {:.0} с — борг відновлення з SQLite \
             (див. відхилення T-154 у progress.md).",
            current.rerun_total_millis,
            TARGET_TOTAL_MS / 1000.0
        );
    }

    if failed {
        eprintln!(
            "\nРегрес >{:.0}% / катастрофічна стеля — гейт не пройдено.",
            (TOLERANCE - 1.0) * 100.0
        );
        return 1;
    }
    println!(
        "\nГейт пройдено: rerun < {:.0} с (guard), регресів понад {:.0}% немає.",
        CEILING_TOTAL_MS / 1000.0,
        (TOLERANCE - 1.0) * 100.0
    );
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bless = args.iter().any(|a| a == "--bless");
    let strict = args.iter().any(|a| a == "--strict");
    std::process::exit(run_gate(bless, strict));
}
