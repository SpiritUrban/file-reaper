//! Адаптер `QuarantineFs` — єдиний шлюз деструктивних операцій.
//!
//! ІНВАРІАНТ D4 (docs/architecture.md §8): лише цей крейт має право
//! змінювати файлову систему. T-077 створює службовий каталог на тому;
//! наступні задачі додають manifest, reap, restore, purge і recovery.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use trashradar_app::ports::QuarantineFs;
use trashradar_domain::{
    error::CoreError,
    quarantine::{FileIdentity, QuarantineGuard},
};
use trashradar_platform_win::{read_file_identity, set_hidden};

pub const SERVICE_DIRECTORY_NAME: &str = ".trashradar";
pub const QUARANTINE_DIRECTORY_NAME: &str = "quarantine";

static PROBE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Підготовлений службовий каталог конкретного тому.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineDirectory {
    pub volume_root: PathBuf,
    pub service_root: PathBuf,
    pub quarantine_root: PathBuf,
}

/// Нативний адаптер — майбутній єдиний виконавець reap/restore/purge.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeQuarantineFs;

impl QuarantineFs for NativeQuarantineFs {}

impl NativeQuarantineFs {
    /// Остання лінія захисту перед будь-яким reap/move (T-085).
    pub fn validate_reap_path<I, S>(&self, path: &str, trashradar_roots: I) -> Result<(), CoreError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let normalized = path.trim().trim_start_matches(r"\\?\");
        let bytes = normalized.as_bytes();
        let volume = if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            bytes[0] as char
        } else {
            '?'
        };
        QuarantineGuard::windows_volume(volume)
            .with_trashradar_roots(trashradar_roots)
            .validate(path)
    }
    /// Звірити живий файл з size+mtime індексу безпосередньо перед move (T-086).
    pub fn validate_file_identity(
        &self,
        path: &Path,
        expected: FileIdentity,
    ) -> Result<(), CoreError> {
        let live = read_file_identity(path)?;
        expected.validate_unchanged(live, &path.to_string_lossy())
    }
    /// Створити `<том>\.trashradar\quarantine` і підтвердити право запису.
    pub fn ensure_on_volume(&self, volume: char) -> Result<QuarantineDirectory, CoreError> {
        if !volume.is_ascii_alphabetic() {
            return Err(CoreError::invalid_argument(format!(
                "Некоректна літера тому «{volume}»."
            )));
        }
        let root = PathBuf::from(format!("{}:\\", volume.to_ascii_uppercase()));
        self.ensure_at_root(&root)
    }

    /// Підготувати всі передані томи. Помилка лишає вже підготовлені томи валідними.
    pub fn ensure_on_volumes(
        &self,
        volumes: impl IntoIterator<Item = char>,
    ) -> Result<Vec<QuarantineDirectory>, CoreError> {
        volumes
            .into_iter()
            .map(|volume| self.ensure_on_volume(volume))
            .collect()
    }

    /// Варіант із явним коренем для портативного режиму та ізольованих тестів.
    pub fn ensure_at_root(&self, volume_root: &Path) -> Result<QuarantineDirectory, CoreError> {
        if !volume_root.is_dir() {
            return Err(CoreError::io(format!(
                "Корінь тому недоступний: {}.",
                volume_root.display()
            )));
        }

        let service_root = volume_root.join(SERVICE_DIRECTORY_NAME);
        let quarantine_root = service_root.join(QUARANTINE_DIRECTORY_NAME);
        fs::create_dir_all(&quarantine_root).map_err(|error| {
            CoreError::io(format!(
                "Не вдалося створити каталог карантину {}: {error}",
                quarantine_root.display()
            ))
        })?;
        set_hidden(&service_root)?;
        verify_writable(&quarantine_root)?;

        Ok(QuarantineDirectory {
            volume_root: volume_root.to_path_buf(),
            service_root,
            quarantine_root,
        })
    }
}

fn verify_writable(directory: &Path) -> Result<(), CoreError> {
    let sequence = PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let probe = directory.join(format!(".write-probe-{}-{sequence}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|error| {
            CoreError::io(format!(
                "Каталог карантину недоступний для запису {}: {error}",
                directory.display()
            ))
        })?;
    file.write_all(b"trashradar-write-probe")
        .and_then(|_| file.sync_all())
        .map_err(|error| CoreError::io(format!("Не вдалося перевірити запис: {error}")))?;
    drop(file);
    fs::remove_file(&probe)
        .map_err(|error| CoreError::io(format!("Не вдалося прибрати write-probe: {error}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use trashradar_platform_win::is_hidden;

    fn temp_volume(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("trashradar-t077-{name}-{nonce}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn creates_hidden_writable_quarantine_directory() {
        let root = temp_volume("create");
        let directory = NativeQuarantineFs.ensure_at_root(&root).unwrap();

        assert!(directory.quarantine_root.is_dir());
        #[cfg(windows)]
        assert!(is_hidden(&directory.service_root).unwrap());
        verify_writable(&directory.quarantine_root).unwrap();
        assert!(fs::read_dir(&directory.quarantine_root)
            .unwrap()
            .next()
            .is_none());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn provisioning_is_idempotent() {
        let root = temp_volume("idempotent");
        let first = NativeQuarantineFs.ensure_at_root(&root).unwrap();
        let second = NativeQuarantineFs.ensure_at_root(&root).unwrap();
        assert_eq!(first, second);
        #[cfg(windows)]
        assert!(is_hidden(&second.service_root).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn gateway_rejects_protected_path_before_move() {
        use trashradar_domain::error::ErrorCode;
        let error = NativeQuarantineFs
            .validate_reap_path(
                r"C:\Windows\System32\drivers\etc\hosts",
                [r"C:\Users\Ada\AppData\Local\TrashRadar"],
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PathProtected);
        assert!(NativeQuarantineFs
            .validate_reap_path(
                r"C:\Users\Ada\Videos\clip.mp4",
                [r"C:\Users\Ada\AppData\Local\TrashRadar"]
            )
            .is_ok());
    }
    #[test]
    fn changed_file_is_rejected_before_move() {
        use trashradar_domain::error::ErrorCode;

        let root = temp_volume("identity");
        let file_path = root.join("candidate.bin");
        fs::write(&file_path, b"before").unwrap();
        let expected = read_file_identity(&file_path).unwrap();
        NativeQuarantineFs
            .validate_file_identity(&file_path, expected)
            .unwrap();

        fs::write(&file_path, b"after-change").unwrap();
        let error = NativeQuarantineFs
            .validate_file_identity(&file_path, expected)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::FileChanged);
        assert!(error.message.contains("змінився після сканування"));
        assert!(
            file_path.exists(),
            "перевірка не переміщує і не видаляє файл"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mtime_mismatch_is_rejected_even_when_size_matches() {
        use trashradar_domain::{candidate::FsTimestamp, error::ErrorCode};

        let root = temp_volume("mtime");
        let file_path = root.join("same-size.bin");
        fs::write(&file_path, b"12345678").unwrap();
        let live = read_file_identity(&file_path).unwrap();
        let expected = FileIdentity {
            size: live.size,
            modified_at: live.modified_at.map(|time| FsTimestamp(time.0 - 1)),
        };
        assert_eq!(
            NativeQuarantineFs
                .validate_file_identity(&file_path, expected)
                .unwrap_err()
                .code,
            ErrorCode::FileChanged
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn rejects_missing_root_and_invalid_volume() {
        let missing = std::env::temp_dir().join("trashradar-t077-missing-root");
        let _ = fs::remove_dir_all(&missing);
        assert!(NativeQuarantineFs.ensure_at_root(&missing).is_err());
        assert!(NativeQuarantineFs.ensure_on_volume('1').is_err());
    }
}
