//! Спільні WinAPI-обгортки для інфраструктурних крейтів.
//!
//! Єдиний крейт, від якого дозволені горизонтальні залежності
//! всередині `infra/*` (docs/repository.md §10).
//!
//! T-028: тип файлової системи тому, elevation процесу, перелік томів.
//! T-034: детекція + UAC-relaunch (`relaunch_elevated`); сесійна політика
//! відмови — у `trashradar_app::elevation`.

use trashradar_app::ports::ScanEnvironment;
use trashradar_domain::{
    candidate::{ByteSize, FsTimestamp},
    error::CoreError,
    forecast::VolumeUsage,
    quarantine::FileIdentity,
};

/// Адаптер [`ScanEnvironment`] на WinAPI (T-028).
#[derive(Debug, Default, Clone, Copy)]
pub struct WinScanEnvironment;

impl ScanEnvironment for WinScanEnvironment {
    fn is_ntfs(&self, volume: char) -> Result<bool, CoreError> {
        Ok(volume_file_system(volume)?
            .map(|fs| fs.eq_ignore_ascii_case("NTFS"))
            .unwrap_or(false))
    }

    fn is_elevated(&self) -> bool {
        is_process_elevated()
    }

    fn file_system_name(&self, volume: char) -> Result<Option<String>, CoreError> {
        volume_file_system(volume)
    }

    fn list_scan_volumes(&self) -> Vec<char> {
        list_drive_letters()
    }
}

/// Чи процес має elevated token (адмін-права).
pub fn is_process_elevated() -> bool {
    #[cfg(windows)]
    {
        windows::is_process_elevated()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Ім'я файлової системи тому (`Some("NTFS")`, …) або `None`, якщо том
/// недоступний / не готовий.
pub fn volume_file_system(volume: char) -> Result<Option<String>, CoreError> {
    if !volume.is_ascii_alphabetic() {
        return Err(CoreError::invalid_argument(format!(
            "Некоректна літера тому «{volume}»."
        )));
    }
    #[cfg(windows)]
    {
        windows::volume_file_system(volume)
    }
    #[cfg(not(windows))]
    {
        let _ = volume;
        Ok(None)
    }
}

/// Прочитати живі size+mtime у тому самому FILETIME-форматі, що й індекс (T-086).
pub fn read_file_identity(path: &std::path::Path) -> Result<FileIdentity, CoreError> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        CoreError::io(format!(
            "Не вдалося прочитати метадані {}: {error}",
            path.display()
        ))
    })?;
    #[cfg(windows)]
    let modified_at = {
        use std::os::windows::fs::MetadataExt;
        let ticks = metadata.last_write_time();
        if ticks == 0 {
            None
        } else {
            i64::try_from(ticks).ok().map(FsTimestamp)
        }
    };
    #[cfg(not(windows))]
    let modified_at = metadata.modified().ok().and_then(system_time_to_filetime);

    Ok(FileIdentity {
        size: ByteSize(metadata.len()),
        modified_at,
    })
}

#[cfg(not(windows))]
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

#[cfg(not(windows))]
fn system_time_to_filetime(time: std::time::SystemTime) -> Option<FsTimestamp> {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => {
            let ticks = duration
                .as_secs()
                .checked_mul(10_000_000)?
                .checked_add((duration.subsec_nanos() / 100) as u64)?
                .checked_add(FILETIME_UNIX_EPOCH)?;
            i64::try_from(ticks).ok().map(FsTimestamp)
        }
        Err(error) => {
            let duration = error.duration();
            let delta = duration
                .as_secs()
                .checked_mul(10_000_000)?
                .checked_add((duration.subsec_nanos() / 100) as u64)?;
            (delta <= FILETIME_UNIX_EPOCH)
                .then(|| FsTimestamp((FILETIME_UNIX_EPOCH - delta) as i64))
        }
    }
}
/// Результат no-replace move (T-080).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoReplaceMove {
    Moved,
    /// Ціль уже існує — джерело НЕ переміщено, ціль НЕ перезаписано.
    DestinationOccupied,
}

/// Атомарний move, який ніколи не перезаписує наявну ціль (T-080 restore).
///
/// На відміну від `std::fs::rename` (на Windows = `MOVEFILE_REPLACE_EXISTING`,
/// тобто мовчки затирає ціль), зайнятий шлях повертає `DestinationOccupied` —
/// викликач підбирає інше ім'я. Захист D4: відновлення не знищує чужий файл.
pub fn move_file_no_replace(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<NoReplaceMove, CoreError> {
    #[cfg(windows)]
    {
        windows::move_file_no_replace(source, destination)
    }
    #[cfg(not(windows))]
    {
        if destination.exists() {
            return Ok(NoReplaceMove::DestinationOccupied);
        }
        std::fs::rename(source, destination)
            .map(|_| NoReplaceMove::Moved)
            .map_err(|error| {
                CoreError::io(format!(
                    "Не вдалося перемістити {} → {}: {error}",
                    source.display(),
                    destination.display()
                ))
            })
    }
}

/// Додати Windows-атрибут `HIDDEN`, зберігши решту атрибутів (T-077).
pub fn set_hidden(path: &std::path::Path) -> Result<(), CoreError> {
    #[cfg(windows)]
    {
        windows::set_hidden(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(())
    }
}

/// Перевірити Windows-атрибут `HIDDEN` (діагностика й тести T-077).
pub fn is_hidden(path: &std::path::Path) -> Result<bool, CoreError> {
    #[cfg(windows)]
    {
        windows::is_hidden(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(false)
    }
}
/// Літери томів, придатних до скану (fixed + removable, готові).
pub fn list_drive_letters() -> Vec<char> {
    #[cfg(windows)]
    {
        windows::list_drive_letters()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Зручність: NTFS-перевірка без трейта.
pub fn is_ntfs_volume(volume: char) -> Result<bool, CoreError> {
    WinScanEnvironment.is_ntfs(volume)
}

/// Знімок пам'яті процесу (T-157): поточний і піковий working set (RSS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcessMemory {
    /// Поточний резидентний обсяг (working set) процесу, байти.
    pub working_set_bytes: u64,
    /// Піковий working set за час життя процесу, байти.
    pub peak_working_set_bytes: u64,
}

/// Виміряти пам'ять поточного процесу (working set = RSS) — джерело правди
/// для профілювання §15 «RAM Core < 300 МБ» (T-157). На не-Windows повертає
/// нулі: продукт MVP Windows-only, а точний RSS — платформозалежний.
pub fn process_memory() -> ProcessMemory {
    #[cfg(windows)]
    {
        windows::process_memory()
    }
    #[cfg(not(windows))]
    {
        ProcessMemory::default()
    }
}

/// Живе заповнення тому (capacity/free) для прогнозу T-056 і Sidebar T-106.
///
/// `Ok(None)` — том не готовий (порожній привід, немає носія): не помилка,
/// UI просто не показує смужку для такого тому.
pub fn volume_usage(volume: char) -> Result<Option<VolumeUsage>, CoreError> {
    if !volume.is_ascii_alphabetic() {
        return Err(CoreError::invalid_argument(format!(
            "Некоректна літера тому «{volume}»."
        )));
    }
    #[cfg(windows)]
    {
        windows::volume_usage(volume)
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

/// Запит UAC elevation: перезапуск поточного exe з дієсловом `runas` (T-034).
///
/// - `Ok(ElevationRelaunch::Started)` — новий elevated-процес запущено;
///   **викликач має завершити поточний процес** (інакше два вікна).
/// - `Ok(ElevationRelaunch::AlreadyElevated)` — уже admin, relaunch не потрібен.
/// - `Err` — користувач скасував UAC (`cancelled`) або збій запуску (`io`).
///
/// Політика «не питати знову в сесії» — **не** тут: shell викликає
/// [`trashradar_app::elevation::ElevationSession::decline`] окремо.
pub fn relaunch_elevated() -> Result<ElevationRelaunch, CoreError> {
    if is_process_elevated() {
        return Ok(ElevationRelaunch::AlreadyElevated);
    }
    #[cfg(windows)]
    {
        windows::relaunch_elevated()
    }
    #[cfg(not(windows))]
    {
        Err(CoreError::new(
            trashradar_domain::error::ErrorCode::NotImplemented,
            "Elevation relaunch доступний лише на Windows.",
        ))
    }
}

/// Результат спроби UAC-relaunch (T-034).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevationRelaunch {
    /// Новий процес з admin-правами запущено.
    Started,
    /// Поточний процес уже elevated.
    AlreadyElevated,
}

/// «Показати у провіднику» (T-125): відкрити Explorer із виділеним файлом.
/// Fire-and-forget за задумом виклику (spawn, не чекаємо завершення Explorer);
/// `Err` — лише якщо сам процес не вдалося запустити (шлях не існує чи
/// видалений — Explorer сам покаже свою помилку, ми цього не перевіряємо).
pub fn reveal_in_explorer(path: &str) -> Result<(), CoreError> {
    if path.is_empty() {
        return Err(CoreError::invalid_argument("Порожній шлях до файла."));
    }
    #[cfg(windows)]
    {
        windows::reveal_in_explorer(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(CoreError::new(
            trashradar_domain::error::ErrorCode::NotImplemented,
            "Показ у провіднику підтримується лише на Windows.",
        ))
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetVolumeInformationW(
            root_path_name: *const u16,
            volume_name_buffer: *mut u16,
            volume_name_size: u32,
            volume_serial_number: *mut u32,
            maximum_component_length: *mut u32,
            file_system_flags: *mut u32,
            file_system_name_buffer: *mut u16,
            file_system_name_size: u32,
        ) -> i32;
        fn GetLogicalDrives() -> u32;
        fn GetDriveTypeW(root_path_name: *const u16) -> u32;
        fn GetDiskFreeSpaceExW(
            directory_name: *const u16,
            free_bytes_available_to_caller: *mut u64,
            total_number_of_bytes: *mut u64,
            total_number_of_free_bytes: *mut u64,
        ) -> i32;
        fn GetLastError() -> u32;
        fn GetFileAttributesW(file_name: *const u16) -> u32;
        fn SetFileAttributesW(file_name: *const u16, file_attributes: u32) -> i32;
        fn MoveFileW(existing_file_name: *const u16, new_file_name: *const u16) -> i32;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(
            process_handle: *mut c_void,
            desired_access: u32,
            token_handle: *mut *mut c_void,
        ) -> i32;
        fn GetTokenInformation(
            token_handle: *mut c_void,
            token_information_class: u32,
            token_information: *mut c_void,
            token_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
    }

    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCountersEx,
            cb: u32,
        ) -> i32;
    }

    /// `PROCESS_MEMORY_COUNTERS_EX` — беремо working set (RSS) поля (T-157).
    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCountersEx {
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
        private_usage: usize,
    }

    pub fn process_memory() -> super::ProcessMemory {
        let mut counters = ProcessMemoryCountersEx {
            cb: std::mem::size_of::<ProcessMemoryCountersEx>() as u32,
            ..Default::default()
        };
        // Псевдо-хендл GetCurrentProcess не потребує CloseHandle.
        let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
        if ok == 0 {
            return super::ProcessMemory::default();
        }
        super::ProcessMemory {
            working_set_bytes: counters.working_set_size as u64,
            peak_working_set_bytes: counters.peak_working_set_size as u64,
        }
    }

    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
    const INVALID_FILE_ATTRIBUTES: u32 = 0xFFFF_FFFF;
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ELEVATION_CLASS: u32 = 20; // TokenElevation
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_RAMDISK: u32 = 6;
    /// ERROR_CANCELLED — користувач закрив UAC.
    const ERROR_CANCELLED: u32 = 1223;
    /// Ціль move вже існує (T-080): FILE_EXISTS / ALREADY_EXISTS.
    const ERROR_FILE_EXISTS: u32 = 80;
    const ERROR_ALREADY_EXISTS: u32 = 183;
    /// SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI (без зайвих діалогів shell).
    const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
    const SW_SHOWNORMAL: i32 = 1;

    #[repr(C)]
    struct ShellExecuteInfoW {
        cb_size: u32,
        f_mask: u32,
        hwnd: *mut c_void,
        lp_verb: *const u16,
        lp_file: *const u16,
        lp_parameters: *const u16,
        lp_directory: *const u16,
        n_show: i32,
        h_inst_app: *mut c_void,
        lp_id_list: *mut c_void,
        lp_class: *const u16,
        h_key_class: *mut c_void,
        dw_hot_key: u32,
        h_icon_or_monitor: *mut c_void,
        h_process: *mut c_void,
    }

    #[repr(C)]
    struct TokenElevation {
        token_is_elevated: u32,
    }

    fn wide_root(volume: char) -> Vec<u16> {
        format!("{}:\\", volume.to_ascii_uppercase())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    fn wide_path(path: &std::path::Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// MoveFileW без replace-прапорців: зайнята ціль → помилка ОС, не перезапис.
    pub fn move_file_no_replace(
        source: &std::path::Path,
        destination: &std::path::Path,
    ) -> Result<super::NoReplaceMove, CoreError> {
        let wide_source = wide_path(source);
        let wide_destination = wide_path(destination);
        if unsafe { MoveFileW(wide_source.as_ptr(), wide_destination.as_ptr()) } != 0 {
            return Ok(super::NoReplaceMove::Moved);
        }
        let code = unsafe { GetLastError() };
        if code == ERROR_FILE_EXISTS || code == ERROR_ALREADY_EXISTS {
            return Ok(super::NoReplaceMove::DestinationOccupied);
        }
        Err(CoreError::io(format!(
            "Не вдалося перемістити {} → {} (Win32 {code}).",
            source.display(),
            destination.display()
        )))
    }

    pub fn set_hidden(path: &std::path::Path) -> Result<(), CoreError> {
        let wide = wide_path(path);
        let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
        if attributes == INVALID_FILE_ATTRIBUTES {
            return Err(CoreError::io(format!(
                "Не вдалося прочитати атрибути {} (Win32 {}).",
                path.display(),
                unsafe { GetLastError() }
            )));
        }
        if attributes & FILE_ATTRIBUTE_HIDDEN != 0 {
            return Ok(());
        }
        if unsafe { SetFileAttributesW(wide.as_ptr(), attributes | FILE_ATTRIBUTE_HIDDEN) } == 0 {
            return Err(CoreError::io(format!(
                "Не вдалося приховати {} (Win32 {}).",
                path.display(),
                unsafe { GetLastError() }
            )));
        }
        Ok(())
    }

    pub fn is_hidden(path: &std::path::Path) -> Result<bool, CoreError> {
        let wide = wide_path(path);
        let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
        if attributes == INVALID_FILE_ATTRIBUTES {
            return Err(CoreError::io(format!(
                "Не вдалося прочитати атрибути {} (Win32 {}).",
                path.display(),
                unsafe { GetLastError() }
            )));
        }
        Ok(attributes & FILE_ATTRIBUTE_HIDDEN != 0)
    }
    pub fn is_process_elevated() -> bool {
        unsafe {
            let mut token: *mut c_void = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut elevation = TokenElevation {
                token_is_elevated: 0,
            };
            let mut returned = 0u32;
            let ok = GetTokenInformation(
                token,
                TOKEN_ELEVATION_CLASS,
                &mut elevation as *mut _ as *mut c_void,
                std::mem::size_of::<TokenElevation>() as u32,
                &mut returned,
            );
            CloseHandle(token);
            ok != 0 && elevation.token_is_elevated != 0
        }
    }

    pub fn volume_file_system(volume: char) -> Result<Option<String>, CoreError> {
        let root = wide_root(volume);
        let mut fs_name = [0u16; 64];
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            )
        };
        if ok == 0 {
            // Том не готовий / немає носія — не помилка планування скану.
            return Ok(None);
        }
        let end = fs_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(fs_name.len());
        let name = String::from_utf16_lossy(&fs_name[..end]);
        if name.is_empty() {
            Ok(None)
        } else {
            Ok(Some(name))
        }
    }

    /// GetDiskFreeSpaceExW: загальний обсяг і вільне місце тому (T-106).
    pub fn volume_usage(volume: char) -> Result<Option<super::VolumeUsage>, CoreError> {
        let root = wide_root(volume);
        let mut available: u64 = 0;
        let mut capacity: u64 = 0;
        let mut free: u64 = 0;
        let ok =
            unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut available, &mut capacity, &mut free) };
        if ok == 0 {
            // Том не готовий / немає носія — як у volume_file_system: не помилка.
            return Ok(None);
        }
        Ok(Some(super::VolumeUsage::new(
            format!("{}:", volume.to_ascii_uppercase()),
            capacity,
            free,
        )))
    }

    pub fn list_drive_letters() -> Vec<char> {
        let mask = unsafe { GetLogicalDrives() };
        let mut out = Vec::new();
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i as u8) as char;
            let root = wide_root(letter);
            let dtype = unsafe { GetDriveTypeW(root.as_ptr()) };
            if matches!(dtype, DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_RAMDISK) {
                out.push(letter);
            }
        }
        out
    }

    /// UAC-relaunch поточного exe (ShellExecuteExW + runas).
    pub fn relaunch_elevated() -> Result<super::ElevationRelaunch, CoreError> {
        use super::ElevationRelaunch;
        use std::os::windows::ffi::OsStrExt;
        use trashradar_domain::error::ErrorCode;

        let exe = std::env::current_exe().map_err(|e| {
            CoreError::new(
                ErrorCode::Io,
                format!("Не вдалося визначити шлях до exe: {e}."),
            )
        })?;
        let exe_wide: Vec<u16> = exe
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // Аргументи командного рядка без argv[0] (як GetCommandLine / args_os).
        let params: String = std::env::args_os()
            .skip(1)
            .map(|a| {
                let s = a.to_string_lossy();
                if s.contains(' ') || s.contains('"') {
                    format!("\"{}\"", s.replace('"', "\\\""))
                } else {
                    s.into_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let params_wide: Vec<u16> = if params.is_empty() {
            vec![0]
        } else {
            params.encode_utf16().chain(std::iter::once(0)).collect()
        };
        let verb: Vec<u16> = "runas\0".encode_utf16().collect();

        let mut info = ShellExecuteInfoW {
            cb_size: std::mem::size_of::<ShellExecuteInfoW>() as u32,
            f_mask: SEE_MASK_NOCLOSEPROCESS,
            hwnd: std::ptr::null_mut(),
            lp_verb: verb.as_ptr(),
            lp_file: exe_wide.as_ptr(),
            lp_parameters: if params.is_empty() {
                std::ptr::null()
            } else {
                params_wide.as_ptr()
            },
            lp_directory: std::ptr::null(),
            n_show: SW_SHOWNORMAL,
            h_inst_app: std::ptr::null_mut(),
            lp_id_list: std::ptr::null_mut(),
            lp_class: std::ptr::null(),
            h_key_class: std::ptr::null_mut(),
            dw_hot_key: 0,
            h_icon_or_monitor: std::ptr::null_mut(),
            h_process: std::ptr::null_mut(),
        };

        let ok = unsafe { ShellExecuteExW(&mut info) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_CANCELLED {
                return Err(CoreError::new(
                    ErrorCode::Cancelled,
                    "Запит адмін-прав скасовано. Сканування продовжиться через обхід каталогів.",
                ));
            }
            return Err(CoreError::new(
                ErrorCode::Io,
                format!("Не вдалося запустити elevated-процес (Win32 error {err})."),
            ));
        }
        // Закриваємо handle нового процесу — нам достатньо факту старту.
        if !info.h_process.is_null() {
            unsafe {
                CloseHandle(info.h_process);
            }
        }
        Ok(ElevationRelaunch::Started)
    }

    /// Відкрити Explorer з виділеним файлом (T-125): `explorer.exe /select,"path"`.
    ///
    /// `raw_arg` (не `.arg()`) навмисно: `/select,` і шлях мають прийти
    /// Explorer-у як **один** аргумент командного рядка з лапками навколо
    /// шляху — так Explorer коректно парсить пробіли в шляху. Звичайний
    /// `.arg()` заекранував би вбудовані лапки й зламав парсинг.
    pub fn reveal_in_explorer(path: &str) -> Result<(), CoreError> {
        use std::os::windows::process::CommandExt;
        // Explorer — GUI-підсистема, не консоль: CREATE_NO_WINDOW (як для
        // ffmpeg sidecar, T-071) тут ні до чого, не додаємо.
        let arg = format!("/select,\"{path}\"");
        std::process::Command::new("explorer.exe")
            .raw_arg(&arg)
            .spawn()
            .map(|_| ())
            .map_err(|err| CoreError::io(err.to_string()))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use trashradar_domain::error::ErrorCode;

        #[test]
        fn invalid_drive_is_invalid_argument() {
            let err = super::super::volume_file_system('1').expect_err("bad drive");
            assert_eq!(err.code, ErrorCode::InvalidArgument);
        }

        #[test]
        fn lists_some_drive_on_windows() {
            let drives = list_drive_letters();
            assert!(
                !drives.is_empty(),
                "очікували хоча б один том, отримали {drives:?}"
            );
        }

        #[test]
        fn system_drive_reports_filesystem() {
            let drives = list_drive_letters();
            let c = drives
                .iter()
                .copied()
                .find(|&d| d == 'C')
                .unwrap_or(drives[0]);
            let fs = volume_file_system(c).expect("fs probe");
            assert!(fs.is_some(), "том {c}: мав би мати ім'я FS");
        }

        #[test]
        fn elevation_probe_does_not_panic() {
            let _ = is_process_elevated();
        }

        /// T-157: RSS процесу — ненульовий, пік не менший за поточний.
        #[test]
        fn process_memory_reports_nonzero_working_set() {
            let mem = super::super::process_memory();
            assert!(mem.working_set_bytes > 0, "working set має бути ненульовим");
            assert!(
                mem.peak_working_set_bytes >= mem.working_set_bytes,
                "пік ({}) не може бути меншим за поточний ({})",
                mem.peak_working_set_bytes,
                mem.working_set_bytes
            );
        }

        /// T-106: заповнення тому — реальні ненульові цифри, free ≤ capacity.
        #[test]
        fn system_drive_reports_usage() {
            let drives = list_drive_letters();
            let c = drives
                .iter()
                .copied()
                .find(|&d| d == 'C')
                .unwrap_or(drives[0]);
            let usage = super::super::volume_usage(c)
                .expect("usage probe")
                .expect("готовий том має usage");
            assert_eq!(usage.volume, format!("{c}:"));
            assert!(usage.capacity_bytes > 0, "capacity має бути > 0");
            assert!(usage.free_bytes <= usage.capacity_bytes);
        }

        #[test]
        fn invalid_drive_usage_is_invalid_argument() {
            let err = super::super::volume_usage('!').expect_err("bad drive");
            assert_eq!(err.code, ErrorCode::InvalidArgument);
        }

        #[test]
        fn scan_environment_impl_matches_helpers() {
            let env = WinScanEnvironment;
            assert_eq!(env.is_elevated(), is_process_elevated());
            assert_eq!(env.list_scan_volumes(), list_drive_letters());
        }

        /// T-080: move без перезапису — зайнята ціль недоторкана, джерело на місці.
        #[test]
        fn move_no_replace_moves_and_never_overwrites() {
            use super::super::{move_file_no_replace, NoReplaceMove};
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("trashradar-t080-move-{nonce}"));
            std::fs::create_dir_all(&dir).unwrap();
            let source = dir.join("source.bin");
            let destination = dir.join("destination.bin");

            // Вільна ціль → переміщено.
            std::fs::write(&source, b"payload").unwrap();
            assert_eq!(
                move_file_no_replace(&source, &destination).unwrap(),
                NoReplaceMove::Moved
            );
            assert!(!source.exists());
            assert_eq!(std::fs::read(&destination).unwrap(), b"payload");

            // Зайнята ціль → відмова без перезапису; обидва файли недоторкані.
            std::fs::write(&source, b"second").unwrap();
            assert_eq!(
                move_file_no_replace(&source, &destination).unwrap(),
                NoReplaceMove::DestinationOccupied
            );
            assert_eq!(std::fs::read(&source).unwrap(), b"second");
            assert_eq!(std::fs::read(&destination).unwrap(), b"payload");

            std::fs::remove_dir_all(dir).unwrap();
        }

        /// `relaunch_elevated` при вже elevated — no-op без UAC (не відкриває діалог).
        #[test]
        fn relaunch_when_elevated_is_already_elevated_or_ok_path() {
            // Без elevation цей тест лише перевіряє, що API не панікує
            // на гілці «вже elevated»; повний UAC не викликаємо в CI.
            if is_process_elevated() {
                let r = super::super::relaunch_elevated().expect("already elevated");
                assert_eq!(r, super::super::ElevationRelaunch::AlreadyElevated);
            }
        }
    }
}
