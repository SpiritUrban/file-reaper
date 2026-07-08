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
| T-002 CI | 🔄 в роботі | 2026-07-08 | — | ci.yml: UI + Core(без shell) були з T-001; додаються shell-job, installer-артефакт, branch protection |

## Легенда

✅ виконано й верифіковано · 🔄 в роботі · ⛔ заблоковано (причина в нотатках)

## Відхилення від документів (фіксувати обов'язково)

- T-002: repository.md §9 вимагає «+1 рев'ю» на PR — не вмикається, бо репозиторій
  однієї людини (власник не може заапрувити власний PR). Увімкнути при появі команди.
- T-002: `enforce_admins = false` у branch protection — власник може пушити в main
  повз checks (відповідає фактичному робочому процесу власника).
