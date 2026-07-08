/**
 * Каркас розкладки: Sidebar зліва, TopBar зверху, екран у <Outlet/>.
 * Мінімум пустого простору — принцип «кожен піксель працює» (ui.md §0).
 */

import { Outlet, useLocation, useParams } from "react-router-dom";

import { categoryTitle } from "@/store/categories";
import type { CategoryId } from "@/ipc/types";

import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";

function useScreenContext(): string {
  const { pathname } = useLocation();
  const params = useParams();
  if (pathname.startsWith("/category/")) {
    return categoryTitle(params["categoryId"] as CategoryId);
  }
  if (pathname.startsWith("/quarantine")) return "Quarantine";
  if (pathname.startsWith("/settings")) return "Налаштування";
  return "Cleanup";
}

export function AppLayout() {
  const context = useScreenContext();
  return (
    <div className="flex h-full">
      <Sidebar />
      <div className="flex min-w-0 flex-1 flex-col">
        <TopBar context={context} />
        <main className="min-h-0 flex-1 overflow-y-auto">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
