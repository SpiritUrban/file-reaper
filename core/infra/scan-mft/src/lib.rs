//! Адаптер `ScanSource`: прямий парсинг MFT тому NTFS (швидкий шлях).
//!
//! Розкладка (docs/tasks.md, E3/F3.1):
//! - [`record`] — чистий парсер байтів запису MFT (T-021), тестований у CI
//!   без доступу до тому;
//! - [`paths`] — побудова повних шляхів із `parent_ref`-ланцюжка (T-022);
//! - [`pipeline`] — батчева видача `FileRecord` у індекс зі зворотним тиском (T-024);
//! - [`volume`] — читач `$MFT` через WinAPI (T-021), потребує адмін-прав;
//!   перевіряється ігнорованим інтеграційним тестом на реальному томі.
//!
//! Обробка помилок — T-025.

pub mod paths;
pub mod pipeline;
pub mod record;

#[cfg(windows)]
pub mod volume;

#[cfg(windows)]
pub use volume::{
    enumerate, enumerate_with, enumerate_with_cancel, enumerate_with_cancel_progress, ScanStats,
};

/// Джерело скану на базі прямого читання MFT (NTFS).
#[cfg(windows)]
#[derive(Debug, Default)]
pub struct MftScanner;

#[cfg(windows)]
impl trashradar_app::ports::ScanSource for MftScanner {
    fn scan_volume(
        &self,
        volume: char,
    ) -> Result<Vec<trashradar_domain::scan::ScanEntry>, trashradar_domain::error::CoreError> {
        volume::enumerate(volume)
    }
}

// --- Інтеграційний тест DoD T-021 (перелік = контроль, розбіжність 0) --------
//
// Перевіряє на реальному томі, що прямий парсинг $MFT перелічує ту саму
// популяцію файлів, що й незалежний контроль — OS-перелік FSCTL_ENUM_USN_DATA.
// Жорстка умова DoD: жоден файл контролю не пропущений сирим парсером
// (звірка за номером запису MFT). Сирий парсер може бути надмножиною
// (системні метафайли 0–15) — це не розбіжність.
//
// ВАЖЛИВО: запускати на *тихому* томі (без активного запису). На живому
// системному диску два послідовні проходи бачать різні знімки ФС: файли,
// створені/видалені між проходами, дають хибну «розбіжність». Так само різні
// імена одного запису — це гард-лінки (один MFT-запис під кількома іменами,
// напр. WinSxS): парсер і USN обирають різні валідні імена того самого файла,
// тож розбіжність імен звітується інформаційно, а не валить тест.
// Верифіковано: тихий том F: — 0 пропущених, 0 різних імен.
//
// Потребує адмін-прав; запускається вручну на елевованій консолі:
//   set TR_MFT_TEST_DRIVE=F
//   cargo test -p trashradar-scan-mft --release raw_mft_matches_usn_enumeration -- --ignored --nocapture
#[cfg(all(windows, test))]
mod integration {
    use std::collections::HashMap;
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            lp_file_name: *const u16,
            dw_desired_access: u32,
            dw_share_mode: u32,
            lp_security_attributes: *mut c_void,
            dw_creation_disposition: u32,
            dw_flags_and_attributes: u32,
            h_template_file: *mut c_void,
        ) -> *mut c_void;
        fn DeviceIoControl(
            h_device: *mut c_void,
            dw_io_control_code: u32,
            lp_in_buffer: *const c_void,
            n_in_buffer_size: u32,
            lp_out_buffer: *mut c_void,
            n_out_buffer_size: u32,
            lp_bytes_returned: *mut u32,
            lp_overlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(h_object: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }

    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;
    const FSCTL_ENUM_USN_DATA: u32 = 0x0009_00B3;
    const ERROR_HANDLE_EOF: u32 = 38;
    const MFT_REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

    #[repr(C)]
    struct MftEnumDataV0 {
        start_file_reference_number: u64,
        low_usn: i64,
        high_usn: i64,
    }

    /// Незалежний контроль: (номер запису MFT → ім'я) з FSCTL_ENUM_USN_DATA.
    fn usn_enumeration(drive: char) -> HashMap<u64, String> {
        let path: Vec<u16> = format!("\\\\.\\{}:", drive.to_ascii_uppercase())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        assert!(
            handle as isize != -1,
            "USN control: відкриття тому не вдалося (потрібні адмін-права)"
        );

        let mut map = HashMap::new();
        let mut buffer = vec![0u8; 1 << 20];
        let mut enum_data = MftEnumDataV0 {
            start_file_reference_number: 0,
            low_usn: 0,
            high_usn: i64::MAX,
        };
        loop {
            let mut returned: u32 = 0;
            let ok = unsafe {
                DeviceIoControl(
                    handle,
                    FSCTL_ENUM_USN_DATA,
                    &enum_data as *const _ as *const c_void,
                    std::mem::size_of::<MftEnumDataV0>() as u32,
                    buffer.as_mut_ptr() as *mut c_void,
                    buffer.len() as u32,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let err = unsafe { GetLastError() };
                assert_eq!(
                    err, ERROR_HANDLE_EOF,
                    "USN control: DeviceIoControl помилка {err}"
                );
                break;
            }
            if returned <= 8 {
                break;
            }
            enum_data.start_file_reference_number =
                u64::from_le_bytes(buffer[0..8].try_into().unwrap());

            let mut off = 8usize;
            let limit = returned as usize;
            while off + 60 <= limit {
                let record_length =
                    u32::from_le_bytes(buffer[off..off + 4].try_into().unwrap()) as usize;
                let major = u16::from_le_bytes(buffer[off + 4..off + 6].try_into().unwrap());
                if record_length == 0 || off + record_length > limit {
                    break;
                }
                if major == 2 {
                    let frn = u64::from_le_bytes(buffer[off + 8..off + 16].try_into().unwrap())
                        & MFT_REF_MASK;
                    let name_len =
                        u16::from_le_bytes(buffer[off + 56..off + 58].try_into().unwrap()) as usize;
                    let name_off =
                        u16::from_le_bytes(buffer[off + 58..off + 60].try_into().unwrap()) as usize;
                    let s = off + name_off;
                    let e = s + name_len;
                    if e <= limit {
                        let units: Vec<u16> = buffer[s..e]
                            .chunks_exact(2)
                            .map(|c| u16::from_le_bytes([c[0], c[1]]))
                            .collect();
                        map.insert(frn, String::from_utf16_lossy(&units));
                    }
                }
                off += record_length;
            }
        }
        unsafe { CloseHandle(handle) };
        map
    }

    #[test]
    #[ignore = "DoD T-021: потребує адмін-прав і реального тому; TR_MFT_TEST_DRIVE"]
    fn raw_mft_matches_usn_enumeration() {
        let drive = std::env::var("TR_MFT_TEST_DRIVE")
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or('C');

        // Сирий парсинг $MFT: номер запису → ім'я.
        let mut raw: HashMap<u64, String> = HashMap::new();
        let stats = super::enumerate_with(drive, |e| {
            raw.insert(e.file_ref, e.name);
        })
        .expect("сирий парсинг $MFT");

        // Незалежний контроль.
        let control = usn_enumeration(drive);

        let mut missing = 0u64;
        let mut differing_name = 0u64; // гард-лінки / інший простір імен — не помилка
        for (frn, name) in &control {
            match raw.get(frn) {
                None => missing += 1,
                Some(raw_name) if raw_name != name => differing_name += 1,
                _ => {}
            }
        }

        println!(
            "Том {drive}: сирий $MFT — {} записів ({} тек); USN-контроль — {} записів; \
             відсутніх у сирому: {}; різних імен (гард-лінки): {}",
            stats.entries,
            stats.directories,
            control.len(),
            missing,
            differing_name
        );

        // DoD: на тихому томі жоден файл контролю не пропущений сирим парсером.
        assert_eq!(
            missing, 0,
            "сирий парсер пропустив {missing} файлів контролю (том має бути тихим)"
        );
    }

    // DoD T-022: побудований повний шлях кожного запису вказує на реальний
    // об'єкт ФС, а розмір збігається. Звіряємо вибірку зі станом диска
    // (ground truth). Том має бути тихим; запуск як вище, з TR_MFT_TEST_DRIVE.
    #[test]
    #[ignore = "DoD T-022: потребує адмін-прав і реального тому; TR_MFT_TEST_DRIVE"]
    fn resolved_paths_point_to_real_files() {
        use crate::paths::PathResolver;

        let drive = std::env::var("TR_MFT_TEST_DRIVE")
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or('F');

        let mut entries = Vec::new();
        super::enumerate_with(drive, |e| entries.push(e)).expect("сирий парсинг $MFT");
        let resolver = PathResolver::from_entries(drive, &entries);

        // Вибірка файлів (не директорій), щоб не робити мільйони stat-ів.
        let mut sampled = 0u64;
        let mut resolved = 0u64;
        let mut missing_on_disk = 0u64;
        let mut size_mismatch = 0u64;
        for e in entries.iter().filter(|e| !e.is_directory).step_by(200) {
            sampled += 1;
            let Some(path) = resolver.full_path(e) else {
                continue;
            };
            resolved += 1;
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    // Розмір логічного потоку може легально різнитись для
                    // reparse-точок; рахуємо як інфо, не як фейл.
                    if meta.is_file() && meta.len() != e.size.0 {
                        size_mismatch += 1;
                    }
                }
                Err(_) => missing_on_disk += 1,
            }
        }

        let exist_rate = (resolved - missing_on_disk) as f64 / resolved.max(1) as f64;
        println!(
            "Том {drive}: вибірка {sampled} файлів; шлях побудовано {resolved}; \
             немає на диску {missing_on_disk}; частка існуючих {:.4}; \
             розбіжність розміру {size_mismatch}",
            exist_rate
        );

        // Кожен запис отримав повний шлях.
        assert_eq!(resolved, sampled, "не для всіх записів побудовано шлях");
        // Практично всі побудовані шляхи вказують на реальний об'єкт ФС
        // (лишок — файли, зниклі за час скану/недоступні навіть elevated).
        assert!(
            exist_rate > 0.98,
            "лише {:.4} побудованих шляхів існують на диску",
            exist_rate
        );
    }

    // T-023 (інфо): розподіл FileKind по реальному тому та топ нерозпізнаних
    // розширень — доказова база для критерію покриття і виявлення прогалин
    // таблиці. Не асертить відсоток (частка «інше» на диску легально велика
    // через код/систему); друкує зведення. Запуск як вище.
    #[test]
    #[ignore = "T-023 інфо-вимір: потребує адмін-прав і реального тому"]
    fn classifier_coverage_on_real_volume() {
        use std::collections::HashMap;
        use trashradar_domain::candidate::FileKind;

        let drive = std::env::var("TR_MFT_TEST_DRIVE")
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or('F');

        let mut per_kind: HashMap<String, u64> = HashMap::new();
        let mut other_ext: HashMap<String, u64> = HashMap::new();
        let mut files = 0u64;
        super::enumerate_with(drive, |e| {
            if e.is_directory {
                return;
            }
            files += 1;
            let kind = FileKind::from_path(&e.name);
            *per_kind.entry(format!("{kind:?}")).or_default() += 1;
            if kind == FileKind::Other {
                let ext = e
                    .name
                    .rsplit_once('.')
                    .map(|(_, x)| x.to_ascii_lowercase())
                    .filter(|x| !x.is_empty() && x.len() <= 8)
                    .unwrap_or_else(|| "<none>".to_string());
                *other_ext.entry(ext).or_default() += 1;
            }
        })
        .expect("сирий парсинг $MFT");

        let mut kinds: Vec<_> = per_kind.into_iter().collect();
        kinds.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let mut others: Vec<_> = other_ext.into_iter().collect();
        others.sort_by_key(|(_, n)| std::cmp::Reverse(*n));

        println!("Том {drive}: {files} файлів");
        for (kind, n) in &kinds {
            println!(
                "  {kind:<10} {n:>10} ({:.1}%)",
                *n as f64 / files as f64 * 100.0
            );
        }
        println!("Топ-15 нерозпізнаних розширень (→ Other):");
        for (ext, n) in others.iter().take(15) {
            println!("  .{ext:<10} {n:>10}");
        }
    }

    // DoD T-024: сканер віддає батчі по N тис. записів у справжній HotIndex;
    // зворотний тиск не роздуває пам'ять (у льоті — не більше одного батча).
    // Том має бути тихим; запуск як вище.
    #[test]
    #[ignore = "DoD T-024: потребує адмін-прав і реального тому; TR_MFT_TEST_DRIVE"]
    fn scan_fills_index_in_bounded_batches() {
        use std::cell::Cell;
        use trashradar_app::ports::HotIndex;
        use trashradar_index_memory::InMemoryIndex;

        let drive = std::env::var("TR_MFT_TEST_DRIVE")
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or('F');

        const BATCH: usize = 10_000;
        let index = InMemoryIndex::new();
        let max_batch = Cell::new(0usize);
        let batch_count = Cell::new(0u64);

        let stats = crate::pipeline::scan_volume_to_index(drive, BATCH, |batch| {
            max_batch.set(max_batch.get().max(batch.len()));
            batch_count.set(batch_count.get() + 1);
            index.insert_batch(batch)
        })
        .expect("скан у індекс");

        index.finish_indexing();

        println!(
            "Том {drive}: у індекс {} файлів, {} батчів, макс. батч {}; пропущено без шляху {}",
            stats.files_indexed,
            stats.batches,
            max_batch.get(),
            stats.skipped_no_path
        );

        // Індекс наповнено рівно стількома записами, скільки віддано.
        assert_eq!(index.len() as u64, stats.files_indexed);
        assert!(stats.files_indexed > 0, "нічого не проіндексовано");
        // Зворотний тиск: у пам'яті жодного разу не більше одного батча.
        assert!(
            max_batch.get() <= BATCH,
            "батч {} перевищив ліміт {BATCH}",
            max_batch.get()
        );
        // Батчів приблизно files/BATCH (остача — останній неповний батч).
        assert_eq!(
            stats.batches,
            stats.files_indexed.div_ceil(BATCH as u64),
            "кількість батчів не відповідає розміру батча"
        );
    }
}
