/**
 * Оверлей підтвердження REAP (T-135, docs/ui.md §8) — єдиний оверлей у
 * продукті: відкривається кнопкою [REAP] (TopBar) або `Ctrl+Enter`
 * (`reap_confirm`, глобальний контекст, T-103), керується `reapOverlayStore`.
 *
 * Дані: `selectionStore` (T-108) тримає лише candidateId+sizeBytes на всю
 * сесію (позначення з кількох категорій) — повні path/kind/decision для
 * прев'ю й групування дістає нова команда `candidate.batch` (не прив'язана
 * до однієї категорії, на відміну від `category.window`).
 *
 * Групування — за батьківською директорією позначених файлів (`dirName`):
 * папки-одиниці (node_modules тощо, T-053) уже прийшли одним кандидатом з
 * агрегованим розміром/кількістю — нової логіки агрегації тут не треба.
 *
 * Прогноз диска — клієнтський порт чистої формули Core `compute_cleanup_forecast`
 * (T-056, `domain::forecast`): capacity/freeBytes уже в `useAppState().volumes`
 * (T-106), quarantine held bytes — у бейджі (T-106/T-113). Спрощення: беремо
 * ТОМ з найбільшим позначеним обсягом («том, що очищується») і глобальний
 * held (per-volume розбивку Quarantine IPC поки не віддає) — достатньо для
 * DoD «відповідає wireframe», не претендує на точність для мульти-дискового
 * позначення одночасно.
 *
 * T-136: маркер «?» — м'яка підказка, не блокування (ui.md §8): недавній
 * доступ (< 7 днів) або робоча локація (Desktop/Documents). Підозрілі рядки
 * підіймаються нагору **в межах своєї групи**, а групи, що містять хоча б
 * одну підозрілу позицію, підіймаються над рештою груп — групування
 * директорій (T-135) при цьому не ламається.
 *
 * Викидання ✕ зі списку (T-137) — окрема задача, тут немає. «Відправити у
 * Quarantine» — навмисно `disabled`: виконання (прогрес батч-reap + тост-
 * скасування) — T-138, залежить від `reap.execute` (planned) і не
 * будувалось заздалегідь.
 */

import { useEffect, useState } from "react";

import { command, ipcErrorMessage } from "@/ipc/client";
import { useAppState } from "@/store/appState";
import { formatBytes } from "@/store/format";
import { useThumbnail } from "@/store/preview";
import { reapOverlayStore, useReapOverlayOpen } from "@/store/reapOverlay";
import { selectionStore, useMarkedSummary } from "@/store/selection";
import type { Candidate, VolumeUsageInfo } from "@/ipc/types";

function fileName(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  return normalized.split("/").filter(Boolean).at(-1) || path;
}

function dirName(path: string): string {
  const idx = path.lastIndexOf("\\");
  return idx > 0 ? path.slice(0, idx) : path;
}

function driveLetter(path: string): string {
  const match = /^([A-Za-z]:)/.exec(path);
  return match?.[1] ?? "?:";
}

/** Відносний вік («сьогодні»/«N дн тому»/«N міс»/«N р») з `lastAccessAt`. */
function ageLabel(iso: string): string {
  const accessed = Date.parse(iso);
  if (!Number.isFinite(accessed)) return "—";
  const days = Math.max(0, Math.floor((Date.now() - accessed) / 86_400_000));
  if (days < 1) return "сьогодні";
  if (days < 30) return `${days} дн тому`;
  const months = Math.floor(days / 30);
  if (months < 24) return `${months} міс`;
  return `${Math.floor(months / 12)} р`;
}

function groupByDirectory(candidates: Candidate[]): Array<[string, Candidate[]]> {
  const map = new Map<string, Candidate[]>();
  for (const c of candidates) {
    const dir = dirName(c.path);
    const list = map.get(dir);
    if (list) list.push(c);
    else map.set(dir, [c]);
  }
  return [...map.entries()];
}

const RECENT_ACCESS_MS = 7 * 86_400_000;
const WORKING_LOCATION_SEGMENTS = ["desktop", "documents"];

function isWorkingLocation(path: string): boolean {
  const lower = path.toLowerCase();
  return WORKING_LOCATION_SEGMENTS.some((seg) => lower.includes(`\\${seg}\\`) || lower.endsWith(`\\${seg}`));
}

/** «?» — м'яка підказка (T-136): недавній доступ або робоча локація. */
function suspicionReason(candidate: Candidate): string | null {
  const accessed = Date.parse(candidate.lastAccessAt);
  const recentAccess = Number.isFinite(accessed) && Date.now() - accessed < RECENT_ACCESS_MS;
  const workingLocation = isWorkingLocation(candidate.path);
  if (recentAccess && workingLocation) return "Недавній доступ і робоча директорія (Desktop/Documents)";
  if (recentAccess) return "Недавній доступ (менш ніж 7 днів тому)";
  if (workingLocation) return "Робоча директорія (Desktop/Documents)";
  return null;
}

/** Підозрілі — нагору в межах групи; групи з підозрілими — над рештою груп. */
function sortBySuspicion(groups: Array<[string, Candidate[]]>): Array<[string, Candidate[]]> {
  const withFlags = groups.map<[string, Candidate[], boolean]>(([dir, items]) => {
    const sorted = [...items].sort((a, b) => {
      const as = suspicionReason(a) ? 1 : 0;
      const bs = suspicionReason(b) ? 1 : 0;
      return bs - as;
    });
    const hasSuspicious = sorted.some((c) => suspicionReason(c) !== null);
    return [dir, sorted, hasSuspicious];
  });
  withFlags.sort((a, b) => Number(b[2]) - Number(a[2]));
  return withFlags.map(([dir, items]) => [dir, items]);
}

interface DiskForecast {
  volume: string;
  usedPercentNow: number;
  usedPercentAfter: number;
}

/** Клієнтський порт `domain::forecast::compute_cleanup_forecast` (T-056). */
function computeForecast(
  candidates: Candidate[],
  volumes: VolumeUsageInfo[],
  quarantineHeldBytes: number,
): DiskForecast | null {
  const [firstVolume] = volumes;
  if (!firstVolume) return null;
  const markedByVolume = new Map<string, number>();
  for (const c of candidates) {
    const v = driveLetter(c.path);
    markedByVolume.set(v, (markedByVolume.get(v) ?? 0) + c.sizeBytes);
  }
  let targetVolume = firstVolume.volume;
  let maxMarked = -1;
  for (const [v, bytes] of markedByVolume) {
    if (bytes > maxMarked) {
      maxMarked = bytes;
      targetVolume = v;
    }
  }
  const usage = volumes.find((v) => v.volume === targetVolume) ?? firstVolume;
  const markedOnVolume = markedByVolume.get(usage.volume) ?? 0;
  const capacity = usage.capacityBytes;
  const freeNow = Math.min(usage.freeBytes, capacity);
  const freeAfter = Math.min(freeNow + quarantineHeldBytes + markedOnVolume, capacity);
  const pct = (part: number, whole: number) =>
    whole === 0 ? 0 : Math.min(100, Math.round((part / whole) * 100));
  return {
    volume: usage.volume,
    usedPercentNow: pct(capacity - freeNow, capacity),
    usedPercentAfter: pct(capacity - freeAfter, capacity),
  };
}

function ReapRow({ candidate }: { candidate: Candidate }) {
  const thumbnail = useThumbnail(candidate);
  const reason = suspicionReason(candidate);
  return (
    <div className="flex items-center gap-2 border-b border-line/50 py-1 text-xs last:border-b-0">
      <span
        className="w-3 shrink-0 text-center font-semibold text-quarantine"
        title={reason ?? undefined}
      >
        {reason ? "?" : ""}
      </span>
      <div className="flex h-8 w-8 shrink-0 items-center justify-center overflow-hidden rounded-sm bg-panel-2">
        {thumbnail ? (
          <img src={thumbnail} alt="" className="h-full w-full object-cover" draggable={false} />
        ) : (
          <span className="text-ink-faint" aria-hidden="true">
            ◇
          </span>
        )}
      </div>
      <span className="min-w-0 flex-1 truncate text-ink">{fileName(candidate.path)}</span>
      <span className="shrink-0 font-mono text-ink-dim">{formatBytes(candidate.sizeBytes)}</span>
      <span className="shrink-0 text-ink-faint">{ageLabel(candidate.lastAccessAt)}</span>
    </div>
  );
}

export function ReapConfirmOverlay() {
  const open = useReapOverlayOpen();
  const { volumes, quarantine, settings } = useAppState();
  const summary = useMarkedSummary();
  const [candidates, setCandidates] = useState<Candidate[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setLoading(true);
    setError(null);
    command<Candidate[]>("candidate.batch", {
      payload: { candidateIds: selectionStore.markedIds() },
    })
      .then((res) => setCandidates(res))
      .catch((err) => setError(ipcErrorMessage(err)))
      .finally(() => setLoading(false));
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") reapOverlayStore.close();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open]);

  if (!open) return null;

  const groups = sortBySuspicion(groupByDirectory(candidates));
  const forecast = computeForecast(candidates, volumes, quarantine.heldBytes);
  const ttlDays = settings?.quarantine.ttlDays ?? 30;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-bg/80 backdrop-blur-sm">
      <div className="flex max-h-[80vh] w-[36rem] flex-col rounded border border-line bg-panel shadow-lg">
        <div className="flex items-center justify-between gap-3 border-b border-line px-4 py-2">
          <h2 className="text-sm font-semibold text-ink">
            REAP: підтвердження — {summary.count} {summary.count === 1 ? "файл" : "файлів"} ·{" "}
            {formatBytes(summary.bytes)} → Quarantine
          </h2>
          {forecast ? (
            <span className="shrink-0 font-mono text-xs text-ink-faint">
              {forecast.volume} {forecast.usedPercentNow}% → {forecast.usedPercentAfter}% після
              очищення
            </span>
          ) : null}
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-2">
          {loading ? (
            <div className="py-6 text-center text-sm text-ink-faint">Завантаження…</div>
          ) : error ? (
            <div className="py-6 text-center text-sm text-ink-faint">{error}</div>
          ) : groups.length === 0 ? (
            <div className="py-6 text-center text-sm text-ink-faint">Немає позначених файлів</div>
          ) : (
            groups.map(([dir, items]) => (
              <div key={dir} className="mb-3">
                <div className="mb-1 truncate font-mono text-xs text-ink-faint">{dir}</div>
                {items.map((c) => (
                  <ReapRow key={c.id} candidate={c} />
                ))}
              </div>
            ))
          )}
        </div>
        <div className="border-t border-line px-4 py-3">
          <p className="mb-2 text-xs text-ink-faint">
            Автознищення через {ttlDays} дн · відновлення будь-коли
          </p>
          <div className="flex justify-end gap-2">
            <button
              type="button"
              onClick={() => reapOverlayStore.close()}
              className="rounded border border-line px-3 py-1 text-sm text-ink-dim hover:bg-panel-2"
            >
              Назад
            </button>
            <button
              type="button"
              disabled
              title="Виконання — T-138"
              className="cursor-not-allowed rounded bg-reap/50 px-3 py-1 text-sm font-semibold text-bg opacity-70"
            >
              Відправити у Quarantine
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
