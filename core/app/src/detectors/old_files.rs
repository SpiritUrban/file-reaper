//! Детектор «Старі файли» (T-040).
//!
//! product.md §5.3: файли з давнім доступом/зміною (поріг налаштовується).
//! Предикат: `max(вік)` за `accessed_at`, інакше `modified_at` ≥ `min_age_days`.
//! Пояснення: «останній доступ N міс./р. тому» або «остання зміна …».
//!
//! Поріг `min_age_days` — live (T-038). Годинник інжектиться для тестів
//! (`with_now_filetime`); у проді — системний час.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use super::contract::{Detector, DetectorId};
use super::format::{age_days_filetime, old_file_explanation, system_now_filetime};
use super::thresholds::{self, keys, ThresholdValue};
use trashradar_domain::candidate::{CandidateUnit, FileRecord, SafetyLevel, Verdict};
use trashradar_domain::category::CategoryId;
use trashradar_domain::error::CoreError;

/// Стабільний id детектора.
pub const DETECTOR_ID: DetectorId = DetectorId::new("old_files");

/// Дефолт: 365 днів (1 рік) — «не відкривались роками» у м’якій інтерпретації MVP.
pub const DEFAULT_MIN_AGE_DAYS: u64 = 365;

/// Sentinel: брати системний годинник.
const NOW_USE_SYSTEM: i64 = 0;

/// Предикатний детектор [`CategoryId::OldFiles`].
#[derive(Debug)]
pub struct OldFilesDetector {
    min_age_days: AtomicU64,
    enabled: AtomicBool,
    /// Фіксований «зараз» (FILETIME) для тестів; 0 = system clock.
    now_filetime: AtomicI64,
}

impl OldFilesDetector {
    pub fn new() -> Self {
        Self::with_min_age_days(DEFAULT_MIN_AGE_DAYS)
    }

    pub fn with_min_age_days(min_age_days: u64) -> Self {
        Self {
            min_age_days: AtomicU64::new(min_age_days),
            enabled: AtomicBool::new(true),
            now_filetime: AtomicI64::new(NOW_USE_SYSTEM),
        }
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

    pub fn min_age_days(&self) -> u64 {
        self.min_age_days.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    fn now(&self) -> i64 {
        let n = self.now_filetime.load(Ordering::Relaxed);
        if n == NOW_USE_SYSTEM {
            system_now_filetime()
        } else {
            n
        }
    }
}

impl Default for OldFilesDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector for OldFilesDetector {
    fn id(&self) -> DetectorId {
        DETECTOR_ID
    }

    fn category(&self) -> CategoryId {
        CategoryId::OldFiles
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
        if record.unit != CandidateUnit::File {
            return None;
        }

        let (ts, from_access) = match (record.accessed_at, record.modified_at) {
            (Some(a), _) => (a.0, true),
            (None, Some(m)) => (m.0, false),
            (None, None) => return None, // немає дат — не стверджуємо «старий»
        };

        let age = age_days_filetime(self.now(), ts);
        let min = self.min_age_days.load(Ordering::Relaxed);
        if age < min {
            return None;
        }

        Some(Verdict::new(
            CategoryId::OldFiles,
            old_file_explanation(age, from_access),
            SafetyLevel::ReviewRecommended,
        ))
    }

    fn set_threshold(&self, key: &str, value: ThresholdValue) -> Result<(), CoreError> {
        if key != keys::MIN_AGE_DAYS {
            return Err(thresholds::unknown_threshold(self.id().as_str(), key));
        }
        let days = value
            .as_u64()
            .ok_or_else(|| thresholds::bad_threshold_type(self.id().as_str(), key, "u64 днів"))?;
        self.min_age_days.store(days, Ordering::Relaxed);
        Ok(())
    }

    fn get_threshold(&self, key: &str) -> Option<ThresholdValue> {
        if key == keys::MIN_AGE_DAYS {
            Some(ThresholdValue::U64(self.min_age_days()))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::format::FILETIME_PER_DAY;
    use crate::detectors::{DetectorOrchestrator, DetectorRegistry};
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
        FsTimestamp,
    };

    const NOW: i64 = 133_000_000_000_000_000;

    fn file_age(id: u64, age_days: u64, use_access: bool) -> FileRecord {
        let ts = Some(FsTimestamp(NOW - (age_days as i64) * FILETIME_PER_DAY));
        FileRecord {
            candidate_id: CandidateId(id),
            path: format!("C:\\old\\f{id}.dat"),
            size: ByteSize(1024),
            created_at: None,
            modified_at: if use_access { None } else { ts },
            accessed_at: if use_access { ts } else { None },
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
    fn old_access_matches_with_explanation() {
        let det = OldFilesDetector::with_min_age_days(180).with_now_filetime(NOW);
        let v = det
            .evaluate(&file_age(1, 400, true))
            .expect("400 дн. ≥ 180");
        assert_eq!(v.category, CategoryId::OldFiles);
        assert_eq!(v.safety, SafetyLevel::ReviewRecommended);
        assert!(v.explanation.contains("доступ"), "{}", v.explanation);
        assert!(v.explanation.contains("тому"), "{}", v.explanation);
        assert!(v.is_complete());
    }

    #[test]
    fn falls_back_to_modified_when_no_access() {
        let det = OldFilesDetector::with_min_age_days(30).with_now_filetime(NOW);
        let v = det
            .evaluate(&file_age(1, 90, false))
            .expect("modified 90 дн.");
        assert!(v.explanation.contains("зміна"), "{}", v.explanation);
    }

    #[test]
    fn prefers_access_over_modified() {
        let det = OldFilesDetector::with_min_age_days(30).with_now_filetime(NOW);
        let mut r = file_age(1, 10, true); // access свіжий
        r.modified_at = Some(FsTimestamp(NOW - 500 * FILETIME_PER_DAY)); // modified старий
                                                                         // Вік береться з access → 10 дн. < 30 → не кандидат.
        assert!(det.evaluate(&r).is_none());
    }

    #[test]
    fn below_threshold_not_candidate() {
        let det = OldFilesDetector::with_min_age_days(365).with_now_filetime(NOW);
        assert!(det.evaluate(&file_age(1, 100, true)).is_none());
        assert!(det.evaluate(&file_age(2, 365, true)).is_some());
    }

    #[test]
    fn no_dates_skipped() {
        let det = OldFilesDetector::with_min_age_days(1).with_now_filetime(NOW);
        let mut r = file_age(1, 999, true);
        r.accessed_at = None;
        r.modified_at = None;
        assert!(det.evaluate(&r).is_none());
    }

    #[test]
    fn folder_units_ignored() {
        let det = OldFilesDetector::with_min_age_days(1).with_now_filetime(NOW);
        let mut r = file_age(1, 999, true);
        r.unit = CandidateUnit::Folder;
        assert!(det.evaluate(&r).is_none());
    }

    #[test]
    fn threshold_configurable_live() {
        let mut reg = DetectorRegistry::new();
        reg.register(OldFilesDetector::with_min_age_days(180).with_now_filetime(NOW));
        let orch = DetectorOrchestrator::new(&reg);
        let batch = [file_age(1, 100, true), file_age(2, 200, true)];

        let out = orch.categorize_batch(&batch);
        assert_eq!(out.stats.records_updated, 1); // лише 200

        reg.set_threshold(DETECTOR_ID, keys::MIN_AGE_DAYS, ThresholdValue::U64(90))
            .unwrap();
        let out = orch.recalculate_batch(&batch);
        assert_eq!(out.stats.records_updated, 2);
        assert_eq!(
            reg.get_threshold(DETECTOR_ID, keys::MIN_AGE_DAYS),
            Some(ThresholdValue::U64(90))
        );
    }

    #[test]
    fn raising_threshold_clears_via_orchestrator() {
        let mut reg = DetectorRegistry::new();
        reg.register(OldFilesDetector::with_min_age_days(90).with_now_filetime(NOW));
        let orch = DetectorOrchestrator::new(&reg);
        let batch = [file_age(1, 100, true)];
        let categorized = orch.categorize_batch(&batch).updated;
        assert_eq!(categorized[0].category, CategoryId::OldFiles);

        reg.set_threshold(DETECTOR_ID, keys::MIN_AGE_DAYS, ThresholdValue::U64(180))
            .unwrap();
        let out = orch.recalculate_batch(&categorized);
        assert_eq!(out.stats.records_cleared, 1);
        assert_eq!(out.updated[0].category, CategoryId::Uncategorized);
    }

    #[test]
    fn default_id_category_threshold() {
        let det = OldFilesDetector::new();
        assert_eq!(det.id(), DETECTOR_ID);
        assert_eq!(det.category(), CategoryId::OldFiles);
        assert_eq!(det.min_age_days(), DEFAULT_MIN_AGE_DAYS);
    }
}
