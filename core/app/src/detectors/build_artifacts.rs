//! Розпізнавання build/dist/target/obj за маркерами екосистем (T-050) +
//! активність проєкту (T-052).
//!
//! DoD T-050: кожен патерн підтверджується **маркерним файлом**, а не лише
//! іменем папки (architecture.md §6.1 — структурні детектори).
//!
//! | Папка   | Маркери в parent |
//! |---------|------------------|
//! | target  | Cargo.toml (Rust) |
//! | dist    | package.json (JS) |
//! | build   | package.json, CMakeLists.txt, build.gradle(.kts), pom.xml |
//! | obj     | *.csproj, *.sln (.NET) |
//!
//! DoD T-052: свіжі джерела поза артефактами → ReviewRecommended.
//!
//! Без I/O: маркери й дати з індексу.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use super::contract::{Detector, DetectorId};
use super::folder_units::{
    build_folder_unit_from_acc, id_ns, sort_folder_units_by_size_desc, FolderUnitAcc,
    FolderUnitSpec,
};
use super::format::format_bytes_as_gb;
use super::node_modules::{file_name, normalize_path, parent_dir};
use super::project_activity::{append_activity_phrase, ProjectActivityIndex};
use super::thresholds::{self, keys, ThresholdValue};
use trashradar_domain::candidate::{CandidateUnit, Decision, FileRecord, SafetyLevel, Verdict};
use trashradar_domain::category::CategoryId;
use trashradar_domain::error::CoreError;

/// Стабільний id детектора.
pub const DETECTOR_ID: DetectorId = DetectorId::new("dev.build_artifacts");

/// Тип build-папки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuildFolderKind {
    Target,
    Dist,
    Build,
    Obj,
}

impl BuildFolderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BuildFolderKind::Target => "target",
            BuildFolderKind::Dist => "dist",
            BuildFolderKind::Build => "build",
            BuildFolderKind::Obj => "obj",
        }
    }

    fn from_segment(seg: &str) -> Option<Self> {
        match seg.to_ascii_lowercase().as_str() {
            "target" => Some(BuildFolderKind::Target),
            "dist" => Some(BuildFolderKind::Dist),
            "build" => Some(BuildFolderKind::Build),
            "obj" => Some(BuildFolderKind::Obj),
            _ => None,
        }
    }
}

/// Які маркери бачили в project root (parent build-папки).
#[derive(Debug, Clone, Copy, Default)]
struct MarkerFlags {
    package_json: bool,
    cargo_toml: bool,
    cmake_lists: bool,
    gradle: bool,
    pom_xml: bool,
    csproj_or_sln: bool,
}

impl MarkerFlags {
    fn confirms(self, kind: BuildFolderKind) -> bool {
        match kind {
            BuildFolderKind::Target => self.cargo_toml,
            BuildFolderKind::Dist => self.package_json,
            BuildFolderKind::Build => {
                self.package_json
                    || self.cmake_lists
                    || self.gradle
                    || self.pom_xml
                    || self.cargo_toml
            }
            BuildFolderKind::Obj => self.csproj_or_sln,
        }
    }

    fn merge(&mut self, other: MarkerFlags) {
        self.package_json |= other.package_json;
        self.cargo_toml |= other.cargo_toml;
        self.cmake_lists |= other.cmake_lists;
        self.gradle |= other.gradle;
        self.pom_xml |= other.pom_xml;
        self.csproj_or_sln |= other.csproj_or_sln;
    }
}

fn flags_from_file_name(name: &str) -> Option<MarkerFlags> {
    let lower = name.to_ascii_lowercase();
    let mut f = MarkerFlags::default();
    match lower.as_str() {
        "package.json" => f.package_json = true,
        "cargo.toml" => f.cargo_toml = true,
        "cmakelists.txt" => f.cmake_lists = true,
        "build.gradle" | "build.gradle.kts" | "settings.gradle" | "settings.gradle.kts" => {
            f.gradle = true
        }
        "pom.xml" => f.pom_xml = true,
        _ if lower.ends_with(".csproj")
            || lower.ends_with(".sln")
            || lower.ends_with(".fsproj") =>
        {
            f.csproj_or_sln = true
        }
        _ => return None,
    }
    Some(f)
}

/// Структурний детектор build/dist/target/obj → [`CategoryId::DevArtifacts`].
#[derive(Debug)]
pub struct BuildArtifactsDetector {
    enabled: AtomicBool,
    /// project root (parent of build folder) → markers present.
    markers: RwLock<HashMap<String, MarkerFlags>>,
    /// Активність за датами поза артефактами (T-052).
    activity: ProjectActivityIndex,
}

impl BuildArtifactsDetector {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            markers: RwLock::new(HashMap::new()),
            activity: ProjectActivityIndex::new(),
        }
    }

    pub fn from_index_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Self {
        let det = Self::new();
        det.ingest_paths(paths);
        det
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn with_now_filetime(self, now: i64) -> Self {
        self.activity.set_now_filetime(Some(now));
        self
    }

    pub fn set_now_filetime(&self, now: Option<i64>) {
        self.activity.set_now_filetime(now);
    }

    pub fn inactive_after_days(&self) -> u64 {
        self.activity.inactive_after_days()
    }

    pub fn activity_index(&self) -> &ProjectActivityIndex {
        &self.activity
    }

    pub fn ingest_paths<'a>(&self, paths: impl IntoIterator<Item = &'a str>) {
        let mut map = self.markers.write().expect("markers lock");
        for p in paths {
            if let Some((parent, flags)) = marker_from_path(p) {
                map.entry(parent).or_default().merge(flags);
            }
        }
    }

    pub fn clear_markers(&self) {
        self.markers.write().expect("markers lock").clear();
        self.activity.clear();
    }

    pub fn project_marker_count(&self) -> usize {
        self.markers.read().expect("markers lock").len()
    }

    /// Перерахувати активність для коренів із маркерами (T-052).
    pub fn rebuild_activity(&self, records: &[FileRecord]) {
        let roots: Vec<String> = self
            .markers
            .read()
            .expect("markers lock")
            .keys()
            .cloned()
            .collect();
        self.activity
            .rebuild_for_roots(roots.iter().map(|s| s.as_str()), records);
    }

    fn project_confirms(&self, project_root: &str, kind: BuildFolderKind) -> bool {
        let map = self.markers.read().expect("markers lock");
        map.get(&normalize_path(project_root))
            .is_some_and(|f| f.confirms(kind))
    }

    /// Outermost confirmed build-folder root for path.
    pub fn confirmed_root(&self, path: &str) -> Option<(String, BuildFolderKind)> {
        let (root, kind) = outermost_build_root(path)?;
        let project = parent_dir(&root)?;
        if self.project_confirms(&project, kind) {
            Some((root, kind))
        } else {
            None
        }
    }

    fn safety_and_phrase(&self, artifact_root: &str) -> (SafetyLevel, String) {
        let project = match parent_dir(artifact_root) {
            Some(p) => p,
            None => return (SafetyLevel::SafeToBulk, String::new()),
        };
        (
            self.activity.safety_for(&project),
            self.activity.explanation_phrase(&project),
        )
    }

    /// Згорнути файли в одиниці-папки (T-053). Оновлює активність (T-052).
    pub fn aggregate_units(&self, records: &[FileRecord]) -> Vec<FileRecord> {
        self.rebuild_activity(records);

        #[derive(Default)]
        struct Acc {
            unit: FolderUnitAcc,
            kind: Option<BuildFolderKind>,
        }
        let mut by_root: HashMap<String, Acc> = HashMap::new();

        for record in records {
            if record.decision == Decision::Keep || record.unit != CandidateUnit::File {
                continue;
            }
            let Some((root, kind)) = self.confirmed_root(&record.path) else {
                continue;
            };
            let acc = by_root.entry(root).or_default();
            acc.unit.add_file(record.size.0);
            acc.kind = Some(kind);
        }

        let mut units: Vec<FileRecord> = by_root
            .into_iter()
            .filter_map(|(path, acc)| {
                let kind = acc.kind?;
                let (safety, phrase) = self.safety_and_phrase(&path);
                let notes = if phrase.is_empty() {
                    vec![]
                } else {
                    vec![phrase]
                };
                build_folder_unit_from_acc(
                    FolderUnitSpec {
                        path,
                        label: kind.as_str().into(),
                        bytes: 0,
                        file_count: 0,
                        category: CategoryId::DevArtifacts,
                        safety,
                        detector_id: DETECTOR_ID,
                        notes,
                        id_namespace: id_ns::BUILD_ARTIFACTS,
                    },
                    acc.unit,
                )
            })
            .collect();
        sort_folder_units_by_size_desc(&mut units);
        units
    }
}

impl Default for BuildArtifactsDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector for BuildArtifactsDetector {
    fn id(&self) -> DetectorId {
        DETECTOR_ID
    }

    fn category(&self) -> CategoryId {
        CategoryId::DevArtifacts
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
        let (root, kind) = self.confirmed_root(&record.path)?;
        let (safety, phrase) = self.safety_and_phrase(&root);
        if record.unit == CandidateUnit::Folder {
            if normalize_path(&record.path) != normalize_path(&root) {
                return None;
            }
            let base = format!(
                "{} · {} (артефакт збірки)",
                kind.as_str(),
                format_bytes_as_gb(record.size.0)
            );
            return Some(Verdict::new(
                CategoryId::DevArtifacts,
                append_activity_phrase(&base, &phrase),
                safety,
            ));
        }
        if record.unit != CandidateUnit::File {
            return None;
        }
        let base = format!(
            "{} · {} · {}",
            kind.as_str(),
            format_bytes_as_gb(record.size.0),
            root
        );
        Some(Verdict::new(
            CategoryId::DevArtifacts,
            append_activity_phrase(&base, &phrase),
            safety,
        ))
    }

    fn set_threshold(&self, key: &str, value: ThresholdValue) -> Result<(), CoreError> {
        match key {
            keys::INACTIVE_AFTER_DAYS => {
                let days = value.as_u64().ok_or_else(|| {
                    thresholds::bad_threshold_type(DETECTOR_ID.as_str(), key, "u64")
                })?;
                self.activity.set_inactive_after_days(days);
                Ok(())
            }
            _ => Err(thresholds::unknown_threshold(DETECTOR_ID.as_str(), key)),
        }
    }

    fn get_threshold(&self, key: &str) -> Option<ThresholdValue> {
        match key {
            keys::INACTIVE_AFTER_DAYS => {
                Some(ThresholdValue::U64(self.activity.inactive_after_days()))
            }
            _ => None,
        }
    }
}

fn marker_from_path(path: &str) -> Option<(String, MarkerFlags)> {
    let name = file_name(path)?;
    let flags = flags_from_file_name(name)?;
    let parent = parent_dir(path)?;
    Some((parent, flags))
}

/// Outermost build/dist/target/obj root.
pub fn outermost_build_root(path: &str) -> Option<(String, BuildFolderKind)> {
    let norm = normalize_path(path);
    let parts: Vec<&str> = norm.split('\\').filter(|s| !s.is_empty()).collect();
    let mut prefix = String::new();
    let mut start = 0;
    if let Some(first) = parts.first() {
        if first.ends_with(':') {
            prefix = (*first).to_string();
            start = 1;
        }
    }
    for i in start..parts.len() {
        if let Some(kind) = BuildFolderKind::from_segment(parts[i]) {
            let mut root = prefix.clone();
            for p in &parts[start..=i] {
                if root.is_empty() || root.ends_with(':') {
                    if root.ends_with(':') {
                        root.push('\\');
                    }
                    root.push_str(p);
                } else {
                    root.push('\\');
                    root.push_str(p);
                }
            }
            return Some((root, kind));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::format::FILETIME_PER_DAY;
    use crate::detectors::{DetectorOrchestrator, DetectorRegistry};
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, FileAttributes, FileKind, FsTimestamp,
    };

    const NOW: i64 = 50_000 * FILETIME_PER_DAY;

    fn rec(id: u64, path: &str, size: u64) -> FileRecord {
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
    fn target_requires_cargo_toml() {
        let det = BuildArtifactsDetector::new();
        // alone — false positive
        assert!(det
            .evaluate(&rec(1, r"C:\code\app\target\release\app.exe", 1000))
            .is_none());
        det.ingest_paths([r"C:\code\app\Cargo.toml"]);
        let v = det
            .evaluate(&rec(1, r"C:\code\app\target\release\app.exe", 1000))
            .expect("cargo project");
        assert_eq!(v.category, CategoryId::DevArtifacts);
        assert!(v.explanation.contains("target"));
    }

    #[test]
    fn dist_requires_package_json() {
        let det = BuildArtifactsDetector::from_index_paths([r"C:\web\package.json"]);
        assert!(det
            .evaluate(&rec(1, r"C:\web\dist\index.js", 100))
            .is_some());
        assert!(det
            .evaluate(&rec(2, r"C:\other\dist\index.js", 100))
            .is_none());
    }

    #[test]
    fn build_accepts_cmake_or_gradle_or_package_json() {
        let cmake = BuildArtifactsDetector::from_index_paths([r"C:\cpp\CMakeLists.txt"]);
        assert!(cmake.evaluate(&rec(1, r"C:\cpp\build\lib.a", 50)).is_some());

        let gradle = BuildArtifactsDetector::from_index_paths([r"C:\android\build.gradle.kts"]);
        assert!(gradle
            .evaluate(&rec(1, r"C:\android\build\outputs\apk", 50))
            .is_some());

        let bare = BuildArtifactsDetector::new();
        assert!(bare
            .evaluate(&rec(1, r"C:\random\build\out.bin", 50))
            .is_none());
    }

    #[test]
    fn obj_requires_csproj_or_sln() {
        let det = BuildArtifactsDetector::from_index_paths([r"C:\dot\MyApp.csproj"]);
        assert!(det
            .evaluate(&rec(1, r"C:\dot\obj\Debug\net8.0\x.dll", 10))
            .is_some());
        let no = BuildArtifactsDetector::new();
        assert!(no
            .evaluate(&rec(1, r"C:\dot\obj\Debug\x.dll", 10))
            .is_none());
    }

    #[test]
    fn each_pattern_needs_its_marker_not_folder_name_alone() {
        // DoD: не лише ім'я папки.
        let det = BuildArtifactsDetector::new();
        for path in [
            r"C:\x\target\a",
            r"C:\x\dist\a",
            r"C:\x\build\a",
            r"C:\x\obj\a",
        ] {
            assert!(
                det.evaluate(&rec(1, path, 1)).is_none(),
                "false positive without marker: {path}"
            );
        }
    }

    #[test]
    fn outermost_build_root_helper() {
        let (root, kind) = outermost_build_root(r"C:\p\target\debug\deps\foo.rlib").expect("root");
        assert_eq!(kind, BuildFolderKind::Target);
        assert_eq!(root, r"c:\p\target");
    }

    #[test]
    fn aggregate_units_one_per_confirmed_root() {
        let det =
            BuildArtifactsDetector::from_index_paths([r"C:\rs\Cargo.toml", r"C:\js\package.json"]);
        let units = det.aggregate_units(&[
            rec(1, r"C:\rs\target\release\a.exe", 1000),
            rec(2, r"C:\rs\target\debug\a.pdb", 500),
            rec(3, r"C:\js\dist\main.js", 200),
            rec(4, r"C:\js\src\main.js", 50), // not artifact
        ]);
        assert_eq!(units.len(), 2);
        let target = units.iter().find(|u| u.path.ends_with("target")).unwrap();
        assert_eq!(target.size.0, 1500);
        assert_eq!(target.unit, CandidateUnit::Folder);
        assert!(target.explanation.contains("target"));
    }

    #[test]
    fn orchestrator_register() {
        let det = BuildArtifactsDetector::from_index_paths([r"D:\app\Cargo.toml"]);
        let mut reg = DetectorRegistry::new();
        reg.register(det);
        let orch = DetectorOrchestrator::new(&reg);
        let out = orch.categorize_batch(&[
            rec(1, r"D:\app\target\release\app.exe", 10),
            rec(2, r"D:\app\src\main.rs", 10),
        ]);
        assert_eq!(out.stats.records_updated, 1);
        assert_eq!(out.updated[0].category, CategoryId::DevArtifacts);
    }

    fn rec_dated(id: u64, path: &str, size: u64, days_ago: u64) -> FileRecord {
        let mut r = rec(id, path, size);
        r.modified_at = Some(FsTimestamp(NOW - (days_ago as i64) * FILETIME_PER_DAY));
        r
    }

    #[test]
    fn active_rust_project_target_not_safe_to_bulk() {
        let det =
            BuildArtifactsDetector::from_index_paths([r"C:\rs\Cargo.toml"]).with_now_filetime(NOW);
        let records = [
            rec_dated(1, r"C:\rs\src\main.rs", 100, 2),
            rec_dated(2, r"C:\rs\Cargo.toml", 10, 2),
            rec_dated(3, r"C:\rs\target\release\app.exe", 50_000, 1),
        ];
        det.rebuild_activity(&records);
        let v = det.evaluate(&records[2]).expect("target");
        assert_eq!(v.safety, SafetyLevel::ReviewRecommended);
        assert!(v.explanation.contains("активний"), "{}", v.explanation);
    }
}
