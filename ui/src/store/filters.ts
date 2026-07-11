/**
 * Фільтр-чипси категорії (docs/ui.md §2, T-107): звуження ВЖЕ знайдених
 * кандидатів за розміром/типом/віком/диском — не пошук по диску.
 * Стан — per-категорія (кожна категорія пам'ятає свої чипси); чипси
 * комбінуються за AND. TopBar редагує стан, CategoryScreen застосовує
 * `applyCandidateFilters` до свого вікна кандидатів.
 */

import { useSyncExternalStore } from "react";

import type { Candidate, CategoryId, FileKind } from "@/ipc/types";

export interface CandidateFilters {
  /** Мінімальний розмір, байти. */
  minSizeBytes: number | null;
  /** Мінімальний вік останнього доступу, дні. */
  minAgeDays: number | null;
  kind: FileKind | null;
  /** Літера тому з двокрапкою, напр. "C:". */
  volume: string | null;
}

export const EMPTY_FILTERS: CandidateFilters = {
  minSizeBytes: null,
  minAgeDays: null,
  kind: null,
  volume: null,
};

/** Пресети чипа «Розмір» (ui.md §2: випадайки з пресетами). */
export const SIZE_PRESETS = [
  { label: ">100 МБ", bytes: 100 * 1024 ** 2 },
  { label: ">1 ГБ", bytes: 1024 ** 3 },
  { label: ">5 ГБ", bytes: 5 * 1024 ** 3 },
] as const;

/** Пресети чипа «Вік» (за останнім доступом). */
export const AGE_PRESETS = [
  { label: "1+ міс без доступу", days: 30 },
  { label: "6+ міс без доступу", days: 180 },
  { label: "1+ рік без доступу", days: 365 },
] as const;

/** Людські назви типів для чипа «Тип». */
export const KIND_LABELS: Record<FileKind, string> = {
  video: "Відео",
  image: "Зображення",
  audio: "Аудіо",
  archive: "Архіви",
  installer: "Інсталятори",
  disk_image: "Образи дисків",
  document: "Документи",
  other: "Інше",
};

export type FiltersByCategory = Readonly<
  Partial<Record<CategoryId, CandidateFilters>>
>;

class CandidateFilterStore {
  private state: FiltersByCategory = {};
  private readonly listeners = new Set<() => void>();

  getSnapshot = (): FiltersByCategory => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  /** Встановити/змінити один вимір; null — зняти фільтр (хрестик чипа). */
  patch(category: CategoryId, patch: Partial<CandidateFilters>): void {
    const current = this.state[category] ?? EMPTY_FILTERS;
    this.state = { ...this.state, [category]: { ...current, ...patch } };
    for (const listener of this.listeners) listener();
  }

  /** Зняти всі фільтри категорії. */
  reset(category: CategoryId): void {
    if (!this.state[category]) return;
    const next = { ...this.state };
    delete next[category];
    this.state = next;
    for (const listener of this.listeners) listener();
  }
}

export const candidateFilterStore = new CandidateFilterStore();

export function useCandidateFilters(
  category: CategoryId | null,
): CandidateFilters {
  const all = useSyncExternalStore(
    candidateFilterStore.subscribe,
    candidateFilterStore.getSnapshot,
    candidateFilterStore.getSnapshot,
  );
  return (category && all[category]) || EMPTY_FILTERS;
}

export function hasActiveFilters(filters: CandidateFilters): boolean {
  return (
    filters.minSizeBytes !== null ||
    filters.minAgeDays !== null ||
    filters.kind !== null ||
    filters.volume !== null
  );
}

const DAY_MS = 86_400_000;

/**
 * Чиста AND-комбінація активних чипів. Без активних фільтрів повертає
 * вхідний масив без копії — сітка не перебудовується дарма.
 * Невалідна дата доступу при активному чипі «Вік» = не доведено, що файл
 * старий → кандидат ховається (детерміновано, без хибних збігів).
 */
export function applyCandidateFilters(
  candidates: Candidate[],
  filters: CandidateFilters,
  nowMs: number = Date.now(),
): Candidate[] {
  if (!hasActiveFilters(filters)) return candidates;
  const volumePrefix = filters.volume?.toUpperCase() ?? null;
  const latestAccessMs =
    filters.minAgeDays === null ? null : nowMs - filters.minAgeDays * DAY_MS;
  return candidates.filter((candidate) => {
    if (
      filters.minSizeBytes !== null &&
      candidate.sizeBytes < filters.minSizeBytes
    ) {
      return false;
    }
    if (filters.kind !== null && candidate.kind !== filters.kind) return false;
    if (
      volumePrefix !== null &&
      !candidate.path.toUpperCase().startsWith(volumePrefix)
    ) {
      return false;
    }
    if (latestAccessMs !== null) {
      const accessedMs = Date.parse(candidate.lastAccessAt);
      if (!Number.isFinite(accessedMs) || accessedMs > latestAccessMs) {
        return false;
      }
    }
    return true;
  });
}
