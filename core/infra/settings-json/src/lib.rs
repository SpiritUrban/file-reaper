//! Людиночитаний settings.json: вбудовані дефолти + файл відхилень (T-090).

use std::fs;
use std::path::{Path, PathBuf};

use trashradar_app::ports::SettingsSource;
use trashradar_domain::error::CoreError;
use trashradar_domain::settings::{AppSettings, SettingsOverrides};

pub const SETTINGS_FILE_NAME: &str = "settings.json";

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
        let overrides: SettingsOverrides = serde_json::from_str(&text).map_err(|error| {
            CoreError::invalid_argument(format!("Некоректний settings.json: {error}"))
        })?;
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
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                CoreError::io(format!("Не вдалося створити профіль settings: {error}"))
            })?;
        }
        let text = serde_json::to_string_pretty(&overrides).map_err(|error| {
            CoreError::internal(format!("Не вдалося серіалізувати settings: {error}"))
        })?;
        fs::write(&self.path, format!("{text}\n"))
            .map_err(|error| CoreError::io(format!("Не вдалося записати settings.json: {error}")))
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
        assert!(text.contains("ttlDays"));
        assert!(!text.contains("warningThresholdBytes"));
        assert!(!text.contains("minimumSizeBytes"));
        assert_eq!(source.load().unwrap(), settings);

        source.save(&AppSettings::default()).unwrap();
        assert!(!source.path().exists());
        assert_eq!(source.load().unwrap(), AppSettings::default());
        let _ = fs::remove_dir_all(profile);
    }
}
