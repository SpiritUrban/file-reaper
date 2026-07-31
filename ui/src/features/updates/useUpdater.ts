/**
 * Автооновлення: перевірка при старті й встановлення на вимогу
 * (правило 28 брифу Стадії 2).
 *
 * Без виклику `check()` з UI плагін `tauri-plugin-updater` мовчить: він не
 * опитує ендпоінт сам. Тобто конфігурація в tauri.conf.json, підпис у CI і
 * `latest.json` у релізі дають рівно нуль, поки цей хук не змонтований.
 *
 * Помилка перевірки — НЕ подія для користувача: немає мережі, ендпоінт ще
 * без релізу, корпоративний проксі. Тихий console.warn; банер з'являється
 * лише коли оновлення справді є. Помилка ВСТАНОВЛЕННЯ — навпаки, видима:
 * користувач натиснув кнопку і має право знати результат.
 */

import { useEffect, useState } from "react";

import { isTauri } from "@/ipc/client";

type UpdaterPhase = "idle" | "available" | "installing" | "restart" | "failed";

export interface UpdaterState {
  phase: UpdaterPhase;
  /** Версія, яку пропонує ендпоінт. Ніколи не хардкодиться (правило 18). */
  version: string | null;
  /** 0..1, поки триває завантаження; null, поки розмір невідомий. */
  progress: number | null;
  error: string | null;
}

const INITIAL: UpdaterState = {
  phase: "idle",
  version: null,
  progress: null,
  error: null,
};

export function useUpdater() {
  const [state, setState] = useState<UpdaterState>(INITIAL);
  // Об'єкт оновлення живе поза стейтом: він не серіалізується і потрібен
  // лише для другого кроку (downloadAndInstall).
  const [pending, setPending] = useState<UpdateHandle | null>(null);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;

    void (async () => {
      try {
        const { check } = await import("@tauri-apps/plugin-updater");
        const update = await check();
        if (cancelled || !update) return;
        setPending(update as UpdateHandle);
        setState({
          phase: "available",
          version: update.version,
          progress: null,
          error: null,
        });
      } catch (error) {
        console.warn("Перевірка оновлень не пройшла:", error);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, []);

  async function install(): Promise<void> {
    if (!pending) return;
    setState((current) => ({ ...current, phase: "installing", progress: null }));

    let contentLength = 0;
    let downloaded = 0;
    try {
      await pending.downloadAndInstall((event) => {
        if (event.event === "Started") {
          contentLength = event.data.contentLength ?? 0;
          downloaded = 0;
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          setState((current) => ({
            ...current,
            progress: contentLength > 0 ? downloaded / contentLength : null,
          }));
        }
      });
      // Windows (installMode: passive) закриває застосунок сам, і до цього
      // рядка виконання не доходить. macOS/Linux — доходить, і там перезапуск
      // наш: без нього користувач лишається у старій копії, впевнений, що
      // оновився.
      setState((current) => ({ ...current, phase: "restart", progress: 1 }));
      const { relaunch } = await import("@tauri-apps/plugin-process");
      await relaunch();
    } catch (error) {
      setState({
        phase: "failed",
        version: state.version,
        progress: null,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  function dismiss(): void {
    setState(INITIAL);
    setPending(null);
  }

  return { state, install, dismiss };
}

/** Мінімальна форма Update з плагіна — рівно те, чим користується хук. */
interface UpdateHandle {
  version: string;
  downloadAndInstall(
    onEvent: (event: DownloadEvent) => void,
  ): Promise<void>;
}

type DownloadEvent =
  | { event: "Started"; data: { contentLength?: number } }
  | { event: "Progress"; data: { chunkLength: number } }
  | { event: "Finished" };
