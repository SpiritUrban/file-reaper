/**
 * Ціль превью Live Preview (T-140, docs/ui.md §10.2): кандидат «під курсором»
 * (наведення) або у фокусі клавіатури, чиє велике превью показує права зона.
 *
 * Ефемерний — НЕ персиститься (на відміну від геометрії `livePreviewStore`,
 * T-139): міняється буквально щовзмах миші, тримати таке в localStorage було б
 * і зайвим I/O, і безглуздям. Спільний стор (той самий патерн, що й
 * `detailsPanelStore`) — плитки в глибині `CategoryScreen` ставлять ціль, а
 * права зона (`LivePreviewPane` в `AppLayout`) її читає.
 */

import { useSyncExternalStore } from "react";

import type { Candidate } from "@/ipc/types";

class PreviewTargetStore {
  private candidate: Candidate | null = null;
  private readonly listeners = new Set<() => void>();

  getSnapshot = (): Candidate | null => this.candidate;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  set(candidate: Candidate | null): void {
    // Наведення на ту саму плитку (той самий id) не смикає підписників —
    // велике превью в правій зоні не перезапитує той самий файл.
    if (this.candidate?.id === candidate?.id) return;
    this.candidate = candidate;
    for (const listener of this.listeners) listener();
  }

  clear(): void {
    this.set(null);
  }
}

export const previewTargetStore = new PreviewTargetStore();

export function usePreviewTarget(): Candidate | null {
  return useSyncExternalStore(
    previewTargetStore.subscribe,
    previewTargetStore.getSnapshot,
    previewTargetStore.getSnapshot,
  );
}
