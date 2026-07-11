/**
 * Sidebar — список категорій-детекторів з обсягами (docs/ui.md §1).
 * Живі обсяги — проєкція подій cleanup.total_updated / category.updated
 * (T-105); категорії відсортовані за вагою, порожні — приглушені.
 * Згортання `[`, бейдж Quarantine і блок дисків — T-106.
 */

import { NavLink } from "react-router-dom";

import { AnimatedBytes } from "@/components/AnimatedCounter";
import { Meter } from "@/components/Meter";
import { CATEGORIES, type CategoryDescriptor } from "@/store/categories";
import { useAppState } from "@/store/appState";
import type { CategorySummary } from "@/ipc/types";

const itemBase =
  "flex items-center gap-2 rounded px-2 py-1.5 text-sm hover:bg-panel-2";
const itemActive = "bg-panel-2 text-ink";
const itemIdle = "text-ink-dim";
/** Порожня категорія: приглушена, але лишається клікабельною. */
const itemEmpty = "text-ink-faint";

interface CategoryRow {
  descriptor: CategoryDescriptor;
  summary: CategorySummary | undefined;
}

/**
 * Порядок Sidebar: непорожні категорії за вагою (спадання байтів),
 * порожні — у каталожному порядку в кінці. Сортування стабільне,
 * тож рівні обсяги не «стрибають» між подіями скану.
 */
function categoryRowsByWeight(live: CategorySummary[]): CategoryRow[] {
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

export function Sidebar() {
  const { cleanup, scanRunning } = useAppState();
  const rows = categoryRowsByWeight(cleanup.categories);
  const hasTotal = cleanup.reclaimableBytes > 0;

  return (
    <aside className="flex w-56 shrink-0 flex-col border-r border-line bg-panel">
      {/* Логотип */}
      <div className="flex items-center gap-2 px-3 py-3">
        <span className="text-quarantine">◉</span>
        <span className="font-semibold tracking-wide">TrashRadar</span>
      </div>

      {/* Головний екран: Cleanup з живою цифрою; ⟳ — активний скан */}
      <nav className="flex flex-col gap-0.5 px-2">
        <NavLink
          to="/"
          end
          className={({ isActive }) =>
            `${itemBase} ${isActive ? itemActive : itemIdle}`
          }
        >
          <span>▦</span>
          <span className="flex-1">Cleanup</span>
          {scanRunning ? (
            <span
              className="inline-block animate-spin text-xs text-accent"
              aria-label="сканування триває"
            >
              ⟳
            </span>
          ) : null}
          {hasTotal ? (
            <AnimatedBytes
              value={cleanup.reclaimableBytes}
              className="font-mono text-xs text-ink-dim"
            />
          ) : (
            <span className="font-mono text-xs text-ink-faint">—</span>
          )}
        </NavLink>
      </nav>

      <div className="mx-3 my-2 border-t border-line" />

      {/* Категорії за вагою: найважча зверху, порожні приглушені (T-105) */}
      <nav className="flex flex-1 flex-col gap-0.5 overflow-y-auto px-2">
        {rows.map(({ descriptor, summary }) => {
          const totalBytes = summary?.totalBytes ?? 0;
          const isEmpty = totalBytes === 0;
          return (
            <NavLink
              key={descriptor.id}
              to={`/category/${descriptor.id}`}
              className={({ isActive }) =>
                `${itemBase} ${isActive ? itemActive : isEmpty ? itemEmpty : itemIdle}`
              }
            >
              <span className="w-4 text-center">{descriptor.glyph}</span>
              <span className="flex-1 truncate">{descriptor.title}</span>
              {isEmpty ? (
                <span className="font-mono text-xs text-ink-faint">—</span>
              ) : (
                <AnimatedBytes
                  value={totalBytes}
                  className="font-mono text-xs text-ink-dim"
                />
              )}
            </NavLink>
          );
        })}
      </nav>

      <div className="mx-3 my-2 border-t border-line" />

      {/* Quarantine з бейджем (живий лічильник — T-106) */}
      <nav className="px-2">
        <NavLink
          to="/quarantine"
          className={({ isActive }) =>
            `${itemBase} ${isActive ? "bg-panel-2 text-quarantine" : "text-quarantine/80"}`
          }
        >
          <span>☣</span>
          <span className="flex-1">Quarantine</span>
          <span className="font-mono text-xs">—</span>
        </NavLink>
      </nav>

      <div className="mx-3 my-2 border-t border-line" />

      {/* Диски: статус і рескан, НЕ навігація по вмісту (нуль браузингу) */}
      <div className="flex flex-col gap-2 px-4 pb-2">
        <div className="flex items-center gap-2 text-xs text-ink-dim">
          <span className="font-mono">C:</span>
          <Meter fraction={null} />
          <span className="font-mono text-ink-faint">—%</span>
        </div>
      </div>

      <div className="mx-3 my-1 border-t border-line" />

      <nav className="px-2 pb-2">
        <NavLink
          to="/health"
          className={({ isActive }) =>
            `${itemBase} ${isActive ? itemActive : itemIdle}`
          }
        >
          <span>↯</span>
          <span>Health</span>
        </NavLink>
        <NavLink
          to="/settings"
          className={({ isActive }) =>
            `${itemBase} ${isActive ? itemActive : itemIdle}`
          }
        >
          <span>⛭</span>
          <span>Налаштування</span>
        </NavLink>
      </nav>
    </aside>
  );
}
