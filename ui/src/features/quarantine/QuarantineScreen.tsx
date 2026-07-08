/**
 * Quarantine — «передсмертна зона» у бурштиновій темі (docs/ui.md §7).
 * Каркас: рамка режиму + порожня сітка. Наповнення — T-130…T-134.
 */

import { EmptyState } from "@/components/EmptyState";

export function QuarantineScreen() {
  return (
    <div className="flex h-full flex-col border-t-2 border-quarantine/60">
      <div className="flex h-8 shrink-0 items-center gap-3 border-b border-line px-3 text-xs">
        <span className="text-quarantine">— файлів · — · найближче автознищення: —</span>
        <div className="flex-1" />
        <button
          type="button"
          disabled
          className="rounded border border-line px-2 py-0.5 text-keep opacity-50"
        >
          Відновити позначені
        </button>
        <button
          type="button"
          disabled
          className="rounded border border-line px-2 py-0.5 text-reap opacity-50"
        >
          Знищити позначені
        </button>
      </div>
      <div className="min-h-0 flex-1">
        <EmptyState title="Сітка карантину" taskRef="T-130…T-134" />
      </div>
    </div>
  );
}
