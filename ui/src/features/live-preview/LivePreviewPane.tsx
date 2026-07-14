/**
 * Права зона Live Preview (docs/ui.md §10.1/§10.2, T-140..T-148): велике
 * превью файла «під курсором» або у фокусі клавіатури на весь екран, без
 * власних контролів (нуль втрат площі, §10.1).
 *
 * Категорії: `previewTargetStore` + `preview.large` (T-073/T-124) і
 * відео-скраб T-145. Quarantine (T-148): `quarantinePreviewTargetStore` +
 * `quarantine.thumbnail` (сурогатний шлях — файл уже переміщено, T-088).
 *
 * T-141: тонкий рядок-накладка знизу (шлях·розмір·вік/таймер), зникає за 2 с.
 */

import { useEffect, useState } from "react";

import { formatAge, formatBytes } from "@/store/format";
import {
  useLargePreview,
  useLivePreviewVideo,
  useQuarantineThumbnail,
} from "@/store/preview";
import { usePreviewTarget } from "@/store/previewTarget";
import { useQuarantinePreviewTarget } from "@/store/quarantinePreviewTarget";

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
  const candidateTarget = usePreviewTarget();
  const quarantineTarget = useQuarantinePreviewTarget();
  // Quarantine має пріоритет, коли обидві цілі випадково ненульові
  // (перемикання екранів — стори чистяться, але fail-safe).
  const mode = quarantineTarget ? "quarantine" : candidateTarget ? "candidate" : "empty";

  const largeSrc = useLargePreview(mode === "candidate" ? candidateTarget : null);
  const { displaySrc, playing, scrubbing } = useLivePreviewVideo(
    mode === "candidate" ? candidateTarget : null,
    largeSrc,
  );
  // Великий maxEdge для правої зони (Core clamp 16..1024, T-130).
  const quarantineSrc = useQuarantineThumbnail(
    mode === "quarantine" ? quarantineTarget : null,
    1024,
  );

  const overlayKey =
    mode === "quarantine"
      ? `q-${quarantineTarget!.id}`
      : mode === "candidate"
        ? `c-${candidateTarget!.id}`
        : "empty";
  const infoVisible = useIdleOverlayVisible(overlayKey);

  const imgSrc =
    mode === "quarantine" ? quarantineSrc : mode === "candidate" ? displaySrc : null;
  const fallbackName =
    mode === "quarantine"
      ? fileName(quarantineTarget!.originalPath)
      : mode === "candidate"
        ? fileName(candidateTarget!.path)
        : "";

  return (
    <div
      className="relative flex min-w-0 flex-1 items-center justify-center overflow-hidden bg-bg"
      data-video-playing={playing || undefined}
      data-video-scrubbing={scrubbing || undefined}
      data-preview-mode={mode}
    >
      {mode === "empty" ? (
        <span className="select-none text-xs text-ink-faint">
          Зона превью — наведіть курсор на плитку зліва
        </span>
      ) : imgSrc ? (
        <img
          src={imgSrc}
          alt=""
          className="max-h-full max-w-full object-contain"
          draggable={false}
        />
      ) : (
        <span className="max-w-[80%] select-none truncate px-4 text-sm text-ink-dim">
          {fallbackName}
        </span>
      )}

      {mode === "candidate" && candidateTarget ? (
        <div
          className={`pointer-events-none absolute inset-x-0 bottom-0 flex items-center gap-2 bg-bg/85 px-3 py-1.5 font-mono text-xs text-ink-dim backdrop-blur-sm transition-opacity duration-500 ${
            infoVisible ? "opacity-100" : "opacity-0"
          }`}
        >
          <span className="min-w-0 flex-1 truncate text-ink">
            {candidateTarget.path}
          </span>
          <span className="shrink-0">· {formatBytes(candidateTarget.sizeBytes)}</span>
          <span className="shrink-0">· {formatAge(candidateTarget.lastAccessAt)}</span>
        </div>
      ) : null}

      {mode === "quarantine" && quarantineTarget ? (
        <div
          className={`pointer-events-none absolute inset-x-0 bottom-0 flex items-center gap-2 bg-bg/85 px-3 py-1.5 font-mono text-xs text-ink-dim backdrop-blur-sm transition-opacity duration-500 ${
            infoVisible ? "opacity-100" : "opacity-0"
          }`}
        >
          <span className="min-w-0 flex-1 truncate text-ink">
            {quarantineTarget.originalPath}
          </span>
          <span className="shrink-0">· {formatBytes(quarantineTarget.sizeBytes)}</span>
          <span className="shrink-0 text-quarantine">
            · до{" "}
            {new Date(quarantineTarget.expiresAt).toLocaleDateString("uk-UA")}
          </span>
        </div>
      ) : null}
    </div>
  );
}
