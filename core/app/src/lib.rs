//! # Application Layer — use cases і порти
//!
//! Правила шару (docs/repository.md §4):
//! - залежить лише від `trashradar-domain`;
//! - зовнішній світ — виключно через порти (`ports`);
//! - тут живуть оркестратори: ферма детекторів, каскад дублікатів,
//!   Aggregator, планувальник фонових модулів.
//!
//! Каркас T-001: оголошені порти. Use cases додаються задачами
//! T-033, T-037, T-054, T-061, T-079 (docs/tasks.md).

pub mod aggregator;
pub mod change_monitor;
pub mod decisions;
pub mod detectors;
pub mod disk_forecast;
pub mod elevation;
pub mod location_registry;
pub mod mvp_farm;
pub mod ports;
pub mod scan_control;
pub mod scan_strategy;
pub mod usn_apply;
pub mod usn_fallback;
pub mod workers;

pub use aggregator::{Aggregator, LiveTotals};
pub use decisions::{
    apply_decision_hot, apply_decision_hot_and_persist, apply_decision_to_records, keep_hot,
    mark_hot, unkeep_hot, ApplyDecisionResult, DecisionSelector,
};
pub use disk_forecast::{marked_unique_bytes, DiskForecast, QuarantineHeld};
pub use mvp_farm::mvp_predicate_registry;
pub use trashradar_domain::aggregate::{CandidateContribution, CategoryRollup, FreeableSummary};
pub use trashradar_domain::forecast::{CleanupForecast, ForecastInputs, VolumeUsage};
