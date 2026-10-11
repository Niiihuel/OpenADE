//! Apply composer choices only after the admitted worker has released its turn.

use super::*;
use crate::composer_pending::{ComposerScope, PendingComposerModel, PendingComposerSelection};

impl AgentsWindow {
    pub(super) fn composer_choices_editable(&self) -> bool {
        self.view.selected_backend == "native"
            && !self.view.show_settings
            && self.view.main_page == MainPage::Workspace
    }

    pub(super) fn composer_selection_busy(&self) -> bool {
        self.view.running
            || self.view.background_run_selected
            || self.active_run.is_some()
            || self.active_resume.is_some()
            || self.active_child_recovery.is_some()
            || self.active_compact.is_some()
            || self.view.selected_session.as_ref().is_some_and(|selected| {
                self.subagent_dispatches.contains_key(selected)
                    || self.subagent_bridge.active_child_ids().contains(selected)
            })
    }

    /// The composer changes the policy for future turns. Unlike the global
    /// Settings revocation action, it must not stop commands owned by another
    /// conversation which may still have a live worker.
    pub(super) fn commit_composer_tool_settings(
        &mut self,
        next: AgentToolSettings,
        message: &str,
    ) -> bool {
        if !self.composer_choices_editable() || self.composer_selection_busy() {
            return false;
        }
        if let Err(error) = next.save(&self.data_root) {
            self.set_settings_notice(error, true);
            return false;
        }
        let revoke = next.revokes_commands_started_under(&self.agent_tool_settings);
        if revoke {
            if let Some(owner) = &self.view.selected_session {
                self.native_command_sessions.revoke_session(owner);
            }
            self.clear_native_command_ui();
        }
        self.agent_tool_settings = next;
        self.view.native_tool_permissions = next.permissions();
        self.view.native_full_access = next.full_access();
        self.view.native_command_isolation = next.command_isolation();
        self.view.workspace_writes_enabled =
            next.permissions().write_file && next.permissions().run_command;
        self.set_settings_notice(message, false);
        true
    }

    pub(super) fn composer_effort_options_for_selection(&self) -> &[(String, String)] {
        self.view
            .composer_pending
            .as_ref()
            .and_then(|pending| pending.model.as_ref())
            .map_or(self.view.composer_effort_options.as_slice(), |model| {
                model.route.effort_options.as_slice()
            })
    }

    pub(super) fn sync_pending_composer_selection(&mut self) -> bool {
        if self.pending_composer_selections.is_empty() && self.view.composer_pending.is_none() {
            return false;
        }
        let pending = self
            .pending_composer_selections
            .get(&ComposerScope::from_view(&self.view))
            .cloned();
        if self.view.composer_pending == pending {
            return false;
        }
        self.view.composer_pending = pending;
        true
    }

    fn store_pending_composer_selection(&mut self, mut pending: PendingComposerSelection) {
        pending.error = None;
        if pending.is_empty() {
            self.pending_composer_selections.remove(&pending.scope);
        } else {
            self.pending_composer_selections
                .insert(pending.scope.clone(), pending);
        }
        self.sync_pending_composer_selection();
        self.view.status = "Selección guardada para el próximo mensaje".into();
    }

    pub(super) fn queue_composer_model(
        &mut self,
        route: composer_selection::ModelRouteOption,
        effort: String,
    ) {
        let scope = ComposerScope::from_view(&self.view);
        let mut pending = self
            .pending_composer_selections
            .get(&scope)
            .cloned()
            .unwrap_or_else(|| PendingComposerSelection::new(scope));
        pending.model = if route.provider_id == self.view.native_provider_id
            && route.model_id == self.view.composer_model
            && effort == self.view.composer_effort
        {
            None
        } else {
            Some(PendingComposerModel { route, effort })
        };
        self.close_composer_floating_controls();
        self.store_pending_composer_selection(pending);
    }

    pub(super) fn queue_composer_permissions(&mut self, settings: AgentToolSettings) {
        let scope = ComposerScope::from_view(&self.view);
        let mut pending = self
            .pending_composer_selections
            .get(&scope)
            .cloned()
            .unwrap_or_else(|| PendingComposerSelection::new(scope));
        pending.permissions = (settings != self.agent_tool_settings).then_some(settings);
        pending.permission_base = pending.permissions.map(|_| self.agent_tool_settings);
        self.store_pending_composer_selection(pending);
    }

    pub(super) fn undo_pending_composer_selection(&mut self, token: u64) {
        let scope = ComposerScope::from_view(&self.view);
        if self
            .pending_composer_selections
            .get(&scope)
            .is_some_and(|pending| pending.token == token)
        {
            self.pending_composer_selections.remove(&scope);
            self.sync_pending_composer_selection();
            self.close_composer_floating_controls();
            self.view.composer_error = None;
            self.view.status = "Se mantendrá la selección anterior".into();
        }
    }

    fn pending_composer_error(&mut self, scope: &ComposerScope, message: String) {
        if let Some(pending) = self.pending_composer_selections.get_mut(scope) {
            pending.error = Some(message.clone());
        }
        self.view.status = message;
        self.sync_pending_composer_selection();
    }

    /// This is also called before admitting a new message. Terminal journal
    /// events alone do not unlock it: worker shutdown and its lease must finish.
    pub(super) fn poll_pending_composer_selection(&mut self) -> bool {
        let changed = self.sync_pending_composer_selection();
        if self.composer_selection_busy() || !self.composer_choices_editable() {
            return changed;
        }
        let scope = ComposerScope::from_view(&self.view);
        let Some(mut pending) = self.pending_composer_selections.get(&scope).cloned() else {
            return changed;
        };
        if pending.error.is_some() {
            return changed;
        }
        // Refresh discovery before trusting a queued route. Never substitute a
        // fallback model if the requested provider/model stopped being usable.
        if let Some(model) = &pending.model {
            let provider_status = self
                .view
                .composer_provider_status
                .get(&model.route.provider_id)
                .copied();
            if provider_status == Some(ProviderModelStatus::Checking) {
                return changed;
            }
            self.refresh_composer_routes();
            let route = composer_selection::model_route_option_by_id(
                &self.view.composer_routes,
                &model.route.route_id,
            );
            let valid = route.is_some_and(|route| {
                route.backend == "native"
                    && route.selectable()
                    && route.provider_id == model.route.provider_id
                    && route.model_id == model.route.model_id
                    && (model.effort == "default"
                        || route
                            .effort_options
                            .iter()
                            .any(|(id, _)| id == &model.effort))
            }) && provider_status != Some(ProviderModelStatus::CheckFailed);
            if !valid {
                self.pending_composer_error(
                    &scope,
                    "El modelo elegido ya no está disponible. Elige otro o deshaz el cambio."
                        .into(),
                );
                return true;
            }
        }
        if pending.permissions.is_some()
            && (pending.permission_base != Some(self.agent_tool_settings)
                || AgentToolSettings::load(&self.data_root).ok() != pending.permission_base)
        {
            self.pending_composer_error(
                &scope,
                "Los permisos cambiaron. Revisa los ajustes o deshaz este cambio antes del próximo mensaje.".into(),
            );
            return true;
        }
        // Idle selection methods are reused below. Remove the presentation
        // first so they cannot enqueue the same request a second time.
        self.pending_composer_selections.remove(&scope);
        self.sync_pending_composer_selection();
        if let Some(model) = pending.model.clone() {
            let saved = self.select_composer_route(&model.route.route_id);
            if !saved
                || self.view.native_provider_id != model.route.provider_id
                || self.view.composer_model != model.route.model_id
            {
                self.pending_composer_selections
                    .insert(scope.clone(), pending);
                let error = self.view.composer_error.take().unwrap_or_else(|| {
                    "No se pudo guardar el modelo elegido. Elige de nuevo o deshaz el cambio."
                        .into()
                });
                self.pending_composer_error(&scope, error);
                return true;
            }
            if !self.select_composer_effort(&model.effort) {
                self.pending_composer_selections
                    .insert(scope.clone(), pending);
                self.pending_composer_error(
                    &scope,
                    "No se pudo guardar el esfuerzo elegido. Elige de nuevo o deshaz el cambio."
                        .into(),
                );
                return true;
            }
            pending.model = None;
        }
        if let Some(settings) = pending.permissions {
            if let Err(error) = settings.save(&self.data_root) {
                self.pending_composer_selections
                    .insert(scope.clone(), pending);
                self.pending_composer_error(
                    &scope,
                    format!("No se pudieron guardar los permisos: {error}"),
                );
                return true;
            }
            // Only this completed conversation loses its persistent command
            // handles. Other conversations may still have immutable live turns.
            let revoke = settings.revokes_commands_started_under(&self.agent_tool_settings);
            if revoke {
                if let Some(session) = &scope.session {
                    self.native_command_sessions.revoke_session(session);
                }
                self.clear_native_command_ui();
            }
            self.agent_tool_settings = settings;
            self.view.native_tool_permissions = settings.permissions();
            self.view.native_full_access = settings.full_access();
            self.view.native_command_isolation = settings.command_isolation();
            self.view.workspace_writes_enabled =
                settings.permissions().write_file && settings.permissions().run_command;
        }
        self.pending_composer_selections.remove(&scope);
        self.sync_pending_composer_selection();
        self.view.status = "Selección aplicada para el próximo mensaje".into();
        true
    }
}
