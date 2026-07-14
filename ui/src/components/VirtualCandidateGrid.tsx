import { useEffect, useMemo, useRef, useState } from "react";

import type { Candidate } from "@/ipc/types";

import { CandidateTile, type CandidatePreview } from "./CandidateTile";
import { EmptyState } from "./EmptyState";

export type GridDensity = "compact" | "standard" | "large";

const MIN_TILE_WIDTH: Record<GridDensity, number> = {
  compact: 140,
  standard: 190,
  large: 260,
};
const GAP = 4;
const OVERSCAN_ROWS = 3;

export interface VirtualGridWindow {
  columns: number;
  rowHeight: number;
  totalHeight: number;
  startIndex: number;
  endIndex: number;
  offsetY: number;
}

/** Pure geometry used by the component and perf/invariant checks. */
export function calculateVirtualGridWindow(
  itemCount: number,
  width: number,
  viewportHeight: number,
  scrollTop: number,
  density: GridDensity,
): VirtualGridWindow {
  const safeWidth = Math.max(1, width);
  const columns = Math.max(
    1,
    Math.floor((safeWidth + GAP) / (MIN_TILE_WIDTH[density] + GAP)),
  );
  const tileWidth = (safeWidth - GAP * (columns - 1)) / columns;
  const rowHeight = Math.max(1, tileWidth * 0.75 + GAP);
  const rowCount = Math.ceil(itemCount / columns);
  const firstVisibleRow = Math.min(
    Math.max(0, rowCount - 1),
    Math.max(0, Math.floor(scrollTop / rowHeight)),
  );
  const visibleRows = Math.max(1, Math.ceil(viewportHeight / rowHeight));
  const startRow = Math.max(0, firstVisibleRow - OVERSCAN_ROWS);
  const endRow = Math.min(
    rowCount,
    firstVisibleRow + visibleRows + OVERSCAN_ROWS,
  );

  return {
    columns,
    rowHeight,
    totalHeight: Math.max(0, rowCount * rowHeight - GAP),
    startIndex: startRow * columns,
    endIndex: Math.min(itemCount, endRow * columns),
    offsetY: startRow * rowHeight,
  };
}

export interface VirtualCandidateGridProps {
  candidates: Candidate[];
  density?: GridDensity;
  focusedId?: number | null;
  previewFor?: (candidate: Candidate) => CandidatePreview | undefined;
  /** Локальне оптимістичне позначення (T-116) — переважає candidate.decision. */
  isMarked?: (candidate: Candidate) => boolean;
  /** CSS-курсор плиток (T-142): іконка озброєної дії у Live Preview. */
  tileCursor?: string;
  /** Плитка з коротким пульс-відгуком після дії (T-143) та клас її кільця. */
  flash?: { id: number; ring: string } | null;
  onActivate?: (candidate: Candidate, event: React.MouseEvent) => void;
  /** ПКМ по плитці (T-143): у Live Preview → протилежна озброєна дія. */
  onSecondaryActivate?: (candidate: Candidate, event: React.MouseEvent) => void;
  onFocusCandidate?: (candidate: Candidate) => void;
  /** Наведення курсора (T-140): у Live Preview → велике превью праворуч. */
  onHoverCandidate?: (candidate: Candidate) => void;
  /** Поточна кількість колонок геометрії (T-123: клавіатурна навігація ↑/↓). */
  onColumnsChange?: (columns: number) => void;
  emptyTitle?: string;
}

export function VirtualCandidateGrid({
  candidates,
  density = "standard",
  focusedId = null,
  previewFor,
  isMarked,
  tileCursor,
  flash,
  onActivate,
  onSecondaryActivate,
  onFocusCandidate,
  onHoverCandidate,
  onColumnsChange,
  emptyTitle = "Немає кандидатів у цій категорії",
}: VirtualCandidateGridProps) {
  const viewportRef = useRef<HTMLDivElement>(null);
  const frameRef = useRef<number | null>(null);
  const [viewport, setViewport] = useState({
    width: 1,
    height: 1,
    scrollTop: 0,
  });

  useEffect(() => {
    const element = viewportRef.current;
    if (!element) return;

    const measure = () => {
      setViewport((current) => ({
        width: Math.max(1, element.clientWidth - GAP * 2),
        height: element.clientHeight,
        scrollTop: current.scrollTop,
      }));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  useEffect(
    () => () => {
      if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
    },
    [],
  );

  const window = useMemo(
    () =>
      calculateVirtualGridWindow(
        candidates.length,
        viewport.width,
        viewport.height,
        viewport.scrollTop,
        density,
      ),
    [candidates.length, density, viewport],
  );
  const visible = candidates.slice(window.startIndex, window.endIndex);

  // T-123: батько (CategoryScreen) рахує ↑/↓ від кількості колонок —
  // геометрія віртуалізації живе тут, назовні віддаємо лише число.
  useEffect(() => {
    onColumnsChange?.(window.columns);
  }, [window.columns, onColumnsChange]);

  // T-123: клавіатурна навігація може перевести фокус за межі відрендереного
  // вікна — підскролити так, щоб рядок фокусу лишався видимим.
  useEffect(() => {
    if (focusedId == null) return;
    const element = viewportRef.current;
    if (!element) return;
    const index = candidates.findIndex((c) => c.id === focusedId);
    if (index === -1) return;
    const row = Math.floor(index / window.columns);
    const rowTop = row * window.rowHeight;
    const rowBottom = rowTop + window.rowHeight;
    if (rowTop < element.scrollTop) {
      element.scrollTop = rowTop;
    } else if (rowBottom > element.scrollTop + element.clientHeight) {
      element.scrollTop = rowBottom - element.clientHeight;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusedId, candidates, window.columns, window.rowHeight]);

  if (candidates.length === 0) {
    return <EmptyState title={emptyTitle} taskRef="category.window" />;
  }

  return (
    <div
      ref={viewportRef}
      className="h-full overflow-y-auto overscroll-contain p-1"
      onScroll={(event) => {
        const scrollTop = event.currentTarget.scrollTop;
        if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
        frameRef.current = requestAnimationFrame(() => {
          frameRef.current = null;
          setViewport((current) => ({ ...current, scrollTop }));
        });
      }}
    >
      <div className="relative" style={{ height: window.totalHeight }}>
        <div
          className="absolute inset-x-0 top-0 grid gap-1"
          style={{
            gridTemplateColumns: `repeat(${window.columns}, minmax(0, 1fr))`,
            transform: `translateY(${window.offsetY}px)`,
          }}
          data-visible-start={window.startIndex}
          data-visible-end={window.endIndex}
          data-total={candidates.length}
        >
          {visible.map((candidate) => {
            const preview = previewFor?.(candidate);
            return (
              <CandidateTile
                key={candidate.id}
                candidate={candidate}
                focused={candidate.id === focusedId}
                {...(preview ? { preview } : {})}
                {...(isMarked ? { marked: isMarked(candidate) } : {})}
                {...(tileCursor ? { cursor: tileCursor } : {})}
                {...(flash?.id === candidate.id ? { flashRing: flash.ring } : {})}
                {...(onActivate ? { onActivate } : {})}
                {...(onSecondaryActivate ? { onSecondaryActivate } : {})}
                {...(onFocusCandidate ? { onFocusCandidate } : {})}
                {...(onHoverCandidate ? { onHoverCandidate } : {})}
              />
            );
          })}
        </div>
      </div>
    </div>
  );
}