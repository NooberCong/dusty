//! Small Win32 helpers shared across modules.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, HANDLE, MAX_PATH};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::PCWSTR;

/// Null-terminated UTF-16 copy of `s`.
pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

/// UTF-16 buffer (possibly null-terminated) to String.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// `\\?\`-prefixed path so deep trees are not limited to MAX_PATH.
pub fn long_path(p: &Path) -> Vec<u16> {
    let s = p.as_os_str();
    if s.to_string_lossy().starts_with(r"\\") {
        wide(s)
    } else {
        let mut v: Vec<u16> = r"\\?\".encode_utf16().collect();
        v.extend(s.encode_wide());
        v.push(0);
        v
    }
}

/// Owned kernel handle closed on drop.
pub struct Handle(pub HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            unsafe { CloseHandle(self.0).ok() };
        }
    }
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let token = Handle(token);
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0;
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok()
            && elevation.TokenIsElevated != 0
    }
}

pub fn reg_dword(root: HKEY, key: &str, value: &str) -> Option<u32> {
    let (key, value) = (wide(key), wide(value));
    let mut data = 0u32;
    let mut size = 4u32;
    let mut kind = REG_DWORD;
    unsafe {
        RegGetValueW(
            root,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            Some(&mut kind),
            Some(&mut data as *mut _ as *mut _),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    Some(data)
}

pub fn reg_string(root: HKEY, key: &str, value: &str) -> Option<String> {
    let (key, value) = (wide(key), wide(value));
    let mut buf = [0u16; 512];
    let mut size = size_of_val(&buf) as u32;
    unsafe {
        RegGetValueW(
            root,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    Some(from_wide(&buf))
}

/// Windows "app mode": true when apps should use the light theme.
pub fn apps_use_light_theme() -> bool {
    reg_dword(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "AppsUseLightTheme",
    )
    .is_some_and(|v| v != 0)
}

/// Windows "system mode": true when the taskbar uses the light theme.
pub fn system_uses_light_theme() -> bool {
    reg_dword(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "SystemUsesLightTheme",
    )
    .is_some_and(|v| v != 0)
}

pub fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Windows' own folder, from the system rather than %SystemRoot%: the elevated
/// task inherits the user's environment, which their programs can change.
pub fn windows_dir() -> Option<PathBuf> {
    let mut buf = [0u16; MAX_PATH as usize];
    let len = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
    (1..buf.len()).contains(&len).then(|| PathBuf::from(from_wide(&buf)))
}

pub fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("dusty.exe"))
}

/// "1.2 GB" style size with one decimal above KB.
/// An hour on the clock, the way this PC writes times.
pub fn fmt_hour(hour: u8) -> String {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::Globalization::{GetTimeFormatEx, TIME_NOSECONDS};
    let time = SYSTEMTIME { wHour: hour as u16, ..Default::default() };
    let mut text = [0u16; 32];
    let written = unsafe {
        GetTimeFormatEx(windows::core::PCWSTR::null(), TIME_NOSECONDS, Some(&time), windows::core::PCWSTR::null(), Some(&mut text))
    };
    if written > 0 { from_wide(&text) } else { format!("{hour:02}:00") }
}

pub fn fmt_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else if v >= 100.0 { format!("{v:.0} {}", UNITS[i]) } else { format!("{v:.1} {}", UNITS[i]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_format() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(1023), "1023 B");
        assert_eq!(fmt_bytes(1536), "1.5 KB");
        assert_eq!(fmt_bytes(250 * 1024 * 1024), "250 MB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
    }

    #[test]
    fn long_path_prefix() {
        let p = long_path(Path::new(r"C:\x"));
        assert_eq!(from_wide(&p), r"\\?\C:\x");
        let unc = long_path(Path::new(r"\\?\C:\x"));
        assert_eq!(from_wide(&unc), r"\\?\C:\x");
    }
}
