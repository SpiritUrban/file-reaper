/**
 * Права зона Live Preview (docs/ui.md §10.1/§10.2, T-140): велике превью
 * файла «під курсором» або у фокусі клавіатури (`previewTargetStore`) на
 * весь екран, без власних контролів (нуль втрат площі, §10.1).
 *
 * Джерело — той самий `preview.large` (T-073 конвеєр, T-124 хук
 * `useLargePreview`), що й панель деталей: draft із кешу за <100 мс, sharp
 * підмінює його подією `preview.ready`. Наступна ціль при русі курсора
 * замінює попередню одразу (ефект хука скасовує попередній запит).
 *
 * T-141: єдина інформація в зоні — тонкий рядок-накладка знизу (шлях·розмір·
 * вік), що зникає за 2 с бездіяльності курсора й вертається на будь-який рух.
 * Свідомо НЕ тут: відео автовідтворення + скраб рухом миші — T-145.
 */

import { useEffect, useState } from "react";

import { formatAge, formatBytes } from "@/store/format";
import { useLargePreview } from "@/store/preview";
import { usePreviewTarget } from "@/store/previewTarget";

const IDLE_HIDE_MS = 2000;

function fileName(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  return normalized.split("/").filter(Boolean).at(-1) || path;
}

/**
 * Накладка видима, поки курсор рухається; ховається за 2 с спокою. Рух
 * миші відстежується на window — у Live Preview курсор «водять» по сітці
 * зліва, тож активність саме там і має тримати накладку. Зміна цілі
 * (нове наведення) — теж активність: ефект перезапускає таймер.
 */
function useIdleOverlayVisible(resetKey: unknown): boolean {
  const [visible, setVisible] = useState(true);

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout>;
    const bump = () => {
      setVisible(true);
      clearTimeout(timer);
      timer = setTimeout(() => setVisible(false), IDLE_HIDE_MS);
    };
    bump();
    window.addEventListener("mousemove", bump);
    return () => {
      clearTimeout(timer);
      window.removeEventListener("mousemove", bump);
    };
  }, [resetKey]);

  return visible;
}

export function LivePreviewPane() {
  const target = usePreviewTarget();
  const src = useLargePreview(target);
  const infoVisible = useIdleOverlayVisible(target?.id ?? null);

  return (
    <div className="relative flex min-w-0 flex-1 items-center justify-center overflow-hidden bg-bg">
      {!target ? (
        <span className="select-none text-xs text-ink-faint">
          Зона превью — наведіть курсор на плитку зліва
        </span>
      ) : src ? (
        <img
          src={src}
          alt=""
          className="max-h-full max-w-full object-contain"
          draggable={false}
        />
      ) : (
        // Ще вантажиться, або папка-одиниця (T-053) без єдиного файла для
        // превью — показуємо ім'я замість вічного спінера.
        <span className="max-w-[80%] select-none truncate px-4 text-sm text-ink-dim">
          {fileName(target.path)}
        </span>
      )}

      {target ? (
        <div
          className={`pointer-events-none absolute inset-x-0 bottom-0 flex items-center gap-2 bg-bg/85 px-3 py-1.5 font-mono text-xs text-ink-dim backdrop-blur-sm transition-opacity duration-500 ${
            infoVisible ? "opacity-100" : "opacity-0"
          }`}
        >
          <span className="min-w-0 flex-1 truncate text-ink">{target.path}</span>
          <span className="shrink-0">· {formatBytes(target.sizeBytes)}</span>
          <span className="shrink-0">· {formatAge(target.lastAccessAt)}</span>
        </div>
      ) : null}
    </div>
  );
}
