//! The driver: the one authoritative page of a browser context.
//!
//! Every public method is one blocking request. It validates its arguments
//! before anything reaches the browser, bounds everything the page returns
//! and leaves the driver usable after a failure of the page.

use crate::cdp::{CdpError, Client};
use crate::observer::{
    child_setup, limited, ConsoleEntry, ConsoleLevel, DialogPlan, NetworkEntry, Observer, Report, Shared,
};
use crate::scripts::LIBRARY;
use crate::snapshot::{self, AxTree, DocumentKey, DomSnapshot, Frame as SnapshotFrame, RefTable, SnapshotOptions};
use crate::transport::Transport;
use crate::{DriverError, ErrorKind};
use base64::Engine;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long one action, wait or navigation may take.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// How long attaching to the page may take.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(20);
/// Bytes of the accessibility snapshot.
pub const MAX_ARIA_BYTES: usize = 64 * 1024;
/// Files one file chooser can be given.
pub const MAX_UPLOAD_FILES: usize = 16;
/// Bytes of an evaluated value's JSON, and of the script that produces it.
pub const MAX_EVALUATE_BYTES: usize = 16 * 1024;
/// Bytes of a PNG returned by `screenshot`.
pub const MAX_SCREENSHOT_BYTES: usize = 2 * 1024 * 1024;
/// Bytes of the selection handed to the clipboard.
pub const MAX_SELECTION_BYTES: usize = 16 * 1024;
/// Bytes of text typed or filled in one request.
pub const MAX_INPUT_TEXT_BYTES: usize = 16 * 1024;
pub const MAX_URL_BYTES: usize = 4096;
const MAX_TITLE_BYTES: usize = 2048;
/// Documents one snapshot descends into.
const MAX_SNAPSHOT_FRAMES: usize = 32;
/// The script world of the driver's own helpers.
const WORLD: &str = "openide-driver";
/// Handles of one request, released when it ends.
const GROUP: &str = "openide";
/// The handle a prepared cut keeps between its two requests.
const CUT_GROUP: &str = "openide-cut";

pub(crate) type Outcome<T> = Result<T, DriverError>;

pub(crate) fn tool(message: impl Into<String>) -> DriverError {
    DriverError::new(ErrorKind::Tool, message)
}

pub(crate) fn invalid(message: impl Into<String>) -> DriverError {
    DriverError::new(ErrorKind::Invalid, message)
}

pub(crate) fn failed(message: impl Into<String>) -> DriverError {
    DriverError::new(ErrorKind::Failed, message)
}

pub(crate) fn timed_out() -> String {
    format!("Timeout {}ms exceeded.", TIMEOUT.as_millis())
}

impl From<CdpError> for DriverError {
    fn from(error: CdpError) -> Self {
        match error {
            CdpError::Closed => DriverError::new(ErrorKind::Closed, "El navegador se cerró."),
            CdpError::Timeout => failed(timed_out()),
            other => failed(other.to_string()),
        }
    }
}

/// Where the page is and what its history allows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageInfo {
    pub url: String,
    pub title: String,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub text: String,
    /// The tree was longer than `MAX_ARIA_BYTES` and ends at a whole line.
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evaluated {
    /// The JSON of the value, or its text when it has no JSON form.
    pub text: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Png {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capture<'a> {
    Viewport,
    /// The whole scrollable page.
    FullPage,
    /// One element, scrolled into view if needed.
    Element(Target<'a>),
}

/// One way of naming exactly one element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    /// A `[ref=…]` of the latest snapshot.
    Ref(&'a str),
    /// An exact role and accessible name, as the snapshot prints them.
    Role { role: &'a str, name: &'a str },
    /// A CSS selector that matches a single element of the main frame.
    Selector(&'a str),
}

impl<'a> Target<'a> {
    /// Builds a target from optional fields, of which exactly one way of
    /// naming an element must be given.
    pub fn from_parts(
        reference: Option<&'a str>,
        selector: Option<&'a str>,
        role: Option<&'a str>,
        name: Option<&'a str>,
    ) -> Result<Self, DriverError> {
        let target = match (reference, selector, role, name) {
            (Some(reference), None, None, None) => Self::Ref(reference),
            (None, Some(selector), None, None) => Self::Selector(selector),
            (None, None, Some(role), Some(name)) => Self::Role { role, name },
            _ => return Err(invalid(TARGET_HELP)),
        };
        target.validate()?;
        Ok(target)
    }

    pub(crate) fn validate(&self) -> Outcome<()> {
        let short = |text: &str, limit: usize| !text.is_empty() && text.len() <= limit && !text.chars().any(char::is_control);
        let valid = match self {
            Self::Ref(reference) => valid_ref(reference),
            Self::Selector(selector) => short(selector, 1024),
            Self::Role { role, name } => short(role, 64) && short(name, 1024),
        };
        if valid {
            Ok(())
        } else {
            Err(invalid(TARGET_HELP))
        }
    }
}

const TARGET_HELP: &str =
    "Elegí una referencia de la última captura (ref), un selector único o un rol y nombre accesible exactos";

/// Shape of a ref in a snapshot: `e12`, or `f1e12` inside a frame.
pub(crate) fn valid_ref(reference: &str) -> bool {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    reference.len() <= 32
        && reference.rsplit_once('e').is_some_and(|(frame, element)| {
            digits(element) && (frame.is_empty() || frame.strip_prefix('f').is_some_and(digits))
        })
}

pub(crate) fn validate_viewport(width: u32, height: u32) -> Outcome<()> {
    if (320..=1600).contains(&width) && (240..=1200).contains(&height) {
        Ok(())
    } else {
        Err(invalid("Viewport permitido: 320–1600 × 240–1200 CSS píxeles"))
    }
}

fn validate_scale(scale: f64) -> Outcome<()> {
    if scale.is_finite() && (0.5..=4.0).contains(&scale) {
        Ok(())
    } else {
        Err(invalid("Escala de dispositivo permitida: 0,5 a 4"))
    }
}

/// Only `about:blank` and HTTP or HTTPS addresses with a host and without
/// embedded credentials. Stricter than a URL parser on purpose: what is not
/// plainly `scheme://host` is refused rather than interpreted.
pub(crate) fn validate_url(url: &str) -> Outcome<()> {
    if url.is_empty() || url.len() > MAX_URL_BYTES || url.chars().any(char::is_control) {
        return Err(invalid("URL vacía o demasiado extensa"));
    }
    if url == "about:blank" {
        return Ok(());
    }
    let refused = || invalid("Sólo se permiten URLs HTTP/HTTPS sin credenciales incrustadas.");
    let lower = url.to_ascii_lowercase();
    let rest = ["http://", "https://"]
        .into_iter()
        .find_map(|scheme| lower.starts_with(scheme).then(|| &url[scheme.len()..]))
        .ok_or_else(refused)?;
    // A browser also ends the authority at a backslash.
    let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or("");
    let host = authority.rsplit_once(':').map_or(authority, |(host, port)| {
        if port.bytes().all(|byte| byte.is_ascii_digit()) { host } else { authority }
    });
    if host.is_empty() || authority.contains('@') || authority.chars().any(char::is_whitespace) {
        return Err(refused());
    }
    Ok(())
}

/// An element found for a target: the protocol session and frame it lives in
/// and its node there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub session: String,
    pub frame: String,
    pub backend: i64,
}

struct PreparedCut {
    token: String,
    session: String,
    object: String,
}

/// What a pointed action is about to do at a point of the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    /// The pointer goes there: a hover, or an end of a drag.
    Move,
    Click,
    /// A field is about to be written in.
    Type,
}

/// Told of each pointed action before its input is sent: what it does and
/// where, in CSS pixels of the viewport. It answers whether it took time
/// over it, in which the page may have moved.
pub type ActionWatch = Box<dyn FnMut(Cue, (f64, f64)) -> bool + Send>;

pub struct Driver {
    pub(crate) client: Client,
    pub(crate) shared: Arc<Shared>,
    /// The session of the page and the id of its main frame.
    pub(crate) session: String,
    pub(crate) main_frame: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) scale: f64,
    pub(crate) refs: RefTable,
    /// When the request in progress gives up.
    pub(crate) deadline: Instant,
    /// Sessions holding handles of the request in progress.
    touched: Vec<String>,
    cut: Option<PreparedCut>,
    /// The pointer and keyboard as the page last saw them.
    pub(crate) pointer: (f64, f64),
    pub(crate) buttons: u8,
    pub(crate) modifiers: u8,
    watch: Option<ActionWatch>,
}

impl std::fmt::Debug for Driver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Driver")
            .field("viewport", &(self.width, self.height, self.scale))
            .field("closed", &self.client.is_closed())
            .finish_non_exhaustive()
    }
}

impl Driver {
    /// Has `watch` told of each pointed action before its input is sent. The
    /// page hears nothing of it: it is for a host to show the action, and the
    /// time `watch` takes is time the action waits.
    pub fn watch_actions(&mut self, watch: Option<ActionWatch>) {
        self.watch = watch;
    }

    /// Whether the watch took time over it.
    pub(crate) fn cue(&mut self, cue: Cue, point: (f64, f64)) -> bool {
        self.watch.as_mut().is_some_and(|watch| watch(cue, point))
    }

    /// Drives the one page an embedder already created, which `transport`
    /// reaches: an embedded Chromium hands out the DevTools endpoint of each
    /// page it makes. The page's lifetime, its storage, its popups and its
    /// downloads are the embedder's to keep, isolate and refuse, and `scale`
    /// is the one it draws the page at.
    pub fn connect_to_page(transport: Transport, width: u32, height: u32, scale: f64) -> Result<Self, DriverError> {
        validate_viewport(width, height)?;
        validate_scale(scale)?;
        let shared = Shared::new();
        let observer = Observer {
            shared: shared.clone(),
        };
        let client = Client::connect(transport, Box::new(observer)).map_err(|error| failed(error.to_string()))?;
        let mut driver = Self {
            client,
            shared,
            session: String::new(),
            main_frame: String::new(),
            width,
            height,
            scale,
            refs: RefTable::default(),
            deadline: Instant::now() + ATTACH_TIMEOUT,
            touched: Vec::new(),
            cut: None,
            pointer: (0.0, 0.0),
            buttons: 0,
            modifiers: 0,
            watch: None,
        };
        // The endpoint is the page, and names itself.
        let info: Value = driver.call("", "Target.getTargetInfo", json!({}))?;
        let target = info["targetInfo"]["targetId"].as_str().unwrap_or_default().to_owned();
        // A session of its own for the page: everything below addresses it
        // as it addresses the frames of other sites, each with its session.
        let attached: Value = driver.call("", "Target.attachToTarget", json!({"targetId": target, "flatten": true}))?;
        driver.session = attached["sessionId"].as_str().unwrap_or_default().to_owned();
        if target.is_empty() || driver.session.is_empty() {
            return Err(failed("El navegador no entregó su página."));
        }
        {
            // A page's target id is the id of its main frame.
            let mut state = driver.shared.state();
            state.page_session = driver.session.clone();
            state.main_frame = target.clone();
        }
        driver.main_frame = target;
        let session = driver.session.clone();
        driver.call::<Value>(&session, "Page.setLifecycleEventsEnabled", json!({"enabled": true}))?;
        driver.call::<Value>(&session, "Emulation.setFocusEmulationEnabled", json!({"enabled": true}))?;
        driver.apply_viewport()?;
        for (method, params) in child_setup() {
            driver.call::<Value>(&session, method, params)?;
        }
        Ok(driver)
    }

    fn apply_viewport(&mut self) -> Outcome<()> {
        let session = self.session.clone();
        // A scale of zero keeps the one the embedder draws the page at.
        self.call::<Value>(
            &session,
            "Emulation.setDeviceMetricsOverride",
            json!({"width": self.width, "height": self.height, "deviceScaleFactor": 0, "mobile": false}),
        )?;
        Ok(())
    }

    /// Sends one protocol request within what is left of the current deadline.
    pub(crate) fn call<T: DeserializeOwned>(&self, session: &str, method: &str, params: Value) -> Outcome<T> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(failed(timed_out()));
        }
        Ok(self.client.call(session, method, params, left)?)
    }

    /// Runs one public request: a fresh deadline before, and afterwards the
    /// release of every handle it took in the page.
    pub(crate) fn run<T>(&mut self, limit: Duration, work: impl FnOnce(&mut Self) -> Outcome<T>) -> Outcome<T> {
        if let Some(reason) = self.shared.state().gone.clone() {
            return Err(DriverError::new(ErrorKind::Closed, reason));
        }
        self.deadline = Instant::now() + limit;
        let result = work(self);
        for session in std::mem::take(&mut self.touched) {
            let _ = self.client.post(&session, "Runtime.releaseObjectGroup", json!({"objectGroup": GROUP}));
        }
        match result {
            Err(error) if error.kind != ErrorKind::Closed => {
                // A request that failed because the page died says so.
                match self.shared.state().gone.clone() {
                    Some(reason) => Err(DriverError::new(ErrorKind::Closed, reason)),
                    None => Err(error),
                }
            }
            other => other,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Outcome<()> {
        validate_viewport(width, height)?;
        self.run(TIMEOUT, |driver| {
            (driver.width, driver.height) = (width, height);
            driver.apply_viewport()
        })
    }

    // ---- Navigation ----

    pub fn navigate(&mut self, url: &str) -> Outcome<()> {
        validate_url(url)?;
        self.run(TIMEOUT, |driver| {
            let before = driver.shared.state().loader.clone();
            let reply = driver.navigation_command("Page.navigate", json!({"url": url}))?;
            let loader = reply["loaderId"].as_str();
            if let Some(error) = reply["errorText"].as_str().filter(|error| !error.is_empty()) {
                // Chrome now shows its error page. Let it settle, so the next
                // request does not mistake its arrival for its own navigation.
                if let Some(loader) = loader {
                    let settled = (Instant::now() + Duration::from_secs(2)).min(driver.deadline);
                    driver.shared.wait(settled, |state| (state.dom_content_loader == loader).then_some(()));
                }
                return Err(DriverError::new(ErrorKind::Navigation, format!("{error} at {url}")));
            }
            // Without a loader the navigation stayed inside the document.
            let Some(loader) = loader else {
                return Ok(());
            };
            let loaded = driver.shared.wait(driver.deadline, |state| {
                let superseded = state.loader != before && state.loader != loader;
                (state.dom_content_loader == loader || (superseded && state.dom_content_loader == state.loader))
                    .then_some(())
            });
            loaded.ok_or_else(|| DriverError::new(ErrorKind::Navigation, timed_out()))
        })
    }

    pub fn back(&mut self) -> Outcome<()> {
        self.history(-1)
    }

    pub fn forward(&mut self) -> Outcome<()> {
        self.history(1)
    }

    fn history(&mut self, step: i64) -> Outcome<()> {
        self.run(TIMEOUT, |driver| {
            let session = driver.session.clone();
            let history: Value = driver.call(&session, "Page.getNavigationHistory", json!({}))?;
            let entry = history["currentIndex"]
                .as_i64()
                .and_then(|current| usize::try_from(current + step).ok())
                .and_then(|at| history["entries"].get(at))
                .and_then(|entry| entry["id"].as_i64());
            // Nowhere to go is not a failure, as in any browser.
            let Some(entry) = entry else {
                return Ok(());
            };
            let commits = driver.shared.state().commits;
            driver.navigation_command("Page.navigateToHistoryEntry", json!({"entryId": entry}))?;
            driver.committed(commits)
        })
    }

    pub fn reload(&mut self) -> Outcome<()> {
        self.run(TIMEOUT, |driver| {
            let commits = driver.shared.state().commits;
            driver.navigation_command("Page.reload", json!({}))?;
            driver.committed(commits)
        })
    }

    /// Asks the page to navigate. Between two renderer processes (leaving an
    /// error page, for one) it has no active frame for an instant, and says
    /// so instead of navigating: that is asked again.
    fn navigation_command(&mut self, method: &str, params: Value) -> Outcome<Value> {
        let session = self.session.clone();
        loop {
            match self.call(&session, method, params.clone()) {
                Err(error) if error.message.contains("Not attached to an active page") => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                reply => return reply.map_err(navigation),
            }
        }
    }

    /// Waits for the main frame to commit after `commits` and for that
    /// document's `DOMContentLoaded`.
    fn committed(&mut self, commits: u64) -> Outcome<()> {
        let loaded = self.shared.wait(self.deadline, |state| {
            (state.commits > commits && state.dom_content_loader == state.loader).then(|| state.unreachable.clone())
        });
        match loaded {
            None => Err(DriverError::new(ErrorKind::Navigation, timed_out())),
            Some(None) => Ok(()),
            Some(Some(url)) => {
                let state = self.shared.state();
                let failure = state.network.report(|entry| entry.failure.is_some() && entry.resource_type == "document");
                let reason = failure.entries.last().and_then(|entry| entry.failure.clone());
                Err(DriverError::new(
                    ErrorKind::Navigation,
                    format!("{} at {url}", reason.as_deref().unwrap_or("net::ERR_FAILED")),
                ))
            }
        }
    }

    pub fn info(&mut self) -> Outcome<PageInfo> {
        self.run(TIMEOUT, |driver| {
            let session = driver.session.clone();
            let mut info = PageInfo {
                url: limited(&driver.shared.state().url, MAX_URL_BYTES).to_owned(),
                width: driver.width,
                height: driver.height,
                scale: driver.scale,
                ..PageInfo::default()
            };
            // History is optional while a page is changing or crashing; it
            // must not turn an answer about the page into a failure.
            if let Ok(history) = driver.call::<Value>(&session, "Page.getNavigationHistory", json!({})) {
                let current = history["currentIndex"].as_u64().unwrap_or(0) as usize;
                let entries = history["entries"].as_array().map_or(&[][..], Vec::as_slice);
                info.can_go_back = current > 0;
                info.can_go_forward = current + 1 < entries.len();
                let title = entries.get(current).and_then(|entry| entry["title"].as_str()).unwrap_or("");
                info.title = limited(title, MAX_TITLE_BYTES).to_owned();
            }
            Ok(info)
        })
    }

    // ---- Snapshot ----

    pub fn snapshot(&mut self, options: &SnapshotOptions) -> Outcome<Snapshot> {
        if let Some(selector) = &options.selector {
            Target::Selector(selector).validate()?;
        }
        self.run(TIMEOUT, |driver| {
            let root = match &options.selector {
                Some(selector) => Some(driver.resolve(&Target::Selector(selector))?.backend),
                None => None,
            };
            let frames = driver.collect_frames()?;
            let text = snapshot::render(&frames, root, options, &mut driver.refs)
                .ok_or_else(|| tool("El selector no corresponde a un elemento visible de la página."))?;
            // Cut at a whole line: half a line would carry half a ref.
            let mut end = limited(&text, MAX_ARIA_BYTES).len();
            if end < text.len() {
                end = text[..end].rfind('\n').unwrap_or(0);
            }
            Ok(Snapshot {
                truncated: end < text.len(),
                text: text[..end].to_owned(),
            })
        })
    }

    /// The documents the page shows, the main frame first: each with its
    /// accessibility tree and what the DOM adds to it.
    fn collect_frames(&mut self) -> Outcome<Vec<SnapshotFrame>> {
        let mut frames: Vec<SnapshotFrame> = Vec::new();
        let mut queue = VecDeque::from([(self.session.clone(), self.main_frame.clone())]);
        // Per session: what the DOM knows about each of its documents, and their loaders.
        let mut sessions: HashMap<String, (HashMap<String, snapshot::DomIndex>, HashMap<String, String>)> = HashMap::new();
        while let Some((session, frame)) = queue.pop_front() {
            let (doms, loaders) = match sessions.entry(session.clone()) {
                Entry::Occupied(known) => known.into_mut(),
                Entry::Vacant(unknown) => {
                    let dom = self
                        .call::<DomSnapshot>(
                            &session,
                            "DOMSnapshot.captureSnapshot",
                            json!({"computedStyles": snapshot::COMPUTED_STYLES}),
                        )
                        .map(DomSnapshot::into_frames);
                    // Without the DOM the tree is still readable: every
                    // element gets a ref and frames are not descended into.
                    unknown.insert((dom.unwrap_or_default(), self.loaders(&session)?))
                }
            };
            let dom = doms.remove(&frame).unwrap_or_default();
            let loader = loaders.get(&frame).cloned().unwrap_or_default();
            let tree = self.call::<AxTree>(&session, "Accessibility.getFullAXTree", json!({"frameId": frame}));
            // A frame that went away meanwhile keeps its place, empty.
            let ax = match tree {
                Ok(tree) => tree.nodes,
                Err(error) if frames.is_empty() => return Err(error),
                Err(_) => Vec::new(),
            };
            let document = self.refs.document(&DocumentKey {
                session: session.clone(),
                frame: frame.clone(),
                loader,
            });
            let mut children = HashMap::new();
            for node in &ax {
                let embeds = !node.ignored && snapshot::aria_role(node.role.as_ref().and_then(|role| role.value.as_str()).unwrap_or("")) == "iframe";
                let Some(backend) = node.backend.filter(|_| embeds) else {
                    continue;
                };
                if frames.len() + 1 + queue.len() >= MAX_SNAPSHOT_FRAMES {
                    break;
                }
                let child = match dom.get(&backend).and_then(|node| node.content_frame.clone()) {
                    Some(frame) => Some((session.clone(), frame)),
                    None => self.remote_frame(&session, backend),
                };
                if let Some(child) = child {
                    children.insert(backend, frames.len() + 1 + queue.len());
                    queue.push_back(child);
                }
            }
            frames.push(SnapshotFrame {
                ax,
                dom,
                session,
                frame,
                document,
                children,
            });
        }
        Ok(frames)
    }

    /// The protocol replies a snapshot of the current page is built from, as
    /// JSON. This is how the recordings the unit tests replay are made.
    #[doc(hidden)]
    pub fn snapshot_recording(&mut self) -> Outcome<String> {
        self.run(TIMEOUT, |driver| {
            let frames = driver.collect_frames()?;
            driver.refs.settle(false);
            let mut recorded = snapshot::Recorded::default();
            let mut doms: HashMap<String, usize> = HashMap::new();
            for frame in &frames {
                let dom = match doms.get(&frame.session) {
                    Some(dom) => *dom,
                    None => {
                        let reply = driver.call::<DomSnapshot>(
                            &frame.session,
                            "DOMSnapshot.captureSnapshot",
                            json!({"computedStyles": snapshot::COMPUTED_STYLES}),
                        )?;
                        let dom = recorded.push_dom(reply);
                        doms.insert(frame.session.clone(), dom);
                        dom
                    }
                };
                let tree: AxTree = driver.call(&frame.session, "Accessibility.getFullAXTree", json!({"frameId": frame.frame}))?;
                recorded.push_frame(frame, dom, tree.nodes);
            }
            serde_json::to_string(&recorded).map_err(|error| failed(error.to_string()))
        })
    }

    /// The loader of every frame a session renders, by frame id.
    pub(crate) fn loaders(&self, session: &str) -> Outcome<HashMap<String, String>> {
        fn walk(tree: &Value, depth: usize, loaders: &mut HashMap<String, String>) {
            let frame = &tree["frame"];
            if let (Some(id), Some(loader)) = (frame["id"].as_str(), frame["loaderId"].as_str()) {
                loaders.insert(id.to_owned(), loader.to_owned());
            }
            for child in tree["childFrames"].as_array().map_or(&[][..], Vec::as_slice) {
                if depth < 64 && loaders.len() < 1024 {
                    walk(child, depth + 1, loaders);
                }
            }
        }
        let tree: Value = self.call(session, "Page.getFrameTree", json!({}))?;
        let mut loaders = HashMap::new();
        walk(&tree["frameTree"], 0, &mut loaders);
        Ok(loaders)
    }

    /// The session and frame behind an `<iframe>` that another process renders.
    fn remote_frame(&mut self, session: &str, backend: i64) -> Option<(String, String)> {
        let node: Value = self.call(session, "DOM.describeNode", json!({"backendNodeId": backend})).ok()?;
        let frame = node["node"]["frameId"].as_str()?;
        let child = self.shared.state().children.get(frame)?.session.clone();
        Some((child, frame.to_owned()))
    }

    // ---- Targets ----

    /// Finds the one element a target names. Nothing on the page is touched:
    /// a stale ref, no match and several matches are answered here.
    pub(crate) fn resolve(&mut self, target: &Target) -> Outcome<Resolved> {
        target.validate()?;
        let ambiguous = |count: usize| {
            tool(format!(
                "El destino coincide con {count} elementos y debe identificar exactamente uno. Tomá una captura nueva con browser_snapshot y usá la referencia del elemento."
            ))
        };
        match *target {
            Target::Ref(reference) => {
                let stale = || {
                    tool(format!(
                        "La referencia {reference} no corresponde a un elemento de la página actual. Tomá una captura nueva con browser_snapshot y usá sus referencias."
                    ))
                };
                let (key, backend) = self.refs.resolve(reference).map(|(key, backend)| (key.clone(), backend)).ok_or_else(stale)?;
                // The ref was issued for one document: the frame must still hold it.
                let current = self.loaders(&key.session).map_err(closed_or(stale))?;
                if current.get(&key.frame) != Some(&key.loader) {
                    self.refs.retain(|document| *document != key);
                    return Err(stale());
                }
                let resolved = Resolved {
                    session: key.session,
                    frame: key.frame,
                    backend,
                };
                let connected = self
                    .element(&resolved)
                    .and_then(|object| self.library::<bool>(&resolved.session, Some(&object), None, "connected", json!([])));
                match connected {
                    Ok(true) => Ok(resolved),
                    Ok(false) => Err(stale()),
                    Err(error) => Err(closed_or(stale)(error)),
                }
            }
            Target::Selector(selector) => {
                let (session, frame) = (self.session.clone(), self.main_frame.clone());
                let world = self.world(&session, &frame)?;
                let found = self
                    .invoke(&session, None, Some(world), LIBRARY, json!([{"value": "query"}, {"value": selector}]), false)
                    .map_err(|error| match error.kind {
                        // The page's own complaint about the selector.
                        ErrorKind::Tool => tool(format!("Selector no válido: {}", error.message)),
                        _ => error,
                    })?;
                let Some(object) = found["objectId"].as_str() else {
                    return Err(ambiguous(found["value"].as_u64().unwrap_or(0) as usize));
                };
                let node: Value = self.call(&session, "DOM.describeNode", json!({"objectId": object}))?;
                let backend = node["node"]["backendNodeId"].as_i64().ok_or_else(|| ambiguous(0))?;
                Ok(Resolved {
                    session,
                    frame,
                    backend,
                })
            }
            Target::Role { role, name } => {
                let (session, frame) = (self.session.clone(), self.main_frame.clone());
                let tree: AxTree = self.call(&session, "Accessibility.getFullAXTree", json!({"frameId": frame}))?;
                let page = SnapshotFrame {
                    ax: tree.nodes,
                    ..SnapshotFrame::default()
                };
                match snapshot::matching(&page, role, &snapshot::normalize(name)).as_slice() {
                    [backend] => Ok(Resolved {
                        session,
                        frame,
                        backend: *backend,
                    }),
                    matches => Err(ambiguous(matches.len())),
                }
            }
        }
    }

    /// The id of the driver's own script world in a frame. Chrome returns the
    /// existing one when the frame already has it.
    pub(crate) fn world(&mut self, session: &str, frame: &str) -> Outcome<i64> {
        let world: Value = self.call(
            session,
            "Page.createIsolatedWorld",
            json!({"frameId": frame, "worldName": WORLD}),
        )?;
        world["executionContextId"].as_i64().ok_or_else(|| failed("El navegador no creó el contexto del driver."))
    }

    /// A handle to the element in the driver's world.
    pub(crate) fn element(&mut self, resolved: &Resolved) -> Outcome<String> {
        let world = self.world(&resolved.session, &resolved.frame)?;
        self.handle(resolved, Some(world))
    }

    /// A handle to the element, in the given world or in the page's own.
    pub(crate) fn handle(&mut self, resolved: &Resolved, world: Option<i64>) -> Outcome<String> {
        let mut params = json!({"backendNodeId": resolved.backend, "objectGroup": GROUP});
        if let Some(world) = world {
            params["executionContextId"] = Value::from(world);
        }
        if !self.touched.contains(&resolved.session) {
            self.touched.push(resolved.session.clone());
        }
        let node: Value = self.call(&resolved.session, "DOM.resolveNode", params)?;
        node["object"]["objectId"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| failed("Element is not attached to the DOM"))
    }

    /// Calls `function` on an object or in a script world. The reply is the
    /// protocol's remote object: its `value` when `by_value`, else a handle.
    /// What the page's script throws comes back as a tool failure carrying
    /// the first line of the error.
    pub(crate) fn invoke(
        &mut self,
        session: &str,
        object: Option<&str>,
        world: Option<i64>,
        function: &str,
        arguments: Value,
        by_value: bool,
    ) -> Outcome<Value> {
        let mut params = json!({
            "functionDeclaration": function,
            "arguments": arguments,
            "returnByValue": by_value,
            "awaitPromise": true,
            "objectGroup": GROUP,
        });
        match (object, world) {
            (Some(object), _) => params["objectId"] = Value::from(object),
            (None, Some(world)) => params["executionContextId"] = Value::from(world),
            (None, None) => return Err(failed("El navegador no tiene un contexto para el script.")),
        }
        if !self.touched.iter().any(|touched| touched == session) {
            self.touched.push(session.to_owned());
        }
        let mut reply: Value = self.call(session, "Runtime.callFunctionOn", params)?;
        if let Some(details) = reply.get("exceptionDetails") {
            return Err(tool(thrown(details)));
        }
        Ok(reply["result"].take())
    }

    /// Runs one operation of the driver's script library and reads its value.
    pub(crate) fn library<T: DeserializeOwned>(
        &mut self,
        session: &str,
        object: Option<&str>,
        world: Option<i64>,
        operation: &str,
        arguments: Value,
    ) -> Outcome<T> {
        let mut list = vec![json!({"value": operation})];
        list.extend(arguments.as_array().into_iter().flatten().map(|value| json!({"value": value})));
        let mut result = self.invoke(session, object, world, LIBRARY, Value::Array(list), true)?;
        serde_json::from_value(result["value"].take()).map_err(|_| failed("La página devolvió un resultado inesperado."))
    }

    // ---- Waiting ----

    /// Waits until some visible element shows `text`.
    pub fn wait_for_text(&mut self, text: &str) -> Outcome<()> {
        self.wait_text(text, true)
    }

    /// Waits until no visible element shows `text`.
    pub fn wait_for_text_gone(&mut self, text: &str) -> Outcome<()> {
        self.wait_text(text, false)
    }

    fn wait_text(&mut self, text: &str, present: bool) -> Outcome<()> {
        if text.is_empty() || text.len() > 1024 || text.chars().any(char::is_control) {
            return Err(invalid("Indicá sólo uno: text, text_gone o seconds (de 0 a 10)"));
        }
        self.run(TIMEOUT, |driver| loop {
            let (session, frame) = (driver.session.clone(), driver.main_frame.clone());
            // A page in the middle of a navigation has no context for a moment: ask again.
            let shown = driver
                .world(&session, &frame)
                .and_then(|world| driver.library::<bool>(&session, None, Some(world), "text", json!([text])));
            match shown {
                Ok(shown) if shown == present => return Ok(()),
                Err(error) if error.kind == ErrorKind::Closed => return Err(error),
                _ => {}
            }
            if driver.deadline.saturating_duration_since(Instant::now()) < Duration::from_millis(100) {
                let outcome = if present { "no apareció" } else { "sigue visible" };
                return Err(tool(format!("El texto {outcome} después de {} s.", TIMEOUT.as_secs())));
            }
            std::thread::sleep(Duration::from_millis(100));
        })
    }

    /// Waits until the page's address contains `part`: a path, a query or
    /// the whole address, as after a redirect or a sign-in.
    pub fn wait_for_url(&mut self, part: &str) -> Outcome<()> {
        if part.is_empty() || part.len() > MAX_URL_BYTES || part.chars().any(char::is_control) {
            return Err(invalid("Indicá la parte de la dirección que se espera"));
        }
        self.run(TIMEOUT, |driver| loop {
            let url = driver.shared.state().url.clone();
            if url.contains(part) {
                return Ok(());
            }
            if driver.deadline.saturating_duration_since(Instant::now()) < Duration::from_millis(100) {
                return Err(tool(format!(
                    "La dirección no llegó a contener eso después de {} s. Es {}",
                    TIMEOUT.as_secs(),
                    limited(&url, 512)
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        })
    }

    /// Waits until the document has been parsed (`complete` false) or has
    /// also loaded what it refers to: images, styles, frames.
    pub fn wait_for_load(&mut self, complete: bool) -> Outcome<()> {
        let ready = if complete { "document.readyState === 'complete'" } else { "document.readyState !== 'loading'" };
        self.run(TIMEOUT, |driver| loop {
            let session = driver.session.clone();
            // A page in the middle of a navigation has no context for a moment: ask again.
            let state: Outcome<Value> =
                driver.call(&session, "Runtime.evaluate", json!({"expression": ready, "returnByValue": true}));
            match state {
                Ok(state) if state["result"]["value"] == true => return Ok(()),
                Err(error) if error.kind == ErrorKind::Closed => return Err(error),
                _ => {}
            }
            if driver.deadline.saturating_duration_since(Instant::now()) < Duration::from_millis(100) {
                return Err(tool(format!("La página no terminó de cargar después de {} s.", TIMEOUT.as_secs())));
            }
            std::thread::sleep(Duration::from_millis(100));
        })
    }

    /// A fixed pause of at most ten seconds.
    pub fn wait_seconds(&mut self, seconds: f64) -> Outcome<()> {
        if !(0.0..=10.0).contains(&seconds) {
            return Err(invalid("Indicá sólo uno: text, text_gone o seconds (de 0 a 10)"));
        }
        std::thread::sleep(Duration::from_secs_f64(seconds));
        Ok(())
    }

    // ---- Evaluate ----

    /// Runs `expression` in the page's own world. An expression is evaluated;
    /// a function is called, with the target element when one is given, and
    /// awaited. The value comes back as JSON text.
    pub fn evaluate(&mut self, expression: &str, target: Option<&Target>) -> Outcome<Evaluated> {
        if expression.trim().is_empty() || expression.len() > MAX_EVALUATE_BYTES || expression.contains('\0') {
            return Err(invalid("Expresión vacía o demasiado extensa"));
        }
        let abandoned = format!("El script no terminó en {} s.", TIMEOUT.as_secs());
        let mut ran_in = None;
        let result = self.run(TIMEOUT, |driver| {
            // A stale target is answered before the script can run.
            let resolved = target.map(|target| driver.resolve(target)).transpose()?;
            let (session, frame) = match &resolved {
                Some(resolved) => (resolved.session.clone(), resolved.frame.clone()),
                None => (driver.session.clone(), driver.main_frame.clone()),
            };
            ran_in = Some(session.clone());
            let unfinished = |error: DriverError| match error.kind {
                ErrorKind::Failed if error.message == timed_out() => tool(abandoned.clone()),
                ErrorKind::Failed => tool(error.message),
                _ => error,
            };
            let mut source = expression.trim().to_owned();
            // A bare function declaration is a statement; as an expression it is a value.
            let declaration = ["function", "async function"].into_iter().any(|keyword| {
                source.strip_prefix(keyword).is_some_and(|rest| rest.starts_with(|next: char| next.is_whitespace() || next == '(' || next == '*'))
            });
            if declaration {
                source = format!("({source})");
            }
            let mut params = json!({"expression": source, "objectGroup": GROUP, "returnByValue": false});
            let context = driver.shared.state().contexts.get(&(session.clone(), frame.clone())).copied();
            if let Some(context) = context {
                params["contextId"] = Value::from(context);
            } else if resolved.is_some() {
                return Err(tool("El marco del elemento todavía no tiene un contexto de script."));
            }
            if !driver.touched.contains(&session) {
                driver.touched.push(session.clone());
            }
            let mut reply: Outcome<Value> = driver.call(&session, "Runtime.evaluate", params.clone());
            // The page's world was replaced a moment ago: its current one is the default.
            if resolved.is_none() && reply.as_ref().is_err_and(|error| error.message.contains("Cannot find context")) {
                params.as_object_mut().map(|params| params.remove("contextId"));
                reply = driver.call(&session, "Runtime.evaluate", params);
            }
            let mut reply = reply.map_err(unfinished)?;
            if let Some(details) = reply.get("exceptionDetails") {
                return Err(tool(thrown(details)));
            }
            let subject = reply["result"].take();
            let Some(object) = subject["objectId"].as_str() else {
                // A primitive is its own value; only its JSON form is needed.
                let text = match (&subject["value"], subject["unserializableValue"].as_str()) {
                    (_, Some(number)) if subject["type"] == "bigint" => number.trim_end_matches('n').to_owned(),
                    (_, Some("-0")) => "0".to_owned(),
                    (_, Some(_)) => "null".to_owned(),
                    (Value::Null, None) if subject["type"] == "undefined" => "undefined".to_owned(),
                    (value, None) => value.to_string(),
                };
                return Ok(bounded_result(&text, false));
            };
            let element = match &resolved {
                Some(resolved) => json!({"objectId": driver.handle(resolved, None)?}),
                None => json!({}),
            };
            let arguments = json!([element, {"value": MAX_EVALUATE_BYTES}]);
            let value = driver
                .invoke(&session, Some(object), None, crate::scripts::EVALUATE, arguments, true)
                .map_err(unfinished)?;
            let text = value["value"]["text"].as_str().unwrap_or_default();
            Ok(bounded_result(text, value["value"]["cut"] == true))
        });
        if let (Err(error), Some(session)) = (&result, &ran_in) {
            if error.message == abandoned {
                self.release_page(session);
            }
        }
        result
    }

    /// After a script ran out of time. One that waits on a promise leaves the
    /// page free and is simply abandoned. One that never yields would hold
    /// the page for every request after it: that one is stopped.
    fn release_page(&self, session: &str) {
        let probe = self.client.call::<Value>(session, "Runtime.evaluate", json!({"expression": "0"}), Duration::from_millis(500));
        if matches!(probe, Err(CdpError::Timeout)) {
            // Handled by Chrome while the script runs; it ends that script only.
            let _ = self.client.post(session, "Runtime.terminateExecution", json!({}));
        }
    }

    // ---- Observation ----

    /// Console messages, uncaught errors and dialogs since the last
    /// navigation, at `least` severity or above.
    pub fn console_messages(&self, least: ConsoleLevel) -> Report<ConsoleEntry> {
        self.shared.state().console.report(|entry| entry.level <= least)
    }

    /// Requests since the last navigation.
    pub fn network_requests(&self) -> Report<NetworkEntry> {
        self.shared.state().network.report(|_| true)
    }

    /// With `on`, what asks for another tab or window (a link with
    /// `target="_blank"`, a `window.open` behind a click) no longer opens in
    /// this page: its address waits in `take_window_request` for a host that
    /// opens tabs.
    pub fn open_windows_as_tabs(&self, on: bool) {
        self.shared.state().windows_as_tabs = on;
    }

    /// The address the page last asked to open in another tab, once.
    pub fn take_window_request(&self) -> Option<String> {
        self.shared.state().window_request.take()
    }

    /// Decides how the next JavaScript dialog is answered. Used once, and
    /// forgotten if the page navigates first. Without it dialogs are dismissed.
    pub fn handle_dialog(&self, accept: bool, prompt_text: Option<&str>) -> Outcome<()> {
        if prompt_text.is_some_and(|text| text.len() > 4096 || text.contains('\0')) {
            return Err(invalid("Texto de diálogo demasiado extenso"));
        }
        self.shared.state().dialog_plan = Some(DialogPlan {
            accept,
            prompt_text: prompt_text.map(str::to_owned),
        });
        Ok(())
    }

    /// The files the next file chooser of the page gets, in place of a
    /// chooser on the desktop. Used once, and forgotten if the page navigates
    /// first. Without it a chooser the page opens stays closed.
    pub fn upload_files(&self, paths: &[String]) -> Outcome<()> {
        if paths.is_empty() || paths.len() > MAX_UPLOAD_FILES {
            return Err(invalid(format!("Indicá entre 1 y {MAX_UPLOAD_FILES} archivos")));
        }
        for path in paths {
            let file = std::path::Path::new(path);
            if !file.is_absolute() || !file.is_file() {
                return Err(invalid(format!("No es la ruta absoluta de un archivo: {}", limited(path, 256))));
            }
        }
        self.shared.state().upload_plan = Some(paths.to_vec());
        Ok(())
    }

    // ---- Screenshots ----

    /// A PNG at one image pixel per CSS pixel, whatever the device scale.
    pub fn screenshot(&mut self, capture: Capture) -> Outcome<Png> {
        if let Capture::Element(target) = &capture {
            target.validate()?;
        }
        // Reading never changes the page, so a capture that fails is only this request's answer.
        let reading = |error: DriverError| match error.kind {
            ErrorKind::Failed => tool(error.message),
            _ => error,
        };
        self.run(TIMEOUT, |driver| {
            let session = driver.session.clone();
            let clip = match capture {
                Capture::Viewport | Capture::FullPage => {
                    let metrics: Value = driver.call(&session, "Page.getLayoutMetrics", json!({}))?;
                    let number = |value: &Value| value.as_f64().unwrap_or(0.0);
                    if capture == Capture::FullPage {
                        let size = &metrics["cssContentSize"];
                        (0.0, 0.0, number(&size["width"]).ceil(), number(&size["height"]).ceil())
                    } else {
                        let viewport = &metrics["cssVisualViewport"];
                        (number(&viewport["pageX"]), number(&viewport["pageY"]), f64::from(driver.width), f64::from(driver.height))
                    }
                }
                Capture::Element(target) => {
                    let resolved = driver.resolve(&target)?;
                    driver.element_clip(&resolved)?
                }
            };
            let (x, y, width, height) = clip;
            let params = json!({
                "format": "png",
                "clip": {"x": x, "y": y, "width": width, "height": height, "scale": 1.0 / driver.scale},
                "captureBeyondViewport": capture != Capture::Viewport,
            });
            let too_large = |bytes: Option<usize>| {
                let size = bytes.map_or(String::new(), |bytes| format!("pesa {} KiB y ", bytes.div_ceil(1024)));
                tool(format!("La captura {size}supera el límite de 2 MiB; capturá el viewport o un elemento."))
            };
            let mut attempt = 0;
            let reply: Value = loop {
                match driver.client.call(&session, "Page.captureScreenshot", params.clone(), driver.deadline.saturating_duration_since(Instant::now())) {
                    Ok(reply) => break reply,
                    Err(CdpError::Oversized) => return Err(too_large(None)),
                    // A surface that was just navigated or resized may not
                    // have a frame yet. Only the capture is retried.
                    Err(CdpError::Protocol { message, .. }) if attempt == 0 && message.contains("Unable to capture screenshot") => {
                        attempt += 1;
                        std::thread::sleep(Duration::from_millis(80));
                    }
                    Err(error) => return Err(DriverError::from(error)),
                }
            };
            let encoded = reply["data"].as_str().unwrap_or_default();
            // Refused by its encoded length when far above the limit, without decoding it.
            if encoded.len() / 4 * 3 > MAX_SCREENSHOT_BYTES + 2 {
                return Err(too_large(Some(encoded.len() / 4 * 3)));
            }
            let image = png(encoded)?;
            if image.data.len() > MAX_SCREENSHOT_BYTES {
                return Err(too_large(Some(image.data.len())));
            }
            Ok(image)
        })
        .map_err(reading)
    }

    /// The element's box in page coordinates, once it is on screen.
    fn element_clip(&mut self, resolved: &Resolved) -> Outcome<(f64, f64, f64, f64)> {
        loop {
            let object = self.element(resolved)?;
            let _: Outcome<Value> = self.call(&resolved.session, "DOM.scrollIntoViewIfNeeded", json!({"objectId": object}));
            let rect: Value = self.library(&resolved.session, Some(&object), None, "rect", json!([]))?;
            if rect["shown"] == true {
                let origin = self.frame_origin(&resolved.session, &resolved.frame, 0)?;
                let session = self.session.clone();
                let metrics: Value = self.call(&session, "Page.getLayoutMetrics", json!({}))?;
                let number = |value: &Value| value.as_f64().unwrap_or(0.0);
                let viewport = &metrics["cssVisualViewport"];
                return Ok((
                    origin.0 + number(&rect["x"]) + number(&viewport["pageX"]),
                    origin.1 + number(&rect["y"]) + number(&viewport["pageY"]),
                    number(&rect["width"]),
                    number(&rect["height"]),
                ));
            }
            if self.deadline.saturating_duration_since(Instant::now()) < Duration::from_millis(100) {
                return Err(failed(timed_out()));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Where a frame's viewport starts, in CSS pixels of the page's viewport.
    pub(crate) fn frame_origin(&mut self, session: &str, frame: &str, depth: usize) -> Outcome<(f64, f64)> {
        if (session == self.session && frame == self.main_frame) || depth > 16 {
            return Ok((0.0, 0.0));
        }
        // The frame is the root of its session when another process renders
        // it: then the element holding it lives in the embedding session.
        let embedding = self.shared.state().children.get(frame).filter(|child| child.session == session).cloned();
        let owner_session = embedding.map_or_else(|| session.to_owned(), |child| child.parent);
        let owner: Value = self.call(&owner_session, "DOM.getFrameOwner", json!({"frameId": frame}))?;
        let model: Value = self.call(&owner_session, "DOM.getBoxModel", json!({"backendNodeId": owner["backendNodeId"]}))?;
        let content = &model["model"]["content"];
        let corner = (content[0].as_f64().unwrap_or(0.0), content[1].as_f64().unwrap_or(0.0));
        // Box coordinates are relative to the root of the session they were asked in.
        let root = if owner_session == self.session {
            (0.0, 0.0)
        } else {
            let root_frame = self
                .shared
                .state()
                .children
                .iter()
                .find(|(_, child)| child.session == owner_session)
                .map(|(frame, _)| frame.clone());
            match root_frame {
                Some(root_frame) => self.frame_origin(&owner_session, &root_frame, depth + 1)?,
                None => (0.0, 0.0),
            }
        };
        Ok((root.0 + corner.0, root.1 + corner.1))
    }

    // ---- Clipboard for the user's view ----

    /// The text the user has selected. Never a password.
    pub fn copy_selection(&mut self) -> Outcome<String> {
        self.run(TIMEOUT, |driver| {
            driver.forget_cut();
            let (session, frame) = (driver.session.clone(), driver.main_frame.clone());
            let world = driver.world(&session, &frame)?;
            let options = json!([{"cut": false, "maximum": MAX_SELECTION_BYTES}]);
            let selection: Value = driver.library(&session, None, Some(world), "selection", options)?;
            selected(&selection)
        })
    }

    /// The first half of a cut: the selected text and a token. Nothing is
    /// removed until `confirm_cut` gets the token back, which the host does
    /// only after its clipboard accepted the text.
    pub fn prepare_cut(&mut self) -> Outcome<(String, String)> {
        self.run(TIMEOUT, |driver| {
            driver.forget_cut();
            let (session, frame) = (driver.session.clone(), driver.main_frame.clone());
            let world = driver.world(&session, &frame)?;
            let arguments = json!([{"value": "selection"}, {"value": {"cut": true, "maximum": MAX_SELECTION_BYTES}}]);
            let params = json!({
                "functionDeclaration": LIBRARY,
                "arguments": arguments,
                "executionContextId": world,
                "returnByValue": false,
                "objectGroup": CUT_GROUP,
            });
            let mut reply: Value = driver.call(&session, "Runtime.callFunctionOn", params)?;
            if let Some(details) = reply.get("exceptionDetails") {
                return Err(failed(thrown(details)));
            }
            let object = reply["result"]["objectId"].take();
            let Some(object) = object.as_str() else {
                return Err(failed("La página devolvió un resultado inesperado."));
            };
            let prepared = PreparedCut {
                token: token(),
                session: session.clone(),
                object: object.to_owned(),
            };
            let text = driver
                .library::<Value>(&session, Some(object), None, "selected", json!([]))
                .and_then(|selection| selected(&selection));
            match text {
                Ok(text) => {
                    let token = prepared.token.clone();
                    driver.cut = Some(prepared);
                    Ok((text, token))
                }
                Err(error) => {
                    driver.cut = Some(prepared);
                    driver.forget_cut();
                    Err(error)
                }
            }
        })
    }

    /// Removes the text a cut was prepared for, if the field, its value and
    /// its selection are all as they were.
    pub fn confirm_cut(&mut self, token: &str) -> Outcome<()> {
        if token.is_empty() || token.len() > 64 || token.chars().any(char::is_control) {
            return Err(invalid("Confirmación de corte no válida"));
        }
        self.run(TIMEOUT, |driver| {
            let prepared = driver.cut.take();
            let Some(prepared) = prepared.filter(|prepared| prepared.token == token) else {
                driver.forget_cut();
                return Err(failed("El corte ya no corresponde a la selección actual."));
            };
            let outcome = driver.library::<Value>(&prepared.session, Some(&prepared.object), None, "cut", json!([]));
            driver.cut = Some(prepared);
            driver.forget_cut();
            match outcome?["error"].as_str() {
                Some(error) => Err(failed(limited(error, 512))),
                None => Ok(()),
            }
        })
    }

    fn forget_cut(&mut self) {
        if let Some(prepared) = self.cut.take() {
            let _ = self.client.post(&prepared.session, "Runtime.releaseObjectGroup", json!({"objectGroup": CUT_GROUP}));
        }
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // Whoever still waits on the page is told it is over.
        self.client.close();
    }
}

fn navigation(error: DriverError) -> DriverError {
    match error.kind {
        ErrorKind::Failed => DriverError::new(ErrorKind::Navigation, error.message),
        _ => error,
    }
}

/// Maps a failure while checking a ref to "stale", unless the browser is gone.
fn closed_or(stale: impl Fn() -> DriverError) -> impl Fn(DriverError) -> DriverError {
    move |error| if error.kind == ErrorKind::Closed { error } else { stale() }
}

/// The first line of what a page script threw.
pub(crate) fn thrown(details: &Value) -> String {
    let exception = &details["exception"];
    // An Error describes itself with its stack; anything else thrown is its
    // own message.
    let message = match (&exception["value"], exception["description"].as_str()) {
        (Value::String(text), _) => text.clone(),
        (_, Some(description)) => description.to_owned(),
        (Value::Null, None) => exception["unserializableValue"]
            .as_str()
            .or(details["text"].as_str())
            .unwrap_or("Uncaught")
            .to_owned(),
        (other, None) => other.to_string(),
    };
    limited(message.lines().next().unwrap_or(""), 2048).to_owned()
}

/// Cuts a value's text at the bound without splitting a character.
fn bounded_result(text: &str, cut: bool) -> Evaluated {
    let kept = limited(text, MAX_EVALUATE_BYTES);
    Evaluated {
        truncated: cut || kept.len() < text.len(),
        text: kept.to_owned(),
    }
}

fn selected(selection: &Value) -> Outcome<String> {
    if let Some(error) = selection["error"].as_str() {
        return Err(failed(limited(error, 512)));
    }
    match selection["text"].as_str() {
        Some(text) if !text.is_empty() && text.len() <= MAX_SELECTION_BYTES && !text.contains('\0') => Ok(text.to_owned()),
        _ => Err(failed("La selección supera el límite de 16 KiB; copiá una parte más pequeña.")),
    }
}

fn png(encoded: &str) -> Outcome<Png> {
    let data = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| failed("Captura base64 no válida"))?;
    // A PNG says its size at a fixed place of its header.
    let word = |at: usize| Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?));
    match (data.starts_with(b"\x89PNG\r\n\x1a\n"), word(16), word(20)) {
        (true, Some(width), Some(height)) => Ok(Png { data, width, height }),
        _ => Err(failed("Captura PNG no válida")),
    }
}

/// 128 unpredictable bits as text. The standard hasher is keyed from the
/// operating system for every `RandomState`.
fn token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let part = || std::collections::hash_map::RandomState::new().build_hasher().finish();
    format!("{:016x}{:016x}", part(), part())
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;
