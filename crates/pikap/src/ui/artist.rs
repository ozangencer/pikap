//! The artist page, discography first: name and popularity, popular and
//! liked tracks as strips, the releases as a grid of covers, related artists at the
//! bottom. The same page serves the home artist tab and any opened artist.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{fit, row, styled};
use crate::app::{App, ArtistView, LIKED, POPULAR, RELATED, RELEASES};
use crate::gallery::Slots;
use crate::spotify::meta::ArtistPage;

/// Width of the section labels in front of the strips.
const LABEL_W: u16 = 18;
const CARD_W: u16 = 14;
const CARD_GAP: u16 = 2;
/// Popularity is drawn as this many blocks.
const POPULARITY_BLOCKS: usize = 8;

pub(super) fn artist_page(
    f: &mut Frame,
    app: &App,
    area: Rect,
    page: &ArtistPage,
    view: &ArtistView,
    slots: &mut Slots,
) {
    let p = app.p();
    let s = app.s;
    let filled = (usize::from(page.popularity) * POPULARITY_BLOCKS).div_ceil(100);
    let header = Line::from(vec![
        Span::styled(
            crate::i18n::display_upper(&page.name),
            Style::new().fg(p.bright).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled("▮".repeat(filled), p.accent()),
        Span::styled("▯".repeat(POPULARITY_BLOCKS - filled), p.line()),
        Span::styled(format!(" {}", s.popularity), p.muted()),
    ]);
    f.render_widget(header, row(area, 0));
    let keys = Line::from(vec![
        Span::styled("P", p.key()),
        Span::styled(format!(" {}   ", s.artist_play), p.muted()),
        Span::styled("r", p.key()),
        Span::styled(format!(" {}", s.radio_short), p.muted()),
    ])
    .right_aligned();
    if area.width > 90 {
        f.render_widget(keys, row(area, 0));
    }

    let mut y = area.y + 2;
    let names = |tracks: &[crate::spotify::meta::TrackInfo]| -> Vec<String> {
        tracks.iter().map(|t| t.name.clone()).collect()
    };
    let lines = |n: u16| Rect {
        y,
        height: n.min(area.bottom().saturating_sub(y)),
        ..area
    };
    y += strip(
        f,
        app,
        lines(2),
        POPULAR,
        s.artist_popular,
        &names(&page.top),
        view,
        slots,
    ) + 1;
    let liked = app.liked_by(&page.uri);
    if !liked.is_empty() {
        let label = (s.artist_liked)(liked.len());
        let lines = Rect {
            y,
            height: 2.min(area.bottom().saturating_sub(y)),
            ..area
        };
        y += strip(f, app, lines, LIKED, &label, &names(&liked), view, slots) + 1;
    }

    let related_h = if page.related.is_empty() { 0 } else { 2 };
    let bottom = area.bottom().saturating_sub(related_h);
    // Related artists follow the grid; they sit at the bottom only when it scrolls.
    let mut related_y = y;
    if y < bottom {
        let albums = page.discography.iter().filter(|c| !c.single).count();
        let singles = page.discography.len() - albums;
        let detail = if page.discography.is_empty() && !page.releases.is_empty() {
            format!("  {}", s.loading)
        } else {
            format!("  {}", (s.artist_releases)(albums, singles))
        };
        let heading = Line::from(vec![
            Span::styled(s.artist_discography, label_style(app, view, RELEASES)),
            Span::styled(detail, p.muted()),
        ]);
        f.render_widget(heading, row(area, y - area.y));
        let grid_area = Rect {
            y: y + 2,
            height: bottom.saturating_sub(y + 2),
            ..area
        };
        let used = grid(f, app, grid_area, page, view, slots);
        related_y = grid_area.y + used + u16::from(used > 0);
    }
    if related_h > 0 && related_y < area.bottom() {
        let names: Vec<String> = page.related.iter().map(|(_, n)| n.clone()).collect();
        let last = Rect {
            y: related_y,
            height: 1,
            ..area
        };
        strip(f, app, last, RELATED, s.artist_related, &names, view, slots);
    }
}

fn label_style(app: &App, view: &ArtistView, section: usize) -> Style {
    if view.section == section {
        app.p()
            .text()
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        app.p().muted()
    }
}

/// A label, then items separated by dots, wrapping over `area.height` lines. The view
/// scrolls by whole items so the selection stays visible; returns the lines used.
#[allow(clippy::too_many_arguments)]
fn strip(
    f: &mut Frame,
    app: &App,
    area: Rect,
    section: usize,
    label: &str,
    items: &[String],
    view: &ArtistView,
    slots: &mut Slots,
) -> u16 {
    let p = app.p();
    if area.height == 0 || items.is_empty() {
        return 0;
    }
    f.render_widget(
        Span::styled(fit(label, LABEL_W - 2), label_style(app, view, section)),
        row(area, 0),
    );
    let width = usize::from(area.width.saturating_sub(LABEL_W));
    let lines = usize::from(area.height);
    let focused = view.section == section;
    let selected = focused.then(|| view.selected[section].min(items.len() - 1));
    let mut start = view.strip_start[section].get().min(items.len() - 1);
    if let Some(sel) = selected {
        start = start.min(sel);
    }
    let mut placed = layout(items, start, width, lines);
    while let Some(sel) = selected
        && !placed.iter().any(|(i, ..)| *i == sel)
        && start < sel
    {
        start += 1;
        placed = layout(items, start, width, lines);
    }
    view.strip_start[section].set(start);

    let x0 = area.x + LABEL_W;
    for &(i, line, x, w) in &placed {
        let cell = Rect {
            x: x0 + x as u16,
            y: area.y + line as u16,
            width: w as u16,
            height: 1,
        };
        if x > 0 {
            f.render_widget(
                Span::styled(" · ", p.line()),
                Rect {
                    x: cell.x - 3,
                    width: 3,
                    ..cell
                },
            );
        }
        let style = if selected == Some(i) {
            slots.fill(cell, p.selected);
            p.accent().add_modifier(Modifier::BOLD)
        } else {
            p.text()
        };
        f.render_widget(Span::styled(fit(&items[i], cell.width), style), cell);
    }
    let more = start > 0 || placed.last().is_some_and(|(i, ..)| i + 1 < items.len());
    if more && let Some(&(_, line, x, w)) = placed.last() {
        let room = width.saturating_sub(x + w);
        if room >= 2 {
            f.render_widget(
                Span::styled(" …", p.muted()),
                Rect {
                    x: x0 + (x + w) as u16,
                    y: area.y + line as u16,
                    width: 2,
                    height: 1,
                },
            );
        }
    }
    placed.last().map_or(1, |(_, line, ..)| *line as u16 + 1)
}

/// (item, line, x, width) for items from `start` that fit in `lines` lines.
fn layout(
    items: &[String],
    start: usize,
    width: usize,
    lines: usize,
) -> Vec<(usize, usize, usize, usize)> {
    let mut out = Vec::new();
    let (mut line, mut x) = (0, 0);
    for (i, item) in items.iter().enumerate().skip(start) {
        // Keep room for the trailing "…".
        let w = item.chars().count().min(width.saturating_sub(2)).max(1);
        let gap = if x == 0 { 0 } else { 3 };
        if x + gap + w + 2 > width && x > 0 {
            line += 1;
            if line >= lines {
                break;
            }
            out.push((i, line, 0, w));
            x = w;
            continue;
        }
        out.push((i, line, x + gap, w));
        x += gap + w;
    }
    out
}

/// Releases as covers with name and year under each, scrolled by whole rows; returns
/// the rows used.
fn grid(
    f: &mut Frame,
    app: &App,
    area: Rect,
    page: &ArtistPage,
    view: &ArtistView,
    slots: &mut Slots,
) -> u16 {
    let p = app.p();
    let cards = &page.discography;
    if cards.is_empty() || area.height < 3 {
        return 0;
    }
    let cols = usize::from((area.width + CARD_GAP) / (CARD_W + CARD_GAP)).max(1);
    view.grid_cols.set(cols);
    let cover_rows = (f32::from(CARD_W) / slots.cell_ratio.max(1.0))
        .round()
        .clamp(2.0, 12.0) as u16;
    let card_h = cover_rows + 3;
    let visible = usize::from((area.height + 1) / card_h).max(1);
    let total = cards.len().div_ceil(cols);
    let focused = view.section == RELEASES;
    let selected = view.selected[RELEASES].min(cards.len() - 1);
    let mut first = view.grid_row.get();
    if focused {
        let row = selected / cols;
        if row < first {
            first = row;
        } else if row >= first + visible {
            first = row + 1 - visible;
        }
    }
    let first = first.min(total.saturating_sub(visible));
    view.grid_row.set(first);

    for r in 0..visible {
        for c in 0..cols {
            let i = (first + r) * cols + c;
            let Some(card) = cards.get(i) else { break };
            let x = area.x + c as u16 * (CARD_W + CARD_GAP);
            let y = area.y + r as u16 * card_h;
            if y + cover_rows + 2 > area.bottom() {
                break;
            }
            let cover = Rect {
                x,
                y,
                width: CARD_W,
                height: cover_rows,
            };
            match app.model.card_covers.get(&card.uri) {
                Some(path) => slots.cards.push((cover, path.clone())),
                None => {
                    f.render_widget(styled("◎", p.line()).centered(), row(cover, cover_rows / 2))
                }
            }
            let text = Rect {
                y: y + cover_rows,
                height: 2,
                ..cover
            };
            let is_selected = focused && i == selected;
            let style = if is_selected {
                slots.fill(text, p.selected);
                p.accent().add_modifier(Modifier::BOLD)
            } else {
                p.text()
            };
            f.render_widget(Span::styled(fit(&card.name, CARD_W), style), row(text, 0));
            let mut year = card.year.map(|y| y.to_string()).unwrap_or_default();
            if card.single {
                year = format!("{year} · {}", app.s.single);
            }
            f.render_widget(Span::styled(fit(&year, CARD_W), p.muted()), row(text, 1));
        }
    }
    let shown = visible.min(total - first) as u16;
    if first + visible < total && area.height > 0 {
        let more = format!("↓ {}", (total - first - visible) * cols);
        f.render_widget(
            styled(more, p.muted()).right_aligned(),
            row(area, area.height - 1),
        );
        return area.height;
    }
    (shown * card_h).saturating_sub(1).min(area.height)
}
