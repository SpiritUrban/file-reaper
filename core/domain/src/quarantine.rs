//! Quarantine: життєвий цикл «передсмертної зони» (docs/architecture.md §7).
//!
//! Інваріант переходів: `Quarantined → Restored | Purged`, інших переходів
//! не існує. Правила переходів реалізуються у T-078/T-079.

use serde::{Deserialize, Serialize};

use crate::candidate::ByteSize;

/// Ім'я службового каталогу TrashRadar у корені тому (`<том>\.trashradar`).
/// Єдине джерело правди для guard-list (T-085), FS-шлюзу (T-077) і виключення
/// зі сканування (T-088).
pub const SERVICE_DIRECTORY_NAME: &str = ".trashradar";

/// Чи лежить шлях у службовому каталозі TrashRadar тому (`<X>:\.trashradar\…`).
///
/// Правило продукту «сміття не знаходить саме себе» (architecture.md §7.4):
/// вміст карантину ніколи не потрапляє в кандидати — жодне джерело скану
/// (MFT / walk / USN-дельта) не заносить такі шляхи в індекс.
pub fn is_under_service_directory(path: &str) -> bool {
    let Some(normalized) = normalize_windows_path(path) else {
        return false;
    };
    let Some(root) = volume_root_prefix(&normalized) else {
        return false;
    };
    let service_root = format!("{root}{SERVICE_DIRECTORY_NAME}");
    path_is_under(&normalized, &service_root)
}

/// Префікс кореня тому в уже нормалізованому шляху: `x:\` на Windows, `/` на
/// Unix. `None` — шлях не абсолютний, і службового каталогу в ньому бути не
/// може.
///
/// Правило 6a: без цієї розвилки перевірка вимагала літери диска й на Unix
/// поверталася `false` **завжди** — тобто вміст карантину потрапляв би назад
/// у кандидати, і застосунок «знаходив сам себе». Помилки при цьому нема
/// жодної, лише тихо неправильна поведінка.
/// Реалізація розділена справжніми `cfg`-блоками, а не `if cfg!(windows)`:
/// `cfg!` — це рантайм-булеан, тому **обидві гілки все одно компілюються**, і
/// виклик Windows-only `drive_letter` завалив би збірку на Linux
/// (`cannot find function in this scope`).
#[cfg(windows)]
fn volume_root_prefix(normalized: &str) -> Option<&str> {
    // normalized має форму `x:\...`, коли літера диска є.
    drive_letter(normalized).map(|_| &normalized[..3])
}

#[cfg(not(windows))]
fn volume_root_prefix(normalized: &str) -> Option<&str> {
    normalized
        .starts_with(crate::path_key::SEPARATOR)
        .then(|| &normalized[..1])
}

/// Ідентифікатор запису журналу Quarantine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuarantineEntryId(pub u64);

/// Ідентифікатор батчу операції (для масового «Скасувати», T-081).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BatchId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOperation {
    Reap,
    Restore,
    PurgeTtl,
    PurgeManual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditActor {
    User,
    Sweeper,
    Recovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestructiveAuditEvent {
    pub entry_id: QuarantineEntryId,
    pub batch_id: Option<BatchId>,
    pub operation: AuditOperation,
    pub actor: AuditActor,
    pub original_path: String,
    pub size: ByteSize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestructiveAuditRecord {
    pub sequence: u64,
    pub event: DestructiveAuditEvent,
    pub occurred_at_unix: i64,
}

/// Статус запису журналу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineStatus {
    /// Move розпочато, але не підтверджено (вікно crash recovery, T-084).
    InFlight,
    Quarantined,
    Restored,
    Purged,
}
/// Persistent-запис manifest (T-078, architecture.md §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantineEntry {
    pub id: QuarantineEntryId,
    pub batch_id: Option<BatchId>,
    pub original_path: String,
    pub surrogate_name: String,
    pub size: ByteSize,
    pub quarantined_at_unix: i64,
    pub expires_at_unix: i64,
    pub status: QuarantineStatus,
}

/// Ідентичність файла для optimistic concurrency перед move (T-086).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIdentity {
    pub size: ByteSize,
    pub modified_at: Option<crate::candidate::FsTimestamp>,
}

impl FileIdentity {
    pub fn validate_unchanged(self, live: Self, path: &str) -> Result<(), crate::error::CoreError> {
        if self == live {
            return Ok(());
        }
        Err(crate::error::CoreError::file_changed(format!(
            "Файл «{path}» змінився після сканування (очікувалось size={} mtime={:?}, зараз size={} mtime={:?}); reap скасовано.",
            self.size.0, self.modified_at, live.size.0, live.modified_at
        )))
    }
}
/// Стеля спроб підбору вільного імені при відновленні (T-080).
pub const RESTORE_SUFFIX_MAX_ATTEMPTS: u32 = 100;

/// Шлях призначення для відновлення з карантину (T-080, чисте правило).
///
/// Спроба 0 — оригінальний шлях як є; далі суфікс перед розширенням:
/// `clip.mp4` → `clip (відновлено).mp4` → `clip (відновлено 2).mp4` → …
/// Існування шляху перевіряє шлюз (atomic no-replace move), не це правило.
pub fn restore_destination(original_path: &str, attempt: u32) -> String {
    if attempt == 0 {
        return original_path.to_string();
    }
    let (directory, name) = match original_path.rfind(['\\', '/']) {
        Some(separator) => original_path.split_at(separator + 1),
        None => ("", original_path),
    };
    // Остання крапка не на початку імені; dotfile (`.gitignore`) — без розширення.
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    let suffix = if attempt == 1 {
        " (відновлено)".to_string()
    } else {
        format!(" (відновлено {attempt})")
    };
    format!("{directory}{stem}{suffix}{extension}")
}

/// Причина блокування шляху останньою лінією захисту (T-085).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedPathKind {
    System,
    Applications,
    TrashRadar,
    UnsupportedLocation,
}

/// Доменний guard-list. Не торкається FS і не довіряє вердикту детектора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineGuard {
    protected_roots: Vec<(String, ProtectedPathKind)>,
}

impl QuarantineGuard {
    /// Мінімальний незмінний guard-list для локального Windows-тому.
    pub fn windows_volume(volume: char) -> Self {
        if !volume.is_ascii_alphabetic() {
            return Self {
                protected_roots: Vec::new(),
            };
        }
        let drive = volume.to_ascii_lowercase();
        let roots = [
            (format!(r"{drive}:\windows"), ProtectedPathKind::System),
            (format!(r"{drive}:\programdata"), ProtectedPathKind::System),
            (format!(r"{drive}:\$recycle.bin"), ProtectedPathKind::System),
            (
                format!(r"{drive}:\system volume information"),
                ProtectedPathKind::System,
            ),
            (format!(r"{drive}:\recovery"), ProtectedPathKind::System),
            (
                format!(r"{drive}:\program files"),
                ProtectedPathKind::Applications,
            ),
            (
                format!(r"{drive}:\program files (x86)"),
                ProtectedPathKind::Applications,
            ),
            (
                format!(r"{drive}:\{SERVICE_DIRECTORY_NAME}"),
                ProtectedPathKind::TrashRadar,
            ),
        ];
        Self {
            protected_roots: roots.into_iter().collect(),
        }
    }

    /// Guard-list для Unix-платформ (Linux, macOS).
    ///
    /// Правило 6a брифу Стадії 2, останній рядок таблиці: «список системних
    /// папок виду `c:\windows` — на Linux/macOS захист просто не діє, тихо».
    /// Саме так і було: [`Self::windows_volume`] на Unix-шляху отримував том
    /// `'?'`, повертав ПОРОЖНІЙ список, і `validate` пропускав будь-що —
    /// включно з `/usr/bin`. Помилки при цьому не було жодної.
    pub fn unix_roots() -> Self {
        let system = [
            "/bin", "/sbin", "/boot", "/dev", "/etc", "/lib", "/lib32", "/lib64", "/proc", "/run",
            "/sys", "/usr", "/var", "/system", "/private", "/library", "/volumes",
        ];
        let applications = ["/applications", "/opt", "/snap"];
        let mut protected_roots: Vec<(String, ProtectedPathKind)> = system
            .into_iter()
            .map(|root| (root.to_string(), ProtectedPathKind::System))
            .chain(
                applications
                    .into_iter()
                    .map(|root| (root.to_string(), ProtectedPathKind::Applications)),
            )
            .collect();
        protected_roots.push((
            format!("/{SERVICE_DIRECTORY_NAME}"),
            ProtectedPathKind::TrashRadar,
        ));
        Self { protected_roots }
    }

    /// Guard-list поточної платформи: том — лише на Windows.
    ///
    /// Єдина точка вибору, щоб жодна платформа не лишилась із порожнім
    /// списком через те, що хтось викликав «не той» конструктор.
    pub fn for_current_platform(volume: char) -> Self {
        if cfg!(windows) {
            Self::windows_volume(volume)
        } else {
            Self::unix_roots()
        }
    }

    /// Додати власні файли/каталоги TrashRadar (профіль, БД, кеш, executable).
    pub fn with_trashradar_roots<I, S>(mut self, roots: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.protected_roots
            .extend(roots.into_iter().filter_map(|root| {
                normalize_windows_path(root.as_ref())
                    .map(|path| (path, ProtectedPathKind::TrashRadar))
            }));
        self
    }

    pub fn classify(&self, candidate_path: &str) -> Option<ProtectedPathKind> {
        let normalized = normalize_windows_path(candidate_path)?;
        // Правило 6a: «немає кореня тому» — це мережевий/відносний шлях, а не
        // «не Windows». Через `drive_letter` тут на Unix кожен шлях ставав
        // UnsupportedLocation, і reap не працював би взагалі ніде.
        if volume_root_prefix(&normalized).is_none() {
            return Some(ProtectedPathKind::UnsupportedLocation);
        }
        self.protected_roots
            .iter()
            .find_map(|(root, kind)| path_is_under(&normalized, root).then_some(*kind))
    }

    pub fn validate(&self, candidate_path: &str) -> Result<(), crate::error::CoreError> {
        if normalize_windows_path(candidate_path).is_none() {
            return Err(crate::error::CoreError::invalid_argument(
                "Шлях для reap порожній або некоректний.",
            ));
        }
        if let Some(kind) = self.classify(candidate_path) {
            return Err(crate::error::CoreError::path_protected(format!(
                "Шлях «{candidate_path}» захищений guard-list ({kind:?}) і не може бути переміщений у Quarantine."
            )));
        }
        Ok(())
    }
}

fn normalize_windows_path(path: &str) -> Option<String> {
    use crate::path_key::{fold_case, normalize_separators, SEPARATOR};
    let mut normalized = fold_case(&normalize_separators(path.trim()));
    if normalized.is_empty() {
        return None;
    }
    // Префікс довгого шляху існує лише на Windows; на Unix `\\?\` — це
    // звичайні символи імені, і зрізати їх не можна (правило 6a).
    #[cfg(windows)]
    if let Some(stripped) = normalized.strip_prefix(r"\\?\") {
        normalized = stripped.to_string();
    }
    let doubled = format!("{SEPARATOR}{SEPARATOR}");
    while normalized.contains(&doubled) {
        normalized = normalized.replace(&doubled, &SEPARATOR.to_string());
    }
    while normalized.ends_with(SEPARATOR) {
        normalized.pop();
    }
    (!normalized.is_empty()).then_some(normalized)
}

/// Літера диска — поняття, якого поза Windows не існує; без `cfg` функція
/// стала б мертвим кодом на Linux, а `-D warnings` у CI зробив би це помилкою
/// збірки (правило 6).
#[cfg(windows)]
fn drive_letter(path: &str) -> Option<char> {
    let bytes = path.as_bytes();
    (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\')
        .then(|| bytes[0] as char)
}

fn path_is_under(path: &str, root: &str) -> bool {
    let separator = crate::path_key::SEPARATOR as u8;
    path == root || (path.starts_with(root) && path.as_bytes().get(root.len()) == Some(&separator))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    /// Перевіряє guard-list **Windows-тому**: конструктор `windows_volume`,
    /// шляхи з літерою диска, регістронезалежне порівняння. Поза Windows це
    /// не «зламано», а просто інша модель шляхів — там діє `unix_roots`, і
    /// його покривають `platform_guard_tests` нижче (правило 6a).
    #[cfg(windows)]
    #[test]
    fn guard_blocks_system_app_and_own_paths_segment_safely() {
        let guard = QuarantineGuard::windows_volume('C')
            .with_trashradar_roots([r"C:\Users\Ada\AppData\Local\TrashRadar"]);
        for path in [
            r"C:\Windows\System32\kernel32.dll",
            r"c:/PROGRAM FILES/App/app.exe",
            r"C:\.trashradar\quarantine\entry.bin",
            r"C:\Users\Ada\AppData\Local\TrashRadar\index.sqlite3",
        ] {
            assert_eq!(
                guard.validate(path).unwrap_err().code,
                ErrorCode::PathProtected
            );
        }
        assert!(guard.validate(r"C:\Users\Ada\Videos\clip.mp4").is_ok());
        assert!(guard.validate(r"C:\WindowsBackup\clip.mp4").is_ok());
        assert!(guard.validate(r"C:\Program Files Backup\clip.mp4").is_ok());
    }

    /// Windows-форма шляху: літера диска, `\\?\`-префікс, регістронезалежність.
    /// Unix-двійник — [`service_directory_detection_on_unix`] нижче, щоб
    /// покриття цієї функції не зникло на другій платформі.
    #[cfg(windows)]
    #[test]
    fn service_directory_detection_is_segment_safe_and_normalized() {
        // Під службовим каталогом — будь-яка глибина, регістр, роздільники.
        assert!(is_under_service_directory(r"C:\.trashradar"));
        assert!(is_under_service_directory(
            r"C:\.trashradar\quarantine\00000001.bin"
        ));
        assert!(is_under_service_directory(r"d:/.TrashRadar/quarantine"));
        assert!(is_under_service_directory(
            r"\\?\E:\.trashradar\quarantine\x"
        ));
        // Не під ним: сусідні імена, службовий каталог не в корені тому, UNC.
        assert!(!is_under_service_directory(r"C:\.trashradar2\file.bin"));
        assert!(!is_under_service_directory(r"C:\Users\Ada\.trashradar\x"));
        assert!(!is_under_service_directory(r"C:\Users\Ada\video.mp4"));
        assert!(!is_under_service_directory(r"\\server\.trashradar\x"));
        assert!(!is_under_service_directory(""));
    }

    #[test]
    fn restore_destination_suffixes_before_extension() {
        let original = r"C:\Users\Ada\Videos\clip.mp4";
        assert_eq!(restore_destination(original, 0), original);
        assert_eq!(
            restore_destination(original, 1),
            r"C:\Users\Ada\Videos\clip (відновлено).mp4"
        );
        assert_eq!(
            restore_destination(original, 2),
            r"C:\Users\Ada\Videos\clip (відновлено 2).mp4"
        );
        // Без розширення — суфікс у кінці.
        assert_eq!(
            restore_destination(r"C:\data\archive", 1),
            r"C:\data\archive (відновлено)"
        );
        // Dotfile — крапка на початку не є розширенням.
        assert_eq!(
            restore_destination(r"C:\repo\.gitignore", 1),
            r"C:\repo\.gitignore (відновлено)"
        );
        // Multi-dot: суфікс перед останнім розширенням.
        assert_eq!(
            restore_destination(r"C:\data\backup.tar.gz", 1),
            r"C:\data\backup.tar (відновлено).gz"
        );
        // Ім'я без каталогу.
        assert_eq!(
            restore_destination("clip.mp4", 3),
            "clip (відновлено 3).mp4"
        );
    }

    #[test]
    fn guard_normalizes_extended_paths_and_rejects_unsupported_locations() {
        let guard = QuarantineGuard::windows_volume('D');
        assert_eq!(
            guard
                .validate(r"\\?\D:\Windows\Temp\x.tmp")
                .unwrap_err()
                .code,
            ErrorCode::PathProtected
        );
        assert_eq!(
            guard.validate(r"\\server\share\file.bin").unwrap_err().code,
            ErrorCode::PathProtected
        );
    }
}

#[cfg(test)]
mod platform_guard_tests {
    use super::*;

    /// Правило 33: спершу довести, що вхід непорожній. Guard із порожнім
    /// списком коренів пропускає ВСЕ — і робить це тихо й зелено.
    #[test]
    fn current_platform_guard_is_never_empty() {
        let guard = QuarantineGuard::for_current_platform('C');
        assert!(
            !guard.protected_roots.is_empty(),
            "guard-list поточної платформи порожній — остання лінія захисту вимкнена"
        );
    }

    /// Системний шлях СВОЄЇ платформи мусить бути заблокований. Очікуваний
    /// шлях будується під платформу, а не хардкодиться (правило 6a).
    #[test]
    fn system_path_of_this_platform_is_protected() {
        let guard = QuarantineGuard::for_current_platform('C');
        let system_path = if cfg!(windows) {
            r"C:\Windows\System32\kernel32.dll"
        } else {
            "/usr/bin/ls"
        };
        assert!(
            guard.classify(system_path).is_some(),
            "{system_path} мусить бути захищений guard-list'ом"
        );
        assert!(guard.validate(system_path).is_err());
    }

    /// Unix-двійник Windows-тесту `service_directory_detection_…`: службовий
    /// каталог лежить у корені файлової системи, а не тому з літерою.
    #[cfg(not(windows))]
    #[test]
    fn service_directory_detection_on_unix() {
        assert!(is_under_service_directory("/.trashradar"));
        assert!(is_under_service_directory(
            "/.trashradar/quarantine/00000001.bin"
        ));
        // Сусіднє ім'я, службовий каталог не в корені, відносний шлях і
        // порожній рядок — не під ним.
        assert!(!is_under_service_directory("/.trashradar2/file.bin"));
        assert!(!is_under_service_directory("/home/ada/.trashradar/x"));
        assert!(!is_under_service_directory("/home/ada/video.mp4"));
        assert!(!is_under_service_directory(".trashradar/x"));
        assert!(!is_under_service_directory(""));
    }

    /// А звичайний файл користувача — ні, інакше застосунок не працює взагалі.
    #[test]
    fn ordinary_user_path_is_not_protected() {
        let guard = QuarantineGuard::for_current_platform('C');
        let user_path = if cfg!(windows) {
            r"C:\Users\Ada\Downloads\big.iso"
        } else {
            "/home/ada/Downloads/big.iso"
        };
        assert!(guard.classify(user_path).is_none(), "{user_path}");
    }
}
