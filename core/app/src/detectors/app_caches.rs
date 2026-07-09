//! Детектор «Кеші програм» (T-048).
//!
//! Локаційний детектор на `kind: app_caches` з
//! [`crate::location_registry::KnownLocationsRegistry`] (T-044/T-046).
//!
//! DoD: **кеш-каталоги** — одиниці [`CandidateUnit::Folder`] із
//! **сумарним розміром** (і кількістю файлів у поясненні).
//!
//! - [`AppCachesDetector::evaluate`] — файли під коренем (потік скану);
//! - [`AppCachesDetector::aggregate_units`] — згортка файлів у папки-одиниці.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use super::contract::{Detector, DetectorId};
use super::folder_units::{
    build_folder_unit_from_acc, id_ns, sort_folder_units_by_size_desc, FolderUnitAcc,
    FolderUnitSpec,
};
use super::format::format_bytes_as_gb;
use crate::location_registry::{path_matches_prefix, KnownLocationsRegistry, LocationKind};
use trashradar_domain::candidate::{CandidateUnit, Decision, FileRecord, SafetyLevel, Verdict};
use trashradar_domain::category::CategoryId;

/// Стабільний id детектора.
pub const DETECTOR_ID: DetectorId = DetectorId::new("app_caches");

#[derive(Debug, Clone)]
struct CacheRoot {
    path: String,
    location_id: String,
    label: String,
    explanation: String,
    safety: SafetyLevel,
}

/// Локаційний детектор [`CategoryId::AppCaches`].
#[derive(Debug)]
pub struct AppCachesDetector {
    enabled: AtomicBool,
    roots: RwLock<Vec<CacheRoot>>,
}

impl AppCachesDetector {
    pub fn from_registry(registry: &KnownLocationsRegistry) -> Self {
        let det = Self {
            enabled: AtomicBool::new(true),
            roots: RwLock::new(Vec::new()),
        };
        det.rebuild_from_registry(registry);
        det
    }

    pub fn empty() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            roots: RwLock::new(Vec::new()),
        }
    }

    pub fn rebuild_from_registry(&self, registry: &KnownLocationsRegistry) {
        let mut roots = Vec::new();
        for entry in registry.by_kind(LocationKind::AppCaches) {
            let label = entry.label.clone().unwrap_or_else(|| entry.id.clone());
            for expanded in entry.expanded_roots() {
                roots.push(CacheRoot {
                    path: expanded,
                    location_id: entry.id.clone(),
                    label: label.clone(),
                    explanation: entry.explanation.clone(),
                    safety: entry.safety,
                });
            }
        }
        roots.sort_by_key(|r| std::cmp::Reverse(r.path.len()));
        *self.roots.write().expect("cache roots lock") = roots;
    }

    /// Явні корені для тестів: (path, location_id, label, safety).
    pub fn from_explicit_roots(
        roots: impl IntoIterator<Item = (String, String, String, SafetyLevel)>,
    ) -> Self {
        let mut list: Vec<CacheRoot> = roots
            .into_iter()
            .map(|(path, location_id, label, safety)| CacheRoot {
                path,
                location_id,
                label,
                explanation: String::new(),
                safety,
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
        self.roots.read().expect("cache roots lock").len()
    }

    fn find_match(&self, candidate_path: &str) -> Option<CacheRoot> {
        let roots = self.roots.read().expect("cache roots lock");
        roots
            .iter()
            .find(|r| path_matches_prefix(candidate_path, &r.path))
            .cloned()
    }

    /// Згорнути файли індексу в **папки-одиниці** кешу (DoD T-048 / T-053).
    ///
    /// Для кожного кореня реєстру, під яким є ≥1 файл (не Keep), будує
    /// [`FileRecord`] з `unit = Folder`, `size = Σ size`, поясненням
    /// «Label · N ГБ · M файлів» — **одна** одиниця позначення.
    pub fn aggregate_units(&self, records: &[FileRecord]) -> Vec<FileRecord> {
        #[derive(Default)]
        struct Acc {
            unit: FolderUnitAcc,
            root: Option<CacheRoot>,
        }

        let mut by_root: HashMap<String, Acc> = HashMap::new();

        for record in records {
            if record.decision == Decision::Keep {
                continue;
            }
            if record.unit != CandidateUnit::File {
                continue;
            }
            let Some(root) = self.find_match(&record.path) else {
                continue;
            };
            let key = root.path.clone();
            let acc = by_root.entry(key).or_default();
            acc.unit.add_file(record.size.0);
            if acc.root.is_none() {
                acc.root = Some(root);
            }
        }

        let mut units: Vec<FileRecord> = by_root
            .into_values()
            .filter_map(|acc| {
                let root = acc.root?;
                let label = if root.label.is_empty() {
                    root.location_id.clone()
                } else {
                    root.label.clone()
                };
                build_folder_unit_from_acc(
                    FolderUnitSpec {
                        path: root.path,
                        label,
                        bytes: 0,
                        file_count: 0,
                        category: CategoryId::AppCaches,
                        safety: root.safety,
                        detector_id: DETECTOR_ID,
                        notes: vec![],
                        id_namespace: id_ns::APP_CACHES,
                    },
                    acc.unit,
                )
            })
            .collect();

        sort_folder_units_by_size_desc(&mut units);
        units
    }
}

fn paths_equal_norm(a: &str, b: &str) -> bool {
    path_matches_prefix(a, b) && path_matches_prefix(b, a)
}

impl Default for AppCachesDetector {
    fn default() -> Self {
        Self::empty()
    }
}

impl Detector for AppCachesDetector {
    fn id(&self) -> DetectorId {
        DETECTOR_ID
    }

    fn category(&self) -> CategoryId {
        CategoryId::AppCaches
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
        let hit = self.find_match(&record.path)?;

        match record.unit {
            CandidateUnit::File => {
                let expl = if hit.explanation.is_empty() {
                    format!("кеш {} · {}", hit.label, format_bytes_as_gb(record.size.0))
                } else {
                    format!(
                        "{} · {}",
                        hit.explanation,
                        format_bytes_as_gb(record.size.0)
                    )
                };
                Some(Verdict::new(CategoryId::AppCaches, expl, hit.safety))
            }
            CandidateUnit::Folder => {
                // Лише сама тека-корінь кешу (одиниця), не вкладені папки.
                if !paths_equal_norm(&record.path, &hit.path) {
                    return None;
                }
                let label = if hit.label.is_empty() {
                    hit.location_id.as_str()
                } else {
                    hit.label.as_str()
                };
                let expl = format!("{} · {}", label, format_bytes_as_gb(record.size.0));
                Some(Verdict::new(CategoryId::AppCaches, expl, hit.safety))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::{DetectorOrchestrator, DetectorRegistry};
    use crate::location_registry::KnownLocationsRegistry;
    use trashradar_domain::candidate::{ByteSize, CandidateId, FileAttributes, FileKind};

    const MIB: u64 = 1024 * 1024;

    fn file(id: u64, path: &str, size: u64) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: path.into(),
            size: ByteSize(size),
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
    fn aggregate_units_sums_size_and_count() {
        let det = AppCachesDetector::from_explicit_roots([(
            r"C:\Users\Ada\AppData\Local\Google\Chrome\User Data\Default\Cache".into(),
            "browser.chrome.cache".into(),
            "Chrome Cache".into(),
            SafetyLevel::SafeToBulk,
        )]);

        let records = [
            file(
                1,
                r"C:\Users\Ada\AppData\Local\Google\Chrome\User Data\Default\Cache\f1",
                100 * MIB,
            ),
            file(
                2,
                r"C:\Users\Ada\AppData\Local\Google\Chrome\User Data\Default\Cache\f2",
                50 * MIB,
            ),
            file(3, r"C:\Users\Ada\Documents\keep.bin", 999 * MIB),
        ];

        let units = det.aggregate_units(&records);
        assert_eq!(units.len(), 1);
        let u = &units[0];
        assert_eq!(u.unit, CandidateUnit::Folder);
        assert_eq!(u.category, CategoryId::AppCaches);
        assert_eq!(u.safety, SafetyLevel::SafeToBulk);
        assert_eq!(u.size.0, 150 * MIB);
        assert!(u.explanation.contains("Chrome Cache"), "{}", u.explanation);
        assert!(u.explanation.contains("ГБ"), "{}", u.explanation);
        assert!(u.explanation.contains("2 файлів"), "{}", u.explanation);
        assert_eq!(
            u.path,
            r"C:\Users\Ada\AppData\Local\Google\Chrome\User Data\Default\Cache"
        );
    }

    #[test]
    fn aggregate_skips_keep_and_groups_by_root() {
        let det = AppCachesDetector::from_explicit_roots([
            (
                r"C:\CacheA".into(),
                "a".into(),
                "A".into(),
                SafetyLevel::SafeToBulk,
            ),
            (
                r"C:\CacheB".into(),
                "b".into(),
                "B".into(),
                SafetyLevel::ReviewRecommended,
            ),
        ]);
        let mut kept = file(1, r"C:\CacheA\x", 10 * MIB);
        kept.decision = Decision::Keep;
        let records = [
            kept,
            file(2, r"C:\CacheA\y", 20 * MIB),
            file(3, r"C:\CacheB\z", 5 * MIB),
        ];
        let units = det.aggregate_units(&records);
        assert_eq!(units.len(), 2);
        let a = units.iter().find(|u| u.path == r"C:\CacheA").unwrap();
        assert_eq!(a.size.0, 20 * MIB);
        assert_eq!(a.safety, SafetyLevel::SafeToBulk);
        let b = units.iter().find(|u| u.path == r"C:\CacheB").unwrap();
        assert_eq!(b.size.0, 5 * MIB);
        assert_eq!(b.safety, SafetyLevel::ReviewRecommended);
        assert!(units[0].size.0 >= units[1].size.0);
    }

    #[test]
    fn evaluate_files_under_cache_root() {
        let det = AppCachesDetector::from_explicit_roots([(
            r"C:\npm-cache".into(),
            "pkg.npm.cache".into(),
            "npm cache".into(),
            SafetyLevel::SafeToBulk,
        )]);
        let v = det
            .evaluate(&file(1, r"C:\npm-cache\content-v2\ab", MIB))
            .expect("match");
        assert_eq!(v.category, CategoryId::AppCaches);
        assert_eq!(v.safety, SafetyLevel::SafeToBulk);
    }

    #[test]
    fn evaluate_folder_unit_only_at_root() {
        let det = AppCachesDetector::from_explicit_roots([(
            r"C:\npm-cache".into(),
            "pkg.npm.cache".into(),
            "npm cache".into(),
            SafetyLevel::SafeToBulk,
        )]);
        let mut root = file(1, r"C:\npm-cache", 100 * MIB);
        root.unit = CandidateUnit::Folder;
        assert!(det.evaluate(&root).is_some());

        let mut nested = file(2, r"C:\npm-cache\sub", 10 * MIB);
        nested.unit = CandidateUnit::Folder;
        assert!(det.evaluate(&nested).is_none());
    }

    #[test]
    fn temp_kind_not_matched() {
        let json = r#"{
          "schema_version": 1,
          "locations": [
            {
              "id": "windows.temp.user",
              "kind": "temp_files",
              "safety": "safe_to_bulk",
              "paths": ["C:\\Temp"],
              "explanation": "temp"
            },
            {
              "id": "browser.chrome.cache",
              "kind": "app_caches",
              "safety": "safe_to_bulk",
              "paths": ["C:\\ChromeCache"],
              "label": "Chrome Cache",
              "explanation": "chrome"
            }
          ]
        }"#;
        let reg = KnownLocationsRegistry::from_json_str(json).unwrap();
        let det = AppCachesDetector::from_registry(&reg);
        assert_eq!(det.root_count(), 1);
        assert!(det.evaluate(&file(1, r"C:\Temp\x", 1)).is_none());
        assert!(det.evaluate(&file(2, r"C:\ChromeCache\x", 1)).is_some());
    }

    #[test]
    fn orchestrator_and_units() {
        let det = AppCachesDetector::from_explicit_roots([(
            r"D:\Caches\App".into(),
            "app".into(),
            "App".into(),
            SafetyLevel::SafeToBulk,
        )]);
        let mut reg = DetectorRegistry::new();
        reg.register(det);
        let det2 = AppCachesDetector::from_explicit_roots([(
            r"D:\Caches\App".into(),
            "app".into(),
            "App".into(),
            SafetyLevel::SafeToBulk,
        )]);
        let orch = DetectorOrchestrator::new(&reg);
        let batch = [
            file(1, r"D:\Caches\App\a", 10 * MIB),
            file(2, r"D:\Other\b", 99 * MIB),
        ];
        let out = orch.categorize_batch(&batch);
        assert_eq!(out.stats.records_updated, 1);

        let units = det2.aggregate_units(&batch);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].size.0, 10 * MIB);
        assert_eq!(units[0].unit, CandidateUnit::Folder);
    }

    #[test]
    fn default_id_category() {
        let det = AppCachesDetector::empty();
        assert_eq!(det.id(), DETECTOR_ID);
        assert_eq!(det.category(), CategoryId::AppCaches);
    }
}
