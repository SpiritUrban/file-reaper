//! Тестові генератори мінімальних валідних зображень (лише для тестів).
//!
//! Дають детерміновані суцільні картинки без залежностей на кодеки:
//! BMP — сирий 24bpp; PNG — stored-deflate (без стиснення) з власними
//! CRC32/Adler32. Використовуються тестами T-069 (системні мініатюри)
//! та T-070 (декодування з даунскейлом).

/// Згенерувати суцільний 24-бітний BMP (BGR) заданого розміру.
pub fn make_bmp(width: u32, height: u32, bgr: [u8; 3]) -> Vec<u8> {
    let row_stride = (width * 3).div_ceil(4) * 4;
    let pixel_bytes = (row_stride * height) as usize;
    let file_size = 54 + pixel_bytes;
    let mut out = Vec::with_capacity(file_size);
    // BITMAPFILEHEADER
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    out.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset
                                                 // BITMAPINFOHEADER
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&24u16.to_le_bytes()); // bit count
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes()); // x ppm (~72 dpi)
    out.extend_from_slice(&2835i32.to_le_bytes()); // y ppm
    out.extend_from_slice(&0u32.to_le_bytes()); // clr used
    out.extend_from_slice(&0u32.to_le_bytes()); // clr important
                                                // Пікселі (bottom-up), з паддінгом рядків
    let pad = (row_stride - width * 3) as usize;
    for _ in 0..height {
        for _ in 0..width {
            out.extend_from_slice(&bgr);
        }
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    out
}

/// Згенерувати суцільний 8-бітний RGB PNG заданого розміру.
///
/// PNG-примітиви (stored-deflate, CRC32/Adler32) — спільні з робочим
/// кодувальником `encode.rs` (T-073); тут лише збірка RGB-варіанта.
pub fn make_png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    use crate::encode::{png_chunk, zlib_stored, PNG_SIGNATURE};

    // Сирі скан-лінії: filter 0 + RGB-пікселі.
    let mut scanline = Vec::with_capacity(1 + 3 * width as usize);
    scanline.push(0u8);
    for _ in 0..width {
        scanline.extend_from_slice(&rgb);
    }
    let mut raw = Vec::with_capacity((height as usize) * scanline.len());
    for _ in 0..height {
        raw.extend_from_slice(&scanline);
    }

    let mut out = Vec::new();
    out.extend_from_slice(PNG_SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    // 8 біт на канал, колір RGB (2), deflate, filter 0, без interlace.
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut out, b"IEND", &[]);
    out
}
