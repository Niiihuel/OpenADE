//! Browser-style navigation between the main destinations of one Agents Window.
//! The app owns effects (loading sessions, files, and settings); this module only
//! records where the user went and which destination back/forward should restore.

use std::path::{Path, PathBuf};

const DEFAULT_HISTORY_LIMIT: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NavigationTarget {
    Task {
        workspace: PathBuf,
        session_id: String,
    },
    Draft {
        workspace: PathBuf,
    },
    Settings {
        workspace: PathBuf,
    },
    Automations {
        workspace: PathBuf,
    },
    Marketplace {
        workspace: PathBuf,
    },
}

impl NavigationTarget {
    pub fn workspace(&self) -> &Path {
        match self {
            Self::Task { workspace, .. }
            | Self::Draft { workspace }
            | Self::Settings { workspace }
            | Self::Automations { workspace }
            | Self::Marketplace { workspace } => workspace,
        }
    }
}

/// An in-memory history. `visit` records an explicit user navigation; restoring
/// a target returned by `back` or `forward` must not call `visit` again.
pub struct NavigationHistory {
    entries: Vec<NavigationTarget>,
    cursor: Option<usize>,
    limit: usize,
}

impl Default for NavigationHistory {
    fn default() -> Self {
        Self::with_limit(DEFAULT_HISTORY_LIMIT)
    }
}

impl NavigationHistory {
    pub fn with_limit(limit: usize) -> Self {
        Self {
            entries: Vec::new(),
            cursor: None,
            limit: limit.max(1),
        }
    }

    pub fn current(&self) -> Option<&NavigationTarget> {
        self.cursor.and_then(|index| self.entries.get(index))
    }

    /// Returns `true` when a new destination was recorded. Repeated navigation
    /// to the current destination is ignored, including after going back.
    pub fn visit(&mut self, target: NavigationTarget) -> bool {
        if self.current() == Some(&target) {
            return false;
        }
        let next = self.cursor.map_or(0, |index| index + 1);
        self.entries.truncate(next);
        self.entries.push(target);
        if self.entries.len() > self.limit {
            self.entries.remove(0);
        }
        self.cursor = Some(self.entries.len() - 1);
        true
    }

    pub fn can_back(&self) -> bool {
        self.cursor.is_some_and(|index| index > 0)
    }

    pub fn can_forward(&self) -> bool {
        self.cursor
            .is_some_and(|index| index + 1 < self.entries.len())
    }

    pub fn back(&mut self) -> Option<&NavigationTarget> {
        let index = self.cursor?.checked_sub(1)?;
        self.cursor = Some(index);
        self.current()
    }

    pub fn forward(&mut self) -> Option<&NavigationTarget> {
        let index = self.cursor?;
        if index + 1 >= self.entries.len() {
            return None;
        }
        self.cursor = Some(index + 1);
        self.current()
    }

    /// Drop routes for a deleted task. Other workspaces can reuse the same ID.
    pub fn forget_task(&mut self, workspace: &Path, session_id: &str) -> bool {
        let original_len = self.entries.len();
        let current_index = self.cursor;
        let mut retained_before_current = 0;
        let mut current_retained = false;
        let mut next = Vec::with_capacity(original_len);
        for (index, target) in self.entries.drain(..).enumerate() {
            let belongs_to_deleted_task = target.workspace() == workspace
                && match &target {
                    NavigationTarget::Task {
                        session_id: target_id,
                        ..
                    } => target_id == session_id,
                    NavigationTarget::Draft { .. }
                    | NavigationTarget::Settings { .. }
                    | NavigationTarget::Automations { .. }
                    | NavigationTarget::Marketplace { .. } => false,
                };
            if !belongs_to_deleted_task {
                if current_index.is_some_and(|current| index <= current) {
                    retained_before_current += 1;
                }
                if current_index == Some(index) {
                    current_retained = true;
                }
                next.push(target);
            }
        }
        let changed = next.len() != original_len;
        self.entries = next;
        self.cursor = if self.entries.is_empty() {
            None
        } else if current_retained {
            Some(retained_before_current - 1)
        } else {
            current_index.map(|index| index.min(self.entries.len() - 1))
        };
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(workspace: &str) -> NavigationTarget {
        NavigationTarget::Draft {
            workspace: workspace.into(),
        }
    }

    fn task(workspace: &str, id: &str) -> NavigationTarget {
        NavigationTarget::Task {
            workspace: workspace.into(),
            session_id: id.to_owned(),
        }
    }

    #[test]
    fn back_forward_and_duplicate_visit() {
        let mut history = NavigationHistory::default();
        assert!(history.back().is_none());
        assert!(history.forward().is_none());
        assert!(history.visit(draft("repo")));
        assert!(!history.visit(draft("repo")));
        assert!(history.visit(task("repo", "one")));
        assert!(history.can_back());
        assert_eq!(history.back(), Some(&draft("repo")));
        assert!(!history.visit(draft("repo")));
        assert!(history.can_forward());
        assert_eq!(history.forward(), Some(&task("repo", "one")));
        assert!(!history.can_forward());
    }

    #[test]
    fn branching_discards_forward_history() {
        let mut history = NavigationHistory::default();
        history.visit(draft("repo"));
        history.visit(task("repo", "one"));
        history.visit(task("repo", "two"));
        history.back();
        history.visit(task("other", "three"));
        assert!(!history.can_forward());
        assert_eq!(history.back(), Some(&task("repo", "one")));
        assert_eq!(history.forward(), Some(&task("other", "three")));
    }

    #[test]
    fn settings_restore_conversation_context() {
        let mut history = NavigationHistory::default();
        history.visit(task("repo", "one"));
        history.visit(NavigationTarget::Settings {
            workspace: "repo".into(),
        });
        assert_eq!(history.back().unwrap().workspace(), Path::new("repo"));
        assert_eq!(history.current(), Some(&task("repo", "one")));
    }

    #[test]
    fn bounded_history_keeps_current_and_back_neighbour() {
        let mut history = NavigationHistory::with_limit(2);
        history.visit(draft("repo"));
        history.visit(task("repo", "one"));
        history.visit(task("repo", "two"));
        assert_eq!(history.current(), Some(&task("repo", "two")));
        assert_eq!(history.back(), Some(&task("repo", "one")));
        assert!(!history.can_back());
        assert_eq!(history.forward(), Some(&task("repo", "two")));
    }

    #[test]
    fn zero_limit_still_retains_current_destination() {
        let mut history = NavigationHistory::with_limit(0);
        history.visit(draft("repo"));
        history.visit(task("repo", "one"));
        assert_eq!(history.current(), Some(&task("repo", "one")));
        assert!(!history.can_back());
    }

    #[test]
    fn deleted_task_routes_are_removed_without_touching_other_workspaces() {
        let mut history = NavigationHistory::default();
        history.visit(draft("repo"));
        history.visit(task("repo", "one"));
        history.visit(task("other", "one"));
        assert!(history.forget_task(Path::new("repo"), "one"));
        assert_eq!(history.current(), Some(&task("other", "one")));
        assert_eq!(history.back(), Some(&draft("repo")));
        assert!(!history.forget_task(Path::new("repo"), "one"));
    }

    #[test]
    fn deleting_current_route_moves_cursor_to_nearest_remaining_route() {
        let mut history = NavigationHistory::default();
        history.visit(draft("repo"));
        history.visit(task("repo", "one"));
        history.visit(task("repo", "two"));
        history.back();
        assert!(history.forget_task(Path::new("repo"), "one"));
        assert_eq!(history.current(), Some(&task("repo", "two")));
        assert_eq!(history.back(), Some(&draft("repo")));
    }
}
