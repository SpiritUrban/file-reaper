#!/usr/bin/env node
/**
 * Статичний сторож сітки плиток.
 *
 * Ловить анти-патерни, через які VirtualCandidateGrid уже ламали кілька разів
 * (width:1 → гігант; measured && → порожньо; замір без active).
 *
 * Запуск: node scripts/check-grid-invariants.mjs
 * Також: npm --prefix ui run test:grid
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const gridTsx = path.join(root, "ui/src/components/VirtualCandidateGrid.tsx");
const geometryTs = path.join(root, "ui/src/components/candidate-grid/geometry.ts");
const categoryTsx = path.join(
  root,
  "ui/src/features/category/CategoryScreen.tsx",
);

let failed = 0;

function fail(msg) {
  failed += 1;
  console.error(`FAIL: ${msg}`);
}

function ok(msg) {
  console.log(`ok  — ${msg}`);
}

function read(p) {
  if (!fs.existsSync(p)) {
    fail(`missing file: ${path.relative(root, p)}`);
    return "";
  }
  return fs.readFileSync(p, "utf8");
}

const grid = read(gridTsx);
const geometry = read(geometryTs);
const category = read(categoryTsx);

// --- VirtualCandidateGrid.tsx ---
if (grid) {
  if (/\bwidth:\s*1\b/.test(grid) || /\bheight:\s*1\b/.test(grid)) {
    fail(
      "VirtualCandidateGrid: FORBIDDEN initial width/height 1 (плитка-гігант). Use defaultViewport() from candidate-grid/geometry.",
    );
  } else {
    ok("no width:1 / height:1 in VirtualCandidateGrid");
  }

  if (/Math\.max\(\s*1\s*,\s*.*clientWidth/.test(grid)) {
    fail(
      "VirtualCandidateGrid: FORBIDDEN Math.max(1, clientWidth) — turns display:none 0 into width 1",
    );
  } else {
    ok("no Math.max(1, clientWidth) coerce");
  }

  if (/measured\s*&&/.test(grid)) {
    fail(
      "VirtualCandidateGrid: FORBIDDEN `measured &&` tile gate — causes empty grid when measure is 0 on mount",
    );
  } else {
    ok("no measured&& tile render gate");
  }

  if (!/useGridViewport/.test(grid)) {
    fail("VirtualCandidateGrid must use useGridViewport (not ad-hoc ResizeObserver)");
  } else {
    ok("uses useGridViewport");
  }

  if (!/shouldRenderTiles/.test(grid)) {
    fail("VirtualCandidateGrid must use shouldRenderTiles from geometry");
  } else {
    ok("uses shouldRenderTiles");
  }

  if (!/\bactive\b/.test(grid)) {
    fail("VirtualCandidateGrid must accept active prop");
  } else {
    ok("has active prop");
  }

  // Геометрія не має бути заново імплементована в JSX-файлі
  if (/function calculateVirtualGridWindow/.test(grid)) {
    fail(
      "calculateVirtualGridWindow must live only in candidate-grid/geometry.ts",
    );
  } else {
    ok("geometry not duplicated in VirtualCandidateGrid");
  }
}

// --- geometry.ts ---
if (geometry) {
  if (!/DEFAULT_VIEWPORT_WIDTH\s*=\s*960/.test(geometry)) {
    // allow change only if still multi-column — soft check min
    const m = geometry.match(/DEFAULT_VIEWPORT_WIDTH\s*=\s*(\d+)/);
    const w = m ? Number(m[1]) : 0;
    if (w < 400) {
      fail(`DEFAULT_VIEWPORT_WIDTH too small (${w}); need multi-column default`);
    } else {
      ok(`DEFAULT_VIEWPORT_WIDTH=${w}`);
    }
  } else {
    ok("DEFAULT_VIEWPORT_WIDTH present");
  }

  if (!/export function applyViewportMeasure/.test(geometry)) {
    fail("geometry.ts must export applyViewportMeasure");
  } else {
    ok("applyViewportMeasure exported");
  }

  if (!/export function shouldRenderTiles/.test(geometry)) {
    fail("geometry.ts must export shouldRenderTiles");
  } else {
    ok("shouldRenderTiles exported");
  }
}

// --- CategoryScreen must wire active ---
if (category) {
  if (!/active=\{isActive\}/.test(category) && !/active=\{[^}]*isActive/.test(category)) {
    fail("CategoryScreen must pass active={isActive} to VirtualCandidateGrid");
  } else {
    ok("CategoryScreen passes active={isActive}");
  }
}

// --- pure selftest ---
const selftest = path.join(
  root,
  "ui/src/components/candidate-grid/geometry.selftest.ts",
);
if (!fs.existsSync(selftest)) {
  fail("missing geometry.selftest.ts");
} else {
  // Node 22+ strip-types; fallback tsx if present in ui/node_modules
  const nodeMajor = Number(process.versions.node.split(".")[0]);
  let result;
  if (nodeMajor >= 22) {
    result = spawnSync(
      process.execPath,
      ["--experimental-strip-types", selftest],
      { encoding: "utf8", cwd: root },
    );
  } else {
    const tsx = path.join(root, "ui/node_modules/tsx/dist/cli.mjs");
    if (fs.existsSync(tsx)) {
      result = spawnSync(process.execPath, [tsx, selftest], {
        encoding: "utf8",
        cwd: root,
      });
    } else {
      // Last resort: run via npx tsx (network may be needed first time)
      result = spawnSync("npx", ["--yes", "tsx", selftest], {
        encoding: "utf8",
        cwd: root,
        shell: true,
      });
    }
  }

  if (result.status !== 0) {
    fail("geometry.selftest failed");
    if (result.stdout) console.error(result.stdout);
    if (result.stderr) console.error(result.stderr);
  } else {
    ok("geometry.selftest passed");
    if (result.stdout) {
      for (const line of result.stdout.trim().split("\n")) {
        console.log(`     ${line}`);
      }
    }
  }
}

if (failed > 0) {
  console.error(`\n${failed} grid guard(s) failed — see ui/src/components/candidate-grid/README.md`);
  process.exit(1);
}
console.log("\ngrid invariants OK");
