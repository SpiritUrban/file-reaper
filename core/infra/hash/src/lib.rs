//! Адаптер [`Hasher`]: щаблі 2–3 каскаду дублікатів (architecture.md §4).
//!
//! - **T-059**: partial fingerprint — перші + останні 64 КіБ, ≤ 128 КіБ I/O;
//! - **T-060**: повний потоковий BLAKE3, **константна** пам'ять на файл.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use trashradar_app::ports::Hasher;
use trashradar_app::workers::CancellationToken;
use trashradar_domain::candidate::ByteSize;
use trashradar_domain::duplicates::{
    ContentHash, PartialHash, PARTIAL_HASH_CHUNK_BYTES, PARTIAL_HASH_MAX_READ_BYTES,
};
use trashradar_domain::error::CoreError;

/// Розмір буфера потокового читання для повного хешу (T-060).
/// Фіксований — O(1) пам'ять на файл незалежно від size.
pub const FULL_HASH_STREAM_BUFFER_BYTES: usize = 256 * 1024;

/// Файловий Hasher (BLAKE3 partial + full stream).
#[derive(Debug, Default, Clone, Copy)]
pub struct FileHasher;

impl FileHasher {
    pub fn new() -> Self {
        Self
    }
}

impl Hasher for FileHasher {
    fn partial_hash(&self, path: &str, size: ByteSize) -> Result<PartialHash, CoreError> {
        let (hash, _bytes_read) = partial_hash_path(Path::new(path), size.0)?;
        Ok(hash)
    }

    fn full_hash(
        &self,
        path: &str,
        size: ByteSize,
        cancel: &CancellationToken,
    ) -> Result<ContentHash, CoreError> {
        let (hash, _bytes_read) = full_hash_path(Path::new(path), size.0, cancel)?;
        Ok(hash)
    }
}

/// Прочитати head/tail і порахувати BLAKE3. Повертає (hash, bytes_read).
///
/// DoD T-059: `bytes_read ≤ PARTIAL_HASH_MAX_READ_BYTES` (128 КіБ).
pub fn partial_hash_path(path: &Path, size: u64) -> Result<(PartialHash, u64), CoreError> {
    if size == 0 {
        return Err(CoreError::invalid_argument(
            "Порожній файл не хешується у каскаді дублікатів.",
        ));
    }

    let mut file = File::open(path).map_err(|e| {
        CoreError::io(format!(
            "Не вдалося відкрити файл для часткового хешу: {path}: {e}",
            path = path.display()
        ))
    })?;

    let (head, tail, bytes_read) = read_partial_chunks(&mut file, size)?;
    debug_assert!(bytes_read <= PARTIAL_HASH_MAX_READ_BYTES);

    let hash = fingerprint_head_tail(&head, &tail);
    Ok((hash, bytes_read))
}

/// Повний потоковий BLAKE3. Пам'ять = O(`FULL_HASH_STREAM_BUFFER_BYTES`), не O(size).
///
/// DoD T-060: константна пам'ять; cancel між блоками.
pub fn full_hash_path(
    path: &Path,
    size: u64,
    cancel: &CancellationToken,
) -> Result<(ContentHash, u64), CoreError> {
    if size == 0 {
        return Err(CoreError::invalid_argument(
            "Порожній файл не хешується у каскаді дублікатів.",
        ));
    }
    if cancel.is_cancelled() {
        return Err(CoreError::cancelled("full_hash"));
    }

    let mut file = File::open(path).map_err(|e| {
        CoreError::io(format!(
            "Не вдалося відкрити файл для повного хешу: {path}: {e}",
            path = path.display()
        ))
    })?;

    let mut hasher = blake3::Hasher::new();
    // Єдиний heap-буфер на весь прохід — константний розмір.
    let mut buf = vec![0u8; FULL_HASH_STREAM_BUFFER_BYTES];
    let mut bytes_read = 0u64;

    loop {
        if cancel.is_cancelled() {
            return Err(CoreError::cancelled("full_hash"));
        }
        let n = file
            .read(&mut buf)
            .map_err(|e| CoreError::io(format!("Помилка читання під час повного хешу: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        bytes_read += n as u64;
    }

    // Якщо size з індексу відомий — м'яка перевірка (файл міг змінитись; T-086 жорсткіше).
    if size > 0 && bytes_read != size {
        // Не валимо: індекс міг застаріти; хеш усе одно коректний для прочитаних байтів.
        // (Кеш за size+mtime — T-062.)
    }

    Ok((ContentHash(*hasher.finalize().as_bytes()), bytes_read))
}

/// BLAKE3(head ‖ tail). Для size ≤ 64 КіБ `tail` порожній (файл уже в head).
pub fn fingerprint_head_tail(head: &[u8], tail: &[u8]) -> PartialHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(head);
    hasher.update(tail);
    PartialHash(*hasher.finalize().as_bytes())
}

/// Прочитати перші й (за потреби) останні 64 КіБ.
pub fn read_partial_chunks(
    file: &mut File,
    size: u64,
) -> Result<(Vec<u8>, Vec<u8>, u64), CoreError> {
    let chunk = PARTIAL_HASH_CHUNK_BYTES as usize;

    if size <= PARTIAL_HASH_CHUNK_BYTES {
        let n = size as usize;
        let mut head = vec![0u8; n];
        file.read_exact(&mut head)
            .map_err(|e| CoreError::io(format!("Не вдалося прочитати початок файла: {e}")))?;
        return Ok((head, Vec::new(), size));
    }

    let mut head = vec![0u8; chunk];
    file.read_exact(&mut head)
        .map_err(|e| CoreError::io(format!("Не вдалося прочитати перші 64 КіБ: {e}")))?;

    let tail_len = chunk;
    let start = size.saturating_sub(tail_len as u64);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| CoreError::io(format!("Не вдалося перейти до кінця файла: {e}")))?;
    let mut tail = vec![0u8; tail_len];
    file.read_exact(&mut tail)
        .map_err(|e| CoreError::io(format!("Не вдалося прочитати останні 64 КіБ: {e}")))?;

    let bytes_read = (head.len() + tail.len()) as u64;
    debug_assert!(bytes_read <= PARTIAL_HASH_MAX_READ_BYTES);
    Ok((head, tail, bytes_read))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use trashradar_app::ports::Hasher;
    use trashradar_domain::candidate::ByteSize;
    use trashradar_domain::error::ErrorCode;

    fn write_temp(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn same_content_same_partial_hash() {
        let a = write_temp(b"hello-duplicate-payload-xxxx");
        let b = write_temp(b"hello-duplicate-payload-xxxx");
        let size = ByteSize(a.as_file().metadata().unwrap().len());
        let ha = FileHasher
            .partial_hash(a.path().to_str().unwrap(), size)
            .unwrap();
        let hb = FileHasher
            .partial_hash(b.path().to_str().unwrap(), size)
            .unwrap();
        assert_eq!(ha, hb);
    }

    #[test]
    fn different_content_different_partial_hash() {
        let a = write_temp(b"aaaaaaaaaaaaaaaa");
        let b = write_temp(b"bbbbbbbbbbbbbbbb");
        let sa = ByteSize(16);
        let sb = ByteSize(16);
        let ha = FileHasher
            .partial_hash(a.path().to_str().unwrap(), sa)
            .unwrap();
        let hb = FileHasher
            .partial_hash(b.path().to_str().unwrap(), sb)
            .unwrap();
        assert_ne!(ha, hb);
    }

    #[test]
    fn large_file_reads_at_most_128kib() {
        let size = 1024 * 1024u64;
        let mut data = vec![0u8; size as usize];
        data[..64].fill(0xAB);
        data[size as usize - 64..].fill(0xCD);
        let f = write_temp(&data);
        let (hash, bytes_read) = partial_hash_path(f.path(), size).unwrap();
        assert!(bytes_read <= PARTIAL_HASH_MAX_READ_BYTES);
        assert_eq!(bytes_read, PARTIAL_HASH_MAX_READ_BYTES);
        assert_ne!(hash, PartialHash::ZERO);

        data[size as usize / 2] = 0xFF;
        let f2 = write_temp(&data);
        let (hash2, _) = partial_hash_path(f2.path(), size).unwrap();
        assert_eq!(hash, hash2);

        data[size as usize - 1] = 0x11;
        let f3 = write_temp(&data);
        let (hash3, _) = partial_hash_path(f3.path(), size).unwrap();
        assert_ne!(hash, hash3);
    }

    #[test]
    fn small_file_reads_whole_once() {
        let data = vec![7u8; 1000];
        let f = write_temp(&data);
        let mut file = File::open(f.path()).unwrap();
        let (head, tail, n) = read_partial_chunks(&mut file, 1000).unwrap();
        assert_eq!(n, 1000);
        assert_eq!(head.len(), 1000);
        assert!(tail.is_empty());
        assert!(n <= PARTIAL_HASH_MAX_READ_BYTES);
    }

    #[test]
    fn mid_size_file_first_and_last_64k() {
        let size = 100 * 1024u64;
        let mut data = vec![0u8; size as usize];
        data[0] = 1;
        data[size as usize - 1] = 2;
        let f = write_temp(&data);
        let mut file = File::open(f.path()).unwrap();
        let (head, tail, n) = read_partial_chunks(&mut file, size).unwrap();
        assert_eq!(head.len(), 64 * 1024);
        assert_eq!(tail.len(), 64 * 1024);
        assert_eq!(n, PARTIAL_HASH_MAX_READ_BYTES);
        assert_eq!(head[0], 1);
        assert_eq!(tail[tail.len() - 1], 2);
    }

    #[test]
    fn missing_file_is_io_error() {
        let err = FileHasher
            .partial_hash(r"C:\no\such\trashradar-hash-test.bin", ByteSize(10))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
    }

    #[test]
    fn full_hash_matches_blake3_of_entire_file() {
        // Multi-buffer size so we exercise the stream loop.
        let size = FULL_HASH_STREAM_BUFFER_BYTES * 3 + 123;
        let mut data = vec![0u8; size];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let f = write_temp(&data);
        let expected = ContentHash(*blake3::hash(&data).as_bytes());
        let (got, bytes_read) =
            full_hash_path(f.path(), size as u64, &CancellationToken::new()).unwrap();
        assert_eq!(got, expected);
        assert_eq!(bytes_read, size as u64);
        // Buffer is fixed constant (DoD constant memory) — not size of file.
        assert!(FULL_HASH_STREAM_BUFFER_BYTES < size);
        assert_eq!(FULL_HASH_STREAM_BUFFER_BYTES, 256 * 1024);
    }

    #[test]
    fn full_hash_identical_files_equal() {
        let payload = b"full-hash-duplicate-body-0123456789";
        let a = write_temp(payload);
        let b = write_temp(payload);
        let size = ByteSize(payload.len() as u64);
        let cancel = CancellationToken::new();
        let ha = FileHasher
            .full_hash(a.path().to_str().unwrap(), size, &cancel)
            .unwrap();
        let hb = FileHasher
            .full_hash(b.path().to_str().unwrap(), size, &cancel)
            .unwrap();
        assert_eq!(ha, hb);
    }

    #[test]
    fn full_hash_differs_when_middle_differs() {
        // Partial would match (same head/tail); full must differ — cascade value.
        let size = 1024 * 1024usize;
        let mut a = vec![0u8; size];
        let mut b = vec![0u8; size];
        a[..64].fill(1);
        b[..64].fill(1);
        a[size - 64..].fill(2);
        b[size - 64..].fill(2);
        a[size / 2] = 0xAA;
        b[size / 2] = 0xBB;
        let fa = write_temp(&a);
        let fb = write_temp(&b);
        let cancel = CancellationToken::new();
        let (pa, _) = partial_hash_path(fa.path(), size as u64).unwrap();
        let (pb, _) = partial_hash_path(fb.path(), size as u64).unwrap();
        assert_eq!(pa, pb, "partial ignores middle");
        let ha = FileHasher
            .full_hash(fa.path().to_str().unwrap(), ByteSize(size as u64), &cancel)
            .unwrap();
        let hb = FileHasher
            .full_hash(fb.path().to_str().unwrap(), ByteSize(size as u64), &cancel)
            .unwrap();
        assert_ne!(ha, hb, "full hash sees middle");
    }

    #[test]
    fn full_hash_respects_cancel() {
        let size = FULL_HASH_STREAM_BUFFER_BYTES * 4;
        let data = vec![9u8; size];
        let f = write_temp(&data);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = full_hash_path(f.path(), size as u64, &cancel).unwrap_err();
        assert_eq!(err.code, ErrorCode::Cancelled);
    }
}
