# Benchmarks

Еталонні сценарії бенчмарків — цілі docs/architecture.md §15.
Наповнюються задачами T-019, T-035, T-066, T-076, T-154.
Політика: регрес метрик понад поріг блокує merge (docs/repository.md §9).

Бенчмарки винесені з `core/` (окремі крейти зі своїм target-каталогом),
бо їхні еталонні сценарії спільні для кількох крейтів і для CI-гейта.

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
