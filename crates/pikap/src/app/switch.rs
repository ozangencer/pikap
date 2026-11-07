//! Two services in one pikap (`g S`). The app works on one service at a time; the
//! other one's share (what it plays, its library and pages) waits in a [`Side`] and
//! trades places with the active one. A service's worker starts the first time it is
//! switched to, and a waiting worker is told to rest: Spotify stops polling (its quota),
//! YouTube Music stops reporting.

use std::time::{Duration, Instant};

use super::{App, ArtistView, LibraryView, Live, Opened, Page, SearchView};
use crate::backend::{Command, Quota, Service};
use crate::data::{Model, Track};

/// Starts a service's worker; fails with a message for the screen (not signed in…).
pub type Spawn = Box<dyn FnMut(Service) -> Result<Live, String>>;

/// One service's share of the app.
pub(super) struct Side {
    service: Service,
    live: Option<Live>,
    model: Model,
    page: Page,
    history: Vec<Page>,
    library: LibraryView,
    search: SearchView,
    selected: usize,
    playing: bool,
    loading: bool,
    quota: Quota,
    synced: bool,
    artist_view: ArtistView,
    home_open: Option<Opened>,
    home_trail: Vec<Opened>,
    upcoming: Vec<Track>,
    context: Option<String>,
    playing_context: Option<String>,
    resume_fallback: Option<(Option<String>, String)>,
    position: Duration,
    last_tick: Instant,
}

impl Side {
    fn fresh(service: Service, live: Live) -> Self {
        Self {
            service,
            live: Some(live),
            model: Model::default(),
            page: Page::Home,
            history: Vec::new(),
            library: LibraryView::default(),
            search: SearchView::default(),
            selected: 0,
            playing: false,
            loading: false,
            quota: Quota::Normal,
            synced: false,
            artist_view: ArtistView::default(),
            home_open: None,
            home_trail: Vec::new(),
            upcoming: Vec::new(),
            context: None,
            playing_context: None,
            resume_fallback: None,
            position: Duration::ZERO,
            last_tick: Instant::now(),
        }
    }
}

pub fn other(service: Service) -> Service {
    match service {
        Service::Spotify => Service::YtMusic,
        Service::YtMusic => Service::Spotify,
    }
}

impl App {
    /// The service `g S` goes to.
    pub fn other_service(&self) -> Service {
        self.other
            .as_ref()
            .map_or_else(|| other(self.service), |s| s.service)
    }

    /// `g S`: pauses this service and shows the other one, starting it the first time.
    pub(super) fn switch_service(&mut self) {
        let target = self.other_service();
        let incoming = match self.other.take() {
            Some(side) => side,
            None => {
                let Some(spawn) = self.spawn.as_mut() else {
                    return;
                };
                match spawn(target) {
                    Ok(live) => Side::fresh(target, live),
                    Err(message) => {
                        self.notice = Some(message);
                        return;
                    }
                }
            }
        };
        if self.playing {
            self.command(Command::SetPlaying(false));
            self.playing = false;
        }
        self.command(Command::Active(false));
        let outgoing = self.take_side();
        self.put_side(incoming);
        self.other = Some(outgoing);
        self.command(Command::Active(true));
        self.popup = None;
        self.leader = false;
        self.pending_seek = None;
        self.pending_volume = None;
        self.hold_position = None;
        self.hold_volume = None;
        self.hover = None;
        self.prepared = None;
        self.list_offset.set(None);
        self.notice = Some(format!("● {}", self.service.name()));
        if let Some(remember) = self.remember_service.as_ref() {
            remember(self.service);
        }
    }

    fn take_side(&mut self) -> Side {
        Side {
            service: self.service,
            live: self.live.take(),
            model: std::mem::take(&mut self.model),
            page: self.page,
            history: std::mem::take(&mut self.history),
            library: std::mem::take(&mut self.library),
            search: std::mem::take(&mut self.search),
            selected: self.selected,
            playing: self.playing,
            loading: self.loading,
            quota: self.quota,
            synced: self.synced,
            artist_view: std::mem::take(&mut self.artist_view),
            home_open: self.home_open.take(),
            home_trail: std::mem::take(&mut self.home_trail),
            upcoming: std::mem::take(&mut self.upcoming),
            context: self.context.take(),
            playing_context: self.playing_context.take(),
            resume_fallback: self.resume_fallback.take(),
            position: self.position,
            last_tick: self.last_tick,
        }
    }

    fn put_side(&mut self, side: Side) {
        self.service = side.service;
        self.live = side.live;
        self.model = side.model;
        self.page = side.page;
        self.history = side.history;
        self.library = side.library;
        self.search = side.search;
        self.selected = side.selected;
        self.playing = side.playing;
        self.loading = side.loading;
        self.quota = side.quota;
        self.synced = side.synced;
        self.artist_view = side.artist_view;
        self.home_open = side.home_open;
        self.home_trail = side.home_trail;
        self.upcoming = side.upcoming;
        self.context = side.context;
        self.playing_context = side.playing_context;
        self.resume_fallback = side.resume_fallback;
        self.position = side.position;
        // The clock only runs while playing, and this side was paused on the way out.
        self.last_tick = Instant::now().max(side.last_tick);
    }

    /// Both services' workers, the active one first, for closing.
    pub(super) fn lives(&self) -> impl Iterator<Item = &Live> {
        self.live
            .iter()
            .chain(self.other.as_ref().and_then(|s| s.live.as_ref()))
    }
}
