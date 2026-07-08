//! Адаптер `HotIndex`: гарячий in-memory індекс метаданих.
//!
//! Реалізація T-015: компактний запис метаданих (48 байт) з інтернуванням
//! директорій та назв файлів у суцільний буфер для мінімізації кучі.

use std::collections::HashMap;
use trashradar_domain::candidate::{
    ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
    FsTimestamp, SafetyLevel,
};
use trashradar_domain::category::CategoryId;

const UNIX_EPOCH_FILETIME_SECS: i64 = 11_644_473_600;

/// Перетворює Windows FILETIME (100ns з 1601 року) у секунди Unix Epoch (з 1970 року).
/// Повертає 0 для None (sentinel).
pub fn filetime_to_unix_secs(filetime: i64) -> u32 {
    if filetime == 0 {
        return 0;
    }
    let filetime_secs = filetime / 10_000_000;
    let unix_secs = filetime_secs - UNIX_EPOCH_FILETIME_SECS;
    if unix_secs <= 0 {
        1 // Клемпимо до 1, бо 0 — маркер відсутності значення (None)
    } else if unix_secs >= u32::MAX as i64 {
        u32::MAX - 1
    } else {
        unix_secs as u32
    }
}

/// Перетворює секунди Unix Epoch назад у Windows FILETIME.
pub fn unix_secs_to_filetime(unix_secs: u32) -> i64 {
    if unix_secs == 0 {
        return 0;
    }
    let filetime_secs = unix_secs as i64 + UNIX_EPOCH_FILETIME_SECS;
    filetime_secs * 10_000_000
}

/// Інтернер рядків з послідовним пакуванням у спільний буфер (string arena).
/// Це уникає накладних витрат 24 байт String-заголовка на кожен елемент.
#[derive(Debug, Clone)]
pub struct PathInterner {
    buffer: String,
    offsets: Vec<u32>,
    lookup: HashMap<String, u32>,
}

impl Default for PathInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl PathInterner {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            offsets: vec![0],
            lookup: HashMap::new(),
        }
    }

    /// Додає рядок до пулу та повертає його ID. Якщо рядок вже існує, повертає його ID.
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.lookup.get(s) {
            return id;
        }
        let id = (self.offsets.len() - 1) as u32;
        self.buffer.push_str(s);
        let next_offset = self.buffer.len() as u32;
        self.offsets.push(next_offset);
        self.lookup.insert(s.to_string(), id);
        id
    }

    /// Повертає рядок за його ID.
    pub fn resolve(&self, id: u32) -> Option<&str> {
        let start = *self.offsets.get(id as usize)? as usize;
        let end = *self.offsets.get(id as usize + 1)? as usize;
        Some(&self.buffer[start..end])
    }

    /// Звільняє мапу пошуку для мінімізації використання пам'яті після завершення індексування.
    pub fn shrink_to_fit(&mut self) {
        self.buffer.shrink_to_fit();
        self.offsets.shrink_to_fit();
        self.lookup = HashMap::new();
        self.lookup.shrink_to_fit();
    }

    /// Відновлює мапу пошуку за потреби (наприклад, для інкрементального додавання).
    pub fn rebuild_lookup(&mut self) {
        if self.lookup.is_empty() && self.offsets.len() > 1 {
            self.lookup.reserve(self.offsets.len() - 1);
            for id in 0..(self.offsets.len() - 1) {
                if let Some(s) = self.resolve(id as u32) {
                    self.lookup.insert(s.to_string(), id as u32);
                }
            }
        }
    }

    /// Оцінює обсяг пам'яті, що займає інтернер у купі (heap).
    pub fn memory_usage(&self) -> usize {
        let mut total = 0;
        total += self.buffer.capacity();
        total += self.offsets.capacity() * std::mem::size_of::<u32>();
        total += self.lookup.capacity() * 32;
        total
    }
}

/// Компактна структура запису файлу в пам'яті (рівно 48 байт).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactFileRecord {
    pub candidate_id: u64,
    pub size: u64,
    pub accessed_at: u32, // Unix Epoch secs
    pub created_at: u32,  // Unix Epoch secs
    pub modified_at: u32, // Unix Epoch secs
    pub dir_id: u32,
    pub filename_id: u32,
    pub attributes: u32,
    pub packed_meta: u16,
}

impl CompactFileRecord {
    /// Запаковує доменну модель `FileRecord` у компактний вигляд.
    pub fn pack(record: &FileRecord, dir_id: u32, filename_id: u32) -> Self {
        let accessed_at = record
            .accessed_at
            .map(|t| filetime_to_unix_secs(t.0))
            .unwrap_or(0);
        let created_at = record
            .created_at
            .map(|t| filetime_to_unix_secs(t.0))
            .unwrap_or(0);
        let modified_at = record
            .modified_at
            .map(|t| filetime_to_unix_secs(t.0))
            .unwrap_or(0);

        let kind_val = record.kind as u16;
        let unit_val = record.unit as u16;
        let category_val = record.category as u16;
        let safety_val = record.safety as u16;
        let decision_val = record.decision as u16;

        let packed_meta = (kind_val & 0x7)
            | ((unit_val & 0x1) << 3)
            | ((category_val & 0xF) << 4)
            | ((safety_val & 0x1) << 8)
            | ((decision_val & 0x3) << 9);

        Self {
            candidate_id: record.candidate_id.0,
            size: record.size.0,
            accessed_at,
            created_at,
            modified_at,
            dir_id,
            filename_id,
            attributes: record.attributes.raw_bits,
            packed_meta,
        }
    }

    /// Розпаковує компактний запис назад у доменну модель `FileRecord`.
    pub fn unpack(&self, path: String) -> FileRecord {
        let accessed_at = if self.accessed_at == 0 {
            None
        } else {
            Some(FsTimestamp(unix_secs_to_filetime(self.accessed_at)))
        };
        let created_at = if self.created_at == 0 {
            None
        } else {
            Some(FsTimestamp(unix_secs_to_filetime(self.created_at)))
        };
        let modified_at = if self.modified_at == 0 {
            None
        } else {
            Some(FsTimestamp(unix_secs_to_filetime(self.modified_at)))
        };

        let kind = parse_file_kind_val(self.packed_meta & 0x7);
        let unit = parse_candidate_unit_val((self.packed_meta >> 3) & 0x1);
        let category = parse_category_val((self.packed_meta >> 4) & 0xF);
        let safety = parse_safety_level_val((self.packed_meta >> 8) & 0x1);
        let decision = parse_decision_val((self.packed_meta >> 9) & 0x3);

        FileRecord {
            candidate_id: CandidateId(self.candidate_id),
            path,
            size: ByteSize(self.size),
            created_at,
            modified_at,
            accessed_at,
            kind,
            unit,
            category,
            safety,
            decision,
            detector_id: String::new(),
            explanation: String::new(),
            attributes: FileAttributes {
                raw_bits: self.attributes,
                is_readonly: (self.attributes & 0x1) != 0,
                is_hidden: (self.attributes & 0x2) != 0,
                is_system: (self.attributes & 0x4) != 0,
                is_temporary: (self.attributes & 0x8) != 0,
            },
        }
    }
}

fn parse_file_kind_val(val: u16) -> FileKind {
    match val {
        0 => FileKind::Video,
        1 => FileKind::Image,
        2 => FileKind::Audio,
        3 => FileKind::Archive,
        4 => FileKind::Installer,
        5 => FileKind::DiskImage,
        6 => FileKind::Document,
        _ => FileKind::Other,
    }
}

fn parse_candidate_unit_val(val: u16) -> CandidateUnit {
    match val {
        0 => CandidateUnit::File,
        _ => CandidateUnit::Folder,
    }
}

fn parse_category_val(val: u16) -> CategoryId {
    match val {
        0 => CategoryId::LargeFiles,
        1 => CategoryId::OldFiles,
        2 => CategoryId::ForgottenVideos,
        3 => CategoryId::Duplicates,
        4 => CategoryId::Archives,
        5 => CategoryId::Installers,
        6 => CategoryId::TempFiles,
        7 => CategoryId::AppCaches,
        _ => CategoryId::DevArtifacts,
    }
}

fn parse_safety_level_val(val: u16) -> SafetyLevel {
    match val {
        0 => SafetyLevel::SafeToBulk,
        _ => SafetyLevel::ReviewRecommended,
    }
}

fn parse_decision_val(val: u16) -> Decision {
    match val {
        0 => Decision::Undecided,
        1 => Decision::Keep,
        _ => Decision::Marked,
    }
}

use std::sync::RwLock;
use trashradar_domain::error::CoreError;

/// Внутрішній стан гарячого in-memory індексу.
#[derive(Debug, Default, Clone)]
struct InMemoryIndexInner {
    records: Vec<CompactFileRecord>,
    dir_interner: PathInterner,
    filename_interner: PathInterner,
}

/// Гарячий in-memory індекс.
#[derive(Debug, Default)]
pub struct InMemoryIndex {
    inner: RwLock<InMemoryIndexInner>,
}

impl InMemoryIndex {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(InMemoryIndexInner::default()),
        }
    }

    /// Додає запис файлу до індексу, розділяючи та інтернуючи шлях.
    pub fn insert(&self, record: &FileRecord) {
        let mut inner = self.inner.write().unwrap();
        inner.dir_interner.rebuild_lookup();
        inner.filename_interner.rebuild_lookup();

        let path = std::path::Path::new(&record.path);
        let parent_str = path.parent().and_then(|p| p.to_str()).unwrap_or("");
        let file_name_str = path.file_name().and_then(|f| f.to_str()).unwrap_or("");

        let dir_id = inner.dir_interner.intern(parent_str);
        let filename_id = inner.filename_interner.intern(file_name_str);

        let compact = CompactFileRecord::pack(record, dir_id, filename_id);
        inner.records.push(compact);
    }

    /// Отримує розпакований запис файлу за його індексом.
    pub fn get(&self, idx: usize) -> Option<FileRecord> {
        let inner = self.inner.read().unwrap();
        let compact = inner.records.get(idx)?;
        let parent = inner.dir_interner.resolve(compact.dir_id).unwrap_or("");
        let file_name = inner
            .filename_interner
            .resolve(compact.filename_id)
            .unwrap_or("");

        let path = if parent.is_empty() {
            file_name.to_string()
        } else {
            let mut p = std::path::PathBuf::from(parent);
            p.push(file_name);
            p.to_string_lossy().into_owned()
        };

        Some(compact.unpack(path))
    }

    pub fn len(&self) -> usize {
        self.inner.read().unwrap().records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().unwrap().records.is_empty()
    }

    pub fn clear(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.records.clear();
    }

    /// Завершує наповнення індексу, вивільняючи lookup-таблиці інтернерів для економії пам'яті.
    pub fn finish_indexing(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.records.shrink_to_fit();
        inner.dir_interner.shrink_to_fit();
        inner.filename_interner.shrink_to_fit();
    }

    pub fn get_all(&self) -> Vec<FileRecord> {
        let inner = self.inner.read().unwrap();
        let mut records = Vec::with_capacity(inner.records.len());
        for compact in &inner.records {
            let parent = inner.dir_interner.resolve(compact.dir_id).unwrap_or("");
            let file_name = inner
                .filename_interner
                .resolve(compact.filename_id)
                .unwrap_or("");

            let path = if parent.is_empty() {
                file_name.to_string()
            } else {
                let mut p = std::path::PathBuf::from(parent);
                p.push(file_name);
                p.to_string_lossy().into_owned()
            };

            records.push(compact.unpack(path));
        }
        records
    }

    /// Розраховує загальний обсяг пам'яті в купі (heap).
    pub fn memory_usage(&self) -> usize {
        let inner = self.inner.read().unwrap();
        let mut total = 0;
        total += inner.records.capacity() * std::mem::size_of::<CompactFileRecord>();
        total += inner.dir_interner.memory_usage();
        total += inner.filename_interner.memory_usage();
        total
    }
}

impl trashradar_app::ports::HotIndex for InMemoryIndex {
    fn insert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError> {
        let mut inner = self.inner.write().unwrap();
        inner.dir_interner.rebuild_lookup();
        inner.filename_interner.rebuild_lookup();

        for record in records {
            let path = std::path::Path::new(&record.path);
            let parent_str = path.parent().and_then(|p| p.to_str()).unwrap_or("");
            let file_name_str = path.file_name().and_then(|f| f.to_str()).unwrap_or("");

            let dir_id = inner.dir_interner.intern(parent_str);
            let filename_id = inner.filename_interner.intern(file_name_str);

            let compact = CompactFileRecord::pack(&record, dir_id, filename_id);
            inner.records.push(compact);
        }

        Ok(())
    }

    fn finish_indexing(&self) -> Result<(), CoreError> {
        self.finish_indexing();
        Ok(())
    }

    fn len(&self) -> Result<usize, CoreError> {
        Ok(self.len())
    }

    fn is_empty(&self) -> Result<bool, CoreError> {
        Ok(self.is_empty())
    }

    fn clear(&self) -> Result<(), CoreError> {
        self.clear();
        Ok(())
    }

    fn get_all(&self) -> Result<Vec<FileRecord>, CoreError> {
        Ok(self.get_all())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(id: u64, path: &str) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: path.to_string(),
            size: ByteSize(id * 1024),
            created_at: Some(FsTimestamp(130000000000000000 + id as i64 * 10_000_000)),
            modified_at: Some(FsTimestamp(140000000000000000 + id as i64 * 10_000_000)),
            accessed_at: Some(FsTimestamp(150000000000000000 + id as i64 * 10_000_000)),
            kind: FileKind::Video,
            unit: CandidateUnit::File,
            category: CategoryId::ForgottenVideos,
            safety: SafetyLevel::SafeToBulk,
            decision: Decision::Undecided,
            detector_id: String::new(),
            explanation: String::new(),
            attributes: FileAttributes {
                raw_bits: 7,
                is_readonly: true,
                is_hidden: true,
                is_system: true,
                is_temporary: false,
            },
        }
    }

    #[test]
    fn test_interning_and_resolution() {
        let mut interner = PathInterner::new();
        let id1 = interner.intern("C:\\Users\\Ada\\Videos");
        let id2 = interner.intern("C:\\Users\\Ada\\Documents");
        let id3 = interner.intern("C:\\Users\\Ada\\Videos"); // Duplicate

        assert_eq!(id1, id3);
        assert_ne!(id1, id2);
        assert_eq!(interner.resolve(id1), Some("C:\\Users\\Ada\\Videos"));
        assert_eq!(interner.resolve(id2), Some("C:\\Users\\Ada\\Documents"));
    }

    #[test]
    fn test_compact_record_size() {
        assert_eq!(std::mem::size_of::<CompactFileRecord>(), 48);
    }

    #[test]
    fn test_pack_unpack_roundtrip() {
        let record = sample_record(42, "C:\\Users\\Ada\\Videos\\holiday.mp4");
        let index = InMemoryIndex::new();
        index.insert(&record);

        assert_eq!(index.len(), 1);
        let restored = index.get(0).unwrap();

        assert_eq!(restored.candidate_id, record.candidate_id);
        assert_eq!(restored.path, record.path);
        assert_eq!(restored.size, record.size);
        assert_eq!(
            restored.created_at.unwrap().0 / 10_000_000,
            record.created_at.unwrap().0 / 10_000_000
        );
        assert_eq!(
            restored.modified_at.unwrap().0 / 10_000_000,
            record.modified_at.unwrap().0 / 10_000_000
        );
        assert_eq!(
            restored.accessed_at.unwrap().0 / 10_000_000,
            record.accessed_at.unwrap().0 / 10_000_000
        );
        assert_eq!(restored.kind, record.kind);
        assert_eq!(restored.unit, record.unit);
        assert_eq!(restored.category, record.category);
        assert_eq!(restored.safety, record.safety);
        assert_eq!(restored.decision, record.decision);
        assert_eq!(restored.attributes, record.attributes);
    }

    #[test]
    fn test_five_million_records_memory_footprint() {
        let index = InMemoryIndex::new();

        {
            let mut inner = index.inner.write().unwrap();
            inner.records.reserve(5_000_000);
            inner.dir_interner.offsets.reserve(500_000);
            inner.dir_interner.lookup.reserve(500_000);
            inner.filename_interner.offsets.reserve(1_000_000);
            inner.filename_interner.lookup.reserve(1_000_000);

            for i in 0..500_000 {
                let dir = format!("C:\\Users\\User\\Folder_{}", i);
                inner.dir_interner.intern(&dir);
            }

            for i in 0..1_000_000 {
                let filename = format!("file_name_{}.dat", i);
                inner.filename_interner.intern(&filename);
            }

            for i in 0..5_000_000 {
                let dir_id = (i % 500_000) as u32;
                let filename_id = (i % 1_000_000) as u32;

                let compact = CompactFileRecord {
                    candidate_id: i as u64,
                    size: (i * 123) as u64,
                    accessed_at: (1500000000 + i) as u32,
                    created_at: (1300000000 + i) as u32,
                    modified_at: (1400000000 + i) as u32,
                    dir_id,
                    filename_id,
                    attributes: 7,
                    packed_meta: 42,
                };
                inner.records.push(compact);
            }
        }

        // Очищуємо lookup таблиці для економії пам'яті в режимі запитів
        index.finish_indexing();

        let total_bytes = index.memory_usage();
        let total_mb = total_bytes as f64 / 1024.0 / 1024.0;
        println!(
            "Estimated memory usage for 5M records (with cleanups): {:.2} MB",
            total_mb
        );

        assert!(
            total_mb < 300.0,
            "Memory usage {:.2} MB exceeds the 300 MB limit!",
            total_mb
        );
    }

    #[test]
    fn test_concurrent_batch_insertions() {
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let index = Arc::new(InMemoryIndex::new());
        let mut threads = Vec::new();

        // Спавнить 8 потоків для паралельного запису пакетів записів
        for thread_idx in 0..8 {
            let index_clone = Arc::clone(&index);
            threads.push(thread::spawn(move || {
                let mut records = Vec::new();
                for i in 0..10_000 {
                    let id = thread_idx * 100_000 + i;
                    records.push(sample_record(
                        id,
                        &format!("C:\\Users\\User\\dir_{}\\file_{}.mp4", thread_idx, i),
                    ));
                }
                use trashradar_app::ports::HotIndex;
                index_clone.insert_batch(records).unwrap();
            }));
        }

        // Потік читача, який робить запити до індексу паралельно із записом
        let index_clone = Arc::clone(&index);
        let reader_thread = thread::spawn(move || {
            let mut read_attempts = 0;
            let mut max_observed_len = 0;
            while max_observed_len < 80_000 && read_attempts < 1000 {
                if let Ok(current_len) = trashradar_app::ports::HotIndex::len(&*index_clone) {
                    if current_len > max_observed_len {
                        max_observed_len = current_len;
                    }
                }
                read_attempts += 1;
                thread::sleep(Duration::from_millis(1));
            }
            println!(
                "Reader thread finished after {} attempts. Max observed len: {}",
                read_attempts, max_observed_len
            );
        });

        for t in threads {
            t.join().unwrap();
        }
        reader_thread.join().unwrap();

        assert_eq!(index.len(), 80_000);
    }

    fn temp_profile_dir(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "trashradar-test-{}-{}",
            name,
            std::time::Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn test_sqlite_to_in_memory_sync_and_checksum_verification() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        use trashradar_index_sqlite::IndexDatabase;

        let temp_dir = temp_profile_dir("sync-test");
        let db_path = temp_dir.join("index.sqlite3");

        // 1. Open SQLite database and insert 1,000 file records
        let mut sqlite_db = IndexDatabase::open(&db_path).expect("open sqlite db");

        let mut records = Vec::new();
        for i in 0..1000 {
            records.push(sample_record(
                i,
                &format!("C:\\Users\\Ada\\dir_{}\\file_{}.txt", i / 10, i),
            ));
        }
        sqlite_db
            .upsert_file_records_batch(&records)
            .expect("upsert records to sqlite");

        // 2. Read all records from SQLite (simulating startup recovery)
        let loaded_records = sqlite_db
            .read_all_file_records()
            .expect("read all from sqlite");

        // 3. Load them into HotIndex in-memory index
        let in_memory_index = InMemoryIndex::new();
        use trashradar_app::ports::HotIndex;
        in_memory_index
            .insert_batch(loaded_records)
            .expect("insert batch to memory");
        in_memory_index.finish_indexing();

        // 4. Retrieve all records from both and verify they match by length and checksum
        let sqlite_all = sqlite_db
            .read_all_file_records()
            .expect("read all sqlite");
        let memory_all = in_memory_index.get_all();

        assert_eq!(sqlite_all.len(), 1000);
        assert_eq!(memory_all.len(), 1000);

        // Helper to calculate checksum
        let calculate_checksum = |recs: &[FileRecord]| -> u64 {
            let mut sorted = recs.to_vec();
            sorted.sort_by_key(|r| r.candidate_id.0);
            let mut hasher = DefaultHasher::new();
            for r in sorted {
                r.candidate_id.0.hash(&mut hasher);
                r.path.hash(&mut hasher);
                r.size.0.hash(&mut hasher);
                r.created_at.map(|t| t.0).hash(&mut hasher);
                r.modified_at.map(|t| t.0).hash(&mut hasher);
                r.accessed_at.map(|t| t.0).hash(&mut hasher);
                (r.kind as u8).hash(&mut hasher);
                (r.unit as u8).hash(&mut hasher);
                (r.category as u8).hash(&mut hasher);
                (r.safety as u8).hash(&mut hasher);
                (r.decision as u8).hash(&mut hasher);
                r.attributes.raw_bits.hash(&mut hasher);
                r.attributes.is_readonly.hash(&mut hasher);
                r.attributes.is_hidden.hash(&mut hasher);
                r.attributes.is_system.hash(&mut hasher);
                r.attributes.is_temporary.hash(&mut hasher);
            }
            hasher.finish()
        };

        let checksum_sqlite = calculate_checksum(&sqlite_all);
        let checksum_memory = calculate_checksum(&memory_all);

        println!(
            "Checksum SQLite: {}, Checksum Memory: {}",
            checksum_sqlite, checksum_memory
        );
        assert_eq!(checksum_sqlite, checksum_memory, "Checksums do not match!");

        // Clean up
        std::fs::remove_dir_all(temp_dir).unwrap_or(());
    }
}
