//! CPU temperature from ASUS firmware. `\\.\ATKACPI` belongs to the ASUS System
//! Control Interface that ships on ASUS laptops; unlike the CPU's own sensor
//! registers it is open to normal users, so this needs no driver and no admin.
//! Only the read-only DSTS method is ever called.

use std::ffi::c_void;

use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::core::w;

use crate::win::Handle;

const IOCTL_ATK_CALL: u32 = 0x0022_240C;
const METHOD_DSTS: u32 = u32::from_le_bytes(*b"DSTS");
const DEVICE_CPU_TEMP: u32 = 0x0012_0094;
const PRESENT: u32 = 0x0001_0000;

#[repr(C)]
struct Call {
    method: u32,
    args_len: u32,
    device: u32,
    reserved: u32,
}

pub struct CpuSensor(Handle);

impl CpuSensor {
    /// None on anything without the interface, or where it has no CPU sensor.
    pub fn open() -> Option<Self> {
        let h = unsafe {
            CreateFileW(
                w!(r"\\.\ATKACPI"),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        }
        .ok()?;
        let sensor = Self(Handle(h));
        sensor.temperature().map(|_| sensor)
    }

    /// CPU temperature in °C. The firmware refreshes it about once a second.
    pub fn temperature(&self) -> Option<f32> {
        let call = Call { method: METHOD_DSTS, args_len: 8, device: DEVICE_CPU_TEMP, reserved: 0 };
        let mut out = [0u32; 4];
        unsafe {
            DeviceIoControl(
                self.0.0,
                IOCTL_ATK_CALL,
                Some(&call as *const _ as *const c_void),
                size_of::<Call>() as u32,
                Some(out.as_mut_ptr() as *mut c_void),
                size_of_val(&out) as u32,
                None,
                None,
            )
        }
        .ok()?;
        parse(out[0])
    }
}

/// The high half says the sensor exists; models without one report 0 or an error code.
fn parse(raw: u32) -> Option<f32> {
    let celsius = raw & 0xFFFF;
    (raw & 0xFFFF_0000 == PRESENT && (1..=125).contains(&celsius)).then_some(celsius as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_is_the_ascii_tag() {
        assert_eq!(METHOD_DSTS, 0x5354_5344);
    }

    #[test]
    fn rejects_absent_and_impossible_readings() {
        assert_eq!(parse(0x0001_0054), Some(84.0));
        assert_eq!(parse(0x0001_0000), None); // present but no value: this laptop's GPU id
        assert_eq!(parse(0xFFFF_FFFE), None); // unsupported device
        assert_eq!(parse(0x0000_0054), None); // presence bit clear
        assert_eq!(parse(0x0001_00FF), None); // out of range
    }
}
