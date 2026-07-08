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

impl ScanStrategy {
    /// Стабільний рядок для health/IPC (збігається з serde).
    pub fn as_str(self) -> &'static str {
        match self {
            ScanStrategy::Mft => "mft",
            ScanStrategy::DirectoryWalk => "directory_walk",
            ScanStrategy::UsnDelta => "usn_delta",
        }
    }
}

/// Чому обрано стратегію (T-028). Видно у health-екрані.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStrategyReason {
    /// NTFS + процес з правами адміністратора → швидкий MFT-шлях.
    NtfsElevated,
    /// Том не NTFS (FAT/exFAT/ReFS/мережа…) → лише обхід каталогів.
    NotNtfs,
    /// NTFS, але без elevation → обхід (запит elevation — T-034).
    NotElevated,
}

impl ScanStrategyReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ScanStrategyReason::NtfsElevated => "ntfs_elevated",
            ScanStrategyReason::NotNtfs => "not_ntfs",
            ScanStrategyReason::NotElevated => "not_elevated",
        }
    }
}

/// Позиція в USN Change Journal тому (T-029).
///
/// `journal_id` ідентифікує конкретний екземпляр журналу: якщо ОС
/// перестворила журнал, id змінюється → потрібен повний рескан (T-031).
/// `next_usn` — USN, з якого читати наступну дельту (невключно з уже
/// обробленими записами).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsnCursor {
    pub journal_id: u64,
    pub next_usn: i64,
}

/// Знімок стану USN Journal (результат QUERY).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsnJournalInfo {
    pub journal_id: u64,
    /// Найменший ще валідний USN у журналі.
    pub lowest_valid_usn: i64,
    /// Перший USN, який ще не записаний (кінець журналу).
    pub next_usn: i64,
    pub first_usn: i64,
}

impl UsnJournalInfo {
    /// Курсор «на кінці журналу» — після повного скану (T-029 DoD:
    /// позиція зафіксована; наступна дельта = лише нові зміни).
    pub fn cursor_at_end(self) -> UsnCursor {
        UsnCursor {
            journal_id: self.journal_id,
            next_usn: self.next_usn,
        }
    }

    /// Чи `cursor` ще валідний для цього журналу.
    pub fn is_cursor_valid(self, cursor: UsnCursor) -> bool {
        cursor.journal_id == self.journal_id && cursor.next_usn >= self.lowest_valid_usn
    }
}

/// Одна зміна з USN Journal (T-029). Сирий запис до застосування в індекс (T-030).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsnChange {
    pub usn: i64,
    /// Номер запису MFT (нижні 48 біт FileReferenceNumber).
    pub file_ref: u64,
    pub parent_ref: u64,
    /// Бітова маска USN_REASON_*.
    pub reason: u32,
    pub name: String,
    pub is_directory: bool,
    pub timestamp: Option<FsTimestamp>,
}

/// Чому потрібен повний рескан замість USN-дельти (T-031).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FullRescanReason {
    /// Journal ID змінився (журнал перестворено / перезаписано).
    JournalIdChanged,
    /// Збережений USN нижче LowestValidUsn (старі записи витіснені).
    UsnBelowLowestValid,
    /// OS повернув ERROR_JOURNAL_ENTRY_DELETED.
    JournalEntryDeleted,
    /// Journal ID змінився під час читання дельти.
    JournalIdChangedDuringRead,
    /// Невідома / інша причина stale (стабільний фолбек).
    Other,
}

impl FullRescanReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FullRescanReason::JournalIdChanged => "journal_id_changed",
            FullRescanReason::UsnBelowLowestValid => "usn_below_lowest_valid",
            FullRescanReason::JournalEntryDeleted => "journal_entry_deleted",
            FullRescanReason::JournalIdChangedDuringRead => "journal_id_changed_during_read",
            FullRescanReason::Other => "other",
        }
    }

    /// Розпізнати reason-рядок з `UsnReadOutcome::JournalStale`.
    pub fn from_stale_reason(reason: &str) -> Self {
        match reason {
            "journal_id_changed" => FullRescanReason::JournalIdChanged,
            "usn_below_lowest_valid" => FullRescanReason::UsnBelowLowestValid,
            "journal_entry_deleted" => FullRescanReason::JournalEntryDeleted,
            "journal_id_changed_during_read" => FullRescanReason::JournalIdChangedDuringRead,
            _ => FullRescanReason::Other,
        }
    }

    /// Людське пояснення для UI (українською).
    pub fn user_message(self, volume: char) -> String {
        let v = volume.to_ascii_uppercase();
        match self {
            FullRescanReason::JournalIdChanged
            | FullRescanReason::JournalIdChangedDuringRead => format!(
                "Журнал змін тома {v}: перестворено. Запускається повне сканування."
            ),
            FullRescanReason::UsnBelowLowestValid | FullRescanReason::JournalEntryDeleted => {
                format!(
                    "Журнал змін тома {v}: застаріла позиція (записів більше немає). Запускається повне сканування."
                )
            }
            FullRescanReason::Other => format!(
                "Журнал змін тома {v}: недоступний для інкрементального оновлення. Запускається повне сканування."
            ),
        }
    }
}

/// Типові біти Reason з USN_RECORD (підмножина, потрібна дельті).
pub mod usn_reason {
    pub const DATA_OVERWRITE: u32 = 0x0000_0001;
    pub const DATA_EXTEND: u32 = 0x0000_0002;
    pub const DATA_TRUNCATION: u32 = 0x0000_0004;
    pub const FILE_CREATE: u32 = 0x0000_0100;
    pub const FILE_DELETE: u32 = 0x0000_0200;
    pub const RENAME_OLD_NAME: u32 = 0x0000_1000;
    pub const RENAME_NEW_NAME: u32 = 0x0000_2000;
    pub const BASIC_INFO_CHANGE: u32 = 0x0000_8000;
    pub const CLOSE: u32 = 0x8000_0000;

    /// Маска «зміни, релевантні індексу метаданих».
    pub const INDEX_RELEVANT: u32 = DATA_OVERWRITE
        | DATA_EXTEND
        | DATA_TRUNCATION
        | FILE_CREATE
        | FILE_DELETE
        | RENAME_OLD_NAME
        | RENAME_NEW_NAME
        | BASIC_INFO_CHANGE
        | CLOSE;
}
