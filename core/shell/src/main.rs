//! Tauri-оболонка TrashRadar (docs/repository.md §5, крейт `shell`).
//!
//! Єдине місце, де світ UI зустрічає світ Core. Містить лише:
//! створення вікна, реєстрацію IPC за контрактами з `contracts/`,
//! життєвий цикл. Каркас T-001: реальні команди/події — T-004/T-005.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod logging;

use serde::Serialize;

/// Відповідь команди `app.health` — єдиної команди каркаса.
/// Використовується діагностичним екраном (T-009) для перевірки
/// зв'язку UI ↔ Core.
#[derive(Serialize)]
struct HealthInfo {
    app_version: &'static str,
    core_status: &'static str,
}

#[tauri::command]
fn app_health() -> HealthInfo {
    tracing::debug!("запит app.health");
    HealthInfo {
        app_version: env!("CARGO_PKG_VERSION"),
        core_status: "skeleton",
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

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![app_health])
        .run(tauri::generate_context!())
        .expect("не вдалося запустити TrashRadar shell");
}
