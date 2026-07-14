/**
 * Редагований поріг предикатного детектора (T-115). Спільний для рядка
 * детектора над сіткою категорії та секції «Категорії-детектори» екрана
 * Налаштувань (T-150, ui.md §9.3: «дублює швидке налаштування з рядка
 * детектора»). Зміна йде через `category.set_threshold` → гарячий
 * перерахунок Core без рескану → подія settings.changed.
 */

import { useState } from "react";

import {
  setCategoryThreshold,
  type ThresholdFieldConfig,
} from "@/store/detectorThresholds";
import type { CategoryId } from "@/ipc/types";

export function ThresholdInput({
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
