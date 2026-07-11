/**
 * Статичний довідник категорій-детекторів: назви, іконки, порядок за
 * замовчуванням. Це НЕ дані — живі обсяги приходять подіями Core
 * (cleanup.total_updated, T-055) і сортують Sidebar за вагою.
 */

import type { CategoryId } from "@/ipc/types";

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
};

export function categoryRule(id: CategoryId): string {
  return CATEGORY_RULES[id] ?? "";
}
