//! Elevated helper. The UI runs unelevated; for admin items it relaunches
//! this same exe with `--helper`, through one UAC prompt per batch. The
//! helper only accepts catalogue ids (never paths), so it cannot be steered
//! into deleting anything outside the catalogue. Results come back as one
//! tab-separated line per item in a temp file the parent chose.

use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_CANCELLED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{PCWSTR, w};

use super::{Outcome, Tally, catalog};
use crate::win::{Handle, exe_path, wide};

pub type Results = Vec<(&'static catalog::Item, Outcome)>;

/// Runs `ids` elevated and waits. `Err(Interrupted)` means the user declined UAC.
pub fn run_elevated(ids: &[&str], delete: bool) -> io::Result<Results> {
    let out = std::env::temp_dir().join(format!(
        "dusty-{}-{}.tsv",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    let params = format!("--helper {} \"{}\" {}", if delete { "clean" } else { "scan" }, out.display(), ids.join(" "));
    let (file, params) = (wide(exe_path()), wide(&params));
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
    let process = Handle(sei.hProcess);
    if unsafe { WaitForSingleObject(process.0, INFINITE) } != WAIT_OBJECT_0 {
        return Err(io::Error::other("helper did not finish"));
    }
    let mut code = 1u32;
    unsafe { GetExitCodeProcess(process.0, &mut code) }.ok();
    let text = std::fs::read_to_string(&out);
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
    Ok(parse(&text?))
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
    let items: Vec<&catalog::Item> = ids.iter().filter_map(|id| catalog::find(id)).collect();
    let results: Results = items.into_iter().map(|i| (i, super::run(i, delete))).collect();
    if delete {
        super::log(&results);
    }
    // create_new: never overwrite, and never write through a planted link.
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&out)
        .and_then(|mut f| std::io::Write::write_all(&mut f, format(&results).as_bytes()));
    if written.is_ok() { 0 } else { 4 }
}

/// An absolute `dusty-*.tsv` path. The helper is started only through a UAC
/// prompt, writes only this small report and refuses to replace any file.
fn is_result_path(p: &Path) -> bool {
    p.is_absolute()
        && p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("dusty-") && n.ends_with(".tsv"))
}

fn format(results: &Results) -> String {
    results
        .iter()
        .map(|(item, o)| {
            let note = o.note.as_deref().unwrap_or("").replace(['\t', '\n'], " ");
            format!("{}\t{}\t{}\t{}\t{}\n", item.id, o.tally.files, o.tally.bytes, o.failed as u8, note)
        })
        .collect()
}

fn parse(text: &str) -> Results {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let item = catalog::find(f.next()?)?;
            let files = f.next()?.parse().ok()?;
            let bytes = f.next()?.parse().ok()?;
            let failed = f.next()? == "1";
            let note = f.next().filter(|n| !n.is_empty()).map(String::from);
            Some((item, Outcome { tally: Tally { files, bytes }, note, failed }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let r: Results = vec![
            (catalog::find("temp").unwrap(), Outcome { tally: Tally { files: 3, bytes: 99 }, note: None, failed: false }),
            (
                catalog::find("browsers").unwrap(),
                Outcome { tally: Tally::default(), note: Some("Edge is open, skipped".into()), failed: true },
            ),
        ];
        let ids = |r: &Results| r.iter().map(|(i, o)| (i.id, o.clone())).collect::<Vec<_>>();
        assert_eq!(ids(&parse(&format(&r))), ids(&r));
    }

    #[test]
    fn rejects_foreign_ids_and_paths() {
        assert!(parse("../etc\t1\t1\t0\t\n").is_empty());
        assert!(!is_result_path(Path::new(r"C:\Windows\System32\evil.tsv")));
        assert!(!is_result_path(Path::new("dusty-1-2.tsv")));
        assert!(is_result_path(&std::env::temp_dir().join("dusty-1-2.tsv")));
    }
}
