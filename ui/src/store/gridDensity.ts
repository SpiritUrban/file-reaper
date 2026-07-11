/**
 * Щільність сітки плиток — три ступені масштабу (docs/ui.md §187, T-118):
 * Compact / Standard / Large, клавіші `-`/`=` (zoom_out/zoom_in, T-103).
 * ОДИН спільний вибір на всю сесію (як Sidebar collapsed) — перемикання
 * категорій не скидає масштаб; позиція фокуса окрема (`focusedId` у
 * CategoryScreen) і від density не залежить, тож не губиться.
 */

import { useSyncExternalStore } from "react";

import type { GridDensity } from "@/components/VirtualCandidateGrid";

const ORDER: readonly GridDensity[] = ["compact", "standard", "large"];

class GridDensityStore {
  private density: GridDensity = "standard";
  private readonly listeners = new Set<() => void>();

  getSnapshot = (): GridDensity => this.density;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  set(density: GridDensity): void {
    if (this.density === density) return;
    this.density = density;
    this.publish();
  }

  /** `=` — крупніше (менше плиток на рядок). */
  zoomIn(): void {
    this.step(1);
  }

  /** `-` — компактніше (більше плиток на рядок). */
  zoomOut(): void {
    this.step(-1);
  }

  private step(delta: number): void {
    const index = ORDER.indexOf(this.density);
    const next = ORDER[Math.min(ORDER.length - 1, Math.max(0, index + delta))];
    if (next) this.set(next);
  }

  private publish(): void {
    for (const listener of this.listeners) listener();
  }
}

export const gridDensityStore = new GridDensityStore();

export function useGridDensity(): GridDensity {
  return useSyncExternalStore(
    gridDensityStore.subscribe,
    gridDensityStore.getSnapshot,
    gridDensityStore.getSnapshot,
  );
}
