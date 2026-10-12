//! Pure, incremental projection of stored or live agent events into timeline rows.

use openide_core::{
    validate_tool_result_media, AgentEvent, AgentEventKind, ModelStreamEvent, ModelTokenUsage,
    OutputStream, PermissionResolution, UserImage, UserImageMime, MAX_USER_IMAGES_PER_MESSAGE,
    MAX_USER_IMAGE_MESSAGE_BYTES,
};
use openide_ui::{RasterImage, RasterImageFormat};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[path = "transcript_rows.rs"]
mod rows;
use rows::TranscriptRows;

const MAX_MODEL_STREAM_ROWS: usize = 64;
const MAX_MODEL_STREAM_BYTES: usize = 256 * 1024;
const MAX_REASONING_BYTES: usize = 16 * 1024;
const MAX_TOOL_PREVIEW_BYTES: usize = 32 * 1024;
const MAX_STREAM_ID_BYTES: usize = 256;
const MAX_TOOL_OUTPUT_PREVIEW_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelStreamStatus {
    Receiving,
    Committed,
    Interrupted,
    Failed,
    Cancelled,
    /// The confirmed call was explicitly discarded before execution.
    Discarded,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptImage {
    /// The renderer verifies and decodes only visible images on cache misses.
    /// An unavailable image retains its accessible label.
    pub raster: Option<RasterImage>,
    pub alt: String,
}

fn transcript_images(images: &[UserImage]) -> Vec<TranscriptImage> {
    let mut encoded_total = 0usize;
    let encoded_cap =
        MAX_USER_IMAGE_MESSAGE_BYTES.div_ceil(3) * 4 + MAX_USER_IMAGES_PER_MESSAGE * 4;
    images
        .iter()
        .take(MAX_USER_IMAGES_PER_MESSAGE)
        .enumerate()
        .map(|(index, image)| {
            let alt = format!("Imagen adjunta {} de {}", index + 1, images.len());
            let format = match image.mime_type {
                UserImageMime::Png => RasterImageFormat::Png,
                UserImageMime::Jpeg => RasterImageFormat::Jpeg,
                UserImageMime::Webp => RasterImageFormat::Webp,
                UserImageMime::Gif => RasterImageFormat::Gif,
            };
            let raster = encoded_total
                .checked_add(image.data_base64.len())
                .filter(|total| *total <= encoded_cap)
                .and_then(|total| {
                    encoded_total = total;
                    RasterImage::new(
                        &image.sha256,
                        format,
                        image.size_bytes,
                        Arc::from(image.data_base64.as_str()),
                        &alt,
                    )
                });
            TranscriptImage { raster, alt }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct HookActivity {
    pub tool_call_id: String,
    pub hook_id: String,
    pub phase: String,
    pub decision: Option<String>,
    pub interrupted: bool,
    pub reason: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityStatus {
    Running,
    WaitingPermission,
    PermissionInterrupted,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    /// A pending call was explicitly discarded without executing it.
    Discarded,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptActivity {
    Model {
        id: String,
        step: usize,
        model: String,
        has_text: bool,
        status: ActivityStatus,
    },
    Checkpoint {
        id: String,
        covered_event_sequence: u64,
    },
}

/// Recovery details only for the current turn and exact tool-call identity.
/// A later ToolFinished never fabricates a missing hook result.
pub fn hooks_for_tool(events: &[AgentEvent], call_id: &str) -> Vec<HookActivity> {
    let start = events
        .iter()
        .rposition(|event| matches!(event.kind, AgentEventKind::TurnStarted { .. }))
        .unwrap_or(0);
    TranscriptProjection::from_events(&events[start..])
        .items
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::Hook(hook) if hook.tool_call_id == call_id => Some(hook),
            _ => None,
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptItem {
    User {
        content: String,
        images: Vec<TranscriptImage>,
    },
    Assistant {
        content: String,
        streaming: bool,
    },
    /// Immutable plan event in its original position in the conversation.
    /// Later progress updates append a snapshot instead of replacing history.
    PlanSnapshot {
        id: String,
        sequence: Option<u64>,
        plan: openide_core::PlanSnapshot,
        is_proposal: bool,
        tool_call_id: Option<String>,
    },
    Reasoning {
        id: String,
        step: usize,
        content: String,
        status: ModelStreamStatus,
        truncated: bool,
    },
    ToolPreview {
        id: String,
        step: usize,
        item_id: String,
        call_id: Option<String>,
        name: Option<String>,
        arguments: String,
        input_complete: bool,
        status: ModelStreamStatus,
        truncated: bool,
    },
    ModelUsage {
        id: String,
        step: usize,
        usage: ModelTokenUsage,
        status: ModelStreamStatus,
    },
    Tool {
        id: String,
        name: String,
        arguments: Value,
        output: Option<String>,
        is_error: bool,
        activity: ActivityStatus,
    },
    /// Display-only stdout/stderr accepted before a tool's terminal result.
    /// Kept directly after its tool row, including after completion: the bounded
    /// final result can omit output that was already shown during execution.
    ToolOutput {
        tool_call_id: String,
        text: String,
        truncated: bool,
    },
    /// A typed, bounded tool result retained independently of the shorter
    /// textual model context. Arc-backed rasters are constructed once per event.
    ToolMedia {
        tool_call_id: String,
        images: Vec<TranscriptImage>,
        text: String,
        notices: Vec<String>,
    },
    Activity(TranscriptActivity),
    Hook(HookActivity),
    ProcessOutput {
        stream: OutputStream,
        text: String,
    },
    WebCitation {
        title: String,
        url: String,
    },
    /// A provider-hosted search is display-only and cannot dispatch local tools.
    WebSearch {
        id: String,
        item_id: String,
        action: String,
        query: Option<String>,
        url: Option<String>,
        sources: Vec<(String, String)>,
        status: ModelStreamStatus,
    },
    Notice {
        message: String,
        is_error: bool,
    },
}

/// Start and end event times of reasoning rows (keyed by row id), tool
/// calls (keyed by call id) and finished turns (keyed by [`turn_time_id`]).
/// They come from `AgentEvent::timestamp_ms`, never the wall clock, so a
/// restored session shows the times it recorded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemTimes(HashMap<String, (u64, Option<u64>)>);

impl ItemTimes {
    /// Milliseconds between an item's start and end events. `None` while it
    /// has no end, or for an id never timed. A clock that went backwards
    /// counts as zero.
    pub(crate) fn duration_ms(&self, id: &str) -> Option<u64> {
        let (start, end) = self.0.get(id)?;
        Some((*end)?.saturating_sub(*start))
    }

    /// When an item ended, in milliseconds since the Unix epoch.
    pub(crate) fn ended_ms(&self, id: &str) -> Option<u64> {
        self.0.get(id)?.1
    }

    #[cfg(test)]
    pub(crate) fn record(&mut self, id: &str, start: u64, end: Option<u64>) {
        self.0.insert(id.into(), (start, end));
    }
}

/// The [`ItemTimes`] key of the turn whose last `User` row is this one,
/// counting only `User` rows. A turn still open, or cut short without an
/// end event, has no entry.
pub(crate) fn turn_time_id(user_row: usize) -> String {
    format!("turn:{user_row}")
}

/// A duration for a transcript line: "un momento" under a second, then
/// "12 s" and "1 min 4 s".
pub(crate) fn duration_label(ms: u64) -> String {
    match ms / 1000 {
        0 => "un momento".into(),
        seconds @ 1..=59 => format!("{seconds} s"),
        seconds if seconds % 60 == 0 => format!("{} min", seconds / 60),
        seconds => format!("{} min {} s", seconds / 60, seconds % 60),
    }
}

/// A native turn's opening message: which `User` row it is, counting only
/// `User` rows, and the sequence of its admission, where a rewind cuts.
/// Steered messages and turns of external CLIs have no entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserTurn {
    pub user_row: usize,
    pub sequence: u64,
}

/// Replaying the same ordered events produces the same rows as applying them live.
#[derive(Debug, Default)]
pub struct TranscriptProjection {
    items: TranscriptRows,
    times: ItemTimes,
    times_dirty: Vec<String>,
    times_published: bool,
    previous_event_ms: Option<u64>,
    turn_started_ms: Option<u64>,
    plan_events: HashSet<(String, u64)>,
    next_local_plan_id: u64,
    tool_rows: HashMap<String, usize>,
    // Retain durable call identities across interruption. They are not live
    // execution authority and are cleared when the next turn starts.
    committed_tool_batch: Vec<String>,
    committed_tool_batch_start: usize,
    unfinished_tool_calls: HashSet<String>,
    tool_output_rows: HashMap<String, usize>,
    tool_media_rows: HashMap<String, usize>,
    hook_rows: HashMap<(String, String, String), usize>,
    // Text only merges with the adjacent live text row. A model response may
    // alternate text with reasoning or a tool preview; retain all its rows
    // until the authoritative snapshot reconciles their concatenation.
    active_assistant_rows: Vec<usize>,
    active_assistant_committed: bool,
    committed_final_assistant: Vec<usize>,
    active_model: Option<usize>,
    latest_model_step: Option<usize>,
    model_reasoning_rows: HashMap<String, usize>,
    model_reasoning_segments: Vec<usize>,
    model_tool_preview_rows: HashMap<String, usize>,
    model_web_search_rows: HashMap<String, usize>,
    model_usage_row: Option<usize>,
    model_stream_bytes: usize,
    model_response_id: Option<String>,
    model_retry: Option<crate::pause::ModelRetry>,
    active_stream_scope: Option<(String, u64, u64)>,
    stdout_row: Option<usize>,
    stdout_text: String,
    pending_web_citations: Vec<(String, String)>,
    pending_admission_images: Vec<TranscriptImage>,
    pending_admission_sequence: Option<u64>,
    /// `(next_turn_id, model)` of a superseded turn awaiting its continuation.
    superseded_by: Option<(String, String)>,
    /// Model of the admitted turn that continues a superseded one: its prompt
    /// is shown as a notice, never as something the person typed.
    continuation_model: Option<String>,
    user_turns: Vec<UserTurn>,
    tool_disclosure_aliases: Vec<(String, String)>,
}

impl TranscriptProjection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild an in-process live stream without inventing an interruption
    /// for its still-open turn. The caller must verify that the producer is
    /// alive; persisted history uses `from_events` for recovery annotations.
    pub fn from_live_events(events: &[AgentEvent]) -> Self {
        let mut projection = Self::new();
        for event in events {
            projection.apply_event(event);
        }
        projection
    }

    pub fn from_events(events: &[AgentEvent]) -> Self {
        let mut projection = Self::from_live_events(events);
        projection.flush_web_citations();
        let last_admission = events.iter().rposition(|event| {
            matches!(
                &event.kind,
                AgentEventKind::TurnAdmitted { .. } | AgentEventKind::TurnAdmittedWithImages { .. }
            )
        });
        let last_start = events
            .iter()
            .rposition(|event| matches!(&event.kind, AgentEventKind::TurnStarted { .. }));
        if let Some(index) =
            last_admission.filter(|index| last_start.is_none_or(|start| *index > start))
        {
            if let AgentEventKind::TurnAdmitted { prompt, .. }
            | AgentEventKind::TurnAdmittedWithImages { prompt, .. } = &events[index].kind
            {
                projection.items.push(TranscriptItem::User {
                    content: prompt.clone(),
                    images: std::mem::take(&mut projection.pending_admission_images),
                });
                projection.items.push(TranscriptItem::Notice {
                    message: "Turno admitido, pero interrumpido antes de iniciar".to_owned(),
                    is_error: true,
                });
            }
            return projection;
        }
        let unfinished_turn = events
            .iter()
            .rposition(|event| matches!(&event.kind, AgentEventKind::TurnStarted { .. }))
            .is_some_and(|start| {
                !events[start + 1..].iter().any(|event| {
                    matches!(
                        &event.kind,
                        AgentEventKind::TurnCompleted { .. }
                            | AgentEventKind::TurnFailed { .. }
                            | AgentEventKind::TurnCancelled | AgentEventKind::TurnSuperseded { .. }
                    )
                })
            });
        if unfinished_turn {
            projection.finish_streaming();
            let uncertain_tools = projection.unfinished_tool_calls.len();
            let pending_permission = projection.tool_rows.values().any(|index| {
                matches!(
                    projection.items[*index],
                    TranscriptItem::Tool {
                        output: None,
                        activity: ActivityStatus::WaitingPermission,
                        ..
                    }
                )
            });
            projection.items.push(TranscriptItem::Notice {
                message: if uncertain_tools > 0 {
                    "Turno interrumpido. Una herramienta pudo haberse ejecutado; su resultado es incierto. No se reintentará automáticamente.".to_owned()
                } else if pending_permission {
                    "Turno interrumpido con un permiso pendiente. La herramienta no se ejecutó.".to_owned()
                } else {
                    "Turno interrumpido al cerrar la aplicación".to_owned()
                },
                is_error: true,
            });
            // A response commit can be the last durable event before a crash.
            // Its final answer already exists; an explicit Resume may append
            // TurnCompleted without another AssistantMessage. Retain only
            // that current response's rows, never a partial/tool commentary.
            let committed_response = projection.active_assistant_committed;
            let committed_final = std::mem::take(&mut projection.committed_final_assistant);
            let recoverable_tools = projection.tool_rows.clone();
            let recoverable_previews = projection.model_tool_preview_rows.clone();
            projection.reset_turn();
            // Resume delivers only new journal events. Keep the old row
            // identity after sealing its state so PermissionRequested or
            // ToolStarted promotes it in place instead of duplicating it.
            // Sealed rows cannot accept new model/output deltas by themselves.
            projection.tool_rows = recoverable_tools;
            projection.model_tool_preview_rows = recoverable_previews;
            projection.active_assistant_committed = committed_response;
            if !committed_final.is_empty() {
                projection.active_assistant_rows = committed_final.clone();
                projection.active_assistant_committed = true;
                projection.committed_final_assistant = committed_final;
            }
        }
        projection
    }

    #[cfg(test)]
    pub fn items(&self) -> &[TranscriptItem] {
        &self.items
    }

    /// A live turn is admitted by the host, so its admission never arrives
    /// as an event here: without this its message would only become a turn,
    /// and editable, when the session is read again.
    fn note_continuation(&mut self, turn_id: &str) {
        self.continuation_model = self
            .superseded_by
            .take()
            .filter(|(next, _)| next == turn_id)
            .map(|(_, model)| model);
    }

    pub fn note_admission(&mut self, sequence: u64) {
        self.pending_admission_sequence = Some(sequence);
    }

    /// The native turns of this transcript, oldest first.
    pub fn user_turns(&self) -> &[UserTurn] {
        &self.user_turns
    }

    /// Publish changed rows to the host's retained snapshot. Use one target per
    /// projection; replacing the projection establishes a new complete baseline.
    /// This leaves historical strings/images untouched during live deltas.
    /// The provider request is waiting to be sent again.
    pub fn model_retry(&self) -> Option<crate::pause::ModelRetry> {
        self.model_retry
    }

    pub fn sync_into(&mut self, target: &mut Vec<TranscriptItem>) {
        self.items.sync_into(target);
    }

    /// Publish the times changed since the last call, as `sync_into` does rows.
    pub fn sync_times_into(&mut self, target: &mut ItemTimes) {
        if self.times_published {
            for id in self.times_dirty.drain(..) {
                if let Some(times) = self.times.0.get(&id) {
                    target.0.insert(id, *times);
                }
            }
        } else {
            target.clone_from(&self.times);
            self.times_dirty.clear();
            self.times_published = true;
        }
    }

    /// A tool call runs from ToolStarted to ToolFinished. A reasoning row runs
    /// from the event before its first delta, when the model went quiet to
    /// think, to its last delta.
    fn record_times(&mut self, event: &AgentEvent, changed: bool) {
        let now = event.timestamp_ms;
        match &event.kind {
            AgentEventKind::TurnStarted { .. } => self.turn_started_ms = Some(now),
            AgentEventKind::TurnCompleted { .. }
            | AgentEventKind::TurnFailed { .. }
            | AgentEventKind::TurnCancelled | AgentEventKind::TurnSuperseded { .. } => {
                let users = self
                    .items
                    .iter()
                    .filter(|item| matches!(item, TranscriptItem::User { .. }))
                    .count();
                if let (Some(start), Some(row)) = (self.turn_started_ms.take(), users.checked_sub(1)) {
                    let id = turn_time_id(row);
                    self.times.0.insert(id.clone(), (start, Some(now)));
                    self.times_dirty.push(id);
                }
            }
            _ => {}
        }
        let id = match &event.kind {
            AgentEventKind::ToolStarted { tool_call } => {
                self.times.0.insert(tool_call.id.clone(), (now, None));
                Some(&tool_call.id)
            }
            AgentEventKind::ToolFinished { tool_call_id, .. } => {
                self.times.0.get_mut(tool_call_id).map(|times| {
                    times.1 = Some(now);
                    tool_call_id
                })
            }
            AgentEventKind::ModelStreamUpdate {
                update: ModelStreamEvent::ReasoningDelta { .. },
                ..
            } if changed => match self.items.last() {
                Some(TranscriptItem::Reasoning { id, .. }) => {
                    if let Some(times) = self.times.0.get_mut(id) {
                        times.1 = Some(now);
                    } else {
                        let start = self.previous_event_ms.unwrap_or(now);
                        self.times.0.insert(id.clone(), (start, Some(now)));
                    }
                    Some(id)
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(id) = id.filter(|id| self.times_dirty.last() != Some(*id)) {
            self.times_dirty.push(id.clone());
        }
        self.previous_event_ms = Some(now);
    }

    pub fn has_live_tool_output(&self, call_id: &str) -> bool {
        self.tool_output_rows.contains_key(call_id)
    }

    /// A provider can disclose the call ID after a provisional row was already
    /// visible. The host migrates its explicit expansion choice at publication.
    pub fn take_tool_disclosure_aliases(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.tool_disclosure_aliases)
    }

    /// Returns whether the visible timeline changed.
    pub fn apply_event(&mut self, event: &AgentEvent) -> bool {
        if let AgentEventKind::PlanProposed { plan } | AgentEventKind::PlanUpdated { plan, .. } =
            &event.kind
        {
            if !plan.is_valid()
                || !self
                    .plan_events
                    .insert((event.session_id.clone(), event.sequence))
            {
                return false;
            }
            let tool_call_id = match &event.kind {
                AgentEventKind::PlanUpdated { tool_call_id, .. } => Some(tool_call_id.clone()),
                _ => None,
            };
            self.items.push(TranscriptItem::PlanSnapshot {
                id: format!("plan:{}:{}", event.session_id, event.sequence),
                sequence: Some(event.sequence),
                plan: plan.clone(),
                is_proposal: matches!(event.kind, AgentEventKind::PlanProposed { .. }),
                tool_call_id,
            });
            return true;
        }
        if matches!(event.kind, AgentEventKind::ModelStreamUpdate { .. }) {
            if let Some((session_id, request_sequence, last_sequence)) =
                &mut self.active_stream_scope
            {
                if session_id != &event.session_id
                    || event.sequence <= *request_sequence
                    || event.sequence <= *last_sequence
                {
                    return false;
                }
                *last_sequence = event.sequence;
            }
        }
        let mut changed = self.apply_kind(&event.kind);
        // A retry wait is shown until anything else arrives for the turn.
        let retry = match &event.kind {
            AgentEventKind::ModelStreamUpdate {
                update:
                    ModelStreamEvent::Retrying {
                        attempt,
                        max_attempts,
                        delay_ms,
                        ..
                    },
                ..
            } => Some(crate::pause::ModelRetry {
                attempt: *attempt,
                max_attempts: *max_attempts,
                until_ms: event.timestamp_ms.saturating_add(*delay_ms),
            }),
            _ => None,
        };
        changed |= std::mem::replace(&mut self.model_retry, retry) != retry;
        match &event.kind {
            AgentEventKind::TurnAdmitted { .. } | AgentEventKind::TurnAdmittedWithImages { .. } => {
                self.pending_admission_sequence = Some(event.sequence);
            }
            AgentEventKind::TurnStarted { .. } => {
                let typed = !matches!(self.items.last(), Some(TranscriptItem::Notice { .. }));
                if let Some(sequence) = self.pending_admission_sequence.take().filter(|_| typed) {
                    let users = self
                        .items
                        .iter()
                        .filter(|item| matches!(item, TranscriptItem::User { .. }))
                        .count();
                    self.user_turns.push(UserTurn { user_row: users - 1, sequence });
                }
            }
            _ => {}
        }
        self.record_times(event, changed);
        if let AgentEventKind::ModelRequestStarted { step, .. } = &event.kind {
            self.active_stream_scope =
                Some((event.session_id.clone(), event.sequence, event.sequence));
            if let Some(index) = self.active_model {
                if let TranscriptItem::Activity(TranscriptActivity::Model { id, .. }) =
                    &mut self.items[index]
                {
                    *id = format!(
                        "model-request:{}:{}:{step}",
                        event.session_id, event.sequence
                    );
                }
            }
        }
        changed
    }

    /// The event kind controls content; apply_event also scopes animation IDs
    /// to the stored session and exact request sequence.
    pub fn apply_kind(&mut self, kind: &AgentEventKind) -> bool {
        match kind {
            AgentEventKind::SessionStarted { .. }
            | AgentEventKind::ModelContextCommitted { .. }
            | AgentEventKind::ModelRouteCommitted { .. }
            | AgentEventKind::SystemInstructionsCommitted { .. }
            | AgentEventKind::McpRuntimeCommitted { .. }
            | AgentEventKind::HooksCommitted { .. }
            | AgentEventKind::ExecPolicyCommitted { .. }
            | AgentEventKind::ToolCatalogCommitted { .. } => false,
            AgentEventKind::PlanProposed { plan } | AgentEventKind::PlanUpdated { plan, .. } => {
                if !plan.is_valid() {
                    return false;
                }
                let tool_call_id = match kind {
                    AgentEventKind::PlanUpdated { tool_call_id, .. } => Some(tool_call_id.clone()),
                    _ => None,
                };
                // Kind-only callers have no durable identity to deduplicate.
                // Identical snapshots at different moments are still events.
                let id = self.next_local_plan_id;
                self.next_local_plan_id += 1;
                self.items.push(TranscriptItem::PlanSnapshot {
                    id: format!("plan:local:{id}"),
                    sequence: None,
                    plan: plan.clone(),
                    is_proposal: matches!(kind, AgentEventKind::PlanProposed { .. }),
                    tool_call_id,
                });
                true
            }
            AgentEventKind::CheckpointCommitted { reference } => {
                // Only the durable marker certifies compaction. A candidate,
                // model response, stdout message or terminal turn is insufficient.
                self.items
                    .push(TranscriptItem::Activity(TranscriptActivity::Checkpoint {
                        id: reference.checkpoint_id.clone(),
                        covered_event_sequence: reference.covered_event_sequence,
                    }));
                true
            }
            AgentEventKind::HookStarted {
                tool_call_id,
                hook_id,
                phase,
            } => {
                let key = (tool_call_id.clone(), hook_id.clone(), phase.clone());
                if self.hook_rows.contains_key(&key) {
                    return false;
                }
                self.hook_rows.insert(key, self.items.len());
                self.items.push(TranscriptItem::Hook(HookActivity {
                    tool_call_id: tool_call_id.clone(),
                    hook_id: hook_id.clone(),
                    phase: phase.clone(),
                    decision: None,
                    interrupted: false,
                    reason: None,
                    stdout: None,
                    stderr: None,
                    error: None,
                }));
                true
            }
            AgentEventKind::HookDiagnosticCommitted {
                tool_call_id,
                hook_id,
                phase,
                reason,
                stdout,
                stderr,
                error,
            } => {
                let key = (tool_call_id.clone(), hook_id.clone(), phase.clone());
                if let Some(index) = self.hook_rows.get(&key).copied() {
                    if let TranscriptItem::Hook(hook) = &mut self.items[index] {
                        hook.reason = reason.clone();
                        hook.stdout = stdout.clone();
                        hook.stderr = stderr.clone();
                        hook.error = error.clone();
                        if error.is_some() {
                            hook.interrupted = true;
                        }
                        return true;
                    }
                }
                false
            }
            AgentEventKind::HookFinished {
                tool_call_id,
                hook_id,
                phase,
                decision,
            } => {
                let key = (tool_call_id.clone(), hook_id.clone(), phase.clone());
                if let Some(index) = self.hook_rows.get(&key).copied() {
                    if let TranscriptItem::Hook(hook) = &mut self.items[index] {
                        hook.decision = Some(decision.clone());
                        hook.interrupted = false;
                        return true;
                    }
                }
                self.hook_rows.insert(key, self.items.len());
                self.items.push(TranscriptItem::Hook(HookActivity {
                    tool_call_id: tool_call_id.clone(),
                    hook_id: hook_id.clone(),
                    phase: phase.clone(),
                    decision: Some(decision.clone()),
                    interrupted: false,
                    reason: None,
                    stdout: None,
                    stderr: None,
                    error: None,
                }));
                true
            }
            AgentEventKind::TurnAdmitted { turn_id, .. } => {
                self.pending_admission_images.clear();
                self.note_continuation(turn_id);
                false
            }
            AgentEventKind::TurnSuperseded { model_id, next_turn_id, .. } => {
                self.finish_model(ActivityStatus::Cancelled);
                self.finish_model_stream(ModelStreamStatus::Cancelled);
                self.finish_streaming();
                self.flush_web_citations();
                self.superseded_by = Some((next_turn_id.clone(), model_id.clone()));
                self.reset_turn();
                true
            }
            // The turn stays open exactly as an interrupted one. Its cause is
            // shown by the pause card, from replay, not by a transcript row.
            AgentEventKind::TurnPaused { .. } => false,
            AgentEventKind::HostNotice { code, detail } => {
                self.items.push(TranscriptItem::Notice {
                    message: host_notice_text(code, detail),
                    is_error: false,
                });
                true
            }
            AgentEventKind::TurnAdmittedWithImages { images, turn_id, .. } => {
                self.pending_admission_images = transcript_images(images);
                self.note_continuation(turn_id);
                false
            }
            AgentEventKind::ModelStreamUpdate { step, update } => {
                self.model_stream_update(*step, update)
            }
            AgentEventKind::ModelResponseCommitted {
                step,
                assistant,
                usage,
                ..
            } => {
                if self.latest_model_step.is_some_and(|active| active != *step) {
                    return false;
                }
                self.pending_web_citations =
                    openide_harness::hosted_web_citations(&assistant.provider_output_items)
                        .unwrap_or_default()
                        .into_iter()
                        .take(32)
                        .map(|citation| {
                            (
                                citation.title.unwrap_or_else(|| citation.url.clone()),
                                citation.url,
                            )
                        })
                        .collect();
                if let Some(index) = self.active_model {
                    if let TranscriptItem::Activity(TranscriptActivity::Model {
                        has_text, ..
                    }) = &mut self.items[index]
                    {
                        *has_text = !assistant.content.is_empty();
                    }
                }
                let web_changed = self.commit_web_search(&assistant.provider_output_items);
                let changed = if assistant.content.is_empty() {
                    self.remove_active_assistant()
                } else {
                    self.assistant_message(&assistant.content)
                };
                self.commit_model_stream(&assistant.tool_calls, usage.as_ref());
                self.active_assistant_committed = true;
                self.committed_final_assistant = if assistant.tool_calls.is_empty() {
                    self.active_assistant_rows.clone()
                } else {
                    Vec::new()
                };
                let model_changed = self.finish_model(ActivityStatus::Completed);
                self.flush_web_citations() || changed || model_changed || web_changed
            }
            AgentEventKind::ModelRequestStarted { step, model } => {
                self.finish_streaming();
                self.finish_model(ActivityStatus::Interrupted);
                self.finish_model_stream(ModelStreamStatus::Interrupted);
                self.clear_model_stream();
                self.active_assistant_rows.clear();
                self.active_assistant_committed = false;
                self.committed_final_assistant.clear();
                self.latest_model_step = Some(*step);
                let index = self.items.len();
                self.items
                    .push(TranscriptItem::Activity(TranscriptActivity::Model {
                        id: format!("model-request:{index}:{step}"),
                        step: *step,
                        model: model.clone(),
                        has_text: false,
                        status: ActivityStatus::Running,
                    }));
                self.active_model = Some(index);
                true
            }
            AgentEventKind::TurnStarted { prompt } => {
                self.reset_turn();
                self.committed_tool_batch.clear();
                self.unfinished_tool_calls.clear();
                self.items.push(match self.continuation_model.take() {
                    // The conversation already shows what the person asked.
                    Some(model) => {
                        self.pending_admission_images.clear();
                        TranscriptItem::Notice {
                            message: continuation_notice(&model),
                            is_error: false,
                        }
                    }
                    None => TranscriptItem::User {
                        content: prompt.clone(),
                        images: std::mem::take(&mut self.pending_admission_images),
                    },
                });
                true
            }
            AgentEventKind::TurnSteered { text, input_id, .. } => {
                self.finish_streaming();
                self.active_assistant_rows.clear();
                self.active_assistant_committed = false;
                self.committed_final_assistant.clear();
                // What the host wrote for the model is a notice, never a
                // bubble the person could mistake for their own or edit.
                self.items.push(if input_id.starts_with(openide_harness::HOST_CONTINUE_INPUT) {
                    TranscriptItem::Notice {
                        message: "La respuesta llegó al límite de salida del modelo: se le pidió continuar".into(),
                        is_error: false,
                    }
                } else {
                    TranscriptItem::User {
                        content: text.clone(),
                        images: Vec::new(),
                    }
                });
                true
            }
            AgentEventKind::TurnSteeredWithImages { text, images, .. } => {
                self.finish_streaming();
                self.active_assistant_rows.clear();
                self.active_assistant_committed = false;
                self.committed_final_assistant.clear();
                self.items.push(TranscriptItem::User {
                    content: text.clone(),
                    images: transcript_images(images),
                });
                true
            }
            AgentEventKind::AssistantDelta { content } => {
                if self.active_assistant_committed {
                    return false;
                }
                if !content.is_empty() {
                    if let Some(index) = self.active_model {
                        if let TranscriptItem::Activity(TranscriptActivity::Model {
                            has_text,
                            ..
                        }) = &mut self.items[index]
                        {
                            *has_text = true;
                        }
                    }
                }
                self.assistant_delta(content)
            }
            AgentEventKind::AssistantMessage { content } => {
                let model_changed = self.finish_model(ActivityStatus::Completed);
                // Modern display events repeat the authoritative response.
                // They must not overwrite it or create a duplicate row.
                let changed = !self.active_assistant_committed && self.assistant_message(content);
                self.flush_web_citations() || changed || model_changed
            }
            AgentEventKind::ToolStarted { tool_call } => {
                self.unfinished_tool_calls.insert(tool_call.id.clone());
                self.finish_streaming();
                self.active_assistant_rows.clear();
                self.active_assistant_committed = false;
                self.committed_final_assistant.clear();
                self.flush_web_citations();
                if let Some(index) = self
                    .model_tool_preview_rows
                    .values()
                    .copied()
                    .find(|index| {
                        matches!(&self.items[*index], TranscriptItem::ToolPreview {
                        call_id: Some(call_id), status: ModelStreamStatus::Committed, ..
                    } if call_id == &tool_call.id)
                    })
                {
                    self.items[index] = TranscriptItem::Tool {
                        id: tool_call.id.clone(),
                        name: tool_call.name.clone(),
                        arguments: tool_call.arguments.clone(),
                        output: None,
                        is_error: false,
                        activity: ActivityStatus::Running,
                    };
                    self.model_tool_preview_rows.retain(|_, row| *row != index);
                    self.tool_rows.insert(tool_call.id.clone(), index);
                    return true;
                }
                if let Some(index) = self.tool_rows.get(&tool_call.id).copied() {
                    if let TranscriptItem::Tool {
                        name,
                        arguments,
                        output,
                        activity,
                        ..
                    } = &mut self.items[index]
                    {
                        if name.is_empty() || output.is_none() {
                            *name = tool_call.name.clone();
                            *arguments = tool_call.arguments.clone();
                            *activity = ActivityStatus::Running;
                            return true;
                        }
                    }
                }
                let index = self.items.len();
                self.items.push(TranscriptItem::Tool {
                    id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    arguments: tool_call.arguments.clone(),
                    output: None,
                    is_error: false,
                    activity: ActivityStatus::Running,
                });
                self.tool_rows.insert(tool_call.id.clone(), index);
                true
            }
            AgentEventKind::PermissionRequested { tool_call, .. } => {
                self.finish_streaming();
                // Permission arrives before ToolStarted. Promote the same
                // provisional row now so the file's state is truthful while
                // the executor waits, without a second generic notice.
                if !self.tool_rows.contains_key(&tool_call.id) {
                    let pending = TranscriptItem::Tool {
                        id: tool_call.id.clone(),
                        name: tool_call.name.clone(),
                        arguments: tool_call.arguments.clone(),
                        output: None,
                        is_error: false,
                        activity: ActivityStatus::WaitingPermission,
                    };
                    let preview = self
                        .model_tool_preview_rows
                        .values()
                        .copied()
                        .find(|index| {
                            matches!(&self.items[*index], TranscriptItem::ToolPreview {
                            call_id: Some(id), status: ModelStreamStatus::Committed, ..
                        } if id == &tool_call.id)
                        });
                    let index = if let Some(index) = preview {
                        self.items[index] = pending;
                        self.model_tool_preview_rows.retain(|_, row| *row != index);
                        index
                    } else {
                        let index = self.items.len();
                        self.items.push(pending);
                        index
                    };
                    self.tool_rows.insert(tool_call.id.clone(), index);
                }
                self.set_tool_activity(&tool_call.id, ActivityStatus::WaitingPermission);
                true
            }
            AgentEventKind::PermissionResolved {
                tool_call_id,
                resolution,
                ..
            } => {
                self.set_tool_activity(
                    tool_call_id,
                    match resolution {
                        PermissionResolution::Allowed => ActivityStatus::Running,
                        PermissionResolution::Denied | PermissionResolution::TimedOut => {
                            ActivityStatus::Failed
                        }
                        PermissionResolution::Cancelled => ActivityStatus::Cancelled,
                    },
                );
                // A question is not a permission: its answer is the result
                // the tool row shows, and skipping it is not a failure.
                if self.tool_rows.get(tool_call_id).is_some_and(|index| {
                    matches!(&self.items[*index], TranscriptItem::Tool { name, .. }
                        if openide_harness::is_user_interaction_tool(name))
                }) {
                    if *resolution == PermissionResolution::Denied {
                        self.set_tool_activity(tool_call_id, ActivityStatus::Running);
                    }
                    return true;
                }
                let (message, is_error) = match resolution {
                    PermissionResolution::Allowed => ("Permiso concedido", false),
                    PermissionResolution::Denied => ("Permiso denegado", true),
                    PermissionResolution::TimedOut => ("Permiso vencido", true),
                    PermissionResolution::Cancelled => ("Permiso cancelado", false),
                };
                self.items.push(TranscriptItem::Notice {
                    message: message.to_owned(),
                    is_error,
                });
                true
            }
            AgentEventKind::ToolFinished {
                tool_call_id,
                output,
                is_error,
            } => {
                self.unfinished_tool_calls.remove(tool_call_id);
                self.tool_output_rows.remove(tool_call_id);
                self.tool_media_rows.remove(tool_call_id);
                for ((call_id, _, _), index) in &self.hook_rows {
                    if call_id == tool_call_id {
                        if let TranscriptItem::Hook(hook) = &mut self.items[*index] {
                            if hook.decision.is_none() {
                                hook.interrupted = true;
                            }
                        }
                    }
                }
                // Manual recovery may confirm an uncertain effect after a
                // terminal event cleared the active-turn maps. Resolve that
                // original row so its captured output keeps the same owner.
                let index = self.tool_rows.get(tool_call_id).copied().or_else(|| {
                    self.items.iter().rposition(|item| {
                        matches!(item,
                            TranscriptItem::Tool { id, output: None, .. } if id == tool_call_id
                        )
                    })
                });
                if let Some(index) = index {
                    if let TranscriptItem::Tool {
                        output: row_output,
                        is_error: row_error,
                        activity,
                        ..
                    } = &mut self.items[index]
                    {
                        *row_output = Some(output.clone());
                        *row_error = *is_error;
                        *activity = if *is_error {
                            ActivityStatus::Failed
                        } else {
                            ActivityStatus::Completed
                        };
                        return true;
                    }
                }
                let index = self.items.len();
                self.items.push(TranscriptItem::Tool {
                    id: tool_call_id.clone(),
                    name: String::new(),
                    arguments: Value::Null,
                    output: Some(output.clone()),
                    is_error: *is_error,
                    activity: if *is_error {
                        ActivityStatus::Failed
                    } else {
                        ActivityStatus::Completed
                    },
                });
                self.tool_rows.insert(tool_call_id.clone(), index);
                true
            }
            AgentEventKind::ToolBatchDiscarded { next_tool_call_id } => {
                self.discard_tool_batch(next_tool_call_id);
                self.items.push(TranscriptItem::Notice {
                    message: format!(
                        "Llamadas pendientes descartadas desde {next_tool_call_id}; los efectos anteriores permanecen"
                    ),
                    is_error: false,
                });
                true
            }
            AgentEventKind::ProcessOutput { stream, text } => self.process_output(*stream, text),
            AgentEventKind::ToolOutputDelta {
                tool_call_id, text, ..
            } => self.tool_output_delta(tool_call_id, text),
            // What the user is shown; which images the model was given is
            // the journal's concern, not the view's.
            AgentEventKind::ToolResultMedia {
                tool_call_id,
                images,
                text,
                notices,
                ..
            } => self.tool_result_media(tool_call_id, images, text, notices),
            AgentEventKind::TurnCompleted { response, .. } => {
                let model_changed = self.finish_model(ActivityStatus::Completed);
                self.finish_model_stream(ModelStreamStatus::Committed);
                let changed = self.complete_assistant(response);
                let cited = self.flush_web_citations();
                self.reset_turn();
                changed || cited || model_changed
            }
            AgentEventKind::TurnFailed { error } => {
                self.finish_model(ActivityStatus::Failed);
                self.finish_model_stream(ModelStreamStatus::Failed);
                self.finish_streaming();
                self.flush_web_citations();
                self.items.push(TranscriptItem::Notice {
                    message: error.clone(),
                    is_error: true,
                });
                self.reset_turn();
                true
            }
            AgentEventKind::TurnCancelled => {
                self.finish_model(ActivityStatus::Cancelled);
                self.finish_model_stream(ModelStreamStatus::Cancelled);
                self.finish_streaming();
                self.flush_web_citations();
                self.items.push(TranscriptItem::Notice {
                    message: "Turno cancelado".into(),
                    is_error: false,
                });
                self.reset_turn();
                true
            }
        }
    }

    fn model_stream_update(&mut self, step: usize, update: &ModelStreamEvent) -> bool {
        if self.latest_model_step != Some(step)
            || self.active_model.is_none()
            || self.active_assistant_committed
        {
            return false;
        }
        match update {
            ModelStreamEvent::Started { response_id } => {
                if let Some(response_id) = response_id {
                    if !valid_stream_id(response_id)
                        || self
                            .model_response_id
                            .as_ref()
                            .is_some_and(|id| id != response_id)
                    {
                        return false;
                    }
                    self.model_response_id = Some(response_id.clone());
                }
                false
            }
            ModelStreamEvent::TextDelta { content } => self.assistant_delta(content),
            ModelStreamEvent::ReasoningDelta { item_id, content } => {
                if !valid_stream_id(item_id) || content.is_empty() {
                    return false;
                }
                let adjacent = self
                    .model_reasoning_rows
                    .get(item_id)
                    .copied()
                    .filter(|index| Some(*index) == self.items.len().checked_sub(1));
                let created = adjacent.is_none();
                let index = if let Some(index) = adjacent {
                    index
                } else {
                    if !self.can_add_model_stream_row() {
                        return false;
                    }
                    self.finish_streaming();
                    let index = self.items.len();
                    // Multiple non-adjacent segments can share a provider
                    // item. Suffix every segment, so a provider ID containing
                    // our separator cannot collide with an unsuffixed row.
                    let id = format!(
                        "{}:segment:{index}",
                        self.model_stream_row_id("reasoning", item_id)
                    );
                    self.items.push(TranscriptItem::Reasoning {
                        id,
                        step,
                        content: String::new(),
                        status: ModelStreamStatus::Receiving,
                        truncated: false,
                    });
                    self.model_reasoning_rows.insert(item_id.clone(), index);
                    self.model_reasoning_segments.push(index);
                    index
                };
                if let TranscriptItem::Reasoning {
                    content: text,
                    truncated,
                    ..
                } = &mut self.items[index]
                {
                    return append_stream_text(
                        text,
                        content,
                        MAX_REASONING_BYTES,
                        &mut self.model_stream_bytes,
                        truncated,
                    ) || created;
                }
                false
            }
            ModelStreamEvent::ToolCallDelta {
                item_id,
                call_id,
                name,
                arguments_delta,
            } => self.tool_preview_update(
                step,
                item_id,
                call_id.as_deref(),
                name.as_deref(),
                arguments_delta,
                false,
            ),
            ModelStreamEvent::ToolCallReady {
                item_id,
                call_id,
                name,
            } => self.tool_preview_update(step, item_id, Some(call_id), Some(name), "", true),
            update @ ModelStreamEvent::WebSearchUpdate { .. } => {
                self.web_search_update(update, false)
            }
            ModelStreamEvent::Usage { usage } => self.model_usage_update(step, usage),
            // Shown by the live waiting line, not as a row: see `apply_event`.
            ModelStreamEvent::Retrying { .. } => false,
        }
    }

    fn tool_preview_update(
        &mut self,
        step: usize,
        item_id: &str,
        call_id: Option<&str>,
        name: Option<&str>,
        arguments_delta: &str,
        input_complete: bool,
    ) -> bool {
        if !valid_stream_id(item_id)
            || call_id.is_some_and(|id| !valid_stream_id(id))
            || name.is_some_and(|name| name.is_empty() || name.len() > MAX_STREAM_ID_BYTES)
        {
            return false;
        }
        if call_id.is_some_and(|call_id| {
            self.tool_rows.contains_key(call_id)
                || self
                    .model_tool_preview_rows
                    .iter()
                    .any(|(other_item, index)| {
                        other_item != item_id
                            && matches!(&self.items[*index],
                        TranscriptItem::ToolPreview { call_id: Some(existing), .. }
                        if existing == call_id)
                    })
        }) {
            return false;
        }
        let created = !self.model_tool_preview_rows.contains_key(item_id);
        let index = if let Some(index) = self.model_tool_preview_rows.get(item_id) {
            *index
        } else {
            if !self.can_add_model_stream_row() {
                return false;
            }
            self.finish_streaming();
            let index = self.items.len();
            self.items.push(TranscriptItem::ToolPreview {
                id: self.model_stream_row_id("tool", item_id),
                step,
                item_id: item_id.to_owned(),
                call_id: None,
                name: None,
                arguments: String::new(),
                input_complete: false,
                status: ModelStreamStatus::Receiving,
                truncated: false,
            });
            self.model_tool_preview_rows
                .insert(item_id.to_owned(), index);
            index
        };
        if let TranscriptItem::ToolPreview {
            id,
            call_id: current_call_id,
            name: current_name,
            arguments,
            input_complete: complete,
            truncated,
            ..
        } = &mut self.items[index]
        {
            if call_id
                .zip(current_call_id.as_deref())
                .is_some_and(|(new, old)| new != old)
                || name
                    .zip(current_name.as_deref())
                    .is_some_and(|(new, old)| new != old && (*complete || !new.starts_with(old)))
                || *complete && !arguments_delta.is_empty()
            {
                return false;
            }
            let changed = created
                || call_id.is_some_and(|id| current_call_id.as_deref() != Some(id))
                || name.is_some_and(|name| current_name.as_deref() != Some(name))
                || input_complete && !*complete;
            if let Some(call_id) = call_id {
                if current_call_id.is_none() {
                    // Match the bounded host disclosure state, including when
                    // several model/tool transitions arrive in the same frame.
                    if self.tool_disclosure_aliases.len() == 128 {
                        self.tool_disclosure_aliases.remove(0);
                    }
                    self.tool_disclosure_aliases
                        .push((id.clone(), format!("tool:{call_id}")));
                }
                *current_call_id = Some(call_id.to_owned());
            }
            if let Some(name) = name {
                *current_name = Some(name.to_owned());
            }
            *complete |= input_complete;
            return append_stream_text(
                arguments,
                arguments_delta,
                MAX_TOOL_PREVIEW_BYTES,
                &mut self.model_stream_bytes,
                truncated,
            ) || changed;
        }
        false
    }

    fn model_usage_update(&mut self, step: usize, usage: &ModelTokenUsage) -> bool {
        if let Some(index) = self.model_usage_row {
            if let TranscriptItem::ModelUsage {
                usage: existing, ..
            } = &mut self.items[index]
            {
                let changed = existing != usage;
                *existing = usage.clone();
                return changed;
            }
        }
        if !self.can_add_model_stream_row() {
            return false;
        }
        self.finish_streaming();
        self.model_usage_row = Some(self.items.len());
        self.items.push(TranscriptItem::ModelUsage {
            id: self.model_stream_row_id("usage", "reported"),
            step,
            usage: usage.clone(),
            status: ModelStreamStatus::Receiving,
        });
        true
    }

    fn web_search_update(&mut self, update: &ModelStreamEvent, committed: bool) -> bool {
        let Some(ModelStreamEvent::WebSearchUpdate {
            item_id,
            status: provider_status,
            mut action,
            mut query,
            mut url,
            mut sources,
        }) = update.bounded_web_search_update()
        else {
            return false;
        };
        let index = self.model_web_search_rows.get(&item_id).copied();
        let previous = index.and_then(|index| self.items.get(index)).cloned();
        if let Some(TranscriptItem::WebSearch {
            action: old_action,
            query: old_query,
            url: old_url,
            sources: old_sources,
            status,
            ..
        }) = &previous
        {
            if !committed {
                if *status != ModelStreamStatus::Receiving {
                    return false;
                }
                if action.is_none() {
                    action = Some(old_action.clone());
                }
                if query.is_none() {
                    query = old_query.clone();
                }
                if url.is_none() {
                    url = old_url.clone();
                }
                if sources.is_empty() {
                    sources = old_sources.clone();
                }
            }
        } else if !self.can_add_model_stream_row() {
            return false;
        }
        // Merging a sparse phase update with a previous query must preserve
        // the same encoded cap as an individual provider snapshot.
        let Some(ModelStreamEvent::WebSearchUpdate {
            action,
            query,
            url,
            sources,
            ..
        }) = (ModelStreamEvent::WebSearchUpdate {
            item_id: item_id.clone(),
            status: provider_status.clone(),
            action,
            query,
            url,
            sources,
        })
        .bounded_web_search_update()
        else {
            return false;
        };
        let status = match provider_status.as_str() {
            "completed" if committed => ModelStreamStatus::Committed,
            "failed" => ModelStreamStatus::Failed,
            "cancelled" | "canceled" => ModelStreamStatus::Cancelled,
            "incomplete" => ModelStreamStatus::Interrupted,
            _ if committed => ModelStreamStatus::Interrupted,
            _ => ModelStreamStatus::Receiving,
        };
        let id = match &previous {
            Some(TranscriptItem::WebSearch { id, .. }) => id.clone(),
            _ => self.model_stream_row_id("web", &item_id),
        };
        let next = TranscriptItem::WebSearch {
            id,
            item_id: item_id.clone(),
            action: action.unwrap_or_else(|| "web_search".into()),
            query,
            url,
            sources,
            status,
        };
        if previous.as_ref() == Some(&next) {
            return false;
        }
        let next_bytes = self
            .model_stream_bytes
            .saturating_sub(previous.as_ref().map_or(0, web_search_row_bytes))
            .saturating_add(web_search_row_bytes(&next));
        if next_bytes > MAX_MODEL_STREAM_BYTES {
            return false;
        }
        self.model_stream_bytes = next_bytes;
        if let Some(index) = index {
            self.items[index] = next;
        } else {
            self.finish_streaming();
            // Non-streaming responses still describe work preceding their
            // answer. Insert new committed activity before that answer, while
            // an already observed stream row retains its exact position.
            let position = committed
                .then(|| self.active_assistant_rows.first().copied())
                .flatten();
            if let Some(position) = position {
                self.insert_row(position, next);
                self.model_web_search_rows.insert(item_id, position);
            } else {
                self.model_web_search_rows.insert(item_id, self.items.len());
                self.items.push(next);
            }
        }
        true
    }

    fn commit_web_search(&mut self, output_items: &[Value]) -> bool {
        let mut changed = false;
        for item in output_items {
            if let Some(update) = openide_harness::hosted_web_search_presentation(item) {
                changed |= self.web_search_update(&update, true);
            }
        }
        changed
    }

    fn can_add_model_stream_row(&self) -> bool {
        self.model_reasoning_segments.len()
            + self.model_tool_preview_rows.len()
            + self.model_web_search_rows.len()
            + usize::from(self.model_usage_row.is_some())
            < MAX_MODEL_STREAM_ROWS
    }

    fn model_stream_row_id(&self, kind: &str, item_id: &str) -> String {
        let request_id = self
            .active_model
            .and_then(|index| self.items.get(index))
            .and_then(|item| match item {
                TranscriptItem::Activity(TranscriptActivity::Model { id, .. }) => Some(id.as_str()),
                _ => None,
            })
            .unwrap_or("model-request");
        format!("{request_id}:{kind}:{item_id}")
    }

    fn commit_model_stream(
        &mut self,
        calls: &[openide_core::ToolCall],
        usage: Option<&ModelTokenUsage>,
    ) {
        self.committed_tool_batch = calls.iter().map(|call| call.id.clone()).collect();
        self.committed_tool_batch_start = self.active_model.unwrap_or(self.items.len());
        if let (Some(step), Some(usage)) = (self.latest_model_step, usage) {
            self.model_usage_update(step, usage);
        }
        self.finish_model_stream(ModelStreamStatus::Committed);
        for index in self.model_tool_preview_rows.values() {
            if let TranscriptItem::ToolPreview {
                call_id,
                name,
                status,
                ..
            } = &mut self.items[*index]
            {
                let confirmed = call_id
                    .as_deref()
                    .and_then(|id| calls.iter().find(|call| call.id == id));
                if let Some(call) = confirmed {
                    *name = Some(bounded_stream_prefix(&call.name, MAX_STREAM_ID_BYTES).to_owned());
                } else {
                    *status = ModelStreamStatus::Interrupted;
                }
            }
        }
    }

    fn discard_tool_batch(&mut self, next_call_id: &str) {
        let Some(start) = self
            .committed_tool_batch
            .iter()
            .position(|call_id| call_id == next_call_id)
        else {
            return;
        };
        let discarded: HashSet<_> = self.committed_tool_batch[start..].iter().cloned().collect();
        for index in self.committed_tool_batch_start..self.items.len() {
            match &self.items[index] {
                TranscriptItem::ToolPreview {
                    call_id: Some(call_id),
                    status: ModelStreamStatus::Committed,
                    ..
                } if discarded.contains(call_id) => {
                    if let TranscriptItem::ToolPreview { status, .. } = &mut self.items[index] {
                        *status = ModelStreamStatus::Discarded;
                    }
                }
                TranscriptItem::Tool {
                    id, output: None, ..
                } if discarded.contains(id) && !self.unfinished_tool_calls.contains(id) => {
                    if let TranscriptItem::Tool { activity, .. } = &mut self.items[index] {
                        *activity = ActivityStatus::Discarded;
                    }
                }
                _ => {}
            }
        }
        self.committed_tool_batch.clear();
    }

    fn finish_model_stream(&mut self, terminal: ModelStreamStatus) -> bool {
        let mut changed = false;
        for index in self
            .model_reasoning_segments
            .iter()
            .chain(self.model_tool_preview_rows.values())
            .chain(self.model_web_search_rows.values())
            .copied()
            .chain(self.model_usage_row)
        {
            let status = match &mut self.items[index] {
                TranscriptItem::Reasoning { status, .. }
                | TranscriptItem::ToolPreview { status, .. }
                | TranscriptItem::ModelUsage { status, .. } => status,
                TranscriptItem::WebSearch { status, .. } => {
                    // A missing item in the authoritative output cannot become
                    // successful just because the surrounding response commits.
                    if *status == ModelStreamStatus::Receiving
                        && terminal == ModelStreamStatus::Committed
                    {
                        *status = ModelStreamStatus::Interrupted;
                        changed = true;
                    }
                    status
                }
                _ => continue,
            };
            if *status == ModelStreamStatus::Receiving {
                *status = terminal;
                changed = true;
            }
        }
        changed
    }

    fn clear_model_stream(&mut self) {
        self.model_reasoning_rows.clear();
        self.model_reasoning_segments.clear();
        self.model_tool_preview_rows.clear();
        self.model_web_search_rows.clear();
        self.model_usage_row = None;
        self.model_stream_bytes = 0;
        self.model_response_id = None;
        self.active_stream_scope = None;
    }

    fn assistant_delta(&mut self, content: &str) -> bool {
        if content.is_empty() {
            return false;
        }
        if let Some(index) = self
            .active_assistant_rows
            .last()
            .copied()
            .filter(|index| Some(*index) == self.items.len().checked_sub(1))
        {
            if let TranscriptItem::Assistant {
                content: existing,
                streaming: true,
            } = &mut self.items[index]
            {
                existing.push_str(content);
                return true;
            }
        }
        self.finish_streaming();
        self.active_assistant_rows.push(self.items.len());
        self.items.push(TranscriptItem::Assistant {
            content: content.to_owned(),
            streaming: true,
        });
        true
    }

    fn assistant_message(&mut self, content: &str) -> bool {
        if content.is_empty() {
            return false;
        }
        // The provider's final text is the concatenation of its text events,
        // excluding interleaved reasoning and previews. Keep that chronology
        // when the snapshot confirms it, rather than copying the whole answer
        // into the last segment and duplicating every preceding one.
        if self.assistant_segments_match(content) {
            return self.finish_streaming();
        }
        if let Some(index) = self.active_assistant_rows.first().copied() {
            let obsolete = self.active_assistant_rows[1..].to_vec();
            let mut removed = false;
            for index in obsolete.into_iter().rev() {
                removed |= self.remove_assistant_row(index);
            }
            if let TranscriptItem::Assistant {
                content: existing,
                streaming,
            } = &mut self.items[index]
            {
                let changed = removed || existing != content || *streaming;
                *existing = content.to_owned();
                *streaming = false;
                return changed;
            }
        }
        self.active_assistant_rows.push(self.items.len());
        self.items.push(TranscriptItem::Assistant {
            content: content.to_owned(),
            streaming: false,
        });
        true
    }

    fn assistant_segments_match(&self, snapshot: &str) -> bool {
        if self.active_assistant_rows.is_empty() {
            return false;
        }
        // Compare borrowed slices, without allocating a second complete
        // response solely for reconciliation.
        let mut remaining = snapshot;
        for index in &self.active_assistant_rows {
            let Some(TranscriptItem::Assistant { content, .. }) = self.items.get(*index) else {
                return false;
            };
            let Some(suffix) = remaining.strip_prefix(content.as_str()) else {
                return false;
            };
            remaining = suffix;
        }
        remaining.is_empty()
    }

    fn remove_active_assistant(&mut self) -> bool {
        let rows = std::mem::take(&mut self.active_assistant_rows);
        let mut changed = false;
        for index in rows.into_iter().rev() {
            changed |= self.remove_assistant_row(index);
        }
        changed
    }

    fn remove_assistant_row(&mut self, index: usize) -> bool {
        if !matches!(
            self.items.get(index),
            Some(TranscriptItem::Assistant { .. })
        ) {
            return false;
        }
        self.remove_row(index);
        true
    }

    fn remove_row(&mut self, index: usize) {
        self.items.remove(index);
        if self.committed_tool_batch_start > index {
            self.committed_tool_batch_start -= 1;
        }
        for row in self
            .tool_rows
            .values_mut()
            .chain(self.tool_output_rows.values_mut())
            .chain(self.tool_media_rows.values_mut())
            .chain(self.hook_rows.values_mut())
            .chain(self.model_reasoning_rows.values_mut())
            .chain(self.model_tool_preview_rows.values_mut())
            .chain(self.model_web_search_rows.values_mut())
        {
            if *row > index {
                *row -= 1;
            }
        }
        for row in [
            &mut self.active_model,
            &mut self.stdout_row,
            &mut self.model_usage_row,
        ] {
            if row.is_some_and(|value| value > index) {
                *row = row.map(|value| value - 1);
            } else if *row == Some(index) {
                *row = None;
            }
        }
        for rows in [
            &mut self.active_assistant_rows,
            &mut self.committed_final_assistant,
            &mut self.model_reasoning_segments,
        ] {
            rows.retain(|row| *row != index);
            for row in rows {
                if *row > index {
                    *row -= 1;
                }
            }
        }
    }

    fn insert_row(&mut self, index: usize, item: TranscriptItem) {
        self.items.insert(index, item);
        if self.committed_tool_batch_start >= index {
            self.committed_tool_batch_start += 1;
        }
        for row in self
            .tool_rows
            .values_mut()
            .chain(self.tool_output_rows.values_mut())
            .chain(self.tool_media_rows.values_mut())
            .chain(self.hook_rows.values_mut())
            .chain(self.model_reasoning_rows.values_mut())
            .chain(self.model_tool_preview_rows.values_mut())
            .chain(self.model_web_search_rows.values_mut())
        {
            if *row >= index {
                *row += 1;
            }
        }
        for row in [
            &mut self.active_model,
            &mut self.stdout_row,
            &mut self.model_usage_row,
        ] {
            if row.is_some_and(|value| value >= index) {
                *row = row.map(|value| value + 1);
            }
        }
        for rows in [
            &mut self.active_assistant_rows,
            &mut self.committed_final_assistant,
            &mut self.model_reasoning_segments,
        ] {
            for row in rows {
                if *row >= index {
                    *row += 1;
                }
            }
        }
    }

    fn tool_result_media(
        &mut self,
        call_id: &str,
        images: &[UserImage],
        text: &str,
        notices: &[String],
    ) -> bool {
        let Some(tool_index) = self.tool_rows.get(call_id).copied() else {
            return false;
        };
        // One typed observation belongs to one confirmed, executing MCP call.
        // A duplicate, late or orphan payload cannot replace the captured result.
        if self.tool_media_rows.contains_key(call_id)
            || !matches!(&self.items[tool_index], TranscriptItem::Tool {
                name, output: None, activity: ActivityStatus::Running, ..
            } if name.starts_with("mcp__"))
            || validate_tool_result_media(images, text, notices).is_err()
        {
            return false;
        }
        let images = images
            .iter()
            .enumerate()
            .map(|(index, image)| {
                let alt = format!("Imagen {} del resultado de la herramienta", index + 1);
                let format = match image.mime_type {
                    UserImageMime::Png => RasterImageFormat::Png,
                    UserImageMime::Jpeg => RasterImageFormat::Jpeg,
                    UserImageMime::Webp => RasterImageFormat::Webp,
                    UserImageMime::Gif => RasterImageFormat::Gif,
                };
                TranscriptImage {
                    raster: RasterImage::new(
                        &image.sha256,
                        format,
                        image.size_bytes,
                        Arc::from(image.data_base64.as_str()),
                        &alt,
                    ),
                    alt,
                }
            })
            .collect();
        let index = self
            .tool_output_rows
            .get(call_id)
            .map_or(tool_index + 1, |row| row + 1);
        self.insert_row(
            index,
            TranscriptItem::ToolMedia {
                tool_call_id: call_id.to_owned(),
                images,
                text: text.to_owned(),
                notices: notices.to_vec(),
            },
        );
        self.tool_media_rows.insert(call_id.to_owned(), index);
        true
    }

    fn tool_output_delta(&mut self, call_id: &str, delta: &str) -> bool {
        if delta.is_empty() {
            return false;
        }
        let Some(tool_index) = self.tool_rows.get(call_id).copied() else {
            return false;
        };
        // A late delta cannot overwrite an authoritative result or revive a
        // cancelled/denied call. stderr is output, not evidence of failure.
        if !matches!(
            &self.items[tool_index],
            TranscriptItem::Tool {
                output: None,
                activity: ActivityStatus::Running,
                ..
            }
        ) {
            return false;
        }
        let index = if let Some(index) = self.tool_output_rows.get(call_id) {
            *index
        } else {
            let index = tool_index + 1;
            self.insert_row(
                index,
                TranscriptItem::ToolOutput {
                    tool_call_id: call_id.to_owned(),
                    text: String::new(),
                    truncated: false,
                },
            );
            self.tool_output_rows.insert(call_id.to_owned(), index);
            index
        };
        let TranscriptItem::ToolOutput {
            text, truncated, ..
        } = &mut self.items[index]
        else {
            return false;
        };
        // A terminal preview must keep moving under sustained output. Preserve
        // the latest complete UTF-8 suffix rather than freezing the first 32 KiB.
        if delta.len() >= MAX_TOOL_OUTPUT_PREVIEW_BYTES {
            let mut start = delta.len() - MAX_TOOL_OUTPUT_PREVIEW_BYTES;
            while !delta.is_char_boundary(start) {
                start += 1;
            }
            *truncated |= !text.is_empty() || start > 0;
            text.clear();
            text.push_str(&delta[start..]);
        } else {
            let mut remove =
                (text.len() + delta.len()).saturating_sub(MAX_TOOL_OUTPUT_PREVIEW_BYTES);
            while !text.is_char_boundary(remove) {
                remove += 1;
            }
            if remove > 0 {
                text.drain(..remove);
                *truncated = true;
            }
            text.push_str(delta);
        }
        true
    }

    fn process_output(&mut self, stream: OutputStream, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        if stream == OutputStream::Stdout {
            self.stdout_text.push_str(text);
            if let Some(index) = self.stdout_row {
                if let TranscriptItem::ProcessOutput { text: existing, .. } = &mut self.items[index]
                {
                    existing.push_str(text);
                    return true;
                }
            }
            self.stdout_row = Some(self.items.len());
        } else if let Some(TranscriptItem::ProcessOutput {
            stream: OutputStream::Stderr,
            text: existing,
        }) = self.items.last_mut()
        {
            existing.push_str(text);
            return true;
        }
        self.items.push(TranscriptItem::ProcessOutput {
            stream,
            text: text.to_owned(),
        });
        true
    }

    fn complete_assistant(&mut self, response: &str) -> bool {
        if !self.active_assistant_rows.is_empty() {
            return if response.is_empty() {
                self.finish_streaming()
            } else {
                self.assistant_message(response)
            };
        }
        // The CLI's final response is its captured stdout, possibly truncated by
        // the runner. Reuse the streamed row even when the capture was limited.
        if let Some(index) = self.stdout_row {
            self.items[index] = TranscriptItem::Assistant {
                content: self.stdout_text.clone(),
                streaming: false,
            };
            return true;
        }
        if !response.is_empty() {
            self.items.push(TranscriptItem::Assistant {
                content: response.to_owned(),
                streaming: false,
            });
            return true;
        }
        false
    }

    fn finish_streaming(&mut self) -> bool {
        let mut changed = false;
        for index in &self.active_assistant_rows {
            if let Some(TranscriptItem::Assistant { streaming, .. }) = self.items.get_mut(*index) {
                changed |= *streaming;
                *streaming = false;
            }
        }
        changed
    }

    fn finish_model(&mut self, terminal: ActivityStatus) -> bool {
        if let Some(index) = self.active_model.take() {
            if let TranscriptItem::Activity(TranscriptActivity::Model { status, .. }) =
                &mut self.items[index]
            {
                *status = terminal;
                return true;
            }
        }
        false
    }

    fn set_tool_activity(&mut self, id: &str, activity: ActivityStatus) {
        if let Some(index) = self.tool_rows.get(id) {
            if let TranscriptItem::Tool {
                activity: current, ..
            } = &mut self.items[*index]
            {
                *current = activity;
            }
        }
    }

    fn flush_web_citations(&mut self) -> bool {
        let mut added = false;
        for (title, url) in self.pending_web_citations.drain(..) {
            if self.items.iter().rev().take(32).any(|item| {
                matches!(item, TranscriptItem::WebCitation { url: existing, .. } if existing == &url)
            }) {
                continue;
            }
            self.items.push(TranscriptItem::WebCitation { title, url });
            added = true;
        }
        added
    }

    fn reset_turn(&mut self) {
        self.finish_streaming();
        self.finish_model(ActivityStatus::Interrupted);
        self.finish_model_stream(ModelStreamStatus::Interrupted);
        self.clear_model_stream();
        for index in self.tool_rows.values() {
            if let TranscriptItem::Tool {
                activity,
                output: None,
                ..
            } = &mut self.items[*index]
            {
                // No terminal tool result exists. Cancelling the turn cannot
                // prove that a previously dispatched effect was cancelled.
                *activity = match *activity {
                    ActivityStatus::WaitingPermission => ActivityStatus::PermissionInterrupted,
                    ActivityStatus::Running => ActivityStatus::Interrupted,
                    terminal => terminal,
                };
            }
        }
        for index in self.hook_rows.values() {
            if let TranscriptItem::Hook(hook) = &mut self.items[*index] {
                if hook.decision.is_none() {
                    hook.interrupted = true;
                }
            }
        }
        self.hook_rows.clear();
        self.tool_rows.clear();
        self.tool_output_rows.clear();
        self.tool_media_rows.clear();
        self.active_assistant_rows.clear();
        self.active_assistant_committed = false;
        self.committed_final_assistant.clear();
        self.latest_model_step = None;
        self.stdout_row = None;
        self.stdout_text.clear();
        self.pending_web_citations.clear();
    }
}

fn valid_stream_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_STREAM_ID_BYTES
}

fn web_search_row_bytes(item: &TranscriptItem) -> usize {
    match item {
        TranscriptItem::WebSearch {
            action,
            query,
            url,
            sources,
            ..
        } => {
            action.len()
                + query.as_ref().map_or(0, String::len)
                + url.as_ref().map_or(0, String::len)
                + sources
                    .iter()
                    .map(|(title, url)| title.len() + url.len())
                    .sum::<usize>()
        }
        _ => 0,
    }
}

fn bounded_stream_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn append_stream_text(
    target: &mut String,
    delta: &str,
    row_cap: usize,
    request_bytes: &mut usize,
    truncated: &mut bool,
) -> bool {
    let available = row_cap
        .saturating_sub(target.len())
        .min(MAX_MODEL_STREAM_BYTES.saturating_sub(*request_bytes));
    let visible = bounded_stream_prefix(delta, available);
    let changed = !visible.is_empty() || !*truncated && visible.len() != delta.len();
    target.push_str(visible);
    *request_bytes += visible.len();
    *truncated |= visible.len() != delta.len();
    changed
}

#[cfg(test)]
#[path = "transcript_stream_tests.rs"]
mod stream_tests;
#[cfg(test)]
#[path = "transcript_web_tests.rs"]
mod web_tests;

#[cfg(test)]
#[path = "transcript_plan_tests.rs"]
mod plan_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder as _;
    use openide_core::{AgentMessage, SessionStore, ToolCall, TurnMode};
    use serde_json::json;

    fn event(sequence: u64, kind: AgentEventKind) -> AgentEvent {
        AgentEvent::new("session", sequence, kind)
    }

    fn timed(sequence: u64, timestamp_ms: u64, kind: AgentEventKind) -> AgentEvent {
        AgentEvent {
            timestamp_ms,
            ..event(sequence, kind)
        }
    }

    fn timed_turn(times: [u64; 5]) -> Vec<AgentEvent> {
        let call = ToolCall {
            id: "call".into(),
            name: "read_file".into(),
            arguments: json!({"path":"a.rs"}),
        };
        let reasoning = |content: &str| AgentEventKind::ModelStreamUpdate {
            step: 1,
            update: ModelStreamEvent::ReasoningDelta {
                item_id: "r".into(),
                content: content.into(),
            },
        };
        vec![
            timed(
                1,
                times[0],
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "model".into(),
                },
            ),
            timed(2, times[1], reasoning("a")),
            timed(3, times[2], reasoning("b")),
            timed(4, times[3], AgentEventKind::ToolStarted { tool_call: call }),
            timed(
                5,
                times[4],
                AgentEventKind::ToolFinished {
                    tool_call_id: "call".into(),
                    output: "ok".into(),
                    is_error: false,
                },
            ),
        ]
    }

    fn reasoning_id(projection: &TranscriptProjection) -> String {
        projection
            .items()
            .iter()
            .find_map(|item| match item {
                TranscriptItem::Reasoning { id, .. } => Some(id.clone()),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn durations_come_from_event_times_live_and_replayed() {
        // Long past: a wall clock read at replay would give a huge duration.
        let events = timed_turn([1_000, 3_000, 4_500, 5_000, 17_250]);
        let mut live = TranscriptProjection::new();
        let mut published = ItemTimes::default();
        for event in &events {
            live.apply_event(event);
            live.sync_times_into(&mut published);
        }
        let mut replayed = TranscriptProjection::from_events(&events);
        let mut restored = ItemTimes::default();
        replayed.sync_times_into(&mut restored);
        assert_eq!(published, restored);
        // Reasoning counts from the event before its first delta.
        assert_eq!(restored.duration_ms(&reasoning_id(&replayed)), Some(3_500));
        assert_eq!(restored.duration_ms("call"), Some(12_250));
        assert_eq!(restored.duration_ms("unknown"), None);
    }

    #[test]
    fn a_call_without_its_end_has_no_duration_and_a_backwards_clock_is_zero() {
        let mut events = timed_turn([9_000, 8_000, 7_000, 6_000, 5_000]);
        let mut backwards = TranscriptProjection::from_events(&events);
        let mut times = ItemTimes::default();
        backwards.sync_times_into(&mut times);
        assert_eq!(times.duration_ms(&reasoning_id(&backwards)), Some(0));
        assert_eq!(times.duration_ms("call"), Some(0));

        events.pop();
        let mut unfinished = TranscriptProjection::from_live_events(&events);
        unfinished.sync_times_into(&mut times);
        assert_eq!(times.duration_ms("call"), None);
    }

    #[test]
    fn duration_labels() {
        for (ms, label) in [
            (0, "un momento"),
            (999, "un momento"),
            (1_000, "1 s"),
            (12_400, "12 s"),
            (64_000, "1 min 4 s"),
            (120_000, "2 min"),
        ] {
            assert_eq!(duration_label(ms), label);
        }
    }

    fn image_fixture() -> UserImage {
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[255, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        UserImage::from_bytes(UserImageMime::Png, &png).unwrap()
    }

    #[test]
    fn admission_and_steer_images_match_live_and_replayed_rows() {
        let image = image_fixture();
        let events = vec![
            event(
                1,
                AgentEventKind::TurnAdmittedWithImages {
                    turn_id: "turn-1".into(),
                    prompt: "Describe".into(),
                    model: "model".into(),
                    mode: TurnMode::Plan,
                    images: vec![image.clone()],
                },
            ),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Describe".into(),
                },
            ),
            event(
                3,
                AgentEventKind::TurnSteeredWithImages {
                    turn_id: "turn-1".into(),
                    input_id: "input-1".into(),
                    text: "También esta".into(),
                    images: vec![image.clone()],
                },
            ),
            event(
                4,
                AgentEventKind::TurnCompleted {
                    response: "Listo".into(),
                    steps: 1,
                },
            ),
        ];
        let mut live = TranscriptProjection::new();
        for event in &events {
            live.apply_event(event);
        }
        let reopened = TranscriptProjection::from_events(&events);
        assert_eq!(live.items(), reopened.items());
        for (index, content) in [(0, "Describe"), (1, "También esta")] {
            let TranscriptItem::User {
                content: actual,
                images,
            } = &reopened.items()[index]
            else {
                panic!("expected user image row");
            };
            assert_eq!(actual, content);
            assert_eq!(images.len(), 1);
            assert_eq!(images[0].alt, "Imagen adjunta 1 de 1");
            assert_eq!(
                images[0].raster.as_ref().unwrap().data_base64(),
                image.data_base64
            );
        }
    }

    #[test]
    fn hook_activity_survives_store_reopen_and_manual_tool_resolution() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let kinds = vec![
            AgentEventKind::TurnStarted {
                prompt: "Build".into(),
            },
            AgentEventKind::ToolStarted {
                tool_call: ToolCall {
                    id: "call-1".into(),
                    name: "write_file".into(),
                    arguments: json!({"path":"note.txt"}),
                },
            },
            AgentEventKind::HookStarted {
                tool_call_id: "call-1".into(),
                hook_id: "pre".into(),
                phase: "pre_tool_use".into(),
            },
            AgentEventKind::HookDiagnosticCommitted {
                tool_call_id: "call-1".into(),
                hook_id: "pre".into(),
                phase: "pre_tool_use".into(),
                reason: Some("Approved".into()),
                stdout: Some("{\"decision\":\"allow\"}".into()),
                stderr: Some("Hook note".into()),
                error: None,
            },
            AgentEventKind::HookFinished {
                tool_call_id: "call-1".into(),
                hook_id: "pre".into(),
                phase: "pre_tool_use".into(),
                decision: "allow".into(),
            },
            AgentEventKind::HookStarted {
                tool_call_id: "call-1".into(),
                hook_id: "post".into(),
                phase: "post_tool_use".into(),
            },
            AgentEventKind::ToolFinished {
                tool_call_id: "call-1".into(),
                output: "Manually verified".into(),
                is_error: false,
            },
            AgentEventKind::TurnCompleted {
                response: "Done".into(),
                steps: 1,
            },
        ];
        let mut live = TranscriptProjection::new();
        for (index, kind) in kinds.into_iter().enumerate() {
            let event = AgentEvent::new(&session.id, index as u64 + 1, kind);
            live.apply_event(&event);
            store.append_event(&session.id, &event).unwrap();
        }
        drop(store);
        let reopened = SessionStore::open(root.path()).unwrap();
        let events = reopened.load(&session.id).unwrap().unwrap().events;
        let replayed = TranscriptProjection::from_events(&events);
        assert_eq!(live.items(), replayed.items());
        let hooks = hooks_for_tool(&events, "call-1");
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].decision.as_deref(), Some("allow"));
        assert_eq!(hooks[0].reason.as_deref(), Some("Approved"));
        assert_eq!(hooks[0].stderr.as_deref(), Some("Hook note"));
        assert_eq!(hooks[0].stdout.as_deref(), Some("{\"decision\":\"allow\"}"));
        assert!(!hooks[0].interrupted);
        assert!(hooks[1].interrupted);
        assert!(
            hooks[1].decision.is_none(),
            "manual tool result must not invent hook success"
        );
        assert!(hooks_for_tool(&events, "unrelated").is_empty());
        let mut next = events.clone();
        next.push(event(
            8,
            AgentEventKind::TurnStarted {
                prompt: "Next turn".into(),
            },
        ));
        assert!(
            hooks_for_tool(&next, "call-1").is_empty(),
            "recovery must not collect hooks from an earlier turn"
        );
    }

    #[test]
    fn pending_hook_changes_from_live_to_uncertain_after_interruption() {
        let events = vec![
            event(
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Build".into(),
                },
            ),
            event(
                2,
                AgentEventKind::HookStarted {
                    tool_call_id: "call".into(),
                    hook_id: "guard".into(),
                    phase: "pre_tool_use".into(),
                },
            ),
        ];
        let mut live = TranscriptProjection::new();
        for event in &events {
            live.apply_event(event);
        }
        let TranscriptItem::Hook(hook) = &live.items()[1] else {
            panic!("missing live hook");
        };
        assert!(!hook.interrupted);
        assert!(hook.decision.is_none());
        assert!(hooks_for_tool(&events, "call")[0].interrupted);
        live.apply_kind(&AgentEventKind::HookFinished {
            tool_call_id: "call".into(),
            hook_id: "guard".into(),
            phase: "pre_tool_use".into(),
            decision: "block".into(),
        });
        let TranscriptItem::Hook(hook) = &live.items()[1] else {
            panic!("missing result");
        };
        assert_eq!(hook.decision.as_deref(), Some("block"));
        assert!(!hook.interrupted);
    }

    #[test]
    fn image_admission_reopens_from_disk_without_original_file() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let image = image_fixture();
        store
            .admit_native_turn_with_images(
                &session.id,
                "turn-image",
                "Describe",
                "model",
                TurnMode::Plan,
                std::slice::from_ref(&image),
            )
            .unwrap();
        drop(store);
        let reopened = SessionStore::open(root.path()).unwrap();
        let events = reopened.load(&session.id).unwrap().unwrap().events;
        let projection = TranscriptProjection::from_events(&events);
        let TranscriptItem::User { images, .. } = &projection.items()[0] else {
            panic!("missing admitted user row");
        };
        assert_eq!(
            images[0].raster.as_ref().unwrap().data_base64(),
            image.data_base64
        );
        assert!(matches!(
            &projection.items()[1],
            TranscriptItem::Notice { .. }
        ));
    }

    #[test]
    fn malformed_live_image_keeps_label_for_render_boundary_validation() {
        let mut image = image_fixture();
        image.sha256 = "0".repeat(64);
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnAdmittedWithImages {
            turn_id: "turn-1".into(),
            prompt: "Describe".into(),
            model: "model".into(),
            mode: TurnMode::Build,
            images: vec![image],
        });
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Describe".into(),
        });
        let TranscriptItem::User { images, .. } = &projection.items()[0] else {
            unreachable!()
        };
        assert_eq!(images[0].alt, "Imagen adjunta 1 de 1");
        assert_eq!(images[0].raster.as_ref().unwrap().sha256(), "0".repeat(64));
    }

    #[test]
    fn saved_system_instructions_never_appear_in_transcript() {
        let content = "Instrucción privada del proyecto";
        let events = vec![
            event(
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Revisar código".into(),
                },
            ),
            event(
                2,
                AgentEventKind::SystemInstructionsCommitted {
                    version: openide_core::SYSTEM_INSTRUCTIONS_VERSION,
                    digest: openide_core::system_instructions_digest(content).unwrap(),
                    content: content.into(),
                },
            ),
        ];
        let mut projection = TranscriptProjection::new();
        for event in &events {
            projection.apply_event(event);
        }
        assert_eq!(
            projection.items(),
            &[TranscriptItem::User {
                content: "Revisar código".into(),
                images: Vec::new(),
            }]
        );
    }

    #[test]
    fn web_citation_survives_live_projection_and_reopen() {
        let mut assistant = AgentMessage::assistant("Encontré una fuente", vec![]);
        assistant.provider_output_items = vec![json!({
            "type": "message",
            "content": [{
                "type": "output_text",
                "text": "Encontré una fuente",
                "annotations": [{
                    "type": "url_citation",
                    "start_index": 0,
                    "end_index": 8,
                    "url": "https://example.com/source",
                    "title": "Fuente primaria"
                }]
            }]
        })];
        let events = vec![
            event(
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Busca".into(),
                },
            ),
            event(
                2,
                AgentEventKind::AssistantDelta {
                    content: "Encontré una fuente".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelResponseCommitted {
                    step: 1,
                    assistant,
                    response_id: Some("response-1".into()),
                    usage: None,
                },
            ),
            event(
                4,
                AgentEventKind::AssistantMessage {
                    content: "Encontré una fuente".into(),
                },
            ),
            event(
                5,
                AgentEventKind::TurnCompleted {
                    response: "Encontré una fuente".into(),
                    steps: 1,
                },
            ),
        ];
        let mut live = TranscriptProjection::new();
        for event in &events {
            live.apply_event(event);
        }
        let reopened = TranscriptProjection::from_events(&events);
        assert_eq!(live.items(), reopened.items());
        assert!(matches!(
            &live.items()[2],
            TranscriptItem::WebCitation { title, url }
                if title == "Fuente primaria" && url == "https://example.com/source"
        ));
    }

    #[test]
    fn groups_tool_calls_and_results_in_live_and_replayed_timeline() {
        let call = ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: json!({"path":"src/main.rs"}),
        };
        let events = vec![
            event(
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Read".into(),
                },
            ),
            event(2, AgentEventKind::ToolStarted { tool_call: call }),
            event(
                3,
                AgentEventKind::ToolFinished {
                    tool_call_id: "call-1".into(),
                    output: "fn main() {}".into(),
                    is_error: false,
                },
            ),
            event(
                4,
                AgentEventKind::TurnCompleted {
                    response: "Found it".into(),
                    steps: 2,
                },
            ),
        ];
        let mut live = TranscriptProjection::new();
        for event in &events {
            live.apply_event(event);
        }
        let replayed = TranscriptProjection::from_events(&events);
        assert_eq!(live.items(), replayed.items());
        assert_eq!(live.items().len(), 3);
        assert!(matches!(&live.items()[1], TranscriptItem::Tool {
            id, name, arguments, output: Some(output), is_error: false, ..
        } if id == "call-1" && name == "read_file"
            && arguments == &json!({"path":"src/main.rs"}) && output == "fn main() {}"));
    }

    #[test]
    fn streaming_deltas_are_replaced_by_final_answer_without_duplicate_row() {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Hi".into(),
        });
        projection.apply_kind(&AgentEventKind::AssistantDelta {
            content: "Hel".into(),
        });
        projection.apply_kind(&AgentEventKind::AssistantDelta {
            content: "lo".into(),
        });
        assert!(matches!(&projection.items()[1], TranscriptItem::Assistant {
            content, streaming: true,
        } if content == "Hello"));
        projection.apply_kind(&AgentEventKind::AssistantMessage {
            content: "Hello".into(),
        });
        projection.apply_kind(&AgentEventKind::TurnCompleted {
            response: "Hello!".into(),
            steps: 1,
        });
        assert_eq!(projection.items().len(), 2);
        assert_eq!(
            projection.items()[1],
            TranscriptItem::Assistant {
                content: "Hello!".into(),
                streaming: false,
            }
        );
    }

    #[test]
    fn cli_stdout_becomes_one_final_response_and_stderr_remains_visible() {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Run".into(),
        });
        projection.apply_kind(&AgentEventKind::ProcessOutput {
            stream: OutputStream::Stdout,
            text: "Good ".into(),
        });
        projection.apply_kind(&AgentEventKind::ProcessOutput {
            stream: OutputStream::Stderr,
            text: "warning".into(),
        });
        projection.apply_kind(&AgentEventKind::ProcessOutput {
            stream: OutputStream::Stdout,
            text: "morning".into(),
        });
        assert!(
            matches!(&projection.items()[1], TranscriptItem::ProcessOutput {
            stream: OutputStream::Stdout, text,
        } if text == "Good morning")
        );
        projection.apply_kind(&AgentEventKind::TurnCompleted {
            response: "Good morning".into(),
            steps: 1,
        });
        assert_eq!(projection.items().len(), 3);
        assert_eq!(
            projection.items()[1],
            TranscriptItem::Assistant {
                content: "Good morning".into(),
                streaming: false,
            }
        );
        assert!(
            matches!(&projection.items()[2], TranscriptItem::ProcessOutput {
            stream: OutputStream::Stderr, text,
        } if text == "warning")
        );
    }

    #[test]
    fn truncated_cli_capture_does_not_duplicate_streamed_output() {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Run".into(),
        });
        projection.apply_kind(&AgentEventKind::ProcessOutput {
            stream: OutputStream::Stdout,
            text: "complete streamed output".into(),
        });
        projection.apply_kind(&AgentEventKind::TurnCompleted {
            response: "complete\n[output truncated]".into(),
            steps: 1,
        });
        assert_eq!(projection.items().len(), 2);
        assert_eq!(
            projection.items()[1],
            TranscriptItem::Assistant {
                content: "complete streamed output".into(),
                streaming: false,
            }
        );
    }

    #[test]
    fn replay_marks_crashed_turn_as_interrupted() {
        let events = vec![
            event(
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Run".into(),
                },
            ),
            event(
                2,
                AgentEventKind::AssistantDelta {
                    content: "Partial".into(),
                },
            ),
        ];
        let projection = TranscriptProjection::from_events(&events);
        assert_eq!(projection.items().len(), 3);
        assert!(matches!(&projection.items()[1], TranscriptItem::Assistant {
            content, streaming: false,
        } if content == "Partial"));
        assert!(matches!(
            &projection.items()[2],
            TranscriptItem::Notice { is_error: true, .. }
        ));
    }

    #[test]
    fn replay_marks_unfinished_tool_effect_as_uncertain() {
        let events = vec![
            event(
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Change file".into(),
                },
            ),
            event(
                2,
                AgentEventKind::ToolStarted {
                    tool_call: ToolCall {
                        id: "call-1".into(),
                        name: "write_file".into(),
                        arguments: json!({"path":"file.txt","content":"x"}),
                    },
                },
            ),
        ];
        let projection = TranscriptProjection::from_events(&events);
        assert!(matches!(
            &projection.items()[1],
            TranscriptItem::Tool { output: None, .. }
        ));
        assert!(matches!(
            &projection.items()[2],
            TranscriptItem::Notice { message, is_error: true }
                if message.contains("resultado es incierto")
                    && message.contains("No se reintentará automáticamente")
        ));
    }

    #[test]
    fn replay_shows_admitted_prompt_when_worker_never_started() {
        let events = vec![event(
            1,
            AgentEventKind::TurnAdmitted {
                turn_id: "turn-1".into(),
                prompt: "Inspect the workspace".into(),
                model: "model-a".into(),
                mode: TurnMode::Plan,
            },
        )];
        let projection = TranscriptProjection::from_events(&events);
        assert_eq!(projection.items().len(), 2);
        assert_eq!(
            projection.items()[0],
            TranscriptItem::User {
                content: "Inspect the workspace".into(),
                images: Vec::new(),
            }
        );
        assert!(matches!(
            &projection.items()[1],
            TranscriptItem::Notice { is_error: true, .. }
        ));
    }

    #[test]
    fn admitted_then_started_turn_projects_one_user_prompt() {
        let events = vec![
            event(
                1,
                AgentEventKind::TurnAdmitted {
                    turn_id: "turn-1".into(),
                    prompt: "Inspect the workspace".into(),
                    model: "model-a".into(),
                    mode: TurnMode::Plan,
                },
            ),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Inspect the workspace".into(),
                },
            ),
            event(
                3,
                AgentEventKind::TurnCompleted {
                    response: "Done".into(),
                    steps: 1,
                },
            ),
        ];
        let projection = TranscriptProjection::from_events(&events);
        assert_eq!(projection.items().len(), 2);
        assert!(matches!(
            &projection.items()[0],
            TranscriptItem::User { .. }
        ));
        assert!(matches!(
            &projection.items()[1],
            TranscriptItem::Assistant { .. }
        ));
    }

    #[test]
    fn cancellation_preserves_partial_output_and_adds_notice() {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Long task".into(),
        });
        projection.apply_kind(&AgentEventKind::AssistantDelta {
            content: "Partial".into(),
        });
        projection.apply_kind(&AgentEventKind::TurnCancelled);
        assert_eq!(projection.items().len(), 3);
        assert_eq!(
            projection.items()[1],
            TranscriptItem::Assistant {
                content: "Partial".into(),
                streaming: false,
            }
        );
        assert_eq!(
            projection.items()[2],
            TranscriptItem::Notice {
                message: "Turno cancelado".into(),
                is_error: false,
            }
        );
    }

    #[test]
    fn discarded_batch_is_visible_without_synthesizing_tool_results() {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Edit notes".into(),
        });
        projection.apply_kind(&AgentEventKind::TurnCancelled);
        projection.apply_kind(&AgentEventKind::ToolBatchDiscarded {
            next_tool_call_id: "call-2".into(),
        });
        assert_eq!(projection.items().len(), 3);
        assert!(matches!(
            &projection.items()[2],
            TranscriptItem::Notice { message, is_error: false }
                if message.contains("call-2")
        ));
    }

    fn assistant_rows(projection: &TranscriptProjection) -> Vec<(&str, bool)> {
        projection
            .items()
            .iter()
            .filter_map(|row| match row {
                TranscriptItem::Assistant { content, streaming } => {
                    Some((content.as_str(), *streaming))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn committed_response_cut_reopens_full_content_citations_and_recovers_without_duplicate() {
        for (with_delta, with_tools) in [(false, false), (true, false), (false, true), (true, true)]
        {
            let root = tempfile::tempdir().unwrap();
            let store = SessionStore::open(root.path()).unwrap();
            let session = store.create(root.path(), "native").unwrap();
            let admission = store
                .admit_native_turn(
                    &session.id,
                    "cut-turn",
                    "Busca",
                    "exact/model",
                    TurnMode::Build,
                )
                .unwrap();
            let call = ToolCall {
                id: "read-1".into(),
                name: "read_file".into(),
                arguments: json!({"path":"note.txt"}),
            };
            let full = "Respuesta completa ñ🙂 con fuente 終わり";
            let mut assistant = AgentMessage::assistant(
                full,
                if with_tools {
                    vec![call.clone()]
                } else {
                    Vec::new()
                },
            );
            assistant.provider_output_items = vec![json!({
                "type":"message", "content":[{"type":"output_text", "text":full,
                "annotations":[{"type":"url_citation", "start_index":0, "end_index":9,
                "url":"https://example.com/primary", "title":"Fuente guardada"}]}]
            })];
            let mut sequence = admission.sequence;
            let mut append = |kind| {
                sequence += 1;
                let event = AgentEvent::new(&session.id, sequence, kind);
                store.append_native_event(&event).unwrap();
                event
            };
            append(AgentEventKind::TurnStarted {
                prompt: "Busca".into(),
            });
            append(AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "exact/model".into(),
            });
            if with_delta {
                append(AgentEventKind::AssistantDelta {
                    content: "Respuesta incompleta".into(),
                });
            }
            append(AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant,
                response_id: Some("saved-response".into()),
                usage: None,
            });
            let saved = store.load(&session.id).unwrap().unwrap().events;
            let journal_path = root.path().join(format!("{}.jsonl", session.id));
            let bytes = std::fs::read(&journal_path).unwrap();
            drop(store);
            let reopened = SessionStore::open(root.path()).unwrap();
            let record = reopened.load(&session.id).unwrap().unwrap();
            assert_eq!(record.events, saved);
            let compatibility = openide_core::CheckpointCompatibility {
                provider: "fixture".into(),
                model: "exact/model".into(),
                context_format_key: "chat-completions-v1".into(),
            };
            let replay =
                openide_core::replay_native(&session.id, &record.events, None, &compatibility)
                    .unwrap();
            if with_tools {
                assert!(
                    matches!(&replay.recovery, openide_core::ReplayRecovery::ToolReady { tool_call } if tool_call == &call)
                );
                assert_eq!(
                    replay
                        .pending_tool_batch
                        .as_ref()
                        .unwrap()
                        .assistant
                        .content,
                    full
                );
            } else {
                assert!(matches!(
                    replay.recovery,
                    openide_core::ReplayRecovery::ResponseCommitted { step: 1 }
                ));
                assert_eq!(replay.history.last().unwrap().content, full);
            }
            let mut projection = TranscriptProjection::from_events(&record.events);
            assert_eq!(
                assistant_rows(&projection),
                [(full, false)],
                "cut with delta={with_delta}, tools={with_tools}"
            );
            let assistant_index = projection
                .items()
                .iter()
                .position(|row| matches!(row, TranscriptItem::Assistant { .. }))
                .unwrap();
            assert!(
                matches!(&projection.items()[assistant_index+1], TranscriptItem::WebCitation { title, url } if title == "Fuente guardada" && url == "https://example.com/primary")
            );
            assert!(projection.items().iter().any(|row| matches!(
                row,
                TranscriptItem::Activity(TranscriptActivity::Model {
                    step: 1,
                    has_text: true,
                    status: ActivityStatus::Completed,
                    ..
                })
            )));
            assert_eq!(
                std::fs::read(&journal_path).unwrap(),
                bytes,
                "Opening and projecting never rewrites the journal"
            );

            let mut append = |kind| {
                sequence += 1;
                let event = AgentEvent::new(&session.id, sequence, kind);
                reopened.append_native_event(&event).unwrap();
                event
            };
            projection.apply_event(&append(AgentEventKind::AssistantMessage {
                content: full.into(),
            }));
            assert_eq!(
                assistant_rows(&projection),
                [(full, false)],
                "Later display event is redundant after a reopen"
            );
            let final_answer = if with_tools {
                "Resultado final después de leer"
            } else {
                full
            };
            if with_tools {
                projection.apply_event(&append(AgentEventKind::ToolStarted {
                    tool_call: call.clone(),
                }));
                projection.apply_event(&append(AgentEventKind::ToolFinished {
                    tool_call_id: call.id,
                    output: "Texto leído".into(),
                    is_error: false,
                }));
                projection.apply_event(&append(AgentEventKind::ModelRequestStarted {
                    step: 2,
                    model: "exact/model".into(),
                }));
                projection.apply_event(&append(AgentEventKind::ModelResponseCommitted {
                    step: 2,
                    assistant: AgentMessage::assistant(final_answer, vec![]),
                    response_id: None,
                    usage: None,
                }));
                projection.apply_event(&append(AgentEventKind::AssistantMessage {
                    content: final_answer.into(),
                }));
            }
            projection.apply_event(&append(AgentEventKind::TurnCompleted {
                response: final_answer.into(),
                steps: if with_tools { 2 } else { 1 },
            }));
            let expected = if with_tools {
                vec![(full, false), (final_answer, false)]
            } else {
                vec![(full, false)]
            };
            assert_eq!(assistant_rows(&projection), expected);
            assert_eq!(
                projection
                    .items()
                    .iter()
                    .filter(|row| matches!(row, TranscriptItem::WebCitation { .. }))
                    .count(),
                1
            );
            let final_record = reopened.load(&session.id).unwrap().unwrap();
            assert_eq!(&final_record.events[..saved.len()], &saved);
            assert!(matches!(
                openide_core::replay_native(
                    &session.id,
                    &final_record.events,
                    None,
                    &compatibility
                )
                .unwrap()
                .recovery,
                openide_core::ReplayRecovery::Idle
            ));
            assert_eq!(
                assistant_rows(&TranscriptProjection::from_events(&final_record.events)),
                expected
            );

            // A new turn owns a new response row even when its content is the
            // same; old committed answers are never reused across turns.
            projection.apply_kind(&AgentEventKind::TurnStarted {
                prompt: "Siguiente".into(),
            });
            projection.apply_kind(&AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "exact/model".into(),
            });
            projection.apply_kind(&AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant(full, vec![]),
                response_id: None,
                usage: None,
            });
            assert_eq!(assistant_rows(&projection).len(), expected.len() + 1);
            assert_eq!(&assistant_rows(&projection)[..expected.len()], expected);
        }
    }

    #[test]
    fn committed_tool_only_response_removes_unconfirmed_text_and_keeps_row_indices() {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Read".into(),
        });
        projection.apply_kind(&AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "exact/model".into(),
        });
        projection.apply_kind(&AgentEventKind::AssistantDelta {
            content: "Unconfirmed commentary".into(),
        });
        projection.apply_kind(&AgentEventKind::ProcessOutput {
            stream: OutputStream::Stdout,
            text: "Background process output".into(),
        });
        projection.apply_kind(&AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant(
                "",
                vec![ToolCall {
                    id: "read".into(),
                    name: "read_file".into(),
                    arguments: json!({"path":"file"}),
                }],
            ),
            response_id: None,
            usage: None,
        });
        assert!(assistant_rows(&projection).is_empty());
        assert!(projection.items().iter().any(|row| matches!(
            row,
            TranscriptItem::Activity(TranscriptActivity::Model {
                step: 1,
                has_text: false,
                status: ActivityStatus::Completed,
                ..
            })
        )));
        projection.apply_kind(&AgentEventKind::ProcessOutput {
            stream: OutputStream::Stdout,
            text: " next".into(),
        });
        assert_eq!(
            projection
                .items()
                .iter()
                .filter(|row| matches!(row, TranscriptItem::ProcessOutput { .. }))
                .count(),
            1
        );
        assert!(projection.items().iter().any(|row| matches!(row,TranscriptItem::ProcessOutput { text,.. } if text == "Background process output next")));
    }

    #[test]
    fn response_commit_cannot_overwrite_a_newer_model_step_or_its_citations() {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Read".into(),
        });
        projection.apply_kind(&AgentEventKind::ModelRequestStarted {
            step: 2,
            model: "exact/model".into(),
        });
        projection.apply_kind(&AgentEventKind::AssistantDelta {
            content: "Current step".into(),
        });
        let before = projection.items().to_vec();
        assert!(
            !projection.apply_kind(&AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("Stale response", vec![]),
                response_id: None,
                usage: None
            })
        );
        assert_eq!(projection.items(), before);
        projection.apply_kind(&AgentEventKind::ModelResponseCommitted {
            step: 2,
            assistant: AgentMessage::assistant("Authoritative response", vec![]),
            response_id: None,
            usage: None,
        });
        projection.apply_kind(&AgentEventKind::AssistantMessage {
            content: "Different redundant display".into(),
        });
        projection.apply_kind(&AgentEventKind::AssistantDelta {
            content: "Late delta".into(),
        });
        assert_eq!(
            assistant_rows(&projection),
            [("Authoritative response", false)]
        );
    }

    #[derive(Clone, Copy)]
    enum PlanCut {
        Response,
        Proposal,
    }

    struct PlanCutJournal {
        store: std::sync::Arc<SessionStore>,
        cut_after: Option<PlanCut>,
        cut: std::sync::atomic::AtomicBool,
    }

    impl openide_core::NativeEventJournal for PlanCutJournal {
        fn append_native_event(&self, event: &AgentEvent) -> Result<(), openide_core::StoreError> {
            if self.cut.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(std::io::Error::other("plan fixture journal is cut").into());
            }
            self.store.append_native_event(event)?;
            if matches!(
                (self.cut_after, &event.kind),
                (
                    Some(PlanCut::Response),
                    AgentEventKind::ModelResponseCommitted { .. }
                ) | (Some(PlanCut::Proposal), AgentEventKind::PlanProposed { .. })
            ) {
                self.cut.store(true, std::sync::atomic::Ordering::SeqCst);
                return Err(
                    std::io::Error::other("plan fixture cut after persisted marker").into(),
                );
            }
            Ok(())
        }
    }

    struct ProjectionPlanModel(std::sync::atomic::AtomicUsize);

    const PLAN_RESPONSE: &str = "1. Revisar archivos ñ🙂\n2. Implementar el cambio 終わり";

    #[async_trait::async_trait]
    impl openide_harness::ModelBackend for ProjectionPlanModel {
        fn model_id(&self) -> &str {
            "plan/exact-model"
        }

        fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
            Some([9; 32])
        }

        async fn complete(
            &self,
            _messages: &[AgentMessage],
            _tools: &[openide_harness::ToolDefinition],
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<openide_harness::ModelOutput, openide_harness::AgentError> {
            panic!("Plan projection acceptance uses the streaming engine path")
        }

        async fn stream_complete_detailed(
            &self,
            _messages: &[AgentMessage],
            _tools: &[openide_harness::ToolDefinition],
            _cancel: tokio_util::sync::CancellationToken,
            deltas: tokio::sync::mpsc::UnboundedSender<String>,
        ) -> Result<openide_harness::ModelCompletion, openide_harness::AgentError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            deltas.send("1. Revisar archivos ñ🙂\n".into()).unwrap();
            deltas
                .send("2. Implementar el cambio 終わり".into())
                .unwrap();
            Ok(openide_harness::ModelOutput {
                content: PLAN_RESPONSE.into(),
                tool_calls: Vec::new(),
            }
            .into())
        }
    }

    #[test]
    fn plan_runner_and_resume_commit_cut_preserve_text_and_structured_card_without_duplicates() {
        use openide_core::{
            CheckpointCompatibility, CheckpointFileStore, NativeReplay, ReplayRecovery,
        };
        use openide_harness::{
            AgentRunner, LocalTools, PermissionPolicy, ResumeEngine, ToolCatalog,
        };
        use std::sync::{atomic::Ordering, Arc};

        let plan_frame = |workspace: &std::path::Path,
                          session_id: &str,
                          events: &[AgentEvent],
                          replay: &NativeReplay,
                          width| {
            let mut state = crate::view::tests::empty_state();
            state.main_page = crate::view::MainPage::Workspace;
            state.workspace = workspace.display().to_string();
            state.selected_session = Some(session_id.into());
            state.selected_backend = "native".into();
            state.running = false;
            state.transcript = TranscriptProjection::from_events(events).items().to_vec();
            if let Some(snapshot) = replay.latest_plan.clone() {
                state.session_plans.insert(
                    session_id.into(),
                    crate::view::SessionPlanView {
                        snapshot,
                        can_build: matches!(replay.recovery, ReplayRecovery::Idle),
                    },
                );
            }
            openide_ui::UiTree::new(crate::session_surface::build_session_surface(
                &state, width, false, false,
            ))
            .layout(openide_ui::Size::new(width, 950.0))
        };
        for cut_after in [None, Some(PlanCut::Response), Some(PlanCut::Proposal)] {
            let root = tempfile::tempdir().unwrap();
            let store = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
            let session = store.create(root.path(), "native").unwrap();
            let admission = store
                .admit_native_turn(
                    &session.id,
                    "plan-turn",
                    "Preparar el plan",
                    "plan/exact-model",
                    TurnMode::Plan,
                )
                .unwrap();
            let tools = Arc::new(LocalTools::new());
            let model = Arc::new(ProjectionPlanModel(std::sync::atomic::AtomicUsize::new(0)));
            let journal = Arc::new(PlanCutJournal {
                store: store.clone(),
                cut_after,
                cut: std::sync::atomic::AtomicBool::new(false),
            });
            let handle = AgentRunner::new(model.clone(), tools.clone())
                .with_native_runtime_identity("fixture", "https://plan.example/v1")
                .with_event_journal(journal)
                .start_with_lease(
                    openide_core::AgentRunRequest {
                        session_id: Some(session.id.clone()),
                        sequence_start: admission.sequence,
                        workspace: root.path().into(),
                        history: Vec::new(),
                        prompt: "Preparar el plan".into(),
                        mode: TurnMode::Plan,
                    },
                    store.try_acquire_native_lease(&session.id).unwrap(),
                );
            let mut live = TranscriptProjection::new();
            loop {
                match handle
                    .events
                    .recv_timeout(std::time::Duration::from_secs(5))
                {
                    Ok(event) => {
                        live.apply_event(&event);
                        if matches!(event.kind, AgentEventKind::ModelResponseCommitted { .. }) {
                            assert_eq!(assistant_rows(&live), [(PLAN_RESPONSE, false)]);
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(error) => panic!("Plan worker stalled: {error}"),
                }
            }
            assert_eq!(handle.journal_error().is_some(), cut_after.is_some());
            let prefix = store.load(&session.id).unwrap().unwrap().events;
            drop(handle);
            drop(store);
            let store = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
            let checkpoints =
                Arc::new(CheckpointFileStore::new(root.path().join("sessions")).unwrap());
            let compatibility = CheckpointCompatibility {
                provider: "fixture".into(),
                model: "plan/exact-model".into(),
                context_format_key: "fixture-v1".into(),
            };
            let saved = store.load(&session.id).unwrap().unwrap();
            assert_eq!(saved.events, prefix);
            let replay = openide_core::replay_native(
                &session.id,
                &saved.events,
                Some(&checkpoints),
                &compatibility,
            )
            .unwrap();
            let mut projection = TranscriptProjection::from_events(&saved.events);
            assert_eq!(assistant_rows(&projection), [(PLAN_RESPONSE, false)]);
            assert_eq!(
                replay.latest_plan.is_some(),
                !matches!(cut_after, Some(PlanCut::Response))
            );
            if cut_after.is_some() {
                assert!(matches!(
                    replay.recovery,
                    ReplayRecovery::ResponseCommitted { step: 1 }
                ));
                let frame = plan_frame(root.path(), &session.id, &saved.events, &replay, 360.0);
                assert!(
                    !frame
                        .regions
                        .iter()
                        .any(|region| region.id == format!("session_build_plan:{}", session.id)),
                    "An interrupted Plan cannot build yet"
                );
                let engine = ResumeEngine::new(
                    store.clone(),
                    checkpoints,
                    compatibility.clone(),
                    model.clone(),
                    tools.clone(),
                    ToolCatalog::from_executor(tools.as_ref()).unwrap(),
                    PermissionPolicy::new(),
                    None::<openide_harness::TokenBudgetPolicy>,
                )
                .with_native_runtime_identity("fixture", "https://plan.example/v1");
                let lease = store.try_acquire_native_lease(&session.id).unwrap();
                let step = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(
                        engine.resume_step(&lease, tokio_util::sync::CancellationToken::new()),
                    )
                    .unwrap();
                assert_eq!(
                    step.disposition,
                    openide_harness::ResumeDisposition::Complete
                );
                for event in &step.appended_events {
                    projection.apply_event(event);
                }
                assert_eq!(assistant_rows(&projection), [(PLAN_RESPONSE, false)]);
            }
            let finished = store.load(&session.id).unwrap().unwrap().events;
            assert_eq!(&finished[..prefix.len()], &prefix);
            assert_eq!(
                finished
                    .iter()
                    .filter(|event| matches!(event.kind, AgentEventKind::PlanProposed { .. }))
                    .count(),
                1
            );
            assert_eq!(
                finished
                    .iter()
                    .filter(|event| matches!(event.kind, AgentEventKind::AssistantMessage { .. }))
                    .count(),
                1
            );
            let replay =
                openide_core::replay_native(&session.id, &finished, None, &compatibility).unwrap();
            assert!(matches!(replay.recovery, ReplayRecovery::Idle));
            assert_eq!(
                model.0.load(Ordering::SeqCst),
                1,
                "Confirmed Plan response never calls the model again during Resume"
            );
            let snapshot = replay.latest_plan.as_ref().unwrap();
            assert_eq!(snapshot.plan.len(), 2);
            assert_eq!(snapshot.explanation.as_deref(), Some(PLAN_RESPONSE));
            assert_eq!(
                assistant_rows(&TranscriptProjection::from_events(&finished)),
                [(PLAN_RESPONSE, false)]
            );
            for width in [360.0, 900.0] {
                let frame = plan_frame(root.path(), &session.id, &finished, &replay, width);
                assert!(frame
                    .regions
                    .iter()
                    .any(|region| region.id == format!("session_build_plan:{}", session.id)));
                assert!(frame
                    .text_layouts()
                    .iter()
                    .any(|text| text.source.contains("Revisar archivos ñ🙂")));
                assert!(frame
                    .text_layouts()
                    .iter()
                    .any(|text| text.source.contains("Implementar el cambio 終わり")));
                assert!(
                    !frame
                        .text_layouts()
                        .iter()
                        .any(|text| text.source == serde_json::to_string(snapshot).unwrap()),
                    "The structured card renders steps, not serialized Plan metadata"
                );
            }
        }
    }
}

pub(crate) fn continuation_notice(model: &str) -> String {
    format!("Continúa en {model} desde donde quedó")
}

/// The words for something the host did on its own.
pub(crate) fn host_notice_text(code: &str, detail: &str) -> String {
    match code {
        openide_core::NOTICE_OUTPUT_LIMIT => format!(
            "{detail} llegó a su límite de salida varias veces seguidas: la respuesta quedó incompleta"
        ),
        openide_core::NOTICE_CONTEXT_SUMMARIZED => format!(
            "La conversación no entraba en {detail}: se resumió antes de continuar"
        ),
        openide_core::NOTICE_CONTEXT_TRIMMED => format!(
            "La conversación no entraba en {detail}: se recortaron salidas de herramientas antiguas"
        ),
        openide_core::NOTICE_CONTEXT_LIMIT => format!(
            "El proveedor informó su límite de contexto: {detail} tokens. Se usa desde ahora para este modelo"
        ),
        _ => detail.to_owned(),
    }
}
