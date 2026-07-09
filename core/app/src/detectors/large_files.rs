//! Детектор «Великі файли» (T-039).
//!
//! product.md §5.3 / architecture.md §6.1: предикатний детектор —
//! файли з розміром ≥ порогу. Пояснення: «розмір N ГБ» (DoD T-039).
//! Рівень: `review_recommended` (не safe-to-bulk — користувач дивиться).
//!
//! Поріг `min_size_bytes` змінюється «на льоту» (T-038) без рескану.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::contract::{Detector, DetectorId};
use super::format::large_file_explanation;
use super::thresholds::{self, keys, ThresholdValue};
use trashradar_domain::candidate::{CandidateUnit, FileRecord, SafetyLevel, Verdict};
use trashradar_domain::category::CategoryId;
use trashradar_domain::error::CoreError;

/// Стабільний id детектора (реєстр / IPC / settings).
pub const DETECTOR_ID: DetectorId = DetectorId::new("large_files");

/// Дефолтний поріг: 100 МіБ. UI може змінити через `min_size_bytes`.
pub const DEFAULT_MIN_SIZE_BYTES: u64 = 100 * 1024 * 1024;

/// Предикатний детектор категорії [`CategoryId::LargeFiles`].
#[derive(Debug)]
pub struct LargeFilesDetector {
    min_size_bytes: AtomicU64,
    enabled: AtomicBool,
}

impl LargeFilesDetector {
    pub fn new() -> Self {
        Self::with_min_size(DEFAULT_MIN_SIZE_BYTES)
    }

    pub fn with_min_size(min_size_bytes: u64) -> Self {
        Self {
            min_size_bytes: AtomicU64::new(min_size_bytes),
            enabled: AtomicBool::new(true),
        }
    }

    pub fn min_size_bytes(&self) -> u64 {
        self.min_size_bytes.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }
}

impl Default for LargeFilesDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector for LargeFilesDetector {
    fn id(&self) -> DetectorId {
        DETECTOR_ID
    }

    fn category(&self) -> CategoryId {
        CategoryId::LargeFiles
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
        // Папки-одиниці (T-053) не сюди — лише файли.
        if record.unit != CandidateUnit::File {
            return None;
        }
        let min = self.min_size_bytes.load(Ordering::Relaxed);
        if record.size.0 < min {
            return None;
        }
        Some(Verdict::new(
            CategoryId::LargeFiles,
            large_file_explanation(record.size.0),
            SafetyLevel::ReviewRecommended,
        ))
    }

    fn set_threshold(&self, key: &str, value: ThresholdValue) -> Result<(), CoreError> {
        if key != keys::MIN_SIZE_BYTES {
            return Err(thresholds::unknown_threshold(self.id().as_str(), key));
        }
        let bytes = value
            .as_u64()
            .ok_or_else(|| thresholds::bad_threshold_type(self.id().as_str(), key, "u64 байт"))?;
        self.min_size_bytes.store(bytes, Ordering::Relaxed);
        Ok(())
    }

    fn get_threshold(&self, key: &str) -> Option<ThresholdValue> {
        if key == keys::MIN_SIZE_BYTES {
            Some(ThresholdValue::U64(self.min_size_bytes()))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::{DetectorOrchestrator, DetectorRegistry};
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
    };

    fn file(id: u64, size: u64) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: format!("C:\\big\\file_{id}.dat"),
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
    fn files_over_threshold_match_with_size_gb_explanation() {
        let det = LargeFilesDetector::with_min_size(1024 * 1024 * 1024); // 1 GiB
        let hit = det
            .evaluate(&file(1, 4_294_967_296)) // 4 GiB
            .expect("must match");
        assert_eq!(hit.category, CategoryId::LargeFiles);
        assert_eq!(hit.safety, SafetyLevel::ReviewRecommended);
        assert!(
            hit.explanation.starts_with("розмір "),
            "{}",
            hit.explanation
        );
        assert!(
            hit.explanation.contains("ГБ"),
            "DoD: пояснення «розмір N ГБ», got {}",
            hit.explanation
        );
        assert!(hit.is_complete());
    }

    #[test]
    fn files_below_threshold_are_not_candidates() {
        let det = LargeFilesDetector::with_min_size(100 * 1024 * 1024);
        assert!(det.evaluate(&file(1, 50 * 1024 * 1024)).is_none());
        assert!(det.evaluate(&file(2, 100 * 1024 * 1024 - 1)).is_none());
        // Рівно поріг — кандидат.
        assert!(det.evaluate(&file(3, 100 * 1024 * 1024)).is_some());
    }

    #[test]
    fn folder_units_are_ignored() {
        let det = LargeFilesDetector::new();
        let mut folder = file(1, 10 * 1024 * 1024 * 1024);
        folder.unit = CandidateUnit::Folder;
        assert!(det.evaluate(&folder).is_none());
    }

    #[test]
    fn threshold_change_rebuilds_without_rescan() {
        let mut reg = DetectorRegistry::new();
        reg.register(LargeFilesDetector::with_min_size(1_000_000_000));
        let orch = DetectorOrchestrator::new(&reg);
        let batch = [
            file(1, 500_000_000),   // 0.5 GB — only if threshold drops
            file(2, 2_000_000_000), // 2 GB — always
        ];
        let out = orch.categorize_batch(&batch);
        assert_eq!(out.stats.records_updated, 1);
        assert_eq!(out.updated[0].candidate_id, CandidateId(2));
        assert!(out.updated[0].explanation.contains("ГБ"));

        reg.set_threshold(
            DETECTOR_ID,
            keys::MIN_SIZE_BYTES,
            ThresholdValue::U64(400_000_000),
        )
        .unwrap();
        // Перерахунок: обидва матчать; clear не потрібен (порог знижено).
        let out = orch.recalculate_batch(&batch);
        assert_eq!(out.stats.records_updated, 2);
        assert_eq!(
            reg.get_threshold(DETECTOR_ID, keys::MIN_SIZE_BYTES),
            Some(ThresholdValue::U64(400_000_000))
        );
    }

    #[test]
    fn disabled_detector_skipped_by_orchestrator() {
        let det = LargeFilesDetector::with_min_size(1);
        det.set_enabled(false);
        let mut reg = DetectorRegistry::new();
        reg.register(det);
        let orch = DetectorOrchestrator::new(&reg);
        let out = orch.categorize_batch(&[file(1, 99_999_999_999)]);
        assert_eq!(out.stats.detectors_enabled, 0);
        assert!(out.updated.is_empty());
    }

    #[test]
    fn default_id_and_category() {
        let det = LargeFilesDetector::new();
        assert_eq!(det.id(), DETECTOR_ID);
        assert_eq!(det.category(), CategoryId::LargeFiles);
        assert_eq!(det.min_size_bytes(), DEFAULT_MIN_SIZE_BYTES);
    }

    #[test]
    fn unknown_threshold_key_is_rejected() {
        let det = LargeFilesDetector::new();
        let err = det
            .set_threshold("nope", ThresholdValue::U64(1))
            .expect_err("unknown");
        assert_eq!(
            err.code,
            trashradar_domain::error::ErrorCode::InvalidArgument
        );
    }
}
