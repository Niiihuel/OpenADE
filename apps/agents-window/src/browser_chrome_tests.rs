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
//!
//! With `--features cef` the same scenarios also run on the embedded
//! Chromium, as `real_cef_…`. They need the application built with the
//! feature, whose executable CEF starts its helper processes as, and never
//! open a window or touch the GPU:
//!
//! ```sh
//! nix-shell nix/shell-cef.nix --run \
//!   "cargo test -p agents-window --features cef -- --ignored real_cef --test-threads=1"
//! ```

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

/// What a person uses with the mouse and the keyboard: a list to choose
/// from, a link, a line of text, a field and a text area.
const FORM: &str = r#"<!doctype html><html lang="es"><head><meta charset="utf-8"><title>Formulario</title><style>
html{background:#fff}body{font:16px/1.5 sans-serif;margin:0;min-height:2400px;color:#111}
#pick{position:absolute;left:20px;top:20px;width:200px;height:30px;font:16px sans-serif}
#link{position:absolute;left:300px;top:20px}
#line{position:absolute;left:20px;top:80px;margin:0;font:20px/30px monospace;white-space:nowrap}
#field{position:absolute;left:20px;top:140px;width:300px;font:16px monospace}
#area{position:absolute;left:20px;top:200px;width:300px;height:120px;font:16px monospace}
#no{position:absolute;left:400px;top:80px;width:100px;height:30px;cursor:not-allowed;background:#eee}
#split{position:absolute;left:400px;top:140px;width:100px;height:30px;cursor:col-resize;background:#eee}
</style></head><body>
<select id="pick" aria-label="Fruta">OPTIONS</select>
<a id="link" href="/">Otra página</a>
<p id="line">Uno dos tres cuatro cinco</p>
<input id="field" aria-label="Campo">
<textarea id="area" aria-label="Área"></textarea>
<div id="no"></div><div id="split"></div>
<script>
const held = event => ['alt', 'ctrl', 'meta', 'shift'].filter(key => event[key + 'Key']).join('+');
const note = (...parts) => seen.push(parts.filter(part => part !== '').join(' '));
window.seen = [];
for (const kind of ['click', 'auxclick']) addEventListener(kind, event => note(kind, held(event), event.target.id), true);
addEventListener('wheel', event => note('wheel', held(event), Math.round(event.deltaX) + ',' + Math.round(event.deltaY)), true);
addEventListener('keydown', event => note('key', held(event), event.key), true);
addEventListener('mousemove', event => { window.buttons = event.buttons; }, true);
</script></body></html>"#;

/// Every pixel of the view changes with every frame.
const FULL: &str = r#"<!doctype html><html lang="es"><head><meta charset="utf-8"><title>Completa</title><style>
html{animation:hue 2s linear infinite alternate}@keyframes hue{from{background:#024}to{background:#fc8}}
</style></head><body></body></html>"#;

/// Ways a page asks for another tab or another window.
const LINKS: &str = r#"<!doctype html><html lang="es"><head><meta charset="utf-8"><title>Enlaces</title><style>
html{background:#fff}body{font:16px/1.5 sans-serif;margin:0}a,button{position:absolute;left:20px;font:inherit}
</style></head><body>
<a id="blank" style="top:20px" target="_blank" href="/landing?blank">Otra pestaña</a>
<button id="open" style="top:60px" onclick="window.opened = window.open('/landing?open')">Otra ventana</button>
<a id="plain" style="top:100px" href="/landing?plain">Enlace</a>
<a id="named" style="top:140px" target="ayuda" href="/landing?named">Ventana con nombre</a>
<a id="data" style="top:180px" target="_blank" href="data:text/html,<title>No</title>">Datos</a>
<form method="post" action="/landing?post" target="_blank"><button id="post" style="top:220px">Enviar</button></form>
<button id="empty" style="top:260px" onclick="window.opened = window.open()">Ventana vacía</button>
</body></html>"#;

/// The fixture pages on a free loopback port: `/` holds still, `/animated`
/// never does, `/form` is the one above, `/full` changes whole, `/links` asks
/// for other tabs, each of which is a `/landing?…` that `/landed` lists as
/// they were asked for. Only this machine can reach them.
fn site() -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        let rows: String = (0..40)
            .map(|row| format!(r#"<div class="row"><b>Fila {row}</b> — Lorem ipsum dolor sit amet, consectetur adipiscing elit.</div>"#))
            .collect();
        let mut landed = Vec::new();
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 2048];
            let read = stream.read(&mut request).unwrap_or(0);
            let animated = request[..read].starts_with(b"GET /animated");
            let asked = String::from_utf8_lossy(&request[..read]);
            let mut asked = asked.split_whitespace();
            let (method, path) = (asked.next().unwrap_or(""), asked.next().unwrap_or(""));
            let body = if request[..read].starts_with(b"GET /form") {
                let options: String = (0..30).map(|option| format!("<option>Opción {option}</option>")).collect();
                FORM.replace("OPTIONS", &options)
            } else if request[..read].starts_with(b"GET /full") {
                FULL.to_owned()
            } else if path == "/links" {
                LINKS.to_owned()
            } else if let Some(which) = path.strip_prefix("/landing?") {
                landed.push(format!("{method} {which}"));
                format!(r#"<!doctype html><html lang="es"><head><meta charset="utf-8"><title>Destino {which}</title></head><body><p>Llegaste por {which}</p></body></html>"#)
            } else if path == "/landed" {
                landed.join(",")
            } else {
                PAGE.replace("ROWS", &rows)
                    .replace("SPIN", if animated { r#"<div id="spin"></div>"# } else { "" })
            };
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    origin
}

/// What a context runs on.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Chromium {
    /// The Chrome or Chromium of `OPENIDE_BROWSER_EXECUTABLE`, headless.
    Launched,
    /// The one embedded with `--features cef`.
    #[cfg(feature = "cef")]
    Embedded,
}

struct Session {
    runtime: BrowserRuntime,
    engine: Chromium,
    data: tempfile::TempDir,
    _workspace: tempfile::TempDir,
    /// Counts the frames the decoder published for the window.
    frames: Arc<AtomicUsize>,
}

fn launch(engine: Chromium, viewport: (u32, u32), scale: f32) -> Session {
    launch_as(engine, viewport, scale, true)
}

/// `visible` is what the surface is from before the browser starts, as the
/// window sets it for a tab that is not shown.
fn launch_as(engine: Chromium, viewport: (u32, u32), scale: f32, visible: bool) -> Session {
    let browser_executable = match engine {
        Chromium::Launched => Some(
            std::env::var_os("OPENIDE_BROWSER_EXECUTABLE")
                .expect("set OPENIDE_BROWSER_EXECUTABLE to a Chrome or Chromium")
                .into(),
        ),
        // The window names no browser, and gets the embedded one.
        #[cfg(feature = "cef")]
        Chromium::Embedded => None,
    };
    let (data, workspace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let runtime = BrowserRuntime::start(
        BrowserScope {
            workspace: workspace.path().into(),
            session_id: "real-chrome".into(),
        },
        data.path(),
        BrowserLaunchOptions {
            browser_executable,
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
        engine,
        data,
        _workspace: workspace,
        frames,
    };
    session.until("the context to start", || session.runtime.snapshot().started);
    // Never a silent fallback to the other one.
    #[cfg(feature = "cef")]
    assert_eq!(
        session.runtime.handle.view.embedded.is_open(),
        engine == Chromium::Embedded,
        "the context runs on {engine:?}"
    );
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

    /// The browser's own processes: Chrome's group, or the helpers the
    /// embedded Chromium started below this process.
    fn browser_processes(&self) -> Vec<u32> {
        match self.engine {
            Chromium::Launched => group(self.process()),
            #[cfg(feature = "cef")]
            Chromium::Embedded => descendants(std::process::id()),
        }
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

/// The size of the frame the view was handed, or of the one it was handed a
/// part of: an embedded page hands over what changed.
fn extent(frame: &limn_egui::SurfaceFrame) -> [usize; 2] {
    frame.part().map_or(frame.size(), |(whole, _)| whole)
}

/// The canvas an embedded page is copied to was, after every paint, pixel
/// for pixel what Chromium had painted: the rectangle it names as changed is
/// all that did.
fn every_paint_left_the_canvas_as_chromium_painted_it() {
    #[cfg(feature = "cef")]
    assert_eq!(cef::STRAY_PAINTS.load(Ordering::Relaxed), 0, "paints after which the canvas was not what Chromium holds");
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

/// The fields of `/proc/<process>/stat` after the command, which may itself
/// hold spaces: state, parent, group, and on.
fn stat(process: u32) -> Vec<String> {
    let stat = fs::read_to_string(format!("/proc/{process}/stat")).unwrap_or_default();
    stat.rsplit(')').next().unwrap_or("").split_whitespace().map(str::to_owned).collect()
}

fn running() -> Vec<u32> {
    fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|process| stat(*process).first().is_some_and(|state| state != "Z"))
        .collect()
}

/// Every process of the browser's group that is still running.
fn group(process: u32) -> Vec<u32> {
    running()
        .into_iter()
        .filter(|candidate| stat(*candidate).get(2).and_then(|group| group.parse().ok()) == Some(process))
        .collect()
}

/// Every running process started, directly or not, by `process`.
#[cfg(feature = "cef")]
fn descendants(process: u32) -> Vec<u32> {
    let parents: Vec<(u32, u32)> = running()
        .into_iter()
        .filter_map(|candidate| Some((candidate, stat(candidate).get(1)?.parse().ok()?)))
        .collect();
    let mut found = vec![process];
    let mut next = 0;
    while next < found.len() {
        let parent = found[next];
        found.extend(parents.iter().filter(|(_, of)| *of == parent).map(|(child, _)| *child));
        next += 1;
    }
    found.split_off(1)
}

/// Processor time `processes` have used, in seconds.
fn processor_seconds(processes: &[u32]) -> f64 {
    let ticks: u64 = processes
        .iter()
        .map(|process| {
            let fields = stat(*process);
            // utime and stime, the 14th and 15th fields of the whole line.
            [11, 12].iter().filter_map(|field| fields.get(*field)?.parse::<u64>().ok()).sum::<u64>()
        })
        .sum();
    ticks as f64 / unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64
}

#[test]
#[ignore = "opt-in: launches a real Chrome against local fixture pages"]
fn real_chrome_serves_the_agent_and_an_external_client_and_leaves_nothing_behind() {
    let origin = site();
    let session = launch(Chromium::Launched, (900, 700), 2.0);
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
    let session = serves_the_agent_and_an_external_client(session, &origin);

    // Closing leaves no process and no directory.
    drop(session.runtime);
    assert!(group(process).is_empty(), "{:?}", group(process));
    assert!(!directory.exists());
    assert_eq!(fs::read_dir(&base).unwrap().count(), 0);
}

fn serves_the_agent_and_an_external_client(session: Session, origin: &str) -> Session {

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
    session
}

#[test]
#[ignore = "opt-in: launches a real Chrome against local fixture pages"]
fn real_chrome_frames_reach_the_view_at_the_windows_scale_only_while_it_is_visible() {
    for scale in [2.0, 1.0] {
        frames_at(Chromium::Launched, scale);
    }
}

fn frames_at(engine: Chromium, scale: f32) {
    let origin = site();
    let session = launch(engine, (640, 480), scale);
    let device = [(640.0 * scale) as usize, (480.0 * scale) as usize];
    let wider = [(800.0 * scale) as usize, (600.0 * scale) as usize];
    session.open(format!("{origin}/animated"));

    // Visible and animating: frames arrive decoded, at device pixels.
    let first = session.next_frame("a first frame");
    assert_eq!((first.size(), first.part()), (device, None), "the first is a whole frame");
    assert!(format!("{first:?}").contains(&format!("pixels_per_point: {scale:?}")), "{first:?}");
    let (started, mut frames, mut corners) = (Instant::now(), 0, 0);
    while started.elapsed() < Duration::from_secs(3) {
        if let Some(frame) = session.runtime.take_frame() {
            assert_eq!(extent(&frame), device);
            let [width, height] = frame.size();
            corners += usize::from(frame.part().is_some() && width * height * 20 < device[0] * device[1]);
            frames += 1;
        }
        thread::sleep(Duration::from_millis(1));
    }
    // The development profile decodes slowly; the rate itself is measured
    // with the optimised one.
    assert!(frames >= 3, "only {frames} frames in three seconds");
    // The launched Chrome sends whole frames. The embedded one hands over
    // the corner where the page animates, and little else once it loaded.
    #[cfg(feature = "cef")]
    if engine == Chromium::Embedded {
        assert!(corners * 10 >= frames * 9, "{corners} of {frames} frames were the corner that animates");
    }
    let _ = corners;
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
    let shown = session.next_frame("a frame of the new stream");
    assert_eq!((shown.size(), shown.part()), (device, None), "shown again, the view is owed a whole frame");

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
    assert_eq!(extent(&session.next_frame("a frame of what the agent changed")), device);
    session.settle(Duration::from_millis(900));
    let painted = session.frames.load(Ordering::Relaxed);
    thread::sleep(Duration::from_millis(1200));
    assert_eq!(session.frames.load(Ordering::Relaxed), painted);

    // A resize keeps the CSS viewport logical and the frames at its device size.
    let resized = session.window(BrowserCommand::Resize { width: 800, height: 600 }).unwrap();
    assert_eq!((resized.viewport_width, resized.viewport_height), (800, 600));
    session.until("a frame at the new size", || {
        session.runtime.take_frame().is_some_and(|frame| frame.size() == wider && frame.part().is_none())
    });
    session.settle(Duration::from_millis(900));
    assert!(session.runtime.snapshot().error.is_none(), "the stream was never lost");
    every_paint_left_the_canvas_as_chromium_painted_it();
}

#[test]
#[ignore = "opt-in: launches a real Chrome against local fixture pages"]
fn real_chrome_takes_the_windows_pointer_buttons_clicks_wheel_keys_and_clipboard() {
    takes_the_windows_input(Chromium::Launched);
}

fn takes_the_windows_input(engine: Chromium) {
    let origin = site();
    let session = launch(engine, (640, 480), 2.0);
    session.open(format!("{origin}/"));
    let left = MouseButton::Left;
    let press = |x: f64, y: f64, button, clicks| {
        session.window(BrowserCommand::PointerDown { x, y, button, clicks, modifiers: 0 }).unwrap();
        session.window(BrowserCommand::PointerUp { x, y, button, clicks, modifiers: 0 }).unwrap()
    };
    let copied = || session.window(BrowserCommand::CopySelection).unwrap().clipboard.unwrap().text;

    // Hover: a move alone changes the page.
    session.window(BrowserCommand::PointerMove { x: 60.0, y: 40.0, modifiers: 0 }).unwrap();
    session.title("Encima");
    session.window(BrowserCommand::PointerMove { x: 400.0, y: 400.0, modifiers: 0 }).unwrap();
    session.title("Fuera");

    // Press, drag and release select text like a real pointer.
    session.window(BrowserCommand::PointerDown { x: 21.0, y: 175.0, button: left, clicks: 1, modifiers: 0 }).unwrap();
    for x in [60.0, 120.0, 200.0] {
        session.window(BrowserCommand::PointerMove { x, y: 175.0, modifiers: 0 }).unwrap();
    }
    session.window(BrowserCommand::PointerUp { x: 258.0, y: 175.0, button: left, clicks: 1, modifiers: 0 }).unwrap();
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
    session.window(BrowserCommand::Scroll { dx: 0.0, dy: 400.0, modifiers: 0 }).unwrap();
    session.title("Scroll ");
    session.window(BrowserCommand::Scroll { dx: 0.0, dy: -400.0, modifiers: 0 }).unwrap();
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
    measurements(Chromium::Launched);
}

fn measurements(engine: Chromium) {
    let origin = site();
    let own = std::process::id();
    // The tests compare every paint with the canvas. The application does not.
    #[cfg(feature = "cef")]
    cef::COMPARE_PAINTS.store(false, Ordering::Relaxed);
    #[cfg(feature = "cef")]
    let untouched = pss(own);
    // Chrome starts slower the first time after another environment used it.
    // The embedded Chromium starts with the first context of the process.
    let launched = Instant::now();
    let first = launch_as(engine, (1280, 800), 2.0, false);
    eprintln!("launch: first context ready in {:.0} ms", launched.elapsed().as_secs_f64() * 1000.0);
    drop(first);

    // Memory, with a view that was never shown so no frame was ever held:
    // what driving the browser adds to this process, beside the browser's
    // own processes. An embedded Chromium's browser process is this one, so
    // what starting it added is counted too.
    let before = match engine {
        Chromium::Launched => pss(own),
        #[cfg(feature = "cef")]
        Chromium::Embedded => untouched,
    };
    let launched = Instant::now();
    let session = launch_as(engine, (1280, 800), 2.0, false);
    let ready = launched.elapsed();
    session.open(format!("{origin}/"));
    let _ = session.agent("browser_snapshot", json!({}));
    thread::sleep(Duration::from_millis(1500));
    let processes = session.browser_processes();
    let browser: f64 = processes.iter().copied().map(pss).sum();
    eprintln!("memory (PSS): this process grew {:.1} MB with the context open, a page read and the view hidden; the browser's {} processes hold {browser:.0} MB", pss(own) - before, processes.len());
    let shown = Instant::now();
    session.runtime.set_visible(true);
    session.next_frame("a frame of the page");
    eprintln!("launch: context ready in {:.0} ms; shown, its first frame is decoded {:.0} ms later", ready.as_secs_f64() * 1000.0, shown.elapsed().as_secs_f64() * 1000.0);
    drop(session);
    let launched = Instant::now();
    let session = launch(engine, (1280, 800), 2.0);
    session.next_frame("the first frame");
    eprintln!("launch: visible from the start, first frame decoded {:.0} ms after the start was asked for", launched.elapsed().as_secs_f64() * 1000.0);
    session.open(format!("{origin}/"));
    session.settle(Duration::from_millis(900));
    eprintln!("memory (PSS): this process grew {:.1} MB with the view showing a still page", pss(own) - before);

    // Frames per second on a page that animates, and what producing them
    // costs: this process, which turns them into pixels, and the browser's.
    let busy = |session: &Session| (processor_seconds(&[own]), processor_seconds(&session.browser_processes()));
    let share = |session: &Session, from: (f64, f64), seconds: f64| {
        let now = busy(session);
        format!("this process {:.0} % of one core, the browser's processes {:.0} %", (now.0 - from.0) / seconds * 100.0, (now.1 - from.1) / seconds * 100.0)
    };
    session.open(format!("{origin}/animated"));
    session.next_frame("an animated frame");
    thread::sleep(Duration::from_millis(500));
    let (from, started, used) = (session.frames.load(Ordering::Relaxed), Instant::now(), busy(&session));
    thread::sleep(Duration::from_secs(4));
    let elapsed = started.elapsed().as_secs_f64();
    let animated = (session.frames.load(Ordering::Relaxed) - from) as f64 / elapsed;
    eprintln!("animated page: {animated:.1} decoded frames per second at 2560 × 1600");
    eprintln!("processor while it animates on screen: {}", share(&session, used, elapsed));
    eprintln!("memory (PSS): this process grew {:.1} MB while streaming it", pss(own) - before);
    // Frames that are no longer in use may still be held by the allocator.
    unsafe { libc::malloc_trim(0) };
    eprintln!("memory (PSS): this process grew {:.1} MB, counted after the allocator gave back what it held free", pss(own) - before);
    let processes = session.browser_processes();
    eprintln!("memory (PSS): the browser's {} processes hold {:.0} MB while it animates on screen", processes.len(), processes.iter().copied().map(pss).sum::<f64>());

    // The same page off screen: what it still costs, and how long the agent
    // waits for a reading and for a picture of it.
    session.runtime.set_visible(false);
    thread::sleep(Duration::from_millis(700));
    let (started, used) = (Instant::now(), busy(&session));
    thread::sleep(Duration::from_secs(3));
    eprintln!("processor while it animates off screen: {}", share(&session, used, started.elapsed().as_secs_f64()));
    let timed = |name: &str, arguments: Value| -> Vec<Duration> {
        (0..8)
            .map(|_| {
                let asked = Instant::now();
                assert!(!session.agent(name, arguments.clone()).is_error);
                asked.elapsed()
            })
            .collect()
    };
    eprintln!("off screen, the agent's evaluate: {}", summary(timed("browser_evaluate", json!({"expression": "1 + 1"}))));
    eprintln!("off screen, the agent's screenshot: {}", summary(timed("browser_take_screenshot", json!({}))));
    eprintln!("off screen, the agent's click: {}", summary(timed("browser_click", json!({"selector": "#paint"}))));
    session.runtime.set_visible(true);
    session.next_frame("a frame once shown again");
    eprintln!("on screen, the agent's screenshot: {}", summary(timed("browser_take_screenshot", json!({}))));
    eprintln!("on screen, the agent's click: {}", summary(timed("browser_click", json!({"selector": "#paint"}))));

    // One wheel step, as the window sends it: the time until the page
    // answered, watching for frames meanwhile.
    let wheel = |session: &Session, dy: f64| -> Duration {
        let sent = Instant::now();
        let mut request = session.runtime.submit_ui(BrowserCommand::Scroll { dx: 0.0, dy, modifiers: 0 }).unwrap();
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
    session.window(BrowserCommand::PointerMove { x: 640.0, y: 400.0, modifiers: 0 }).unwrap();
    let (mut moving, mut replies, mut waiting) = (Vec::new(), Vec::new(), Vec::<Instant>::new());
    let (mut direction, mut travelled) = (30.0, 0.0);
    let (from, started, used) = (session.frames.load(Ordering::Relaxed), Instant::now(), busy(&session));
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
    let elapsed = started.elapsed().as_secs_f64();
    let scrolling = (seen - from) as f64 / elapsed;
    eprintln!("continuous scroll, a wheel step every 16 ms: {scrolling:.1} decoded frames per second");
    eprintln!("processor while scrolling: {}", share(&session, used, elapsed));
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

    let ctx = eframe::egui::Context::default();
    // A window's renderer reports what its GPU takes; a bare context assumes
    // less than a frame at this scale.
    let input = eframe::egui::RawInput {
        max_texture_side: Some(8192),
        ..Default::default()
    };
    ctx.run_ui(input, |_| {}).textures_delta.clear();
    // The UI thread's own share of one frame. The copy to the GPU that
    // follows it inside the renderer is not part of this number.
    let present = |frame: limn_egui::SurfaceFrame| -> Duration {
        assert_eq!(frame.size(), [2560, 1600]);
        let started = Instant::now();
        assert!(limn_egui::present_surface(&ctx, "timing", frame));
        let taken = started.elapsed();
        ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        taken
    };
    match engine {
        Chromium::Launched => {
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
            for (format, bytes) in &bytes {
                let (mut decode, mut presented) = (Vec::new(), Vec::new());
                for _ in 0..40 {
                    let started = Instant::now();
                    let decoded = decode_frame(bytes, *format, 2.0).unwrap();
                    decode.push(started.elapsed());
                    presented.push(present(decoded));
                }
                presented.sort();
                eprintln!("{format:?} frame of {} KiB: decoder thread {}; UI thread median {:.1?}", bytes.len() / 1024, summary(decode), presented[20]);
            }
        }
        #[cfg(feature = "cef")]
        Chromium::Embedded => {
            native_wheel(&session);
            // What one frame costs this process: the copy of Chromium's
            // pixels on CEF's UI thread, and the frame built from them.
            let bgra = vec![0x7fu8; 2560 * 1600 * 4];
            let (mut canvas, mut rgb) = (Vec::new(), vec![0u8; 2560 * 1600 * 3]);
            let (mut painted, mut built, mut presented) = (Vec::new(), Vec::new(), Vec::new());
            for _ in 0..40 {
                let started = Instant::now();
                canvas.clear();
                canvas.extend_from_slice(&bgra);
                painted.push(started.elapsed());
                let started = Instant::now();
                cef::bgra_to_rgb(&canvas, &mut rgb);
                let frame = limn_egui::SurfaceFrame::from_rgb([2560, 1600], &rgb, 2.0).unwrap();
                built.push(started.elapsed());
                presented.push(present(frame));
            }
            presented.sort();
            eprintln!("uncompressed frame of 2560 × 1600: CEF's UI thread {}; frame thread {}; UI thread median {:.1?}", summary(painted), summary(built), presented[20]);
        }
    }
    limn_egui::forget_surface("timing");

    let closing = Instant::now();
    match engine {
        Chromium::Launched => {
            let process = session.process();
            drop(session.runtime);
            eprintln!("close: {:.0} ms, {} browser processes left", closing.elapsed().as_secs_f64() * 1000.0, group(process).len());
        }
        #[cfg(feature = "cef")]
        Chromium::Embedded => {
            drop(session.runtime);
            eprintln!("close: {:.0} ms; {} processes of the embedded Chromium stay for the next context", closing.elapsed().as_secs_f64() * 1000.0, descendants(own).len());
        }
    }
    #[cfg(feature = "cef")]
    cef::COMPARE_PAINTS.store(true, Ordering::Relaxed);
}

// ---- What a person does with the mouse and the keyboard -----------------------

/// What an expression is in the page.
fn value(session: &Session, expression: &str) -> Value {
    let read = session.agent("browser_evaluate", json!({"expression": expression}));
    assert!(!read.is_error, "{expression}: {:?}", read.content);
    serde_json::from_str(read.content[1]["text"].as_str().unwrap()).unwrap()
}

/// What the window's texture of the page holds: every frame, and every part
/// of one, that the runtime handed over, presented as the window presents
/// them and written where egui tells a renderer to write them.
struct Texture {
    ctx: eframe::egui::Context,
    image: eframe::egui::ColorImage,
}

impl Texture {
    fn new() -> Self {
        let ctx = eframe::egui::Context::default();
        let input = eframe::egui::RawInput {
            max_texture_side: Some(8192),
            ..Default::default()
        };
        ctx.run_ui(input, |_| {}).textures_delta.clear();
        Self {
            ctx,
            image: Default::default(),
        }
    }

    /// Takes what the runtime has for the view, if anything, and says how
    /// many pixels it carried as a part of a frame: none when it was a whole
    /// one.
    fn take(&mut self, session: &Session) -> Option<usize> {
        let frame = session.runtime.take_frame()?;
        let part = frame.part().map_or(0, |_| frame.size()[0] * frame.size()[1]);
        assert!(limn_egui::present_surface(&self.ctx, "texture", frame), "a part of the frame the view shows");
        let mut delta = self.ctx.run_ui(Default::default(), |_| {}).textures_delta;
        for write in delta.set.values().flatten() {
            let eframe::egui::ImageData::Color(image) = &write.image;
            match write.pos {
                None => self.image = (**image).clone(),
                Some([left, top]) => {
                    for (row, pixels) in image.pixels.chunks_exact(image.size[0]).enumerate() {
                        let start = (top + row) * self.image.size[0] + left;
                        self.image.pixels[start..start + pixels.len()].copy_from_slice(pixels);
                    }
                }
            }
        }
        delta.clear();
        Some(part)
    }
}

impl Drop for Texture {
    fn drop(&mut self) {
        limn_egui::forget_surface("texture");
        self.ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
    }
}

#[test]
#[ignore = "opt-in: launches a real Chrome against local fixture pages"]
fn real_chrome_draws_the_list_of_a_select_and_takes_keys_on_it() {
    for scale in [2.0, 1.0] {
        the_list_of_a_select(Chromium::Launched, scale);
    }
}

/// The list of a `<select>` is a widget of its own, which Chromium paints
/// apart from the page: the view shows it where it opens, at the page's
/// scale, and what is done on it chooses.
fn the_list_of_a_select(engine: Chromium, scale: f32) {
    let origin = site();
    let session = launch(engine, (640, 480), scale);
    session.open(format!("{origin}/form"));
    // A click sent the instant a load ends can be lost on the embedded
    // Chromium: the page is clicked once it holds still.
    session.settle(Duration::from_millis(300));
    let click = |x: f64, y: f64| {
        let (button, clicks, modifiers) = (MouseButton::Left, 1, 0);
        session.window(BrowserCommand::PointerDown { x, y, button, clicks, modifiers }).unwrap();
        session.window(BrowserCommand::PointerUp { x, y, button, clicks, modifiers }).unwrap();
    };
    let press = |key: &str| session.window(BrowserCommand::Press { key: key.into() }).unwrap();
    let chosen = || value(&session, "document.querySelector('#pick').value");
    // The select is 200 × 30 at (20, 20) and its list opens under it, in rows
    // of 26.5 CSS pixels. The chosen row of an open list is blue.
    let row = |index: f64| 50.0 + 26.5 * (index + 0.5);
    let shown = std::cell::RefCell::new(Texture::new());
    let blue = |what: &str, index: f64, wanted: bool| {
        session.until(what, || {
            let mut shown = shown.borrow_mut();
            while shown.take(&session).is_some() {}
            if shown.image.size == [0, 0] {
                return false;
            }
            let colour = shown.image[((150.0 * scale) as usize, (row(index) as f32 * scale) as usize)];
            (colour.b() > colour.r().saturating_add(60)) == wanted
        });
    };

    // Opened with the pointer, it is drawn under the select.
    click(100.0, 35.0);
    blue("the list under the select, its first row chosen", 0.0, true);
    #[cfg(feature = "cef")]
    if engine == Chromium::Embedded {
        let (at, size) = session.runtime.handle.view.embedded.popup_box().expect("the list is open");
        assert_eq!((at, size[0]), (((20.0 * scale) as i32, (50.0 * scale) as i32), (200.0 * scale) as usize), "{size:?}");
    }
    // The keyboard moves along it and chooses, and it is gone.
    press("ArrowDown");
    press("ArrowDown");
    blue("the third row chosen", 2.0, true);
    press("Enter");
    assert_eq!(chosen(), "Opción 2");
    blue("the page again where the list was", 2.0, false);
    // Escape closes it with nothing else chosen.
    click(100.0, 35.0);
    blue("the list again", 2.0, true);
    press("Escape");
    blue("the page again", 2.0, false);
    assert_eq!(chosen(), "Opción 2");

    click(100.0, 35.0);
    blue("the list once more", 2.0, true);
    match engine {
        // The launched Chrome draws the list but its protocol's clicks only
        // ever reach the page under it: one there closes the list.
        Chromium::Launched => {
            click(100.0, row(4.0));
            blue("the page again", 2.0, false);
            assert_eq!(chosen(), "Opción 2");
        }
        #[cfg(feature = "cef")]
        Chromium::Embedded => {
            let embedded = &session.runtime.handle.view.embedded;
            // The pointer over a row, and a click on it.
            session.window(BrowserCommand::PointerMove { x: 100.0, y: row(4.0), modifiers: 0 }).unwrap();
            blue("the row under the pointer", 4.0, true);
            click(100.0, row(4.0));
            session.until("the list to close", || embedded.popup_box().is_none());
            assert_eq!(chosen(), "Opción 4");
            blue("the page again", 4.0, false);
            // The wheel turns the list, not the page under it.
            click(100.0, 35.0);
            session.until("the list to open", || embedded.popup_box().is_some());
            session.window(BrowserCommand::PointerMove { x: 100.0, y: row(4.0), modifiers: 0 }).unwrap();
            session.window(BrowserCommand::Scroll { dx: 0.0, dy: 265.0, modifiers: 0 }).unwrap();
            session.settle(Duration::from_millis(500));
            click(100.0, row(4.0));
            session.until("the list to close", || embedded.popup_box().is_none());
            let further: u32 = chosen().as_str().unwrap().trim_start_matches("Opción ").parse().unwrap();
            assert!(further >= 10, "the row under the pointer is now Opción {further}");
            assert_eq!(value(&session, "scrollY"), 0);
            // Pressed on the select, dragged to a row and released there: the
            // row is chosen, and the page knows the button came up.
            let (button, clicks, modifiers) = (MouseButton::Left, 1, 0);
            session.window(BrowserCommand::PointerDown { x: 100.0, y: 35.0, button, clicks, modifiers }).unwrap();
            session.until("the list to open", || embedded.popup_box().is_some());
            session.window(BrowserCommand::PointerMove { x: 100.0, y: row(1.0), modifiers }).unwrap();
            session.window(BrowserCommand::PointerUp { x: 100.0, y: row(1.0), button, clicks, modifiers }).unwrap();
            session.until("the list to close", || embedded.popup_box().is_none());
            assert_eq!(chosen(), "Opción 1");
            session.window(BrowserCommand::PointerMove { x: 500.0, y: 400.0, modifiers }).unwrap();
            session.until("the page to hear the pointer move", || value(&session, "window.buttons") == 0);
            // A list left open goes with the view, and comes back with it.
            click(100.0, 35.0);
            session.until("the list to open", || embedded.popup_box().is_some());
            session.runtime.set_visible(false);
            session.until("the hidden view to let go of it", || embedded.popup_box().is_none());
            session.runtime.set_visible(true);
            session.until("the list of the view shown again", || embedded.popup_box().is_some());
        }
    }
    every_paint_left_the_canvas_as_chromium_painted_it();
}

#[test]
#[ignore = "opt-in: launches a real Chrome against local fixture pages"]
fn real_chrome_takes_modifier_keys_with_the_pointer_and_the_wheel() {
    modifier_keys_with_the_pointer_and_the_wheel(Chromium::Launched);
}

fn modifier_keys_with_the_pointer_and_the_wheel(engine: Chromium) {
    use openide_browser::modifiers::{ALT, CONTROL, SHIFT};
    let origin = site();
    let session = launch(engine, (640, 480), 2.0);
    let form = format!("{origin}/form");
    session.open(form.clone());
    // A click sent the instant a load ends can be lost on the embedded
    // Chromium: the page is clicked once it holds still.
    session.settle(Duration::from_millis(300));
    let (left, middle) = (MouseButton::Left, MouseButton::Middle);
    let click = |x: f64, y: f64, button, modifiers| {
        session.window(BrowserCommand::PointerDown { x, y, button, clicks: 1, modifiers }).unwrap();
        session.window(BrowserCommand::PointerUp { x, y, button, clicks: 1, modifiers }).unwrap();
    };
    // What the page heard since it was last asked.
    let seen = || value(&session, "seen.splice(0).join(' | ')");

    // Shift + click extends the selection from where the last click left it.
    click(22.0, 95.0, left, 0);
    click(120.0, 95.0, left, SHIFT);
    assert_eq!(value(&session, "getSelection().toString()"), "Uno dos ");
    assert_eq!(seen(), "click line | click shift line");
    // The page's own handlers are told which keys were held, with any button
    // and with the wheel, which Shift turns sideways before it gets here.
    value(&session, "(link => link.onclick = link.onauxclick = event => event.preventDefault())(document.querySelector('#link')); 0");
    click(320.0, 30.0, left, CONTROL);
    click(320.0, 30.0, middle, SHIFT);
    click(450.0, 95.0, left, ALT);
    click(450.0, 95.0, left, CONTROL | SHIFT);
    assert_eq!(seen(), "click ctrl link | auxclick shift link | click alt no | click ctrl+shift no");
    session.window(BrowserCommand::Scroll { dx: 120.0, dy: 0.0, modifiers: SHIFT }).unwrap();
    session.until("the wheel to reach the page", || value(&session, "seen.length") == 1);
    assert_eq!(seen(), "wheel shift 120,0");
    assert_eq!(value(&session, "scrollY"), 0);

    // A link clicked for another tab or another window, where there is one
    // page and no other is ever made.
    value(&session, "(link => link.onclick = link.onauxclick = null)(document.querySelector('#link')); 0");
    for (button, modifiers) in [(left, CONTROL), (middle, 0), (left, SHIFT)] {
        click(320.0, 30.0, button, modifiers);
        // The embedded Chromium is asked first, has nowhere else to open it
        // and opens it in the page itself. The launched Chrome makes the
        // tab; the driver closes it and opens its address in the page.
        session.until("the link to open in the page itself", || session.runtime.snapshot().url == format!("{origin}/"));
        session.open(form.clone());
        session.settle(Duration::from_millis(300));
    }
    // The one page is still there and still drawn.
    session.settle(Duration::from_millis(700));
    let _ = session.runtime.take_frame();
    value(&session, "document.body.style.background = '#fde'; 0");
    session.next_frame("a frame of the page after those clicks");
    assert!(session.runtime.snapshot().error.is_none());
    every_paint_left_the_canvas_as_chromium_painted_it();
}

#[test]
#[ignore = "opt-in: launches a real Chrome against local fixture pages"]
fn real_chrome_opens_in_the_view_what_a_click_asked_to_see_in_another_tab() {
    what_a_click_asked_to_see_in_another_tab(Chromium::Launched);
}

/// One panel has one page. A link for another tab, a script that opens a
/// window and a link clicked with Control, Shift or the middle button open in
/// that page when a click asked for them, the user's or the agent's alike. A
/// window the page opens by itself opens nothing.
fn what_a_click_asked_to_see_in_another_tab(engine: Chromium) {
    use openide_browser::modifiers::{CONTROL, SHIFT};
    let origin = site();
    let session = launch(engine, (640, 480), 2.0);
    let links = format!("{origin}/links");
    let landing = |which: &str| format!("{origin}/landing?{which}");
    let (left, middle) = (MouseButton::Left, MouseButton::Middle);
    let click = |y: f64, button, modifiers| {
        session.window(BrowserCommand::PointerDown { x: 40.0, y, button, clicks: 1, modifiers }).unwrap();
        session.window(BrowserCommand::PointerUp { x: 40.0, y, button, clicks: 1, modifiers }).unwrap();
    };
    // A click sent the instant a load ends can be lost on the embedded
    // Chromium: the page is clicked once it holds still.
    let reopen = || {
        session.open(links.clone());
        session.settle(Duration::from_millis(300));
    };
    let arrives = |which: &str| {
        session.until(&format!("the view to open what {which} asked for"), || session.runtime.snapshot().url == landing(which));
    };
    let stays = |what: &str| {
        thread::sleep(Duration::from_millis(700));
        assert_eq!(value(&session, "location.href"), links.as_str(), "{what}");
    };

    // The user's click on a link for another tab, on a button whose script
    // opens a window, on a link for a window with a name and on a form that
    // sends to another tab. Each opens here, and Back returns from it.
    reopen();
    for (which, y) in [("blank", 32.0), ("open", 72.0), ("named", 152.0), ("post", 232.0)] {
        click(y, left, 0);
        arrives(which);
        assert_eq!(session.runtime.snapshot().title, format!("Destino {which}"));
        let back = session.window(BrowserCommand::Back).unwrap();
        assert_eq!((back.url.as_str(), back.can_go_forward), (links.as_str(), true), "{which}");
        session.settle(Duration::from_millis(300));
    }
    // A link clicked for another tab or another window.
    for (button, modifiers) in [(left, CONTROL), (middle, 0), (left, SHIFT)] {
        click(112.0, button, modifiers);
        arrives("plain");
        reopen();
    }
    // The agent's click is the same click, and what it is answered is the
    // page that opened: its address, its title and its tree.
    for (target, which) in [(json!({"selector": "#blank"}), "blank"), (json!({"role": "button", "name": "Otra ventana"}), "open")] {
        let clicked = session.agent("browser_click", target);
        assert!(!clicked.is_error, "{:?}", clicked.content);
        let state = page(&clicked);
        assert_eq!((state["url"].as_str(), state["title"].as_str()), (Some(landing(which).as_str()), Some(format!("Destino {which}").as_str())));
        assert!(state["aria_snapshot"].as_str().unwrap().contains(&format!("Llegaste por {which}")), "{state}");
        assert_eq!(state["can_go_back"], true);
        reopen();
    }

    // What the page does by itself, with no click behind it, opens nothing:
    // a script's window, a click of its own on the link, one with Control.
    assert_eq!(value(&session, "String(window.open('/landing?script'))"), "null");
    value(&session, "document.querySelector('#blank').click(); 0");
    value(&session, "document.querySelector('#plain').dispatchEvent(new MouseEvent('click', {ctrlKey: true, bubbles: true})); 0");
    stays("a page opens nothing by itself");
    // Nor does a click open what is not an address of the web: the page of a
    // data: link, or a window with no address for a script to fill.
    click(192.0, left, 0);
    click(272.0, left, 0);
    stays("only an address of the web opens in the view");

    // Every address was asked for by the view that opened it. The embedded
    // Chromium never makes the other tab, so that is the only time.
    let landed = value(&session, "() => fetch('/landed').then(reply => reply.text())");
    let landed: Vec<&str> = landed.as_str().unwrap().split(',').collect();
    let asked = |what: &str| landed.iter().filter(|one| **one == what).count();
    assert_eq!(asked("GET script"), 0, "{landed:?}");
    for (what, times) in [("GET blank", 2), ("GET open", 2), ("GET named", 1), ("GET plain", 3)] {
        match engine {
            // The launched Chrome makes the tab, which asks for the address
            // too before the driver closes it.
            Chromium::Launched => assert!((times..=times * 2).contains(&asked(what)), "{what}: {landed:?}"),
            #[cfg(feature = "cef")]
            Chromium::Embedded => assert_eq!(asked(what), times, "{landed:?}"),
        }
    }
    // The view opens the address a form sends to, without what it sent.
    assert_eq!(asked("GET post"), 1, "{landed:?}");
    #[cfg(feature = "cef")]
    if engine == Chromium::Embedded {
        assert_eq!(asked("POST post"), 0, "{landed:?}");
    }
    assert!(session.runtime.snapshot().error.is_none());
    every_paint_left_the_canvas_as_chromium_painted_it();
}

#[test]
#[ignore = "opt-in: launches a real Chrome against local fixture pages"]
fn real_chrome_takes_what_a_spanish_keyboard_types_and_its_editing_keys() {
    what_a_spanish_keyboard_types(Chromium::Launched);
}

/// Text and keys as the window sends them: a character of the US layout is a
/// key that goes down and up, any other text is inserted, and editing keys
/// and shortcuts are chords by name.
fn what_a_spanish_keyboard_types(engine: Chromium) {
    let origin = site();
    let session = launch(engine, (640, 480), 2.0);
    session.open(format!("{origin}/form"));
    // A click sent the instant a load ends can be lost on the embedded
    // Chromium: the page is clicked once it holds still.
    session.settle(Duration::from_millis(300));
    let click = |x: f64, y: f64| {
        let (button, clicks, modifiers) = (MouseButton::Left, 1, 0);
        session.window(BrowserCommand::PointerDown { x, y, button, clicks, modifiers }).unwrap();
        session.window(BrowserCommand::PointerUp { x, y, button, clicks, modifiers }).unwrap();
    };
    let press = |key: &str| {
        session.window(BrowserCommand::Press { key: key.into() }).unwrap();
    };
    let insert = |text: &str| {
        session.window(BrowserCommand::Type { text: text.into() }).unwrap();
    };
    let copied = || session.window(BrowserCommand::CopySelection).unwrap().clipboard.unwrap().text;
    // The focused field: which, what it holds and where its selection is.
    let field = || value(&session, "(field => [field.id, field.value, field.selectionStart, field.selectionEnd])(document.activeElement)");

    // An <input>. Accents, ñ, ü, the opening marks and the euro sign, which
    // a Spanish keyboard types with dead keys and AltGr and the window gets
    // as text, like an emoji and what an input method commits.
    click(60.0, 150.0);
    insert("á é í ó ú ñ ü ¿ ¡ €");
    assert_eq!(field(), json!(["field", "á é í ó ú ñ ü ¿ ¡ €", 19, 19]));
    press("Control+a");
    insert("hola ñandú 🧠👍🏽");
    assert_eq!(field(), json!(["field", "hola ñandú 🧠👍🏽", 17, 17]));
    // Home, End, Delete, Backspace and the arrows.
    press("Home");
    press("Delete");
    assert_eq!(field(), json!(["field", "ola ñandú 🧠👍🏽", 0, 0]));
    press("End");
    press("Backspace");
    assert_eq!(field(), json!(["field", "ola ñandú 🧠", 12, 12]), "a whole emoji at once");
    press("ArrowLeft");
    press("ArrowLeft");
    insert("¿");
    assert_eq!(field(), json!(["field", "ola ñandú¿ 🧠", 10, 10]));
    // Shift with the arrows and with Home selects.
    for _ in 0..3 {
        press("Shift+ArrowLeft");
    }
    assert_eq!(field(), json!(["field", "ola ñandú¿ 🧠", 7, 10]));
    assert_eq!(copied(), "dú¿");
    press("Shift+Home");
    assert_eq!(field(), json!(["field", "ola ñandú¿ 🧠", 0, 10]));
    // Cut in its two steps, and paste, which arrives as text.
    let cut = session.window(BrowserCommand::PrepareCut).unwrap().clipboard.unwrap();
    assert_eq!(cut.text, "ola ñandú¿");
    session.window(BrowserCommand::ConfirmCut { token: cut.cut_token.unwrap() }).unwrap();
    assert_eq!(field(), json!(["field", " 🧠", 0, 0]));
    insert("pegado ");
    insert("€");
    assert_eq!(field(), json!(["field", "pegado € 🧠", 8, 8]));
    // Undo and redo, by both of its shortcuts.
    press("Control+z");
    let undone = field()[1].as_str().unwrap().to_owned();
    assert!(undone.len() < "pegado € 🧠".len() && undone.ends_with(" 🧠"), "{undone:?}");
    press("Control+Shift+z");
    assert_eq!(field()[1], "pegado € 🧠");
    press("Control+z");
    assert_eq!(field()[1], undone.as_str());
    press("Control+y");
    assert_eq!(field()[1], "pegado € 🧠");
    // A character of the layout is a key the page hears, and types itself.
    value(&session, "seen.length = 0");
    for key in ["a", "B", "?", " ", "+"] {
        press(key);
    }
    assert_eq!(field()[1], "pegado €aB? + 🧠");
    assert_eq!(value(&session, "seen.splice(0).join('|')"), "key a|key B|key ?|key  |key +");
    press("Control+a");
    assert_eq!(value(&session, "seen.splice(0).join('|')"), "key ctrl Control|key ctrl a", "a shortcut names its letter as it is typed");
    // Enter does not type into a field of one line, and Tab leaves it.
    press("Enter");
    assert_eq!(field()[1], "pegado €aB? + 🧠");
    press("Tab");

    // A <textarea>: Enter breaks the line.
    insert("línea ñ");
    press("Enter");
    insert("línea ü");
    assert_eq!(field(), json!(["area", "línea ñ\nlínea ü", 15, 15]));
    press("ArrowUp");
    press("Home");
    press("Shift+End");
    assert_eq!(field(), json!(["area", "línea ñ\nlínea ü", 0, 7]));
    assert_eq!(copied(), "línea ñ");
    press("Shift+ArrowDown");
    assert_eq!(field(), json!(["area", "línea ñ\nlínea ü", 0, 15]));
    // PageUp and PageDown move the caret by what the area shows.
    press("Control+End");
    for _ in 0..40 {
        press("Enter");
    }
    insert("fin");
    assert_eq!(field()[2], 58);
    press("PageUp");
    let up = field()[2].as_u64().unwrap();
    assert!(up < 58, "{up}");
    press("PageDown");
    assert_eq!(field()[2], 58);
    press("Shift+Tab");
    assert_eq!(field()[0], "field");

    // With no field focused, the same keys move the page.
    value(&session, "document.activeElement.blur(); 0");
    press("PageDown");
    session.until("PageDown to scroll the page", || value(&session, "scrollY").as_f64().unwrap() > 200.0);
    press(" ");
    session.until("Space to scroll it further", || value(&session, "scrollY").as_f64().unwrap() > 600.0);
    press("Home");
    session.until("Home to go back to its start", || value(&session, "scrollY") == 0);
}

// ---- The embedded Chromium ---------------------------------------------------

#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_serves_the_agent_and_an_external_client() {
    let origin = site();
    let session = launch(Chromium::Embedded, (900, 700), 2.0);
    let session = serves_the_agent_and_an_external_client(session, &origin);
    // What an embedder must refuse itself: dialogs nobody answers are
    // dismissed, the armed one is answered, and no other window opens.
    session.open(format!("{origin}/"));
    let value = |expression: &str| session.agent("browser_evaluate", json!({"expression": expression})).content[1]["text"].clone();
    assert_eq!(value("confirm('¿Seguir?')"), "false");
    assert!(!session.agent("browser_handle_dialog", json!({"accept": true, "prompt_text": "Ada 🧠"})).is_error);
    assert_eq!(value("prompt('Nombre')"), "\"Ada 🧠\"");
    assert_eq!(value("String(window.open('/'))"), "\"null\"");
    // Pages are told the languages of the system's locale, as Chrome tells them.
    let languages = cef::accept_languages(&cef::system_locale());
    assert_eq!(value("navigator.languages.join(',')"), format!("\"{languages}\""));
    let console = session.agent("browser_console_messages", json!({}));
    let logged = console.content[0]["text"].as_str().unwrap();
    assert!(logged.contains("\"action\":\"dismissed\"") && logged.contains("\"action\":\"accepted\""), "{logged}");
    // A frame of another site runs in a process of its own, behind a
    // session of its own: it is read and acted on by ref all the same.
    let other = origin.replace("127.0.0.1", "localhost");
    value(&format!("(() => {{ const frame = document.createElement('iframe'); frame.title = 'Otro sitio'; frame.src = '{other}/'; frame.style.cssText = 'position:absolute;left:20px;top:220px;width:600px;height:200px'; document.body.append(frame); }})()"));
    let mut tree = String::new();
    session.until("the frame of the other site", || {
        tree = page(&session.agent("browser_snapshot", json!({})))["aria_snapshot"].as_str().unwrap().to_owned();
        tree.matches("button \"Pintar\"").count() == 2
    });
    assert!(tree.contains("iframe \"Otro sitio\" [ref="), "{tree}");
    let buttons: Vec<String> = tree
        .lines()
        .filter(|line| line.contains("button \"Pintar\""))
        .map(|line| ref_of(line, "button \"Pintar\""))
        .collect();
    let (outer, inner) = (&buttons[0], &buttons[1]);
    let document = |reference: &str| reference.rsplit_once('e').unwrap().0.to_owned();
    assert_ne!(document(outer), document(inner), "the frame's document has refs of its own");
    assert!(!session.agent("browser_click", json!({"ref": inner})).is_error);
    let painted = session.agent("browser_evaluate", json!({"expression": "button => button.ownerDocument.body.style.background", "ref": inner}));
    assert_eq!(painted.content[1]["text"], "\"rgb(255, 221, 238)\"", "{:?}", painted.content);
    assert_eq!(value("document.body.style.background"), "\"\"", "only the frame's own document was clicked");
    // The page of a context closes with it; CEF itself stays for the next.
    drop(session.runtime);
}

#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_frames_reach_the_view_at_the_windows_scale_only_while_it_is_visible() {
    for scale in [2.0, 1.0] {
        frames_at(Chromium::Embedded, scale);
    }
}

#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_takes_the_windows_pointer_buttons_clicks_wheel_keys_and_clipboard() {
    takes_the_windows_input(Chromium::Embedded);
}

#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_opens_no_graphics_device_and_loads_no_graphics_driver() {
    let origin = site();
    let session = launch(Chromium::Embedded, (640, 480), 2.0);
    session.open(format!("{origin}/animated"));
    session.next_frame("a frame of the animation");
    // A page that asks for WebGL gets none, not a software stand-in.
    let webgl = session.agent("browser_evaluate", json!({"expression": "!!document.createElement('canvas').getContext('webgl')"}));
    assert_eq!(webgl.content[1]["text"], "false");
    // This process, where CEF's browser side runs, and every helper of it.
    let mut processes = descendants(std::process::id());
    assert!(processes.len() >= 5, "CEF runs its helpers below this process: {processes:?}");
    processes.push(std::process::id());
    let mut mapped = 0;
    for process in processes {
        for descriptor in fs::read_dir(format!("/proc/{process}/fd")).into_iter().flatten().flatten() {
            let target = fs::read_link(descriptor.path()).unwrap_or_default();
            let device = target.starts_with("/dev/dri") || target.to_string_lossy().starts_with("/dev/nvidia");
            assert!(!device, "process {process} holds {target:?}");
        }
        let Ok(maps) = fs::read_to_string(format!("/proc/{process}/maps")) else {
            continue;
        };
        mapped += 1;
        for line in maps.lines() {
            let library = line.rsplit('/').next().unwrap_or("");
            let driver = ["libEGL", "libGLX", "libGLESv2", "libGL.so", "_dri.so", "dri_gbm", "libgallium", "libnvidia", "libvulkan_", "libvk_swiftshader"]
                .iter()
                .any(|name| library.contains(name));
            assert!(!driver, "process {process} maps {library}");
        }
    }
    assert!(mapped >= 5, "the mappings of {mapped} processes could be read");
}

#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless"]
fn real_cef_leaves_the_signals_that_end_the_application_as_they_were() {
    let _session = launch_as(Chromium::Embedded, (640, 480), 1.0, false);
    // Chromium takes these for a browser's own exit, after which a signal
    // would end the application with status 0. A test harness catches none.
    let status = fs::read_to_string("/proc/self/status").unwrap();
    let caught = status.lines().find_map(|line| line.strip_prefix("SigCgt:")).unwrap().trim();
    let caught = u64::from_str_radix(caught, 16).unwrap();
    for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
        assert_eq!(caught >> (signal - 1) & 1, 0, "signal {signal} is caught: {caught:#x}");
    }
}

/// A site that counts how often `/hits` is asked for, and lets it be cached.
#[cfg(feature = "cef")]
fn counting_site() -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        let mut hits = 0;
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 2048];
            let read = stream.read(&mut request).unwrap_or(0);
            let (kind, cache, body) = if request[..read].starts_with(b"GET /hits") {
                hits += 1;
                ("text/plain", "max-age=3600", hits.to_string())
            } else {
                ("text/html; charset=utf-8", "no-store", "<!doctype html><title>Almacén</title><p>Almacén</p>".to_owned())
            };
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nCache-Control: {cache}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    origin
}

#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_contexts_share_no_cookies_storage_or_cache_and_keep_none_after_closing() {
    let origin = counting_site();
    let open = || {
        let session = launch_as(Chromium::Embedded, (640, 480), 1.0, false);
        session.open(format!("{origin}/"));
        session
    };
    let value = |session: &Session, expression: &str| -> String {
        let read = session.agent("browser_evaluate", json!({"expression": expression}));
        assert!(!read.is_error, "{expression}: {:?}", read.content);
        read.content[1]["text"].as_str().unwrap().to_owned()
    };
    const STORED: &str = "[document.cookie, localStorage.getItem('openide_isolation_key'), sessionStorage.getItem('clave')]";
    const HITS: &str = "() => fetch('/hits').then(reply => reply.text())";
    const DATABASES: &str = "() => indexedDB.databases().then(all => all.map(one => one.name))";
    const KEPT: &str = "[\"openide_isolation_cookie=secreta\",\"valor\",\"valor\"]";

    // Two contexts at once, as a restored window starts them: the first of
    // the process is the one that starts CEF.
    let (first, second) = thread::scope(|scope| {
        let (first, second) = (scope.spawn(open), scope.spawn(open));
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_eq!(value(&first, STORED), "[\"\",null,null]");
    value(&first, "document.cookie = 'openide_isolation_cookie=secreta; max-age=3600'; localStorage.setItem('openide_isolation_key', 'valor'); sessionStorage.setItem('clave', 'valor'); 0");
    assert_eq!(value(&first, "() => new Promise(done => { indexedDB.open('base').onsuccess = () => done(1); })"), "1");
    assert_eq!(value(&first, STORED), KEPT);
    assert_eq!(value(&first, DATABASES), "[\"base\"]");
    // The other context, on the same site at the same time, has none of it.
    assert_eq!(value(&second, STORED), "[\"\",null,null]");
    assert_eq!(value(&second, DATABASES), "[]");
    // Each has a cache of its own: what one fetched, the other asks the site for.
    assert_eq!((value(&first, HITS), value(&first, HITS)), ("\"1\"".to_owned(), "\"1\"".to_owned()));
    assert_eq!((value(&second, HITS), value(&second, HITS)), ("\"2\"".to_owned(), "\"2\"".to_owned()));
    // Within one context they last: a reload still has them.
    first.window(BrowserCommand::Reload).unwrap();
    assert_eq!(value(&first, STORED), KEPT);
    assert_eq!(value(&first, HITS), "\"1\"");

    // Closed, they are gone: a later context starts from nothing.
    drop(first);
    let third = open();
    assert_eq!(value(&third, STORED), "[\"\",null,null]");
    assert_eq!(value(&third, DATABASES), "[]");
    assert_eq!(value(&third, HITS), "\"3\"");
    assert_eq!(value(&second, STORED), "[\"\",null,null]", "the one that stayed open is as it was");
    // Nothing of theirs is on disk either: the only profile CEF keeps there
    // is its own, which no page uses.
    let directory = openide_cef::directory().expect("CEF runs in this process");
    let mut pending = vec![directory.clone()];
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(&folder).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(bytes) = fs::read(&path) {
                let holds = |needle: &[u8]| bytes.windows(needle.len()).any(|window| window == needle);
                assert!(
                    !holds(b"openide_isolation_cookie") && !holds(b"openide_isolation_key"),
                    "{path:?} holds what a page stored"
                );
            }
        }
    }
}

/// The two wheel measurements again, with CEF's own input call in place of
/// the driver's protocol event: which one the window should send.
#[cfg(feature = "cef")]
fn native_wheel(session: &Session) {
    let embedded = &session.runtime.handle.view.embedded;
    let scrolled = || session.agent("browser_evaluate", json!({"expression": "scrollY"})).content[1]["text"].clone();
    session.open(session.runtime.snapshot().url);
    session.settle(Duration::from_millis(600));
    // CEF counts a turn towards the user as negative.
    embedded.native_wheel(640, 400, -120);
    session.settle(Duration::from_millis(450));
    eprintln!("CEF's own wheel event of 120 at the centre of the view scrolls the page by {} CSS pixels", scrolled());
    embedded.native_wheel(640, 400, 120);
    session.settle(Duration::from_millis(450));

    let (mut moving, mut waiting) = (Vec::new(), Vec::<Instant>::new());
    let (mut direction, mut travelled) = (30, 0);
    let (from, started) = (session.frames.load(Ordering::Relaxed), Instant::now());
    let mut seen = from;
    while started.elapsed() < Duration::from_secs(4) {
        let tick = Instant::now();
        waiting.push(tick);
        embedded.native_wheel(640, 400, -direction);
        travelled += direction;
        if !(0..1200).contains(&travelled) {
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
    eprintln!("CEF's own wheel event every 16 ms: {scrolling:.1} frames per second");
    eprintln!("CEF's own wheel event to the next frame while scrolling: {}", summary(moving));
    let mut resting = Vec::new();
    for step in 0..20 {
        session.settle(Duration::from_millis(450));
        let seen = session.frames.load(Ordering::Relaxed);
        let sent = Instant::now();
        embedded.native_wheel(640, 400, if step % 2 == 0 { -120 } else { 120 });
        while session.frames.load(Ordering::Relaxed) == seen && sent.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_micros(100));
        }
        resting.push(sent.elapsed());
    }
    eprintln!("CEF's own wheel event to the first frame on a page at rest: {}", summary(resting));
}

/// The same measurements on the embedded Chromium. Run it alone, with the
/// optimised profile and nothing compiling; the first context of the process
/// is the one that starts CEF:
///
/// ```sh
/// nix-shell nix/shell-cef.nix --run \
///   "cargo test --profile desktop -p agents-window --features cef -- --ignored --nocapture real_cef_measurements"
/// ```
#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in timing: starts the embedded Chromium and prints measurements"]
fn real_cef_measurements_of_the_live_view_and_of_what_the_browser_adds() {
    measurements(Chromium::Embedded);
}

/// What a change costs by how much of the view it is: the processor this
/// process and Chromium's spend on a blinking caret, a small spinner, a
/// continuous scroll and an animation of the whole view, at 2560 × 1600,
/// composited 60 and 120 times a second. Run like the measurements above.
#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in timing: starts the embedded Chromium and prints measurements"]
fn real_cef_measurements_of_what_a_change_costs_by_how_much_of_the_view_it_is() {
    let origin = site();
    let own = std::process::id();
    // The tests compare every paint with the canvas. The application does not.
    cef::COMPARE_PAINTS.store(false, Ordering::Relaxed);
    let session = launch(Chromium::Embedded, (1280, 800), 2.0);
    // The window takes every frame it is told of. Here it is taken and dropped.
    let (taken, view) = (session.frames.clone(), Arc::downgrade(&session.runtime.handle.view));
    session.runtime.on_frame(move || {
        taken.fetch_add(1, Ordering::Relaxed);
        if let Some(view) = view.upgrade() {
            view.decoded.lock().unwrap().clear();
        }
    });
    let load = || fs::read_to_string("/proc/loadavg").unwrap_or_default().trim().to_owned();
    eprintln!("load average before: {}", load());
    let measure = |what: &str, rate: u32, during: &dyn Fn(Instant)| {
        let busy = || (processor_seconds(&[own]), processor_seconds(&session.browser_processes()));
        let (from, started, used) = (session.frames.load(Ordering::Relaxed), Instant::now(), busy());
        during(started);
        let (elapsed, now) = (started.elapsed().as_secs_f64(), busy());
        eprintln!(
            "{what}, composited {rate} times a second: {:.1} frames per second to the view; this process {:.1} % of one core, the browser's processes {:.1} %",
            (session.frames.load(Ordering::Relaxed) - from) as f64 / elapsed,
            (now.0 - used.0) / elapsed * 100.0,
            (now.1 - used.1) / elapsed * 100.0
        );
    };
    for rate in [60, 120] {
        cef::set_display_rate(rate);
        for (what, path) in [("a blinking caret", "/form"), ("a small spinner", "/animated"), ("an animation of the whole view", "/full")] {
            session.open(format!("{origin}{path}"));
            if path == "/form" {
                thread::sleep(Duration::from_millis(300));
                assert!(!session.agent("browser_click", json!({"selector": "#field"})).is_error);
            }
            thread::sleep(Duration::from_secs(1));
            measure(what, rate, &|_| thread::sleep(Duration::from_secs(4)));
        }
        // A wheel step with every frame, one in flight at a time, as the
        // window sends them.
        session.open(format!("{origin}/"));
        session.settle(Duration::from_millis(600));
        session.window(BrowserCommand::PointerMove { x: 640.0, y: 400.0, modifiers: 0 }).unwrap();
        measure("a continuous scroll of a page of text", rate, &|started| {
            let (mut direction, mut travelled) = (30.0, 0.0);
            while started.elapsed() < Duration::from_secs(4) {
                let tick = Instant::now();
                let mut request = session.runtime.submit_ui(BrowserCommand::Scroll { dx: 0.0, dy: direction, modifiers: 0 }).unwrap();
                while request.poll().is_none() {
                    assert!(tick.elapsed() < Duration::from_secs(10), "the wheel was never answered");
                    thread::sleep(Duration::from_micros(100));
                }
                travelled += direction;
                if !(0.0..1200.0).contains(&travelled) {
                    direction = -direction;
                }
                thread::sleep((Duration::from_secs(1) / rate).saturating_sub(tick.elapsed()));
            }
        });
    }
    cef::set_display_rate(60);
    cef::COMPARE_PAINTS.store(true, Ordering::Relaxed);
    eprintln!("load average after: {}", load());
}

#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_draws_the_list_of_a_select_and_takes_keys_the_pointer_and_the_wheel_on_it() {
    for scale in [2.0, 1.0] {
        the_list_of_a_select(Chromium::Embedded, scale);
    }
}

#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_takes_modifier_keys_with_the_pointer_and_the_wheel() {
    modifier_keys_with_the_pointer_and_the_wheel(Chromium::Embedded);
}

#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_takes_what_a_spanish_keyboard_types_and_its_editing_keys() {
    what_a_spanish_keyboard_types(Chromium::Embedded);
}

#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_opens_in_the_view_what_a_click_asked_to_see_in_another_tab() {
    what_a_click_asked_to_see_in_another_tab(Chromium::Embedded);
}

#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_parts_of_frames_leave_the_view_pixel_for_pixel_what_a_whole_frame_would() {
    for scale in [2.0, 1.0, 1.5] {
        parts_of_frames_leave_the_view_what_a_whole_frame_would(scale);
    }
}

/// The view is handed the parts of the page that changed, one after another.
/// After each thing a person or the agent does, its texture is pixel for
/// pixel the page as Chromium last painted it, and at the end what a view
/// that had none of those parts is handed whole.
#[cfg(feature = "cef")]
fn parts_of_frames_leave_the_view_what_a_whole_frame_would(scale: f32) {
    let origin = site();
    let session = launch(Chromium::Embedded, (640, 480), scale);
    let embedded = &session.runtime.handle.view.embedded;
    let mut texture = Texture::new();
    // Takes what the view is handed until its texture is the page as it was
    // last painted, with what is painted over it.
    let agree = |texture: &mut Texture, what: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            while texture.take(&session).is_some() {}
            let (size, rgb) = embedded.whole();
            let differing = if size == texture.image.size {
                let page = rgb.chunks_exact(3);
                texture.image.pixels.iter().zip(page).filter(|(pixel, page)| [pixel.r(), pixel.g(), pixel.b()] != **page).count()
            } else {
                usize::MAX
            };
            if differing == 0 && size != [0, 0] {
                return;
            }
            assert!(Instant::now() < deadline, "{what}, at scale {scale}: {differing} pixels of the view are not the page's");
            thread::sleep(Duration::from_millis(5));
        }
    };
    // The parts the view is handed for a while, by the pixels of each.
    let watch = |texture: &mut Texture, time: Duration| -> Vec<usize> {
        let (started, mut parts) = (Instant::now(), Vec::new());
        while started.elapsed() < time {
            parts.extend(texture.take(&session).filter(|pixels| *pixels > 0));
            thread::sleep(Duration::from_millis(1));
        }
        parts
    };
    let click = |x: f64, y: f64| {
        let (button, clicks, modifiers) = (MouseButton::Left, 1, 0);
        session.window(BrowserCommand::PointerDown { x, y, button, clicks, modifiers }).unwrap();
        session.window(BrowserCommand::PointerUp { x, y, button, clicks, modifiers }).unwrap();
    };
    let insert = |text: &str| {
        session.window(BrowserCommand::Type { text: text.into() }).unwrap();
    };
    // Hides the view, with all it was handed until then in its texture.
    let hide = |texture: &mut Texture| {
        session.runtime.set_visible(false);
        session.until("the hidden view to let go of the page", || embedded.whole().0 == [0, 0]);
        // A part that was being built when the view hid is handed over too.
        thread::sleep(Duration::from_millis(100));
        while texture.take(&session).is_some() {}
    };
    let device = |width: f32, height: f32| [(width * scale) as usize, (height * scale) as usize];

    session.open(format!("{origin}/form"));
    session.settle(Duration::from_millis(300));
    agree(&mut texture, "the page once it loaded");
    assert_eq!(texture.image.size, device(640.0, 480.0));

    // Typing in a field, and the caret that blinks in it afterwards: each
    // blink is a part of a few pixels, not a frame.
    click(60.0, 150.0);
    insert("hola ñandú");
    session.settle(Duration::from_millis(250));
    agree(&mut texture, "what was typed");
    let (blinks, frame) = (watch(&mut texture, Duration::from_millis(1800)), texture.image.pixels.len());
    assert!(blinks.len() >= 2, "the caret blinked {} times", blinks.len());
    assert!(blinks.iter().all(|pixels| pixels * 50 < frame), "each blink of the caret is a small part of {frame} pixels: {blinks:?}");
    agree(&mut texture, "the caret");

    // The page scrolled under the pointer, there and back.
    session.window(BrowserCommand::PointerMove { x: 400.0, y: 300.0, modifiers: 0 }).unwrap();
    for dy in [300.0, 250.0, -120.0, -430.0] {
        session.window(BrowserCommand::Scroll { dx: 0.0, dy, modifiers: 0 }).unwrap();
        session.settle(Duration::from_millis(250));
        agree(&mut texture, "the page scrolled");
    }

    // A list that opens over the page, the row under the pointer, and the
    // page that shows again where the list was.
    click(100.0, 35.0);
    session.until("the list to open", || embedded.popup_box().is_some());
    agree(&mut texture, "the list open over the page");
    session.window(BrowserCommand::PointerMove { x: 100.0, y: 50.0 + 26.5 * 3.5, modifiers: 0 }).unwrap();
    session.settle(Duration::from_millis(250));
    agree(&mut texture, "the row under the pointer");
    session.window(BrowserCommand::Press { key: "Escape".into() }).unwrap();
    session.until("the list to close", || embedded.popup_box().is_none());
    agree(&mut texture, "the page where the list was");

    // An agent capture in the middle of what changes, whose own paints are
    // held back, and one of a page that did not change.
    click(60.0, 150.0);
    insert(" uno");
    assert!(!session.agent("browser_take_screenshot", json!({"full_page": true})).is_error);
    insert(" dos");
    agree(&mut texture, "what changed around an agent capture");
    assert!(!session.agent("browser_take_screenshot", json!({})).is_error);
    agree(&mut texture, "the page after a capture that changed nothing");

    // Hidden and shown again, the view is handed a whole frame.
    hide(&mut texture);
    session.runtime.set_visible(true);
    let mut shown = None;
    session.until("a frame of the view shown again", || {
        shown = texture.take(&session);
        shown.is_some()
    });
    assert_eq!(shown, Some(0), "shown again, the view is handed a whole frame");
    agree(&mut texture, "the view shown again");

    // A page of another size, and what changes in it.
    session.window(BrowserCommand::Resize { width: 800, height: 600 }).unwrap();
    session.until("a frame at the new size", || {
        texture.take(&session);
        texture.image.size == device(800.0, 600.0)
    });
    agree(&mut texture, "the page at its new size");
    insert(" tres");
    agree(&mut texture, "what was typed after the resize");

    // A corner that animates, part after part, and where it stopped.
    session.open(format!("{origin}/animated"));
    let turns = watch(&mut texture, Duration::from_millis(900));
    assert!(turns.len() >= 5, "{} parts of the animation", turns.len());
    value(&session, "document.getAnimations().forEach(animation => animation.pause()); 0");
    agree(&mut texture, "the animation where it stopped");

    // The same page as a view that had none of those parts gets it: whole,
    // from Chromium's own pixels.
    session.settle(Duration::from_millis(300));
    hide(&mut texture);
    let held = &texture.image;
    session.runtime.set_visible(true);
    let mut fresh = Texture::new();
    let mut whole = None;
    session.until("a whole frame of the same page", || {
        whole = fresh.take(&session);
        whole.is_some()
    });
    assert_eq!(whole, Some(0));
    session.settle(Duration::from_millis(300));
    while fresh.take(&session).is_some() {}
    assert_eq!(fresh.image.size, held.size);
    let differing = fresh.image.pixels.iter().zip(&held.pixels).filter(|(fresh, held)| fresh != held).count();
    assert_eq!(differing, 0, "pixels of what the view held, part by part, that a whole frame of the page has otherwise, at scale {scale}");
    every_paint_left_the_canvas_as_chromium_painted_it();
}

#[cfg(feature = "cef")]
#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_says_which_pointer_the_page_asks_for_under_the_windows() {
    use eframe::egui::CursorIcon;
    let origin = site();
    let session = launch(Chromium::Embedded, (640, 480), 2.0);
    session.open(format!("{origin}/form"));
    // A click sent the instant a load ends can be lost on the embedded
    // Chromium: the page is clicked once it holds still.
    session.settle(Duration::from_millis(300));
    for (what, x, y, icon) in [
        ("a link", 320.0, 30.0, CursorIcon::PointingHand),
        ("a line of text", 60.0, 95.0, CursorIcon::Text),
        ("what is not allowed", 450.0, 95.0, CursorIcon::NotAllowed),
        ("a column divider", 450.0, 155.0, CursorIcon::ResizeColumn),
        ("a field", 60.0, 150.0, CursorIcon::Text),
        ("the page itself", 600.0, 440.0, CursorIcon::Default),
    ] {
        session.window(BrowserCommand::PointerMove { x, y, modifiers: 0 }).unwrap();
        session.until(&format!("the pointer of {what}"), || session.runtime.cursor() == Some(icon));
    }
}
