//! Lazy, bounded history projections. A diff never opens the user's dock.
use super::*;
use crate::{
    chat_diff_ui::{self, ChatDiffUiState, ToolDiffEntry},
    session_tool_diff::{SessionToolDiffRequest, SessionToolDiffWorker},
};

const MAX_CACHED_DIFFS: usize = 16;
pub(super) struct ChatDiffHost {
    worker: Option<SessionToolDiffWorker>,
    pending: Option<SessionToolDiffRequest>,
    waiting: VecDeque<String>,
    generation: u64,
    expansion_revision: Option<(u64, usize, u64, Option<String>)>,
}

#[cfg(test)]
#[path = "chat_diff_app_tests.rs"]
mod tests;
impl Default for ChatDiffHost {
    fn default() -> Self {
        Self {
            worker: SessionToolDiffWorker::spawn().ok(),
            pending: None,
            waiting: VecDeque::new(),
            generation: 0,
            expansion_revision: None,
        }
    }
}

impl AgentsWindow {
    fn sync_chat_diff_scope(&mut self) -> bool {
        let workspace = Path::new(&self.view.workspace);
        if self.view.chat_diffs.workspace.as_deref() != Some(workspace)
            || self.view.chat_diffs.session_id != self.view.selected_session
        {
            if let Some(owner) = &self.view.chat_diffs.session_id {
                for (tool, entry) in &self.view.chat_diffs.entries {
                    for file in 0..entry.files.len() {
                        self.tree
                            .forget_code_view(&chat_diff_ui::view_id(owner, tool, file));
                    }
                }
            }
            self.view.chat_diffs = ChatDiffUiState {
                workspace: Some(workspace.to_path_buf()),
                session_id: self.view.selected_session.clone(),
                ..Default::default()
            };
            self.chat_diffs.pending = None;
            self.chat_diffs.waiting.clear();
            self.chat_diffs.expansion_revision = None;
            self.view
                .conversation_details
                .sync(self.view.selected_session.as_deref());
            return true;
        }
        false
    }

    /// Reconcile the card's disclosure with bounded background work. This is
    /// presentation-only: no journal, filesystem or Git reads run on the UI thread.
    fn sync_expanded_chat_diffs(&mut self) -> bool {
        let revision = (
            self.last_sequence,
            self.view.transcript.len(),
            self.view.conversation_details.revision(),
            self.view.selected_tool_id.clone(),
        );
        if self.chat_diffs.expansion_revision.as_ref() == Some(&revision) {
            return false;
        }
        self.chat_diffs.expansion_revision = Some(revision);
        if self.view.selected_session.is_none() {
            return false;
        }
        let defaults = crate::file_activity_ui::default_expanded_calls(&self.view);
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        for item in &self.view.transcript {
            if let TranscriptItem::Tool { id, .. } = item {
                *counts.entry(id).or_default() += 1;
            }
        }
        let mut desired = Vec::new();
        let mut limit_notice = None;
        for item in self.view.transcript.iter().rev() {
            let TranscriptItem::Tool {
                id,
                name,
                output: Some(_),
                is_error: false,
                activity: crate::transcript::ActivityStatus::Completed,
                ..
            } = item
            else {
                continue;
            };
            if !matches!(name.as_str(), "write_file" | "apply_patch")
                || counts.get(id.as_str()) != Some(&1)
                // The Activity detail of the selected edit shows its diff too.
                || !(self.view.selected_tool_id.as_deref() == Some(id.as_str())
                    || self
                        .view
                        .conversation_details
                        .is_expanded(&format!("tool:{id}"), defaults.contains(&id.as_str())))
            {
                continue;
            }
            if desired.len() < MAX_CACHED_DIFFS {
                desired.push(id.clone());
            } else if limit_notice.is_none() {
                limit_notice = Some(id.clone());
            }
        }
        let open = desired.iter().cloned().collect();
        let mut changed = self.view.chat_diffs.open != open;
        self.view.chat_diffs.open = open;
        self.chat_diffs
            .waiting
            .retain(|id| self.view.chat_diffs.open.contains(id));
        let pending = self
            .chat_diffs
            .pending
            .as_ref()
            .map(|r| r.tool_call_id.as_str());
        self.view
            .chat_diffs
            .loading
            .retain(|id| pending == Some(id.as_str()) || self.view.chat_diffs.open.contains(id));
        for tool in desired {
            if !self.view.chat_diffs.entries.contains_key(&tool)
                && !self.view.chat_diffs.loading.contains(&tool)
            {
                if self.queue_chat_diff(&tool) {
                    changed = true;
                } else {
                    limit_notice.get_or_insert(tool);
                }
            }
        }
        changed |= self.view.chat_diffs.limit_notice != limit_notice;
        self.view.chat_diffs.limit_notice = limit_notice;
        changed
    }

    fn set_chat_diff_disclosure(&mut self, tool: &str, expanded: bool) {
        let id = format!("tool:{tool}");
        self.view.conversation_details.set_expanded(&id, expanded);
    }
    /// A manual save changes provenance for every chat baseline in this
    /// workspace. Discard scoped cached/in-flight projections, preserving the
    /// user's open rows and loading only their confirmed edits again.
    pub(super) fn invalidate_chat_diffs_after_editor_save(&mut self, workspace: &Path) {
        if workspace != Path::new(&self.view.workspace) {
            return;
        }
        self.sync_chat_diff_scope();
        if let Some(owner) = &self.view.chat_diffs.session_id {
            for (tool, entry) in &self.view.chat_diffs.entries {
                for file in 0..entry.files.len() {
                    self.tree
                        .forget_code_view(&chat_diff_ui::view_id(owner, tool, file));
                }
            }
        }
        self.view.chat_diffs.entries.clear();
        self.view.chat_diffs.loading.clear();
        self.chat_diffs.pending = None;
        self.chat_diffs.waiting.clear();
        self.chat_diffs.expansion_revision = None;
        self.chat_diffs.generation = self.chat_diffs.generation.saturating_add(1);
        let opened: Vec<_> = self.view.chat_diffs.open.iter().cloned().collect();
        for tool in opened {
            let confirmed = self
                .view
                .transcript
                .iter()
                .filter(|item| {
                    matches!(item,
                TranscriptItem::Tool { id, name, output: Some(_), is_error: false,
                    activity: crate::transcript::ActivityStatus::Completed, .. }
                if id == &tool && matches!(name.as_str(), "write_file" | "apply_patch"))
                })
                .count()
                == 1;
            if confirmed {
                self.queue_chat_diff(&tool);
            }
        }
    }

    fn queue_chat_diff(&mut self, tool: &str) -> bool {
        if self.view.chat_diffs.loading.contains(tool) {
            return true;
        }
        if self.chat_diffs.waiting.len() + usize::from(self.chat_diffs.pending.is_some())
            >= MAX_CACHED_DIFFS
        {
            return false;
        }
        self.view.chat_diffs.loading.insert(tool.to_owned());
        self.chat_diffs.waiting.push_back(tool.to_owned());
        self.pump_chat_diff();
        true
    }
    fn pump_chat_diff(&mut self) -> bool {
        if self.chat_diffs.pending.is_some() {
            return false;
        }
        let Some(tool_call_id) = self.chat_diffs.waiting.pop_front() else {
            return false;
        };
        let Some(session_id) = self.view.selected_session.clone() else {
            return self.view.chat_diffs.loading.remove(&tool_call_id);
        };
        self.chat_diffs.generation = self.chat_diffs.generation.saturating_add(1);
        let result = self
            .store
            .as_ref()
            .ok_or_else(|| "El registro del chat no está disponible".to_owned())
            .and_then(|store| {
                let request = SessionToolDiffRequest {
                    storage_root: store.storage_root().to_path_buf(),
                    workspace: PathBuf::from(&self.view.workspace),
                    session_id,
                    tool_call_id: tool_call_id.clone(),
                    generation: self.chat_diffs.generation,
                };
                self.chat_diffs
                    .worker
                    .as_ref()
                    .ok_or_else(|| "No se pudo iniciar el lector de diffs".to_owned())
                    .and_then(|worker| worker.submit(request.clone()).map_err(|e| e.to_string()))?;
                Ok(request)
            });
        match result {
            Ok(request) => {
                self.chat_diffs.pending = Some(request);
                false
            }
            Err(error) => {
                self.view.chat_diffs.loading.remove(&tool_call_id);
                self.cache_chat_diff(tool_call_id, ToolDiffEntry::from_result(Err(error)));
                true
            }
        }
    }
    fn cache_chat_diff(&mut self, tool: String, entry: ToolDiffEntry) {
        if !self.view.chat_diffs.entries.contains_key(&tool)
            && self.view.chat_diffs.entries.len() >= MAX_CACHED_DIFFS
        {
            let remove = self
                .view
                .chat_diffs
                .entries
                .keys()
                .find(|id| !self.view.chat_diffs.open.contains(*id))
                .cloned();
            if let Some(id) = remove {
                if let (Some(old), Some(owner)) = (
                    self.view.chat_diffs.entries.remove(&id),
                    &self.view.chat_diffs.session_id,
                ) {
                    for file in 0..old.files.len() {
                        self.tree
                            .forget_code_view(&chat_diff_ui::view_id(owner, &id, file));
                    }
                }
            } else {
                return;
            }
        }
        self.view.chat_diffs.entries.insert(tool, entry);
    }
    pub(super) fn poll_chat_diffs(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = self.sync_chat_diff_scope();
        let response = self.chat_diffs.pending.as_ref().and_then(|r| {
            self.chat_diffs.worker.as_ref()?.poll_latest(
                r.generation,
                &r.session_id,
                &r.workspace,
                &r.tool_call_id,
            )
        });
        changed |= response.is_some();
        if let Some(response) = response {
            // A completed read frees queue capacity for an expanded card that
            // could not be admitted earlier. Idle hover frames skip the scan.
            self.chat_diffs.expansion_revision = None;
            self.chat_diffs.pending = None;
            self.view.chat_diffs.loading.remove(&response.tool_call_id);
            self.cache_chat_diff(
                response.tool_call_id,
                ToolDiffEntry::from_result(response.result),
            );
        }
        changed |= self.sync_expanded_chat_diffs();
        changed |= self.pump_chat_diff();
        if self.chat_diffs.pending.is_some() || !self.chat_diffs.waiting.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(25));
        }
        changed
    }
    pub(super) fn apply_chat_diff_action(&mut self, action: &UiAction) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        let retry = id.starts_with("chat_diff_retry:");
        if !retry && !id.starts_with("chat_diff_toggle:") {
            return false;
        }
        self.sync_chat_diff_scope();
        let Some(owner) = self.view.selected_session.clone() else {
            return true;
        };
        let prefix = format!(
            "chat_diff_{}:{owner}:",
            if retry { "retry" } else { "toggle" }
        );
        let Some(tool) = id.strip_prefix(&prefix) else {
            return true;
        };
        let confirmed = self.view.transcript.iter().filter(|item| matches!(item, TranscriptItem::Tool { id, name, output: Some(_), is_error: false, activity: crate::transcript::ActivityStatus::Completed, .. } if id == tool && matches!(name.as_str(), "write_file" | "apply_patch"))).count() == 1;
        if !confirmed {
            return true;
        }
        if !retry && self.view.chat_diffs.open.remove(tool) {
            self.set_chat_diff_disclosure(tool, false);
            self.chat_diffs.waiting.retain(|id| id != tool);
            if self
                .chat_diffs
                .pending
                .as_ref()
                .is_none_or(|request| request.tool_call_id != tool)
            {
                self.view.chat_diffs.loading.remove(tool);
            }
            self.view.chat_diffs.limit_notice = None;
            return true;
        }
        if !self.view.chat_diffs.open.contains(tool)
            && self.view.chat_diffs.open.len() >= MAX_CACHED_DIFFS
        {
            self.view.chat_diffs.limit_notice = Some(tool.to_owned());
            self.set_chat_diff_disclosure(tool, false);
            return true;
        }
        self.view.chat_diffs.open.insert(tool.to_owned());
        self.set_chat_diff_disclosure(tool, true);
        self.view.chat_diffs.limit_notice = None;
        if retry {
            if let Some(old) = self.view.chat_diffs.entries.remove(tool) {
                for file in 0..old.files.len() {
                    self.tree
                        .forget_code_view(&chat_diff_ui::view_id(&owner, tool, file));
                }
            }
        }
        if !self.view.chat_diffs.entries.contains_key(tool) && !self.queue_chat_diff(tool) {
            self.view.chat_diffs.open.remove(tool);
            self.set_chat_diff_disclosure(tool, false);
            self.view.chat_diffs.limit_notice = Some(tool.to_owned());
        }
        true
    }
}
