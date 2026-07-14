/**
 * Скраб-керування Live Preview зліва (T-145, docs/ui.md §10.2).
 *
 * Права зона не отримує mouse-подій (курсор «водять» по сітці зліва),
 * тому плитки повідомляють X-позицію курсора сюди, а `LivePreviewPane`
 * читає її й індексує вже отриману скраб-смугу (T-072) — без декодування
 * відео на льоту (architecture.md §5.3).
 *
 * Ефемерний, сесійний (як `previewTargetStore`): не персиститься.
 * `ratio === null` = «курсор на плитці, але не скрабить» → автоplay.
 * `ratio` 0..1 = горизонтальна позиція на плитці → кадр смуги.
 */

import { useSyncExternalStore } from "react";

export interface LivePreviewScrubState {
  /** Кандидат, по плитці якого рухається курсор (або null). */
  candidateId: number | null;
  /**
   * 0..1 — частка ширини плитки зліва; `null` — утримання без руху
   * (або лише фокус клавіатури), тоді права зона автовідтворює.
   */
  ratio: number | null;
  /** Інкремент на кожен reportMove — для debounce idle у правій зоні. */
  moveGeneration: number;
}

const IDLE: LivePreviewScrubState = {
  candidateId: null,
  ratio: null,
  moveGeneration: 0,
};

class LivePreviewScrubStore {
  private state: LivePreviewScrubState = IDLE;
  private readonly listeners = new Set<() => void>();

  getSnapshot = (): LivePreviewScrubState => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  private emit(next: LivePreviewScrubState): void {
    this.state = next;
    for (const listener of this.listeners) listener();
  }

  /**
   * Горизонтальний рух по плитці відео (Live Preview).
   * `ratio` clamp 0..0.999 — той самий контракт, що й `useVideoScrub`.
   */
  reportMove(candidateId: number, ratio: number): void {
    const clamped = Math.min(0.999, Math.max(0, ratio));
    this.emit({
      candidateId,
      ratio: clamped,
      moveGeneration: this.state.moveGeneration + 1,
    });
  }

  /**
   * Курсор увійшов на плитку / фокус клавіатури — без X-руху.
   * Скидає ratio → права зона переходить у режим автовідтворення
   * після dwell (T-145 DoD: «утримання курсора»).
   */
  reportHold(candidateId: number): void {
    if (
      this.state.candidateId === candidateId &&
      this.state.ratio === null
    ) {
      return;
    }
    this.emit({
      candidateId,
      ratio: null,
      moveGeneration: this.state.moveGeneration,
    });
  }

  /** Курсор полишив сітку / Live Preview вимкнено. */
  clear(): void {
    if (this.state.candidateId === null && this.state.ratio === null) return;
    this.emit(IDLE);
  }
}

export const livePreviewScrubStore = new LivePreviewScrubStore();

export function useLivePreviewScrub(): LivePreviewScrubState {
  return useSyncExternalStore(
    livePreviewScrubStore.subscribe,
    livePreviewScrubStore.getSnapshot,
    livePreviewScrubStore.getSnapshot,
  );
}
