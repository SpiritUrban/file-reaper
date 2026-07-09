//! Прогноз «диск після очищення» (T-056) поверх Aggregator / індексу.
//!
//! - **позначення** (`Decision::Marked`) → `marked_bytes` → free_after ↑
//! - **reap** → marked → quarantine held (free **не** змінюється)
//! - **purge** → held ↓, free_now ↑; free_after збігається з free_now
//!
//! I/O тома (capacity/free) — інжектований [`VolumeUsage`]; журнал
//! Quarantine T-078 підключить реальні held-байти.

use std::collections::HashSet;

use trashradar_domain::candidate::{Decision, FileRecord};
use trashradar_domain::forecast::{
    compute_cleanup_forecast, CleanupForecast, ForecastInputs, VolumeUsage,
};

/// Облік байтів, утриманих у Quarantine (до purge).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuarantineHeld {
    bytes: u64,
}

impl QuarantineHeld {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bytes(bytes: u64) -> Self {
        Self { bytes }
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Reap: файли переїхали в Quarantine (same-volume move).
    pub fn on_reap(&mut self, size_bytes: u64) {
        self.bytes = self.bytes.saturating_add(size_bytes);
    }

    /// Purge: місце **фактично** звільняється (caller також ↑ free_now).
    pub fn on_purge(&mut self, size_bytes: u64) {
        self.bytes = self.bytes.saturating_sub(size_bytes);
    }

    /// Restore: файл повернуто з Quarantine.
    pub fn on_restore(&mut self, size_bytes: u64) {
        self.bytes = self.bytes.saturating_sub(size_bytes);
    }
}

/// Живий калькулятор прогнозу для тома (T-056).
#[derive(Debug, Clone)]
pub struct DiskForecast {
    usage: VolumeUsage,
    held: QuarantineHeld,
    marked_bytes: u64,
}

impl DiskForecast {
    pub fn new(usage: VolumeUsage) -> Self {
        Self {
            usage,
            held: QuarantineHeld::new(),
            marked_bytes: 0,
        }
    }

    pub fn usage(&self) -> &VolumeUsage {
        &self.usage
    }

    pub fn set_usage(&mut self, usage: VolumeUsage) {
        self.usage = usage;
    }

    pub fn quarantine_held(&self) -> &QuarantineHeld {
        &self.held
    }

    pub fn quarantine_held_mut(&mut self) -> &mut QuarantineHeld {
        &mut self.held
    }

    pub fn marked_bytes(&self) -> u64 {
        self.marked_bytes
    }

    /// Перерахувати marked з індексу (Decision::Marked, унікальні id).
    pub fn set_marked_from_records(&mut self, records: &[FileRecord]) {
        self.marked_bytes = marked_unique_bytes(records);
    }

    pub fn set_marked_bytes(&mut self, bytes: u64) {
        self.marked_bytes = bytes;
    }

    /// Поточний прогноз.
    pub fn forecast(&self) -> CleanupForecast {
        compute_cleanup_forecast(&ForecastInputs {
            usage: self.usage.clone(),
            quarantine_held_bytes: self.held.bytes(),
            marked_bytes: self.marked_bytes,
        })
    }

    /// Reap marked → held; marked скидаємо на 0 (або caller оновить з індексу).
    ///
    /// `free_now` **не** змінюється (architecture.md §7.1).
    pub fn apply_reap(&mut self, reaped_bytes: u64) {
        self.held.on_reap(reaped_bytes);
        self.marked_bytes = self.marked_bytes.saturating_sub(reaped_bytes);
    }

    /// Purge: held ↓ і free_now ↑ на той самий обсяг (факт на томі).
    pub fn apply_purge(&mut self, purged_bytes: u64) {
        let n = purged_bytes.min(self.held.bytes());
        self.held.on_purge(n);
        let free = self.usage.free_bytes.saturating_add(n);
        self.usage = VolumeUsage::new(self.usage.volume.clone(), self.usage.capacity_bytes, free);
    }

    /// Restore: held ↓ без зміни free (файл знову «зайнятий» як звичайний).
    pub fn apply_restore(&mut self, restored_bytes: u64) {
        self.held.on_restore(restored_bytes);
    }
}

/// Сума розмірів записів з [`Decision::Marked`] (унікальні `candidate_id`).
pub fn marked_unique_bytes(records: &[FileRecord]) -> u64 {
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for r in records {
        if r.decision != Decision::Marked {
            continue;
        }
        if seen.insert(r.candidate_id.0) {
            total = total.saturating_add(r.size.0);
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use trashradar_domain::candidate::{
        ByteSize, CandidateId, CandidateUnit, FileAttributes, FileKind, SafetyLevel,
    };
    use trashradar_domain::category::CategoryId;

    fn rec(id: u64, size: u64, decision: Decision) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: format!(r"C:\x\{id}"),
            size: ByteSize(size),
            created_at: None,
            modified_at: None,
            accessed_at: None,
            kind: FileKind::Other,
            unit: CandidateUnit::File,
            category: CategoryId::LargeFiles,
            safety: SafetyLevel::ReviewRecommended,
            decision,
            detector_id: "t".into(),
            explanation: "t".into(),
            attributes: FileAttributes::default(),
        }
    }

    #[test]
    fn mark_updates_forecast() {
        let mut df = DiskForecast::new(VolumeUsage::new("C:", 1000, 100));
        assert_eq!(df.forecast().free_after_cleanup_bytes, 100);

        df.set_marked_from_records(&[
            rec(1, 40, Decision::Marked),
            rec(2, 10, Decision::Undecided),
            rec(1, 40, Decision::Marked), // duplicate id
        ]);
        assert_eq!(df.marked_bytes(), 40);
        assert_eq!(df.forecast().free_after_cleanup_bytes, 140);
    }

    #[test]
    fn reap_then_purge_matches_fact() {
        // DoD T-056: прогноз оновлюється на reap; після purge == факт.
        let mut df = DiskForecast::new(VolumeUsage::new("C:", 1000, 100));
        df.set_marked_bytes(200);
        let before_reap = df.forecast();
        assert_eq!(before_reap.free_after_cleanup_bytes, 300);
        assert_eq!(before_reap.free_now_bytes, 100);

        // reap 200
        df.apply_reap(200);
        let after_reap = df.forecast();
        assert_eq!(after_reap.free_now_bytes, 100, "reap не звільняє місце");
        assert_eq!(after_reap.quarantine_held_bytes, 200);
        assert_eq!(after_reap.marked_bytes, 0);
        assert_eq!(after_reap.free_after_cleanup_bytes, 300);

        // purge 200
        df.apply_purge(200);
        let after_purge = df.forecast();
        assert_eq!(after_purge.free_now_bytes, 300);
        assert_eq!(after_purge.free_after_cleanup_bytes, 300);
        assert_eq!(after_purge.pending_free_bytes(), 0);
    }

    #[test]
    fn quarantine_held_plus_marked() {
        let mut df = DiskForecast::new(VolumeUsage::new("E:", 500, 50));
        df.quarantine_held_mut().on_reap(30);
        df.set_marked_bytes(20);
        let f = df.forecast();
        assert_eq!(f.free_after_cleanup_bytes, 100);
        assert_eq!(f.quarantine_held_bytes, 30);
        assert_eq!(f.marked_bytes, 20);
    }
}
