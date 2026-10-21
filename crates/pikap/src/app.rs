mod artist;
mod browse;

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc::UnboundedSender;

use crate::data::{Model, NowPlaying, Track};
use crate::i18n::{Lang, Strings};
use crate::spotify::api::Quota;
use crate::spotify::config;
use crate::spotify::meta::ListPage;
use crate::spotify::model::{Item, Playback};
use crate::spotify::worker::{Command, ResumeStage, Update};
use crate::theme::{Mode, Palette};

pub use artist::{ArtistView, LIKED, POPULAR, RELATED, RELEASES};
pub use browse::{LibraryView, Opened, Page, Popup, SearchView};

/// Leader (`g`) keys, shown in the panel in this order; labels live in `Strings::leader_labels`.
pub const LEADER_KEYS: [char; 12] = ['l', 's', 'y', 'r', 'k', 'L', 'c', 'a', 'b', 'm', 'd', 'z'];

const SEEK_STEP: Duration = Duration::from_secs(5);
const VOLUME_STEP: u8 = 5;
const QUEUE_SHOWN: usize = 50;
const DEBOUNCE: Duration = Duration::from_millis(300);
const HOLD: Duration = Duration::from_millis(1500);

pub enum Flow {
    Continue,
    Quit,
    /// Clear the screen and send every image again (`ctrl-l`).
    Redraw,
}

/// Channels to the Spotify worker; absent in `--demo`.
pub struct Live {
    pub commands: UnboundedSender<Command>,
    pub updates: Receiver<Update>,
}

pub struct App {
    pub model: Model,
    pub page: Page,
    /// Pages to return to with `⌫`.
    history: Vec<Page>,
    pub library: LibraryView,
    pub search: SearchView,
    pub popup: Option<Popup>,
    /// Stage mode (`z`): the whole screen for the playing track.
    pub stage: bool,
    pub s: &'static Strings,
    pub mode: Mode,
    pub tab: usize,
    pub selected: usize,
    pub leader: bool,
    pub playing: bool,
    pub notice: Option<String>,
    pub quota: Quota,
    /// False until the first playback poll answers, so startup does not flash "nothing
    /// is playing".
    pub synced: bool,
    /// Dominant colour of the current cover, for the equalizer and the current lyric.
    pub cover_color: Option<[u8; 3]>,
    /// Equalizer liveliness: eases to 1 while playing and to 0 when paused.
    pub eq_energy: f32,
    /// Equalizer clock; advances with `eq_energy`, so bars slow down and settle.
    pub eq_time: f32,
    /// First visible row of the current list. `None` asks the next render to place the
    /// selection a third of the way down; afterwards the view only moves to keep the
    /// selection visible.
    pub list_offset: std::cell::Cell<Option<usize>>,
    /// Seek and volume keys update locally and reach Spotify once the key is released,
    /// instead of one request per key repeat.
    pending_seek: Option<Instant>,
    pending_volume: Option<Instant>,
    selected_on_playing: bool,
    /// The home artist tab's sections and selection.
    pub artist_view: ArtistView,
    /// A list or artist opened in place of the home tab body; `⌫` closes it.
    pub home_open: Option<Opened>,
    home_trail: Vec<Opened>,
    /// Polled positions and volumes are stale for a moment after we change them.
    hold_position: Option<Instant>,
    hold_volume: Option<Instant>,
    live: Option<Live>,
    upcoming: Vec<Item>,
    /// Context the playback came from, if it is one a track offset can point into.
    context: Option<String>,
    /// Context URI of any kind, to match the loaded context list.
    playing_context: Option<String>,
    /// What to play when Spotify has no session to resume: (context, track).
    resume_fallback: Option<(Option<String>, String)>,
    position: Duration,
    last_tick: Instant,
}

impl App {
    pub fn new(lang: Lang, mode: Mode, model: Model, live: Option<Live>) -> Self {
        Self {
            playing: live.is_none(),
            position: if live.is_none() {
                Duration::from_secs(88)
            } else {
                Duration::ZERO
            },
            model,
            page: Page::Home,
            history: Vec::new(),
            library: LibraryView::default(),
            search: SearchView::default(),
            popup: None,
            stage: false,
            s: lang.strings(),
            mode,
            tab: 0,
            selected: 0,
            leader: false,
            notice: None,
            quota: Quota::Normal,
            synced: live.is_none(),
            cover_color: None,
            eq_energy: 0.0,
            eq_time: 0.0,
            list_offset: std::cell::Cell::new(None),
            pending_seek: None,
            pending_volume: None,
            selected_on_playing: true,
            artist_view: ArtistView::default(),
            home_open: None,
            home_trail: Vec::new(),
            hold_position: None,
            hold_volume: None,
            live,
            upcoming: Vec::new(),
            context: None,
            playing_context: None,
            resume_fallback: None,
            last_tick: Instant::now(),
        }
    }

    pub fn p(&self) -> &'static Palette {
        self.mode.palette()
    }

    pub fn position(&self) -> Duration {
        self.position
    }

    pub fn progress(&self) -> f64 {
        let total = self
            .model
            .now
            .as_ref()
            .map_or(1.0, |n| n.duration.as_secs_f64());
        self.position.as_secs_f64() / total.max(1.0)
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f32().min(0.25);
        let target = if self.playing { 1.0 } else { 0.0 };
        self.eq_energy += (target - self.eq_energy) * (dt * 3.0).min(1.0);
        self.eq_time += dt * self.eq_energy;
        if self.pending_seek.is_some_and(|t| now - t >= DEBOUNCE) {
            self.pending_seek = None;
            self.hold_position = Some(now + HOLD);
            self.command(Command::SeekTo(self.position.as_millis() as u64));
        }
        if self.pending_volume.is_some_and(|t| now - t >= DEBOUNCE) {
            self.pending_volume = None;
            self.hold_volume = Some(now + HOLD);
            self.command(Command::Volume(self.model.volume));
        }
        if self.playing
            && let Some(track) = &self.model.now
        {
            self.position = (self.position + (now - self.last_tick)).min(track.duration);
        }
        self.last_tick = now;
    }

    /// Applies everything the worker sent since the last frame.
    pub fn pump(&mut self) {
        let mut updates = Vec::new();
        if let Some(live) = &self.live {
            while let Ok(update) = live.updates.try_recv() {
                updates.push(update);
            }
        }
        for update in updates {
            self.apply(update);
        }
    }

    pub(crate) fn apply(&mut self, update: Update) {
        if matches!(update, Update::Playback(_)) {
            self.synced = true;
        }
        match update {
            Update::Playback(None) => {
                self.model.now = None;
                self.playing = false;
            }
            Update::Playback(Some(playback)) => self.apply_playback(*playback),
            Update::Queue(items) => {
                self.upcoming = items;
                self.rebuild_queue();
            }
            Update::Liked(uri, liked) => {
                if let Some(now) = self.model.now.as_mut().filter(|n| n.id == uri) {
                    now.liked = liked;
                }
            }
            Update::Cover(uri, path) => {
                if let Some(now) = self.model.now.as_mut().filter(|n| n.id == uri) {
                    now.cover = Some(path);
                }
            }
            Update::Quota(quota) => self.quota = quota,
            Update::OnHold => self.notice = Some(self.s.on_hold.to_owned()),
            Update::Volume(volume) => {
                let now = Instant::now();
                let held =
                    self.pending_volume.is_some() || self.hold_volume.is_some_and(|h| now < h);
                if !held {
                    self.model.volume = volume;
                }
            }
            Update::Error(message) => self.notice = Some(message),
            Update::Meta(ready) => self.model.meta = Some(ready),
            Update::LastPlayed(last) => {
                self.model.last_played = last
                    .as_ref()
                    .map(|h| (h.track.name.clone(), h.track.artist_line()));
                self.resume_fallback = last.map(|h| {
                    let context = h
                        .context
                        .filter(|c| !c.uri.is_empty() && c.kind != "artist")
                        .map(|c| c.uri);
                    (context, h.track.uri)
                });
            }
            Update::Resume(stage) => {
                self.notice = Some(match stage {
                    ResumeStage::Launching => self.s.resume_launching.to_owned(),
                    ResumeStage::Resumed(device) => (self.s.resume_on)(&device),
                    ResumeStage::NoDevice => self.s.resume_no_device.to_owned(),
                });
            }
            Update::Artist(page) => {
                for open in [
                    &mut self.library.open,
                    &mut self.search.open,
                    &mut self.home_open,
                ]
                .into_iter()
                .flatten()
                .filter(|o| o.entry.uri == page.uri)
                {
                    open.artist = Some(page.clone());
                }
                let current = self.model.now.as_ref().and_then(|n| n.artist_uri.as_ref());
                if current == Some(&page.uri) {
                    if self.model.artist.as_ref().map(|a| &a.uri) != Some(&page.uri) {
                        self.artist_view = ArtistView::default();
                    }
                    self.model.near = page.related.iter().map(|(_, n)| n.clone()).collect();
                    self.model.artist = Some(page);
                }
            }
            Update::Devices(devices) => self.apply_devices(devices),
            Update::CardCover(uri, path) => {
                self.model.card_covers.insert(uri, path);
            }
            Update::Album(page) => {
                let current = self.model.now.as_ref().and_then(|n| n.album_uri.as_ref());
                if current == Some(&page.uri) {
                    self.model.album = Some(page);
                }
            }
            Update::Radio(seed, tracks) => self.model.radio = Some((seed, tracks)),
            Update::LikedLibrary(tracks) => self.model.liked = Some(tracks),
            Update::Context(page) => {
                let fresh = self.model.context_list.as_ref().map(|l| &l.uri) != Some(&page.uri);
                self.model.context_list = Some(page);
                if fresh && self.tab == 0 {
                    self.on_tab_shown();
                }
            }
            Update::Lyrics(uri, page) => {
                if self.model.lyrics_for.as_ref() == Some(&uri) {
                    self.model.lyrics = Some(page);
                }
            }
            Update::Library(index) => {
                let named = self.model.library.take().map(|i| i.made_for_you);
                let mut index = *index;
                if let Some(named) = named.filter(|m| m.iter().any(|e| !e.title.is_empty())) {
                    index.made_for_you = named;
                }
                self.model.library = Some(index);
            }
            Update::MadeForYou(entries) => {
                if let Some(index) = self.model.library.as_mut() {
                    index.made_for_you = entries;
                }
            }
            Update::List(page) => self.apply_list(page),
            Update::Search(found) => self.apply_search(found),
            Update::Edited {
                playlist,
                track,
                added,
            } => self.apply_edit(&playlist, &track, added),
        }
    }

    fn apply_playback(&mut self, playback: Playback) {
        self.model.device = playback.device.name;
        let now_instant = Instant::now();
        let held = |hold: Option<Instant>, pending: Option<Instant>| {
            pending.is_some() || hold.is_some_and(|h| now_instant < h)
        };
        if !held(self.hold_volume, self.pending_volume) {
            self.model.volume = playback.device.volume_percent.unwrap_or(self.model.volume);
        }
        self.model.shuffle = playback.shuffle_state;
        self.model.repeat = match playback.repeat_state.as_str() {
            "context" => 1,
            "track" => 2,
            _ => 0,
        };
        self.playing = playback.is_playing;
        if !held(self.hold_position, self.pending_seek) {
            self.position = Duration::from_millis(playback.progress_ms.unwrap_or(0));
        }
        self.last_tick = Instant::now();
        let context = playback.context.filter(|c| !c.uri.is_empty());
        self.playing_context = context.as_ref().map(|c| c.uri.clone());
        // Artist contexts reject a track offset; everything else is worth trying.
        self.context = context.filter(|c| c.kind != "artist").map(|c| c.uri);
        let Some(item) = playback.item else {
            self.model.now = None;
            return;
        };
        if self.model.now.as_ref().is_some_and(|n| n.id == item.uri) {
            return;
        }
        let item_uri = item.uri.clone();
        let old_rows = self.tab_rows();
        let old_row = self
            .model
            .now
            .as_ref()
            .and_then(|n| old_rows.iter().position(|r| r.uri == n.id));
        self.selected_on_playing = old_row.is_none_or(|i| i == self.selected);
        // The previous queue already lists this track first; drop it until the fresh
        // queue arrives so it does not show twice.
        if self.upcoming.first().is_some_and(|i| i.uri == item.uri) {
            self.upcoming.remove(0);
        }
        let album = item.album.clone().unwrap_or_default();
        let cover = item
            .cover(640)
            .map(|(id, _)| config::cache_dir().join("covers").join(format!("{id}.jpg")))
            .filter(|p: &PathBuf| p.exists());
        self.model.now = Some(NowPlaying {
            id: item.uri.clone(),
            title: item.name.clone(),
            artist: item.artist_line(),
            album: if album.name.is_empty() {
                item.show
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default()
            } else {
                album.name
            },
            year: album.release_date.get(..4).and_then(|y| y.parse().ok()),
            liked: false,
            duration: Duration::from_millis(item.duration_ms),
            cover,
            artist_uri: item
                .artists
                .first()
                .map(|a| a.uri.clone())
                .filter(|u| !u.is_empty()),
            album_uri: Some(album.uri).filter(|u| !u.is_empty()),
        });
        if self.model.artist.as_ref().map(|a| &a.uri) != self.now_ref(|n| n.artist_uri.as_ref()) {
            self.model.artist = None;
            self.model.near.clear();
        }
        if self.model.album.as_ref().map(|a| &a.uri) != self.now_ref(|n| n.album_uri.as_ref()) {
            self.model.album = None;
        }
        self.model.lyrics = None;
        self.model.lyrics_for = Some(item.uri);
        let rows = self.tab_rows();
        let new_row = rows.iter().position(|r| r.uri == item_uri);
        if self.selected_on_playing || rows.is_empty() {
            self.selected = new_row.unwrap_or(0);
        }
        self.rebuild_queue();
        if self.tab == 4 && self.current_radio().is_none() {
            self.command(Command::LoadRadio(item_uri.clone()));
        }
    }

    fn now_ref<'a>(
        &'a self,
        f: impl Fn(&'a NowPlaying) -> Option<&'a String>,
    ) -> Option<&'a String> {
        self.model.now.as_ref().and_then(f)
    }

    fn rebuild_queue(&mut self) {
        let Some(now) = &self.model.now else {
            self.model.queue.clear();
            return;
        };
        let mut queue = vec![Track {
            uri: now.id.clone(),
            title: now.title.clone(),
            artist: now.artist.clone(),
            duration: now.duration,
        }];
        // Spotify fills the 20-item answer by looping a short context; stop at the first
        // track already listed so every row is a real upcoming track.
        let mut seen = std::collections::HashSet::from([now.id.clone()]);
        let mut loops = false;
        for item in self.upcoming.iter().take(QUEUE_SHOWN) {
            if !seen.insert(item.uri.clone()) {
                loops = true;
                break;
            }
            queue.push(Track {
                uri: item.uri.clone(),
                title: item.name.clone(),
                artist: item.artist_line(),
                duration: Duration::from_millis(item.duration_ms),
            });
        }
        self.model.queue = queue;
        self.model.queue_loops = loops;
        self.selected = self.selected.min(self.tab_rows().len().saturating_sub(1));
    }

    /// Rows of the list on the current tab; the lyrics tab has none.
    pub fn tab_rows(&self) -> Vec<Track> {
        match self.tab {
            0 => self.list_rows(),
            3 => self
                .model
                .album
                .as_ref()
                .map_or_else(Vec::new, |a| a.tracks.iter().map(Track::from).collect()),
            4 => self
                .current_radio()
                .map_or_else(Vec::new, |r| r.tracks.iter().map(Track::from).collect()),
            _ => Vec::new(),
        }
    }

    /// The loaded list of the context playback runs from, if it matches.
    pub fn context_list(&self) -> Option<&ListPage> {
        let list = self.model.context_list.as_deref()?;
        (Some(&list.uri) == self.playing_context.as_ref()).then_some(list)
    }

    /// The main list: the whole playing context when known, so earlier tracks stay put,
    /// otherwise Spotify's queue.
    pub fn list_rows(&self) -> Vec<Track> {
        match self.context_list() {
            Some(list) => list.tracks.iter().map(Track::from).collect(),
            None => self.model.queue.clone(),
        }
    }

    /// Name for the header: the context's own, liked songs, or nothing yet.
    pub fn context_title(&self) -> Option<String> {
        let list = self.context_list()?;
        Some(if list.name.is_empty() {
            self.s.liked_songs.to_owned()
        } else {
            list.name.clone()
        })
    }

    /// The radio seeded by the playing track, once loaded.
    pub fn current_radio(&self) -> Option<&ListPage> {
        let (seed, radio) = self.model.radio.as_ref()?;
        (Some(seed) == self.model.now.as_ref().map(|n| &n.id)).then_some(radio.as_ref())
    }

    /// Loads what the newly shown tab needs and puts the selection on the playing track.
    fn on_tab_shown(&mut self) {
        let now = self.model.now.as_ref().map(|n| n.id.clone());
        if self.tab == 4
            && self.current_radio().is_none()
            && let Some(seed) = now.clone()
        {
            self.command(Command::LoadRadio(seed));
        }
        let rows = self.tab_rows();
        self.selected = now
            .and_then(|uri| rows.iter().position(|r| r.uri == uri))
            .unwrap_or(0);
        self.list_offset.set(None);
    }

    /// Plays the selected row: album rows in the album's context, other lists as a run of
    /// tracks starting at the selection.
    fn play_selected(&mut self) {
        let rows = self.tab_rows();
        let Some(row) = rows.get(self.selected).filter(|r| !r.uri.is_empty()) else {
            return;
        };
        if self.model.now.as_ref().is_some_and(|n| n.id == row.uri) {
            return;
        }
        self.notice = Some(format!("▶ {}", row.title));
        match self.tab {
            0 => {
                let uris = rows[self.selected..]
                    .iter()
                    .map(|r| r.uri.clone())
                    .collect();
                self.command(Command::PlayFrom {
                    context: self.context.clone(),
                    uris,
                });
            }
            3 => {
                if let Some(album) = &self.model.album {
                    self.command(Command::PlayInContext {
                        context: album.uri.clone(),
                        track: row.uri.clone(),
                    });
                }
            }
            4 => {
                if let Some(radio) = self.current_radio() {
                    let context = radio.uri.clone();
                    self.command(Command::PlayInContext {
                        context,
                        track: row.uri.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    fn command(&self, command: Command) {
        if let Some(live) = &self.live {
            let _ = live.commands.send(command);
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Flow {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Flow::Quit;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('l') {
            return Flow::Redraw;
        }
        if self.leader {
            self.leader = false;
            if let KeyCode::Char(c) = key.code {
                self.leader_key(c);
            }
            return Flow::Continue;
        }
        if self.popup.is_some() {
            self.popup_key(key);
            return Flow::Continue;
        }
        if self.page == Page::Search && self.search.typing {
            self.typing_key(key);
            return Flow::Continue;
        }
        self.notice = None;
        // Shift-arrows seek everywhere, before the page takes the plain arrows.
        if key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(key.code, KeyCode::Left | KeyCode::Right)
        {
            self.seek_by(SEEK_STEP, key.code == KeyCode::Right);
            return Flow::Continue;
        }
        if self.stage {
            // Only playback keys reach through the stage; navigation waits behind it.
            match key.code {
                KeyCode::Backspace | KeyCode::Esc => self.stage = false,
                KeyCode::Char(
                    ' ' | 'n' | 'p' | 's' | 'R' | 'f' | 'T' | 'q' | '+' | '=' | '-' | 'ö' | 'Ö'
                    | 'ç' | 'Ç' | ',' | '.' | '<' | '>',
                ) => {}
                _ => return Flow::Continue,
            }
            if !self.stage {
                return Flow::Continue;
            }
        }
        let handled = if self.stage {
            false
        } else {
            match self.page {
                Page::Home => self.home_key(key),
                Page::Library => self.library_key(key),
                Page::Search => self.search_key(key),
            }
        };
        if handled {
            return Flow::Continue;
        }
        match key.code {
            KeyCode::Char('q') => return Flow::Quit,
            KeyCode::Char('g') => self.leader = true,
            KeyCode::Char('/') => self.open_search(),
            // Esc at home stays home; only ⌫ goes on to the library from there.
            KeyCode::Esc if self.page == Page::Home && self.home_open.is_none() => {}
            KeyCode::Backspace | KeyCode::Esc => self.back(),
            KeyCode::Char(' ') => {
                self.playing = !self.playing;
                self.command(Command::SetPlaying(self.playing));
            }
            KeyCode::Char('n') => self.command(Command::Next),
            KeyCode::Char('p') => self.command(Command::Previous),
            KeyCode::Char('s') => {
                self.model.shuffle = !self.model.shuffle;
                self.command(Command::SetShuffle(self.model.shuffle));
            }
            KeyCode::Char('R') => {
                self.model.repeat = (self.model.repeat + 1) % 3;
                let state = ["off", "context", "track"][usize::from(self.model.repeat)];
                self.command(Command::SetRepeat(state));
            }
            KeyCode::Char('r') => {
                if let Some(now) = &self.model.now {
                    self.command(Command::StartRadio(now.id.clone()));
                }
            }
            // Arrows move around the page; seeking has its own keys: ö and ç sit side
            // by side on a Turkish keyboard, comma and period on an English one.
            KeyCode::Char('ç' | 'Ç' | '.' | '>') => self.seek_by(SEEK_STEP, true),
            KeyCode::Char('ö' | 'Ö' | ',' | '<') => self.seek_by(SEEK_STEP, false),
            KeyCode::Char('+' | '=') => self.volume_by(VOLUME_STEP, true),
            KeyCode::Char('-') => self.volume_by(VOLUME_STEP, false),
            KeyCode::Char('f') => {
                if let Some(now) = self.model.now.as_mut() {
                    now.liked = !now.liked;
                    let command = Command::SetLiked(now.id.clone(), now.liked);
                    self.command(command);
                }
            }
            KeyCode::Char('a') => self.ask_add(),
            KeyCode::Char('e') => self.add_to_queue(),
            KeyCode::Char('z') => self.enter_stage(),
            KeyCode::Char('x') => self.ask_remove(),
            KeyCode::Char('T') => {
                self.mode = self.mode.toggled();
                self.notice = Some((self.s.theme_now)(self.p().name));
            }
            KeyCode::Char(c @ '1'..='5') => self.show_tab(c as usize - '1' as usize),
            _ => {}
        }
        Flow::Continue
    }

    /// `g` then a key. Destinations home already has are its tabs, not new screens.
    fn leader_key(&mut self, c: char) {
        match c {
            'l' => self.goto(Page::Library),
            's' => self.open_search(),
            'y' => self.open_library_kind(crate::data::Kind::Liked),
            'r' => self.open_library_kind(crate::data::Kind::Recent),
            'm' => self.show_made_for_you(),
            'c' => self.show_tab(0),
            'L' => self.show_tab(1),
            'a' => self.go_to_artist(),
            'b' => self.go_to_album(),
            'k' => self.show_queue(),
            'd' => self.show_devices(),
            'z' => self.enter_stage(),
            'g' => {}
            c => {
                if let Some(i) = LEADER_KEYS.iter().position(|k| *k == c) {
                    self.notice = Some((self.s.screen_missing)(c, self.s.leader_labels[i]));
                }
            }
        }
    }

    fn enter_stage(&mut self) {
        if self.model.now.is_some() {
            self.stage = true;
        } else {
            self.notice = Some(self.s.nothing_playing.to_owned());
        }
    }

    /// Home with the given tab, the selection on the playing track.
    pub(super) fn show_tab(&mut self, tab: usize) {
        self.goto(Page::Home);
        self.home_open = None;
        self.home_trail.clear();
        self.tab = tab;
        self.on_tab_shown();
    }

    /// The home list keys; everything else falls through to the shared keys.
    fn home_key(&mut self, key: KeyEvent) -> bool {
        if self.home_open.is_some() {
            return self.open_list_key(key);
        }
        if self.tab == 2 && self.model.now.is_some() && self.artist_key(key) {
            return true;
        }
        let last = self.tab_rows().len().saturating_sub(1);
        match key.code {
            KeyCode::Enter if self.model.now.is_none() => {
                self.command(Command::Resume {
                    fallback: self.resume_fallback.clone(),
                });
            }
            KeyCode::Enter => self.play_selected(),
            KeyCode::Char('j') | KeyCode::Down => self.step(1, last),
            KeyCode::Char('k') | KeyCode::Up => self.step(-1, last),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = last,
            KeyCode::Left | KeyCode::Right => {
                let tabs = self.s.tabs.len();
                let step = if key.code == KeyCode::Right {
                    1
                } else {
                    tabs - 1
                };
                self.show_tab((self.tab + step) % tabs);
            }
            _ => return false,
        }
        true
    }

    /// Moves the selection one row, stepping over heading rows.
    fn step(&mut self, delta: isize, last: usize) {
        let rows = self.tab_rows();
        let heading = |i: usize| rows.get(i).is_some_and(|r| r.uri.is_empty());
        self.selected = browse::step(self.selected, delta, last, heading);
    }

    fn seek_by(&mut self, step: Duration, forward: bool) {
        let Some(now) = &self.model.now else { return };
        self.position = if forward {
            (self.position + step).min(now.duration)
        } else {
            self.position.saturating_sub(step)
        };
        self.pending_seek = Some(Instant::now());
    }

    fn volume_by(&mut self, step: u8, up: bool) {
        self.model.volume = if up {
            self.model.volume.saturating_add(step).min(100)
        } else {
            self.model.volume.saturating_sub(step)
        };
        self.pending_volume = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::model::Device;

    fn item(uri: &str) -> Item {
        Item {
            uri: uri.into(),
            name: uri.into(),
            duration_ms: 1000,
            ..Item::default()
        }
    }

    #[test]
    fn looping_queue_is_cut_at_the_first_repeat() {
        let mut app = App::new(Lang::Tr, Mode::Dark, Model::default(), None);
        app.apply(Update::Playback(Some(Box::new(Playback {
            device: Device::default(),
            is_playing: true,
            item: Some(item("now")),
            ..Playback::default()
        }))));
        let looped = ["a", "b", "c", "now", "a", "b", "now", "a"];
        app.apply(Update::Queue(looped.iter().map(|u| item(u)).collect()));
        let uris: Vec<&str> = app.model.queue.iter().map(|t| t.uri.as_str()).collect();
        assert_eq!(uris, ["now", "a", "b", "c"]);
        assert!(app.model.queue_loops);

        app.apply(Update::Queue(["a", "b"].iter().map(|u| item(u)).collect()));
        assert_eq!(app.model.queue.len(), 3);
        assert!(!app.model.queue_loops);
    }

    fn playing(uri: &str) -> Update {
        Update::Playback(Some(Box::new(Playback {
            is_playing: true,
            item: Some(item(uri)),
            context: Some(crate::spotify::model::PlayContext {
                uri: "spotify:playlist:p".into(),
                kind: "playlist".into(),
            }),
            ..Playback::default()
        })))
    }

    #[test]
    fn selection_follows_playing_track_only_when_it_was_on_it() {
        let mut app = App::new(Lang::Tr, Mode::Dark, Model::default(), None);
        app.apply(playing("c"));
        let tracks = ["a", "b", "c", "d", "e"].map(|u| item(u).info()).to_vec();
        app.apply(Update::Context(std::sync::Arc::new(ListPage {
            uri: "spotify:playlist:p".into(),
            name: "p".into(),
            tracks,
        })));
        assert_eq!(app.selected, 2);

        app.apply(playing("d"));
        assert_eq!(
            app.selected, 3,
            "selection sat on the playing track, so it follows"
        );

        app.selected = 0;
        app.apply(playing("e"));
        assert_eq!(
            app.selected, 0,
            "the user moved away, so the selection stays"
        );
        assert_eq!(app.list_rows().len(), 5, "the list itself never changes");
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn library_app() -> App {
        use crate::data::{Entry, Kind, LibraryIndex};
        let mut app = App::new(Lang::Tr, Mode::Dark, Model::default(), None);
        let list = |uri: &str, editable: bool| Entry {
            editable,
            count: Some(2),
            ..Entry::new(Kind::Playlist, uri, uri)
        };
        app.model.library = Some(LibraryIndex {
            user: "u".into(),
            made_for_you: vec![list("spotify:playlist:mfy", false)],
            playlists: vec![list("spotify:playlist:mine", true)],
            albums: Vec::new(),
            held: None,
        });
        app
    }

    #[test]
    fn leader_keys_reuse_home_tabs() {
        let mut app = library_app();
        app.on_key(key(KeyCode::Char('g')));
        app.on_key(key(KeyCode::Char('l')));
        assert_eq!(app.page, Page::Library);
        app.on_key(key(KeyCode::Char('g')));
        app.on_key(key(KeyCode::Char('L')));
        assert_eq!(
            (app.page, app.tab),
            (Page::Home, 1),
            "g L is the lyrics tab"
        );
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.page, Page::Library, "⌫ goes back where we came from");
    }

    #[test]
    fn library_steps_over_headings_and_opens_in_place() {
        use crate::data::Kind;
        let mut app = library_app();
        app.on_key(key(KeyCode::Char('g')));
        app.on_key(key(KeyCode::Char('l')));
        let rows = app.library_rows();
        assert_eq!(
            rows[app.library.selected].kind,
            Kind::Playlist,
            "starts on Made For You"
        );
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(
            rows[app.library.selected].kind,
            Kind::Liked,
            "spacer and heading skipped"
        );
        app.on_key(key(KeyCode::Enter));
        assert!(app.library.in_list);
        assert_eq!(
            app.library.open.as_ref().map(|o| o.entry.kind),
            Some(Kind::Liked)
        );
        app.on_key(key(KeyCode::Esc));
        assert!(!app.library.in_list, "esc first leaves the list");
        assert_eq!(app.page, Page::Library);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.page, Page::Home);
    }

    #[test]
    fn stale_search_answers_are_ignored() {
        use crate::data::SearchResults;
        let mut app = library_app();
        app.on_key(key(KeyCode::Char('/')));
        for c in "blur".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert!(app.search.busy && !app.search.typing);
        let answer = |query: &str| {
            Update::Search(std::sync::Arc::new(SearchResults {
                query: query.into(),
                tracks: Vec::new(),
                albums: Vec::new(),
                artists: vec![crate::data::Entry::new(
                    crate::data::Kind::Artist,
                    "a",
                    "Blur",
                )],
                playlists: Vec::new(),
            }))
        };
        app.apply(answer("blu"));
        assert!(app.search.results.is_none());
        app.apply(answer("blur"));
        assert_eq!(
            app.search.section, 2,
            "focus lands on the first section with results"
        );
    }

    #[test]
    fn removing_trims_the_open_list_in_place() {
        let mut app = library_app();
        app.on_key(key(KeyCode::Char('g')));
        app.on_key(key(KeyCode::Char('l')));
        let rows = app.library_rows();
        app.library.selected = rows
            .iter()
            .position(|e| e.uri == "spotify:playlist:mine")
            .unwrap();
        app.on_key(key(KeyCode::Enter));
        let tracks = ["spotify:track:a", "spotify:track:b"]
            .map(|u| item(u).info())
            .to_vec();
        app.apply(Update::List(std::sync::Arc::new(ListPage {
            uri: "spotify:playlist:mine".into(),
            name: "mine".into(),
            tracks,
        })));
        app.on_key(key(KeyCode::Char('x')));
        assert!(matches!(app.popup, Some(Popup::Remove { .. })));
        app.on_key(key(KeyCode::Char('y')));
        app.apply(Update::Edited {
            playlist: "spotify:playlist:mine".into(),
            track: "spotify:track:a".into(),
            added: false,
        });
        let open = app.library.open.as_ref().unwrap();
        assert_eq!(app.opened_rows(open).len(), 1);
        let index = app.model.library.as_ref().unwrap();
        assert_eq!(index.playlists[0].count, Some(1));
    }

    #[test]
    fn go_to_artist_and_album_follow_the_selected_track() {
        use crate::data::{Entry, Kind, SearchResults};
        let mut app = library_app();
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Enter));
        app.apply(Update::Search(std::sync::Arc::new(SearchResults {
            query: "a".into(),
            tracks: Vec::new(),
            albums: Vec::new(),
            artists: vec![Entry::new(Kind::Artist, "spotify:artist:athena", "Athena")],
            playlists: Vec::new(),
        })));
        app.on_key(key(KeyCode::Enter));
        app.apply(Update::Artist(athena()));
        app.on_key(key(KeyCode::Char('g')));
        app.on_key(key(KeyCode::Char('b')));
        let open = |app: &App| app.search.open.as_ref().map(|o| o.entry.uri.clone());
        assert_eq!(open(&app).as_deref(), Some("spotify:album:holigan"));
        assert_eq!(
            app.page,
            Page::Search,
            "opened in place, not on the home tab"
        );
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(open(&app).as_deref(), Some("spotify:artist:athena"));
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(open(&app), None);

        app.on_key(key(KeyCode::Char('g')));
        app.on_key(key(KeyCode::Char('a')));
        assert_eq!(
            open(&app).as_deref(),
            Some("spotify:artist:athena"),
            "on the artists section the selected artist itself"
        );
        app.on_key(key(KeyCode::Char('g')));
        app.on_key(key(KeyCode::Char('a')));
        assert!(app.notice.is_some(), "already open: said, not silent");

        app.on_key(key(KeyCode::Char('1')));
        app.on_key(key(KeyCode::Char('g')));
        app.on_key(key(KeyCode::Char('a')));
        assert_eq!((app.page, app.tab), (Page::Home, 2), "home: the artist tab");
    }

    fn athena() -> std::sync::Arc<crate::spotify::meta::ArtistPage> {
        use crate::spotify::meta::{ArtistPage, Card, TrackInfo};
        std::sync::Arc::new(ArtistPage {
            uri: "spotify:artist:athena".into(),
            name: "Athena".into(),
            top: vec![TrackInfo {
                uri: "spotify:track:dam".into(),
                name: "Dam Üstüne Çul Serer".into(),
                artists: vec![("spotify:artist:athena".into(), "Athena".into())],
                duration: Duration::from_secs(1),
                album: Some(("spotify:album:holigan".into(), "Holigan".into())),
            }],
            related: vec![("spotify:artist:duman".into(), "Duman".into())],
            popularity: 60,
            releases: Vec::new(),
            discography: (0..7)
                .map(|i| Card {
                    uri: format!("spotify:album:{i}"),
                    name: format!("Album {i}"),
                    year: Some(2000 + i),
                    single: false,
                    cover: None,
                })
                .collect(),
        })
    }

    #[test]
    fn artist_page_moves_through_sections() {
        use crate::app::{POPULAR, RELATED, RELEASES};
        let mut app = library_app();
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Enter));
        app.apply(Update::Search(std::sync::Arc::new(
            crate::data::SearchResults {
                query: "a".into(),
                tracks: Vec::new(),
                albums: Vec::new(),
                artists: vec![crate::data::Entry::new(
                    crate::data::Kind::Artist,
                    "spotify:artist:athena",
                    "Athena",
                )],
                playlists: Vec::new(),
            },
        )));
        app.on_key(key(KeyCode::Enter));
        app.apply(Update::Artist(athena()));
        let view = |app: &App| {
            let v = &app.search.open.as_ref().unwrap().view;
            (v.section, v.selected[v.section])
        };
        assert_eq!(view(&app), (POPULAR, 0));
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(
            view(&app).0,
            RELEASES,
            "no liked tracks: down goes to the releases"
        );
        app.search.open.as_ref().unwrap().view.grid_cols.set(5);
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(view(&app), (RELEASES, 5), "down a grid row");
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(view(&app).0, RELATED);
        app.on_key(key(KeyCode::Enter));
        let open = app.search.open.as_ref().unwrap();
        assert_eq!(
            open.entry.uri, "spotify:artist:duman",
            "related artist opened in place"
        );
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(
            app.search.open.as_ref().unwrap().entry.uri,
            "spotify:artist:athena"
        );
    }

    #[test]
    fn arrows_navigate_and_comma_period_seek() {
        let mut app = App::new(Lang::Tr, Mode::Dark, crate::data::demo(), None);
        app.on_key(key(KeyCode::Right));
        assert_eq!(app.tab, 1, "→ is the next tab");
        app.on_key(key(KeyCode::Left));
        app.on_key(key(KeyCode::Left));
        assert_eq!(app.tab, 4, "← wraps to the last tab");
        let before = app.position();
        app.on_key(key(KeyCode::Char('.')));
        assert_eq!(app.position(), before + SEEK_STEP);
        app.on_key(key(KeyCode::Char(',')));
        assert_eq!(app.position(), before);
        app.on_key(key(KeyCode::Char('ç')));
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(app.position(), before + SEEK_STEP * 2);
        assert_eq!(app.tab, 4, "shift-arrows seek, they do not switch tabs");
        app.on_key(key(KeyCode::Char('ö')));
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        assert_eq!(app.position(), before);
    }

    #[test]
    fn an_album_opened_from_the_artist_tab_gets_its_tracks() {
        let mut app = App::new(Lang::Tr, Mode::Dark, Model::default(), None);
        app.apply(playing("now"));
        app.model.now.as_mut().unwrap().artist_uri = Some("spotify:artist:athena".into());
        app.apply(Update::Artist(athena()));
        app.on_key(key(KeyCode::Char('3')));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.artist_view.section, crate::app::RELEASES);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            app.home_open.as_ref().map(|o| o.entry.uri.as_str()),
            Some("spotify:album:0")
        );
        app.apply(Update::List(std::sync::Arc::new(ListPage {
            uri: "spotify:album:0".into(),
            name: "Album 0".into(),
            tracks: vec![item("spotify:track:x").info()],
        })));
        let open = app.home_open.as_ref().unwrap();
        assert_eq!(
            app.opened_rows(open).len(),
            1,
            "the home list is filled too"
        );
    }

    #[test]
    fn queue_and_device_popups() {
        use crate::spotify::model::Device;
        let mut app = App::new(Lang::Tr, Mode::Dark, Model::default(), None);
        app.apply(playing("now"));
        app.apply(Update::Queue(
            ["a", "b", "c"].iter().map(|u| item(u)).collect(),
        ));
        app.on_key(key(KeyCode::Char('g')));
        app.on_key(key(KeyCode::Char('k')));
        assert!(
            matches!(app.popup, Some(Popup::Queue { selected: 1, .. })),
            "opens on the next track"
        );
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Enter));
        assert!(app.popup.is_none());
        assert_eq!(app.notice.as_deref(), Some("▶ b"), "two skips ahead");

        app.on_key(key(KeyCode::Char('g')));
        app.on_key(key(KeyCode::Char('d')));
        assert!(app.model.devices.is_none(), "asked for fresh");
        let device = |name: &str, active: bool| Device {
            id: Some(name.into()),
            name: name.into(),
            kind: "Computer".into(),
            is_active: active,
            ..Device::default()
        };
        app.apply(Update::Devices(vec![
            device("phone", false),
            device("mac", true),
        ]));
        assert!(matches!(
            app.popup,
            Some(Popup::Devices { selected: 1, .. })
        ));
        app.on_key(key(KeyCode::Esc));
        assert!(app.popup.is_none());
    }

    #[test]
    fn stage_mode_lets_only_playback_keys_through() {
        let mut app = App::new(Lang::Tr, Mode::Dark, crate::data::demo(), None);
        app.on_key(key(KeyCode::Char('z')));
        assert!(app.stage);
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Char('/')));
        assert_eq!(app.page, Page::Home, "navigation waits behind the stage");
        let playing = app.playing;
        app.on_key(key(KeyCode::Char(' ')));
        assert_ne!(app.playing, playing, "space still plays and pauses");
        app.on_key(key(KeyCode::Char('z')));
        assert!(app.stage, "z only opens the stage");
        app.on_key(key(KeyCode::Backspace));
        assert!(!app.stage);
        assert_eq!(app.page, Page::Home, "⌫ leaves the stage, not the page");
    }

    #[test]
    fn backspace_from_home_goes_to_the_library() {
        let mut app = library_app();
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.page, Page::Library);
        assert!(
            !app.library.in_list,
            "on the collections, ready to pick a list"
        );
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.page, Page::Home, "and back home again");
    }

    #[test]
    fn slash_starts_a_new_search() {
        let mut app = library_app();
        app.on_key(key(KeyCode::Char('/')));
        for c in "austra".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Char('/')));
        assert!(app.search.query.is_empty() && app.search.typing);
        app.on_key(key(KeyCode::Esc));
        app.search.query = "daft".into();
        app.on_key(key(KeyCode::Char('i')));
        assert_eq!(app.search.query, "daft", "i edits the query instead");
    }
}
