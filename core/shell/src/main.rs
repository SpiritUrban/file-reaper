//! Tauri-оболонка TrashRadar (docs/repository.md §5, крейт `shell`).
//!
//! Єдине місце, де світ UI зустрічає світ Core. Містить лише:
//! створення вікна, реєстрацію IPC за контрактами з `contracts/`,
//! життєвий цикл. Каркас T-001: реальні команди/події — T-004/T-005.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod events;
mod ipc;
mod logging;
mod scan_runtime;

fn recover_quarantine_at_startup() {
    let Some(profile) = logging::data_profile_dir() else {
        tracing::error!("Crash recovery пропущено: LOCALAPPDATA недоступна");
        return;
    };
    let database = match trashradar_index_sqlite::IndexDatabase::open_profile(profile) {
        Ok(database) => database,
        Err(error) => {
            tracing::error!(%error, "Crash recovery не відкрив manifest");
            return;
        }
    };
    let filesystem = trashradar_quarantine_fs::NativeQuarantineFs;
    match trashradar_app::QuarantineRecovery::new(&filesystem, &database).reconcile(|entry| {
        filesystem
            .surrogate_path(&entry.original_path, &entry.surrogate_name)
            .map(|path| path.to_string_lossy().into_owned())
    }) {
        Ok(result) => tracing::info!(
            completed = result.completed.len(),
            rolled_back = result.rolled_back.len(),
            "Crash recovery завершено"
        ),
        Err(error) => tracing::error!(%error, "Crash recovery потребує ручного втручання"),
    }
}

fn main() {
    // Логи — найперше: далі всі підсистеми вже під наглядом (T-003).
    match logging::init() {
        Ok(path) => tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            log_file = %path.display(),
            "TrashRadar Core запускається"
        ),
        Err(reason) => eprintln!("логування у файл недоступне: {reason}"),
    }
    recover_quarantine_at_startup();

    tauri::Builder::default()
        .manage(scan_runtime::ScanRuntime::new())
        .invoke_handler(tauri::generate_handler![
            ipc::app_health,
            ipc::app_ping,
            ipc::app_test_stream,
            ipc::app_request_elevation,
            ipc::app_decline_elevation,
            scan_runtime::scan_start,
            scan_runtime::scan_stop,
            scan_runtime::candidate_keep,
            scan_runtime::candidate_mark,
        ])
        .run(tauri::generate_context!())
        .expect("не вдалося запустити TrashRadar shell");
}
