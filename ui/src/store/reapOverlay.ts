/**
 * Оверлей підтвердження REAP (T-135, docs/ui.md §8): «яка кнопка/хоткей
 * відкрив, чи оверлей взагалі відкритий» — той самий патерн, що й
 * `detailsPanelStore` (T-123): один спільний стор на застосунок, відкриває
 * TopBar (кнопка REAP) або глобальний хоткей `Ctrl+Enter` (AppLayout),
 * рендерить сам оверлей `features/reap-flow/ReapConfirmOverlay`.
 */

import { useSyncExternalStore } from "react";

class ReapOverlayStore {
  private open_ = false;
  private readonly listeners = new Set<() => void>();

  getSnapshot = (): boolean => this.open_;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  open(): void {
    if (this.open_) return;
    this.open_ = true;
    this.publish();
  }

  close(): void {
    if (!this.open_) return;
    this.open_ = false;
    this.publish();
  }

  private publish(): void {
    for (const listener of this.listeners) listener();
  }
}

export const reapOverlayStore = new ReapOverlayStore();

export function useReapOverlayOpen(): boolean {
  return useSyncExternalStore(
    reapOverlayStore.subscribe,
    reapOverlayStore.getSnapshot,
    reapOverlayStore.getSnapshot,
  );
}
