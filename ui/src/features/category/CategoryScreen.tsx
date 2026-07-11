/**
 * Екран категорії — сітка кандидатів (docs/ui.md §5).
 * T-115: рядок детектора (пояснення правила + редаговані пороги предикатних
 * детекторів) і сітка живляться `category.window`; зміна порога шле
 * `category.set_threshold` → Core перераховує з індексу без рескану →
 * подія `settings.changed` провокує рефетч (без ручного стейт-менеджменту).
 * Фільтр-чипси TopBar звужують кандидатів через store/filters (T-107).
 * T-116: позначення клік/Space (toggle), Shift-клік/Shift+Space (діапазон
 * від останнього toggle до поточного) і A (все видиме) — усі три способи
 * пишуть в один спільний `selectionStore` (T-108), тож Reap Bar і плитки
 * оновлюються ідентично й миттєво.
 * T-117: K на сфокусованій плитці — Keep; ховає файл з усіх категорій сесії
 * одразу (спільний `keepStore`) і персистентно на боці Core (`candidate.keep`).
 * Дублікати групами — T-126.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { useLocation } from "react-router-dom";

import { VirtualCandidateGrid } from "@/components/VirtualCandidateGrid";
import type { HotkeyActionEventDetail } from "@/hotkeys";
import { useAppState } from "@/store/appState";
import { categoryRule, categoryTitle } from "@/store/categories";
import { useCategoryWindow } from "@/store/categoryWindow";
import {
  CATEGORY_THRESHOLDS,
  effectiveThreshold,
  setCategoryThreshold,
  type ThresholdFieldConfig,
} from "@/store/detectorThresholds";
import {
  applyCandidateFilters,
  hasActiveFilters,
  useCandidateFilters,
} from "@/store/filters";
import { applySearchQuery, useSearchState } from "@/store/search";
import { selectionStore, useMarkedSummary } from "@/store/selection";
import { keepCandidate, useKeptIds } from "@/store/keep";
import type { Candidate, CategoryId } from "@/ipc/types";

interface CategoryScreenProps {
  categoryId: CategoryId;
}

/** Один редагований поріг у рядку детектора (T-115). */
function ThresholdInput({
  categoryId,
  field,
  value,
}: {
  categoryId: CategoryId;
  field: ThresholdFieldConfig;
  value: number;
}) {
  const displayValue = field.unit === "size_mib" ? Math.round(value / (1024 * 1024)) : value;
  const [draft, setDraft] = useState(String(displayValue));
  const [saving, setSaving] = useState(false);

  const commit = async () => {
    const parsed = Number(draft);
    if (!Number.isFinite(parsed) || parsed <= 0) {
      setDraft(String(displayValue));
      return;
    }
    const bytesOrDays = field.unit === "size_mib" ? Math.round(parsed) * 1024 * 1024 : Math.round(parsed);
    if (bytesOrDays === value) return;
    setSaving(true);
    try {
      await setCategoryThreshold(categoryId, field.key, bytesOrDays);
      // Сітка перебудується автоматично через settings.changed → useCategoryWindow refetch.
    } catch (error) {
      console.warn(`Failed to set threshold ${field.key} for ${categoryId}:`, error);
      setDraft(String(displayValue));
    } finally {
      setSaving(false);
    }
  };

  return (
    <label className="flex items-center gap-1 text-ink-faint">
      <span>{field.label}:</span>
      <input
        type="number"
        min={1}
        value={draft}
        disabled={saving}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        }}
        className="w-16 border-b border-line bg-transparent px-1 font-mono text-ink-dim focus:border-accent focus:outline-none"
      />
      <span>{field.unit === "size_mib" ? "МіБ" : "дн."}</span>
    </label>
  );
}

export function CategoryScreen({ categoryId }: CategoryScreenProps) {
  const title = categoryTitle(categoryId);
  const rule = categoryRule(categoryId);
  const { settings } = useAppState();
  const filters = useCandidateFilters(categoryId);
  const search = useSearchState(categoryId);
  // settings — нова посилання на кожну settings.changed подію (T-098), тому
  // зміна порога детектора перебудовує сітку без ручного тригера (T-115 DoD).
  const { candidates: fetched } = useCategoryWindow(categoryId, settings);
  // T-117: Keep ховає файл з усіх категорій сесії одразу — навіть якщо цю
  // категорію завантажено раніше і сервер про Keep у ній ще не знає.
  const keptIds = useKeptIds();
  const candidates = useMemo(
    () => fetched.filter((c) => !keptIds.has(c.id)),
    [fetched, keptIds],
  );
  const visible = useMemo(
    () => {
      const filtered = applyCandidateFilters(candidates, filters);
      return applySearchQuery(filtered, search.query);
    },
    [candidates, filters, search.query],
  );
  const hasFilters = hasActiveFilters(filters);
  const hasSearch = search.query.length > 0;
  const thresholdFields = CATEGORY_THRESHOLDS[categoryId] ?? [];

  // T-099: усі 9 CategoryScreen змонтовані постійно, приховані CSS-ом —
  // хук на глобальний хоткей мусить діяти лише для видимої категорії.
  const { pathname } = useLocation();
  const isActiveRef = useRef(pathname === `/category/${categoryId}`);
  isActiveRef.current = pathname === `/category/${categoryId}`;

  const [focusedId, setFocusedId] = useState<number | null>(null);
  const focusedIdRef = useRef<number | null>(null);
  focusedIdRef.current = focusedId;

  const anchorRef = useRef<number | null>(null);
  const visibleRef = useRef<Candidate[]>(visible);
  visibleRef.current = visible;

  // Реактивність до selectionStore: будь-яка mark/unmark у будь-якій
  // категорії провокує ре-рендер, тож плитки завжди показують свіжий стан.
  useMarkedSummary();

  const markRange = (fromId: number, toId: number) => {
    const list = visibleRef.current;
    const fromIndex = list.findIndex((c) => c.id === fromId);
    const toIndex = list.findIndex((c) => c.id === toId);
    if (fromIndex === -1 || toIndex === -1) return;
    const [start, end] =
      fromIndex <= toIndex ? [fromIndex, toIndex] : [toIndex, fromIndex];
    selectionStore.markMultiple(list.slice(start, end + 1));
  };

  const toggleOne = (candidate: Candidate) => {
    selectionStore.toggle(candidate);
    anchorRef.current = candidate.id;
  };

  const handleActivate = (candidate: Candidate, event: React.MouseEvent) => {
    if (event.shiftKey && anchorRef.current !== null) {
      markRange(anchorRef.current, candidate.id);
    } else {
      toggleOne(candidate);
    }
  };

  useEffect(() => {
    const onHotkey = (event: Event) => {
      if (!isActiveRef.current) return;
      const { action } = (event as CustomEvent<HotkeyActionEventDetail>).detail;
      const list = visibleRef.current;
      if (action === "mark_toggle") {
        const focused = list.find((c) => c.id === focusedIdRef.current);
        if (focused) toggleOne(focused);
      } else if (action === "mark_range") {
        if (focusedIdRef.current !== null && anchorRef.current !== null) {
          markRange(anchorRef.current, focusedIdRef.current);
        }
      } else if (action === "mark_all") {
        selectionStore.markMultiple(list);
      } else if (action === "keep") {
        const focused = list.find((c) => c.id === focusedIdRef.current);
        if (focused) void keepCandidate(focused);
      }
    };
    window.addEventListener("trashradar:hotkey", onHotkey);
    return () => window.removeEventListener("trashradar:hotkey", onHotkey);
  }, []);

  return (
    <div className="flex h-full flex-col">
      {/* Рядок детектора: пояснення правила + редаговані пороги (T-115) */}
      <div className="flex h-8 shrink-0 items-center gap-3 border-b border-line px-3 text-xs text-ink-dim">
        <span>Детектор:</span>
        <span className="text-ink-faint">{rule}</span>
        {thresholdFields.map((field) => (
          <ThresholdInput
            key={field.key}
            categoryId={categoryId}
            field={field}
            value={effectiveThreshold(settings, categoryId, field)}
          />
        ))}
        {hasFilters || hasSearch ? (
          <span className="text-ink-faint">
            · результати: {visible.length} з {candidates.length}
          </span>
        ) : null}
      </div>

      <div className="min-h-0 flex-1">
        <VirtualCandidateGrid
          candidates={visible}
          focusedId={focusedId}
          isMarked={(candidate) => selectionStore.isMarked(candidate.id)}
          onActivate={handleActivate}
          onFocusCandidate={(candidate) => setFocusedId(candidate.id)}
          emptyTitle={
            (hasFilters || hasSearch) && candidates.length > 0
              ? "Жодного збігу з фільтрами чи пошуком"
              : `Сітка кандидатів: ${title}`
          }
        />
      </div>
    </div>
  );
}