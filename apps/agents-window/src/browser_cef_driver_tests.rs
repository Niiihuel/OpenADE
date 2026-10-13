//! The driver against the embedded Chromium, without the runtime around it:
//! each scenario opens a page of its own and drives it through the API of
//! `openide_browser::Driver`. Opt-in, and run with the scenarios of
//! `browser_cef_tests.rs`:
//!
//! ```sh
//! nix-shell --run "cargo test -p agents-window -- --ignored real_cef --test-threads=1"
//! ```
//!
//! Chromium opens no window and never touches the GPU, and only ever visits
//! the loopback server each test starts.

use super::*;
use openide_browser::{
    ConsoleEntry, Evaluated, NetworkEntry, Png, Report, MAX_ARIA_BYTES, MAX_CONSOLE_TEXT_BYTES,
    MAX_EVALUATE_BYTES, MAX_EVENTS, MAX_INPUT_TEXT_BYTES, MAX_REPORT_BYTES, MAX_REQUEST_URL_BYTES,
    MAX_SCREENSHOT_BYTES,
};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;

/// The pages the driver's recorded snapshots were taken from, and the
/// recordings its unit tests reproduce them with.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/openide-browser/tests/fixtures");
const RECORDED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/openide-browser/tests/recorded");
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// A page of the embedded Chromium with a driver on it, as a context has
/// them. The view only keeps the page open: nothing takes its frames.
fn open(width: u32, height: u32, scale: f32) -> (Arc<LiveView>, Driver) {
    let view = Arc::<LiveView>::default();
    let driver = cef::Embedded::start(&view, &std::env::temp_dir(), (width, height), scale)
        .unwrap_or_else(|failure| panic!("{}", failure.message));
    // Composited as often as a page a command is running on.
    view.embedded.busy(true);
    (view, driver)
}

struct Response {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl Response {
    fn html(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type", "text/html; charset=utf-8".into())],
            body: body.into(),
        }
    }

    fn status(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: vec![("Content-Type", "text/plain; charset=utf-8".into())],
            body: body.as_bytes().to_vec(),
        }
    }

    fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.retain(|(existing, _)| *existing != name);
        self.headers.push((name, value.to_owned()));
        self
    }
}

/// One of the driver's fixture pages.
fn fixture(name: &str) -> Option<Response> {
    let name = name.trim_start_matches('/');
    if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
        return None;
    }
    std::fs::read(Path::new(FIXTURES).join(name)).ok().map(Response::html)
}

/// An HTTP server on a free loopback port, alive until dropped. Only this
/// machine can reach it and it serves only what the test hands it.
struct Server {
    origin: String,
    port: u16,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// `route` gets the method and the path with its query.
    fn start(route: impl Fn(&str, &str) -> Response + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let route = Arc::new(route);
        let stopping = stop.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let route = route.clone();
                thread::spawn(move || {
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
    fn fixtures() -> Self {
        Self::start(|_, path| {
            let path = path.split('?').next().unwrap_or("");
            if path.starts_with("/api/") {
                return Response::status(200, "{}").header("Content-Type", "application/json");
            }
            fixture(path).unwrap_or_else(|| Response::status(404, "no"))
        })
    }

    /// The same server under another host name: a different site for Chromium.
    fn origin_as(&self, host: &str) -> String {
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

fn snapshot(driver: &mut Driver) -> String {
    let snapshot = driver.snapshot(&SnapshotOptions::default()).unwrap();
    assert!(snapshot.text.len() <= MAX_ARIA_BYTES);
    snapshot.text
}

/// The ref of the first line that contains `label`.
fn ref_of(snapshot: &str, label: &str) -> String {
    let line = snapshot
        .lines()
        .find(|line| line.contains(label) && line.contains("[ref="))
        .unwrap_or_else(|| panic!("{label} carries a ref in:\n{snapshot}"));
    let start = line.find("[ref=").unwrap() + 5;
    line[start..start + line[start..].find(']').unwrap()].to_owned()
}

fn viewport(driver: &mut Driver, width: u32, height: u32) -> Png {
    let png = driver.screenshot(Capture::Viewport).unwrap();
    assert!(png.data.len() < MAX_SCREENSHOT_BYTES);
    assert!(png.data.starts_with(PNG_SIGNATURE));
    assert_eq!((png.width, png.height), (width, height));
    assert_eq!(decoded_size(&png.data), (width, height));
    png
}

/// The size a PNG decodes to.
fn decoded_size(png: &[u8]) -> (u32, u32) {
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png).unwrap();
    (image.width(), image.height())
}

fn tool_error<T: std::fmt::Debug>(result: Result<T, DriverError>, pattern: &str) -> String {
    let error = result.expect_err(pattern);
    assert_eq!(error.kind, ErrorKind::Tool, "{error}");
    assert!(error.message.contains(pattern), "{:?} lacks {pattern:?}", error.message);
    error.message
}

fn evaluate(driver: &mut Driver, expression: &str) -> String {
    driver.evaluate(expression, None).unwrap().text
}

const INPUT_PAGE: &str = r#"<!doctype html><html lang="es"><head><title>Fixture OpenIDE</title><style>
  body{font:18px sans-serif;margin:20px;background:linear-gradient(#fff,#bcd);min-height:2200px}
  #panel{height:130px}input,button,a{font:inherit;margin:4px}footer{position:absolute;top:1900px}
  </style></head><body><div id="panel"><label>Nombre <input id="name" aria-label="Nombre"></label>
  <button id="apply" onclick="document.querySelector('#value').textContent=document.querySelector('#name').value">Aplicar</button>
  <p id="value">Sin cambios</p><a id="next" href="/second">Segunda página</a></div>
  <textarea id="memo" aria-label="Notas">Texto de notas</textarea>
  <input id="readonly" aria-label="Sólo lectura" readonly value="Conservar lectura">
  <input id="password" type="password" value="no copiar">
  <div id="rich" contenteditable="true">Texto enriquecido</div>
  <p id="selection">Texto seleccionable 🧠</p>
  <button id="select" onclick="let r=document.createRange();r.selectNodeContents(document.querySelector('#selection'));getSelection().removeAllRanges();getSelection().addRange(r)">Seleccionar párrafo</button>
  <footer>Final de página</footer><script>addEventListener('scroll',()=>document.title='Scroll '+Math.round(scrollY))</script></body></html>"#;

fn input_server() -> Server {
    Server::start(|_, path| match path {
        "/second" => Response::html(
            "<!doctype html><title>Segunda página</title><h1>Segunda página</h1><a href=\"/\">Inicio</a>",
        ),
        _ => Response::html(INPUT_PAGE),
    })
}

#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_driver_navigates_takes_input_copies_cuts_scrolls_and_keeps_history() {
    let server = input_server();
    let origin = server.origin.clone();
    let (_page, mut driver) = open(640, 480, 1.0);
    // CEF's files are in a directory of this user alone, short enough for
    // the sockets Chromium makes below it.
    let directory = openide_cef::directory().expect("CEF runs in this process");
    assert!(directory.file_name().unwrap().to_str().unwrap().starts_with("oid-"));
    assert!(directory.as_os_str().len() <= 48, "{directory:?}");
    assert_eq!(std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777, 0o700);

    let info = driver.info().unwrap();
    assert_eq!((info.url.as_str(), info.can_go_back, info.can_go_forward), ("about:blank", false, false));
    assert_eq!((info.width, info.height, info.scale), (640, 480, 1.0));
    viewport(&mut driver, 640, 480);

    driver.navigate(&origin).unwrap();
    assert_eq!(driver.info().unwrap().title, "Fixture OpenIDE");
    viewport(&mut driver, 640, 480);
    driver.click_at(200.0, 35.0).unwrap();
    driver.type_text("Niño 🧠").unwrap();
    driver.click(&Target::Selector("#apply")).unwrap();
    assert!(snapshot(&mut driver).contains("Niño 🧠"));
    driver.fill(&Target::Role { role: "textbox", name: "Nombre" }, "Otro").unwrap();
    driver.press("Control+A").unwrap();
    driver.type_text("Reemplazo á").unwrap();
    driver.click(&Target::Role { role: "button", name: "Aplicar" }).unwrap();
    assert!(snapshot(&mut driver).contains("paragraph [ref=e"), "the page is the first document");
    assert!(snapshot(&mut driver).contains(": Reemplazo á"));

    // Copy, and the two-phase cut.
    driver.click(&Target::Selector("#name")).unwrap();
    driver.press("Control+A").unwrap();
    assert_eq!(driver.copy_selection().unwrap(), "Reemplazo á");
    let (text, token) = driver.prepare_cut().unwrap();
    assert_eq!(text, "Reemplazo á");
    assert!(token.len() >= 32);
    assert_eq!(driver.copy_selection().unwrap(), "Reemplazo á", "preparing a cut alone deletes nothing");
    // Copying invalidated that preparation.
    assert_eq!(driver.confirm_cut(&token).unwrap_err().kind, ErrorKind::Failed);
    assert_eq!(driver.copy_selection().unwrap(), "Reemplazo á");
    let (_, token) = driver.prepare_cut().unwrap();
    driver.confirm_cut(&token).unwrap();
    assert!(driver.copy_selection().is_err(), "the cut field is now empty");
    assert_eq!(evaluate(&mut driver, "document.querySelector('#name').value"), "\"\"");
    assert!(driver.confirm_cut(&token).is_err(), "a token works once");

    driver.fill(&Target::Selector("#memo"), "Notas á 🧠").unwrap();
    driver.press("Control+A").unwrap();
    let (text, token) = driver.prepare_cut().unwrap();
    assert_eq!(text, "Notas á 🧠");
    driver.fill(&Target::Selector("#memo"), "Texto cambiado").unwrap();
    let changed = driver.confirm_cut(&token).unwrap_err();
    assert!(changed.message.contains("cambiaron"), "a changed field is never cut: {changed}");
    driver.press("Control+A").unwrap();
    assert_eq!(driver.copy_selection().unwrap(), "Texto cambiado");

    driver.click(&Target::Selector("#readonly")).unwrap();
    driver.press("Control+A").unwrap();
    assert_eq!(driver.copy_selection().unwrap(), "Conservar lectura");
    assert!(driver.prepare_cut().unwrap_err().message.contains("editables"));
    driver.click(&Target::Selector("#password")).unwrap();
    driver.press("Control+A").unwrap();
    assert!(driver.copy_selection().unwrap_err().message.contains("contraseña"), "passwords never leave the page");
    assert!(driver.prepare_cut().unwrap_err().message.contains("contraseña"));
    driver.click(&Target::Selector("#rich")).unwrap();
    driver.press("Control+A").unwrap();
    assert!(driver.prepare_cut().is_err(), "a contenteditable cut is not emulated");
    driver.click(&Target::Selector("#select")).unwrap();
    assert_eq!(driver.copy_selection().unwrap(), "Texto seleccionable 🧠");
    // One byte above the limit.
    evaluate(&mut driver, "() => { document.querySelector('#name').value = 'x'.repeat(16 * 1024 + 1); }");
    driver.click(&Target::Selector("#name")).unwrap();
    driver.press("Control+A").unwrap();
    assert!(driver.prepare_cut().unwrap_err().message.contains("16 KiB"));
    assert!(driver.copy_selection().unwrap_err().message.contains("16 KiB"));
    driver.fill(&Target::Selector("#name"), "Reemplazo á").unwrap();
    driver.type_text("!").unwrap();
    assert_eq!(evaluate(&mut driver, "document.querySelector('#name').value"), "\"Reemplazo á!\"");

    // Scrolling, then history.
    driver.scroll(0.0, 800.0).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !driver.info().unwrap().title.starts_with("Scroll ") && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(30));
    }
    let title = driver.info().unwrap().title;
    assert!(title.starts_with("Scroll ") && title != "Scroll 0", "{title}");
    driver.click(&Target::Selector("#next")).unwrap();
    let info = driver.info().unwrap();
    assert_eq!((info.url, info.can_go_back), (format!("{origin}/second"), true));
    driver.back().unwrap();
    let info = driver.info().unwrap();
    assert_eq!((info.url, info.can_go_forward), (format!("{origin}/"), true));
    driver.forward().unwrap();
    assert_eq!(driver.info().unwrap().url, format!("{origin}/second"));
    driver.forward().unwrap();
    assert_eq!(driver.info().unwrap().url, format!("{origin}/second"), "nowhere further to go is not an error");
    driver.reload().unwrap();
    assert_eq!(driver.info().unwrap().title, "Segunda página");
    driver.resize(800, 600).unwrap();
    viewport(&mut driver, 800, 600);
    assert_eq!(driver.resize(100, 100).unwrap_err().kind, ErrorKind::Invalid);

    // Refused and failed navigations leave the page and the driver as they were.
    for refused in ["file:///etc/passwd", "javascript:alert(1)", "http://user:secret@127.0.0.1/", "chrome://version", "data:text/html,x"] {
        let error = driver.navigate(refused).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Invalid, "{refused}");
        assert!(error.message.contains("HTTP"), "{error}");
    }
    assert_eq!(driver.info().unwrap().url, format!("{origin}/second"));
    let unsafe_port = driver.navigate("http://127.0.0.1:1/").unwrap_err();
    assert_eq!((unsafe_port.kind, unsafe_port.message.as_str()), (ErrorKind::Navigation, "net::ERR_UNSAFE_PORT at http://127.0.0.1:1/"));
    // A port nothing listens on any more.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let failed = driver.navigate(&format!("http://127.0.0.1:{closed}/")).unwrap_err();
    assert_eq!(failed.kind, ErrorKind::Navigation);
    assert!(failed.message.starts_with("net::ERR_CONNECTION_REFUSED at http://127.0.0.1:"), "{failed}");
    assert_eq!(driver.info().unwrap().url, "chrome-error://chromewebdata/", "as the page itself reports it");
    // History and reload of a page that no longer loads fail the same readable way.
    let reload = driver.reload().unwrap_err();
    assert_eq!(reload.kind, ErrorKind::Navigation);
    assert!(reload.message.starts_with("net::ERR_CONNECTION_REFUSED at http://127.0.0.1:"), "{reload}");
    driver.navigate(&origin).unwrap();
    assert_eq!(driver.info().unwrap().title, "Fixture OpenIDE");
    viewport(&mut driver, 800, 600);
}

// ---- Agent tools: refs, explicit actions, page observers and bounded output ----

fn tools_server() -> Server {
    Server::start(|_, path| {
        let (path, _) = path.split_once('?').unwrap_or((path, ""));
        match path {
            "/redirect" => Response::status(302, "").header("Location", "/tools"),
            "/missing.js" => Response::status(404, "no"),
            "/api/items" => Response::status(200, "{\"secret\":\"respuesta secreta\"}")
                .header("Content-Type", "application/json")
                .header("X-Reply-Secret", "cabecera de respuesta"),
            "/frame" | "/frame.html" => fixture("frame.html").unwrap(),
            "/tall" => Response::html("<!doctype html><title>Alta</title><body style=\"margin:0\"><div style=\"height:1500px;background:linear-gradient(#fff,#369)\">Arriba</div><p>Fondo</p>"),
            // Noise does not compress: the full page is far above the capture limit.
            "/noise" => Response::html("<!doctype html><title>Ruido</title><body style=\"margin:0\"><canvas width=\"640\" height=\"2400\"></canvas><script>\
                const context=document.querySelector('canvas').getContext('2d'),image=context.createImageData(640,2400);\
                for(let i=0;i<image.data.length;i++)image.data[i]=i%4===3?255:Math.random()*256;context.putImageData(image,0,0)</script>"),
            "/quiet" => Response::html("<!doctype html><title>Silencio</title><h1>Silencio</h1>"),
            "/named" => Response::html("<!doctype html><title>Nombres</title>\
                <div><button onclick=\"out.textContent='guardó'\">  Guardar   cambios </button><button onclick=\"out.textContent='otro'\">Guardar</button></div>\
                <label>Correo <input id=\"mail\"></label><input id=\"busca\" placeholder=\"Buscar…\"><input aria-label=\"Teléfono\" id=\"phone\">\
                <button data-testid=\"enviar\" onclick=\"out.textContent='envió'\">OK</button><span>Repetido</span><span>Repetido</span>\
                <a href=\"/quiet?llegada=1\">Ir a silencio</a><p id=\"out\">nada</p>"),
            "/copy" => Response::html("<!doctype html><title>Copiar</title>\
                <button id=\"api\" onclick=\"navigator.clipboard.writeText('por la API').then(() => out.textContent = 'api ok', error => out.textContent = 'api no: ' + error.name)\">API</button>\
                <button id=\"old\" onclick=\"const area = document.createElement('textarea'); area.value = 'a la antigua'; document.body.append(area); area.select(); out.textContent = 'viejo ' + document.execCommand('copy'); area.remove()\">Viejo</button>\
                <p id=\"out\">nada</p>"),
            "/upload" => Response::html("<!doctype html><title>Subida</title><input id=\"one\" type=\"file\"><input id=\"many\" type=\"file\" multiple><p id=\"got\">nada</p>\
                <script>for (const input of document.querySelectorAll('input')) input.onchange = async () => { got.textContent = (await Promise.all([...input.files].map(async file => file.name + '=' + await file.text()))).join(); }</script>"),
            _ => fixture("tools.html").unwrap(),
        }
    })
}

#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_driver_acts_by_ref_waits_and_evaluates() {
    let server = tools_server();
    let origin = server.origin.clone();
    let (_page, mut driver) = open(640, 480, 1.0);
    driver.navigate(&format!("{origin}/tools")).unwrap();
    let mut tree = snapshot(&mut driver);
    assert!(tree.contains("- button \"Efímero\" [ref=e"), "{tree}");
    let heading = ref_of(&tree, "heading \"Herramientas\"");
    let erase: Vec<&str> = tree.lines().filter(|line| line.contains("button \"Borrar\" [ref=e")).collect();
    assert_eq!(erase.len(), 2, "both buttons have their own ref");
    let second = ref_of(erase[1], "Borrar");
    assert_ne!(ref_of(erase[0], "Borrar"), second);

    // A ref names exactly one of two elements that share role and name.
    driver.click(&Target::Ref(&second)).unwrap();
    tree = snapshot(&mut driver);
    assert!(tree.contains("B pulsado"));
    assert_eq!(ref_of(&tree, "heading \"Herramientas\""), heading, "an element keeps its ref across snapshots");
    let ambiguous = tool_error(driver.click(&Target::Role { role: "button", name: "Borrar" }), "coincide con 2 elementos");
    assert!(ambiguous.contains("browser_snapshot"));
    tool_error(driver.click(&Target::Selector("#absent")), "coincide con 0 elementos");
    tool_error(driver.click(&Target::Selector("p[")), "Selector no válido");
    assert_eq!(evaluate(&mut driver, "document.querySelector(\"#out\").textContent"), "\"B pulsado\"", "a refused target changed nothing");

    // The user's own input takes no snapshot and leaves the agent's refs usable.
    driver.click_at(600.0, 440.0).unwrap();
    driver.fill(&Target::Ref(&ref_of(&tree, "textbox \"Correo\"")), "ada@example.test").unwrap();
    let value = driver.evaluate("el => el.value", Some(&Target::Selector("input[aria-label=Correo]"))).unwrap();
    assert_eq!(value.text, "\"ada@example.test\"");

    // A removed element's ref goes stale; so do refs that never existed.
    let temporary = ref_of(&tree, "button \"Efímero\"");
    driver.click(&Target::Ref(&temporary)).unwrap();
    tree = snapshot(&mut driver);
    assert!(!tree.contains("Efímero"));
    let stale = tool_error(driver.click(&Target::Ref(&temporary)), &format!("La referencia {temporary} no corresponde"));
    assert!(stale.contains("Tomá una captura nueva con browser_snapshot"));
    tool_error(driver.fill(&Target::Ref("e99999"), "x"), "La referencia e99999 no corresponde");
    tool_error(driver.hover(&Target::Ref("f9e1")), "La referencia f9e1 no corresponde");
    tool_error(driver.drag(&Target::Ref(&heading), &Target::Ref("e99999")), "La referencia e99999 no corresponde");
    tool_error(driver.evaluate("el => { window.touched = true; }", Some(&Target::Ref(&temporary))), "no corresponde");
    assert_eq!(evaluate(&mut driver, "window.touched"), "undefined", "the script of a stale target never ran");
    assert_eq!(driver.click(&Target::Ref("e1; drop")).unwrap_err().kind, ErrorKind::Invalid);

    // Refs reach inside an iframe.
    let inner = ref_of(&tree, "button \"Dentro\"");
    assert!(inner.starts_with('f') && inner.contains('e'), "{inner}");
    driver.click(&Target::Ref(&inner)).unwrap();
    tree = snapshot(&mut driver);
    assert!(tree.contains("Dentro pulsado"));
    let inside = driver.evaluate("el => el.ownerDocument.title + '/' + el.textContent", Some(&Target::Ref(&ref_of(&tree, "Dentro pulsado")))).unwrap();
    assert_eq!(inside.text, "\"Marco/Dentro pulsado\"", "a script receives an element of another frame");

    // Explicit actions, each by a different way of naming its element.
    driver.hover(&Target::Ref(&ref_of(&tree, "button \"Menú\""))).unwrap();
    assert!(snapshot(&mut driver).contains("Sobre el menú"));
    driver.select_options(&Target::Role { role: "combobox", name: "Color" }, &["Verde"]).unwrap();
    assert!(snapshot(&mut driver).contains("Color g"));
    driver.select_options(&Target::Role { role: "combobox", name: "Color" }, &["r"]).unwrap();
    assert!(snapshot(&mut driver).contains("Color r"), "an option is chosen by its value too");
    driver.set_checked(&Target::Selector("#terms"), true).unwrap();
    tree = snapshot(&mut driver);
    assert!(tree.contains("Acepto true"));
    assert!(tree.contains("checkbox \"Acepto\" [checked]"), "{tree}");
    driver.set_checked(&Target::Selector("#terms"), true).unwrap();
    assert!(snapshot(&mut driver).contains("Acepto true"), "nothing happens when it already is");
    driver.set_checked(&Target::Ref(&ref_of(&tree, "checkbox \"Acepto\"")), false).unwrap();
    assert!(snapshot(&mut driver).contains("Acepto false"));
    tool_error(driver.set_checked(&Target::Selector("#menu"), true), "Not a checkbox");
    driver.drag(&Target::Selector("#drag"), &Target::Selector("#drop")).unwrap();
    assert!(snapshot(&mut driver).contains("Soltada carta"));

    // Waiting: text that arrives, text already gone, a fixed pause, and a wait that runs out.
    driver.click(&Target::Selector("#later")).unwrap();
    driver.wait_for_text("Listo tarde").unwrap();
    assert!(snapshot(&mut driver).contains("Listo tarde"));
    driver.wait_for_text("listo   TARDE").unwrap();
    driver.wait_for_text_gone("Soltada nunca").unwrap();
    driver.wait_for_text_gone("Texto oculto").unwrap();
    let mut started = Instant::now();
    driver.wait_seconds(0.3).unwrap();
    assert!(started.elapsed() >= Duration::from_millis(290));
    assert_eq!(driver.wait_seconds(10.5).unwrap_err().kind, ErrorKind::Invalid);
    started = Instant::now();
    tool_error(driver.wait_for_text("Texto oculto"), "no apareció después de 10 s");
    assert!(started.elapsed() >= Duration::from_secs(9), "hidden text does not count as visible");
    assert!(started.elapsed() < Duration::from_secs(12));

    // Evaluate: values, functions, an element, errors as text and a bounded result.
    assert_eq!(driver.evaluate("document.title", None).unwrap(), Evaluated { text: "\"Herramientas\"".into(), truncated: false });
    assert_eq!(evaluate(&mut driver, "() => ({ n: 1, list: [1, \"dos\"] })"), "{\"n\":1,\"list\":[1,\"dos\"]}");
    assert_eq!(evaluate(&mut driver, "async () => { await new Promise(done => setTimeout(done, 20)); return 7; }"), "7");
    assert_eq!(evaluate(&mut driver, "function () { return [1.5, null, true]; }"), "[1.5,null,true]");
    assert_eq!(evaluate(&mut driver, "1 + 1"), "2");
    assert_eq!(evaluate(&mut driver, "10n ** 20n"), "100000000000000000000");
    assert_eq!(evaluate(&mut driver, "0 / 0"), "null");
    assert_eq!(evaluate(&mut driver, "null"), "null");
    assert_eq!(evaluate(&mut driver, "() => ({ big: 1n })"), "[object Object]", "a value without JSON is its text");
    assert_eq!(driver.evaluate("el => el.textContent", Some(&Target::Ref(&heading))).unwrap().text, "\"Herramientas\"");
    assert_eq!(driver.evaluate("el => el.id", Some(&Target::Role { role: "button", name: "Confirmar" })).unwrap().text, "\"confirm\"");
    assert_eq!(evaluate(&mut driver, "document.querySelector(\"#out\").click()"), "undefined");
    assert_eq!(evaluate(&mut driver, "typeof process + \"/\" + typeof require"), "\"undefined/undefined\"");
    assert_eq!(tool_error(driver.evaluate("(() => { throw new Error(\"explota\"); })()", None), "explota"), "Error: explota");
    assert_eq!(tool_error(driver.evaluate("noExiste + 1", None), "noExiste"), "ReferenceError: noExiste is not defined");
    assert!(tool_error(driver.evaluate("function (", None), "SyntaxError").starts_with("SyntaxError"));
    assert_eq!(tool_error(driver.evaluate("() => { throw \"texto\"; }", None), "texto"), "texto");
    assert_eq!(tool_error(driver.evaluate("(() => { throw 7; })()", None), "7"), "7");
    assert_eq!(tool_error(driver.evaluate("async () => { throw new TypeError(\"tarde\"); }", None), "tarde"), "TypeError: tarde");
    let oversized = driver.evaluate("() => \"é\".repeat(60000)", None).unwrap();
    assert!(oversized.truncated);
    assert!(oversized.text.len() <= MAX_EVALUATE_BYTES && oversized.text.len() > MAX_EVALUATE_BYTES - 4);
    assert!(!oversized.text.contains('\u{fffd}'), "the cut never splits a character");
    let split = driver.evaluate("() => \"x\".repeat(16382) + \"🧠🧠\"", None).unwrap();
    assert!(split.truncated && split.text.len() <= MAX_EVALUATE_BYTES && !split.text.contains('\u{fffd}'));
    let nested = |depth: usize| format!("() => {{ let value = 1; for (let level = 0; level < {depth}; level++) value = [value]; return value; }}");
    assert_eq!(evaluate(&mut driver, &nested(60)), format!("{}1{}", "[".repeat(60), "]".repeat(60)));
    assert_eq!(evaluate(&mut driver, &nested(500)).len(), 1001, "nested page data travels as text, never as a tree");
    assert_eq!(driver.evaluate("  ", None).unwrap_err().kind, ErrorKind::Invalid);
    assert_eq!(driver.evaluate(&"x".repeat(MAX_EVALUATE_BYTES + 1), None).unwrap_err().kind, ErrorKind::Invalid);
    started = Instant::now();
    tool_error(driver.evaluate("new Promise(() => {})", None), "El script no terminó en 10 s");
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(driver.info().unwrap().title.contains("Herramientas"), "the page is still served after an abandoned script");
    assert_eq!(evaluate(&mut driver, "1 + 1"), "2");
    assert_eq!(evaluate(&mut driver, "document.querySelector(\"#out\").textContent.length > 0"), "true", "the abandoned promise stopped nothing else");
    // A script that never yields would hold the page for good: it is stopped, and only it.
    evaluate(&mut driver, "() => { window.ticks = 0; setInterval(() => window.ticks++, 20); }");
    started = Instant::now();
    tool_error(driver.evaluate("(() => { for (;;) {} })()", None), "El script no terminó en 10 s");
    assert!(started.elapsed() < Duration::from_secs(13), "{:?}", started.elapsed());
    assert_eq!(evaluate(&mut driver, "1 + 1"), "2", "the page answers again");
    driver.wait_seconds(0.3).unwrap();
    assert_eq!(evaluate(&mut driver, "window.ticks > 3"), "true", "the page's own timers go on");
    driver.click(&Target::Selector("#a")).unwrap();
    assert!(snapshot(&mut driver).contains("A pulsado"));
}

#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_driver_gives_a_file_chooser_the_files_armed_for_it_and_leaves_it_closed_otherwise() {
    let server = tools_server();
    let (_page, mut driver) = open(640, 480, 1.0);
    driver.navigate(&format!("{}/upload", server.origin)).unwrap();
    let folder = tempfile::tempdir().unwrap();
    let file = |name: &str, text: &str| {
        let path = folder.path().join(name);
        std::fs::write(&path, text).unwrap();
        path.to_str().unwrap().to_owned()
    };
    let (first, second) = (file("uno.txt", "hola"), file("dos.txt", "chau"));
    let choosers = |driver: &mut Driver| -> Vec<&'static str> {
        let log = driver.console_messages(ConsoleLevel::Warning);
        log.entries.iter().filter(|entry| entry.kind == "filechooser").filter_map(|entry| entry.action).collect()
    };

    // With nothing armed the chooser stays closed and the page gets no file.
    driver.click(&Target::Selector("#one")).unwrap();
    driver.wait_seconds(0.3).unwrap();
    assert_eq!(evaluate(&mut driver, "got.textContent"), "\"nada\"");
    assert_eq!(choosers(&mut driver), ["dismissed"]);

    // Armed for the next chooser only.
    driver.upload_files(&[first.clone()]).unwrap();
    driver.click(&Target::Selector("#one")).unwrap();
    driver.wait_seconds(0.3).unwrap();
    assert_eq!(evaluate(&mut driver, "got.textContent"), "\"uno.txt=hola\"");
    driver.click(&Target::Selector("#one")).unwrap();
    driver.wait_seconds(0.3).unwrap();
    assert_eq!(choosers(&mut driver), ["dismissed", "accepted", "dismissed"]);

    // Several files only where the input takes several.
    driver.upload_files(&[first.clone(), second.clone()]).unwrap();
    driver.click(&Target::Selector("#one")).unwrap();
    driver.wait_seconds(0.3).unwrap();
    assert_eq!(choosers(&mut driver).last(), Some(&"dismissed"));
    driver.upload_files(&[first.clone(), second]).unwrap();
    driver.click(&Target::Selector("#many")).unwrap();
    driver.wait_seconds(0.3).unwrap();
    assert_eq!(evaluate(&mut driver, "got.textContent"), "\"uno.txt=hola,dos.txt=chau\"");

    // Only files that exist, by absolute path.
    for bad in ["uno.txt".to_owned(), folder.path().join("falta.txt").to_str().unwrap().to_owned(), folder.path().to_str().unwrap().to_owned()] {
        assert_eq!(driver.upload_files(&[bad]).unwrap_err().kind, ErrorKind::Invalid);
    }
    assert_eq!(driver.upload_files(&[]).unwrap_err().kind, ErrorKind::Invalid);
}

#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_driver_names_elements_by_what_they_show_and_waits_for_an_address_and_a_load() {
    let server = tools_server();
    let (_page, mut driver) = open(640, 480, 1.0);
    driver.navigate(&format!("{}/named", server.origin)).unwrap();
    driver.wait_for_load(true).unwrap();

    // The whole text of the smallest element, whatever its case and spacing.
    driver.click(&Target::Selector("text=guardar cambios")).unwrap();
    assert_eq!(evaluate(&mut driver, "out.textContent"), "\"guardó\"");
    driver.click(&Target::Selector("text=Guardar")).unwrap();
    assert_eq!(evaluate(&mut driver, "out.textContent"), "\"otro\"");
    // A label, an aria-label, a placeholder and a test id.
    driver.fill(&Target::Selector("label=Correo"), "ana@ejemplo.com").unwrap();
    driver.fill(&Target::Selector("label=teléfono"), "123").unwrap();
    driver.fill(&Target::Selector("placeholder=Buscar…"), "limn").unwrap();
    assert_eq!(evaluate(&mut driver, "[mail.value, phone.value, busca.value].join()"), "\"ana@ejemplo.com,123,limn\"");
    driver.click(&Target::Selector("testid=enviar")).unwrap();
    assert_eq!(evaluate(&mut driver, "out.textContent"), "\"envió\"");
    // It still has to be exactly one element.
    tool_error(driver.click(&Target::Selector("text=Repetido")), "coincide con 2 elementos");
    tool_error(driver.click(&Target::Selector("text=No está")), "coincide con 0 elementos");
    tool_error(driver.click(&Target::Selector("testid=")), "coincide con 0 elementos");

    // An address the page reaches, and one it never does.
    driver.click(&Target::Selector("text=Ir a silencio")).unwrap();
    driver.wait_for_url("/quiet?llegada=1").unwrap();
    driver.wait_for_load(false).unwrap();
    assert_eq!(evaluate(&mut driver, "document.title"), "\"Silencio\"");
    assert_eq!(driver.wait_for_url("").unwrap_err().kind, ErrorKind::Invalid);
}

#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_driver_takes_what_a_page_copies_behind_a_click_and_nothing_else() {
    let server = tools_server();
    let (_page, mut driver) = open(640, 480, 1.0);
    driver.navigate(&format!("{}/copy", server.origin)).unwrap();
    assert_eq!(driver.take_copied(), None);

    // The page's Copy button works, through the clipboard API and the old command.
    driver.click(&Target::Selector("#api")).unwrap();
    driver.wait_for_text("api ok").unwrap();
    assert_eq!(driver.take_copied().as_deref(), Some("por la API"));
    assert_eq!(driver.take_copied(), None, "taken once");
    driver.click(&Target::Selector("#old")).unwrap();
    driver.wait_for_text("viejo true").unwrap();
    assert_eq!(driver.take_copied().as_deref(), Some("a la antigua"));

    // The page has no way of its own to hand text over, and cannot read any back.
    assert_eq!(evaluate(&mut driver, "typeof window.__openide_copy"), "\"undefined\"");
    assert_eq!(
        evaluate(&mut driver, "() => navigator.clipboard.readText().then(() => 'leyó', error => error.name)"),
        "\"NotAllowedError\""
    );
    // After a navigation the stand-in is there again.
    driver.navigate(&format!("{}/copy?otra", server.origin)).unwrap();
    driver.click(&Target::Selector("#api")).unwrap();
    driver.wait_for_text("api ok").unwrap();
    assert_eq!(driver.take_copied().as_deref(), Some("por la API"));
}

#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_driver_keeps_console_network_dialogs_and_screenshots_within_their_bounds() {
    let server = tools_server();
    let origin = server.origin.clone();
    let (_page, mut driver) = open(640, 480, 1.0);
    driver.navigate(&format!("{origin}/tools")).unwrap();
    driver.wait_seconds(0.5).unwrap();

    // Console: levels, uncaught errors and where each message came from.
    let log = driver.console_messages(ConsoleLevel::Info);
    assert_eq!(log.omitted, 0);
    let has = |log: &Report<ConsoleEntry>, check: &dyn Fn(&ConsoleEntry) -> bool| log.entries.iter().any(check);
    let location = format!("{origin}/tools:20:19");
    assert!(
        has(&log, &|entry| entry.level == ConsoleLevel::Info && entry.kind == "log" && entry.text == "carga {n: 1}" && entry.location.as_deref() == Some(&location)),
        "{:?}",
        log.entries
    );
    assert!(has(&log, &|entry| entry.level == ConsoleLevel::Warning && entry.text == "aviso"));
    assert!(has(&log, &|entry| entry.level == ConsoleLevel::Error && entry.kind == "error" && entry.text == "falla"));
    assert!(has(&log, &|entry| entry.level == ConsoleLevel::Error && entry.kind == "pageerror" && entry.text.starts_with("Error: sin capturar\n")));
    let missing = format!("{origin}/missing.js");
    assert!(has(&log, &|entry| entry.level == ConsoleLevel::Error && entry.text.contains("404") && entry.location.as_deref().is_some_and(|at| at.starts_with(&missing))));
    assert!(!has(&log, &|entry| entry.text == "depura"), "debug is below the default level");
    assert!(has(&driver.console_messages(ConsoleLevel::Debug), &|entry| entry.level == ConsoleLevel::Debug && entry.text == "depura"));
    let errors = driver.console_messages(ConsoleLevel::Error);
    assert!(errors.entries.len() >= 3 && errors.entries.iter().all(|entry| entry.level == ConsoleLevel::Error));

    // Network: method, URL, status, type and failure, and nothing else.
    evaluate(&mut driver, "fetch(\"http://127.0.0.1:1/caido\").catch(() => \"sin conexión\")");
    evaluate(&mut driver, "fetch(\"/api/items?q=\" + \"z\".repeat(2000)).then(reply => reply.status)");
    let network = driver.network_requests();
    let find = |network: &Report<NetworkEntry>, needle: &str| network.entries.iter().find(|entry| entry.url.contains(needle)).cloned().unwrap_or_else(|| panic!("{needle} in {:?}", network.entries));
    let entry = |method: &str, url: String, kind: &str, status: Option<u16>| serde_json::json!({"method": method, "url": url, "resource_type": kind, "status": status});
    assert_eq!(serde_json::to_value(&network.entries[0]).unwrap(), entry("GET", format!("{origin}/tools"), "document", Some(200)));
    assert_eq!(
        serde_json::to_value(find(&network, "token=visible")).unwrap(),
        entry("POST", format!("{origin}/api/items?token=visible"), "fetch", Some(200)),
        "URLs are returned as they are"
    );
    assert_eq!(find(&network, "missing.js").status, Some(404));
    assert_eq!(find(&network, "/frame").resource_type, "document");
    let refused = find(&network, "caido");
    assert!(refused.failure.as_deref().is_some_and(|failure| failure.starts_with("net::ERR_")) && refused.status.is_none());
    assert_eq!(serde_json::to_value(&refused).unwrap().as_object().unwrap().len(), 4, "an absent status is not serialised");
    assert_eq!(find(&network, "q=zzz").url.len(), MAX_REQUEST_URL_BYTES, "a long URL is cut at its bound");
    let serialised = serde_json::to_string(&network).unwrap();
    for secret in ["cuerpo secreto", "cabecera secreta", "respuesta secreta", "cabecera de respuesta", "X-Secret", "request_id"] {
        assert!(!serialised.contains(secret), "{secret}");
    }

    // Both logs are bounded: the newest entries that fit, and a count of the rest.
    evaluate(&mut driver, "() => { for (let i = 0; i < 450; i++) console.log(`linea ${i} ` + \"y\".repeat(180)); console.log(\"ñ\".repeat(4000)); }");
    let log = driver.console_messages(ConsoleLevel::Info);
    assert!(log.omitted > 250, "omitted {}", log.omitted);
    assert!(log.entries.len() > 20 && log.entries.len() < MAX_EVENTS);
    assert!(serde_json::to_string(&log.entries).unwrap().len() <= MAX_REPORT_BYTES + 2);
    let newest = log.entries.last().unwrap();
    assert!(newest.text.len() <= MAX_CONSOLE_TEXT_BYTES && newest.text.chars().all(|character| character == 'ñ') && newest.text.len() > 1000);
    assert!(log.entries[log.entries.len() - 2].text.starts_with("linea 449 "));
    evaluate(&mut driver, "() => Promise.all(Array.from({ length: 260 }, (_, i) => fetch(\"/api/items?n=\" + i)))");
    let network = driver.network_requests();
    assert!(network.omitted > 100, "omitted {}", network.omitted);
    assert!(serde_json::to_string(&network.entries).unwrap().len() <= MAX_REPORT_BYTES + 2);
    assert!(network.entries.iter().all(|entry| entry.status == Some(200)));

    // Dialogs are dismissed by default and each one is recorded.
    let dialogs = |driver: &mut Driver| -> Vec<serde_json::Value> {
        let log = driver.console_messages(ConsoleLevel::Warning);
        log.entries.iter().filter(|entry| entry.kind == "dialog").map(|entry| serde_json::to_value(entry).unwrap()).collect()
    };
    driver.click(&Target::Selector("#confirm")).unwrap();
    assert!(snapshot(&mut driver).contains("Confirmó false"));
    assert_eq!(
        dialogs(&mut driver).last().unwrap(),
        &serde_json::json!({"level": "warning", "type": "dialog", "dialog": "confirm", "action": "dismissed", "text": "¿Borrar todo?"})
    );
    // Armed for the next dialog only.
    driver.handle_dialog(true, Some("Ada 🧠")).unwrap();
    driver.click(&Target::Selector("#prompt")).unwrap();
    assert!(snapshot(&mut driver).contains("Respondió Ada 🧠"));
    assert_eq!(
        dialogs(&mut driver).last().unwrap(),
        &serde_json::json!({"level": "warning", "type": "dialog", "dialog": "prompt", "action": "accepted", "text": "¿Nombre?"})
    );
    driver.click(&Target::Selector("#confirm")).unwrap();
    assert!(snapshot(&mut driver).contains("Confirmó false"), "the answer was used once");
    driver.handle_dialog(true, None).unwrap();
    driver.click(&Target::Selector("#confirm")).unwrap();
    assert!(snapshot(&mut driver).contains("Confirmó true"));
    assert_eq!(driver.handle_dialog(true, Some(&"x".repeat(4097))).unwrap_err().kind, ErrorKind::Invalid);

    // A navigation starts both logs over and disarms an unused answer; its redirect does not.
    driver.handle_dialog(true, None).unwrap();
    driver.navigate(&format!("{origin}/redirect")).unwrap();
    driver.wait_seconds(0.5).unwrap();
    let network = driver.network_requests();
    let first: Vec<_> = network.entries.iter().take(2).map(|entry| (entry.url.clone(), entry.status)).collect();
    assert_eq!(first, [(format!("{origin}/redirect"), Some(302)), (format!("{origin}/tools"), Some(200))]);
    assert_eq!(network.omitted, 0);
    assert!(!network.entries.iter().any(|entry| entry.url.contains("caido") || entry.url.contains("?n=")));
    let log = driver.console_messages(ConsoleLevel::Debug);
    assert!(!log.entries.iter().any(|entry| entry.text.starts_with("linea") || entry.kind == "dialog"));
    assert!(log.entries.iter().any(|entry| entry.text.starts_with("carga")));
    driver.click(&Target::Selector("#confirm")).unwrap();
    assert!(snapshot(&mut driver).contains("Confirmó false"), "an answer armed for the previous page is forgotten");
    driver.navigate(&format!("{origin}/quiet")).unwrap();
    let quiet = driver.console_messages(ConsoleLevel::Debug);
    assert_eq!((quiet.omitted, quiet.entries.len()), (0, 0));

    // Screenshots: viewport, element and full page as PNG at CSS scale.
    driver.navigate(&format!("{origin}/tall")).unwrap();
    viewport(&mut driver, 640, 480);
    let page = driver.screenshot(Capture::FullPage).unwrap();
    assert!(page.data.starts_with(PNG_SIGNATURE) && page.data.len() <= MAX_SCREENSHOT_BYTES);
    assert_eq!(page.width, 640);
    assert!(page.height >= 1500, "full page height {}", page.height);
    let tree = snapshot(&mut driver);
    let paragraph = ref_of(&tree, "paragraph");
    let element = driver.screenshot(Capture::Element(Target::Ref(&paragraph))).unwrap();
    assert!(element.width <= 640 && element.height > 0 && element.height < 100, "element {}x{}", element.width, element.height);
    assert_eq!(decoded_size(&element.data), (element.width, element.height));
    if let Some(directory) = std::env::var_os("OPENIDE_BROWSER_E2E_ARTIFACTS") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(Path::new(&directory).join("driver-full-page.png"), &page.data).unwrap();
        std::fs::write(Path::new(&directory).join("driver-element.png"), &element.data).unwrap();
    }
    tool_error(driver.screenshot(Capture::Element(Target::Ref("e99999"))), "La referencia e99999 no corresponde");
    // An element that cannot be captured is this tool's answer, not a page error.
    evaluate(&mut driver, "() => { document.querySelector(\"p\").style.display = \"none\"; }");
    let started = Instant::now();
    let hidden = tool_error(driver.screenshot(Capture::Element(Target::Selector("p"))), "Timeout 10000ms exceeded");
    assert!(!hidden.contains('\n') && started.elapsed() >= Duration::from_secs(9));
    // Above the limit the capture is refused with a way forward, and the page stays usable.
    driver.navigate(&format!("{origin}/noise")).unwrap();
    let refused = tool_error(driver.screenshot(Capture::FullPage), "supera el límite de 2 MiB; capturá el viewport o un elemento");
    assert!(refused.contains("KiB"), "{refused}");
    viewport(&mut driver, 640, 480);
}

// ---- Snapshot shapes and options ----

/// The fixture pages whose snapshot is kept under `tests/recorded`, with the
/// protocol replies it was built from.
const RECORDED_PAGES: [&str; 6] = ["tools", "form", "lists", "table", "frames", "custom"];

/// Chromium's snapshot of each fixture is the text the driver's unit suite
/// reproduces from its recording. `OPENIDE_BROWSER_RECORD=1` rewrites the
/// recordings instead, for a new Chromium or a changed fixture.
#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_driver_snapshots_of_the_fixtures_are_what_their_recordings_reproduce() {
    let server = Server::fixtures();
    let record = std::env::var_os("OPENIDE_BROWSER_RECORD").is_some();
    let directory = Path::new(RECORDED);
    for page in RECORDED_PAGES {
        // A browser of its own, so the refs are the ones a first page gets.
        let (_page, mut driver) = open(900, 700, 1.0);
        driver.navigate(&format!("{}/{page}.html", server.origin)).unwrap();
        // Frames and late scripts settle.
        driver.wait_seconds(0.5).unwrap();
        // The port of the fixture server differs on every run.
        let portable = |text: String| text.replace(&server.origin, "http://fixture");
        let text = portable(snapshot(&mut driver)) + "\n";
        let expected = directory.join(format!("{page}.yaml"));
        if record {
            let inputs = portable(driver.snapshot_recording().unwrap());
            std::fs::write(directory.join(format!("{page}.json")), inputs + "\n").unwrap();
            std::fs::write(&expected, &text).unwrap();
        } else {
            assert_eq!(text, std::fs::read_to_string(&expected).unwrap(), "{page}");
        }
    }
}

#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_driver_snapshots_cover_frames_custom_controls_and_can_be_narrowed() {
    let server = Server::fixtures();
    let origin = server.origin.clone();
    let (_page, mut driver) = open(900, 700, 1.0);

    driver.navigate(&format!("{origin}/custom.html")).unwrap();
    let tree = snapshot(&mut driver);
    // A clickable element without a role is pointed out and can be acted on.
    assert!(tree.contains("- generic [ref=") && tree.contains("[cursor=pointer]: Tarjeta uno"), "{tree}");
    driver.click(&Target::Ref(&ref_of(&tree, "Tarjeta uno"))).unwrap();
    assert!(snapshot(&mut driver).contains("paragraph [ref=") && evaluate(&mut driver, "out.textContent") == "\"Tarjeta uno\"");
    driver.click(&Target::Ref(&ref_of(&tree, "Texto con clic sin cursor"))).unwrap();
    assert_eq!(evaluate(&mut driver, "out.textContent"), "\"Texto con clic\"");
    assert!(tree.contains("- generic: Tarjeta inerte"), "what takes no pointer events gets no ref: {tree}");
    assert!(!tree.contains("Invisible") && !tree.contains("aria-hidden"));
    // Checkboxes whose real input is hidden in three different ways.
    for (selector, expected) in [("#c1", "Avisos true"), ("#c2", "Oculto true"), ("#c3", "Cero true")] {
        driver.set_checked(&Target::Selector(selector), true).unwrap();
        assert_eq!(evaluate(&mut driver, "out.textContent"), format!("\"{expected}\""), "{selector}");
        assert_eq!(evaluate(&mut driver, &format!("document.querySelector('{selector}').checked")), "true");
    }
    driver.set_checked(&Target::Role { role: "checkbox", name: "Avisos" }, false).unwrap();
    assert_eq!(evaluate(&mut driver, "out.textContent"), "\"Avisos false\"");
    let tree = snapshot(&mut driver);
    assert!(tree.contains("checkbox \"Opacidad cero\" [checked]"), "{tree}");

    driver.navigate(&format!("{origin}/frames.html")).unwrap();
    driver.wait_for_text("Después de los marcos").unwrap();
    driver.wait_seconds(0.3).unwrap();
    let tree = snapshot(&mut driver);
    assert_eq!(tree.matches("button \"Dentro\" [ref=f").count(), 2, "both visible frames with the button, not the hidden one:\n{tree}");
    assert!(tree.contains("heading \"Marco intermedio\" [level=2] [ref=f"));
    assert!(tree.contains("textbox \"Dentro de srcdoc\" [ref=f"));
    // A ref inside a frame inside a frame is clicked where it is on screen.
    let innermost = ref_of(tree.split("Marco intermedio").nth(1).unwrap(), "button \"Dentro\"");
    driver.click(&Target::Ref(&innermost)).unwrap();
    let tree = snapshot(&mut driver);
    assert_eq!(tree.matches("Dentro pulsado").count(), 1, "{tree}");
    assert!(tree.split("Marco intermedio").nth(1).unwrap().contains("Dentro pulsado"));
    driver.fill(&Target::Ref(&ref_of(&tree, "Dentro de srcdoc")), "en el marco").unwrap();
    assert!(snapshot(&mut driver).contains("textbox \"Dentro de srcdoc\" [active] [ref=f") && snapshot(&mut driver).contains(": en el marco"));

    // Options that make the tree fit a small budget. The defaults keep it whole.
    driver.navigate(&format!("{origin}/form.html")).unwrap();
    let whole = snapshot(&mut driver);
    let interactive = driver.snapshot(&SnapshotOptions { interactive_only: true, ..SnapshotOptions::default() }).unwrap().text;
    assert!(interactive.len() < whole.len() * 3 / 4, "{} of {}", interactive.len(), whole.len());
    assert!(!interactive.contains("heading") && !interactive.contains("- text:") && !interactive.contains("generic"), "{interactive}");
    for kept in ["form \"Cliente\"", "group \"Plan\"", "textbox \"Correo\"", ": ada@example.test", "radio \"Gratis\" [checked]", "button \"Guardar\"", "link \"Ayuda\""] {
        assert!(interactive.contains(kept), "{kept} in:\n{interactive}");
    }
    assert_eq!(ref_of(&interactive, "button \"Guardar\""), ref_of(&whole, "button \"Guardar\""), "the same element, the same ref");
    let shallow = driver.snapshot(&SnapshotOptions { max_depth: Some(1), ..SnapshotOptions::default() }).unwrap().text;
    assert_eq!(shallow.lines().map(|line| line.len() - line.trim_start().len()).max(), Some(2), "{shallow}");
    assert!(shallow.contains("- main [ref=") && shallow.contains("  - form \"Cliente\" [ref=") && !shallow.contains("textbox"));
    let subtree = driver.snapshot(&SnapshotOptions { selector: Some("fieldset".into()), ..SnapshotOptions::default() }).unwrap().text;
    assert!(subtree.starts_with("- group \"Plan\" [ref="), "{subtree}");
    assert_eq!(subtree.matches("radio").count(), 3);
    assert!(!subtree.contains("textbox"));
    tool_error(driver.snapshot(&SnapshotOptions { selector: Some("input".into()), ..SnapshotOptions::default() }), "coincide con");
    // A narrowed snapshot leaves the refs of the rest of the page usable.
    driver.click(&Target::Ref(&ref_of(&whole, "button \"Guardar\""))).unwrap();

    // A page far larger than the bound is cut at a whole line and says so.
    evaluate(&mut driver, "() => { document.body.innerHTML = Array.from({ length: 3000 }, (_, i) => `<button>Botón número ${i} con un nombre largo</button>`).join(''); }");
    let large = driver.snapshot(&SnapshotOptions::default()).unwrap();
    assert!(large.truncated && large.text.len() <= MAX_ARIA_BYTES && large.text.len() > MAX_ARIA_BYTES - 200);
    assert!(large.text.lines().last().unwrap().ends_with(']'), "the last line is whole");
}

#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_driver_reads_and_acts_on_a_frame_of_another_site_and_opens_another_window_in_the_page() {
    let server = Server::fixtures();
    // The same loopback server under another host name is another site, which
    // Chromium renders in another process.
    let outer = format!(
        "<!doctype html><title>Sitios</title><h1>Dos sitios</h1><iframe src=\"{}/frame.html\" title=\"Otro sitio\" style=\"width:300px;height:80px;margin-left:120px\"></iframe>\
         <button onclick=\"window.open('/form.html')\">Abrir ventana</button>",
        server.origin_as("localhost")
    );
    let pages = Server::start(move |_, _| Response::html(outer.clone()));
    let (_page, mut driver) = open(640, 480, 1.0);
    driver.navigate(&pages.origin).unwrap();
    driver.wait_seconds(0.5).unwrap();
    // One panel has one page. A window the page opens by itself, with no
    // click behind it, is refused before it exists.
    assert_eq!(evaluate(&mut driver, "String(window.open('/form.html'))"), "\"null\"");
    driver.wait_seconds(0.3).unwrap();
    assert_eq!(driver.info().unwrap().url, format!("{}/", pages.origin), "the page did not move by itself");
    let tree = snapshot(&mut driver);
    assert!(tree.contains("iframe \"Otro sitio\" [ref=e"), "{tree}");
    let inner = ref_of(&tree, "button \"Dentro\"");
    driver.hover(&Target::Ref(&inner)).unwrap();
    driver.click(&Target::Ref(&inner)).unwrap();
    let tree = snapshot(&mut driver);
    assert!(tree.contains("button \"Dentro pulsado\" [active] [ref=f"), "{tree}");
    let document = driver.evaluate("el => location.host", Some(&Target::Ref(&ref_of(&tree, "Dentro pulsado")))).unwrap();
    assert!(document.text.starts_with("\"localhost:"), "{}", document.text);
    assert!(driver.network_requests().entries.iter().any(|entry| entry.url.contains("localhost") && entry.status == Some(200)));

    // A window a click asked for opens in the page itself: the click's own
    // answer is already that document.
    driver.click(&Target::Role { role: "button", name: "Abrir ventana" }).unwrap();
    assert_eq!(driver.info().unwrap().url, format!("{}/form.html", pages.origin));
    assert!(snapshot(&mut driver).contains("Dos sitios"), "the document that opened is read like any other");
}

// ---- Raw input, as the window sends it ----

#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_driver_raw_pointer_and_keyboard_input_reach_the_page_as_the_users_own() {
    let server = tools_server();
    let (_page, mut driver) = open(640, 480, 2.0);
    driver.navigate(&format!("{}/tools", server.origin)).unwrap();
    let centre = |driver: &mut Driver, selector: &str| -> (f64, f64) {
        let rect = evaluate(driver, &format!("(() => {{ const r = document.querySelector('{selector}').getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; }})()"));
        let numbers: Vec<f64> = serde_json::from_str(&rect).unwrap();
        (numbers[0], numbers[1])
    };

    // Hover.
    let menu = centre(&mut driver, "#menu");
    driver.pointer_move(menu.0, menu.1).unwrap();
    assert_eq!(evaluate(&mut driver, "out.textContent"), "\"Sobre el menú\"");

    // A click made of its parts.
    let later = centre(&mut driver, "#a");
    driver.pointer_move(later.0, later.1).unwrap();
    driver.pointer_down(later.0, later.1, MouseButton::Left, 1).unwrap();
    driver.pointer_up(later.0, later.1, MouseButton::Left, 1).unwrap();
    assert_eq!(evaluate(&mut driver, "out.textContent"), "\"A pulsado\"");

    // A click twice in a row, with another button, and with a key held.
    evaluate(&mut driver, "(() => { window.seen = []; const a = document.querySelector('#a'); a.addEventListener('dblclick', () => seen.push('dbl')); a.addEventListener('contextmenu', event => { event.preventDefault(); seen.push('menu'); }); a.addEventListener('click', event => seen.push(event.shiftKey ? 'shift' : 'plain')); return 0; })()");
    driver.click_with(&Target::Selector("#a"), MouseButton::Left, 2).unwrap();
    driver.click_with(&Target::Selector("#a"), MouseButton::Right, 1).unwrap();
    driver.hold_modifiers(openide_browser::modifiers::SHIFT);
    driver.click(&Target::Selector("#a")).unwrap();
    driver.hold_modifiers(0);
    assert_eq!(evaluate(&mut driver, "seen.join()"), "\"plain,plain,dbl,menu,shift\"");

    // Dragging over text selects it.
    let heading = evaluate(&mut driver, "(() => { const r = document.querySelector('h1').getBoundingClientRect(); return [r.left, r.top + r.height / 2, r.right]; })()");
    let heading: Vec<f64> = serde_json::from_str(&heading).unwrap();
    driver.pointer_move(heading[0] + 1.0, heading[1]).unwrap();
    driver.pointer_down(heading[0] + 1.0, heading[1], MouseButton::Left, 1).unwrap();
    for step in 1..=8 {
        driver.pointer_move(heading[0] + 1.0 + (heading[2] - heading[0]) * f64::from(step) / 8.0, heading[1]).unwrap();
    }
    driver.pointer_up(heading[2], heading[1], MouseButton::Left, 1).unwrap();
    assert_eq!(driver.copy_selection().unwrap(), "Herramientas");
    // A double click selects a word.
    driver.pointer_down(heading[0] + 20.0, heading[1], MouseButton::Left, 1).unwrap();
    driver.pointer_up(heading[0] + 20.0, heading[1], MouseButton::Left, 1).unwrap();
    driver.pointer_down(heading[0] + 20.0, heading[1], MouseButton::Left, 2).unwrap();
    driver.pointer_up(heading[0] + 20.0, heading[1], MouseButton::Left, 2).unwrap();
    assert_eq!(driver.copy_selection().unwrap(), "Herramientas");

    // Keys: typed characters, a held modifier, editing keys and text from an input method.
    let field = centre(&mut driver, "input[aria-label=Correo]");
    driver.pointer_down(field.0, field.1, MouseButton::Left, 1).unwrap();
    driver.pointer_up(field.0, field.1, MouseButton::Left, 1).unwrap();
    for key in ["h", "o", "l", "a"] {
        driver.key_down(key).unwrap();
        driver.key_up(key).unwrap();
    }
    driver.key_down("Shift").unwrap();
    driver.key_down("1").unwrap();
    driver.key_up("1").unwrap();
    driver.key_down("KeyA").unwrap();
    driver.key_up("KeyA").unwrap();
    driver.key_up("Shift").unwrap();
    driver.insert_text(" ñandú 🧠").unwrap();
    let value = "document.querySelector('input[aria-label=Correo]').value";
    assert_eq!(evaluate(&mut driver, value), "\"hola!A ñandú 🧠\"");
    driver.key_down("Backspace").unwrap();
    driver.key_up("Backspace").unwrap();
    driver.key_down("Home").unwrap();
    driver.key_up("Home").unwrap();
    driver.key_down("Delete").unwrap();
    driver.key_up("Delete").unwrap();
    assert_eq!(evaluate(&mut driver, value), "\"ola!A ñandú \"");
    driver.key_down("Control").unwrap();
    driver.key_down("a").unwrap();
    driver.key_up("a").unwrap();
    driver.key_up("Control").unwrap();
    assert_eq!(driver.copy_selection().unwrap(), "ola!A ñandú ");
    assert_eq!(evaluate(&mut driver, value), "\"ola!A ñandú \"", "a shortcut types nothing");
    driver.press("Shift+End").unwrap();
    assert_eq!(driver.key_down("NoSuchKey").unwrap_err().kind, ErrorKind::Invalid);
    assert_eq!(driver.pointer_move(-1.0, 5.0).unwrap_err().kind, ErrorKind::Invalid);

    // The wheel scrolls where the pointer is.
    evaluate(&mut driver, "() => { document.body.style.height = '3000px'; }");
    driver.wheel(320.0, 240.0, 0.0, 500.0).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while evaluate(&mut driver, "scrollY") == "0" && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert_ne!(evaluate(&mut driver, "scrollY"), "0");
}

#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless, against local fixture pages"]
fn real_cef_driver_actions_wait_for_an_element_to_be_ready_and_give_up_with_the_reason() {
    let page = r#"<!doctype html><title>Esperas</title><style>button{margin:4px}#cover{position:fixed;inset:0;background:rgba(0,0,0,.2)}
      #moving{position:relative;animation:slide 1.2s linear 1 forwards}@keyframes slide{from{left:0}to{left:300px}}</style>
      <button id="late" style="display:none" onclick="out.textContent='tarde'">Tarde</button>
      <button id="off" disabled onclick="out.textContent='habilitado'">Deshabilitado</button>
      <button id="moving" onclick="out.textContent='quieto en '+Math.round(this.getBoundingClientRect().left)">Móvil</button>
      <button id="covered" onclick="out.textContent='cubierto'">Cubierto</button>
      <input id="locked" readonly value="fijo"><input id="number" type="number"><input id="date" type="date">
      <div id="far" style="margin-top:3000px"><button onclick="out.textContent='lejos'">Lejos</button></div>
      <p id="out">Sin cambios</p><div id="cover"></div>
      <script>setTimeout(()=>{late.style.display=''},600);setTimeout(()=>{off.disabled=false},600);setTimeout(()=>{cover.remove()},900)</script>"#;
    let server = Server::start(move |_, _| Response::html(page));
    let (_page, mut driver) = open(640, 480, 1.0);
    driver.navigate(&server.origin).unwrap();
    let started = Instant::now();
    // Covered until the overlay goes: the click waits instead of hitting the overlay.
    driver.click(&Target::Selector("#covered")).unwrap();
    assert_eq!(evaluate(&mut driver, "out.textContent"), "\"cubierto\"");
    assert!(started.elapsed() >= Duration::from_millis(700), "{:?}", started.elapsed());
    driver.click(&Target::Selector("#late")).unwrap();
    assert_eq!(evaluate(&mut driver, "out.textContent"), "\"tarde\"");
    driver.click(&Target::Selector("#off")).unwrap();
    assert_eq!(evaluate(&mut driver, "out.textContent"), "\"habilitado\"");
    // Clicked only once it stopped moving, where it ended up.
    driver.navigate(&server.origin).unwrap();
    driver.click(&Target::Selector("#moving")).unwrap();
    let rested = evaluate(&mut driver, "out.textContent");
    let left: u32 = rested.trim_matches('"').trim_start_matches("quieto en ").parse().unwrap_or_else(|_| panic!("{rested}"));
    assert!(left >= 300, "{rested}");
    // Out of view: scrolled to first.
    driver.click(&Target::Role { role: "button", name: "Lejos" }).unwrap();
    assert_eq!(evaluate(&mut driver, "out.textContent"), "\"lejos\"");
    assert_ne!(evaluate(&mut driver, "scrollY"), "0");

    // Fields: what cannot be filled says so, and special inputs take their value.
    driver.fill(&Target::Selector("#date"), "2026-10-11").unwrap();
    assert_eq!(evaluate(&mut driver, "document.querySelector('#date').value"), "\"2026-10-11\"");
    assert!(driver.fill(&Target::Selector("#date"), "ayer").unwrap_err().message.contains("Malformed"));
    driver.fill(&Target::Selector("#number"), "42").unwrap();
    assert_eq!(evaluate(&mut driver, "document.querySelector('#number').value"), "\"42\"");
    driver.fill(&Target::Selector("#number"), "").unwrap();
    assert_eq!(evaluate(&mut driver, "document.querySelector('#number').value"), "\"\"", "an empty text clears the field");
    assert!(driver.fill(&Target::Selector("#number"), "doce").unwrap_err().message.contains("number"));
    assert!(driver.fill(&Target::Selector("#out"), "x").unwrap_err().message.contains("not an <input>"));
    assert_eq!(driver.fill(&Target::Selector("#number"), &"9".repeat(MAX_INPUT_TEXT_BYTES + 1)).unwrap_err().kind, ErrorKind::Invalid);

    // What never becomes ready fails after the timeout, saying why, and the driver goes on.
    evaluate(&mut driver, "() => { const cover = document.createElement('div'); cover.id = 'cover'; document.body.append(cover); scrollTo(0, 0); }");
    let started = Instant::now();
    let covered = driver.click(&Target::Selector("#covered")).unwrap_err();
    // The click was never sent: the tool's own answer, not an unknown effect.
    assert_eq!(covered.kind, ErrorKind::Tool);
    assert!(covered.message.starts_with("Timeout 10000ms exceeded.") && covered.message.contains("<div id=\"cover\">"), "{covered}");
    assert!(started.elapsed() >= Duration::from_secs(9) && started.elapsed() < Duration::from_secs(12));
    let readonly = driver.fill(&Target::Selector("#locked"), "x").unwrap_err();
    assert!(readonly.message.contains("sólo lectura"), "{readonly}");
    assert_eq!(evaluate(&mut driver, "out.textContent"), "\"lejos\"", "nothing was clicked meanwhile");
}
