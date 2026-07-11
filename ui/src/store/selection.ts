/**
 * Кошик сесії (docs/ui.md §2, T-108): позначені до reap кандидати.
 * ОДИН спільний стор на всю сесію — позначення зберігаються при
 * перемиканні категорій, Reap Bar показує сумарні N · X ГБ звідси.
 * Позначення оптимістичне (architecture.md §14): сітка пише сюди миттєво
 * (інтерактив — T-116), синхронізація з Core через candidate.mark — там само;
 * reap-флоу T-135/T-137 читає і знімає позначення звідси.
 */

import { useSyncExternalStore } from "react";

import type { Candidate } from "@/ipc/types";

export interface MarkedSummary {
  count: number;
  bytes: number;
}

const EMPTY_SUMMARY: MarkedSummary = { count: 0, bytes: 0 };

type MarkedCandidate = Pick<Candidate, "id" | "sizeBytes">;

class SelectionStore {
  /** candidateId → sizeBytes; підсумок ведеться інкрементально O(1). */
  private marked = new Map<number, number>();
  private summary: MarkedSummary = EMPTY_SUMMARY;
  private readonly listeners = new Set<() => void>();

  getSummary = (): MarkedSummary => this.summary;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  isMarked(id: number): boolean {
    return this.marked.has(id);
  }

  markedIds(): number[] {
    return [...this.marked.keys()];
  }

  mark(candidate: MarkedCandidate): void {
    const previous = this.marked.get(candidate.id);
    if (previous === candidate.sizeBytes) return;
    this.marked.set(candidate.id, candidate.sizeBytes);
    this.publish({
      count: this.summary.count + (previous === undefined ? 1 : 0),
      bytes: this.summary.bytes - (previous ?? 0) + candidate.sizeBytes,
    });
  }

  unmark(id: number): void {
    const bytes = this.marked.get(id);
    if (bytes === undefined) return;
    this.marked.delete(id);
    this.publish({
      count: this.summary.count - 1,
      bytes: this.summary.bytes - bytes,
    });
  }

  toggle(candidate: MarkedCandidate): boolean {
    if (this.marked.has(candidate.id)) {
      this.unmark(candidate.id);
      return false;
    }
    this.mark(candidate);
    return true;
  }

  /** Зняти всі позначення (після reap T-138 або скасування). */
  clear(): void {
    if (this.marked.size === 0) return;
    this.marked.clear();
    this.publish(EMPTY_SUMMARY);
  }

  /** Позначити кілька кандидатів (T-112: позначити всю категорію). */
  markMultiple(candidates: MarkedCandidate[]): void {
    let countDelta = 0;
    let bytesDelta = 0;
    for (const candidate of candidates) {
      const previous = this.marked.get(candidate.id);
      if (previous === candidate.sizeBytes) continue;
      this.marked.set(candidate.id, candidate.sizeBytes);
      if (previous === undefined) countDelta += 1;
      bytesDelta += candidate.sizeBytes - (previous ?? 0);
    }
    if (countDelta === 0 && bytesDelta === 0) return;
    this.publish({
      count: this.summary.count + countDelta,
      bytes: this.summary.bytes + bytesDelta,
    });
  }

  private publish(summary: MarkedSummary): void {
    this.summary = summary;
    for (const listener of this.listeners) listener();
  }
}

export const selectionStore = new SelectionStore();

/** Живий підсумок для Reap Bar: «Позначено N · X ГБ». */
export function useMarkedSummary(): MarkedSummary {
  return useSyncExternalStore(
    selectionStore.subscribe,
    selectionStore.getSummary,
    selectionStore.getSummary,
  );
}

/** Публічний API для позначення всіх кандидатів (T-112). */
export function markAllCandidates(candidates: MarkedCandidate[]): void {
  selectionStore.markMultiple(candidates);
}

/** Публічний API для зняття всіх позначень. */
export function clearAllMarked(): void {
  selectionStore.clear();
}
