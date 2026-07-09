use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::ports::{PreviewCache, PreviewCacheEntry, PreviewKind};
use crate::workers::CancellationToken;
use trashradar_domain::candidate::{ByteSize, FsTimestamp};

/// Пріоритети для запитів генерації превью (docs/architecture.md §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PreviewPriority {
    /// Фонова генерація для топ-N найбільших кандидатів кожної категорії.
    P3 = 0,
    /// Префетч: 2–3 плитки за напрямом руху курсора / наступний скрол-екран.
    P2 = 1,
    /// Видимі плитки поточної сітки.
    P1 = 2,
    /// Файл під курсором у Live Preview (правий монітор).
    P0 = 3,
}

struct QueuedTask {
    path: String,
    priority: PreviewPriority,
    created_at: Instant,
    job: Box<dyn FnOnce(CancellationToken) + Send + 'static>,
}

struct RunningTask {
    priority: PreviewPriority,
    cancellation: CancellationToken,
}

struct SchedulerState {
    pending: Vec<QueuedTask>,
    running: HashMap<String, RunningTask>,
    worker_count: usize,
    shutdown: bool,
    aging_rate_per_ms: f64,
}

impl SchedulerState {
    fn select_next(&mut self) -> Option<QueuedTask> {
        if self.pending.is_empty() {
            return None;
        }

        let now = Instant::now();
        let mut best_index = 0;
        let mut best_score = -1.0;

        for (index, task) in self.pending.iter().enumerate() {
            let wait_ms = now.duration_since(task.created_at).as_millis() as f64;
            let base_score = match task.priority {
                PreviewPriority::P3 => 0.0,
                PreviewPriority::P2 => 1000.0,
                PreviewPriority::P1 => 2000.0,
                PreviewPriority::P0 => 3000.0,
            };
            let score = base_score + wait_ms * self.aging_rate_per_ms;
            if score > best_score {
                best_score = score;
                best_index = index;
            }
        }

        Some(self.pending.remove(best_index))
    }
}

/// Пріоритетний планувальник превью-запитів з підтримкою старіння,
/// скасування та витіснення (T-067).
pub struct PreviewScheduler {
    state: Arc<Mutex<SchedulerState>>,
    changed: Arc<Condvar>,
    workers: Vec<JoinHandle<()>>,
}

impl PreviewScheduler {
    /// Створити новий планувальник з заданою кількістю воркерів та коефіцієнтом старіння.
    /// `aging_rate_per_ms` визначає скільки балів пріоритету додається за 1 мс очікування.
    pub fn new(workers_count: usize, aging_rate_per_ms: f64) -> Self {
        let state = Arc::new(Mutex::new(SchedulerState {
            pending: Vec::new(),
            running: HashMap::new(),
            worker_count: workers_count.max(1),
            shutdown: false,
            aging_rate_per_ms,
        }));
        let changed = Arc::new(Condvar::new());
        let mut workers = Vec::with_capacity(workers_count);

        for _ in 0..workers_count {
            let worker_state = Arc::clone(&state);
            let worker_changed = Arc::clone(&changed);
            workers.push(thread::spawn(move || {
                worker_loop(worker_state, worker_changed)
            }));
        }

        Self {
            state,
            changed,
            workers,
        }
    }

    /// Додати задачу до черги.
    ///
    /// Якщо задача для цього шляху вже є в черзі очікування, вона замінюється.
    /// Якщо приходить задача пріоритету `P0` (Live Preview), а вільних потоків немає,
    /// планувальник негайно скасовує одне з активних фонових завдань `P3`.
    pub fn submit<F>(&self, path: String, priority: PreviewPriority, job: F)
    where
        F: FnOnce(CancellationToken) + Send + 'static,
    {
        let mut state = self.state.lock().unwrap();
        if state.shutdown {
            return;
        }

        // Замінюємо існуючу заплановану задачу для цього шляху
        state.pending.retain(|task| task.path != path);

        state.pending.push(QueuedTask {
            path: path.clone(),
            priority,
            created_at: Instant::now(),
            job: Box::new(job),
        });

        // Витіснення P3 задач при запиті P0
        if priority == PreviewPriority::P0 {
            let active_jobs = state.running.len();
            if active_jobs >= state.worker_count {
                let mut p3_to_cancel = None;
                for (running_path, running_task) in state.running.iter() {
                    if running_task.priority == PreviewPriority::P3 {
                        p3_to_cancel = Some(running_path.clone());
                        break;
                    }
                }
                if let Some(cancel_path) = p3_to_cancel {
                    if let Some(running) = state.running.get(&cancel_path) {
                        running.cancellation.cancel();
                    }
                }
            }
        }

        self.changed.notify_one();
    }

    /// Скасувати задачу для вказаного шляху (якщо вона в черзі або виконується).
    pub fn cancel(&self, path: &str) {
        let mut state = self.state.lock().unwrap();
        state.pending.retain(|task| task.path != path);
        if let Some(running) = state.running.get(path) {
            running.cancellation.cancel();
        }
    }

    /// Отримати кількість задач в черзі очікування.
    pub fn pending_count(&self) -> usize {
        let state = self.state.lock().unwrap();
        state.pending.len()
    }

    /// Отримати кількість задач, що виконуються в даний момент.
    pub fn running_count(&self) -> usize {
        let state = self.state.lock().unwrap();
        state.running.len()
    }

    /// Перевірити чи задача для вказаного шляху є в черзі або виконується.
    pub fn is_queued_or_running(&self, path: &str) -> bool {
        let state = self.state.lock().unwrap();
        state.pending.iter().any(|t| t.path == path) || state.running.contains_key(path)
    }
}

impl Drop for PreviewScheduler {
    fn drop(&mut self) {
        {
            let mut state = self.state.lock().unwrap();
            state.shutdown = true;
            for running in state.running.values() {
                running.cancellation.cancel();
            }
            state.pending.clear();
        }
        self.changed.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn worker_loop(state_lock: Arc<Mutex<SchedulerState>>, changed: Arc<Condvar>) {
    loop {
        let mut state = state_lock.lock().unwrap();
        let task = loop {
            if state.shutdown {
                return;
            }
            if let Some(t) = state.select_next() {
                break t;
            }
            state = changed.wait(state).unwrap();
        };

        let cancellation = CancellationToken::new();
        state.running.insert(
            task.path.clone(),
            RunningTask {
                priority: task.priority,
                cancellation: cancellation.clone(),
            },
        );
        drop(state);

        // Виконання задачі
        (task.job)(cancellation.clone());

        let mut state = state_lock.lock().unwrap();
        state.running.remove(&task.path);
        changed.notify_all();
    }
}

/// Дисковий кеш прев'ю інтегрований з базою даних (T-068).
pub struct PreviewCacheManager {
    cache_dir: PathBuf,
    db: Arc<dyn PreviewCache>,
}

impl PreviewCacheManager {
    /// Створити новий менеджер кешу прев'ю.
    pub fn new(cache_dir: PathBuf, db: Arc<dyn PreviewCache>) -> Self {
        let _ = std::fs::create_dir_all(&cache_dir);
        Self { cache_dir, db }
    }

    /// Отримати шлях до кешованого прев'ю, якщо воно є валідним за розміром та mtime джерела,
    /// і відповідний файл дійсно існує на диску.
    pub fn get_valid_preview_path(
        &self,
        source_path: &str,
        kind: PreviewKind,
        current_size: ByteSize,
        current_modified: Option<FsTimestamp>,
    ) -> Option<String> {
        let entry = self.db.get_preview_entry(source_path, kind).ok()??;
        if entry.is_valid_for(current_size, current_modified) {
            if Path::new(&entry.preview_path).exists() {
                Some(entry.preview_path)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Зберегти прев'ю на диск та зафіксувати посилання на нього в індексі.
    /// Ім'я файлу генерується детерміновано за допомогою хэшування шляху джерела.
    pub fn save_preview(
        &self,
        source_path: &str,
        kind: PreviewKind,
        current_size: ByteSize,
        current_modified: Option<FsTimestamp>,
        preview_data: &[u8],
    ) -> Result<String, trashradar_domain::error::CoreError> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        source_path.hash(&mut hasher);
        let hash_val = hasher.finish();

        let ext = match kind {
            PreviewKind::Thumbnail => "thumb.jpg",
            PreviewKind::Scrub => "scrub.bin",
        };
        let filename = format!("{:x}_{}", hash_val, ext);
        let preview_path_buf = self.cache_dir.join(filename);
        let preview_path = preview_path_buf
            .to_str()
            .ok_or_else(|| {
                trashradar_domain::error::CoreError::invalid_argument("invalid cache path encoding")
            })?
            .to_string();

        std::fs::write(&preview_path_buf, preview_data)
            .map_err(|err| trashradar_domain::error::CoreError::io(err.to_string()))?;

        let entry = PreviewCacheEntry {
            source_path: source_path.to_string(),
            preview_kind: kind,
            size: current_size,
            modified_at: current_modified,
            preview_path: preview_path.clone(),
        };
        self.db.put_preview_entry(&entry)?;

        Ok(preview_path)
    }

    /// Видалити всі кешовані файли прев'ю та записи в індексі для заданого шляху джерела.
    pub fn delete_previews(
        &self,
        source_path: &str,
    ) -> Result<(), trashradar_domain::error::CoreError> {
        if let Some(entry) = self
            .db
            .get_preview_entry(source_path, PreviewKind::Thumbnail)?
        {
            let _ = std::fs::remove_file(Path::new(&entry.preview_path));
        }
        if let Some(entry) = self.db.get_preview_entry(source_path, PreviewKind::Scrub)? {
            let _ = std::fs::remove_file(Path::new(&entry.preview_path));
        }

        self.db.delete_all_previews_for_path(source_path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn test_priority_ordering() {
        let scheduler = PreviewScheduler::new(1, 0.0); // 1 worker, no aging for this test
        let (tx, rx) = mpsc::channel();

        // Block the single worker first
        let (block_tx, block_rx) = mpsc::channel();
        scheduler.submit("blocker".to_string(), PreviewPriority::P1, move |_| {
            block_rx.recv().unwrap();
        });

        // Submit tasks with different priorities
        let tx_p3 = tx.clone();
        scheduler.submit("p3_task".to_string(), PreviewPriority::P3, move |_| {
            tx_p3.send("P3").unwrap();
        });

        let tx_p0 = tx.clone();
        scheduler.submit("p0_task".to_string(), PreviewPriority::P0, move |_| {
            tx_p0.send("P0").unwrap();
        });

        let tx_p2 = tx.clone();
        scheduler.submit("p2_task".to_string(), PreviewPriority::P2, move |_| {
            tx_p2.send("P2").unwrap();
        });

        // Release worker
        block_tx.send(()).unwrap();

        // Check sequence of execution: P0 -> P2 -> P3
        assert_eq!(rx.recv_timeout(Duration::from_millis(500)).unwrap(), "P0");
        assert_eq!(rx.recv_timeout(Duration::from_millis(500)).unwrap(), "P2");
        assert_eq!(rx.recv_timeout(Duration::from_millis(500)).unwrap(), "P3");
    }

    #[test]
    fn test_cancellation_pending() {
        let scheduler = PreviewScheduler::new(1, 0.0);
        let (block_tx, block_rx) = mpsc::channel();
        let ran = Arc::new(AtomicBool::new(false));

        scheduler.submit("blocker".to_string(), PreviewPriority::P1, move |_| {
            block_rx.recv().unwrap();
        });

        let ran_clone = Arc::clone(&ran);
        scheduler.submit(
            "cancelled_task".to_string(),
            PreviewPriority::P1,
            move |_| {
                ran_clone.store(true, Ordering::SeqCst);
            },
        );

        assert!(scheduler.is_queued_or_running("cancelled_task"));
        scheduler.cancel("cancelled_task");
        assert!(!scheduler.is_queued_or_running("cancelled_task"));

        block_tx.send(()).unwrap();
        thread::sleep(Duration::from_millis(50));

        assert!(!ran.load(Ordering::SeqCst));
    }

    #[test]
    fn test_cancellation_running() {
        let scheduler = PreviewScheduler::new(1, 0.0);
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();

        scheduler.submit(
            "running_task".to_string(),
            PreviewPriority::P1,
            move |token| {
                started_tx.send(()).unwrap();
                let mut cancelled = false;
                for _ in 0..50 {
                    if token.is_cancelled() {
                        cancelled = true;
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                finished_tx.send(cancelled).unwrap();
            },
        );

        started_rx.recv_timeout(Duration::from_millis(500)).unwrap();
        scheduler.cancel("running_task");

        let was_cancelled = finished_rx
            .recv_timeout(Duration::from_millis(500))
            .unwrap();
        assert!(was_cancelled);
    }

    #[test]
    fn test_aging() {
        // We set high aging rate: 1000 points per ms.
        // P3 has base score 0. P2 has base score 1000.
        // If we queue P3 first, wait 2 ms, then queue P2, the P3 task should have aged to score 0 + 2 * 1000 = 2000.
        // Thus, the aged P3 should run before the new P2.
        let scheduler = PreviewScheduler::new(1, 1000.0);
        let (tx, rx) = mpsc::channel();

        let (block_tx, block_rx) = mpsc::channel();
        scheduler.submit("blocker".to_string(), PreviewPriority::P1, move |_| {
            block_rx.recv().unwrap();
        });

        let tx_p3 = tx.clone();
        scheduler.submit("p3_task".to_string(), PreviewPriority::P3, move |_| {
            tx_p3.send("P3").unwrap();
        });

        // Let it age!
        thread::sleep(Duration::from_millis(5));

        let tx_p2 = tx.clone();
        scheduler.submit("p2_task".to_string(), PreviewPriority::P2, move |_| {
            tx_p2.send("P2").unwrap();
        });

        block_tx.send(()).unwrap();

        // Due to aging, P3 should run first
        assert_eq!(rx.recv_timeout(Duration::from_millis(500)).unwrap(), "P3");
        assert_eq!(rx.recv_timeout(Duration::from_millis(500)).unwrap(), "P2");
    }

    #[test]
    fn test_preemption() {
        let scheduler = PreviewScheduler::new(1, 0.0); // 1 worker
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();

        // Submit a P3 task that loops and checks cancellation
        scheduler.submit("p3_task".to_string(), PreviewPriority::P3, move |token| {
            started_tx.send(()).unwrap();
            let mut cancelled = false;
            for _ in 0..100 {
                if token.is_cancelled() {
                    cancelled = true;
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            finished_tx.send(cancelled).unwrap();
        });

        started_rx.recv_timeout(Duration::from_millis(500)).unwrap();

        // Submit a P0 task. Since 1 worker is busy with P3, this P0 submit should trigger cancellation of P3 task
        let (p0_tx, p0_rx) = mpsc::channel();
        scheduler.submit("p0_task".to_string(), PreviewPriority::P0, move |_| {
            p0_tx.send(()).unwrap();
        });

        // Verify P3 was indeed cancelled
        let p3_cancelled = finished_rx
            .recv_timeout(Duration::from_millis(500))
            .unwrap();
        assert!(p3_cancelled);

        // Verify P0 task ran immediately after
        p0_rx.recv_timeout(Duration::from_millis(500)).unwrap();
    }

    struct MockPreviewCache {
        entries: Mutex<HashMap<(String, PreviewKind), PreviewCacheEntry>>,
    }

    impl MockPreviewCache {
        fn new() -> Self {
            Self {
                entries: Mutex::new(HashMap::new()),
            }
        }
    }

    impl PreviewCache for MockPreviewCache {
        fn get_preview_entry(
            &self,
            source_path: &str,
            kind: PreviewKind,
        ) -> std::result::Result<Option<PreviewCacheEntry>, trashradar_domain::error::CoreError>
        {
            let map = self.entries.lock().unwrap();
            Ok(map.get(&(source_path.to_string(), kind)).cloned())
        }

        fn put_preview_entry(
            &self,
            entry: &PreviewCacheEntry,
        ) -> std::result::Result<(), trashradar_domain::error::CoreError> {
            let mut map = self.entries.lock().unwrap();
            map.insert(
                (entry.source_path.clone(), entry.preview_kind),
                entry.clone(),
            );
            Ok(())
        }

        fn delete_preview_entry(
            &self,
            source_path: &str,
            kind: PreviewKind,
        ) -> std::result::Result<(), trashradar_domain::error::CoreError> {
            let mut map = self.entries.lock().unwrap();
            map.remove(&(source_path.to_string(), kind));
            Ok(())
        }

        fn delete_all_previews_for_path(
            &self,
            source_path: &str,
        ) -> std::result::Result<(), trashradar_domain::error::CoreError> {
            let mut map = self.entries.lock().unwrap();
            map.retain(|(path, _), _| path != source_path);
            Ok(())
        }
    }

    #[test]
    fn test_preview_cache_manager_flow() {
        let temp_dir = std::env::temp_dir().join(format!(
            "tr_test_cache_{}",
            Instant::now().elapsed().as_micros()
        ));
        let db = Arc::new(MockPreviewCache::new());
        let manager =
            PreviewCacheManager::new(temp_dir.clone(), Arc::clone(&db) as Arc<dyn PreviewCache>);

        let source = r"C:\T\source_file.jpg";
        let size = ByteSize(500);
        let mtime = Some(FsTimestamp(999));
        let preview_data = b"fake jpeg content";

        // Query missing
        assert!(manager
            .get_valid_preview_path(source, PreviewKind::Thumbnail, size, mtime)
            .is_none());

        // Save
        let ppath = manager
            .save_preview(source, PreviewKind::Thumbnail, size, mtime, preview_data)
            .unwrap();
        assert!(Path::new(&ppath).exists());

        // Query valid hit
        let hit = manager
            .get_valid_preview_path(source, PreviewKind::Thumbnail, size, mtime)
            .unwrap();
        assert_eq!(hit, ppath);

        // Query invalid size
        assert!(manager
            .get_valid_preview_path(source, PreviewKind::Thumbnail, ByteSize(501), mtime)
            .is_none());

        // Query invalid mtime
        assert!(manager
            .get_valid_preview_path(
                source,
                PreviewKind::Thumbnail,
                size,
                Some(FsTimestamp(1000))
            )
            .is_none());

        // Save new version
        let ppath2 = manager
            .save_preview(
                source,
                PreviewKind::Thumbnail,
                ByteSize(501),
                Some(FsTimestamp(1000)),
                b"updated data",
            )
            .unwrap();
        assert!(Path::new(&ppath2).exists());

        let hit2 = manager
            .get_valid_preview_path(
                source,
                PreviewKind::Thumbnail,
                ByteSize(501),
                Some(FsTimestamp(1000)),
            )
            .unwrap();
        assert_eq!(hit2, ppath2);

        // Delete
        manager.delete_previews(source).unwrap();
        assert!(!Path::new(&ppath2).exists());
        assert!(manager
            .get_valid_preview_path(
                source,
                PreviewKind::Thumbnail,
                ByteSize(501),
                Some(FsTimestamp(1000))
            )
            .is_none());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
