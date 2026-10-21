//! Dalga screens. Text is drawn with ratatui with foreground colours only; artwork
//! rectangles and filled surfaces go into [`Slots`] for the kitty layer.
//!
//! Two layouts: the wide one from the mockup, and a narrow one for sidebars (no cover,
//! the halo still carries its colours). The library and search pages live in [`pages`].

mod artist;
mod pages;
mod stage;

use std::cell::Cell;
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders};

use crate::app::{App, LEADER_KEYS, Page, Popup};
use crate::art;
use crate::data::{NowPlaying, Track};
use crate::gallery::{CellPx, Slots};
use crate::spotify::api::Quota;

const LEADER_HEIGHT: u16 = 5;
const COMPACT_LEADER_HEIGHT: u16 = 9;
const REFLECTION_ROWS: u16 = 3;
const COLUMN_GAP: u16 = 5;
const WIDE_MIN: (u16, u16) = (100, 30);
const COMPACT_MIN: (u16, u16) = (32, 14);

pub fn render(f: &mut Frame, app: &App, cell: CellPx, title_as_image: bool) -> Slots {
    let screen = f.area();
    let mut slots = Slots {
        screen,
        cell_ratio: cell.ratio(),
        ..Slots::default()
    };
    let fits = screen.width >= COMPACT_MIN.0 && screen.height >= COMPACT_MIN.1;
    if fits && app.stage {
        stage::stage(f, app, title_as_image, &mut slots);
        return slots;
    }
    if fits && app.page != Page::Home {
        pages::content(f, app, cell, title_as_image, &mut slots);
    } else if fits && app.model.now.is_none() {
        idle(f, app, &mut slots);
    } else if screen.width >= WIDE_MIN.0 && screen.height >= WIDE_MIN.1 {
        wide(f, app, cell, title_as_image, &mut slots);
    } else if screen.width >= COMPACT_MIN.0 && screen.height >= COMPACT_MIN.1 {
        compact(f, app, title_as_image, &mut slots);
    } else {
        let msg = (app.s.too_small)(COMPACT_MIN, (screen.width, screen.height));
        f.render_widget(styled(fit(&msg, screen.width), app.p().muted()), screen);
    }
    if fits {
        pages::popup(f, app, &mut slots);
    }
    slots
}

/// The playing track. Only called from layouts `render` picks when something plays.
fn now(app: &App) -> &NowPlaying {
    app.model
        .now
        .as_ref()
        .expect("render() shows the idle screen when nothing plays")
}

/// Nothing playing: the header, a centred note and the status line.
fn idle(f: &mut Frame, app: &App, slots: &mut Slots) {
    let p = app.p();
    let compact = slots.screen.width < WIDE_MIN.0;
    let outer = slots.screen.inner(Margin {
        horizontal: if compact { 2 } else { 3 },
        vertical: 1,
    });
    let [header, body, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(outer);
    header_line(f, app, header, compact);
    let middle = body.y + body.height / 2;
    let title = Style::new().fg(p.bright).add_modifier(Modifier::BOLD);
    let heading = if app.synced {
        app.s.idle_title
    } else {
        app.s.connecting
    };
    f.render_widget(
        styled(fit(heading, body.width), title).centered(),
        Rect {
            y: middle.saturating_sub(1),
            height: 1,
            ..body
        },
    );
    if !app.synced {
        status_line(f, app, status, compact, slots);
        return;
    }
    if let Some((track, artist)) = &app.model.last_played {
        f.render_widget(
            styled(fit(&(app.s.idle_last)(track, artist), body.width), p.text()).centered(),
            Rect {
                y: middle + 1,
                height: 1,
                ..body
            },
        );
    }
    f.render_widget(
        styled(fit(app.s.idle_hint, body.width), p.muted()).centered(),
        Rect {
            y: middle + 3,
            height: 1,
            ..body
        },
    );
    status_line(f, app, status, compact, slots);
}

fn wide(f: &mut Frame, app: &App, cell: CellPx, title_as_image: bool, slots: &mut Slots) {
    let outer = slots.screen.inner(Margin {
        horizontal: 3,
        vertical: 1,
    });
    let leader_h = if app.leader { LEADER_HEIGHT + 1 } else { 0 };
    let [header, _, main, leader, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(leader_h),
        Constraint::Length(1),
    ])
    .areas(outer);

    header_line(f, app, header, false);

    // Size the cover as if the leader panel were open so it never jumps when toggled.
    let budget = outer
        .height
        .saturating_sub(3 + LEADER_HEIGHT + 1 + REFLECTION_ROWS + 4);
    let mut rows = budget.min(26);
    let mut cols = (f32::from(rows) * cell.ratio()).round() as u16;
    let max_cols = main.width * 2 / 5;
    if cols > max_cols {
        cols = max_cols;
        rows = (f32::from(cols) / cell.ratio()).round() as u16;
    }
    let left = Rect {
        width: cols,
        ..main
    };
    let right = Rect {
        x: main.x + cols + COLUMN_GAP,
        width: main.width.saturating_sub(cols + COLUMN_GAP),
        ..main
    };

    slots.cover = Rect {
        height: rows,
        ..left
    };
    slots.reflection = Rect {
        y: slots.cover.bottom(),
        height: REFLECTION_ROWS,
        ..left
    };
    wave_and_times(
        f,
        app,
        Rect {
            y: slots.reflection.bottom(),
            height: 3,
            ..left
        },
    );
    let below_times = slots.reflection.bottom() + 5;
    discovery(
        f,
        app,
        Rect {
            y: below_times,
            height: left.bottom().saturating_sub(below_times),
            ..left
        },
    );
    slots.title = Rect { height: 2, ..right };
    right_column(f, app, right, title_as_image, slots);

    if app.leader {
        let area = Rect {
            y: leader.y + 1,
            height: LEADER_HEIGHT,
            ..leader
        };
        leader_panel(f, app, area, 4, slots);
    }
    status_line(f, app, status, false, slots);
}

fn compact(f: &mut Frame, app: &App, title_as_image: bool, slots: &mut Slots) {
    let p = app.p();
    let outer = slots.screen.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let leader_h = if app.leader {
        COMPACT_LEADER_HEIGHT + 1
    } else {
        0
    };
    let [header, _, title, meta, _, wave, _, list, leader, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(leader_h),
        Constraint::Length(1),
    ])
    .areas(outer);

    header_line(f, app, header, true);
    slots.title = title;
    if !title_as_image {
        title_text(f, app, title);
    }
    let now = now(app);
    let heart = if now.liked { " ♥" } else { " ♡" };
    let room = meta
        .width
        .saturating_sub(now.artist.chars().count() as u16 + 2);
    let meta_line = Line::from(vec![
        Span::styled(now.artist.as_str(), p.accent()),
        Span::styled(
            fit(&format!(" · {}", now.album), room.saturating_sub(2)),
            p.muted(),
        ),
        Span::styled(heart, Style::new().fg(p.like)),
        Span::styled(" f", p.muted()),
    ]);
    f.render_widget(meta_line, meta);
    wave_and_times(f, app, wave);

    if list.height > 1 {
        f.render_widget(styled(app.s.tabs[0], p.muted()), row(list, 0));
        let rows = Rect {
            y: list.y + 1,
            height: list.height - 1,
            ..list
        };
        track_rows(
            f,
            app,
            rows,
            &app.list_rows(),
            &home_list(app, false),
            slots,
        );
    }

    if app.leader {
        let area = Rect {
            y: leader.y + 1,
            height: COMPACT_LEADER_HEIGHT,
            ..leader
        };
        leader_panel(f, app, area, 2, slots);
    }
    status_line(f, app, status, true, slots);
}

fn header_line(f: &mut Frame, app: &App, area: Rect, compact: bool) {
    let p = app.p();
    if compact {
        let page = match app.page {
            Page::Home => "",
            Page::Library => app.s.library,
            Page::Search => app.s.search,
        };
        let logo = Line::from(vec![
            Span::styled("◉ pikap", p.key()),
            Span::styled(format!("  {page}"), p.muted()),
        ]);
        f.render_widget(logo, area);
        let device = app
            .model
            .device
            .rsplit('·')
            .next()
            .unwrap_or_default()
            .trim();
        let room = area.width.saturating_sub(11 + page.chars().count() as u16);
        f.render_widget(
            styled(fit(&format!("▣ {device}"), room), p.muted()).right_aligned(),
            area,
        );
        return;
    }
    let mut crumbs = vec![Span::styled("◉ pikap", p.key())];
    let mut trail: Vec<String> = Vec::new();
    match app.page {
        Page::Home => {
            crumbs.push(Span::styled(format!("   {}", app.s.library), p.muted()));
            match &app.home_open {
                Some(open) => trail.push(app.entry_title(&open.entry)),
                None => trail.extend(app.context_title()),
            }
        }
        Page::Library => {
            crumbs.push(Span::styled(format!("   {}", app.s.library), p.muted()));
            trail.extend(app.library.open.as_ref().map(|o| app.entry_title(&o.entry)));
        }
        Page::Search => {
            crumbs.push(Span::styled(format!("   {}", app.s.search), p.muted()));
            if let Some(r) = &app.search.results {
                trail.push(format!("“{}”", r.query));
            }
            trail.extend(app.search.open.as_ref().map(|o| app.entry_title(&o.entry)));
        }
    }
    for title in trail {
        crumbs.push(Span::styled(" › ", p.muted()));
        crumbs.push(Span::styled(title, p.text()));
    }
    let left = Line::from(crumbs);
    let filled = usize::from(app.model.volume / 10);
    let right = Line::from(vec![
        Span::styled(
            format!("▣ {}    {} ", app.model.device, app.s.vol),
            p.muted(),
        ),
        Span::styled("▮".repeat(filled), p.accent()),
        Span::styled("▮".repeat(10 - filled), p.line()),
        Span::styled(format!(" {}%", app.model.volume), p.muted()),
    ])
    .right_aligned();
    f.render_widget(left, area);
    f.render_widget(right, area);
}

fn title_text(f: &mut Frame, app: &App, area: Rect) {
    let style = Style::new().fg(app.p().bright).add_modifier(Modifier::BOLD);
    let title = crate::i18n::display_upper(&now(app).title);
    f.render_widget(Span::styled(fit(&title, area.width), style), area);
}

/// Two-row braille equalizer doubling as the progress bar, then the times row.
fn wave_and_times(f: &mut Frame, app: &App, area: Rect) {
    let p = app.p();
    equalizer(f, app, Rect { height: 2, ..area });
    let area = Rect {
        y: area.y + 1,
        ..area
    };
    let times = row(area, 1);
    f.render_widget(Span::styled(clock(app.position()), p.text()), times);
    let state = if app.playing {
        format!(
            "{} · {}",
            app.s.shuffle[usize::from(app.model.shuffle)],
            app.s.repeat[usize::from(app.model.repeat.min(2))]
        )
    } else {
        app.s.paused.to_owned()
    };
    let room = area.width.saturating_sub(14);
    f.render_widget(styled(fit(&state, room), p.muted()).centered(), times);
    f.render_widget(
        styled(clock(now(app).duration), p.muted()).right_aligned(),
        times,
    );
}

fn rgb(color: ratatui::style::Color) -> [u8; 3] {
    match color {
        ratatui::style::Color::Rgb(r, g, b) => [r, g, b],
        _ => [255, 255, 255],
    }
}

/// The album colour, adjusted to stay readable on the halo; the accent for grey covers.
fn album_color(app: &App) -> ratatui::style::Color {
    let p = app.p();
    p.readable(app.cover_color.unwrap_or_else(|| rgb(p.accent)))
}

/// Braille bars, two per cell and eight dots tall over two rows. The played part takes
/// the album colour, the rest the rule colour.
fn equalizer(f: &mut Frame, app: &App, area: Rect) {
    // Dot bits bottom-up for the left and right braille columns.
    const LEFT: [u32; 4] = [0x40, 0x04, 0x02, 0x01];
    const RIGHT: [u32; 4] = [0x80, 0x20, 0x10, 0x08];
    let p = app.p();
    let width = usize::from(area.width);
    let bars = width * 2;
    let seed = art::seed(&now(app).id);
    let shape = art::wave_levels(seed, bars);
    let played = ((app.progress() * bars as f64) as usize).min(bars);
    let played_style = Style::new().fg(album_color(app));

    let heights: Vec<usize> = (0..bars)
        .map(|i| {
            let dance = art::eq_level(seed, i, app.eq_time);
            let level = shape[i] * (0.35 + 0.65 * app.eq_energy * dance);
            1 + (level * 7.0).round() as usize
        })
        .collect();

    let mut top = Vec::with_capacity(width);
    let mut bottom = Vec::with_capacity(width);
    for x in 0..width {
        let (l, r) = (heights[2 * x], heights[2 * x + 1]);
        let dots = |h: usize, row: usize| h.saturating_sub(row * 4).min(4);
        let cell = |row: usize| {
            let bits = LEFT[..dots(l, row)].iter().sum::<u32>()
                + RIGHT[..dots(r, row)].iter().sum::<u32>();
            char::from_u32(0x2800 + bits).unwrap_or(' ').to_string()
        };
        let style = if 2 * x < played {
            played_style
        } else {
            p.line()
        };
        bottom.push(Span::styled(cell(0), style));
        top.push(Span::styled(cell(1), style));
    }
    f.render_widget(Line::from(top), row(area, 0));
    f.render_widget(Line::from(bottom), row(area, 1));
}

/// Artists close to the playing one, in the free space under the equalizer: a heading,
/// the names in up to two columns, and the radio key.
fn discovery(f: &mut Frame, app: &App, area: Rect) {
    let p = app.p();
    let Some(now) = app.model.now.as_ref() else {
        return;
    };
    if app.model.near.is_empty() || area.height < 5 {
        return;
    }
    f.render_widget(
        styled(fit(&(app.s.near)(&now.artist), area.width), p.muted()),
        row(area, 0),
    );
    let rows = usize::from(area.height - 4);
    let columns = if area.width >= 36 { 2 } else { 1 };
    let col_w = area.width / columns as u16;
    let names: Vec<&String> = app.model.near.iter().take(rows * columns).collect();
    let per_column = names.len().div_ceil(columns);
    for (i, name) in names.iter().enumerate() {
        let cell = Rect {
            x: area.x + col_w * (i / per_column) as u16,
            y: area.y + 2 + (i % per_column) as u16,
            width: col_w.saturating_sub(2),
            height: 1,
        };
        f.render_widget(Span::styled(fit(name, cell.width), p.text()), cell);
    }
    let radio_row = area.y + 3 + per_column as u16;
    if radio_row < area.bottom() {
        f.render_widget(
            Line::from(vec![
                Span::styled("r", p.key()),
                Span::styled(format!(" {}", app.s.radio), p.muted()),
            ]),
            Rect {
                y: radio_row,
                height: 1,
                ..area
            },
        );
    }
}

fn right_column(f: &mut Frame, app: &App, area: Rect, title_as_image: bool, slots: &mut Slots) {
    let p = app.p();
    let now = now(app);
    if !title_as_image {
        title_text(f, app, area);
    }
    let meta = Line::from(vec![
        Span::styled(now.artist.as_str(), p.accent()),
        Span::styled(
            match now.year {
                Some(year) => format!("  ·  {} · {year} · ", now.album),
                None => format!("  ·  {} · ", now.album),
            },
            p.muted(),
        ),
        Span::styled(if now.liked { "♥" } else { "♡" }, Style::new().fg(p.like)),
        Span::styled("  f", p.muted()),
    ]);
    f.render_widget(meta, row(area, 2));

    tabs_line(f, app, row(area, 4));
    f.render_widget(
        styled("─".repeat(usize::from(area.width)), p.line()),
        row(area, 5),
    );

    let list = Rect {
        y: area.y + 7,
        height: area.height.saturating_sub(7),
        ..area
    };
    tab_body(f, app, list, slots);
}

/// Tab labels with their number keys; the gap shrinks before any label is dropped.
fn tabs_line(f: &mut Frame, app: &App, area: Rect) {
    let p = app.p();
    let labels: usize = app.s.tabs.iter().map(|t| t.chars().count() + 2).sum();
    let width = usize::from(area.width);
    let gap = (width.saturating_sub(labels) / app.s.tabs.len()).clamp(2, 5);
    let mut spans = Vec::new();
    let mut used = 0;
    for (i, name) in app.s.tabs.iter().enumerate() {
        let w = name.chars().count() + 2 + gap;
        if used + w - gap > width {
            break;
        }
        used += w;
        let style = if i == app.tab {
            p.text().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            p.muted()
        };
        spans.push(Span::styled(format!("{} ", i + 1), p.key()));
        spans.push(Span::styled(*name, style));
        spans.push(Span::raw(" ".repeat(gap)));
    }
    f.render_widget(Line::from(spans), area);
}

fn queue_columns(r: Rect, with_artist: bool) -> [Rect; 4] {
    let artist = if with_artist {
        Constraint::Fill(2)
    } else {
        Constraint::Length(0)
    };
    Layout::horizontal([
        Constraint::Length(5),
        Constraint::Fill(3),
        artist,
        Constraint::Length(5),
    ])
    .spacing(if with_artist { 2 } else { 1 })
    .areas(r)
}

fn queue_header(f: &mut Frame, app: &App, area: Rect, with_artist: bool) {
    let p = app.p();
    let [h0, h1, h2, h3] = queue_columns(area, with_artist);
    f.render_widget(Span::styled("  #", p.muted()), h0);
    f.render_widget(Span::styled(app.s.col_track, p.muted()), h1);
    if with_artist {
        f.render_widget(Span::styled(app.s.col_artist, p.muted()), h2);
    }
    f.render_widget(styled(app.s.col_time, p.muted()).right_aligned(), h3);
}

/// The list on the current tab; returns how many rows it used.
fn tab_body(f: &mut Frame, app: &App, list: Rect, slots: &mut Slots) -> u16 {
    let p = app.p();
    let note = |f: &mut Frame, text: &str| {
        f.render_widget(styled(fit(text, list.width), p.muted()), row(list, 0));
        1
    };
    if let Some(open) = &app.home_open {
        pages::opened(f, app, list, open, true, slots);
        return list.height;
    }
    if app.tab == 2 && app.model.meta != Some(false) {
        match &app.model.artist {
            Some(page) => artist::artist_page(f, app, list, page, &app.artist_view, slots),
            None => {
                note(f, app.s.loading);
            }
        }
        return list.height;
    }
    if app.tab == 0
        && let Some(title) = app.context_title()
    {
        let rows = app.list_rows();
        let heading = format!("{title} · {}", (app.s.track_count)(rows.len()));
        f.render_widget(
            styled(
                fit(&heading, list.width),
                p.text().add_modifier(Modifier::BOLD),
            ),
            row(list, 0),
        );
        queue_header(f, app, row(list, 2), true);
        let body = below(list, 3);
        track_rows(f, app, body, &rows, &home_list(app, true), slots);
        return 3 + (rows.len() as u16).min(body.height);
    }
    if app.tab == 0 {
        queue_header(f, app, row(list, 0), true);
        let body = below(list, 1);
        track_rows(f, app, body, &app.model.queue, &home_list(app, true), slots);
        let shown = (app.model.queue.len() as u16).min(body.height);
        if app.model.queue_loops && shown < body.height {
            f.render_widget(styled(app.s.queue_loops, p.muted()), row(body, shown));
            return 2 + shown;
        }
        return 1 + shown;
    }
    if app.model.meta == Some(false) {
        return note(f, app.s.meta_missing);
    }
    if app.tab == 1 {
        return lyrics_view(f, app, list, slots);
    }

    let title = match app.tab {
        2 => app
            .model
            .artist
            .as_ref()
            .map(|a| (app.s.artist_top)(&a.name)),
        3 => app.model.album.as_ref().map(|a| match a.year {
            Some(year) => format!("{} · {} · {year}", a.name, a.artist),
            None => format!("{} · {}", a.name, a.artist),
        }),
        _ => app
            .current_radio()
            .and(app.model.now.as_ref())
            .map(|n| (app.s.radio_of)(&n.title)),
    };
    let Some(title) = title else {
        return note(f, app.s.loading);
    };
    f.render_widget(
        styled(
            fit(&title, list.width),
            p.text().add_modifier(Modifier::BOLD),
        ),
        row(list, 0),
    );
    let rows = app.tab_rows();
    let body = Rect {
        y: list.y + 2,
        height: list.height.saturating_sub(2),
        ..list
    };
    track_rows(f, app, body, &rows, &home_list(app, true), slots);
    2 + (rows.len() as u16).min(body.height)
}

/// Synced lyrics scroll so the current line sits a third of the way down.
fn lyrics_view(f: &mut Frame, app: &App, area: Rect, slots: &mut Slots) -> u16 {
    let p = app.p();
    let page = match &app.model.lyrics {
        None => {
            f.render_widget(styled(app.s.loading, p.muted()), row(area, 0));
            return 1;
        }
        Some(None) => {
            f.render_widget(styled(app.s.no_lyrics, p.muted()), row(area, 0));
            return 1;
        }
        Some(Some(page)) => page,
    };
    let mut area = area;
    if !page.synced {
        f.render_widget(styled(app.s.lyrics_unsynced, p.muted()), row(area, 0));
        area = below(area, 2);
    }
    let height = usize::from(area.height);
    let current = page
        .synced
        .then(|| page.lines.iter().rposition(|l| l.start <= app.position()))
        .flatten();
    let start = current
        .map_or(0, |c| c.saturating_sub(height / 3))
        .min(page.lines.len().saturating_sub(height));
    for (y, (i, line)) in page
        .lines
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .enumerate()
    {
        let r = row(area, y as u16);
        let words = if line.words.trim().is_empty() {
            "♪"
        } else {
            line.words.as_str()
        };
        let text = fit(&format!("  {words}"), r.width);
        if current == Some(i) {
            // The current line takes the equalizer's album colour.
            let color = album_color(app);
            let [cr, cg, cb] = rgb(color);
            slots.fill(r, [cr, cg, cb, p.selected[3]]);
            let style = Style::new().fg(color).add_modifier(Modifier::BOLD);
            f.render_widget(Span::styled("▎", style), r);
            f.render_widget(Span::styled(text, style), r);
            continue;
        }
        let style = match current {
            Some(c) if i < c => p.muted(),
            _ => p.text(),
        };
        f.render_widget(Span::styled(text, style), r);
    }
    area.height
}

/// Where a list keeps its selection and scroll position, and how it numbers rows.
struct ListState<'a> {
    selected: Option<usize>,
    offset: &'a Cell<Option<usize>>,
    /// The first row is the playing track; upcoming rows count from 1.
    queue: bool,
    with_artist: bool,
}

/// The home list on the current tab.
fn home_list(app: &App, with_artist: bool) -> ListState<'_> {
    ListState {
        selected: Some(app.selected),
        offset: &app.list_offset,
        queue: app.tab == 0 && app.context_list().is_none(),
        with_artist,
    }
}

/// First visible row. The view stays put and only scrolls as far as needed to keep
/// the selection visible; a fresh list (`None`) starts with the selection a third of
/// the way down.
fn scroll(
    offset: &Cell<Option<usize>>,
    selected: Option<usize>,
    len: usize,
    height: usize,
) -> usize {
    let mut first = offset
        .get()
        .unwrap_or_else(|| selected.unwrap_or(0).saturating_sub(height / 3));
    if let Some(selected) = selected {
        if selected < first {
            first = selected;
        } else if selected >= first + height {
            first = selected + 1 - height;
        }
    }
    let first = first.min(len.saturating_sub(height));
    offset.set(Some(first));
    first
}

/// Track list with the playing track marked and the selection kept in view.
fn track_rows(
    f: &mut Frame,
    app: &App,
    area: Rect,
    rows: &[Track],
    state: &ListState<'_>,
    slots: &mut Slots,
) {
    let p = app.p();
    let frame = (app.position().as_millis() / 300) as usize;
    let playing_uri = app.model.now.as_ref().map(|n| n.id.as_str());
    let height = usize::from(area.height);
    let offset = scroll(state.offset, state.selected, rows.len(), height);
    // Numbers restart after each heading row.
    let mut numbers = Vec::with_capacity(rows.len());
    let mut n = usize::from(!state.queue);
    for t in rows {
        if t.uri.is_empty() {
            n = 0;
        }
        numbers.push(n);
        n += 1;
    }
    for (y, (i, t)) in rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .enumerate()
    {
        let r = row(area, y as u16);
        if t.uri.is_empty() {
            if !t.title.is_empty() {
                // A section heading in the like colour with a rule to the edge.
                let title = format!("  {}  ", t.title);
                let rule = usize::from(r.width).saturating_sub(title.chars().count());
                let heading = Line::from(vec![
                    Span::styled(title, Style::new().fg(p.like).add_modifier(Modifier::BOLD)),
                    Span::styled("─".repeat(rule), p.line()),
                ]);
                f.render_widget(heading, r);
            }
            continue;
        }
        let queue_head = state.queue && i == 0;
        let now = Some(t.uri.as_str()) == playing_uri || queue_head;
        let selected = state.selected == Some(i);
        if selected {
            slots.fill(r, p.selected);
            f.render_widget(Span::styled("▎", p.accent()), r);
        }
        let style = if now {
            p.accent().add_modifier(Modifier::BOLD)
        } else {
            p.text()
        };
        let mark = if now {
            if app.playing {
                ["▁▅▃", "▃▁▅", "▅▃▁"][frame % 3]
            } else {
                "▁▁▁"
            }
            .to_owned()
        } else if selected {
            "▶".to_owned()
        } else {
            format!("{:02}", numbers[i])
        };
        let [c0, c1, c2, c3] = queue_columns(r, state.with_artist);
        f.render_widget(Span::styled(format!("  {mark}"), style), c0);
        f.render_widget(Span::styled(fit(&t.title, c1.width), style), c1);
        if state.with_artist {
            f.render_widget(Span::styled(fit(&t.artist, c2.width), style), c2);
        }
        f.render_widget(styled(clock(t.duration), style).right_aligned(), c3);
    }
}

fn below(area: Rect, offset: u16) -> Rect {
    Rect {
        y: area.y + offset,
        height: area.height.saturating_sub(offset),
        ..area
    }
}

fn leader_panel(f: &mut Frame, app: &App, area: Rect, columns: u16, slots: &mut Slots) {
    let p = app.p();
    slots.fill(area, p.panel);
    let block = Block::new()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(p.line());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let tag_w = app.s.leader_title.chars().count() as u16;
    let close_w = app.s.leader_close.chars().count() as u16 + 2;
    let (tag, grid, hint) = if columns >= 4 {
        let [tag, _, grid, hint] = Layout::horizontal([
            Constraint::Length(tag_w),
            Constraint::Length(3),
            Constraint::Fill(1),
            Constraint::Length(close_w),
        ])
        .areas(inner);
        (row(tag, 0), grid, row(hint, 0))
    } else {
        let [top, _, grid] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(inner);
        let tag = Rect {
            width: tag_w.min(top.width),
            ..top
        };
        (tag, grid, top)
    };
    slots.fill(tag, p.tag);
    f.render_widget(Span::styled(app.s.leader_title, p.tag()), tag);
    f.render_widget(styled(app.s.leader_close, p.muted()).right_aligned(), hint);

    let col_w = grid.width / columns;
    for (i, (k, label)) in LEADER_KEYS.iter().zip(app.s.leader_labels).enumerate() {
        let i = i as u16;
        let cell = Rect {
            x: grid.x + col_w * (i % columns),
            y: grid.y + i / columns,
            width: col_w,
            height: 1,
        };
        if cell.y >= grid.bottom() {
            break;
        }
        let key_w = if columns >= 4 { 6 } else { 3 };
        let line = Line::from(vec![
            Span::styled(format!("{k:<key_w$}"), p.key()),
            Span::styled(fit(label, col_w.saturating_sub(key_w as u16 + 1)), p.text()),
        ]);
        f.render_widget(line, cell);
    }
}

fn status_line(f: &mut Frame, app: &App, area: Rect, compact: bool, slots: &mut Slots) {
    let p = app.p();
    let tag = Rect { width: 5, ..area };
    slots.fill(tag, p.tag);
    f.render_widget(
        Span::styled(if app.leader { " g ✕ " } else { " g … " }, p.tag()),
        tag,
    );

    let quota_w = if compact { 2 } else { 24 };
    let rest = Rect {
        x: area.x + 7,
        width: area.width.saturating_sub(7 + quota_w),
        ..area
    };
    if let Some(Popup::Remove { playlist, track }) = &app.popup {
        let ask = (app.s.confirm_remove)(&track.title, &playlist.1);
        f.render_widget(Span::styled(fit(&ask, rest.width), p.key()), rest);
    } else if let Some(notice) = &app.notice {
        f.render_widget(Span::styled(fit(notice, rest.width), p.accent()), rest);
    } else {
        let hints: &[(&str, &str)] = match app.page {
            Page::Search if app.search.typing => app.s.hints_typing,
            Page::Home if compact => &app.s.hints_compact,
            _ if compact => &app.s.hints_compact_page,
            _ if app.active_artist().is_some() => app.s.hints_artist,
            Page::Home => app.s.hints,
            Page::Library => app.s.hints_library,
            Page::Search => app.s.hints_search,
        };
        let mut spans = Vec::new();
        for (k, label) in hints {
            spans.push(Span::styled(*k, p.text()));
            spans.push(Span::styled(format!(" {label}   "), p.muted()));
        }
        f.render_widget(Line::from(spans), rest);
    }

    let (dot, label) = match app.quota {
        Quota::Normal => (p.ok, app.s.quota_ok.to_owned()),
        Quota::Retrying => (p.accent, app.s.quota_retrying.to_owned()),
        Quota::Holding(n) => (p.like, (app.s.quota_hold)(n)),
    };
    let quota = if compact {
        styled("●", Style::new().fg(dot))
    } else {
        Line::from(vec![
            Span::styled(format!("{} ", app.s.quota), p.muted()),
            Span::styled("●", Style::new().fg(dot)),
            Span::styled(format!(" {label}"), p.muted()),
        ])
    };
    f.render_widget(quota.right_aligned(), area);
}

/// A line whose style covers only its text. `Line::styled` styles the whole width
/// it is drawn over, which recoloured everything else on a shared row (the pink
/// quota dot turned the whole narrow status line pink).
fn styled<'a>(text: impl Into<std::borrow::Cow<'a, str>>, style: Style) -> Line<'a> {
    Line::from(Span::styled(text, style))
}

fn row(area: Rect, offset: u16) -> Rect {
    Rect {
        y: area.y + offset,
        height: 1,
        ..area
    }
}

/// Cuts `s` to `width` cells with an ellipsis so neighbouring columns never touch.
fn fit(s: &str, width: u16) -> String {
    let width = usize::from(width);
    if s.chars().count() <= width {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}
