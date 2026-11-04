# dusty

A tiny, honest PC cleaner and system monitor for Windows 10/11.

- **Monitor**: CPU, memory, disk and GPU load on a dial with 60-second history, GPU and
  drive temperatures, CPU temperature on ASUS laptops, installed memory type and speed,
  free space per drive. It only speaks up when something needs attention.
- **Taskbar readout**: load and temperatures on the empty end of the taskbar.
  Click it to open dusty; right-click it for the menu.
- **Clean**: one-click *Quick* items (no admin) and configurable *Deep* items. System
  stores are cleaned through Windows' own Disk Cleanup handlers, never by deleting their
  files directly.
- No registry cleaning, no "boost", no RAM optimiser, no background service, no telemetry.

## Build

Needs Rust (MSVC toolchain, pinned in `rust-toolchain.toml`) and the Windows SDK.

```
cargo build --release      # target\release\dusty.exe, single file, about 570 KB
cargo test                 # engine, parser and safety tests (create junctions in %TEMP%)
```

## Command line

```
dusty                      open the window
dusty --tray               start in the notification area (used by "Start with Windows")
dusty scan                 dry run: size of every item
dusty clean quick          clean the default Quick items
dusty clean deep           every default item (admin items need an elevated console)
dusty clean temp dev       specific items by id
dusty sensors              five live samples
```

Every clean is logged to `%LOCALAPPDATA%\dusty\clean.log`. Settings are kept in
`%APPDATA%\dusty\settings.ini`.

## What gets cleaned

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
use, and keeps each target's root folder.

## Design

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
  with `--helper` behind one UAC prompt per batch. The helper accepts catalogue ids only,
  never paths.
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
