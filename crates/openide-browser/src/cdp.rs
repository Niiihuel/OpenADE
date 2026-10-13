//! A thin DevTools protocol client: send a method with parameters on a
//! session, wait for its result, and receive events.
//!
//! Callers block on their own thread; one reader thread routes replies to the
//! request that waits for them and hands events to a single `EventSink`.
//! Events are consumed where they arrive instead of being queued, so a page
//! that floods the protocol cannot grow memory while nobody is asking. A
//! reply is parsed once, by its caller and into the type it asked for, so a
//! large one never holds up the events behind it.

use crate::transport::{MessageSink, Transport};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CdpError {
    /// The endpoint answered the request with an error.
    Protocol { code: i64, message: String },
    /// No answer within the caller's deadline. The request may still take effect.
    Timeout,
    /// The answer was larger than the transport keeps.
    Oversized,
    /// The transport ended; no further request can succeed.
    Closed,
}

impl fmt::Display for CdpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol { message, .. } => formatter.write_str(message),
            Self::Timeout => formatter.write_str("the browser did not answer in time"),
            Self::Oversized => formatter.write_str("the browser's answer exceeds the message limit"),
            Self::Closed => formatter.write_str("the browser connection is closed"),
        }
    }
}

impl std::error::Error for CdpError {}

/// Receives everything that is not a reply, on the reader thread. It must not
/// block: use `Client::post` to answer the browser from here, never `call`.
pub trait EventSink: Send {
    fn event(&mut self, client: &Client, session: &str, method: &str, params: Value);
    /// The transport ended. Called once, after every waiting request failed.
    fn closed(&mut self);
}

/// The raw message of a reply, or why there is none.
type Reply = Result<Vec<u8>, CdpError>;

struct Inner {
    sink: Mutex<Option<Box<dyn MessageSink>>>,
    pending: Mutex<HashMap<u64, SyncSender<Reply>>>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

impl Client {
    /// Starts the reader thread. It ends when the transport does.
    pub fn connect(transport: Transport, mut events: Box<dyn EventSink>) -> std::io::Result<Self> {
        let client = Self {
            inner: Arc::new(Inner {
                sink: Mutex::new(Some(transport.sink)),
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                closed: AtomicBool::new(false),
            }),
        };
        let reader = client.clone();
        let mut source = transport.source;
        std::thread::Builder::new()
            .name("browser-cdp".into())
            .spawn(move || {
                while let Ok(Some(incoming)) = source.receive() {
                    match (leading_id(&incoming.bytes), incoming.oversized) {
                        (Some(id), true) => reader.resolve(id, Err(CdpError::Oversized)),
                        (Some(id), false) => reader.resolve(id, Ok(incoming.bytes)),
                        (None, true) => {}
                        (None, false) => reader.dispatch(incoming.bytes, events.as_mut()),
                    }
                }
                reader.close();
                events.closed();
            })?;
        Ok(client)
    }

    /// Sends `method` and blocks until its result, an error or `timeout`.
    /// An empty `session` addresses the browser itself.
    pub fn call<T: DeserializeOwned>(
        &self,
        session: &str,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<T, CdpError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::sync_channel(1);
        lock(&self.inner.pending).insert(id, sender);
        if let Err(error) = self.write(id, session, method, params) {
            lock(&self.inner.pending).remove(&id);
            return Err(error);
        }
        match receiver.recv_timeout(timeout) {
            Ok(reply) => decode(&reply?),
            Err(RecvTimeoutError::Timeout) => {
                // A late answer finds no waiter and is dropped.
                lock(&self.inner.pending).remove(&id);
                Err(CdpError::Timeout)
            }
            Err(RecvTimeoutError::Disconnected) => Err(CdpError::Closed),
        }
    }

    /// Sends `method` without waiting for its result. This is how the reader
    /// thread answers the browser (a dialog, a link meant for another tab).
    pub fn post(&self, session: &str, method: &str, params: Value) -> Result<(), CdpError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.write(id, session, method, params)
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    /// Drops the sending half and fails every waiting request.
    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::Release);
        lock(&self.inner.sink).take();
        // Dropping the senders wakes each waiter with `Closed`.
        lock(&self.inner.pending).clear();
    }

    fn write(&self, id: u64, session: &str, method: &str, params: Value) -> Result<(), CdpError> {
        let mut message = json!({"id": id, "method": method, "params": params});
        if !session.is_empty() {
            message["sessionId"] = Value::from(session);
        }
        let bytes = serde_json::to_vec(&message).map_err(|_| CdpError::Closed)?;
        let mut sink = lock(&self.inner.sink);
        let Some(writer) = sink.as_mut() else {
            return Err(CdpError::Closed);
        };
        if writer.send(&bytes).is_err() {
            sink.take();
            self.inner.closed.store(true, Ordering::Release);
            return Err(CdpError::Closed);
        }
        Ok(())
    }

    fn resolve(&self, id: u64, reply: Reply) {
        if let Some(waiter) = lock(&self.inner.pending).remove(&id) {
            let _ = waiter.try_send(reply);
        }
    }

    /// Everything that does not begin the way Chrome begins a reply.
    fn dispatch(&self, bytes: Vec<u8>, events: &mut dyn EventSink) {
        let repaired;
        let message: Message = match serde_json::from_slice(&bytes) {
            Ok(message) => message,
            Err(_) => {
                repaired = repair_surrogates(&bytes);
                match serde_json::from_slice(&repaired) {
                    Ok(message) => message,
                    Err(_) => return,
                }
            }
        };
        match (message.id, message.method) {
            // A reply from an endpoint that orders its keys differently.
            (Some(id), _) => self.resolve(id, Ok(bytes)),
            (None, Some(method)) => events.event(
                self,
                message.session.as_deref().unwrap_or(""),
                &method,
                message.params.unwrap_or(Value::Null),
            ),
            (None, None) => {}
        }
    }
}

#[derive(Deserialize)]
#[serde(bound = "T: DeserializeOwned")]
struct ReplyBody<T> {
    result: Option<T>,
    error: Option<ErrorBody>,
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CdpError> {
    let invalid = |_| CdpError::Protocol {
        code: 0,
        message: "the browser sent an answer that is not valid JSON".to_owned(),
    };
    // Page text reaches the protocol unchecked, and an unpaired UTF-16
    // surrogate in it is not valid JSON for a Rust string.
    let body: ReplyBody<T> = serde_json::from_slice(bytes)
        .or_else(|_| serde_json::from_slice(&repair_surrogates(bytes)))
        .map_err(invalid)?;
    match (body.error, body.result) {
        (Some(error), _) => Err(CdpError::Protocol {
            code: error.code,
            message: error.message,
        }),
        (None, Some(result)) => Ok(result),
        (None, None) => serde_json::from_value(Value::Null).map_err(invalid),
    }
}

#[derive(Deserialize)]
struct Message<'a> {
    id: Option<u64>,
    #[serde(borrow)]
    method: Option<Cow<'a, str>>,
    params: Option<Value>,
    #[serde(borrow, rename = "sessionId")]
    session: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

/// The `id` of a reply that could not be parsed whole. Chrome writes it first.
fn leading_id(bytes: &[u8]) -> Option<u64> {
    let digits = bytes.strip_prefix(b"{\"id\":")?;
    let end = digits.iter().position(|byte| !byte.is_ascii_digit())?;
    std::str::from_utf8(&digits[..end]).ok()?.parse().ok()
}

/// Rewrites every `\uD800`–`\uDFFF` escape that is not half of a valid pair
/// as `\uFFFD`. Only escapes are touched, so the result is the same JSON.
fn repair_surrogates(bytes: &[u8]) -> Vec<u8> {
    fn surrogate(bytes: &[u8], at: usize) -> Option<u16> {
        let escape = bytes.get(at..at + 6)?;
        if escape[0] != b'\\' || escape[1] != b'u' {
            return None;
        }
        let value = u16::from_str_radix(std::str::from_utf8(&escape[2..]).ok()?, 16).ok()?;
        (0xD800..=0xDFFF).contains(&value).then_some(value)
    }
    let mut repaired = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            repaired.push(bytes[index]);
            index += 1;
            continue;
        }
        match surrogate(bytes, index) {
            Some(high) if high < 0xDC00 => {
                if surrogate(bytes, index + 6).is_some_and(|low| low >= 0xDC00) {
                    repaired.extend_from_slice(&bytes[index..index + 12]);
                    index += 12;
                } else {
                    repaired.extend_from_slice(b"\\ufffd");
                    index += 6;
                }
            }
            Some(_) => {
                repaired.extend_from_slice(b"\\ufffd");
                index += 6;
            }
            // Any other escape is two bytes; copying both keeps `\\u` literal.
            None => {
                repaired.extend_from_slice(&bytes[index..(index + 2).min(bytes.len())]);
                index += 2;
            }
        }
    }
    repaired
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{Incoming, MessageSource};
    use std::sync::mpsc::{Receiver, Sender};

    /// An endpoint driven by the test: it sees what the client wrote and
    /// decides what the client reads.
    struct Endpoint {
        written: Receiver<Value>,
        incoming: Sender<Incoming>,
    }

    struct Sink(Sender<Value>);
    impl MessageSink for Sink {
        fn send(&mut self, message: &[u8]) -> std::io::Result<()> {
            self.0
                .send(serde_json::from_slice(message).unwrap())
                .map_err(|_| std::io::ErrorKind::BrokenPipe.into())
        }
    }
    struct Source(Receiver<Incoming>);
    impl MessageSource for Source {
        fn receive(&mut self) -> std::io::Result<Option<Incoming>> {
            Ok(self.0.recv().ok())
        }
    }

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<String>>>);
    impl EventSink for Recorder {
        fn event(&mut self, client: &Client, session: &str, method: &str, params: Value) {
            lock(&self.0).push(format!("{session}|{method}|{params}"));
            if method == "Page.javascriptDialogOpening" {
                client
                    .post(session, "Page.handleJavaScriptDialog", json!({"accept": false}))
                    .unwrap();
            }
        }
        fn closed(&mut self) {
            lock(&self.0).push("closed".into());
        }
    }

    fn connect() -> (Client, Endpoint, Recorder) {
        let (written_sender, written) = mpsc::channel();
        let (incoming, incoming_receiver) = mpsc::channel();
        let recorder = Recorder::default();
        let client = Client::connect(
            Transport {
                sink: Box::new(Sink(written_sender)),
                source: Box::new(Source(incoming_receiver)),
            },
            Box::new(recorder.clone()),
        )
        .unwrap();
        (client, Endpoint { written, incoming }, recorder)
    }

    impl Endpoint {
        fn send(&self, text: &str) {
            self.incoming
                .send(Incoming {
                    bytes: text.as_bytes().to_vec(),
                    oversized: false,
                })
                .unwrap();
        }
    }

    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn replies_reach_the_request_that_asked_whatever_their_order() {
        let (client, endpoint, _) = connect();
        let workers: Vec<_> = ["first", "second", "third"]
            .into_iter()
            .map(|label| {
                let client = client.clone();
                std::thread::spawn(move || client.call::<Value>("S", "Echo", json!({"label": label}), WAIT))
            })
            .collect();
        let mut requests: Vec<Value> = (0..3).map(|_| endpoint.written.recv().unwrap()).collect();
        assert!(requests.iter().all(|request| request["sessionId"] == "S"));
        // Answer in the opposite order of arrival.
        requests.reverse();
        for request in &requests {
            endpoint.send(&format!(
                "{{\"id\":{},\"result\":{{\"label\":{}}},\"sessionId\":\"S\"}}",
                request["id"], request["params"]["label"]
            ));
        }
        for (worker, label) in workers.into_iter().zip(["first", "second", "third"]) {
            assert_eq!(worker.join().unwrap().unwrap()["label"], label);
        }
    }

    #[test]
    fn protocol_errors_timeouts_and_late_answers_are_told_apart() {
        let (client, endpoint, _) = connect();
        let failing = {
            let client = client.clone();
            std::thread::spawn(move || client.call::<Value>("", "Nope", json!({}), WAIT))
        };
        let request = endpoint.written.recv().unwrap();
        assert!(request.get("sessionId").is_none(), "the browser session has no id");
        endpoint.send(&format!(
            "{{\"id\":{},\"error\":{{\"code\":-32601,\"message\":\"'Nope' wasn't found\"}}}}",
            request["id"]
        ));
        assert_eq!(
            failing.join().unwrap(),
            Err(CdpError::Protocol {
                code: -32601,
                message: "'Nope' wasn't found".into()
            })
        );

        let timeout = client.call::<Value>("", "Slow", json!({}), Duration::from_millis(30));
        assert_eq!(timeout, Err(CdpError::Timeout));
        let slow = endpoint.written.recv().unwrap();
        assert!(lock(&client.inner.pending).is_empty(), "no waiter is left behind");
        // The answer that arrives after the deadline is dropped, not misdelivered.
        endpoint.send(&format!("{{\"id\":{},\"result\":{{}}}}", slow["id"]));
        let next = {
            let client = client.clone();
            std::thread::spawn(move || client.call::<Value>("", "Next", json!({}), WAIT))
        };
        let request = endpoint.written.recv().unwrap();
        endpoint.send(&format!("{{\"id\":{},\"result\":{{\"fresh\":true}}}}", request["id"]));
        assert_eq!(next.join().unwrap().unwrap()["fresh"], true);
    }

    #[test]
    fn events_are_delivered_in_order_and_the_sink_can_answer_without_waiting() {
        let (client, endpoint, recorder) = connect();
        endpoint.send("{\"method\":\"Page.frameNavigated\",\"params\":{\"n\":1},\"sessionId\":\"S\"}");
        endpoint.send("{\"method\":\"Page.javascriptDialogOpening\",\"params\":{},\"sessionId\":\"S\"}");
        // An event is one whatever the order of its keys.
        endpoint.send("{\"sessionId\":\"S\",\"method\":\"Page.windowOpen\",\"params\":null}");
        let answer = endpoint.written.recv_timeout(WAIT).unwrap();
        assert_eq!(answer["method"], "Page.handleJavaScriptDialog");
        assert_eq!(answer["sessionId"], "S");
        // A request made afterwards proves the reader thread is not blocked.
        let call = {
            let client = client.clone();
            std::thread::spawn(move || client.call::<Value>("S", "Ping", json!({}), WAIT))
        };
        let request = endpoint.written.recv().unwrap();
        endpoint.send(&format!("{{\"id\":{},\"result\":{{}}}}", request["id"]));
        call.join().unwrap().unwrap();
        assert_eq!(
            *lock(&recorder.0),
            [
                "S|Page.frameNavigated|{\"n\":1}",
                "S|Page.javascriptDialogOpening|{}",
                "S|Page.windowOpen|null",
            ]
        );
    }

    #[test]
    fn the_end_of_the_transport_fails_waiting_and_later_requests() {
        let (client, endpoint, recorder) = connect();
        let waiting = {
            let client = client.clone();
            std::thread::spawn(move || client.call::<Value>("", "Never", json!({}), WAIT))
        };
        endpoint.written.recv().unwrap();
        drop(endpoint.incoming);
        assert_eq!(waiting.join().unwrap(), Err(CdpError::Closed));
        assert_eq!(client.call::<Value>("", "After", json!({}), WAIT), Err(CdpError::Closed));
        assert_eq!(client.post("", "After", json!({})), Err(CdpError::Closed));
        assert!(client.is_closed());
        let deadline = std::time::Instant::now() + WAIT;
        while lock(&recorder.0).last().map(String::as_str) != Some("closed") {
            assert!(std::time::Instant::now() < deadline, "the sink hears about the end");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn an_oversized_or_malformed_reply_fails_only_its_own_request() {
        let (client, endpoint, _) = connect();
        for (bytes, oversized, expected) in [
            (b"{\"id\":ID,\"result\":{\"data\":\"AAAA".to_vec(), true, CdpError::Oversized),
            (
                b"{\"id\":ID,\"result\":{\"broken\":}}".to_vec(),
                false,
                CdpError::Protocol {
                    code: 0,
                    message: "the browser sent an answer that is not valid JSON".into(),
                },
            ),
        ] {
            let call = {
                let client = client.clone();
                std::thread::spawn(move || client.call::<Value>("", "Big", json!({}), WAIT))
            };
            let id = endpoint.written.recv().unwrap()["id"].to_string();
            let text = String::from_utf8(bytes).unwrap().replace("ID", &id);
            endpoint
                .incoming
                .send(Incoming {
                    bytes: text.into_bytes(),
                    oversized,
                })
                .unwrap();
            assert_eq!(call.join().unwrap(), Err(expected));
        }
        assert!(!client.is_closed(), "the connection survives");
    }

    #[test]
    fn unpaired_surrogates_from_page_text_do_not_lose_the_message() {
        let (client, endpoint, _) = connect();
        let call = {
            let client = client.clone();
            std::thread::spawn(move || client.call::<Value>("", "Title", json!({}), WAIT))
        };
        let id = endpoint.written.recv().unwrap()["id"].clone();
        endpoint.send(&format!(
            "{{\"id\":{id},\"result\":{{\"lone\":\"a\\ud83eb\",\"low\":\"\\udde0\",\"pair\":\"\\ud83e\\udde0\",\"literal\":\"\\\\ud83e\",\"end\":\"\\ud83e\"}}}}"
        ));
        let result = call.join().unwrap().unwrap();
        assert_eq!(result["lone"], "a\u{fffd}b");
        assert_eq!(result["low"], "\u{fffd}");
        assert_eq!(result["pair"], "🧠");
        assert_eq!(result["literal"], "\\ud83e", "an escaped backslash is not an escape");
        assert_eq!(result["end"], "\u{fffd}");
    }

    #[test]
    fn the_leading_id_is_read_only_from_where_chrome_writes_it() {
        assert_eq!(leading_id(b"{\"id\":17,\"result\":{"), Some(17));
        assert_eq!(leading_id(b"{\"id\":17"), None, "an id cut short is not trusted");
        assert_eq!(leading_id(b"{\"method\":\"x\",\"id\":17}"), None);
        assert_eq!(leading_id(b"{\"id\":\"17\"}"), None);
    }
}
