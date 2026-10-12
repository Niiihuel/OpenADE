//! The live preview of a server the agent starts: finding its address in the
//! command's output, checking that something answers there, and the user's
//! choice of when it opens. Only addresses of this machine are ever found,
//! checked or opened here; the check is a TCP connection and sends nothing.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openide_ui::DropdownOption;
use serde::{Deserialize, Serialize};

pub(crate) const SETTING_ACTION_PREFIX: &str = "preview_opening:";
/// A server says where it listens when it starts, or not at all.
const WATCH_BYTES: usize = 8 * 1024;
const WATCH_TIME: Duration = Duration::from_secs(60);
/// A server that printed its address is ready, or about to be.
const CHECK_ATTEMPTS: u32 = 20;
const CHECK_PAUSE: Duration = Duration::from_millis(250);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);

/// The persisted choice of Settings › Navegador.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PreviewOpening {
    /// The first time the server answers.
    #[default]
    Auto,
    /// The command's card offers it.
    Ask,
    Never,
}

impl PreviewOpening {
    const ALL: [Self; 3] = [Self::Auto, Self::Ask, Self::Never];

    fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ask => "ask",
            Self::Never => "never",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automáticamente",
            Self::Ask => "Preguntar",
            Self::Never => "Nunca",
        }
    }

    pub(crate) fn from_action_id(id: &str) -> Option<Self> {
        let id = id.strip_prefix(SETTING_ACTION_PREFIX)?;
        Self::ALL.into_iter().find(|choice| choice.id() == id)
    }

    pub(crate) fn options(self) -> Vec<DropdownOption> {
        Self::ALL
            .into_iter()
            .map(|choice| {
                DropdownOption::new(
                    format!("{SETTING_ACTION_PREFIX}{}", choice.id()),
                    choice.label(),
                )
                .selected(choice == self)
            })
            .collect()
    }
}

/// `url` as the address of a server on this machine, or nothing: plain
/// HTTP(S) without credentials whose host is `localhost`, the IPv4 loopback
/// block, `[::1]` or `0.0.0.0`. The last is where a server listens, not where
/// a page is: it becomes `localhost`.
pub(crate) fn loopback_url(url: &str) -> Option<String> {
    let mut parsed = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    // The parser already wrote the host in its one canonical spelling.
    let host = parsed.host_str()?;
    if host == "0.0.0.0" {
        parsed.set_host(Some("localhost")).ok()?;
    } else if host != "localhost"
        && host != "[::1]"
        && !host.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
    {
        return None;
    }
    Some(parsed.into())
}

/// The first loopback address a command printed, from its output without
/// escape sequences. An address the output ends on may still be arriving
/// (`http://localhost` before `:5173`), so only one followed by something
/// else counts.
pub(crate) fn first_loopback_url(output: &str) -> Option<String> {
    let mut rest = output;
    while let Some(start) = rest.find("http") {
        let candidate = &rest[start..];
        let end = candidate
            .find(|character: char| {
                character.is_whitespace()
                    || character.is_control()
                    || matches!(character, '"' | '\'' | '<' | '>' | '`' | '|' | '\\' | '^')
            })
            .unwrap_or(candidate.len());
        let web = candidate.starts_with("http://") || candidate.starts_with("https://");
        if web && end < candidate.len() {
            // Prose ends a sentence after an address; a URL does not end so.
            let address = candidate[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
            if let Some(url) = loopback_url(address) {
                return Some(url);
            }
        }
        rest = &candidate["http".len()..];
    }
    None
}

/// Where to knock for `url`, a loopback address: never a name to resolve.
fn loopback_sockets(url: &str) -> Vec<SocketAddr> {
    let Some(parsed) = loopback_url(url).and_then(|url| reqwest::Url::parse(&url).ok()) else {
        return Vec::new();
    };
    let Some(port) = parsed.port_or_known_default() else {
        return Vec::new();
    };
    let host = parsed.host_str().unwrap_or_default();
    let hosts: Vec<IpAddr> = match host.parse::<Ipv4Addr>() {
        Ok(ip) => vec![ip.into()],
        Err(_) if host == "[::1]" => vec![Ipv6Addr::LOCALHOST.into()],
        // `localhost` is either family, whichever the server chose.
        Err(_) => vec![Ipv4Addr::LOCALHOST.into(), Ipv6Addr::LOCALHOST.into()],
    };
    hosts.into_iter().map(|host| SocketAddr::new(host, port)).collect()
}

/// Whether something accepts connections at a loopback `url`, asking up to
/// `attempts` times. Connects and hangs up: no request is made.
pub(crate) fn answers(url: &str, attempts: u32, pause: Duration) -> bool {
    let sockets = loopback_sockets(url);
    for attempt in 0..attempts {
        if sockets
            .iter()
            .any(|socket| TcpStream::connect_timeout(socket, CONNECT_TIMEOUT).is_ok())
        {
            return true;
        }
        if attempt + 1 < attempts {
            std::thread::sleep(pause);
        }
    }
    false
}

/// Reads the start of a command's output for the address of its server and
/// publishes it once the address answers.
pub(crate) struct OutputWatch {
    head: Vec<u8>,
    since: Instant,
    found: Arc<Mutex<Option<String>>>,
    woke: Arc<dyn Fn() + Send + Sync>,
}

impl OutputWatch {
    /// `woke` runs after `found` is set, from a thread of its own.
    pub(crate) fn new(found: Arc<Mutex<Option<String>>>, woke: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            head: Vec::new(),
            since: Instant::now(),
            found,
            woke,
        }
    }

    /// Looks at more output. False once there is nothing left to look for:
    /// an address was found, or the start of the output went by without one.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> bool {
        if self.since.elapsed() > WATCH_TIME {
            return false;
        }
        let room = WATCH_BYTES - self.head.len();
        self.head.extend_from_slice(&bytes[..bytes.len().min(room)]);
        // A server colours its address, sometimes in the middle of it.
        let text = openide_harness::strip_ansi(&String::from_utf8_lossy(&self.head));
        let Some(url) = first_loopback_url(&text) else {
            return self.head.len() < WATCH_BYTES;
        };
        let (found, woke) = (self.found.clone(), self.woke.clone());
        let _ = std::thread::Builder::new()
            .name("openide-preview-check".into())
            .spawn(move || {
                if answers(&url, CHECK_ATTEMPTS, CHECK_PAUSE) {
                    *found.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(url);
                    woke();
                }
            });
        false
    }
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;
