//! Taskbar readout: a small click-to-open, never-focused layered window that
//! sits on the empty end of the taskbar. Windows has no supported API for
//! taskbar content (deskbands are gone in Windows 11), so this is a child of
//! the taskbar window. Windows lifts the taskbar into a system layer above all
//! topmost windows while Start is open, and a child goes with it.
//!
//! A child shares input with its parent's thread, so a stalled window would
//! stall the taskbar. The window therefore lives on a thread of its own, which
//! only ever draws, and the UI thread hands it frames without waiting.
//!
//! It is always on, so it is drawn with plain GDI into a DIB (no Direct2D,
//! which would keep a software rasteriser and ~19 MB resident) and the
//! per-pixel alpha is derived from the grayscale glyph coverage.

use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::time::Duration;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CLIP_DEFAULT_PRECIS,
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_LEFT, DT_NOPREFIX, DT_RIGHT,
    DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FW_NORMAL, GdiFlush, GetDC, HBITMAP,
    HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::HKEY_CURRENT_USER;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    ABM_GETSTATE, ABS_AUTOHIDE, APPBARDATA, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
    SHAppBarMessage, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowExW, FindWindowW, GWL_STYLE,
    GW_HWNDPREV, GetMessageW, GetWindow, GetWindowRect, GetWindowThreadProcessId, HWND_TOP, IDC_ARROW,
    IsWindowVisible, LoadCursorW, MA_NOACTIVATE, MSG, PM_NOREMOVE, PeekMessageW, PostMessageW, PostThreadMessageW,
    RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetParent, SetWindowLongW,
    SetWindowPos, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_APP,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_RBUTTONUP, WNDCLASSW, WS_CHILD, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use super::theme::{Rgba, Theme};
use super::{WM_APP_MENU, WM_APP_SHOW};
use crate::monitor::Snapshot;
use crate::win::reg_dword;

const CLASS: PCWSTR = w!("dusty.overlay");
const W_DIP: f32 = 204.0;
const H_DIP: f32 = 36.0;
const EDGE_DIP: f32 = 12.0;
/// Width the Widgets button takes at the left end of a centred taskbar.
const WIDGETS_DIP: f32 = 190.0;
/// Posted to the readout's thread: there is a frame, or the channel closed.
const WM_FRAME: u32 = WM_APP + 1;

/// The main window, told about clicks.
static MAIN: AtomicIsize = AtomicIsize::new(0);

/// The UI thread's handle on the readout.
pub struct Overlay {
    frames: Option<SyncSender<Frame>>,
    /// The readout thread's id, 0 until its message queue exists.
    thread: Arc<AtomicU32>,
}

/// Everything one redraw needs, built on the UI thread.
struct Frame {
    key: String,
    runs: Vec<Run>,
}

/// The window, owned by the readout's thread.
struct Readout {
    hwnd: HWND,
    taskbar: HWND,
    surface: Option<Surface>,
    /// What is currently on screen; identical frames are not redrawn.
    shown: Option<(i32, i32, String)>,
}

/// 32-bit top-down DIB plus the fonts for one DPI scale.
struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
    scale: f32,
    label: HFONT,
    value: HFONT,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp.into()).ok().ok();
            DeleteObject(self.label.into()).ok().ok();
            DeleteObject(self.value.into()).ok().ok();
            DeleteDC(self.dc).ok().ok();
        }
    }
}

fn font(px: f32, weight: u32, face: PCWSTR) -> HFONT {
    unsafe {
        CreateFontW(
            -(px.round() as i32),
            0,
            0,
            0,
            weight as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY, // grayscale coverage, which becomes the alpha
            0,
            face,
        )
    }
}

impl Surface {
    fn new(scale: f32) -> Option<Self> {
        let (w, h) = ((W_DIP * scale).round() as i32, (H_DIP * scale).round() as i32);
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let old = SelectObject(dc, bmp.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00FF_FFFF));
            Some(Surface {
                dc,
                bmp,
                old,
                bits: bits as *mut u32,
                w,
                h,
                scale,
                label: font(11.0 * scale, FW_NORMAL.0, w!("Segoe UI Variable Text")),
                value: font(12.0 * scale, FW_NORMAL.0, w!("Segoe UI Variable Text")),
            })
        }
    }

    fn pixels(&mut self) -> &mut [u32] {
        // SAFETY: the DIB holds exactly w*h 32-bit pixels for the Surface's lifetime.
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }
}

/// One piece of text: DIP rect, right-aligned or not, colour.
struct Run {
    text: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    right: bool,
    bold: bool,
    color: Rgba,
}

impl Overlay {
    pub fn create(main: HWND) -> Option<Self> {
        MAIN.store(main.0 as isize, Ordering::Release);
        // One slot: if the readout falls behind, frames are dropped, not queued.
        let (frames, inbox) = sync_channel(1);
        let thread = Arc::new(AtomicU32::new(0));
        let id = thread.clone();
        std::thread::Builder::new().name("readout".into()).spawn(move || run(&inbox, &id)).ok()?;
        Some(Overlay { frames: Some(frames), thread })
    }

    /// Hands over the latest numbers. Never waits on the readout.
    pub fn update(&mut self, snap: &Snapshot, theme: &Theme, light_taskbar: bool) {
        let frame = Frame { key: frame_key(snap, light_taskbar), runs: runs(snap, theme, light_taskbar) };
        if self.frames.as_ref().is_some_and(|f| f.try_send(frame).is_ok()) {
            self.wake();
        }
    }

    fn wake(&self) {
        let id = self.thread.load(Ordering::SeqCst);
        if id != 0 {
            unsafe { PostThreadMessageW(id, WM_FRAME, WPARAM(0), LPARAM(0)) }.ok();
        }
    }
}

impl Drop for Overlay {
    /// Not joined: a readout stuck on the taskbar must not take the UI with it.
    fn drop(&mut self) {
        self.frames = None;
        self.wake();
    }
}

/// The readout's thread: draws each frame it is handed until the channel closes.
fn run(frames: &Receiver<Frame>, id: &AtomicU32) {
    let mut msg = MSG::default();
    // Makes the message queue, so nothing posted from here on is lost.
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
    // The taskbar can refuse a new child while it is busy, or not exist yet at login.
    let mut readout = loop {
        if let Some(readout) = Readout::create() {
            break readout;
        }
        if matches!(frames.try_recv(), Err(TryRecvError::Disconnected)) {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    };
    id.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    loop {
        loop {
            match frames.try_recv() {
                Ok(frame) => readout.update(frame),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        loop {
            if unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 <= 0 {
                return;
            }
            if msg.message == WM_FRAME {
                break;
            }
            unsafe { DispatchMessageW(&msg) };
        }
    }
}

impl Readout {
    fn create() -> Option<Self> {
        unsafe {
            let instance = GetModuleHandleW(None).ok()?;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: instance.into(),
                lpszClassName: CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW).ok()?,
                ..Default::default()
            };
            RegisterClassW(&wc);
            let taskbar = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).ok()?;
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE,
                CLASS,
                w!("dusty readout"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .ok()?;
            // Creating it directly under the taskbar is refused; moving it there isn't.
            SetWindowLongW(hwnd, GWL_STYLE, WS_CHILD.0 as i32);
            if SetParent(hwnd, Some(taskbar)).is_err() {
                DestroyWindow(hwnd).ok();
                return None;
            }
            Some(Readout { hwnd, taskbar, surface: None, shown: None })
        }
    }

    /// Redraws with the latest numbers, or hides when there is nowhere
    /// sensible to sit (auto-hide taskbar, fullscreen app, no taskbar).
    fn update(&mut self, frame: Frame) {
        let Some((x, y, scale)) = placement() else {
            self.hide();
            return;
        };
        let key = (x, y, frame.key);
        let visible = unsafe { IsWindowVisible(self.hwnd) }.as_bool();
        if visible && self.shown.as_ref() == Some(&key) {
            return;
        }
        if self.surface.as_ref().is_none_or(|s| s.scale != scale) {
            self.surface = Surface::new(scale);
        }
        let Some(surface) = self.surface.as_mut() else { return };
        render(surface, &frame.runs);
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
            ..Default::default()
        };
        let (size, src) = (SIZE { cx: surface.w, cy: surface.h }, POINT::default());
        unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                None,
                None,
                Some(&size),
                Some(surface.dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
            .ok();
            let keep = if under_taskbar_content(self.hwnd, self.taskbar) { Default::default() } else { SWP_NOZORDER };
            SetWindowPos(self.hwnd, Some(HWND_TOP), x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE | keep).ok();
            if !visible {
                ShowWindow(self.hwnd, SW_SHOWNOACTIVATE).ok().ok();
            }
        }
        self.shown = Some(key);
    }

    fn hide(&mut self) {
        self.shown = None;
        unsafe { ShowWindow(self.hwnd, SW_HIDE).ok().ok() };
    }
}

impl Drop for Readout {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.hwnd) }.ok();
    }
}

/// Everything the readout displays, as rounded text.
fn frame_key(s: &Snapshot, light: bool) -> String {
    let t = |v: Option<f32>| v.map_or(-1, |v| v.round() as i32);
    let gpu = s.gpu.as_ref();
    format!(
        "{:.0} {} {:.0} {} {:.0} {:.0} {} {light}",
        s.cpu,
        t(s.cpu_temp),
        gpu.map_or(0.0, |g| g.util),
        t(gpu.and_then(|g| g.temp)),
        s.mem_percent(),
        s.disk_active,
        t(s.disk_temp)
    )
}

/// Layout: a 2×2 grid of "LABEL  load  temp", load and temperature
/// right-aligned in fixed slots so digits never jiggle as values change.
fn runs(s: &Snapshot, theme: &Theme, light: bool) -> Vec<Run> {
    let ink = if light { Rgba::hex(0x1A1A1A) } else { Rgba::hex(0xFFFFFF) };
    let dim = ink.alpha(0.62);
    // Warning colours must read on the taskbar, not on the app background.
    let theme = Theme {
        text: ink,
        warn: Rgba::hex(if light { 0x9D5D00 } else { 0xFCC33C }),
        crit: Rgba::hex(if light { 0xC42B1C } else { 0xFF6B6B }),
        ..*theme
    };
    let gpu = s.gpu.as_ref();
    let cells: [(&str, f32, Option<f32>); 4] = [
        ("CPU", s.cpu, s.cpu_temp),
        ("GPU", gpu.map_or(0.0, |g| g.util), gpu.and_then(|g| g.temp)),
        ("RAM", s.mem_percent(), None),
        ("DISK", s.disk_active, s.disk_temp),
    ];
    let (cell_w, row_h, top) = (92.0, 15.0, (H_DIP - 30.0) / 2.0);
    let mut out = Vec::new();
    for (i, (label, pct, temp)) in cells.into_iter().enumerate() {
        let x = 4.0 + (i % 2) as f32 * (cell_w + 14.0);
        let y = top + (i / 2) as f32 * row_h;
        let run = |text: String, dx: f32, w: f32, right: bool, bold: bool, color: Rgba| Run {
            text,
            x: x + dx,
            y,
            w,
            h: row_h,
            right,
            bold,
            color,
        };
        out.push(run(label.into(), 0.0, 32.0, false, false, dim));
        out.push(run(format!("{pct:.0}%"), 26.0, 32.0, true, true, theme.load(pct)));
        if let Some(t) = temp {
            out.push(run(format!("{t:.0}°"), 58.0, 32.0, true, true, theme.temp(t, dim)));
        }
    }
    out
}

/// Draws white glyphs with GDI, then turns each run's coverage into its
/// premultiplied colour. Empty pixels get alpha 1/255: invisible, but it
/// keeps the whole box clickable (alpha 0 would pass clicks to the taskbar).
fn render(s: &mut Surface, runs: &[Run]) {
    s.pixels().fill(0);
    let px = |r: &Run| {
        let k = s.scale;
        RECT {
            left: (r.x * k).round() as i32,
            top: (r.y * k).round() as i32,
            right: ((r.x + r.w) * k).round() as i32,
            bottom: ((r.y + r.h) * k).round() as i32,
        }
    };
    let rects: Vec<RECT> = runs.iter().map(px).collect();
    unsafe {
        for (run, rect) in runs.iter().zip(&rects) {
            SelectObject(s.dc, if run.bold { s.value } else { s.label }.into());
            let mut text: Vec<u16> = run.text.encode_utf16().collect();
            let mut rc = *rect;
            let align = if run.right { DT_RIGHT } else { DT_LEFT };
            DrawTextW(s.dc, &mut text, &mut rc, align | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        }
        GdiFlush().ok().ok();
    }
    let (w, h) = (s.w, s.h);
    let pixels = s.pixels();
    for (run, rc) in runs.iter().zip(&rects) {
        let Rgba(r, g, b, a) = run.color;
        for y in rc.top.max(0)..rc.bottom.min(h) {
            for x in rc.left.max(0)..rc.right.min(w) {
                let p = &mut pixels[(y * w + x) as usize];
                // GDI coverage is linear; a gamma curve keeps stems as thin as
                // DirectWrite renders them instead of bleeding into bold.
                let coverage = ((*p & 0xFF) as f32 / 255.0).powf(1.6);
                let alpha = coverage * a;
                let ch = |c: f32| ((c * alpha * 255.0).round() as u32).min(255);
                *p = ((alpha * 255.0).round() as u32) << 24 | ch(r) << 16 | ch(g) << 8 | ch(b);
            }
        }
    }
    for p in pixels.iter_mut() {
        if *p >> 24 == 0 {
            *p = 0x0100_0000;
        }
    }
}

/// Top-left corner in pixels from the taskbar's corner, and the taskbar's DPI scale.
fn placement() -> Option<(i32, i32, f32)> {
    if matches!(
        unsafe { SHQueryUserNotificationState() },
        Ok(QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE)
    ) {
        return None;
    }
    let mut abd = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, ..Default::default() };
    if unsafe { SHAppBarMessage(ABM_GETSTATE, &mut abd) } as u32 & ABS_AUTOHIDE != 0 {
        return None;
    }
    let taskbar = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.ok()?;
    let mut tb = RECT::default();
    unsafe { GetWindowRect(taskbar, &mut tb) }.ok()?;
    let (tw, th) = (tb.right - tb.left, tb.bottom - tb.top);
    if tw <= th {
        return None; // vertical taskbar (Windows 10 only): no room.
    }
    let scale = unsafe { GetDpiForWindow(taskbar) } as f32 / 96.0;
    let (w, h) = (W_DIP * scale, H_DIP * scale);
    let advanced = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced";
    // Windows 11 defaults: centred icons, Widgets shown.
    let centred = reg_dword(HKEY_CURRENT_USER, advanced, "TaskbarAl").is_none_or(|v| v != 0);
    let widgets = reg_dword(HKEY_CURRENT_USER, advanced, "TaskbarDa").is_none_or(|v| v != 0);
    let x = if centred {
        (if widgets { WIDGETS_DIP } else { EDGE_DIP }) * scale
    } else {
        // Left-aligned icons: sit just before the notification area.
        let tray = unsafe { FindWindowExW(Some(taskbar), None, w!("TrayNotifyWnd"), PCWSTR::null()) }.ok()?;
        let mut tr = RECT::default();
        unsafe { GetWindowRect(tray, &mut tr) }.ok()?;
        (tr.left - tb.left) as f32 - w - EDGE_DIP * scale
    };
    let y = (th as f32 - h) / 2.0;
    Some((x.round() as i32, y.round() as i32, scale))
}

/// Whether any of the taskbar's own children sit above `hwnd`. Other apps'
/// windows on the taskbar don't count: answering those would be a fight, seen
/// as the two taking turns on top. The bound guards against the list changing.
fn under_taskbar_content(hwnd: HWND, taskbar: HWND) -> bool {
    let owner = |h| {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        pid
    };
    let shell = owner(taskbar);
    let mut above = hwnd;
    for _ in 0..256 {
        match unsafe { GetWindow(above, GW_HWNDPREV) } {
            Ok(h) if owner(h) == shell => return true,
            Ok(h) => above = h,
            Err(_) => return false,
        }
    }
    false
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let main = HWND(MAIN.load(Ordering::Acquire) as _);
            let m = if msg == WM_LBUTTONUP { WM_APP_SHOW } else { WM_APP_MENU };
            unsafe { PostMessageW(Some(main), m, WPARAM(0), LPARAM(0)) }.ok();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
