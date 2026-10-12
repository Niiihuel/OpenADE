//! What the page reports while nobody is asking: console output, requests,
//! dialogs, navigations, child frames and screencast frames.
//!
//! `Observer` runs on the client's reader thread and folds every event into
//! bounded state the driver reads under a lock. Nothing is queued, so a page
//! that logs or fetches without pause costs a fixed amount of memory. What
//! must be answered at once (a dialog blocks the page until it is, a popup
//! must not linger, a link meant for another tab opens in this one) is
//! answered from here without waiting for a reply.

use crate::cdp::{Client, EventSink, ScreencastFrame};
use crate::stream::StreamSlot;
use base64::Engine;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Entries kept per log since the last navigation.
pub const MAX_EVENTS: usize = 200;
/// Bytes of one console entry's text.
pub const MAX_CONSOLE_TEXT_BYTES: usize = 1024;
/// Bytes of one request's URL.
pub const MAX_REQUEST_URL_BYTES: usize = 512;
/// Bytes of the entries one report returns; the newest that fit.
pub const MAX_REPORT_BYTES: usize = 12 * 1024;
const MAX_LOCATION_BYTES: usize = 512;
const MAX_FAILURE_BYTES: usize = 256;
/// Out-of-process frames and script contexts tracked at once. A page that
/// creates more keeps working; the ones beyond are not addressable.
const MAX_FRAMES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleLevel {
    Error,
    Warning,
    Info,
    Debug,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConsoleEntry {
    pub level: ConsoleLevel,
    /// The console method (`log`, `warning`, `error`…), `pageerror` for an
    /// uncaught exception, or `dialog`.
    #[serde(rename = "type")]
    pub kind: String,
    /// For a dialog: `alert`, `confirm`, `prompt` or `beforeunload`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dialog: Option<String>,
    /// For a dialog: `accepted` or `dismissed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<&'static str>,
    pub text: String,
    /// `url:line:column`, both counted from one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

/// Method, URL, status, type and failure only: never headers or bodies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NetworkEntry {
    pub method: String,
    pub url: String,
    pub resource_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(skip)]
    pub(crate) request_id: String,
}

/// The newest entries that fit the byte budget, oldest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report<T> {
    /// Older entries left out by the budget or already dropped from the log.
    pub omitted: usize,
    pub entries: Vec<T>,
}

#[derive(Debug)]
pub(crate) struct Log<T> {
    entries: VecDeque<T>,
    dropped: usize,
}

impl<T> Default for Log<T> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            dropped: 0,
        }
    }
}

impl<T: Clone + Serialize> Log<T> {
    fn record(&mut self, entry: T) {
        self.entries.push_back(entry);
        if self.entries.len() > MAX_EVENTS {
            self.entries.pop_front();
            self.dropped += 1;
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.dropped = 0;
    }

    pub(crate) fn report(&self, keep: impl Fn(&T) -> bool) -> Report<T> {
        let kept: Vec<&T> = self.entries.iter().filter(|entry| keep(entry)).collect();
        let mut first = kept.len();
        let mut bytes = 0;
        while first > 0 {
            bytes += serde_json::to_vec(kept[first - 1]).map_or(0, |json| json.len()) + 1;
            if bytes > MAX_REPORT_BYTES {
                break;
            }
            first -= 1;
        }
        Report {
            omitted: self.dropped + first,
            entries: kept[first..].iter().map(|entry| (*entry).clone()).collect(),
        }
    }
}

/// The longest prefix of `text` within `limit` bytes that ends on a character.
pub(crate) fn limited(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// How the next JavaScript dialog is answered. Used once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DialogPlan {
    pub accept: bool,
    pub prompt_text: Option<String>,
}

/// A frame rendered by another process, which has its own protocol session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChildSession {
    pub session: String,
    /// The session of the frame that embeds it.
    pub parent: String,
}

#[derive(Debug, Default)]
pub(crate) struct State {
    pub page_session: String,
    pub page_target: String,
    pub main_frame: String,
    /// Where the main frame is, as the page itself reports it.
    pub url: String,
    /// Identifies the main frame's current document.
    pub loader: String,
    /// The document whose `DOMContentLoaded` fired last.
    pub dom_content_loader: String,
    /// Main-frame commits, new documents and same-document ones alike.
    pub commits: u64,
    /// Navigations the page itself asked for in the main frame, and the times
    /// the frame stopped loading: together they tell an action that started
    /// a navigation from one that did not.
    pub requested: u64,
    pub stopped: u64,
    /// Set while the main frame shows Chrome's error page.
    pub unreachable: Option<String>,
    pub console: Log<ConsoleEntry>,
    pub network: Log<NetworkEntry>,
    pub dialog_plan: Option<DialogPlan>,
    /// Out-of-process frames by the id of their root frame.
    pub children: HashMap<String, ChildSession>,
    /// The page's own script world of each frame, by session and frame id.
    pub contexts: HashMap<(String, String), i64>,
    /// The payload of a drag Chrome handed over instead of running it.
    pub drag: Option<Value>,
    /// Why the page can no longer be driven.
    pub gone: Option<String>,
}

pub(crate) struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    pub stream: StreamSlot,
}

impl Shared {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                url: "about:blank".into(),
                ..State::default()
            }),
            changed: Condvar::new(),
            stream: StreamSlot::default(),
        })
    }

    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// Blocks until `ready` yields a value, the page is gone, or `deadline`.
    pub(crate) fn wait<T>(
        &self,
        deadline: Instant,
        mut ready: impl FnMut(&mut State) -> Option<T>,
    ) -> Option<T> {
        let mut state = self.state();
        loop {
            if let Some(value) = ready(&mut state) {
                return Some(value);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || state.gone.is_some() {
                return None;
            }
            state = self
                .changed
                .wait_timeout(state, left.min(Duration::from_millis(250)))
                .unwrap_or_else(|poison| poison.into_inner())
                .0;
        }
    }
}

pub(crate) struct Observer {
    pub shared: Arc<Shared>,
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

impl EventSink for Observer {
    fn event(&mut self, client: &Client, session: &str, method: &str, params: Value) {
        let mut state = self.shared.state();
        match method {
            "Runtime.consoleAPICalled" => state.console.record(console_entry(&params)),
            "Runtime.exceptionThrown" => state.console.record(exception_entry(&params)),
            "Log.entryAdded" => state.console.record(log_entry(&params["entry"])),
            "Network.requestWillBeSent" => state.request(&params),
            "Network.responseReceived" => {
                let status = params["response"]["status"].as_u64().and_then(|status| u16::try_from(status).ok());
                if let Some(entry) = state.entry(text(&params, "requestId")) {
                    entry.status = status;
                }
            }
            "Network.loadingFailed" => {
                if let Some(entry) = state.entry(text(&params, "requestId")) {
                    entry.failure = Some(limited(text(&params, "errorText"), MAX_FAILURE_BYTES).to_owned());
                }
            }
            "Page.javascriptDialogOpening" => {
                // The page is blocked until this is answered, and so is any
                // request the driver is waiting on.
                let plan = state.dialog_plan.take();
                let accept = plan.as_ref().is_some_and(|plan| plan.accept);
                state.console.record(ConsoleEntry {
                    level: ConsoleLevel::Warning,
                    kind: "dialog".into(),
                    dialog: Some(limited(text(&params, "type"), 32).to_owned()),
                    action: Some(if accept { "accepted" } else { "dismissed" }),
                    text: limited(text(&params, "message"), MAX_CONSOLE_TEXT_BYTES).to_owned(),
                    location: None,
                });
                let mut answer = json!({"accept": accept});
                if let Some(prompt) = plan.and_then(|plan| plan.prompt_text).filter(|_| accept) {
                    answer["promptText"] = Value::from(prompt);
                }
                let _ = client.post(session, "Page.handleJavaScriptDialog", answer);
            }
            "Page.frameNavigated" => {
                let frame = &params["frame"];
                if session == state.page_session && frame.get("parentId").is_none() {
                    state.url = format!("{}{}", text(frame, "url"), text(frame, "urlFragment"));
                    state.loader = text(frame, "loaderId").to_owned();
                    state.unreachable = frame.get("unreachableUrl").and_then(Value::as_str).map(str::to_owned);
                    state.commits += 1;
                }
            }
            "Page.navigatedWithinDocument" => {
                if session == state.page_session && text(&params, "frameId") == state.main_frame {
                    state.url = text(&params, "url").to_owned();
                    state.commits += 1;
                }
            }
            "Page.frameRequestedNavigation" | "Page.frameStoppedLoading" => {
                if session == state.page_session && text(&params, "frameId") == state.main_frame {
                    if method == "Page.frameStoppedLoading" {
                        state.stopped += 1;
                    } else {
                        state.requested += 1;
                    }
                }
            }
            "Page.lifecycleEvent" => {
                let main = session == state.page_session && text(&params, "frameId") == state.main_frame;
                if main && text(&params, "name") == "DOMContentLoaded" {
                    state.dom_content_loader = text(&params, "loaderId").to_owned();
                }
            }
            "Runtime.executionContextCreated" => {
                let context = &params["context"];
                let aux = &context["auxData"];
                if let (true, Some(id)) = (aux["isDefault"] == true, context["id"].as_i64()) {
                    let key = (session.to_owned(), text(aux, "frameId").to_owned());
                    if state.contexts.len() < MAX_FRAMES || state.contexts.contains_key(&key) {
                        state.contexts.insert(key, id);
                    }
                }
            }
            "Runtime.executionContextDestroyed" => {
                let id = params["executionContextId"].as_i64();
                state.contexts.retain(|key, context| key.0 != session || Some(*context) != id);
            }
            "Runtime.executionContextsCleared" => state.contexts.retain(|key, _| key.0 != session),
            "Input.dragIntercepted" => state.drag = Some(params["data"].clone()),
            // Announced on the session that auto-attaches; the driver's own
            // attachment to its page is announced on the browser's, and is not this.
            "Target.attachedToTarget" if !session.is_empty() => {
                let child = text(&params, "sessionId");
                let info = &params["targetInfo"];
                if text(info, "type") == "iframe" && state.children.len() < MAX_FRAMES {
                    state.children.insert(
                        text(info, "targetId").to_owned(),
                        ChildSession {
                            session: child.to_owned(),
                            parent: session.to_owned(),
                        },
                    );
                    for (method, params) in child_setup() {
                        let _ = client.post(child, method, params);
                    }
                } else {
                    // Workers and the like are not driven; let them run.
                    let _ = client.post(child, "Runtime.runIfWaitingForDebugger", json!({}));
                    let _ = client.post(session, "Target.detachFromTarget", json!({"sessionId": child}));
                }
            }
            "Target.detachedFromTarget" => {
                let detached = text(&params, "sessionId");
                state.children.retain(|_, child| child.session != detached);
                state.contexts.retain(|key, _| key.0 != detached);
                if detached == state.page_session && !detached.is_empty() {
                    state.gone = Some("La página del navegador se cerró.".into());
                }
            }
            "Page.windowOpen" => {
                // A link for another tab, or a script that opens a window:
                // announced before the browser is asked, with where it goes.
                // What the user did not ask for with a click opens nothing.
                if params["userGesture"] == true {
                    state.open_here(client, text(&params, "url"));
                }
            }
            "Target.targetCreated" => {
                // One panel has one authoritative page. Another window is
                // closed rather than left running where nobody observes it.
                let info = &params["targetInfo"];
                let popup = text(info, "type") == "page"
                    && !state.page_target.is_empty()
                    && text(info, "targetId") != state.page_target;
                if popup {
                    let _ = client.post("", "Target.closeTarget", json!({"targetId": text(info, "targetId")}));
                    // A tab the browser itself made, for a link clicked with
                    // Control, Shift or the middle button, says where it
                    // goes. One a page's script made does not: it was
                    // announced above. The browser makes neither without a
                    // click behind it.
                    state.open_here(client, text(info, "url"));
                }
            }
            "Inspector.targetCrashed" if session == state.page_session => {
                state.gone = Some("La página del navegador dejó de funcionar.".into());
            }
            _ => return,
        }
        drop(state);
        self.shared.changed.notify_all();
    }

    fn screencast_frame(&mut self, client: &Client, session: &str, frame: ScreencastFrame<'_>) {
        // The one decode of the image: from the message into the bytes the consumer takes.
        let Ok(data) = base64::engine::general_purpose::STANDARD.decode(frame.data.as_bytes()) else {
            let _ = client.post(session, "Page.screencastFrameAck", json!({"sessionId": frame.ack}));
            return;
        };
        self.shared.stream.offer(client, session, data, frame.ack, frame.metadata.timestamp);
    }

    fn closed(&mut self) {
        self.shared
            .state()
            .gone
            .get_or_insert_with(|| "El navegador se cerró.".into());
        self.shared.changed.notify_all();
        self.shared.stream.close();
    }
}

/// What a session needs enabled to be observed like the page itself.
pub(crate) fn child_setup() -> [(&'static str, Value); 6] {
    [
        ("Page.enable", json!({})),
        ("Runtime.enable", json!({})),
        ("Network.enable", json!({})),
        ("Log.enable", json!({})),
        (
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
        ),
        ("Runtime.runIfWaitingForDebugger", json!({})),
    ]
}

impl State {
    /// Opens in the page itself an address that was meant for another tab or
    /// window. Only an address of the web: the browser is the one navigating,
    /// which a page's own link could not make it do to a file or a script.
    fn open_here(&mut self, client: &Client, url: &str) {
        if self.page_session.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
            return;
        }
        // As when the page itself asks to navigate: the action that led here
        // waits for the new document.
        self.requested += 1;
        let _ = client.post(&self.page_session, "Page.navigate", json!({"url": url}));
    }

    fn entry(&mut self, request_id: &str) -> Option<&mut NetworkEntry> {
        self.network.entries.iter_mut().rev().find(|entry| entry.request_id == request_id)
    }

    fn request(&mut self, params: &Value) {
        let request_id = text(params, "requestId");
        let redirect = params.get("redirectResponse");
        // A redirect continues the same request: its response is the status
        // of the hop before.
        if let Some(response) = redirect {
            let status = response["status"].as_u64().and_then(|status| u16::try_from(status).ok());
            if let Some(entry) = self.entry(request_id) {
                entry.status = status;
            }
        }
        let kind = text(params, "type");
        let navigation = kind == "Document"
            && redirect.is_none()
            && text(params, "frameId") == self.main_frame
            && request_id == text(params, "loaderId");
        if navigation {
            // A new main document starts both logs over and disarms a dialog
            // answer that never fired, so it cannot reach an unrelated page.
            self.console.clear();
            self.network.clear();
            self.dialog_plan = None;
        }
        let request = &params["request"];
        // An inline resource is not a request anyone made.
        if text(request, "url").starts_with("data:") {
            return;
        }
        let url = format!("{}{}", text(request, "url"), text(request, "urlFragment"));
        self.network.record(NetworkEntry {
            method: limited(text(request, "method"), 16).to_owned(),
            url: limited(&url, MAX_REQUEST_URL_BYTES).to_owned(),
            resource_type: limited(&kind.to_ascii_lowercase(), 32).to_owned(),
            status: None,
            failure: None,
            request_id: limited(request_id, 128).to_owned(),
        });
    }
}

/// Stack traces and editors count lines and columns from one; the protocol
/// counts from zero.
fn location(url: &str, line: &Value, column: &Value) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    let position = format!(
        "{url}:{}:{}",
        line.as_u64().unwrap_or(0) + 1,
        column.as_u64().unwrap_or(0) + 1
    );
    Some(limited(&position, MAX_LOCATION_BYTES).to_owned())
}

fn console_entry(params: &Value) -> ConsoleEntry {
    let kind = text(params, "type");
    let level = match kind {
        "error" | "assert" => ConsoleLevel::Error,
        "warning" => ConsoleLevel::Warning,
        "debug" => ConsoleLevel::Debug,
        _ => ConsoleLevel::Info,
    };
    let mut message = String::new();
    for argument in params["args"].as_array().map_or(&[][..], Vec::as_slice) {
        if message.len() > MAX_CONSOLE_TEXT_BYTES {
            break;
        }
        if !message.is_empty() {
            message.push(' ');
        }
        describe(argument, &mut message);
    }
    let frame = &params["stackTrace"]["callFrames"][0];
    ConsoleEntry {
        level,
        kind: limited(kind, 32).to_owned(),
        dialog: None,
        action: None,
        text: limited(&message, MAX_CONSOLE_TEXT_BYTES).to_owned(),
        location: location(text(frame, "url"), &frame["lineNumber"], &frame["columnNumber"]),
    }
}

/// One logged value the way a console shows it: strings bare, objects by
/// their preview. Only text is kept; the logged objects are never fetched.
fn describe(argument: &Value, out: &mut String) {
    if let Some(value) = argument.get("value") {
        match value {
            Value::String(value) => out.push_str(limited(value, MAX_CONSOLE_TEXT_BYTES + 1)),
            other => out.push_str(&other.to_string()),
        }
        return;
    }
    let preview = &argument["preview"];
    let Some(properties) = preview["properties"].as_array().filter(|_| text(argument, "subtype") != "error") else {
        let fallback = ["unserializableValue", "description", "type"]
            .into_iter()
            .map(|key| text(argument, key))
            .find(|text| !text.is_empty())
            .unwrap_or("");
        out.push_str(limited(fallback, MAX_CONSOLE_TEXT_BYTES + 1));
        return;
    };
    let array = text(argument, "subtype") == "array";
    out.push(if array { '[' } else { '{' });
    for (at, property) in properties.iter().enumerate() {
        if at > 0 {
            out.push_str(", ");
        }
        if !array {
            out.push_str(limited(text(property, "name"), 128));
            out.push_str(": ");
        }
        let value = limited(text(property, "value"), 256);
        if text(property, "type") == "string" {
            out.push('"');
            out.push_str(value);
            out.push('"');
        } else {
            out.push_str(value);
        }
    }
    if preview["overflow"] == true {
        out.push_str(", …");
    }
    out.push(if array { ']' } else { '}' });
}

fn exception_entry(params: &Value) -> ConsoleEntry {
    let details = &params["exceptionDetails"];
    let exception = &details["exception"];
    // An Error carries its stack as description; anything else thrown is its value.
    let message = match exception.get("description").and_then(Value::as_str) {
        Some(description) => description.to_owned(),
        None => {
            let mut message = text(details, "text").to_owned();
            if exception.is_object() {
                message.push(' ');
                describe(exception, &mut message);
            }
            message
        }
    };
    ConsoleEntry {
        level: ConsoleLevel::Error,
        kind: "pageerror".into(),
        dialog: None,
        action: None,
        text: limited(&message, MAX_CONSOLE_TEXT_BYTES).to_owned(),
        location: None,
    }
}

/// A message Chrome itself logs for the page, such as a failed resource.
fn log_entry(entry: &Value) -> ConsoleEntry {
    let kind = text(entry, "level");
    ConsoleEntry {
        level: match kind {
            "error" => ConsoleLevel::Error,
            "warning" => ConsoleLevel::Warning,
            _ => ConsoleLevel::Info,
        },
        kind: limited(kind, 32).to_owned(),
        dialog: None,
        action: None,
        text: limited(text(entry, "text"), MAX_CONSOLE_TEXT_BYTES).to_owned(),
        location: location(text(entry, "url"), &entry["lineNumber"], &Value::Null),
    }
}

#[cfg(test)]
#[path = "observer_tests.rs"]
mod tests;
