//! Готова ферма предикатних детекторів MVP для live-скану (T-055).
//!
//! Без реєстрів локацій/маркерів (temp/app_caches/dev — окремий wiring
//! коли shell підвантажить known-locations / structural markers).

use crate::detectors::{
    ArchivesDetector, DetectorRegistry, ForgottenVideosDetector, InstallersDetector,
    LargeFilesDetector, OldFilesDetector,
};

/// Предикатні детектори, що працюють лише з метаданими `FileRecord`.
pub fn mvp_predicate_registry() -> DetectorRegistry {
    let mut reg = DetectorRegistry::new();
    reg.register(LargeFilesDetector::new());
    reg.register(OldFilesDetector::new());
    reg.register(ForgottenVideosDetector::new());
    reg.register(ArchivesDetector::new());
    reg.register(InstallersDetector::new());
    reg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::DetectorOrchestrator;
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
        SafetyLevel,
    };
    use trashradar_domain::category::CategoryId;

    #[test]
    fn farm_registers_five_predicate_detectors() {
        let reg = mvp_predicate_registry();
        assert_eq!(reg.len(), 5);
        assert_eq!(reg.enabled().count(), 5);
    }

    #[test]
    fn large_archive_gets_categorized() {
        let reg = mvp_predicate_registry();
        let orch = DetectorOrchestrator::new(&reg);
        let rec = FileRecord {
            candidate_id: CandidateId(1),
            path: r"C:\data\big.zip".into(),
            size: ByteSize(200 * 1024 * 1024),
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
        };
        let out = orch.categorize_batch(&[rec]);
        assert_eq!(out.stats.records_updated, 1);
        // LargeFiles і/або Archives — primary = перший у реєстрі (LargeFiles).
        assert_ne!(out.updated[0].category, CategoryId::Uncategorized);
        assert!(!out.hits.is_empty());
    }
}
