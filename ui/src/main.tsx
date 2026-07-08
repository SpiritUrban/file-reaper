import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { RouterProvider } from "react-router-dom";

import { command, isTauri } from "./ipc/client";
import type { PingReply } from "./ipc/types";

import { router } from "./app/routes";
import "./design/theme.css";

// Handshake з Core при старті (T-004): підтверджує живий канал команд.
// У браузері (npm run dev) IPC відсутній — handshake пропускається.
if (isTauri()) {
  command<PingReply>("app.ping")
    .then((reply) => console.info(`Core на зв'язку: v${reply.version}`))
    .catch((error) => console.warn("Handshake з Core не пройшов:", error));
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
