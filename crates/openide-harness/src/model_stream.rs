//! Bounded presentation events for model inference. A ready tool event does
//! not authorize execution; only the validated completion can do that.

use crate::AgentError;
use openide_core::ModelStreamEvent;
use std::future::Future;
use tokio::sync::mpsc::{self, Receiver, Sender, UnboundedSender};
use tokio_util::sync::CancellationToken;

pub const MODEL_EVENT_CHANNEL_CAPACITY: usize = 32;
pub const MAX_MODEL_EVENT_BYTES: usize = 32 * 1024;

#[derive(Clone)]
pub struct ModelEventSender {
    sender: Sender<ModelStreamEvent>,
    cancel: CancellationToken,
}

pub fn model_event_channel(
    cancel: CancellationToken,
) -> (ModelEventSender, Receiver<ModelStreamEvent>) {
    let (sender, receiver) = mpsc::channel(MODEL_EVENT_CHANNEL_CAPACITY);
    (ModelEventSender { sender, cancel }, receiver)
}

impl ModelEventSender {
    /// An abandoned consumer must release the whole provider operation,
    /// including a stalled connection, retry delay, or stream with no deltas.
    pub(crate) async fn while_open<T>(
        &self,
        operation: impl Future<Output = Result<T, AgentError>>,
    ) -> Result<T, AgentError> {
        tokio::select! {
            biased;
            error = self.interrupted() => Err(error),
            result = operation => result,
        }
    }

    pub(crate) async fn interrupted(&self) -> AgentError {
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => AgentError::Cancelled,
            _ = self.sender.closed() => consumer_closed(),
        }
    }

    /// Apply backpressure before the provider reads another frame. Strings
    /// are split on UTF-8 boundaries; each serialized event fits the byte cap.
    pub async fn send(&self, event: ModelStreamEvent) -> Result<(), AgentError> {
        match event {
            ModelStreamEvent::TextDelta { content } => {
                self.send_fragments(content, |content| ModelStreamEvent::TextDelta { content })
                    .await
            }
            ModelStreamEvent::ReasoningDelta { item_id, content } => {
                check_metadata_bytes(&[Some(&item_id)])?;
                self.send_fragments(content, |content| ModelStreamEvent::ReasoningDelta {
                    item_id: item_id.clone(),
                    content,
                })
                .await
            }
            ModelStreamEvent::ToolCallDelta {
                item_id,
                call_id,
                name,
                arguments_delta,
            } => {
                check_metadata_bytes(&[Some(&item_id), call_id.as_deref(), name.as_deref()])?;
                self.send_fragments(arguments_delta, |arguments_delta| {
                    ModelStreamEvent::ToolCallDelta {
                        item_id: item_id.clone(),
                        call_id: call_id.clone(),
                        name: name.clone(),
                        arguments_delta,
                    }
                })
                .await
            }
            ModelStreamEvent::Started { response_id } => {
                check_metadata_bytes(&[response_id.as_deref()])?;
                self.send_one(ModelStreamEvent::Started { response_id })
                    .await
            }
            ModelStreamEvent::ToolCallReady {
                item_id,
                call_id,
                name,
            } => {
                check_metadata_bytes(&[Some(&item_id), Some(&call_id), Some(&name)])?;
                self.send_one(ModelStreamEvent::ToolCallReady {
                    item_id,
                    call_id,
                    name,
                })
                .await
            }
            event @ ModelStreamEvent::WebSearchUpdate { .. } => {
                let bounded = event.bounded_web_search_update().ok_or_else(|| {
                    AgentError::Model("invalid hosted web search presentation identity".into())
                })?;
                self.send_one(bounded).await
            }
            event => self.send_one(event).await,
        }
    }

    async fn send_fragments(
        &self,
        content: String,
        make_event: impl Fn(String) -> ModelStreamEvent,
    ) -> Result<(), AgentError> {
        let metadata_size = serialized_size(&make_event(String::new()))?;
        // JSON can escape each input byte into at most six bytes. Reserving
        // that expansion also bounds memory while serializing each fragment.
        let budget = MAX_MODEL_EVENT_BYTES.saturating_sub(metadata_size) / 6;
        if budget < 4 {
            return Err(AgentError::Limit(
                "model stream event metadata exceeds the event byte limit".into(),
            ));
        }
        if content.is_empty() {
            return self.send_one(make_event(content)).await;
        }
        let mut remaining = content.as_str();
        while !remaining.is_empty() {
            let mut boundary = budget.min(remaining.len());
            while !remaining.is_char_boundary(boundary) {
                boundary -= 1;
            }
            self.send_one(make_event(remaining[..boundary].to_owned()))
                .await?;
            remaining = &remaining[boundary..];
        }
        Ok(())
    }

    async fn send_one(&self, event: ModelStreamEvent) -> Result<(), AgentError> {
        if serialized_size(&event)? > MAX_MODEL_EVENT_BYTES {
            return Err(AgentError::Limit(format!(
                "model stream event exceeds {MAX_MODEL_EVENT_BYTES} bytes"
            )));
        }
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => Err(AgentError::Cancelled),
            result = self.sender.send(event) => result.map_err(|_| consumer_closed()),
        }
    }
}

fn consumer_closed() -> AgentError {
    AgentError::Runtime("model stream consumer closed its event channel".into())
}

fn serialized_size(event: &ModelStreamEvent) -> Result<usize, AgentError> {
    serde_json::to_vec(event)
        .map(|encoded| encoded.len())
        .map_err(|error| AgentError::Runtime(format!("cannot encode model stream event: {error}")))
}

fn check_metadata_bytes(fields: &[Option<&str>]) -> Result<(), AgentError> {
    let mut bytes = 0usize;
    for field in fields.iter().flatten() {
        bytes = bytes
            .checked_add(field.len())
            .filter(|bytes| *bytes <= MAX_MODEL_EVENT_BYTES)
            .ok_or_else(|| {
                AgentError::Limit("model stream event metadata exceeds the event byte limit".into())
            })?;
    }
    Ok(())
}

/// Legacy callers explicitly opt into their original text-only transport.
/// Native production inference uses Events and never creates an unbounded
/// channel. Frame collectors are released before reading the next frame.
#[derive(Clone)]
pub(crate) enum ModelStreamDelivery {
    Legacy(UnboundedSender<String>),
    Events(ModelEventSender),
}

impl ModelStreamDelivery {
    pub(crate) async fn publish(&self, events: Vec<ModelStreamEvent>) -> Result<(), AgentError> {
        match self {
            Self::Events(sender) => {
                for event in events {
                    sender.send(event).await?;
                }
            }
            Self::Legacy(sender) => publish_legacy(events, sender),
        }
        Ok(())
    }
}

pub(crate) fn publish_legacy(events: Vec<ModelStreamEvent>, sender: &UnboundedSender<String>) {
    for event in events {
        if let ModelStreamEvent::TextDelta { content } = event {
            let _ = sender.send(content);
        }
    }
}

#[cfg(test)]
#[path = "model_stream_tests.rs"]
mod tests;
