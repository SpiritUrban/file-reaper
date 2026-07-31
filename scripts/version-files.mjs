// Спільне джерело правди для sync-version.mjs і check-version.mjs
// (розділ 6.5 брифу Стадії 2).
//
// Версія TrashRadar дублюється в п'яти місцях. Тримати список тут, а не в
// двох скриптах, — щоб «синхронізувати» і «перевірити» ніколи не розійшлися
// в тому, ЩО саме вони вважають версією проєкту.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..');

/** Крейти воркспейсу — їхні рядки версій у Cargo.lock оновлюються разом. */
export const WORKSPACE_CRATES = [
  'trashradar-app',
  'trashradar-domain',
  'trashradar-hash',
  'trashradar-index-memory',
  'trashradar-index-sqlite',
  'trashradar-platform-win',
  'trashradar-preview',
  'trashradar-quarantine-fs',
  'trashradar-scan-mft',
  'trashradar-scan-usn',
  'trashradar-scan-walk',
  'trashradar-settings-json',
  'trashradar-shell',
  'trashradar-testkit',
];

export const SEMVER = /^\d+\.\d+\.\d+$/;

/**
 * Кожен запис описує один носій версії: як прочитати і як записати.
 * `read` повертає рядок версії або кидає — «не знайшов» тут завжди помилка,
 * а не тиха відсутність (правило 33: перевірка, яка нічого не читає,
 * проходить завжди).
 */
export const VERSION_FILES = [
  {
    label: 'package.json',
    path: join(repoRoot, 'package.json'),
    read: (text) => requireMatch(text, /"version"\s*:\s*"([^"]+)"/, 'package.json'),
    write: (text, version) => text.replace(/"version"\s*:\s*"[^"]+"/, `"version": "${version}"`),
  },
  {
    label: 'ui/package.json',
    path: join(repoRoot, 'ui', 'package.json'),
    read: (text) => requireMatch(text, /"version"\s*:\s*"([^"]+)"/, 'ui/package.json'),
    write: (text, version) => text.replace(/"version"\s*:\s*"[^"]+"/, `"version": "${version}"`),
  },
  {
    label: 'core/shell/tauri.conf.json',
    path: join(repoRoot, 'core', 'shell', 'tauri.conf.json'),
    read: (text) => requireMatch(text, /"version"\s*:\s*"([^"]+)"/, 'tauri.conf.json'),
    write: (text, version) => text.replace(/"version"\s*:\s*"[^"]+"/, `"version": "${version}"`),
  },
  {
    label: 'core/Cargo.toml',
    // [workspace.package] version — успадковують усі крейти через version.workspace.
    path: join(repoRoot, 'core', 'Cargo.toml'),
    read: (text) =>
      requireMatch(
        text,
        /\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m,
        'core/Cargo.toml [workspace.package]',
      ),
    write: (text, version) =>
      replaceOnce(
        text,
        /(\[workspace\.package\][\s\S]*?^version\s*=\s*")[^"]+(")/m,
        `$1${version}$2`,
        'core/Cargo.toml [workspace.package]',
      ),
  },
  {
    label: 'core/Cargo.lock',
    // Регексом, без запуску cargo: `cargo update` тягне мережу, а в
    // validate-version джобі її може не бути взагалі.
    path: join(repoRoot, 'core', 'Cargo.lock'),
    read: (text) => {
      const versions = new Set();
      for (const crate of WORKSPACE_CRATES) {
        versions.add(readLockVersion(text, crate));
      }
      if (versions.size !== 1) {
        throw new Error(
          `core/Cargo.lock: крейти воркспейсу мають різні версії: ${[...versions].sort().join(', ')}`,
        );
      }
      return [...versions][0];
    },
    write: (text, version) => {
      let out = text;
      for (const crate of WORKSPACE_CRATES) {
        out = replaceOnce(
          out,
          new RegExp(`(name = "${crate}"\\nversion = ")[^"]+(")`),
          `$1${version}$2`,
          `core/Cargo.lock: ${crate}`,
        );
      }
      return out;
    },
  },
];

export function readVersions() {
  return VERSION_FILES.map((file) => ({
    label: file.label,
    version: file.read(readFileSync(file.path, 'utf8')),
  }));
}

function readLockVersion(text, crate) {
  const match = text.match(new RegExp(`name = "${crate}"\\nversion = "([^"]+)"`));
  if (!match) {
    throw new Error(`core/Cargo.lock: не знайдено запис крейта ${crate}`);
  }
  return match[1];
}

function requireMatch(text, pattern, where) {
  const match = text.match(pattern);
  if (!match) {
    throw new Error(`${where}: поле версії не знайдено`);
  }
  return match[1];
}

function replaceOnce(text, pattern, replacement, where) {
  const out = text.replace(pattern, replacement);
  if (out === text) {
    throw new Error(`${where}: заміна нічого не змінила — шаблон більше не збігається`);
  }
  return out;
}
