//! Бенчмарк-гейт скану «до наповнення індексу» (T-035).
//!
//! Ціль architecture.md §15: NTFS, ~1.5 млн файлів, теплий диск →
//! **< 10 с** до наповнення індексу (перша цифра / категорії — окремо, T-054+).
//!
//! Два режими:
//!
//! 1. **Синтетичний (CI, за замовчуванням)** — CPU-конвеєр T-022/T-023/T-024:
//!    `PathResolver` + `Batcher` → `InMemoryIndex` на 1 500 000 файлів без
//!    доступу до диску. Фіксує baseline; регрес >15% (пам'ять — жорстко,
//!    тайминг — WARN + абсолютна стеля 10 с). Прапорець `--strict` — жорсткий
//!    регрес усіх метрик (локально на машині baseline).
//!
//! 2. **Живий том (`--volume X`)** — повний MFT → індекс (`scan_volume_to_index`).
//!    Потребує elevation. Гріє диск першим проходом, міряє другим (теплий).
//!    Проєктує час на 1.5 млн і валить процес, якщо проєкція > 10 с.
//!
//! Використання:
//!   cargo run --release
//!   cargo run --release -- --strict
//!   cargo run --release -- --bless
//!   cargo run --release -- --volume C

use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use trashradar_app::ports::HotIndex;
use trashradar_domain::candidate::{ByteSize, FileAttributes};
use trashradar_domain::scan::ScanEntry;
use trashradar_index_memory::InMemoryIndex;
use trashradar_scan_mft::paths::PathResolver;
use trashradar_scan_mft::pipeline::{Batcher, IndexingStats};

/// Еталон §15: ~1.5 млн файлів.
const REFERENCE_FILES: u64 = 1_500_000;
/// Директорій у синтетичному дереві (глибина 2: root → dir → file).
const DIR_COUNT: u64 = 15_000;
/// Розмір батча як у продакшн-пайплайні (T-024: ~10k).
const BATCH_SIZE: usize = 10_000;
/// Корінь NTFS MFT (T-022).
const ROOT_RECORD: u64 = 5;
/// Перший номер запису для директорій (після кореневого 5).
const DIR_BASE: u64 = 100;
/// Перший номер запису для файлів.
const FILE_BASE: u64 = DIR_BASE + DIR_COUNT;
/// Допустимий регрес baseline (як T-019).
const TOLERANCE: f64 = 1.15;
/// Абсолютна стеля §15: 10 с до наповнення індексу.
const CEILING_FILL_MS: f64 = 10_000.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Baseline {
    /// Час PathResolver+Batcher→InMemoryIndex на REFERENCE_FILES, мс.
    index_fill_millis: f64,
    /// Пропускна здатність (файлів/с) на синтетиці.
    files_per_sec: f64,
    /// Оцінка heap після `finish_indexing`, байти.
    index_memory_bytes: u64,
    /// Скільки файлів у прогоні (має = REFERENCE_FILES).
    files_indexed: u64,
}

struct MetricSpec {
    name: &'static str,
    current: f64,
    baseline: f64,
    /// Вищий = гірше (час, пам'ять); для throughput інвертуємо ratio.
    higher_is_worse: bool,
    deterministic: bool,
    ceiling: f64,
    unit: &'static str,
}

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("baseline.json")
}

fn make_dir_entry(i: u64) -> ScanEntry {
    // dir_i під коренем.
    ScanEntry {
        file_ref: DIR_BASE + i,
        parent_ref: ROOT_RECORD,
        name: format!("d{i}"),
        size: ByteSize(0),
        created_at: None,
        modified_at: None,
        accessed_at: None,
        is_directory: true,
        attributes: FileAttributes::default(),
    }
}

fn make_file_entry(i: u64) -> ScanEntry {
    let dir_i = i % DIR_COUNT;
    ScanEntry {
        file_ref: FILE_BASE + i,
        parent_ref: DIR_BASE + dir_i,
        name: format!("f{i}.dat"),
        size: ByteSize((i.wrapping_mul(17)) % (1 << 30)),
        created_at: None,
        modified_at: None,
        accessed_at: None,
        is_directory: false,
        attributes: FileAttributes::default(),
    }
}

/// Синтетичний прогін: дерево → шляхи → батчі → hot index.
fn measure_synthetic() -> Baseline {
    // Директорії для PathResolver (не входять у index fill time — підготовка корпусу).
    let dirs: Vec<ScanEntry> = (0..DIR_COUNT).map(make_dir_entry).collect();
    let resolver = PathResolver::from_entries('C', &dirs);

    let index = InMemoryIndex::new();
    let started = Instant::now();
    {
        let mut batcher = Batcher::new(&resolver, BATCH_SIZE, |batch| {
            HotIndex::insert_batch(&index, batch)
        });
        for i in 0..REFERENCE_FILES {
            batcher.push(make_file_entry(i));
        }
        let stats: IndexingStats = batcher.finish().expect("batcher finish");
        assert_eq!(
            stats.files_indexed, REFERENCE_FILES,
            "усі файли мають потрапити в індекс (skipped={})",
            stats.skipped_no_path
        );
        assert_eq!(stats.skipped_no_path, 0, "сиріт у синтетиці бути не може");
    }
    // Inherent finish_indexing() → (); trait-версія теж no-op на помилках.
    index.finish_indexing();
    let fill = started.elapsed();
    let index_fill_millis = fill.as_secs_f64() * 1000.0;
    let files_per_sec = if fill.as_secs_f64() > 0.0 {
        REFERENCE_FILES as f64 / fill.as_secs_f64()
    } else {
        f64::INFINITY
    };
    let files_indexed = HotIndex::len(&index).expect("len") as u64;
    assert_eq!(files_indexed, REFERENCE_FILES);

    Baseline {
        index_fill_millis,
        files_per_sec,
        index_memory_bytes: index.memory_usage() as u64,
        files_indexed,
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

fn run_synthetic_gate(bless: bool, strict: bool) -> i32 {
    println!(
        "T-035 synthetic: {} файлів, {} директорій, batch={}",
        REFERENCE_FILES, DIR_COUNT, BATCH_SIZE
    );
    println!("(конвеєр PathResolver+Batcher→InMemoryIndex; без I/O диску)");

    let current = measure_synthetic();
    println!(
        "Заміри: fill={:.1} мс ({:.0} files/s), memory={:.2} МБ, indexed={}",
        current.index_fill_millis,
        current.files_per_sec,
        current.index_memory_bytes as f64 / 1024.0 / 1024.0,
        current.files_indexed,
    );

    if bless {
        write_baseline(&current);
        return 0;
    }

    let Some(baseline) = load_baseline() else {
        eprintln!("ПОМИЛКА: baseline.json відсутній. Згенеруйте: cargo run --release -- --bless");
        return 2;
    };

    // files_per_sec: вищий = краще → ratio = baseline/current (>1.15 = регрес).
    let specs = [
        MetricSpec {
            name: "index_fill_millis",
            current: current.index_fill_millis,
            baseline: baseline.index_fill_millis,
            higher_is_worse: true,
            deterministic: false,
            ceiling: CEILING_FILL_MS,
            unit: "ms",
        },
        MetricSpec {
            name: "files_per_sec",
            current: current.files_per_sec,
            baseline: baseline.files_per_sec,
            higher_is_worse: false,
            deterministic: false,
            // Нижня «стеля» як absolute floor: катастрофічне падіння throughput.
            // Перевіряємо окремо: current < floor → fail. Тут ceiling = мін. прийнятне.
            ceiling: REFERENCE_FILES as f64 / (CEILING_FILL_MS / 1000.0), // 150k/s
            unit: "f/s",
        },
        MetricSpec {
            name: "index_memory_bytes",
            current: current.index_memory_bytes as f64,
            baseline: baseline.index_memory_bytes as f64,
            higher_is_worse: true,
            deterministic: true,
            ceiling: 300.0 * 1024.0 * 1024.0, // §15 RAM budget
            unit: "B",
        },
    ];

    let mut failed = false;
    println!(
        "\n{:<22} {:>12} {:>12} {:>8}  verdict",
        "metric", "baseline", "current", "ratio"
    );
    for spec in &specs {
        let (ratio, regressed, over_limit) = if spec.higher_is_worse {
            let ratio = if spec.baseline > 0.0 {
                spec.current / spec.baseline
            } else {
                f64::INFINITY
            };
            (ratio, ratio > TOLERANCE, spec.current > spec.ceiling)
        } else {
            // throughput: регрес = current << baseline; over = current < floor(ceiling)
            let ratio = if spec.current > 0.0 {
                spec.baseline / spec.current
            } else {
                f64::INFINITY
            };
            (ratio, ratio > TOLERANCE, spec.current < spec.ceiling)
        };

        let hard = over_limit || (regressed && (spec.deterministic || strict));
        let verdict = if over_limit {
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
            "{:<22} {:>12.1} {:>12.1} {:>7.2}x  {} [{}]",
            spec.name, spec.baseline, spec.current, ratio, verdict, spec.unit
        );
    }

    // Жорсткий інваріант DoD: fill завжди < 10 с (навіть без baseline).
    if current.index_fill_millis > CEILING_FILL_MS {
        eprintln!(
            "\nDoD §15: index fill {:.1} мс > {} мс — гейт не пройдено.",
            current.index_fill_millis, CEILING_FILL_MS as u64
        );
        failed = true;
    }

    if failed {
        eprintln!(
            "\nРегрес >{:.0}% / стеля §15 — гейт не пройдено.",
            (TOLERANCE - 1.0) * 100.0
        );
        return 1;
    }
    println!(
        "\nГейт пройдено: fill < {:.0} с (§15), регресів понад {:.0}% немає.",
        CEILING_FILL_MS / 1000.0,
        (TOLERANCE - 1.0) * 100.0
    );
    0
}

#[cfg(windows)]
fn run_live_volume(drive: char, strict: bool) -> i32 {
    use trashradar_platform_win::{is_ntfs_volume, is_process_elevated};
    use trashradar_scan_mft::pipeline::scan_volume_to_index;

    println!(
        "T-035 live volume: {}:  (теплий прохід = другий; ціль §15 < {:.0} с @ {} files)",
        drive.to_ascii_uppercase(),
        CEILING_FILL_MS / 1000.0,
        REFERENCE_FILES
    );

    if !is_process_elevated() {
        eprintln!(
            "ПОМИЛКА: live-режим потребує адмін-прав (MFT). \
             Запустіть elevated-консоль або спочатку app.request_elevation."
        );
        return 3;
    }
    match is_ntfs_volume(drive) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("ПОМИЛКА: том {drive}: не NTFS — MFT-шлях недоступний.");
            return 3;
        }
        Err(e) => {
            eprintln!("ПОМИЛКА: не вдалося визначити FS: {e}");
            return 3;
        }
    }

    let run_once = |label: &str| -> Result<(IndexingStats, f64), String> {
        let index = InMemoryIndex::new();
        let started = Instant::now();
        let stats = scan_volume_to_index(drive, BATCH_SIZE, |batch| {
            HotIndex::insert_batch(&index, batch)
        })
        .map_err(|e| format!("{e}"))?;
        index.finish_indexing();
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        println!(
            "  [{label}] indexed={} batches={} corrupt={} skipped={} in {:.1} мс ({:.0} f/s); index_len={}",
            stats.files_indexed,
            stats.batches,
            stats.corrupt,
            stats.skipped_no_path,
            ms,
            if ms > 0.0 {
                stats.files_indexed as f64 / (ms / 1000.0)
            } else {
                0.0
            },
            HotIndex::len(&index).unwrap_or(0),
        );
        Ok((stats, ms))
    };

    // Прогрів (холодний/напівтеплий) — не гейтиться.
    println!("Прогрів (discard)…");
    if let Err(e) = run_once("warmup") {
        eprintln!("ПОМИЛКА warmup: {e}");
        return 3;
    }

    println!("Замір (теплий)…");
    let (stats, ms) = match run_once("warm") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ПОМИЛКА warm: {e}");
            return 3;
        }
    };

    if stats.files_indexed == 0 {
        eprintln!("ПОМИЛКА: 0 файлів на томі — нічого міряти.");
        return 3;
    }

    let projected_ms = (ms / stats.files_indexed as f64) * REFERENCE_FILES as f64;
    let rate = stats.files_indexed as f64 / (ms / 1000.0);
    println!(
        "\nТеплий: {:.1} мс на {} файлів → {:.0} f/s",
        ms, stats.files_indexed, rate
    );
    println!(
        "Проєкція на {} файлів: {:.1} мс ({:.2} с)  [стеля §15 = {:.0} с]",
        REFERENCE_FILES,
        projected_ms,
        projected_ms / 1000.0,
        CEILING_FILL_MS / 1000.0
    );

    let over = projected_ms > CEILING_FILL_MS;
    if over {
        eprintln!(
            "\nFAIL: проєкція {:.2} с > {:.0} с (§15). Теплий MFT→index не вкладається в ціль.",
            projected_ms / 1000.0,
            CEILING_FILL_MS / 1000.0
        );
        return 1;
    }
    if strict {
        println!("strict: live проєкція в межах §15.");
    }
    println!(
        "\nLive-гейт пройдено: проєкція < {:.0} с.",
        CEILING_FILL_MS / 1000.0
    );
    0
}

#[cfg(not(windows))]
fn run_live_volume(_drive: char, _strict: bool) -> i32 {
    eprintln!("Live volume доступний лише на Windows.");
    3
}

fn parse_volume_arg(args: &[String]) -> Option<char> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--volume" {
            let v = args.get(i + 1)?;
            let c = v.chars().next().filter(|c| c.is_ascii_alphabetic())?;
            return Some(c.to_ascii_uppercase());
        }
        if let Some(rest) = args[i].strip_prefix("--volume=") {
            let c = rest.chars().next().filter(|c| c.is_ascii_alphabetic())?;
            return Some(c.to_ascii_uppercase());
        }
        i += 1;
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bless = args.iter().any(|a| a == "--bless");
    let strict = args.iter().any(|a| a == "--strict");

    if let Some(drive) = parse_volume_arg(&args) {
        // Live-режим не пише synthetic baseline.
        if bless {
            eprintln!("--bless стосується лише synthetic-режиму (без --volume).");
            std::process::exit(2);
        }
        std::process::exit(run_live_volume(drive, strict));
    }

    std::process::exit(run_synthetic_gate(bless, strict));
}
