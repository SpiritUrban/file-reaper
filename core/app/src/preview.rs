use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::ports::{
    PreviewCache, PreviewCacheEntry, PreviewKind, RawThumbnail, ThumbnailEncoder, ThumbnailSource,
    VideoFrameSource,
};
use crate::workers::CancellationToken;
use trashradar_domain::candidate::{ByteSize, FsTimestamp};
use trashradar_domain::error::CoreError;

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

        // T-074: декодери медіа можуть панікувати на битих/екзотичних файлах.
        // Паніка однієї preview-задачі не має вбивати worker і всю чергу.
        let _ = panic::catch_unwind(AssertUnwindSafe(|| (task.job)(cancellation.clone())));

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
            // Мініатюри/великі превью кодуються PNG (T-073, PngThumbnailEncoder).
            PreviewKind::Thumbnail => "thumb.png",
            PreviewKind::Large => "large.png",
            // Скраб-смуга — сирі кадри BGRA (T-072), формат зрізів у ScrubStrip.
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
        for kind in [
            PreviewKind::Thumbnail,
            PreviewKind::Large,
            PreviewKind::Scrub,
        ] {
            if let Some(entry) = self.db.get_preview_entry(source_path, kind)? {
                let _ = std::fs::remove_file(Path::new(&entry.preview_path));
            }
        }

        self.db.delete_all_previews_for_path(source_path)?;
        Ok(())
    }
}

/// Якість доставленого превью (T-073, architecture.md §5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewQuality {
    /// Чорновий кадр з дискового кешу — мініатюра, розтягнута UI; миттєво.
    Draft,
    /// Повнорозмірне (в межах запитаного краю) превью — підміняє чорнове.
    Sharp,
}

/// Доставка превью споживачу; shell перетворює на подію UI (T-124/T-140).
#[derive(Debug, Clone)]
pub struct PreviewDelivery {
    /// Шлях файла-джерела (кандидата).
    pub source_path: String,
    /// Якість цієї доставки.
    pub quality: PreviewQuality,
    /// Файл у дисковому кеші превью (T-068), готовий до показу.
    pub preview_path: String,
}

/// Колбек доставки превью (може викликатись з фонового воркера).
pub type PreviewDeliveryFn = Arc<dyn Fn(PreviewDelivery) + Send + Sync>;

/// Підсумок прийому запиту двоступеневого превью (T-073).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TwoStageOutcome {
    /// Різке превью було в кеші — доставлено одразу, генерація не потрібна.
    SharpFromCache,
    /// Чорнове доставлено з кешу миттєво; різке генерується у фоні.
    DraftThenSharpScheduled,
    /// Кешу немає: доставки не було, генерацію різкого заплановано.
    SharpScheduledOnly,
}

/// Міст: джерело кадрів відео (T-071) як джерело мініатюр у ланцюжку превью.
pub struct VideoKeyFrameSource(pub Arc<dyn VideoFrameSource>);

impl ThumbnailSource for VideoKeyFrameSource {
    fn thumbnail(&self, path: &str, max_edge: u32) -> Result<Option<RawThumbnail>, CoreError> {
        self.0.key_frame(path, max_edge)
    }
}

/// Двоступенева якість великого превью (T-073, architecture.md §5.3):
/// ступінь 1 — чорновий кадр **лише з дискового кешу** (жодної генерації
/// та декодування — це шлях < 100 мс); ступінь 2 — повнорозмірна генерація
/// P0-задачею через ланцюжок джерел, збереження в кеш (вид `Large`)
/// і доставка-підміна (ціль < 600 мс, автозамір — T-076).
pub struct TwoStagePreviewer {
    cache: Arc<PreviewCacheManager>,
    scheduler: Arc<PreviewScheduler>,
    sources: Vec<Arc<dyn ThumbnailSource>>,
    encoder: Arc<dyn ThumbnailEncoder>,
}

impl TwoStagePreviewer {
    /// Створити оркестратор. `sources` — ланцюжок джерел за спаданням
    /// швидкості (architecture.md §5.2); перше `Some` виграє.
    pub fn new(
        cache: Arc<PreviewCacheManager>,
        scheduler: Arc<PreviewScheduler>,
        sources: Vec<Arc<dyn ThumbnailSource>>,
        encoder: Arc<dyn ThumbnailEncoder>,
    ) -> Self {
        Self {
            cache,
            scheduler,
            sources,
            encoder,
        }
    }

    /// Запит великого превью для файла під курсором / у панелі деталей.
    ///
    /// `size`/`modified_at` — з індексу (валідність кешу за T-068);
    /// `sharp_edge` — край повнорозмірного превью. Доставки йдуть через
    /// `on_delivery`: спершу (за наявності кешу) `Draft`, потім `Sharp`.
    pub fn request_large_preview(
        &self,
        source_path: &str,
        size: ByteSize,
        modified_at: Option<FsTimestamp>,
        sharp_edge: u32,
        on_delivery: PreviewDeliveryFn,
    ) -> Result<TwoStageOutcome, CoreError> {
        if source_path.is_empty() {
            return Err(CoreError::invalid_argument(
                "Порожній шлях до файла превью.",
            ));
        }

        // Різке вже в кеші (валідне за size+mtime) → миттєва відповідь,
        // без генерації: повторний показ — з кешу (DoD T-068/T-073).
        if let Some(sharp_path) =
            self.cache
                .get_valid_preview_path(source_path, PreviewKind::Large, size, modified_at)
        {
            on_delivery(PreviewDelivery {
                source_path: source_path.to_string(),
                quality: PreviewQuality::Sharp,
                preview_path: sharp_path,
            });
            return Ok(TwoStageOutcome::SharpFromCache);
        }

        // Ступінь 1 (чорновий, < 100 мс): лише читання кешу мініатюр —
        // на цьому шляху немає ні декодування, ні звертань до джерел.
        let draft_delivered = match self.cache.get_valid_preview_path(
            source_path,
            PreviewKind::Thumbnail,
            size,
            modified_at,
        ) {
            Some(draft_path) => {
                on_delivery(PreviewDelivery {
                    source_path: source_path.to_string(),
                    quality: PreviewQuality::Draft,
                    preview_path: draft_path,
                });
                true
            }
            None => false,
        };

        // Ступінь 2 (різкий): генерація P0-задачею (T-067 — витісняє фонові),
        // збереження у кеш і доставка-підміна. Збій генерації не «забирає»
        // вже показане чорнове превью — просто немає другої доставки.
        let cache = Arc::clone(&self.cache);
        let sources = self.sources.clone();
        let encoder = Arc::clone(&self.encoder);
        let path_owned = source_path.to_string();
        self.scheduler
            .submit(path_owned.clone(), PreviewPriority::P0, move |token| {
                if token.is_cancelled() {
                    return;
                }
                let Some(raw) = generate_from_chain(&sources, &path_owned, sharp_edge) else {
                    return;
                };
                if token.is_cancelled() {
                    return;
                }
                let Some(bytes) = encode_thumbnail(encoder.as_ref(), &raw) else {
                    return;
                };
                if let Ok(preview_path) =
                    cache.save_preview(&path_owned, PreviewKind::Large, size, modified_at, &bytes)
                {
                    on_delivery(PreviewDelivery {
                        source_path: path_owned.clone(),
                        quality: PreviewQuality::Sharp,
                        preview_path,
                    });
                }
            });

        Ok(if draft_delivered {
            TwoStageOutcome::DraftThenSharpScheduled
        } else {
            TwoStageOutcome::SharpScheduledOnly
        })
    }

    /// Скасувати запит превью для шляху (користувач поїхав далі, T-067).
    pub fn cancel(&self, source_path: &str) {
        self.scheduler.cancel(source_path);
    }
}

/// Пройти ланцюжок джерел: перше `Some` виграє; `Ok(None)`, помилка
/// та panic окремого джерела не зривають ланцюжок (T-074).
fn generate_from_chain(
    sources: &[Arc<dyn ThumbnailSource>],
    path: &str,
    max_edge: u32,
) -> Option<RawThumbnail> {
    for source in sources {
        match panic::catch_unwind(AssertUnwindSafe(|| source.thumbnail(path, max_edge))) {
            Ok(Ok(Some(raw))) => return Some(raw),
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => continue,
        }
    }
    None
}

fn encode_thumbnail(encoder: &dyn ThumbnailEncoder, raw: &RawThumbnail) -> Option<Vec<u8>> {
    match panic::catch_unwind(AssertUnwindSafe(|| encoder.encode(raw))) {
        Ok(Ok(bytes)) => Some(bytes),
        Ok(Err(_)) | Err(_) => None,
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

    // --- T-073: двоступенева якість -------------------------------------

    use std::sync::atomic::AtomicUsize;

    /// Джерело-лічильник: віддає суцільну мініатюру та рахує звертання.
    struct CountingSource {
        calls: Arc<AtomicUsize>,
    }

    impl ThumbnailSource for CountingSource {
        fn thumbnail(&self, _path: &str, max_edge: u32) -> Result<Option<RawThumbnail>, CoreError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let edge = max_edge.min(8);
            Ok(Some(RawThumbnail {
                width: edge,
                height: edge,
                bgra: vec![7u8; (edge * edge * 4) as usize],
            }))
        }
    }

    /// Джерело, що не покриває жодного файла (`Ok(None)`).
    struct NoneSource;

    impl ThumbnailSource for NoneSource {
        fn thumbnail(&self, _p: &str, _e: u32) -> Result<Option<RawThumbnail>, CoreError> {
            Ok(None)
        }
    }

    /// Джерело, що завжди падає (помилка не має зривати ланцюжок).
    struct FailingSource;

    impl ThumbnailSource for FailingSource {
        fn thumbnail(&self, _p: &str, _e: u32) -> Result<Option<RawThumbnail>, CoreError> {
            Err(CoreError::io("декодер впав"))
        }
    }

    /// Джерело, що панікує як битий/екзотичний декодер (T-074).
    struct PanickingSource;

    impl ThumbnailSource for PanickingSource {
        fn thumbnail(&self, _p: &str, _e: u32) -> Result<Option<RawThumbnail>, CoreError> {
            panic!("decoder panic");
        }
    }

    /// Тестовий кодувальник: префікс + пікселі як є.
    struct PassthroughEncoder;

    impl ThumbnailEncoder for PassthroughEncoder {
        fn encode(&self, thumbnail: &RawThumbnail) -> Result<Vec<u8>, CoreError> {
            let mut out = b"ENC".to_vec();
            out.extend_from_slice(&thumbnail.bgra);
            Ok(out)
        }
    }

    struct PanickingEncoder;

    impl ThumbnailEncoder for PanickingEncoder {
        fn encode(&self, _thumbnail: &RawThumbnail) -> Result<Vec<u8>, CoreError> {
            panic!("encoder panic");
        }
    }

    struct TwoStageFixture {
        previewer: TwoStagePreviewer,
        cache: Arc<PreviewCacheManager>,
        source_calls: Arc<AtomicUsize>,
        temp_dir: std::path::PathBuf,
    }

    fn two_stage_fixture(
        name: &str,
        chain_prefix: Vec<Arc<dyn ThumbnailSource>>,
    ) -> TwoStageFixture {
        let temp_dir = std::env::temp_dir().join(format!(
            "tr_t073_{name}_{}",
            Instant::now().elapsed().as_nanos()
        ));
        let db = Arc::new(MockPreviewCache::new());
        let cache = Arc::new(PreviewCacheManager::new(
            temp_dir.clone(),
            db as Arc<dyn PreviewCache>,
        ));
        let source_calls = Arc::new(AtomicUsize::new(0));
        let mut sources = chain_prefix;
        sources.push(Arc::new(CountingSource {
            calls: Arc::clone(&source_calls),
        }));
        let previewer = TwoStagePreviewer::new(
            Arc::clone(&cache),
            Arc::new(PreviewScheduler::new(1, 0.0)),
            sources,
            Arc::new(PassthroughEncoder),
        );
        TwoStageFixture {
            previewer,
            cache,
            source_calls,
            temp_dir,
        }
    }

    /// Зібрати доставки у вектор + канал для очікування Sharp.
    fn delivery_recorder() -> (
        PreviewDeliveryFn,
        Arc<Mutex<Vec<PreviewDelivery>>>,
        mpsc::Receiver<PreviewQuality>,
    ) {
        let log: Arc<Mutex<Vec<PreviewDelivery>>> = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::channel();
        let log_cb = Arc::clone(&log);
        let cb: PreviewDeliveryFn = Arc::new(move |delivery: PreviewDelivery| {
            let quality = delivery.quality;
            log_cb.lock().unwrap().push(delivery);
            let _ = tx.send(quality);
        });
        (cb, log, rx)
    }

    /// Чорнове превью доставляється синхронно з кешу (шлях < 100 мс —
    /// без генерації), різке генерується і підміняє його.
    #[test]
    fn draft_from_cache_then_sharp_replacement() {
        let fx = two_stage_fixture("draft_sharp", vec![]);
        let source = r"C:\media\clip_frame.jpg";
        let size = ByteSize(1000);
        let mtime = Some(FsTimestamp(42));

        // Наповнюємо кеш мініатюр (як це зробив би конвеєр плиток).
        fx.cache
            .save_preview(source, PreviewKind::Thumbnail, size, mtime, b"tiny")
            .unwrap();

        let (cb, log, rx) = delivery_recorder();
        let outcome = fx
            .previewer
            .request_large_preview(source, size, mtime, 512, cb)
            .unwrap();
        assert_eq!(outcome, TwoStageOutcome::DraftThenSharpScheduled);

        // Чорнове вже доставлено до повернення з request (синхронний шлях
        // кешу — жодного джерела/декодування ще не викликано).
        {
            let seen = log.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].quality, PreviewQuality::Draft);
        }
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(50)).unwrap(),
            PreviewQuality::Draft
        );

        // Різке доганяє і підміняє.
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(500)).unwrap(),
            PreviewQuality::Sharp
        );
        let seen = log.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].quality, PreviewQuality::Sharp);
        assert!(Path::new(&seen[1].preview_path).exists(), "різке — у кеші");
        assert_eq!(fx.source_calls.load(Ordering::SeqCst), 1);

        // Наступний запит того самого файла — різке одразу з кешу.
        let (cb2, log2, _rx2) = delivery_recorder();
        let outcome2 = fx
            .previewer
            .request_large_preview(source, size, mtime, 512, cb2)
            .unwrap();
        assert_eq!(outcome2, TwoStageOutcome::SharpFromCache);
        assert_eq!(log2.lock().unwrap()[0].quality, PreviewQuality::Sharp);
        assert_eq!(fx.source_calls.load(Ordering::SeqCst), 1, "без регенерації");

        let _ = std::fs::remove_dir_all(&fx.temp_dir);
    }

    /// Порожній кеш: чорнового немає, різке генерується і доставляється.
    #[test]
    fn empty_cache_schedules_sharp_only() {
        let fx = two_stage_fixture("cold", vec![]);
        let (cb, log, rx) = delivery_recorder();
        let outcome = fx
            .previewer
            .request_large_preview(r"C:\media\new.png", ByteSize(5), None, 256, cb)
            .unwrap();
        assert_eq!(outcome, TwoStageOutcome::SharpScheduledOnly);
        assert!(log.lock().unwrap().is_empty(), "нічого до генерації");

        assert_eq!(
            rx.recv_timeout(Duration::from_millis(500)).unwrap(),
            PreviewQuality::Sharp
        );
        let _ = std::fs::remove_dir_all(&fx.temp_dir);
    }

    /// `Ok(None)` та помилка джерела не зривають ланцюжок — виграє перше
    /// джерело, що віддає мініатюру.
    #[test]
    fn chain_falls_through_none_and_errors() {
        let fx = two_stage_fixture("chain", vec![Arc::new(NoneSource), Arc::new(FailingSource)]);
        let (cb, _log, rx) = delivery_recorder();
        fx.previewer
            .request_large_preview(r"C:\media\odd.bin", ByteSize(1), None, 128, cb)
            .unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(500)).unwrap(),
            PreviewQuality::Sharp
        );
        assert_eq!(fx.source_calls.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&fx.temp_dir);
    }

    /// Паніка одного джерела не вбиває preview worker і не зриває ланцюжок.
    #[test]
    fn chain_falls_through_panicking_source() {
        let fx = two_stage_fixture("panic_source", vec![Arc::new(PanickingSource)]);
        let (cb, _log, rx) = delivery_recorder();
        fx.previewer
            .request_large_preview(r"C:\media\panic.bin", ByteSize(1), None, 128, cb)
            .unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(500)).unwrap(),
            PreviewQuality::Sharp
        );
        assert_eq!(fx.source_calls.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&fx.temp_dir);
    }

    /// Паніка всередині preview job не завершує worker: наступна задача виконується.
    #[test]
    fn scheduler_survives_panicking_preview_job() {
        let scheduler = PreviewScheduler::new(1, 0.0);
        scheduler.submit("panic".to_string(), PreviewPriority::P1, move |_| {
            panic!("decoder worker panic");
        });

        let (tx, rx) = mpsc::channel();
        scheduler.submit("after".to_string(), PreviewPriority::P1, move |_| {
            tx.send(()).unwrap();
        });

        rx.recv_timeout(Duration::from_millis(500))
            .expect("worker має пережити panic і виконати наступну задачу");
    }

    /// Panic енкодера деградує у відсутнє різке превью без доставки й без падіння.
    #[test]
    fn panicking_encoder_means_preview_unavailable() {
        let temp_dir = std::env::temp_dir().join(format!(
            "tr_t074_encoder_{}",
            Instant::now().elapsed().as_nanos()
        ));
        let db = Arc::new(MockPreviewCache::new());
        let cache = Arc::new(PreviewCacheManager::new(
            temp_dir.clone(),
            db as Arc<dyn PreviewCache>,
        ));
        let previewer = TwoStagePreviewer::new(
            cache,
            Arc::new(PreviewScheduler::new(1, 0.0)),
            vec![Arc::new(CountingSource {
                calls: Arc::new(AtomicUsize::new(0)),
            })],
            Arc::new(PanickingEncoder),
        );
        let (cb, log, rx) = delivery_recorder();
        let outcome = previewer
            .request_large_preview(r"C:\media\bad-encode.png", ByteSize(1), None, 128, cb)
            .unwrap();
        assert_eq!(outcome, TwoStageOutcome::SharpScheduledOnly);
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        assert!(log.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
    /// Порожній шлях — помилка аргументу, як у всіх джерел конвеєра.
    #[test]
    fn two_stage_empty_path_is_invalid_argument() {
        let fx = two_stage_fixture("badarg", vec![]);
        let (cb, _log, _rx) = delivery_recorder();
        let err = fx
            .previewer
            .request_large_preview("", ByteSize(1), None, 128, cb)
            .expect_err("порожній шлях");
        assert_eq!(
            err.code,
            trashradar_domain::error::ErrorCode::InvalidArgument
        );
        let _ = std::fs::remove_dir_all(&fx.temp_dir);
    }
}
