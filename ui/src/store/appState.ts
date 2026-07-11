import { useSyncExternalStore } from "react";

import { command, isTauri, subscribe } from "@/ipc/client";
import type {
  AppSettings,
  AppStateSnapshot,
  CategorySummary,
  CleanupTotal,
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
  settings: AppSettings | null;
  /** Живі томи для смужок дисків Sidebar (T-106). */
  volumes: VolumeUsageInfo[];
  /** Бейдж Quarantine (T-106): snapshot + події quarantine.changed. */
  quarantine: QuarantineBadge;
  /** Перший запуск (T-114): сигнал для автостарту скану. */
  isFirstRun: boolean;
  error: string | null;
}

const EMPTY_CLEANUP: CleanupTotal = {
  reclaimableBytes: 0,
  uniqueFiles: 0,
  categories: [],
};

const EMPTY_QUARANTINE: QuarantineBadge = { heldCount: 0, heldBytes: 0 };

const INITIAL_STATE: AppState = {
  status: "idle",
  cleanup: EMPTY_CLEANUP,
  scanRunning: false,
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
          this.project((state) => ({ ...state, scanRunning: !event.done })),
        ),
        subscribe<QuarantineChangedEvent>("quarantine.changed", (event) =>
          this.project((state) => ({
            ...state,
            quarantine: {
              // heldBytes у події authoritative; count коригуємо purge-ами.
              heldCount: Math.max(0, state.quarantine.heldCount - event.purgedCount),
              heldBytes: event.heldBytes,
            },
          })),
        ),
      ]);

      const snapshot = await command<AppStateSnapshot>("app.state");
      let hydrated: AppState = {
        status: "ready",
        cleanup: snapshot.cleanup,
        scanRunning: snapshot.scanRunning,
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