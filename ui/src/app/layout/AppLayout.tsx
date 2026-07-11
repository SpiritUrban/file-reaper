/**
 * Постійний host екранів: навігація змінює видимість, а не монтування.
 * DOM scrollTop і локальний React state (зокрема позначення) переживають
 * переходи між Cleanup / категоріями / Quarantine / Settings (T-099).
 */

import { useEffect, useRef } from "react";
import { useLocation } from "react-router-dom";

import { ToastViewport } from "@/components/ToastViewport";
import { CategoryScreen } from "@/features/category/CategoryScreen";
import { CleanupSummaryScreen } from "@/features/cleanup-summary/CleanupSummaryScreen";
import { HealthScreen } from "@/features/health/HealthScreen";
import { QuarantineScreen } from "@/features/quarantine/QuarantineScreen";
import { SettingsScreen } from "@/features/settings/SettingsScreen";
import { CATEGORIES, categoryTitle } from "@/store/categories";
import { useMarkedSummary } from "@/store/selection";
import { useAppState } from "@/store/appState";
import type { CategoryId } from "@/ipc/types";
import { hotkeys } from "@/hotkeys";
import { command } from "@/ipc/client";

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
  const marked = useMarkedSummary();
  const appState = useAppState();
  const firstRunHandledRef = useRef(false);

  const activeCategory = categoryIdFromPath(pathname);
  const isQuarantine = pathname === "/quarantine";
  const isSettings = pathname === "/settings";
  const isHealth = pathname === "/health";
  const isCleanup =
    pathname === "/" ||
    (!activeCategory && !isQuarantine && !isSettings && !isHealth);

  // T-114: Автостарт скану на першому запуску (чистий профіль)
  useEffect(() => {
    if (
      appState.status === "ready" &&
      appState.isFirstRun &&
      !firstRunHandledRef.current
    ) {
      firstRunHandledRef.current = true;
      command("scan.start", { payload: {} }).catch((error) =>
        console.warn("Автостарт скану при першому запуску не пройшов:", error),
      );
    }
  }, [appState.status, appState.isFirstRun]);

  useEffect(() => {
    hotkeys.setActiveContexts([
      ...(activeCategory ? (["grid"] as const) : []),
      ...(isQuarantine ? (["quarantine"] as const) : []),
    ]);
  }, [activeCategory, isQuarantine]);

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
      <div className="flex min-w-0 flex-1 flex-col">
        <TopBar
          context={context}
          categoryId={activeCategory}
          markedCount={marked.count}
          markedBytes={marked.bytes}
        />
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
                <CategoryScreen categoryId={category.id} />
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
      </div>
      <ToastViewport />
    </div>
  );
}