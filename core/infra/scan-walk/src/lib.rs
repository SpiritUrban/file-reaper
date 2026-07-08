//! Адаптер `ScanSource`: паралельний обхід каталогів з work-stealing (T-026).
//!
//! Резервний шлях скану (architecture.md §2.2): для не-NTFS, відсутності
//! elevation і випадків, коли MFT недоступний. Воркери розбирають чергу
//! каталогів; глибина не обмежує паралелізм. Виключені шляхи — T-027.
//!
//! Вихід — той самий [`ScanEntry`], що й у `scan-mft`, зі синтетичними
//! `file_ref`/`parent_ref` (ланцюжок батьків від кореня тому, ref = 0).
//! `attributes.raw_bits` — Win32 file attributes.

mod walk;

pub use walk::{walk_path, walk_volume, walk_volume_with, WalkConfig, WalkStats};

use trashradar_app::ports::ScanSource;
use trashradar_domain::error::CoreError;
use trashradar_domain::scan::ScanEntry;

/// Джерело скану через паралельний обхід каталогів.
#[derive(Debug, Clone)]
pub struct WalkScanner {
    config: WalkConfig,
}

impl WalkScanner {
    pub fn new(config: WalkConfig) -> Self {
        Self { config }
    }

    pub fn with_default_workers() -> Self {
        Self::new(WalkConfig::default())
    }
}

impl Default for WalkScanner {
    fn default() -> Self {
        Self::with_default_workers()
    }
}

impl ScanSource for WalkScanner {
    fn scan_volume(&self, volume: char) -> Result<Vec<ScanEntry>, CoreError> {
        walk_volume(volume, self.config)
    }
}

/// Реконструює повний шлях запису за синтетичними `parent_ref` (корінь = 0).
/// Потрібно для звірки з MFT/диском і майбутнього pipeline (аналог PathResolver).
pub fn full_path(drive: char, entries: &[ScanEntry], entry: &ScanEntry) -> Option<String> {
    walk::full_path(drive, entries, entry)
}
