//! Активність проєкту за датами файлів **поза** артефактами (T-052).
//!
//! architecture.md §6.1: структурні детектори обчислюють «активність проєкту»
//! за датами сусідів. product.md Сценарій C: проєкти без правок **понад 3 міс.**
//! — неактивні; їхні `node_modules` / build / Unity Library можна safe-to-bulk.
//!
//! DoD T-052: проєкт зі свіжими правками джерел = **активний** → артефакти
//! **не** [`SafetyLevel::SafeToBulk`] (лише [`SafetyLevel::ReviewRecommended`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::RwLock;

use super::format::{age_days_filetime, format_age_uk, system_now_filetime};
use super::node_modules::normalize_path;
use trashradar_domain::candidate::{CandidateUnit, FileRecord, FsTimestamp, SafetyLevel};

/// Дефолт: 90 днів (3 місяці) — product.md Сценарій C.
pub const DEFAULT_INACTIVE_AFTER_DAYS: u64 = 90;

/// Sentinel: брати системний годинник.
const NOW_USE_SYSTEM: i64 = 0;

/// Імена тек-артефактів (сегменти шляху), які **не** враховуються в активності.
///
/// Регістронезалежно. Лише regenerable-дерева з T-049…T-051 + типовий
/// `.git` (не джерела).
const ARTIFACT_SEGMENTS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "obj",
    "library", // Unity Library
    "temp",    // Unity Temp (під коренем проєкту)
    ".git",
];

/// Сигнал активності одного проєктного кореня.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProjectActivity {
    /// Найсвіжіший mtime/atime файлу поза артефактами (FILETIME).
    pub last_ts: Option<i64>,
}

impl ProjectActivity {
    /// Вік сигналу в повних днях; `None` — немає дат поза артефактами.
    pub fn age_days(self, now: i64) -> Option<u64> {
        self.last_ts.map(|ts| age_days_filetime(now, ts))
    }

    /// Активний = є сигнал і вік **менший** за `inactive_after_days`.
    ///
    /// Без дат поза артефактами → **неактивний** (типовий «лише сміття» /
    /// покинутий клон без джерел у індексі) → safe-to-bulk дозволений.
    pub fn is_active(self, now: i64, inactive_after_days: u64) -> bool {
        match self.age_days(now) {
            Some(age) => age < inactive_after_days,
            None => false,
        }
    }

    /// Активний проєкт → review; неактивний → safe-to-bulk.
    pub fn safety(self, now: i64, inactive_after_days: u64) -> SafetyLevel {
        if self.is_active(now, inactive_after_days) {
            SafetyLevel::ReviewRecommended
        } else {
            SafetyLevel::SafeToBulk
        }
    }
}

/// Кеш активності: `project_root` (нормалізований) → [`ProjectActivity`].
///
/// Ділиться між structural-детекторами (спільний rebuild з батчу індексу).
#[derive(Debug)]
pub struct ProjectActivityIndex {
    by_root: RwLock<HashMap<String, ProjectActivity>>,
    inactive_after_days: AtomicU64,
    now_filetime: AtomicI64,
}

impl ProjectActivityIndex {
    pub fn new() -> Self {
        Self {
            by_root: RwLock::new(HashMap::new()),
            inactive_after_days: AtomicU64::new(DEFAULT_INACTIVE_AFTER_DAYS),
            now_filetime: AtomicI64::new(NOW_USE_SYSTEM),
        }
    }

    pub fn with_inactive_after_days(inactive_after_days: u64) -> Self {
        let idx = Self::new();
        idx.inactive_after_days
            .store(inactive_after_days, Ordering::Relaxed);
        idx
    }

    /// Зафіксувати «зараз» для детермінованих тестів (Windows FILETIME).
    pub fn with_now_filetime(self, now: i64) -> Self {
        self.now_filetime.store(now, Ordering::Relaxed);
        self
    }

    pub fn set_now_filetime(&self, now: Option<i64>) {
        self.now_filetime
            .store(now.unwrap_or(NOW_USE_SYSTEM), Ordering::Relaxed);
    }

    pub fn inactive_after_days(&self) -> u64 {
        self.inactive_after_days.load(Ordering::Relaxed)
    }

    pub fn set_inactive_after_days(&self, days: u64) {
        self.inactive_after_days.store(days, Ordering::Relaxed);
    }

    pub fn now(&self) -> i64 {
        let n = self.now_filetime.load(Ordering::Relaxed);
        if n == NOW_USE_SYSTEM {
            system_now_filetime()
        } else {
            n
        }
    }

    pub fn clear(&self) {
        self.by_root.write().expect("activity lock").clear();
    }

    pub fn len(&self) -> usize {
        self.by_root.read().expect("activity lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Перебудувати активність для відомих коренів проєктів з повних записів індексу.
    ///
    /// Для кожного `project_root` береться max(mtime, else atime) серед **файлів**
    /// під коренем, шлях яких не проходить через артефактний сегмент.
    pub fn rebuild_for_roots<'a>(
        &self,
        project_roots: impl IntoIterator<Item = &'a str>,
        records: &[FileRecord],
    ) {
        let roots: Vec<String> = project_roots
            .into_iter()
            .map(normalize_path)
            .filter(|r| !r.is_empty())
            .collect();
        if roots.is_empty() {
            self.clear();
            return;
        }

        let mut max_ts: HashMap<String, i64> = HashMap::with_capacity(roots.len());
        for root in &roots {
            max_ts.insert(root.clone(), i64::MIN);
        }

        for record in records {
            if record.unit != CandidateUnit::File {
                continue;
            }
            let Some(ts) = activity_timestamp(record) else {
                continue;
            };
            let path = normalize_path(&record.path);
            for root in &roots {
                if !path_is_under_project(&path, root) {
                    continue;
                }
                if path_is_under_artifact(&path, root) {
                    continue;
                }
                let entry = max_ts.get_mut(root).expect("root inserted");
                if ts > *entry {
                    *entry = ts;
                }
            }
        }

        let mut map = self.by_root.write().expect("activity lock");
        map.clear();
        for (root, ts) in max_ts {
            map.insert(
                root,
                ProjectActivity {
                    last_ts: if ts == i64::MIN { None } else { Some(ts) },
                },
            );
        }
    }

    /// Оновити/додати активність для одного кореня (інкрементально).
    pub fn set_activity(&self, project_root: &str, activity: ProjectActivity) {
        self.by_root
            .write()
            .expect("activity lock")
            .insert(normalize_path(project_root), activity);
    }

    pub fn get(&self, project_root: &str) -> ProjectActivity {
        self.by_root
            .read()
            .expect("activity lock")
            .get(&normalize_path(project_root))
            .copied()
            .unwrap_or_default()
    }

    pub fn safety_for(&self, project_root: &str) -> SafetyLevel {
        self.get(project_root)
            .safety(self.now(), self.inactive_after_days())
    }

    pub fn is_active_project(&self, project_root: &str) -> bool {
        self.get(project_root)
            .is_active(self.now(), self.inactive_after_days())
    }

    /// Фраза для explanation: «активний (зміна 3 дн. тому)» / «неактивний 4 міс.».
    pub fn explanation_phrase(&self, project_root: &str) -> String {
        activity_explanation_phrase(
            self.get(project_root),
            self.now(),
            self.inactive_after_days(),
        )
    }
}

impl Default for ProjectActivityIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Дата-сигнал з запису: **modified** пріоритетніше (правки джерел), інакше accessed.
pub fn activity_timestamp(record: &FileRecord) -> Option<i64> {
    match (record.modified_at, record.accessed_at) {
        (Some(FsTimestamp(m)), _) => Some(m),
        (None, Some(FsTimestamp(a))) => Some(a),
        (None, None) => None,
    }
}

/// Чи `path` (нормалізований) лежить під `project_root` (включно з самим коренем).
pub fn path_is_under_project(norm_path: &str, norm_project_root: &str) -> bool {
    let path = normalize_path(norm_path);
    let root = normalize_path(norm_project_root);
    if path == root {
        return true;
    }
    let prefix = if root.ends_with('\\') {
        root
    } else {
        format!("{root}\\")
    };
    path.starts_with(&prefix)
}

/// Чи шлях проходить через артефактну теку **відносно** кореня проєкту.
///
/// `c:\proj\node_modules\x` → true; `c:\proj\src\main.ts` → false.
/// `c:\proj\packages\a\node_modules\b` → true (вкладені артефакти).
pub fn path_is_under_artifact(norm_path: &str, norm_project_root: &str) -> bool {
    let path = normalize_path(norm_path);
    let root = normalize_path(norm_project_root);
    if !path_is_under_project(&path, &root) {
        return false;
    }
    let rel = if path.len() > root.len() {
        let rest = &path[root.len()..];
        rest.trim_start_matches('\\')
    } else {
        return false; // сам корінь — не артефакт
    };
    if rel.is_empty() {
        return false;
    }
    for seg in rel.split('\\').filter(|s| !s.is_empty()) {
        // Ім'я файла в кінці: перевіряємо лише як сегмент-директорію, якщо
        // є наступні — але `node_modules` як файл рідкісний; сегмент у шляху
        // артефакта завжди директорія. Для простоти: будь-який сегмент з
        // ARTIFACT_SEGMENTS (включно з останнім, якщо шлях = …\node_modules).
        if is_artifact_segment(seg) {
            return true;
        }
    }
    false
}

fn is_artifact_segment(seg: &str) -> bool {
    ARTIFACT_SEGMENTS
        .iter()
        .any(|a| seg.eq_ignore_ascii_case(a))
}

/// Людська фраза активності (українською) для explanation.
pub fn activity_explanation_phrase(
    activity: ProjectActivity,
    now: i64,
    inactive_after_days: u64,
) -> String {
    match activity.age_days(now) {
        Some(age) if activity.is_active(now, inactive_after_days) => {
            format!("активний (зміна {} тому)", format_age_uk(age))
        }
        Some(age) => format!("неактивний {}", format_age_uk(age)),
        None => "активність невідома".into(),
    }
}

/// Дописати фразу активності до explanation (через « · »).
pub fn append_activity_phrase(explanation: &str, phrase: &str) -> String {
    if phrase.is_empty() {
        explanation.to_string()
    } else {
        format!("{explanation} · {phrase}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::format::FILETIME_PER_DAY;
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, SafetyLevel,
    };
    use trashradar_domain::category::CategoryId;

    const NOW: i64 = 100_000 * FILETIME_PER_DAY; // synthetic epoch days

    fn rec(path: &str, size: u64, modified_days_ago: Option<u64>) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(1),
            path: path.into(),
            size: ByteSize(size),
            created_at: None,
            modified_at: modified_days_ago
                .map(|d| FsTimestamp(NOW - (d as i64) * FILETIME_PER_DAY)),
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
    fn artifact_segments_detected_under_project() {
        assert!(path_is_under_artifact(
            r"c:\proj\node_modules\lodash\index.js",
            r"c:\proj"
        ));
        assert!(path_is_under_artifact(
            r"c:\proj\target\debug\app",
            r"c:\proj"
        ));
        assert!(path_is_under_artifact(
            r"c:\u\Library\ShaderCache\x",
            r"c:\u"
        ));
        assert!(!path_is_under_artifact(r"c:\proj\src\main.ts", r"c:\proj"));
        assert!(!path_is_under_artifact(r"c:\proj\package.json", r"c:\proj"));
    }

    #[test]
    fn nested_artifact_excluded() {
        assert!(path_is_under_artifact(
            r"c:\proj\packages\a\node_modules\b\x.js",
            r"c:\proj"
        ));
    }

    #[test]
    fn activity_ignores_artifact_mtimes() {
        let idx = ProjectActivityIndex::with_inactive_after_days(90).with_now_filetime(NOW);
        let records = [
            // свіжий node_modules — НЕ рахується
            rec(r"C:\proj\node_modules\x\index.js", 100, Some(1)),
            // старе джерело — рахується
            rec(r"C:\proj\src\main.ts", 50, Some(200)),
            rec(r"C:\proj\package.json", 10, Some(200)),
        ];
        idx.rebuild_for_roots([r"C:\proj"], &records);
        let act = idx.get(r"C:\proj");
        assert_eq!(act.age_days(NOW), Some(200));
        assert!(!act.is_active(NOW, 90));
        assert_eq!(idx.safety_for(r"C:\proj"), SafetyLevel::SafeToBulk);
    }

    #[test]
    fn fresh_sources_mark_project_active() {
        // DoD: свіжі правки джерел → активний → не safe-to-bulk
        let idx = ProjectActivityIndex::with_inactive_after_days(90).with_now_filetime(NOW);
        let records = [
            rec(r"C:\live\src\app.ts", 100, Some(5)),
            rec(r"C:\live\node_modules\x\a.js", 999, Some(1)),
            rec(r"C:\live\package.json", 10, Some(5)),
        ];
        idx.rebuild_for_roots([r"C:\live"], &records);
        assert!(idx.is_active_project(r"C:\live"));
        assert_eq!(idx.safety_for(r"C:\live"), SafetyLevel::ReviewRecommended);
        let phrase = idx.explanation_phrase(r"C:\live");
        assert!(phrase.contains("активний"), "{phrase}");
        assert!(phrase.contains("5 дн."), "{phrase}");
    }

    #[test]
    fn inactive_after_threshold() {
        let idx = ProjectActivityIndex::with_inactive_after_days(90).with_now_filetime(NOW);
        let records = [rec(r"C:\old\src\a.rs", 1, Some(90))]; // age == 90 → not active (< 90)
        idx.rebuild_for_roots([r"C:\old"], &records);
        assert!(!idx.is_active_project(r"C:\old"));
        assert_eq!(idx.safety_for(r"C:\old"), SafetyLevel::SafeToBulk);

        let records2 = [rec(r"C:\old\src\a.rs", 1, Some(89))];
        idx.rebuild_for_roots([r"C:\old"], &records2);
        assert!(idx.is_active_project(r"C:\old"));
    }

    #[test]
    fn no_non_artifact_dates_is_inactive() {
        let idx = ProjectActivityIndex::with_inactive_after_days(90).with_now_filetime(NOW);
        let records = [rec(r"C:\only\node_modules\x", 1, Some(1))];
        idx.rebuild_for_roots([r"C:\only"], &records);
        assert!(!idx.is_active_project(r"C:\only"));
        assert_eq!(idx.safety_for(r"C:\only"), SafetyLevel::SafeToBulk);
        assert!(idx.explanation_phrase(r"C:\only").contains("невідома"));
    }

    #[test]
    fn prefers_modified_over_accessed() {
        let mut r = rec(r"C:\p\src\a.ts", 1, Some(10));
        r.accessed_at = Some(FsTimestamp(NOW - 2 * FILETIME_PER_DAY));
        assert_eq!(
            activity_timestamp(&r),
            Some(NOW - 10 * FILETIME_PER_DAY),
            "mtime wins"
        );
    }

    #[test]
    fn two_projects_independent() {
        let idx = ProjectActivityIndex::with_inactive_after_days(90).with_now_filetime(NOW);
        let records = [
            rec(r"C:\a\src\a.ts", 1, Some(3)),
            rec(r"C:\b\src\b.ts", 1, Some(120)),
        ];
        idx.rebuild_for_roots([r"C:\a", r"C:\b"], &records);
        assert!(idx.is_active_project(r"C:\a"));
        assert!(!idx.is_active_project(r"C:\b"));
    }

    #[test]
    fn phrase_inactive_months() {
        let act = ProjectActivity {
            last_ts: Some(NOW - 120 * FILETIME_PER_DAY),
        };
        let p = activity_explanation_phrase(act, NOW, 90);
        assert!(p.contains("неактивний"), "{p}");
        assert!(p.contains("4 міс."), "{p}");
    }
}
