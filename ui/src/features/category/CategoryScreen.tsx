/**
 * Екран категорії — сітка кандидатів (docs/ui.md §5).
 * Каркас: рядок детектора + порожня зона сітки. Віртуалізована сітка —
 * T-101/T-115, позначення — T-116, дублікати групами — T-126.
 */

import { useParams } from "react-router-dom";

import { EmptyState } from "@/components/EmptyState";
import { categoryTitle } from "@/store/categories";
import type { CategoryId } from "@/ipc/types";

export function CategoryScreen() {
  const { categoryId } = useParams();
  const title = categoryTitle(categoryId as CategoryId);

  return (
    <div className="flex h-full flex-col">
      {/* Рядок детектора: пояснення правила + редаговані пороги (T-115) */}
      <div className="flex h-8 shrink-0 items-center gap-2 border-b border-line px-3 text-xs text-ink-dim">
        <span>Детектор:</span>
        <span className="font-mono text-ink-faint">правило категорії «{title}» — T-115</span>
      </div>

      {/* Зона віртуалізованої сітки плиток (T-101) */}
      <div className="min-h-0 flex-1">
        <EmptyState title={`Сітка кандидатів: ${title}`} taskRef="T-101, T-115…T-122" />
      </div>
    </div>
  );
}
