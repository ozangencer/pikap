//! Library and search. The left column keeps "now playing" in a compact form (smaller
//! cover, title under it, no reflection or discovery) and the halo is dimmer, so the
//! lists carry the page. Lists opened from either page show in place.

use std::cell::Cell;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use super::{
    COLUMN_GAP, COMPACT_LEADER_HEIGHT, LEADER_HEIGHT, ListState, WIDE_MIN, below, fit, header_line,
    leader_panel, queue_header, row, scroll, status_line, styled, title_text, track_rows,
    wave_and_times,
};
use crate::app::{App, Opened, Page, Popup};
use crate::data::{Entry, Kind, Track};
use crate::gallery::{CellPx, Slots};

/// Width below which the library shows one pane at a time.
const TWO_PANES: u16 = 70;
/// Search shows its four sections side by side from this size on.
const GRID: (u16, u16) = (80, 16);

pub(super) fn content(
    f: &mut Frame,
    app: &App,
    cell: CellPx,
    title_as_image: bool,
    slots: &mut Slots,
) {
    let screen = slots.screen;
    let wide = screen.width >= WIDE_MIN.0 && screen.height >= WIDE_MIN.1;
    slots.halo_dim = true;
    let outer = screen.inner(Margin {
        horizontal: if wide { 3 } else { 2 },
        vertical: 1,
    });
    let leader_h = match (app.leader, wide) {
        (false, _) => 0,
        (true, true) => LEADER_HEIGHT + 1,
        (true, false) => COMPACT_LEADER_HEIGHT + 1,
    };
    let [header, _, main, leader, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(leader_h),
        Constraint::Length(1),
    ])
    .areas(outer);
    header_line(f, app, header, !wide);

    let body = if wide {
        let cols = now_column(f, app, main, cell, title_as_image, slots);
        Rect {
            x: main.x + cols + COLUMN_GAP,
            width: main.width.saturating_sub(cols + COLUMN_GAP),
            ..main
        }
    } else {
        now_line(f, app, row(main, 0));
        below(main, 2)
    };
    match app.page {
        Page::Library => library(f, app, body, slots),
        Page::Search => search(f, app, body, slots),
        Page::Home => {}
    }

    if app.leader {
        let area = Rect {
            y: leader.y + 1,
            height: leader_h - 1,
            ..leader
        };
        leader_panel(f, app, area, if wide { 4 } else { 2 }, slots);
    }
    status_line(f, app, status, !wide, slots);
}

/// Cover, title, artist, album and the equalizer, stacked; returns the width used.
fn now_column(
    f: &mut Frame,
    app: &App,
    main: Rect,
    cell: CellPx,
    title_as_image: bool,
    slots: &mut Slots,
) -> u16 {
    let p = app.p();
    let max_cols = (main.width / 4).max(24);
    let Some(now) = app.model.now.as_ref() else {
        let cols = max_cols.min(28);
        f.render_widget(
            Paragraph::new(app.s.nothing_playing)
                .style(p.muted())
                .wrap(Wrap { trim: true }),
            Rect {
                width: cols,
                ..main
            },
        );
        return cols;
    };
    // Title (2), gap, artist, album, gap, equalizer and times (3).
    const TEXT_ROWS: u16 = 9;
    let mut rows = main.height.saturating_sub(TEXT_ROWS).min(20);
    let mut cols = (f32::from(rows) * cell.ratio()).round() as u16;
    if cols > max_cols {
        cols = max_cols;
        rows = (f32::from(cols) / cell.ratio()).round() as u16;
    }
    let column = Rect {
        width: cols,
        ..main
    };
    slots.cover = Rect {
        height: rows,
        ..column
    };
    let title = Rect {
        y: slots.cover.bottom() + 1,
        height: 2,
        ..column
    };
    slots.title = title;
    if !title_as_image {
        title_text(f, app, title);
    }
    let heart = if now.liked { "  ♥" } else { "  ♡" };
    let artist = Line::from(vec![
        Span::styled(fit(&now.artist, cols.saturating_sub(3)), p.accent()),
        Span::styled(heart, Style::new().fg(p.like)),
    ]);
    f.render_widget(artist, row(column, title.bottom() - column.y));
    let album = match now.year {
        Some(year) => format!("{} · {year}", now.album),
        None => now.album.clone(),
    };
    f.render_widget(
        Span::styled(fit(&album, cols), p.muted()),
        row(column, title.bottom() + 1 - column.y),
    );
    wave_and_times(
        f,
        app,
        Rect {
            y: title.bottom() + 3,
            height: 3,
            ..column
        },
    );
    cols
}

/// The narrow layout's one-line "now playing".
fn now_line(f: &mut Frame, app: &App, area: Rect) {
    let p = app.p();
    let line = match &app.model.now {
        Some(now) => Line::from(vec![
            Span::styled(if app.playing { "▶ " } else { "▌▌ " }, p.accent()),
            Span::styled(now.title.as_str(), p.text().add_modifier(Modifier::BOLD)),
            Span::styled(format!(" · {}", now.artist), p.muted()),
        ]),
        None => styled(app.s.nothing_playing, p.muted()),
    };
    f.render_widget(line, area);
}

/// Collections on the left, the opened list on the right; one pane when narrow.
fn library(f: &mut Frame, app: &App, area: Rect, slots: &mut Slots) {
    let p = app.p();
    let view = &app.library;
    let rows = app.library_rows();
    if area.width < TWO_PANES {
        match view.open.as_ref().filter(|_| view.in_list) {
            Some(open) => opened(f, app, area, open, true, slots),
            None => entry_rows(
                f,
                app,
                area,
                &rows,
                Some(view.selected),
                &view.offset,
                slots,
            ),
        }
        return;
    }
    let left_w = (area.width * 2 / 5).clamp(28, 44);
    let left = Rect {
        width: left_w,
        ..area
    };
    let right = Rect {
        x: area.x + left_w + 4,
        width: area.width.saturating_sub(left_w + 4),
        ..area
    };
    let selected = (!view.in_list).then_some(view.selected);
    entry_rows(f, app, left, &rows, selected, &view.offset, slots);
    for y in 0..area.height {
        f.render_widget(
            Span::styled("│", p.line()),
            Rect {
                x: area.x + left_w + 2,
                y: area.y + y,
                width: 1,
                height: 1,
            },
        );
    }
    match &view.open {
        Some(open) => opened(f, app, right, open, view.in_list, slots),
        None => f.render_widget(styled(app.s.open_hint, p.muted()), row(right, 0)),
    }
}

/// A list opened from the library or search: its name and count, then the tracks.
pub(super) fn opened(
    f: &mut Frame,
    app: &App,
    area: Rect,
    open: &Opened,
    focused: bool,
    slots: &mut Slots,
) {
    let p = app.p();
    if let Some(page) = open
        .artist
        .as_ref()
        .filter(|_| open.entry.kind == Kind::Artist)
    {
        return super::artist::artist_page(f, app, area, page, &open.view, slots);
    }
    let title = app.entry_title(&open.entry);
    let Some(page) = &open.page else {
        f.render_widget(
            styled(
                fit(&title, area.width),
                p.text().add_modifier(Modifier::BOLD),
            ),
            row(area, 0),
        );
        f.render_widget(styled(app.s.loading, p.muted()), row(area, 2));
        return;
    };
    let heading = match open.entry.kind {
        Kind::Artist => (app.s.artist_top)(&title),
        _ => format!("{title} · {}", (app.s.track_count)(page.tracks.len())),
    };
    f.render_widget(
        styled(
            fit(&heading, area.width),
            p.text().add_modifier(Modifier::BOLD),
        ),
        row(area, 0),
    );
    let with_artist = area.width >= 50;
    queue_header(f, app, row(area, 2), with_artist);
    let rows: Vec<Track> = app.opened_rows(open).iter().map(Track::from).collect();
    let state = ListState {
        selected: focused.then_some(open.selected),
        offset: &open.offset,
        queue: false,
        with_artist,
    };
    track_rows(f, app, below(area, 3), &rows, &state, slots);
}

/// The query line, then the results: four numbered sections at once when there is
/// room, otherwise the focused one under a row of section names.
fn search(f: &mut Frame, app: &App, area: Rect, slots: &mut Slots) {
    let p = app.p();
    let view = &app.search;
    let input = row(area, 0);
    let mut spans = vec![Span::styled("/ ", p.key())];
    if view.query.is_empty() {
        spans.push(Span::styled(app.s.search_prompt, p.muted()));
    } else {
        spans.push(Span::styled(
            view.query.as_str(),
            Style::new().fg(p.bright).add_modifier(Modifier::BOLD),
        ));
    }
    if view.typing {
        spans.push(Span::styled("▏", p.accent()));
    }
    f.render_widget(Line::from(spans), input);
    let limit = app.s.search_limit;
    if input.width > 60 + limit.chars().count() as u16 {
        f.render_widget(styled(limit, p.muted()).right_aligned(), input);
    }
    f.render_widget(
        styled("─".repeat(usize::from(area.width)), p.line()),
        row(area, 1),
    );
    let body = below(area, 3);
    if let Some(open) = &view.open {
        opened(f, app, body, open, true, slots);
        return;
    }
    let note = |f: &mut Frame, text: &str| {
        f.render_widget(styled(fit(text, body.width), p.muted()), row(body, 0));
    };
    if view.busy {
        return note(f, app.s.searching);
    }
    if view.results.is_none() {
        return;
    }
    if body.width >= GRID.0 && body.height >= GRID.1 {
        let [top, _, bottom] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(body);
        for (i, half) in [top, bottom].into_iter().enumerate() {
            let [left, _, right] = Layout::horizontal([
                Constraint::Fill(1),
                Constraint::Length(4),
                Constraint::Fill(1),
            ])
            .areas(half);
            section(f, app, left, 2 * i, slots);
            section(f, app, right, 2 * i + 1, slots);
        }
        return;
    }
    let mut tabs = Vec::new();
    for (i, label) in app.s.search_sections.iter().enumerate() {
        tabs.push(Span::styled(format!("{} ", i + 1), p.key()));
        tabs.push(Span::styled(*label, section_style(app, i)));
        tabs.push(Span::raw("   "));
    }
    f.render_widget(Line::from(tabs), row(body, 0));
    rows_of(f, app, below(body, 2), view.section, slots);
}

fn section_style(app: &App, section: usize) -> Style {
    if section == app.search.section {
        app.p()
            .text()
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        app.p().muted()
    }
}

/// One numbered search section with its heading.
fn section(f: &mut Frame, app: &App, area: Rect, section: usize, slots: &mut Slots) {
    let p = app.p();
    let count = app.search.section_len(section);
    let heading = Line::from(vec![
        Span::styled(format!("{} ", section + 1), p.key()),
        Span::styled(app.s.search_sections[section], section_style(app, section)),
        Span::styled(format!("  {count}"), p.muted()),
    ]);
    f.render_widget(heading, row(area, 0));
    rows_of(f, app, below(area, 2), section, slots);
}

fn rows_of(f: &mut Frame, app: &App, area: Rect, section: usize, slots: &mut Slots) {
    let view = &app.search;
    let selected = (section == view.section).then_some(view.selected[section]);
    if view.section_len(section) == 0 {
        f.render_widget(styled(app.s.no_results, app.p().muted()), row(area, 0));
        return;
    }
    if section == 0 {
        let rows: Vec<Track> = view
            .results
            .as_ref()
            .map_or_else(Vec::new, |r| r.tracks.iter().map(Track::from).collect());
        let state = ListState {
            selected,
            offset: &view.offsets[0],
            queue: false,
            with_artist: area.width >= 44,
        };
        track_rows(f, app, area, &rows, &state, slots);
    } else {
        let rows = view.entries(section);
        entry_rows(f, app, area, rows, selected, &view.offsets[section], slots);
    }
}

/// Playlists, albums, artists and library lists: an icon, the name, and on the right
/// the track count or the artist. Heading rows title a section; empty ones space it.
fn entry_rows(
    f: &mut Frame,
    app: &App,
    area: Rect,
    rows: &[Entry],
    selected: Option<usize>,
    offset: &Cell<Option<usize>>,
    slots: &mut Slots,
) {
    let p = app.p();
    let height = usize::from(area.height);
    let first = scroll(offset, selected, rows.len(), height);
    for (y, (i, e)) in rows.iter().enumerate().skip(first).take(height).enumerate() {
        let r = row(area, y as u16);
        if e.kind == Kind::Heading {
            if !e.title.is_empty() {
                let title = format!("{}  ", e.title);
                let rule = usize::from(r.width).saturating_sub(title.chars().count());
                let heading = Line::from(vec![
                    Span::styled(title, p.accent().add_modifier(Modifier::BOLD)),
                    Span::styled("─".repeat(rule), p.line()),
                ]);
                f.render_widget(heading, r);
            }
            continue;
        }
        let is_selected = selected == Some(i);
        if is_selected {
            slots.fill(r, p.selected);
            f.render_widget(Span::styled("▎", p.accent()), r);
        }
        let (icon, icon_style) = match e.kind {
            Kind::Liked => ("♥", Style::new().fg(p.like)),
            Kind::Recent => ("↺", p.accent()),
            Kind::Album => ("◎", p.muted()),
            Kind::Artist => ("✦", p.muted()),
            Kind::Playlist | Kind::Heading => ("≡", p.muted()),
        };
        let right = match e.kind {
            Kind::Album => e.sub.clone(),
            _ => e.count.map(app.s.track_count).unwrap_or_default(),
        };
        let right_w = (right.chars().count() as u16).min(r.width / 3);
        let title = match e.kind {
            Kind::Liked | Kind::Recent => app.entry_title(e),
            _ if e.title.is_empty() => "…".to_owned(),
            _ => e.title.clone(),
        };
        let style = if is_selected {
            p.text().add_modifier(Modifier::BOLD)
        } else {
            p.text()
        };
        let room = r.width.saturating_sub(right_w + 7);
        f.render_widget(
            Line::from(vec![
                Span::styled(format!("  {icon} "), icon_style),
                Span::styled(fit(&title, room), style),
            ]),
            r,
        );
        f.render_widget(styled(fit(&right, right_w), p.muted()).right_aligned(), r);
    }
}

/// The popups (add to playlist, queue, devices), centred over everything else.
pub(super) fn popup(f: &mut Frame, app: &App, slots: &mut Slots) {
    let p = app.p();
    match &app.popup {
        Some(Popup::AddTo {
            track,
            selected,
            offset,
        }) => {
            let lists = app.editable_playlists();
            let name = format!("{} · {}", track.title, track.artist);
            let list = frame(
                f,
                app,
                slots,
                60,
                lists.len(),
                app.s.add_title,
                &name,
                app.s.add_hint,
            );
            entry_rows(f, app, list, &lists, Some(*selected), offset, slots);
        }
        Some(Popup::Queue { selected, offset }) => {
            let queue = &app.model.queue;
            let detail = (app.s.track_count)(queue.len().saturating_sub(1));
            let list = frame(
                f,
                app,
                slots,
                84,
                queue.len(),
                app.s.queue_title,
                &detail,
                app.s.queue_hint,
            );
            let state = ListState {
                selected: Some(*selected),
                offset,
                queue: true,
                with_artist: true,
            };
            track_rows(f, app, list, queue, &state, slots);
            let shown = (queue.len() as u16).min(list.height);
            if app.model.queue_loops && shown < list.height {
                f.render_widget(styled(app.s.queue_loops, p.muted()), row(list, shown));
            }
        }
        Some(Popup::Devices { selected, offset }) => {
            let devices = app.model.devices.as_deref();
            let count = devices.map_or(1, |d| d.len().max(1));
            let list = frame(
                f,
                app,
                slots,
                64,
                count,
                app.s.devices_title,
                "",
                app.s.devices_hint,
            );
            match devices {
                None => f.render_widget(styled(app.s.loading, p.muted()), row(list, 0)),
                Some([]) => f.render_widget(
                    styled(fit(app.s.no_devices, list.width), p.muted()),
                    row(list, 0),
                ),
                Some(devices) => device_rows(f, app, list, devices, *selected, offset, slots),
            }
        }
        _ => {}
    }
}

/// A bordered panel sized for `rows`, with a tag and a detail on top and a hint at
/// the bottom; returns the area for the rows.
#[allow(clippy::too_many_arguments)]
fn frame(
    f: &mut Frame,
    app: &App,
    slots: &mut Slots,
    width: u16,
    rows: usize,
    title: &str,
    detail: &str,
    hint: &str,
) -> Rect {
    let p = app.p();
    let screen = slots.screen;
    let w = width.min(screen.width.saturating_sub(4));
    let h = (rows as u16 + 6).clamp(8, screen.height.saturating_sub(4));
    let area = Rect {
        x: screen.x + (screen.width - w) / 2,
        y: screen.y + (screen.height - h) / 2,
        width: w,
        height: h,
    };
    // Surfaces of the page underneath would show through the panel.
    slots.surfaces.retain(|(r, _)| !r.intersects(area));
    slots.cards.retain(|(r, _)| !r.intersects(area));
    slots.hole = area;
    f.render_widget(Clear, area);
    slots.fill(area, p.panel);
    let block = Block::bordered().border_style(p.line());
    let inner = block.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    f.render_widget(block, area);

    let tag_w = title.chars().count() as u16;
    let tag = Rect {
        width: tag_w.min(inner.width),
        ..row(inner, 0)
    };
    slots.fill(tag, p.tag);
    f.render_widget(Span::styled(title.to_owned(), p.tag()), tag);
    let rest = Rect {
        x: tag.right() + 1,
        width: inner.width.saturating_sub(tag_w + 1),
        ..row(inner, 0)
    };
    f.render_widget(Span::styled(fit(detail, rest.width), p.muted()), rest);
    f.render_widget(
        styled(fit(hint, inner.width), p.muted()),
        row(inner, inner.height.saturating_sub(1)),
    );
    Rect {
        y: inner.y + 2,
        height: inner.height.saturating_sub(4),
        ..inner
    }
}

/// Connect devices: name, kind and volume, the active one marked.
fn device_rows(
    f: &mut Frame,
    app: &App,
    area: Rect,
    devices: &[crate::spotify::model::Device],
    selected: usize,
    offset: &Cell<Option<usize>>,
    slots: &mut Slots,
) {
    let p = app.p();
    let height = usize::from(area.height);
    let first = scroll(offset, Some(selected), devices.len(), height);
    for (y, (i, d)) in devices
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .enumerate()
    {
        let r = row(area, y as u16);
        let is_selected = i == selected;
        if is_selected {
            slots.fill(r, p.selected);
            f.render_widget(Span::styled("▎", p.accent()), r);
        }
        let (mark, style) = if d.is_active {
            ("▶", p.accent().add_modifier(Modifier::BOLD))
        } else if d.is_restricted || d.id.is_none() {
            ("·", p.muted())
        } else {
            ("▣", p.text())
        };
        let right = match (d.is_active, d.volume_percent) {
            (true, Some(v)) => format!("{} · {v}%", app.s.active),
            (true, None) => app.s.active.to_owned(),
            (false, Some(v)) => format!("{v}%"),
            (false, None) => String::new(),
        };
        let kind = (app.s.device_kind)(&d.kind);
        let room = r.width.saturating_sub(right.chars().count() as u16 + 6);
        f.render_widget(
            Line::from(vec![
                Span::styled(format!("  {mark} "), style),
                Span::styled(
                    fit(
                        &d.name,
                        room.saturating_sub(kind.chars().count() as u16 + 3),
                    ),
                    style,
                ),
                Span::styled(format!("  {kind}"), p.muted()),
            ]),
            r,
        );
        f.render_widget(styled(right, p.muted()).right_aligned(), r);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::App;
    use crate::data::{Entry, Kind, LibraryIndex, SearchResults};
    use crate::gallery::CellPx;
    use crate::i18n::Lang;
    use crate::spotify::meta::{ListPage, TrackInfo};
    use crate::spotify::worker::Update;
    use crate::theme::Mode;

    fn track(name: &str, artist: &str) -> TrackInfo {
        TrackInfo {
            uri: format!("spotify:track:{name}"),
            name: name.into(),
            artists: vec![(String::new(), artist.into())],
            duration: std::time::Duration::from_secs(200),
            album: None,
        }
    }

    fn demo_app() -> App {
        let mut app = App::new(Lang::Tr, Mode::Dark, crate::data::demo(), None);
        let list = |name: &str, n: usize, editable: bool| Entry {
            editable,
            count: Some(n),
            ..Entry::new(Kind::Playlist, format!("spotify:playlist:{name}"), name)
        };
        app.model.library = Some(LibraryIndex {
            user: "u".into(),
            made_for_you: vec![
                list("Daily Mix 1", 50, false),
                list("Discover Weekly", 30, false),
            ],
            playlists: vec![list("yol şarkıları", 52, true), list("odak", 18, true)],
            albums: vec![Entry {
                sub: "Gorillaz".into(),
                ..Entry::new(Kind::Album, "spotify:album:pb", "Plastic Beach")
            }],
            held: None,
        });
        app.model.liked = Some(Arc::new(vec![track("Stylo", "Gorillaz"); 728]));
        app
    }

    fn press(app: &mut App, keys: &str) {
        for c in keys.chars() {
            let code = match c {
                '\n' => KeyCode::Enter,
                '\t' => KeyCode::Tab,
                c => KeyCode::Char(c),
            };
            app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
    }

    fn show(app: &App, w: u16, h: u16) {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| drop(super::super::render(f, app, CellPx { w: 8, h: 16 }, false)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        for y in 0..h {
            let line: String = (0..w).map(|x| buffer[(x, y)].symbol().to_owned()).collect();
            println!("{}", line.trim_end());
        }
        println!("{}", "=".repeat(usize::from(w)));
    }

    /// `cargo test --release -- --ignored --nocapture snapshots` prints the pages.
    #[test]
    #[ignore = "prints screens for a human to look at"]
    fn snapshots() {
        let mut app = demo_app();
        press(&mut app, "gl");
        show(&app, 150, 40);
        press(&mut app, "jjjjj\n");
        app.apply(Update::List(Arc::new(ListPage {
            uri: "spotify:playlist:yol şarkıları".into(),
            name: "yol şarkıları".into(),
            tracks: vec![track("Stylo", "Gorillaz"), track("Loser", "Beck")],
        })));
        show(&app, 150, 40);
        press(&mut app, "a");
        show(&app, 150, 40);
        press(&mut app, "\x1b");
        app.popup = None;
        show(&app, 60, 30);
        press(&mut app, "/blur\n");
        app.apply(Update::Search(Arc::new(SearchResults {
            query: "blur".into(),
            tracks: (0..10)
                .map(|i| track(&format!("Song {i}"), "Blur"))
                .collect(),
            albums: vec![Entry {
                sub: "Blur".into(),
                ..Entry::new(Kind::Album, "spotify:album:b", "Parklife")
            }],
            artists: vec![Entry::new(Kind::Artist, "spotify:artist:b", "Blur")],
            playlists: Vec::new(),
        })));
        show(&app, 150, 40);
        show(&app, 60, 30);
    }

    /// A right-aligned line drawn over a shared row must not recolour the rest of it.
    #[test]
    fn narrow_status_line_keeps_its_colours() {
        let mut app = App::new(Lang::Tr, Mode::Dark, crate::data::demo(), None);
        app.quota = crate::spotify::api::Quota::Holding(3);
        let mut terminal = Terminal::new(TestBackend::new(60, 30)).unwrap();
        terminal
            .draw(|f| drop(super::super::render(f, &app, CellPx { w: 8, h: 16 }, false)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let p = app.p();
        let tag = (0..60)
            .map(|x| &buffer[(x, 28)])
            .find(|c| c.symbol() == "g")
            .expect("the g tag on the status line");
        assert_eq!(tag.fg, p.on_accent, "not the quota dot's colour");
        assert_eq!(
            buffer[(57, 28)].fg,
            p.like,
            "the dot itself keeps its colour"
        );
    }

    /// The narrow layout follows the tab keys instead of always showing the list.
    #[test]
    fn narrow_layout_shows_the_chosen_tab() {
        use crate::spotify::meta::{LyricLine, LyricsPage};
        let screen = |app: &App| {
            let mut terminal = Terminal::new(TestBackend::new(70, 50)).unwrap();
            terminal
                .draw(|f| drop(super::super::render(f, app, CellPx { w: 8, h: 16 }, false)))
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..50)
                .map(|y| (0..70).map(|x| buffer[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut app = App::new(Lang::En, Mode::Dark, crate::data::demo(), None);
        app.model.lyrics = Some(Some(Arc::new(LyricsPage {
            synced: false,
            lines: vec![LyricLine {
                start: std::time::Duration::ZERO,
                words: "la la lyric line".into(),
            }],
        })));
        let first = app.list_rows()[0].title.clone();
        assert!(screen(&app).contains(&first), "starts on the list");
        press(&mut app, "gL");
        let lyrics = screen(&app);
        assert!(lyrics.contains("la la lyric line"), "g L shows the lyrics");
        assert!(!lyrics.contains(&first), "the list is gone");
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.tab, 0);
        assert!(screen(&app).contains(&first), "← goes back to the list");
    }

    /// Without kitty graphics, surfaces turn into cell backgrounds.
    #[test]
    fn surfaces_become_backgrounds_without_graphics() {
        let mut app = App::new(Lang::En, Mode::Dark, crate::data::demo(), None);
        app.graphics = false;
        let mut terminal = Terminal::new(TestBackend::new(150, 40)).unwrap();
        terminal
            .draw(|f| drop(super::super::render(f, &app, CellPx { w: 8, h: 16 }, false)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let tag = (0..150)
            .map(|x| &buffer[(x, 38)])
            .find(|c| c.symbol() == "g")
            .expect("the g tag on the status line");
        assert_ne!(tag.bg, ratatui::style::Color::Reset, "the tag is filled");
    }
}
