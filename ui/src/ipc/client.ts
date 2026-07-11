/**
 * Клієнт IPC — єдина точка звернення UI до Core.
 *
 * Правило шару (docs/repository.md §6): компоненти не викликають
 * Tauri API напряму — лише через цей модуль.
 *
 * Помилки (T-007): будь-яке відхилення команди нормалізується у
 * CoreIpcError зі стабільним кодом. Компоненти розгалужуються за
 * error.code і показують error.message як є.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { CommandName, CoreErrorCode, CoreErrorPayload, EventName } from "./types";

/**
 * Коди, які може мати помилка на боці UI: конверт Core плюс два
 * суто клієнтські стани (не перетинають IPC, тому їх немає в domain).
 */
export type IpcErrorCode = CoreErrorCode | "ipc_unavailable" | "unknown";

/** Типізована помилка виклику Core. `message` готовий до показу. */
export class CoreIpcError extends Error {
  readonly code: IpcErrorCode;

  constructor(code: IpcErrorCode, message: string) {
    super(message);
    this.name = "CoreIpcError";
    this.code = code;
  }
}

function isCoreErrorPayload(value: unknown): value is CoreErrorPayload {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as CoreErrorPayload).code === "string" &&
    typeof (value as CoreErrorPayload).message === "string"
  );
}

/** Нормалізує будь-яку помилку IPC у CoreIpcError. */
export function toCoreIpcError(raw: unknown): CoreIpcError {
  if (raw instanceof CoreIpcError) return raw;
  if (isCoreErrorPayload(raw)) return new CoreIpcError(raw.code, raw.message);

  let fallback: string | undefined;
  if (raw instanceof Error) {
    fallback = raw.message;
  } else if (typeof raw === "string") {
    fallback = raw;
  } else {
    try {
      fallback = JSON.stringify(raw);
    } catch {
      fallback = undefined;
    }
  }

  return new CoreIpcError(
    "unknown",
    fallback || "Невідома помилка взаємодії з Core.",
  );
}

/** Єдиний готовий до показу текст для catch-блоків компонентів. */
export function ipcErrorMessage(raw: unknown): string {
  return toCoreIpcError(raw).message;
}

/** UI запущено всередині Tauri (а не у браузері `npm run dev`). */
export function isTauri(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

/**
 * Виклик команди Core. Неблокуючий за контрактом
 * (docs/architecture.md §1.2): довгі операції повертають підтвердження,
 * результат приходить подіями. Кидає ЛИШЕ CoreIpcError.
 */
export async function command<TResult>(
  name: CommandName,
  payload?: Record<string, unknown>,
): Promise<TResult> {
  if (!isTauri()) {
    throw new CoreIpcError(
      "ipc_unavailable",
      `IPC недоступний поза Tauri (команда «${name}»). Запустіть через tauri dev.`,
    );
  }
  try {
    // Tauri-команди іменуються snake_case; контракт — namespace.dot.
    return await invoke<TResult>(name.replace(/\./g, "_"), payload);
  } catch (raw) {
    throw toCoreIpcError(raw);
  }
}

/** Підписка на подію Core. Повертає функцію відписки. */
export async function subscribe<TPayload>(
  name: EventName,
  handler: (payload: TPayload) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) {
    return () => undefined;
  }
  try {
    // Tauri забороняє крапки в іменах подій: на дроті app.test → app:test.
    // Дзеркальне перетворення — core/shell/src/events.rs (wire_name).
    return await listen<TPayload>(name.replace(/\./g, ":"), (event) =>
      handler(event.payload),
    );
  } catch (raw) {
    throw toCoreIpcError(raw);
  }
}
