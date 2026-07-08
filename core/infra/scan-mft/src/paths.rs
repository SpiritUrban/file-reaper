//! Побудова повних шляхів із записів MFT (T-022).
//!
//! Парсер [`crate::record`] віддає кожен запис із `file_ref` і `parent_ref`
//! (номери записів MFT), але без повного шляху. Тут ланцюжок батьків
//! розкручується до кореня тому (запис MFT №5) і збирається у повний шлях
//! на кшталт `C:\Users\Ada\holiday.mp4`.
//!
//! Hard links не роздувають розмір: один фізичний файл — це один запис MFT,
//! отже один [`ScanEntry`] (парсер обирає єдине ім'я на запис). Резолвер
//! будує рівно один шлях на запис, тож агрегація розмірів рахує кожен файл
//! один раз. Чиста логіка над зібраними записами — тестується без доступу до FS.

use std::cell::RefCell;
use std::collections::HashMap;

use trashradar_domain::scan::ScanEntry;

/// Номер запису MFT кореневої директорії тому.
const ROOT_RECORD: u64 = 5;
/// Стеля глибини розкрутки — захист від циклів у пошкодженій MFT.
/// Реальна вкладеність директорій NTFS на порядки менша.
const MAX_DEPTH: usize = 1024;

/// Відновлює повні шляхи записів одного тому за їхніми `parent_ref`.
pub struct PathResolver {
    drive: char,
    /// Номер запису директорії → (батько, ім'я). Лише директорії, бо тільки
    /// вони бувають батьками у ланцюжку шляху.
    dirs: HashMap<u64, (u64, String)>,
    /// Мемоізація повних шляхів директорій.
    cache: RefCell<HashMap<u64, String>>,
}

impl PathResolver {
    /// Будує резолвер із зібраних записів тому `drive`.
    pub fn from_entries(drive: char, entries: &[ScanEntry]) -> Self {
        let mut dirs = HashMap::new();
        for e in entries {
            if e.is_directory {
                dirs.insert(e.file_ref, (e.parent_ref, e.name.clone()));
            }
        }
        Self {
            drive,
            dirs,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Повний шлях директорії за її номером запису MFT.
    /// `None`, якщо ланцюжок розривається (відсутній батько) або зациклений.
    ///
    /// Розкрутка ітеративна (не рекурсивна) — глибокі шляхи не переповнюють
    /// стек; проміжні предки кешуються дорогою вниз.
    pub fn directory_path(&self, record: u64) -> Option<String> {
        if record == ROOT_RECORD {
            return Some(self.drive_root());
        }
        if let Some(cached) = self.cache.borrow().get(&record) {
            return Some(cached.clone());
        }

        // Йдемо вгору до кореня або вже закешованого предка, збираючи ланцюжок.
        let mut chain: Vec<(u64, String)> = Vec::new();
        let mut cur = record;
        let base = loop {
            if cur == ROOT_RECORD {
                break self.drive_root();
            }
            if let Some(cached) = self.cache.borrow().get(&cur) {
                break cached.clone();
            }
            if chain.len() > MAX_DEPTH {
                return None; // цикл у пошкодженій MFT
            }
            let (parent, name) = self.dirs.get(&cur)?.clone();
            chain.push((cur, name));
            cur = parent;
        };

        // Будуємо вниз від бази: від найближчого до кореня предка до `record`,
        // кешуючи кожну проміжну директорію.
        let mut path = base;
        for (rec, name) in chain.iter().rev() {
            path.push('\\');
            path.push_str(name);
            self.cache.borrow_mut().insert(*rec, path.clone());
        }
        Some(path)
    }

    fn drive_root(&self) -> String {
        format!("{}:", self.drive.to_ascii_uppercase())
    }

    /// Повний шлях запису (файла або директорії).
    /// `None`, якщо батьківський ланцюжок не розкручується до кореня.
    pub fn full_path(&self, entry: &ScanEntry) -> Option<String> {
        if entry.file_ref == ROOT_RECORD {
            return Some(format!("{}:\\", self.drive.to_ascii_uppercase()));
        }
        let parent_path = self.directory_path(entry.parent_ref)?;
        Some(format!("{parent_path}\\{}", entry.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trashradar_domain::candidate::{ByteSize, FileAttributes};

    fn entry(file_ref: u64, parent_ref: u64, name: &str, is_dir: bool, size: u64) -> ScanEntry {
        ScanEntry {
            file_ref,
            parent_ref,
            name: name.to_string(),
            size: ByteSize(size),
            created_at: None,
            modified_at: None,
            accessed_at: None,
            is_directory: is_dir,
            attributes: FileAttributes::default(),
        }
    }

    /// Дерево: C:\ (5) → Users (100) → Ada (101) → holiday.mp4 (200).
    fn sample_tree() -> Vec<ScanEntry> {
        vec![
            entry(ROOT_RECORD, ROOT_RECORD, ".", true, 0),
            entry(100, ROOT_RECORD, "Users", true, 0),
            entry(101, 100, "Ada", true, 0),
            entry(200, 101, "holiday.mp4", false, 4096),
            entry(201, ROOT_RECORD, "readme.txt", false, 12),
        ]
    }

    #[test]
    fn builds_full_path_through_parent_chain() {
        let entries = sample_tree();
        let r = PathResolver::from_entries('c', &entries);
        assert_eq!(
            r.full_path(&entries[3]).unwrap(),
            "C:\\Users\\Ada\\holiday.mp4"
        );
    }

    #[test]
    fn file_directly_in_root() {
        let entries = sample_tree();
        let r = PathResolver::from_entries('C', &entries);
        assert_eq!(r.full_path(&entries[4]).unwrap(), "C:\\readme.txt");
    }

    #[test]
    fn root_entry_resolves_to_drive_root() {
        let entries = sample_tree();
        let r = PathResolver::from_entries('C', &entries);
        assert_eq!(r.full_path(&entries[0]).unwrap(), "C:\\");
    }

    #[test]
    fn directory_path_of_intermediate_dir() {
        let entries = sample_tree();
        let r = PathResolver::from_entries('C', &entries);
        assert_eq!(r.directory_path(101).unwrap(), "C:\\Users\\Ada");
    }

    #[test]
    fn missing_parent_yields_none() {
        // Файл посилається на батька 999, якого немає серед директорій.
        let entries = vec![entry(200, 999, "orphan.dat", false, 1)];
        let r = PathResolver::from_entries('C', &entries);
        assert!(r.full_path(&entries[0]).is_none());
    }

    #[test]
    fn cyclic_parent_chain_yields_none() {
        // 300 ↔ 301 утворюють цикл, не досягаючи кореня.
        let entries = vec![
            entry(300, 301, "A", true, 0),
            entry(301, 300, "B", true, 0),
            entry(400, 300, "f.txt", false, 5),
        ];
        let r = PathResolver::from_entries('C', &entries);
        assert!(r.full_path(&entries[2]).is_none());
    }

    #[test]
    fn memoization_is_consistent_across_calls() {
        let entries = sample_tree();
        let r = PathResolver::from_entries('C', &entries);
        // Другий виклик іде через кеш — результат мусить збігатися.
        let first = r.directory_path(101).unwrap();
        let second = r.directory_path(101).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            r.full_path(&entries[3]).unwrap(),
            "C:\\Users\\Ada\\holiday.mp4"
        );
    }

    #[test]
    fn hard_links_do_not_duplicate_size() {
        // Парсер віддає один запис на фізичний файл, навіть якщо той має
        // кілька імен (hard links). Тож сумарний розмір рахує кожен запис раз.
        let entries = sample_tree();
        let r = PathResolver::from_entries('C', &entries);

        let mut total = 0u64;
        let mut paths = Vec::new();
        for e in &entries {
            if let Some(p) = r.full_path(e) {
                if !e.is_directory {
                    total += e.size.0;
                }
                paths.push(p);
            }
        }
        // Рівно один шлях на запис (бієкція), розмір файлів порахований раз.
        assert_eq!(paths.len(), entries.len());
        assert_eq!(total, 4096 + 12);
    }
}
