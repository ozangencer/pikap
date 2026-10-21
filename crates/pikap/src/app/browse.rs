//! The two content pages, library and search, and the playlist popups. Anything the
//! home screen already shows (lyrics, artist, album, discover) stays a home tab; these
//! pages only add what home cannot: the user's lists and search results. A list opened
//! from either page shows in place and `⌫` closes it.

use std::cell::Cell;
use std::sync::Arc;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, ArtistView};
use crate::data::{Entry, Kind, SearchResults, Track};
use crate::spotify::meta::{ArtistPage, ListPage, TrackInfo};
use crate::spotify::worker::Command;

/// URI of the "recently played" library row; the Web API has no URI for it.
pub const RECENT_URI: &str = "pikap:recent";
const HISTORY: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Home,
    Library,
    Search,
}

/// A list opened from the library or search.
pub struct Opened {
    pub entry: Entry,
    pub page: Option<Arc<ListPage>>,
    pub selected: usize,
    pub offset: Cell<Option<usize>>,
    /// An opened artist's page and its sections.
    pub artist: Option<Arc<ArtistPage>>,
    pub view: ArtistView,
}

impl Opened {
    fn new(entry: Entry) -> Self {
        Self {
            entry,
            page: None,
            selected: 0,
            offset: Cell::new(None),
            artist: None,
            view: ArtistView::default(),
        }
    }
}

#[derive(Default)]
pub struct LibraryView {
    pub selected: usize,
    pub offset: Cell<Option<usize>>,
    /// Keys move in the opened list rather than in the collections.
    pub in_list: bool,
    pub open: Option<Opened>,
    /// Lists `open` replaced through `g a` / `g b`; `⌫` returns to them.
    pub trail: Vec<Opened>,
}

#[derive(Default)]
pub struct SearchView {
    pub query: String,
    pub typing: bool,
    pub busy: bool,
    pub results: Option<Arc<SearchResults>>,
    /// Focused section: 0 tracks, 1 albums, 2 artists, 3 playlists.
    pub section: usize,
    pub selected: [usize; 4],
    pub offsets: [Cell<Option<usize>>; 4],
    pub open: Option<Opened>,
    pub trail: Vec<Opened>,
}

impl SearchView {
    pub fn section_len(&self, section: usize) -> usize {
        self.results.as_ref().map_or(0, |r| match section {
            0 => r.tracks.len(),
            1 => r.albums.len(),
            2 => r.artists.len(),
            _ => r.playlists.len(),
        })
    }

    pub fn entries(&self, section: usize) -> &[Entry] {
        self.results.as_deref().map_or(&[], |r| match section {
            1 => &r.albums,
            2 => &r.artists,
            3 => &r.playlists,
            _ => &[],
        })
    }
}

pub enum Popup {
    /// Pick one of the user's editable playlists for `track`.
    AddTo {
        track: Track,
        selected: usize,
        offset: Cell<Option<usize>>,
    },
    /// Confirm removing `track` from the playlist (uri, name).
    Remove {
        playlist: (String, String),
        track: Track,
    },
    /// Spotify's queue; the first row is the playing track.
    Queue {
        selected: usize,
        offset: Cell<Option<usize>>,
    },
    /// Connect devices to move playback to.
    Devices {
        selected: usize,
        offset: Cell<Option<usize>>,
    },
}

impl App {
    /// Collections down the library's left side: Made For You, liked and recent, then
    /// the user's playlists and saved albums, each under a heading.
    pub fn library_rows(&self) -> Vec<Entry> {
        let s = self.s;
        let index = self.model.library.as_ref();
        let mut rows = Vec::new();
        if let Some(index) = index.filter(|i| !i.made_for_you.is_empty()) {
            rows.push(Entry::heading(s.made_for_you));
            rows.extend(index.made_for_you.iter().cloned());
            rows.push(Entry::heading(""));
        }
        rows.push(Entry::heading(s.your_library));
        rows.push(Entry {
            count: self.model.liked.as_ref().map(|l| l.len()),
            ..Entry::new(
                Kind::Liked,
                index.map_or_else(|| "pikap:liked".to_owned(), |i| i.liked_uri()),
                s.liked_songs,
            )
        });
        rows.push(Entry::new(Kind::Recent, RECENT_URI, s.recently_played));
        let Some(index) = index else {
            rows.push(Entry::heading(""));
            rows.push(Entry::heading(s.loading));
            return rows;
        };
        if !index.playlists.is_empty() {
            rows.push(Entry::heading(""));
            rows.push(Entry::heading(format!(
                "{} · {}",
                s.playlists,
                index.playlists.len()
            )));
            rows.extend(index.playlists.iter().cloned());
        }
        if let Some(held) = index.held {
            rows.push(Entry::heading(""));
            rows.push(Entry::heading((s.library_held)(held)));
        }
        if !index.albums.is_empty() {
            rows.push(Entry::heading(""));
            rows.push(Entry::heading(format!(
                "{} · {}",
                s.albums,
                index.albums.len()
            )));
            rows.extend(index.albums.iter().cloned());
        }
        rows
    }

    /// The user's playlists that accept edits, for the add popup.
    pub fn editable_playlists(&self) -> Vec<Entry> {
        self.model.library.as_ref().map_or_else(Vec::new, |i| {
            i.playlists.iter().filter(|e| e.editable).cloned().collect()
        })
    }

    pub(super) fn goto(&mut self, page: Page) {
        if page == self.page {
            return;
        }
        self.history.push(self.page);
        if self.history.len() > HISTORY {
            self.history.remove(0);
        }
        self.page = page;
        if page == Page::Library
            && self
                .library_rows()
                .get(self.library.selected)
                .is_none_or(|e| e.kind == Kind::Heading)
        {
            self.library.selected = first_entry(&self.library_rows(), 0);
        }
    }

    /// One step back: close what the page has open, otherwise the previous page.
    pub(super) fn back(&mut self) {
        match self.page {
            Page::Home if self.home_open.is_some() => self.home_open = self.home_trail.pop(),
            Page::Library if self.library.in_list => match self.library.trail.pop() {
                Some(previous) => self.library.open = Some(previous),
                None => self.library.in_list = false,
            },
            Page::Search if self.search.open.is_some() => {
                self.search.open = self.search.trail.pop();
            }
            // Home with nowhere to go back to: back to choosing a list.
            Page::Home if self.history.is_empty() => {
                self.goto(Page::Library);
                self.history.clear();
                self.library.in_list = false;
            }
            _ => {
                self.page = self.history.pop().unwrap_or(Page::Home);
                if self.page == Page::Home {
                    self.history.clear();
                }
            }
        }
    }

    /// `/`: a new search, the previous query cleared (`i` edits it instead).
    pub(super) fn open_search(&mut self) {
        self.goto(Page::Search);
        self.search.query.clear();
        self.search.open = None;
        self.search.trail.clear();
        self.search.typing = true;
    }

    /// `g y`, `g r`: the library with liked songs or recently played open.
    pub(super) fn open_library_kind(&mut self, kind: Kind) {
        self.goto(Page::Library);
        let rows = self.library_rows();
        if let Some(i) = rows.iter().position(|e| e.kind == kind) {
            self.library.selected = i;
            self.library.offset.set(None);
            self.open_in_library(rows[i].clone());
        }
    }

    /// `g m`: the library with the cursor on the first Made For You list.
    pub(super) fn show_made_for_you(&mut self) {
        self.goto(Page::Library);
        self.library.in_list = false;
        let has = self
            .model
            .library
            .as_ref()
            .is_some_and(|i| !i.made_for_you.is_empty());
        if has {
            self.library.selected = first_entry(&self.library_rows(), 0);
            self.library.offset.set(None);
        } else {
            self.notice = Some(self.s.no_made_for_you.to_owned());
        }
    }

    fn open_in_library(&mut self, entry: Entry) {
        self.library.in_list = true;
        self.library.trail.clear();
        if self
            .library
            .open
            .as_ref()
            .is_some_and(|o| o.entry.uri == entry.uri)
        {
            return;
        }
        self.command(Command::OpenList(entry.kind, entry.uri.clone()));
        self.library.open = Some(Opened::new(entry));
    }

    pub(super) fn library_key(&mut self, key: KeyEvent) -> bool {
        if self.library.in_list && self.library.open.is_some() {
            let artist = self
                .library
                .open
                .as_ref()
                .is_some_and(|o| o.entry.kind == Kind::Artist);
            // An artist page uses tab and arrows for its own sections; ⌫ returns left.
            if !artist && matches!(key.code, KeyCode::Tab | KeyCode::BackTab | KeyCode::Left) {
                self.library.in_list = false;
                return true;
            }
            return self.open_list_key(key);
        }
        let rows = self.library_rows();
        let last = rows.len().saturating_sub(1);
        let heading = |i: usize| rows.get(i).is_some_and(|e| e.kind == Kind::Heading);
        let selected = self.library.selected;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.library.selected = step(selected, 1, last, heading);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.library.selected = step(selected, -1, last, heading);
            }
            KeyCode::Home => self.library.selected = first_entry(&rows, 0),
            KeyCode::End => self.library.selected = step(last + 1, -1, last, heading),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Right => {
                self.library.in_list = self.library.open.is_some();
            }
            KeyCode::Enter => {
                if let Some(entry) = rows.get(selected).filter(|e| e.kind != Kind::Heading) {
                    self.open_in_library(entry.clone());
                }
            }
            KeyCode::Char('P') => {
                if let Some(entry) = rows.get(selected).filter(|e| e.kind != Kind::Heading) {
                    self.play_all(&entry.clone());
                }
            }
            _ => return false,
        }
        true
    }

    /// Keys while typing a search: letters edit the query, `↵` searches, `esc` stops.
    pub(super) fn typing_key(&mut self, key: KeyEvent) {
        let search = &mut self.search;
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                search.query.push(c);
            }
            KeyCode::Backspace => {
                search.query.pop();
            }
            KeyCode::Enter => {
                let query = search.query.trim().to_owned();
                if !query.is_empty() {
                    search.typing = false;
                    search.busy = true;
                    self.command(Command::Search(query));
                }
            }
            KeyCode::Esc if search.query.is_empty() && search.results.is_none() => self.back(),
            KeyCode::Esc | KeyCode::Down | KeyCode::Tab => search.typing = false,
            _ => {}
        }
    }

    pub(super) fn search_key(&mut self, key: KeyEvent) -> bool {
        if self.search.open.is_some() {
            if key.code == KeyCode::Char('/') {
                self.open_search();
                return true;
            }
            return self.open_list_key(key);
        }
        let search = &mut self.search;
        let section = search.section;
        let last = search.section_len(section).saturating_sub(1);
        match key.code {
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Char('i') => search.typing = true,
            KeyCode::Char(c @ '1'..='4') => search.section = c as usize - '1' as usize,
            KeyCode::Tab => search.section = next_section(search, 1),
            // The sections sit two by two: sideways is the other column.
            KeyCode::Left | KeyCode::Right => search.section ^= 1,
            KeyCode::BackTab => search.section = next_section(search, -1),
            KeyCode::Char('j') | KeyCode::Down => {
                search.selected[section] = (search.selected[section] + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                search.selected[section] = search.selected[section].saturating_sub(1);
            }
            KeyCode::Home => search.selected[section] = 0,
            KeyCode::End => search.selected[section] = last,
            KeyCode::Enter | KeyCode::Char('P') => {
                let play_all = key.code == KeyCode::Char('P');
                let selected = search.selected[section];
                if section == 0 {
                    let track = search.results.as_ref().and_then(|r| r.tracks.get(selected));
                    if let Some(track) = track.cloned() {
                        self.notice = Some(format!("▶ {}", track.name));
                        self.command(Command::PlayTrack(track.uri));
                    }
                } else if let Some(entry) = search.entries(section).get(selected).cloned() {
                    if play_all {
                        self.play_all(&entry);
                    } else {
                        self.command(Command::OpenList(entry.kind, entry.uri.clone()));
                        self.search.trail.clear();
                        self.search.open = Some(Opened::new(entry));
                    }
                }
            }
            _ => return false,
        }
        true
    }

    /// The list opened on this page, when it has the keys.
    pub(super) fn active_open(&self) -> Option<&Opened> {
        match self.page {
            Page::Home => self.home_open.as_ref(),
            Page::Library if self.library.in_list => self.library.open.as_ref(),
            Page::Search => self.search.open.as_ref(),
            _ => None,
        }
    }

    pub(super) fn active_open_mut(&mut self) -> Option<&mut Opened> {
        match self.page {
            Page::Home => self.home_open.as_mut(),
            Page::Library if self.library.in_list => self.library.open.as_mut(),
            Page::Search => self.search.open.as_mut(),
            _ => None,
        }
    }

    /// Rows of an opened list; an artist is a page, not a list.
    pub fn opened_rows(&self, open: &Opened) -> Vec<TrackInfo> {
        match &open.page {
            Some(page) if open.entry.kind != Kind::Artist => page.tracks.clone(),
            _ => Vec::new(),
        }
    }

    /// Keys in an opened list; false for keys it leaves to the page.
    pub(super) fn open_list_key(&mut self, key: KeyEvent) -> bool {
        let Some(open) = self.active_open() else {
            return false;
        };
        if open.entry.kind == Kind::Artist {
            return self.artist_key(key);
        }
        let rows = self.opened_rows(open);
        let (entry, selected) = (open.entry.clone(), open.selected);
        let last = rows.len().saturating_sub(1);
        let heading = |i: usize| rows.get(i).is_some_and(|r| r.uri.is_empty());
        let next = match key.code {
            KeyCode::Char('j') | KeyCode::Down => step(selected, 1, last, heading),
            KeyCode::Char('k') | KeyCode::Up => step(selected, -1, last, heading),
            KeyCode::Home => 0,
            KeyCode::End => last,
            KeyCode::Enter => {
                let tracks: Vec<Track> = rows.iter().map(Track::from).collect();
                self.play_from(&entry, &tracks, selected);
                return true;
            }
            KeyCode::Char('P') => {
                self.play_all(&entry);
                return true;
            }
            _ => return false,
        };
        if let Some(open) = self.active_open_mut() {
            open.selected = next;
        }
        true
    }

    /// The selected track of the list that has the keys on a content page.
    fn selected_info(&self) -> Option<TrackInfo> {
        if self.active_artist().is_some() {
            return self.artist_track();
        }
        let in_open = |o: &Opened| {
            self.opened_rows(o)
                .get(o.selected)
                .filter(|t| t.uri.starts_with("spotify:track:"))
                .cloned()
        };
        match self.page {
            Page::Home => None,
            Page::Library if self.library.in_list => self.library.open.as_ref().and_then(in_open),
            Page::Library => None,
            Page::Search => match &self.search.open {
                Some(open) => in_open(open),
                None if self.search.section == 0 => self
                    .search
                    .results
                    .as_ref()?
                    .tracks
                    .get(self.search.selected[0])
                    .cloned(),
                None => None,
            },
        }
    }

    /// The selected row on a content page when it is an entry (artist, album, list).
    fn selected_entry(&self) -> Option<Entry> {
        match self.page {
            Page::Library if !self.library.in_list => {
                self.library_rows().get(self.library.selected).cloned()
            }
            Page::Search if self.search.open.is_none() && self.search.section > 0 => {
                let section = self.search.section;
                self.search
                    .entries(section)
                    .get(self.search.selected[section])
                    .cloned()
            }
            _ => None,
        }
    }

    /// `g a` / `g b` on a content page: the artist or album of whatever row is selected
    /// (a track, an album, an artist), opened in place.
    fn go_target(&self, kind: Kind) -> Option<Entry> {
        if self.active_artist().is_some() {
            return self.artist_go_target(kind);
        }
        if let Some(entry) = self.selected_entry() {
            return match (kind, entry.kind) {
                (Kind::Artist, Kind::Artist) | (Kind::Album, Kind::Album) => Some(entry),
                (Kind::Artist, Kind::Album) => entry
                    .artist
                    .map(|(uri, name)| Entry::new(Kind::Artist, uri, name)),
                _ => None,
            };
        }
        let track = self.selected_info()?;
        let (uri, name) = match kind {
            Kind::Artist => track.artists.into_iter().find(|(uri, _)| !uri.is_empty())?,
            _ => track.album?,
        };
        Some(Entry::new(kind, uri, name))
    }

    /// `g a`: at home the playing track's artist tab; on a content page the selected
    /// row's artist, opened in place.
    pub(super) fn go_to_artist(&mut self) {
        self.go_to(Kind::Artist, 2);
    }

    /// `g b`: like `g a`, for the album.
    pub(super) fn go_to_album(&mut self) {
        self.go_to(Kind::Album, 3);
    }

    fn go_to(&mut self, kind: Kind, home_tab: usize) {
        let on_page = self.page != Page::Home || self.active_open().is_some();
        if !on_page && self.active_artist().is_none() {
            return self.show_tab(home_tab);
        }
        let current = self.active_artist().map(|(page, _)| page.uri.clone());
        match self.go_target(kind) {
            Some(entry) if Some(&entry.uri) == current.as_ref() => {
                self.notice = Some((self.s.already_open)(&entry.title));
            }
            Some(entry) => self.open_here(entry),
            None if !on_page => self.show_tab(home_tab),
            None => self.notice = Some(self.s.nothing_to_go.to_owned()),
        }
    }

    /// Opens a list where the current one is; `⌫` comes back to it.
    pub(super) fn open_here(&mut self, entry: Entry) {
        let (open, trail) = match self.page {
            Page::Home => (&mut self.home_open, &mut self.home_trail),
            Page::Library => {
                self.library.in_list = true;
                (&mut self.library.open, &mut self.library.trail)
            }
            Page::Search => (&mut self.search.open, &mut self.search.trail),
        };
        if open.as_ref().is_some_and(|o| o.entry.uri == entry.uri) {
            let name = entry.title;
            self.notice = Some((self.s.already_open)(&name));
            return;
        }
        trail.extend(open.take());
        let (kind, uri) = (entry.kind, entry.uri.clone());
        *open = Some(Opened::new(entry));
        self.command(Command::OpenList(kind, uri));
    }

    /// Plays `rows[selected..]`, inside the list's own context when it has one.
    fn play_from(&mut self, entry: &Entry, rows: &[Track], selected: usize) {
        let Some(row) = rows.get(selected) else {
            return;
        };
        self.notice = Some(format!("▶ {}", row.title));
        // Headings and "show more" rows are not tracks.
        let uris = rows[selected..]
            .iter()
            .filter(|r| r.uri.starts_with("spotify:"))
            .map(|r| r.uri.clone())
            .collect();
        let context = match entry.kind {
            Kind::Playlist | Kind::Album => Some(entry.uri.clone()),
            Kind::Liked => self
                .model
                .library
                .as_ref()
                .filter(|i| !i.user.is_empty())
                .map(|i| i.liked_uri()),
            _ => None,
        };
        self.command(match context {
            Some(context) => Command::PlayFrom {
                context: Some(context),
                uris,
            },
            None if entry.kind == Kind::Recent => Command::PlayRecent(row.uri.clone()),
            None => Command::PlayTrack(row.uri.clone()),
        });
    }

    /// `P`: the whole list from its start.
    fn play_all(&mut self, entry: &Entry) {
        self.notice = Some(format!("▶ {}", self.entry_title(entry)));
        let user_known = self
            .model
            .library
            .as_ref()
            .is_some_and(|i| !i.user.is_empty());
        match entry.kind {
            Kind::Heading => {}
            Kind::Recent | Kind::Liked if entry.kind == Kind::Recent || !user_known => {
                let open = [&self.library.open, &self.search.open, &self.home_open]
                    .into_iter()
                    .flatten()
                    .find(|o| o.entry.uri == entry.uri)
                    .and_then(|o| o.page.clone());
                let tracks = open.map(|p| p.tracks.clone()).or_else(|| {
                    (entry.kind == Kind::Liked)
                        .then(|| self.model.liked.as_ref().map(|l| l.as_ref().clone()))
                        .flatten()
                });
                match tracks.and_then(|t| t.into_iter().next()) {
                    Some(first) if entry.kind == Kind::Recent => {
                        self.command(Command::PlayRecent(first.uri));
                    }
                    Some(first) => self.command(Command::PlayTrack(first.uri)),
                    None => self.notice = Some(self.s.loading.to_owned()),
                }
            }
            _ => self.command(Command::PlayContext(entry.uri.clone())),
        }
    }

    /// The name a list goes by on screen; liked songs and recently played have none of
    /// their own.
    pub fn entry_title(&self, entry: &Entry) -> String {
        match entry.kind {
            Kind::Liked => self.s.liked_songs.to_owned(),
            Kind::Recent => self.s.recently_played.to_owned(),
            _ if entry.title.is_empty() => "…".to_owned(),
            _ => entry.title.clone(),
        }
    }

    /// The track the add and remove keys act on: the selected row of whatever list has
    /// the keys, otherwise the playing track.
    fn selected_track(&self) -> Option<Track> {
        let row = if self.active_artist().is_some() {
            self.artist_track().map(|t| Track::from(&t))
        } else if let Some(open) = self.active_open() {
            self.opened_rows(open).get(open.selected).map(Track::from)
        } else {
            self.page_track()
        };
        row.filter(|t| t.uri.starts_with("spotify:track:"))
            .or_else(|| {
                self.model.now.as_ref().map(|n| Track {
                    uri: n.id.clone(),
                    title: n.title.clone(),
                    artist: n.artist.clone(),
                    duration: n.duration,
                })
            })
            .filter(|t| t.uri.starts_with("spotify:track:"))
    }

    /// The selected track row of a page with nothing opened on it.
    fn page_track(&self) -> Option<Track> {
        match self.page {
            Page::Home => self.tab_rows().get(self.selected).cloned(),
            Page::Library if self.library.in_list => self
                .library
                .open
                .as_ref()
                .and_then(|o| self.opened_rows(o).get(o.selected).map(Track::from)),
            Page::Search => match &self.search.open {
                Some(o) => self.opened_rows(o).get(o.selected).map(Track::from),
                None if self.search.section == 0 => self
                    .search
                    .results
                    .as_ref()
                    .and_then(|r| r.tracks.get(self.search.selected[0]))
                    .map(Track::from),
                None => None,
            },
            Page::Library => None,
        }
    }

    /// `a`: pick a playlist for the selected track.
    pub(super) fn ask_add(&mut self) {
        let Some(track) = self.selected_track() else {
            self.notice = Some(self.s.nothing_selected.to_owned());
            return;
        };
        if self.model.library.is_none() {
            self.notice = Some(self.s.loading.to_owned());
            return;
        }
        if self.editable_playlists().is_empty() {
            self.notice = Some(self.s.no_editable.to_owned());
            return;
        }
        self.popup = Some(Popup::AddTo {
            track,
            selected: 0,
            offset: Cell::new(None),
        });
    }

    /// `g k`: Spotify's queue, asked for again so it is current.
    pub(super) fn show_queue(&mut self) {
        if self.model.now.is_none() {
            self.notice = Some(self.s.queue_empty.to_owned());
            return;
        }
        self.command(Command::LoadQueue);
        self.popup = Some(Popup::Queue {
            selected: usize::from(self.model.queue.len() > 1),
            offset: Cell::new(None),
        });
    }

    /// `e`: the selected track (or the playing one) goes to the end of the queue.
    pub(super) fn add_to_queue(&mut self) {
        let Some(track) = self.selected_track() else {
            self.notice = Some(self.s.nothing_selected.to_owned());
            return;
        };
        self.notice = Some((self.s.queued)(&track.title));
        self.command(Command::AddToQueue(track.uri));
    }

    /// `g d`: Connect devices, asked for fresh.
    pub(super) fn show_devices(&mut self) {
        self.model.devices = None;
        self.command(Command::LoadDevices);
        self.popup = Some(Popup::Devices {
            selected: 0,
            offset: Cell::new(None),
        });
    }

    pub(super) fn apply_devices(&mut self, devices: Vec<crate::spotify::model::Device>) {
        let active = devices.iter().position(|d| d.is_active).unwrap_or(0);
        if let Some(Popup::Devices { selected, .. }) = self.popup.as_mut() {
            *selected = active;
        }
        self.model.devices = Some(devices);
    }

    fn transfer(&mut self, device: &crate::spotify::model::Device) {
        let Some(id) = device.id.clone().filter(|_| !device.is_restricted) else {
            self.notice = Some(self.s.device_restricted.to_owned());
            return;
        };
        if device.is_active {
            return;
        }
        self.notice = Some((self.s.resume_on)(&device.name));
        self.command(Command::Transfer {
            device: id,
            play: self.playing || self.model.now.is_none(),
        });
    }

    /// `x`: remove the selected track from the list on screen, after a confirmation.
    pub(super) fn ask_remove(&mut self) {
        let list = match self.page {
            Page::Home if self.tab == 0 => self
                .context_list()
                .map(|l| (l.uri.clone(), self.context_title().unwrap_or_default())),
            Page::Library if self.library.in_list => self
                .library
                .open
                .as_ref()
                .map(|o| (o.entry.uri.clone(), self.entry_title(&o.entry))),
            Page::Search => self
                .search
                .open
                .as_ref()
                .map(|o| (o.entry.uri.clone(), self.entry_title(&o.entry))),
            _ => None,
        };
        let Some(list) = list else {
            return;
        };
        let Some(index) = self.model.library.as_ref() else {
            self.notice = Some(self.s.loading.to_owned());
            return;
        };
        if !index.editable(&list.0) {
            self.notice = Some(self.s.not_editable.to_owned());
            return;
        }
        let track = match self.page {
            Page::Home => self.tab_rows().get(self.selected).cloned(),
            _ => self.selected_track(),
        };
        if let Some(track) = track.filter(|t| t.uri.starts_with("spotify:track:")) {
            self.popup = Some(Popup::Remove {
                playlist: list,
                track,
            });
        }
    }

    pub(super) fn popup_key(&mut self, key: KeyEvent) {
        let Some(popup) = self.popup.as_mut() else {
            return;
        };
        match popup {
            Popup::AddTo {
                track, selected, ..
            } => {
                let lists = self
                    .model
                    .library
                    .as_ref()
                    .map_or(0, |i| i.playlists.iter().filter(|e| e.editable).count());
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        *selected = (*selected + 1).min(lists.saturating_sub(1));
                    }
                    KeyCode::Char('k') | KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Home => *selected = 0,
                    KeyCode::End => *selected = lists.saturating_sub(1),
                    KeyCode::Enter => {
                        let (track, selected) = (track.clone(), *selected);
                        self.popup = None;
                        if let Some(list) = self.editable_playlists().get(selected) {
                            self.notice = Some((self.s.adding)(&track.title, &list.title));
                            self.command(Command::AddToPlaylist {
                                playlist: list.uri.clone(),
                                track: track.uri,
                            });
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q' | 'a') => self.popup = None,
                    _ => {}
                }
            }
            Popup::Queue { selected, .. } => {
                let last = self.model.queue.len().saturating_sub(1);
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => *selected = (*selected + 1).min(last),
                    KeyCode::Char('k') | KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Home => *selected = 0,
                    KeyCode::End => *selected = last,
                    KeyCode::Enter => {
                        let ahead = *selected;
                        self.popup = None;
                        if let Some(track) = self.model.queue.get(ahead).filter(|_| ahead > 0) {
                            self.notice = Some(format!("▶ {}", track.title));
                            self.command(Command::SkipAhead(ahead));
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q' | 'g') => self.popup = None,
                    _ => {}
                }
            }
            Popup::Devices { selected, .. } => {
                let devices = self.model.devices.clone().unwrap_or_default();
                let last = devices.len().saturating_sub(1);
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => *selected = (*selected + 1).min(last),
                    KeyCode::Char('k') | KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Enter => {
                        let chosen = devices.get(*selected).cloned();
                        self.popup = None;
                        if let Some(device) = chosen {
                            self.transfer(&device);
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q' | 'g') => self.popup = None,
                    _ => {}
                }
            }
            Popup::Remove { playlist, track } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let (playlist, track) = (playlist.0.clone(), track.uri.clone());
                    self.popup = None;
                    self.command(Command::RemoveFromPlaylist { playlist, track });
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => self.popup = None,
                _ => {}
            },
        }
    }

    pub(super) fn apply_list(&mut self, page: Arc<ListPage>) {
        for open in [
            &mut self.library.open,
            &mut self.search.open,
            &mut self.home_open,
        ]
        .into_iter()
        .flatten()
        .filter(|o| o.entry.uri == page.uri)
        {
            open.selected = open.selected.min(page.tracks.len().saturating_sub(1));
            // Made For You lists can be opened before librespot named them.
            if open.entry.title.is_empty() {
                open.entry.title.clone_from(&page.name);
            }
            open.page = Some(page.clone());
        }
        if self
            .model
            .context_list
            .as_ref()
            .is_some_and(|l| l.uri == page.uri)
        {
            self.model.context_list = Some(page);
        }
    }

    pub(super) fn apply_search(&mut self, found: Arc<SearchResults>) {
        let search = &mut self.search;
        if found.query != search.query.trim() {
            return;
        }
        search.busy = false;
        search.selected = [0; 4];
        for offset in &search.offsets {
            offset.set(None);
        }
        search.results = Some(found);
        search.section = (0..4).find(|&s| search.section_len(s) > 0).unwrap_or(0);
    }

    pub(super) fn apply_edit(&mut self, playlist: &str, track: &str, added: bool) {
        let name = self
            .model
            .library
            .as_ref()
            .and_then(|i| i.playlists.iter().find(|e| e.uri == playlist))
            .map(|e| e.title.clone())
            .unwrap_or_default();
        let title = self.known_title(track).unwrap_or_else(|| track.to_owned());
        if added {
            self.notice = Some((self.s.added)(&title, &name));
            if let Some(entry) = self
                .model
                .library
                .as_mut()
                .and_then(|i| i.playlist_mut(playlist))
            {
                entry.count = entry.count.map(|n| n + 1);
            }
            return;
        }
        self.notice = Some((self.s.removed)(&title, &name));
        let mut removed = 0;
        let trimmed = |page: &Arc<ListPage>| {
            Arc::new(ListPage {
                uri: page.uri.clone(),
                name: page.name.clone(),
                tracks: page
                    .tracks
                    .iter()
                    .filter(|t| t.uri != track)
                    .cloned()
                    .collect(),
            })
        };
        for open in [
            &mut self.library.open,
            &mut self.search.open,
            &mut self.home_open,
        ]
        .into_iter()
        .flatten()
        .filter(|o| o.entry.uri == playlist)
        {
            if let Some(page) = &open.page {
                let page = trimmed(page);
                removed = removed
                    .max(open.page.as_ref().map_or(0, |p| p.tracks.len()) - page.tracks.len());
                open.selected = open.selected.min(page.tracks.len().saturating_sub(1));
                open.page = Some(page);
            }
        }
        if let Some(list) = self
            .model
            .context_list
            .as_ref()
            .filter(|l| l.uri == playlist)
        {
            let page = trimmed(list);
            removed = removed.max(list.tracks.len() - page.tracks.len());
            self.model.context_list = Some(page);
            self.selected = self.selected.min(self.tab_rows().len().saturating_sub(1));
        }
        if let Some(entry) = self
            .model
            .library
            .as_mut()
            .and_then(|i| i.playlist_mut(playlist))
        {
            entry.count = entry.count.map(|n| n.saturating_sub(removed.max(1)));
        }
    }

    /// A track's title from any list on screen, for messages.
    fn known_title(&self, uri: &str) -> Option<String> {
        let lists = [&self.library.open, &self.search.open, &self.home_open]
            .into_iter()
            .flatten()
            .filter_map(|o| o.page.clone())
            .chain(self.model.context_list.clone());
        for list in lists {
            if let Some(t) = list.tracks.iter().find(|t| t.uri == uri) {
                return Some(t.name.clone());
            }
        }
        let found = self
            .search
            .results
            .as_ref()
            .and_then(|r| r.tracks.iter().find(|t| t.uri == uri));
        if let Some(t) = found {
            return Some(t.name.clone());
        }
        self.model
            .now
            .as_ref()
            .filter(|n| n.id == uri)
            .map(|n| n.title.clone())
    }
}

/// The first selectable row at or after `from`.
fn first_entry(rows: &[Entry], from: usize) -> usize {
    rows.iter()
        .skip(from)
        .position(|e| e.kind != Kind::Heading)
        .map_or(0, |i| from + i)
}

/// Moves one row, stepping over headings; stays put at either end.
pub fn step(selected: usize, delta: isize, last: usize, heading: impl Fn(usize) -> bool) -> usize {
    let mut next = selected.saturating_add_signed(delta).min(last);
    while heading(next) {
        let further = next.saturating_add_signed(delta).min(last);
        if further == next {
            return selected.min(last);
        }
        next = further;
    }
    next
}

/// The next section with results, wrapping around.
fn next_section(search: &SearchView, delta: isize) -> usize {
    (1..=4)
        .map(|i| (search.section as isize + delta * i).rem_euclid(4) as usize)
        .find(|&s| search.section_len(s) > 0)
        .unwrap_or(search.section)
}
