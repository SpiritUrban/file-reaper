/**
 * Розкладка двох зон Live Preview (docs/ui.md §10.1/§10.5/§10.6, T-139/T-149).
 *
 * Ліва зона — host екранів (сітка + навігація, T-099), права — суцільне
 * превью. Перетяжна межа: пропорція **окремо** для одно- й дводисплейного
 * режиму (T-149): на одному моніторі дефолт right ≈ 55% (left 45%); на
 * розтягнутому вікні — 50/50, кожен слот у localStorage.
 */

import {
  useCallback,
  useEffect,
  useRef,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";

import { livePreviewStore, useLivePreview } from "@/store/livePreview";
import { livePreviewScrubStore } from "@/store/livePreviewScrub";

import { LivePreviewActionBar } from "./LivePreviewActionBar";
import { LivePreviewPane } from "./LivePreviewPane";

export function LivePreviewSplit({ children }: { children: ReactNode }) {
  const { enabled, leftRatio, singleDisplay } = useLivePreview();
  const containerRef = useRef<HTMLDivElement>(null);
  const draggingRef = useRef(false);

  // T-145: при вимкненні режиму скидаємо скраб-стан, щоб наступне
  // увімкнення не стартувало autoplay на застарілому candidateId.
  useEffect(() => {
    if (!enabled) livePreviewScrubStore.clear();
  }, [enabled]);

  // T-149: при увімкненні LP і на resize — підтягнути single/dual слот.
  useEffect(() => {
    if (enabled) livePreviewStore.refreshDisplayMode();
  }, [enabled]);

  // Межа лівої зони рахується від власного контейнера спліту (виключає
  // Sidebar і панель деталей) — тож частка завжди узгоджена з видимою
  // шириною зон, незалежно від згорнутого Sidebar (T-105).
  const onPointerMove = useCallback((event: globalThis.PointerEvent) => {
    if (!draggingRef.current) return;
    const rect = containerRef.current?.getBoundingClientRect();
    if (!rect || rect.width === 0) return;
    livePreviewStore.setLeftRatio((event.clientX - rect.left) / rect.width);
  }, []);

  const endDrag = useCallback(() => {
    draggingRef.current = false;
    window.removeEventListener("pointermove", onPointerMove);
    window.removeEventListener("pointerup", endDrag);
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
  }, [onPointerMove]);

  const startDrag = useCallback(
    (event: ReactPointerEvent) => {
      event.preventDefault();
      draggingRef.current = true;
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      window.addEventListener("pointermove", onPointerMove);
      window.addEventListener("pointerup", endDrag);
    },
    [onPointerMove, endDrag],
  );

  const leftStyle = enabled
    ? { flex: `0 0 ${leftRatio * 100}%`, minWidth: 0 }
    : undefined;
  const rightPct = Math.round((1 - leftRatio) * 100);

  return (
    <div
      ref={containerRef}
      className="flex min-w-0 flex-1"
      data-lp-display={singleDisplay ? "single" : "dual"}
      data-lp-right-pct={enabled ? rightPct : undefined}
    >
      <div className={`flex min-w-0 flex-col ${enabled ? "" : "flex-1"}`} style={leftStyle}>
        {/* Панель дій — зверху лівої зони (§10.5), лише в режимі (T-142). */}
        {enabled ? <LivePreviewActionBar /> : null}
        {children}
      </div>
      {enabled ? (
        <>
          <div
            role="separator"
            aria-orientation="vertical"
            aria-label="Перетягнути межу превью"
            aria-valuenow={rightPct}
            aria-valuetext={
              singleDisplay
                ? `Один монітор: превью ${rightPct}%`
                : `Кілька моніторів: превью ${rightPct}%`
            }
            onPointerDown={startDrag}
            className="w-1 shrink-0 cursor-col-resize bg-line transition-colors hover:bg-accent"
          />
          <LivePreviewPane />
        </>
      ) : null}
    </div>
  );
}
