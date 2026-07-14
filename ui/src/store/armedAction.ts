/**
 * Озброєна дія Live Preview (T-142, docs/ui.md §10.3): у двомоніторному
 * режимі користувач один раз обирає дію на панелі, і далі клік по плитці
 * виконує саме її (виконання — T-143). Тут — лише СТАН вибору + метадані
 * (колір/іконка) для панелі й курсора.
 *
 * Ефемерний, сесійний (як `previewTargetStore`): дефолт — `none` («Нічого»,
 * тільки перегляд). Перемикання клавішами 1–5 / Esc-розрядження /
 * авторозрядження деструктивної за таймаутом — T-144.
 */

import { useSyncExternalStore } from "react";

export type ArmedAction = "reap" | "keep" | "move" | "open" | "none";

export interface ArmedActionMeta {
  id: ArmedAction;
  /** Підпис на кнопці панелі. */
  label: string;
  /** Іконка дії — на кнопці й у курсорі над плитками. */
  glyph: string;
  /** Літерал кольору теми (T-095) для SVG-курсора; null — нейтральний. */
  hex: string | null;
  /** Класи Tailwind підсвітки озброєної кнопки (колір ролі, §10.3). */
  armedClass: string;
  /** Цифра швидкого вибору 1–5 (розкладка §10.5; хоткеї — T-144). */
  digit: number;
}

// Порядок і склад — за ui.md §10.3: reap · keep · move · open · none.
// Чистий reap-червоний лише на деструктивній дії (інваріант T-095); keep —
// зелений; move/open — нейтральний accent; none — без кольору.
export const ARMED_ACTIONS: readonly ArmedActionMeta[] = [
  {
    id: "reap",
    label: "Позначити",
    glyph: "╳",
    hex: "#e5484d",
    armedClass: "border-reap bg-reap/15 text-reap",
    digit: 1,
  },
  {
    id: "keep",
    label: "Залишити",
    glyph: "✓",
    hex: "#46a758",
    armedClass: "border-keep bg-keep/15 text-keep",
    digit: 2,
  },
  {
    id: "move",
    label: "Перемістити",
    glyph: "↪",
    hex: "#5b9dd9",
    armedClass: "border-accent bg-accent/15 text-accent",
    digit: 3,
  },
  {
    id: "open",
    label: "Відкрити папку",
    glyph: "⌖",
    hex: "#5b9dd9",
    armedClass: "border-accent bg-accent/15 text-accent",
    digit: 4,
  },
  {
    id: "none",
    label: "Нічого",
    glyph: "",
    hex: null,
    armedClass: "border-ink-dim/50 bg-panel-2 text-ink",
    digit: 5,
  },
];

export function armedActionMeta(action: ArmedAction): ArmedActionMeta {
  return ARMED_ACTIONS.find((meta) => meta.id === action) ?? ARMED_ACTIONS[4]!;
}

/**
 * Протилежна дія для ПКМ (T-143, §10.3): reap↔keep — дзеркальна пара. Для
 * move/open «протилежне» = безпечне `keep` (лишити), бо в осі
 * деструктивне↔безпечне keep — універсальний безпечний вибір. `none` не має
 * протилежного (тільки перегляд).
 */
export function oppositeAction(action: ArmedAction): ArmedAction {
  switch (action) {
    case "reap":
      return "keep";
    case "keep":
      return "reap";
    case "move":
    case "open":
      return "keep";
    case "none":
      return "none";
  }
}

/**
 * CSS-курсор із іконкою дії в її кольорі (§10.3: «курсор над плитками
 * змінює вигляд на іконку дії»). `none` → undefined (звичайний курсор).
 * Хотспот у центрі 28×28 (14,14).
 */
export function armedActionCursor(action: ArmedAction): string | undefined {
  const meta = armedActionMeta(action);
  if (meta.hex === null || meta.glyph === "") return undefined;
  const svg =
    `<svg xmlns='http://www.w3.org/2000/svg' width='28' height='28'>` +
    `<circle cx='14' cy='14' r='11' fill='${meta.hex}' stroke='white' stroke-width='1.5'/>` +
    `<text x='14' y='19' font-size='15' text-anchor='middle' fill='white' ` +
    `font-family='sans-serif'>${meta.glyph}</text></svg>`;
  return `url("data:image/svg+xml,${encodeURIComponent(svg)}") 14 14, pointer`;
}

class ArmedActionStore {
  private action: ArmedAction = "none";
  private readonly listeners = new Set<() => void>();

  getSnapshot = (): ArmedAction => this.action;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  set(action: ArmedAction): void {
    if (this.action === action) return;
    this.action = action;
    for (const listener of this.listeners) listener();
  }

  /** Розрядити до «Нічого» (Esc — T-144; тут для повноти API). */
  disarm(): void {
    this.set("none");
  }
}

export const armedActionStore = new ArmedActionStore();

export function useArmedAction(): ArmedAction {
  return useSyncExternalStore(
    armedActionStore.subscribe,
    armedActionStore.getSnapshot,
    armedActionStore.getSnapshot,
  );
}
