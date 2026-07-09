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
pub trait ChangeSource {
    /// Поточний стан USN Journal тому (id + межі USN).
    fn query_journal(&self, volume: char) -> Result<UsnJournalInfo, CoreError>;

    /// Прочитати дельту з `from` (невключно з уже обробленими).
    ///
    /// - [`UsnReadOutcome::Changes`] — лише записи після курсора;
    /// - [`UsnReadOutcome::JournalStale`] — журнал перезаписано / курсор
    ///   застарів → потрібен повний рескан (T-031).
    fn read_delta(&self, volume: char, from: UsnCursor) -> Result<UsnReadOutcome, CoreError>;
}

/// Результат читання USN-дельти (T-029).
#[derive(Debug, Clone)]
pub enum UsnReadOutcome {
    /// Дельта з новим курсором для збереження в індексі.
    Changes {
        changes: Vec<UsnChange>,
        next_cursor: UsnCursor,
    },
    /// Позиція невалідна: journal id змінився або USN випав з журналу.
    JournalStale {
        info: UsnJournalInfo,
        reason: &'static str,
    },
}

use trashradar_domain::{
    candidate::{ByteSize, FileRecord, FileRecordSort, FsTimestamp},
    category::CategoryId,
    error::CoreError,
    scan::{ScanEntry, UsnChange, UsnCursor, UsnJournalInfo},
    ContentHash, FileHashCacheEntry, PartialHash,
};

use crate::workers::CancellationToken;

/// Persistent-індекс кандидатів і журнал Quarantine.
/// Реалізація: `index-sqlite` (T-011…T-014, T-078).
/// Курсор USN (T-029) — частина persistent-індексу (architecture.md §10).
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

    /// Збережена позиція USN Journal для тому (літера `C`/`D`/…).
    fn get_usn_cursor(&self, volume: char) -> Result<Option<UsnCursor>, CoreError>;

    /// Зафіксувати позицію USN після повного скану або обробки дельти (T-029).
    fn set_usn_cursor(&self, volume: char, cursor: UsnCursor) -> Result<(), CoreError>;

    /// Скинути збережений USN-курсор тому (T-031: перед повним ресканом
    /// після JournalStale, щоб не повторювати інкремент на застарілій позиції).
    fn clear_usn_cursor(&self, volume: char) -> Result<(), CoreError>;

    /// Видалити всі persistent-записи з даним шляхом (T-030 delete/rename).
    /// Повертає кількість видалених рядків.
    fn delete_file_records_by_path(&self, path: &str) -> Result<u64, CoreError>;
}

/// Гарячий in-memory індекс. Реалізація: `index-memory` (T-015…T-018, T-030).
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

    /// Видалити записи за повними шляхами (T-030 USN delete/rename).
    /// Порівняння шляхів — регістронезалежне (Windows). Повертає кількість
    /// видалених записів.
    fn remove_paths(&self, paths: &[String]) -> Result<usize, CoreError>;

    /// Вставити або замінити записи з тим самим шляхом (T-030 create/modify).
    fn upsert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError>;
}

/// Сховище і генерація превью. Реалізація: `preview` (E6).
pub trait PreviewStore {}

/// Види прев'ю (T-068).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewKind {
    /// Статична мініатюра (зображення або ключовий кадр відео).
    Thumbnail,
    /// Скраб-смуга кадрів відео.
    Scrub,
}

/// Запис кешу прев'ю (T-068).
#[derive(Debug, Clone)]
pub struct PreviewCacheEntry {
    pub source_path: String,
    pub preview_kind: PreviewKind,
    pub size: ByteSize,
    pub modified_at: Option<FsTimestamp>,
    pub preview_path: String,
}

impl PreviewCacheEntry {
    /// Чи є запис кешу валідним для поточного файлу за розміром та датою зміни.
    pub fn is_valid_for(
        &self,
        current_size: ByteSize,
        current_modified: Option<FsTimestamp>,
    ) -> bool {
        self.size == current_size && self.modified_at == current_modified
    }
}

/// Порт для збереження посилань на прев'ю в індексі (T-068).
pub trait PreviewCache: Send + Sync {
    /// Отримати запис кешу прев'ю.
    fn get_preview_entry(
        &self,
        source_path: &str,
        kind: PreviewKind,
    ) -> Result<Option<PreviewCacheEntry>, CoreError>;

    /// Зберегти або оновити запис кешу прев'ю.
    fn put_preview_entry(&self, entry: &PreviewCacheEntry) -> Result<(), CoreError>;

    /// Видалити конкретне прев'ю для шляху.
    fn delete_preview_entry(&self, source_path: &str, kind: PreviewKind) -> Result<(), CoreError>;

    /// Видалити всі прев'ю для шляху.
    fn delete_all_previews_for_path(&self, source_path: &str) -> Result<(), CoreError>;
}

/// Декодована мініатюра з джерела превью (T-069+).
///
/// Пікселі — `BGRA`, 4 байти на піксель, рядки зверху вниз (top-down DIB,
/// як віддає GDI). Це сира растрова форма; кодування у файл кешу (T-068) —
/// відповідальність викликача. Джерело віддає мініатюру вже декодованою.
#[derive(Clone)]
pub struct RawThumbnail {
    /// Ширина мініатюри в пікселях.
    pub width: u32,
    /// Висота мініатюри в пікселях.
    pub height: u32,
    /// Пікселі BGRA, `width * height * 4` байти, рядки зверху вниз.
    pub bgra: Vec<u8>,
}

impl std::fmt::Debug for RawThumbnail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Не друкуємо мільйони байтів пікселів — лише геометрію.
        f.debug_struct("RawThumbnail")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bgra.len())
            .finish()
    }
}

/// Джерело мініатюр — ланка ланцюжка превью (architecture.md §5.2).
///
/// Реалізації (кожна — окреме джерело за спаданням швидкості):
/// системний кеш мініатюр Windows (T-069, `preview`), власне декодування
/// зображень (T-070), витяг ключового кадру відео (T-071).
///
/// Контракт: `Ok(None)` означає «це джерело не має мініатюри для файла» —
/// викликач переходить до наступного джерела в ланцюжку. `Err` — реальний
/// збій (некоректний аргумент, недоступний ресурс).
pub trait ThumbnailSource: Send + Sync {
    /// Спробувати отримати мініатюру файла, вписану в квадрат `max_edge` пікселів
    /// (зі збереженням пропорцій). `Ok(None)` — джерело не покриває цей файл.
    fn thumbnail(&self, path: &str, max_edge: u32) -> Result<Option<RawThumbnail>, CoreError>;
}

/// Метадані відеофайла, визначені декодером (T-071).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoMetadata {
    /// Тривалість відео в мілісекундах.
    pub duration_ms: u64,
    /// Ширина кадру джерела в пікселях.
    pub width: u32,
    /// Висота кадру джерела в пікселях.
    pub height: u32,
}

/// Рекомендована кількість кадрів скраб-смуги (architecture.md §5.2: 10–20).
pub const DEFAULT_SCRUB_FRAME_COUNT: u32 = 16;

/// Скраб-смуга кадрів відео (T-072): `frame_count` кадрів, рівномірно
/// розподілених по таймлайну (кадр `i` ≈ момент `duration * i / frame_count`),
/// одним суцільним буфером BGRA — кадри складені послідовно, зріз кадру `i`:
/// `bgra[i * frame_bytes() .. (i + 1) * frame_bytes()]`.
///
/// Скраб у UI (T-120/T-124) лише гортає готові зрізи — відео на льоту
/// не декодується.
#[derive(Clone)]
pub struct ScrubStrip {
    /// Ширина одного кадру в пікселях.
    pub frame_width: u32,
    /// Висота одного кадру в пікселях.
    pub frame_height: u32,
    /// Фактична кількість кадрів у смузі (може бути меншою за запитану
    /// для дуже коротких відео).
    pub frame_count: u32,
    /// Пікселі BGRA всіх кадрів послідовно, `frame_count * frame_bytes()` байтів.
    pub bgra: Vec<u8>,
}

impl ScrubStrip {
    /// Розмір одного кадру в байтах.
    pub fn frame_bytes(&self) -> usize {
        (self.frame_width as usize) * (self.frame_height as usize) * 4
    }

    /// Зріз пікселів кадру `index` (`None` поза межами смуги).
    pub fn frame_slice(&self, index: u32) -> Option<&[u8]> {
        if index >= self.frame_count {
            return None;
        }
        let size = self.frame_bytes();
        let start = (index as usize) * size;
        self.bgra.get(start..start + size)
    }
}

impl std::fmt::Debug for ScrubStrip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Не друкуємо мегабайти пікселів — лише геометрію смуги.
        f.debug_struct("ScrubStrip")
            .field("frame_width", &self.frame_width)
            .field("frame_height", &self.frame_height)
            .field("frame_count", &self.frame_count)
            .field("bytes", &self.bgra.len())
            .finish()
    }
}

/// Джерело кадрів відео — ланка ланцюжка превью для відеофайлів
/// (architecture.md §5.2: «відео — витяг ключових кадрів»).
///
/// Реалізація: `preview` (T-071 кадр/тривалість, T-072 скраб-смуга;
/// ffmpeg-функціональність). Порт платформонезалежний.
///
/// Контракт `Ok(None)` — як у [`ThumbnailSource`]: файл не розпізнано
/// як відео або декодер недоступний → викликач рендерить типізовану
/// плитку без превью. `Err` — некоректні аргументи чи реальний збій.
pub trait VideoFrameSource: Send + Sync {
    /// Метадані відео: тривалість (DoD T-071) і розмір кадру джерела.
    fn probe(&self, path: &str) -> Result<Option<VideoMetadata>, CoreError>;

    /// Ключовий кадр для мініатюри плитки, вписаний у квадрат `max_edge`
    /// зі збереженням пропорцій (лише даунскейл).
    fn key_frame(&self, path: &str, max_edge: u32) -> Result<Option<RawThumbnail>, CoreError>;

    /// Скраб-смуга з `frames` кадрів по таймлайну, кожен вписаний у квадрат
    /// `max_edge` (DoD T-072: генерується одним проходом декодування).
    fn scrub_strip(
        &self,
        path: &str,
        max_edge: u32,
        frames: u32,
    ) -> Result<Option<ScrubStrip>, CoreError>;
}

/// Єдиний шлюз деструктивних операцій з FS (move/restore/purge).
/// Реалізація: `quarantine-fs` (T-077, T-079…T-084). Інваріант D4:
/// жоден інший порт не має права змінювати файлову систему.
pub trait QuarantineFs {}

/// Хешування для каскаду дублікатів. Реалізація: `hash` (T-059/T-060).
///
/// T-059: partial fingerprint (перші+останні 64 КіБ, ≤ 128 КіБ читання).
/// T-060: повний потоковий BLAKE3 (константна пам'ять на файл).
pub trait Hasher: Send + Sync {
    /// Частковий відбиток файла. `size` — з індексу (уникаємо зайвого stat).
    ///
    /// Інваріант DoD T-059: адаптер читає з диска **не більше**
    /// [`PARTIAL_HASH_MAX_READ_BYTES`](trashradar_domain::PARTIAL_HASH_MAX_READ_BYTES).
    fn partial_hash(&self, path: &str, size: ByteSize) -> Result<PartialHash, CoreError>;

    /// Повний BLAKE3 файла потоково (T-060).
    ///
    /// Інваріанти: константна пам'ять (фіксований буфер читання);
    /// перевірка `cancel` між блоками — кооперативна відміна великих файлів.
    fn full_hash(
        &self,
        path: &str,
        size: ByteSize,
        cancel: &CancellationToken,
    ) -> Result<ContentHash, CoreError>;
}

/// Кеш partial/full хешів у індексі (T-062, architecture.md §10).
///
/// Валідність: size + mtime. Повторний запуск не перехешовує незмінені файли.
pub trait HashCache: Send + Sync {
    /// Сирий lookup за шляхом (нормалізація — всередині адаптера).
    fn get_entry(&self, path: &str) -> Result<Option<FileHashCacheEntry>, CoreError>;

    /// Upsert запису (merge partial/content на стороні викликача або адаптера).
    fn put_entry(&self, entry: &FileHashCacheEntry) -> Result<(), CoreError>;
}

/// Lookup: запис валідний лише якщо size+mtime збігаються.
pub fn hash_cache_lookup(
    cache: &dyn HashCache,
    path: &str,
    size: ByteSize,
    modified_at: Option<FsTimestamp>,
) -> Result<Option<FileHashCacheEntry>, CoreError> {
    match cache.get_entry(path)? {
        Some(entry) if entry.is_valid_for(size, modified_at) => Ok(Some(entry)),
        _ => Ok(None),
    }
}

/// Джерело часу — для тестованості TTL і «віку» файлів.
pub trait Clock {}
