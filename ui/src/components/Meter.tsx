/** Тонка горизонтальна смужка-частка (диски в Sidebar, ряди категорій). */

interface MeterProps {
  /** Частка 0..1; null = дані ще не надійшли. */
  fraction: number | null;
  /** Смужка «гаряча» — напр., диск заповнений > 85%. */
  hot?: boolean;
}

export function Meter({ fraction, hot = false }: MeterProps) {
  const width = fraction === null ? 0 : Math.min(100, Math.max(0, fraction * 100));
  return (
    <div className="h-1 w-full overflow-hidden rounded-full bg-panel-2">
      <div
        className={`h-full rounded-full ${hot ? "bg-heat-3" : "bg-accent"}`}
        style={{ width: `${width}%` }}
      />
    </div>
  );
}
