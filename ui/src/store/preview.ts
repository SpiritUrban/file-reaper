/**
 * Прев'ю-міст (T-120/T-124): підключення UI до Core-конвеєра превью
 * (T-067..T-076). До T-120 жодна `preview.*` команда не викликалась з UI —
 * плитки показували лише гліфи-заглушки (T-100).
 *
 * - `useThumbnail` — статична мініатюра плитки: кеш синхронно, інакше
 *   очікування події `preview.ready` (T-067 черга, P1-пріоритет);
 * - `useLargePreview` — велике превью панелі деталей (T-124): draft з кешу
 *   синхронно (<100 мс), sharp підміняє його подією `preview.ready`
 *   (kind="large_sharp") — той самий канал, що й мініатюри, інший kind;
 * - `loadScrubStrip` / `useVideoScrub` — скраб-смуга кадрів відео на вимогу
 *   (перший hover), з клієнтським кешем у сесії (DoD T-072/T-120: «без
 *   декодування на льоту» — повторні наведення читають уже отримані кадри
 *   з пам'яті); спільний хук, бо і плитка (T-120), і велике превью в
 *   панелі деталей (T-124) скраблять однаково — рух курсора по X.
 */

import { useEffect, useRef, useState } from "react";

import { command, subscribe } from "@/ipc/client";
import type {
  Candidate,
  PreviewLargeAck,
  PreviewReadyEvent,
  PreviewScrubStripAck,
  PreviewThumbnailAck,
} from "@/ipc/types";

/** candidateId → data URL мініатюри; переживає розмонтування плитки (скрол). */
const thumbnailCache = new Map<number, string>();
/** candidateId → кадри скраб-смуги в порядку таймлайну. */
const scrubCache = new Map<number, string[]>();

/**
 * Статична мініатюра кандидата-файла. Папки-одиниці (T-053) не мають
 * єдиного файла для превью — хук для них одразу повертає `null`.
 */
export function useThumbnail(
  candidate: Pick<Candidate, "id" | "path" | "unit">,
): string | null {
  const [src, setSrc] = useState<string | null>(
    () => thumbnailCache.get(candidate.id) ?? null,
  );

  useEffect(() => {
    if (candidate.unit !== "file") return;
    const cached = thumbnailCache.get(candidate.id);
    if (cached) {
      setSrc(cached);
      return;
    }

    let cancelled = false;
    let unlisten: (() => void) | undefined;

    command<PreviewThumbnailAck>("preview.thumbnail", {
      payload: { candidateId: candidate.id },
    })
      .then(async (ack) => {
        if (cancelled) return;
        if (ack.status === "cached" && ack.dataUrl) {
          thumbnailCache.set(candidate.id, ack.dataUrl);
          setSrc(ack.dataUrl);
          return;
        }
        if (ack.status === "scheduled") {
          unlisten = await subscribe<PreviewReadyEvent>(
            "preview.ready",
            (event) => {
              if (cancelled || event.path !== candidate.path) return;
              thumbnailCache.set(candidate.id, event.dataUrl);
              setSrc(event.dataUrl);
              unlisten?.();
              unlisten = undefined;
            },
          );
        }
      })
      .catch((error) => {
        console.warn(
          `Failed to request thumbnail for candidate ${candidate.id}:`,
          error,
        );
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [candidate.id, candidate.path, candidate.unit]);

  return src;
}

/**
 * Скраб-смуга відео на вимогу (T-120: рух курсора = кадри). Один запит
 * на сесію на кандидата — клієнтський кеш покриває повторні наведення.
 */
export async function loadScrubStrip(
  candidate: Pick<Candidate, "id">,
): Promise<string[]> {
  const cached = scrubCache.get(candidate.id);
  if (cached) return cached;

  try {
    const ack = await command<PreviewScrubStripAck>("preview.scrub_strip", {
      payload: { candidateId: candidate.id },
    });
    if (ack.frames.length > 0) scrubCache.set(candidate.id, ack.frames);
    return ack.frames;
  } catch (error) {
    console.warn(
      `Failed to load scrub strip for candidate ${candidate.id}:`,
      error,
    );
    return [];
  }
}

/**
 * Велике превью панелі деталей (T-124). На відміну від `useThumbnail` —
 * без клієнтського кешу: Core сам віддає draft із дискового кешу (T-068)
 * за <100 мс на кожен запит, зайвий JS-кеш лише роздував би пам'ять для
 * важких (до 1024px) превью, не додаючи вимірної користі до DoD.
 */
export function useLargePreview(
  candidate: Pick<Candidate, "id" | "path" | "unit"> | null,
): string | null {
  const [src, setSrc] = useState<string | null>(null);

  useEffect(() => {
    setSrc(null);
    if (!candidate || candidate.unit !== "file") return;

    let cancelled = false;
    let unlisten: (() => void) | undefined;

    command<PreviewLargeAck>("preview.large", {
      payload: { candidateId: candidate.id },
    })
      .then(async (ack) => {
        if (cancelled) return;
        if (ack.dataUrl) setSrc(ack.dataUrl);
        if (ack.status === "sharp_from_cache") return;
        unlisten = await subscribe<PreviewReadyEvent>(
          "preview.ready",
          (event) => {
            if (cancelled || event.path !== candidate.path) return;
            if (event.kind === "large_sharp") setSrc(event.dataUrl);
          },
        );
      })
      .catch((error) => {
        console.warn(
          `Failed to request large preview for candidate ${candidate.id}:`,
          error,
        );
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [candidate?.id, candidate?.path, candidate?.unit]);

  return src;
}

/**
 * Скраб-взаємодія відео на наведення (T-120 плитка / T-124 велике превью):
 * перший `onMouseMove` ліниво тягне смугу, далі рух X лише індексує вже
 * отримані кадри; `onMouseLeave` повертає на кадр 0 (постер).
 */
export function useVideoScrub(
  candidate: Pick<Candidate, "id" | "kind"> | null,
  fallbackSrc: string | null | undefined,
): {
  displaySrc: string | null | undefined;
  scrubbing: boolean;
  handleMouseMove: (event: React.MouseEvent<HTMLElement>) => void;
  handleMouseLeave: () => void;
} {
  const isVideo = candidate?.kind === "video";
  const [scrubFrames, setScrubFrames] = useState<string[] | null>(null);
  const [scrubIndex, setScrubIndex] = useState(0);
  const loadingRef = useRef(false);

  useEffect(() => {
    setScrubFrames(null);
    setScrubIndex(0);
    loadingRef.current = false;
  }, [candidate?.id]);

  const handleMouseMove = (event: React.MouseEvent<HTMLElement>) => {
    if (!isVideo || !candidate) return;
    if (!scrubFrames) {
      if (!loadingRef.current) {
        loadingRef.current = true;
        loadScrubStrip(candidate).then((frames) => {
          loadingRef.current = false;
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

  const handleMouseLeave = () => setScrubIndex(0);

  const scrubbing = isVideo && scrubFrames !== null && scrubFrames.length > 0;
  const displaySrc = scrubbing ? scrubFrames[scrubIndex] : fallbackSrc;

  return { displaySrc, scrubbing, handleMouseMove, handleMouseLeave };
}
