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
/// IDAT — zlib зі stored-блоками deflate (нуль стиснення): валідний PNG
/// для будь-якого декодера без потреби у справжньому deflate-кодері.
pub fn make_png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
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
    // zlib: заголовок + stored-блоки (максимум 65535 байтів кожен) + Adler32.
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
    idat.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    // 8 біт на канал, колір RGB (2), deflate, filter 0, без interlace.
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &idat);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// Додати PNG-чанк: довжина + тип + дані + CRC32(тип‖дані).
fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// CRC32 (IEEE 802.3, поліном 0xEDB88320) — потоковий, без таблиці.
struct Crc32(u32);

impl Crc32 {
    fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    fn update(&mut self, data: &[u8]) {
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

    fn finish(self) -> u32 {
        !self.0
    }
}

/// Adler32 контрольна сума zlib-потоку.
fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
