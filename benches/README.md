# Benchmarks

Еталонні сценарії бенчмарків — цілі docs/architecture.md §15.
Наповнюються задачами T-019, T-035, T-066, T-076, T-154.
Політика: регрес метрик понад поріг блокує merge (docs/repository.md §9).

Бенчмарки винесені з `core/` (окремі крейти зі своїм target-каталогом),
бо їхні еталонні сценарії спільні для кількох крейтів і для CI-гейта.

## Мапа цілей §15 → автозаміри (T-154)

Кожна метрика architecture.md §15 має автозамір; регрес блокує CI жорсткими
стелями та детермінованими інваріантами. Одна метрика, де поточна реалізація
ще не досягає цілі §15 (борг зафіксовано у progress.md, відхилення T-154),
має ціль як WARN-лінію + катастрофічний guard як жорстку стелю.

| Метрика §15 | Ціль | Автозамір | Гейт у CI |
|---|---|---|---|
| Перша цифра (NTFS, ~1.5 млн файлів, теплий диск) | < 10 с | `scan-bench` (T-035) | **стеля 10 с** (synthetic конвеєр); живий том — локально `--volume` |
| Повторний запуск (USN-дельта) | < 2 с | `rerun-bench` (T-154) | ціль 2 с — WARN (борг відновлення з SQLite); **guard 60 с** ловить квадратичні регреси |
| Превью P0: кеш / генерація | < 100 мс / < 600 мс | тести `trashradar-app` `preview` (T-076) | джоб Core `cargo test --workspace` — **тест падає** при перевищенні |
| Reap 1 000 файлів на одному томі | < 2 с | `reap-bench` (T-154) | **стеля 2 с** (жорстко; досяжна після фазового батчування manifest-транзакцій) |
| RAM Core після скану 1 ТБ | < 300 МБ | `scan-bench` `index_memory_bytes` (**стеля 300 МіБ**) + `index-bench` (регрес >15% жорстко) | детермінований hard-gate; повний RSS-профіль процесу — T-157 |

## `index-bench` — гейт in-memory індексу (T-019)

Фіксує пам'ять і швидкість `InMemoryIndex` на детермінованому наборі з
1 000 000 записів і звіряє їх із `index-bench/baseline.json`.

```sh
cargo run --release --manifest-path benches/index-bench/Cargo.toml            # check-режим (CI)
cargo run --release --manifest-path benches/index-bench/Cargo.toml -- --strict  # жорсткий гейт усіх метрик
cargo run --release --manifest-path benches/index-bench/Cargo.toml -- --bless   # перезаписати baseline
```

Метрики:

| Метрика | Гейт | Пояснення |
|---|---|---|
| `index_memory_bytes` | **жорсткий, регрес >15% валить CI** | детермінована оцінка heap (`capacity()`), однакова на однаковому таргеті |
| `build_millis` | попередження + абс. стеля | час наповнення індексу батчами |
| `search_rare_millis` | попередження + абс. стеля | підрядковий пошук, повний прохід |
| `search_frequent_millis` | попередження + абс. стеля | пошук з лімітом (рання зупинка) |

Тайминги залежать від заліза shared-runner'а GitHub, тому їхній регрес у CI —
попередження (плюс щедра абсолютна стеля проти катастрофічного регресу).
На машині, де знято baseline, `--strict` перетворює регрес будь-якої метрики
на помилку. Оновлювати baseline (`--bless`) слід свідомо, з поясненням у PR.

## `scan-bench` — гейт скану «до наповнення індексу» (T-035)

Ціль architecture.md §15: **~1.5 млн файлів → < 10 с** (теплий диск) до
наповнення індексу. Два режими:

### Synthetic (CI, за замовчуванням)

CPU-конвеєр T-022/T-023/T-024: `PathResolver` + `Batcher` → `InMemoryIndex`
на 1 500 000 синтетичних файлів (без I/O). Жорстка стеля fill = 10 с;
пам'ять — hard-gate >15%; тайминги — WARN + стеля (як T-019).

```sh
cargo run --release --manifest-path benches/scan-bench/Cargo.toml              # CI check
cargo run --release --manifest-path benches/scan-bench/Cargo.toml -- --strict  # hard timing
cargo run --release --manifest-path benches/scan-bench/Cargo.toml -- --bless   # rewrite baseline
```

| Метрика | Гейт | Пояснення |
|---|---|---|
| `index_fill_millis` | WARN + **стеля 10 с (§15)** | час PathResolver+Batcher→index |
| `files_per_sec` | WARN + floor (≥150k/s) | throughput (інвертований ratio) |
| `index_memory_bytes` | **жорсткий, регрес >15%** | heap після `finish_indexing` |

### Live volume (локально, elevated)

Повний MFT → index (`scan_volume_to_index`). Прогрів + теплий прохід;
проєкція часу на 1.5 млн файлів валить процес, якщо > 10 с.

```sh
# Elevated PowerShell / cmd:
cargo run --release --manifest-path benches/scan-bench/Cargo.toml -- --volume C
```

Без адмін-прав — exit 3 (не використовується в CI).

## `dup-bench` — гейт каскаду дублікатів (T-066)

Еталонна «медіатека» — синтетичний корпус метаданих (50 000 файлів,
2 000 dup-груп × 3; `MapHasher` емулює partial/full без реального I/O).
Дві сесії:

1. **Перша (cold)** — каскад size→partial→full з наповненням `MemoryHashCache`.
2. **Повторна (warm)** — той самий корпус + кеш (T-062): **disk_reads = 0**.

```sh
cargo run --release --manifest-path benches/dup-bench/Cargo.toml              # CI check
cargo run --release --manifest-path benches/dup-bench/Cargo.toml -- --strict  # hard timing
cargo run --release --manifest-path benches/dup-bench/Cargo.toml -- --bless   # rewrite baseline
```

| Метрика | Гейт | Пояснення |
|---|---|---|
| `first_session_millis` | WARN + стеля **8 с** | cold cascade |
| `second_session_millis` | WARN + стеля **2 с** | warm cache session |
| `second_disk_reads` | **жорстко = 0** | DoD T-062: повтор без перехешу |
| `confirmed_groups` | **жорстко = 2000** | усі dup-групи підтверджені |
| `reclaimable_bytes` | **жорстко = expected** | Σ size×(n−1) по групах |

Тайминги — як у T-019/T-035: на shared-runner WARN + absolute ceiling;
`--strict` — жорсткий 15%-регрес локально.

## `rerun-bench` — гейт повторного запуску (T-154)

Ціль §15: **повторний запуск (USN-дельта) < 2 с до актуальної цифри**.
Синтетичний детермінований корпус: 1 500 000 записів у SQLite (сетап, не
гейтиться) → гейтований конвеєр рестарту:

1. відновлення in-memory індексу з SQLite (T-017);
2. USN-дельта ~11 тис. подій: create/modify/delete/rename (T-030);
3. перерахунок предикатних детекторів (T-038);
4. агрегація унікальної цифри (T-054).

```sh
cargo run --release --manifest-path benches/rerun-bench/Cargo.toml              # CI check
cargo run --release --manifest-path benches/rerun-bench/Cargo.toml -- --strict  # hard timing
cargo run --release --manifest-path benches/rerun-bench/Cargo.toml -- --bless   # rewrite baseline
```

| Метрика | Гейт | Пояснення |
|---|---|---|
| `rerun_total_millis` | WARN на регрес; ціль §15 2 с — WARN; **guard 60 с жорстко** | увесь конвеєр рестарту; guard ловить квадратичні шляхи (до фіксів T-154 — хвилини+) |
| `restore_millis` / `usn_apply_millis` / `recalc_millis` / `totals_millis` | WARN + guard | розбивка фаз для діагностики регресу |
| `files_after_delta` | **жорстко = 1 502 000** | create/delete/rename застосовані точно |

Корпус свідомо песимістичний: **усі** 1.5 млн файлів — кандидати у
persistent-індексі (реальний обсяг залежить від детекторів). Ціль §15 2 с
на ньому поки не досягається (домінує відновлення з SQLite ~5 с) — див.
відхилення T-154 у progress.md.

## `reap-bench` — гейт транзакційного reap (T-154)

Ціль §15: **reap 1 000 файлів на одному томі < 2 с**. Реальна FS
(temp-каталог = один том), справжні `NativeQuarantineFs` + SQLite-manifest —
повний шлях T-079 (durable in_flight → атомарний move → підтвердження +
аудит). Прогрівний батч (AV/кеші FS/WAL) — не гейтиться; вимірюється другий
(«теплий», як live-режим scan-bench).

```sh
cargo run --release --manifest-path benches/reap-bench/Cargo.toml              # CI check
cargo run --release --manifest-path benches/reap-bench/Cargo.toml -- --strict  # hard timing
cargo run --release --manifest-path benches/reap-bench/Cargo.toml -- --bless   # rewrite baseline
```

| Метрика | Гейт | Пояснення |
|---|---|---|
| `reap_batch_millis` | WARN на регрес; **стеля §15 2 с жорстко** | журнал + move + підтвердження на 1 000 файлів |
| `journal_millis` / `moves_millis` / `confirm_millis` | інформаційно | фазова розбивка для діагностики регресу |
| `reaped_count` / стан manifest | **жорстко** | всі 1 000 переміщені і підтверджені |

Ціль §15 досягнута фазовим батчуванням manifest-транзакцій (закрите
відхилення T-154): усі in_flight одним tx → послідовні атомарні move →
підтвердження + аудит одним tx (семантика crash-recovery T-084 незмінна —
reconcile докочує/відкочує in_flight звіркою з реальністю). SQLite-фази
тепер ~50 мс сумарно; домінує move-фаза (~0.9–1.8 с: WinAPI move + guard +
identity під AV-фільтром). Перед заміром — пауза 1.5 с: реал-тайм AV сканує
щойно створені файли і спотворює замір до ~7×; прогони бенча впритул один
за одним контамінують замір (AV ще сканує сліди попереднього).

Windows-only (WinAPI move/identity); на інших ОС — no-op.
