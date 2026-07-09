//! Детектор «Інсталятори та ISO» (T-043).
//!
//! product.md §5.3 / tasks.md:
//! - **ISO/образи** ([`FileKind::DiskImage`]) — **будь-де**;
//! - **інсталятори** ([`FileKind::Installer`]: exe/msi/…) — лише в
//!   Downloads-подібних локаціях (`Downloads` / `Download` / `Завантаження`).
//!
//! Категорія: [`CategoryId::Installers`]. Пояснення розрізняє тип.

use std::sync::atomic::{AtomicBool, Ordering};

use super::contract::{Detector, DetectorId};
use super::format::{disk_image_explanation, installer_explanation, is_downloads_like_path};
use trashradar_domain::candidate::{CandidateUnit, FileKind, FileRecord, SafetyLevel, Verdict};
use trashradar_domain::category::CategoryId;

/// Стабільний id детектора.
pub const DETECTOR_ID: DetectorId = DetectorId::new("installers");

/// Предикатний детектор [`CategoryId::Installers`].
#[derive(Debug)]
pub struct InstallersDetector {
    enabled: AtomicBool,
}

impl InstallersDetector {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(true),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }
}

impl Default for InstallersDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector for InstallersDetector {
    fn id(&self) -> DetectorId {
        DETECTOR_ID
    }

    fn category(&self) -> CategoryId {
        CategoryId::Installers
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn evaluate(&self, record: &FileRecord) -> Option<Verdict> {
        if record.unit != CandidateUnit::File {
            return None;
        }

        match record.kind {
            FileKind::DiskImage => Some(Verdict::new(
                CategoryId::Installers,
                disk_image_explanation(record.size.0),
                SafetyLevel::ReviewRecommended,
            )),
            FileKind::Installer if is_downloads_like_path(&record.path) => Some(Verdict::new(
                CategoryId::Installers,
                installer_explanation(record.size.0),
                SafetyLevel::ReviewRecommended,
            )),
            _ => None,
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

    fn file(id: u64, path: &str, kind: FileKind, size: u64) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: path.into(),
            size: ByteSize(size),
            created_at: None,
            modified_at: None,
            accessed_at: None,
            kind,
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
    fn iso_img_anywhere() {
        let det = InstallersDetector::new();
        for (id, path, kind) in [
            (1u64, r"D:\isos\win.iso", FileKind::DiskImage),
            (2, r"C:\temp\disk.img", FileKind::DiskImage),
            (3, r"E:\vhds\vm.vhdx", FileKind::DiskImage),
        ] {
            let v = det
                .evaluate(&file(id, path, kind, 4_000_000_000))
                .unwrap_or_else(|| panic!("{path}"));
            assert_eq!(v.category, CategoryId::Installers);
            assert!(v.explanation.contains("образ"), "{}", v.explanation);
        }
    }

    #[test]
    fn installer_only_in_downloads_like() {
        let det = InstallersDetector::new();
        let ok = det
            .evaluate(&file(
                1,
                r"C:\Users\Ada\Downloads\Setup.exe",
                FileKind::Installer,
                80_000_000,
            ))
            .expect("Downloads");
        assert!(ok.explanation.contains("інсталятор"), "{}", ok.explanation);

        assert!(det
            .evaluate(&file(
                2,
                r"C:\Users\Ada\Download\app.msi",
                FileKind::Installer,
                10_000_000,
            ))
            .is_some());

        assert!(det
            .evaluate(&file(
                3,
                r"C:\Users\Ada\Завантаження\game.exe",
                FileKind::Installer,
                10_000_000,
            ))
            .is_some());

        // Program Files / System32 — не Downloads
        assert!(det
            .evaluate(&file(
                4,
                r"C:\Program Files\App\setup.exe",
                FileKind::Installer,
                10_000_000,
            ))
            .is_none());
        assert!(det
            .evaluate(&file(
                5,
                r"C:\Windows\System32\msiexec.exe",
                FileKind::Installer,
                1_000_000,
            ))
            .is_none());
    }

    #[test]
    fn false_positive_windows_backup_path() {
        let det = InstallersDetector::new();
        assert!(det
            .evaluate(&file(
                1,
                r"C:\WindowsBackup\setup.exe",
                FileKind::Installer,
                5_000_000,
            ))
            .is_none());
    }

    #[test]
    fn non_relevant_kinds_rejected() {
        let det = InstallersDetector::new();
        assert!(det
            .evaluate(&file(
                1,
                r"C:\Users\Ada\Downloads\movie.mp4",
                FileKind::Video,
                9_000_000_000,
            ))
            .is_none());
        assert!(det
            .evaluate(&file(
                2,
                r"C:\Users\Ada\Downloads\pack.zip",
                FileKind::Archive,
                500_000_000,
            ))
            .is_none());
    }

    #[test]
    fn folder_units_ignored() {
        let det = InstallersDetector::new();
        let mut r = file(
            1,
            r"C:\Users\Ada\Downloads\Setup.exe",
            FileKind::Installer,
            1,
        );
        r.unit = CandidateUnit::Folder;
        assert!(det.evaluate(&r).is_none());
    }

    #[test]
    fn orchestrator_registers_without_special_casing() {
        let mut reg = DetectorRegistry::new();
        reg.register(InstallersDetector::new());
        let orch = DetectorOrchestrator::new(&reg);
        let batch = [
            file(
                1,
                r"C:\Users\Ada\Downloads\a.exe",
                FileKind::Installer,
                1_000_000,
            ),
            file(2, r"D:\media\b.iso", FileKind::DiskImage, 2_000_000_000),
            file(3, r"C:\Tools\c.exe", FileKind::Installer, 1_000_000),
        ];
        let out = orch.categorize_batch(&batch);
        assert_eq!(out.stats.records_updated, 2);
        let ids: Vec<_> = out.updated.iter().map(|r| r.candidate_id.0).collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
        assert!(!ids.contains(&3));
    }

    #[test]
    fn default_id_and_category() {
        let det = InstallersDetector::new();
        assert_eq!(det.id(), DETECTOR_ID);
        assert_eq!(det.category(), CategoryId::Installers);
    }
}
