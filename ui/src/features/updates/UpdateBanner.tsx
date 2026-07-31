/**
 * Банер оновлення (правило 28 брифу Стадії 2).
 *
 * З'являється лише коли оновлення справді доступне, і живе смужкою під
 * TopBar — не модалкою поверх роботи. Це єдиний банер, який брифу дозволено
 * (розділ 7 забороняє переривати роботу заради промо; повідомлення про нову
 * версію — не промо, але й воно не має права красти фокус).
 */

import { useUpdater } from "./useUpdater";

export function UpdateBanner() {
  const { state, install, dismiss } = useUpdater();

  if (state.phase === "idle") return null;

  const percent =
    state.progress === null ? null : Math.round(state.progress * 100);

  return (
    <div className="flex items-center gap-3 border-b border-line bg-panel-2 px-4 py-1.5 text-xs">
      <span aria-hidden>🎉</span>

      {state.phase === "available" ? (
        <>
          <span className="text-ink">
            Доступне оновлення{" "}
            <span className="font-mono text-accent">{state.version}</span>
          </span>
          <button
            type="button"
            onClick={() => void install()}
            className="rounded border border-accent/60 bg-panel px-2 py-0.5 text-accent hover:bg-accent/10"
          >
            Оновити зараз
          </button>
        </>
      ) : null}

      {state.phase === "installing" ? (
        <span className="text-ink-dim">
          Завантаження {state.version}
          {percent === null ? "…" : ` · ${percent}%`}
        </span>
      ) : null}

      {state.phase === "restart" ? (
        <span className="text-ink-dim">
          Оновлено до {state.version} — перезапуск…
        </span>
      ) : null}

      {state.phase === "failed" ? (
        <span className="text-reap">
          Оновлення не встановилося: {state.error}
        </span>
      ) : null}

      <button
        type="button"
        onClick={dismiss}
        title="Приховати до наступного запуску"
        aria-label="Приховати повідомлення про оновлення"
        className="ml-auto rounded px-1 text-ink-faint hover:bg-panel hover:text-ink"
      >
        ✕
      </button>
    </div>
  );
}
