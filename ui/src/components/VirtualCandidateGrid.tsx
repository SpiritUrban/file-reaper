/**
 * Віртуалізована сітка плиток категорії.
 *
 * ⚠️  Геометрія й замір — ТІЛЬКИ в `./candidate-grid/`.
 *     Перед змінами прочитай `./candidate-grid/README.md`.
 *     Після змін: `npm --prefix ui run test:grid`
 */

import { useEffect, useMemo, useRef, useState } from "react";

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
  /**
   * Виділення прямокутною областю (гумка): тягнення з порожнього місця сітки
   * позначає всі плитки, що перетнула область. Передаються id перетнутих —
   * власник selectionStore (CategoryScreen) вирішує, як застосувати.
   */
  onMarqueeSelect?: (ids: number[]) => void;
  emptyTitle?: string;
}

interface MarqueeRect {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
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
  onMarqueeSelect,
  emptyTitle = "Немає кандидатів у цій категорії",
}: VirtualCandidateGridProps) {
  const viewportRef = useRef<HTMLDivElement>(null);
  const gridInnerRef = useRef<HTMLDivElement>(null);
  const frameRef = useRef<number | null>(null);
  const [viewport, setScrollTop] = useGridViewport(active, viewportRef);
  const [marquee, setMarquee] = useState<MarqueeRect | null>(null);

  // Гумка: старт лише з порожнього місця (не з плитки — там клік/тягнення).
  // Слухачі на window, щоб тягнути й за межі viewport; на mouseup — hit-test
  // прямокутника проти реально відрендерених плиток (віртуалізація — лише
  // видимі мають DOM, що й треба для екранної гумки).
  const beginMarquee = (event: React.MouseEvent) => {
    if (event.button !== 0 || !onMarqueeSelect) return;
    if ((event.target as HTMLElement).closest("[data-candidate-id]")) return;
    event.preventDefault();
    const start = { x: event.clientX, y: event.clientY };
    let moved = false;
    const onMove = (move: MouseEvent) => {
      if (!moved && Math.hypot(move.clientX - start.x, move.clientY - start.y) < 4) {
        return;
      }
      moved = true;
      setMarquee({ x0: start.x, y0: start.y, x1: move.clientX, y1: move.clientY });
    };
    const onUp = (up: MouseEvent) => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      setMarquee(null);
      if (!moved) return;
      const left = Math.min(start.x, up.clientX);
      const right = Math.max(start.x, up.clientX);
      const top = Math.min(start.y, up.clientY);
      const bottom = Math.max(start.y, up.clientY);
      const ids: number[] = [];
      gridInnerRef.current
        ?.querySelectorAll<HTMLElement>("[data-candidate-id]")
        .forEach((el) => {
          const r = el.getBoundingClientRect();
          const outside =
            r.right < left || r.left > right || r.bottom < top || r.top > bottom;
          if (outside) return;
          const id = Number(el.dataset.candidateId);
          if (Number.isFinite(id)) ids.push(id);
        });
      if (ids.length > 0) onMarqueeSelect(ids);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

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
      className="relative h-full overflow-y-auto overscroll-contain p-1"
      data-grid-active={active || undefined}
      data-columns={gridWindow.columns}
      onMouseDown={beginMarquee}
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
          ref={gridInnerRef}
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
      {marquee ? (
        <div
          className="pointer-events-none fixed z-50 rounded-sm border border-accent bg-accent/20"
          style={{
            left: Math.min(marquee.x0, marquee.x1),
            top: Math.min(marquee.y0, marquee.y1),
            width: Math.abs(marquee.x1 - marquee.x0),
            height: Math.abs(marquee.y1 - marquee.y0),
          }}
        />
      ) : null}
    </div>
  );
}
