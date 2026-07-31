/**
 * Метадані продукту й автора — одне місце на весь UI (розділ 7 брифу
 * Стадії 2, обов'язковий мінімум №2).
 *
 * Версія тут НЕ дублюється навмисне (правило 18): її віддає бандл через
 * `getVersion()` з @tauri-apps/api/app. Захардкоджена версія в UI розходиться
 * з інсталятором тихо й помітна лише в скарзі користувача.
 */

export const PRODUCT_METADATA = {
  name: "TrashRadar",
  author: "Vitaliy Dyachuk",
  /** Особистий хаб: про автора, його продукти й послуги. */
  authorUrl: "https://spiriturban.github.io/",
  authorGithubUrl: "https://github.com/SpiritUrban",
  repositoryUrl: "https://github.com/SpiritUrban/file-reaper",
  releasesUrl: "https://github.com/SpiritUrban/file-reaper/releases",
  siteUrl: "https://spiriturban.github.io/file-reaper/",
  license: "MIT",
  copyright: "© 2026 Vitaliy Dyachuk",
} as const;
