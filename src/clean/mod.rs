//! Cleanup engine: measures or removes catalogue items, in-process or via the
//! elevated helper for items that need admin.

pub mod catalog;
mod fs;
pub mod helper;
pub mod schedule;
pub mod task;
mod volcache;

use std::collections::HashSet;
use std::io::Write;

use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Shell::{
    SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW, SHQUERYRBINFO,
    SHQueryRecycleBinW,
};
use windows::core::PCWSTR;

pub use catalog::{ITEMS, Item, Tier};
pub use fs::Tally;

use crate::win::{Handle, env_path, from_wide, windows_dir};
use catalog::Action;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub tally: Tally,
    /// Short user-facing remark, e.g. "Edge is open, skipped".
    pub note: Option<String>,
    /// Nothing could be measured or cleaned (missing rights, handler error).
    pub failed: bool,
    /// What Windows still calls removable after cleaning it.
    pub left: u64,
}

/// Measures (`delete == false`) or cleans one item in this process.
pub fn run(item: &Item, delete: bool) -> Outcome {
    match &item.action {
        Action::Paths(targets) => run_paths(&targets(), delete),
        Action::RecycleBin => recycle_bin(delete),
        Action::Handlers(keys) => run_handlers(keys, delete),
    }
}

fn run_paths(targets: &[catalog::Target], delete: bool) -> Outcome {
    let running = if targets.iter().any(|t| t.guard.is_some()) { running_processes() } else { HashSet::new() };
    let mut out = Outcome::default();
    let mut skipped: Vec<&str> = Vec::new();
    for t in targets {
        if let Some((exe, name)) = t.guard
            && running.contains(exe)
        {
            if !skipped.contains(&name) {
                skipped.push(name);
            }
            continue;
        }
        out.tally += fs::sweep(&t.path, t.filter, delete);
    }
    if !skipped.is_empty() {
        let verb = if skipped.len() == 1 { "is" } else { "are" };
        out.note = Some(format!("{} {verb} open, skipped", join_names(&skipped)));
    }
    out
}

fn join_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn recycle_bin(delete: bool) -> Outcome {
    let mut info = SHQUERYRBINFO { cbSize: size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    if unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info) }.is_err() {
        return Outcome { failed: true, ..Default::default() };
    }
    let tally = Tally { files: info.i64NumItems.max(0) as u64, bytes: info.i64Size.max(0) as u64 };
    if delete && tally.files > 0 {
        let flags = SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND;
        if unsafe { SHEmptyRecycleBinW(None, PCWSTR::null(), flags) }.is_err() {
            return Outcome { failed: true, ..Default::default() };
        }
    }
    Outcome { tally, ..Default::default() }
}

fn run_handlers(keys: &[&str], delete: bool) -> Outcome {
    let _com = volcache::ComScope::enter();
    let windows = windows_dir().unwrap_or_else(|| r"C:\Windows".into());
    let volume = windows.ancestors().last().unwrap_or(&windows).to_string_lossy().into_owned();
    let mut out = Outcome::default();
    let mut ok = 0;
    for key in keys {
        if let Ok((bytes, left)) = volcache::run(key, &volume, delete) {
            out.tally.bytes += bytes;
            out.left += left;
            ok += 1;
        }
    }
    out.failed = ok == 0;
    out
}

fn running_processes() -> HashSet<String> {
    let mut set = HashSet::new();
    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else { return set };
    let snap = Handle(snap);
    let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut more = unsafe { Process32FirstW(snap.0, &mut e) }.is_ok();
    while more {
        set.insert(from_wide(&e.szExeFile).to_ascii_lowercase());
        more = unsafe { Process32NextW(snap.0, &mut e) }.is_ok();
    }
    set
}

/// Appends one line per cleaned item to %LOCALAPPDATA%\dusty\clean.log.
pub fn log(results: &[(&Item, Outcome)]) {
    let Some(dir) = env_path("LOCALAPPDATA").map(|l| l.join("dusty")) else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("clean.log")) else {
        return;
    };
    let t = unsafe { GetLocalTime() };
    for (item, o) in results {
        let status = if o.failed { "failed" } else { "ok" };
        let _ = writeln!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}\t{}\t{}\t{} files\t{} bytes\t{}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, item.id, status, o.tally.files, o.tally.bytes,
            o.note.as_deref().unwrap_or("")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_join_naturally() {
        assert_eq!(join_names(&["Edge"]), "Edge");
        assert_eq!(join_names(&["Chrome", "Edge"]), "Chrome and Edge");
        assert_eq!(join_names(&["Chrome", "Edge", "Brave"]), "Chrome, Edge and Brave");
    }

    #[test]
    fn dry_run_of_every_unprivileged_item_succeeds() {
        for item in ITEMS.iter().filter(|i| !i.admin) {
            let o = run(item, false);
            assert!(!o.failed, "{} failed", item.id);
        }
    }
}
