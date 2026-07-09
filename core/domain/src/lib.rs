//! # Domain Layer — словник продукту в типах
//!
//! Правила шару (docs/repository.md §3):
//! - жодного I/O, WinAPI, SQLite, async-рантайму;
//! - лише типи, інваріанти та доменні правила;
//! - залежності: тільки `serde` (для контракту IPC).
//!
//! Каркас T-001: типи оголошені, доменні правила (політики, інваріанти
//! переходів) додаються задачами E4/E5/E7 з docs/tasks.md.

pub mod aggregate;
pub mod candidate;
pub mod category;
pub mod duplicates;
pub mod error;
pub mod forecast;
pub mod quarantine;
pub mod scan;

pub use aggregate::{summarize_unique, CandidateContribution, CategoryRollup, FreeableSummary};
pub use duplicates::{
    group_by_exact_size, group_by_partial_hash, ExactSizeGroup, PartialHash, PartialHashGroup,
    PartialHashKey, PartialHashStageStats, SizeKey, SizeStageStats, PARTIAL_HASH_CHUNK_BYTES,
    PARTIAL_HASH_MAX_READ_BYTES,
};
pub use forecast::{
    compute_cleanup_forecast, percent_of, CleanupForecast, ForecastInputs, VolumeUsage,
};
