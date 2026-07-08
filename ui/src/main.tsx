import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { RouterProvider } from "react-router-dom";

import { router } from "./app/routes";
import "./design/theme.css";

const rootElement = document.getElementById("root");
if (!rootElement) {
  throw new Error("Не знайдено #root у index.html");
}

createRoot(rootElement).render(
  <StrictMode>
    <RouterProvider router={router} />
  </StrictMode>,
);
