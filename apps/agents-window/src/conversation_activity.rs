//! Host observations for conversation markers. These are temporary UI state,
//! never checkpoint, provider catalog or tool-effect authority.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Controlled disclosures belong to one visible conversation, never the journal.
#[derive(Default)]
pub(crate) struct ConversationDetailsUi {
    session: Option<String>,
    expanded: BTreeMap<String, bool>,
    revision: u64,
    // An offset into one existing assistant row, never a copy of its source.
    source_view: Option<(usize, usize)>,
}

impl ConversationDetailsUi {
    pub fn sync(&mut self, session: Option<&str>) {
        if self.session.as_deref() != session {
            self.session = session.map(str::to_owned);
            self.expanded.clear();
            self.source_view = None;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn is_expanded(&self, id: &str, default: bool) -> bool {
        self.expanded.get(id).copied().unwrap_or(default)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn toggle(&mut self, id: &str, default: bool) {
        let next = !self.is_expanded(id, default);
        self.set_expanded(id, next);
    }

    pub fn set_expanded(&mut self, id: &str, expanded: bool) {
        if self.expanded.get(id).copied() == Some(expanded) {
            return;
        }
        // Keep this optional presentation state bounded in long histories.
        if self.expanded.len() >= 128 && !self.expanded.contains_key(id) {
            self.expanded.pop_first();
        }
        self.expanded.insert(id.to_owned(), expanded);
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn migrate(&mut self, previous: &str, current: &str) {
        if previous == current {
            return;
        }
        if let Some(expanded) = self.expanded.remove(previous) {
            self.expanded.entry(current.to_owned()).or_insert(expanded);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn source_view(&self) -> Option<(usize, usize)> {
        self.source_view
    }

    pub fn open_source(&mut self, item_index: usize) {
        self.source_view = Some((item_index, 0));
    }

    pub fn move_source(&mut self, item_index: usize, expected_start: usize, next_start: usize) {
        if self.source_view == Some((item_index, expected_start)) {
            self.source_view = Some((item_index, next_start));
        }
    }

    pub fn close_source(&mut self) {
        self.source_view = None;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BranchScope {
    workspace: PathBuf,
    owner: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BranchChange {
    scope: BranchScope,
    pub previous: String,
    pub current: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ConversationActivityUi {
    pub compacting_session: Option<String>,
    compacting_workspace: Option<PathBuf>,
    branch_baseline: Option<(BranchScope, String)>,
    branch_change: Option<BranchChange>,
}

impl ConversationActivityUi {
    pub fn begin_compaction(&mut self, session: &str, workspace: &Path) {
        self.compacting_session = Some(session.to_owned());
        self.compacting_workspace = workspace.canonicalize().ok();
    }

    pub fn end_compaction(&mut self) {
        self.compacting_session = None;
        self.compacting_workspace = None;
    }

    pub fn compacting_for(&self, workspace: &Path, owner: Option<&str>) -> Option<&str> {
        let workspace = workspace.canonicalize().ok()?;
        self.compacting_session.as_deref().filter(|session| {
            Some(*session) == owner && self.compacting_workspace.as_ref() == Some(&workspace)
        })
    }

    /// Call only with a successful fresh Git review for this workspace. The
    /// first observation establishes a baseline; it does not claim a checkout.
    pub fn observe_branch(&mut self, workspace: &Path, owner: Option<&str>, branch: &str) -> bool {
        let Ok(workspace) = workspace.canonicalize() else {
            let changed = self.branch_change.take().is_some();
            self.branch_baseline = None;
            return changed;
        };
        if branch.is_empty() || branch == "Rama desconocida" {
            return false;
        }
        let scope = BranchScope {
            workspace,
            owner: owner.map(str::to_owned),
        };
        let change = match self.branch_baseline.as_ref() {
            Some((previous_scope, previous)) if previous_scope == &scope && previous != branch => {
                Some(BranchChange {
                    scope: scope.clone(),
                    previous: previous.clone(),
                    current: branch.to_owned(),
                })
            }
            Some((previous_scope, previous)) if previous_scope == &scope && previous == branch => {
                return false
            }
            _ => None,
        };
        let changed = self.branch_change != change;
        self.branch_baseline = Some((scope, branch.to_owned()));
        self.branch_change = change;
        changed
    }

    pub fn branch_change(&self, workspace: &Path, owner: Option<&str>) -> Option<&BranchChange> {
        let workspace = workspace.canonicalize().ok()?;
        self.branch_change.as_ref().filter(|change| {
            change.scope.workspace == workspace && change.scope.owner.as_deref() == owner
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn confirmed_git_observations_detect_external_checkout_and_reopen_is_a_baseline() {
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .current_dir(root.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        git(&["init", "--initial-branch=before"]);
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "--allow-empty",
            "-m",
            "baseline",
        ]);
        let mut state = ConversationActivityUi::default();
        let initial = crate::git_review::load_git_review(root.path()).unwrap();
        assert!(!state.observe_branch(root.path(), Some("session"), &initial.branch));
        assert!(state.branch_change(root.path(), Some("session")).is_none());
        git(&["switch", "-c", "after"]);
        let fresh = crate::git_review::load_git_review(root.path()).unwrap();
        assert!(state.observe_branch(root.path(), Some("session"), &fresh.branch));
        let change = state.branch_change(root.path(), Some("session")).unwrap();
        assert_eq!(
            (change.previous.as_str(), change.current.as_str()),
            ("before", "after")
        );
        assert!(!state.observe_branch(root.path(), Some("session"), &fresh.branch));
        assert!(state
            .branch_change(root.path(), Some("other-session"))
            .is_none());
        let other = tempfile::tempdir().unwrap();
        assert!(state.branch_change(other.path(), Some("session")).is_none());
        assert!(state.observe_branch(root.path(), Some("other-session"), &fresh.branch));
        assert!(state
            .branch_change(root.path(), Some("other-session"))
            .is_none());
        let mut reopened = ConversationActivityUi::default();
        assert!(!reopened.observe_branch(root.path(), Some("session"), &fresh.branch));
        assert!(reopened
            .branch_change(root.path(), Some("session"))
            .is_none());
    }
}
