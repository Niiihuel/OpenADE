//! A decision comes straight from the request's card. Each one resolves only
//! a current, scoped request through the existing durable permission controller.
use super::*;
use crate::approval_ui::{self, ApprovalTarget};

impl AgentsWindow {
    pub(super) fn apply_approval_action(&mut self, action: &UiAction, ctx: &egui::Context) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        let Some(raw) = id.strip_prefix("approval_decide:") else {
            return false;
        };
        if raw.len() > 16 * 1024 {
            return true;
        }
        let Ok((owner_session_id, workspace, child_session_id, request_id, choice)) =
            serde_json::from_str::<(String, String, Option<String>, String, String)>(raw)
        else {
            return true;
        };
        let target = ApprovalTarget {
            owner_session_id,
            workspace,
            child_session_id,
            request_id,
        };
        if !approval_ui::target_is_pending(&self.view, &target) {
            return true;
        }
        let decision = match choice.as_str() {
            "allow_once" => PermissionChoice::Allow,
            "allow_session" => PermissionChoice::AllowSession,
            "deny" => PermissionChoice::Deny,
            _ => return true,
        };
        if let Some(child) = target.child_session_id.as_deref() {
            self.answer_subagent_permission(child, &target.request_id, decision);
        } else if decision == PermissionChoice::AllowSession {
            self.answer_permission_for_session(&target.request_id);
        } else {
            self.answer_permission(&target.request_id, decision == PermissionChoice::Allow);
        }
        ctx.request_repaint();
        true
    }
}

#[cfg(test)]
#[path = "approvals_app_tests.rs"]
mod tests;
