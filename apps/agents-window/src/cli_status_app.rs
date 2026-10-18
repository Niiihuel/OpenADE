//! The status of tasks whose agent is a CLI in a terminal; how each CLI
//! reports it is in `cli_status`.

use super::*;
use crate::cli_status::{self, Observed, Signal};

impl AgentsWindow {
    /// Read what each task's CLI reported. A task off screen gets no
    /// repaints from its terminal, so while one works the app looks again.
    pub(super) fn poll_cli_status(&mut self, ctx: &egui::Context) -> bool {
        if self.cli_terminal_tasks.is_empty() {
            return false;
        }
        let interval = Duration::from_millis(250);
        if self.cli_status_polled.is_some_and(|at| at.elapsed() < interval) {
            if !self.cli_status.is_empty() {
                ctx.request_repaint_after(interval);
            }
            return false;
        }
        self.cli_status_polled = Some(Instant::now());
        let tasks: Vec<_> = self
            .cli_terminal_tasks
            .iter()
            .map(|(id, (session, report))| (*id, session.clone(), *report))
            .collect();
        let mut changed = false;
        for (id, session, report) in tasks {
            let Some(terminal) = self.terminals.get(&id) else {
                self.cli_terminal_tasks.remove(&id);
                changed |= self.cli_status.remove(&session).is_some();
                continue;
            };
            let mut observed = Observed {
                input: terminal.take_input(),
                signals: Vec::new(),
                quiet_for: terminal.quiet_for(),
                running: terminal.is_running(),
            };
            let notices = terminal.take_notices();
            if let Ok(word) = std::fs::read_to_string(cli_status::status_file(&self.data_root, &session)) {
                if self.cli_status_words.get(&session) != Some(&word) {
                    observed.signals.extend(cli_status::from_file(&word));
                    self.cli_status_words.insert(session.clone(), word);
                }
            }
            observed.signals.extend(notices.iter().map(|notice| cli_status::from_notice(notice)));
            let before = self.cli_status.get(&session).copied();
            let (status, notice) = cli_status::next(before, report, &observed);
            match notice {
                Some(Signal::Input) => self.notify_session(
                    crate::notifications::Event::Approval,
                    "Esperando tu respuesta",
                    &session,
                ),
                Some(Signal::Done) => {
                    self.notify_session(
                        crate::notifications::Event::TurnCompleted,
                        "Turno completado",
                        &session,
                    );
                    let watching = self.window_focused
                        && self.view.selected_session.as_deref() == Some(session.as_str());
                    if !watching {
                        self.leave_unread(&session);
                        self.refresh_sessions();
                    }
                }
                _ => {}
            }
            if before.is_none() && status == Some(false) {
                self.checkpoint_before_turn(&session, "");
            }
            if status != before {
                match status {
                    Some(asks) => self.cli_status.insert(session, asks),
                    None => self.cli_status.remove(&session),
                };
                changed = true;
            }
        }
        if changed {
            self.sync_off_screen_turns();
        }
        if !self.cli_status.is_empty() {
            ctx.request_repaint_after(interval);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> (tempfile::TempDir, AgentsWindow) {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        app.view.workspace = workspace.to_string_lossy().into_owned();
        (temp, app)
    }

    #[cfg(unix)]
    #[test]
    fn a_claude_task_off_screen_shows_working_then_its_question_then_unread() {
        let (_temp, mut app) = window();
        let ctx = egui::Context::default();
        let terminal = crate::terminal::TerminalSession::spawn_with_env(
            Path::new(&app.view.workspace),
            80,
            24,
            std::ffi::OsStr::new("sh"),
            &["-c".into(), "sleep 30".into()],
            &[],
        )
        .unwrap();
        app.terminals.insert(7, terminal);
        app.cli_terminal_tasks
            .insert(7, ("task".into(), crate::cli_status::StatusReport::ClaudeHooks));
        let file = cli_status::status_file(&app.data_root, "task");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let look = |app: &mut AgentsWindow, word: &str| {
            std::fs::write(&file, word).unwrap();
            app.cli_status_polled = None;
            app.poll_cli_status(&ctx);
            app.view.off_screen_turns.get("task").copied()
        };
        assert_eq!(look(&mut app, "working"), Some(false));
        assert_eq!(look(&mut app, "input"), Some(true));
        assert_eq!(app.view.notifications.items().len(), 1);
        assert_eq!(look(&mut app, "done"), None);
        assert_eq!(app.view.notifications.items().len(), 2);
        // Closing the terminal forgets the task.
        app.terminals.remove(&7);
        app.cli_status.insert("task".into(), false);
        assert_eq!(look(&mut app, "working"), None);
        assert!(app.cli_terminal_tasks.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_terminal_hands_over_the_notifications_its_program_sends() {
        let workspace = tempfile::tempdir().unwrap();
        let terminal = crate::terminal::TerminalSession::spawn_with_env(
            workspace.path(),
            80,
            24,
            std::ffi::OsStr::new("sh"),
            &["-c".into(), "printf '\\033]9;4;1;50\\007\\033]9;Approval requested: ls\\007'; sleep 5".into()],
            &[],
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut notices = Vec::new();
        while notices.is_empty() && Instant::now() < deadline {
            notices = terminal.take_notices();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(notices, ["Approval requested: ls"]);
        assert!(terminal.quiet_for().is_some());
        assert_eq!(terminal.take_input(), None);
    }
}
