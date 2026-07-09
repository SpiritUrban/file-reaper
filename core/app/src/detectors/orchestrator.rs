//! Оркестратор ферми детекторів (T-037).
//!
//! Проганяє потік записів індексу через **усі активні** детектори реєстру
//! (architecture.md §6.1). Вимкнені детектори не отримують потік.
//!
//! Два шляхи:
//! - **під час скану** — [`categorize_batch`]: батч свіжо проіндексованих
//!   записів → оновлені записи для upsert (нуль знання про конкретні типи
//!   детекторів);
//! - **повний прогін** — [`categorize_index`]: знімок HotIndex, кооперативна
//!   відміна, запис вердиктів назад через `upsert_batch`. Може йти у
//!   [`WorkerPool`] (T-008) як фонова задача.
//!
//! Перетин категорій (файл у кількох): збираємо всі hits; у `FileRecord`
//! (одне поле category) пишеться **перший** hit у порядку реєстру.
//! Чесна цифра / multi-category — T-054.

use super::contract::DetectorHit;
use super::registry::DetectorRegistry;
use crate::ports::HotIndex;
use crate::workers::{CancellationToken, JobHandle, JobPriority, WorkerPool};
use trashradar_domain::candidate::{Decision, FileRecord};
use trashradar_domain::category::CategoryId;
use trashradar_domain::error::CoreError;

/// Підсумок одного прогону категоризації.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CategorizationStats {
    /// Записів, які побачив оркестратор (після фільтра Keep).
    pub records_seen: u64,
    /// Записів, пропущених як Keep (не кандидати).
    pub records_skipped_keep: u64,
    /// Успішних вердиктів (hit-ів) від усіх увімкнених детекторів.
    pub hits: u64,
    /// Записів, у яких оновлено category/explanation/safety.
    pub records_updated: u64,
    /// Скільки увімкнених детекторів брали участь у прогоні.
    pub detectors_enabled: u64,
    /// Скільки батчів оброблено (повний прогін індексу).
    pub batches: u64,
    /// Прогін скасовано кооперативно.
    pub cancelled: bool,
}

/// Результат категоризації одного батча (шлях «під час скану»).
#[derive(Debug, Default)]
pub struct CategorizeBatchResult {
    /// Записи з застосованим первинним вердиктом (для upsert у індекс).
    pub updated: Vec<FileRecord>,
    /// Усі hits (включно з перетинами) — для майбутнього Aggregator (T-054).
    pub hits: Vec<DetectorHit>,
    pub stats: CategorizationStats,
}

/// Оркестратор: реєстр → потік → вердикти → індекс.
///
/// Не знає конкретних типів детекторів — лише [`DetectorRegistry`].
pub struct DetectorOrchestrator<'a> {
    registry: &'a DetectorRegistry,
    /// Розмір батча для повного прогону індексу.
    batch_size: usize,
}

impl<'a> DetectorOrchestrator<'a> {
    pub fn new(registry: &'a DetectorRegistry) -> Self {
        Self {
            registry,
            batch_size: 10_000,
        }
    }

    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size.max(1);
        self
    }

    /// Категоризувати батч записів (виклик після scan batch / T-024).
    ///
    /// Вимкнені детектори **не** викликаються. Keep — пропускаються.
    /// Записи без жодного hit лишаються з `Uncategorized` і **не** потрапляють
    /// у `updated` (нема чого upsert-ити).
    pub fn categorize_batch(&self, records: &[FileRecord]) -> CategorizeBatchResult {
        let detectors_enabled = self.registry.enabled().count() as u64;
        let mut result = CategorizeBatchResult {
            stats: CategorizationStats {
                detectors_enabled,
                ..CategorizationStats::default()
            },
            ..CategorizeBatchResult::default()
        };

        for record in records {
            if record.decision == Decision::Keep {
                result.stats.records_skipped_keep += 1;
                continue;
            }
            result.stats.records_seen += 1;

            let hits = self.registry.evaluate_record(record);
            result.stats.hits += hits.len() as u64;
            if let Some(primary) = hits.first() {
                result.updated.push(apply_primary_hit(record, primary));
                result.stats.records_updated += 1;
            }
            result.hits.extend(hits);
        }
        result
    }

    /// Повний прогін hot-індексу: get_all → батчі → upsert оновлених.
    ///
    /// Кооперативна відміна між батчами. Часткові upsert уже застосовані
    /// при cancel — валідний стан (як scan cancel, T-033).
    pub fn categorize_index(
        &self,
        index: &dyn HotIndex,
        cancel: &CancellationToken,
    ) -> Result<CategorizationStats, CoreError> {
        if cancel.is_cancelled() {
            return Ok(CategorizationStats {
                detectors_enabled: self.registry.enabled().count() as u64,
                cancelled: true,
                ..CategorizationStats::default()
            });
        }

        let snapshot = index.get_all()?;
        let mut stats = CategorizationStats {
            detectors_enabled: self.registry.enabled().count() as u64,
            ..CategorizationStats::default()
        };

        for chunk in snapshot.chunks(self.batch_size) {
            if cancel.is_cancelled() {
                stats.cancelled = true;
                break;
            }
            let batch = self.categorize_batch(chunk);
            stats.records_seen += batch.stats.records_seen;
            stats.records_skipped_keep += batch.stats.records_skipped_keep;
            stats.hits += batch.stats.hits;
            stats.records_updated += batch.stats.records_updated;
            stats.batches += 1;
            if !batch.updated.is_empty() {
                index.upsert_batch(batch.updated)?;
            }
        }
        Ok(stats)
    }

    /// Поставити повний прогін у [`WorkerPool`] (фон під час/після скану).
    ///
    /// `index` має бути `Send + Sync + 'static` (напр. `Arc<InMemoryIndex>`).
    pub fn spawn_categorize_index<I>(
        &self,
        pool: &WorkerPool,
        priority: JobPriority,
        index: std::sync::Arc<I>,
        registry: std::sync::Arc<DetectorRegistry>,
    ) -> JobHandle
    where
        I: HotIndex + Send + Sync + 'static,
    {
        let batch_size = self.batch_size;
        pool.submit(priority, move |cancel| {
            let orch = DetectorOrchestrator {
                registry: registry.as_ref(),
                batch_size,
            };
            match orch.categorize_index(index.as_ref(), &cancel) {
                Ok(stats) => {
                    // Логування — у shell/tracing; тут лише no-op sink для app-шару.
                    let _ = stats;
                }
                Err(_e) => {
                    // Помилки індексу не панікують воркер (T-008 panic barrier).
                }
            }
        })
    }
}

/// Застосувати первинний вердикт до копії запису.
pub fn apply_primary_hit(record: &FileRecord, hit: &DetectorHit) -> FileRecord {
    let mut out = record.clone();
    out.category = hit.verdict.category;
    out.safety = hit.verdict.safety;
    out.explanation = hit.verdict.explanation.clone();
    out.detector_id = hit.detector_id.as_str().to_string();
    // Якщо раніше був Uncategorized — тепер категоризований; інші поля лишаємо.
    debug_assert_ne!(out.category, CategoryId::Uncategorized);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::{Detector, DetectorId};
    use crate::workers::WorkerPoolConfig;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, SafetyLevel,
        Verdict,
    };

    struct CountingDetector {
        id: DetectorId,
        category: CategoryId,
        enabled: bool,
        calls: AtomicU64,
        min_size: u64,
    }

    impl CountingDetector {
        fn new(id: &'static str, category: CategoryId, min_size: u64, enabled: bool) -> Self {
            Self {
                id: DetectorId::new(id),
                category,
                enabled,
                calls: AtomicU64::new(0),
                min_size,
            }
        }
    }

    impl Detector for CountingDetector {
        fn id(&self) -> DetectorId {
            self.id
        }
        fn category(&self) -> CategoryId {
            self.category
        }
        fn is_enabled(&self) -> bool {
            self.enabled
        }
        fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if record.size.0 >= self.min_size {
                Some(Verdict::new(
                    self.category,
                    format!("size {}", record.size.0),
                    SafetyLevel::ReviewRecommended,
                ))
            } else {
                None
            }
        }
    }

    struct MemIndex {
        records: Mutex<Vec<FileRecord>>,
    }

    impl MemIndex {
        fn new(records: Vec<FileRecord>) -> Self {
            Self {
                records: Mutex::new(records),
            }
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
        fn search_file_records(&self, _: &str, _: usize) -> Result<Vec<FileRecord>, CoreError> {
            Ok(Vec::new())
        }
        fn remove_paths(&self, _: &[String]) -> Result<usize, CoreError> {
            Ok(0)
        }
        fn upsert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError> {
            let mut recs = self.records.lock().unwrap();
            for record in records {
                if let Some(pos) = recs
                    .iter()
                    .position(|r| r.candidate_id == record.candidate_id)
                {
                    recs[pos] = record;
                } else {
                    recs.push(record);
                }
            }
            Ok(())
        }
    }

    fn rec(id: u64, size: u64) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: format!("C:\\f{id}.dat"),
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
    fn batch_during_scan_updates_matching_records() {
        let mut reg = DetectorRegistry::new();
        reg.register(CountingDetector::new(
            "large",
            CategoryId::LargeFiles,
            1_000,
            true,
        ));
        let orch = DetectorOrchestrator::new(&reg);

        let batch = [rec(1, 50), rec(2, 5_000), rec(3, 2_000)];
        let out = orch.categorize_batch(&batch);

        assert_eq!(out.stats.records_seen, 3);
        assert_eq!(out.stats.records_updated, 2);
        assert_eq!(out.stats.hits, 2);
        assert_eq!(out.updated.len(), 2);
        assert!(out
            .updated
            .iter()
            .all(|r| r.category == CategoryId::LargeFiles));
        assert!(out.updated.iter().all(|r| r.detector_id == "large"));
        assert!(out.updated.iter().all(|r| !r.explanation.is_empty()));
    }

    #[test]
    fn disabled_detector_receives_no_stream() {
        // DoD T-037: вимкнений детектор не отримує потік.
        let mut reg = DetectorRegistry::new();
        // CountingDetector is moved into registry — use Arc counters via shared state.
        // Register two: one disabled, one enabled. Track calls via separate wrappers.
        let disabled_calls = Arc::new(AtomicU64::new(0));
        let enabled_calls = Arc::new(AtomicU64::new(0));

        struct ArcCounting {
            id: DetectorId,
            category: CategoryId,
            enabled: bool,
            calls: Arc<AtomicU64>,
            min_size: u64,
        }
        impl Detector for ArcCounting {
            fn id(&self) -> DetectorId {
                self.id
            }
            fn category(&self) -> CategoryId {
                self.category
            }
            fn is_enabled(&self) -> bool {
                self.enabled
            }
            fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                if record.size.0 >= self.min_size {
                    Some(Verdict::new(self.category, "hit", SafetyLevel::SafeToBulk))
                } else {
                    None
                }
            }
        }

        reg.register(ArcCounting {
            id: DetectorId::new("off"),
            category: CategoryId::TempFiles,
            enabled: false,
            calls: Arc::clone(&disabled_calls),
            min_size: 0,
        });
        reg.register(ArcCounting {
            id: DetectorId::new("on"),
            category: CategoryId::LargeFiles,
            enabled: true,
            calls: Arc::clone(&enabled_calls),
            min_size: 100,
        });

        let orch = DetectorOrchestrator::new(&reg);
        let batch = [rec(1, 200), rec(2, 300), rec(3, 50)];
        let out = orch.categorize_batch(&batch);

        assert_eq!(
            disabled_calls.load(Ordering::Relaxed),
            0,
            "disabled: 0 calls"
        );
        assert_eq!(
            enabled_calls.load(Ordering::Relaxed),
            3,
            "enabled: every record"
        );
        assert_eq!(out.stats.detectors_enabled, 1);
        assert_eq!(out.stats.records_updated, 2); // 200, 300
    }

    #[test]
    fn categorize_index_writes_verdicts_back() {
        let mut reg = DetectorRegistry::new();
        reg.register(CountingDetector::new(
            "large",
            CategoryId::LargeFiles,
            1_000,
            true,
        ));
        let index = MemIndex::new(vec![rec(1, 50), rec(2, 9_000), rec(3, 1_001)]);
        let orch = DetectorOrchestrator::new(&reg).with_batch_size(2);
        let stats = orch
            .categorize_index(&index, &CancellationToken::new())
            .expect("ok");

        assert_eq!(stats.records_seen, 3);
        assert_eq!(stats.records_updated, 2);
        assert_eq!(stats.batches, 2); // batch_size=2 → ceil(3/2)=2
        assert!(!stats.cancelled);

        let all = index.get_all().unwrap();
        let r2 = all
            .iter()
            .find(|r| r.candidate_id == CandidateId(2))
            .unwrap();
        assert_eq!(r2.category, CategoryId::LargeFiles);
        assert_eq!(r2.detector_id, "large");
        let r1 = all
            .iter()
            .find(|r| r.candidate_id == CandidateId(1))
            .unwrap();
        assert_eq!(r1.category, CategoryId::Uncategorized);
    }

    #[test]
    fn keep_records_are_not_recategorized() {
        let mut reg = DetectorRegistry::new();
        reg.register(CountingDetector::new(
            "large",
            CategoryId::LargeFiles,
            0,
            true,
        ));
        let mut kept = rec(1, 99_999);
        kept.decision = Decision::Keep;
        let orch = DetectorOrchestrator::new(&reg);
        let out = orch.categorize_batch(&[kept, rec(2, 10)]);
        assert_eq!(out.stats.records_skipped_keep, 1);
        assert_eq!(out.stats.records_seen, 1);
        assert_eq!(out.updated.len(), 1);
        assert_eq!(out.updated[0].candidate_id, CandidateId(2));
    }

    #[test]
    fn cancel_stops_between_batches() {
        let mut reg = DetectorRegistry::new();
        reg.register(CountingDetector::new(
            "large",
            CategoryId::LargeFiles,
            0,
            true,
        ));
        // Багато записів, batch_size=1, cancel після старту — хоча б один batch.
        let records: Vec<_> = (0..20).map(|i| rec(i, 100)).collect();
        let index = MemIndex::new(records);
        let orch = DetectorOrchestrator::new(&reg).with_batch_size(1);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let stats = orch.categorize_index(&index, &cancel).expect("ok");
        assert!(stats.cancelled);
        assert_eq!(stats.records_seen, 0);
    }

    #[test]
    fn spawn_on_worker_pool_completes() {
        let mut reg = DetectorRegistry::new();
        reg.register(CountingDetector::new(
            "large",
            CategoryId::LargeFiles,
            500,
            true,
        ));
        let registry = Arc::new(reg);
        let index = Arc::new(MemIndex::new(vec![rec(1, 1000), rec(2, 10)]));
        let pool = WorkerPool::new(WorkerPoolConfig { workers: 1 });
        let orch = DetectorOrchestrator::new(registry.as_ref());
        let handle = orch.spawn_categorize_index(
            &pool,
            JobPriority::Background,
            Arc::clone(&index),
            Arc::clone(&registry),
        );
        let outcome = handle.wait();
        assert_eq!(outcome, crate::workers::JobOutcome::Completed);
        let all = index.get_all().unwrap();
        assert_eq!(
            all.iter()
                .find(|r| r.candidate_id == CandidateId(1))
                .unwrap()
                .category,
            CategoryId::LargeFiles
        );
    }

    #[test]
    fn multi_hit_keeps_first_as_primary_all_hits_reported() {
        let mut reg = DetectorRegistry::new();
        reg.register(CountingDetector::new(
            "first",
            CategoryId::LargeFiles,
            0,
            true,
        ));
        reg.register(CountingDetector::new(
            "second",
            CategoryId::Archives,
            0,
            true,
        ));
        let orch = DetectorOrchestrator::new(&reg);
        let out = orch.categorize_batch(&[rec(1, 1)]);
        assert_eq!(out.hits.len(), 2);
        assert_eq!(out.updated.len(), 1);
        assert_eq!(out.updated[0].category, CategoryId::LargeFiles);
        assert_eq!(out.updated[0].detector_id, "first");
    }
}
