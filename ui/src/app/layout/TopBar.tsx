/**
 * Верхня панель (docs/ui.md §2): контекст → фільтри → сортування →
 * пошук → Reap Bar. Каркас: статичні зони; фільтри-чипси — T-107,
 * живий Reap Bar — T-108, пошук — T-109.
 */

import { AnimatedBytes, AnimatedInteger } from "@/components/AnimatedCounter";

interface TopBarProps {
  /** Контекст зліва: назва екрана або категорії. */
  context: string;
  markedCount?: number;
  markedBytes?: number;
}

export function TopBar({
  context,
  markedCount = 0,
  markedBytes = 0,
}: TopBarProps) {
  return (
    <header className="flex h-11 shrink-0 items-center gap-3 border-b border-line bg-panel px-3">
      {/* Контекст */}
      <div className="flex items-center gap-2 text-sm">
        <span className="text-ink">{context}</span>
        <button
          type="button"
          className="rounded px-1 text-ink-dim hover:bg-panel-2"
          title="Пересканувати"
        >
          ⟳
        </button>
      </div>

      {/* Фільтр-чипси (T-107) */}
      <div className="flex items-center gap-1 text-xs text-ink-dim">
        <span className="rounded-full border border-line px-2 py-0.5">Розмір ▾</span>
        <span className="rounded-full border border-line px-2 py-0.5">Вік ▾</span>
        <span className="rounded-full border border-line px-2 py-0.5">Диск ▾</span>
      </div>

      <div className="flex-1" />

      {/* Пошук (T-109) */}
      <span className="font-mono text-xs text-ink-faint">🔍 /</span>

      {/* Reap Bar (T-108) */}
      <div className="flex items-center gap-2">
        <span className="text-xs text-ink-dim">
          Позначено:{" "}
          <span className="font-mono">
            <AnimatedInteger value={markedCount} /> ·{" "}
            <AnimatedBytes value={markedBytes} />
          </span>
        </span>
        <button
          type="button"
          disabled={markedCount === 0}
          className="rounded bg-reap/20 px-3 py-1 text-sm font-semibold text-reap opacity-50"
        >
          REAP
        </button>
      </div>
    </header>
  );
}
