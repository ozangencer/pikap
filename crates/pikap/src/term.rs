//! Questions to the terminal, asked once at startup while it is in raw mode: its
//! background colour (for the theme) and whether it shows kitty graphics.
//!
//! Each query is followed by a primary device attributes request (`CSI c`), which
//! every terminal answers, so a terminal that ignores the query is recognised at once
//! instead of after a timeout.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_millis(500);
/// A 1x1 RGB image, queried and never stored (`a=q`).
const KITTY_QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";
/// Terminals known to show kitty graphics, trusted when they stay silent.
const KITTY_TERMINALS: &[&str] = &["ghostty", "kitty", "WezTerm", "muxy"];

/// Sends `query` and returns what the terminal answers up to its device attributes.
pub fn ask(query: &[u8]) -> Vec<u8> {
    let mut out = std::io::stdout();
    if out
        .write_all(query)
        .and_then(|()| out.write_all(b"\x1b[c"))
        .is_err()
        || out.flush().is_err()
    {
        return Vec::new();
    }
    let stdin = std::io::stdin();
    let fd = stdin.as_raw_fd();
    let deadline = Instant::now() + WAIT;
    let mut reply = Vec::new();
    while Instant::now() < deadline {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is a valid pollfd for the duration of the call.
        if unsafe { libc::poll(&mut pfd, 1, 50) } <= 0 {
            continue;
        }
        let mut buf = [0u8; 256];
        let Ok(n) = stdin.lock().read(&mut buf) else {
            break;
        };
        reply.extend_from_slice(&buf[..n]);
        if answered(&reply) {
            break;
        }
    }
    reply
}

/// The device attributes reply (`CSI ? … c`) has arrived.
fn answered(reply: &[u8]) -> bool {
    let text = String::from_utf8_lossy(reply);
    text.rfind("\x1b[?")
        .is_some_and(|i| text[i..].contains('c'))
}

/// `Some(true)` when the terminal accepts kitty graphics, `Some(false)` when it
/// answered without them, `None` when it said nothing at all.
pub fn kitty_graphics_reply(reply: &[u8]) -> Option<bool> {
    let text = String::from_utf8_lossy(reply);
    if text.contains("\x1b_Gi=31;OK") {
        Some(true)
    } else if answered(reply) {
        Some(false)
    } else {
        None
    }
}

/// Whether to draw images. `PIKAP_GRAPHICS=on|off` decides first, then the
/// terminal's own answer, then its name. A terminal without kitty graphics prints
/// the image data as text, so the default when in doubt is no.
pub fn graphics_enabled() -> bool {
    match std::env::var("PIKAP_GRAPHICS").ok().as_deref() {
        Some("on" | "1" | "yes") => return true,
        Some("off" | "0" | "no") => return false,
        _ => {}
    }
    kitty_graphics_reply(&ask(KITTY_QUERY)).unwrap_or_else(|| {
        std::env::var("TERM_PROGRAM").is_ok_and(|t| KITTY_TERMINALS.contains(&t.as_str()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_replies() {
        assert_eq!(
            kitty_graphics_reply(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c"),
            Some(true)
        );
        assert_eq!(
            kitty_graphics_reply(b"\x1b[?1;2c"),
            Some(false),
            "Terminal.app"
        );
        assert_eq!(kitty_graphics_reply(b""), None);
    }
}
