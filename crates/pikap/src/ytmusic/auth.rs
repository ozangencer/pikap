//! `pikap auth ytmusic`: YouTube Music works signed out; signing in adds the user's
//! library. The sign-in is the browser's cookie, read by yt-dlp when macOS lets the
//! terminal into the browser's data (Full Disk Access), or pasted from DevTools.
//! Either way it is checked against YouTube Music and saved where only the user can
//! read it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::client::{self, Client};
use crate::i18n::Strings;
use crate::spotify::config;

/// Browsers yt-dlp can read cookies from: (app bundle, yt-dlp name, shown name).
const BROWSERS: &[(&str, &str, &str)] = &[
    ("Google Chrome.app", "chrome", "Chrome"),
    ("Brave Browser.app", "brave", "Brave"),
    ("Safari.app", "safari", "Safari"),
    ("Firefox.app", "firefox", "Firefox"),
    ("Microsoft Edge.app", "edge", "Edge"),
    ("Vivaldi.app", "vivaldi", "Vivaldi"),
    ("Opera.app", "opera", "Opera"),
    ("Chromium.app", "chromium", "Chromium"),
];

const FULL_DISK_ACCESS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";
const YTM: &str = "https://music.youtube.com";
const PASTE_TRIES: usize = 3;

pub fn sign_in(s: &Strings, force: bool) -> Result<()> {
    println!("{}\n", s.ytm_intro);
    let missing: Vec<&str> = ["mpv", "yt-dlp"]
        .into_iter()
        .filter(|tool| !installed(tool))
        .collect();
    if !missing.is_empty() {
        println!("{}\n", (s.ytm_missing)(&missing.join(" ")));
    }
    let runtime = tokio::runtime::Runtime::new()?;

    if !force && let Some(cookie) = client::read_cookie() {
        println!("{}", s.ytm_checking);
        if runtime.block_on(signs_in(&cookie))? {
            println!("{}", s.ytm_already);
            return Ok(());
        }
        println!("{}\n", s.ytm_expired);
    }
    if !yes(s.ytm_ask)? {
        println!("{}", s.ytm_signed_out);
        return Ok(());
    }

    let browsers: Vec<&(&str, &str, &str)> = BROWSERS
        .iter()
        .filter(|(app, ..)| installed_app(app))
        .collect();
    println!("\n{}", s.ytm_from);
    for (i, (_, _, name)) in browsers.iter().enumerate() {
        println!("  {}) {name}", i + 1);
    }
    println!("  p) {}", s.ytm_paste_option);
    let choice = ask(s.ytm_choice)?;
    let picked = match choice.trim() {
        "" if !browsers.is_empty() => Some(browsers[0]),
        "p" | "P" => None,
        n => n
            .parse::<usize>()
            .ok()
            .and_then(|n| browsers.get(n.wrapping_sub(1)).copied()),
    };

    let mut cookie = None;
    if let Some((_, key, name)) = picked {
        println!("\n{}", (s.ytm_reading)(name));
        match from_browser(key) {
            Ok(found) => cookie = Some(found),
            Err(err) => {
                println!("{}\n", (s.ytm_unreadable)(name, &format!("{err:#}")));
                println!("{}", (s.ytm_full_disk)(&terminal_name()));
                let _ = Command::new("open").arg(FULL_DISK_ACCESS).status();
                if !yes(s.ytm_paste_instead)? {
                    println!("{}", s.ytm_signed_out);
                    return Ok(());
                }
            }
        }
    }
    let cookie = match cookie {
        Some(c) => c,
        None => match pasted(s)? {
            Some(c) => c,
            None => {
                println!("{}", s.ytm_signed_out);
                return Ok(());
            }
        },
    };

    println!("{}", s.ytm_checking);
    if !runtime.block_on(signs_in(&cookie))? {
        bail!("{}", s.ytm_not_signed_in);
    }
    let path = save(&cookie)?;
    println!("{}", (s.ytm_done)(&path.display().to_string()));
    Ok(())
}

async fn signs_in(cookie: &str) -> Result<bool> {
    Client::connect(Some(cookie)).await?.has_account().await
}

/// The cookie from DevTools, through the clipboard.
fn pasted(s: &Strings) -> Result<Option<String>> {
    println!();
    for (i, step) in s.ytm_paste_steps.iter().enumerate() {
        println!("  {}. {step}", i + 1);
    }
    let _ = Command::new("open").arg(YTM).status();
    for _ in 0..PASTE_TRIES {
        let answer = ask(s.ytm_paste_prompt)?;
        if answer.trim().eq_ignore_ascii_case("q") {
            return Ok(None);
        }
        let clipboard = Command::new("pbpaste").output().context("pbpaste")?;
        match header(&String::from_utf8_lossy(&clipboard.stdout)) {
            Some(cookie) => return Ok(Some(cookie)),
            None => println!("{}", s.ytm_paste_invalid),
        }
    }
    Ok(None)
}

/// A `Cookie:` header as DevTools copies it, with or without the name in front.
pub fn header(text: &str) -> Option<String> {
    let text = text.trim();
    let text = text
        .strip_prefix("cookie:")
        .or_else(|| text.strip_prefix("Cookie:"))
        .unwrap_or(text)
        .trim();
    let has = |name: &str| text.split(';').any(|p| p.trim().starts_with(name));
    (has("SAPISID=") && !text.contains('\n')).then(|| text.to_owned())
}

/// Lets yt-dlp read the browser's cookie store and keeps the YouTube ones.
fn from_browser(browser: &str) -> Result<String> {
    let dir = config::cache_dir().join("ytmusic");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("browser-cookies.txt");
    let _ = std::fs::remove_file(&file);
    let out = Command::new("yt-dlp")
        .args(["--cookies-from-browser", browser, "--cookies"])
        .arg(&file)
        .args(["--skip-download", "--quiet", "--no-warnings", "--simulate"])
        .arg(YTM)
        .output()
        .context("yt-dlp is not installed (brew install yt-dlp)")?;
    let jar = std::fs::read_to_string(&file);
    let _ = std::fs::remove_file(&file);
    let Ok(jar) = jar else {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{}", err.lines().last().unwrap_or("yt-dlp read no cookies"));
    };
    header(&netscape_to_header(&jar)).context("no YouTube sign-in among the browser's cookies")
}

/// The YouTube cookies of a Netscape cookie file, as one header.
pub fn netscape_to_header(jar: &str) -> String {
    jar.lines()
        .map(|l| l.strip_prefix("#HttpOnly_").unwrap_or(l))
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() == 7 && f[0].ends_with("youtube.com")).then(|| format!("{}={}", f[5], f[6]))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn save(cookie: &str) -> Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = client::cookie_path();
    std::fs::create_dir_all(path.parent().context("config dir")?)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    writeln!(file, "{cookie}")?;
    Ok(path)
}

fn installed(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn installed_app(bundle: &str) -> bool {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    Path::new("/Applications").join(bundle).exists()
        || home.is_some_and(|h| h.join("Applications").join(bundle).exists())
}

/// The terminal app to give Full Disk Access to, as macOS lists it.
fn terminal_name() -> String {
    match std::env::var("TERM_PROGRAM").unwrap_or_default().as_str() {
        "Apple_Terminal" => "Terminal".into(),
        "iTerm.app" => "iTerm".into(),
        "ghostty" => "Ghostty".into(),
        "WezTerm" => "WezTerm".into(),
        "vscode" => "Visual Studio Code".into(),
        "" => "Terminal".into(),
        other => {
            let mut name = other.to_owned();
            if let Some(first) = name.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            name
        }
    }
}

fn ask(prompt: &str) -> Result<String> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line)
}

/// Yes unless the answer starts with n or h (hayır); an empty answer is yes.
fn yes(prompt: &str) -> Result<bool> {
    let answer = ask(prompt)?.trim().to_lowercase();
    Ok(!(answer.starts_with('n') || answer.starts_with('h')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_headers() {
        let h = "cookie: SID=a; SAPISID=b/c; __Secure-3PSID=d";
        assert_eq!(
            header(h).as_deref(),
            Some("SID=a; SAPISID=b/c; __Secure-3PSID=d")
        );
        assert_eq!(
            header("  SAPISID=x; HSID=y \n").as_deref(),
            Some("SAPISID=x; HSID=y")
        );
        assert_eq!(
            header("__Secure-3PAPISID=x"),
            None,
            "only the plain SAPISID counts"
        );
        assert_eq!(header("https://music.youtube.com"), None);
        assert_eq!(header("SAPISID=x\nsomething else"), None);
    }

    #[test]
    fn netscape_file_keeps_youtube_cookies() {
        let jar = "# Netscape HTTP Cookie File\n\
                   .youtube.com\tTRUE\t/\tTRUE\t0\tSAPISID\tabc\n\
                   #HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t0\tHSID\tdef\n\
                   .google.com\tTRUE\t/\tTRUE\t0\tNID\txyz\n";
        assert_eq!(netscape_to_header(jar), "SAPISID=abc; HSID=def");
    }
}
