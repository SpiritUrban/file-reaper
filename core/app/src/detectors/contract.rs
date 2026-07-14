//! Контракт детектора (T-036 / T-038).
//!
//! architecture.md §6.1–6.2: детектор отримує записи індексу й повертає
//! вердикт «кандидат / ні» з **категорією**, **поясненням** і **рівнем
//! безпечності**. Жодного I/O — лише чиста логіка над [`FileRecord`].
//!
//! T-038: пороги змінюються через [`Detector::set_threshold`] (interior
//! mutability); оркестратор потім перераховує індекс без рескану.

use super::thresholds::ThresholdValue;
use trashradar_domain::candidate::{CandidateId, FileRecord, Verdict};
use trashradar_domain::category::CategoryId;
use trashradar_domain::error::CoreError;

/// Стабільний ідентифікатор детектора (машиночитаний, snake / dotted).
///
/// Не плутати з [`CategoryId`]: один детектор ↔ одна MVP-категорія, але
/// id детектора версіонується окремо (напр. `large_files`, майбутні
/// `large_files.v2` / користувацькі правила).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DetectorId(pub &'static str);

impl DetectorId {
    pub const fn new(id: &'static str) -> Self {
        Self(id)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for DetectorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Знахідка детектора на одному записі індексу (T-036 → T-037).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectorHit {
    pub candidate_id: CandidateId,
    pub detector_id: DetectorId,
    pub verdict: Verdict,
}

/// Контракт детектора категорії (T-036).
///
/// Реалізації — модулі в `detectors/*` (T-039+). Оркестратор (T-037) бачить
/// лише цей trait: **нова категорія = новий тип + `registry.register`, без
/// правок оркестратора**.
pub trait Detector: Send + Sync {
    /// Стабільний id (унікальний у реєстрі).
    fn id(&self) -> DetectorId;

    /// Категорія, яку наповнює цей детектор.
    fn category(&self) -> CategoryId;

    /// Чи активний (вимкнений — не отримує потік; T-037 / settings T-152).
    fn is_enabled(&self) -> bool {
        true
    }

    /// Увімкнути/вимкнути детектор «на льоту» (T-152). Дефолт — детектор
    /// без перемикача (як [`Detector::set_threshold`] без порогів).
    ///
    /// Реалізації тримають прапорець в `AtomicBool`, щоб працювати через
    /// `&self` (спільний реєстр у воркерах).
    fn set_enabled(&self, enabled: bool) -> Result<(), CoreError> {
        let _ = enabled;
        Err(CoreError::invalid_argument(format!(
            "Детектор «{}» не має перемикача.",
            self.id()
        )))
    }

    /// Оцінити один запис індексу.
    ///
    /// - `Some(verdict)` — кандидат; вердикт **мусить** містити category,
    ///   непорожнє explanation і safety (architecture.md §6.2);
    /// - `None` — цей детектор файл не заявляє.
    ///
    /// `category` у вердикті має збігатися з [`Detector::category`].
    fn evaluate(&self, record: &FileRecord) -> Option<Verdict>;

    /// Змінити поріг «на льоту» (T-038). Дефолт — детектор неконфігурований.
    ///
    /// Реалізації предикатних детекторів тримають пороги в `Atomic*` /
    /// `Mutex`, щоб `set_threshold` працював через `&self` (спільний
    /// `Arc` у реєстрі та воркерах).
    fn set_threshold(&self, key: &str, value: ThresholdValue) -> Result<(), CoreError> {
        let _ = (key, value);
        Err(CoreError::invalid_argument(format!(
            "Детектор «{}» не має конфігурованих порогів.",
            self.id()
        )))
    }

    /// Прочитати поточне значення порога (для UI / health). `None` — ключ
    /// невідомий або детектор без порогів.
    fn get_threshold(&self, key: &str) -> Option<ThresholdValue> {
        let _ = key;
        None
    }
}
