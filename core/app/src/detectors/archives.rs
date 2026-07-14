//! Детектор «Архіви» (T-042).
//!
//! product.md §5.3: zip/rar/7z/… (і інші з [`FileKind::Archive`]),
//! особливо великі. Предикат: **Archive ∧ size ≥ min_size_bytes**.
//!
//! Пояснення: «архів N ГБ». Поріг `min_size_bytes` — live (T-038).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::contract::{Detector, DetectorId};
use super::format::archive_explanation;
use super::thresholds::{self, keys, ThresholdValue};
use trashradar_domain::candidate::{CandidateUnit, FileKind, FileRecord, SafetyLevel, Verdict};
use trashradar_domain::category::CategoryId;
use trashradar_domain::error::CoreError;

/// Стабільний id детектора.
pub const DETECTOR_ID: DetectorId = DetectorId::new("archives");

/// Дефолт: 50 МіБ — дрібні zip з кодом/ассетами не смітять категорію.
pub const DEFAULT_MIN_SIZE_BYTES: u64 = 50 * 1024 * 1024;

/// Предикатний детектор [`CategoryId::Archives`].
#[derive(Debug)]
pub struct ArchivesDetector {
    min_size_bytes: AtomicU64,
    enabled: AtomicBool,
}

impl ArchivesDetector {
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

impl Default for ArchivesDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector for ArchivesDetector {
    fn id(&self) -> DetectorId {
        DETECTOR_ID
    }

    fn category(&self) -> CategoryId {
        CategoryId::Archives
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
        if record.kind != FileKind::Archive {
            return None;
        }
        let min = self.min_size_bytes.load(Ordering::Relaxed);
        if record.size.0 < min {
            return None;
        }
        Some(Verdict::new(
            CategoryId::Archives,
            archive_explanation(record.size.0),
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
        ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileRecord,
    };

    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;

    fn archive(id: u64, size: u64, ext: &str) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: format!("C:\\dl\\pack_{id}.{ext}"),
            size: ByteSize(size),
            created_at: None,
            modified_at: None,
            accessed_at: None,
            kind: FileKind::Archive,
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
    fn zip_rar_7z_over_threshold_match() {
        let det = ArchivesDetector::with_min_size(50 * MIB);
        for (id, ext) in [(1u64, "zip"), (2, "rar"), (3, "7z"), (4, "tar"), (5, "gz")] {
            let v = det
                .evaluate(&archive(id, 200 * MIB, ext))
                .unwrap_or_else(|| panic!("{ext} must match"));
            assert_eq!(v.category, CategoryId::Archives);
            assert!(v.explanation.contains("архів"), "{}", v.explanation);
            assert!(v.explanation.contains("ГБ"), "{}", v.explanation);
        }
    }

    #[test]
    fn non_archive_rejected() {
        let det = ArchivesDetector::with_min_size(1);
        let mut r = archive(1, GIB, "zip");
        r.kind = FileKind::Video;
        assert!(det.evaluate(&r).is_none());
        r.kind = FileKind::Installer;
        assert!(det.evaluate(&r).is_none());
    }

    #[test]
    fn below_threshold_rejected() {
        let det = ArchivesDetector::with_min_size(100 * MIB);
        assert!(det.evaluate(&archive(1, 50 * MIB, "zip")).is_none());
        assert!(det.evaluate(&archive(2, 100 * MIB, "zip")).is_some());
    }

    #[test]
    fn folder_units_ignored() {
        let det = ArchivesDetector::with_min_size(1);
        let mut r = archive(1, GIB, "zip");
        r.unit = CandidateUnit::Folder;
        assert!(det.evaluate(&r).is_none());
    }

    #[test]
    fn threshold_live_via_registry() {
        let mut reg = DetectorRegistry::new();
        reg.register(ArchivesDetector::with_min_size(GIB));
        let orch = DetectorOrchestrator::new(&reg);
        let batch = [archive(1, 200 * MIB, "7z")];
        assert_eq!(orch.categorize_batch(&batch).stats.records_updated, 0);

        reg.set_threshold(
            DETECTOR_ID,
            keys::MIN_SIZE_BYTES,
            ThresholdValue::U64(100 * MIB),
        )
        .unwrap();
        let out = orch.recalculate_batch(&batch);
        assert_eq!(out.stats.records_updated, 1);
        assert_eq!(out.updated[0].category, CategoryId::Archives);
        assert_eq!(
            reg.get_threshold(DETECTOR_ID, keys::MIN_SIZE_BYTES),
            Some(ThresholdValue::U64(100 * MIB))
        );
    }

    #[test]
    fn default_id_and_threshold() {
        let det = ArchivesDetector::new();
        assert_eq!(det.id(), DETECTOR_ID);
        assert_eq!(det.category(), CategoryId::Archives);
        assert_eq!(det.min_size_bytes(), DEFAULT_MIN_SIZE_BYTES);
    }
}
