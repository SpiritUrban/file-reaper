# Assets

`app-icon.svg` — **єдине джерело** всього набору іконок застосунку
(радарний екран: кільця дальності, промінь розгортки, засвітки за heat-шкалою
з `ui/src/design/theme.css`).

Набір у `core/shell/icons/` генерується з нього і поштучно не правиться:

```sh
cd core/shell
../../ui/node_modules/.bin/tauri icon ../../assets/app-icon.svg -o icons
rm -rf icons/ios icons/android      # мобільні платформи проєкту не потрібні
```

Перевіряти іконки треба **вмістом, а не наявністю файлів**: збірка мовчки
приймає заглушку на 70 байт, і видно це лише на встановленому застосунку.

```sh
python -c "import struct;d=open('core/shell/icons/icon.ico','rb').read();print(len(d), struct.unpack('<H',d[4:6])[0],'зображень')"
# правильно: ~35 КБ і 6 зображень; icon.icns — заголовок icns і ~290 КБ;
# icon.png — 512×512
```

У SVG-джерелі **не можна писати подвійне тире в коментарях**: XML це
забороняє, і `tauri icon` падає з `ParsingFailed(InvalidComment)`.

Іконка інсталятора задається окремо — `bundle.windows.nsis.installerIcon`
у `core/shell/tauri.conf.json`. Без неї інсталятор має стандартний значок
NSIS, навіть коли сам застосунок уже з фірмовою іконкою. MSI власну іконку
отримати не може в принципі.

Локальна перезбірка може лишити стару іконку: `embed-resource` перекомпілює
ресурс лише коли змінюється **текст** `resource.rc`, а зміна вмісту
`icon.ico` не відстежується — і `cargo clean -p` цей артефакт не чіпає.
Лікується видаленням `core/target/release/build/trashradar-shell-*/out/resource.{rc,lib}`.
У CI те саме робить крок «Drop stale bundles from cache».
