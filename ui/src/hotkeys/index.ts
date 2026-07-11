/** Typed configurable hotkey registry — docs/ui.md §11 (T-103). */

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

  constructor(bindings: readonly HotkeyBinding[] = DEFAULT_HOTKEYS) {
    this.bindings = bindings.map(normalizeBinding);
    assertNoConflicts(this.bindings);
  }

  list(): readonly HotkeyBinding[] {
    return this.bindings;
  }

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

function chordFromEvent(event: KeyboardEvent): string {
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