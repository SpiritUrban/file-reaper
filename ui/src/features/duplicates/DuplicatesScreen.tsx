/**
 * Екран «Дублікати» (T-126, docs/ui.md §4): особливе подання — сітка
 * ГРУПАМИ замість стандартної `category.window`/`CategoryScreen`. Кожна
 * група — один візуальний ряд з усіма копіями поруч (однакові превью);
 * дефолтна розмітка ✓ (залишити) / ╳ (видалити) приходить готовою з Core
 * (T-065 keep policy) — тут лише показ.
 *
 * Джерело даних — нова команда `duplicates.groups`, не `category.window`:
 * дублікати рахуються групами каскаду T-058…066 (`countUnit: "groups"`),
 * а не окремими `FileRecord`. Рефетч на кожну подію `duplicates.cascade_updated`
 * (той самий каскад іде у фоні після скану, T-126) підхоплює й попередню
 * (preliminary), і згодом підтверджену (confirmed) розмітку.
 *
 * T-127: «Прийняти» на групу / «Прийняти всі дефолти» — обидві пишуть у той
 * самий сесійний `selectionStore` (T-108), що й Space/A в `CategoryScreen`
 * (T-116): жодної нової логіки позначення, лише масовий виклик `markMultiple`
 * на ╳-екземплярах групи (кандидат на видалення, T-065). Reap Bar (TopBar)
 * уже підписаний на цей стор — оновлюється миттєво без додаткового коду.
 * Клік по копії, щоб змінити ✓ (T-128), — окрема задача.
 */

import { useCallback, useEffect, useState } from "react";

import { CandidateTile } from "@/components/CandidateTile";
import { command, ipcErrorMessage, subscribe } from "@/ipc/client";
import { formatBytes } from "@/store/format";
import { selectionStore, useMarkedSummary } from "@/store/selection";
import type {
  Candidate,
  DuplicatesGroupsAck,
  MarkedDuplicateGroup,
  MarkedDuplicateMember,
} from "@/ipc/types";

function fileName(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  return normalized.split("/").filter(Boolean).at(-1) || path;
}

/**
 * Синтетичний `Candidate` для перевикористання `CandidateTile` без зміни
 * компонента: `decision="keep"` для ✓-екземпляра вмикає зелену позначку,
 * `marked` override для ╳-екземпляра — червону (той самий проп, що й T-116
 * дає сітці категорії локальне оптимістичне позначення).
 */
function memberToCandidate(member: MarkedDuplicateMember, sizeBytes: number): Candidate {
  return {
    id: member.candidateId,
    path: member.path,
    kind: "other",
    unit: "file",
    sizeBytes,
    lastAccessAt: "",
    createdAt: null,
    decision: member.keep ? "keep" : "undecided",
    explanation: "",
    alsoIn: [],
  };
}

/** ╳-екземпляри групи (кандидати на видалення) — те, що позначає «Прийняти». */
function reapMembers(group: MarkedDuplicateGroup): MarkedDuplicateMember[] {
  return group.members.filter((m) => !m.keep);
}

function acceptGroup(group: MarkedDuplicateGroup): void {
  selectionStore.markMultiple(
    reapMembers(group).map((m) => ({ id: m.candidateId, sizeBytes: group.size })),
  );
}

/** «Прийняти всі дефолти» (ui.md §4): одним кліком по всіх групах екрана. */
function acceptAllDefaults(groups: MarkedDuplicateGroup[]): void {
  selectionStore.markMultiple(
    groups.flatMap((g) => reapMembers(g).map((m) => ({ id: m.candidateId, sizeBytes: g.size }))),
  );
}

function GroupRow({ group }: { group: MarkedDuplicateGroup }) {
  // Реактивність до selectionStore (T-108): кнопка «Прийнято» одразу після
  // прийняття — той самий механізм, що вже рухає Reap Bar і плитки сітки.
  useMarkedSummary();
  const reap = reapMembers(group);
  const accepted = reap.length > 0 && reap.every((m) => selectionStore.isMarked(m.candidateId));

  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center gap-3 text-xs text-ink-dim">
        <span>
          ГРУПА · {fileName(group.members[0]?.path ?? "")} · {group.members.length} копії ·{" "}
          {formatBytes(group.size)} кожна
        </span>
        <button
          type="button"
          disabled={accepted}
          onClick={() => acceptGroup(group)}
          title="Позначити всі ╳-копії цієї групи до видалення"
          className={`ml-auto rounded px-2 py-0.5 text-xs font-medium transition-colors ${
            accepted
              ? "cursor-not-allowed border border-keep/30 text-keep/60"
              : "border border-line text-ink-dim hover:bg-panel-2 hover:text-ink"
          }`}
        >
          {accepted ? "Прийнято ✓" : "Прийняти ✓"}
        </button>
      </div>
      <div className="flex flex-wrap gap-2">
        {group.members.map((member) => (
          <div key={member.candidateId} className="w-40" title={member.path}>
            <CandidateTile
              candidate={memberToCandidate(member, group.size)}
              marked={!member.keep}
            />
          </div>
        ))}
      </div>
    </div>
  );
}

export function DuplicatesScreen() {
  const [ack, setAck] = useState<DuplicatesGroupsAck | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    command<DuplicatesGroupsAck>("duplicates.groups")
      .then((res) => {
        setAck(res);
        setError(null);
      })
      .catch((err) => setError(ipcErrorMessage(err)));
  }, []);

  useEffect(() => {
    load();
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    subscribe("duplicates.cascade_updated", () => load()).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [load]);

  const groups = ack?.groups ?? [];
  const state = ack?.state;

  // Реактивність до selectionStore (T-108) — «Прийняти всі дефолти» показує
  // «Прийнято» коли жодного ╳-екземпляра по всіх групах ще не лишилось.
  useMarkedSummary();
  const allReap = groups.flatMap(reapMembers);
  const allAccepted = allReap.length > 0 && allReap.every((m) => selectionStore.isMarked(m.candidateId));

  return (
    <div className="flex h-full flex-col">
      <div className="flex h-8 shrink-0 items-center gap-3 border-b border-line px-3 text-xs text-ink-dim">
        <span>Детектор:</span>
        <span className="text-ink-faint">побайтово ідентичні файли (BLAKE3)</span>
        {state?.refining ? <span className="text-ink-faint">· уточнюється…</span> : null}
        {groups.length > 0 ? (
          <span className="text-ink-faint">
            · {groups.length} {groups.length === 1 ? "група" : "груп"} ·{" "}
            {formatBytes(state?.reclaimableBytes ?? 0)}
          </span>
        ) : null}
        {groups.length > 0 ? (
          <button
            type="button"
            disabled={allAccepted}
            onClick={() => acceptAllDefaults(groups)}
            title="Позначити всі ╳-копії в усіх групах до видалення"
            className={`ml-auto rounded px-2 py-0.5 text-xs font-medium transition-colors ${
              allAccepted
                ? "cursor-not-allowed border border-keep/30 text-keep/60"
                : "border border-line text-ink-dim hover:bg-panel-2 hover:text-ink"
            }`}
          >
            {allAccepted ? "Усі прийнято ✓" : "Прийняти всі дефолти"}
          </button>
        ) : null}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        {error ? (
          <div className="text-sm text-ink-faint">{error}</div>
        ) : groups.length === 0 ? (
          <div className="flex h-full items-center justify-center text-sm text-ink-faint">
            {state && state.phase !== "idle"
              ? "Дублікатів не знайдено"
              : "Сітка кандидатів: Дублікати"}
          </div>
        ) : (
          <div className="flex flex-col gap-4">
            {groups.map((group) => (
              <GroupRow key={group.contentHash} group={group} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
