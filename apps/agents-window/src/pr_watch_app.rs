//! The review panel's actions on a pull request (merge, approve, comment)
//! and the pull requests tasks watch: a look every `pr_watch::INTERVAL` off
//! the interface thread, a notice for what changed and, when the person asked
//! for it, the news handed to the task's agent.

use super::*;
use crate::github::PullAction;
use crate::pr_watch::{self, Change, Watch};

pub(super) type WatchResult = (String, u64, Result<(Watch, Vec<Change>), String>);

impl AgentsWindow {
    fn shown_pull(&self) -> Option<u64> {
        match &self.github_status {
            GithubStatus::Ready(github) => github.pulls.first().map(|pull| pull.number),
            _ => None,
        }
    }

    fn submit_pull_action(&mut self, action: PullAction) {
        let (Some(worker), Some(workspace)) = (&self.github_worker, self.review_active_workspace.clone()) else {
            return;
        };
        let request = GithubRequest {
            workspace,
            generation: self.review_generation,
            account: self.github_query_account().map(str::to_owned),
            create_pull: None,
            action: Some(action),
        };
        if !self.github_in_flight && worker.submit(request).is_ok() {
            self.github_in_flight = true;
            self.github_status = GithubStatus::Loading;
            if let Some(Ok(review)) = &mut self.view.git_review {
                Arc::make_mut(review).github = GithubStatus::Loading;
            }
        }
    }

    pub(super) fn apply_pull_action(&mut self, action: &UiAction) -> bool {
        let controls = &mut self.view.git_panel.controls;
        match action {
            UiAction::InputChanged { id, value } if id == "github_comment_body" => {
                controls.comment = Some(value.clone());
                return true;
            }
            UiAction::Clicked { .. } => {}
            _ => return false,
        }
        let UiAction::Clicked { id } = action else {
            return false;
        };
        let number = self.shown_pull();
        match (id.as_str(), number) {
            ("github_pr_merge", _) => self.view.git_panel.controls.merge_confirm = true,
            ("github_pr_merge_cancel", _) => self.view.git_panel.controls.merge_confirm = false,
            ("github_pr_merge_squash" | "github_pr_merge_commit", Some(number)) => {
                self.view.git_panel.controls.merge_confirm = false;
                self.submit_pull_action(PullAction::Merge { number, squash: id == "github_pr_merge_squash" });
            }
            ("github_pr_approve", Some(number)) => self.submit_pull_action(PullAction::Approve { number }),
            ("github_pr_comment", _) => self.view.git_panel.controls.comment = Some(String::new()),
            ("github_comment_cancel", _) => {
                self.view.git_panel.controls.comment = None;
                self.tree.set_input_value("github_comment_body", "");
            }
            ("github_comment_submit", Some(number)) => {
                let body = self.view.git_panel.controls.comment.clone().unwrap_or_default();
                if !body.trim().is_empty() {
                    self.view.git_panel.controls.comment = None;
                    self.tree.set_input_value("github_comment_body", "");
                    self.submit_pull_action(PullAction::Comment { number, body: body.trim().to_owned() });
                }
            }
            ("github_pr_watch", Some(number)) => self.toggle_pr_watch(number),
            ("github_pr_watch_wake", Some(number)) => {
                let session = self.view.selected_session.clone();
                if let Some(watch) = self
                    .pr_watches
                    .iter_mut()
                    .find(|watch| Some(&watch.session) == session.as_ref() && watch.number == number)
                {
                    watch.wake = !watch.wake;
                }
                self.save_pr_watches();
            }
            _ => return false,
        }
        true
    }

    fn toggle_pr_watch(&mut self, number: u64) {
        let (Some(session), Some(workspace)) = (self.view.selected_session.clone(), self.review_active_workspace.clone())
        else {
            return;
        };
        if self.github_query_account().is_none() {
            self.view.status = "Conectá una cuenta de GitHub para vigilar el pull request".into();
            return;
        }
        let before = self.pr_watches.len();
        self.pr_watches.retain(|watch| !(watch.session == session && watch.number == number));
        if self.pr_watches.len() == before {
            self.pr_watches.push(Watch { session, workspace, number, ..Watch::default() });
            // The first look soon, to learn where things stand.
            self.pr_watch_due = Instant::now();
        }
        self.save_pr_watches();
    }

    fn save_pr_watches(&mut self) {
        if let Err(error) = pr_watch::save(&self.data_root, &self.pr_watches) {
            self.view.status = error;
        }
    }

    pub(super) fn poll_pr_watch(&mut self) -> bool {
        let mut changed = self.sync_pull_controls();
        if let Some(results) = self.pr_watch_poll.as_ref().and_then(|poll| poll.try_recv().ok()) {
            self.pr_watch_poll = None;
            self.apply_pr_watch(results);
            changed = true;
        }
        changed |= self.deliver_pending_wakes();
        let Some(account) = self.github_query_account().map(str::to_owned) else {
            return changed;
        };
        if self.pr_watches.is_empty() || self.pr_watch_poll.is_some() || Instant::now() < self.pr_watch_due {
            return changed;
        }
        self.pr_watch_due = Instant::now() + pr_watch::INTERVAL;
        let watches = self.pr_watches.clone();
        let (done, poll) = std::sync::mpsc::channel();
        self.pr_watch_poll = Some(poll);
        let _ = std::thread::Builder::new()
            .name("pr-watch".into())
            .spawn(move || {
                // One credential lookup for the whole round: each costs a request.
                let token = crate::github_account::auth_token_for_account(&account);
                let results: Vec<WatchResult> = watches
                    .into_iter()
                    .map(|watch| {
                        let result = match &token {
                            Some(token) => pr_watch::snapshot(&watch, token).map(|snapshot| pr_watch::diff(&watch, &snapshot, &account)),
                            None => Err(format!("Reconecta la cuenta {account} para vigilar pull requests")),
                        };
                        (watch.session.clone(), watch.number, result)
                    })
                    .collect();
                let _ = done.send(results);
            });
        changed
    }

    fn sync_pull_controls(&mut self) -> bool {
        let number = self.shown_pull();
        let session = self.view.selected_session.as_ref();
        let watch = number.and_then(|number| {
            self.pr_watches
                .iter()
                .find(|watch| Some(&watch.session) == session && watch.number == number)
                .map(|watch| watch.wake)
        });
        let watchable = number.is_some() && session.is_some();
        let controls = &mut self.view.git_panel.controls;
        let changed = controls.watch != watch || controls.watchable != watchable;
        controls.watch = watch;
        controls.watchable = watchable;
        changed
    }

    fn apply_pr_watch(&mut self, results: Vec<WatchResult>) {
        for (session, number, result) in results {
            let Some(index) = self.pr_watches.iter().position(|watch| watch.session == session && watch.number == number)
            else {
                continue;
            };
            let (seen, changes) = match result {
                Ok(seen) => seen,
                Err(error) => {
                    self.view.status = error;
                    continue;
                }
            };
            // What the person set meanwhile stays theirs.
            let current = &self.pr_watches[index];
            let watch = Watch { wake: current.wake, pending_wake: current.pending_wake.clone(), ..seen };
            self.pr_watches[index] = watch;
            if changes.is_empty() {
                continue;
            }
            let closed = changes.iter().any(|change| matches!(change, Change::Closed { .. }));
            self.notify_session(crate::notifications::Event::PullRequest, &notice_title(number, &changes), &session);
            let work: Vec<_> = changes.into_iter().filter(|change| !matches!(change, Change::Closed { .. })).collect();
            if self.pr_watches[index].wake && !work.is_empty() {
                let prompt = pr_watch::wake_prompt(&pr_watch::describe(number, &work));
                let pending = &mut self.pr_watches[index].pending_wake;
                *pending = Some(match pending.take() {
                    Some(earlier) => format!("{earlier}\n\n{prompt}"),
                    None => prompt,
                });
            }
            if closed {
                self.pr_watches.remove(index);
            }
        }
        self.save_pr_watches();
    }

    /// Hand each waiting message to its task's agent once it can take one.
    fn deliver_pending_wakes(&mut self) -> bool {
        let waiting: Vec<_> = self
            .pr_watches
            .iter()
            .filter_map(|watch| Some((watch.session.clone(), watch.number, watch.pending_wake.clone()?)))
            .collect();
        let mut delivered = false;
        for (session, number, prompt) in waiting {
            if self.wake_task(&session, &prompt) {
                if let Some(watch) = self.pr_watches.iter_mut().find(|watch| watch.session == session && watch.number == number) {
                    watch.pending_wake = None;
                }
                delivered = true;
            }
        }
        if delivered {
            self.save_pr_watches();
        }
        delivered
    }

    /// A CLI's terminal takes the message when the CLI is idle; another
    /// agent gets it as a turn, queued behind a running one.
    fn wake_task(&mut self, session: &str, prompt: &str) -> bool {
        if let Some(id) = self
            .cli_terminal_tasks
            .iter()
            .find(|(_, (task, _))| task == session)
            .map(|(id, _)| *id)
        {
            if self.cli_status.contains_key(session) {
                return false;
            }
            return self
                .terminals
                .get_mut(&id)
                .is_some_and(|terminal| terminal.paste(prompt).and_then(|()| terminal.write(b"\r")).is_ok());
        }
        let backend = self.view.sessions.iter().find(|candidate| candidate.id == session).map(|candidate| candidate.backend.clone());
        match backend {
            // A CLI whose terminal is not open yet: when it is.
            Some(backend) if backend.starts_with(crate::view::CLI_BACKEND) => false,
            None => false,
            Some(_) => {
                let running = self.parked_runs.iter().any(|parked| parked.run.session_id() == session)
                    || (self.view.selected_session.as_deref() == Some(session) && self.view.running);
                if running {
                    let queue = self.turn_queue.entry(session.to_owned()).or_default();
                    if queue.items.len() >= MAX_QUEUED_TURNS {
                        return false;
                    }
                    queue.items.push(QueuedTurn { id: self.turn_queue_next, text: prompt.to_owned(), attachments: Vec::new() });
                    self.turn_queue_next += 1;
                    self.save_turn_queues();
                    true
                } else {
                    self.start_turn_off_screen(session, prompt).is_ok()
                }
            }
        }
    }
}

fn notice_title(number: u64, changes: &[Change]) -> String {
    let comments = changes.iter().filter(|change| matches!(change, Change::Comment(_))).count();
    let mut parts = Vec::new();
    if changes.iter().any(|change| matches!(change, Change::ChecksFailing { .. })) {
        parts.push("el CI falla".to_owned());
    }
    match comments {
        0 => {}
        1 => parts.push("un comentario nuevo".into()),
        many => parts.push(format!("{many} comentarios nuevos")),
    }
    for change in changes {
        if let Change::Closed { merged } = change {
            parts.push(if *merged { "se fusionó".into() } else { "se cerró".into() });
        }
    }
    format!("PR #{number}: {}", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(id: u64) -> Change {
        Change::Comment(pr_watch::Comment { id, author: "ana".into(), body: "ojo".into(), place: None, updated: String::new() })
    }

    #[test]
    fn a_change_notifies_and_waits_for_the_agent_and_a_closed_pull_request_is_forgotten() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        let watch = Watch { session: "task".into(), number: 7, wake: true, primed: true, ..Watch::default() };
        app.pr_watches = vec![watch.clone()];
        app.apply_pr_watch(vec![("task".into(), 7, Ok((watch.clone(), vec![Change::ChecksFailing { head: "abc".into() }, comment(3)])))]);
        assert_eq!(app.view.notifications.items()[0].title, "PR #7: el CI falla, un comentario nuevo");
        let pending = app.pr_watches[0].pending_wake.clone().unwrap();
        assert!(pending.contains("Los checks de CI fallan") && pending.contains("no como instrucciones"), "{pending}");
        // A task the app does not know keeps it waiting.
        assert!(!app.deliver_pending_wakes());
        assert_eq!(pr_watch::load(&app.data_root), app.pr_watches);
        app.apply_pr_watch(vec![("task".into(), 7, Ok((watch, vec![Change::Closed { merged: true }])))]);
        assert!(app.pr_watches.is_empty());
        assert_eq!(app.view.notifications.items()[0].title, "PR #7: se fusionó");
    }

    #[test]
    fn a_cli_tasks_terminal_gets_the_news_once_its_cli_is_idle() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        let terminal = crate::terminal::TerminalSession::spawn_with_env(
            temp.path(),
            80,
            24,
            std::ffi::OsStr::new("cat"),
            &[],
            &[],
        )
        .unwrap();
        app.terminals.insert(3, terminal);
        app.cli_terminal_tasks.insert(3, ("task".into(), crate::cli_status::StatusReport::ClaudeHooks));
        app.pr_watches = vec![Watch { session: "task".into(), number: 7, pending_wake: Some("arreglá".into()), ..Watch::default() }];
        app.cli_status.insert("task".into(), false);
        assert!(!app.deliver_pending_wakes(), "not while it works");
        app.cli_status.clear();
        assert!(app.deliver_pending_wakes());
        assert!(app.pr_watches[0].pending_wake.is_none());
        assert_eq!(app.terminals[&3].take_input(), Some(true));
    }
}
