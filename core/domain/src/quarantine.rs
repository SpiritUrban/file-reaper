//! Quarantine: життєвий цикл «передсмертної зони» (docs/architecture.md §7).
//!
//! Інваріант переходів: `Quarantined → Restored | Purged`, інших переходів
//! не існує. Правила переходів реалізуються у T-078/T-079.

use serde::{Deserialize, Serialize};

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
