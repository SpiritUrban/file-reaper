/**
 * Жива смуга прогресу скану: проіндексовано / залишилось / підфаза MFT|walk.
 */

import { command, ipcErrorMessage } from "@/ipc/client";
import { describeScanActivity, useAppState } from "@/store/appState";
import { toast } from "@/store/toasts";
import type { ScanProgressEvent, ScanStopAck } from "@/ipc/types";

function fmtCount(n: number): string {
  return n.toLocaleString("uk-UA");
}

function stageLabel(stage: string | undefined): string | null {
  switch (stage) {
    case "mft_dirs":
      return "читання MFT (карти тек)";
    case "mft_files":
      return "читання MFT (файли)";
    case "walking":
      return "обхід каталогів";
    case "indexing":
      return "індексація файлів";
    case "cascade":
      return "пошук дублікатів";
    default:
      return null;
  }
}

function progressFraction(p: ScanProgressEvent): number {
  // 1) Підфаза з відомим total одиниць (MFT records)
  const unitsDone = p.stageUnitsDone ?? 0;
  const unitsTotal = p.stageUnitsTotal ?? 0;
  if (unitsTotal > 0 && (p.filesIndexed ?? 0) === 0) {
    return Math.min(0.99, unitsDone / unitsTotal);
  }
  // 2) Файли на томі з total
  if (p.volumeFilesTotal && p.volumeFilesTotal > 0) {
    const volFrac = Math.min(1, p.volumeFilesIndexed / p.volumeFilesTotal);
    if (p.volumeCount <= 1) return volFrac;
    return Math.min(1, (p.volumeIndex + volFrac) / p.volumeCount);
  }
  // 3) Лише томи
  if (p.volumeCount > 0) {
    const base = p.volumeIndex / p.volumeCount;
    const bump =
      p.phase === "volume_finished" ||
      p.phase === "session_finished" ||
      p.phase === "duplicates_cascade"
        ? 1 / p.volumeCount
        : p.filesIndexed > 0
          ? 0.4 / p.volumeCount
          : unitsDone > 0
            ? 0.2 / p.volumeCount
            : 0.05 / p.volumeCount;
    return Math.min(0.99, base + bump);
  }
  return 0;
}

export function ScanActivityBanner() {
  const state = useAppState();
  const label = describeScanActivity(state);
  const p = state.scanProgress;

  if (!state.scanRunning && !label) return null;

  const text = label ?? "Сканування…";
  const files = p?.filesIndexed ?? 0;
  const volFiles = p?.volumeFilesIndexed ?? 0;
  const volTotal = p?.volumeFilesTotal;
  const unitsDone = p?.stageUnitsDone ?? 0;
  const unitsTotal = p?.stageUnitsTotal;
  const stage = stageLabel(p?.stage);
  const reclaimable = state.cleanup.reclaimableBytes;
  const frac = p && !p.done ? progressFraction(p) : p?.done ? 1 : 0;
  const pct = Math.round(frac * 100);

  const remOnVol =
    volTotal != null && volTotal > 0 ? Math.max(0, volTotal - volFiles) : null;
  const remUnits =
    unitsTotal != null && unitsTotal > 0
      ? Math.max(0, unitsTotal - unitsDone)
      : null;

  const stop = () => {
    void command<ScanStopAck>("scan.stop").catch((error) =>
      toast({ message: ipcErrorMessage(error), tone: "warning" }),
    );
  };

  return (
    <div
      className="flex shrink-0 flex-col gap-1 border-b border-accent/30 bg-accent/10 px-3 py-2 text-xs"
      role="status"
      aria-live="polite"
    >
      <div className="flex items-center gap-3">
        <span
          className="inline-block h-2 w-2 shrink-0 animate-pulse rounded-full bg-accent"
          aria-hidden
        />
        <span className="min-w-0 flex-1 text-ink">
          <span className="font-medium">{text}</span>
          {stage ? (
            <span className="text-ink-dim"> · {stage}</span>
          ) : null}
        </span>
        <button
          type="button"
          onClick={stop}
          className="shrink-0 rounded border border-line px-2 py-0.5 text-ink-dim hover:bg-panel-2 hover:text-ink"
        >
          Зупинити
        </button>
      </div>

      <div className="flex flex-wrap items-baseline gap-x-4 gap-y-0.5 pl-5 font-mono text-ink">
        <span>
          <span className="text-ink-faint">Проіндексовано </span>
          <span className="text-sm font-semibold">{fmtCount(files)}</span>
          <span className="text-ink-faint"> файлів</span>
        </span>

        {/* Підфаза: MFT-записи / walk — росте, коли files ще 0 */}
        {unitsDone > 0 || (unitsTotal != null && unitsTotal > 0) ? (
          <span>
            <span className="text-ink-faint">
              {p?.stage === "mft_dirs" || p?.stage === "mft_files"
                ? "MFT "
                : p?.stage === "walking"
                  ? "об'єктів "
                  : "крок "}
            </span>
            <span className="font-semibold">
              {fmtCount(unitsDone)}
              {unitsTotal != null && unitsTotal > 0
                ? ` / ${fmtCount(unitsTotal)}`
                : ""}
            </span>
            {remUnits != null && remUnits > 0 && files === 0 ? (
              <span className="text-quarantine">
                {" "}
                · лишилось {fmtCount(remUnits)}
              </span>
            ) : null}
          </span>
        ) : null}

        {p && volTotal != null && volTotal > 0 ? (
          <span>
            <span className="text-ink-faint">на {p.volume} </span>
            <span className="font-semibold">
              {fmtCount(volFiles)} / {fmtCount(volTotal)}
            </span>
            {remOnVol != null && remOnVol > 0 ? (
              <span className="text-quarantine">
                {" "}
                · лишилось {fmtCount(remOnVol)}
              </span>
            ) : null}
          </span>
        ) : p && volFiles > 0 ? (
          <span>
            <span className="text-ink-faint">на {p.volume} </span>
            <span className="font-semibold">{fmtCount(volFiles)}</span>
          </span>
        ) : null}

        {p && p.volumeCount > 1 ? (
          <span className="text-ink-faint">
            том {p.volumeIndex + 1}/{p.volumeCount}
          </span>
        ) : null}

        {reclaimable > 0 ? (
          <span className="text-ink-dim">
            кандидати{" "}
            <span className="text-ink">
              {(reclaimable / 1024 ** 3).toFixed(2)} ГБ
            </span>
          </span>
        ) : null}
      </div>

      <div className="flex items-center gap-2 pl-5">
        <div className="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-panel-2">
          <div
            className="h-full rounded-full bg-accent transition-[width] duration-200 ease-out"
            style={{ width: `${pct}%` }}
          />
        </div>
        <span className="w-10 shrink-0 text-right font-mono text-ink-faint">
          {pct}%
        </span>
      </div>
    </div>
  );
}
