mod app;
mod art;
mod backend;
mod data;
mod gallery;
mod i18n;
mod kitty;
mod log;
mod spotify;
mod term;
mod theme;
mod ui;
mod ytmusic;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use app::{App, Flow, Live};
use gallery::{CellPx, Gallery};
use i18n::{Lang, Strings};
use kitty::Kitty;
use spotify::{api::Api, auth, config, meta, worker};
use theme::Mode;

/// ~20 fps keeps the equalizer smooth; ratatui only redraws cells that changed.
const FRAME: Duration = Duration::from_millis(50);

const USAGE: &str =
    "pikap [auth [spotify|ytmusic] [--force]] [--backend spotify|ytmusic] [--lang tr|en] [--theme auto|dark|light] [--demo] [--check-terminal] [--version]

  auth     sign in: Spotify (the Web API and librespot, opens the browser) or YouTube
           Music (optional, adds your library); asks which unless named
  --force  with auth: sign in again even when already connected
  --backend  the music service (default: PIKAP_BACKEND, then spotify); YouTube Music
           plays through mpv and needs mpv and yt-dlp (brew install mpv yt-dlp)
  --lang   UI language (default: PIKAP_LANG, then the system locale)
  --theme  colours for a dark or light terminal (default: PIKAP_THEME, then auto)
  --demo   show the built-in sample screen instead of Spotify
  --check-terminal  tell whether this terminal can show pikap's images (kitty graphics);
           PIKAP_GRAPHICS=on|off overrides the answer";

#[derive(Default)]
struct Args {
    auth: bool,
    force: bool,
    demo: bool,
    /// The service to show, when named (`--backend`, `PIKAP_BACKEND`).
    backend: Option<backend::Service>,
    /// The service `auth` signs in to, when named.
    service: Option<backend::Service>,
    lang: Option<Lang>,
    theme: Option<Mode>,
}

fn parse_args() -> Result<Args> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "auth" => args.auth = true,
            "spotify" | "ytmusic" if args.auth => args.service = backend::Service::from_key(&arg),
            "--force" => args.force = true,
            "--demo" => args.demo = true,
            "--backend" => {
                let v = it.next().unwrap_or_default();
                args.backend =
                    Some(backend::Service::from_key(&v).ok_or_else(|| anyhow!("--backend: {v}?"))?);
            }
            "--lang" => {
                let v = it.next().unwrap_or_default();
                args.lang = Some(Lang::parse(&v).ok_or_else(|| anyhow!("--lang: {v}?"))?);
            }
            "--theme" => {
                let v = it.next().unwrap_or_default();
                args.theme = match v.as_str() {
                    "auto" => None,
                    _ => Some(Mode::parse(&v).ok_or_else(|| anyhow!("--theme: {v}?"))?),
                };
            }
            "--check-terminal" => {
                ratatui::crossterm::terminal::enable_raw_mode()?;
                let shows = term::graphics_enabled();
                ratatui::crossterm::terminal::disable_raw_mode()?;
                let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
                if shows {
                    println!("{program}: kitty graphics yes, pikap shows images");
                } else {
                    println!("{program}: kitty graphics no, pikap runs as text only");
                }
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("pikap {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}\n\n{USAGE}"),
        }
    }
    if args.backend.is_none() {
        args.backend = std::env::var("PIKAP_BACKEND")
            .ok()
            .and_then(|v| backend::Service::from_key(&v));
    }
    if args.theme.is_none() {
        args.theme = std::env::var("PIKAP_THEME")
            .ok()
            .and_then(|v| Mode::parse(&v));
    }
    Ok(args)
}

/// `pikap auth` with no service named; Spotify unless the answer is 2.
fn ask_service(s: &Strings) -> Result<backend::Service> {
    use std::io::Write;
    print!("{}", s.auth_which);
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(if line.trim() == "2" {
        backend::Service::YtMusic
    } else {
        backend::Service::Spotify
    })
}

/// Where the service last shown is kept, so the next pikap opens on it.
fn last_service_path() -> std::path::PathBuf {
    config::cache_dir().join("service")
}

fn last_service() -> Option<backend::Service> {
    let text = std::fs::read_to_string(last_service_path()).ok()?;
    backend::Service::from_key(&text)
}

fn remember_service(service: backend::Service) {
    let _ = std::fs::create_dir_all(config::cache_dir());
    let _ = std::fs::write(last_service_path(), service.key());
}

/// Starts a service's worker. Spotify needs its sign-in; YouTube Music works without.
fn spawn_service(service: backend::Service, s: &Strings) -> Result<Live, String> {
    let live = match service {
        backend::Service::YtMusic => start_worker(service, ytmusic::worker::run),
        backend::Service::Spotify => {
            let cfg = config::load().map_err(|_| s.need_auth.to_owned())?;
            let token = auth::load_token().ok_or_else(|| s.need_auth.to_owned())?;
            let api = Arc::new(Api::new(http().map_err(|e| e.to_string())?, cfg, token));
            start_worker(service, move |commands, updates| {
                worker::run(api, commands, updates)
            })
        }
    };
    live.map_err(|e| format!("{e:#}"))
}

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("pikap/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(15))
        .build()?)
}

fn main() -> Result<()> {
    let args = parse_args()?;
    log::init();
    let lang = args.lang.unwrap_or_else(Lang::detect);
    let s = lang.strings();
    if args.auth {
        let service = match args.service.or(args.backend) {
            Some(service) => service,
            None => ask_service(s)?,
        };
        return match service {
            backend::Service::Spotify => sign_in(s, args.force),
            backend::Service::YtMusic => ytmusic::auth::sign_in(s, args.force),
        };
    }

    let (model, live) = if args.demo {
        (data::demo(), None)
    } else {
        let service = args
            .backend
            .or_else(last_service)
            .unwrap_or(backend::Service::Spotify);
        match spawn_service(service, s) {
            Ok(live) => (data::Model::default(), Some(live)),
            Err(message) => {
                eprintln!("{message}");
                std::process::exit(1);
            }
        }
    };

    let mut terminal = ratatui::init();
    let mode = args.theme.unwrap_or_else(Mode::detect);
    let graphics = term::graphics_enabled();
    let demo = live.is_none();
    let mut app = App::new(lang, mode, model, live);
    if !demo {
        app.spawn = Some(Box::new(move |service| spawn_service(service, s)));
        app.remember_service = Some(Box::new(remember_service));
        remember_service(app.service);
    }
    app.graphics = graphics;
    if !graphics {
        app.notice = Some(s.no_graphics.to_owned());
    }
    let result = run(&mut terminal, app, graphics);
    let _ = kitty::delete_all(terminal.backend_mut());
    ratatui::restore();
    result
}

/// `pikap auth`: the user's Developer app for the Web API, then Spotify's desktop client
/// for librespot. Each step is skipped when already connected, unless `force`.
fn sign_in(s: &Strings, force: bool) -> Result<()> {
    let cfg = match config::load() {
        Ok(cfg) => cfg,
        Err(_) => {
            setup(s)?;
            config::load()?
        }
    };
    println!("{}", (s.using_config)(&cfg.source.display().to_string()));
    let announce = |url: &str| {
        println!("\n{}\n{url}\n", s.auth_open);
        println!("{}", (s.auth_wait)(&cfg.redirect_uri));
    };
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let http = http()?;
        println!("\n{}", (s.auth_step)(1, s.auth_web));
        if !force && auth::load_token().is_some() {
            println!("  {}", s.auth_skip);
        } else {
            auth::login(&http, &cfg, announce).await?;
            println!("  ✓");
        }
        println!("\n{}", (s.auth_step)(2, s.auth_meta));
        if !force && meta::has_credentials() {
            println!("  {}", s.auth_skip);
        } else {
            let token = auth::login_librespot(&http, &cfg.redirect_uri, announce).await?;
            meta::store_credentials(token).await?;
            println!("  ✓");
        }
        anyhow::Ok(())
    })?;
    println!(
        "\n{}",
        (s.auth_done)(&auth::token_path().display().to_string())
    );
    Ok(())
}

/// First run: walks through creating the Spotify Developer app pikap needs and saves
/// its Client ID, so nobody has to write the config file by hand.
fn setup(s: &Strings) -> Result<()> {
    use std::io::Write;
    const DASHBOARD: &str = "https://developer.spotify.com/dashboard";
    println!("{}\n  {DASHBOARD}\n", s.setup_intro);
    for (i, step) in s.setup_steps.iter().enumerate() {
        println!("  {}. {step}", i + 1);
    }
    println!();
    let _ = std::process::Command::new("open").arg(DASHBOARD).status();
    let client_id = loop {
        print!("{}", s.setup_prompt);
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let id = line.trim();
        if id.is_empty() {
            bail!("{}", s.setup_cancel);
        }
        if config::valid_client_id(id) {
            break id.to_owned();
        }
        println!("{}", s.setup_invalid);
    };
    let path = config::save_client_id(&client_id)?;
    println!("{}", (s.setup_saved)(&path.display().to_string()));
    Ok(())
}

/// Runs a service's worker on its own thread and runtime; the UI loop stays synchronous.
fn start_worker<F, Fut>(service: backend::Service, worker: F) -> Result<Live>
where
    F: FnOnce(
            tokio::sync::mpsc::UnboundedReceiver<backend::Command>,
            std::sync::mpsc::Sender<backend::Update>,
        ) -> Fut
        + Send
        + 'static,
    Fut: std::future::Future<Output = ()>,
{
    let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let (update_tx, updates) = std::sync::mpsc::channel();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    std::thread::Builder::new()
        .name(format!("{service:?}").to_lowercase())
        .spawn(move || runtime.block_on(worker(command_rx, update_tx)))?;
    Ok(Live {
        service,
        commands,
        updates,
    })
}

/// `graphics` is false in terminals without kitty graphics, which would print the
/// image data as text: nothing is sent to them and titles stay text.
fn run(terminal: &mut DefaultTerminal, mut app: App, graphics: bool) -> Result<()> {
    let mut gallery = Gallery::new(None);
    let mut kitty = Kitty::default();
    let mut last_size = None;

    loop {
        app.pump();
        app.tick();
        let size = terminal.size()?;
        if last_size != Some(size) {
            kitty.invalidate();
            last_size = Some(size);
        }
        let cover = app.model.now.as_ref().and_then(|n| n.cover.clone());
        if gallery.set_cover(cover.as_deref()) {
            app.cover_color = gallery.cover_color();
        }
        let title = app
            .model
            .now
            .as_ref()
            .map(|n| i18n::display_upper(&n.title))
            .unwrap_or_default();

        let cell = CellPx::detect();
        let title_as_image = graphics && gallery.has_title_font();
        let mut slots = None;
        terminal.draw(|f| slots = Some(ui::render(f, &app, cell, title_as_image)))?;
        if let Some(slots) = slots.filter(|_| graphics) {
            let images = gallery.graphics(&slots, cell, &title, app.p());
            kitty.sync(terminal.backend_mut(), &images)?;
        }

        if event::poll(FRAME)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match app.on_key(key) {
                    Flow::Quit => {
                        // YouTube Music plays through mpv: closing pikap stops it.
                        for done in app.shutdown() {
                            let _ = done.recv_timeout(Duration::from_secs(1));
                        }
                        break;
                    }
                    Flow::Redraw => {
                        kitty.reset(terminal.backend_mut())?;
                        terminal.clear()?;
                    }
                    Flow::Continue => {}
                },
                Event::Resize(..) => kitty.invalidate(),
                _ => {}
            }
        }
    }
    Ok(())
}
