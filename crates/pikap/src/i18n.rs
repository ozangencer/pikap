//! UI strings. Each language is one `Strings` value, so a missing translation is a
//! compile error. Content that comes from Spotify (playlist names, titles) is never
//! translated.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Tr,
    En,
}

impl Lang {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().get(..2)? {
            "tr" => Some(Self::Tr),
            "en" => Some(Self::En),
            _ => None,
        }
    }

    /// `PIKAP_LANG`, then the POSIX locale (unless it is `C`/`POSIX`), then the first
    /// macOS preferred language, falling back to English.
    pub fn detect() -> Self {
        if let Some(lang) = std::env::var("PIKAP_LANG")
            .ok()
            .and_then(|v| Self::parse(&v))
        {
            return lang;
        }
        for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(v) = std::env::var(var)
                && !v.is_empty()
                && !v.starts_with('C')
                && v != "POSIX"
            {
                return Self::parse(&v).unwrap_or(Self::En);
            }
        }
        std::process::Command::new("defaults")
            .args(["read", "-g", "AppleLanguages"])
            .output()
            .ok()
            .and_then(|o| {
                let out = String::from_utf8_lossy(&o.stdout).into_owned();
                let first = out.split('"').nth(1)?.to_owned();
                Self::parse(&first)
            })
            .unwrap_or(Self::En)
    }

    pub fn strings(self) -> &'static Strings {
        match self {
            Self::Tr => &TR,
            Self::En => &EN,
        }
    }
}

pub struct Strings {
    pub library: &'static str,
    pub liked_songs: &'static str,
    pub vol: &'static str,
    pub tabs: [&'static str; 5],
    pub col_track: &'static str,
    pub col_artist: &'static str,
    pub col_time: &'static str,
    /// Shuffle off/on.
    pub shuffle: [&'static str; 2],
    /// Repeat off/context/track.
    pub repeat: [&'static str; 3],
    pub queue_loops: &'static str,
    pub paused: &'static str,
    pub radio: &'static str,
    pub leader_title: &'static str,
    pub leader_close: &'static str,
    /// Labels for `app::LEADER_KEYS`, same order.
    pub leader_labels: [&'static str; 12],
    /// Status line hints as (key, label), per page.
    pub hints: &'static [(&'static str, &'static str)],
    pub hints_library: &'static [(&'static str, &'static str)],
    pub hints_search: &'static [(&'static str, &'static str)],
    pub hints_typing: &'static [(&'static str, &'static str)],
    /// The playback keys shown along the bottom of stage mode.
    pub hints_stage: &'static [(&'static str, &'static str)],
    /// Shorter hints for the narrow (sidebar) layout.
    pub hints_compact: [(&'static str, &'static str); 3],
    /// Narrow hints on the library and search pages.
    pub hints_compact_page: [(&'static str, &'static str); 3],
    pub quota: &'static str,
    pub quota_ok: &'static str,
    pub near: fn(&str) -> String,
    pub screen_missing: fn(char, &str) -> String,
    pub too_small: fn((u16, u16), (u16, u16)) -> String,
    pub theme_now: fn(&str) -> String,
    pub idle_title: &'static str,
    pub connecting: &'static str,
    pub idle_hint: &'static str,
    pub idle_last: fn(&str, &str) -> String,
    pub resume_launching: &'static str,
    pub resume_on: fn(&str) -> String,
    pub resume_no_device: &'static str,
    pub quota_retrying: &'static str,
    pub quota_hold: fn(usize) -> String,
    pub need_auth: &'static str,
    pub auth_open: &'static str,
    pub auth_wait: fn(&str) -> String,
    pub auth_done: fn(&str) -> String,
    pub using_config: fn(&str) -> String,
    pub loading: &'static str,
    pub meta_missing: &'static str,
    pub no_lyrics: &'static str,
    pub lyrics_unsynced: &'static str,
    pub track_count: fn(usize) -> String,
    pub artist_top: fn(&str) -> String,
    pub radio_of: fn(&str) -> String,
    pub auth_step: fn(u8, &str) -> String,
    pub auth_skip: &'static str,
    pub auth_web: &'static str,
    pub auth_meta: &'static str,
    pub search: &'static str,
    pub made_for_you: &'static str,
    pub your_library: &'static str,
    pub playlists: &'static str,
    pub albums: &'static str,
    pub recently_played: &'static str,
    pub search_prompt: &'static str,
    pub search_limit: &'static str,
    /// Search sections, keyed 1-4.
    pub search_sections: [&'static str; 4],
    pub searching: &'static str,
    pub no_results: &'static str,
    pub open_hint: &'static str,
    pub nothing_playing: &'static str,
    pub add_title: &'static str,
    pub add_hint: &'static str,
    pub adding: fn(&str, &str) -> String,
    pub added: fn(&str, &str) -> String,
    pub removed: fn(&str, &str) -> String,
    pub confirm_remove: fn(&str, &str) -> String,
    pub not_editable: &'static str,
    pub no_editable: &'static str,
    pub nothing_selected: &'static str,
    pub no_made_for_you: &'static str,
    pub nothing_to_go: &'static str,
    pub hints_artist: &'static [(&'static str, &'static str)],
    pub popularity: &'static str,
    pub artist_play: &'static str,
    pub radio_short: &'static str,
    pub artist_popular: &'static str,
    pub artist_liked: fn(usize) -> String,
    pub artist_discography: &'static str,
    pub artist_releases: fn(usize, usize) -> String,
    pub artist_related: &'static str,
    pub single: &'static str,
    pub queue_title: &'static str,
    pub queue_hint: &'static str,
    pub queue_empty: &'static str,
    pub queued: fn(&str) -> String,
    pub devices_title: &'static str,
    pub devices_hint: &'static str,
    pub no_devices: &'static str,
    pub device_restricted: &'static str,
    pub device_kind: fn(&str) -> String,
    pub active: &'static str,
    pub stage: &'static str,
    pub stage_exit: &'static str,
    pub on_hold: &'static str,
    pub no_graphics: &'static str,
    pub setup_intro: &'static str,
    pub setup_steps: [&'static str; 4],
    pub setup_prompt: &'static str,
    pub setup_invalid: &'static str,
    pub setup_cancel: &'static str,
    pub setup_saved: fn(&str) -> String,
    pub library_held: fn(std::time::Duration) -> String,
    pub already_open: fn(&str) -> String,
}

pub static TR: Strings = Strings {
    library: "kütüphane",
    liked_songs: "Beğenilenler",
    vol: "ses",
    tabs: ["liste", "sözler", "sanatçı", "albüm", "keşif"],
    col_track: "parça",
    col_artist: "sanatçı",
    col_time: "süre",
    shuffle: ["⇄ sıralı", "⇄ karışık"],
    repeat: ["↻ kapalı", "↻ liste", "↻ parça"],
    queue_loops: "↻ liste başa dönüyor",
    paused: "▌▌ duraklatıldı",
    radio: "radyo başlat",
    leader_title: " g ▸ git ",
    leader_close: "g / esc kapat",
    leader_labels: [
        "kütüphane",
        "ara",
        "beğenilenler",
        "son çalınanlar",
        "kuyruk",
        "sözler",
        "çalana git",
        "sanatçıya git",
        "albüme git",
        "made for you",
        "cihaz seç",
        "sahne modu",
    ],
    hints: &[
        ("/", "ara"),
        ("1-5 ← →", "sekme"),
        ("space", "çal/duraklat"),
        ("n p", "ileri/geri"),
        ("ö ç", "sar"),
        ("s R", "karışık/tekrar"),
        ("a e", "listeye/kuyruğa ekle"),
        ("z", "sahne"),
        ("q", "çık"),
    ],
    hints_library: &[
        ("↵", "aç/çal"),
        ("P", "listeyi baştan çal"),
        ("tab", "bölme"),
        ("a", "listeye ekle"),
        ("x", "listeden çıkar"),
        ("⌫", "geri"),
    ],
    hints_search: &[
        ("↵", "aç/çal"),
        ("1-4", "bölüm"),
        ("tab", "sonraki"),
        ("/", "yeni arama"),
        ("a", "listeye ekle"),
        ("⌫", "geri"),
    ],
    hints_typing: &[("↵", "ara"), ("esc", "yazmayı bırak"), ("⌫", "sil")],
    hints_stage: &[
        ("space", "çal/duraklat"),
        ("n p", "ileri/geri"),
        ("ö ç", "sar"),
        ("+ -", "ses"),
        ("f", "beğen"),
        ("s R", "karışık/tekrar"),
        ("⌫ esc", "sahneden çık"),
    ],
    hints_compact: [("/", "ara"), ("space", "çal"), ("q", "çık")],
    hints_compact_page: [("↵", "aç"), ("tab", "bölme"), ("⌫", "geri")],
    quota: "kota",
    quota_ok: "normal",
    near: |artist| format!("keşif · {}{} yakın", artist, dative_suffix(artist)),
    screen_missing: |key, label| format!("g {key} → {label}: bu ekran henüz yok"),
    too_small: |min, now| {
        format!(
            "pikap en az {}x{} hücre ister (şu an {}x{})",
            min.0, min.1, now.0, now.1
        )
    },
    theme_now: |name| format!("tema: {name}"),
    idle_title: "Spotify'da şu an çalan bir şey yok",
    connecting: "Spotify'a bağlanılıyor…",
    idle_hint: "↵ kaldığın yerden devam et  ·  g l kütüphane  ·  / ara  ·  ya da Spotify'da başlat, pikap yakalar",
    idle_last: |title, artist| format!("son çalınan: {title} · {artist}"),
    resume_launching: "Spotify açılıyor…",
    resume_on: |device| format!("▶ {device} üzerinde devam ediliyor"),
    resume_no_device: "Spotify cihazı bulunamadı. Spotify.app kurulu mu?",
    quota_retrying: "yavaşlatıldı",
    quota_hold: |n| format!("beklemede: {n}"),
    need_auth: "pikap henüz Spotify'a bağlı değil. Önce şunu çalıştır: pikap auth (kurulumu da o yapar)",
    auth_open: "Spotify izin sayfası tarayıcıda açılıyor. Açılmazsa bu adresi tarayıcına yapıştır:",
    auth_wait: |uri| format!("Spotify'dan dönüş bekleniyor ({uri}) …"),
    auth_done: |path| format!("Giriş tamam, token kaydedildi: {path}\nArtık pikap'ı açabilirsin."),
    using_config: |path| format!("Spotify uygulaması: {path}"),
    loading: "yükleniyor…",
    meta_missing: "Bu sekme librespot girişi istiyor: pikap auth",
    no_lyrics: "Bu parçanın sözleri yok.",
    lyrics_unsynced: "zamanlanmamış sözler",
    track_count: |n| format!("{n} parça"),
    artist_top: |name| format!("{name} · en çok dinlenenler"),
    radio_of: |title| format!("{title} radyosu"),
    auth_step: |n, label| format!("[{n}/2] {label}"),
    auth_skip: "zaten bağlı, atlanıyor (yeniden bağlamak için: pikap auth --force)",
    auth_web: "Spotify Web API (kendi Developer uygulaman)",
    auth_meta: "Spotify metadata (librespot: sanatçı, albüm, radyo, sözler)",
    search: "ara",
    made_for_you: "made for you",
    your_library: "kütüphanen",
    playlists: "playlist'lerin",
    albums: "albümlerin",
    recently_played: "Son çalınanlar",
    search_prompt: "parça, albüm, sanatçı ya da playlist yaz, ↵ ile ara",
    search_limit: "tür başına 10 sonuç",
    search_sections: ["parçalar", "albümler", "sanatçılar", "playlist'ler"],
    searching: "aranıyor…",
    no_results: "sonuç yok",
    open_hint: "soldan bir liste seç, ↵ ile aç",
    nothing_playing: "Spotify'da çalan yok",
    add_title: " listeye ekle ",
    add_hint: "↵ ekle   esc vazgeç",
    adding: |track, list| format!("“{track}”, {list} listesine ekleniyor…"),
    added: |track, list| format!("“{track}”, {list} listesine eklendi"),
    removed: |track, list| format!("“{track}”, {list} listesinden çıkarıldı"),
    confirm_remove: |track, list| {
        format!("“{track}”, {list} listesinden çıkarılsın mı?   y evet   n hayır")
    },
    not_editable: "Bu liste senin değil, değiştirilemez.",
    no_editable: "Düzenleyebileceğin bir playlist yok.",
    nothing_selected: "Seçili parça yok.",
    no_made_for_you: "Made For You listesi yok: config.toml'daki made_for_you'ya adres ekle.",
    nothing_to_go: "Seçili satırın sanatçısı ya da albümü yok.",
    hints_artist: &[
        ("tab", "bölüm"),
        ("← → ↑ ↓", "gez"),
        ("↵", "çal/aç"),
        ("P", "popülerleri çal"),
        ("r", "sanatçı radyosu"),
        ("a", "listeye ekle"),
        ("⌫", "geri"),
    ],
    popularity: "popülerlik",
    artist_play: "popülerleri çal",
    radio_short: "radyo",
    artist_popular: "popüler",
    artist_liked: |n| format!("♥ beğendiklerin · {n}"),
    artist_discography: "diskografi",
    artist_releases: |albums, singles| format!("{albums} albüm · {singles} single"),
    artist_related: "benzer",
    single: "single",
    queue_title: " kuyruk ",
    queue_hint: "↵ buna atla   esc kapat   (Spotify kuyruktan silmeye izin vermiyor)",
    queue_empty: "Çalan bir şey yok, kuyruk boş.",
    queued: |title| format!("“{title}” kuyruğa eklendi"),
    devices_title: " cihaz ",
    devices_hint: "↵ çalmayı buraya taşı   esc kapat",
    no_devices: "Spotify cihazı yok: Spotify.app'i ya da telefonda Spotify'ı aç.",
    device_restricted: "Bu cihaz Web API'den yönetilemiyor.",
    device_kind: |kind| {
        match kind {
            "Computer" => "bilgisayar",
            "Smartphone" => "telefon",
            "Speaker" => "hoparlör",
            "TV" => "televizyon",
            "Tablet" => "tablet",
            "CastAudio" | "CastVideo" => "cast",
            other => other,
        }
        .to_owned()
    },
    active: "çalıyor",
    stage: "sahne",
    stage_exit: "⌫ / esc çık",
    on_hold: "Spotify şu an bunu kabul etmiyor (istek sınırı), sonra tekrar dene.",
    no_graphics: "Bu terminal görselleri gösteremiyor (kitty graphics); kapak ve plak için Ghostty, kitty ya da WezTerm.",
    setup_intro: "pikap kendi Spotify Developer uygulamanı kullanır (Spotify ortak bir anahtar vermiyor). Panel tarayıcıda açılıyor:",
    setup_steps: [
        "Bir uygulama oluştur; ad ve açıklama serbest. API'lerden Web API'yi işaretle.",
        "Redirect URI olarak tam şunu ekle: http://127.0.0.1:8989/login",
        "Settings → User Management: Spotify hesabının e-postasını ekle.",
        "Basic Information'daki Client ID'yi kopyala.",
    ],
    setup_prompt: "Client ID: ",
    setup_invalid: "Bu bir Client ID'ye benzemiyor (32 harf ve rakam). Tekrar dene ya da çıkmak için ↵.",
    setup_cancel: "Durduruldu. Client ID'yi aldığında `pikap auth`'u yeniden çalıştır.",
    setup_saved: |path| format!("Kaydedildi: {path}"),
    library_held: |wait| match wait.as_secs() {
        s if s >= 3600 => format!("Spotify istek sınırı · ~{} sa sonra", s.div_ceil(3600)),
        s => format!("Spotify istek sınırı · ~{} dk sonra", s.div_ceil(60)),
    },
    already_open: |name| format!("{name} zaten açık"),
};

pub static EN: Strings = Strings {
    library: "library",
    liked_songs: "Liked Songs",
    vol: "vol",
    tabs: ["list", "lyrics", "artist", "album", "discover"],
    col_track: "track",
    col_artist: "artist",
    col_time: "time",
    shuffle: ["⇄ in order", "⇄ shuffle"],
    repeat: ["↻ off", "↻ list", "↻ track"],
    queue_loops: "↻ the list starts over",
    paused: "▌▌ paused",
    radio: "start radio",
    leader_title: " g ▸ go ",
    leader_close: "g / esc close",
    leader_labels: [
        "library",
        "search",
        "liked songs",
        "recently played",
        "queue",
        "lyrics",
        "jump to playing",
        "go to artist",
        "go to album",
        "made for you",
        "pick device",
        "stage mode",
    ],
    hints: &[
        ("/", "search"),
        ("1-5 ← →", "tab"),
        ("space", "play/pause"),
        ("n p", "next/prev"),
        ("⇧← ⇧→", "seek"),
        ("s R", "shuffle/repeat"),
        ("a e", "add to playlist/queue"),
        ("z", "stage"),
        ("q", "quit"),
    ],
    hints_library: &[
        ("↵", "open/play"),
        ("P", "play the whole list"),
        ("tab", "pane"),
        ("a", "add to playlist"),
        ("x", "remove"),
        ("⌫", "back"),
    ],
    hints_search: &[
        ("↵", "open/play"),
        ("1-4", "section"),
        ("tab", "next"),
        ("/", "new search"),
        ("a", "add to playlist"),
        ("⌫", "back"),
    ],
    hints_typing: &[("↵", "search"), ("esc", "stop typing"), ("⌫", "delete")],
    hints_stage: &[
        ("space", "play/pause"),
        ("n p", "next/prev"),
        ("⇧← ⇧→", "seek"),
        ("+ -", "volume"),
        ("f", "like"),
        ("s R", "shuffle/repeat"),
        ("⌫ esc", "leave stage"),
    ],
    hints_compact: [("/", "search"), ("space", "play"), ("q", "quit")],
    hints_compact_page: [("↵", "open"), ("tab", "pane"), ("⌫", "back")],
    quota: "quota",
    quota_ok: "ok",
    near: |artist| format!("discover · close to {artist}"),
    screen_missing: |key, label| format!("g {key} → {label}: not built yet"),
    too_small: |min, now| {
        format!(
            "pikap needs at least {}x{} cells (now {}x{})",
            min.0, min.1, now.0, now.1
        )
    },
    theme_now: |name| format!("theme: {name}"),
    idle_title: "Nothing is playing on Spotify",
    connecting: "Connecting to Spotify…",
    idle_hint: "↵ resume where you left off  ·  g l library  ·  / search  ·  or start in Spotify, pikap picks it up",
    idle_last: |title, artist| format!("last played: {title} · {artist}"),
    resume_launching: "Opening Spotify…",
    resume_on: |device| format!("▶ resuming on {device}"),
    resume_no_device: "No Spotify device found. Is Spotify.app installed?",
    quota_retrying: "throttled",
    quota_hold: |n| format!("on hold: {n}"),
    need_auth: "pikap is not connected to Spotify yet. Run this first: pikap auth (it also sets things up)",
    auth_open: "Opening the Spotify consent page in your browser. If it does not open, paste this address:",
    auth_wait: |uri| format!("Waiting for Spotify to redirect to {uri} …"),
    auth_done: |path| format!("Signed in, token saved to {path}\nYou can start pikap now."),
    using_config: |path| format!("Spotify app from {path}"),
    loading: "loading…",
    meta_missing: "This tab needs the librespot sign-in: pikap auth",
    no_lyrics: "No lyrics for this track.",
    lyrics_unsynced: "unsynced lyrics",
    track_count: |n| {
        if n == 1 {
            "1 track".to_owned()
        } else {
            format!("{n} tracks")
        }
    },
    artist_top: |name| format!("{name} · top tracks"),
    radio_of: |title| format!("{title} radio"),
    auth_step: |n, label| format!("[{n}/2] {label}"),
    auth_skip: "already connected, skipping (to reconnect: pikap auth --force)",
    auth_web: "Spotify Web API (your Developer app)",
    auth_meta: "Spotify metadata (librespot: artists, albums, radio, lyrics)",
    search: "search",
    made_for_you: "made for you",
    your_library: "your library",
    playlists: "your playlists",
    albums: "saved albums",
    recently_played: "Recently played",
    search_prompt: "type a track, album, artist or playlist, ↵ to search",
    search_limit: "10 results per kind",
    search_sections: ["tracks", "albums", "artists", "playlists"],
    searching: "searching…",
    no_results: "no results",
    open_hint: "pick a list on the left, ↵ opens it",
    nothing_playing: "Nothing is playing on Spotify",
    add_title: " add to playlist ",
    add_hint: "↵ add   esc cancel",
    adding: |track, list| format!("adding “{track}” to {list}…"),
    added: |track, list| format!("added “{track}” to {list}"),
    removed: |track, list| format!("removed “{track}” from {list}"),
    confirm_remove: |track, list| format!("remove “{track}” from {list}?   y yes   n no"),
    not_editable: "Not your playlist, it cannot be changed.",
    no_editable: "You have no playlist you can edit.",
    nothing_selected: "No track selected.",
    no_made_for_you: "No Made For You lists: add their links to made_for_you in config.toml.",
    nothing_to_go: "The selected row has no artist or album.",
    hints_artist: &[
        ("tab", "section"),
        ("← → ↑ ↓", "move"),
        ("↵", "play/open"),
        ("P", "play popular"),
        ("r", "artist radio"),
        ("a", "add to playlist"),
        ("⌫", "back"),
    ],
    popularity: "popularity",
    artist_play: "play popular",
    radio_short: "radio",
    artist_popular: "popular",
    artist_liked: |n| format!("♥ you liked · {n}"),
    artist_discography: "discography",
    artist_releases: |albums, singles| format!("{albums} albums · {singles} singles"),
    artist_related: "related",
    single: "single",
    queue_title: " queue ",
    queue_hint: "↵ jump here   esc close   (Spotify does not allow removing from the queue)",
    queue_empty: "Nothing is playing, the queue is empty.",
    queued: |title| format!("“{title}” added to the queue"),
    devices_title: " device ",
    devices_hint: "↵ move playback here   esc close",
    no_devices: "No Spotify device: open Spotify.app or Spotify on your phone.",
    device_restricted: "This device cannot be controlled through the Web API.",
    device_kind: |kind| kind.to_lowercase(),
    active: "playing",
    stage: "stage",
    stage_exit: "⌫ / esc leave",
    on_hold: "Spotify is not accepting this right now (rate limit); try again later.",
    no_graphics: "This terminal cannot show images (kitty graphics); for the cover and the record, try Ghostty, kitty or WezTerm.",
    setup_intro: "pikap uses a Spotify Developer app of your own (Spotify offers no shared key). The dashboard is opening in your browser:",
    setup_steps: [
        "Create an app; any name and description. Under APIs, tick Web API.",
        "Add this Redirect URI exactly: http://127.0.0.1:8989/login",
        "Settings → User Management: add the e-mail of your Spotify account.",
        "Copy the Client ID from Basic Information.",
    ],
    setup_prompt: "Client ID: ",
    setup_invalid: "That does not look like a Client ID (32 letters and digits). Try again, or press ↵ to stop.",
    setup_cancel: "Stopped. Run `pikap auth` again once you have the Client ID.",
    setup_saved: |path| format!("Saved to {path}"),
    library_held: |wait| match wait.as_secs() {
        s if s >= 3600 => format!("Spotify rate limit · ~{} h", s.div_ceil(3600)),
        s => format!("Spotify rate limit · ~{} min", s.div_ceil(60)),
    },
    already_open: |name| format!("{name} is already open"),
};

/// Turkish dative case ending for a (possibly foreign) name: `'a`, `'e`, `'ya`, `'ye`.
/// Vowel harmony follows the last written vowel; names without vowels (MGMT) are read
/// letter by letter and end in "e".
pub fn dative_suffix(name: &str) -> String {
    const BACK: &str = "aıouAIOU";
    const FRONT: &str = "eiöüEİÖÜ";
    let last_vowel = name
        .chars()
        .rev()
        .find(|c| BACK.contains(*c) || FRONT.contains(*c));
    let ends_with_vowel = name
        .chars()
        .last()
        .is_some_and(|c| BACK.contains(c) || FRONT.contains(c));
    let (vowel, buffer) = match last_vowel {
        Some(v) if BACK.contains(v) => ('a', ends_with_vowel),
        Some(_) => ('e', ends_with_vowel),
        None => ('e', true),
    };
    format!("'{}{}", if buffer { "y" } else { "" }, vowel)
}

/// Uppercase that keeps Turkish dotted and dotless i apart ("istanbul" → "İSTANBUL")
/// when the text itself looks Turkish. Track titles carry no language tag, so the
/// check is on the letters, not on the UI language.
pub fn display_upper(text: &str) -> String {
    let turkish = text.chars().any(|c| "ğşıİĞŞ".contains(c));
    if !turkish {
        return text.to_uppercase();
    }
    text.chars()
        .map(|c| match c {
            'i' => "İ".to_owned(),
            'ı' => "I".to_owned(),
            other => other.to_uppercase().collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dative() {
        assert_eq!(dative_suffix("Gorillaz"), "'a");
        assert_eq!(dative_suffix("Beck"), "'e");
        assert_eq!(dative_suffix("Tame Impala"), "'ya");
        assert_eq!(dative_suffix("Blur"), "'a");
        assert_eq!(dative_suffix("MGMT"), "'ye");
        assert_eq!(dative_suffix("Sezen Aksu"), "'ya");
        assert_eq!(dative_suffix("Barış Manço"), "'ya");
        assert_eq!(dative_suffix("Ezhel"), "'e");
    }

    #[test]
    fn upper() {
        assert_eq!(display_upper("Rhinestone Eyes"), "RHINESTONE EYES");
        assert_eq!(
            display_upper("Gülpembe'nin şarkısı"),
            "GÜLPEMBE'NİN ŞARKISI"
        );
    }

    #[test]
    fn lang_parse() {
        assert_eq!(Lang::parse("tr_TR.UTF-8"), Some(Lang::Tr));
        assert_eq!(Lang::parse("en-TR"), Some(Lang::En));
        assert_eq!(Lang::parse("C"), None);
    }
}
