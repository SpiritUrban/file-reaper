//! Батчева видача результатів скану в індекс (T-024).
//!
//! Зшиває шматки: MFT-скан (T-021) → повні шляхи (T-022) → класифікація
//! типу (T-023) → батчі [`FileRecord`] у приймач (сток). Приймач — синхронний,
//! тож продюсер природно паузиться на споживачі: зворотний тиск утримує в
//! пам'яті лише один батч, а не весь том.
//!
//! Категорія свіжо сканованого запису — [`CategoryId::Uncategorized`]:
//! детектори (T-037+) заявлять справжню категорію пізніше (architecture.md §3.4).

use trashradar_domain::candidate::{
    CandidateId, CandidateUnit, Decision, FileKind, FileRecord, SafetyLevel,
};
use trashradar_domain::category::CategoryId;
use trashradar_domain::error::CoreError;
use trashradar_domain::scan::ScanEntry;

use crate::paths::PathResolver;

/// Перетворює сирий запис скану + повний шлях у `FileRecord` для індексу.
/// Тип файла класифікується за розширенням (T-023); категорія/безпечність/
/// рішення — стартові, їх уточнять детектори.
pub fn to_file_record(entry: &ScanEntry, path: String, candidate_id: u64) -> FileRecord {
    let kind = FileKind::from_path(&path);
    FileRecord {
        candidate_id: CandidateId(candidate_id),
        path,
        size: entry.size,
        created_at: entry.created_at,
        modified_at: entry.modified_at,
        accessed_at: entry.accessed_at,
        kind,
        unit: CandidateUnit::File,
        category: CategoryId::Uncategorized,
        safety: SafetyLevel::ReviewRecommended,
        decision: Decision::Undecided,
        detector_id: String::new(),
        explanation: String::new(),
        attributes: entry.attributes,
    }
}

/// Підсумок наповнення індексу.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IndexingStats {
    /// Файлів віддано в приймач.
    pub files_indexed: u64,
    /// Пропущено (шлях не розкрутився — сирота/цикл).
    pub skipped_no_path: u64,
    /// Кількість зданих батчів.
    pub batches: u64,
}

/// Акумулятор батчів: збирає `FileRecord` і здає їх приймачу партіями по
/// `batch_size`. Синхронний виклик приймача забезпечує зворотний тиск —
/// у пам'яті ніколи не більше одного батча. Логіка чиста (без I/O), тож
/// тестується без доступу до тому.
pub struct Batcher<'r, F> {
    resolver: &'r PathResolver,
    sink: F,
    batch_size: usize,
    batch: Vec<FileRecord>,
    next_id: u64,
    stats: IndexingStats,
    first_err: Option<CoreError>,
}

impl<'r, F> Batcher<'r, F>
where
    F: FnMut(Vec<FileRecord>) -> Result<(), CoreError>,
{
    pub fn new(resolver: &'r PathResolver, batch_size: usize, sink: F) -> Self {
        let batch_size = batch_size.max(1);
        Self {
            resolver,
            sink,
            batch_size,
            batch: Vec::with_capacity(batch_size),
            next_id: 0,
            stats: IndexingStats::default(),
            first_err: None,
        }
    }

    /// Приймає один запис скану. Директорії ігноруються (кандидати — файли;
    /// папки-одиниці — T-053). Після першої помилки приймача — no-op.
    pub fn push(&mut self, entry: ScanEntry) {
        if self.first_err.is_some() || entry.is_directory {
            return;
        }
        let Some(path) = self.resolver.full_path(&entry) else {
            self.stats.skipped_no_path += 1;
            return;
        };
        let record = to_file_record(&entry, path, self.next_id);
        self.next_id += 1;
        self.batch.push(record);
        if self.batch.len() >= self.batch_size {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.batch.is_empty() || self.first_err.is_some() {
            return;
        }
        let taken = std::mem::take(&mut self.batch);
        let count = taken.len() as u64;
        match (self.sink)(taken) {
            Ok(()) => {
                self.stats.files_indexed += count;
                self.stats.batches += 1;
            }
            Err(e) => {
                self.first_err = Some(e);
            }
        }
        self.batch = Vec::with_capacity(self.batch_size);
    }

    /// Здає залишковий батч і повертає підсумок (або першу помилку приймача).
    pub fn finish(mut self) -> Result<IndexingStats, CoreError> {
        self.flush();
        match self.first_err {
            Some(e) => Err(e),
            None => Ok(self.stats),
        }
    }
}

/// Сканує том у приймач батчами `FileRecord`, з побудовою шляхів (T-022) і
/// класифікацією (T-023). Два проходи MFT: перший збирає директорії для
/// резолвера шляхів, другий стрімить файли в приймач батчами. Пам'ять
/// обмежена картою директорій + одним батчем (зворотний тиск).
#[cfg(windows)]
pub fn scan_volume_to_index<F>(
    drive: char,
    batch_size: usize,
    sink: F,
) -> Result<IndexingStats, CoreError>
where
    F: FnMut(Vec<FileRecord>) -> Result<(), CoreError>,
{
    // Прохід 1: тільки директорії — вони потрібні резолверу шляхів.
    let mut dirs = Vec::new();
    crate::volume::enumerate_with(drive, |e| {
        if e.is_directory {
            dirs.push(e);
        }
    })?;
    let resolver = PathResolver::from_entries(drive, &dirs);
    drop(dirs); // резолвер тримає власну копію — вихідний вектор більше не потрібен

    // Прохід 2: файли → батчі у приймач (сток паузить продюсера = зворотний тиск).
    let mut batcher = Batcher::new(&resolver, batch_size, sink);
    crate::volume::enumerate_with(drive, |e| batcher.push(e))?;
    batcher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use trashradar_domain::candidate::{ByteSize, FileAttributes};

    fn entry(file_ref: u64, parent_ref: u64, name: &str, is_dir: bool, size: u64) -> ScanEntry {
        ScanEntry {
            file_ref,
            parent_ref,
            name: name.to_string(),
            size: ByteSize(size),
            created_at: None,
            modified_at: None,
            accessed_at: None,
            is_directory: is_dir,
            attributes: FileAttributes::default(),
        }
    }

    /// Резолвер із коренем (5) → тека Media (10).
    fn resolver() -> PathResolver {
        let dirs = vec![entry(5, 5, ".", true, 0), entry(10, 5, "Media", true, 0)];
        PathResolver::from_entries('C', &dirs)
    }

    #[test]
    fn to_file_record_sets_path_kind_and_uncategorized() {
        let e = entry(200, 10, "clip.mp4", false, 4096);
        let rec = to_file_record(&e, "C:\\Media\\clip.mp4".to_string(), 7);
        assert_eq!(rec.candidate_id, CandidateId(7));
        assert_eq!(rec.path, "C:\\Media\\clip.mp4");
        assert_eq!(rec.size, ByteSize(4096));
        assert_eq!(rec.kind, FileKind::Video);
        assert_eq!(rec.unit, CandidateUnit::File);
        assert_eq!(rec.category, CategoryId::Uncategorized);
        assert_eq!(rec.decision, Decision::Undecided);
    }

    #[test]
    fn batches_flush_at_size_and_remainder_on_finish() {
        let resolver = resolver();
        let batches: RefCell<Vec<usize>> = RefCell::new(Vec::new());
        let mut batcher = Batcher::new(&resolver, 3, |b: Vec<FileRecord>| {
            batches.borrow_mut().push(b.len());
            Ok(())
        });
        // 7 файлів у теці Media → батчі 3,3,1.
        for i in 0..7 {
            batcher.push(entry(100 + i, 10, &format!("f{i}.bin"), false, 10));
        }
        let stats = batcher.finish().unwrap();
        assert_eq!(*batches.borrow(), vec![3, 3, 1]);
        assert!(
            batches.borrow().iter().all(|&n| n <= 3),
            "жоден батч не > batch_size"
        );
        assert_eq!(stats.files_indexed, 7);
        assert_eq!(stats.batches, 3);
    }

    #[test]
    fn candidate_ids_are_sequential_across_batches() {
        let resolver = resolver();
        let ids: RefCell<Vec<u64>> = RefCell::new(Vec::new());
        let mut batcher = Batcher::new(&resolver, 2, |b: Vec<FileRecord>| {
            for r in &b {
                ids.borrow_mut().push(r.candidate_id.0);
            }
            Ok(())
        });
        for i in 0..5 {
            batcher.push(entry(100 + i, 10, &format!("f{i}.bin"), false, 1));
        }
        batcher.finish().unwrap();
        assert_eq!(*ids.borrow(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn directories_and_unresolvable_are_skipped() {
        let resolver = resolver();
        let count = RefCell::new(0u64);
        let mut batcher = Batcher::new(&resolver, 10, |b: Vec<FileRecord>| {
            *count.borrow_mut() += b.len() as u64;
            Ok(())
        });
        batcher.push(entry(11, 5, "Docs", true, 0)); // директорія — ігнор
        batcher.push(entry(200, 10, "ok.bin", false, 5)); // ок
        batcher.push(entry(201, 999, "orphan.bin", false, 5)); // немає батька → skip
        let stats = batcher.finish().unwrap();
        assert_eq!(*count.borrow(), 1);
        assert_eq!(stats.files_indexed, 1);
        assert_eq!(stats.skipped_no_path, 1);
    }

    #[test]
    fn sink_error_stops_and_propagates() {
        let resolver = resolver();
        let mut batcher = Batcher::new(&resolver, 1, |_b: Vec<FileRecord>| {
            Err(CoreError::internal("сток впав"))
        });
        batcher.push(entry(200, 10, "a.bin", false, 1));
        batcher.push(entry(201, 10, "b.bin", false, 1));
        let res = batcher.finish();
        assert!(res.is_err());
    }
}
