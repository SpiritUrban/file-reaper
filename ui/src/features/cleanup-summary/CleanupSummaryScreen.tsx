/**
 * Cleanup Summary — головний екран (docs/ui.md §3).
 * «Цифра — перша»: N ГБ можна звільнити + ряди категорій (T-110/T-111).
 * T-110: жива головна цифра з анімацією (AnimatedBytes/AnimatedInteger).
 * T-111: ряди категорій з обсягами, кількістю файлів, смужками прогресу.
 * T-113: пульс Quarantine внизу екрана.
 */

import { Link } from "react-router-dom";
import { useState, useEffect } from "react";

import { AnimatedBytes, AnimatedInteger } from "@/components/AnimatedCounter";
import { Meter } from "@/components/Meter";
import { categoryRowsByWeight, type CategoryDescriptor } from "@/store/categories";
import { useAppState } from "@/store/appState";
import { fetchCategoryTopCandidates, fetchCategoryAllCandidates } from "@/store/categoryWindow";
import { markAllCandidates } from "@/store/selection";
import type { CategorySummary, CandidatePreview } from "@/ipc/types";

/** Гліф для типу файла мініпревью (T-111). */
function fileKindGlyph(kind: string): string {
  const glyphs: Record<string, string> = {
    Video: "▶",
    Image: "🖼",
    Audio: "♫",
    Archive: "▤",
    Installer: "⬇",
    DiskImage: "📀",
    Document: "📄",
    Other: "◻",
  };
  return glyphs[kind] || "◻";
}

/** Компонент рядка категорії з мініпревью (T-111). */
function CategoryRow({
  descriptor,
  summary,
  totalBytes,
}: {
  descriptor: CategoryDescriptor;
  summary: CategorySummary | undefined;
  totalBytes: number;
}) {
  const isEmpty = !summary || summary.totalBytes === 0;
  const isSafeToBulk = summary?.safety === "safe_to_bulk";
  const [topCandidates, setTopCandidates] = useState<CandidatePreview[]>([]);
  const [markingAll, setMarkingAll] = useState(false);

  useEffect(() => {
    if (isEmpty) return;
    fetchCategoryTopCandidates(descriptor.id, 6).then(setTopCandidates);
  }, [descriptor.id, isEmpty]);

  const handleMarkAll = async (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setMarkingAll(true);
    try {
      const candidates = await fetchCategoryAllCandidates(descriptor.id);
      markAllCandidates(candidates);
    } catch (error) {
      console.warn(`Failed to mark all candidates for ${descriptor.id}:`, error);
    } finally {
      setMarkingAll(false);
    }
  };

  const itemClass = isEmpty
    ? "text-ink-faint"
    : "text-ink-dim hover:text-ink hover:bg-panel";

  return (
    <Link
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

      {/* Обсяг (animated) + кількість файлів */}
      <span className="w-28 font-mono text-sm">
        {isEmpty ? (
          "—"
        ) : (
          <AnimatedBytes value={summary!.totalBytes} />
        )}
      </span>
      <span className="text-xs text-ink-faint">
        {isEmpty
          ? "—"
          : `${summary!.itemCount} ${
              summary!.countUnit === "files"
                ? "ф."
                : summary!.countUnit === "groups"
                  ? "гр."
                  : "папок"
            }`}
      </span>

      {/* Смужка прогресу */}
      <div className="w-32">
        <Meter
          fraction={
            isEmpty || totalBytes === 0
              ? 0
              : summary!.totalBytes / totalBytes
          }
        />
      </div>

      {/* Мініпревью — 4–6 найбільших файлів (T-111) */}
      <div className="flex gap-1">
        {!isEmpty &&
          topCandidates.slice(0, 6).map((candidate) => (
            <div
              key={candidate.id}
              className="flex h-6 w-6 flex-shrink-0 items-center justify-center rounded bg-panel-2 text-xs text-ink-dim hover:bg-accent hover:text-ink transition-colors"
              title={`${candidate.kind}`}
            >
              {fileKindGlyph(candidate.kind)}
            </div>
          ))}
      </div>

      {/* Кнопка "Позначити все" для safe-to-bulk категорій (T-112) */}
      {!isEmpty && isSafeToBulk && (
        <button
          onClick={handleMarkAll}
          disabled={markingAll}
          className="ml-2 rounded px-2 py-1 text-xs text-ink-faint hover:bg-accent hover:text-ink disabled:opacity-50 transition-colors"
          title="Позначити всю категорію для видалення"
        >
          {markingAll ? "..." : "✓"}
        </button>
      )}

      {/* Стрілочка наведення */}
      <div className="flex-1" />
      {!isEmpty && (
        <span className="text-ink-faint group-hover:text-ink">→</span>
      )}
    </Link>
  );
}

export function CleanupSummaryScreen() {
  const { cleanup, scanRunning, status, quarantine, settings } = useAppState();
  // Спільний порядок зі Sidebar (T-105); вимкнені детектори приховано (T-152).
  const rows = categoryRowsByWeight(cleanup.categories, settings);
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
        {rows.map(({ descriptor, summary }) => (
          <CategoryRow
            key={descriptor.id}
            descriptor={descriptor}
            summary={summary}
            totalBytes={cleanup.reclaimableBytes}
          />
        ))}
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
                {quarantine.nextPurgeAtUnix > 0 && (
                  <>
                    {" "}· автоочищення{" "}
                    {new Date(quarantine.nextPurgeAtUnix * 1000).toLocaleDateString("uk-UA")}
                  </>
                )}
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
