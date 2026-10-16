//! Durable parent/child session coordination without model dispatch.
//!
//! A child is a normal native `SessionStore` session. Its immutable lineage is
//! committed with its metadata; mutable lifecycle state is kept in an atomic
//! sidecar protected by an OS lock shared by all coordinator instances.
//! Reopening a `Running` child whose OS session lease is gone marks it
//! `Interrupted`. Only an explicit `resume_child` can claim it again.

use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::PathBuf,
    sync::Arc,
};

use openide_core::{NativeSessionLease, Session, SessionStore, StoreError, TurnMode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

const REGISTRY_VERSION: u32 = 3;
const MAX_REGISTRY_BYTES: usize = 2 * 1024 * 1024;
const MAX_INPUTS_PER_CHILD: usize = 16;
const MAX_INPUT_PROMPT_BYTES: usize = 16 * 1024;
const MAX_INPUT_ID_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubagentLimits {
    pub max_depth: u8,
    pub max_children_per_parent: usize,
    pub max_concurrent_children: usize,
    /// Maximum distinct descendants beneath one root session, including
    /// completed children and children created in earlier turns.
    pub max_total_descendants: usize,
}

/// Immutable planned first request. The context digest covers the provider,
/// endpoint, instructions, catalog, backend request profile, and host hooks.
/// Native turn admission and model-context checks still happen separately.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentSpec {
    pub prompt: String,
    pub model: String,
    pub mode: TurnMode,
    pub context_digest: String,
}

impl SubagentSpec {
    pub fn digest_hex(&self) -> Result<String, SubagentError> {
        if self.prompt.trim().is_empty()
            || self.prompt.len() > 64 * 1024
            || self.model.trim().is_empty()
            || self.model.len() > 256
            || self.context_digest.len() != 64
            || !self
                .context_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(SubagentError::InvalidSpawnSpec);
        }
        let mut hash = Sha256::new();
        hash.update(b"openide-subagent-spec-v1\0");
        for field in [
            self.prompt.as_bytes(),
            self.model.as_bytes(),
            match self.mode {
                TurnMode::Plan => b"plan".as_slice(),
                TurnMode::Build => b"build".as_slice(),
            },
            self.context_digest.as_bytes(),
        ] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field);
        }
        Ok(format!("{:x}", hash.finalize()))
    }
}

impl Default for SubagentLimits {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_children_per_parent: 4,
            max_concurrent_children: 8,
            max_total_descendants: 32,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    Ready,
    Running,
    Interrupted,
    Completed,
    Failed,
    Cancelled,
}

impl SubagentStatus {
    fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubagentOutcome {
    Completed,
    Failed,
    Cancelled,
}

impl From<SubagentOutcome> for SubagentStatus {
    fn from(value: SubagentOutcome) -> Self {
        match value {
            SubagentOutcome::Completed => Self::Completed,
            SubagentOutcome::Failed => Self::Failed,
            SubagentOutcome::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubagentRecord {
    pub session: Session,
    pub status: SubagentStatus,
    pub cancellation_requested: bool,
}

/// One consistent registry snapshot for presentation, without worker authority.
#[derive(Clone, Debug, PartialEq)]
pub struct SubagentUiSnapshot {
    pub agent: SubagentRecord,
    pub open_inputs: usize,
    pub inputs: Vec<SubagentInputSnapshot>,
}

/// Read-only mailbox data; this snapshot grants no dispatch or edit authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentInputSnapshot {
    pub id: String,
    pub prompt: String,
    pub mode: TurnMode,
    pub status: SubagentInputStatus,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubagentInputStatus {
    Pending,
    Active,
    Done,
    Skipped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QueuedInputState {
    Pending,
    Active,
    Done,
    Skipped,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct QueuedSubagentInput {
    pub id: String,
    pub prompt: String,
    pub mode: TurnMode,
    /// Immutable digest of the original send_input request for exact retries.
    pub digest: String,
    pub state: QueuedInputState,
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Error)]
pub enum SubagentError {
    #[error("invalid subagent limits")]
    InvalidLimits,
    #[error("invalid subagent spawn request")]
    InvalidSpawnSpec,
    #[error("invalid subagent follow-up input")]
    InvalidInput,
    #[error("subagent follow-up input ID was reused with different content")]
    ConflictingInput,
    #[error("subagent input changed or was claimed; reload before editing")]
    InputEditConflict,
    #[error("subagent follow-up input limit exceeded")]
    InputLimit,
    #[error("subagent {0} does not accept another input")]
    InputClosed(String),
    #[error("subagent depth limit exceeded")]
    DepthLimit,
    #[error("active child limit exceeded for parent {0}")]
    ParentChildLimit(String),
    #[error("concurrent subagent limit exceeded")]
    ConcurrencyLimit,
    #[error("total subagent limit exceeded for root {0}")]
    TotalTaskLimit(String),
    #[error("session {child} is not a direct child of {parent}")]
    NotDirectChild { parent: String, child: String },
    #[error("subagent {0} requires journal recovery before desktop cancellation")]
    CancellationRequiresRecovery(String),
    #[error("live subagent has no durable cancellation authority")]
    CancellationAuthorityUnavailable,
    #[error(
        "durable subagent cancellation committed but command revocation reported an error: {0}"
    )]
    CommandRevocation(String),
    #[error("subagent control belongs to another session store")]
    ControlStoreMismatch,
    #[error("subagent {0} has no parent session")]
    MissingParent(String),
    #[error("subagent {0} has inconsistent parent lineage")]
    CorruptLineage(String),
    #[error("subagent {child} cannot move from {from:?} to {to:?}")]
    InvalidTransition {
        child: String,
        from: SubagentStatus,
        to: SubagentStatus,
    },
    #[error("subagent registry version {0} is unsupported")]
    UnsupportedRegistryVersion(u32),
    #[error("subagent registry exceeds {MAX_REGISTRY_BYTES} bytes")]
    RegistryTooLarge,
    #[error("subagent registry references missing or unrelated session {0}")]
    OrphanedState(String),
    #[error("session listing contains corruption: {0}")]
    CorruptSessionListing(String),
    #[error("subagent registry I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("subagent registry JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session store failed: {0}")]
    Store(#[from] StoreError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct ChildState {
    status: SubagentStatus,
    cancellation_requested: bool,
}

impl Default for ChildState {
    fn default() -> Self {
        Self {
            status: SubagentStatus::Ready,
            cancellation_requested: false,
        }
    }
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
struct Registry {
    version: u32,
    children: BTreeMap<String, ChildState>,
    #[serde(default)]
    inputs: BTreeMap<String, Vec<QueuedSubagentInput>>,
}

impl Registry {
    fn empty() -> Self {
        Self {
            version: REGISTRY_VERSION,
            children: BTreeMap::new(),
            inputs: BTreeMap::new(),
        }
    }

    fn state(&self, id: &str) -> ChildState {
        self.children.get(id).copied().unwrap_or_default()
    }
}

/// A child worker must retain this guard until it records a terminal outcome.
/// Dropping it without a terminal outcome leaves `Running` in the sidecar;
/// reopening observes the released OS lease and marks it `Interrupted`.
#[derive(Debug)]
pub struct SubagentRunLease {
    lease: Arc<NativeSessionLease>,
}

/// In-process authority for a parent's children, issued while its native
/// lease is held. It does not retain that lease, so a child can continue after
/// the parent turn ends and the next parent turn can acquire its own lease.
/// A restarted process obtains fresh authority from a new parent lease.
#[derive(Clone, Debug)]
pub struct SubagentControl {
    parent_id: String,
    store_root: PathBuf,
    workspace: PathBuf,
}

impl SubagentControl {
    pub fn parent_id(&self) -> &str {
        &self.parent_id
    }

    pub fn workspace(&self) -> &std::path::Path {
        &self.workspace
    }
}

impl SubagentRunLease {
    pub fn session_id(&self) -> &str {
        self.lease.session_id()
    }

    pub fn native_lease(&self) -> &NativeSessionLease {
        &self.lease
    }

    /// Share the same OS lease with a child worker while the coordinator
    /// retains authority to record its terminal state before the lock closes.
    pub fn shared_native_lease(&self) -> Arc<NativeSessionLease> {
        Arc::clone(&self.lease)
    }
}

pub struct SubagentCoordinator {
    sessions: Arc<SessionStore>,
    root: PathBuf,
    limits: SubagentLimits,
}

impl SubagentCoordinator {
    pub fn new(sessions: Arc<SessionStore>, limits: SubagentLimits) -> Result<Self, SubagentError> {
        if limits.max_depth == 0
            || limits.max_depth > 8
            || limits.max_children_per_parent == 0
            || limits.max_children_per_parent > 32
            || limits.max_concurrent_children == 0
            || limits.max_concurrent_children > 32
            || limits.max_total_descendants == 0
            || limits.max_total_descendants > 128
        {
            return Err(SubagentError::InvalidLimits);
        }
        Ok(Self {
            root: fs::canonicalize(sessions.storage_root())?,
            sessions,
            limits,
        })
    }

    pub fn limits(&self) -> SubagentLimits {
        self.limits
    }

    /// Remove abandoned publication files under the writer's OS lock. Never
    /// promote a temporary into authority: only the published registry counts.
    /// UI snapshots remain read-only; hosts call this during store recovery.
    pub fn recover_registry_temporaries(&self) -> Result<usize, SubagentError> {
        let _lock = self.lock_registry()?;
        self.read_registry()?;
        self.cleanup_registry_temporaries()
    }

    // Caller holds the registry lock, so no live writer can own these files.
    fn cleanup_registry_temporaries(&self) -> Result<usize, SubagentError> {
        let mut removed = 0;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(id) = name
                .strip_prefix(".subagents.")
                .and_then(|name| name.strip_suffix(".tmp"))
            else {
                continue;
            };
            let Ok(uuid) = Uuid::parse_str(id) else {
                continue;
            };
            if uuid.get_version() != Some(uuid::Version::Random)
                || uuid.hyphenated().to_string() != id
                || !entry.file_type()?.is_file()
            {
                continue;
            }
            fs::remove_file(entry.path())?;
            removed += 1;
        }
        #[cfg(unix)]
        if removed > 0 {
            File::open(&self.root)?.sync_all()?;
        }
        Ok(removed)
    }

    pub fn issue_control(
        &self,
        parent_lease: &NativeSessionLease,
    ) -> Result<SubagentControl, SubagentError> {
        let parent = self.sessions.native_session_for_lease(parent_lease)?;
        Ok(SubagentControl {
            parent_id: parent.id,
            store_root: self.root.clone(),
            workspace: parent.workspace,
        })
    }

    /// Create a child session while the parent worker holds its native lease.
    /// The child has no admitted turn and performs no model or tool work.
    pub fn create_child(
        &self,
        parent_lease: &NativeSessionLease,
        spawn_id: &str,
        spec: &SubagentSpec,
    ) -> Result<SubagentRecord, SubagentError> {
        let request_digest = spec.digest_hex()?;
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile(&mut registry)?;
        let parent = self.sessions.native_session_for_lease(parent_lease)?;
        if let Some(existing) =
            self.sessions
                .native_child_for_spawn(parent_lease, spawn_id, &request_digest)?
        {
            let state = registry.state(&existing.id);
            return Ok(record(existing, state));
        }
        if parent
            .subagent
            .as_ref()
            .is_some_and(|link| link.depth >= self.limits.max_depth)
        {
            return Err(SubagentError::DepthLimit);
        }
        let parent_state = registry.state(&parent.id);
        if parent.subagent.is_some()
            && (parent_state.status != SubagentStatus::Running
                || parent_state.cancellation_requested)
        {
            return Err(SubagentError::InvalidTransition {
                child: parent.id.clone(),
                from: parent_state.status,
                to: SubagentStatus::Running,
            });
        }
        let active_children = sessions
            .values()
            .filter(|session| {
                session
                    .subagent
                    .as_ref()
                    .is_some_and(|link| link.parent_session_id == parent.id)
                    && !registry.state(&session.id).status.terminal()
            })
            .count();
        if active_children >= self.limits.max_children_per_parent {
            return Err(SubagentError::ParentChildLimit(parent.id));
        }
        let root_id = root_session_id(&parent, &sessions)?;
        let descendants = sessions
            .values()
            .filter(|session| belongs_to_root(session, &root_id, &sessions))
            .count();
        if descendants >= self.limits.max_total_descendants {
            return Err(SubagentError::TotalTaskLimit(root_id));
        }
        // The initial Ready state is implicit in the child's atomic metadata
        // write. A crash here can never leave an unlinked child or a state
        // record pointing at a session that was not created.
        let child = self.sessions.create_native_child(
            parent_lease,
            spawn_id,
            &request_digest,
            self.limits.max_depth,
        )?;
        Ok(SubagentRecord {
            session: child,
            status: SubagentStatus::Ready,
            cancellation_requested: false,
        })
    }

    pub fn list_children(
        &self,
        parent_lease: &NativeSessionLease,
    ) -> Result<Vec<SubagentRecord>, SubagentError> {
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile(&mut registry)?;
        let parent = self.sessions.native_session_for_lease(parent_lease)?;
        let mut children = sessions
            .values()
            .filter(|session| {
                session
                    .subagent
                    .as_ref()
                    .is_some_and(|link| link.parent_session_id == parent.id)
            })
            .map(|session| record(session.clone(), registry.state(&session.id)))
            .collect::<Vec<_>>();
        children.sort_by(|left, right| {
            left.session
                .created_at_ms
                .cmp(&right.session.created_at_ms)
                .then_with(|| left.session.id.cmp(&right.session.id))
        });
        Ok(children)
    }

    /// UI snapshot without claiming the parent's lease or changing child
    /// lifecycle state. A released worker lease is shown as `Interrupted` in
    /// this snapshot; the next coordinator mutation persists reconciliation.
    pub fn list_children_for_ui(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<SubagentRecord>, SubagentError> {
        Ok(self
            .list_children_snapshot_for_ui(parent_session_id)?
            .into_iter()
            .map(|snapshot| snapshot.agent)
            .collect())
    }

    /// Read status, cancellation and all mailbox entries under the same registry
    /// lock. Does not acquire worker leases, dispatch children or mutate disk.
    pub fn list_children_snapshot_for_ui(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<SubagentUiSnapshot>, SubagentError> {
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile_inner(&mut registry, false)?;
        let parent = sessions
            .get(parent_session_id)
            .ok_or_else(|| StoreError::UnknownSession(parent_session_id.to_owned()))?;
        if parent.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession.into());
        }
        let mut children = sessions
            .values()
            .filter(|session| {
                session
                    .subagent
                    .as_ref()
                    .is_some_and(|link| link.parent_session_id == parent_session_id)
            })
            .map(|session| SubagentUiSnapshot {
                agent: record(session.clone(), registry.state(&session.id)),
                open_inputs: registry.inputs.get(&session.id).map_or(0, |inputs| {
                    inputs
                        .iter()
                        .filter(|input| {
                            matches!(
                                input.state,
                                QueuedInputState::Pending | QueuedInputState::Active
                            )
                        })
                        .count()
                }),
                inputs: registry
                    .inputs
                    .get(&session.id)
                    .into_iter()
                    .flatten()
                    .map(|input| SubagentInputSnapshot {
                        id: input.id.clone(),
                        prompt: input.prompt.clone(),
                        mode: input.mode,
                        revision: input.revision,
                        status: match input.state {
                            QueuedInputState::Pending => SubagentInputStatus::Pending,
                            QueuedInputState::Active => SubagentInputStatus::Active,
                            QueuedInputState::Done => SubagentInputStatus::Done,
                            QueuedInputState::Skipped => SubagentInputStatus::Skipped,
                        },
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        children.sort_by(|left, right| {
            left.agent
                .session
                .created_at_ms
                .cmp(&right.agent.session.created_at_ms)
                .then_with(|| left.agent.session.id.cmp(&right.agent.session.id))
        });
        Ok(children)
    }

    /// Count follow-up inputs that still require a child turn. This is a
    /// snapshot for host UI checks; destructive operations must also verify
    /// the queue under the registry lock in their own transaction.
    pub fn open_input_count_for_ui(&self, child_id: &str) -> Result<usize, SubagentError> {
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile_inner(&mut registry, false)?;
        let child = sessions
            .get(child_id)
            .ok_or_else(|| StoreError::UnknownSession(child_id.to_owned()))?;
        if child.subagent.is_none() {
            return Err(SubagentError::MissingParent(child_id.to_owned()));
        }
        Ok(registry.inputs.get(child_id).map_or(0, |inputs| {
            inputs
                .iter()
                .filter(|input| {
                    matches!(
                        input.state,
                        QueuedInputState::Pending | QueuedInputState::Active
                    )
                })
                .count()
        }))
    }

    /// Verify that a pre-created child belongs to this parent and the exact
    /// immutable spawn request. This lets an asynchronous worker start after
    /// the parent turn has released its lease without creating another child.
    pub fn prepared_child(
        &self,
        control: &SubagentControl,
        child_id: &str,
        spawn_id: &str,
        request_digest: &str,
    ) -> Result<SubagentRecord, SubagentError> {
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile(&mut registry)?;
        let child = self.require_direct_child_control(control, child_id, &sessions)?;
        let link = child.subagent.as_ref().expect("verified direct child");
        if link.spawn_id != spawn_id || !link.request_digest.eq_ignore_ascii_case(request_digest) {
            return Err(StoreError::ConflictingSubagentIdentity.into());
        }
        Ok(record(child.clone(), registry.state(child_id)))
    }

    /// Commit an idempotent follow-up before any new child turn is admitted.
    /// The caller's tool-call ID is the durable deduplication key. A child may
    /// receive input while Ready, Running, Interrupted, or Completed; failed
    /// and cancelled children are closed. Queued input survives process exit.
    pub(crate) fn enqueue_input(
        &self,
        parent_lease: &NativeSessionLease,
        child_id: &str,
        input_id: &str,
        prompt: &str,
        mode: TurnMode,
    ) -> Result<QueuedSubagentInput, SubagentError> {
        if input_id.is_empty()
            || input_id.len() > MAX_INPUT_ID_BYTES
            || prompt.trim().is_empty()
            || prompt.len() > MAX_INPUT_PROMPT_BYTES
        {
            return Err(SubagentError::InvalidInput);
        }
        let mut hasher = Sha256::new();
        hasher.update(b"openide-subagent-input-v1\0");
        hasher.update((prompt.len() as u64).to_le_bytes());
        hasher.update(prompt.as_bytes());
        hasher.update(match mode { TurnMode::Build => b"build".as_slice(), TurnMode::Plan => b"plan".as_slice() });
        let digest = format!("{:x}", hasher.finalize());
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile(&mut registry)?;
        let parent = self.sessions.native_session_for_lease(parent_lease)?;
        let control = SubagentControl {
            parent_id: parent.id,
            store_root: self.root.clone(),
            workspace: parent.workspace,
        };
        self.require_direct_child_control(&control, child_id, &sessions)?;
        let state = registry.state(child_id);
        let inputs = registry.inputs.entry(child_id.to_owned()).or_default();
        if let Some(existing) = inputs.iter().find(|entry| entry.id == input_id) {
            return if existing.digest == digest {
                Ok(existing.clone())
            } else {
                Err(SubagentError::ConflictingInput)
            };
        }
        if state.cancellation_requested
            || matches!(
                state.status,
                SubagentStatus::Failed | SubagentStatus::Cancelled
            )
        {
            return Err(SubagentError::InputClosed(child_id.to_owned()));
        }
        if inputs.len() >= MAX_INPUTS_PER_CHILD {
            return Err(SubagentError::InputLimit);
        }
        let entry = QueuedSubagentInput {
            id: input_id.to_owned(),
            prompt: prompt.to_owned(),
            mode,
            digest,
            state: QueuedInputState::Pending,
            revision: 0,
        };
        inputs.push(entry.clone());
        self.write_registry(&registry)?;
        Ok(entry)
    }

    /// Replace only unclaimed content under the same lock as worker claims.
    /// The original request digest is immutable so send_input retries remain
    /// idempotent. A repeated edit after an ambiguous commit is also idempotent.
    pub fn edit_pending_input(
        &self,
        parent_lease: &NativeSessionLease,
        child_id: &str,
        input_id: &str,
        expected_revision: u64,
        prompt: &str,
    ) -> Result<SubagentInputSnapshot, SubagentError> {
        if prompt.trim().is_empty() || prompt.len() > MAX_INPUT_PROMPT_BYTES {
            return Err(SubagentError::InvalidInput);
        }
        let control = self.issue_control(parent_lease)?;
        let _lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile(&mut registry)?;
        self.require_direct_child_control(&control, child_id, &sessions)?;
        let state = registry.state(child_id);
        if state.cancellation_requested
            || matches!(
                state.status,
                SubagentStatus::Failed | SubagentStatus::Cancelled
            )
        {
            return Err(SubagentError::InputClosed(child_id.to_owned()));
        }
        let input = registry
            .inputs
            .get_mut(child_id)
            .and_then(|entries| entries.iter_mut().find(|input| input.id == input_id))
            .ok_or(SubagentError::InputEditConflict)?;
        let next_revision = expected_revision
            .checked_add(1)
            .ok_or(SubagentError::InputEditConflict)?;
        if input.state != QueuedInputState::Pending {
            return Err(SubagentError::InputEditConflict);
        }
        if input.revision == next_revision && input.prompt == prompt {
            return Ok(SubagentInputSnapshot {
                id: input.id.clone(),
                prompt: input.prompt.clone(),
                mode: input.mode,
                status: SubagentInputStatus::Pending,
                revision: input.revision,
            });
        }
        if input.revision != expected_revision {
            return Err(SubagentError::InputEditConflict);
        }
        input.prompt = prompt.to_owned();
        input.revision = next_revision;
        let snapshot = SubagentInputSnapshot {
            id: input.id.clone(),
            prompt: input.prompt.clone(),
            mode: input.mode,
            status: SubagentInputStatus::Pending,
            revision: input.revision,
        };
        self.write_registry(&registry)?;
        Ok(snapshot)
    }

    /// Omit one unclaimed input without closing the child or the rest of its
    /// mailbox. Shares the registry lock with claims and edits. The revision
    /// makes a retry after an ambiguous commit idempotent, while stale UI
    /// actions cannot silently discard a newer edit.
    pub fn skip_pending_input(
        &self,
        parent_lease: &NativeSessionLease,
        child_id: &str,
        input_id: &str,
        expected_revision: u64,
    ) -> Result<SubagentInputSnapshot, SubagentError> {
        let control = self.issue_control(parent_lease)?;
        let _lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile(&mut registry)?;
        self.require_direct_child_control(&control, child_id, &sessions)?;
        let input = registry
            .inputs
            .get_mut(child_id)
            .and_then(|entries| entries.iter_mut().find(|input| input.id == input_id))
            .ok_or(SubagentError::InputEditConflict)?;
        let next_revision = expected_revision
            .checked_add(1)
            .ok_or(SubagentError::InputEditConflict)?;
        let retry = input.state == QueuedInputState::Skipped && input.revision == next_revision;
        if !retry {
            if input.state != QueuedInputState::Pending || input.revision != expected_revision {
                return Err(SubagentError::InputEditConflict);
            }
            input.state = QueuedInputState::Skipped;
            input.revision = next_revision;
        }
        let snapshot = SubagentInputSnapshot {
            id: input.id.clone(),
            prompt: input.prompt.clone(),
            mode: input.mode,
            status: SubagentInputStatus::Skipped,
            revision: input.revision,
        };
        if !retry {
            self.write_registry(&registry)?;
        }
        Ok(snapshot)
    }

    pub(crate) fn input_state(
        &self,
        control: &SubagentControl,
        child_id: &str,
    ) -> Result<(SubagentStatus, Option<QueuedSubagentInput>, usize), SubagentError> {
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile(&mut registry)?;
        self.require_direct_child_control(control, child_id, &sessions)?;
        let inputs = registry
            .inputs
            .get(child_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        Ok((
            registry.state(child_id).status,
            inputs
                .iter()
                .find(|entry| entry.state == QueuedInputState::Active)
                .cloned(),
            inputs
                .iter()
                .filter(|entry| entry.state == QueuedInputState::Pending)
                .count(),
        ))
    }

    pub(crate) fn lookup_input_control(
        &self,
        control: &SubagentControl,
        child_id: &str,
        input_id: &str,
    ) -> Result<Option<QueuedSubagentInput>, SubagentError> {
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile(&mut registry)?;
        self.require_direct_child_control(control, child_id, &sessions)?;
        Ok(registry
            .inputs
            .get(child_id)
            .and_then(|inputs| inputs.iter().find(|input| input.id == input_id))
            .cloned())
    }

    pub(crate) fn inputs_control(
        &self,
        control: &SubagentControl,
        child_id: &str,
    ) -> Result<Vec<QueuedSubagentInput>, SubagentError> {
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile(&mut registry)?;
        self.require_direct_child_control(control, child_id, &sessions)?;
        Ok(registry.inputs.get(child_id).cloned().unwrap_or_default())
    }

    /// Claim exactly one queued input under the registry lock and the child's
    /// OS lease. A crash before admission leaves `Active` and `Interrupted`,
    /// so recovery can inspect the journal before any model request.
    pub(crate) fn claim_next_input_control(
        &self,
        control: &SubagentControl,
        child_id: &str,
    ) -> Result<Option<(SubagentRunLease, QueuedSubagentInput)>, SubagentError> {
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile(&mut registry)?;
        self.require_direct_child_control(control, child_id, &sessions)?;
        let state = registry.state(child_id);
        if state.status != SubagentStatus::Completed || state.cancellation_requested {
            return Ok(None);
        }
        let Some(index) = registry.inputs.get(child_id).and_then(|inputs| {
            inputs
                .iter()
                .position(|entry| entry.state == QueuedInputState::Pending)
        }) else {
            return Ok(None);
        };
        if registry
            .children
            .values()
            .filter(|state| state.status == SubagentStatus::Running)
            .count()
            >= self.limits.max_concurrent_children
        {
            return Err(SubagentError::ConcurrencyLimit);
        }
        let lease = self.sessions.try_acquire_native_lease(child_id)?;
        let entry = {
            let inputs = registry
                .inputs
                .get_mut(child_id)
                .expect("queued input exists");
            inputs[index].state = QueuedInputState::Active;
            inputs[index].clone()
        };
        registry.children.insert(
            child_id.to_owned(),
            ChildState {
                status: SubagentStatus::Running,
                cancellation_requested: false,
            },
        );
        self.write_registry(&registry)?;
        Ok(Some((
            SubagentRunLease {
                lease: Arc::new(lease),
            },
            entry,
        )))
    }

    /// Claim a new child. An interrupted child requires `resume_child`, so a
    /// caller cannot accidentally rerun its original turn as fresh work.
    pub fn start_child(
        &self,
        parent_lease: &NativeSessionLease,
        child_id: &str,
    ) -> Result<SubagentRunLease, SubagentError> {
        self.claim_child(parent_lease, child_id, SubagentStatus::Ready)
    }

    pub fn start_child_control(
        &self,
        control: &SubagentControl,
        child_id: &str,
    ) -> Result<SubagentRunLease, SubagentError> {
        self.claim_child_control(control, child_id, SubagentStatus::Ready)
    }

    /// Claim a previously interrupted child for explicit replay/recovery.
    /// This does not itself invoke `ResumeEngine` or reexecute a tool.
    pub fn resume_child(
        &self,
        parent_lease: &NativeSessionLease,
        child_id: &str,
    ) -> Result<SubagentRunLease, SubagentError> {
        self.claim_child(parent_lease, child_id, SubagentStatus::Interrupted)
    }

    pub fn resume_child_control(
        &self,
        control: &SubagentControl,
        child_id: &str,
    ) -> Result<SubagentRunLease, SubagentError> {
        self.claim_child_control(control, child_id, SubagentStatus::Interrupted)
    }

    fn claim_child(
        &self,
        parent_lease: &NativeSessionLease,
        child_id: &str,
        expected: SubagentStatus,
    ) -> Result<SubagentRunLease, SubagentError> {
        let control = self.issue_control(parent_lease)?;
        self.claim_child_control(&control, child_id, expected)
    }

    fn claim_child_control(
        &self,
        control: &SubagentControl,
        child_id: &str,
        expected: SubagentStatus,
    ) -> Result<SubagentRunLease, SubagentError> {
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile(&mut registry)?;
        self.require_direct_child_control(control, child_id, &sessions)?;
        let current = registry.state(child_id);
        if current.status != expected || current.cancellation_requested {
            return Err(SubagentError::InvalidTransition {
                child: child_id.to_owned(),
                from: current.status,
                to: SubagentStatus::Running,
            });
        }
        if registry
            .children
            .values()
            .filter(|state| state.status == SubagentStatus::Running)
            .count()
            >= self.limits.max_concurrent_children
        {
            return Err(SubagentError::ConcurrencyLimit);
        }
        let lease = self.sessions.try_acquire_native_lease(child_id)?;
        registry.children.insert(
            child_id.to_owned(),
            ChildState {
                status: SubagentStatus::Running,
                cancellation_requested: false,
            },
        );
        self.write_registry(&registry)?;
        Ok(SubagentRunLease {
            lease: Arc::new(lease),
        })
    }

    /// Persist cancellation for the child and all descendants before any
    /// worker is notified. Idle children become terminal immediately. A
    /// running worker must poll its flag and close cooperatively.
    pub fn request_cancel(
        &self,
        parent_lease: &NativeSessionLease,
        child_id: &str,
    ) -> Result<SubagentRecord, SubagentError> {
        self.request_cancel_with_affected(parent_lease, child_id)
            .map(|(record, _affected)| record)
    }

    /// Commit cancellation and return the complete affected subtree from the
    /// same registry transaction. The returned IDs include terminal children;
    /// hosts may revoke their remaining command managers only after success.
    pub fn request_cancel_with_affected(
        &self,
        parent_lease: &NativeSessionLease,
        child_id: &str,
    ) -> Result<(SubagentRecord, Vec<String>), SubagentError> {
        let control = self.issue_control(parent_lease)?;
        self.request_cancel_control_with_affected(&control, child_id)
    }

    /// Desktop cancellation without a live worker must preserve unresolved
    /// journals in this entire subtree for explicit recovery. The check and
    /// cancellation flag are serialized with worker claims by the registry lock.
    pub fn request_cancel_from_ui(
        &self,
        parent_lease: &NativeSessionLease,
        child_id: &str,
    ) -> Result<SubagentRecord, SubagentError> {
        self.request_cancel_from_ui_with_affected(parent_lease, child_id)
            .map(|(record, _affected)| record)
    }

    /// Apply the desktop recovery checks and return the exact committed subtree
    /// without a second lineage read. A rejected request returns no IDs and
    /// does not publish lifecycle reconciliation or mailbox changes.
    pub fn request_cancel_from_ui_with_affected(
        &self,
        parent_lease: &NativeSessionLease,
        child_id: &str,
    ) -> Result<(SubagentRecord, Vec<String>), SubagentError> {
        let control = self.issue_control(parent_lease)?;
        self.request_cancel_inner(&control, child_id, true)
    }

    pub fn request_cancel_control(
        &self,
        control: &SubagentControl,
        child_id: &str,
    ) -> Result<SubagentRecord, SubagentError> {
        self.request_cancel_control_with_affected(control, child_id)
            .map(|(record, _affected)| record)
    }

    /// Return the target and all descendants covered by this cancellation,
    /// including Completed, Failed and already Cancelled sessions. No targets
    /// are returned until the cancellation and mailbox skips are committed.
    pub fn request_cancel_control_with_affected(
        &self,
        control: &SubagentControl,
        child_id: &str,
    ) -> Result<(SubagentRecord, Vec<String>), SubagentError> {
        self.request_cancel_inner(control, child_id, false)
    }

    fn request_cancel_inner(
        &self,
        control: &SubagentControl,
        child_id: &str,
        require_recoverable_journals: bool,
    ) -> Result<(SubagentRecord, Vec<String>), SubagentError> {
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let original_registry = registry.clone();
        let sessions = self.reconcile_inner(&mut registry, false)?;
        let child = self.require_direct_child_control(control, child_id, &sessions)?;
        let mut affected = vec![child_id.to_owned()];
        let mut cursor = 0;
        while cursor < affected.len() {
            let parent_id = &affected[cursor];
            let mut descendants = sessions
                .values()
                .filter(|session| {
                    session
                        .subagent
                        .as_ref()
                        .is_some_and(|link| &link.parent_session_id == parent_id)
                })
                .map(|session| session.id.clone())
                .collect::<Vec<_>>();
            descendants.sort();
            affected.extend(descendants);
            cursor += 1;
        }
        if require_recoverable_journals {
            for id in &affected {
                let state = registry.state(id);
                // A child paused on a provider limit stopped at a model
                // boundary: nothing of it is uncertain, so it can be cancelled.
                let paused = state.status == SubagentStatus::Interrupted
                    && self.sessions.load(id)?.is_some_and(|stored| {
                        matches!(
                            stored.events.last().map(|event| &event.kind),
                            Some(openide_core::AgentEventKind::TurnPaused { .. })
                        )
                    });
                if (state.status == SubagentStatus::Interrupted && !paused)
                    || (state.status == SubagentStatus::Ready
                        && self
                            .sessions
                            .load(id)?
                            .is_some_and(|stored| !stored.events.is_empty()))
                {
                    return Err(SubagentError::CancellationRequiresRecovery(id.clone()));
                }
            }
        }
        for id in &affected {
            let mut state = registry.state(id);
            if !state.status.terminal() {
                state.cancellation_requested = true;
                if state.status != SubagentStatus::Running {
                    state.status = SubagentStatus::Cancelled;
                }
                registry.children.insert(id.clone(), state);
            } else if state.status == SubagentStatus::Completed && !state.cancellation_requested {
                state.cancellation_requested = true;
                registry.children.insert(id.clone(), state);
            }
            if let Some(inputs) = registry.inputs.get_mut(id) {
                for input in inputs {
                    if input.state == QueuedInputState::Pending {
                        input.state = QueuedInputState::Skipped;
                    }
                }
            }
        }
        if registry != original_registry {
            self.write_registry(&registry)?;
        }
        Ok((record(child.clone(), registry.state(child_id)), affected))
    }

    pub fn cancellation_requested(&self, run: &SubagentRunLease) -> Result<bool, SubagentError> {
        self.sessions.native_session_for_lease(&run.lease)?;
        let _registry_lock = self.lock_registry()?;
        let registry = self.read_registry()?;
        Ok(registry.state(run.session_id()).cancellation_requested)
    }

    /// Sync a terminal state while the child lease is still held. A worker's
    /// confirmed terminal outcome wins a cancellation race; the saved
    /// `cancellation_requested` flag remains visible. Repeating the same
    /// outcome is idempotent; conflicting outcomes are rejected.
    pub fn finish_child(
        &self,
        run: &SubagentRunLease,
        outcome: SubagentOutcome,
    ) -> Result<SubagentRecord, SubagentError> {
        let child = self.sessions.native_session_for_lease(&run.lease)?;
        if child.subagent.is_none() {
            return Err(SubagentError::NotDirectChild {
                parent: String::new(),
                child: child.id,
            });
        }
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let desired = SubagentStatus::from(outcome);
        let mut state = registry.state(run.session_id());
        if state.status != desired && state.status != SubagentStatus::Running {
            return Err(SubagentError::InvalidTransition {
                child: run.session_id().to_owned(),
                from: state.status,
                to: desired,
            });
        }
        state.status = desired;
        registry.children.insert(run.session_id().to_owned(), state);
        if let Some(inputs) = registry.inputs.get_mut(run.session_id()) {
            if let Some(active) = inputs
                .iter_mut()
                .find(|entry| entry.state == QueuedInputState::Active)
            {
                active.state = if desired == SubagentStatus::Completed {
                    QueuedInputState::Done
                } else {
                    QueuedInputState::Skipped
                };
            }
            if desired != SubagentStatus::Completed || state.cancellation_requested {
                for input in inputs {
                    if input.state == QueuedInputState::Pending {
                        input.state = QueuedInputState::Skipped;
                    }
                }
            }
        }
        self.write_registry(&registry)?;
        Ok(record(child, state))
    }

    /// Move a child to another model route: close its paused turn with
    /// `TurnSuperseded` and give it `new_identity_digest`, the identity of the
    /// worker that will continue it.
    ///
    /// Allowed ONLY while the child is paused at a clean boundary: it is a
    /// direct child of `control`, the registry has it `Interrupted` with no
    /// cancellation requested, no worker holds its lease, and the last event
    /// of its journal is the `TurnPaused` of its open turn (which replay only
    /// accepts at a model boundary, with every tool result committed) or the
    /// `TurnSuperseded` of an earlier attempt, which this call completes.
    /// Anything else is refused and changes nothing.
    pub fn rebind_child(
        &self,
        control: &SubagentControl,
        child_id: &str,
        new_identity_digest: &str,
        provider_id: &str,
        model_id: &str,
    ) -> Result<SubagentRecord, SubagentError> {
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile(&mut registry)?;
        self.require_direct_child_control(control, child_id, &sessions)?;
        let state = registry.state(child_id);
        let refused = || SubagentError::InvalidTransition {
            child: child_id.to_owned(),
            from: state.status,
            to: SubagentStatus::Interrupted,
        };
        if state.status != SubagentStatus::Interrupted || state.cancellation_requested {
            return Err(refused());
        }
        let lease = self.sessions.try_acquire_native_lease(child_id)?;
        let stored = self
            .sessions
            .load(child_id)?
            .ok_or(StoreError::UnknownSession(child_id.to_owned()))?;
        match stored.events.last().map(|event| (&event.kind, event.sequence)) {
            Some((openide_core::AgentEventKind::TurnPaused { step, .. }, sequence)) => {
                self.sessions.append_native_event(&openide_core::AgentEvent::new(
                    child_id,
                    sequence + 1,
                    openide_core::AgentEventKind::TurnSuperseded {
                        provider_id: provider_id.to_owned(),
                        model_id: model_id.to_owned(),
                        next_turn_id: Uuid::new_v4().to_string(),
                        // Step 1 never got a response: its own prompt is
                        // admitted again instead of a continuation.
                        host_prompt: *step > 1,
                    },
                ))?;
            }
            Some((openide_core::AgentEventKind::TurnSuperseded { .. }, _)) => {}
            _ => return Err(refused()),
        }
        let session = self.sessions.rebind_native_child(&lease, new_identity_digest)?;
        Ok(record(session, state))
    }

    /// Persist an explicit recovery pause while the child OS lease is still
    /// held. The next attempt must claim `Interrupted`; no new turn is admitted.
    /// A previously requested cancellation remains terminal in the sidecar.
    pub fn pause_child(&self, run: &SubagentRunLease) -> Result<SubagentRecord, SubagentError> {
        let child = self.sessions.native_session_for_lease(&run.lease)?;
        if child.subagent.is_none() {
            return Err(SubagentError::NotDirectChild {
                parent: String::new(),
                child: child.id,
            });
        }
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let mut state = registry.state(run.session_id());
        if state.status != SubagentStatus::Running {
            return Err(SubagentError::InvalidTransition {
                child: run.session_id().to_owned(),
                from: state.status,
                to: SubagentStatus::Interrupted,
            });
        }
        state.status = if state.cancellation_requested {
            SubagentStatus::Cancelled
        } else {
            SubagentStatus::Interrupted
        };
        registry.children.insert(run.session_id().to_owned(), state);
        if state.cancellation_requested {
            if let Some(inputs) = registry.inputs.get_mut(run.session_id()) {
                for input in inputs {
                    if matches!(
                        input.state,
                        QueuedInputState::Active | QueuedInputState::Pending
                    ) {
                        input.state = QueuedInputState::Skipped;
                    }
                }
            }
        }
        self.write_registry(&registry)?;
        Ok(record(child, state))
    }

    fn require_direct_child_control<'a>(
        &self,
        control: &SubagentControl,
        child_id: &str,
        sessions: &'a HashMap<String, Session>,
    ) -> Result<&'a Session, SubagentError> {
        if control.store_root != self.root {
            return Err(SubagentError::ControlStoreMismatch);
        }
        let parent = sessions
            .get(&control.parent_id)
            .ok_or_else(|| StoreError::UnknownSession(control.parent_id.clone()))?;
        if parent.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession.into());
        }
        if parent.workspace != control.workspace {
            return Err(SubagentError::CorruptLineage(parent.id.clone()));
        }
        let child = sessions
            .get(child_id)
            .ok_or_else(|| StoreError::UnknownSession(child_id.to_owned()))?;
        if child
            .subagent
            .as_ref()
            .is_none_or(|link| link.parent_session_id != control.parent_id)
        {
            return Err(SubagentError::NotDirectChild {
                parent: control.parent_id.clone(),
                child: child_id.to_owned(),
            });
        }
        Ok(child)
    }

    fn reconcile(
        &self,
        registry: &mut Registry,
    ) -> Result<HashMap<String, Session>, SubagentError> {
        self.reconcile_inner(registry, true)
    }

    fn reconcile_inner(
        &self,
        registry: &mut Registry,
        persist: bool,
    ) -> Result<HashMap<String, Session>, SubagentError> {
        let listing = self.sessions.list_with_diagnostics()?;
        if !listing.diagnostics.is_empty() {
            return Err(SubagentError::CorruptSessionListing(
                listing
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.session_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        let sessions = listing
            .sessions
            .into_iter()
            .map(|stored| (stored.session.id.clone(), stored.session))
            .collect::<HashMap<_, _>>();
        for (id, state) in &registry.children {
            if state.status == SubagentStatus::Ready
                || sessions
                    .get(id)
                    .is_none_or(|session| session.subagent.is_none())
            {
                return Err(SubagentError::OrphanedState(id.clone()));
            }
        }
        for (id, inputs) in &registry.inputs {
            if sessions
                .get(id)
                .is_none_or(|session| session.subagent.is_none())
                || inputs.len() > MAX_INPUTS_PER_CHILD
                || inputs
                    .iter()
                    .filter(|entry| entry.state == QueuedInputState::Active)
                    .count()
                    > 1
            {
                return Err(SubagentError::OrphanedState(id.clone()));
            }
        }
        for session in sessions
            .values()
            .filter(|session| session.subagent.is_some())
        {
            let link = session.subagent.as_ref().expect("filtered");
            let Some(parent) = sessions.get(&link.parent_session_id) else {
                return Err(SubagentError::MissingParent(session.id.clone()));
            };
            let expected_depth = parent
                .subagent
                .as_ref()
                .map_or(Some(1), |parent_link| parent_link.depth.checked_add(1));
            if expected_depth != Some(link.depth)
                || session.backend != "native"
                || parent.backend != "native"
                || session.workspace != parent.workspace
            {
                return Err(SubagentError::CorruptLineage(session.id.clone()));
            }
        }
        let mut changed = false;
        for (id, state) in &mut registry.children {
            if state.status == SubagentStatus::Running {
                match self.sessions.try_acquire_native_lease(id) {
                    Ok(_released_worker_lease) => {
                        state.status = if state.cancellation_requested {
                            SubagentStatus::Cancelled
                        } else {
                            SubagentStatus::Interrupted
                        };
                        if state.cancellation_requested {
                            if let Some(inputs) = registry.inputs.get_mut(id) {
                                for input in inputs {
                                    if matches!(
                                        input.state,
                                        QueuedInputState::Active | QueuedInputState::Pending
                                    ) {
                                        input.state = QueuedInputState::Skipped;
                                    }
                                }
                            }
                        }
                        changed = true;
                    }
                    Err(StoreError::NativeSessionBusy(_)) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        if changed && persist {
            self.write_registry(registry)?;
        }
        Ok(sessions)
    }

    fn registry_path(&self) -> PathBuf {
        self.root.join("subagents.registry.json")
    }

    fn lock_registry(&self) -> Result<File, SubagentError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join(".subagents.registry.lock"))?;
        file.lock()?;
        Ok(file)
    }

    fn read_registry(&self) -> Result<Registry, SubagentError> {
        let file = match File::open(self.registry_path()) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Registry::empty()),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take((MAX_REGISTRY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_REGISTRY_BYTES {
            return Err(SubagentError::RegistryTooLarge);
        }
        let registry: Registry = serde_json::from_slice(&bytes)?;
        if ![1, 2, REGISTRY_VERSION].contains(&registry.version) {
            return Err(SubagentError::UnsupportedRegistryVersion(registry.version));
        }
        Ok(Registry {
            version: REGISTRY_VERSION,
            ..registry
        })
    }

    fn write_registry(&self, registry: &Registry) -> Result<(), SubagentError> {
        let serialized = serde_json::to_vec(registry)?;
        if serialized.len() + 1 > MAX_REGISTRY_BYTES {
            return Err(SubagentError::RegistryTooLarge);
        }
        self.cleanup_registry_temporaries()?;
        let temporary = self.root.join(format!(".subagents.{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<(), SubagentError> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&serialized)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            #[cfg(test)]
            registry_publication_cut(registry, "temporary-synced");
            fs::rename(&temporary, self.registry_path())?;
            #[cfg(test)]
            registry_publication_cut(registry, "renamed");
            #[cfg(unix)]
            File::open(&self.root)?.sync_all()?;
            #[cfg(test)]
            registry_publication_cut(registry, "directory-synced");
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

#[cfg(test)]
fn registry_publication_cut(registry: &Registry, stage: &str) {
    if std::env::var("OPENIDE_MAILBOX_PUBLICATION_STAGE").as_deref() != Ok(stage) {
        return;
    }
    let operation = std::env::var("OPENIDE_MAILBOX_PUBLICATION_OPERATION").unwrap_or_default();
    let target = registry.inputs.values().flatten().any(|input| {
        input.id == "publication-cut"
            && match operation.as_str() {
                "enqueue" => input.state == QueuedInputState::Pending && input.revision == 0,
                "edit" => input.revision == 1,
                "skip" => input.state == QueuedInputState::Skipped && input.revision == 1,
                "claim" => input.state == QueuedInputState::Active,
                _ => false,
            }
    });
    if target {
        // Exit without Rust destructors: OS locks/leases must release on death.
        std::process::exit(86);
    }
}

fn root_session_id(
    session: &Session,
    sessions: &HashMap<String, Session>,
) -> Result<String, SubagentError> {
    let mut current = session;
    while let Some(link) = &current.subagent {
        current = sessions
            .get(&link.parent_session_id)
            .ok_or_else(|| SubagentError::MissingParent(current.id.clone()))?;
    }
    Ok(current.id.clone())
}

fn belongs_to_root(session: &Session, root_id: &str, sessions: &HashMap<String, Session>) -> bool {
    if session.id == root_id || session.subagent.is_none() {
        return false;
    }
    let mut current = session;
    while let Some(link) = &current.subagent {
        if link.parent_session_id == root_id {
            return true;
        }
        let Some(parent) = sessions.get(&link.parent_session_id) else {
            return false;
        };
        current = parent;
    }
    false
}

fn record(session: Session, state: ChildState) -> SubagentRecord {
    SubagentRecord {
        session,
        status: state.status,
        cancellation_requested: state.cancellation_requested,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup(limits: SubagentLimits) -> (TempDir, Arc<SessionStore>, SubagentCoordinator, Session) {
        let root = TempDir::new().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
        let parent = sessions.create(root.path(), "native").unwrap();
        let coordinator = SubagentCoordinator::new(Arc::clone(&sessions), limits).unwrap();
        (root, sessions, coordinator, parent)
    }

    fn spec() -> SubagentSpec {
        SubagentSpec {
            prompt: "Inspect the child task".into(),
            model: "model-a".into(),
            mode: TurnMode::Build,
            context_digest: "a".repeat(64),
        }
    }

    #[test]
    fn cancellation_returns_complete_committed_subtree_after_reopen() {
        let (root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "completed-root", &spec())
            .unwrap();
        let sibling = coordinator
            .create_child(&parent_lease, "foreign-sibling", &spec())
            .unwrap();
        let child_run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        let completed = coordinator
            .create_child(child_run.native_lease(), "completed-descendant", &spec())
            .unwrap();
        let idle = coordinator
            .create_child(child_run.native_lease(), "idle-descendant", &spec())
            .unwrap();
        let failed = coordinator
            .create_child(child_run.native_lease(), "failed-descendant", &spec())
            .unwrap();
        let completed_run = coordinator
            .start_child(child_run.native_lease(), &completed.session.id)
            .unwrap();
        let nested = coordinator
            .create_child(completed_run.native_lease(), "nested-idle", &spec())
            .unwrap();
        for (lease, id) in [
            (child_run.native_lease(), completed.session.id.as_str()),
            (child_run.native_lease(), idle.session.id.as_str()),
            (completed_run.native_lease(), nested.session.id.as_str()),
            (&parent_lease, sibling.session.id.as_str()),
        ] {
            coordinator
                .enqueue_input(lease, id, "pending", "Keep exact input", TurnMode::Build)
                .unwrap();
        }
        coordinator
            .finish_child(&completed_run, SubagentOutcome::Completed)
            .unwrap();
        let failed_run = coordinator
            .start_child(child_run.native_lease(), &failed.session.id)
            .unwrap();
        coordinator
            .finish_child(&failed_run, SubagentOutcome::Failed)
            .unwrap();
        coordinator
            .finish_child(&child_run, SubagentOutcome::Completed)
            .unwrap();
        coordinator
            .enqueue_input(
                &parent_lease,
                &child.session.id,
                "pending",
                "Completed mailbox remains open until cancellation",
                TurnMode::Build,
            )
            .unwrap();
        let stranger = sessions.create(root.path(), "native").unwrap();
        let stranger_lease = sessions.try_acquire_native_lease(&stranger.id).unwrap();
        let unrelated = coordinator
            .create_child(&stranger_lease, "foreign-root-child", &spec())
            .unwrap();
        coordinator
            .enqueue_input(
                &stranger_lease,
                &unrelated.session.id,
                "foreign-pending",
                "Unrelated work",
                TurnMode::Build,
            )
            .unwrap();
        drop(completed_run);
        drop(failed_run);
        drop(child_run);
        drop(stranger_lease);
        drop(parent_lease);
        drop(coordinator);
        drop(sessions);

        let reopened = Arc::new(SessionStore::open(root.path()).unwrap());
        let coordinator =
            SubagentCoordinator::new(Arc::clone(&reopened), Default::default()).unwrap();
        let parent_lease = reopened.try_acquire_native_lease(&parent.id).unwrap();
        let sibling_before = coordinator
            .list_children_snapshot_for_ui(&parent.id)
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.agent.session.id == sibling.session.id)
            .unwrap();
        let unrelated_before = coordinator
            .list_children_snapshot_for_ui(&stranger.id)
            .unwrap();
        let result = coordinator
            .request_cancel_from_ui_with_affected(&parent_lease, &child.session.id)
            .unwrap();
        let mut descendants = vec![
            completed.session.id.clone(),
            idle.session.id.clone(),
            failed.session.id.clone(),
        ];
        descendants.sort();
        let mut expected = vec![child.session.id.clone()];
        expected.extend(descendants);
        expected.push(nested.session.id.clone());
        assert_eq!(result.1, expected);
        assert_eq!(result.0.status, SubagentStatus::Completed);
        assert!(result.0.cancellation_requested);
        let committed_bytes = fs::read(coordinator.registry_path()).unwrap();
        drop(parent_lease);
        drop(coordinator);
        drop(reopened);

        let reopened = Arc::new(SessionStore::open(root.path()).unwrap());
        let coordinator =
            SubagentCoordinator::new(Arc::clone(&reopened), Default::default()).unwrap();
        let parent_lease = reopened.try_acquire_native_lease(&parent.id).unwrap();
        let control = coordinator.issue_control(&parent_lease).unwrap();
        assert_eq!(
            coordinator
                .request_cancel_control_with_affected(&control, &child.session.id)
                .unwrap(),
            result
        );
        assert_eq!(
            coordinator
                .request_cancel(&parent_lease, &child.session.id)
                .unwrap(),
            result.0
        );
        assert_eq!(
            fs::read(coordinator.registry_path()).unwrap(),
            committed_bytes
        );
        let root_children = coordinator
            .list_children_snapshot_for_ui(&parent.id)
            .unwrap();
        assert_eq!(
            root_children
                .iter()
                .find(|snapshot| snapshot.agent.session.id == sibling.session.id)
                .unwrap(),
            &sibling_before
        );
        assert_eq!(
            coordinator
                .list_children_snapshot_for_ui(&stranger.id)
                .unwrap(),
            unrelated_before
        );
        let cancelled_child = root_children
            .iter()
            .find(|snapshot| snapshot.agent.session.id == child.session.id)
            .unwrap();
        assert_eq!(cancelled_child.open_inputs, 0);
        assert_eq!(
            cancelled_child.inputs[0].status,
            SubagentInputStatus::Skipped
        );
        let descendants = coordinator
            .list_children_snapshot_for_ui(&child.session.id)
            .unwrap();
        for snapshot in &descendants {
            if snapshot.agent.session.id == failed.session.id {
                assert_eq!(snapshot.agent.status, SubagentStatus::Failed);
            } else {
                assert!(snapshot.agent.cancellation_requested);
                assert_eq!(snapshot.open_inputs, 0);
                assert_eq!(snapshot.inputs[0].status, SubagentInputStatus::Skipped);
                assert_eq!(
                    snapshot.agent.status,
                    if snapshot.agent.session.id == completed.session.id {
                        SubagentStatus::Completed
                    } else {
                        SubagentStatus::Cancelled
                    }
                );
            }
        }
        let nested = coordinator
            .list_children_snapshot_for_ui(&completed.session.id)
            .unwrap();
        assert_eq!(nested[0].agent.status, SubagentStatus::Cancelled);
        assert_eq!(nested[0].open_inputs, 0);
        assert_eq!(nested[0].inputs[0].status, SubagentInputStatus::Skipped);
    }

    #[test]
    fn cancellation_authority_failures_do_not_reconcile_or_publish_targets() {
        let (root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "authority-child", &spec())
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        coordinator
            .enqueue_input(
                &parent_lease,
                &child.session.id,
                "pending",
                "Do not skip on rejection",
                TurnMode::Build,
            )
            .unwrap();
        drop(run);
        let original_bytes = fs::read(coordinator.registry_path()).unwrap();
        let stranger = sessions.create(root.path(), "native").unwrap();
        let stranger_lease = sessions.try_acquire_native_lease(&stranger.id).unwrap();
        let stranger_control = coordinator.issue_control(&stranger_lease).unwrap();
        assert!(matches!(
            coordinator.request_cancel_control_with_affected(&stranger_control, &child.session.id),
            Err(SubagentError::NotDirectChild { .. })
        ));
        assert_eq!(
            fs::read(coordinator.registry_path()).unwrap(),
            original_bytes
        );
        let (_foreign_root, foreign_store, foreign_coordinator, foreign_parent) =
            setup(Default::default());
        let foreign_lease = foreign_store
            .try_acquire_native_lease(&foreign_parent.id)
            .unwrap();
        let foreign_control = foreign_coordinator.issue_control(&foreign_lease).unwrap();
        assert!(matches!(
            coordinator.request_cancel_control_with_affected(&foreign_control, &child.session.id),
            Err(SubagentError::ControlStoreMismatch)
        ));
        assert_eq!(
            fs::read(coordinator.registry_path()).unwrap(),
            original_bytes
        );
        let snapshot = coordinator
            .list_children_snapshot_for_ui(&parent.id)
            .unwrap();
        assert_eq!(snapshot[0].agent.status, SubagentStatus::Interrupted);
        assert!(!snapshot[0].agent.cancellation_requested);
        assert_eq!(snapshot[0].inputs[0].status, SubagentInputStatus::Pending);
        assert_eq!(
            fs::read(coordinator.registry_path()).unwrap(),
            original_bytes
        );
    }

    #[test]
    fn cancellation_closes_new_descendant_spawn_but_preserves_existing_retry() {
        let (_root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "running-child", &spec())
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        let descendant = coordinator
            .create_child(run.native_lease(), "existing-descendant", &spec())
            .unwrap();
        let mut before = sessions
            .list_with_diagnostics()
            .unwrap()
            .sessions
            .into_iter()
            .map(|stored| stored.session)
            .collect::<Vec<_>>();
        before.sort_by(|left, right| left.id.cmp(&right.id));
        let (_, affected_before) = coordinator
            .request_cancel_with_affected(&parent_lease, &child.session.id)
            .unwrap();
        assert_eq!(
            affected_before,
            vec![child.session.id.clone(), descendant.session.id.clone()]
        );
        let committed_bytes = fs::read(coordinator.registry_path()).unwrap();
        assert!(matches!(
            coordinator.create_child(run.native_lease(), "late-descendant", &spec()),
            Err(SubagentError::InvalidTransition { child: id, from, to })
                if id == child.session.id
                    && from == SubagentStatus::Running
                    && to == SubagentStatus::Running
        ));
        assert!(sessions
            .native_child_for_spawn(
                run.native_lease(),
                "late-descendant",
                &spec().digest_hex().unwrap()
            )
            .unwrap()
            .is_none());
        let retried = coordinator
            .create_child(run.native_lease(), "existing-descendant", &spec())
            .unwrap();
        assert_eq!(retried.session, descendant.session);
        assert_eq!(retried.status, SubagentStatus::Cancelled);
        assert!(retried.cancellation_requested);
        let mut after = sessions
            .list_with_diagnostics()
            .unwrap()
            .sessions
            .into_iter()
            .map(|stored| stored.session)
            .collect::<Vec<_>>();
        after.sort_by(|left, right| left.id.cmp(&right.id));
        assert_eq!(after, before);
        assert_eq!(
            coordinator
                .request_cancel_with_affected(&parent_lease, &child.session.id)
                .unwrap()
                .1,
            affected_before
        );
        assert_eq!(
            fs::read(coordinator.registry_path()).unwrap(),
            committed_bytes
        );
    }

    #[test]
    fn desktop_cancellation_closes_completed_mailbox_after_reopen() {
        let (root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "ui-mailbox", &spec())
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        coordinator
            .finish_child(&run, SubagentOutcome::Completed)
            .unwrap();
        drop(run);
        coordinator
            .enqueue_input(
                &parent_lease,
                &child.session.id,
                "follow-up",
                "More work",
                TurnMode::Build,
            )
            .unwrap();
        drop(parent_lease);
        drop(coordinator);
        let coordinator = SubagentCoordinator::new(
            Arc::new(SessionStore::open(root.path()).unwrap()),
            Default::default(),
        )
        .unwrap();
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let result = coordinator
            .request_cancel_from_ui(&parent_lease, &child.session.id)
            .unwrap();
        assert_eq!(result.status, SubagentStatus::Completed);
        assert!(result.cancellation_requested);
        let control = coordinator.issue_control(&parent_lease).unwrap();
        assert_eq!(
            coordinator
                .inputs_control(&control, &child.session.id)
                .unwrap()[0]
                .state,
            QueuedInputState::Skipped
        );
        assert!(matches!(
            coordinator.enqueue_input(
                &parent_lease,
                &child.session.id,
                "next",
                "Not allowed",
                TurnMode::Build
            ),
            Err(SubagentError::InputClosed(_))
        ));
        drop(coordinator);
        let coordinator = SubagentCoordinator::new(
            Arc::new(SessionStore::open(root.path()).unwrap()),
            Default::default(),
        )
        .unwrap();
        let snapshot = coordinator
            .list_children_snapshot_for_ui(&parent.id)
            .unwrap();
        assert_eq!(snapshot[0].open_inputs, 0);
        assert!(snapshot[0].agent.cancellation_requested);
    }

    #[test]
    fn desktop_cancellation_rechecks_entire_subtree_before_mutation() {
        let (_root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "ui-root", &spec())
            .unwrap();
        let child_run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        let descendant = coordinator
            .create_child(child_run.native_lease(), "ui-nested", &spec())
            .unwrap();
        let nested_run = coordinator
            .start_child(child_run.native_lease(), &descendant.session.id)
            .unwrap();
        coordinator
            .enqueue_input(
                &parent_lease,
                &child.session.id,
                "child-pending",
                "Retain child input",
                TurnMode::Build,
            )
            .unwrap();
        coordinator
            .enqueue_input(
                child_run.native_lease(),
                &descendant.session.id,
                "descendant-pending",
                "Retain descendant input",
                TurnMode::Build,
            )
            .unwrap();
        drop(nested_run);
        let registry_before = fs::read(coordinator.registry_path()).unwrap();
        let child_journal_before = sessions.load(&child.session.id).unwrap().unwrap().events;
        let nested_journal_before = sessions
            .load(&descendant.session.id)
            .unwrap()
            .unwrap()
            .events;
        assert!(matches!(
            coordinator.request_cancel_from_ui_with_affected(&parent_lease, &child.session.id),
            Err(SubagentError::CancellationRequiresRecovery(id)) if id == descendant.session.id
        ));
        assert_eq!(
            fs::read(coordinator.registry_path()).unwrap(),
            registry_before
        );
        assert_eq!(
            sessions.load(&child.session.id).unwrap().unwrap().events,
            child_journal_before
        );
        assert_eq!(
            sessions
                .load(&descendant.session.id)
                .unwrap()
                .unwrap()
                .events,
            nested_journal_before
        );
        assert!(!coordinator.cancellation_requested(&child_run).unwrap());
        let child = coordinator
            .list_children_snapshot_for_ui(&parent.id)
            .unwrap();
        assert_eq!(child[0].inputs[0].status, SubagentInputStatus::Pending);
        let nested = coordinator
            .list_children_snapshot_for_ui(&child[0].agent.session.id)
            .unwrap();
        assert_eq!(nested[0].agent.status, SubagentStatus::Interrupted);
        assert_eq!(nested[0].inputs[0].status, SubagentInputStatus::Pending);
        assert_eq!(
            fs::read(coordinator.registry_path()).unwrap(),
            registry_before
        );
    }

    #[test]
    fn desktop_cancellation_preserves_ready_admission_for_recovery() {
        let (_root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "ui-admitted", &spec())
            .unwrap();
        sessions
            .admit_native_turn(
                &child.session.id,
                "ui-admitted",
                &spec().prompt,
                "model-a",
                TurnMode::Build,
            )
            .unwrap();
        assert!(matches!(
            coordinator.request_cancel_from_ui(&parent_lease, &child.session.id),
            Err(SubagentError::CancellationRequiresRecovery(id)) if id == child.session.id
        ));
        let child = coordinator
            .list_children_for_ui(&parent.id)
            .unwrap()
            .remove(0);
        assert_eq!(child.status, SubagentStatus::Ready);
        assert!(!child.cancellation_requested);
        assert_eq!(
            sessions
                .load(&child.session.id)
                .unwrap()
                .unwrap()
                .events
                .len(),
            1
        );
    }

    #[test]
    fn child_lineage_survives_reopen_and_depth_is_limited() {
        let (root, sessions, coordinator, parent) = setup(SubagentLimits {
            max_depth: 1,
            ..Default::default()
        });
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "child", &spec())
            .unwrap();
        assert_eq!(
            child.session.subagent.as_ref().unwrap().parent_session_id,
            parent.id
        );
        assert_eq!(child.session.subagent.as_ref().unwrap().depth, 1);
        drop(parent_lease);
        drop(coordinator);
        drop(sessions);

        let reopened = Arc::new(SessionStore::open(root.path()).unwrap());
        let coordinator = SubagentCoordinator::new(
            Arc::clone(&reopened),
            SubagentLimits {
                max_depth: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let parent_lease = reopened.try_acquire_native_lease(&parent.id).unwrap();
        assert_eq!(
            coordinator.list_children(&parent_lease).unwrap()[0]
                .session
                .id,
            child.session.id
        );
        let child_lease = reopened
            .try_acquire_native_lease(&child.session.id)
            .unwrap();
        assert!(matches!(
            coordinator.create_child(&child_lease, "grandchild", &spec()),
            Err(SubagentError::DepthLimit)
        ));
    }

    #[test]
    fn dropped_run_reopens_interrupted_and_never_starts_as_fresh_work() {
        let (root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "child", &spec())
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        assert_eq!(
            coordinator.list_children(&parent_lease).unwrap()[0].status,
            SubagentStatus::Running
        );
        drop(run);
        drop(parent_lease);
        drop(coordinator);
        drop(sessions);

        let reopened = Arc::new(SessionStore::open(root.path()).unwrap());
        let coordinator =
            SubagentCoordinator::new(Arc::clone(&reopened), Default::default()).unwrap();
        let parent_lease = reopened.try_acquire_native_lease(&parent.id).unwrap();
        let state = &coordinator.list_children(&parent_lease).unwrap()[0];
        assert_eq!(state.status, SubagentStatus::Interrupted);
        assert!(matches!(
            coordinator.start_child(&parent_lease, &child.session.id),
            Err(SubagentError::InvalidTransition { .. })
        ));
        let resumed = coordinator
            .resume_child(&parent_lease, &child.session.id)
            .unwrap();
        assert_eq!(
            coordinator
                .finish_child(&resumed, SubagentOutcome::Completed)
                .unwrap()
                .status,
            SubagentStatus::Completed
        );
    }

    #[test]
    fn cancellation_is_durable_and_running_child_must_close() {
        let (root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let idle = coordinator
            .create_child(&parent_lease, "idle", &spec())
            .unwrap();
        let active = coordinator
            .create_child(&parent_lease, "active", &spec())
            .unwrap();
        assert_eq!(
            coordinator
                .request_cancel(&parent_lease, &idle.session.id)
                .unwrap()
                .status,
            SubagentStatus::Cancelled
        );
        let run = coordinator
            .start_child(&parent_lease, &active.session.id)
            .unwrap();
        let requested = coordinator
            .request_cancel(&parent_lease, &active.session.id)
            .unwrap();
        assert_eq!(requested.status, SubagentStatus::Running);
        assert!(requested.cancellation_requested);
        assert!(coordinator.cancellation_requested(&run).unwrap());
        drop(parent_lease);
        drop(coordinator);
        let second = SubagentCoordinator::new(Arc::clone(&sessions), Default::default()).unwrap();
        assert!(second.cancellation_requested(&run).unwrap());
        assert_eq!(
            second
                .finish_child(&run, SubagentOutcome::Cancelled)
                .unwrap()
                .status,
            SubagentStatus::Cancelled
        );
        drop(run);
        drop(second);
        drop(sessions);
        let reopened = Arc::new(SessionStore::open(root.path()).unwrap());
        let second = SubagentCoordinator::new(Arc::clone(&reopened), Default::default()).unwrap();
        let parent_lease = reopened.try_acquire_native_lease(&parent.id).unwrap();
        assert!(second
            .list_children(&parent_lease)
            .unwrap()
            .iter()
            .all(|child| child.status == SubagentStatus::Cancelled));
    }

    #[test]
    fn depth_and_concurrency_limits_apply_across_store_handles() {
        let (root, sessions, coordinator, parent) = setup(SubagentLimits {
            max_children_per_parent: 2,
            max_concurrent_children: 1,
            ..Default::default()
        });
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let first = coordinator
            .create_child(&parent_lease, "first", &spec())
            .unwrap();
        let second = coordinator
            .create_child(&parent_lease, "second", &spec())
            .unwrap();
        assert!(matches!(
            coordinator.create_child(&parent_lease, "third", &spec()),
            Err(SubagentError::ParentChildLimit(_))
        ));
        let first_run = coordinator
            .start_child(&parent_lease, &first.session.id)
            .unwrap();
        let other_store = Arc::new(SessionStore::open(root.path()).unwrap());
        let other = SubagentCoordinator::new(
            Arc::clone(&other_store),
            SubagentLimits {
                max_children_per_parent: 2,
                max_concurrent_children: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(matches!(
            other.start_child(&parent_lease, &second.session.id),
            Err(SubagentError::ConcurrencyLimit)
        ));
        other_store
            .rename(&first.session.id, Some("Renamed elsewhere"))
            .unwrap();
        coordinator
            .finish_child(&first_run, SubagentOutcome::Completed)
            .unwrap();
        assert_eq!(
            sessions
                .load(&first.session.id)
                .unwrap()
                .unwrap()
                .session
                .title
                .as_deref(),
            Some("Renamed elsewhere")
        );
        drop(first_run);
        let second_run = other
            .start_child(&parent_lease, &second.session.id)
            .unwrap();
        other
            .finish_child(&second_run, SubagentOutcome::Completed)
            .unwrap();
    }

    #[test]
    fn a_child_can_spawn_only_while_its_own_run_is_active() {
        let (_root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "child", &spec())
            .unwrap();
        let idle_lease = sessions
            .try_acquire_native_lease(&child.session.id)
            .unwrap();
        assert!(matches!(
            coordinator.create_child(&idle_lease, "grandchild", &spec()),
            Err(SubagentError::InvalidTransition { .. })
        ));
        drop(idle_lease);
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        let grandchild = coordinator
            .create_child(run.native_lease(), "grandchild", &spec())
            .unwrap();
        assert_eq!(grandchild.session.subagent.as_ref().unwrap().depth, 2);
    }

    #[test]
    fn cancellation_request_after_worker_crash_reopens_as_cancelled() {
        let (root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "child", &spec())
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        coordinator
            .request_cancel(&parent_lease, &child.session.id)
            .unwrap();
        drop(run);
        drop(parent_lease);
        drop(coordinator);
        drop(sessions);
        let reopened = Arc::new(SessionStore::open(root.path()).unwrap());
        let coordinator =
            SubagentCoordinator::new(Arc::clone(&reopened), Default::default()).unwrap();
        let parent_lease = reopened.try_acquire_native_lease(&parent.id).unwrap();
        let record = &coordinator.list_children(&parent_lease).unwrap()[0];
        assert_eq!(record.status, SubagentStatus::Cancelled);
        assert!(record.cancellation_requested);
    }

    #[test]
    fn cancellation_propagates_to_descendants() {
        let (_root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "child", &spec())
            .unwrap();
        let child_run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        let grandchild = coordinator
            .create_child(child_run.native_lease(), "grandchild", &spec())
            .unwrap();
        let grandchild_run = coordinator
            .start_child(child_run.native_lease(), &grandchild.session.id)
            .unwrap();
        coordinator
            .request_cancel(&parent_lease, &child.session.id)
            .unwrap();
        assert!(coordinator.cancellation_requested(&child_run).unwrap());
        assert!(coordinator.cancellation_requested(&grandchild_run).unwrap());
    }

    #[test]
    fn total_descendant_limit_counts_completed_work_after_restart() {
        let limits = SubagentLimits {
            max_total_descendants: 2,
            ..Default::default()
        };
        let (root, sessions, coordinator, parent) = setup(limits);
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "child", &spec())
            .unwrap();
        let child_run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        let grandchild = coordinator
            .create_child(child_run.native_lease(), "grandchild", &spec())
            .unwrap();
        let grandchild_run = coordinator
            .start_child(child_run.native_lease(), &grandchild.session.id)
            .unwrap();
        coordinator
            .finish_child(&grandchild_run, SubagentOutcome::Completed)
            .unwrap();
        drop(grandchild_run);
        coordinator
            .finish_child(&child_run, SubagentOutcome::Completed)
            .unwrap();
        drop(child_run);
        assert!(matches!(
            coordinator.create_child(&parent_lease, "another", &spec()),
            Err(SubagentError::TotalTaskLimit(id)) if id == parent.id
        ));
        drop(parent_lease);
        drop(coordinator);
        drop(sessions);

        let reopened = Arc::new(SessionStore::open(root.path()).unwrap());
        let coordinator = SubagentCoordinator::new(reopened.clone(), limits).unwrap();
        let parent_lease = reopened.try_acquire_native_lease(&parent.id).unwrap();
        assert_eq!(
            coordinator
                .create_child(&parent_lease, "child", &spec())
                .unwrap()
                .session
                .id,
            child.session.id
        );
        assert!(matches!(
            coordinator.create_child(&parent_lease, "another", &spec()),
            Err(SubagentError::TotalTaskLimit(_))
        ));
    }

    #[test]
    fn cancellation_of_nested_work_survives_reopened_store() {
        let (root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "child", &spec())
            .unwrap();
        let child_run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        let grandchild = coordinator
            .create_child(child_run.native_lease(), "grandchild", &spec())
            .unwrap();
        let grandchild_run = coordinator
            .start_child(child_run.native_lease(), &grandchild.session.id)
            .unwrap();
        coordinator
            .request_cancel(&parent_lease, &child.session.id)
            .unwrap();
        drop(grandchild_run);
        drop(child_run);
        drop(parent_lease);
        drop(coordinator);
        drop(sessions);

        let reopened = Arc::new(SessionStore::open(root.path()).unwrap());
        let coordinator = SubagentCoordinator::new(reopened.clone(), Default::default()).unwrap();
        let parent_lease = reopened.try_acquire_native_lease(&parent.id).unwrap();
        let child_record = &coordinator.list_children(&parent_lease).unwrap()[0];
        assert_eq!(child_record.status, SubagentStatus::Cancelled);
        assert!(child_record.cancellation_requested);
        let child_lease = reopened
            .try_acquire_native_lease(&child.session.id)
            .unwrap();
        let grandchild_record = &coordinator.list_children(&child_lease).unwrap()[0];
        assert_eq!(grandchild_record.session.id, grandchild.session.id);
        assert_eq!(grandchild_record.status, SubagentStatus::Cancelled);
        assert!(grandchild_record.cancellation_requested);
        assert!(matches!(
            coordinator.resume_child(&child_lease, &grandchild.session.id),
            Err(SubagentError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn confirmed_completion_wins_cancel_race_without_erasing_request() {
        let (_root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "child", &spec())
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        coordinator
            .request_cancel(&parent_lease, &child.session.id)
            .unwrap();
        let completed = coordinator
            .finish_child(&run, SubagentOutcome::Completed)
            .unwrap();
        assert_eq!(completed.status, SubagentStatus::Completed);
        assert!(completed.cancellation_requested);
        assert_eq!(
            coordinator
                .request_cancel(&parent_lease, &child.session.id)
                .unwrap(),
            completed
        );
    }

    #[test]
    fn spawn_retry_is_idempotent_and_changed_spec_conflicts_after_reopen() {
        let mut different_fields = spec();
        different_fields.prompt = "ab".into();
        different_fields.model = "c".into();
        let mut shifted_fields = spec();
        shifted_fields.prompt = "a".into();
        shifted_fields.model = "bc".into();
        assert_ne!(
            different_fields.digest_hex().unwrap(),
            shifted_fields.digest_hex().unwrap()
        );
        let limits = SubagentLimits {
            max_children_per_parent: 1,
            ..Default::default()
        };
        let (root, sessions, coordinator, parent) = setup(limits);
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let initial = coordinator
            .create_child(&parent_lease, "request-42", &spec())
            .unwrap();
        assert_eq!(
            coordinator
                .create_child(&parent_lease, "request-42", &spec())
                .unwrap()
                .session
                .id,
            initial.session.id
        );
        assert!(matches!(
            coordinator.create_child(&parent_lease, "request-43", &spec()),
            Err(SubagentError::ParentChildLimit(_))
        ));
        drop(parent_lease);
        drop(coordinator);
        drop(sessions);

        let reopened = Arc::new(SessionStore::open(root.path()).unwrap());
        let coordinator = SubagentCoordinator::new(Arc::clone(&reopened), limits).unwrap();
        let parent_lease = reopened.try_acquire_native_lease(&parent.id).unwrap();
        assert_eq!(
            coordinator
                .create_child(&parent_lease, "request-42", &spec())
                .unwrap()
                .session
                .id,
            initial.session.id
        );
        let mut changed = spec();
        changed.prompt = "Different request".into();
        assert!(matches!(
            coordinator.create_child(&parent_lease, "request-42", &changed),
            Err(SubagentError::Store(
                StoreError::ConflictingSubagentIdentity
            ))
        ));
    }

    #[test]
    fn pending_edit_and_claim_share_one_atomic_winner_across_handles() {
        let (root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "edit-race", &spec())
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        coordinator
            .finish_child(&run, SubagentOutcome::Completed)
            .unwrap();
        drop(run);
        coordinator
            .enqueue_input(
                &parent_lease,
                &child.session.id,
                "race-input",
                "Original",
                TurnMode::Build,
            )
            .unwrap();
        let other = SubagentCoordinator::new(
            Arc::new(SessionStore::open(root.path()).unwrap()),
            Default::default(),
        )
        .unwrap();
        let control = other.issue_control(&parent_lease).unwrap();
        let barrier = std::sync::Barrier::new(2);
        let (edited, claimed) = std::thread::scope(|scope| {
            let edit = scope.spawn(|| {
                barrier.wait();
                coordinator.edit_pending_input(
                    &parent_lease,
                    &child.session.id,
                    "race-input",
                    0,
                    "Edited",
                )
            });
            let claim = scope.spawn(|| {
                barrier.wait();
                other
                    .claim_next_input_control(&control, &child.session.id)
                    .unwrap()
                    .unwrap()
            });
            (edit.join().unwrap(), claim.join().unwrap())
        });
        let (run, input) = claimed;
        match edited {
            Ok(edit) => {
                assert_eq!(input.prompt, edit.prompt);
                assert_eq!(input.revision, 1);
            }
            Err(SubagentError::InputEditConflict) => {
                assert_eq!(input.prompt, "Original");
                assert_eq!(input.revision, 0);
            }
            Err(error) => panic!("unexpected edit failure: {error}"),
        }
        assert_eq!(
            coordinator
                .list_children_snapshot_for_ui(&parent.id)
                .unwrap()[0]
                .inputs[0]
                .prompt,
            input.prompt
        );
        coordinator
            .finish_child(&run, SubagentOutcome::Completed)
            .unwrap();
        drop(run);
    }

    #[test]
    fn registry_temporary_recovery_is_scoped_preserves_authority_and_rejects_corruption() {
        let (root, _sessions, coordinator, _parent) = setup(Default::default());
        let temporary = root
            .path()
            .join(format!(".subagents.{}.tmp", Uuid::new_v4()));
        fs::write(&temporary, b"uncommitted").unwrap();
        let unrelated = root.path().join(".subagents.user.tmp");
        fs::write(&unrelated, b"keep").unwrap();
        let non_v4 = root.path().join(format!(".subagents.{}.tmp", Uuid::nil()));
        fs::write(&non_v4, b"keep nil").unwrap();
        let directory = root
            .path()
            .join(format!(".subagents.{}.tmp", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        let link = {
            let target = root.path().join("external-file");
            fs::write(&target, b"external").unwrap();
            let link = root
                .path()
                .join(format!(".subagents.{}.tmp", Uuid::new_v4()));
            std::os::unix::fs::symlink(&target, &link).unwrap();
            (link, target)
        };
        // A corrupt authoritative registry prevents any cleanup, including a
        // perfectly named orphan. Preserve diagnostic evidence.
        fs::write(coordinator.registry_path(), b"{broken").unwrap();
        assert!(coordinator.recover_registry_temporaries().is_err());
        assert!(temporary.exists());
        let registry = serde_json::to_vec(&Registry::empty()).unwrap();
        fs::write(coordinator.registry_path(), &registry).unwrap();
        assert_eq!(coordinator.recover_registry_temporaries().unwrap(), 1);
        assert!(!temporary.exists());
        assert_eq!(fs::read(coordinator.registry_path()).unwrap(), registry);
        assert_eq!(fs::read(unrelated).unwrap(), b"keep");
        assert!(non_v4.exists());
        assert!(directory.is_dir());
        #[cfg(unix)]
        {
            assert!(fs::symlink_metadata(&link.0)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(fs::read(link.1).unwrap(), b"external");
        }
        assert_eq!(coordinator.recover_registry_temporaries().unwrap(), 0);
    }

    #[test]
    fn registry_temporary_recovery_waits_for_live_writer_lock() {
        let (root, sessions, coordinator, _parent) = setup(Default::default());
        let lock = coordinator.lock_registry().unwrap();
        let temporary = root
            .path()
            .join(format!(".subagents.{}.tmp", Uuid::new_v4()));
        fs::write(&temporary, b"live writer").unwrap();
        let other = SubagentCoordinator::new(sessions, Default::default()).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx.send(other.recover_registry_temporaries()).unwrap();
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(matches!(
            done_rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(temporary.exists());
        // The writer commits/renames its own file before releasing its lock.
        fs::remove_file(&temporary).unwrap();
        drop(lock);
        assert_eq!(
            done_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
                .unwrap(),
            0
        );
        thread.join().unwrap();
    }

    #[test]
    fn pending_skip_reopens_preserves_retries_and_only_removes_one_entry() {
        let (root, sessions, coordinator, parent) = setup(Default::default());
        let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&lease, "skip-child", &spec())
            .unwrap();
        let run = coordinator.start_child(&lease, &child.session.id).unwrap();
        coordinator
            .finish_child(&run, SubagentOutcome::Completed)
            .unwrap();
        drop(run);
        let original = coordinator
            .enqueue_input(
                &lease,
                &child.session.id,
                "skip-input",
                "Original",
                TurnMode::Build,
            )
            .unwrap();
        coordinator
            .enqueue_input(
                &lease,
                &child.session.id,
                "keep-input",
                "Keep",
                TurnMode::Build,
            )
            .unwrap();
        coordinator
            .edit_pending_input(&lease, &child.session.id, "skip-input", 0, "Edited")
            .unwrap();
        assert!(matches!(
            coordinator.skip_pending_input(&lease, &child.session.id, "skip-input", 0),
            Err(SubagentError::InputEditConflict)
        ));
        let stranger = sessions.create(root.path(), "native").unwrap();
        let stranger_lease = sessions.try_acquire_native_lease(&stranger.id).unwrap();
        assert!(matches!(
            coordinator.skip_pending_input(&stranger_lease, &child.session.id, "skip-input", 1),
            Err(SubagentError::NotDirectChild { .. })
        ));
        let skipped = coordinator
            .skip_pending_input(&lease, &child.session.id, "skip-input", 1)
            .unwrap();
        assert_eq!(skipped.status, SubagentInputStatus::Skipped);
        assert_eq!(skipped.revision, 2);
        let reopened = SubagentCoordinator::new(
            Arc::new(SessionStore::open(root.path()).unwrap()),
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            reopened
                .skip_pending_input(&lease, &child.session.id, "skip-input", 1)
                .unwrap(),
            skipped
        );
        let retried = reopened
            .enqueue_input(
                &lease,
                &child.session.id,
                "skip-input",
                "Original",
                TurnMode::Build,
            )
            .unwrap();
        assert_eq!(retried.digest, original.digest);
        assert_eq!(retried.prompt, "Edited");
        assert_eq!(retried.state, QueuedInputState::Skipped);
        let control = reopened.issue_control(&lease).unwrap();
        let (run, input) = reopened
            .claim_next_input_control(&control, &child.session.id)
            .unwrap()
            .unwrap();
        assert_eq!(input.id, "keep-input");
        assert!(matches!(
            reopened.skip_pending_input(&lease, &child.session.id, "keep-input", 0),
            Err(SubagentError::InputEditConflict)
        ));
        reopened
            .finish_child(&run, SubagentOutcome::Completed)
            .unwrap();
        drop(run);
        assert!(reopened
            .claim_next_input_control(&control, &child.session.id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn pending_skip_and_claim_share_one_atomic_winner_across_handles() {
        for _ in 0..8 {
            let (root, sessions, coordinator, parent) = setup(Default::default());
            let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
            let child = coordinator
                .create_child(&lease, "skip-race", &spec())
                .unwrap();
            let run = coordinator.start_child(&lease, &child.session.id).unwrap();
            coordinator
                .finish_child(&run, SubagentOutcome::Completed)
                .unwrap();
            drop(run);
            coordinator
                .enqueue_input(&lease, &child.session.id, "race", "Prompt", TurnMode::Build)
                .unwrap();
            let other = SubagentCoordinator::new(
                Arc::new(SessionStore::open(root.path()).unwrap()),
                Default::default(),
            )
            .unwrap();
            let control = other.issue_control(&lease).unwrap();
            let barrier = std::sync::Barrier::new(2);
            let (skip, claim) = std::thread::scope(|scope| {
                let a = scope.spawn(|| {
                    barrier.wait();
                    coordinator.skip_pending_input(&lease, &child.session.id, "race", 0)
                });
                let b = scope.spawn(|| {
                    barrier.wait();
                    other.claim_next_input_control(&control, &child.session.id)
                });
                (a.join().unwrap(), b.join().unwrap().unwrap())
            });
            match (skip, claim) {
                (Ok(input), None) => assert_eq!(input.status, SubagentInputStatus::Skipped),
                (Err(SubagentError::InputEditConflict), Some((run, input))) => {
                    assert_eq!(input.state, QueuedInputState::Active);
                    other
                        .finish_child(&run, SubagentOutcome::Completed)
                        .unwrap();
                }
                _ => panic!("skip and claim must have exactly one winner"),
            }
        }
    }

    #[test]
    fn pending_edit_reopens_preserves_original_retry_and_blocks_claimed_changes() {
        let (root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "edit-child", &spec())
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        coordinator
            .finish_child(&run, SubagentOutcome::Completed)
            .unwrap();
        drop(run);
        let original = coordinator
            .enqueue_input(
                &parent_lease,
                &child.session.id,
                "edit-input",
                "Original",
                TurnMode::Build,
            )
            .unwrap();
        assert!(matches!(
            coordinator.edit_pending_input(&parent_lease, &child.session.id, "edit-input", 0, " "),
            Err(SubagentError::InvalidInput)
        ));
        assert!(matches!(
            coordinator.edit_pending_input(
                &parent_lease,
                &child.session.id,
                "edit-input",
                0,
                &"ñ".repeat(MAX_INPUT_PROMPT_BYTES)
            ),
            Err(SubagentError::InvalidInput)
        ));
        let edited = coordinator
            .edit_pending_input(
                &parent_lease,
                &child.session.id,
                "edit-input",
                0,
                "Editado\ncon contexto ñ",
            )
            .unwrap();
        assert_eq!(edited.revision, 1);
        let reopened = SubagentCoordinator::new(
            Arc::new(SessionStore::open(root.path()).unwrap()),
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            reopened
                .edit_pending_input(
                    &parent_lease,
                    &child.session.id,
                    "edit-input",
                    0,
                    &edited.prompt
                )
                .unwrap(),
            edited
        );
        let retried = reopened
            .enqueue_input(
                &parent_lease,
                &child.session.id,
                "edit-input",
                "Original",
                TurnMode::Build,
            )
            .unwrap();
        assert_eq!(retried.digest, original.digest);
        assert_eq!(retried.prompt, edited.prompt);
        assert!(matches!(
            reopened.enqueue_input(
                &parent_lease,
                &child.session.id,
                "edit-input",
                &edited.prompt,
                TurnMode::Build
            ),
            Err(SubagentError::ConflictingInput)
        ));
        assert!(matches!(
            reopened.edit_pending_input(&parent_lease, &child.session.id, "edit-input", 0, "Otro"),
            Err(SubagentError::InputEditConflict)
        ));
        let stranger = sessions.create(root.path(), "native").unwrap();
        let stranger_lease = sessions.try_acquire_native_lease(&stranger.id).unwrap();
        assert!(matches!(
            reopened.edit_pending_input(
                &stranger_lease,
                &child.session.id,
                "edit-input",
                1,
                "Otro"
            ),
            Err(SubagentError::NotDirectChild { .. })
        ));
        let control = reopened.issue_control(&parent_lease).unwrap();
        let (run, claimed) = reopened
            .claim_next_input_control(&control, &child.session.id)
            .unwrap()
            .unwrap();
        assert_eq!(claimed.prompt, edited.prompt);
        assert_eq!(claimed.revision, 1);
        assert!(matches!(
            reopened.edit_pending_input(
                &parent_lease,
                &child.session.id,
                "edit-input",
                1,
                "Late edit"
            ),
            Err(SubagentError::InputEditConflict)
        ));
        reopened
            .finish_child(&run, SubagentOutcome::Completed)
            .unwrap();
        drop(run);
        assert!(matches!(
            reopened.edit_pending_input(
                &parent_lease,
                &child.session.id,
                "edit-input",
                1,
                "After completion"
            ),
            Err(SubagentError::InputEditConflict)
        ));
        assert_eq!(
            reopened.list_children_snapshot_for_ui(&parent.id).unwrap()[0].inputs[0].prompt,
            edited.prompt
        );
    }

    #[test]
    fn mailbox_snapshot_reopens_exact_prompts_and_all_lifecycle_states() {
        let (root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "inspect-mailbox", &spec())
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        coordinator
            .finish_child(&run, SubagentOutcome::Completed)
            .unwrap();
        drop(run);
        let prompt = format!("Primera línea\n{}", "contexto ñ\n".repeat(1000));
        for (id, text) in [
            ("first", prompt.as_str()),
            ("second", "Second"),
            ("third", "Third"),
        ] {
            coordinator
                .enqueue_input(&parent_lease, &child.session.id, id, text, TurnMode::Build)
                .unwrap();
        }
        let control = coordinator.issue_control(&parent_lease).unwrap();
        let snapshot = coordinator
            .list_children_snapshot_for_ui(&parent.id)
            .unwrap();
        assert_eq!(snapshot[0].open_inputs, 3);
        assert_eq!(snapshot[0].inputs[0].prompt, prompt);
        assert_eq!(snapshot[0].inputs[0].status, SubagentInputStatus::Pending);
        let (first, _) = coordinator
            .claim_next_input_control(&control, &child.session.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            coordinator
                .list_children_snapshot_for_ui(&parent.id)
                .unwrap()[0]
                .inputs[0]
                .status,
            SubagentInputStatus::Active
        );
        coordinator
            .finish_child(&first, SubagentOutcome::Completed)
            .unwrap();
        drop(first);
        let (second, _) = coordinator
            .claim_next_input_control(&control, &child.session.id)
            .unwrap()
            .unwrap();
        coordinator
            .request_cancel_control(&control, &child.session.id)
            .unwrap();
        let reopened = SubagentCoordinator::new(
            Arc::new(SessionStore::open(root.path()).unwrap()),
            Default::default(),
        )
        .unwrap();
        let bytes_before = std::fs::read(root.path().join("subagents.registry.json")).unwrap();
        let snapshot = reopened.list_children_snapshot_for_ui(&parent.id).unwrap();
        assert_eq!(snapshot[0].open_inputs, 1);
        assert_eq!(
            snapshot[0]
                .inputs
                .iter()
                .map(|input| input.status)
                .collect::<Vec<_>>(),
            vec![
                SubagentInputStatus::Done,
                SubagentInputStatus::Active,
                SubagentInputStatus::Skipped
            ]
        );
        assert_eq!(snapshot[0].inputs[0].prompt, prompt);
        assert_eq!(
            snapshot[0]
                .inputs
                .iter()
                .map(|input| input.id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second", "third"]
        );
        assert!(snapshot[0]
            .inputs
            .iter()
            .all(|input| input.mode == TurnMode::Build));
        assert_eq!(
            std::fs::read(root.path().join("subagents.registry.json")).unwrap(),
            bytes_before
        );
        coordinator
            .finish_child(&second, SubagentOutcome::Cancelled)
            .unwrap();
        drop(second);
    }

    #[test]
    fn ui_snapshot_needs_no_parent_lease_and_does_not_mutate_registry() {
        let (_root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "ui-child", &spec())
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        coordinator
            .enqueue_input(
                &parent_lease,
                &child.session.id,
                "ui-input",
                "Follow up",
                TurnMode::Build,
            )
            .unwrap();
        drop(parent_lease);
        let snapshot = coordinator
            .list_children_snapshot_for_ui(&parent.id)
            .unwrap();
        assert_eq!(snapshot[0].open_inputs, 1);
        assert!(!snapshot[0].agent.cancellation_requested);
        assert_eq!(
            coordinator.list_children_for_ui(&parent.id).unwrap()[0].status,
            SubagentStatus::Running
        );
        drop(run);
        assert_eq!(
            coordinator
                .read_registry()
                .unwrap()
                .state(&child.session.id)
                .status,
            SubagentStatus::Running
        );
        assert_eq!(
            coordinator.list_children_for_ui(&parent.id).unwrap()[0].status,
            SubagentStatus::Interrupted
        );
        assert_eq!(
            coordinator
                .read_registry()
                .unwrap()
                .state(&child.session.id)
                .status,
            SubagentStatus::Running
        );
        let reopened =
            SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()).unwrap();
        let snapshot = reopened.list_children_snapshot_for_ui(&parent.id).unwrap();
        assert_eq!(snapshot[0].agent.status, SubagentStatus::Interrupted);
        assert_eq!(snapshot[0].open_inputs, 1);
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        assert_eq!(
            coordinator.list_children(&parent_lease).unwrap()[0].status,
            SubagentStatus::Interrupted
        );
    }

    #[test]
    fn explicit_pause_releases_running_child_for_later_recovery() {
        let (_root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "paused", &spec())
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        assert_eq!(
            coordinator.pause_child(&run).unwrap().status,
            SubagentStatus::Interrupted
        );
        drop(run);
        let recovered = coordinator
            .resume_child(&parent_lease, &child.session.id)
            .unwrap();
        assert_eq!(recovered.session_id(), child.session.id);
    }

    #[test]
    fn follow_up_mailbox_is_idempotent_bounded_and_survives_claim_crash() {
        let (root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "mailbox", &spec())
            .unwrap();
        let control = coordinator.issue_control(&parent_lease).unwrap();
        let id = &child.session.id;
        let first = coordinator
            .enqueue_input(&parent_lease, id, "input-a", "Continue", TurnMode::Build)
            .unwrap();
        assert_eq!(first.state, QueuedInputState::Pending);
        assert_eq!(coordinator.open_input_count_for_ui(id).unwrap(), 1);
        assert_eq!(
            coordinator
                .enqueue_input(&parent_lease, id, "input-a", "Continue", TurnMode::Build)
                .unwrap(),
            first
        );
        assert!(matches!(
            coordinator.enqueue_input(&parent_lease, id, "input-a", "Changed", TurnMode::Build),
            Err(SubagentError::ConflictingInput)
        ));
        assert!(matches!(
            coordinator.enqueue_input(&parent_lease, id, "input-a", "Continue", TurnMode::Plan),
            Err(SubagentError::ConflictingInput)
        ));
        let initial = coordinator.start_child(&parent_lease, id).unwrap();
        coordinator
            .finish_child(&initial, SubagentOutcome::Completed)
            .unwrap();
        drop(initial);
        let (claimed, entry) = coordinator
            .claim_next_input_control(&control, id)
            .unwrap()
            .unwrap();
        assert_eq!(entry.id, "input-a");
        assert_eq!(entry.state, QueuedInputState::Active);
        assert_eq!(coordinator.open_input_count_for_ui(id).unwrap(), 1);
        drop(claimed);
        drop(parent_lease);
        drop(coordinator);
        drop(sessions);

        let reopened = Arc::new(SessionStore::open(root.path()).unwrap());
        let coordinator = SubagentCoordinator::new(reopened.clone(), Default::default()).unwrap();
        let parent_lease = reopened.try_acquire_native_lease(&parent.id).unwrap();
        let control = coordinator.issue_control(&parent_lease).unwrap();
        let (status, active, pending) = coordinator.input_state(&control, id).unwrap();
        assert_eq!(status, SubagentStatus::Interrupted);
        assert_eq!(active.unwrap().id, "input-a");
        assert_eq!(pending, 0);
        assert!(coordinator
            .claim_next_input_control(&control, id)
            .unwrap()
            .is_none());
        let resumed = coordinator.resume_child_control(&control, id).unwrap();
        coordinator
            .finish_child(&resumed, SubagentOutcome::Completed)
            .unwrap();
        assert_eq!(
            coordinator
                .lookup_input_control(&control, id, "input-a")
                .unwrap()
                .unwrap()
                .state,
            QueuedInputState::Done
        );
        assert_eq!(coordinator.open_input_count_for_ui(id).unwrap(), 0);
    }

    #[test]
    fn plan_mailbox_retains_mode_and_digest_after_reopening() {
        let (root, sessions, coordinator, parent) = setup(Default::default());
        let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator.create_child(&lease, "plan-mailbox", &spec()).unwrap();
        let first = coordinator.enqueue_input(&lease, &child.session.id, "input-plan", "Inspect", TurnMode::Plan).unwrap();
        assert_eq!(first.mode, TurnMode::Plan);
        assert!(matches!(coordinator.enqueue_input(&lease, &child.session.id, "input-plan", "Inspect", TurnMode::Build), Err(SubagentError::ConflictingInput)));
        drop(lease);
        drop(coordinator);
        drop(sessions);
        let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
        let coordinator = SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap();
        let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        assert_eq!(coordinator.enqueue_input(&lease, &child.session.id, "input-plan", "Inspect", TurnMode::Plan).unwrap(), first);
    }

    #[test]
    fn input_requires_direct_parent_and_cancellation_discards_pending_items() {
        let (_root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "mailbox", &spec())
            .unwrap();
        let stranger = sessions.create(&parent.workspace, "native").unwrap();
        let stranger_lease = sessions.try_acquire_native_lease(&stranger.id).unwrap();
        let id = &child.session.id;
        assert!(matches!(
            coordinator.enqueue_input(&stranger_lease, id, "wrong", "Continue", TurnMode::Build),
            Err(SubagentError::NotDirectChild { .. })
        ));
        for index in 0..MAX_INPUTS_PER_CHILD {
            coordinator
                .enqueue_input(
                    &parent_lease,
                    id,
                    &format!("input-{index}"),
                    "Continue",
                    TurnMode::Build,
                )
                .unwrap();
        }
        assert!(matches!(
            coordinator.enqueue_input(&parent_lease, id, "overflow", "Continue", TurnMode::Build),
            Err(SubagentError::InputLimit)
        ));
        coordinator.request_cancel(&parent_lease, id).unwrap();
        let control = coordinator.issue_control(&parent_lease).unwrap();
        let (status, active, pending) = coordinator.input_state(&control, id).unwrap();
        assert_eq!(status, SubagentStatus::Cancelled);
        assert!(active.is_none());
        assert_eq!(pending, 0);
        assert_eq!(
            coordinator
                .lookup_input_control(&control, id, "input-0")
                .unwrap()
                .unwrap()
                .state,
            QueuedInputState::Skipped
        );
        assert!(matches!(
            coordinator.enqueue_input(&parent_lease, id, "later", "Continue", TurnMode::Build),
            Err(SubagentError::InputClosed(_))
        ));
    }

    #[test]
    fn cancellation_during_running_turn_closes_queued_input_even_if_completion_wins() {
        let (_root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "mailbox", &spec())
            .unwrap();
        let id = &child.session.id;
        let run = coordinator.start_child(&parent_lease, id).unwrap();
        coordinator
            .enqueue_input(&parent_lease, id, "queued", "Continue", TurnMode::Build)
            .unwrap();
        coordinator.request_cancel(&parent_lease, id).unwrap();
        let terminal = coordinator
            .finish_child(&run, SubagentOutcome::Completed)
            .unwrap();
        assert_eq!(terminal.status, SubagentStatus::Completed);
        assert!(terminal.cancellation_requested);
        let control = coordinator.issue_control(&parent_lease).unwrap();
        let (_, active, pending) = coordinator.input_state(&control, id).unwrap();
        assert!(active.is_none());
        assert_eq!(pending, 0);
        assert_eq!(
            coordinator
                .lookup_input_control(&control, id, "queued")
                .unwrap()
                .unwrap()
                .state,
            QueuedInputState::Skipped
        );
        assert!(matches!(
            coordinator.enqueue_input(&parent_lease, id, "later", "Again", TurnMode::Build),
            Err(SubagentError::InputClosed(_))
        ));
    }
}
