//! The artist page, one design wherever an artist shows: the home artist tab for the
//! playing artist, and any artist opened from search, the library or another page.
//! Discography first: popular and liked tracks as strips, the releases as a cover
//! grid, related artists at the bottom. `tab` moves between sections, `j`/`k` move
//! down and up (into the next section at either end), `h`/`l` sideways.

use std::cell::Cell;
use std::sync::Arc;

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::{App, Page};
use crate::data::{Entry, Kind};
use crate::spotify::meta::{ArtistPage, TrackInfo};
use crate::spotify::worker::Command;

pub const POPULAR: usize = 0;
pub const LIKED: usize = 1;
pub const RELEASES: usize = 2;
pub const RELATED: usize = 3;

#[derive(Default)]
pub struct ArtistView {
    pub section: usize,
    pub selected: [usize; 4],
    /// Cards per grid row at the last render, for moving up and down.
    pub grid_cols: Cell<usize>,
    /// First visible grid row.
    pub grid_row: Cell<usize>,
    /// First visible item of each strip.
    pub strip_start: [Cell<usize>; 4],
}

impl App {
    /// Liked tracks by this artist, in the order they were liked.
    pub fn liked_by(&self, artist_uri: &str) -> Vec<TrackInfo> {
        self.model
            .liked
            .iter()
            .flat_map(|l| l.iter())
            .filter(|t| t.artists.iter().any(|(uri, _)| uri == artist_uri))
            .cloned()
            .collect()
    }

    pub fn artist_section_len(&self, page: &ArtistPage, section: usize) -> usize {
        match section {
            POPULAR => page.top.len(),
            LIKED => self.liked_by(&page.uri).len(),
            RELEASES => page.discography.len(),
            _ => page.related.len(),
        }
    }

    /// The artist page that has the keys: an opened artist, or home's artist tab.
    pub fn active_artist(&self) -> Option<(Arc<ArtistPage>, &ArtistView)> {
        if let Some(open) = self.active_open() {
            return (open.entry.kind == Kind::Artist)
                .then(|| Some((open.artist.clone()?, &open.view)))
                .flatten();
        }
        (self.page == Page::Home && self.tab == 2)
            .then(|| Some((self.model.artist.clone()?, &self.artist_view)))
            .flatten()
    }

    fn active_artist_view_mut(&mut self) -> Option<&mut ArtistView> {
        if self.active_open().is_some() {
            return self.active_open_mut().map(|o| &mut o.view);
        }
        (self.page == Page::Home && self.tab == 2).then_some(&mut self.artist_view)
    }

    /// The selected popular or liked track on the artist page that has the keys.
    pub(super) fn artist_track(&self) -> Option<TrackInfo> {
        let (page, view) = self.active_artist()?;
        let selected = view.selected[view.section];
        match view.section {
            POPULAR => page.top.get(selected).cloned(),
            LIKED => self.liked_by(&page.uri).get(selected).cloned(),
            _ => None,
        }
    }

    /// `g a` / `g b` targets on an artist page: a release, a related artist, or the
    /// selected track's album.
    pub(super) fn artist_go_target(&self, kind: Kind) -> Option<Entry> {
        let (page, view) = self.active_artist()?;
        let selected = view.selected[view.section];
        match (kind, view.section) {
            (Kind::Album, RELEASES) => page
                .discography
                .get(selected)
                .map(|c| Entry::new(Kind::Album, &c.uri, &c.name)),
            (Kind::Artist, RELATED) => page
                .related
                .get(selected)
                .map(|(uri, name)| Entry::new(Kind::Artist, uri, name)),
            (Kind::Album, POPULAR | LIKED) => {
                let (uri, name) = self.artist_track()?.album?;
                Some(Entry::new(Kind::Album, uri, name))
            }
            _ => Some(Entry::new(Kind::Artist, &page.uri, &page.name)),
        }
    }

    /// Keys on an artist page; false for keys it leaves to the page around it.
    pub(super) fn artist_key(&mut self, key: KeyEvent) -> bool {
        let Some((page, view)) = self.active_artist() else {
            return false;
        };
        let lens: Vec<usize> = (0..4).map(|s| self.artist_section_len(&page, s)).collect();
        let section = view.section;
        let selected = view.selected[section];
        let cols = if section == RELEASES {
            view.grid_cols.get().max(1)
        } else {
            1
        };
        let last = lens[section].saturating_sub(1);
        // Moves within the section, or into the neighbouring one past either end.
        let vertical = |down: bool| -> (usize, usize) {
            if down && selected + cols <= last {
                return (section, selected + cols);
            }
            if down && selected / cols < last / cols {
                return (section, last);
            }
            if !down && selected >= cols {
                return (section, selected - cols);
            }
            let next = if down {
                (section + 1..4).find(|&s| lens[s] > 0)
            } else {
                (0..section).rev().find(|&s| lens[s] > 0)
            };
            next.map_or((section, selected), |s| (s, usize::MAX))
        };
        let (to_section, to_item) = match key.code {
            KeyCode::Tab => (next_section(&lens, section, 1), usize::MAX),
            KeyCode::BackTab => (next_section(&lens, section, -1), usize::MAX),
            KeyCode::Char('j') | KeyCode::Down => vertical(true),
            KeyCode::Char('k') | KeyCode::Up => vertical(false),
            KeyCode::Char('l') | KeyCode::Right => (section, (selected + 1).min(last)),
            KeyCode::Char('h') | KeyCode::Left => (section, selected.saturating_sub(1)),
            KeyCode::Home => (section, 0),
            KeyCode::End => (section, last),
            KeyCode::Enter => {
                self.artist_enter(&page, section, selected);
                return true;
            }
            KeyCode::Char('P') => {
                self.notice = Some(format!("▶ {}", page.name));
                self.command(Command::PlayContext(page.uri.clone()));
                return true;
            }
            KeyCode::Char('r') => {
                self.notice = Some((self.s.radio_of)(&page.name));
                self.command(Command::StartRadio(page.uri.clone()));
                return true;
            }
            _ => return false,
        };
        if let Some(view) = self.active_artist_view_mut() {
            view.section = to_section;
            // Entering a section keeps its own last position.
            if to_item != usize::MAX {
                view.selected[to_section] = to_item;
            }
        }
        true
    }

    fn artist_enter(&mut self, page: &ArtistPage, section: usize, selected: usize) {
        match section {
            POPULAR | LIKED => {
                let tracks = if section == POPULAR {
                    page.top.clone()
                } else {
                    self.liked_by(&page.uri)
                };
                let Some(first) = tracks.get(selected) else {
                    return;
                };
                self.notice = Some(format!("▶ {}", first.name));
                // Liked tracks continue through the liked songs; popular ones play in
                // their album (Spotify refuses a starting track in an artist context).
                let liked = self
                    .model
                    .library
                    .as_ref()
                    .filter(|i| section == LIKED && !i.user.is_empty())
                    .map(|i| i.liked_uri());
                self.command(match liked {
                    Some(context) => Command::PlayFrom {
                        context: Some(context),
                        uris: vec![first.uri.clone()],
                    },
                    None => Command::PlayTrack(first.uri.clone()),
                });
            }
            RELEASES => {
                if let Some(card) = page.discography.get(selected) {
                    self.open_here(Entry::new(Kind::Album, &card.uri, &card.name));
                }
            }
            _ => {
                if let Some((uri, name)) = page.related.get(selected) {
                    self.open_here(Entry::new(Kind::Artist, uri, name));
                }
            }
        }
    }
}

/// The next section with something in it, wrapping around.
fn next_section(lens: &[usize], section: usize, delta: isize) -> usize {
    (1..=4)
        .map(|i| (section as isize + delta * i).rem_euclid(4) as usize)
        .find(|&s| lens[s] > 0)
        .unwrap_or(section)
}
