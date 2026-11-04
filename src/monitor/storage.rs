//! Drive temperature through the documented storage property query.
//! The device is opened with no access rights, which needs no elevation.

use std::ffi::c_void;

use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_PROPERTY_QUERY,
    STORAGE_TEMPERATURE_DATA_DESCRIPTOR, STORAGE_TEMPERATURE_INFO, StorageDeviceTemperatureProperty,
};
use windows::core::PCWSTR;

use crate::win::{Handle, wide};

pub struct Drives {
    disks: Vec<Handle>,
}

// SAFETY: the handles are only used from the sampler thread that owns them.
unsafe impl Send for Drives {}

impl Drives {
    pub fn open() -> Self {
        let disks = (0..16)
            .filter_map(|n| {
                let path = wide(format!(r"\\.\PhysicalDrive{n}"));
                let h = unsafe {
                    CreateFileW(
                        PCWSTR(path.as_ptr()),
                        0,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        None,
                        OPEN_EXISTING,
                        FILE_FLAGS_AND_ATTRIBUTES(0),
                        None,
                    )
                }
                .ok()?;
                let h = Handle(h);
                temperature(&h).map(|_| h)
            })
            .collect();
        Self { disks }
    }

    /// Hottest drive that reports a temperature, °C.
    pub fn hottest(&self) -> Option<f32> {
        self.disks.iter().filter_map(temperature).reduce(f32::max)
    }
}

fn temperature(disk: &Handle) -> Option<f32> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceTemperatureProperty,
        QueryType: PropertyStandardQuery,
        ..Default::default()
    };
    // Descriptor plus room for several sensor entries.
    let mut buf = [0u64; 32];
    let mut returned = 0u32;
    unsafe {
        DeviceIoControl(
            disk.0,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const c_void),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            size_of_val(&buf) as u32,
            Some(&mut returned),
            None,
        )
        .ok()?;
    }
    if (returned as usize) < size_of::<STORAGE_TEMPERATURE_DATA_DESCRIPTOR>() {
        return None;
    }
    let desc = unsafe { &*(buf.as_ptr() as *const STORAGE_TEMPERATURE_DATA_DESCRIPTOR) };
    let first = std::ptr::addr_of!(desc.TemperatureInfo) as *const STORAGE_TEMPERATURE_INFO;
    let room = (size_of_val(&buf) - (first as usize - buf.as_ptr() as usize)) / size_of::<STORAGE_TEMPERATURE_INFO>();
    let infos = unsafe { std::slice::from_raw_parts(first, (desc.InfoCount as usize).min(room)) };
    // Index 0 is the composite temperature; sanity-bound the rest.
    infos.iter().map(|i| i.Temperature).filter(|t| (1..=150).contains(t)).max().map(f32::from)
}
