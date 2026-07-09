//! Ферма детекторів: контракт, реєстр, склад (T-036+).
//!
//! architecture.md §6: кожна категорія — окремий детектор з єдиним контрактом
//! (потік записів індексу → вердикти). repository.md §4: нова категорія =
//! **новий модуль + `register`**, без правок оркестратора (T-037).
//!
//! Конкретні детектори MVP: T-039…T-053; оркестратор прогону — T-037.

mod contract;
mod registry;

pub use contract::{Detector, DetectorHit, DetectorId};
pub use registry::DetectorRegistry;

#[cfg(test)]
mod tests {
    use super::*;
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
        SafetyLevel, Verdict,
    };
    use trashradar_domain::category::CategoryId;

    /// Тестовий детектор: файли з розміром ≥ порогу → LargeFiles.
    /// Живе лише в тестах — доводить DoD T-036 «реєструється без змін оркестратора».
    struct SizeProbeDetector {
        id: DetectorId,
        threshold: u64,
        enabled: bool,
    }

    impl SizeProbeDetector {
        fn new(threshold: u64) -> Self {
            Self {
                id: DetectorId::new("test.size_probe"),
                threshold,
                enabled: true,
            }
        }
    }

    impl Detector for SizeProbeDetector {
        fn id(&self) -> DetectorId {
            self.id
        }

        fn category(&self) -> CategoryId {
            CategoryId::LargeFiles
        }

        fn is_enabled(&self) -> bool {
            self.enabled
        }

        fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
            if record.size.0 >= self.threshold {
                Some(Verdict::new(
                    CategoryId::LargeFiles,
                    format!("розмір {} байт (≥ {})", record.size.0, self.threshold),
                    SafetyLevel::ReviewRecommended,
                ))
            } else {
                None
            }
        }
    }

    fn sample_record(id: u64, size: u64) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: format!("C:\\data\\file_{id}.bin"),
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
    fn verdict_contains_all_three_fields() {
        let v = Verdict::new(
            CategoryId::TempFiles,
            "тимчасовий файл у %TEMP%",
            SafetyLevel::SafeToBulk,
        );
        assert_eq!(v.category, CategoryId::TempFiles);
        assert!(!v.explanation.is_empty());
        assert_eq!(v.safety, SafetyLevel::SafeToBulk);
        assert!(v.is_complete());
    }

    #[test]
    fn test_detector_registers_without_orchestrator_changes() {
        // DoD T-036: реєстрація — лише register(); оркестратор (evaluate_record)
        // не знає конкретного типу SizeProbeDetector.
        let mut farm = DetectorRegistry::new();
        assert_eq!(farm.len(), 0);

        farm.register(SizeProbeDetector::new(1_000_000));
        assert_eq!(farm.len(), 1);
        assert!(farm.get(DetectorId::new("test.size_probe")).is_some());
        assert!(farm.get(DetectorId::new("missing")).is_none());

        // Другий детектор іншої «категорії» — знову лише register.
        struct PathProbe;
        impl Detector for PathProbe {
            fn id(&self) -> DetectorId {
                DetectorId::new("test.path_probe")
            }
            fn category(&self) -> CategoryId {
                CategoryId::Archives
            }
            fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
                if record.path.contains(".zip") {
                    Some(Verdict::new(
                        CategoryId::Archives,
                        "архів за розширенням у шляху",
                        SafetyLevel::ReviewRecommended,
                    ))
                } else {
                    None
                }
            }
        }
        farm.register(PathProbe);
        assert_eq!(farm.len(), 2);

        // Прогін «оркестратора» — універсальний evaluate_record, без match по типах.
        let large = sample_record(1, 5_000_000);
        let hits = farm.evaluate_record(&large);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].detector_id.as_str(), "test.size_probe");
        assert_eq!(hits[0].verdict.category, CategoryId::LargeFiles);
        assert!(hits[0].verdict.explanation.contains("5000000"));
        assert_eq!(hits[0].verdict.safety, SafetyLevel::ReviewRecommended);

        let zip = FileRecord {
            path: "C:\\dl\\pack.zip".into(),
            size: ByteSize(100),
            ..sample_record(2, 100)
        };
        let hits = farm.evaluate_record(&zip);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].detector_id.as_str(), "test.path_probe");
        assert_eq!(hits[0].verdict.category, CategoryId::Archives);
    }

    #[test]
    fn disabled_detector_does_not_receive_stream() {
        // Підготовка T-037: вимкнений детектор не оцінює записи.
        let mut farm = DetectorRegistry::new();
        let mut probe = SizeProbeDetector::new(1);
        probe.enabled = false;
        farm.register(probe);

        let hits = farm.evaluate_record(&sample_record(1, 99_999));
        assert!(hits.is_empty());
        assert_eq!(farm.enabled().count(), 0);
        assert_eq!(farm.iter().count(), 1);
    }

    #[test]
    fn stream_batch_collects_hits_from_all_enabled() {
        let mut farm = DetectorRegistry::new();
        farm.register(SizeProbeDetector::new(1_000));
        struct AlwaysTemp;
        impl Detector for AlwaysTemp {
            fn id(&self) -> DetectorId {
                DetectorId::new("test.always_temp")
            }
            fn category(&self) -> CategoryId {
                CategoryId::TempFiles
            }
            fn evaluate(&self, _: &FileRecord) -> Option<Verdict> {
                Some(Verdict::new(
                    CategoryId::TempFiles,
                    "завжди temp (тест перетину)",
                    SafetyLevel::SafeToBulk,
                ))
            }
        }
        farm.register(AlwaysTemp);

        let records = [
            sample_record(1, 50),    // лише AlwaysTemp
            sample_record(2, 5_000), // SizeProbe + AlwaysTemp
        ];
        let hits = farm.evaluate_stream(records.iter());
        // 1 + 2 = 3 вердикти
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().any(|h| {
            h.candidate_id == CandidateId(2) && h.detector_id.as_str() == "test.size_probe"
        }));
        assert_eq!(
            hits.iter()
                .filter(|h| h.detector_id.as_str() == "test.always_temp")
                .count(),
            2
        );
        // Кожен вердикт — повний (три поля).
        for h in &hits {
            assert!(h.verdict.is_complete());
        }
    }

    #[test]
    fn duplicate_id_is_rejected() {
        let mut farm = DetectorRegistry::new();
        assert!(farm.try_register(SizeProbeDetector::new(1)).is_ok());
        let err = farm
            .try_register(SizeProbeDetector::new(2))
            .expect_err("duplicate id");
        assert!(err.contains("test.size_probe"));
        assert_eq!(farm.len(), 1);
    }
}
