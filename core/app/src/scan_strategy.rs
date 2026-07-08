//! Автовибір шляху скану MFT ↔ обхід (T-028).
//!
//! architecture.md §2.1–2.2, repository.md §5: вибір робить Application;
//! `scan-mft` / `scan-walk` лише виконують. Правило:
//! **NTFS + elevated → MFT; інакше → directory walk.**
//!
//! Чиста функція над можливостями тому — без I/O; проби FS/прав —
//! у `platform-win`, збірка плану для health — у shell.

use trashradar_domain::scan::{ScanStrategy, ScanStrategyReason};

/// Спостережувані можливості тому / процесу для вибору стратегії.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeCapabilities {
    /// Файлова система — NTFS (GetVolumeInformation / еквівалент).
    pub is_ntfs: bool,
    /// Процес має адмін-права (TokenElevation).
    pub is_elevated: bool,
}

/// Рішення автовибору для одного тому.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanStrategyChoice {
    pub strategy: ScanStrategy,
    pub reason: ScanStrategyReason,
}

/// Обирає стратегію скану для тому за його можливостями (T-028).
///
/// | is_ntfs | elevated | результат |
/// |---------|----------|-----------|
/// | true    | true     | Mft / NtfsElevated |
/// | true    | false    | DirectoryWalk / NotElevated |
/// | false   | *        | DirectoryWalk / NotNtfs |
pub fn choose_scan_strategy(caps: &VolumeCapabilities) -> ScanStrategyChoice {
    if caps.is_ntfs && caps.is_elevated {
        ScanStrategyChoice {
            strategy: ScanStrategy::Mft,
            reason: ScanStrategyReason::NtfsElevated,
        }
    } else if !caps.is_ntfs {
        ScanStrategyChoice {
            strategy: ScanStrategy::DirectoryWalk,
            reason: ScanStrategyReason::NotNtfs,
        }
    } else {
        ScanStrategyChoice {
            strategy: ScanStrategy::DirectoryWalk,
            reason: ScanStrategyReason::NotElevated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntfs_elevated_selects_mft() {
        let c = choose_scan_strategy(&VolumeCapabilities {
            is_ntfs: true,
            is_elevated: true,
        });
        assert_eq!(c.strategy, ScanStrategy::Mft);
        assert_eq!(c.reason, ScanStrategyReason::NtfsElevated);
    }

    #[test]
    fn ntfs_without_elevation_falls_back_to_walk() {
        let c = choose_scan_strategy(&VolumeCapabilities {
            is_ntfs: true,
            is_elevated: false,
        });
        assert_eq!(c.strategy, ScanStrategy::DirectoryWalk);
        assert_eq!(c.reason, ScanStrategyReason::NotElevated);
    }

    #[test]
    fn non_ntfs_uses_walk_even_when_elevated() {
        let c = choose_scan_strategy(&VolumeCapabilities {
            is_ntfs: false,
            is_elevated: true,
        });
        assert_eq!(c.strategy, ScanStrategy::DirectoryWalk);
        assert_eq!(c.reason, ScanStrategyReason::NotNtfs);
    }

    #[test]
    fn non_ntfs_not_elevated_uses_walk() {
        let c = choose_scan_strategy(&VolumeCapabilities {
            is_ntfs: false,
            is_elevated: false,
        });
        assert_eq!(c.strategy, ScanStrategy::DirectoryWalk);
        assert_eq!(c.reason, ScanStrategyReason::NotNtfs);
    }
}
