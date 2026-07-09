//! T-073: кодування превью у файловий формат дискового кешу (T-068).
//!
//! PNG **без стиснення** (stored-блоки deflate, власні CRC32/Adler32):
//! валідний для будь-якого декодера/webview, нуль зовнішніх залежностей,
//! детермінований і швидкий (без витрат CPU на deflate — превью-файли
//! живуть у LRU-кеші з бюджетом розміру, T-094).

use trashradar_app::ports::{RawThumbnail, ThumbnailEncoder};
use trashradar_domain::error::CoreError;

/// Кодувальник превью у нестиснений PNG (RGBA, 8 біт на канал).
#[derive(Debug, Default, Clone, Copy)]
pub struct PngThumbnailEncoder;

impl PngThumbnailEncoder {
    /// Створити кодувальник.
    pub fn new() -> Self {
        Self
    }
}

impl ThumbnailEncoder for PngThumbnailEncoder {
    fn encode(&self, thumbnail: &RawThumbnail) -> Result<Vec<u8>, CoreError> {
        let expected = (thumbnail.width as usize) * (thumbnail.height as usize) * 4;
        if thumbnail.width == 0 || thumbnail.height == 0 || thumbnail.bgra.len() != expected {
            return Err(CoreError::invalid_argument(format!(
                "Некоректна мініатюра: {}x{}, буфер {} байтів (очікували {expected}).",
                thumbnail.width,
                thumbnail.height,
                thumbnail.bgra.len()
            )));
        }
        Ok(encode_png_rgba(thumbnail))
    }
}

/// Зібрати PNG з BGRA-пікселів: скан-лінії RGBA + zlib stored + чанки.
fn encode_png_rgba(thumbnail: &RawThumbnail) -> Vec<u8> {
    let width = thumbnail.width as usize;
    // Сирі скан-лінії: filter 0 + RGBA (обмін B↔R з BGRA).
    let mut raw = Vec::with_capacity((thumbnail.height as usize) * (1 + width * 4));
    for row in thumbnail.bgra.chunks_exact(width * 4) {
        raw.push(0u8);
        for px in row.chunks_exact(4) {
            raw.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }

    let mut out = Vec::with_capacity(raw.len() + raw.len() / 6_000 + 128);
    out.extend_from_slice(PNG_SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&thumbnail.width.to_be_bytes());
    ihdr.extend_from_slice(&thumbnail.height.to_be_bytes());
    // 8 біт на канал, колір RGBA (6), deflate, filter 0, без interlace.
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// Сигнатура PNG-файла.
pub(crate) const PNG_SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// zlib-потік зі stored-блоками deflate (без стиснення) + Adler32.
pub(crate) fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut idat = vec![0x78, 0x01];
    let mut offset = 0;
    while offset < raw.len() {
        let chunk = (raw.len() - offset).min(65535);
        let last = offset + chunk == raw.len();
        idat.push(if last { 1 } else { 0 }); // BFINAL | BTYPE=00 (stored)
        idat.extend_from_slice(&(chunk as u16).to_le_bytes());
        idat.extend_from_slice(&(!(chunk as u16)).to_le_bytes());
        idat.extend_from_slice(&raw[offset..offset + chunk]);
        offset += chunk;
    }
    idat.extend_from_slice(&adler32(raw).to_be_bytes());
    idat
}

/// Додати PNG-чанк: довжина + тип + дані + CRC32(тип‖дані).
pub(crate) fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// CRC32 (IEEE 802.3, поліном 0xEDB88320) — потоковий, без таблиці.
pub(crate) struct Crc32(u32);

impl Crc32 {
    pub(crate) fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        for &byte in data {
            self.0 ^= byte as u32;
            for _ in 0..8 {
                self.0 = if self.0 & 1 != 0 {
                    (self.0 >> 1) ^ 0xEDB8_8320
                } else {
                    self.0 >> 1
                };
            }
        }
    }

    pub(crate) fn finish(self) -> u32 {
        !self.0
    }
}

/// Adler32 контрольна сума zlib-потоку.
pub(crate) fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_thumbnail() -> RawThumbnail {
        // 2×2, чотири різні пікселі BGRA.
        RawThumbnail {
            width: 2,
            height: 2,
            bgra: vec![
                10, 20, 30, 255, // (0,0)
                40, 50, 60, 255, // (1,0)
                70, 80, 90, 255, // (0,1)
                100, 110, 120, 200, // (1,1) з альфою
            ],
        }
    }

    #[test]
    fn rejects_mismatched_buffer() {
        let bad = RawThumbnail {
            width: 2,
            height: 2,
            bgra: vec![0; 15],
        };
        let err = PngThumbnailEncoder::new()
            .encode(&bad)
            .expect_err("буфер не збігається з геометрією");
        assert_eq!(
            err.code,
            trashradar_domain::error::ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn png_structure_is_valid() {
        let bytes = PngThumbnailEncoder::new()
            .encode(&sample_thumbnail())
            .expect("encode");
        assert_eq!(&bytes[..8], PNG_SIGNATURE);
        // IHDR: довжина 13, ширина/висота BE, RGBA 8 біт.
        assert_eq!(&bytes[8..12], &13u32.to_be_bytes());
        assert_eq!(&bytes[12..16], b"IHDR");
        assert_eq!(&bytes[16..20], &2u32.to_be_bytes());
        assert_eq!(&bytes[20..24], &2u32.to_be_bytes());
        assert_eq!(bytes[24], 8); // біт на канал
        assert_eq!(bytes[25], 6); // RGBA
        assert_eq!(&bytes[bytes.len() - 8..bytes.len() - 4], b"IEND");
    }

    /// Roundtrip проти незалежного декодера (WIC, T-070): закодований PNG
    /// декодується назад у ті самі пікселі BGRA без втрат.
    #[cfg(windows)]
    #[test]
    fn encoded_png_roundtrips_through_wic() {
        use trashradar_app::ports::ThumbnailSource;

        let sample = sample_thumbnail();
        let bytes = PngThumbnailEncoder::new().encode(&sample).expect("encode");

        let dir = std::env::temp_dir().join("tr_t073_encode");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip_2x2.png");
        std::fs::write(&path, &bytes).unwrap();

        let decoded = crate::ImageThumbnailSource::new()
            .thumbnail(&path.to_string_lossy(), 16)
            .expect("decode call")
            .expect("PNG має декодуватися");
        assert_eq!((decoded.width, decoded.height), (2, 2));
        assert_eq!(decoded.bgra, sample.bgra);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
