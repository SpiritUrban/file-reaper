# TrashRadar

Радар для сміття на диску: програма сама знаходить файли-кандидати на видалення, групує їх за категоріями і показує, скільки місця можна звільнити. **Не файловий менеджер.**

## Завантажити

**[Сторінка завантажень](https://spiriturban.github.io/file-reaper/)** · [усі релізи](https://github.com/SpiritUrban/file-reaper/releases)

Встановлена копія оновлюється сама: застосунок перевіряє ендпоінт оновлень при
старті й показує банер, коли виходить нова версія.

## Документація

| Документ | Зміст |
|---|---|
| [docs/product.md](docs/product.md) | Product vision, MVP, roadmap |
| [docs/ui.md](docs/ui.md) | UI/UX специфікація, wireframes |
| [docs/architecture.md](docs/architecture.md) | Технічна архітектура, стек |
| [docs/features.md](docs/features.md) | Конкурентний аналіз, стратегія фіч |
| [docs/tasks.md](docs/tasks.md) | Декомпозиція: 20 епіків, 165 задач |
| [docs/repository.md](docs/repository.md) | Структура репозиторію, шари, конвенції |

## Структура

```
core/       Rust workspace — Core Engine (domain / app / infra / shell)
ui/         React + TypeScript + Tailwind — тонкий webview-клієнт
contracts/  Контракт IPC (єдине джерело правди для обох світів)
registry/   Декларативні дані детекторів
site/       Сайт-вітрина на GitHub Pages (статика + маніфест завантажень)
assets/     Джерело іконок (app-icon.svg → `tauri icon`)
scripts/    Запуск Tauri, версії, анотації CI, комплектація ffmpeg
```

## Запуск (dev)

Вимоги: Node 20+, Rust stable MSVC (rustup) + VS Build Tools (C++), WebView2 (Windows 10/11).

З **кореня репозиторію**:

```
npm run setup    # один раз: залежності UI
npm run dev      # повний застосунок (UI + Core / Tauri)
```

`npm run dev` піднімає Tauri з `core/` (де лежить `shell/tauri.conf.json`) — не з `ui/`.

Інші скрипти:

```
npm run dev:ui     # лише UI у браузері (localhost:5173)
npm run build      # релізний інсталятор (NSIS)
npm run build:ui   # лише фронт у ui/dist
```

Tauri CLI ставиться разом із dev-залежностями UI (`@tauri-apps/cli`).

## Реліз

Реліз збирається пушем тега `v*.*.*`; повний порядок дій, ручні кроки на GitHub і
чекліст — у [docs/release.md](docs/release.md).

```
node scripts/sync-version.mjs 0.2.0   # розставити версію по всіх файлах
node scripts/check-version.mjs        # звірити (той самий скрипт, що в CI)
```

## Статус

Каркас (T-001, tasks.md). Функціонал — за беклогом [docs/tasks.md](docs/tasks.md).

## Ліцензія

[MIT](LICENSE) — вільне використання зі збереженням авторства.

## Автор

**Vitaliy Dyachuk** — розробляю десктопні застосунки й інструменти, які роблять
видиму роботу за користувача: знаходять, показують і прибирають те, на що інакше
йде вечір ручної праці.

Інші проєкти та послуги: **[spiriturban.github.io](https://spiriturban.github.io/)** ·
GitHub: [@SpiritUrban](https://github.com/SpiritUrban)
