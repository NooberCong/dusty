//! Cleans system items with no UAC prompt: a scheduled task that runs elevated
//! on demand. The first approved prompt sets it up, with no setting to find.
//!
//! Anything running as the user can start this task, so it trusts nothing
//! from them. It runs a copy of dusty in Program Files, which only admins can
//! replace. Its request names catalogue items and nothing else, and only the
//! items `allowed` lets through. It reports into Program Files as well, so it
//! never writes to a path the user's programs control.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::TaskScheduler::{
    IRegisteredTask, ITaskFolder, ITaskService, TASK_CREATE_OR_UPDATE, TASK_LOGON_INTERACTIVE_TOKEN, TASK_STATE_RUNNING, TaskScheduler,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::core::BSTR;

use super::catalog::{self, Item};
use super::helper::{self, Event};
use super::volcache::ComScope;
use crate::win::{env_path, exe_path};

const TASK: &str = "dusty system clean";
const POLL: Duration = Duration::from_millis(150);
/// A task that hasn't reported by now isn't going to.
const START_TIMEOUT: Duration = Duration::from_secs(15);
const DONE: &str = "done\n";
const BUSY: &str = "another clean is running";

/// What the task may touch. Items in the user's own folders would have it
/// deleting on behalf of whatever program asked, and items off by default,
/// like the previous Windows version, are too final to delete without a
/// prompt, though measuring them is harmless. Windows\Temp is in, and users
/// can plant links there, which is why `fs` opens nothing below a root by path.
pub fn allowed(item: &Item, delete: bool) -> bool {
    item.admin && (item.default_on || !delete)
}

fn install_dir() -> Option<PathBuf> {
    let path = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, None) }.ok()?;
    let dir = unsafe { path.to_string() }.ok();
    unsafe { CoTaskMemFree(Some(path.0 as _)) };
    Some(PathBuf::from(dir?).join("dusty"))
}

fn installed_exe() -> Option<PathBuf> {
    Some(install_dir()?.join("dusty.exe"))
}

fn report_path() -> Option<PathBuf> {
    Some(install_dir()?.join("report.tsv"))
}

fn request_path() -> Option<PathBuf> {
    Some(env_path("LOCALAPPDATA")?.join("dusty").join("system-clean.request"))
}

/// A copy from an older build may not speak the same protocol.
fn up_to_date() -> bool {
    let read = |p: Option<PathBuf>| p.and_then(|p| std::fs::read(p).ok());
    let installed = read(installed_exe());
    installed.is_some() && installed == read(Some(exe_path()))
}

/// Whether the task can take all of `ids`, so that no prompt is needed.
pub fn covers(ids: &[&str], delete: bool) -> bool {
    ids.iter().all(|id| catalog::find(id).is_some_and(|i| allowed(i, delete))) && up_to_date()
}

fn tasks() -> windows::core::Result<ITaskFolder> {
    let none = VARIANT::default();
    unsafe {
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        service.Connect(&none, &none, &none, &none)?;
        service.GetFolder(&BSTR::from("\\"))
    }
}

/// Runs `ids` through the task, reporting as it goes. None means it never
/// started and nothing was cleaned, so the caller should ask for elevation.
/// The task runs one clean at a time, so while another is going this reports
/// an error, and that other clean is left to finish.
pub fn run(ids: &[&str], delete: bool, on_event: &mut impl FnMut(Event)) -> Option<io::Result<()>> {
    let (report, request) = (report_path()?, request_path()?);
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let _com = ComScope::enter();
    let task = unsafe { tasks().ok()?.GetTask(&BSTR::from(TASK)) }.ok()?;
    if is_running(&task) {
        return Some(Err(busy()));
    }
    std::fs::create_dir_all(request.parent()?).ok()?;
    std::fs::write(&request, format_request(nonce, delete, ids)).ok()?;
    let result = follow(&task, &report, nonce, on_event);
    std::fs::remove_file(&request).ok();
    result
}

fn is_running(task: &IRegisteredTask) -> bool {
    unsafe { task.State() }.is_ok_and(|s| s == TASK_STATE_RUNNING)
}

fn busy() -> io::Error {
    io::Error::other(BUSY)
}

fn follow(
    task: &IRegisteredTask,
    report: &std::path::Path,
    nonce: u128,
    on_event: &mut impl FnMut(Event),
) -> Option<io::Result<()>> {
    unsafe { task.Run(&VARIANT::default()) }.ok()?;
    let header = header(nonce);
    let started = Instant::now();
    let mut seen = header.len();
    loop {
        std::thread::sleep(POLL);
        // State first: if it has stopped, the report read after it is final.
        let running = is_running(task);
        let text = std::fs::read_to_string(report).unwrap_or_default();
        // Until the header matches, this is the last run's report.
        if !text.starts_with(&header) {
            if started.elapsed() > START_TIMEOUT {
                // Running without our header means it's someone else's clean,
                // so it's never stopped. A run of ours starting after this
                // is harmless: the caller takes the request away.
                return if running { Some(Err(busy())) } else { None };
            }
            continue;
        }
        seen += helper::take_events(text.get(seen..).unwrap_or(""), on_event);
        if text.ends_with(DONE) {
            return Some(Ok(()));
        }
        if !running {
            return Some(Err(io::Error::other("it stopped unexpectedly")));
        }
    }
}

fn header(nonce: u128) -> String {
    format!("run\t{nonce}\n")
}

fn format_request(nonce: u128, delete: bool, ids: &[&str]) -> String {
    format!("{nonce} {} {}", if delete { "clean" } else { "scan" }, ids.join(" "))
}

/// The request's nonce, mode and the items it may act on. Anything else in
/// the file is dropped: any program can have written it.
fn parse_request(text: &str) -> Option<(u128, bool, Vec<&'static Item>)> {
    let mut words = text.split_whitespace();
    let nonce = words.next()?.parse().ok()?;
    let delete = match words.next()? {
        "scan" => false,
        "clean" => true,
        _ => return None,
    };
    Some((nonce, delete, words.filter_map(catalog::find).filter(|i| allowed(i, delete)).collect()))
}

/// Entry point of the task itself. The request stays where it is: removing
/// files in the user's folders is no job for an elevated process.
pub fn serve() -> i32 {
    let Some(text) = request_path().and_then(|p| std::fs::read_to_string(p).ok()) else { return 2 };
    let Some((nonce, delete, items)) = parse_request(&text) else { return 2 };
    let Some(path) = report_path() else { return 4 };
    let Ok(mut report) = std::fs::File::create(path) else { return 4 };
    let wrote = report.write_all(header(nonce).as_bytes()).is_ok()
        && helper::work(&items, delete, &mut report)
        && report.write_all(DONE.as_bytes()).is_ok();
    if wrote { 0 } else { 4 }
}

/// Installs this build as the task, or brings it up to date. Runs after any
/// approved prompt, already elevated, so that prompt is the last one.
pub fn setup() -> i32 {
    let _com = ComScope::enter();
    if install().is_some() { 0 } else { 1 }
}

fn install() -> Option<()> {
    std::fs::create_dir_all(install_dir()?).ok()?;
    std::fs::copy(exe_path(), installed_exe()?).ok()?;
    register(TASK, &task_xml(&installed_exe()?.to_string_lossy(), &install_dir()?.to_string_lossy()))
}

/// Registers a task for the current user, replacing any of that name.
pub(super) fn register(name: &str, xml: &str) -> Option<()> {
    let user = format!("{}\\{}", std::env::var("USERDOMAIN").ok()?, std::env::var("USERNAME").ok()?);
    let (none, user) = (VARIANT::default(), VARIANT::from(BSTR::from(user)));
    unsafe {
        tasks()
            .ok()?
            .RegisterTask(&BSTR::from(name), &BSTR::from(xml), TASK_CREATE_OR_UPDATE.0, &user, &none, TASK_LOGON_INTERACTIVE_TOKEN, &none)
            .ok()?;
    }
    Some(())
}

/// Already gone is fine: the point is that it ends up gone.
pub(super) fn unregister(name: &str) {
    if let Ok(folder) = tasks() {
        unsafe { folder.DeleteTask(&BSTR::from(name), 0) }.ok();
    }
}

/// Takes the task and its copy out again. Needs an elevated console.
pub fn uninstall() -> Option<()> {
    let _com = ComScope::enter();
    unregister(TASK);
    std::fs::remove_file(report_path()?).ok();
    let exe = installed_exe()?;
    if exe.exists() {
        std::fs::remove_file(exe).ok()?;
    }
    std::fs::remove_dir(install_dir()?).ok();
    Some(())
}

pub(super) fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;")
}

fn task_xml(exe: &str, dir: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.3" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Lets dusty clean system items without an administrator prompt.</Description></RegistrationInfo>
  <Principals><Principal id="Author"><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <ExecutionTimeLimit>PT3H</ExecutionTimeLimit>
  </Settings>
  <Actions Context="Author"><Exec><Command>{}</Command><Arguments>--helper-task</Arguments><WorkingDirectory>{}</WorkingDirectory></Exec></Actions>
</Task>"#,
        escape(exe),
        escape(dir)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_roundtrips() {
        let (nonce, delete, items) = parse_request(&format_request(7, true, &["wintemp", "updates"])).unwrap();
        assert_eq!((nonce, delete), (7, true));
        assert_eq!(items.iter().map(|i| i.id).collect::<Vec<_>>(), ["wintemp", "updates"]);
    }

    #[test]
    fn a_request_can_only_name_system_items_that_are_safe_unattended() {
        let (.., items) = parse_request(r"1 clean temp old-windows C:\Windows dev wintemp").unwrap();
        assert_eq!(items.iter().map(|i| i.id).collect::<Vec<_>>(), ["wintemp"]);
    }

    #[test]
    fn measuring_may_reach_items_that_cleaning_may_not() {
        let (.., items) = parse_request("1 scan temp old-windows").unwrap();
        assert_eq!(items.iter().map(|i| i.id).collect::<Vec<_>>(), ["old-windows"]);
    }

    /// Against the real task, so it needs one set up, and it only measures:
    /// `cargo test --release second_request -- --ignored`.
    #[test]
    #[ignore]
    fn a_second_request_leaves_the_running_one_alone() {
        // Measured over and over, to outlast START_TIMEOUT as a real clean would.
        let slow = ["updates", "drivers", "old-windows"].repeat(5);
        let count = slow.len();
        let first = std::thread::spawn(move || {
            let started = Instant::now();
            let mut finished = 0;
            let result = run(&slow, false, &mut |event| finished += matches!(event, Event::Finished(..)) as usize);
            (result.map(|r| r.map_err(|e| e.to_string())), finished, started.elapsed())
        });
        std::thread::sleep(Duration::from_secs(3));

        let second = run(&["delivery"], false, &mut |_| {});
        let refused = matches!(&second, Some(Err(e)) if e.to_string() == BUSY);
        println!("second request: {second:?}");

        let (result, finished, took) = first.join().unwrap();
        println!("first request: {result:?}, {finished} items, took {took:?}");
        assert!(took > START_TIMEOUT + Duration::from_secs(3), "the first run was over too soon to prove anything");
        assert!(refused, "the second request should be told a clean is running");
        assert_eq!(result, Some(Ok(())), "the first run must not be stopped");
        assert_eq!(finished, count);
    }

    #[test]
    fn a_malformed_request_is_refused() {
        assert!(parse_request("").is_none());
        assert!(parse_request("x clean wintemp").is_none());
        assert!(parse_request("1 format wintemp").is_none());
    }
}
