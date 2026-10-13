//! The host side of continuing a paused conversation on another model.

use super::*;
use crate::model_switch::{
    host_turn, unfinished_switch, SwitchBlocker, SwitchConfirm, SwitchStep, UnfinishedSwitch,
};

impl AgentsWindow {
    /// A route was picked while the selected turn is paused: ask before
    /// moving it, with whatever stands in the way stated first.
    pub(super) fn open_model_switch(&mut self, route: &composer_selection::ModelRouteOption) -> bool {
        let Some(session_id) = self.view.selected_session.clone() else {
            return false;
        };
        let Some(pause) = self.view.pause.sessions.get(&session_id).cloned() else {
            return false;
        };
        let order = crate::pause::resume_order(&self.view.pause.sessions, &session_id);
        let working = self
            .view
            .subagent_sessions
            .iter()
            .filter(|run| {
                order.contains(&run.parent_session_id)
                    && run.status == openide_harness::SubagentStatus::Running
            })
            .count();
        let mut notices = Vec::new();
        let effort = self.view.composer_effort.clone();
        if effort != "default" && !route.effort_options.iter().any(|(id, _)| id == &effort) {
            notices.push(format!(
                "El esfuerzo «{effort}» no está en {}: se usa el predeterminado.",
                route.model_name
            ));
        }
        // Said before, not only after: the switch may summarise.
        let history = self
            .store
            .as_ref()
            .and_then(|store| store.load(&session_id).ok().flatten())
            .and_then(|stored| replay_stored_native(&self.data_root, &stored).ok())
            .map(|replay| replay.history)
            .unwrap_or_default();
        if self.history_needs_summary(&history, &route.provider_id, &route.model_id) {
            notices.push(format!(
                "La conversación no entra en {}: se resume antes de continuar.",
                route.model_name
            ));
        }
        let blocker = self.model_switch_blocker(&session_id, route, pause.parent.is_some());
        self.view.model_switch.confirm = Some(SwitchConfirm {
            session_id,
            route_id: route.route_id.clone(),
            model_name: route.model_name.clone(),
            moving: order.len(),
            working,
            child: pause.parent.is_some(),
            notices,
            blocker,
        });
        self.view.composer_model_menu_open = false;
        self.view.composer_model_list_open = false;
        true
    }

    /// What prevents `route` from taking the paused turn, checked before
    /// anything is written or sent.
    fn model_switch_blocker(
        &self,
        session_id: &str,
        route: &composer_selection::ModelRouteOption,
        child: bool,
    ) -> Option<SwitchBlocker> {
        let settings = NativeProviderSettings {
            provider_id: route.provider_id.clone(),
            model_id: route.model_id.clone(),
            custom_endpoint: self.view.endpoint.clone(),
        };
        let model = settings.resolve_at(&self.data_root).and_then(|connection| {
            if !self.provider_ready_for_turn(&route.provider_id, &connection) {
                return Err(format!("{} no está conectado.", route.provider_name));
            }
            native_model_backend(
                connection,
                &route.provider_id,
                None,
                self.view.hosted_web_search_mode,
                &self.data_root,
                self.view.catalog.as_deref(),
                &self.model_discoveries,
            )
        });
        let caps = match model {
            Ok(model) => model.target_caps(),
            Err(error) => return Some(SwitchBlocker::Refused(error)),
        };
        if !caps.tools {
            return Some(SwitchBlocker::Refused(format!(
                "Este turno necesita herramientas y {} no las admite.",
                route.model_name
            )));
        }
        // A turn paused before its first response is sent again as it was
        // typed, images included; a subagent's task is always text.
        if caps.vision || child {
            return None;
        }
        let events = self.store.as_ref()?.load(session_id).ok()??.events;
        let admitted = events.iter().rposition(|event| {
            matches!(
                event.kind,
                AgentEventKind::TurnAdmitted { .. } | AgentEventKind::TurnAdmittedWithImages { .. }
            )
        })?;
        let answered = events[admitted..]
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ModelResponseCommitted { .. }));
        match &events[admitted].kind {
            AgentEventKind::TurnAdmittedWithImages { images, .. } if !answered => {
                Some(SwitchBlocker::Images { count: images.len() })
            }
            _ => None,
        }
    }

    pub(super) fn apply_model_switch_action(&mut self, id: &str) {
        match id {
            "model_switch_cancel" => self.view.model_switch.confirm = None,
            "model_switch_other" => {
                self.view.model_switch.confirm = None;
                self.view.composer_model_menu_open = true;
            }
            "model_switch_confirm" => self.perform_model_switch(false),
            "model_switch_without_images" => self.perform_model_switch(true),
            _ => {
                if let Some(session_id) = id.strip_prefix("model_switch_finish:") {
                    let session_id = session_id.to_owned();
                    self.finish_model_switch(&session_id, None, false);
                }
            }
        }
    }

    fn perform_model_switch(&mut self, drop_images: bool) {
        let Some(confirm) = self.view.model_switch.confirm.take() else {
            return;
        };
        if matches!(confirm.blocker, Some(SwitchBlocker::Refused(_))) {
            return;
        }
        if !self.pause_idle() {
            self.view.status = "Espera a que termine la operación activa".to_owned();
            return;
        }
        let Some(route) = composer_selection::model_route_option_by_id(
            &self.view.composer_routes,
            &confirm.route_id,
        )
        .cloned() else {
            self.view.status = "El modelo elegido ya no está disponible".to_owned();
            return;
        };
        if !confirm.child {
            // Closing the turn is the point of no return: from here the
            // journal itself says where the conversation continues.
            let closed = (|| -> Result<(), String> {
                let store = self.store.clone().ok_or("El historial no está disponible")?;
                let checkpoints = CheckpointFileStore::new(self.data_root.join("sessions"))
                    .map_err(|error| format!("No se pudo abrir checkpoints: {error}"))?;
                let lease = store
                    .try_acquire_native_lease(&confirm.session_id)
                    .map_err(|error| format!("La sesión está en uso: {error}"))?;
                openide_harness::supersede_paused_turn(
                    &store,
                    &checkpoints,
                    &lease,
                    &route.provider_id,
                    &route.model_id,
                    &Uuid::new_v4().to_string(),
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
            })();
            if let Err(error) = closed {
                self.view.status = format!("No se pudo cambiar de modelo: {error}");
                return;
            }
        }
        self.finish_model_switch(&confirm.session_id, Some(route), drop_images);
    }

    /// Everything after the root turn was closed. Runs again from the
    /// journal when the app stopped in the middle: each part either already
    /// happened or happens now.
    pub(super) fn finish_model_switch(
        &mut self,
        session_id: &str,
        route: Option<composer_selection::ModelRouteOption>,
        drop_images: bool,
    ) {
        if !self.pause_idle() {
            self.view.status = "Espera a que termine la operación activa".to_owned();
            return;
        }
        let Some(store) = self.store.clone() else {
            return;
        };
        let stored = store.load(session_id).ok().flatten();
        let unfinished = stored.as_ref().and_then(|stored| unfinished_switch(&stored.events));
        let child = stored.as_ref().is_some_and(|stored| stored.session.subagent.is_some());
        let route = route.or_else(|| {
            let switch = unfinished.as_ref()?;
            self.view
                .composer_routes
                .iter()
                .find(|route| {
                    route.backend == "native"
                        && route.provider_id == switch.provider_id
                        && route.model_id == switch.model_id
                })
                .cloned()
        });
        let Some(route) = route else {
            self.note_unfinished_switch(session_id, unfinished, "El modelo elegido ya no está disponible.");
            return;
        };
        if self.view.selected_session.as_deref() != Some(session_id) {
            self.open_session(session_id);
        }
        // The route every later step builds its backend from.
        self.model_switch_applying = true;
        let selected = self.select_composer_route(&route.route_id);
        self.model_switch_applying = false;
        if !selected {
            let reason = self
                .view
                .composer_error
                .clone()
                .unwrap_or_else(|| self.view.status.clone());
            self.note_unfinished_switch(session_id, unfinished, &reason);
            return;
        }
        if !route.effort_options.iter().any(|(id, _)| id == &self.view.composer_effort) {
            self.view.composer_effort = "default".to_owned();
        }
        // Paused subagents first, leaves first; the root's own turn last.
        let mut order = crate::pause::resume_order(&self.view.pause.sessions, session_id);
        if !child {
            order.retain(|id| id != session_id);
        }
        let mut notes = Vec::new();
        // The workers below are built for the new route on purpose.
        self.model_switch_applying = true;
        for child_id in order {
            match self.switch_paused_child(&store, &child_id) {
                Ok(()) => self.switch_queue.push_back(SwitchStep::ResumeChild(child_id)),
                Err(error) => notes.push(self.cancel_unmovable_child(&store, &child_id, &error)),
            }
        }
        self.model_switch_applying = false;
        if !child {
            self.switch_queue.push_back(SwitchStep::StartRoot {
                session_id: session_id.to_owned(),
                drop_images,
            });
        }
        self.view.model_switch.unfinished.remove(session_id);
        self.view.status = if notes.is_empty() {
            format!("Continuando en {}", route.model_name)
        } else {
            notes.join(" ")
        };
        self.refresh_sessions();
    }

    fn note_unfinished_switch(
        &mut self,
        session_id: &str,
        unfinished: Option<UnfinishedSwitch>,
        reason: &str,
    ) {
        self.view.status = format!("El cambio de modelo no terminó: {reason}");
        if let Some(mut switch) = unfinished {
            switch.error = Some(reason.to_owned());
            self.view.model_switch.unfinished.insert(session_id.to_owned(), switch);
        }
    }

    fn switch_paused_child(&self, store: &Arc<SessionStore>, child_id: &str) -> Result<(), String> {
        let stored = store
            .load(child_id)
            .map_err(|error| error.to_string())?
            .ok_or("El subagente ya no existe")?;
        let parent_id = stored
            .session
            .subagent
            .as_ref()
            .map(|link| link.parent_session_id.clone())
            .ok_or("La sesión no es un subagente")?;
        let request = self.child_worker_request(&stored, false)?;
        let parent_lease = store
            .try_acquire_native_lease(&parent_id)
            .map_err(|error| format!("La sesión padre está en uso: {error}"))?;
        request
            .worker
            .switch_paused_child(&parent_lease, child_id)
            .map_err(|error| error.to_string())
    }

    /// A subagent that cannot move safely is cancelled, not left paused on a
    /// route nobody will continue. Returns what to tell the person.
    fn cancel_unmovable_child(
        &mut self,
        store: &Arc<SessionStore>,
        child_id: &str,
        error: &str,
    ) -> String {
        let cancelled = (|| -> Result<Vec<String>, String> {
            let stored = store
                .load(child_id)
                .map_err(|error| error.to_string())?
                .ok_or("El subagente ya no existe")?;
            let parent_id = stored
                .session
                .subagent
                .as_ref()
                .map(|link| link.parent_session_id.clone())
                .ok_or("La sesión no es un subagente")?;
            let coordinator = SubagentCoordinator::new(store.clone(), SubagentLimits::default())
                .map_err(|error| error.to_string())?;
            let parent_lease = store
                .try_acquire_native_lease(&parent_id)
                .map_err(|error| error.to_string())?;
            let (_, affected) = coordinator
                .request_cancel_from_ui_with_affected(&parent_lease, child_id)
                .map_err(|error| error.to_string())?;
            let checkpoints = CheckpointFileStore::new(self.data_root.join("sessions"))
                .map_err(|error| error.to_string())?;
            if let Ok(child_lease) = store.try_acquire_native_lease(child_id) {
                // Already closed by a supersede: nothing more to write.
                let _ = openide_harness::cancel_paused_turn(store, &checkpoints, &child_lease);
            }
            Ok(affected)
        })();
        match cancelled {
            Ok(affected) => {
                for owner in affected {
                    self.native_command_sessions.revoke_session(&owner);
                }
                format!("Un subagente no pudo cambiar de modelo y se canceló: {error}.")
            }
            Err(cancel_error) => format!(
                "Un subagente no pudo cambiar de modelo ({error}) ni cancelarse ({cancel_error})."
            ),
        }
    }

    /// For each paused root with no end in sight (or every one, when the
    /// person chose automatic fallback): the first model of their list that
    /// is connected and whose limit is open. A proposal, shown on the card.
    pub(super) fn fallback_proposals(&self) -> BTreeMap<String, (String, String)> {
        let models = &self.view.model_switch.fallback_models;
        if models.is_empty() {
            return BTreeMap::new();
        }
        let available = models.iter().find_map(|entry| {
            let (provider_id, model_id) = entry.split_once(':')?;
            let route = self.view.composer_routes.iter().find(|route| {
                route.backend == "native"
                    && route.selectable()
                    && route.provider_id == provider_id
                    && route.model_id == model_id
            })?;
            let settings = NativeProviderSettings {
                provider_id: provider_id.to_owned(),
                model_id: model_id.to_owned(),
                custom_endpoint: self.view.endpoint.clone(),
            };
            let connection = settings.resolve_at(&self.data_root).ok()?;
            if !self.provider_ready_for_turn(provider_id, &connection) {
                return None;
            }
            let backend = native_model_backend(
                connection,
                provider_id,
                None,
                None,
                &self.data_root,
                self.view.catalog.as_deref(),
                &self.model_discoveries,
            )
            .ok()?;
            (!openide_harness::limit_gate_blocked(backend.as_ref()))
                .then(|| (route.route_id.clone(), route.model_name.clone()))
        });
        let Some(proposal) = available else {
            return BTreeMap::new();
        };
        self.view
            .pause
            .sessions
            .iter()
            .filter(|(_, pause)| {
                pause.parent.is_none()
                    && (pause.resets_at_ms.is_none() || self.view.model_switch.auto_fallback)
            })
            .map(|(id, _)| (id.clone(), proposal.clone()))
            .collect()
    }

    /// The card's one click: move to the proposed model, unless something
    /// stands in the way, which is then shown instead of done.
    pub(super) fn continue_on_fallback(&mut self, session_id: &str) {
        let Some((route_id, _)) = self.view.model_switch.proposals.get(session_id).cloned() else {
            self.view.status = "El modelo de respaldo ya no está disponible".to_owned();
            return;
        };
        if self.view.selected_session.as_deref() != Some(session_id) {
            self.open_session(session_id);
        }
        let Some(route) =
            composer_selection::model_route_option_by_id(&self.view.composer_routes, &route_id).cloned()
        else {
            return;
        };
        if self.open_model_switch(&route)
            && self.view.model_switch.confirm.as_ref().is_some_and(|confirm| confirm.blocker.is_none())
        {
            self.apply_model_switch_action("model_switch_confirm");
        }
    }

    /// Only with "Cambiar automáticamente al modelo de respaldo" on, only
    /// for the session on screen, and once per pause.
    pub(super) fn arm_auto_fallback(&mut self) -> bool {
        if !self.view.model_switch.auto_fallback
            || !self.pause_idle()
            || !self.switch_queue.is_empty()
            || !self.pause_resume_queue.is_empty()
        {
            return false;
        }
        let Some(session_id) = self.view.selected_session.clone() else {
            return false;
        };
        let Some(paused_at_ms) = self.view.pause.sessions.get(&session_id).map(|pause| pause.paused_at_ms)
        else {
            return false;
        };
        let Some((_, model)) = self.view.model_switch.proposals.get(&session_id).cloned() else {
            return false;
        };
        if self.fallback_attempted.insert(session_id.clone(), paused_at_ms) == Some(paused_at_ms) {
            return false;
        }
        self.notify_session(
            crate::notifications::Event::TurnCompleted,
            &format!("En pausa: continúa en {model}"),
            &session_id,
        );
        self.continue_on_fallback(&session_id);
        true
    }

    /// One step of a switch in progress, once nothing else is running.
    pub(super) fn drive_switch_queue(&mut self) -> bool {
        if self.switch_queue.is_empty() || !self.pause_idle() {
            return false;
        }
        let Some(step) = self.switch_queue.pop_front() else {
            return false;
        };
        let Some(store) = self.store.clone() else {
            return true;
        };
        match step {
            SwitchStep::ResumeChild(child_id) => {
                let loaded = store.load(&child_id).ok().flatten().and_then(|stored| {
                    let replay = replay_stored_native(&self.data_root, &stored).ok()?;
                    Some((stored, replay))
                });
                match loaded {
                    Some((stored, replay)) => self.start_child_resume(stored, replay),
                    None => self.view.status = "No se pudo leer el subagente".to_owned(),
                }
            }
            SwitchStep::StartRoot { session_id, drop_images } => {
                if self.view.selected_session.as_deref() != Some(session_id.as_str()) {
                    self.open_session(&session_id);
                }
                let stored = store.load(&session_id).ok().flatten();
                let unfinished = stored.as_ref().and_then(|stored| unfinished_switch(&stored.events));
                let turn = stored
                    .as_ref()
                    .zip(unfinished.as_ref())
                    .and_then(|(stored, switch)| host_turn(&stored.events, switch));
                let Some(mut turn) = turn else {
                    return true;
                };
                if drop_images {
                    turn.images.clear();
                }
                self.view.turn_mode = turn.mode;
                self.host_turn = Some(turn);
                self.start_run();
                self.host_turn = None;
                if self.active_run.is_none() {
                    let reason = self
                        .view
                        .composer_error
                        .clone()
                        .unwrap_or_else(|| self.view.status.clone());
                    self.note_unfinished_switch(&session_id, unfinished, &reason);
                    self.switch_queue.clear();
                }
            }
        }
        true
    }
}
