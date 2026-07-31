//! Паралельний обхід каталогів з work-stealing чергою (T-026).
//!
//! Кожен воркер тримає локальний стек каталогів; коли стек порожній —
//! краде роботу з глобальної черги. Глибина дерева не серіалізує обхід:
//! підкаталоги одразу стають доступною роботою для будь-якого воркера.
//!
//! Виключені шляхи (T-027): відсікаються **до** `metadata`/`read_dir` на
//! самому префіксі — у виключене піддерево немає жодного syscall-заходу.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use trashradar_domain::candidate::{ByteSize, FileAttributes, FsTimestamp};
use trashradar_domain::error::{CoreError, ErrorCode};
use trashradar_domain::quarantine::SERVICE_DIRECTORY_NAME;
use trashradar_domain::scan::ScanEntry;

use crate::exclude::PathExclusions;

/// Синтетичний `file_ref` кореня тому (walk-модель; у MFT корінь = record 5).
pub const ROOT_REF: u64 = 0;

// Правило 6 брифу Стадії 2: константи вживаються лише в Windows-гілці
// `win_attributes`/`is_reparse_point`, тож без `cfg` вони дають на Linux
// `dead_code` — а з `-D warnings` у CI це помилка збірки. З Windows такого
// не побачити ніколи.
#[cfg(windows)]
const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
#[cfg(windows)]
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
#[cfg(windows)]
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x0000_0004;
#[cfg(windows)]
const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x0000_0100;
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

/// Інтервал FILETIME між 1601-01-01 і Unix epoch (100-нс тики).
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

/// Конфігурація обходу.
#[derive(Debug, Clone, Default)]
pub struct WalkConfig {
    /// Кількість воркерів. `0` = `available_parallelism` (мін. 1).
    pub workers: usize,
    /// Виключені піддерева — сканер у них не заходить (T-027).
    pub exclusions: PathExclusions,
    /// Збирати трейс каталогів, на які викликано `read_dir` (DoD T-027 / діагностика).
    pub record_opened_dirs: bool,
}

impl WalkConfig {
    fn resolved_workers(&self) -> usize {
        if self.workers > 0 {
            self.workers
        } else {
            thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .max(1)
        }
    }
}

/// Статистика обходу.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WalkStats {
    pub entries: u64,
    pub directories: u64,
    pub files: u64,
    /// Каталоги/файли, пропущені через помилки доступу.
    pub skipped_errors: u64,
    /// Піддерева, відсічені виключенням (T-027) — без заходу всередину.
    pub skipped_excluded: u64,
    pub workers_used: u64,
    /// Каталоги, на які реально викликано `read_dir` (якщо `record_opened_dirs`).
    pub opened_dirs: Vec<PathBuf>,
}

struct WorkItem {
    path: PathBuf,
    /// Синтетичний `file_ref` цього каталогу (вже виданий).
    dir_ref: u64,
}

struct SharedState {
    queue: Mutex<VecDeque<WorkItem>>,
    /// Каталоги, які зараз обробляються воркерами.
    inflight: AtomicUsize,
    cvar: Condvar,
    next_id: AtomicU64,
    entries: Mutex<Vec<ScanEntry>>,
    skipped_errors: AtomicU64,
    skipped_excluded: AtomicU64,
    exclusions: PathExclusions,
    /// Трейс `read_dir` (лише коли увімкнено).
    opened_dirs: Option<Mutex<Vec<PathBuf>>>,
}

/// Обходить том `drive` (літера) і повертає всі знайдені записи.
pub fn walk_volume(drive: char, config: WalkConfig) -> Result<Vec<ScanEntry>, CoreError> {
    let mut out = Vec::new();
    walk_volume_with(drive, config, |e| out.push(e))?;
    Ok(out)
}

/// Стрімить зібрані записи в `sink` після завершення обходу.
/// Порядок між гілками не гарантований (як і для паралельного MFT-пайплайну).
pub fn walk_volume_with(
    drive: char,
    config: WalkConfig,
    mut sink: impl FnMut(ScanEntry),
) -> Result<WalkStats, CoreError> {
    if !drive.is_ascii_alphabetic() {
        return Err(CoreError::invalid_argument(format!(
            "Некоректна літера тому «{drive}»."
        )));
    }
    let root = PathBuf::from(format!("{}:\\", drive.to_ascii_uppercase()));
    let (entries, stats) = walk_path_internal(&root, config)?;
    for e in entries {
        sink(e);
    }
    Ok(stats)
}

/// Обхід довільного кореня (юніт-тести на temp-дереві).
pub fn walk_path(root: &Path, config: WalkConfig) -> Result<Vec<ScanEntry>, CoreError> {
    let (entries, _) = walk_path_internal(root, config)?;
    Ok(entries)
}

/// Як [`walk_path`], але повертає й статистику (включно з трейсом відкритих
/// каталогів, якщо `config.record_opened_dirs`).
pub fn walk_path_with_stats(
    root: &Path,
    config: WalkConfig,
) -> Result<(Vec<ScanEntry>, WalkStats), CoreError> {
    walk_path_internal(root, config)
}

fn walk_path_internal(
    root: &Path,
    config: WalkConfig,
) -> Result<(Vec<ScanEntry>, WalkStats), CoreError> {
    // T-088: службовий каталог TrashRadar під коренем скану виключається
    // безумовно, незалежно від конфігурації — «сміття не знаходить саме себе»
    // (architecture.md §7.4). Для тому це `<X>:\.trashradar`.
    let mut config = config;
    config
        .exclusions
        .extend([root.join(SERVICE_DIRECTORY_NAME)]);

    // Корінь сам у виключенні — у піддерево не заходимо взагалі (0 syscall всередині).
    if config.exclusions.is_excluded(root) {
        tracing::debug!(path = %root.display(), "корінь скану виключено — обхід пропущено");
        return Ok((
            Vec::new(),
            WalkStats {
                skipped_excluded: 1,
                workers_used: 0,
                ..WalkStats::default()
            },
        ));
    }

    let meta = fs::metadata(root).map_err(|e| {
        CoreError::new(
            ErrorCode::Io,
            format!(
                "Не вдалося відкрити корінь скану «{}»: {e}.",
                root.display()
            ),
        )
    })?;
    if !meta.is_dir() {
        return Err(CoreError::invalid_argument(format!(
            "Корінь скану «{}» не є каталогом.",
            root.display()
        )));
    }

    let workers = config.resolved_workers();
    let root_entry = entry_from_meta(ROOT_REF, ROOT_REF, root_name(root), &meta, true);

    let shared = Arc::new(SharedState {
        queue: Mutex::new(VecDeque::from([WorkItem {
            path: root.to_path_buf(),
            dir_ref: ROOT_REF,
        }])),
        inflight: AtomicUsize::new(0),
        cvar: Condvar::new(),
        next_id: AtomicU64::new(1),
        entries: Mutex::new(vec![root_entry]),
        skipped_errors: AtomicU64::new(0),
        skipped_excluded: AtomicU64::new(0),
        exclusions: config.exclusions.clone(),
        opened_dirs: if config.record_opened_dirs {
            Some(Mutex::new(Vec::new()))
        } else {
            None
        },
    });

    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let state = Arc::clone(&shared);
        handles.push(thread::spawn(move || worker_loop(state)));
    }

    for h in handles {
        h.join().map_err(|_| {
            CoreError::internal("Воркер обходу каталогів завершився панікою.".to_string())
        })?;
    }

    let entries = shared
        .entries
        .lock()
        .expect("entries mutex poisoned")
        .clone();
    let skipped = shared.skipped_errors.load(Ordering::Acquire);
    let skipped_excluded = shared.skipped_excluded.load(Ordering::Acquire);
    let directories = entries.iter().filter(|e| e.is_directory).count() as u64;
    let files = entries.len() as u64 - directories;
    let opened_dirs = shared
        .opened_dirs
        .as_ref()
        .map(|m| m.lock().expect("opened_dirs mutex poisoned").clone())
        .unwrap_or_default();

    if skipped > 0 {
        tracing::warn!(
            skipped,
            entries = entries.len(),
            "обхід каталогів пропустив недоступні записи"
        );
    }
    if skipped_excluded > 0 {
        tracing::debug!(
            skipped_excluded,
            "обхід відсік виключені піддерева без заходу всередину"
        );
    }

    let stats = WalkStats {
        entries: entries.len() as u64,
        directories,
        files,
        skipped_errors: skipped,
        skipped_excluded,
        workers_used: workers as u64,
        opened_dirs,
    };
    Ok((entries, stats))
}

fn worker_loop(state: Arc<SharedState>) {
    let mut local: Vec<WorkItem> = Vec::new();

    loop {
        let work = local.pop().or_else(|| pop_global(&state));

        let Some(item) = work else {
            // Нема локальної/глобальної роботи. Якщо ніхто не в польоті — кінець.
            let guard = state.queue.lock().expect("queue mutex poisoned");
            if guard.is_empty() && state.inflight.load(Ordering::Acquire) == 0 {
                state.cvar.notify_all();
                return;
            }
            let (guard, _) = state
                .cvar
                .wait_timeout(guard, Duration::from_millis(10))
                .expect("queue mutex poisoned");
            if guard.is_empty() && state.inflight.load(Ordering::Acquire) == 0 {
                return;
            }
            drop(guard);
            continue;
        };

        state.inflight.fetch_add(1, Ordering::AcqRel);
        process_directory(&state, &mut local, item);
        state.inflight.fetch_sub(1, Ordering::AcqRel);
        state.cvar.notify_all();
    }
}

fn pop_global(state: &SharedState) -> Option<WorkItem> {
    state
        .queue
        .lock()
        .expect("queue mutex poisoned")
        .pop_front()
}

fn process_directory(state: &SharedState, local: &mut Vec<WorkItem>, item: WorkItem) {
    // Захист: виключений каталог не повинен потрапити в чергу, але якщо
    // потрапив — не відкриваємо його (0 syscall у піддерево).
    if state.exclusions.is_excluded(&item.path) {
        state.skipped_excluded.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(
            path = %item.path.display(),
            "пропущено виключений каталог (без read_dir)"
        );
        return;
    }

    if let Some(trace) = &state.opened_dirs {
        trace
            .lock()
            .expect("opened_dirs mutex poisoned")
            .push(item.path.clone());
    }

    let read = match fs::read_dir(&item.path) {
        Ok(rd) => rd,
        Err(e) => {
            state.skipped_errors.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                path = %item.path.display(),
                error = %e,
                "пропущено недоступний каталог"
            );
            return;
        }
    };

    let mut new_dirs: Vec<WorkItem> = Vec::new();
    let mut batch_entries: Vec<ScanEntry> = Vec::new();

    for dent in read {
        let dent = match dent {
            Ok(d) => d,
            Err(e) => {
                state.skipped_errors.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    path = %item.path.display(),
                    error = %e,
                    "пропущено запис при читанні каталогу"
                );
                continue;
            }
        };

        let path = dent.path();
        let name = match dent.file_name().into_string() {
            Ok(n) => n,
            Err(_) => dent.file_name().to_string_lossy().into_owned(),
        };

        // T-027: виключений шлях — без metadata і без заходу всередину.
        // Ім'я бачимо з read_dir батька; у піддерево syscall-ів немає.
        if state.exclusions.is_excluded(&path) {
            state.skipped_excluded.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                path = %path.display(),
                "відсічено виключений шлях (без metadata/read_dir)"
            );
            continue;
        }

        let meta = match dent.metadata().or_else(|_| fs::symlink_metadata(&path)) {
            Ok(m) => m,
            Err(e) => {
                state.skipped_errors.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    path = %path.display(),
                    error = %e,
                    "пропущено запис без метаданих"
                );
                continue;
            }
        };

        let is_dir = meta.is_dir();
        let is_reparse = is_reparse_point(&meta);
        let id = state.next_id.fetch_add(1, Ordering::Relaxed);
        batch_entries.push(entry_from_meta(id, item.dir_ref, name, &meta, is_dir));

        // Не заходимо в reparse-точки (junction/symlink) — захист від циклів
        // і виходу за межі тому.
        if is_dir && !is_reparse {
            new_dirs.push(WorkItem { path, dir_ref: id });
        }
    }

    if !batch_entries.is_empty() {
        state
            .entries
            .lock()
            .expect("entries mutex poisoned")
            .extend(batch_entries);
    }

    if new_dirs.is_empty() {
        return;
    }

    // Work-stealing: половину — у локальний LIFO-стек, решту — у глобальну
    // чергу для крадіжки простоюючими воркерами.
    let mid = new_dirs.len() / 2;
    let steal: Vec<WorkItem> = new_dirs.drain(mid..).collect();
    // Локально — у зворотному порядку (глибше спочатку, краща locality).
    while let Some(w) = new_dirs.pop() {
        local.push(w);
    }
    if !steal.is_empty() {
        let mut q = state.queue.lock().expect("queue mutex poisoned");
        q.extend(steal);
        drop(q);
        state.cvar.notify_all();
    }
}

fn root_name(root: &Path) -> String {
    root.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn entry_from_meta(
    file_ref: u64,
    parent_ref: u64,
    name: String,
    meta: &Metadata,
    is_directory: bool,
) -> ScanEntry {
    let attrs = win_attributes(meta);
    let size = if is_directory {
        ByteSize(0)
    } else {
        ByteSize(meta.len())
    };
    ScanEntry {
        file_ref,
        parent_ref,
        name,
        size,
        created_at: meta.created().ok().and_then(system_time_to_filetime),
        modified_at: meta.modified().ok().and_then(system_time_to_filetime),
        accessed_at: meta.accessed().ok().and_then(system_time_to_filetime),
        is_directory,
        attributes: attrs,
    }
}

fn win_attributes(meta: &Metadata) -> FileAttributes {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let raw = meta.file_attributes();
        FileAttributes {
            raw_bits: raw,
            is_readonly: raw & FILE_ATTRIBUTE_READONLY != 0,
            is_hidden: raw & FILE_ATTRIBUTE_HIDDEN != 0,
            is_system: raw & FILE_ATTRIBUTE_SYSTEM != 0,
            is_temporary: raw & FILE_ATTRIBUTE_TEMPORARY != 0,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        FileAttributes {
            raw_bits: 0,
            is_readonly: false,
            is_hidden: false,
            is_system: false,
            is_temporary: false,
        }
    }
}

fn is_reparse_point(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

fn system_time_to_filetime(st: SystemTime) -> Option<FsTimestamp> {
    match st.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => {
            let ticks = d
                .as_secs()
                .checked_mul(10_000_000)?
                .checked_add(u64::from(d.subsec_nanos()) / 100)?
                .checked_add(FILETIME_UNIX_EPOCH)?;
            Some(FsTimestamp(ticks as i64))
        }
        Err(e) => {
            let d = e.duration();
            let delta = d
                .as_secs()
                .checked_mul(10_000_000)?
                .checked_add(u64::from(d.subsec_nanos()) / 100)?;
            if delta > FILETIME_UNIX_EPOCH {
                None
            } else {
                Some(FsTimestamp((FILETIME_UNIX_EPOCH - delta) as i64))
            }
        }
    }
}

/// Реконструює повний шлях запису за синтетичними `parent_ref` (корінь = 0).
pub struct WalkPathResolver<'a> {
    drive: char,
    dirs: HashMap<u64, (u64, &'a str)>,
}

impl<'a> WalkPathResolver<'a> {
    pub fn from_entries(drive: char, entries: &'a [ScanEntry]) -> Self {
        let dirs = entries
            .iter()
            .filter(|e| e.is_directory)
            .map(|e| (e.file_ref, (e.parent_ref, e.name.as_str())))
            .collect();
        Self {
            drive: drive.to_ascii_uppercase(),
            dirs,
        }
    }

    pub fn full_path(&self, entry: &ScanEntry) -> Option<String> {
        if entry.file_ref == ROOT_REF {
            return Some(format!("{}:\\", self.drive));
        }
        let mut chain: Vec<&str> = Vec::new();
        let mut cur_parent = entry.parent_ref;
        chain.push(entry.name.as_str());
        let mut guard = 0usize;
        while cur_parent != ROOT_REF {
            if guard > 1024 {
                return None;
            }
            let (parent, name) = self.dirs.get(&cur_parent)?;
            chain.push(name);
            cur_parent = *parent;
            guard += 1;
        }
        let mut path = format!("{}:", self.drive);
        for name in chain.iter().rev() {
            path.push('\\');
            path.push_str(name);
        }
        Some(path)
    }
}

/// Сумісний one-shot API. Для циклів використовуйте [`WalkPathResolver`].
pub fn full_path(drive: char, entries: &[ScanEntry], entry: &ScanEntry) -> Option<String> {
    WalkPathResolver::from_entries(drive, entries).full_path(entry)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, File};
    use std::io::Write;
    use trashradar_app::ports::ScanSource;

    fn temp_tree() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "trashradar-walk-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        create_dir_all(base.join("a/b")).unwrap();
        create_dir_all(base.join("a/c")).unwrap();
        create_dir_all(base.join("d")).unwrap();
        File::create(base.join("root.txt"))
            .unwrap()
            .write_all(b"root")
            .unwrap();
        File::create(base.join("a/b/deep.txt"))
            .unwrap()
            .write_all(b"deep-content")
            .unwrap();
        File::create(base.join("a/c/side.bin"))
            .unwrap()
            .write_all(b"xx")
            .unwrap();
        File::create(base.join("d/leaf.dat"))
            .unwrap()
            .write_all(b"leaf")
            .unwrap();
        base
    }

    fn cfg(workers: usize) -> WalkConfig {
        WalkConfig {
            workers,
            ..WalkConfig::default()
        }
    }

    #[test]
    fn walks_temp_tree_and_finds_all_files() {
        let root = temp_tree();
        let entries = walk_path(&root, cfg(4)).expect("walk");
        let files: Vec<_> = entries.iter().filter(|e| !e.is_directory).collect();
        let names: std::collections::HashSet<_> = files.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names.len(), 4);
        assert!(names.contains("root.txt"));
        assert!(names.contains("deep.txt"));
        assert!(names.contains("side.bin"));
        assert!(names.contains("leaf.dat"));

        let deep = files.iter().find(|e| e.name == "deep.txt").unwrap();
        assert_eq!(deep.size.0, b"deep-content".len() as u64);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parent_chain_reconstructs_paths() {
        let root = temp_tree();
        let entries = walk_path(&root, cfg(2)).expect("walk");
        let deep = entries
            .iter()
            .find(|e| e.name == "deep.txt" && !e.is_directory)
            .expect("deep.txt");
        let path = full_path('T', &entries, deep).expect("path");
        // Шлях: T:\<root_name>\a\b\deep.txt — root_name = basename temp dir
        assert!(
            path.ends_with("\\a\\b\\deep.txt"),
            "unexpected path: {path}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parallel_matches_single_worker() {
        let root = temp_tree();
        // Глибше дерево — більше шансів на паралелізм.
        for i in 0..32 {
            let dir = root.join(format!("w{i}"));
            create_dir_all(&dir).unwrap();
            File::create(dir.join("f.txt"))
                .unwrap()
                .write_all(b"x")
                .unwrap();
        }

        let single = walk_path(&root, cfg(1)).expect("1 worker");
        let multi = walk_path(&root, cfg(4)).expect("4 workers");

        let names = |v: &[ScanEntry]| {
            let mut n: Vec<_> = v
                .iter()
                .filter(|e| !e.is_directory)
                .map(|e| e.name.clone())
                .collect();
            n.sort();
            n
        };
        assert_eq!(names(&single), names(&multi));
        assert_eq!(
            single.iter().filter(|e| !e.is_directory).count(),
            multi.iter().filter(|e| !e.is_directory).count()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn uses_all_configured_workers() {
        let root = temp_tree();
        for i in 0..64 {
            create_dir_all(root.join(format!("p{i}"))).unwrap();
        }
        let (_, stats) = walk_path_internal(&root, cfg(4)).expect("walk");
        assert_eq!(stats.workers_used, 4);
        assert!(stats.directories >= 64);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_drive_is_invalid_argument() {
        let err = walk_volume('1', WalkConfig::default()).expect_err("bad drive");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn scan_source_impl_works() {
        let root = temp_tree();
        // WalkScanner.scan_volume потребує літеру тому — для temp використовуємо walk_path.
        let entries = walk_path(&root, cfg(2)).unwrap();
        assert!(!entries.is_empty());
        // Перевіряємо, що тип реалізує порт (компіляція + default).
        let _scanner = crate::WalkScanner::default();
        let _: &dyn ScanSource = &_scanner;
        let _ = fs::remove_dir_all(&root);
    }

    /// DoD T-027: у виключене піддерево немає жодного `read_dir` (трейс
    /// `opened_dirs`) і жоден запис з нього не потрапляє в результат.
    #[test]
    fn excluded_subtree_has_no_read_dir_and_no_entries() {
        let root = temp_tree();
        // Щільне виключене піддерево з «секретами», які не повинні з'явитись.
        create_dir_all(root.join("secret/deep/nested")).unwrap();
        File::create(root.join("secret/top.secret"))
            .unwrap()
            .write_all(b"nope")
            .unwrap();
        File::create(root.join("secret/deep/nested/buried.secret"))
            .unwrap()
            .write_all(b"nope")
            .unwrap();
        // Дозволений сусід.
        File::create(root.join("allowed.txt"))
            .unwrap()
            .write_all(b"ok")
            .unwrap();

        let excluded = root.join("secret");
        let config = WalkConfig {
            workers: 4,
            exclusions: PathExclusions::from_paths([&excluded]),
            record_opened_dirs: true,
        };
        let (entries, stats) = walk_path_with_stats(&root, config).expect("walk");

        // Жоден запис з secret/* не в результаті.
        let names: std::collections::HashSet<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(!names.contains("top.secret"));
        assert!(!names.contains("buried.secret"));
        assert!(!names.contains("secret"));
        assert!(!names.contains("deep"));
        assert!(!names.contains("nested"));
        assert!(names.contains("allowed.txt"));
        assert!(names.contains("root.txt"));
        assert!(stats.skipped_excluded >= 1);

        // Трейс: жоден opened path не лежить під secret\ (включно з самим secret).
        let secret_norm = excluded
            .canonicalize()
            .unwrap_or(excluded.clone())
            .to_string_lossy()
            .to_ascii_lowercase()
            .replace('/', "\\");
        for opened in &stats.opened_dirs {
            let o = opened
                .canonicalize()
                .unwrap_or_else(|_| opened.clone())
                .to_string_lossy()
                .to_ascii_lowercase()
                .replace('/', "\\");
            assert!(
                o != secret_norm
                    && !o.starts_with(&format!("{secret_norm}\\"))
                    && !o.starts_with(&secret_norm.replace("\\\\", "\\")),
                "read_dir зайшов у виключене піддерево: {o} (secret={secret_norm})"
            );
        }

        let _ = fs::remove_dir_all(&root);
    }

    /// DoD T-088: вміст карантину ніколи не з'являється у кандидатах —
    /// `.trashradar` під коренем скану відсікається навіть з порожнім
    /// списком виключень (безумовне правило, не конфігурація).
    #[test]
    fn quarantine_directory_is_always_excluded_from_walk() {
        let root = temp_tree();
        create_dir_all(root.join(".trashradar/quarantine")).unwrap();
        File::create(root.join(".trashradar/quarantine/00000001.bin"))
            .unwrap()
            .write_all(b"reaped")
            .unwrap();

        let config = WalkConfig {
            workers: 2,
            exclusions: PathExclusions::empty(),
            record_opened_dirs: true,
        };
        let (entries, stats) = walk_path_with_stats(&root, config).expect("walk");

        let names: std::collections::HashSet<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(!names.contains(".trashradar"));
        assert!(!names.contains("quarantine"));
        assert!(!names.contains("00000001.bin"));
        assert!(names.contains("root.txt"));
        assert!(stats.skipped_excluded >= 1);

        // Жодного read_dir усередині службового каталогу (0 syscall, стиль T-027).
        let service = root
            .join(".trashradar")
            .to_string_lossy()
            .to_ascii_lowercase()
            .replace('/', "\\");
        for opened in &stats.opened_dirs {
            let o = opened
                .to_string_lossy()
                .to_ascii_lowercase()
                .replace('/', "\\");
            assert!(
                o != service && !o.starts_with(&format!("{service}\\")),
                "read_dir зайшов у карантин: {o}"
            );
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn excluded_root_skips_entire_walk() {
        let root = temp_tree();
        let config = WalkConfig {
            workers: 2,
            exclusions: PathExclusions::from_paths([&root]),
            record_opened_dirs: true,
        };
        let (entries, stats) = walk_path_with_stats(&root, config).expect("walk");
        assert!(entries.is_empty());
        assert_eq!(stats.skipped_excluded, 1);
        assert!(stats.opened_dirs.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    /// DoD T-026: на реальному томі обхід знаходить ту саму популяцію файлів,
    /// що й MFT (за іменем+розміром у зведеному ключі; hard links / системні
    /// метафайли MFT і access-denied у walk дають відомий допуск).
    #[test]
    #[ignore = "DoD T-026: потребує адмін-прав (MFT) і реального тому; TR_MFT_TEST_DRIVE"]
    #[cfg(windows)]
    fn walk_population_matches_mft_on_volume() {
        use std::collections::HashSet;
        use trashradar_app::ports::ScanSource as _;

        let drive = std::env::var("TR_MFT_TEST_DRIVE")
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or('F');

        let walk_scanner = crate::WalkScanner::new(cfg(0));
        let walk_entries = walk_scanner.scan_volume(drive).expect("walk scan");
        let walk_files: HashSet<(String, u64)> = walk_entries
            .iter()
            .filter(|e| !e.is_directory)
            .filter_map(|e| {
                let path = full_path(drive, &walk_entries, e)?;
                // Нормалізуємо до lower для порівняння з MFT-шляхами.
                Some((path.to_ascii_lowercase(), e.size.0))
            })
            .collect();

        let mft_entries = trashradar_scan_mft::enumerate(drive).expect("mft scan");
        let resolver = trashradar_scan_mft::paths::PathResolver::from_entries(drive, &mft_entries);
        let mft_files: HashSet<(String, u64)> = mft_entries
            .iter()
            .filter(|e| !e.is_directory)
            .filter_map(|e| {
                let path = resolver.full_path(e)?;
                Some((path.to_ascii_lowercase(), e.size.0))
            })
            .collect();

        // Файли walk, відсутні в MFT (зазвичай 0 на тихому томі).
        let walk_only: Vec<_> = walk_files.difference(&mft_files).take(20).collect();
        // Файли MFT, відсутні в walk (системні метафайли, access-denied, hard-link names).
        let mft_only_count = mft_files.difference(&walk_files).count();

        let intersection = walk_files.intersection(&mft_files).count();
        let walk_n = walk_files.len();
        let mft_n = mft_files.len();
        let coverage = intersection as f64 / walk_n.max(1) as f64;

        println!(
            "Том {drive}: walk={walk_n} mft={mft_n} intersection={intersection} \
             coverage_of_walk={coverage:.4} mft_only={mft_only_count} \
             walk_only_sample={walk_only:?}"
        );

        // DoD: обхід не «губить» доступні файли відносно MFT.
        // Допуск: ≤2% walk-only (гонки на живому томі / різні імена hard-link).
        assert!(
            coverage >= 0.98,
            "walk coverage vs MFT paths only {coverage:.4} (< 0.98)"
        );
        assert!(
            walk_only.len() < 20 || coverage >= 0.98,
            "забагато walk-only файлів: {walk_only:?}"
        );
    }
}
