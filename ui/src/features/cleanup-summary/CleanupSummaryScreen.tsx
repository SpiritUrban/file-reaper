/**
 * Cleanup Summary — головний екран (docs/ui.md §3).
 * «Цифра — перша»: N ГБ можна звільнити + ряди категорій.
 * Каркас: розмітка без даних; жива цифра — T-110, ряди — T-111.
 */

import { Link } from "react-router-dom";

import { Meter } from "@/components/Meter";
import { CATEGORIES } from "@/store/categories";

export function CleanupSummaryScreen() {
  return (
    <div className="flex h-full flex-col px-6 py-4">
      {/* Головна цифра — найбільший текст у продукті (T-110) */}
      <div className="flex items-baseline justify-between py-6">
        <h1 className="text-5xl font-bold tracking-tight">
          <span className="font-mono">— ГБ</span>{" "}
          <span className="text-ink-dim text-3xl">можна звільнити</span>
        </h1>
        <span className="text-xs text-ink-faint">скан не запускався</span>
      </div>

      {/* Ряди категорій (T-111); мініпревью і [Позначити все] — T-111/T-112 */}
      <div className="flex flex-col divide-y divide-line border-y border-line">
        {CATEGORIES.map((category) => (
          <Link
            key={category.id}
            to={`/category/${category.id}`}
            className="group flex items-center gap-3 py-2.5 hover:bg-panel"
          >
            <span className="w-5 text-center text-ink-dim">{category.glyph}</span>
            <span className="w-40 truncate text-sm font-medium uppercase tracking-wide">
              {category.title}
            </span>
            <span className="w-28 font-mono text-sm text-ink-dim">— · — ф.</span>
            <div className="w-40">
              <Meter fraction={null} />
            </div>
            <div className="flex-1" />
            <span className="text-ink-faint group-hover:text-ink">→</span>
          </Link>
        ))}
      </div>

      <div className="flex-1" />

      {/* Пульс Quarantine (T-113) */}
      <div className="flex items-center gap-3 border-t border-line py-3 text-sm">
        <span className="text-quarantine">☣</span>
        <span className="text-ink-dim">
          Quarantine: <span className="font-mono">— файлів · —</span>
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
