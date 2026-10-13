//! The message transport between the driver and a DevTools endpoint.
//!
//! A transport moves whole JSON messages and knows nothing about the protocol.
//! Whoever embeds Chromium implements the two ends: it exchanges the same
//! messages in-process, and nothing listens on a socket.

use std::io;

/// Largest message kept in memory. A longer one is handed over as its first
/// bytes and marked oversized, so one huge reply cannot exhaust memory.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

/// One message from the endpoint.
#[derive(Debug, PartialEq, Eq)]
pub struct Incoming {
    /// The whole message, or only its first bytes when `oversized`.
    pub bytes: Vec<u8>,
    pub oversized: bool,
}

/// Sends one complete JSON message.
pub trait MessageSink: Send {
    fn send(&mut self, message: &[u8]) -> io::Result<()>;
}

/// Blocks for the next message. `Ok(None)` is the orderly end of the endpoint.
/// An implementation never returns more than `MAX_MESSAGE_BYTES`.
pub trait MessageSource: Send {
    fn receive(&mut self) -> io::Result<Option<Incoming>>;
}

/// The two directions are separate objects because a reader thread owns the
/// source while callers share the sink.
pub struct Transport {
    pub sink: Box<dyn MessageSink>,
    pub source: Box<dyn MessageSource>,
}
