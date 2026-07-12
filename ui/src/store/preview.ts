/**
 * Прев'ю-міст (T-120): перше реальне підключення UI до Core-конвеєра
 * превью (T-067..T-076). До цієї задачі жодна `preview.*` команда не
 * викликалась з UI — плитки показували лише гліфи-заглушки (T-100).
 *
 * Дві незалежні можливості:
 * - `useThumbnail` — статична мініатюра плитки: кеш синхронно, інакше
 *   очікування події `preview.ready` (T-067 черга, P1-пріоритет);
 * - `loadScrubStrip` — скраб-смуга кадрів відео на вимогу (перший hover),
 *   з клієнтським кешем у сесії (DoD T-072/T-120: «без декодування на
 *   льоту» — повторні наведення читають уже отримані кадри з пам'яті).
 */

import { useEffect, useState } from "react";

import { command, subscribe } from "@/ipc/client";
import type {
  Candidate,
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
