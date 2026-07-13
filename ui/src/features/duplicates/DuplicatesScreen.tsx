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
 * «Прийняти» на групу / «Прийняти всі дефолти» (T-127) і клік по копії, щоб
 * змінити ✓ (T-128), — окремі задачі; тут кнопок дії навмисно немає.
 */

import { useCallback, useEffect, useState } from "react";

import { CandidateTile } from "@/components/CandidateTile";
import { command, ipcErrorMessage, subscribe } from "@/ipc/client";
import { formatBytes } from "@/store/format";
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

function GroupRow({ group }: { group: MarkedDuplicateGroup }) {
  return (
    <div className="flex flex-col gap-1.5">
      <div className="text-xs text-ink-dim">
        ГРУПА · {fileName(group.members[0]?.path ?? "")} · {group.members.length} копії ·{" "}
        {formatBytes(group.size)} кожна
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
