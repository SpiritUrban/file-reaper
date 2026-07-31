//! Комплектація ffmpeg: пошук у бандлі й 1-клік завантаження з UI
//! (правило 29 брифу Стадії 2).
//!
//! Продукт не має вимагати від користувача ручних дій. «Скачайте FFmpeg і
//! пропишіть PATH» — бар'єр, через який відео-превʼю просто не працюють у
//! більшості встановлених копій, а користувач вважає, що зламана програма.
//!
//! Два механізми, саме в цьому порядку:
//!
//! 1. **Комплектація в CI** — `scripts/download-ffmpeg-resources.mjs` кладе
//!    бінарник у ресурси бандла, і він знаходиться сам;
//! 2. **1-клік з UI** — резерв для збірок, де ресурсу немає (macOS arm64:
//!    статичної збірки під цю платформу джерело не публікує) або де його
//!    видалив антивірус.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Індекс статичних збірок. Ті самі ключі платформ, що в CI-скрипті:
/// `windows-64`, `linux-64`, `linux-arm64`, `osx-64`. Ключа `macos-64`
/// не існує — перевірено запитом до API, а не з пам'яті.
const FFBINARIES_API: &str = "https://ffbinaries.com/api/v1/version/latest";

/// Стеля розміру завантаження: бінарник ffmpeg важить ~130 МБ, тож усе, що
/// суттєво більше, — не той файл (редирект на HTML-сторінку, підміна).
const MAX_DOWNLOAD_BYTES: usize = 400 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegStatus {
    /// Відео-превʼю працюють просто зараз.
    pub available: bool,
    /// Шлях до знайденого бінарника — для health-екрана.
    pub path: Option<String>,
    /// Чи є що качати для цієї платформи (інакше кнопку показувати нема сенсу).
    pub downloadable: bool,
}

/// Теки, де шукається ffmpeg, у порядку пріоритету (правило 29).
///
/// Розкладку встановленої копії знає лише оболонка, і на кожній платформі
/// вона інша, тому список будується тут, а не в крейті превʼю.
///
/// **Шляхи будуються лише через `PathBuf::push`/`join`** — жодного
/// хардкоду роздільника (правило 6a): рядок виду `resources\\ffmpeg.exe`
/// зробив би цю функцію робочою рівно на одній платформі.
pub fn ffmpeg_search_dirs(profile: Option<&Path>, exe_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 1. Завантажене користувачем (1-клік) — свіже за бандл, і саме його
    //    людина щойно свідомо поставила.
    if let Some(profile) = profile {
        dirs.push(download_dir(profile));
    }

    if let Some(exe_dir) = exe_dir {
        // 2. Ресурси бандла поруч із виконуваним файлом: Windows (NSIS) і
        //    Linux (deb/AppImage) кладуть їх саме так.
        dirs.push(exe_dir.join("resources"));

        // 3. macOS: виконуваний файл у Contents/MacOS, ресурси —
        //    у Contents/Resources.
        if let Some(contents) = exe_dir.parent() {
            let mut macos_resources = contents.join("Resources");
            dirs.push(macos_resources.clone());
            macos_resources.push("resources");
            dirs.push(macos_resources);
        }

        // 4. Сам застосунок: ffmpeg, покладений поруч руками.
        dirs.push(exe_dir.to_path_buf());
    }

    dirs
}

/// Тека, куди лягає завантажений 1-кліком бінарник.
pub fn download_dir(profile: &Path) -> PathBuf {
    profile.join("bin")
}

/// Ключ платформи в ffbinaries для поточної збірки.
///
/// macOS завжди `osx-64`: під arm64 статичної збірки джерело не публікує, а
/// x86_64-бінарник під Rosetta зазвичай працює. «Зазвичай» тут не здогадка —
/// після завантаження ми його запускаємо й перевіряємо (див. [`verify_runs`]).
fn platform_key() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("windows-64"),
        ("linux", "x86_64") => Some("linux-64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("macos", _) => Some("osx-64"),
        _ => None,
    }
}

pub fn status_for(video: &trashradar_preview::FfmpegVideoFrameSource) -> FfmpegStatus {
    let path = video.binary_path();
    FfmpegStatus {
        available: path.is_some(),
        path: path.map(|value| value.to_string_lossy().into_owned()),
        downloadable: platform_key().is_some(),
    }
}

/// Завантажити ffmpeg у профіль і одразу ним скористатися.
///
/// Повертає помилку текстом — її UI показує дослівно: користувач натиснув
/// кнопку і має право знати, що саме не вийшло.
pub async fn download(
    profile: Option<PathBuf>,
    video: trashradar_preview::FfmpegVideoFrameSource,
) -> Result<FfmpegStatus, String> {
    let profile = profile
        .ok_or_else(|| "Тека профілю недоступна — нема куди зберегти ffmpeg.".to_string())?;
    let key = platform_key()
        .ok_or_else(|| format!("Немає збірки ffmpeg для {}.", std::env::consts::OS))?;

    let client = reqwest::Client::builder()
        .user_agent(concat!("TrashRadar/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("HTTP-клієнт не створено: {error}"))?;

    let index: serde_json::Value = {
        let body = client
            .get(FFBINARIES_API)
            .send()
            .await
            .map_err(|error| format!("Індекс збірок недоступний: {error}"))?
            .text()
            .await
            .map_err(|error| format!("Індекс збірок не прочитано: {error}"))?;
        serde_json::from_str(&body)
            .map_err(|error| format!("Індекс збірок нечитабельний: {error}"))?
    };

    let url = index
        .get("bin")
        .and_then(|bin| bin.get(key))
        .and_then(|entry| entry.get("ffmpeg"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| format!("Індекс збірок не має ffmpeg для {key}."))?;

    let archive = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("Завантаження не почалося: {error}"))?
        .bytes()
        .await
        .map_err(|error| format!("Завантаження обірвалося: {error}"))?;

    if archive.len() > MAX_DOWNLOAD_BYTES {
        return Err(format!(
            "Архів {} МБ — це не збірка ffmpeg.",
            archive.len() / 1024 / 1024
        ));
    }

    let exe_name = trashradar_preview::ffmpeg_exe_name();
    let binary = extract(&archive, exe_name)?;

    let dir = download_dir(&profile);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("Тека {} не створена: {error}", dir.display()))?;
    let destination = dir.join(exe_name);
    std::fs::write(&destination, &binary)
        .map_err(|error| format!("Запис {} не вдався: {error}", destination.display()))?;
    make_executable(&destination)?;

    // Правило 31: перевіряти НАСЛІДОК, а не передумову. «Файл записано» — це
    // інвентаризація; єдине, що тут важить, — чи він справді запускається.
    // Саме тут ловиться macOS arm64 без Rosetta й обрізане завантаження.
    verify_runs(&destination)?;

    video.set_binary(destination.clone());
    tracing::info!(target: "preview", path = %destination.display(), "ffmpeg завантажено — відео-превʼю активні без перезапуску");

    Ok(status_for(&video))
}

fn extract(archive: &[u8], exe_name: &str) -> Result<Vec<u8>, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .map_err(|error| format!("Архів не відкрився: {error}"))?;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| format!("Запис архіву не прочитано: {error}"))?;
        // Ім'я порівнюється за останнім сегментом: у різних збірках бінарник
        // лежить то в корені, то в підтеці bin/.
        let is_wanted = entry
            .name()
            .rsplit(['/', '\\'])
            .next()
            .is_some_and(|name| name == exe_name);
        if !is_wanted {
            continue;
        }
        let mut buffer = Vec::new();
        entry
            .read_to_end(&mut buffer)
            .map_err(|error| format!("Розпакування не вдалося: {error}"))?;
        return Ok(buffer);
    }
    Err(format!("В архіві немає {exe_name}."))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .map_err(|error| format!("Атрибути {} не прочитано: {error}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)
        .map_err(|error| format!("Права на запуск не встановлені: {error}"))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    // На Windows біт виконання відсутній як поняття.
    Ok(())
}

fn verify_runs(path: &Path) -> Result<(), String> {
    let output = std::process::Command::new(path)
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| {
            format!("Завантажений ffmpeg не запускається: {error}. На Apple Silicon для цієї збірки потрібен Rosetta.")
        })?;
    if !output.status.success() {
        return Err(format!(
            "Завантажений ffmpeg завершився з кодом {}.",
            output.status
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Правило 6a: очікувані шляхи будуються `PathBuf::push`, а не рядком з
    /// хардкодженим роздільником. Інакше тест перевіряв би роздільник, а не
    /// логіку, і був би зеленим рівно на одній ОС.
    #[test]
    fn search_order_puts_downloaded_binary_first() {
        let mut profile = PathBuf::new();
        profile.push("profile");
        let mut exe_dir = PathBuf::new();
        exe_dir.push("apps");
        exe_dir.push("TrashRadar");

        let dirs = ffmpeg_search_dirs(Some(&profile), Some(&exe_dir));

        let mut expected_first = profile.clone();
        expected_first.push("bin");
        assert_eq!(dirs.first(), Some(&expected_first));

        let mut bundled = exe_dir.clone();
        bundled.push("resources");
        assert!(dirs.contains(&bundled));

        // macOS-розкладка: Contents/MacOS/exe → Contents/Resources.
        let mut macos = PathBuf::new();
        macos.push("apps");
        macos.push("Resources");
        assert!(dirs.contains(&macos));
    }

    #[test]
    fn search_dirs_survive_missing_inputs() {
        assert!(ffmpeg_search_dirs(None, None).is_empty());
        assert_eq!(ffmpeg_search_dirs(Some(Path::new("p")), None).len(), 1);
    }

    /// Перевірка мусить бути хоч раз побачена червоною (розділ 9): порожній
    /// архів не має «тихо» дати порожній бінарник.
    #[test]
    fn extract_reports_missing_binary() {
        let error = extract(b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0", "ffmpeg")
            .expect_err("порожній архів мусить бути помилкою");
        assert!(error.contains("ffmpeg"), "повідомлення: {error}");
    }
}
