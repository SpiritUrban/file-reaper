#!/usr/bin/env node
// Розставляє одну версію по всіх носіях (розділ 6.5 брифу Стадії 2).
//
//   node scripts/sync-version.mjs 0.2.0
//
// Cargo.lock правиться регексом, без запуску cargo: інакше знадобилась би
// мережа, а `cargo update` заразом підтягнув би сторонні крейти — реліз
// перестав би бути відтворюваним.

import { readFileSync, writeFileSync } from 'node:fs';

import { SEMVER, VERSION_FILES } from './version-files.mjs';

const version = (process.argv[2] || '').replace(/^v/, '');

if (!SEMVER.test(version)) {
  console.error('usage: node scripts/sync-version.mjs <major.minor.patch>');
  process.exit(1);
}

for (const file of VERSION_FILES) {
  const before = readFileSync(file.path, 'utf8');
  const after = file.write(before, version);
  if (before === after) {
    console.log(`  = ${file.label} (вже ${version})`);
    continue;
  }
  writeFileSync(file.path, after);
  console.log(`  ✎ ${file.label} → ${version}`);
}

console.log(`\nВерсія ${version} розставлена. Перевірка: node scripts/check-version.mjs`);
