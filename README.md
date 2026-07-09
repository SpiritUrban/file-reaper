# TrashRadar

Радар для сміття на диску: програма сама знаходить файли-кандидати на видалення, групує їх за категоріями і показує, скільки місця можна звільнити. **Не файловий менеджер.**

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

## Статус

Каркас (T-001, tasks.md). Функціонал — за беклогом [docs/tasks.md](docs/tasks.md).
