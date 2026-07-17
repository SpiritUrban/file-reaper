//! Адаптер `HotIndex`: гарячий in-memory індекс метаданих.
//!
//! Реалізація T-015: компактний запис метаданих (48 байт) з інтернуванням
//! директорій та назв файлів у суцільний буфер для мінімізації кучі.

use std::collections::HashMap;
use trashradar_domain::candidate::{
    ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
    FsTimestamp, SafetyLevel,
};
use trashradar_domain::category::CategoryId;

const UNIX_EPOCH_FILETIME_SECS: i64 = 11_644_473_600;

/// Перетворює Windows FILETIME (100ns з 1601 року) у секунди Unix Epoch (з 1970 року).
/// Повертає 0 для None (sentinel).
pub fn filetime_to_unix_secs(filetime: i64) -> u32 {
    if filetime == 0 {
        return 0;
    }
    let filetime_secs = filetime / 10_000_000;
    let unix_secs = filetime_secs - UNIX_EPOCH_FILETIME_SECS;
    if unix_secs <= 0 {
        1 // Клемпимо до 1, бо 0 — маркер відсутності значення (None)
    } else if unix_secs >= u32::MAX as i64 {
        u32::MAX - 1
    } else {
        unix_secs as u32
    }
}

/// Перетворює секунди Unix Epoch назад у Windows FILETIME.
pub fn unix_secs_to_filetime(unix_secs: u32) -> i64 {
    if unix_secs == 0 {
        return 0;
    }
    let filetime_secs = unix_secs as i64 + UNIX_EPOCH_FILETIME_SECS;
    filetime_secs * 10_000_000
}

/// Інтернер рядків з послідовним пакуванням у спільний буфер (string arena).
/// Це уникає накладних витрат 24 байт String-заголовка на кожен елемент.
#[derive(Debug, Clone)]
pub struct PathInterner {
    buffer: String,
    offsets: Vec<u32>,
    lookup: HashMap<String, u32>,
}

impl Default for PathInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl PathInterner {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            offsets: vec![0],
            lookup: HashMap::new(),
        }
    }

    /// Додає рядок до пулу та повертає його ID. Якщо рядок вже існує, повертає його ID.
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.lookup.get(s) {
            return id;
        }
        let id = (self.offsets.len() - 1) as u32;
        self.buffer.push_str(s);
        let next_offset = self.buffer.len() as u32;
        self.offsets.push(next_offset);
        self.lookup.insert(s.to_string(), id);
        id
    }

    /// Повертає рядок за його ID.
    pub fn resolve(&self, id: u32) -> Option<&str> {
        let start = *self.offsets.get(id as usize)? as usize;
        let end = *self.offsets.get(id as usize + 1)? as usize;
        Some(&self.buffer[start..end])
    }

    /// Звільняє мапу пошуку для мінімізації використання пам'яті після завершення індексування.
    pub fn shrink_to_fit(&mut self) {
        self.buffer.shrink_to_fit();
        self.offsets.shrink_to_fit();
        self.lookup = HashMap::new();
        self.lookup.shrink_to_fit();
    }

    /// Відновлює мапу пошуку за потреби (наприклад, для інкрементального додавання).
    pub fn rebuild_lookup(&mut self) {
        if self.lookup.is_empty() && self.offsets.len() > 1 {
            self.lookup.reserve(self.offsets.len() - 1);
            for id in 0..(self.offsets.len() - 1) {
                if let Some(s) = self.resolve(id as u32) {
                    self.lookup.insert(s.to_string(), id as u32);
                }
            }
        }
    }

    /// ID вже інтернованого рядка без мутації (точний збіг за регістром).
    /// Валідний після `rebuild_lookup()`.
    pub fn lookup_id(&self, s: &str) -> Option<u32> {
        self.lookup.get(s).copied()
    }

    /// Кількість інтернованих рядків.
    pub fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    /// Чи порожній інтернер.
    pub fn is_empty(&self) -> bool {
        self.offsets.len() == 1
    }

    /// Оцінює обсяг пам'яті, що займає інтернер у купі (heap).
    pub fn memory_usage(&self) -> usize {
        let mut total = 0;
        total += self.buffer.capacity();
        total += self.offsets.capacity() * std::mem::size_of::<u32>();
        total += self.lookup.capacity() * 32;
        total
    }
}

/// Компактна структура запису файлу в пам'яті (рівно 48 байт).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactFileRecord {
    pub candidate_id: u64,
    pub size: u64,
    pub accessed_at: u32, // Unix Epoch secs
    pub created_at: u32,  // Unix Epoch secs
    pub modified_at: u32, // Unix Epoch secs
    pub dir_id: u32,
    pub filename_id: u32,
    pub attributes: u32,
    pub packed_meta: u16,
}

impl CompactFileRecord {
    /// Запаковує доменну модель `FileRecord` у компактний вигляд.
    pub fn pack(record: &FileRecord, dir_id: u32, filename_id: u32) -> Self {
        let accessed_at = record
            .accessed_at
            .map(|t| filetime_to_unix_secs(t.0))
            .unwrap_or(0);
        let created_at = record
            .created_at
            .map(|t| filetime_to_unix_secs(t.0))
            .unwrap_or(0);
        let modified_at = record
            .modified_at
            .map(|t| filetime_to_unix_secs(t.0))
            .unwrap_or(0);

        let kind_val = record.kind as u16;
        let unit_val = record.unit as u16;
        let category_val = record.category as u16;
        let safety_val = record.safety as u16;
        let decision_val = record.decision as u16;

        let packed_meta = (kind_val & 0x7)
            | ((unit_val & 0x1) << 3)
            | ((category_val & 0xF) << 4)
            | ((safety_val & 0x1) << 8)
            | ((decision_val & 0x3) << 9);

        Self {
            candidate_id: record.candidate_id.0,
            size: record.size.0,
            accessed_at,
            created_at,
            modified_at,
            dir_id,
            filename_id,
            attributes: record.attributes.raw_bits,
            packed_meta,
        }
    }

    /// Розпаковує компактний запис назад у доменну модель `FileRecord`.
    pub fn unpack(&self, path: String) -> FileRecord {
        let accessed_at = if self.accessed_at == 0 {
            None
        } else {
            Some(FsTimestamp(unix_secs_to_filetime(self.accessed_at)))
        };
        let created_at = if self.created_at == 0 {
            None
        } else {
            Some(FsTimestamp(unix_secs_to_filetime(self.created_at)))
        };
        let modified_at = if self.modified_at == 0 {
            None
        } else {
            Some(FsTimestamp(unix_secs_to_filetime(self.modified_at)))
        };

        let kind = parse_file_kind_val(self.packed_meta & 0x7);
        let unit = parse_candidate_unit_val((self.packed_meta >> 3) & 0x1);
        let category = parse_category_val((self.packed_meta >> 4) & 0xF);
        let safety = parse_safety_level_val((self.packed_meta >> 8) & 0x1);
        let decision = parse_decision_val((self.packed_meta >> 9) & 0x3);

        FileRecord {
            candidate_id: CandidateId(self.candidate_id),
            path,
            size: ByteSize(self.size),
            created_at,
            modified_at,
            accessed_at,
            kind,
            unit,
            category,
            safety,
            decision,
            detector_id: String::new(),
            explanation: String::new(),
            attributes: FileAttributes {
                raw_bits: self.attributes,
                is_readonly: (self.attributes & 0x1) != 0,
                is_hidden: (self.attributes & 0x2) != 0,
                is_system: (self.attributes & 0x4) != 0,
                is_temporary: (self.attributes & 0x8) != 0,
            },
        }
    }
}

fn parse_file_kind_val(val: u16) -> FileKind {
    match val {
        0 => FileKind::Video,
        1 => FileKind::Image,
        2 => FileKind::Audio,
        3 => FileKind::Archive,
        4 => FileKind::Installer,
        5 => FileKind::DiskImage,
        6 => FileKind::Document,
        _ => FileKind::Other,
    }
}

fn parse_candidate_unit_val(val: u16) -> CandidateUnit {
    match val {
        0 => CandidateUnit::File,
        _ => CandidateUnit::Folder,
    }
}

fn parse_category_val(val: u16) -> CategoryId {
    match val {
        0 => CategoryId::LargeFiles,
        1 => CategoryId::OldFiles,
        2 => CategoryId::ForgottenVideos,
        3 => CategoryId::Duplicates,
        4 => CategoryId::Archives,
        5 => CategoryId::Installers,
        6 => CategoryId::TempFiles,
        7 => CategoryId::AppCaches,
        8 => CategoryId::DevArtifacts,
        9 => CategoryId::EmptyFolders,
        10 => CategoryId::SparseFolders,
        11 => CategoryId::DeepPaths,
        _ => CategoryId::Uncategorized,
    }
}

fn parse_safety_level_val(val: u16) -> SafetyLevel {
    match val {
        0 => SafetyLevel::SafeToBulk,
        _ => SafetyLevel::ReviewRecommended,
    }
}

fn parse_decision_val(val: u16) -> Decision {
    match val {
        0 => Decision::Undecided,
        1 => Decision::Keep,
        _ => Decision::Marked,
    }
}

use std::sync::RwLock;
use trashradar_domain::error::CoreError;

/// Внутрішній стан гарячого in-memory індексу.
#[derive(Debug, Default, Clone)]
struct InMemoryIndexInner {
    records: Vec<CompactFileRecord>,
    dir_interner: PathInterner,
    filename_interner: PathInterner,
}

impl InMemoryIndexInner {
    /// Відновлює повний шлях запису з інтернованих директорії та імені файла.
    fn resolve_path(&self, compact: &CompactFileRecord) -> String {
        let parent = self.dir_interner.resolve(compact.dir_id).unwrap_or("");
        let file_name = self
            .filename_interner
            .resolve(compact.filename_id)
            .unwrap_or("");

        if parent.is_empty() {
            file_name.to_string()
        } else {
            let mut p = std::path::PathBuf::from(parent);
            p.push(file_name);
            p.to_string_lossy().into_owned()
        }
    }
}

/// Регістронезалежний пошук підрядка. Для ASCII-рядків (переважна більшість
/// шляхів) — побайтове порівняння без алокацій; для не-ASCII — Unicode-фолбек
/// через lowercase у багаторазовий буфер.
fn contains_ignore_case(haystack: &str, needle_lower: &str, buf: &mut String) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if haystack.is_ascii() {
        let h = haystack.as_bytes();
        let n = needle_lower.as_bytes();
        if h.len() < n.len() {
            return false;
        }
        let first = n[0];
        'outer: for start in 0..=(h.len() - n.len()) {
            if h[start].to_ascii_lowercase() != first {
                continue;
            }
            for j in 1..n.len() {
                if h[start + j].to_ascii_lowercase() != n[j] {
                    continue 'outer;
                }
            }
            return true;
        }
        false
    } else {
        buf.clear();
        buf.extend(haystack.chars().flat_map(|c| c.to_lowercase()));
        buf.contains(needle_lower)
    }
}

/// Позначає, які рядки інтернера містять `needle` (порівняння без регістру).
/// Пошук іде по унікальних рядках, а не по кожному з мільйонів записів.
fn match_interned_entries(interner: &PathInterner, needle: &str) -> Vec<bool> {
    let count = interner.len();
    let mut matches = vec![false; count];
    let mut buf = String::new();
    for (id, hit) in matches.iter_mut().enumerate() {
        if let Some(entry) = interner.resolve(id as u32) {
            *hit = contains_ignore_case(entry, needle, &mut buf);
        }
    }
    matches
}

/// Останні щонайбільше `max_bytes` байтів рядка, вирівняні на межу символа.
fn tail_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Перші щонайбільше `max_bytes` байтів рядка, вирівняні на межу символа.
fn head_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Перевіряє збіг `needle` через межу «директорія\ім'я файла»: збіги всередині
/// директорії чи імені вже покриті бітовими масками, тож вікно будується лише
/// з хвоста директорії, роздільника та голови імені файла.
fn boundary_window_contains(
    dir: &str,
    file_name: &str,
    needle: &str,
    window_buf: &mut String,
    lower_buf: &mut String,
) -> bool {
    let margin = needle.len();
    window_buf.clear();
    window_buf.push_str(tail_at_char_boundary(dir, margin));
    window_buf.push(std::path::MAIN_SEPARATOR);
    window_buf.push_str(head_at_char_boundary(file_name, margin));
    contains_ignore_case(window_buf, needle, lower_buf)
}

/// Гарячий in-memory індекс.
#[derive(Debug, Default)]
pub struct InMemoryIndex {
    inner: RwLock<InMemoryIndexInner>,
}

impl InMemoryIndex {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(InMemoryIndexInner::default()),
        }
    }

    /// Додає запис файлу до індексу, розділяючи та інтернуючи шлях.
    pub fn insert(&self, record: &FileRecord) {
        let mut inner = self.inner.write().unwrap();
        inner.dir_interner.rebuild_lookup();
        inner.filename_interner.rebuild_lookup();

        let path = std::path::Path::new(&record.path);
        let parent_str = path.parent().and_then(|p| p.to_str()).unwrap_or("");
        let file_name_str = path.file_name().and_then(|f| f.to_str()).unwrap_or("");

        let dir_id = inner.dir_interner.intern(parent_str);
        let filename_id = inner.filename_interner.intern(file_name_str);

        let compact = CompactFileRecord::pack(record, dir_id, filename_id);
        inner.records.push(compact);
    }

    /// Отримує розпакований запис файлу за його індексом.
    pub fn get(&self, idx: usize) -> Option<FileRecord> {
        let inner = self.inner.read().unwrap();
        let compact = inner.records.get(idx)?;
        Some(compact.unpack(inner.resolve_path(compact)))
    }

    pub fn len(&self) -> usize {
        self.inner.read().unwrap().records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().unwrap().records.is_empty()
    }

    pub fn clear(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.records.clear();
    }

    /// Завершує наповнення індексу, вивільняючи lookup-таблиці інтернерів для економії пам'яті.
    pub fn finish_indexing(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.records.shrink_to_fit();
        inner.dir_interner.shrink_to_fit();
        inner.filename_interner.shrink_to_fit();
    }

    pub fn get_all(&self) -> Vec<FileRecord> {
        let inner = self.inner.read().unwrap();
        let mut records = Vec::with_capacity(inner.records.len());
        for compact in &inner.records {
            records.push(compact.unpack(inner.resolve_path(compact)));
        }
        records
    }

    /// Підрядковий регістронезалежний пошук за іменем файла та шляхом (T-018).
    ///
    /// Підрядок шукається один раз по унікальних інтернованих директоріях та
    /// іменах файлів (їх на порядки менше, ніж записів), далі записи
    /// відбираються за бітовими масками збігів. Збіг через межу
    /// «директорія\ім'я» перевіряється лише коли запит містить роздільник
    /// шляху — інакше такий збіг неможливий без збігу в директорії чи імені.
    /// Записи з рішенням Keep приховані з кандидатів і не повертаються.
    pub fn search(&self, query: &str, limit: usize) -> Vec<FileRecord> {
        if query.is_empty() || limit == 0 {
            return Vec::new();
        }
        let needle: String = query.chars().flat_map(|c| c.to_lowercase()).collect();
        let needs_boundary_check = needle.contains(std::path::MAIN_SEPARATOR);

        let inner = self.inner.read().unwrap();
        let dir_matches = match_interned_entries(&inner.dir_interner, &needle);
        let name_matches = match_interned_entries(&inner.filename_interner, &needle);

        let mut results = Vec::new();
        let mut window_buf = String::new();
        let mut lower_buf = String::new();
        for compact in &inner.records {
            if results.len() >= limit {
                break;
            }
            if parse_decision_val((compact.packed_meta >> 9) & 0x3) == Decision::Keep {
                continue;
            }

            let mut hit = dir_matches
                .get(compact.dir_id as usize)
                .copied()
                .unwrap_or(false)
                || name_matches
                    .get(compact.filename_id as usize)
                    .copied()
                    .unwrap_or(false);

            if !hit && needs_boundary_check {
                let dir = inner.dir_interner.resolve(compact.dir_id).unwrap_or("");
                let file_name = inner
                    .filename_interner
                    .resolve(compact.filename_id)
                    .unwrap_or("");
                hit = !dir.is_empty()
                    && boundary_window_contains(
                        dir,
                        file_name,
                        &needle,
                        &mut window_buf,
                        &mut lower_buf,
                    );
            }

            if hit {
                results.push(compact.unpack(inner.resolve_path(compact)));
            }
        }
        results
    }

    /// Розраховує загальний обсяг пам'яті в купі (heap).
    pub fn memory_usage(&self) -> usize {
        let inner = self.inner.read().unwrap();
        let mut total = 0;
        total += inner.records.capacity() * std::mem::size_of::<CompactFileRecord>();
        total += inner.dir_interner.memory_usage();
        total += inner.filename_interner.memory_usage();
        total
    }
}

impl trashradar_app::ports::HotIndex for InMemoryIndex {
    fn insert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError> {
        let mut inner = self.inner.write().unwrap();
        inner.dir_interner.rebuild_lookup();
        inner.filename_interner.rebuild_lookup();

        for record in records {
            let path = std::path::Path::new(&record.path);
            let parent_str = path.parent().and_then(|p| p.to_str()).unwrap_or("");
            let file_name_str = path.file_name().and_then(|f| f.to_str()).unwrap_or("");

            let dir_id = inner.dir_interner.intern(parent_str);
            let filename_id = inner.filename_interner.intern(file_name_str);

            let compact = CompactFileRecord::pack(&record, dir_id, filename_id);
            inner.records.push(compact);
        }

        Ok(())
    }

    fn finish_indexing(&self) -> Result<(), CoreError> {
        self.finish_indexing();
        Ok(())
    }

    fn len(&self) -> Result<usize, CoreError> {
        Ok(self.len())
    }

    fn is_empty(&self) -> Result<bool, CoreError> {
        Ok(self.is_empty())
    }

    fn clear(&self) -> Result<(), CoreError> {
        self.clear();
        Ok(())
    }

    fn get_all(&self) -> Result<Vec<FileRecord>, CoreError> {
        Ok(self.get_all())
    }

    fn for_each_mut(&self, f: &mut dyn FnMut(&mut FileRecord) -> bool) -> Result<(), CoreError> {
        // Тримаємо в пам'яті лише поточний запис (T-157): розпаковуємо один
        // компакт → викликаємо `f` → пакуємо назад ті самі dir/filename_id
        // (шлях не змінюється). Жодного get_all-піка на мільйони FileRecord.
        let mut inner = self.inner.write().unwrap();
        let len = inner.records.len();
        for i in 0..len {
            // CompactFileRecord — Copy (48 байт): копіюємо з-під immutable
            // резолву шляху, далі мутуємо records[i] без конфлікту borrow.
            let compact = inner.records[i];
            let path = inner.resolve_path(&compact);
            let mut record = compact.unpack(path);
            let keep_going = f(&mut record);
            inner.records[i] =
                CompactFileRecord::pack(&record, compact.dir_id, compact.filename_id);
            if !keep_going {
                break;
            }
        }
        Ok(())
    }

    fn search_file_records(&self, query: &str, limit: usize) -> Result<Vec<FileRecord>, CoreError> {
        Ok(self.search(query, limit))
    }

    fn max_candidate_id(&self) -> Result<u64, CoreError> {
        // Без get_all: прохід компактними записами без алокацій (T-154).
        let inner = self.inner.read().unwrap();
        Ok(inner
            .records
            .iter()
            .map(|compact| compact.candidate_id)
            .max()
            .unwrap_or(0))
    }

    fn remove_paths(&self, paths: &[String]) -> Result<usize, CoreError> {
        if paths.is_empty() {
            return Ok(0);
        }
        // Set lower-case цілей: перевірка запису за O(1) замість O(targets)
        // (T-154: remove_paths на 1.5 млн записів — гарячий шлях USN-дельти).
        let targets: std::collections::HashSet<String> = paths
            .iter()
            .map(|p| p.replace('/', "\\").to_ascii_lowercase())
            .collect();
        let mut inner = self.inner.write().unwrap();
        let before = inner.records.len();
        let resolved: Vec<String> = inner
            .records
            .iter()
            .map(|c| inner.resolve_path(c))
            .collect();
        let mut i = 0usize;
        inner.records.retain(|_| {
            let path = &resolved[i];
            i += 1;
            !targets.contains(&path.to_ascii_lowercase())
        });
        Ok(before - inner.records.len())
    }

    fn upsert_batch(&self, records: Vec<FileRecord>) -> Result<(), CoreError> {
        if records.is_empty() {
            return Ok(());
        }
        let mut inner = self.inner.write().unwrap();
        inner.dir_interner.rebuild_lookup();
        inner.filename_interner.rebuild_lookup();

        // T-154: пошук позиції запису має бути O(1) на запис, а не O(n) —
        // upsert 1.5 млн записів перерахунком детекторів інакше квадратичний.
        // Швидкий шлях: (dir_id, filename_id) → позиція (без алокацій рядків;
        // шляхи з одного джерела byte-ідентичні, інтернер дає точний збіг).
        let mut by_ids: std::collections::HashMap<(u32, u32), usize> =
            std::collections::HashMap::with_capacity(inner.records.len());
        for (idx, compact) in inner.records.iter().enumerate() {
            by_ids.insert((compact.dir_id, compact.filename_id), idx);
        }
        // Повільний шлях (лише при промаху точного збігу — нові файли або
        // інший регістр): lower-case повний шлях → позиція; будується один
        // раз на виклик, ліниво.
        let mut by_lower: Option<std::collections::HashMap<String, usize>> = None;

        for record in records {
            let path_norm = record.path.replace('/', "\\");
            let path = std::path::Path::new(&path_norm);
            let parent_str = path.parent().and_then(|p| p.to_str()).unwrap_or("");
            let file_name_str = path.file_name().and_then(|f| f.to_str()).unwrap_or("");

            let exact = inner
                .dir_interner
                .lookup_id(parent_str)
                .zip(inner.filename_interner.lookup_id(file_name_str))
                .and_then(|ids| by_ids.get(&ids).copied());
            let idx = match exact {
                Some(idx) => Some(idx),
                None => {
                    let lower = by_lower.get_or_insert_with(|| {
                        inner
                            .records
                            .iter()
                            .enumerate()
                            .map(|(idx, c)| (inner.resolve_path(c).to_ascii_lowercase(), idx))
                            .collect()
                    });
                    lower.get(&path_norm.to_ascii_lowercase()).copied()
                }
            };

            let dir_id = inner.dir_interner.intern(parent_str);
            let filename_id = inner.filename_interner.intern(file_name_str);
            let mut packed = record;
            packed.path = path_norm;
            let path_lower = packed.path.to_ascii_lowercase();
            let compact = CompactFileRecord::pack(&packed, dir_id, filename_id);
            let slot = match idx {
                Some(idx) => {
                    inner.records[idx] = compact;
                    idx
                }
                None => {
                    inner.records.push(compact);
                    inner.records.len() - 1
                }
            };
            // Наступні записи цього ж батча мають бачити щойно вставлене.
            by_ids.insert((dir_id, filename_id), slot);
            if let Some(lower) = by_lower.as_mut() {
                lower.insert(path_lower, slot);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(id: u64, path: &str) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: path.to_string(),
            size: ByteSize(id * 1024),
            created_at: Some(FsTimestamp(130000000000000000 + id as i64 * 10_000_000)),
            modified_at: Some(FsTimestamp(140000000000000000 + id as i64 * 10_000_000)),
            accessed_at: Some(FsTimestamp(150000000000000000 + id as i64 * 10_000_000)),
            kind: FileKind::Video,
            unit: CandidateUnit::File,
            category: CategoryId::ForgottenVideos,
            safety: SafetyLevel::SafeToBulk,
            decision: Decision::Undecided,
            detector_id: String::new(),
            explanation: String::new(),
            attributes: FileAttributes {
                raw_bits: 7,
                is_readonly: true,
                is_hidden: true,
                is_system: true,
                is_temporary: false,
            },
        }
    }

    #[test]
    fn test_interning_and_resolution() {
        let mut interner = PathInterner::new();
        let id1 = interner.intern("C:\\Users\\Ada\\Videos");
        let id2 = interner.intern("C:\\Users\\Ada\\Documents");
        let id3 = interner.intern("C:\\Users\\Ada\\Videos"); // Duplicate

        assert_eq!(id1, id3);
        assert_ne!(id1, id2);
        assert_eq!(interner.resolve(id1), Some("C:\\Users\\Ada\\Videos"));
        assert_eq!(interner.resolve(id2), Some("C:\\Users\\Ada\\Documents"));
    }

    #[test]
    fn test_compact_record_size() {
        assert_eq!(std::mem::size_of::<CompactFileRecord>(), 48);
    }

    #[test]
    fn test_pack_unpack_roundtrip() {
        let record = sample_record(42, "C:\\Users\\Ada\\Videos\\holiday.mp4");
        let index = InMemoryIndex::new();
        index.insert(&record);

        assert_eq!(index.len(), 1);
        let restored = index.get(0).unwrap();

        assert_eq!(restored.candidate_id, record.candidate_id);
        assert_eq!(restored.path, record.path);
        assert_eq!(restored.size, record.size);
        assert_eq!(
            restored.created_at.unwrap().0 / 10_000_000,
            record.created_at.unwrap().0 / 10_000_000
        );
        assert_eq!(
            restored.modified_at.unwrap().0 / 10_000_000,
            record.modified_at.unwrap().0 / 10_000_000
        );
        assert_eq!(
            restored.accessed_at.unwrap().0 / 10_000_000,
            record.accessed_at.unwrap().0 / 10_000_000
        );
        assert_eq!(restored.kind, record.kind);
        assert_eq!(restored.unit, record.unit);
        assert_eq!(restored.category, record.category);
        assert_eq!(restored.safety, record.safety);
        assert_eq!(restored.decision, record.decision);
        assert_eq!(restored.attributes, record.attributes);
    }

    /// Регрес: КОЖНА категорія має пережити pack→unpack. Раніше нові
    /// EmptyFolders/SparseFolders/DeepPaths (дискримінанти 9/10/11) падали в
    /// `_ => Uncategorized` у `parse_category_val` — folder-розділи виглядали
    /// порожніми, бо категорія губилась у компактному записі. Ітеруємо
    /// `CategoryId::ALL`, щоб будь-яка майбутня категорія не повторила це мовчки.
    #[test]
    fn every_category_survives_pack_unpack() {
        for (i, category) in CategoryId::ALL.iter().enumerate() {
            let mut record = sample_record(i as u64, &format!("C:\\dir_{i}\\unit"));
            record.category = *category;
            record.unit = CandidateUnit::Folder;
            let index = InMemoryIndex::new();
            index.insert(&record);
            let restored = index.get(0).unwrap();
            assert_eq!(
                restored.category, *category,
                "категорія {category:?} не пережила pack/unpack"
            );
            assert_eq!(restored.unit, CandidateUnit::Folder);
        }
    }

    #[test]
    fn test_five_million_records_memory_footprint() {
        let index = InMemoryIndex::new();

        {
            let mut inner = index.inner.write().unwrap();
            inner.records.reserve(5_000_000);
            inner.dir_interner.offsets.reserve(500_000);
            inner.dir_interner.lookup.reserve(500_000);
            inner.filename_interner.offsets.reserve(1_000_000);
            inner.filename_interner.lookup.reserve(1_000_000);

            for i in 0..500_000 {
                let dir = format!("C:\\Users\\User\\Folder_{}", i);
                inner.dir_interner.intern(&dir);
            }

            for i in 0..1_000_000 {
                let filename = format!("file_name_{}.dat", i);
                inner.filename_interner.intern(&filename);
            }

            for i in 0..5_000_000 {
                let dir_id = (i % 500_000) as u32;
                let filename_id = (i % 1_000_000) as u32;

                let compact = CompactFileRecord {
                    candidate_id: i as u64,
                    size: (i * 123) as u64,
                    accessed_at: (1500000000 + i) as u32,
                    created_at: (1300000000 + i) as u32,
                    modified_at: (1400000000 + i) as u32,
                    dir_id,
                    filename_id,
                    attributes: 7,
                    packed_meta: 42,
                };
                inner.records.push(compact);
            }
        }

        // Очищуємо lookup таблиці для економії пам'яті в режимі запитів
        index.finish_indexing();

        let total_bytes = index.memory_usage();
        let total_mb = total_bytes as f64 / 1024.0 / 1024.0;
        println!(
            "Estimated memory usage for 5M records (with cleanups): {:.2} MB",
            total_mb
        );

        assert!(
            total_mb < 300.0,
            "Memory usage {:.2} MB exceeds the 300 MB limit!",
            total_mb
        );
    }

    #[test]
    fn test_concurrent_batch_insertions() {
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let index = Arc::new(InMemoryIndex::new());
        let mut threads = Vec::new();

        // Спавнить 8 потоків для паралельного запису пакетів записів
        for thread_idx in 0..8 {
            let index_clone = Arc::clone(&index);
            threads.push(thread::spawn(move || {
                let mut records = Vec::new();
                for i in 0..10_000 {
                    let id = thread_idx * 100_000 + i;
                    records.push(sample_record(
                        id,
                        &format!("C:\\Users\\User\\dir_{}\\file_{}.mp4", thread_idx, i),
                    ));
                }
                use trashradar_app::ports::HotIndex;
                index_clone.insert_batch(records).unwrap();
            }));
        }

        // Потік читача, який робить запити до індексу паралельно із записом
        let index_clone = Arc::clone(&index);
        let reader_thread = thread::spawn(move || {
            let mut read_attempts = 0;
            let mut max_observed_len = 0;
            while max_observed_len < 80_000 && read_attempts < 1000 {
                if let Ok(current_len) = trashradar_app::ports::HotIndex::len(&*index_clone) {
                    if current_len > max_observed_len {
                        max_observed_len = current_len;
                    }
                }
                read_attempts += 1;
                thread::sleep(Duration::from_millis(1));
            }
            println!(
                "Reader thread finished after {} attempts. Max observed len: {}",
                read_attempts, max_observed_len
            );
        });

        for t in threads {
            t.join().unwrap();
        }
        reader_thread.join().unwrap();

        assert_eq!(index.len(), 80_000);
    }

    #[test]
    fn test_search_by_filename_substring_case_insensitive() {
        let index = InMemoryIndex::new();
        index.insert(&sample_record(
            1,
            "C:\\Users\\Ada\\Videos\\Holiday_Trip.mp4",
        ));
        index.insert(&sample_record(
            2,
            "C:\\Users\\Ada\\Videos\\work_recording.mp4",
        ));
        index.insert(&sample_record(3, "C:\\Users\\Ada\\Documents\\notes.txt"));

        let results = index.search("holiday", 100);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].candidate_id, CandidateId(1));

        // Регістр запиту не має значення
        let results = index.search("HOLIDAY_trip", 100);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "C:\\Users\\Ada\\Videos\\Holiday_Trip.mp4");
    }

    #[test]
    fn test_search_by_directory_substring() {
        let index = InMemoryIndex::new();
        index.insert(&sample_record(1, "C:\\Users\\Ada\\Videos\\clip.mp4"));
        index.insert(&sample_record(2, "C:\\Users\\Ada\\Documents\\notes.txt"));
        index.insert(&sample_record(3, "D:\\Projects\\demo\\report.pdf"));

        let results = index.search("users\\ada", 100);
        assert_eq!(results.len(), 2);

        let results = index.search("projects", 100);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].candidate_id, CandidateId(3));
    }

    #[test]
    fn test_search_spanning_directory_filename_boundary() {
        let index = InMemoryIndex::new();
        index.insert(&sample_record(1, "C:\\Users\\Ada\\Videos\\holiday.mp4"));
        index.insert(&sample_record(2, "C:\\Users\\Ada\\Videos\\other.mp4"));

        // Підрядок перетинає межу «директорія\ім'я файла»
        let results = index.search("videos\\holiday", 100);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].candidate_id, CandidateId(1));
    }

    #[test]
    fn test_search_respects_limit_and_empty_query() {
        let index = InMemoryIndex::new();
        for i in 0..50 {
            index.insert(&sample_record(
                i,
                &format!("C:\\Users\\Ada\\Videos\\clip_{}.mp4", i),
            ));
        }

        assert_eq!(index.search("clip", 10).len(), 10);
        assert_eq!(index.search("clip", 100).len(), 50);
        assert!(index.search("", 100).is_empty());
        assert!(index.search("clip", 0).is_empty());
    }

    #[test]
    fn test_search_excludes_keep_decisions() {
        let index = InMemoryIndex::new();
        let mut kept = sample_record(1, "C:\\Users\\Ada\\Videos\\keep_me.mp4");
        kept.decision = Decision::Keep;
        index.insert(&kept);
        index.insert(&sample_record(2, "C:\\Users\\Ada\\Videos\\reap_me.mp4"));

        let results = index.search("me.mp4", 100);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].candidate_id, CandidateId(2));
    }

    #[test]
    #[ignore = "перф-гейт T-018: запускається окремо у release-збірці"]
    fn search_one_million_records_under_100ms() {
        use std::time::Instant;
        use trashradar_app::ports::HotIndex;

        let index = InMemoryIndex::new();
        let mut batch = Vec::with_capacity(100_000);
        for i in 0u64..1_000_000 {
            batch.push(sample_record(
                i,
                &format!(
                    "C:\\Users\\User\\Folder_{}\\file_name_{}.dat",
                    i % 200_000,
                    i
                ),
            ));
            if batch.len() == 100_000 {
                index.insert_batch(std::mem::take(&mut batch)).unwrap();
                batch = Vec::with_capacity(100_000);
            }
        }
        index.finish_indexing();
        assert_eq!(InMemoryIndex::len(&index), 1_000_000);

        // Рідкісний підрядок: пошук мусить пройти всі записи, а не зупинитись на limit
        let started = Instant::now();
        let results = index.search("name_999999", 200);
        let elapsed = started.elapsed();
        println!(
            "Substring search over 1M records took {:?}, found {}",
            elapsed,
            results.len()
        );
        assert_eq!(results.len(), 1);
        assert!(
            elapsed.as_millis() < 100,
            "Search took {:?}, DoD target is < 100 ms",
            elapsed
        );

        // Частий підрядок з лімітом — так само в межах цілі
        let started = Instant::now();
        let results = index.search("folder_1999", 200);
        let elapsed = started.elapsed();
        println!(
            "Frequent-substring search took {:?}, found {}",
            elapsed,
            results.len()
        );
        assert!(!results.is_empty());
        assert!(results.len() <= 200);
        assert!(
            elapsed.as_millis() < 100,
            "Search took {:?}, DoD target is < 100 ms",
            elapsed
        );
    }

    fn temp_profile_dir(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "trashradar-test-{}-{}",
            name,
            std::time::Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn test_sqlite_to_in_memory_sync_and_checksum_verification() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        use trashradar_index_sqlite::IndexDatabase;

        let temp_dir = temp_profile_dir("sync-test");
        let db_path = temp_dir.join("index.sqlite3");

        // 1. Open SQLite database and insert 1,000 file records
        let mut sqlite_db = IndexDatabase::open(&db_path).expect("open sqlite db");

        let mut records = Vec::new();
        for i in 0..1000 {
            records.push(sample_record(
                i,
                &format!("C:\\Users\\Ada\\dir_{}\\file_{}.txt", i / 10, i),
            ));
        }
        sqlite_db
            .upsert_file_records_batch(&records)
            .expect("upsert records to sqlite");

        // 2. Read all records from SQLite (simulating startup recovery)
        let loaded_records = sqlite_db
            .read_all_file_records()
            .expect("read all from sqlite");

        // 3. Load them into HotIndex in-memory index
        let in_memory_index = InMemoryIndex::new();
        use trashradar_app::ports::HotIndex;
        in_memory_index
            .insert_batch(loaded_records)
            .expect("insert batch to memory");
        in_memory_index.finish_indexing();

        // 4. Retrieve all records from both and verify they match by length and checksum
        let sqlite_all = sqlite_db.read_all_file_records().expect("read all sqlite");
        let memory_all = in_memory_index.get_all();

        assert_eq!(sqlite_all.len(), 1000);
        assert_eq!(memory_all.len(), 1000);

        // Helper to calculate checksum
        let calculate_checksum = |recs: &[FileRecord]| -> u64 {
            let mut sorted = recs.to_vec();
            sorted.sort_by_key(|r| r.candidate_id.0);
            let mut hasher = DefaultHasher::new();
            for r in sorted {
                r.candidate_id.0.hash(&mut hasher);
                r.path.hash(&mut hasher);
                r.size.0.hash(&mut hasher);
                r.created_at.map(|t| t.0).hash(&mut hasher);
                r.modified_at.map(|t| t.0).hash(&mut hasher);
                r.accessed_at.map(|t| t.0).hash(&mut hasher);
                (r.kind as u8).hash(&mut hasher);
                (r.unit as u8).hash(&mut hasher);
                (r.category as u8).hash(&mut hasher);
                (r.safety as u8).hash(&mut hasher);
                (r.decision as u8).hash(&mut hasher);
                r.attributes.raw_bits.hash(&mut hasher);
                r.attributes.is_readonly.hash(&mut hasher);
                r.attributes.is_hidden.hash(&mut hasher);
                r.attributes.is_system.hash(&mut hasher);
                r.attributes.is_temporary.hash(&mut hasher);
            }
            hasher.finish()
        };

        let checksum_sqlite = calculate_checksum(&sqlite_all);
        let checksum_memory = calculate_checksum(&memory_all);

        println!(
            "Checksum SQLite: {}, Checksum Memory: {}",
            checksum_sqlite, checksum_memory
        );
        assert_eq!(checksum_sqlite, checksum_memory, "Checksums do not match!");

        // Clean up
        std::fs::remove_dir_all(temp_dir).unwrap_or(());
    }

    /// T-157: `for_each_mut` пише зміни назад у компактний запис (шлях і id
    /// незмінні), зберігаючи інші поля через pack/unpack roundtrip.
    #[test]
    fn for_each_mut_writes_changes_back_preserving_path_and_id() {
        use trashradar_app::ports::HotIndex;
        let index = InMemoryIndex::new();
        index
            .insert_batch(vec![
                sample_record(1, "C:\\a\\one.mp4"),
                sample_record(2, "C:\\a\\two.mp4"),
                sample_record(3, "C:\\b\\three.mp4"),
            ])
            .unwrap();
        index.finish_indexing();

        // Мутуємо лише запис #2: категорія + рішення.
        let mut seen = 0u64;
        index
            .for_each_mut(&mut |record| {
                seen += 1;
                if record.candidate_id.0 == 2 {
                    record.category = CategoryId::LargeFiles;
                    record.decision = Decision::Keep;
                }
                true
            })
            .unwrap();
        assert_eq!(seen, 3, "прохід має побачити кожен запис");

        let all = index.get_all();
        let r2 = all.iter().find(|r| r.candidate_id.0 == 2).unwrap();
        assert_eq!(r2.category, CategoryId::LargeFiles);
        assert_eq!(r2.decision, Decision::Keep);
        assert_eq!(r2.path, "C:\\a\\two.mp4", "шлях лишається незмінним");
        // Незачеплені записи зберегли свої значення (repack — no-op за вмістом).
        let r1 = all.iter().find(|r| r.candidate_id.0 == 1).unwrap();
        assert_eq!(r1.category, CategoryId::ForgottenVideos);
        assert_eq!(r1.decision, Decision::Undecided);
        assert_eq!(r1.path, "C:\\a\\one.mp4");
    }

    /// T-157: `f` повертає `false` → обхід зупиняється (кооперативна відміна).
    #[test]
    fn for_each_mut_stops_when_callback_returns_false() {
        use trashradar_app::ports::HotIndex;
        let index = InMemoryIndex::new();
        index
            .insert_batch((0..10).map(|i| sample_record(i, "C:\\x\\f.mp4")).collect())
            .unwrap();

        let mut visited = 0u64;
        index
            .for_each_mut(&mut |_record| {
                visited += 1;
                visited < 3 // зупинитись після третього
            })
            .unwrap();
        assert_eq!(visited, 3, "обхід має зупинитись рівно на третьому записі");
    }
}
