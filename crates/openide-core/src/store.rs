use crate::{
    replay_native, system_instructions_digest, validate_user_images, AgentEvent, AgentEventKind,
    CheckpointCompatibility, CheckpointFileStore, ReplayError, TurnMode, UserImage, UserImageError,
    SYSTEM_INSTRUCTIONS_VERSION,
};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

#[path = "store_rewind.rs"]
mod rewind;
pub use rewind::{NativeRewind, NativeRewindRefusal};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("invalid session id")]
    InvalidSessionId,
    #[error("session store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("session event encoding failed: {0}")]
    Json(#[source] serde_json::Error),
    #[error("session store lock is poisoned")]
    PoisonedLock,
    #[error("session event belongs to a different session")]
    SessionMismatch,
    #[error("session metadata exceeds the 4 MiB size limit")]
    SessionMetadataTooLarge,
    #[error("native automation origin is invalid: {0}")]
    NativeAutomationOrigin(#[from] crate::NativeAutomationOriginError),
    #[error("native automation occurrence conflicts with its existing session: {0}")]
    ConflictingNativeAutomationOrigin(String),
    #[error("native automation occurrence was explicitly deleted and cannot be recreated: {0}")]
    NativeAutomationOccurrenceDeleted(String),
    #[error("native automation accepts only its original text-only Build admission")]
    NativeAutomationAdmissionBlocked,
    #[error("read-only native automation sessions cannot create subagents")]
    NativeAutomationSubagentsDisabled,
    #[error("read-only native automation sessions do not accept steering inputs")]
    NativeAutomationSteeringDisabled,
    #[error("system instruction snapshot is invalid or exceeds its size limit")]
    InvalidSystemInstructionsSnapshot,
    #[error("model route snapshot has invalid version, IDs, or digests")]
    InvalidModelRouteSnapshot,
    #[error("plan snapshot is invalid or exceeds its size limit")]
    InvalidPlanSnapshot,
    #[error("session event record exceeds the 64 MiB journal line limit")]
    EventRecordTooLarge,
    #[error("user image input is invalid: {0}")]
    InvalidUserImages(#[from] UserImageError),
    #[error("unknown session: {0}")]
    UnknownSession(String),
    #[error("session title cannot be blank")]
    InvalidTitle,
    #[error("session search query is too long")]
    SearchQueryTooLong,
    #[error("session search query could not be compiled: {0}")]
    SearchPattern(#[from] regex::Error),
    #[error("unsupported composer draft format version: {0}")]
    UnsupportedDraftVersion(u32),
    #[error("unsupported permission journal format version: {0}")]
    UnsupportedPermissionJournalVersion(u32),
    #[error("permission journal exceeds the 16 MiB size limit")]
    PermissionJournalTooLarge,
    #[error("invalid permission request id")]
    InvalidPermissionRequestId,
    #[error("invalid session approval grant")]
    InvalidSessionApprovalGrant,
    #[error("user answer exceeds the 64 KiB size limit")]
    InvalidUserAnswer,
    #[error("permission decision conflicts with an existing decision for request {0}")]
    ConflictingPermissionDecision(String),
    #[error("permission journal contains a duplicate request id: {0}")]
    DuplicatePermissionRequestId(String),
    #[error("invalid turn id")]
    InvalidTurnId,
    #[error("native turn admission requires a native session")]
    NativeTurnRequiresNativeSession,
    #[error("native session {0} already has a running worker")]
    NativeSessionBusy(String),
    #[error("native session lease belongs to a different store")]
    NativeLeaseStoreMismatch,
    #[error("subagent depth limit exceeded")]
    SubagentDepthExceeded,
    #[error("invalid subagent spawn id")]
    InvalidSubagentSpawnId,
    #[error("invalid subagent request digest")]
    InvalidSubagentRequestDigest,
    #[error("subagent spawn id conflicts with existing child metadata")]
    ConflictingSubagentIdentity,
    #[error("subagent {0} cannot be deleted directly; delete its top-level native tree")]
    CannotDeleteSubagentDirectly(String),
    #[error("session {parent} still has subagent {child}")]
    SessionHasSubagent { parent: String, child: String },
    #[error("cannot verify session lineage from metadata file {0}")]
    UnverifiableSessionLineage(String),
    #[error("native session {0} is pending coordinated deletion")]
    NativeDeletionPending(String),
    #[error("native deletion intent is invalid: {0}")]
    InvalidNativeDeletionIntent(String),
    #[error("native deletion requires a top-level native session")]
    NativeDeletionRequiresRoot,
    #[error("subagent {child} is not terminal ({status})")]
    NonterminalSubagent { child: String, status: String },
    #[error("subagent {0} has pending follow-up input")]
    PendingSubagentInput(String),
    #[error("subagent registry is invalid: {0}")]
    InvalidSubagentRegistry(String),
    #[error("turn admission conflicts with the saved request for {0}")]
    ConflictingTurnAdmission(String),
    #[error("turn prompt and model must be nonempty")]
    InvalidTurnAdmission,
    #[error("invalid native steering input id")]
    InvalidNativeSteerInputId,
    #[error("native steering input is empty or exceeds 32 KiB")]
    InvalidNativeSteerText,
    #[error("native steering inbox exceeds its size or pending-count limit")]
    NativeSteerInboxTooLarge,
    #[error("unsupported native steering inbox format version: {0}")]
    UnsupportedNativeSteerInboxVersion(u32),
    #[error("native steering inbox is invalid")]
    InvalidNativeSteerInbox,
    #[error("native steering input id conflicts with an existing input: {0}")]
    ConflictingNativeSteerInput(String),
    #[error("native steering requires active turn {0}")]
    NativeSteerTurnMismatch(String),
    #[error("native steering input is not queued: {0}")]
    NativeSteerNotQueued(String),
    #[error("native steering must be acknowledged through its inbox")]
    NativeSteerRequiresAck,
    #[error("native turn has unacknowledged steering input")]
    PendingNativeSteerInput,
    #[error("native command operation admission blocked: {0}")]
    NativeCommandOperationAdmissionBlocked(String),
    #[error("native command output transaction blocked: {0}")]
    NativeCommandOutputBlocked(String),
    #[error(
        "native turn cannot complete before a committed steering input reaches a model request"
    )]
    NativeSteerAwaitingModel,
    #[error("native turn {0} has entered its terminal shutdown fence")]
    NativeTurnFinishing(String),
    #[error("native steering acknowledgement is not at a safe model boundary: {0}")]
    UnsafeNativeSteerBoundary(#[source] ReplayError),
    #[error("native steering checkpoint verification failed: {0}")]
    NativeSteerCheckpoint(#[from] crate::CheckpointError),
    #[error("native event requires an admitted turn")]
    NativeEventWithoutAdmission,
    #[error("native event sequence {incoming} does not follow {previous}")]
    NativeEventOutOfOrder { previous: u64, incoming: u64 },
    #[error("native event conflicts with saved sequence {0}")]
    ConflictingNativeEvent(u64),
    #[error("native rewind refused: {0}")]
    NativeRewindRefused(NativeRewindRefusal),
    #[error("session event log for {session_id} is inconsistent at line {line}: {reason}")]
    InconsistentEventLog {
        session_id: String,
        line: usize,
        reason: &'static str,
    },
}

pub trait EventStore: Send + Sync {
    fn append(&self, event: &AgentEvent) -> Result<(), StoreError>;
    fn load(&self, session_id: &str) -> Result<Vec<AgentEvent>, StoreError>;
}

/// Synchronous journal used by the native worker before it advances to the
/// next model step or tool effect. Assistant deltas are flushed; all other
/// events are synced to disk before this method returns.
pub trait NativeEventJournal: Send + Sync {
    fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError>;
}

#[derive(Default)]
pub struct MemorySessionStore {
    events: Mutex<HashMap<String, Vec<AgentEvent>>>,
}

impl MemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl EventStore for MemorySessionStore {
    fn append(&self, event: &AgentEvent) -> Result<(), StoreError> {
        if !valid_model_route_snapshot(event) {
            return Err(StoreError::InvalidModelRouteSnapshot);
        }
        if !valid_system_instructions_snapshot(event) {
            return Err(StoreError::InvalidSystemInstructionsSnapshot);
        }
        if !valid_plan_snapshot(event) {
            return Err(StoreError::InvalidPlanSnapshot);
        }
        validate_user_image_event(event)?;
        self.events
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?
            .entry(event.session_id.clone())
            .or_default()
            .push(event.clone());
        Ok(())
    }

    fn load(&self, session_id: &str) -> Result<Vec<AgentEvent>, StoreError> {
        Ok(self
            .events
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?
            .get(session_id)
            .cloned()
            .unwrap_or_default())
    }
}

fn valid_model_route_snapshot(event: &AgentEvent) -> bool {
    crate::ModelRouteSnapshot::from_event(&event.kind).is_none_or(|route| route.is_valid())
}

fn valid_system_instructions_snapshot(event: &AgentEvent) -> bool {
    match &event.kind {
        AgentEventKind::SystemInstructionsCommitted {
            version,
            digest,
            content,
        } => {
            *version == SYSTEM_INSTRUCTIONS_VERSION
                && system_instructions_digest(content).as_deref() == Some(digest.as_str())
        }
        _ => true,
    }
}

fn valid_plan_snapshot(event: &AgentEvent) -> bool {
    match &event.kind {
        AgentEventKind::PlanProposed { plan } | AgentEventKind::PlanUpdated { plan, .. } => {
            plan.is_valid()
        }
        _ => true,
    }
}

fn validate_user_image_event(event: &AgentEvent) -> Result<(), StoreError> {
    match &event.kind {
        AgentEventKind::TurnAdmittedWithImages { images, .. } => {
            if images.is_empty() {
                return Err(StoreError::InvalidUserImages(UserImageError::MessageLimit));
            }
            validate_user_images(images)?;
        }
        AgentEventKind::TurnSteeredWithImages { text, images, .. } => {
            if images.is_empty() {
                return Err(StoreError::InvalidUserImages(UserImageError::MessageLimit));
            }
            validate_native_steer_text_and_images(text, images)?;
        }
        AgentEventKind::ModelResponseCommitted { assistant, .. }
            if !assistant.images.is_empty() =>
        {
            return Err(StoreError::InvalidUserImages(UserImageError::MessageLimit));
        }
        _ => {}
    }
    Ok(())
}

const MAX_EVENT_RECORD_BYTES: u64 = 64 * 1024 * 1024;

/// One JSON record per line. The lock protects simultaneous appends in this process.
pub struct JsonlSessionStore {
    root: PathBuf,
    /// A log is checked once before its first append in this process. The
    /// same lock serializes validation, tail repair, loads, and writes.
    write_lock: Mutex<HashSet<String>>,
}

impl JsonlSessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            write_lock: Mutex::new(HashSet::new()),
        })
    }

    fn path(&self, session_id: &str) -> Result<PathBuf, StoreError> {
        validate_session_id(session_id)?;
        Ok(self.root.join(format!("{session_id}.jsonl")))
    }

    /// Return IDs with the most recently modified event log first.
    pub fn list_sessions(&self) -> Result<Vec<String>, StoreError> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if validate_session_id(id).is_err() {
                continue;
            }
            let modified = entry.metadata()?.modified().unwrap_or(UNIX_EPOCH);
            files.push((id.to_owned(), modified));
        }
        files.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        Ok(files.into_iter().map(|(id, _)| id).collect())
    }
}

fn validate_session_id(session_id: &str) -> Result<(), StoreError> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(StoreError::InvalidSessionId);
    }
    Ok(())
}

fn validate_permission_request_id(request_id: &str) -> Result<(), StoreError> {
    if request_id.trim().is_empty()
        || request_id.len() > 256
        || request_id.chars().any(char::is_control)
    {
        return Err(StoreError::InvalidPermissionRequestId);
    }
    Ok(())
}

fn validate_turn_id(turn_id: &str) -> Result<(), StoreError> {
    if turn_id.is_empty()
        || turn_id.len() > 128
        || !turn_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(StoreError::InvalidTurnId);
    }
    Ok(())
}

fn validate_native_steer_input_id(input_id: &str) -> Result<(), StoreError> {
    if input_id.is_empty()
        || input_id.len() > 128
        || !input_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(StoreError::InvalidNativeSteerInputId);
    }
    Ok(())
}

fn validate_native_steer_text_and_images(
    text: &str,
    images: &[UserImage],
) -> Result<(), StoreError> {
    if text.len() > MAX_NATIVE_STEER_TEXT_BYTES || text.trim().is_empty() && images.is_empty() {
        return Err(StoreError::InvalidNativeSteerText);
    }
    validate_user_images(images)?;
    Ok(())
}

fn active_native_turn_id(events: &[AgentEvent]) -> Option<&str> {
    let mut active = None;
    for event in events {
        match &event.kind {
            AgentEventKind::TurnAdmitted { turn_id, .. }
            | AgentEventKind::TurnAdmittedWithImages { turn_id, .. } => {
                active = Some(turn_id.as_str())
            }
            AgentEventKind::TurnCompleted { .. }
            | AgentEventKind::TurnFailed { .. }
            | AgentEventKind::TurnCancelled => active = None,
            _ => {}
        }
    }
    active
}

fn native_admission_fields(
    event: &AgentEvent,
) -> Option<(&str, &str, &str, TurnMode, &[UserImage])> {
    match &event.kind {
        AgentEventKind::TurnAdmitted {
            turn_id,
            prompt,
            model,
            mode,
        } => Some((turn_id, prompt, model, *mode, &[])),
        AgentEventKind::TurnAdmittedWithImages {
            turn_id,
            prompt,
            model,
            mode,
            images,
        } => Some((turn_id, prompt, model, *mode, images)),
        _ => None,
    }
}

fn native_steer_fields(event: &AgentEvent) -> Option<(&str, &str, &str, &[UserImage])> {
    match &event.kind {
        AgentEventKind::TurnSteered {
            turn_id,
            input_id,
            text,
        } => Some((turn_id, input_id, text, &[])),
        AgentEventKind::TurnSteeredWithImages {
            turn_id,
            input_id,
            text,
            images,
        } => Some((turn_id, input_id, text, images)),
        _ => None,
    }
}

fn committed_native_steers(
    events: &[AgentEvent],
) -> Result<HashMap<&str, &AgentEvent>, StoreError> {
    let mut committed = HashMap::new();
    for event in events {
        if let AgentEventKind::TurnSteered {
            turn_id,
            input_id,
            text,
        }
        | AgentEventKind::TurnSteeredWithImages {
            turn_id,
            input_id,
            text,
            ..
        } = &event.kind
        {
            validate_turn_id(turn_id)?;
            validate_native_steer_input_id(input_id)?;
            let images = match &event.kind {
                AgentEventKind::TurnSteeredWithImages { images, .. } => images.as_slice(),
                _ => &[],
            };
            validate_native_steer_text_and_images(text, images)?;
            if committed.insert(input_id.as_str(), event).is_some() {
                return Err(StoreError::ConflictingNativeSteerInput(input_id.clone()));
            }
        }
    }
    Ok(committed)
}

fn native_steer_awaiting_model(events: &[AgentEvent]) -> bool {
    let mut awaiting = false;
    for event in events {
        match &event.kind {
            AgentEventKind::TurnSteered { .. } | AgentEventKind::TurnSteeredWithImages { .. } => {
                awaiting = true
            }
            AgentEventKind::ModelRequestStarted { .. }
            | AgentEventKind::TurnAdmitted { .. }
            | AgentEventKind::TurnAdmittedWithImages { .. }
            | AgentEventKind::TurnCompleted { .. }
            | AgentEventKind::TurnFailed { .. }
            | AgentEventKind::TurnCancelled => awaiting = false,
            _ => {}
        }
    }
    awaiting
}

fn pending_native_steers_from(
    inbox: &NativeSteerInbox,
    committed: &HashMap<&str, &AgentEvent>,
) -> Result<Vec<NativeSteerInput>, StoreError> {
    let mut pending = Vec::new();
    for entry in &inbox.entries {
        if let Some(event) = committed.get(entry.input_id.as_str()) {
            let same = match &event.kind {
                AgentEventKind::TurnSteered {
                    turn_id,
                    input_id,
                    text,
                } => {
                    turn_id == &entry.turn_id
                        && input_id == &entry.input_id
                        && text == &entry.text
                        && entry.images.is_empty()
                }
                AgentEventKind::TurnSteeredWithImages {
                    turn_id,
                    input_id,
                    text,
                    images,
                } => {
                    turn_id == &entry.turn_id
                        && input_id == &entry.input_id
                        && text == &entry.text
                        && images == &entry.images
                }
                _ => false,
            };
            if !same {
                return Err(StoreError::ConflictingNativeSteerInput(
                    entry.input_id.clone(),
                ));
            }
        } else {
            pending.push(entry.clone());
        }
    }
    Ok(pending)
}

fn validate_subagent_spawn_id(spawn_id: &str) -> Result<(), StoreError> {
    if spawn_id.is_empty()
        || spawn_id.len() > 128
        || !spawn_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(StoreError::InvalidSubagentSpawnId);
    }
    Ok(())
}

fn validate_subagent_request_digest(digest: &str) -> Result<(), StoreError> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StoreError::InvalidSubagentRequestDigest);
    }
    Ok(())
}

fn child_session_id(parent_id: &str, spawn_id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"openide-native-child-v1\0");
    hash.update(parent_id.as_bytes());
    hash.update(b"\0");
    hash.update(spawn_id.as_bytes());
    format!("sub-{:x}", hash.finalize())
}

fn validate_native_automation_metadata(session: &Session) -> Result<(), StoreError> {
    match &session.native_automation {
        Some(origin) => origin.validate_session(session).map_err(StoreError::from),
        None if session
            .id
            .starts_with(crate::native_automation_origin::NATIVE_AUTOMATION_SESSION_PREFIX) =>
        {
            Err(crate::NativeAutomationOriginError::SessionBindingMismatch.into())
        }
        None => Ok(()),
    }
}

fn validate_native_automation_admissions(
    session: &Session,
    events: &[AgentEvent],
) -> Result<(), StoreError> {
    let Some(origin) = &session.native_automation else {
        return Ok(());
    };
    let mut count = 0;
    for (index, event) in events.iter().enumerate() {
        if native_steer_fields(event).is_some() {
            return Err(StoreError::NativeAutomationSteeringDisabled);
        }
        if let Some((turn_id, prompt, model, mode, images)) = native_admission_fields(event) {
            count += 1;
            if count > 1
                || index != 0
                || event.sequence != 1
                || turn_id != origin.first_turn_id
                || !images.is_empty()
                || !crate::native_automation_request_digest(prompt, model, mode)
                    .is_ok_and(|digest| digest == origin.first_request_digest)
            {
                return Err(StoreError::NativeAutomationAdmissionBlocked);
            }
        }
    }
    if !events.is_empty() && count != 1 {
        return Err(StoreError::NativeAutomationAdmissionBlocked);
    }
    Ok(())
}

#[cfg(test)]
fn native_automation_metadata_publication_cut(path: &Path, stage: &str) {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.starts_with(crate::native_automation_origin::NATIVE_AUTOMATION_SESSION_PREFIX)
                && name.ends_with(".session.json")
        })
        && std::env::var("OPENIDE_AUTOMATION_METADATA_PUBLICATION_STAGE").as_deref() == Ok(stage)
    {
        // Test subprocess only: release locks by actual process death, without
        // running destructors or promoting an unpublished temporary.
        std::process::exit(87);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub workspace: PathBuf,
    pub backend: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread_at_ms: Option<u64>,
    #[serde(default)]
    pub last_unread_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<SubagentLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_automation: Option<crate::NativeAutomationOrigin>,
}

/// Accommodates a 512 KiB exact authority JSON string even when its metadata
/// representation escapes every byte. Reading never allocates past this cap.
pub const MAX_SESSION_METADATA_BYTES: u64 = 4 * 1024 * 1024;

/// Immutable lineage committed in the same atomic metadata write that creates
/// the child session. Runtime status lives in the separate subagent registry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SubagentLink {
    pub parent_session_id: String,
    pub depth: u8,
    #[serde(default)]
    pub spawn_id: String,
    #[serde(default)]
    pub request_digest: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredSession {
    pub session: Session,
    pub events: Vec<AgentEvent>,
}

/// A session that could not be read while listing the rest of the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDiagnostic {
    pub session_id: String,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionListing {
    pub sessions: Vec<StoredSession>,
    pub diagnostics: Vec<SessionDiagnostic>,
}

/// The same mutually exclusive list scopes used by the sessions dock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionListKind {
    /// Active tasks except those in the global pinned section.
    #[default]
    ActiveUnpinned,
    Pinned,
    Archived,
    All,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionSort {
    #[default]
    Updated,
    Created,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionQuery {
    /// Empty means every workspace. Paths are compared exactly as persisted.
    pub workspaces: Vec<PathBuf>,
    pub kind: SessionListKind,
    pub sort: SessionSort,
    /// A literal, case-insensitive substring in the title or visible chat text.
    pub search: Option<String>,
    pub offset: usize,
    pub limit: usize,
}

impl Default for SessionQuery {
    fn default() -> Self {
        Self {
            workspaces: Vec::new(),
            kind: SessionListKind::default(),
            sort: SessionSort::default(),
            search: None,
            offset: 0,
            limit: 20,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionQueryItem {
    pub session: Session,
    pub title: String,
    pub search_snippets: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionQueryResult {
    pub items: Vec<SessionQueryItem>,
    /// Number of readable matches before pagination.
    pub total: usize,
    pub has_more: bool,
    pub diagnostics: Vec<SessionDiagnostic>,
}

/// The user's choice, recorded before asking the runner to resolve a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedPermissionDecision {
    pub session_id: String,
    pub request_id: String,
    pub decision: PermissionDecision,
    pub recorded_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<SessionApprovalGrantRecord>,
    /// Revocation affects later tool checks without rewriting the original
    /// approval choice. Older journals omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_ms: Option<u64>,
    /// What the user answered when the request was a question rather than a
    /// permission. User-authored content; older journals omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

/// Bound of one recorded answer, well above what the question card produces.
pub const MAX_USER_ANSWER_BYTES: usize = 64 * 1024;

/// Durable session-scoped approval for one exact call shape and effective
/// policy. Raw tool arguments and paths are intentionally excluded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionApprovalGrantRecord {
    pub version: u16,
    pub session_id: String,
    pub tool_name: String,
    pub effective_policy_digest: String,
    pub key_digest: String,
}

const SESSION_APPROVAL_GRANT_RECORD_VERSION: u16 = 1;
const MAX_PERMISSION_JOURNAL_BYTES: u64 = 16 * 1024 * 1024;

fn validate_session_approval_grant(
    grant: &SessionApprovalGrantRecord,
    session_id: &str,
    decision: PermissionDecision,
) -> Result<(), StoreError> {
    let valid_digest = |value: &str| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if decision != PermissionDecision::Allow
        || grant.version != SESSION_APPROVAL_GRANT_RECORD_VERSION
        || grant.session_id != session_id
        || grant.session_id.len() > 128
        || grant.tool_name.is_empty()
        || grant.tool_name.len() > 256
        || grant.tool_name.trim() != grant.tool_name
        || grant.tool_name.chars().any(char::is_control)
        || !valid_digest(&grant.effective_policy_digest)
        || !valid_digest(&grant.key_digest)
    {
        return Err(StoreError::InvalidSessionApprovalGrant);
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct PermissionJournal {
    version: u32,
    entries: Vec<RecordedPermissionDecision>,
}

impl Default for PermissionJournal {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct DraftFile {
    version: u32,
    entries: Vec<DraftEntry>,
}

impl Default for DraftFile {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct DraftEntry {
    workspace: PathBuf,
    session_id: Option<String>,
    text: String,
    updated_at_ms: u64,
}

const NATIVE_DELETE_INTENT_VERSION: u32 = 1;
const MAX_NATIVE_DELETE_INTENT_BYTES: u64 = 1024 * 1024;
const MAX_SUBAGENT_REGISTRY_BYTES: u64 = 2 * 1024 * 1024;
const MAX_NATIVE_STEER_TEXT_BYTES: usize = 32 * 1024;
const MAX_NATIVE_STEER_INBOX_BYTES: u64 = 40 * 1024 * 1024;
const MAX_PENDING_NATIVE_STEERS: usize = 32;

/// Immutable user input. Re-reading pending items never acknowledges them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSteerInput {
    pub turn_id: String,
    pub input_id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<UserImage>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeSteerInbox {
    version: u32,
    session_id: String,
    entries: Vec<NativeSteerInput>,
    #[serde(default)]
    finishing_turn_id: Option<String>,
}

impl NativeSteerInbox {
    fn empty(session_id: &str) -> Self {
        Self {
            version: 1,
            session_id: session_id.to_owned(),
            entries: Vec::new(),
            finishing_turn_id: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeDeleteIntent {
    version: u32,
    root_id: String,
    /// Parent-before-child order. Cleanup processes this in reverse.
    nodes: Vec<NativeDeleteNode>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeDeleteNode {
    id: String,
    parent_id: Option<String>,
    spawn_id: Option<String>,
}

/// Session metadata plus a per-session append-only event log.
pub struct SessionStore {
    events: JsonlSessionStore,
    operation_lock: Mutex<()>,
    native_tail: Mutex<HashMap<String, NativeJournalTail>>,
}

/// The length changes on every append; the lockfile generation changes before
/// deletion. Together they let a handle reject a stale cached tail, including
/// after a deterministic child session ID is recreated.
struct NativeJournalTail {
    event: AgentEvent,
    journal_len: u64,
    journal_generation: u64,
}

/// Closing the stable lock file releases this transaction across processes.
/// Readers take it too because loading may repair a torn final JSONL record.
struct NativeJournalTransaction(File);

impl NativeJournalTransaction {
    fn generation(&self) -> Result<u64, StoreError> {
        Ok(self.0.metadata()?.len())
    }

    /// Advance the durable generation before removing a journal. A recreated
    /// deterministic child ID cannot reuse a stale tail even if its new log
    /// happens to have the same byte length as the old one.
    fn bump_generation(&mut self) -> Result<(), StoreError> {
        use std::io::{Seek, SeekFrom};
        self.0.seek(SeekFrom::End(0))?;
        self.0.write_all(b"x")?;
        self.0.sync_all()?;
        Ok(())
    }
}

impl Drop for NativeJournalTransaction {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Exclusive OS lease for one native session. Keep this guard alive for the
/// whole worker run. Closing its file releases the lock, including when the
/// process exits without running destructors. The lock file itself is stable
/// and intentionally remains in place after release.
#[derive(Debug)]
pub struct NativeSessionLease {
    session_id: String,
    store_root: PathBuf,
    child_creation_lock: Mutex<()>,
    file: File,
}

impl NativeSessionLease {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

impl Drop for NativeSessionLease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl SessionStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Ok(Self {
            events: JsonlSessionStore::new(root)?,
            operation_lock: Mutex::new(()),
            native_tail: Mutex::new(HashMap::new()),
        })
    }

    pub fn storage_root(&self) -> &Path {
        &self.events.root
    }

    /// Verify a lease was acquired from this store and its session remains
    /// native. Subagent operations use this before inspecting lineage.
    pub fn native_session_for_lease(
        &self,
        lease: &NativeSessionLease,
    ) -> Result<Session, StoreError> {
        if lease.store_root != fs::canonicalize(&self.events.root)? {
            return Err(StoreError::NativeLeaseStoreMismatch);
        }
        self.ensure_not_pending_native_deletion(lease.session_id())?;
        let session = self
            .read_metadata(lease.session_id())?
            .ok_or_else(|| StoreError::UnknownSession(lease.session_id().to_owned()))?;
        if session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession);
        }
        Ok(session)
    }

    /// Intention-only command inbox writes share admissions/deletion's
    /// transaction order. They do not acquire the worker's native lease.
    pub(crate) fn with_native_command_operation_admission<T>(
        &self,
        session_id: &str,
        operation: impl FnOnce(&Session) -> Result<T, crate::NativeCommandOperationError>,
    ) -> Result<T, crate::NativeCommandOperationError> {
        validate_session_id(session_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _journal_transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.into()))?;
        if session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession.into());
        }
        operation(&session)
    }

    /// Captures use the same admission/deletion journal transaction, but no
    /// worker lease or empty human-command inbox is required. A live capture
    /// can finish after archiving; registration checks archive under this lock.
    pub(crate) fn with_native_command_output_transaction<T>(
        &self,
        session_id: &str,
        operation: impl FnOnce(&Session) -> Result<T, crate::NativeCommandOutputError>,
    ) -> Result<T, crate::NativeCommandOutputError> {
        validate_session_id(session_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _journal_transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.into()))?;
        if session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession.into());
        }
        operation(&session)
    }

    /// Acquire a nonblocking, process-wide lease before starting a native
    /// worker. Separate store handles and processes contend on the same OS
    /// lock. A busy session is never treated as a stale lock file.
    pub fn try_acquire_native_lease(
        &self,
        session_id: &str,
    ) -> Result<NativeSessionLease, StoreError> {
        validate_session_id(session_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        self.try_acquire_native_lease_locked(session_id)
    }

    /// Caller holds `operation_lock`; the OS lock also coordinates separate
    /// store handles and processes.
    fn try_acquire_native_lease_locked(
        &self,
        session_id: &str,
    ) -> Result<NativeSessionLease, StoreError> {
        self.try_acquire_native_lease_for_deletion(session_id, false)
    }

    fn try_acquire_native_lease_for_deletion(
        &self,
        session_id: &str,
        allow_pending_deletion: bool,
    ) -> Result<NativeSessionLease, StoreError> {
        if !allow_pending_deletion {
            self.ensure_not_pending_native_deletion(session_id)?;
        }
        let session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        if session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession);
        }
        let lease_dir = self.events.root.join(".native-leases");
        fs::create_dir_all(&lease_dir)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lease_dir.join(format!("{session_id}.lock")))?;
        match file.try_lock() {
            Ok(()) => {
                // A concurrent deleter can remove metadata between the
                // initial read and this lock acquisition. Recheck while the
                // OS lease is ours before admitting a worker.
                if !allow_pending_deletion {
                    self.ensure_not_pending_native_deletion(session_id)?;
                }
                let current = self
                    .read_metadata(session_id)?
                    .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
                if current.backend != "native" {
                    return Err(StoreError::NativeTurnRequiresNativeSession);
                }
                Ok(NativeSessionLease {
                    session_id: session_id.to_owned(),
                    store_root: fs::canonicalize(&self.events.root)?,
                    child_creation_lock: Mutex::new(()),
                    file,
                })
            }
            Err(TryLockError::WouldBlock) => {
                Err(StoreError::NativeSessionBusy(session_id.to_owned()))
            }
            Err(TryLockError::Error(error)) => Err(StoreError::Io(error)),
        }
    }

    fn native_delete_intent_path(&self, root_id: &str) -> Result<PathBuf, StoreError> {
        validate_session_id(root_id)?;
        Ok(self
            .native_delete_intent_directory()
            .join(format!("{root_id}.json")))
    }

    fn native_delete_intent_directory(&self) -> PathBuf {
        self.events.root.join(".native-delete-intents")
    }

    fn verified_native_delete_intent_directory(&self) -> Result<Option<PathBuf>, StoreError> {
        let path = self.native_delete_intent_directory();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_dir() => Ok(Some(path)),
            Ok(_) => Err(StoreError::InvalidNativeDeletionIntent(
                path.display().to_string(),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn read_native_delete_intent(
        &self,
        root_id: &str,
    ) -> Result<Option<NativeDeleteIntent>, StoreError> {
        if self.verified_native_delete_intent_directory()?.is_none() {
            return Ok(None);
        }
        let path = self.native_delete_intent_path(root_id)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.file_type().is_file() => {
                return Err(StoreError::InvalidNativeDeletionIntent(root_id.to_owned()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_NATIVE_DELETE_INTENT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_NATIVE_DELETE_INTENT_BYTES {
            return Err(StoreError::InvalidNativeDeletionIntent(root_id.to_owned()));
        }
        let intent: NativeDeleteIntent = serde_json::from_slice(&bytes)
            .map_err(|_| StoreError::InvalidNativeDeletionIntent(root_id.to_owned()))?;
        self.validate_native_delete_intent(&intent, root_id)?;
        Ok(Some(intent))
    }

    fn validate_native_delete_intent(
        &self,
        intent: &NativeDeleteIntent,
        root_id: &str,
    ) -> Result<(), StoreError> {
        let invalid = || StoreError::InvalidNativeDeletionIntent(root_id.to_owned());
        if intent.version != NATIVE_DELETE_INTENT_VERSION
            || intent.root_id != root_id
            || intent.nodes.first().is_none_or(|node| {
                node.id != root_id || node.parent_id.is_some() || node.spawn_id.is_some()
            })
        {
            return Err(invalid());
        }
        let mut seen = HashSet::new();
        for node in &intent.nodes {
            validate_session_id(&node.id).map_err(|_| invalid())?;
            if !seen.insert(node.id.as_str()) {
                return Err(invalid());
            }
            if let Some(parent_id) = &node.parent_id {
                validate_session_id(parent_id).map_err(|_| invalid())?;
                let spawn_id = node.spawn_id.as_deref().ok_or_else(invalid)?;
                validate_subagent_spawn_id(spawn_id).map_err(|_| invalid())?;
                if !seen.contains(parent_id.as_str())
                    || child_session_id(parent_id, spawn_id) != node.id
                {
                    return Err(invalid());
                }
            } else if node.id != root_id || node.spawn_id.is_some() {
                return Err(invalid());
            }
        }
        Ok(())
    }

    fn native_delete_intents(&self) -> Result<Vec<NativeDeleteIntent>, StoreError> {
        let mut intents = Vec::new();
        let Some(directory) = self.verified_native_delete_intent_directory()? else {
            return Ok(intents);
        };
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                if name.to_string_lossy().ends_with(".json") {
                    return Err(StoreError::InvalidNativeDeletionIntent(
                        name.to_string_lossy().into_owned(),
                    ));
                }
                continue;
            };
            let Some(root_id) = name.strip_suffix(".json") else {
                continue;
            };
            validate_session_id(root_id)
                .map_err(|_| StoreError::InvalidNativeDeletionIntent(name.to_owned()))?;
            if !entry.file_type()?.is_file() {
                return Err(StoreError::InvalidNativeDeletionIntent(name.to_owned()));
            }
            let intent = self
                .read_native_delete_intent(root_id)?
                .ok_or_else(|| StoreError::InvalidNativeDeletionIntent(name.to_owned()))?;
            intents.push(intent);
        }
        Ok(intents)
    }

    fn ensure_not_pending_native_deletion(&self, session_id: &str) -> Result<(), StoreError> {
        for intent in self.native_delete_intents()? {
            if intent.nodes.iter().any(|node| node.id == session_id) {
                return Err(StoreError::NativeDeletionPending(session_id.to_owned()));
            }
        }
        Ok(())
    }

    /// With the parent OS lease held, no conforming native worker can add a
    /// child while this scan and the subsequent deletion run.
    fn ensure_no_direct_children_locked(&self, parent_id: &str) -> Result<(), StoreError> {
        for entry in fs::read_dir(&self.events.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                let lossy = name.to_string_lossy();
                if lossy.ends_with(".session.json") {
                    return Err(StoreError::UnverifiableSessionLineage(lossy.into_owned()));
                }
                continue;
            };
            let Some(id) = name.strip_suffix(".session.json") else {
                continue;
            };
            if validate_session_id(id).is_err() || !entry.file_type()?.is_file() {
                return Err(StoreError::UnverifiableSessionLineage(name.to_owned()));
            }
            let Some(session) = self.read_metadata(id)? else {
                return Err(StoreError::UnverifiableSessionLineage(name.to_owned()));
            };
            if session
                .subagent
                .as_ref()
                .is_some_and(|link| link.parent_session_id == parent_id)
            {
                return Err(StoreError::SessionHasSubagent {
                    parent: parent_id.to_owned(),
                    child: id.to_owned(),
                });
            }
        }
        Ok(())
    }

    fn scan_session_metadata(&self) -> Result<Vec<Session>, StoreError> {
        let mut sessions = Vec::new();
        for entry in fs::read_dir(&self.events.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                if name.to_string_lossy().ends_with(".session.json") {
                    return Err(StoreError::UnverifiableSessionLineage(
                        name.to_string_lossy().into_owned(),
                    ));
                }
                continue;
            };
            let Some(id) = name.strip_suffix(".session.json") else {
                continue;
            };
            if validate_session_id(id).is_err() || !entry.file_type()?.is_file() {
                return Err(StoreError::UnverifiableSessionLineage(name.to_owned()));
            }
            let session = self
                .read_metadata(id)?
                .ok_or_else(|| StoreError::UnverifiableSessionLineage(name.to_owned()))?;
            sessions.push(session);
        }
        Ok(sessions)
    }

    fn validate_child_lineage(&self, child: &Session, parent: &Session) -> Result<(), StoreError> {
        let Some(link) = child.subagent.as_ref() else {
            return Err(StoreError::UnverifiableSessionLineage(child.id.clone()));
        };
        let expected_depth = parent
            .subagent
            .as_ref()
            .map_or(Some(1), |link| link.depth.checked_add(1));
        if child.backend != "native"
            || parent.backend != "native"
            || child.workspace != parent.workspace
            || link.parent_session_id != parent.id
            || expected_depth != Some(link.depth)
            || child_session_id(&parent.id, &link.spawn_id) != child.id
        {
            return Err(StoreError::UnverifiableSessionLineage(child.id.clone()));
        }
        Ok(())
    }

    fn discover_native_tree(
        &self,
        root: Session,
        leases: &mut Vec<NativeSessionLease>,
    ) -> Result<NativeDeleteIntent, StoreError> {
        let root_id = root.id.clone();
        let mut nodes = vec![NativeDeleteNode {
            id: root_id.clone(),
            parent_id: None,
            spawn_id: None,
        }];
        let mut visited = HashSet::from([root_id.clone()]);
        let mut queue = VecDeque::from([root]);
        while let Some(parent) = queue.pop_front() {
            // The parent's exclusive lease is already held. Thus no worker
            // can add a direct child after this scan, even across processes.
            let mut children = self
                .scan_session_metadata()?
                .into_iter()
                .filter(|session| {
                    session
                        .subagent
                        .as_ref()
                        .is_some_and(|link| link.parent_session_id == parent.id)
                })
                .collect::<Vec<_>>();
            children.sort_by(|a, b| a.id.cmp(&b.id));
            for child in children {
                self.validate_child_lineage(&child, &parent)?;
                if !visited.insert(child.id.clone()) {
                    return Err(StoreError::UnverifiableSessionLineage(child.id));
                }
                leases.push(self.try_acquire_native_lease_for_deletion(&child.id, false)?);
                nodes.push(NativeDeleteNode {
                    id: child.id.clone(),
                    parent_id: Some(parent.id.clone()),
                    spawn_id: child.subagent.as_ref().map(|link| link.spawn_id.clone()),
                });
                queue.push_back(child);
            }
        }
        Ok(NativeDeleteIntent {
            version: NATIVE_DELETE_INTENT_VERSION,
            root_id,
            nodes,
        })
    }

    fn validate_recovery_tree(&self, intent: &NativeDeleteIntent) -> Result<(), StoreError> {
        let members = intent
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node.parent_id.as_deref()))
            .collect::<HashMap<_, _>>();
        let sessions = self.scan_session_metadata()?;
        let by_id = sessions
            .iter()
            .map(|session| (session.id.as_str(), session))
            .collect::<HashMap<_, _>>();
        for session in &sessions {
            if let Some(link) = &session.subagent {
                if members.contains_key(link.parent_session_id.as_str())
                    && !members.contains_key(session.id.as_str())
                {
                    return Err(StoreError::SessionHasSubagent {
                        parent: link.parent_session_id.clone(),
                        child: session.id.clone(),
                    });
                }
            }
        }
        for node in &intent.nodes {
            let Some(session) = by_id.get(node.id.as_str()) else {
                continue;
            };
            if session.backend != "native"
                || session
                    .subagent
                    .as_ref()
                    .map(|link| link.parent_session_id.as_str())
                    != node.parent_id.as_deref()
                || session.subagent.as_ref().map(|link| link.spawn_id.as_str())
                    != node.spawn_id.as_deref()
            {
                return Err(StoreError::InvalidNativeDeletionIntent(
                    intent.root_id.clone(),
                ));
            }
            if let Some(parent_id) = &node.parent_id {
                if let Some(parent) = by_id.get(parent_id.as_str()) {
                    self.validate_child_lineage(session, parent)?;
                }
            }
        }
        Ok(())
    }

    fn lock_subagent_registry(&self) -> Result<File, StoreError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.events.root.join(".subagents.registry.lock"))?;
        file.lock()?;
        Ok(file)
    }

    /// Serialize the read/validate/append transaction for one journal across
    /// store handles and processes. Keep this file stable after a session is
    /// deleted; unlinking an active lock file would split the lock domain.
    fn lock_native_journal(
        &self,
        session_id: &str,
    ) -> Result<NativeJournalTransaction, StoreError> {
        validate_session_id(session_id)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(
                self.events
                    .root
                    .join(format!(".native-journal.{session_id}.lock")),
            )?;
        file.lock()?;
        Ok(NativeJournalTransaction(file))
    }

    fn native_journal_len(&self, session_id: &str) -> Result<u64, StoreError> {
        match fs::metadata(self.events.path(session_id)?) {
            Ok(metadata) => Ok(metadata.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error.into()),
        }
    }

    fn lock_native_tree_deletion(&self, root_id: &str) -> Result<File, StoreError> {
        validate_session_id(root_id)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(
                self.events
                    .root
                    .join(format!(".native-delete.{root_id}.lock")),
            )?;
        file.lock()?;
        Ok(file)
    }

    fn read_subagent_registry(&self) -> Result<Option<serde_json::Value>, StoreError> {
        let path = self.events.root.join("subagents.registry.json");
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_SUBAGENT_REGISTRY_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SUBAGENT_REGISTRY_BYTES {
            return Err(StoreError::InvalidSubagentRegistry("oversized".into()));
        }
        let registry: serde_json::Value = serde_json::from_slice(&bytes)?;
        let object = registry
            .as_object()
            .ok_or_else(|| StoreError::InvalidSubagentRegistry("root".into()))?;
        if !matches!(
            object.get("version").and_then(|value| value.as_u64()),
            Some(1..=3)
        ) || object
            .get("children")
            .and_then(|value| value.as_object())
            .is_none()
            || object.get("inputs").is_some_and(|value| !value.is_object())
        {
            return Err(StoreError::InvalidSubagentRegistry("schema".into()));
        }
        Ok(Some(registry))
    }

    fn ensure_tree_terminal(
        &self,
        intent: &NativeDeleteIntent,
        registry: Option<&serde_json::Value>,
    ) -> Result<(), StoreError> {
        let children = registry
            .and_then(|value| value.get("children"))
            .and_then(serde_json::Value::as_object);
        let inputs = registry
            .and_then(|value| value.get("inputs"))
            .and_then(serde_json::Value::as_object);
        for node in intent.nodes.iter().skip(1) {
            let status = children
                .and_then(|children| children.get(&node.id))
                .and_then(|state| state.get("status"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("ready");
            if !matches!(status, "completed" | "failed" | "cancelled") {
                return Err(StoreError::NonterminalSubagent {
                    child: node.id.clone(),
                    status: status.to_owned(),
                });
            }
            if let Some(queued) = inputs.and_then(|inputs| inputs.get(&node.id)) {
                let queued = queued
                    .as_array()
                    .ok_or_else(|| StoreError::InvalidSubagentRegistry(node.id.clone()))?;
                if queued.iter().any(|input| {
                    !matches!(
                        input.get("state").and_then(serde_json::Value::as_str),
                        Some("done" | "skipped")
                    )
                }) {
                    return Err(StoreError::PendingSubagentInput(node.id.clone()));
                }
            }
        }
        Ok(())
    }

    fn metadata_path(&self, id: &str) -> Result<PathBuf, StoreError> {
        validate_session_id(id)?;
        Ok(self.events.root.join(format!("{id}.session.json")))
    }

    fn draft_path(&self) -> PathBuf {
        self.events.root.join("composer-drafts.json")
    }

    fn permission_journal_path(&self, session_id: &str) -> Result<PathBuf, StoreError> {
        validate_session_id(session_id)?;
        Ok(self
            .events
            .root
            .join(format!("{session_id}.permissions.json")))
    }

    fn native_steer_inbox_path(&self, session_id: &str) -> Result<PathBuf, StoreError> {
        validate_session_id(session_id)?;
        Ok(self
            .events
            .root
            .join(format!("{session_id}.native-steer.json")))
    }

    /// The caller holds the per-session native journal lock. The inbox shares
    /// that lock with admission, worker events, and deletion.
    fn read_native_steer_inbox(&self, session_id: &str) -> Result<NativeSteerInbox, StoreError> {
        let file = match File::open(self.native_steer_inbox_path(session_id)?) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(NativeSteerInbox::empty(session_id));
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_NATIVE_STEER_INBOX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_NATIVE_STEER_INBOX_BYTES {
            return Err(StoreError::NativeSteerInboxTooLarge);
        }
        let inbox: NativeSteerInbox = serde_json::from_slice(&bytes)?;
        if inbox.version != 1 {
            return Err(StoreError::UnsupportedNativeSteerInboxVersion(
                inbox.version,
            ));
        }
        if inbox.session_id != session_id || inbox.entries.len() > MAX_PENDING_NATIVE_STEERS {
            return Err(StoreError::InvalidNativeSteerInbox);
        }
        if let Some(turn_id) = inbox.finishing_turn_id.as_deref() {
            validate_turn_id(turn_id)?;
        }
        let mut seen = HashSet::new();
        for entry in &inbox.entries {
            validate_turn_id(&entry.turn_id)?;
            validate_native_steer_input_id(&entry.input_id)?;
            validate_native_steer_text_and_images(&entry.text, &entry.images)?;
            if !seen.insert(entry.input_id.as_str()) {
                return Err(StoreError::ConflictingNativeSteerInput(
                    entry.input_id.clone(),
                ));
            }
        }
        Ok(inbox)
    }

    fn write_native_steer_inbox(
        &self,
        session_id: &str,
        inbox: &NativeSteerInbox,
    ) -> Result<(), StoreError> {
        if inbox.entries.len() > MAX_PENDING_NATIVE_STEERS
            || serde_json::to_vec(inbox)?.len() as u64 + 1 > MAX_NATIVE_STEER_INBOX_BYTES
        {
            return Err(StoreError::NativeSteerInboxTooLarge);
        }
        self.write_json_atomically(
            self.native_steer_inbox_path(session_id)?,
            &format!("{session_id}.native-steer"),
            inbox,
        )
    }

    // Caller holds the stable native journal transaction. Even a foreign or
    // corrupt inbox must not silently enlarge this original request's scope.
    fn require_no_native_automation_steers(&self, session: &Session) -> Result<(), StoreError> {
        if let Some(origin) = &session.native_automation {
            let inbox = self.read_native_steer_inbox(&session.id)?;
            if !inbox.entries.is_empty()
                || inbox
                    .finishing_turn_id
                    .as_ref()
                    .is_some_and(|turn_id| turn_id != &origin.first_turn_id)
            {
                return Err(StoreError::NativeAutomationSteeringDisabled);
            }
        }
        Ok(())
    }

    fn read_permission_journal(&self, session_id: &str) -> Result<PermissionJournal, StoreError> {
        let path = self.permission_journal_path(session_id)?;
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PermissionJournal::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_PERMISSION_JOURNAL_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_PERMISSION_JOURNAL_BYTES {
            return Err(StoreError::PermissionJournalTooLarge);
        }
        let journal: PermissionJournal = serde_json::from_slice(&bytes)?;
        if journal.version != 1 && journal.version != 2 {
            return Err(StoreError::UnsupportedPermissionJournalVersion(
                journal.version,
            ));
        }
        let mut seen = HashSet::new();
        for entry in &journal.entries {
            if entry.session_id != session_id {
                return Err(StoreError::SessionMismatch);
            }
            validate_permission_request_id(&entry.request_id)?;
            if let Some(grant) = &entry.grant {
                validate_session_approval_grant(grant, session_id, entry.decision)?;
            }
            if let Some(revoked_at) = entry.revoked_at_ms {
                if journal.version == 1
                    || entry.grant.is_none()
                    || revoked_at < entry.recorded_at_ms
                {
                    return Err(StoreError::InvalidSessionApprovalGrant);
                }
            }
            if !seen.insert(&entry.request_id) {
                return Err(StoreError::DuplicatePermissionRequestId(
                    entry.request_id.clone(),
                ));
            }
        }
        Ok(journal)
    }

    fn write_permission_journal_atomically(
        &self,
        session_id: &str,
        journal: &PermissionJournal,
    ) -> Result<(), StoreError> {
        if serde_json::to_vec(journal)?.len() as u64 + 1 > MAX_PERMISSION_JOURNAL_BYTES {
            return Err(StoreError::PermissionJournalTooLarge);
        }
        self.write_json_atomically(
            self.permission_journal_path(session_id)?,
            &format!("{session_id}.permissions"),
            journal,
        )
    }

    fn read_drafts(&self) -> Result<DraftFile, StoreError> {
        let file = match File::open(self.draft_path()) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DraftFile::default());
            }
            Err(error) => return Err(error.into()),
        };
        let drafts: DraftFile = serde_json::from_reader(file)?;
        if drafts.version != 1 {
            return Err(StoreError::UnsupportedDraftVersion(drafts.version));
        }
        Ok(drafts)
    }

    fn write_drafts_atomically(&self, drafts: &DraftFile) -> Result<(), StoreError> {
        self.write_json_atomically(self.draft_path(), "composer-drafts", drafts)
    }

    fn write_json_atomically<T: Serialize>(
        &self,
        path: PathBuf,
        temporary_stem: &str,
        value: &T,
    ) -> Result<(), StoreError> {
        let temporary = self
            .events
            .root
            .join(format!(".{temporary_stem}.{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<(), StoreError> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            serde_json::to_writer(&mut file, value)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            #[cfg(test)]
            native_automation_metadata_publication_cut(&path, "temporary-synced");
            fs::rename(&temporary, &path)?;
            #[cfg(test)]
            native_automation_metadata_publication_cut(&path, "renamed");
            OpenOptions::new().write(true).open(&path)?.sync_all()?;
            #[cfg(unix)]
            File::open(path.parent().unwrap_or(&self.events.root))?.sync_all()?;
            #[cfg(test)]
            native_automation_metadata_publication_cut(&path, "directory-synced");
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn read_metadata(&self, id: &str) -> Result<Option<Session>, StoreError> {
        let path = self.metadata_path(id)?;
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_SESSION_METADATA_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SESSION_METADATA_BYTES {
            return Err(StoreError::SessionMetadataTooLarge);
        }
        let session: Session = serde_json::from_slice(&bytes)?;
        if session.id != id {
            return Err(StoreError::SessionMismatch);
        }
        validate_native_automation_metadata(&session)?;
        Ok(Some(session))
    }

    /// Replace a metadata file only after a complete, synced copy exists beside it.
    fn write_metadata_atomically(&self, session: &Session) -> Result<(), StoreError> {
        validate_native_automation_metadata(session)?;
        if serde_json::to_vec(session)?.len() as u64 + 1 > MAX_SESSION_METADATA_BYTES {
            return Err(StoreError::SessionMetadataTooLarge);
        }
        self.write_json_atomically(self.metadata_path(&session.id)?, &session.id, session)
    }

    fn update_metadata(
        &self,
        id: &str,
        change: impl FnOnce(&mut Session),
    ) -> Result<Session, StoreError> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        self.ensure_not_pending_native_deletion(id)?;
        let mut session = self
            .read_metadata(id)?
            .ok_or_else(|| StoreError::UnknownSession(id.to_owned()))?;
        change(&mut session);
        session.updated_at_ms = current_time_ms().max(session.updated_at_ms.saturating_add(1));
        self.write_metadata_atomically(&session)?;
        Ok(session)
    }

    pub fn create(
        &self,
        workspace: impl Into<PathBuf>,
        backend: impl Into<String>,
    ) -> Result<Session, StoreError> {
        let now = current_time_ms();
        let session = Session {
            id: Uuid::new_v4().to_string(),
            workspace: workspace.into(),
            backend: backend.into(),
            created_at_ms: now,
            updated_at_ms: now,
            title: None,
            pinned: false,
            archived: false,
            unread_at_ms: None,
            last_unread_at_ms: 0,
            subagent: None,
            native_automation: None,
        };
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        self.write_metadata_atomically(&session)?;
        Ok(session)
    }

    /// Create the occurrence's native session and its immutable read-only
    /// origin in one synced metadata publication. Exact retries return the
    /// same session; neither the scheduler ledger nor current settings are
    /// consulted. Creation and deletion share the stable journal OS lock.
    pub fn create_native_automation(
        &self,
        workspace: impl Into<PathBuf>,
        origin: crate::NativeAutomationOrigin,
    ) -> Result<Session, StoreError> {
        origin.validate()?;
        let workspace = fs::canonicalize(workspace.into())?;
        if !workspace.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "native automation workspace is not a directory",
            )
            .into());
        }
        let session_id = crate::native_automation_session_id(&workspace, &origin.run_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let transaction = self.lock_native_journal(&session_id)?;
        self.ensure_not_pending_native_deletion(&session_id)?;
        if let Some(existing) = self.read_metadata(&session_id)? {
            if existing.workspace != workspace
                || existing.native_automation.as_ref() != Some(&origin)
            {
                return Err(StoreError::ConflictingNativeAutomationOrigin(origin.run_id));
            }
            return Ok(existing);
        }
        if transaction.generation()? != 0 {
            return Err(StoreError::NativeAutomationOccurrenceDeleted(origin.run_id));
        }
        if self.native_journal_len(&session_id)? != 0 {
            return Err(StoreError::ConflictingNativeAutomationOrigin(origin.run_id));
        }
        let now = current_time_ms();
        let session = Session {
            id: session_id,
            workspace,
            backend: "native".into(),
            created_at_ms: now,
            updated_at_ms: now,
            title: None,
            pinned: false,
            archived: false,
            unread_at_ms: None,
            last_unread_at_ms: 0,
            subagent: None,
            native_automation: Some(origin),
        };
        self.write_metadata_atomically(&session)?;
        Ok(session)
    }

    /// Create a native child while the caller holds the parent session's OS
    /// lease. Lineage and the initial session are one atomic metadata write.
    /// `spawn_id` is stable for one parent request so a crash/retry returns
    /// the original child instead of creating another session.
    pub fn create_native_child(
        &self,
        parent_lease: &NativeSessionLease,
        spawn_id: &str,
        request_digest: &str,
        max_depth: u8,
    ) -> Result<Session, StoreError> {
        validate_subagent_spawn_id(spawn_id)?;
        validate_subagent_request_digest(request_digest)?;
        let _parent_guard = parent_lease
            .child_creation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let parent = self.native_session_for_lease(parent_lease)?;
        if parent.native_automation.is_some() {
            return Err(StoreError::NativeAutomationSubagentsDisabled);
        }
        if let Some(existing) =
            self.native_child_for_spawn(parent_lease, spawn_id, request_digest)?
        {
            return Ok(existing);
        }
        let depth = parent
            .subagent
            .as_ref()
            .map_or(Some(1), |link| link.depth.checked_add(1))
            .ok_or(StoreError::SubagentDepthExceeded)?;
        if depth > max_depth {
            return Err(StoreError::SubagentDepthExceeded);
        }
        let now = current_time_ms();
        let child = Session {
            id: child_session_id(&parent.id, spawn_id),
            workspace: parent.workspace,
            backend: "native".to_owned(),
            created_at_ms: now,
            updated_at_ms: now,
            title: None,
            pinned: false,
            archived: false,
            unread_at_ms: None,
            last_unread_at_ms: 0,
            subagent: Some(SubagentLink {
                parent_session_id: parent.id,
                depth,
                spawn_id: spawn_id.to_owned(),
                request_digest: request_digest.to_ascii_lowercase(),
            }),
            native_automation: None,
        };
        self.write_metadata_atomically(&child)?;
        Ok(child)
    }

    /// Look up the deterministic child ID for an authorized parent request.
    /// An existing ID with different immutable request data is a conflict.
    pub fn native_child_for_spawn(
        &self,
        parent_lease: &NativeSessionLease,
        spawn_id: &str,
        request_digest: &str,
    ) -> Result<Option<Session>, StoreError> {
        validate_subagent_spawn_id(spawn_id)?;
        validate_subagent_request_digest(request_digest)?;
        let parent = self.native_session_for_lease(parent_lease)?;
        if parent.native_automation.is_some() {
            return Err(StoreError::NativeAutomationSubagentsDisabled);
        }
        let child_id = child_session_id(&parent.id, spawn_id);
        let Some(child) = self.read_metadata(&child_id)? else {
            return Ok(None);
        };
        let valid = child.backend == "native"
            && child.workspace == parent.workspace
            && child.subagent.as_ref().is_some_and(|link| {
                link.parent_session_id == parent.id
                    && link.spawn_id == spawn_id
                    && link.request_digest.eq_ignore_ascii_case(request_digest)
            });
        if !valid {
            return Err(StoreError::ConflictingSubagentIdentity);
        }
        Ok(Some(child))
    }

    pub fn append_event(&self, session_id: &str, event: &AgentEvent) -> Result<(), StoreError> {
        if session_id != event.session_id {
            return Err(StoreError::SessionMismatch);
        }
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _journal_transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        if session.backend == "native" {
            self.require_no_native_automation_steers(&session)?;
            if session.native_automation.is_some() && native_admission_fields(event).is_some() {
                return Err(StoreError::NativeAutomationAdmissionBlocked);
            }
            if matches!(
                &event.kind,
                AgentEventKind::TurnSteered { .. } | AgentEventKind::TurnSteeredWithImages { .. }
            ) {
                return Err(StoreError::NativeSteerRequiresAck);
            }
            // The compatibility append API may seed a legacy native journal
            // with any first sequence, but later records must be contiguous.
            let saved = self.events.load(session_id)?;
            validate_native_automation_admissions(&session, &saved)?;
            if session.native_automation.is_some() && saved.is_empty() {
                return Err(StoreError::NativeAutomationAdmissionBlocked);
            }
            if let Some(last) = saved.last() {
                if event.sequence <= last.sequence {
                    return if saved.iter().find(|saved| saved.sequence == event.sequence)
                        == Some(event)
                    {
                        Ok(())
                    } else {
                        Err(StoreError::ConflictingNativeEvent(event.sequence))
                    };
                }
                if last.sequence.checked_add(1) != Some(event.sequence) {
                    return Err(StoreError::NativeEventOutOfOrder {
                        previous: last.sequence,
                        incoming: event.sequence,
                    });
                }
            }
            if matches!(
                &event.kind,
                AgentEventKind::TurnCompleted { .. }
                    | AgentEventKind::TurnFailed { .. }
                    | AgentEventKind::TurnCancelled
            ) {
                let inbox = self.read_native_steer_inbox(session_id)?;
                if !pending_native_steers_from(&inbox, &committed_native_steers(&saved)?)?
                    .is_empty()
                {
                    return Err(StoreError::PendingNativeSteerInput);
                }
                if matches!(&event.kind, AgentEventKind::TurnCompleted { .. })
                    && native_steer_awaiting_model(&saved)
                {
                    return Err(StoreError::NativeSteerAwaitingModel);
                }
            }
        }
        self.events.append(event)?;
        self.native_tail
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?
            .remove(session_id);
        if matches!(
            &event.kind,
            crate::AgentEventKind::PermissionRequested { .. }
        ) {
            let path = self.events.path(session_id)?;
            OpenOptions::new().write(true).open(path)?.sync_all()?;
        }
        Ok(())
    }

    /// Durably admit a native turn before a worker can make a model request.
    /// Repeating the same ID with the same payload returns its original event,
    /// including its sequence number, even after reopening the store.
    ///
    /// A stable per-session OS lock covers the whole read/validate/append
    /// transaction, including when independent stores share this directory.
    pub fn admit_native_turn(
        &self,
        session_id: &str,
        turn_id: &str,
        prompt: &str,
        model: &str,
        mode: TurnMode,
    ) -> Result<AgentEvent, StoreError> {
        self.admit_native_turn_with_images(session_id, turn_id, prompt, model, mode, &[])
    }

    /// Atomically admit an image-bearing turn. Images are inline in the
    /// event, so reopen and checkpoint replay do not depend on local paths.
    pub fn admit_native_turn_with_images(
        &self,
        session_id: &str,
        turn_id: &str,
        prompt: &str,
        model: &str,
        mode: TurnMode,
        images: &[UserImage],
    ) -> Result<AgentEvent, StoreError> {
        validate_session_id(session_id)?;
        validate_turn_id(turn_id)?;
        validate_user_images(images)?;
        if prompt.trim().is_empty() && images.is_empty() || model.trim().is_empty() {
            return Err(StoreError::InvalidTurnAdmission);
        }
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let journal_transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        if session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession);
        }
        if let Some(origin) = &session.native_automation {
            if turn_id != origin.first_turn_id
                || !images.is_empty()
                || !crate::native_automation_request_digest(prompt, model, mode)
                    .is_ok_and(|digest| digest == origin.first_request_digest)
            {
                return Err(StoreError::NativeAutomationAdmissionBlocked);
            }
        }
        self.require_no_native_automation_steers(&session)?;
        let events = self.events.load(session_id)?;
        validate_native_automation_admissions(&session, &events)?;
        let journal_len = self.native_journal_len(session_id)?;
        let journal_generation = journal_transaction.generation()?;
        let mut native_tail = self
            .native_tail
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        for event in &events {
            if let Some((saved_id, saved_prompt, saved_model, saved_mode, saved_images)) =
                native_admission_fields(event)
            {
                if saved_id == turn_id {
                    if saved_prompt == prompt
                        && saved_model == model
                        && saved_mode == mode
                        && saved_images == images
                    {
                        if let Some(last) = events.last() {
                            native_tail.insert(
                                session_id.to_owned(),
                                NativeJournalTail {
                                    event: last.clone(),
                                    journal_len,
                                    journal_generation,
                                },
                            );
                        }
                        return Ok(event.clone());
                    }
                    return Err(StoreError::ConflictingTurnAdmission(turn_id.to_owned()));
                }
            }
        }
        let _command_operation_guard =
            crate::native_command_operation::lock_and_require_no_pending(
                self.storage_root(),
                &session,
            )
            .map_err(|error| {
                StoreError::NativeCommandOperationAdmissionBlocked(error.to_string())
            })?;
        let inbox = self.read_native_steer_inbox(session_id)?;
        if !pending_native_steers_from(&inbox, &committed_native_steers(&events)?)?.is_empty() {
            return Err(StoreError::PendingNativeSteerInput);
        }
        let sequence = events
            .iter()
            .map(|event| event.sequence)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| StoreError::Io(std::io::Error::other("event sequence exhausted")))?;
        let kind = if images.is_empty() {
            AgentEventKind::TurnAdmitted {
                turn_id: turn_id.to_owned(),
                prompt: prompt.to_owned(),
                model: model.to_owned(),
                mode,
            }
        } else {
            AgentEventKind::TurnAdmittedWithImages {
                turn_id: turn_id.to_owned(),
                prompt: prompt.to_owned(),
                model: model.to_owned(),
                mode,
                images: images.to_vec(),
            }
        };
        let event = AgentEvent::new(session_id, sequence, kind);
        self.events.append_synced(&event)?;
        native_tail.insert(
            session_id.to_owned(),
            NativeJournalTail {
                event: event.clone(),
                journal_len: self.native_journal_len(session_id)?,
                journal_generation,
            },
        );
        Ok(event)
    }

    /// Queue a second user message for the active admitted turn. This does
    /// not touch the event sequence, so a UI handle can call it while the
    /// worker owns the native session lease. The caller supplies a stable
    /// input ID and can retry after an uncertain write outcome.
    pub fn enqueue_native_steer(
        &self,
        session_id: &str,
        turn_id: &str,
        input_id: &str,
        text: &str,
    ) -> Result<NativeSteerInput, StoreError> {
        self.enqueue_native_steer_with_images(session_id, turn_id, input_id, text, &[])
    }

    /// Queue a text/image input while leaving the worker's event sequence
    /// untouched. The input ID binds exact text and image bytes on retries.
    pub fn enqueue_native_steer_with_images(
        &self,
        session_id: &str,
        turn_id: &str,
        input_id: &str,
        text: &str,
        images: &[UserImage],
    ) -> Result<NativeSteerInput, StoreError> {
        validate_session_id(session_id)?;
        validate_turn_id(turn_id)?;
        validate_native_steer_input_id(input_id)?;
        validate_native_steer_text_and_images(text, images)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        if session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession);
        }
        if session.native_automation.is_some() {
            return Err(StoreError::NativeAutomationSteeringDisabled);
        }
        let events = self.events.load(session_id)?;
        let committed = committed_native_steers(&events)?;
        let mut inbox = self.read_native_steer_inbox(session_id)?;
        let pending = pending_native_steers_from(&inbox, &committed)?;
        if let Some(saved) = inbox
            .entries
            .iter()
            .find(|entry| entry.input_id == input_id)
        {
            return if saved.turn_id == turn_id && saved.text == text && saved.images == images {
                Ok(saved.clone())
            } else {
                Err(StoreError::ConflictingNativeSteerInput(input_id.to_owned()))
            };
        }
        if let Some(event) = committed.get(input_id) {
            let (saved_turn, _, saved_text, saved_images) =
                native_steer_fields(event).expect("committed map contains only steer events");
            return if saved_turn == turn_id && saved_text == text && saved_images == images {
                Ok(NativeSteerInput {
                    turn_id: turn_id.to_owned(),
                    input_id: input_id.to_owned(),
                    text: text.to_owned(),
                    images: images.to_vec(),
                })
            } else {
                Err(StoreError::ConflictingNativeSteerInput(input_id.to_owned()))
            };
        }
        if active_native_turn_id(&events) != Some(turn_id)
            || pending.iter().any(|entry| entry.turn_id != turn_id)
        {
            return Err(StoreError::NativeSteerTurnMismatch(turn_id.to_owned()));
        }
        if inbox.finishing_turn_id.as_deref() == Some(turn_id) {
            return Err(StoreError::NativeTurnFinishing(turn_id.to_owned()));
        }
        if pending.len() >= MAX_PENDING_NATIVE_STEERS {
            return Err(StoreError::NativeSteerInboxTooLarge);
        }
        // Committed IDs are already durable in the journal. Keep only the
        // live inbox items so a long-lived session does not exhaust the file.
        inbox.entries = pending;
        let input = NativeSteerInput {
            turn_id: turn_id.to_owned(),
            input_id: input_id.to_owned(),
            text: text.to_owned(),
            images: images.to_vec(),
        };
        inbox.entries.push(input.clone());
        self.write_native_steer_inbox(session_id, &inbox)?;
        Ok(input)
    }

    /// Read unacknowledged steering inputs in queue order. Reading never
    /// consumes them; only a synced TurnSteered journal event does.
    pub fn pending_native_steers(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Vec<NativeSteerInput>, StoreError> {
        validate_session_id(session_id)?;
        validate_turn_id(turn_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        if session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession);
        }
        self.require_no_native_automation_steers(&session)?;
        let events = self.events.load(session_id)?;
        validate_native_automation_admissions(&session, &events)?;
        if active_native_turn_id(&events) != Some(turn_id) {
            return Err(StoreError::NativeSteerTurnMismatch(turn_id.to_owned()));
        }
        let inbox = self.read_native_steer_inbox(session_id)?;
        let committed = committed_native_steers(&events)?;
        let pending = pending_native_steers_from(&inbox, &committed)?;
        if pending.iter().any(|entry| entry.turn_id != turn_id) {
            return Err(StoreError::NativeSteerTurnMismatch(turn_id.to_owned()));
        }
        Ok(pending)
    }

    /// Return the admitted turn that has no terminal event yet. A host may
    /// use this to address a steering submission without guessing the turn
    /// ID from an in-memory worker handle.
    pub fn active_native_turn_id(&self, session_id: &str) -> Result<Option<String>, StoreError> {
        validate_session_id(session_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        if session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession);
        }
        self.require_no_native_automation_steers(&session)?;
        let events = self.events.load(session_id)?;
        validate_native_automation_admissions(&session, &events)?;
        Ok(active_native_turn_id(&events).map(str::to_owned))
    }

    /// Freeze a completed model response before shutting down its prepared
    /// tool runtime. The fence shares the journal lock with enqueue, so a
    /// pending input is visible here or a later enqueue is rejected. It is
    /// durable across restart and idempotent for the same active turn.
    pub fn fence_native_turn_completion(
        &self,
        lease: &NativeSessionLease,
        turn_id: &str,
    ) -> Result<(), StoreError> {
        let session_id = lease.session_id();
        validate_session_id(session_id)?;
        validate_turn_id(turn_id)?;
        if lease.store_root != fs::canonicalize(&self.events.root)? {
            return Err(StoreError::NativeLeaseStoreMismatch);
        }
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        if session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession);
        }
        self.require_no_native_automation_steers(&session)?;
        let events = self.events.load(session_id)?;
        validate_native_automation_admissions(&session, &events)?;
        if active_native_turn_id(&events) != Some(turn_id) {
            return Err(StoreError::NativeSteerTurnMismatch(turn_id.to_owned()));
        }
        let committed = committed_native_steers(&events)?;
        let mut inbox = self.read_native_steer_inbox(session_id)?;
        if !pending_native_steers_from(&inbox, &committed)?.is_empty() {
            return Err(StoreError::PendingNativeSteerInput);
        }
        if native_steer_awaiting_model(&events) {
            return Err(StoreError::NativeSteerAwaitingModel);
        }
        if inbox.finishing_turn_id.as_deref() == Some(turn_id) {
            return Ok(());
        }
        inbox.finishing_turn_id = Some(turn_id.to_owned());
        self.write_native_steer_inbox(session_id, &inbox)
    }

    /// Return whether a durable terminal shutdown fence exists for this turn.
    /// A true result remains true after termination; inspect replay too.
    pub fn native_turn_finish_fence(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<bool, StoreError> {
        validate_session_id(session_id)?;
        validate_turn_id(turn_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        if session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession);
        }
        Ok(self
            .read_native_steer_inbox(session_id)?
            .finishing_turn_id
            .as_deref()
            == Some(turn_id))
    }

    /// Commit one queued input at a worker-controlled model boundary while
    /// holding the exclusive native session lease. The caller passes its next
    /// event sequence and forwards the returned event
    /// to its EventSink, advancing that sink without appending a second copy.
    /// A retry after a crash returns the original journal event.
    pub fn ack_native_steer(
        &self,
        lease: &NativeSessionLease,
        turn_id: &str,
        input_id: &str,
        expected_sequence: u64,
    ) -> Result<AgentEvent, StoreError> {
        let session_id = lease.session_id();
        validate_session_id(session_id)?;
        validate_turn_id(turn_id)?;
        validate_native_steer_input_id(input_id)?;
        if lease.store_root != fs::canonicalize(&self.events.root)? {
            return Err(StoreError::NativeLeaseStoreMismatch);
        }
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        if session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession);
        }
        if session.native_automation.is_some() {
            return Err(StoreError::NativeAutomationSteeringDisabled);
        }
        let events = self.events.load(session_id)?;
        let committed = committed_native_steers(&events)?;
        let inbox = self.read_native_steer_inbox(session_id)?;
        let pending = pending_native_steers_from(&inbox, &committed)?;
        if let Some(event) = committed.get(input_id) {
            let (saved_turn, _, _, _) =
                native_steer_fields(event).expect("committed map contains only steer events");
            return if saved_turn == turn_id {
                Ok((*event).clone())
            } else {
                Err(StoreError::ConflictingNativeSteerInput(input_id.to_owned()))
            };
        }
        let input = pending
            .iter()
            .find(|entry| entry.input_id == input_id)
            .ok_or_else(|| StoreError::NativeSteerNotQueued(input_id.to_owned()))?;
        if input.turn_id != turn_id || active_native_turn_id(&events) != Some(turn_id) {
            return Err(StoreError::NativeSteerTurnMismatch(turn_id.to_owned()));
        }
        if inbox.finishing_turn_id.as_deref() == Some(turn_id) {
            return Err(StoreError::NativeTurnFinishing(turn_id.to_owned()));
        }
        let previous = events.last().map(|event| event.sequence).unwrap_or(0);
        if previous.checked_add(1) != Some(expected_sequence) {
            return Err(StoreError::NativeEventOutOfOrder {
                previous,
                incoming: expected_sequence,
            });
        }
        let kind = if input.images.is_empty() {
            AgentEventKind::TurnSteered {
                turn_id: turn_id.to_owned(),
                input_id: input_id.to_owned(),
                text: input.text.clone(),
            }
        } else {
            AgentEventKind::TurnSteeredWithImages {
                turn_id: turn_id.to_owned(),
                input_id: input_id.to_owned(),
                text: input.text.clone(),
                images: input.images.clone(),
            }
        };
        let event = AgentEvent::new(session_id, expected_sequence, kind);
        // Validate the complete transition before the durable append. This
        // rejects acknowledgements inside model calls or unfinished tool
        // batches, which would corrupt replay ordering.
        let compatibility = events
            .iter()
            .rev()
            .find_map(|saved| match &saved.kind {
                AgentEventKind::CheckpointCommitted { reference } => {
                    Some(reference.compatibility.clone())
                }
                _ => None,
            })
            .unwrap_or(CheckpointCompatibility {
                provider: String::new(),
                model: String::new(),
                context_format_key: String::new(),
            });
        let checkpoints = events
            .iter()
            .any(|saved| matches!(saved.kind, AgentEventKind::CheckpointCommitted { .. }))
            .then(|| CheckpointFileStore::new(&self.events.root))
            .transpose()?;
        let mut proposed = events;
        proposed.push(event.clone());
        replay_native(session_id, &proposed, checkpoints.as_ref(), &compatibility)
            .map_err(StoreError::UnsafeNativeSteerBoundary)?;
        self.events.append_synced(&event)?;
        self.native_tail
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?
            .insert(
                session_id.to_owned(),
                NativeJournalTail {
                    event: event.clone(),
                    journal_len: self.native_journal_len(session_id)?,
                    journal_generation: transaction.generation()?,
                },
            );
        Ok(event)
    }

    /// Append one worker event in order. A repeated, byte-equivalent event is
    /// harmless; a conflicting or skipped sequence is rejected. The tail cache
    /// avoids reparsing the entire log for each streamed delta, but is reused
    /// only while the journal length still matches under the OS lock.
    pub fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
        let session_id = &event.session_id;
        validate_session_id(session_id)?;
        if matches!(
            &event.kind,
            AgentEventKind::TurnAdmitted { .. } | AgentEventKind::TurnAdmittedWithImages { .. }
        ) {
            return Err(StoreError::NativeEventWithoutAdmission);
        }
        if matches!(
            &event.kind,
            AgentEventKind::TurnSteered { .. } | AgentEventKind::TurnSteeredWithImages { .. }
        ) {
            return Err(StoreError::NativeSteerRequiresAck);
        }
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let journal_transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        if session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession);
        }
        self.require_no_native_automation_steers(&session)?;
        let journal_len = self.native_journal_len(session_id)?;
        let journal_generation = journal_transaction.generation()?;
        let mut tails = self
            .native_tail
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        if tails
            .get(session_id)
            .map(|tail| (tail.journal_len, tail.journal_generation))
            != Some((journal_len, journal_generation))
        {
            let events = self.events.load(session_id)?;
            validate_native_automation_admissions(&session, &events)?;
            let last = events
                .last()
                .ok_or(StoreError::NativeEventWithoutAdmission)?;
            if !events.iter().any(|saved| {
                matches!(
                    &saved.kind,
                    AgentEventKind::TurnAdmitted { .. }
                        | AgentEventKind::TurnAdmittedWithImages { .. }
                )
            }) {
                return Err(StoreError::NativeEventWithoutAdmission);
            }
            tails.insert(
                session_id.to_owned(),
                NativeJournalTail {
                    event: last.clone(),
                    journal_len: self.native_journal_len(session_id)?,
                    journal_generation,
                },
            );
        }
        let last = &tails.get(session_id).expect("tail was initialized").event;
        if event.sequence <= last.sequence {
            if event == last {
                return Ok(());
            }
            let saved = self
                .events
                .load(session_id)?
                .into_iter()
                .find(|saved| saved.sequence == event.sequence);
            return if saved.as_ref() == Some(event) {
                Ok(())
            } else {
                Err(StoreError::ConflictingNativeEvent(event.sequence))
            };
        }
        if last.sequence.checked_add(1) != Some(event.sequence) {
            return Err(StoreError::NativeEventOutOfOrder {
                previous: last.sequence,
                incoming: event.sequence,
            });
        }
        // A queued manual operation must be consumed by the leased actor,
        // and an unfinished effect must be explicitly inspected. Retain this
        // ledger fence through journal publication; a prior read would race
        // an intention writer on another store handle/process.
        let _command_operation_guard = if matches!(
            &event.kind,
            AgentEventKind::ModelRequestStarted { .. }
                | AgentEventKind::TurnCompleted { .. }
                | AgentEventKind::TurnFailed { .. }
                | AgentEventKind::TurnCancelled
        ) {
            Some(
                crate::native_command_operation::lock_and_require_no_pending(
                    self.storage_root(),
                    &session,
                )
                .map_err(|error| {
                    StoreError::NativeCommandOperationAdmissionBlocked(error.to_string())
                })?,
            )
        } else {
            None
        };
        if matches!(
            &event.kind,
            AgentEventKind::TurnCompleted { .. }
                | AgentEventKind::TurnFailed { .. }
                | AgentEventKind::TurnCancelled
        ) {
            let inbox = self.read_native_steer_inbox(session_id)?;
            let events = self.events.load(session_id)?;
            if !pending_native_steers_from(&inbox, &committed_native_steers(&events)?)?.is_empty() {
                return Err(StoreError::PendingNativeSteerInput);
            }
            if matches!(&event.kind, AgentEventKind::TurnCompleted { .. })
                && native_steer_awaiting_model(&events)
            {
                return Err(StoreError::NativeSteerAwaitingModel);
            }
        }
        let append = if matches!(
            &event.kind,
            AgentEventKind::AssistantDelta { .. }
                | AgentEventKind::ModelStreamUpdate { .. }
                | AgentEventKind::ToolOutputDelta { .. }
        ) {
            self.events.append(event)
        } else {
            self.events.append_synced(event)
        };
        if let Err(error) = append {
            tails.remove(session_id);
            return Err(error);
        }
        let journal_len = match self.native_journal_len(session_id) {
            Ok(len) => len,
            Err(error) => {
                tails.remove(session_id);
                return Err(error);
            }
        };
        tails.insert(
            session_id.to_owned(),
            NativeJournalTail {
                event: event.clone(),
                journal_len,
                journal_generation,
            },
        );
        Ok(())
    }

    /// Override the display title, or clear the override to use a derived title.
    pub fn rename(&self, session_id: &str, title: Option<&str>) -> Result<Session, StoreError> {
        validate_session_id(session_id)?;
        let title = title
            .map(str::trim)
            .map(|value| {
                if value.is_empty() {
                    Err(StoreError::InvalidTitle)
                } else {
                    Ok(value.to_owned())
                }
            })
            .transpose()?;
        self.update_metadata(session_id, |session| session.title = title)
    }

    pub fn set_pinned(&self, session_id: &str, pinned: bool) -> Result<Session, StoreError> {
        self.update_metadata(session_id, |session| session.pinned = pinned)
    }

    pub fn set_archived(&self, session_id: &str, archived: bool) -> Result<Session, StoreError> {
        validate_session_id(session_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _journal_transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let mut session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.into()))?;
        let _command_operation_guard = if archived && session.backend == "native" {
            Some(
                crate::native_command_operation::lock_and_require_no_pending(
                    self.storage_root(),
                    &session,
                )
                .map_err(|error| {
                    StoreError::NativeCommandOperationAdmissionBlocked(error.to_string())
                })?,
            )
        } else {
            None
        };
        let _command_output_guard = if session.backend == "native" {
            Some(
                crate::native_command_output::lock_and_validate(self.storage_root(), &session)
                    .map_err(|error| StoreError::NativeCommandOutputBlocked(error.to_string()))?,
            )
        } else {
            None
        };
        session.archived = archived;
        session.updated_at_ms = current_time_ms().max(session.updated_at_ms.saturating_add(1));
        self.write_metadata_atomically(&session)?;
        Ok(session)
    }

    /// Allocate a strictly increasing unread marker, even when two changes
    /// happen in the same millisecond or a previous marker was cleared.
    pub fn mark_unread(&self, session_id: &str) -> Result<Session, StoreError> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let mut session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        let watermark = session
            .last_unread_at_ms
            .max(session.unread_at_ms.unwrap_or(0));
        let next = current_time_ms().max(watermark.saturating_add(1));
        session.unread_at_ms = Some(next);
        session.last_unread_at_ms = next;
        self.write_metadata_atomically(&session)?;
        Ok(session)
    }

    /// Clear only the unread state the caller actually observed. A newer
    /// marker survives a delayed navigation or notification acknowledgement.
    pub fn clear_unread_if_matches(
        &self,
        session_id: &str,
        expected_unread_at_ms: u64,
    ) -> Result<(Session, bool), StoreError> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let mut session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        if session.unread_at_ms != Some(expected_unread_at_ms) {
            return Ok((session, false));
        }
        session.unread_at_ms = None;
        self.write_metadata_atomically(&session)?;
        Ok((session, true))
    }

    /// Durably record the user's choice before passing it to the runner.
    /// Retrying the same request and choice returns the original record;
    /// a conflicting choice is rejected so the audit trail stays unambiguous.
    pub fn record_permission_decision(
        &self,
        session_id: &str,
        request_id: &str,
        decision: PermissionDecision,
    ) -> Result<RecordedPermissionDecision, StoreError> {
        self.record_permission_decision_with_grant(session_id, request_id, decision, None)
    }

    /// Persist an Allow decision and its optional session grant in one atomic
    /// journal replacement. The same request, choice, and grant are idempotent;
    /// changing any of them on retry is a conflict. A Deny cannot carry a grant.
    pub fn record_permission_decision_with_grant(
        &self,
        session_id: &str,
        request_id: &str,
        decision: PermissionDecision,
        grant: Option<SessionApprovalGrantRecord>,
    ) -> Result<RecordedPermissionDecision, StoreError> {
        self.record_decision(session_id, request_id, decision, grant, None)
    }

    /// Durably record what the user answered to a question the agent asked,
    /// before the runner is told the request is resolved. The same request
    /// and answer are idempotent; a different answer on retry is a conflict.
    pub fn record_user_answer(
        &self,
        session_id: &str,
        request_id: &str,
        answer: &str,
    ) -> Result<RecordedPermissionDecision, StoreError> {
        if answer.len() > MAX_USER_ANSWER_BYTES {
            return Err(StoreError::InvalidUserAnswer);
        }
        self.record_decision(
            session_id,
            request_id,
            PermissionDecision::Allow,
            None,
            Some(answer.to_owned()),
        )
    }

    fn record_decision(
        &self,
        session_id: &str,
        request_id: &str,
        decision: PermissionDecision,
        grant: Option<SessionApprovalGrantRecord>,
        answer: Option<String>,
    ) -> Result<RecordedPermissionDecision, StoreError> {
        validate_session_id(session_id)?;
        validate_permission_request_id(request_id)?;
        if let Some(grant) = &grant {
            validate_session_approval_grant(grant, session_id, decision)?;
        }
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _session_transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        if self.read_metadata(session_id)?.is_none() {
            return Err(StoreError::UnknownSession(session_id.to_owned()));
        }
        let mut journal = self.read_permission_journal(session_id)?;
        if let Some(existing) = journal
            .entries
            .iter()
            .find(|entry| entry.request_id == request_id)
        {
            return if existing.decision == decision
                && existing.grant == grant
                && existing.answer == answer
            {
                Ok(existing.clone())
            } else {
                Err(StoreError::ConflictingPermissionDecision(
                    request_id.to_owned(),
                ))
            };
        }
        let record = RecordedPermissionDecision {
            session_id: session_id.to_owned(),
            request_id: request_id.to_owned(),
            decision,
            recorded_at_ms: current_time_ms(),
            grant,
            revoked_at_ms: None,
            answer,
        };
        journal.entries.push(record.clone());
        self.write_permission_journal_atomically(session_id, &journal)?;
        Ok(record)
    }

    pub fn load_permission_decisions(
        &self,
        session_id: &str,
    ) -> Result<Vec<RecordedPermissionDecision>, StoreError> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _session_transaction = self.lock_native_journal(session_id)?;
        Ok(self.read_permission_journal(session_id)?.entries)
    }

    /// Return only approved session grants from this session's durable journal.
    pub fn session_approval_grants(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionApprovalGrantRecord>, StoreError> {
        Ok(self
            .load_permission_decisions(session_id)?
            .into_iter()
            .filter(|record| record.revoked_at_ms.is_none())
            .filter_map(|record| record.grant)
            .collect())
    }

    /// Revoke every currently active approval for an exact call/policy key.
    /// The original decisions remain in the journal for audit and idempotent
    /// replay. A later explicit decision may issue the same key again.
    pub fn revoke_session_approval_grant(
        &self,
        session_id: &str,
        key_digest: &str,
    ) -> Result<bool, StoreError> {
        validate_session_id(session_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _session_transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        if self.read_metadata(session_id)?.is_none() {
            return Err(StoreError::UnknownSession(session_id.to_owned()));
        }
        let mut journal = self.read_permission_journal(session_id)?;
        let now = current_time_ms();
        let mut changed = false;
        for entry in &mut journal.entries {
            if entry.revoked_at_ms.is_none()
                && entry
                    .grant
                    .as_ref()
                    .is_some_and(|grant| grant.key_digest == key_digest)
            {
                entry.revoked_at_ms = Some(now.max(entry.recorded_at_ms));
                changed = true;
            }
        }
        if changed {
            // Older binaries understand version 1 but ignore unknown record
            // fields. Bump the journal version so a downgrade cannot reuse a
            // grant that this binary revoked.
            journal.version = 2;
            self.write_permission_journal_atomically(session_id, &journal)?;
        }
        Ok(changed)
    }

    /// Delete metadata first, making the session invisible even if log cleanup fails.
    /// Retrying also removes orphaned event and permission logs. Native children
    /// and native parents with children are protected until a coordinator can
    /// update their lineage and registry together.
    pub fn delete(&self, session_id: &str) -> Result<bool, StoreError> {
        let metadata = self.metadata_path(session_id)?;
        let events = self.events.path(session_id)?;
        let permissions = self.permission_journal_path(session_id)?;
        let native_steers = self.native_steer_inbox_path(session_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let mut journal_transaction = self.lock_native_journal(session_id)?;
        self.ensure_not_pending_native_deletion(session_id)?;
        let session = self.read_metadata(session_id)?;
        if session
            .as_ref()
            .is_some_and(|session| session.subagent.is_some())
        {
            return Err(StoreError::CannotDeleteSubagentDirectly(
                session_id.to_owned(),
            ));
        }
        let _native_lease = if session
            .as_ref()
            .is_some_and(|session| session.backend == "native")
        {
            Some(self.try_acquire_native_lease_locked(session_id)?)
        } else {
            None
        };
        let _command_operation_guard =
            if let Some(session) = session.as_ref().filter(|s| s.backend == "native") {
                Some(
                    crate::native_command_operation::lock_and_require_no_pending(
                        self.storage_root(),
                        session,
                    )
                    .map_err(|error| {
                        StoreError::NativeCommandOperationAdmissionBlocked(error.to_string())
                    })?,
                )
            } else {
                if self
                    .storage_root()
                    .join(format!("{session_id}.native-command-operations.json"))
                    .exists()
                {
                    return Err(StoreError::NativeCommandOperationAdmissionBlocked(
                        "orphaned command operation ledger requires explicit repair".into(),
                    ));
                }
                None
            };
        let _command_output_guard =
            if let Some(session) = session.as_ref().filter(|s| s.backend == "native") {
                Some(crate::native_command_output::lock_and_validate(self.storage_root(), session)
                    .map_err(|error| StoreError::NativeCommandOutputBlocked(error.to_string()))?)
            } else {
                if crate::native_command_output::has_sidecar(self.storage_root(), session_id)
                    .map_err(|error| StoreError::NativeCommandOutputBlocked(error.to_string()))?
                {
                    return Err(StoreError::NativeCommandOutputBlocked(
                        "orphaned command output requires explicit repair".into(),
                    ));
                }
                None
            };
        // A previous interrupted deletion may have removed the parent's
        // metadata while leaving its event log. Keep that log if a child
        // still refers to the session ID.
        if session
            .as_ref()
            .is_none_or(|session| session.backend == "native")
        {
            self.ensure_no_direct_children_locked(session_id)?;
        }
        let mut drafts = self.read_drafts()?;
        let previous_draft_count = drafts.entries.len();
        drafts
            .entries
            .retain(|entry| entry.session_id.as_deref() != Some(session_id));
        let removed_draft = drafts.entries.len() != previous_draft_count;
        if removed_draft {
            self.write_drafts_atomically(&drafts)?;
        }
        if metadata.exists() || events.exists() {
            journal_transaction.bump_generation()?;
        }
        let removed_native_command_operations =
            crate::native_command_operation::remove_settled_files(self.storage_root(), session_id)
                .map_err(|error| {
                    StoreError::NativeCommandOperationAdmissionBlocked(error.to_string())
                })?;
        let removed_native_command_output =
            crate::native_command_output::remove_files(self.storage_root(), session_id)
                .map_err(|error| StoreError::NativeCommandOutputBlocked(error.to_string()))?;
        let removed_metadata = remove_if_exists(metadata)?;
        let removed_events = remove_if_exists(events)?;
        let removed_permissions = remove_if_exists(permissions)?;
        let removed_native_steers = remove_if_exists(native_steers)?;
        let removed_native_goals = remove_if_exists(
            self.events
                .root
                .join(format!("{session_id}.native-goals.json")),
        )?;
        let removed_rewind_tails = self.remove_rewind_tails(session_id)?;
        Ok(removed_draft
            || removed_rewind_tails
            || removed_metadata
            || removed_events
            || removed_permissions
            || removed_native_steers
            || removed_native_goals
            || removed_native_command_operations
            || removed_native_command_output)
    }

    /// Remove an entire top-level native session tree after every worker lease
    /// is free and every child has a terminal registry state. A durable intent
    /// blocks new native leases and admissions until cleanup finishes. A retry
    /// with the same root ID completes an interrupted cleanup, including when
    /// some child metadata or the root metadata has already been removed.
    /// This only removes files inside the session state root; workspace files
    /// and stable OS lease lock files are never removed.
    pub fn delete_native_tree(&self, root_id: &str) -> Result<bool, StoreError> {
        validate_session_id(root_id)?;
        let _deletion_guard = self.lock_native_tree_deletion(root_id)?;
        let _operation_guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;

        let existing_intent = self.read_native_delete_intent(root_id)?;
        let mut leases = Vec::new();
        let intent = if let Some(intent) = existing_intent {
            self.validate_recovery_tree(&intent)?;
            for node in &intent.nodes {
                if self.read_metadata(&node.id)?.is_some() {
                    leases.push(self.try_acquire_native_lease_for_deletion(&node.id, true)?);
                }
            }
            intent
        } else {
            let Some(root) = self.read_metadata(root_id)? else {
                return Ok(false);
            };
            if root.backend != "native" || root.subagent.is_some() {
                return Err(StoreError::NativeDeletionRequiresRoot);
            }
            leases.push(self.try_acquire_native_lease_for_deletion(root_id, false)?);
            self.discover_native_tree(root, &mut leases)?
        };

        // Freeze every journal before publishing the deletion intent. An
        // admission that began before the intent must finish first; once the
        // intent is durable, new admissions fail while these locks are held.
        let mut journal_ids = intent
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        journal_ids.sort();
        journal_ids.dedup();
        let mut journal_transactions = HashMap::with_capacity(journal_ids.len());
        for id in journal_ids {
            journal_transactions.insert(id.clone(), self.lock_native_journal(&id)?);
        }

        // Preserve uncertain human effects before publishing a destructive
        // tree intent. Intention writers are frozen by the journal locks;
        // workers by the leases. Retain each ledger lock through cleanup.
        let mut command_operation_guards = Vec::new();
        for node in &intent.nodes {
            if let Some(session) = self.read_metadata(&node.id)? {
                command_operation_guards.push(
                    crate::native_command_operation::lock_and_require_no_pending(
                        self.storage_root(),
                        &session,
                    )
                    .map_err(|error| {
                        StoreError::NativeCommandOperationAdmissionBlocked(error.to_string())
                    })?,
                );
            } else if self
                .storage_root()
                .join(format!("{}.native-command-operations.json", node.id))
                .exists()
            {
                return Err(StoreError::NativeCommandOperationAdmissionBlocked(
                    "orphaned command operation ledger requires explicit repair".into(),
                ));
            }
        }

        let mut command_output_guards = Vec::new();
        for node in &intent.nodes {
            if let Some(session) = self.read_metadata(&node.id)? {
                command_output_guards.push(
                    crate::native_command_output::lock_and_validate(self.storage_root(), &session)
                        .map_err(|error| {
                            StoreError::NativeCommandOutputBlocked(error.to_string())
                        })?,
                );
            } else if crate::native_command_output::has_sidecar(self.storage_root(), &node.id)
                .map_err(|error| StoreError::NativeCommandOutputBlocked(error.to_string()))?
            {
                return Err(StoreError::NativeCommandOutputBlocked(
                    "orphaned command output requires explicit repair".into(),
                ));
            }
        }

        let _registry_guard = self.lock_subagent_registry()?;
        let mut registry = self.read_subagent_registry()?;
        let mut drafts = self.read_drafts()?;
        if !self.native_delete_intent_path(root_id)?.exists() {
            self.ensure_tree_terminal(&intent, registry.as_ref())?;
            self.validate_recovery_tree(&intent)?;
            if serde_json::to_vec(&intent)?.len() as u64 + 1 > MAX_NATIVE_DELETE_INTENT_BYTES {
                return Err(StoreError::InvalidNativeDeletionIntent(root_id.to_owned()));
            }
            fs::create_dir_all(self.native_delete_intent_directory())?;
            self.verified_native_delete_intent_directory()?
                .ok_or_else(|| StoreError::InvalidNativeDeletionIntent(root_id.to_owned()))?;
            self.write_json_atomically(
                self.native_delete_intent_path(root_id)?,
                &format!("native-delete.{root_id}"),
                &intent,
            )?;
        }

        let members = intent
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<HashSet<_>>();
        if let Some(value) = registry.as_mut() {
            let object = value
                .as_object_mut()
                .expect("validated subagent registry is an object");
            let mut changed = false;
            if let Some(children) = object
                .get_mut("children")
                .and_then(serde_json::Value::as_object_mut)
            {
                for id in &members {
                    changed |= children.remove(*id).is_some();
                }
            }
            if let Some(inputs) = object
                .get_mut("inputs")
                .and_then(serde_json::Value::as_object_mut)
            {
                for id in &members {
                    changed |= inputs.remove(*id).is_some();
                }
            }
            if changed {
                self.write_json_atomically(
                    self.events.root.join("subagents.registry.json"),
                    "subagents.registry",
                    value,
                )?;
            }
        }

        let previous_drafts = drafts.entries.len();
        drafts.entries.retain(|draft| {
            !draft
                .session_id
                .as_deref()
                .is_some_and(|id| members.contains(id))
        });
        if drafts.entries.len() != previous_drafts {
            self.write_drafts_atomically(&drafts)?;
        }

        // Descendants disappear before their parents. The intent stays until
        // every sidecar and metadata file has been handled successfully.
        for node in intent.nodes.iter().rev() {
            journal_transactions
                .get_mut(&node.id)
                .expect("validated deletion node has a journal lock")
                .bump_generation()?;
            crate::native_command_operation::remove_settled_files(self.storage_root(), &node.id)
                .map_err(|error| {
                    StoreError::NativeCommandOperationAdmissionBlocked(error.to_string())
                })?;
            crate::native_command_output::remove_files(self.storage_root(), &node.id)
                .map_err(|error| StoreError::NativeCommandOutputBlocked(error.to_string()))?;
            remove_if_exists(self.events.path(&node.id)?)?;
            remove_if_exists(self.permission_journal_path(&node.id)?)?;
            remove_if_exists(self.native_steer_inbox_path(&node.id)?)?;
            remove_if_exists(
                self.events
                    .root
                    .join(format!("{}.native-goals.json", node.id)),
            )?;
            remove_if_exists(self.metadata_path(&node.id)?)?;
            #[cfg(unix)]
            File::open(&self.events.root)?.sync_all()?;
            self.native_tail
                .lock()
                .map_err(|_| StoreError::PoisonedLock)?
                .remove(&node.id);
        }
        remove_if_exists(self.native_delete_intent_path(root_id)?)?;
        #[cfg(unix)]
        File::open(self.native_delete_intent_directory())?.sync_all()?;
        Ok(true)
    }

    /// Roots with a durable deletion intent. Hosts can surface or resume
    /// these after startup even when the root metadata was already removed.
    pub fn pending_native_deletion_roots(&self) -> Result<Vec<String>, StoreError> {
        let mut roots = self
            .native_delete_intents()?
            .into_iter()
            .map(|intent| intent.root_id)
            .collect::<Vec<_>>();
        roots.sort();
        Ok(roots)
    }

    /// Finish every interrupted native tree deletion. Returns the number of
    /// intents successfully completed; the first unsafe/corrupt tree is left
    /// untouched and reported as an error for explicit repair.
    pub fn recover_pending_native_deletions(&self) -> Result<usize, StoreError> {
        let roots = self.pending_native_deletion_roots()?;
        for root in &roots {
            self.delete_native_tree(root)?;
        }
        Ok(roots.len())
    }

    /// Persist a plain-text composer draft for a workspace and optional
    /// session. `None` addresses the new-session draft for that workspace.
    /// Saving blank text clears the draft.
    pub fn save_draft(
        &self,
        workspace: &Path,
        session_id: Option<&str>,
        text: &str,
    ) -> Result<(), StoreError> {
        if let Some(id) = session_id {
            validate_session_id(id)?;
        }
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        if let Some(id) = session_id {
            self.ensure_not_pending_native_deletion(id)?;
        }
        let mut drafts = self.read_drafts()?;
        drafts.entries.retain(|entry| {
            entry.workspace != workspace || entry.session_id.as_deref() != session_id
        });
        if !text.trim().is_empty() {
            drafts.entries.push(DraftEntry {
                workspace: workspace.to_path_buf(),
                session_id: session_id.map(str::to_owned),
                text: text.to_owned(),
                updated_at_ms: current_time_ms(),
            });
        }
        self.write_drafts_atomically(&drafts)
    }

    pub fn load_draft(
        &self,
        workspace: &Path,
        session_id: Option<&str>,
    ) -> Result<Option<String>, StoreError> {
        if let Some(id) = session_id {
            validate_session_id(id)?;
        }
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        Ok(self
            .read_drafts()?
            .entries
            .into_iter()
            .find(|entry| entry.workspace == workspace && entry.session_id.as_deref() == session_id)
            .map(|entry| entry.text))
    }

    /// Returns whether a draft existed. Removing a missing draft is harmless.
    pub fn clear_draft(
        &self,
        workspace: &Path,
        session_id: Option<&str>,
    ) -> Result<bool, StoreError> {
        if let Some(id) = session_id {
            validate_session_id(id)?;
        }
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let mut drafts = self.read_drafts()?;
        let before = drafts.entries.len();
        drafts.entries.retain(|entry| {
            entry.workspace != workspace || entry.session_id.as_deref() != session_id
        });
        if before == drafts.entries.len() {
            return Ok(false);
        }
        self.write_drafts_atomically(&drafts)?;
        Ok(true)
    }

    pub fn list(&self) -> Result<Vec<Session>, StoreError> {
        Ok(self
            .list_with_events()?
            .into_iter()
            .map(|record| record.session)
            .collect())
    }

    pub fn list_active(&self) -> Result<Vec<Session>, StoreError> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|session| !session.archived)
            .collect())
    }

    pub fn list_archived(&self) -> Result<Vec<Session>, StoreError> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|session| session.archived)
            .collect())
    }

    pub fn list_active_with_events(&self) -> Result<Vec<StoredSession>, StoreError> {
        Ok(self
            .list_with_events()?
            .into_iter()
            .filter(|record| !record.session.archived)
            .collect())
    }

    pub fn list_archived_with_events(&self) -> Result<Vec<StoredSession>, StoreError> {
        Ok(self
            .list_with_events()?
            .into_iter()
            .filter(|record| record.session.archived)
            .collect())
    }

    /// Lists sessions together with their event logs so callers can derive
    /// display titles without reading each log a second time.
    pub fn list_with_events(&self) -> Result<Vec<StoredSession>, StoreError> {
        Ok(self.list_with_diagnostics()?.sessions)
    }

    /// Keep readable sessions available when one session's metadata or event
    /// log is malformed. Directory and other filesystem failures still fail
    /// the listing, so they cannot silently look like an empty store.
    pub fn list_with_diagnostics(&self) -> Result<SessionListing, StoreError> {
        let mut sessions = Vec::new();
        let mut diagnostics = Vec::new();
        for entry in fs::read_dir(&self.events.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(id) = name.strip_suffix(".session.json") else {
                continue;
            };
            if validate_session_id(id).is_err() {
                diagnostics.push(SessionDiagnostic {
                    session_id: id.to_owned(),
                    error: StoreError::InvalidSessionId.to_string(),
                });
                continue;
            }
            match self.load(id) {
                Ok(Some(record)) => sessions.push(record),
                Ok(None) => {} // The file disappeared between read_dir and load.
                Err(error) if is_session_corruption(&error) => {
                    diagnostics.push(SessionDiagnostic {
                        session_id: id.to_owned(),
                        error: error.to_string(),
                    })
                }
                Err(error) => return Err(error),
            }
        }
        sessions.sort_by(|left, right| {
            right
                .session
                .updated_at_ms
                .cmp(&left.session.updated_at_ms)
                .then_with(|| left.session.id.cmp(&right.session.id))
        });
        diagnostics.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        Ok(SessionListing {
            sessions,
            diagnostics,
        })
    }

    /// Query the sessions dock and command-center search with deterministic
    /// filtering, ordering and pagination. This initial implementation scans
    /// readable JSONL logs; a dedicated index can replace it without changing
    /// the result contract.
    pub fn query_sessions(&self, query: &SessionQuery) -> Result<SessionQueryResult, StoreError> {
        let search = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if search.is_some_and(|s| s.chars().count() > 256) {
            return Err(StoreError::SearchQueryTooLong);
        }
        let pattern = search
            .map(|s| {
                RegexBuilder::new(&regex::escape(s))
                    .case_insensitive(true)
                    .build()
            })
            .transpose()?;
        let listing = self.list_with_diagnostics()?;
        let mut matches = Vec::new();
        for record in listing.sessions {
            let session = &record.session;
            if !query.workspaces.is_empty() && !query.workspaces.contains(&session.workspace) {
                continue;
            }
            let in_scope = match query.kind {
                SessionListKind::ActiveUnpinned => !session.archived && !session.pinned,
                SessionListKind::Pinned => !session.archived && session.pinned,
                SessionListKind::Archived => session.archived,
                SessionListKind::All => true,
            };
            if !in_scope {
                continue;
            }
            let title = display_title(&record);
            let snippets = if let Some(pattern) = &pattern {
                let visible_text = visible_search_text(&record.events);
                let title_matches = pattern.is_match(&title);
                if !title_matches && !pattern.is_match(&visible_text) {
                    continue;
                }
                search_snippets(&visible_text, pattern, title_matches)
            } else {
                Vec::new()
            };
            matches.push(SessionQueryItem {
                session: record.session,
                title,
                search_snippets: snippets,
            });
        }
        matches.sort_by(|left, right| {
            let left_time = match query.sort {
                SessionSort::Updated => left.session.updated_at_ms,
                SessionSort::Created => left.session.created_at_ms,
            };
            let right_time = match query.sort {
                SessionSort::Updated => right.session.updated_at_ms,
                SessionSort::Created => right.session.created_at_ms,
            };
            right_time
                .cmp(&left_time)
                .then_with(|| right.session.created_at_ms.cmp(&left.session.created_at_ms))
                .then_with(|| left.session.id.cmp(&right.session.id))
        });
        let total = matches.len();
        let items = matches
            .into_iter()
            .skip(query.offset)
            .take(query.limit)
            .collect::<Vec<_>>();
        let has_more = total > query.offset.saturating_add(items.len());
        Ok(SessionQueryResult {
            items,
            total,
            has_more,
            diagnostics: listing.diagnostics,
        })
    }

    pub fn load(&self, session_id: &str) -> Result<Option<StoredSession>, StoreError> {
        self.load_record(session_id, true)
    }

    /// Observe a journal without truncating or repairing an unterminated tail.
    /// A projection must not silently repair its own evidence. Explicit recovery
    /// and the normal writer retain the existing tail-repair contract.
    pub fn load_strict(&self, session_id: &str) -> Result<Option<StoredSession>, StoreError> {
        self.load_record(session_id, false)
    }

    fn load_record(
        &self,
        session_id: &str,
        repair_tail: bool,
    ) -> Result<Option<StoredSession>, StoreError> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let _journal_transaction = self.lock_native_journal(session_id)?;
        let Some(mut session) = self.read_metadata(session_id)? else {
            return Ok(None);
        };
        self.require_no_native_automation_steers(&session)?;
        let events = if repair_tail {
            self.events.load(session_id)?
        } else {
            let _guard = self
                .events
                .write_lock
                .lock()
                .map_err(|_| StoreError::PoisonedLock)?;
            self.events
                .load_inner_with_repair(&self.events.path(session_id)?, session_id, false)?
        };
        validate_native_automation_admissions(&session, &events)?;
        session.updated_at_ms = session.updated_at_ms.max(
            events
                .last()
                .map(|event| event.timestamp_ms.max(session.created_at_ms))
                .unwrap_or(session.created_at_ms),
        );
        Ok(Some(StoredSession { session, events }))
    }
}

fn display_title(record: &StoredSession) -> String {
    record
        .session
        .title
        .as_deref()
        .or_else(|| {
            record.events.iter().find_map(|event| match &event.kind {
                AgentEventKind::TurnAdmitted { prompt, .. }
                | AgentEventKind::TurnAdmittedWithImages { prompt, .. }
                | AgentEventKind::TurnStarted { prompt } => {
                    Some(prompt.lines().next().unwrap_or(prompt).trim())
                }
                _ => None,
            })
        })
        .filter(|title| !title.is_empty())
        .unwrap_or("Nueva tarea")
        .to_owned()
}

/// Mirrors the text a user sees in the transcript. Streaming deltas and tool
/// input/output are excluded, so private or duplicated implementation detail
/// does not leak into search results.
fn visible_search_text(events: &[AgentEvent]) -> String {
    const MAX_SEARCH_TEXT_CHARS: usize = 200_000;
    let mut text = String::new();
    let mut remaining = MAX_SEARCH_TEXT_CHARS;
    for event in events {
        let part = match &event.kind {
            AgentEventKind::TurnStarted { prompt } => prompt.as_str(),
            AgentEventKind::TurnSteered { text, .. }
            | AgentEventKind::TurnSteeredWithImages { text, .. } => text.as_str(),
            AgentEventKind::AssistantMessage { content } => content.as_str(),
            AgentEventKind::TurnCompleted { response, .. } => response.as_str(),
            AgentEventKind::ProcessOutput { text, .. } => text.as_str(),
            _ => continue,
        };
        if part.trim().is_empty() || remaining == 0 {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
            remaining -= 1;
        }
        if remaining == 0 {
            break;
        }
        let capped = part.chars().take(remaining).collect::<String>();
        remaining -= capped.chars().count();
        text.push_str(&capped);
    }
    text
}

fn search_snippets(text: &str, pattern: &Regex, title_matches: bool) -> Vec<String> {
    const MAX_SNIPPETS: usize = 4;
    const MAX_SNIPPET_CHARS: usize = 140;
    let mut snippets = Vec::new();
    let mut ranges = Vec::new();
    for found in pattern.find_iter(text) {
        let start = text[..found.start()]
            .char_indices()
            .rev()
            .nth(19)
            .map_or(0, |(index, _)| index);
        let end = text[found.end()..]
            .char_indices()
            .nth(72)
            .map_or(text.len(), |(index, _)| found.end() + index);
        if ranges
            .iter()
            .any(|&(old_start, old_end)| start < old_end && old_start < end)
        {
            continue;
        }
        let raw = format!(
            "{}{}{}",
            if start > 0 { "…" } else { "" },
            &text[start..end],
            if end < text.len() { "…" } else { "" }
        );
        let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        let snippet = normalized
            .chars()
            .take(MAX_SNIPPET_CHARS)
            .collect::<String>();
        if !snippet.is_empty() {
            snippets.push(snippet);
            ranges.push((start, end));
        }
        if snippets.len() == MAX_SNIPPETS {
            break;
        }
    }
    if snippets.is_empty() && title_matches {
        let fallback = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let fallback = fallback.chars().take(MAX_SNIPPET_CHARS).collect::<String>();
        if !fallback.is_empty() {
            snippets.push(fallback);
        }
    }
    snippets
}

// serde_json wraps reader failures in its own Error. Preserve their I/O
// classification so listing cannot treat an inaccessible store as bad JSON.
impl From<serde_json::Error> for StoreError {
    fn from(error: serde_json::Error) -> Self {
        if error.is_io() {
            Self::Io(std::io::Error::from(error))
        } else {
            Self::Json(error)
        }
    }
}

fn is_session_corruption(error: &StoreError) -> bool {
    matches!(
        error,
        StoreError::Json(_)
            | StoreError::SessionMismatch
            | StoreError::SessionMetadataTooLarge
            | StoreError::NativeAutomationOrigin(_)
            | StoreError::NativeAutomationAdmissionBlocked
            | StoreError::NativeAutomationSteeringDisabled
    )
}

fn remove_if_exists(path: PathBuf) -> Result<bool, StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

impl EventStore for SessionStore {
    fn append(&self, event: &AgentEvent) -> Result<(), StoreError> {
        self.append_event(&event.session_id, event)
    }

    fn load(&self, session_id: &str) -> Result<Vec<AgentEvent>, StoreError> {
        Ok(SessionStore::load(self, session_id)?
            .map(|record| record.events)
            .unwrap_or_default())
    }
}

impl NativeEventJournal for SessionStore {
    fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
        SessionStore::append_native_event(self, event)
    }
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

impl EventStore for JsonlSessionStore {
    fn append(&self, event: &AgentEvent) -> Result<(), StoreError> {
        self.append_inner(event, false)
    }

    fn load(&self, session_id: &str) -> Result<Vec<AgentEvent>, StoreError> {
        let path = self.path(session_id)?;
        let mut checked = self
            .write_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let events = self.load_inner(&path, session_id)?;
        checked.insert(session_id.to_owned());
        Ok(events)
    }
}

impl JsonlSessionStore {
    /// The caller holds `write_lock`, so a repaired tail cannot race an append.
    fn load_inner(&self, path: &Path, session_id: &str) -> Result<Vec<AgentEvent>, StoreError> {
        self.load_inner_with_repair(path, session_id, true)
    }

    fn load_inner_with_repair(
        &self,
        path: &Path,
        session_id: &str,
        repair_tail: bool,
    ) -> Result<Vec<AgentEvent>, StoreError> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut events: Vec<AgentEvent> = Vec::new();
        let mut committed_len = 0u64;
        let repair = loop {
            line.clear();
            let read = (&mut reader)
                .take(MAX_EVENT_RECORD_BYTES + 1)
                .read_until(b'\n', &mut line)?;
            if read == 0 {
                break None;
            }
            if read as u64 > MAX_EVENT_RECORD_BYTES {
                return Err(StoreError::InconsistentEventLog {
                    session_id: session_id.to_owned(),
                    line: events.len() + 1,
                    reason: "event record exceeds journal line limit",
                });
            }
            let terminated = line.last() == Some(&b'\n');
            let event = match serde_json::from_slice::<AgentEvent>(&line) {
                Ok(event) => event,
                // An unterminated final record may be a torn write. Complete
                // lines are never discarded, even when their JSON is invalid.
                Err(_) if !terminated => break Some(TailRepair::Truncate(committed_len)),
                Err(error) => return Err(error.into()),
            };
            if !valid_system_instructions_snapshot(&event) {
                return Err(StoreError::InconsistentEventLog {
                    session_id: session_id.to_owned(),
                    line: events.len() + 1,
                    reason: "invalid system instruction snapshot",
                });
            }
            if !valid_model_route_snapshot(&event) {
                return Err(StoreError::InconsistentEventLog {
                    session_id: session_id.to_owned(),
                    line: events.len() + 1,
                    reason: "invalid model route snapshot",
                });
            }
            if !valid_plan_snapshot(&event) {
                return Err(StoreError::InconsistentEventLog {
                    session_id: session_id.to_owned(),
                    line: events.len() + 1,
                    reason: "invalid plan snapshot",
                });
            }
            if validate_user_image_event(&event).is_err() {
                return Err(StoreError::InconsistentEventLog {
                    session_id: session_id.to_owned(),
                    line: events.len() + 1,
                    reason: "invalid user image input",
                });
            }
            if event.session_id != session_id {
                return Err(StoreError::InconsistentEventLog {
                    session_id: session_id.to_owned(),
                    line: events.len() + 1,
                    reason: "session id mismatch",
                });
            }
            if let Some(previous) = events.last() {
                if previous.sequence.checked_add(1) != Some(event.sequence) {
                    return Err(StoreError::InconsistentEventLog {
                        session_id: session_id.to_owned(),
                        line: events.len() + 1,
                        reason: "event sequence is not contiguous",
                    });
                }
            }
            events.push(event);
            if !terminated {
                // A complete JSON record without its delimiter is committed;
                // restore the newline so the next append remains separate.
                break Some(TailRepair::AppendNewline);
            }
            committed_len = committed_len.checked_add(read as u64).ok_or_else(|| {
                StoreError::Io(std::io::Error::other("event log exceeds supported size"))
            })?;
        };
        drop(reader);
        if let Some(repair) = repair {
            if !repair_tail {
                return Err(StoreError::InconsistentEventLog {
                    session_id: session_id.to_owned(),
                    line: events.len() + 1,
                    reason: "unterminated journal tail requires explicit recovery",
                });
            }
            let mut file = OpenOptions::new().write(true).open(path)?;
            match repair {
                TailRepair::Truncate(len) => file.set_len(len)?,
                TailRepair::AppendNewline => {
                    use std::io::{Seek, SeekFrom};
                    file.seek(SeekFrom::End(0))?;
                    file.write_all(b"\n")?;
                }
            }
            file.sync_all()?;
        }
        Ok(events)
    }

    fn append_synced(&self, event: &AgentEvent) -> Result<(), StoreError> {
        self.append_inner(event, true)
    }

    fn append_inner(&self, event: &AgentEvent, sync: bool) -> Result<(), StoreError> {
        if !valid_model_route_snapshot(event) {
            return Err(StoreError::InvalidModelRouteSnapshot);
        }
        if !valid_system_instructions_snapshot(event) {
            return Err(StoreError::InvalidSystemInstructionsSnapshot);
        }
        if !valid_plan_snapshot(event) {
            return Err(StoreError::InvalidPlanSnapshot);
        }
        validate_user_image_event(event)?;
        let path = self.path(&event.session_id)?;
        let mut checked = self
            .write_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        if !checked.contains(&event.session_id) {
            self.load_inner(&path, &event.session_id)?;
            checked.insert(event.session_id.clone());
        }
        let result = (|| {
            let mut payload = serde_json::to_vec(event)?;
            if payload.len() as u64 + 1 > MAX_EVENT_RECORD_BYTES {
                return Err(StoreError::EventRecordTooLarge);
            }
            payload.push(b'\n');
            let mut file = OpenOptions::new().create(true).append(true).open(path)?;
            file.write_all(&payload)?;
            if sync {
                file.sync_all()?;
                #[cfg(unix)]
                File::open(&self.root)?.sync_all()?;
            } else {
                file.flush()?;
            }
            Ok(())
        })();
        if result.is_err() {
            // A failed write may have left a partial record. Revalidate and
            // repair that file before any later append in this process.
            checked.remove(&event.session_id);
        }
        result
    }
}

enum TailRepair {
    Truncate(u64),
    AppendNewline,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentEventKind;
    use std::sync::Arc;

    #[test]
    fn strict_session_observation_preserves_torn_and_unterminated_journal_evidence() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let session = store.create(root.path(), "native").unwrap();
        store
            .admit_native_turn(
                &session.id,
                "strict-turn",
                "Inspect",
                "model",
                TurnMode::Build,
            )
            .unwrap();
        let path = root.path().join(format!("{}.jsonl", session.id));
        let original = fs::read(&path).unwrap();
        let mut torn = original.clone();
        torn.extend_from_slice(b"{\"session_id\":");
        fs::write(&path, &torn).unwrap();
        assert!(store.load_strict(&session.id).is_err());
        assert_eq!(fs::read(&path).unwrap(), torn);
        let reopened = SessionStore::open(root.path()).unwrap();
        assert!(reopened.load_strict(&session.id).is_err());
        assert_eq!(fs::read(&path).unwrap(), torn);
        assert_eq!(reopened.load(&session.id).unwrap().unwrap().events.len(), 1);
        assert_eq!(fs::read(&path).unwrap(), original);
        let mut undelimited = original.clone();
        assert_eq!(undelimited.pop(), Some(b'\n'));
        fs::write(&path, &undelimited).unwrap();
        assert!(reopened.load_strict(&session.id).is_err());
        assert_eq!(fs::read(&path).unwrap(), undelimited);
        assert_eq!(reopened.load(&session.id).unwrap().unwrap().events.len(), 1);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(reopened.load_strict(&session.id).unwrap().is_some());
    }

    #[test]
    fn jsonl_round_trip_and_session_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::new(dir.path()).unwrap();
        let event = AgentEvent::new("session_1", 1, AgentEventKind::TurnCancelled);
        store.append(&event).unwrap();
        assert_eq!(store.load("session_1").unwrap(), vec![event]);
        assert!(store.load("session_2").unwrap().is_empty());
        assert!(matches!(
            store.load("../escape"),
            Err(StoreError::InvalidSessionId)
        ));
        assert_eq!(store.list_sessions().unwrap(), vec!["session_1"]);
    }

    #[test]
    fn instruction_snapshot_is_validated_on_write_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::new(dir.path()).unwrap();
        let valid = AgentEvent::new(
            "session_1",
            1,
            AgentEventKind::SystemInstructionsCommitted {
                version: SYSTEM_INSTRUCTIONS_VERSION,
                digest: system_instructions_digest("original project text").unwrap(),
                content: "original project text".into(),
            },
        );
        store.append(&valid).unwrap();
        assert_eq!(store.load("session_1").unwrap(), vec![valid.clone()]);
        assert!(visible_search_text(&[valid]).is_empty());
        let oversized = AgentEvent::new(
            "session_1",
            2,
            AgentEventKind::SystemInstructionsCommitted {
                version: SYSTEM_INSTRUCTIONS_VERSION,
                digest: "a".repeat(64),
                content: "x".repeat(crate::MAX_SYSTEM_INSTRUCTIONS_BYTES + 1),
            },
        );
        assert!(matches!(
            store.append(&oversized),
            Err(StoreError::InvalidSystemInstructionsSnapshot)
        ));
        assert!(matches!(
            MemorySessionStore::new().append(&oversized),
            Err(StoreError::InvalidSystemInstructionsSnapshot)
        ));
        let tampered = AgentEvent::new(
            "session_1",
            1,
            AgentEventKind::SystemInstructionsCommitted {
                version: SYSTEM_INSTRUCTIONS_VERSION,
                digest: "0".repeat(64),
                content: "original project text".into(),
            },
        );
        let mut encoded = serde_json::to_vec(&tampered).unwrap();
        encoded.push(b'\n');
        fs::write(dir.path().join("session_1.jsonl"), encoded).unwrap();
        assert!(matches!(
            JsonlSessionStore::new(dir.path())
                .unwrap()
                .load("session_1"),
            Err(StoreError::InconsistentEventLog {
                reason: "invalid system instruction snapshot",
                ..
            })
        ));
        let mut encoded = serde_json::to_vec(&oversized).unwrap();
        encoded.push(b'\n');
        fs::write(dir.path().join("session_1.jsonl"), encoded).unwrap();
        assert!(matches!(
            JsonlSessionStore::new(dir.path())
                .unwrap()
                .load("session_1"),
            Err(StoreError::InconsistentEventLog {
                reason: "invalid system instruction snapshot",
                ..
            })
        ));
    }

    #[test]
    fn plan_snapshots_are_validated_on_write_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::new(dir.path()).unwrap();
        let valid = AgentEvent::new(
            "session_1",
            1,
            AgentEventKind::PlanProposed {
                plan: crate::PlanSnapshot::from_plan_response("1. Inspect files"),
            },
        );
        store.append(&valid).unwrap();
        let invalid = crate::PlanSnapshot {
            explanation: None,
            plan: Vec::new(),
        };
        for kind in [
            AgentEventKind::PlanProposed {
                plan: invalid.clone(),
            },
            AgentEventKind::PlanUpdated {
                tool_call_id: "plan-tool".into(),
                plan: invalid.clone(),
            },
        ] {
            let event = AgentEvent::new("session_1", 2, kind);
            assert!(matches!(
                store.append(&event),
                Err(StoreError::InvalidPlanSnapshot)
            ));
            assert!(matches!(
                MemorySessionStore::new().append(&event),
                Err(StoreError::InvalidPlanSnapshot)
            ));
            let mut encoded = serde_json::to_vec(&valid).unwrap();
            encoded.push(b'\n');
            encoded.extend_from_slice(&serde_json::to_vec(&event).unwrap());
            encoded.push(b'\n');
            fs::write(dir.path().join("session_1.jsonl"), encoded).unwrap();
            assert!(matches!(
                JsonlSessionStore::new(dir.path())
                    .unwrap()
                    .load("session_1"),
                Err(StoreError::InconsistentEventLog {
                    line: 2,
                    reason: "invalid plan snapshot",
                    ..
                })
            ));
        }
    }

    #[test]
    fn jsonl_reopen_discards_only_a_torn_final_record_before_append() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::new(dir.path()).unwrap();
        let first = AgentEvent::new("session_1", 1, AgentEventKind::TurnCancelled);
        let second = AgentEvent::new("session_1", 2, AgentEventKind::TurnCancelled);
        let third = AgentEvent::new("session_1", 3, AgentEventKind::TurnCancelled);
        store.append(&first).unwrap();
        store.append(&second).unwrap();
        let path = dir.path().join("session_1.jsonl");
        let torn = serde_json::to_vec(&third).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&torn[..torn.len() - 2])
            .unwrap();
        drop(store);

        // The first append after reopening validates and repairs the tail,
        // even if the caller has not loaded the log explicitly.
        let reopened = JsonlSessionStore::new(dir.path()).unwrap();
        reopened.append(&third).unwrap();
        assert_eq!(
            reopened.load("session_1").unwrap(),
            vec![first, second, third]
        );
        assert!(fs::read(path).unwrap().ends_with(b"\n"));
    }

    #[test]
    fn jsonl_load_preserves_a_complete_final_record_without_newline() {
        let dir = tempfile::tempdir().unwrap();
        let first = AgentEvent::new("session_1", 1, AgentEventKind::TurnCancelled);
        let second = AgentEvent::new("session_1", 2, AgentEventKind::TurnCancelled);
        let path = dir.path().join("session_1.jsonl");
        fs::write(&path, serde_json::to_vec(&first).unwrap()).unwrap();
        let store = JsonlSessionStore::new(dir.path()).unwrap();

        assert_eq!(store.load("session_1").unwrap(), vec![first.clone()]);
        store.append(&second).unwrap();
        assert_eq!(store.load("session_1").unwrap(), vec![first, second]);
        assert_eq!(
            fs::read(path)
                .unwrap()
                .iter()
                .filter(|byte| **byte == b'\n')
                .count(),
            2
        );
    }

    #[test]
    fn jsonl_rejects_internal_corruption_and_inconsistent_identity_or_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let first = AgentEvent::new("session_1", 1, AgentEventKind::TurnCancelled);
        let next = AgentEvent::new("session_1", 2, AgentEventKind::TurnCancelled);
        let wrong_session = AgentEvent::new("session_2", 2, AgentEventKind::TurnCancelled);
        let skipped = AgentEvent::new("session_1", 3, AgentEventKind::TurnCancelled);
        let cases = [
            (b"{broken}".to_vec(), "invalid json"),
            (
                serde_json::to_vec(&wrong_session).unwrap(),
                "session id mismatch",
            ),
            (
                serde_json::to_vec(&first).unwrap(),
                "event sequence is not contiguous",
            ),
            (
                serde_json::to_vec(&skipped).unwrap(),
                "event sequence is not contiguous",
            ),
        ];

        for (index, (middle, expected)) in cases.into_iter().enumerate() {
            let session_id = format!("session_{index}");
            let mut prefix = first.clone();
            prefix.session_id = session_id.clone();
            let mut suffix = next.clone();
            suffix.session_id = session_id.clone();
            let middle = if index == 0 {
                middle
            } else {
                let mut event: AgentEvent = serde_json::from_slice(&middle).unwrap();
                if index != 1 {
                    event.session_id = session_id.clone();
                }
                serde_json::to_vec(&event).unwrap()
            };
            let mut bytes = serde_json::to_vec(&prefix).unwrap();
            bytes.push(b'\n');
            bytes.extend(middle);
            bytes.push(b'\n');
            bytes.extend(serde_json::to_vec(&suffix).unwrap());
            bytes.push(b'\n');
            let path = dir.path().join(format!("{session_id}.jsonl"));
            fs::write(&path, &bytes).unwrap();
            let store = JsonlSessionStore::new(dir.path()).unwrap();

            let error = store.load(&session_id).unwrap_err();
            if expected == "invalid json" {
                assert!(matches!(error, StoreError::Json(_)));
            } else {
                assert!(matches!(
                    error,
                    StoreError::InconsistentEventLog { line: 2, reason, .. } if reason == expected
                ));
            }
            assert_eq!(fs::read(&path).unwrap(), bytes);
            assert!(store.append(&suffix).is_err());
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn session_store_lists_and_loads_events() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("/workspace", "mock-model").unwrap();
        let event = AgentEvent::new(&session.id, 1, AgentEventKind::TurnCancelled);
        store.append_event(&session.id, &event).unwrap();
        let loaded = store.load(&session.id).unwrap().unwrap();
        assert_eq!(loaded.session.id, session.id);
        assert_eq!(loaded.events, vec![event]);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn native_admission_survives_reopen_and_retry_without_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let first = store
            .admit_native_turn(
                &session.id,
                "turn-1",
                "Inspect files",
                "model-a",
                TurnMode::Plan,
            )
            .unwrap();
        assert_eq!(first.sequence, 1);
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        let retried = reopened
            .admit_native_turn(
                &session.id,
                "turn-1",
                "Inspect files",
                "model-a",
                TurnMode::Plan,
            )
            .unwrap();
        assert_eq!(retried, first);
        assert_eq!(
            reopened.load(&session.id).unwrap().unwrap().events,
            vec![first]
        );
    }

    #[test]
    fn native_admission_uses_next_sequence_and_rejects_conflicting_retry() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        store
            .append_event(
                &session.id,
                &AgentEvent::new(&session.id, 7, AgentEventKind::TurnCancelled),
            )
            .unwrap();
        let admitted = store
            .admit_native_turn(
                &session.id,
                "turn-1",
                "Build it",
                "model-a",
                TurnMode::Build,
            )
            .unwrap();
        assert_eq!(admitted.sequence, 8);
        store
            .append_event(
                &session.id,
                &AgentEvent::new(
                    &session.id,
                    9,
                    AgentEventKind::TurnStarted {
                        prompt: "Build it".into(),
                    },
                ),
            )
            .unwrap();
        assert!(matches!(
            store.admit_native_turn(&session.id, "turn-1", "Build it", "model-a", TurnMode::Plan),
            Err(StoreError::ConflictingTurnAdmission(id)) if id == "turn-1"
        ));
        let next = store
            .admit_native_turn(&session.id, "turn-2", "Continue", "model-b", TurnMode::Plan)
            .unwrap();
        assert_eq!(next.sequence, 10);
        assert_eq!(store.load(&session.id).unwrap().unwrap().events.len(), 4);
    }

    #[test]
    fn native_admission_does_not_change_cli_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("/workspace", "codex").unwrap();
        assert!(matches!(
            store.admit_native_turn(&session.id, "turn-1", "Prompt", "model-a", TurnMode::Build),
            Err(StoreError::NativeTurnRequiresNativeSession)
        ));
        assert!(store.load(&session.id).unwrap().unwrap().events.is_empty());
    }

    #[test]
    fn native_lease_excludes_other_store_handles_and_reacquires_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first_store = SessionStore::open(dir.path()).unwrap();
        let second_store = SessionStore::open(dir.path()).unwrap();
        let first = first_store.create("/workspace", "native").unwrap();
        let second = first_store.create("/workspace", "native").unwrap();

        let lease = first_store.try_acquire_native_lease(&first.id).unwrap();
        assert_eq!(lease.session_id(), first.id);
        assert!(matches!(
            first_store.try_acquire_native_lease(&first.id),
            Err(StoreError::NativeSessionBusy(id)) if id == first.id
        ));
        assert!(matches!(
            second_store.try_acquire_native_lease(&first.id),
            Err(StoreError::NativeSessionBusy(id)) if id == first.id
        ));
        let other_lease = second_store.try_acquire_native_lease(&second.id).unwrap();
        drop(first_store);
        assert!(matches!(
            second_store.try_acquire_native_lease(&first.id),
            Err(StoreError::NativeSessionBusy(id)) if id == first.id
        ));

        drop(lease);
        let reacquired = second_store.try_acquire_native_lease(&first.id).unwrap();
        assert_eq!(reacquired.session_id(), first.id);
        assert!(dir
            .path()
            .join(".native-leases")
            .join(format!("{}.lock", first.id))
            .exists());
        drop(other_lease);
        drop(reacquired);
    }

    #[test]
    fn native_lease_requires_an_existing_native_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let cli = store.create("/workspace", "codex").unwrap();
        assert!(matches!(
            store.try_acquire_native_lease(&cli.id),
            Err(StoreError::NativeTurnRequiresNativeSession)
        ));
        assert!(matches!(
            store.try_acquire_native_lease("unknown"),
            Err(StoreError::UnknownSession(id)) if id == "unknown"
        ));
        assert!(matches!(
            store.try_acquire_native_lease("../escape"),
            Err(StoreError::InvalidSessionId)
        ));
    }

    #[test]
    fn native_child_lineage_is_atomic_and_bound_to_parent_lease() {
        assert_ne!(child_session_id("a", "bc"), child_session_id("ab", "c"));
        let root = tempfile::tempdir().unwrap();
        let other_root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let other_store = SessionStore::open(other_root.path()).unwrap();
        let parent = store.create("/workspace", "native").unwrap();
        let parent_lease = store.try_acquire_native_lease(&parent.id).unwrap();
        let request_digest = "a".repeat(64);
        assert!(matches!(
            store.create_native_child(&parent_lease, "../escape", &request_digest, 2),
            Err(StoreError::InvalidSubagentSpawnId)
        ));
        assert!(matches!(
            store.create_native_child(&parent_lease, "spawn-1", "short", 2),
            Err(StoreError::InvalidSubagentRequestDigest)
        ));
        let child = store
            .create_native_child(&parent_lease, "spawn-1", &request_digest, 2)
            .unwrap();
        assert_eq!(child.workspace, parent.workspace);
        assert_eq!(
            child.subagent,
            Some(SubagentLink {
                parent_session_id: parent.id.clone(),
                depth: 1,
                spawn_id: "spawn-1".into(),
                request_digest: request_digest.clone(),
            })
        );
        let saved = SessionStore::open(root.path())
            .unwrap()
            .load(&child.id)
            .unwrap()
            .unwrap()
            .session;
        assert_eq!(saved, child);
        assert_eq!(
            store
                .create_native_child(&parent_lease, "spawn-1", &request_digest, 2)
                .unwrap(),
            child
        );
        let same_root_store = SessionStore::open(root.path()).unwrap();
        std::thread::scope(|scope| {
            let first = scope
                .spawn(|| store.create_native_child(&parent_lease, "spawn-1", &request_digest, 2));
            let second = scope.spawn(|| {
                same_root_store.create_native_child(&parent_lease, "spawn-1", &request_digest, 2)
            });
            assert_eq!(first.join().unwrap().unwrap(), child);
            assert_eq!(second.join().unwrap().unwrap(), child);
        });
        assert!(matches!(
            store.create_native_child(&parent_lease, "spawn-1", &"b".repeat(64), 2),
            Err(StoreError::ConflictingSubagentIdentity)
        ));

        let child_lease = store.try_acquire_native_lease(&child.id).unwrap();
        let grandchild = store
            .create_native_child(&child_lease, "spawn-2", &request_digest, 2)
            .unwrap();
        assert_eq!(grandchild.subagent.as_ref().unwrap().depth, 2);
        assert!(matches!(
            store.create_native_child(&child_lease, "spawn-3", &request_digest, 1),
            Err(StoreError::SubagentDepthExceeded)
        ));
        assert!(matches!(
            other_store.create_native_child(&parent_lease, "spawn-3", &request_digest, 2),
            Err(StoreError::NativeLeaseStoreMismatch)
        ));
    }

    #[test]
    fn direct_deletion_preserves_subagent_lineage_and_sidecars() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let parent = store.create("/workspace", "native").unwrap();
        let lease = store.try_acquire_native_lease(&parent.id).unwrap();
        let child = store
            .create_native_child(&lease, "spawn-1", &"a".repeat(64), 2)
            .unwrap();
        drop(lease);
        store
            .save_draft(&parent.workspace, Some(&parent.id), "parent draft")
            .unwrap();
        store
            .save_draft(&child.workspace, Some(&child.id), "child draft")
            .unwrap();
        let registry_path = root.path().join("subagents.registry.json");
        fs::write(&registry_path, b"registry sentinel").unwrap();

        assert!(matches!(
            store.delete(&parent.id),
            Err(StoreError::SessionHasSubagent { parent: id, child: child_id })
                if id == parent.id && child_id == child.id
        ));
        assert!(matches!(
            store.delete(&child.id),
            Err(StoreError::CannotDeleteSubagentDirectly(id)) if id == child.id
        ));
        assert!(store.load(&parent.id).unwrap().is_some());
        assert!(store.load(&child.id).unwrap().is_some());
        assert_eq!(
            store
                .load_draft(&parent.workspace, Some(&parent.id))
                .unwrap()
                .as_deref(),
            Some("parent draft")
        );
        assert_eq!(
            store
                .load_draft(&child.workspace, Some(&child.id))
                .unwrap()
                .as_deref(),
            Some("child draft")
        );
        assert_eq!(fs::read(registry_path).unwrap(), b"registry sentinel");
    }

    #[test]
    fn native_deletion_holds_worker_lease_and_fails_closed_on_unreadable_lineage() {
        let root = tempfile::tempdir().unwrap();
        let first_store = SessionStore::open(root.path()).unwrap();
        let second_store = SessionStore::open(root.path()).unwrap();
        let native = first_store.create("/workspace", "native").unwrap();
        let cli = first_store.create("/workspace", "codex").unwrap();
        let lease = first_store.try_acquire_native_lease(&native.id).unwrap();
        assert!(matches!(
            second_store.delete(&native.id),
            Err(StoreError::NativeSessionBusy(id)) if id == native.id
        ));
        assert!(second_store.delete(&cli.id).unwrap());
        drop(lease);

        let broken = root.path().join("broken.session.json");
        fs::write(&broken, b"{").unwrap();
        assert!(matches!(
            second_store.delete(&native.id),
            Err(StoreError::Json(_))
        ));
        assert!(second_store.load(&native.id).unwrap().is_some());
        fs::remove_file(broken).unwrap();
        assert!(second_store.delete(&native.id).unwrap());
    }

    #[test]
    fn deleting_orphaned_parent_log_keeps_evidence_for_existing_child() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let parent = store.create("/workspace", "native").unwrap();
        let lease = store.try_acquire_native_lease(&parent.id).unwrap();
        let child = store
            .create_native_child(&lease, "spawn-1", &"b".repeat(64), 2)
            .unwrap();
        drop(lease);
        let log = root.path().join(format!("{}.jsonl", parent.id));
        fs::write(&log, b"parent evidence\n").unwrap();
        fs::remove_file(root.path().join(format!("{}.session.json", parent.id))).unwrap();

        assert!(matches!(
            store.delete(&parent.id),
            Err(StoreError::SessionHasSubagent { child: id, .. }) if id == child.id
        ));
        assert_eq!(fs::read(log).unwrap(), b"parent evidence\n");
    }

    #[test]
    fn native_tree_delete_removes_terminal_lineage_and_preserves_other_state() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("keep.txt"), b"workspace evidence").unwrap();
        let store = SessionStore::open(root.path().join("sessions")).unwrap();
        let parent = store.create(&workspace, "native").unwrap();
        let other = store.create(&workspace, "native").unwrap();
        let parent_lease = store.try_acquire_native_lease(&parent.id).unwrap();
        let child = store
            .create_native_child(&parent_lease, "spawn-1", &"a".repeat(64), 3)
            .unwrap();
        let child_lease = store.try_acquire_native_lease(&child.id).unwrap();
        let grandchild = store
            .create_native_child(&child_lease, "spawn-2", &"b".repeat(64), 3)
            .unwrap();
        drop(child_lease);
        drop(parent_lease);
        let registry = serde_json::json!({
            "version": 2,
            "children": {
                child.id.clone(): {"status": "completed", "cancellation_requested": false},
                grandchild.id.clone(): {"status": "failed", "cancellation_requested": false},
                "unrelated": {"status": "cancelled", "cancellation_requested": true}
            },
            "inputs": {child.id.clone(): [{"state": "done"}]}
        });
        store
            .write_json_atomically(
                store.storage_root().join("subagents.registry.json"),
                "subagents.registry",
                &registry,
            )
            .unwrap();
        for session in [&parent, &child, &grandchild] {
            store
                .save_draft(&workspace, Some(&session.id), "delete draft")
                .unwrap();
            fs::write(store.events.path(&session.id).unwrap(), b"event evidence\n").unwrap();
            fs::write(
                store.permission_journal_path(&session.id).unwrap(),
                b"permission evidence",
            )
            .unwrap();
        }
        store
            .save_draft(&workspace, Some(&other.id), "keep draft")
            .unwrap();

        assert!(store.delete_native_tree(&parent.id).unwrap());
        assert!(!store.delete_native_tree(&parent.id).unwrap());
        assert!(store.pending_native_deletion_roots().unwrap().is_empty());
        for session in [&parent, &child, &grandchild] {
            assert!(store.load(&session.id).unwrap().is_none());
            assert!(!store.events.path(&session.id).unwrap().exists());
            assert!(!store.permission_journal_path(&session.id).unwrap().exists());
            assert!(store
                .load_draft(&workspace, Some(&session.id))
                .unwrap()
                .is_none());
        }
        assert!(store.load(&other.id).unwrap().is_some());
        assert_eq!(
            store
                .load_draft(&workspace, Some(&other.id))
                .unwrap()
                .as_deref(),
            Some("keep draft")
        );
        assert_eq!(
            fs::read(workspace.join("keep.txt")).unwrap(),
            b"workspace evidence"
        );
        let saved_registry = store.read_subagent_registry().unwrap().unwrap();
        assert!(saved_registry["children"].get(&child.id).is_none());
        assert!(saved_registry["children"].get(&grandchild.id).is_none());
        assert!(saved_registry["inputs"].get(&child.id).is_none());
        assert!(saved_registry["children"].get("unrelated").is_some());
    }

    #[test]
    fn native_tree_delete_rejects_live_or_queued_children_without_mutation() {
        let root = tempfile::tempdir().unwrap();
        let first_store = SessionStore::open(root.path()).unwrap();
        let second_store = SessionStore::open(root.path()).unwrap();
        let parent = first_store.create("/workspace", "native").unwrap();
        let parent_lease = first_store.try_acquire_native_lease(&parent.id).unwrap();
        let child = first_store
            .create_native_child(&parent_lease, "spawn-1", &"a".repeat(64), 2)
            .unwrap();
        drop(parent_lease);
        let child_lease = first_store.try_acquire_native_lease(&child.id).unwrap();
        assert!(matches!(
            second_store.delete_native_tree(&parent.id),
            Err(StoreError::NativeSessionBusy(id)) if id == child.id
        ));
        drop(child_lease);
        let registry_path = root.path().join("subagents.registry.json");
        fs::write(
            &registry_path,
            serde_json::to_vec(&serde_json::json!({
                "version": 2,
                "children": {child.id.clone(): {"status": "interrupted", "cancellation_requested": false}},
                "inputs": {}
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            second_store.delete_native_tree(&parent.id),
            Err(StoreError::NonterminalSubagent { child: id, status })
                if id == child.id && status == "interrupted"
        ));
        fs::write(
            &registry_path,
            serde_json::to_vec(&serde_json::json!({
                "version": 2,
                "children": {child.id.clone(): {"status": "completed", "cancellation_requested": false}},
                "inputs": {child.id.clone(): [{"state": "pending"}]}
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            second_store.delete_native_tree(&parent.id),
            Err(StoreError::PendingSubagentInput(id)) if id == child.id
        ));
        assert!(first_store.load(&parent.id).unwrap().is_some());
        assert!(first_store.load(&child.id).unwrap().is_some());
        assert!(second_store
            .pending_native_deletion_roots()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn native_tree_delete_recovers_after_partial_cleanup_and_blocks_new_work() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let parent = store.create("/workspace", "native").unwrap();
        let parent_lease = store.try_acquire_native_lease(&parent.id).unwrap();
        let child = store
            .create_native_child(&parent_lease, "spawn-1", &"a".repeat(64), 2)
            .unwrap();
        let intent = NativeDeleteIntent {
            version: NATIVE_DELETE_INTENT_VERSION,
            root_id: parent.id.clone(),
            nodes: vec![
                NativeDeleteNode {
                    id: parent.id.clone(),
                    parent_id: None,
                    spawn_id: None,
                },
                NativeDeleteNode {
                    id: child.id.clone(),
                    parent_id: Some(parent.id.clone()),
                    spawn_id: Some("spawn-1".into()),
                },
            ],
        };
        fs::create_dir_all(store.native_delete_intent_directory()).unwrap();
        store
            .write_json_atomically(
                store.native_delete_intent_path(&parent.id).unwrap(),
                &format!("native-delete.{}", parent.id),
                &intent,
            )
            .unwrap();
        assert!(matches!(
            store.create_native_child(&parent_lease, "spawn-2", &"b".repeat(64), 2),
            Err(StoreError::NativeDeletionPending(id)) if id == parent.id
        ));
        drop(parent_lease);
        assert!(matches!(
            store.try_acquire_native_lease(&child.id),
            Err(StoreError::NativeDeletionPending(id)) if id == child.id
        ));
        assert!(matches!(
            store.admit_native_turn(&parent.id, "turn-1", "prompt", "model", TurnMode::Build),
            Err(StoreError::NativeDeletionPending(id)) if id == parent.id
        ));
        fs::remove_file(store.metadata_path(&child.id).unwrap()).unwrap();
        fs::write(
            root.path().join("subagents.registry.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 2,
                "children": {child.id.clone(): {"status": "completed", "cancellation_requested": false}},
                "inputs": {}
            }))
            .unwrap(),
        )
        .unwrap();
        let reopened = SessionStore::open(root.path()).unwrap();
        assert_eq!(
            reopened.pending_native_deletion_roots().unwrap(),
            vec![parent.id.clone()]
        );
        assert_eq!(reopened.recover_pending_native_deletions().unwrap(), 1);
        assert!(reopened.load(&parent.id).unwrap().is_none());
        assert!(
            reopened.read_subagent_registry().unwrap().unwrap()["children"]
                .get(&child.id)
                .is_none()
        );
        assert_eq!(reopened.recover_pending_native_deletions().unwrap(), 0);
    }

    #[test]
    fn native_tree_delete_serializes_two_store_handles() {
        use std::sync::{Arc, Barrier};

        let root = tempfile::tempdir().unwrap();
        let first = SessionStore::open(root.path()).unwrap();
        let second = SessionStore::open(root.path()).unwrap();
        let parent = first.create("/workspace", "native").unwrap();
        let lease = first.try_acquire_native_lease(&parent.id).unwrap();
        let child = first
            .create_native_child(&lease, "spawn-1", &"a".repeat(64), 2)
            .unwrap();
        drop(lease);
        fs::write(
            root.path().join("subagents.registry.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 2,
                "children": {child.id.clone(): {"status": "completed", "cancellation_requested": false}},
                "inputs": {}
            }))
            .unwrap(),
        )
        .unwrap();

        let barrier = Arc::new(Barrier::new(3));
        let first_id = parent.id.clone();
        let first_barrier = Arc::clone(&barrier);
        let first_worker = std::thread::spawn(move || {
            first_barrier.wait();
            first.delete_native_tree(&first_id)
        });
        let second_id = parent.id.clone();
        let second_barrier = Arc::clone(&barrier);
        let second_worker = std::thread::spawn(move || {
            second_barrier.wait();
            second.delete_native_tree(&second_id)
        });
        barrier.wait();
        let results = [
            first_worker.join().unwrap().unwrap(),
            second_worker.join().unwrap().unwrap(),
        ];
        assert_eq!(results.iter().filter(|&&deleted| deleted).count(), 1);
        let reopened = SessionStore::open(root.path()).unwrap();
        assert!(reopened.load(&parent.id).unwrap().is_none());
        assert!(reopened.load(&child.id).unwrap().is_none());
        assert!(reopened.pending_native_deletion_roots().unwrap().is_empty());
    }

    #[test]
    fn native_tree_delete_waits_for_journal_before_publishing_intent() {
        use std::sync::{Arc, Barrier};
        use std::time::Duration;

        let root = tempfile::tempdir().unwrap();
        let writer = SessionStore::open(root.path()).unwrap();
        let deleter = SessionStore::open(root.path()).unwrap();
        let session = writer.create("/workspace", "native").unwrap();
        let transaction = writer.lock_native_journal(&session.id).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        let id = session.id.clone();
        let delete_worker = std::thread::spawn(move || {
            worker_barrier.wait();
            deleter.delete_native_tree(&id)
        });
        barrier.wait();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!writer
            .native_delete_intent_path(&session.id)
            .unwrap()
            .exists());
        drop(transaction);
        assert!(delete_worker.join().unwrap().unwrap());
        assert!(writer.load(&session.id).unwrap().is_none());
    }

    #[test]
    fn native_tree_delete_finishes_after_root_metadata_was_removed() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let parent = store.create("/workspace", "native").unwrap();
        let intent = NativeDeleteIntent {
            version: NATIVE_DELETE_INTENT_VERSION,
            root_id: parent.id.clone(),
            nodes: vec![NativeDeleteNode {
                id: parent.id.clone(),
                parent_id: None,
                spawn_id: None,
            }],
        };
        fs::create_dir_all(store.native_delete_intent_directory()).unwrap();
        store
            .write_json_atomically(
                store.native_delete_intent_path(&parent.id).unwrap(),
                &format!("native-delete.{}", parent.id),
                &intent,
            )
            .unwrap();
        fs::remove_file(store.metadata_path(&parent.id).unwrap()).unwrap();
        fs::write(store.events.path(&parent.id).unwrap(), b"tail\n").unwrap();

        let reopened = SessionStore::open(root.path()).unwrap();
        assert_eq!(reopened.recover_pending_native_deletions().unwrap(), 1);
        assert!(!reopened.events.path(&parent.id).unwrap().exists());
        assert!(reopened.pending_native_deletion_roots().unwrap().is_empty());
        assert!(!reopened.delete_native_tree(&parent.id).unwrap());
    }

    #[test]
    fn native_tree_delete_rejects_forged_lineage_intent() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let parent = store.create("/workspace", "native").unwrap();
        let unrelated = store.create("/workspace", "native").unwrap();
        let intent = NativeDeleteIntent {
            version: NATIVE_DELETE_INTENT_VERSION,
            root_id: parent.id.clone(),
            nodes: vec![
                NativeDeleteNode {
                    id: parent.id.clone(),
                    parent_id: None,
                    spawn_id: None,
                },
                NativeDeleteNode {
                    id: unrelated.id.clone(),
                    parent_id: Some(parent.id.clone()),
                    spawn_id: Some("forged".into()),
                },
            ],
        };
        fs::create_dir_all(store.native_delete_intent_directory()).unwrap();
        store
            .write_json_atomically(
                store.native_delete_intent_path(&parent.id).unwrap(),
                &format!("native-delete.{}", parent.id),
                &intent,
            )
            .unwrap();
        assert!(matches!(
            store.recover_pending_native_deletions(),
            Err(StoreError::InvalidNativeDeletionIntent(id)) if id == parent.id
        ));
        assert!(store.load(&parent.id).unwrap().is_some());
        assert!(store.load(&unrelated.id).unwrap().is_some());
    }

    #[test]
    fn native_journal_orders_events_and_replays_idempotently_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let admitted = store
            .admit_native_turn(&session.id, "turn-1", "Inspect", "model-a", TurnMode::Plan)
            .unwrap();
        let started = AgentEvent::new(
            &session.id,
            admitted.sequence + 1,
            AgentEventKind::TurnStarted {
                prompt: "Inspect".into(),
            },
        );
        store.append_native_event(&started).unwrap();
        store.append_native_event(&started).unwrap();
        let skipped = AgentEvent::new(
            &session.id,
            started.sequence + 2,
            AgentEventKind::TurnCancelled,
        );
        assert!(matches!(
            store.append_native_event(&skipped),
            Err(StoreError::NativeEventOutOfOrder { previous, incoming })
                if previous == started.sequence && incoming == skipped.sequence
        ));
        let conflict =
            AgentEvent::new(&session.id, started.sequence, AgentEventKind::TurnCancelled);
        assert!(matches!(
            store.append_native_event(&conflict),
            Err(StoreError::ConflictingNativeEvent(sequence)) if sequence == started.sequence
        ));
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        reopened.append_native_event(&started).unwrap();
        let finished = AgentEvent::new(
            &session.id,
            started.sequence + 1,
            AgentEventKind::TurnCompleted {
                response: "Done".into(),
                steps: 1,
            },
        );
        reopened.append_native_event(&finished).unwrap();
        assert_eq!(
            reopened.load(&session.id).unwrap().unwrap().events,
            vec![admitted, started, finished]
        );
    }

    #[test]
    fn two_store_handles_racing_native_writes_keep_one_contiguous_journal() {
        use std::sync::{Arc, Barrier};

        let root = tempfile::tempdir().unwrap();
        let first = SessionStore::open(root.path()).unwrap();
        let session = first.create("/workspace", "native").unwrap();
        let second = SessionStore::open(root.path()).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let id = session.id.clone();
        let first_barrier = Arc::clone(&barrier);
        let first_worker = std::thread::spawn(move || {
            first_barrier.wait();
            first.admit_native_turn(&id, "turn-1", "Prompt", "model", TurnMode::Build)
        });
        let id = session.id.clone();
        let second_barrier = Arc::clone(&barrier);
        let second_worker = std::thread::spawn(move || {
            second_barrier.wait();
            second.admit_native_turn(&id, "turn-1", "Prompt", "model", TurnMode::Build)
        });
        barrier.wait();
        let admitted = first_worker.join().unwrap().unwrap();
        assert_eq!(second_worker.join().unwrap().unwrap(), admitted);

        let first = SessionStore::open(root.path()).unwrap();
        let second = SessionStore::open(root.path()).unwrap();
        // Both handles cache the same tail before racing on sequence two.
        first
            .admit_native_turn(&session.id, "turn-1", "Prompt", "model", TurnMode::Build)
            .unwrap();
        second
            .admit_native_turn(&session.id, "turn-1", "Prompt", "model", TurnMode::Build)
            .unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let id = session.id.clone();
        let first_barrier = Arc::clone(&barrier);
        let first_worker = std::thread::spawn(move || {
            let event = AgentEvent::new(
                id,
                admitted.sequence + 1,
                AgentEventKind::TurnStarted {
                    prompt: "first".into(),
                },
            );
            first_barrier.wait();
            first.append_native_event(&event)
        });
        let id = session.id.clone();
        let second_barrier = Arc::clone(&barrier);
        let second_worker = std::thread::spawn(move || {
            let event = AgentEvent::new(
                id,
                admitted.sequence + 1,
                AgentEventKind::TurnStarted {
                    prompt: "second".into(),
                },
            );
            second_barrier.wait();
            second.append_native_event(&event)
        });
        barrier.wait();
        let results = [first_worker.join().unwrap(), second_worker.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(StoreError::ConflictingNativeEvent(2))))
                .count(),
            1
        );
        let events = SessionStore::open(root.path())
            .unwrap()
            .load(&session.id)
            .unwrap()
            .unwrap()
            .events;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], admitted);
        assert_eq!(events[1].sequence, admitted.sequence + 1);
    }

    #[test]
    fn native_journal_child_process_helper() {
        let Some(root) = std::env::var_os("OPENIDE_CORE_JOURNAL_CHILD_ROOT") else {
            return;
        };
        let session_id = std::env::var("OPENIDE_CORE_JOURNAL_CHILD_SESSION").unwrap();
        let ready = std::env::var_os("OPENIDE_CORE_JOURNAL_CHILD_READY").unwrap();
        let done = std::env::var_os("OPENIDE_CORE_JOURNAL_CHILD_DONE").unwrap();
        let store = SessionStore::open(root).unwrap();
        fs::write(ready, b"ready").unwrap();
        let result = store.append_native_event(&AgentEvent::new(
            session_id,
            2,
            AgentEventKind::TurnStarted {
                prompt: "child".into(),
            },
        ));
        fs::write(done, format!("{result:?}")).unwrap();
        result.unwrap();
    }

    #[test]
    fn native_journal_lock_blocks_a_second_process_and_reloads_its_tail() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let admitted = store
            .admit_native_turn(&session.id, "turn-1", "Prompt", "model", TurnMode::Build)
            .unwrap();
        let ready = root.path().join("child-ready");
        let done = root.path().join("child-done");
        let transaction = store.lock_native_journal(&session.id).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("native_journal_child_process_helper")
            .arg("--nocapture")
            .env("OPENIDE_CORE_JOURNAL_CHILD_ROOT", root.path())
            .env("OPENIDE_CORE_JOURNAL_CHILD_SESSION", &session.id)
            .env("OPENIDE_CORE_JOURNAL_CHILD_READY", &ready)
            .env("OPENIDE_CORE_JOURNAL_CHILD_DONE", &done)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "child did not reach the journal append"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(250));
        assert!(
            !done.exists(),
            "child bypassed the journal transaction lock"
        );
        drop(transaction);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("child remained blocked after journal lock release");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "child append failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read_to_string(done).unwrap(), "Ok(())");
        assert!(matches!(
            store.append_native_event(&AgentEvent::new(
                &session.id,
                admitted.sequence + 1,
                AgentEventKind::TurnStarted {
                    prompt: "parent".into(),
                },
            )),
            Err(StoreError::ConflictingNativeEvent(2))
        ));
        let events = store.load(&session.id).unwrap().unwrap().events;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], admitted);
        assert_eq!(events[1].sequence, 2);
    }

    #[test]
    fn deleting_and_recreating_an_id_invalidates_same_length_cached_tail() {
        let root = tempfile::tempdir().unwrap();
        let first = SessionStore::open(root.path()).unwrap();
        let second = SessionStore::open(root.path()).unwrap();
        let session = first.create("/workspace", "native").unwrap();
        let admitted = first
            .admit_native_turn(&session.id, "old-turn", "Prompt", "model", TurnMode::Build)
            .unwrap();
        first
            .append_native_event(&AgentEvent::new(
                &session.id,
                admitted.sequence + 1,
                AgentEventKind::TurnStarted {
                    prompt: "old".into(),
                },
            ))
            .unwrap();
        assert!(second.delete(&session.id).unwrap());
        // A deterministic child ID may be recreated after a native tree is
        // deleted. Recreate that metadata here to exercise the same journal
        // lifecycle without depending on the child registry.
        second.write_metadata_atomically(&session).unwrap();
        second
            .admit_native_turn(&session.id, "new-turn", "Prompt", "model", TurnMode::Build)
            .unwrap();
        let new_len = second.native_journal_len(&session.id).unwrap();
        first
            .native_tail
            .lock()
            .unwrap()
            .get_mut(&session.id)
            .unwrap()
            .journal_len = new_len;
        assert!(matches!(
            first.append_native_event(&AgentEvent::new(
                &session.id,
                3,
                AgentEventKind::TurnCompleted {
                    response: "wrong turn".into(),
                    steps: 1,
                },
            )),
            Err(StoreError::NativeEventOutOfOrder {
                previous: 1,
                incoming: 3
            })
        ));
        let events = second.load(&session.id).unwrap().unwrap().events;
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0].kind,
            AgentEventKind::TurnAdmitted { turn_id, .. } if turn_id == "new-turn"
        ));
    }

    #[test]
    fn native_journal_requires_admission_and_rejects_cli_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let native = store.create("/workspace", "native").unwrap();
        let cli = store.create("/workspace", "codex").unwrap();
        for (session, expected_native) in [(&native, true), (&cli, false)] {
            let event = AgentEvent::new(
                &session.id,
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Prompt".into(),
                },
            );
            let result = store.append_native_event(&event);
            if expected_native {
                assert!(matches!(
                    result,
                    Err(StoreError::NativeEventWithoutAdmission)
                ));
            } else {
                assert!(matches!(
                    result,
                    Err(StoreError::NativeTurnRequiresNativeSession)
                ));
            }
            assert!(store.load(&session.id).unwrap().unwrap().events.is_empty());
        }
    }

    #[test]
    fn create_publishes_complete_metadata_and_cleans_failed_temporary_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let created = store.create("/workspace", "native").unwrap();
        let metadata_path = dir.path().join(format!("{}.session.json", created.id));
        let metadata = fs::read_to_string(metadata_path).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&metadata).unwrap(), created);

        let blocked = Session {
            id: "blocked".to_owned(),
            workspace: PathBuf::from("/workspace"),
            backend: "native".to_owned(),
            created_at_ms: 1,
            updated_at_ms: 1,
            title: None,
            pinned: false,
            archived: false,
            unread_at_ms: None,
            last_unread_at_ms: 0,
            subagent: None,
            native_automation: None,
        };
        let blocked_path = dir.path().join("blocked.session.json");
        fs::create_dir(&blocked_path).unwrap();
        assert!(matches!(
            store.write_metadata_atomically(&blocked),
            Err(StoreError::Io(_))
        ));
        assert!(blocked_path.is_dir());
        assert!(!fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn listing_reports_corrupt_metadata_and_event_log_without_losing_good_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let good = store.create("/workspace", "native").unwrap();
        let broken_log = store.create("/workspace", "native").unwrap();
        fs::write(
            dir.path().join("broken_metadata.session.json"),
            b"{\"id\":\"broken_metadata\"",
        )
        .unwrap();
        fs::write(
            dir.path().join(format!("{}.jsonl", broken_log.id)),
            b"{\"session_id\":\n",
        )
        .unwrap();

        let listing = store.list_with_diagnostics().unwrap();
        assert_eq!(listing.sessions.len(), 1);
        assert_eq!(listing.sessions[0].session.id, good.id);
        assert_eq!(listing.diagnostics.len(), 2);
        assert!(listing
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.session_id == "broken_metadata"));
        assert!(listing
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.session_id == broken_log.id));
        assert!(listing
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.error.contains("encoding")));
        assert_eq!(store.list_with_events().unwrap().len(), 1);
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.list_with_diagnostics().unwrap().diagnostics.len(),
            2
        );
        assert_eq!(reopened.list().unwrap().len(), 1);
    }

    #[test]
    fn listing_does_not_hide_general_filesystem_failures() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        store.create("/workspace", "native").unwrap();
        fs::create_dir(dir.path().join("inaccessible.session.json")).unwrap();
        assert!(matches!(
            store.list_with_diagnostics(),
            Err(StoreError::Io(_))
        ));
        assert!(matches!(store.list_with_events(), Err(StoreError::Io(_))));
    }

    #[test]
    fn composer_drafts_persist_by_workspace_and_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let one = Path::new("C:/work/one");
        let two = Path::new("C:/work/two");
        let session = store.create(one, "native").unwrap();
        store.save_draft(one, None, "new task draft").unwrap();
        store
            .save_draft(one, Some(&session.id), "session follow-up")
            .unwrap();
        store.save_draft(two, None, "other workspace").unwrap();
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.load_draft(one, None).unwrap().as_deref(),
            Some("new task draft")
        );
        assert_eq!(
            reopened
                .load_draft(one, Some(&session.id))
                .unwrap()
                .as_deref(),
            Some("session follow-up")
        );
        assert_eq!(
            reopened.load_draft(two, None).unwrap().as_deref(),
            Some("other workspace")
        );
        assert_eq!(reopened.load_draft(two, Some(&session.id)).unwrap(), None);

        reopened.save_draft(one, None, "  ").unwrap();
        assert_eq!(reopened.load_draft(one, None).unwrap(), None);
        assert!(reopened.clear_draft(one, Some(&session.id)).unwrap());
        assert!(!reopened.clear_draft(one, Some(&session.id)).unwrap());
        assert_eq!(
            reopened.load_draft(two, None).unwrap().as_deref(),
            Some("other workspace")
        );
        assert!(matches!(
            reopened.save_draft(one, Some("../escape"), "bad"),
            Err(StoreError::InvalidSessionId)
        ));
    }

    #[test]
    fn composer_draft_write_failure_cleans_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let path = store.draft_path();
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            store.write_drafts_atomically(&DraftFile::default()),
            Err(StoreError::Io(_))
        ));
        assert!(path.is_dir());
        assert!(!fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn deletion_cleans_session_draft_but_keeps_new_task_and_other_session_drafts() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let workspace = Path::new("C:/work/project");
        let removed = store.create(workspace, "native").unwrap();
        let kept = store.create(workspace, "native").unwrap();
        store.save_draft(workspace, None, "new task").unwrap();
        store
            .save_draft(workspace, Some(&removed.id), "remove me")
            .unwrap();
        store
            .save_draft(workspace, Some(&kept.id), "keep me")
            .unwrap();

        assert!(store.delete(&removed.id).unwrap());
        assert_eq!(
            store.load_draft(workspace, Some(&removed.id)).unwrap(),
            None
        );
        assert_eq!(
            store.load_draft(workspace, None).unwrap().as_deref(),
            Some("new task")
        );
        assert_eq!(
            store
                .load_draft(workspace, Some(&kept.id))
                .unwrap()
                .as_deref(),
            Some("keep me")
        );
        store
            .save_draft(workspace, Some(&removed.id), "orphaned draft")
            .unwrap();
        assert!(store.delete(&removed.id).unwrap());
        assert!(!store.delete(&removed.id).unwrap());
    }

    #[test]
    fn failed_draft_cleanup_leaves_session_intact_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let workspace = Path::new("C:/work/project");
        let session = store.create(workspace, "native").unwrap();
        store
            .save_draft(workspace, Some(&session.id), "unsent")
            .unwrap();
        let draft_path = store.draft_path();
        fs::write(&draft_path, b"{\"version\":1,\"entries\":[").unwrap();

        assert!(matches!(
            store.delete(&session.id),
            Err(StoreError::Json(_))
        ));
        assert!(store.load(&session.id).unwrap().is_some());
    }

    #[test]
    fn legacy_metadata_defaults_survive_atomic_updates_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let id = "legacy_session";
        let old_metadata = serde_json::json!({
            "id": id,
            "workspace": "C:/legacy/project",
            "backend": "native",
            "created_at_ms": 10,
            "updated_at_ms": 10
        });
        fs::write(
            dir.path().join(format!("{id}.session.json")),
            old_metadata.to_string(),
        )
        .unwrap();

        let store = SessionStore::open(dir.path()).unwrap();
        let loaded = store.load(id).unwrap().unwrap().session;
        assert_eq!(loaded.title, None);
        assert!(!loaded.pinned);
        assert!(!loaded.archived);
        assert_eq!(loaded.unread_at_ms, None);
        assert_eq!(loaded.last_unread_at_ms, 0);
        store.rename(id, Some("  Legacy renamed  ")).unwrap();
        store.set_pinned(id, true).unwrap();
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        let loaded = reopened.load(id).unwrap().unwrap().session;
        assert_eq!(loaded.title.as_deref(), Some("Legacy renamed"));
        assert!(loaded.pinned);
        assert!(!loaded.archived);
        assert_eq!(loaded.workspace, PathBuf::from("C:/legacy/project"));
        assert_eq!(loaded.backend, "native");
        assert_eq!(loaded.unread_at_ms, None);
        assert_eq!(reopened.list_active().unwrap().len(), 1);
    }

    #[test]
    fn unread_compare_and_clear_preserves_newer_notification() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let created = store.create("/workspace", "native").unwrap();
        let first = store.mark_unread(&created.id).unwrap();
        let first_marker = first.unread_at_ms.unwrap();
        assert_eq!(first.updated_at_ms, created.updated_at_ms);
        let second = store.mark_unread(&created.id).unwrap();
        let second_marker = second.unread_at_ms.unwrap();
        assert!(second_marker > first_marker);
        let (unchanged, cleared) = store
            .clear_unread_if_matches(&created.id, first_marker)
            .unwrap();
        assert!(!cleared);
        assert_eq!(unchanged.unread_at_ms, Some(second_marker));

        let (cleared_session, cleared) = store
            .clear_unread_if_matches(&created.id, second_marker)
            .unwrap();
        assert!(cleared);
        assert_eq!(cleared_session.unread_at_ms, None);
        assert_eq!(cleared_session.last_unread_at_ms, second_marker);
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        let third = reopened.mark_unread(&created.id).unwrap();
        assert!(third.unread_at_ms.unwrap() > second_marker);
        assert_eq!(third.updated_at_ms, created.updated_at_ms);
    }

    fn approval_grant(session_id: &str, key_digest: &str) -> SessionApprovalGrantRecord {
        SessionApprovalGrantRecord {
            version: SESSION_APPROVAL_GRANT_RECORD_VERSION,
            session_id: session_id.into(),
            tool_name: "run_command".into(),
            effective_policy_digest: "a".repeat(64),
            key_digest: key_digest.into(),
        }
    }

    #[test]
    fn permission_decisions_are_durable_idempotent_and_separate_from_agent_events() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path()).unwrap());
        let session = store.create("/workspace", "native").unwrap();
        let first = store
            .record_permission_decision(&session.id, "request-1", PermissionDecision::Allow)
            .unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let store = Arc::clone(&store);
                let session_id = &session.id;
                let expected = &first;
                scope.spawn(move || {
                    assert_eq!(
                        store
                            .record_permission_decision(
                                session_id,
                                "request-1",
                                PermissionDecision::Allow,
                            )
                            .unwrap(),
                        *expected
                    );
                });
            }
        });
        assert!(matches!(
            store.record_permission_decision(&session.id, "request-1", PermissionDecision::Deny),
            Err(StoreError::ConflictingPermissionDecision(_))
        ));
        // A question's answer is recorded with the same idempotence.
        let answered = store
            .record_user_answer(&session.id, "question-1", "{\"answers\":[]}")
            .unwrap();
        assert_eq!(answered.decision, PermissionDecision::Allow);
        assert_eq!(answered.answer.as_deref(), Some("{\"answers\":[]}"));
        assert_eq!(
            store
                .record_user_answer(&session.id, "question-1", "{\"answers\":[]}")
                .unwrap(),
            answered
        );
        assert!(matches!(
            store.record_user_answer(&session.id, "question-1", "other"),
            Err(StoreError::ConflictingPermissionDecision(_))
        ));
        assert!(matches!(
            store.record_user_answer(
                &session.id,
                "question-2",
                &"x".repeat(MAX_USER_ANSWER_BYTES + 1)
            ),
            Err(StoreError::InvalidUserAnswer)
        ));
        let second = store
            .record_permission_decision(&session.id, "request-2", PermissionDecision::Deny)
            .unwrap();
        assert_eq!(
            store.load_permission_decisions(&session.id).unwrap(),
            vec![first.clone(), answered.clone(), second.clone()]
        );
        assert!(store.load(&session.id).unwrap().unwrap().events.is_empty());
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.load_permission_decisions(&session.id).unwrap(),
            vec![first, answered, second]
        );
        assert!(matches!(
            reopened.record_permission_decision(&session.id, " ", PermissionDecision::Allow),
            Err(StoreError::InvalidPermissionRequestId)
        ));
        assert!(reopened.delete(&session.id).unwrap());
        assert!(reopened
            .load_permission_decisions(&session.id)
            .unwrap()
            .is_empty());
        assert!(!reopened.delete(&session.id).unwrap());
    }

    #[test]
    fn session_approval_grant_reopens_and_retries_only_exact_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let grant = approval_grant(&session.id, &"b".repeat(64));
        let first = store
            .record_permission_decision_with_grant(
                &session.id,
                "request-1",
                PermissionDecision::Allow,
                Some(grant.clone()),
            )
            .unwrap();
        assert_eq!(first.grant, Some(grant.clone()));
        assert_eq!(
            store
                .record_permission_decision_with_grant(
                    &session.id,
                    "request-1",
                    PermissionDecision::Allow,
                    Some(grant.clone()),
                )
                .unwrap(),
            first
        );
        for changed in [None, Some(approval_grant(&session.id, &"c".repeat(64)))] {
            assert!(matches!(
                store.record_permission_decision_with_grant(
                    &session.id,
                    "request-1",
                    PermissionDecision::Allow,
                    changed,
                ),
                Err(StoreError::ConflictingPermissionDecision(id)) if id == "request-1"
            ));
        }
        assert!(matches!(
            store.record_permission_decision(&session.id, "request-1", PermissionDecision::Allow),
            Err(StoreError::ConflictingPermissionDecision(_))
        ));
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.load_permission_decisions(&session.id).unwrap(),
            vec![first]
        );
        assert_eq!(
            reopened.session_approval_grants(&session.id).unwrap(),
            vec![grant]
        );
        assert!(reopened
            .load(&session.id)
            .unwrap()
            .unwrap()
            .events
            .is_empty());
    }

    #[test]
    fn session_approval_revocation_is_durable_and_preserves_decision_history() {
        let dir = tempfile::tempdir().unwrap();
        let first = SessionStore::open(dir.path()).unwrap();
        let second = SessionStore::open(dir.path()).unwrap();
        let session = first.create("/workspace", "native").unwrap();
        let grant = approval_grant(&session.id, &"b".repeat(64));
        for request_id in ["request-1", "request-2"] {
            first
                .record_permission_decision_with_grant(
                    &session.id,
                    request_id,
                    PermissionDecision::Allow,
                    Some(grant.clone()),
                )
                .unwrap();
        }
        assert_eq!(
            second.session_approval_grants(&session.id).unwrap().len(),
            2
        );
        assert!(!second
            .revoke_session_approval_grant(&session.id, &"c".repeat(64))
            .unwrap());
        assert!(second
            .revoke_session_approval_grant(&session.id, &grant.key_digest)
            .unwrap());
        let bytes = fs::read(second.permission_journal_path(&session.id).unwrap()).unwrap();
        let version =
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["version"].as_u64();
        assert_eq!(version, Some(2));
        assert!(!first
            .revoke_session_approval_grant(&session.id, &grant.key_digest)
            .unwrap());
        assert!(first
            .session_approval_grants(&session.id)
            .unwrap()
            .is_empty());
        let decisions = first.load_permission_decisions(&session.id).unwrap();
        assert_eq!(decisions.len(), 2);
        assert!(decisions
            .iter()
            .all(|entry| entry.grant.as_ref() == Some(&grant) && entry.revoked_at_ms.is_some()));
        let retried = first
            .record_permission_decision_with_grant(
                &session.id,
                "request-1",
                PermissionDecision::Allow,
                Some(grant.clone()),
            )
            .unwrap();
        assert!(retried.revoked_at_ms.is_some());
        assert!(first
            .session_approval_grants(&session.id)
            .unwrap()
            .is_empty());
        drop(first);
        let reopened = SessionStore::open(dir.path()).unwrap();
        assert!(reopened
            .session_approval_grants(&session.id)
            .unwrap()
            .is_empty());
        reopened
            .record_permission_decision_with_grant(
                &session.id,
                "request-3",
                PermissionDecision::Allow,
                Some(grant.clone()),
            )
            .unwrap();
        assert_eq!(
            reopened.session_approval_grants(&session.id).unwrap(),
            vec![grant]
        );
    }

    #[test]
    fn session_approval_grant_rejects_malformed_and_legacy_journal_reads() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let valid = approval_grant(&session.id, &"b".repeat(64));
        let invalid = vec![
            SessionApprovalGrantRecord {
                version: 2,
                ..valid.clone()
            },
            SessionApprovalGrantRecord {
                session_id: "other-session".into(),
                ..valid.clone()
            },
            SessionApprovalGrantRecord {
                tool_name: "".into(),
                ..valid.clone()
            },
            SessionApprovalGrantRecord {
                tool_name: "x".repeat(257),
                ..valid.clone()
            },
            SessionApprovalGrantRecord {
                effective_policy_digest: "A".repeat(64),
                ..valid.clone()
            },
            SessionApprovalGrantRecord {
                key_digest: "b".repeat(63),
                ..valid.clone()
            },
        ];
        for grant in invalid {
            assert!(matches!(
                store.record_permission_decision_with_grant(
                    &session.id,
                    "new-request",
                    PermissionDecision::Allow,
                    Some(grant),
                ),
                Err(StoreError::InvalidSessionApprovalGrant)
            ));
        }
        assert!(matches!(
            store.record_permission_decision_with_grant(
                &session.id,
                "new-request",
                PermissionDecision::Deny,
                Some(valid.clone()),
            ),
            Err(StoreError::InvalidSessionApprovalGrant)
        ));
        assert!(store
            .load_permission_decisions(&session.id)
            .unwrap()
            .is_empty());

        let path = store.permission_journal_path(&session.id).unwrap();
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "entries": [{
                    "session_id": session.id,
                    "request_id": "legacy",
                    "decision": "allow",
                    "recorded_at_ms": 1
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let legacy = store.load_permission_decisions(&session.id).unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].grant, None);
        assert!(store
            .session_approval_grants(&session.id)
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .record_permission_decision(&session.id, "legacy", PermissionDecision::Allow)
                .unwrap(),
            legacy[0]
        );

        let malformed = PermissionJournal {
            version: 1,
            entries: vec![RecordedPermissionDecision {
                session_id: session.id.clone(),
                request_id: "bad-grant".into(),
                decision: PermissionDecision::Deny,
                recorded_at_ms: 2,
                grant: Some(valid),
                revoked_at_ms: None,
                answer: None,
            }],
        };
        fs::write(&path, serde_json::to_vec(&malformed).unwrap()).unwrap();
        assert!(matches!(
            store.session_approval_grants(&session.id),
            Err(StoreError::InvalidSessionApprovalGrant)
        ));

        let malformed_revocation = PermissionJournal {
            version: 1,
            entries: vec![RecordedPermissionDecision {
                session_id: session.id.clone(),
                request_id: "bad-revocation".into(),
                decision: PermissionDecision::Deny,
                recorded_at_ms: 2,
                grant: None,
                revoked_at_ms: Some(3),
                answer: None,
            }],
        };
        fs::write(&path, serde_json::to_vec(&malformed_revocation).unwrap()).unwrap();
        assert!(matches!(
            store.session_approval_grants(&session.id),
            Err(StoreError::InvalidSessionApprovalGrant)
        ));
    }

    #[test]
    fn two_store_handles_cannot_lose_or_change_permission_grant() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
        let first = SessionStore::open(dir.path()).unwrap();
        let second = SessionStore::open(dir.path()).unwrap();
        let session = first.create("/workspace", "native").unwrap();
        let grant = approval_grant(&session.id, &"b".repeat(64));
        let barrier = Arc::new(Barrier::new(3));
        let first_barrier = Arc::clone(&barrier);
        let first_id = session.id.clone();
        let first_grant = grant.clone();
        let first_worker = std::thread::spawn(move || {
            first_barrier.wait();
            first.record_permission_decision_with_grant(
                &first_id,
                "same-request",
                PermissionDecision::Allow,
                Some(first_grant),
            )
        });
        let second_barrier = Arc::clone(&barrier);
        let second_id = session.id.clone();
        let second_grant = grant.clone();
        let second_worker = std::thread::spawn(move || {
            second_barrier.wait();
            second.record_permission_decision_with_grant(
                &second_id,
                "same-request",
                PermissionDecision::Allow,
                Some(second_grant),
            )
        });
        barrier.wait();
        assert_eq!(
            first_worker.join().unwrap().unwrap(),
            second_worker.join().unwrap().unwrap()
        );
        let reopened = SessionStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened
                .load_permission_decisions(&session.id)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            reopened.session_approval_grants(&session.id).unwrap(),
            vec![grant]
        );

        let first = SessionStore::open(dir.path()).unwrap();
        let second = SessionStore::open(dir.path()).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let first_barrier = Arc::clone(&barrier);
        let first_id = session.id.clone();
        let first_worker = std::thread::spawn(move || {
            first_barrier.wait();
            first.record_permission_decision_with_grant(
                &first_id,
                "second-request",
                PermissionDecision::Allow,
                Some(approval_grant(&first_id, &"c".repeat(64))),
            )
        });
        let second_barrier = Arc::clone(&barrier);
        let second_id = session.id.clone();
        let second_worker = std::thread::spawn(move || {
            second_barrier.wait();
            second.record_permission_decision_with_grant(
                &second_id,
                "third-request",
                PermissionDecision::Allow,
                Some(approval_grant(&second_id, &"d".repeat(64))),
            )
        });
        barrier.wait();
        first_worker.join().unwrap().unwrap();
        second_worker.join().unwrap().unwrap();
        let records = reopened.load_permission_decisions(&session.id).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(
            records
                .iter()
                .map(|record| record.request_id.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["same-request", "second-request", "third-request"])
        );
    }

    #[test]
    fn permission_journal_publish_failure_leaves_session_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let journal_path = store.permission_journal_path(&session.id).unwrap();
        fs::create_dir(&journal_path).unwrap();
        assert!(matches!(
            store.record_permission_decision(&session.id, "request-1", PermissionDecision::Allow),
            Err(StoreError::Io(_))
        ));
        assert!(store.load(&session.id).unwrap().is_some());
        assert!(!fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
        fs::remove_dir(journal_path).unwrap();
        assert!(store
            .record_permission_decision(&session.id, "request-1", PermissionDecision::Allow)
            .is_ok());
    }

    #[test]
    fn session_management_persists_and_deletion_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("C:/work/project", "codex").unwrap();
        let event = AgentEvent::new(&session.id, 1, AgentEventKind::TurnCancelled);
        store.append_event(&session.id, &event).unwrap();
        let renamed = store.rename(&session.id, Some("A better title")).unwrap();
        assert!(renamed.updated_at_ms > session.updated_at_ms);
        store.set_pinned(&session.id, true).unwrap();
        store.set_archived(&session.id, true).unwrap();
        assert!(store.list_active().unwrap().is_empty());
        assert!(store.list_active_with_events().unwrap().is_empty());
        assert_eq!(store.list_archived().unwrap().len(), 1);
        assert_eq!(
            store.list_archived_with_events().unwrap()[0].events,
            vec![event.clone()]
        );
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        let loaded = reopened.load(&session.id).unwrap().unwrap();
        assert_eq!(loaded.session.title.as_deref(), Some("A better title"));
        assert!(loaded.session.pinned);
        assert!(loaded.session.archived);
        assert_eq!(loaded.events, vec![event.clone()]);

        reopened.rename(&session.id, None).unwrap();
        reopened.set_archived(&session.id, false).unwrap();
        assert_eq!(reopened.list_active().unwrap().len(), 1);
        assert_eq!(
            reopened.load(&session.id).unwrap().unwrap().session.title,
            None
        );
        assert!(reopened.delete(&session.id).unwrap());
        assert!(!reopened.delete(&session.id).unwrap());
        assert!(reopened.load(&session.id).unwrap().is_none());
        assert!(reopened.list().unwrap().is_empty());
        let log_path = dir.path().join(format!("{}.jsonl", session.id));
        assert!(!log_path.exists());
        // A retry still cleans up a log left behind by an interrupted delete.
        fs::write(&log_path, b"orphaned log\n").unwrap();
        assert!(reopened.delete(&session.id).unwrap());
        assert!(!log_path.exists());
        assert!(matches!(
            reopened.append_event(&session.id, &event),
            Err(StoreError::UnknownSession(_))
        ));
        assert!(!fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn metadata_mutations_validate_inputs_and_preserve_independent_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path()).unwrap());
        let session = store.create("C:/work/project", "native").unwrap();
        assert!(matches!(
            store.rename(&session.id, Some("  ")),
            Err(StoreError::InvalidTitle)
        ));
        assert!(matches!(
            store.set_pinned("../escape", true),
            Err(StoreError::InvalidSessionId)
        ));
        assert!(matches!(
            store.delete("../escape"),
            Err(StoreError::InvalidSessionId)
        ));
        assert!(matches!(
            store.set_archived("unknown", true),
            Err(StoreError::UnknownSession(_))
        ));

        std::thread::scope(|scope| {
            let renamed = Arc::clone(&store);
            let id = &session.id;
            scope.spawn(move || {
                for index in 0..8 {
                    renamed.rename(id, Some(&format!("Title {index}"))).unwrap();
                }
            });
            let pinned = Arc::clone(&store);
            scope.spawn(move || {
                for index in 0..8 {
                    pinned.set_pinned(id, index % 2 == 1).unwrap();
                }
            });
        });
        let loaded = store.load(&session.id).unwrap().unwrap().session;
        assert_eq!(loaded.title.as_deref(), Some("Title 7"));
        assert!(loaded.pinned);
    }

    #[test]
    fn session_query_filters_searches_and_paginates_visible_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let main = store.create("C:/work/main", "native").unwrap();
        store
            .append_event(
                &main.id,
                &AgentEvent::new(
                    &main.id,
                    1,
                    AgentEventKind::TurnStarted {
                        prompt: "Investiga navegación\ncon detalle".into(),
                    },
                ),
            )
            .unwrap();
        store
            .append_event(
                &main.id,
                &AgentEvent::new(
                    &main.id,
                    2,
                    AgentEventKind::AssistantMessage {
                        content: "El résumé de las sesiones está listo".into(),
                    },
                ),
            )
            .unwrap();
        store
            .append_event(
                &main.id,
                &AgentEvent::new(
                    &main.id,
                    3,
                    AgentEventKind::ToolFinished {
                        tool_call_id: "tool-1".into(),
                        output: "secreto-interno".into(),
                        is_error: false,
                    },
                ),
            )
            .unwrap();
        let pinned = store.create("C:/work/main", "codex").unwrap();
        store.set_pinned(&pinned.id, true).unwrap();
        store.rename(&pinned.id, Some("Tarea fijada")).unwrap();
        let archived = store.create("C:/work/other", "native").unwrap();
        store.set_archived(&archived.id, true).unwrap();
        store
            .rename(&archived.id, Some("Navegación antigua"))
            .unwrap();

        let base = SessionQuery::default();
        let regular = store.query_sessions(&base).unwrap();
        assert_eq!(regular.total, 1);
        assert_eq!(regular.items[0].session.id, main.id);
        assert_eq!(regular.items[0].title, "Investiga navegación");
        assert!(regular.items[0].search_snippets.is_empty());

        let body = store
            .query_sessions(&SessionQuery {
                search: Some("RÉSUMÉ".into()),
                ..base.clone()
            })
            .unwrap();
        assert_eq!(body.total, 1);
        assert!(body.items[0].search_snippets[0].contains("résumé"));
        assert_eq!(
            store
                .query_sessions(&SessionQuery {
                    search: Some("secreto-interno".into()),
                    ..base.clone()
                })
                .unwrap()
                .total,
            0
        );

        let pinned_result = store
            .query_sessions(&SessionQuery {
                kind: SessionListKind::Pinned,
                search: Some("FIJADA".into()),
                ..base.clone()
            })
            .unwrap();
        assert_eq!(pinned_result.total, 1);
        assert_eq!(pinned_result.items[0].session.id, pinned.id);
        let archived_result = store
            .query_sessions(&SessionQuery {
                kind: SessionListKind::Archived,
                workspaces: vec![PathBuf::from("C:/work/other")],
                search: Some("ANTIGUA".into()),
                ..base.clone()
            })
            .unwrap();
        assert_eq!(archived_result.total, 1);
        assert_eq!(archived_result.items[0].session.id, archived.id);

        let first_page = store
            .query_sessions(&SessionQuery {
                kind: SessionListKind::All,
                limit: 1,
                ..base.clone()
            })
            .unwrap();
        let next_page = store
            .query_sessions(&SessionQuery {
                kind: SessionListKind::All,
                offset: 1,
                limit: 1,
                ..base.clone()
            })
            .unwrap();
        assert_eq!(first_page.total, 3);
        assert!(first_page.has_more);
        assert_ne!(
            first_page.items[0].session.id,
            next_page.items[0].session.id
        );
        assert!(matches!(
            store.query_sessions(&SessionQuery {
                search: Some("x".repeat(257)),
                ..base
            }),
            Err(StoreError::SearchQueryTooLong)
        ));
    }

    #[test]
    fn session_query_reports_corrupt_records_and_limits_snippets() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let prompt = (0..8)
            .map(|index| format!("Coincidencia {index} {}", "x".repeat(150)))
            .collect::<Vec<_>>()
            .join("\n");
        store
            .append_event(
                &session.id,
                &AgentEvent::new(&session.id, 1, AgentEventKind::TurnStarted { prompt }),
            )
            .unwrap();
        fs::write(dir.path().join("broken.session.json"), b"not-json").unwrap();
        let result = store
            .query_sessions(&SessionQuery {
                search: Some("coincidencia".into()),
                ..SessionQuery::default()
            })
            .unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.items[0].search_snippets.len(), 4);
    }

    fn start_steer_test_turn(store: &SessionStore, session_id: &str) {
        store
            .admit_native_turn(
                session_id,
                "turn-1",
                "First prompt",
                "model",
                TurnMode::Build,
            )
            .unwrap();
        store
            .append_native_event(&AgentEvent::new(
                session_id,
                2,
                AgentEventKind::TurnStarted {
                    prompt: "First prompt".into(),
                },
            ))
            .unwrap();
    }

    #[test]
    fn native_steer_two_handles_retry_and_reopen_do_not_duplicate_input() {
        let root = tempfile::tempdir().unwrap();
        let first = SessionStore::open(root.path()).unwrap();
        let second = SessionStore::open(root.path()).unwrap();
        let session = first.create("/workspace", "native").unwrap();
        start_steer_test_turn(&first, &session.id);
        assert_eq!(
            second
                .active_native_turn_id(&session.id)
                .unwrap()
                .as_deref(),
            Some("turn-1")
        );
        std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                first.enqueue_native_steer(&session.id, "turn-1", "input-1", "Second prompt")
            });
            let b = scope.spawn(|| {
                second.enqueue_native_steer(&session.id, "turn-1", "input-1", "Second prompt")
            });
            assert_eq!(a.join().unwrap().unwrap(), b.join().unwrap().unwrap());
        });
        assert_eq!(
            second
                .pending_native_steers(&session.id, "turn-1")
                .unwrap()
                .len(),
            1
        );
        assert!(matches!(
            second.enqueue_native_steer(&session.id, "turn-1", "input-1", "Different"),
            Err(StoreError::ConflictingNativeSteerInput(_))
        ));
        drop(first);
        drop(second);
        let reopened = SessionStore::open(root.path()).unwrap();
        let pending = reopened
            .pending_native_steers(&session.id, "turn-1")
            .unwrap();
        assert_eq!(pending[0].text, "Second prompt");
        let lease = reopened.try_acquire_native_lease(&session.id).unwrap();
        let ack = reopened
            .ack_native_steer(&lease, "turn-1", "input-1", 3)
            .unwrap();
        assert_eq!(ack.sequence, 3);
        assert!(matches!(
            reopened.append_native_event(&AgentEvent::new(
                &session.id,
                4,
                AgentEventKind::TurnCompleted {
                    response: "First answer".into(),
                    steps: 1,
                },
            )),
            Err(StoreError::NativeSteerAwaitingModel)
        ));
        assert_eq!(
            reopened
                .ack_native_steer(&lease, "turn-1", "input-1", 99)
                .unwrap(),
            ack
        );
        assert!(reopened
            .pending_native_steers(&session.id, "turn-1")
            .unwrap()
            .is_empty());
        let events = reopened.load(&session.id).unwrap().unwrap().events;
        assert_eq!(events.len(), 3);
        let replay = replay_native(
            &session.id,
            &events,
            None,
            &CheckpointCompatibility {
                provider: String::new(),
                model: String::new(),
                context_format_key: String::new(),
            },
        )
        .unwrap();
        assert_eq!(replay.history.len(), 2);
        assert_eq!(replay.history[1].content, "Second prompt");
        assert!(matches!(
            replay.recovery,
            crate::ReplayRecovery::ReadyForModel { next_step: 1 }
        ));
        // A process can crash after the synced event but before receiving
        // the ack. The sidecar still contains its old entry; reopening must
        // infer acknowledgement from the journal alone.
        drop(lease);
        drop(reopened);
        let after_crash = SessionStore::open(root.path()).unwrap();
        assert!(after_crash
            .pending_native_steers(&session.id, "turn-1")
            .unwrap()
            .is_empty());
        assert_eq!(
            after_crash
                .ack_native_steer(
                    &after_crash.try_acquire_native_lease(&session.id).unwrap(),
                    "turn-1",
                    "input-1",
                    99,
                )
                .unwrap(),
            ack
        );
    }

    #[test]
    fn native_steer_rejects_malformed_input_and_turn_mismatch() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        start_steer_test_turn(&store, &session.id);
        assert!(matches!(
            store.enqueue_native_steer(&session.id, "turn-1", "bad/id", "text"),
            Err(StoreError::InvalidNativeSteerInputId)
        ));
        assert!(matches!(
            store.enqueue_native_steer(&session.id, "turn-1", "input-1", " "),
            Err(StoreError::InvalidNativeSteerText)
        ));
        assert!(matches!(
            store.enqueue_native_steer(
                &session.id,
                "turn-1",
                "input-1",
                &"x".repeat(32 * 1024 + 1)
            ),
            Err(StoreError::InvalidNativeSteerText)
        ));
        assert!(matches!(
            store.enqueue_native_steer(&session.id, "turn-other", "input-1", "text"),
            Err(StoreError::NativeSteerTurnMismatch(_))
        ));
        store
            .append_native_event(&AgentEvent::new(
                &session.id,
                3,
                AgentEventKind::TurnFailed {
                    error: "stopped".into(),
                },
            ))
            .unwrap();
        assert!(matches!(
            store.enqueue_native_steer(&session.id, "turn-1", "input-1", "text"),
            Err(StoreError::NativeSteerTurnMismatch(_))
        ));
        assert_eq!(store.active_native_turn_id(&session.id).unwrap(), None);
    }

    #[test]
    fn native_steer_corrupt_inbox_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        start_steer_test_turn(&store, &session.id);
        store
            .enqueue_native_steer(&session.id, "turn-1", "input-1", "Second prompt")
            .unwrap();
        fs::write(
            store.native_steer_inbox_path(&session.id).unwrap(),
            format!(
                r#"{{"version":1,"session_id":"{}","entries":[{{"turn_id":"turn-1","input_id":"input-1","text":"Second prompt"}},{{"turn_id":"turn-1","input_id":"input-1","text":"Second prompt"}}]}}"#,
                session.id
            ),
        )
        .unwrap();
        let reopened = SessionStore::open(root.path()).unwrap();
        assert!(matches!(
            reopened.pending_native_steers(&session.id, "turn-1"),
            Err(StoreError::ConflictingNativeSteerInput(_))
        ));
        assert!(matches!(
            reopened.enqueue_native_steer(&session.id, "turn-1", "input-2", "More"),
            Err(StoreError::ConflictingNativeSteerInput(_))
        ));
    }

    #[test]
    fn native_steer_ack_boundary_and_terminal_event_preserve_pending_input() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        start_steer_test_turn(&store, &session.id);
        store
            .append_native_event(&AgentEvent::new(
                &session.id,
                3,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "model".into(),
                },
            ))
            .unwrap();
        store
            .enqueue_native_steer(&session.id, "turn-1", "input-1", "Please revise")
            .unwrap();
        let lease = store.try_acquire_native_lease(&session.id).unwrap();
        assert!(matches!(
            store.ack_native_steer(&lease, "turn-1", "input-1", 4),
            Err(StoreError::UnsafeNativeSteerBoundary(_))
        ));
        assert!(matches!(
            store.append_native_event(&AgentEvent::new(
                &session.id,
                4,
                AgentEventKind::TurnFailed {
                    error: "stopped".into(),
                },
            )),
            Err(StoreError::PendingNativeSteerInput)
        ));
        assert_eq!(
            store
                .pending_native_steers(&session.id, "turn-1")
                .unwrap()
                .len(),
            1
        );
        let reopened = SessionStore::open(root.path()).unwrap();
        assert_eq!(
            reopened
                .pending_native_steers(&session.id, "turn-1")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(reopened.load(&session.id).unwrap().unwrap().events.len(), 3);
    }

    #[test]
    fn native_steer_enqueue_and_terminal_race_has_one_durable_winner() {
        let root = tempfile::tempdir().unwrap();
        let first = SessionStore::open(root.path()).unwrap();
        let second = SessionStore::open(root.path()).unwrap();
        let session = first.create("/workspace", "native").unwrap();
        start_steer_test_turn(&first, &session.id);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let enqueue_barrier = Arc::clone(&barrier);
        let terminal_barrier = Arc::clone(&barrier);
        let (enqueued, terminated) = std::thread::scope(|scope| {
            let enqueue = scope.spawn(|| {
                enqueue_barrier.wait();
                first.enqueue_native_steer(&session.id, "turn-1", "input-1", "One more thing")
            });
            let terminal = scope.spawn(|| {
                terminal_barrier.wait();
                second.append_native_event(&AgentEvent::new(
                    &session.id,
                    3,
                    AgentEventKind::TurnFailed {
                        error: "done".into(),
                    },
                ))
            });
            barrier.wait();
            (enqueue.join().unwrap(), terminal.join().unwrap())
        });
        let reopened = SessionStore::open(root.path()).unwrap();
        match (enqueued, terminated) {
            (Ok(input), Err(StoreError::PendingNativeSteerInput)) => {
                assert_eq!(input.input_id, "input-1");
                assert_eq!(
                    reopened
                        .pending_native_steers(&session.id, "turn-1")
                        .unwrap(),
                    vec![input]
                );
                assert_eq!(reopened.load(&session.id).unwrap().unwrap().events.len(), 2);
            }
            (Err(StoreError::NativeSteerTurnMismatch(_)), Ok(())) => {
                assert_eq!(reopened.active_native_turn_id(&session.id).unwrap(), None);
                assert_eq!(reopened.load(&session.id).unwrap().unwrap().events.len(), 3);
            }
            other => panic!("unexpected steer/terminal race result: {other:?}"),
        }
    }

    #[test]
    fn native_turn_finish_fence_serializes_with_enqueue_and_survives_reopen() {
        let root = tempfile::tempdir().unwrap();
        let first = SessionStore::open(root.path()).unwrap();
        let second = SessionStore::open(root.path()).unwrap();
        let session = first.create("/workspace", "native").unwrap();
        start_steer_test_turn(&first, &session.id);
        let lease = first.try_acquire_native_lease(&session.id).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let fence_barrier = Arc::clone(&barrier);
        let enqueue_barrier = Arc::clone(&barrier);
        let (fenced, enqueued) = std::thread::scope(|scope| {
            let fence = scope.spawn(|| {
                fence_barrier.wait();
                first.fence_native_turn_completion(&lease, "turn-1")
            });
            let enqueue = scope.spawn(|| {
                enqueue_barrier.wait();
                second.enqueue_native_steer(&session.id, "turn-1", "input-1", "Late input")
            });
            barrier.wait();
            (fence.join().unwrap(), enqueue.join().unwrap())
        });
        let reopened = SessionStore::open(root.path()).unwrap();
        match (fenced, enqueued) {
            (Ok(()), Err(StoreError::NativeTurnFinishing(_))) => {
                assert!(reopened
                    .native_turn_finish_fence(&session.id, "turn-1")
                    .unwrap());
                first
                    .fence_native_turn_completion(&lease, "turn-1")
                    .unwrap();
                assert!(reopened
                    .pending_native_steers(&session.id, "turn-1")
                    .unwrap()
                    .is_empty());
                first
                    .append_native_event(&AgentEvent::new(
                        &session.id,
                        3,
                        AgentEventKind::TurnFailed {
                            error: "shutdown failed".into(),
                        },
                    ))
                    .unwrap();
                assert!(reopened
                    .native_turn_finish_fence(&session.id, "turn-1")
                    .unwrap());
            }
            (Err(StoreError::PendingNativeSteerInput), Ok(input)) => {
                assert_eq!(input.input_id, "input-1");
                assert!(!reopened
                    .native_turn_finish_fence(&session.id, "turn-1")
                    .unwrap());
                assert_eq!(
                    reopened
                        .pending_native_steers(&session.id, "turn-1")
                        .unwrap(),
                    vec![input]
                );
            }
            other => panic!("unexpected fence/enqueue race result: {other:?}"),
        }
    }

    #[test]
    fn native_turn_finish_fence_blocks_late_input_but_not_next_turn() {
        let root = tempfile::tempdir().unwrap();
        let first = SessionStore::open(root.path()).unwrap();
        let session = first.create("/workspace", "native").unwrap();
        start_steer_test_turn(&first, &session.id);
        let lease = first.try_acquire_native_lease(&session.id).unwrap();
        first
            .fence_native_turn_completion(&lease, "turn-1")
            .unwrap();
        drop(lease);
        drop(first);
        let reopened = SessionStore::open(root.path()).unwrap();
        assert!(reopened
            .native_turn_finish_fence(&session.id, "turn-1")
            .unwrap());
        assert!(matches!(
            reopened.enqueue_native_steer(&session.id, "turn-1", "input-1", "Too late"),
            Err(StoreError::NativeTurnFinishing(_))
        ));
        reopened
            .append_native_event(&AgentEvent::new(
                &session.id,
                3,
                AgentEventKind::TurnFailed {
                    error: "shutdown failed".into(),
                },
            ))
            .unwrap();
        reopened
            .admit_native_turn(&session.id, "turn-2", "Continue", "model", TurnMode::Build)
            .unwrap();
        reopened
            .enqueue_native_steer(&session.id, "turn-2", "input-2", "New turn input")
            .unwrap();
    }

    #[test]
    fn image_admission_and_steer_survive_reopen_without_local_paths() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let image = UserImage::from_bytes(
            crate::UserImageMime::Png,
            b"\x89PNG\r\n\x1a\nportable-image-data",
        )
        .unwrap();
        let admitted = store
            .admit_native_turn_with_images(
                &session.id,
                "image-turn",
                "",
                "model",
                TurnMode::Plan,
                std::slice::from_ref(&image),
            )
            .unwrap();
        assert!(matches!(
            admitted.kind,
            AgentEventKind::TurnAdmittedWithImages { .. }
        ));
        store
            .append_native_event(&AgentEvent::new(
                &session.id,
                2,
                AgentEventKind::TurnStarted {
                    prompt: String::new(),
                },
            ))
            .unwrap();
        let input = store
            .enqueue_native_steer_with_images(
                &session.id,
                "image-turn",
                "image-input",
                "",
                std::slice::from_ref(&image),
            )
            .unwrap();
        assert_eq!(input.images, vec![image.clone()]);
        let lease = store.try_acquire_native_lease(&session.id).unwrap();
        let ack = store
            .ack_native_steer(&lease, "image-turn", "image-input", 3)
            .unwrap();
        assert!(matches!(
            ack.kind,
            AgentEventKind::TurnSteeredWithImages { .. }
        ));
        drop(lease);
        drop(store);
        let reopened = SessionStore::open(root.path()).unwrap();
        let events = reopened.load(&session.id).unwrap().unwrap().events;
        let replay = replay_native(
            &session.id,
            &events,
            None,
            &CheckpointCompatibility {
                provider: String::new(),
                model: String::new(),
                context_format_key: String::new(),
            },
        )
        .unwrap();
        assert_eq!(replay.history.len(), 2);
        assert_eq!(replay.history[0].images, vec![image.clone()]);
        assert_eq!(replay.history[1].images, vec![image]);
        assert!(reopened
            .pending_native_steers(&session.id, "image-turn")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn image_journal_rejects_tampered_bytes_on_append_and_reopen() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let image =
            UserImage::from_bytes(crate::UserImageMime::Png, b"\x89PNG\r\n\x1a\nimage").unwrap();
        let mut invalid = image.clone();
        invalid.sha256 = "0".repeat(64);
        assert!(matches!(
            store.admit_native_turn_with_images(
                &session.id,
                "turn-1",
                "See image",
                "model",
                TurnMode::Build,
                &[invalid],
            ),
            Err(StoreError::InvalidUserImages(UserImageError::HashMismatch))
        ));
        let mut admitted = store
            .admit_native_turn_with_images(
                &session.id,
                "turn-1",
                "See image",
                "model",
                TurnMode::Build,
                &[image],
            )
            .unwrap();
        if let AgentEventKind::TurnAdmittedWithImages { images, .. } = &mut admitted.kind {
            images[0].sha256 = "0".repeat(64);
        }
        fs::write(
            store.events.path(&session.id).unwrap(),
            format!("{}\n", serde_json::to_string(&admitted).unwrap()),
        )
        .unwrap();
        drop(store);
        let reopened = SessionStore::open(root.path()).unwrap();
        assert!(matches!(
            reopened.load(&session.id),
            Err(StoreError::InconsistentEventLog {
                reason: "invalid user image input",
                ..
            })
        ));
    }
}
