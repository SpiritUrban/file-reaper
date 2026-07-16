# TrashRadar — WebView2 Baseline & Web Capability Inventory

> Роль документа: зафіксувати **мінімально підтримувану версію WebView2** і
> **повний перелік web-можливостей**, що їх використовує UI (T-158, DoD:
> «ключові екрани працюють; список використаних web-можливостей зафіксовано»).
> Пом'якшує ризик «дрейфу версій WebView2» з architecture.md §11.2.

## Мінімальна версія

**WebView2 Runtime 111 (Chromium 111, ~березень 2023).**

Флор диктує **не наш код, а Tailwind CSS v4**: генерований CSS покладається на
`color-mix()`, чия офіційно підтримувана база — Chromium 111 / Safari 16.4 /
Firefox 128. Наш власний JS/DOM/CSS-код має нижчий флор (найвище — `.at()`,
Chromium 92), тож саме Tailwind визначає мінімум.

Ціль збірки закріплено явно: `build.target: "chrome111"` у `ui/vite.config.ts`.
Vite/esbuild не емітить JS-синтаксис, новіший за Chromium 111 — якщо хтось
внесе таку фічу, збірка впаде. Це смоук-гейт для JS-шару.

### Чому саме WebView2, а не Windows

WebView2 Runtime — **evergreen** і оновлюється системою незалежно від версії
Windows (architecture.md §11.1). На Win10/11 з увімкненим авто-оновленням
рантайм завжди на кілька версій вище за 111. Ризик нижчої версії реальний лише
для машин із вимкненим оновленням WebView2 або фіксованою (fixed-version)
дистрибуцією рантайму. Продукт таргетує Windows 10/11 x64 (product.md §6).

## Інвентар web-можливостей

Джерело — аудит `ui/src` (grep по API) і перевірка **фактично згенерованого**
бандла `dist/assets/*.css` та `*.js`. Колонка «мін. Chromium» — версія, з якої
можливість доступна; «деградація» — що стається на старішій за флор версії.

### CSS (через Tailwind v4 + `theme.css`)

| Можливість | Мін. Chromium | У бандлі | Деградація нижче версії |
|---|---|---|---|
| Cascade layers `@layer` | 99 | так (структурно, без guard) | **порядок каскаду ламається** — головна причина не опускатися нижче 99 |
| `color-mix()` | 111 | так (43×, **обгорнуто `@supports`**) | ефект пропускається → кольори-тінти (hover, opacity-модифікатори) відсутні, решта рендериться |
| Registered custom props `@property` | 85 | так (Tailwind `--tw-*`) | at-rule ігнорується, кастом-проперті працюють як незареєстровані |
| `:where()` / `:is()` | 88 | так | без них селектори не матчаться коректно |
| CSS custom properties (`var()`) | 49 | так (усі токени палітри) | — |
| Flexbox / CSS Grid | 57 / 57 | так (розкладка, віртуальна сітка) | — |
| `aspect-ratio` (Tailwind `aspect-*`) | 88 | так (плитки 4:3) | — |
| `prefers-reduced-motion` / `prefers-color-scheme` | 74 / 76 | так (`useAnimatedNumber`, тема) | анімації не приглушуються |

### JavaScript (ES) — транспіляція під `chrome111`

| Можливість | Мін. Chromium | Використання |
|---|---|---|
| `Array.prototype.at()` | 92 | 6 місць (індексація з кінця) — **найвищий JS-флор нашого коду** |
| `Array.prototype.flatMap()` | 69 | 2 місця |
| ES modules / dynamic `import()` | 63 / 63 | Vite-бандл (статичний; динамічного import у продакшн-коді немає) |
| ES2022 (top-level синтаксис, `#private`, `.at`) | 94 | ціль tsconfig `ES2022` — весь у межах 111 |

Немає (свідомо): `structuredClone`, top-level `await`, `Array.fromAsync`,
декоратори — щоб не піднімати флор без потреби.

### DOM / Web API

| API | Мін. Chromium | Використання |
|---|---|---|
| `ResizeObserver` | 64 | `VirtualCandidateGrid` — адаптивні колонки (T-101) |
| `requestAnimationFrame` / `cancelAnimationFrame` | 24 | `useAnimatedNumber` (T-102), коалесинг скролу сітки (T-101) |
| `useSyncExternalStore` (React 18) | — (полі-філ у React) | усі стори (`selection`, `keep`, `livePreview`, …) |
| `window.matchMedia` | 9 | `prefers-reduced-motion` |
| `localStorage` | 4 | геометрія Live Preview (T-139), густина сітки, хоткеї (T-153) |
| `Intl` / `toLocaleDateString` | усі | дати карантину/деталей (uk-UA) |
| `performance.now` | 24 | заміри в dev-перевірках |
| `window.addEventListener('keydown')` (capture) | усі | реєстр хоткеїв (T-103) |
| Custom `CustomEvent` (`trashradar:*`) | 15 | шина хоткеїв, focus-category |
| Tauri IPC (`@tauri-apps/api` `invoke`/`listen`) | — (місток рантайму) | єдина межа UI→Core (T-097) |

**Не використовуються** (щоб не піднімати флор і за принципом «консервативні
web-можливості», architecture.md §11.2): WebGL/Canvas 2D для рендера UI,
Web Workers, WebAssembly у UI-шарі, Service Worker, IndexedDB, WebRTC,
File System Access API, `<dialog>`, container queries `@container`,
`:has()`-селектор, View Transitions. Превью декодуються в Core і приходять
як `data:`-URL у `<img>` — жодного клієнтського декодування медіа.

## Смоук-перевірка ключових екранів

DoD «ключові екрани працюють» перевіряється на трьох рівнях:

1. **Гейт збірки:** `npm run build` з `build.target: "chrome111"` — падає, якщо
   в код потрапляє JS-синтаксис, новіший за Chromium 111.
2. **Інвентар:** цей документ звірено з фактичним бандлом (`color-mix()`,
   `@layer`, `@property` реально присутні в `dist/*.css`; JS обмежено 111).
3. **Функціональний прогін** усіх ключових екранів у WebView-рушії (браузер
   pane, Chromium): Cleanup Summary, екран категорії з віртуальною сіткою
   (T-101), Live Preview (T-140), Quarantine, Settings — рендеряться без
   помилок консолі. Прогін на модерному Chromium доводить функціональність;
   інвентар + guard-и `@supports` доводять, що на Chromium 111 екрани
   рендеряться (гірше з color-mix-тінтами, але без поломки розкладки).

Живий прогін на реальному WebView2 111 (fixed-version рантайм) — ручна
передрелізна перевірка беты (M6), не автоматизується в цій задачі: CI-раннери
несуть evergreen-рантайм, а не історичні версії.

## Політика підтримки

- Підняття мінімуму (напр. нова фіча Tailwind/CSS з вищим флором) — **свідоме
  рішення**: оновити `build.target`, цей документ і рядок мінімуму вище.
- Нову web-можливість, вищу за поточний флор (111), додавати лише з оновленням
  мінімуму або за `@supports`/feature-detect із деградацією.
- Перед релізом бети — димовий прогін ключових екранів на fixed-version
  WebView2 мінімальної версії.
