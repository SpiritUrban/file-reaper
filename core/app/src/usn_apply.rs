//! Інкрементальне оновлення індексу за USN-дельтою (T-030).
//!
//! Чиста логіка Application: класифікація змін → мутації → застосування
//! до [`HotIndex`]. Розв'язання шляхів через [`FrnPathCache`] (file_ref /
//! parent_ref → шлях); розмір/дати — через колбек probe (I/O ззовні).
//!
//! DoD: створення / зміна / видалення / перейменування відбиваються в
//! індексі без повного рескану.

use std::collections::HashMap;
use std::path::Path;

use trashradar_domain::candidate::{
    ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
    FsTimestamp, SafetyLevel,
};
use trashradar_domain::category::CategoryId;
use trashradar_domain::error::CoreError;
use trashradar_domain::scan::{usn_reason, UsnChange, UsnCursor};

use crate::ports::{HotIndex, IndexStore, UsnReadOutcome};

/// Клас події USN для планування мутацій.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsnChangeClass {
    Create,
    Delete,
    RenameOld,
    RenameNew,
    Modify,
    Ignore,
}

/// Класифікує бітову маску Reason (пріоритет: delete > rename > create > modify).
pub fn classify_usn_reason(reason: u32) -> UsnChangeClass {
    if reason & usn_reason::FILE_DELETE != 0 {
        return UsnChangeClass::Delete;
    }
    if reason & usn_reason::RENAME_OLD_NAME != 0 {
        return UsnChangeClass::RenameOld;
    }
    if reason & usn_reason::RENAME_NEW_NAME != 0 {
        return UsnChangeClass::RenameNew;
    }
    if reason & usn_reason::FILE_CREATE != 0 {
        return UsnChangeClass::Create;
    }
    if reason
        & (usn_reason::DATA_OVERWRITE
            | usn_reason::DATA_EXTEND
            | usn_reason::DATA_TRUNCATION
            | usn_reason::BASIC_INFO_CHANGE)
        != 0
    {
        return UsnChangeClass::Modify;
    }
    // CLOSE сам по собі — шум (часто йде з CREATE/RENAME); ігноруємо.
    UsnChangeClass::Ignore
}

/// Метадані файла з FS/MFT для upsert (розмір, дати).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileProbe {
    pub size: u64,
    pub modified_at: Option<FsTimestamp>,
    pub created_at: Option<FsTimestamp>,
    pub accessed_at: Option<FsTimestamp>,
    pub attributes: FileAttributes,
    pub is_directory: bool,
}

/// Кеш FRN → повний шлях для розкрутки parent_ref + name (T-030).
#[derive(Debug, Default, Clone)]
pub struct FrnPathCache {
    /// file_ref (MFT record) → абсолютний шлях.
    map: HashMap<u64, String>,
}

impl FrnPathCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Засіяти відомі корені томів (MFT root = 5, walk root = 0).
    pub fn seed_volume_root(&mut self, drive: char) {
        let root = format!("{}:", drive.to_ascii_uppercase());
        self.map.insert(5, root.clone());
        self.map.insert(0, root);
    }

    pub fn insert(&mut self, file_ref: u64, path: impl Into<String>) {
        self.map.insert(file_ref, normalize_path(&path.into()));
    }

    pub fn get(&self, file_ref: u64) -> Option<&str> {
        self.map.get(&file_ref).map(String::as_str)
    }

    pub fn remove(&mut self, file_ref: u64) -> Option<String> {
        self.map.remove(&file_ref)
    }

    /// Шлях до запису: з кешу за file_ref, або parent_path + name.
    pub fn resolve_change_path(&self, change: &UsnChange) -> Option<String> {
        if let Some(p) = self.get(change.file_ref) {
            // Для rename_old ім'я в кеші може бути старим; для delete — ок.
            // Якщо reason rename_new — завжди parent+name.
            if classify_usn_reason(change.reason) != UsnChangeClass::RenameNew {
                return Some(p.to_string());
            }
        }
        let parent = self.get(change.parent_ref)?;
        Some(join_child(parent, &change.name))
    }
}

fn normalize_path(path: &str) -> String {
    // Єдиний роздільник для порівнянь у індексі (Windows).
    path.replace('/', "\\")
}

fn join_child(parent: &str, name: &str) -> String {
    let parent = parent.trim_end_matches(['\\', '/']);
    format!("{parent}\\{name}")
}

fn paths_equal(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Запланована мутація індексу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexMutation {
    Upsert(FileRecord),
    Remove { path: String },
}

/// Статистика застосування дельти.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UsnApplyStats {
    pub created: u64,
    pub modified: u64,
    pub deleted: u64,
    pub renamed: u64,
    pub skipped_unresolved: u64,
    pub skipped_directory: u64,
    pub ignored: u64,
}

/// Планує мутації з сирих USN-записів (без I/O, крім `probe`).
///
/// `probe(path)` — розмір/атрибути з диска; `None` → create/modify з size=0
/// (файл зник між USN і probe).
pub fn plan_usn_mutations(
    changes: &[UsnChange],
    cache: &mut FrnPathCache,
    mut probe: impl FnMut(&str) -> Option<FileProbe>,
    next_candidate_id: &mut u64,
) -> (Vec<IndexMutation>, UsnApplyStats) {
    let mut mutations = Vec::new();
    let mut stats = UsnApplyStats::default();

    // Парні rename: file_ref → (optional old path, optional new change)
    let mut rename_old_paths: HashMap<u64, String> = HashMap::new();

    for change in changes {
        if change.is_directory {
            // Каталоги в індексі як кандидати — лише T-053 (папка-одиниця).
            // Для FRN-кешу шляхів директорій оновлюємо map, але не індекс файлів.
            match classify_usn_reason(change.reason) {
                UsnChangeClass::Delete => {
                    cache.remove(change.file_ref);
                    stats.skipped_directory += 1;
                }
                UsnChangeClass::RenameOld => {
                    if let Some(p) = cache.resolve_change_path(change) {
                        rename_old_paths.insert(change.file_ref, p);
                    }
                    stats.skipped_directory += 1;
                }
                UsnChangeClass::RenameNew | UsnChangeClass::Create => {
                    if let Some(parent) = cache.get(change.parent_ref).map(str::to_string) {
                        let path = join_child(&parent, &change.name);
                        cache.insert(change.file_ref, path);
                    } else if let Some(old) = rename_old_paths.remove(&change.file_ref) {
                        // rename dir: parent may still be same
                        let parent = Path::new(&old)
                            .parent()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let path = if parent.is_empty() {
                            change.name.clone()
                        } else {
                            join_child(&parent, &change.name)
                        };
                        cache.insert(change.file_ref, path);
                    }
                    stats.skipped_directory += 1;
                }
                UsnChangeClass::Modify => {
                    stats.skipped_directory += 1;
                }
                UsnChangeClass::Ignore => {
                    stats.ignored += 1;
                }
            }
            continue;
        }

        match classify_usn_reason(change.reason) {
            UsnChangeClass::Ignore => {
                stats.ignored += 1;
            }
            UsnChangeClass::Delete => {
                let path = cache
                    .resolve_change_path(change)
                    .or_else(|| cache.remove(change.file_ref));
                if let Some(path) = path {
                    cache.remove(change.file_ref);
                    mutations.push(IndexMutation::Remove { path });
                    stats.deleted += 1;
                } else {
                    stats.skipped_unresolved += 1;
                }
            }
            UsnChangeClass::RenameOld => {
                if let Some(path) = cache.resolve_change_path(change) {
                    rename_old_paths.insert(change.file_ref, path.clone());
                    // Не remove з індексу одразу — дочекаємось RenameNew,
                    // щоб не мерехтіло; якщо пари немає — remove в кінці.
                    cache.remove(change.file_ref);
                } else {
                    stats.skipped_unresolved += 1;
                }
            }
            UsnChangeClass::RenameNew => {
                let new_path = if let Some(parent) = cache.get(change.parent_ref) {
                    join_child(parent, &change.name)
                } else if let Some(old) = rename_old_paths.get(&change.file_ref) {
                    let parent = Path::new(old)
                        .parent()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if parent.is_empty() {
                        change.name.clone()
                    } else {
                        join_child(&parent, &change.name)
                    }
                } else {
                    stats.skipped_unresolved += 1;
                    continue;
                };

                if let Some(old_path) = rename_old_paths.remove(&change.file_ref) {
                    mutations.push(IndexMutation::Remove {
                        path: old_path.clone(),
                    });
                    let probe_data = probe(&new_path);
                    let record =
                        build_record(*next_candidate_id, &new_path, probe_data.as_ref(), change);
                    *next_candidate_id += 1;
                    cache.insert(change.file_ref, new_path);
                    mutations.push(IndexMutation::Upsert(record));
                    stats.renamed += 1;
                } else {
                    // RenameNew без Old — трактуємо як create.
                    let probe_data = probe(&new_path);
                    let record =
                        build_record(*next_candidate_id, &new_path, probe_data.as_ref(), change);
                    *next_candidate_id += 1;
                    cache.insert(change.file_ref, new_path);
                    mutations.push(IndexMutation::Upsert(record));
                    stats.created += 1;
                }
            }
            UsnChangeClass::Create => {
                let Some(path) = cache.resolve_change_path(change).or_else(|| {
                    cache
                        .get(change.parent_ref)
                        .map(|p| join_child(p, &change.name))
                }) else {
                    stats.skipped_unresolved += 1;
                    continue;
                };
                let probe_data = probe(&path);
                let record = build_record(*next_candidate_id, &path, probe_data.as_ref(), change);
                *next_candidate_id += 1;
                cache.insert(change.file_ref, path);
                mutations.push(IndexMutation::Upsert(record));
                stats.created += 1;
            }
            UsnChangeClass::Modify => {
                let Some(path) = cache.resolve_change_path(change).or_else(|| {
                    cache
                        .get(change.parent_ref)
                        .map(|p| join_child(p, &change.name))
                }) else {
                    stats.skipped_unresolved += 1;
                    continue;
                };
                let probe_data = probe(&path);
                let record = build_record(*next_candidate_id, &path, probe_data.as_ref(), change);
                *next_candidate_id += 1;
                cache.insert(change.file_ref, path);
                mutations.push(IndexMutation::Upsert(record));
                stats.modified += 1;
            }
        }
    }

    // Незавершені rename_old без new — видалення.
    for (_, old_path) in rename_old_paths {
        mutations.push(IndexMutation::Remove { path: old_path });
        stats.deleted += 1;
    }

    (mutations, stats)
}

fn build_record(id: u64, path: &str, probe: Option<&FileProbe>, change: &UsnChange) -> FileRecord {
    let (size, modified_at, created_at, accessed_at, attributes, is_dir) = match probe {
        Some(p) => (
            p.size,
            p.modified_at,
            p.created_at,
            p.accessed_at,
            p.attributes,
            p.is_directory,
        ),
        None => (
            0,
            change.timestamp,
            None,
            change.timestamp,
            FileAttributes::default(),
            change.is_directory,
        ),
    };
    let _ = is_dir;
    FileRecord {
        candidate_id: CandidateId(id),
        path: normalize_path(path),
        size: ByteSize(size),
        created_at,
        modified_at,
        accessed_at,
        kind: FileKind::from_path(path),
        unit: CandidateUnit::File,
        category: CategoryId::Uncategorized,
        safety: SafetyLevel::ReviewRecommended,
        decision: Decision::Undecided,
        detector_id: String::new(),
        explanation: String::new(),
        attributes,
    }
}

/// Застосовує мутації до гарячого індексу (T-030).
pub fn apply_mutations_to_hot_index(
    index: &impl HotIndex,
    mutations: &[IndexMutation],
) -> Result<(), CoreError> {
    let mut removes: Vec<String> = Vec::new();
    let mut upserts: Vec<FileRecord> = Vec::new();
    for m in mutations {
        match m {
            IndexMutation::Remove { path } => removes.push(path.clone()),
            IndexMutation::Upsert(r) => upserts.push(r.clone()),
        }
    }
    if !removes.is_empty() {
        index.remove_paths(&removes)?;
    }
    if !upserts.is_empty() {
        index.upsert_batch(upserts)?;
    }
    Ok(())
}

/// Повний цикл T-030: дельта → hot index → (опц.) persistent deletes → курсор.
pub fn apply_usn_read_outcome(
    outcome: UsnReadOutcome,
    volume: char,
    index: &impl HotIndex,
    store: &impl IndexStore,
    cache: &mut FrnPathCache,
    probe: impl FnMut(&str) -> Option<FileProbe>,
) -> Result<UsnApplyStats, CoreError> {
    match outcome {
        UsnReadOutcome::JournalStale { reason, .. } => {
            // T-031 зробить повний рескан; тут лише не чіпаємо курсор.
            tracing_stub_stale(volume, reason);
            Ok(UsnApplyStats::default())
        }
        UsnReadOutcome::Changes {
            changes,
            next_cursor,
        } => {
            let mut next_id = next_candidate_id(index)?;
            let (mutations, stats) = plan_usn_mutations(&changes, cache, probe, &mut next_id);
            apply_mutations_to_hot_index(index, &mutations)?;

            // Persistent: лише видалення шляхів (create лишається Uncategorized
            // у hot index до детекторів; SQLite CHECK не приймає Uncategorized).
            for m in &mutations {
                if let IndexMutation::Remove { path } = m {
                    let _ = store.delete_file_records_by_path(path)?;
                }
            }

            store.set_usn_cursor(volume, next_cursor)?;
            Ok(stats)
        }
    }
}

fn next_candidate_id(index: &impl HotIndex) -> Result<u64, CoreError> {
    let max = index
        .get_all()?
        .into_iter()
        .map(|r| r.candidate_id.0)
        .max()
        .unwrap_or(0);
    Ok(max.saturating_add(1))
}

fn tracing_stub_stale(volume: char, reason: &str) {
    let _ = (volume, reason);
    // tracing optional in app — shell/оркестратор залогує; app без tracing dep.
}

/// Зручність: просунути курсор без змін (порожня дельта).
pub fn advance_cursor_only(
    store: &impl IndexStore,
    volume: char,
    cursor: UsnCursor,
) -> Result<(), CoreError> {
    store.set_usn_cursor(volume, cursor)
}

/// Порівняння шляхів для індексу (експорт для тестів адаптерів).
pub fn index_paths_equal(a: &str, b: &str) -> bool {
    paths_equal(&normalize_path(a), &normalize_path(b))
}

/// Нормалізація шляху (експорт для адаптерів).
pub fn normalize_index_path(path: &str) -> String {
    normalize_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::HotIndex;
    use std::sync::Mutex;
    use trashradar_domain::scan::UsnChange;

    struct MemIndex {
        records: Mutex<Vec<FileRecord>>,
    }

    impl MemIndex {
        fn new() -> Self {
            Self {
                records: Mutex::new(Vec::new()),
            }
        }
        fn paths(&self) -> Vec<String> {
            self.records
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.path.clone())
                .collect()
        }
    }

    impl HotIndex for MemIndex {
        fn insert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError> {
            self.records.lock().unwrap().extend(records);
            Ok(())
        }
        fn finish_indexing(&self) -> Result<(), CoreError> {
            Ok(())
        }
        fn len(&self) -> Result<usize, CoreError> {
            Ok(self.records.lock().unwrap().len())
        }
        fn is_empty(&self) -> Result<bool, CoreError> {
            Ok(self.records.lock().unwrap().is_empty())
        }
        fn clear(&self) -> Result<(), CoreError> {
            self.records.lock().unwrap().clear();
            Ok(())
        }
        fn get_all(&self) -> Result<Vec<FileRecord>, CoreError> {
            Ok(self.records.lock().unwrap().clone())
        }
        fn search_file_records(
            &self,
            _query: &str,
            _limit: usize,
        ) -> Result<Vec<FileRecord>, CoreError> {
            Ok(Vec::new())
        }
        fn remove_paths(&self, paths: &[String]) -> Result<usize, CoreError> {
            let mut recs = self.records.lock().unwrap();
            let before = recs.len();
            recs.retain(|r| !paths.iter().any(|p| paths_equal(&r.path, p)));
            Ok(before - recs.len())
        }
        fn upsert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError> {
            let mut recs = self.records.lock().unwrap();
            for record in records {
                if let Some(pos) = recs.iter().position(|r| paths_equal(&r.path, &record.path)) {
                    recs[pos] = record;
                } else {
                    recs.push(record);
                }
            }
            Ok(())
        }
    }

    fn change(
        usn: i64,
        file_ref: u64,
        parent_ref: u64,
        reason: u32,
        name: &str,
        is_dir: bool,
    ) -> UsnChange {
        UsnChange {
            usn,
            file_ref,
            parent_ref,
            reason,
            name: name.to_string(),
            is_directory: is_dir,
            timestamp: None,
        }
    }

    fn probe_map(sizes: HashMap<String, u64>) -> impl FnMut(&str) -> Option<FileProbe> {
        move |path: &str| {
            let key = normalize_path(path);
            sizes.get(&key).map(|&size| FileProbe {
                size,
                modified_at: None,
                created_at: None,
                accessed_at: None,
                attributes: FileAttributes::default(),
                is_directory: false,
            })
        }
    }

    #[test]
    fn classify_priority() {
        assert_eq!(
            classify_usn_reason(usn_reason::FILE_CREATE | usn_reason::CLOSE),
            UsnChangeClass::Create
        );
        assert_eq!(
            classify_usn_reason(usn_reason::FILE_DELETE | usn_reason::CLOSE),
            UsnChangeClass::Delete
        );
        assert_eq!(
            classify_usn_reason(usn_reason::RENAME_OLD_NAME),
            UsnChangeClass::RenameOld
        );
        assert_eq!(
            classify_usn_reason(usn_reason::DATA_EXTEND),
            UsnChangeClass::Modify
        );
        assert_eq!(
            classify_usn_reason(usn_reason::CLOSE),
            UsnChangeClass::Ignore
        );
    }

    #[test]
    fn create_modify_delete_reflected_in_index() {
        let index = MemIndex::new();
        let mut cache = FrnPathCache::new();
        cache.seed_volume_root('C');
        cache.insert(10, "C:\\data");

        let mut sizes: HashMap<String, u64> = HashMap::new();
        sizes.insert("C:\\data\\a.bin".into(), 100);
        sizes.insert("C:\\data\\a.bin".into(), 200); // modify size

        let mut next_id = 1u64;
        let changes = [
            change(1, 20, 10, usn_reason::FILE_CREATE, "a.bin", false),
            change(2, 20, 10, usn_reason::DATA_EXTEND, "a.bin", false),
            change(3, 20, 10, usn_reason::FILE_DELETE, "a.bin", false),
        ];

        // create
        let mut probe = probe_map({
            let mut m = HashMap::new();
            m.insert("C:\\data\\a.bin".into(), 100);
            m
        });
        let (m1, s1) = plan_usn_mutations(&changes[0..1], &mut cache, &mut probe, &mut next_id);
        apply_mutations_to_hot_index(&index, &m1).unwrap();
        assert_eq!(s1.created, 1);
        assert_eq!(index.paths(), vec!["C:\\data\\a.bin"]);
        assert_eq!(index.get_all().unwrap()[0].size.0, 100);

        // modify
        let mut probe = probe_map({
            let mut m = HashMap::new();
            m.insert("C:\\data\\a.bin".into(), 200);
            m
        });
        let (m2, s2) = plan_usn_mutations(&changes[1..2], &mut cache, &mut probe, &mut next_id);
        apply_mutations_to_hot_index(&index, &m2).unwrap();
        assert_eq!(s2.modified, 1);
        assert_eq!(index.len().unwrap(), 1);
        assert_eq!(index.get_all().unwrap()[0].size.0, 200);

        // delete
        let (m3, s3) = plan_usn_mutations(&changes[2..3], &mut cache, |_| None, &mut next_id);
        apply_mutations_to_hot_index(&index, &m3).unwrap();
        assert_eq!(s3.deleted, 1);
        assert!(index.is_empty().unwrap());
    }

    #[test]
    fn rename_removes_old_and_adds_new() {
        let index = MemIndex::new();
        let mut cache = FrnPathCache::new();
        cache.seed_volume_root('C');
        cache.insert(10, "C:\\data");
        cache.insert(20, "C:\\data\\old.txt");

        // seed existing
        index
            .insert_batch(vec![FileRecord {
                candidate_id: CandidateId(1),
                path: "C:\\data\\old.txt".into(),
                size: ByteSize(50),
                created_at: None,
                modified_at: None,
                accessed_at: None,
                kind: FileKind::Document,
                unit: CandidateUnit::File,
                category: CategoryId::Uncategorized,
                safety: SafetyLevel::ReviewRecommended,
                decision: Decision::Undecided,
                detector_id: String::new(),
                explanation: String::new(),
                attributes: FileAttributes::default(),
            }])
            .unwrap();

        let changes = vec![
            change(1, 20, 10, usn_reason::RENAME_OLD_NAME, "old.txt", false),
            change(2, 20, 10, usn_reason::RENAME_NEW_NAME, "new.txt", false),
        ];
        let mut next_id = 2u64;
        let mut probe = probe_map({
            let mut m = HashMap::new();
            m.insert("C:\\data\\new.txt".into(), 50);
            m
        });
        let (mutations, stats) = plan_usn_mutations(&changes, &mut cache, &mut probe, &mut next_id);
        apply_mutations_to_hot_index(&index, &mutations).unwrap();

        assert_eq!(stats.renamed, 1);
        let paths = index.paths();
        assert_eq!(paths.len(), 1);
        assert!(paths
            .iter()
            .any(|p| p.eq_ignore_ascii_case("C:\\data\\new.txt")));
        assert!(!paths
            .iter()
            .any(|p| p.eq_ignore_ascii_case("C:\\data\\old.txt")));
    }

    #[test]
    fn unresolved_without_parent_is_skipped() {
        let mut cache = FrnPathCache::new();
        let mut next_id = 1u64;
        let changes = vec![change(
            1,
            99,
            88,
            usn_reason::FILE_CREATE,
            "orphan.bin",
            false,
        )];
        let (mutations, stats) = plan_usn_mutations(&changes, &mut cache, |_| None, &mut next_id);
        assert!(mutations.is_empty());
        assert_eq!(stats.skipped_unresolved, 1);
    }
}
