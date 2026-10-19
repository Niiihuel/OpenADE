use openide_core::{AgentEvent, AgentEventKind as Kind, ModelStreamEvent};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize)]
pub struct PublicEvent {
    pub seq: u64,
    pub timestamp_ms: u64,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PendingPermission {
    pub request_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: Value,
    pub arguments_truncated: bool,
    /// What "always allow" would allow from now on, as the phone must show
    /// it: a command prefix. None where only "allow in this task" exists.
    pub always: Option<String>,
}

#[derive(Serialize)]
pub struct EventPage {
    pub events: Vec<PublicEvent>,
    pub next_cursor: u64,
    pub has_more: bool,
}

pub fn clip(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.into();
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated]", &text[..end])
}

fn public_failure(error: &str) -> &'static str {
    // Match only the transport's closed diagnostic prefix. Never interpolate
    // the provider body, which can contain echoed credentials or request data.
    if error.starts_with(
        "model request failed: Responses stream failed: subscription_sharing_usage_limit_exceeded:",
    ) {
        "Se alcanzó el límite de uso de tu plan de ChatGPT para esta conexión. Reintentá cuando se restablezca la cuota."
    } else {
        "La ejecución falló. Revisa la conexión, configuración y registro privado del host."
    }
}

pub fn safe_arguments(arguments: &Value) -> (Value, bool) {
    if serde_json::to_vec(arguments).map_or(true, |v| v.len() > 64 * 1024) {
        (
            serde_json::json!({"notice":"Arguments exceed mobile approval limit. Deny this request or inspect on the host."}),
            true,
        )
    } else {
        (arguments.clone(), false)
    }
}

pub fn pending_permissions(events: &[AgentEvent]) -> Vec<PendingPermission> {
    let mut pending = BTreeMap::new();
    for event in events {
        match &event.kind {
            Kind::PermissionRequested {
                request_id,
                tool_call,
            } => {
                let (arguments, arguments_truncated) = safe_arguments(&tool_call.arguments);
                let always = openide_harness::always_allow_prefix(&tool_call.name, &tool_call.arguments)
                    .filter(|_| !arguments_truncated)
                    .map(|prefix| prefix.join(" "));
                pending.insert(
                    request_id.clone(),
                    PendingPermission {
                        request_id: request_id.clone(),
                        tool_call_id: tool_call.id.clone(),
                        tool_name: tool_call.name.clone(),
                        arguments,
                        arguments_truncated,
                        always,
                    },
                );
            }
            Kind::PermissionResolved { request_id, .. } => {
                pending.remove(request_id);
            }
            Kind::TurnCompleted { .. } | Kind::TurnCancelled | Kind::TurnSuperseded { .. } | Kind::TurnFailed { .. } => {
                pending.clear()
            }
            _ => {}
        }
    }
    pending.into_values().collect()
}

/// Explicit allowlist: opaque provider items, snapshots of host instructions,
/// checkpoint digests and private journal authority never cross this boundary.
pub fn project_events(events: &[AgentEvent], after: u64, limit: usize) -> EventPage {
    let mut projected = Vec::new();
    let mut turn_id = None;
    let mut cursor = after;
    let mut step = 0;
    let mut committed = false;
    let mut page_bytes = 0usize;
    for event in events {
        match &event.kind {
            Kind::TurnAdmitted { turn_id: id, .. }
            | Kind::TurnAdmittedWithImages { turn_id: id, .. } => {
                turn_id = Some(id.clone());
                step = 0;
                committed = false;
            }
            Kind::ModelRequestStarted { step: current, .. } => {
                step = *current;
                committed = false;
            }
            _ => {}
        }
        if event.sequence <= after {
            if matches!(event.kind, Kind::ModelResponseCommitted { .. }) {
                committed = true;
            }
            continue;
        }
        if projected.len() >= limit {
            break;
        }
        let previous_cursor = cursor;
        cursor = event.sequence;
        let mut item = PublicEvent {
            seq: event.sequence,
            timestamp_ms: event.timestamp_ms,
            kind: "",
            text: None,
            turn_id: turn_id.clone(),
            message_id: None,
            request_id: None,
            tool_call_id: None,
            tool_name: None,
            arguments: None,
            decision: None,
            is_error: None,
        };
        match &event.kind {
            Kind::TurnAdmitted { prompt, .. } | Kind::TurnAdmittedWithImages { prompt, .. } => {
                item.kind = "user_message";
                item.text = Some(clip(prompt, 32 * 1024));
            }
            Kind::TurnSteered { text, .. } | Kind::TurnSteeredWithImages { text, .. } => {
                item.kind = "user_message";
                item.text = Some(clip(text, 32 * 1024));
            }
            Kind::ModelStreamUpdate {
                update: ModelStreamEvent::TextDelta { content },
                ..
            } => {
                item.kind = "assistant_delta";
                item.text = Some(clip(content, 32 * 1024));
            }
            Kind::ModelResponseCommitted { assistant, .. } => {
                committed = true;
                if assistant.content.is_empty() {
                    continue;
                }
                item.kind = "assistant_message";
                item.text = Some(clip(&assistant.content, 64 * 1024));
            }
            Kind::AssistantDelta { content } if !committed => {
                item.kind = "assistant_delta";
                item.text = Some(clip(content, 32 * 1024));
            }
            Kind::AssistantMessage { content } if !committed => {
                item.kind = "assistant_message";
                item.text = Some(clip(content, 64 * 1024));
            }
            Kind::ToolStarted { tool_call } => {
                item.kind = "tool_started";
                item.tool_call_id = Some(tool_call.id.clone());
                item.tool_name = Some(tool_call.name.clone());
                item.arguments = Some(safe_arguments(&tool_call.arguments).0);
            }
            Kind::ToolOutputDelta {
                tool_call_id, text, ..
            } => {
                item.kind = "tool_output";
                item.tool_call_id = Some(tool_call_id.clone());
                item.text = Some(clip(text, 32 * 1024));
            }
            Kind::ToolFinished {
                tool_call_id,
                output,
                is_error,
            } => {
                item.kind = "tool_finished";
                item.tool_call_id = Some(tool_call_id.clone());
                item.text = Some(clip(output, 64 * 1024));
                item.is_error = Some(*is_error);
            }
            Kind::PermissionRequested {
                request_id,
                tool_call,
            } => {
                item.kind = "permission_requested";
                item.request_id = Some(request_id.clone());
                item.tool_call_id = Some(tool_call.id.clone());
                item.tool_name = Some(tool_call.name.clone());
                item.arguments = Some(safe_arguments(&tool_call.arguments).0);
            }
            Kind::PermissionResolved {
                request_id,
                tool_call_id,
                resolution,
            } => {
                item.kind = "permission_resolved";
                item.request_id = Some(request_id.clone());
                item.tool_call_id = Some(tool_call_id.clone());
                item.decision = Some(format!("{resolution:?}").to_lowercase());
            }
            Kind::TurnCompleted { response, .. } => {
                item.kind = "turn_completed";
                item.text = Some(clip(response, 64 * 1024));
            }
            Kind::TurnFailed { error } => {
                item.kind = "turn_failed";
                // Some transports include a provider's raw HTTP error body in
                // the local journal. It is not a safe public error contract and
                // might echo request headers or other private diagnostics.
                item.text = Some(public_failure(error).into());
            }
            Kind::TurnCancelled => {
                item.kind = "turn_cancelled";
            }
            // The provider's text stays local; a consumer learns the state,
            // why in one word, and when it renews.
            Kind::TurnPaused { class, resets_at_ms, .. } => {
                item.kind = "turn_paused";
                item.text = Some(
                    serde_json::json!({"reason": class, "resets_at_ms": resets_at_ms}).to_string(),
                );
            }
            Kind::TurnSuperseded { model_id, .. } => {
                item.kind = "turn_superseded";
                item.text = Some(model_id.clone());
            }
            _ => continue,
        }
        if item.kind.starts_with("assistant_") {
            item.message_id = turn_id.as_ref().map(|id| format!("{id}:{step}"));
        }
        let size = serde_json::to_vec(&item).map_or(usize::MAX, |v| v.len());
        if !projected.is_empty() && page_bytes.saturating_add(size) > 512 * 1024 {
            // This event was inspected, not delivered; retry it on the next
            // page instead of advancing a cursor past invisible content.
            cursor = previous_cursor;
            break;
        }
        page_bytes = page_bytes.saturating_add(size);
        projected.push(item);
    }
    EventPage {
        events: projected,
        next_cursor: cursor,
        has_more: events.last().is_some_and(|e| e.sequence > cursor),
    }
}
