//! Elevated helper. The UI runs unelevated; for admin items it uses the
//! scheduled task when it can, and otherwise relaunches this same exe with
//! `--helper` through a UAC prompt, which also sets up the task. The helper
//! only accepts catalogue ids (never paths), so it cannot be steered
//! into deleting anything outside the catalogue. Progress comes back as
//! tab-separated lines in a temp file the parent chose and reads as it grows.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_CANCELLED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{PCWSTR, w};

use super::{Outcome, Tally, catalog, task};
use crate::win::{Handle, exe_path, wide};

/// How often the parent looks for new progress.
const POLL_MS: u32 = 150;
/// Marks an item the helper has just started on.
const STARTED: &str = "started";

pub enum Event {
    Started(&'static catalog::Item),
    Finished(&'static catalog::Item, Outcome),
}

/// Runs `ids` elevated, reporting each item as it starts and finishes, and
/// waits. Goes through the scheduled task when it can, so there's no prompt.
/// `Err(Interrupted)` means the user declined UAC.
pub fn run_elevated(ids: &[&str], delete: bool, mut on_event: impl FnMut(Event)) -> io::Result<()> {
    if task::covers(ids, delete)
        && let Some(result) = task::run(ids, delete, &mut on_event)
    {
        return result;
    }
    let out = std::env::temp_dir().join(format!(
        "dusty-{}-{}.tsv",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    let params = format!("--helper {} \"{}\" {}", if delete { "clean" } else { "scan" }, out.display(), ids.join(" "));
    let process = elevate(&params)?;
    let mut seen = 0;
    let mut read_new = || {
        let text = std::fs::read_to_string(&out).unwrap_or_default();
        seen += take_events(text.get(seen..).unwrap_or(""), &mut on_event);
    };
    loop {
        let wait = unsafe { WaitForSingleObject(process.0, POLL_MS) };
        read_new();
        if wait == WAIT_OBJECT_0 {
            break;
        }
        if wait != WAIT_TIMEOUT {
            return Err(io::Error::other("helper did not finish"));
        }
    }
    let mut code = 1u32;
    unsafe { GetExitCodeProcess(process.0, &mut code) }.ok();
    std::fs::remove_file(&out).ok();
    if code != 0 {
        let why = match code {
            2 => "bad arguments",
            3 => "result path rejected",
            4 => "could not write its report",
            _ => "it stopped unexpectedly",
        };
        return Err(io::Error::other(format!("{why} (code {code:#x})")));
    }
    Ok(())
}

/// Sets up the task through a UAC prompt, with no cleaning to go with it.
pub fn install_task() -> io::Result<()> {
    let process = elevate("--helper-setup")?;
    if unsafe { WaitForSingleObject(process.0, INFINITE) } != WAIT_OBJECT_0 {
        return Err(io::Error::other("setup did not finish"));
    }
    let mut code = 1u32;
    unsafe { GetExitCodeProcess(process.0, &mut code) }.ok();
    if code == 0 { Ok(()) } else { Err(io::Error::other("setup failed")) }
}

/// Starts this exe through a UAC prompt. `Err(Interrupted)` means it was declined.
fn elevate(params: &str) -> io::Result<Handle> {
    let (file, params) = (wide(exe_path()), wide(params));
    // The shell may hand "runas" to COM components; the calling thread needs COM.
    let _com = super::volcache::ComScope::enter();
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    if let Err(e) = unsafe { ShellExecuteExW(&mut sei) } {
        return Err(if e.code() == ERROR_CANCELLED.to_hresult() {
            io::Error::from(io::ErrorKind::Interrupted)
        } else {
            io::Error::other(e.message())
        });
    }
    Ok(Handle(sei.hProcess))
}

/// Entry point inside the elevated process: `--helper <scan|clean> <out> <ids...>`.
/// Exit codes: 0 ok, 2 bad arguments, 3 result path rejected, 4 result not written.
pub fn serve(args: &[String]) -> i32 {
    let [mode, out, ids @ ..] = args else { return 2 };
    let delete = match mode.as_str() {
        "scan" => false,
        "clean" => true,
        _ => return 2,
    };
    let out = PathBuf::from(out);
    if !is_result_path(&out) {
        return 3;
    }
    // create_new: never overwrite, and never write through a planted link.
    let Ok(mut report) = std::fs::OpenOptions::new().write(true).create_new(true).open(&out) else { return 4 };
    let items: Vec<&catalog::Item> = ids.iter().filter_map(|id| catalog::find(id)).collect();
    let written = work(&items, delete, &mut report);
    task::setup();
    if written { 0 } else { 4 }
}

/// Runs `items`, writing progress to `report`. Returns whether it all got
/// written. The parent keeps the clean log: an elevated process has no
/// business writing into the user's own folders.
pub(super) fn work(items: &[&catalog::Item], delete: bool, report: &mut impl Write) -> bool {
    let mut written = true;
    for item in items {
        written &= report.write_all(format!("{STARTED}\t{}\n", item.id).as_bytes()).is_ok();
        let outcome = super::run(item, delete);
        written &= report.write_all(finished_line(item, &outcome).as_bytes()).is_ok();
    }
    written
}

/// An absolute `dusty-*.tsv` path. The helper is started only through a UAC
/// prompt, writes only this small report and refuses to replace any file.
fn is_result_path(p: &Path) -> bool {
    p.is_absolute()
        && p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("dusty-") && n.ends_with(".tsv"))
}

fn finished_line(item: &catalog::Item, o: &Outcome) -> String {
    let note = o.note.as_deref().unwrap_or("").replace(['\t', '\n'], " ");
    format!("{}\t{}\t{}\t{}\t{}\n", item.id, o.tally.files, o.tally.bytes, o.failed as u8, note)
}

fn parse(line: &str) -> Option<Event> {
    let mut f = line.split('\t');
    let first = f.next()?;
    if first == STARTED {
        return Some(Event::Started(catalog::find(f.next()?)?));
    }
    let item = catalog::find(first)?;
    let files = f.next()?.parse().ok()?;
    let bytes = f.next()?.parse().ok()?;
    let failed = f.next()? == "1";
    let note = f.next().filter(|n| !n.is_empty()).map(String::from);
    Some(Event::Finished(item, Outcome { tally: Tally { files, bytes }, note, failed }))
}

/// Reports every complete line in `text` and returns the bytes they took up.
/// A line still being written is left for the next read.
pub(super) fn take_events(text: &str, on_event: &mut impl FnMut(Event)) -> usize {
    let complete = text.rfind('\n').map_or(0, |i| i + 1);
    text[..complete].lines().filter_map(parse).for_each(on_event);
    complete
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for o in [
            Outcome { tally: Tally { files: 3, bytes: 99 }, note: None, failed: false },
            Outcome { tally: Tally::default(), note: Some("Edge is open, skipped".into()), failed: true },
        ] {
            let item = catalog::find("temp").unwrap();
            let line = finished_line(item, &o);
            let parsed = parse(line.trim_end_matches('\n'));
            assert!(matches!(parsed, Some(Event::Finished(i, got)) if i.id == "temp" && got == o));
        }
    }

    #[test]
    fn a_half_written_line_waits_for_the_next_read() {
        let mut events = Vec::new();
        let text = "started\ttemp\nstarted\tcra";
        let used = take_events(text, &mut |e| events.push(e));
        assert_eq!(used, "started\ttemp\n".len());
        assert!(matches!(events[..], [Event::Started(i)] if i.id == "temp"));
    }

    #[test]
    fn rejects_foreign_ids_and_paths() {
        assert!(parse("../etc\t1\t1\t0\t").is_none());
        assert!(parse("started\t../etc").is_none());
        assert!(!is_result_path(Path::new(r"C:\Windows\System32\evil.tsv")));
        assert!(!is_result_path(Path::new("dusty-1-2.tsv")));
        assert!(is_result_path(&std::env::temp_dir().join("dusty-1-2.tsv")));
    }
}
