/**
 * Live Preview Mode — двомоніторний режим тріажу (docs/ui.md §10, T-139).
 *
 * Тут — лише СТАН розкладки: увімкнено/вимкнено + позиція перетяжної межі
 * (частка ширини, віддана лівій зоні-сітці). Наведення=превью (T-140) і
 * озброєні дії (T-142+) — окремими задачами.
 *
 * Геометрія переживає перезапуск: це суто UI-стан вікна, не доменний, тож
 * зберігається в `localStorage`, а НЕ в `settings.json` Core (T-090) — Core
 * не має й не повинен мати поняття про геометрію зон інтерфейсу (той самий
 * принцип шару, що й будь-який інший клієнтський стор).
 */

import { useSyncExternalStore } from "react";

const STORAGE_KEY = "trashradar.livePreview";

/** Межі частки лівої зони — жодна зі зон не колапсує в нуль. */
export const MIN_LEFT_RATIO = 0.15;
export const MAX_LEFT_RATIO = 0.85;
const DEFAULT_LEFT_RATIO = 0.5;

export interface LivePreviewState {
  enabled: boolean;
  /** Частка ширини спліт-зони для лівої (сітка); решта — правій (превью). */
  leftRatio: number;
}

function clampRatio(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_LEFT_RATIO;
  return Math.min(MAX_LEFT_RATIO, Math.max(MIN_LEFT_RATIO, value));
}

function load(): LivePreviewState {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<LivePreviewState>;
      return {
        enabled: parsed.enabled === true,
        leftRatio: clampRatio(parsed.leftRatio ?? DEFAULT_LEFT_RATIO),
      };
    }
  } catch {
    // Приватний режим або зіпсований запис — тихо падаємо на дефолти.
  }
  return { enabled: false, leftRatio: DEFAULT_LEFT_RATIO };
}

class LivePreviewStore {
  // Одна незмінна ланка стану — замінюється цілком на кожну зміну, тож
  // getSnapshot стабільний для useSyncExternalStore (без зайвих ре-рендерів).
  private state: LivePreviewState = load();
  private readonly listeners = new Set<() => void>();

  getSnapshot = (): LivePreviewState => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  toggle(): void {
    this.commit({ ...this.state, enabled: !this.state.enabled });
  }

  setEnabled(enabled: boolean): void {
    if (this.state.enabled === enabled) return;
    this.commit({ ...this.state, enabled });
  }

  setLeftRatio(ratio: number): void {
    const leftRatio = clampRatio(ratio);
    if (leftRatio === this.state.leftRatio) return;
    this.commit({ ...this.state, leftRatio });
  }

  private commit(next: LivePreviewState): void {
    this.state = next;
    this.persist();
    for (const listener of this.listeners) listener();
  }

  private persist(): void {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(this.state));
    } catch {
      // Не критично: втратимо збереження геометрії, але UI лишається живим.
    }
  }
}

export const livePreviewStore = new LivePreviewStore();

export function useLivePreview(): LivePreviewState {
  return useSyncExternalStore(
    livePreviewStore.subscribe,
    livePreviewStore.getSnapshot,
    livePreviewStore.getSnapshot,
  );
}
