//! T-070: генерація мініатюр зображень — декодування з даунскейлом.
//!
//! Третя ланка ланцюжка превью (architecture.md §5.2): якщо дисковий кеш
//! (T-068) і системний кеш мініатюр (T-069) не дали результату, зображення
//! декодується власне — через Windows Imaging Component (WIC), нативний
//! стек кодеків ОС. Це покриває jpg/png/bmp/gif/tiff одразу та webp/heic
//! через системні кодек-пакети — без зовнішніх крейтів-декодерів.
//!
//! Даунскейл робить `IWICBitmapScaler` (Fant) ще на етапі декодування:
//! вихідний розмір обмежений `max_edge` конструктивно — буфер результату
//! ніколи не перевищує `max_edge² × 4` байтів, хай яким великим є джерело.

use trashradar_app::ports::{RawThumbnail, ThumbnailSource};
use trashradar_domain::error::CoreError;

/// Джерело мініатюр із власним декодуванням зображень (T-070).
///
/// Контракт ланцюжка (як у T-069): `Ok(None)` — джерело не покриває файл
/// (не зображення, немає кодека, файл бітий/недоступний) → викликач іде
/// до наступної ланки або рендерить типізовану плитку. `Err` — лише
/// некоректні аргументи чи збій COM-інфраструктури.
#[derive(Debug, Default, Clone, Copy)]
pub struct ImageThumbnailSource;

impl ImageThumbnailSource {
    /// Створити джерело.
    pub fn new() -> Self {
        Self
    }
}

impl ThumbnailSource for ImageThumbnailSource {
    fn thumbnail(&self, path: &str, max_edge: u32) -> Result<Option<RawThumbnail>, CoreError> {
        if path.is_empty() {
            return Err(CoreError::invalid_argument(
                "Порожній шлях до файла превью.",
            ));
        }
        let edge = max_edge.max(1);
        #[cfg(windows)]
        {
            win::decode_thumbnail(path, edge)
        }
        #[cfg(not(windows))]
        {
            // Декодування через WIC — можливість лише Windows; на інших
            // платформах джерело нічого не покриває (ланцюжок піде далі).
            let _ = (path, edge);
            Ok(None)
        }
    }
}

/// Вписати розмір у квадрат `max_edge` зі збереженням пропорцій.
/// Лише даунскейл: менше зображення не розтягується. Мінімум 1 піксель.
fn fit_within(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    if width <= max_edge && height <= max_edge {
        return (width, height);
    }
    if width >= height {
        let scaled = ((height as u64 * max_edge as u64) / width as u64).max(1) as u32;
        (max_edge, scaled)
    } else {
        let scaled = ((width as u64 * max_edge as u64) / height as u64).max(1) as u32;
        (scaled, max_edge)
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    use trashradar_app::ports::RawThumbnail;
    use trashradar_domain::error::CoreError;

    use crate::com::{CoCreateInstance, ComApartment, Guid, CLSCTX_INPROC_SERVER};

    /// `CLSID_WICImagingFactory` — {cacaf262-9370-4615-a13b-9f5539da4c0a}.
    const CLSID_IMAGING_FACTORY: Guid = Guid {
        data1: 0xcaca_f262,
        data2: 0x9370,
        data3: 0x4615,
        data4: [0xa1, 0x3b, 0x9f, 0x55, 0x39, 0xda, 0x4c, 0x0a],
    };

    /// `IID_IWICImagingFactory` — {ec5ec8a9-c395-4314-9c77-54d7a935ff70}.
    const IID_IMAGING_FACTORY: Guid = Guid {
        data1: 0xec5e_c8a9,
        data2: 0xc395,
        data3: 0x4314,
        data4: [0x9c, 0x77, 0x54, 0xd7, 0xa9, 0x35, 0xff, 0x70],
    };

    /// `GUID_WICPixelFormat32bppBGRA` — {6fddc324-4e03-4bfe-b185-3d77768dc90f}.
    const PIXEL_FORMAT_32BPP_BGRA: Guid = Guid {
        data1: 0x6fdd_c324,
        data2: 0x4e03,
        data3: 0x4bfe,
        data4: [0xb1, 0x85, 0x3d, 0x77, 0x76, 0x8d, 0xc9, 0x0f],
    };

    /// `GENERIC_READ` для відкриття файла декодером.
    const GENERIC_READ: u32 = 0x8000_0000;
    /// `WICDecodeMetadataCacheOnDemand` — метадані не читаються наперед.
    const DECODE_METADATA_CACHE_ON_DEMAND: u32 = 0;
    /// `WICBitmapInterpolationModeFant` — якісний даунскейл.
    const INTERPOLATION_FANT: u32 = 3;
    /// `WICBitmapDitherTypeNone`.
    const DITHER_NONE: u32 = 0;
    /// `WICBitmapPaletteTypeCustom` (палітра не використовується).
    const PALETTE_CUSTOM: u32 = 0;

    /// Заголовок будь-якого COM-об'єкта: перший вказівник — на vtable.
    #[repr(C)]
    struct ComObject {
        vtbl: *const c_void,
    }

    /// Прочитати vtable COM-об'єкта як конкретний тип.
    unsafe fn vtbl<T>(obj: *mut c_void) -> *const T {
        (*(obj as *const ComObject)).vtbl as *const T
    }

    /// Початок кожної vtable — `IUnknown`; для release достатньо цього зрізу.
    #[repr(C)]
    struct UnknownVtbl {
        query_interface: usize,
        add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
    }

    /// RAII-власник COM-вказівника: release при виході з області видимості.
    struct ComPtr(*mut c_void);

    impl ComPtr {
        fn get(&self) -> *mut c_void {
            self.0
        }
    }

    impl Drop for ComPtr {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { ((*vtbl::<UnknownVtbl>(self.0)).release)(self.0) };
            }
        }
    }

    /// vtable `IWICImagingFactory` (потрібні слоти; решта — заглушки-вказівники,
    /// порядок методів — за wincodec.h).
    #[repr(C)]
    struct ImagingFactoryVtbl {
        query_interface: usize,
        add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        create_decoder_from_filename: unsafe extern "system" fn(
            *mut c_void,
            *const u16,
            *const Guid,
            u32,
            u32,
            *mut *mut c_void,
        ) -> i32,
        create_decoder_from_stream: usize,
        create_decoder_from_file_handle: usize,
        create_component_info: usize,
        create_decoder: usize,
        create_encoder: usize,
        create_palette: usize,
        create_format_converter: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
        create_bitmap_scaler: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
        // решта методів фабрики не потрібні
    }

    /// vtable `IWICBitmapDecoder` (потрібен лише `GetFrame`).
    #[repr(C)]
    struct BitmapDecoderVtbl {
        query_interface: usize,
        add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        query_capability: usize,
        initialize: usize,
        get_container_format: usize,
        get_decoder_info: usize,
        copy_palette: usize,
        get_metadata_query_reader: usize,
        get_preview: usize,
        get_color_contexts: usize,
        get_thumbnail: usize,
        get_frame_count: usize,
        get_frame: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32,
    }

    /// Прямокутник WIC для `CopyPixels`.
    #[repr(C)]
    struct WicRect {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    }

    /// vtable `IWICBitmapSource` — спільний початок frame/scaler/converter.
    #[repr(C)]
    struct BitmapSourceVtbl {
        query_interface: usize,
        add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        get_size: unsafe extern "system" fn(*mut c_void, *mut u32, *mut u32) -> i32,
        get_pixel_format: usize,
        get_resolution: usize,
        copy_palette: usize,
        copy_pixels:
            unsafe extern "system" fn(*mut c_void, *const WicRect, u32, u32, *mut u8) -> i32,
    }

    /// vtable `IWICBitmapScaler` = `IWICBitmapSource` + `Initialize`.
    #[repr(C)]
    struct BitmapScalerVtbl {
        base: BitmapSourceVtbl,
        initialize: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32, u32) -> i32,
    }

    /// vtable `IWICFormatConverter` = `IWICBitmapSource` + `Initialize` (+ `CanConvert`).
    #[repr(C)]
    struct FormatConverterVtbl {
        base: BitmapSourceVtbl,
        initialize: unsafe extern "system" fn(
            *mut c_void,
            *mut c_void,
            *const Guid,
            u32,
            *mut c_void,
            f64,
            u32,
        ) -> i32,
        can_convert: usize,
    }

    fn wide(path: &str) -> Vec<u16> {
        std::ffi::OsStr::new(path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Декодувати зображення у мініатюру ≤ `max_edge`. `Ok(None)` — файл
    /// не покривається WIC (не зображення / немає кодека / бітий).
    pub fn decode_thumbnail(path: &str, max_edge: u32) -> Result<Option<RawThumbnail>, CoreError> {
        let _apartment = ComApartment::enter()?;
        unsafe { decode(path, max_edge) }
    }

    /// Внутрішнє декодування (COM уже ініціалізовано на потоці).
    unsafe fn decode(path: &str, max_edge: u32) -> Result<Option<RawThumbnail>, CoreError> {
        // Фабрика WIC — інфраструктура; її відсутність — реальний збій.
        let mut factory_ptr: *mut c_void = std::ptr::null_mut();
        let hr = CoCreateInstance(
            &CLSID_IMAGING_FACTORY,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_IMAGING_FACTORY,
            &mut factory_ptr,
        );
        if hr < 0 || factory_ptr.is_null() {
            return Err(CoreError::io(format!(
                "WIC-фабрика недоступна (HRESULT {hr:#010x})."
            )));
        }
        let factory = ComPtr(factory_ptr);
        let factory_vtbl = vtbl::<ImagingFactoryVtbl>(factory.get());

        // Декодер за вмістом файла: не зображення / бітий / недоступний →
        // джерело не покриває цей файл.
        let wpath = wide(path);
        let mut decoder_ptr: *mut c_void = std::ptr::null_mut();
        let hr = ((*factory_vtbl).create_decoder_from_filename)(
            factory.get(),
            wpath.as_ptr(),
            std::ptr::null(),
            GENERIC_READ,
            DECODE_METADATA_CACHE_ON_DEMAND,
            &mut decoder_ptr,
        );
        if hr < 0 || decoder_ptr.is_null() {
            return Ok(None);
        }
        let decoder = ComPtr(decoder_ptr);

        // Перший кадр (для багатокадрових gif/tiff мініатюра — кадр 0).
        let mut frame_ptr: *mut c_void = std::ptr::null_mut();
        let hr = ((*vtbl::<BitmapDecoderVtbl>(decoder.get())).get_frame)(
            decoder.get(),
            0,
            &mut frame_ptr,
        );
        if hr < 0 || frame_ptr.is_null() {
            return Ok(None);
        }
        let frame = ComPtr(frame_ptr);

        let (mut src_w, mut src_h) = (0u32, 0u32);
        let hr = ((*vtbl::<BitmapSourceVtbl>(frame.get())).get_size)(
            frame.get(),
            &mut src_w,
            &mut src_h,
        );
        if hr < 0 || src_w == 0 || src_h == 0 {
            return Ok(None);
        }

        // Даунскейл на етапі декодування: результат обмежений max_edge
        // конструктивно, вихідний розмір джерела на буфер не впливає.
        let (out_w, out_h) = super::fit_within(src_w, src_h, max_edge);
        let mut scaler_guard: Option<ComPtr> = None;
        let source_ptr = if (out_w, out_h) != (src_w, src_h) {
            let mut scaler_ptr: *mut c_void = std::ptr::null_mut();
            let hr = ((*factory_vtbl).create_bitmap_scaler)(factory.get(), &mut scaler_ptr);
            if hr < 0 || scaler_ptr.is_null() {
                return Ok(None);
            }
            let scaler = ComPtr(scaler_ptr);
            let hr = ((*vtbl::<BitmapScalerVtbl>(scaler.get())).initialize)(
                scaler.get(),
                frame.get(),
                out_w,
                out_h,
                INTERPOLATION_FANT,
            );
            if hr < 0 {
                return Ok(None);
            }
            let ptr = scaler.get();
            scaler_guard = Some(scaler);
            ptr
        } else {
            frame.get()
        };
        let _ = &scaler_guard; // тримає scaler живим до кінця decode

        // Конвертація у BGRA (формат RawThumbnail незалежно від джерела).
        let mut converter_ptr: *mut c_void = std::ptr::null_mut();
        let hr = ((*factory_vtbl).create_format_converter)(factory.get(), &mut converter_ptr);
        if hr < 0 || converter_ptr.is_null() {
            return Ok(None);
        }
        let converter = ComPtr(converter_ptr);
        let hr = ((*vtbl::<FormatConverterVtbl>(converter.get())).initialize)(
            converter.get(),
            source_ptr,
            &PIXEL_FORMAT_32BPP_BGRA,
            DITHER_NONE,
            std::ptr::null_mut(),
            0.0,
            PALETTE_CUSTOM,
        );
        if hr < 0 {
            return Ok(None);
        }

        // CopyPixels на весь бітмап (`prc = NULL`) у top-down BGRA.
        let stride = out_w * 4;
        let byte_len = (stride as usize) * (out_h as usize);
        let mut buffer = vec![0u8; byte_len];
        let hr = ((*vtbl::<BitmapSourceVtbl>(converter.get())).copy_pixels)(
            converter.get(),
            std::ptr::null(),
            stride,
            byte_len as u32,
            buffer.as_mut_ptr(),
        );
        if hr < 0 {
            return Ok(None);
        }

        Ok(Some(RawThumbnail {
            width: out_w,
            height: out_h,
            bgra: buffer,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_path_is_invalid_argument() {
        let src = ImageThumbnailSource::new();
        let err = src.thumbnail("", 128).expect_err("порожній шлях = помилка");
        assert_eq!(
            err.code,
            trashradar_domain::error::ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn fit_within_downscales_landscape_preserving_aspect() {
        assert_eq!(fit_within(400, 200, 100), (100, 50));
    }

    #[test]
    fn fit_within_downscales_portrait_preserving_aspect() {
        assert_eq!(fit_within(200, 400, 100), (50, 100));
    }

    #[test]
    fn fit_within_never_upscales() {
        assert_eq!(fit_within(30, 20, 128), (30, 20));
    }

    #[test]
    fn fit_within_extreme_aspect_keeps_at_least_one_pixel() {
        assert_eq!(fit_within(100_000, 1, 64), (64, 1));
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_source_is_transparent() {
        let src = ImageThumbnailSource::new();
        let out = src.thumbnail("/tmp/whatever.png", 128).expect("no error");
        assert!(out.is_none());
    }

    #[cfg(windows)]
    mod windows_decode {
        use super::super::*;
        use crate::test_media::{make_bmp, make_png};
        use std::path::PathBuf;

        fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
            let dir = std::env::temp_dir().join("tr_t070_decode");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }

        /// DoD: поширений формат (BMP) дає мініатюру; вихідний розмір обмежений.
        #[test]
        fn bmp_decodes_with_downscale_and_correct_color() {
            let path = temp_file("solid_256.bmp", &make_bmp(256, 256, [40, 90, 200]));
            let src = ImageThumbnailSource::new();
            let thumb = src
                .thumbnail(&path.to_string_lossy(), 64)
                .expect("decode call")
                .expect("BMP має декодуватися");

            assert_eq!((thumb.width, thumb.height), (64, 64));
            assert_eq!(thumb.bgra.len(), 64 * 64 * 4);
            // Суцільний колір переживає даунскейл: BGRA = (B,G,R,255).
            assert_eq!(&thumb.bgra[0..4], &[40, 90, 200, 255]);
            let mid = ((32 * 64 + 32) * 4) as usize;
            assert_eq!(&thumb.bgra[mid..mid + 4], &[40, 90, 200, 255]);
        }

        /// DoD: PNG (несуцільний контейнер зі стисненням) дає мініатюру
        /// з пропорційним даунскейлом; колір RGB → BGRA конвертовано вірно.
        #[test]
        fn png_decodes_and_preserves_color() {
            // Червоний RGB(255,0,0) → у BGRA має стати (0,0,255,255).
            let path = temp_file("solid_red_96x48.png", &make_png(96, 48, [255, 0, 0]));
            let src = ImageThumbnailSource::new();
            let thumb = src
                .thumbnail(&path.to_string_lossy(), 64)
                .expect("decode call")
                .expect("PNG має декодуватися");

            assert_eq!((thumb.width, thumb.height), (64, 32));
            assert_eq!(&thumb.bgra[0..4], &[0, 0, 255, 255]);
        }

        /// Менше за max_edge зображення не розтягується (лише даунскейл).
        #[test]
        fn small_image_is_not_upscaled() {
            let path = temp_file("small_16.bmp", &make_bmp(16, 16, [10, 20, 30]));
            let src = ImageThumbnailSource::new();
            let thumb = src
                .thumbnail(&path.to_string_lossy(), 128)
                .expect("decode call")
                .expect("BMP має декодуватися");
            assert_eq!((thumb.width, thumb.height), (16, 16));
        }

        /// Не-зображення → `Ok(None)` (джерело не покриває), не помилка.
        #[test]
        fn non_image_returns_none_not_error() {
            let path = temp_file("not_an_image.txt", b"just some text, no pixels here");
            let src = ImageThumbnailSource::new();
            let out = src
                .thumbnail(&path.to_string_lossy(), 64)
                .expect("не-зображення не має бути помилкою");
            assert!(out.is_none());
        }

        /// Неіснуючий файл → `Ok(None)`, скан/конвеєр не падає.
        #[test]
        fn nonexistent_file_returns_none_not_error() {
            let src = ImageThumbnailSource::new();
            let out = src
                .thumbnail(r"C:\this\path\does\not\exist_ktr70.png", 64)
                .expect("неіснуючий файл не має бути помилкою");
            assert!(out.is_none());
        }
    }
}
