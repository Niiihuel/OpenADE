//! Process-local command managers retained across turns of one native session.

use openide_core::SessionStore;
use openide_harness::{
    CommandIsolation, NativeCommandProfile, NativeCommandProfileOptions, NativeCommandRegistry,
    NativeManualCommandPolicySource, PersistentCommandLaunch, PersistentCommandLimits,
    PersistentCommandManager,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{mpsc, Arc};

/// A command an agent just started, with the screen that already follows it.
pub(crate) struct StartedCommand {
    /// The native session whose agent, parent or child worker, started it.
    pub owner: String,
    /// The manager that runs it; another one later means it was revoked.
    pub generation: String,
    pub id: String,
    pub argv: Vec<String>,
    /// A pipe-backed command (the contained profile): a mirror without keys.
    pub read_only: bool,
    pub session: std::io::Result<crate::terminal::TerminalSession>,
}

impl StartedCommand {
    /// Attached at once, on the thread that started the command: the screen
    /// drains the output copy from its first byte into a bounded grid, even
    /// while the window is hidden and paints no frame.
    pub(crate) fn attach(launch: PersistentCommandLaunch) -> Option<Self> {
        let label = launch
            .argv
            .first()
            .and_then(|program| Path::new(program).file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Some(Self {
            owner: launch.owner_session_id?,
            generation: launch.generation,
            id: launch.id,
            argv: launch.argv,
            read_only: !launch.terminal.interactive(),
            // A contained command cannot serve the host: whatever answers at
            // an address it prints is another program's, and is not opened.
            session: crate::terminal::TerminalSession::attach(
                launch.terminal,
                &label,
                launch.isolation == CommandIsolation::HostUnrestricted,
            ),
        })
    }
}

#[derive(Default)]
pub(crate) struct NativeCommandSessions {
    managers: HashMap<String, Arc<PersistentCommandManager>>,
    registry: Option<Arc<NativeCommandRegistry>>,
    durable_required: bool,
    /// Each command a manager of the registry starts, for its terminal tab.
    started: Option<mpsc::Receiver<StartedCommand>>,
    /// Only tests set it; see `keep_pipes`.
    pipes_only: bool,
}

impl NativeCommandSessions {
    pub(crate) fn with_session_store(
        sessions: Option<Arc<SessionStore>>,
        manual_policy: Arc<NativeManualCommandPolicySource>,
    ) -> Self {
        let (sender, started) = mpsc::channel();
        Self {
            managers: HashMap::new(),
            registry: sessions.map(|sessions| {
                Arc::new(
                    NativeCommandRegistry::new(sessions)
                        .with_manual_policy_source(manual_policy)
                        .with_start_observer(Arc::new(move |launch| {
                            if let Some(started) = StartedCommand::attach(launch) {
                                let _ = sender.send(started);
                            }
                        })),
                )
            }),
            durable_required: true,
            started: Some(started),
            pipes_only: false,
        }
    }

    /// For tests of the pipe contract (separate stdout and stderr, exact
    /// bytes both ways), which the contained profile and Windows still have:
    /// host commands of this app stay on pipes instead of a terminal.
    #[cfg(test)]
    pub(crate) fn keep_pipes(&mut self) {
        self.pipes_only = true;
    }

    /// Commands started since the last call, by a parent or a child worker.
    pub(crate) fn take_started(&self) -> Vec<StartedCommand> {
        self.started
            .as_ref()
            .map(|started| started.try_iter().collect())
            .unwrap_or_default()
    }

    pub(crate) fn shared_registry(&self) -> Option<Arc<NativeCommandRegistry>> {
        self.registry.clone()
    }

    pub(crate) fn command_profile(
        &self,
        isolation: CommandIsolation,
        enabled: bool,
    ) -> Result<Option<NativeCommandProfile>, String> {
        if !enabled || !isolation.is_available_on_host() {
            return Ok(None);
        }
        let registry = self
            .registry
            .as_ref()
            .ok_or("El store nativo no está disponible para los comandos del worker")?;
        registry
            .profile(NativeCommandProfileOptions {
                isolation,
                limits: PersistentCommandLimits::default(),
                pty: cfg!(windows) && isolation == CommandIsolation::HostUnrestricted,
                resize: cfg!(windows) && isolation == CommandIsolation::HostUnrestricted,
                // On Linux and macOS a host command runs on a terminal the
                // user sees and types into; the contained profile keeps pipes.
                host_terminal: cfg!(unix) && !self.pipes_only,
                ..NativeCommandProfileOptions::default()
            })
            .map(Some)
            .map_err(|error| error.to_string())
    }
    /// Observe an already attached manager without creating one or changing
    /// its lifetime. The UI never consumes command output while listing it.
    pub(crate) fn manager(&self, session_id: &str) -> Option<Arc<PersistentCommandManager>> {
        match &self.registry {
            Some(registry) => registry.manager(session_id).ok().flatten(),
            None => self.managers.get(session_id).cloned(),
        }
    }

    /// A checkout must not replace files used by a retained command, including
    /// a command from another conversation in the same worktree.
    pub(crate) fn has_running_in_worktree(&self, root: &Path) -> Result<bool, String> {
        let managers = match &self.registry {
            Some(registry) => registry
                .managers()
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|(_, manager)| manager)
                .collect(),
            None => self.managers.values().cloned().collect::<Vec<_>>(),
        };
        for manager in managers
            .iter()
            .filter(|manager| manager.workspace().starts_with(root))
        {
            let sessions = manager.list_sessions().map_err(|error| error.to_string())?;
            if sessions.iter().any(|session| session.exit.is_none()) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn for_session(
        &mut self,
        session_id: &str,
        workspace: &Path,
        isolation: CommandIsolation,
        run_command_enabled: bool,
    ) -> Result<Option<Arc<PersistentCommandManager>>, String> {
        if !run_command_enabled || !isolation.is_available_on_host() {
            self.revoke_session(session_id);
            return Ok(None);
        }
        if session_id.is_empty() || session_id.len() > 128 {
            return Err("La sesión nativa no tiene un ID válido para comandos persistentes".into());
        }
        if self.durable_required && self.registry.is_none() {
            return Err(
                "El store nativo no está disponible para guardar la salida de comandos".into(),
            );
        }
        let workspace = workspace
            .canonicalize()
            .map_err(|error| format!("No se pudo resolver el workspace: {error}"))?;
        if let Some(existing) = self.manager(session_id) {
            if existing.workspace() != workspace.as_path() {
                return Err("La sesión de comandos pertenece a otro workspace".into());
            }
            if existing.isolation() == isolation {
                return Ok(Some(existing.clone()));
            }
            self.revoke_session(session_id);
        }
        if self.registry.is_some() {
            return self
                .command_profile(isolation, true)?
                .expect("available enabled profile")
                .manager(session_id, &workspace)
                .map(Some)
                .map_err(|error| error.to_string());
        }
        // The default facade is retained only for tests of legacy in-memory
        // managers. Production always owns the shared durable registry.
        let manager =
            PersistentCommandManager::new(workspace, isolation, PersistentCommandLimits::default())
                .map_err(|error| format!("No se pudieron preparar comandos persistentes: {error}"))?
                .with_native_session_owner(session_id)
                .map_err(|error| format!("No se pudo vincular el gestor a la sesión: {error}"))?;
        #[cfg(windows)]
        let manager = if isolation == CommandIsolation::HostUnrestricted {
            manager.with_pty_tools().with_pty_resize_tools()
        } else {
            manager
        };
        let manager = Arc::new(manager);
        self.managers.insert(session_id.to_owned(), manager.clone());
        Ok(Some(manager))
    }

    pub(crate) fn revoke_session(&mut self, session_id: &str) {
        if let Some(registry) = &self.registry {
            // The registry fences profiles and stops retained trees before
            // returning a diagnostic, including poisoned-lock recovery.
            if let Err(error) = registry.revoke_session(session_id) {
                eprintln!("No se pudo completar la revocación de comandos: {error}");
            }
        }
        if let Some(manager) = self.managers.remove(session_id) {
            manager.stop_all();
        }
    }

    /// A preference downgrade revokes even managers still held by a live turn.
    /// Switching back to host execution creates fresh managers and IDs.
    pub(crate) fn revoke_all(&mut self) {
        if let Some(registry) = &self.registry {
            if let Err(error) = registry.revoke_all() {
                eprintln!("No se pudo completar la revocación de comandos: {error}");
            }
        }
        for manager in self.managers.values() {
            manager.stop_all();
        }
        self.managers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_identity_is_scoped_to_session_and_revoked_on_downgrade() {
        let workspace = tempfile::tempdir().unwrap();
        let mut sessions = NativeCommandSessions::default();
        let first = sessions
            .for_session(
                "first",
                workspace.path(),
                CommandIsolation::HostUnrestricted,
                true,
            )
            .unwrap()
            .unwrap();
        let repeated = sessions
            .for_session(
                "first",
                workspace.path(),
                CommandIsolation::HostUnrestricted,
                true,
            )
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &repeated));
        assert_eq!(first.native_session_owner(), Some("first"));
        assert!(Arc::ptr_eq(&first, &sessions.manager("first").unwrap()));
        assert_eq!(first.pty_tools_enabled(), cfg!(windows));
        assert_eq!(first.pty_resize_tools_enabled(), cfg!(windows));
        let second = sessions
            .for_session(
                "second",
                workspace.path(),
                CommandIsolation::HostUnrestricted,
                true,
            )
            .unwrap()
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(second.native_session_owner(), Some("second"));
        let offline = sessions
            .for_session(
                "first",
                workspace.path(),
                CommandIsolation::WorkspaceWriteOffline,
                true,
            )
            .unwrap();
        if CommandIsolation::WorkspaceWriteOffline.is_available_on_host() {
            let offline = offline.unwrap();
            assert_eq!(offline.isolation(), CommandIsolation::WorkspaceWriteOffline);
            assert!(!offline.pty_tools_enabled());
            assert!(!offline.pty_resize_tools_enabled());
            assert!(!Arc::ptr_eq(&first, &offline));
        } else {
            assert!(offline.is_none());
        }
        let fresh = sessions
            .for_session(
                "first",
                workspace.path(),
                CommandIsolation::HostUnrestricted,
                true,
            )
            .unwrap()
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &fresh));
        sessions.revoke_all();
        assert!(sessions.manager("first").is_none());
    }
}
