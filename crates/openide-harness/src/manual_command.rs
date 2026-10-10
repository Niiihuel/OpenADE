//! Human persistent-command operations, independent of model tool calls.
//!
//! Enqueue publishes only a confirmed intention. The current lease owner is
//! the sole actor allowed to validate policy and perform its external effect.
use crate::{
    run_manual_command_hook, AgentError, CommandHooks, CommandIsolation, HookDecision, HookPhase,
    HookRunResult, ManualCommandHookInvocation, PermissionMode, PermissionPolicy,
    PersistentCommandControlInspection, PersistentCommandManager,
};
use openide_core::{
    NativeCommandOperationAction, NativeCommandOperationError, NativeCommandOperationOutcome,
    NativeCommandOperationPhase, NativeCommandOperationRequest, NativeCommandOperationSnapshot,
    NativeCommandOperationState, NativeCommandOperationStore, NativeSessionLease, SessionStore,
    ToolCall,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct NativeManualCommandPolicy {
    pub permissions: PermissionPolicy,
    pub run_command_enabled: bool,
    pub isolation: CommandIsolation,
    pub hooks: Arc<CommandHooks>,
}

impl NativeManualCommandPolicy {
    pub fn digest_hex(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"openide-human-command-policy-v1\0");
        hash.update(self.permissions.digest_hex());
        hash.update([u8::from(self.run_command_enabled)]);
        hash.update(match self.isolation {
            CommandIsolation::HostUnrestricted => b"host".as_slice(),
            CommandIsolation::WorkspaceWriteOffline => b"workspace_offline".as_slice(),
        });
        format!("{:x}", hash.finalize())
    }
}

pub type NativeManualCommandPolicySource =
    dyn Fn() -> Result<NativeManualCommandPolicy, String> + Send + Sync;

/// Called after the exact snapshot was published and synced. Failure stops
/// the actor at that saved boundary; it never authorizes repeating effects.
pub trait NativeManualCommandObserver: Send + Sync {
    fn on_persisted_operation(
        &self,
        snapshot: &NativeCommandOperationSnapshot,
    ) -> Result<(), AgentError>;
}

pub struct NativeManualCommandActor {
    sessions: Arc<SessionStore>,
    operations: NativeCommandOperationStore,
    owner: String,
    manager: Option<Arc<PersistentCommandManager>>,
    source: Arc<NativeManualCommandPolicySource>,
    observer: Option<Arc<dyn NativeManualCommandObserver>>,
    notify: Notify,
    actor_lock: AsyncMutex<()>,
    active_operation: Mutex<Option<String>>,
}

fn runtime(error: impl std::fmt::Display) -> AgentError {
    AgentError::Runtime(error.to_string())
}

impl NativeManualCommandActor {
    pub fn new(
        sessions: Arc<SessionStore>,
        owner: String,
        manager: Option<Arc<PersistentCommandManager>>,
        source: Arc<NativeManualCommandPolicySource>,
    ) -> Result<Self, AgentError> {
        let session = sessions.load(&owner).map_err(runtime)?.ok_or_else(|| {
            AgentError::InvalidRequest("manual command session does not exist".into())
        })?;
        if session.session.backend != "native"
            || session.session.archived
            || session.session.native_automation.is_some()
        {
            return Err(AgentError::InvalidRequest(
                "manual commands require an interactive native session".into(),
            ));
        }
        if let Some(manager) = &manager {
            if manager.native_session_owner() != Some(owner.as_str()) {
                return Err(AgentError::InvalidRequest(
                    "manual command manager belongs to another session".into(),
                ));
            }
        }
        Ok(Self {
            operations: NativeCommandOperationStore::new(sessions.clone()),
            sessions,
            owner,
            manager,
            source,
            observer: None,
            notify: Notify::new(),
            actor_lock: AsyncMutex::new(()),
            active_operation: Mutex::new(None),
        })
    }

    pub fn owner_session_id(&self) -> &str {
        &self.owner
    }
    pub fn with_operation_observer(
        mut self,
        observer: Arc<dyn NativeManualCommandObserver>,
    ) -> Self {
        self.observer = Some(observer);
        self
    }
    fn observe(&self, snapshot: &NativeCommandOperationSnapshot) -> Result<(), AgentError> {
        if let Some(observer) = &self.observer {
            observer.on_persisted_operation(snapshot)?;
        }
        Ok(())
    }
    pub fn sessions(&self) -> &Arc<SessionStore> {
        &self.sessions
    }
    /// Process-local observation only. After a dropped worker/reopen there is
    /// no live owner and Started requires verification from the durable ledger.
    pub fn active_operation_id(&self) -> Option<String> {
        self.active_operation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub fn operations(&self) -> &NativeCommandOperationStore {
        &self.operations
    }
    pub fn manager_generation(&self) -> Option<&str> {
        self.manager.as_ref().map(|m| m.generation())
    }
    pub async fn notified(&self) {
        self.notify.notified().await;
    }

    pub fn validate_lease(&self, lease: &NativeSessionLease) -> Result<(), AgentError> {
        let session = self
            .sessions
            .native_session_for_lease(lease)
            .map_err(runtime)?;
        if session.id != self.owner {
            return Err(AgentError::InvalidRequest(
                "manual command lease belongs to another session".into(),
            ));
        }
        self.operations
            .require_no_uncertain(lease)
            .map_err(|e| AgentError::EffectUncertain(e.to_string()))
    }

    /// Capture the exact current route/capability/policy for a confirmation.
    /// This method neither queues an intention nor executes a process effect.
    pub fn prepare_request(
        &self,
        command_id: &str,
        action: NativeCommandOperationAction,
    ) -> Result<NativeCommandOperationRequest, AgentError> {
        let manager = self.manager.as_ref().ok_or_else(|| {
            AgentError::InvalidRequest("processes are not restored after reopening".into())
        })?;
        let policy = (self.source)().map_err(AgentError::InvalidRequest)?;
        let request = NativeCommandOperationRequest {
            operation_id: uuid::Uuid::new_v4().to_string(),
            session_id: self.owner.clone(),
            workspace: manager.workspace().to_path_buf(),
            manager_generation: manager.generation().into(),
            command_id: command_id.into(),
            action,
            policy_digest: policy.digest_hex(),
            hooks_digest: policy.hooks.digest_hex().map_err(runtime)?,
        };
        self.validate_request(&request, &policy)?;
        Ok(request)
    }

    /// Call only for an explicitly confirmed exact request. Ask authorizes
    /// this operation once; no session grant is issued or inherited.
    pub fn enqueue_confirmed(
        &self,
        request: NativeCommandOperationRequest,
    ) -> Result<NativeCommandOperationSnapshot, AgentError> {
        if request.session_id != self.owner {
            return Err(AgentError::InvalidRequest(
                "confirmed command belongs to another session".into(),
            ));
        }
        if let Some(saved) = self
            .operations
            .snapshot(&self.owner, &request.operation_id)
            .map_err(runtime)?
        {
            if saved.request != request {
                return Err(AgentError::InvalidRequest(
                    "operation ID was reused with different arguments".into(),
                ));
            }
            if saved.state == NativeCommandOperationState::Requested {
                self.notify.notify_one();
            }
            // A known receipt is an ACK, even after EOF/revocation/reopen.
            // Returning it grants no new execution or policy approval.
            return Ok(saved);
        }
        let policy = (self.source)().map_err(AgentError::InvalidRequest)?;
        self.validate_request(&request, &policy)?;
        let snapshot = self
            .operations
            .enqueue_requested(request)
            .map_err(runtime)?;
        self.notify.notify_one();
        Ok(snapshot)
    }

    fn validate_request(
        &self,
        request: &NativeCommandOperationRequest,
        policy: &NativeManualCommandPolicy,
    ) -> Result<PersistentCommandControlInspection, AgentError> {
        let manager = self.validate_policy_request(request, policy)?;
        let control = manager.control_snapshot(&request.command_id)?;
        if control.revoked || control.stop_requested || control.exit.is_some() {
            return Err(AgentError::InvalidRequest(
                "the command is no longer available for controls".into(),
            ));
        }
        match &request.action {
            NativeCommandOperationAction::Stdin { input, close_after } => {
                if !control.stdin_open()
                    || input.len() > control.max_stdin_bytes
                    || (input.is_empty() && !close_after)
                {
                    return Err(AgentError::InvalidRequest(
                        "stdin is closed, busy, uncertain or exceeds its byte limit".into(),
                    ));
                }
            }
            NativeCommandOperationAction::Resize { columns, rows } => {
                if !control.resize_enabled
                    || !control.pty
                    || control.dimensions_uncertain
                    || !(20..=500).contains(columns)
                    || !(5..=200).contains(rows)
                {
                    return Err(AgentError::InvalidRequest(
                        "this command does not support the requested terminal resize".into(),
                    ));
                }
            }
            NativeCommandOperationAction::Terminate => {}
        }
        Ok(control)
    }

    fn validate_policy_request(
        &self,
        request: &NativeCommandOperationRequest,
        policy: &NativeManualCommandPolicy,
    ) -> Result<&Arc<PersistentCommandManager>, AgentError> {
        let manager = self.manager.as_ref().ok_or_else(|| {
            AgentError::InvalidRequest(
                "original process manager is unavailable; no process or input was restored".into(),
            )
        })?;
        if request.session_id != self.owner
            || manager.native_session_owner() != Some(self.owner.as_str())
            || request.manager_generation != manager.generation()
            || request.workspace != manager.workspace()
            || request.policy_digest != policy.digest_hex()
            || request.hooks_digest != policy.hooks.digest_hex().map_err(runtime)?
            || !policy.run_command_enabled
            || policy.isolation != manager.isolation()
            || !policy.isolation.is_available_on_host()
        {
            return Err(AgentError::InvalidRequest(
                "manual command owner, generation, policy or hooks changed".into(),
            ));
        }
        let call = operation_call(request);
        if policy.permissions.mode_for_call(&call) == PermissionMode::Deny {
            return Err(AgentError::InvalidRequest(
                "current policy denies this manual command operation".into(),
            ));
        }
        Ok(manager)
    }

    pub async fn process_pending(
        &self,
        lease: &NativeSessionLease,
        cancel: &CancellationToken,
    ) -> Result<(), AgentError> {
        // Cancellation still needs to acknowledge queued intentions before a
        // terminal event. Waiting for this mutex cannot perform an effect.
        let _actor = self.actor_lock.lock().await;
        self.validate_lease(lease)?;
        self.operations
            .recover_temporaries(lease)
            .map_err(runtime)?;
        // The lease serializes actors. A snapshot of this batch avoids starving
        // model progress with an unbounded stream of incoming human requests.
        for operation in self.operations.list_pending(&self.owner).map_err(runtime)? {
            if operation.state != NativeCommandOperationState::Requested {
                continue;
            }
            if cancel.is_cancelled() {
                if let Err(error) = self.operations.cancel_requested(
                    lease,
                    &operation.request.operation_id,
                    operation.revision,
                    "cancelled before any hook or command effect",
                ) {
                    if !self.withdrawal_won(&operation.request, &error)? {
                        return Err(runtime(error));
                    }
                }
                continue;
            }
            let policy = match (self.source)()
                .map_err(AgentError::InvalidRequest)
                .and_then(|policy| {
                    self.validate_request(&operation.request, &policy)
                        .map(|_| policy)
                }) {
                Ok(policy) => policy,
                Err(error) => {
                    if let Err(store_error) = self.operations.reject_requested(
                        lease,
                        &operation.request.operation_id,
                        operation.revision,
                        &error.to_string(),
                    ) {
                        if !self.withdrawal_won(&operation.request, &store_error)? {
                            return Err(runtime(store_error));
                        }
                    }
                    continue;
                }
            };
            self.execute(lease, operation, &policy, cancel).await?;
        }
        self.operations
            .require_no_uncertain(lease)
            .map_err(|e| AgentError::EffectUncertain(e.to_string()))
    }

    async fn execute(
        &self,
        lease: &NativeSessionLease,
        operation: NativeCommandOperationSnapshot,
        policy: &NativeManualCommandPolicy,
        cancel: &CancellationToken,
    ) -> Result<(), AgentError> {
        let _active =
            ActiveOperationGuard::new(&self.active_operation, &operation.request.operation_id);
        let call = operation_call(&operation.request);
        // Worst-case lossy UTF-8 and JSON escaping, repeated ledger fields,
        // metadata and final receipts. The core reserves capacity durably so
        // another enqueue cannot take the bytes after this preflight.
        let budget = policy
            .hooks
            .for_call(HookPhase::PreToolUse, &call.name)
            .chain(policy.hooks.for_call(HookPhase::PostToolUse, &call.name))
            .try_fold(512 * 1024usize, |sum, hook| {
                sum.checked_add(hook.max_output_bytes * 48 + 64 * 1024)
            })
            .ok_or_else(|| AgentError::Limit("manual hook receipt capacity overflow".into()))?;
        if let Err(error) =
            self.operations
                .preflight_capacity(lease, &operation.request.operation_id, budget)
        {
            if self.withdrawal_won(&operation.request, &error)? {
                return Ok(());
            }
            if let Err(rejection) = self.operations.reject_requested(
                lease,
                &operation.request.operation_id,
                operation.revision,
                "insufficient durable capacity for complete hook/effect receipts; nothing executed",
            ) {
                if self.withdrawal_won(&operation.request, &rejection)? {
                    return Ok(());
                }
                return Err(runtime(rejection));
            }
            return Err(AgentError::Limit(error.to_string()));
        }
        let start = match self.operations.begin_started(
            lease,
            &operation.request.operation_id,
            operation.revision,
        ) {
            Ok(start) => start,
            Err(error) if self.withdrawal_won(&operation.request, &error)? => return Ok(()),
            Err(error) => return Err(runtime(error)),
        };
        if !start.newly_started {
            return Err(AgentError::EffectUncertain(
                "existing Started receipt must be inspected; effect not repeated".into(),
            ));
        }
        let mut saved = start.snapshot;
        self.observe(&saved)?;
        if let Some(outcome) = self
            .run_hooks(
                lease,
                &mut saved,
                policy,
                HookPhase::PreToolUse,
                None,
                cancel,
            )
            .await?
        {
            return self.finish(lease, saved, outcome);
        }
        // Recheck fresh policy/file identities/capabilities after pre-hooks.
        // A blocking hook may have changed either while the UI was hidden.
        let fresh = (self.source)()
            .map_err(AgentError::InvalidRequest)
            .and_then(|fresh| self.validate_request(&saved.request, &fresh));
        if let Err(error) = fresh {
            return self.finish(
                lease,
                saved,
                NativeCommandOperationOutcome::Failed {
                    diagnostic: error.to_string(),
                },
            );
        }
        saved = self
            .operations
            .record_phase(
                lease,
                &saved.request.operation_id,
                saved.revision,
                NativeCommandOperationPhase::BeforeCommand,
                "current policy and process capability revalidated",
            )
            .map_err(runtime)?;
        self.observe(&saved)?;
        let manager = self.manager.as_ref().expect("validated manager");
        let effect = match &saved.request.action {
            NativeCommandOperationAction::Stdin { input, close_after } => {
                manager
                    .write_stdin(
                        &saved.request.command_id,
                        input.as_bytes(),
                        *close_after,
                        cancel,
                    )
                    .await
            }
            NativeCommandOperationAction::Resize { columns, rows } => {
                manager
                    .resize_pty(&saved.request.command_id, *columns, *rows, cancel)
                    .await
            }
            NativeCommandOperationAction::Terminate => {
                if cancel.is_cancelled() {
                    Err(AgentError::Cancelled)
                } else {
                    manager.terminate(&saved.request.command_id).await
                }
            }
        };
        let outcome = match effect {
            Ok(()) => NativeCommandOperationOutcome::Succeeded { diagnostic: "process manager confirmed the requested operation; this is not a model/tool result".into() },
            Err(AgentError::EffectUncertain(detail)) => return self.finish(lease, saved, NativeCommandOperationOutcome::EffectUncertain { diagnostic: detail }),
            Err(error) => NativeCommandOperationOutcome::Failed { diagnostic: error.to_string() },
        };
        saved = self
            .operations
            .record_effect_result(
                lease,
                &saved.request.operation_id,
                saved.revision,
                outcome.clone(),
            )
            .map_err(runtime)?;
        self.observe(&saved)?;
        let output = serde_json::to_value(&outcome).map_err(runtime)?;
        if let Some(hook_outcome) = self
            .run_hooks(
                lease,
                &mut saved,
                policy,
                HookPhase::PostToolUse,
                Some(output),
                cancel,
            )
            .await?
        {
            return self.finish(lease, saved, hook_outcome);
        }
        self.finish(lease, saved, outcome)
    }

    async fn run_hooks(
        &self,
        lease: &NativeSessionLease,
        saved: &mut NativeCommandOperationSnapshot,
        policy: &NativeManualCommandPolicy,
        phase: HookPhase,
        output: Option<Value>,
        cancel: &CancellationToken,
    ) -> Result<Option<NativeCommandOperationOutcome>, AgentError> {
        let call = operation_call(&saved.request);
        let durable_phase = if phase == HookPhase::PreToolUse {
            NativeCommandOperationPhase::BeforePreHooks
        } else {
            NativeCommandOperationPhase::BeforePostHooks
        };
        for hook in policy.hooks.for_call(phase, &call.name) {
            // Each hook has its own synced start marker and full observation.
            *saved = self
                .operations
                .record_phase(
                    lease,
                    &saved.request.operation_id,
                    saved.revision,
                    durable_phase,
                    &json!({"hook_id":hook.id,"observation":"before_hook_spawn"}).to_string(),
                )
                .map_err(runtime)?;
            self.observe(saved)?;
            let current = (self.source)()
                .map_err(AgentError::InvalidRequest)
                .and_then(|fresh| {
                    self.validate_policy_request(&saved.request, &fresh)
                        .map(|_| ())
                });
            if let Err(error) = current {
                return Ok(Some(NativeCommandOperationOutcome::Failed {
                    diagnostic: format!("current policy or hooks changed before hook spawn; remaining hooks not executed: {error}"),
                }));
            }
            let invocation = ManualCommandHookInvocation {
                version: 1,
                phase,
                session_id: self.owner.clone(),
                operation_id: saved.request.operation_id.clone(),
                command_id: saved.request.command_id.clone(),
                tool_name: call.name.clone(),
                tool_input: call.arguments.clone(),
                tool_output: output.clone(),
            };
            let result = run_manual_command_hook(hook, &invocation, cancel).await;
            let (diagnostic, outcome) = match result {
                HookRunResult::Completed { response, stdout, stderr } => {
                    let blocked = response.decision == HookDecision::Block;
                    (json!({"hook_id":hook.id,"response":response,"stdout":stdout,"stderr":stderr}).to_string(),
                        blocked.then(|| NativeCommandOperationOutcome::Failed { diagnostic: if phase == HookPhase::PreToolUse {
                            "pre-hook blocked the manual operation; command effect not executed".into()
                        } else {
                            "post-hook blocked further hooks; the previously recorded command effect remains unchanged".into()
                        } }))
                }
                HookRunResult::NotStarted(failure) => (json!({"hook_id":hook.id,"not_started":format!("{failure:?}")}).to_string(),
                    Some(NativeCommandOperationOutcome::Failed { diagnostic: format!("hook did not start: {failure:?}") })),
                HookRunResult::OutcomeUncertain(failure) => (json!({"hook_id":hook.id,"uncertain":format!("{failure:?}")}).to_string(),
                    Some(NativeCommandOperationOutcome::EffectUncertain { diagnostic: format!("manual hook effect requires verification: {failure:?}") })),
                HookRunResult::OutcomeUncertainCaptured { failure, stdout, stderr } =>
                    (json!({"hook_id":hook.id,"uncertain":format!("{failure:?}"),"stdout":stdout,"stderr":stderr}).to_string(),
                    Some(NativeCommandOperationOutcome::EffectUncertain { diagnostic: format!("manual hook effect requires verification: {failure:?}") })),
            };
            *saved = self
                .operations
                .record_phase(
                    lease,
                    &saved.request.operation_id,
                    saved.revision,
                    durable_phase,
                    &diagnostic,
                )
                .map_err(runtime)?;
            self.observe(saved)?;
            if outcome.is_some() {
                return Ok(outcome);
            }
        }
        Ok(None)
    }

    fn finish(
        &self,
        lease: &NativeSessionLease,
        saved: NativeCommandOperationSnapshot,
        outcome: NativeCommandOperationOutcome,
    ) -> Result<(), AgentError> {
        let uncertain = matches!(
            outcome,
            NativeCommandOperationOutcome::EffectUncertain { .. }
        );
        let id = saved.request.operation_id.clone();
        let finished = self
            .operations
            .finish(lease, &id, saved.revision, outcome)
            .map_err(runtime)?;
        self.observe(&finished)?;
        if uncertain {
            Err(AgentError::EffectUncertain(format!(
                "manual command operation {id} requires inspection"
            )))
        } else {
            Ok(())
        }
    }

    fn withdrawal_won(
        &self,
        request: &NativeCommandOperationRequest,
        error: &NativeCommandOperationError,
    ) -> Result<bool, AgentError> {
        if !matches!(error, NativeCommandOperationError::Conflict) {
            return Ok(false);
        }
        Ok(self
            .operations
            .snapshot(&self.owner, &request.operation_id)
            .map_err(runtime)?
            .is_some_and(|saved| {
                saved.request == *request
                    && matches!(saved.state, NativeCommandOperationState::Cancelled { .. })
            }))
    }
}

struct ActiveOperationGuard<'a>(&'a Mutex<Option<String>>);
impl<'a> ActiveOperationGuard<'a> {
    fn new(active: &'a Mutex<Option<String>>, id: &str) -> Self {
        *active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id.into());
        Self(active)
    }
}
impl Drop for ActiveOperationGuard<'_> {
    fn drop(&mut self) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

fn operation_call(request: &NativeCommandOperationRequest) -> ToolCall {
    let (name, input) = match &request.action {
        NativeCommandOperationAction::Stdin { input, close_after } => (
            "write_stdin",
            json!({"id":request.command_id,"input":input,"close_after":close_after}),
        ),
        NativeCommandOperationAction::Resize { columns, rows } => (
            "resize_command",
            json!({"id":request.command_id,"columns":columns,"rows":rows}),
        ),
        NativeCommandOperationAction::Terminate => {
            ("terminate_command", json!({"id":request.command_id}))
        }
    };
    ToolCall {
        id: request.operation_id.clone(),
        name: name.into(),
        arguments: input,
    }
}
