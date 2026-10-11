//! The message transport between the driver and a DevTools endpoint.
//!
//! A transport moves whole JSON messages and knows nothing about the protocol.
//! Chrome's `--remote-debugging-pipe` is the one implemented here: commands
//! go to the browser's descriptor 3 and replies and events come from its
//! descriptor 4, each message terminated by a NUL byte. Nothing listens on a
//! socket. An embedded Chromium exchanges the same messages in-process and
//! only needs its own `MessageSink` and `MessageSource`.

use std::io::{self, Read, Write};

/// Largest message kept in memory. A longer one is consumed and reported as
/// oversized, so one huge reply can neither exhaust memory nor desynchronise
/// the stream.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
/// Leading bytes kept from an oversized message: enough to find its `id`.
const OVERSIZED_PREFIX: usize = 256;
const READ_CHUNK: usize = 64 * 1024;

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

impl Transport {
    /// NUL-delimited JSON over a byte stream in each direction.
    pub fn nul_delimited(
        writer: impl Write + Send + 'static,
        reader: impl Read + Send + 'static,
    ) -> Self {
        Self {
            sink: Box::new(NulSink(writer)),
            source: Box::new(NulSource::new(reader, MAX_MESSAGE_BYTES)),
        }
    }
}

pub(crate) struct NulSink<W>(pub W);

impl<W: Write + Send> MessageSink for NulSink<W> {
    fn send(&mut self, message: &[u8]) -> io::Result<()> {
        // JSON escapes NUL, so a raw one could only split the record.
        if message.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a DevTools message cannot contain a NUL byte",
            ));
        }
        let mut record = Vec::with_capacity(message.len() + 1);
        record.extend_from_slice(message);
        record.push(0);
        self.0.write_all(&record)?;
        self.0.flush()
    }
}

pub(crate) struct NulSource<R> {
    reader: R,
    /// Where each read lands before the bytes join the message.
    scratch: Box<[u8]>,
    buffer: Vec<u8>,
    /// Start of the message being assembled.
    start: usize,
    /// Bytes before this offset hold no terminator.
    scanned: usize,
    limit: usize,
    skipping: bool,
}

impl<R> NulSource<R> {
    pub(crate) fn new(reader: R, limit: usize) -> Self {
        Self {
            reader,
            scratch: vec![0; READ_CHUNK].into_boxed_slice(),
            buffer: Vec::new(),
            start: 0,
            scanned: 0,
            limit: limit.max(OVERSIZED_PREFIX),
            skipping: false,
        }
    }
}

impl<R: Read + Send> MessageSource for NulSource<R> {
    fn receive(&mut self) -> io::Result<Option<Incoming>> {
        loop {
            if let Some(offset) = self.buffer[self.scanned..].iter().position(|byte| *byte == 0) {
                let end = self.scanned + offset;
                let oversized = std::mem::take(&mut self.skipping);
                let bytes = if oversized {
                    self.buffer[self.start..self.start + OVERSIZED_PREFIX].to_vec()
                } else if self.start == 0 && end + 1 == self.buffer.len() {
                    // The buffer is exactly this message: hand it over instead of copying it.
                    let mut bytes = std::mem::take(&mut self.buffer);
                    bytes.pop();
                    self.scanned = 0;
                    return Ok(Some(Incoming {
                        bytes,
                        oversized: false,
                    }));
                } else {
                    self.buffer[self.start..end].to_vec()
                };
                self.start = end + 1;
                self.scanned = self.start;
                return Ok(Some(Incoming { bytes, oversized }));
            }
            // Nothing complete is left: drop what was consumed before reading on.
            if self.start > 0 {
                self.buffer.drain(..self.start);
                self.start = 0;
            }
            if self.buffer.len() > self.limit {
                self.skipping = true;
            }
            if self.skipping {
                self.buffer.truncate(OVERSIZED_PREFIX);
            }
            self.scanned = self.buffer.len();
            let count = loop {
                match self.reader.read(&mut self.scratch) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    other => break other?,
                }
            };
            self.buffer.extend_from_slice(&self.scratch[..count]);
            if count == 0 {
                return if self.buffer.is_empty() {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the DevTools endpoint closed in the middle of a message",
                    ))
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Hands out at most `step` bytes per read, as a pipe may.
    struct Trickle(Cursor<Vec<u8>>, usize);
    impl Read for Trickle {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let step = self.1.min(buffer.len());
            self.0.read(&mut buffer[..step])
        }
    }

    fn source(bytes: &[u8], step: usize, limit: usize) -> NulSource<Trickle> {
        NulSource::new(Trickle(Cursor::new(bytes.to_vec()), step), limit)
    }

    fn messages(source: &mut NulSource<Trickle>) -> Vec<(String, bool)> {
        let mut seen = Vec::new();
        while let Some(incoming) = source.receive().unwrap() {
            seen.push((String::from_utf8(incoming.bytes).unwrap(), incoming.oversized));
        }
        seen
    }

    #[test]
    fn records_are_split_at_nul_whatever_the_read_boundaries() {
        let stream = "{\"id\":1}\0{\"método\":\"ñ🧠\"}\0\0{}\0".as_bytes();
        for step in [1, 2, 3, 7, 4096] {
            assert_eq!(
                messages(&mut source(stream, step, 1024)),
                [
                    ("{\"id\":1}".to_owned(), false),
                    ("{\"método\":\"ñ🧠\"}".to_owned(), false),
                    (String::new(), false),
                    ("{}".to_owned(), false),
                ],
                "step {step}"
            );
        }
    }

    #[test]
    fn a_sink_writes_one_terminated_record_and_refuses_an_embedded_nul() {
        let mut sink = NulSink(Vec::new());
        sink.send(b"{\"id\":1}").unwrap();
        sink.send(b"{}").unwrap();
        assert_eq!(sink.0, b"{\"id\":1}\0{}\0");
        assert_eq!(
            sink.send(b"{\"a\":\0}").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(sink.0, b"{\"id\":1}\0{}\0", "nothing was written");
    }

    #[test]
    fn an_oversized_message_is_consumed_and_keeps_only_its_head() {
        let limit = 1024;
        let mut stream = format!("{{\"id\":42,\"result\":\"{}\"}}", "x".repeat(300_000)).into_bytes();
        stream.push(0);
        stream.extend_from_slice(b"{\"id\":43}\0");
        for step in [5, 1000, 70_000] {
            let mut source = source(&stream, step, limit);
            let first = source.receive().unwrap().unwrap();
            assert!(first.oversized);
            assert_eq!(first.bytes.len(), OVERSIZED_PREFIX);
            assert!(first.bytes.starts_with(b"{\"id\":42,"));
            assert!(
                source.buffer.capacity() < limit + 4 * READ_CHUNK,
                "the oversized body is never accumulated"
            );
            assert_eq!(messages(&mut source), [("{\"id\":43}".to_owned(), false)]);
        }
    }

    #[test]
    fn end_of_stream_is_orderly_only_between_messages() {
        assert!(source(b"", 8, 1024).receive().unwrap().is_none());
        let mut truncated = source(b"{}\0{\"id\"", 8, 1024);
        assert_eq!(truncated.receive().unwrap().unwrap().bytes, b"{}");
        assert_eq!(
            truncated.receive().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
