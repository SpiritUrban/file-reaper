//! Дочірній процес-«жертва» матриці T-156.
//!
//! Виконує **продуктовий** шлях reap або purge (`TransactionalReaper` /
//! `ManualPurger` над реальними `NativeQuarantineFs` і SQLite-manifest) і
//! гине у фазі, заданій аргументом `--phase`. Гине через `std::process::exit`:
//! без розкрутки стека, без деструкторів, без коректного закриття БД — усе,
//! що переживе таку смерть, пережило б і справжню аварію застосунку.
//!
//! Використання (запускають тести, не людина):
//!   crash-victim --root <каталог> --op reap|purge --phase <фаза>

use trashradar_e2e::{
    entry_id, surrogate_name, CrashPhase, CrashPlan, CrashingFs, CrashingManifest, Layout,
    Operation, CANDIDATE_FILES, CRASH_EXIT_CODE,
};

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let position = args.iter().position(|value| value == name)?;
    args.get(position + 1).cloned()
}

fn main() {
    let root = arg("--root").expect("--root <каталог сценарію>");
    let operation =
        Operation::parse(&arg("--op").expect("--op reap|purge")).expect("невідома --op");
    let phase =
        CrashPhase::parse(&arg("--phase").expect("--phase <фаза>")).expect("невідома --phase");

    let layout = Layout::new(root);
    let plan = CrashPlan::for_phase(phase);

    let code = match operation {
        Operation::Reap => run_reap(&layout, phase, plan),
        Operation::Purge => run_purge(&layout, phase, plan),
    };
    std::process::exit(code);
}

#[cfg(windows)]
fn run_reap(layout: &Layout, phase: CrashPhase, plan: CrashPlan) -> i32 {
    use trashradar_app::ports::QuarantineManifest;
    use trashradar_app::{ReapRequest, TransactionalReaper};
    use trashradar_domain::quarantine::{BatchId, QuarantineEntry, QuarantineStatus};
    use trashradar_index_sqlite::IndexDatabase;
    use trashradar_platform_win::read_file_identity;
    use trashradar_quarantine_fs::NativeQuarantineFs;

    let directory = NativeQuarantineFs
        .ensure_at_root(&layout.root)
        .expect("підготувати каталог карантину");
    let database = IndexDatabase::open_profile(layout.profile_dir()).expect("відкрити manifest");

    let mut requests = Vec::with_capacity(CANDIDATE_FILES as usize);
    for index in 0..CANDIDATE_FILES {
        let source = layout.original_path(index);
        let identity = read_file_identity(&source).expect("прочитати ідентичність файла");
        let surrogate = surrogate_name(index);
        let destination = directory.quarantine_root.join(&surrogate);
        requests.push(ReapRequest {
            entry: QuarantineEntry {
                id: entry_id(index),
                batch_id: None,
                original_path: source.to_string_lossy().into_owned(),
                surrogate_name: surrogate,
                size: identity.size,
                quarantined_at_unix: 1_750_000_000,
                expires_at_unix: 1_752_592_000,
                status: QuarantineStatus::InFlight,
            },
            destination_path: destination.to_string_lossy().into_owned(),
            expected_identity: identity,
        });
    }

    // Фаза «до журналу»: аварія ще до першого дотику до manifest.
    if phase == CrashPhase::ReapBeforeJournal {
        return CRASH_EXIT_CODE;
    }

    let filesystem = CrashingFs::new(NativeQuarantineFs, plan);
    let manifest = CrashingManifest::new(database, plan);
    let reaper = TransactionalReaper::new(&filesystem, &manifest, &[]);
    let outcomes = reaper
        .reap_batch(BatchId(1), requests)
        .expect("reap_batch без аварії має пройти");
    assert_eq!(outcomes.len() as u64, CANDIDATE_FILES);
    // Не аварія: журнал і диск лишились узгодженими.
    let entries = manifest.list_entries().expect("список журналу");
    assert_eq!(entries.len() as u64, CANDIDATE_FILES);
    0
}

#[cfg(windows)]
fn run_purge(layout: &Layout, phase: CrashPhase, plan: CrashPlan) -> i32 {
    use trashradar_app::{ManualPurgeSelection, ManualPurger};
    use trashradar_index_sqlite::IndexDatabase;
    use trashradar_quarantine_fs::NativeQuarantineFs;

    let database = IndexDatabase::open_profile(layout.profile_dir()).expect("відкрити manifest");

    if phase == CrashPhase::PurgeBeforeDelete {
        return CRASH_EXIT_CODE;
    }

    let filesystem = CrashingFs::new(NativeQuarantineFs, plan);
    let manifest = CrashingManifest::new(database, plan);
    let purger = ManualPurger::new(&filesystem, &manifest);
    let quarantine_root = layout.quarantine_root();
    let result = purger
        .purge(ManualPurgeSelection::All, |entry| {
            Ok(quarantine_root
                .join(&entry.surrogate_name)
                .to_string_lossy()
                .into_owned())
        })
        .expect("purge без аварії має пройти");
    // «Спорожнити все» знищує всі наявні quarantined-записи. Скільки їх —
    // залежить від сценарію (після докату перерваного purge частина вже
    // Purged), тож фіксованого числа не вимагаємо: важливо, що операція
    // дійшла до кінця без помилки.
    assert!(
        !result.purged.is_empty(),
        "purge мав знищити хоча б один запис"
    );
    0
}

#[cfg(not(windows))]
fn run_reap(_layout: &Layout, _phase: CrashPhase, _plan: CrashPlan) -> i32 {
    // MVP — Windows-only (product.md §6): NativeQuarantineFs і
    // read_file_identity — WinAPI.
    0
}

#[cfg(not(windows))]
fn run_purge(_layout: &Layout, _phase: CrashPhase, _plan: CrashPlan) -> i32 {
    0
}
