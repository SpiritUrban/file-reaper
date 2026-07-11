import { useState, useEffect } from "react";
import { command } from "@/ipc/client";
import type { CategoryId, CandidatePreview } from "@/ipc/types";

export interface CategoryWindow {
  categoryId: CategoryId;
  topCandidates: CandidatePreview[];
}

/**
 * Запит топ-кандидатів категорії для мініпревью у Cleanup Summary (T-111).
 * Повертає 4–6 найбільших файлів категорії.
 */
export async function fetchCategoryTopCandidates(
  categoryId: CategoryId,
  limit = 4,
): Promise<CandidatePreview[]> {
  try {
    const result = await command<CategoryWindow>("category.top_candidates", {
      payload: { categoryId, limit },
    });
    return result.topCandidates;
  } catch (error) {
    console.warn(`Failed to fetch top candidates for ${categoryId}:`, error);
    return [];
  }
}

/**
 * Хук для запиту топ-кандидатів категорії (мініпревью).
 */
export function useCategoryTopCandidates(
  categoryId: CategoryId | undefined,
  limit = 4,
) {
  const [candidates, setCandidates] = useState<CandidatePreview[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!categoryId) return;

    setLoading(true);
    fetchCategoryTopCandidates(categoryId, limit)
      .then(setCandidates)
      .finally(() => setLoading(false));
  }, [categoryId, limit]);

  return { candidates, loading };
}
