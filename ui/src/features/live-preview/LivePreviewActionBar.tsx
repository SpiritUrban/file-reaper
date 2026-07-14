/**
 * Панель озброєної дії Live Preview (docs/ui.md §10.3/§10.5/§10.6):
 * зверху лівої зони. Набір дій залежить від екрана:
 * - категорії (T-142): reap · keep · move · open · none;
 * - Quarantine (T-148): restore · purge · none.
 *
 * T-144: 1–5 / Esc / авторозрядження деструктивної (reap або purge).
 * T-147: «Приховати опрацьовані» — лише на категоріях (не на карантині).
 */

import { useEffect } from "react";
import { useLocation } from "react-router-dom";

import type { HotkeyActionEventDetail } from "@/hotkeys";
import {
  ARMED_ACTIONS,
  QUARANTINE_ARMED_ACTIONS,
  armedActionStore,
  isActionInPalette,
  isDestructiveArmed,
  useArmedAction,
  type ArmedActionMeta,
} from "@/store/armedAction";
import { livePreviewStore, useLivePreview } from "@/store/livePreview";

const ARM_ACTION_PREFIX = "arm_action_";

export function LivePreviewActionBar() {
  const armed = useArmedAction();
  const { disarmTimeoutSec, hideProcessed } = useLivePreview();
  const { pathname } = useLocation();
  const isQuarantine = pathname === "/quarantine";
  const palette: readonly ArmedActionMeta[] = isQuarantine
    ? QUARANTINE_ARMED_ACTIONS
    : ARMED_ACTIONS;

  // T-148: при вході/виході з Quarantine скинути дію, якщо вона не з
  // поточного набору (інакше клік зробив би «reap» на карантинній плитці).
  useEffect(() => {
    if (!isActionInPalette(armed, palette)) {
      armedActionStore.disarm();
    }
    // лише зміна екрана / палітри — не при кожному set дії
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isQuarantine]);

  // T-144: клавіші 1–5 озброюють відповідну дію палітри; Esc розряджає.
  useEffect(() => {
    const onHotkey = (event: Event) => {
      const { action } = (event as CustomEvent<HotkeyActionEventDetail>).detail;
      if (action === "dismiss") {
        armedActionStore.disarm();
        return;
      }
      if (action.startsWith(ARM_ACTION_PREFIX)) {
        const digit = Number(action.slice(ARM_ACTION_PREFIX.length));
        const meta = palette.find((item) => item.digit === digit);
        if (meta) armedActionStore.set(meta.id);
      }
    };
    window.addEventListener("trashradar:hotkey", onHotkey);
    return () => window.removeEventListener("trashradar:hotkey", onHotkey);
  }, [palette]);

  // T-144: reap (категорії) і purge (карантин) — деструктивні, авторозряд.
  useEffect(() => {
    if (!isDestructiveArmed(armed)) return;
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
        {palette.map((meta) => {
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

      {!isQuarantine ? (
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
      ) : (
        <span className="ml-auto text-xs text-ink-faint">
          Live Preview · карантин
        </span>
      )}
    </div>
  );
}
