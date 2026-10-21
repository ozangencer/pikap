//! Raster artwork: cover crops, the halo, the cover reflection and the glitch title.
//! Every image is rendered at the exact pixel size of its cell rectangle so the
//! terminal never stretches it.

use std::f32::consts::TAU;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Cursor;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage, imageops};

use crate::theme;

pub fn png(img: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    // Encoding an in-memory RGBA buffer cannot fail.
    let _ = img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png);
    out
}

/// Blurred, saturated cover stretched behind the whole screen. Tiny on purpose:
/// the terminal upscales it and the blur hides the pixels. Each pixel keeps its hue
/// but its luminance is clamped into `band` (see `Palette::halo_band`), so text stays
/// readable whatever colour the cover is.
pub fn halo(cover: &DynamicImage, band: (f32, f32), alpha: u8) -> RgbaImage {
    let mut small = cover
        .resize_exact(48, 30, imageops::FilterType::Triangle)
        .to_rgba8();
    small = imageops::blur(&small, 3.0);
    for p in small.pixels_mut() {
        let [r, g, b, _] = p.0.map(f32::from);
        let gray = (r + g + b) / 3.0;
        let saturated = [r, g, b].map(|v| (gray + (v - gray) * 1.8).clamp(0.0, 255.0) as u8);
        let clamped = clamp_luminance(saturated, band);
        p.0 = [clamped[0], clamped[1], clamped[2], alpha];
    }
    small
}

/// Scales towards black, or brightens (keeping saturation, then mixing in white), in
/// linear light until the relative luminance lies inside `band`.
pub fn clamp_luminance(rgb: [u8; 3], (min, max): (f32, f32)) -> [u8; 3] {
    let mut lin = theme::to_linear(rgb);
    let l = theme::linear_luminance(lin);
    if l > max {
        lin = lin.map(|c| c * max / l);
    } else if l < min {
        // Brighten keeping the hue and saturation first; only what that cannot reach
        // comes from mixing in white.
        let peak = lin.iter().copied().fold(0.0_f32, f32::max);
        if peak > 0.0 && l > 0.0 {
            let k = (min / l).min(1.0 / peak);
            lin = lin.map(|c| c * k);
        }
        let l = theme::linear_luminance(lin);
        if l < min {
            let t = (min - l) / (1.0 - l);
            lin = lin.map(|c| c + (1.0 - c) * t);
        }
    }
    lin.map(|c| {
        (theme::linear_to_channel(c) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8
    })
}

pub fn fit_cover(cover: &DynamicImage, w: u32, h: u32) -> RgbaImage {
    cover
        .resize_to_fill(w, h, imageops::FilterType::Lanczos3)
        .to_rgba8()
}

/// Mirrored bottom strip of the cover fading out over `h` pixels.
pub fn reflection(cover: &DynamicImage, w: u32, cover_h: u32, h: u32) -> RgbaImage {
    let full = cover
        .resize_to_fill(w, cover_h, imageops::FilterType::Triangle)
        .to_rgba8();
    let flipped = imageops::flip_vertical(&full);
    let mut strip = imageops::crop_imm(&flipped, 0, 0, w, h.min(cover_h)).to_image();
    strip = imageops::blur(&strip, 1.2);
    let rows = strip.height().max(1) as f32;
    for (_, y, p) in strip.enumerate_pixels_mut() {
        p[3] = (255.0 * 0.26 * (1.0 - y as f32 / rows)) as u8;
    }
    strip
}

/// JetBrains Mono ExtraBold (SIL Open Font License 1.1, `assets/OFL.txt`), for the
/// glitch titles; built in so they look the same on every machine.
pub const TITLE_FONT: &[u8] = include_bytes!("../assets/JetBrainsMono-ExtraBold.ttf");

/// Share of a record's diameter taken by its label.
pub const LABEL_SHARE: f32 = 0.36;

/// A black record seen from above, filling the smaller side of `w` x `h`: fine
/// grooves, a lighter run-out ring around the label, and a fixed sheen of two
/// opposite wedges. The label area is left dark; [`label`] frames sit on top.
pub fn vinyl(w: u32, h: u32, tint: [u8; 3]) -> RgbaImage {
    let mut img = RgbaImage::new(w, h);
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let outer = cx.min(cy) - 1.0;
    let label = outer * LABEL_SHARE;
    for (x, y, p) in img.enumerate_pixels_mut() {
        let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
        let r = dx.hypot(dy);
        let edge = (outer + 0.5 - r).clamp(0.0, 1.0);
        if edge <= 0.0 {
            continue;
        }
        let theta = dy.atan2(dx);
        let sheen = (theta - 0.7).cos().powi(2).powi(6) * 0.22;
        let groove = if r > label * 1.25 && r < outer - 3.0 {
            0.035 * (r * 1.7).sin() + 0.02 * (r * 0.23).sin()
        } else {
            0.0
        };
        let runout = if r > label && r < label * 1.25 {
            0.05
        } else {
            0.0
        };
        let base = 0.075 + groove + runout + sheen;
        let rgb = [0, 1, 2].map(|c| {
            let tinted = base * (0.85 + 0.15 * f32::from(tint[c]) / 255.0);
            (tinted.clamp(0.0, 1.0) * 255.0) as u8
        });
        *p = Rgba([rgb[0], rgb[1], rgb[2], (edge * 255.0) as u8]);
    }
    img
}

/// The record's label: the cover cut to a circle, turned by `angle` radians, with
/// the spindle hole, filling the smaller side of `w` x `h`.
pub fn label(cover: &DynamicImage, w: u32, h: u32, angle: f32) -> RgbaImage {
    let d = w.min(h).max(2);
    let art = cover
        .resize_to_fill(d, d, imageops::FilterType::Triangle)
        .to_rgba8();
    let mut img = RgbaImage::new(w, h);
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let radius = d as f32 / 2.0 - 0.5;
    let hole = radius * 0.07;
    let (sin, cos) = angle.sin_cos();
    for (x, y, p) in img.enumerate_pixels_mut() {
        let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
        let r = dx.hypot(dy);
        let alpha = (radius + 0.5 - r).clamp(0.0, 1.0) * (r - hole).clamp(0.0, 1.0);
        if alpha <= 0.0 {
            continue;
        }
        // Rotate back into the cover's own frame.
        let sx = (dx * cos + dy * sin + d as f32 / 2.0).clamp(0.0, d as f32 - 1.0);
        let sy = (-dx * sin + dy * cos + d as f32 / 2.0).clamp(0.0, d as f32 - 1.0);
        let src = art.get_pixel(sx as u32, sy as u32);
        // A faint ring near the edge, like a printed label.
        let ring = if (r - radius * 0.9).abs() < 1.0 {
            0.85
        } else {
            1.0
        };
        *p = Rgba([
            (f32::from(src[0]) * ring) as u8,
            (f32::from(src[1]) * ring) as u8,
            (f32::from(src[2]) * ring) as u8,
            (alpha * 255.0) as u8,
        ]);
    }
    img
}

/// `text` in `colors[0]` with a channel split in `colors[1]` and `colors[2]`, fitted into a
/// transparent `w` x `h` canvas.
pub fn glitch_title(
    font_bytes: &[u8],
    text: &str,
    colors: [[u8; 3]; 3],
    w: u32,
    h: u32,
) -> Option<RgbaImage> {
    let font = FontRef::try_from_slice(font_bytes).ok()?;
    let measure = |px: f32| {
        let sf = font.as_scaled(PxScale::from(px));
        text.chars()
            .map(|c| sf.h_advance(sf.glyph_id(c)) + px * 0.08)
            .sum::<f32>()
    };
    let mut px = h as f32 * 0.8;
    let split = (px / 16.0).max(2.0);
    let room = w as f32 - split * 2.0 - 4.0;
    let natural = measure(px);
    if natural > room {
        px *= room / natural;
    }

    let scale = PxScale::from(px);
    let sf = font.as_scaled(scale);
    let top = ((h as f32 - (sf.ascent() - sf.descent())) / 2.0).max(0.0);
    let mut x = split + 2.0;
    let mut glyphs = Vec::new();
    for c in text.chars() {
        let id = sf.glyph_id(c);
        // Glyphs the font lacks would draw as boxes; leave them out.
        if id.0 == 0 && !c.is_whitespace() {
            continue;
        }
        glyphs.push(id.with_scale_and_position(scale, point(x, top + sf.ascent())));
        x += sf.h_advance(id) + px * 0.08;
    }

    let mut img = RgbaImage::new(w, h);
    let split = split as i32;
    let [core, left, right] = colors;
    for (dx, rgb) in [(-split, left), (split, right), (0, core)] {
        for g in &glyphs {
            let Some(og) = font.outline_glyph(g.clone()) else {
                continue;
            };
            let b = og.px_bounds();
            og.draw(|gx, gy, cov| {
                let px = b.min.x as i32 + gx as i32 + dx;
                let py = b.min.y as i32 + gy as i32;
                if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                    blend(img.get_pixel_mut(px as u32, py as u32), rgb, cov);
                }
            });
        }
    }
    Some(img)
}

fn blend(dst: &mut Rgba<u8>, rgb: [u8; 3], cov: f32) {
    let sa = cov.clamp(0.0, 1.0);
    let da = f32::from(dst[3]) / 255.0;
    let oa = sa + da * (1.0 - sa);
    if oa <= 0.0 {
        return;
    }
    for c in 0..3 {
        let mixed = f32::from(rgb[c]) * sa + f32::from(dst[c]) * da * (1.0 - sa);
        dst[c] = (mixed / oa).round() as u8;
    }
    dst[3] = (oa * 255.0).round() as u8;
}

/// Stable 64-bit seed for a track URI.
pub fn seed(uri: &str) -> u64 {
    let mut h = DefaultHasher::new();
    uri.hash(&mut h);
    h.finish()
}

/// The track's waveform silhouette, `n` levels in `0..=1`, stable per seed.
pub fn wave_levels(seed: u64, n: usize) -> Vec<f32> {
    let phase = |shift: u32| f32::from(((seed >> shift) & 0xff) as u8) / 40.0;
    let (a, b, c) = (phase(0), phase(8), phase(16));
    (0..n)
        .map(|i| {
            let i = i as f32 * 0.5;
            ((i * 0.55 + a).sin() * 0.6 + (i * 1.7 + b).sin() * 0.3 + (i * 0.13 + c).sin() * 0.25)
                .abs()
                .min(1.0)
        })
        .collect()
}

/// One equalizer bar at animation time `t`: each bar sways at its own pace with a
/// light beat on top. Decorative: pikap is a remote and never hears the audio, and
/// Spotify offers no tempo or analysis data to new apps.
pub fn eq_level(seed: u64, bar: usize, t: f32) -> f32 {
    let h = splitmix(seed ^ (bar as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let unit = |shift: u32| f32::from(((h >> shift) & 0xffff) as u16) / 65_535.0;
    let (f1, f2) = (1.1 + unit(0) * 2.4, 2.7 + unit(16) * 3.1);
    let (p1, p2) = (unit(32) * TAU, unit(48) * TAU);
    let sway = 0.55 + 0.30 * (t * f1 + p1).sin() + 0.15 * (t * f2 + p2).sin();
    let beat = (t * TAU * 2.0).sin().max(0.0).powi(6) * 0.25;
    (sway + beat).clamp(0.0, 1.0)
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The cover's dominant colour: pixels are grouped by hue, weighted by how saturated
/// and bright they are, and the heaviest group (with its neighbours) is averaged.
/// Pastel results are pushed to a minimum saturation so they still read as a colour.
/// `None` for grey covers.
pub fn album_color(cover: &DynamicImage) -> Option<[u8; 3]> {
    const BUCKETS: usize = 24;
    let small = cover
        .resize_exact(32, 32, imageops::FilterType::Triangle)
        .to_rgb8();
    let mut weight = [0.0_f32; BUCKETS];
    let mut sums = [[0.0_f32; 3]; BUCKETS];
    for p in small.pixels() {
        let (hue, sat, val) = hsv(p.0);
        if sat < 0.12 || val < 0.2 {
            continue;
        }
        let w = sat * val;
        let b = (hue / (360.0 / BUCKETS as f32)) as usize % BUCKETS;
        weight[b] += w;
        for (sum, channel) in sums[b].iter_mut().zip(p.0) {
            *sum += f32::from(channel) * w;
        }
    }
    let total = |b: usize| {
        (0..3)
            .map(|d| weight[(b + BUCKETS + d - 1) % BUCKETS])
            .sum::<f32>()
    };
    let best = (0..BUCKETS).max_by(|a, b| total(*a).total_cmp(&total(*b)))?;
    // Under ~3 % of the cover in any colour family means a grey cover.
    if total(best) < 0.03 * (32.0 * 32.0) * 0.5 {
        return None;
    }
    let (mut w, mut rgb) = (0.0, [0.0_f32; 3]);
    for d in 0..3 {
        let b = (best + BUCKETS + d - 1) % BUCKETS;
        w += weight[b];
        for (total, sum) in rgb.iter_mut().zip(sums[b]) {
            *total += sum;
        }
    }
    let mean = rgb.map(|c| (c / w).round().clamp(0.0, 255.0) as u8);
    let (hue, sat, val) = hsv(mean);
    Some(hsv_to_rgb(hue, sat.max(0.45), val.max(0.6)))
}

fn hsv_to_rgb(hue: f32, sat: f32, val: f32) -> [u8; 3] {
    let c = val * sat;
    let x = c * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match (hue / 60.0) as u32 % 6 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = val - c;
    [r, g, b].map(|v| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8)
}

fn hsv([r, g, b]: [u8; 3]) -> (f32, f32, f32) {
    let [r, g, b] = [r, g, b].map(|c| f32::from(c) / 255.0);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let hue = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let sat = if max == 0.0 { 0.0 } else { d / max };
    (hue, sat, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{DENIZ_DARK, DENIZ_LIGHT, contrast, linear_luminance, luminance, to_linear};

    #[test]
    fn clamped_halo_pixels_keep_text_contrast() {
        for palette in [&DENIZ_DARK, &DENIZ_LIGHT] {
            let band = palette.halo_band();
            for r in (0..=255).step_by(17) {
                for g in (0..=255).step_by(17) {
                    for b in (0..=255).step_by(17) {
                        let out = clamp_luminance([r as u8, g as u8, b as u8], band);
                        let l = linear_luminance(to_linear(out));
                        assert!(
                            contrast(luminance(palette.muted), l) >= 4.4,
                            "{}: {out:?}",
                            palette.name
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn eq_levels_stay_in_range() {
        for bar in 0..64 {
            for step in 0..200 {
                let v = eq_level(seed("spotify:track:x"), bar, step as f32 * 0.05);
                assert!((0.0..=1.0).contains(&v));
            }
        }
    }

    #[test]
    fn album_color_finds_the_dominant_hue() {
        let mut img = RgbaImage::from_pixel(32, 32, Rgba([235, 190, 210, 255]));
        for x in 0..32 {
            for y in 0..4 {
                img.put_pixel(x, y, Rgba([40, 200, 60, 255]));
            }
        }
        let [r, g, b] = album_color(&DynamicImage::ImageRgba8(img)).unwrap();
        assert!(
            r > g && b > g,
            "a pastel pink cover reads as pink, got {r},{g},{b}"
        );

        let grey = RgbaImage::from_pixel(32, 32, Rgba([120, 120, 120, 255]));
        assert!(album_color(&DynamicImage::ImageRgba8(grey)).is_none());
    }
}

#[cfg(test)]
mod font_tests {
    use ab_glyph::{Font, FontRef};

    /// The stage draws rests with "•"; the title font must have it (and lacks "♪").
    #[test]
    fn title_font_has_the_rest_dots() {
        let font = FontRef::try_from_slice(super::TITLE_FONT).unwrap();
        assert_ne!(font.glyph_id('•').0, 0);
        assert_eq!(font.glyph_id('♪').0, 0, "why the note showed as a box");
    }
}
