# Candidate grid — інваріанти (НЕ ЛАМАТИ)

Віртуалізована сітка плиток (`VirtualCandidateGrid`) уже кілька разів
ламалась «дрібними» правками заміру. Усі правила нижче **закриті тестами**
(`geometry.selftest.ts`) і **статичним чеком** (`scripts/check-grid-invariants.mjs`).

## Контекст

- Усі 9 `CategoryScreen` змонтовані постійно (T-099) і ховаються через
  `display: none` (`AppLayout.screenClass`).
- На `display: none` → `clientWidth` / `clientHeight` = **0**.
- CSS `grid-template-columns: repeat(N, minmax(0, 1fr))` розтягує колонки
  на **реальну** ширину контейнера, навіть якщо JS думає, що ширина = 1px.

## MUST (обовʼязково)

1. **`active` prop** — CategoryScreen передає, чи екран видимий. Замір і
   малювання плиток — лише коли `active === true`.
2. **Нульові заміри ігнорувати** — `applyViewportMeasure` не записує 0/крихітні
   розміри поверх валідної геометрії (див. `MIN_MEASURE_PX`).
3. **Стартова геометрія multi-column** — `DEFAULT_VIEWPORT_WIDTH` ≥ 3×
   `MIN_TILE_WIDTH.standard`, ніколи `width: 1`.
4. **Плитки без «measured»-гейта** — якщо `active` і є кандидати, DOM плиток
   рендериться (дефолтна ширина вже дає кілька колонок; ResizeObserver
   підправить).
5. **Геометрія лише в `geometry.ts`** — колонки/rowHeight/overscan не
   дублювати в JSX «на око».

## FORBIDDEN (заборонено)

| Анти-патерн | Симптом |
|-------------|---------|
| `useState({ width: 1, height: 1 })` | 1 колонка → плитка-гігант на всю ширину |
| `Math.max(1, clientWidth)` при 0 | те саме: 0 → 1 |
| `measured && visible.map(...)` | порожній екран, коли замір 0 на mount |
| `useLayoutEffect(..., [])` без `active` | замір раз на прихованому mount → назавжди 0 |
| Перезаписувати валідний viewport нулями з `display:none` | стрибки / гіганти при навігації |
| Рахувати колонки в іншому файлі «для зручності» | розсинхрон віртуалізації й CSS |

## Як перевірити

```bash
npm --prefix ui run test:grid
# або з кореня:
node scripts/check-grid-invariants.mjs
```

Обидва кроки варто ганяти після будь-якої правки сітки/CategoryScreen.

## Куди класти зміни

| Що міняєш | Файл |
|-----------|------|
| Мін. ширина плитки / density | `geometry.ts` (`MIN_TILE_WIDTH`) |
| Логіка колонок / overscan | `geometry.ts` (`calculateVirtualGridWindow`) |
| Політика 0-замірів | `geometry.ts` (`applyViewportMeasure`) |
| ResizeObserver / active | `useGridViewport.ts` |
| Рендер плиток / props | `../VirtualCandidateGrid.tsx` |
| Видимість екрана | `CategoryScreen` → `active={isActive}` |
