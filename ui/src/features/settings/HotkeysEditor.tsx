/**
 * Переглядач/редактор гарячих клавіш — T-153 (ui.md §9.4 «таблиця гарячих
 * клавіш (переглянути/змінити)», зведення §11).
 *
 * Список — живий стан HotkeyRegistry (T-103), не статичні дефолти.
 * Перепризначення: [Змінити] → запис наступної натиснутої комбінації
 * (Esc скасовує запис); конфлікт у перетинних контекстах відхиляється
 * реєстром (HotkeyConflictError) з поясненням, яка дія вже тримає комбінацію.
 * Валідні зміни застосовуються одразу і персистяться у localStorage
 * (rebindHotkey); відхилення від типових позначені й скидаються — порядково
 * або всі разом.
 */

import { useEffect, useState, useSyncExternalStore } from "react";

import {
  chordFromEvent,
  defaultChord,
  HotkeyConflictError,
  hotkeys,
  rebindHotkey,
  resetHotkeys,
  type HotkeyAction,
} from "@/hotkeys";
import {
  formatChord,
  HOTKEY_ACTION_LABELS,
  HOTKEY_CONTEXT_LABELS,
} from "@/hotkeys/labels";

/** Модифікатори самі по собі — не комбінація: чекаємо основну клавішу. */
const MODIFIER_CODES = new Set([
  "ControlLeft",
  "ControlRight",
  "ShiftLeft",
  "ShiftRight",
  "AltLeft",
  "AltRight",
  "MetaLeft",
  "MetaRight",
]);

function conflictMessage(error: unknown, rebound: HotkeyAction): string {
  if (error instanceof HotkeyConflictError) {
    const other = error.first === rebound ? error.second : error.first;
    return `Конфлікт: ${formatChord(error.chord)} вже призначено на «${HOTKEY_ACTION_LABELS[other]}».`;
  }
  return error instanceof Error ? error.message : String(error);
}

export function HotkeysEditor() {
  // Реєстр нотифікує про rebind/reset — таблиця завжди показує актуальний стан.
  useSyncExternalStore(hotkeys.subscribe, hotkeys.getVersion);
  const bindings = hotkeys.list();

  const [capturing, setCapturing] = useState<HotkeyAction | null>(null);
  const [error, setError] = useState<{
    action: HotkeyAction;
    message: string;
  } | null>(null);

  // Запис комбінації: слухач у capture-фазі на document перехоплює подію до
  // реєстру (T-103 слухає bubble на window) — під час запису дії не спрацьовують.
  useEffect(() => {
    if (capturing === null) return;
    const onKeyDown = (event: KeyboardEvent) => {
      event.preventDefault();
      event.stopPropagation();
      if (MODIFIER_CODES.has(event.code)) return;
      if (event.code === "Escape") {
        setCapturing(null);
        return;
      }
      try {
        rebindHotkey(capturing, chordFromEvent(event));
        setError(null);
      } catch (cause) {
        setError({ action: capturing, message: conflictMessage(cause, capturing) });
      }
      setCapturing(null);
    };
    document.addEventListener("keydown", onKeyDown, { capture: true });
    return () =>
      document.removeEventListener("keydown", onKeyDown, { capture: true });
  }, [capturing]);

  const customized = bindings.filter(
    (binding) => binding.chord !== defaultChord(binding.action),
  );

  return (
    <div className="flex flex-col gap-2">
      <table className="w-full max-w-2xl border-collapse">
        <thead>
          <tr className="text-left text-ink-faint">
            <th className="py-1 pr-3 font-normal">Дія</th>
            <th className="py-1 pr-3 font-normal">Комбінація</th>
            <th className="py-1 pr-3 font-normal">Контекст</th>
            <th className="py-1 font-normal" />
          </tr>
        </thead>
        <tbody className="divide-y divide-line/60">
          {bindings.map((binding) => {
            const isCapturing = capturing === binding.action;
            const isCustom = binding.chord !== defaultChord(binding.action);
            return (
              <tr key={binding.action}>
                <td className="py-1 pr-3 text-ink-dim">
                  {HOTKEY_ACTION_LABELS[binding.action]}
                </td>
                <td className="py-1 pr-3">
                  <span
                    className={`rounded border px-1.5 font-mono ${
                      isCustom
                        ? "border-accent/60 text-ink"
                        : "border-line text-ink-dim"
                    }`}
                    title={isCustom ? "Змінено (типова: " +
                        formatChord(defaultChord(binding.action)) +
                        ")"
                      : "Типова комбінація"}
                  >
                    {formatChord(binding.chord)}
                  </span>
                </td>
                <td className="py-1 pr-3 text-ink-faint">
                  {binding.contexts
                    .map((context) => HOTKEY_CONTEXT_LABELS[context])
                    .join(", ")}
                </td>
                <td className="py-1">
                  <span className="flex items-center gap-1.5">
                    <button
                      type="button"
                      onClick={() =>
                        setCapturing(isCapturing ? null : binding.action)
                      }
                      className={`rounded border px-1.5 py-0.5 ${
                        isCapturing
                          ? "border-accent text-accent"
                          : "border-line bg-panel-2 text-ink-dim hover:border-accent hover:text-ink"
                      }`}
                    >
                      {isCapturing ? "натисніть комбінацію… (Esc — скасувати)" : "Змінити"}
                    </button>
                    {isCustom ? (
                      <button
                        type="button"
                        title={`Повернути типову: ${formatChord(defaultChord(binding.action))}`}
                        onClick={() => {
                          rebindHotkey(
                            binding.action,
                            defaultChord(binding.action),
                          );
                          setError(null);
                        }}
                        className="rounded border border-line bg-panel-2 px-1.5 py-0.5 text-ink-dim hover:border-accent hover:text-ink"
                      >
                        ↺
                      </button>
                    ) : null}
                  </span>
                  {error && error.action === binding.action ? (
                    <div className="pt-1 text-quarantine" role="alert">
                      {error.message}
                    </div>
                  ) : null}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>

      <div className="flex items-center gap-3">
        {customized.length > 0 ? (
          <button
            type="button"
            onClick={() => {
              resetHotkeys();
              setError(null);
            }}
            className="rounded border border-line bg-panel-2 px-2 py-0.5 text-ink hover:border-accent"
          >
            Скинути всі до типових ({customized.length} змін.)
          </button>
        ) : null}
        <span className="text-ink-faint">
          Зміни діють одразу і зберігаються між сесіями. Esc зарезервовано
          (скасовує запис комбінації).
        </span>
      </div>
    </div>
  );
}
