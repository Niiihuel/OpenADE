//! Process-local command managers retained across turns of one native session.

use openide_harness::{CommandIsolation, PersistentCommandLimits, PersistentCommandManager};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct NativeCommandSessions {
    managers: HashMap<String, Arc<PersistentCommandManager>>,
}

impl NativeCommandSessions {
    /// Observe an already attached manager without creating one or changing
    /// its lifetime. The UI never consumes command output while listing it.
    pub(crate) fn manager(&self, session_id: &str) -> Option<Arc<PersistentCommandManager>> {
        self.managers.get(session_id).cloned()
    }

    pub(crate) fn for_session(
        &mut self,
        session_id: &str,
        workspace: &Path,
        isolation: CommandIsolation,
        run_command_enabled: bool,
    ) -> Result<Option<Arc<PersistentCommandManager>>, String> {
        if !run_command_enabled
            || (isolation == CommandIsolation::WorkspaceWriteOffline && !cfg!(windows))
        {
            self.revoke_session(session_id);
            return Ok(None);
        }
        if session_id.is_empty() || session_id.len() > 128 {
            return Err("La sesión nativa no tiene un ID válido para comandos persistentes".into());
        }
        let workspace = workspace
            .canonicalize()
            .map_err(|error| format!("No se pudo resolver el workspace: {error}"))?;
        if let Some(existing) = self.managers.get(session_id) {
            if existing.workspace() != workspace.as_path() {
                return Err("La sesión de comandos pertenece a otro workspace".into());
            }
            if existing.isolation() == isolation {
                return Ok(Some(existing.clone()));
            }
            self.revoke_session(session_id);
        }
        let manager =
            PersistentCommandManager::new(workspace, isolation, PersistentCommandLimits::default())
                .map_err(|error| {
                    format!("No se pudieron preparar comandos persistentes: {error}")
                })?;
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
        if let Some(manager) = self.managers.remove(session_id) {
            manager.stop_all();
        }
    }

    /// A preference downgrade revokes even managers still held by a live turn.
    /// Switching back to host execution creates fresh managers and IDs.
    pub(crate) fn revoke_all(&mut self) {
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
        let offline = sessions
            .for_session(
                "first",
                workspace.path(),
                CommandIsolation::WorkspaceWriteOffline,
                true,
            )
            .unwrap();
        #[cfg(windows)]
        {
            let offline = offline.unwrap();
            assert_eq!(offline.isolation(), CommandIsolation::WorkspaceWriteOffline);
            assert!(!offline.pty_tools_enabled());
            assert!(!offline.pty_resize_tools_enabled());
            assert!(!Arc::ptr_eq(&first, &offline));
        }
        #[cfg(not(windows))]
        assert!(offline.is_none());
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
