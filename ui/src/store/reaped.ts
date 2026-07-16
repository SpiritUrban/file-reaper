/**
 * Сесійний список Reaped (T-138) — id кандидатів, уже відправлених у Quarantine.
 * Reap ховає файл з УСІХ категорій сесії одразу (як і Keep, T-117): Core
 * застосовує Decision::Keep на reaped-id і шле оновлені totals (лічильники/бейдж
 * ідуть самі), але вже завантажена сітка `category.window` не рефетчиться на ці
 * події — тому тримаємо окремий сесійний стор оптимістичної видимості, щоб
 * плитки зникали миттєво без чекання нового фетчу.
 *
 * Відрізняється від `keepStore` семантикою: reaped-файл фізично переміщено —
 * тому фільтрується завжди (навіть у Live Preview без hideProcessed), не має
 * ✓-стану плитки, а «Скасувати» тосту (reap.undo_batch) знімає його назад.
 */

import { useSyncExternalStore } from "react";

class ReapedStore {
  private reaped = new Set<number>();
  private readonly listeners = new Set<() => void>();

  getSnapshot = (): ReadonlySet<number> => this.reaped;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  isReaped(id: number): boolean {
    return this.reaped.has(id);
  }

  /** Позначити батч reaped — ховає плитки з усіх категорій сесії. */
  reap(ids: Iterable<number>): void {
    const next = new Set(this.reaped);
    let changed = false;
    for (const id of ids) {
      if (!next.has(id)) {
        next.add(id);
        changed = true;
      }
    }
    if (!changed) return;
    this.reaped = next;
    this.publish();
  }

  /** Скасувати reap (undo_batch) — повертає плитки у сітку сесії. */
  unreap(ids: Iterable<number>): void {
    const next = new Set(this.reaped);
    let changed = false;
    for (const id of ids) {
      if (next.delete(id)) changed = true;
    }
    if (!changed) return;
    this.reaped = next;
    this.publish();
  }

  private publish(): void {
    for (const listener of this.listeners) listener();
  }
}

export const reapedStore = new ReapedStore();

/** Реактивна підписка: ре-рендер при будь-якій зміні Reaped-списку. */
export function useReapedIds(): ReadonlySet<number> {
  return useSyncExternalStore(reapedStore.subscribe, reapedStore.getSnapshot, reapedStore.getSnapshot);
}
