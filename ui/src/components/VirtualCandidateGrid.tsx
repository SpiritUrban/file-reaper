/**
 * Віртуалізована сітка плиток категорії.
 *
 * ⚠️  Геометрія й замір — ТІЛЬКИ в `./candidate-grid/`.
 *     Перед змінами прочитай `./candidate-grid/README.md`.
 *     Після змін: `npm --prefix ui run test:grid`
 */

import { useEffect, useMemo, useRef } from "react";

import type { Candidate } from "@/ipc/types";

import {
  calculateVirtualGridWindow,
  shouldRenderTiles,
  type GridDensity,
} from "./candidate-grid/geometry";
import { useGridViewport } from "./candidate-grid/useGridViewport";
import { CandidateTile, type CandidatePreview } from "./CandidateTile";
import { EmptyState } from "./EmptyState";

export type { GridDensity } from "./candidate-grid/geometry";
export { calculateVirtualGridWindow } from "./candidate-grid/geometry";

export interface VirtualCandidateGridProps {
  candidates: Candidate[];
  density?: GridDensity;
  focusedId?: number | null;
  /**
   * Екран видимий (не `display:none`). Обовʼязково з CategoryScreen:
   * `active={pathname === `/category/${id}`}`.
   * Без цього замір на mount прихованого екрана ламає сітку.
   */
  active?: boolean;
  previewFor?: (candidate: Candidate) => CandidatePreview | undefined;
  isMarked?: (candidate: Candidate) => boolean;
  isKept?: (candidate: Candidate) => boolean;
  tileCursor?: string;
  flash?: { id: number; ring: string } | null;
  onActivate?: (candidate: Candidate, event: React.MouseEvent) => void;
  onSecondaryActivate?: (candidate: Candidate, event: React.MouseEvent) => void;
  onFocusCandidate?: (candidate: Candidate) => void;
  onHoverCandidate?: (candidate: Candidate) => void;
  onHoverMove?: (candidate: Candidate, ratio: number) => void;
  onColumnsChange?: (columns: number) => void;
  emptyTitle?: string;
}

export function VirtualCandidateGrid({
  candidates,
  density = "standard",
  focusedId = null,
  active = true,
  previewFor,
  isMarked,
  isKept,
  tileCursor,
  flash,
  onActivate,
  onSecondaryActivate,
  onFocusCandidate,
  onHoverCandidate,
  onHoverMove,
  onColumnsChange,
  emptyTitle = "Немає кандидатів у цій категорії",
}: VirtualCandidateGridProps) {
  const viewportRef = useRef<HTMLDivElement>(null);
  const frameRef = useRef<number | null>(null);
  const [viewport, setScrollTop] = useGridViewport(active, viewportRef);

  useEffect(
    () => () => {
      if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
    },
    [],
  );

  const gridWindow = useMemo(
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

  const renderTiles = shouldRenderTiles(active, candidates.length);
  const visible = renderTiles
    ? candidates.slice(gridWindow.startIndex, gridWindow.endIndex)
    : [];

  useEffect(() => {
    if (active) onColumnsChange?.(gridWindow.columns);
  }, [active, gridWindow.columns, onColumnsChange]);

  useEffect(() => {
    if (!active || focusedId == null) return;
    const element = viewportRef.current;
    if (!element) return;
    const index = candidates.findIndex((c) => c.id === focusedId);
    if (index === -1) return;
    const row = Math.floor(index / gridWindow.columns);
    const rowTop = row * gridWindow.rowHeight;
    const rowBottom = rowTop + gridWindow.rowHeight;
    if (rowTop < element.scrollTop) {
      element.scrollTop = rowTop;
    } else if (rowBottom > element.scrollTop + element.clientHeight) {
      element.scrollTop = rowBottom - element.clientHeight;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, focusedId, candidates, gridWindow.columns, gridWindow.rowHeight]);

  if (candidates.length === 0) {
    return <EmptyState title={emptyTitle} taskRef="category.window" />;
  }

  return (
    <div
      ref={viewportRef}
      className="h-full overflow-y-auto overscroll-contain p-1"
      data-grid-active={active || undefined}
      data-columns={gridWindow.columns}
      onScroll={(event) => {
        const scrollTop = event.currentTarget.scrollTop;
        if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
        frameRef.current = requestAnimationFrame(() => {
          frameRef.current = null;
          setScrollTop(scrollTop);
        });
      }}
    >
      <div className="relative" style={{ height: gridWindow.totalHeight }}>
        <div
          className="absolute inset-x-0 top-0 grid gap-1"
          style={{
            gridTemplateColumns: `repeat(${gridWindow.columns}, minmax(0, 1fr))`,
            transform: `translateY(${gridWindow.offsetY}px)`,
          }}
          data-visible-start={gridWindow.startIndex}
          data-visible-end={gridWindow.endIndex}
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
                {...(isKept ? { kept: isKept(candidate) } : {})}
                {...(tileCursor ? { cursor: tileCursor } : {})}
                {...(flash?.id === candidate.id
                  ? { flashRing: flash.ring }
                  : {})}
                {...(onActivate ? { onActivate } : {})}
                {...(onSecondaryActivate ? { onSecondaryActivate } : {})}
                {...(onFocusCandidate ? { onFocusCandidate } : {})}
                {...(onHoverCandidate ? { onHoverCandidate } : {})}
                {...(onHoverMove ? { onHoverMove } : {})}
              />
            );
          })}
        </div>
      </div>
    </div>
  );
}
