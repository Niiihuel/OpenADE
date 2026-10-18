//! HTML written in a reply, shown rendered: each block is served once from
//! this machine at an unguessable address and opened in the integrated
//! browser. The page is sandboxed (an opaque origin, no forms, no requests
//! of its own), so it can draw and run its scripts but not reach openADE,
//! this machine's other servers or anything the person is signed in to.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Pages kept, oldest dropped first.
const KEEP: usize = 32;
const MAX_PAGE: usize = 2 * 1024 * 1024;
const POLICY: &str = "sandbox allow-scripts allow-popups; connect-src 'none'; form-action 'none'";

pub(crate) struct HtmlPreviews {
    port: u16,
    pages: Arc<Mutex<VecDeque<(String, String)>>>,
}

impl HtmlPreviews {
    pub(crate) fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let pages: Arc<Mutex<VecDeque<(String, String)>>> = Arc::default();
        let served = pages.clone();
        std::thread::Builder::new().name("html-preview".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let pages = served.clone();
                let _ = std::thread::Builder::new()
                    .name("html-preview-request".into())
                    .spawn(move || serve(stream, &pages));
            }
        })?;
        Ok(Self { port, pages })
    }

    /// The address that shows `html`, kept among the last pages.
    pub(crate) fn publish(&self, html: &str) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let mut pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        pages.push_back((token.clone(), html.chars().take(MAX_PAGE).collect()));
        while pages.len() > KEEP {
            pages.pop_front();
        }
        format!("http://127.0.0.1:{}/{token}.html", self.port)
    }
}

fn serve(stream: TcpStream, pages: &Mutex<VecDeque<(String, String)>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.by_ref().take(8 * 1024).read_line(&mut line).is_err() {
        return;
    }
    let token = line
        .strip_prefix("GET /")
        .and_then(|rest| rest.split(' ').next())
        .and_then(|path| path.strip_suffix(".html"))
        .unwrap_or("");
    let page = pages
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(known, _)| !token.is_empty() && known == token)
        .map(|(_, html)| html.clone());
    let mut stream = reader.into_inner();
    let response = match page {
        Some(html) => format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Security-Policy: {POLICY}\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
            html.len()
        ),
        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    };
    let _ = stream.write_all(response.as_bytes());
}

/// The ```html blocks of a reply, in order.
pub(crate) fn html_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in markdown.lines() {
        let fence = line.trim_start();
        match &mut current {
            None if fence.starts_with("```") && fence[3..].trim().eq_ignore_ascii_case("html") => {
                current = Some(Vec::new())
            }
            Some(lines) if fence.starts_with("```") && fence[3..].trim().is_empty() => {
                blocks.push(lines.join("\n"));
                current = None;
            }
            Some(lines) => lines.push(line),
            None => {}
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_names_its_html_blocks_and_only_complete_ones() {
        let reply = "Mirá:\n```html\n<h1>Hola</h1>\n```\ny\n```rust\nfn x() {}\n```\n```HTML\n<p>dos</p>\n```\n```html\n<p>a medias";
        assert_eq!(html_blocks(reply), ["<h1>Hola</h1>", "<p>dos</p>"]);
    }

    #[test]
    fn a_published_page_is_served_sandboxed_and_nothing_else_is() {
        let previews = HtmlPreviews::start().unwrap();
        let url = previews.publish("<h1>Hola</h1>");
        let get = |path: &str| {
            let address = url.trim_start_matches("http://").split('/').next().unwrap().to_owned();
            let mut stream = TcpStream::connect(address).unwrap();
            write!(stream, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        let path = format!("/{}", url.splitn(4, '/').nth(3).unwrap());
        let page = get(&path);
        assert!(page.starts_with("HTTP/1.1 200") && page.ends_with("<h1>Hola</h1>"), "{page}");
        assert!(page.contains("Content-Security-Policy: sandbox allow-scripts"));
        assert!(get("/otra.html").starts_with("HTTP/1.1 404"));
        assert!(get("/.html").starts_with("HTTP/1.1 404"));
    }
}
