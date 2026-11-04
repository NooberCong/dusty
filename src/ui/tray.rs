//! Notification-area icon: live tooltip, left click opens, right click menu.

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_NONE, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HICON, LoadImageW, IMAGE_ICON, LR_DEFAULTCOLOR,
    MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, GetSystemMetrics, SM_CXSMICON, SM_CYSMICON,
};
use windows::core::{PCSTR, PCWSTR, w};

use super::WM_APP_TRAY;

pub struct Tray {
    data: NOTIFYICONDATAW,
}

pub enum MenuChoice {
    Open,
    QuickClean,
    ToggleOverlay,
    Exit,
}

fn copy(dst: &mut [u16], s: &str) {
    let n = dst.len() - 1;
    let mut i = 0;
    for c in s.encode_utf16().take(n) {
        dst[i] = c;
        i += 1;
    }
    dst[i] = 0;
}

impl Tray {
    pub fn new(hwnd: HWND) -> Self {
        let icon = unsafe {
            let size = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
            LoadImageW(GetModuleHandleW(None).ok().map(Into::into), PCWSTR(1 as _), IMAGE_ICON, size.0, size.1, LR_DEFAULTCOLOR)
                .map(|h| HICON(h.0))
                .unwrap_or_default()
        };
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_APP_TRAY,
            hIcon: icon,
            ..Default::default()
        };
        copy(&mut data.szTip, "dusty");
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let tray = Tray { data };
        tray.add();
        tray
    }

    /// (Re-)registers the icon, e.g. after Explorer restarts.
    pub fn add(&self) {
        unsafe {
            Shell_NotifyIconW(NIM_ADD, &self.data).ok().ok();
            Shell_NotifyIconW(NIM_SETVERSION, &self.data).ok().ok();
        }
    }

    pub fn set_tip(&mut self, tip: &str) {
        copy(&mut self.data.szTip, tip);
        self.data.uFlags = NIF_TIP | NIF_SHOWTIP;
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.data) }.ok().ok();
    }

    /// Windows notification toast attributed to the tray icon.
    pub fn notify(&mut self, title: &str, body: &str) {
        copy(&mut self.data.szInfoTitle, title);
        copy(&mut self.data.szInfo, body);
        self.data.dwInfoFlags = NIIF_NONE;
        self.data.uFlags = NIF_INFO;
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.data) }.ok().ok();
    }

    pub fn menu(&self, hwnd: HWND, overlay_on: bool, busy: bool) -> Option<MenuChoice> {
        unsafe {
            let menu = CreatePopupMenu().ok()?;
            AppendMenuW(menu, MF_STRING, 1, w!("Open dusty")).ok();
            AppendMenuW(menu, if busy { MF_STRING | MF_GRAYED } else { MF_STRING }, 2, w!("Quick clean")).ok();
            AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()).ok();
            AppendMenuW(menu, if overlay_on { MF_STRING | MF_CHECKED } else { MF_STRING }, 3, w!("Taskbar readout")).ok();
            AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()).ok();
            AppendMenuW(menu, MF_STRING, 4, w!("Exit")).ok();
            let mut pt = POINT::default();
            GetCursorPos(&mut pt).ok();
            // Required so the menu closes when the user clicks elsewhere.
            SetForegroundWindow(hwnd).ok().ok();
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, None, hwnd, None);
            DestroyMenu(menu).ok();
            match cmd.0 {
                1 => Some(MenuChoice::Open),
                2 => Some(MenuChoice::QuickClean),
                3 => Some(MenuChoice::ToggleOverlay),
                4 => Some(MenuChoice::Exit),
                _ => None,
            }
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data) }.ok().ok();
    }
}

/// Lets classic popup menus follow the dark theme. These uxtheme exports
/// (ordinals 135/136) are undocumented but stable since Windows 10 1903;
/// if they are missing nothing happens and menus stay light.
pub fn allow_dark_menus() {
    unsafe {
        let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(set_mode) = GetProcAddress(ux, PCSTR(135 as _)) {
            let set_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
            set_mode(1); // AllowDark: follow the system app theme.
        }
        if let Some(flush) = GetProcAddress(ux, PCSTR(136 as _)) {
            let flush: extern "system" fn() = std::mem::transmute(flush);
            flush();
        }
    }
}
