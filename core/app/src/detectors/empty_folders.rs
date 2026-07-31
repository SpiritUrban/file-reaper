//! Порожні та майже порожні папки як folder-одиниці (розділи «Порожні папки»
//! і «Майже порожні»).
//!
//! На відміну від предикатних детекторів ферми (T-037), ці категорії
//! **не** можна визначити з потоку файлів: порожня папка не має жодного файла
//! в індексі й тому невидима файловому конвеєру. Тут — чиста агрегація над
//! **повним переліком директорій тому** (винесеним зі сканера) + файлами:
//!
//! - **Порожня** = рекурсивно порожня (жодного файла в усьому піддереві).
//!   Показуємо **найвищу** порожню папку (батько НЕ порожній) — видалення
//!   прибирає всю порожню гілку одним махом.
//! - **Майже порожня** = рекурсивно `1..=N` файлів (N — поріг, дефолт 3).
//!   Показуємо найвищу таку папку (батько вже НЕ майже порожній).
//!
//! Результат — `FileRecord` з `unit = Folder`, стабільним id (namespace, щоб
//! не колізіонувати з файловими 0..N) і готовою категорією. Інʼєкція в індекс —
//! пост-скановим пасом у shell (той самий патерн, що й каскад дублікатів).

use std::collections::HashMap;

use super::folder_units::{id_ns, stable_folder_id};
use super::format::{format_bytes_as_gb, format_file_count};
use trashradar_domain::candidate::{
    ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
    SafetyLevel,
};
use trashradar_domain::category::CategoryId;

/// Конфіг детекції папок: пороги «майже порожньої» та «занадто глибокої».
#[derive(Debug, Clone, Copy)]
pub struct FolderScanConfig {
    /// Максимум рекурсивних файлів, щоб папка вважалась «майже порожньою» (≥1).
    pub sparse_max_files: u32,
    /// Глибина папки (сегментів під коренем тому) понад це значення →
    /// «Глибокі шляхи».
    pub deep_path_max_depth: u32,
}

impl Default for FolderScanConfig {
    fn default() -> Self {
        Self {
            sparse_max_files: trashradar_domain::settings::DEFAULT_SPARSE_MAX_FILES,
            deep_path_max_depth: trashradar_domain::settings::DEFAULT_DEEP_PATH_MAX_DEPTH,
        }
    }
}

/// Класифікація директорії (взаємовиключна, за пріоритетом Empty > Sparse >
/// Deep) — щоб одна папка потрапляла рівно в одну категорію й не роздвоювала
/// цифру «можна звільнити».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirClass {
    Empty,
    Sparse,
    Deep,
    None,
}

/// Внутрішній вузол директорії під час агрегації.
struct DirNode {
    /// Оригінальний шлях (для показу/reap) — регістр збережено.
    orig: String,
    /// Індекс батька у векторі `nodes` (None = топ-рівень / корінь тому).
    parent: Option<usize>,
    recursive_files: u64,
    recursive_bytes: u64,
}

/// Нормалізувати шлях директорії для ключа: `/`→`\`, без хвостового `\`,
/// нижній регістр (Windows — регістронезалежний). Порожній лишається порожнім.
fn normalize(path: &str) -> String {
    use trashradar_domain::path_key;
    // Правило 6a: роздільник і регістр — за платформою.
    let replaced = path_key::normalize_separators(path);
    path_key::fold_case(path_key::trim_trailing_separators(&replaced))
}

/// Батьківський нормалізований шлях: усе до останнього роздільника. `None`,
/// якщо роздільника немає (корінь тому на кшталт `c:`).
fn parent_of(norm: &str) -> Option<&str> {
    norm.rfind(trashradar_domain::path_key::SEPARATOR)
        .map(|idx| &norm[..idx])
}

/// Це корінь тому (`c:` без жодного роздільника) — цілий том у розділ не
/// пропонуємо.
fn is_drive_root(norm: &str) -> bool {
    !norm.contains(trashradar_domain::path_key::SEPARATOR)
}

/// Службовий файл, що не рахується як «вміст» теки для порожності:
/// `desktop.ini`, `Thumbs.db`, `ehthumbs.db`, `.DS_Store`.
fn is_ignorable_leaf(path: &str) -> bool {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    matches!(
        name.to_ascii_lowercase().as_str(),
        "desktop.ini" | "thumbs.db" | "ehthumbs.db" | ".ds_store"
    )
}

/// Знайти folder-одиниці порожніх і майже порожніх папок.
///
/// - `dir_paths` — **усі** директорії тому (включно з порожніми), виключення
///   вже застосовані сканером;
/// - `files` — `(шлях файла, розмір)` з індексу (лише `unit == File`);
/// - `cfg.sparse_max_files` — поріг «майже порожньої» (клемпиться до ≥1).
pub fn detect_folder_units(
    dir_paths: &[String],
    files: &[(String, u64)],
    cfg: FolderScanConfig,
) -> Vec<FileRecord> {
    let sparse_max = cfg.sparse_max_files.max(1) as u64;
    let deep_max = cfg.deep_path_max_depth.max(1) as usize;

    // 1) Унікальні директорії за нормалізованим ключем; перший оригінал виграє.
    let mut index_of: HashMap<String, usize> = HashMap::with_capacity(dir_paths.len());
    let mut nodes: Vec<DirNode> = Vec::with_capacity(dir_paths.len());
    let mut norms: Vec<String> = Vec::with_capacity(dir_paths.len());
    for path in dir_paths {
        let norm = normalize(path);
        if norm.is_empty() {
            continue;
        }
        if let std::collections::hash_map::Entry::Vacant(slot) = index_of.entry(norm.clone()) {
            slot.insert(nodes.len());
            nodes.push(DirNode {
                orig: path.trim_end_matches(['\\', '/']).to_string(),
                parent: None,
                recursive_files: 0,
                recursive_bytes: 0,
            });
            norms.push(norm);
        }
    }

    // 2) Батьківські звʼязки за нормалізованими префіксами.
    for i in 0..nodes.len() {
        if let Some(parent_norm) = parent_of(&norms[i]) {
            nodes[i].parent = index_of.get(parent_norm).copied();
        }
    }

    // 3) Безпосередні файли/байти → директорія-батько кожного файла.
    //    Службові junk-файли (desktop.ini, Thumbs.db, .DS_Store) НЕ рахуються
    //    як вміст: інакше на реальному Windows майже кожна «порожня» тека має
    //    desktop.ini і розділ «Порожні папки» нічого не знаходить. Такий
    //    junk піде в карантин разом з текою — це очікувано для прибирання.
    for (path, size) in files {
        if is_ignorable_leaf(path) {
            continue;
        }
        let norm = normalize(path);
        let Some(dir_norm) = parent_of(&norm) else {
            continue;
        };
        if let Some(&idx) = index_of.get(dir_norm) {
            nodes[idx].recursive_files = nodes[idx].recursive_files.saturating_add(1);
            nodes[idx].recursive_bytes = nodes[idx].recursive_bytes.saturating_add(*size);
        }
    }

    // 4) Рекурсивний rollup знизу вгору: діти глибші → додаємо в батька.
    //    Сортуємо індекси за глибиною (кількість `\`) спадання.
    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by(|&a, &b| depth(&norms[b]).cmp(&depth(&norms[a])));
    for &i in &order {
        if let Some(parent) = nodes[i].parent {
            let (rf, rb) = (nodes[i].recursive_files, nodes[i].recursive_bytes);
            nodes[parent].recursive_files = nodes[parent].recursive_files.saturating_add(rf);
            nodes[parent].recursive_bytes = nodes[parent].recursive_bytes.saturating_add(rb);
        }
    }

    // 5) Взаємовиключна класифікація кожної директорії (Empty > Sparse > Deep).
    let class: Vec<DirClass> = (0..nodes.len())
        .map(|i| {
            if is_drive_root(&norms[i]) {
                return DirClass::None; // цілий том — не кандидат
            }
            let files = nodes[i].recursive_files;
            if files == 0 {
                DirClass::Empty
            } else if files <= sparse_max {
                DirClass::Sparse
            } else if depth(&norms[i]) > deep_max {
                DirClass::Deep
            } else {
                DirClass::None
            }
        })
        .collect();

    // 6) Topmost кожної категорії = вузол свого класу, чий батько має ІНШИЙ клас
    //    (батько без запису в множині → цей вузол найвищий у гілці).
    let mut units = Vec::new();
    for i in 0..nodes.len() {
        let my = class[i];
        if my == DirClass::None {
            continue;
        }
        let parent_same = nodes[i].parent.map(|p| class[p] == my).unwrap_or(false);
        if parent_same {
            continue; // topmost — вище по гілці
        }
        let (category, explanation, id_namespace) = match my {
            DirClass::Empty => (
                CategoryId::EmptyFolders,
                empty_explanation(),
                id_ns::EMPTY_FOLDERS,
            ),
            DirClass::Sparse => (
                CategoryId::SparseFolders,
                sparse_explanation(nodes[i].recursive_files, nodes[i].recursive_bytes),
                id_ns::SPARSE_FOLDERS,
            ),
            DirClass::Deep => (
                CategoryId::DeepPaths,
                deep_explanation(depth(&norms[i]), nodes[i].recursive_bytes),
                id_ns::DEEP_PATHS,
            ),
            DirClass::None => unreachable!(),
        };
        units.push(make_unit(
            &nodes[i].orig,
            nodes[i].recursive_bytes,
            nodes[i].recursive_files,
            category,
            explanation,
            id_namespace,
        ));
    }
    units
}

/// Глибина шляху для порядку rollup = кількість роздільників.
///
/// Правило 6a: тут був хардкод `b'\\'`, і поза Windows глибина ставала **0
/// для всіх** папок. Наслідки тихі й обидва неправильні: сортування кроку 4
/// вироджувалось, тож батьки не збирали лічильники дітей (гілка з файлом
/// у листі виглядала порожньою аж до передостаннього рівня), а умова
/// `depth > deep_max` не спрацьовувала ніколи — розділ «Задовгі шляхи»
/// просто не знаходив нічого. Компіляція про це не казала нічого.
fn depth(norm: &str) -> usize {
    norm.chars()
        .filter(|&c| c == trashradar_domain::path_key::SEPARATOR)
        .count()
}

fn empty_explanation() -> String {
    "порожня папка (без файлів у піддереві)".to_string()
}

fn sparse_explanation(files: u64, bytes: u64) -> String {
    if bytes == 0 {
        format!("майже порожня папка · {} файлів", format_file_count(files))
    } else {
        format!(
            "майже порожня папка · {} файлів · {}",
            format_file_count(files),
            format_bytes_as_gb(bytes)
        )
    }
}

fn deep_explanation(depth: usize, bytes: u64) -> String {
    format!(
        "задовга вкладеність · глибина {} · {}",
        depth,
        format_bytes_as_gb(bytes)
    )
}

fn make_unit(
    path: &str,
    bytes: u64,
    file_count: u64,
    category: CategoryId,
    explanation: String,
    id_namespace: u64,
) -> FileRecord {
    let _ = file_count; // count відображається UI зі списку файлів; тут — у поясненні
    FileRecord {
        candidate_id: CandidateId(stable_folder_id(path, id_namespace)),
        path: path.to_string(),
        size: ByteSize(bytes),
        created_at: None,
        modified_at: None,
        accessed_at: None,
        kind: FileKind::Other,
        unit: CandidateUnit::Folder,
        category,
        // Порожні/майже порожні папки бувають керовані застосунками — огляд
        // рекомендовано; карантин 30 днів усе одно страхує.
        safety: SafetyLevel::ReviewRecommended,
        decision: Decision::Undecided,
        detector_id: String::new(),
        explanation,
        attributes: FileAttributes::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Фікстури записані Windows-шляхами (детектор народився на NTFS), але в
    /// шлях поточної платформи їх переводить один хелпер — інакше кожен
    /// літерал перевіряв би роздільник, а не логіку (правило 6a).
    use trashradar_domain::path_key::fixture as p;

    fn dirs(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|s| p(s)).collect()
    }

    fn files(pairs: &[(&str, u64)]) -> Vec<(String, u64)> {
        pairs.iter().map(|(path, size)| (p(path), *size)).collect()
    }

    /// Поріг глибини «вимкнено» (дуже високий) — щоб тести empty/sparse не
    /// зачіпались класифікацією Deep.
    fn cfg(n: u32) -> FolderScanConfig {
        FolderScanConfig {
            sparse_max_files: n,
            deep_path_max_depth: 1000,
        }
    }

    fn cfg_deep(sparse: u32, deep: u32) -> FolderScanConfig {
        FolderScanConfig {
            sparse_max_files: sparse,
            deep_path_max_depth: deep,
        }
    }

    #[test]
    fn recursively_empty_branch_yields_single_topmost_unit() {
        // C:\x\A\B\C — всі порожні; x має достатньо файлів, щоб не бути sparse
        // (5 > поріг 3) → лишається лише одна порожня одиниця = topmost A.
        let dir_paths = dirs(&[r"C:\x", r"C:\x\A", r"C:\x\A\B", r"C:\x\A\B\C"]);
        let file_list: Vec<(String, u64)> = (0..5)
            .map(|i| (p(&format!(r"C:\x\keep{i}.txt")), 10))
            .collect();
        let units = detect_folder_units(&dir_paths, &file_list, cfg(3));
        assert_eq!(units.len(), 1, "лише найвища порожня папка");
        assert_eq!(units[0].path, p(r"C:\x\A"));
        assert_eq!(units[0].category, CategoryId::EmptyFolders);
        assert_eq!(units[0].unit, CandidateUnit::Folder);
        assert_eq!(units[0].size.0, 0);
    }

    #[test]
    fn empty_child_and_sparse_ancestor_coexist_as_distinct_categories() {
        // Реальна вкладеність: C:\x має 1 файл (sparse) + порожню гілку A.
        // Обидва — валідні topmost у своїх категоріях (перетин навмисний).
        let dir_paths = dirs(&[r"C:\x", r"C:\x\A", r"C:\x\A\B"]);
        let file_list = files(&[(r"C:\x\only.txt", 10)]);
        let units = detect_folder_units(&dir_paths, &file_list, cfg(3));
        let empty: Vec<_> = units
            .iter()
            .filter(|u| u.category == CategoryId::EmptyFolders)
            .collect();
        let sparse: Vec<_> = units
            .iter()
            .filter(|u| u.category == CategoryId::SparseFolders)
            .collect();
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].path, p(r"C:\x\A"));
        assert_eq!(sparse.len(), 1);
        assert_eq!(sparse[0].path, p(r"C:\x"));
    }

    #[test]
    fn whole_empty_top_level_folder_is_topmost() {
        // C:\Empty та піддерево повністю без файлів → сама C:\Empty.
        let dir_paths = dirs(&[r"C:\Empty", r"C:\Empty\sub"]);
        let units = detect_folder_units(&dir_paths, &[], cfg(3));
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, p(r"C:\Empty"));
        assert_eq!(units[0].category, CategoryId::EmptyFolders);
    }

    #[test]
    fn drive_root_never_reported() {
        // Порожній «том» (лише корінь) не пропонуємо цілком.
        let dir_paths = dirs(&[r"C:", r"C:\"]);
        let units = detect_folder_units(&dir_paths, &[], cfg(3));
        assert!(units.is_empty());
    }

    #[test]
    fn sparse_topmost_respects_threshold_and_nesting() {
        // C:\p: 2 файли безпосередньо + підпапка q з 1 файлом → рекурсивно 3.
        // Поріг 3 → p майже порожня (topmost, бо в корені). q (1 файл) НЕ
        // окремо: його батько p теж майже порожній.
        let dir_paths = dirs(&[r"C:\p", r"C:\p\q"]);
        let file_list = files(&[(r"C:\p\a.txt", 1), (r"C:\p\b.txt", 2), (r"C:\p\q\c.txt", 4)]);
        let units = detect_folder_units(&dir_paths, &file_list, cfg(3));
        assert_eq!(units.len(), 1, "лише topmost sparse");
        assert_eq!(units[0].path, p(r"C:\p"));
        assert_eq!(units[0].category, CategoryId::SparseFolders);
        assert_eq!(units[0].size.0, 7);
    }

    #[test]
    fn folder_over_threshold_is_not_sparse_but_inner_sparse_child_is_topmost() {
        // C:\big має 10 файлів (не sparse), але підпапка thin — 2 файли (sparse).
        let dir_paths = dirs(&[r"C:\big", r"C:\big\thin"]);
        let mut pairs: Vec<(String, u64)> = (0..10)
            .map(|i| (p(&format!(r"C:\big\f{i}.dat")), 100))
            .collect();
        pairs.push((p(r"C:\big\thin\x.txt"), 5));
        pairs.push((p(r"C:\big\thin\y.txt"), 6));
        let units = detect_folder_units(&dir_paths, &pairs, cfg(3));
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, p(r"C:\big\thin"));
        assert_eq!(units[0].category, CategoryId::SparseFolders);
        assert_eq!(units[0].size.0, 11);
    }

    #[test]
    fn threshold_boundary_excludes_over_limit() {
        // Рівно N+1 файлів → не sparse.
        let dir_paths = dirs(&[r"C:\d"]);
        let pairs: Vec<(String, u64)> = (0..4).map(|i| (p(&format!(r"C:\d\f{i}")), 1)).collect();
        let units = detect_folder_units(&dir_paths, &pairs, cfg(3));
        assert!(units.is_empty(), "4 файли при порозі 3 — не майже порожня");
    }

    /// Ізолюючий тест на баг, що завалив Linux-джобу: `depth` рахував лише
    /// `\`, тож поза Windows глибина була 0 для всіх папок — rollup лічильників
    /// вироджувався, а «Задовгі шляхи» не знаходились ніколи. Перевіряє саме
    /// підрахунок, а не наслідок, і осмислений на будь-якій платформі.
    #[test]
    fn depth_counts_native_separators() {
        assert_eq!(depth(&normalize(&p(r"C:\a"))), 1);
        assert_eq!(depth(&normalize(&p(r"C:\a\b\c\d"))), 4);
        assert!(
            depth(&normalize(&p(r"C:\a\b\c\d"))) > depth(&normalize(&p(r"C:\a\b"))),
            "глибша тека мусить мати більшу глибину — інакше сортування rollup вироджується"
        );
    }

    #[test]
    fn path_matching_follows_the_filesystem_case_rules() {
        // Той самий вхід дає РІЗНИЙ правильний результат на різних ФС, і це
        // не дефект (правило 6a): Windows/macOS регістр не розрізняють, тож
        // файл належить теці; Linux розрізняє, тож `/proj/bin` і `/Proj/Bin`
        // — дві різні теки, і зливати їх було б помилкою.
        let dir_paths = dirs(&[r"C:\Proj\Bin"]);
        let file_list = files(&[(r"c:\proj\bin\a.o", 3)]);
        let units = detect_folder_units(&dir_paths, &file_list, cfg(3));
        assert_eq!(units.len(), 1);
        if trashradar_domain::path_key::CASE_INSENSITIVE {
            assert_eq!(units[0].category, CategoryId::SparseFolders);
            assert_eq!(units[0].size.0, 3);
        } else {
            assert_eq!(units[0].category, CategoryId::EmptyFolders);
            assert_eq!(units[0].size.0, 0);
        }
    }

    #[test]
    fn folder_with_only_junk_files_counts_as_empty() {
        // Тека лише з desktop.ini/Thumbs.db → «Порожня папка» (реальний Windows).
        let dir_paths = dirs(&[r"C:\Root", r"C:\Root\JunkOnly"]);
        let file_list = files(&[
            (r"C:\Root\real.txt", 100), // Root не порожній
            (r"C:\Root\JunkOnly\desktop.ini", 282),
            (r"C:\Root\JunkOnly\Thumbs.db", 4096),
        ]);
        let units = detect_folder_units(&dir_paths, &file_list, cfg(3));
        let empty: Vec<_> = units
            .iter()
            .filter(|u| u.category == CategoryId::EmptyFolders)
            .collect();
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].path, p(r"C:\Root\JunkOnly"));
    }

    #[test]
    fn deep_path_reports_topmost_too_deep_folder() {
        // Гілка глибиною 5; поріг 3 → topmost-deep = папка на глибині 4
        // (батько на глибині 3 = поріг, не «занадто глибокий»).
        let dir_paths = dirs(&[
            r"C:\a",         // depth 1
            r"C:\a\b",       // 2
            r"C:\a\b\c",     // 3
            r"C:\a\b\c\d",   // 4  ← topmost deep (parent depth 3 == поріг)
            r"C:\a\b\c\d\e", // 5
        ]);
        // Файли в найглибших папках, щоб вони не були empty/sparse.
        let file_list: Vec<(String, u64)> = (0..10)
            .map(|i| (p(&format!(r"C:\a\b\c\d\e\f{i}.dat")), 100))
            .collect();
        let units = detect_folder_units(&dir_paths, &file_list, cfg_deep(3, 3));
        let deep: Vec<_> = units
            .iter()
            .filter(|u| u.category == CategoryId::DeepPaths)
            .collect();
        assert_eq!(deep.len(), 1, "лише topmost занадто-глибока");
        assert_eq!(deep[0].path, p(r"C:\a\b\c\d"));
        assert_eq!(deep[0].unit, CandidateUnit::Folder);
    }

    #[test]
    fn deep_and_sparse_are_mutually_exclusive_per_folder() {
        // Глибока папка з ≤3 файлів → класифікується Sparse (пріоритет), НЕ Deep.
        let dir_paths = dirs(&[
            r"C:\a",
            r"C:\a\b",
            r"C:\a\b\c",
            r"C:\a\b\c\d",
            r"C:\a\b\c\d\e",
        ]);
        let file_list = files(&[(r"C:\a\b\c\d\e\only.txt", 5)]);
        let units = detect_folder_units(&dir_paths, &file_list, cfg_deep(3, 3));
        // Уся гілка має 1 файл рекурсивно → topmost sparse = C:\a; жодної Deep.
        assert!(units.iter().all(|u| u.category != CategoryId::DeepPaths));
        let sparse: Vec<_> = units
            .iter()
            .filter(|u| u.category == CategoryId::SparseFolders)
            .collect();
        assert_eq!(sparse.len(), 1);
        assert_eq!(sparse[0].path, p(r"C:\a"));
    }

    #[test]
    fn empty_and_sparse_ids_are_disjoint_and_stable() {
        let dir_paths = dirs(&[r"C:\e", r"C:\s"]);
        let file_list = files(&[(r"C:\s\one.txt", 1)]);
        let a = detect_folder_units(&dir_paths, &file_list, cfg(3));
        let b = detect_folder_units(&dir_paths, &file_list, cfg(3));
        // Стабільність між прогонами.
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 2);
        let ids_a: Vec<u64> = a.iter().map(|r| r.candidate_id.0).collect();
        let ids_b: Vec<u64> = b.iter().map(|r| r.candidate_id.0).collect();
        assert_eq!(ids_a, ids_b);
        // Порожня й майже порожня не колізіонують по id.
        assert_ne!(ids_a[0], ids_a[1]);
    }
}
