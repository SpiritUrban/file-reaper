/**
 * Плейсхолдер порожнього стану екрана-каркаса: пояснює, якою задачею
 * беклогу екран наповнюється. Зникає в міру реалізації M1–M5.
 */

interface EmptyStateProps {
  title: string;
  taskRef: string;
}

export function EmptyState({ title, taskRef }: EmptyStateProps) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-2">
      <p className="text-ink-dim">{title}</p>
      <p className="font-mono text-xs text-ink-faint">
        наповнюється задачами {taskRef} · docs/tasks.md
      </p>
    </div>
  );
}
