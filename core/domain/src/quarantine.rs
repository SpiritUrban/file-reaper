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
