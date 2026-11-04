//! The dusty mark as vector geometry: a cog whose bore holds an eye of two
//! interlocking arcs. The cog can turn while the eye holds still. Same shape
//! as res/make_icon.py, which draws the icon files.

use std::f32::consts::{PI, TAU};

use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_FILL_MODE, D2D1_FILL_MODE_ALTERNATE, D2D1_FILL_MODE_WINDING,
};
use windows::Win32::Graphics::Direct2D::{ID2D1Factory, ID2D1PathGeometry};
use windows::core::Result;
use windows_numerics::Vector2;

const TEETH: u32 = 24;
/// One tooth's turn, in degrees: the cog looks the same at every multiple.
pub const TOOTH_DEG: f32 = 360.0 / TEETH as f32;
const ROOT: f32 = 0.82;
const BORE: f32 = 0.62;
const LID_WIDTH: f32 = 0.135;
const TILT: f32 = -32.0 * PI / 180.0;

type Shape = Vec<(f32, f32)>;

pub struct Logo {
    pub cog: ID2D1PathGeometry,
    pub eye: ID2D1PathGeometry,
}

impl Logo {
    pub fn new(d2d: &ID2D1Factory) -> Result<Self> {
        Ok(Logo {
            // Alternate fill turns the bore into a hole; winding keeps the
            // place where the lids cross solid.
            cog: geometry(d2d, &[cog(), circle(BORE)], D2D1_FILL_MODE_ALTERNATE)?,
            eye: geometry(d2d, &[lid(1.0), lid(-1.0)], D2D1_FILL_MODE_WINDING)?,
        })
    }
}

fn cog() -> Shape {
    let pitch = TAU / TEETH as f32;
    (0..TEETH)
        .flat_map(|i| {
            [(-0.31, ROOT), (-0.17, 1.0), (0.17, 1.0), (0.31, ROOT)].map(|(da, r)| {
                let a = (i as f32 + da) * pitch;
                (r * a.cos(), r * a.sin())
            })
        })
        .collect()
}

fn circle(r: f32) -> Shape {
    (0..96).map(|i| (i as f32 / 96.0 * TAU).sin_cos()).map(|(s, c)| (r * c, r * s)).collect()
}

/// One eyelid: a tapered band whose ends run into the cog's ring.
fn lid(side: f32) -> Shape {
    let (half_x, half_h) = (0.74 * BORE, 0.4 * BORE);
    let radius = (half_x * half_x + half_h * half_h) / (2.0 * half_h);
    let d = radius - half_h;
    let reach = BORE + 0.04;
    let span = ((radius * radius + d * d - reach * reach) / (2.0 * d * radius)).acos();
    let (ts, tc) = TILT.sin_cos();
    let edge = |offset: f32, t: f32| {
        let (s, c) = (t * span).sin_cos();
        let r = radius + offset * LID_WIDTH * (1.0 - 0.4 * t * t) / 2.0;
        let (x, y) = (r * s, side * (r * c - d));
        (x * tc - y * ts, x * ts + y * tc)
    };
    let along = |i: i32| i as f32 / 24.0 - 1.0;
    let outer = (0..=48).map(|i| edge(1.0, along(i)));
    let inner = (0..=48).rev().map(|i| edge(-1.0, along(i)));
    let shape: Shape = outer.chain(inner).collect();
    // Both lids wind the same way, or their crossing would cancel out.
    if side < 0.0 { shape.into_iter().rev().collect() } else { shape }
}

fn geometry(d2d: &ID2D1Factory, shapes: &[Shape], fill: D2D1_FILL_MODE) -> Result<ID2D1PathGeometry> {
    unsafe {
        let path = d2d.CreatePathGeometry()?;
        let sink = path.Open()?;
        sink.SetFillMode(fill);
        for shape in shapes {
            let (&(x, y), rest) = shape.split_first().expect("shape has points");
            sink.BeginFigure(Vector2 { X: x, Y: y }, D2D1_FIGURE_BEGIN_FILLED);
            for &(x, y) in rest {
                sink.AddLine(Vector2 { X: x, Y: y });
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        }
        sink.Close()?;
        Ok(path)
    }
}
