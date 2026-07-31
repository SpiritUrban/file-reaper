#!/usr/bin/env node
// Звіряє версію в усіх носіях між собою і з іменем тега (розділ 6.5).
//
//   node scripts/check-version.mjs
//
// У джобі validate-version це єдиний бар'єр між «тег v0.2.0» і релізом, у
// якому інсталятор називається 0.1.0, а апдейтер ніколи не спрацює: клієнт
// порівнює версію з бандла, а не з імені тега.

import { readVersions, SEMVER } from './version-files.mjs';

let failed = false;

const versions = readVersions();

// Правило 33: спершу довести, що вхід непорожній. Порожній список тут
// означав би зелену перевірку, яка не перевірила нічого.
if (versions.length === 0) {
  console.error('::error title=check-version::список носіїв версії порожній');
  process.exit(1);
}

for (const { label, version } of versions) {
  console.log(`  ${label.padEnd(28)} ${version}`);
  if (!SEMVER.test(version)) {
    console.error(`::error title=check-version::${label}: «${version}» не є major.minor.patch`);
    failed = true;
  }
}

const unique = [...new Set(versions.map((entry) => entry.version))];
if (unique.length > 1) {
  const detail = versions.map((entry) => `${entry.label}=${entry.version}`).join(', ');
  console.error(`::error title=check-version::версії розійшлися: ${detail}`);
  failed = true;
}

const ref = process.env.GITHUB_REF_NAME || '';
if (ref.startsWith('v')) {
  const tagVersion = ref.slice(1);
  console.log(`  ${'тег GITHUB_REF_NAME'.padEnd(28)} ${ref}`);
  if (!SEMVER.test(tagVersion)) {
    console.error(`::error title=check-version::тег «${ref}» не має форми v<major.minor.patch>`);
    failed = true;
  } else if (unique.length === 1 && unique[0] !== tagVersion) {
    console.error(
      `::error title=check-version::тег ${ref} не збігається з версією проєкту ${unique[0]}. ` +
        `Виправлення: node scripts/sync-version.mjs ${tagVersion}, закомітити, і тег ставити заново ` +
        '(поки з нього не створено реліз — правило 13).',
    );
    failed = true;
  }
} else {
  console.log(`  ${'тег GITHUB_REF_NAME'.padEnd(28)} — (не тег: ${ref || 'порожньо'})`);
}

if (failed) {
  process.exit(1);
}

console.log(`\nВерсія узгоджена: ${unique[0]}`);
