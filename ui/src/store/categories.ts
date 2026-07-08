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
