//! Brass on deep petrol, in dark and light forms.

use crate::settings::ThemeMode;
use crate::win::apps_use_light_theme;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

impl Rgba {
    pub const fn hex(v: u32) -> Self {
        Rgba(((v >> 16) & 0xFF) as f32 / 255.0, ((v >> 8) & 0xFF) as f32 / 255.0, (v & 0xFF) as f32 / 255.0, 1.0)
    }

    pub const fn alpha(self, a: f32) -> Self {
        Rgba(self.0, self.1, self.2, a)
    }

    pub fn mix(self, o: Rgba, t: f32) -> Self {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Rgba(l(self.0, o.0), l(self.1, o.1), l(self.2, o.2), l(self.3, o.3))
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Theme {
    pub dark: bool,
    pub bg: Rgba,
    pub sidebar: Rgba,
    pub surface: Rgba,
    pub stroke: Rgba,
    pub text: Rgba,
    pub text2: Rgba,
    pub text3: Rgba,
    pub accent: Rgba,
    /// The light end of the brass gradient.
    pub accent_hi: Rgba,
    /// Text drawn on top of the accent colour.
    pub on_accent: Rgba,
    pub good: Rgba,
    pub warn: Rgba,
    pub crit: Rgba,
}

impl Theme {
    pub fn resolve(mode: ThemeMode) -> Self {
        let light = match mode {
            ThemeMode::System => apps_use_light_theme(),
            ThemeMode::Light => true,
            ThemeMode::Dark => false,
        };
        if light { Self::light() } else { Self::dark() }
    }

    /// Part way from this theme to `o`, for fading between them.
    pub fn mix(self, o: Theme, t: f32) -> Self {
        let m = |a: Rgba, b: Rgba| a.mix(b, t);
        Theme {
            dark: o.dark,
            bg: m(self.bg, o.bg),
            sidebar: m(self.sidebar, o.sidebar),
            surface: m(self.surface, o.surface),
            stroke: m(self.stroke, o.stroke),
            text: m(self.text, o.text),
            text2: m(self.text2, o.text2),
            text3: m(self.text3, o.text3),
            accent: m(self.accent, o.accent),
            accent_hi: m(self.accent_hi, o.accent_hi),
            on_accent: m(self.on_accent, o.on_accent),
            good: m(self.good, o.good),
            warn: m(self.warn, o.warn),
            crit: m(self.crit, o.crit),
        }
    }

    fn dark() -> Self {
        Theme {
            dark: true,
            bg: Rgba::hex(0x10191C),
            sidebar: Rgba::hex(0x0C1417),
            surface: Rgba::hex(0x1A282D),
            stroke: Rgba::hex(0x26383E),
            text: Rgba::hex(0xF2F1EC),
            text2: Rgba::hex(0x9DB0B3),
            text3: Rgba::hex(0x71868A),
            accent: Rgba::hex(0xE9B44C),
            accent_hi: Rgba::hex(0xFFD27A),
            on_accent: Rgba::hex(0x1F1603),
            good: Rgba::hex(0x4FD1A5),
            warn: Rgba::hex(0xFF8A3D),
            crit: Rgba::hex(0xFF5C5C),
        }
    }

    fn light() -> Self {
        Theme {
            dark: false,
            bg: Rgba::hex(0xF0F4F4),
            sidebar: Rgba::hex(0xE4EAEA),
            surface: Rgba::hex(0xDDE5E5),
            stroke: Rgba::hex(0xCFD9DA),
            text: Rgba::hex(0x11262B),
            text2: Rgba::hex(0x4A6166),
            text3: Rgba::hex(0x6B8085),
            accent: Rgba::hex(0x9A6503),
            accent_hi: Rgba::hex(0xC48A16),
            on_accent: Rgba::hex(0xFFFFFF),
            good: Rgba::hex(0x0B7F5E),
            warn: Rgba::hex(0xBF4F06),
            crit: Rgba::hex(0xC42B1C),
        }
    }

    /// Normal / warning / critical colour for a 0–100 load.
    pub fn load(&self, pct: f32) -> Rgba {
        if pct >= 95.0 { self.crit } else if pct >= 85.0 { self.warn } else { self.text }
    }

    /// Normal / warning / critical colour for a temperature in °C.
    pub fn temp(&self, c: f32, base: Rgba) -> Rgba {
        if c >= 95.0 { self.crit } else if c >= 85.0 { self.warn } else { base }
    }
}

#[cfg(test)]
mod tests {
    use super::{Theme, ThemeMode};

    #[test]
    fn explicit_modes_ignore_windows() {
        assert!(!Theme::resolve(ThemeMode::Light).dark);
        assert!(Theme::resolve(ThemeMode::Dark).dark);
    }
}
