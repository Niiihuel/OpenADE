use crate::{AgentMessage, CheckpointReference, PlanSnapshot, ToolCall, TurnMode, UserImage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Version 2 also binds the backend's wire request profile. Version 1 remains
/// readable but is insufficient to resume an in-flight native request.
pub const MODEL_CONTEXT_VERSION: u16 = 2;
pub const MCP_RUNTIME_IDENTITY_VERSION: u16 = 1;
pub const HOOK_CONFIG_IDENTITY_VERSION: u16 = 1;
pub const EXEC_POLICY_IDENTITY_VERSION: u16 = 1;
pub const SYSTEM_INSTRUCTIONS_VERSION: u16 = 1;
pub const MAX_SYSTEM_INSTRUCTIONS_BYTES: usize = 512 * 1024;

/// Domain-separated digest of the exact host/project instructions that were
/// sent to the native model. Returning None enforces the journal size limit.
pub fn system_instructions_digest(content: &str) -> Option<String> {
    (content.len() <= MAX_SYSTEM_INSTRUCTIONS_BYTES).then(|| {
        let mut hasher = Sha256::new();
        hasher.update(b"openide-system-instructions-v1\0");
        hasher.update((content.len() as u64).to_be_bytes());
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    })
}

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelTokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub reasoning_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelStreamEvent {
    Started {
        response_id: Option<String>,
    },
    TextDelta {
        content: String,
    },
    /// Public reasoning text only. Opaque reasoning and signatures remain in
    /// the committed provider output, never in presentation events.
    ReasoningDelta {
        item_id: String,
        content: String,
    },
    /// Incomplete arguments are display data, never executable ToolCall input.
    ToolCallDelta {
        item_id: String,
        call_id: Option<String>,
        /// Current name snapshot; some protocols extend it across fragments.
        name: Option<String>,
        arguments_delta: String,
    },
    /// Complete provider input is still provisional until response commit.
    ToolCallReady {
        item_id: String,
        call_id: String,
        name: String,
    },
    /// Provider-hosted web activity is presentation only, never a local tool
    /// call. Even a completed item remains provisional until response commit.
    WebSearchUpdate {
        item_id: String,
        status: String,
        action: Option<String>,
        query: Option<String>,
        url: Option<String>,
        sources: Vec<(String, String)>,
    },
    Usage {
        usage: ModelTokenUsage,
    },
    /// The provider request is being repeated after a retryable failure.
    /// Presentation only: the wait is `delay_ms` before attempt `attempt`.
    Retrying {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        rate_limited: bool,
    },
}

impl ModelStreamEvent {
    /// Bound hosted-search presentation independently of authoritative provider
    /// output. All byte clipping preserves UTF-8 and accounts for JSON escapes.
    /// Invalid identities are rejected rather than truncated into collisions.
    pub fn bounded_web_search_update(&self) -> Option<Self> {
        let Self::WebSearchUpdate {
            item_id,
            status,
            action,
            query,
            url,
            sources,
        } = self
        else {
            return None;
        };
        if item_id.is_empty() || item_id.len() > 256 || item_id.chars().any(char::is_control) {
            return None;
        }
        fn prefix(text: &str, cap: usize) -> String {
            let mut end = cap.min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text[..end].to_owned()
        }
        let mut bounded = Self::WebSearchUpdate {
            item_id: item_id.clone(),
            status: prefix(status, 32),
            action: action.as_deref().map(|value| prefix(value, 64)),
            query: query.as_deref().map(|value| prefix(value, 1024)),
            url: url.as_deref().map(|value| prefix(value, 2048)),
            sources: sources
                .iter()
                .take(8)
                .map(|(title, url)| (prefix(title, 256), prefix(url, 2048)))
                .collect(),
        };
        while serde_json::to_vec(&bounded).ok()?.len() > 8 * 1024 {
            let Self::WebSearchUpdate {
                sources,
                query,
                url,
                ..
            } = &mut bounded
            else {
                unreachable!()
            };
            if sources.pop().is_some() {
                continue;
            }
            let field =
                if url.as_ref().map_or(0, String::len) > query.as_ref().map_or(0, String::len) {
                    url
                } else {
                    query
                };
            let value = field.as_mut()?;
            *value = prefix(value, value.len() / 2);
        }
        Some(bounded)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEventKind {
    SessionStarted {
        workspace: PathBuf,
    },
    /// Written and synced before the native runner is started. The turn ID
    /// makes admission idempotent across a store reopen or caller retry.
    TurnAdmitted {
        turn_id: String,
        prompt: String,
        model: String,
        #[serde(default)]
        mode: TurnMode,
    },
    /// Atomic admission of text and verified inline images. Separate from
    /// the legacy variant so old journals and call sites remain unchanged.
    TurnAdmittedWithImages {
        turn_id: String,
        prompt: String,
        model: String,
        mode: TurnMode,
        images: Vec<UserImage>,
    },
    TurnStarted {
        prompt: String,
    },
    /// A user message submitted while this admitted turn is active. The
    /// native worker commits it only at a model-safe boundary; its sequence
    /// is the durable acknowledgement of an inbox item.
    TurnSteered {
        turn_id: String,
        input_id: String,
        text: String,
    },
    /// Durable acknowledgement of a steered user message with images.
    TurnSteeredWithImages {
        turn_id: String,
        input_id: String,
        text: String,
        images: Vec<UserImage>,
    },
    /// Exact frozen tool catalog advertised to the model for this turn.
    /// Synced before the first model request so recovery can reject a changed
    /// executor before dispatching a previously committed tool call.
    ToolCatalogCommitted {
        digest: String,
    },
    /// Verified snapshot of all connected MCP servers and their frozen tool
    /// catalog. Absent when any server is not eligible;
    /// such calls may run normally but cannot be resumed automatically.
    McpRuntimeCommitted {
        version: u16,
        digest: String,
    },
    /// Ordered host hook policy and launch-file identity, including the empty
    /// policy. Synced before the first model request of a native turn.
    HooksCommitted {
        version: u16,
        digest: String,
    },
    /// Effective host tool permissions and argv-prefix rules. Synced before
    /// the first model request so recovery cannot silently change command
    /// approval behavior after a crash.
    ExecPolicyCommitted {
        version: u16,
        digest: String,
    },
    /// Exact host, skill and project instructions for this turn. Synced before
    /// ModelContextCommitted so recovery can reuse the same text after the
    /// project documents change. This event is private journal data, never a
    /// transcript or search result.
    SystemInstructionsCommitted {
        version: u16,
        digest: String,
        content: String,
    },
    /// Versioned hash of provider, endpoint, model, mode, workspace, frozen
    /// catalog, full system prompt (including active skills), and backend
    /// request profile. The private source values are not stored in the journal.
    ModelContextCommitted {
        version: u16,
        digest: String,
    },
    /// Exact route of the forthcoming model request. Endpoint text and secrets
    /// are not journaled. Absent for legacy turns without explicit attribution.
    ModelRouteCommitted {
        version: u16,
        provider_id: String,
        model_id: String,
        endpoint_digest: String,
        request_profile_digest: String,
    },
    ModelRequestStarted {
        step: usize,
        model: String,
    },
    /// Durable presentation update for one in-flight model step. This cannot
    /// modify authoritative model history, authorize a tool, or close a turn.
    ModelStreamUpdate {
        step: usize,
        update: ModelStreamEvent,
    },
    /// One complete model response, synced before any subsequent tool effect.
    /// The assistant carries original provider items for stateless replay.
    ModelResponseCommitted {
        step: usize,
        assistant: AgentMessage,
        response_id: Option<String>,
        usage: Option<ModelTokenUsage>,
    },
    /// Makes an already-synced checkpoint candidate authoritative for replay.
    CheckpointCommitted {
        reference: CheckpointReference,
    },
    AssistantDelta {
        content: String,
    },
    AssistantMessage {
        content: String,
    },
    /// Full structured proposal from a completed Plan-mode response.
    PlanProposed {
        plan: PlanSnapshot,
    },
    /// Full Build progress snapshot, committed before the tool result.
    PlanUpdated {
        tool_call_id: String,
        plan: PlanSnapshot,
    },
    ToolStarted {
        tool_call: ToolCall,
    },
    /// Bounded observations from a running tool. Only ToolFinished commits
    /// its model-visible result; these bytes never authorize an effect.
    ToolOutputDelta {
        tool_call_id: String,
        stream: OutputStream,
        text: String,
    },
    /// Validated, bounded MCP image observations preserved independently of
    /// the truncated model-facing result. This never resolves a tool call.
    ToolResultMedia {
        tool_call_id: String,
        images: Vec<UserImage>,
        text: String,
        notices: Vec<String>,
    },
    /// Synced after ToolStarted and before launching one configured hook.
    HookStarted {
        tool_call_id: String,
        hook_id: String,
        phase: String,
    },
    /// Bounded diagnostics only. This does not resolve a pending hook; only
    /// HookFinished confirms its protocol decision. Old journals omit it.
    HookDiagnosticCommitted {
        tool_call_id: String,
        hook_id: String,
        phase: String,
        reason: Option<String>,
        stdout: Option<String>,
        stderr: Option<String>,
        error: Option<String>,
    },
    HookFinished {
        tool_call_id: String,
        hook_id: String,
        phase: String,
        decision: String,
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
    /// Human decision after a failed/cancelled turn with an unfinished tool
    /// batch. It discards only the pending calls; no tool result is invented.
    ToolBatchDiscarded {
        next_tool_call_id: String,
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

    #[test]
    fn system_instruction_snapshot_digest_is_bounded_and_round_trips() {
        let content = "Project rules\n\nSkill: inspect changes";
        let digest = system_instructions_digest(content).unwrap();
        let event = AgentEvent::new(
            "session-1",
            3,
            AgentEventKind::SystemInstructionsCommitted {
                version: SYSTEM_INSTRUCTIONS_VERSION,
                digest: digest.clone(),
                content: content.into(),
            },
        );
        let encoded = serde_json::to_vec(&event).unwrap();
        assert_eq!(
            serde_json::from_slice::<AgentEvent>(&encoded).unwrap(),
            event
        );
        assert_eq!(digest.len(), 64);
        assert_ne!(digest, system_instructions_digest("Project rules").unwrap());
        assert!(system_instructions_digest(&"x".repeat(MAX_SYSTEM_INSTRUCTIONS_BYTES)).is_some());
        assert!(
            system_instructions_digest(&"x".repeat(MAX_SYSTEM_INSTRUCTIONS_BYTES + 1)).is_none()
        );
    }
}
