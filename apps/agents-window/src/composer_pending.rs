//! A next-turn selection is owned by the conversation which requested it.
//! It is presentation data, never authority for an already admitted turn.

use crate::{agent_tool_settings::AgentToolSettings, composer_selection::ModelRouteOption};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SELECTION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ComposerScope {
    pub workspace: String,
    pub session: Option<String>,
}

impl ComposerScope {
    pub(crate) fn from_view(view: &crate::view::ViewState) -> Self {
        Self {
            workspace: view.workspace.clone(),
            session: view.selected_session.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingComposerModel {
    pub route: ModelRouteOption,
    pub effort: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingComposerSelection {
    pub token: u64,
    pub scope: ComposerScope,
    pub model: Option<PendingComposerModel>,
    pub permissions: Option<AgentToolSettings>,
    /// Changing settings elsewhere invalidates permission consent, especially
    /// full access. A queued choice must never restore an obsolete authority.
    pub permission_base: Option<AgentToolSettings>,
    pub error: Option<String>,
}

impl PendingComposerSelection {
    pub(crate) fn new(scope: ComposerScope) -> Self {
        Self {
            token: NEXT_SELECTION.fetch_add(1, Ordering::Relaxed),
            scope,
            model: None,
            permissions: None,
            permission_base: None,
            error: None,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.model.is_none() && self.permissions.is_none()
    }

    pub(crate) fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(model) = &self.model {
            let effort = model
                .route
                .effort_options
                .iter()
                .find(|(id, _)| id == &model.effort)
                .map(|(_, label)| label.as_str())
                .unwrap_or("");
            parts.push(if effort.is_empty() {
                model.route.model_name.clone()
            } else {
                format!("{} · {effort}", model.route.model_name)
            });
        }
        if let Some(settings) = self.permissions {
            parts.push(
                crate::composer_permissions_ui::preset_for_permissions(
                    settings.permissions(),
                    settings.full_access(),
                )
                .label()
                .to_owned(),
            );
        }
        parts.join(" · ")
    }
}
