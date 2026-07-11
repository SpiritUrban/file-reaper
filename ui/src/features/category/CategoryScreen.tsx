/**
 * Екран категорії — сітка кандидатів (docs/ui.md §5).
 * Каркас: рядок детектора + порожня зона сітки. Віртуалізована сітка —
 * T-101/T-115, позначення — T-116, дублікати групами — T-126.
 */

import { VirtualCandidateGrid } from "@/components/VirtualCandidateGrid";
import { categoryTitle } from "@/store/categories";
import type { CategoryId } from "@/ipc/types";

interface CategoryScreenProps {
  categoryId: CategoryId;
}

export function CategoryScreen({ categoryId }: CategoryScreenProps) {
  const title = categoryTitle(categoryId);

  return (
    <div className="flex h-full flex-col">
      {/* Рядок детектора: пояснення правила + редаговані пороги (T-115) */}
      <div className="flex h-8 shrink-0 items-center gap-2 border-b border-line px-3 text-xs text-ink-dim">
        <span>Детектор:</span>
        <span className="font-mono text-ink-faint">
          правило категорії «{title}» — T-115
        </span>
      </div>

      {/* category.window підключить записи у T-114; grid уже bounded за DOM. */}
      <div className="min-h-0 flex-1">
        <VirtualCandidateGrid
          candidates={[]}
          emptyTitle={`Сітка кандидатів: ${title}`}
        />
      </div>
    </div>
  );
}