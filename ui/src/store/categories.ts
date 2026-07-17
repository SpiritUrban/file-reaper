/**
 * Статичний довідник категорій-детекторів: назви, іконки, порядок за
 * замовчуванням. Це НЕ дані — живі обсяги приходять подіями Core
 * (cleanup.total_updated, T-055) і сортують Sidebar за вагою.
 */

import type { AppSettings, CategoryId, CategorySummary } from "@/ipc/types";

export interface CategoryDescriptor {
  id: CategoryId;
  /** Назва у UI (docs/ui.md §1). */
  title: string;
  /** Символ-іконка каркаса; іконографія — разом з T-095. */
  glyph: string;
}

export const CATEGORIES: readonly CategoryDescriptor[] = [
  { id: "forgotten_videos", title: "Старі відео", glyph: "▶" },
  { id: "duplicates", title: "Дублікати", glyph: "⧉" },
  { id: "archives", title: "Архіви", glyph: "▤" },
  { id: "dev_artifacts", title: "Dev-сміття", glyph: "⚙" },
  { id: "temp_files", title: "Тимчасові", glyph: "♻" },
  { id: "app_caches", title: "Кеші", glyph: "▨" },
  { id: "installers", title: "Інсталятори", glyph: "⬇" },
  { id: "large_files", title: "Великі файли", glyph: "◼" },
  { id: "old_files", title: "Старі файли", glyph: "◻" },
  { id: "empty_folders", title: "Порожні папки", glyph: "▱" },
  { id: "sparse_folders", title: "Майже порожні", glyph: "▨" },
  { id: "deep_paths", title: "Глибокі шляхи", glyph: "⋮" },
] as const;

export function categoryTitle(id: CategoryId): string {
  return CATEGORIES.find((c) => c.id === id)?.title ?? id;
}

/** Пояснення правила детектора для рядка над сіткою категорії (T-115). */
const CATEGORY_RULES: Record<CategoryId, string> = {
  large_files: "Файли понад поріг розміру",
  old_files: "Файли з давнім останнім доступом чи зміною понад поріг віку",
  forgotten_videos: "Відео понад поріг розміру і давності доступу",
  archives: "Архіви (zip/rar/7z/…) понад поріг розміру",
  installers: "Інсталятори у теках завантажень; ISO/IMG — будь-де",
  temp_files: "Файли з Temp-локацій реєстру Windows і програм",
  app_caches: "Кеш-каталоги популярних програм (реєстр відомих локацій)",
  dev_artifacts: "node_modules/build/dist/target/obj/Library — за маркерами проєкту",
  duplicates: "Групи файлів з ідентичним вмістом (каскад хешування)",
  empty_folders: "Рекурсивно порожні папки (жодного файла в піддереві) — показано найвищу",
  sparse_folders: "Папки з дуже малою кількістю файлів (поріг у Налаштуваннях)",
  deep_paths: "Папки із задовгим ланцюжком вкладення (поріг глибини у Налаштуваннях)",
};

export function categoryRule(id: CategoryId): string {
  return CATEGORY_RULES[id] ?? "";
}

/**
 * Категорії з перемикачем детектора (T-152) — 5 предикатних детекторів ферми
 * скану (`configured_registry` у core/shell) + `duplicates`: каскад не в фермі,
 * але його enabled читається окремо в `scan_runtime` і вимикає важке хешування
 * (проблемний розділ). Решта (temp/caches/dev) — детектори, ще не підключені до
 * скану: перемикач з'явиться разом з їхнім wiring.
 */
export const TOGGLEABLE_CATEGORIES: readonly CategoryId[] = [
  "large_files",
  "old_files",
  "forgotten_videos",
  "archives",
  "installers",
  "duplicates",
  "temp_files",
  "app_caches",
  "dev_artifacts",
];

/** Детектор категорії увімкнено (відсутність запису/поля = увімкнено). */
export function isCategoryEnabled(
  settings: AppSettings | null,
  id: CategoryId,
): boolean {
  return settings?.detectors?.[id]?.enabled !== false;
}

export interface CategoryRow {
  descriptor: CategoryDescriptor;
  summary: CategorySummary | undefined;
}

/**
 * Порядок Sidebar і Ctrl+↑/↓ (T-105/T-122): непорожні категорії за вагою
 * (спадання байтів), порожні — у каталожному порядку в кінці. Сортування
 * стабільне, тож рівні обсяги не «стрибають» між подіями скану — той самий
 * порядок, що бачить користувач, а не окрема послідовність для клавіатури.
 *
 * T-152: категорії з вимкненим детектором (settings) прибираються зі
 * списку повністю — і з Sidebar, і з навігації Ctrl+↑/↓, і з Summary.
 */
export function categoryRowsByWeight(
  live: CategorySummary[],
  settings: AppSettings | null = null,
): CategoryRow[] {
  const byId = new Map(live.map((summary) => [summary.id, summary]));
  return CATEGORIES.filter((descriptor) =>
    isCategoryEnabled(settings, descriptor.id),
  )
    .map((descriptor, catalogIndex) => ({
      descriptor,
      summary: byId.get(descriptor.id),
      catalogIndex,
    }))
    .sort((left, right) => {
      const leftBytes = left.summary?.totalBytes ?? 0;
      const rightBytes = right.summary?.totalBytes ?? 0;
      if (leftBytes !== rightBytes) return rightBytes - leftBytes;
      return left.catalogIndex - right.catalogIndex;
    })
    .map(({ descriptor, summary }) => ({ descriptor, summary }));
}
