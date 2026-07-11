import { useSyncExternalStore } from "react";

export type ToastTone = "info" | "success" | "warning";

export interface ToastAction {
  label: string;
  run: () => void | Promise<void>;
}

export interface ToastInput {
  message: string;
  durationMs?: number;
  tone?: ToastTone;
  action?: ToastAction;
}

export interface ToastRecord {
  id: number;
  message: string;
  durationMs: number;
  tone: ToastTone;
  actionConsumed: boolean;
  action?: ToastAction;
}

const DEFAULT_DURATION_MS = 5_000;

export class ToastStore {
  private records: ToastRecord[] = [];
  private readonly listeners = new Set<() => void>();
  private nextId = 1;

  getSnapshot = (): readonly ToastRecord[] => this.records;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  show(input: ToastInput): number {
    const id = this.nextId++;
    const record: ToastRecord = {
      id,
      message: input.message,
      durationMs: Math.max(1, input.durationMs ?? DEFAULT_DURATION_MS),
      tone: input.tone ?? "info",
      actionConsumed: false,
      ...(input.action ? { action: input.action } : {}),
    };
    this.publish([...this.records, record]);
    return id;
  }

  dismiss(id: number): void {
    const next = this.records.filter((record) => record.id !== id);
    if (next.length !== this.records.length) this.publish(next);
  }

  /** Marks consumed before invoking callback: rapid/double clicks execute once. */
  runAction(id: number): void {
    const record = this.records.find((item) => item.id === id);
    if (!record?.action || record.actionConsumed) return;
    this.publish(
      this.records.map((item) =>
        item.id === id ? { ...item, actionConsumed: true } : item,
      ),
    );
    let result: void | Promise<void>;
    try {
      result = record.action.run();
    } catch (error) {
      console.error("Toast action failed:", error);
      result = undefined;
    } finally {
      this.dismiss(id);
    }
    if (result instanceof Promise) {
      void result.catch((error: unknown) =>
        console.error("Toast action failed:", error),
      );
    }
  }

  clear(): void {
    if (this.records.length > 0) this.publish([]);
  }

  private publish(records: ToastRecord[]): void {
    this.records = records;
    for (const listener of this.listeners) listener();
  }
}

export const toastStore = new ToastStore();

export function toast(input: ToastInput): number {
  return toastStore.show(input);
}

export function useToasts(): readonly ToastRecord[] {
  return useSyncExternalStore(
    toastStore.subscribe,
    toastStore.getSnapshot,
    toastStore.getSnapshot,
  );
}