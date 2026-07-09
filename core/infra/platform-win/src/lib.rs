//! Спільні WinAPI-обгортки для інфраструктурних крейтів.
//!
//! Єдиний крейт, від якого дозволені горизонтальні залежності
//! всередині `infra/*` (docs/repository.md §10).
//!
//! T-028: тип файлової системи тому, elevation процесу, перелік томів.
//! T-034: детекція + UAC-relaunch (`relaunch_elevated`); сесійна політика
//! відмови — у `trashradar_app::elevation`.

use trashradar_app::ports::ScanEnvironment;
use trashradar_domain::error::CoreError;

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
        fn GetLastError() -> u32;
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

    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ELEVATION_CLASS: u32 = 20; // TokenElevation
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_RAMDISK: u32 = 6;
    /// ERROR_CANCELLED — користувач закрив UAC.
    const ERROR_CANCELLED: u32 = 1223;
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

        #[test]
        fn scan_environment_impl_matches_helpers() {
            let env = WinScanEnvironment;
            assert_eq!(env.is_elevated(), is_process_elevated());
            assert_eq!(env.list_scan_volumes(), list_drive_letters());
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
