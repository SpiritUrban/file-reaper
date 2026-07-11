/**
 * Редаговані пороги предикатних детекторів (T-039..042) для рядка над
 * сіткою категорії (T-115). Лише 4 категорії мають числовий поріг —
 * решта (temp_files/app_caches/dev_artifacts/installers/duplicates)
 * реєстрові/структурні/обчислювальні детектори без user-порога в MVP.
 *
 * Дефолти дзеркалять Core (core/app/src/detectors/*.rs) — коли settings
 * не містить override, детектор працює на цих значеннях.
 */

import { command } from "@/ipc/client";
import type { AppSettings, CategoryId } from "@/ipc/types";

export interface ThresholdFieldConfig {
  key: string;
  label: string;
  /** МіБ — вводиться і показується у мебібайтах, зберігається у байтах. */
  unit: "size_mib" | "days";
  defaultValue: number;
}

const MIB = 1024 * 1024;

export const CATEGORY_THRESHOLDS: Partial<
  Record<CategoryId, ThresholdFieldConfig[]>
> = {
  large_files: [
    { key: "min_size_bytes", label: "Мін. розмір", unit: "size_mib", defaultValue: 100 * MIB },
  ],
  old_files: [
    { key: "min_age_days", label: "Мін. вік", unit: "days", defaultValue: 365 },
  ],
  forgotten_videos: [
    { key: "min_size_bytes", label: "Мін. розмір", unit: "size_mib", defaultValue: 100 * MIB },
    { key: "min_age_days", label: "Мін. вік", unit: "days", defaultValue: 180 },
  ],
  archives: [
    { key: "min_size_bytes", label: "Мін. розмір", unit: "size_mib", defaultValue: 50 * MIB },
  ],
};

/** Поточне ефективне значення порога: override з settings або дефолт детектора. */
export function effectiveThreshold(
  settings: AppSettings | null,
  categoryId: CategoryId,
  field: ThresholdFieldConfig,
): number {
  return (
    settings?.detectors?.[categoryId]?.thresholds?.[field.key] ??
    field.defaultValue
  );
}

/**
 * Зміна порога детектора (T-115). Делегує в `category.set_threshold`:
 * Core перераховує категорію з індексу гарячим шляхом і шле
 * `settings.changed` — сітка перебудовується без рескану диска.
 */
export async function setCategoryThreshold(
  categoryId: CategoryId,
  key: string,
  value: number,
): Promise<AppSettings> {
  return command<AppSettings>("category.set_threshold", {
    payload: { categoryId, key, value },
  });
}
