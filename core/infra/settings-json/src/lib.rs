//! Людиночитаний settings.json: вбудовані дефолти + файл відхилень (T-090).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trashradar_app::ports::SettingsSource;
use trashradar_domain::error::CoreError;
use trashradar_domain::settings::{AppSettings, SettingsOverrides};

pub const SETTINGS_FILE_NAME: &str = "settings.json";
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsDocument {
    schema_version: u32,
    settings: SettingsOverrides,
}

#[derive(Debug, Clone)]
pub struct JsonSettingsSource {
    path: PathBuf,
}

impl JsonSettingsSource {
    pub fn in_profile(profile_dir: impl AsRef<Path>) -> Self {
        Self {
            path: profile_dir.as_ref().join(SETTINGS_FILE_NAME),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn parse_and_migrate(&self, text: &str) -> Result<SettingsOverrides, CoreError> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(invalid_json)?;
        match value.get("schemaVersion") {
            None => {
                let overrides = serde_json::from_value(value).map_err(invalid_json)?;
                self.write_overrides(&overrides)?;
                Ok(overrides)
            }
            Some(version) => {
                let version = version.as_u64().ok_or_else(|| {
                    CoreError::invalid_argument("settings.schemaVersion має бути цілим числом.")
                })?;
                if version != u64::from(CURRENT_SCHEMA_VERSION) {
                    return Err(CoreError::invalid_argument(format!(
                        "Непідтримувана версія settings schema: {version}."
                    )));
                }
                let document: SettingsDocument =
                    serde_json::from_value(value).map_err(invalid_json)?;
                Ok(document.settings)
            }
        }
    }

    fn write_overrides(&self, overrides: &SettingsOverrides) -> Result<(), CoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                CoreError::io(format!("Не вдалося створити профіль settings: {error}"))
            })?;
        }
        let document = SettingsDocument {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: overrides.clone(),
        };
        let text = serde_json::to_string_pretty(&document).map_err(|error| {
            CoreError::internal(format!("Не вдалося серіалізувати settings: {error}"))
        })?;
        fs::write(&self.path, format!("{text}\n"))
            .map_err(|error| CoreError::io(format!("Не вдалося записати settings.json: {error}")))
    }
}

fn invalid_json(error: serde_json::Error) -> CoreError {
    CoreError::invalid_argument(format!("Некоректний settings.json: {error}"))
}

impl SettingsSource for JsonSettingsSource {
    fn load(&self) -> Result<AppSettings, CoreError> {
        let defaults = AppSettings::default();
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(defaults),
            Err(error) => {
                return Err(CoreError::io(format!(
                    "Не вдалося прочитати settings.json: {error}"
                )))
            }
        };
        let overrides = self.parse_and_migrate(&text)?;
        Ok(overrides.apply_to(defaults))
    }

    fn save(&self, settings: &AppSettings) -> Result<(), CoreError> {
        let overrides = SettingsOverrides::between(&AppSettings::default(), settings);
        if overrides.is_empty() {
            match fs::remove_file(&self.path) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => {
                    return Err(CoreError::io(format!(
                        "Не вдалося видалити порожній settings.json: {error}"
                    )))
                }
            }
        }
        self.write_overrides(&overrides)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn profile(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("trashradar-settings-{name}-{nonce}"))
    }

    #[test]
    fn missing_file_is_clean_defaults() {
        let source = JsonSettingsSource::in_profile(profile("missing"));
        assert_eq!(source.load().unwrap(), AppSettings::default());
    }

    #[test]
    fn file_contains_only_overrides_and_delete_resets_defaults() {
        let profile = profile("overrides");
        let source = JsonSettingsSource::in_profile(&profile);
        let mut settings = AppSettings::default();
        settings.quarantine.ttl_days = 14;
        source.save(&settings).unwrap();
        let text = fs::read_to_string(source.path()).unwrap();
        assert!(text.contains("schemaVersion"));
        assert!(text.contains("ttlDays"));
        assert!(!text.contains("warningThresholdBytes"));
        assert!(!text.contains("minimumSizeBytes"));
        assert_eq!(source.load().unwrap(), settings);

        source.save(&AppSettings::default()).unwrap();
        assert!(!source.path().exists());
        assert_eq!(source.load().unwrap(), AppSettings::default());
        let _ = fs::remove_dir_all(profile);
    }

    #[test]
    fn migrates_unversioned_v0_without_losing_valid_values() {
        let profile = profile("migrate-v0");
        let source = JsonSettingsSource::in_profile(&profile);
        fs::create_dir_all(&profile).unwrap();
        fs::write(
            source.path(),
            r#"{"quarantine":{"ttlDays":7},"scan":{"minimumSizeBytes":4096}}"#,
        )
        .unwrap();

        let loaded = source.load().unwrap();
        assert_eq!(loaded.quarantine.ttl_days, 7);
        assert_eq!(loaded.scan.minimum_size_bytes, 4096);
        let migrated = fs::read_to_string(source.path()).unwrap();
        let document: serde_json::Value = serde_json::from_str(&migrated).unwrap();
        assert_eq!(document["schemaVersion"], 1);
        assert_eq!(document["settings"]["quarantine"]["ttlDays"], 7);
        assert_eq!(document["settings"]["scan"]["minimumSizeBytes"], 4096);
        let _ = fs::remove_dir_all(profile);
    }

    #[test]
    fn rejects_future_schema_without_rewriting_file() {
        use trashradar_domain::error::ErrorCode;
        let profile = profile("future");
        let source = JsonSettingsSource::in_profile(&profile);
        fs::create_dir_all(&profile).unwrap();
        let future = r#"{"schemaVersion":99,"settings":{"quarantine":{"ttlDays":9}}}"#;
        fs::write(source.path(), future).unwrap();
        let error = source.load().unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(fs::read_to_string(source.path()).unwrap(), future);
        let _ = fs::remove_dir_all(profile);
    }
}
