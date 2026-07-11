import { useState, useEffect } from "react";
import { command } from "@/ipc/client";
import type { Candidate, CategoryId, CandidatePreview } from "@/ipc/types";

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
 * Запит УСІХ кандидатів категорії для позначення всередину (T-112).
 * Повертає масив ID та розмірів всіх файлів категорії.
 */
export async function fetchCategoryAllCandidates(
  categoryId: CategoryId,
): Promise<Array<{ id: number; sizeBytes: number }>> {
  try {
    const candidates = await command<CandidatePreview[]>(
      "category.all_candidates",
      { payload: categoryId },
    );
    return candidates.map((c) => ({ id: Number(c.id), sizeBytes: c.sizeBytes }));
  } catch (error) {
    console.warn(`Failed to fetch all candidates for ${categoryId}:`, error);
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

/**
 * Запит повної сітки кандидатів категорії (T-115): усі поля Candidate,
 * Keep приховано, сортовано за розміром спаданням.
 */
export async function fetchCategoryWindow(
  categoryId: CategoryId,
): Promise<Candidate[]> {
  try {
    return await command<Candidate[]>("category.window", {
      payload: { categoryId },
    });
  } catch (error) {
    console.warn(`Failed to fetch category window for ${categoryId}:`, error);
    return [];
  }
}

/**
 * Хук сітки категорії. `refreshKey` (напр. об'єкт settings) провокує
 * рефетч без рескану, коли Core перерахував категорію з індексу
 * (T-093/T-115: зміна порога детектора → нова сітка тим самим кліком).
 */
export function useCategoryWindow(
  categoryId: CategoryId,
  refreshKey: unknown,
): { candidates: Candidate[]; loading: boolean } {
  const [candidates, setCandidates] = useState<Candidate[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    fetchCategoryWindow(categoryId)
      .then((result) => {
        if (!cancelled) setCandidates(result);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [categoryId, refreshKey]);

  return { candidates, loading };
}
