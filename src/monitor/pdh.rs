//! One persistent PDH query for the counters Task Manager itself uses:
//! physical-disk activity and throughput, GPU engine utilisation and VRAM.

use std::collections::HashMap;

use windows::Win32::System::Performance::{
    PDH_CSTATUS_INVALID_DATA, PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE,
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_FMT, PDH_FMT_LARGE, PDH_HCOUNTER,
    PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
    PdhGetFormattedCounterArrayW, PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows::core::PCWSTR;

use crate::win::{from_wide, wide};

// Not exported by the windows crate.
const PDH_FMT_NOCAP100: u32 = 0x8000;

pub struct Pdh {
    query: PDH_HQUERY,
    disk_idle: Option<PDH_HCOUNTER>,
    disk_read: Option<PDH_HCOUNTER>,
    disk_write: Option<PDH_HCOUNTER>,
    gpu_engine: Option<PDH_HCOUNTER>,
    gpu_memory: Option<PDH_HCOUNTER>,
}

#[derive(Default)]
pub struct PdhSample {
    /// Busiest physical disk, 0–100 (Task Manager's "Active time").
    pub disk_active: f32,
    pub disk_read: f64,
    pub disk_write: f64,
    /// Per adapter LUID key ("0x00000000_0x0000D1B5"): utilisation 0–100.
    pub gpu_util: HashMap<String, f32>,
    /// Per adapter LUID key: dedicated VRAM in use, bytes.
    pub gpu_vram: HashMap<String, u64>,
}

// SAFETY: PDH handles are plain process-wide handles; the query is only used
// from the sampler thread that owns it.
unsafe impl Send for Pdh {}

impl Pdh {
    pub fn open() -> Option<Self> {
        let mut query = PDH_HQUERY::default();
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) } != 0 {
            return None;
        }
        let add = |path: &str| {
            let w = wide(path);
            let mut c = PDH_HCOUNTER::default();
            (unsafe { PdhAddEnglishCounterW(query, PCWSTR(w.as_ptr()), 0, &mut c) } == 0).then_some(c)
        };
        let pdh = Self {
            query,
            disk_idle: add(r"\PhysicalDisk(*)\% Idle Time"),
            disk_read: add(r"\PhysicalDisk(_Total)\Disk Read Bytes/sec"),
            disk_write: add(r"\PhysicalDisk(_Total)\Disk Write Bytes/sec"),
            gpu_engine: add(r"\GPU Engine(*)\Utilization Percentage"),
            gpu_memory: add(r"\GPU Adapter Memory(*)\Dedicated Usage"),
        };
        // Rate counters need a baseline sample.
        unsafe { PdhCollectQueryData(query) };
        Some(pdh)
    }

    pub fn sample(&mut self) -> PdhSample {
        let mut s = PdhSample::default();
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return s;
        }
        if let Some(c) = self.disk_idle {
            s.disk_active = array_f64(c, PDH_FMT_DOUBLE)
                .into_iter()
                .filter(|(name, _)| name != "_Total")
                .map(|(_, idle)| (100.0 - idle).clamp(0.0, 100.0) as f32)
                .fold(0.0, f32::max);
        }
        s.disk_read = self.disk_read.and_then(single_f64).unwrap_or(0.0);
        s.disk_write = self.disk_write.and_then(single_f64).unwrap_or(0.0);
        if let Some(c) = self.gpu_engine {
            s.gpu_util = gpu_utilisation(array_f64(c, PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100)));
        }
        if let Some(c) = self.gpu_memory {
            for (name, bytes) in array_f64(c, PDH_FMT_LARGE) {
                if let Some(luid) = luid_key(&name) {
                    *s.gpu_vram.entry(luid).or_default() += bytes as u64;
                }
            }
        }
        s
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

fn status_ok(status: u32) -> bool {
    status == PDH_CSTATUS_VALID_DATA || status == PDH_CSTATUS_NEW_DATA
}

fn single_f64(c: PDH_HCOUNTER) -> Option<f64> {
    let mut v = PDH_FMT_COUNTERVALUE::default();
    let rc = unsafe { PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE, None, &mut v) };
    (rc == 0 && status_ok(v.CStatus)).then(|| unsafe { v.Anonymous.doubleValue })
}

/// All instances of a wildcard counter as (instance name, value).
fn array_f64(c: PDH_HCOUNTER, fmt: PDH_FMT) -> Vec<(String, f64)> {
    let large = fmt.0 & PDH_FMT_LARGE.0 != 0;
    let (mut size, mut count) = (0u32, 0u32);
    let rc = unsafe { PdhGetFormattedCounterArrayW(c, fmt, &mut size, &mut count, None) };
    if rc != PDH_MORE_DATA || size == 0 {
        return Vec::new();
    }
    // u64 backing keeps the item array correctly aligned.
    let mut buf = vec![0u64; (size as usize).div_ceil(8)];
    let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
    if unsafe { PdhGetFormattedCounterArrayW(c, fmt, &mut size, &mut count, Some(items)) } != 0 {
        return Vec::new();
    }
    let items = unsafe { std::slice::from_raw_parts(items, count as usize) };
    items
        .iter()
        .filter(|it| it.FmtValue.CStatus != PDH_CSTATUS_INVALID_DATA && status_ok(it.FmtValue.CStatus))
        .map(|it| {
            let name = unsafe { from_wide(it.szName.as_wide()) };
            let v = unsafe {
                if large { it.FmtValue.Anonymous.largeValue as f64 } else { it.FmtValue.Anonymous.doubleValue }
            };
            (name, v)
        })
        .collect()
}

/// "pid_4_luid_0x00000000_0x0000D1B5_phys_0_eng_0_engtype_3D" -> "0x00000000_0x0000D1B5".
fn luid_key(instance: &str) -> Option<String> {
    let rest = &instance[instance.find("luid_")? + 5..];
    rest.get(..21).map(|k| k.to_ascii_uppercase().replace("0X", "0x"))
}

/// Task Manager's method: sum each engine type across processes, then take
/// the busiest engine type as the adapter's utilisation.
fn gpu_utilisation(instances: Vec<(String, f64)>) -> HashMap<String, f32> {
    let mut per_engine: HashMap<(String, String), f64> = HashMap::new();
    for (name, v) in instances {
        let (Some(luid), Some(pos)) = (luid_key(&name), name.find("engtype_")) else { continue };
        *per_engine.entry((luid, name[pos + 8..].to_string())).or_default() += v;
    }
    let mut out: HashMap<String, f32> = HashMap::new();
    for ((luid, _), v) in per_engine {
        let e = out.entry(luid).or_default();
        *e = e.max(v.min(100.0) as f32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_luid() {
        assert_eq!(
            luid_key("pid_1234_luid_0x00000000_0x0000d1b5_phys_0_eng_0_engtype_3D").as_deref(),
            Some("0x00000000_0x0000D1B5")
        );
        assert_eq!(luid_key("luid_0x00000000_0x0000D1B5_phys_0").as_deref(), Some("0x00000000_0x0000D1B5"));
        assert_eq!(luid_key("_Total"), None);
    }

    #[test]
    fn utilisation_sums_engine_then_maxes() {
        let l = "luid_0x00000000_0x00000001_phys_0";
        let u = gpu_utilisation(vec![
            (format!("pid_1_{l}_eng_0_engtype_3D"), 30.0),
            (format!("pid_2_{l}_eng_0_engtype_3D"), 25.0),
            (format!("pid_2_{l}_eng_4_engtype_VideoDecode"), 40.0),
        ]);
        assert_eq!(u["0x00000000_0x00000001"], 55.0);
    }
}
