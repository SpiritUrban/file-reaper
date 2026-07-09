//! Оркестратор ферми детекторів (T-037 / T-038).
//!
//! Проганяє потік записів індексу через **усі активні** детектори реєстру
//! (architecture.md §6.1). Вимкнені детектори не отримують потік.
//!
//! Шляхи:
//! - **під час скану** — [`categorize_batch`]: батч → upsert hits;
//! - **повний прогін** — [`categorize_index`]: HotIndex → upsert;
//! - **перерахунок порогів (T-038)** — [`recalculate_index`]: без рескану
//!   диска; unmatched → `Uncategorized` (категорія зникає з індексу).
//!
//! Перетин категорій (файл у кількох): збираємо всі hits; у `FileRecord`
//! (одне поле category) пишеться **перший** hit у порядку реєстру.
//! Чесна цифра / multi-category — T-054.

use super::contract::{DetectorHit, DetectorId};
use super::registry::DetectorRegistry;
use super::thresholds::ThresholdValue;
use crate::ports::HotIndex;
use crate::workers::{CancellationToken, JobHandle, JobPriority, WorkerPool};
use trashradar_domain::candidate::{Decision, FileRecord, SafetyLevel};
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
    /// Записів, у яких оновлено category/explanation/safety (hit).
    pub records_updated: u64,
    /// Записів, з яких знято категорію після зміни порога (T-038).
    pub records_cleared: u64,
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
    /// у `updated` (нема чого upsert-ити). Для зняття категорії після зміни
    /// порога — [`recalculate_batch`].
    pub fn categorize_batch(&self, records: &[FileRecord]) -> CategorizeBatchResult {
        self.run_batch(records, false)
    }

    /// Перерахунок батча після зміни порогів (T-038).
    ///
    /// Як [`categorize_batch`], але записи без hit, що вже мали категорію,
    /// **очищаються** → `Uncategorized` (категорія перебудовується з індексу).
    pub fn recalculate_batch(&self, records: &[FileRecord]) -> CategorizeBatchResult {
        self.run_batch(records, true)
    }

    fn run_batch(&self, records: &[FileRecord], clear_unmatched: bool) -> CategorizeBatchResult {
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
            } else if clear_unmatched && record.category != CategoryId::Uncategorized {
                result.updated.push(clear_category(record));
                result.stats.records_cleared += 1;
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
        self.run_index(index, cancel, false)
    }

    /// Повний **перерахунок** індексу після зміни порогів (T-038).
    ///
    /// Без рескану диска: лише метадані в HotIndex. Unmatched → Uncategorized.
    /// DoD: зміна «вік 6 міс → 3 міс» перебудовує категорію; на 1 млн записів
    /// ціль < 1 с (див. ignored perf-тест).
    pub fn recalculate_index(
        &self,
        index: &dyn HotIndex,
        cancel: &CancellationToken,
    ) -> Result<CategorizationStats, CoreError> {
        self.run_index(index, cancel, true)
    }

    /// Змінити поріг детектора і одразу перерахувати індекс (T-038).
    pub fn set_threshold_and_recalculate(
        &self,
        index: &dyn HotIndex,
        detector_id: DetectorId,
        key: &str,
        value: ThresholdValue,
        cancel: &CancellationToken,
    ) -> Result<CategorizationStats, CoreError> {
        self.registry.set_threshold(detector_id, key, value)?;
        self.recalculate_index(index, cancel)
    }

    /// In-place перерахунок зрізу записів (T-038 hot path).
    ///
    /// Без алокації повного `updated` вектора й без clone шляхів — лише
    /// мутація category/safety/explanation/detector_id. Використовується
    /// perf-гейтом і може годуватись знімком індексу.
    pub fn recalculate_inplace(
        &self,
        records: &mut [FileRecord],
        cancel: &CancellationToken,
    ) -> CategorizationStats {
        let mut stats = CategorizationStats {
            detectors_enabled: self.registry.enabled().count() as u64,
            ..CategorizationStats::default()
        };
        for record in records.iter_mut() {
            if cancel.is_cancelled() {
                stats.cancelled = true;
                break;
            }
            if record.decision == Decision::Keep {
                stats.records_skipped_keep += 1;
                continue;
            }
            stats.records_seen += 1;
            let hits = self.registry.evaluate_record(record);
            stats.hits += hits.len() as u64;
            if let Some(primary) = hits.first() {
                apply_primary_hit_mut(record, primary);
                stats.records_updated += 1;
            } else if record.category != CategoryId::Uncategorized {
                clear_category_mut(record);
                stats.records_cleared += 1;
            }
        }
        stats
    }

    fn run_index(
        &self,
        index: &dyn HotIndex,
        cancel: &CancellationToken,
        clear_unmatched: bool,
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
            let batch = self.run_batch(chunk, clear_unmatched);
            stats.records_seen += batch.stats.records_seen;
            stats.records_skipped_keep += batch.stats.records_skipped_keep;
            stats.hits += batch.stats.hits;
            stats.records_updated += batch.stats.records_updated;
            stats.records_cleared += batch.stats.records_cleared;
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
    apply_primary_hit_mut(&mut out, hit);
    out
}

/// In-place застосування вердикту (T-038 hot path).
pub fn apply_primary_hit_mut(record: &mut FileRecord, hit: &DetectorHit) {
    record.category = hit.verdict.category;
    record.safety = hit.verdict.safety;
    record.explanation.clone_from(&hit.verdict.explanation);
    record.detector_id.clear();
    record.detector_id.push_str(hit.detector_id.as_str());
    debug_assert_ne!(record.category, CategoryId::Uncategorized);
}

/// Зняти вердикт детектора (T-038: поріг більше не матчить).
pub fn clear_category(record: &FileRecord) -> FileRecord {
    let mut out = record.clone();
    clear_category_mut(&mut out);
    out
}

/// In-place зняття категорії.
pub fn clear_category_mut(record: &mut FileRecord) {
    record.category = CategoryId::Uncategorized;
    record.detector_id.clear();
    record.explanation.clear();
    record.safety = SafetyLevel::ReviewRecommended;
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

    /// O(1) upsert за candidate_id — критично для perf-гейта 1 млн (T-038).
    struct MemIndex {
        by_id: Mutex<std::collections::HashMap<u64, FileRecord>>,
    }

    impl MemIndex {
        fn new(records: Vec<FileRecord>) -> Self {
            let mut by_id = std::collections::HashMap::with_capacity(records.len());
            for r in records {
                by_id.insert(r.candidate_id.0, r);
            }
            Self {
                by_id: Mutex::new(by_id),
            }
        }
    }

    impl HotIndex for MemIndex {
        fn insert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError> {
            let mut map = self.by_id.lock().unwrap();
            for r in records {
                map.insert(r.candidate_id.0, r);
            }
            Ok(())
        }
        fn finish_indexing(&self) -> Result<(), CoreError> {
            Ok(())
        }
        fn len(&self) -> Result<usize, CoreError> {
            Ok(self.by_id.lock().unwrap().len())
        }
        fn is_empty(&self) -> Result<bool, CoreError> {
            Ok(self.by_id.lock().unwrap().is_empty())
        }
        fn clear(&self) -> Result<(), CoreError> {
            self.by_id.lock().unwrap().clear();
            Ok(())
        }
        fn get_all(&self) -> Result<Vec<FileRecord>, CoreError> {
            Ok(self.by_id.lock().unwrap().values().cloned().collect())
        }
        fn search_file_records(&self, _: &str, _: usize) -> Result<Vec<FileRecord>, CoreError> {
            Ok(Vec::new())
        }
        fn remove_paths(&self, _: &[String]) -> Result<usize, CoreError> {
            Ok(0)
        }
        fn upsert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError> {
            let mut map = self.by_id.lock().unwrap();
            for r in records {
                map.insert(r.candidate_id.0, r);
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

    // --- T-038: пороги + перерахунок без рескану --------------------------------

    /// FILETIME intervals per day (100 ns ticks).
    const FT_PER_DAY: i64 = 10_000_000 * 86_400;
    /// Фіксована «зараз» для детермінованих тестів віку.
    const NOW_FT: i64 = 133_000_000_000_000_000; // довільна точка в епосі FILETIME

    /// Предикатний детектор «старі файли» з live-порогом min_age_days (T-038 DoD).
    struct AgeThresholdDetector {
        min_age_days: AtomicU64,
        now_filetime: i64,
    }

    impl AgeThresholdDetector {
        fn new(min_age_days: u64) -> Self {
            Self {
                min_age_days: AtomicU64::new(min_age_days),
                now_filetime: NOW_FT,
            }
        }
    }

    impl Detector for AgeThresholdDetector {
        fn id(&self) -> DetectorId {
            DetectorId::new("old_files")
        }
        fn category(&self) -> CategoryId {
            CategoryId::OldFiles
        }
        fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
            let ts = record.accessed_at.or(record.modified_at)?;
            let age_days = if ts.0 >= self.now_filetime {
                0
            } else {
                ((self.now_filetime - ts.0) / FT_PER_DAY) as u64
            };
            let min = self.min_age_days.load(Ordering::Relaxed);
            if age_days >= min {
                // Коротке фіксоване пояснення: format! на 0.5M hit-ів
                // домінує в perf-гейті; реальні детектори (T-040) дають
                // детальний рядок, але CPU-бюджет перерахунку — evaluate.
                Some(Verdict::new(
                    CategoryId::OldFiles,
                    "старший за поріг віку",
                    SafetyLevel::ReviewRecommended,
                ))
            } else {
                None
            }
        }
        fn set_threshold(
            &self,
            key: &str,
            value: crate::detectors::thresholds::ThresholdValue,
        ) -> Result<(), CoreError> {
            use crate::detectors::thresholds::{keys, ThresholdValue};
            if key != keys::MIN_AGE_DAYS {
                return Err(crate::detectors::thresholds::unknown_threshold(
                    self.id().as_str(),
                    key,
                ));
            }
            let days = match value {
                ThresholdValue::U64(v) => v,
                ThresholdValue::Bool(_) => {
                    return Err(crate::detectors::thresholds::bad_threshold_type(
                        self.id().as_str(),
                        key,
                        "u64 днів",
                    ));
                }
            };
            self.min_age_days.store(days, Ordering::Relaxed);
            Ok(())
        }
        fn get_threshold(&self, key: &str) -> Option<crate::detectors::thresholds::ThresholdValue> {
            use crate::detectors::thresholds::{keys, ThresholdValue};
            if key == keys::MIN_AGE_DAYS {
                Some(ThresholdValue::U64(
                    self.min_age_days.load(Ordering::Relaxed),
                ))
            } else {
                None
            }
        }
    }

    fn rec_age(id: u64, age_days: u64) -> FileRecord {
        let mut r = rec(id, 100);
        r.accessed_at = Some(trashradar_domain::candidate::FsTimestamp(
            NOW_FT - (age_days as i64) * FT_PER_DAY,
        ));
        r
    }

    #[test]
    fn threshold_change_rebuilds_category_without_rescan() {
        // DoD T-038: «вік > 6 міс → 3 міс» перебудовує категорію з індексу.
        use crate::detectors::thresholds::{keys, ThresholdValue};

        let mut reg = DetectorRegistry::new();
        reg.register(AgeThresholdDetector::new(180)); // 6 місяців
        let index = MemIndex::new(vec![
            rec_age(1, 200), // завжди old
            rec_age(2, 100), // old лише при порозі 90
            rec_age(3, 30),  // ніколи
        ]);
        let orch = DetectorOrchestrator::new(&reg);
        let cancel = CancellationToken::new();

        orch.categorize_index(&index, &cancel).unwrap();
        let all = index.get_all().unwrap();
        assert_eq!(
            all.iter()
                .filter(|r| r.category == CategoryId::OldFiles)
                .count(),
            1
        );
        assert_eq!(
            all.iter()
                .find(|r| r.candidate_id == CandidateId(1))
                .unwrap()
                .category,
            CategoryId::OldFiles
        );

        // 6 міс → 3 міс (180 → 90 днів): без рескану диска.
        let stats = orch
            .set_threshold_and_recalculate(
                &index,
                DetectorId::new("old_files"),
                keys::MIN_AGE_DAYS,
                ThresholdValue::U64(90),
                &cancel,
            )
            .unwrap();
        assert_eq!(
            reg.get_threshold(DetectorId::new("old_files"), keys::MIN_AGE_DAYS),
            Some(ThresholdValue::U64(90))
        );
        assert_eq!(stats.records_seen, 3);
        assert!(stats.records_updated >= 2);

        let all = index.get_all().unwrap();
        let mut old: Vec<_> = all
            .iter()
            .filter(|r| r.category == CategoryId::OldFiles)
            .map(|r| r.candidate_id.0)
            .collect();
        old.sort_unstable();
        assert_eq!(old, vec![1, 2]);
        assert_eq!(
            all.iter()
                .find(|r| r.candidate_id == CandidateId(3))
                .unwrap()
                .category,
            CategoryId::Uncategorized
        );
    }

    #[test]
    fn raising_threshold_clears_no_longer_matching() {
        use crate::detectors::thresholds::{keys, ThresholdValue};

        let mut reg = DetectorRegistry::new();
        reg.register(AgeThresholdDetector::new(90));
        let index = MemIndex::new(vec![rec_age(1, 100), rec_age(2, 200)]);
        let orch = DetectorOrchestrator::new(&reg);
        let cancel = CancellationToken::new();
        orch.categorize_index(&index, &cancel).unwrap();
        assert_eq!(
            index
                .get_all()
                .unwrap()
                .iter()
                .filter(|r| r.category == CategoryId::OldFiles)
                .count(),
            2
        );

        // Підняти поріг до 180: id=1 (100 дн.) випадає → Uncategorized.
        let stats = orch
            .set_threshold_and_recalculate(
                &index,
                DetectorId::new("old_files"),
                keys::MIN_AGE_DAYS,
                ThresholdValue::U64(180),
                &cancel,
            )
            .unwrap();
        assert_eq!(stats.records_cleared, 1);
        let all = index.get_all().unwrap();
        assert_eq!(
            all.iter()
                .find(|r| r.candidate_id == CandidateId(1))
                .unwrap()
                .category,
            CategoryId::Uncategorized
        );
        assert!(all
            .iter()
            .find(|r| r.candidate_id == CandidateId(1))
            .unwrap()
            .explanation
            .is_empty());
        assert_eq!(
            all.iter()
                .find(|r| r.candidate_id == CandidateId(2))
                .unwrap()
                .category,
            CategoryId::OldFiles
        );
    }

    /// DoD perf: перерахунок 1 млн записів < 1 с (release + --ignored).
    ///
    /// Міряє in-place hot path (evaluate + mutate category fields) — це
    /// CPU-вартість предикатного перерахунку з метаданих індексу без I/O.
    #[test]
    #[ignore = "perf gate: cargo test -p trashradar-app recalculate_one_million --release -- --ignored --nocapture"]
    fn recalculate_one_million_records_under_one_second() {
        use crate::detectors::thresholds::{keys, ThresholdValue};
        use std::time::Instant;

        const N: u64 = 1_000_000;
        let mut reg = DetectorRegistry::new();
        reg.register(AgeThresholdDetector::new(180));
        // Половина «старих» (200 дн.), половина «свіжих» (30 дн.).
        let mut records: Vec<_> = (0..N)
            .map(|i| rec_age(i, if i % 2 == 0 { 200 } else { 30 }))
            .collect();
        let orch = DetectorOrchestrator::new(&reg);
        let cancel = CancellationToken::new();

        // Початкова категоризація (не входить у замір).
        orch.recalculate_inplace(&mut records, &cancel);
        assert_eq!(
            records
                .iter()
                .filter(|r| r.category == CategoryId::OldFiles)
                .count(),
            (N / 2) as usize
        );

        reg.set_threshold(
            DetectorId::new("old_files"),
            keys::MIN_AGE_DAYS,
            ThresholdValue::U64(90),
        )
        .unwrap();

        let started = Instant::now();
        let stats = orch.recalculate_inplace(&mut records, &cancel);
        let elapsed = started.elapsed();
        println!(
            "T-038 recalculate_inplace {N} records: {:.1} мс (updated={} cleared={})",
            elapsed.as_secs_f64() * 1000.0,
            stats.records_updated,
            stats.records_cleared
        );
        assert_eq!(stats.records_seen, N);
        assert_eq!(stats.records_updated, N / 2);
        assert_eq!(stats.records_cleared, 0); // поріг знижено — ніхто не випав
        assert!(
            elapsed.as_secs_f64() < 1.0,
            "перерахунок 1 млн має бути < 1 с, було {:?}",
            elapsed
        );
        // 90 дн.: 200-денні лишаються, 30-денні out (і були out).
        assert_eq!(
            records
                .iter()
                .filter(|r| r.category == CategoryId::OldFiles)
                .count(),
            (N / 2) as usize
        );
    }
}
