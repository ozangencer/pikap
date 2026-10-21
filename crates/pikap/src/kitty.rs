//! Kitty graphics protocol output. Images are sent once per content key and placed
//! again only when their cell rectangle or layer changes. One image can have several
//! placements (a solid colour fills every tag, for instance).
//!
//! Muxy only accepts direct transmission (`t=d`) and paints layers in kitty order:
//! `z < -1_073_741_824` under cell backgrounds, `[-1_073_741_824, 0)` above
//! backgrounds but under text, `z >= 0` above text. With a translucent window Muxy
//! also paints the default background, so pikap keeps every layer in the middle band
//! and never relies on cell backgrounds.

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use ratatui::layout::Rect;

pub const Z_HALO: i32 = -1_000_000;
/// Surfaces stack upwards from here in drawing order: Muxy does not define the order
/// of placements on the same layer, and a panel drawn over its own tag hid the tag.
pub const Z_SURFACE: i32 = -900_000;
pub const Z_ART: i32 = 0;

/// The halo's and the surfaces' layers, overridable for diagnosing a terminal with
/// `PIKAP_Z_HALO` and `PIKAP_Z_SURFACE` (surfaces stack upwards from theirs).
pub fn layers() -> (i32, i32) {
    static LAYERS: std::sync::OnceLock<(i32, i32)> = std::sync::OnceLock::new();
    *LAYERS.get_or_init(|| {
        let var = |name: &str, default: i32| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(default)
        };
        (
            var("PIKAP_Z_HALO", Z_HALO),
            var("PIKAP_Z_SURFACE", Z_SURFACE),
        )
    })
}

/// Images up to this size (the halo and the solid surfaces) are sent again every
/// [`RESEND_SMALL`], so the theme comes back on its own if the terminal ever drops
/// them; resending costs a few kilobytes.
const SMALL_PNG: usize = 16 * 1024;
const RESEND_SMALL: Duration = Duration::from_secs(10);

pub struct Graphic {
    pub id: u32,
    pub placement: u32,
    pub rect: Rect,
    pub z: i32,
    pub key: u64,
    pub png: Arc<Vec<u8>>,
    /// Source rectangle (x, y, w, h) in image pixels; `None` shows the whole image.
    pub crop: Option<[u32; 4]>,
    /// Hidden graphics stay transmitted but unplaced: animation frames are sent once
    /// and then only their placements change.
    pub visible: bool,
}

/// Where a placement was put: cells, layer and source rectangle.
type Placed = (Rect, i32, Option<[u32; 4]>);

#[derive(Default)]
pub struct Kitty {
    /// Content key and PNG size of every image sent.
    sent: HashMap<u32, (u64, usize)>,
    placed: HashMap<(u32, u32), Placed>,
    last_resend: Option<Instant>,
}

impl Kitty {
    /// Sends and places every image again after a resize. The screen clear that comes
    /// with a resize also drops the images themselves (a freshly split Muxy pane
    /// resizes once after pikap starts, and its halo stayed black until resent).
    pub fn invalidate(&mut self) {
        self.sent.clear();
        self.placed.clear();
    }

    /// Deletes everything and starts over (`ctrl-l`).
    pub fn reset(&mut self, w: &mut impl Write) -> io::Result<()> {
        self.sent.clear();
        self.placed.clear();
        delete_all(w)
    }

    pub fn sync(&mut self, w: &mut impl Write, want: &[Graphic]) -> io::Result<()> {
        if self.last_resend.is_none_or(|t| t.elapsed() >= RESEND_SMALL) {
            self.last_resend = Some(Instant::now());
            // Keep them listed (so they are still deleted when unused), only due again.
            for (key, len) in self.sent.values_mut() {
                if *len <= SMALL_PNG {
                    *key = 0;
                }
            }
        }
        let wanted = |id: u32, p: Option<u32>| match p {
            None => want.iter().any(|g| g.id == id),
            Some(p) => want
                .iter()
                .any(|g| g.id == id && g.placement == p && g.visible),
        };
        let stale_images: Vec<u32> = self
            .sent
            .keys()
            .filter(|id| !wanted(**id, None))
            .copied()
            .collect();
        for id in stale_images {
            write!(w, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")?;
            self.sent.remove(&id);
            self.placed.retain(|(i, _), _| *i != id);
        }
        let stale_placements: Vec<(u32, u32)> = self
            .placed
            .keys()
            .filter(|(id, p)| !wanted(*id, Some(*p)))
            .copied()
            .collect();
        for (id, p) in stale_placements {
            write!(w, "\x1b_Ga=d,d=i,i={id},p={p},q=2\x1b\\")?;
            self.placed.remove(&(id, p));
        }

        for g in want {
            if g.rect.width == 0 || g.rect.height == 0 {
                continue;
            }
            if self.sent.get(&g.id).map(|(key, _)| *key) != Some(g.key) {
                transmit(w, g.id, &g.png)?;
                self.sent.insert(g.id, (g.key, g.png.len()));
                self.placed.retain(|(i, _), _| *i != g.id);
            }
            let slot = (g.id, g.placement);
            if !g.visible {
                continue;
            }
            if self.placed.get(&slot) != Some(&(g.rect, g.z, g.crop)) {
                let crop = g
                    .crop
                    .map(|[x, y, cw, ch]| format!(",x={x},y={y},w={cw},h={ch}"))
                    .unwrap_or_default();
                write!(
                    w,
                    "\x1b7\x1b[{};{}H\x1b_Ga=p,i={},p={},c={},r={},z={}{crop},C=1,q=2\x1b\\\x1b8",
                    g.rect.y + 1,
                    g.rect.x + 1,
                    g.id,
                    g.placement,
                    g.rect.width,
                    g.rect.height,
                    g.z
                )?;
                self.placed.insert(slot, (g.rect, g.z, g.crop));
            }
        }
        w.flush()
    }
}

fn transmit(w: &mut impl Write, id: u32, png: &[u8]) -> io::Result<()> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let chunks: Vec<&[u8]> = b64.as_bytes().chunks(4096).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        if i == 0 {
            write!(w, "\x1b_Ga=t,f=100,t=d,i={id},q=2,m={more};")?;
        } else {
            write!(w, "\x1b_Gm={more},q=2;")?;
        }
        w.write_all(chunk)?;
        w.write_all(b"\x1b\\")?;
    }
    Ok(())
}

pub fn delete_all(w: &mut impl Write) -> io::Result<()> {
    w.write_all(b"\x1b_Ga=d,d=A,q=2\x1b\\")?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graphic(id: u32, png_len: usize) -> Graphic {
        Graphic {
            id,
            placement: 1,
            rect: Rect::new(0, 0, 2, 2),
            z: Z_SURFACE,
            key: 7,
            png: Arc::new(vec![0; png_len]),
            crop: None,
            visible: true,
        }
    }

    fn transmits(out: &[u8]) -> usize {
        String::from_utf8_lossy(out).matches("a=t,").count()
    }

    #[test]
    fn small_images_come_back_on_their_own() {
        let want = [graphic(1, 100), graphic(2, SMALL_PNG + 1)];
        let mut kitty = Kitty::default();
        let mut out = Vec::new();
        kitty.sync(&mut out, &want).unwrap();
        assert_eq!(transmits(&out), 2);

        let mut out = Vec::new();
        kitty.sync(&mut out, &want).unwrap();
        assert_eq!(transmits(&out), 0, "nothing changed");

        kitty.last_resend = Some(Instant::now() - RESEND_SMALL);
        let mut out = Vec::new();
        kitty.sync(&mut out, &want).unwrap();
        assert_eq!(transmits(&out), 1, "only the small one is sent again");
    }
}
