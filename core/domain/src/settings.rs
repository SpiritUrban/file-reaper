//! Конфігурація Core: ефективні значення та optional overrides (T-090).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_QUARANTINE_TTL_DAYS: u32 = 30;
pub const DEFAULT_QUARANTINE_WARNING_BYTES: u64 = 50 * 1024 * 1024 * 1024;
pub const MIN_QUARANTINE_WARNING_BYTES: u64 = 1024 * 1024;
pub const MAX_QUARANTINE_TTL_DAYS: u32 = 3650;
pub const MAX_EXCLUDED_PATHS: usize = 4096;
/// Дефолтний поріг «майже порожньої» папки: рекурсивно 1..=N файлів (T: порожні/
/// майже порожні папки). N=3 — консервативно (Q&A з користувачем).
pub const DEFAULT_SPARSE_MAX_FILES: u32 = 3;
/// Стеля порога sparse: захист від абсурдних значень (папка з тисячами файлів
/// уже не «майже порожня»).
pub const MAX_SPARSE_MAX_FILES: u32 = 1000;
/// Дефолтний поріг «занадто глибокої» вкладеності: глибина папки (кількість
/// сегментів під коренем тому) понад це значення → розділ «Глибокі шляхи».
pub const DEFAULT_DEEP_PATH_MAX_DEPTH: u32 = 10;
/// Межі порога глибини: мін. 2 (сенс), макс. 64 (реальна вкладеність NTFS
/// значно менша; захист від абсурду).
pub const MIN_DEEP_PATH_MAX_DEPTH: u32 = 2;
pub const MAX_DEEP_PATH_MAX_DEPTH: u32 = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsFieldError {
    pub field: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub quarantine: QuarantineSettings,
    pub scan: ScanSettings,
    #[serde(default)]
    pub detectors: BTreeMap<String, DetectorSettings>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DetectorSettings {
    #[serde(default)]
    pub thresholds: BTreeMap<String, u64>,
    /// Перемикач детектора (T-152). Відсутнє поле у файлі/старому конфігу =
    /// увімкнено — тому дефолт true і серіалізація лише відхилення (false).
    #[serde(default = "detector_enabled_default", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

impl Default for DetectorSettings {
    fn default() -> Self {
        Self {
            thresholds: BTreeMap::new(),
            enabled: true,
        }
    }
}

fn detector_enabled_default() -> bool {
    true
}

#[allow(clippy::trivially_copy_pass_by_ref)] // сигнатура serde skip_serializing_if
fn is_true(value: &bool) -> bool {
    *value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineSettings {
    pub ttl_days: u32,
    pub warning_threshold_bytes: u64,
}

impl Default for QuarantineSettings {
    fn default() -> Self {
        Self {
            ttl_days: DEFAULT_QUARANTINE_TTL_DAYS,
            warning_threshold_bytes: DEFAULT_QUARANTINE_WARNING_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSettings {
    pub excluded_paths: Vec<String>,
    pub minimum_size_bytes: u64,
    /// Поріг розділу «майже порожні папки» (рекурсивно 1..=N файлів). Відсутнє
    /// поле у старому конфігу → [`DEFAULT_SPARSE_MAX_FILES`] (не 0 від derive).
    #[serde(default = "default_sparse_max_files")]
    pub sparse_max_files: u32,
    /// Поріг розділу «Глибокі шляхи»: глибина папки понад це значення.
    #[serde(default = "default_deep_path_max_depth")]
    pub deep_path_max_depth: u32,
    /// Літери томів, виключених з аналізу (напр. `["D", "E"]`). Порожньо =
    /// сканувати всі доступні томи. Відсутнє поле у старому конфігу → порожньо.
    #[serde(default)]
    pub excluded_volumes: Vec<String>,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            excluded_paths: Vec::new(),
            minimum_size_bytes: 0,
            sparse_max_files: DEFAULT_SPARSE_MAX_FILES,
            deep_path_max_depth: DEFAULT_DEEP_PATH_MAX_DEPTH,
            excluded_volumes: Vec::new(),
        }
    }
}

fn default_sparse_max_files() -> u32 {
    DEFAULT_SPARSE_MAX_FILES
}

fn default_deep_path_max_depth() -> u32 {
    DEFAULT_DEEP_PATH_MAX_DEPTH
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsOverrides {
    #[serde(default, skip_serializing_if = "QuarantineOverrides::is_empty")]
    pub quarantine: QuarantineOverrides,
    #[serde(default, skip_serializing_if = "ScanOverrides::is_empty")]
    pub scan: ScanOverrides,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub detectors: BTreeMap<String, DetectorSettings>,
}

impl SettingsOverrides {
    pub fn apply_to(&self, mut settings: AppSettings) -> AppSettings {
        if let Some(value) = self.quarantine.ttl_days {
            settings.quarantine.ttl_days = value;
        }
        if let Some(value) = self.quarantine.warning_threshold_bytes {
            settings.quarantine.warning_threshold_bytes = value;
        }
        if let Some(value) = &self.scan.excluded_paths {
            settings.scan.excluded_paths = value.clone();
        }
        if let Some(value) = self.scan.minimum_size_bytes {
            settings.scan.minimum_size_bytes = value;
        }
        if let Some(value) = self.scan.sparse_max_files {
            settings.scan.sparse_max_files = value;
        }
        if let Some(value) = self.scan.deep_path_max_depth {
            settings.scan.deep_path_max_depth = value;
        }
        if let Some(value) = &self.scan.excluded_volumes {
            settings.scan.excluded_volumes = value.clone();
        }
        settings.detectors = self.detectors.clone();
        settings
    }

    pub fn between(defaults: &AppSettings, settings: &AppSettings) -> Self {
        Self {
            quarantine: QuarantineOverrides {
                ttl_days: (settings.quarantine.ttl_days != defaults.quarantine.ttl_days)
                    .then_some(settings.quarantine.ttl_days),
                warning_threshold_bytes: (settings.quarantine.warning_threshold_bytes
                    != defaults.quarantine.warning_threshold_bytes)
                    .then_some(settings.quarantine.warning_threshold_bytes),
            },
            scan: ScanOverrides {
                excluded_paths: (settings.scan.excluded_paths != defaults.scan.excluded_paths)
                    .then(|| settings.scan.excluded_paths.clone()),
                minimum_size_bytes: (settings.scan.minimum_size_bytes
                    != defaults.scan.minimum_size_bytes)
                    .then_some(settings.scan.minimum_size_bytes),
                sparse_max_files: (settings.scan.sparse_max_files
                    != defaults.scan.sparse_max_files)
                    .then_some(settings.scan.sparse_max_files),
                deep_path_max_depth: (settings.scan.deep_path_max_depth
                    != defaults.scan.deep_path_max_depth)
                    .then_some(settings.scan.deep_path_max_depth),
                excluded_volumes: (settings.scan.excluded_volumes
                    != defaults.scan.excluded_volumes)
                    .then(|| settings.scan.excluded_volumes.clone()),
            },
            detectors: settings.detectors.clone(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.quarantine.is_empty() && self.scan.is_empty() && self.detectors.is_empty()
    }
}

pub fn validate_settings(settings: &AppSettings) -> Result<(), SettingsFieldError> {
    if !(1..=MAX_QUARANTINE_TTL_DAYS).contains(&settings.quarantine.ttl_days) {
        return Err(SettingsFieldError {
            field: "quarantine.ttlDays",
            message: format!("має бути від 1 до {MAX_QUARANTINE_TTL_DAYS}"),
        });
    }
    if settings.quarantine.warning_threshold_bytes < MIN_QUARANTINE_WARNING_BYTES {
        return Err(SettingsFieldError {
            field: "quarantine.warningThresholdBytes",
            message: format!("має бути не менше {MIN_QUARANTINE_WARNING_BYTES}"),
        });
    }
    if settings.scan.excluded_paths.len() > MAX_EXCLUDED_PATHS {
        return Err(SettingsFieldError {
            field: "scan.excludedPaths",
            message: format!("містить більше {MAX_EXCLUDED_PATHS} шляхів"),
        });
    }
    if let Some(index) = settings
        .scan
        .excluded_paths
        .iter()
        .position(|path| path.trim().is_empty())
    {
        return Err(SettingsFieldError {
            field: "scan.excludedPaths",
            message: format!("елемент {index} не може бути порожнім"),
        });
    }
    if !(1..=MAX_SPARSE_MAX_FILES).contains(&settings.scan.sparse_max_files) {
        return Err(SettingsFieldError {
            field: "scan.sparseMaxFiles",
            message: format!("має бути від 1 до {MAX_SPARSE_MAX_FILES}"),
        });
    }
    if !(MIN_DEEP_PATH_MAX_DEPTH..=MAX_DEEP_PATH_MAX_DEPTH)
        .contains(&settings.scan.deep_path_max_depth)
    {
        return Err(SettingsFieldError {
            field: "scan.deepPathMaxDepth",
            message: format!(
                "має бути від {MIN_DEEP_PATH_MAX_DEPTH} до {MAX_DEEP_PATH_MAX_DEPTH}"
            ),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuarantineOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning_threshold_bytes: Option<u64>,
}

impl QuarantineOverrides {
    pub fn is_empty(&self) -> bool {
        self.ttl_days.is_none() && self.warning_threshold_bytes.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excluded_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sparse_max_files: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deep_path_max_depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excluded_volumes: Option<Vec<String>>,
}

impl ScanOverrides {
    pub fn is_empty(&self) -> bool {
        self.excluded_paths.is_none()
            && self.minimum_size_bytes.is_none()
            && self.sparse_max_files.is_none()
            && self.deep_path_max_depth.is_none()
            && self.excluded_volumes.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_roundtrip_only_differences() {
        let defaults = AppSettings::default();
        let mut changed = defaults.clone();
        changed.quarantine.ttl_days = 14;
        let overrides = SettingsOverrides::between(&defaults, &changed);
        assert_eq!(overrides.quarantine.ttl_days, Some(14));
        assert!(overrides.scan.is_empty());
        assert_eq!(overrides.apply_to(defaults), changed);
    }

    #[test]
    fn validation_reports_field_path() {
        let mut settings = AppSettings::default();
        settings.quarantine.ttl_days = 0;
        let error = validate_settings(&settings).unwrap_err();
        assert_eq!(error.field, "quarantine.ttlDays");
    }

    /// T-152: старий конфіг без `enabled` = детектор увімкнено; false
    /// зберігається і читається; true не роздуває серіалізацію.
    #[test]
    fn detector_enabled_defaults_true_and_roundtrips_false() {
        let legacy: DetectorSettings =
            serde_json::from_str(r#"{"thresholds":{"minSizeBytes":1}}"#).unwrap();
        assert!(legacy.enabled);
        assert!(DetectorSettings::default().enabled);

        let disabled = DetectorSettings {
            enabled: false,
            ..DetectorSettings::default()
        };
        let json = serde_json::to_string(&disabled).unwrap();
        assert!(json.contains("\"enabled\":false"));
        let back: DetectorSettings = serde_json::from_str(&json).unwrap();
        assert!(!back.enabled);

        let enabled_json = serde_json::to_string(&DetectorSettings::default()).unwrap();
        assert!(!enabled_json.contains("enabled"));
    }
}
