//! Restore points before each turn (see `checkpoints`), taken off the
//! interface thread, and the task header's menu that brings one back.

use super::*;

pub(super) enum CheckpointDone {
    Restored { undo: bool },
    Failed(String),
}

impl AgentsWindow {
    fn session_workspace(&self, session: &str) -> String {
        self.view
            .sessions
            .iter()
            .find(|candidate| candidate.id == session)
            .map(|candidate| candidate.project.clone())
            .filter(|project| !project.is_empty())
            .unwrap_or_else(|| self.view.workspace.clone())
    }

    /// Keep the project as it is before a turn of `session` begins.
    pub(super) fn checkpoint_before_turn(&self, session: &str, prompt: &str) {
        let data_root = self.data_root.clone();
        let workspace = PathBuf::from(self.session_workspace(session));
        let (session, label) = (session.to_owned(), prompt.to_owned());
        let done = self.checkpoint_channel.0.clone();
        let _ = std::thread::Builder::new()
            .name("checkpoint".into())
            .spawn(move || {
                let at = crate::cli_tasks::now_ms();
                if let Err(error) = crate::checkpoints::capture(&data_root, &session, &workspace, &label, at) {
                    let _ = done.send(CheckpointDone::Failed(format!(
                        "No se pudo guardar el punto de restauración: {error}"
                    )));
                }
            });
    }

    /// A deleted task's points go with it.
    pub(super) fn forget_checkpoints(&self, session: &str) {
        let (data_root, session) = (self.data_root.clone(), session.to_owned());
        let _ = std::thread::Builder::new()
            .name("checkpoint-remove".into())
            .spawn(move || crate::checkpoints::remove(&data_root, &session));
    }

    pub(super) fn poll_checkpoints(&mut self) -> bool {
        let mut changed = false;
        while let Ok(done) = self.checkpoint_channel.1.try_recv() {
            changed = true;
            self.view.checkpoint_restoring = false;
            match done {
                CheckpointDone::Restored { undo } => {
                    self.view.status = if undo {
                        "Restauración deshecha".into()
                    } else {
                        "Archivos restaurados · el menú de puntos puede deshacerlo".into()
                    };
                    self.environment_dirty = true;
                    self.refresh_clean_code_editor();
                }
                CheckpointDone::Failed(error) => self.view.status = error,
            }
        }
        changed
    }

    pub(super) fn apply_checkpoint_action(&mut self, action: &UiAction) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        match id.as_str() {
            "checkpoints_toggle" => {
                self.view.checkpoint_menu = match (&self.view.checkpoint_menu, &self.view.selected_session) {
                    (None, Some(session)) => Some(crate::checkpoints::load(&self.data_root, session)),
                    _ => None,
                };
            }
            "checkpoints_dismiss" => self.view.checkpoint_menu = None,
            "checkpoint_undo" => self.restore_checkpoint(None),
            _ => {
                let Some(ordinal) = id.strip_prefix("checkpoint_restore:").and_then(|n| n.parse().ok()) else {
                    return false;
                };
                self.restore_checkpoint(Some(ordinal));
            }
        }
        true
    }

    /// `None` undoes the last restore.
    fn restore_checkpoint(&mut self, ordinal: Option<u32>) {
        self.view.checkpoint_menu = None;
        let Some(session) = self.view.selected_session.clone() else {
            return;
        };
        let workspace = self.session_workspace(&session);
        // A restore under a working agent would race its writes; one over
        // unsaved editor text would leave the editor and the disk apart.
        if self.view.running || !self.parked_runs.is_empty() || !self.cli_status.is_empty() {
            self.view.status = "Espera a que los agentes terminen antes de restaurar archivos".into();
            return;
        }
        if !workspace.is_empty() && !self.code_editor.unsaved_paths(Path::new(&workspace)).is_empty() {
            self.view.status = "Guarda o descarta los cambios del editor antes de restaurar".into();
            return;
        }
        if self.view.checkpoint_restoring {
            return;
        }
        self.view.checkpoint_restoring = true;
        self.view.status = "Restaurando archivos…".into();
        let data_root = self.data_root.clone();
        let done = self.checkpoint_channel.0.clone();
        let _ = std::thread::Builder::new()
            .name("checkpoint-restore".into())
            .spawn(move || {
                let result = crate::checkpoints::restore(&data_root, &session, ordinal);
                let _ = done.send(match result {
                    Ok(()) => CheckpointDone::Restored { undo: ordinal.is_none() },
                    Err(error) => CheckpointDone::Failed(format!("No se pudo restaurar: {error}")),
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn the_header_menu_restores_a_point_and_undoes_it() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let git = |args: &[&str]| {
            let env = [
                ("GIT_AUTHOR_NAME", OsStr::new("t")),
                ("GIT_AUTHOR_EMAIL", OsStr::new("t@t")),
                ("GIT_COMMITTER_NAME", OsStr::new("t")),
                ("GIT_COMMITTER_EMAIL", OsStr::new("t@t")),
            ];
            assert!(crate::git_branches::run_git_env(&workspace, args, &env).unwrap().status.success());
        };
        git(&["init", "-q"]);
        std::fs::write(workspace.join("a.txt"), "before\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "one"]);
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.view.selected_session = Some("task".into());
        let ctx = egui::Context::default();
        let wait = |app: &mut AgentsWindow, what: &dyn Fn(&AgentsWindow) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !what(app) && Instant::now() < deadline {
                app.poll_checkpoints();
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(what(app));
        };
        app.checkpoint_before_turn("task", "Cambiá a.txt");
        let data_root = app.data_root.clone();
        wait(&mut app, &|_| !crate::checkpoints::load(&data_root, "task").checkpoints.is_empty());
        std::fs::write(workspace.join("a.txt"), "after\n").unwrap();

        app.apply_action(UiAction::Clicked { id: "checkpoints_toggle".into() }, &ctx);
        let menu = app.view.checkpoint_menu.clone().unwrap();
        assert_eq!(menu.checkpoints[0].label, "Cambiá a.txt");
        // Not while an agent works.
        app.cli_status.insert("other".into(), false);
        app.apply_action(UiAction::Clicked { id: "checkpoint_restore:1".into() }, &ctx);
        assert!(!app.view.checkpoint_restoring);
        app.cli_status.clear();
        app.view.checkpoint_menu = Some(menu);
        app.apply_action(UiAction::Clicked { id: "checkpoint_restore:1".into() }, &ctx);
        wait(&mut app, &|app| !app.view.checkpoint_restoring);
        assert_eq!(std::fs::read_to_string(workspace.join("a.txt")).unwrap(), "before\n");
        app.view.checkpoint_menu = Some(crate::checkpoints::load(&data_root, "task"));
        app.apply_action(UiAction::Clicked { id: "checkpoint_undo".into() }, &ctx);
        wait(&mut app, &|app| !app.view.checkpoint_restoring);
        assert_eq!(std::fs::read_to_string(workspace.join("a.txt")).unwrap(), "after\n");
    }
}
