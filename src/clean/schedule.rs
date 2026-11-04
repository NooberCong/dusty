//! Automatic cleaning. A scheduled task starts this exe as the user, with no
//! elevation, to clean the ticked items and exit. It never prompts: nobody may
//! be there to answer, so without the system task its items wait for a clean
//! started by hand.

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToFileTime};

use super::catalog::{Action, ITEMS, Item};
use super::helper::Event;
use super::volcache::ComScope;
use super::{log, run, task};
use crate::settings::{Plan, Schedule, Settings, WEEKDAYS};
use crate::win::exe_path;

const TASK: &str = "dusty automatic clean";
const TICKS_PER_DAY: u64 = 864_000_000_000;

/// Registers `plan`, or removes the task if it's off. None means it failed.
pub fn set(plan: Plan) -> Option<()> {
    let _com = ComScope::enter();
    let Some(repeat) = repeat_xml(plan) else {
        task::unregister(TASK);
        return Some(());
    };
    let start = start_boundary(unsafe { GetLocalTime() }, plan.hour)?;
    task::register(TASK, &task_xml(&exe_path().to_string_lossy(), &start, &repeat))
}

fn repeat_xml(plan: Plan) -> Option<String> {
    const MONTHS: &str = "<January/><February/><March/><April/><May/><June/><July/><August/><September/><October/><November/><December/>";
    match plan.every {
        Schedule::Off => None,
        Schedule::Daily => Some("<ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay>".into()),
        Schedule::Weekly => {
            let day = WEEKDAYS[plan.weekday as usize];
            Some(format!("<ScheduleByWeek><DaysOfWeek><{day}/></DaysOfWeek><WeeksInterval>1</WeeksInterval></ScheduleByWeek>"))
        }
        Schedule::Monthly => {
            let day = plan.month_day;
            Some(format!("<ScheduleByMonth><DaysOfMonth><Day>{day}</Day></DaysOfMonth><Months>{MONTHS}</Months></ScheduleByMonth>"))
        }
    }
}

/// The next time `hour` comes round. The repeat picks the day from there. A
/// start already past could fire the moment it's registered, and setting a
/// schedule shouldn't clean anything by itself.
fn start_boundary(now: SYSTEMTIME, hour: u8) -> Option<String> {
    let days = (now.wHour >= hour as u16) as u64;
    let mut time = FILETIME::default();
    unsafe { SystemTimeToFileTime(&now, &mut time) }.ok()?;
    let ticks = ((time.dwHighDateTime as u64) << 32 | time.dwLowDateTime as u64) + days * TICKS_PER_DAY;
    let later = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut date = SYSTEMTIME::default();
    unsafe { FileTimeToSystemTime(&later, &mut date) }.ok()?;
    Some(format!("{:04}-{:02}-{:02}T{hour:02}:00:00", date.wYear, date.wMonth, date.wDay))
}

/// Emptying the Recycle Bin can lose a file someone still wants, and that
/// takes a person's say-so each time. So does anything the task won't touch.
fn runs_unattended(item: &Item) -> bool {
    !matches!(item.action, Action::RecycleBin) && (!item.admin || task::allowed(item, true))
}

fn items(settings: &Settings) -> Vec<&'static Item> {
    ITEMS.iter().filter(|i| settings.is_selected(i.id) && runs_unattended(i)).collect()
}

/// Entry point of the scheduled task.
pub fn serve() -> i32 {
    let settings = Settings::load();
    if settings.plan.every == Schedule::Off {
        return 0;
    }
    let (system, own): (Vec<_>, Vec<_>) = items(&settings).into_iter().partition(|i| i.admin);
    let mut cleaned: Vec<_> = own.into_iter().map(|item| (item, run(item, true))).collect();
    let ids: Vec<&str> = system.iter().map(|i| i.id).collect();
    if !ids.is_empty() && task::covers(&ids, true) {
        let mut keep = |event| {
            if let Event::Finished(item, outcome) = event {
                cleaned.push((item, outcome));
            }
        };
        task::run(&ids, true, &mut keep);
    }
    log(&cleaned);
    0
}

fn task_xml(exe: &str, start: &str, repeat: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.3" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Runs dusty's automatic cleaning.</Description></RegistrationInfo>
  <Triggers><CalendarTrigger><StartBoundary>{start}</StartBoundary>{repeat}</CalendarTrigger></Triggers>
  <Principals><Principal id="Author"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>PT3H</ExecutionTimeLimit>
  </Settings>
  <Actions Context="Author"><Exec><Command>{}</Command><Arguments>--scheduled-clean</Arguments></Exec></Actions>
</Task>"#,
        task::escape(exe)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_is_always_still_to_come() {
        let now = SYSTEMTIME { wYear: 2026, wMonth: 12, wDay: 31, wHour: 18, ..Default::default() };
        assert_eq!(start_boundary(now, 21).unwrap(), "2026-12-31T21:00:00");
        assert_eq!(start_boundary(now, 18).unwrap(), "2027-01-01T18:00:00");
        assert_eq!(start_boundary(now, 9).unwrap(), "2027-01-01T09:00:00");
    }

    #[test]
    fn the_picked_day_reaches_the_task() {
        let plan = Plan { weekday: 2, month_day: 15, ..Plan::default() };
        assert!(repeat_xml(Plan { every: Schedule::Weekly, ..plan }).unwrap().contains("<Wednesday/>"));
        assert!(repeat_xml(Plan { every: Schedule::Monthly, ..plan }).unwrap().contains("<Day>15</Day>"));
        assert!(repeat_xml(Plan { every: Schedule::Off, ..plan }).is_none());
    }

    #[test]
    fn only_ticked_items_run_and_never_the_recycle_bin_or_old_windows() {
        let selected = ["temp", "recycle", "wintemp", "old-windows", "dev"].map(String::from).to_vec();
        let settings = Settings { selected, ..Default::default() };
        assert_eq!(items(&settings).iter().map(|i| i.id).collect::<Vec<_>>(), ["temp", "wintemp", "dev"]);
    }
}
