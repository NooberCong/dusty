//! Thin Direct2D / DirectWrite layer: factories, text formats and a Canvas
//! with the handful of primitives the UI needs. Everything is in DIPs.

use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_BEZIER_SEGMENT, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW,
    D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_GRADIENT_STOP,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BRUSH_PROPERTIES, D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_OPTIONS,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2, D2D1_LINE_JOIN_ROUND,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES, D2D1CreateFactory, ID2D1Factory, ID2D1GradientStopCollection,
    ID2D1LinearGradientBrush, ID2D1PathGeometry, ID2D1RenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat,
};
use windows::core::{PCWSTR, Result};
use windows_numerics::{Matrix3x2, Vector2};

use super::logo::Logo;
use super::theme::Rgba;
use crate::win::wide;

pub use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL as NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD as SEMIBOLD,
    DWRITE_TEXT_ALIGNMENT_CENTER as CENTER, DWRITE_TEXT_ALIGNMENT_LEADING as LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING as TRAILING,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct R {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl R {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        R { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn inset(&self, dx: f32, dy: f32) -> R {
        R::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }
    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
}

pub fn color(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.0, g: c.1, b: c.2, a: c.3 }
}

pub struct Gfx {
    pub d2d: ID2D1Factory,
    dw: IDWriteFactory,
    round: ID2D1StrokeStyle,
    text_family: Vec<u16>,
    display_family: Vec<u16>,
    number_family: Vec<u16>,
    icon_family: Vec<u16>,
}

impl Gfx {
    pub fn new() -> Result<Self> {
        let d2d: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, Some(&D2D1_FACTORY_OPTIONS::default()))? };
        let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let pick = |names: &[&str]| {
            let found = names.iter().find(|n| family_exists(&dw, n)).copied();
            wide(found.unwrap_or("Segoe UI"))
        };
        let round = unsafe {
            d2d.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    miterLimit: 10.0,
                    ..Default::default()
                },
                None,
            )?
        };
        Ok(Gfx {
            text_family: pick(&["Segoe UI Variable Text", "Segoe UI"]),
            display_family: pick(&["Segoe UI Variable Display", "Segoe UI"]),
            // Windows' own DIN: instrument-panel digits, at no cost in exe size.
            number_family: pick(&["Bahnschrift", "Segoe UI Variable Display", "Segoe UI"]),
            icon_family: pick(&["Segoe Fluent Icons", "Segoe MDL2 Assets"]),
            round,
            d2d,
            dw,
        })
    }

    /// Single-line, vertically centred, ellipsis-trimmed text format.
    pub fn format(&self, size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT) -> IDWriteTextFormat {
        self.format_in(&self.text_family, size, weight, align)
    }

    pub fn display(&self, size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT) -> IDWriteTextFormat {
        self.format_in(&self.display_family, size, weight, align)
    }

    /// Digits and readouts.
    pub fn number(&self, size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT) -> IDWriteTextFormat {
        self.format_in(&self.number_family, size, weight, align)
    }

    /// Glyphs from the Windows icon font, centred in their box.
    pub fn icons(&self, size: f32) -> IDWriteTextFormat {
        self.format_in(&self.icon_family, size, NORMAL, CENTER)
    }

    /// Advance width of `s` in DIPs.
    pub fn measure(&self, s: &str, f: &IDWriteTextFormat) -> f32 {
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            self.dw.CreateTextLayout(&w, f, 10_000.0, 100.0).ok().and_then(|l| {
                let mut m = DWRITE_TEXT_METRICS::default();
                l.GetMetrics(&mut m).ok().map(|_| m.widthIncludingTrailingWhitespace)
            })
        }
        .unwrap_or(0.0)
    }

    fn format_in(
        &self,
        family: &[u16],
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        align: DWRITE_TEXT_ALIGNMENT,
    ) -> IDWriteTextFormat {
        let locale = wide("en-us");
        unsafe {
            let f = self
                .dw
                .CreateTextFormat(
                    PCWSTR(family.as_ptr()),
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    PCWSTR(locale.as_ptr()),
                )
                .expect("text format");
            f.SetTextAlignment(align).ok();
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER).ok();
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP).ok();
            if let Ok(sign) = self.dw.CreateEllipsisTrimmingSign(&f) {
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
                f.SetTrimming(&trim, &sign).ok();
            }
            f
        }
    }
}

fn family_exists(dw: &IDWriteFactory, name: &str) -> bool {
    let name = wide(name);
    unsafe {
        let mut coll = None;
        if dw.GetSystemFontCollection(&mut coll, false).is_err() {
            return false;
        }
        let Some(coll) = coll else { return false };
        let (mut index, mut exists) = (0u32, false.into());
        coll.FindFamilyName(PCWSTR(name.as_ptr()), &mut index, &mut exists).is_ok() && exists.as_bool()
    }
}

/// Slope at each point: level at a peak or trough, otherwise the harmonic
/// mean of its two sides, which keeps every segment rising or falling only.
fn monotone_tangents(pts: &[(f32, f32)]) -> Vec<f32> {
    let slope = |i: usize| (pts[i + 1].1 - pts[i].1) / (pts[i + 1].0 - pts[i].0);
    (0..pts.len())
        .map(|i| {
            if i == 0 || i == pts.len() - 1 {
                return 0.0;
            }
            let (a, b) = (slope(i - 1), slope(i));
            if a * b <= 0.0 { 0.0 } else { 2.0 * a * b / (a + b) }
        })
        .collect()
}

/// Drawing primitives over any render target (window or layered DC).
pub struct Canvas<'a> {
    pub rt: &'a ID2D1RenderTarget,
    pub d2d: &'a ID2D1Factory,
    brush: ID2D1SolidColorBrush,
    round: &'a ID2D1StrokeStyle,
}

impl<'a> Canvas<'a> {
    pub fn new(rt: &'a ID2D1RenderTarget, gfx: &'a Gfx) -> Result<Self> {
        let brush = unsafe { rt.CreateSolidColorBrush(&color(Rgba(0.0, 0.0, 0.0, 1.0)), None)? };
        unsafe { rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        Ok(Canvas { rt, d2d: &gfx.d2d, brush, round: &gfx.round })
    }

    fn paint(&self, c: Rgba) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&color(c)) };
        &self.brush
    }

    pub fn clear(&self, c: Rgba) {
        unsafe { self.rt.Clear(Some(&color(c))) };
    }

    pub fn fill(&self, r: R, radius: f32, c: Rgba) {
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt.FillRoundedRectangle(&rr, self.paint(c)) };
    }

    pub fn stroke(&self, r: R, radius: f32, c: Rgba, width: f32) {
        // Keep hairlines crisp by stroking on the half pixel inside the box.
        let h = width / 2.0;
        let rr = D2D1_ROUNDED_RECT { rect: R::new(r.x + h, r.y + h, r.w - width, r.h - width).d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt.DrawRoundedRectangle(&rr, self.paint(c), width, None) };
    }

    pub fn circle(&self, cx: f32, cy: f32, radius: f32, c: Rgba) {
        let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: radius, radiusY: radius };
        unsafe { self.rt.FillEllipse(&e, self.paint(c)) };
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: Rgba, width: f32) {
        unsafe { self.rt.DrawLine(Vector2 { X: x0, Y: y0 }, Vector2 { X: x1, Y: y1 }, self.paint(c), width, None) };
    }

    pub fn text(&self, s: &str, f: &IDWriteTextFormat, r: R, c: Rgba) {
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            self.rt.DrawText(&w, f, &r.d2d(), self.paint(c), D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL)
        };
    }

    /// Radial tick from `r0` to `r1`; `angle` in radians, 0 pointing right.
    pub fn tick(&self, (cx, cy): (f32, f32), angle: f32, (r0, r1): (f32, f32), c: Rgba, width: f32) {
        let (s, co) = angle.sin_cos();
        self.line(cx + co * r0, cy + s * r0, cx + co * r1, cy + s * r1, c, width);
    }

    fn stops(&self, stops: &[(f32, Rgba)]) -> Option<ID2D1GradientStopCollection> {
        let stops: Vec<D2D1_GRADIENT_STOP> =
            stops.iter().map(|&(position, c)| D2D1_GRADIENT_STOP { position, color: color(c) }).collect();
        unsafe { self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok() }
    }

    fn linear(&self, from: (f32, f32), to: (f32, f32), a: Rgba, b: Rgba) -> Option<ID2D1LinearGradientBrush> {
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: Vector2 { X: from.0, Y: from.1 },
            endPoint: Vector2 { X: to.0, Y: to.1 },
        };
        let brush = D2D1_BRUSH_PROPERTIES { opacity: 1.0, transform: Matrix3x2::identity() };
        unsafe { self.rt.CreateLinearGradientBrush(&props, Some(&brush), &self.stops(&[(0.0, a), (1.0, b)])?).ok() }
    }

    /// Rounded box shaded from `top` to `bottom`.
    pub fn fill_v(&self, r: R, radius: f32, top: Rgba, bottom: Rgba) {
        let Some(brush) = self.linear((r.x, r.y), (r.x, r.bottom()), top, bottom) else { return };
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt.FillRoundedRectangle(&rr, &brush) };
    }

    /// Soft light falling off to nothing at `radius`.
    pub fn glow(&self, cx: f32, cy: f32, radius: f32, c: Rgba) {
        let Some(stops) = self.stops(&[(0.0, c), (1.0, c.alpha(0.0))]) else { return };
        let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
            center: Vector2 { X: cx, Y: cy },
            gradientOriginOffset: Vector2 { X: 0.0, Y: 0.0 },
            radiusX: radius,
            radiusY: radius,
        };
        let brush = D2D1_BRUSH_PROPERTIES { opacity: 1.0, transform: Matrix3x2::identity() };
        let Ok(brush) = (unsafe { self.rt.CreateRadialGradientBrush(&props, Some(&brush), &stops) }) else { return };
        let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: radius, radiusY: radius };
        unsafe { self.rt.FillEllipse(&e, &brush) };
    }

    /// The mark with radius `size`, its cog turned by `turn` radians, shaded from `hi` to `lo`.
    pub fn logo(&self, logo: &Logo, (cx, cy): (f32, f32), size: f32, turn: f32, hi: Rgba, lo: Rgba) {
        let Some(brush) = self.linear((-1.0, -1.0), (1.0, 1.0), hi, lo) else { return };
        let place = |angle: f32| {
            let (s, c) = angle.sin_cos();
            Matrix3x2 { M11: c * size, M12: s * size, M21: -s * size, M22: c * size, M31: cx, M32: cy }
        };
        let mut saved = Matrix3x2::default();
        unsafe {
            self.rt.GetTransform(&mut saved);
            for (shape, angle) in [(&logo.cog, turn), (&logo.eye, 0.0)] {
                self.rt.SetTransform(&(place(angle) * saved));
                self.rt.FillGeometry(shape, &brush, None);
            }
            self.rt.SetTransform(&saved);
        }
    }

    /// Check mark drawn as a polyline inside `r`.
    pub fn check(&self, r: R, c: Rgba, width: f32) {
        self.polyline(
            &[(r.x + r.w * 0.22, r.y + r.h * 0.52), (r.x + r.w * 0.42, r.y + r.h * 0.72), (r.x + r.w * 0.78, r.y + r.h * 0.3)],
            c,
            width,
        );
    }

    pub fn polyline(&self, pts: &[(f32, f32)], c: Rgba, width: f32) {
        let Some(path) = self.path(pts) else { return };
        unsafe { self.rt.DrawGeometry(&path, self.paint(c), width, Some(self.round)) };
    }

    /// Area chart of `values` (0–100), newest at the right edge, pushed
    /// `shift` samples further right so a new sample can slide into view.
    pub fn area_chart(&self, r: R, values: &[f32], capacity: usize, shift: f32, line: Rgba, fill: Rgba) {
        if values.len() < 2 {
            return;
        }
        let step = r.w / (capacity.max(2) - 1) as f32;
        let x0 = r.right() + step * (shift - (values.len() - 1) as f32);
        let pts: Vec<(f32, f32)> = values
            .iter()
            .enumerate()
            .map(|(i, v)| (x0 + step * i as f32, r.bottom() - (v.clamp(0.0, 100.0) / 100.0) * r.h))
            .collect();
        if let (Some(path), Some(brush)) =
            (self.curve(&pts, Some(r.bottom())), self.linear((0.0, r.y), (0.0, r.bottom()), fill, fill.alpha(0.0)))
        {
            unsafe { self.rt.FillGeometry(&path, &brush, None) };
        }
        if let Some(path) = self.curve(&pts, None) {
            unsafe { self.rt.DrawGeometry(&path, self.paint(line), 2.0, Some(self.round)) };
        }
    }

    /// Smooth line through `pts` with monotone tangents, so it follows the
    /// samples without inventing peaks or wiggles between them. With `floor`
    /// it closes down to that y as a filled area.
    fn curve(&self, pts: &[(f32, f32)], floor: Option<f32>) -> Option<ID2D1PathGeometry> {
        let (&first, rest) = pts.split_first()?;
        let point = |(x, y): (f32, f32)| Vector2 { X: x, Y: y };
        unsafe {
            let path = self.d2d.CreatePathGeometry().ok()?;
            let sink = path.Open().ok()?;
            match floor {
                Some(y) => {
                    sink.BeginFigure(point((first.0, y)), D2D1_FIGURE_BEGIN_FILLED);
                    sink.AddLine(point(first));
                }
                None => sink.BeginFigure(point(first), D2D1_FIGURE_BEGIN_HOLLOW),
            }
            let tangents = monotone_tangents(pts);
            for (i, &p) in rest.iter().enumerate() {
                let prev = pts[i];
                let third = (p.0 - prev.0) / 3.0;
                sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                    point1: point((prev.0 + third, prev.1 + tangents[i] * third)),
                    point2: point((p.0 - third, p.1 - tangents[i + 1] * third)),
                    point3: point(p),
                });
            }
            if let Some(y) = floor {
                sink.AddLine(point((pts[pts.len() - 1].0, y)));
            }
            sink.EndFigure(if floor.is_some() { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
            sink.Close().ok()?;
            Some(path)
        }
    }

    fn path(&self, pts: &[(f32, f32)]) -> Option<ID2D1PathGeometry> {
        let (&(x, y), rest) = pts.split_first()?;
        unsafe {
            let path = self.d2d.CreatePathGeometry().ok()?;
            let sink = path.Open().ok()?;
            sink.BeginFigure(Vector2 { X: x, Y: y }, D2D1_FIGURE_BEGIN_HOLLOW);
            for &(x, y) in rest {
                sink.AddLine(Vector2 { X: x, Y: y });
            }
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close().ok()?;
            Some(path)
        }
    }

    pub fn clip<T>(&self, r: R, f: impl FnOnce() -> T) -> T {
        unsafe { self.rt.PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        let out = f();
        unsafe { self.rt.PopAxisAlignedClip() };
        out
    }
}
