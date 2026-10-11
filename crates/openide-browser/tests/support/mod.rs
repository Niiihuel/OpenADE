//! What the real-Chrome tests share: a local HTTP fixture server and a
//! driver started on the Chrome named by `OPENIDE_BROWSER_EXECUTABLE`.
#![allow(dead_code)]

use openide_browser::{Driver, LaunchOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct Response {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn html(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type", "text/html; charset=utf-8".into())],
            body: body.into(),
        }
    }

    pub fn status(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: vec![("Content-Type", "text/plain; charset=utf-8".into())],
            body: body.as_bytes().to_vec(),
        }
    }

    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.retain(|(existing, _)| *existing != name);
        self.headers.push((name, value.to_owned()));
        self
    }
}

/// One of the pages under `tests/fixtures`.
pub fn fixture(name: &str) -> Option<Response> {
    let name = name.trim_start_matches('/');
    if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
        return None;
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    std::fs::read(path).ok().map(Response::html)
}

/// An HTTP server on a free loopback port, alive until dropped. Only this
/// machine can reach it and it serves only what the test hands it.
pub struct Server {
    pub origin: String,
    port: u16,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// `route` gets the method and the path with its query.
    pub fn start(route: impl Fn(&str, &str) -> Response + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let route = Arc::new(route);
        let stopping = stop.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let route = route.clone();
                std::thread::spawn(move || {
                    let _ = serve(stream, route.as_ref());
                });
            }
        });
        Self {
            origin: format!("http://127.0.0.1:{port}"),
            port,
            stop,
        }
    }

    /// The fixture pages by file name, plus an `/api/` that answers JSON.
    pub fn fixtures() -> Self {
        Self::start(|_, path| {
            let path = path.split('?').next().unwrap_or("");
            if path.starts_with("/api/") {
                return Response::status(200, "{}").header("Content-Type", "application/json");
            }
            fixture(path).unwrap_or_else(|| Response::status(404, "no"))
        })
    }

    /// The same server under another host name: a different site for Chrome.
    pub fn origin_as(&self, host: &str) -> String {
        format!("http://{host}:{}", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn serve(mut stream: TcpStream, route: &dyn Fn(&str, &str) -> Response) -> std::io::Result<()> {
    let mut request = Vec::new();
    let mut scratch = [0u8; 4096];
    let head = loop {
        let count = stream.read(&mut scratch)?;
        request.extend_from_slice(&scratch[..count]);
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        if count == 0 || request.len() > 256 * 1024 {
            return Ok(());
        }
    };
    let text = String::from_utf8_lossy(&request[..head]).into_owned();
    let mut line = text.lines().next().unwrap_or("").split(' ');
    let (method, path) = (line.next().unwrap_or(""), line.next().unwrap_or("/"));
    let length = text
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    // The body is read and never looked at.
    let mut body = request.len() - head - 4;
    while body < length.min(1024 * 1024) {
        let count = stream.read(&mut scratch)?;
        if count == 0 {
            break;
        }
        body += count;
    }
    let response = route(method, path);
    let mut reply = format!("HTTP/1.1 {} X\r\nConnection: close\r\nContent-Length: {}\r\n", response.status, response.body.len());
    for (name, value) in &response.headers {
        reply.push_str(&format!("{name}: {value}\r\n"));
    }
    reply.push_str("\r\n");
    stream.write_all(reply.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

pub fn executable() -> PathBuf {
    std::env::var_os("OPENIDE_BROWSER_EXECUTABLE")
        .map(PathBuf::from)
        .expect("set OPENIDE_BROWSER_EXECUTABLE to a Chrome or Chromium binary")
}

pub fn launch(width: u32, height: u32, scale: f64) -> Driver {
    Driver::launch(&LaunchOptions {
        executable: executable(),
        width,
        height,
        scale,
        temp_root: None,
    })
    .unwrap()
}
