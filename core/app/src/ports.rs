//! Порти Application Layer.
//!
//! Кожен порт реалізується адаптером у `core/infra/*` (docs/repository.md §5).
//! Сигнатури методів визначаються задачею, вказаною біля кожного порту, —
//! на етапі каркаса трейти навмисно порожні, щоб не фіксувати контракт
//! до проєктування відповідного use case.

/// Джерело повного скану тому. Реалізації: `scan-mft`, `scan-walk`.
/// Контракт визначає T-021 / T-026; автовибір реалізації — T-028
/// ([`crate::scan_strategy::choose_scan_strategy`]).
///
/// Перший зріз контракту (T-021): перелічити всі записи тому як сирі
/// [`ScanEntry`] (метадані без повного шляху). Побудова шляхів — T-022,
/// батчева/стрімінгова видача зі зворотним тиском — T-024.
pub trait ScanSource {
    /// Перелічити всі файли та теки тому за його літерою.
    fn scan_volume(&self, volume: char) -> Result<Vec<ScanEntry>, CoreError>;
}

/// Проби середовища для автовибору стратегії скану (T-028).
/// Реалізація: `platform-win` (FS type, elevation).
pub trait ScanEnvironment {
    /// Чи том має файлову систему NTFS.
    fn is_ntfs(&self, volume: char) -> Result<bool, CoreError>;

    /// Чи процес запущено з правами адміністратора.
    fn is_elevated(&self) -> bool;

    /// Ім'я FS («NTFS», «FAT32», …) для health; `None` якщо невідомо.
    fn file_system_name(&self, volume: char) -> Result<Option<String>, CoreError>;

    /// Літери фіксованих/знімних томів, доступних для скану.
    fn list_scan_volumes(&self) -> Vec<char>;
}

/// Джерело інкрементальних змін тому. Реалізація: `scan-usn` (T-029/T-030).
pub trait ChangeSource {}

use trashradar_domain::{
    candidate::{FileRecord, FileRecordSort},
    category::CategoryId,
    error::CoreError,
    scan::ScanEntry,
};

/// Persistent-індекс кандидатів і журнал Quarantine.
/// Реалізація: `index-sqlite` (T-011…T-014, T-078).
pub trait IndexStore {
    /// Отримати вікно записів кандидатів для конкретної категорії з сортуванням.
    fn read_file_records_window(
        &self,
        category: CategoryId,
        sort: FileRecordSort,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<FileRecord>, CoreError>;

    /// Отримати всі збережені записи кандидатів.
    fn read_all_file_records(&self) -> Result<Vec<FileRecord>, CoreError>;
}

/// Гарячий in-memory індекс. Реалізація: `index-memory` (T-015…T-018).
pub trait HotIndex {
    /// Додати пакет записів файлів до індексу.
    fn insert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError>;

    /// Завершити наповнення індексу, оптимізувавши пам'ять.
    fn finish_indexing(&self) -> Result<(), CoreError>;

    /// Отримати кількість записів в індексі.
    fn len(&self) -> Result<usize, CoreError>;

    /// Чи порожній індекс.
    fn is_empty(&self) -> Result<bool, CoreError>;

    /// Очистити індекс.
    fn clear(&self) -> Result<(), CoreError>;

    /// Отримати всі записи з індексу.
    fn get_all(&self) -> Result<Vec<FileRecord>, CoreError>;

    /// Підрядковий пошук за іменем файла та шляхом серед кандидатів (T-018).
    /// Регістронезалежний; записи з рішенням Keep не повертаються
    /// (вони приховані з кандидатів). Результат обмежений `limit` записами.
    fn search_file_records(&self, query: &str, limit: usize) -> Result<Vec<FileRecord>, CoreError>;
}

/// Сховище і генерація превью. Реалізація: `preview` (E6).
pub trait PreviewStore {}

/// Єдиний шлюз деструктивних операцій з FS (move/restore/purge).
/// Реалізація: `quarantine-fs` (T-077, T-079…T-084). Інваріант D4:
/// жоден інший порт не має права змінювати файлову систему.
pub trait QuarantineFs {}

/// Хешування для каскаду дублікатів. Реалізація: `hash` (T-059/T-060).
pub trait Hasher {}

/// Джерело часу — для тестованості TTL і «віку» файлів.
pub trait Clock {}
