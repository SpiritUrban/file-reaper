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

pub mod ports;
pub mod workers;
