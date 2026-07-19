import type { Candidate, FileKind } from "@/ipc/types";
import { categoryTitle } from "@/store/categories";
import { doNotDeleteHint } from "@/store/doNotDeleteHints";
import { formatAge, formatBytes } from "@/store/format";
import { useThumbnail, useVideoScrub } from "@/store/preview";

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
  /**
   * Оптимістичний Keep (T-117/T-147): має пріоритет над `candidate.decision`.
   * У Live Preview keep лишається на місці затемненим (сітка не стрибає).
   */
  kept?: boolean;
  className?: string;
  /** CSS-курсор плитки (T-142): іконка озброєної дії у Live Preview. */
  cursor?: string;
  /**
   * Короткий візуальний відгук після дії (T-143): пульс-обвід кольором дії.
   * CSS-клас кільця (напр. `ring-reap`) або null.
   */
  flashRing?: string | null;
  onActivate?: (candidate: Candidate, event: React.MouseEvent) => void;
  /** ПКМ (T-143): у Live Preview → протилежна озброєна дія. */
  onSecondaryActivate?: (candidate: Candidate, event: React.MouseEvent) => void;
  onFocusCandidate?: (candidate: Candidate) => void;
  /** Наведення курсора (T-140): у Live Preview → велике превью праворуч. */
  onHoverCandidate?: (candidate: Candidate) => void;
  /**
   * Горизонтальний рух по плитці (T-145): у Live Preview ratio 0..1
   * скрабить великий екран праворуч. Окремо від внутрішнього скрабу
   * мініатюри плитки (T-120).
   */
  onHoverMove?: (candidate: Candidate, ratio: number) => void;
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
  kept: keptOverride,
  className = "",
  cursor,
  flashRing,
  onActivate,
  onSecondaryActivate,
  onFocusCandidate,
  onHoverCandidate,
  onHoverMove,
}: CandidateTileProps) {
  const marked = markedOverride ?? candidate.decision === "marked";
  const kept = keptOverride ?? candidate.decision === "keep";
  // T-147: опрацьована (marked/keep) — затемнена на місці, сітка не зсувається.
  const processed = marked || kept;
  // Відомі системні/службові файли: напівпрозорі + короткий опис (+ ⚠ critical).
  const keepHint = doNotDeleteHint(candidate.path);
  const isCritical = keepHint?.severity === "critical";
  const stateClass = marked
    ? "border-reap"
    : kept
      ? "border-keep/80"
      : isCritical
        ? "border-reap/70 hover:border-reap"
        : keepHint
          ? "border-ink-faint hover:border-ink-dim"
          : "border-line hover:border-ink-dim";
  const focusClass = focused ? "ring-2 ring-accent/70" : "";
  // Keep-hint слабший за processed (marked/keep), щоб стани дій лишались читабельними.
  const processedClass = processed
    ? "opacity-50"
    : keepHint
      ? isCritical
        ? "opacity-45"
        : "opacity-40"
      : "";

  // T-120: коли батьківський компонент не передав preview явно (звичайний
  // шлях сітки категорії), плитка сама запитує статичну мініатюру.
  const fetchedThumbnail = useThumbnail(candidate);
  const preview: CandidatePreview | undefined =
    previewProp ?? (fetchedThumbnail ? { src: fetchedThumbnail } : undefined);

  // T-120/T-124: скраб-смуга відео на вимогу — спільний хук з великим
  // превью панелі деталей (store/preview.ts), без дублювання логіки.
  const isVideo = candidate.kind === "video";
  const { displaySrc, scrubbing, handleMouseMove, handleMouseLeave } =
    useVideoScrub(candidate, preview?.src);

  // T-145: ratio для правого екрана Live Preview — паралельно з локальним
  // скрабом мініатюри; батько (CategoryScreen) фільтрує за режимом.
  const handleMove = (event: React.MouseEvent<HTMLElement>) => {
    handleMouseMove(event);
    if (!onHoverMove) return;
    const rect = event.currentTarget.getBoundingClientRect();
    if (rect.width <= 0) return;
    const ratio = Math.min(
      0.999,
      Math.max(0, (event.clientX - rect.left) / rect.width),
    );
    onHoverMove(candidate, ratio);
  };

  return (
    <button
      type="button"
      className={`group relative aspect-[4/3] w-full overflow-hidden rounded-sm border bg-panel text-left outline-none transition-colors ${stateClass} ${focusClass} ${processedClass} ${flashRing ?? ""} focus-visible:border-accent focus-visible:ring-2 focus-visible:ring-accent/70 ${className}`}
      {...(cursor ? { style: { cursor } } : {})}
      aria-label={[
        fileName(candidate.path),
        formatBytes(candidate.sizeBytes),
        formatAge(candidate.lastAccessAt),
        keepHint
          ? `${isCritical ? "критично не видаляти" : "не видаляти"}: ${keepHint.label}`
          : null,
      ]
        .filter(Boolean)
        .join(", ")}
      aria-pressed={marked}
      data-candidate-id={candidate.id}
      data-decision={candidate.decision}
      data-marked={marked || undefined}
      data-kept={kept || undefined}
      data-processed={processed || undefined}
      data-do-not-delete={keepHint?.id || undefined}
      data-keep-severity={keepHint?.severity || undefined}
      data-focused={focused || undefined}
      onContextMenu={
        onSecondaryActivate
          ? (event) => onSecondaryActivate(candidate, event)
          : undefined
      }
      data-scrubbing={scrubbing || undefined}
      onClick={(event) => onActivate?.(candidate, event)}
      onFocus={() => onFocusCandidate?.(candidate)}
      onMouseEnter={() => onHoverCandidate?.(candidate)}
      onMouseMove={handleMove}
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

      {keepHint ? (
        <>
          <span
            className={`absolute left-2 top-2 z-30 max-w-[calc(100%-2.75rem)] rounded-sm px-2 py-1 text-xs leading-snug backdrop-blur-sm ${
              isCritical
                ? "border border-reap/50 bg-reap/15 text-ink"
                : "bg-bg/90 text-ink-dim"
            }`}
            title={
              isCritical
                ? `Критично не видаляти: ${keepHint.label}`
                : `Не варто видаляти: ${keepHint.label}`
            }
          >
            <span
              className={`font-medium ${isCritical ? "text-reap" : "text-ink-faint"}`}
            >
              {isCritical ? "критично" : "не видаляти"}
            </span>
            <span
              className={`mt-0.5 block truncate ${isCritical ? "text-ink" : "text-ink-dim"}`}
            >
              {keepHint.label}
            </span>
          </span>
          {isCritical ? (
            <span
              className="absolute right-2 top-2 z-30 grid h-7 w-7 place-items-center rounded-full bg-reap text-sm font-bold text-bg shadow-sm"
              title="Критично: не видаляти"
              aria-hidden="true"
            >
              ⚠
            </span>
          ) : null}
        </>
      ) : candidate.alsoIn[0] ? (
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
        <span className="shrink-0 text-xs text-ink-dim">· {formatAge(candidate.lastAccessAt)}</span>
        <span className="min-w-0 flex-1 truncate font-mono text-xs text-ink-dim">
          · {pathTail(candidate.path)}
        </span>
      </span>

      {marked ? (
        <>
          <span className="absolute inset-0 z-10 bg-reap/30" />
          {/* §10.6: перекреслена опрацьована (reap) — діагональ поверх превью. */}
          <span
            className="pointer-events-none absolute inset-0 z-20"
            aria-hidden="true"
            style={{
              background:
                "linear-gradient(to top left, transparent calc(50% - 1px), rgba(229,72,77,0.85) calc(50% - 1px), rgba(229,72,77,0.85) calc(50% + 1px), transparent calc(50% + 1px))",
            }}
          />
          <span
            className="absolute right-2 top-2 z-30 grid h-7 w-7 place-items-center rounded-full bg-reap text-lg font-bold text-bg"
            aria-label="Позначено до видалення"
          >
            ╳
          </span>
        </>
      ) : null}

      {kept && !marked ? (
        <>
          <span className="absolute inset-0 z-10 bg-keep/20" />
          <span
            className="absolute right-2 top-2 z-30 grid h-7 w-7 place-items-center rounded-full bg-keep text-lg font-bold text-bg"
            aria-label="Залишити"
          >
            ✓
          </span>
        </>
      ) : null}
    </button>
  );
}