//! Pure state for the right dock and independent bottom terminal. Rendering
//! and external resources (files, PTYs,
//! browsers) belong to the app layer; this module only owns tab identity, scope,
//! ordering, selection, and per-workspace layout preferences.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Component, Path, PathBuf},
};

const DEFAULT_WIDTH_RATIO: f32 = 0.32;
const MAX_WIDTH_RATIO: f32 = 0.65;
const MIN_WIDTH_PX: f32 = 320.0;
const MAX_RECENT_PER_OWNER: usize = 8;
const DEFAULT_TERMINAL_HEIGHT_PX: f32 = 240.0;
const MIN_TERMINAL_HEIGHT_PX: f32 = 120.0;
const MAX_TERMINAL_HEIGHT_RATIO: f32 = 0.65;

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
    /// The workspace's files and how they link, drawn as a graph.
    Graph,
    CodeReview,
    SessionChanges,
    Terminal,
    Browser,
    /// The browser an external agent CLI drives from terminal tab `terminal`.
    /// It is that CLI's own: never the conversation's `Browser`.
    CliBrowser {
        terminal: u64,
    },
    FilePreview {
        path: PathBuf,
    },
    /// Immutable captured tool content, never a writable file identity.
    CodeSnapshot {
        source: String,
        title: String,
    },
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
            DockTabKind::Context => "Nueva pestaña".to_owned(),
            DockTabKind::Files => "Archivos".to_owned(),
            DockTabKind::Activity => "Actividad".to_owned(),
            DockTabKind::Graph => "Grafo".to_owned(),
            DockTabKind::CodeReview => "Repositorio".to_owned(),
            DockTabKind::SessionChanges => "Cambios del chat".to_owned(),
            DockTabKind::Terminal => "Terminal".to_owned(),
            DockTabKind::Browser => "Navegador".to_owned(),
            DockTabKind::CliBrowser { .. } => "Navegador de la CLI".to_owned(),
            DockTabKind::CodeSnapshot { title, .. } => format!("{title} · Vista previa"),
            DockTabKind::FilePreview { path } => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
        }
    }
}

/// A closed tab kept for the overview. Terminals and browsers are listed for
/// context but cannot restore a runtime that the host has already released.
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
    terminal_active_by_owner: HashMap<DockOwner, DockTabId>,
    terminal_collapsed_by_owner: HashMap<DockOwner, bool>,
    terminal_maximized_by_owner: HashMap<DockOwner, bool>,
    preferred_terminal_height_px: Option<f32>,
}

/// In-memory registry. Keep one instance in the application while switching
/// workspaces or tasks. Dropping it intentionally discards these preferences.
#[derive(Default)]
pub struct DockState {
    workspaces: HashMap<String, WorkspaceDock>,
    next_tab_id: u64,
    /// The scope whose right dock is maximized, and whether that hid its
    /// bottom terminal. One at a time: a conversation is left restored.
    maximized: Option<(DockScope, bool)>,
}

impl DockState {
    /// Open a tab, or activate the existing tab of the same kind and owner.
    /// Opening expands only its surface (right dock or bottom terminal).
    pub fn open(&mut self, scope: &DockScope, kind: DockTabKind) -> DockTabId {
        self.open_tab(scope, kind, true)
    }

    /// Explicit terminal creation owns a fresh PTY; opening the Terminal view
    /// through navigation can still reuse the existing tab with `open`.
    pub fn new_terminal(&mut self, scope: &DockScope) -> DockTabId {
        self.open_tab(scope, DockTabKind::Terminal, false)
    }

    fn open_tab(&mut self, scope: &DockScope, kind: DockTabKind, reuse: bool) -> DockTabId {
        let workspace = self
            .workspaces
            .entry(scope.workspace_id.clone())
            .or_default();
        // An explicit open supersedes an older closed entry for this same
        // owner and kind, even if an existing tab is merely reactivated.
        workspace
            .recently_closed
            .retain(|entry| entry.tab.owner != scope.owner || entry.tab.kind != kind);
        let selected = if kind == DockTabKind::Terminal {
            workspace.terminal_active_by_owner.get(&scope.owner)
        } else {
            workspace.active_by_owner.get(&scope.owner)
        };
        let reusable = |tab: &&DockTab| reuse && tab.owner == scope.owner && tab.kind == kind;
        if let Some(tab) = workspace
            .tabs
            .iter()
            .filter(reusable)
            .find(|tab| Some(&tab.id) == selected)
            .or_else(|| workspace.tabs.iter().find(reusable))
        {
            let id = tab.id;
            select_tab(workspace, &scope.owner, id, kind == DockTabKind::Terminal);
            return id;
        }

        self.next_tab_id = self
            .next_tab_id
            .checked_add(1)
            .expect("dock tab IDs exhausted");
        let id = DockTabId(self.next_tab_id);
        let terminal = kind == DockTabKind::Terminal;
        workspace.tabs.push(DockTab {
            id,
            workspace_id: scope.workspace_id.clone(),
            owner: scope.owner.clone(),
            kind,
        });
        select_tab(workspace, &scope.owner, id, terminal);
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
    /// singleton kind, the draft tab wins; the existing task duplicate is removed.
    /// Terminals retain every identity because each one owns a distinct live PTY.
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
            .filter(|tab| {
                tab.owner == target
                    && tab.kind != DockTabKind::Terminal
                    && draft_kinds.contains(&tab.kind)
            })
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

        promote_selection(
            &workspace.tabs,
            &mut workspace.active_by_owner,
            &target,
            false,
        );
        promote_selection(
            &workspace.tabs,
            &mut workspace.terminal_active_by_owner,
            &target,
            true,
        );
        for preferences in [
            &mut workspace.collapsed_by_owner,
            &mut workspace.terminal_collapsed_by_owner,
            &mut workspace.terminal_maximized_by_owner,
        ] {
            if let Some(value) = preferences.remove(&DockOwner::Draft) {
                preferences.insert(target.clone(), value);
            }
        }
        if let Some((scope, _)) = &mut self.maximized {
            if scope.workspace_id == workspace_id && scope.owner == DockOwner::Draft {
                scope.owner = target.clone();
            }
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

    /// A hidden task's resources still belong to their open tabs. Hosts use
    /// this across all workspaces rather than releasing a PTY/browser just
    /// because its owner is temporarily outside the visible scope.
    pub fn contains_tab(&self, raw_id: u64) -> bool {
        self.workspaces
            .values()
            .any(|workspace| workspace.tabs.iter().any(|tab| tab.id.get() == raw_id))
    }

    /// An open tab of any workspace and owner, by its raw ID.
    pub fn tab(&self, raw_id: u64) -> Option<&DockTab> {
        self.workspaces
            .values()
            .flat_map(|workspace| &workspace.tabs)
            .find(|tab| tab.id.get() == raw_id)
    }

    /// The open tab of this kind, of any workspace and owner.
    pub fn tab_of_kind(&self, kind: &DockTabKind) -> Option<&DockTab> {
        self.workspaces
            .values()
            .flat_map(|workspace| &workspace.tabs)
            .find(|tab| tab.kind == *kind)
    }

    /// A durably deleted task must release its private resources even when its
    /// tabs are currently hidden by another task. Shared workspace tabs survive.
    pub fn forget_task(&mut self, workspace_id: &str, task_id: &str) {
        let Some(workspace) = self.workspaces.get_mut(workspace_id) else {
            return;
        };
        let owner = DockOwner::Task(task_id.to_owned());
        workspace.tabs.retain(|tab| tab.owner != owner);
        workspace
            .recently_closed
            .retain(|entry| entry.tab.owner != owner);
        workspace.active_by_owner.remove(&owner);
        workspace.collapsed_by_owner.remove(&owner);
        workspace.terminal_active_by_owner.remove(&owner);
        workspace.terminal_collapsed_by_owner.remove(&owner);
        workspace.terminal_maximized_by_owner.remove(&owner);
        if self
            .maximized
            .as_ref()
            .is_some_and(|(scope, _)| scope.workspace_id == workspace_id && scope.owner == owner)
        {
            self.maximized = None;
        }
    }

    /// The complete visible resource registry, including the bottom terminals.
    /// Use `right_tabs` or `terminal_tabs` when rendering a particular surface.
    pub fn visible_tabs(&self, scope: &DockScope) -> Vec<&DockTab> {
        self.tabs(&scope.workspace_id)
            .iter()
            .filter(|tab| visible_to(tab, &scope.owner))
            .collect()
    }

    pub fn right_tabs(&self, scope: &DockScope) -> Vec<&DockTab> {
        self.visible_tabs(scope)
            .into_iter()
            .filter(|tab| tab.kind != DockTabKind::Terminal)
            .collect()
    }

    pub fn terminal_tabs(&self, scope: &DockScope) -> Vec<&DockTab> {
        self.visible_tabs(scope)
            .into_iter()
            .filter(|tab| tab.kind == DockTabKind::Terminal)
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

    /// Selection on the right never changes when opening or activating a terminal.
    pub fn active_tab(&self, scope: &DockScope) -> Option<&DockTab> {
        let workspace = self.workspaces.get(&scope.workspace_id)?;
        selected_tab(workspace, scope, false)
    }

    /// Bottom terminal selection is independent per owner, even while collapsed.
    pub fn active_terminal(&self, scope: &DockScope) -> Option<&DockTab> {
        let workspace = self.workspaces.get(&scope.workspace_id)?;
        selected_tab(workspace, scope, true)
    }

    /// Returns false if the tab belongs to another workspace or task.
    pub fn activate(&mut self, scope: &DockScope, id: DockTabId) -> bool {
        let Some(workspace) = self.workspaces.get_mut(&scope.workspace_id) else {
            return false;
        };
        let Some(tab) = workspace
            .tabs
            .iter()
            .find(|tab| tab.id == id && visible_to(tab, &scope.owner))
        else {
            return false;
        };
        let terminal = tab.kind == DockTabKind::Terminal;
        select_tab(workspace, &scope.owner, id, terminal);
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
            restorable: !matches!(
                removed.kind,
                DockTabKind::Terminal
                    | DockTabKind::Browser
                    | DockTabKind::CliBrowser { .. }
                    | DockTabKind::CodeSnapshot { .. }
            ),
            tab: removed.clone(),
        });
        trim_recent_owner(workspace, &removed.owner);
        Some(removed)
    }

    /// The file or folder `old` moved to `new`, or is gone when `new` is
    /// `None`. Its file tabs and closed entries, of every owner, follow it in
    /// place; a tab that is gone, or whose new path already has a tab of the
    /// same owner, is closed without a closed entry. Returns the closed IDs.
    pub fn retarget_files(
        &mut self,
        workspace_id: &str,
        old: &Path,
        new: Option<&Path>,
    ) -> Vec<DockTabId> {
        let Some(workspace) = self.workspaces.get_mut(workspace_id) else {
            return Vec::new();
        };
        // `None`: not under `old`. `Some(None)`: under it, and gone.
        let target = |kind: &DockTabKind| match kind {
            DockTabKind::FilePreview { path } => crate::file_ops::moved(path, old, new)
                .map(|path| path.map(|path| DockTabKind::FilePreview { path })),
            _ => None,
        };
        workspace
            .recently_closed
            .retain_mut(|entry| match target(&entry.tab.kind) {
                Some(Some(kind)) => {
                    entry.tab.kind = kind;
                    true
                }
                Some(None) => false,
                None => true,
            });
        let mut closed = Vec::new();
        let mut index = 0;
        while index < workspace.tabs.len() {
            let owner = &workspace.tabs[index].owner;
            let kind = match target(&workspace.tabs[index].kind) {
                None => {
                    index += 1;
                    continue;
                }
                Some(kind) => kind.filter(|kind| {
                    !workspace
                        .tabs
                        .iter()
                        .any(|tab| tab.owner == *owner && tab.kind == *kind)
                }),
            };
            if let Some(kind) = kind {
                workspace.tabs[index].kind = kind;
                index += 1;
            } else {
                let id = workspace.tabs.remove(index).id;
                repair_active_after_close(workspace, id, index);
                closed.push(id);
            }
        }
        closed
    }

    /// Close other right-side tabs. Bottom terminals and hidden tasks survive.
    /// Passing a terminal as `keep` is rejected without changing either selection.
    pub fn close_others(&mut self, scope: &DockScope, keep: DockTabId) -> Vec<DockTab> {
        if !self.right_tabs(scope).iter().any(|tab| tab.id == keep) || !self.activate(scope, keep) {
            return Vec::new();
        }
        let ids: Vec<DockTabId> = self
            .right_tabs(scope)
            .into_iter()
            .filter(|tab| tab.id != keep)
            .map(|tab| tab.id)
            .collect();
        ids.into_iter()
            .filter_map(|id| self.close(scope, id))
            .collect()
    }

    /// Close all right-side tabs visible to this owner, preserving terminals
    /// and tabs of other tasks. Explicit resource teardown uses `visible_tabs`.
    pub fn close_all_visible(&mut self, scope: &DockScope) -> Vec<DockTab> {
        let ids: Vec<DockTabId> = self
            .right_tabs(scope)
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
        if index == over_index
            || (workspace.tabs[index].kind == DockTabKind::Terminal)
                != (workspace.tabs[over_index].kind == DockTabKind::Terminal)
        {
            return false;
        }
        let tab = workspace.tabs.remove(index);
        workspace.tabs.insert(over_index, tab);
        true
    }

    pub fn set_collapsed(&mut self, scope: &DockScope, collapsed: bool) {
        if collapsed {
            // A hidden dock covers nothing: the conversation comes back.
            self.set_maximized(scope, false);
        }
        self.workspaces
            .entry(scope.workspace_id.clone())
            .or_default()
            .collapsed_by_owner
            .insert(scope.owner.clone(), collapsed);
    }

    /// An empty scope can remain open to show the tab launcher. Closing the
    /// final tab must not silently change the user's dock visibility choice.
    /// A conversation without a visibility preference starts collapsed until
    /// the user opens the panel or one of its tabs.
    pub fn is_collapsed(&self, scope: &DockScope) -> bool {
        self.workspaces
            .get(&scope.workspace_id)
            .and_then(|workspace| workspace.collapsed_by_owner.get(&scope.owner))
            .copied()
            .unwrap_or(true)
    }

    /// The right dock takes the room of the conversation. A bottom terminal
    /// that was showing is hidden for as long and comes back on restore; the
    /// dock's tabs, selection and preferred width are not touched. Any other
    /// maximized scope is restored first.
    pub fn set_maximized(&mut self, scope: &DockScope, maximized: bool) {
        if self.is_maximized(scope) == maximized {
            return;
        }
        self.restore_maximized();
        if maximized {
            self.set_collapsed(scope, false);
            let hid_terminal = !self.is_terminal_collapsed(scope);
            if hid_terminal {
                self.set_terminal_collapsed(scope, true);
            }
            self.maximized = Some((scope.clone(), hid_terminal));
        }
    }

    pub fn is_maximized(&self, scope: &DockScope) -> bool {
        self.maximized
            .as_ref()
            .is_some_and(|(maximized, _)| maximized == scope)
    }

    /// Back to the conversation, whichever scope's dock was maximized.
    pub fn restore_maximized(&mut self) {
        if let Some((scope, true)) = self.maximized.take() {
            self.set_terminal_collapsed(&scope, false);
        }
    }

    pub fn set_terminal_collapsed(&mut self, scope: &DockScope, collapsed: bool) {
        self.workspaces
            .entry(scope.workspace_id.clone())
            .or_default()
            .terminal_collapsed_by_owner
            .insert(scope.owner.clone(), collapsed);
    }

    pub fn is_terminal_collapsed(&self, scope: &DockScope) -> bool {
        self.workspaces
            .get(&scope.workspace_id)
            .and_then(|workspace| workspace.terminal_collapsed_by_owner.get(&scope.owner))
            .copied()
            .unwrap_or(true)
    }

    pub fn set_terminal_maximized(&mut self, scope: &DockScope, maximized: bool) {
        self.workspaces
            .entry(scope.workspace_id.clone())
            .or_default()
            .terminal_maximized_by_owner
            .insert(scope.owner.clone(), maximized);
    }

    pub fn is_terminal_maximized(&self, scope: &DockScope) -> bool {
        self.workspaces
            .get(&scope.workspace_id)
            .and_then(|workspace| workspace.terminal_maximized_by_owner.get(&scope.owner))
            .copied()
            .unwrap_or(false)
    }

    pub fn set_terminal_height_px(&mut self, workspace_id: &str, height_px: f32) {
        if height_px.is_finite() && height_px > 0.0 {
            self.workspaces
                .entry(workspace_id.to_owned())
                .or_default()
                .preferred_terminal_height_px = Some(height_px);
        }
    }

    /// Unclamped workspace preference. Presentation applies current viewport
    /// limits without overwriting this value when the window becomes smaller.
    pub fn preferred_terminal_height_px(&self, workspace_id: &str) -> f32 {
        self.workspaces
            .get(workspace_id)
            .and_then(|workspace| workspace.preferred_terminal_height_px)
            .unwrap_or(DEFAULT_TERMINAL_HEIGHT_PX)
    }

    /// Keep the user's preference while fitting within 65% of the available
    /// height. The 120 px minimum yields on very short windows; it never pushes
    /// the transcript or composer outside their available space.
    pub fn terminal_height_px(&self, workspace_id: &str, available_height_px: f32) -> f32 {
        if !available_height_px.is_finite() || available_height_px <= 0.0 {
            return 0.0;
        }
        let max = available_height_px * MAX_TERMINAL_HEIGHT_RATIO;
        let min = MIN_TERMINAL_HEIGHT_PX.min(max);
        let preferred = self.preferred_terminal_height_px(workspace_id);
        preferred.clamp(min, max)
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

    /// Width is 32% initially, at least 320 px when space allows, and at most
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

fn select_tab(workspace: &mut WorkspaceDock, owner: &DockOwner, id: DockTabId, terminal: bool) {
    let (active, collapsed) = if terminal {
        (
            &mut workspace.terminal_active_by_owner,
            &mut workspace.terminal_collapsed_by_owner,
        )
    } else {
        (
            &mut workspace.active_by_owner,
            &mut workspace.collapsed_by_owner,
        )
    };
    active.insert(owner.clone(), id);
    collapsed.insert(owner.clone(), false);
}

fn selected_tab<'a>(
    workspace: &'a WorkspaceDock,
    scope: &DockScope,
    terminal: bool,
) -> Option<&'a DockTab> {
    let active = if terminal {
        &workspace.terminal_active_by_owner
    } else {
        &workspace.active_by_owner
    }
    .get(&scope.owner);
    let eligible = |tab: &&DockTab| {
        (tab.kind == DockTabKind::Terminal) == terminal && visible_to(tab, &scope.owner)
    };
    workspace
        .tabs
        .iter()
        .filter(eligible)
        .find(|tab| Some(&tab.id) == active)
        .or_else(|| workspace.tabs.iter().rev().find(eligible))
}

fn promote_selection(
    tabs: &[DockTab],
    selections: &mut HashMap<DockOwner, DockTabId>,
    target: &DockOwner,
    terminal: bool,
) {
    let draft = selections.remove(&DockOwner::Draft);
    let previous = selections.get(target).copied();
    let eligible = |id: &DockTabId| {
        tabs.iter().any(|tab| {
            tab.id == *id
                && visible_to(tab, target)
                && (tab.kind == DockTabKind::Terminal) == terminal
        })
    };
    if let Some(id) = draft.filter(eligible).or_else(|| previous.filter(eligible)) {
        selections.insert(target.clone(), id);
    } else {
        selections.remove(target);
    }
}

fn repair_active_after_close(workspace: &mut WorkspaceDock, removed_id: DockTabId, index: usize) {
    for (selections, terminal) in [
        (&mut workspace.active_by_owner, false),
        (&mut workspace.terminal_active_by_owner, true),
    ] {
        for (owner, active_id) in selections.iter_mut() {
            if *active_id != removed_id {
                continue;
            }
            let eligible = |tab: &&DockTab| {
                visible_to(tab, owner) && (tab.kind == DockTabKind::Terminal) == terminal
            };
            let split = index.min(workspace.tabs.len());
            let next = workspace.tabs[split..]
                .iter()
                .find(eligible)
                .or_else(|| workspace.tabs[..split].iter().rev().find(eligible));
            *active_id = next.map_or(DockTabId(0), |tab| tab.id);
        }
        selections.retain(|_, id| id.get() != 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_terminal_creation_keeps_independent_ids_and_lifetimes() {
        let mut dock = DockState::default();
        let scope = DockScope::new("workspace", DockOwner::Draft);
        let first = dock.new_terminal(&scope);
        let second = dock.new_terminal(&scope);
        assert_ne!(first, second);
        assert_eq!(dock.visible_tabs(&scope).len(), 2);
        dock.set_terminal_collapsed(&scope, true);
        assert_eq!(dock.open(&scope, DockTabKind::Terminal), second);
        assert!(!dock.is_terminal_collapsed(&scope));
        dock.close(&scope, first).unwrap();
        assert!(dock.contains_tab(second.get()));
        assert_eq!(dock.active_terminal(&scope).unwrap().id, second);
        assert!(dock.active_tab(&scope).is_none());
    }

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
    fn file_tabs_follow_a_moved_path_in_place_and_close_on_one_that_is_gone() {
        let file = |path: &str| DockTabKind::FilePreview { path: path.into() };
        let mut dock = DockState::default();
        let alice = task("repo", "alice");
        let bob = task("repo", "bob");
        let a = dock.open(&alice, file("src/a.rs"));
        let context = dock.open(&alice, DockTabKind::Context);
        let b = dock.open(&alice, file("src/deep/b.rs"));
        let other = dock.open(&alice, file("src.rs"));
        let hidden = dock.open(&bob, file("src/a.rs"));
        let closed = dock.open(&alice, file("src/c.rs"));
        dock.close(&alice, closed);
        assert!(dock.activate(&alice, a));
        assert!(dock.retarget_files("repo", Path::new("src"), Some(Path::new("lib"))).is_empty());
        assert!(dock.retarget_files("other", Path::new("lib"), None).is_empty());
        let tabs: Vec<_> = dock.tabs("repo").iter().map(|tab| (tab.id, tab.kind.clone())).collect();
        assert_eq!(
            tabs,
            [
                (a, file("lib/a.rs")),
                (context, DockTabKind::Context),
                (b, file("lib/deep/b.rs")),
                (other, file("src.rs")),
                (hidden, file("lib/a.rs")),
            ]
        );
        assert_eq!(dock.active_tab(&alice).map(|tab| tab.id), Some(a));
        assert_eq!(dock.recently_closed(&alice)[0].tab.kind, file("lib/c.rs"));
        // A tab already at the destination stays; the one that moved onto it closes.
        assert_eq!(
            dock.retarget_files("repo", Path::new("src.rs"), Some(Path::new("lib/a.rs"))),
            [other]
        );
        // Gone: the tabs of every owner close, the active one hands over, and
        // nothing is left to reopen.
        assert_eq!(dock.retarget_files("repo", Path::new("lib"), None), [a, b, hidden]);
        assert_eq!(dock.active_tab(&alice).map(|tab| tab.id), Some(context));
        assert!(dock.recently_closed(&alice).is_empty() && dock.tabs("repo").len() == 1);
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
        assert_eq!(dock.width_px("other", 1200.0), 384.0);
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
        assert!(
            dock.open_file(workspace, "alice", Path::new("../secret"))
                .is_none()
        );
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
        assert!(
            !dock
                .tabs("repo")
                .iter()
                .any(|tab| tab.id == duplicate_context || tab.id == duplicate_activity)
        );
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
        assert!(
            !dock
                .tabs("repo")
                .iter()
                .any(|tab| tab.id == first || tab.id == global)
        );
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

    #[test]
    fn promoting_multiple_draft_terminals_preserves_each_live_pty_identity() {
        let mut dock = DockState::default();
        let draft = DockScope::new("repo", DockOwner::Draft);
        let target = task("repo", "task");
        let existing = dock.new_terminal(&target);
        let first = dock.new_terminal(&draft);
        let second = dock.new_terminal(&draft);
        assert_eq!(dock.promote_draft("repo", "task"), 2);
        let terminals: Vec<_> = dock
            .visible_tabs(&target)
            .iter()
            .map(|tab| tab.id)
            .collect();
        assert_eq!(terminals, vec![existing, first, second]);
        assert!(dock.visible_tabs(&draft).is_empty());
        assert_eq!(
            dock.active_terminal(&target).map(|tab| tab.id),
            Some(second)
        );
        assert!(dock.active_tab(&target).is_none());
        assert!(dock.contains_tab(existing.get()));
        assert!(dock.contains_tab(first.get()));
        assert!(dock.contains_tab(second.get()));
    }

    #[test]
    fn a_maximized_dock_hides_its_terminal_and_every_exit_brings_it_back() {
        let mut dock = DockState::default();
        let alice = task("repo", "alice");
        let bob = task("repo", "bob");
        let browser = dock.open(&alice, DockTabKind::Browser);
        let file = dock.open(&alice, DockTabKind::Files);
        dock.new_terminal(&alice);
        dock.set_width_px("repo", 512.0);
        let tabs = |dock: &DockState| -> Vec<DockTabId> {
            dock.right_tabs(&alice).iter().map(|tab| tab.id).collect()
        };
        // The toggle, collapsing the dock, and another scope taking its turn.
        let exits: [fn(&mut DockState, &DockScope, &DockScope); 3] = [
            |dock, alice, _| dock.set_maximized(alice, false),
            |dock, alice, _| dock.set_collapsed(alice, true),
            |dock, _, bob| dock.set_maximized(bob, true),
        ];
        for (index, exit) in exits.into_iter().enumerate() {
            dock.set_collapsed(&alice, false);
            dock.set_maximized(&bob, false);
            assert!(!dock.is_maximized(&alice) && !dock.is_terminal_collapsed(&alice));
            dock.set_maximized(&alice, true);
            assert!(dock.is_maximized(&alice) && !dock.is_maximized(&bob));
            assert!(!dock.is_collapsed(&alice) && dock.is_terminal_collapsed(&alice));
            exit(&mut dock, &alice, &bob);
            assert!(!dock.is_maximized(&alice), "exit {index}");
            assert!(!dock.is_terminal_collapsed(&alice), "exit {index}");
            assert_eq!(dock.is_collapsed(&alice), index == 1);
            assert_eq!(tabs(&dock), [browser, file]);
            assert_eq!(dock.active_tab(&alice).map(|tab| tab.id), Some(file));
            assert_eq!(dock.width_px("repo", 2000.0), 512.0);
        }
        // A terminal that was hidden stays hidden, and one shown while the
        // dock was maximized is not hidden again by the restore.
        dock.set_maximized(&bob, false);
        dock.set_terminal_collapsed(&alice, true);
        dock.set_maximized(&alice, true);
        dock.set_maximized(&alice, false);
        assert!(dock.is_terminal_collapsed(&alice));
        dock.set_maximized(&alice, true);
        dock.set_terminal_collapsed(&alice, false);
        dock.set_maximized(&alice, false);
        assert!(!dock.is_terminal_collapsed(&alice));
    }

    #[test]
    fn a_maximized_draft_stays_maximized_as_its_task_and_a_forgotten_task_is_not() {
        let mut dock = DockState::default();
        let draft = DockScope::new("repo", DockOwner::Draft);
        let alice = task("repo", "alice");
        dock.open(&draft, DockTabKind::Browser);
        dock.new_terminal(&draft);
        dock.set_maximized(&draft, true);
        dock.promote_draft("repo", "alice");
        assert!(dock.is_maximized(&alice) && !dock.is_maximized(&draft));
        dock.set_maximized(&alice, false);
        assert!(!dock.is_terminal_collapsed(&alice), "the task's terminal comes back");
        dock.set_maximized(&alice, true);
        dock.forget_task("repo", "alice");
        dock.restore_maximized();
        assert!(!dock.is_maximized(&alice) && dock.is_terminal_collapsed(&alice));
    }

    #[test]
    fn terminal_and_right_selections_and_visibility_are_independent() {
        let mut dock = DockState::default();
        let scope = task("repo", "alice");
        let browser = dock.open(&scope, DockTabKind::Browser);
        dock.set_collapsed(&scope, true);
        let terminal = dock.open(&scope, DockTabKind::Terminal);
        assert_eq!(dock.active_tab(&scope).map(|tab| tab.id), Some(browser));
        assert_eq!(
            dock.active_terminal(&scope).map(|tab| tab.id),
            Some(terminal)
        );
        assert!(dock.is_collapsed(&scope));
        assert!(!dock.is_terminal_collapsed(&scope));
        dock.set_terminal_collapsed(&scope, true);
        dock.set_terminal_maximized(&scope, true);
        assert!(dock.activate(&scope, browser));
        assert!(!dock.is_collapsed(&scope));
        assert!(dock.is_terminal_collapsed(&scope));
        assert!(dock.is_terminal_maximized(&scope));
        assert!(dock.activate(&scope, terminal));
        assert!(!dock.is_terminal_collapsed(&scope));
        assert_eq!(
            dock.right_tabs(&scope)
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            [browser]
        );
        assert_eq!(
            dock.terminal_tabs(&scope)
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            [terminal]
        );
        assert_eq!(dock.visible_tabs(&scope).len(), 2);
        assert!(
            dock.contains_tab(terminal.get()),
            "collapsing never releases the PTY"
        );
    }

    #[test]
    fn right_bulk_close_and_reorder_never_touch_bottom_terminals() {
        let mut dock = DockState::default();
        let scope = task("repo", "alice");
        let browser = dock.open(&scope, DockTabKind::Browser);
        let terminal = dock.new_terminal(&scope);
        let file = dock.open(&scope, DockTabKind::Files);
        let second = dock.new_terminal(&scope);
        dock.set_terminal_collapsed(&scope, true);
        assert!(!dock.reorder(&scope, terminal, file));
        assert!(dock.reorder(&scope, second, terminal));
        assert!(dock.close_others(&scope, terminal).is_empty());
        assert_eq!(dock.active_tab(&scope).map(|tab| tab.id), Some(file));
        assert_eq!(
            dock.close_others(&scope, browser)
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            [file]
        );
        assert_eq!(dock.active_terminal(&scope).map(|tab| tab.id), Some(second));
        assert!(dock.is_terminal_collapsed(&scope));
        assert_eq!(
            dock.close_all_visible(&scope)
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            [browser]
        );
        assert!(dock.right_tabs(&scope).is_empty());
        assert_eq!(dock.terminal_tabs(&scope).len(), 2);
        assert!(dock.contains_tab(terminal.get()) && dock.contains_tab(second.get()));
        dock.close(&scope, second).unwrap();
        assert_eq!(
            dock.active_terminal(&scope).map(|tab| tab.id),
            Some(terminal)
        );
        assert!(dock.active_tab(&scope).is_none());
    }

    #[test]
    fn shared_terminal_close_repairs_only_terminal_selection_for_each_owner() {
        let mut dock = DockState::default();
        let shared = DockScope::new("repo", DockOwner::Workspace);
        let alice = task("repo", "alice");
        let bob = task("repo", "bob");
        let global = dock.new_terminal(&shared);
        let personal = dock.new_terminal(&alice);
        let browser_a = dock.open(&alice, DockTabKind::Browser);
        let browser_b = dock.open(&bob, DockTabKind::Browser);
        assert!(dock.activate(&alice, global));
        assert!(dock.activate(&bob, global));
        dock.close(&alice, global).unwrap();
        assert_eq!(
            dock.active_terminal(&alice).map(|tab| tab.id),
            Some(personal)
        );
        assert!(dock.active_terminal(&bob).is_none());
        assert_eq!(dock.active_tab(&alice).map(|tab| tab.id), Some(browser_a));
        assert_eq!(dock.active_tab(&bob).map(|tab| tab.id), Some(browser_b));
    }

    #[test]
    fn promotion_transfers_both_surfaces_without_overwriting_the_other_selection() {
        let mut dock = DockState::default();
        let draft = DockScope::new("repo", DockOwner::Draft);
        let target = task("repo", "task");
        let right = dock.open(&draft, DockTabKind::Browser);
        let first = dock.new_terminal(&draft);
        let second = dock.new_terminal(&draft);
        let existing = dock.new_terminal(&target);
        dock.activate(&draft, first);
        dock.set_collapsed(&draft, true);
        dock.set_terminal_collapsed(&draft, false);
        dock.set_terminal_maximized(&draft, true);
        assert_eq!(dock.promote_draft("repo", "task"), 3);
        assert_eq!(dock.active_tab(&target).map(|tab| tab.id), Some(right));
        assert_eq!(dock.active_terminal(&target).map(|tab| tab.id), Some(first));
        assert!(dock.is_collapsed(&target));
        assert!(!dock.is_terminal_collapsed(&target));
        assert!(dock.is_terminal_maximized(&target));
        assert!(dock.is_terminal_collapsed(&draft));
        assert!(!dock.is_terminal_maximized(&draft));
        assert!(dock.contains_tab(existing.get()) && dock.contains_tab(second.get()));
        dock.close(&target, first).unwrap();
        assert_eq!(
            dock.active_terminal(&target).map(|tab| tab.id),
            Some(second)
        );
        assert_eq!(dock.active_tab(&target).map(|tab| tab.id), Some(right));
    }

    #[test]
    fn terminal_preferences_are_per_owner_and_height_is_per_workspace_and_bounded() {
        let mut dock = DockState::default();
        let alice = task("repo", "alice");
        let bob = task("repo", "bob");
        let other = task("other", "alice");
        let a = dock.new_terminal(&alice);
        let b = dock.new_terminal(&bob);
        let c = dock.new_terminal(&other);
        dock.set_terminal_collapsed(&alice, true);
        dock.set_terminal_maximized(&alice, true);
        dock.set_terminal_height_px("repo", 400.0);
        assert_eq!(dock.terminal_height_px("repo", 1000.0), 400.0);
        assert_eq!(dock.terminal_height_px("other", 1000.0), 240.0);
        assert_eq!(dock.terminal_height_px("repo", 400.0), 260.0);
        assert_eq!(dock.terminal_height_px("repo", 100.0), 65.0);
        assert_eq!(dock.terminal_height_px("repo", 1000.0), 400.0);
        for invalid in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
            dock.set_terminal_height_px("repo", invalid);
            assert_eq!(dock.terminal_height_px("repo", 1000.0), 400.0);
            assert_eq!(dock.terminal_height_px("repo", invalid), 0.0);
        }
        assert!(!dock.is_terminal_collapsed(&bob));
        assert!(!dock.is_terminal_maximized(&bob));
        assert!(!dock.is_terminal_collapsed(&other));
        dock.forget_task("repo", "alice");
        assert!(!dock.contains_tab(a.get()));
        assert!(dock.contains_tab(b.get()) && dock.contains_tab(c.get()));
        assert!(dock.is_terminal_collapsed(&alice));
        assert!(!dock.is_terminal_maximized(&alice));
        assert_eq!(dock.terminal_height_px("repo", 1000.0), 400.0);
    }
}
