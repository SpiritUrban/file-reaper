import { useRef, useState } from "react";

import type { Candidate, FileKind } from "@/ipc/types";
import { categoryTitle } from "@/store/categories";
import { formatBytes } from "@/store/format";
import { loadScrubStrip, useThumbnail } from "@/store/preview";

export type TileHeat = 1 | 2 | 3;

export interface CandidatePreview {
  src: string;
  alt?: string;
  /** Вже відформатована тривалість відео, напр. 12:44. */
  duration?: string;
}

export interface CandidateTileProps {
  candidate: Candidate;
  preview?: CandidatePreview;
  /** Grid може передати відносну heat-оцінку; fallback рахується з size. */
  heat?: TileHeat;
  focused?: boolean;
  /**
   * Локальне оптимістичне позначення (T-116, Reap Bar кошик): має пріоритет
   * над `candidate.decision`. `undefined` — використати decision як є.
   */
  marked?: boolean;
  className?: string;
  onActivate?: (candidate: Candidate, event: React.MouseEvent) => void;
  onFocusCandidate?: (candidate: Candidate) => void;
}

const KIND_GLYPH: Record<FileKind, string> = {
  video: "▶",
  image: "▧",
  audio: "♫",
  archive: "▣",
  installer: "⬡",
  disk_image: "◉",
  document: "▤",
  other: "◇",
};

function pathTail(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  const segments = normalized.split("/").filter(Boolean);
  return segments.slice(-2).join("/") || path;
}

function fileName(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  return normalized.split("/").filter(Boolean).at(-1) || path;
}

function ageLabel(value: string): string {
  const accessed = Date.parse(value);
  if (!Number.isFinite(accessed)) return "вік —";
  const days = Math.max(0, Math.floor((Date.now() - accessed) / 86_400_000));
  if (days < 1) return "сьогодні";
  if (days < 30) return `${days} дн`;
  const months = Math.floor(days / 30);
  if (months < 24) return `${months} міс`;
  return `${Math.floor(months / 12)} р`;
}

function inferredHeat(sizeBytes: number): TileHeat {
  if (sizeBytes >= 10 * 1024 ** 3) return 3;
  if (sizeBytes >= 1024 ** 3) return 2;
  return 1;
}

function heatClass(heat: TileHeat): string {
  if (heat === 3) return "bg-heat-3";
  if (heat === 2) return "bg-heat-2";
  return "bg-heat-1";
}

export function CandidateTile({
  candidate,
  preview: previewProp,
  heat = inferredHeat(candidate.sizeBytes),
  focused = false,
  marked: markedOverride,
  className = "",
  onActivate,
  onFocusCandidate,
}: CandidateTileProps) {
  const marked = markedOverride ?? candidate.decision === "marked";
  const kept = candidate.decision === "keep";
  const stateClass = marked
    ? "border-reap"
    : kept
      ? "border-keep/80"
      : "border-line hover:border-ink-dim";
  const focusClass = focused ? "ring-2 ring-accent/70" : "";

  // T-120: коли батьківський компонент не передав preview явно (звичайний
  // шлях сітки категорії), плитка сама запитує статичну мініатюру.
  const fetchedThumbnail = useThumbnail(candidate);
  const preview: CandidatePreview | undefined =
    previewProp ?? (fetchedThumbnail ? { src: fetchedThumbnail } : undefined);

  // T-120: скраб-смуга відео на вимогу — перше наведення тягне кадри,
  // рух курсора по X лише індексує вже отриманий масив (без декодування
  // на льоту, DoD T-072/T-120).
  const isVideo = candidate.kind === "video";
  const [scrubFrames, setScrubFrames] = useState<string[] | null>(null);
  const [scrubIndex, setScrubIndex] = useState(0);
  const scrubLoadingRef = useRef(false);

  const handleMouseMove = (event: React.MouseEvent<HTMLButtonElement>) => {
    if (!isVideo) return;
    if (!scrubFrames) {
      if (!scrubLoadingRef.current) {
        scrubLoadingRef.current = true;
        loadScrubStrip(candidate).then((frames) => {
          scrubLoadingRef.current = false;
          if (frames.length > 0) setScrubFrames(frames);
        });
      }
      return;
    }
    const rect = event.currentTarget.getBoundingClientRect();
    const ratio = Math.min(
      0.999,
      Math.max(0, (event.clientX - rect.left) / rect.width),
    );
    setScrubIndex(Math.floor(ratio * scrubFrames.length));
  };

  const handleMouseLeave = () => {
    setScrubIndex(0);
  };

  const scrubbing = isVideo && scrubFrames !== null && scrubFrames.length > 0;
  const displaySrc = scrubbing ? scrubFrames[scrubIndex] : preview?.src;

  return (
    <button
      type="button"
      className={`group relative aspect-[4/3] w-full overflow-hidden rounded-sm border bg-panel text-left outline-none transition-colors ${stateClass} ${focusClass} focus-visible:border-accent focus-visible:ring-2 focus-visible:ring-accent/70 ${className}`}
      aria-label={`${fileName(candidate.path)}, ${formatBytes(candidate.sizeBytes)}, ${ageLabel(candidate.lastAccessAt)}`}
      aria-pressed={marked}
      data-decision={candidate.decision}
      data-marked={marked || undefined}
      data-focused={focused || undefined}
      data-scrubbing={scrubbing || undefined}
      onClick={(event) => onActivate?.(candidate, event)}
      onFocus={() => onFocusCandidate?.(candidate)}
      onMouseMove={handleMouseMove}
      onMouseLeave={handleMouseLeave}
    >
      <span className={`absolute inset-x-0 top-0 z-30 h-1 ${heatClass(heat)}`} />

      <span className="absolute inset-0 flex items-center justify-center overflow-hidden bg-panel-2">
        {displaySrc ? (
          <img
            src={displaySrc}
            alt={preview?.alt ?? ""}
            className="h-full w-full object-cover"
            draggable={false}
          />
        ) : (
          <span className="flex max-w-[85%] flex-col items-center gap-2 text-center text-ink-dim">
            <span className="text-4xl text-ink-faint" aria-hidden="true">
              {candidate.unit === "folder" ? "▰" : KIND_GLYPH[candidate.kind]}
            </span>
            <span className="line-clamp-2 break-all text-sm font-medium text-ink-dim">
              {fileName(candidate.path)}
            </span>
            {candidate.unit === "folder" ? (
              <span className="text-xs text-ink-faint">група файлів</span>
            ) : null}
          </span>
        )}
      </span>

      {isVideo && preview?.duration ? (
        <span className="absolute bottom-[17%] right-1.5 z-20 rounded bg-bg/80 px-1.5 py-0.5 font-mono text-xs text-ink">
          ▶ {preview.duration}
        </span>
      ) : null}

      {candidate.alsoIn[0] ? (
        <span
          className="absolute left-2 top-2 z-30 max-w-[75%] truncate rounded-full bg-bg/85 px-2 py-0.5 text-xs text-ink-dim backdrop-blur-sm"
          title={`Також у: ${candidate.alsoIn.map(categoryTitle).join(", ")}`}
        >
          ⧉ також у: {categoryTitle(candidate.alsoIn[0])}
          {candidate.alsoIn.length > 1 ? ` +${candidate.alsoIn.length - 1}` : ""}
        </span>
      ) : null}

      <span className="absolute inset-x-0 bottom-0 z-20 flex h-[15%] min-h-7 items-center gap-1.5 bg-bg/85 px-2 backdrop-blur-sm">
        <strong className="shrink-0 font-mono text-sm font-semibold text-ink">
          {formatBytes(candidate.sizeBytes)}
        </strong>
        <span className="shrink-0 text-xs text-ink-dim">· {ageLabel(candidate.lastAccessAt)}</span>
        <span className="min-w-0 flex-1 truncate font-mono text-xs text-ink-dim">
          · {pathTail(candidate.path)}
        </span>
      </span>

      {marked ? (
        <>
          <span className="absolute inset-0 z-10 bg-reap/30" />
          <span
            className="absolute right-2 top-2 z-30 grid h-7 w-7 place-items-center rounded-full bg-reap text-lg font-bold text-bg"
            aria-label="Позначено до видалення"
          >
            ╳
          </span>
        </>
      ) : null}

      {kept ? (
        <span
          className="absolute right-2 top-2 z-30 grid h-7 w-7 place-items-center rounded-full bg-keep text-lg font-bold text-bg"
          aria-label="Залишити"
        >
          ✓
        </span>
      ) : null}
    </button>
  );
}