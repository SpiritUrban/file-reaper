/**
 * Панель деталей (T-123/T-124, docs/ui.md §6): немодальна колонка праворуч —
 * сітка лишається видимою й керованою (не рендериться поверх, а поруч,
 * у AppLayout). `Esc` закриває через контекст хоткеїв "details" (T-103):
 * реєстр уже мав дію `dismiss` на цей контекст, бракувало лише того, хто
 * його активує і хто на неї реагує.
 *
 * T-124: велике превью — `useLargePreview` (draft з кешу <100 мс, sharp
 * підміняє його), для відео — той самий `useVideoScrub`, що й на плитці
 * (T-120), лише більша площа наведення.
 *
 * T-125: блок дат (створено/останній доступ) + [Позначити]/[Залишити]/[Папка].
 * Дії дублюють клавіші сітки (DoD) — «Позначити» і «Залишити» пишуть у ті самі
 * спільні сесійні стори `selectionStore`/`keepStore`, що й Space/K у
 * `CategoryScreen` (T-116/T-117), тому Reap Bar і плитки реагують ідентично.
 * «Папка» — нова команда Core `candidate.reveal_in_explorer`.
 */

import { useEffect } from "react";

import { hotkeys, type HotkeyActionEventDetail } from "@/hotkeys";
import { command, ipcErrorMessage } from "@/ipc/client";
import { formatBytes } from "@/store/format";
import { categoryTitle } from "@/store/categories";
import { detailsPanelStore, useDetailsPanelCandidate } from "@/store/detailsPanel";
import { useLargePreview, useVideoScrub } from "@/store/preview";
import { selectionStore, useMarkedSummary } from "@/store/selection";
import { keepCandidate, useKeptIds } from "@/store/keep";
import { toast } from "@/store/toasts";
import type { Candidate } from "@/ipc/types";

function fileName(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  return normalized.split("/").filter(Boolean).at(-1) || path;
}

/** `null`/невалідне значення — чесний «—», не фальшива дата (T-125). */
function formatDate(iso: string | null): string {
  if (!iso) return "—";
  const parsed = new Date(iso);
  return Number.isNaN(parsed.getTime()) ? "—" : parsed.toLocaleDateString("uk-UA");
}

async function revealInExplorer(candidate: Candidate): Promise<void> {
  try {
    await command("candidate.reveal_in_explorer", {
      payload: { candidateId: candidate.id },
    });
  } catch (error) {
    toast({ message: ipcErrorMessage(error), tone: "warning" });
  }
}

export function DetailsPanel() {
  const candidate = useDetailsPanelCandidate();

  // Контекст "details" (T-103) активний лише поки панель відкрита — Esc
  // резолвиться в дію dismiss (нижче), не конфліктує з grid-хоткеями.
  useEffect(() => {
    if (candidate) hotkeys.activate("details");
    else hotkeys.deactivate("details");
    return () => hotkeys.deactivate("details");
  }, [candidate]);

  useEffect(() => {
    const onHotkey = (event: Event) => {
      const { action } = (event as CustomEvent<HotkeyActionEventDetail>).detail;
      if (action === "dismiss") detailsPanelStore.close();
    };
    window.addEventListener("trashradar:hotkey", onHotkey);
    return () => window.removeEventListener("trashradar:hotkey", onHotkey);
  }, []);

  // T-124: draft/sharp великого превью і скраб — хуки викликаються завжди
  // (правило hooks), самі толерантні до candidate === null.
  const largeSrc = useLargePreview(candidate);
  const { displaySrc, scrubbing, handleMouseMove, handleMouseLeave } =
    useVideoScrub(candidate, largeSrc);

  // T-125: реактивність до тих самих сесійних сторів, що й сітка (T-108/T-117) —
  // кнопки нижче показують актуальний стан незалежно від того, де саме
  // (плитка чи ця панель) позначення/keep відбулися.
  useMarkedSummary();
  const keptIds = useKeptIds();

  if (!candidate) return null;

  const marked = selectionStore.isMarked(candidate.id) || candidate.decision === "marked";
  const kept = keptIds.has(candidate.id) || candidate.decision === "keep";

  return (
    <aside
      className="flex w-80 shrink-0 flex-col overflow-y-auto border-l border-line bg-panel"
      aria-label="Панель деталей"
    >
      <div className="flex items-center justify-between border-b border-line px-3 py-2">
        <span className="text-xs font-semibold tracking-wide text-ink-dim">
          ДЕТАЛІ
        </span>
        <button
          type="button"
          onClick={() => detailsPanelStore.close()}
          aria-label="Закрити панель деталей (Esc)"
          title="Закрити (Esc)"
          className="rounded px-1.5 text-ink-faint hover:bg-panel-2 hover:text-ink"
        >
          ✕
        </button>
      </div>

      {/* Велике превью — T-124: draft з кешу <100 мс, sharp підміняє;
          для відео рух курсора по X скрабить уже отриману смугу кадрів. */}
      <div
        className="flex aspect-[4/3] items-center justify-center overflow-hidden border-b border-line bg-panel-2 text-ink-faint"
        onMouseMove={handleMouseMove}
        onMouseLeave={handleMouseLeave}
        data-scrubbing={scrubbing || undefined}
      >
        {displaySrc ? (
          <img
            src={displaySrc}
            alt={fileName(candidate.path)}
            className="h-full w-full object-contain"
            draggable={false}
          />
        ) : (
          <span className="text-xs">завантаження превью…</span>
        )}
      </div>

      <div className="flex flex-col gap-1.5 px-3 py-3">
        <div className="break-all text-sm font-medium text-ink">
          {fileName(candidate.path)}
        </div>
        <div className="break-all font-mono text-xs text-ink-dim">
          {candidate.path}
        </div>
        <div className="font-mono text-sm text-ink">
          {formatBytes(candidate.sizeBytes)}
        </div>
        <div className="text-xs text-ink-faint">{candidate.explanation}</div>
        {candidate.alsoIn.length > 0 ? (
          <div className="text-xs text-ink-faint">
            Також у: {candidate.alsoIn.map(categoryTitle).join(", ")}
          </div>
        ) : null}

        {/* Метадані (дати, ui.md §6): createdAt — null, якщо Core не зміг
            прочитати дату створення (напр. FAT-том) — чесний «—». */}
        <div className="mt-1 flex flex-col gap-0.5 border-t border-line pt-2 font-mono text-xs text-ink-dim">
          <span>Створено: {formatDate(candidate.createdAt)}</span>
          <span>Останній доступ: {formatDate(candidate.lastAccessAt)}</span>
        </div>
      </div>

      {/* Дії (T-125): ті самі клавіші, що в сітці (Space/K), працюють і тут —
          панель не «краде» фокус (T-103 контекст "details" лише для Esc). */}
      <div className="mt-auto flex gap-2 border-t border-line px-3 py-2">
        <button
          type="button"
          onClick={() => selectionStore.toggle(candidate)}
          aria-pressed={marked}
          title="Позначити до видалення (Space у сітці)"
          className={`flex-1 rounded px-2 py-1.5 text-xs font-medium transition-colors ${
            marked
              ? "bg-reap text-bg hover:bg-reap/85"
              : "border border-line text-ink-dim hover:bg-panel-2 hover:text-ink"
          }`}
        >
          {marked ? "Позначено ╳" : "Позначити"}
        </button>
        <button
          type="button"
          disabled={kept}
          onClick={() => {
            void keepCandidate(candidate);
            detailsPanelStore.close();
          }}
          title={kept ? "Уже залишено" : "Залишити (K у сітці)"}
          className={`flex-1 rounded px-2 py-1.5 text-xs font-medium transition-colors ${
            kept
              ? "cursor-not-allowed border border-keep/30 text-keep/50"
              : "border border-keep/60 text-keep hover:bg-keep/10"
          }`}
        >
          {kept ? "Залишено ✓" : "Залишити"}
        </button>
        <button
          type="button"
          onClick={() => void revealInExplorer(candidate)}
          title="Показати у провіднику"
          className="flex-1 rounded border border-line px-2 py-1.5 text-xs font-medium text-ink-dim transition-colors hover:bg-panel-2 hover:text-ink"
        >
          Папка
        </button>
      </div>
    </aside>
  );
}
