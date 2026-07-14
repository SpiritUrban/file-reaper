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
 * Свідомо НЕ тут: інформаційна накладка шлях·розмір·вік — T-141; відео
 * автовідтворення + скраб рухом миші — T-145. Поки лише вписане зображення.
 */

import { useLargePreview } from "@/store/preview";
import { usePreviewTarget } from "@/store/previewTarget";

function fileName(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  return normalized.split("/").filter(Boolean).at(-1) || path;
}

export function LivePreviewPane() {
  const target = usePreviewTarget();
  const src = useLargePreview(target);

  return (
    <div className="flex min-w-0 flex-1 items-center justify-center overflow-hidden bg-bg">
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
    </div>
  );
}
