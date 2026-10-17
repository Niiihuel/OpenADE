//! The parts of the Agent Client Protocol (version 1) a client needs, as in
//! its published `schema.json`. Agents add variants and fields of their own,
//! so every enum has a catch-all and every optional field a default: a
//! message the client does not understand is skipped, never fatal.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: u16,
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
    #[serde(default)]
    pub agent_info: Option<Implementation>,
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
    /// Extensions, such as the models and efforts Grok lists here.
    #[serde(default, rename = "_meta")]
    pub meta: Option<Value>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentCapabilities {
    pub load_session: bool,
    pub prompt_capabilities: PromptCapabilities,
    pub mcp_capabilities: McpCapabilities,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PromptCapabilities {
    pub image: bool,
    pub audio: bool,
    pub embedded_context: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct McpCapabilities {
    pub http: bool,
    pub sse: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Implementation {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// `terminal` methods run a login command the client launches itself;
    /// absent means the agent signs in on `authenticate`.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    pub session_id: String,
    #[serde(default)]
    pub modes: Option<SessionModeState>,
    #[serde(default)]
    pub config_options: Option<Vec<ConfigOption>>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoadSessionResponse {
    #[serde(default)]
    pub modes: Option<SessionModeState>,
    #[serde(default)]
    pub config_options: Option<Vec<ConfigOption>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionModeState {
    pub current_mode_id: String,
    #[serde(default)]
    pub available_modes: Vec<SessionMode>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SessionMode {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// A setting of the session the agent lets the client change: its model,
/// its reasoning level, a mode.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// `mode`, `model`, `model_config`, `thought_level` or the agent's own.
    #[serde(default)]
    pub category: Option<String>,
    #[serde(flatten)]
    pub value: ConfigValue,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfigValue {
    #[serde(rename_all = "camelCase")]
    Select {
        current_value: String,
        #[serde(default)]
        options: SelectOptions,
    },
    #[serde(rename_all = "camelCase")]
    Boolean { current_value: bool },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum SelectOptions {
    Grouped(Vec<SelectGroup>),
    Flat(Vec<SelectOption>),
}

impl Default for SelectOptions {
    fn default() -> Self {
        Self::Flat(Vec::new())
    }
}

impl SelectOptions {
    /// Every choice, groups flattened in order.
    pub fn all(&self) -> Vec<&SelectOption> {
        match self {
            Self::Flat(options) => options.iter().collect(),
            Self::Grouped(groups) => groups.iter().flat_map(|group| &group.options).collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SelectGroup {
    pub group: String,
    pub name: String,
    #[serde(default)]
    pub options: Vec<SelectOption>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SelectOption {
    pub value: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// What the client sends or the agent streams: text, an image, a reference.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Image {
        data: String,
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ResourceLink {
        name: String,
        uri: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    #[serde(other)]
    Other,
}

impl ContentBlock {
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    UserMessageChunk { content: ContentBlock },
    AgentMessageChunk { content: ContentBlock },
    AgentThoughtChunk { content: ContentBlock },
    ToolCall(ToolCall),
    ToolCallUpdate(ToolCallUpdate),
    Plan { entries: Vec<PlanEntry> },
    #[serde(rename_all = "camelCase")]
    AvailableCommandsUpdate { available_commands: Vec<AvailableCommand> },
    #[serde(rename_all = "camelCase")]
    CurrentModeUpdate { current_mode_id: String },
    #[serde(rename_all = "camelCase")]
    ConfigOptionUpdate { config_options: Vec<ConfigOption> },
    SessionInfoUpdate {
        #[serde(default)]
        title: Option<String>,
    },
    /// Context use: tokens in the window and its size.
    UsageUpdate { used: u64, size: u64 },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub tool_call_id: String,
    pub title: String,
    #[serde(default)]
    pub kind: ToolKind,
    #[serde(default)]
    pub status: ToolCallStatus,
    #[serde(default)]
    pub content: Vec<ToolCallContent>,
    #[serde(default)]
    pub locations: Vec<ToolCallLocation>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub raw_output: Option<Value>,
}

/// Only what changed; the rest of the call stays as it was.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallUpdate {
    pub tool_call_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<ToolKind>,
    #[serde(default)]
    pub status: Option<ToolCallStatus>,
    #[serde(default)]
    pub content: Option<Vec<ToolCallContent>>,
    #[serde(default)]
    pub locations: Option<Vec<ToolCallLocation>>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub raw_output: Option<Value>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    #[default]
    #[serde(other)]
    Other,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCallContent {
    Content { content: ContentBlock },
    #[serde(rename_all = "camelCase")]
    Diff {
        path: String,
        #[serde(default)]
        old_text: Option<String>,
        new_text: String,
    },
    #[serde(rename_all = "camelCase")]
    Terminal { terminal_id: String },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ToolCallLocation {
    pub path: String,
    #[serde(default)]
    pub line: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct PlanEntry {
    pub content: String,
    #[serde(default)]
    pub priority: Option<String>,
    pub status: PlanEntryStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct AvailableCommand {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    pub session_id: String,
    pub tool_call: ToolCallUpdate,
    pub options: Vec<PermissionOption>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: PermissionOptionKind,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    #[serde(other)]
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PromptResponse {
    pub stop_reason: StopReason,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetConfigOptionResponse {
    #[serde(default)]
    pub config_options: Vec<ConfigOption>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionNotification {
    pub session_id: String,
    pub update: SessionUpdate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn updates_parse_and_unknown_ones_are_skipped_not_fatal() {
        let parse = |value: Value| serde_json::from_value::<SessionUpdate>(value).unwrap();
        assert_eq!(
            parse(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hola"}})),
            SessionUpdate::AgentMessageChunk { content: ContentBlock::Text { text: "hola".into() } }
        );
        let SessionUpdate::ToolCall(call) = parse(json!({
            "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Edit main.rs", "kind": "edit",
            "status": "in_progress", "locations": [{"path": "/w/main.rs", "line": 3}],
            "content": [{"type": "diff", "path": "/w/main.rs", "oldText": "a", "newText": "b"}, {"type": "mystery"}]
        })) else {
            panic!("a tool call");
        };
        assert_eq!(call.kind, ToolKind::Edit);
        assert_eq!(call.status, ToolCallStatus::InProgress);
        assert_eq!(call.content[0], ToolCallContent::Diff { path: "/w/main.rs".into(), old_text: Some("a".into()), new_text: "b".into() });
        assert_eq!(call.content[1], ToolCallContent::Other);
        assert_eq!(parse(json!({"sessionUpdate": "x.ai/something", "whatever": 1})), SessionUpdate::Other);
        let SessionUpdate::ToolCall(call) = parse(json!({"sessionUpdate": "tool_call", "toolCallId": "t2", "title": "?", "kind": "teleport"})) else {
            panic!("a tool call");
        };
        assert_eq!(call.kind, ToolKind::Other);
    }

    #[test]
    fn model_choices_come_flat_or_grouped() {
        let option: ConfigOption = serde_json::from_value(json!({
            "id": "model", "name": "Modelo", "category": "model", "type": "select", "currentValue": "b",
            "options": [{"group": "g", "name": "G", "options": [{"value": "a", "name": "A"}, {"value": "b", "name": "B"}]}]
        }))
        .unwrap();
        let ConfigValue::Select { current_value, options } = &option.value else {
            panic!("a select");
        };
        assert_eq!(current_value, "b");
        assert_eq!(options.all().iter().map(|choice| choice.value.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }
}
