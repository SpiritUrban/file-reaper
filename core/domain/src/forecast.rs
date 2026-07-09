//! Прогноз «диск після очищення» з урахуванням Quarantine (T-056).
//!
//! architecture.md §7.1 / product.md: **фактичне** звільнення місця — лише при
//! **purge** Quarantine. Reap = move у межах тому → `free` **не** змінюється.
//!
//! Прогноз Aggregator-а завжди: `free_after = free_now + quarantine_held + marked`.
//! Після purge `free_now` зростає, `quarantine_held` падає — `free_after` лишається
//! узгодженим і **збігається з фактом**, коли marked=0 і held=0.

use serde::{Deserialize, Serialize};

use crate::candidate::ByteSize;

/// Поточний стан тома (знімок з FS / health; без I/O в domain).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeUsage {
    /// Напр. `"C:"`.
    pub volume: String,
    pub capacity_bytes: u64,
    pub free_bytes: u64,
}

impl VolumeUsage {
    pub fn new(volume: impl Into<String>, capacity_bytes: u64, free_bytes: u64) -> Self {
        let free_bytes = free_bytes.min(capacity_bytes);
        Self {
            volume: volume.into(),
            capacity_bytes,
            free_bytes,
        }
    }

    pub fn used_bytes(&self) -> u64 {
        self.capacity_bytes.saturating_sub(self.free_bytes)
    }

    /// Відсоток **зайнятого** (0–100), як у UI «C: 91%».
    pub fn used_percent(&self) -> u8 {
        percent_of(self.used_bytes(), self.capacity_bytes)
    }

    pub fn free_percent(&self) -> u8 {
        percent_of(self.free_bytes, self.capacity_bytes)
    }
}

/// Вхід прогнозу: диск + утримання Quarantine + позначене до REAP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForecastInputs {
    pub usage: VolumeUsage,
    /// Байти у Quarantine (status quarantined/in_flight) — ще на диску.
    pub quarantine_held_bytes: u64,
    /// Унікальний обсяг з `Decision::Marked` (Reap Bar).
    pub marked_bytes: u64,
}

/// Результат прогнозу «після очищення» (після purge циклу).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupForecast {
    pub volume: String,
    pub capacity_bytes: u64,
    pub free_now_bytes: u64,
    pub used_now_bytes: u64,
    pub quarantine_held_bytes: u64,
    pub marked_bytes: u64,
    /// `free_now + quarantine_held + marked` (cap capacity).
    pub free_after_cleanup_bytes: u64,
    pub used_after_cleanup_bytes: u64,
    /// % зайнятого зараз / після (UI «91% → 86%»).
    pub used_percent_now: u8,
    pub used_percent_after: u8,
}

impl CleanupForecast {
    pub fn free_after(&self) -> ByteSize {
        ByteSize(self.free_after_cleanup_bytes)
    }

    /// Скільки **ще** звільниться після повного purge (held + marked).
    pub fn pending_free_bytes(&self) -> u64 {
        self.quarantine_held_bytes.saturating_add(self.marked_bytes)
    }
}

/// Обчислити прогноз (чиста функція).
pub fn compute_cleanup_forecast(inputs: &ForecastInputs) -> CleanupForecast {
    let capacity = inputs.usage.capacity_bytes;
    let free_now = inputs.usage.free_bytes.min(capacity);
    let held = inputs.quarantine_held_bytes;
    let marked = inputs.marked_bytes;

    let free_after = free_now
        .saturating_add(held)
        .saturating_add(marked)
        .min(capacity);
    let used_now = capacity.saturating_sub(free_now);
    let used_after = capacity.saturating_sub(free_after);

    CleanupForecast {
        volume: inputs.usage.volume.clone(),
        capacity_bytes: capacity,
        free_now_bytes: free_now,
        used_now_bytes: used_now,
        quarantine_held_bytes: held,
        marked_bytes: marked,
        free_after_cleanup_bytes: free_after,
        used_after_cleanup_bytes: used_after,
        used_percent_now: percent_of(used_now, capacity),
        used_percent_after: percent_of(used_after, capacity),
    }
}

/// Відсоток 0–100 (округлення half-up); capacity=0 → 0.
pub fn percent_of(part: u64, whole: u64) -> u8 {
    if whole == 0 {
        return 0;
    }
    let p = (part as u128 * 100 + whole as u128 / 2) / whole as u128;
    p.min(100) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_after_sums_free_quarantine_and_marked() {
        let usage = VolumeUsage::new("C:", 100, 10);
        let f = compute_cleanup_forecast(&ForecastInputs {
            usage,
            quarantine_held_bytes: 5,
            marked_bytes: 20,
        });
        assert_eq!(f.free_now_bytes, 10);
        assert_eq!(f.free_after_cleanup_bytes, 35);
        assert_eq!(f.used_after_cleanup_bytes, 65);
        assert_eq!(f.pending_free_bytes(), 25);
    }

    #[test]
    fn caps_at_capacity() {
        let f = compute_cleanup_forecast(&ForecastInputs {
            usage: VolumeUsage::new("D:", 100, 90),
            quarantine_held_bytes: 50,
            marked_bytes: 50,
        });
        assert_eq!(f.free_after_cleanup_bytes, 100);
        assert_eq!(f.used_after_cleanup_bytes, 0);
    }

    #[test]
    fn dod_matches_fact_after_purge_cycle() {
        // DoD T-056: після повного purge free_now == free_after (marked=0, held=0).
        let capacity = 1_000u64;
        let mut free = 100u64;
        let mut held = 50u64;
        let mut marked = 200u64;

        let step = |free: u64, held: u64, marked: u64| {
            compute_cleanup_forecast(&ForecastInputs {
                usage: VolumeUsage::new("C:", capacity, free),
                quarantine_held_bytes: held,
                marked_bytes: marked,
            })
        };

        let f0 = step(free, held, marked);
        assert_eq!(f0.free_after_cleanup_bytes, 350);

        // reap marked → held += marked, free unchanged (same-volume move)
        held += marked;
        marked = 0;
        let f1 = step(free, held, marked);
        assert_eq!(f1.free_now_bytes, 100);
        assert_eq!(f1.free_after_cleanup_bytes, 350);
        assert_eq!(f1.quarantine_held_bytes, 250);

        // purge all held → free grows, held → 0; fact == forecast
        free += held;
        held = 0;
        let f2 = step(free, held, marked);
        assert_eq!(f2.free_now_bytes, 350);
        assert_eq!(f2.free_after_cleanup_bytes, 350);
        assert_eq!(f2.pending_free_bytes(), 0);
    }

    #[test]
    fn forecast_updates_when_marked_changes() {
        let usage = VolumeUsage::new("C:", 1000, 100);
        let a = compute_cleanup_forecast(&ForecastInputs {
            usage: usage.clone(),
            quarantine_held_bytes: 0,
            marked_bytes: 10,
        });
        let b = compute_cleanup_forecast(&ForecastInputs {
            usage,
            quarantine_held_bytes: 0,
            marked_bytes: 50,
        });
        assert!(b.free_after_cleanup_bytes > a.free_after_cleanup_bytes);
        assert_eq!(a.free_after_cleanup_bytes, 110);
        assert_eq!(b.free_after_cleanup_bytes, 150);
    }

    #[test]
    fn used_percent_for_ui_wireframe() {
        // 91% used → after free more → lower %
        let f = compute_cleanup_forecast(&ForecastInputs {
            usage: VolumeUsage::new("C:", 100, 9), // used 91
            quarantine_held_bytes: 0,
            marked_bytes: 5, // free → 14, used 86%
        });
        assert_eq!(f.used_percent_now, 91);
        assert_eq!(f.used_percent_after, 86);
    }
}
