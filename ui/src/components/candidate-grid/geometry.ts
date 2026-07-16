/**
 * Чиста геометрія віртуалізованої сітки плиток.
 *
 * ⚠️  Інваріанти: `README.md` у цій теці. Не дублювати формули в JSX.
 *     Тести: `geometry.selftest.ts`. Статичний чек: `scripts/check-grid-invariants.mjs`.
 */

export type GridDensity = "compact" | "standard" | "large";

/** Мін. ширина плитки → кількість колонок (щільна сітка в LP-лівій зоні). */
export const MIN_TILE_WIDTH: Readonly<Record<GridDensity, number>> = {
  compact: 104,
  standard: 140,
  large: 200,
};

export const GRID_GAP_PX = 4;
export const OVERSCAN_ROWS = 3;

/**
 * Стартова ширина до першого валідного заміру.
 * FORBIDDEN: 1 — давало `columns=1` + CSS 1fr = плитка-гігант.
 * Має давати ≥3 колонок на standard density.
 */
export const DEFAULT_VIEWPORT_WIDTH = 960;
export const DEFAULT_VIEWPORT_HEIGHT = 720;

/** Нижче цього clientWidth/Height вважаємо «не розкладено / display:none». */
export const MIN_MEASURE_PX = 64;

export interface VirtualGridWindow {
  columns: number;
  rowHeight: number;
  totalHeight: number;
  startIndex: number;
  endIndex: number;
  offsetY: number;
}

export interface ViewportSize {
  width: number;
  height: number;
  scrollTop: number;
}

/** Початковий viewport: multi-column, ніколи 1×1. */
export function defaultViewport(): ViewportSize {
  return {
    width: DEFAULT_VIEWPORT_WIDTH,
    height: DEFAULT_VIEWPORT_HEIGHT,
    scrollTop: 0,
  };
}

/**
 * Чи приймати сирий замір DOM.
 * 0 (display:none) і крихітні розміри — reject; не затирати валідну геометрію.
 */
export function isUsableMeasure(width: number, height: number): boolean {
  return width >= MIN_MEASURE_PX && height >= MIN_MEASURE_PX;
}

/**
 * Застосувати замір контейнера до попереднього viewport.
 * `clientWidth`/`clientHeight` з DOM; padding гріда (`GRID_GAP_PX * 2`)
 * віднімається з ширини (як у VirtualCandidateGrid p-1).
 *
 * Повертає `null`, якщо замір треба **ігнорувати** (прихований екран).
 */
export function applyViewportMeasure(
  previous: ViewportSize,
  clientWidth: number,
  clientHeight: number,
  horizontalPadding: number = GRID_GAP_PX * 2,
): ViewportSize | null {
  const width = clientWidth - horizontalPadding;
  const height = clientHeight;
  if (!isUsableMeasure(width, height)) return null;
  if (previous.width === width && previous.height === height) return previous;
  return { width, height, scrollTop: previous.scrollTop };
}

/**
 * Чи малювати DOM плиток.
 *
 * FORBIDDEN: гейт «тільки після measured» без active — на mount при
 * display:none measured=false назавжди → порожній екран.
 *
 * Правило: active + є кандидати → малюємо (дефолтна ширина вже multi-col).
 */
export function shouldRenderTiles(
  active: boolean,
  candidateCount: number,
): boolean {
  return active && candidateCount > 0;
}

/** Pure geometry for virtualization + column count. */
export function calculateVirtualGridWindow(
  itemCount: number,
  width: number,
  viewportHeight: number,
  scrollTop: number,
  density: GridDensity,
): VirtualGridWindow {
  // width уже має бути ≥ DEFAULT або валідний замір; clamp від дурості.
  const safeWidth = Math.max(MIN_MEASURE_PX, width);
  const minTile = MIN_TILE_WIDTH[density];
  const columns = Math.max(
    1,
    Math.floor((safeWidth + GRID_GAP_PX) / (minTile + GRID_GAP_PX)),
  );
  const tileWidth = (safeWidth - GRID_GAP_PX * (columns - 1)) / columns;
  const rowHeight = Math.max(1, tileWidth * 0.75 + GRID_GAP_PX);
  const rowCount = Math.ceil(itemCount / columns);
  const firstVisibleRow = Math.min(
    Math.max(0, rowCount - 1),
    Math.max(0, Math.floor(scrollTop / rowHeight)),
  );
  const visibleRows = Math.max(1, Math.ceil(viewportHeight / rowHeight));
  const startRow = Math.max(0, firstVisibleRow - OVERSCAN_ROWS);
  const endRow = Math.min(
    rowCount,
    firstVisibleRow + visibleRows + OVERSCAN_ROWS,
  );

  return {
    columns,
    rowHeight,
    totalHeight: Math.max(0, rowCount * rowHeight - GRID_GAP_PX),
    startIndex: startRow * columns,
    endIndex: Math.min(itemCount, endRow * columns),
    offsetY: startRow * rowHeight,
  };
}

/**
 * Інваріант для тестів: дефолтна ширина дає кілька колонок, не одного гіганта.
 * Повертає кількість колонок на standard density.
 */
export function defaultColumnCount(density: GridDensity = "standard"): number {
  return calculateVirtualGridWindow(
    100,
    DEFAULT_VIEWPORT_WIDTH,
    DEFAULT_VIEWPORT_HEIGHT,
    0,
    density,
  ).columns;
}
