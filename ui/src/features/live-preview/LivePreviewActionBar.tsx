/**
 * Панель озброєної дії Live Preview (docs/ui.md §10.3/§10.5, T-142): зверху
 * лівої зони. Клік по кнопці озброює дію; озброєна кнопка підсвічена кольором
 * ролі (червоний reap, зелений keep, accent move/open), тож поточна дія
 * завжди видима й кольорово однозначна. Курсор над плитками — окремо
 * (`armedActionCursor`, застосовує `CategoryScreen`).
 *
 * Тут лише вибір+підсвітка. Клік по плитці = дія й ПКМ = протилежна — T-143;
 * хоткеї 1–5 / Esc / авторозрядження — T-144.
 */

import {
  ARMED_ACTIONS,
  armedActionStore,
  useArmedAction,
} from "@/store/armedAction";

export function LivePreviewActionBar() {
  const armed = useArmedAction();

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
    </div>
  );
}
