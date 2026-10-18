//! "Nueva tarea en un worktree": the worktree is made by the environment's
//! Git worker, the window moves to it as a new draft, and the repository's
//! `.openide/setup` runs in a terminal that goes with the task.

use super::*;

impl AgentsWindow {
    pub(super) fn start_task_worktree(&mut self, ctx: &egui::Context) {
        let Some(snapshot) = self.view.environment.workflow.snapshot.clone() else {
            self.view.status = "El proyecto no es un repositorio Git".into();
            return;
        };
        if let Some(reason) = self.branch_change_blocked_reason() {
            self.view.environment.workflow.error = Some(reason.into());
            return;
        }
        // The main worktree is listed first, whichever one is open.
        let root = snapshot
            .worktrees
            .first()
            .map(|worktree| worktree.path.clone())
            .unwrap_or_else(|| PathBuf::from(&self.view.workspace));
        let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
        let Some((path, name)) = crate::worktree_tasks::task_worktree(&root, &id) else {
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                self.view.environment.workflow.error = Some(format!("No se pudo crear {}: {error}", parent.display()));
                return;
            }
        }
        self.task_worktree = Some(path.clone());
        self.close_environment();
        self.request_environment_git(crate::environment_git::Operation::CreateWorktree { snapshot, name, path }, ctx);
    }

    /// After the worker answered: on success, the new task's draft opens in
    /// the worktree and its setup starts.
    pub(super) fn finish_task_worktree(&mut self, succeeded: bool) {
        let Some(path) = self.task_worktree.take().filter(|path| succeeded && path.is_dir()) else {
            return;
        };
        let workspace = path.to_string_lossy().into_owned();
        self.change_workspace(workspace.clone());
        if self.view.workspace != workspace {
            return;
        }
        self.view.status = format!("Nueva tarea en el worktree {}", path.display());
        let Some(command) = crate::worktree_tasks::setup_command(&path) else {
            return;
        };
        let scope = self.dock_scope();
        let tab = self.dock.new_terminal(&scope);
        self.dock.activate(&scope, tab);
        self.sync_dock_view();
        let sent = self
            .terminals
            .get_mut(&tab.get())
            .ok_or("No se pudo abrir la terminal".to_owned())
            .and_then(|terminal| terminal.send_command(&command).map_err(|error| error.to_string()));
        self.view.status = match sent {
            Ok(()) => "Preparando el worktree con .openide/setup".into(),
            Err(error) => format!("No se pudo correr .openide/setup: {error}"),
        };
    }

    /// The last task of a task worktree takes the worktree with it when
    /// nothing in it is uncommitted; its branch stays.
    pub(super) fn forget_task_worktree(&self, session: &str, workspace: &Path) {
        let shared = self
            .view
            .sessions
            .iter()
            .any(|other| other.id != session && Path::new(&other.project) == workspace);
        if shared || !crate::worktree_tasks::is_task_worktree(workspace) {
            return;
        }
        let workspace = workspace.to_path_buf();
        let _ = std::thread::Builder::new()
            .name("worktree-remove".into())
            .spawn(move || crate::worktree_tasks::remove_if_clean(&workspace));
    }
}
