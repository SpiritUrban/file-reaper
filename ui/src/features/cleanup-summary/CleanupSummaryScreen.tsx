/**
 * Cleanup Summary — головний екран (docs/ui.md §3).
 * «Цифра — перша»: N ГБ можна звільнити + ряди категорій (T-110/T-111).
 * T-110: жива головна цифра з анімацією (AnimatedBytes/AnimatedInteger).
 * T-111: ряди категорій з обсягами, кількістю файлів, смужками прогресу.
 * T-113: пульс Quarantine внизу екрана.
 */

import { Link } from "react-router-dom";

import { AnimatedBytes, AnimatedInteger } from "@/components/AnimatedCounter";
import { Meter } from "@/components/Meter";
import { CATEGORIES } from "@/store/categories";
import { useAppState } from "@/store/appState";
import type { CategorySummary } from "@/ipc/types";

/**
 * Порядок Sidebar: непорожні категорії за вагою (спадання байтів),
 * порожні — у каталожному порядку в кінці. Жорсткий порядок = нема стрибків.
 */
function categoryRowsByWeight(live: CategorySummary[]) {
  const byId = new Map(live.map((summary) => [summary.id, summary]));
  return CATEGORIES.map((descriptor, catalogIndex) => ({
    descriptor,
    summary: byId.get(descriptor.id),
    catalogIndex,
  }))
    .sort((left, right) => {
      const leftBytes = left.summary?.totalBytes ?? 0;
      const rightBytes = right.summary?.totalBytes ?? 0;
      if (leftBytes !== rightBytes) return rightBytes - leftBytes;
      return left.catalogIndex - right.catalogIndex;
    })
    .map(({ descriptor, summary }) => ({ descriptor, summary }));
}

export function CleanupSummaryScreen() {
  const { cleanup, scanRunning, status, quarantine } = useAppState();
  const rows = categoryRowsByWeight(cleanup.categories);
  const hasTotal = cleanup.reclaimableBytes > 0;
  const hasQuarantine = quarantine.heldCount > 0;

  return (
    <div className="flex h-full flex-col px-6 py-4">
      {/* Головна цифра — найбільший текст у продукті (T-110) */}
      <div className="flex items-baseline justify-between py-6">
        <h1 className="text-5xl font-bold tracking-tight">
          <AnimatedBytes value={cleanup.reclaimableBytes} className="font-mono" />{" "}
          <span className="text-ink-dim text-3xl">можна звільнити</span>
        </h1>
        <span className="text-xs text-ink-faint">
          {scanRunning
            ? "сканування…"
            : status === "hydrating"
              ? "відновлення стану…"
              : hasTotal
                ? (
                    <>
                      <AnimatedInteger value={cleanup.uniqueFiles} /> кандидатів
                    </>
                  )
                : "—"}
        </span>
      </div>

      {/* Ряди категорій з живими обсягами (T-111) */}
      <div className="flex flex-col divide-y divide-line border-y border-line overflow-y-auto">
        {rows.map(({ descriptor, summary }) => {
          const isEmpty = !summary || summary.totalBytes === 0;
          const itemClass = isEmpty
            ? "text-ink-faint"
            : "text-ink-dim hover:text-ink hover:bg-panel";

          return (
            <Link
              key={descriptor.id}
              to={isEmpty ? "#" : `/category/${descriptor.id}`}
              onClick={(e) => isEmpty && e.preventDefault()}
              className={`group flex items-center gap-3 py-2.5 transition-colors ${itemClass}`}
            >
              {/* Іконка категорії */}
              <span className="w-5 text-center text-ink-dim">{descriptor.glyph}</span>

              {/* Назва категорії */}
              <span className="w-40 truncate text-sm font-medium uppercase tracking-wide">
                {descriptor.title}
              </span>

              {/* Обсяг (animated) + кількість файлів (T-110 AnimatedBytes + T-111 count) */}
              <span className="w-28 font-mono text-sm">
                {isEmpty ? (
                  "—"
                ) : (
                  <AnimatedBytes value={summary.totalBytes} />
                )}
              </span>
              <span className="text-xs text-ink-faint">
                {isEmpty
                  ? "—"
                  : `${summary.itemCount} ${
                      summary.countUnit === "files"
                        ? "ф."
                        : summary.countUnit === "groups"
                          ? "гр."
                          : "папок"
                    }`}
              </span>

              {/* Смужка прогресу (часткаот загального) */}
              <div className="w-32">
                <Meter
                  fraction={
                    isEmpty || cleanup.reclaimableBytes === 0
                      ? 0
                      : summary.totalBytes / cleanup.reclaimableBytes
                  }
                />
              </div>

              {/* Стрілочка наведення */}
              <div className="flex-1" />
              {!isEmpty && (
                <span className="text-ink-faint group-hover:text-ink">→</span>
              )}
            </Link>
          );
        })}
      </div>

      <div className="flex-1" />

      {/* Пульс Quarantine внизу екрана (T-113) */}
      <div className="flex items-center gap-3 border-t border-line py-3 text-sm">
        <span className={hasQuarantine ? "text-quarantine" : "text-ink-faint"}>
          ☣
        </span>
        <span className={hasQuarantine ? "text-ink-dim" : "text-ink-faint"}>
          Quarantine:{" "}
          <span className="font-mono">
            {hasQuarantine ? (
              <>
                <AnimatedInteger value={quarantine.heldCount} /> файлів ·{" "}
                <AnimatedBytes value={quarantine.heldBytes} />
              </>
            ) : (
              "—"
            )}
          </span>
        </span>
        <div className="flex-1" />
        <Link
          to="/quarantine"
          className="rounded border border-line px-3 py-1 text-xs text-ink-dim hover:bg-panel-2"
        >
          Переглянути
        </Link>
      </div>
    </div>
  );
}
