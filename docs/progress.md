# TrashRadar — Execution Progress

> Журнал виконання задач з [tasks.md](tasks.md). Одна задача = один рядок.
> Правила ведення: статус міняється лише разом з фактом (коміт/PR/верифікація);
> нова сесія починає з першого рядка без ✅ у порядку tasks.md.

## Стан середовища розробки (локальна машина)

- Node 20.19.6, npm 10.8.2
- Rust 1.96.1 stable-msvc (rustup) + rustfmt + clippy; MSVC Build Tools 2022; WebView2 — є
- Tauri CLI 2.11.4 — через `ui` devDependencies (`npm run tauri`)
- Запуск повного застосунку: з `ui/`: `npm run tauri dev -- --config ../core/shell/tauri.conf.json`

## Журнал задач

| Задача | Статус | Дата | Де | Нотатки |
|---|---|---|---|---|
| T-001 Каркас (Tauri+Rust+React) | ✅ | 2026-07-08 | PR #1 | 13 крейтів + UI; `cargo check/clippy/fmt` чисто; UI build чисто; 4 екрани перевірені у браузері. Іконка-заглушка `core/shell/icons/icon.ico` (tauri-build вимагає .ico) — фінальна у T-159 |
| T-002 CI | ✅ | 2026-07-08 | PR #2 | 3 джоби (UI / Core / Core shell+rust-cache) — зелені; installer.yml збирає NSIS-артефакт на push у main + dispatch; локальна збірка інсталятора верифікована (TrashRadar_0.1.0_x64-setup.exe, 1.9 МБ). Попутно виправлено tauri.conf: before*Command виконуються з кореня workspace (../ui, не ../../ui). Branch protection НЕ застосовано — див. відхилення. Автовидалення гілок після merge увімкнено |
| T-003 Логування Core | ✅ | 2026-07-08 | PR #3 | tracing + tracing-subscriber (env-filter); власний RotatingWriter за розміром (5 МБ, 3 бекапи, 4 юніт-тести); файл %LOCALAPPDATA%\TrashRadar\logs\core.log; рівні через TRASHRADAR_LOG; паніки логуються; збій init → деградація до stderr. Живий запуск верифікував стартовий запис у файлі |
| T-004 Канал команд UI→Core | ✅ | 2026-07-08 | PR #5 | shell/src/ipc.rs: модуль команд; app.ping {delayMs?, fail?} — постійна діагностична команда (roundtrip + неблокування + конверт помилок E2E). 4 інтеграційні тести через tauri mock runtime (tauri/test dev-фіча): roundtrip, помилка = конверт T-007, паралельність (швидка <300мс поки повільна спить 400мс), deny_unknown_fields. UI: handshake ping при старті. Живий запуск: ping від реального UI у core.log. +tokio/time (вже в дереві) |
| T-005 Канал подій Core→UI | ✅ | 2026-07-08 | PR #6 | shell/src/events.rs: реєстр топіків + emit() (fire-and-forget, з логуванням). Дротові імена: Tauri забороняє крапки в подіях → app.test ↔ app:test (wire_name ↔ client.ts). app.test_stream: ack одразу, події у фоні (модель architecture.md §1.2). Capabilities: core/shell/capabilities/default.json (core:default) — без нього webview не має права listen. 3 тести шини (доставка, unlisten зупиняє, ack до завершення потоку). UI self-check обох каналів при старті. Живий запуск: 3 емісії в core.log |
| T-007 Модель помилок Core | ✅ | 2026-07-08 | PR #4 | domain::error::CoreError: конверт {code, message}, 5 стартових стабільних кодів (internal/invalid_argument/not_implemented/io/cancelled), From<io::Error>, 4 тести форми/roundtrip. UI: CoreErrorPayload у types.ts, CoreIpcError + нормалізація в client.ts (+ клієнтські коди ipc_unavailable/unknown). Конверт зафіксовано в contracts/ipc-contract.json → error_envelope. E2E через живий IPC — вправляється у T-004 |

## Легенда

✅ виконано й верифіковано · 🔄 в роботі · ⛔ заблоковано (причина в нотатках)

## Відхилення від документів (фіксувати обов'язково)

- T-002: repository.md §9 вимагає «+1 рев'ю» на PR — не вмикається, бо репозиторій
  однієї людини (власник не може заапрувити власний PR). Увімкнути при появі команди.
- T-002: **branch protection (required checks) НЕ застосовано** — GitHub Free не
  підтримує його на приватних репозиторіях (API: «Upgrade to GitHub Pro or make
  this repository public»). Готовий конфіг: contexts = UI (typecheck + build),
  Core (fmt + clippy + check), Core shell (tauri check); strict=false;
  enforce_admins=false. Застосувати одразу після переходу на public/Pro.
