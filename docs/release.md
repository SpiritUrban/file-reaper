# TrashRadar — Release & Installer

> Роль документа: як збирається інсталятор, як він встановлює/видаляє
> застосунок, і як увімкнути підпис коду (T-159). Розташування даних профілю
> і портативність — окремо, T-160; бета-канал і оновлення — T-163.

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
