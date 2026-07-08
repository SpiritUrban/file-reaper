//! Стани процесу сканування (docs/architecture.md §2).

use serde::{Deserialize, Serialize};

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
