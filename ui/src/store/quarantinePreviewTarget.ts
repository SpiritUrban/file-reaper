/**
 * Ціль превью Live Preview на екрані Quarantine (T-148, docs/ui.md §10.6).
 *
 * Окремо від `previewTargetStore` (Candidate / HotIndex): файл у карантині
 * уже переміщений (T-088), `preview.large` за candidateId не спрацює.
 * Права зона читає `quarantine.thumbnail` (T-130) для сурогатного шляху.
 *
 * Ефемерний — як `previewTargetStore`, без localStorage.
 */

import { useSyncExternalStore } from "react";

import type { QuarantineEntry } from "@/ipc/types";

class QuarantinePreviewTargetStore {
  private entry: QuarantineEntry | null = null;
  private readonly listeners = new Set<() => void>();

  getSnapshot = (): QuarantineEntry | null => this.entry;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  set(entry: QuarantineEntry | null): void {
    if (this.entry?.id === entry?.id) return;
    this.entry = entry;
    for (const listener of this.listeners) listener();
  }

  clear(): void {
    this.set(null);
  }
}

export const quarantinePreviewTargetStore = new QuarantinePreviewTargetStore();

export function useQuarantinePreviewTarget(): QuarantineEntry | null {
  return useSyncExternalStore(
    quarantinePreviewTargetStore.subscribe,
    quarantinePreviewTargetStore.getSnapshot,
    quarantinePreviewTargetStore.getSnapshot,
  );
}
