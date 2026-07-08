/**
 * Маршрути застосунку. HashRouter — безпечний вибір для Tauri
 * (роздача з вбудованого frontendDist без серверних rewrite-ів).
 */

import { createHashRouter } from "react-router-dom";

import { CategoryScreen } from "@/features/category/CategoryScreen";
import { CleanupSummaryScreen } from "@/features/cleanup-summary/CleanupSummaryScreen";
import { HealthScreen } from "@/features/health/HealthScreen";
import { QuarantineScreen } from "@/features/quarantine/QuarantineScreen";
import { SettingsScreen } from "@/features/settings/SettingsScreen";

import { AppLayout } from "./layout/AppLayout";

export const router = createHashRouter([
  {
    element: <AppLayout />,
    children: [
      { path: "/", element: <CleanupSummaryScreen /> },
      { path: "/category/:categoryId", element: <CategoryScreen /> },
      { path: "/quarantine", element: <QuarantineScreen /> },
      { path: "/health", element: <HealthScreen /> },
      { path: "/settings", element: <SettingsScreen /> },
    ],
  },
]);
