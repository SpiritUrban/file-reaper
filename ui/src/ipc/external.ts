/**
 * Відкриття зовнішнього посилання в системному браузері.
 *
 * У webview навігація за http-посиланням завантажила б сайт ПОВЕРХ
 * застосунку (CSP це блокує, і користувач бачить порожнє вікно), тому
 * посилання завжди йдуть через плагін opener. Поза Tauri (npm run dev:ui)
 * лишається звичайний window.open — щоб сторінки автора перевірялися і в
 * браузері.
 */

import { openUrl } from "@tauri-apps/plugin-opener";

import { isTauri } from "./client";

export function openExternal(url: string): void {
  if (!isTauri()) {
    window.open(url, "_blank", "noopener,noreferrer");
    return;
  }
  void openUrl(url).catch((error) =>
    console.warn("Не вдалося відкрити посилання:", url, error),
  );
}
