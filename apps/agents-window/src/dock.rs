//! Pure state for the right dock. Rendering and external resources (files, PTYs,
//! browsers) belong to the app layer; this module only owns tab identity, scope,
//! ordering, selection, and per-workspace layout preferences.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Component, Path, PathBuf},
};

const DEFAULT_WIDTH_RATIO: f32 = 0.45;
const MAX_WIDTH_RATIO: f32 = 0.65;
const MIN_WIDTH_PX: f32 = 320.0;
const MAX_RECENT_PER_OWNER: usize = 8;

/// A tab can belong to the whole workspace, a draft, or one task.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum DockOwner {
    Workspace,
    Draft,
    Task(String),
}

/// The caller supplies a stable workspace ID (identity or absolute path).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DockScope {
    pub workspace_id: String,
    pub owner: DockOwner,
}

impl DockScope {
    pub fn new(workspace_id: impl Into<String>, owner: DockOwner) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            owner,
        }
    }

    /// Convenience constructor for a task ID. An empty ID denotes the draft.
    pub fn for_owner(workspace: &Path, owner: &str) -> Self {
        let owner = if owner.is_empty() {
            DockOwner::Draft
        } else {
            DockOwner::Task(owner.to_owned())
        };
        Self::new(workspace.to_string_lossy(), owner)
    }
}

/// File preview paths are relative to the owning workspace.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum DockTabKind {
    Context,
    Files,
    Activity,
    CodeReview,
    Terminal,
    FilePreview { path: PathBuf },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DockTabId(u64);

impl DockTabId {
    pub fn from_raw(raw: u64) -> Option<Self> {
        (raw != 0).then_some(Self(raw))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DockTab {
    pub id: DockTabId,
    pub workspace_id: String,
    pub owner: DockOwner,
    pub kind: DockTabKind,
}

impl DockTab {
    pub fn title(&self) -> String {
        match &self.kind {
            DockTabKind::Context => "Contexto".to_owned(),
            DockTabKind::Files => "Archivos".to_owned(),
            DockTabKind::Activity => "Actividad".to_owned(),
            DockTabKind::CodeReview => "Cambios".to_owned(),
            DockTabKind::Terminal => "Terminal".to_owned(),
            DockTabKind::FilePreview { path } => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
        }
    }
}

/// A closed tab kept for the overview. Resource-backed terminal sessions are
/// listed for context but cannot be restored after the host releases the PTY.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentlyClosedTab {
    pub tab: DockTab,
    pub restorable: bool,
}

#[derive(Default)]
struct WorkspaceDock {
    tabs: Vec<DockTab>,
    recently_closed: VecDeque<RecentlyClosedTab>,
    active_by_owner: HashMap<DockOwner, DockTabId>,
    collapsed_by_owner: HashMap<DockOwner, bool>,
    preferred_width_px: Option<f32>,
}

/// In-memory registry. Keep one instance in the application while switching
/// workspaces or tasks. Dropping it intentionally discards these preferences.
#[derive(Default)]
pub struct DockState {
    workspaces: HashMap<String, WorkspaceDock>,
    next_tab_id: u64,
}

impl DockState {
    /// Open a tab, or activate the existing tab of the same kind and owner.
    /// Opening also expands the dock for that owner.
    pub fn open(&mut self, scope: &DockScope, kind: DockTabKind) -> DockTabId {
        let workspace = self
            .workspaces
            .entry(scope.workspace_id.clone())
            .or_default();
        // An explicit open supersedes an older closed entry for this same
        // owner and kind, even if an existing tab is merely reactivated.
        workspace
            .recently_closed
            .retain(|entry| entry.tab.owner != scope.owner || entry.tab.kind != kind);
        if let Some(tab) = workspace
            .tabs
            .iter()
            .find(|tab| tab.owner == scope.owner && tab.kind == kind)
        {
            workspace
                .active_by_owner
                .insert(scope.owner.clone(), tab.id);
            workspace
                .collapsed_by_owner
                .insert(scope.owner.clone(), false);
            return tab.id;
        }

        self.next_tab_id = self
            .next_tab_id
            .checked_add(1)
            .expect("dock tab IDs exhausted");
        let id = DockTabId(self.next_tab_id);
        workspace.tabs.push(DockTab {
            id,
            workspace_id: scope.workspace_id.clone(),
            owner: scope.owner.clone(),
            kind,
        });
        workspace.active_by_owner.insert(scope.owner.clone(), id);
        workspace
            .collapsed_by_owner
            .insert(scope.owner.clone(), false);
        id
    }

    /// Open a file under `workspace`. Returns `None` for an empty path or a
    /// path containing an absolute/root/prefix/parent component. This is only
    /// a lexical check; the file service must still enforce its own scope.
    pub fn open_file(
        &mut self,
        workspace: &Path,
        owner: &str,
        relative: &Path,
    ) -> Option<DockTabId> {
        if workspace.as_os_str().is_empty()
            || !relative
                .components()
                .any(|component| matches!(component, Component::Normal(_)))
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return None;
        }
        let scope = DockScope::for_owner(workspace, owner);
        Some(self.open(
            &scope,
            DockTabKind::FilePreview {
                path: relative.to_path_buf(),
            },
        ))
    }

    /// When a draft becomes its first task, transfer its tabs and preferences
    /// without changing their IDs. If the task already has a tab of the same
    /// kind, the draft tab wins; the existing task duplicate is removed.
    /// Returns the number of draft tabs transferred.
    pub fn promote_draft(&mut self, workspace_id: &str, task_id: &str) -> usize {
        if task_id.is_empty() {
            return 0;
        }
        let Some(workspace) = self.workspaces.get_mut(workspace_id) else {
            return 0;
        };
        let draft_kinds: Vec<DockTabKind> = workspace
            .tabs
            .iter()
            .filter(|tab| tab.owner == DockOwner::Draft)
            .map(|tab| tab.kind.clone())
            .collect();

        let target = DockOwner::Task(task_id.to_owned());
        let duplicate_ids: HashSet<DockTabId> = workspace
            .tabs
            .iter()
            .filter(|tab| tab.owner == target && draft_kinds.contains(&tab.kind))
            .map(|tab| tab.id)
            .collect();
        workspace
            .tabs
            .retain(|tab| !duplicate_ids.contains(&tab.id));
        for tab in &mut workspace.tabs {
            if tab.owner == DockOwner::Draft {
                tab.owner = target.clone();
            }
        }

        let draft_active = workspace.active_by_owner.remove(&DockOwner::Draft);
        let target_active = workspace.active_by_owner.get(&target).copied();
        let next_active = draft_active
            .filter(|id| workspace.tabs.iter().any(|tab| tab.id == *id))
            .or_else(|| target_active.filter(|id| !duplicate_ids.contains(id)));
        if let Some(id) = next_active {
            workspace.active_by_owner.insert(target.clone(), id);
        } else {
            workspace.active_by_owner.remove(&target);
        }
        if let Some(collapsed) = workspace.collapsed_by_owner.remove(&DockOwner::Draft) {
            workspace.collapsed_by_owner.insert(target, collapsed);
        }
        // Draft history follows the task just like its open tabs. Keep the
        // newest entry for each kind and discard entries now open in the task.
        let target = DockOwner::Task(task_id.to_owned());
        for entry in &mut workspace.recently_closed {
            if entry.tab.owner == DockOwner::Draft {
                entry.tab.owner = target.clone();
            }
        }
        let mut seen = HashSet::new();
        workspace.recently_closed.retain(|entry| {
            if entry.tab.owner != target {
                return true;
            }
            !workspace
                .tabs
                .iter()
                .any(|tab| tab.owner == target && tab.kind == entry.tab.kind)
                && seen.insert(entry.tab.kind.clone())
        });
        trim_recent_owner(workspace, &target);
        draft_kinds.len()
    }

    /// All tabs in this workspace, including tabs hidden from the current task.
    pub fn tabs(&self, workspace_id: &str) -> &[DockTab] {
        self.workspaces
            .get(workspace_id)
            .map_or(&[], |workspace| workspace.tabs.as_slice())
    }

    pub fn visible_tabs(&self, scope: &DockScope) -> Vec<&DockTab> {
        self.tabs(&scope.workspace_id)
            .iter()
            .filter(|tab| visible_to(tab, &scope.owner))
            .collect()
    }

    /// Newest first. Task scopes can inspect their own entries and shared
    /// workspace entries, never those belonging to a different task.
    pub fn recently_closed(&self, scope: &DockScope) -> Vec<&RecentlyClosedTab> {
        self.workspaces
            .get(&scope.workspace_id)
            .map(|workspace| {
                workspace
                    .recently_closed
                    .iter()
                    .filter(|entry| visible_to(&entry.tab, &scope.owner))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Restore one visible entry as a fresh tab identity and activate it in
    /// the requesting scope. Terminal entries remain unavailable because a
    /// closed tab's PTY has already been released by the host.
    pub fn reopen_recent(&mut self, scope: &DockScope, closed_id: DockTabId) -> Option<DockTabId> {
        let workspace = self.workspaces.get_mut(&scope.workspace_id)?;
        let index = workspace.recently_closed.iter().position(|entry| {
            entry.tab.id == closed_id && entry.restorable && visible_to(&entry.tab, &scope.owner)
        })?;
        let entry = workspace.recently_closed.remove(index)?;
        let original_scope = DockScope::new(scope.workspace_id.clone(), entry.tab.owner);
        let id = self.open(&original_scope, entry.tab.kind);
        self.activate(scope, id);
        Some(id)
    }

    /// The owner keeps a separate active tab. If that tab was closed, the last
    /// visible tab becomes active without changing other owners' selections.
    pub fn active_tab(&self, scope: &DockScope) -> Option<&DockTab> {
        let workspace = self.workspaces.get(&scope.workspace_id)?;
        let active = workspace.active_by_owner.get(&scope.owner);
        workspace
            .tabs
            .iter()
            .find(|tab| Some(&tab.id) == active && visible_to(tab, &scope.owner))
            .or_else(|| {
                workspace
                    .tabs
                    .iter()
                    .rev()
                    .find(|tab| visible_to(tab, &scope.owner))
            })
    }

    /// Returns false if the tab belongs to another workspace or task.
    pub fn activate(&mut self, scope: &DockScope, id: DockTabId) -> bool {
        let Some(workspace) = self.workspaces.get_mut(&scope.workspace_id) else {
            return false;
        };
        if !workspace
            .tabs
            .iter()
            .any(|tab| tab.id == id && visible_to(tab, &scope.owner))
        {
            return false;
        }
        workspace.active_by_owner.insert(scope.owner.clone(), id);
        workspace
            .collapsed_by_owner
            .insert(scope.owner.clone(), false);
        true
    }

    /// Close one visible tab and choose its next neighbour for owners that had
    /// it selected. The removed tab is returned so the app can release resources.
    pub fn close(&mut self, scope: &DockScope, id: DockTabId) -> Option<DockTab> {
        let workspace = self.workspaces.get_mut(&scope.workspace_id)?;
        let index = workspace
            .tabs
            .iter()
            .position(|tab| tab.id == id && visible_to(tab, &scope.owner))?;
        let removed = workspace.tabs.remove(index);
        repair_active_after_close(workspace, id, index);
        workspace
            .recently_closed
            .retain(|entry| entry.tab.owner != removed.owner || entry.tab.kind != removed.kind);
        workspace.recently_closed.push_front(RecentlyClosedTab {
            restorable: !matches!(removed.kind, DockTabKind::Terminal),
            tab: removed.clone(),
        });
        trim_recent_owner(workspace, &removed.owner);
        Some(removed)
    }

    /// Close every visible tab except `keep`. Hidden tabs from other tasks
    /// remain in the registry. Returns removed tabs for resource cleanup.
    pub fn close_others(&mut self, scope: &DockScope, keep: DockTabId) -> Vec<DockTab> {
        if !self.activate(scope, keep) {
            return Vec::new();
        }
        let ids: Vec<DockTabId> = self
            .visible_tabs(scope)
            .into_iter()
            .filter(|tab| tab.id != keep)
            .map(|tab| tab.id)
            .collect();
        ids.into_iter()
            .filter_map(|id| self.close(scope, id))
            .collect()
    }

    /// Close all tabs visible to this owner, preserving tabs of other tasks.
    pub fn close_all_visible(&mut self, scope: &DockScope) -> Vec<DockTab> {
        let ids: Vec<DockTabId> = self
            .visible_tabs(scope)
            .into_iter()
            .map(|tab| tab.id)
            .collect();
        ids.into_iter()
            .filter_map(|id| self.close(scope, id))
            .collect()
    }

    /// Move a visible tab over another visible tab in the shared workspace order.
    pub fn reorder(&mut self, scope: &DockScope, id: DockTabId, over: DockTabId) -> bool {
        let Some(workspace) = self.workspaces.get_mut(&scope.workspace_id) else {
            return false;
        };
        let index = workspace
            .tabs
            .iter()
            .position(|tab| tab.id == id && visible_to(tab, &scope.owner));
        let over_index = workspace
            .tabs
            .iter()
            .position(|tab| tab.id == over && visible_to(tab, &scope.owner));
        let (Some(index), Some(over_index)) = (index, over_index) else {
            return false;
        };
        if index == over_index {
            return false;
        }
        let tab = workspace.tabs.remove(index);
        workspace.tabs.insert(over_index, tab);
        true
    }

    pub fn set_collapsed(&mut self, scope: &DockScope, collapsed: bool) {
        self.workspaces
            .entry(scope.workspace_id.clone())
            .or_default()
            .collapsed_by_owner
            .insert(scope.owner.clone(), collapsed);
    }

    /// An empty scope can remain open to show the tab launcher. Closing the
    /// final tab must not silently change the user's dock visibility choice.
    pub fn is_collapsed(&self, scope: &DockScope) -> bool {
        self.workspaces
            .get(&scope.workspace_id)
            .and_then(|workspace| workspace.collapsed_by_owner.get(&scope.owner))
            .copied()
            .unwrap_or(false)
    }

    /// Save a user-resized width in logical pixels. The rendered width is
    /// clamped to the current viewport by `width_px`.
    pub fn set_width_px(&mut self, workspace_id: &str, width_px: f32) {
        if width_px.is_finite() && width_px > 0.0 {
            self.workspaces
                .entry(workspace_id.to_owned())
                .or_default()
                .preferred_width_px = Some(width_px);
        }
    }

    /// Width is 45% initially, at least 320 px when space allows, and at most
    /// 65% of the available horizontal space. A narrow viewport cannot exceed
    /// its available width. The app still decides when to hide the whole dock.
    pub fn width_px(&self, workspace_id: &str, available_width_px: f32) -> f32 {
        let available = available_width_px.max(0.0);
        if !available.is_finite() {
            return 0.0;
        }
        let min = MIN_WIDTH_PX.min(available);
        let max = (available * MAX_WIDTH_RATIO).max(min);
        let preferred = self
            .workspaces
            .get(workspace_id)
            .and_then(|workspace| workspace.preferred_width_px)
            .unwrap_or(available * DEFAULT_WIDTH_RATIO);
        preferred.clamp(min, max)
    }
}

fn visible_to(tab: &DockTab, owner: &DockOwner) -> bool {
    tab.owner == DockOwner::Workspace || &tab.owner == owner
}

fn trim_recent_owner(workspace: &mut WorkspaceDock, owner: &DockOwner) {
    let mut count = 0;
    workspace.recently_closed.retain(|entry| {
        if &entry.tab.owner != owner {
            return true;
        }
        count += 1;
        count <= MAX_RECENT_PER_OWNER
    });
}

fn repair_active_after_close(workspace: &mut WorkspaceDock, removed_id: DockTabId, index: usize) {
    for (owner, active_id) in &mut workspace.active_by_owner {
        if *active_id != removed_id {
            continue;
        }
        let next = workspace.tabs[index.min(workspace.tabs.len())..]
            .iter()
            .find(|tab| visible_to(tab, owner))
            .or_else(|| {
                workspace.tabs[..index.min(workspace.tabs.len())]
                    .iter()
                    .rev()
                    .find(|tab| visible_to(tab, owner))
            });
        if let Some(tab) = next {
            *active_id = tab.id;
        } else {
            *active_id = DockTabId(0);
        }
    }
    workspace.active_by_owner.retain(|_, id| id.get() != 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(workspace: &str, id: &str) -> DockScope {
        DockScope::new(workspace, DockOwner::Task(id.to_owned()))
    }

    #[test]
    fn task_switch_keeps_tabs_and_per_owner_selection() {
        let mut dock = DockState::default();
        let alice = task("repo", "alice");
        let bob = task("repo", "bob");
        let shared = DockScope::new("repo", DockOwner::Workspace);
        let git_like_context = dock.open(&shared, DockTabKind::Context);
        let alice_activity = dock.open(&alice, DockTabKind::Activity);
        let bob_activity = dock.open(&bob, DockTabKind::Activity);

        assert_eq!(dock.visible_tabs(&alice).len(), 2);
        assert_eq!(dock.visible_tabs(&bob).len(), 2);
        assert_eq!(
            dock.active_tab(&alice).map(|tab| tab.id),
            Some(alice_activity)
        );
        assert_eq!(dock.active_tab(&bob).map(|tab| tab.id), Some(bob_activity));
        assert!(dock.activate(&alice, git_like_context));
        assert_eq!(
            dock.active_tab(&alice).map(|tab| tab.id),
            Some(git_like_context)
        );
        assert_eq!(dock.active_tab(&bob).map(|tab| tab.id), Some(bob_activity));
    }

    #[test]
    fn open_reuses_only_same_kind_workspace_and_owner() {
        let mut dock = DockState::default();
        let alice = task("repo", "alice");
        let bob = task("repo", "bob");
        let file = DockTabKind::FilePreview {
            path: PathBuf::from("src/main.rs"),
        };
        let first = dock.open(&alice, file.clone());
        dock.set_collapsed(&alice, true);
        assert_eq!(dock.open(&alice, file.clone()), first);
        assert!(!dock.is_collapsed(&alice));
        assert_ne!(dock.open(&bob, file.clone()), first);
        assert_ne!(dock.open(&task("other", "alice"), file), first);
        assert_eq!(dock.tabs("repo").len(), 2);
    }

    #[test]
    fn close_active_selects_neighbour_and_rejects_foreign_scope() {
        let mut dock = DockState::default();
        let alice = task("repo", "alice");
        let bob = task("repo", "bob");
        let first = dock.open(&alice, DockTabKind::Context);
        let middle = dock.open(&alice, DockTabKind::Activity);
        let last = dock.open(
            &alice,
            DockTabKind::FilePreview {
                path: PathBuf::from("notes.md"),
            },
        );
        assert!(dock.activate(&alice, middle));
        assert!(dock.close(&bob, middle).is_none());
        assert!(dock.close(&task("other", "alice"), middle).is_none());
        assert_eq!(dock.close(&alice, middle).map(|tab| tab.id), Some(middle));
        assert_eq!(dock.active_tab(&alice).map(|tab| tab.id), Some(last));
        dock.close(&alice, last);
        assert_eq!(dock.active_tab(&alice).map(|tab| tab.id), Some(first));
        dock.close(&alice, first);
        assert!(dock.active_tab(&alice).is_none());
        assert!(!dock.is_collapsed(&alice));
    }

    #[test]
    fn width_and_collapse_preferences_survive_task_switch() {
        let mut dock = DockState::default();
        let alice = task("repo", "alice");
        let bob = task("repo", "bob");
        let other = task("other", "alice");
        dock.open(&alice, DockTabKind::Context);
        dock.open(&bob, DockTabKind::Context);
        dock.open(&other, DockTabKind::Context);
        dock.set_collapsed(&alice, true);
        dock.set_width_px("repo", 500.0);
        assert!(dock.is_collapsed(&alice));
        assert!(!dock.is_collapsed(&bob));
        assert_eq!(dock.width_px("repo", 1200.0), 500.0);
        assert_eq!(dock.width_px("other", 1200.0), 540.0);
        assert_eq!(dock.width_px("repo", 600.0), 390.0);
        assert_eq!(dock.width_px("repo", 200.0), 200.0);
        assert_eq!(dock.width_px("repo", 1200.0), 500.0);
    }

    #[test]
    fn reorder_cannot_move_a_hidden_tab() {
        let mut dock = DockState::default();
        let alice = task("repo", "alice");
        let bob = task("repo", "bob");
        let a = dock.open(&alice, DockTabKind::Context);
        let b = dock.open(&alice, DockTabKind::Activity);
        let foreign = dock.open(&bob, DockTabKind::Context);
        assert!(!dock.reorder(&alice, foreign, a));
        assert!(dock.reorder(&alice, a, b));
        assert_eq!(
            dock.visible_tabs(&alice)
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            vec![b, a]
        );
    }

    #[test]
    fn open_file_uses_workspace_scope_and_rejects_escape() {
        let mut dock = DockState::default();
        let workspace = Path::new("repo");
        let file = Path::new("src/main.rs");
        let first = dock.open_file(workspace, "alice", file).unwrap();
        assert_eq!(dock.open_file(workspace, "alice", file), Some(first));
        assert_eq!(
            dock.active_tab(&DockScope::for_owner(workspace, "alice"))
                .map(|tab| &tab.kind),
            Some(&DockTabKind::FilePreview {
                path: file.to_path_buf()
            })
        );
        assert_eq!(
            dock.visible_tabs(&DockScope::for_owner(workspace, "alice"))
                .len(),
            1
        );
        assert!(dock
            .open_file(workspace, "alice", Path::new("../secret"))
            .is_none());
        assert!(dock.open_file(workspace, "alice", Path::new("")).is_none());
        assert_eq!(dock.tabs("repo").len(), 1);
    }

    #[test]
    fn file_tab_path_can_be_previewed_from_its_workspace() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("notes.txt"), "visible").unwrap();
        let mut dock = DockState::default();
        let scope = DockScope::for_owner(temp.path(), "task");
        dock.open_file(temp.path(), "task", Path::new("notes.txt"))
            .unwrap();
        let tab = dock.active_tab(&scope).unwrap();
        let DockTabKind::FilePreview { path } = &tab.kind else {
            panic!("file tab should be active")
        };
        assert_eq!(
            crate::files::read_text_preview(temp.path(), path).unwrap(),
            "visible"
        );
    }

    #[test]
    fn promote_draft_preserves_ids_active_and_collapse_without_duplicates() {
        let mut dock = DockState::default();
        let draft = DockScope::new("repo", DockOwner::Draft);
        let task = task("repo", "first-task");
        let draft_context = dock.open(&draft, DockTabKind::Context);
        let draft_activity = dock.open(&draft, DockTabKind::Activity);
        let duplicate_context = dock.open(&task, DockTabKind::Context);
        let duplicate_activity = dock.open(&task, DockTabKind::Activity);
        dock.set_collapsed(&draft, true);

        assert_eq!(dock.promote_draft("repo", "first-task"), 2);
        assert!(dock.visible_tabs(&draft).is_empty());
        assert_eq!(dock.visible_tabs(&task).len(), 2);
        assert_eq!(dock.tabs("repo")[0].id, draft_context);
        assert_eq!(dock.tabs("repo")[1].id, draft_activity);
        assert!(!dock
            .tabs("repo")
            .iter()
            .any(|tab| tab.id == duplicate_context || tab.id == duplicate_activity));
        assert_eq!(
            dock.active_tab(&task).map(|tab| tab.id),
            Some(draft_activity)
        );
        assert!(dock.is_collapsed(&task));
        assert_eq!(dock.promote_draft("repo", "first-task"), 0);
    }

    #[test]
    fn promote_draft_carries_global_tab_selection_without_draft_tabs() {
        let mut dock = DockState::default();
        let shared = DockScope::new("repo", DockOwner::Workspace);
        let draft = DockScope::new("repo", DockOwner::Draft);
        let task = task("repo", "first-task");
        let id = dock.open(&shared, DockTabKind::Context);
        assert!(dock.activate(&draft, id));
        dock.set_collapsed(&draft, true);
        assert_eq!(dock.promote_draft("repo", "first-task"), 0);
        assert_eq!(dock.active_tab(&task).map(|tab| tab.id), Some(id));
        assert!(dock.is_collapsed(&task));
    }

    #[test]
    fn bulk_close_only_affects_visible_scope() {
        let mut dock = DockState::default();
        let alice = task("repo", "alice");
        let bob = task("repo", "bob");
        let shared = DockScope::new("repo", DockOwner::Workspace);
        let global = dock.open(&shared, DockTabKind::Context);
        let first = dock.open(&alice, DockTabKind::Context);
        let second = dock.open(&alice, DockTabKind::Activity);
        let foreign = dock.open(&bob, DockTabKind::Activity);
        assert_eq!(dock.close_others(&alice, second).len(), 2);
        assert_eq!(dock.active_tab(&alice).map(|tab| tab.id), Some(second));
        assert!(!dock
            .tabs("repo")
            .iter()
            .any(|tab| tab.id == first || tab.id == global));
        assert!(dock.tabs("repo").iter().any(|tab| tab.id == foreign));
        assert_eq!(dock.close_all_visible(&alice).len(), 1);
        assert_eq!(dock.tabs("repo").len(), 1);
        assert_eq!(dock.tabs("repo")[0].id, foreign);
    }

    #[test]
    fn closed_active_tab_is_reopened_with_new_identity_and_focus() {
        let mut dock = DockState::default();
        let scope = task("repo", "alice");
        let context = dock.open(&scope, DockTabKind::Context);
        let activity = dock.open(&scope, DockTabKind::Activity);
        assert_eq!(
            dock.close(&scope, activity).map(|tab| tab.id),
            Some(activity)
        );
        assert_eq!(dock.active_tab(&scope).map(|tab| tab.id), Some(context));
        assert_eq!(dock.recently_closed(&scope)[0].tab.id, activity);
        let restored = dock
            .reopen_recent(&scope, activity)
            .expect("activity can reopen");
        assert_ne!(restored, activity);
        assert_eq!(dock.active_tab(&scope).map(|tab| tab.id), Some(restored));
        assert!(dock.recently_closed(&scope).is_empty());
    }

    #[test]
    fn recent_tabs_respect_workspace_and_owner_visibility() {
        let mut dock = DockState::default();
        let alice = task("repo", "alice");
        let bob = task("repo", "bob");
        let other_workspace = task("other", "alice");
        let shared = DockScope::new("repo", DockOwner::Workspace);
        let private = dock.open(&alice, DockTabKind::Activity);
        let global = dock.open(&shared, DockTabKind::CodeReview);
        dock.close(&alice, private);
        dock.close(&alice, global);
        assert_eq!(dock.recently_closed(&alice).len(), 2);
        assert_eq!(dock.recently_closed(&bob).len(), 1);
        assert_eq!(dock.recently_closed(&bob)[0].tab.id, global);
        assert!(dock.recently_closed(&other_workspace).is_empty());
        assert_eq!(dock.reopen_recent(&bob, private), None);
        assert_eq!(dock.reopen_recent(&other_workspace, global), None);
        let restored = dock.reopen_recent(&bob, global).expect("shared tab");
        assert_eq!(dock.active_tab(&bob).map(|tab| tab.id), Some(restored));
        assert_eq!(dock.active_tab(&alice).map(|tab| tab.id), Some(restored));
        assert_eq!(dock.recently_closed(&alice)[0].tab.id, private);
    }

    #[test]
    fn explicit_open_and_repeated_close_never_leave_duplicate_recent_entries() {
        let mut dock = DockState::default();
        let scope = task("repo", "alice");
        let first = dock.open(&scope, DockTabKind::Context);
        dock.close(&scope, first);
        let second = dock.open(&scope, DockTabKind::Context);
        assert!(dock.recently_closed(&scope).is_empty());
        dock.close(&scope, second);
        assert_eq!(dock.recently_closed(&scope).len(), 1);
        assert_eq!(dock.recently_closed(&scope)[0].tab.id, second);
        assert_eq!(dock.reopen_recent(&scope, first), None);
    }

    #[test]
    fn recent_history_is_bounded_to_eight_per_owner() {
        let mut dock = DockState::default();
        let alice = task("repo", "alice");
        let bob = task("repo", "bob");
        let bob_tab = dock.open(&bob, DockTabKind::Activity);
        dock.close(&bob, bob_tab);
        let mut ids = Vec::new();
        for index in 0..10 {
            let id = dock.open(
                &alice,
                DockTabKind::FilePreview {
                    path: PathBuf::from(format!("file-{index}.rs")),
                },
            );
            ids.push(id);
            dock.close(&alice, id);
        }
        assert_eq!(dock.recently_closed(&alice).len(), 8);
        assert_eq!(dock.recently_closed(&alice)[0].tab.id, ids[9]);
        assert_eq!(dock.recently_closed(&alice)[7].tab.id, ids[2]);
        assert_eq!(dock.reopen_recent(&alice, ids[0]), None);
        assert_eq!(dock.recently_closed(&bob).len(), 1);
    }

    #[test]
    fn closed_terminal_is_recorded_but_cannot_restore_a_dead_pty() {
        let mut dock = DockState::default();
        let scope = task("repo", "alice");
        let terminal = dock.open(&scope, DockTabKind::Terminal);
        dock.close(&scope, terminal);
        assert_eq!(dock.recently_closed(&scope).len(), 1);
        assert!(!dock.recently_closed(&scope)[0].restorable);
        assert_eq!(dock.reopen_recent(&scope, terminal), None);
        assert!(dock.active_tab(&scope).is_none());
    }

    #[test]
    fn promoting_draft_moves_only_its_recent_history() {
        let mut dock = DockState::default();
        let draft = DockScope::new("repo", DockOwner::Draft);
        let alice = task("repo", "alice");
        let bob = task("repo", "bob");
        let draft_tab = dock.open(&draft, DockTabKind::Activity);
        let bob_tab = dock.open(&bob, DockTabKind::Activity);
        dock.close(&draft, draft_tab);
        dock.close(&bob, bob_tab);
        assert_eq!(dock.promote_draft("repo", "alice"), 0);
        assert!(dock.recently_closed(&draft).is_empty());
        assert_eq!(dock.recently_closed(&alice)[0].tab.id, draft_tab);
        assert_eq!(dock.recently_closed(&bob)[0].tab.id, bob_tab);
        assert_eq!(dock.reopen_recent(&bob, draft_tab), None);
    }
}
