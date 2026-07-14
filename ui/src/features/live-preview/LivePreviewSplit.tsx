/**
 * Розкладка двох зон Live Preview (docs/ui.md §10.1/§10.5, T-139).
 *
 * Ліва зона — наявний host екранів (сітка + вся навігація, T-099), права —
 * суцільна зона превью без власних контролів (нуль втрат площі, §10.1). Між
 * ними перетяжна межа: тягнеться мишею, позиція запам'ятовується
 * (`livePreviewStore` → localStorage), тож праву зону можна виставити точно
 * під фізичний монітор. Саме превью (наведення=файл на весь екран) — T-140;
 * тут лише каркас розкладки.
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
  const { enabled, leftRatio } = useLivePreview();
  const containerRef = useRef<HTMLDivElement>(null);
  const draggingRef = useRef(false);

  // T-145: при вимкненні режиму скидаємо скраб-стан, щоб наступне
  // увімкнення не стартувало autoplay на застарілому candidateId.
  useEffect(() => {
    if (!enabled) livePreviewScrubStore.clear();
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

  return (
    <div ref={containerRef} className="flex min-w-0 flex-1">
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
            onPointerDown={startDrag}
            className="w-1 shrink-0 cursor-col-resize bg-line transition-colors hover:bg-accent"
          />
          <LivePreviewPane />
        </>
      ) : null}
    </div>
  );
}
