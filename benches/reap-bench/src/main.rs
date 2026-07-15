//! Бенчмарк-гейт транзакційного reap (T-154).
//!
//! Ціль architecture.md §15: **reap 1 000 файлів на одному томі < 2 с**.
//! Міряється продуктовий шлях T-079 без спрощень: реальна FS (temp-каталог
//! = той самий том), справжній `NativeQuarantineFs` (guard + identity check +
//! атомарний same-volume move) і справжній SQLite-manifest (durable
//! in_flight → move → confirmation + append-only аудит на кожен файл).
//!
//! Гейти за політикою T-019/T-035: тайминг на shared-runner — WARN на
//! регрес >15% + жорстка стеля = ціль §15 (2 с; досяжна після фазового
//! батчування manifest-транзакцій T-154 — SQLite-фази ~50 мс, домінує
//! move-фаза); `--strict` робить регрес помилкою (локально на машині
//! baseline); кількість переміщених файлів і стан manifest — жорсткі
//! інваріанти.
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
/// Ціль §15: 2 с на батч — жорстка стеля. Після фазового батчування
/// manifest-транзакцій (T-154: усі in_flight одним tx → move'и →
/// підтвердження + аудит одним tx) SQLite-фази займають ~50 мс, домінує
/// move-фаза (~1.1–1.8 с: WinAPI move + guard + identity під AV-фільтром).
const CEILING_BATCH_MS: f64 = 2_000.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Baseline {
    /// Повний `reap_batch` (журнал + move + підтвердження + аудит), мс.
    reap_batch_millis: f64,
    /// Похідна пропускна здатність, файлів/с.
    reap_files_per_sec: f64,
    /// Скільки файлів переміщено (має = REAP_FILES).
    reaped_count: u64,
    /// Фазова розбивка (діагностика, не гейтиться окремо): durable-запис
    /// усіх in_flight одним tx (T-154).
    #[serde(default)]
    journal_millis: f64,
    /// Послідовні атомарні move (WinAPI, домінанта батчу).
    #[serde(default)]
    moves_millis: f64,
    /// Підтвердження + аудит одним tx (T-154).
    #[serde(default)]
    confirm_millis: f64,
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

/// Таймінгові обгортки портів: фазова розбивка reap_batch без зміни
/// продуктового шляху. Обгортка manifest ОБОВ'ЯЗКОВО делегує батч-методи —
/// інакше замір міряв би дефолтні per-item цикли, а не SQLite-транзакції.
#[cfg(windows)]
mod probes {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    use trashradar_app::ports::{QuarantineFs, QuarantineManifest, RecoveryLocation, RestoreMove};
    use trashradar_domain::error::CoreError;
    use trashradar_domain::quarantine::{
        DestructiveAuditEvent, DestructiveAuditRecord, FileIdentity, QuarantineEntry,
        QuarantineEntryId, QuarantineStatus,
    };

    pub struct TimingFs<F: QuarantineFs> {
        inner: F,
        move_nanos: AtomicU64,
    }

    impl<F: QuarantineFs> TimingFs<F> {
        pub fn new(inner: F) -> Self {
            Self {
                inner,
                move_nanos: AtomicU64::new(0),
            }
        }

        pub fn take_move_millis(&self) -> f64 {
            self.move_nanos.swap(0, Ordering::Relaxed) as f64 / 1_000_000.0
        }
    }

    impl<F: QuarantineFs> QuarantineFs for TimingFs<F> {
        fn move_into_quarantine(
            &self,
            source: &str,
            destination: &str,
            expected: FileIdentity,
            roots: &[String],
        ) -> Result<(), CoreError> {
            let started = Instant::now();
            let result = self
                .inner
                .move_into_quarantine(source, destination, expected, roots);
            self.move_nanos
                .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
            result
        }

        fn restore_from_quarantine(
            &self,
            surrogate_path: &str,
            destination_path: &str,
        ) -> Result<RestoreMove, CoreError> {
            self.inner
                .restore_from_quarantine(surrogate_path, destination_path)
        }

        fn purge_from_quarantine(&self, surrogate_path: &str) -> Result<(), CoreError> {
            self.inner.purge_from_quarantine(surrogate_path)
        }

        fn recovery_location(
            &self,
            source_path: &str,
            surrogate_path: &str,
        ) -> Result<RecoveryLocation, CoreError> {
            self.inner.recovery_location(source_path, surrogate_path)
        }
    }

    pub struct TimingManifest<M: QuarantineManifest> {
        inner: M,
        journal_nanos: AtomicU64,
        confirm_nanos: AtomicU64,
    }

    impl<M: QuarantineManifest> TimingManifest<M> {
        pub fn new(inner: M) -> Self {
            Self {
                inner,
                journal_nanos: AtomicU64::new(0),
                confirm_nanos: AtomicU64::new(0),
            }
        }

        pub fn inner(&self) -> &M {
            &self.inner
        }

        pub fn take_millis(&self) -> (f64, f64) {
            (
                self.journal_nanos.swap(0, Ordering::Relaxed) as f64 / 1_000_000.0,
                self.confirm_nanos.swap(0, Ordering::Relaxed) as f64 / 1_000_000.0,
            )
        }
    }

    impl<M: QuarantineManifest> QuarantineManifest for TimingManifest<M> {
        fn insert_entry(&self, entry: &QuarantineEntry) -> Result<(), CoreError> {
            self.inner.insert_entry(entry)
        }

        fn insert_entries(&self, entries: &[QuarantineEntry]) -> Result<(), CoreError> {
            let started = Instant::now();
            let result = self.inner.insert_entries(entries);
            self.journal_nanos
                .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
            result
        }

        fn get_entry(&self, id: QuarantineEntryId) -> Result<Option<QuarantineEntry>, CoreError> {
            self.inner.get_entry(id)
        }

        fn list_entries(&self) -> Result<Vec<QuarantineEntry>, CoreError> {
            self.inner.list_entries()
        }

        fn remove_entry(&self, id: QuarantineEntryId) -> Result<(), CoreError> {
            self.inner.remove_entry(id)
        }

        fn append_audit(&self, event: &DestructiveAuditEvent) -> Result<(), CoreError> {
            self.inner.append_audit(event)
        }

        fn list_audit(&self) -> Result<Vec<DestructiveAuditRecord>, CoreError> {
            self.inner.list_audit()
        }

        fn confirm_batch_with_audit(
            &self,
            confirmations: &[(QuarantineEntryId, QuarantineStatus, DestructiveAuditEvent)],
        ) -> Result<(), CoreError> {
            let started = Instant::now();
            let result = self.inner.confirm_batch_with_audit(confirmations);
            self.confirm_nanos
                .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
            result
        }

        fn update_status(
            &self,
            id: QuarantineEntryId,
            status: QuarantineStatus,
        ) -> Result<(), CoreError> {
            self.inner.update_status(id, status)
        }
    }
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

    let filesystem = probes::TimingFs::new(NativeQuarantineFs);
    let manifest = probes::TimingManifest::new(database);
    let reaper = TransactionalReaper::new(&filesystem, &manifest, &[]);

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
    // Скинути акумульовані warmup-фази перед заміром.
    filesystem.take_move_millis();
    manifest.take_millis();

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
    let moves_millis = filesystem.take_move_millis();
    let (journal_millis, confirm_millis) = manifest.take_millis();
    println!(
        "  [фази] журнал {journal_millis:.1} мс | move {moves_millis:.1} мс | підтвердження {confirm_millis:.1} мс"
    );

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
    let quarantined = manifest
        .inner()
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

    drop(manifest);
    let _ = std::fs::remove_dir_all(&root);

    Baseline {
        reap_batch_millis,
        reap_files_per_sec: if reap_batch_millis > 0.0 {
            REAP_FILES as f64 / (reap_batch_millis / 1000.0)
        } else {
            f64::INFINITY
        },
        reaped_count: REAP_FILES,
        journal_millis,
        moves_millis,
        confirm_millis,
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
    for (name, base, cur) in [
        (
            "  journal_millis",
            baseline.journal_millis,
            current.journal_millis,
        ),
        (
            "  moves_millis",
            baseline.moves_millis,
            current.moves_millis,
        ),
        (
            "  confirm_millis",
            baseline.confirm_millis,
            current.confirm_millis,
        ),
    ] {
        println!("{name:<20} {base:>12.1} {cur:>12.1}        -  info [ms]");
    }

    if hard {
        eprintln!(
            "\nРегрес >{:.0}% / стеля §15 — гейт не пройдено.",
            (TOLERANCE - 1.0) * 100.0
        );
        return 1;
    }
    println!(
        "\nГейт пройдено: reap {} файлів < {:.0} с (стеля §15).",
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
