#!/usr/bin/env node
// Маніфест завантажень для сайту-вітрини (розділ 6.6 брифу Стадії 2).
//
// Імена артефактів НІКОЛИ не вигадуються (правило 15): Tauri іменує бандли за
// productName, а GitHub замінює пробіли на крапки — вгадати це неможливо, тому
// список береться з GitHub API.
//
//   GITHUB_TOKEN=... node scripts/generate-download-manifest.mjs
//
// Без токена GitHub дає 60 запитів/год на IP — на раннері цього не вистачає,
// і відповідь приходить 403 замість релізу.

import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import { repoRoot } from './version-files.mjs';

const OWNER = 'SpiritUrban';
const REPO = 'file-reaper';
const OUTPUT = join(repoRoot, 'site', 'download-manifest.json');

const ref = process.env.GITHUB_REF_NAME || '';
const isTag = /^v\d+\.\d+\.\d+$/.test(ref);

// Деплой, викликаний з тега, мусить показати САМЕ цей тег: `releases/latest`
// у ту ж секунду ще може віддавати попередній реліз, і сайт вийде зі старою
// версією при зеленому рані (розділ 11).
const apiUrl = isTag
  ? `https://api.github.com/repos/${OWNER}/${REPO}/releases/tags/${ref}`
  : `https://api.github.com/repos/${OWNER}/${REPO}/releases/latest`;

const headers = {
  'User-Agent': `${REPO}-site-builder`,
  Accept: 'application/vnd.github+json',
};
if (process.env.GITHUB_TOKEN) {
  headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
}

const fallbackVersion = JSON.parse(
  readFileSync(join(repoRoot, 'package.json'), 'utf8'),
).version;

const manifest = await buildManifest();
writeFileSync(OUTPUT, `${JSON.stringify(manifest, null, 2)}\n`);

console.log(
  `download-manifest.json: ref=${ref || '(немає)'} -> version ${manifest.version}, ` +
    `${manifest.assets.length} ассетів (джерело: ${manifest.source})`,
);

async function buildManifest() {
  let release = null;
  try {
    const response = await fetch(apiUrl, { headers });
    if (response.ok) {
      release = await response.json();
    } else if (response.status === 404) {
      console.warn(`Реліз не знайдено (${apiUrl}) — маніфест без ассетів.`);
    } else {
      throw new Error(`GitHub API ${response.status} ${response.statusText}`);
    }
  } catch (error) {
    // Мережа/ліміт не мають робити сайт неможливим до збірки: краще вітрина
    // з кнопкою «усі релізи», ніж червоний деплой.
    console.warn(`Реліз не отримано: ${error.message}`);
  }

  if (!release) {
    // Правило 6.6: ніколи не вигадувати імена файлів. Кнопка веде на
    // сторінку релізів, а не на 404.
    return {
      version: fallbackVersion,
      tag: null,
      source: 'fallback',
      releaseUrl: `https://github.com/${OWNER}/${REPO}/releases`,
      publishedAt: null,
      assets: [],
    };
  }

  return {
    version: String(release.tag_name || '').replace(/^v/, '') || fallbackVersion,
    tag: release.tag_name ?? null,
    source: isTag ? 'tag' : 'latest',
    releaseUrl:
      release.html_url ?? `https://github.com/${OWNER}/${REPO}/releases`,
    publishedAt: release.published_at ?? null,
    assets: describeAssets(release.assets || []),
  };
}

function describeAssets(assets) {
  return assets
    .filter((asset) => {
      // Правило 17: підписи й маніфест апдейтера — не збірки.
      const name = asset.name.toLowerCase();
      return !name.endsWith('.sig') && name !== 'latest.json';
    })
    .map((asset) => {
      const name = asset.name.toLowerCase();
      return {
        platform: platformOf(name),
        architecture: architectureOf(name),
        kind: kindOf(name),
        fileName: asset.name,
        sizeBytes: asset.size ?? null,
        downloadUrl: asset.browser_download_url,
      };
    })
    .sort((a, b) => a.fileName.localeCompare(b.fileName));
}

/**
 * Правило 16: платформа визначається за РОЗШИРЕННЯМ, а не за словом у назві.
 * `.rpm` і `.app.tar.gz` не містять жодного платформного слова і без цього
 * потрапили б у Windows як «усе інше».
 */
function platformOf(name) {
  if (
    name.endsWith('.dmg') ||
    name.endsWith('.app.tar.gz') ||
    name.includes('macos') ||
    name.includes('darwin')
  ) {
    return 'macos';
  }
  if (
    name.endsWith('.appimage') ||
    name.endsWith('.appimage.tar.gz') ||
    name.endsWith('.deb') ||
    name.endsWith('.rpm') ||
    name.includes('linux')
  ) {
    return 'linux';
  }
  return 'windows';
}

function architectureOf(name) {
  if (name.includes('arm64') || name.includes('aarch64')) return 'arm64';
  return 'x64';
}

/**
 * Формат пакета. Під Windows їх два (.exe і .msi), під Linux теж
 * (.AppImage і .deb) — без цього поля картка «MSI» повела б на `.exe`
 * (розділ 6.6).
 */
function kindOf(name) {
  if (name.endsWith('.msi')) return 'msi';
  if (name.endsWith('.exe')) return 'nsis';
  if (name.endsWith('.deb')) return 'deb';
  if (name.endsWith('.rpm')) return 'rpm';
  if (name.endsWith('.appimage')) return 'appimage';
  if (name.endsWith('.dmg')) return 'dmg';
  if (name.endsWith('.tar.gz') || name.endsWith('.zip')) return 'updater';
  return 'other';
}
