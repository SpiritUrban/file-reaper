//! Кандидат на видалення — центральна сутність продукту.

use serde::{Deserialize, Serialize};

use crate::category::CategoryId;

/// Стабільний ідентифікатор запису в індексі.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateId(pub u64);

/// Розмір у байтах (value object, щоб не плутати з іншими u64).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ByteSize(pub u64);

/// Часова мітка FS (Windows FILETIME, 100-нс інтервали від 1601-01-01 UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FsTimestamp(pub i64);

/// Клас файла за розширенням (T-023).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Video,
    Image,
    Audio,
    Archive,
    Installer,
    DiskImage,
    Document,
    Other,
}

/// Що показує сітка: окремий файл або папка-одиниця (node_modules тощо, T-053).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateUnit {
    File,
    Folder,
}

/// Рішення користувача щодо кандидата.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Рішення ще не прийняте.
    Undecided,
    /// Залишити: ховається з кандидатів, рішення персистентне (T-057).
    Keep,
    /// Позначено до видалення (Reap Bar).
    Marked,
}

/// Вердикт детектора: чому файл є кандидатом (docs/architecture.md §6.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub category: CategoryId,
    /// Людське пояснення: «відео 4.2 ГБ, останній доступ 8 міс тому».
    pub explanation: String,
    pub safety: SafetyLevel,
}

/// Рівень безпечності масового видалення.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyLevel {
    /// Можна позначати категорію цілком ([Позначити все]).
    SafeToBulk,
    /// Рекомендований перегляд людиною (маркер «?» у REAP-флоу).
    ReviewRecommended,
}
