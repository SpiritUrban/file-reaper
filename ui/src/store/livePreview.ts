/**
 * Live Preview Mode — двомоніторний режим тріажу (docs/ui.md §10, T-139).
 *
 * Тут — UI-стан і налаштування режиму: увімкнено/вимкнено, позиція перетяжної
 * межі (частка ширини лівої зони), таймаут авторозрядження озброєної
 * деструктивної дії (T-144) і перемикач «приховати опрацьовані» (T-147 /
 * §10.6). Наведення=превью (T-140), озброєні дії (T-142+) — в інших сторах.
 *
 * Усе це переживає перезапуск: суто UI-стан/налаштування взаємодії, не
 * доменні, тож зберігаються в `localStorage`, а НЕ в `settings.json` Core
 * (T-090) — Core не має й не повинен мати поняття про геометрію зон чи
 * таймінги озброєної дії інтерфейсу (той самий принцип шару, що й геометрія
 * T-139). Екран налаштувань (E18) згодом прив'яже контрол до `disarmTimeoutSec`.
 */

import { useSyncExternalStore } from "react";

const STORAGE_KEY = "trashradar.livePreview";

/** Межі частки лівої зони — жодна зі зон не колапсує в нуль. */
export const MIN_LEFT_RATIO = 0.15;
export const MAX_LEFT_RATIO = 0.85;
const DEFAULT_LEFT_RATIO = 0.5;

/** Авторозрядження озброєної деструктивної дії (§10.3): дефолт 60 с. */
export const DEFAULT_DISARM_TIMEOUT_SEC = 60;
export const MIN_DISARM_TIMEOUT_SEC = 5;
export const MAX_DISARM_TIMEOUT_SEC = 600;

export interface LivePreviewState {
  enabled: boolean;
  /** Частка ширини спліт-зони для лівої (сітка); решта — правій (превью). */
  leftRatio: number;
  /** Секунди бездіяльності курсора до авторозрядження reap (T-144, §10.3). */
  disarmTimeoutSec: number;
  /**
   * T-147 / §10.6: коли false (дефолт) — опрацьовані (keep / marked) лишаються
   * на місці затемненими, сітка не стрибає; коли true — ховаються з сітки.
   */
  hideProcessed: boolean;
}

function clampRatio(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_LEFT_RATIO;
  return Math.min(MAX_LEFT_RATIO, Math.max(MIN_LEFT_RATIO, value));
}

function clampTimeout(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_DISARM_TIMEOUT_SEC;
  return Math.min(MAX_DISARM_TIMEOUT_SEC, Math.max(MIN_DISARM_TIMEOUT_SEC, value));
}

function load(): LivePreviewState {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<LivePreviewState>;
      return {
        enabled: parsed.enabled === true,
        leftRatio: clampRatio(parsed.leftRatio ?? DEFAULT_LEFT_RATIO),
        disarmTimeoutSec: clampTimeout(
          parsed.disarmTimeoutSec ?? DEFAULT_DISARM_TIMEOUT_SEC,
        ),
        // Дефолт false: сітка не стрибає після keep/reap (DoD T-147).
        hideProcessed: parsed.hideProcessed === true,
      };
    }
  } catch {
    // Приватний режим або зіпсований запис — тихо падаємо на дефолти.
  }
  return {
    enabled: false,
    leftRatio: DEFAULT_LEFT_RATIO,
    disarmTimeoutSec: DEFAULT_DISARM_TIMEOUT_SEC,
    hideProcessed: false,
  };
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

  setDisarmTimeoutSec(seconds: number): void {
    const disarmTimeoutSec = clampTimeout(seconds);
    if (disarmTimeoutSec === this.state.disarmTimeoutSec) return;
    this.commit({ ...this.state, disarmTimeoutSec });
  }

  /** T-147: показати/сховати опрацьовані (keep + marked) у сітці Live Preview. */
  setHideProcessed(hideProcessed: boolean): void {
    if (this.state.hideProcessed === hideProcessed) return;
    this.commit({ ...this.state, hideProcessed });
  }

  toggleHideProcessed(): void {
    this.setHideProcessed(!this.state.hideProcessed);
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
