/**
 * Стартовий попап: явна дія «Почати сканування», коли даних ще немає.
 * Замінює тихий автостарт T-114 — користувач бачить, що треба зробити,
 * і що далі відбудеться.
 */

import { useState } from "react";

import { command, ipcErrorMessage, isTauri } from "@/ipc/client";
import { appStateStore, useAppState } from "@/store/appState";
import { toast } from "@/store/toasts";
import type { ScanStartAck } from "@/ipc/types";

export function ScanStartOverlay() {
  const {
    status,
    scanRunning,
    scanStartedThisSession,
    cleanup,
    isFirstRun,
    volumes,
  } = useAppState();
  const [starting, setStarting] = useState(false);
  const [dismissed, setDismissed] = useState(false);

  const emptyData =
    cleanup.reclaimableBytes === 0 && (cleanup.uniqueFiles ?? 0) === 0;
  const show =
    isTauri() &&
    status === "ready" &&
    !scanRunning &&
    !scanStartedThisSession &&
    emptyData &&
    !dismissed;

  if (!show) return null;

  const volumeList =
    volumes.length > 0
      ? volumes.map((v) => v.volume.replace(/:$/, "")).join(", ")
      : "усі доступні диски";

  const startScan = async () => {
    setStarting(true);
    appStateStore.markScanStarted();
    try {
      await command<ScanStartAck>("scan.start", { payload: {} });
    } catch (error) {
      toast({ message: ipcErrorMessage(error), tone: "warning" });
      setStarting(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-40 flex items-center justify-center bg-bg/80 px-4"
      role="dialog"
      aria-modal="true"
      aria-labelledby="scan-start-title"
    >
      <div className="w-full max-w-md rounded-lg border border-line bg-panel p-6 shadow-xl">
        <p className="mb-1 text-xs font-medium uppercase tracking-wider text-accent">
          {isFirstRun ? "Перший запуск" : "Немає даних"}
        </p>
        <h2 id="scan-start-title" className="text-xl font-semibold text-ink">
          Знайдемо, що можна звільнити
        </h2>
        <p className="mt-3 text-sm leading-relaxed text-ink-dim">
          TrashRadar не показує «всі файли» — він сканує диски, знаходить
          кандидатів на видалення (великі/старі файли, дублікати, архіви…) і
          показує лише їх. Поки скан не запущено, цифра «можна звільнити»
          порожня.
        </p>
        <ul className="mt-4 space-y-1.5 text-xs text-ink-dim">
          <li>
            <span className="text-ink-faint">Диски: </span>
            <span className="font-mono text-ink">{volumeList}</span>
          </li>
          <li>
            <span className="text-ink-faint">Що побачите: </span>
            прогрес у верхній смузі, потім живу цифру на Cleanup і категорії
            в Sidebar
          </li>
          <li>
            <span className="text-ink-faint">Час: </span>
            від десятків секунд до кількох хвилин залежно від обсягу диска
          </li>
        </ul>

        <div className="mt-6 flex flex-col gap-2 sm:flex-row sm:items-center">
          <button
            type="button"
            disabled={starting}
            onClick={() => void startScan()}
            className="flex-1 rounded bg-accent px-4 py-2.5 text-sm font-semibold text-bg hover:brightness-110 disabled:opacity-60"
          >
            {starting ? "Запуск…" : "Почати сканування"}
          </button>
          <button
            type="button"
            disabled={starting}
            onClick={() => setDismissed(true)}
            className="rounded border border-line px-4 py-2.5 text-sm text-ink-dim hover:bg-panel-2 hover:text-ink disabled:opacity-60"
          >
            Пізніше
          </button>
        </div>
        <p className="mt-3 text-xs text-ink-faint">
          Пізніше можна запустити кнопкою ⟳ у верхній панелі або біля диска в
          Sidebar.
        </p>
      </div>
    </div>
  );
}
