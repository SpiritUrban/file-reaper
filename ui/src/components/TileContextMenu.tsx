/**
 * Контекстне меню плитки (ПКМ у сітці категорій / дублікатів). Один екземпляр
 * на застосунок, змонтований в AppLayout; ціль і позиція — у `tileContextMenuStore`.
 *
 * Пункти: копіювати шлях / ім'я, відкрити в папці (reveal_in_explorer, T-125),
 * Залишити (Keep, T-117), відправити у Quarantine (reap.execute одним файлом,
 * T-138 — той самий шлях, що й Reap-оверлей: тост із «Скасувати» + reapedStore).
 */

import { useEffect, useLayoutEffect, useRef, useState } from "react";

import { command, ipcErrorMessage } from "@/ipc/client";
import { copyToClipboard } from "@/store/clipboard";
import { formatBytes } from "@/store/format";
import { keepCandidate } from "@/store/keep";
import { reapedStore } from "@/store/reaped";
import { selectionStore } from "@/store/selection";
import { tileContextMenuStore, useTileContextMenu } from "@/store/tileContextMenu";
import { toast } from "@/store/toasts";
import type { Candidate, ReapExecuteAck, QuarantineRestoreOutcome } from "@/ipc/types";

function fileName(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  return normalized.split("/").filter(Boolean).at(-1) || path;
}

async function copyWithToast(text: string, label: string): Promise<void> {
  const ok = await copyToClipboard(text);
  toast(
    ok
      ? { message: `${label} скопійовано`, tone: "success" }
      : { message: "Не вдалося скопіювати", tone: "warning" },
  );
}

function revealInExplorer(candidate: Candidate): void {
  void command("candidate.reveal_in_explorer", {
    payload: { candidateId: candidate.id },
  }).catch((error) => toast({ message: ipcErrorMessage(error), tone: "warning" }));
}

/** Один файл → Quarantine (той самий канал, що й Reap-оверлей T-138). */
function reapOne(candidate: Candidate): void {
  const ids = [candidate.id];
  command<ReapExecuteAck>("reap.execute", { payload: { candidateIds: ids } })
    .then((ack) => {
      reapedStore.reap(ids);
      selectionStore.unmark(candidate.id);
      toast({
        message: `${formatBytes(ack.reapedBytes)} у Quarantine`,
        tone: "success",
        action: {
          label: "Скасувати",
          run: () =>
            command<QuarantineRestoreOutcome[]>("reap.undo_batch", {
              payload: { batchId: ack.batchId },
            })
              .then(() => {
                reapedStore.unreap(ids);
                toast({ message: "Відновлено з Quarantine", tone: "success" });
              })
              .catch((error) => {
                toast({ message: ipcErrorMessage(error), tone: "warning" });
              }),
        },
      });
    })
    .catch((error) => toast({ message: ipcErrorMessage(error), tone: "warning" }));
}

interface MenuItem {
  label: string;
  run: (candidate: Candidate) => void;
  danger?: boolean;
}

const ITEMS: MenuItem[] = [
  {
    label: "Скопіювати шлях",
    run: (c) => void copyWithToast(c.path, "Шлях"),
  },
  {
    label: "Скопіювати ім'я файлу",
    run: (c) => void copyWithToast(fileName(c.path), "Ім'я"),
  },
  { label: "Відкрити в папці", run: revealInExplorer },
  { label: "Залишити (Keep)", run: (c) => void keepCandidate(c) },
  { label: "Відправити у Quarantine", run: reapOne, danger: true },
];

const MENU_WIDTH = 224;

export function TileContextMenu() {
  const state = useTileContextMenu();
  const menuRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  // Позиція: клампимо в межі вікна, щоб меню не вилазило за край.
  useLayoutEffect(() => {
    if (!state) {
      setPos(null);
      return;
    }
    const height = menuRef.current?.offsetHeight ?? ITEMS.length * 36 + 8;
    const left = Math.min(state.x, window.innerWidth - MENU_WIDTH - 8);
    const top = Math.min(state.y, window.innerHeight - height - 8);
    setPos({ left: Math.max(8, left), top: Math.max(8, top) });
  }, [state]);

  // Закриття: клік поза меню, Esc, скрол, зміна розміру.
  useEffect(() => {
    if (!state) return;
    const close = () => tileContextMenuStore.close();
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    const onPointerDown = (event: PointerEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) close();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
    };
  }, [state]);

  if (!state) return null;

  const run = (item: MenuItem) => {
    item.run(state.candidate);
    tileContextMenuStore.close();
  };

  return (
    <div
      ref={menuRef}
      role="menu"
      className="fixed z-[60] w-56 overflow-hidden rounded-md border border-line bg-panel py-1 text-sm text-ink shadow-lg"
      style={{
        left: pos?.left ?? state.x,
        top: pos?.top ?? state.y,
        visibility: pos ? "visible" : "hidden",
      }}
      onContextMenu={(event) => event.preventDefault()}
    >
      <div className="truncate border-b border-line/60 px-3 py-1.5 font-mono text-xs text-ink-faint">
        {fileName(state.candidate.path)}
      </div>
      {ITEMS.map((item) => (
        <button
          key={item.label}
          type="button"
          role="menuitem"
          onClick={() => run(item)}
          className={`block w-full px-3 py-1.5 text-left transition-colors hover:bg-panel-2 ${
            item.danger ? "text-reap hover:text-reap" : "text-ink"
          }`}
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}
