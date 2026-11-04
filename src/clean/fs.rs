//! Safe, fast filesystem walking and deletion.
//!
//! Rules: never follow reparse points (a junction or symlink is removed as a
//! link, its target is never touched or counted); skip anything locked or
//! access-denied; never remove the root folder of a target.
//!
//! The elevated task works in Windows\Temp, where any user can create folders
//! and swap them for junctions while a clean runs. So below the root nothing
//! is opened by path: each entry is opened by name relative to the handle of
//! the folder that listed it, which leaves no path to redirect.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use windows::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    NTCREATEFILE_CREATE_OPTIONS, NtCreateFile,
};
use windows::Win32::Foundation::{HANDLE, UNICODE_STRING};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ACCESS_RIGHTS, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO,
    FILE_DISPOSITION_INFO_EX, FILE_DISPOSITION_INFO_EX_FLAGS, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_FULL_DIR_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FileBasicInfo, FileDispositionInfo, FileDispositionInfoEx,
    FileFullDirectoryInfo, GetFileInformationByHandleEx, OPEN_EXISTING, SYNCHRONIZE, SetFileInformationByHandle,
};
use windows::Win32::System::IO::IO_STATUS_BLOCK;
use windows::core::{PCWSTR, PWSTR};

use crate::win::{Handle, long_path};

/// Room for a few hundred entries per directory read.
const LIST_BUFFER_BYTES: usize = 64 * 1024;

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
    /// As stored on disk: a name needn't be valid Unicode to be deletable.
    name: Vec<u16>,
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

fn filetime(ticks: i64) -> SystemTime {
    // FILETIME counts 100 ns ticks since 1601-01-01.
    const EPOCH_DIFF: u64 = 11_644_473_600;
    let secs = (ticks.max(0) as u64 / 10_000_000).saturating_sub(EPOCH_DIFF);
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

/// Opens a target's root folder. This is the one open by path, and it follows
/// links, so a TEMP moved to another drive works: the root comes from the
/// catalogue, and the system folders' parents are closed to users.
fn open_root(path: &Path) -> Option<Handle> {
    let p = long_path(path);
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    unsafe {
        CreateFileW(PCWSTR(p.as_ptr()), FILE_LIST_DIRECTORY.0, share, None, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, None)
    }
    .ok()
    .map(Handle)
}

/// Opens the entry `name` of `dir` itself, even if it's a link.
fn open_at(dir: &Handle, name: &[u16], access: FILE_ACCESS_RIGHTS, options: NTCREATEFILE_CREATE_OPTIONS) -> Option<Handle> {
    let bytes = u16::try_from(size_of_val(name)).ok()?;
    let name = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: PWSTR(name.as_ptr() as *mut u16) };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: dir.0,
        ObjectName: &name,
        ..Default::default()
    };
    let mut handle = HANDLE::default();
    let mut status = IO_STATUS_BLOCK::default();
    unsafe {
        NtCreateFile(
            &mut handle,
            access | SYNCHRONIZE,
            &attributes,
            &mut status,
            None,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            None,
            0,
        )
    }
    .is_ok()
    .then_some(Handle(handle))
}

/// Opens a sub-folder to list it. The listing may be stale by now, so what
/// counts is what actually opened: a link is refused. From here on the handle
/// stays on this folder, whatever then happens to its name.
fn open_subdir(dir: &Handle, name: &[u16]) -> Option<Handle> {
    let sub = open_at(dir, name, FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES, FILE_DIRECTORY_FILE)?;
    let mut info = FILE_BASIC_INFO::default();
    unsafe { GetFileInformationByHandleEx(sub.0, FileBasicInfo, &mut info as *mut _ as *mut _, size_of_val(&info) as u32) }
        .ok()?;
    (info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0).then_some(sub)
}

/// Lists one open directory.
fn list(dir: &Handle) -> Vec<Entry> {
    let mut buffer = vec![0u64; LIST_BUFFER_BYTES / size_of::<u64>()];
    let mut out = Vec::new();
    while unsafe {
        GetFileInformationByHandleEx(dir.0, FileFullDirectoryInfo, buffer.as_mut_ptr().cast(), LIST_BUFFER_BYTES as u32)
    }
    .is_ok()
    {
        let mut record = buffer.as_ptr().cast::<u8>();
        loop {
            let info = record.cast::<FILE_FULL_DIR_INFO>();
            // SAFETY: the call above filled the buffer with a chain of records,
            // each holding FileNameLength bytes of name.
            let (entry, next) = unsafe {
                let name = std::ptr::addr_of!((*info).FileName).cast::<u16>();
                let name = std::slice::from_raw_parts(name, (*info).FileNameLength as usize / size_of::<u16>());
                let entry = Entry {
                    name: name.to_vec(),
                    attrs: (*info).FileAttributes,
                    size: (*info).EndOfFile.max(0) as u64,
                    newest: filetime((*info).LastWriteTime.max((*info).CreationTime)),
                };
                (entry, (*info).NextEntryOffset as usize)
            };
            if !is_dot(&entry.name) {
                out.push(entry);
            }
            if next == 0 {
                break;
            }
            record = unsafe { record.add(next) };
        }
    }
    out
}

fn is_dot(name: &[u16]) -> bool {
    const DOT: u16 = b'.' as u16;
    matches!(name, [DOT] | [DOT, DOT])
}

/// Measures (dry run) or deletes what `filter` selects under `root`.
/// The root folder itself is always kept.
pub fn sweep(root: &Path, filter: Filter, delete: bool) -> Tally {
    let cutoff = SystemTime::now().checked_sub(filter.min_age).unwrap_or(SystemTime::UNIX_EPOCH);
    open_root(root).map_or(Tally::default(), |dir| sweep_tree(&dir, cutoff, delete).0)
}

/// Returns the tally and whether the directory ended up (or would end up) empty.
fn sweep_tree(dir: &Handle, cutoff: SystemTime, delete: bool) -> (Tally, bool) {
    let mut tally = Tally::default();
    let mut emptied = true;
    for e in list(dir) {
        if e.is_link() {
            if e.newest <= cutoff && (!delete || remove(dir, &e.name)) {
                continue;
            }
            emptied = false;
        } else if e.is_dir() {
            let swept = open_subdir(dir, &e.name).map(|sub| sweep_tree(&sub, cutoff, delete));
            let (sub, empty) = swept.unwrap_or_default();
            tally += sub;
            let gone = empty && e.newest <= cutoff && (!delete || remove(dir, &e.name));
            emptied &= gone;
        } else if e.newest <= cutoff && (!delete || remove(dir, &e.name)) {
            tally += Tally { files: 1, bytes: e.size };
        } else {
            emptied = false;
        }
    }
    (tally, emptied)
}

/// Deletes one file, empty directory or link of `dir`. POSIX semantics drop
/// the name immediately even if another process holds the file open; read-only
/// is ignored. Falls back to the classic path on older systems / filesystems.
fn remove(dir: &Handle, name: &[u16]) -> bool {
    let Some(entry) = open_at(dir, name, DELETE, NTCREATEFILE_CREATE_OPTIONS(0)) else { return false };
    let ex = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
            FILE_DISPOSITION_FLAG_DELETE.0
                | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0
                | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE.0,
        ),
    };
    unsafe {
        SetFileInformationByHandle(entry.0, FileDispositionInfoEx, &ex as *const _ as *const _, size_of_val(&ex) as u32)
    }
    .is_ok()
        || remove_classic(dir, name, &entry)
}

/// Clears read-only, then deletes on close. `entry` is `name`, opened for delete.
fn remove_classic(dir: &Handle, name: &[u16], entry: &Handle) -> bool {
    if let Some(attrs) = open_at(dir, name, FILE_WRITE_ATTRIBUTES, NTCREATEFILE_CREATE_OPTIONS(0)) {
        // Zero leaves a timestamp as it is.
        let info = FILE_BASIC_INFO { FileAttributes: FILE_ATTRIBUTE_NORMAL.0, ..Default::default() };
        unsafe { SetFileInformationByHandle(attrs.0, FileBasicInfo, &info as *const _ as *const _, size_of_val(&info) as u32) }
            .ok();
    }
    let info = FILE_DISPOSITION_INFO { DeleteFile: true.into() };
    unsafe {
        SetFileInformationByHandle(entry.0, FileDispositionInfo, &info as *const _ as *const _, size_of_val(&info) as u32)
    }
    .is_ok()
}

/// Sub-directories of `dir` (no links), e.g. browser profiles.
pub fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let entries = open_root(dir).map_or(Vec::new(), |d| list(&d));
    entries.iter().filter(|e| e.is_dir() && !e.is_link()).map(|e| dir.join(OsString::from_wide(&e.name))).collect()
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

    fn junction(link: &Path, target: &Path) {
        let ok = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "mklink /J failed");
    }

    /// Backdates a file past every age filter.
    fn make_old(file: &Path) {
        use std::os::windows::fs::FileTimesExt;
        let old = SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
        let times = fs::FileTimes::new().set_modified(old).set_created(old);
        fs::File::options().write(true).open(file).unwrap().set_times(times).unwrap();
    }

    /// What an attacker does in a folder they can write to, such as Windows\Temp:
    /// show the clean a real folder, then swap a junction in behind its back. The
    /// filler folders sort first, buying the swap time after the listing is taken.
    #[test]
    fn a_folder_swapped_for_a_junction_mid_clean_is_not_followed() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let d = scratch("swap");
        let stage = scratch("swap-stage");
        let target = scratch("swap-target");
        let precious = target.join("precious.txt");
        fs::write(&precious, b"keep me").unwrap();
        make_old(&precious);
        let (real, link, slot) = (stage.join("real"), stage.join("link"), d.join("zz-slot"));
        fs::create_dir(&real).unwrap();
        for i in 0..500 {
            fs::create_dir(d.join(format!("filler-{i}"))).unwrap();
        }
        junction(&link, &target);

        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    for side in [&real, &link] {
                        fs::rename(side, &slot).ok();
                        fs::rename(&slot, side).ok();
                    }
                }
            });
            let end = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < end && precious.exists() {
                sweep(&d, Filter::older_than_hours(24), true);
            }
            stop.store(true, Ordering::Relaxed);
        });

        let survived = precious.exists();
        fs::remove_dir(&link).ok();
        fs::remove_dir(&slot).ok();
        for dir in [&d, &stage, &target] {
            fs::remove_dir_all(dir).ok();
        }
        assert!(survived, "the clean followed a swapped-in junction");
    }

    fn wide_name(name: &str) -> Vec<u16> {
        name.encode_utf16().collect()
    }

    #[test]
    fn a_link_is_never_opened_as_a_folder() {
        let d = scratch("subdir");
        let target = scratch("subdir-target");
        fs::create_dir(d.join("real")).unwrap();
        junction(&d.join("link"), &target);
        let dir = open_root(&d).unwrap();
        assert!(open_subdir(&dir, &wide_name("real")).is_some());
        assert!(open_subdir(&dir, &wide_name("link")).is_none());
        drop(dir);
        fs::remove_dir(d.join("link")).ok();
        fs::remove_dir_all(&d).ok();
        fs::remove_dir_all(&target).ok();
    }

    #[test]
    fn the_classic_delete_clears_read_only() {
        let d = scratch("classic");
        let f = d.join("ro.tmp");
        fs::write(&f, [0u8; 10]).unwrap();
        let mut perm = fs::metadata(&f).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&f, perm).unwrap();
        let (dir, name) = (open_root(&d).unwrap(), wide_name("ro.tmp"));
        let entry = open_at(&dir, &name, DELETE, NTCREATEFILE_CREATE_OPTIONS(0)).unwrap();
        assert!(remove_classic(&dir, &name, &entry));
        drop(entry);
        assert!(!f.exists());
        drop(dir);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_name_that_is_not_valid_unicode_is_still_removed() {
        let d = scratch("surrogate");
        let bad = OsString::from_wide(&[0xD800, b'x' as u16]);
        fs::write(d.join(&bad), [0u8; 10]).unwrap();
        assert_eq!(sweep(&d, Filter::ALL, true), Tally { files: 1, bytes: 10 });
        assert_eq!(fs::read_dir(&d).unwrap().count(), 0);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn trees_deeper_than_max_path_are_swept() {
        let d = scratch("deep");
        let mut deep = PathBuf::from(format!(r"\\?\{}", d.display()));
        for _ in 0..40 {
            deep.push("a-folder-with-a-long-name");
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("x.tmp"), [0u8; 10]).unwrap();
        assert_eq!(sweep(&d, Filter::ALL, true), Tally { files: 1, bytes: 10 });
        assert_eq!(fs::read_dir(&d).unwrap().count(), 0);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn never_follows_junctions() {
        let d = scratch("junction");
        let target = scratch("junction-target");
        fs::write(target.join("precious.txt"), b"keep me").unwrap();
        let link = d.join("link");
        junction(&link, &target);
        let t = sweep(&d, Filter::ALL, true);
        assert_eq!(t.bytes, 0, "link target must not be counted");
        assert!(!link.exists(), "the junction itself is removed");
        assert!(target.join("precious.txt").exists(), "target must survive");
        fs::remove_dir_all(&d).ok();
        fs::remove_dir_all(&target).ok();
    }
}
