//! Quarantine: життєвий цикл «передсмертної зони» (docs/architecture.md §7).
//!
//! Інваріант переходів: `Quarantined → Restored | Purged`, інших переходів
//! не існує. Правила переходів реалізуються у T-078/T-079.

use serde::{Deserialize, Serialize};

use crate::candidate::ByteSize;

/// Ідентифікатор запису журналу Quarantine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuarantineEntryId(pub u64);

/// Ідентифікатор батчу операції (для масового «Скасувати», T-081).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BatchId(pub u64);

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
                format!(r"{drive}:\.trashradar"),
                ProtectedPathKind::TrashRadar,
            ),
        ];
        Self {
            protected_roots: roots.into_iter().collect(),
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
        if drive_letter(&normalized).is_none() {
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
    let mut normalized = path.trim().replace('/', "\\").to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    if let Some(stripped) = normalized.strip_prefix(r"\\?\") {
        normalized = stripped.to_string();
    }
    while normalized.contains(r"\\") {
        normalized = normalized.replace(r"\\", r"\");
    }
    while normalized.ends_with('\\') {
        normalized.pop();
    }
    (!normalized.is_empty()).then_some(normalized)
}

fn drive_letter(path: &str) -> Option<char> {
    let bytes = path.as_bytes();
    (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\')
        .then(|| bytes[0] as char)
}

fn path_is_under(path: &str, root: &str) -> bool {
    path == root || (path.starts_with(root) && path.as_bytes().get(root.len()) == Some(&b'\\'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

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
