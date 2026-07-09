//! Спільні COM-примітиви для джерел превью (лише Windows).
//!
//! Guid + RAII-guard апартаменту потоку — використовуються джерелом
//! системних мініатюр (T-069) і декодером зображень (T-070), щоб
//! логіка `CoInitializeEx`/`CoUninitialize` не дублювалась.

use std::ffi::c_void;

use trashradar_domain::error::CoreError;

/// Мінімальний COM GUID.
#[repr(C)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, co_init: u32) -> i32;
    fn CoUninitialize();
    pub fn CoCreateInstance(
        rclsid: *const Guid,
        punk_outer: *mut c_void,
        cls_context: u32,
        riid: *const Guid,
        ppv: *mut *mut c_void,
    ) -> i32;
}

/// `COINIT_APARTMENTTHREADED` — модель для Shell/WIC-провайдерів.
const COINIT_APARTMENTTHREADED: u32 = 0x2;
/// `S_OK`.
const S_OK: i32 = 0;
/// `S_FALSE` — COM уже ініціалізовано на цьому потоці цією ж моделлю.
const S_FALSE: i32 = 1;
/// `RPC_E_CHANGED_MODE` — потік уже в COM з іншою моделлю; працюємо без
/// власної ініціалізації і не викликаємо `CoUninitialize`.
const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;
/// `CLSCTX_INPROC_SERVER` для `CoCreateInstance`.
pub const CLSCTX_INPROC_SERVER: u32 = 0x1;

/// RAII-guard COM-апартаменту потоку.
///
/// `CoUninitialize` викликається у `Drop` лише якщо ініціалізацію підняли
/// ми самі (`S_OK`/`S_FALSE`); якщо потік уже в COM з іншою моделлю
/// (`RPC_E_CHANGED_MODE`) — працюємо як є, нічого не знімаємо.
pub struct ComApartment {
    owns_com: bool,
}

impl ComApartment {
    /// Увійти в апартамент (ідемпотентно для потоку).
    pub fn enter() -> Result<Self, CoreError> {
        let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED) };
        if hr < 0 && hr != RPC_E_CHANGED_MODE {
            return Err(CoreError::io(format!(
                "CoInitializeEx не вдалося (HRESULT {hr:#010x})."
            )));
        }
        Ok(Self {
            owns_com: hr == S_OK || hr == S_FALSE,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owns_com {
            unsafe { CoUninitialize() };
        }
    }
}
