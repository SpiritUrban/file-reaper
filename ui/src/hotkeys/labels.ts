/**
 * Людські підписи дій/контекстів гарячих клавіш і форматування комбінацій
 * для переглядача/редактора в Налаштуваннях — T-153 (ui.md §9.4, §11).
 */

import type { HotkeyAction, HotkeyContext } from "./index";

/** Формулювання — зі зведення ui.md §11. */
export const HOTKEY_ACTION_LABELS: Record<HotkeyAction, string> = {
  navigate_left: "Навігація сіткою: ліворуч",
  navigate_right: "Навігація сіткою: праворуч",
  navigate_up: "Навігація сіткою: вгору",
  navigate_down: "Навігація сіткою: вниз",
  mark_toggle: "Позначити / зняти позначення",
  mark_range: "Позначити діапазон",
  mark_all: "Позначити все",
  keep: "Keep — залишити й прибрати з кандидатів",
  details: "Панель деталей",
  reap_confirm: "REAP → екран підтвердження",
  category_previous: "Попередня категорія",
  category_next: "Наступна категорія",
  live_preview: "Live Preview mode",
  arm_action_1: "Озброїти дію 1",
  arm_action_2: "Озброїти дію 2",
  arm_action_3: "Озброїти дію 3",
  arm_action_4: "Озброїти дію 4",
  arm_action_5: "Озброїти дію 5",
  dismiss: "Закрити панель / розрядити дію",
  search: "Пошук серед кандидатів",
  toggle_sidebar: "Згорнути/розгорнути Sidebar",
  zoom_out: "Масштаб плиток: менше",
  zoom_in: "Масштаб плиток: більше",
  restore: "Відновити файл",
};

export const HOTKEY_CONTEXT_LABELS: Record<HotkeyContext, string> = {
  global: "всюди",
  grid: "сітка",
  details: "деталі",
  live_preview: "Live Preview",
  quarantine: "Quarantine",
};

/** Гліфи для KeyboardEvent.code без Key/Digit-префіксів. */
const KEY_GLYPHS: Record<string, string> = {
  ArrowLeft: "←",
  ArrowRight: "→",
  ArrowUp: "↑",
  ArrowDown: "↓",
  Escape: "Esc",
  Slash: "/",
  BracketLeft: "[",
  BracketRight: "]",
  Minus: "-",
  Equal: "=",
  Comma: ",",
  Period: ".",
  Semicolon: ";",
  Quote: "'",
  Backquote: "`",
  Backslash: "\\",
};

/** «Ctrl+ArrowUp» → «Ctrl+↑», «KeyK» → «K», «Digit1» → «1». */
export function formatChord(chord: string): string {
  return chord
    .split("+")
    .map((part) => {
      const glyph = KEY_GLYPHS[part];
      if (glyph) return glyph;
      if (/^Key[A-Z]$/.test(part)) return part.slice(3);
      if (/^Digit\d$/.test(part)) return part.slice(5);
      if (/^Numpad\d$/.test(part)) return `Num ${part.slice(6)}`;
      return part;
    })
    .join("+");
}
