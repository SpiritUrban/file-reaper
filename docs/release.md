# TrashRadar — Release & Installer

> Роль документа: як збирається інсталятор, як публікується реліз під усі
> платформи, як оновлюється сайт-вітрина, і як увімкнути підпис коду (T-159).
> Розташування даних профілю і портативність — окремо, T-160.

## Конвеєр релізу (Стадія 2)

| Воркфлоу | Тригер | Що робить |
|---|---|---|
| `ci.yml` | пуш у `main`, PR | UI, Core на Windows **і на Linux**, shell, e2e, бенчі |
| `installer.yml` | пуш у `main`, вручну | NSIS-інсталятор артефактом рану (не реліз) |
| `release.yml` | тег `v*.*.*`, вручну | звірка версій → збірка 4 платформ → GitHub Release + `latest.json` → деплой сайту |
| `pages.yml` | пуш у `main` (site/), виклик з релізу | маніфест завантажень → Pages → **перевірка живого URL** |

### Як випустити версію

```sh
node scripts/sync-version.mjs 0.2.0   # усі package.json, tauri.conf.json, Cargo.toml, Cargo.lock
node scripts/check-version.mjs        # той самий скрипт, що в джобі validate-version
git commit -am "release: 0.2.0" && git push
# ДОЧЕКАТИСЯ зеленого CI (особливо джоби «Core on Linux»), і лише тоді:
git tag v0.2.0 && git push origin v0.2.0
```

**Пробний прогін без публікації:** `Actions → Release → Run workflow`. Збере всі
чотири платформи й нічого не опублікує (`tagName` порожній поза тегом). Ловить
~90% проблем за один ран. **Чого він НЕ перевіряє:** джоба `deploy-site` у ньому
пропускається, тож деплой сайту вперше виконується вже на тезі.

**Опублікований тег не переставляти** — дозволено лише поки з нього не створився
реліз. Якщо деплой сайту відхилено правилом оточення, реліз уже справний:
виправте правило й зробіть `Re-run failed jobs` (перезапуститься лише
`deploy-site`, збірки не повторяться).

### ⚠️ Ручні кроки на GitHub — їх мусить зробити власник репозиторію

Поки не зроблено, пайплайн падає незрозуміло. У кожного кроку є **сигнал**, що
він виконаний правильно.

| # | Що | Де | Сигнал, що вийшло |
|---|---|---|---|
| 1 | Активний платіжний метод, ненульовий spending limit | Settings → Billing and plans | джоби стартують; інакше падіння за **3 с із порожнім списком кроків** (0 steps) |
| 2 | Pages: Source = **GitHub Actions** | Settings → Pages | сторінка віддає сайт, а не 404 |
| 3 | Секрет `TAURI_SIGNING_PRIVATE_KEY` = **весь вміст `.tauri-key`** | Settings → Secrets → Actions, **Repository secrets** | крок «Normalize and verify signing secret» друкує «Ключ підпису прийнято» |
| 4 | Секрет `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — **не створювати** | — | ключ згенеровано без пароля; відсутній секрет приходить порожнім рядком, а це саме те, що треба |
| 5 | Права воркфлоу на запис | Settings → Actions → General | реліз створюється |
| 6 | **Після першого деплою Pages:** оточення `github-pages` дозволяє гілку `main` **і тег** `v*.*.*` | Settings → Environments → github-pages | заголовок читається **«1 branch and 1 tag allowed»** |

> **Пастка кроку 6.** У діалозі `Add deployment branch or tag rule` перемикач
> **Ref type** за замовчуванням стоїть на **Branch**. Патерн `v*.*.*`, доданий як
> branch-правило, шукає *гілку* з такою назвою і тегів не бачить — а дізнаєтесь
> ви про це вже після пушу тега. Якщо біля `v*.*.*` написано «Currently applies
> to 0 branches», а в заголовку «0 tags allowed» — видаліть і додайте заново з
> `Ref type: Tag`.

> **Порядок обов'язковий:** оточення `github-pages` **не існує**, поки Pages не
> задеплоїлись хоча б раз. Тобто: 1 → 2 → 3 → 4 → 5 → пуш у `main` → перший
> деплой Pages → 6 → тег.

> ### Червоний ран «pages build and deployment» — це не наш воркфлоу
>
> Якщо в Actions висить ран з назвою **`pages build and deployment`**
> (`on: dynamic`, джоби `build / report-build-status / deploy`, у логах Jekyll і
> `jekyll-theme-primer`) — це **вбудований збирач GitHub**, а не `pages.yml`.
> Він запускається лише поки Pages Source = «Deploy from a branch»: GitHub
> намагається зібрати корінь репозиторію як Jekyll-сайт, і для проєкту, який
> Jekyll-сайтом не є, це падіння закономірне.
>
> Лікується кроком 2 цієї таблиці: Source = **GitHub Actions**. Після цього
> легасі-воркфлоу зникає сам, і деплоїть лише наш `pages.yml` (у нього одна
> джоба `deploy` і жодного Jekyll).

### Як читати падіння

Спершу дивіться на **список кроків**, а не на текст помилки:

- **0 кроків, ~3 с** → джоба не стартувала: білінг (крок 1 вище). Текст причини
  видно в анотаціях, без токена:
  `curl -s https://api.github.com/repos/SpiritUrban/file-reaper/check-runs/<JOB_ID>/annotations`
- червоні `Set up job` / `Post Run …` / `Complete job` → впав раннер, не збірка;
- 1–3 с — конфігурація; 20–40 с — встановлення пакетів; хвилини — справді код.

Логи ранів закриті без авторизації навіть у публічному репозиторії
(«Sign in to view logs»), **анотації — ні**. Тому кожен крок, що може впасти,
проводить вивід через `scripts/ci-annotate.sh`.

### Ключі підпису оновлень

Згенеровані локально в `.tauri-key` / `.tauri-key.pub` (у `.gitignore`), без
пароля. Публічний ключ уже в `tauri.conf.json` → `plugins.updater.pubkey`.

> **Втрата приватного ключа незворотна.** Без нього підписати оновлення
> неможливо, а зміна ключа означає, що всі встановлені копії перестануть
> приймати оновлення. Тримайте копію `.tauri-key` поза репозиторієм.

### Перевірка після релізу

```sh
# ендпоінт апдейтера мусить існувати
curl -s -L "https://github.com/SpiritUrban/file-reaper/releases/latest/download/latest.json"

# кожне посилання завантаження мусить дати 206
curl -s -o /dev/null -w '%{http_code}\n' -L -r 0-0 \
  "https://github.com/SpiritUrban/file-reaper/releases/download/v0.2.0/<файл>"

# версія на сайті
curl -s "https://spiriturban.github.io/file-reaper/download-manifest.json"
```

Реліз вважати завершеним, лише коли `latest.json` містить усі очікувані
платформні ключі: кожна джоба матриці спершу вивантажує інсталятори і **аж
потім** дописує свої записи в маніфест, тож проміжний стан виглядає як повний
реліз, а частина клієнтів оновлення ще не бачить.

## Інсталятор

**Формат:** NSIS (`bundle.targets: ["nsis"]`), один `TrashRadar_<version>_x64-setup.exe`.

**Режим встановлення:** **per-user** (`bundle.windows.nsis.installMode: "currentUser"`).
Застосунок ставиться у профіль користувача (`%LOCALAPPDATA%\<productName>`),
метадані видалення — під `HKCU`. Інсталяція **не потребує адмін-прав / UAC**.
Це свідомий вибір: сам застосунок піднімає права окремо і лише для MFT-скану
(T-034), тож інсталяції elevation не потрібен — менше тертя й менше приводів
для UAC/SmartScreen на кроці встановлення.

**Метадані** (`bundle` у `core/shell/tauri.conf.json`): `publisher`,
`copyright`, `category`, `shortDescription`/`longDescription`, `identifier`
(`app.trashradar.desktop`) — показуються в «Програми та засоби» й у
властивостях exe. Мову інсталятора взято з ОС (українська → англійська як
фолбек; `displayLanguageSelector: false`).

**Мінімальна версія WebView2:** `bundle.windows.minimumWebview2Version:
"111.0.0.0"` — інсталятор перевіряє рантайм і за потреби тягне/оновлює
WebView2 до 111+. Це enforce-версія баз­лайну з
[webview2-baseline.md](webview2-baseline.md) (T-158) на етапі встановлення.

### Збірка

```sh
# з core/shell (beforeBuildCommand збере фронтенд сам)
../../ui/node_modules/.bin/tauri.cmd build
```

Артефакт: `core/target/release/bundle/nsis/TrashRadar_<version>_x64-setup.exe`.

CI: джоб `Installer (NSIS)` у `.github/workflows/installer.yml` збирає той самий
артефакт на push у `main` і за `workflow_dispatch` (не в PR-checks, щоб не
подвоювати найдовший джоб на кожен пуш гілки).

### Чисте встановлення / видалення (DoD T-159)

- **Встановлення:** per-user, без UAC; ярлик у Start Menu; реєстрація під
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall`.
- **Видалення:** згенерований NSIS-uninstaller прибирає встановлені файли,
  ярлик і uninstall-реєстрацію. **Дані профілю** (`%LOCALAPPDATA%\TrashRadar` —
  конфіг, БД індексу, кеші, логи) навмисно НЕ видаляються деінсталятором:
  повторне встановлення зберігає налаштування й «keep»-рішення. Політика
  «видалення профілю = чистий старт» і повний перелік того, що живе в
  профілі, — T-160.

## Підпис коду (відкладено)

Статус: **не увімкнено** (свідоме рішення — див. відхилення в progress.md).
Без Authenticode-підпису SmartScreen показує попередження «Windows protected
your PC» для завантажених exe, доки збірка не набере репутації. DoD-частина
«SmartScreen не блокує підписану збірку» лишається відкритою до появи
сертифіката.

Коли зʼявиться сертифікат, підпис вмикається **без змін коду** — лише
конфігурація. Два підтримувані шляхи Tauri (`bundle.windows`):

1. **Сертифікат за відбитком** (cert уже в сховищі Windows на раннері):
   ```json
   "windows": {
     "certificateThumbprint": "<SHA1 thumbprint>",
     "digestAlgorithm": "sha256",
     "timestampUrl": "http://timestamp.digicert.com"
   }
   ```
2. **Власна команда підпису** (`signCommand`) — для HSM/хмарних підписувачів
   (напр. Azure Trusted Signing, `signtool` з токеном): Tauri викликає її для
   кожного бінарника.

Секрети (thumbprint / .pfx base64 + пароль, або креденшали хмарного
підписувача) додаються в **GitHub Actions secrets** і підставляються в
`installer.yml` перед `tauri build`. Рекомендація: тимчасова стеля — набрати
репутацію швидше дає **EV-сертифікат** (миттєвий SmartScreen-траст) проти
OV (репутація накопичується за завантаженнями/часом).

Timestamp обовʼязковий: без нього підпис «протухає» з експірацією сертифіката;
з ним — лишається валідним і після.
