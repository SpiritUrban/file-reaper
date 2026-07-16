# E2E Tests

Наскрізні тести повного циклу: скан → категорії → позначення → reap → restore.
Наповнюються з Milestone M3 (docs/tasks.md).

## `e2e/` — аварійне відновлення Quarantine (T-156)

Матриця точок переривання reap/purge на **реальній** FS і SQLite. Кожен рядок
матриці — окремий дочірній процес (`crash-victim`), що виконує продуктовий
шлях і гине у заданій фазі через `std::process::exit` (без деструкторів і без
закриття БД). Батько відкриває ту саму БД наново, виконує
`QuarantineRecovery::reconcile` (T-084) і перевіряє інваріанти безпеки D4:
жодної втрати даних, дублювання, файлів-сиріт чи фантомних записів.

Standalone-крейт (як `benches/*`): власний `target/`, не в `core/` workspace.
Windows-only (`NativeQuarantineFs`/`read_file_identity` — WinAPI; MVP теж).

```
cd tests/e2e
cargo test
```
