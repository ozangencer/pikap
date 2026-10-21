//! "Deniz" palettes from the Dalga design, one per terminal background. Cells that
//! should show the halo keep the default background; only opaque surfaces
//! (selection, panels, tags) set one.

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Dark,
    Light,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "dark" | "gece" => Some(Self::Dark),
            "light" | "gunduz" | "gündüz" => Some(Self::Light),
            _ => None,
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }

    pub fn palette(self) -> &'static Palette {
        match self {
            Self::Dark => &DENIZ_DARK,
            Self::Light => &DENIZ_LIGHT,
        }
    }

    /// Asks the terminal for its background (OSC 11) and falls back to the macOS
    /// appearance. Must run in raw mode, before the event reader starts.
    pub fn detect() -> Self {
        if let Some(luma) = query_background_luma() {
            return if luma > 0.5 { Self::Light } else { Self::Dark };
        }
        let dark = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "Dark");
        if dark { Self::Dark } else { Self::Light }
    }
}

pub struct Palette {
    pub name: &'static str,
    pub accent: Color,
    pub text: Color,
    pub bright: Color,
    pub muted: Color,
    pub line: Color,
    pub like: Color,
    pub ok: Color,
    /// Foreground on accent-filled tags.
    pub on_accent: Color,
    /// Filled surfaces as RGBA. They are drawn as kitty images between the halo and the
    /// text, never as cell backgrounds, which some terminals paint over the halo.
    pub selected: [u8; 4],
    pub panel: [u8; 4],
    pub tag: [u8; 4],
    /// Glitch title core and its two split channels, as RGB.
    pub title_rgb: [[u8; 3]; 3],
}

pub static DENIZ_DARK: Palette = Palette {
    name: "deniz · gece",
    accent: Color::Rgb(63, 208, 201),
    text: Color::Rgb(227, 242, 243),
    bright: Color::Rgb(244, 251, 251),
    muted: Color::Rgb(163, 188, 192),
    line: Color::Rgb(85, 127, 134),
    like: Color::Rgb(255, 111, 154),
    ok: Color::Rgb(111, 220, 140),
    on_accent: Color::Rgb(10, 20, 24),
    selected: [63, 208, 201, 42],
    panel: [8, 17, 20, 200],
    tag: [63, 208, 201, 255],
    title_rgb: [[244, 251, 251], [255, 111, 154], [63, 208, 201]],
};

pub static DENIZ_LIGHT: Palette = Palette {
    name: "deniz · gündüz",
    accent: Color::Rgb(8, 112, 107),
    text: Color::Rgb(16, 41, 44),
    bright: Color::Rgb(8, 28, 31),
    muted: Color::Rgb(71, 95, 99),
    line: Color::Rgb(138, 163, 166),
    like: Color::Rgb(214, 51, 108),
    ok: Color::Rgb(36, 140, 82),
    on_accent: Color::Rgb(255, 255, 255),
    selected: [8, 112, 107, 40],
    panel: [255, 255, 255, 200],
    tag: [8, 112, 107, 255],
    title_rgb: [[12, 34, 37], [230, 73, 128], [14, 160, 153]],
};

impl Palette {
    /// Luminance band the halo must stay inside so that every colour drawn on it keeps
    /// its contrast: 4.5:1 for text, 3:1 for icons, 2:1 for rules and the waveform.
    /// The cover can be any colour, so the halo is clamped to this band instead of the
    /// palette guessing.
    pub fn halo_band(&self) -> (f32, f32) {
        let needs = [
            (self.text, 4.5),
            (self.bright, 4.5),
            (self.muted, 4.5),
            (self.accent, 4.5),
            (self.like, 3.0),
            (self.ok, 3.0),
            (self.line, 2.0),
        ];
        let dark_ui = luminance(self.text) > 0.5;
        if dark_ui {
            let max = needs
                .iter()
                .map(|&(c, ratio)| (luminance(c) + 0.05) / ratio - 0.05)
                .fold(1.0_f32, f32::min);
            // The terminal blends neighbouring halo pixels in sRGB, which can only darken
            // the mix, so the upper bound needs no margin.
            (0.0, max)
        } else {
            let min = needs
                .iter()
                .map(|&(c, ratio)| ratio * (luminance(c) + 0.05) - 0.05)
                .fold(0.0_f32, f32::max);
            (min + 0.02, 1.0)
        }
    }

    /// `rgb` lightened (dark theme) or darkened (light theme) just enough to keep 3:1
    /// against the brightest (or darkest) the halo can get.
    pub fn readable(&self, rgb: [u8; 3]) -> Color {
        let (min, max) = self.halo_band();
        let band = if luminance(self.text) > 0.5 {
            ((3.0 * (max + 0.05) - 0.05).min(1.0), 1.0)
        } else {
            (0.0, ((min + 0.05) / 3.0 - 0.05).max(0.0))
        };
        let [r, g, b] = crate::art::clamp_luminance(rgb, band);
        Color::Rgb(r, g, b)
    }

    pub fn text(&self) -> Style {
        Style::new().fg(self.text)
    }

    pub fn muted(&self) -> Style {
        Style::new().fg(self.muted)
    }

    pub fn accent(&self) -> Style {
        Style::new().fg(self.accent)
    }

    pub fn line(&self) -> Style {
        Style::new().fg(self.line)
    }

    pub fn key(&self) -> Style {
        Style::new().fg(self.accent).add_modifier(Modifier::BOLD)
    }

    /// Text on an accent-filled tag such as `g …`; the fill itself is a `tag` surface.
    pub fn tag(&self) -> Style {
        Style::new().fg(self.on_accent).add_modifier(Modifier::BOLD)
    }
}

fn channel_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_channel(l: f32) -> f32 {
    if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB bytes to linear channels.
pub fn to_linear(rgb: [u8; 3]) -> [f32; 3] {
    rgb.map(|c| channel_to_linear(f32::from(c) / 255.0))
}

pub fn linear_luminance(l: [f32; 3]) -> f32 {
    0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2]
}

/// WCAG relative luminance of a palette colour.
pub fn luminance(color: Color) -> f32 {
    match color {
        Color::Rgb(r, g, b) => linear_luminance(to_linear([r, g, b])),
        _ => 0.5,
    }
}

#[cfg(test)]
pub fn contrast(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Relative luminance of the terminal background from an OSC 11 reply, if the terminal
/// answers within half a second. A DA1 request follows so silent terminals still reply.
fn query_background_luma() -> Option<f32> {
    parse_osc11(&String::from_utf8_lossy(&crate::term::ask(
        b"\x1b]11;?\x1b\\",
    )))
}

fn parse_osc11(reply: &str) -> Option<f32> {
    let rgb = reply.split("rgb:").nth(1)?;
    let mut channels = rgb.split(['/', '\x1b', '\x07']).take(3).map(|h| {
        let v = u32::from_str_radix(h, 16).ok()?;
        let max = (1u32 << (4 * h.len() as u32)) - 1;
        Some(v as f32 / max as f32)
    });
    let (r, g, b) = (channels.next()??, channels.next()??, channels.next()??);
    Some(0.2126 * r + 0.7152 * g + 0.0722 * b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halo_band_keeps_every_colour_readable() {
        for palette in [&DENIZ_DARK, &DENIZ_LIGHT] {
            let (min, max) = palette.halo_band();
            assert!(min < max, "{}: empty band", palette.name);
            for edge in [min, max] {
                for (c, ratio) in [
                    (palette.muted, 4.5),
                    (palette.accent, 4.5),
                    (palette.line, 2.0),
                ] {
                    assert!(
                        contrast(luminance(c), edge) >= ratio - 0.01,
                        "{}: {c:?} at halo luminance {edge}",
                        palette.name
                    );
                }
            }
        }
    }

    #[test]
    fn osc11() {
        let light = parse_osc11("\x1b]11;rgb:f8f8/f8f8/f8f8\x1b\\").unwrap();
        let dark = parse_osc11("\x1b]11;rgb:1a/1b/26\x07").unwrap();
        assert!(light > 0.9);
        assert!(dark < 0.2);
        assert!(parse_osc11("\x1b[?62;22c").is_none());
    }
}
