/**
 * Постійний host екранів: навігація змінює видимість, а не монтування.
 * DOM scrollTop і локальний React state (зокрема позначення) переживають
 * переходи між Cleanup / категоріями / Quarantine / Settings (T-099).
 */

import { useEffect, useRef } from "react";
import { useLocation, useNavigate } from "react-router-dom";

import { DetailsPanel } from "@/components/DetailsPanel";
import { ScanActivityBanner } from "@/components/ScanActivityBanner";
import { ScanStartOverlay } from "@/components/ScanStartOverlay";
import { TileContextMenu } from "@/components/TileContextMenu";
import { ToastViewport } from "@/components/ToastViewport";
import { CategoryScreen } from "@/features/category/CategoryScreen";
import { CleanupSummaryScreen } from "@/features/cleanup-summary/CleanupSummaryScreen";
import { DuplicatesScreen } from "@/features/duplicates/DuplicatesScreen";
import { HealthScreen } from "@/features/health/HealthScreen";
import { LivePreviewSplit } from "@/features/live-preview/LivePreviewSplit";
import { QuarantineScreen } from "@/features/quarantine/QuarantineScreen";
import { ReapConfirmOverlay } from "@/features/reap-flow/ReapConfirmOverlay";
import { SettingsScreen } from "@/features/settings/SettingsScreen";
import { UpdateBanner } from "@/features/updates/UpdateBanner";
import { CATEGORIES, categoryRowsByWeight, categoryTitle } from "@/store/categories";
import { livePreviewStore, useLivePreview } from "@/store/livePreview";
import { reapOverlayStore } from "@/store/reapOverlay";
import { useMarkedSummary } from "@/store/selection";
import { useAppState } from "@/store/appState";
import type { CategoryId } from "@/ipc/types";
import { hotkeys, type HotkeyActionEventDetail } from "@/hotkeys";

import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";

function categoryIdFromPath(pathname: string): CategoryId | null {
  const match = /^\/category\/([^/]+)$/.exec(pathname);
  const id = match?.[1];
  return CATEGORIES.some((category) => category.id === id)
    ? (id as CategoryId)
    : null;
}

function screenClass(active: boolean): string {
  return active ? "h-full overflow-y-auto" : "hidden";
}

export function AppLayout() {
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const marked = useMarkedSummary();
  const appState = useAppState();
  const { enabled: livePreviewEnabled } = useLivePreview();

  const activeCategory = categoryIdFromPath(pathname);
  const isQuarantine = pathname === "/quarantine";
  const isSettings = pathname === "/settings";
  const isHealth = pathname === "/health";
  const isCleanup =
    pathname === "/" ||
    (!activeCategory && !isQuarantine && !isSettings && !isHealth);

  // Старт скану — лише явна дія (ScanStartOverlay / ⟳), не тихий автостарт.

  useEffect(() => {
    hotkeys.setActiveContexts([
      ...(activeCategory ? (["grid"] as const) : []),
      ...(isQuarantine ? (["quarantine"] as const) : []),
      ...(livePreviewEnabled ? (["live_preview"] as const) : []),
    ]);
  }, [activeCategory, isQuarantine, livePreviewEnabled]);

  // T-139: `P` (global контекст, T-103 `live_preview`) вмикає/вимикає
  // двомоніторний режим; сам стан і геометрія межі — у livePreviewStore
  // (переживають перезапуск через localStorage).
  useEffect(() => {
    const onHotkey = (event: Event) => {
      const { action } = (event as CustomEvent<HotkeyActionEventDetail>).detail;
      if (action === "live_preview") livePreviewStore.toggle();
    };
    window.addEventListener("trashradar:hotkey", onHotkey);
    return () => window.removeEventListener("trashradar:hotkey", onHotkey);
  }, []);

  // T-122: Ctrl+↑/↓ — перемикання категорій за тим самим порядком ваги,
  // що бачить користувач у Sidebar (T-105); "global" контекст — працює і
  // з Cleanup Summary. Позначення (selectionStore, T-108) спільні на сесію
  // й самі переживають навігацію; фокус на першій плитці — сигнал для
  // CategoryScreen, що вже змонтований (T-099), а не новий рендер.
  //
  // Ref-и, не значення в deps: два Ctrl+↓ поспіль (швидкий auto-repeat)
  // можуть дати другий keydown до того, як React встигне пере-рендерити
  // AppLayout і перепідписати onHotkey зі свіжим activeCategory — застарілий
  // closure тоді порахував би індекс від попередньої категорії. Ref завжди
  // читає значення на момент самого keydown, не на момент підписки.
  const activeCategoryRef = useRef(activeCategory);
  activeCategoryRef.current = activeCategory;
  const categoriesRef = useRef(appState.cleanup.categories);
  categoriesRef.current = appState.cleanup.categories;
  // T-152: вимкнені детектори випадають і з навігації Ctrl+↑/↓.
  const settingsRef = useRef(appState.settings);
  settingsRef.current = appState.settings;

  useEffect(() => {
    const onHotkey = (event: Event) => {
      const { action } = (event as CustomEvent<HotkeyActionEventDetail>).detail;
      if (action !== "category_previous" && action !== "category_next") return;

      const ids = categoryRowsByWeight(
        categoriesRef.current,
        settingsRef.current,
      ).map((row) => row.descriptor.id);
      const current = activeCategoryRef.current;
      const currentIndex = current ? ids.indexOf(current) : -1;
      const delta = action === "category_next" ? 1 : -1;
      const nextIndex =
        currentIndex === -1
          ? action === "category_next"
            ? 0
            : ids.length - 1
          : (currentIndex + delta + ids.length) % ids.length;
      const nextId = ids[nextIndex];
      if (!nextId) return;

      // Синхронно оновити ref: без цього другий keydown у тому самому тіку
      // (перед ре-рендером) знову порахував би від тієї самої current.
      activeCategoryRef.current = nextId;
      navigate(`/category/${nextId}`);
      window.dispatchEvent(
        new CustomEvent("trashradar:focus-category-first", {
          detail: { categoryId: nextId },
        }),
      );
    };
    window.addEventListener("trashradar:hotkey", onHotkey);
    return () => window.removeEventListener("trashradar:hotkey", onHotkey);
  }, [navigate]);

  // T-135: Ctrl+Enter відкриває оверлей підтвердження REAP (global контекст,
  // T-103 `reap_confirm`) — той самий guard, що й кнопка REAP у TopBar
  // (нічого не відкривати, якщо нема позначень). Ref — той самий stale-closure
  // захист, що й вище для category_previous/next.
  const markedCountRef = useRef(marked.count);
  markedCountRef.current = marked.count;
  useEffect(() => {
    const onHotkey = (event: Event) => {
      const { action } = (event as CustomEvent<HotkeyActionEventDetail>).detail;
      if (action === "reap_confirm" && markedCountRef.current > 0) {
        reapOverlayStore.open();
      }
    };
    window.addEventListener("trashradar:hotkey", onHotkey);
    return () => window.removeEventListener("trashradar:hotkey", onHotkey);
  }, []);

  const context = activeCategory
    ? categoryTitle(activeCategory)
    : isQuarantine
      ? "Quarantine"
      : isSettings
        ? "Налаштування"
        : isHealth
          ? "Health"
          : "Cleanup";

  return (
    <div className="flex h-full">
      <Sidebar />
      {/* T-139: у Live Preview LivePreviewSplit віддає частину ширини правій
          зоні превью з перетяжною межею; поза режимом — прозорий wrapper,
          ліва колонка займає всю ширину (як було до режиму). */}
      <LivePreviewSplit>
        <TopBar
          context={context}
          categoryId={activeCategory}
          markedCount={marked.count}
          markedBytes={marked.bytes}
        />
        {/* Правило 28: без цього компонента встановлена копія ніколи не
            дізнається про нову версію — плагін сам ендпоінт не опитує. */}
        <UpdateBanner />
        <ScanActivityBanner />
        <main className="min-h-0 flex-1">
          <section className={screenClass(isCleanup)} aria-hidden={!isCleanup}>
            <CleanupSummaryScreen />
          </section>

          {CATEGORIES.map((category) => {
            const active = activeCategory === category.id;
            return (
              <section
                key={category.id}
                className={screenClass(active)}
                aria-hidden={!active}
              >
                {/* T-126: Дублікати — сітка групами (свій екран), не
                    стандартний per-file category.window (ui.md §4). */}
                {category.id === "duplicates" ? (
                  <DuplicatesScreen />
                ) : (
                  <CategoryScreen categoryId={category.id} />
                )}
              </section>
            );
          })}

          <section
            className={screenClass(isQuarantine)}
            aria-hidden={!isQuarantine}
          >
            <QuarantineScreen />
          </section>
          <section className={screenClass(isSettings)} aria-hidden={!isSettings}>
            <SettingsScreen />
          </section>
          {isHealth ? (
            <section className="h-full overflow-y-auto">
              <HealthScreen />
            </section>
          ) : null}
        </main>
      </LivePreviewSplit>
      {/* Немодальна панель деталей (T-123): поруч із сіткою, не поверх неї. */}
      <DetailsPanel />
      <ToastViewport />
      {/* Старт: явна кнопка, коли даних ще немає. */}
      <ScanStartOverlay />
      {/* Єдиний деструктивний оверлей (T-135): REAP-кнопка/Ctrl+Enter. */}
      <ReapConfirmOverlay />
      {/* Контекстне меню плитки (ПКМ): копіювати шлях/ім'я, відкрити в папці,
          Keep, у Quarantine. Один екземпляр на застосунок. */}
      <TileContextMenu />
    </div>
  );
}