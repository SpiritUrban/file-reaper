/**
 * Панель озброєної дії Live Preview (docs/ui.md §10.3/§10.5, T-142/T-144/T-147):
 * зверху лівої зони. Клік по кнопці озброює дію; озброєна кнопка підсвічена
 * кольором ролі (червоний reap, зелений keep, accent move/open), тож поточна
 * дія завжди видима й кольорово однозначна. Курсор над плитками — окремо
 * (`armedActionCursor`, застосовує `CategoryScreen`).
 *
 * T-144: швидке перемикання дій клавішами 1–5 (хоткеї `arm_action_1..5`,
 * контекст `live_preview`), Esc розряджає до «Нічого» (`dismiss`), а озброєна
 * ДЕСТРУКТИВНА дія (reap) авторозряджається після N с бездіяльності курсора
 * (N — `disarmTimeoutSec` з `livePreviewStore`, §10.3/§9). Слухачі живуть лише
 * поки панель змонтована (тобто поки режим увімкнено), тож самоприбираються.
 *
 * T-147 / §10.6: перемикач «Приховати опрацьовані» — keep/marked лишаються
 * затемненими на місці (сітка не стрибає), доки перемикач їх не сховає.
 *
 * Клік по плитці = дія й ПКМ = протилежна — T-143 (у `CategoryScreen`).
 */

import { useEffect } from "react";

import type { HotkeyActionEventDetail } from "@/hotkeys";
import {
  ARMED_ACTIONS,
  armedActionStore,
  useArmedAction,
} from "@/store/armedAction";
import { livePreviewStore, useLivePreview } from "@/store/livePreview";

const ARM_ACTION_PREFIX = "arm_action_";

export function LivePreviewActionBar() {
  const armed = useArmedAction();
  const { disarmTimeoutSec, hideProcessed } = useLivePreview();

  // T-144: клавіші 1–5 озброюють відповідну дію; Esc розряджає.
  useEffect(() => {
    const onHotkey = (event: Event) => {
      const { action } = (event as CustomEvent<HotkeyActionEventDetail>).detail;
      if (action === "dismiss") {
        armedActionStore.disarm();
        return;
      }
      if (action.startsWith(ARM_ACTION_PREFIX)) {
        const digit = Number(action.slice(ARM_ACTION_PREFIX.length));
        const meta = ARMED_ACTIONS.find((item) => item.digit === digit);
        if (meta) armedActionStore.set(meta.id);
      }
    };
    window.addEventListener("trashradar:hotkey", onHotkey);
    return () => window.removeEventListener("trashradar:hotkey", onHotkey);
  }, []);

  // T-144: захист від випадкових серій — озброєний reap (єдина деструктивна
  // дія; keep/move/open безпечні) сам розряджається після N с без руху миші.
  useEffect(() => {
    if (armed !== "reap") return;
    let timer: ReturnType<typeof setTimeout>;
    const bump = () => {
      clearTimeout(timer);
      timer = setTimeout(() => armedActionStore.disarm(), disarmTimeoutSec * 1000);
    };
    bump();
    window.addEventListener("mousemove", bump);
    return () => {
      clearTimeout(timer);
      window.removeEventListener("mousemove", bump);
    };
  }, [armed, disarmTimeoutSec]);

  return (
    <div className="flex h-10 shrink-0 items-center gap-2 border-b border-line bg-panel px-3">
      <span className="text-xs font-semibold uppercase tracking-wide text-ink-dim">
        Дія
      </span>
      <div className="flex items-center gap-1">
        {ARMED_ACTIONS.map((meta) => {
          const active = armed === meta.id;
          return (
            <button
              key={meta.id}
              type="button"
              onClick={() => armedActionStore.set(meta.id)}
              aria-pressed={active}
              title={`${meta.label}${meta.digit ? ` (${meta.digit})` : ""}`}
              className={`flex items-center gap-1.5 rounded border px-2 py-1 text-xs font-medium transition-colors ${
                active
                  ? meta.armedClass
                  : "border-line text-ink-dim hover:bg-panel-2 hover:text-ink"
              }`}
            >
              {meta.glyph ? (
                <span aria-hidden="true" className="text-sm leading-none">
                  {meta.glyph}
                </span>
              ) : null}
              <span>{meta.label}</span>
            </button>
          );
        })}
      </div>

      {/* T-147: сітка не стрибає після keep/reap, доки не увімкнути приховування. */}
      <label
        className="ml-auto flex cursor-pointer select-none items-center gap-1.5 text-xs text-ink-dim hover:text-ink"
        title="Опрацьовані (позначені / залишені) лишаються затемненими на місці; увімкніть, щоб сховати їх зі сітки"
      >
        <input
          type="checkbox"
          checked={hideProcessed}
          onChange={() => livePreviewStore.toggleHideProcessed()}
          className="accent-[var(--color-accent)]"
          aria-label="Приховати опрацьовані плитки"
        />
        <span>Приховати опрацьовані</span>
      </label>
    </div>
  );
}
