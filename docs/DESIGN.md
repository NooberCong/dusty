# How dusty is built

- **Rust + raw Win32**: no runtime and no framework. The main window draws its own title
  bar with Direct2D/DirectWrite (CPU rasteriser), and all Direct2D state is released when
  the window hides. The always-on taskbar readout is drawn with GDI into a layered window.
- **Motion**: frames are paced by the compositor and only run while something moves, so a
  settled window draws nothing. What the user drives runs at the panel's rate; data
  settling after a sample runs near 30 fps, and only in the foreground. Windows' "Animation
  effects" setting turns it all off.
- **Identity**: brass on petrol, with Bahnschrift, Windows' own DIN, for readouts. The logo
  is geometry in `src/ui/logo.rs`, so the cog can turn. `res/make_icon.py` draws the same
  mark into `dusty.ico` and `logo.svg`.
- **Privileges**: dusty runs as a normal user. Admin items run in the same exe relaunched
  with `--helper` behind a UAC prompt, or in the scheduled task once a prompt has set it
  up. Both accept catalogue ids only, never paths. `dusty uninstall` in an elevated console
  removes the tasks and the copy.
- **Sampling** (one thread, 1 Hz, parked when nothing is on screen):
  - `GetSystemTimes` for CPU, matching Task Manager on current Windows 11.
  - `GlobalMemoryStatusEx` for memory.
  - One PDH query for disk activity and throughput, GPU engines and VRAM.
  - D3DKMT adapter perf data for GPU temperature.
  - `IOCTL_STORAGE_QUERY_PROPERTY` for drive temperature, every 10 s.
  - SMBIOS for memory type and speed, once at start.
- **CPU temperature** has no Windows API. On ASUS laptops the firmware reports it through
  `\\.\ATKACPI`, which needs no driver or admin; dusty only ever calls its read-only `DSTS`
  method. Elsewhere reading the sensor takes a kernel driver, and dusty installs none, so
  the temperature is left out rather than replaced by an ACPI zone that isn't the CPU.

Measured on an i9-13980HX / RTX 4080 laptop (release build):

| State | Private memory | CPU |
|---|---|---|
| Closed to tray, after the window was open | ~15 MB | ~0.3 % of one core |
| Window open, behind other windows | ~37 MB | ~1 % of one core |
| Window open, in front, on Monitor | ~37 MB | ~6 % of one core |

## How each item is cleaned

| Item | Tier | Admin | Method |
|---|---|---|---|
| Temporary files (older than 24 h) | Quick | | delete |
| Browser caches (Chromium family, Firefox; only closed browsers) | Quick | | delete `Cache`, `Code Cache`, `GPUCache` |
| App crash reports | Quick | | delete |
| Recycle Bin | Quick | | `SHEmptyRecycleBin` |
| Windows temp files (older than 7 days) | Deep | yes | delete |
| Windows Update cleanup | Deep | yes | `Update Cleanup` handler |
| Update downloads (older than 10 days) | Deep | yes | delete |
| Delivery Optimization | Deep | yes | handler |
| System error reports and dumps | Deep | yes | handlers |
| Old driver versions | Deep | yes | `Device Driver Packages` handler |
| Developer caches (npm, pip, NuGet, Cargo, Go, Yarn, Gradle) | Deep, off | | delete |
| Shader caches | Deep, off | | delete |
| Previous Windows version | Deep, off | yes | handlers |

Deletion never follows junctions or symlinks (the link itself is removed), skips files in
use, and keeps each target's root folder. Below a target's root everything is opened
relative to its parent folder's handle, never by path, so a folder swapped for a junction
mid-clean can't redirect the elevated task. It finds Windows' folders through the system
rather than `%SystemRoot%`, because it inherits the user's environment.

The task runs one clean at a time. A request made while it's busy is refused with an error,
and a running clean is never stopped. An automatic clean that can't reach its system items,
because the Program Files copy is out of date or the task is busy, logs each one as skipped.
A clean by hand updates the copy.
