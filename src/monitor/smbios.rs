//! Installed memory as the firmware describes it. SMBIOS is the only source
//! for type and speed that needs neither admin rights nor WMI.

use windows::Win32::System::SystemInformation::{FIRMWARE_TABLE_PROVIDER, GetSystemFirmwareTable};

/// 'RSMB', the raw SMBIOS provider.
const RSMB: FIRMWARE_TABLE_PROVIDER = FIRMWARE_TABLE_PROVIDER(0x5253_4D42);
/// RawSMBIOSData's fields ahead of the table itself.
const HEADER: usize = 8;
const END_OF_TABLE: u8 = 127;
const MEMORY_DEVICE: u8 = 17;

/// For example "DDR5-4800, 2 × 16 GB". None if the firmware doesn't say.
pub fn memory_spec() -> Option<String> {
    let len = unsafe { GetSystemFirmwareTable(RSMB, 0, None) } as usize;
    if len <= HEADER {
        return None;
    }
    let mut raw = vec![0u8; len];
    let read = unsafe { GetSystemFirmwareTable(RSMB, 0, Some(&mut raw)) } as usize;
    describe(raw.get(HEADER..read.min(len))?)
}

#[derive(PartialEq, Debug)]
struct Module {
    mb: u32,
    kind: Option<&'static str>,
    speed: u16,
}

fn describe(table: &[u8]) -> Option<String> {
    let modules = modules(table);
    let first = modules.first()?;
    let same_size = modules.iter().all(|m| m.mb == first.mb);
    let amount = match (same_size, first.mb % 1024) {
        (true, 0) => format!("{} × {} GB", modules.len(), first.mb / 1024),
        (true, _) => format!("{} × {} MB", modules.len(), first.mb),
        (false, _) => format!("{} modules", modules.len()),
    };
    Some(match (first.kind, first.speed) {
        (Some(kind), 0) => format!("{kind}, {amount}"),
        (Some(kind), speed) => format!("{kind}-{speed}, {amount}"),
        (None, _) => amount,
    })
}

/// Populated memory slots.
fn modules(table: &[u8]) -> Vec<Module> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(&[kind, len]) = table.get(at..at + 2).and_then(|h| h.first_chunk::<2>()) {
        let len = len as usize;
        if kind == END_OF_TABLE || len < 4 {
            break;
        }
        if kind == MEMORY_DEVICE
            && let Some(m) = table.get(at..at + len).and_then(module)
        {
            out.push(m);
        }
        // Each structure is followed by its strings, ended by a double NUL.
        let strings = at + len;
        let Some(end) = table.get(strings..).and_then(|t| t.windows(2).position(|w| w == [0, 0])) else { break };
        at = strings + end + 2;
    }
    out
}

fn module(s: &[u8]) -> Option<Module> {
    let word = |at: usize| s.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let mb = match word(0x0C)? {
        0 | 0xFFFF => return None,
        // Too large for this field: the real size is in the extended one.
        0x7FFF => u32::from_le_bytes(s.get(0x1C..0x20)?.try_into().ok()?) & 0x7FFF_FFFF,
        v if v & 0x8000 != 0 => (v & 0x7FFF) as u32 / 1024,
        v => v as u32,
    };
    let kind = match *s.get(0x12)? {
        0x18 => Some("DDR3"),
        0x1A => Some("DDR4"),
        0x1D => Some("LPDDR3"),
        0x1E => Some("LPDDR4"),
        0x22 => Some("DDR5"),
        0x23 => Some("LPDDR5"),
        _ => None,
    };
    // The speed it actually runs at, where the firmware reports one.
    let rated = word(0x15).unwrap_or(0);
    let speed = word(0x20).filter(|&v| v != 0 && v != 0xFFFF).unwrap_or(rated);
    Some(Module { mb, kind, speed })
}

#[cfg(test)]
mod tests {
    use super::{describe, memory_spec};

    /// A type 17 structure with no strings.
    fn dimm(size: u16, kind: u8, rated: u16, configured: u16) -> Vec<u8> {
        let mut s = vec![0u8; 0x22];
        s[0] = 17;
        s[1] = 0x22;
        s[0x0C..0x0E].copy_from_slice(&size.to_le_bytes());
        s[0x12] = kind;
        s[0x15..0x17].copy_from_slice(&rated.to_le_bytes());
        s[0x20..0x22].copy_from_slice(&configured.to_le_bytes());
        s.extend([0, 0]);
        s
    }

    fn table(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut t = parts.concat();
        t.extend([127, 4, 0, 0, 0, 0]);
        t
    }

    #[test]
    fn matched_modules_read_as_a_kit() {
        let t = table(&[dimm(16384, 0x22, 5600, 4800), dimm(16384, 0x22, 5600, 4800)]);
        assert_eq!(describe(&t).as_deref(), Some("DDR5-4800, 2 × 16 GB"));
    }

    #[test]
    fn empty_slots_are_ignored() {
        let t = table(&[dimm(8192, 0x1A, 3200, 0), dimm(0, 0x02, 0, 0)]);
        assert_eq!(describe(&t).as_deref(), Some("DDR4-3200, 1 × 8 GB"));
    }

    #[test]
    fn mixed_sizes_and_unknown_types_stay_honest() {
        let t = table(&[dimm(8192, 0x02, 0, 0), dimm(4096, 0x02, 0, 0)]);
        assert_eq!(describe(&t).as_deref(), Some("2 modules"));
    }

    #[test]
    fn a_truncated_table_yields_nothing() {
        assert_eq!(describe(&[17, 0x22, 0]), None);
        assert_eq!(describe(&[]), None);
    }

    #[test]
    fn this_pc_reports_something_sensible() {
        // Not every firmware fills this in, so only the shape is checked.
        if let Some(spec) = memory_spec() {
            println!("{spec}");
            assert!(!spec.is_empty());
        }
    }
}
