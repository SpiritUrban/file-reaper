#!/usr/bin/env node
/**
 * Запуск Tauri з кореня репо.
 *
 * CLI шукає tauri.conf.json у cwd і підпапках. Конфіг лежить у
 * core/shell/tauri.conf.json → стартуємо з core/ (не з ui/).
 *
 * Usage: node scripts/tauri.mjs dev | build [...extra args]
 */
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const coreDir = path.join(root, "core");
const configAbs = path.join(coreDir, "shell", "tauri.conf.json");

const tauriCmd = path.join(
  root,
  "ui",
  "node_modules",
  ".bin",
  process.platform === "win32" ? "tauri.cmd" : "tauri",
);

if (!fs.existsSync(tauriCmd)) {
  console.error("Tauri CLI not found. From repo root run:\n  npm run setup");
  process.exit(1);
}
if (!fs.existsSync(configAbs)) {
  console.error(`Missing config: ${configAbs}`);
  process.exit(1);
}

const args = process.argv.slice(2);
if (args.length === 0) {
  console.error("Usage: node scripts/tauri.mjs <dev|build> [args...]");
  process.exit(1);
}

const child = spawn(tauriCmd, args, {
  cwd: coreDir,
  stdio: "inherit",
  shell: process.platform === "win32",
  env: process.env,
});

child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  process.exit(code ?? 1);
});
