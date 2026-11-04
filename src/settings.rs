//! Persisted preferences in %APPDATA%\dusty\settings.ini (key=value lines).
//! "Start with Windows" lives in the HKCU Run key itself, never duplicated here.

use std::path::PathBuf;

use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

use crate::clean::ITEMS;
use crate::win::{env_path, exe_path, wide};

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum ThemeMode {
    /// Follows the Windows app theme.
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    /// In display order. The labels are also how the file spells them.
    pub const ALL: [(ThemeMode, &str); 3] =
        [(ThemeMode::System, "System"), (ThemeMode::Light, "Light"), (ThemeMode::Dark, "Dark")];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|(m, _)| *m == self).unwrap_or(0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub overlay: bool,
    pub theme: ThemeMode,
    /// Ids of the cleanup items the user has ticked.
    pub selected: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            overlay: true,
            theme: ThemeMode::default(),
            selected: ITEMS.iter().filter(|i| i.default_on).map(|i| i.id.to_string()).collect(),
        }
    }
}

fn file() -> Option<PathBuf> {
    env_path("APPDATA").map(|d| d.join("dusty").join("settings.ini"))
}

impl Settings {
    pub fn load() -> Self {
        let text = file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
        Self::parse(&text)
    }

    fn parse(text: &str) -> Self {
        let mut s = Settings::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                "overlay" => s.overlay = v.trim() == "1",
                "theme" => s.theme = ThemeMode::ALL.iter().find(|(_, l)| *l == v.trim()).map_or(s.theme, |(m, _)| *m),
                "selected" => {
                    s.selected = v
                        .split(',')
                        .map(str::trim)
                        .filter(|id| ITEMS.iter().any(|i| i.id == *id))
                        .map(String::from)
                        .collect()
                }
                _ => {}
            }
        }
        s
    }

    fn serialize(&self) -> String {
        let theme = ThemeMode::ALL[self.theme.index()].1;
        format!("overlay={}\ntheme={theme}\nselected={}\n", self.overlay as u8, self.selected.join(","))
    }

    pub fn save(&self) {
        let Some(f) = file() else { return };
        if let Some(dir) = f.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(f, self.serialize()).ok();
    }

    pub fn is_selected(&self, id: &str) -> bool {
        self.selected.iter().any(|s| s == id)
    }

    pub fn set_selected(&mut self, id: &str, on: bool) {
        self.selected.retain(|s| s != id);
        if on {
            self.selected.push(id.to_string());
        }
    }
}

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("dusty");

fn run_key(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Option<HKEY> {
    let mut key = HKEY::default();
    unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, access, &mut key) }.ok().ok()?;
    Some(key)
}

/// True when the Run entry exists and points at this exe.
pub fn autostart_enabled() -> bool {
    let Some(key) = run_key(KEY_READ) else { return false };
    let mut buf = [0u16; 1024];
    let mut size = size_of_val(&buf) as u32;
    let ok = unsafe { RegQueryValueExW(key, RUN_VALUE, None, None, Some(buf.as_mut_ptr() as *mut u8), Some(&mut size)) }
        .is_ok();
    unsafe { RegCloseKey(key) }.ok().ok();
    ok && crate::win::from_wide(&buf).to_lowercase().contains(&exe_path().to_string_lossy().to_lowercase())
}

pub fn set_autostart(on: bool) {
    let Some(key) = run_key(KEY_SET_VALUE) else { return };
    unsafe {
        if on {
            let cmd = wide(format!("\"{}\" --tray", exe_path().display()));
            let bytes = std::slice::from_raw_parts(cmd.as_ptr() as *const u8, cmd.len() * 2);
            RegSetValueExW(key, RUN_VALUE, None, REG_SZ, Some(bytes)).ok().ok();
        } else {
            RegDeleteValueW(key, RUN_VALUE).ok().ok();
        }
        RegCloseKey(key).ok().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_unknown_ids_dropped() {
        let s = Settings { overlay: false, theme: ThemeMode::Dark, selected: vec!["temp".into(), "dev".into()] };
        assert_eq!(Settings::parse(&s.serialize()), s);
        let p = Settings::parse("overlay=1\nselected=temp, bogus ,recycle\n");
        assert_eq!(p.selected, vec!["temp", "recycle"]);
        assert!(p.overlay);
    }

    #[test]
    fn an_unknown_theme_falls_back_to_system() {
        assert_eq!(Settings::parse("theme=Light\n").theme, ThemeMode::Light);
        assert_eq!(Settings::parse("theme=neon\n").theme, ThemeMode::System);
    }

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Settings::parse(""), Settings::default());
    }
}
