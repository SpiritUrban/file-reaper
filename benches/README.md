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
