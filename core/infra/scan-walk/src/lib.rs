//! Адаптер `ScanSource`: паралельний обхід каталогів з work-stealing (T-026).
//!
//! Резервний шлях скану (architecture.md §2.2): для не-NTFS, відсутності
//! elevation і випадків, коли MFT недоступний. Воркери розбирають чергу
//! каталогів; глибина не обмежує паралелізм.
//!
//! Виключені шляхи (T-027): у виключене піддерево сканер не заходить —
//! без `read_dir`/`metadata` на самому префіксі та нащадках. Список задається
//! в [`WalkConfig::exclusions`] (системні дефолти + користувацькі з Settings).
//!
//! Вихід — той самий [`ScanEntry`], що й у `scan-mft`, зі синтетичними
//! `file_ref`/`parent_ref` (ланцюжок батьків від кореня тому, ref = 0).
//! `attributes.raw_bits` — Win32 file attributes.

mod exclude;
mod walk;

pub use exclude::PathExclusions;
pub use walk::{
    full_path, walk_path, walk_path_with_stats, walk_volume, walk_volume_with, WalkConfig,
    WalkPathResolver, WalkStats, ROOT_REF,
};

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

    /// Сканер із системними Windows-виключеннями для тому (T-027 дефолти).
    /// Application/Settings зливають сюди й користувацький список (T-090/T-151).
    pub fn with_system_exclusions(drive: char) -> Self {
        Self::new(WalkConfig {
            exclusions: PathExclusions::windows_system_defaults(drive),
            ..WalkConfig::default()
        })
    }
}

impl Default for WalkScanner {
    fn default() -> Self {
        Self::with_default_workers()
    }
}

impl ScanSource for WalkScanner {
    fn scan_volume(&self, volume: char) -> Result<Vec<ScanEntry>, CoreError> {
        walk_volume(volume, self.config.clone())
    }
}
