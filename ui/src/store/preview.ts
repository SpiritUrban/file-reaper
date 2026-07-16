/**
 * Прев'ю-міст (T-120/T-124/T-145): підключення UI до Core-конвеєра превью
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
 *   панелі деталей (T-124) скраблять однаково — рух курсора по X;
 * - `useLivePreviewVideo` — права зона Live Preview (T-145): автовідтворення
 *   смуги без звуку при утриманні + скраб X-позицією зліва
 *   (`livePreviewScrubStore`); теж лише pre-extracted кадри, не `<video>`;
 * - `prefetchAlongTrajectory` — T-146 / T-075: P2-прогрів 2–3 наступних
 *   плиток за знаком руху, щоб Draft у правій зоні був готовий до наведення.
 */

import { useEffect, useRef, useState } from "react";

import { command, subscribe } from "@/ipc/client";
import type {
  Candidate,
  PreviewLargeAck,
  PreviewPrefetchAck,
  PreviewReadyEvent,
  PreviewScrubStripAck,
  PreviewThumbnailAck,
} from "@/ipc/types";
import { useLivePreviewScrub } from "@/store/livePreviewScrub";

/** Затримка «утримання» перед автовідтворенням у правій зоні (T-145 / §10.2). */
const LIVE_PREVIEW_AUTOPLAY_DWELL_MS = 350;
/** Темп «відтворення» смуги: ~10 fps — плавно, без навантаження на GPU. */
const LIVE_PREVIEW_AUTOPLAY_FPS = 10;
/** Після скрабу (рух X) — стільки мс спокою, перш ніж відновити autoplay. */
const LIVE_PREVIEW_SCRUB_IDLE_MS = 200;

/** candidateId → data URL мініатюри; переживає розмонтування плитки (скрол). */
const thumbnailCache = new Map<number, string>();
/** candidateId → кадри скраб-смуги в порядку таймлайну. */
const scrubCache = new Map<number, string[]>();

/**
 * Спільна шина `preview.ready`: рівно ОДНА підписка на застосунок, і вона
 * реєструється **до** видачі будь-якої `preview.*` команди. Раніше кожен хук
 * підписувався вже **після** `command(...)` (`command().then(async ack =>
 * await subscribe(...))`) — але P1-задача декодування стартує синхронно при
 * виклику команди й на швидкому WIC-декоді встигає емітнути подію ДО того, як
 * хук зареєструє слухача. Tauri події не буферизуються → подія губилась, і
 * плитка лишалась порожньою («пусті площини»). Шина + `ensurePreviewReadyBus`
 * до команди усувають цю гонку й тримають лише одного слухача замість N.
 */
const previewReadyWaiters = new Map<string, Set<(e: PreviewReadyEvent) => void>>();
let previewReadyBus: Promise<() => void> | null = null;

function ensurePreviewReadyBus(): Promise<() => void> {
  if (!previewReadyBus) {
    previewReadyBus = subscribe<PreviewReadyEvent>("preview.ready", (event) => {
      const waiters = previewReadyWaiters.get(event.path);
      if (!waiters) return;
      // Копія: колбек може зняти себе під час ітерації.
      for (const cb of [...waiters]) cb(event);
    });
  }
  return previewReadyBus;
}

/** Зареєструвати очікувача `preview.ready` для конкретного шляху. */
function onPreviewReady(
  path: string,
  cb: (e: PreviewReadyEvent) => void,
): () => void {
  let set = previewReadyWaiters.get(path);
  if (!set) {
    set = new Set();
    previewReadyWaiters.set(path, set);
  }
  set.add(cb);
  return () => {
    const s = previewReadyWaiters.get(path);
    if (!s) return;
    s.delete(cb);
    if (s.size === 0) previewReadyWaiters.delete(path);
  };
}

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
    let off: (() => void) | undefined;

    void (async () => {
      // Слухача реєструємо ДО команди — інакше швидка P1-задача емітне
      // preview.ready раніше, ніж ми підпишемось, і подія загубиться.
      await ensurePreviewReadyBus();
      if (cancelled) return;
      off = onPreviewReady(candidate.path, (event) => {
        if (cancelled) return;
        thumbnailCache.set(candidate.id, event.dataUrl);
        setSrc(event.dataUrl);
        off?.();
        off = undefined;
      });
      try {
        const ack = await command<PreviewThumbnailAck>("preview.thumbnail", {
          payload: { candidateId: candidate.id },
        });
        if (cancelled) return;
        if (ack.status === "cached" && ack.dataUrl) {
          thumbnailCache.set(candidate.id, ack.dataUrl);
          setSrc(ack.dataUrl);
          off?.();
          off = undefined;
        } else if (ack.status !== "scheduled") {
          // "unavailable" (папка-одиниця) — події не буде, знімаємо слухача.
          off?.();
          off = undefined;
        }
      } catch (error) {
        off?.();
        off = undefined;
        console.warn(
          `Failed to request thumbnail for candidate ${candidate.id}:`,
          error,
        );
      }
    })();

    return () => {
      cancelled = true;
      off?.();
    };
  }, [candidate.id, candidate.path, candidate.unit]);

  return src;
}

/** entryId → data URL мініатюри Quarantine (T-130); окремий кеш від плиток. */
const quarantineThumbnailCache = new Map<number, string>();

/**
 * Мініатюра плитки Quarantine (T-130): `quarantine.thumbnail` — окрема
 * команда від `preview.thumbnail`, бо джерело мініатюри — сурогатний шлях
 * карантину, не HotIndex-запис (файл уже переміщено, T-088). Синхронна
 * відповідь Core (без P1-черги/`preview.ready`, T-130) — жодної підписки
 * на подію не потрібно, на відміну від `useThumbnail`.
 */
/**
 * Мініатюра / велике превью запису Quarantine.
 * `maxEdge` — 160 для плитки (T-130), до 1024 для Live Preview (T-148).
 * Кеш ключується лише `entryId` (перший успішний dataUrl); для LP
 * достатньо того самого PNG, розтягнутого `object-contain`.
 */
export function useQuarantineThumbnail(
  entry: { id: number } | null,
  maxEdge = 160,
): string | null {
  const [src, setSrc] = useState<string | null>(() =>
    entry ? (quarantineThumbnailCache.get(entry.id) ?? null) : null,
  );

  useEffect(() => {
    if (!entry) {
      setSrc(null);
      return;
    }
    const cached = quarantineThumbnailCache.get(entry.id);
    if (cached) {
      setSrc(cached);
      return;
    }
    setSrc(null);
    let cancelled = false;
    command<PreviewThumbnailAck>("quarantine.thumbnail", {
      payload: { entryId: entry.id, maxEdge },
    })
      .then((ack) => {
        if (cancelled || ack.status !== "cached" || !ack.dataUrl) return;
        quarantineThumbnailCache.set(entry.id, ack.dataUrl);
        setSrc(ack.dataUrl);
      })
      .catch((error) => {
        console.warn(`Failed to request thumbnail for quarantine entry ${entry.id}:`, error);
      });
    return () => {
      cancelled = true;
    };
  }, [entry?.id, maxEdge]);

  return src;
}

/**
 * T-146: префетч за траєкторією (IPC → Core `PreviewPrefetcher` T-075).
 * `ordered` — видимий порядок плиток; `anchorId` — під курсором/у фокусі;
 * `prevAnchorId` — попередня ціль (для знаку motion). Fire-and-forget:
 * помилка не ламає Live Preview (деградація до cold large).
 *
 * Кумулятивні metrics (hitRate) пишуться в `lastPrefetchMetrics` для
 * діагностики DoD «замір хітрейту» без окремого health-екрана.
 */
let lastPrefetchMetrics: PreviewPrefetchAck["metrics"] | null = null;
let prefetchInFlight = false;

export function getLastPrefetchMetrics(): PreviewPrefetchAck["metrics"] | null {
  return lastPrefetchMetrics;
}

export function prefetchAlongTrajectory(
  ordered: readonly Pick<Candidate, "id">[],
  anchorId: number,
  prevAnchorId: number | null,
): void {
  if (ordered.length === 0) return;
  const anchorIndex = ordered.findIndex((c) => c.id === anchorId);
  if (anchorIndex < 0) return;

  let motion = 1; // дефолт «уперед», коли перше наведення без prev
  if (prevAnchorId != null) {
    const prevIndex = ordered.findIndex((c) => c.id === prevAnchorId);
    if (prevIndex >= 0 && prevIndex !== anchorIndex) {
      motion = anchorIndex > prevIndex ? 1 : -1;
    } else if (prevIndex === anchorIndex) {
      return; // той самий якір — не спамимо IPC
    }
  }

  // Не більше однієї prefetch-команди одночасно (швидкий скрол / hover).
  if (prefetchInFlight) return;
  prefetchInFlight = true;

  // Стеля 256 — узгоджена з Core; віртуальна сітка + overscan вміщається.
  const windowStart = Math.max(0, anchorIndex - 8);
  const windowEnd = Math.min(ordered.length, anchorIndex + 9);
  const slice = ordered.slice(windowStart, windowEnd);
  const localAnchor = anchorIndex - windowStart;

  void command<PreviewPrefetchAck>("preview.prefetch", {
    payload: {
      candidateIds: slice.map((c) => c.id),
      anchorIndex: localAnchor,
      motion,
    },
  })
    .then((ack) => {
      lastPrefetchMetrics = ack.metrics;
    })
    .catch((error) => {
      console.warn("preview.prefetch failed:", error);
    })
    .finally(() => {
      prefetchInFlight = false;
    });
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

    const id = candidate.id;
    const path = candidate.path;
    let cancelled = false;
    let off: (() => void) | undefined;

    void (async () => {
      // Та сама гонка, що й у useThumbnail: слухача Sharp-доставки реєструємо
      // ДО команди, інакше P0-подія large_sharp може випередити підписку.
      await ensurePreviewReadyBus();
      if (cancelled) return;
      off = onPreviewReady(path, (event) => {
        if (cancelled || event.kind !== "large_sharp") return;
        setSrc(event.dataUrl);
        // Sharp — фінальна доставка; слухача можна зняти.
        off?.();
        off = undefined;
      });
      try {
        const ack = await command<PreviewLargeAck>("preview.large", {
          payload: { candidateId: id },
        });
        if (cancelled) return;
        if (ack.dataUrl) setSrc(ack.dataUrl);
        if (ack.status === "sharp_from_cache") {
          off?.();
          off = undefined;
        }
      } catch (error) {
        off?.();
        off = undefined;
        console.warn(
          `Failed to request large preview for candidate ${id}:`,
          error,
        );
      }
    })();

    return () => {
      cancelled = true;
      off?.();
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

/**
 * Відео у правій зоні Live Preview (T-145, ui.md §10.2):
 * - утримання курсора на плитці (dwell) → автовідтворення скраб-смуги
 *   без звуку (циклічна зміна pre-extracted кадрів T-072, не `<video>`);
 * - горизонтальний рух по плитці зліва → скраб великого екрана
 *   (ratio з `livePreviewScrubStore`);
 * - фокус клавіатури (без миші) → те саме автовідтворення після dwell.
 *
 * Fallback — `useLargePreview` (ключ-кадр/мініатюра), поки смуга ще
 * вантажиться або файл не відео / смуга недоступна (немає ffmpeg).
 */
export function useLivePreviewVideo(
  candidate: Pick<Candidate, "id" | "kind" | "unit"> | null,
  fallbackSrc: string | null,
): {
  displaySrc: string | null;
  /** true, коли показуємо кадр смуги (autoplay або scrub), не static large. */
  playing: boolean;
  scrubbing: boolean;
} {
  const isVideo =
    candidate?.kind === "video" && candidate.unit === "file";
  const scrub = useLivePreviewScrub();
  const [frames, setFrames] = useState<string[] | null>(null);
  const [frameIndex, setFrameIndex] = useState(0);
  const [mode, setMode] = useState<"idle" | "autoplay" | "scrub">("idle");
  const framesRef = useRef<string[] | null>(null);
  framesRef.current = frames;

  // Завантаження смуги при зміні цілі-відео (той самий клієнтський кеш,
  // що й плитка T-120 — повторне наведення без IPC).
  useEffect(() => {
    setFrames(null);
    setFrameIndex(0);
    setMode("idle");
    if (!isVideo || !candidate) return;

    let cancelled = false;
    loadScrubStrip(candidate).then((loaded) => {
      if (cancelled || loaded.length === 0) return;
      setFrames(loaded);
    });
    return () => {
      cancelled = true;
    };
  }, [candidate?.id, isVideo]);

  // Скраб зліва: ratio з плитки мапиться на кадр. Лише для поточної цілі.
  useEffect(() => {
    if (!isVideo || !candidate || !frames) return;
    if (scrub.candidateId !== candidate.id) return;
    if (scrub.ratio === null) return;

    setMode("scrub");
    setFrameIndex(Math.floor(scrub.ratio * frames.length));
  }, [scrub.candidateId, scrub.ratio, scrub.moveGeneration, candidate?.id, frames, isVideo]);

  // Автовідтворення: dwell після появи цілі / після паузи скрабу.
  // Залежить від moveGeneration — кожен рух перезапускає таймер idle.
  useEffect(() => {
    if (!isVideo || !candidate || !frames || frames.length === 0) return;

    // Активний скраб цієї ж цілі — чекаємо спокою, потім autoplay.
    const scrubbingThis =
      scrub.candidateId === candidate.id && scrub.ratio !== null;
    const delay = scrubbingThis
      ? LIVE_PREVIEW_SCRUB_IDLE_MS
      : LIVE_PREVIEW_AUTOPLAY_DWELL_MS;

    const timer = setTimeout(() => {
      // Стартуємо з поточного кадру (після скрабу — з місця зупинки).
      setMode("autoplay");
    }, delay);

    return () => clearTimeout(timer);
  }, [
    candidate?.id,
    frames,
    isVideo,
    scrub.candidateId,
    scrub.ratio,
    scrub.moveGeneration,
  ]);

  // Тік autoplay: циклічно гортає кадри, поки mode === "autoplay".
  useEffect(() => {
    if (mode !== "autoplay") return;
    const strip = framesRef.current;
    if (!strip || strip.length === 0) return;

    const interval = setInterval(() => {
      setFrameIndex((i) => (i + 1) % strip.length);
    }, 1000 / LIVE_PREVIEW_AUTOPLAY_FPS);

    return () => clearInterval(interval);
  }, [mode, frames?.length, candidate?.id]);

  if (!isVideo || !frames || frames.length === 0 || mode === "idle") {
    return {
      displaySrc: fallbackSrc,
      playing: false,
      scrubbing: false,
    };
  }

  return {
    displaySrc: frames[frameIndex] ?? fallbackSrc,
    playing: mode === "autoplay",
    scrubbing: mode === "scrub",
  };
}
