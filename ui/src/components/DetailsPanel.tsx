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
 * Блок дат створення/зміни і дії [Позначити]/[Залишити]/[Папка] — T-125.
 */

import { useEffect } from "react";

import { hotkeys, type HotkeyActionEventDetail } from "@/hotkeys";
import { formatBytes } from "@/store/format";
import { categoryTitle } from "@/store/categories";
import { detailsPanelStore, useDetailsPanelCandidate } from "@/store/detailsPanel";
import { useLargePreview, useVideoScrub } from "@/store/preview";

function fileName(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  return normalized.split("/").filter(Boolean).at(-1) || path;
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

  if (!candidate) return null;

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
      </div>

      {/* Метадані (дати) + [Позначити] [Залишити] [Папка] — T-125 */}
    </aside>
  );
}
