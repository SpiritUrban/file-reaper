/**
 * Верхня панель (docs/ui.md §2): контекст → фільтри → пошук → Reap Bar.
 * T-107: живий контекст категорії (обсяг · кількість) і фільтр-чипси
 * Розмір/Тип/Вік/Диск — випадайки з пресетами, активний чип заповнений
 * і знімається хрестиком; чипси комбінуються (AND у store/filters).
 * Живий Reap Bar — T-108, пошук — T-109. Кнопка REAP відкриває оверлей
 * підтвердження (T-135) — `reapOverlayStore`, той самий патерн, що й
 * `detailsPanelStore` (T-123).
 */

import { useEffect, useRef, useState } from "react";

import { AnimatedBytes, AnimatedInteger } from "@/components/AnimatedCounter";
import { command, ipcErrorMessage } from "@/ipc/client";
import { formatBytes } from "@/store/format";
import { toast } from "@/store/toasts";
import { useAppState } from "@/store/appState";
import {
  AGE_PRESETS,
  KIND_LABELS,
  SIZE_PRESETS,
  candidateFilterStore,
  useCandidateFilters,
} from "@/store/filters";
import { livePreviewStore, useLivePreview } from "@/store/livePreview";
import { reapOverlayStore } from "@/store/reapOverlay";
import { searchStore, useSearchState } from "@/store/search";
import type { CategoryId, FileKind, ScanStartAck } from "@/ipc/types";

interface TopBarProps {
  /** Контекст зліва: назва екрана або категорії. */
  context: string;
  /** Активна категорія — вмикає чипси і живий підпис обсягу (T-107). */
  categoryId?: CategoryId | null;
  markedCount?: number;
  markedBytes?: number;
}

interface ChipOption {
  label: string;
  onSelect: () => void;
}

/**
 * Чип одного виміру фільтра: неактивний — контурний з випадайкою пресетів;
 * активний — заповнений, клік по назві знову відкриває пресети, ✕ знімає.
 */
function FilterChip({
  title,
  active,
  options,
  onClear,
}: {
  title: string;
  active: string | null;
  options: ChipOption[];
  onClear: () => void;
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: MouseEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("mousedown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  return (
    <div ref={rootRef} className="relative">
      {active === null ? (
        <button
          type="button"
          onClick={() => setOpen((value) => !value)}
          aria-haspopup="menu"
          aria-expanded={open}
          className="rounded-full border border-line px-2 py-0.5 text-xs text-ink-dim hover:bg-panel-2 hover:text-ink"
        >
          {title} ▾
        </button>
      ) : (
        <span className="flex items-center gap-1 rounded-full border border-accent/40 bg-accent/15 px-2 py-0.5 text-xs text-ink">
          <button
            type="button"
            onClick={() => setOpen((value) => !value)}
            aria-haspopup="menu"
            aria-expanded={open}
            title={`Змінити фільтр «${title}»`}
          >
            {active}
          </button>
          <button
            type="button"
            onClick={onClear}
            aria-label={`Зняти фільтр «${title}»`}
            className="text-ink-dim hover:text-ink"
          >
            ✕
          </button>
        </span>
      )}
      {open ? (
        <div
          role="menu"
          className="absolute left-0 top-full z-20 mt-1 min-w-40 rounded border border-line bg-panel-2 py-1 shadow-lg"
        >
          {options.length === 0 ? (
            <div className="px-3 py-1 text-xs text-ink-faint">
              немає даних
            </div>
          ) : (
            options.map((option) => (
              <button
                key={option.label}
                type="button"
                role="menuitem"
                onClick={() => {
                  option.onSelect();
                  setOpen(false);
                }}
                className="block w-full px-3 py-1 text-left text-xs text-ink-dim hover:bg-panel hover:text-ink"
              >
                {option.label}
              </button>
            ))
          )}
        </div>
      ) : null}
    </div>
  );
}

/** Одиниця лічильника категорії: файли/групи/папки. */
function countUnitLabel(unit: "files" | "groups" | "folders"): string {
  return unit === "groups" ? "груп" : unit === "folders" ? "папок" : "ф.";
}

/** Рестарт скану всіх томів — статус/рескан, не навігація (ui.md §2). */
function rescanAll(): void {
  void command<ScanStartAck>("scan.start").catch((error) =>
    toast({ message: ipcErrorMessage(error), tone: "warning" }),
  );
}

/** Інпут пошуку: видимий при T-109 active, Escape закриває (ui.md §2). */
function SearchBox({ categoryId }: { categoryId: CategoryId | null }) {
  const search = useSearchState(categoryId);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!categoryId) return;
    const onHotkey = (event: Event) => {
      const { action } = (event as CustomEvent).detail;
      if (action === "search") {
        event.preventDefault();
        inputRef.current?.focus();
        searchStore.setActive(categoryId, true);
      }
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && search.active) {
        searchStore.clear(categoryId);
      }
    };
    window.addEventListener("trashradar:hotkey", onHotkey);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("trashradar:hotkey", onHotkey);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [categoryId, search.active]);

  if (!categoryId || !search.active) {
    return <span className="font-mono text-xs text-ink-faint">🔍 /</span>;
  }

  return (
    <input
      ref={inputRef}
      type="text"
      placeholder="Пошук по шляху…"
      value={search.query}
      onChange={(event) => searchStore.setQuery(categoryId, event.currentTarget.value)}
      onBlur={() => searchStore.setActive(categoryId, search.query.length > 0)}
      autoFocus
      className="w-40 rounded bg-panel-2 px-2 py-0.5 text-xs text-ink placeholder:text-ink-faint focus:outline-none focus:ring-1 focus:ring-accent"
    />
  );
}

export function TopBar({
  context,
  categoryId = null,
  markedCount = 0,
  markedBytes = 0,
}: TopBarProps) {
  const { cleanup, volumes } = useAppState();
  const livePreview = useLivePreview();
  const filters = useCandidateFilters(categoryId);
  const summary = categoryId
    ? cleanup.categories.find((category) => category.id === categoryId)
    : undefined;

  const sizeActive =
    filters.minSizeBytes === null
      ? null
      : (SIZE_PRESETS.find((preset) => preset.bytes === filters.minSizeBytes)
          ?.label ?? `>${formatBytes(filters.minSizeBytes)}`);
  const ageActive =
    filters.minAgeDays === null
      ? null
      : (AGE_PRESETS.find((preset) => preset.days === filters.minAgeDays)
          ?.label ?? `${filters.minAgeDays}+ дн`);
  const kindActive = filters.kind === null ? null : KIND_LABELS[filters.kind];

  return (
    <header className="flex h-11 shrink-0 items-center gap-3 border-b border-line bg-panel px-3">
      {/* Контекст: категорія · обсяг · кількість (живі, T-107) */}
      <div className="flex items-center gap-2 text-sm">
        <span className="text-ink">{context}</span>
        {summary ? (
          <span className="font-mono text-xs text-ink-dim">
            · <AnimatedBytes value={summary.totalBytes} /> ·{" "}
            <AnimatedInteger value={summary.itemCount} />{" "}
            {countUnitLabel(summary.countUnit)}
          </span>
        ) : null}
        <button
          type="button"
          onClick={rescanAll}
          className="rounded px-1 text-ink-dim hover:bg-panel-2"
          title="Пересканувати"
          aria-label="Пересканувати"
        >
          ⟳
        </button>
      </div>

      {/* Фільтр-чипси: лише в контексті категорії (ui.md §2) */}
      {categoryId ? (
        <div className="flex items-center gap-1">
          <FilterChip
            title="Розмір"
            active={sizeActive}
            options={SIZE_PRESETS.map((preset) => ({
              label: preset.label,
              onSelect: () =>
                candidateFilterStore.patch(categoryId, {
                  minSizeBytes: preset.bytes,
                }),
            }))}
            onClear={() =>
              candidateFilterStore.patch(categoryId, { minSizeBytes: null })
            }
          />
          <FilterChip
            title="Тип"
            active={kindActive}
            options={(Object.keys(KIND_LABELS) as FileKind[]).map((kind) => ({
              label: KIND_LABELS[kind],
              onSelect: () => candidateFilterStore.patch(categoryId, { kind }),
            }))}
            onClear={() => candidateFilterStore.patch(categoryId, { kind: null })}
          />
          <FilterChip
            title="Вік"
            active={ageActive}
            options={AGE_PRESETS.map((preset) => ({
              label: preset.label,
              onSelect: () =>
                candidateFilterStore.patch(categoryId, {
                  minAgeDays: preset.days,
                }),
            }))}
            onClear={() =>
              candidateFilterStore.patch(categoryId, { minAgeDays: null })
            }
          />
          <FilterChip
            title="Диск"
            active={filters.volume}
            options={volumes.map((volume) => ({
              label: volume.volume,
              onSelect: () =>
                candidateFilterStore.patch(categoryId, {
                  volume: volume.volume,
                }),
            }))}
            onClear={() =>
              candidateFilterStore.patch(categoryId, { volume: null })
            }
          />
        </div>
      ) : null}

      <div className="flex-1" />

      {/* Пошук (T-109): хоткей `/` фокусує, Escape закриває, infix по path */}
      <SearchBox categoryId={categoryId} />

      {/* Live Preview (T-139, ui.md §10.1): вмикає двомоніторний режим —
          той самий стан, що й `P` (livePreviewStore). */}
      <button
        type="button"
        onClick={() => livePreviewStore.toggle()}
        aria-pressed={livePreview.enabled}
        title="Live Preview (P)"
        aria-label="Live Preview"
        className={`rounded px-2 py-1 text-sm transition-colors ${
          livePreview.enabled
            ? "bg-accent/20 text-ink"
            : "text-ink-dim hover:bg-panel-2 hover:text-ink"
        }`}
      >
        ▐▐
      </button>

      {/* Reap Bar (T-108): спільний кошик сесії, живі лічильники T-102 */}
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
          onClick={() => reapOverlayStore.open()}
          title={
            markedCount === 0
              ? "Немає позначених кандидатів"
              : "Переглянути перед відправкою у Quarantine (Ctrl+Enter)"
          }
          className={`rounded px-3 py-1 text-sm font-semibold transition-colors ${
            markedCount === 0
              ? "cursor-not-allowed bg-reap/15 text-reap/50"
              : "bg-reap text-bg hover:bg-reap/85"
          }`}
        >
          REAP
        </button>
      </div>
    </header>
  );
}
