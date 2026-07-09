//! Пороги предикатних детекторів (T-038).
//!
//! architecture.md §6.4: пороги (мін. розмір, вік) змінюються «на льоту»;
//! предикатні детектори перераховуються з індексу **без рескану диска**.
//!
//! Значення типізовані; ключі — стабільні рядки (IPC `category.set_threshold`).

use trashradar_domain::error::CoreError;

/// Стабільні ключі порогів (UI / IPC / settings).
pub mod keys {
    /// Мінімальний розмір файла в байтах (LargeFiles, Archives, …).
    pub const MIN_SIZE_BYTES: &str = "min_size_bytes";
    /// Мінімальний вік у днях: файл «старший за N днів» (OldFiles, ForgottenVideos).
    /// Приклад DoD T-038: 180 (6 міс) → 90 (3 міс).
    pub const MIN_AGE_DAYS: &str = "min_age_days";
    /// Після скількох днів без правок джерел проєкт вважається **неактивним**
    /// (T-052, structural / DevArtifacts). Дефолт 90 (3 міс., product.md Сценарій C).
    /// Активний проєкт → артефакти `ReviewRecommended`, не safe-to-bulk.
    pub const INACTIVE_AFTER_DAYS: &str = "inactive_after_days";
}

/// Значення порога, яке UI/IPC передає детектору.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThresholdValue {
    /// Ціле беззнакова (байти, дні, кількість…).
    U64(u64),
    /// Логічний перемикач (рідко для предикатів).
    Bool(bool),
}

impl ThresholdValue {
    pub fn as_u64(self) -> Option<u64> {
        match self {
            ThresholdValue::U64(v) => Some(v),
            ThresholdValue::Bool(b) => Some(u64::from(b)),
        }
    }

    pub fn as_bool(self) -> Option<bool> {
        match self {
            ThresholdValue::Bool(b) => Some(b),
            ThresholdValue::U64(v) => Some(v != 0),
        }
    }
}

/// Помилка застосування порога → [`CoreError::invalid_argument`].
pub fn unknown_threshold(detector: &str, key: &str) -> CoreError {
    CoreError::invalid_argument(format!("Детектор «{detector}» не підтримує поріг «{key}»."))
}

pub fn bad_threshold_type(detector: &str, key: &str, expected: &str) -> CoreError {
    CoreError::invalid_argument(format!(
        "Детектор «{detector}»: поріг «{key}» очікує {expected}."
    ))
}
