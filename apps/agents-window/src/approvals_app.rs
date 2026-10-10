//! A review surface is not a grant. Every decision resolves a current, scoped
//! request through the existing durable permission controller.
use super::*;
use crate::approval_ui::{self, ApprovalTarget};

impl AgentsWindow {
    pub(super) fn sync_approval_ui(&mut self) -> bool {
        if self
            .view
            .approval_ui
            .selected
            .as_ref()
            .is_some_and(|target| !approval_ui::target_is_pending(&self.view, target))
        {
            self.view.approval_ui = Default::default();
            return true;
        }
        false
    }

    pub(super) fn apply_approval_action(&mut self, action: &UiAction, ctx: &egui::Context) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if let Some(raw) = id.strip_prefix("approval_review:") {
            if raw.len() <= 16 * 1024 {
                if let Ok(target) = serde_json::from_str::<ApprovalTarget>(raw) {
                    if approval_ui::target_is_pending(&self.view, &target) {
                        self.view.approval_ui.selected = Some(target);
                        self.view.approval_ui.details_open = false;
                        ctx.request_repaint();
                    }
                }
            }
            return true;
        }
        if id == "approval_close" {
            self.view.approval_ui = Default::default();
            ctx.request_repaint();
            return true;
        }
        if id == "approval_details" {
            if self
                .view
                .approval_ui
                .selected
                .as_ref()
                .is_some_and(|target| approval_ui::target_is_pending(&self.view, target))
            {
                self.view.approval_ui.details_open = !self.view.approval_ui.details_open;
                ctx.request_repaint();
            }
            return true;
        }
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
        if self.view.approval_ui.selected.as_ref() != Some(&target)
            || !approval_ui::target_is_pending(&self.view, &target)
        {
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
        self.sync_approval_ui();
        ctx.request_repaint();
        true
    }
}

#[cfg(test)]
#[path = "approvals_app_tests.rs"]
mod tests;
