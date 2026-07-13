/**
 * Quarantine — «передсмертна зона» файлів (T-130, docs/ui.md §7): та сама
 * механіка сітки з превью, що й Radar, у бурштиновій рамці режиму (T-095
 * amber — «режим карантину», не плутати з reap-червоним). Дані —
 * `quarantine.window` (лише статус `quarantined`, той самий фільтр, що й
 * бейдж T-106); превью — окрема команда `quarantine.thumbnail` (T-130), бо
 * джерело — сурогатний шлях карантину, не HotIndex-запис (файл уже
 * переміщено з оригінального шляху, T-088).
 *
 * Верхній рядок (обсяг/дата автоочищення) живиться вже наявним `useAppState`
 * `quarantine`-бейджем (T-106/T-113) — не новий фетч. Рефетч списку на
 * кожну зміну бейджа (той самий сигнал, що й `quarantine.changed`, T-082).
 *
 * T-131: таймер ⏳ на плитці «живий» — `useLiveNow` тікає раз на 30 с,
 * достатньо для гранулярності дн/год/хв (без секундної стрілки); дефолтне
 * сортування Core (найближче згорання, T-130) доповнено клієнтським пікером
 * дата видалення/розмір/шлях — той самий патерн, що й фільтр-чипси T-107
 * (сортування над уже завантаженим `entries`, без повторного запиту).
 *
 * T-132: Restore — клавіша `R` (контекст "quarantine", вже в реєстрі T-103,
 * активується AppLayout, коли цей екран видимий) на сфокусованій плитці, або
 * кнопка «R Відновити» на будь-якій. `quarantine.restore_batch` (Core
 * use case T-080, щойно підключений) — FS move + manifest→Restored + аудит,
 * тут лише виклик з одним entryId; тост «Відновлено у …» з дією «Показати»
 * (T-104) → `quarantine.reveal_path`. Відновлений запис прибирається зі
 * списку локально (той самий `entries`, без повторного фетчу).
 *
 * T-133: Знищення — клік по плитці (поза кнопкою Restore) позначає її до
 * знищення (червона рамка/підсвітка — деструктивна дія, ui.md палітра: «Червоний
 * ТІЛЬКИ для видалення»); «Знищити позначені» / «Спорожнити все» відкривають
 * ЄДИНЕ у продукті жорстке модальне підтвердження (усе інше в застосунку —
 * м'який undo через тости, тут відновити ніяк — тому підтвердження, а не тост).
 * `quarantine.purge` (Core use case `ManualPurger`, T-083, щойно підключений)
 * реально видаляє сурогат з диска й переводить manifest у Purged — саме тому
 * підтверджені записи прибираються з `entries` без нового фетчу: місце вже
 * звільнено.
 *
 * T-134: Пошук `/` — той самий хоткей і UX, що й T-109 (real-time, Escape
 * закриває й очищує), але локальний до цього екрана: TopBar `SearchBox`
 * (T-109) прив'язаний до `CategoryId` через `searchStore` і мовчки
 * нічого не робить, коли `categoryId===null` (Quarantine — не категорія).
 * Тому власний невеликий стан тут, а не розширення категорійного стору.
 * Фільтрує за `originalPath` (DoD: не суррогатним іменем — той файл, що вже
 * лежить під `.trashradar\quarantine`, користувачу не показується і не має
 * значення для пошуку «де він був»), той самий infix/case-insensitive
 * принцип, що й `applySearchQuery` (T-109), лише на іншому полі запису.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { command, ipcErrorMessage } from "@/ipc/client";
import { formatBytes } from "@/store/format";
import { useAppState } from "@/store/appState";
import { useQuarantineThumbnail } from "@/store/preview";
import { toast } from "@/store/toasts";
import type { HotkeyActionEventDetail } from "@/hotkeys";
import type {
  QuarantineEntry,
  QuarantinePurgeAck,
  QuarantineRestoreOutcome,
} from "@/ipc/types";

function fileName(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  return normalized.split("/").filter(Boolean).at(-1) || path;
}

function sourceVolume(path: string): string {
  const match = /^([A-Za-z]:)/.exec(path);
  return match?.[1] ?? "?:";
}

/** Час до автознищення (ui.md §7 «⏳ 2 дн»); минуле → «згорає». */
function timeUntilExpiry(iso: string, now: number): string {
  const expires = Date.parse(iso);
  if (!Number.isFinite(expires)) return "—";
  const ms = expires - now;
  if (ms <= 0) return "згорає";
  const days = Math.floor(ms / 86_400_000);
  if (days >= 1) return `${days} дн`;
  const hours = Math.floor(ms / 3_600_000);
  if (hours >= 1) return `${hours} год`;
  return `${Math.max(1, Math.floor(ms / 60_000))} хв`;
}

/** Живий годинник з кроком 30 с — досить для дн/год/хв гранулярності таймера. */
function useLiveNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(id);
  }, []);
  return now;
}

type SortKey = "expiry" | "quarantined_at" | "size" | "path";

const SORT_OPTIONS: { value: SortKey; label: string }[] = [
  { value: "expiry", label: "Згорання" },
  { value: "quarantined_at", label: "Дата видалення" },
  { value: "size", label: "Розмір" },
  { value: "path", label: "Початковий шлях" },
];

/** Пошук infix по `originalPath` (T-134 DoD: не суррогатним іменем). */
function filterByOriginalPath(entries: QuarantineEntry[], query: string): QuarantineEntry[] {
  if (!query) return entries;
  const needle = query.toLowerCase();
  return entries.filter((e) => e.originalPath.toLowerCase().includes(needle));
}

function sortEntries(entries: QuarantineEntry[], sort: SortKey): QuarantineEntry[] {
  const sorted = [...entries];
  switch (sort) {
    case "expiry":
      sorted.sort((a, b) => Date.parse(a.expiresAt) - Date.parse(b.expiresAt));
      break;
    case "quarantined_at":
      sorted.sort((a, b) => Date.parse(b.quarantinedAt) - Date.parse(a.quarantinedAt));
      break;
    case "size":
      sorted.sort((a, b) => b.sizeBytes - a.sizeBytes);
      break;
    case "path":
      sorted.sort((a, b) => a.originalPath.localeCompare(b.originalPath));
      break;
  }
  return sorted;
}

function QuarantineTile({
  entry,
  now,
  focused,
  marked,
  onFocusEntry,
  onRestore,
  onToggleMark,
}: {
  entry: QuarantineEntry;
  now: number;
  focused: boolean;
  marked: boolean;
  onFocusEntry: (id: number) => void;
  onRestore: (entry: QuarantineEntry) => void;
  onToggleMark: (id: number) => void;
}) {
  const thumbnail = useQuarantineThumbnail(entry);
  return (
    <div
      tabIndex={0}
      onFocus={() => onFocusEntry(entry.id)}
      onClick={() => onToggleMark(entry.id)}
      data-focused={focused || undefined}
      data-marked={marked || undefined}
      title={entry.originalPath}
      className={`relative flex aspect-[4/3] w-full cursor-pointer flex-col overflow-hidden rounded-sm border bg-panel outline-none transition-colors focus-visible:ring-2 focus-visible:ring-accent/70 ${
        marked ? "border-reap" : focused ? "border-quarantine" : "border-quarantine/40"
      }`}
    >
      <span className="absolute inset-0 flex items-center justify-center overflow-hidden bg-panel-2">
        {thumbnail ? (
          <img
            src={thumbnail}
            alt=""
            className="h-full w-full object-cover"
            draggable={false}
          />
        ) : (
          <span className="text-4xl text-ink-faint" aria-hidden="true">
            ◇
          </span>
        )}
      </span>
      {marked ? <span className="absolute inset-0 z-10 bg-reap/30" /> : null}
      <span className="absolute left-2 top-2 z-20 rounded-full bg-bg/85 px-2 py-0.5 text-xs text-quarantine backdrop-blur-sm">
        ⏳ {timeUntilExpiry(entry.expiresAt, now)}
      </span>
      {marked ? (
        <span
          className="absolute right-2 top-2 z-20 grid h-6 w-6 place-items-center rounded-full bg-reap text-sm font-bold text-bg"
          aria-label="Позначено до знищення"
        >
          ╳
        </span>
      ) : (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            onRestore(entry);
          }}
          title="Відновити на початкове місце (R)"
          className="absolute right-1 top-1 z-20 rounded bg-keep/90 px-1.5 py-0.5 text-xs font-medium text-bg hover:bg-keep"
        >
          R Відновити
        </button>
      )}
      <span className="absolute inset-x-0 bottom-0 z-20 flex h-[15%] min-h-7 items-center gap-1.5 bg-bg/85 px-2 backdrop-blur-sm">
        <strong className="shrink-0 font-mono text-sm font-semibold text-ink">
          {formatBytes(entry.sizeBytes)}
        </strong>
        <span className="min-w-0 flex-1 truncate font-mono text-xs text-ink-dim">
          з {sourceVolume(entry.originalPath)} {fileName(entry.originalPath)}
        </span>
      </span>
    </div>
  );
}

/** «Єдине у продукті жорстке підтвердження» (ui.md §7) — деструктивне, без undo. */
function PurgeConfirmDialog({
  count,
  bytes,
  all,
  busy,
  onCancel,
  onConfirm,
}: {
  count: number;
  bytes: number;
  all: boolean;
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-bg/80 backdrop-blur-sm">
      <div className="w-96 rounded border border-reap/60 bg-panel p-4 shadow-lg">
        <h2 className="mb-2 text-sm font-semibold text-reap">Остаточне знищення</h2>
        <p className="mb-4 text-sm text-ink-dim">
          {all
            ? `Спорожнити весь карантин: ${count} ${count === 1 ? "файл" : "файлів"} · ${formatBytes(bytes)}.`
            : `Знищити позначені: ${count} ${count === 1 ? "файл" : "файлів"} · ${formatBytes(bytes)}.`}{" "}
          Місце звільниться одразу. Це не можна скасувати — файли не потрапляють у кошик Windows.
        </p>
        <div className="flex justify-end gap-2">
          <button
            type="button"
            disabled={busy}
            onClick={onCancel}
            className="rounded border border-line px-3 py-1 text-sm text-ink-dim hover:bg-panel-2"
          >
            Скасувати
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={onConfirm}
            className="rounded bg-reap px-3 py-1 text-sm font-semibold text-bg hover:bg-reap/85 disabled:cursor-not-allowed disabled:opacity-60"
          >
            {busy ? "Знищення…" : "Знищити назавжди"}
          </button>
        </div>
      </div>
    </div>
  );
}

/** Що саме зараз підтверджується жорстким діалогом. */
type PurgeRequest = { all: true } | { all: false; ids: number[] };

export function QuarantineScreen() {
  const { quarantine } = useAppState();
  const [entries, setEntries] = useState<QuarantineEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [sort, setSort] = useState<SortKey>("expiry");
  const [focusedId, setFocusedId] = useState<number | null>(null);
  const [markedIds, setMarkedIds] = useState<ReadonlySet<number>>(new Set());
  const [purgeRequest, setPurgeRequest] = useState<PurgeRequest | null>(null);
  const [purging, setPurging] = useState(false);
  const [searchActive, setSearchActive] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const searchInputRef = useRef<HTMLInputElement>(null);
  const now = useLiveNow();

  // T-134: `/` активує пошук (той самий хоткей, що й T-109 у категоріях),
  // Escape закриває й очищує. Локальний стан — не searchStore (T-109), бо той
  // прив'язаний до CategoryId, а Quarantine не категорія.
  useEffect(() => {
    const onHotkey = (event: Event) => {
      const { action } = (event as CustomEvent<HotkeyActionEventDetail>).detail;
      if (action === "search") setSearchActive(true);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && searchActive) {
        setSearchActive(false);
        setSearchQuery("");
      }
    };
    window.addEventListener("trashradar:hotkey", onHotkey);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("trashradar:hotkey", onHotkey);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [searchActive]);

  useEffect(() => {
    if (searchActive) searchInputRef.current?.focus();
  }, [searchActive]);

  const toggleMark = useCallback((id: number) => {
    setMarkedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const load = useCallback(() => {
    command<QuarantineEntry[]>("quarantine.window")
      .then((res) => {
        setEntries(res);
        setError(null);
      })
      .catch((err) => setError(ipcErrorMessage(err)));
  }, []);

  // Рефетч на кожну зміну бейджа (T-106) — той самий сигнал, що й
  // quarantine.changed (purge/restore міняють held-лічильники).
  useEffect(() => {
    load();
  }, [load, quarantine.heldCount, quarantine.heldBytes]);

  // T-132: Restore — Core вже робить move+manifest+аудит (`QuarantineRestorer`,
  // T-080); тут лише виклик з одним entryId, локальне видалення з `entries`
  // (без повторного фетчу) і тост із дією «Показати» → quarantine.reveal_path.
  const restoreEntry = useCallback((entry: QuarantineEntry) => {
    command<QuarantineRestoreOutcome[]>("quarantine.restore_batch", {
      payload: { entryIds: [entry.id] },
    })
      .then((outcomes) => {
        const outcome = outcomes[0];
        if (!outcome) return;
        setEntries((prev) => prev.filter((e) => e.id !== entry.id));
        toast({
          message: `Відновлено у ${outcome.restoredPath}`,
          tone: "success",
          action: {
            label: "Показати",
            run: () =>
              command<void>("quarantine.reveal_path", {
                payload: { path: outcome.restoredPath },
              }).catch((err) => {
                toast({ message: ipcErrorMessage(err), tone: "warning" });
              }),
          },
        });
      })
      .catch((err) => toast({ message: ipcErrorMessage(err), tone: "warning" }));
  }, []);

  // R (контекст "quarantine", T-103) — відновити сфокусовану плитку.
  useEffect(() => {
    const onHotkey = (event: Event) => {
      const { action } = (event as CustomEvent<HotkeyActionEventDetail>).detail;
      if (action !== "restore" || focusedId === null) return;
      const entry = entries.find((e) => e.id === focusedId);
      if (entry) restoreEntry(entry);
    };
    window.addEventListener("trashradar:hotkey", onHotkey);
    return () => window.removeEventListener("trashradar:hotkey", onHotkey);
  }, [focusedId, entries, restoreEntry]);

  // T-133: виконати підтверджене знищення. `quarantine.purge` (ManualPurger,
  // T-083) реально видаляє сурогат з диска й переводить manifest у Purged —
  // тому знищені записи прибираються локально, без рефетчу.
  const runPurge = useCallback((purgeRequestToRun: PurgeRequest) => {
    setPurging(true);
    command<QuarantinePurgeAck>("quarantine.purge", {
      payload: purgeRequestToRun.all ? {} : { entryIds: purgeRequestToRun.ids },
    })
      .then((ack) => {
        if (purgeRequestToRun.all) {
          setEntries([]);
        } else {
          const purgedIds = new Set(purgeRequestToRun.ids);
          setEntries((prev) => prev.filter((e) => !purgedIds.has(e.id)));
        }
        setMarkedIds(new Set());
        setPurgeRequest(null);
        toast({
          message: `Знищено ${ack.purgedCount} ${ack.purgedCount === 1 ? "файл" : "файлів"} · ${formatBytes(ack.purgedBytes)}.`,
          tone: "success",
        });
      })
      .catch((err) => toast({ message: ipcErrorMessage(err), tone: "warning" }))
      .finally(() => setPurging(false));
  }, []);

  const markedEntries = useMemo(
    () => entries.filter((e) => markedIds.has(e.id)),
    [entries, markedIds],
  );
  const markedBytes = markedEntries.reduce((sum, e) => sum + e.sizeBytes, 0);

  const searched = useMemo(
    () => filterByOriginalPath(entries, searchQuery),
    [entries, searchQuery],
  );
  const sorted = useMemo(() => sortEntries(searched, sort), [searched, sort]);

  return (
    <div className="flex h-full flex-col border-t-2 border-quarantine/60">
      <div className="flex h-8 shrink-0 items-center gap-3 border-b border-line px-3 text-xs">
        <span className="font-mono text-quarantine">
          {quarantine.heldCount} файлів · {formatBytes(quarantine.heldBytes)} · найближче
          автознищення:{" "}
          {quarantine.nextPurgeAtUnix > 0
            ? new Date(quarantine.nextPurgeAtUnix * 1000).toLocaleDateString("uk-UA")
            : "—"}
        </span>
        {searchActive ? (
          <input
            ref={searchInputRef}
            type="text"
            placeholder="Пошук за початковим шляхом…"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.currentTarget.value)}
            onBlur={() => setSearchActive(searchQuery.length > 0)}
            className="w-48 rounded bg-panel-2 px-2 py-0.5 text-ink placeholder:text-ink-faint focus:outline-none focus:ring-1 focus:ring-accent"
          />
        ) : (
          <button
            type="button"
            onClick={() => setSearchActive(true)}
            className="font-mono text-ink-faint hover:text-ink"
            title="Пошук за початковим шляхом (/)"
          >
            🔍 /
          </button>
        )}
        {searchQuery ? (
          <span className="text-ink-faint">
            · результати: {sorted.length} з {entries.length}
          </span>
        ) : null}
        <label className="flex items-center gap-1 text-ink-faint">
          <span>Сорт:</span>
          <select
            value={sort}
            onChange={(e) => setSort(e.target.value as SortKey)}
            className="rounded border border-line bg-transparent px-1 py-0.5 text-ink-dim focus:border-accent focus:outline-none"
          >
            {SORT_OPTIONS.map((option) => (
              <option key={option.value} value={option.value} className="bg-panel text-ink">
                {option.label}
              </option>
            ))}
          </select>
        </label>
        <div className="flex-1" />
        <button
          type="button"
          disabled
          className="rounded border border-line px-2 py-0.5 text-keep opacity-50"
        >
          Відновити позначені
        </button>
        <button
          type="button"
          disabled={markedEntries.length === 0}
          onClick={() => setPurgeRequest({ all: false, ids: markedEntries.map((e) => e.id) })}
          className={`rounded border px-2 py-0.5 ${
            markedEntries.length === 0
              ? "cursor-not-allowed border-line text-reap opacity-50"
              : "border-reap/60 text-reap hover:bg-reap/10"
          }`}
        >
          Знищити позначені{markedEntries.length > 0 ? ` (${markedEntries.length})` : ""}
        </button>
        <button
          type="button"
          disabled={entries.length === 0}
          onClick={() => setPurgeRequest({ all: true })}
          className={`rounded border px-2 py-0.5 ${
            entries.length === 0
              ? "cursor-not-allowed border-line text-reap opacity-50"
              : "border-reap/60 text-reap hover:bg-reap/10"
          }`}
        >
          Спорожнити все
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        {error ? (
          <div className="text-sm text-ink-faint">{error}</div>
        ) : sorted.length === 0 ? (
          <div className="flex h-full items-center justify-center text-sm text-ink-faint">
            {searchQuery && entries.length > 0
              ? "Жодного збігу з пошуком"
              : "Карантин порожній"}
          </div>
        ) : (
          <div className="grid grid-cols-[repeat(auto-fill,minmax(10rem,1fr))] gap-2">
            {sorted.map((entry) => (
              <QuarantineTile
                key={entry.id}
                entry={entry}
                now={now}
                focused={focusedId === entry.id}
                marked={markedIds.has(entry.id)}
                onFocusEntry={setFocusedId}
                onRestore={restoreEntry}
                onToggleMark={toggleMark}
              />
            ))}
          </div>
        )}
      </div>
      {purgeRequest ? (
        <PurgeConfirmDialog
          all={purgeRequest.all}
          count={purgeRequest.all ? entries.length : purgeRequest.ids.length}
          bytes={
            purgeRequest.all
              ? entries.reduce((sum, e) => sum + e.sizeBytes, 0)
              : markedBytes
          }
          busy={purging}
          onCancel={() => setPurgeRequest(null)}
          onConfirm={() => runPurge(purgeRequest)}
        />
      ) : null}
    </div>
  );
}
