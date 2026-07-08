/**
 * Sidebar — список категорій-детекторів з обсягами (docs/ui.md §1).
 * Каркас: статичний довідник без живих даних; обсяги («—») підключаються
 * до подій cleanup.total_updated у T-105/T-106.
 */

import { NavLink } from "react-router-dom";

import { Meter } from "@/components/Meter";
import { CATEGORIES } from "@/store/categories";

const itemBase =
  "flex items-center gap-2 rounded px-2 py-1.5 text-sm hover:bg-panel-2";
const itemActive = "bg-panel-2 text-ink";
const itemIdle = "text-ink-dim";

export function Sidebar() {
  return (
    <aside className="flex w-56 shrink-0 flex-col border-r border-line bg-panel">
      {/* Логотип */}
      <div className="flex items-center gap-2 px-3 py-3">
        <span className="text-quarantine">◉</span>
        <span className="font-semibold tracking-wide">TrashRadar</span>
      </div>

      {/* Головний екран: Cleanup з живою цифрою */}
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
          <span className="font-mono text-xs text-ink-faint">— ГБ</span>
        </NavLink>
      </nav>

      <div className="mx-3 my-2 border-t border-line" />

      {/* Категорії: порядок стане «за вагою» після підключення даних (T-105) */}
      <nav className="flex flex-1 flex-col gap-0.5 overflow-y-auto px-2">
        {CATEGORIES.map((category) => (
          <NavLink
            key={category.id}
            to={`/category/${category.id}`}
            className={({ isActive }) =>
              `${itemBase} ${isActive ? itemActive : itemIdle}`
            }
          >
            <span className="w-4 text-center">{category.glyph}</span>
            <span className="flex-1 truncate">{category.title}</span>
            <span className="font-mono text-xs text-ink-faint">—</span>
          </NavLink>
        ))}
      </nav>

      <div className="mx-3 my-2 border-t border-line" />

      {/* Quarantine з бейджем */}
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
