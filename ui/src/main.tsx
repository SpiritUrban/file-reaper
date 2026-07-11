import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { RouterProvider } from "react-router-dom";

import { hotkeys } from "./hotkeys";
import { command, isTauri, subscribe } from "./ipc/client";
import type { PingReply, TestEvent, TestStreamAck } from "./ipc/types";

import { router } from "./app/routes";
import { appStateStore } from "./store/appState";
import "./design/theme.css";

// Self-check IPC при старті: канал команд (T-004) + канал подій (T-005).
// У браузері (npm run dev) IPC відсутній — перевірка пропускається.
async function ipcSelfCheck(): Promise<void> {
  const reply = await command<PingReply>("app.ping");
  console.info(`Core на зв'язку: v${reply.version}`);

  const expected = 3;
  let received = 0;
  const unlisten = await subscribe<TestEvent>("app.test", (event) => {
    received += 1;
    if (event.seq === event.of) {
      unlisten();
      console.info(`Канал подій працює: ${received}/${expected} доставлено`);
    }
  });
  await command<TestStreamAck>("app.test_stream", {
    payload: { count: expected, intervalMs: 10 },
  });
}

hotkeys.attach(window);

void appStateStore.start().catch((error) =>
  console.warn("Відновлення UI state не пройшло:", error),
);

if (isTauri()) {
  ipcSelfCheck().catch((error) => console.warn("Self-check IPC не пройшов:", error));
}

const rootElement = document.getElementById("root");
if (!rootElement) {
  throw new Error("Не знайдено #root у index.html");
}

createRoot(rootElement).render(
  <StrictMode>
    <RouterProvider router={router} />
  </StrictMode>,
);
