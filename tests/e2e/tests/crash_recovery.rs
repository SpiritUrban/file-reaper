//! T-156: матриця точок переривання reap/purge.
//!
//! **DoD: матриця точок переривання проходить без втрати даних і файлів-сиріт.**
//!
//! Кожен рядок матриці — це повний цикл: чистий «том» → корпус файлів →
//! **дочірній процес** виконує продуктовий reap/purge і гине у заданій фазі
//! (`std::process::exit` без деструкторів і без закриття SQLite) → батько
//! відкриває ту саму БД наново (це і є наступний старт застосунку) і виконує
//! `QuarantineRecovery::reconcile` (T-084) → інваріанти D4:
//!
//! - у кожного кандидата рівно одна копія — на місці або в карантині
//!   (виняток: журнал явно каже `Purged` — файл знищено на вимогу);
//! - жодного файла-сироти в карантині (файл без запису журналу);
//! - жодного фантомного запису (журнал обіцяє restore файла, якого немає);
//! - жодної незавершеної транзакції (`in_flight`) після відновлення.
//!
//! Windows-only: `NativeQuarantineFs`/`read_file_identity` — WinAPI, MVP
//! теж Windows (product.md §6).

#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;

use trashradar_app::ports::QuarantineManifest;
use trashradar_app::QuarantineRecovery;
use trashradar_domain::quarantine::{QuarantineEntry, QuarantineStatus};
use trashradar_e2e::{
    assert_no_in_flight, assert_no_loss_no_duplicates, assert_no_orphan_files,
    assert_no_phantom_entries, create_candidates, reset_root, CrashPhase, Layout, Operation,
    CANDIDATE_FILES, CRASH_EXIT_CODE, MID_BATCH_CALL,
};
use trashradar_index_sqlite::IndexDatabase;
use trashradar_quarantine_fs::NativeQuarantineFs;

/// Окремий «том» на рядок матриці: стан між сценаріями не переноситься.
fn scenario_root(name: &str) -> Layout {
    let root = std::env::temp_dir().join(format!("trashradar-e2e-{name}-{}", std::process::id()));
    reset_root(&root);
    std::fs::create_dir_all(&root).expect("створити корінь сценарію");
    let layout = Layout::new(root);
    create_candidates(&layout).expect("створити корпус кандидатів");
    layout
}

/// Запустити «жертву» і дочекатись її смерті/завершення.
fn run_victim(layout: &Layout, operation: Operation, phase: CrashPhase) -> i32 {
    let victim = PathBuf::from(env!("CARGO_BIN_EXE_crash-victim"));
    let status = Command::new(victim)
        .args([
            "--root",
            &layout.root.to_string_lossy(),
            "--op",
            operation.as_str(),
            "--phase",
            phase.as_str(),
        ])
        .status()
        .expect("запустити crash-victim");
    status.code().expect("код виходу crash-victim")
}

/// «Наступний старт застосунку»: свіже відкриття БД + звірка журналу з
/// реальністю (той самий виклик, що робить shell на старті, T-084).
fn restart_and_reconcile(layout: &Layout) -> Vec<QuarantineEntry> {
    let database = IndexDatabase::open_profile(layout.profile_dir()).expect("відкрити manifest");
    let quarantine_root = layout.quarantine_root();
    let recovery = QuarantineRecovery::new(&NativeQuarantineFs, &database);
    recovery
        .reconcile(|entry| {
            Ok(quarantine_root
                .join(&entry.surrogate_name)
                .to_string_lossy()
                .into_owned())
        })
        .expect("reconcile має розв'язати кожен in_flight-запис");
    database.list_entries().expect("список журналу")
}

/// Спільна перевірка всіх інваріантів безпеки після відновлення.
fn assert_safe_state(layout: &Layout, entries: &[QuarantineEntry], context: &str) {
    assert_no_loss_no_duplicates(layout, entries, context);
    assert_no_orphan_files(layout, entries, context);
    assert_no_phantom_entries(layout, entries, context);
    assert_no_in_flight(entries, context);
}

fn count(entries: &[QuarantineEntry], status: QuarantineStatus) -> usize {
    entries
        .iter()
        .filter(|entry| entry.status == status)
        .count()
}

/// Довести кандидатів до карантину без аварії — стартовий стан purge-рядків.
fn quarantine_everything(layout: &Layout) {
    let code = run_victim(layout, Operation::Reap, CrashPhase::None);
    assert_eq!(code, 0, "сетап: чистий reap мав завершитись успішно");
}

// --- Матриця: аварія під час reap ------------------------------------------

/// Аварія до першого дотику до журналу: сліду немає, файли на місці.
#[test]
fn reap_crash_before_journal_leaves_everything_untouched() {
    let layout = scenario_root("reap-before-journal");
    let code = run_victim(&layout, Operation::Reap, CrashPhase::ReapBeforeJournal);
    assert_eq!(code, CRASH_EXIT_CODE);

    let entries = restart_and_reconcile(&layout);
    assert_safe_state(&layout, &entries, "reap: аварія до журналу");
    assert!(entries.is_empty(), "журнал мав лишитись порожнім");
    let inventory = trashradar_e2e::inventory(&layout);
    assert_eq!(inventory.original_only.len() as u64, CANDIDATE_FILES);
    reset_root(&layout.root);
}

/// Аварія одразу після durable-запису `in_flight`: move не стартував →
/// відновлення відкочує кожен запис, файли лишаються на місці.
#[test]
fn reap_crash_after_journal_rolls_back_every_entry() {
    let layout = scenario_root("reap-after-journal");
    let code = run_victim(&layout, Operation::Reap, CrashPhase::ReapAfterJournal);
    assert_eq!(code, CRASH_EXIT_CODE);

    // Журнал пережив аварію процесу — інакше відкочувати не було б чого
    // (це і є перевірка durability SQLite, якої фейкові порти не дають).
    let before = IndexDatabase::open_profile(layout.profile_dir())
        .expect("відкрити manifest")
        .list_entries()
        .expect("список журналу");
    assert_eq!(
        count(&before, QuarantineStatus::InFlight) as u64,
        CANDIDATE_FILES,
        "після аварії журнал мав містити всі незавершені записи"
    );

    let entries = restart_and_reconcile(&layout);
    assert_safe_state(&layout, &entries, "reap: аварія після журналу");
    assert!(entries.is_empty(), "відкат мав прибрати всі записи");
    let inventory = trashradar_e2e::inventory(&layout);
    assert_eq!(inventory.original_only.len() as u64, CANDIDATE_FILES);
    reset_root(&layout.root);
}

/// Аварія посеред move'ів: частина файлів у карантині, решта на місці —
/// відновлення докочує перші й відкочує другі.
#[test]
fn reap_crash_mid_move_rolls_forward_moved_and_back_untouched() {
    let layout = scenario_root("reap-mid-move");
    let code = run_victim(&layout, Operation::Reap, CrashPhase::ReapMidMove);
    assert_eq!(code, CRASH_EXIT_CODE);

    let entries = restart_and_reconcile(&layout);
    assert_safe_state(&layout, &entries, "reap: аварія посеред move");

    let inventory = trashradar_e2e::inventory(&layout);
    assert_eq!(
        inventory.quarantine_only.len(),
        MID_BATCH_CALL,
        "у карантині мали лишитись рівно переміщені до аварії файли"
    );
    assert_eq!(
        inventory.original_only.len() as u64,
        CANDIDATE_FILES - MID_BATCH_CALL as u64
    );
    assert_eq!(
        count(&entries, QuarantineStatus::Quarantined),
        MID_BATCH_CALL
    );
    assert_eq!(
        entries.len(),
        MID_BATCH_CALL,
        "записи незачеплених файлів мали зникнути з журналу"
    );
    reset_root(&layout.root);
}

/// Аварія після всіх move, до підтвердження: файли в карантині, журнал каже
/// `in_flight` → відновлення докочує весь батч.
#[test]
fn reap_crash_after_moves_rolls_forward_whole_batch() {
    let layout = scenario_root("reap-after-moves");
    let code = run_victim(&layout, Operation::Reap, CrashPhase::ReapAfterMoves);
    assert_eq!(code, CRASH_EXIT_CODE);

    let entries = restart_and_reconcile(&layout);
    assert_safe_state(&layout, &entries, "reap: аварія до підтвердження");
    assert_eq!(
        count(&entries, QuarantineStatus::Quarantined) as u64,
        CANDIDATE_FILES
    );
    let inventory = trashradar_e2e::inventory(&layout);
    assert_eq!(inventory.quarantine_only.len() as u64, CANDIDATE_FILES);
    reset_root(&layout.root);
}

/// Аварія одразу після підтвердження: стан уже консистентний, відновленню
/// нема чого робити.
#[test]
fn reap_crash_after_confirm_needs_no_recovery() {
    let layout = scenario_root("reap-after-confirm");
    let code = run_victim(&layout, Operation::Reap, CrashPhase::ReapAfterConfirm);
    assert_eq!(code, CRASH_EXIT_CODE);

    let entries = restart_and_reconcile(&layout);
    assert_safe_state(&layout, &entries, "reap: аварія після підтвердження");
    assert_eq!(
        count(&entries, QuarantineStatus::Quarantined) as u64,
        CANDIDATE_FILES
    );
    reset_root(&layout.root);
}

// --- Матриця: аварія під час purge -----------------------------------------

/// Аварія до першого видалення: карантин недоторканий, все відновлюване.
#[test]
fn purge_crash_before_delete_keeps_quarantine_intact() {
    let layout = scenario_root("purge-before-delete");
    quarantine_everything(&layout);
    let code = run_victim(&layout, Operation::Purge, CrashPhase::PurgeBeforeDelete);
    assert_eq!(code, CRASH_EXIT_CODE);

    let entries = restart_and_reconcile(&layout);
    assert_safe_state(&layout, &entries, "purge: аварія до видалення");
    assert_eq!(
        count(&entries, QuarantineStatus::Quarantined) as u64,
        CANDIDATE_FILES
    );
    reset_root(&layout.root);
}

/// Аварія посеред батчу, на межі записів: оброблені знищені й позначені
/// `Purged`, решта лишається відновлюваною.
#[test]
fn purge_crash_mid_batch_leaves_rest_restorable() {
    let layout = scenario_root("purge-mid-batch");
    quarantine_everything(&layout);
    let code = run_victim(&layout, Operation::Purge, CrashPhase::PurgeMidBatch);
    assert_eq!(code, CRASH_EXIT_CODE);

    let entries = restart_and_reconcile(&layout);
    assert_safe_state(&layout, &entries, "purge: аварія посеред батчу");
    assert_eq!(count(&entries, QuarantineStatus::Purged), MID_BATCH_CALL);
    assert_eq!(
        count(&entries, QuarantineStatus::Quarantined) as u64,
        CANDIDATE_FILES - MID_BATCH_CALL as u64
    );
    reset_root(&layout.root);
}

/// Найгостріша точка purge: файл уже фізично знищено, а статус запису ще
/// `Quarantined`. Після рестарту журнал не має обіцяти відновлення того,
/// чого немає (фантомний запис), і не має губити решту карантину.
#[test]
fn purge_crash_between_delete_and_confirm_leaves_no_phantom_entry() {
    let layout = scenario_root("purge-after-delete");
    quarantine_everything(&layout);
    let code = run_victim(&layout, Operation::Purge, CrashPhase::PurgeAfterDelete);
    assert_eq!(code, CRASH_EXIT_CODE);

    let entries = restart_and_reconcile(&layout);
    assert_safe_state(&layout, &entries, "purge: аварія між видаленням і статусом");
    // Оброблені до аварії + сам перерваний запис — знищені (звірка з
    // реальністю докотила перерваний purge, T-156); решта карантину ціла.
    assert_eq!(
        count(&entries, QuarantineStatus::Quarantined) as u64,
        CANDIDATE_FILES - MID_BATCH_CALL as u64
    );
    assert_eq!(count(&entries, QuarantineStatus::Purged), MID_BATCH_CALL);

    // Найважливіше: після відновлення підсистема лишається робочою —
    // «Спорожнити все» (T-083) доводить справу до кінця, а не спотикається
    // об запис, чийого файла вже немає (без фіксу T-156 код виходу = 101).
    let code = run_victim(&layout, Operation::Purge, CrashPhase::None);
    assert_eq!(
        code, 0,
        "після аварії purge «Спорожнити все» має працювати, а не падати вічно"
    );
    reset_root(&layout.root);
}
