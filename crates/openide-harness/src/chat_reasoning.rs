//! Chat Completions reasoning state is part of the assistant's tool roundtrip.
//! These fields never replace canonical roles, text, calls or tool results.
use super::*;

const STATE_TYPE: &str = "openide.chat_reasoning.v1";
const MAX_DETAILS: usize = 128;

#[derive(Default)]
pub(super) struct ChatReasoning {
    content: Option<String>,
    details: BTreeMap<usize, Value>,
}

impl ChatReasoning {
    pub(super) fn ingest(
        &mut self,
        delta: &Value,
        bytes: &mut usize,
        events: &mut Vec<ModelStreamEvent>,
    ) -> Result<(), AgentError> {
        if let Some(value) = delta
            .get("reasoning_content")
            .filter(|value| !value.is_null())
        {
            let text = value
                .as_str()
                .ok_or_else(|| AgentError::Model("reasoning_content is not text".into()))?;
            append_assembled_fragment(self.content.get_or_insert_with(String::new), text, bytes)?;
            if !text.is_empty() {
                events.push(ModelStreamEvent::ReasoningDelta {
                    item_id: "chat-reasoning-0".into(),
                    content: text.into(),
                });
            }
        }
        let Some(details) = delta
            .get("reasoning_details")
            .filter(|value| !value.is_null())
        else {
            return Ok(());
        };
        let details = details
            .as_array()
            .ok_or_else(|| AgentError::Model("reasoning_details is not an array".into()))?;
        if details.len() > MAX_DETAILS {
            return Err(AgentError::Limit(
                "too many Chat Completions reasoning blocks".into(),
            ));
        }
        for (position, fragment) in details.iter().enumerate() {
            let object = fragment
                .as_object()
                .ok_or_else(|| AgentError::Model("reasoning block is not an object".into()))?;
            let index = match object.get("index") {
                Some(value) => value
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n < MAX_DETAILS)
                    .ok_or_else(|| AgentError::Model("invalid reasoning block index".into()))?,
                None => position,
            };
            let added = serde_json::to_vec(fragment)
                .map_err(|error| AgentError::Model(error.to_string()))?
                .len();
            *bytes = bytes
                .checked_add(added)
                .filter(|n| *n <= MAX_ASSEMBLED_STREAM_BYTES)
                .ok_or_else(|| {
                    AgentError::Limit("assembled reasoning exceeds the 8 MiB byte limit".into())
                })?;
            let target = self
                .details
                .entry(index)
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .unwrap();
            for (key, value) in object {
                if matches!(key.as_str(), "text" | "summary" | "data" | "signature") {
                    let text = value.as_str().ok_or_else(|| {
                        AgentError::Model("reasoning fragment is not text".into())
                    })?;
                    match target.get_mut(key) {
                        Some(Value::String(current)) => current.push_str(text),
                        None => {
                            target.insert(key.clone(), value.clone());
                        }
                        _ => {
                            return Err(AgentError::Model(
                                "reasoning fragment changed type".into(),
                            ));
                        }
                    }
                } else if target.get(key).is_some_and(|old| old != value) {
                    return Err(AgentError::Model("reasoning block changed identity".into()));
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
            // Only documented public text is presentation data. Opaque data
            // and signatures remain journaled provider state.
            if delta.get("reasoning_content").is_none_or(Value::is_null)
                && matches!(
                    object
                        .get("type")
                        .and_then(Value::as_str)
                        .or_else(|| target.get("type").and_then(Value::as_str)),
                    Some("reasoning.text" | "reasoning.summary")
                )
            {
                for field in ["text", "summary"] {
                    if let Some(text) = object
                        .get(field)
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        events.push(ModelStreamEvent::ReasoningDelta {
                            item_id: format!("chat-reasoning-detail-{index}"),
                            content: text.into(),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn into_items(self) -> Vec<Value> {
        if self.content.is_none() && self.details.is_empty() {
            return Vec::new();
        }
        let mut item = json!({"type": STATE_TYPE});
        if let Some(content) = self.content {
            item["reasoning_content"] = json!(content);
        }
        if !self.details.is_empty() {
            item["reasoning_details"] = Value::Array(self.details.into_values().collect());
        }
        vec![item]
    }
}

pub(super) fn restore(message: &AgentMessage, wire: &mut Value) {
    if message.role != MessageRole::Assistant {
        return;
    }
    let Some(state) = message
        .provider_output_items
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some(STATE_TYPE))
    else {
        return;
    };
    if let Some(content) = state.get("reasoning_content").filter(|v| v.is_string()) {
        wire["reasoning_content"] = content.clone();
    }
    if let Some(details) = state.get("reasoning_details").filter(|v| v.is_array()) {
        wire["reasoning_details"] = details.clone();
    }
}
