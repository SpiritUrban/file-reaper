# IPC Contract

Єдине джерело правди для команд і подій між Core (Rust) та UI (TypeScript).

- `ipc-contract.json` — перелік команд і подій з просторами імен (naming: docs/repository.md §7.3).
- Кодогенерація типів для обох сторін — задача T-004/T-005 (docs/tasks.md). До її виконання
  TypeScript-типи живуть у `ui/src/ipc/types.ts` і мають збігатися з цим файлом.
- Ручна розсинхронізація заборонена: зміна контракту = зміна цього файлу + PR з областю `contracts`.
- Breaking-зміна = коміт `feat(contracts)!: …` (docs/repository.md §8).
