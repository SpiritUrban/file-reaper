/**
 * Екран категорії — сітка кандидатів (docs/ui.md §5).
 * Каркас: рядок детектора + зона сітки; фільтр-чипси TopBar звужують
 * кандидатів через store/filters (T-107). Віртуалізована сітка —
 * T-101/T-115, позначення — T-116, дублікати групами — T-126.
 */

import { useMemo } from "react";

import { VirtualCandidateGrid } from "@/components/VirtualCandidateGrid";
import { categoryTitle } from "@/store/categories";
import {
  applyCandidateFilters,
  hasActiveFilters,
  useCandidateFilters,
} from "@/store/filters";
import type { Candidate, CategoryId } from "@/ipc/types";

interface CategoryScreenProps {
  categoryId: CategoryId;
}

/** Джерело кандидатів підключить category.window (T-115); поки порожньо. */
const NO_CANDIDATES: Candidate[] = [];

export function CategoryScreen({ categoryId }: CategoryScreenProps) {
  const title = categoryTitle(categoryId);
  const filters = useCandidateFilters(categoryId);
  const candidates = NO_CANDIDATES;
  const visible = useMemo(
    () => applyCandidateFilters(candidates, filters),
    [candidates, filters],
  );
  const filtered = hasActiveFilters(filters);

  return (
    <div className="flex h-full flex-col">
      {/* Рядок детектора: пояснення правила + редаговані пороги (T-115) */}
      <div className="flex h-8 shrink-0 items-center gap-2 border-b border-line px-3 text-xs text-ink-dim">
        <span>Детектор:</span>
        <span className="font-mono text-ink-faint">
          правило категорії «{title}» — T-115
        </span>
        {filtered ? (
          <span className="text-ink-faint">
            · фільтри активні: {visible.length} з {candidates.length}
          </span>
        ) : null}
      </div>

      {/* category.window підключить записи у T-115; grid уже bounded за DOM. */}
      <div className="min-h-0 flex-1">
        <VirtualCandidateGrid
          candidates={visible}
          emptyTitle={
            filtered && candidates.length > 0
              ? "Жодного збігу з фільтрами"
              : `Сітка кандидатів: ${title}`
          }
        />
      </div>
    </div>
  );
}