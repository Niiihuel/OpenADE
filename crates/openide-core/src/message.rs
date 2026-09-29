use crate::{validate_user_images, UserImage, UserImageError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// The execution contract chosen when a turn is admitted. Old requests and
/// event logs without a mode retain the existing build behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnMode {
    Plan,
    #[default]
    Build,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub role: MessageRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Original provider output items, including opaque reasoning needed for
    /// stateless continuation. Persisted only for assistant messages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_output_items: Vec<Value>,
    /// Self-contained image inputs. Empty on old serialized messages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<UserImage>,
}

impl AgentMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_output_items: Vec::new(),
            images: Vec::new(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_output_items: Vec::new(),
            images: Vec::new(),
        }
    }

    pub fn user_with_images(
        content: impl Into<String>,
        images: Vec<UserImage>,
    ) -> Result<Self, UserImageError> {
        validate_user_images(&images)?;
        let mut message = Self::user(content);
        message.images = images;
        Ok(message)
    }

    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
            provider_output_items: Vec::new(),
            images: Vec::new(),
        }
    }

    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            provider_output_items: Vec::new(),
            images: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentRunRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default)]
    pub sequence_start: u64,
    pub workspace: PathBuf,
    pub history: Vec<AgentMessage>,
    pub prompt: String,
    #[serde(default)]
    pub mode: TurnMode,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn old_run_requests_default_to_build_mode() {
        let value = json!({
            "session_id": "session-1",
            "sequence_start": 0,
            "workspace": "/workspace",
            "history": [],
            "prompt": "hello"
        });
        let request: AgentRunRequest = serde_json::from_value(value).unwrap();
        assert_eq!(request.mode, TurnMode::Build);
    }

    #[test]
    fn old_user_messages_default_to_no_images() {
        let value = json!({
            "role": "user",
            "content": "legacy text",
            "tool_calls": [],
            "tool_call_id": null,
            "provider_output_items": []
        });
        let message: AgentMessage = serde_json::from_value(value).unwrap();
        assert_eq!(message, AgentMessage::user("legacy text"));
        assert!(message.images.is_empty());
    }
}
