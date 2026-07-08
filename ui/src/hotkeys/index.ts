/**
 * Реєстр гарячих клавіш — зведення docs/ui.md §11.
 *
 * Каркас T-001: константи оголошені для єдності термінології.
 * Система контекстів, реєстрація і конфігурованість — T-103.
 */

export const HOTKEYS = {
  navigate: ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"],
  mark: " ",
  keep: "k",
  details: "Enter",
  reap: "Ctrl+Enter",
  categoryPrev: "Ctrl+ArrowUp",
  categoryNext: "Ctrl+ArrowDown",
  livePreview: "p",
  armAction: ["1", "2", "3", "4", "5"],
  disarm: "Escape",
  search: "/",
  toggleSidebar: "[",
  zoomOut: "-",
  zoomIn: "=",
  restore: "r",
} as const;
