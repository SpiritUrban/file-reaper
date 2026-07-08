import { fileURLToPath, URL } from "node:url";

import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

// Порт 5173 зафіксований у core/shell/tauri.conf.json (devUrl).
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    port: 5173,
    strictPort: true,
  },
  // Tauri: відносні шляхи для вбудованої роздачі з frontendDist.
  base: "./",
  clearScreen: false,
});
