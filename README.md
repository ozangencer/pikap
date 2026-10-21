# pikap

A Spotify remote for the terminal. pikap does not play audio itself: it drives
Spotify.app (or any Spotify Connect device) and gives the terminal the album art,
synced lyrics, an artist page with the whole discography, and a record that turns
while the music plays.

<p align="center"><img src="docs/demo.gif" alt="pikap in Muxy: home, lyrics, artist page, library, search, stage mode, with a second pikap in a side pane" width="900"></p>
<p align="center"><a href="docs/demo.mp4">Watch the demo in full resolution (70 s)</a></p>

*pikap* is Turkish for record player.

## Features

- **Now playing** with the real album cover, a blurred halo of it behind the whole
  screen, a glitch title and an equalizer that doubles as the progress bar, all in
  the cover's colours.
- **Synced lyrics** that follow the song, and a **stage mode** (`z`): the cover as a
  sleeve, the record slid out and turning, the current line karaoke style.
- **Artist pages**: popular tracks, the songs you liked, every album and single as a
  grid of covers, related artists.
- **Library**: Made For You, liked songs, recently played, your playlists and saved
  albums. **Search** across tracks, albums, artists and playlists at once.
- **Playlists**: add the selected track to one of your playlists (`a`), remove it
  (`x`), add it to the queue (`e`).
- **Queue** (`g k`) and **devices** (`g d`): jump ahead in the queue, move playback
  to your phone or speaker.
- **Radio**: Spotify's own radio for the playing track or artist.
- A `g` key panel for getting around, a narrow layout for side panes, light and dark
  themes, English and Turkish.

| | |
|---|---|
| ![Now playing](docs/home.png) | ![Artist page](docs/artist.png) |
| ![Library](docs/library.png) | ![Stage mode](docs/stage.png) |

## Requirements

- **macOS**. pikap talks to Spotify.app and the system (volume, focus) through macOS
  tools.
- A **terminal with kitty graphics**. pikap is developed in
  Muxy; Ghostty and kitty speak the same protocol. Without it you
  get the text interface only.
- **Spotify Premium** (Spotify only allows remote control with Premium).
- Rust 1.88 or newer to build it.

## Install

```sh
cargo install --locked --git https://github.com/ozangencer/pikap pikap
```

## Set up Spotify

pikap uses your own Spotify Developer app, so its requests count against your own
quota and nobody else's.

1. Open the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard)
   and create an app. Any name works. Under *Which API/SDKs are you planning to use*,
   tick **Web API**.
2. Add this **Redirect URI** exactly: `http://127.0.0.1:8989/login`
3. In *User Management*, add the e-mail of your Spotify account (apps in
   Development Mode only work for listed users).
4. Copy the app's **Client ID** into `~/.config/pikap/config.toml`:

   ```toml
   client_id = "your-client-id"
   ```

   [`config.example.toml`](config.example.toml) shows the other settings, including
   the links that make your Made For You playlists appear in the library.
5. Sign in:

   ```sh
   pikap auth
   ```

   The browser opens twice: once for the Web API (your app) and once for Spotify's
   metadata service (artists, albums, radio, lyrics), which pikap reaches through
   [librespot](https://github.com/librespot-org/librespot) the way other librespot
   players do. Tokens are kept in `~/.cache/pikap/`, readable only by you.

Then start Spotify.app (or have any Connect device online) and run `pikap`. With
nothing playing, `↵` resumes where you left off.

## Keys

| Key | |
|---|---|
| `space` | play / pause |
| `n` `p` | next / previous |
| `ö` `ç` or `⇧←` `⇧→` | seek 5 s back / forward |
| `+` `-` | volume |
| `f` | like |
| `s` `R` | shuffle / repeat |
| `1`–`5`, `←` `→` | home tabs: list, lyrics, artist, album, discover |
| `↑` `↓` / `j` `k` | move |
| `↵` | play or open |
| `P` | play the whole list |
| `a` `x` `e` | add to playlist / remove from it / add to queue |
| `r` | radio |
| `/` | search |
| `g` | key panel: `l` library, `s` search, `y` liked, `r` recent, `k` queue, `d` devices, `a` artist, `b` album, `L` lyrics, `z` stage |
| `z` | stage mode (`⌫` leaves) |
| `⌫` | back |
| `T` | light / dark theme |
| `ctrl-l` | redraw everything |
| `q` | quit |

`pikap --demo` shows a sample screen without Spotify; `--lang en|tr` and
`--theme dark|light` override the detected ones.

## Notes

- Development Mode apps get small daily quotas per endpoint. pikap keeps its
  requests low (a normal start makes about three), caches the library, liked songs
  and track details under `~/.cache/pikap/`, and shows on the status line when
  Spotify puts an endpoint on hold.
- Lists always play as a Spotify context (album, playlist, liked songs, Spotify's
  radio playlist). Starting playback from a plain list of tracks leaves current
  Spotify.app versions stuck with no track, so pikap never does.
- When Spotify plays on this Mac, the volume shown and changed is the system volume.
- A log of every request is kept in `~/.cache/pikap/pikap.log`.

## License

MIT. The bundled JetBrains Mono font is under the SIL Open Font License 1.1
(`crates/pikap/assets/OFL.txt`).
