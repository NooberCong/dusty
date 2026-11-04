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

/// How often automatic cleaning runs.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Schedule {
    #[default]
    Off,
    Daily,
    Weekly,
    Monthly,
}

impl Schedule {
    /// In display order. The labels are also how the file spells them.
    pub const ALL: [(Schedule, &str); 4] = [
        (Schedule::Off, "Off"),
        (Schedule::Daily, "Daily"),
        (Schedule::Weekly, "Weekly"),
        (Schedule::Monthly, "Monthly"),
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|(s, _)| *s == self).unwrap_or(0)
    }
}

/// Monday first. Task Scheduler spells them the same way.
pub const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
/// Every month has this many days, so a monthly clean never skips one.
const LAST_MONTH_DAY: u8 = 28;

/// When automatic cleaning runs. It remembers both kinds of day, so trying
/// another frequency doesn't lose the one picked for this.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    pub every: Schedule,
    /// Index into `WEEKDAYS`.
    pub weekday: u8,
    /// 1 to 28.
    pub month_day: u8,
    /// 0 to 23.
    pub hour: u8,
}

impl Default for Plan {
    fn default() -> Self {
        Plan { every: Schedule::Off, weekday: 6, month_day: 1, hour: 12 }
    }
}

impl Plan {
    /// The picked day, moved on by `by` and wrapping round.
    pub fn step_day(self, by: i8) -> Self {
        match self.every {
            Schedule::Weekly => Plan { weekday: wrap(self.weekday, by, 7), ..self },
            Schedule::Monthly => Plan { month_day: wrap(self.month_day - 1, by, LAST_MONTH_DAY) + 1, ..self },
            _ => self,
        }
    }

    pub fn step_hour(self, by: i8) -> Self {
        Plan { hour: wrap(self.hour, by, 24), ..self }
    }

    /// What the day picker shows, for frequencies that have one.
    pub fn day_label(self) -> Option<String> {
        match self.every {
            Schedule::Weekly => Some(WEEKDAYS[self.weekday as usize].to_string()),
            Schedule::Monthly => Some(format!("Day {}", self.month_day)),
            _ => None,
        }
    }
}

fn wrap(value: u8, by: i8, count: u8) -> u8 {
    (value as i16 + by as i16).rem_euclid(count as i16) as u8
}

/// A number from the file, if it's below `limit` and at least `first`.
fn in_range(text: &str, first: u8, limit: u8) -> Option<u8> {
    text.trim().parse().ok().filter(|n| (first..limit).contains(n))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub overlay: bool,
    pub theme: ThemeMode,
    pub plan: Plan,
    /// Ids of the cleanup items the user has ticked.
    pub selected: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            overlay: true,
            theme: ThemeMode::default(),
            plan: Plan::default(),
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
                "schedule" => s.plan.every = Schedule::ALL.iter().find(|(_, l)| *l == v.trim()).map_or(s.plan.every, |(m, _)| *m),
                "weekday" => s.plan.weekday = in_range(v, 0, 7).unwrap_or(s.plan.weekday),
                "month_day" => s.plan.month_day = in_range(v, 1, LAST_MONTH_DAY + 1).unwrap_or(s.plan.month_day),
                "hour" => s.plan.hour = in_range(v, 0, 24).unwrap_or(s.plan.hour),
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
        let Plan { every, weekday, month_day, hour } = self.plan;
        let schedule = Schedule::ALL[every.index()].1;
        format!(
            "overlay={}\ntheme={theme}\nschedule={schedule}\nweekday={weekday}\nmonth_day={month_day}\nhour={hour}\nselected={}\n",
            self.overlay as u8,
            self.selected.join(",")
        )
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
        let selected = vec!["temp".into(), "dev".into()];
        let plan = Plan { every: Schedule::Weekly, weekday: 2, month_day: 15, hour: 21 };
        let s = Settings { overlay: false, theme: ThemeMode::Dark, plan, selected };
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
    fn automatic_cleaning_is_off_unless_the_file_says_otherwise() {
        assert_eq!(Settings::parse("schedule=Monthly\n").plan.every, Schedule::Monthly);
        assert_eq!(Settings::parse("schedule=hourly\n").plan.every, Schedule::Off);
        assert_eq!(Settings::parse("").plan.every, Schedule::Off);
    }

    #[test]
    fn a_day_or_hour_that_cannot_exist_is_ignored() {
        assert_eq!(Settings::parse("weekday=7\nmonth_day=29\nhour=24\n").plan, Plan::default());
        assert_eq!(Settings::parse("month_day=0\n").plan, Plan::default());
    }

    #[test]
    fn the_day_and_hour_wrap_round() {
        let weekly = Plan { every: Schedule::Weekly, weekday: 6, ..Plan::default() };
        assert_eq!(weekly.step_day(1).weekday, 0);
        assert_eq!(weekly.step_day(1).step_day(-1).weekday, 6);
        let monthly = Plan { every: Schedule::Monthly, month_day: 28, ..Plan::default() };
        assert_eq!(monthly.step_day(1).month_day, 1);
        assert_eq!(monthly.step_day(1).step_day(-1).month_day, 28);
        assert_eq!(Plan { hour: 23, ..Plan::default() }.step_hour(1).hour, 0);
        assert_eq!(Plan { hour: 0, ..Plan::default() }.step_hour(-1).hour, 23);
    }

    #[test]
    fn only_weekly_and_monthly_have_a_day_to_pick() {
        let plan = Plan { weekday: 0, month_day: 15, ..Plan::default() };
        assert_eq!(Plan { every: Schedule::Weekly, ..plan }.day_label().unwrap(), "Monday");
        assert_eq!(Plan { every: Schedule::Monthly, ..plan }.day_label().unwrap(), "Day 15");
        assert_eq!(Plan { every: Schedule::Daily, ..plan }.day_label(), None);
        assert_eq!(Plan { every: Schedule::Daily, ..plan }.step_day(1), Plan { every: Schedule::Daily, ..plan });
    }

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Settings::parse(""), Settings::default());
    }
}
