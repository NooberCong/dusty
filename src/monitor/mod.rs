//! System sampling: one background thread, ~1 Hz while anything is showing
//! the numbers, parked (zero CPU) otherwise.

mod asus;
mod smbios;
mod gpu;
mod pdh;
mod storage;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{FILETIME, HWND, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::PCWSTR;

use crate::win::wide;

pub const HISTORY_LEN: usize = 60;
const INTERVAL: Duration = Duration::from_secs(1);
/// Free space and drive temperature change slowly; the drive query is also
/// the most expensive call in a tick (~2 ms), so refresh these every 10 s.
const SLOW_REFRESH: u32 = 10;
const DRIVE_FIXED: u32 = 3;

#[derive(Clone, Default, Debug)]
pub struct Snapshot {
    /// 0–100, busy time (matches Task Manager on current Windows 11).
    pub cpu: f32,
    pub cpu_name: String,
    /// CPU package temperature when a trustworthy sensor is available.
    pub cpu_temp: Option<f32>,
    /// Installed memory, such as "DDR5-4800, 2 × 16 GB", when the firmware says.
    pub mem_spec: Option<String>,
    pub mem_used: u64,
    pub mem_total: u64,
    /// Busiest physical disk's active time, 0–100.
    pub disk_active: f32,
    pub disk_read: f64,
    pub disk_write: f64,
    pub disk_temp: Option<f32>,
    pub gpu: Option<GpuStat>,
    pub volumes: Vec<Volume>,
}

#[derive(Clone, Default, Debug)]
pub struct GpuStat {
    pub name: String,
    pub util: f32,
    pub temp: Option<f32>,
    pub vram_used: u64,
    pub vram_total: u64,
}

#[derive(Clone, Debug)]
pub struct Volume {
    pub letter: char,
    pub free: u64,
    pub total: u64,
}

impl Snapshot {
    pub fn mem_percent(&self) -> f32 {
        if self.mem_total == 0 { 0.0 } else { (self.mem_used as f64 / self.mem_total as f64 * 100.0) as f32 }
    }
}

/// Fixed-length ring of recent values, oldest first when iterated.
#[derive(Clone)]
pub struct Series {
    buf: [f32; HISTORY_LEN],
    head: usize,
    len: usize,
}

impl Default for Series {
    fn default() -> Self {
        Self { buf: [0.0; HISTORY_LEN], head: 0, len: 0 }
    }
}

impl Series {
    pub fn push(&mut self, v: f32) {
        self.buf[self.head] = v;
        self.head = (self.head + 1) % HISTORY_LEN;
        self.len = (self.len + 1).min(HISTORY_LEN);
    }

    pub fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        let start = (self.head + HISTORY_LEN - self.len) % HISTORY_LEN;
        (0..self.len).map(move |i| self.buf[(start + i) % HISTORY_LEN])
    }
}

#[derive(Clone, Default)]
pub struct History {
    pub cpu: Series,
    pub mem: Series,
    pub disk: Series,
    pub gpu: Series,
}

#[derive(Default)]
pub struct State {
    pub now: Snapshot,
    pub history: History,
    /// False until the first real sample lands.
    pub ready: bool,
}

/// Collects one snapshot per call; owns every OS handle it needs.
pub struct Sampler {
    cpu_name: String,
    mem_spec: Option<String>,
    cpu_prev: Option<(u64, u64)>,
    pdh: Option<pdh::Pdh>,
    adapters: Vec<gpu::Adapter>,
    cpu_sensor: Option<asus::CpuSensor>,
    drives: storage::Drives,
    volumes: Vec<Volume>,
    disk_temp: Option<f32>,
    ticks: u32,
}

// SAFETY: every handle inside is used only by the thread that owns the Sampler.
unsafe impl Send for Sampler {}

impl Sampler {
    pub fn new() -> Self {
        let mut s = Self {
            cpu_name: cpu_name(),
            mem_spec: smbios::memory_spec(),
            cpu_prev: None,
            pdh: pdh::Pdh::open(),
            adapters: gpu::adapters(),
            cpu_sensor: asus::CpuSensor::open(),
            drives: storage::Drives::open(),
            volumes: Vec::new(),
            disk_temp: None,
            ticks: 0,
        };
        s.cpu_prev = cpu_times();
        s
    }

    pub fn sample(&mut self) -> Snapshot {
        let mut snap = Snapshot {
            cpu: self.cpu(),
            cpu_name: self.cpu_name.clone(),
            mem_spec: self.mem_spec.clone(),
            cpu_temp: self.cpu_sensor.as_ref().and_then(|s| s.temperature()),
            ..Default::default()
        };

        let mut mem = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if unsafe { GlobalMemoryStatusEx(&mut mem) }.is_ok() {
            snap.mem_total = mem.ullTotalPhys;
            snap.mem_used = mem.ullTotalPhys - mem.ullAvailPhys;
        }

        let pdh = self.pdh.as_mut().map(|p| p.sample()).unwrap_or_default();
        snap.disk_active = pdh.disk_active;
        snap.disk_read = pdh.disk_read;
        snap.disk_write = pdh.disk_write;

        // The discrete adapter (most dedicated VRAM) is the one people care about.
        snap.gpu = self.adapters.first().map(|a| GpuStat {
            name: a.name.clone(),
            util: pdh.gpu_util.get(&a.key).copied().unwrap_or(0.0),
            temp: a.temperature(),
            vram_used: pdh.gpu_vram.get(&a.key).copied().unwrap_or(0),
            vram_total: a.vram_total,
        });

        if self.ticks % SLOW_REFRESH == 0 {
            self.volumes = fixed_volumes();
            self.disk_temp = self.drives.hottest();
        }
        snap.disk_temp = self.disk_temp;
        self.ticks = self.ticks.wrapping_add(1);
        snap.volumes = self.volumes.clone();
        snap
    }

    fn cpu(&mut self) -> f32 {
        let Some((idle, total)) = cpu_times() else { return 0.0 };
        let usage = match self.cpu_prev {
            Some((pi, pt)) if total > pt => {
                let busy = (total - pt).saturating_sub(idle - pi);
                busy as f32 / (total - pt) as f32 * 100.0
            }
            _ => 0.0,
        };
        self.cpu_prev = Some((idle, total));
        usage.clamp(0.0, 100.0)
    }
}

/// (idle, kernel+user) in 100 ns units; kernel time already includes idle.
fn cpu_times() -> Option<(u64, u64)> {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    let ft = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    Some((ft(idle), ft(kernel) + ft(user)))
}

/// "13th Gen Intel(R) Core(TM) i9-13980HX" -> "Core i9-13980HX".
fn cpu_name() -> String {
    let raw = crate::win::reg_string(
        windows::Win32::System::Registry::HKEY_LOCAL_MACHINE,
        r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
        "ProcessorNameString",
    )
    .unwrap_or_default();
    tidy_cpu(&raw)
}

fn tidy_cpu(raw: &str) -> String {
    let s = raw.replace("(R)", "").replace("(TM)", "").replace(" CPU", "").replace(" Processor", "");
    let s = s.split(" @ ").next().unwrap_or("");
    let s = s.split(" w/ ").next().unwrap_or("").split(" with ").next().unwrap_or("");
    let s = s.find("Ryzen").or_else(|| s.find("Core")).map_or(s, |i| &s[i..]);
    // Drop AMD's "8-Core" style suffix.
    s.split_whitespace().filter(|w| !w.ends_with("-Core")).collect::<Vec<_>>().join(" ")
}

fn fixed_volumes() -> Vec<Volume> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .filter_map(|i| {
            let letter = (b'A' + i) as char;
            let root = wide(format!("{letter}:\\"));
            if unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } != DRIVE_FIXED {
                return None;
            }
            let (mut free, mut total) = (0u64, 0u64);
            unsafe { GetDiskFreeSpaceExW(PCWSTR(root.as_ptr()), Some(&mut free), Some(&mut total), None) }.ok()?;
            (total > 0).then_some(Volume { letter, free, total })
        })
        .collect()
}

/// Background sampler thread. Posts `msg` to the window after each sample.
pub struct Monitor {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    active: AtomicBool,
    stop: AtomicBool,
}

impl Monitor {
    pub fn start(notify: HWND, msg: u32) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            active: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let s = shared.clone();
        let hwnd = notify.0 as usize;
        let thread = thread::Builder::new()
            .name("sampler".into())
            .spawn(move || {
                let mut sampler = Sampler::new();
                while !s.stop.load(Ordering::Acquire) {
                    if !s.active.load(Ordering::Acquire) {
                        thread::park();
                        continue;
                    }
                    let started = Instant::now();
                    let snap = sampler.sample();
                    {
                        let mut st = s.state.lock().unwrap();
                        st.history.cpu.push(snap.cpu);
                        st.history.mem.push(snap.mem_percent());
                        st.history.disk.push(snap.disk_active);
                        st.history.gpu.push(snap.gpu.as_ref().map_or(0.0, |g| g.util));
                        st.now = snap;
                        st.ready = true;
                    }
                    unsafe { PostMessageW(Some(HWND(hwnd as _)), msg, WPARAM(0), LPARAM(0)) }.ok();
                    thread::park_timeout(INTERVAL.saturating_sub(started.elapsed()));
                }
            })
            .expect("spawn sampler");
        Self { shared, thread: Some(thread) }
    }

    /// Sample while something on screen shows the numbers; park otherwise.
    pub fn set_active(&self, on: bool) {
        if self.shared.active.swap(on, Ordering::AcqRel) != on && on {
            self.unpark();
        }
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.shared.state.lock().unwrap()
    }

    fn unpark(&self) {
        if let Some(t) = &self.thread {
            t.thread().unpark();
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.unpark();
        if let Some(t) = self.thread.take() {
            t.join().ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_wraps_oldest_first() {
        let mut s = Series::default();
        for i in 0..(HISTORY_LEN + 5) {
            s.push(i as f32);
        }
        let v: Vec<f32> = s.iter().collect();
        assert_eq!(v.len(), HISTORY_LEN);
        assert_eq!(v[0], 5.0);
        assert_eq!(*v.last().unwrap(), (HISTORY_LEN + 4) as f32);
    }

    /// Per-source cost of one sample: `cargo test --release sample_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn sample_cost() {
        let time = |name: &str, f: &mut dyn FnMut()| {
            let t = Instant::now();
            for _ in 0..20 {
                f();
            }
            println!("{name:<10} {:>8.3} ms/sample", t.elapsed().as_secs_f64() * 1000.0 / 20.0);
        };
        let mut s = Sampler::new();
        std::thread::sleep(Duration::from_millis(1100));
        time("cpu", &mut || {
            s.cpu();
        });
        let pdh = s.pdh.as_mut().unwrap();
        time("pdh", &mut || {
            pdh.sample();
        });
        if let Some(sensor) = &s.cpu_sensor {
            time("cpu temp", &mut || {
                sensor.temperature();
            });
        }
        time("gpu temp", &mut || {
            s.adapters.iter().for_each(|a| {
                a.temperature();
            })
        });
        time("disk temp", &mut || {
            s.drives.hottest();
        });
        time("volumes", &mut || {
            fixed_volumes();
        });
        time("memory", &mut || {
            let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
            unsafe { GlobalMemoryStatusEx(&mut m) }.ok();
        });
    }

    #[test]
    fn cpu_names() {
        assert_eq!(tidy_cpu("13th Gen Intel(R) Core(TM) i9-13980HX"), "Core i9-13980HX");
        assert_eq!(tidy_cpu("AMD Ryzen 7 7800X3D 8-Core Processor           "), "Ryzen 7 7800X3D");
        assert_eq!(tidy_cpu("Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz"), "Core i7-8700K");
        assert_eq!(tidy_cpu("AMD Ryzen 9 7940HS w/ Radeon 780M Graphics"), "Ryzen 9 7940HS");
    }

    #[test]
    fn sampler_produces_sane_values() {
        let mut s = Sampler::new();
        std::thread::sleep(Duration::from_millis(300));
        let snap = s.sample();
        assert!((0.0..=100.0).contains(&snap.cpu));
        assert!(snap.mem_total > 0 && snap.mem_used <= snap.mem_total);
        assert!(!snap.volumes.is_empty());
    }
}
