import { useCallback, useEffect, useMemo, useState } from "react";

import { command, isTauri } from "@/ipc/client";
import type {
  DeclineElevationReply,
  HealthInfo,
  RequestElevationReply,
} from "@/ipc/types";

type HealthState =
  | { status: "loading"; data: null; error: null }
  | { status: "ready"; data: HealthInfo; error: null }
  | { status: "error"; data: HealthInfo | null; error: string };

const MODULE_LABELS: Record<string, string> = {
  shell: "Shell",
  "ipc.commands": "IPC commands",
  "ipc.events": "IPC events",
  scanner: "Scanner",
  index: "Index",
  quarantine: "Quarantine",
};

function statusClass(status: string): string {
  switch (status) {
    case "online":
      return "border-keep/40 bg-keep/10 text-keep";
    case "planned":
      return "border-line bg-panel-2 text-ink-dim";
    case "degraded":
      return "border-quarantine/40 bg-quarantine/10 text-quarantine";
    default:
      return "border-reap/40 bg-reap/10 text-reap";
  }
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat("uk-UA").format(value);
}

function strategyLabel(strategy: string): string {
  switch (strategy) {
    case "mft":
      return "MFT";
    case "directory_walk":
      return "Walk";
    case "usn_delta":
      return "USN";
    default:
      return strategy;
  }
}

function reasonLabel(reason: string): string {
  switch (reason) {
    case "ntfs_elevated":
      return "NTFS + admin";
    case "not_ntfs":
      return "not NTFS";
    case "not_elevated":
      return "no elevation";
    default:
      return reason;
  }
}

function elevationStatusLabel(status: string): string {
  switch (status) {
    case "elevated":
      return "admin (MFT available)";
    case "offer":
      return "offer pending";
    case "declined":
      return "declined this session → walk";
    case "not_needed":
      return "not needed (no NTFS)";
    default:
      return status;
  }
}

export function HealthScreen() {
  const [state, setState] = useState<HealthState>({
    status: "loading",
    data: null,
    error: null,
  });
  const [elevationBusy, setElevationBusy] = useState(false);
  const [elevationNote, setElevationNote] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!isTauri()) {
      setState({
        status: "error",
        data: null,
        error: "IPC недоступний поза Tauri.",
      });
      return;
    }

    try {
      const data = await command<HealthInfo>("app.health");
      setState({ status: "ready", data, error: null });
    } catch (error) {
      setState((previous) => ({
        status: "error",
        data: previous.data,
        error: error instanceof Error ? error.message : String(error),
      }));
    }
  }, []);

  useEffect(() => {
    let cancelled = false;

    async function tick() {
      if (cancelled) return;
      await refresh();
    }

    void tick();
    const intervalId = window.setInterval(() => {
      void tick();
    }, 1000);
    return () => {
      cancelled = true;
      window.clearInterval(intervalId);
    };
  }, [refresh]);

  async function onRequestElevation() {
    setElevationBusy(true);
    setElevationNote(null);
    try {
      const reply = await command<RequestElevationReply>("app.request_elevation");
      if (reply.willExit) {
        setElevationNote("UAC прийнято — перезапуск з адмін-правами…");
      } else {
        setElevationNote("Уже з адмін-правами.");
        await refresh();
      }
    } catch (error) {
      setElevationNote(
        error instanceof Error ? error.message : String(error),
      );
      await refresh();
    } finally {
      setElevationBusy(false);
    }
  }

  async function onDeclineElevation() {
    setElevationBusy(true);
    setElevationNote(null);
    try {
      const reply = await command<DeclineElevationReply>("app.decline_elevation");
      setElevationNote(
        reply.declined
          ? "Відмова на сесію: сканування через обхід каталогів, без повторних запитів."
          : "Відмову не зафіксовано.",
      );
      await refresh();
    } catch (error) {
      setElevationNote(
        error instanceof Error ? error.message : String(error),
      );
    } finally {
      setElevationBusy(false);
    }
  }

  const metrics = useMemo(() => {
    const ipc = state.data?.ipc;
    return [
      { label: "Commands", value: ipc?.commandsReceived ?? 0 },
      { label: "Command errors", value: ipc?.commandErrors ?? 0 },
      { label: "Events", value: ipc?.eventsEmitted ?? 0 },
      { label: "Event errors", value: ipc?.eventErrors ?? 0 },
    ];
  }, [state.data]);

  return (
    <div className="flex h-full flex-col gap-5 px-6 py-4">
      <div className="flex items-end justify-between border-b border-line pb-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-normal text-ink">Health</h1>
          <p className="mt-1 text-sm text-ink-dim">Core diagnostics</p>
        </div>
        <div className="text-right">
          <div className="font-mono text-sm text-ink">
            v{state.data?.appVersion ?? "—"}
          </div>
          <div className="mt-1 text-xs text-ink-faint">
            {state.status === "ready" ? "live" : state.status}
          </div>
        </div>
      </div>

      {state.error ? (
        <div className="rounded border border-quarantine/40 bg-quarantine/10 px-3 py-2 text-sm text-quarantine">
          {state.error}
        </div>
      ) : null}

      <section>
        <h2 className="text-xs font-semibold uppercase tracking-wider text-ink-dim">
          Core
        </h2>
        <div className="mt-2 grid grid-cols-3 gap-3">
          <div className="rounded border border-line bg-panel px-3 py-3">
            <div className="text-xs text-ink-faint">Status</div>
            <div className="mt-1 font-mono text-lg text-ink">
              {state.data?.coreStatus ?? "—"}
            </div>
          </div>
          <div className="rounded border border-line bg-panel px-3 py-3">
            <div className="text-xs text-ink-faint">Runtime</div>
            <div className="mt-1 font-mono text-lg text-ink">Tauri</div>
          </div>
          <div className="rounded border border-line bg-panel px-3 py-3">
            <div className="text-xs text-ink-faint">Elevation</div>
            <div className="mt-1 font-mono text-lg text-ink">
              {state.data
                ? state.data.elevated
                  ? "admin"
                  : "user"
                : "—"}
            </div>
          </div>
        </div>
      </section>

      <section>
        <h2 className="text-xs font-semibold uppercase tracking-wider text-ink-dim">
          Elevation (T-034)
        </h2>
        <p className="mt-1 text-xs text-ink-faint">
          Сесійний запит адмін-прав для MFT; відмова → walk без повторних запитів
        </p>
        <div className="mt-2 rounded border border-line bg-panel px-3 py-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="font-mono text-sm text-ink">
              {state.data?.elevation
                ? elevationStatusLabel(state.data.elevation.status)
                : "—"}
            </div>
            <div className="text-xs text-ink-faint">
              NTFS volumes:{" "}
              {state.data?.elevation?.ntfsVolumeCount ?? "—"}
            </div>
          </div>
          {state.data?.elevation?.message ? (
            <p className="mt-2 text-sm leading-relaxed text-ink-dim">
              {state.data.elevation.message}
            </p>
          ) : null}
          <div className="mt-3 flex flex-wrap gap-2">
            <button
              type="button"
              disabled={
                elevationBusy ||
                !isTauri() ||
                state.data?.elevated === true ||
                state.data?.elevation?.status === "not_needed"
              }
              onClick={() => void onRequestElevation()}
              className="rounded border border-keep/40 bg-keep/10 px-3 py-1.5 text-sm text-keep disabled:cursor-not-allowed disabled:opacity-40"
            >
              Увімкнути швидке сканування (UAC)
            </button>
            <button
              type="button"
              disabled={
                elevationBusy ||
                !isTauri() ||
                state.data?.elevated === true ||
                state.data?.elevation?.declinedThisSession === true
              }
              onClick={() => void onDeclineElevation()}
              className="rounded border border-line bg-panel-2 px-3 py-1.5 text-sm text-ink-dim disabled:cursor-not-allowed disabled:opacity-40"
            >
              Продовжити без прав
            </button>
          </div>
          {elevationNote ? (
            <p className="mt-2 text-xs text-quarantine">{elevationNote}</p>
          ) : null}
        </div>
      </section>

      <section>
        <h2 className="text-xs font-semibold uppercase tracking-wider text-ink-dim">
          Scan strategy
        </h2>
        <p className="mt-1 text-xs text-ink-faint">
          Автовибір MFT ↔ walk (T-028): NTFS+admin → MFT, інакше → walk
        </p>
        <div className="mt-2 flex flex-col gap-2">
          {(state.data?.scanPlans ?? []).length === 0 ? (
            <div className="rounded border border-line bg-panel px-3 py-2 text-sm text-ink-dim">
              Немає томів для планування
            </div>
          ) : (
            (state.data?.scanPlans ?? []).map((plan) => (
              <div
                key={plan.volume}
                className="flex items-center justify-between gap-3 rounded border border-line bg-panel px-3 py-2"
              >
                <div className="min-w-0">
                  <div className="font-mono text-sm text-ink">{plan.volume}</div>
                  <div className="truncate text-xs text-ink-faint">
                    {plan.fileSystem} · {reasonLabel(plan.reason)}
                  </div>
                </div>
                <span
                  className={`shrink-0 rounded border px-2 py-0.5 font-mono text-xs ${
                    plan.strategy === "mft"
                      ? "border-keep/40 bg-keep/10 text-keep"
                      : "border-line bg-panel-2 text-ink-dim"
                  }`}
                >
                  {strategyLabel(plan.strategy)}
                </span>
              </div>
            ))
          )}
        </div>
      </section>

      <section>
        <h2 className="text-xs font-semibold uppercase tracking-wider text-ink-dim">
          Quarantine (T-089)
        </h2>
        <p className="mt-1 text-xs text-ink-faint">
          Том без записуваного каталогу карантину не підтримує reap — причина
          показується тут і біля файлів
        </p>
        <div className="mt-2 flex flex-col gap-2">
          {(state.data?.quarantineVolumes ?? []).length === 0 ? (
            <div className="rounded border border-line bg-panel px-3 py-2 text-sm text-ink-dim">
              Перевірка доступності карантину триває…
            </div>
          ) : (
            (state.data?.quarantineVolumes ?? []).map((volumeInfo) => (
              <div
                key={volumeInfo.volume}
                className="flex items-center justify-between gap-3 rounded border border-line bg-panel px-3 py-2"
              >
                <div className="min-w-0">
                  <div className="font-mono text-sm text-ink">
                    {volumeInfo.volume}
                  </div>
                  {volumeInfo.reason ? (
                    <div className="text-xs leading-relaxed text-quarantine">
                      {volumeInfo.reason}
                    </div>
                  ) : (
                    <div className="text-xs text-ink-faint">
                      Каталог карантину записуваний
                    </div>
                  )}
                </div>
                <span
                  className={`shrink-0 rounded border px-2 py-0.5 font-mono text-xs ${
                    volumeInfo.available
                      ? "border-keep/40 bg-keep/10 text-keep"
                      : "border-reap/40 bg-reap/10 text-reap"
                  }`}
                >
                  {volumeInfo.available ? "reap OK" : "reap заблоковано"}
                </span>
              </div>
            ))
          )}
        </div>
      </section>

      <section>
        <h2 className="text-xs font-semibold uppercase tracking-wider text-ink-dim">
          Modules
        </h2>
        <div className="mt-2 grid grid-cols-3 gap-2">
          {(state.data?.modules ?? []).map((module) => (
            <div
              key={module.name}
              className="flex items-center justify-between rounded border border-line bg-panel px-3 py-2"
            >
              <span className="truncate text-sm text-ink">
                {MODULE_LABELS[module.name] ?? module.name}
              </span>
              <span
                className={`ml-2 rounded border px-2 py-0.5 font-mono text-xs ${statusClass(
                  module.status,
                )}`}
              >
                {module.status}
              </span>
            </div>
          ))}
        </div>
      </section>

      <section>
        <h2 className="text-xs font-semibold uppercase tracking-wider text-ink-dim">
          IPC
        </h2>
        <div className="mt-2 grid grid-cols-4 gap-3">
          {metrics.map((metric) => (
            <div key={metric.label} className="rounded border border-line bg-panel px-3 py-3">
              <div className="text-xs text-ink-faint">{metric.label}</div>
              <div className="mt-1 font-mono text-xl text-ink">
                {formatNumber(metric.value)}
              </div>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}
