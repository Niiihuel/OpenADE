//! Strict replay of a native session log. The returned model history contains
//! only complete assistant/tool pairs; interrupted effects remain explicit.

use crate::{
    system_instructions_digest, validate_user_images, AgentEvent, AgentEventKind, AgentMessage,
    Checkpoint, CheckpointCompatibility, CheckpointError, CheckpointFileStore, CheckpointReference,
    MessageRole, ModelTokenUsage, PlanSnapshot, ToolCall, TurnMode, UserImage,
    EXEC_POLICY_IDENTITY_VERSION, HOOK_CONFIG_IDENTITY_VERSION, MCP_RUNTIME_IDENTITY_VERSION, MODEL_CONTEXT_VERSION,
    SYSTEM_INSTRUCTIONS_VERSION,
};
use std::collections::{HashSet, VecDeque};

const MAX_CONCURRENT_READ_STARTS: usize = 4;

fn trusted_read_name(name: &str) -> bool {
    matches!(name, "read_file" | "list_files" | "search_files")
}
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("session event sequence is missing, repeated, or out of order at {0}")]
    InvalidSequence(u64),
    #[error("event belongs to another session at sequence {0}")]
    SessionMismatch(u64),
    #[error("native replay conflict at sequence {sequence}: {reason}")]
    Conflict { sequence: u64, reason: &'static str },
    #[error("checkpoint marker is invalid at sequence {0}")]
    InvalidCheckpointMarker(u64),
    #[error("checkpoint is incompatible with the active provider/model")]
    IncompatibleCheckpoint,
    #[error("a committed checkpoint requires a checkpoint store")]
    MissingCheckpointStore,
    #[error("committed checkpoint failed verification: {0}")]
    Checkpoint(#[from] CheckpointError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayedTurn {
    pub turn_id: Option<String>,
    pub prompt: String,
    pub model: Option<String>,
    pub mode: TurnMode,
    pub started: bool,
    pub last_step: usize,
    pub terminated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReplayedToolBatch {
    pub assistant: AgentMessage,
    pub assistant_event_sequence: u64,
    /// Results already durably recorded, in the order of the assistant calls.
    pub completed_results: Vec<AgentMessage>,
    pub completed_result_sequences: Vec<u64>,
    /// Index of the next call without a committed result.
    pub next_call_index: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReplayedMessage {
    pub event_sequence: u64,
    pub message: AgentMessage,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReplayRecovery {
    Idle,
    AdmittedNotStarted,
    ReadyForModel {
        next_step: usize,
    },
    ModelInFlight {
        step: usize,
        model: String,
    },
    ResponseCommitted {
        step: usize,
    },
    PermissionPending {
        request_id: String,
        tool_call: ToolCall,
    },
    ToolReady {
        tool_call: ToolCall,
    },
    /// A tool may have made external changes, but no result was committed.
    ToolOutcomeUncertain {
        tool_call: ToolCall,
    },
    /// A failed/cancelled turn has unfinished calls. Never auto-execute them.
    AbortedToolBatch {
        next_tool_call: ToolCall,
    },
}

/// The cause recorded by the last `TurnPaused` of the open turn.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayPause {
    pub class: crate::TurnPauseClass,
    pub limit_key: String,
    pub resets_at_ms: Option<u64>,
    pub step: usize,
    pub detail: String,
    pub event_sequence: u64,
    pub paused_at_ms: u64,
    /// `TurnPaused` events of this turn so far, this one included.
    pub pauses_in_turn: u32,
}

/// A switch to another model that closed its turn and has not yet admitted
/// the one that continues it.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplaySuperseded {
    pub provider_id: String,
    pub model_id: String,
    pub next_turn_id: String,
    pub host_prompt: bool,
    /// Where the superseded turn's own prompt (and images) were admitted.
    pub prompt_event_sequence: Option<u64>,
    pub event_sequence: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NativeReplay {
    pub session_id: String,
    pub last_sequence: u64,
    /// Safe model history. A checkpoint summary is an assistant message, not
    /// a high-priority system instruction.
    pub history: Vec<AgentMessage>,
    /// Exact route of the materialized provider items. Unlike active authority,
    /// this survives completion; None means their provenance is unknown.
    pub history_model_route: Option<crate::ModelRouteSnapshot>,
    /// Complete model messages after the selected checkpoint, with their
    /// source event sequence for a subsequent semantic compaction request.
    pub committed_messages: Vec<ReplayedMessage>,
    pub checkpoint: Option<Checkpoint>,
    /// Latest complete plan snapshot, independent of compacted model history.
    pub latest_plan: Option<PlanSnapshot>,
    /// Snapshot presented at the start of the interrupted turn. Later
    /// update_plan calls do not rewrite that frozen model context.
    pub active_plan_at_turn_start: Option<PlanSnapshot>,
    pub active_turn: Option<ReplayedTurn>,
    pub recovery: ReplayRecovery,
    /// Set while the open turn is paused for a known cause. It marks the
    /// same recovery state as clean; it never changes that state.
    pub pause: Option<ReplayPause>,
    /// Set from a `TurnSuperseded` until the next turn is admitted.
    pub superseded: Option<ReplaySuperseded>,
    pub pending_tool_batch: Option<ReplayedToolBatch>,
    pub latest_response_id: Option<String>,
    pub latest_usage: Option<ModelTokenUsage>,
    /// None for older logs that did not commit a catalog identity.
    pub active_tool_catalog_digest: Option<String>,
    /// Strong stdio MCP runtime snapshot for this turn. Its absence keeps
    /// pending MCP calls in fail-closed recovery.
    pub active_mcp_runtime_digest: Option<String>,
    pub active_mcp_runtime_version: Option<u16>,
    /// Ordered command hook policy used for this turn, including an empty policy.
    pub active_hooks_digest: Option<String>,
    pub active_hooks_version: Option<u16>,
    /// Effective command approval policy committed before this turn's model.
    pub active_exec_policy_digest: Option<String>,
    pub active_exec_policy_version: Option<u16>,
    /// Exact host/project text committed for this turn. Legacy logs have None.
    pub active_system_instructions: Option<String>,
    pub active_system_instructions_version: Option<u16>,
    /// None for legacy logs; recovery cannot resend an earlier model request
    /// unless this versioned model environment was committed first.
    pub active_model_context_digest: Option<String>,
    /// Version 1 logs remain readable but cannot prove the model request
    /// profile; recovery must require MODEL_CONTEXT_VERSION.
    pub active_model_context_version: Option<u16>,
    /// Explicit route of this active turn. Legacy turns have no attribution.
    pub active_model_route: Option<crate::ModelRouteSnapshot>,
    /// Starting a new turn while an interrupted turn is unresolved is unsafe.
    pub can_start_new_turn: bool,
}

struct ModelFlight {
    step: usize,
    model: String,
}

struct PendingPermission {
    request_id: String,
    tool_call: ToolCall,
}

struct PendingHook {
    tool_call_id: String,
    hook_id: String,
    phase: String,
    diagnostic_recorded: bool,
}

struct ReplayBuilder {
    history: Vec<AgentMessage>,
    committed_messages: Vec<ReplayedMessage>,
    record_after_sequence: u64,
    active_turn: Option<ReplayedTurn>,
    latest_plan: Option<PlanSnapshot>,
    active_plan_at_turn_start: Option<PlanSnapshot>,
    plan_proposed_this_turn: bool,
    plan_updated_for_started_tool: bool,
    active_prompt_event_sequence: Option<u64>,
    turn_has_committed_response: bool,
    model_flight: Option<ModelFlight>,
    pause: Option<ReplayPause>,
    superseded: Option<ReplaySuperseded>,
    pauses_in_turn: u32,
    pending_batch: Option<ReplayedToolBatch>,
    pending_permission: Option<PendingPermission>,
    /// Consecutive synced starts in one read-only wave. Results are committed
    /// in this order; an interrupted suffix remains uncertain by call ID.
    started_tools: VecDeque<ToolCall>,
    /// The images the started tool's result gives the model, once its media
    /// was seen; its ToolFinished takes them.
    media_for_started_tool: Option<Vec<UserImage>>,
    pending_hook: Option<PendingHook>,
    seen_hook_ids_for_call: HashSet<String>,
    read_wave_result_seen: bool,
    duplicate_assistant_content: Option<String>,
    last_assistant_content: Option<String>,
    modern_response_seen: bool,
    response_without_terminal: Option<usize>,
    latest_response_id: Option<String>,
    latest_usage: Option<ModelTokenUsage>,
    seen_tool_calls: HashSet<String>,
    seen_permission_requests: HashSet<String>,
    seen_steer_inputs: HashSet<String>,
    steer_awaiting_model: bool,
    active_tool_catalog_digest: Option<String>,
    active_mcp_runtime_digest: Option<String>,
    active_mcp_runtime_version: Option<u16>,
    active_hooks_digest: Option<String>,
    active_hooks_version: Option<u16>,
    active_exec_policy_digest: Option<String>,
    active_exec_policy_version: Option<u16>,
    active_system_instructions: Option<String>,
    active_system_instructions_version: Option<u16>,
    active_model_context_digest: Option<String>,
    active_model_context_version: Option<u16>,
    active_model_route: Option<crate::ModelRouteSnapshot>,
    /// Attribution for materialized history, retained across turn boundaries.
    /// This is independent of the frozen authority of an interrupted turn.
    history_model_route: Option<crate::ModelRouteSnapshot>,
}

/// Replay one ordered native log. The checkpoint store is consulted only when
/// the log contains a `CheckpointCommitted` marker. An unreferenced candidate
/// file is ignored. A referenced but missing/corrupt file is an error.
pub fn replay_native(
    session_id: &str,
    events: &[AgentEvent],
    checkpoint_store: Option<&CheckpointFileStore>,
    expected_compatibility: &CheckpointCompatibility,
) -> Result<NativeReplay, ReplayError> {
    if session_id.is_empty() {
        return Err(ReplayError::Conflict {
            sequence: 0,
            reason: "session ID is empty",
        });
    }
    let mut previous = 0u64;
    let mut selected_reference: Option<&CheckpointReference> = None;
    let mut committed_ids = HashSet::<String>::new();
    for event in events {
        if event.session_id != session_id {
            return Err(ReplayError::SessionMismatch(event.sequence));
        }
        if previous.checked_add(1) != Some(event.sequence) {
            return Err(ReplayError::InvalidSequence(event.sequence));
        }
        previous = event.sequence;
        if let AgentEventKind::CheckpointCommitted { reference } = &event.kind {
            if reference.session_id != session_id
                || reference.covered_event_sequence == 0
                || reference.covered_event_sequence >= event.sequence
                || !committed_ids.insert(reference.checkpoint_id.clone())
            {
                return Err(ReplayError::InvalidCheckpointMarker(event.sequence));
            }
            selected_reference = Some(reference);
        }
    }

    let checkpoint = if let Some(reference) = selected_reference {
        if &reference.compatibility != expected_compatibility {
            return Err(ReplayError::IncompatibleCheckpoint);
        }
        let store = checkpoint_store.ok_or(ReplayError::MissingCheckpointStore)?;
        store.load_after_commit(Some(reference))?
    } else {
        None
    };
    let covered = checkpoint
        .as_ref()
        .map(|checkpoint| checkpoint.covered_event_sequence)
        .unwrap_or(0);
    if let Some(checkpoint) = &checkpoint {
        for retained in &checkpoint.retained_user_messages {
            let event = &events[(retained.event_sequence - 1) as usize];
            let saved_prompt = match &event.kind {
                AgentEventKind::TurnAdmitted { prompt, .. }
                | AgentEventKind::TurnAdmittedWithImages { prompt, .. }
                | AgentEventKind::TurnStarted { prompt }
                | AgentEventKind::TurnSteered { text: prompt, .. }
                | AgentEventKind::TurnSteeredWithImages { text: prompt, .. } => prompt,
                _ => return Err(ReplayError::InvalidCheckpointMarker(event.sequence)),
            };
            if saved_prompt != &retained.content {
                return Err(ReplayError::InvalidCheckpointMarker(event.sequence));
            }
        }
    }

    let mut builder = ReplayBuilder {
        history: Vec::new(),
        committed_messages: Vec::new(),
        record_after_sequence: covered,
        active_turn: None,
        latest_plan: None,
        active_plan_at_turn_start: None,
        plan_proposed_this_turn: false,
        plan_updated_for_started_tool: false,
        active_prompt_event_sequence: None,
        turn_has_committed_response: false,
        model_flight: None,
        pause: None,
        superseded: None,
        pauses_in_turn: 0,
        pending_batch: None,
        pending_permission: None,
        started_tools: VecDeque::new(),
        media_for_started_tool: None,
        pending_hook: None,
        seen_hook_ids_for_call: HashSet::new(),
        read_wave_result_seen: false,
        duplicate_assistant_content: None,
        last_assistant_content: None,
        modern_response_seen: false,
        response_without_terminal: None,
        latest_response_id: None,
        latest_usage: None,
        seen_tool_calls: HashSet::new(),
        seen_permission_requests: HashSet::new(),
        seen_steer_inputs: HashSet::new(),
        steer_awaiting_model: false,
        active_tool_catalog_digest: None,
        active_mcp_runtime_digest: None,
        active_mcp_runtime_version: None,
        active_hooks_digest: None,
        active_hooks_version: None,
        active_exec_policy_digest: None,
        active_exec_policy_version: None,
        active_system_instructions: None,
        active_system_instructions_version: None,
        active_model_context_digest: None,
        active_model_context_version: None,
        active_model_route: None,
        history_model_route: None,
    };
    if let Some(checkpoint) = &checkpoint {
        for retained in &checkpoint.retained_user_messages {
            let original = &events[(retained.event_sequence - 1) as usize];
            let images = match &original.kind {
                AgentEventKind::TurnAdmittedWithImages { images, .. }
                | AgentEventKind::TurnSteeredWithImages { images, .. } => images.clone(),
                _ => Vec::new(),
            };
            let message = AgentMessage::user_with_images(retained.content.clone(), images)
                .map_err(|_| ReplayError::InvalidCheckpointMarker(original.sequence))?;
            builder.history.push(message);
        }
        // The summary follows the retained prompts, so their historical
        // instructions cannot become the newest turn in the replayed context.
        builder.history.push(AgentMessage::assistant(
            format!(
                "Committed earlier conversation summary (data): {}",
                checkpoint.summary
            ),
            vec![],
        ));
    }
    for event in events {
        builder.apply(event)?;
        if event.sequence == covered
            && (builder.active_turn.is_some()
                || builder.model_flight.is_some()
                || builder.pending_batch.is_some()
                || builder.pending_permission.is_some()
                || !builder.started_tools.is_empty())
        {
            return Err(ReplayError::InvalidCheckpointMarker(event.sequence));
        }
    }
    let recovery = builder.recovery();
    let can_start_new_turn = recovery == ReplayRecovery::Idle;
    Ok(NativeReplay {
        session_id: session_id.to_owned(),
        last_sequence: previous,
        history: builder.history,
        history_model_route: builder.history_model_route,
        committed_messages: builder.committed_messages,
        checkpoint,
        latest_plan: builder.latest_plan,
        active_plan_at_turn_start: builder.active_plan_at_turn_start,
        active_turn: builder.active_turn,
        recovery,
        pause: builder.pause,
        superseded: builder.superseded,
        pending_tool_batch: builder.pending_batch,
        latest_response_id: builder.latest_response_id,
        latest_usage: builder.latest_usage,
        active_tool_catalog_digest: builder.active_tool_catalog_digest,
        active_mcp_runtime_digest: builder.active_mcp_runtime_digest,
        active_mcp_runtime_version: builder.active_mcp_runtime_version,
        active_hooks_digest: builder.active_hooks_digest,
        active_hooks_version: builder.active_hooks_version,
        active_exec_policy_digest: builder.active_exec_policy_digest,
        active_exec_policy_version: builder.active_exec_policy_version,
        active_system_instructions: builder.active_system_instructions,
        active_system_instructions_version: builder.active_system_instructions_version,
        active_model_context_digest: builder.active_model_context_digest,
        active_model_context_version: builder.active_model_context_version,
        active_model_route: builder.active_model_route,
        can_start_new_turn,
    })
}

impl ReplayBuilder {
    fn push_message(&mut self, event_sequence: u64, message: AgentMessage) {
        if event_sequence > self.record_after_sequence {
            self.committed_messages.push(ReplayedMessage {
                event_sequence,
                message: message.clone(),
            });
            self.history.push(message);
        }
    }

    fn conflict(event: &AgentEvent, reason: &'static str) -> ReplayError {
        ReplayError::Conflict {
            sequence: event.sequence,
            reason,
        }
    }

    fn expected_tool_call(&self) -> Option<&ToolCall> {
        self.pending_batch
            .as_ref()
            .and_then(|batch| batch.assistant.tool_calls.get(batch.next_call_index))
    }

    fn apply(&mut self, event: &AgentEvent) -> Result<(), ReplayError> {
        match &event.kind {
            AgentEventKind::SessionStarted { .. }
            | AgentEventKind::CheckpointCommitted { .. }
            | AgentEventKind::AssistantDelta { .. }
            | AgentEventKind::HostNotice { .. }
            | AgentEventKind::ProcessOutput { .. } => {}
            AgentEventKind::TurnAdmitted {
                turn_id,
                prompt,
                model,
                mode,
            }
            | AgentEventKind::TurnAdmittedWithImages {
                turn_id,
                prompt,
                model,
                mode,
                ..
            } => {
                if self.active_turn.is_some()
                    || self.pending_batch.is_some()
                    || !self.started_tools.is_empty()
                    || self.pending_permission.is_some()
                {
                    return Err(Self::conflict(
                        event,
                        "new turn overlaps an unfinished turn",
                    ));
                }
                let images = match &event.kind {
                    AgentEventKind::TurnAdmittedWithImages { images, .. } => images.clone(),
                    _ => Vec::new(),
                };
                if turn_id.is_empty()
                    || prompt.trim().is_empty() && images.is_empty()
                    || model.trim().is_empty()
                {
                    return Err(Self::conflict(event, "invalid admitted turn"));
                }
                self.superseded = None;
                if validate_user_images(&images).is_err()
                    || images.is_empty()
                        && matches!(&event.kind, AgentEventKind::TurnAdmittedWithImages { .. })
                {
                    return Err(Self::conflict(event, "invalid admitted images"));
                }
                self.active_turn = Some(ReplayedTurn {
                    turn_id: Some(turn_id.clone()),
                    prompt: prompt.clone(),
                    model: Some(model.clone()),
                    mode: *mode,
                    started: false,
                    last_step: 0,
                    terminated: false,
                });
                self.active_plan_at_turn_start = self.latest_plan.clone();
                self.plan_proposed_this_turn = false;
                self.plan_updated_for_started_tool = false;
                self.active_prompt_event_sequence = Some(event.sequence);
                self.turn_has_committed_response = false;
                self.steer_awaiting_model = false;
                self.duplicate_assistant_content = None;
                self.last_assistant_content = None;
                self.modern_response_seen = false;
                self.response_without_terminal = None;
                self.active_tool_catalog_digest = None;
                self.active_mcp_runtime_digest = None;
                self.active_mcp_runtime_version = None;
                self.active_hooks_digest = None;
                self.active_hooks_version = None;
                self.active_exec_policy_digest = None;
                self.active_exec_policy_version = None;
                self.active_system_instructions = None;
                self.active_system_instructions_version = None;
                self.active_model_context_digest = None;
                self.active_model_context_version = None;
                self.active_model_route = None;
                self.push_message(
                    event.sequence,
                    AgentMessage::user_with_images(prompt.clone(), images)
                        .map_err(|_| Self::conflict(event, "invalid admitted images"))?,
                );
            }
            AgentEventKind::TurnSteered {
                turn_id,
                input_id,
                text,
            }
            | AgentEventKind::TurnSteeredWithImages {
                turn_id,
                input_id,
                text,
                ..
            } => {
                let images = match &event.kind {
                    AgentEventKind::TurnSteeredWithImages { images, .. } => images.clone(),
                    _ => Vec::new(),
                };
                let active = self
                    .active_turn
                    .as_ref()
                    .ok_or_else(|| Self::conflict(event, "steering input has no active turn"))?;
                let valid_id = !input_id.is_empty()
                    && input_id.len() <= 128
                    && input_id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
                if active.turn_id.as_deref() != Some(turn_id)
                    || !active.started
                    || active.terminated
                    || !valid_id
                    || text.trim().is_empty() && images.is_empty()
                    || text.len() > 32 * 1024
                    || validate_user_images(&images).is_err()
                    || images.is_empty()
                        && matches!(&event.kind, AgentEventKind::TurnSteeredWithImages { .. })
                    || self.model_flight.is_some()
                    || self.pending_batch.is_some()
                    || self.pending_permission.is_some()
                    || !self.started_tools.is_empty()
                    || self.pending_hook.is_some()
                    || self.duplicate_assistant_content.is_some()
                    || !self.seen_steer_inputs.insert(input_id.clone())
                {
                    return Err(Self::conflict(
                        event,
                        "steering input is invalid or out of place",
                    ));
                }
                self.push_message(
                    event.sequence,
                    AgentMessage::user_with_images(text.clone(), images)
                        .map_err(|_| Self::conflict(event, "invalid steering images"))?,
                );
                self.steer_awaiting_model = true;
                self.response_without_terminal = None;
                // A Plan-mode answer can be revised after an intervening
                // user steer. The new proposal still requires a newer model
                // response that exactly matches its snapshot.
                self.plan_proposed_this_turn = false;
            }
            AgentEventKind::TurnStarted { prompt } => {
                self.pause = None;
                self.pauses_in_turn = 0;
                if let Some(turn) = self.active_turn.as_mut() {
                    if turn.started || turn.prompt != *prompt || turn.terminated {
                        return Err(Self::conflict(event, "turn start conflicts with admission"));
                    }
                    turn.started = true;
                } else {
                    if prompt.trim().is_empty() {
                        return Err(Self::conflict(event, "legacy turn prompt is empty"));
                    }
                    self.active_turn = Some(ReplayedTurn {
                        turn_id: None,
                        prompt: prompt.clone(),
                        model: None,
                        mode: TurnMode::Build,
                        started: true,
                        last_step: 0,
                        terminated: false,
                    });
                    self.active_plan_at_turn_start = self.latest_plan.clone();
                    self.plan_proposed_this_turn = false;
                    self.plan_updated_for_started_tool = false;
                    self.active_prompt_event_sequence = Some(event.sequence);
                    self.turn_has_committed_response = false;
                    self.active_tool_catalog_digest = None;
                    self.active_mcp_runtime_digest = None;
                    self.active_mcp_runtime_version = None;
                    self.active_hooks_digest = None;
                    self.active_hooks_version = None;
                    self.active_exec_policy_digest = None;
                    self.active_exec_policy_version = None;
                    self.active_system_instructions = None;
                    self.active_system_instructions_version = None;
                    self.active_model_context_digest = None;
                    self.active_model_context_version = None;
                    self.active_model_route = None;
                    self.push_message(event.sequence, AgentMessage::user(prompt.clone()));
                }
            }
            AgentEventKind::ToolCatalogCommitted { digest } => {
                let valid_digest = digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
                if !valid_digest
                    || self.active_tool_catalog_digest.is_some()
                    || self
                        .active_turn
                        .as_ref()
                        .is_none_or(|turn| turn.terminated || turn.last_step != 0)
                    || self.model_flight.is_some()
                    || self.pending_batch.is_some()
                    || !self.started_tools.is_empty()
                {
                    return Err(Self::conflict(event, "tool catalog commit is out of place"));
                }
                self.active_tool_catalog_digest = Some(digest.clone());
            }
            AgentEventKind::McpRuntimeCommitted { version, digest } => {
                let valid_digest = digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
                if *version != MCP_RUNTIME_IDENTITY_VERSION
                    || !valid_digest
                    || self.active_mcp_runtime_digest.is_some()
                    || self.active_tool_catalog_digest.is_none()
                    || self.active_hooks_digest.is_some()
                    || self.active_exec_policy_digest.is_some()
                    || self.active_model_context_digest.is_some()
                    || self.active_turn.as_ref().is_none_or(|turn| {
                        !turn.started
                            || turn.terminated
                            || turn.mode != TurnMode::Build
                            || turn.last_step != 0
                    })
                    || self.model_flight.is_some()
                    || self.pending_batch.is_some()
                    || self.pending_permission.is_some()
                    || !self.started_tools.is_empty()
                {
                    return Err(Self::conflict(event, "MCP runtime commit is out of place"));
                }
                self.active_mcp_runtime_digest = Some(digest.clone());
                self.active_mcp_runtime_version = Some(*version);
            }
            AgentEventKind::HooksCommitted { version, digest } => {
                let valid_digest = digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
                if *version != HOOK_CONFIG_IDENTITY_VERSION
                    || !valid_digest
                    || self.active_hooks_digest.is_some()
                    || self.active_tool_catalog_digest.is_none()
                    || self.active_exec_policy_digest.is_some()
                    || self.active_model_context_digest.is_some()
                    || self
                        .active_turn
                        .as_ref()
                        .is_none_or(|turn| !turn.started || turn.terminated || turn.last_step != 0)
                    || self.model_flight.is_some()
                    || self.pending_batch.is_some()
                    || self.pending_permission.is_some()
                    || !self.started_tools.is_empty()
                {
                    return Err(Self::conflict(event, "hook policy commit is out of place"));
                }
                self.active_hooks_digest = Some(digest.clone());
                self.active_hooks_version = Some(*version);
            }
            AgentEventKind::ExecPolicyCommitted { version, digest } => {
                let valid_digest = digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
                if *version != EXEC_POLICY_IDENTITY_VERSION
                    || !valid_digest
                    || self.active_exec_policy_digest.is_some()
                    || self.active_hooks_digest.is_none()
                    || self.active_tool_catalog_digest.is_none()
                    || self.active_model_context_digest.is_some()
                    || self
                        .active_turn
                        .as_ref()
                        .is_none_or(|turn| !turn.started || turn.terminated || turn.last_step != 0)
                    || self.model_flight.is_some()
                    || self.pending_batch.is_some()
                    || self.pending_permission.is_some()
                    || !self.started_tools.is_empty()
                {
                    return Err(Self::conflict(event, "exec policy commit is out of place"));
                }
                self.active_exec_policy_digest = Some(digest.clone());
                self.active_exec_policy_version = Some(*version);
            }
            AgentEventKind::SystemInstructionsCommitted {
                version,
                digest,
                content,
            } => {
                if *version != SYSTEM_INSTRUCTIONS_VERSION
                    || system_instructions_digest(content).as_deref() != Some(digest.as_str())
                    || self.active_system_instructions.is_some()
                    || self.active_exec_policy_digest.is_none()
                    || self.active_tool_catalog_digest.is_none()
                    || self.active_model_context_digest.is_some()
                    || self
                        .active_turn
                        .as_ref()
                        .is_none_or(|turn| !turn.started || turn.terminated || turn.last_step != 0)
                    || self.model_flight.is_some()
                    || self.pending_batch.is_some()
                    || self.pending_permission.is_some()
                    || !self.started_tools.is_empty()
                {
                    return Err(Self::conflict(
                        event,
                        "system instruction snapshot is invalid or out of place",
                    ));
                }
                self.active_system_instructions = Some(content.clone());
                self.active_system_instructions_version = Some(*version);
            }
            AgentEventKind::ModelContextCommitted { version, digest } => {
                let valid_digest = digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
                if !(1..=MODEL_CONTEXT_VERSION).contains(version)
                    || !valid_digest
                    || self.active_model_context_digest.is_some()
                    || self.active_tool_catalog_digest.is_none()
                    || self
                        .active_turn
                        .as_ref()
                        .is_none_or(|turn| !turn.started || turn.terminated || turn.last_step != 0)
                    || self.model_flight.is_some()
                    || self.pending_batch.is_some()
                    || !self.started_tools.is_empty()
                    || self.pending_permission.is_some()
                {
                    return Err(Self::conflict(
                        event,
                        "model context commit is out of place",
                    ));
                }
                self.active_model_context_digest = Some(digest.clone());
                self.active_model_context_version = Some(*version);
            }
            AgentEventKind::ModelRouteCommitted { .. } => {
                let route =
                    crate::ModelRouteSnapshot::from_event(&event.kind).expect("route variant");
                if !route.is_valid()
                    || self.active_model_route.is_some()
                    || self.active_model_context_digest.is_none()
                    || self.active_turn.as_ref().is_none_or(|turn| {
                        !turn.started
                            || turn.terminated
                            || turn.last_step != 0
                            || turn
                                .model
                                .as_deref()
                                .is_some_and(|model| model != route.model_id)
                    })
                    || self.model_flight.is_some()
                    || self.pending_batch.is_some()
                    || self.pending_permission.is_some()
                    || !self.started_tools.is_empty()
                {
                    return Err(Self::conflict(
                        event,
                        "model route snapshot is invalid or out of place",
                    ));
                }
                if self.history_model_route.as_ref() != Some(&route) {
                    crate::reconcile_model_history_route(
                        &mut self.history,
                        self.history_model_route.as_ref(),
                        &route,
                    );
                    // Auto-compaction consumes this second projection. Keep it
                    // in the same route as requests and interrupted replay.
                    for message in &mut self.committed_messages {
                        message.message.provider_output_items.clear();
                    }
                }
                self.history_model_route = Some(route.clone());
                self.active_model_route = Some(route);
            }
            AgentEventKind::ModelRequestStarted { step, model } => {
                let turn = self
                    .active_turn
                    .as_mut()
                    .ok_or_else(|| Self::conflict(event, "model request has no active turn"))?;
                if !turn.started
                    || turn.terminated
                    || *step == 0
                    || *step != turn.last_step.saturating_add(1)
                    || turn.model.as_ref().is_some_and(|saved| saved != model)
                    || self
                        .active_model_route
                        .as_ref()
                        .is_some_and(|route| &route.model_id != model)
                    || self.model_flight.is_some()
                    || self.pending_batch.is_some()
                    || !self.started_tools.is_empty()
                    || self.pending_permission.is_some()
                {
                    return Err(Self::conflict(
                        event,
                        "model request conflicts with turn state",
                    ));
                }
                turn.last_step = *step;
                if self.active_model_route.is_none() {
                    // A legacy turn may use another provider/endpoint even if
                    // its model label matches. Its outputs cannot inherit a
                    // previous modern turn's exact route attribution.
                    self.history_model_route = None;
                }
                self.model_flight = Some(ModelFlight {
                    step: *step,
                    model: model.clone(),
                });
                self.steer_awaiting_model = false;
                self.response_without_terminal = None;
                self.duplicate_assistant_content = None;
                self.last_assistant_content = None;
                self.modern_response_seen = false;
            }
            AgentEventKind::ModelStreamUpdate { step, .. } => {
                if self
                    .model_flight
                    .as_ref()
                    .is_none_or(|flight| flight.step != *step)
                    || self.active_turn.is_none()
                    || self.pending_batch.is_some()
                {
                    return Err(Self::conflict(
                        event,
                        "stream update has no matching model request",
                    ));
                }
                // Presentation metadata is deliberately excluded from model
                // history and tool authority. Only a full response commits it.
            }
            AgentEventKind::ModelResponseCommitted {
                step,
                assistant,
                response_id,
                usage,
            } => {
                self.pause = None;
                let flight = self.model_flight.take().ok_or_else(|| {
                    Self::conflict(event, "committed response has no model request")
                })?;
                if flight.step != *step
                    || assistant.role != MessageRole::Assistant
                    || !assistant.images.is_empty()
                    || assistant.tool_call_id.is_some()
                    || self.pending_batch.is_some()
                {
                    return Err(Self::conflict(
                        event,
                        "committed model response is inconsistent",
                    ));
                }
                for call in &assistant.tool_calls {
                    if call.id.is_empty() || !self.seen_tool_calls.insert(call.id.clone()) {
                        return Err(Self::conflict(event, "tool call ID is empty or repeated"));
                    }
                }
                self.latest_response_id = response_id.clone();
                self.latest_usage = usage.clone();
                self.turn_has_committed_response = true;
                self.modern_response_seen = true;
                self.duplicate_assistant_content = Some(assistant.content.clone());
                if assistant.tool_calls.is_empty() {
                    self.push_message(event.sequence, assistant.clone());
                    self.last_assistant_content = Some(assistant.content.clone());
                    self.response_without_terminal = Some(*step);
                } else {
                    self.pending_batch = Some(ReplayedToolBatch {
                        assistant: assistant.clone(),
                        assistant_event_sequence: event.sequence,
                        completed_results: Vec::new(),
                        completed_result_sequences: Vec::new(),
                        next_call_index: 0,
                    });
                }
            }
            AgentEventKind::AssistantMessage { content } => {
                if let Some(committed) = self.duplicate_assistant_content.take() {
                    if &committed != content {
                        return Err(Self::conflict(
                            event,
                            "assistant display differs from committed response",
                        ));
                    }
                } else {
                    if self.active_turn.is_none()
                        || self.pending_batch.is_some()
                        || self.modern_response_seen
                        || self
                            .active_turn
                            .as_ref()
                            .is_some_and(|turn| turn.last_step == 0)
                    {
                        return Err(Self::conflict(
                            event,
                            "legacy assistant message is out of place",
                        ));
                    }
                    self.model_flight = None;
                    self.turn_has_committed_response = true;
                    self.push_message(
                        event.sequence,
                        AgentMessage::assistant(content.clone(), vec![]),
                    );
                    self.last_assistant_content = Some(content.clone());
                    self.response_without_terminal =
                        self.active_turn.as_ref().map(|turn| turn.last_step);
                }
            }
            AgentEventKind::PlanProposed { plan } => {
                if !plan.is_valid()
                    || self.plan_proposed_this_turn
                    || self.active_turn.as_ref().is_none_or(|turn| {
                        !turn.started || turn.terminated || turn.mode != TurnMode::Plan
                    })
                    || self.pending_batch.is_some()
                    || self.model_flight.is_some()
                    || self.response_without_terminal.is_none()
                    || self
                        .last_assistant_content
                        .as_ref()
                        .is_none_or(|response| PlanSnapshot::from_plan_response(response) != *plan)
                {
                    return Err(Self::conflict(
                        event,
                        "plan proposal is invalid or out of place",
                    ));
                }
                self.latest_plan = Some(plan.clone());
                self.plan_proposed_this_turn = true;
            }
            AgentEventKind::PlanUpdated { tool_call_id, plan } => {
                let started = self.started_tools.front();
                if !plan.is_valid()
                    || self.plan_updated_for_started_tool
                    || self.started_tools.len() != 1
                    || self.pending_hook.is_some()
                    || self.active_turn.as_ref().is_none_or(|turn| {
                        !turn.started || turn.terminated || turn.mode != TurnMode::Build
                    })
                    || started.is_none_or(|call| {
                        call.id != *tool_call_id
                            || call.name != "update_plan"
                            || serde_json::from_value::<PlanSnapshot>(call.arguments.clone())
                                .ok()
                                .as_ref()
                                != Some(plan)
                    })
                {
                    return Err(Self::conflict(
                        event,
                        "plan update is invalid or out of place",
                    ));
                }
                self.latest_plan = Some(plan.clone());
                self.plan_updated_for_started_tool = true;
            }
            AgentEventKind::PermissionRequested {
                request_id,
                tool_call,
            } => {
                if self.active_turn.is_none()
                    || !self.started_tools.is_empty()
                    || self.pending_permission.is_some()
                    || request_id.is_empty()
                    || !self.seen_permission_requests.insert(request_id.clone())
                {
                    return Err(Self::conflict(event, "permission request is out of place"));
                }
                self.ensure_tool_batch(event, tool_call)?;
                if self.expected_tool_call() != Some(tool_call) {
                    return Err(Self::conflict(
                        event,
                        "permission targets a different tool call",
                    ));
                }
                self.pending_permission = Some(PendingPermission {
                    request_id: request_id.clone(),
                    tool_call: tool_call.clone(),
                });
            }
            AgentEventKind::PermissionResolved {
                request_id,
                tool_call_id,
                resolution: _,
            } => {
                let pending = self
                    .pending_permission
                    .take()
                    .ok_or_else(|| Self::conflict(event, "permission resolution has no request"))?;
                if pending.request_id != *request_id || pending.tool_call.id != *tool_call_id {
                    return Err(Self::conflict(
                        event,
                        "permission resolution targets another call",
                    ));
                }
            }
            AgentEventKind::ToolStarted { tool_call } => {
                if self.active_turn.is_none()
                    || self.pending_permission.is_some()
                    || self.pending_hook.is_some()
                {
                    return Err(Self::conflict(event, "tool start is out of place"));
                }
                self.ensure_tool_batch(event, tool_call)?;
                let queued = self.started_tools.len();
                let expected = self.pending_batch.as_ref().and_then(|batch| {
                    batch
                        .assistant
                        .tool_calls
                        .get(batch.next_call_index.saturating_add(queued))
                });
                if expected != Some(tool_call) {
                    return Err(Self::conflict(
                        event,
                        "tool start differs from committed call",
                    ));
                }
                if queued > 0
                    && (self.read_wave_result_seen
                        || queued >= MAX_CONCURRENT_READ_STARTS
                        || !self.modern_response_seen
                        || !trusted_read_name(&tool_call.name)
                        || self
                            .started_tools
                            .iter()
                            .any(|started| !trusted_read_name(&started.name)))
                {
                    return Err(Self::conflict(
                        event,
                        "concurrent tool starts require one bounded trusted read wave",
                    ));
                }
                if queued == 0 {
                    self.seen_hook_ids_for_call.clear();
                    self.plan_updated_for_started_tool = false;
                    self.media_for_started_tool = None;
                }
                self.started_tools.push_back(tool_call.clone());
            }
            AgentEventKind::HookStarted {
                tool_call_id,
                hook_id,
                phase,
            } => {
                if self.active_hooks_digest.is_none()
                    || self.started_tools.len() != 1
                    || self.started_tools.front().map(|call| call.id.as_str())
                        != Some(tool_call_id.as_str())
                    || self.pending_hook.is_some()
                    || hook_id.is_empty()
                    || hook_id.len() > 128
                    || !matches!(phase.as_str(), "pre_tool_use" | "post_tool_use")
                    || self.seen_hook_ids_for_call.contains(hook_id)
                {
                    return Err(Self::conflict(event, "hook start is out of place"));
                }
                self.seen_hook_ids_for_call.insert(hook_id.clone());
                self.pending_hook = Some(PendingHook {
                    tool_call_id: tool_call_id.clone(),
                    hook_id: hook_id.clone(),
                    phase: phase.clone(),
                    diagnostic_recorded: false,
                });
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
                let valid = self.pending_hook.as_ref().is_some_and(|pending| {
                    pending.tool_call_id == *tool_call_id
                        && pending.hook_id == *hook_id
                        && pending.phase == *phase
                        && !pending.diagnostic_recorded
                });
                if !valid
                    || reason.as_ref().is_some_and(|value| value.len() > 8192)
                    || error.as_ref().is_some_and(|value| value.len() > 8192)
                    || stdout
                        .as_ref()
                        .map_or(0, String::len)
                        .saturating_add(stderr.as_ref().map_or(0, String::len))
                        > 256 * 1024
                {
                    return Err(Self::conflict(
                        event,
                        "hook diagnostics are invalid or out of place",
                    ));
                }
                self.pending_hook
                    .as_mut()
                    .expect("validated pending hook")
                    .diagnostic_recorded = true;
            }
            AgentEventKind::HookFinished {
                tool_call_id,
                hook_id,
                phase,
                decision,
            } => {
                let pending = self
                    .pending_hook
                    .take()
                    .ok_or_else(|| Self::conflict(event, "hook finish has no start"))?;
                if pending.tool_call_id != *tool_call_id
                    || pending.hook_id != *hook_id
                    || pending.phase != *phase
                    || !matches!(decision.as_str(), "allow" | "block")
                    || (phase == "post_tool_use" && decision == "block")
                {
                    return Err(Self::conflict(event, "hook finish conflicts with start"));
                }
            }
            AgentEventKind::ToolOutputDelta { tool_call_id, .. } => {
                if self.active_turn.as_ref().is_none_or(|turn| turn.terminated)
                    || self.pending_hook.is_some()
                    || !self
                        .started_tools
                        .iter()
                        .any(|call| call.id == *tool_call_id)
                {
                    return Err(Self::conflict(
                        event,
                        "tool output has no matching started call",
                    ));
                }
                // Partial output never closes the call or enters model history.
            }
            AgentEventKind::ToolResultMedia {
                tool_call_id,
                images,
                text,
                notices,
                model_images,
            } => {
                let for_model = crate::model_tool_images(images, model_images);
                if self.active_turn.as_ref().is_none_or(|turn| turn.terminated)
                    || self.pending_hook.is_some()
                    || self.media_for_started_tool.is_some()
                    || !self
                        .started_tools
                        .iter()
                        .any(|call| call.id == *tool_call_id && call.name.starts_with("mcp__"))
                    || crate::validate_tool_result_media(images, text, notices).is_err()
                    || for_model.is_none()
                {
                    return Err(Self::conflict(
                        event,
                        "tool media is invalid or has no matching started MCP call",
                    ));
                }
                // Media completes nothing and grants no authority; the images
                // it marks for the model join the result ToolFinished commits.
                self.media_for_started_tool = for_model;
            }
            AgentEventKind::ToolFinished {
                tool_call_id,
                output,
                is_error,
            } => {
                // A manually verified result may resolve a crashed hook and
                // its enclosing tool call without inventing a hook result.
                self.pending_hook = None;
                let started = self
                    .started_tools
                    .pop_front()
                    .ok_or_else(|| Self::conflict(event, "tool result has no started call"))?;
                if started.id != *tool_call_id {
                    return Err(Self::conflict(event, "tool result ID does not match start"));
                }
                if self.plan_updated_for_started_tool && *is_error {
                    return Err(Self::conflict(
                        event,
                        "committed plan update has an error result",
                    ));
                }
                self.plan_updated_for_started_tool = false;
                self.read_wave_result_seen = !self.started_tools.is_empty();
                let batch = self
                    .pending_batch
                    .as_mut()
                    .ok_or_else(|| Self::conflict(event, "tool result has no assistant call"))?;
                let mut result = AgentMessage::tool(tool_call_id.clone(), output.clone());
                result.images = self.media_for_started_tool.take().unwrap_or_default();
                batch.completed_results.push(result);
                batch.completed_result_sequences.push(event.sequence);
                batch.next_call_index += 1;
                if batch.next_call_index == batch.assistant.tool_calls.len() {
                    if !self.started_tools.is_empty() {
                        return Err(Self::conflict(event, "tool batch ended with open starts"));
                    }
                    let complete = self.pending_batch.take().expect("batch exists");
                    self.push_message(complete.assistant_event_sequence, complete.assistant);
                    for (sequence, result) in complete
                        .completed_result_sequences
                        .into_iter()
                        .zip(complete.completed_results)
                    {
                        self.push_message(sequence, result);
                    }
                    // An empty assistant display is intentionally omitted
                    // by the runner for tool-only responses. It must not
                    // block a steer after the final tool result.
                    if self.duplicate_assistant_content.as_deref() == Some("") {
                        self.duplicate_assistant_content = None;
                    }
                    // A verified result can arrive after TurnFailed or
                    // TurnCancelled. Once it closes the batch, the terminated
                    // turn has no remaining work to resume.
                    if self
                        .active_turn
                        .as_ref()
                        .is_some_and(|turn| turn.terminated)
                    {
                        self.close_terminated_turn();
                    }
                }
                self.response_without_terminal = None;
            }
            AgentEventKind::ToolBatchDiscarded { next_tool_call_id } => {
                if self
                    .active_turn
                    .as_ref()
                    .is_none_or(|turn| !turn.terminated)
                    || self.pending_batch.is_none()
                    || !self.started_tools.is_empty()
                    || self.model_flight.is_some()
                    || self
                        .expected_tool_call()
                        .is_none_or(|call| call.id != *next_tool_call_id)
                    || self
                        .pending_permission
                        .as_ref()
                        .is_some_and(|permission| permission.tool_call.id != *next_tool_call_id)
                {
                    return Err(Self::conflict(event, "tool batch discard is out of place"));
                }
                if let Some(prompt_sequence) = self.active_prompt_event_sequence {
                    let removed = self
                        .committed_messages
                        .iter()
                        .filter(|message| message.event_sequence >= prompt_sequence)
                        .count();
                    self.committed_messages
                        .retain(|message| message.event_sequence < prompt_sequence);
                    let keep = self.history.len().checked_sub(removed).ok_or_else(|| {
                        Self::conflict(event, "discarded turn history is inconsistent")
                    })?;
                    self.history.truncate(keep);
                }
                self.pending_permission = None;
                self.pending_batch = None;
                self.active_turn = None;
                self.active_plan_at_turn_start = None;
                self.active_prompt_event_sequence = None;
                self.turn_has_committed_response = false;
                self.response_without_terminal = None;
                self.duplicate_assistant_content = None;
                self.last_assistant_content = None;
                self.modern_response_seen = false;
                self.active_tool_catalog_digest = None;
                self.active_mcp_runtime_digest = None;
                self.active_mcp_runtime_version = None;
                self.active_hooks_digest = None;
                self.active_hooks_version = None;
                self.active_exec_policy_digest = None;
                self.active_exec_policy_version = None;
                self.active_system_instructions = None;
                self.active_system_instructions_version = None;
                self.active_model_context_digest = None;
                self.active_model_context_version = None;
                self.active_model_route = None;
            }
            AgentEventKind::TurnCompleted { response, steps } => {
                self.pause = None;
                let turn = self
                    .active_turn
                    .as_ref()
                    .ok_or_else(|| Self::conflict(event, "completion has no active turn"))?;
                if !turn.started
                    || turn.terminated
                    || turn.last_step != *steps
                    || self.model_flight.is_some()
                    || self.pending_batch.is_some()
                    || self.pending_permission.is_some()
                    || !self.started_tools.is_empty()
                    || self.last_assistant_content.as_deref() != Some(response.as_str())
                    || self.steer_awaiting_model
                {
                    return Err(Self::conflict(event, "turn completion is inconsistent"));
                }
                self.active_turn = None;
                self.active_plan_at_turn_start = None;
                self.active_prompt_event_sequence = None;
                self.turn_has_committed_response = false;
                self.response_without_terminal = None;
                self.duplicate_assistant_content = None;
                self.active_tool_catalog_digest = None;
                self.active_mcp_runtime_digest = None;
                self.active_mcp_runtime_version = None;
                self.active_hooks_digest = None;
                self.active_hooks_version = None;
                self.active_exec_policy_digest = None;
                self.active_exec_policy_version = None;
                self.active_system_instructions = None;
                self.active_system_instructions_version = None;
                self.active_model_context_digest = None;
                self.active_model_context_version = None;
                self.active_model_route = None;
            }
            AgentEventKind::TurnPaused {
                class,
                limit_key,
                resets_at_ms,
                step,
                detail,
            } => {
                let turn = self
                    .active_turn
                    .as_ref()
                    .ok_or_else(|| Self::conflict(event, "pause has no active turn"))?;
                // Only a model boundary can pause: the request of `step` is in
                // flight, or it is the next one and no tool is open.
                let at_boundary = match &self.model_flight {
                    Some(flight) => flight.step == *step,
                    None => *step == turn.last_step.saturating_add(1),
                };
                if !turn.started
                    || turn.terminated
                    || !at_boundary
                    || self.pending_batch.is_some()
                    || !self.started_tools.is_empty()
                    || self.pending_permission.is_some()
                {
                    return Err(Self::conflict(event, "pause is not at a model boundary"));
                }
                self.pauses_in_turn = self.pauses_in_turn.saturating_add(1);
                self.pause = Some(ReplayPause {
                    class: *class,
                    limit_key: limit_key.clone(),
                    resets_at_ms: *resets_at_ms,
                    step: *step,
                    detail: detail.clone(),
                    event_sequence: event.sequence,
                    paused_at_ms: event.timestamp_ms,
                    pauses_in_turn: self.pauses_in_turn,
                });
            }
            AgentEventKind::TurnFailed { .. }
            | AgentEventKind::TurnCancelled
            | AgentEventKind::TurnSuperseded { .. } => {
                if let AgentEventKind::TurnSuperseded {
                    provider_id,
                    model_id,
                    next_turn_id,
                    host_prompt,
                } = &event.kind
                {
                    // Only a turn paused at its model boundary can move: no
                    // tool is open, so closing it loses and repeats nothing.
                    if self.pause.is_none()
                        || self.pending_batch.is_some()
                        || self.pending_permission.is_some()
                        || !self.started_tools.is_empty()
                        || next_turn_id.is_empty()
                        || provider_id.is_empty()
                        || model_id.is_empty()
                    {
                        return Err(Self::conflict(event, "supersede needs a paused turn"));
                    }
                    self.superseded = Some(ReplaySuperseded {
                        provider_id: provider_id.clone(),
                        model_id: model_id.clone(),
                        next_turn_id: next_turn_id.clone(),
                        host_prompt: *host_prompt,
                        prompt_event_sequence: self.active_prompt_event_sequence,
                        event_sequence: event.sequence,
                    });
                }
                self.pause = None;
                let turn = self
                    .active_turn
                    .as_mut()
                    .ok_or_else(|| Self::conflict(event, "terminal event has no active turn"))?;
                if turn.terminated {
                    return Err(Self::conflict(event, "turn has two terminal events"));
                }
                turn.terminated = true;
                if !self.turn_has_committed_response {
                    if let Some(sequence) = self.active_prompt_event_sequence {
                        if self
                            .committed_messages
                            .last()
                            .is_some_and(|message| message.event_sequence == sequence)
                        {
                            self.committed_messages.pop();
                            self.history.pop();
                        }
                    }
                }
                if self.pending_batch.is_none()
                    && self.pending_permission.is_none()
                    && self.started_tools.is_empty()
                {
                    self.close_terminated_turn();
                }
            }
        }
        Ok(())
    }

    fn close_terminated_turn(&mut self) {
        self.active_turn = None;
        self.active_plan_at_turn_start = None;
        self.active_prompt_event_sequence = None;
        self.turn_has_committed_response = false;
        self.steer_awaiting_model = false;
        self.model_flight = None;
        self.response_without_terminal = None;
        self.active_tool_catalog_digest = None;
        self.active_mcp_runtime_digest = None;
        self.active_mcp_runtime_version = None;
        self.active_hooks_digest = None;
        self.active_hooks_version = None;
        self.active_exec_policy_digest = None;
        self.active_exec_policy_version = None;
        self.active_system_instructions = None;
        self.active_system_instructions_version = None;
        self.active_model_context_digest = None;
        self.active_model_context_version = None;
        self.active_model_route = None;
    }

    fn ensure_tool_batch(
        &mut self,
        event: &AgentEvent,
        call: &ToolCall,
    ) -> Result<(), ReplayError> {
        if self.pending_batch.is_none() {
            // Legacy logs have no ModelResponseCommitted. A recorded permission
            // request or tool start is the first durable evidence of a call.
            if self.modern_response_seen
                || (self.model_flight.is_none()
                    && self
                        .active_turn
                        .as_ref()
                        .is_none_or(|turn| turn.last_step == 0))
            {
                return Err(Self::conflict(event, "tool call has no model response"));
            }
            if call.id.is_empty() || !self.seen_tool_calls.insert(call.id.clone()) {
                return Err(Self::conflict(event, "tool call ID is empty or repeated"));
            }
            self.model_flight = None;
            self.turn_has_committed_response = true;
            self.response_without_terminal = None;
            self.pending_batch = Some(ReplayedToolBatch {
                assistant: AgentMessage::assistant("", vec![call.clone()]),
                assistant_event_sequence: event.sequence,
                completed_results: Vec::new(),
                completed_result_sequences: Vec::new(),
                next_call_index: 0,
            });
        }
        Ok(())
    }

    fn recovery(&self) -> ReplayRecovery {
        if let Some(call) = self.started_tools.front() {
            return ReplayRecovery::ToolOutcomeUncertain {
                tool_call: call.clone(),
            };
        }
        if let Some(permission) = &self.pending_permission {
            if self
                .active_turn
                .as_ref()
                .is_some_and(|turn| turn.terminated)
            {
                return ReplayRecovery::AbortedToolBatch {
                    next_tool_call: permission.tool_call.clone(),
                };
            }
            return ReplayRecovery::PermissionPending {
                request_id: permission.request_id.clone(),
                tool_call: permission.tool_call.clone(),
            };
        }
        if let Some(call) = self.expected_tool_call() {
            if self
                .active_turn
                .as_ref()
                .is_some_and(|turn| turn.terminated)
            {
                return ReplayRecovery::AbortedToolBatch {
                    next_tool_call: call.clone(),
                };
            }
            return ReplayRecovery::ToolReady {
                tool_call: call.clone(),
            };
        }
        if let Some(flight) = &self.model_flight {
            return ReplayRecovery::ModelInFlight {
                step: flight.step,
                model: flight.model.clone(),
            };
        }
        if let Some(turn) = &self.active_turn {
            if !turn.started {
                return ReplayRecovery::AdmittedNotStarted;
            }
            if let Some(step) = self.response_without_terminal {
                return ReplayRecovery::ResponseCommitted { step };
            }
            return ReplayRecovery::ReadyForModel {
                next_step: turn.last_step.saturating_add(1),
            };
        }
        ReplayRecovery::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CheckpointBudgetSnapshot, PlanStep, PlanStepStatus, RetainedUserMessage};
    use serde_json::json;

    fn compatibility() -> CheckpointCompatibility {
        CheckpointCompatibility {
            provider: "openai".into(),
            model: "gpt-example".into(),
            context_format_key: "responses-v1".into(),
        }
    }

    fn event(sequence: u64, kind: AgentEventKind) -> AgentEvent {
        AgentEvent::new("session-1", sequence, kind)
    }

    fn admitted() -> AgentEventKind {
        AgentEventKind::TurnAdmitted {
            turn_id: "turn-1".into(),
            prompt: "Build this".into(),
            model: "gpt-example".into(),
            mode: TurnMode::Build,
        }
    }

    #[test]
    fn interrupted_hook_is_an_uncertain_tool_and_malformed_hook_log_fails_closed() {
        let call = ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: json!({"path":"README.md"}),
        };
        let mut events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ToolCatalogCommitted {
                    digest: "a".repeat(64),
                },
            ),
            event(
                4,
                AgentEventKind::HooksCommitted {
                    version: HOOK_CONFIG_IDENTITY_VERSION,
                    digest: "b".repeat(64),
                },
            ),
            event(
                5,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(
                6,
                AgentEventKind::ModelResponseCommitted {
                    step: 1,
                    assistant: AgentMessage::assistant("", vec![call.clone()]),
                    response_id: None,
                    usage: None,
                },
            ),
            event(
                7,
                AgentEventKind::ToolStarted {
                    tool_call: call.clone(),
                },
            ),
            event(
                8,
                AgentEventKind::HookStarted {
                    tool_call_id: call.id.clone(),
                    hook_id: "guard".into(),
                    phase: "pre_tool_use".into(),
                },
            ),
        ];
        let interrupted = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(
            interrupted.active_hooks_digest.as_deref(),
            Some("b".repeat(64).as_str())
        );
        assert!(
            matches!(interrupted.recovery, ReplayRecovery::ToolOutcomeUncertain { tool_call } if tool_call.id == "call-1")
        );
        assert!(!interrupted.can_start_new_turn);
        let diagnostic = AgentEventKind::HookDiagnosticCommitted {
            tool_call_id: call.id.clone(),
            hook_id: "guard".into(),
            phase: "pre_tool_use".into(),
            reason: Some("approved".into()),
            stdout: Some("{}".into()),
            stderr: Some("note".into()),
            error: None,
        };
        let mut diagnosed = events.clone();
        diagnosed.push(event(9, diagnostic.clone()));
        let replay = replay_native("session-1", &diagnosed, None, &compatibility()).unwrap();
        assert!(
            matches!(replay.recovery, ReplayRecovery::ToolOutcomeUncertain { .. }),
            "diagnostics do not confirm a hook outcome"
        );
        diagnosed.push(event(10, diagnostic.clone()));
        assert!(
            replay_native("session-1", &diagnosed, None, &compatibility()).is_err(),
            "duplicate diagnostic must fail closed"
        );
        for malformed in [
            AgentEventKind::HookDiagnosticCommitted {
                tool_call_id: "other".into(),
                hook_id: "guard".into(),
                phase: "pre_tool_use".into(),
                reason: None,
                stdout: None,
                stderr: None,
                error: None,
            },
            AgentEventKind::HookDiagnosticCommitted {
                tool_call_id: call.id.clone(),
                hook_id: "guard".into(),
                phase: "pre_tool_use".into(),
                reason: Some("x".repeat(8193)),
                stdout: None,
                stderr: None,
                error: None,
            },
            AgentEventKind::HookDiagnosticCommitted {
                tool_call_id: call.id.clone(),
                hook_id: "guard".into(),
                phase: "pre_tool_use".into(),
                reason: None,
                stdout: Some("x".repeat(256 * 1024 + 1)),
                stderr: None,
                error: None,
            },
        ] {
            let mut invalid = events.clone();
            invalid.push(event(9, malformed));
            assert!(replay_native("session-1", &invalid, None, &compatibility()).is_err());
        }
        // The unmodified legacy sequence below remains accepted without diagnostics.
        events.push(event(
            9,
            AgentEventKind::HookFinished {
                tool_call_id: call.id.clone(),
                hook_id: "other".into(),
                phase: "pre_tool_use".into(),
                decision: "allow".into(),
            },
        ));
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 9, .. })
        ));
        events[8].kind = AgentEventKind::HookFinished {
            tool_call_id: call.id.clone(),
            hook_id: "guard".into(),
            phase: "pre_tool_use".into(),
            decision: "block".into(),
        };
        events.push(event(
            10,
            AgentEventKind::ToolFinished {
                tool_call_id: call.id.clone(),
                output: "blocked".into(),
                is_error: true,
            },
        ));
        let completed = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert!(matches!(
            completed.recovery,
            ReplayRecovery::ReadyForModel { next_step: 2 }
        ));

        events[8].kind = AgentEventKind::HookFinished {
            tool_call_id: call.id,
            hook_id: "guard".into(),
            phase: "post_tool_use".into(),
            decision: "block".into(),
        };
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 9, .. })
        ));
    }

    #[test]
    fn tool_catalog_digest_is_bound_once_before_first_model_request() {
        let digest = "a".repeat(64);
        let mut events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ToolCatalogCommitted {
                    digest: digest.clone(),
                },
            ),
        ];
        let replay = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(
            replay.active_tool_catalog_digest.as_deref(),
            Some(digest.as_str())
        );
        events.push(event(
            4,
            AgentEventKind::ToolCatalogCommitted {
                digest: digest.clone(),
            },
        ));
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 4, .. })
        ));
        events[3] = event(
            4,
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "gpt-example".into(),
            },
        );
        assert_eq!(
            replay_native("session-1", &events, None, &compatibility())
                .unwrap()
                .active_tool_catalog_digest
                .as_deref(),
            Some(digest.as_str())
        );
        events.push(event(5, AgentEventKind::ToolCatalogCommitted { digest }));
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 5, .. })
        ));
    }

    #[test]
    fn model_context_marker_is_versioned_and_ordered_before_first_request() {
        let digest = "b".repeat(64);
        let mut events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ToolCatalogCommitted {
                    digest: "a".repeat(64),
                },
            ),
            event(
                4,
                AgentEventKind::ModelContextCommitted {
                    version: MODEL_CONTEXT_VERSION,
                    digest: digest.clone(),
                },
            ),
            event(
                5,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
        ];
        assert_eq!(
            replay_native("session-1", &events, None, &compatibility())
                .unwrap()
                .active_model_context_digest
                .as_deref(),
            Some(digest.as_str())
        );
        events[3] = event(
            4,
            AgentEventKind::ModelContextCommitted {
                version: 1,
                digest: digest.clone(),
            },
        );
        assert_eq!(
            replay_native("session-1", &events, None, &compatibility())
                .unwrap()
                .active_model_context_version,
            Some(1)
        );
        events[3] = event(
            4,
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION + 1,
                digest: digest.clone(),
            },
        );
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 4, .. })
        ));
        events[3] = event(
            4,
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest: digest.clone(),
            },
        );
        events.push(event(
            6,
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest,
            },
        ));
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 6, .. })
        ));
    }

    #[test]
    fn system_instruction_snapshot_is_exact_bounded_and_ordered() {
        let content = "Original AGENTS.md\nSkill text";
        let snapshot = AgentEventKind::SystemInstructionsCommitted {
            version: SYSTEM_INSTRUCTIONS_VERSION,
            digest: system_instructions_digest(content).unwrap(),
            content: content.into(),
        };
        let mut events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ToolCatalogCommitted {
                    digest: "a".repeat(64),
                },
            ),
            event(
                4,
                AgentEventKind::HooksCommitted {
                    version: HOOK_CONFIG_IDENTITY_VERSION,
                    digest: "b".repeat(64),
                },
            ),
            event(
                5,
                AgentEventKind::ExecPolicyCommitted {
                    version: EXEC_POLICY_IDENTITY_VERSION,
                    digest: "c".repeat(64),
                },
            ),
            event(6, snapshot.clone()),
            event(
                7,
                AgentEventKind::ModelContextCommitted {
                    version: MODEL_CONTEXT_VERSION,
                    digest: "d".repeat(64),
                },
            ),
        ];
        let replay = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(replay.active_system_instructions.as_deref(), Some(content));
        assert_eq!(
            replay.active_system_instructions_version,
            Some(SYSTEM_INSTRUCTIONS_VERSION)
        );
        events[5].kind = AgentEventKind::SystemInstructionsCommitted {
            version: SYSTEM_INSTRUCTIONS_VERSION,
            digest: "0".repeat(64),
            content: content.into(),
        };
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 6, .. })
        ));
        events[5].kind = snapshot.clone();
        events.insert(6, event(7, snapshot));
        events[7].sequence = 8;
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 7, .. })
        ));
        events.remove(6);
        events[6].sequence = 7;
        events[5].kind = AgentEventKind::SystemInstructionsCommitted {
            version: SYSTEM_INSTRUCTIONS_VERSION,
            digest: "e".repeat(64),
            content: "x".repeat(crate::MAX_SYSTEM_INSTRUCTIONS_BYTES + 1),
        };
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 6, .. })
        ));
    }

    #[test]
    fn exec_policy_marker_is_versioned_ordered_and_cleared_with_turn() {
        let digest = "c".repeat(64);
        let mut events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ToolCatalogCommitted {
                    digest: "a".repeat(64),
                },
            ),
            event(
                4,
                AgentEventKind::HooksCommitted {
                    version: HOOK_CONFIG_IDENTITY_VERSION,
                    digest: "b".repeat(64),
                },
            ),
            event(
                5,
                AgentEventKind::ExecPolicyCommitted {
                    version: EXEC_POLICY_IDENTITY_VERSION,
                    digest: digest.clone(),
                },
            ),
            event(
                6,
                AgentEventKind::ModelContextCommitted {
                    version: MODEL_CONTEXT_VERSION,
                    digest: "d".repeat(64),
                },
            ),
            event(
                7,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
        ];
        let replay = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(
            replay.active_exec_policy_digest.as_deref(),
            Some(digest.as_str())
        );
        assert_eq!(
            replay.active_exec_policy_version,
            Some(EXEC_POLICY_IDENTITY_VERSION)
        );

        let duplicate = event(
            8,
            AgentEventKind::ExecPolicyCommitted {
                version: EXEC_POLICY_IDENTITY_VERSION,
                digest: digest.clone(),
            },
        );
        events.push(duplicate);
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 8, .. })
        ));
        events.pop();
        events[4].kind = AgentEventKind::ExecPolicyCommitted {
            version: EXEC_POLICY_IDENTITY_VERSION + 1,
            digest: digest.clone(),
        };
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 5, .. })
        ));
        events[4].kind = AgentEventKind::ExecPolicyCommitted {
            version: EXEC_POLICY_IDENTITY_VERSION,
            digest,
        };
        events.swap(3, 4);
        events[3].sequence = 4;
        events[4].sequence = 5;
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 4, .. })
        ));
        events.swap(3, 4);
        events[3].sequence = 4;
        events[4].sequence = 5;
        events.push(event(
            8,
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("Done", vec![]),
                response_id: None,
                usage: None,
            },
        ));
        events.push(event(
            9,
            AgentEventKind::TurnCompleted {
                response: "Done".into(),
                steps: 1,
            },
        ));
        let replay = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert!(replay.active_exec_policy_digest.is_none());
        assert!(replay.active_exec_policy_version.is_none());
    }

    #[test]
    fn legacy_turn_has_no_catalog_digest_and_malformed_commit_is_rejected() {
        let mut events = vec![event(1, admitted())];
        assert!(replay_native("session-1", &events, None, &compatibility())
            .unwrap()
            .active_tool_catalog_digest
            .is_none());
        events.push(event(
            2,
            AgentEventKind::ToolCatalogCommitted {
                digest: "bad".into(),
            },
        ));
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 2, .. })
        ));
    }

    #[test]
    fn mcp_runtime_identity_requires_one_ordered_versioned_marker() {
        let mut events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ToolCatalogCommitted {
                    digest: "a".repeat(64),
                },
            ),
            event(
                4,
                AgentEventKind::McpRuntimeCommitted {
                    version: MCP_RUNTIME_IDENTITY_VERSION,
                    digest: "b".repeat(64),
                },
            ),
        ];
        let replay = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(
            replay.active_mcp_runtime_digest.as_deref(),
            Some("b".repeat(64).as_str())
        );
        assert_eq!(
            replay.active_mcp_runtime_version,
            Some(MCP_RUNTIME_IDENTITY_VERSION)
        );

        let duplicate = event(
            5,
            AgentEventKind::McpRuntimeCommitted {
                version: MCP_RUNTIME_IDENTITY_VERSION,
                digest: "b".repeat(64),
            },
        );
        events.push(duplicate);
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 5, .. })
        ));
        events.pop();
        events[3].kind = AgentEventKind::McpRuntimeCommitted {
            version: MCP_RUNTIME_IDENTITY_VERSION + 1,
            digest: "b".repeat(64),
        };
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 4, .. })
        ));
        events[3].kind = AgentEventKind::McpRuntimeCommitted {
            version: MCP_RUNTIME_IDENTITY_VERSION,
            digest: "BAD".into(),
        };
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 4, .. })
        ));
    }

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "read_file".into(),
            arguments: json!({"path":format!("{id}.txt")}),
        }
    }

    fn response(step: usize, assistant: AgentMessage) -> AgentEventKind {
        AgentEventKind::ModelResponseCommitted {
            step,
            assistant,
            response_id: Some(format!("response-{step}")),
            usage: Some(ModelTokenUsage {
                input_tokens: 100,
                output_tokens: 20,
                total_tokens: 120,
                reasoning_tokens: None,
                cached_input_tokens: None,
            }),
        }
    }

    #[test]
    fn admitted_model_and_permission_interruption_states() {
        let one = vec![event(1, admitted())];
        let replay = replay_native("session-1", &one, None, &compatibility()).unwrap();
        assert_eq!(replay.recovery, ReplayRecovery::AdmittedNotStarted);
        assert!(!replay.can_start_new_turn);
        assert_eq!(replay.history, vec![AgentMessage::user("Build this")]);
        assert_eq!(replay.committed_messages[0].event_sequence, 1);

        let mut events = one;
        events.push(event(
            2,
            AgentEventKind::TurnStarted {
                prompt: "Build this".into(),
            },
        ));
        events.push(event(
            3,
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "gpt-example".into(),
            },
        ));
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility())
                .unwrap()
                .recovery,
            ReplayRecovery::ModelInFlight { step: 1, .. }
        ));
        events.push(event(
            4,
            response(1, AgentMessage::assistant("", vec![call("a")])),
        ));
        events.push(event(
            5,
            AgentEventKind::PermissionRequested {
                request_id: "permission-1".into(),
                tool_call: call("a"),
            },
        ));
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()).unwrap().recovery,
            ReplayRecovery::PermissionPending { request_id, .. } if request_id == "permission-1"
        ));
    }

    #[test]
    fn a_pause_marks_the_open_turn_without_changing_its_recovery_state() {
        let paused = |step, class, resets_at_ms| AgentEventKind::TurnPaused {
            class,
            limit_key: "key".into(),
            resets_at_ms,
            step,
            detail: "HTTP 429".into(),
        };
        let replay = |events: &[AgentEvent]| replay_native("session-1", events, None, &compatibility());
        let mut events = vec![
            event(1, admitted()),
            event(2, AgentEventKind::TurnStarted { prompt: "Build this".into() }),
        ];
        // Blocked before the request: still ReadyForModel, now with a cause.
        events.push(event(3, paused(1, crate::TurnPauseClass::LimitReached, Some(9_000))));
        let state = replay(&events).unwrap();
        assert_eq!(state.recovery, ReplayRecovery::ReadyForModel { next_step: 1 });
        assert_eq!(state.pause.as_ref().map(|pause| (pause.step, pause.resets_at_ms, pause.pauses_in_turn)), Some((1, Some(9_000), 1)));
        events.push(event(4, AgentEventKind::ModelRequestStarted { step: 1, model: "gpt-example".into() }));
        // Failed in flight: still ModelInFlight, the same request is reused.
        events.push(event(5, paused(1, crate::TurnPauseClass::OutOfCredits, None)));
        let state = replay(&events).unwrap();
        assert!(matches!(state.recovery, ReplayRecovery::ModelInFlight { step: 1, .. }));
        let pause = state.pause.unwrap();
        assert_eq!((pause.class, pause.pauses_in_turn, pause.event_sequence), (crate::TurnPauseClass::OutOfCredits, 2, 5));
        assert!(!state.can_start_new_turn);
        // A pause for another step is not this boundary.
        let mut wrong = events.clone();
        wrong.push(event(6, paused(2, crate::TurnPauseClass::Auth, None)));
        assert!(replay(&wrong).is_err());
        // The committed response clears it; a pause while a tool is open is refused.
        events.push(event(6, response(1, AgentMessage::assistant("", vec![call("a")]))));
        assert!(replay(&events).unwrap().pause.is_none());
        let mut open_tool = events.clone();
        open_tool.push(event(7, AgentEventKind::ToolStarted { tool_call: call("a") }));
        open_tool.push(event(8, paused(2, crate::TurnPauseClass::LimitReached, None)));
        assert!(replay(&open_tool).is_err());
        // With the batch pending, even before the tool starts, it is refused.
        let mut pending = events.clone();
        pending.push(event(7, paused(2, crate::TurnPauseClass::LimitReached, None)));
        assert!(replay(&pending).is_err());
        // A terminal event clears the marker.
        let mut cancelled = events[..5].to_vec();
        cancelled.push(event(6, AgentEventKind::TurnCancelled));
        let state = replay(&cancelled).unwrap();
        assert!(state.pause.is_none() && state.can_start_new_turn);
        // The event survives its JSONL round trip.
        let line = serde_json::to_string(&events[4]).unwrap();
        assert!(line.contains("\"turn_paused\"") && line.contains("\"out_of_credits\""));
        assert_eq!(serde_json::from_str::<AgentEvent>(&line).unwrap(), events[4]);
    }

    #[test]
    fn a_superseded_turn_closes_like_a_cancellation_and_names_what_continues_it() {
        let superseded = |host_prompt| AgentEventKind::TurnSuperseded {
            provider_id: "other".into(),
            model_id: "other-model".into(),
            next_turn_id: "turn-2".into(),
            host_prompt,
        };
        let paused = |step| AgentEventKind::TurnPaused {
            class: crate::TurnPauseClass::LimitReached,
            limit_key: "key".into(),
            resets_at_ms: None,
            step,
            detail: "HTTP 429".into(),
        };
        let replay = |events: &[AgentEvent]| replay_native("session-1", events, None, &compatibility());
        let started = vec![
            event(1, admitted()),
            event(2, AgentEventKind::TurnStarted { prompt: "Build this".into() }),
        ];
        // Only a paused turn can move: an open one must be cancelled or resumed.
        let mut open = started.clone();
        open.push(event(3, superseded(false)));
        assert!(replay(&open).is_err());

        // Paused before any response: the prompt leaves the model history,
        // exactly as a cancellation does, and waits to be admitted again.
        let mut first = started.clone();
        first.push(event(3, paused(1)));
        first.push(event(4, superseded(false)));
        let state = replay(&first).unwrap();
        assert_eq!(state.recovery, ReplayRecovery::Idle);
        assert!(state.can_start_new_turn && state.pause.is_none() && state.history.is_empty());
        let pending = state.superseded.unwrap();
        assert_eq!(
            (pending.model_id.as_str(), pending.next_turn_id.as_str(), pending.host_prompt, pending.prompt_event_sequence, pending.event_sequence),
            ("other-model", "turn-2", false, Some(1), 4)
        );
        let mut cancelled = first[..3].to_vec();
        cancelled.push(event(4, AgentEventKind::TurnCancelled));
        let as_cancelled = replay(&cancelled).unwrap();
        assert_eq!((&state.history, &state.recovery), (&as_cancelled.history, &as_cancelled.recovery));
        assert!(as_cancelled.superseded.is_none());

        // Paused after committed work: every pair stays in the history.
        let mut worked = started.clone();
        worked.push(event(3, AgentEventKind::ModelRequestStarted { step: 1, model: "gpt-example".into() }));
        worked.push(event(4, response(1, AgentMessage::assistant("", vec![call("a")]))));
        worked.push(event(5, AgentEventKind::ToolStarted { tool_call: call("a") }));
        worked.push(event(6, AgentEventKind::ToolFinished { tool_call_id: "a".into(), output: "ok".into(), is_error: false }));
        worked.push(event(7, paused(2)));
        worked.push(event(8, superseded(true)));
        let state = replay(&worked).unwrap();
        assert_eq!(state.history.len(), 3, "prompt, call and result");
        assert!(state.superseded.is_some());
        // The admission that continues it settles the switch.
        worked.push(event(9, AgentEventKind::TurnAdmitted {
            turn_id: "turn-2".into(),
            prompt: "continue".into(),
            model: "other-model".into(),
            mode: TurnMode::Build,
        }));
        assert!(replay(&worked).unwrap().superseded.is_none());
        // A second terminal event on the closed turn is refused.
        let mut twice = first.clone();
        twice.push(event(5, AgentEventKind::TurnCancelled));
        assert!(replay(&twice).is_err());
        // Old journals have no such event and new ones round-trip it.
        let line = serde_json::to_string(&first[3]).unwrap();
        assert!(line.contains("\"turn_superseded\""));
        assert_eq!(serde_json::from_str::<AgentEvent>(&line).unwrap(), first[3]);
        let notice = AgentEvent::new("session-1", 5, AgentEventKind::HostNotice { code: "x".into(), detail: "y".into() });
        let mut noted = first.clone();
        noted.push(notice);
        assert!(replay(&noted).unwrap().superseded.is_some(), "a notice changes nothing");
    }

    #[test]
    fn multiple_steps_preserve_complete_pairs_and_opaque_response_items() {
        let mut assistant = AgentMessage::assistant("", vec![call("a"), call("b")]);
        assistant.provider_output_items =
            vec![json!({"type":"reasoning","encrypted_content":"opaque"})];
        let events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(4, response(1, assistant.clone())),
            event(
                5,
                AgentEventKind::ToolStarted {
                    tool_call: call("a"),
                },
            ),
            event(
                6,
                AgentEventKind::ToolFinished {
                    tool_call_id: "a".into(),
                    output: "A".into(),
                    is_error: false,
                },
            ),
            event(
                7,
                AgentEventKind::ToolStarted {
                    tool_call: call("b"),
                },
            ),
            event(
                8,
                AgentEventKind::ToolFinished {
                    tool_call_id: "b".into(),
                    output: "B".into(),
                    is_error: false,
                },
            ),
            event(
                9,
                AgentEventKind::ModelRequestStarted {
                    step: 2,
                    model: "gpt-example".into(),
                },
            ),
            event(10, response(2, AgentMessage::assistant("Done", vec![]))),
            event(
                11,
                AgentEventKind::AssistantMessage {
                    content: "Done".into(),
                },
            ),
            event(
                12,
                AgentEventKind::TurnCompleted {
                    response: "Done".into(),
                    steps: 2,
                },
            ),
        ];
        let replay = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(replay.recovery, ReplayRecovery::Idle);
        assert!(replay.can_start_new_turn);
        assert_eq!(replay.history.len(), 5);
        assert_eq!(
            replay
                .committed_messages
                .iter()
                .map(|message| message.event_sequence)
                .collect::<Vec<_>>(),
            vec![1, 4, 6, 8, 10]
        );
        assert_eq!(replay.history[1], assistant);
        assert_eq!(replay.history[2].tool_call_id.as_deref(), Some("a"));
        assert_eq!(replay.history[3].tool_call_id.as_deref(), Some("b"));
        assert_eq!(replay.history[4].content, "Done");
        assert_eq!(replay.latest_response_id.as_deref(), Some("response-2"));
        assert_eq!(replay.latest_usage.unwrap().input_tokens, 100);
    }

    #[test]
    fn partial_batch_keeps_results_separate_and_marks_uncertain_effect() {
        let mut events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(
                4,
                response(1, AgentMessage::assistant("", vec![call("a"), call("b")])),
            ),
            event(
                5,
                AgentEventKind::ToolStarted {
                    tool_call: call("a"),
                },
            ),
            event(
                6,
                AgentEventKind::ToolFinished {
                    tool_call_id: "a".into(),
                    output: "A".into(),
                    is_error: false,
                },
            ),
        ];
        let replay = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(replay.history.len(), 1);
        assert!(!replay.can_start_new_turn);
        assert!(
            matches!(replay.recovery, ReplayRecovery::ToolReady { tool_call } if tool_call.id == "b")
        );
        let batch = replay.pending_tool_batch.unwrap();
        assert_eq!(batch.completed_results.len(), 1);
        assert_eq!(batch.assistant_event_sequence, 4);
        assert_eq!(batch.completed_result_sequences, vec![6]);
        assert_eq!(
            batch.completed_results[0].tool_call_id.as_deref(),
            Some("a")
        );
        assert_eq!(batch.next_call_index, 1);

        events.push(event(
            7,
            AgentEventKind::ToolStarted {
                tool_call: call("b"),
            },
        ));
        let uncertain = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert!(!uncertain.can_start_new_turn);
        assert!(
            matches!(uncertain.recovery, ReplayRecovery::ToolOutcomeUncertain { tool_call } if tool_call.id == "b")
        );
        assert_eq!(uncertain.history.len(), 1);
        assert_eq!(
            uncertain
                .pending_tool_batch
                .unwrap()
                .completed_results
                .len(),
            1
        );
    }

    #[test]
    fn two_persisted_read_starts_recover_one_uncertain_id_at_a_time() {
        let mut events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(
                4,
                response(1, AgentMessage::assistant("", vec![call("a"), call("b")])),
            ),
            event(
                5,
                AgentEventKind::ToolStarted {
                    tool_call: call("a"),
                },
            ),
            event(
                6,
                AgentEventKind::ToolStarted {
                    tool_call: call("b"),
                },
            ),
        ];
        let interrupted = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert!(!interrupted.can_start_new_turn);
        assert!(matches!(
            interrupted.recovery,
            ReplayRecovery::ToolOutcomeUncertain { tool_call } if tool_call.id == "a"
        ));
        assert_eq!(interrupted.pending_tool_batch.unwrap().next_call_index, 0);
        assert_eq!(interrupted.history, vec![AgentMessage::user("Build this")]);

        events.push(event(
            7,
            AgentEventKind::TurnFailed {
                error: "worker crashed".into(),
            },
        ));
        events.push(event(
            8,
            AgentEventKind::ToolFinished {
                tool_call_id: "a".into(),
                output: "verified A".into(),
                is_error: false,
            },
        ));
        let partial = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert!(!partial.can_start_new_turn);
        assert!(matches!(
            partial.recovery,
            ReplayRecovery::ToolOutcomeUncertain { tool_call } if tool_call.id == "b"
        ));
        let batch = partial.pending_tool_batch.unwrap();
        assert_eq!(batch.next_call_index, 1);
        assert_eq!(batch.completed_result_sequences, vec![8]);
        assert_eq!(batch.completed_results[0].content, "verified A");
        assert_eq!(partial.history, vec![AgentMessage::user("Build this")]);

        events.push(event(
            9,
            AgentEventKind::ToolFinished {
                tool_call_id: "b".into(),
                output: "verified B".into(),
                is_error: false,
            },
        ));
        let recovered = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(recovered.recovery, ReplayRecovery::Idle);
        assert!(recovered.can_start_new_turn);
        assert_eq!(
            recovered
                .committed_messages
                .iter()
                .map(|entry| entry.event_sequence)
                .collect::<Vec<_>>(),
            vec![1, 4, 8, 9],
        );
        assert_eq!(
            recovered.history[1]
                .tool_calls
                .iter()
                .map(|call| call.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(recovered.history[2].tool_call_id.as_deref(), Some("a"));
        assert_eq!(recovered.history[3].tool_call_id.as_deref(), Some("b"));
    }

    #[test]
    fn concurrent_read_replay_rejects_out_of_order_or_untrusted_starts() {
        let base = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(
                4,
                response(1, AgentMessage::assistant("", vec![call("a"), call("b")])),
            ),
            event(
                5,
                AgentEventKind::ToolStarted {
                    tool_call: call("a"),
                },
            ),
            event(
                6,
                AgentEventKind::ToolStarted {
                    tool_call: call("b"),
                },
            ),
        ];
        let mut out_of_order = base.clone();
        out_of_order.push(event(
            7,
            AgentEventKind::ToolFinished {
                tool_call_id: "b".into(),
                output: "B".into(),
                is_error: false,
            },
        ));
        assert!(matches!(
            replay_native("session-1", &out_of_order, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 7, .. })
        ));

        let mut untrusted = base[..4].to_vec();
        let mut command = call("b");
        command.name = "run_command".into();
        if let AgentEventKind::ModelResponseCommitted { assistant, .. } = &mut untrusted[3].kind {
            assistant.tool_calls[1] = command.clone();
        }
        untrusted.push(event(
            5,
            AgentEventKind::ToolStarted {
                tool_call: call("a"),
            },
        ));
        untrusted.push(event(6, AgentEventKind::ToolStarted { tool_call: command }));
        assert!(matches!(
            replay_native("session-1", &untrusted, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 6, .. })
        ));
    }

    #[test]
    fn verified_last_tool_after_failed_turn_closes_recovery_without_retry() {
        let mut events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(4, response(1, AgentMessage::assistant("", vec![call("a")]))),
            event(
                5,
                AgentEventKind::ToolStarted {
                    tool_call: call("a"),
                },
            ),
            event(
                6,
                AgentEventKind::TurnFailed {
                    error: "worker stopped".into(),
                },
            ),
        ];
        let uncertain = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert!(matches!(
            uncertain.recovery,
            ReplayRecovery::ToolOutcomeUncertain { tool_call } if tool_call.id == "a"
        ));
        events.push(event(
            7,
            AgentEventKind::ToolFinished {
                tool_call_id: "a".into(),
                output: "independently verified result".into(),
                is_error: false,
            },
        ));
        let resolved = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(resolved.recovery, ReplayRecovery::Idle);
        assert!(resolved.can_start_new_turn);
        assert!(resolved.active_turn.is_none());
        assert_eq!(resolved.history.len(), 3);
        assert_eq!(resolved.history[1].tool_calls[0].id, "a");
        assert_eq!(resolved.history[2].content, "independently verified result");
        assert_eq!(resolved.history[2].tool_call_id.as_deref(), Some("a"));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ModelRequestStarted { .. }))
                .count(),
            1
        );
        let mut duplicate = events.clone();
        duplicate.push(event(
            8,
            AgentEventKind::ToolStarted {
                tool_call: call("a"),
            },
        ));
        assert!(matches!(
            replay_native("session-1", &duplicate, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 8, .. })
        ));
        events.push(event(
            8,
            AgentEventKind::TurnAdmitted {
                turn_id: "turn-2".into(),
                prompt: "Next task".into(),
                model: "gpt-example".into(),
                mode: TurnMode::Build,
            },
        ));
        assert_eq!(
            replay_native("session-1", &events, None, &compatibility())
                .unwrap()
                .recovery,
            ReplayRecovery::AdmittedNotStarted
        );
    }

    #[test]
    fn verified_tool_with_more_aborted_calls_still_requires_manual_discard() {
        let mut events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(
                4,
                response(1, AgentMessage::assistant("", vec![call("a"), call("b")])),
            ),
            event(
                5,
                AgentEventKind::ToolStarted {
                    tool_call: call("a"),
                },
            ),
            event(6, AgentEventKind::TurnCancelled),
            event(
                7,
                AgentEventKind::ToolFinished {
                    tool_call_id: "a".into(),
                    output: "verified result".into(),
                    is_error: false,
                },
            ),
        ];
        let unresolved = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert!(!unresolved.can_start_new_turn);
        assert!(matches!(
            unresolved.recovery,
            ReplayRecovery::AbortedToolBatch { next_tool_call } if next_tool_call.id == "b"
        ));
        assert_eq!(
            unresolved
                .pending_tool_batch
                .unwrap()
                .completed_results
                .len(),
            1
        );
        assert_eq!(unresolved.history.len(), 1);
        events.push(event(
            8,
            AgentEventKind::ToolBatchDiscarded {
                next_tool_call_id: "b".into(),
            },
        ));
        let discarded = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(discarded.recovery, ReplayRecovery::Idle);
        assert!(discarded.can_start_new_turn);
        assert!(discarded.history.is_empty());
    }

    fn aborted_batch_events() -> Vec<AgentEvent> {
        vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(
                4,
                AgentEventKind::AssistantMessage {
                    content: "Prior result".into(),
                },
            ),
            event(
                5,
                AgentEventKind::TurnCompleted {
                    response: "Prior result".into(),
                    steps: 1,
                },
            ),
            event(
                6,
                AgentEventKind::TurnAdmitted {
                    turn_id: "turn-2".into(),
                    prompt: "Second task".into(),
                    model: "gpt-example".into(),
                    mode: TurnMode::Build,
                },
            ),
            event(
                7,
                AgentEventKind::TurnStarted {
                    prompt: "Second task".into(),
                },
            ),
            event(
                8,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(
                9,
                response(1, AgentMessage::assistant("", vec![call("a"), call("b")])),
            ),
            event(
                10,
                AgentEventKind::ToolStarted {
                    tool_call: call("a"),
                },
            ),
            event(
                11,
                AgentEventKind::ToolFinished {
                    tool_call_id: "a".into(),
                    output: "confirmed effect".into(),
                    is_error: false,
                },
            ),
            event(
                12,
                AgentEventKind::TurnFailed {
                    error: "interrupted".into(),
                },
            ),
        ]
    }

    #[test]
    fn discard_closes_aborted_batch_and_removes_entire_turn_from_model_history() {
        let mut events = aborted_batch_events();
        let before = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert!(
            matches!(before.recovery, ReplayRecovery::AbortedToolBatch { next_tool_call } if next_tool_call.id == "b")
        );
        assert_eq!(before.history.len(), 3);
        events.push(event(
            13,
            AgentEventKind::ToolBatchDiscarded {
                next_tool_call_id: "b".into(),
            },
        ));
        let after = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(after.recovery, ReplayRecovery::Idle);
        assert!(after.can_start_new_turn);
        assert!(after.active_turn.is_none());
        assert!(after.pending_tool_batch.is_none());
        assert_eq!(after.history.len(), 2);
        assert_eq!(after.history[0], AgentMessage::user("Build this"));
        assert_eq!(
            after.history[1],
            AgentMessage::assistant("Prior result", vec![])
        );
        assert!(after
            .committed_messages
            .iter()
            .all(|message| message.event_sequence < 6));
        events.push(event(
            14,
            AgentEventKind::TurnAdmitted {
                turn_id: "turn-3".into(),
                prompt: "New task".into(),
                model: "gpt-example".into(),
                mode: TurnMode::Build,
            },
        ));
        assert!(replay_native("session-1", &events, None, &compatibility()).is_ok());
    }

    #[test]
    fn discard_requires_terminal_batch_exact_call_and_no_started_effect() {
        let mut events = aborted_batch_events();
        events.push(event(
            13,
            AgentEventKind::ToolBatchDiscarded {
                next_tool_call_id: "wrong".into(),
            },
        ));
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 13, .. })
        ));
        events[12] = event(
            13,
            AgentEventKind::ToolStarted {
                tool_call: call("b"),
            },
        );
        events.push(event(
            14,
            AgentEventKind::ToolBatchDiscarded {
                next_tool_call_id: "b".into(),
            },
        ));
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 14, .. })
        ));
        events.pop();
        events[12] = event(
            13,
            AgentEventKind::ToolBatchDiscarded {
                next_tool_call_id: "b".into(),
            },
        );
        events[11] = event(
            12,
            AgentEventKind::PermissionRequested {
                request_id: "ask-b".into(),
                tool_call: call("b"),
            },
        );
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 13, .. })
        ));
    }

    #[test]
    fn discard_clears_permission_requested_before_a_failed_turn() {
        let mut events = aborted_batch_events();
        events[11] = event(
            12,
            AgentEventKind::PermissionRequested {
                request_id: "ask-b".into(),
                tool_call: call("b"),
            },
        );
        events.push(event(13, AgentEventKind::TurnCancelled));
        events.push(event(
            14,
            AgentEventKind::ToolBatchDiscarded {
                next_tool_call_id: "b".into(),
            },
        ));
        let replay = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(replay.recovery, ReplayRecovery::Idle);
        assert!(replay.can_start_new_turn);
        assert_eq!(replay.history.len(), 2);
    }

    #[test]
    fn checkpoint_marker_is_required_and_corruption_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = CheckpointFileStore::new(dir.path()).unwrap();
        let checkpoint = Checkpoint {
            session_id: "session-1".into(),
            checkpoint_id: "checkpoint-1".into(),
            covered_event_sequence: 6,
            compatibility: compatibility(),
            summary: "{\"objective\":\"Build this\"}".into(),
            retained_user_messages: vec![RetainedUserMessage {
                event_sequence: 1,
                content: "Build this".into(),
            }],
            budget: CheckpointBudgetSnapshot {
                input_limit_tokens: None,
                context_window_tokens: 128_000,
                estimated_input_tokens: 1_000,
                observed_input_tokens: Some(400),
                output_reserve_tokens: 4_096,
                tool_schema_reserve_tokens: 100,
                limits_unverified: false,
            },
        };
        let reference = store.write_candidate(&checkpoint).unwrap();
        let mut events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(4, response(1, AgentMessage::assistant("Done", vec![]))),
            event(
                5,
                AgentEventKind::AssistantMessage {
                    content: "Done".into(),
                },
            ),
            event(
                6,
                AgentEventKind::TurnCompleted {
                    response: "Done".into(),
                    steps: 1,
                },
            ),
        ];
        let orphan = replay_native("session-1", &events, Some(&store), &compatibility()).unwrap();
        assert!(orphan.checkpoint.is_none());
        events.push(event(
            7,
            AgentEventKind::CheckpointCommitted {
                reference: reference.clone(),
            },
        ));
        let replay = replay_native("session-1", &events, Some(&store), &compatibility()).unwrap();
        assert_eq!(replay.checkpoint, Some(checkpoint));
        assert_eq!(replay.history.len(), 2);
        assert_eq!(replay.history[0], AgentMessage::user("Build this"));
        assert_eq!(replay.history[1].role, MessageRole::Assistant);

        let path = dir
            .path()
            .join("checkpoints")
            .join("session-1")
            .join("checkpoint-1.json");
        std::fs::write(path, b"corrupt").unwrap();
        assert!(matches!(
            replay_native("session-1", &events, Some(&store), &compatibility()),
            Err(ReplayError::Checkpoint(_))
        ));
    }

    #[test]
    fn legacy_log_recovers_tool_pair_and_conflicts_fail_closed() {
        let mut events = vec![
            event(
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Old task".into(),
                },
            ),
            event(
                2,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ToolStarted {
                    tool_call: call("a"),
                },
            ),
            event(
                4,
                AgentEventKind::ToolFinished {
                    tool_call_id: "a".into(),
                    output: "A".into(),
                    is_error: false,
                },
            ),
            event(
                5,
                AgentEventKind::ModelRequestStarted {
                    step: 2,
                    model: "gpt-example".into(),
                },
            ),
            event(
                6,
                AgentEventKind::AssistantMessage {
                    content: "Done".into(),
                },
            ),
            event(
                7,
                AgentEventKind::TurnCompleted {
                    response: "Done".into(),
                    steps: 2,
                },
            ),
        ];
        let replay = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(replay.history.len(), 4);
        assert_eq!(replay.history[1].tool_calls[0].id, "a");
        assert_eq!(replay.history[2].tool_call_id.as_deref(), Some("a"));
        events[3].kind = AgentEventKind::ToolFinished {
            tool_call_id: "wrong".into(),
            output: "A".into(),
            is_error: false,
        };
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { .. })
        ));
        events[3].sequence = 8;
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::InvalidSequence(8))
        ));
    }

    #[test]
    fn modern_final_response_cannot_gain_a_legacy_tool_call() {
        let events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(4, response(1, AgentMessage::assistant("Done", vec![]))),
            event(
                5,
                AgentEventKind::ToolStarted {
                    tool_call: call("unexpected"),
                },
            ),
        ];
        assert!(matches!(
            replay_native("session-1", &events, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 5, .. })
        ));
    }

    #[test]
    fn failed_turn_before_response_does_not_duplicate_retry_prompt() {
        let mut events = vec![
            event(1, admitted()),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::TurnFailed {
                    error: "offline".into(),
                },
            ),
        ];
        let replay = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(replay.recovery, ReplayRecovery::Idle);
        assert!(replay.can_start_new_turn);
        assert!(replay.history.is_empty());
        assert!(replay.committed_messages.is_empty());

        events.pop();
        events.push(event(
            3,
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "gpt-example".into(),
            },
        ));
        events.push(event(
            4,
            response(1, AgentMessage::assistant("Partial answer", vec![])),
        ));
        events.push(event(
            5,
            AgentEventKind::TurnFailed {
                error: "later".into(),
            },
        ));
        let replay = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(replay.history[0], AgentMessage::user("Build this"));
        assert_eq!(replay.history[1].content, "Partial answer");
        assert!(replay.can_start_new_turn);
    }

    #[test]
    fn plan_snapshots_survive_checkpoint_and_freeze_build_handoff() {
        let dir = tempfile::tempdir().unwrap();
        let store = CheckpointFileStore::new(dir.path()).unwrap();
        let plan_text = "1. Inspect files\n2. Implement change";
        let proposed = PlanSnapshot::from_plan_response(plan_text);
        let updated = PlanSnapshot {
            explanation: Some("Inspection finished".into()),
            plan: vec![
                PlanStep {
                    step: "Inspect files".into(),
                    status: PlanStepStatus::Completed,
                },
                PlanStep {
                    step: "Implement change".into(),
                    status: PlanStepStatus::InProgress,
                },
            ],
        };
        let checkpoint = Checkpoint {
            session_id: "session-1".into(),
            checkpoint_id: "plan-checkpoint".into(),
            covered_event_sequence: 7,
            compatibility: compatibility(),
            summary: "Earlier task summary".into(),
            retained_user_messages: vec![RetainedUserMessage {
                event_sequence: 1,
                content: "Plan this".into(),
            }],
            budget: CheckpointBudgetSnapshot::unverified(100, None),
        };
        let reference = store.write_candidate(&checkpoint).unwrap();
        let call = ToolCall {
            id: "update-1".into(),
            name: "update_plan".into(),
            arguments: serde_json::to_value(&updated).unwrap(),
        };
        let mut events = vec![
            event(
                1,
                AgentEventKind::TurnAdmitted {
                    turn_id: "plan-turn".into(),
                    prompt: "Plan this".into(),
                    model: "gpt-example".into(),
                    mode: TurnMode::Plan,
                },
            ),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Plan this".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(4, response(1, AgentMessage::assistant(plan_text, vec![]))),
            event(
                5,
                AgentEventKind::AssistantMessage {
                    content: plan_text.into(),
                },
            ),
            event(
                6,
                AgentEventKind::PlanProposed {
                    plan: proposed.clone(),
                },
            ),
            event(
                7,
                AgentEventKind::TurnCompleted {
                    response: plan_text.into(),
                    steps: 1,
                },
            ),
            event(8, AgentEventKind::CheckpointCommitted { reference }),
            event(
                9,
                AgentEventKind::TurnAdmitted {
                    turn_id: "build-turn".into(),
                    prompt: "Build this".into(),
                    model: "gpt-example".into(),
                    mode: TurnMode::Build,
                },
            ),
            event(
                10,
                AgentEventKind::TurnStarted {
                    prompt: "Build this".into(),
                },
            ),
            event(
                11,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(
                12,
                response(1, AgentMessage::assistant("", vec![call.clone()])),
            ),
            event(
                13,
                AgentEventKind::ToolStarted {
                    tool_call: call.clone(),
                },
            ),
            event(
                14,
                AgentEventKind::PlanUpdated {
                    tool_call_id: call.id.clone(),
                    plan: updated.clone(),
                },
            ),
        ];
        let replay = replay_native("session-1", &events, Some(&store), &compatibility()).unwrap();
        assert_eq!(replay.latest_plan, Some(updated.clone()));
        assert_eq!(replay.active_plan_at_turn_start, Some(proposed.clone()));
        assert!(matches!(
            replay.recovery,
            ReplayRecovery::ToolOutcomeUncertain { .. }
        ));
        events.push(event(
            15,
            AgentEventKind::ToolFinished {
                tool_call_id: call.id.clone(),
                output: "Plan updated".into(),
                is_error: false,
            },
        ));
        let replay = replay_native("session-1", &events, Some(&store), &compatibility()).unwrap();
        assert_eq!(replay.latest_plan, Some(updated));
        assert_eq!(replay.active_plan_at_turn_start, Some(proposed));

        if let AgentEventKind::ToolFinished { is_error, .. } = &mut events[14].kind {
            *is_error = true;
        }
        assert!(matches!(
            replay_native("session-1", &events, Some(&store), &compatibility()),
            Err(ReplayError::Conflict { sequence: 15, .. })
        ));
        events[13].kind = AgentEventKind::PlanUpdated {
            tool_call_id: call.id,
            plan: PlanSnapshot::from_plan_response("Different"),
        };
        assert!(matches!(
            replay_native("session-1", &events, Some(&store), &compatibility()),
            Err(ReplayError::Conflict { sequence: 14, .. })
        ));
    }

    #[test]
    fn steered_plan_turn_accepts_revised_proposal_after_new_model_response() {
        let first_text = "Initial plan";
        let revised_text = "Revised plan";
        let first_plan = PlanSnapshot::from_plan_response(first_text);
        let revised_plan = PlanSnapshot::from_plan_response(revised_text);
        let events = vec![
            event(
                1,
                AgentEventKind::TurnAdmitted {
                    turn_id: "plan-turn".into(),
                    prompt: "Plan it".into(),
                    model: "gpt-example".into(),
                    mode: TurnMode::Plan,
                },
            ),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Plan it".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(4, response(1, AgentMessage::assistant(first_text, vec![]))),
            event(
                5,
                AgentEventKind::AssistantMessage {
                    content: first_text.into(),
                },
            ),
            event(
                6,
                AgentEventKind::PlanProposed {
                    plan: first_plan.clone(),
                },
            ),
            event(
                7,
                AgentEventKind::TurnSteered {
                    turn_id: "plan-turn".into(),
                    input_id: "input-2".into(),
                    text: "Change the plan".into(),
                },
            ),
            event(
                8,
                AgentEventKind::ModelRequestStarted {
                    step: 2,
                    model: "gpt-example".into(),
                },
            ),
            event(
                9,
                response(2, AgentMessage::assistant(revised_text, vec![])),
            ),
            event(
                10,
                AgentEventKind::AssistantMessage {
                    content: revised_text.into(),
                },
            ),
            event(
                11,
                AgentEventKind::PlanProposed {
                    plan: revised_plan.clone(),
                },
            ),
            event(
                12,
                AgentEventKind::TurnCompleted {
                    response: revised_text.into(),
                    steps: 2,
                },
            ),
        ];
        let replay = replay_native("session-1", &events, None, &compatibility()).unwrap();
        assert_eq!(replay.latest_plan, Some(revised_plan));
        assert_eq!(
            replay
                .history
                .iter()
                .filter(|message| message.role == MessageRole::User)
                .count(),
            2
        );
        let mut invalid = events[..7].to_vec();
        invalid.push(event(8, AgentEventKind::PlanProposed { plan: first_plan }));
        assert!(matches!(
            replay_native("session-1", &invalid, None, &compatibility()),
            Err(ReplayError::Conflict { sequence: 8, .. })
        ));
    }

    #[test]
    fn a_tool_result_replays_with_the_images_its_media_marked_for_the_model() {
        let image =
            crate::UserImage::from_bytes(crate::UserImageMime::Png, b"\x89PNG\r\n\x1a\nshot")
                .unwrap();
        let shot = ToolCall {
            id: "shot".into(),
            name: "mcp__browser__take_screenshot".into(),
            arguments: serde_json::json!({}),
        };
        let journal = |model_images: Vec<u8>| {
            vec![
                event(1, admitted()),
                event(2, AgentEventKind::TurnStarted { prompt: "Build this".into() }),
                event(3, AgentEventKind::ModelRequestStarted { step: 1, model: "gpt-example".into() }),
                event(4, response(1, AgentMessage::assistant("", vec![shot.clone()]))),
                event(5, AgentEventKind::ToolStarted { tool_call: shot.clone() }),
                event(
                    6,
                    AgentEventKind::ToolResultMedia {
                        tool_call_id: "shot".into(),
                        images: vec![image.clone(), image.clone()],
                        text: String::new(),
                        notices: vec![],
                        model_images,
                    },
                ),
                event(
                    7,
                    AgentEventKind::ToolFinished {
                        tool_call_id: "shot".into(),
                        output: "captured".into(),
                        is_error: false,
                    },
                ),
            ]
        };
        let replay = |events: &[AgentEvent]| replay_native("session-1", events, None, &compatibility());
        let sent = replay(&journal(vec![1])).unwrap();
        assert_eq!(sent.history.last().unwrap().content, "captured");
        assert_eq!(sent.history.last().unwrap().images, vec![image.clone()]);
        assert_eq!(sent.committed_messages.last().unwrap().message.images, vec![image.clone()]);
        // A journal written before tool images reached a model has no such
        // mark: it loads, and its result is the text the model read then.
        let old = serde_json::to_value(journal(vec![])).unwrap();
        assert!(old[5]["kind"].get("model_images").is_none(), "{}", old[5]["kind"]["type"]);
        let old: Vec<AgentEvent> = serde_json::from_value(old).unwrap();
        assert!(replay(&old).unwrap().history.last().unwrap().images.is_empty());
        // A mark that names no image of the event is a corrupt journal.
        for bad in [vec![2], vec![1, 0], vec![0, 0]] {
            assert!(matches!(replay(&journal(bad)), Err(ReplayError::Conflict { sequence: 6, .. })));
        }

        // A checkpoint that covers the result summarises it: its image leaves
        // the model's history with it, and stays in the journal for the view.
        let dir = tempfile::tempdir().unwrap();
        let checkpoint_store = CheckpointFileStore::new(dir.path()).unwrap();
        let checkpoint = Checkpoint {
            session_id: "session-1".into(),
            checkpoint_id: "after-capture".into(),
            covered_event_sequence: 11,
            compatibility: compatibility(),
            summary: "A screenshot was taken and described".into(),
            retained_user_messages: vec![RetainedUserMessage {
                event_sequence: 1,
                content: "Build this".into(),
            }],
            budget: CheckpointBudgetSnapshot::unverified(100, None),
        };
        let reference = checkpoint_store.write_candidate(&checkpoint).unwrap();
        let mut events = journal(vec![0, 1]);
        events.extend([
            event(8, AgentEventKind::ModelRequestStarted { step: 2, model: "gpt-example".into() }),
            event(9, response(2, AgentMessage::assistant("It shows a form", vec![]))),
            event(10, AgentEventKind::AssistantMessage { content: "It shows a form".into() }),
            event(11, AgentEventKind::TurnCompleted { response: "It shows a form".into(), steps: 2 }),
        ]);
        let whole = replay(&events).unwrap();
        assert_eq!(whole.history[2].images.len(), 2);
        events.push(event(12, AgentEventKind::CheckpointCommitted { reference }));
        let compacted =
            replay_native("session-1", &events, Some(&checkpoint_store), &compatibility()).unwrap();
        assert_eq!(compacted.history.len(), 2, "the retained prompt and the summary");
        assert!(compacted.history.iter().all(|message| message.images.is_empty()));
        assert!(compacted.committed_messages.is_empty());
    }

    #[test]
    fn checkpoint_retains_inline_user_image_from_original_admission() {
        let dir = tempfile::tempdir().unwrap();
        let checkpoint_store = CheckpointFileStore::new(dir.path()).unwrap();
        let image = crate::UserImage::from_bytes(
            crate::UserImageMime::Png,
            b"\x89PNG\r\n\x1a\ncheckpoint-image",
        )
        .unwrap();
        let checkpoint = Checkpoint {
            session_id: "session-1".into(),
            checkpoint_id: "image-checkpoint".into(),
            covered_event_sequence: 6,
            compatibility: compatibility(),
            summary: "Image was reviewed".into(),
            retained_user_messages: vec![RetainedUserMessage {
                event_sequence: 1,
                content: "Inspect this image".into(),
            }],
            budget: CheckpointBudgetSnapshot::unverified(100, None),
        };
        let reference = checkpoint_store.write_candidate(&checkpoint).unwrap();
        let events = vec![
            event(
                1,
                AgentEventKind::TurnAdmittedWithImages {
                    turn_id: "image-turn".into(),
                    prompt: "Inspect this image".into(),
                    model: "gpt-example".into(),
                    mode: TurnMode::Build,
                    images: vec![image.clone()],
                },
            ),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Inspect this image".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                },
            ),
            event(4, response(1, AgentMessage::assistant("I see it", vec![]))),
            event(
                5,
                AgentEventKind::AssistantMessage {
                    content: "I see it".into(),
                },
            ),
            event(
                6,
                AgentEventKind::TurnCompleted {
                    response: "I see it".into(),
                    steps: 1,
                },
            ),
            event(7, AgentEventKind::CheckpointCommitted { reference }),
        ];
        let replay = replay_native(
            "session-1",
            &events,
            Some(&checkpoint_store),
            &compatibility(),
        )
        .unwrap();
        assert_eq!(replay.history[0].images, vec![image]);
        assert_eq!(replay.history[0].content, "Inspect this image");
        let mut tampered = events;
        if let AgentEventKind::TurnAdmittedWithImages { images, .. } = &mut tampered[0].kind {
            images[0].sha256 = "0".repeat(64);
        }
        assert!(matches!(
            replay_native(
                "session-1",
                &tampered,
                Some(&checkpoint_store),
                &compatibility()
            ),
            Err(ReplayError::InvalidCheckpointMarker(1))
        ));
    }
}
