# Scripts

| Файл | Призначення |
|---|---|
| `tauri.mjs` | Запуск Tauri з кореня (`npm run dev` / `npm run build` / `npm run tauri`). Стартує з `core/`, щоб CLI знайшов `shell/tauri.conf.json`. Цей самий скрипт викликає `tauri-action` у релізі. |
| `check-grid-invariants.mjs` | Сторож сітки плиток: заборонені анти-патерни + selftest геометрії. `npm run test:grid`. |
| `version-files.mjs` | Спільний список носіїв версії для двох скриптів нижче — щоб «синхронізувати» і «перевірити» ніколи не розійшлися. |
| `sync-version.mjs` | Розставляє одну версію по `package.json` × 2, `tauri.conf.json`, `Cargo.toml`, `Cargo.lock`. `npm run version:sync 0.2.0`. |
| `check-version.mjs` | Звіряє їх між собою і з `GITHUB_REF_NAME`. Джоба `validate-version` у `release.yml`. |
| `ci-annotate.sh` | Виводить хвіст логу як GitHub-анотацію. Логи ранів закриті без авторизації, анотації — ні. Ліміт 2500 символів: GitHub ріже анотацію на 4096 і відкидає **хвіст**, тобто саму помилку. |
| `download-ffmpeg-resources.mjs` | Кладе статичний ffmpeg у `core/shell/resources/` перед `tauri build`. Викликається в `release.yml` та `installer.yml`. |
| `generate-download-manifest.mjs` | Будує `site/download-manifest.json` зі списку ассетів релізу (GitHub API). Імена файлів ніколи не вигадуються. |

Повний порядок релізу і ручні кроки на GitHub — [docs/release.md](../docs/release.md).
