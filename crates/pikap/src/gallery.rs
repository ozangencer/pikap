//! Turns the cell rectangles the UI reserved into kitty graphics, re-rendering an
//! image only when its content or pixel size changes.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::{DynamicImage, Rgba, RgbaImage};
use ratatui::layout::Rect;

use crate::art;
use crate::kitty::{Graphic, Z_ART};
use crate::theme::Palette;

const HALO: u32 = 1;
const COVER: u32 = 2;
const REFLECTION: u32 = 3;
const TITLE: u32 = 4;
const SOLID_BASE: u32 = 100;
const VINYL: u32 = 5;
const LYRIC: u32 = 6;
const LABEL_BASE: u32 = 200;
/// Turns of the record's label, sent once and then only placed in turn.
pub const LABEL_FRAMES: usize = 16;
/// Longest side, in pixels, of the stage pieces; Muxy scales them up to their cells.
/// Keeps the stage within Muxy's 8 MB of decoded images.
const VINYL_PX: u32 = 512;
const LABEL_PX: u32 = 192;
const SLEEVE_PX: u32 = 640;
/// Longest side of the home cover: Spotify's covers are 640 px, so more only costs
/// memory. Muxy keeps 8 MB of decoded images and drops the oldest past that (the halo
/// and the surfaces, which pikap would then never send again), so every image is
/// kept small and Muxy scales it to its cells.
const COVER_PX: u32 = 640;
const TITLE_PX: u32 = 1280;
const CARD_PX: u32 = 160;
/// Release covers take ids from here; a page shows a few dozen at most.
const CARD_BASE: u32 = 1000;
/// Rendered release covers kept in memory.
const CARD_CACHE: usize = 96;
/// Side of the solid-colour images behind surfaces. Muxy scales images through a
/// texture atlas with linear filtering, so a stretched image blends with its
/// neighbours along its edges (a 1×1 pixel became a glowing line instead of a filled
/// row). Placements show only the inner half (`SOLID_CROP`), so the blended rim
/// falls outside the cell rectangle.
const SOLID_PX: u32 = 64;
const SOLID_CROP: [u32; 4] = [SOLID_PX / 4, SOLID_PX / 4, SOLID_PX / 2, SOLID_PX / 2];
/// Halo opacity on content pages: the Dalga rule of 40% there against 60% at home.
const HALO_DIM: u8 = 170;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CellPx {
    pub w: u16,
    pub h: u16,
}

impl CellPx {
    /// Muxy does not answer `CSI 16 t`, but `TIOCGWINSZ` carries the pixel size.
    pub fn detect() -> Self {
        match ratatui::crossterm::terminal::window_size() {
            Ok(ws) if ws.width > 0 && ws.columns > 0 && ws.rows > 0 => Self {
                w: ws.width / ws.columns,
                h: ws.height / ws.rows,
            },
            _ => Self { w: 8, h: 16 },
        }
    }

    pub fn ratio(self) -> f32 {
        f32::from(self.h) / f32::from(self.w.max(1))
    }

    fn px(self, r: Rect) -> (u32, u32) {
        (
            u32::from(r.width) * u32::from(self.w),
            u32::from(r.height) * u32::from(self.h),
        )
    }
}

/// Cell rectangles the UI left for artwork, plus the filled surfaces it wants
/// under its text. Empty rectangles mean "not on this layout".
#[derive(Default)]
pub struct Slots {
    pub screen: Rect,
    pub cover: Rect,
    pub reflection: Rect,
    pub title: Rect,
    pub surfaces: Vec<(Rect, [u8; 4])>,
    /// Content pages (library, search) keep the halo fainter behind their lists.
    pub halo_dim: bool,
    /// Release covers on an artist page: cell rectangle and image file.
    pub cards: Vec<(Rect, PathBuf)>,
    /// Cell height over width, to keep covers square.
    pub cell_ratio: f32,
    /// Stage mode: the cover becomes a sleeve laid over the record.
    pub stage: bool,
    pub vinyl: Rect,
    /// The record's label, centred on `vinyl` to the cell.
    pub label: Rect,
    /// Which turn of the label shows (`LABEL_FRAMES` of them).
    pub label_frame: usize,
    /// The current lyric, drawn large.
    pub lyric: Option<(Rect, String)>,
}

impl Slots {
    pub fn fill(&mut self, rect: Rect, rgba: [u8; 4]) {
        if rect.width > 0 && rect.height > 0 {
            self.surfaces.push((rect, rgba));
        }
    }
}

pub struct Gallery {
    cover: Option<(DynamicImage, PathBuf)>,
    wanted: Option<PathBuf>,
    color: Option<[u8; 3]>,
    font: Option<Vec<u8>>,
    cache: HashMap<u32, (u64, Arc<Vec<u8>>)>,
    solids: HashMap<[u8; 4], (u32, Arc<Vec<u8>>)>,
    /// Release covers fitted to their cell size, by (file, size) key.
    cards: HashMap<u64, Arc<Vec<u8>>>,
    /// The label's turns for one cover and size.
    labels: Option<(u64, Vec<Arc<Vec<u8>>>)>,
}

impl Gallery {
    pub fn new(cover: Option<&Path>) -> Self {
        let cover = cover.and_then(|p| Some((image::open(p).ok()?, p.to_path_buf())));
        let font = Some(art::TITLE_FONT.to_vec());
        Self {
            wanted: cover.as_ref().map(|(_, p)| p.clone()),
            color: None,
            cover,
            font,
            cache: HashMap::new(),
            solids: HashMap::new(),
            cards: HashMap::new(),
            labels: None,
        }
    }

    /// Switches the artwork when the playing track's cover changes. A path that fails to
    /// load is remembered, so a broken file is not reopened every frame.
    /// Returns true when the cover actually changed.
    pub fn set_cover(&mut self, path: Option<&Path>) -> bool {
        if self.wanted.as_deref() == path {
            return false;
        }
        self.wanted = path.map(Path::to_path_buf);
        self.cover = path.and_then(|p| Some((image::open(p).ok()?, p.to_path_buf())));
        self.color = self
            .cover
            .as_ref()
            .and_then(|(img, _)| art::album_color(img));
        true
    }

    /// The cover's dominant colour, for the equalizer and the current lyric.
    pub fn cover_color(&self) -> Option<[u8; 3]> {
        self.color
    }

    pub fn has_title_font(&self) -> bool {
        self.font.is_some()
    }

    pub fn graphics(
        &mut self,
        slots: &Slots,
        cell: CellPx,
        title: &str,
        palette: &Palette,
    ) -> Vec<Graphic> {
        let mut out = Vec::new();
        if let Some((cover, path)) = &self.cover {
            let full = cell.px(slots.cover);
            let (cw, ch) = capped(full, if slots.stage { SLEEVE_PX } else { COVER_PX });
            // The reflection shrinks by the same factor as the cover above it.
            let rh = cell.px(slots.reflection).1 * ch / full.1.max(1);
            let band = palette.halo_band();
            let alpha = if slots.halo_dim { HALO_DIM } else { 255 };
            out.push(cached(
                &mut self.cache,
                HALO,
                slots.screen,
                crate::kitty::layers().0,
                key(&(path, "halo", band.0.to_bits(), band.1.to_bits(), alpha)),
                || Some(art::halo(cover, band, alpha)),
            ));
            out.push(cached(
                &mut self.cache,
                COVER,
                slots.cover,
                if slots.stage { Z_ART + 2 } else { Z_ART },
                key(&(path, cw, ch)),
                || Some(art::fit_cover(cover, cw, ch)),
            ));
            out.push(cached(
                &mut self.cache,
                REFLECTION,
                slots.reflection,
                Z_ART,
                key(&(path, cw, ch, rh)),
                || Some(art::reflection(cover, cw, ch, rh)),
            ));
        }
        if let Some((cover, path)) = self.cover.as_ref().filter(|_| !slots.vinyl.is_empty()) {
            let (vw, vh) = capped(cell.px(slots.vinyl), VINYL_PX);
            let tint = self.color.unwrap_or([255, 255, 255]);
            out.push(cached(
                &mut self.cache,
                VINYL,
                slots.vinyl,
                Z_ART,
                key(&(vw, vh, tint)),
                || Some(art::vinyl(vw, vh, tint)),
            ));
            let (lw, lh) = capped(cell.px(slots.label), LABEL_PX);
            let labels_key = key(&(path, lw, lh));
            if self.labels.as_ref().is_none_or(|(k, _)| *k != labels_key) {
                let frames = (0..LABEL_FRAMES)
                    .map(|i| {
                        let angle = std::f32::consts::TAU * i as f32 / LABEL_FRAMES as f32;
                        Arc::new(art::png(&art::label(cover, lw, lh, angle)))
                    })
                    .collect();
                self.labels = Some((labels_key, frames));
            }
            if let Some((_, frames)) = &self.labels {
                for (i, png) in frames.iter().enumerate() {
                    out.push(Graphic {
                        id: LABEL_BASE + i as u32,
                        placement: 1,
                        rect: slots.label,
                        z: Z_ART + 1,
                        key: key(&(labels_key, i)),
                        png: png.clone(),
                        crop: None,
                        visible: i == slots.label_frame % LABEL_FRAMES,
                    });
                }
            }
        }
        if let (Some(font), Some((rect, text))) = (&self.font, &slots.lyric) {
            let (lw, lh) = cell.px(*rect);
            out.push(cached(
                &mut self.cache,
                LYRIC,
                *rect,
                Z_ART,
                key(&(text, lw, lh, palette.title_rgb)),
                || art::glitch_title(font, text, palette.title_rgb, lw, lh),
            ));
        }
        if let Some(font) = &self.font {
            let (tw, th) = capped(cell.px(slots.title), TITLE_PX);
            out.push(cached(
                &mut self.cache,
                TITLE,
                slots.title,
                Z_ART,
                key(&(title, tw, th, palette.title_rgb)),
                || art::glitch_title(font, title, palette.title_rgb, tw, th),
            ));
        }
        for (i, (rect, path)) in slots.cards.iter().enumerate() {
            let (w, h) = capped(cell.px(*rect), CARD_PX);
            let card_key = key(&(path, w, h));
            if !self.cards.contains_key(&card_key) {
                if self.cards.len() >= CARD_CACHE {
                    self.cards.clear();
                }
                let png = image::open(path)
                    .map(|img| art::png(&art::fit_cover(&img, w, h)))
                    .unwrap_or_default();
                self.cards.insert(card_key, Arc::new(png));
            }
            out.push(Graphic {
                id: CARD_BASE + i as u32,
                placement: 1,
                rect: *rect,
                z: Z_ART,
                key: card_key,
                png: self.cards[&card_key].clone(),
                crop: None,
                visible: true,
            });
        }
        out.retain(|g| !g.png.is_empty());

        let mut used: HashMap<u32, u32> = HashMap::new();
        for (layer, &(rect, rgba)) in slots.surfaces.iter().enumerate() {
            let next_id = SOLID_BASE + self.solids.len() as u32;
            let (id, png) = self
                .solids
                .entry(rgba)
                .or_insert_with(|| {
                    (
                        next_id,
                        Arc::new(art::png(&RgbaImage::from_pixel(
                            SOLID_PX,
                            SOLID_PX,
                            Rgba(rgba),
                        ))),
                    )
                })
                .clone();
            let placement = used.entry(id).or_default();
            *placement += 1;
            out.push(Graphic {
                id,
                placement: *placement,
                rect,
                z: crate::kitty::layers().1 + layer as i32,
                key: key(&rgba),
                png,
                crop: Some(SOLID_CROP),
                visible: true,
            });
        }
        out
    }
}

/// Scales a pixel size down so its longer side is at most `max`.
fn capped((w, h): (u32, u32), max: u32) -> (u32, u32) {
    let long = w.max(h).max(1);
    if long <= max {
        return (w, h);
    }
    ((w * max / long).max(1), (h * max / long).max(1))
}

fn key(v: &impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

fn cached(
    cache: &mut HashMap<u32, (u64, Arc<Vec<u8>>)>,
    id: u32,
    rect: Rect,
    z: i32,
    key: u64,
    render: impl FnOnce() -> Option<RgbaImage>,
) -> Graphic {
    let empty = rect.width == 0 || rect.height == 0;
    let png = match cache.get(&id) {
        Some((k, png)) if *k == key => png.clone(),
        _ => {
            let png = Arc::new(if empty {
                Vec::new()
            } else {
                render().map(|i| art::png(&i)).unwrap_or_default()
            });
            cache.insert(id, (key, png.clone()));
            png
        }
    };
    Graphic {
        id,
        placement: 1,
        rect,
        z,
        key,
        png,
        crop: None,
        visible: true,
    }
}
