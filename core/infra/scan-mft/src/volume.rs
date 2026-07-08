//! Читач `$MFT` тому NTFS через WinAPI (T-021, швидкий шлях).
//!
//! Відкриває том, дізнається геометрію через `FSCTL_GET_NTFS_VOLUME_DATA`,
//! читає запис `$MFT` (record 0), відновлює його run-list і послідовно
//! вичитує всі записи MFT великими батчами, згодовуючи їх чистому парсеру
//! [`crate::record::parse_record`]. Потребує прав адміністратора.
//!
//! FFI оголошено локально в межах крейта: до проєктування спільного
//! `platform-win` (T-028/T-034) це найменш зв'язне рішення.

#![cfg(windows)]

use std::ffi::c_void;

use trashradar_domain::error::{CoreError, ErrorCode};
use trashradar_domain::scan::ScanEntry;

use crate::record::{extract_mft_runs, parse_record};

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
    fn SetFilePointerEx(
        h_file: *mut c_void,
        li_distance_to_move: i64,
        lp_new_file_pointer: *mut i64,
        dw_move_method: u32,
    ) -> i32;
    fn ReadFile(
        h_file: *mut c_void,
        lp_buffer: *mut c_void,
        n_number_of_bytes_to_read: u32,
        lp_number_of_bytes_read: *mut u32,
        lp_overlapped: *mut c_void,
    ) -> i32;
    fn CloseHandle(h_object: *mut c_void) -> i32;
}

const GENERIC_READ: u32 = 0x8000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;
const FILE_BEGIN: u32 = 0;
const FSCTL_GET_NTFS_VOLUME_DATA: u32 = 0x0009_0064;

/// Верхня межа одного ReadFile — щоб не тримати гігабайтний буфер.
const MAX_CHUNK_BYTES: u64 = 8 << 20; // 8 MiB

/// Підмножина `NTFS_VOLUME_DATA_BUFFER`, потрібна для читання `$MFT`.
#[repr(C)]
#[derive(Default)]
struct NtfsVolumeData {
    volume_serial_number: i64,
    number_sectors: i64,
    total_clusters: i64,
    free_clusters: i64,
    total_reserved: i64,
    bytes_per_sector: u32,
    bytes_per_cluster: u32,
    bytes_per_file_record_segment: u32,
    clusters_per_file_record_segment: u32,
    mft_valid_data_length: i64,
    mft_start_lcn: i64,
    mft2_start_lcn: i64,
    mft_zone_start: i64,
    mft_zone_end: i64,
}

/// RAII-обгортка над HANDLE тому.
struct VolumeHandle(*mut c_void);

impl Drop for VolumeHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_os_error(context: &str) -> CoreError {
    CoreError::new(
        ErrorCode::Io,
        format!("{context}: {}.", std::io::Error::last_os_error()),
    )
}

fn open_volume(drive: char) -> Result<VolumeHandle, CoreError> {
    if !drive.is_ascii_alphabetic() {
        return Err(CoreError::invalid_argument(format!(
            "Некоректна літера тому: «{drive}»."
        )));
    }
    let path = wide(&format!("\\\\.\\{}:", drive.to_ascii_uppercase()));
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
        return Err(last_os_error(&format!(
            "Не вдалося відкрити том {drive}: (потрібні адмін-права або том зайнятий/недоступний)"
        )));
    }
    Ok(VolumeHandle(handle))
}

fn query_volume_data(handle: &VolumeHandle) -> Result<NtfsVolumeData, CoreError> {
    let mut data = NtfsVolumeData::default();
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle.0,
            FSCTL_GET_NTFS_VOLUME_DATA,
            std::ptr::null(),
            0,
            &mut data as *mut _ as *mut c_void,
            std::mem::size_of::<NtfsVolumeData>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_os_error("FSCTL_GET_NTFS_VOLUME_DATA не вдалося"));
    }
    if data.bytes_per_file_record_segment == 0 || data.bytes_per_cluster == 0 {
        return Err(CoreError::new(
            ErrorCode::Io,
            "Том повернув нульову геометрію NTFS.".to_string(),
        ));
    }
    Ok(data)
}

fn read_at(handle: &VolumeHandle, offset: i64, buf: &mut [u8]) -> Result<usize, CoreError> {
    if unsafe { SetFilePointerEx(handle.0, offset, std::ptr::null_mut(), FILE_BEGIN) } == 0 {
        return Err(last_os_error("Позиціювання на томі не вдалося"));
    }
    let mut total = 0usize;
    while total < buf.len() {
        let mut read: u32 = 0;
        let want = (buf.len() - total).min(u32::MAX as usize) as u32;
        let ok = unsafe {
            ReadFile(
                handle.0,
                buf[total..].as_mut_ptr() as *mut c_void,
                want,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_os_error("Читання тому не вдалося"));
        }
        if read == 0 {
            break; // кінець даних
        }
        total += read as usize;
    }
    Ok(total)
}

/// Статистика проходу скану.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScanStats {
    pub records_scanned: u64,
    pub entries: u64,
    pub directories: u64,
    /// Пошкоджені записи, пропущені з логом (T-025).
    pub corrupt: u64,
}

/// Перелічує всі записи `$MFT` тому, викликаючи `sink` для кожного валідного
/// запису (файл або тека). Повертає статистику проходу.
pub fn enumerate_with(
    drive: char,
    mut sink: impl FnMut(ScanEntry),
) -> Result<ScanStats, CoreError> {
    let handle = open_volume(drive)?;
    let vol = query_volume_data(&handle)?;

    let cluster = vol.bytes_per_cluster as u64;
    let rec_size = vol.bytes_per_file_record_segment as u64;
    let sector = vol.bytes_per_sector as u16;
    let total_records = (vol.mft_valid_data_length.max(0) as u64) / rec_size;

    // Запис 0 ($MFT сам) — з його run-list дізнаємось фізичне розташування таблиці.
    let mut first_cluster = vec![0u8; cluster.max(rec_size) as usize];
    read_at(
        &handle,
        vol.mft_start_lcn * cluster as i64,
        &mut first_cluster,
    )?;
    let record0 = &first_cluster[..rec_size as usize];
    let runs = extract_mft_runs(record0, sector).ok_or_else(|| {
        CoreError::new(
            ErrorCode::Io,
            "Не вдалося прочитати run-list $MFT (запис 0).".to_string(),
        )
    })?;

    let mut stats = ScanStats::default();
    let mut buffer = vec![0u8; MAX_CHUNK_BYTES as usize];

    'runs: for (lcn, clusters) in runs {
        let run_bytes = clusters * cluster;
        let Some(lcn) = lcn else {
            // Розріджений екстент $MFT: записи в ньому відсутні, лише зсуваємо лічильник.
            stats.records_scanned += run_bytes / rec_size;
            if stats.records_scanned >= total_records {
                break;
            }
            continue;
        };
        let run_start = lcn * cluster as i64;

        let mut done = 0u64;
        while done < run_bytes {
            if stats.records_scanned >= total_records {
                break 'runs;
            }
            let chunk = (run_bytes - done).min(MAX_CHUNK_BYTES);
            let chunk = chunk as usize;
            read_at(&handle, run_start + done as i64, &mut buffer[..chunk])?;

            let mut pos = 0usize;
            while pos + rec_size as usize <= chunk {
                if stats.records_scanned >= total_records {
                    break 'runs;
                }
                let rec = &buffer[pos..pos + rec_size as usize];
                let record_number = stats.records_scanned;
                stats.records_scanned += 1;
                match parse_record(rec, record_number, sector) {
                    Ok(Some(entry)) => {
                        stats.entries += 1;
                        if entry.is_directory {
                            stats.directories += 1;
                        }
                        sink(entry);
                    }
                    Ok(None) => {} // нормальний пропуск (невживаний/розширювальний)
                    Err(err) => {
                        // Пошкоджений запис: пропускаємо з логом, скан триває (T-025).
                        stats.corrupt += 1;
                        tracing::debug!(
                            record = record_number,
                            reason = err.reason(),
                            "пропущено пошкоджений запис MFT"
                        );
                    }
                }
                pos += rec_size as usize;
            }
            done += chunk as u64;
        }
    }

    if stats.corrupt > 0 {
        tracing::warn!(
            corrupt = stats.corrupt,
            scanned = stats.records_scanned,
            "MFT-скан пропустив пошкоджені записи"
        );
    }

    Ok(stats)
}

/// Перелічує весь том, збираючи записи у `Vec`. Зручність поверх
/// [`enumerate_with`]; батчева/стрімінгова видача — задача T-024.
pub fn enumerate(drive: char) -> Result<Vec<ScanEntry>, CoreError> {
    let mut out = Vec::new();
    enumerate_with(drive, |e| out.push(e))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trashradar_domain::error::ErrorCode;

    #[test]
    fn non_alphabetic_drive_is_invalid_argument() {
        // Недоступний/некоректний том → чиста типізована помилка, не паніка (T-025).
        let err = enumerate('1').expect_err("некоректна літера тому");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }
}
