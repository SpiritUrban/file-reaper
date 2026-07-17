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
use trashradar_domain::quarantine::is_under_service_directory;
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
    /// Пропущено з-під `<том>\.trashradar` — карантин не сканується (T-088).
    pub skipped_quarantine: u64,
    /// Пошкоджені записи MFT, пропущені з логом (T-025).
    pub corrupt: u64,
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
        // T-088: вміст карантину ніколи не стає кандидатом (architecture.md §7.4).
        if is_under_service_directory(&path) {
            self.stats.skipped_quarantine += 1;
            return;
        }
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
    let (stats, _cancelled) = scan_volume_to_index_cancel(drive, batch_size, || false, sink)?;
    Ok(stats)
}

/// Прогрес MFT-скану для UI (pass 1 = карта тек, pass 2 = файли в індекс).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MftIndexProgress {
    /// 1 = читання MFT (директорії), 2 = індексація файлів.
    pub pass: u8,
    pub records_scanned: u64,
    pub records_total: u64,
    /// Файлів уже здано sink-ом (лише pass 2).
    pub files_emitted: u64,
}

/// Як [`scan_volume_to_index`], з кооперативною відміною (T-033).
/// Повертає `(stats, cancelled)` — часткові батчі вже в `sink`.
#[cfg(windows)]
pub fn scan_volume_to_index_cancel<F>(
    drive: char,
    batch_size: usize,
    is_cancelled: impl Fn() -> bool,
    sink: F,
) -> Result<(IndexingStats, bool), CoreError>
where
    F: FnMut(Vec<FileRecord>) -> Result<(), CoreError>,
{
    scan_volume_to_index_cancel_progress(drive, batch_size, is_cancelled, |_| {}, |_| {}, sink)
}

/// Як [`scan_volume_to_index_cancel`], з `on_progress` після кожного chunk MFT
/// — щоб UI не зависав на «0 файлів» під час довгого pass 1 — та `on_dir`, який
/// отримує **повний шлях кожної директорії** тому (для розділів «Порожні/майже
/// порожні папки»: порожня папка не має файлів і невидима файловому конвеєру).
#[cfg(windows)]
pub fn scan_volume_to_index_cancel_progress<F, P, D>(
    drive: char,
    batch_size: usize,
    is_cancelled: impl Fn() -> bool,
    mut on_progress: P,
    mut on_dir: D,
    mut sink: F,
) -> Result<(IndexingStats, bool), CoreError>
where
    F: FnMut(Vec<FileRecord>) -> Result<(), CoreError>,
    P: FnMut(MftIndexProgress),
    D: FnMut(String),
{
    // Прохід 1: тільки директорії — вони потрібні резолверу шляхів.
    let mut dirs = Vec::new();
    let (pass1, cancelled1) = crate::volume::enumerate_with_cancel_progress(
        drive,
        &is_cancelled,
        |e| {
            if e.is_directory {
                dirs.push(e);
            }
        },
        |scanned, total| {
            on_progress(MftIndexProgress {
                pass: 1,
                records_scanned: scanned,
                records_total: total,
                files_emitted: 0,
            });
        },
    )?;
    if cancelled1 {
        return Ok((
            IndexingStats {
                corrupt: pass1.corrupt,
                ..IndexingStats::default()
            },
            true,
        ));
    }
    let resolver = PathResolver::from_entries(drive, &dirs);
    // Винести повні шляхи всіх директорій (incl. порожніх) до resolver-drop:
    // caller відфільтрує виключення й агрегує порожні/майже порожні папки.
    for dir in &dirs {
        if let Some(path) = resolver.full_path(dir) {
            on_dir(path);
        }
    }
    drop(dirs);

    // Прохід 2: файли → батчі. Рахуємо files_emitted у обгортці sink.
    let files_emitted = std::cell::Cell::new(0u64);
    let mut batcher = Batcher::new(&resolver, batch_size, |batch: Vec<FileRecord>| {
        let n = batch.len() as u64;
        let result = sink(batch);
        if result.is_ok() {
            files_emitted.set(files_emitted.get().saturating_add(n));
        }
        result
    });

    let (scan_stats, cancelled2) = crate::volume::enumerate_with_cancel_progress(
        drive,
        &is_cancelled,
        |e| batcher.push(e),
        |scanned, total| {
            on_progress(MftIndexProgress {
                pass: 2,
                records_scanned: scanned,
                records_total: total,
                files_emitted: files_emitted.get(),
            });
        },
    )?;
    let mut stats = batcher.finish()?;
    stats.corrupt = scan_stats.corrupt;
    // Фінальний тік — UI бачить 100% MFT і точний files_emitted.
    on_progress(MftIndexProgress {
        pass: 2,
        records_scanned: scan_stats.records_scanned,
        records_total: scan_stats.records_scanned.max(1),
        files_emitted: stats.files_indexed,
    });
    Ok((stats, cancelled2))
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

    /// DoD T-088: вміст `<том>\.trashradar` ніколи не з'являється у кандидатах.
    #[test]
    fn quarantine_service_directory_is_never_indexed() {
        let dirs = vec![
            entry(5, 5, ".", true, 0),
            entry(10, 5, "Media", true, 0),
            entry(20, 5, ".trashradar", true, 0),
            entry(21, 20, "quarantine", true, 0),
        ];
        let resolver = PathResolver::from_entries('C', &dirs);
        let paths: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let mut batcher = Batcher::new(&resolver, 10, |b: Vec<FileRecord>| {
            paths.borrow_mut().extend(b.into_iter().map(|r| r.path));
            Ok(())
        });
        batcher.push(entry(100, 21, "00000001.bin", false, 4096)); // у карантині
        batcher.push(entry(101, 20, "meta.json", false, 10)); // у службовому корені
        batcher.push(entry(102, 10, "clip.mp4", false, 2048)); // звичайний файл
        let stats = batcher.finish().unwrap();
        assert_eq!(*paths.borrow(), vec!["C:\\Media\\clip.mp4".to_string()]);
        assert_eq!(stats.files_indexed, 1);
        assert_eq!(stats.skipped_quarantine, 2);
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
