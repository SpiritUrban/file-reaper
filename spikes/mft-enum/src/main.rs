//! Спайк T-020 — вимір «часу до повного переліку файлів» тому NTFS.
//!
//! Мета (docs/tasks.md T-020): підтвердити go/no-go для драйвера D1
//! («цифра за секунди, не хвилини») — тобто що метадані *всіх* файлів тому
//! читаються за секунди навіть на мільйонах записів. Це **спайк**, не
//! продакшн-парсер: промислова реалізація `ScanSource` — задача T-021.
//!
//! Підхід: перелічуємо MFT через `FSCTL_ENUM_USN_DATA` — стандартний швидкий
//! шлях (той самий, що використовують Everything/WizTree). Один volume-handle,
//! послідовне читання записів MFT великими батчами. Меряємо: кількість
//! записів, час, пропускну здатність (записів/с), обсяг прочитаного та пікову
//! робочу пам'ять процесу.
//!
//! ВАЖЛИВО: відкриття `\\.\<літера>:` для читання потребує прав адміністратора.
//! Запуск: `mft-enum C` (з elevated-консолі).
//!
//! Реалізовано чистим FFI без зовнішніх крейтів, щоб спайк був самодостатнім
//! і не тягнув важких залежностей у дерево до проєктування T-021.

#![cfg(windows)]

use std::ffi::c_void;
use std::time::Instant;

// --- WinAPI FFI ---------------------------------------------------------------

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
    fn GetCurrentProcess() -> *mut c_void;
}

#[link(name = "psapi")]
extern "system" {
    fn GetProcessMemoryInfo(
        process: *mut c_void,
        counters: *mut ProcessMemoryCounters,
        cb: u32,
    ) -> i32;
}

const GENERIC_READ: u32 = 0x8000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;
const FSCTL_ENUM_USN_DATA: u32 = 0x0009_00B3;
const ERROR_HANDLE_EOF: u32 = 38;
const ERROR_ACCESS_DENIED: u32 = 5;

#[repr(C)]
struct MftEnumDataV0 {
    start_file_reference_number: u64,
    low_usn: i64,
    high_usn: i64,
}

/// Заголовок USN_RECORD_V2 (за ним у буфері йде UTF-16 ім'я).
#[repr(C)]
struct UsnRecordV2Header {
    record_length: u32,
    major_version: u16,
    minor_version: u16,
    file_reference_number: u64,
    parent_file_reference_number: u64,
    usn: i64,
    timestamp: i64,
    reason: u32,
    source_info: u32,
    security_id: u32,
    file_attributes: u32,
    file_name_length: u16,
    file_name_offset: u16,
}

#[repr(C)]
#[derive(Default)]
struct ProcessMemoryCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
}

// --- Спайк --------------------------------------------------------------------

/// Розмір компактного запису in-memory індексу (T-015) — для проєкції пам'яті.
const COMPACT_RECORD_BYTES: u64 = 48;
/// Розмір батча читання MFT. Більший буфер = менше syscall-ів.
const READ_BUFFER_BYTES: usize = 1 << 20; // 1 MiB

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct Stats {
    records: u64,
    unsupported_version: u64,
    dirs: u64,
    name_utf16_units: u64,
    bytes_read: u64,
    syscalls: u64,
}

fn enumerate(drive: char) -> Result<(Stats, f64), String> {
    // Win32-атрибут FILE_ATTRIBUTE_DIRECTORY.
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

    let path = wide(&format!("\\\\.\\{drive}:"));
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
    if handle as isize == -1 {
        let err = unsafe { GetLastError() };
        if err == ERROR_ACCESS_DENIED {
            return Err(
                "ACCESS_DENIED (err 5): відкриття тому потребує прав адміністратора. \
                 Запустіть спайк з elevated-консолі."
                    .to_string(),
            );
        }
        return Err(format!(
            "CreateFileW(\\\\.\\{drive}:) не вдалося, GetLastError={err}"
        ));
    }

    let mut stats = Stats {
        records: 0,
        unsupported_version: 0,
        dirs: 0,
        name_utf16_units: 0,
        bytes_read: 0,
        syscalls: 0,
    };
    let mut buffer = vec![0u8; READ_BUFFER_BYTES];
    let mut enum_data = MftEnumDataV0 {
        start_file_reference_number: 0,
        low_usn: 0,
        high_usn: i64::MAX,
    };

    let started = Instant::now();
    loop {
        let mut bytes_returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_ENUM_USN_DATA,
                &enum_data as *const _ as *const c_void,
                std::mem::size_of::<MftEnumDataV0>() as u32,
                buffer.as_mut_ptr() as *mut c_void,
                buffer.len() as u32,
                &mut bytes_returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_HANDLE_EOF {
                break; // усі записи перелічено
            }
            unsafe { CloseHandle(handle) };
            return Err(format!(
                "DeviceIoControl(FSCTL_ENUM_USN_DATA) помилка, GetLastError={err}"
            ));
        }
        stats.syscalls += 1;
        stats.bytes_read += bytes_returned as u64;
        if bytes_returned <= 8 {
            break; // лише службовий next-FRN, записів більше немає
        }

        // Перші 8 байтів — наступний StartFileReferenceNumber.
        let next_frn = u64::from_le_bytes(buffer[0..8].try_into().unwrap());
        enum_data.start_file_reference_number = next_frn;

        // Далі — послідовність USN_RECORD_V2.
        let mut offset = 8usize;
        let limit = bytes_returned as usize;
        while offset + std::mem::size_of::<UsnRecordV2Header>() <= limit {
            let header = unsafe { &*(buffer.as_ptr().add(offset) as *const UsnRecordV2Header) };
            let record_length = header.record_length as usize;
            if record_length == 0 || offset + record_length > limit {
                break;
            }
            if header.major_version == 2 {
                stats.records += 1;
                stats.name_utf16_units += (header.file_name_length / 2) as u64;
                if header.file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                    stats.dirs += 1;
                }
            } else {
                // V3/V4 (128-бітні FRN) — на стандартному NTFS не очікуються.
                stats.unsupported_version += 1;
            }
            offset += record_length;
        }
    }
    let elapsed = started.elapsed().as_secs_f64();

    unsafe { CloseHandle(handle) };
    Ok((stats, elapsed))
}

fn peak_working_set_bytes() -> u64 {
    let mut pmc = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        ..Default::default()
    };
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc,
            std::mem::size_of::<ProcessMemoryCounters>() as u32,
        )
    };
    if ok == 0 {
        0
    } else {
        pmc.peak_working_set_size as u64
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

fn main() {
    let drive = std::env::args()
        .nth(1)
        .and_then(|s| s.chars().next())
        .unwrap_or('C')
        .to_ascii_uppercase();

    println!("== Спайк T-020: перелік MFT тому {drive}: ==");
    match enumerate(drive) {
        Ok((stats, elapsed)) => {
            let files = stats.records - stats.dirs;
            let rate = if elapsed > 0.0 {
                stats.records as f64 / elapsed
            } else {
                f64::INFINITY
            };
            let projected_index = stats.records * COMPACT_RECORD_BYTES;
            let projected_names = stats.name_utf16_units; // ~1 байт/символ у UTF-8 для ASCII-імен

            println!("Записів MFT (файли+теки): {}", stats.records);
            println!("  з них теки:             {}", stats.dirs);
            println!("  з них файли:            {}", files);
            if stats.unsupported_version > 0 {
                println!(
                    "  записів V3/V4 (пропущено підрахунок імен): {}",
                    stats.unsupported_version
                );
            }
            println!("Час перелічення:          {:.3} с", elapsed);
            println!("Пропускна здатність:      {:.0} записів/с", rate);
            println!(
                "Прочитано з тому:         {:.1} МБ ({} syscall-ів)",
                mib(stats.bytes_read),
                stats.syscalls
            );
            println!(
                "Пікова робоча пам'ять:    {:.1} МБ (лише буфер+лічильники)",
                mib(peak_working_set_bytes())
            );
            println!(
                "Проєкція in-memory індексу (T-015, 48 Б/запис): {:.1} МБ + ~{:.1} МБ інтерновані імена",
                mib(projected_index),
                mib(projected_names)
            );

            // Go/no-go: D1 вимагає перших цифр < 10 с (architecture.md §15).
            // Оцінюємо, чи вкладеться еталонний том у ціль за поточною швидкістю.
            let ten_sec_capacity = rate * 10.0;
            println!("\n-- Go/No-Go (драйвер D1) --");
            println!(
                "За цією швидкістю за 10 с перелічується ~{:.0} записів.",
                ten_sec_capacity
            );
            if rate >= 150_000.0 {
                println!(
                    "ВЕРДИКТ: GO. Пропускна здатність достатня — том на ~1.5 млн файлів \
                     перелічується за {:.1} с (ціль < 10 с).",
                    1_500_000.0 / rate
                );
            } else {
                println!(
                    "ВЕРДИКТ: NO-GO/переглянути. Швидкість нижча за орієнтир 150k записів/с — \
                     потрібен сирий парсинг $MFT замість FSCTL_ENUM_USN_DATA."
                );
            }
            println!(
                "\nПримітка для T-021: FSCTL_ENUM_USN_DATA дає ім'я/батька/атрибути, але НЕ розмір файла. \
                 Розмір для D1 потребує або сирого парсингу $MFT ($STANDARD_INFORMATION/$DATA), \
                 або окремого запиту — рішення за T-021."
            );
        }
        Err(e) => {
            eprintln!("ПОМИЛКА: {e}");
            std::process::exit(1);
        }
    }
}
