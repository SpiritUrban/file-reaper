//! Адаптер `PreviewStore`: конвеєр превью (docs/architecture.md §5).
//!
//! Реалізація: E6 (T-067…T-076). Найнестабільніший код системи —
//! декодери працюють за бар'єром ізоляції збоїв (T-074).
//!
//! T-069: перше швидке джерело мініатюр — **системний кеш мініатюр Windows**
//! через Shell API (`IShellItemImageFactory::GetImage`). Для файлів, які
//! користувач колись бачив у Explorer (або має системний thumbnail-провайдер),
//! превью віддається без власного декодування — decode робить ОС/її провайдери.

use trashradar_app::ports::{RawThumbnail, ThumbnailSource};
use trashradar_domain::error::CoreError;

/// Джерело мініатюр на базі системного кешу мініатюр Windows (T-069).
///
/// Використовує `IShellItemImageFactory::GetImage` з прапорцем
/// `SIIGBF_THUMBNAILONLY` — жодних generic-іконок: якщо для файла немає
/// системної мініатюри (або її провайдера), джерело повертає `Ok(None)`,
/// і викликач переходить до наступної ланки ланцюжка (власне декодування,
/// T-070). Так виконується DoD: превью віддається лише коли decode зробила ОС.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsShellThumbnailSource;

impl WindowsShellThumbnailSource {
    /// Створити джерело.
    pub fn new() -> Self {
        Self
    }
}

impl ThumbnailSource for WindowsShellThumbnailSource {
    fn thumbnail(&self, path: &str, max_edge: u32) -> Result<Option<RawThumbnail>, CoreError> {
        if path.is_empty() {
            return Err(CoreError::invalid_argument(
                "Порожній шлях до файла превью.",
            ));
        }
        let edge = max_edge.max(1);
        #[cfg(windows)]
        {
            win::shell_thumbnail(path, edge)
        }
        #[cfg(not(windows))]
        {
            // Системний кеш мініатюр — можливість лише Windows; на інших
            // платформах джерело нічого не покриває (ланцюжок піде далі).
            let _ = (path, edge);
            Ok(None)
        }
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    use trashradar_app::ports::RawThumbnail;
    use trashradar_domain::error::CoreError;

    /// Мінімальний COM GUID.
    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    /// `IShellItemImageFactory` — {bcc18b79-ba16-442f-80c4-8a59c30c463b}.
    const IID_SHELL_ITEM_IMAGE_FACTORY: Guid = Guid {
        data1: 0xbcc1_8b79,
        data2: 0xba16,
        data3: 0x442f,
        data4: [0x80, 0xc4, 0x8a, 0x59, 0xc3, 0x0c, 0x46, 0x3b],
    };

    #[repr(C)]
    struct Size {
        cx: i32,
        cy: i32,
    }

    /// vtable `IShellItemImageFactory` (успадковує `IUnknown`).
    #[repr(C)]
    struct ImageFactoryVtbl {
        query_interface:
            unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        get_image: unsafe extern "system" fn(*mut c_void, Size, u32, *mut *mut c_void) -> i32,
    }

    #[repr(C)]
    struct ImageFactory {
        vtbl: *const ImageFactoryVtbl,
    }

    #[repr(C)]
    struct Bitmap {
        bm_type: i32,
        bm_width: i32,
        bm_height: i32,
        bm_width_bytes: i32,
        bm_planes: u16,
        bm_bits_pixel: u16,
        bm_bits: *mut c_void,
    }

    #[repr(C)]
    struct BitmapInfoHeader {
        bi_size: u32,
        bi_width: i32,
        bi_height: i32,
        bi_planes: u16,
        bi_bit_count: u16,
        bi_compression: u32,
        bi_size_image: u32,
        bi_x_pels_per_meter: i32,
        bi_y_pels_per_meter: i32,
        bi_clr_used: u32,
        bi_clr_important: u32,
    }

    #[repr(C)]
    struct BitmapInfo {
        header: BitmapInfoHeader,
        // Місце під палітру; для 32-бітного BI_RGB не використовується.
        colors: [u32; 3],
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHCreateItemFromParsingName(
            psz_path: *const u16,
            pbc: *mut c_void,
            riid: *const Guid,
            ppv: *mut *mut c_void,
        ) -> i32;
    }

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, co_init: u32) -> i32;
        fn CoUninitialize();
    }

    #[link(name = "gdi32")]
    extern "system" {
        fn GetObjectW(handle: *mut c_void, c: i32, pv: *mut c_void) -> i32;
        fn GetDIBits(
            hdc: *mut c_void,
            hbm: *mut c_void,
            start: u32,
            lines: u32,
            bits: *mut c_void,
            info: *mut BitmapInfo,
            usage: u32,
        ) -> i32;
        fn DeleteObject(handle: *mut c_void) -> i32;
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetDC(hwnd: *mut c_void) -> *mut c_void;
        fn ReleaseDC(hwnd: *mut c_void, hdc: *mut c_void) -> i32;
    }

    /// `COINIT_APARTMENTTHREADED` — модель для Shell thumbnail-провайдерів.
    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    /// `S_OK`.
    const S_OK: i32 = 0;
    /// `S_FALSE` — COM уже ініціалізовано на цьому потоці цією ж моделлю.
    const S_FALSE: i32 = 1;
    /// `RPC_E_CHANGED_MODE` — потік уже в COM з іншою моделлю; працюємо без
    /// власної ініціалізації і не викликаємо `CoUninitialize`.
    const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;
    /// `SIIGBF_RESIZETOFIT` (0) | `SIIGBF_THUMBNAILONLY` (0x8): вписати в розмір,
    /// лише справжні мініатюри — без generic-іконок.
    const SIIGBF_THUMBNAILONLY: u32 = 0x0000_0008;
    /// `DIB_RGB_COLORS`.
    const DIB_RGB_COLORS: u32 = 0;
    /// `BI_RGB`.
    const BI_RGB: u32 = 0;

    fn wide(path: &str) -> Vec<u16> {
        std::ffi::OsStr::new(path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Отримати системну мініатюру файла. `Ok(None)` — системної мініатюри немає.
    pub fn shell_thumbnail(path: &str, max_edge: u32) -> Result<Option<RawThumbnail>, CoreError> {
        unsafe {
            let hr_init = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED);
            // Ми володіємо ініціалізацією (і маємо Uninitialize) лише якщо самі
            // її підняли. RPC_E_CHANGED_MODE — потік уже в COM, працюємо як є.
            let owns_com = hr_init == S_OK || hr_init == S_FALSE;
            if hr_init < 0 && hr_init != RPC_E_CHANGED_MODE {
                return Err(CoreError::io(format!(
                    "CoInitializeEx для thumbnail не вдалося (HRESULT {hr_init:#010x})."
                )));
            }

            let result = extract(path, max_edge);

            if owns_com {
                CoUninitialize();
            }
            result
        }
    }

    /// Внутрішнє видобування (COM уже ініціалізовано на потоці).
    unsafe fn extract(path: &str, max_edge: u32) -> Result<Option<RawThumbnail>, CoreError> {
        let wpath = wide(path);
        let mut factory_ptr: *mut c_void = std::ptr::null_mut();
        let hr = SHCreateItemFromParsingName(
            wpath.as_ptr(),
            std::ptr::null_mut(),
            &IID_SHELL_ITEM_IMAGE_FACTORY,
            &mut factory_ptr,
        );
        if hr < 0 || factory_ptr.is_null() {
            // Файл недоступний / немає shell-елемента — джерело його не покриває.
            return Ok(None);
        }

        let factory = factory_ptr as *mut ImageFactory;
        let vtbl = (*factory).vtbl;
        let size = Size {
            cx: max_edge as i32,
            cy: max_edge as i32,
        };
        let mut hbitmap: *mut c_void = std::ptr::null_mut();
        let hr_img = ((*vtbl).get_image)(factory_ptr, size, SIIGBF_THUMBNAILONLY, &mut hbitmap);

        // Незалежно від результату — звільняємо фабрику.
        ((*vtbl).release)(factory_ptr);

        if hr_img < 0 || hbitmap.is_null() {
            // Немає системної мініатюри для цього файла (THUMBNAILONLY).
            return Ok(None);
        }

        let thumb = read_hbitmap(hbitmap);
        DeleteObject(hbitmap);
        Ok(thumb)
    }

    /// Скопіювати пікселі HBITMAP у top-down BGRA через GetDIBits.
    unsafe fn read_hbitmap(hbitmap: *mut c_void) -> Option<RawThumbnail> {
        let mut bm = std::mem::zeroed::<Bitmap>();
        let got = GetObjectW(
            hbitmap,
            std::mem::size_of::<Bitmap>() as i32,
            &mut bm as *mut _ as *mut c_void,
        );
        if got == 0 || bm.bm_width <= 0 || bm.bm_height <= 0 {
            return None;
        }
        let width = bm.bm_width as u32;
        let height = bm.bm_height as u32;
        let byte_len = (width as usize) * (height as usize) * 4;

        let mut info = std::mem::zeroed::<BitmapInfo>();
        info.header.bi_size = std::mem::size_of::<BitmapInfoHeader>() as u32;
        info.header.bi_width = bm.bm_width;
        // Від'ємна висота → рядки зверху вниз (top-down DIB).
        info.header.bi_height = -bm.bm_height;
        info.header.bi_planes = 1;
        info.header.bi_bit_count = 32;
        info.header.bi_compression = BI_RGB;

        let mut buffer = vec![0u8; byte_len];
        let hdc = GetDC(std::ptr::null_mut());
        if hdc.is_null() {
            return None;
        }
        let lines = GetDIBits(
            hdc,
            hbitmap,
            0,
            height,
            buffer.as_mut_ptr() as *mut c_void,
            &mut info,
            DIB_RGB_COLORS,
        );
        ReleaseDC(std::ptr::null_mut(), hdc);

        if lines == 0 {
            return None;
        }
        Some(RawThumbnail {
            width,
            height,
            bgra: buffer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_path_is_invalid_argument() {
        let src = WindowsShellThumbnailSource::new();
        let err = src.thumbnail("", 128).expect_err("порожній шлях = помилка");
        assert_eq!(
            err.code,
            trashradar_domain::error::ErrorCode::InvalidArgument
        );
    }

    #[cfg(windows)]
    #[test]
    fn nonexistent_file_returns_none_not_error() {
        // Неіснуючий файл: shell не має елемента → джерело його не покриває.
        let src = WindowsShellThumbnailSource::new();
        let out = src
            .thumbnail(r"C:\this\path\does\not\exist_ktr69.png", 96)
            .expect("неіснуючий файл не має бути помилкою — лише None");
        assert!(out.is_none(), "очікували None для неіснуючого файла");
    }

    #[cfg(windows)]
    #[test]
    fn existing_file_probe_never_errors() {
        // На валідному наявному файлі codepath COM+Shell має завершитись без
        // помилки: або Some (є мініатюра), або None (провайдера немає на раннері).
        // Не панікує і не повертає Err — цього досить для CI без десктоп-shell.
        let src = WindowsShellThumbnailSource::new();
        let exe = std::env::current_exe().expect("current exe");
        let path = exe.to_string_lossy().to_string();
        let out = src.thumbnail(&path, 128);
        assert!(
            out.is_ok(),
            "валідний файл: очікували Ok(_), отримали {out:?}"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_source_is_transparent() {
        let src = WindowsShellThumbnailSource::new();
        let out = src.thumbnail("/tmp/whatever.png", 128).expect("no error");
        assert!(out.is_none());
    }

    /// DoD T-069 (elevated не потрібен, але потрібен десктоп-shell з
    /// thumbnail-провайдером зображень): для реального зображення системна
    /// мініатюра віддається без власного декодування. Ігнорується в CI —
    /// GitHub windows-раннери можуть не мати thumbnail-провайдерів.
    #[cfg(windows)]
    #[test]
    #[ignore = "DoD: потребує десктоп-shell з thumbnail-провайдером"]
    fn real_image_yields_system_thumbnail() {
        let dir = std::env::temp_dir().join("tr_t069_thumb");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("solid_256.bmp");
        std::fs::write(&path, make_bmp(256, 256, [40, 90, 200])).unwrap();

        let src = WindowsShellThumbnailSource::new();
        let thumb = src
            .thumbnail(&path.to_string_lossy(), 128)
            .expect("thumbnail call")
            .expect("система мала б віддати мініатюру для BMP");

        assert!(thumb.width > 0 && thumb.height > 0);
        assert_eq!(thumb.bgra.len(), (thumb.width * thumb.height * 4) as usize);
        // Вписано в квадрат max_edge зі збереженням пропорцій.
        assert!(thumb.width <= 128 && thumb.height <= 128);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Згенерувати суцільний 24-бітний BMP (BGR) заданого розміру.
    #[cfg(windows)]
    fn make_bmp(width: u32, height: u32, bgr: [u8; 3]) -> Vec<u8> {
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
}
