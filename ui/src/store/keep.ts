/**
 * Сесійний список Keep (T-117, docs/ui.md §11 клавіша K).
 * Keep ховає файл з УСІХ категорій сесії (architecture.md §6.3, §3.4) —
 * тому тримається в одному спільному сторі, а не локальному стані екрана:
 * файл, позначений Keep у категорії A, миттєво зникає і з уже завантаженого
 * списку категорії B (multi-hit), без очікування на новий фетч.
 * Рішення персистентне на боці Core (`candidate.keep`, T-057) — переживає
 * перезапуск; цей стор лише дає миттєву оптимістичну реакцію (architecture.md §14).
 */

import { useSyncExternalStore } from "react";

import { command } from "@/ipc/client";
import { selectionStore } from "@/store/selection";
import type { Candidate } from "@/ipc/types";

class KeepStore {
  private kept = new Set<number>();
  private readonly listeners = new Set<() => void>();

  getSnapshot = (): ReadonlySet<number> => this.kept;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  isKept(id: number): boolean {
    return this.kept.has(id);
  }

  keep(id: number): void {
    if (this.kept.has(id)) return;
    this.kept = new Set(this.kept).add(id);
    this.publish();
  }

  unkeep(id: number): void {
    if (!this.kept.has(id)) return;
    const next = new Set(this.kept);
    next.delete(id);
    this.kept = next;
    this.publish();
  }

  private publish(): void {
    for (const listener of this.listeners) listener();
  }
}

export const keepStore = new KeepStore();

/** Реактивна підписка: ре-рендер при будь-якій зміні Keep-списку. */
export function useKeptIds(): ReadonlySet<number> {
  return useSyncExternalStore(keepStore.subscribe, keepStore.getSnapshot, keepStore.getSnapshot);
}

/**
 * Keep кандидата (K): оптимістично ховає з усіх категорій сесії одразу,
 * знімає Reap-позначення (Keep і Marked взаємовиключні), потім синхронізує
 * з Core; відмова Core — відкат оптимістичного стану.
 */
export async function keepCandidate(candidate: Pick<Candidate, "id">): Promise<void> {
  keepStore.keep(candidate.id);
  selectionStore.unmark(candidate.id);
  try {
    await command("candidate.keep", { payload: { candidateId: candidate.id } });
  } catch (error) {
    keepStore.unkeep(candidate.id);
    console.warn(`Failed to keep candidate ${candidate.id}:`, error);
  }
}
