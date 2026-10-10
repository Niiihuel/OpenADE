//! Durable intentions and receipts for human persistent-command controls.
//!
//! This ledger never touches a process or manufactures model/tool events. A
//! published `Started` receipt permits only its original caller to proceed;
//! reopening or retrying that receipt never authorizes another execution.
use crate::{NativeSessionLease, Session, SessionStore, StoreError};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use uuid::Uuid;

pub const NATIVE_COMMAND_OPERATION_VERSION: u32 = 1;
pub const MAX_NATIVE_COMMAND_INPUT_BYTES: usize = 8 * 1024;
pub const MAX_NATIVE_COMMAND_DIAGNOSTIC_BYTES: usize = 512 * 1024;
const MAX_REASON_BYTES: usize = 16 * 1024;
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
// Enough for a full escaped Requested rejection/cancellation receipt. Effects
// require a separate actor-sized reservation before Start.
const INTENTION_RESERVATION_BYTES: usize = 256 * 1024;
const MAX_OPERATIONS: usize = 256;
const MAX_CHANGES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeCommandOperationAction {
    Stdin { input: String, close_after: bool },
    Resize { columns: u16, rows: u16 },
    Terminate,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommandOperationRequest {
    pub operation_id: String,
    pub session_id: String,
    pub workspace: PathBuf,
    /// Process-local manager identity. Its processes are never restored.
    pub manager_generation: String,
    pub command_id: String,
    pub action: NativeCommandOperationAction,
    pub policy_digest: String,
    pub hooks_digest: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeCommandOperationPhase {
    BeforePreHooks,
    BeforeCommand,
    BeforePostHooks,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeCommandOperationOutcome {
    Succeeded {
        diagnostic: String,
    },
    /// A proven failure, rather than a claim that the process state rolled back.
    Failed {
        diagnostic: String,
    },
    EffectUncertain {
        diagnostic: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeCommandOperationState {
    Requested,
    Started {
        phase: NativeCommandOperationPhase,
        diagnostic: String,
    },
    Finished {
        outcome: NativeCommandOperationOutcome,
    },
    Rejected {
        reason: String,
    },
    Cancelled {
        reason: String,
    },
    /// Explicit human acknowledgment after inspection; not an effect result.
    Resolved {
        acknowledgment: String,
    },
}

impl NativeCommandOperationState {
    pub fn requires_verification(&self) -> bool {
        matches!(
            self,
            Self::Started { .. }
                | Self::Finished {
                    outcome: NativeCommandOperationOutcome::EffectUncertain { .. }
                }
        )
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Requested) || self.requires_verification()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommandOperationSnapshot {
    pub request: NativeCommandOperationRequest,
    pub revision: u64,
    pub state: NativeCommandOperationState,
    /// Exact command effect receipt saved before post-hooks. A later hook
    /// failure cannot erase or reinterpret this observation.
    pub effect_result: Option<NativeCommandOperationOutcome>,
    pub requested_at_ms: u64,
    pub updated_at_ms: u64,
    changes: Vec<Change>,
}

/// `newly_started == false` explicitly forbids repeating an external effect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeCommandOperationStartReceipt {
    pub snapshot: NativeCommandOperationSnapshot,
    pub newly_started: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeCommandOperationPhaseDiagnostic {
    pub revision: u64,
    pub at_ms: u64,
    pub phase: NativeCommandOperationPhase,
    /// Exact host diagnostic (including structured hook observations if
    /// supplied). Reading this history never acknowledges uncertain effects.
    pub diagnostic: String,
}

impl NativeCommandOperationSnapshot {
    pub fn phase_diagnostics(&self) -> Vec<NativeCommandOperationPhaseDiagnostic> {
        self.changes
            .iter()
            .filter_map(|change| {
                let (phase, diagnostic) = match &change.mutation {
                    Mutation::Start => (
                        NativeCommandOperationPhase::BeforePreHooks,
                        "Started before any configured pre-hook or command effect".into(),
                    ),
                    Mutation::Phase { phase, diagnostic } => (*phase, diagnostic.clone()),
                    _ => return None,
                };
                Some(NativeCommandOperationPhaseDiagnostic {
                    revision: change.expected_revision + 1,
                    at_ms: change.at_ms,
                    phase,
                    diagnostic,
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Mutation {
    Start,
    Phase {
        phase: NativeCommandOperationPhase,
        diagnostic: String,
    },
    EffectResult {
        outcome: NativeCommandOperationOutcome,
    },
    Finish {
        outcome: NativeCommandOperationOutcome,
    },
    Reject {
        reason: String,
    },
    Cancel {
        reason: String,
    },
    Resolve {
        acknowledgment: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    expected_revision: u64,
    at_ms: u64,
    mutation: Mutation,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    version: u32,
    session_id: String,
    operations: Vec<NativeCommandOperationSnapshot>,
    reservations: Vec<CapacityReservation>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityReservation {
    operation_id: String,
    prepared_budget: Option<usize>,
    remaining_bytes: usize,
}

#[derive(Debug, Error)]
pub enum NativeCommandOperationError {
    #[error("native automation read-only profile forbids manual command operations")]
    AutomationReadOnly,
    #[error("native command operation store failed: {0}")]
    Store(#[from] StoreError),
    #[error("native command operation I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("native command operation encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid or over-limit native command operation ledger")]
    Invalid,
    #[error("native command operation owner, arguments or revision conflict")]
    Conflict,
    #[error("native command operation remains pending or requires manual verification: {0}")]
    Pending(String),
}

pub struct NativeCommandOperationStore {
    sessions: Arc<SessionStore>,
}

impl NativeCommandOperationStore {
    pub fn new(sessions: Arc<SessionStore>) -> Self {
        Self { sessions }
    }

    /// Queue an exact intention, even while a worker owns the native lease.
    /// Serialized with session admissions/deletion; no permission or effect is
    /// implied. The actor must revalidate current policy, hooks and generation.
    pub fn enqueue_requested(
        &self,
        request: NativeCommandOperationRequest,
    ) -> Result<NativeCommandOperationSnapshot, NativeCommandOperationError> {
        validate_request(&request)?;
        self.sessions
            .with_native_command_operation_admission(&request.session_id, |session| {
                if session.native_automation.is_some() {
                    return Err(NativeCommandOperationError::AutomationReadOnly);
                }
                let _lock = lock(self.sessions.storage_root(), &request.session_id)?;
                validate_owner(&request, session)?;
                let mut ledger = read(self.sessions.storage_root(), session)?;
                if let Some(saved) = ledger
                    .operations
                    .iter()
                    .find(|op| op.request.operation_id == request.operation_id)
                {
                    return if saved.request == request {
                        Ok(saved.clone())
                    } else {
                        Err(NativeCommandOperationError::Conflict)
                    };
                }
                if session.archived {
                    return Err(NativeCommandOperationError::Conflict);
                }
                if ledger.operations.len() >= MAX_OPERATIONS {
                    return Err(NativeCommandOperationError::Invalid);
                }
                let now = now_ms()?;
                let snapshot = NativeCommandOperationSnapshot {
                    request: request.clone(),
                    revision: 0,
                    state: NativeCommandOperationState::Requested,
                    effect_result: None,
                    requested_at_ms: now,
                    updated_at_ms: now,
                    changes: vec![],
                };
                ledger.operations.push(snapshot.clone());
                ledger.reservations.push(CapacityReservation {
                    operation_id: request.operation_id.clone(),
                    prepared_budget: None,
                    remaining_bytes: INTENTION_RESERVATION_BYTES,
                });
                self.write(&ledger)?;
                Ok(snapshot)
            })
    }

    /// Reserve a conservative serialized upper bound *before* starting any
    /// hooks/effect. Queue writers cannot consume these bytes. This changes no
    /// operation revision and an exact preparation retry is idempotent.
    pub fn preflight_capacity(
        &self,
        lease: &NativeSessionLease,
        operation_id: &str,
        additional_serialized_bytes: usize,
    ) -> Result<(), NativeCommandOperationError> {
        let session = self.sessions.native_session_for_lease(lease)?;
        let _lock = lock(self.sessions.storage_root(), &session.id)?;
        let mut ledger = read(self.sessions.storage_root(), &session)?;
        let operation = ledger
            .operations
            .iter()
            .find(|op| op.request.operation_id == operation_id)
            .ok_or(NativeCommandOperationError::Conflict)?;
        if operation.state != NativeCommandOperationState::Requested
            || additional_serialized_bytes == 0
            || additional_serialized_bytes > MAX_FILE_BYTES
        {
            return Err(NativeCommandOperationError::Conflict);
        }
        let reservation = ledger
            .reservations
            .iter_mut()
            .find(|v| v.operation_id == operation_id)
            .ok_or(NativeCommandOperationError::Invalid)?;
        if let Some(budget) = reservation.prepared_budget {
            return if budget == additional_serialized_bytes {
                Ok(())
            } else {
                Err(NativeCommandOperationError::Conflict)
            };
        }
        reservation.prepared_budget = Some(additional_serialized_bytes);
        reservation.remaining_bytes = additional_serialized_bytes
            .checked_add(INTENTION_RESERVATION_BYTES)
            .ok_or(NativeCommandOperationError::Invalid)?;
        self.write(&ledger)
    }

    pub fn begin_started(
        &self,
        lease: &NativeSessionLease,
        operation_id: &str,
        expected_revision: u64,
    ) -> Result<NativeCommandOperationStartReceipt, NativeCommandOperationError> {
        let (snapshot, committed) =
            self.mutate(lease, operation_id, expected_revision, Mutation::Start)?;
        Ok(NativeCommandOperationStartReceipt {
            snapshot,
            newly_started: committed,
        })
    }

    /// Persist this boundary *before* its external effect. Repeated phases can
    /// identify individual hooks; their exact bounded diagnostic is immutable
    /// for that revision. The phase never proves any earlier hook succeeded.
    pub fn record_phase(
        &self,
        lease: &NativeSessionLease,
        operation_id: &str,
        expected_revision: u64,
        phase: NativeCommandOperationPhase,
        diagnostic: &str,
    ) -> Result<NativeCommandOperationSnapshot, NativeCommandOperationError> {
        self.mutate(
            lease,
            operation_id,
            expected_revision,
            Mutation::Phase {
                phase,
                diagnostic: diagnostic.into(),
            },
        )
        .map(|v| v.0)
    }

    pub fn finish(
        &self,
        lease: &NativeSessionLease,
        operation_id: &str,
        expected_revision: u64,
        outcome: NativeCommandOperationOutcome,
    ) -> Result<NativeCommandOperationSnapshot, NativeCommandOperationError> {
        self.mutate(
            lease,
            operation_id,
            expected_revision,
            Mutation::Finish { outcome },
        )
        .map(|v| v.0)
    }

    /// Save the known process effect before invoking any post-hook. An
    /// uncertain effect must be finished/inspected, never followed by hooks.
    pub fn record_effect_result(
        &self,
        lease: &NativeSessionLease,
        operation_id: &str,
        expected_revision: u64,
        outcome: NativeCommandOperationOutcome,
    ) -> Result<NativeCommandOperationSnapshot, NativeCommandOperationError> {
        self.mutate(
            lease,
            operation_id,
            expected_revision,
            Mutation::EffectResult { outcome },
        )
        .map(|v| v.0)
    }

    pub fn reject_requested(
        &self,
        lease: &NativeSessionLease,
        operation_id: &str,
        expected_revision: u64,
        reason: &str,
    ) -> Result<NativeCommandOperationSnapshot, NativeCommandOperationError> {
        self.mutate(
            lease,
            operation_id,
            expected_revision,
            Mutation::Reject {
                reason: reason.into(),
            },
        )
        .map(|v| v.0)
    }

    pub fn cancel_requested(
        &self,
        lease: &NativeSessionLease,
        operation_id: &str,
        expected_revision: u64,
        reason: &str,
    ) -> Result<NativeCommandOperationSnapshot, NativeCommandOperationError> {
        self.mutate(
            lease,
            operation_id,
            expected_revision,
            Mutation::Cancel {
                reason: reason.into(),
            },
        )
        .map(|v| v.0)
    }

    /// Withdraw a queued human intention while the model worker owns the
    /// lease. This cannot stop/acknowledge an already Started external effect.
    /// The same transaction/CAS as Start decides exactly one winner.
    pub fn cancel_queued_requested(
        &self,
        session_id: &str,
        operation_id: &str,
        expected_revision: u64,
        reason: &str,
    ) -> Result<NativeCommandOperationSnapshot, NativeCommandOperationError> {
        if !valid_uuid(operation_id) {
            return Err(NativeCommandOperationError::Invalid);
        }
        validate_reason(reason)?;
        self.sessions
            .with_native_command_operation_admission(session_id, |session| {
                let _lock = lock(self.sessions.storage_root(), &session.id)?;
                self.mutate_locked(
                    session,
                    operation_id,
                    expected_revision,
                    Mutation::Cancel {
                        reason: reason.into(),
                    },
                )
                .map(|v| v.0)
            })
    }

    /// Human inspection acknowledgment only. Does not claim success/failure,
    /// restore a process, rerun input/hooks, or edit the model journal.
    pub fn resolve_started(
        &self,
        lease: &NativeSessionLease,
        operation_id: &str,
        expected_revision: u64,
        acknowledgment: &str,
    ) -> Result<NativeCommandOperationSnapshot, NativeCommandOperationError> {
        self.mutate(
            lease,
            operation_id,
            expected_revision,
            Mutation::Resolve {
                acknowledgment: acknowledgment.into(),
            },
        )
        .map(|v| v.0)
    }

    pub fn snapshot(
        &self,
        session_id: &str,
        operation_id: &str,
    ) -> Result<Option<NativeCommandOperationSnapshot>, NativeCommandOperationError> {
        if !valid_uuid(operation_id) {
            return Err(NativeCommandOperationError::Invalid);
        }
        Ok(self
            .list(session_id)?
            .into_iter()
            .find(|op| op.request.operation_id == operation_id))
    }

    pub fn list(
        &self,
        session_id: &str,
    ) -> Result<Vec<NativeCommandOperationSnapshot>, NativeCommandOperationError> {
        let session = self
            .sessions
            .load(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.into()))?
            .session;
        if session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession.into());
        }
        Ok(read(self.sessions.storage_root(), &session)?.operations)
    }

    pub fn list_pending(
        &self,
        session_id: &str,
    ) -> Result<Vec<NativeCommandOperationSnapshot>, NativeCommandOperationError> {
        Ok(self
            .list(session_id)?
            .into_iter()
            .filter(|op| op.state.is_pending())
            .collect())
    }

    /// Recovery gate for a leased actor. Requested input must be explicitly
    /// consumed/rejected; Started input must be inspected, never replayed.
    pub fn require_no_pending(
        &self,
        lease: &NativeSessionLease,
    ) -> Result<(), NativeCommandOperationError> {
        let session = self.sessions.native_session_for_lease(lease)?;
        let _lock = lock_and_require_no_pending(self.sessions.storage_root(), &session)?;
        Ok(())
    }

    /// An actor can consume Requested items under its lease. It may never
    /// proceed past an unfinished external effect merely by resuming a turn.
    pub fn require_no_uncertain(
        &self,
        lease: &NativeSessionLease,
    ) -> Result<(), NativeCommandOperationError> {
        let session = self.sessions.native_session_for_lease(lease)?;
        let _lock = lock(self.sessions.storage_root(), &session.id)?;
        if let Some(op) = read(self.sessions.storage_root(), &session)?
            .operations
            .iter()
            .find(|op| op.state.requires_verification())
        {
            return Err(NativeCommandOperationError::Pending(
                op.request.operation_id.clone(),
            ));
        }
        Ok(())
    }

    pub fn recover_temporaries(
        &self,
        lease: &NativeSessionLease,
    ) -> Result<usize, NativeCommandOperationError> {
        let session = self.sessions.native_session_for_lease(lease)?;
        let _lock = lock(self.sessions.storage_root(), &session.id)?;
        read(self.sessions.storage_root(), &session)?;
        cleanup_temporaries(self.sessions.storage_root(), &session.id)
    }

    fn mutate(
        &self,
        lease: &NativeSessionLease,
        operation_id: &str,
        expected_revision: u64,
        mutation: Mutation,
    ) -> Result<(NativeCommandOperationSnapshot, bool), NativeCommandOperationError> {
        let session = self.sessions.native_session_for_lease(lease)?;
        if !valid_uuid(operation_id) {
            return Err(NativeCommandOperationError::Invalid);
        }
        let _lock = lock(self.sessions.storage_root(), &session.id)?;
        // Archiving shares this ledger lock. Re-read metadata after acquiring
        // it so a pre-lock projection cannot authorize a Start after archive.
        let session = self.sessions.native_session_for_lease(lease)?;
        self.mutate_locked(&session, operation_id, expected_revision, mutation)
    }

    /// Caller holds the operation ledger lock. Intention-only cancellation
    /// also holds the journal transaction; effects require the native lease.
    fn mutate_locked(
        &self,
        session: &Session,
        operation_id: &str,
        expected_revision: u64,
        mutation: Mutation,
    ) -> Result<(NativeCommandOperationSnapshot, bool), NativeCommandOperationError> {
        let mut ledger = read(self.sessions.storage_root(), session)?;
        let before_bytes = serde_json::to_vec(&ledger)?.len();
        let snapshot = ledger
            .operations
            .iter_mut()
            .find(|op| op.request.operation_id == operation_id)
            .ok_or(NativeCommandOperationError::Conflict)?;
        if let Some(previous) = snapshot
            .changes
            .iter()
            .find(|change| change.expected_revision == expected_revision)
        {
            return if previous.mutation == mutation {
                Ok((snapshot.clone(), false))
            } else {
                Err(NativeCommandOperationError::Conflict)
            };
        }
        if snapshot.revision != expected_revision
            || snapshot.changes.len() >= MAX_CHANGES
            || (session.archived && matches!(mutation, Mutation::Start))
        {
            return Err(NativeCommandOperationError::Conflict);
        }
        if matches!(mutation, Mutation::Start)
            && !ledger
                .reservations
                .iter()
                .any(|v| v.operation_id == operation_id && v.prepared_budget.is_some())
        {
            return Err(NativeCommandOperationError::Conflict);
        }
        if matches!(
            mutation,
            Mutation::Finish {
                outcome: NativeCommandOperationOutcome::Succeeded { .. }
            }
        ) && !matches!(
            snapshot.effect_result,
            Some(NativeCommandOperationOutcome::Succeeded { .. })
        ) {
            return Err(NativeCommandOperationError::Conflict);
        }
        snapshot.state = transition(&snapshot.state, &mutation)?;
        if let Mutation::EffectResult { outcome } = &mutation {
            if snapshot.effect_result.is_some() {
                return Err(NativeCommandOperationError::Conflict);
            }
            snapshot.effect_result = Some(outcome.clone());
        }
        let at_ms = now_ms()?.max(snapshot.updated_at_ms);
        snapshot.changes.push(Change {
            expected_revision,
            at_ms,
            mutation,
        });
        snapshot.revision += 1;
        snapshot.updated_at_ms = at_ms;
        validate_snapshot(snapshot, session)?;
        let result = snapshot.clone();
        let consumed = serde_json::to_vec(&ledger)?
            .len()
            .saturating_sub(before_bytes);
        let reservation = ledger
            .reservations
            .iter_mut()
            .find(|v| v.operation_id == operation_id)
            .ok_or(NativeCommandOperationError::Invalid)?;
        reservation.remaining_bytes = reservation
            .remaining_bytes
            .checked_sub(consumed)
            .ok_or(NativeCommandOperationError::Invalid)?;
        if matches!(
            result.state,
            NativeCommandOperationState::Finished {
                outcome: NativeCommandOperationOutcome::EffectUncertain { .. }
            }
        ) {
            if reservation.remaining_bytes < INTENTION_RESERVATION_BYTES {
                return Err(NativeCommandOperationError::Invalid);
            }
            reservation.remaining_bytes = INTENTION_RESERVATION_BYTES;
        } else if !matches!(
            result.state,
            NativeCommandOperationState::Requested | NativeCommandOperationState::Started { .. }
        ) {
            ledger
                .reservations
                .retain(|v| v.operation_id != operation_id);
        }
        self.write(&ledger)?;
        Ok((result, true))
    }

    fn write(&self, ledger: &Ledger) -> Result<(), NativeCommandOperationError> {
        let bytes = serde_json::to_vec(ledger)?;
        let reserved: usize = ledger
            .reservations
            .iter()
            .try_fold(0usize, |sum, v| sum.checked_add(v.remaining_bytes))
            .ok_or(NativeCommandOperationError::Invalid)?;
        if bytes
            .len()
            .checked_add(reserved)
            .and_then(|v| v.checked_add(1))
            .is_none_or(|v| v > MAX_FILE_BYTES)
        {
            return Err(NativeCommandOperationError::Invalid);
        }
        let root = self.sessions.storage_root();
        cleanup_temporaries(root, &ledger.session_id)?;
        let temporary = root.join(format!(
            ".{}.native-command-operations.{}.tmp",
            ledger.session_id,
            Uuid::new_v4()
        ));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            #[cfg(test)]
            publication_cut(ledger, "temporary-synced");
            fs::rename(&temporary, ledger_path(root, &ledger.session_id))?;
            #[cfg(test)]
            publication_cut(ledger, "renamed");
            #[cfg(unix)]
            File::open(root)?.sync_all()?;
            #[cfg(test)]
            publication_cut(ledger, "directory-synced");
            Ok::<(), NativeCommandOperationError>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

/// Used under SessionStore's journal transaction. Keep the returned OS lock
/// until the new admission is published, so enqueue cannot race that gate.
pub(crate) fn lock_and_require_no_pending(
    root: &Path,
    session: &Session,
) -> Result<File, NativeCommandOperationError> {
    let guard = lock(root, &session.id)?;
    if let Some(op) = read(root, session)?
        .operations
        .iter()
        .find(|op| op.state.is_pending())
    {
        return Err(NativeCommandOperationError::Pending(
            op.request.operation_id.clone(),
        ));
    }
    Ok(guard)
}

fn lock(root: &Path, id: &str) -> Result<File, NativeCommandOperationError> {
    if !valid_session_id(id) {
        return Err(NativeCommandOperationError::Invalid);
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(format!(".{id}.native-command-operations.lock")))?;
    file.lock()?;
    Ok(file)
}

fn ledger_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.native-command-operations.json"))
}

fn read(root: &Path, session: &Session) -> Result<Ledger, NativeCommandOperationError> {
    if session.backend != "native" || !valid_session_id(&session.id) {
        return Err(NativeCommandOperationError::Invalid);
    }
    let file = match File::open(ledger_path(root, &session.id)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Ledger {
                version: NATIVE_COMMAND_OPERATION_VERSION,
                session_id: session.id.clone(),
                operations: vec![],
                reservations: vec![],
            })
        }
        Err(e) => return Err(e.into()),
    };
    let mut bytes = vec![];
    file.take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(NativeCommandOperationError::Invalid);
    }
    let ledger: Ledger = serde_json::from_slice(&bytes)?;
    if ledger.version != NATIVE_COMMAND_OPERATION_VERSION
        || ledger.session_id != session.id
        || ledger.operations.len() > MAX_OPERATIONS
    {
        return Err(NativeCommandOperationError::Invalid);
    }
    let mut ids = HashSet::new();
    for op in &ledger.operations {
        validate_snapshot(op, session)?;
        if !ids.insert(&op.request.operation_id) {
            return Err(NativeCommandOperationError::Invalid);
        }
    }
    let mut reservations = HashSet::new();
    let mut reserved = 0usize;
    for reservation in &ledger.reservations {
        if !reservations.insert(&reservation.operation_id)
            || reservation.remaining_bytes > MAX_FILE_BYTES
            || reservation
                .prepared_budget
                .is_some_and(|v| v == 0 || v > MAX_FILE_BYTES)
            || !ledger.operations.iter().any(|op| {
                op.request.operation_id == reservation.operation_id && op.state.is_pending()
            })
        {
            return Err(NativeCommandOperationError::Invalid);
        }
        reserved = reserved
            .checked_add(reservation.remaining_bytes)
            .ok_or(NativeCommandOperationError::Invalid)?;
    }
    if bytes
        .len()
        .checked_add(reserved)
        .is_none_or(|v| v > MAX_FILE_BYTES)
        || ledger
            .operations
            .iter()
            .any(|op| op.state.is_pending() && !reservations.contains(&op.request.operation_id))
    {
        return Err(NativeCommandOperationError::Invalid);
    }
    Ok(ledger)
}

fn cleanup_temporaries(
    root: &Path,
    session_id: &str,
) -> Result<usize, NativeCommandOperationError> {
    let prefix = format!(".{session_id}.native-command-operations.");
    let mut count = 0;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
            .and_then(|name| name.strip_suffix(".tmp"))
        else {
            continue;
        };
        if valid_uuid(id) && entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
            count += 1;
        }
    }
    #[cfg(unix)]
    if count > 0 {
        File::open(root)?.sync_all()?;
    }
    Ok(count)
}

/// Caller already froze the journal, owns its native lease, and validated the
/// settled published ledger under its OS lock before deleting metadata.
pub(crate) fn remove_settled_files(
    root: &Path,
    session_id: &str,
) -> Result<bool, NativeCommandOperationError> {
    let temporary_count = cleanup_temporaries(root, session_id)?;
    let removed = match fs::remove_file(ledger_path(root, session_id)) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    #[cfg(unix)]
    if removed {
        File::open(root)?.sync_all()?;
    }
    Ok(removed || temporary_count > 0)
}

fn transition(
    state: &NativeCommandOperationState,
    mutation: &Mutation,
) -> Result<NativeCommandOperationState, NativeCommandOperationError> {
    use NativeCommandOperationState as S;
    Ok(match (state, mutation) {
        (S::Requested, Mutation::Start) => S::Started {
            phase: NativeCommandOperationPhase::BeforePreHooks,
            diagnostic: "Started before any configured pre-hook or command effect".into(),
        },
        (
            S::Started {
                phase: previous, ..
            },
            Mutation::Phase { phase, diagnostic },
        ) if phase >= previous
            && (*phase != NativeCommandOperationPhase::BeforePostHooks
                || *previous == NativeCommandOperationPhase::BeforePostHooks) =>
        {
            validate_text(diagnostic)?;
            S::Started {
                phase: *phase,
                diagnostic: diagnostic.clone(),
            }
        }
        (S::Started { .. }, Mutation::Finish { outcome }) => {
            if matches!(outcome, NativeCommandOperationOutcome::Succeeded { .. })
                && !matches!(
                    state,
                    S::Started {
                        phase: NativeCommandOperationPhase::BeforePostHooks,
                        ..
                    }
                )
            {
                return Err(NativeCommandOperationError::Conflict);
            }
            let (NativeCommandOperationOutcome::Succeeded { diagnostic }
            | NativeCommandOperationOutcome::Failed { diagnostic }
            | NativeCommandOperationOutcome::EffectUncertain { diagnostic }) = outcome;
            validate_text(diagnostic)?;
            S::Finished {
                outcome: outcome.clone(),
            }
        }
        (
            S::Started {
                phase: NativeCommandOperationPhase::BeforeCommand,
                ..
            },
            Mutation::EffectResult { outcome },
        ) => {
            let (NativeCommandOperationOutcome::Succeeded { diagnostic }
            | NativeCommandOperationOutcome::Failed { diagnostic }) = outcome
            else {
                return Err(NativeCommandOperationError::Conflict);
            };
            validate_text(diagnostic)?;
            S::Started {
                phase: NativeCommandOperationPhase::BeforePostHooks,
                diagnostic: diagnostic.clone(),
            }
        }
        (S::Requested, Mutation::Reject { reason }) => {
            validate_reason(reason)?;
            S::Rejected {
                reason: reason.clone(),
            }
        }
        (S::Requested, Mutation::Cancel { reason }) => {
            validate_reason(reason)?;
            S::Cancelled {
                reason: reason.clone(),
            }
        }
        (state, Mutation::Resolve { acknowledgment }) if state.requires_verification() => {
            validate_reason(acknowledgment)?;
            S::Resolved {
                acknowledgment: acknowledgment.clone(),
            }
        }
        _ => return Err(NativeCommandOperationError::Conflict),
    })
}

fn validate_snapshot(
    snapshot: &NativeCommandOperationSnapshot,
    session: &Session,
) -> Result<(), NativeCommandOperationError> {
    validate_request(&snapshot.request)?;
    validate_owner(&snapshot.request, session)?;
    if snapshot.revision != snapshot.changes.len() as u64
        || snapshot.changes.len() > MAX_CHANGES
        || snapshot.requested_at_ms == 0
    {
        return Err(NativeCommandOperationError::Invalid);
    }
    let mut projected = NativeCommandOperationState::Requested;
    let mut effect_result = None;
    let mut time = snapshot.requested_at_ms;
    for (index, change) in snapshot.changes.iter().enumerate() {
        if change.expected_revision != index as u64 || change.at_ms < time {
            return Err(NativeCommandOperationError::Invalid);
        }
        if matches!(
            change.mutation,
            Mutation::Finish {
                outcome: NativeCommandOperationOutcome::Succeeded { .. }
            }
        ) && !matches!(
            effect_result,
            Some(NativeCommandOperationOutcome::Succeeded { .. })
        ) {
            return Err(NativeCommandOperationError::Invalid);
        }
        projected = transition(&projected, &change.mutation)
            .map_err(|_| NativeCommandOperationError::Invalid)?;
        if let Mutation::EffectResult { outcome } = &change.mutation {
            if effect_result.is_some() {
                return Err(NativeCommandOperationError::Invalid);
            }
            effect_result = Some(outcome.clone());
        }
        time = change.at_ms;
    }
    if projected != snapshot.state
        || effect_result != snapshot.effect_result
        || time != snapshot.updated_at_ms
    {
        return Err(NativeCommandOperationError::Invalid);
    }
    Ok(())
}

fn validate_owner(
    request: &NativeCommandOperationRequest,
    session: &Session,
) -> Result<(), NativeCommandOperationError> {
    if request.session_id != session.id
        || request.workspace != fs::canonicalize(&session.workspace)?
        || request.workspace != fs::canonicalize(&request.workspace)?
    {
        return Err(NativeCommandOperationError::Conflict);
    }
    Ok(())
}

fn validate_request(
    request: &NativeCommandOperationRequest,
) -> Result<(), NativeCommandOperationError> {
    if !valid_session_id(&request.session_id)
        || !valid_uuid(&request.operation_id)
        || !valid_uuid(&request.manager_generation)
        || !valid_uuid(&request.command_id)
        || !request.workspace.is_absolute()
        || request.workspace.as_os_str().len() > 16 * 1024
        || !valid_digest(&request.policy_digest)
        || !valid_digest(&request.hooks_digest)
    {
        return Err(NativeCommandOperationError::Invalid);
    }
    match &request.action {
        NativeCommandOperationAction::Stdin { input, close_after }
            if input.len() > MAX_NATIVE_COMMAND_INPUT_BYTES
                || (input.is_empty() && !close_after) =>
        {
            Err(NativeCommandOperationError::Invalid)
        }
        NativeCommandOperationAction::Resize { columns, rows }
            if !(20..=500).contains(columns) || !(5..=200).contains(rows) =>
        {
            Err(NativeCommandOperationError::Invalid)
        }
        _ => Ok(()),
    }
}

fn validate_text(text: &str) -> Result<(), NativeCommandOperationError> {
    if text.trim().is_empty() || text.len() > MAX_NATIVE_COMMAND_DIAGNOSTIC_BYTES {
        Err(NativeCommandOperationError::Invalid)
    } else {
        Ok(())
    }
}
fn validate_reason(text: &str) -> Result<(), NativeCommandOperationError> {
    if text.trim().is_empty() || text.len() > MAX_REASON_BYTES {
        Err(NativeCommandOperationError::Invalid)
    } else {
        Ok(())
    }
}
fn valid_uuid(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|uuid| {
        uuid.get_version() == Some(uuid::Version::Random) && uuid.hyphenated().to_string() == id
    })
}
fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn now_ms() -> Result<u64, NativeCommandOperationError> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| NativeCommandOperationError::Invalid)?
            .as_millis(),
    )
    .map_err(|_| NativeCommandOperationError::Invalid)
}

#[cfg(test)]
fn publication_cut(ledger: &Ledger, stage: &str) {
    if std::env::var("OPENIDE_COMMAND_OPERATION_CUT_ACTION").as_deref() == Ok("reserve")
        && ledger
            .reservations
            .last()
            .is_none_or(|v| v.prepared_budget.is_none())
    {
        return;
    }
    if std::env::var("OPENIDE_COMMAND_OPERATION_CUT_STAGE").as_deref() == Ok(stage)
        && ledger.operations.last().is_some_and(|op| {
            std::env::var("OPENIDE_COMMAND_OPERATION_CUT_REVISION")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                == Some(op.revision)
        })
    {
        std::process::exit(86);
    }
}

#[cfg(test)]
#[path = "native_command_operation_tests.rs"]
mod tests;
