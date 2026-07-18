//! Адаптер `QuarantineFs` — єдиний шлюз деструктивних операцій.
//!
//! ІНВАРІАНТ D4 (docs/architecture.md §8): лише цей крейт має право
//! змінювати файлову систему. T-077 створює службовий каталог на тому;
//! наступні задачі додають manifest, reap, restore, purge і recovery.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use trashradar_app::ports::{QuarantineFs, RecoveryLocation, RestoreMove, SurrogateState};
use trashradar_domain::{
    error::CoreError,
    quarantine::{is_under_service_directory, FileIdentity, QuarantineGuard},
};
use trashradar_platform_win::{
    move_file_no_replace, read_file_identity, set_hidden, NoReplaceMove,
};

pub use trashradar_domain::quarantine::SERVICE_DIRECTORY_NAME;

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

impl QuarantineFs for NativeQuarantineFs {
    fn move_into_quarantine(
        &self,
        source_path: &str,
        destination_path: &str,
        expected: FileIdentity,
        trashradar_roots: &[String],
    ) -> Result<(), CoreError> {
        self.validate_reap_path(source_path, trashradar_roots)?;
        let source = Path::new(source_path);
        let destination = Path::new(destination_path);
        self.validate_file_identity(source, expected)?;
        validate_quarantine_destination(destination)?;
        // T-089: без готового каталогу карантину reap неможливий — типізована
        // відмова замість generic io від rename (read-only / непідготовлений том).
        match destination.parent() {
            Some(quarantine_dir) if quarantine_dir.is_dir() => {}
            _ => {
                return Err(CoreError::quarantine_unavailable(format!(
                    "Том не має каталогу карантину ({}) — безпечне видалення неможливе; reap скасовано.",
                    destination.display()
                )));
            }
        }
        if let (Some(source_volume), Some(destination_volume)) = (
            windows_volume(source_path),
            windows_volume(destination_path),
        ) {
            if source_volume != destination_volume {
                return Err(CoreError::invalid_argument(
                    "Reap дозволяє лише atomic move у межах того самого тому.",
                ));
            }
        }
        fs::rename(source, destination).map_err(|error| {
            CoreError::io(format!(
                "Не вдалося перемістити {} у Quarantine: {error}",
                source.display()
            ))
        })
    }

    fn restore_from_quarantine(
        &self,
        surrogate_path: &str,
        destination_path: &str,
    ) -> Result<RestoreMove, CoreError> {
        let surrogate = Path::new(surrogate_path);
        let destination = Path::new(destination_path);
        // Відновлюємо лише з-під `.trashradar\quarantine` (та сама форма
        // шляху, що й destination reap-а).
        validate_quarantine_destination(surrogate).map_err(|_| {
            CoreError::invalid_argument(
                "Джерело restore має бути всередині .trashradar\\quarantine.",
            )
        })?;
        // Ціль ніколи не всередині службового каталогу (відновлення «в карантин»
        // безглузде і небезпечне для журналу).
        if is_under_service_directory(destination_path) {
            return Err(CoreError::invalid_argument(
                "Ціль restore не може бути в службовому каталозі TrashRadar.",
            ));
        }
        if let (Some(source_volume), Some(destination_volume)) = (
            windows_volume(surrogate_path),
            windows_volume(destination_path),
        ) {
            if source_volume != destination_volume {
                return Err(CoreError::invalid_argument(
                    "Restore дозволяє лише atomic move у межах того самого тому.",
                ));
            }
        }
        // Каталог оригінального шляху міг зникнути після reap — відтворюємо.
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                CoreError::io(format!(
                    "Не вдалося відтворити каталог для відновлення {}: {error}",
                    parent.display()
                ))
            })?;
        }
        match move_file_no_replace(surrogate, destination)? {
            NoReplaceMove::Moved => Ok(RestoreMove::Restored),
            NoReplaceMove::DestinationOccupied => Ok(RestoreMove::DestinationOccupied),
        }
    }

    fn purge_from_quarantine(&self, surrogate_path: &str) -> Result<(), CoreError> {
        let surrogate = Path::new(surrogate_path);
        validate_quarantine_destination(surrogate).map_err(|_| {
            CoreError::invalid_argument("Purge дозволений лише всередині .trashradar\\quarantine.")
        })?;
        let metadata = fs::symlink_metadata(surrogate).map_err(|error| {
            CoreError::io(format!(
                "Не вдалося перевірити surrogate перед purge: {error}"
            ))
        })?;
        let result = if metadata.is_dir() {
            fs::remove_dir_all(surrogate)
        } else {
            fs::remove_file(surrogate)
        };
        result.map_err(|error| {
            CoreError::io(format!(
                "Не вдалося остаточно видалити {} з Quarantine: {error}",
                surrogate.display()
            ))
        })
    }

    fn recovery_location(
        &self,
        source_path: &str,
        surrogate_path: &str,
    ) -> Result<RecoveryLocation, CoreError> {
        let surrogate = Path::new(surrogate_path);
        validate_quarantine_destination(surrogate).map_err(|_| {
            CoreError::invalid_argument(
                "Recovery surrogate має бути всередині .trashradar\\quarantine.",
            )
        })?;
        let source_exists = Path::new(source_path).try_exists().map_err(|error| {
            CoreError::io(format!("Не вдалося перевірити original path: {error}"))
        })?;
        let quarantine_exists = surrogate.try_exists().map_err(|error| {
            CoreError::io(format!(
                "Не вдалося перевірити quarantine surrogate: {error}"
            ))
        })?;
        Ok(match (source_exists, quarantine_exists) {
            (true, false) => RecoveryLocation::SourceOnly,
            (false, true) => RecoveryLocation::QuarantineOnly,
            (true, true) => RecoveryLocation::Both,
            (false, false) => RecoveryLocation::Missing,
        })
    }

    fn surrogate_state(&self, surrogate_path: &str) -> Result<SurrogateState, CoreError> {
        let surrogate = Path::new(surrogate_path);
        validate_quarantine_destination(surrogate).map_err(|_| {
            CoreError::invalid_argument("Surrogate має бути всередині .trashradar\\quarantine.")
        })?;
        if surrogate.try_exists().map_err(|error| {
            CoreError::io(format!(
                "Не вдалося перевірити quarantine surrogate: {error}"
            ))
        })? {
            return Ok(SurrogateState::Present);
        }
        // Файла немає — але це висновок лише тоді, коли є де його шукати:
        // недоступний каталог карантину (від'єднаний том) не свідчить ні про
        // що (T-156).
        let root_available = surrogate
            .parent()
            .map(|root| root.is_dir())
            .unwrap_or(false);
        Ok(if root_available {
            SurrogateState::Missing
        } else {
            SurrogateState::RootUnavailable
        })
    }
}

fn windows_volume(path: &str) -> Option<char> {
    let normalized = path.trim().trim_start_matches(r"\\?\");
    let bytes = normalized.as_bytes();
    (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        .then(|| (bytes[0] as char).to_ascii_uppercase())
}

fn validate_quarantine_destination(destination: &Path) -> Result<(), CoreError> {
    let quarantine = destination.parent().and_then(Path::file_name);
    let service = destination
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name);
    let valid = quarantine
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(QUARANTINE_DIRECTORY_NAME))
        && service
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(SERVICE_DIRECTORY_NAME));
    if valid {
        Ok(())
    } else {
        Err(CoreError::invalid_argument(
            "Destination reap має бути всередині .trashradar\\quarantine.",
        ))
    }
}

impl NativeQuarantineFs {
    pub fn surrogate_path(
        &self,
        original_path: &str,
        surrogate_name: &str,
    ) -> Result<PathBuf, CoreError> {
        let volume = windows_volume(original_path).ok_or_else(|| {
            CoreError::invalid_argument("Original path recovery не містить локального тому.")
        })?;
        let mut root = PathBuf::from(format!("{volume}:"));
        root.push(std::path::MAIN_SEPARATOR.to_string());
        Ok(root
            .join(SERVICE_DIRECTORY_NAME)
            .join(QUARANTINE_DIRECTORY_NAME)
            .join(surrogate_name))
    }

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

    /// T-089: чи можливий reap на томі. Успіх = службовий каталог існує
    /// (створюється) і реально записуваний; будь-яка відмова (read-only том,
    /// брак прав, недоступний корінь) → стабільний `quarantine_unavailable`
    /// з людським поясненням для UI.
    pub fn capability_on_volume(&self, volume: char) -> Result<QuarantineDirectory, CoreError> {
        self.ensure_on_volume(volume).map_err(|cause| {
            quarantine_unavailable_for(&format!("{}:", volume.to_ascii_uppercase()), &cause)
        })
    }

    /// Як [`Self::capability_on_volume`], з явним коренем (портативний режим, тести).
    pub fn capability_at_root(&self, volume_root: &Path) -> Result<QuarantineDirectory, CoreError> {
        self.ensure_at_root(volume_root)
            .map_err(|cause| quarantine_unavailable_for(&volume_root.display().to_string(), &cause))
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

/// Причина недоступності карантину → стабільний код + пояснення для UI (T-089).
fn quarantine_unavailable_for(volume: &str, cause: &CoreError) -> CoreError {
    CoreError::quarantine_unavailable(format!(
        "Том {volume} не підтримує карантин — reap для його файлів заблоковано. Причина: {}",
        cause.message
    ))
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

    /// Ізолюючий тест: reap РЕАЛЬНОЇ порожньої теки через FS-шар. Ловить
    /// будь-яке файло-специфічне припущення, що ламає reap папок-одиниць.
    #[cfg(windows)]
    #[test]
    fn moves_real_empty_directory_into_quarantine() {
        let root = temp_volume("reap_empty_dir");
        let directory = NativeQuarantineFs.ensure_at_root(&root).unwrap();

        let empty = root.join("EmptyFolder");
        fs::create_dir_all(&empty).unwrap();
        let empty_str = empty.to_string_lossy().into_owned();

        let identity = trashradar_platform_win::read_file_identity(&empty).unwrap();
        let destination = directory.quarantine_root.join("0000000000000001.bin");
        let dest_str = destination.to_string_lossy().into_owned();
        let roots = vec![directory.service_root.to_string_lossy().into_owned()];

        let result =
            NativeQuarantineFs.move_into_quarantine(&empty_str, &dest_str, identity, &roots);
        assert!(result.is_ok(), "reap порожньої теки провалився: {result:?}");
        assert!(!empty.exists(), "тека мала зникнути з оригінального місця");
        assert!(destination.is_dir(), "тека має опинитись у карантині як директорія");

        fs::remove_dir_all(&root).unwrap();
    }

    /// Той самий шлях, але тека З ВМІСТОМ (sparse/deep/dev): rename переносить
    /// усе піддерево одним махом.
    #[cfg(windows)]
    #[test]
    fn moves_real_directory_with_contents_into_quarantine() {
        let root = temp_volume("reap_dir_contents");
        let directory = NativeQuarantineFs.ensure_at_root(&root).unwrap();

        let folder = root.join("Sparse");
        fs::create_dir_all(folder.join("sub")).unwrap();
        fs::write(folder.join("a.txt"), b"hi").unwrap();
        fs::write(folder.join("sub").join("b.txt"), b"there").unwrap();
        let folder_str = folder.to_string_lossy().into_owned();

        let identity = trashradar_platform_win::read_file_identity(&folder).unwrap();
        let destination = directory.quarantine_root.join("0000000000000002.bin");
        let dest_str = destination.to_string_lossy().into_owned();
        let roots = vec![directory.service_root.to_string_lossy().into_owned()];

        let result =
            NativeQuarantineFs.move_into_quarantine(&folder_str, &dest_str, identity, &roots);
        assert!(result.is_ok(), "reap теки з вмістом провалився: {result:?}");
        assert!(!folder.exists());
        assert!(destination.join("a.txt").is_file(), "вміст переїхав разом з текою");

        fs::remove_dir_all(&root).unwrap();
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
    #[cfg(windows)]
    #[test]
    fn transactional_reaper_persists_move_and_confirmation() {
        use trashradar_app::{ReapRequest, TransactionalReaper};
        use trashradar_domain::quarantine::{
            BatchId, QuarantineEntry, QuarantineEntryId, QuarantineStatus,
        };
        use trashradar_index_sqlite::IndexDatabase;

        let root = temp_volume("transactional-reap");
        let directory = NativeQuarantineFs.ensure_at_root(&root).unwrap();
        let source = root.join("candidate.mp4");
        fs::write(&source, b"video-data").unwrap();
        let identity = read_file_identity(&source).unwrap();
        let surrogate = "00000077.bin".to_string();
        let destination = directory.quarantine_root.join(&surrogate);
        let database = IndexDatabase::open_profile(root.join("profile")).unwrap();
        let request = ReapRequest {
            entry: QuarantineEntry {
                id: QuarantineEntryId(77),
                batch_id: Some(BatchId(9)),
                original_path: source.to_string_lossy().into_owned(),
                surrogate_name: surrogate,
                size: identity.size,
                quarantined_at_unix: 1_750_000_000,
                expires_at_unix: 1_752_592_000,
                status: QuarantineStatus::InFlight,
            },
            destination_path: destination.to_string_lossy().into_owned(),
            expected_identity: identity,
        };

        let outcome = TransactionalReaper::new(&NativeQuarantineFs, &database, &[])
            .reap_one(request)
            .unwrap();
        assert_eq!(outcome.entry.status, QuarantineStatus::Quarantined);
        assert!(!source.exists());
        assert!(destination.exists());
        assert_eq!(
            database
                .get_quarantine_entry(QuarantineEntryId(77))
                .unwrap()
                .unwrap()
                .status,
            QuarantineStatus::Quarantined
        );

        // T-080: повний цикл — restore повертає файл, manifest → Restored.
        use trashradar_app::{QuarantineRestorer, RestoreRequest};
        let restored = QuarantineRestorer::new(&NativeQuarantineFs, &database)
            .restore_one(RestoreRequest {
                entry_id: QuarantineEntryId(77),
                surrogate_path: destination.to_string_lossy().into_owned(),
            })
            .unwrap();
        assert!(!restored.used_suffix);
        assert_eq!(fs::read(&source).unwrap(), b"video-data");
        assert!(!destination.exists());
        assert_eq!(
            database
                .get_quarantine_entry(QuarantineEntryId(77))
                .unwrap()
                .unwrap()
                .status,
            QuarantineStatus::Restored
        );
        drop(database);
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn atomic_move_places_exactly_one_copy_in_quarantine() {
        let root = temp_volume("atomic-move");
        let directory = NativeQuarantineFs.ensure_at_root(&root).unwrap();
        let source = root.join("candidate.bin");
        let destination = directory.quarantine_root.join("00000001.bin");
        fs::write(&source, b"candidate-data").unwrap();
        let identity = read_file_identity(&source).unwrap();

        NativeQuarantineFs
            .move_into_quarantine(
                &source.to_string_lossy(),
                &destination.to_string_lossy(),
                identity,
                &[],
            )
            .unwrap();

        assert!(!source.exists());
        assert_eq!(fs::read(&destination).unwrap(), b"candidate-data");
        fs::remove_dir_all(root).unwrap();
    }
    /// DoD T-080: reap → restore повертає файл на оригінальний шлях (реальна FS),
    /// включно з відтворенням зниклого батьківського каталогу.
    #[test]
    fn restore_roundtrip_returns_file_to_original_path() {
        let root = temp_volume("restore-roundtrip");
        let directory = NativeQuarantineFs.ensure_at_root(&root).unwrap();
        let nested = root.join("projects").join("video");
        fs::create_dir_all(&nested).unwrap();
        let source = nested.join("candidate.mp4");
        fs::write(&source, b"video-data").unwrap();
        let identity = read_file_identity(&source).unwrap();
        let surrogate = directory.quarantine_root.join("00000042.bin");

        NativeQuarantineFs
            .move_into_quarantine(
                &source.to_string_lossy(),
                &surrogate.to_string_lossy(),
                identity,
                &[],
            )
            .unwrap();
        assert!(!source.exists());

        // Каталог оригіналу зник після reap — restore його відтворює.
        fs::remove_dir_all(&nested).unwrap();

        let moved = NativeQuarantineFs
            .restore_from_quarantine(&surrogate.to_string_lossy(), &source.to_string_lossy())
            .unwrap();
        assert_eq!(moved, RestoreMove::Restored);
        assert_eq!(fs::read(&source).unwrap(), b"video-data");
        assert!(!surrogate.exists());
        fs::remove_dir_all(root).unwrap();
    }

    /// DoD T-080: зайнятий оригінальний шлях НЕ перезаписується — шлюз
    /// повертає DestinationOccupied, обидва файли недоторкані.
    #[test]
    fn restore_occupied_destination_is_never_overwritten() {
        let root = temp_volume("restore-occupied");
        let directory = NativeQuarantineFs.ensure_at_root(&root).unwrap();
        let source = root.join("report.pdf");
        fs::write(&source, b"original").unwrap();
        let identity = read_file_identity(&source).unwrap();
        let surrogate = directory.quarantine_root.join("00000007.bin");
        NativeQuarantineFs
            .move_into_quarantine(
                &source.to_string_lossy(),
                &surrogate.to_string_lossy(),
                identity,
                &[],
            )
            .unwrap();

        // Користувач створив НОВИЙ файл на тому самому шляху.
        fs::write(&source, b"users-new-file").unwrap();

        let moved = NativeQuarantineFs
            .restore_from_quarantine(&surrogate.to_string_lossy(), &source.to_string_lossy())
            .unwrap();
        assert_eq!(moved, RestoreMove::DestinationOccupied);
        assert_eq!(fs::read(&source).unwrap(), b"users-new-file");
        assert_eq!(fs::read(&surrogate).unwrap(), b"original");
        fs::remove_dir_all(root).unwrap();
    }

    /// Межі restore: джерело лише з карантину; ціль ніколи не в службовому каталозі.
    #[test]
    fn restore_validates_source_and_destination_boundaries() {
        use trashradar_domain::error::ErrorCode;

        let outside_source = NativeQuarantineFs
            .restore_from_quarantine(r"C:\Users\Ada\random.bin", r"C:\Users\Ada\target.bin")
            .unwrap_err();
        assert_eq!(outside_source.code, ErrorCode::InvalidArgument);

        let into_service = NativeQuarantineFs
            .restore_from_quarantine(
                r"C:\.trashradar\quarantine\00000001.bin",
                r"C:\.trashradar\quarantine\evil.bin",
            )
            .unwrap_err();
        assert_eq!(into_service.code, ErrorCode::InvalidArgument);
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

    /// DoD T-089: reap на томі без каталогу карантину відхиляється зі
    /// стабільним кодом і поясненням; файл лишається на місці.
    #[test]
    fn reap_blocked_without_quarantine_directory() {
        use trashradar_domain::error::ErrorCode;

        let root = temp_volume("no-quarantine");
        let source = root.join("candidate.bin");
        fs::write(&source, b"candidate-data").unwrap();
        let identity = read_file_identity(&source).unwrap();
        // Каталог карантину свідомо НЕ створено (том «без можливості карантину»).
        let destination = root
            .join(SERVICE_DIRECTORY_NAME)
            .join(QUARANTINE_DIRECTORY_NAME)
            .join("00000001.bin");

        let error = NativeQuarantineFs
            .move_into_quarantine(
                &source.to_string_lossy(),
                &destination.to_string_lossy(),
                identity,
                &[],
            )
            .unwrap_err();

        assert_eq!(error.code, ErrorCode::QuarantineUnavailable);
        assert!(error.message.contains("reap скасовано"));
        assert!(source.exists(), "файл не переміщено і не втрачено");
        assert!(!destination.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_reports_source_quarantine_both_and_missing() {
        let root = temp_volume("recovery");
        let directory = NativeQuarantineFs.ensure_at_root(&root).unwrap();
        let source = root.join("original.bin");
        let surrogate = directory.quarantine_root.join("in-flight.bin");
        fs::write(&source, b"source").unwrap();
        assert_eq!(
            NativeQuarantineFs
                .recovery_location(&source.to_string_lossy(), &surrogate.to_string_lossy())
                .unwrap(),
            RecoveryLocation::SourceOnly
        );
        fs::write(&surrogate, b"quarantine").unwrap();
        assert_eq!(
            NativeQuarantineFs
                .recovery_location(&source.to_string_lossy(), &surrogate.to_string_lossy())
                .unwrap(),
            RecoveryLocation::Both
        );
        fs::remove_file(&source).unwrap();
        assert_eq!(
            NativeQuarantineFs
                .recovery_location(&source.to_string_lossy(), &surrogate.to_string_lossy())
                .unwrap(),
            RecoveryLocation::QuarantineOnly
        );
        fs::remove_file(&surrogate).unwrap();
        assert_eq!(
            NativeQuarantineFs
                .recovery_location(&source.to_string_lossy(), &surrogate.to_string_lossy())
                .unwrap(),
            RecoveryLocation::Missing
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// T-156: стан сурогата при старті. Файл на місці → Present; каталог
    /// карантину доступний, файла немає → Missing (перерваний purge знищив
    /// його); недоступний каталог → RootUnavailable (том від'єднано — про
    /// запис не відомо нічого, чіпати не можна).
    #[test]
    fn surrogate_state_distinguishes_present_missing_and_unavailable_root() {
        let root = temp_volume("surrogate-state");
        let directory = NativeQuarantineFs.ensure_at_root(&root).unwrap();
        let surrogate = directory.quarantine_root.join("entry.bin");

        fs::write(&surrogate, b"payload").unwrap();
        assert_eq!(
            NativeQuarantineFs
                .surrogate_state(&surrogate.to_string_lossy())
                .unwrap(),
            SurrogateState::Present
        );

        fs::remove_file(&surrogate).unwrap();
        assert_eq!(
            NativeQuarantineFs
                .surrogate_state(&surrogate.to_string_lossy())
                .unwrap(),
            SurrogateState::Missing,
            "каталог карантину на місці, файла немає → знищений"
        );

        // Каталог карантину зник (том від'єднано) — висновку про файл немає.
        fs::remove_dir_all(&directory.quarantine_root).unwrap();
        assert_eq!(
            NativeQuarantineFs
                .surrogate_state(&surrogate.to_string_lossy())
                .unwrap(),
            SurrogateState::RootUnavailable
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn purge_removes_only_surrogate_inside_quarantine() {
        use trashradar_domain::error::ErrorCode;

        let root = temp_volume("purge");
        let directory = NativeQuarantineFs.ensure_at_root(&root).unwrap();
        let surrogate = directory.quarantine_root.join("expired.bin");
        fs::write(&surrogate, b"expired").unwrap();
        NativeQuarantineFs
            .purge_from_quarantine(&surrogate.to_string_lossy())
            .unwrap();
        assert!(!surrogate.exists());

        let outside = root.join("outside.bin");
        fs::write(&outside, b"keep").unwrap();
        let error = NativeQuarantineFs
            .purge_from_quarantine(&outside.to_string_lossy())
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(outside.exists());
        fs::remove_dir_all(root).unwrap();
    }

    /// T-089: capability-проба дає стабільний код і людську причину для UI;
    /// на записуваному томі — Ok.
    #[test]
    fn capability_probe_reports_unavailable_with_reason() {
        use trashradar_domain::error::ErrorCode;

        let missing = std::env::temp_dir().join("trashradar-t089-missing-root");
        let _ = fs::remove_dir_all(&missing);
        let error = NativeQuarantineFs.capability_at_root(&missing).unwrap_err();
        assert_eq!(error.code, ErrorCode::QuarantineUnavailable);
        assert!(error.message.contains("не підтримує карантин"));
        assert!(error.message.contains("Причина:"));

        let writable = temp_volume("capability-ok");
        let directory = NativeQuarantineFs.capability_at_root(&writable).unwrap();
        assert!(directory.quarantine_root.is_dir());
        fs::remove_dir_all(writable).unwrap();

        // Некоректна літера тому — теж «карантин недоступний», не панік.
        assert_eq!(
            NativeQuarantineFs
                .capability_on_volume('1')
                .unwrap_err()
                .code,
            ErrorCode::QuarantineUnavailable
        );
    }
}
