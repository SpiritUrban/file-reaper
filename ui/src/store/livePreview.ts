/**
 * Live Preview Mode — двомоніторний / однодисплейний режим тріажу
 * (docs/ui.md §10, T-139 / T-149).
 *
 * Тут — UI-стан: увімкнено/вимкнено, позиції перетяжної межі **окремо** для
 * одного й двох моніторів (T-149), таймаут авторозрядження (T-144),
 * «приховати опрацьовані» (T-147). Наведення=превью, озброєні дії — в інших
 * сторах.
 *
 * Усе в `localStorage`, не в Core settings.json (T-090): геометрія зон — UI.
 */

import { useSyncExternalStore } from "react";

import {
  ARMED_ACTIONS,
  armedActionStore,
  type ArmedAction,
} from "./armedAction";

const STORAGE_KEY = "trashradar.livePreview";

/** Межі частки лівої зони — жодна зі зон не колапсує в нуль. */
export const MIN_LEFT_RATIO = 0.15;
export const MAX_LEFT_RATIO = 0.85;

/**
 * T-149 / §10.6: на одному моніторі права зона ~55% → ліва 45%.
 * Два монітори (вікно розтягнуте): симетричний 50/50 як стартова точка.
 */
export const DEFAULT_LEFT_RATIO_SINGLE = 0.45;
export const DEFAULT_LEFT_RATIO_DUAL = 0.5;
/** Права зона на одному моніторі (1 − DEFAULT_LEFT_RATIO_SINGLE). */
export const DEFAULT_RIGHT_RATIO_SINGLE = 1 - DEFAULT_LEFT_RATIO_SINGLE; // 0.55

/** Авторозрядження озброєної деструктивної дії (§10.3): дефолт 60 с. */
export const DEFAULT_DISARM_TIMEOUT_SEC = 60;
export const MIN_DISARM_TIMEOUT_SEC = 5;
export const MAX_DISARM_TIMEOUT_SEC = 600;

/**
 * T-150 / architecture.md §9: дефолтна озброєна дія при вході в Live Preview
 * («дія кліку за замовч.» з екрана Налаштувань, ui.md §9.4). Дефолт `none` —
 * режим стартує в безпечному «тільки перегляд», доки користувач не обере інше.
 */
export const DEFAULT_ARMED_ACTION: ArmedAction = "none";

/** Дозволені значення — лише категорійна палітра (карантин має свій набір). */
function sanitizeDefaultAction(value: unknown): ArmedAction {
  return ARMED_ACTIONS.some((meta) => meta.id === value)
    ? (value as ArmedAction)
    : DEFAULT_ARMED_ACTION;
}

export interface LivePreviewState {
  enabled: boolean;
  /**
   * Активна частка лівої зони (обчислюється з single/dual + детекції).
   * Зручний зріз для UI; пишеться через setLeftRatio у відповідний слот.
   */
  leftRatio: number;
  /** T-149: межа, коли вікно на одному моніторі (дефолт 0.45 → right 55%). */
  leftRatioSingle: number;
  /** T-149: межа, коли вікно розтягнуте на кілька екранів (дефолт 0.5). */
  leftRatioDual: number;
  /** true, якщо зараз детектимо однодисплейний режим. */
  singleDisplay: boolean;
  /** Секунди бездіяльності курсора до авторозрядження reap/purge (T-144). */
  disarmTimeoutSec: number;
  /** T-150: дія, що озброюється при вході в Live Preview (ui.md §9.4). */
  defaultArmedAction: ArmedAction;
  /**
   * T-147 / §10.6: false (дефолт) — опрацьовані лишаються затемненими;
   * true — ховаються з сітки.
   */
  hideProcessed: boolean;
}

function clampRatio(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_LEFT_RATIO_SINGLE;
  return Math.min(MAX_LEFT_RATIO, Math.max(MIN_LEFT_RATIO, value));
}

function clampTimeout(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_DISARM_TIMEOUT_SEC;
  return Math.min(MAX_DISARM_TIMEOUT_SEC, Math.max(MIN_DISARM_TIMEOUT_SEC, value));
}

/**
 * Евіристика «одне вікно на одному моніторі» (T-149).
 * Якщо outerWidth помітно більше за availWidth — вікно, ймовірно, розтягнуте
 * на два екрани. WebView2/Chromium не дає надійного списку моніторів без
 * Tauri API; для DoD «окремо запам'ятати пропорцію» цього достатньо.
 */
export function detectSingleDisplay(): boolean {
  if (typeof window === "undefined" || typeof screen === "undefined") return true;
  const outer = window.outerWidth || window.innerWidth;
  const avail = screen.availWidth || screen.width;
  if (!outer || !avail) return true;
  // 15% запас: максимізоване на одному екрані + chrome; 2× екран — dual.
  return outer <= avail * 1.15;
}

function pickLeftRatio(
  single: boolean,
  leftRatioSingle: number,
  leftRatioDual: number,
): number {
  return single ? leftRatioSingle : leftRatioDual;
}

interface PersistedShape {
  enabled?: boolean;
  /** Legacy T-139: одна спільна межа — мігруємо в dual, single лишаємо дефолт. */
  leftRatio?: number;
  leftRatioSingle?: number;
  leftRatioDual?: number;
  disarmTimeoutSec?: number;
  defaultArmedAction?: string;
  hideProcessed?: boolean;
}

function load(): LivePreviewState {
  const singleDisplay = detectSingleDisplay();
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as PersistedShape;
      // T-149: окремі слоти; legacy leftRatio → dual (двомоніторний спліт
      // був основним сценарієм T-139), single — дефолт 45% якщо не збережено.
      const leftRatioDual = clampRatio(
        parsed.leftRatioDual ?? parsed.leftRatio ?? DEFAULT_LEFT_RATIO_DUAL,
      );
      const leftRatioSingle = clampRatio(
        parsed.leftRatioSingle ?? DEFAULT_LEFT_RATIO_SINGLE,
      );
      return {
        enabled: parsed.enabled === true,
        leftRatioSingle,
        leftRatioDual,
        singleDisplay,
        leftRatio: pickLeftRatio(singleDisplay, leftRatioSingle, leftRatioDual),
        disarmTimeoutSec: clampTimeout(
          parsed.disarmTimeoutSec ?? DEFAULT_DISARM_TIMEOUT_SEC,
        ),
        defaultArmedAction: sanitizeDefaultAction(parsed.defaultArmedAction),
        hideProcessed: parsed.hideProcessed === true,
      };
    }
  } catch {
    // Приватний режим або зіпсований запис — дефолти.
  }
  return {
    enabled: false,
    leftRatioSingle: DEFAULT_LEFT_RATIO_SINGLE,
    leftRatioDual: DEFAULT_LEFT_RATIO_DUAL,
    singleDisplay,
    leftRatio: pickLeftRatio(
      singleDisplay,
      DEFAULT_LEFT_RATIO_SINGLE,
      DEFAULT_LEFT_RATIO_DUAL,
    ),
    disarmTimeoutSec: DEFAULT_DISARM_TIMEOUT_SEC,
    defaultArmedAction: DEFAULT_ARMED_ACTION,
    hideProcessed: false,
  };
}

class LivePreviewStore {
  private state: LivePreviewState = load();
  private readonly listeners = new Set<() => void>();
  private displayListenerAttached = false;

  getSnapshot = (): LivePreviewState => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    this.ensureDisplayListener();
    return () => this.listeners.delete(listener);
  };

  /**
   * Підписка на resize: якщо вікно перетягли з одного на два монітори
   * (або навпаки) — підставляємо відповідну збережену пропорцію.
   */
  private ensureDisplayListener(): void {
    if (this.displayListenerAttached || typeof window === "undefined") return;
    this.displayListenerAttached = true;
    const onResize = () => this.refreshDisplayMode();
    window.addEventListener("resize", onResize);
  }

  /** Оновити singleDisplay і active leftRatio без зміни збережених слотів. */
  refreshDisplayMode(): void {
    const singleDisplay = detectSingleDisplay();
    const leftRatio = pickLeftRatio(
      singleDisplay,
      this.state.leftRatioSingle,
      this.state.leftRatioDual,
    );
    if (
      singleDisplay === this.state.singleDisplay &&
      leftRatio === this.state.leftRatio
    ) {
      return;
    }
    // Не persist: singleDisplay — runtime-детекція, слоти вже на диску.
    this.state = { ...this.state, singleDisplay, leftRatio };
    for (const listener of this.listeners) listener();
  }

  toggle(): void {
    this.setEnabled(!this.state.enabled);
  }

  setEnabled(enabled: boolean): void {
    if (this.state.enabled === enabled) return;
    this.refreshDisplayMode();
    this.commit({ ...this.state, enabled });
    // T-150: вхід у режим озброює дію за замовчуванням з Налаштувань
    // (ui.md §9.4). Невалідну для поточного екрана дію (напр. reap на
    // Quarantine) ActionBar розрядить сам (T-148).
    if (enabled) armedActionStore.set(this.state.defaultArmedAction);
  }

  /**
   * Записати межу в **поточний** слот (single або dual) — DoD T-149
   * «пропорція запам'ятовується окремо».
   */
  setLeftRatio(ratio: number): void {
    const leftRatio = clampRatio(ratio);
    const single = this.state.singleDisplay;
    const next: LivePreviewState = single
      ? {
          ...this.state,
          leftRatioSingle: leftRatio,
          leftRatio,
        }
      : {
          ...this.state,
          leftRatioDual: leftRatio,
          leftRatio,
        };
    if (
      next.leftRatioSingle === this.state.leftRatioSingle &&
      next.leftRatioDual === this.state.leftRatioDual &&
      next.leftRatio === this.state.leftRatio
    ) {
      return;
    }
    this.commit(next);
  }

  setDisarmTimeoutSec(seconds: number): void {
    const disarmTimeoutSec = clampTimeout(seconds);
    if (disarmTimeoutSec === this.state.disarmTimeoutSec) return;
    this.commit({ ...this.state, disarmTimeoutSec });
  }

  /** T-150: «дія кліку за замовч.» з екрана Налаштувань (ui.md §9.4). */
  setDefaultArmedAction(action: ArmedAction): void {
    const defaultArmedAction = sanitizeDefaultAction(action);
    if (defaultArmedAction === this.state.defaultArmedAction) return;
    this.commit({ ...this.state, defaultArmedAction });
    // Гаряче застосування: якщо режим уже активний — переозброїти одразу.
    if (this.state.enabled) armedActionStore.set(defaultArmedAction);
  }

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
      // singleDisplay — не персистимо (runtime); legacy leftRatio не пишемо.
      const payload = {
        enabled: this.state.enabled,
        leftRatioSingle: this.state.leftRatioSingle,
        leftRatioDual: this.state.leftRatioDual,
        disarmTimeoutSec: this.state.disarmTimeoutSec,
        defaultArmedAction: this.state.defaultArmedAction,
        hideProcessed: this.state.hideProcessed,
      };
      localStorage.setItem(STORAGE_KEY, JSON.stringify(payload));
    } catch {
      // Не критично.
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
