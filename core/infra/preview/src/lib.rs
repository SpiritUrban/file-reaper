//! Адаптер `PreviewStore`: конвеєр превью (docs/architecture.md §5).
//!
//! Реалізація: E6 (T-067…T-076). Найнестабільніший код системи —
//! декодери працюють за бар'єром ізоляції збоїв (T-074).
//!
//! T-069: перше швидке джерело мініатюр — **системний кеш мініатюр Windows**
//! через Shell API (`IShellItemImageFactory::GetImage`). Для файлів, які
//! користувач колись бачив у Explorer (або має системний thumbnail-провайдер),
//! превью віддається без власного декодування — decode робить ОС/її провайдери.
//!
//! T-070: власна генерація мініатюр зображень — декодування з даунскейлом
//! через Windows Imaging Component ([`ImageThumbnailSource`], модуль `image`).
//!
//! T-071: ключовий кадр і тривалість відео — ffmpeg sidecar-процесом
//! ([`FfmpegVideoFrameSource`], модуль `video`; мультиплатформно).

#[cfg(windows)]
mod com;
mod image;
#[cfg(all(test, windows))]
pub(crate) mod test_media;
mod video;

pub use image::ImageThumbnailSource;
pub use video::{FfmpegVideoFrameSource, FFMPEG_ENV_VAR};

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

    use crate::com::{ComApartment, Guid};

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
        let _apartment = ComApartment::enter()?;
        unsafe { extract(path, max_edge) }
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
        std::fs::write(&path, crate::test_media::make_bmp(256, 256, [40, 90, 200])).unwrap();

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
}
