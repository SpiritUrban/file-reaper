//! Чистий парсер запису MFT (FILE record) — без I/O і WinAPI.
//!
//! Виділяє з сирих байтів одного запису метадані: ім'я, розмір, дати,
//! атрибути, посилання на батьківську директорію (T-021). Тестується
//! синтетичними записами без доступу до тому (docs/repository.md §4).
//!
//! Формат NTFS: заголовок FILE + Update Sequence Array (фіксапи) +
//! послідовність атрибутів. Використовуються `$STANDARD_INFORMATION` (дати,
//! DOS-атрибути), `$FILE_NAME` (ім'я + батько), unnamed `$DATA` (розмір).

use trashradar_domain::candidate::{ByteSize, FileAttributes, FsTimestamp};
use trashradar_domain::scan::ScanEntry;

/// Маска номера запису у File Reference (нижні 48 біт; старші 16 — sequence).
const MFT_REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

const FLAG_IN_USE: u16 = 0x0001;
const FLAG_DIRECTORY: u16 = 0x0002;

const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_END: u32 = 0xFFFF_FFFF;

// DOS-атрибути файлової системи.
const DOS_READONLY: u32 = 0x0001;
const DOS_HIDDEN: u32 = 0x0002;
const DOS_SYSTEM: u32 = 0x0004;
const DOS_TEMPORARY: u32 = 0x0100;

fn u16le(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes(s.try_into().unwrap()))
}
fn u32le(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
}
fn u64le(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8)
        .map(|s| u64::from_le_bytes(s.try_into().unwrap()))
}

fn filetime_opt(ft: u64) -> Option<FsTimestamp> {
    if ft == 0 {
        None
    } else {
        Some(FsTimestamp(ft as i64))
    }
}

fn dos_to_attributes(dos: u32) -> FileAttributes {
    FileAttributes {
        raw_bits: dos,
        is_readonly: dos & DOS_READONLY != 0,
        is_hidden: dos & DOS_HIDDEN != 0,
        is_system: dos & DOS_SYSTEM != 0,
        is_temporary: dos & DOS_TEMPORARY != 0,
    }
}

/// Пріоритет простору імен `$FILE_NAME`: Win32&DOS > Win32 > POSIX > DOS(8.3).
/// Обираємо «людське» ім'я, але ніколи не відкидаємо файл лише через 8.3.
fn namespace_rank(namespace: u8) -> u8 {
    match namespace {
        3 => 3, // Win32 & DOS
        1 => 2, // Win32
        0 => 1, // POSIX
        _ => 0, // DOS (8.3)
    }
}

fn utf16le_to_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Застосовує Update Sequence Array (фіксапи): відновлює останні 2 байти
/// кожного сектора з масиву оригіналів. Повертає `None`, якщо запис торнутий
/// (check-значення не збігається) або розмітка виходить за межі буфера.
fn apply_fixups(
    buf: &mut [u8],
    usa_offset: usize,
    usa_count: usize,
    bytes_per_sector: usize,
) -> Option<()> {
    if usa_count == 0 || bytes_per_sector < 2 {
        return None;
    }
    let usn = u16le(buf, usa_offset)?;
    let sectors = usa_count - 1;
    for i in 0..sectors {
        let sector_end = (i + 1) * bytes_per_sector;
        if sector_end > buf.len() {
            return None;
        }
        let last2 = sector_end - 2;
        let usa_entry = usa_offset + 2 * (i + 1);
        let original = u16le(buf, usa_entry)?;
        // Кожен сектор мусить нести check-значення USN на своїх останніх 2 байтах.
        if u16le(buf, last2)? != usn {
            return None;
        }
        let bytes = original.to_le_bytes();
        buf[last2] = bytes[0];
        buf[last2 + 1] = bytes[1];
    }
    Some(())
}

/// Розбирає один запис MFT завдовжки `record_size` байтів.
///
/// Повертає `None` для записів, які не є кандидатами на перелік: невживані,
/// розширювальні (extension) записи, або без жодного `$FILE_NAME`.
pub fn parse_record(raw: &[u8], record_number: u64, bytes_per_sector: u16) -> Option<ScanEntry> {
    if raw.len() < 0x30 || &raw[0..4] != b"FILE" {
        return None;
    }
    let flags = u16le(raw, 0x16)?;
    if flags & FLAG_IN_USE == 0 {
        return None; // невживаний запис
    }
    // Розширювальний запис: його атрибути належать базовому — пропускаємо.
    if u64le(raw, 0x20)? & MFT_REF_MASK != 0 {
        return None;
    }

    let usa_offset = u16le(raw, 0x04)? as usize;
    let usa_count = u16le(raw, 0x06)? as usize;
    let mut buf = raw.to_vec();
    apply_fixups(&mut buf, usa_offset, usa_count, bytes_per_sector as usize)?;

    let is_directory = flags & FLAG_DIRECTORY != 0;
    let first_attr = u16le(&buf, 0x14)? as usize;

    let mut best_name: Option<(u8, String, u64)> = None; // (rank, name, parent_ref)
    let mut si: Option<(u64, u64, u64, u32)> = None; // created, modified, accessed, dos
    let mut data_size: Option<u64> = None;
    let mut fn_real_size: u64 = 0;

    let mut off = first_attr;
    while off + 8 <= buf.len() {
        let atype = u32le(&buf, off)?;
        if atype == ATTR_END {
            break;
        }
        let alen = u32le(&buf, off + 4)? as usize;
        if alen < 8 || off + alen > buf.len() {
            break;
        }
        let non_resident = *buf.get(off + 8)?;

        match atype {
            ATTR_STANDARD_INFORMATION => {
                let voff = u16le(&buf, off + 0x14)? as usize;
                let v = off + voff;
                si = Some((
                    u64le(&buf, v)?,
                    u64le(&buf, v + 0x08)?,
                    u64le(&buf, v + 0x18)?,
                    u32le(&buf, v + 0x20)?,
                ));
            }
            ATTR_FILE_NAME => {
                let voff = u16le(&buf, off + 0x14)? as usize;
                let v = off + voff;
                let parent = u64le(&buf, v)? & MFT_REF_MASK;
                fn_real_size = u64le(&buf, v + 0x30)?;
                let name_len = *buf.get(v + 0x40)? as usize;
                let namespace = *buf.get(v + 0x41)?;
                let start = v + 0x42;
                let end = start + name_len * 2;
                if end <= buf.len() {
                    let rank = namespace_rank(namespace);
                    if best_name.as_ref().is_none_or(|(r, _, _)| rank > *r) {
                        best_name = Some((rank, utf16le_to_string(&buf[start..end]), parent));
                    }
                }
            }
            // Лише unnamed default-потік (name_length == 0) дає розмір файла.
            ATTR_DATA if buf.get(off + 9).copied() == Some(0) => {
                data_size = Some(if non_resident == 0 {
                    u32le(&buf, off + 0x10)? as u64
                } else {
                    u64le(&buf, off + 0x30)?
                });
            }
            _ => {}
        }
        off += alen;
    }

    let (_, name, parent_ref) = best_name?;
    let (created, modified, accessed, dos) =
        si.unwrap_or((0, 0, 0, if is_directory { 0x10 } else { 0 }));
    let size = if is_directory {
        0
    } else {
        data_size.unwrap_or(fn_real_size)
    };

    Some(ScanEntry {
        file_ref: record_number,
        parent_ref,
        name,
        size: ByteSize(size),
        created_at: filetime_opt(created),
        modified_at: filetime_opt(modified),
        accessed_at: filetime_opt(accessed),
        is_directory,
        attributes: dos_to_attributes(dos),
    })
}

/// Витягує run-list unnamed `$DATA` із запису `$MFT` (record 0), щоб знати
/// фізичне розташування таблиці на диску. Приймає сирий запис; фіксапи
/// застосовуються всередині. Повертає перелік екстентів `(lcn, clusters)`;
/// `lcn == None` — розріджений екстент (для `$MFT` не очікується).
pub fn extract_mft_runs(raw: &[u8], bytes_per_sector: u16) -> Option<Vec<(Option<i64>, u64)>> {
    if raw.len() < 0x30 || &raw[0..4] != b"FILE" {
        return None;
    }
    let usa_offset = u16le(raw, 0x04)? as usize;
    let usa_count = u16le(raw, 0x06)? as usize;
    let mut buf = raw.to_vec();
    apply_fixups(&mut buf, usa_offset, usa_count, bytes_per_sector as usize)?;

    let mut off = u16le(&buf, 0x14)? as usize;
    while off + 8 <= buf.len() {
        let atype = u32le(&buf, off)?;
        if atype == ATTR_END {
            break;
        }
        let alen = u32le(&buf, off + 4)? as usize;
        if alen < 8 || off + alen > buf.len() {
            break;
        }
        let non_resident = *buf.get(off + 8)?;
        let name_len = *buf.get(off + 9)?;
        if atype == ATTR_DATA && non_resident == 1 && name_len == 0 {
            let run_off = u16le(&buf, off + 0x20)? as usize;
            let runs_start = off + run_off;
            let runs_end = off + alen;
            return Some(parse_runlist(buf.get(runs_start..runs_end)?));
        }
        off += alen;
    }
    None
}

/// Декодує run-list NTFS у перелік екстентів `(lcn, clusters)`.
fn parse_runlist(bytes: &[u8]) -> Vec<(Option<i64>, u64)> {
    let mut runs = Vec::new();
    let mut i = 0;
    let mut lcn: i64 = 0;
    while i < bytes.len() {
        let header = bytes[i];
        if header == 0 {
            break;
        }
        let len_size = (header & 0x0F) as usize;
        let off_size = (header >> 4) as usize;
        i += 1;
        if len_size == 0 || i + len_size + off_size > bytes.len() {
            break;
        }
        let run_len = read_uint_le(&bytes[i..i + len_size]);
        i += len_size;
        if off_size == 0 {
            runs.push((None, run_len)); // розріджений екстент
        } else {
            lcn += read_int_le(&bytes[i..i + off_size]);
            i += off_size;
            runs.push((Some(lcn), run_len));
            continue;
        }
    }
    runs
}

fn read_uint_le(bytes: &[u8]) -> u64 {
    let mut v: u64 = 0;
    for (i, &b) in bytes.iter().enumerate() {
        v |= (b as u64) << (8 * i);
    }
    v
}

/// Знакове ціле змінної довжини (доповнення до двох) для дельти LCN.
fn read_int_le(bytes: &[u8]) -> i64 {
    let mut v: i64 = 0;
    for (i, &b) in bytes.iter().enumerate() {
        v |= (b as i64) << (8 * i);
    }
    let bits = bytes.len() * 8;
    if bits < 64 && (v & (1 << (bits - 1))) != 0 {
        v |= -1i64 << bits; // розширення знака
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    const REC: usize = 1024;
    const SECTOR: u16 = 512;

    /// Записує u16/u32/u64 LE у буфер за зсувом.
    fn put16(b: &mut [u8], at: usize, v: u16) {
        b[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn put32(b: &mut [u8], at: usize, v: u32) {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put64(b: &mut [u8], at: usize, v: u64) {
        b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }

    /// Конструює валідний запис FILE з SI + FILE_NAME + resident DATA.
    /// Атрибути розміщені нижче межі першого сектора (510), тож фіксапи
    /// їх не зачіпають.
    fn build_record(name: &str, parent: u64, dir: bool, data_len: u32) -> Vec<u8> {
        let mut b = vec![0u8; REC];
        b[0..4].copy_from_slice(b"FILE");
        let usa_offset = 0x30u16;
        let usa_count = 3u16; // 1 USN + 2 сектори
        put16(&mut b, 0x04, usa_offset);
        put16(&mut b, 0x06, usa_count);
        put16(&mut b, 0x10, 1); // sequence
        put16(&mut b, 0x14, 0x38); // перший атрибут
        put16(
            &mut b,
            0x16,
            FLAG_IN_USE | if dir { FLAG_DIRECTORY } else { 0 },
        );
        put64(&mut b, 0x20, 0); // base ref = 0 (базовий запис)

        // USA: USN + оригінали останніх 2 байтів кожного сектора.
        let usn: u16 = 0xABCD;
        put16(&mut b, usa_offset as usize, usn);
        put16(&mut b, usa_offset as usize + 2, 0x1111); // orig сектор 0
        put16(&mut b, usa_offset as usize + 4, 0x2222); // orig сектор 1
                                                        // На диску останні 2 байти секторів несуть check-значення USN.
        put16(&mut b, 510, usn);
        put16(&mut b, 1022, usn);

        // $STANDARD_INFORMATION @ 0x38, resident, value @ 0x18, len 0x30.
        let si = 0x38usize;
        put32(&mut b, si, ATTR_STANDARD_INFORMATION);
        put32(&mut b, si + 4, 0x48); // alen = 0x18 + 0x30
        b[si + 8] = 0; // resident
        put16(&mut b, si + 0x14, 0x18); // value offset
        put32(&mut b, si + 0x10, 0x30); // value length
        let siv = si + 0x18;
        put64(&mut b, siv, 111); // created
        put64(&mut b, siv + 0x08, 222); // modified
        put64(&mut b, siv + 0x18, 333); // accessed
        put32(&mut b, siv + 0x20, DOS_HIDDEN | DOS_READONLY);

        // $FILE_NAME @ 0x80, resident.
        let fnn = 0x80usize;
        let name_units: Vec<u16> = name.encode_utf16().collect();
        let name_bytes = name_units.len() * 2;
        let fn_value_len = 0x42 + name_bytes;
        let fn_alen = (0x18 + fn_value_len).div_ceil(8) * 8;
        put32(&mut b, fnn, ATTR_FILE_NAME);
        put32(&mut b, fnn + 4, fn_alen as u32);
        b[fnn + 8] = 0;
        put16(&mut b, fnn + 0x14, 0x18);
        put32(&mut b, fnn + 0x10, fn_value_len as u32);
        let fv = fnn + 0x18;
        put64(&mut b, fv, parent); // parent ref
        put64(&mut b, fv + 0x30, 4096); // real size (fallback)
        b[fv + 0x40] = name_units.len() as u8;
        b[fv + 0x41] = 1; // Win32 namespace
        for (i, u) in name_units.iter().enumerate() {
            put16(&mut b, fv + 0x42 + i * 2, *u);
        }

        // $DATA одразу після $FILE_NAME (атрибути йдуть суцільно), resident, unnamed.
        let dat = fnn + fn_alen;
        put32(&mut b, dat, ATTR_DATA);
        let d_alen = (0x18 + data_len as usize).div_ceil(8) * 8;
        put32(&mut b, dat + 4, d_alen as u32);
        b[dat + 8] = 0; // resident
        b[dat + 9] = 0; // unnamed
        put16(&mut b, dat + 0x14, 0x18);
        put32(&mut b, dat + 0x10, data_len);

        // Кінець списку атрибутів.
        put32(&mut b, dat + d_alen, ATTR_END);
        b
    }

    #[test]
    fn parses_file_record_metadata() {
        let rec = build_record("holiday.mp4", 5, false, 10);
        let e = parse_record(&rec, 42, SECTOR).expect("valid file record");
        assert_eq!(e.file_ref, 42);
        assert_eq!(e.parent_ref, 5);
        assert_eq!(e.name, "holiday.mp4");
        assert_eq!(e.size, ByteSize(10)); // з unnamed $DATA, не з $FILE_NAME
        assert!(!e.is_directory);
        assert_eq!(e.created_at, Some(FsTimestamp(111)));
        assert_eq!(e.modified_at, Some(FsTimestamp(222)));
        assert_eq!(e.accessed_at, Some(FsTimestamp(333)));
        assert!(e.attributes.is_hidden);
        assert!(e.attributes.is_readonly);
        assert!(!e.attributes.is_system);
    }

    #[test]
    fn directory_record_has_zero_size_and_flag() {
        let rec = build_record("Videos", 5, true, 0);
        let e = parse_record(&rec, 7, SECTOR).expect("valid dir record");
        assert!(e.is_directory);
        assert_eq!(e.size, ByteSize(0));
        assert_eq!(e.name, "Videos");
    }

    #[test]
    fn fixups_restore_sector_tail_bytes() {
        let rec = build_record("f.txt", 5, false, 4);
        // Після фіксапів останні 2 байти секторів = оригінали з USA.
        let e = parse_record(&rec, 1, SECTOR);
        assert!(e.is_some());
    }

    #[test]
    fn rejects_record_with_torn_fixup() {
        let mut rec = build_record("f.txt", 5, false, 4);
        // Псуємо check-значення на межі сектора → запис вважається торнутим.
        put16(&mut rec, 510, 0x9999);
        assert!(parse_record(&rec, 1, SECTOR).is_none());
    }

    #[test]
    fn skips_unused_record() {
        let mut rec = build_record("f.txt", 5, false, 4);
        put16(&mut rec, 0x16, 0); // прапорець in-use знято
        assert!(parse_record(&rec, 1, SECTOR).is_none());
    }

    #[test]
    fn skips_extension_record() {
        let mut rec = build_record("f.txt", 5, false, 4);
        put64(&mut rec, 0x20, 99); // base ref != 0 → розширювальний запис
        assert!(parse_record(&rec, 1, SECTOR).is_none());
    }

    #[test]
    fn skips_non_file_signature() {
        let mut rec = build_record("f.txt", 5, false, 4);
        rec[0..4].copy_from_slice(b"BAAD");
        assert!(parse_record(&rec, 1, SECTOR).is_none());
    }

    #[test]
    fn prefers_win32_name_over_dos_short_name() {
        let mut rec = build_record("longname.txt", 5, false, 4);
        // Додаємо другий $FILE_NAME (DOS 8.3) одразу після першого блоку.
        // Він має нижчий пріоритет, тож парсер лишає Win32-ім'я.
        let fnn = 0x80usize;
        let first_len = u32le(&rec, fnn + 4).unwrap() as usize;
        let dos = fnn + first_len;
        let dos_name: Vec<u16> = "LONGNA~1.TXT".encode_utf16().collect();
        put32(&mut rec, dos, ATTR_FILE_NAME);
        let val_len = 0x42 + dos_name.len() * 2;
        let alen = (0x18 + val_len).div_ceil(8) * 8;
        put32(&mut rec, dos + 4, alen as u32);
        put16(&mut rec, dos + 0x14, 0x18);
        put32(&mut rec, dos + 0x10, val_len as u32);
        let dv = dos + 0x18;
        put64(&mut rec, dv, 5);
        rec[dv + 0x40] = dos_name.len() as u8;
        rec[dv + 0x41] = 2; // DOS namespace
        for (i, u) in dos_name.iter().enumerate() {
            put16(&mut rec, dv + 0x42 + i * 2, *u);
        }
        put32(&mut rec, dos + alen, ATTR_END);

        let e = parse_record(&rec, 1, SECTOR).unwrap();
        assert_eq!(e.name, "longname.txt");
    }

    #[test]
    fn runlist_decodes_extents_with_signed_offsets() {
        // 0x21 0x08 0x00_02: len=8 кластерів, зсув LCN=+0x200.
        // 0x11 0x04 0x05: len=4, зсув +5 (відносно попереднього).
        // 0x00: кінець.
        let bytes = [0x21, 0x08, 0x00, 0x02, 0x11, 0x04, 0x05, 0x00];
        let runs = parse_runlist(&bytes);
        assert_eq!(runs, vec![(Some(0x200), 8), (Some(0x205), 4)]);
    }

    #[test]
    fn signed_varint_handles_negative_offset() {
        // 0xFF як 1-байтовий зсув = -1.
        assert_eq!(read_int_le(&[0xFF]), -1);
        assert_eq!(read_int_le(&[0x00, 0x02]), 0x200);
        assert_eq!(read_uint_le(&[0x08]), 8);
    }
}
