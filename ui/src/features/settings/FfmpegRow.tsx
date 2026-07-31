/**
 * Відео-превʼю: стан ffmpeg і завантаження в 1 клік (правило 29 брифу
 * Стадії 2).
 *
 * Замість пасивного попередження «встановіть ffmpeg і пропишіть PATH» —
 * дія. Пасивне попередження перекладає роботу на користувача, і 90% просто
 * лишаються без відео-превʼю, вважаючи, що вони зламані.
 */

import { useEffect, useState } from "react";

import { command, ipcErrorMessage, isTauri } from "@/ipc/client";

interface FfmpegStatus {
  available: boolean;
  path: string | null;
  downloadable: boolean;
}

export function FfmpegRow() {
  const [status, setStatus] = useState<FfmpegStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isTauri()) return;
    void command<FfmpegStatus>("ffmpeg.status")
      .then(setStatus)
      .catch((reason) => console.warn("Стан ffmpeg не отримано:", reason));
  }, []);

  async function download(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      // Команда повертається лише після того, як бінарник справді
      // запустився (перевірка наслідку, а не наявності файла).
      setStatus(await command<FfmpegStatus>("ffmpeg.download"));
    } catch (reason) {
      setError(ipcErrorMessage(reason));
    } finally {
      setBusy(false);
    }
  }

  if (status?.available) {
    return (
      <>
        <span className="text-keep">✓ працюють</span>
        <span className="truncate font-mono text-ink-faint" title={status.path ?? ""}>
          {status.path}
        </span>
      </>
    );
  }

  return (
    <>
      <span className="text-ink-faint">
        {status === null ? "—" : "недоступні: немає ffmpeg"}
      </span>
      {status !== null && status.downloadable ? (
        <button
          type="button"
          disabled={busy}
          onClick={() => void download()}
          className="rounded border border-accent/60 bg-panel-2 px-2 py-0.5 text-accent hover:bg-accent/10 disabled:cursor-not-allowed disabled:text-ink-faint"
        >
          {busy ? "Завантаження… (~130 МБ)" : "📥 Завантажити FFmpeg (1 клік)"}
        </button>
      ) : null}
      {status !== null && !status.downloadable ? (
        <span className="text-ink-faint">
          для цієї платформи готової збірки немає
        </span>
      ) : null}
      {error ? <span className="text-reap">{error}</span> : null}
    </>
  );
}
