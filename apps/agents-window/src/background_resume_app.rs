//! Paused turns that continue while their conversation is not on screen.
//!
//! The window's own turn belongs to the conversation on screen. A paused
//! tree elsewhere continues the way an automation's run does: off screen,
//! at most two such workers at once, each under its session's lease. Nothing
//! here reads the composer or changes what is selected.

use super::*;
use crate::notifications::Event;
use crate::pause::AutoResume;

enum Worker {
    Turn(ResumeWorkerHandle),
    Child(SubagentRecoveryHandle),
}

/// How one worker of a tree ended.
enum Ended {
    /// This session did its part: the rest of the tree goes on.
    Next,
    Completed,
    /// Paused again, or nothing was left to do. A new pause is the clock's
    /// to arm again, within its cap.
    Quiet,
    /// It stopped at something only a person decides.
    NeedsPerson,
    Failed(String),
}

#[derive(Clone, Copy)]
enum Stopped {
    NeedsPerson,
    ByUser,
}

impl Worker {
    fn cancel(&self) {
        match self {
            Self::Turn(handle) => handle.cancel(),
            Self::Child(handle) => handle.cancel(),
        }
    }

    /// What arrived since the last frame: the journaled events, whether the
    /// worker waits for a person, and how it ended once it has.
    fn drain(&self) -> (Vec<AgentEvent>, bool, Option<Ended>) {
        let mut events = Vec::new();
        let mut needs_person = false;
        let mut ended = None;
        let disconnected = match self {
            Self::Turn(handle) => {
                let (updates, disconnected) = drain_ui_updates(&handle.updates);
                for update in updates {
                    match update {
                        ResumeUpdate::Events(batch) => events.extend(batch),
                        ResumeUpdate::NeedsPermission { .. }
                        | ResumeUpdate::NeedsVerification { .. } => needs_person = true,
                        ResumeUpdate::Finished(result) => {
                            ended = Some(match result {
                                Ok(ResumeDisposition::Complete) => Ended::Completed,
                                Ok(ResumeDisposition::Paused | ResumeDisposition::AbortedTurn) => {
                                    Ended::Quiet
                                }
                                Ok(_) => Ended::NeedsPerson,
                                Err(error) => Ended::Failed(error),
                            })
                        }
                    }
                }
                disconnected
            }
            Self::Child(handle) => {
                let (updates, disconnected) = drain_ui_updates(&handle.updates);
                for update in updates {
                    match update {
                        SubagentRecoveryUpdate::NeedsPermission { .. } => needs_person = true,
                        SubagentRecoveryUpdate::Finished(result) => {
                            ended = Some(match result {
                                // Finished, or paused again: its parent goes
                                // on and waits for it if it has to.
                                Ok(SubagentRecoveryState::Terminal(_))
                                | Ok(SubagentRecoveryState::Paused {
                                    reason: SubagentRecoveryPause::ProviderPaused,
                                    ..
                                }) => Ended::Next,
                                Ok(SubagentRecoveryState::Paused { .. }) => Ended::NeedsPerson,
                                Err(error) => Ended::Failed(error),
                            })
                        }
                    }
                }
                disconnected
            }
        };
        if disconnected && ended.is_none() {
            ended = Some(Ended::Failed("El worker terminó sin un resultado final".into()));
        }
        (events, needs_person, ended)
    }
}

/// A paused tree the clock is continuing off screen, one session at a time.
pub(super) struct BackgroundResume {
    /// The paused root: what the notifications point at.
    root_id: String,
    /// The session whose worker runs now.
    session_id: String,
    worker: Worker,
    /// The rest of the tree, leaves first.
    pending: VecDeque<String>,
    /// Why this window stopped the worker itself, when it did.
    stopped: Option<Stopped>,
}

impl BackgroundResume {
    /// Whether `session_id` is part of this tree: no other turn of it starts.
    pub(super) fn holds(&self, session_id: &str) -> bool {
        self.root_id == session_id
            || self.session_id == session_id
            || self.pending.iter().any(|id| id == session_id)
    }

    /// The stop button of a conversation opened while it continues.
    pub(super) fn stop_by_user(&mut self) {
        self.stop(Stopped::ByUser);
    }

    fn stop(&mut self, why: Stopped) {
        self.worker.cancel();
        self.stopped.get_or_insert(why);
    }
}

impl AgentsWindow {
    /// What holds this session off screen, worded as the reason a turn of it
    /// cannot start here.
    pub(super) fn off_screen_busy(&self, session_id: &str) -> Option<&'static str> {
        if self.parked_runs.iter().any(|parked| parked.run.session_id() == session_id) {
            Some("Espera a que termine el turno")
        } else if self
            .background_automation_runs
            .iter()
            .any(|run| run.session_id == session_id)
        {
            Some("Espera a que termine la automatización")
        } else if self.background_resumes.iter().any(|resume| resume.holds(session_id)) {
            Some("Espera a que termine la continuación automática")
        } else {
            None
        }
    }

    /// The root of the tree `session_id` belongs to; itself without a parent.
    fn tree_root(&self, session_id: &str) -> String {
        let mut root = session_id.to_owned();
        // Bounded by the list: a chain of parents visits each child once.
        for _ in 0..self.view.subagent_sessions.len() {
            match self
                .view
                .subagent_sessions
                .iter()
                .find(|run| run.child_session_id == root)
            {
                Some(run) => root = run.parent_session_id.clone(),
                None => break,
            }
        }
        root
    }

    /// Paused trees that are not on screen and whose limit renewed continue
    /// off screen, under the same plan the selected one answers to. One that
    /// finds both off-screen workers taken stays armed.
    pub(super) fn arm_background_resumes(&mut self) -> bool {
        let selected = self.view.selected_session.clone();
        let on_screen = selected.as_deref().map(|selected| self.tree_root(selected));
        let due: Vec<(String, u64)> = self
            .view
            .pause
            .sessions
            .iter()
            .filter(|(id, pause)| {
                pause.parent.is_none()
                    && selected.as_deref() != Some(id.as_str())
                    && matches!(self.view.pause.plan(id, pause),
                        AutoResume::At(due) if self.view.pause.clock_ms >= due)
            })
            .map(|(id, pause)| (id.clone(), pause.paused_at_ms))
            .collect();
        let (mut changed, mut started) = (false, false);
        for (root, paused_at_ms) in due {
            if on_screen.as_deref() == Some(root.as_str()) {
                // One of its subagents is on screen, where the window's own
                // turn works: the root is announced, as it always was.
                self.view.pause.auto_attempted.insert(root.clone(), paused_at_ms);
                self.notify_session(
                    Event::TurnCompleted,
                    "Límite renovado: abrí la tarea para continuar",
                    &root,
                );
                changed = true;
                continue;
            }
            // A run waiting in the queue, which a person may have asked for,
            // takes a free worker before the clock does.
            if self.background_automation_runs.len() + self.background_resumes.len() >= 2
                || !self.automation_queue.is_empty()
                || self.view.composer_branches.switching()
                || self.plugin_removal_pending
            {
                continue;
            }
            // One attempt per pause, whatever comes of it: a failure is told
            // once and never retried by the clock.
            self.view.pause.auto_attempted.insert(root.clone(), paused_at_ms);
            let mut pending = crate::pause::resume_order(&self.view.pause.sessions, &root).into();
            match self.start_next_off_screen(&mut pending) {
                Ok(Some((session_id, worker))) => {
                    // The timer never opens the gate: a limit that has not
                    // renewed for the provider pauses the turn again.
                    self.notify_session(
                        Event::TurnCompleted,
                        "Límite renovado: el turno continúa",
                        &root,
                    );
                    self.background_resumes.push(BackgroundResume {
                        root_id: root,
                        session_id,
                        worker,
                        pending,
                        stopped: None,
                    });
                    started = true;
                }
                // Continued or cancelled by hand in the meantime.
                Ok(None) => {}
                Err(error) => self.notify_off_screen_failure(&root, &error),
            }
            changed = true;
        }
        if started {
            // The tree no longer reads as paused while it continues.
            self.refresh_sessions();
            self.sync_background_selected();
        }
        changed
    }

    fn notify_off_screen_failure(&mut self, root: &str, error: &str) {
        self.notify_session(
            Event::TurnFailed,
            &format!("No se pudo continuar el turno en pausa: {error}"),
            root,
        );
    }

    /// Start the next session of a tree that is still paused, from its own
    /// journal. None: nothing of the tree is left paused.
    fn start_next_off_screen(
        &mut self,
        pending: &mut VecDeque<String>,
    ) -> Result<Option<(String, Worker)>, String> {
        let store = self.store.clone().ok_or("El historial no está disponible")?;
        while let Some(session_id) = pending.pop_front() {
            let stored = store
                .load(&session_id)
                .map_err(|error| format!("No se pudo leer la sesión: {error}"))?
                .ok_or("La sesión ya no existe")?;
            let replay = replay_stored_native(&self.data_root, &stored)?;
            // Continued, cancelled or moved by hand since it was queued.
            if crate::pause::PausedTurn::from_replay(&replay, &stored.session).is_none() {
                continue;
            }
            let worker = if stored.session.subagent.is_some() {
                Worker::Child(SubagentRecoveryHandle::start(
                    self.child_worker_request(&stored, true)?,
                )?)
            } else if let Some(frozen) =
                crate::native_automation::FrozenNativeAutomationAuthorityV1::from_session(
                    &stored.session,
                )?
            {
                Worker::Turn(self.native_automation_resume_handle(&stored.session, frozen)?)
            } else {
                let (request, command_actor) = self.root_resume_request(&stored, &replay, true)?;
                Worker::Turn(ResumeWorkerHandle::start_with_command_actor(
                    request,
                    Some(command_actor),
                )?)
            };
            return Ok(Some((session_id, worker)));
        }
        Ok(None)
    }

    /// Follow the workers continuing off screen. What only a person decides
    /// (a permission, a question, an uncertain tool) stops the worker there
    /// and is handed over: nothing is answered for them.
    pub(super) fn poll_background_resumes(&mut self) -> bool {
        let mut changed = false;
        let mut index = 0;
        while index < self.background_resumes.len() {
            let (events, needs_person, ended) = self.background_resumes[index].worker.drain();
            if needs_person {
                self.background_resumes[index].stop(Stopped::NeedsPerson);
            }
            let session_id = self.background_resumes[index].session_id.clone();
            // Opened while it continues: its transcript follows the journal.
            let shown = self.view.selected_session.as_deref() == Some(session_id.as_str());
            let follow_tail = shown && self.following_selected_transcript();
            let mut transcript_changed = false;
            for event in events.iter().filter(|_| shown) {
                if event.sequence > self.last_sequence {
                    self.last_sequence = event.sequence;
                    transcript_changed |= self.projection.apply_event(event);
                }
            }
            if transcript_changed {
                self.publish_transcript();
                changed = true;
            }
            // Its end adds the turn's actions row, after the host stopped following.
            if follow_tail && (transcript_changed || ended.is_some()) {
                self.tree.scroll_to_end("transcript");
            }
            let Some(ended) = ended else {
                index += 1;
                continue;
            };
            changed = true;
            if shown {
                if let Some(Ok(Some(stored))) = self.store.as_ref().map(|store| store.load(&session_id)) {
                    self.last_sequence = stored.events.last().map_or(0, |event| event.sequence);
                    self.projection = TranscriptProjection::from_events(&stored.events);
                    self.publish_transcript();
                }
            }
            self.reconcile_native_automation_session(&session_id);
            let ended = match (self.background_resumes[index].stopped, ended) {
                (Some(Stopped::ByUser), _) => Ended::Quiet,
                (Some(Stopped::NeedsPerson), _) => Ended::NeedsPerson,
                (None, Ended::Next) => {
                    let mut pending = std::mem::take(&mut self.background_resumes[index].pending);
                    match self.start_next_off_screen(&mut pending) {
                        Ok(Some((session_id, worker))) => {
                            let resume = &mut self.background_resumes[index];
                            resume.session_id = session_id;
                            resume.worker = worker;
                            resume.pending = pending;
                            Ended::Next
                        }
                        Ok(None) => Ended::Quiet,
                        Err(error) => Ended::Failed(error),
                    }
                }
                (None, ended) => ended,
            };
            if matches!(ended, Ended::Next) {
                index += 1;
            } else {
                let watched = self
                    .view
                    .selected_session
                    .as_deref()
                    .is_some_and(|selected| self.background_resumes[index].holds(selected));
                let root = self.background_resumes.remove(index).root_id;
                if watched {
                    self.view.status = "Continuación finalizada".to_owned();
                    self.rebuild_history();
                }
                match ended {
                    Ended::Completed => {
                        self.notify_session(Event::TurnCompleted, "Turno completado", &root)
                    }
                    Ended::NeedsPerson => self.notify_session(
                        Event::Approval,
                        "El turno continuó y espera tu decisión",
                        &root,
                    ),
                    Ended::Failed(error) => {
                        if watched {
                            self.view.status =
                                format!("No se pudo continuar el turno en pausa: {error}");
                        }
                        self.notify_off_screen_failure(&root, &error);
                    }
                    Ended::Quiet | Ended::Next => {}
                }
            }
            self.refresh_sessions();
        }
        if changed {
            self.sync_background_selected();
        }
        changed
    }
}
