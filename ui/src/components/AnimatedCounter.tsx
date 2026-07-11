import { useEffect, useRef, useState } from "react";

import { formatBytes } from "@/store/format";

const DEFAULT_DURATION_MS = 360;

function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

/**
 * rAF tween that restarts from the currently painted value. Rapid Core deltas
 * therefore continue smoothly instead of flashing back to the previous target.
 */
export function useAnimatedNumber(
  target: number,
  durationMs = DEFAULT_DURATION_MS,
): number {
  const safeTarget = Number.isFinite(target) ? target : 0;
  const [displayed, setDisplayed] = useState(safeTarget);
  const displayedRef = useRef(safeTarget);

  useEffect(() => {
    if (prefersReducedMotion() || durationMs <= 0) {
      displayedRef.current = safeTarget;
      setDisplayed(safeTarget);
      return;
    }

    const startValue = displayedRef.current;
    const delta = safeTarget - startValue;
    if (delta === 0) return;

    let frame = 0;
    let startedAt: number | null = null;
    const tick = (now: number) => {
      startedAt ??= now;
      const progress = Math.min(1, (now - startedAt) / durationMs);
      const eased = 1 - (1 - progress) ** 3;
      const next = startValue + delta * eased;
      displayedRef.current = next;
      setDisplayed(next);
      if (progress < 1) frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [durationMs, safeTarget]);

  return displayed;
}

interface AnimatedCounterProps {
  value: number;
  className?: string;
  durationMs?: number;
}

export function AnimatedInteger({
  value,
  className,
  durationMs,
}: AnimatedCounterProps) {
  const displayed = useAnimatedNumber(value, durationMs);
  return (
    <span className={className} aria-label={Math.round(value).toLocaleString("uk-UA")}>
      {Math.round(displayed).toLocaleString("uk-UA")}
    </span>
  );
}

export function AnimatedBytes({
  value,
  className,
  durationMs,
}: AnimatedCounterProps) {
  const displayed = useAnimatedNumber(value, durationMs);
  return (
    <span className={className} aria-label={formatBytes(value)}>
      {formatBytes(Math.max(0, Math.round(displayed)))}
    </span>
  );
}