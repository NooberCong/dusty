//! Main window: state, input and jobs. The views themselves are in `views`.
//! The window is also the app's message hub (tray, sampler, jobs), so it
//! exists while dusty runs and is simply hidden when "closed".

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U;
use windows::Win32::Graphics::Direct2D::{
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE,
    ID2D1HwndRenderTarget, ID2D1RenderTarget,
};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, ScreenToClient};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, GWLP_USERDATA, GetClientRect, GetCursorPos, GetForegroundWindow, GetSystemMetrics, GetWindowLongPtrW,
    HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT,
    IDC_ARROW, IDC_HAND, IsIconic, IsWindowVisible, LoadCursorW, LoadIconW, MINMAXINFO, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SC_MINIMIZE, SM_CXSCREEN, SM_CYSCREEN, SPI_GETCLIENTAREAANIMATION,
    SW_HIDE, SW_RESTORE, SW_SHOW, SW_SHOWNORMAL, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetCursor, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, SystemParametersInfoW, WM_ACTIVATEAPP, WM_CLOSE, WM_CONTEXTMENU, WM_DPICHANGED, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCALCSIZE,
    WM_NCHITTEST, WM_PAINT, WM_SETCURSOR, WM_SETTINGCHANGE, WM_SIZE, WM_SYSCOMMAND, WNDCLASSW, WS_MAXIMIZEBOX,
    WS_OVERLAPPEDWINDOW,
};
use windows::core::{PCWSTR, w};

use super::anim::{Ease, Spring, Ticker};
use super::gfx::{CENTER, Canvas, Gfx, LEADING, NORMAL, R, SEMIBOLD, TRAILING};
use super::logo::{Logo, TOOTH_DEG};
use super::overlay::Overlay;
use super::theme::{Rgba, Theme};
use super::tray::{self, MenuChoice, Tray};
use super::{WM_APP_JOB, WM_APP_MENU, WM_APP_SAMPLE, WM_APP_SHOW, WM_APP_TICK, WM_APP_TRAY};
use crate::clean::{self, ITEMS, Item, Outcome, Tier, helper};
use crate::monitor::{HISTORY_LEN, History, Monitor, Snapshot};
use crate::settings::{self, Plan, Schedule, Settings, ThemeMode};
use crate::win::{env_path, fmt_bytes, fmt_hour, is_elevated, system_uses_light_theme, wide};

mod views;

pub const CLASS: PCWSTR = w!("dusty.main");
const WIN_W: f32 = 980.0;
const WIN_H: f32 = 680.0;
const MIN_W: f32 = 900.0;
const MIN_H: f32 = 580.0;
const SIDE: f32 = 208.0;
const PAD: f32 = 32.0;
const NAV_Y: f32 = 86.0;
const NAV_H: f32 = 42.0;
const LIST_TOP: f32 = 240.0;
const ROW_H: f32 = 56.0;
const SECTION_H: f32 = 38.0;
const SECTION_GAP: f32 = 12.0;
const LIST_PAD: f32 = 6.0;
/// Empty space this far from the top drags the window.
const CAPTION_H: f32 = 48.0;
const RESIZE_EDGE: f32 = 6.0;
/// Re-scan sizes when the Clean view is reopened after this long.
const RESCAN_AFTER: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tab {
    Monitor,
    Clean,
    Settings,
}

const TABS: [(Tab, &str, &str); 3] =
    [(Tab::Monitor, "Monitor", "\u{E9D2}"), (Tab::Clean, "Clean", "\u{EA99}"), (Tab::Settings, "Settings", "\u{E713}")];

fn nav_y(i: usize) -> f32 {
    NAV_Y + i as f32 * (NAV_H + 4.0)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Tab(Tab),
    Minimize,
    Close,
    Junk,
    Metric(usize),
    Chart,
    Clean,
    Measure,
    Row(usize),
    Overlay,
    Autostart,
    Schedule(Schedule),
    /// Moves the day of automatic cleaning, back or forward.
    ScheduleDay(i8),
    /// Moves its hour, back or forward.
    ScheduleHour(i8),
    Theme(ThemeMode),
    OpenLog,
}

impl Hit {
    /// Whether a click here does anything. The chart only reads out values,
    /// and the cleanup list is locked while a job runs.
    fn clickable(self, busy: bool) -> bool {
        match self {
            Hit::Chart => false,
            Hit::Row(_) => !busy,
            _ => true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Scan,
    Measure,
    Clean { from_tray: bool },
}

enum JobMsg {
    Started(&'static Item),
    Item(&'static Item, Outcome),
    Done { error: Option<String> },
}

struct Row {
    item: &'static Item,
    checked: bool,
    /// Latest measurement; None until measured.
    size: Option<Outcome>,
    /// Waiting to be measured or cleaned, or being worked on.
    pending: bool,
}

struct Res {
    gfx: Gfx,
    fonts: Fonts,
    logo: Logo,
}

struct Fonts {
    brand: IDWriteTextFormat,
    title: IDWriteTextFormat,
    heading: IDWriteTextFormat,
    nav: IDWriteTextFormat,
    nav_icon: IDWriteTextFormat,
    chrome: IDWriteTextFormat,
    body: IDWriteTextFormat,
    cap: IDWriteTextFormat,
    cap_c: IDWriteTextFormat,
    cap_r: IDWriteTextFormat,
    cap_b: IDWriteTextFormat,
    button: IDWriteTextFormat,
    dial: IDWriteTextFormat,
    dial_unit: IDWriteTextFormat,
    hero: IDWriteTextFormat,
    reading: IDWriteTextFormat,
    reading_r: IDWriteTextFormat,
    number: IDWriteTextFormat,
    number_r: IDWriteTextFormat,
    axis: IDWriteTextFormat,
}

impl Fonts {
    fn new(g: &Gfx) -> Self {
        Fonts {
            brand: g.number(23.0, SEMIBOLD, LEADING),
            title: g.display(26.0, SEMIBOLD, LEADING),
            heading: g.display(20.0, SEMIBOLD, LEADING),
            nav: g.format(14.0, NORMAL, LEADING),
            nav_icon: g.icons(16.0),
            chrome: g.icons(10.0),
            body: g.format(14.0, NORMAL, LEADING),
            cap: g.format(12.0, NORMAL, LEADING),
            cap_c: g.format(12.0, NORMAL, CENTER),
            cap_r: g.format(12.0, NORMAL, TRAILING),
            cap_b: g.format(13.0, SEMIBOLD, LEADING),
            button: g.format(15.0, SEMIBOLD, CENTER),
            dial: g.number(60.0, SEMIBOLD, LEADING),
            dial_unit: g.number(20.0, NORMAL, LEADING),
            hero: g.number(26.0, SEMIBOLD, CENTER),
            reading: g.number(20.0, SEMIBOLD, LEADING),
            reading_r: g.number(20.0, SEMIBOLD, TRAILING),
            number: g.number(14.0, NORMAL, LEADING),
            number_r: g.number(14.0, NORMAL, TRAILING),
            axis: g.number(11.0, NORMAL, TRAILING),
        }
    }
}

/// Every value on screen that glides rather than jumps.
struct Motion {
    last: Instant,
    /// 0 to 1 as a view fades in.
    page: Ease,
    /// Sidebar selection, in DIPs.
    nav: Ease,
    /// The cog's turn, in degrees.
    turn: Ease,
    /// The dial's fill, 0 to 1.
    dial: Ease,
    /// The dial is sweeping up after a view change.
    sweeping: bool,
    /// Selected metric row; fractional while sliding.
    metric_row: Ease,
    loads: [Ease; 4],
    /// Samples the chart still has to slide left.
    chart_shift: Ease,
    /// 0 to 1 as the chart morphs from the previous metric.
    chart_mix: Ease,
    /// Selected bytes, counting up and down.
    junk: Ease,
    footer: Ease,
    /// Flash when a clean finishes, decaying to 0.
    cheer: Ease,
    scroll: Spring,
    checks: Vec<Ease>,
    /// Per row, 0 to 1 as its "finished" tick fades in during a clean.
    done: Vec<Ease>,
    /// Top of the highlight on the item being cleaned, in list DIPs.
    step_y: Ease,
    /// 0 to 1 as that highlight appears.
    step_on: Ease,
    toggles: [Ease; 2],
    /// Position of the theme picker's selection, in segments.
    theme_pick: Ease,
    schedule_pick: Ease,
    /// The row that sets when it runs, shown unless it's off.
    schedule_open: Ease,
    /// 0 to 1 as the colours fade to a new theme.
    theme_fade: Ease,
    hover: Vec<(Hit, Ease)>,
}

pub struct App {
    hwnd: HWND,
    /// Direct2D resources exist only while the window is shown.
    res: Option<Res>,
    rt: Option<ID2D1HwndRenderTarget>,
    /// The colours on screen, which differ from `theme_to` only during a fade.
    theme: Theme,
    theme_from: Theme,
    theme_to: Theme,
    light_taskbar: bool,
    scale: f32,
    tab: Tab,
    /// Which of the four metrics the dial and chart show.
    metric: usize,
    prev_metric: usize,
    motion: Motion,
    ticker: Ticker,
    /// Windows' "Animation effects" setting.
    animate: bool,
    foreground: bool,
    /// Latest sample, copied here so frames never touch the sampler's lock.
    snap: Snapshot,
    hist: History,
    ready: bool,
    /// Which temperature sensors have given a reading since dusty started.
    sensors_seen: [bool; 3],
    mouse: (f32, f32),
    hits: Vec<(R, Hit)>,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    tracking_leave: bool,
    monitor: Monitor,
    overlay: Option<Overlay>,
    tray: Tray,
    settings: Settings,
    autostart: bool,
    /// This clean's system items go through the scheduled task, with no prompt.
    quiet: bool,
    elevated: bool,
    rows: Vec<Row>,
    busy: Option<Phase>,
    job_items: usize,
    /// Row a clean is working on right now.
    step: Option<usize>,
    /// Keep that row in view; the user scrolling takes over.
    follow_step: bool,
    freed: Option<(u64, u64)>,
    last_scan: Option<Instant>,
    error: Option<String>,
    jobs: (Sender<JobMsg>, Receiver<JobMsg>),
    taskbar_created: u32,
}

/// Creates the window and app state; the window stays hidden if `hidden`.
pub fn create(hidden: bool) -> windows::core::Result<HWND> {
    let instance = unsafe { GetModuleHandleW(None)? };
    let wc = WNDCLASSW {
        lpfnWndProc: Some(proc),
        hInstance: instance.into(),
        lpszClassName: CLASS,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        hIcon: unsafe { LoadIconW(Some(instance.into()), PCWSTR(1 as _))? },
        ..Default::default()
    };
    unsafe { RegisterClassW(&wc) };
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            CLASS,
            w!("dusty"),
            WS_OVERLAPPEDWINDOW & !WS_MAXIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            100,
            100,
            None,
            None,
            Some(instance.into()),
            None,
        )?
    };
    let settings = Settings::load();
    tray::set_menu_theme(settings.theme);
    let theme = Theme::resolve(settings.theme);
    let rows: Vec<Row> = ITEMS
        .iter()
        .map(|item| Row { item, checked: settings.is_selected(item.id), size: None, pending: false })
        .collect();
    let autostart = settings::autostart_enabled();
    let motion = Motion {
        last: Instant::now(),
        page: Ease::new(0.0),
        nav: Ease::new(nav_y(0)),
        turn: Ease::new(0.0),
        dial: Ease::new(0.0),
        sweeping: false,
        metric_row: Ease::new(0.0),
        loads: [Ease::new(0.0); 4],
        chart_shift: Ease::new(0.0),
        chart_mix: Ease::new(1.0),
        junk: Ease::new(0.0),
        footer: Ease::new(0.0),
        cheer: Ease::new(0.0),
        scroll: Spring::default(),
        checks: rows.iter().map(|r| Ease::new(r.checked as u8 as f32)).collect(),
        done: vec![Ease::new(0.0); rows.len()],
        step_y: Ease::new(0.0),
        step_on: Ease::new(0.0),
        toggles: [Ease::new(settings.overlay as u8 as f32), Ease::new(autostart as u8 as f32)],
        theme_pick: Ease::new(settings.theme.index() as f32),
        schedule_pick: Ease::new(settings.plan.every.index() as f32),
        schedule_open: Ease::new((settings.plan.every != Schedule::Off) as u8 as f32),
        theme_fade: Ease::new(1.0),
        hover: Vec::new(),
    };
    let app = App {
        hwnd,
        overlay: None,
        res: None,
        rt: None,
        theme,
        theme_from: theme,
        theme_to: theme,
        light_taskbar: system_uses_light_theme(),
        scale: unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0,
        tab: Tab::Monitor,
        metric: 0,
        prev_metric: 0,
        motion,
        ticker: Ticker::start(hwnd, WM_APP_TICK),
        animate: animations_enabled(),
        foreground: true,
        snap: Snapshot::default(),
        hist: History::default(),
        ready: false,
        sensors_seen: [false; 3],
        mouse: (0.0, 0.0),
        hits: Vec::new(),
        hover: None,
        pressed: None,
        tracking_leave: false,
        monitor: Monitor::start(hwnd, WM_APP_SAMPLE),
        tray: Tray::new(hwnd),
        autostart,
        quiet: false,
        settings,
        elevated: is_elevated(),
        rows,
        busy: None,
        job_items: 0,
        step: None,
        follow_step: false,
        freed: None,
        last_scan: None,
        error: None,
        jobs: channel(),
        taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
    };
    let cell = Box::new(RefCell::new(app));
    {
        let mut app = cell.borrow_mut();
        app.apply_frame_theme();
        app.center_window();
        let overlay = app.settings.overlay;
        app.set_overlay(overlay);
    }
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(cell) as isize);
        // Recompute the frame now that WM_NCCALCSIZE removes the title bar.
        SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE).ok();
        if !hidden {
            PostMessageW(Some(hwnd), WM_APP_SHOW, WPARAM(0), LPARAM(0)).ok();
        }
    }
    Ok(hwnd)
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // The whole window is client area: dusty draws its own title bar. Answered
    // here because SetWindowPos can send this while the app is borrowed.
    if msg == WM_NCCALCSIZE && wp.0 != 0 {
        return LRESULT(0);
    }
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<App>;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    if msg == windows::Win32::UI::WindowsAndMessaging::WM_NCDESTROY {
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(ptr as *mut RefCell<App>));
        }
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    // Modal loops (the tray menu) re-enter here; those messages get default handling.
    let cell = unsafe { &*ptr };
    let handled = cell.try_borrow_mut().ok().and_then(|mut app| app.handle(msg, wp, lp));
    // The borrow must end first: DefWindowProc sends WM_SIZE and runs the
    // window's move and resize loops, all of which need to reach `handle`.
    handled.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
}

fn lo(lp: LPARAM) -> i16 {
    (lp.0 & 0xFFFF) as u16 as i16
}
fn hi(lp: LPARAM) -> i16 {
    ((lp.0 >> 16) & 0xFFFF) as u16 as i16
}

fn animations_enabled() -> bool {
    let mut on = windows_core::BOOL(1);
    unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut _ as _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()
    };
    on.as_bool()
}

impl App {
    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => self.paint(),
            WM_ERASEBKGND => return Some(LRESULT(1)),
            WM_NCHITTEST => return Some(LRESULT(self.nc_hit(lp) as isize)),
            WM_SIZE => {
                self.resize_target();
                self.clamp_scroll();
                self.update_activity();
                self.invalidate();
            }
            WM_ACTIVATEAPP => {
                self.foreground = wp.0 != 0;
                self.update_activity();
            }
            WM_GETMINMAXINFO => {
                let mmi = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
                mmi.ptMinTrackSize.x = (MIN_W * self.scale) as i32;
                mmi.ptMinTrackSize.y = (MIN_H * self.scale) as i32;
            }
            WM_DPICHANGED => {
                self.scale = (wp.0 & 0xFFFF) as f32 / 96.0;
                let r = unsafe { &*(lp.0 as *const RECT) };
                unsafe {
                    SetWindowPos(self.hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE)
                        .ok()
                };
                // SetWindowPos re-enters while the app is borrowed, so its WM_SIZE went unseen.
                self.resize_target();
                if let Some(rt) = &self.rt {
                    unsafe { rt.SetDpi(96.0 * self.scale, 96.0 * self.scale) };
                }
                self.invalidate();
            }
            WM_SETTINGCHANGE => {
                self.set_theme(Theme::resolve(self.settings.theme));
                self.light_taskbar = system_uses_light_theme();
                self.animate = animations_enabled();
                self.invalidate();
            }
            WM_SETCURSOR if lo(lp) as u32 == HTCLIENT => {
                let mut p = POINT::default();
                unsafe {
                    GetCursorPos(&mut p).ok();
                    ScreenToClient(self.hwnd, &mut p).ok().ok();
                }
                self.set_cursor(self.hit_at(p.x as f32, p.y as f32));
                return Some(LRESULT(1));
            }
            WM_MOUSEMOVE => self.mouse_move(lo(lp) as f32, hi(lp) as f32),
            WM_MOUSELEAVE => {
                self.tracking_leave = false;
                self.set_hover(None);
            }
            WM_LBUTTONDOWN => self.pressed = self.hit_at(lo(lp) as f32, hi(lp) as f32),
            WM_LBUTTONUP => {
                let hit = self.hit_at(lo(lp) as f32, hi(lp) as f32);
                if hit.is_some() && hit == self.pressed.take() {
                    self.activate(hit.unwrap());
                }
            }
            WM_MOUSEWHEEL => {
                let delta = (wp.0 >> 16) as u16 as i16 as f32 / 120.0;
                if self.tab == Tab::Clean {
                    self.follow_step = false;
                    self.motion.scroll.target -= delta * 3.0 * 24.0;
                    self.clamp_scroll();
                    self.kick();
                }
            }
            WM_KEYDOWN => self.key(wp.0 as u16),
            WM_CLOSE => {
                // Keep running in the tray; Exit lives in the tray menu.
                self.hide();
                return Some(LRESULT(0));
            }
            WM_APP_TICK => self.on_frame(),
            WM_APP_SAMPLE => self.on_sample(),
            WM_APP_JOB => self.drain_jobs(),
            WM_APP_SHOW => self.show(),
            WM_APP_MENU => self.tray_menu(),
            WM_APP_TRAY => match (lp.0 & 0xFFFF) as u32 {
                // NOTIFYICON_VERSION_4: NIN_SELECT / NIN_KEYSELECT on click, WM_CONTEXTMENU on right click.
                0x400 | 0x401 => self.show(),
                // Hovering: refresh the tooltip now instead of pushing it to Explorer every second.
                WM_MOUSEMOVE => self.refresh_tip(),
                WM_CONTEXTMENU => self.tray_menu(),
                _ => {}
            },
            m if m == self.taskbar_created && m != 0 => self.tray.add(),
            _ => return None,
        }
        Some(LRESULT(0))
    }

    // ---- window plumbing -------------------------------------------------

    fn invalidate(&self) {
        unsafe { InvalidateRect(Some(self.hwnd), None, false).ok().ok() };
    }

    fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool() }
    }

    /// Sample only while something shows the numbers.
    fn update_activity(&self) {
        self.monitor.set_active(self.settings.overlay || self.visible());
    }

    fn apply_frame_theme(&self) {
        let dark: i32 = self.theme_to.dark.into();
        let edge = self.theme_to.stroke;
        // COLORREF 0x00BBGGRR.
        let border = ((edge.2 * 255.0) as u32) << 16 | ((edge.1 * 255.0) as u32) << 8 | (edge.0 * 255.0) as u32;
        let corners = DWMWCP_ROUND;
        // A sliver of glass keeps the shadow, which a frameless window would otherwise lose.
        let margins = MARGINS { cxLeftWidth: 1, cxRightWidth: 1, cyTopHeight: 1, cyBottomHeight: 1 };
        unsafe {
            DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4).ok();
            DwmSetWindowAttribute(self.hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, 4).ok();
            DwmSetWindowAttribute(self.hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corners as *const _ as _, 4).ok();
            DwmExtendFrameIntoClientArea(self.hwnd, &margins).ok();
        }
    }

    /// Fades to `to`, or just switches if nobody can see it.
    fn set_theme(&mut self, to: Theme) {
        if to == self.theme_to {
            return;
        }
        self.theme_from = self.theme;
        self.theme_to = to;
        self.apply_frame_theme();
        if self.visible() {
            self.motion.theme_fade = Ease { value: 0.0, target: 1.0 };
            self.kick();
        } else {
            self.theme = to;
            self.motion.theme_fade.snap(1.0);
        }
    }

    fn center_window(&self) {
        let (w, h) = ((WIN_W * self.scale) as i32, (WIN_H * self.scale) as i32);
        let (sw, sh) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        unsafe { SetWindowPos(self.hwnd, None, (sw - w) / 2, (sh - h) / 2, w, h, SWP_NOZORDER | SWP_NOACTIVATE).ok() };
    }

    fn show(&mut self) {
        let was_visible = self.visible();
        unsafe {
            ShowWindow(self.hwnd, if IsIconic(self.hwnd).as_bool() { SW_RESTORE } else { SW_SHOW }).ok().ok();
            SetForegroundWindow(self.hwnd).ok().ok();
        }
        self.foreground = unsafe { GetForegroundWindow() } == self.hwnd;
        self.update_activity();
        if !was_visible {
            self.enter_page();
        }
        // The first open also finds out what there is to clean.
        if self.tab == Tab::Clean || self.last_scan.is_none() {
            self.maybe_rescan();
        }
    }

    /// Hidden: free all Direct2D state; it is rebuilt on the next paint. Done
    /// here and not on WM_SHOWWINDOW, which arrives while the app is borrowed.
    fn hide(&mut self) {
        unsafe { ShowWindow(self.hwnd, SW_HIDE).ok().ok() };
        self.ticker.stop();
        self.rt = None;
        self.res = None;
        self.update_activity();
    }

    fn resize_target(&self) {
        let mut rc = RECT::default();
        if let Some(rt) = &self.rt
            && unsafe { GetClientRect(self.hwnd, &mut rc) }.is_ok()
        {
            unsafe { rt.Resize(&D2D_SIZE_U { width: rc.right as u32, height: rc.bottom as u32 }) }.ok();
        }
    }

    /// Edges resize, empty space along the top drags, everything else is ours.
    fn nc_hit(&self, lp: LPARAM) -> u32 {
        let mut p = POINT { x: lo(lp) as i32, y: hi(lp) as i32 };
        let mut rc = RECT::default();
        unsafe {
            ScreenToClient(self.hwnd, &mut p).ok().ok();
            GetClientRect(self.hwnd, &mut rc).ok();
        }
        let edge = (RESIZE_EDGE * self.scale) as i32;
        let (left, right) = (p.x < edge, p.x >= rc.right - edge);
        let (top, bottom) = (p.y < edge, p.y >= rc.bottom - edge);
        match (top, bottom, left, right) {
            (true, _, true, _) => HTTOPLEFT,
            (true, _, _, true) => HTTOPRIGHT,
            (_, true, true, _) => HTBOTTOMLEFT,
            (_, true, _, true) => HTBOTTOMRIGHT,
            (_, _, true, _) => HTLEFT,
            (_, _, _, true) => HTRIGHT,
            (_, true, _, _) => HTBOTTOM,
            _ if self.hit_at(p.x as f32, p.y as f32).is_some() => HTCLIENT,
            (true, ..) => HTTOP,
            _ if (p.y as f32) < CAPTION_H * self.scale => HTCAPTION,
            _ => HTCLIENT,
        }
    }

    fn size_dip(&self) -> (f32, f32) {
        let mut rc = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut rc).ok() };
        (rc.right as f32 / self.scale, rc.bottom as f32 / self.scale)
    }

    // ---- input -------------------------------------------------------------

    fn hit_at(&self, px: f32, py: f32) -> Option<Hit> {
        let (x, y) = (px / self.scale, py / self.scale);
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)
    }

    fn mouse_move(&mut self, px: f32, py: f32) {
        if !self.tracking_leave {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            self.tracking_leave = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
        }
        self.mouse = (px / self.scale, py / self.scale);
        self.set_hover(self.hit_at(px, py));
        if self.hover == Some(Hit::Chart) {
            self.invalidate();
        }
    }

    fn set_cursor(&self, hit: Option<Hit>) {
        let pointer = hit.is_some_and(|h| h.clickable(self.busy.is_some()));
        let cursor = unsafe { LoadCursorW(None, if pointer { IDC_HAND } else { IDC_ARROW }) };
        unsafe { SetCursor(cursor.ok()) };
    }

    /// A frame can move things under a still mouse: a scroll, a new view, a
    /// job starting. Windows only asks about the cursor when the mouse moves.
    fn refresh_hover(&mut self) {
        if self.tracking_leave {
            self.set_hover(self.hit_at(self.mouse.0 * self.scale, self.mouse.1 * self.scale));
            self.set_cursor(self.hover);
        }
    }

    fn set_hover(&mut self, h: Option<Hit>) {
        if self.hover == h {
            return;
        }
        self.hover = h;
        for (_, e) in &mut self.motion.hover {
            e.target = 0.0;
        }
        if let Some(h) = h {
            match self.motion.hover.iter_mut().find(|(hit, _)| *hit == h) {
                Some((_, e)) => e.target = 1.0,
                None => self.motion.hover.push((h, Ease { value: 0.0, target: 1.0 })),
            }
        }
        self.kick();
    }

    fn key(&mut self, vk: u16) {
        const VK_TAB: u16 = 0x09;
        const VK_ESCAPE: u16 = 0x1B;
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        match vk {
            VK_ESCAPE => self.hide(),
            VK_TAB if ctrl => {
                let i = TABS.iter().position(|(t, ..)| *t == self.tab).unwrap_or(0);
                self.activate(Hit::Tab(TABS[(i + 1) % TABS.len()].0));
            }
            k if ctrl && (b'1'..=b'3').contains(&(k as u8)) => self.activate(Hit::Tab(TABS[(k as u8 - b'1') as usize].0)),
            _ => {}
        }
    }

    fn activate(&mut self, hit: Hit) {
        match hit {
            Hit::Tab(t) => self.set_tab(t),
            Hit::Junk => self.set_tab(Tab::Clean),
            Hit::Minimize => unsafe {
                PostMessageW(Some(self.hwnd), WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0)).ok();
            },
            Hit::Close => self.hide(),
            Hit::Metric(i) if i != self.metric => {
                self.prev_metric = self.metric;
                self.metric = i;
                self.motion.metric_row.target = i as f32;
                self.motion.chart_mix = Ease { value: 0.0, target: 1.0 };
            }
            Hit::Row(i) if self.busy.is_none() => {
                let row = &mut self.rows[i];
                row.checked = !row.checked;
                self.motion.checks[i].target = row.checked as u8 as f32;
                self.settings.set_selected(row.item.id, row.checked);
                self.settings.save();
            }
            Hit::Clean if self.busy.is_none() => self.start_clean(false),
            Hit::Measure if self.busy.is_none() => self.start_measure(),
            Hit::Overlay => {
                self.settings.overlay = !self.settings.overlay;
                self.settings.save();
                self.set_overlay(self.settings.overlay);
                self.motion.toggles[0].target = self.settings.overlay as u8 as f32;
            }
            Hit::Autostart => {
                settings::set_autostart(!self.autostart);
                self.autostart = settings::autostart_enabled();
                self.motion.toggles[1].target = self.autostart as u8 as f32;
            }
            Hit::Schedule(every) if every != self.settings.plan.every => self.set_plan(Plan { every, ..self.settings.plan }),
            Hit::ScheduleDay(by) => self.set_plan(self.settings.plan.step_day(by)),
            Hit::ScheduleHour(by) => self.set_plan(self.settings.plan.step_hour(by)),
            Hit::Theme(mode) if mode != self.settings.theme => {
                self.settings.theme = mode;
                self.settings.save();
                self.motion.theme_pick.target = mode.index() as f32;
                tray::set_menu_theme(mode);
                self.set_theme(Theme::resolve(mode));
            }
            Hit::OpenLog => {
                if let Some(dir) = env_path("LOCALAPPDATA").map(|l| l.join("dusty")) {
                    std::fs::create_dir_all(&dir).ok();
                    let log = dir.join("clean.log");
                    let target = if log.exists() { log } else { dir };
                    let p = wide(&target);
                    unsafe { ShellExecuteW(Some(self.hwnd), w!("open"), PCWSTR(p.as_ptr()), None, None, SW_SHOWNORMAL) };
                }
            }
            _ => {}
        }
        self.retarget();
        self.invalidate();
    }

    /// The controls only move once the task is registered. Turning it on gives
    /// ticked system items their one prompt, while someone is here to answer.
    fn set_plan(&mut self, plan: Plan) {
        // COM gets a thread of its own, away from the window's.
        let registered = std::thread::spawn(move || clean::schedule::set(plan)).join();
        if !registered.is_ok_and(|r| r.is_some()) {
            return;
        }
        let turned_on = self.settings.plan.every == Schedule::Off && plan.every != Schedule::Off;
        self.settings.plan = plan;
        self.settings.save();
        self.motion.schedule_pick.target = plan.every.index() as f32;
        self.motion.schedule_open.target = (plan.every != Schedule::Off) as u8 as f32;
        let ids: Vec<&str> = self.rows.iter().filter(|r| r.checked && r.item.admin && r.item.default_on).map(|r| r.item.id).collect();
        if turned_on && !ids.is_empty() && !self.elevated {
            std::thread::spawn(move || {
                if !clean::task::covers(&ids, true) {
                    helper::install_task().ok();
                }
            });
        }
    }

    fn set_tab(&mut self, tab: Tab) {
        if tab == self.tab {
            return;
        }
        let index = |t: Tab| TABS.iter().position(|(x, ..)| *x == t).unwrap_or(0);
        let (from, to) = (index(self.tab), index(tab));
        self.tab = tab;
        self.freed = None;
        self.error = None;
        // The logo's cog clicks round a tooth per step, in the direction of travel.
        self.motion.turn.target += (to as f32 - from as f32) * 2.0 * TOOTH_DEG;
        self.motion.nav.target = nav_y(to);
        self.enter_page();
        if tab == Tab::Clean {
            self.maybe_rescan();
        }
    }

    /// Fades the view in and sweeps its dial up from empty.
    fn enter_page(&mut self) {
        self.motion.page = Ease { value: 0.0, target: 1.0 };
        self.motion.dial.value = 0.0;
        self.motion.sweeping = true;
        self.retarget();
    }

    fn set_overlay(&mut self, on: bool) {
        if on && self.overlay.is_none() {
            self.overlay = Overlay::create(self.hwnd);
            let st = self.monitor.state();
            if st.ready
                && let Some(o) = &mut self.overlay
            {
                o.update(&st.now, &self.theme, self.light_taskbar);
            }
        } else if !on {
            self.overlay = None;
        }
        self.update_activity();
    }

    fn tray_menu(&mut self) {
        let choice = self.tray.menu(self.hwnd, self.settings.overlay, self.busy.is_some());
        match choice {
            Some(MenuChoice::Open) => self.show(),
            Some(MenuChoice::QuickClean) if self.busy.is_none() => self.start_clean(true),
            Some(MenuChoice::ToggleOverlay) => self.activate(Hit::Overlay),
            Some(MenuChoice::Exit) => {
                self.overlay = None;
                unsafe { PostQuitMessage(0) };
            }
            _ => {}
        }
        // Job results that arrived while the menu's modal loop ran.
        self.drain_jobs();
        self.kick();
    }

    // ---- sampling ---------------------------------------------------------

    fn refresh_tip(&mut self) {
        let snap = &self.snap;
        let gpu = snap.gpu.as_ref().map_or(String::new(), |g| format!(" · GPU {:.0}%", g.util));
        self.tray.set_tip(&format!("dusty
CPU {:.0}% · RAM {:.0}%{gpu}", snap.cpu, snap.mem_percent()));
    }

    fn on_sample(&mut self) {
        let st = self.monitor.state();
        self.snap = st.now.clone();
        self.hist = st.history.clone();
        self.ready = st.ready;
        drop(st);
        if self.ready {
            for (seen, now) in self.sensors_seen.iter_mut().zip(views::sensor_readings(&self.snap)) {
                *seen |= now;
            }
        }
        if let Some(o) = &mut self.overlay {
            o.update(&self.snap, &self.theme, self.light_taskbar);
        }
        if !self.visible() || self.tab == Tab::Settings && !self.ready {
            return;
        }
        let loads = self.loads();
        let m = &mut self.motion;
        // Glide only in front of the user. Behind other windows a plain 1 Hz
        // repaint keeps the numbers live at a fraction of the cost.
        if self.foreground && self.tab == Tab::Monitor {
            for (e, v) in m.loads.iter_mut().zip(loads) {
                e.target = v;
            }
            m.chart_shift = Ease { value: 1.0, target: 0.0 };
            self.retarget();
            self.ticker.pace(self.motion.sweeping);
        } else {
            for (e, v) in m.loads.iter_mut().zip(loads) {
                e.snap(v);
            }
            m.chart_shift.snap(0.0);
            if self.tab == Tab::Monitor {
                m.dial.snap(loads[self.metric] / 100.0);
            }
            if self.tab != Tab::Clean {
                self.invalidate();
            }
        }
    }

    fn loads(&self) -> [f32; 4] {
        let s = &self.snap;
        [s.cpu, s.mem_percent(), s.disk_active, s.gpu.as_ref().map_or(0.0, |g| g.util)]
    }

    // ---- motion -----------------------------------------------------------

    /// Starts the frame clock; it stops itself once everything has settled.
    fn kick(&mut self) {
        if !self.visible() {
            return;
        }
        if !self.ticker.running() {
            self.motion.last = Instant::now();
        }
        // A frame posted during a modal loop is dropped, so never wait on one.
        self.ticker.ack();
        self.ticker.pace(true);
        self.ticker.run();
    }

    /// Points what the current state decides at where it should be.
    fn retarget(&mut self) {
        let selected = self.selected_bytes();
        let scanned: u64 = self.rows.iter().filter_map(|r| r.size.as_ref()).map(|o| o.tally.bytes).sum();
        let done = self.job_items - self.rows.iter().filter(|r| r.pending).count().min(self.job_items);
        let dial = match self.tab {
            Tab::Monitor => self.loads()[self.metric] / 100.0,
            Tab::Clean if matches!(self.busy, Some(Phase::Clean { .. })) => done as f32 / self.job_items.max(1) as f32,
            Tab::Clean if self.busy.is_some() => 0.0,
            Tab::Clean => selected as f32 / scanned.max(1) as f32,
            Tab::Settings => 0.0,
        };
        let m = &mut self.motion;
        m.dial.target = dial;
        m.junk.target = selected as f32;
        m.footer.target = (self.tab != Tab::Clean && selected > 0) as u8 as f32;
        if self.step.is_none() && !matches!(self.busy, Some(Phase::Clean { .. })) {
            m.step_on.target = 0.0;
        }
        self.kick();
    }

    fn selected_bytes(&self) -> u64 {
        self.rows.iter().filter(|r| r.checked).filter_map(|r| r.size.as_ref()).map(|o| o.tally.bytes).sum()
    }

    fn on_frame(&mut self) {
        self.ticker.ack();
        if !self.visible() {
            self.ticker.stop();
            return;
        }
        let (moving, fine) = self.step_motion();
        self.ticker.pace(fine);
        if !moving {
            self.ticker.stop();
        }
        self.invalidate();
    }

    /// Advances everything by the time since the last frame. Returns whether
    /// anything still moves, and whether any of it answers the user.
    fn step_motion(&mut self) -> (bool, bool) {
        let now = Instant::now();
        let m = &mut self.motion;
        // A huge step lands everything at once, which is how "no animations" works.
        let dt = if self.animate { now.duration_since(m.last).as_secs_f32().min(0.05) } else { 10.0 };
        m.last = now;
        let mut fine = false;
        fine |= m.page.step(dt, 0.06, 0.004);
        fine |= m.nav.step(dt, 0.05, 0.1);
        fine |= m.metric_row.step(dt, 0.05, 0.003);
        fine |= m.chart_mix.step(dt, 0.08, 0.004);
        fine |= m.junk.step(dt, 0.1, 512.0 * 1024.0);
        fine |= m.footer.step(dt, 0.07, 0.004);
        fine |= m.cheer.step(dt, 0.35, 0.004);
        fine |= m.scroll.step(dt, 24.0, 0.2);
        fine |= m.step_y.step(dt, 0.07, 0.1);
        fine |= m.step_on.step(dt, 0.07, 0.004);
        for e in m.checks.iter_mut().chain(m.toggles.iter_mut()).chain(m.done.iter_mut()) {
            fine |= e.step(dt, 0.05, 0.004);
        }
        fine |= m.theme_pick.step(dt, 0.05, 0.003);
        fine |= m.schedule_pick.step(dt, 0.05, 0.003);
        fine |= m.schedule_open.step(dt, 0.05, 0.003);
        fine |= m.theme_fade.step(dt, 0.07, 0.004);
        self.theme = self.theme_from.mix(self.theme_to, m.theme_fade.value);
        for (_, e) in m.hover.iter_mut() {
            fine |= e.step(dt, 0.05, 0.004);
        }
        m.hover.retain(|(_, e)| e.value > 0.0 || e.target > 0.0);

        let mut ambient = m.chart_shift.step(dt, 0.05, 0.01);
        for e in m.loads.iter_mut() {
            ambient |= e.step(dt, 0.05, 0.3);
        }
        // The dial is ambient while it follows a sample, but its sweep on
        // entering a view answers the user.
        let dial = m.dial.step(dt, 0.07, 0.002);
        m.sweeping &= dial;
        if m.sweeping {
            fine = true;
        } else {
            ambient |= dial;
        }
        if self.busy.is_some() && self.animate {
            m.turn.value += dt * 150.0;
            m.turn.target = m.turn.value;
            ambient = true;
        } else {
            fine |= m.turn.step(dt, 0.09, 0.05);
        }
        (fine || ambient, fine)
    }

    // ---- cleaning jobs ----------------------------------------------------

    fn maybe_rescan(&mut self) {
        if self.busy.is_none() && self.last_scan.is_none_or(|t| t.elapsed() > RESCAN_AFTER) {
            self.start_scan();
        }
    }

    /// Measures everything this process may touch (admin items only if elevated).
    fn start_scan(&mut self) {
        let items: Vec<&'static Item> =
            self.rows.iter().filter(|r| !r.item.admin || self.elevated).map(|r| r.item).collect();
        self.spawn(Phase::Scan, items, false);
    }

    /// One UAC prompt to measure the system items.
    fn start_measure(&mut self) {
        let items: Vec<&'static Item> = self.rows.iter().filter(|r| r.item.admin).map(|r| r.item).collect();
        self.spawn(Phase::Measure, items, false);
    }

    fn start_clean(&mut self, from_tray: bool) {
        let items: Vec<&'static Item> = self
            .rows
            .iter()
            .filter(|r| r.checked && (!from_tray || r.item.tier == Tier::Quick))
            .map(|r| r.item)
            .collect();
        if items.is_empty() {
            return;
        }
        self.spawn(Phase::Clean { from_tray }, items, true);
    }

    fn spawn(&mut self, phase: Phase, items: Vec<&'static Item>, delete: bool) {
        for r in &mut self.rows {
            r.pending = items.iter().any(|i| i.id == r.item.id);
        }
        self.busy = Some(phase);
        self.job_items = items.len();
        // The rescan after a clean mustn't wipe what the clean had to report.
        if phase != Phase::Scan {
            self.error = None;
        }
        let system: Vec<&str> = items.iter().filter(|i| i.admin).map(|i| i.id).collect();
        self.quiet = delete && clean::task::covers(&system, delete);
        if matches!(phase, Phase::Clean { .. }) {
            self.follow_step = true;
            for e in &mut self.motion.done {
                e.snap(0.0);
            }
        } else {
            self.freed = None;
        }
        let tx = self.jobs.0.clone();
        let hwnd = self.hwnd.0 as usize;
        let elevated = self.elevated;
        std::thread::spawn(move || {
            let post = || unsafe {
                windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(HWND(hwnd as _)), WM_APP_JOB, WPARAM(0), LPARAM(0))
                    .ok()
            };
            let (local, admin): (Vec<_>, Vec<_>) = items.into_iter().partition(|i| !i.admin || elevated);
            let mut cleaned = Vec::new();
            for item in local {
                tx.send(JobMsg::Started(item)).ok();
                post();
                let o = clean::run(item, delete);
                if delete {
                    cleaned.push((item, o.clone()));
                }
                tx.send(JobMsg::Item(item, o)).ok();
                post();
            }
            let mut error = None;
            if !admin.is_empty() {
                let ids: Vec<&str> = admin.iter().map(|i| i.id).collect();
                let report = |event| {
                    tx.send(match event {
                        helper::Event::Started(item) => JobMsg::Started(item),
                        helper::Event::Finished(item, o) => {
                            if delete {
                                cleaned.push((item, o.clone()));
                            }
                            JobMsg::Item(item, o)
                        }
                    })
                    .ok();
                    post();
                };
                match helper::run_elevated(&ids, delete, report) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        error = Some("System items skipped: administrator permission was declined".into())
                    }
                    Err(e) => error = Some(format!("System items could not be processed: {e}")),
                }
            }
            if delete {
                clean::log(&cleaned);
            }
            tx.send(JobMsg::Done { error }).ok();
            post();
        });
        self.retarget();
        self.invalidate();
    }

    fn drain_jobs(&mut self) {
        let mut done = None;
        let mut cleaned_now = 0u64;
        while let Ok(msg) = self.jobs.1.try_recv() {
            match msg {
                JobMsg::Started(item) => self.start_step(item),
                JobMsg::Item(item, o) => {
                    let cleaning = matches!(self.busy, Some(Phase::Clean { .. }));
                    if cleaning && !o.failed {
                        cleaned_now += o.tally.bytes;
                    }
                    if let Some(i) = self.rows.iter().position(|r| r.item.id == item.id) {
                        if cleaning && !o.failed && o.note.is_none() {
                            self.motion.done[i].target = 1.0;
                        }
                        if self.step == Some(i) {
                            self.step = None;
                        }
                        let r = &mut self.rows[i];
                        r.pending = false;
                        // After a clean the item is (nearly) empty; keep any note.
                        r.size = Some(if cleaning && !o.failed { Outcome { tally: Default::default(), ..o } } else { o });
                    }
                }
                JobMsg::Done { error } => done = Some(error),
            }
        }
        if cleaned_now > 0 || matches!(self.busy, Some(Phase::Clean { .. })) {
            let (bytes, files) = self.freed.unwrap_or((0, 0));
            self.freed = Some((bytes + cleaned_now, files));
        }
        if let Some(error) = done {
            let phase = self.busy.take();
            for r in &mut self.rows {
                r.pending = false;
            }
            self.step = None;
            for e in &mut self.motion.done {
                e.target = 0.0;
            }
            if phase != Some(Phase::Scan) {
                self.error = error;
            }
            match phase {
                Some(Phase::Scan) => self.last_scan = Some(Instant::now()),
                Some(Phase::Clean { from_tray }) => {
                    let freed = self.freed.map_or(0, |f| f.0);
                    if from_tray {
                        self.tray.notify("Quick clean finished", &format!("Freed {}", fmt_bytes(freed)));
                    }
                    // Refresh sizes of what regrows or was skipped.
                    self.start_scan();
                    self.freed = Some((freed, 0));
                    self.busy = Some(Phase::Scan);
                    self.motion.cheer = Ease { value: 1.0, target: 0.0 };
                }
                _ => {}
            }
            if self.busy.is_none() {
                // Let the cog coast to rest with its teeth where they started.
                let turn = &mut self.motion.turn;
                turn.target = (turn.value / TOOTH_DEG).ceil() * TOOTH_DEG;
            }
        }
        self.retarget();
        self.invalidate();
    }

    /// Slides the highlight to the item a clean has moved on to, bringing it
    /// into view if the list needs to scroll.
    fn start_step(&mut self, item: &'static Item) {
        let Some(i) = self.rows.iter().position(|r| r.item.id == item.id) else { return };
        if !matches!(self.busy, Some(Phase::Clean { .. })) {
            return;
        }
        self.step = Some(i);
        let y = self.row_offset(i);
        let m = &mut self.motion;
        if m.step_on.target == 0.0 {
            m.step_y.snap(y);
        }
        m.step_y.target = y;
        m.step_on.target = 1.0;
        let visible = self.size_dip().1 - LIST_TOP;
        let scroll = self.motion.scroll.target;
        if self.follow_step && (y < scroll || y + ROW_H > scroll + visible) {
            self.motion.scroll.target = y - (visible - ROW_H) / 2.0;
            self.clamp_scroll();
        }
    }

    /// Top of a row within the list, which is grouped Quick then Deep.
    fn row_offset(&self, i: usize) -> f32 {
        let tier = self.rows[i].item.tier;
        let before = self.rows[..i].iter().filter(|r| r.item.tier == tier).count();
        let quick = self.rows.iter().filter(|r| r.item.tier == Tier::Quick).count();
        let section = if tier == Tier::Quick { 0.0 } else { quick as f32 * ROW_H + SECTION_H + SECTION_GAP };
        LIST_PAD + section + SECTION_H + before as f32 * ROW_H
    }

    fn clamp_scroll(&mut self) {
        let (_, h) = self.size_dip();
        let max = (self.list_height() - (h - LIST_TOP)).max(0.0);
        let scroll = &mut self.motion.scroll;
        scroll.target = scroll.target.clamp(0.0, max);
    }

    fn list_height(&self) -> f32 {
        2.0 * (SECTION_H + SECTION_GAP) + self.rows.len() as f32 * ROW_H + 16.0
    }

    // ---- painting ---------------------------------------------------------

    fn paint(&mut self) {
        let mut ps = PAINTSTRUCT::default();
        unsafe { BeginPaint(self.hwnd, &mut ps) };
        if self.res.is_none() {
            self.res = Gfx::new().ok().and_then(|gfx| {
                let logo = Logo::new(&gfx.d2d).ok()?;
                Some(Res { fonts: Fonts::new(&gfx), gfx, logo })
            });
        }
        if self.rt.is_none() {
            self.rt = self.create_target();
        }
        if let (Some(rt), Some(res)) = (self.rt.clone(), self.res.take()) {
            unsafe { rt.BeginDraw() };
            let base: &ID2D1RenderTarget = &rt;
            if let Ok(c) = Canvas::new(base, &res.gfx) {
                self.hits.clear();
                self.draw(&c, &res);
            }
            self.res = Some(res);
            if let Err(e) = unsafe { rt.EndDraw(None, None) }
                && e.code() == D2DERR_RECREATE_TARGET
            {
                self.rt = None;
                self.invalidate();
            }
        }
        unsafe { EndPaint(self.hwnd, &ps).ok().ok() };
        self.refresh_hover();
    }

    fn create_target(&self) -> Option<ID2D1HwndRenderTarget> {
        let mut rc = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut rc).ok()? };
        let props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: self.hwnd,
            pixelSize: D2D_SIZE_U { width: rc.right as u32, height: rc.bottom as u32 },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        // The CPU rasteriser is plenty for this UI and avoids loading the GPU
        // driver stack into the process (tens of MB of RAM and many threads).
        let rt_props = D2D1_RENDER_TARGET_PROPERTIES { r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE, ..Default::default() };
        let d2d = &self.res.as_ref()?.gfx.d2d;
        let rt = unsafe { d2d.CreateHwndRenderTarget(&rt_props, &props).ok()? };
        unsafe { rt.SetDpi(96.0 * self.scale, 96.0 * self.scale) };
        Some(rt)
    }
}
