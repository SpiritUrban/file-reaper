/**
 * Контекстне меню плитки (ПКМ на кандидаті сітки): один сеансовий стор тримає
 * ціль + координати курсора, а єдиний `<TileContextMenu/>` (змонтований в
 * AppLayout) рендерить меню й виконує дії. Так плитки лишаються «тупими» —
 * лише сигналять `open(candidate, x, y)`, без власного меню-DOM у кожній.
 */

import { useSyncExternalStore } from "react";

import type { Candidate } from "@/ipc/types";

export interface TileContextMenuState {
  candidate: Candidate;
  x: number;
  y: number;
}

class TileContextMenuStore {
  private state: TileContextMenuState | null = null;
  private readonly listeners = new Set<() => void>();

  getSnapshot = (): TileContextMenuState | null => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  open(candidate: Candidate, x: number, y: number): void {
    this.state = { candidate, x, y };
    this.publish();
  }

  close(): void {
    if (!this.state) return;
    this.state = null;
    this.publish();
  }

  private publish(): void {
    for (const listener of this.listeners) listener();
  }
}

export const tileContextMenuStore = new TileContextMenuStore();

export function useTileContextMenu(): TileContextMenuState | null {
  return useSyncExternalStore(
    tileContextMenuStore.subscribe,
    tileContextMenuStore.getSnapshot,
    tileContextMenuStore.getSnapshot,
  );
}
