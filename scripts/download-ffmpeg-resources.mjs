#!/usr/bin/env node
// Комплектація ffmpeg/ffprobe у бандл (правило 29 брифу Стадії 2).
//
// Продукт не має вимагати від користувача ручних дій. «Скачайте FFmpeg і
// пропишіть PATH» — бар'єр, через який відео-превʼю просто не працюють у
// більшості встановлених копій.
//
//   node scripts/download-ffmpeg-resources.mjs [--target <rust-target>]
//
// У CI викликається перед `tauri build`; `--target` потрібен лише там, де
// збірка кросплатформна (macos-x64 з arm64-раннера).
//
// Джерело: ffbinaries.com — статичні збірки без залежностей.
// ВАЖЛИВО щодо ключів платформ: перевірено запитом до API, а не з пам'яті.
// Реальні ключі — windows-64, linux-64, linux-arm64, osx-64. Ключа
// «macos-64» не існує, і збірки під macOS **arm64 теж немає**: для неї
// бінарник не комплектується, і це видно в логах рану, а не мовчки.

import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { chmodSync, mkdirSync, statSync, writeFileSync } from 'node:fs';
import { arch, platform } from 'node:process';
import { join } from 'node:path';
import { inflateRawSync } from 'node:zlib';

import { repoRoot } from './version-files.mjs';

const API = 'https://ffbinaries.com/api/v1/version/latest';
const OUTPUT_DIR = join(repoRoot, 'core', 'shell', 'resources');
// Лише ffmpeg. ffprobe у Core не викликається жодного разу (перевірено
// grep'ом по core/), а це рівно стільки ж мегабайтів у інсталяторі: класти
// його «про запас» означало б подвоїти вагу завантаження заради нуля.
const WANTED = ['ffmpeg'];

const targetArg = argValue('--target') || process.env.TRASHRADAR_TARGET || '';
const key = platformKey(targetArg);

mkdirSync(OUTPUT_DIR, { recursive: true });

if (!key) {
  // Не помилка збірки: без ffmpeg відео-превʼю деградують до іконки, а не
  // ламаються. Але мовчати не можна — інакше «чому на Mac немає кадрів»
  // з'ясовується у користувача.
  notice(
    'warning',
    'FFmpeg not bundled',
    `для цілі «${targetArg || `${platform}/${arch}`}» статичної збірки ffmpeg немає ` +
      '(ffbinaries не публікує macOS arm64). Відео-превʼю в цій збірці працюватимуть ' +
      'лише після завантаження ffmpeg кнопкою в самому застосунку.',
  );
  process.exit(0);
}

const index = await fetchJson(API);
const bin = index.bin?.[key];
if (!bin) {
  fail(`ffbinaries не має платформи «${key}». Доступні: ${Object.keys(index.bin || {}).join(', ')}`);
}

console.log(`FFmpeg ${index.version} для ${key} → ${OUTPUT_DIR}`);

const written = [];
for (const name of WANTED) {
  const url = bin[name];
  if (!url) {
    fail(`ffbinaries не має «${name}» для ${key}`);
  }
  const archive = await fetchBuffer(url);
  const exeName = key.startsWith('windows') ? `${name}.exe` : name;
  const entry = findEntry(archive, exeName);
  if (!entry) {
    fail(`в архіві ${url} немає файла ${exeName}`);
  }
  const dest = join(OUTPUT_DIR, exeName);
  writeFileSync(dest, entry);
  if (!key.startsWith('windows')) {
    chmodSync(dest, 0o755);
  }
  written.push({ name: exeName, path: dest, size: entry.length });
}

// Правило 31: перевіряти наслідок, а не передумову. «Файл на місці» — це
// інвентаризація. Нижче — розмір, а де це можливо, ще й реальний запуск.
for (const file of written) {
  const size = statSync(file.path).size;
  if (size < 1_000_000) {
    fail(`${file.name}: ${size} байт — це не бінарник ffmpeg (обрізане завантаження?)`);
  }
  console.log(`  ${file.name.padEnd(12)} ${(size / 1024 / 1024).toFixed(1)} МБ`);
}

if (canRunHere(key)) {
  const ffmpeg = join(OUTPUT_DIR, key.startsWith('windows') ? 'ffmpeg.exe' : 'ffmpeg');
  const banner = execFileSync(ffmpeg, ['-version'], { encoding: 'utf8' }).split('\n')[0];
  console.log(`  запуск підтверджено: ${banner.trim()}`);
} else {
  console.log('  запуск не перевіряється: бінарник під іншу архітектуру, ніж раннер');
}

console.log('FFmpeg покладено в resources — бандл піде з робочими відео-превʼю.');

function platformKey(rustTarget) {
  const target = rustTarget || '';
  if (target) {
    if (target.includes('windows')) return 'windows-64';
    if (target.includes('linux')) {
      return target.startsWith('aarch64') ? 'linux-arm64' : 'linux-64';
    }
    if (target.includes('apple-darwin')) {
      // Ключ osx-64 — це x86_64. Під aarch64-apple-darwin статичної збірки
      // в ffbinaries немає; підкладати x64-бінарник в arm64-застосунок не
      // можна: без Rosetta він просто не запуститься.
      return target.startsWith('x86_64') ? 'osx-64' : null;
    }
    return null;
  }
  if (platform === 'win32') return 'windows-64';
  if (platform === 'linux') return arch === 'arm64' ? 'linux-arm64' : 'linux-64';
  if (platform === 'darwin') return arch === 'x64' ? 'osx-64' : null;
  return null;
}

function canRunHere(platformKeyValue) {
  if (platformKeyValue === 'windows-64') return platform === 'win32' && arch === 'x64';
  if (platformKeyValue === 'linux-64') return platform === 'linux' && arch === 'x64';
  if (platformKeyValue === 'linux-arm64') return platform === 'linux' && arch === 'arm64';
  if (platformKeyValue === 'osx-64') return platform === 'darwin' && arch === 'x64';
  return false;
}

async function fetchJson(url) {
  const response = await fetch(url, { headers: { 'User-Agent': 'trashradar-build' } });
  if (!response.ok) fail(`${url}: HTTP ${response.status}`);
  return response.json();
}

async function fetchBuffer(url) {
  const response = await fetch(url, { headers: { 'User-Agent': 'trashradar-build' } });
  if (!response.ok) fail(`${url}: HTTP ${response.status}`);
  const buffer = Buffer.from(await response.arrayBuffer());
  console.log(
    `  ↓ ${url.split('/').pop()} ${(buffer.length / 1024 / 1024).toFixed(1)} МБ ` +
      `sha256:${createHash('sha256').update(buffer).digest('hex').slice(0, 12)}`,
  );
  return buffer;
}

/**
 * Мінімальний читач zip: центральний каталог → потрібний запис → inflate.
 *
 * Навмисно без зовнішніх утиліт: `unzip` немає на Windows-раннері, а
 * Expand-Archive немає ніде, крім Windows, — обидва варіанти дали б крок,
 * який працює рівно на одній платформі з чотирьох.
 */
function findEntry(zip, wantedName) {
  const eocd = findEocd(zip);
  if (eocd < 0) fail('zip: не знайдено кінець центрального каталогу');

  const total = zip.readUInt16LE(eocd + 10);
  let offset = zip.readUInt32LE(eocd + 16);

  for (let i = 0; i < total; i += 1) {
    if (zip.readUInt32LE(offset) !== 0x02014b50) fail('zip: побитий центральний каталог');
    const method = zip.readUInt16LE(offset + 10);
    const compressedSize = zip.readUInt32LE(offset + 20);
    const nameLength = zip.readUInt16LE(offset + 28);
    const extraLength = zip.readUInt16LE(offset + 30);
    const commentLength = zip.readUInt16LE(offset + 32);
    const localOffset = zip.readUInt32LE(offset + 42);
    const name = zip.toString('utf8', offset + 46, offset + 46 + nameLength);

    // Архіви ffbinaries кладуть бінарник у корінь, але базове ім'я —
    // єдине, на що можна спиратися між платформами.
    if (name.split('/').pop() === wantedName) {
      return readLocal(zip, localOffset, method, compressedSize);
    }
    offset += 46 + nameLength + extraLength + commentLength;
  }
  return null;
}

function readLocal(zip, localOffset, method, compressedSize) {
  if (zip.readUInt32LE(localOffset) !== 0x04034b50) fail('zip: побитий локальний заголовок');
  const nameLength = zip.readUInt16LE(localOffset + 26);
  const extraLength = zip.readUInt16LE(localOffset + 28);
  const start = localOffset + 30 + nameLength + extraLength;
  const body = zip.subarray(start, start + compressedSize);
  if (method === 0) return Buffer.from(body);
  if (method === 8) return inflateRawSync(body);
  fail(`zip: метод стиснення ${method} не підтримується`);
}

function findEocd(zip) {
  // Коментар архіву максимум 64 КБ — далі назад шукати немає сенсу.
  const from = Math.max(0, zip.length - 0x10000 - 22);
  for (let i = zip.length - 22; i >= from; i -= 1) {
    if (zip.readUInt32LE(i) === 0x06054b50) return i;
  }
  return -1;
}

function argValue(flag) {
  const index = process.argv.indexOf(flag);
  return index >= 0 ? process.argv[index + 1] : null;
}

function notice(level, title, message) {
  if (process.env.GITHUB_ACTIONS) {
    console.log(`::${level} title=${title}::${message}`);
  } else {
    console.log(`[${level}] ${title}: ${message}`);
  }
}

function fail(message) {
  notice('error', 'FFmpeg resources', message);
  process.exit(1);
}
