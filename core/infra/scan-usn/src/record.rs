//! Чистий парсер записів USN Journal (T-029).
//!
//! Розбирає буфер `FSCTL_READ_USN_JOURNAL` / фрагменти `USN_RECORD_V2`
//! без I/O — тестується в CI на синтетичних байтах.

use trashradar_domain::candidate::FsTimestamp;
use trashradar_domain::scan::UsnChange;

/// Маска номера запису MFT у 64-бітному FileReferenceNumber.
const MFT_REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

/// Результат розбору відповіді `FSCTL_READ_USN_JOURNAL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUsnBuffer {
    /// USN для наступного виклику READ (перші 8 байт відповіді).
    pub next_start_usn: i64,
    pub changes: Vec<UsnChange>,
}

/// Розбирає тіло відповіді `FSCTL_READ_USN_JOURNAL`.
///
/// Формат: `USN NextUsn` (8 байт) + послідовність `USN_RECORD_V2`.
pub fn parse_read_usn_buffer(buf: &[u8], bytes_returned: usize) -> Option<ParsedUsnBuffer> {
    if bytes_returned < 8 || buf.len() < bytes_returned {
        return None;
    }
    let next_start_usn = i64::from_le_bytes(buf[0..8].try_into().ok()?);
    let mut changes = Vec::new();
    let mut off = 8usize;
    let limit = bytes_returned;
    while off + 60 <= limit {
        let record_length = u32::from_le_bytes(buf[off..off + 4].try_into().ok()?) as usize;
        if record_length < 60 || off + record_length > limit {
            break;
        }
        if let Some(change) = parse_usn_record_v2(&buf[off..off + record_length]) {
            changes.push(change);
        }
        off += record_length;
    }
    Some(ParsedUsnBuffer {
        next_start_usn,
        changes,
    })
}

/// Розбирає один `USN_RECORD_V2` (або сумісний v3 з тими ж зсувами полів імені).
pub fn parse_usn_record_v2(rec: &[u8]) -> Option<UsnChange> {
    if rec.len() < 60 {
        return None;
    }
    let major = u16::from_le_bytes(rec[4..6].try_into().ok()?);
    // v2 і v3 мають однакові зсуви для полів до FileName; v4 — інший формат.
    if major != 2 && major != 3 {
        return None;
    }

    let file_ref = u64::from_le_bytes(rec[8..16].try_into().ok()?) & MFT_REF_MASK;
    let parent_ref = u64::from_le_bytes(rec[16..24].try_into().ok()?) & MFT_REF_MASK;
    let usn = i64::from_le_bytes(rec[24..32].try_into().ok()?);
    let timestamp_raw = i64::from_le_bytes(rec[32..40].try_into().ok()?);
    let reason = u32::from_le_bytes(rec[40..44].try_into().ok()?);
    let file_attributes = u32::from_le_bytes(rec[52..56].try_into().ok()?);
    let name_len = u16::from_le_bytes(rec[56..58].try_into().ok()?) as usize;
    let name_off = u16::from_le_bytes(rec[58..60].try_into().ok()?) as usize;

    let name_end = name_off.checked_add(name_len)?;
    if name_end > rec.len() {
        return None;
    }
    let name = utf16le_to_string(&rec[name_off..name_end]);

    let timestamp = if timestamp_raw == 0 {
        None
    } else {
        Some(FsTimestamp(timestamp_raw))
    };

    Some(UsnChange {
        usn,
        file_ref,
        parent_ref,
        reason,
        name,
        is_directory: file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0,
        timestamp,
    })
}

fn utf16le_to_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Будує синтетичний `USN_RECORD_V2` для тестів.
#[cfg(test)]
pub fn build_test_record_v2(
    usn: i64,
    file_ref: u64,
    parent_ref: u64,
    reason: u32,
    name: &str,
    is_directory: bool,
) -> Vec<u8> {
    let name_units: Vec<u16> = name.encode_utf16().collect();
    let name_bytes: Vec<u8> = name_units.iter().flat_map(|u| u.to_le_bytes()).collect();
    let name_off = 60u16;
    let record_length = (60 + name_bytes.len() + 7) & !7; // align 8
    let mut rec = vec![0u8; record_length];
    rec[0..4].copy_from_slice(&(record_length as u32).to_le_bytes());
    rec[4..6].copy_from_slice(&2u16.to_le_bytes()); // major
    rec[6..8].copy_from_slice(&0u16.to_le_bytes()); // minor
    rec[8..16].copy_from_slice(&file_ref.to_le_bytes());
    rec[16..24].copy_from_slice(&parent_ref.to_le_bytes());
    rec[24..32].copy_from_slice(&usn.to_le_bytes());
    rec[32..40].copy_from_slice(&0i64.to_le_bytes());
    rec[40..44].copy_from_slice(&reason.to_le_bytes());
    let attrs = if is_directory {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        0
    };
    rec[52..56].copy_from_slice(&attrs.to_le_bytes());
    rec[56..58].copy_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    rec[58..60].copy_from_slice(&name_off.to_le_bytes());
    rec[60..60 + name_bytes.len()].copy_from_slice(&name_bytes);
    rec
}

#[cfg(test)]
mod tests {
    use super::*;
    use trashradar_domain::scan::usn_reason;

    #[test]
    fn parses_single_record_from_read_buffer() {
        let rec = build_test_record_v2(
            42,
            100,
            5,
            usn_reason::FILE_CREATE | usn_reason::CLOSE,
            "photo.jpg",
            false,
        );
        let next_usn: i64 = 99;
        let mut buf = next_usn.to_le_bytes().to_vec();
        buf.extend_from_slice(&rec);

        let parsed = parse_read_usn_buffer(&buf, buf.len()).expect("parse");
        assert_eq!(parsed.next_start_usn, 99);
        assert_eq!(parsed.changes.len(), 1);
        let c = &parsed.changes[0];
        assert_eq!(c.usn, 42);
        assert_eq!(c.file_ref, 100);
        assert_eq!(c.parent_ref, 5);
        assert_eq!(c.name, "photo.jpg");
        assert!(!c.is_directory);
        assert_eq!(c.reason & usn_reason::FILE_CREATE, usn_reason::FILE_CREATE);
    }

    #[test]
    fn parses_directory_flag_and_multiple_records() {
        let r1 = build_test_record_v2(1, 10, 5, usn_reason::FILE_CREATE, "a.txt", false);
        let r2 = build_test_record_v2(2, 11, 5, usn_reason::FILE_CREATE, "subdir", true);
        let mut buf = 3i64.to_le_bytes().to_vec();
        buf.extend_from_slice(&r1);
        buf.extend_from_slice(&r2);

        let parsed = parse_read_usn_buffer(&buf, buf.len()).expect("parse");
        assert_eq!(parsed.changes.len(), 2);
        assert!(!parsed.changes[0].is_directory);
        assert!(parsed.changes[1].is_directory);
        assert_eq!(parsed.changes[1].name, "subdir");
    }

    #[test]
    fn short_buffer_is_none() {
        assert!(parse_read_usn_buffer(&[1, 2, 3], 3).is_none());
    }

    #[test]
    fn rejects_unknown_major_version() {
        let mut rec = build_test_record_v2(1, 1, 5, 0, "x", false);
        rec[4..6].copy_from_slice(&4u16.to_le_bytes());
        assert!(parse_usn_record_v2(&rec).is_none());
    }
}
