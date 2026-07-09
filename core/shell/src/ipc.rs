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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;
use tauri::{AppHandle, Runtime};
use trashradar_app::elevation::{
    elevation_benefit_message, elevation_benefit_summary, evaluate_elevation_prompt,
    ElevationSession,
};
use trashradar_domain::error::CoreError;
use trashradar_platform_win::{relaunch_elevated, ElevationRelaunch};

use crate::events;

static COMMANDS_RECEIVED: AtomicU64 = AtomicU64::new(0);
static COMMAND_ERRORS: AtomicU64 = AtomicU64::new(0);

/// Сесійна відмова від elevation (T-034): один процес = одна сесія.
static ELEVATION_DECLINED: AtomicBool = AtomicBool::new(false);

fn elevation_session() -> ElevationSession {
    let mut s = ElevationSession::new();
    if ELEVATION_DECLINED.load(Ordering::Relaxed) {
        s.decline();
    }
    s
}

pub(crate) fn record_command() {
    COMMANDS_RECEIVED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_command_error() {
    COMMAND_ERRORS.fetch_add(1, Ordering::Relaxed);
}

/// Module status row for the diagnostic health screen (T-009).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleHealth {
    pub name: &'static str,
    pub status: &'static str,
}

/// Live IPC counters for the diagnostic health screen (T-009).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcMetrics {
    pub commands_received: u64,
    pub command_errors: u64,
    pub events_emitted: u64,
    pub event_errors: u64,
}

/// План стратегії скану для одного тому (T-028) — видно у health.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VolumeScanPlan {
    /// Літера тому з двокрапкою, напр. `"C:"`.
    pub volume: String,
    /// `mft` | `directory_walk` | `usn_delta`.
    pub strategy: String,
    /// `ntfs_elevated` | `not_ntfs` | `not_elevated`.
    pub reason: String,
    /// Ім'я FS або `"unknown"`.
    pub file_system: String,
    pub elevated: bool,
}

/// Сесійний стан elevation для health/UI (T-034).
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ElevationInfo {
    /// `elevated` | `not_needed` | `offer` | `declined`.
    pub status: String,
    /// Чи UI має показувати банер з поясненням (активна пропозиція).
    pub offer_pending: bool,
    /// Користувач відмовився в цій сесії (DoD: без повторних запитів).
    pub declined_this_session: bool,
    /// Пояснення вигоди (українською).
    pub message: String,
    /// Короткий підсумок для компактних UI.
    pub summary: String,
    /// Скільки NTFS-томів виграли б від MFT.
    pub ntfs_volume_count: u32,
}

/// Відповідь `app.health` — використовується діагностикою (T-009, T-028, T-034).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthInfo {
    pub app_version: &'static str,
    pub core_status: &'static str,
    pub modules: Vec<ModuleHealth>,
    pub ipc: IpcMetrics,
    /// Процес запущено з адмін-правами (T-028).
    pub elevated: bool,
    /// Автовибір MFT ↔ walk по томах (T-028 DoD: видно у health).
    pub scan_plans: Vec<VolumeScanPlan>,
    /// Пропозиція / відмова elevation (T-034).
    pub elevation: ElevationInfo,
}

/// Відповідь `app.request_elevation` (T-034).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestElevationReply {
    /// `started` | `already_elevated`.
    pub status: String,
    /// Якщо `started` — поточний процес завершується після відповіді.
    pub will_exit: bool,
}

/// Відповідь `app.decline_elevation` (T-034).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclineElevationReply {
    pub declined: bool,
    /// Після decline offer більше не pending.
    pub offer_pending: bool,
}

/// Будує плани скану для всіх видимих томів (T-028).
pub fn build_scan_plans() -> (bool, Vec<VolumeScanPlan>) {
    use trashradar_app::ports::ScanEnvironment;
    use trashradar_app::scan_strategy::{choose_scan_strategy, VolumeCapabilities};
    use trashradar_platform_win::WinScanEnvironment;

    let env = WinScanEnvironment;
    let elevated = env.is_elevated();
    let mut plans = Vec::new();

    for volume in env.list_scan_volumes() {
        let file_system = env
            .file_system_name(volume)
            .ok()
            .flatten()
            .unwrap_or_else(|| "unknown".to_string());
        let is_ntfs = file_system.eq_ignore_ascii_case("NTFS");
        // Недоступний том (unknown + not ntfs) → walk / not_ntfs — чесно.
        let choice = choose_scan_strategy(&VolumeCapabilities {
            is_ntfs,
            is_elevated: elevated,
        });
        plans.push(VolumeScanPlan {
            volume: format!("{}:", volume.to_ascii_uppercase()),
            strategy: choice.strategy.as_str().to_string(),
            reason: choice.reason.as_str().to_string(),
            file_system,
            elevated,
        });
    }

    (elevated, plans)
}

/// Зібрати [`ElevationInfo`] з планів і сесійного стану (T-034).
pub fn build_elevation_info(elevated: bool, scan_plans: &[VolumeScanPlan]) -> ElevationInfo {
    let ntfs_volume_count = scan_plans
        .iter()
        .filter(|p| p.file_system.eq_ignore_ascii_case("NTFS"))
        .count() as u32;
    let session = elevation_session();
    let kind = evaluate_elevation_prompt(elevated, ntfs_volume_count > 0, session);
    ElevationInfo {
        status: kind.as_str().to_string(),
        offer_pending: kind.offer_pending(),
        declined_this_session: session.is_declined(),
        message: elevation_benefit_message().to_string(),
        summary: elevation_benefit_summary().to_string(),
        ntfs_volume_count,
    }
}

#[tauri::command]
pub fn app_health() -> HealthInfo {
    record_command();
    tracing::debug!("запит app.health");
    let event_metrics = events::metrics();
    let (elevated, scan_plans) = build_scan_plans();
    let elevation = build_elevation_info(elevated, &scan_plans);
    // T-028: автовибір готовий; деталі стратегії — у scan_plans, не в status.
    HealthInfo {
        app_version: env!("CARGO_PKG_VERSION"),
        core_status: "skeleton",
        modules: vec![
            ModuleHealth {
                name: "shell",
                status: "online",
            },
            ModuleHealth {
                name: "ipc.commands",
                status: "online",
            },
            ModuleHealth {
                name: "ipc.events",
                status: "online",
            },
            ModuleHealth {
                // T-031: scan.journal_stale зареєстровано; повний оркестратор — T-033.
                name: "scanner",
                status: if crate::events::scan_event_topics().is_empty() {
                    "planned"
                } else {
                    "online"
                },
            },
            ModuleHealth {
                name: "index",
                status: "planned",
            },
            ModuleHealth {
                name: "quarantine",
                status: "planned",
            },
        ],
        ipc: IpcMetrics {
            commands_received: COMMANDS_RECEIVED.load(Ordering::Relaxed),
            command_errors: COMMAND_ERRORS.load(Ordering::Relaxed),
            events_emitted: event_metrics.emitted,
            event_errors: event_metrics.failed,
        },
        elevated,
        scan_plans,
        elevation,
    }
}

/// Запит UAC elevation: relaunch з `runas` (T-034).
///
/// При успіху (`started`) процес завершується після відправки відповіді,
/// щоб не лишалось два вікна. Скасування UAC → `cancelled` (UI може
/// запропонувати decline).
#[tauri::command]
pub fn app_request_elevation() -> Result<RequestElevationReply, CoreError> {
    record_command();
    tracing::info!("запит app.request_elevation");
    match relaunch_elevated() {
        Ok(ElevationRelaunch::AlreadyElevated) => Ok(RequestElevationReply {
            status: "already_elevated".into(),
            will_exit: false,
        }),
        Ok(ElevationRelaunch::Started) => {
            tracing::info!("UAC accepted — elevated process started; exiting current");
            // Дати Tauri відправити IPC-відповідь, потім вийти.
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(200));
                std::process::exit(0);
            });
            Ok(RequestElevationReply {
                status: "started".into(),
                will_exit: true,
            })
        }
        Err(err) => {
            record_command_error();
            // Скасування UAC — не decline сесії автоматично: користувач
            // може натиснути «Продовжити без прав» окремо (явний шлях відмови).
            Err(err)
        }
    }
}

/// Відмова від elevation на сесію (T-034 DoD: без повторних запитів).
///
/// Сканування далі йде через directory walk (T-028). Повторний offer
/// з’явиться лише в новому процесі (перезапуск).
#[tauri::command]
pub fn app_decline_elevation() -> Result<DeclineElevationReply, CoreError> {
    record_command();
    tracing::info!("запит app.decline_elevation");
    ELEVATION_DECLINED.store(true, Ordering::Relaxed);

    let (elevated, plans) = build_scan_plans();
    let info = build_elevation_info(elevated, &plans);
    Ok(DeclineElevationReply {
        declined: true,
        offer_pending: info.offer_pending,
    })
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
    record_command();
    let payload = payload.unwrap_or_default();
    tracing::debug!(delay_ms = ?payload.delay_ms, fail = payload.fail, "запит app.ping");

    if payload.fail {
        record_command_error();
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
    record_command();
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
            .manage(crate::scan_runtime::ScanRuntime::new())
            .invoke_handler(tauri::generate_handler![
                app_health,
                app_ping,
                app_test_stream,
                crate::scan_runtime::scan_start,
                crate::scan_runtime::scan_stop,
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
    fn health_includes_scan_strategy_plans() {
        let (elevated, plans) = build_scan_plans();
        // На Windows є хоча б один том; elevated — bool без паніки.
        let _ = elevated;
        assert!(
            !plans.is_empty(),
            "очікували плани для томів, отримали порожньо"
        );
        for plan in &plans {
            assert!(
                plan.volume.ends_with(':') && plan.volume.len() == 2,
                "volume format: {}",
                plan.volume
            );
            assert!(
                matches!(
                    plan.strategy.as_str(),
                    "mft" | "directory_walk" | "usn_delta"
                ),
                "strategy: {}",
                plan.strategy
            );
            assert!(
                matches!(
                    plan.reason.as_str(),
                    "ntfs_elevated" | "not_ntfs" | "not_elevated"
                ),
                "reason: {}",
                plan.reason
            );
            // Інваріант T-028: MFT лише при NTFS+elevated.
            if plan.strategy == "mft" {
                assert_eq!(plan.reason, "ntfs_elevated");
                assert!(plan.elevated);
                assert!(plan.file_system.eq_ignore_ascii_case("NTFS"));
            }
            if plan.reason == "ntfs_elevated" {
                assert_eq!(plan.strategy, "mft");
            }
            if !plan.elevated {
                assert_ne!(plan.strategy, "mft");
            }
        }
    }

    #[test]
    fn health_includes_elevation_info_t034() {
        let (elevated, plans) = build_scan_plans();
        let info = build_elevation_info(elevated, &plans);
        assert!(
            matches!(
                info.status.as_str(),
                "elevated" | "not_needed" | "offer" | "declined"
            ),
            "status: {}",
            info.status
        );
        assert!(!info.message.is_empty());
        assert!(!info.summary.is_empty());
        // Інваріанти: elevated → status elevated, no offer.
        if elevated {
            assert_eq!(info.status, "elevated");
            assert!(!info.offer_pending);
        }
        // offer_pending лише для status=offer.
        if info.offer_pending {
            assert_eq!(info.status, "offer");
        }
        // З правами — MFT на NTFS (T-034 DoD частина 1).
        if elevated {
            for plan in &plans {
                if plan.file_system.eq_ignore_ascii_case("NTFS") {
                    assert_eq!(plan.strategy, "mft");
                }
            }
        } else {
            // Без прав — walk (T-034 + T-028).
            for plan in &plans {
                assert_ne!(plan.strategy, "mft");
            }
        }
    }

    #[test]
    fn decline_elevation_clears_offer_and_keeps_walk() {
        // DoD T-034: після відмови — walk, без повторного offer у сесії.
        let reply = app_decline_elevation().expect("decline ok");
        assert!(reply.declined);
        assert!(!reply.offer_pending);

        let (elevated, plans) = build_scan_plans();
        let info = build_elevation_info(elevated, &plans);
        assert!(info.declined_this_session);
        assert!(!info.offer_pending);
        if !elevated {
            assert_eq!(info.status, "declined");
            for plan in &plans {
                assert_ne!(
                    plan.strategy, "mft",
                    "після decline без admin — walk, plan={plan:?}"
                );
            }
        }
        // Повторний decline ідемпотентний.
        let again = app_decline_elevation().expect("second decline");
        assert!(again.declined);
        assert!(!again.offer_pending);
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
