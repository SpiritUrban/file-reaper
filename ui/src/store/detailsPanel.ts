/**
 * Панель деталей (T-123, docs/ui.md §6): немодальна колонка праворуч —
 * сітка лишається видимою й керованою. Один спільний стор на застосунок
 * (як selectionStore/keepStore/gridDensityStore) — панель живе в AppLayout,
 * а відкриває/оновлює її активний CategoryScreen.
 *
 * Вміст (велике превью зі скрабом — T-124; блок метаданих і дії — T-125)
 * навмисно не тут: цей стор відповідає лише за «яка плитка показана й чи
 * відкрита панель узагалі».
 */

import { useSyncExternalStore } from "react";

import type { Candidate } from "@/ipc/types";

class DetailsPanelStore {
  private candidate: Candidate | null = null;
  private readonly listeners = new Set<() => void>();

  getSnapshot = (): Candidate | null => this.candidate;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  isOpen(): boolean {
    return this.candidate !== null;
  }

  /** Відкрити на кандидаті або (якщо вже відкрита) підмінити вміст. */
  open(candidate: Candidate): void {
    this.candidate = candidate;
    this.publish();
  }

  close(): void {
    if (this.candidate === null) return;
    this.candidate = null;
    this.publish();
  }

  private publish(): void {
    for (const listener of this.listeners) listener();
  }
}

export const detailsPanelStore = new DetailsPanelStore();

export function useDetailsPanelCandidate(): Candidate | null {
  return useSyncExternalStore(
    detailsPanelStore.subscribe,
    detailsPanelStore.getSnapshot,
    detailsPanelStore.getSnapshot,
  );
}
