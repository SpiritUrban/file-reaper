/**
 * Замір viewport сітки з урахуванням T-099 (приховані CategoryScreen).
 *
 * ⚠️  Не підміняти ad-hoc ResizeObserver у VirtualCandidateGrid.
 *     Політика 0-замірів — лише `applyViewportMeasure` у geometry.ts.
 */

import { useLayoutEffect, useState, type RefObject } from "react";

import {
  applyViewportMeasure,
  defaultViewport,
  type ViewportSize,
} from "./geometry";

/**
 * @param active — екран видимий (не display:none). Коли false, замір
 *   не чіпає стан. Коли стає true — effect перезапускається.
 * @param elementRef — scroll-контейнер сітки (h-full).
 */
export function useGridViewport(
  active: boolean,
  elementRef: RefObject<HTMLElement | null>,
): [ViewportSize, (scrollTop: number) => void] {
  const [viewport, setViewport] = useState<ViewportSize>(defaultViewport);

  useLayoutEffect(() => {
    if (!active) return;
    const element = elementRef.current;
    if (!element) return;

    const measure = () => {
      setViewport((current) => {
        const next = applyViewportMeasure(
          current,
          element.clientWidth,
          element.clientHeight,
        );
        return next ?? current;
      });
    };

    measure();
    const raf = requestAnimationFrame(measure);
    const timer = window.setTimeout(measure, 50);
    const observer = new ResizeObserver(measure);
    observer.observe(element);

    return () => {
      cancelAnimationFrame(raf);
      clearTimeout(timer);
      observer.disconnect();
    };
  }, [active, elementRef]);

  const setScrollTop = (scrollTop: number) => {
    setViewport((current) =>
      current.scrollTop === scrollTop ? current : { ...current, scrollTop },
    );
  };

  return [viewport, setScrollTop];
}
