use crate::ToolCall;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionResolution {
    Allowed,
    Denied,
    TimedOut,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEventKind {
    SessionStarted {
        workspace: PathBuf,
    },
    TurnStarted {
        prompt: String,
    },
    ModelRequestStarted {
        step: usize,
        model: String,
    },
    AssistantDelta {
        content: String,
    },
    AssistantMessage {
        content: String,
    },
    ToolStarted {
        tool_call: ToolCall,
    },
    PermissionRequested {
        request_id: String,
        tool_call: ToolCall,
    },
    PermissionResolved {
        request_id: String,
        tool_call_id: String,
        resolution: PermissionResolution,
    },
    ToolFinished {
        tool_call_id: String,
        output: String,
        is_error: bool,
    },
    ProcessOutput {
        stream: OutputStream,
        text: String,
    },
    TurnCompleted {
        response: String,
        steps: usize,
    },
    TurnFailed {
        error: String,
    },
    TurnCancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentEvent {
    pub session_id: String,
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub kind: AgentEventKind,
}

impl AgentEvent {
    pub fn new(session_id: impl Into<String>, sequence: u64, kind: AgentEventKind) -> Self {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        Self {
            session_id: session_id.into(),
            sequence,
            timestamp_ms,
            kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn permission_events_survive_jsonl_round_trip() {
        let requested = AgentEvent::new(
            "session-1",
            3,
            AgentEventKind::PermissionRequested {
                request_id: "request-1".into(),
                tool_call: ToolCall {
                    id: "call-1".into(),
                    name: "write_file".into(),
                    arguments: json!({"path":"notes.txt","content":"hello"}),
                },
            },
        );
        let resolved = AgentEvent::new(
            "session-1",
            4,
            AgentEventKind::PermissionResolved {
                request_id: "request-1".into(),
                tool_call_id: "call-1".into(),
                resolution: PermissionResolution::Denied,
            },
        );
        for event in [requested, resolved] {
            let encoded = serde_json::to_string(&event).unwrap();
            let decoded: AgentEvent = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, event);
        }
    }
}
