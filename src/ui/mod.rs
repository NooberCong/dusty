//! Win32 + Direct2D user interface.

mod anim;
mod app;
mod gfx;
mod logo;
mod overlay;
mod theme;
mod tray;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, LPARAM, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, FindWindowW, GetMessageW, MSG, PostMessageW, TranslateMessage, WM_APP,
};
use windows::core::{PCWSTR, w};

pub const WM_APP_SAMPLE: u32 = WM_APP + 1;
pub const WM_APP_TRAY: u32 = WM_APP + 2;
pub const WM_APP_JOB: u32 = WM_APP + 3;
pub const WM_APP_SHOW: u32 = WM_APP + 4;
pub const WM_APP_MENU: u32 = WM_APP + 5;
pub const WM_APP_TICK: u32 = WM_APP + 6;

/// Runs the app until Exit. A second launch just surfaces the first window.
pub fn run(start_hidden: bool) -> i32 {
    let _mutex = unsafe { CreateMutexW(None, false, w!(r"Local\dusty.single-instance")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        if !start_hidden
            && let Ok(h) = unsafe { FindWindowW(app::CLASS, PCWSTR::null()) }
        {
            unsafe { PostMessageW(Some(h), WM_APP_SHOW, WPARAM(0), LPARAM(0)) }.ok();
        }
        return 0;
    }
    if app::create(start_hidden).is_err() {
        return 1;
    }
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    msg.wParam.0 as i32
}
