//! Read-only durable output workers. No manager, lease, or journal mutation.

use super::*;
use crate::native_command_history_ui::{
    HistoryScope, NativeCommandHistoryUiState, PAGE_BYTES, PAGE_RECORDS,
};
use openide_core::{
    NativeCommandOutputBinding, NativeCommandOutputPage, NativeCommandOutputStore,
    NativeCommandOutputSummary,
};

pub(super) struct HistoryResponse {
    pub scope: HistoryScope,
    pub request_id: String,
    pub store: Arc<SessionStore>,
    pub requested_binding: Option<NativeCommandOutputBinding>,
    pub cursor: u64,
    pub result: Result<
        (
            Vec<NativeCommandOutputSummary>,
            Option<NativeCommandOutputPage>,
            String,
        ),
        String,
    >,
}

impl AgentsWindow {
    fn command_history_surface_available(&self) -> bool {
        !self.view.show_settings
            && self.view.main_page == MainPage::Workspace
            && self.resource_pane_visible()
            && self.view.side_tab == SideTab::Activity
    }

    pub(super) fn open_native_command_history(&mut self, selected: Option<(String, String)>) {
        if !self.command_history_surface_available() {
            return;
        }
        let Some(owner) = self.view.selected_session.clone() else {
            return;
        };
        let Some(store) = self.store.clone() else {
            return;
        };
        let Ok(workspace) = Path::new(&self.view.workspace).canonicalize() else {
            return;
        };
        let scope = HistoryScope {
            session_id: owner.clone(),
            workspace: workspace.clone(),
        };
        let selected =
            selected.map(
                |(manager_generation, command_id)| NativeCommandOutputBinding {
                    session_id: owner,
                    workspace,
                    manager_generation,
                    command_id,
                },
            );
        self.view.native_command_control.open = false;
        self.view.native_command_output = Default::default();
        self.native_command_output_response = None;
        self.native_command_output_manager = None;
        self.view.native_command_history = NativeCommandHistoryUiState {
            scope: Some(scope),
            workspace_alias: Some(PathBuf::from(&self.view.workspace)),
            selected,
            ..Default::default()
        };
        self.request_native_command_history(store);
    }

    fn request_native_command_history(&mut self, store: Arc<SessionStore>) {
        let local = &mut self.view.native_command_history;
        let Some(scope) = local.scope.clone() else {
            return;
        };
        let request_id = Uuid::new_v4().to_string();
        let binding = local.selected.clone();
        let cursor = local.cursor;
        local.request_id = Some(request_id.clone());
        local.loading = true;
        local.error = None;
        local.selector_open = false;
        // A previous page is never presented as the newly requested range.
        local.page = None;
        local.page_json.clear();
        let (sender, receiver) = mpsc::channel();
        self.native_command_history_response = Some(receiver);
        std::thread::spawn(move || {
            let result = (|| {
                // list/read validate fresh native metadata, canonical workspace
                // and deletion state. SessionStore::load would also repair a
                // truncated journal, which a read-only output inspector must
                // leave to the session recovery workflow.
                let output = NativeCommandOutputStore::new(store.clone());
                let mut summaries = output
                    .list(&scope.session_id, &scope.workspace)
                    .map_err(|error| error.to_string())?;
                let selected = match &binding {
                    Some(binding) => summaries
                        .iter()
                        .find(|summary| &summary.header.binding == binding),
                    None => summaries.last(),
                };
                let Some(selected) = selected else {
                    if binding.is_some() {
                        return Err(
                            "No hay un registro de salida para la identidad exacta del comando"
                                .into(),
                        );
                    }
                    return Ok((summaries, None, String::new()));
                };
                let page = output
                    .read(&selected.header.binding, cursor, PAGE_BYTES, PAGE_RECORDS)
                    .map_err(|error| error.to_string())?;
                let json = crate::native_command_history_ui::page_json(&page);
                if let Some(summary) = summaries
                    .iter_mut()
                    .find(|summary| summary.header.binding == page.summary.header.binding)
                {
                    *summary = page.summary.clone();
                }
                Ok((summaries, Some(page), json))
            })();
            let _ = sender.send(HistoryResponse {
                scope,
                request_id,
                store,
                requested_binding: binding,
                cursor,
                result,
            });
        });
    }

    pub(super) fn poll_native_command_history(&mut self) -> bool {
        if self.view.native_command_history.scope.is_some()
            && (!self.command_history_surface_available()
                || !self.view.native_command_history.matches_scope(
                    self.view.selected_session.as_deref(),
                    Path::new(&self.view.workspace),
                ))
        {
            self.view.native_command_history = Default::default();
            self.native_command_history_response = None;
            return true;
        }
        let response = match self
            .native_command_history_response
            .as_ref()
            .map(Receiver::try_recv)
        {
            Some(Ok(response)) => response,
            Some(Err(TryRecvError::Disconnected)) => {
                self.native_command_history_response = None;
                self.view.native_command_history.loading = false;
                self.view.native_command_history.error =
                    Some("Se interrumpió la consulta del historial".into());
                return true;
            }
            _ => return false,
        };
        self.native_command_history_response = None;
        let local = &mut self.view.native_command_history;
        let request_matches = local.scope.as_ref() == Some(&response.scope)
            && local.request_id.as_ref() == Some(&response.request_id)
            && local.selected == response.requested_binding
            && local.cursor == response.cursor
            && self
                .store
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &response.store));
        if !request_matches {
            if local.request_id.as_ref() != Some(&response.request_id) {
                // A late worker is not allowed to clear a newer accepted page,
                // its error, or its pending request's loading state.
                return false;
            }
            local.loading = false;
            local.page = None;
            local.page_json.clear();
            local.error =
                Some("Se descartó una respuesta de otra consulta; actualiza el historial".into());
            return true;
        }
        local.loading = false;
        match response.result {
            Ok((summaries, page, json)) => {
                let validated = summaries.iter().all(|summary| {
                    summary.header.binding.session_id == response.scope.session_id
                        && summary.header.binding.workspace == response.scope.workspace
                        && local
                            .summaries
                            .iter()
                            .find(|known| known.header.binding == summary.header.binding)
                            .is_none_or(|known| summary.revision >= known.revision)
                }) && page.as_ref().is_none_or(|page| {
                    page.summary.header.binding.session_id == response.scope.session_id
                        && page.summary.header.binding.workspace == response.scope.workspace
                        && response
                            .requested_binding
                            .as_ref()
                            .is_none_or(|binding| &page.summary.header.binding == binding)
                        && page.next_cursor >= response.cursor
                        && page.next_cursor <= page.summary.observed_end
                        && local
                            .summaries
                            .iter()
                            .find(|summary| summary.header.binding == page.summary.header.binding)
                            .is_none_or(|known| page.summary.revision >= known.revision)
                });
                if !validated {
                    local.page = None;
                    local.page_json.clear();
                    local.error = Some("La página no corresponde a su identidad, cursor o revisión guardada; actualiza el historial".into());
                } else {
                    local.selected = page
                        .as_ref()
                        .map(|page| page.summary.header.binding.clone());
                    local.summaries = summaries;
                    local.page = page;
                    local.page_json = json;
                    local.error = None;
                }
            }
            Err(error) => {
                local.page = None;
                local.page_json.clear();
                local.error = Some(error);
            }
        }
        true
    }

    pub(super) fn apply_native_command_history_action(&mut self, id: &str) -> bool {
        if id == "native_command_history_open" {
            self.open_native_command_history(None);
            return true;
        }
        if let Some(raw) = id.strip_prefix("native_command_history_live:") {
            if let Ok((owner, command)) = serde_json::from_str::<(String, String)>(raw) {
                if self.view.selected_session.as_deref() == Some(&owner)
                    && self
                        .view
                        .native_commands
                        .iter()
                        .any(|entry| entry.id == command)
                {
                    if let Some(manager) = self.native_command_sessions.manager(&owner) {
                        if Path::new(&self.view.workspace)
                            .canonicalize()
                            .ok()
                            .as_deref()
                            == Some(manager.workspace())
                        {
                            self.open_native_command_history(Some((
                                manager.generation().into(),
                                command,
                            )));
                        }
                    }
                }
            }
            return true;
        }
        if let Some(raw) = id.strip_prefix("native_command_history_open:") {
            if let Ok((owner, generation, command)) =
                serde_json::from_str::<(String, String, String)>(raw)
            {
                if self.view.selected_session.as_deref() == Some(&owner) {
                    self.open_native_command_history(Some((generation, command)));
                }
            }
            return true;
        }
        if !id.starts_with("native_command_history_") {
            return false;
        }
        if !self.command_history_surface_available()
            || !self.view.native_command_history.matches_scope(
                self.view.selected_session.as_deref(),
                Path::new(&self.view.workspace),
            )
        {
            return true;
        }
        match id {
            "native_command_history_dismiss" => {
                self.view.native_command_history = Default::default();
                self.native_command_history_response = None;
            }
            "native_command_history_selector_toggle" => {
                self.view.native_command_history.selector_open =
                    !self.view.native_command_history.selector_open;
            }
            "native_command_history_selector_dismiss" => {
                self.view.native_command_history.selector_open = false;
            }
            "native_command_history_details_toggle" => {
                self.view.native_command_history.details_open =
                    !self.view.native_command_history.details_open;
            }
            "native_command_history_exact_toggle" => {
                self.view.native_command_history.exact_data_open =
                    !self.view.native_command_history.exact_data_open;
            }
            "native_command_history_refresh" if !self.view.native_command_history.loading => {
                if let Some(store) = self.store.clone() {
                    self.request_native_command_history(store);
                }
            }
            "native_command_history_previous" if !self.view.native_command_history.loading => {
                if let (Some(cursor), Some(store)) = (
                    self.view.native_command_history.previous_cursors.pop(),
                    self.store.clone(),
                ) {
                    self.view.native_command_history.cursor = cursor;
                    self.request_native_command_history(store);
                }
            }
            "native_command_history_next" if self.view.native_command_history.can_next() => {
                if let Some(store) = self.store.clone() {
                    let local = &mut self.view.native_command_history;
                    local.previous_cursors.push(local.cursor);
                    local.cursor = local.page.as_ref().expect("can next").next_cursor;
                    self.request_native_command_history(store);
                }
            }
            _ => {
                if !self.view.native_command_history.loading {
                    if let Some((generation, command)) = id
                        .strip_prefix("native_command_history_select:")
                        .and_then(|raw| serde_json::from_str::<(String, String)>(raw).ok())
                    {
                        let binding = self
                            .view
                            .native_command_history
                            .summaries
                            .iter()
                            .find(|summary| {
                                summary.header.binding.manager_generation == generation
                                    && summary.header.binding.command_id == command
                            })
                            .map(|summary| summary.header.binding.clone());
                        if let (Some(binding), Some(store)) = (binding, self.store.clone()) {
                            self.view.native_command_history.selected = Some(binding);
                            self.view.native_command_history.cursor = 0;
                            self.view.native_command_history.previous_cursors.clear();
                            self.request_native_command_history(store);
                        }
                    }
                }
            }
        }
        true
    }
}
