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

/**
 * Знайти ffmpeg, покладений у репо (напр. розпакований gyan.dev build), щоб
 * відео-превʼю (T-071) працювали без ручного налаштування середовища. Порядок
 * дублює Rust `discover_ffmpeg`, але дає значення ще до старту процесу.
 */
function findRepoFfmpeg() {
  const exe = process.platform === "win32" ? "ffmpeg.exe" : "ffmpeg";
  const direct = [
    path.join(root, exe),
    path.join(root, "vendor", "ffmpeg", exe),
    path.join(root, "vendor", "ffmpeg", "bin", exe),
  ];
  for (const candidate of direct) {
    if (fs.existsSync(candidate)) return candidate;
  }
  // Розпаковані збірки: теки виду `ffmpeg-*` у корені → `<dir>/bin/<exe>`.
  for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
    if (!entry.isDirectory() || !entry.name.toLowerCase().startsWith("ffmpeg")) {
      continue;
    }
    for (const sub of [path.join("bin", exe), exe]) {
      const candidate = path.join(root, entry.name, sub);
      if (fs.existsSync(candidate)) return candidate;
    }
  }
  return null;
}

const env = { ...process.env };
if (!env.TRASHRADAR_FFMPEG) {
  const ffmpeg = findRepoFfmpeg();
  if (ffmpeg) {
    env.TRASHRADAR_FFMPEG = ffmpeg;
    console.log(`[tauri.mjs] TRASHRADAR_FFMPEG → ${ffmpeg}`);
  }
}

const child = spawn(tauriCmd, args, {
  cwd: coreDir,
  stdio: "inherit",
  shell: process.platform === "win32",
  env,
});

child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  process.exit(code ?? 1);
});
