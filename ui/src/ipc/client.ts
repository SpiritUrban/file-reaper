/**
 * Клієнт IPC — єдина точка звернення UI до Core.
 *
 * Правило шару (docs/repository.md §6): компоненти не викликають
 * Tauri API напряму — лише через цей модуль. Повна реалізація
 * (типізовані payload-и, помилки Core, відписки) — T-004/T-005.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { CommandName, EventName } from "./types";

/** UI запущено всередині Tauri (а не у браузері `npm run dev`). */
export function isTauri(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

/**
 * Виклик команди Core. Неблокуючий за контрактом
 * (docs/architecture.md §1.2): довгі операції повертають підтвердження,
 * результат приходить подіями.
 */
export async function command<TResult>(
  name: CommandName,
  payload?: Record<string, unknown>,
): Promise<TResult> {
  if (!isTauri()) {
    throw new Error(
      `IPC недоступний поза Tauri (команда «${name}»). Запустіть через cargo tauri dev.`,
    );
  }
  // Tauri-команди іменуються snake_case; контракт — namespace.dot.
  return invoke<TResult>(name.replace(/\./g, "_"), payload);
}

/** Підписка на подію Core. Повертає функцію відписки. */
export async function subscribe<TPayload>(
  name: EventName,
  handler: (payload: TPayload) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) {
    return () => undefined;
  }
  return listen<TPayload>(name, (event) => handler(event.payload));
}
