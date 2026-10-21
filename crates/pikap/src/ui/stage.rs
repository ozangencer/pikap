//! Stage mode (`z`): the whole screen for the playing track. The cover becomes a
//! sleeve with the record slid out to its right, the label turning while music
//! plays; synced lyrics run karaoke style beside it, the current line drawn large;
//! title, artist and the equalizer sit along the bottom.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{album_color, fit, row, styled, wave_and_times};
use crate::app::App;
use crate::art::LABEL_SHARE;
use crate::gallery::Slots;

/// How far the record slides out of its sleeve, as a share of the sleeve's width.
const SLIDE: f32 = 0.7;
/// Label turns shown per second of playback (the equalizer clock slows them down
/// and stops them when paused): 16 turns at 8 a second is about 30 rpm.
const LABEL_FPS: f32 = 8.0;
const BIG_LINE: u16 = 3;
/// A rest in the lyrics, drawn large. The title font has no musical note.
const REST: &str = "• • •";

pub(super) fn stage(f: &mut Frame, app: &App, title_as_image: bool, slots: &mut Slots) {
    let p = app.p();
    slots.stage = true;
    let outer = slots.screen.inner(Margin {
        horizontal: 3,
        vertical: 1,
    });
    let [top, _, main, _, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(5),
    ])
    .areas(outer);
    f.render_widget(
        Line::from(vec![
            Span::styled("◉ pikap", p.key()),
            Span::styled(format!("  ·  {}", app.s.stage), p.muted()),
        ]),
        top,
    );
    let corner = app.notice.as_deref().unwrap_or(app.s.stage_exit);
    f.render_widget(styled(corner, p.muted()).right_aligned(), top);

    let Some(now) = app.model.now.as_ref() else {
        f.render_widget(
            styled(app.s.nothing_playing, p.muted()).centered(),
            row(main, main.height / 2),
        );
        return;
    };

    let lyrics_area = if main.width >= 100 && main.height >= 12 {
        record(app, main, slots)
    } else {
        main
    };
    karaoke(f, app, lyrics_area, title_as_image, slots);

    let mut meta = vec![
        Span::styled(
            now.title.as_str(),
            Style::new().fg(p.bright).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", p.muted()),
        Span::styled(now.artist.as_str(), p.accent()),
        Span::styled(format!("  ·  {}", now.album), p.muted()),
    ];
    if now.liked {
        meta.push(Span::styled("  ♥", Style::new().fg(p.like)));
    }
    f.render_widget(Line::from(meta), row(bottom, 0));
    wave_and_times(
        f,
        app,
        Rect {
            y: bottom.y + 1,
            height: 3,
            ..bottom
        },
    );
    let mut keys = Vec::new();
    for (k, label) in app.s.hints_stage {
        keys.push(Span::styled(*k, p.text()));
        keys.push(Span::styled(format!(" {label}   "), p.muted()));
    }
    f.render_widget(Line::from(keys), row(bottom, 4));
}

/// The sleeve (cover) with the record half out to its right; returns the space for
/// the lyrics beside it, as tall as the record so the current line sits level with
/// the record's centre.
fn record(app: &App, main: Rect, slots: &mut Slots) -> Rect {
    let ratio = slots.cell_ratio.max(1.0);
    let block_max = f32::from(main.width) * 0.55;
    let mut rows = main.height;
    let mut cols = (f32::from(rows) * ratio).round() as u16;
    if f32::from(cols) * (1.0 + SLIDE) > block_max {
        cols = (block_max / (1.0 + SLIDE)) as u16;
        rows = (f32::from(cols) / ratio).round() as u16;
    }
    let sleeve = Rect {
        x: main.x,
        y: main.y + (main.height - rows) / 2,
        width: cols,
        height: rows,
    };
    let vinyl = Rect {
        x: sleeve.x + (f32::from(cols) * SLIDE) as u16,
        ..sleeve
    };
    // The label's cell box shares the record's centre exactly: same parity.
    let even = |n: f32, of: u16| {
        let n = n.round() as u16;
        if (of - n).is_multiple_of(2) { n } else { n + 1 }
    };
    let (lc, lr) = (
        even(f32::from(cols) * LABEL_SHARE, cols),
        even(f32::from(rows) * LABEL_SHARE, rows),
    );
    slots.cover = sleeve;
    slots.vinyl = vinyl;
    slots.label = Rect {
        x: vinyl.x + (cols - lc) / 2,
        y: vinyl.y + (rows - lr) / 2,
        width: lc,
        height: lr,
    };
    slots.label_frame = (app.eq_time * LABEL_FPS) as usize;
    let x = vinyl.right() + 5;
    Rect {
        x,
        width: main.right().saturating_sub(x),
        ..sleeve
    }
}

/// Synced lyrics with the current line large in the middle, earlier lines dim above
/// and the next ones below. Unsynced lyrics simply list.
fn karaoke(f: &mut Frame, app: &App, area: Rect, title_as_image: bool, slots: &mut Slots) {
    let p = app.p();
    let note = |f: &mut Frame, text: &str| {
        f.render_widget(
            styled(fit(text, area.width), p.muted()),
            row(area, area.height / 2),
        );
    };
    let page = match &app.model.lyrics {
        None => return note(f, app.s.loading),
        Some(None) => return note(f, app.s.no_lyrics),
        Some(Some(page)) => page,
    };
    let words = |i: usize| -> String {
        page.lines
            .get(i)
            .map(|l| l.words.trim())
            .filter(|w| !w.is_empty())
            .unwrap_or("♪")
            .to_owned()
    };
    if !page.synced {
        f.render_widget(styled(app.s.lyrics_unsynced, p.muted()), row(area, 0));
        for y in 2..area.height {
            let i = usize::from(y - 2);
            if i >= page.lines.len() {
                break;
            }
            f.render_widget(
                Span::styled(fit(&words(i), area.width), p.text()),
                row(area, y),
            );
        }
        return;
    }
    let current = page.lines.iter().rposition(|l| l.start <= app.position());
    let mid = area.y + area.height.saturating_sub(BIG_LINE) / 2;
    let big = Rect {
        y: mid,
        height: BIG_LINE,
        ..area
    };
    let big_text = match current.map(words) {
        Some(w) if w != "♪" => w,
        _ => REST.to_owned(),
    };
    if title_as_image {
        slots.lyric = Some((big, crate::i18n::display_upper(&big_text)));
    } else {
        let style = Style::new()
            .fg(album_color(app))
            .add_modifier(Modifier::BOLD);
        f.render_widget(Span::styled(fit(&big_text, area.width), style), row(big, 1));
    }
    let next = current.map_or(0, |c| c + 1);
    // Earlier lines fill the space above, later ones the space below.
    for k in 1.. {
        let Some(i) = current.and_then(|c| c.checked_sub(k)) else {
            break;
        };
        let Some(y) = mid.checked_sub(k as u16 + 1).filter(|y| *y >= area.y) else {
            break;
        };
        f.render_widget(
            Span::styled(fit(&words(i), area.width), p.muted()),
            Rect {
                y,
                height: 1,
                ..area
            },
        );
    }
    let mut y = big.bottom() + 1;
    let mut i = next;
    while y < area.bottom() && i < page.lines.len() {
        let style = if i == next {
            p.text().add_modifier(Modifier::BOLD)
        } else {
            p.text()
        };
        f.render_widget(
            Span::styled(fit(&words(i), area.width), style),
            Rect {
                y,
                height: 1,
                ..area
            },
        );
        y += 1;
        i += 1;
    }
}
