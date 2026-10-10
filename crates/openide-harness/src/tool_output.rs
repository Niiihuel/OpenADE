//! Bounded presentation observations. The runner attaches the trusted call ID
//! and journals every accepted chunk before forwarding it to the desktop.
use crate::AgentError;
use openide_core::{AgentEventKind, OutputStream};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const MAX_TOOL_STREAM_BYTES: usize = 128 * 1024;
pub const MAX_TOOL_STREAM_EVENTS: usize = 256;
const CHUNK_BYTES: usize = 4 * 1024;
const TRUNCATED: &str = "\n[tool live output truncated]\n";

#[derive(Clone)]
pub struct ToolOutputSender {
    sender: mpsc::Sender<ToolOutputChunk>,
    cancel: CancellationToken,
    budget: Arc<Mutex<Budget>>,
}

#[derive(Default)]
struct Budget {
    bytes: usize,
    events: usize,
    truncated: bool,
}

pub(crate) struct ToolOutputChunk {
    pub stream: OutputStream,
    pub text: String,
}

impl ToolOutputChunk {
    pub(crate) fn into_event(self, call_id: &str) -> AgentEventKind {
        AgentEventKind::ToolOutputDelta {
            tool_call_id: call_id.to_owned(),
            stream: self.stream,
            text: self.text,
        }
    }
}

pub(crate) fn tool_output_channel(
    cancel: CancellationToken,
) -> (ToolOutputSender, mpsc::Receiver<ToolOutputChunk>) {
    let (sender, receiver) = mpsc::channel(16);
    (
        ToolOutputSender {
            sender,
            cancel,
            budget: Default::default(),
        },
        receiver,
    )
}

impl ToolOutputSender {
    /// Publish display text only. Exhausting the display budget emits one
    /// truncation marker and continues draining the process without blocking.
    /// The authoritative result remains the executor's returned ToolOutput.
    pub async fn send(&self, stream: OutputStream, text: &str) -> Result<(), AgentError> {
        let mut remaining = text;
        while !remaining.is_empty() {
            let (content, consumed) = {
                let mut budget = self
                    .budget
                    .lock()
                    .map_err(|_| AgentError::Runtime("tool output budget is unavailable".into()))?;
                if budget.truncated {
                    return Ok(());
                }
                let available =
                    (MAX_TOOL_STREAM_BYTES - TRUNCATED.len()).saturating_sub(budget.bytes);
                let mut end = CHUNK_BYTES.min(remaining.len()).min(available);
                while !remaining.is_char_boundary(end) {
                    end -= 1;
                }
                if end == 0 || budget.events >= MAX_TOOL_STREAM_EVENTS - 1 {
                    budget.truncated = true;
                    budget.events += 1;
                    budget.bytes += TRUNCATED.len();
                    (TRUNCATED.to_owned(), remaining.len())
                } else {
                    budget.bytes += end;
                    budget.events += 1;
                    (remaining[..end].to_owned(), end)
                }
            };
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Err(AgentError::Cancelled),
                result = self.sender.send(ToolOutputChunk { stream, text: content }) => {
                    result.map_err(|_| AgentError::Runtime("tool output consumer closed".into()))?;
                }
            }
            remaining = &remaining[consumed..];
        }
        Ok(())
    }
}

/// Preserve code points split across pipe reads. At most three incomplete
/// bytes survive a read; invalid bytes are replaced exactly as in final output.
#[derive(Default)]
pub(crate) struct OutputDecoder {
    pending: Vec<u8>,
}

impl OutputDecoder {
    pub(crate) fn decode(&mut self, bytes: &[u8], finish: bool) -> String {
        self.pending.extend_from_slice(bytes);
        let mut output = String::new();
        let mut consumed = 0;
        while consumed < self.pending.len() {
            match std::str::from_utf8(&self.pending[consumed..]) {
                Ok(text) => {
                    output.push_str(text);
                    consumed = self.pending.len();
                }
                Err(error) => {
                    let valid_end = consumed + error.valid_up_to();
                    output
                        .push_str(std::str::from_utf8(&self.pending[consumed..valid_end]).unwrap());
                    consumed = valid_end;
                    match error.error_len() {
                        Some(invalid) => {
                            output.push('\u{fffd}');
                            consumed += invalid;
                        }
                        None if finish => {
                            output.push('\u{fffd}');
                            consumed = self.pending.len();
                        }
                        None => break,
                    }
                }
            }
        }
        self.pending.drain(..consumed);
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_decoder_matches_final_lossy_decode_at_every_fragment_size() {
        let bytes = b"Caf\xc3\xa9 \xf0\x9f\xa6\x80\xff\xe4\xb8\xad\xe2\x82";
        for size in 1..=bytes.len() {
            let mut decoder = OutputDecoder::default();
            let mut text = String::new();
            for chunk in bytes.chunks(size) {
                text.push_str(&decoder.decode(chunk, false));
                assert!(decoder.pending.len() <= 3);
            }
            text.push_str(&decoder.decode(&[], true));
            assert_eq!(text, String::from_utf8_lossy(bytes));
        }
    }

    #[tokio::test]
    async fn output_budget_bounds_many_tiny_events_and_large_unicode_bursts() {
        for tiny in [false, true] {
            let (sender, mut receiver) = tool_output_channel(CancellationToken::new());
            let producer = tokio::spawn(async move {
                let text = if tiny {
                    "x".to_owned()
                } else {
                    "🦀".repeat(8192)
                };
                for _ in 0..512 {
                    sender.send(OutputStream::Stdout, &text).await.unwrap();
                }
            });
            let mut count = 0;
            let mut text = String::new();
            while let Some(chunk) = receiver.recv().await {
                assert!(chunk.text.len() <= CHUNK_BYTES);
                text.push_str(&chunk.text);
                count += 1;
            }
            producer.await.unwrap();
            assert!(count <= MAX_TOOL_STREAM_EVENTS);
            assert!(text.len() <= MAX_TOOL_STREAM_BYTES);
            assert_eq!(text.matches(TRUNCATED).count(), 1);
        }
    }
}
