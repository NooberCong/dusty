//! The curated cleanup catalogue. Every entry is something that measurably
//! frees space and regrows only as a cache; nothing here touches user data,
//! logins, the registry, prefetch or anything Windows needs to roll back
//! unless the entry says so and is off by default.

use std::path::PathBuf;

use super::fs::Filter;
use crate::win::{env_path, windows_dir};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// No admin, safe for one-click and tray use.
    Quick,
    /// Configurable; may need admin.
    Deep,
}

pub enum Action {
    /// Folder contents, optionally guarded by a process that must be closed.
    Paths(fn() -> Vec<Target>),
    RecycleBin,
    /// Windows' own Disk Cleanup handlers (HKLM\...\Explorer\VolumeCaches\<key>).
    Handlers(&'static [&'static str]),
}

pub struct Target {
    pub path: PathBuf,
    pub filter: Filter,
    /// Executable name (lower case) that must not be running, and its display name.
    pub guard: Option<(&'static str, &'static str)>,
}

pub struct Item {
    pub id: &'static str,
    pub name: &'static str,
    pub detail: &'static str,
    pub tier: Tier,
    pub admin: bool,
    pub default_on: bool,
    pub action: Action,
}

pub fn find(id: &str) -> Option<&'static Item> {
    ITEMS.iter().find(|i| i.id == id)
}

pub static ITEMS: &[Item] = &[
    Item {
        id: "temp",
        name: "Temporary files",
        detail: "Your temp folder, files older than a day",
        tier: Tier::Quick,
        admin: false,
        default_on: true,
        action: Action::Paths(user_temp),
    },
    Item {
        id: "browsers",
        name: "Browser caches",
        detail: "Closed browsers only. Logins and history are kept",
        tier: Tier::Quick,
        admin: false,
        default_on: true,
        action: Action::Paths(browser_caches),
    },
    Item {
        id: "crashes",
        name: "App crash reports",
        detail: "Crash dumps and queued error reports",
        tier: Tier::Quick,
        admin: false,
        default_on: true,
        action: Action::Paths(user_crash_reports),
    },
    Item {
        id: "recycle",
        name: "Recycle Bin",
        detail: "Everything in the Recycle Bin on all drives",
        tier: Tier::Quick,
        admin: false,
        default_on: true,
        action: Action::RecycleBin,
    },
    Item {
        id: "wintemp",
        name: "Windows temp files",
        detail: "System temp folder, files older than a week",
        tier: Tier::Deep,
        admin: true,
        default_on: true,
        action: Action::Paths(windows_temp),
    },
    Item {
        id: "updates",
        name: "Windows Update cleanup",
        detail: "Superseded system components. Can take several minutes",
        tier: Tier::Deep,
        admin: true,
        default_on: true,
        action: Action::Handlers(&["Update Cleanup"]),
    },
    Item {
        id: "update-downloads",
        name: "Update downloads",
        detail: "Installed update packages older than ten days",
        tier: Tier::Deep,
        admin: true,
        default_on: true,
        action: Action::Paths(update_downloads),
    },
    Item {
        id: "delivery",
        name: "Delivery Optimization",
        detail: "Update files cached for sharing with other PCs",
        tier: Tier::Deep,
        admin: true,
        default_on: true,
        action: Action::Handlers(&["Delivery Optimization Files"]),
    },
    Item {
        id: "system-errors",
        name: "System error reports",
        detail: "Windows error reports and system crash dumps",
        tier: Tier::Deep,
        admin: true,
        default_on: true,
        action: Action::Handlers(&[
            "Windows Error Reporting Files",
            "System error memory dump files",
            "System error minidump files",
        ]),
    },
    Item {
        id: "drivers",
        name: "Old driver versions",
        detail: "Superseded driver packages; drivers in use are kept",
        tier: Tier::Deep,
        admin: true,
        default_on: true,
        action: Action::Handlers(&["Device Driver Packages"]),
    },
    Item {
        id: "dev",
        name: "Developer caches",
        detail: "npm, pip, NuGet, Cargo, Go, Yarn, Gradle. Re-downloaded on next build",
        tier: Tier::Deep,
        admin: false,
        default_on: false,
        action: Action::Paths(dev_caches),
    },
    Item {
        id: "shaders",
        name: "Shader caches",
        detail: "Only to fix graphics glitches. Games stutter while they rebuild",
        tier: Tier::Deep,
        admin: false,
        default_on: false,
        action: Action::Paths(shader_caches),
    },
    Item {
        id: "old-windows",
        name: "Previous Windows version",
        detail: "Removes the option to roll back the last upgrade",
        tier: Tier::Deep,
        admin: true,
        default_on: false,
        action: Action::Handlers(&["Previous Installations", "Temporary Setup Files"]),
    },
];

fn local() -> Option<PathBuf> {
    env_path("LOCALAPPDATA")
}

fn all(paths: impl IntoIterator<Item = Option<PathBuf>>, filter: Filter) -> Vec<Target> {
    paths.into_iter().flatten().map(|path| Target { path, filter, guard: None }).collect()
}

fn user_temp() -> Vec<Target> {
    all([env_path("TEMP")], Filter::older_than_hours(24))
}

fn windows_temp() -> Vec<Target> {
    all([windows_dir().map(|r| r.join("Temp"))], Filter::older_than_hours(7 * 24))
}

fn update_downloads() -> Vec<Target> {
    all(
        [windows_dir().map(|r| r.join(r"SoftwareDistribution\Download"))],
        Filter::older_than_hours(10 * 24),
    )
}

fn user_crash_reports() -> Vec<Target> {
    let l = local();
    all(
        [
            l.as_ref().map(|l| l.join("CrashDumps")),
            l.as_ref().map(|l| l.join(r"Microsoft\Windows\WER\ReportArchive")),
            l.as_ref().map(|l| l.join(r"Microsoft\Windows\WER\ReportQueue")),
        ],
        Filter::ALL,
    )
}

fn browser_caches() -> Vec<Target> {
    let mut out = Vec::new();
    let Some(l) = local() else { return out };
    let chromium: [(&str, &str, &str); 5] = [
        (r"Google\Chrome\User Data", "chrome.exe", "Chrome"),
        (r"Microsoft\Edge\User Data", "msedge.exe", "Edge"),
        (r"BraveSoftware\Brave-Browser\User Data", "brave.exe", "Brave"),
        (r"Vivaldi\User Data", "vivaldi.exe", "Vivaldi"),
        (r"Opera Software\Opera Stable", "opera.exe", "Opera"),
    ];
    let mut add = |path: PathBuf, guard| {
        if path.is_dir() {
            out.push(Target { path, filter: Filter::ALL, guard: Some(guard) });
        }
    };
    for (rel, exe, name) in chromium {
        let root = l.join(rel);
        for shared in ["ShaderCache", "GrShaderCache", "GraphiteDawnCache"] {
            add(root.join(shared), (exe, name));
        }
        // Profiles: Default, Profile 1, Guest Profile... (Opera keeps the cache at the root).
        let mut profiles = super::fs::subdirs(&root);
        profiles.push(root.clone());
        for p in profiles {
            for cache in ["Cache", "Code Cache", "GPUCache"] {
                add(p.join(cache), (exe, name));
            }
        }
    }
    for p in super::fs::subdirs(&l.join(r"Mozilla\Firefox\Profiles")) {
        add(p.join("cache2"), ("firefox.exe", "Firefox"));
    }
    out
}

fn dev_caches() -> Vec<Target> {
    let l = local();
    let home = env_path("USERPROFILE");
    let cargo = env_path("CARGO_HOME").or_else(|| home.as_ref().map(|h| h.join(".cargo")));
    let gomod = env_path("GOMODCACHE").or_else(|| home.as_ref().map(|h| h.join(r"go\pkg\mod")));
    let j = |base: &Option<PathBuf>, rel: &str| base.as_ref().map(|b| b.join(rel));
    all(
        [
            j(&l, "npm-cache"),
            j(&l, r"pip\cache"),
            j(&home, r".nuget\packages"),
            j(&l, r"NuGet\v3-cache"),
            j(&cargo, r"registry\cache"),
            j(&cargo, r"registry\src"),
            j(&cargo, r"git\checkouts"),
            gomod,
            j(&l, "go-build"),
            j(&l, r"Yarn\Cache"),
            j(&home, r".gradle\caches"),
        ],
        Filter::ALL,
    )
}

fn shader_caches() -> Vec<Target> {
    let l = local();
    let low = env_path("USERPROFILE").map(|h| h.join(r"AppData\LocalLow"));
    let j = |base: &Option<PathBuf>, rel: &str| base.as_ref().map(|b| b.join(rel));
    all(
        [
            j(&l, "D3DSCache"),
            j(&l, r"NVIDIA\DXCache"),
            j(&l, r"NVIDIA\GLCache"),
            j(&low, r"NVIDIA\PerDriverVersion\DXCache"),
            j(&l, r"AMD\DxCache"),
            j(&l, r"AMD\DxcCache"),
            j(&l, r"AMD\GLCache"),
            j(&l, r"AMD\VkCache"),
            j(&low, r"Intel\ShaderCache"),
        ],
        Filter::ALL,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_unique_and_quick_never_needs_admin() {
        let mut ids: Vec<_> = ITEMS.iter().map(|i| i.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), ITEMS.len());
        assert!(ITEMS.iter().filter(|i| i.tier == Tier::Quick).all(|i| !i.admin));
    }

    /// Means most when run with %SystemRoot% pointing elsewhere.
    #[test]
    fn system_targets_come_from_windows_not_the_environment() {
        let windows = windows_dir().unwrap();
        assert!(windows.join(r"System32\kernel32.dll").exists(), "{windows:?}");
        assert_eq!(windows_temp()[0].path, windows.join("Temp"));
        assert_eq!(update_downloads()[0].path, windows.join(r"SoftwareDistribution\Download"));
    }

    #[test]
    fn targets_are_absolute_and_never_bare_profile_roots() {
        let forbidden: Vec<PathBuf> =
            ["USERPROFILE", "LOCALAPPDATA", "APPDATA", "SystemRoot"].iter().filter_map(|v| env_path(v)).collect();
        for item in ITEMS {
            if let Action::Paths(f) = item.action {
                for t in f() {
                    assert!(t.path.is_absolute(), "{}: {:?}", item.id, t.path);
                    assert!(!forbidden.contains(&t.path), "{} targets a root: {:?}", item.id, t.path);
                }
            }
        }
    }
}
