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

const REGISTRY_VERSION: u32 = 2;
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
    pub digest: String,
    pub state: QueuedInputState,
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

#[derive(Default, Serialize, Deserialize)]
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
        if parent.subagent.is_some() && registry.state(&parent.id).status != SubagentStatus::Running
        {
            return Err(SubagentError::InvalidTransition {
                child: parent.id.clone(),
                from: registry.state(&parent.id).status,
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
            || mode != TurnMode::Build
        {
            return Err(SubagentError::InvalidInput);
        }
        let mut hasher = Sha256::new();
        hasher.update(b"openide-subagent-input-v1\0");
        hasher.update((prompt.len() as u64).to_le_bytes());
        hasher.update(prompt.as_bytes());
        hasher.update(b"build");
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
        };
        inputs.push(entry.clone());
        self.write_registry(&registry)?;
        Ok(entry)
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
        let control = self.issue_control(parent_lease)?;
        self.request_cancel_control(&control, child_id)
    }

    pub fn request_cancel_control(
        &self,
        control: &SubagentControl,
        child_id: &str,
    ) -> Result<SubagentRecord, SubagentError> {
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let sessions = self.reconcile(&mut registry)?;
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
        let mut changed = false;
        for id in affected {
            let mut state = registry.state(&id);
            if !state.status.terminal() {
                state.cancellation_requested = true;
                if state.status != SubagentStatus::Running {
                    state.status = SubagentStatus::Cancelled;
                }
                registry.children.insert(id.clone(), state);
                changed = true;
            } else if state.status == SubagentStatus::Completed && !state.cancellation_requested {
                state.cancellation_requested = true;
                registry.children.insert(id.clone(), state);
                changed = true;
            }
            if let Some(inputs) = registry.inputs.get_mut(&id) {
                for input in inputs {
                    if input.state == QueuedInputState::Pending {
                        input.state = QueuedInputState::Skipped;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.write_registry(&registry)?;
        }
        Ok(record(child.clone(), registry.state(child_id)))
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
        if registry.version != 1 && registry.version != REGISTRY_VERSION {
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
            fs::rename(&temporary, self.registry_path())?;
            #[cfg(unix)]
            File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
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
    fn ui_snapshot_needs_no_parent_lease_and_does_not_mutate_registry() {
        let (_root, sessions, coordinator, parent) = setup(Default::default());
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = coordinator
            .create_child(&parent_lease, "ui-child", &spec())
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        drop(parent_lease);
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
            coordinator.enqueue_input(&parent_lease, id, "input-b", "Plan", TurnMode::Plan),
            Err(SubagentError::InvalidInput)
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
