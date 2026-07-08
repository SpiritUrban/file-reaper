//! Читання NTFS USN Change Journal через WinAPI (T-029).
//!
//! - `FSCTL_QUERY_USN_JOURNAL` — id + межі;
//! - `FSCTL_READ_USN_JOURNAL` — дельта з курсора.
//!
//! Потребує прав на том (зазвичай admin, як і MFT).

#![cfg(windows)]

use std::ffi::c_void;

use trashradar_app::ports::UsnReadOutcome;
use trashradar_domain::error::{CoreError, ErrorCode};
use trashradar_domain::scan::{usn_reason, UsnCursor, UsnJournalInfo};

use crate::record::parse_read_usn_buffer;

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
const INVALID_HANDLE_VALUE: isize = -1;

const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00f4;
const FSCTL_READ_USN_JOURNAL: u32 = 0x0009_00bb;

/// ERROR_JOURNAL_NOT_ACTIVE — журнал вимкнено.
const ERROR_JOURNAL_NOT_ACTIVE: u32 = 1179;
/// ERROR_JOURNAL_ENTRY_DELETED — StartUsn уже випав з журналу.
const ERROR_JOURNAL_ENTRY_DELETED: u32 = 1181;

/// Підмножина USN_JOURNAL_DATA_V0.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct UsnJournalDataV0 {
    usn_journal_id: u64,
    first_usn: i64,
    next_usn: i64,
    lowest_valid_usn: i64,
    max_usn: i64,
    maximum_size: u64,
    allocation_delta: u64,
}

/// READ_USN_JOURNAL_DATA_V0.
#[repr(C)]
#[derive(Clone, Copy)]
struct ReadUsnJournalDataV0 {
    start_usn: i64,
    reason_mask: u32,
    return_only_on_close: u32,
    timeout: u64,
    bytes_to_wait_for: u64,
    usn_journal_id: u64,
}

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
            "Некоректна літера тому «{drive}»."
        )));
    }
    let path = format!("\\\\.\\{}:", drive.to_ascii_uppercase());
    let w = wide(&path);
    let handle = unsafe {
        CreateFileW(
            w.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle as isize == INVALID_HANDLE_VALUE {
        return Err(last_os_error(&format!(
            "Не вдалося відкрити том {} для USN",
            drive.to_ascii_uppercase()
        )));
    }
    Ok(VolumeHandle(handle))
}

/// Запит стану журналу (`FSCTL_QUERY_USN_JOURNAL`).
pub fn query_journal(drive: char) -> Result<UsnJournalInfo, CoreError> {
    let handle = open_volume(drive)?;
    let mut data = UsnJournalDataV0::default();
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle.0,
            FSCTL_QUERY_USN_JOURNAL,
            std::ptr::null(),
            0,
            &mut data as *mut _ as *mut c_void,
            std::mem::size_of::<UsnJournalDataV0>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        if err == ERROR_JOURNAL_NOT_ACTIVE {
            return Err(CoreError::new(
                ErrorCode::Io,
                format!(
                    "USN Journal не активний на томі {}.",
                    drive.to_ascii_uppercase()
                ),
            ));
        }
        return Err(last_os_error("FSCTL_QUERY_USN_JOURNAL не вдалося"));
    }
    Ok(UsnJournalInfo {
        journal_id: data.usn_journal_id,
        lowest_valid_usn: data.lowest_valid_usn,
        next_usn: data.next_usn,
        first_usn: data.first_usn,
    })
}

/// Читає дельту з `from` до поточного кінця журналу (або JournalStale).
pub fn read_delta(drive: char, from: UsnCursor) -> Result<UsnReadOutcome, CoreError> {
    let info = query_journal(drive)?;
    if !info.is_cursor_valid(from) {
        let reason = if from.journal_id != info.journal_id {
            "journal_id_changed"
        } else {
            "usn_below_lowest_valid"
        };
        tracing::warn!(
            volume = %drive.to_ascii_uppercase(),
            reason,
            saved_journal = from.journal_id,
            live_journal = info.journal_id,
            saved_usn = from.next_usn,
            lowest = info.lowest_valid_usn,
            "USN-курсор застарів — потрібен повний рескан"
        );
        return Ok(UsnReadOutcome::JournalStale { info, reason });
    }

    let handle = open_volume(drive)?;
    let mut all_changes = Vec::new();
    let mut start_usn = from.next_usn;
    let mut buffer = vec![0u8; 64 * 1024];

    loop {
        let read_data = ReadUsnJournalDataV0 {
            start_usn,
            reason_mask: usn_reason::INDEX_RELEVANT,
            return_only_on_close: 0,
            timeout: 0,
            bytes_to_wait_for: 0,
            usn_journal_id: from.journal_id,
        };
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                handle.0,
                FSCTL_READ_USN_JOURNAL,
                &read_data as *const _ as *const c_void,
                std::mem::size_of::<ReadUsnJournalDataV0>() as u32,
                buffer.as_mut_ptr() as *mut c_void,
                buffer.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_JOURNAL_ENTRY_DELETED {
                return Ok(UsnReadOutcome::JournalStale {
                    info,
                    reason: "journal_entry_deleted",
                });
            }
            // Немає нових записів / кінець — не помилка, якщо вже щось прочитали
            // або StartUsn == NextUsn.
            if start_usn >= info.next_usn || returned < 8 {
                break;
            }
            return Err(last_os_error("FSCTL_READ_USN_JOURNAL не вдалося"));
        }
        if returned < 8 {
            break;
        }
        let Some(parsed) = parse_read_usn_buffer(&buffer, returned as usize) else {
            return Err(CoreError::internal(
                "Некоректний буфер відповіді USN Journal.".to_string(),
            ));
        };
        let batch_len = parsed.changes.len();
        all_changes.extend(parsed.changes);
        // NextUsn з відповіді — курсор для наступного READ.
        if parsed.next_start_usn <= start_usn {
            // Прогресу немає — вихід, щоб не зациклитись.
            start_usn = parsed.next_start_usn;
            break;
        }
        start_usn = parsed.next_start_usn;
        // Порожня партія і курсор на кінці — готово.
        if batch_len == 0 && start_usn >= info.next_usn {
            break;
        }
        // Якщо дочитали до кінця журналу на момент query — стоп.
        if start_usn >= info.next_usn {
            break;
        }
    }

    // Актуальний кінець: max(прочитаний next, query next) у межах того ж journal id.
    let end_info = query_journal(drive)?;
    if end_info.journal_id != from.journal_id {
        return Ok(UsnReadOutcome::JournalStale {
            info: end_info,
            reason: "journal_id_changed_during_read",
        });
    }
    let next_cursor = UsnCursor {
        journal_id: from.journal_id,
        next_usn: start_usn.max(from.next_usn),
    };

    tracing::debug!(
        volume = %drive.to_ascii_uppercase(),
        changes = all_changes.len(),
        from_usn = from.next_usn,
        next_usn = next_cursor.next_usn,
        "прочитано USN-дельту"
    );

    Ok(UsnReadOutcome::Changes {
        changes: all_changes,
        next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_drive_is_invalid_argument() {
        let err = query_journal('1').expect_err("bad drive");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }
}
