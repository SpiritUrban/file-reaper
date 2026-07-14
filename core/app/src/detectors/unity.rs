//! Розпізнавання Unity Library / Temp / Obj (T-051) + активність проєкту (T-052).
//!
//! Unity-проєкт визначається за структурою (маркери в індексі, без I/O):
//! - `ProjectSettings/ProjectVersion.txt` (канонічний),
//! - або будь-який файл під `ProjectSettings\`,
//! - або `Assets\` **і** `ProjectSettings\` під одним коренем (ingest обох).
//!
//! Артефакти в **корені проєкту** (перестворювані):
//! - `Library/` — головний кеш імпорту;
//! - `Temp/` — тимчасові файли редактора;
//! - `Obj/` — проміжні (якщо є).
//!
//! DoD T-051: проєкт за структурою; Library позначена як перестворювана.
//! DoD T-052: свіжі джерела (Assets/ тощо) → ReviewRecommended.

use std::collections::{HashMap, HashSet};
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
pub const DETECTOR_ID: DetectorId = DetectorId::new("dev.unity");

/// Тип Unity-артефактної теки в корені проєкту.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnityFolderKind {
    Library,
    Temp,
    Obj,
}

impl UnityFolderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UnityFolderKind::Library => "Library",
            UnityFolderKind::Temp => "Temp",
            UnityFolderKind::Obj => "Obj",
        }
    }

    fn from_segment(seg: &str) -> Option<Self> {
        match seg.to_ascii_lowercase().as_str() {
            "library" => Some(UnityFolderKind::Library),
            "temp" => Some(UnityFolderKind::Temp),
            "obj" => Some(UnityFolderKind::Obj),
            _ => None,
        }
    }

    fn regenerable_note(self) -> &'static str {
        match self {
            UnityFolderKind::Library => "перестворюється при відкритті проєкту",
            UnityFolderKind::Temp => "тимчасові файли редактора Unity",
            UnityFolderKind::Obj => "проміжні файли Unity",
        }
    }
}

/// Детектор Unity Library/Temp/Obj → [`CategoryId::DevArtifacts`].
#[derive(Debug)]
pub struct UnityArtifactsDetector {
    enabled: AtomicBool,
    /// Нормалізовані корені Unity-проєктів.
    project_roots: RwLock<HashSet<String>>,
    /// Підказки для евристики Assets+ProjectSettings.
    saw_assets_parent: RwLock<HashSet<String>>,
    saw_project_settings_parent: RwLock<HashSet<String>>,
    /// Активність за датами поза артефактами (T-052).
    activity: ProjectActivityIndex,
}

impl UnityArtifactsDetector {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            project_roots: RwLock::new(HashSet::new()),
            saw_assets_parent: RwLock::new(HashSet::new()),
            saw_project_settings_parent: RwLock::new(HashSet::new()),
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

    pub fn project_count(&self) -> usize {
        self.project_roots.read().expect("lock").len()
    }

    /// Перерахувати активність Unity-коренів (T-052).
    pub fn rebuild_activity(&self, records: &[FileRecord]) {
        let roots: Vec<String> = self
            .project_roots
            .read()
            .expect("lock")
            .iter()
            .cloned()
            .collect();
        self.activity
            .rebuild_for_roots(roots.iter().map(|s| s.as_str()), records);
    }

    /// Оновити маркери з шляхів індексу / корпусу.
    pub fn ingest_paths<'a>(&self, paths: impl IntoIterator<Item = &'a str>) {
        for p in paths {
            self.ingest_one(p);
        }
        self.reconcile_assets_project_settings();
    }

    fn ingest_one(&self, path: &str) {
        let norm = normalize_path(path);
        // Канонічний маркер
        if let Some(root) = project_root_from_project_version(&norm) {
            self.project_roots.write().expect("lock").insert(root);
            return;
        }
        // Будь-який файл під ProjectSettings\ → корінь = parent(ProjectSettings)
        if let Some(root) = project_root_from_segment(&norm, "projectsettings") {
            self.project_roots
                .write()
                .expect("lock")
                .insert(root.clone());
            self.saw_project_settings_parent
                .write()
                .expect("lock")
                .insert(root);
            return;
        }
        // Assets\ → запам'ятати кандидата; підтвердимо разом із ProjectSettings
        if let Some(root) = project_root_from_segment(&norm, "assets") {
            self.saw_assets_parent.write().expect("lock").insert(root);
        }
    }

    fn reconcile_assets_project_settings(&self) {
        let assets = self.saw_assets_parent.read().expect("lock").clone();
        let settings = self.saw_project_settings_parent.read().expect("lock");
        let mut roots = self.project_roots.write().expect("lock");
        for r in assets {
            if settings.contains(&r) {
                roots.insert(r);
            }
        }
    }

    fn is_unity_project(&self, project_root: &str) -> bool {
        self.project_roots
            .read()
            .expect("lock")
            .contains(&normalize_path(project_root))
    }

    /// Підтверджений артефактний корінь (Library/Temp/Obj) у Unity-проєкті.
    pub fn confirmed_root(&self, path: &str) -> Option<(String, UnityFolderKind)> {
        let (artifact_root, kind) = outermost_unity_artifact_root(path)?;
        let project = parent_dir(&artifact_root)?;
        if self.is_unity_project(&project) {
            Some((artifact_root, kind))
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

    /// Згорнути файли в одиниці-папки Library/Temp/Obj (T-053). Оновлює активність (T-052).
    pub fn aggregate_units(&self, records: &[FileRecord]) -> Vec<FileRecord> {
        self.rebuild_activity(records);

        #[derive(Default)]
        struct Acc {
            unit: FolderUnitAcc,
            kind: Option<UnityFolderKind>,
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
                let label = format!("Unity {}", kind.as_str());
                let (safety, phrase) = self.safety_and_phrase(&path);
                let mut notes = vec![kind.regenerable_note().to_string()];
                if !phrase.is_empty() {
                    notes.push(phrase);
                }
                build_folder_unit_from_acc(
                    FolderUnitSpec {
                        path,
                        label,
                        bytes: 0,
                        file_count: 0,
                        category: CategoryId::DevArtifacts,
                        safety,
                        detector_id: DETECTOR_ID,
                        notes,
                        id_namespace: id_ns::UNITY,
                    },
                    acc.unit,
                )
            })
            .collect();
        sort_folder_units_by_size_desc(&mut units);
        units
    }
}

impl Default for UnityArtifactsDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector for UnityArtifactsDetector {
    fn id(&self) -> DetectorId {
        DETECTOR_ID
    }

    fn category(&self) -> CategoryId {
        CategoryId::DevArtifacts
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), CoreError> {
        self.enabled.store(enabled, Ordering::Relaxed);
        Ok(())
    }

    fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
        let (root, kind) = self.confirmed_root(&record.path)?;
        let (safety, phrase) = self.safety_and_phrase(&root);
        if record.unit == CandidateUnit::Folder {
            if normalize_path(&record.path) != normalize_path(&root) {
                return None;
            }
            let base = format!(
                "Unity {} · {} · {}",
                kind.as_str(),
                format_bytes_as_gb(record.size.0),
                kind.regenerable_note()
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
            "Unity {} · {} · {}",
            kind.as_str(),
            format_bytes_as_gb(record.size.0),
            kind.regenerable_note()
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

/// `…\ProjectSettings\ProjectVersion.txt` → project root.
fn project_root_from_project_version(norm_path: &str) -> Option<String> {
    let name = file_name(norm_path)?;
    if !name.eq_ignore_ascii_case("projectversion.txt") {
        return None;
    }
    // ...\ProjectSettings\ProjectVersion.txt
    let ps = parent_dir(norm_path)?;
    let ps_name = file_name(&ps)?;
    if !ps_name.eq_ignore_ascii_case("projectsettings") {
        return None;
    }
    parent_dir(&ps)
}

/// Перший сегмент `segment` у шляху → корінь = усе до сегмента (не включно).
fn project_root_from_segment(norm_path: &str, segment: &str) -> Option<String> {
    let parts: Vec<&str> = norm_path.split('\\').filter(|s| !s.is_empty()).collect();
    let mut prefix = String::new();
    let mut start = 0;
    if let Some(first) = parts.first() {
        if first.ends_with(':') {
            prefix = (*first).to_string();
            start = 1;
        }
    }
    for i in start..parts.len() {
        if parts[i].eq_ignore_ascii_case(segment) {
            if i == start {
                return None; // сегмент одразу під drive
            }
            let mut root = prefix.clone();
            for p in &parts[start..i] {
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
            return Some(root);
        }
    }
    None
}

/// Outermost Library/Temp/Obj у шляху.
pub fn outermost_unity_artifact_root(path: &str) -> Option<(String, UnityFolderKind)> {
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
        if let Some(kind) = UnityFolderKind::from_segment(parts[i]) {
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
    fn library_requires_unity_structure() {
        let det = UnityArtifactsDetector::new();
        assert!(
            det.evaluate(&rec(1, r"C:\Games\Foo\Library\ArtifactDB", 9_000_000_000))
                .is_none(),
            "Library без Unity-маркерів — не кандидат"
        );
        det.ingest_paths([r"C:\Games\Foo\ProjectSettings\ProjectVersion.txt"]);
        let v = det
            .evaluate(&rec(1, r"C:\Games\Foo\Library\ArtifactDB", 9_000_000_000))
            .expect("unity library");
        assert_eq!(v.category, CategoryId::DevArtifacts);
        assert_eq!(v.safety, SafetyLevel::SafeToBulk);
        assert!(v.explanation.contains("Library"), "{}", v.explanation);
        assert!(
            v.explanation.contains("перестворюється"),
            "DoD: Library позначена як перестворювана: {}",
            v.explanation
        );
    }

    #[test]
    fn temp_and_obj_under_unity_project() {
        let det =
            UnityArtifactsDetector::from_index_paths([r"D:\U\ProjectSettings\ProjectVersion.txt"]);
        assert!(det
            .evaluate(&rec(1, r"D:\U\Temp\UnityLockfile", 100))
            .is_some());
        assert!(det.evaluate(&rec(2, r"D:\U\Obj\Debug\x", 100)).is_some());
        // чужий Windows Temp
        assert!(det
            .evaluate(&rec(3, r"C:\Windows\Temp\x.tmp", 100))
            .is_none());
    }

    #[test]
    fn assets_plus_project_settings_heuristic() {
        let det = UnityArtifactsDetector::new();
        det.ingest_paths([
            r"E:\Game\Assets\Scripts\A.cs",
            r"E:\Game\ProjectSettings\EditorBuildSettings.asset",
        ]);
        assert_eq!(det.project_count(), 1);
        assert!(det
            .evaluate(&rec(1, r"E:\Game\Library\ShaderCache\x", 50))
            .is_some());
    }

    #[test]
    fn random_library_folder_not_matched() {
        let det = UnityArtifactsDetector::from_index_paths([r"C:\lib\readme.txt"]);
        assert!(det
            .evaluate(&rec(1, r"C:\lib\Library\data.bin", 1000))
            .is_none());
    }

    #[test]
    fn aggregate_library_unit_with_regenerable_note() {
        let det =
            UnityArtifactsDetector::from_index_paths([r"C:\U\ProjectSettings\ProjectVersion.txt"]);
        let units = det.aggregate_units(&[
            rec(1, r"C:\U\Library\a", 1000),
            rec(2, r"C:\U\Library\b", 500),
            rec(3, r"C:\U\Temp\t", 100),
            rec(4, r"C:\U\Assets\x.cs", 50),
        ]);
        assert_eq!(units.len(), 2);
        let lib = units
            .iter()
            .find(|u| u.path.to_ascii_lowercase().ends_with("library"))
            .unwrap();
        assert_eq!(lib.unit, CandidateUnit::Folder);
        assert_eq!(lib.size.0, 1500);
        assert!(lib.explanation.contains("перестворюється"));
        assert!(lib.explanation.contains("2 файлів"));
    }

    #[test]
    fn outermost_unity_artifact_helper() {
        let (root, kind) = outermost_unity_artifact_root(r"C:\P\Library\Artifacts\xx").unwrap();
        assert_eq!(kind, UnityFolderKind::Library);
        assert_eq!(root, r"c:\p\library");
    }

    #[test]
    fn orchestrator_register() {
        let det =
            UnityArtifactsDetector::from_index_paths([r"F:\P\ProjectSettings\ProjectVersion.txt"]);
        let mut reg = DetectorRegistry::new();
        reg.register(det);
        let orch = DetectorOrchestrator::new(&reg);
        let out =
            orch.categorize_batch(&[rec(1, r"F:\P\Library\x", 10), rec(2, r"F:\P\Assets\y", 10)]);
        assert_eq!(out.stats.records_updated, 1);
        assert_eq!(out.updated[0].category, CategoryId::DevArtifacts);
    }

    fn rec_dated(id: u64, path: &str, size: u64, days_ago: u64) -> FileRecord {
        let mut r = rec(id, path, size);
        r.modified_at = Some(FsTimestamp(NOW - (days_ago as i64) * FILETIME_PER_DAY));
        r
    }

    #[test]
    fn active_unity_library_not_safe_to_bulk() {
        let det =
            UnityArtifactsDetector::from_index_paths([r"C:\U\ProjectSettings\ProjectVersion.txt"])
                .with_now_filetime(NOW);
        let records = [
            rec_dated(1, r"C:\U\Assets\Scripts\Player.cs", 100, 7),
            rec_dated(2, r"C:\U\ProjectSettings\ProjectVersion.txt", 10, 7),
            rec_dated(3, r"C:\U\Library\ArtifactDB", 9_000_000, 1),
        ];
        det.rebuild_activity(&records);
        let v = det.evaluate(&records[2]).expect("library");
        assert_eq!(v.safety, SafetyLevel::ReviewRecommended);
        assert!(v.explanation.contains("активний"), "{}", v.explanation);
        // mtimes у Library не роблять проєкт активним самі по собі
        assert!(
            !det.activity_index()
                .get(r"C:\U")
                .is_active(NOW, det.inactive_after_days())
                || det.activity_index().get(r"C:\U").age_days(NOW) == Some(7)
        );
    }

    #[test]
    fn inactive_unity_library_safe_to_bulk() {
        let det = UnityArtifactsDetector::from_index_paths([
            r"C:\OldU\ProjectSettings\ProjectVersion.txt",
        ])
        .with_now_filetime(NOW);
        let records = [
            rec_dated(1, r"C:\OldU\Assets\x.cs", 10, 400),
            rec_dated(2, r"C:\OldU\Library\ArtifactDB", 1000, 1),
        ];
        det.rebuild_activity(&records);
        let v = det.evaluate(&records[1]).expect("library");
        assert_eq!(v.safety, SafetyLevel::SafeToBulk);
        assert!(v.explanation.contains("неактивний"), "{}", v.explanation);
    }
}
