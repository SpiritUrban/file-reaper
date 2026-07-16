/**
 * Самотест інваріантів сітки (без vitest/jest).
 * Запуск: `npm --prefix ui run test:grid`
 *
 * Падіння = exit 1. Ці кейси ловлять регресії, через які вже ходили колами:
 * width=1 → гігант; measured-гейт → порожньо; 0-замір затирає валідний.
 */

import {
  applyViewportMeasure,
  calculateVirtualGridWindow,
  DEFAULT_VIEWPORT_HEIGHT,
  DEFAULT_VIEWPORT_WIDTH,
  defaultColumnCount,
  defaultViewport,
  isUsableMeasure,
  MIN_MEASURE_PX,
  MIN_TILE_WIDTH,
  shouldRenderTiles,
} from "./geometry";

let failed = 0;

function assert(cond: boolean, message: string): void {
  if (!cond) {
    failed += 1;
    console.error(`FAIL: ${message}`);
  } else {
    console.log(`ok  — ${message}`);
  }
}

// --- дефолт ніколи не 1×1 ---
assert(DEFAULT_VIEWPORT_WIDTH >= 3 * MIN_TILE_WIDTH.standard, "DEFAULT_WIDTH ≥ 3× standard tile");
assert(DEFAULT_VIEWPORT_HEIGHT >= MIN_MEASURE_PX, "DEFAULT_HEIGHT usable");
assert(defaultViewport().width === DEFAULT_VIEWPORT_WIDTH, "defaultViewport width");
assert(defaultViewport().width !== 1, "FORBIDDEN: default width === 1");
assert(defaultViewport().height !== 1, "FORBIDDEN: default height === 1");

// --- multi-column на дефолті (не гігант) ---
const cols = defaultColumnCount("standard");
assert(cols >= 3, `default standard columns ≥ 3 (got ${cols})`);
assert(defaultColumnCount("compact") >= cols, "compact ≥ standard columns");

// --- LP-ліва зона ~45% від 1280 ≈ 576px: ≥3 колонки standard ---
const lpLeft = calculateVirtualGridWindow(50, 560, 700, 0, "standard");
assert(lpLeft.columns >= 3, `LP left 560px → ≥3 cols (got ${lpLeft.columns})`);

// --- 0 / крихітний замір reject ---
assert(!isUsableMeasure(0, 0), "0×0 not usable");
assert(!isUsableMeasure(1, 1), "1×1 not usable (giant trap)");
assert(!isUsableMeasure(MIN_MEASURE_PX - 1, 400), "width < MIN reject");
assert(isUsableMeasure(MIN_MEASURE_PX, MIN_MEASURE_PX), "MIN×MIN ok");

// --- applyViewportMeasure: не затирати валідне нулями ---
const good = { width: 800, height: 600, scrollTop: 12 };
assert(
  applyViewportMeasure(good, 0, 0) === null,
  "display:none measure → null (keep previous)",
);
assert(
  applyViewportMeasure(good, 1, 1) === null,
  "1×1 measure → null",
);
const grown = applyViewportMeasure(good, 900 + 8, 700);
assert(grown !== null && grown.width === 900 && grown.height === 700, "valid measure applies");
assert(grown !== null && grown.scrollTop === 12, "scrollTop preserved on resize");

// --- shouldRenderTiles: active, без measured-гейта ---
assert(shouldRenderTiles(true, 10) === true, "active + items → render");
assert(shouldRenderTiles(true, 0) === false, "active + empty → no tiles (EmptyState)");
assert(shouldRenderTiles(false, 10) === false, "inactive → no tiles");
// Регрес: «чекати measured» тут не існує — active достатньо.
assert(
  shouldRenderTiles(true, 1) === true,
  "FORBIDDEN regression: must render without separate measured flag",
);

// --- віртуалізація: DOM обмежений при 100k ---
const huge = calculateVirtualGridWindow(100_000, 1200, 800, 0, "standard");
const visibleCount = huge.endIndex - huge.startIndex;
assert(visibleCount <= 200, `100k items → ≤200 DOM tiles (got ${visibleCount})`);
assert(huge.columns >= 4, "1200px standard multi-col");

// --- width=1 geometry never used as input path for "good" defaults ---
const trap = calculateVirtualGridWindow(20, 1, 1, 0, "standard");
// safeWidth clamps to MIN_MEASURE_PX → still not "full screen one tile" in JS;
// CSS 1fr is the real giant — prevented by never feeding width=1 from state.
assert(trap.columns >= 1, "clamp still defined");
assert(
  DEFAULT_VIEWPORT_WIDTH > MIN_TILE_WIDTH.standard * 2,
  "default cannot collapse to single-tile intent",
);

if (failed > 0) {
  console.error(`\n${failed} grid invariant(s) failed`);
  process.exit(1);
}
console.log("\nall grid geometry invariants passed");
