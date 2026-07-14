//! Реєстр детекторів (T-036 / T-038).
//!
//! Склад ферми — дані: додавання детектора не вимагає змін у коді прогону
//! (evaluate_record / evaluate_stream). Оркестратор T-037/T-038 лише ітерує реєстр.

use super::contract::{Detector, DetectorHit, DetectorId};
use super::thresholds::ThresholdValue;
use trashradar_domain::candidate::FileRecord;
use trashradar_domain::error::CoreError;

/// Реєстр активних детекторів сесії / застосунку.
#[derive(Default)]
pub struct DetectorRegistry {
    detectors: Vec<Box<dyn Detector>>,
}

impl DetectorRegistry {
    pub fn new() -> Self {
        Self {
            detectors: Vec::new(),
        }
    }

    /// Зареєструвати детектор. Панікує при дублікаті id (помилка програміста).
    pub fn register(&mut self, detector: impl Detector + 'static) {
        if let Err(msg) = self.try_register(detector) {
            panic!("{msg}");
        }
    }

    /// Як [`register`], але з `Result` для тестів і динамічної конфігурації.
    pub fn try_register(&mut self, detector: impl Detector + 'static) -> Result<(), String> {
        let id = detector.id();
        if self.detectors.iter().any(|d| d.id() == id) {
            return Err(format!(
                "детектор з id «{id}» уже зареєстрований (дублікат заборонено)"
            ));
        }
        self.detectors.push(Box::new(detector));
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.detectors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.detectors.is_empty()
    }

    /// Усі зареєстровані (включно з вимкненими).
    pub fn iter(&self) -> impl Iterator<Item = &dyn Detector> {
        self.detectors.iter().map(|d| d.as_ref())
    }

    /// Лише увімкнені — саме вони отримують потік (T-037 DoD).
    pub fn enabled(&self) -> impl Iterator<Item = &dyn Detector> {
        self.iter().filter(|d| d.is_enabled())
    }

    pub fn get(&self, id: DetectorId) -> Option<&dyn Detector> {
        self.detectors
            .iter()
            .find(|d| d.id() == id)
            .map(|d| d.as_ref())
    }

    /// Змінити поріг детектора за id (T-038). Без рескану диска — лише
    /// стан детектора; перерахунок індексу — [`crate::detectors::DetectorOrchestrator::recalculate_index`].
    pub fn set_threshold(
        &self,
        id: DetectorId,
        key: &str,
        value: ThresholdValue,
    ) -> Result<(), CoreError> {
        let det = self.get(id).ok_or_else(|| {
            CoreError::invalid_argument(format!("Детектор «{id}» не зареєстрований."))
        })?;
        det.set_threshold(key, value)
    }

    /// Поточне значення порога або `None`.
    pub fn get_threshold(&self, id: DetectorId, key: &str) -> Option<ThresholdValue> {
        self.get(id).and_then(|d| d.get_threshold(key))
    }

    /// Увімкнути/вимкнути детектор за id (T-152). Перерахунок індексу —
    /// окремим кроком, як і в [`DetectorRegistry::set_threshold`].
    pub fn set_enabled(&self, id: DetectorId, enabled: bool) -> Result<(), CoreError> {
        let det = self.get(id).ok_or_else(|| {
            CoreError::invalid_argument(format!("Детектор «{id}» не зареєстрований."))
        })?;
        det.set_enabled(enabled)
    }

    /// Прогнати **один** запис через усі увімкнені детектори.
    ///
    /// Файл може отримати кілька вердиктів (перетин категорій, T-054).
    /// Вимкнені детектори не викликаються.
    pub fn evaluate_record(&self, record: &FileRecord) -> Vec<DetectorHit> {
        let mut hits = Vec::new();
        for det in self.enabled() {
            if let Some(verdict) = det.evaluate(record) {
                debug_assert!(
                    verdict.is_complete(),
                    "детектор {} повернув порожнє explanation",
                    det.id()
                );
                debug_assert_eq!(
                    verdict.category,
                    det.category(),
                    "детектор {}: category вердикту ≠ Detector::category",
                    det.id()
                );
                hits.push(DetectorHit {
                    candidate_id: record.candidate_id,
                    detector_id: det.id(),
                    verdict,
                });
            }
        }
        hits
    }

    /// Прогнати **потік** записів індексу (підготовка T-037).
    ///
    /// Синхронний ітератор: оркестратор може годувати батчами з HotIndex
    /// під час скану або при перерахунку порогів (T-038).
    pub fn evaluate_stream<'a, I>(&self, records: I) -> Vec<DetectorHit>
    where
        I: IntoIterator<Item = &'a FileRecord>,
    {
        let mut hits = Vec::new();
        for record in records {
            hits.extend(self.evaluate_record(record));
        }
        hits
    }
}
