import { useEffect, useRef, useState } from "react";

import { toastStore, type ToastRecord, useToasts } from "@/store/toasts";

const TONE_CLASS: Record<ToastRecord["tone"], string> = {
  info: "border-accent/50",
  success: "border-keep/50",
  warning: "border-quarantine/60",
};

export function ToastViewport() {
  const toasts = useToasts();
  return (
    <aside
      className="pointer-events-none fixed bottom-4 right-4 z-50 flex w-96 max-w-[calc(100vw-2rem)] flex-col gap-2"
      aria-label="Сповіщення"
      aria-live="polite"
    >
      {toasts.map((record) => (
        <ToastItem key={record.id} record={record} />
      ))}
    </aside>
  );
}

function ToastItem({ record }: { record: ToastRecord }) {
  const [remainingMs, setRemainingMs] = useState(record.durationMs);
  const timeoutRef = useRef<number | null>(null);
  const startedAtRef = useRef(0);
  const remainingRef = useRef(record.durationMs);

  const pause = () => {
    if (timeoutRef.current === null) return;
    window.clearTimeout(timeoutRef.current);
    timeoutRef.current = null;
    const elapsed = performance.now() - startedAtRef.current;
    remainingRef.current = Math.max(0, remainingRef.current - elapsed);
    setRemainingMs(remainingRef.current);
  };

  const resume = () => {
    if (timeoutRef.current !== null || remainingRef.current <= 0) return;
    startedAtRef.current = performance.now();
    timeoutRef.current = window.setTimeout(() => {
      timeoutRef.current = null;
      remainingRef.current = 0;
      setRemainingMs(0);
      toastStore.dismiss(record.id);
    }, remainingRef.current);
  };

  useEffect(() => {
    remainingRef.current = record.durationMs;
    setRemainingMs(record.durationMs);
    resume();
    return pause;
    // record.id identifies a new lifetime; duration is immutable in the store.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [record.id]);

  const progress = Math.max(0, remainingMs / record.durationMs);
  return (
    <div
      className={`pointer-events-auto overflow-hidden rounded border bg-panel shadow-lg ${TONE_CLASS[record.tone]}`}
      role="status"
      onMouseEnter={pause}
      onMouseLeave={resume}
      onFocusCapture={pause}
      onBlurCapture={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) resume();
      }}
    >
      <div className="flex items-start gap-3 px-3 py-2.5">
        <p className="min-w-0 flex-1 text-sm leading-relaxed text-ink">
          {record.message}
        </p>
        {record.action ? (
          <button
            type="button"
            disabled={record.actionConsumed}
            className="shrink-0 rounded px-2 py-0.5 text-sm font-semibold text-accent hover:bg-panel-2 disabled:opacity-40"
            onClick={() => toastStore.runAction(record.id)}
          >
            {record.action.label}
          </button>
        ) : null}
        <button
          type="button"
          className="shrink-0 rounded px-1 text-ink-dim hover:bg-panel-2 hover:text-ink"
          aria-label="Закрити сповіщення"
          onClick={() => toastStore.dismiss(record.id)}
        >
          ×
        </button>
      </div>
      <div className="h-0.5 bg-panel-2">
        <div
          className="h-full origin-left bg-accent transition-[transform] duration-100"
          style={{ transform: `scaleX(${progress})` }}
        />
      </div>
    </div>
  );
}