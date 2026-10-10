//! Host-owned process registry shared by desktop and child workers.
//!
//! Profiles carry authorization epochs, not authority supplied by tool arguments.
//! Dropping or revoking the host registry contains every retained process manager.
use crate::{
    tools::persistent_command_tools, AgentError, CommandIsolation, NativeManualCommandActor,
    NativeManualCommandPolicySource, PersistentCommandLimits, PersistentCommandManager,
    ToolContext, ToolDefinition, ToolExecutor, ToolOutput,
};
use async_trait::async_trait;
use openide_core::{NativeCommandOutputStore, NativeSessionLease, SessionStore, ToolCall};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};

#[derive(Clone, Copy, Debug)]
pub struct NativeCommandProfileOptions {
    pub isolation: CommandIsolation,
    pub limits: PersistentCommandLimits,
    /// Host opt-in. Effective only for supported Windows host terminals.
    pub pty: bool,
    /// Host opt-in. Effective only when PTY is also enabled.
    pub resize: bool,
}

impl Default for NativeCommandProfileOptions {
    fn default() -> Self {
        Self {
            isolation: CommandIsolation::HostUnrestricted,
            limits: PersistentCommandLimits::default(),
            pty: false,
            resize: false,
        }
    }
}

impl NativeCommandProfileOptions {
    fn validate(self) -> Result<Self, AgentError> {
        // Shared pure validator: no manager, output registration or runtime.
        self.limits.validate()?;
        if self.resize && !self.pty {
            return Err(AgentError::InvalidRequest(
                "invalid native command profile options".into(),
            ));
        }
        if !self.isolation.is_available_on_host() {
            return Err(AgentError::InvalidRequest(
                "native command isolation is unavailable on this host".into(),
            ));
        }
        Ok(self)
    }

    fn pty_enabled(self) -> bool {
        self.pty && cfg!(windows) && self.isolation == CommandIsolation::HostUnrestricted
    }

    fn resize_enabled(self) -> bool {
        self.resize && self.pty_enabled()
    }

    fn same_as(self, other: Self) -> bool {
        self.isolation == other.isolation
            && self.pty == other.pty
            && self.resize == other.resize
            && self.limits.max_sessions == other.limits.max_sessions
            && self.limits.max_lifetime == other.limits.max_lifetime
            && self.limits.max_buffered_output_bytes == other.limits.max_buffered_output_bytes
            && self.limits.max_poll_output_bytes == other.limits.max_poll_output_bytes
            && self.limits.max_stdin_bytes == other.limits.max_stdin_bytes
    }
}

struct ManagedCommands {
    options: NativeCommandProfileOptions,
    manager: Arc<PersistentCommandManager>,
    actor: Option<Arc<NativeManualCommandActor>>,
}

struct ExpectedManager<'a> {
    epoch: u64,
    options: NativeCommandProfileOptions,
    workspace: &'a Path,
}

#[derive(Default)]
struct RegistryState {
    epoch: u64,
    minimum_epoch: u64,
    revoked_since: HashMap<String, u64>,
    managers: HashMap<String, ManagedCommands>,
}

pub struct NativeCommandRegistry {
    sessions: Arc<SessionStore>,
    output: Arc<NativeCommandOutputStore>,
    manual_policy: Option<Arc<NativeManualCommandPolicySource>>,
    state: Mutex<RegistryState>,
}

fn invalid(message: impl Into<String>) -> AgentError {
    AgentError::InvalidRequest(message.into())
}

impl NativeCommandRegistry {
    pub fn new(sessions: Arc<SessionStore>) -> Self {
        Self {
            output: Arc::new(NativeCommandOutputStore::new(sessions.clone())),
            sessions,
            manual_policy: None,
            state: Mutex::new(RegistryState::default()),
        }
    }

    /// Trusted host policy, freshly read by the shared actor at each boundary.
    pub fn with_manual_policy_source(
        mut self,
        source: Arc<NativeManualCommandPolicySource>,
    ) -> Self {
        self.manual_policy = Some(source);
        self
    }

    pub fn sessions(&self) -> &Arc<SessionStore> {
        &self.sessions
    }

    pub fn profile(
        self: &Arc<Self>,
        options: NativeCommandProfileOptions,
    ) -> Result<NativeCommandProfile, AgentError> {
        let options = options.validate()?;
        let state = self
            .state
            .lock()
            .map_err(|_| invalid("command registry lock poisoned"))?;
        Ok(NativeCommandProfile {
            registry: Arc::downgrade(self),
            epoch: state.epoch,
            options,
        })
    }

    /// Pure lookup; reopening/history reads must never recreate a process pool.
    pub fn manager(
        &self,
        owner: &str,
    ) -> Result<Option<Arc<PersistentCommandManager>>, AgentError> {
        let state = self
            .state
            .lock()
            .map_err(|_| invalid("command registry lock poisoned"))?;
        Ok(state.managers.get(owner).map(|entry| entry.manager.clone()))
    }

    pub fn managers(&self) -> Result<Vec<(String, Arc<PersistentCommandManager>)>, AgentError> {
        let state = self
            .state
            .lock()
            .map_err(|_| invalid("command registry lock poisoned"))?;
        let mut managers: Vec<_> = state
            .managers
            .iter()
            .map(|(owner, entry)| (owner.clone(), entry.manager.clone()))
            .collect();
        managers.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(managers)
    }

    /// Returns the same actor to the UI and workers for this manager generation.
    /// No manager is created; absent source/manager returns None for recovery UI.
    pub fn manual_actor(
        &self,
        owner: &str,
    ) -> Result<Option<Arc<NativeManualCommandActor>>, AgentError> {
        self.manual_actor_for(owner, None)
    }

    fn manual_actor_for(
        &self,
        owner: &str,
        expected: Option<ExpectedManager<'_>>,
    ) -> Result<Option<Arc<NativeManualCommandActor>>, AgentError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| invalid("command registry lock poisoned"))?;
        if let Some(expected) = &expected {
            Self::validate_epoch(&state, expected.epoch, owner)?;
        }
        let Some(entry) = state.managers.get_mut(owner) else {
            return Ok(None);
        };
        if let Some(expected) = expected {
            if entry.manager.workspace() != expected.workspace
                || !entry.options.same_as(expected.options)
            {
                return Err(invalid(
                    "native command actor has another workspace or profile",
                ));
            }
        }
        let Some(source) = &self.manual_policy else {
            return Ok(None);
        };
        if let Some(actor) = &entry.actor {
            return Ok(Some(actor.clone()));
        }
        let actor = Arc::new(NativeManualCommandActor::new(
            self.sessions.clone(),
            owner.into(),
            Some(entry.manager.clone()),
            source.clone(),
        )?);
        entry.actor = Some(actor.clone());
        Ok(Some(actor))
    }

    pub fn revoke_session(&self, owner: &str) -> Result<(), AgentError> {
        self.revoke(Some(owner))
    }

    pub fn revoke_all(&self) -> Result<(), AgentError> {
        self.revoke(None)
    }

    fn revoke(&self, owner: Option<&str>) -> Result<(), AgentError> {
        // Revocation must contain processes even after an earlier host panic.
        // Recover the poisoned guard, then report poison after stopping them.
        let (mut state, poisoned) = match self.state.lock() {
            Ok(state) => (state, false),
            Err(error) => (error.into_inner(), true),
        };
        let overflow = state.epoch == u64::MAX;
        state.epoch = state.epoch.saturating_add(1);
        if let Some(owner) = owner.filter(|_| !overflow) {
            let epoch = state.epoch;
            state.revoked_since.insert(owner.into(), epoch);
            if let Some(entry) = state.managers.remove(owner) {
                entry.manager.stop_all();
            }
        } else {
            state.minimum_epoch = state.epoch;
            for (_, entry) in state.managers.drain() {
                entry.manager.stop_all();
            }
            state.revoked_since.clear();
        }
        // An exhausted epoch cannot be reused by a profile at MAX. Keep this
        // registry permanently poisoned/unavailable rather than grant a retry.
        if overflow {
            drop(state);
            return Err(invalid("command registry authorization epoch exhausted"));
        }
        if poisoned {
            return Err(invalid("command registry lock poisoned; commands revoked"));
        }
        Ok(())
    }

    fn validate_epoch(state: &RegistryState, epoch: u64, owner: &str) -> Result<(), AgentError> {
        if state.epoch == u64::MAX
            || epoch < state.minimum_epoch
            || state
                .revoked_since
                .get(owner)
                .is_some_and(|minimum| epoch < *minimum)
        {
            return Err(invalid("native command profile has been revoked"));
        }
        Ok(())
    }

    fn manager_for(
        &self,
        epoch: u64,
        options: NativeCommandProfileOptions,
        owner: &str,
        workspace: &Path,
    ) -> Result<Arc<PersistentCommandManager>, AgentError> {
        let workspace = workspace
            .canonicalize()
            .map_err(|e| invalid(format!("invalid command workspace: {e}")))?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| invalid("command registry lock poisoned"))?;
        Self::validate_epoch(&state, epoch, owner)?;
        let session = self
            .sessions
            .load(owner)
            .map_err(|e| invalid(e.to_string()))?
            .ok_or_else(|| invalid("native command session does not exist"))?;
        if session.session.native_automation.is_some() {
            return Err(invalid(
                "native automation read-only profile forbids command managers",
            ));
        }
        // Fresh native metadata/deletion/workspace check, without loading or
        // repairing the model journal. register repeats it before any spawn.
        self.output
            .list(owner, &workspace)
            .map_err(|e| invalid(e.to_string()))?;
        if let Some(entry) = state.managers.get(owner) {
            if entry.manager.workspace() != workspace || !entry.options.same_as(options) {
                return Err(invalid(
                    "native command manager has another workspace or profile",
                ));
            }
            return Ok(entry.manager.clone());
        }
        options.validate()?;
        let mut manager =
            PersistentCommandManager::new(workspace, options.isolation, options.limits)?
                .with_native_session_owner(owner.to_owned())?
                .with_native_output_store(self.output.clone())?;
        if options.pty {
            manager = manager.with_pty_tools();
        }
        if options.resize {
            manager = manager.with_pty_resize_tools();
        }
        let manager = Arc::new(manager);
        state.managers.insert(
            owner.into(),
            ManagedCommands {
                options,
                manager: manager.clone(),
                actor: None,
            },
        );
        Ok(manager)
    }
}

impl Drop for NativeCommandRegistry {
    fn drop(&mut self) {
        let _ = self.revoke_all();
    }
}

/// Cloneable, process-free capability captured by a trusted host builder.
/// It cannot keep a dropped host registry alive or recreate a revoked owner.
#[derive(Clone)]
pub struct NativeCommandProfile {
    registry: Weak<NativeCommandRegistry>,
    epoch: u64,
    options: NativeCommandProfileOptions,
}

impl NativeCommandProfile {
    fn registry(&self) -> Result<Arc<NativeCommandRegistry>, AgentError> {
        self.registry
            .upgrade()
            .ok_or_else(|| invalid("native command registry is no longer available"))
    }

    fn validate_store(
        &self,
        sessions: &Arc<SessionStore>,
    ) -> Result<Arc<NativeCommandRegistry>, AgentError> {
        let registry = self.registry()?;
        if !Arc::ptr_eq(registry.sessions(), sessions) {
            return Err(invalid(
                "native command profile belongs to another session store",
            ));
        }
        Ok(registry)
    }

    pub fn ensure_live(&self, owner: &str) -> Result<(), AgentError> {
        let registry = self.registry()?;
        let state = registry
            .state
            .lock()
            .map_err(|_| invalid("command registry lock poisoned"))?;
        NativeCommandRegistry::validate_epoch(&state, self.epoch, owner)
    }

    pub fn manager(
        &self,
        owner: &str,
        workspace: &Path,
    ) -> Result<Arc<PersistentCommandManager>, AgentError> {
        self.registry()?
            .manager_for(self.epoch, self.options, owner, workspace)
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        persistent_command_tools::definitions_for_profile(
            self.options.isolation,
            self.options.pty_enabled(),
            self.options.resize_enabled(),
        )
    }

    /// Stable runtime identity. Authorization epochs/generations are purposely
    /// absent: reopening verifies configuration, it never resurrects processes.
    pub fn identity_digest(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"OpenIDE.NativeCommandProfile.v1\0");
        hash.update([match self.options.isolation {
            CommandIsolation::HostUnrestricted => 0,
            CommandIsolation::WorkspaceWriteOffline => 1,
        }]);
        hash.update([u8::from(self.options.pty), u8::from(self.options.resize)]);
        for value in [
            self.options.limits.max_sessions as u128,
            self.options.limits.max_lifetime.as_nanos(),
            self.options.limits.max_buffered_output_bytes as u128,
            self.options.limits.max_poll_output_bytes as u128,
            self.options.limits.max_stdin_bytes as u128,
        ] {
            hash.update(value.to_le_bytes());
        }
        format!("{:x}", hash.finalize())
    }

    pub fn overlay(
        &self,
        base: Arc<dyn ToolExecutor>,
        sessions: Arc<SessionStore>,
        lease: &NativeSessionLease,
    ) -> Result<PersistentCommandToolExecutor, AgentError> {
        self.validate_store(&sessions)?;
        let session = sessions
            .native_session_for_lease(lease)
            .map_err(|e| invalid(e.to_string()))?;
        PersistentCommandToolExecutor::new(
            base,
            self.clone(),
            sessions,
            session.id,
            &session.workspace,
        )
    }

    pub fn actor(
        &self,
        sessions: Arc<SessionStore>,
        lease: &NativeSessionLease,
    ) -> Result<Option<Arc<NativeManualCommandActor>>, AgentError> {
        let registry = self.validate_store(&sessions)?;
        let session = sessions
            .native_session_for_lease(lease)
            .map_err(|e| invalid(e.to_string()))?;
        self.manager(&session.id, &session.workspace)?;
        let workspace = session
            .workspace
            .canonicalize()
            .map_err(|e| invalid(format!("invalid command workspace: {e}")))?;
        registry.manual_actor_for(
            &session.id,
            Some(ExpectedManager {
                epoch: self.epoch,
                options: self.options,
                workspace: &workspace,
            }),
        )
    }

    /// Bind human controls to an already retained manager, including in Plan.
    /// This adds no model tools and never creates a manager, capture or runtime.
    /// Epoch, options and workspace are checked while obtaining the same actor
    /// cache used by the UI, so a concurrent revoke cannot return a replacement.
    pub fn actor_existing(
        &self,
        sessions: Arc<SessionStore>,
        lease: &NativeSessionLease,
    ) -> Result<Option<Arc<NativeManualCommandActor>>, AgentError> {
        let registry = self.validate_store(&sessions)?;
        let session = sessions
            .native_session_for_lease(lease)
            .map_err(|e| invalid(e.to_string()))?;
        let workspace = session
            .workspace
            .canonicalize()
            .map_err(|e| invalid(format!("invalid command workspace: {e}")))?;
        registry.manual_actor_for(
            &session.id,
            Some(ExpectedManager {
                epoch: self.epoch,
                options: self.options,
                workspace: &workspace,
            }),
        )
    }

    /// Trusted cleanup after the host verifies a current lease or a committed
    /// subtree cancellation receipt for this owner. Never use model arguments
    /// as that authority. Cleanup also contains managers from a newer profile.
    pub fn revoke_owner(&self, owner: &str) -> Result<(), AgentError> {
        // If the host pool is gone its Drop already contained all managers.
        let Some(registry) = self.registry.upgrade() else {
            return Ok(());
        };
        registry.revoke_session(owner)
    }
}

/// Adds only the host's frozen command profile to an executor that already
/// exposes run_command. Every persistent ID is resolved in this exact owner's
/// manager; owner/workspace/profile cannot be supplied in model arguments.
pub struct PersistentCommandToolExecutor {
    base: Arc<dyn ToolExecutor>,
    profile: NativeCommandProfile,
    owner: String,
    workspace: PathBuf,
    definitions: Vec<ToolDefinition>,
    persistent_names: HashSet<String>,
}

impl PersistentCommandToolExecutor {
    pub fn new(
        base: Arc<dyn ToolExecutor>,
        profile: NativeCommandProfile,
        sessions: Arc<SessionStore>,
        owner: String,
        workspace: &Path,
    ) -> Result<Self, AgentError> {
        let registry = profile.validate_store(&sessions)?;
        profile.ensure_live(&owner)?;
        let session = sessions
            .load(&owner)
            .map_err(|e| invalid(e.to_string()))?
            .ok_or_else(|| invalid("native command session does not exist"))?;
        if session.session.native_automation.is_some() {
            return Err(invalid(
                "native automation read-only profile forbids command overlays",
            ));
        }
        let workspace = workspace
            .canonicalize()
            .map_err(|e| invalid(format!("invalid command workspace: {e}")))?;
        registry
            .output
            .list(&owner, &workspace)
            .map_err(|e| invalid(e.to_string()))?;
        let mut definitions = base.definitions();
        if !definitions
            .iter()
            .any(|definition| definition.name == "run_command")
        {
            return Err(invalid(
                "persistent commands require run_command in the base executor",
            ));
        }
        if definitions
            .iter()
            .any(|definition| persistent_command_tools::is_persistent_tool(&definition.name))
        {
            return Err(invalid(
                "base executor conflicts with persistent command tool names",
            ));
        }
        let command_definitions = profile.definitions();
        let persistent_names = command_definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect();
        definitions.extend(command_definitions);
        Ok(Self {
            base,
            profile,
            owner,
            workspace,
            definitions,
            persistent_names,
        })
    }
}

#[async_trait]
impl ToolExecutor for PersistentCommandToolExecutor {
    async fn execute_streaming(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        output: crate::ToolOutputSender,
    ) -> Result<ToolOutput, AgentError> {
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if persistent_command_tools::is_persistent_tool(&call.name) {
            self.execute(call, context).await
        } else {
            self.base.execute_streaming(call, context, output).await
        }
    }

    fn definitions(&self) -> Vec<ToolDefinition> {
        self.definitions.clone()
    }
    fn certifies_parallel_read(&self, name: &str) -> bool {
        !persistent_command_tools::is_persistent_tool(name)
            && self.base.certifies_parallel_read(name)
    }
    fn certifies_plan_read(&self, name: &str) -> bool {
        !persistent_command_tools::is_persistent_tool(name) && self.base.certifies_plan_read(name)
    }
    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        if !persistent_command_tools::is_persistent_tool(&call.name) {
            return self.base.execute(call, context).await;
        }
        if !self.persistent_names.contains(&call.name) {
            return Err(invalid(
                "persistent tool is not enabled by this host profile",
            ));
        }
        let workspace = context
            .workspace
            .canonicalize()
            .map_err(|e| invalid(format!("invalid command workspace: {e}")))?;
        if workspace != self.workspace {
            return Err(invalid(
                "persistent tool context belongs to another workspace",
            ));
        }
        let manager = self.profile.manager(&self.owner, &self.workspace)?;
        persistent_command_tools::execute(&manager, call, context).await
    }
}

#[cfg(test)]
#[path = "native_command_registry_tests.rs"]
mod tests;
