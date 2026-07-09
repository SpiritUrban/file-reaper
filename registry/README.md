# Detector Registry

Декларативні **дані** детекторів (не код) — docs/architecture.md §6, docs/repository.md §1.

Оновлюються без релізу / перекомпіляції ядра: Core читає JSON **з диска** у рантаймі
(`trashradar_app::location_registry`).

| Файл | Призначення | Задачі |
|---|---|---|
| `known-locations.json` | Temp-локації та кеші програм | T-044 (схема), T-045, T-046 → детектори T-047/T-048 |
| `dev-artifacts.json` | маркери структурних детекторів | T-049…T-051 (схема — окремо) |

---

## `known-locations.json` — схема v1 (T-044)

```json
{
  "schema_version": 1,
  "locations": [ LocationEntry, ... ]
}
```

### `LocationEntry`

| Поле | Тип | Обов’язкове | Опис |
|---|---|---|---|
| `id` | string | так | Стабільний унікальний id (`windows.temp.user`, `browser.chrome.cache`) |
| `kind` | enum | так | `temp_files` \| `app_caches` |
| `safety` | enum | так | `safe_to_bulk` \| `review_recommended` |
| `paths` | string[] | так (≥1) | Шаблони шляхів з `%VAR%` |
| `match_mode` | enum | ні (дефолт `prefix`) | `prefix` — кандидат під коренем |
| `explanation` | string | так | Рядок вердикту / UI |
| `label` | string | ні | Короткий ярлик UI |

Невідомі поля **заборонені** (`deny_unknown_fields`) — помилка завантаження.

### Плейсхолдери шляхів

Підставляються з змінних середовища процесу (Windows):

`%TEMP%`, `%TMP%`, `%LOCALAPPDATA%`, `%APPDATA%`, `%USERPROFILE%`,
`%WINDIR%`, `%SystemRoot%`, `%PROGRAMDATA%`, `%PROGRAMFILES%`,
`%PROGRAMFILES(X86)%`, `%HOMEDRIVE%`, `%HOMEPATH%`.

Приклад:

```json
{
  "id": "windows.temp.user",
  "kind": "temp_files",
  "safety": "safe_to_bulk",
  "paths": ["%TEMP%", "%LOCALAPPDATA%\\Temp"],
  "match_mode": "prefix",
  "explanation": "тимчасові файли користувача",
  "label": "User Temp"
}
```

### Завантаження (рантайм)

Порядок пошуку `known-locations.json`:

1. `$TRASHRADAR_REGISTRY_DIR/known-locations.json`
2. `registry/known-locations.json` поруч із exe
3. евристики dev (`target/…` → корінь репо)

API: `KnownLocationsRegistry::load_default()` / `load_from_file` / `from_json_str`.

**DoD T-044:** новий об’єкт у `locations[]` з’являється після збереження файлу —
без змін і перезбірки Rust-коду (потрібен лише перезапуск процесу / reload реєстру).

### Валідація

- `cargo test -p trashradar-app location_registry`
- завантажує workspace `registry/known-locations.json`, якщо файл існує

### Temp-локації (T-045)

| id | Шляхи | Примітка |
|---|---|---|
| `windows.temp.user` | `%TEMP%`, `%TMP%`, `%LOCALAPPDATA%\Temp` | User profile temp (зазвичай збігаються) |
| `windows.temp.system` | `%WINDIR%\Temp`, `%SystemRoot%\Temp` | `C:\Windows\Temp` |
| `windows.temp.programdata` | `%PROGRAMDATA%\Temp` | може бути відсутній — ok |
| `windows.temp.internet` | INetCache / Temporary Internet Files | legacy IE + сучасний INetCache |

Усі `kind: temp_files`, `safety: safe_to_bulk`. DoD: user + system temp існують на чистій Win10/11.

### Кеші програм ЦА (T-046)

≥20 записів `kind: app_caches` — браузери, IDE, пакетні менеджери, месенджери,
рендер-кеші, ігри/медіа:

| Сегмент | Приклади id |
|---|---|
| Браузери | `browser.chrome.cache`, `browser.edge.cache`, `browser.firefox.cache`, Brave, Opera |
| IDE | `ide.vscode.cache`, `ide.cursor.cache`, JetBrains, VS |
| Пакетні менеджери | npm, yarn, pnpm, pip, cargo, NuGet HTTP |
| Месенджери | Discord, Telegram (review), Slack, Teams |
| Рендер | Adobe Media Cache, AE temp, DaVinci Resolve, Blender |
| Ігри/медіа | Steam htmlcache, Spotify |

**Верифікація:** `cargo test -p trashradar-app t046` — каталог ≥20 + live probe
існуючих каталогів на машині (встановлене ПЗ). Відсутня інсталяція ≠ помилка схеми.

---

## `dev-artifacts.json`

Каркас для структурних детекторів (T-049+). Схема v1 фіксується разом з T-049.
