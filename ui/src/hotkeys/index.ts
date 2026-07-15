/**
 * Typed configurable hotkey registry — docs/ui.md §11 (T-103).
 *
 * T-153: перепризначення персистяться в localStorage як відхилення від
 * DEFAULT_HOTKEYS (той самий шар, що й livePreviewStore — поведінка webview,
 * не Core settings.json). Конфліктне перепризначення відхиляється
 * HotkeyConflictError, реєстр і сховище не змінюються.
 */

export type HotkeyContext =
  | "global"
  | "grid"
  | "details"
  | "live_preview"
  | "quarantine";

export type HotkeyAction =
  | "navigate_left"
  | "navigate_right"
  | "navigate_up"
  | "navigate_down"
  | "mark_toggle"
  | "mark_range"
  | "mark_all"
  | "keep"
  | "details"
  | "reap_confirm"
  | "category_previous"
  | "category_next"
  | "live_preview"
  | "arm_action_1"
  | "arm_action_2"
  | "arm_action_3"
  | "arm_action_4"
  | "arm_action_5"
  | "dismiss"
  | "search"
  | "toggle_sidebar"
  | "zoom_out"
  | "zoom_in"
  | "restore";

export interface HotkeyBinding {
  action: HotkeyAction;
  chord: string;
  contexts: readonly HotkeyContext[];
}

export interface HotkeyActionEventDetail {
  action: HotkeyAction;
  chord: string;
  repeat: boolean;
}

export const DEFAULT_HOTKEYS: readonly HotkeyBinding[] = [
  { action: "navigate_left", chord: "ArrowLeft", contexts: ["grid"] },
  { action: "navigate_right", chord: "ArrowRight", contexts: ["grid"] },
  { action: "navigate_up", chord: "ArrowUp", contexts: ["grid"] },
  { action: "navigate_down", chord: "ArrowDown", contexts: ["grid"] },
  { action: "mark_toggle", chord: "Space", contexts: ["grid"] },
  { action: "mark_range", chord: "Shift+Space", contexts: ["grid"] },
  { action: "mark_all", chord: "KeyA", contexts: ["grid"] },
  { action: "keep", chord: "KeyK", contexts: ["grid"] },
  { action: "details", chord: "Enter", contexts: ["grid"] },
  { action: "reap_confirm", chord: "Ctrl+Enter", contexts: ["global"] },
  { action: "category_previous", chord: "Ctrl+ArrowUp", contexts: ["global"] },
  { action: "category_next", chord: "Ctrl+ArrowDown", contexts: ["global"] },
  { action: "live_preview", chord: "KeyP", contexts: ["global"] },
  { action: "arm_action_1", chord: "Digit1", contexts: ["live_preview"] },
  { action: "arm_action_2", chord: "Digit2", contexts: ["live_preview"] },
  { action: "arm_action_3", chord: "Digit3", contexts: ["live_preview"] },
  { action: "arm_action_4", chord: "Digit4", contexts: ["live_preview"] },
  { action: "arm_action_5", chord: "Digit5", contexts: ["live_preview"] },
  { action: "dismiss", chord: "Escape", contexts: ["details", "live_preview"] },
  { action: "search", chord: "Slash", contexts: ["global"] },
  { action: "toggle_sidebar", chord: "BracketLeft", contexts: ["global"] },
  { action: "zoom_out", chord: "Minus", contexts: ["grid"] },
  { action: "zoom_in", chord: "Equal", contexts: ["grid"] },
  { action: "restore", chord: "KeyR", contexts: ["quarantine"] },
] as const;

export class HotkeyConflictError extends Error {
  constructor(
    readonly chord: string,
    readonly first: HotkeyAction,
    readonly second: HotkeyAction,
  ) {
    super(`Hotkey conflict: ${chord} is assigned to ${first} and ${second}.`);
    this.name = "HotkeyConflictError";
  }
}

export class HotkeyRegistry {
  private bindings: HotkeyBinding[];
  private activeContexts = new Set<HotkeyContext>(["global"]);
  private target: Window | null = null;
  private readonly listeners = new Set<() => void>();
  private version = 0;

  constructor(bindings: readonly HotkeyBinding[] = DEFAULT_HOTKEYS) {
    this.bindings = bindings.map(normalizeBinding);
    assertNoConflicts(this.bindings);
  }

  list(): readonly HotkeyBinding[] {
    return this.bindings;
  }

  /** Підписка для React (useSyncExternalStore): rebind/reset нотифікують. */
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  /** Снапшот-лічильник змін: інкрементується на кожен rebind/reset. */
  getVersion = (): number => this.version;

  setActiveContexts(contexts: Iterable<HotkeyContext>): void {
    this.activeContexts = new Set(["global", ...contexts]);
  }

  activate(context: HotkeyContext): void {
    this.activeContexts.add(context);
  }

  deactivate(context: HotkeyContext): void {
    if (context !== "global") this.activeContexts.delete(context);
  }

  /** Runtime configuration for T-153; invalid rebind leaves registry unchanged. */
  rebind(action: HotkeyAction, chord: string): void {
    const normalized = normalizeChord(chord);
    const next = this.bindings.map((binding) =>
      binding.action === action ? { ...binding, chord: normalized } : binding,
    );
    if (!next.some((binding) => binding.action === action)) {
      throw new Error(`Unknown hotkey action: ${action}.`);
    }
    assertNoConflicts(next);
    this.bindings = next;
    this.notify();
  }

  /** Повернути всі комбінації до типових (T-153 «Скинути всі»). */
  reset(bindings: readonly HotkeyBinding[] = DEFAULT_HOTKEYS): void {
    const next = bindings.map(normalizeBinding);
    assertNoConflicts(next);
    this.bindings = next;
    this.notify();
  }

  private notify(): void {
    this.version += 1;
    for (const listener of this.listeners) listener();
  }

  attach(target: Window): () => void {
    if (this.target === target) return () => this.detach();
    this.detach();
    this.target = target;
    target.addEventListener("keydown", this.onKeyDown);
    return () => this.detach();
  }

  detach(): void {
    this.target?.removeEventListener("keydown", this.onKeyDown);
    this.target = null;
  }

  private onKeyDown = (event: KeyboardEvent): void => {
    if (isEditableTarget(event.target) && event.code !== "Escape") return;
    const chord = chordFromEvent(event);
    const binding = this.bindings.find(
      (item) =>
        item.chord === chord &&
        item.contexts.some((context) => this.activeContexts.has(context)),
    );
    if (!binding) return;
    event.preventDefault();
    window.dispatchEvent(
      new CustomEvent<HotkeyActionEventDetail>("trashradar:hotkey", {
        detail: { action: binding.action, chord, repeat: event.repeat },
      }),
    );
  };
}

function normalizeBinding(binding: HotkeyBinding): HotkeyBinding {
  return { ...binding, chord: normalizeChord(binding.chord) };
}

export function normalizeChord(chord: string): string {
  if (chord === " ") return "Space";
  const parts = chord.split("+").map((part) => part.trim()).filter(Boolean);
  const code = parts.pop();
  if (!code) throw new Error("Hotkey chord must contain a key code.");
  const modifiers = new Set(parts.map((part) => part.toLowerCase()));
  const prefix = [
    modifiers.has("ctrl") ? "Ctrl" : null,
    modifiers.has("alt") ? "Alt" : null,
    modifiers.has("shift") ? "Shift" : null,
    modifiers.has("meta") ? "Meta" : null,
  ].filter(Boolean);
  return [...prefix, codeAlias(code)].join("+");
}

function codeAlias(code: string): string {
  const aliases: Record<string, string> = {
    " ": "Space",
    Spacebar: "Space",
    Esc: "Escape",
    "/": "Slash",
    "[": "BracketLeft",
    "-": "Minus",
    "=": "Equal",
  };
  if (aliases[code]) return aliases[code];
  if (/^[a-z]$/i.test(code)) return `Key${code.toUpperCase()}`;
  if (/^[1-5]$/.test(code)) return `Digit${code}`;
  return code;
}

/** Комбінація з живої події клавіатури — для запису в редакторі (T-153). */
export function chordFromEvent(event: KeyboardEvent): string {
  return [
    event.ctrlKey ? "Ctrl" : null,
    event.altKey ? "Alt" : null,
    event.shiftKey ? "Shift" : null,
    event.metaKey ? "Meta" : null,
    event.code,
  ]
    .filter(Boolean)
    .join("+");
}

function contextsOverlap(left: HotkeyBinding, right: HotkeyBinding): boolean {
  return left.contexts.some(
    (context) =>
      right.contexts.includes(context) ||
      context === "global" ||
      right.contexts.includes("global"),
  );
}

function assertNoConflicts(bindings: readonly HotkeyBinding[]): void {
  for (let left = 0; left < bindings.length; left += 1) {
    for (let right = left + 1; right < bindings.length; right += 1) {
      const a = bindings[left];
      const b = bindings[right];
      if (a && b && a.chord === b.chord && contextsOverlap(a, b)) {
        throw new HotkeyConflictError(a.chord, a.action, b.action);
      }
    }
  }
}

function isEditableTarget(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLElement &&
    (target.isContentEditable ||
      target instanceof HTMLInputElement ||
      target instanceof HTMLTextAreaElement ||
      target instanceof HTMLSelectElement)
  );
}

export const hotkeys = new HotkeyRegistry();

// ─── Персистенція перепризначень (T-153) ────────────────────────────────────

const STORAGE_KEY = "trashradar.hotkeys";

/** Лише відхилення від DEFAULT_HOTKEYS: action → chord. */
type StoredOverrides = Partial<Record<HotkeyAction, string>>;

function loadOverrides(): StoredOverrides {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return {};
    const overrides: StoredOverrides = {};
    for (const [action, chord] of Object.entries(parsed)) {
      if (typeof chord === "string") {
        overrides[action as HotkeyAction] = chord;
      }
    }
    return overrides;
  } catch {
    return {};
  }
}

function persistOverrides(overrides: StoredOverrides): void {
  try {
    if (Object.keys(overrides).length === 0) {
      localStorage.removeItem(STORAGE_KEY);
    } else {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(overrides));
    }
  } catch {
    // Приватний режим — редактор працює без збереження між сесіями.
  }
}

/** Типова комбінація дії з DEFAULT_HOTKEYS. */
export function defaultChord(action: HotkeyAction): string {
  const binding = DEFAULT_HOTKEYS.find((item) => item.action === action);
  return binding ? normalizeChord(binding.chord) : "";
}

/**
 * Перепризначити і зберегти. Конфлікт у перетинних контекстах →
 * HotkeyConflictError, реєстр і сховище не змінюються.
 */
export function rebindHotkey(action: HotkeyAction, chord: string): void {
  hotkeys.rebind(action, chord);
  const overrides = loadOverrides();
  const normalized = normalizeChord(chord);
  if (normalized === defaultChord(action)) {
    delete overrides[action];
  } else {
    overrides[action] = normalized;
  }
  persistOverrides(overrides);
}

/** Скинути всі комбінації до типових і очистити сховище. */
export function resetHotkeys(): void {
  hotkeys.reset();
  persistOverrides({});
}

// Відновлення збережених перепризначень на старті. Запис, що став
// невалідним (конфлікт зі зміненими дефолтами, невідома дія), пропускається
// і прибирається зі сховища — реєстр завжди у несуперечливому стані.
(() => {
  const overrides = loadOverrides();
  let dirty = false;
  for (const [action, chord] of Object.entries(overrides)) {
    try {
      hotkeys.rebind(action as HotkeyAction, chord as string);
    } catch {
      delete overrides[action as HotkeyAction];
      dirty = true;
    }
  }
  if (dirty) persistOverrides(overrides);
})();