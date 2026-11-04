//! Drives Windows' own Disk Cleanup handlers (the COM objects registered under
//! HKLM\...\Explorer\VolumeCaches) exactly as cleanmgr does: InitializeEx,
//! GetSpaceUsed, optionally Purge, then Deactivate. Using Microsoft's logic
//! for component-store, Delivery Optimization, driver-store and upgrade
//! leftovers is the only safe way to clean those stores.

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSIDFromString, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW};
use windows::Win32::UI::LegacyWindowsEnvironmentFeatures::{
    EMPTY_VOLUME_CACHE_FLAGS, IEmptyVolumeCache, IEmptyVolumeCache2, IEmptyVolumeCacheCallBack,
    IEmptyVolumeCacheCallBack_Impl,
};
use windows::core::{GUID, Interface, PCWSTR, PWSTR, Result, implement};

use crate::win::wide;

const ROOT: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\VolumeCaches\";

/// Keeps COM initialised for the calling thread while alive.
pub struct ComScope;

impl ComScope {
    pub fn enter() -> Self {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok().ok();
        ComScope
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

#[implement(IEmptyVolumeCacheCallBack)]
struct Progress;

impl IEmptyVolumeCacheCallBack_Impl for Progress_Impl {
    fn ScanProgress(&self, _: u64, _: u32, _: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn PurgeProgress(&self, _: u64, _: u64, _: u32, _: &PCWSTR) -> Result<()> {
        Ok(())
    }
}

struct RegKey(HKEY);

impl Drop for RegKey {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) }.ok().ok();
    }
}

/// Bytes the handler could free, or when `purge`, the bytes it did free and
/// the bytes it still reports. Errors mean the handler is missing, refused
/// (usually: not elevated) or failed.
pub fn run(key: &str, volume: &str, purge: bool) -> Result<(u64, u64)> {
    let before = session(key, volume, purge)?;
    if !purge || before == 0 {
        return Ok((before, 0));
    }
    Ok(purged(before, session(key, volume, false)))
}

/// A handler's figure is only what it could free, and purging can leave it
/// unchanged, so what went is what it no longer reports. If it can't say,
/// nothing is claimed.
fn purged(before: u64, after: Result<u64>) -> (u64, u64) {
    after.map_or((0, 0), |after| (before.saturating_sub(after), after))
}

/// One pass through a handler, returning the size it reported at the start.
fn session(key: &str, volume: &str, purge: bool) -> Result<u64> {
    let mut hkey = HKEY::default();
    let path = wide(format!("{ROOT}{key}"));
    unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr()), None, KEY_READ, &mut hkey) }.ok()?;
    let hkey = RegKey(hkey);

    let mut buf = [0u16; 64];
    let mut size = size_of_val(&buf) as u32;
    unsafe {
        RegQueryValueExW(hkey.0, PCWSTR::null(), None, None, Some(buf.as_mut_ptr() as *mut u8), Some(&mut size))
    }
    .ok()?;
    let clsid: GUID = unsafe { CLSIDFromString(PCWSTR(buf.as_ptr()))? };

    let cache: IEmptyVolumeCache = unsafe { CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)? };
    let (vol, name) = (wide(volume), wide(key));
    let (mut display, mut desc, mut btn) = (PWSTR::null(), PWSTR::null(), PWSTR::null());
    let mut flags = EMPTY_VOLUME_CACHE_FLAGS(0);
    let init = match cache.cast::<IEmptyVolumeCache2>() {
        Ok(c2) => unsafe {
            c2.InitializeEx(
                hkey.0,
                PCWSTR(vol.as_ptr()),
                PCWSTR(name.as_ptr()),
                &mut display,
                &mut desc,
                &mut btn,
                &mut flags,
            )
        },
        Err(_) => unsafe { cache.Initialize(hkey.0, PCWSTR(vol.as_ptr()), &mut display, &mut desc, &mut flags) },
    };
    for s in [display, desc, btn] {
        if !s.is_null() {
            unsafe { CoTaskMemFree(Some(s.0 as *const _)) };
        }
    }
    init?;

    let progress: IEmptyVolumeCacheCallBack = Progress.into();
    let mut used = 0u64;
    let result = unsafe { cache.GetSpaceUsed(&mut used, &progress) }
        .and_then(|_| if purge && used > 0 { unsafe { cache.Purge(used, &progress) } } else { Ok(()) });
    unsafe { cache.Deactivate() }.ok();
    result.map(|_| used)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_a_handler_stopped_reporting_counts_as_freed() {
        assert_eq!(purged(1520, Ok(190)), (1330, 190));
        assert_eq!(purged(190, Ok(190)), (0, 190));
        assert_eq!(purged(190, Ok(400)), (0, 400));
        assert_eq!(purged(190, Err(windows::core::Error::empty())), (0, 0));
    }
}
