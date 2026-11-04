//! Everything the main window draws: the sidebar, the three views, and the
//! toothed dial they share. Layout is in DIPs and reads animated values from
//! `Motion`, so drawing never changes state beyond recording hit areas.

use std::f32::consts::{FRAC_PI_2, TAU};

use super::*;
use crate::monitor::Snapshot;

const TICKS: usize = 60;
const TITLE_Y: f32 = 18.0;
const DIAL: f32 = 208.0;
const DIAL_TOP: f32 = 72.0;
const METRIC_H: f32 = DIAL / 4.0;
const CLEAN_DIAL: f32 = 156.0;
const SETTING_H: f32 = 66.0;
const GB: f64 = (1u64 << 30) as f64;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Level {
    Warn,
    Crit,
}

struct Metric {
    name: &'static str,
    chart_title: &'static str,
    temp: Option<f32>,
    /// What the hardware is, or for the disk what it's doing.
    detail: String,
    /// Live reading beside the load, for a metric with no temperature.
    reading: Option<String>,
}

/// The one thing most worth knowing about, worst news first. A healthy PC
/// has nothing to report, and the view says nothing.
fn alert(s: &Snapshot) -> Option<(String, Level)> {
    let gpu = s.gpu.as_ref();
    let temps = [("CPU", s.cpu_temp), ("GPU", gpu.and_then(|g| g.temp)), ("Drive", s.disk_temp)];
    let hottest = temps.iter().filter_map(|(n, t)| t.map(|t| (*n, t))).max_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((name, t)) = hottest
        && t >= 95.0
    {
        return Some((format!("{name} is very hot at {t:.0} °C"), Level::Crit));
    }
    if let Some(v) = s.volumes.iter().find(|v| v.free * 20 < v.total) {
        return Some((format!("Drive {}: is almost full", v.letter), Level::Crit));
    }
    if s.mem_percent() >= 90.0 {
        return Some(("Memory is almost full".into(), Level::Warn));
    }
    if let Some((name, t)) = hottest
        && t >= 85.0
    {
        return Some((format!("{name} is running hot at {t:.0} °C"), Level::Warn));
    }
    if s.cpu >= 90.0 {
        return Some(("CPU is under heavy load".into(), Level::Warn));
    }
    None
}

/// Name and explanation of each temperature sensor, for when it stays silent.
const SENSORS: [(&str, &str); 3] = [
    ("CPU", "Not reported by this PC"),
    ("GPU", "No reading from the graphics driver, the GPU may be asleep"),
    ("Drives", "Not reported by the drive"),
];

/// Which of `SENSORS` have a reading now. A PC with no GPU has nothing to miss.
pub(super) fn sensor_readings(s: &Snapshot) -> [bool; 3] {
    [s.cpu_temp.is_some(), s.gpu.as_ref().is_none_or(|g| g.temp.is_some()), s.disk_temp.is_some()]
}

/// The toothed gauge: a minute track whose ticks light up to `value`, the
/// leading tick fading in so the fill moves continuously.
fn dial(c: &Canvas, t: &Theme, center: (f32, f32), r: f32, value: f32, color: Rgba) {
    let lit = value.clamp(0.0, 1.0) * TICKS as f32;
    for i in 0..TICKS {
        let major = i % 5 == 0;
        let len = r * if major { 0.12 } else { 0.08 };
        let on = (lit - i as f32).clamp(0.0, 1.0);
        c.tick(center, tick_angle(i), (r - len, r), t.stroke.mix(color, on), if major { 3.0 } else { 2.2 });
    }
}

fn tick_angle(i: usize) -> f32 {
    i as f32 / TICKS as f32 * TAU - FRAC_PI_2
}

/// A bright run of ticks chasing round the dial: work whose length is unknown.
fn comet(c: &Canvas, center: (f32, f32), r: f32, degrees: f32, color: Rgba) {
    const TAIL: usize = 16;
    let head = (degrees / 360.0 * TICKS as f32) as usize;
    for j in 0..TAIL {
        let i = (head + TICKS - j % TICKS) % TICKS;
        let fade = 1.0 - j as f32 / TAIL as f32;
        let len = r * if i % 5 == 0 { 0.12 } else { 0.08 };
        c.tick(center, tick_angle(i), (r - len, r), color.alpha(fade * fade), if i % 5 == 0 { 3.0 } else { 2.2 });
    }
}

/// Small ring of ticks turning in place: the row being worked on.
fn spinner(c: &Canvas, center: (f32, f32), r: f32, degrees: f32, color: Rgba) {
    const SPOKES: usize = 12;
    let head = (degrees / 360.0 * SPOKES as f32) as usize;
    for j in 0..SPOKES {
        let i = (head + SPOKES - j) % SPOKES;
        let angle = i as f32 / SPOKES as f32 * TAU;
        c.tick(center, angle, (r * 0.55, r), color.alpha(1.0 - j as f32 / SPOKES as f32), 1.6);
    }
}

/// Straight version of the dial's track, for drive space.
fn meter(c: &Canvas, t: &Theme, r: R, value: f32, color: Rgba) {
    let count = (r.w / 6.0) as usize;
    let lit = value.clamp(0.0, 1.0) * count as f32;
    for i in 0..count {
        let x = r.x + 1.0 + i as f32 * r.w / count as f32;
        let on = (lit - i as f32).clamp(0.0, 1.0);
        c.line(x, r.y, x, r.bottom(), t.stroke.mix(color, on), 2.2);
    }
}

fn toggle(c: &Canvas, t: &Theme, r: R, on: f32) {
    c.fill(r, r.h / 2.0, t.stroke.mix(t.accent, on));
    let travel = r.w - r.h;
    let knob = t.text2.mix(t.on_accent, on);
    c.circle(r.x + r.h / 2.0 + travel * on, r.y + r.h / 2.0, r.h / 2.0 - 4.0 + on, knob);
}

fn checkbox(c: &Canvas, t: &Theme, r: R, on: f32) {
    c.stroke(r, 5.0, t.text3.mix(t.accent, on), 1.2);
    if on > 0.01 {
        let grow = r.inset(r.w / 2.0 * (1.0 - on), r.h / 2.0 * (1.0 - on));
        c.fill(grow, 5.0 * on, t.accent);
        c.check(r.inset(2.0, 2.0), t.on_accent.alpha(on), 1.8);
    }
}

impl App {
    pub(super) fn draw(&mut self, c: &Canvas, res: &Res) {
        let t = self.theme;
        let (w, h) = self.size_dip();
        c.clear(t.bg);
        match self.tab {
            Tab::Monitor => self.draw_monitor(c, res, w, h),
            Tab::Clean => self.draw_clean(c, res, w, h),
            Tab::Settings => self.draw_settings(c, res, w),
        }
        let page = self.motion.page.value;
        if page < 1.0 {
            c.fill(R::new(SIDE, 0.0, w - SIDE, h), 0.0, t.bg.alpha(1.0 - page));
        }
        self.draw_sidebar(c, res, h);
        self.draw_window_buttons(c, res, w);
    }

    fn hover(&self, hit: Hit) -> f32 {
        self.motion.hover.iter().find(|(h, _)| *h == hit).map_or(0.0, |(_, e)| e.value)
    }

    fn draw_sidebar(&mut self, c: &Canvas, res: &Res, h: f32) {
        let t = self.theme;
        let f = &res.fonts;
        c.fill(R::new(0.0, 0.0, SIDE, h), 0.0, t.sidebar);
        c.line(SIDE - 0.5, 0.0, SIDE - 0.5, h, t.stroke, 1.0);
        c.logo(&res.logo, (40.0, 42.0), 20.0, self.motion.turn.value.to_radians(), t.accent_hi, t.accent);
        c.text("dusty", &f.brand, R::new(70.0, 24.0, 130.0, 36.0), t.text);

        let y = self.motion.nav.value;
        c.fill(R::new(12.0, y, SIDE - 24.0, NAV_H), 9.0, t.surface);
        c.fill(R::new(12.0, y + 12.0, 3.0, NAV_H - 24.0), 1.5, t.accent);
        for (i, (tab, label, glyph)) in TABS.into_iter().enumerate() {
            let r = R::new(12.0, nav_y(i), SIDE - 24.0, NAV_H);
            let active = self.tab == tab;
            let hover = self.hover(Hit::Tab(tab));
            if !active && hover > 0.0 {
                c.fill(r, 9.0, t.surface.alpha(hover * 0.55));
            }
            c.text(glyph, &f.nav_icon, R::new(r.x + 14.0, r.y, 20.0, r.h), if active { t.accent } else { t.text2 });
            c.text(label, &f.nav, R::new(r.x + 44.0, r.y, r.w - 50.0, r.h), if active { t.text } else { t.text2.mix(t.text, hover) });
            self.hits.push((r, Hit::Tab(tab)));
        }

        // What's waiting to be cleaned, shown from anywhere but the Clean view.
        let a = self.motion.footer.value;
        if a > 0.0 {
            let r = R::new(12.0, h - 80.0, SIDE - 24.0, 64.0);
            let hover = self.hover(Hit::Junk);
            c.fill(r, 10.0, t.surface.alpha(a * (0.5 + 0.5 * hover)));
            let value = fmt_bytes(self.motion.junk.value as u64);
            c.text(&value, &f.reading, R::new(r.x + 14.0, r.y + 9.0, r.w - 24.0, 26.0), t.accent.alpha(a));
            c.text("ready to clean", &f.cap, R::new(r.x + 14.0, r.y + 36.0, r.w - 24.0, 18.0), t.text2.alpha(a));
            if a > 0.5 {
                self.hits.push((r, Hit::Junk));
            }
        }
    }

    fn draw_window_buttons(&mut self, c: &Canvas, res: &Res, w: f32) {
        let t = self.theme;
        let (bw, bh) = (46.0, 34.0);
        for (i, (hit, glyph)) in [(Hit::Minimize, "\u{E921}"), (Hit::Close, "\u{E8BB}")].into_iter().enumerate() {
            let r = R::new(w - bw * (2 - i) as f32, 0.0, bw, bh);
            let hover = self.hover(hit);
            let close = hit == Hit::Close;
            if hover > 0.0 {
                c.fill(r, 0.0, if close { Rgba::hex(0xC42B1C).alpha(hover) } else { t.surface.alpha(hover) });
            }
            let ink = if close { t.text2.mix(Rgba::hex(0xFFFFFF), hover) } else { t.text2.mix(t.text, hover) };
            c.text(glyph, &res.fonts.chrome, r, ink);
            self.hits.push((r, hit));
        }
    }

    fn title(&self, c: &Canvas, res: &Res, text: &str) {
        c.text(text, &res.fonts.title, R::new(SIDE + PAD, TITLE_Y, 400.0, 38.0), self.theme.text);
    }

    // ---- Monitor view -----------------------------------------------------

    fn metrics(&self) -> [Metric; 4] {
        let s = &self.snap;
        let gpu = s.gpu.clone().unwrap_or_default();
        [
            Metric { name: "CPU", chart_title: "CPU load", temp: s.cpu_temp, detail: s.cpu_name.clone(), reading: None },
            Metric {
                name: "Memory",
                chart_title: "Memory in use",
                temp: None,
                detail: s.mem_spec.clone().unwrap_or_else(|| format!("{} installed", fmt_bytes(s.mem_total))),
                reading: Some(format!("{:.1} / {}", s.mem_used as f64 / GB, fmt_bytes(s.mem_total))),
            },
            Metric {
                name: "Disk",
                chart_title: "Disk activity",
                temp: s.disk_temp,
                detail: format!("Read {}/s, write {}/s", fmt_bytes(s.disk_read as u64), fmt_bytes(s.disk_write as u64)),
                reading: None,
            },
            Metric {
                name: "GPU",
                chart_title: "GPU load",
                temp: gpu.temp,
                detail: if gpu.vram_total > 0 {
                    format!("{}, {} of {}", gpu.name, fmt_bytes(gpu.vram_used), fmt_bytes(gpu.vram_total))
                } else {
                    gpu.name
                },
                reading: None,
            },
        ]
    }

    fn series(&self, metric: usize) -> Vec<f32> {
        let h = &self.hist;
        [&h.cpu, &h.mem, &h.disk, &h.gpu][metric].iter().collect()
    }

    fn draw_monitor(&mut self, c: &Canvas, res: &Res, w: f32, h: f32) {
        let t = self.theme;
        let f = &res.fonts;
        let (x0, cw) = (SIDE + PAD, w - SIDE - 2.0 * PAD);
        self.title(c, res, "Monitor");

        if let Some((message, level)) = alert(&self.snap) {
            let x = x0 + res.gfx.measure("Monitor", &f.title) + 20.0;
            c.circle(x + 4.0, TITLE_Y + 21.0, 3.5, if level == Level::Crit { t.crit } else { t.warn });
            c.text(&message, &f.cap, R::new(x + 16.0, TITLE_Y + 11.0, x0 + cw - x - 60.0, 20.0), t.text2);
        }

        let metrics = self.metrics();
        let m = &metrics[self.metric];

        // The dial shows the selected metric.
        let (r, center) = (DIAL / 2.0, (x0 + DIAL / 2.0, DIAL_TOP + DIAL / 2.0));
        let value = self.motion.dial.value;
        let pct = value * 100.0;
        let color = if pct >= 95.0 {
            t.crit
        } else if pct >= 85.0 {
            t.warn
        } else {
            t.accent
        };
        c.glow(center.0, center.1, r * 0.8, t.accent.alpha(if t.dark { 0.03 + 0.09 * value } else { 0.05 + 0.1 * value }));
        dial(c, &t, center, r, value, color);
        let number = if self.ready { format!("{pct:.0}") } else { "–".into() };
        let nw = res.gfx.measure(&number, &f.dial);
        let uw = res.gfx.measure("%", &f.dial_unit);
        let nx = center.0 - (nw + uw) / 2.0;
        c.text(&number, &f.dial, R::new(nx, center.1 - 48.0, nw + 2.0, 72.0), t.text);
        c.text("%", &f.dial_unit, R::new(nx + nw + 1.0, center.1 - 27.0, uw + 2.0, 30.0), t.text2);
        let below = match m.temp {
            Some(temp) => format!("{}  {temp:.0} °C", m.name),
            None => m.name.to_string(),
        };
        c.text(&below, &f.cap_c, R::new(center.0 - 60.0, center.1 + 24.0, 120.0, 18.0), t.text2);

        // Metric rows select what the dial and chart show.
        let (rx, rw) = (x0 + DIAL + 28.0, cw - DIAL - 28.0);
        let py = DIAL_TOP + self.motion.metric_row.value * METRIC_H;
        c.fill(R::new(rx, py + 1.0, rw, METRIC_H - 2.0), 9.0, t.surface);
        c.fill(R::new(rx, py + 15.0, 3.0, METRIC_H - 30.0), 1.5, t.accent);
        for (i, m) in metrics.iter().enumerate() {
            let r = R::new(rx, DIAL_TOP + i as f32 * METRIC_H, rw, METRIC_H);
            let hover = self.hover(Hit::Metric(i));
            if i != self.metric && hover > 0.0 {
                c.fill(r.inset(0.0, 1.0), 9.0, t.surface.alpha(hover * 0.55));
            }
            let text_w = rw - 16.0 - 128.0;
            c.text(m.name, &f.body, R::new(r.x + 16.0, r.y + 7.0, text_w, 20.0), t.text);
            c.text(&m.detail, &f.cap, R::new(r.x + 16.0, r.y + 27.0, text_w, 17.0), t.text2);
            let load = self.motion.loads[i].value;
            let shown = if self.ready { format!("{load:.0}%") } else { "–".into() };
            c.text(&shown, &f.reading_r, R::new(r.right() - 76.0, r.y, 62.0, r.h), t.load(load));
            if let Some(temp) = m.temp {
                c.text(&format!("{temp:.0}°"), &f.number_r, R::new(r.right() - 124.0, r.y, 44.0, r.h), t.temp(temp, t.text2));
            }
            if let Some(reading) = &m.reading {
                c.text(reading, &f.number_r, R::new(r.right() - 224.0, r.y, 144.0, r.h), t.text2);
            }
            self.hits.push((r, Hit::Metric(i)));
        }

        // Drives sit along the bottom; the chart takes whatever is left.
        let n = self.snap.volumes.len();
        let cols = if n > 1 { 2 } else { 1 };
        let rows = n.div_ceil(cols);
        let drives_top = h - 22.0 - rows as f32 * 28.0;
        let col_w = (cw - (cols - 1) as f32 * 32.0) / cols as f32;
        for (i, v) in self.snap.volumes.iter().enumerate() {
            let (x, y) = (x0 + (i % cols) as f32 * (col_w + 32.0), drives_top + (i / cols) as f32 * 28.0);
            let used = 1.0 - v.free as f32 / v.total as f32;
            c.text(&format!("{}:", v.letter), &f.number, R::new(x, y, 24.0, 22.0), t.text);
            let tone = if v.free * 10 < v.total { t.warn } else { t.accent };
            let text_w = 158.0;
            meter(c, &t, R::new(x + 28.0, y + 6.0, col_w - 28.0 - text_w - 8.0, 10.0), used * self.motion.page.value, tone);
            let free = format!("{} free of {}", fmt_bytes(v.free), fmt_bytes(v.total));
            c.text(&free, &f.cap_r, R::new(x + col_w - text_w, y, text_w, 22.0), t.text2);
        }

        let chart_top = DIAL_TOP + DIAL + 18.0;
        self.draw_chart(c, res, R::new(x0, chart_top, cw, drives_top - 14.0 - chart_top), m.chart_title);
    }

    fn draw_chart(&mut self, c: &Canvas, res: &Res, r: R, title: &str) {
        let t = self.theme;
        let f = &res.fonts;
        c.text(title, &f.cap_b, R::new(r.x, r.y, 200.0, 18.0), t.text);
        c.text("Last 60 seconds", &f.cap_r, R::new(r.right() - 200.0, r.y, 200.0, 18.0), t.text3);
        let plot = R::new(r.x + 30.0, r.y + 30.0, r.w - 30.0, (r.h - 36.0).max(40.0));
        for (label, frac) in [("100", 0.0), ("50", 0.5), ("0", 1.0)] {
            let y = (plot.y + plot.h * frac).round() + 0.5;
            c.line(plot.x, y, plot.right(), y, t.stroke, 1.0);
            c.text(label, &f.axis, R::new(r.x, y - 8.0, 24.0, 16.0), t.text3);
        }

        // Switching metric morphs one line into the other.
        let mix = self.motion.chart_mix.value;
        let mut values = self.series(self.metric);
        if mix < 1.0 {
            let old = self.series(self.prev_metric);
            for (v, o) in values.iter_mut().zip(old) {
                *v = o + (*v - o) * mix;
            }
        }
        let shift = self.motion.chart_shift.value;
        let area = R::new(plot.x, plot.y - 2.0, plot.w, plot.h + 3.0);
        c.clip(area, || {
            c.area_chart(plot, &values, HISTORY_LEN, shift, t.accent, t.accent.alpha(if t.dark { 0.26 } else { 0.22 }))
        });

        self.hits.push((area, Hit::Chart));
        if self.hover == Some(Hit::Chart) && !values.is_empty() {
            let step = plot.w / (HISTORY_LEN - 1) as f32;
            let ago = (((plot.right() - self.mouse.0) / step).round().max(0.0) as usize).min(values.len() - 1);
            let v = values[values.len() - 1 - ago];
            let (x, y) = (plot.right() - ago as f32 * step, plot.bottom() - v.clamp(0.0, 100.0) / 100.0 * plot.h);
            c.line(x, plot.y, x, plot.bottom(), t.text3, 1.0);
            c.circle(x, y, 5.5, t.bg);
            c.circle(x, y, 3.5, t.accent);
            let when = if ago == 0 { "Now".to_string() } else { format!("{ago} s ago") };
            let (vw, ww) = (res.gfx.measure(&format!("{v:.0}%"), &f.number), res.gfx.measure(&when, &f.cap));
            let (bw, bh) = (vw + ww + 34.0, 28.0);
            let bx = if x + 12.0 + bw > plot.right() { x - 12.0 - bw } else { x + 12.0 };
            let by = (y - bh / 2.0).clamp(plot.y, plot.bottom() - bh);
            c.fill(R::new(bx, by, bw, bh), 7.0, t.surface);
            c.stroke(R::new(bx, by, bw, bh), 7.0, t.stroke, 1.0);
            c.text(&format!("{v:.0}%"), &f.number, R::new(bx + 11.0, by, vw + 2.0, bh), t.text);
            c.text(&when, &f.cap, R::new(bx + 23.0 + vw, by, ww + 2.0, bh), t.text2);
        }
    }

    // ---- Clean view -------------------------------------------------------

    fn draw_clean(&mut self, c: &Canvas, res: &Res, w: f32, h: f32) {
        let t = self.theme;
        let f = &res.fonts;
        let (x0, cw) = (SIDE + PAD, w - SIDE - 2.0 * PAD);
        self.title(c, res, "Clean");

        // The dial is the progress of whatever is happening.
        let (r, center) = (CLEAN_DIAL / 2.0, (x0 + CLEAN_DIAL / 2.0, 72.0 + CLEAN_DIAL / 2.0));
        let cheer = self.motion.cheer.value;
        let working = self.busy.is_some();
        c.glow(center.0, center.1, r * (0.85 + 0.5 * cheer), t.accent.alpha(0.07 + 0.25 * cheer));
        dial(c, &t, center, r, self.motion.dial.value, t.accent.mix(t.accent_hi, cheer));
        if working {
            comet(c, center, r, self.motion.turn.value, t.accent_hi);
        }
        let scanning = self.busy == Some(Phase::Scan) && self.last_scan.is_none();
        let (number, caption) = match self.clean_progress() {
            Some((done, total)) => (format!("{done} of {total}"), "items done"),
            None if scanning => ("–".to_string(), "selected"),
            None => (fmt_bytes(self.motion.junk.value as u64), "selected"),
        };
        c.text(&number, &f.hero, R::new(center.0 - 62.0, center.1 - 26.0, 124.0, 40.0), t.text);
        c.text(caption, &f.cap_c, R::new(center.0 - 60.0, center.1 + 12.0, 120.0, 18.0), t.text2);

        let tx = x0 + CLEAN_DIAL + 32.0;
        let tw = cw - CLEAN_DIAL - 32.0;
        let (headline, sub, sub_color) = self.clean_status();
        c.text(&headline, &f.heading, R::new(tx, 84.0, tw, 30.0), t.text);
        c.text(&sub, &f.cap, R::new(tx, 114.0, tw, 20.0), sub_color);

        let any_checked = self.rows.iter().any(|r| r.checked);
        let enabled = self.busy.is_none() && any_checked;
        let btn = R::new(tx, 148.0, 148.0, 44.0);
        if enabled {
            let hover = self.hover(Hit::Clean);
            if hover > 0.0 {
                c.glow(btn.x + btn.w / 2.0, btn.y + btn.h / 2.0, 96.0, t.accent.alpha(0.16 * hover));
            }
            let lift = Rgba(1.0, 1.0, 1.0, 1.0);
            c.fill_v(btn, 10.0, t.accent_hi.mix(lift, 0.12 * hover), t.accent.mix(lift, 0.12 * hover));
            c.text("Clean", &f.button, btn, t.on_accent);
            self.hits.push((btn, Hit::Clean));
        } else {
            c.fill(btn, 10.0, t.surface);
            let label = if matches!(self.busy, Some(Phase::Clean { .. })) { "Cleaning…" } else { "Clean" };
            c.text(label, &f.button, btn, t.text3);
        }

        // List. Scrolled by whole device pixels: text snaps to them, so any
        // finer offset would make it shimmer against the lines beside it.
        let scroll = (self.motion.scroll.value * self.scale).round() / self.scale;
        let view = R::new(SIDE, LIST_TOP, w - SIDE, h - LIST_TOP);
        let mut hits = Vec::new();
        c.clip(view, || {
            let on = self.motion.step_on.value;
            if on > 0.0 {
                let r = R::new(x0 - 12.0, LIST_TOP + self.motion.step_y.value - scroll, cw + 24.0, ROW_H);
                c.fill(r.inset(0.0, 1.0), 9.0, t.surface.alpha(on));
                c.fill(R::new(r.x, r.y + 15.0, 3.0, ROW_H - 30.0), 1.5, t.accent.alpha(on));
            }
            let mut y = LIST_TOP + LIST_PAD - scroll;
            for tier in [Tier::Quick, Tier::Deep] {
                let label = if tier == Tier::Quick { "Quick" } else { "Deep" };
                c.text(label, &f.cap_b, R::new(x0, y, 200.0, SECTION_H - 6.0), t.text);
                let needs_measure = tier == Tier::Deep
                    && !self.elevated
                    && self.busy.is_none()
                    && self.rows.iter().any(|r| r.item.admin && r.size.is_none());
                if needs_measure {
                    let text = "Measure system items";
                    let mw = res.gfx.measure(text, &f.cap);
                    let r = R::new(x0 + cw - mw - 6.0, y, mw + 6.0, SECTION_H - 6.0);
                    let ink = t.accent.mix(t.text, self.hover(Hit::Measure));
                    c.text(text, &f.cap_r, R::new(r.x, r.y, r.w, r.h), ink);
                    hits.push((r, Hit::Measure));
                }
                y += SECTION_H;
                for (i, row) in self.rows.iter().enumerate().filter(|(_, r)| r.item.tier == tier) {
                    let r = R::new(x0 - 12.0, y, cw + 24.0, ROW_H);
                    if r.bottom() > LIST_TOP && r.y < h {
                        self.draw_row(c, res, i, row, r);
                        hits.push((r, Hit::Row(i)));
                    }
                    y += ROW_H;
                }
                y += SECTION_GAP;
            }
        });
        // Only rows inside the visible list area are clickable.
        self.hits.extend(hits.into_iter().filter_map(|(r, hit)| {
            let y0 = r.y.max(LIST_TOP);
            let y1 = r.bottom().min(h);
            (y1 > y0).then_some((R::new(r.x, y0, r.w, y1 - y0), hit))
        }));

        // The list fades out under its edges instead of being cut off.
        c.fill_v(R::new(SIDE, LIST_TOP, w - SIDE - 10.0, 12.0), 0.0, t.bg, t.bg.alpha(0.0));
        c.fill_v(R::new(SIDE, h - 24.0, w - SIDE - 10.0, 24.0), 0.0, t.bg.alpha(0.0), t.bg);

        let content = self.list_height();
        let visible = h - LIST_TOP;
        if content > visible {
            let track = visible - 16.0;
            let thumb = (track * visible / content).max(24.0);
            let pos = LIST_TOP + 8.0 + (track - thumb) * (scroll / (content - visible));
            c.fill(R::new(w - 7.0, pos, 3.0, thumb), 1.5, t.text3.alpha(0.5));
        }
    }

    /// Headline, detail line and the detail's colour, for the Clean view.
    fn clean_status(&self) -> (String, String, Rgba) {
        let t = self.theme;
        let freed = self.freed.map_or(0, |f| f.0);
        let checked = self.rows.iter().filter(|r| r.checked).count();
        let count = format!("{checked} of {} items selected", self.rows.len());
        if let Some(e) = &self.error {
            let headline = if freed > 0 { format!("Cleaned {}", fmt_bytes(freed)) } else { "Some items were skipped".into() };
            return (headline, e.clone(), t.warn);
        }
        match self.busy {
            Some(Phase::Clean { .. }) => {
                let headline = if freed > 0 { format!("Cleaned {} so far", fmt_bytes(freed)) } else { "Cleaning".into() };
                (headline, self.cleaning_step(), t.text2)
            }
            Some(Phase::Measure) => ("Measuring system items".into(), "Windows is adding up what it can remove".into(), t.text2),
            Some(Phase::Scan) if self.last_scan.is_none() => ("Scanning".into(), "Adding up what can be removed".into(), t.text2),
            _ if freed > 0 => (format!("Cleaned {}", fmt_bytes(freed)), count, t.text2),
            _ if checked == 0 => ("Nothing selected".into(), "Tick the items you want to clean".into(), t.text2),
            _ if self.rows.iter().any(|r| r.checked && r.size.is_none() && r.item.admin) => {
                ("Ready to clean".into(), format!("{count}, system items not measured yet"), t.text2)
            }
            _ => ("Ready to clean".into(), count, t.text2),
        }
    }

    /// Items finished and items in all, while a clean runs.
    fn clean_progress(&self) -> Option<(usize, usize)> {
        matches!(self.busy, Some(Phase::Clean { .. })).then(|| {
            let waiting = self.rows.iter().filter(|r| r.pending).count();
            (self.job_items - waiting.min(self.job_items), self.job_items)
        })
    }

    /// What a running clean is doing. With no current item, either the
    /// elevated helper hasn't started or the last item has just finished.
    fn cleaning_step(&self) -> String {
        match self.step {
            Some(i) => format!("{}…", self.rows[i].item.name),
            None if self.rows.iter().any(|r| r.pending && r.item.admin) && !self.elevated => {
                if self.quiet { "Starting system items…" } else { "Waiting for administrator permission" }.into()
            }
            None => "Finishing…".into(),
        }
    }

    fn draw_row(&self, c: &Canvas, res: &Res, i: usize, row: &Row, r: R) {
        let t = self.theme;
        let f = &res.fonts;
        let hover = if self.busy.is_none() { self.hover(Hit::Row(i)) } else { 0.0 };
        if hover > 0.0 {
            c.fill(r.inset(0.0, 1.0), 9.0, t.surface.alpha(hover * 0.7));
        }
        c.line(r.x + 12.0, r.bottom() - 0.5, r.right() - 12.0, r.bottom() - 0.5, t.stroke.alpha(0.6 * (1.0 - hover)), 1.0);
        checkbox(c, &t, R::new(r.x + 12.0, r.y + (r.h - 20.0) / 2.0, 20.0, 20.0), self.motion.checks[i].value);
        let (text_x, size_w) = (r.x + 48.0, 120.0);
        let text_w = r.w - 48.0 - size_w - 20.0;
        c.text(row.item.name, &f.body, R::new(text_x, r.y + 9.0, text_w, 20.0), t.text);
        let note = row.size.as_ref().and_then(|o| o.note.as_deref());
        let (detail, color) = match note {
            Some(n) => (n, t.warn),
            None => (row.item.detail, t.text2),
        };
        c.text(detail, &f.cap, R::new(text_x, r.y + 29.0, text_w, 17.0), color);
        let size_box = R::new(r.right() - 12.0 - size_w, r.y, size_w, r.h);
        if self.step == Some(i) {
            spinner(c, (size_box.right() - 9.0, r.y + r.h / 2.0), 9.0, self.motion.turn.value, t.accent);
            return;
        }
        let done = self.motion.done[i].value;
        if done > 0.0 {
            c.check(R::new(size_box.right() - 20.0, r.y + (r.h - 20.0) / 2.0, 20.0, 20.0), t.good.alpha(done), 1.8);
            return;
        }
        let (size, color, font) = match (&row.size, row.pending) {
            (_, true) => ("…".to_string(), t.text3, &f.number_r),
            (Some(o), _) if o.failed => ("Unavailable".to_string(), t.text3, &f.cap_r),
            (Some(o), _) => (fmt_bytes(o.tally.bytes), t.text, &f.number_r),
            (None, _) if row.item.admin && !self.elevated => ("Needs admin".to_string(), t.text3, &f.cap_r),
            (None, _) => ("–".to_string(), t.text3, &f.number_r),
        };
        c.text(&size, font, size_box, color);
    }

    // ---- Settings view ----------------------------------------------------

    /// A row with a picker on its right: the brass pill slides to the choice.
    fn draw_picker(&mut self, c: &Canvas, res: &Res, r: R, text: (&str, &str), options: &[(Hit, &str)], pick: f32) {
        const SEGMENT_W: f32 = 68.0;
        const TRACK_H: f32 = 32.0;
        let t = self.theme;
        let f = &res.fonts;
        c.line(r.x + 12.0, r.bottom() - 0.5, r.right() - 12.0, r.bottom() - 0.5, t.stroke.alpha(0.6), 1.0);
        let track_w = options.len() as f32 * SEGMENT_W;
        let text_w = r.w - track_w - 36.0;
        c.text(text.0, &f.body, R::new(r.x + 12.0, r.y + 13.0, text_w, 20.0), t.text);
        c.text(text.1, &f.cap, R::new(r.x + 12.0, r.y + 34.0, text_w, 18.0), t.text2);

        let track = R::new(r.right() - 12.0 - track_w, r.y + (r.h - TRACK_H) / 2.0, track_w, TRACK_H);
        c.fill(track, 9.0, t.surface);
        let segment = |i: f32| R::new(track.x + i * SEGMENT_W, track.y, SEGMENT_W, TRACK_H);
        c.fill(segment(pick).inset(3.0, 3.0), 6.5, t.accent);
        for (i, &(hit, label)) in options.iter().enumerate() {
            let chosen = (1.0 - (pick - i as f32).abs()).clamp(0.0, 1.0);
            let ink = t.text2.mix(t.text, self.hover(hit)).mix(t.on_accent, chosen);
            c.text(label, &f.cap_c, segment(i as f32), ink);
            self.hits.push((segment(i as f32), hit));
        }
    }

    /// A value with an arrow either side. Fades in with `open`, and only takes
    /// clicks once it's mostly there.
    fn draw_stepper(&mut self, c: &Canvas, res: &Res, r: R, label: &str, hits: [Hit; 2], open: f32) {
        const ARROW_W: f32 = 34.0;
        let t = self.theme;
        let f = &res.fonts;
        c.fill(r, 9.0, t.surface.alpha(open));
        c.text(label, &f.cap_c, r, t.text.alpha(open));
        for (hit, glyph, x) in [(hits[0], "‹", r.x), (hits[1], "›", r.right() - ARROW_W)] {
            let arrow = R::new(x, r.y, ARROW_W, r.h);
            c.text(glyph, &f.cap_c, arrow, t.text2.mix(t.accent, self.hover(hit)).alpha(open));
            if open > 0.5 {
                self.hits.push((arrow, hit));
            }
        }
    }

    /// The day, if the frequency has one, and the hour automatic cleaning runs.
    fn draw_schedule_when(&mut self, c: &Canvas, res: &Res, r: R, open: f32) {
        const TIME_W: f32 = 136.0;
        const DAY_W: f32 = 158.0;
        const STEPPER_H: f32 = 32.0;
        let t = self.theme;
        let f = &res.fonts;
        let plan = self.settings.plan;
        c.line(r.x + 12.0, r.bottom() - 0.5, r.right() - 12.0, r.bottom() - 0.5, t.stroke.alpha(0.6 * open), 1.0);
        let text_w = r.w - TIME_W - DAY_W - 44.0;
        c.text("When", &f.body, R::new(r.x + 12.0, r.y + 13.0, text_w, 20.0), t.text.alpha(open));
        c.text("If the PC is off, it runs once it's back on", &f.cap, R::new(r.x + 12.0, r.y + 34.0, text_w, 18.0), t.text2.alpha(open));

        let y = r.y + (r.h - STEPPER_H) / 2.0;
        let time = R::new(r.right() - 12.0 - TIME_W, y, TIME_W, STEPPER_H);
        self.draw_stepper(c, res, time, &fmt_hour(plan.hour), [Hit::ScheduleHour(-1), Hit::ScheduleHour(1)], open);
        if let Some(day) = plan.day_label() {
            let r = R::new(time.x - 8.0 - DAY_W, y, DAY_W, STEPPER_H);
            self.draw_stepper(c, res, r, &day, [Hit::ScheduleDay(-1), Hit::ScheduleDay(1)], open);
        }
    }

    fn draw_settings(&mut self, c: &Canvas, res: &Res, w: f32) {
        let t = self.theme;
        let f = &res.fonts;
        let (x0, cw) = (SIDE + PAD, w - SIDE - 2.0 * PAD);
        self.title(c, res, "Settings");

        let mut y = 72.0;
        let themes = ThemeMode::ALL.map(|(mode, label)| (Hit::Theme(mode), label));
        let text = ("Theme", "System follows your Windows setting");
        self.draw_picker(c, res, R::new(x0 - 12.0, y, cw + 24.0, SETTING_H), text, &themes, self.motion.theme_pick.value);
        y += SETTING_H;

        let rows = [
            (Hit::Overlay, "Taskbar readout", "Load and temperatures next to the Start button"),
            (Hit::Autostart, "Start with Windows", "Opens quietly in the notification area"),
        ];
        for (i, (hit, title, desc)) in rows.into_iter().enumerate() {
            let r = R::new(x0 - 12.0, y, cw + 24.0, SETTING_H);
            let hover = self.hover(hit);
            if hover > 0.0 {
                c.fill(r.inset(0.0, 1.0), 9.0, t.surface.alpha(hover * 0.7));
            }
            c.line(r.x + 12.0, r.bottom() - 0.5, r.right() - 12.0, r.bottom() - 0.5, t.stroke.alpha(0.6 * (1.0 - hover)), 1.0);
            c.text(title, &f.body, R::new(r.x + 12.0, r.y + 13.0, r.w - 90.0, 20.0), t.text);
            c.text(desc, &f.cap, R::new(r.x + 12.0, r.y + 34.0, r.w - 90.0, 18.0), t.text2);
            toggle(c, &t, R::new(r.right() - 12.0 - 44.0, r.y + (r.h - 24.0) / 2.0, 44.0, 24.0), self.motion.toggles[i].value);
            self.hits.push((r, hit));
            y += SETTING_H;
        }

        let schedules = Schedule::ALL.map(|(schedule, label)| (Hit::Schedule(schedule), label));
        let text = ("Automatic cleaning", "Cleans your ticked items, except the Recycle Bin");
        self.draw_picker(c, res, R::new(x0 - 12.0, y, cw + 24.0, SETTING_H), text, &schedules, self.motion.schedule_pick.value);
        y += SETTING_H;
        let open = self.motion.schedule_open.value;
        if open > 0.0 {
            self.draw_schedule_when(c, res, R::new(x0 - 12.0, y, cw + 24.0, SETTING_H), open);
            y += SETTING_H * open;
        }

        // Sensors only come up when one has never answered. A reading since
        // start proves it works, even if a sleeping GPU has gone quiet now.
        let silent: Vec<_> = SENSORS.iter().zip(self.sensors_seen).filter(|(_, seen)| self.ready && !seen).collect();
        if !silent.is_empty() {
            y += 26.0;
            c.text("Missing temperatures", &f.cap_b, R::new(x0, y, 300.0, 20.0), t.text);
            y += 30.0;
            for ((name, why), _) in silent {
                c.circle(x0 + 4.0, y + 11.0, 3.5, t.warn);
                c.text(name, &f.body, R::new(x0 + 18.0, y, 80.0, 22.0), t.text);
                c.text(why, &f.cap, R::new(x0 + 96.0, y, cw - 96.0, 22.0), t.text2);
                y += 30.0;
            }
        }

        y += 22.0;
        let text = "Open clean log";
        let lw = res.gfx.measure(text, &f.cap);
        let r = R::new(x0, y, lw + 4.0, 22.0);
        c.text(text, &f.cap, r, t.accent.mix(t.text, self.hover(Hit::OpenLog)));
        self.hits.push((r, Hit::OpenLog));
        let version = concat!("Version ", env!("CARGO_PKG_VERSION"));
        c.text(version, &f.cap_r, R::new(x0 + cw - 200.0, y, 200.0, 22.0), t.text3);
    }
}

#[cfg(test)]
mod tests {
    use super::super::Hit;
    use super::{Level, alert, sensor_readings};
    use crate::monitor::{GpuStat, Snapshot, Volume};

    fn calm() -> Snapshot {
        Snapshot { cpu: 10.0, mem_used: 4, mem_total: 16, ..Default::default() }
    }

    #[test]
    fn the_pointer_shows_only_where_a_click_does_something() {
        assert!(Hit::Close.clickable(false));
        assert!(Hit::Row(0).clickable(false));
        assert!(!Hit::Row(0).clickable(true));
        assert!(!Hit::Chart.clickable(false));
    }

    #[test]
    fn only_a_gpu_that_exists_can_miss_a_temperature() {
        let mut s = calm();
        assert_eq!(sensor_readings(&s), [false, true, false]);
        s.gpu = Some(GpuStat::default());
        assert_eq!(sensor_readings(&s), [false, false, false]);
    }

    #[test]
    fn a_calm_pc_says_nothing() {
        assert_eq!(alert(&calm()), None);
    }

    #[test]
    fn the_hottest_sensor_is_named() {
        let mut s = calm();
        s.cpu_temp = Some(88.0);
        s.gpu = Some(GpuStat { temp: Some(96.0), ..Default::default() });
        assert_eq!(alert(&s), Some(("GPU is very hot at 96 °C".to_string(), Level::Crit)));
    }

    #[test]
    fn a_full_drive_outranks_heavy_load() {
        let mut s = calm();
        s.cpu = 99.0;
        s.volumes.push(Volume { letter: 'C', free: 1, total: 100 });
        assert_eq!(alert(&s), Some(("Drive C: is almost full".to_string(), Level::Crit)));
    }
}
