//! GPU identity (DXGI) and driver-reported telemetry (D3DKMT perf data,
//! WDDM 2.4+): temperature without any kernel driver of our own.

use std::ffi::c_void;

use windows::Wdk::Graphics::Direct3D::{
    D3DKMT_ADAPTER_PERFDATA, D3DKMT_CLOSEADAPTER, D3DKMT_OPENADAPTERFROMLUID, D3DKMT_QUERYADAPTERINFO,
    D3DKMTCloseAdapter, D3DKMTOpenAdapterFromLuid, D3DKMTQueryAdapterInfo, KMTQAITYPE_ADAPTERPERFDATA,
};
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1};

use crate::win::from_wide;

pub struct Adapter {
    pub name: String,
    /// Matches the PDH instance LUID format.
    pub key: String,
    pub vram_total: u64,
    kmt: Option<u32>,
}

pub fn luid_key(l: LUID) -> String {
    format!("0x{:08X}_0x{:08X}", l.HighPart as u32, l.LowPart)
}

/// Hardware adapters, discrete (largest dedicated VRAM) first.
pub fn adapters() -> Vec<Adapter> {
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return Vec::new() };
    let mut out: Vec<Adapter> = Vec::new();
    for i in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let key = luid_key(desc.AdapterLuid);
        if out.iter().any(|a| a.key == key) {
            continue;
        }
        let mut open = D3DKMT_OPENADAPTERFROMLUID { AdapterLuid: desc.AdapterLuid, ..Default::default() };
        let kmt = unsafe { D3DKMTOpenAdapterFromLuid(&mut open) }.is_ok().then_some(open.hAdapter);
        out.push(Adapter {
            name: tidy_name(&from_wide(&desc.Description)),
            key,
            vram_total: desc.DedicatedVideoMemory as u64,
            kmt,
        });
    }
    out.sort_by(|a, b| b.vram_total.cmp(&a.vram_total));
    out
}

impl Adapter {
    /// Driver-reported temperature in °C; None when unsupported or asleep.
    pub fn temperature(&self) -> Option<f32> {
        let mut perf = D3DKMT_ADAPTER_PERFDATA::default();
        let mut query = D3DKMT_QUERYADAPTERINFO {
            hAdapter: self.kmt?,
            Type: KMTQAITYPE_ADAPTERPERFDATA,
            pPrivateDriverData: &mut perf as *mut _ as *mut c_void,
            PrivateDriverDataSize: size_of::<D3DKMT_ADAPTER_PERFDATA>() as u32,
        };
        unsafe { D3DKMTQueryAdapterInfo(&mut query) }.ok().ok()?;
        // Deci-degrees Celsius; 0 means the driver does not report it.
        (perf.Temperature > 0).then(|| perf.Temperature as f32 / 10.0)
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        if let Some(h) = self.kmt {
            unsafe { D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER { hAdapter: h }) }.ok().ok();
        }
    }
}

/// "NVIDIA GeForce RTX 4080 Laptop GPU" -> "RTX 4080 Laptop GPU".
fn tidy_name(name: &str) -> String {
    let mut s = name.trim();
    for prefix in ["NVIDIA ", "GeForce ", "AMD ", "Radeon(TM) ", "Intel(R) "] {
        s = s.strip_prefix(prefix).unwrap_or(s);
    }
    s.replace("(R)", "").replace("(TM)", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(tidy_name("NVIDIA GeForce RTX 4080 Laptop GPU"), "RTX 4080 Laptop GPU");
        assert_eq!(tidy_name("Intel(R) UHD Graphics"), "UHD Graphics");
    }

    #[test]
    fn luid_format() {
        assert_eq!(luid_key(LUID { LowPart: 0xD1B5, HighPart: 0 }), "0x00000000_0x0000D1B5");
    }
}
