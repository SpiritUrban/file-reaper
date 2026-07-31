//! Папка як **одна одиниця** позначення (T-053).
//!
//! architecture.md §6.1 / ui.md: плитки груп (`node_modules`, кеші, Unity Library…)
//! показують **сумарний розмір + кількість файлів** і позначаються цілком.
//!
//! DoD T-053: плитка «node_modules · 1.2 ГБ · 44 000 файлів» — **одна** одиниця
//! ([`CandidateUnit::Folder`]), не N окремих файлів.
//!
//! Агрегація — чиста функція над записами індексу (без I/O). Детектори
//! (T-048…T-052) збирають ключі коренів; цей модуль будує `FileRecord`-одиниці.

use super::contract::DetectorId;
use super::format::folder_unit_explanation;
use trashradar_domain::candidate::{
    ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
    SafetyLevel,
};
use trashradar_domain::category::CategoryId;

/// Накопичувач розміру/кількості під одним коренем папки-одиниці.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FolderUnitAcc {
    pub bytes: u64,
    pub file_count: u64,
}

impl FolderUnitAcc {
    pub fn add_file(&mut self, size_bytes: u64) {
        self.bytes = self.bytes.saturating_add(size_bytes);
        self.file_count = self.file_count.saturating_add(1);
    }

    pub fn is_empty(self) -> bool {
        self.file_count == 0
    }
}

/// Специфікація папки-одиниці перед збиранням [`FileRecord`].
#[derive(Debug, Clone)]
pub struct FolderUnitSpec {
    /// Повний шлях кореня (node_modules, Cache, Library…).
    pub path: String,
    /// Підпис у explanation: `node_modules`, `Chrome Cache`, `Unity Library`…
    pub label: String,
    pub bytes: u64,
    pub file_count: u64,
    pub category: CategoryId,
    pub safety: SafetyLevel,
    pub detector_id: DetectorId,
    /// Додаткові фрази після базового «label · size · N файлів»
    /// (активність T-052, «перестворюється…»).
    pub notes: Vec<String>,
    /// Namespace для стабільного [`CandidateId`] (різні детектори не колізять).
    pub id_namespace: u64,
}

/// Зібрати [`FileRecord`] з `unit = Folder` — **одна** одиниця позначення.
///
/// - `size` = сума байтів файлів;
/// - `explanation` = DoD-формат + optional notes;
/// - `decision` = [`Decision::Undecided`] (позначення — UI/T-057).
pub fn build_folder_unit(spec: FolderUnitSpec) -> FileRecord {
    let mut explanation = folder_unit_explanation(&spec.label, spec.bytes, spec.file_count);
    for note in &spec.notes {
        let note = note.trim();
        if !note.is_empty() {
            explanation.push_str(" · ");
            explanation.push_str(note);
        }
    }
    FileRecord {
        candidate_id: CandidateId(stable_folder_id(&spec.path, spec.id_namespace)),
        path: spec.path,
        size: ByteSize(spec.bytes),
        created_at: None,
        modified_at: None,
        accessed_at: None,
        kind: FileKind::Other,
        unit: CandidateUnit::Folder,
        category: spec.category,
        safety: spec.safety,
        decision: Decision::Undecided,
        detector_id: spec.detector_id.as_str().to_string(),
        explanation,
        attributes: FileAttributes::default(),
    }
}

/// Зручний конструктор з [`FolderUnitAcc`] + базових полів у [`FolderUnitSpec`].
///
/// `spec.bytes` / `file_count` ігноруються і беруться з `acc`.
pub fn build_folder_unit_from_acc(
    mut spec: FolderUnitSpec,
    acc: FolderUnitAcc,
) -> Option<FileRecord> {
    if acc.is_empty() {
        return None;
    }
    spec.bytes = acc.bytes;
    spec.file_count = acc.file_count;
    Some(build_folder_unit(spec))
}

/// Стабільний id папки: namespace у бітах 48–50 + 48-бітний hash шляху.
///
/// **Критично — JS-безпечний діапазон:** UI (JavaScript) тримає candidate_id як
/// `number` (f64), що точно представляє цілі лише до `2^53 - 1`
/// (`Number.MAX_SAFE_INTEGER`). Старий namespace у верхніх 4 бітах давав id
/// ~1.5·10¹⁹ ≫ 2^53 → id спотворювався при round-trip category.window →
/// selectionStore → candidate.batch/reap.execute, і жоден позначений
/// папка-кандидат не знаходився в індексі (порожній reap-оверлей, reap теки
/// не спрацьовував). Тепер namespace = `k << 48` (k=1..7), hash = 48 біт →
/// максимум id = 8·2^48 − 1 = 2^51 − 1 ≈ 2.25·10¹⁵ < 2^53 (з великим запасом),
/// і не колізіонує з файловими id (0..N, N ≪ 2^48).
pub fn stable_folder_id(path: &str, namespace: u64) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    path.to_ascii_lowercase().hash(&mut h);
    let hash = h.finish() & ((1u64 << 48) - 1);
    (namespace & (0x7 << 48)) | hash
}

/// Сортування плиток: найбільші зверху, потім шлях.
pub fn sort_folder_units_by_size_desc(units: &mut [FileRecord]) {
    units.sort_by(|a, b| b.size.0.cmp(&a.size.0).then_with(|| a.path.cmp(&b.path)));
}

/// Чи запис — папка-одиниця (сітка / Reap Bar).
pub fn is_folder_unit(record: &FileRecord) -> bool {
    record.unit == CandidateUnit::Folder
}

/// Позначити папку-одиницю як ціле (одна дія → один `Decision`).
///
/// Не чіпає дочірні файли індексу — поширення keep/reap на дітей: T-057.
pub fn apply_unit_decision(unit: &mut FileRecord, decision: Decision) {
    debug_assert!(
        is_folder_unit(unit),
        "apply_unit_decision лише для CandidateUnit::Folder"
    );
    unit.decision = decision;
}

/// Namespace id для детекторів (щоб candidate_id не перетинались).
pub mod id_ns {
    // namespace = `k << 48` (k=1..7): id лишається < 2^53 (JS-safe), не
    // колізіонує з файловими id (0..N ≪ 2^48). Див. [`super::stable_folder_id`].
    pub const APP_CACHES: u64 = 1 << 48;
    pub const BUILD_ARTIFACTS: u64 = 2 << 48;
    pub const UNITY: u64 = 3 << 48;
    pub const NODE_MODULES: u64 = 4 << 48;
    /// Порожні папки (рекурсивно порожні).
    pub const EMPTY_FOLDERS: u64 = 5 << 48;
    /// Майже порожні папки (мало файлів).
    pub const SPARSE_FOLDERS: u64 = 6 << 48;
    /// Задовга вкладеність («Глибокі шляхи»).
    pub const DEEP_PATHS: u64 = 7 << 48;
}

/// Перевірка форми explanation під DoD (label · ГБ · N файлів).
pub fn explanation_looks_like_folder_tile(explanation: &str, label: &str) -> bool {
    explanation.contains(label)
        && explanation.contains("ГБ")
        && explanation.contains("файлів")
        && explanation.matches('·').count() >= 2
}

#[cfg(test)]
mod tests {
    use super::super::format::{format_bytes_as_gb, format_file_count};
    use super::*;
    use trashradar_domain::candidate::Decision;

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn dod_tile_node_modules_one_unit() {
        // DoD T-053: «node_modules · 1.2 ГБ · 44 000 файлів» — одна одиниця.
        let file_count = 44_000u64;
        // ~1.2 GiB total
        let bytes = (1.2f64 * GIB as f64) as u64;
        let unit = build_folder_unit(FolderUnitSpec {
            path: r"C:\dev\old-app\node_modules".into(),
            label: "node_modules".into(),
            bytes,
            file_count,
            category: CategoryId::DevArtifacts,
            safety: SafetyLevel::SafeToBulk,
            detector_id: DetectorId::new("dev.node_modules"),
            notes: vec![],
            id_namespace: id_ns::NODE_MODULES,
        });

        assert_eq!(unit.unit, CandidateUnit::Folder);
        assert_eq!(unit.size.0, bytes);
        assert_eq!(unit.decision, Decision::Undecided);
        assert_eq!(unit.category, CategoryId::DevArtifacts);
        assert!(
            explanation_looks_like_folder_tile(&unit.explanation, "node_modules"),
            "{}",
            unit.explanation
        );
        // Тисячі з пробілом — як у DoD «44 000»
        assert!(
            unit.explanation.contains("44 000"),
            "очікували тисячі з пробілом: {}",
            unit.explanation
        );
        assert!(unit.explanation.contains("ГБ"), "{}", unit.explanation);
        // Розмір ≈ 1.2 ГБ (формат може бути 1.20)
        let size_part = format_bytes_as_gb(bytes);
        assert!(
            unit.explanation.contains(&size_part),
            "{} vs {}",
            unit.explanation,
            size_part
        );
    }

    #[test]
    fn many_files_collapse_to_single_markable_unit() {
        let mut acc = FolderUnitAcc::default();
        for i in 0..440u64 {
            acc.add_file(1_000 + i);
        }
        let unit = build_folder_unit_from_acc(
            FolderUnitSpec {
                path: r"C:\p\node_modules".into(),
                label: "node_modules".into(),
                bytes: 0,
                file_count: 0,
                category: CategoryId::DevArtifacts,
                safety: SafetyLevel::SafeToBulk,
                detector_id: DetectorId::new("dev.node_modules"),
                notes: vec![],
                id_namespace: id_ns::NODE_MODULES,
            },
            acc,
        )
        .expect("non-empty");

        assert_eq!(unit.unit, CandidateUnit::Folder);
        assert_eq!(unit.size.0, acc.bytes);
        assert!(unit.explanation.contains("440"), "{}", unit.explanation);

        // Одна дія позначення — один Decision на одиницю.
        let mut marked = unit.clone();
        apply_unit_decision(&mut marked, Decision::Marked);
        assert_eq!(marked.decision, Decision::Marked);
        assert_eq!(marked.candidate_id, unit.candidate_id);
        assert_eq!(marked.size, unit.size);
    }

    #[test]
    fn empty_acc_yields_no_unit() {
        assert!(build_folder_unit_from_acc(
            FolderUnitSpec {
                path: r"C:\empty".into(),
                label: "x".into(),
                bytes: 0,
                file_count: 0,
                category: CategoryId::AppCaches,
                safety: SafetyLevel::SafeToBulk,
                detector_id: DetectorId::new("app_caches"),
                notes: vec![],
                id_namespace: id_ns::APP_CACHES,
            },
            FolderUnitAcc::default(),
        )
        .is_none());
    }

    #[test]
    fn notes_appended_after_tile_core() {
        let unit = build_folder_unit(FolderUnitSpec {
            path: r"C:\u\Library".into(),
            label: "Unity Library".into(),
            bytes: GIB,
            file_count: 12,
            category: CategoryId::DevArtifacts,
            safety: SafetyLevel::ReviewRecommended,
            detector_id: DetectorId::new("dev.unity"),
            notes: vec![
                "перестворюється при відкритті проєкту".into(),
                "активний (зміна 3 дн. тому)".into(),
            ],
            id_namespace: id_ns::UNITY,
        });
        assert!(unit.explanation.starts_with("Unity Library · "));
        assert!(unit.explanation.contains("12 файлів"));
        assert!(unit.explanation.contains("перестворюється"));
        assert!(unit.explanation.contains("активний"));
    }

    #[test]
    fn sort_largest_first() {
        let mut units = vec![
            build_folder_unit(FolderUnitSpec {
                path: r"C:\a".into(),
                label: "a".into(),
                bytes: 100,
                file_count: 1,
                category: CategoryId::DevArtifacts,
                safety: SafetyLevel::SafeToBulk,
                detector_id: DetectorId::new("t"),
                notes: vec![],
                id_namespace: 0,
            }),
            build_folder_unit(FolderUnitSpec {
                path: r"C:\b".into(),
                label: "b".into(),
                bytes: 500,
                file_count: 1,
                category: CategoryId::DevArtifacts,
                safety: SafetyLevel::SafeToBulk,
                detector_id: DetectorId::new("t"),
                notes: vec![],
                id_namespace: 0,
            }),
        ];
        sort_folder_units_by_size_desc(&mut units);
        assert_eq!(units[0].path, r"C:\b");
        assert_eq!(units[1].path, r"C:\a");
    }

    /// Регрес: id папки має лишатись у JS-safe діапазоні (< 2^53), інакше UI
    /// (f64) спотворює його й позначені папки не знаходяться в індексі →
    /// порожній reap-оверлей / reap теки не працює.
    #[test]
    fn folder_ids_stay_js_safe_and_keep_namespace() {
        const MAX_SAFE: u64 = (1u64 << 53) - 1;
        let namespaces = [
            id_ns::APP_CACHES,
            id_ns::BUILD_ARTIFACTS,
            id_ns::UNITY,
            id_ns::NODE_MODULES,
            id_ns::EMPTY_FOLDERS,
            id_ns::SPARSE_FOLDERS,
            id_ns::DEEP_PATHS,
        ];
        for ns in namespaces {
            for path in [
                r"C:\a",
                r"C:\very\deep\nested\path\node_modules",
                r"D:\проєкт\кеш\тека",
            ] {
                let id = stable_folder_id(path, ns);
                assert!(id <= MAX_SAFE, "id {id} перевищив 2^53-1 для ns {ns:#x}");
                assert_eq!(id >> 48, ns >> 48, "namespace-біти збережено");
            }
        }
        // Реалістичний файловий id (мільярди) не потрапляє в folder-діапазон.
        // `const`-блок: обидві сторони відомі на етапі компіляції, тож це
        // інваріант констант, а не рантайм-перевірка (інакше clippy справедливо
        // каже «assertion has a constant value»).
        const { assert!(2_000_000_000u64 < id_ns::APP_CACHES) };
    }

    #[test]
    fn format_file_count_thousands() {
        assert_eq!(format_file_count(0), "0");
        assert_eq!(format_file_count(999), "999");
        assert_eq!(format_file_count(1_000), "1 000");
        assert_eq!(format_file_count(44_000), "44 000");
        assert_eq!(format_file_count(1_234_567), "1 234 567");
    }
}
