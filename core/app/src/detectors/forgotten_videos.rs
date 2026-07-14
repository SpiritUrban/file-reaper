//! Детектор «Забуті відео» (T-041).
//!
//! product.md §5.3: великі відео з давнім останнім доступом (записи стрімів,
//! сирці, дублі зйомок). Комбінований предикат:
//! **FileKind::Video ∧ size ≥ min_size ∧ age ≥ min_age_days**.
//!
//! DoD: пояснення містить **розмір** і **давність доступу**
//! (напр. «відео 4.2 ГБ, останній доступ 8 міс. тому»).
//!
//! Пороги `min_size_bytes` і `min_age_days` — live (T-038).

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use super::contract::{Detector, DetectorId};
use super::format::{age_days_filetime, forgotten_video_explanation, system_now_filetime};
use super::thresholds::{self, keys, ThresholdValue};
use trashradar_domain::candidate::{CandidateUnit, FileKind, FileRecord, SafetyLevel, Verdict};
use trashradar_domain::category::CategoryId;
use trashradar_domain::error::CoreError;

/// Стабільний id детектора.
pub const DETECTOR_ID: DetectorId = DetectorId::new("forgotten_videos");

/// Дефолт: 100 МіБ — відсікає дрібні кліпи/прев’ю.
pub const DEFAULT_MIN_SIZE_BYTES: u64 = 100 * 1024 * 1024;

/// Дефолт: 180 днів (6 міс.) — «забуте», не обов’язково «роками».
pub const DEFAULT_MIN_AGE_DAYS: u64 = 180;

const NOW_USE_SYSTEM: i64 = 0;

/// Предикатний детектор [`CategoryId::ForgottenVideos`].
#[derive(Debug)]
pub struct ForgottenVideosDetector {
    min_size_bytes: AtomicU64,
    min_age_days: AtomicU64,
    enabled: AtomicBool,
    now_filetime: AtomicI64,
}

impl ForgottenVideosDetector {
    pub fn new() -> Self {
        Self::with_thresholds(DEFAULT_MIN_SIZE_BYTES, DEFAULT_MIN_AGE_DAYS)
    }

    pub fn with_thresholds(min_size_bytes: u64, min_age_days: u64) -> Self {
        Self {
            min_size_bytes: AtomicU64::new(min_size_bytes),
            min_age_days: AtomicU64::new(min_age_days),
            enabled: AtomicBool::new(true),
            now_filetime: AtomicI64::new(NOW_USE_SYSTEM),
        }
    }

    pub fn with_now_filetime(self, now: i64) -> Self {
        self.now_filetime.store(now, Ordering::Relaxed);
        self
    }

    pub fn set_now_filetime(&self, now: Option<i64>) {
        self.now_filetime
            .store(now.unwrap_or(NOW_USE_SYSTEM), Ordering::Relaxed);
    }

    pub fn min_size_bytes(&self) -> u64 {
        self.min_size_bytes.load(Ordering::Relaxed)
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

impl Default for ForgottenVideosDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector for ForgottenVideosDetector {
    fn id(&self) -> DetectorId {
        DETECTOR_ID
    }

    fn category(&self) -> CategoryId {
        CategoryId::ForgottenVideos
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), CoreError> {
        self.enabled.store(enabled, Ordering::Relaxed);
        Ok(())
    }

    fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
        if record.unit != CandidateUnit::File {
            return None;
        }
        if record.kind != FileKind::Video {
            return None;
        }

        let min_size = self.min_size_bytes.load(Ordering::Relaxed);
        if record.size.0 < min_size {
            return None;
        }

        let (ts, from_access) = match (record.accessed_at, record.modified_at) {
            (Some(a), _) => (a.0, true),
            (None, Some(m)) => (m.0, false),
            (None, None) => return None,
        };

        let age = age_days_filetime(self.now(), ts);
        let min_age = self.min_age_days.load(Ordering::Relaxed);
        if age < min_age {
            return None;
        }

        Some(Verdict::new(
            CategoryId::ForgottenVideos,
            forgotten_video_explanation(record.size.0, age, from_access),
            SafetyLevel::ReviewRecommended,
        ))
    }

    fn set_threshold(&self, key: &str, value: ThresholdValue) -> Result<(), CoreError> {
        match key {
            keys::MIN_SIZE_BYTES => {
                let bytes = value.as_u64().ok_or_else(|| {
                    thresholds::bad_threshold_type(self.id().as_str(), key, "u64 байт")
                })?;
                self.min_size_bytes.store(bytes, Ordering::Relaxed);
                Ok(())
            }
            keys::MIN_AGE_DAYS => {
                let days = value.as_u64().ok_or_else(|| {
                    thresholds::bad_threshold_type(self.id().as_str(), key, "u64 днів")
                })?;
                self.min_age_days.store(days, Ordering::Relaxed);
                Ok(())
            }
            _ => Err(thresholds::unknown_threshold(self.id().as_str(), key)),
        }
    }

    fn get_threshold(&self, key: &str) -> Option<ThresholdValue> {
        match key {
            keys::MIN_SIZE_BYTES => Some(ThresholdValue::U64(self.min_size_bytes())),
            keys::MIN_AGE_DAYS => Some(ThresholdValue::U64(self.min_age_days())),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::format::FILETIME_PER_DAY;
    use crate::detectors::{DetectorOrchestrator, DetectorRegistry};
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileRecord, FsTimestamp,
    };

    const NOW: i64 = 133_000_000_000_000_000;
    const GIB: u64 = 1024 * 1024 * 1024;

    fn video(id: u64, size: u64, age_days: u64) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: format!("C:\\vids\\clip_{id}.mp4"),
            size: ByteSize(size),
            created_at: None,
            modified_at: None,
            accessed_at: Some(FsTimestamp(NOW - (age_days as i64) * FILETIME_PER_DAY)),
            kind: FileKind::Video,
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
    fn matches_video_large_and_old_with_size_and_age_in_explanation() {
        let det =
            ForgottenVideosDetector::with_thresholds(100 * 1024 * 1024, 180).with_now_filetime(NOW);
        let v = det
            .evaluate(&video(1, 4 * GIB, 240))
            .expect("4 ГБ, 240 дн.");
        assert_eq!(v.category, CategoryId::ForgottenVideos);
        assert!(v.explanation.contains("відео"), "{}", v.explanation);
        assert!(
            v.explanation.contains("ГБ"),
            "DoD: розмір у поясненні: {}",
            v.explanation
        );
        assert!(
            v.explanation.contains("доступ") || v.explanation.contains("зміна"),
            "DoD: давність у поясненні: {}",
            v.explanation
        );
        assert!(v.explanation.contains("тому"), "{}", v.explanation);
        assert!(v.is_complete());
    }

    #[test]
    fn rejects_non_video() {
        let det = ForgottenVideosDetector::with_thresholds(1, 1).with_now_filetime(NOW);
        let mut r = video(1, GIB, 400);
        r.kind = FileKind::Image;
        assert!(det.evaluate(&r).is_none());
        r.kind = FileKind::Archive;
        assert!(det.evaluate(&r).is_none());
    }

    #[test]
    fn rejects_small_or_recent_video() {
        let det = ForgottenVideosDetector::with_thresholds(GIB, 180).with_now_filetime(NOW);
        // великий, але свіжий
        assert!(det.evaluate(&video(1, 2 * GIB, 30)).is_none());
        // старий, але малий
        assert!(det.evaluate(&video(2, 10 * 1024 * 1024, 400)).is_none());
        // обидва пороги
        assert!(det.evaluate(&video(3, 2 * GIB, 200)).is_some());
    }

    #[test]
    fn falls_back_to_modified_when_no_access() {
        let det = ForgottenVideosDetector::with_thresholds(1, 30).with_now_filetime(NOW);
        let mut r = video(1, GIB, 90);
        r.accessed_at = None;
        r.modified_at = Some(FsTimestamp(NOW - 90 * FILETIME_PER_DAY));
        let v = det.evaluate(&r).expect("modified age");
        assert!(v.explanation.contains("зміна"), "{}", v.explanation);
    }

    #[test]
    fn both_thresholds_live() {
        let mut reg = DetectorRegistry::new();
        reg.register(ForgottenVideosDetector::with_thresholds(GIB, 180).with_now_filetime(NOW));
        let orch = DetectorOrchestrator::new(&reg);
        let batch = [video(1, 500 * 1024 * 1024, 200)]; // 0.5 GiB, 200 дн.

        // size too small for 1 GiB threshold
        assert_eq!(orch.categorize_batch(&batch).stats.records_updated, 0);

        reg.set_threshold(
            DETECTOR_ID,
            keys::MIN_SIZE_BYTES,
            ThresholdValue::U64(100 * 1024 * 1024),
        )
        .unwrap();
        // 200 дн. ≥ 180, size now ok
        let hit = orch.recalculate_batch(&batch);
        assert_eq!(hit.stats.records_updated, 1);
        assert_eq!(hit.updated[0].category, CategoryId::ForgottenVideos);

        reg.set_threshold(DETECTOR_ID, keys::MIN_AGE_DAYS, ThresholdValue::U64(300))
            .unwrap();
        // 200 < 300 → зняти категорію
        let cleared = orch.recalculate_batch(&hit.updated);
        assert_eq!(cleared.stats.records_cleared, 1);
        assert_eq!(cleared.updated[0].category, CategoryId::Uncategorized);

        assert_eq!(
            reg.get_threshold(DETECTOR_ID, keys::MIN_SIZE_BYTES),
            Some(ThresholdValue::U64(100 * 1024 * 1024))
        );
        assert_eq!(
            reg.get_threshold(DETECTOR_ID, keys::MIN_AGE_DAYS),
            Some(ThresholdValue::U64(300))
        );
    }

    #[test]
    fn folder_and_no_dates_skipped() {
        let det = ForgottenVideosDetector::with_thresholds(1, 1).with_now_filetime(NOW);
        let mut folder = video(1, GIB, 999);
        folder.unit = CandidateUnit::Folder;
        assert!(det.evaluate(&folder).is_none());

        let mut no_dates = video(2, GIB, 999);
        no_dates.accessed_at = None;
        no_dates.modified_at = None;
        assert!(det.evaluate(&no_dates).is_none());
    }

    #[test]
    fn default_id_and_thresholds() {
        let det = ForgottenVideosDetector::new();
        assert_eq!(det.id(), DETECTOR_ID);
        assert_eq!(det.category(), CategoryId::ForgottenVideos);
        assert_eq!(det.min_size_bytes(), DEFAULT_MIN_SIZE_BYTES);
        assert_eq!(det.min_age_days(), DEFAULT_MIN_AGE_DAYS);
    }
}
