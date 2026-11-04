//! Safe, fast filesystem walking and deletion.
//!
//! Rules: never follow reparse points (a junction or symlink is removed as a
//! link, its target is never touched or counted); skip anything locked or
//! access-denied; never remove the root folder of a target.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use windows::Win32::Foundation::FILETIME;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_DISPOSITION_INFO_EX_FLAGS,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic, FindExSearchNameMatch,
    FindFirstFileExW, FindNextFileW, FileDispositionInfo, FileDispositionInfoEx, OPEN_EXISTING,
    SetFileAttributesW, SetFileInformationByHandle, WIN32_FIND_DATAW,
};
use windows::core::PCWSTR;

use crate::win::{Handle, from_wide, long_path};

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Tally {
    pub files: u64,
    pub bytes: u64,
}

impl std::ops::AddAssign for Tally {
    fn add_assign(&mut self, o: Self) {
        self.files += o.files;
        self.bytes += o.bytes;
    }
}

/// Which entries under a root qualify for deletion.
#[derive(Clone, Copy, Debug)]
pub struct Filter {
    /// Only entries last written (and created) at least this long ago.
    pub min_age: Duration,
}

impl Filter {
    pub const ALL: Filter = Filter { min_age: Duration::ZERO };

    pub const fn older_than_hours(h: u64) -> Filter {
        Filter { min_age: Duration::from_secs(h * 3600) }
    }
}

struct Entry {
    name: String,
    attrs: u32,
    size: u64,
    newest: SystemTime,
}

impl Entry {
    fn is_dir(&self) -> bool {
        self.attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0
    }
    fn is_link(&self) -> bool {
        self.attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    }
}

fn filetime(f: FILETIME) -> SystemTime {
    // FILETIME counts 100 ns ticks since 1601-01-01.
    const EPOCH_DIFF: u64 = 11_644_473_600;
    let ticks = (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    let secs = (ticks / 10_000_000).saturating_sub(EPOCH_DIFF);
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

/// Lists one directory with the fastest documented enumeration mode.
fn list(dir: &Path) -> Vec<Entry> {
    let pattern = long_path(&dir.join("*"));
    let mut data = WIN32_FIND_DATAW::default();
    let Ok(h) = (unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    loop {
        let name = from_wide(&data.cFileName);
        if name != "." && name != ".." {
            out.push(Entry {
                name,
                attrs: data.dwFileAttributes,
                size: (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow),
                newest: filetime(data.ftLastWriteTime).max(filetime(data.ftCreationTime)),
            });
        }
        if unsafe { FindNextFileW(h, &mut data) }.is_err() {
            break;
        }
    }
    unsafe { FindClose(h) }.ok();
    out
}

/// Measures (dry run) or deletes what `filter` selects under `root`.
/// The root folder itself is always kept.
pub fn sweep(root: &Path, filter: Filter, delete: bool) -> Tally {
    let cutoff = SystemTime::now().checked_sub(filter.min_age).unwrap_or(SystemTime::UNIX_EPOCH);
    sweep_tree(root, cutoff, delete).0
}

/// Returns the tally and whether the directory ended up (or would end up) empty.
fn sweep_tree(dir: &Path, cutoff: SystemTime, delete: bool) -> (Tally, bool) {
    let mut tally = Tally::default();
    let mut emptied = true;
    for e in list(dir) {
        let path = dir.join(&e.name);
        if e.is_link() {
            if e.newest <= cutoff && (!delete || remove(&path, e.is_dir())) {
                continue;
            }
            emptied = false;
        } else if e.is_dir() {
            let (sub, empty) = sweep_tree(&path, cutoff, delete);
            tally += sub;
            let gone = empty && e.newest <= cutoff && (!delete || remove(&path, true));
            emptied &= gone;
        } else if e.newest <= cutoff && (!delete || remove(&path, false)) {
            tally += Tally { files: 1, bytes: e.size };
        } else {
            emptied = false;
        }
    }
    (tally, emptied)
}

/// Deletes one file, empty directory or link. POSIX semantics drop the name
/// immediately even if another process holds the file open; read-only is
/// ignored. Falls back to the classic path on older systems / filesystems.
pub fn remove(path: &Path, is_dir: bool) -> bool {
    let p = long_path(path);
    let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
    if is_dir {
        flags |= FILE_FLAG_BACKUP_SEMANTICS;
    }
    let Ok(h) = (unsafe {
        CreateFileW(
            PCWSTR(p.as_ptr()),
            DELETE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            flags,
            None,
        )
    }) else {
        return false;
    };
    let h = Handle(h);
    let ex = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
            FILE_DISPOSITION_FLAG_DELETE.0
                | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0
                | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE.0,
        ),
    };
    let ok = unsafe {
        SetFileInformationByHandle(h.0, FileDispositionInfoEx, &ex as *const _ as *const _, size_of_val(&ex) as u32)
    }
    .is_ok();
    if ok {
        return true;
    }
    // Legacy path: clear read-only, then classic delete-on-close.
    unsafe { SetFileAttributesW(PCWSTR(p.as_ptr()), FILE_ATTRIBUTE_NORMAL) }.ok();
    let info = FILE_DISPOSITION_INFO { DeleteFile: true.into() };
    unsafe {
        SetFileInformationByHandle(h.0, FileDispositionInfo, &info as *const _ as *const _, size_of_val(&info) as u32)
    }
    .is_ok()
}

/// Sub-directories of `dir` (no links), e.g. browser profiles.
pub fn subdirs(dir: &Path) -> Vec<PathBuf> {
    list(dir).into_iter().filter(|e| e.is_dir() && !e.is_link()).map(|e| dir.join(e.name)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dusty-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn sweeps_tree_keeps_root_and_counts() {
        let d = scratch("sweep");
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::write(d.join("x.tmp"), [0u8; 100]).unwrap();
        fs::write(d.join("a/b/y.tmp"), [0u8; 50]).unwrap();
        let ro = d.join("a/ro.tmp");
        fs::write(&ro, [0u8; 10]).unwrap();
        let mut perm = fs::metadata(&ro).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&ro, perm).unwrap();

        let dry = sweep(&d, Filter::ALL, false);
        assert_eq!(dry, Tally { files: 3, bytes: 160 });
        assert!(d.join("x.tmp").exists(), "dry run must not delete");

        let done = sweep(&d, Filter::ALL, true);
        assert_eq!(done, dry);
        assert!(d.exists(), "root is kept");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 0);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn age_filter_spares_fresh_files() {
        let d = scratch("age");
        fs::write(d.join("fresh.tmp"), [0u8; 10]).unwrap();
        assert_eq!(sweep(&d, Filter::older_than_hours(24), true), Tally::default());
        assert!(d.join("fresh.tmp").exists());
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn open_files_are_still_removed_or_skipped_safely() {
        let d = scratch("open");
        let f = d.join("held.tmp");
        fs::write(&f, [0u8; 10]).unwrap();
        // Held without FILE_SHARE_DELETE: must be skipped, not counted.
        use std::os::windows::fs::OpenOptionsExt;
        let held = fs::OpenOptions::new().read(true).share_mode(1 /* FILE_SHARE_READ */).open(&f).unwrap();
        let t = sweep(&d, Filter::ALL, true);
        assert_eq!(t.files, 0);
        assert!(f.exists());
        drop(held);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn never_follows_junctions() {
        let d = scratch("junction");
        let target = scratch("junction-target");
        fs::write(target.join("precious.txt"), b"keep me").unwrap();
        let link = d.join("link");
        let ok = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "mklink /J failed");
        let t = sweep(&d, Filter::ALL, true);
        assert_eq!(t.bytes, 0, "link target must not be counted");
        assert!(!link.exists(), "the junction itself is removed");
        assert!(target.join("precious.txt").exists(), "target must survive");
        fs::remove_dir_all(&d).ok();
        fs::remove_dir_all(&target).ok();
    }
}
