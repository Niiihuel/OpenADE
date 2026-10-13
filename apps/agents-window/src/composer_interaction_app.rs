//! Composer actions reuse the native permission, skill review and attachment
//! pipelines. Suggestions never execute a tool or activate instructions.

use super::*;
use crate::composer_permissions_ui::{self, ComposerPermissionPreset};
use crate::composer_suggestions::{self, ComposerSuggestionAction, ComposerTriggerKind};

impl AgentsWindow {
    pub(super) fn composer_policy_editable(&self) -> bool {
        self.view.selected_backend == "native"
            && !self.view.running
            && !self.view.background_run_selected
            && !self.view.show_settings
            && self.view.main_page == MainPage::Workspace
    }

    pub(super) fn close_composer_floating_controls(&mut self) {
        self.close_environment();
        self.view.composer_permissions_open = false;
        self.view.composer_full_access_confirm = None;
        self.view.composer_add_menu_open = false;
        self.view.composer_model_menu_open = false;
        self.view.composer_effort_menu_open = false;
        self.view.composer_context_open = false;
        self.view.composer_project_menu_open = false;
        self.view.composer_branches.open = false;
        self.view.composer_suggestions.dismiss();
    }

    pub(super) fn select_composer_mode(&mut self, mode: TurnMode) {
        if self.composer_policy_editable() {
            self.close_composer_floating_controls();
            self.view.turn_mode = mode;
            self.view.status = match mode {
                TurnMode::Plan => {
                    "Modo Plan: inspeccionar y planificar sin editar ni ejecutar comandos"
                }
                TurnMode::Build => "Modo Agente: trabajar con los permisos elegidos",
            }
            .into();
        }
    }

    /// Returns true when a mode-only command was handled without a model turn.
    pub(super) fn prepare_composer_mode_command(&mut self) -> bool {
        if !self.composer_policy_editable() {
            return false;
        }
        let Some((mode, body)) = composer_suggestions::leading_mode_command(&self.view.prompt)
        else {
            return false;
        };
        self.select_composer_mode(mode);
        self.view.prompt = body.clone();
        self.tree.set_input_value("prompt", body);
        self.draft_dirty_at = Some(Instant::now());
        self.view.prompt.trim().is_empty() && self.view.composer_attachments.is_empty()
    }

    pub(super) fn apply_composer_interaction_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if let Some(token) = id.strip_prefix("composer_pending_undo:") {
            if let Ok(token) = token.parse() {
                self.undo_pending_composer_selection(token);
            }
            return true;
        }
        if let Some((confirm, token)) = id
            .strip_prefix("composer_full_access_confirm:")
            .map(|token| (true, token))
            .or_else(|| {
                id.strip_prefix("composer_full_access_cancel:")
                    .map(|token| (false, token))
            })
        {
            let Some(token) = token.parse::<u64>().ok() else {
                return true;
            };
            let Some(pending) = self.view.composer_full_access_confirm.as_ref() else {
                return true;
            };
            if pending.token != token {
                return true;
            }
            if !confirm {
                self.view.composer_full_access_confirm = None;
                return true;
            }
            let Some(pending) = self.view.composer_full_access_confirm.take() else {
                return true;
            };
            if !pending.matches_view(&self.view) || !self.composer_choices_editable() {
                return true;
            }
            if self.agent_tool_settings != pending.settings
                || AgentToolSettings::load(&self.data_root).ok() != Some(pending.settings)
            {
                self.view.composer_error = Some(
                    "Los permisos cambiaron. Revisa de nuevo antes de permitir acceso completo."
                        .into(),
                );
                return true;
            }
            let next = composer_permissions_ui::settings_for_preset(
                pending.settings,
                ComposerPermissionPreset::FullAccess,
            );
            let message = "Acceso completo habilitado para los próximos turnos";
            if self.composer_selection_busy() || self.view.composer_pending.is_some() {
                self.queue_composer_permissions(next);
            } else if self.commit_composer_tool_settings(next, message) {
                self.view.status = message.into();
            } else {
                self.view.composer_error = self.view.settings_notice.clone();
            }
            return true;
        }
        if let Some(preset) = ComposerPermissionPreset::from_action_id(id) {
            if self.composer_choices_editable() {
                self.close_composer_floating_controls();
                if preset == ComposerPermissionPreset::FullAccess {
                    self.view.composer_full_access_confirm =
                        Some(composer_permissions_ui::FullAccessConfirmation::new(
                            &self.view,
                            self.agent_tool_settings,
                        ));
                } else {
                    let next = composer_permissions_ui::settings_for_preset(
                        self.agent_tool_settings,
                        preset,
                    );
                    let message = if preset == ComposerPermissionPreset::ReadOnly {
                        "Herramientas de edición y comandos desactivadas para los próximos turnos"
                    } else if next.command_isolation().is_available_on_host() {
                        "Edición y comandos activos con aprobación para los próximos turnos"
                    } else {
                        "Edición activa con aprobación; comandos bloqueados por el aislamiento actual"
                    };
                    if self.composer_selection_busy() || self.view.composer_pending.is_some() {
                        self.queue_composer_permissions(next);
                    } else if self.commit_composer_tool_settings(next, message) {
                        self.view.status = message.into();
                    } else {
                        self.view.composer_error = self.view.settings_notice.clone();
                    }
                }
            }
            return true;
        }
        match id.as_str() {
            "composer:permissions" => {
                if self.composer_choices_editable() {
                    let open = !self.view.composer_permissions_open;
                    self.close_composer_floating_controls();
                    self.view.composer_permissions_open = open;
                }
            }
            "dismiss:composer_permissions" => self.view.composer_permissions_open = false,
            "composer:mode" => {
                if self.composer_choices_editable() {
                    let open = !self.view.composer_permissions_open;
                    self.close_composer_floating_controls();
                    self.view.composer_permissions_open = open;
                }
            }
            "composer_mode_dismiss" => self.view.composer_permissions_open = false,
            "composer_mode:plan" => self.select_composer_mode(TurnMode::Plan),
            "composer_mode:agent" => self.select_composer_mode(TurnMode::Build),
            "composer_suggestion_dismiss" => self.view.composer_suggestions.dismiss(),
            _ if id.starts_with("composer_suggestion:") => {
                if let Some((generation, index)) = id
                    .strip_prefix("composer_suggestion:")
                    .and_then(|value| value.split_once(':'))
                    .and_then(|(generation, index)| {
                        Some((generation.parse().ok()?, index.parse().ok()?))
                    })
                {
                    self.accept_composer_suggestion(generation, index, ctx);
                }
            }
            _ => return false,
        }
        true
    }

    pub(super) fn update_composer_suggestions(&mut self, ctx: &egui::Context) -> bool {
        // A suggestion is a pointer target, so PointerDown temporarily focuses
        // its row. Keep the frozen list mounted through PointerUp; its action
        // revalidates the token and workspace before changing the draft.
        let suggestion_focused = self.tree.focused_id().is_some_and(|id| {
            id.strip_prefix("composer_suggestion:")
                .and_then(|value| value.split_once(':'))
                .and_then(|(generation, index)| {
                    Some((generation.parse().ok()?, index.parse().ok()?))
                })
                .is_some_and(|(generation, index)| {
                    self.view
                        .composer_suggestions
                        .item(generation, index)
                        .is_some()
                })
        });
        if suggestion_focused {
            let cursor = self.tree.input_cursor("prompt");
            if self.tree.focus("prompt") {
                if let Some(cursor) = cursor {
                    self.tree.set_input_cursor("prompt", cursor);
                }
            }
        }
        let allowed = self.view.selected_backend == "native"
            && self.view.main_page == MainPage::Workspace
            && !self.view.show_settings
            && !self.view.show_command_center
            && self.view.composer_image_viewer.is_none()
            && !self.view.environment.open
            && !self.view.composer_permissions_open
            && !self.composer_popup_blocks_input()
            && self.tree.focused_id() == Some("prompt")
            && self
                .tree
                .input_selection("prompt")
                .is_some_and(|(start, end)| start == end);
        let trigger = allowed
            .then(|| {
                composer_suggestions::trigger_at_cursor(
                    &self.view.prompt,
                    self.tree
                        .input_cursor("prompt")
                        .unwrap_or_else(|| self.view.prompt.chars().count()),
                )
            })
            .flatten();
        let workspace = PathBuf::from(&self.view.workspace);
        let mut changed = self.view.composer_suggestions.sync(&workspace, trigger);
        if changed {
            self.tree.scroll_to_start(composer_suggestions::SCROLL_ID);
            self.composer_suggestion_pending_at = None;
            match self
                .view
                .composer_suggestions
                .trigger
                .as_ref()
                .map(|trigger| trigger.kind)
            {
                Some(ComposerTriggerKind::Command) => {
                    // Read on each lookup, so switching a plugin off takes its
                    // commands away at once. A handful of small folders.
                    self.view.composer_suggestions.plugin_commands =
                        crate::compat_hook_settings::enabled_plugin_roots(&self.data_root)
                            .iter()
                            .flat_map(|(id, root)| {
                                crate::compat_hook_settings::plugin_commands(id, root)
                            })
                            .collect();
                    self.view.composer_suggestions.installed_skills =
                        crate::marketplace::active_skill_names(&self.data_root, Path::new(&self.view.workspace));
                    self.view
                        .composer_suggestions
                        .populate_commands(&self.view.native_skills);
                }
                Some(ComposerTriggerKind::File) => {
                    self.composer_suggestion_pending_at = Some(Instant::now())
                }
                None => {}
            }
        }
        if let Some(queued) = self.composer_suggestion_pending_at {
            if queued.elapsed() >= Duration::from_millis(100) {
                self.composer_suggestion_pending_at = None;
                let submitted = (|| {
                    if self.composer_suggestion_worker.is_none() {
                        self.composer_suggestion_worker = Some(
                            composer_suggestions::FileSuggestionWorker::spawn()
                                .map_err(|error| error.to_string())?,
                        );
                    }
                    if let Some(request) = self.view.composer_suggestions.file_request() {
                        self.composer_suggestion_worker
                            .as_ref()
                            .unwrap()
                            .submit(request)?;
                    }
                    Ok::<_, String>(())
                })();
                if let Err(error) = submitted {
                    self.view.composer_suggestions.loading = false;
                    self.view.composer_suggestions.error = Some(error);
                }
                changed = true;
            } else {
                ctx.request_repaint_after(
                    Duration::from_millis(100).saturating_sub(queued.elapsed()),
                );
            }
        }
        if let Some(response) = self
            .composer_suggestion_worker
            .as_ref()
            .and_then(|worker| worker.poll_latest())
        {
            if self.view.composer_suggestions.accept_files(response) {
                self.tree.scroll_to_start(composer_suggestions::SCROLL_ID);
                changed = true;
            }
        }
        if self.view.composer_suggestions.loading {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        changed
    }

    pub(super) fn handle_composer_suggestion_key(
        &mut self,
        event: &UiEvent,
        ctx: &egui::Context,
    ) -> bool {
        if self.tree.focused_id() != Some("prompt") || self.composer_popup_blocks_input() {
            return false;
        }
        if self.view.composer_suggestions.trigger.is_none() {
            return false;
        }
        let UiEvent::Key {
            key,
            shift: false,
            ctrl: false,
        } = event
        else {
            return false;
        };
        match key {
            openide_ui::Key::ArrowUp => self.view.composer_suggestions.move_selection(-1),
            openide_ui::Key::ArrowDown => self.view.composer_suggestions.move_selection(1),
            openide_ui::Key::Escape => self.view.composer_suggestions.dismiss(),
            openide_ui::Key::Enter | openide_ui::Key::Tab => {
                if self.view.composer_suggestions.selected_item().is_some() {
                    self.accept_composer_suggestion(
                        self.view.composer_suggestions.generation,
                        self.view.composer_suggestions.selected,
                        ctx,
                    );
                } else if !self.view.composer_suggestions.loading {
                    return false;
                }
            }
            _ => return false,
        }
        if matches!(key, openide_ui::Key::ArrowUp | openide_ui::Key::ArrowDown)
            && !self.view.composer_suggestions.items.is_empty()
        {
            self.tree.scroll_to_index(
                composer_suggestions::SCROLL_ID,
                self.view.composer_suggestions.selected_scroll_index(),
            );
        }
        true
    }

    pub(super) fn accept_composer_suggestion(
        &mut self,
        generation: u64,
        index: usize,
        ctx: &egui::Context,
    ) {
        let Some(item) = self
            .view
            .composer_suggestions
            .item(generation, index)
            .cloned()
        else {
            return;
        };
        let Some(trigger) = self.view.composer_suggestions.trigger.clone() else {
            return;
        };
        if Path::new(&self.view.workspace) != self.view.composer_suggestions.workspace {
            return;
        }
        if self.view.running || self.view.background_run_selected {
            if !matches!(item.action, ComposerSuggestionAction::ReferenceFile(_)) {
                self.view.status =
                    "Espera a que termine el turno para cambiar el modo o las skills".into();
                self.view.composer_suggestions.dismiss();
                return;
            }
        }
        let mut attachment = None;
        let replacement = match &item.action {
            ComposerSuggestionAction::ReferenceFile(path) => {
                match composer_attachment_for_path(Path::new(&self.view.workspace), path) {
                    Ok(file) => {
                        let already_attached = self
                            .view
                            .composer_attachments
                            .iter()
                            .any(|existing| existing.path == file.path);
                        if !already_attached
                            && self.view.composer_attachments.len() >= MAX_COMPOSER_ATTACHMENTS
                        {
                            self.view.composer_error =
                                Some("Se alcanzó el límite de adjuntos del mensaje".into());
                            return;
                        }
                        let value = format!(
                            "@{} ",
                            serde_json::to_string(&file.path).unwrap_or_default()
                        );
                        attachment = Some(file);
                        value
                    }
                    Err(error) => {
                        self.view.composer_error = Some(error);
                        return;
                    }
                }
            }
            ComposerSuggestionAction::Files => "@".into(),
            // The prompt lands in the composer; the user adds the request and sends it.
            ComposerSuggestionAction::PluginCommand(file) => {
                match crate::compat_hook_settings::expand_command(file) {
                    Ok(body) => body,
                    Err(error) => {
                        self.view.composer_error = Some(error);
                        return;
                    }
                }
            }
            ComposerSuggestionAction::UseSkill(name) => format!("Usá la skill «{name}» para "),
            _ => String::new(),
        };
        let Some((prompt, cursor)) =
            composer_suggestions::replace_trigger(&self.view.prompt, &trigger, &replacement)
        else {
            return;
        };
        self.view.prompt = prompt.clone();
        self.tree.set_input_value("prompt", prompt);
        self.view.composer_suggestions.dismiss();
        if let Some(file) = attachment {
            if !self
                .view
                .composer_attachments
                .iter()
                .any(|existing| existing.path == file.path)
            {
                self.view.composer_attachments.push(file);
                self.attachment_drafts
                    .insert(self.draft_key(), self.view.composer_attachments.clone());
            }
        }
        self.draft_dirty_at = Some(Instant::now());
        self.prompt_focus_pending = true;
        self.composer_cursor_pending = Some(cursor);
        match item.action {
            ComposerSuggestionAction::Mode(mode) => self.select_composer_mode(mode),
            ComposerSuggestionAction::Permissions => {
                self.close_composer_floating_controls();
                self.view.composer_permissions_open = true;
                self.prompt_focus_pending = false;
            }
            ComposerSuggestionAction::ReviewChanges => self.start_isolated_review(),
            ComposerSuggestionAction::Skills => {
                self.view.show_settings = true;
                self.view.settings_page = SettingsPage::Skills;
                self.refresh_native_skills();
                self.clear_settings_notice();
                self.visit_navigation();
            }
            ComposerSuggestionAction::ReviewSkill(selection) => {
                // Preserve the observed exact path/hash. A stale or changed
                // document must go through the existing review checks again.
                let selected = self
                    .view
                    .native_skills
                    .available
                    .iter()
                    .position(|entry| entry.selection == selection);
                if let Some(index) = selected {
                    self.view.show_settings = true;
                    self.view.settings_page = SettingsPage::Skills;
                    self.clear_settings_notice();
                    self.apply_action(
                        UiAction::Clicked {
                            id: format!("native_skill_preview:{index}"),
                        },
                        ctx,
                    );
                    self.visit_navigation();
                }
            }
            ComposerSuggestionAction::Files
            | ComposerSuggestionAction::ReferenceFile(_)
            | ComposerSuggestionAction::PluginCommand(_)
            | ComposerSuggestionAction::UseSkill(_) => {}
        }
        if !self.view.show_settings && !self.view.composer_permissions_open {
            // Restore the caret before another Text event in this same frame.
            self.focus_prompt_if_pending();
        }
    }
}
