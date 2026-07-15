import { useSyncExternalStore } from "react";

import { command, isTauri, subscribe } from "@/ipc/client";
import type {
  AppSettings,
  AppStateSnapshot,
  CategorySummary,
  CleanupTotal,
  DuplicatesCascadeEvent,
  QuarantineBadge,
  QuarantineChangedEvent,
  ScanProgressEvent,
  SettingsChangedEvent,
  VolumeUsageInfo,
} from "@/ipc/types";

type StoreStatus = "idle" | "hydrating" | "ready" | "error";

export interface AppState {
  status: StoreStatus;
  cleanup: CleanupTotal;
  scanRunning: boolean;
  /** Останній `scan.progress` — для банера «що зараз робиться». */
  scanProgress: ScanProgressEvent | null;
  /** Останній `duplicates.cascade_updated` — фаза хешування дублікатів. */
  cascadeProgress: DuplicatesCascadeEvent | null;
  /**
   * Чи вже запускали скан у цій сесії UI (кнопка «Почати» / ⟳).
   * Після цього попап старту не показується, навіть якщо totals ще 0.
   */
  scanStartedThisSession: boolean;
  settings: AppSettings | null;
  /** Живі томи для смужок дисків Sidebar (T-106). */
  volumes: VolumeUsageInfo[];
  /** Бейдж Quarantine (T-106): snapshot + події quarantine.changed. */
  quarantine: QuarantineBadge;
  /** Перший запуск (маркер профілю) — підказка в попапі старту. */
  isFirstRun: boolean;
  error: string | null;
}

const EMPTY_CLEANUP: CleanupTotal = {
  reclaimableBytes: 0,
  uniqueFiles: 0,
  categories: [],
};

const EMPTY_QUARANTINE: QuarantineBadge = { heldCount: 0, heldBytes: 0, nextPurgeAtUnix: 0 };

const INITIAL_STATE: AppState = {
  status: "idle",
  cleanup: EMPTY_CLEANUP,
  scanRunning: false,
  scanProgress: null,
  cascadeProgress: null,
  scanStartedThisSession: false,
  settings: null,
  volumes: [],
  quarantine: EMPTY_QUARANTINE,
  isFirstRun: false,
  error: null,
};

type StateUpdate = (state: AppState) => AppState;

/**
 * Проєкція authoritative Core state. Під час hydration події буферизуються:
 * snapshot застосовується першим, потім події, що прийшли паралельно з ним.
 */
export class AppStateStore {
  private state: AppState = INITIAL_STATE;
  private readonly listeners = new Set<() => void>();
  private unlisten: Array<() => void> = [];
  private buffered: StateUpdate[] | null = null;
  private startPromise: Promise<void> | null = null;

  getSnapshot = (): AppState => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  start(): Promise<void> {
    if (!this.startPromise) this.startPromise = this.hydrate();
    return this.startPromise;
  }

  stop(): void {
    for (const unlisten of this.unlisten) unlisten();
    this.unlisten = [];
    this.startPromise = null;
    this.buffered = null;
  }

  /** Позначити, що користувач (або UI) ініціював скан у цій сесії. */
  markScanStarted(): void {
    this.project((state) =>
      state.scanStartedThisSession
        ? { ...state, scanRunning: true }
        : { ...state, scanStartedThisSession: true, scanRunning: true },
    );
  }

  private publish(next: AppState): void {
    this.state = next;
    for (const listener of this.listeners) listener();
  }

  private project(update: StateUpdate): void {
    if (this.buffered) {
      this.buffered.push(update);
      return;
    }
    this.publish(update(this.state));
  }

  private async hydrate(): Promise<void> {
    if (!isTauri()) {
      this.publish({ ...INITIAL_STATE, status: "ready" });
      return;
    }

    this.publish({ ...this.state, status: "hydrating", error: null });
    this.buffered = [];
    try {
      this.unlisten = await Promise.all([
        subscribe<CleanupTotal>("cleanup.total_updated", (cleanup) =>
          this.project((state) => ({ ...state, cleanup })),
        ),
        subscribe<CategorySummary>("category.updated", (category) =>
          this.project((state) => ({
            ...state,
            cleanup: {
              ...state.cleanup,
              categories: upsertCategory(state.cleanup.categories, category),
            },
          })),
        ),
        subscribe<SettingsChangedEvent>("settings.changed", (event) =>
          this.project((state) => ({ ...state, settings: event.settings })),
        ),
        subscribe<ScanProgressEvent>("scan.progress", (event) =>
          this.project((state) => ({
            ...state,
            scanProgress: event,
            scanRunning: !event.done,
            scanStartedThisSession: true,
          })),
        ),
        subscribe<DuplicatesCascadeEvent>("duplicates.cascade_updated", (event) =>
          this.project((state) => ({
            ...state,
            cascadeProgress: event,
            // Поки refining / не complete — вважаємо роботу ще активною.
            scanRunning:
              state.scanRunning ||
              (event.phase !== "complete" &&
                event.phase !== "cancelled" &&
                event.phase !== "idle"),
          })),
        ),
        subscribe<QuarantineChangedEvent>("quarantine.changed", (event) =>
          this.project((state) => ({
            ...state,
            quarantine: {
              // heldBytes у події authoritative; count коригуємо purge-ами.
              heldCount: Math.max(0, state.quarantine.heldCount - event.purgedCount),
              heldBytes: event.heldBytes,
              nextPurgeAtUnix: state.quarantine.nextPurgeAtUnix,
            },
          })),
        ),
      ]);

      const snapshot = await command<AppStateSnapshot>("app.state");
      let hydrated: AppState = {
        status: "ready",
        cleanup: snapshot.cleanup,
        scanRunning: snapshot.scanRunning,
        scanProgress: null,
        cascadeProgress: null,
        scanStartedThisSession: snapshot.scanRunning,
        settings: snapshot.settings,
        volumes: snapshot.volumes ?? [],
        quarantine: snapshot.quarantine ?? EMPTY_QUARANTINE,
        isFirstRun: snapshot.isFirstRun ?? false,
        error: null,
      };
      for (const update of this.buffered) hydrated = update(hydrated);
      this.buffered = null;
      this.publish(hydrated);
    } catch (error) {
      this.buffered = null;
      this.stop();
      const message = error instanceof Error ? error.message : String(error);
      this.publish({ ...this.state, status: "error", error: message });
      throw error;
    }
  }
}

function upsertCategory(
  categories: CategorySummary[],
  category: CategorySummary,
): CategorySummary[] {
  const next = categories.filter((item) => item.id !== category.id);
  next.push(category);
  return next.sort(
    (left, right) =>
      right.totalBytes - left.totalBytes || left.id.localeCompare(right.id),
  );
}

export const appStateStore = new AppStateStore();

export function useAppState(): AppState {
  return useSyncExternalStore(
    appStateStore.subscribe,
    appStateStore.getSnapshot,
    appStateStore.getSnapshot,
  );
}

/** Коротка фаза для банера (лічильники — окремим рядком у ScanActivityBanner). */
export function describeScanActivity(state: AppState): string | null {
  if (!state.scanRunning && !state.scanProgress) return null;

  const progress = state.scanProgress;
  if (progress && !progress.done) {
    switch (progress.phase) {
      case "volume_started":
        return `Сканування ${progress.volume} · старт · ${strategyLabel(progress.strategy)}`;
      case "volume_progress":
        return `Сканування ${progress.volume} · ${strategyLabel(progress.strategy)}`;
      case "volume_finished":
        return `Том ${progress.volume} завершено · ${progress.filesIndexed.toLocaleString("uk-UA")} файлів`;
      case "session_finished":
        return `Індекс зібрано · ${progress.filesIndexed.toLocaleString("uk-UA")} файлів · аналіз…`;
      case "duplicates_cascade":
        return describeCascade(state.cascadeProgress);
      case "session_complete":
        return null;
      default:
        if (state.scanRunning) return "Сканування…";
    }
  }

  if (state.scanRunning && state.cascadeProgress) {
    return describeCascade(state.cascadeProgress);
  }

  if (state.scanRunning) return "Сканування…";
  return null;
}

function describeCascade(cascade: DuplicatesCascadeEvent | null): string {
  if (!cascade) return "Пошук дублікатів…";
  switch (cascade.phase) {
    case "size_grouping":
      return "Дублікати: групування за розміром…";
    case "partial_hashing":
      return `Дублікати: частковий хеш · ${cascade.groupCount} груп`;
    case "full_hashing":
      return `Дублікати: повний хеш · ${cascade.groupCount} груп`;
    case "complete":
      return cascade.groupCount > 0
        ? `Дублікати: підтверджено ${cascade.groupCount} груп`
        : "Дублікати: збігів не знайдено";
    case "cancelled":
      return "Пошук дублікатів скасовано";
    default:
      return "Пошук дублікатів…";
  }
}

function strategyLabel(strategy: string): string {
  switch (strategy) {
    case "mft":
      return "швидкий шлях (MFT)";
    case "directory_walk":
      return "обхід каталогів";
    case "usn_delta":
      return "дельта USN";
    default:
      return strategy;
  }
}
