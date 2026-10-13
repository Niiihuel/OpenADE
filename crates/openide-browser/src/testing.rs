//! A DevTools endpoint the unit tests script: what the client writes can be
//! read back, and what it reads is whatever the test sends.

use crate::cdp::{Client, EventSink};
use crate::transport::{Incoming, MessageSink, MessageSource, Transport};
use serde_json::{json, Value};
use std::sync::mpsc::{self, Receiver, Sender};

struct Sink(Sender<Value>);
impl MessageSink for Sink {
    fn send(&mut self, message: &[u8]) -> std::io::Result<()> {
        let _ = self.0.send(serde_json::from_slice(message).unwrap());
        Ok(())
    }
}

struct Source(Receiver<Incoming>);
impl MessageSource for Source {
    fn receive(&mut self) -> std::io::Result<Option<Incoming>> {
        Ok(self.0.recv().ok())
    }
}

struct Ignore;
impl EventSink for Ignore {
    fn event(&mut self, _: &Client, _: &str, _: &str, _: Value) {}
    fn closed(&mut self) {}
}

/// Both ends of a transport: the first is the driver's, the others the test's.
pub(crate) fn transport() -> (Transport, Receiver<Value>, Sender<Incoming>) {
    let (written, read) = mpsc::channel();
    let (incoming, source) = mpsc::channel();
    let transport = Transport {
        sink: Box::new(Sink(written)),
        source: Box::new(Source(source)),
    };
    (transport, read, incoming)
}

/// A client that ignores events. Dropping the sender ends its transport.
pub(crate) fn client() -> (Client, Receiver<Value>, Sender<Incoming>) {
    let (transport, written, incoming) = transport();
    (Client::connect(transport, Box::new(Ignore)).unwrap(), written, incoming)
}

pub(crate) fn message(value: Value) -> Incoming {
    Incoming {
        bytes: serde_json::to_vec(&value).unwrap(),
        oversized: false,
    }
}

/// What `serve` takes as "the browser dies now".
pub(crate) const HANG_UP: &str = "hang up";

/// Answers every request the client writes with what `answer` returns for
/// its method and parameters, until the client goes away. An `Err` is sent
/// back as a protocol error, except `HANG_UP`, which ends the endpoint;
/// `None` leaves the request unanswered.
pub(crate) fn serve(
    written: Receiver<Value>,
    incoming: Sender<Incoming>,
    mut answer: impl FnMut(&str, &Value) -> Option<Result<Value, &'static str>> + Send + 'static,
) {
    std::thread::spawn(move || {
        for request in written {
            let reply = match answer(request["method"].as_str().unwrap_or(""), &request["params"]) {
                Some(Ok(result)) => json!({"id": request["id"], "result": result, "sessionId": request["sessionId"]}),
                Some(Err(HANG_UP)) => break,
                Some(Err(error)) => json!({"id": request["id"], "error": {"code": -32000, "message": error}}),
                None => continue,
            };
            if incoming.send(message(reply)).is_err() {
                break;
            }
        }
    });
}
