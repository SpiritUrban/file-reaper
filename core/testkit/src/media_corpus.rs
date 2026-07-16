//! Генератор корпусу битих і екзотичних медіафайлів (T-155).
//!
//! Конвеєр превью декодує найнестабільніший вхід у системі (architecture.md
//! §5.4), тож потрібен відтворюваний корпус, на якому доводиться інваріант:
//! **жоден файл не валить процес, будь-який збій деградує у «превью
//! недоступне»**. Корпус будується байт-у-байт детерміновано, без зовнішніх
//! крейтів і без кодеків: усі варіанти — це навмисно зіпсовані заголовки,
//! обрізані потоки та сміття під медійними розширеннями.
//!
//! CRC32 тут — частина **генератора тестових даних** (валідний заголовок PNG,
//! за яким іде побитий потік, — найцінніший випадок корпусу: декодер доходить
//! до пікселів, а не відсіює файл на сигнатурі). Кодувальник превью має власний
//! CRC32, але `testkit` не може від нього залежати: `preview` тягне `testkit`
//! як dev-залежність — це був би цикл (docs/repository.md §10).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Один файл корпусу.
#[derive(Debug, Clone)]
pub struct CorpusFile {
    /// Повний шлях до згенерованого файла.
    pub path: PathBuf,
    /// Чим саме файл поламаний або екзотичний (для повідомлень тесту).
    pub label: &'static str,
}

impl CorpusFile {
    /// Шлях рядком — у такому вигляді його приймають порти превью.
    pub fn path_str(&self) -> String {
        self.path.to_string_lossy().to_string()
    }
}

/// Корпус битих/екзотичних медіафайлів у власному каталозі.
///
/// Каталог створюється в [`MediaCorpus::generate`] і видаляється разом зі
/// структурою — тест не лишає сміття навіть після паніки.
#[derive(Debug)]
pub struct MediaCorpus {
    root: PathBuf,
    files: Vec<CorpusFile>,
}

impl MediaCorpus {
    /// Згенерувати корпус у каталозі `root` (створюється, якщо немає).
    pub fn generate(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let mut files = Vec::new();
        for (name, label, bytes) in specimens() {
            let path = root.join(name);
            fs::write(&path, &bytes)?;
            files.push(CorpusFile { path, label });
        }
        Ok(Self { root, files })
    }

    /// Згенерувати корпус у тимчасовому каталозі з унікальним іменем.
    pub fn generate_temp(tag: &str) -> io::Result<Self> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Self::generate(std::env::temp_dir().join(format!("tr_corpus_{tag}_{unique}")))
    }

    /// Усі файли корпусу.
    pub fn files(&self) -> &[CorpusFile] {
        &self.files
    }

    /// Каталог корпусу.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for MediaCorpus {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Перелік зразків корпусу: `(ім'я файла, опис вади, вміст)`.
///
/// Покриття: порожні файли, самі лише сигнатури, валідний заголовок з обрізаним
/// потоком, брехливі розміри в заголовку, зіпсована контрольна сума, сміття під
/// медійним розширенням, розбіжність вмісту й розширення, екзотичні контейнери
/// та екзотичні імена файлів.
fn specimens() -> Vec<(String, &'static str, Vec<u8>)> {
    let mut out: Vec<(String, &'static str, Vec<u8>)> = vec![
        // --- Порожні файли -------------------------------------------------
        ("empty.jpg".into(), "порожній файл (0 байтів)", Vec::new()),
        (
            "empty.mp4".into(),
            "порожній файл під розширенням відео",
            Vec::new(),
        ),
        // --- Самі лише сигнатури -------------------------------------------
        (
            "signature_only.png".into(),
            "лише PNG-сигнатура, жодного чанка",
            PNG_SIGNATURE.to_vec(),
        ),
        (
            "signature_only.gif".into(),
            "лише сигнатура GIF89a",
            b"GIF89a".to_vec(),
        ),
        (
            "soi_only.jpg".into(),
            "лише JPEG SOI + обрив",
            vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00],
        ),
        // --- Валідний заголовок, побитий потік -----------------------------
        (
            "truncated_idat.png".into(),
            "валідний IHDR 64×64, IDAT обірваний посеред потоку",
            truncated_idat_png(64, 64),
        ),
        (
            "corrupt_crc.png".into(),
            "PNG з навмисно зіпсованою контрольною сумою IHDR",
            corrupt_crc_png(),
        ),
        (
            "zero_dimensions.png".into(),
            "PNG із заявленим розміром 0×0",
            zero_dimension_png(),
        ),
        (
            "huge_dimensions.png".into(),
            "PNG заявляє 30000×30000, даних майже немає",
            truncated_idat_png(30_000, 30_000),
        ),
        (
            "truncated_jfif.jpg".into(),
            "JFIF-заголовок + сміття замість сканів, без EOI",
            truncated_jfif(),
        ),
        (
            "header_only.bmp".into(),
            "BMP заявляє 8000×8000, пікселів немає",
            bmp_header(8_000, 8_000, 24, 0),
        ),
        (
            "bogus_fields.bmp".into(),
            "BMP з від'ємною шириною, 7 біт/піксель і невідомим стисненням",
            bmp_header(-1, 8, 7, 99),
        ),
        // --- Сміття під медійним розширенням -------------------------------
        (
            "random.png".into(),
            "4 КіБ сміття під розширенням .png",
            garbage(0xA11CE, 4096),
        ),
        (
            "garbage.tiff".into(),
            "TIFF-сигнатура + сміття",
            with_prefix(b"II\x2A\x00", 0xB0B, 512),
        ),
        (
            "truncated.webp".into(),
            "RIFF/WEBP заявляє ~4 ГіБ, обрив одразу після заголовка",
            truncated_webp(),
        ),
        (
            "garbage.heic".into(),
            "ftyp heic + сміття замість зображення",
            with_prefix(&ftyp_box(b"heic"), 0xC0FFEE, 256),
        ),
        (
            "garbage.avif".into(),
            "ftyp avif + сміття замість зображення",
            with_prefix(&ftyp_box(b"avif"), 0xDEAD, 256),
        ),
        (
            "text_named.mp4".into(),
            "звичайний текст під розширенням відео",
            b"Not a video. Just a note the user renamed by accident.".to_vec(),
        ),
        (
            "garbage.mkv".into(),
            "EBML-сигнатура + сміття",
            with_prefix(&[0x1A, 0x45, 0xDF, 0xA3], 0xBEEF, 512),
        ),
        (
            "garbage.avi".into(),
            "RIFF/AVI-заголовок + сміття",
            with_prefix(b"RIFF\x20\x00\x00\x00AVI LIST", 0xFACE, 512),
        ),
        (
            "garbage.cr2".into(),
            "сирий формат фотокамери — сміття",
            with_prefix(b"II\x2A\x00\x10\x00\x00\x00CR", 0x5A5A, 384),
        ),
        // --- Побиті контейнери відео ---------------------------------------
        (
            "ftyp_only.mp4".into(),
            "лише ftyp-бокс, без moov і mdat",
            ftyp_box(b"isom"),
        ),
        (
            "truncated_moov.mp4".into(),
            "moov-бокс заявляє 1 МіБ, у файлі 64 байти",
            truncated_moov_mp4(),
        ),
        (
            "self_declared_huge.mp4".into(),
            "бокс заявляє розмір 0xFFFFFFFF",
            self_declared_huge_mp4(),
        ),
        // --- Екзотика імен і розширень --------------------------------------
        (
            "no_extension".into(),
            "медійне сміття без розширення",
            garbage(0x1234, 256),
        ),
        (
            "double_extension.jpg.mp4".into(),
            "подвійне розширення, вміст не відповідає жодному",
            with_prefix(PNG_SIGNATURE, 0x77, 128),
        ),
        (
            "відео_🎬_екзотика.mp4".into(),
            "не-ASCII ім'я файла (кирилиця + емодзі)",
            garbage(0x9999, 256),
        ),
    ];

    // Довге ім'я файла (у межах компонента шляху NTFS — 255 символів).
    let long_name = format!("{}.jpg", "l".repeat(150));
    out.push((long_name, "дуже довге ім'я файла", garbage(0x4242, 128)));
    out
}

// --- Байтові будівники ------------------------------------------------------

const PNG_SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Детерміноване «сміття»: LCG замість генератора випадкових чисел, щоб
/// корпус був байт-у-байт відтворюваним між прогонами й машинами.
fn garbage(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}

fn with_prefix(prefix: &[u8], seed: u64, garbage_len: usize) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(&garbage(seed, garbage_len));
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Зібрати PNG-чанк: довжина, тип, дані, CRC32(тип‖дані).
fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// IHDR-дані: розмір, 8 біт/канал, RGB, deflate, filter 0, без interlace.
fn ihdr_data(width: u32, height: u32) -> Vec<u8> {
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    ihdr
}

/// PNG з валідним IHDR і IDAT, що заявляє 4 КіБ даних, але обривається на 32
/// байтах сміття: декодер доходить до розпакування пікселів і падає там.
fn truncated_idat_png(width: u32, height: u32) -> Vec<u8> {
    let mut out = PNG_SIGNATURE.to_vec();
    png_chunk(&mut out, b"IHDR", &ihdr_data(width, height));
    out.extend_from_slice(&4096u32.to_be_bytes());
    out.extend_from_slice(b"IDAT");
    out.extend_from_slice(&[0x78, 0x9C]); // заголовок zlib
    out.extend_from_slice(&garbage(0xDA7A, 30));
    out
}

/// PNG, у якого структура валідна, а контрольна сума IHDR — ні.
fn corrupt_crc_png() -> Vec<u8> {
    let mut out = PNG_SIGNATURE.to_vec();
    png_chunk(&mut out, b"IHDR", &ihdr_data(32, 32));
    let crc_start = out.len() - 4;
    out[crc_start] ^= 0xFF;
    png_chunk(&mut out, b"IEND", &[]);
    out
}

fn zero_dimension_png() -> Vec<u8> {
    let mut out = PNG_SIGNATURE.to_vec();
    png_chunk(&mut out, b"IHDR", &ihdr_data(0, 0));
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// BITMAPFILEHEADER + BITMAPINFOHEADER із заданими (можливо, брехливими)
/// полями — і без жодного байта пікселів після них.
fn bmp_header(width: i32, height: i32, bpp: u16, compression: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(54);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&0xFFFF_FF00u32.to_le_bytes()); // брехливий розмір файла
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&bpp.to_le_bytes());
    out.extend_from_slice(&compression.to_le_bytes());
    out.extend_from_slice(&0xFFFF_FF00u32.to_le_bytes()); // брехливий розмір даних
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

/// JPEG: SOI + APP0/JFIF + SOF0 (512×512) + сміття замість сканів, без EOI.
fn truncated_jfif() -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    out.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
    out.extend_from_slice(b"JFIF\x00");
    out.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    // SOF0: довжина 17, 8 біт, 512×512, 3 компоненти.
    out.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x02, 0x00, 0x02, 0x00, 0x03]);
    out.extend_from_slice(&[0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
    out.extend_from_slice(&[
        0xFF, 0xDA, 0x00, 0x0C, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11,
    ]);
    out.extend_from_slice(&garbage(0x5EED, 200));
    out
}

fn truncated_webp() -> Vec<u8> {
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&0xFFFF_FFF0u32.to_le_bytes()); // заявлений розмір ~4 ГіБ
    out.extend_from_slice(b"WEBPVP8 ");
    out.extend_from_slice(&0xFFFF_0000u32.to_le_bytes());
    out
}

/// ISO-BMFF бокс `ftyp` із заданим major brand.
fn ftyp_box(brand: &[u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&24u32.to_be_bytes());
    out.extend_from_slice(b"ftyp");
    out.extend_from_slice(brand);
    out.extend_from_slice(&0u32.to_be_bytes()); // minor version
    out.extend_from_slice(brand); // compatible brands
    out.extend_from_slice(b"mp42");
    out
}

fn truncated_moov_mp4() -> Vec<u8> {
    let mut out = ftyp_box(b"isom");
    out.extend_from_slice(&(1024u32 * 1024).to_be_bytes()); // заявлений розмір боксу
    out.extend_from_slice(b"moov");
    out.extend_from_slice(&garbage(0x3B0B, 64));
    out
}

fn self_declared_huge_mp4() -> Vec<u8> {
    let mut out = ftyp_box(b"isom");
    out.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
    out.extend_from_slice(b"mdat");
    out.extend_from_slice(&garbage(0x1B0B, 32));
    out
}
