//! Розпізнавання Unity Library / Temp / Obj (T-051).
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
//! DoD: проєкт за структурою; Library позначена як перестворювана.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use super::contract::{Detector, DetectorId};
use super::format::{app_cache_unit_explanation, format_bytes_as_gb};
use super::node_modules::{file_name, normalize_path, parent_dir};
use trashradar_domain::candidate::{
    ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
    SafetyLevel, Verdict,
};
use trashradar_domain::category::CategoryId;

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
}

impl UnityArtifactsDetector {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            project_roots: RwLock::new(HashSet::new()),
            saw_assets_parent: RwLock::new(HashSet::new()),
            saw_project_settings_parent: RwLock::new(HashSet::new()),
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

    pub fn project_count(&self) -> usize {
        self.project_roots.read().expect("lock").len()
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

    /// Згорнути файли в одиниці-папки Library/Temp/Obj.
    pub fn aggregate_units(&self, records: &[FileRecord]) -> Vec<FileRecord> {
        #[derive(Default)]
        struct Acc {
            bytes: u64,
            count: u64,
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
            acc.bytes = acc.bytes.saturating_add(record.size.0);
            acc.count = acc.count.saturating_add(1);
            acc.kind = Some(kind);
        }

        let mut units: Vec<FileRecord> = by_root
            .into_iter()
            .filter_map(|(path, acc)| {
                let kind = acc.kind?;
                let label = format!("Unity {}", kind.as_str());
                let mut explanation = app_cache_unit_explanation(&label, acc.bytes, acc.count);
                explanation.push_str(" · ");
                explanation.push_str(kind.regenerable_note());
                Some(FileRecord {
                    candidate_id: CandidateId(stable_id(&path)),
                    path,
                    size: ByteSize(acc.bytes),
                    created_at: None,
                    modified_at: None,
                    accessed_at: None,
                    kind: FileKind::Other,
                    unit: CandidateUnit::Folder,
                    category: CategoryId::DevArtifacts,
                    safety: SafetyLevel::SafeToBulk,
                    decision: Decision::Undecided,
                    detector_id: DETECTOR_ID.as_str().to_string(),
                    explanation,
                    attributes: FileAttributes::default(),
                })
            })
            .collect();
        units.sort_by(|a, b| b.size.0.cmp(&a.size.0).then_with(|| a.path.cmp(&b.path)));
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

    fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
        let (root, kind) = self.confirmed_root(&record.path)?;
        if record.unit == CandidateUnit::Folder {
            if normalize_path(&record.path) != normalize_path(&root) {
                return None;
            }
            return Some(Verdict::new(
                CategoryId::DevArtifacts,
                format!(
                    "Unity {} · {} · {}",
                    kind.as_str(),
                    format_bytes_as_gb(record.size.0),
                    kind.regenerable_note()
                ),
                SafetyLevel::SafeToBulk,
            ));
        }
        if record.unit != CandidateUnit::File {
            return None;
        }
        Some(Verdict::new(
            CategoryId::DevArtifacts,
            format!(
                "Unity {} · {} · {}",
                kind.as_str(),
                format_bytes_as_gb(record.size.0),
                kind.regenerable_note()
            ),
            SafetyLevel::SafeToBulk,
        ))
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

fn stable_id(path: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    normalize_path(path).hash(&mut h);
    0xB000_0000_0000_0000 | (h.finish() & 0x0fff_ffff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::{DetectorOrchestrator, DetectorRegistry};

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
}
