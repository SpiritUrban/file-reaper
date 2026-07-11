/**
 * Пошук у сітці кандидатів (docs/ui.md §2, T-109): infix по path.
 * Стан — per-категорія (як фільтри T-107); пошук комбінується з фільтрами
 * через AND у CategoryScreen. Хоткей `/` (T-103) фокусує інпут;
 * Escape очищує і закриває; real-time без затримки.
 */

import { useSyncExternalStore } from "react";

import type { CategoryId } from "@/ipc/types";

export interface SearchState {
  query: string;
  active: boolean;
}

const EMPTY_SEARCH: SearchState = { query: "", active: false };

type SearchByCategory = Readonly<Partial<Record<CategoryId, SearchState>>>;

class SearchStore {
  private state: SearchByCategory = {};
  private readonly listeners = new Set<() => void>();

  getSnapshot = (): SearchByCategory => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getSearch(category: CategoryId | null): SearchState {
    return (category && this.state[category]) || EMPTY_SEARCH;
  }

  setQuery(category: CategoryId, query: string): void {
    const current = this.state[category] ?? EMPTY_SEARCH;
    if (current.query === query) return;
    this.state = {
      ...this.state,
      [category]: { ...current, query },
    };
    this.notify();
  }

  setActive(category: CategoryId, active: boolean): void {
    const current = this.state[category] ?? EMPTY_SEARCH;
    if (current.active === active) return;
    this.state = {
      ...this.state,
      [category]: { ...current, active },
    };
    this.notify();
  }

  clear(category: CategoryId): void {
    if (!this.state[category]) return;
    const next = { ...this.state };
    delete next[category];
    this.state = next;
    this.notify();
  }

  private notify(): void {
    for (const listener of this.listeners) listener();
  }
}

export const searchStore = new SearchStore();

export function useSearchState(category: CategoryId | null): SearchState {
  const all = useSyncExternalStore(
    searchStore.subscribe,
    searchStore.getSnapshot,
    searchStore.getSnapshot,
  );
  return (category && all[category]) || EMPTY_SEARCH;
}

/**
 * Пошук infix по шляху (регістронезалежний).
 * Без активного пошуку повертає вхідний масив без копії.
 */
export function applySearchQuery<T extends { path: string }>(
  items: T[],
  query: string,
): T[] {
  if (!query) return items;
  const needle = query.toLowerCase();
  return items.filter((item) => item.path.toLowerCase().includes(needle));
}
