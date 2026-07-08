//! Стани процесу сканування (docs/architecture.md §2).

use serde::{Deserialize, Serialize};

use crate::candidate::{ByteSize, FileAttributes, FsTimestamp};

/// Сира знахідка сканування — один запис файлової системи, як його віддає
/// джерело скану (`ScanSource`) до побудови повного шляху (T-022) і
/// категоризації. Спільний вихід для MFT-парсера (T-021) і обходу (T-026).
///
/// `file_ref` / `parent_ref` — стабільні ідентифікатори запису та його
/// батьківської директорії (для NTFS — номери записів MFT); саме за ними
/// T-022 відновлює повний шлях. `name` — одна компонента імені, не шлях.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanEntry {
    pub file_ref: u64,
    pub parent_ref: u64,
    pub name: String,
    pub size: ByteSize,
    pub created_at: Option<FsTimestamp>,
    pub modified_at: Option<FsTimestamp>,
    pub accessed_at: Option<FsTimestamp>,
    pub is_directory: bool,
    pub attributes: FileAttributes,
}

/// Фаза сканування тому.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    Idle,
    /// Повний скан: MFT (швидкий шлях) або обхід (резервний).
    Full,
    /// Інкрементальна дельта з USN Journal.
    Incremental,
    Cancelled,
    Completed,
}

/// Яким шляхом сканується том (T-028).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStrategy {
    Mft,
    DirectoryWalk,
    UsnDelta,
}
