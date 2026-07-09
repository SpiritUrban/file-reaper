//! Каскад пошуку дублікатів (architecture.md §4, T-058…).
//!
//! Щабель 1 — лише метадані індексу (0 читань диска).

mod stage1;

pub use stage1::{run_size_stage, run_size_stage_from_index, SizeStageResult};
pub use trashradar_domain::duplicates::{
    group_by_exact_size, ExactSizeGroup, SizeKey, SizeStageStats,
};
