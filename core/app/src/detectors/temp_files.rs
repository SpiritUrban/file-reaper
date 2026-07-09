//! Детектор «Тимчасові файли» (T-047).
//!
//! Локаційний детектор: файли під коренями `kind: temp_files` з
//! [`crate::location_registry::KnownLocationsRegistry`] (T-044/T-045).
//!
//! DoD: кандидати з Temp-локацій → [`CategoryId::TempFiles`] з
//! [`SafetyLevel::SafeToBulk`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use super::contract::{Detector, DetectorId};
use crate::location_registry::{path_matches_prefix, KnownLocationsRegistry, LocationKind};
use trashradar_domain::candidate::{CandidateUnit, FileRecord, SafetyLevel, Verdict};
use trashradar_domain::category::CategoryId;

/// Стабільний id детектора.
pub const DETECTOR_ID: DetectorId = DetectorId::new("temp_files");

/// Один розгорнутий корінь temp-локації для швидкого prefix-match.
#[derive(Debug, Clone)]
struct TempRoot {
    /// Абсолютний шлях (як після expand).
    path: String,
    /// Пояснення з реєстру.
    explanation: String,
    location_id: String,
}

/// Локаційний детектор [`CategoryId::TempFiles`].
#[derive(Debug)]
pub struct TempFilesDetector {
    enabled: AtomicBool,
    roots: RwLock<Vec<TempRoot>>,
}

impl TempFilesDetector {
    /// Побудувати з завантаженого реєстру (лише `temp_files`).
    pub fn from_registry(registry: &KnownLocationsRegistry) -> Self {
        let det = Self {
            enabled: AtomicBool::new(true),
            roots: RwLock::new(Vec::new()),
        };
        det.rebuild_from_registry(registry);
        det
    }

    /// Порожній детектор (для тестів / до завантаження реєстру).
    pub fn empty() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            roots: RwLock::new(Vec::new()),
        }
    }

    /// Перечитати корені з реєстру (після hot-reload JSON).
    pub fn rebuild_from_registry(&self, registry: &KnownLocationsRegistry) {
        let mut roots = Vec::new();
        for entry in registry.by_kind(LocationKind::TempFiles) {
            for expanded in entry.expanded_roots() {
                roots.push(TempRoot {
                    path: expanded,
                    explanation: entry.explanation.clone(),
                    location_id: entry.id.clone(),
                });
            }
        }
        // Довші (специфічніші) префікси — першими (майбутні вкладені temp).
        roots.sort_by_key(|r| std::cmp::Reverse(r.path.len()));
        *self.roots.write().expect("temp roots lock") = roots;
    }

    /// Явні корені для unit-тестів (без env expand).
    pub fn from_explicit_roots(roots: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut list: Vec<TempRoot> = roots
            .into_iter()
            .map(|(path, explanation)| TempRoot {
                path,
                explanation,
                location_id: "test".into(),
            })
            .collect();
        list.sort_by_key(|r| std::cmp::Reverse(r.path.len()));
        Self {
            enabled: AtomicBool::new(true),
            roots: RwLock::new(list),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn root_count(&self) -> usize {
        self.roots.read().expect("temp roots lock").len()
    }

    fn find_match(&self, candidate_path: &str) -> Option<TempRoot> {
        let roots = self.roots.read().expect("temp roots lock");
        roots
            .iter()
            .find(|r| path_matches_prefix(candidate_path, &r.path))
            .cloned()
    }
}

impl Default for TempFilesDetector {
    fn default() -> Self {
        Self::empty()
    }
}

impl Detector for TempFilesDetector {
    fn id(&self) -> DetectorId {
        DETECTOR_ID
    }

    fn category(&self) -> CategoryId {
        CategoryId::TempFiles
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
        if record.unit != CandidateUnit::File {
            return None;
        }
        let hit = self.find_match(&record.path)?;
        // DoD T-047: завжди safe_to_bulk для temp (навіть якщо запис у JSON інший).
        Some(Verdict::new(
            CategoryId::TempFiles,
            format!("{} ({})", hit.explanation, hit.location_id),
            SafetyLevel::SafeToBulk,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::{DetectorOrchestrator, DetectorRegistry};
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
    };

    fn file(id: u64, path: &str) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: path.into(),
            size: ByteSize(4096),
            created_at: None,
            modified_at: None,
            accessed_at: None,
            kind: FileKind::Other,
            unit: CandidateUnit::File,
            category: CategoryId::Uncategorized,
            safety: SafetyLevel::ReviewRecommended,
            decision: Decision::Undecided,
            detector_id: String::new(),
            explanation: String::new(),
            attributes: FileAttributes::default(),
        }
    }

    #[test]
    fn files_under_temp_root_are_safe_to_bulk() {
        let det = TempFilesDetector::from_explicit_roots([(
            r"C:\Users\Ada\AppData\Local\Temp".into(),
            "тимчасові файли користувача".into(),
        )]);

        let v = det
            .evaluate(&file(1, r"C:\Users\Ada\AppData\Local\Temp\foo.tmp"))
            .expect("temp file");
        assert_eq!(v.category, CategoryId::TempFiles);
        assert_eq!(v.safety, SafetyLevel::SafeToBulk);
        assert!(v.explanation.contains("тимчасові"), "{}", v.explanation);
        assert!(v.is_complete());
    }

    #[test]
    fn outside_temp_not_matched() {
        let det = TempFilesDetector::from_explicit_roots([(
            r"C:\Users\Ada\AppData\Local\Temp".into(),
            "temp".into(),
        )]);
        assert!(det
            .evaluate(&file(1, r"C:\Users\Ada\Documents\report.pdf"))
            .is_none());
        // Temp2 — не префікс сегмента
        assert!(det
            .evaluate(&file(2, r"C:\Users\Ada\AppData\Local\Temp2\x.tmp"))
            .is_none());
    }

    #[test]
    fn case_insensitive_windows_paths() {
        let det = TempFilesDetector::from_explicit_roots([(
            r"C:\Users\Ada\AppData\Local\Temp".into(),
            "temp".into(),
        )]);
        assert!(det
            .evaluate(&file(1, r"c:\users\ada\appdata\local\temp\X.TMP"))
            .is_some());
    }

    #[test]
    fn folder_units_ignored() {
        let det = TempFilesDetector::from_explicit_roots([(r"C:\Temp".into(), "temp".into())]);
        let mut r = file(1, r"C:\Temp\sub");
        r.unit = CandidateUnit::Folder;
        assert!(det.evaluate(&r).is_none());
    }

    #[test]
    fn from_registry_json_temp_kind_only() {
        let json = r#"{
          "schema_version": 1,
          "locations": [
            {
              "id": "windows.temp.user",
              "kind": "temp_files",
              "safety": "safe_to_bulk",
              "paths": ["C:\\Scratch\\UserTemp"],
              "explanation": "user temp"
            },
            {
              "id": "browser.chrome.cache",
              "kind": "app_caches",
              "safety": "safe_to_bulk",
              "paths": ["C:\\Scratch\\ChromeCache"],
              "explanation": "chrome"
            }
          ]
        }"#;
        let reg = KnownLocationsRegistry::from_json_str(json).unwrap();
        let det = TempFilesDetector::from_registry(&reg);
        assert_eq!(det.root_count(), 1);

        assert!(det
            .evaluate(&file(1, r"C:\Scratch\UserTemp\a.tmp"))
            .is_some());
        // app_caches не підхоплюються цим детектором
        assert!(det
            .evaluate(&file(2, r"C:\Scratch\ChromeCache\x"))
            .is_none());
    }

    #[test]
    fn forces_safe_to_bulk_even_if_registry_said_review() {
        let json = r#"{
          "schema_version": 1,
          "locations": [{
            "id": "odd.temp",
            "kind": "temp_files",
            "safety": "review_recommended",
            "paths": ["D:\\Tmp"],
            "explanation": "odd"
          }]
        }"#;
        let reg = KnownLocationsRegistry::from_json_str(json).unwrap();
        let det = TempFilesDetector::from_registry(&reg);
        let v = det.evaluate(&file(1, r"D:\Tmp\f.bin")).unwrap();
        assert_eq!(v.safety, SafetyLevel::SafeToBulk);
    }

    #[test]
    fn orchestrator_integration() {
        let det = TempFilesDetector::from_explicit_roots([
            (r"C:\Windows\Temp".into(), "system temp".into()),
            (r"C:\Users\X\AppData\Local\Temp".into(), "user temp".into()),
        ]);
        let mut reg = DetectorRegistry::new();
        reg.register(det);
        let orch = DetectorOrchestrator::new(&reg);
        let batch = [
            file(1, r"C:\Windows\Temp\setup.log"),
            file(2, r"C:\Users\X\AppData\Local\Temp\x.dat"),
            file(3, r"C:\Users\X\Desktop\keep.txt"),
        ];
        let out = orch.categorize_batch(&batch);
        assert_eq!(out.stats.records_updated, 2);
        assert!(out
            .updated
            .iter()
            .all(|r| r.category == CategoryId::TempFiles && r.safety == SafetyLevel::SafeToBulk));
    }

    #[test]
    fn default_id_and_category() {
        let det = TempFilesDetector::empty();
        assert_eq!(det.id(), DETECTOR_ID);
        assert_eq!(det.category(), CategoryId::TempFiles);
    }
}
