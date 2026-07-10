//! Конфігурація Core: ефективні значення та optional overrides (T-090).

use serde::{Deserialize, Serialize};

pub const DEFAULT_QUARANTINE_TTL_DAYS: u32 = 30;
pub const DEFAULT_QUARANTINE_WARNING_BYTES: u64 = 50 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub quarantine: QuarantineSettings,
    pub scan: ScanSettings,
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

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSettings {
    pub excluded_paths: Vec<String>,
    pub minimum_size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsOverrides {
    #[serde(default, skip_serializing_if = "QuarantineOverrides::is_empty")]
    pub quarantine: QuarantineOverrides,
    #[serde(default, skip_serializing_if = "ScanOverrides::is_empty")]
    pub scan: ScanOverrides,
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
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        self.quarantine.is_empty() && self.scan.is_empty()
    }
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
}

impl ScanOverrides {
    pub fn is_empty(&self) -> bool {
        self.excluded_paths.is_none() && self.minimum_size_bytes.is_none()
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
}
