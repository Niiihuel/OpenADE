//! The runtime against a real Chrome: the worker, the driver, the decoder,
//! the broker and the proxy executable, short of the window and its GPU.
//! Opt-in, because the central suite must not need a browser:
//!
//! ```sh
//! OPENIDE_BROWSER_EXECUTABLE="$(command -v google-chrome-stable)" \
//!   nix-shell --run "cargo test -p agents-window -- --ignored real_chrome"
//! ```
//!
//! Chrome runs headless with a throwaway profile under a temporary data
//! root and only visits the pages this file serves on a loopback port.

use super::*;
use std::io::Read;
use std::process::{Command, Stdio};

const PAGE: &str = r#"<!doctype html><html lang="es"><head><meta charset="utf-8"><title>Fixture OpenIDE</title><style>
html{background:#fff}body{font:16px/1.5 sans-serif;margin:0;min-height:2400px;color:#111}
#hover{position:absolute;left:20px;top:20px;width:200px;height:40px;background:#dde}#hover:hover{background:#365ff4}
#panel{position:absolute;left:20px;top:80px}input,button{font:inherit}
#text{position:absolute;left:20px;top:160px;margin:0;font:20px/30px monospace;white-space:nowrap}
#spin{position:fixed;right:20px;top:20px;width:60px;height:60px;background:linear-gradient(45deg,#f06,#48f);animation:turn 1s linear infinite}
@keyframes turn{to{transform:rotate(360deg)}}
#rows{position:absolute;top:220px;left:0;right:0}.row{border:1px solid #ccc;border-radius:8px;padding:12px;margin:8px 24px}
footer{position:absolute;top:2340px;left:20px}
</style></head><body>
<div id="hover" onmouseenter="document.title='Encima'" onmouseleave="document.title='Fuera'"></div>
<button id="paint" style="position:absolute;left:300px;top:20px" onclick="document.body.style.background='#fde'">Pintar</button>
<div id="panel"><label>Nombre <input id="name" aria-label="Nombre"></label>
<button id="apply" onclick="document.querySelector('#value').textContent=document.querySelector('#name').value">Aplicar</button>
<span id="value">Sin cambios</span></div>
<p id="text">Texto para arrastrar</p>
<div id="rows">ROWS</div>
<footer>Final de página</footer>
SPIN
<script>
addEventListener('contextmenu', event => { event.preventDefault(); document.title = 'Menú ' + event.button; });
addEventListener('auxclick', event => { if (event.button === 1) document.title = 'Medio'; });
addEventListener('dblclick', () => { document.title = 'Doble: ' + getSelection().toString().trim(); });
addEventListener('scroll', () => { document.title = 'Scroll ' + Math.round(scrollY); });
</script></body></html>"#;

/// The fixture pages on a free loopback port: `/` holds still, `/animated`
/// never does. Only this machine can reach them.
fn site() -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        let rows: String = (0..40)
            .map(|row| format!(r#"<div class="row"><b>Fila {row}</b> — Lorem ipsum dolor sit amet, consectetur adipiscing elit.</div>"#))
            .collect();
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 2048];
            let read = stream.read(&mut request).unwrap_or(0);
            let animated = request[..read].starts_with(b"GET /animated");
            let body = PAGE
                .replace("ROWS", &rows)
                .replace("SPIN", if animated { r#"<div id="spin"></div>"# } else { "" });
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    origin
}

struct Session {
    runtime: BrowserRuntime,
    data: tempfile::TempDir,
    _workspace: tempfile::TempDir,
    /// Counts the frames the decoder published for the window.
    frames: Arc<AtomicUsize>,
}

fn launch(viewport: (u32, u32), scale: f32) -> Session {
    launch_as(viewport, scale, true)
}

/// `visible` is what the surface is from before the browser starts, as the
/// window sets it for a tab that is not shown.
fn launch_as(viewport: (u32, u32), scale: f32, visible: bool) -> Session {
    let executable = std::env::var_os("OPENIDE_BROWSER_EXECUTABLE")
        .expect("set OPENIDE_BROWSER_EXECUTABLE to a Chrome or Chromium");
    let (data, workspace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let runtime = BrowserRuntime::start(
        BrowserScope {
            workspace: workspace.path().into(),
            session_id: "real-chrome".into(),
        },
        data.path(),
        BrowserLaunchOptions {
            browser_executable: Some(executable.into()),
            viewport_width: viewport.0,
            viewport_height: viewport.1,
            device_scale: scale,
        },
    )
    .unwrap();
    runtime.set_visible(visible);
    let frames = Arc::new(AtomicUsize::new(0));
    let counter = frames.clone();
    runtime.on_frame(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    });
    let session = Session {
        runtime,
        data,
        _workspace: workspace,
        frames,
    };
    session.until("the context to start", || session.runtime.snapshot().started);
    session
}

impl Session {
    fn until(&self, what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(40);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {:?}",
                self.runtime.snapshot().error
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// One command as the window sends it.
    fn window(&self, command: BrowserCommand) -> Result<BrowserSnapshot, String> {
        let mut request = self.runtime.submit_ui(command).unwrap();
        let mut reply = None;
        self.until("the window's reply", || {
            reply = request.poll();
            reply.is_some()
        });
        reply.unwrap()
    }

    /// One tool call as the application's own agent makes it.
    fn agent(&self, name: &str, arguments: Value) -> McpCallResult {
        let client = self.runtime.mcp_client();
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(client.call_tool(name, arguments, &CancellationToken::new()))
            .unwrap_or_else(|error| panic!("{name}: {error:?}"))
    }

    fn open(&self, url: String) -> BrowserSnapshot {
        self.window(BrowserCommand::Navigate { url }).unwrap()
    }

    fn title(&self, expected: &str) {
        self.until(&format!("the title {expected:?}"), || {
            // The page's state is read on a poll while it is visible, and
            // with every command otherwise.
            if !self.runtime.visible() {
                let _ = self.agent("browser_network_requests", json!({}));
            }
            self.runtime.snapshot().title.starts_with(expected)
        });
    }

    fn process(&self) -> u32 {
        self.runtime
            .handle
            .process
            .lock()
            .unwrap()
            .expect("a browser process")
    }

    /// Waits until no frame has been published for `quiet`, and returns how
    /// many were since the call.
    fn settle(&self, quiet: Duration) -> usize {
        let before = self.frames.load(Ordering::Relaxed);
        let (mut seen, mut since) = (before, Instant::now());
        self.until("the page to hold still", || {
            let now = self.frames.load(Ordering::Relaxed);
            if now != seen {
                (seen, since) = (now, Instant::now());
            }
            since.elapsed() >= quiet
        });
        seen - before
    }

    fn next_frame(&self, what: &str) -> limn_egui::SurfaceFrame {
        let mut frame = None;
        self.until(what, || {
            frame = self.runtime.take_frame();
            frame.is_some()
        });
        frame.unwrap()
    }
}

fn page(result: &McpCallResult) -> Value {
    serde_json::from_str(result.content[0]["text"].as_str().unwrap()).unwrap()
}

/// The ref the snapshot printed on the first line that contains `label`.
fn ref_of(snapshot: &str, label: &str) -> String {
    let line = snapshot
        .lines()
        .find(|line| line.contains(label))
        .unwrap_or_else(|| panic!("{label} is not in:\n{snapshot}"));
    let start = line.find("[ref=").unwrap_or_else(|| panic!("{line} has no ref")) + 5;
    line[start..].split(']').next().unwrap().to_owned()
}

/// Chrome's private directory, from the profile on its command line.
fn browser_directory(process: u32) -> PathBuf {
    let command = fs::read(format!("/proc/{process}/cmdline")).unwrap();
    let profile = command
        .split(|byte| *byte == 0)
        .find_map(|argument| std::str::from_utf8(argument).ok()?.strip_prefix("--user-data-dir="))
        .expect("the browser was started with a profile of its own")
        .to_owned();
    PathBuf::from(profile).parent().unwrap().to_owned()
}

/// Every process of the browser's group that is still running.
fn group(process: u32) -> Vec<u32> {
    fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|candidate| {
            fs::read_to_string(format!("/proc/{candidate}/stat")).is_ok_and(|stat| {
                // The fields after the command, which may itself hold spaces.
                let fields: Vec<&str> = stat.rsplit(')').next().unwrap_or("").split_whitespace().collect();
                fields.get(2).and_then(|group| group.parse::<u32>().ok()) == Some(process)
                    && fields.first() != Some(&"Z")
            })
        })
        .collect()
}

#[test]
#[ignore = "opt-in: launches a real Chrome against local fixture pages"]
fn real_chrome_serves_the_agent_and_an_external_client_and_leaves_nothing_behind() {
    let origin = site();
    let session = launch((900, 700), 2.0);
    let process = session.process();
    let directory = browser_directory(process);
    assert!(directory.join("profile").is_dir());
    // Chrome's files live under the application's own data root when its
    // path is short enough for Chromium's sockets, and under /tmp otherwise:
    // never in the temporary directory of the shell that started the tests.
    let base = fs::canonicalize(session.data.path().join("browser-runtime")).unwrap();
    assert!(
        [base.as_path(), Path::new("/tmp")].contains(&directory.parent().unwrap()),
        "{directory:?}"
    );
    assert!(directory.to_str().unwrap().len() <= 48);
    assert!(group(process).len() > 1, "the browser and its helpers run in one group");

    // The window navigates; the agent reads the page and acts on it by ref.
    let opened = session.open(format!("{origin}/"));
    assert_eq!((opened.url.as_str(), opened.title.as_str()), (format!("{origin}/").as_str(), "Fixture OpenIDE"));
    let read = session.agent("browser_snapshot", json!({}));
    assert!(!read.is_error && read.content.len() == 1);
    let state = page(&read);
    assert_eq!(state["viewport"], json!({"width": 900, "height": 700}));
    let tree = state["aria_snapshot"].as_str().unwrap().to_owned();
    let (field, button) = (ref_of(&tree, "textbox \"Nombre\""), ref_of(&tree, "button \"Aplicar\""));
    assert!(!session.agent("browser_fill", json!({"ref": field, "text": "Niño 🧠"})).is_error);
    let clicked = session.agent("browser_click", json!({"ref": button}));
    assert!(page(&clicked)["aria_snapshot"].as_str().unwrap().contains("Niño 🧠"));
    // What an approval card would say that ref is.
    let last = session.runtime.mcp_client().last_snapshot();
    assert_eq!(openide_browser::element_of(&last, &button), Some(("button".into(), "Aplicar".into())));
    assert_eq!(openide_browser::element_of(&last, &field).map(|(role, _)| role), Some("textbox".into()));

    // A large page is read in less: only what can be acted on, or one part.
    let whole = page(&session.agent("browser_snapshot", json!({})))["aria_snapshot"].as_str().unwrap().to_owned();
    let narrowed = page(&session.agent("browser_snapshot", json!({"interactive_only": true})))["aria_snapshot"].as_str().unwrap().to_owned();
    assert!(whole.contains("Fila 39") && whole.contains("Final de página"));
    assert!(narrowed.contains("button \"Aplicar\"") && !narrowed.contains("Fila 39"), "{narrowed}");
    assert!(narrowed.len() * 4 < whole.len(), "{} of {} bytes", narrowed.len(), whole.len());
    assert_eq!(ref_of(&narrowed, "button \"Aplicar\""), button, "a narrowed snapshot keeps the refs");
    let part = page(&session.agent("browser_snapshot", json!({"selector": "#panel"})))["aria_snapshot"].as_str().unwrap().to_owned();
    assert!(part.contains("button \"Aplicar\"") && !part.contains("Pintar") && !part.contains("Fila"), "{part}");
    let shallow = page(&session.agent("browser_snapshot", json!({"selector": "#rows", "max_depth": 0})))["aria_snapshot"].as_str().unwrap().to_owned();
    assert!(!shallow.contains("Fila 0"), "{shallow}");
    assert!(session.agent("browser_snapshot", json!({"selector": "#absent"})).is_error);

    // The tool's own failures are answers: nothing uncertain, no page error.
    let stale = session.agent("browser_click", json!({"ref": "e99999"}));
    assert!(stale.is_error && stale.content[0]["text"].as_str().unwrap().contains("no corresponde"));
    assert!(session.runtime.snapshot().error.is_none());
    let evaluated = session.agent("browser_evaluate", json!({"expression": "document.querySelector('#value').textContent"}));
    assert_eq!(evaluated.content[1]["text"], "\"Niño 🧠\"");
    let captured = session.agent("browser_take_screenshot", json!({}));
    assert_eq!(page(&captured)["screenshot"]["width"], 900, "CSS pixels whatever the scale");
    assert_eq!(captured.content[1]["type"], "image");

    // A page that does not load says so, and the context goes on.
    let closed = TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap();
    let refused = session.window(BrowserCommand::Navigate { url: format!("http://{closed}/") }).unwrap_err();
    assert_eq!(refused, format!("net::ERR_CONNECTION_REFUSED at http://{closed}/"));
    assert_eq!(session.runtime.snapshot().error_code.as_deref(), Some("navigation_failed"));
    assert!(session.open(format!("{origin}/")).error.is_none());
    // History, from the toolbar.
    let second = session.open(format!("{origin}/animated"));
    assert!(second.can_go_back && !second.can_go_forward);
    let back = session.window(BrowserCommand::Back).unwrap();
    assert_eq!((back.url, back.can_go_forward), (format!("{origin}/"), true));
    assert_eq!(session.window(BrowserCommand::Forward).unwrap().url, format!("{origin}/animated"));
    assert_eq!(session.window(BrowserCommand::Reload).unwrap().title, "Fixture OpenIDE");
    session.window(BrowserCommand::Back).unwrap();

    // An external MCP client: the built executable in its proxy mode, with
    // the environment the window hands out and nothing else.
    let config = session.runtime.external_mcp_config().unwrap();
    let executable = std::env::current_exe().unwrap();
    let application = executable.parent().unwrap().parent().unwrap().join("agents-window");
    assert!(application.is_file(), "{application:?} is built by `cargo test -p agents-window`");
    let mut proxy = Command::new(&application)
        .args(&config.args)
        .env_clear()
        .envs(&config.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = proxy.stdin.take().unwrap();
    let mut output = BufReader::new(proxy.stdout.take().unwrap());
    let mut request = |id: u64, method: &str, params: Value| -> Value {
        writeln!(input, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        let mut line = String::new();
        assert!(output.read_line(&mut line).unwrap() > 0, "the proxy stopped answering");
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id);
        reply
    };
    assert_eq!(request(1, "initialize", json!({"protocolVersion": "2025-11-25"}))["result"]["serverInfo"]["name"], "openide-browser");
    let listed = request(2, "tools/list", json!({}));
    let names: Vec<_> = listed["result"]["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap().to_owned()).collect();
    assert_eq!(names, browser_mcp_tools().iter().map(|tool| tool.name.clone()).collect::<Vec<_>>());
    let external = request(3, "tools/call", json!({"name": "browser_snapshot", "arguments": {"interactive_only": true}}));
    let content = external["result"]["content"].as_array().unwrap();
    let state: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(state["screenshot_available"], true);
    // The external snapshot also carries the viewport, as a PNG at CSS scale.
    assert_eq!((&content[1]["type"], &content[1]["mimeType"]), (&json!("image"), &json!("image/png")));
    let picture = base64::engine::general_purpose::STANDARD.decode(content[1]["data"].as_str().unwrap()).unwrap();
    assert_eq!(openide_browser::image_size(&picture), Some((900, 700)));
    let paint = ref_of(state["aria_snapshot"].as_str().unwrap(), "button \"Pintar\"");
    let painted = request(4, "tools/call", json!({"name": "browser_click", "arguments": {"ref": paint}}));
    assert_eq!(painted["result"]["isError"], false);
    let colour = session.agent("browser_evaluate", json!({"expression": "document.body.style.background"}));
    assert_eq!(colour.content[1]["text"], "\"rgb(255, 221, 238)\"", "the external click reached the shared page");
    let missed = request(5, "tools/call", json!({"name": "browser_click", "arguments": {"ref": "e99999"}}));
    assert_eq!(missed["result"]["isError"], true);
    assert!(request(6, "tools/call", json!({"name": "browser_tabs", "arguments": {}})).get("error").is_some());
    drop(input);
    assert!(proxy.wait().unwrap().success());

    // Closing leaves no process and no directory.
    drop(session.runtime);
    assert!(group(process).is_empty(), "{:?}", group(process));
    assert!(!directory.exists());
    assert_eq!(fs::read_dir(&base).unwrap().count(), 0);
}

#[test]
#[ignore = "opt-in: launches a real Chrome against local fixture pages"]
fn real_chrome_frames_reach_the_view_at_the_windows_scale_only_while_it_is_visible() {
    for scale in [2.0, 1.0] {
        frames_at(scale);
    }
}

fn frames_at(scale: f32) {
    let origin = site();
    let session = launch((640, 480), scale);
    let device = [(640.0 * scale) as usize, (480.0 * scale) as usize];
    let wider = [(800.0 * scale) as usize, (600.0 * scale) as usize];
    session.open(format!("{origin}/animated"));

    // Visible and animating: frames arrive decoded, at device pixels.
    let first = session.next_frame("a first frame");
    assert_eq!(first.size(), device);
    assert!(format!("{first:?}").contains(&format!("pixels_per_point: {scale:?}")), "{first:?}");
    let (started, mut frames) = (Instant::now(), 0);
    while started.elapsed() < Duration::from_secs(3) {
        if let Some(frame) = session.runtime.take_frame() {
            assert_eq!(frame.size(), device);
            frames += 1;
        }
        thread::sleep(Duration::from_millis(1));
    }
    // The development profile decodes slowly; the rate itself is measured
    // with the optimised one.
    assert!(frames >= 3, "only {frames} frames in three seconds");
    assert!(session.runtime.snapshot().error.is_none());
    // Replies are not starved by the stream.
    for _ in 0..10 {
        let asked = Instant::now();
        let read = session.agent("browser_evaluate", json!({"expression": "1 + 1"}));
        assert_eq!(read.content[1]["text"], "2");
        assert!(asked.elapsed() < Duration::from_millis(500), "a reply waited {:?} behind frames", asked.elapsed());
    }

    // Hidden: Chrome stops encoding, and the agent still reads the page.
    session.runtime.set_visible(false);
    session.settle(Duration::from_millis(700));
    let _ = session.runtime.take_frame();
    thread::sleep(Duration::from_millis(700));
    assert!(session.runtime.take_frame().is_none(), "a hidden page pushes no frame");
    let read = session.agent("browser_snapshot", json!({}));
    assert!(page(&read)["aria_snapshot"].as_str().unwrap().contains("Texto para arrastrar"));
    // Shown again: the stream is back.
    session.runtime.set_visible(true);
    assert_eq!(session.next_frame("a frame of the new stream").size(), device);

    // A page that holds still gets its frames and then nothing: the last is
    // the lossless one, and it is not a loop.
    session.open(format!("{origin}/"));
    session.settle(Duration::from_millis(900));
    let _ = session.runtime.take_frame();
    let still = session.frames.load(Ordering::Relaxed);
    thread::sleep(Duration::from_millis(1500));
    assert_eq!(session.frames.load(Ordering::Relaxed), still, "a still page pushes nothing");

    // The agent's captures at CSS scale make Chrome draw the page at another
    // scale for a moment. The view never shows that.
    for _ in 0..3 {
        let captured = session.agent("browser_take_screenshot", json!({}));
        assert_eq!(page(&captured)["screenshot"], json!({"width": 640, "height": 480, "bytes": page(&captured)["screenshot"]["bytes"]}));
    }
    let full = session.agent("browser_take_screenshot", json!({"full_page": true}));
    assert!(page(&full)["screenshot"]["height"].as_u64().unwrap() >= 2400);
    thread::sleep(Duration::from_millis(1200));
    assert_eq!(session.frames.load(Ordering::Relaxed), still, "captures of an unchanged page push nothing to the view");
    assert!(session.runtime.take_frame().is_none());
    // It does show what the agent changed, and then holds still again.
    assert!(!session.agent("browser_click", json!({"selector": "#paint"})).is_error);
    assert_eq!(session.next_frame("a frame of what the agent changed").size(), device);
    session.settle(Duration::from_millis(900));
    let painted = session.frames.load(Ordering::Relaxed);
    thread::sleep(Duration::from_millis(1200));
    assert_eq!(session.frames.load(Ordering::Relaxed), painted);

    // A resize keeps the CSS viewport logical and the frames at its device size.
    let resized = session.window(BrowserCommand::Resize { width: 800, height: 600 }).unwrap();
    assert_eq!((resized.viewport_width, resized.viewport_height), (800, 600));
    session.until("a frame at the new size", || {
        session.runtime.take_frame().is_some_and(|frame| frame.size() == wider)
    });
    session.settle(Duration::from_millis(900));
    assert!(session.runtime.snapshot().error.is_none(), "the stream was never lost");
}

#[test]
#[ignore = "opt-in: launches a real Chrome against local fixture pages"]
fn real_chrome_takes_the_windows_pointer_buttons_clicks_wheel_keys_and_clipboard() {
    let origin = site();
    let session = launch((640, 480), 2.0);
    session.open(format!("{origin}/"));
    let left = MouseButton::Left;
    let press = |x: f64, y: f64, button, clicks| {
        session.window(BrowserCommand::PointerDown { x, y, button, clicks }).unwrap();
        session.window(BrowserCommand::PointerUp { x, y, button, clicks }).unwrap()
    };
    let copied = || session.window(BrowserCommand::CopySelection).unwrap().clipboard.unwrap().text;

    // Hover: a move alone changes the page.
    session.window(BrowserCommand::PointerMove { x: 60.0, y: 40.0 }).unwrap();
    session.title("Encima");
    session.window(BrowserCommand::PointerMove { x: 400.0, y: 400.0 }).unwrap();
    session.title("Fuera");

    // Press, drag and release select text like a real pointer.
    session.window(BrowserCommand::PointerDown { x: 21.0, y: 175.0, button: left, clicks: 1 }).unwrap();
    for x in [60.0, 120.0, 200.0] {
        session.window(BrowserCommand::PointerMove { x, y: 175.0 }).unwrap();
    }
    session.window(BrowserCommand::PointerUp { x: 258.0, y: 175.0, button: left, clicks: 1 }).unwrap();
    assert_eq!(copied(), "Texto para arrastrar");

    // The second press of a run selects a word and is the page's double
    // click; the third selects the paragraph.
    press(200.0, 175.0, left, 1);
    assert_eq!(press(200.0, 175.0, left, 2).title, "Doble: arrastrar");
    assert_eq!(copied(), "arrastrar");
    press(200.0, 175.0, left, 3);
    assert_eq!(copied().trim(), "Texto para arrastrar");

    // The right and the middle buttons are the page's own events.
    assert_eq!(press(400.0, 300.0, MouseButton::Right, 1).title, "Menú 2");
    assert_eq!(press(400.0, 300.0, MouseButton::Middle, 1).title, "Medio");

    // The wheel turns where the pointer is.
    session.window(BrowserCommand::Scroll { dx: 0.0, dy: 400.0 }).unwrap();
    session.title("Scroll ");
    session.window(BrowserCommand::Scroll { dx: 0.0, dy: -400.0 }).unwrap();
    session.title("Scroll 0");

    // Typing, a chord, and the clipboard in its two steps: nothing is cut
    // until the host confirms it took the text.
    assert!(!session.agent("browser_click", json!({"selector": "#name"})).is_error);
    session.window(BrowserCommand::Type { text: "Niño 🧠".into() }).unwrap();
    session.window(BrowserCommand::Press { key: "Control+A".into() }).unwrap();
    assert_eq!(copied(), "Niño 🧠");
    let prepared = session.window(BrowserCommand::PrepareCut).unwrap().clipboard.unwrap();
    assert_eq!(prepared.text, "Niño 🧠");
    assert_eq!(copied(), "Niño 🧠", "preparing a cut alone deletes nothing");
    let stale = session.window(BrowserCommand::ConfirmCut { token: prepared.cut_token.unwrap() });
    assert!(stale.is_err(), "a copy invalidates the cut prepared before it");
    let prepared = session.window(BrowserCommand::PrepareCut).unwrap().clipboard.unwrap();
    session.window(BrowserCommand::ConfirmCut { token: prepared.cut_token.unwrap() }).unwrap();
    let value = session.agent("browser_evaluate", json!({"expression": "document.querySelector('#name').value"}));
    assert_eq!(value.content[1]["text"], "\"\"");
    // A copy without a selection is a failed input, shown in the footer.
    let empty = session.window(BrowserCommand::CopySelection).unwrap_err();
    assert!(empty.contains("Seleccioná"), "{empty}");
    assert_eq!(session.runtime.snapshot().error.as_deref(), Some(empty.as_str()));
    assert!(session.runtime.snapshot().started);
}

/// Proportional set size of a process, in megabytes.
fn pss(process: u32) -> f64 {
    fs::read_to_string(format!("/proc/{process}/smaps_rollup"))
        .ok()
        .and_then(|rollup| {
            let line = rollup.lines().find(|line| line.starts_with("Pss:"))?.to_owned();
            line.split_whitespace().nth(1)?.parse::<f64>().ok()
        })
        .map_or(0.0, |kilobytes| kilobytes / 1024.0)
}

fn summary(mut samples: Vec<Duration>) -> String {
    samples.sort();
    let at = |fraction: f64| samples[((samples.len() - 1) as f64 * fraction).round() as usize].as_secs_f64() * 1000.0;
    format!("median {:.1} ms, p90 {:.1} ms, max {:.1} ms ({} samples)", at(0.5), at(0.9), at(1.0), samples.len())
}

/// What `docs/browser-runtime.md` reports, taken through the runtime at
/// 1280 × 800 CSS pixels and scale 2. Run it alone, with the optimised
/// profile and nothing compiling:
///
/// ```sh
/// OPENIDE_BROWSER_EXECUTABLE="$(command -v google-chrome-stable)" nix-shell --run \
///   "cargo test --profile desktop -p agents-window -- --ignored --nocapture real_chrome_measurements"
/// ```
#[test]
#[ignore = "opt-in timing: launches a real Chrome and prints measurements"]
fn real_chrome_measurements_of_the_live_view_and_of_what_the_browser_adds() {
    let origin = site();
    let own = std::process::id();
    // Chrome starts slower the first time after another environment used it.
    let launched = Instant::now();
    let first = launch_as((1280, 800), 2.0, false);
    eprintln!("launch: first context ready in {:.0} ms", launched.elapsed().as_secs_f64() * 1000.0);
    drop(first);

    // Memory, with a view that was never shown so no frame was ever held:
    // what driving the browser adds to this process, beside the browser's
    // own processes.
    let before = pss(own);
    let launched = Instant::now();
    let session = launch_as((1280, 800), 2.0, false);
    let ready = launched.elapsed();
    session.open(format!("{origin}/"));
    let _ = session.agent("browser_snapshot", json!({}));
    thread::sleep(Duration::from_millis(1500));
    let browser: f64 = group(session.process()).into_iter().map(pss).sum();
    eprintln!("memory (PSS): this process grew {:.1} MB with the context open, a page read and the view hidden; Chrome's {} processes hold {browser:.0} MB", pss(own) - before, group(session.process()).len());
    let shown = Instant::now();
    session.runtime.set_visible(true);
    session.next_frame("a frame of the page");
    eprintln!("launch: context ready in {:.0} ms; shown, its first frame is decoded {:.0} ms later", ready.as_secs_f64() * 1000.0, shown.elapsed().as_secs_f64() * 1000.0);
    drop(session);
    let launched = Instant::now();
    let session = launch((1280, 800), 2.0);
    session.next_frame("the first frame");
    eprintln!("launch: visible from the start, first frame decoded {:.0} ms after the start was asked for", launched.elapsed().as_secs_f64() * 1000.0);
    session.open(format!("{origin}/"));
    session.settle(Duration::from_millis(900));
    eprintln!("memory (PSS): this process grew {:.1} MB with the view showing a still page", pss(own) - before);

    // Frames per second on a page that animates.
    session.open(format!("{origin}/animated"));
    session.next_frame("an animated frame");
    thread::sleep(Duration::from_millis(500));
    let (from, started) = (session.frames.load(Ordering::Relaxed), Instant::now());
    thread::sleep(Duration::from_secs(4));
    let animated = (session.frames.load(Ordering::Relaxed) - from) as f64 / started.elapsed().as_secs_f64();
    eprintln!("animated page: {animated:.1} decoded frames per second at 2560 × 1600");
    eprintln!("memory (PSS): this process grew {:.1} MB while streaming it", pss(own) - before);

    // One wheel step, as the window sends it: the time until the page
    // answered, watching for frames meanwhile.
    let wheel = |session: &Session, dy: f64| -> Duration {
        let sent = Instant::now();
        let mut request = session.runtime.submit_ui(BrowserCommand::Scroll { dx: 0.0, dy }).unwrap();
        while request.poll().is_none() {
            assert!(sent.elapsed() < Duration::from_secs(10), "the wheel was never answered");
            thread::sleep(Duration::from_micros(100));
        }
        sent.elapsed()
    };

    // Continuous scroll: a wheel step every 16 ms, one in flight at a time
    // as the window sends them, and from each step to the next decoded frame.
    session.open(format!("{origin}/"));
    session.settle(Duration::from_millis(600));
    session.window(BrowserCommand::PointerMove { x: 640.0, y: 400.0 }).unwrap();
    let (mut moving, mut replies, mut waiting) = (Vec::new(), Vec::new(), Vec::<Instant>::new());
    let (mut direction, mut travelled) = (30.0, 0.0);
    let (from, started) = (session.frames.load(Ordering::Relaxed), Instant::now());
    let mut seen = from;
    while started.elapsed() < Duration::from_secs(4) {
        let tick = Instant::now();
        waiting.push(tick);
        replies.push(wheel(&session, direction));
        travelled += direction;
        if !(0.0..1200.0).contains(&travelled) {
            direction = -direction;
        }
        loop {
            let now = session.frames.load(Ordering::Relaxed);
            if now != seen {
                seen = now;
                moving.extend(waiting.drain(..).map(|sent| sent.elapsed()));
            }
            if tick.elapsed() >= Duration::from_millis(16) {
                break;
            }
            thread::sleep(Duration::from_micros(100));
        }
    }
    let scrolling = (seen - from) as f64 / started.elapsed().as_secs_f64();
    eprintln!("continuous scroll, a wheel step every 16 ms: {scrolling:.1} decoded frames per second");
    eprintln!("wheel to the next decoded frame while scrolling: {}", summary(moving));
    eprintln!("wheel reply to the window while scrolling: {}", summary(replies));
    let mut resting = Vec::new();
    for step in 0..20 {
        session.settle(Duration::from_millis(450));
        let seen = session.frames.load(Ordering::Relaxed);
        let sent = Instant::now();
        wheel(&session, if step % 2 == 0 { 120.0 } else { -120.0 });
        while session.frames.load(Ordering::Relaxed) == seen && sent.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_micros(100));
        }
        resting.push(sent.elapsed());
    }
    eprintln!("wheel to the first decoded frame on a page at rest: {}", summary(resting));

    // What one frame costs this process, on frames of this very page.
    let bytes = {
        let executable = std::env::var_os("OPENIDE_BROWSER_EXECUTABLE").unwrap();
        let mut driver = Driver::launch(&LaunchOptions { executable: executable.into(), width: 1280, height: 800, scale: 2.0, temp_root: None }).unwrap();
        driver.navigate(&format!("{origin}/")).unwrap();
        let stream = driver.start_stream(FrameFormat::Jpeg { quality: FRAME_QUALITY }, None).unwrap();
        let jpeg = stream.next(Duration::from_secs(5)).expect("a frame").data;
        // The same scroll with the driver alone and frames taken undecoded:
        // what the runtime's own path costs is the difference.
        let taken = Arc::new(AtomicUsize::new(0));
        let (counter, frames, done) = (taken.clone(), stream.clone(), Arc::new(AtomicBool::new(false)));
        let stop = done.clone();
        let consumer = thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                if frames.next(Duration::from_millis(50)).is_some() {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        thread::sleep(Duration::from_millis(600));
        let (mut direction, mut travelled, mut replies) = (30.0, 0.0, Vec::new());
        let (from, started) = (taken.load(Ordering::Relaxed), Instant::now());
        while started.elapsed() < Duration::from_secs(4) {
            let tick = Instant::now();
            driver.wheel(640.0, 400.0, 0.0, direction).unwrap();
            replies.push(tick.elapsed());
            travelled += direction;
            if !(0.0..1200.0).contains(&travelled) {
                direction = -direction;
            }
            thread::sleep(Duration::from_millis(16).saturating_sub(tick.elapsed()));
        }
        let rate = (taken.load(Ordering::Relaxed) - from) as f64 / started.elapsed().as_secs_f64();
        done.store(true, Ordering::Release);
        consumer.join().unwrap();
        eprintln!("the driver alone, same page and scroll: {rate:.1} undecoded frames per second; wheel reply {}", summary(replies));
        driver.stop_stream().unwrap();
        [(image::ImageFormat::Jpeg, jpeg), (image::ImageFormat::Png, driver.capture_frame().unwrap().data)]
    };
    let ctx = eframe::egui::Context::default();
    // A window's renderer reports what its GPU takes; a bare context assumes
    // less than a frame at this scale.
    let input = eframe::egui::RawInput {
        max_texture_side: Some(8192),
        ..Default::default()
    };
    ctx.run_ui(input, |_| {}).textures_delta.clear();
    for (format, bytes) in &bytes {
        let (mut decode, mut present) = (Vec::new(), Vec::new());
        for _ in 0..40 {
            let started = Instant::now();
            let decoded = decode_frame(bytes, *format, 2.0).unwrap();
            decode.push(started.elapsed());
            assert_eq!(decoded.size(), [2560, 1600]);
            // The UI thread's own share. The copy to the GPU that follows it
            // inside the renderer is not part of this number.
            let started = Instant::now();
            assert!(limn_egui::present_surface(&ctx, "timing", decoded));
            present.push(started.elapsed());
            ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        }
        present.sort();
        eprintln!("{format:?} frame of {} KiB: decoder thread {}; UI thread median {:.1?}", bytes.len() / 1024, summary(decode), present[20]);
    }
    limn_egui::forget_surface("timing");

    let process = session.process();
    let closing = Instant::now();
    drop(session.runtime);
    eprintln!("close: {:.0} ms, {} browser processes left", closing.elapsed().as_secs_f64() * 1000.0, group(process).len());
}
