//! Канал подій Core→UI (T-005).
//!
//! Правила:
//! - топіки — лише з реєстру [`topic`] (дзеркало contracts/ →
//!   ui/src/ipc/types.ts EventName); довільні рядки заборонені;
//! - Core надсилає події через [`emit`] — єдину точку з логуванням;
//!   збій доставки не валить викликача (UI міг ще не піднятися);
//! - підписки й відписки — на боці UI (ui/src/ipc/client.ts
//!   subscribe → UnlistenFn); Rust-підписники використовуються
//!   у тестах для верифікації шини.
//!
//! Тротлінг агрегованих подій — окрема задача T-006.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

/// Реєстр топіків подій (канонічні контрактні імена). Новий топік =
/// константа тут + запис у contracts/ipc-contract.json + EventName
/// в ui/src/ipc/types.ts.
pub mod topic {
    /// Діагностичний потік (команда `app.test_stream`).
    pub const APP_TEST: &str = "app.test";
}

/// Контрактне ім'я → дротове ім'я шини Tauri.
///
/// Tauri забороняє крапки в іменах подій (IllegalEventName), тому на
/// дроті `app.test` стає `app:test`. Дзеркальне перетворення — в
/// ui/src/ipc/client.ts (subscribe). Скрізь у коді й контракті
/// фігурують лише канонічні імена з крапками.
pub fn wire_name(topic: &str) -> String {
    topic.replace('.', ":")
}

/// Надсилає подію всім підписникам топіка.
///
/// Помилка доставки логується і НЕ повертається: емісія подій —
/// fire-and-forget за контрактом (architecture.md §1.2), відправник
/// не має залежати від стану webview.
pub fn emit<R: Runtime, T: Serialize + Clone>(app: &AppHandle<R>, topic: &str, payload: &T) {
    match app.emit(&wire_name(topic), payload.clone()) {
        Ok(()) => tracing::trace!(topic, "подію надіслано"),
        Err(error) => tracing::warn!(topic, %error, "не вдалося надіслати подію"),
    }
}
