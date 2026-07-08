//! Спільні WinAPI-обгортки для інфраструктурних крейтів.
//!
//! Єдиний крейт, від якого дозволені горизонтальні залежності
//! всередині `infra/*` (docs/repository.md §10).
//!
//! T-028: тип файлової системи тому, elevation процесу, перелік томів.
//! T-034: запит elevation з UI (окремо) — тут лише детекція.

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

    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ELEVATION_CLASS: u32 = 20; // TokenElevation
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_RAMDISK: u32 = 6;

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
    }
}
