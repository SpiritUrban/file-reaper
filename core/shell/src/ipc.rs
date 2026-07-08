//! Канал команд UI→Core (T-004).
//!
//! Правила:
//! - імена: контракт `namespace.dot` → функція `namespace_dot`
//!   (дзеркальне перетворення — у ui/src/ipc/client.ts);
//! - кожна команда, що може тривати, — `async`: Tauri виконує її на
//!   своєму рантаймі, IPC-потік webview не блокується
//!   (architecture.md §14: усе довше за 16 мс — асинхронне);
//! - помилки — виключно [`CoreError`] (конверт T-007);
//! - реєстрація — один список у [`crate::main`]: нова команда =
//!   функція тут + рядок у generate_handler + запис у contracts/.

use serde::{Deserialize, Serialize};
use std::time::Instant;
use tauri::{AppHandle, Runtime};
use trashradar_domain::error::CoreError;

use crate::events;

/// Відповідь `app.health` — використовується діагностикою (T-009).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthInfo {
    pub app_version: &'static str,
    pub core_status: &'static str,
}

#[tauri::command]
pub fn app_health() -> HealthInfo {
    tracing::debug!("запит app.health");
    HealthInfo {
        app_version: env!("CARGO_PKG_VERSION"),
        core_status: "skeleton",
    }
}

/// Параметри `app.ping`. Все опційне: `{}` — миттєвий успіх.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PingPayload {
    /// Штучна затримка відповіді — перевірка неблокування UI.
    #[serde(default)]
    pub delay_ms: Option<u64>,
    /// Запросити відмову — перевірка конверта помилок наскрізь.
    #[serde(default)]
    pub fail: bool,
}

/// Відповідь `app.ping`.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PingReply {
    pub version: String,
    /// Фактично застосована затримка.
    pub delayed_ms: u64,
}

/// Діагностичний ping: живий наскрізний шлях UI→Core→UI.
///
/// `async` — довгий ping не блокує ні IPC-потік, ні інші команди.
#[tauri::command]
pub async fn app_ping(payload: Option<PingPayload>) -> Result<PingReply, CoreError> {
    let payload = payload.unwrap_or_default();
    tracing::debug!(delay_ms = ?payload.delay_ms, fail = payload.fail, "запит app.ping");

    if payload.fail {
        return Err(CoreError::invalid_argument(
            "Тестова відмова app.ping (fail=true).",
        ));
    }

    let delayed_ms = payload.delay_ms.unwrap_or(0);
    if delayed_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delayed_ms)).await;
    }

    Ok(PingReply {
        version: env!("CARGO_PKG_VERSION").to_string(),
        delayed_ms,
    })
}

/// Параметри `app.test_stream`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestStreamPayload {
    /// Скільки подій надіслати (1..=1000, дефолт 5).
    #[serde(default)]
    pub count: Option<u32>,
    /// Пауза між подіями, мс (0..=1000, дефолт 50).
    #[serde(default)]
    pub interval_ms: Option<u64>,
}

/// Неблокуюче підтвердження прийняття `app.test_stream`.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestStreamAck {
    pub accepted: u32,
}

/// Подія діагностичного потоку (топік `app.test`).
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestEvent {
    pub seq: u32,
    pub of: u32,
}

/// Діагностичний стрім подій: команда підтверджує прийняття ОДРАЗУ,
/// а події `app.test` летять у фоні — модель «команда → потік подій»
/// з architecture.md §1.2 у мініатюрі.
#[tauri::command]
pub async fn app_test_stream<R: Runtime>(
    app: AppHandle<R>,
    payload: Option<TestStreamPayload>,
) -> Result<TestStreamAck, CoreError> {
    let payload = payload.unwrap_or_default();
    let count = payload.count.unwrap_or(5).clamp(1, 1000);
    let interval = std::time::Duration::from_millis(payload.interval_ms.unwrap_or(50).min(1000));
    tracing::debug!(count, ?interval, "запит app.test_stream");

    tauri::async_runtime::spawn(async move {
        let mut counter_throttle =
            events::CounterThrottle::new_at(std::time::Duration::from_millis(100), Instant::now());
        for seq in 1..=count {
            events::emit(&app, events::topic::APP_TEST, &TestEvent { seq, of: count });
            if let Some(snapshot) = counter_throttle.observe(Instant::now(), 1) {
                events::emit(&app, events::topic::APP_TEST_COUNTER, &snapshot);
            }
            if seq < count {
                tokio::time::sleep(interval).await;
            }
        }
        if let Some(snapshot) = counter_throttle.flush(Instant::now()) {
            events::emit(&app, events::topic::APP_TEST_COUNTER, &snapshot);
        }
    });

    Ok(TestStreamAck { accepted: count })
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use serde_json::json;
    use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
    use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
    use tauri::webview::InvokeRequest;
    use tauri::WebviewWindowBuilder;

    use super::*;

    /// Застосунок на mock-рантаймі з тим самим списком команд, що й main.
    fn test_app() -> (
        tauri::App<tauri::test::MockRuntime>,
        tauri::WebviewWindow<tauri::test::MockRuntime>,
    ) {
        let app = mock_builder()
            .invoke_handler(tauri::generate_handler![
                app_health,
                app_ping,
                app_test_stream
            ])
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("mock webview");
        (app, webview)
    }

    fn request(cmd: &str, body: serde_json::Value) -> InvokeRequest {
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "http://tauri.localhost".parse().expect("url"),
            body: InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        }
    }

    fn body_json(body: InvokeResponseBody) -> serde_json::Value {
        match body {
            InvokeResponseBody::Json(raw) => serde_json::from_str(&raw).expect("json"),
            other => panic!("очікували JSON-відповідь, отримали {other:?}"),
        }
    }

    #[test]
    fn ping_roundtrips_ui_core_ui() {
        let (_app, webview) = test_app();
        let response =
            get_ipc_response(&webview, request("app_ping", json!({}))).expect("успішна відповідь");
        let reply: PingReply = serde_json::from_value(body_json(response)).expect("форма reply");
        assert_eq!(reply.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(reply.delayed_ms, 0);
    }

    #[test]
    fn ping_failure_returns_t007_envelope() {
        let (_app, webview) = test_app();
        let envelope = get_ipc_response(
            &webview,
            request("app_ping", json!({ "payload": { "fail": true } })),
        )
        .expect_err("команда мусить відмовити");
        assert_eq!(envelope["code"], "invalid_argument");
        assert!(envelope["message"].as_str().expect("текст").ends_with('.'));
    }

    #[test]
    fn long_ping_does_not_block_parallel_commands() {
        let (_app, webview) = test_app();
        let started = Instant::now();
        let slow = std::thread::spawn({
            let webview = webview.clone();
            move || {
                get_ipc_response(
                    &webview,
                    request("app_ping", json!({ "payload": { "delayMs": 400 } })),
                )
            }
        });
        // Поки повільний ping спить, швидкі команди мають відповідати.
        let quick = get_ipc_response(&webview, request("app_health", json!({})));
        let quick_elapsed = started.elapsed();
        assert!(quick.is_ok(), "паралельна команда відповіла");
        assert!(
            quick_elapsed.as_millis() < 300,
            "швидка команда не чекала на повільну: {quick_elapsed:?}"
        );
        let slow = slow
            .join()
            .expect("потік")
            .expect("повільний ping успішний");
        let reply: PingReply = serde_json::from_value(body_json(slow)).expect("reply");
        assert_eq!(reply.delayed_ms, 400);
    }

    /// Rust-підписник шини: збирає TestEvent-и у канал.
    fn listen_test_events(
        app: &tauri::App<tauri::test::MockRuntime>,
    ) -> (tauri::EventId, std::sync::mpsc::Receiver<TestEvent>) {
        use tauri::Listener;
        let (tx, rx) = std::sync::mpsc::channel::<TestEvent>();
        let id = app.listen(
            crate::events::wire_name(crate::events::topic::APP_TEST),
            move |event| {
                let parsed: TestEvent = serde_json::from_str(event.payload()).expect("payload");
                let _ = tx.send(parsed);
            },
        );
        (id, rx)
    }

    fn drain_for(
        rx: &std::sync::mpsc::Receiver<TestEvent>,
        window: std::time::Duration,
    ) -> Vec<TestEvent> {
        let deadline = Instant::now() + window;
        let mut received = Vec::new();
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match rx.recv_timeout(deadline - now) {
                Ok(event) => received.push(event),
                Err(_) => break,
            }
        }
        received
    }

    #[test]
    fn test_stream_delivers_all_events_to_subscriber() {
        let (app, webview) = test_app();
        let (_id, rx) = listen_test_events(&app);
        let ack = get_ipc_response(
            &webview,
            request(
                "app_test_stream",
                json!({ "payload": { "count": 5, "intervalMs": 1 } }),
            ),
        )
        .expect("ack");
        let ack: TestStreamAck = serde_json::from_value(body_json(ack)).expect("форма ack");
        assert_eq!(ack.accepted, 5);

        let received = drain_for(&rx, std::time::Duration::from_millis(1500));
        assert_eq!(received.len(), 5, "усі події дійшли: {received:?}");
        assert_eq!(received[0], TestEvent { seq: 1, of: 5 });
        assert_eq!(received[4], TestEvent { seq: 5, of: 5 });
    }

    #[test]
    fn ack_returns_before_stream_completes() {
        let (app, webview) = test_app();
        let (_id, rx) = listen_test_events(&app);
        let started = Instant::now();
        get_ipc_response(
            &webview,
            request(
                "app_test_stream",
                json!({ "payload": { "count": 4, "intervalMs": 150 } }),
            ),
        )
        .expect("ack");
        let ack_elapsed = started.elapsed();
        assert!(
            ack_elapsed.as_millis() < 300,
            "підтвердження прийняття не чекає завершення потоку: {ack_elapsed:?}"
        );
        // Потік завершується вже ПІСЛЯ підтвердження.
        let received = drain_for(&rx, std::time::Duration::from_millis(2000));
        assert_eq!(received.len(), 4);
    }

    #[test]
    fn unlisten_stops_delivery() {
        use tauri::Listener;
        let (app, webview) = test_app();
        let (id, rx) = listen_test_events(&app);

        get_ipc_response(
            &webview,
            request(
                "app_test_stream",
                json!({ "payload": { "count": 2, "intervalMs": 1 } }),
            ),
        )
        .expect("перший потік");
        let first = drain_for(&rx, std::time::Duration::from_millis(1000));
        assert_eq!(first.len(), 2, "до відписки події доходять");

        app.unlisten(id);

        get_ipc_response(
            &webview,
            request(
                "app_test_stream",
                json!({ "payload": { "count": 3, "intervalMs": 1 } }),
            ),
        )
        .expect("другий потік");
        let after = drain_for(&rx, std::time::Duration::from_millis(500));
        assert!(
            after.is_empty(),
            "після відписки доставка зупинена, отримано: {after:?}"
        );
    }

    #[test]
    fn unknown_payload_fields_are_rejected() {
        let (_app, webview) = test_app();
        let result = get_ipc_response(
            &webview,
            request(
                "app_ping",
                json!({ "payload": { "delayMs": 1, "oops": true } }),
            ),
        );
        assert!(result.is_err(), "deny_unknown_fields працює");
    }
}
