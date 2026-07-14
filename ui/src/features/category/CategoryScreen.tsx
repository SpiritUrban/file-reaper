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
 * T-118: щільність сітки `-`/`=` (Compact/Standard/Large) через спільний
 * `gridDensityStore` — один вибір на сесію, перемикання категорій його не
 * скидає; позиція фокуса (`focusedId`) від density не залежить.
 * T-119: під час скану нові знахідки НЕ вливаються в уже відрендерений
 * список автоматично (жодного зсуву позицій) — стрічка «+N нових знахідок ↓»
 * порівнює живий `itemCount` (T-055 cleanup.total_updated/category.updated)
 * з розміром уже завантаженого вікна; клік по стрічці — ручний `refetch()`.
 * T-122: Ctrl+↑/↓ (AppLayout) перемикає категорію і шле сигнал сфокусувати
 * першу плитку.
 * T-123: стрілки рухають фокус по сітці (рядок = кількість колонок з
 * `VirtualCandidateGrid.onColumnsChange`); Enter відкриває `DetailsPanel`
 * на сфокусованому кандидаті; поки панель відкрита, та сама навігація
 * стрілками оновлює її вміст (панель не «краде» фокус сітки — Esc
 * обробляється в самій панелі через контекст хоткеїв "details").
 * Дублікати НЕ рендеряться цим компонентом — окремий `DuplicatesScreen`
 * (T-126, `features/duplicates`), підключений в `AppLayout` для
 * `categoryId === "duplicates"`: групи каскаду, не окремі `FileRecord`.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { useLocation } from "react-router-dom";

import { VirtualCandidateGrid } from "@/components/VirtualCandidateGrid";
import type { HotkeyActionEventDetail } from "@/hotkeys";
import { useAppState } from "@/store/appState";
import { categoryRule, categoryTitle } from "@/store/categories";
import { useCategoryWindow } from "@/store/categoryWindow";
import { detailsPanelStore } from "@/store/detailsPanel";
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
import { gridDensityStore, useGridDensity } from "@/store/gridDensity";
import { livePreviewStore } from "@/store/livePreview";
import { previewTargetStore } from "@/store/previewTarget";
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
  const appState = useAppState();
  const { settings } = appState;
  const filters = useCandidateFilters(categoryId);
  const search = useSearchState(categoryId);
  // settings — нова посилання на кожну settings.changed подію (T-098), тому
  // зміна порога детектора перебудовує сітку без ручного тригера (T-115 DoD).
  const { candidates: fetched, hasLoaded, refetch } = useCategoryWindow(
    categoryId,
    settings,
  );
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
  const density = useGridDensity();

  // T-119: «+N нових знахідок» — лише коли одиниця рахунку категорії це
  // окремі кандидати (не групи дублікатів, T-126) і перший фетч уже прийшов
  // (інакше секунду до даних показало б фальшиве «+N» від порожнього fetched).
  const liveSummary = appState.cleanup.categories.find((c) => c.id === categoryId);
  const pendingNew =
    hasLoaded && liveSummary && liveSummary.countUnit !== "groups"
      ? Math.max(0, liveSummary.itemCount - fetched.length)
      : 0;

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
  // T-123: геометрія від VirtualCandidateGrid — потрібна лише для ↑/↓
  // (рядок = стільки позицій, скільки колонок), ре-рендер тут зайвий.
  const columnsRef = useRef(1);

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

  // T-140: у Live Preview кандидат «під курсором»/у фокусі стає ціллю
  // великого превью праворуч. Поза режимом — no-op (права зона не змонтована),
  // тож наведення в звичайній сітці не тягне жодного preview.large.
  const previewIfLive = (candidate: Candidate) => {
    if (livePreviewStore.getSnapshot().enabled) previewTargetStore.set(candidate);
  };

  const handleActivate = (candidate: Candidate, event: React.MouseEvent) => {
    if (event.shiftKey && anchorRef.current !== null) {
      markRange(anchorRef.current, candidate.id);
    } else {
      toggleOne(candidate);
    }
  };

  // T-123: перемістити фокус на delta позицій (±1 = ліво/право, ±columns =
  // верх/низ), затиснуто в межах видимого списку. Якщо панель деталей уже
  // відкрита — одразу підмінити її вміст новою плиткою (навігація сіткою
  // оновлює вміст, DoD T-123).
  const moveFocus = (delta: number) => {
    const list = visibleRef.current;
    if (list.length === 0) return;
    const currentIndex = list.findIndex((c) => c.id === focusedIdRef.current);
    const nextIndex =
      currentIndex === -1
        ? 0
        : Math.min(list.length - 1, Math.max(0, currentIndex + delta));
    const next = list[nextIndex];
    if (!next) return;
    setFocusedId(next.id);
    previewIfLive(next);
    if (detailsPanelStore.isOpen()) detailsPanelStore.open(next);
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
      } else if (action === "zoom_out") {
        gridDensityStore.zoomOut();
      } else if (action === "zoom_in") {
        gridDensityStore.zoomIn();
      } else if (action === "navigate_left") {
        moveFocus(-1);
      } else if (action === "navigate_right") {
        moveFocus(1);
      } else if (action === "navigate_up") {
        moveFocus(-columnsRef.current);
      } else if (action === "navigate_down") {
        moveFocus(columnsRef.current);
      } else if (action === "details") {
        const focused = list.find((c) => c.id === focusedIdRef.current) ?? list[0];
        if (focused) {
          setFocusedId(focused.id);
          detailsPanelStore.open(focused);
        }
      }
    };
    window.addEventListener("trashradar:hotkey", onHotkey);
    return () => window.removeEventListener("trashradar:hotkey", onHotkey);
  }, []);

  // T-122: Ctrl+↑/↓ (AppLayout) навігує сюди й одразу шле сигнал фокусу —
  // перша плитка видимого списку стає ціллю Space/K без зайвого кліку.
  useEffect(() => {
    const onFocusFirst = (event: Event) => {
      const detail = (event as CustomEvent<{ categoryId: CategoryId }>).detail;
      if (detail.categoryId !== categoryId) return;
      const first = visibleRef.current[0];
      setFocusedId(first ? first.id : null);
      if (first) previewIfLive(first);
    };
    window.addEventListener("trashradar:focus-category-first", onFocusFirst);
    return () =>
      window.removeEventListener("trashradar:focus-category-first", onFocusFirst);
  }, [categoryId]);

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

      <div className="relative min-h-0 flex-1">
        {pendingNew > 0 ? (
          <button
            type="button"
            onClick={refetch}
            className="absolute left-1/2 top-2 z-40 -translate-x-1/2 rounded-full border border-accent/50 bg-panel/95 px-3 py-1 text-xs font-medium text-ink shadow-lg backdrop-blur-sm transition-colors hover:border-accent"
          >
            +{pendingNew} нових знахідок ↓
          </button>
        ) : null}
        <VirtualCandidateGrid
          candidates={visible}
          density={density}
          focusedId={focusedId}
          isMarked={(candidate) => selectionStore.isMarked(candidate.id)}
          onActivate={handleActivate}
          onFocusCandidate={(candidate) => {
            setFocusedId(candidate.id);
            previewIfLive(candidate);
          }}
          onHoverCandidate={previewIfLive}
          onColumnsChange={(columns) => {
            columnsRef.current = columns;
          }}
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