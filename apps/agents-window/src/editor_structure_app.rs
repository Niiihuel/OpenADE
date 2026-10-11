//! Revision-scoped editor structure and local, unverified breakpoint markers.
//!
//! Markers are synchronized with the DAP host. Edits/reloads invalidate their
//! exact source revision and remove them from the adapter rather than guessing.
use super::*;
use openide_language::AnalysisSnapshot;
use openide_ui::{CodeFoldCandidate, CodeStickyScope};
use std::collections::{BTreeSet, VecDeque};

const MAX_MARKED_FILES: usize = 64;
const MAX_MARKERS_PER_FILE: usize = 256;
const MAX_STRUCTURE_ITEMS: usize = 65_536;
const INLINE_STRUCTURE_ITEMS: usize = 2_048;

pub(in crate::app) struct FileBreakpoints {
    workspace: String,
    pub(in crate::app) path: String,
    identity: (u64, u64),
    dirty: bool,
    pub(in crate::app) rows: BTreeSet<usize>,
    /// Rows kept in the editor and withheld from the adapter.
    pub(in crate::app) disabled: BTreeSet<usize>,
}

/// Fold and sticky-scope controls for one analysed revision.
fn build_structure(
    analysis: &AnalysisSnapshot,
    snapshot: &BufferSnapshot,
    tab: u64,
) -> Structure {
    let identity = (snapshot.document_id(), snapshot.revision());
    let mut folds = Vec::new();
    let mut scopes = Vec::new();
    let mut scope_rows = BTreeSet::new();
    for fold in analysis.folds.iter().take(MAX_STRUCTURE_ITEMS) {
        let range = &fold.rows;
        if range.start.saturating_add(1) >= range.end
            || range.end > snapshot.line_count()
        {
            continue;
        }
        folds.push(CodeFoldCandidate {
            start_row: range.start,
            end_row: range.end,
            action_id: format!(
                "editor_fold_range:{tab}:{}:{}:{}:{}",
                identity.0, identity.1, range.start, range.end
            ),
        });
    }
    // Symbols preserve declaration bounds; anonymous blocks still
    // get sticky source headers from the parser's fold ranges.
    for symbol in analysis.symbols.iter().take(MAX_STRUCTURE_ITEMS) {
        if symbol.range.is_empty() || symbol.range.end > snapshot.char_count() {
            continue;
        }
        let Ok(start) = snapshot.row_of(symbol.range.start) else {
            continue;
        };
        let Ok(last) = snapshot.row_of(symbol.range.end - 1) else {
            continue;
        };
        if start >= last || !scope_rows.insert(start) {
            continue;
        }
        let offset = snapshot.line_range(start).unwrap().start;
        scopes.push(CodeStickyScope {
            start_row: start,
            end_row: last + 1,
            action_id: format!(
                "editor_scope_jump:{tab}:{}:{}:{offset}",
                identity.0, identity.1
            ),
        });
    }
    for fold in &folds {
        if scope_rows.insert(fold.start_row) {
            let offset = snapshot.line_range(fold.start_row).unwrap().start;
            scopes.push(CodeStickyScope {
                start_row: fold.start_row,
                end_row: fold.end_row,
                action_id: format!(
                    "editor_scope_jump:{tab}:{}:{}:{offset}",
                    identity.0, identity.1
                ),
            });
        }
    }
    scopes.sort_by_key(|scope| (scope.start_row, std::cmp::Reverse(scope.end_row)));
    (folds, scopes)
}

type Structure = (Vec<CodeFoldCandidate>, Vec<CodeStickyScope>);

#[derive(Default)]
pub(super) struct EditorStructureHost {
    analysis: Option<(u64, Arc<AnalysisSnapshot>)>,
    /// A large document's controls being built away from the interface thread.
    building: Option<(u64, Arc<AnalysisSnapshot>, std::sync::mpsc::Receiver<Structure>)>,
    breakpoints: VecDeque<FileBreakpoints>,
    breakpoint_generation: u64,
}

impl AgentsWindow {
    /// Called after the language worker poll. Mapping is rebuilt only when its
    /// immutable analysis changes; idle paints merely compare identities.
    pub(in crate::app) fn sync_editor_structure(&mut self) -> bool {
        let Some((tab, path, _, snapshot)) = self.active_editor() else {
            return false;
        };
        let identity = (snapshot.document_id(), snapshot.revision());
        let state = &mut self.view.editor_workbench;
        let host = &mut self.editor_services.structure;
        let analysis = state
            .analysis
            .as_ref()
            .filter(|analysis| (analysis.document_id, analysis.revision) == identity);
        let same_analysis = match (analysis, &host.analysis) {
            (Some(current), Some((previous_tab, previous))) => {
                tab == *previous_tab
                    && Arc::ptr_eq(current, previous)
                    && state.structure_revision == Some(identity)
            }
            (None, None) => state.structure_revision.is_none(),
            _ => false,
        };
        let mut changed = false;
        if !same_analysis {
            // An edit of the same document has already moved the folds with
            // the text; only another document invalidates them here.
            if state
                .structure_revision
                .is_some_and(|previous| previous.0 != identity.0)
            {
                state.folds = Arc::from([]);
            }
            state.fold_candidates = Arc::from([]);
            state.sticky_scopes = Arc::from([]);
            state.structure_revision = None;
            host.analysis = None;
            if let Some(analysis) = analysis {
                let waiting = host.building.take().filter(|(owner, pending, _)| {
                    *owner == tab && Arc::ptr_eq(pending, analysis)
                });
                let built = match waiting {
                    Some((_, _, receiver)) => match receiver.try_recv() {
                        Ok(built) => Some(built),
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            host.building = Some((tab, analysis.clone(), receiver));
                            None
                        }
                        Err(_) => Some(build_structure(analysis, &snapshot, tab)),
                    },
                    None if analysis.folds.len() + analysis.symbols.len() <= INLINE_STRUCTURE_ITEMS => {
                        Some(build_structure(analysis, &snapshot, tab))
                    }
                    None => {
                        let (sender, receiver) = std::sync::mpsc::channel();
                        let (work, text) = (analysis.clone(), snapshot.clone());
                        let spawned = std::thread::Builder::new()
                            .name("editor-structure".into())
                            .spawn(move || {
                                let _ = sender.send(build_structure(&work, &text, tab));
                            });
                        if spawned.is_ok() {
                            host.building = Some((tab, analysis.clone(), receiver));
                            None
                        } else {
                            Some(build_structure(analysis, &snapshot, tab))
                        }
                    }
                };
                if let Some((folds, scopes)) = built {
                    state.fold_candidates = folds.into();
                    state.sticky_scopes = scopes.into();
                    state.structure_revision = Some(identity);
                    host.analysis = Some((tab, analysis.clone()));
                }
            } else {
                host.building = None;
            }
            changed = true;
        }
        let index = host
            .breakpoints
            .iter()
            .position(|entry| entry.workspace == self.view.workspace && entry.path == path);
        if let Some(index) = index {
            if host.breakpoints[index].identity != identity {
                host.breakpoints.remove(index);
                host.breakpoint_generation = host.breakpoint_generation.wrapping_add(1);
                changed = true;
            } else if host.breakpoints[index].dirty != snapshot.is_dirty() {
                host.breakpoints[index].dirty = snapshot.is_dirty();
                host.breakpoint_generation = host.breakpoint_generation.wrapping_add(1);
                changed = true;
            }
        }
        let rows = host
            .breakpoints
            .iter()
            .find(|entry| entry.workspace == self.view.workspace && entry.path == path);
        let same_rows = rows.map_or(state.breakpoint_rows.is_empty(), |entry| {
            entry
                .rows
                .iter()
                .copied()
                .eq(state.breakpoint_rows.iter().copied())
        });
        if state.breakpoint_revision != Some(identity) || !same_rows {
            state.breakpoint_rows = rows.map_or_else(
                || Arc::from([]),
                |entry| entry.rows.iter().copied().collect::<Vec<_>>().into(),
            );
            state.breakpoint_revision = Some(identity);
            changed = true;
        }
        changed
    }

    fn toggle_local_breakpoint(
        &mut self,
        path: String,
        identity: (u64, u64),
        row: usize,
        dirty: bool,
    ) {
        let row = self.debug_requested_breakpoint_row(&path, row);
        let host = &mut self.editor_services.structure;
        host.breakpoint_generation = host.breakpoint_generation.wrapping_add(1);
        let existing = host
            .breakpoints
            .iter()
            .position(|entry| entry.workspace == self.view.workspace && entry.path == path);
        let mut entry = existing
            .and_then(|index| host.breakpoints.remove(index))
            .unwrap_or_else(|| FileBreakpoints {
                workspace: self.view.workspace.clone(),
                path,
                identity,
                dirty,
                rows: BTreeSet::new(),
                disabled: BTreeSet::new(),
            });
        entry.dirty = dirty;
        if entry.identity != identity {
            entry.identity = identity;
            entry.rows.clear();
        }
        if !entry.rows.remove(&row) && entry.rows.len() < MAX_MARKERS_PER_FILE {
            entry.rows.insert(row);
        }
        entry.disabled.retain(|row| entry.rows.contains(row));
        if !entry.rows.is_empty() {
            host.breakpoints.push_back(entry);
            while host.breakpoints.len() > MAX_MARKED_FILES {
                host.breakpoints.pop_front();
            }
        }
        self.sync_editor_structure();
    }

    /// Markers follow a file that moved to `new` and go with one that is gone.
    pub(in crate::app) fn retarget_breakpoints(&mut self, old: &Path, new: Option<&Path>) {
        let workspace = &self.view.workspace;
        let host = &mut self.editor_services.structure;
        let before: Vec<_> = host.breakpoints.iter().map(|entry| entry.path.clone()).collect();
        host.breakpoints.retain_mut(|entry| {
            if entry.workspace != *workspace {
                return true;
            }
            match crate::file_ops::moved(Path::new(&entry.path), old, new) {
                Some(Some(path)) => entry.path = path.to_string_lossy().into_owned(),
                Some(None) => return false,
                None => {}
            }
            true
        });
        if !host.breakpoints.iter().map(|entry| &entry.path).eq(&before) {
            host.breakpoint_generation = host.breakpoint_generation.wrapping_add(1);
        }
    }

    /// Changes this workspace's marks from the debug sidebar. A file left
    /// without marks leaves the store, and the adapter is told on the next poll.
    pub(in crate::app) fn edit_debug_breakpoints(
        &mut self,
        mut edit: impl FnMut(&mut FileBreakpoints),
    ) {
        let workspace = &self.view.workspace;
        let host = &mut self.editor_services.structure;
        host.breakpoints.retain_mut(|entry| {
            if entry.workspace == *workspace {
                edit(entry);
                entry.disabled.retain(|row| entry.rows.contains(row));
            }
            !entry.rows.is_empty()
        });
        host.breakpoint_generation = host.breakpoint_generation.wrapping_add(1);
        self.sync_editor_structure();
    }

    pub(in crate::app) fn debug_breakpoint_enabled(&self, path: &str, row: usize) -> bool {
        !self.editor_services.structure.breakpoints.iter().any(|entry| {
            entry.workspace == self.view.workspace
                && entry.path == path
                && entry.disabled.contains(&row)
        })
    }

    pub(in crate::app) fn debug_breakpoint_generation(&self) -> u64 {
        self.editor_services.structure.breakpoint_generation
    }

    pub(in crate::app) fn debug_source_breakpoints(&self) -> Vec<openide_debug::SourceBreakpoints> {
        self.editor_services
            .structure
            .breakpoints
            .iter()
            .filter(|entry| entry.workspace == self.view.workspace && !entry.dirty)
            .map(|entry| openide_debug::SourceBreakpoints {
                path: PathBuf::from(&entry.workspace).join(&entry.path),
                lines: entry
                    .rows
                    .difference(&entry.disabled)
                    .filter_map(|row| u32::try_from(row.saturating_add(1)).ok())
                    .collect(),
            })
            .collect()
    }

    /// Sidebar projection includes unsaved marks. Adapter projection above
    /// deliberately excludes them; hiding them here would lose user context.
    pub(in crate::app) fn debug_sidebar_breakpoints(
        &self,
    ) -> Vec<crate::debug_ui::DebugBreakpointView> {
        let mut markers = self
            .editor_services
            .structure
            .breakpoints
            .iter()
            .filter(|entry| entry.workspace == self.view.workspace)
            .flat_map(|entry| {
                entry.rows.iter().filter_map(|row| {
                    Some(crate::debug_ui::DebugBreakpointView {
                        path: entry.path.clone(),
                        line: u32::try_from(row.saturating_add(1)).ok()?,
                        dirty: entry.dirty,
                        enabled: !entry.disabled.contains(row),
                        ..Default::default()
                    })
                })
            })
            .collect::<Vec<_>>();
        markers.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        markers
    }

    pub(super) fn apply_editor_structure_action(&mut self, id: &str, ctx: &egui::Context) -> bool {
        let Some((kind, fields)) = id
            .split_once(':')
            .or_else(|| (id == "editor_breakpoint").then_some((id, "")))
        else {
            return false;
        };
        if !matches!(
            kind,
            "editor_fold_range" | "editor_scope_jump" | "editor_breakpoint"
        ) {
            return false;
        }
        let Some((tab, path, _, snapshot)) = self.active_editor() else {
            return true;
        };
        let scope = self.dock_scope();
        if kind == "editor_breakpoint" && !self.ide_workspace_active() {
            return true;
        }
        if self.view.main_page != MainPage::Workspace
            || self.view.show_settings
            || !self.resource_pane_visible()
            || self.view.side_tab != SideTab::FilePreview
            || self.view.code_editor.as_ref().is_none_or(|view| view.loading)
            || self.dock.active_tab(&scope).is_none_or(|active| {
                active.id.get() != tab
                    || !matches!(&active.kind, DockTabKind::FilePreview { path: p } if p == &PathBuf::from(&path))
            })
        {
            return true;
        }
        let identity = (snapshot.document_id(), snapshot.revision());
        let mut fields = fields.split(':');
        let row_or_offset = if id == "editor_breakpoint" {
            snapshot
                .position_of(snapshot.selection().head)
                .ok()
                .map(|p| p.row)
        } else {
            let action_tab = fields.next().and_then(|n| n.parse::<u64>().ok());
            let document = fields.next().and_then(|n| n.parse::<u64>().ok());
            let revision = fields.next().and_then(|n| n.parse::<u64>().ok());
            if (action_tab, document, revision) != (Some(tab), Some(identity.0), Some(identity.1)) {
                return true;
            }
            fields.next().and_then(|n| n.parse::<usize>().ok())
        };
        let Some(row_or_offset) = row_or_offset else {
            return true;
        };
        self.sync_editor_structure();
        match kind {
            "editor_breakpoint" => {
                if row_or_offset >= snapshot.line_count()
                    || (id != "editor_breakpoint" && fields.next().is_some())
                {
                    return true;
                }
                self.toggle_local_breakpoint(path, identity, row_or_offset, snapshot.is_dirty());
            }
            "editor_fold_range" => {
                let end = fields.next().and_then(|n| n.parse::<usize>().ok());
                if fields.next().is_some()
                    || !self
                        .view
                        .editor_workbench
                        .fold_candidates
                        .iter()
                        .any(|fold| {
                            fold.start_row == row_or_offset
                                && Some(fold.end_row) == end
                                && fold.action_id == id
                        })
                {
                    return true;
                }
                let range = row_or_offset..end.unwrap();
                let state = &mut self.view.editor_workbench;
                let mut folds = state.folds.to_vec();
                if folds.iter().any(|existing| existing.start == range.start) {
                    // The gutter has one control per physical row. A narrower
                    // block folded by the current-block command can share this
                    // header with the outer candidate; an expand click must
                    // reveal it instead of adding another, larger fold.
                    folds.retain(|existing| existing.start != range.start);
                } else {
                    folds.push(range.clone());
                    folds.sort_by_key(|fold| (fold.start, fold.end));
                    // Explicitly folding a caret's body moves only affected
                    // selections to the header; other directed selections stay.
                    let header = snapshot.line_range(range.start).unwrap().start;
                    let selections: Vec<_> = snapshot
                        .selections()
                        .map(|selection| {
                            if snapshot.position_of(selection.head).is_ok_and(|position| {
                                position.row > range.start && position.row < range.end
                            }) {
                                Selection::caret(header)
                            } else {
                                selection
                            }
                        })
                        .collect();
                    if !selections.iter().copied().eq(snapshot.selections()) {
                        let result =
                            self.editor_command(EditorCommand::SetSelections(selections), ctx);
                        self.editor_error(result);
                    }
                }
                self.view.editor_workbench.folds = folds.into();
            }
            "editor_scope_jump" => {
                if fields.next().is_some()
                    || !self
                        .view
                        .editor_workbench
                        .sticky_scopes
                        .iter()
                        .any(|scope| scope.action_id == id)
                {
                    return true;
                }
                let result = self.editor_command(
                    EditorCommand::SetSelection(Selection::caret(row_or_offset)),
                    ctx,
                );
                self.editor_error(result);
            }
            _ => unreachable!(),
        }
        self.focus_active_editor();
        ctx.request_repaint();
        true
    }
}

#[cfg(test)]
#[path = "editor_structure_app_tests.rs"]
mod tests;

#[cfg(test)]
mod sidebar_breakpoint_tests {
    use super::*;

    #[test]
    fn ide_breakpoint_sidebar_preserves_dirty_marks_and_revalidates_navigation() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("main.rs"), "fn main() {}\n\n").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("state"));
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.view.main_page = MainPage::Workspace;
        app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
        let ctx = egui::Context::default();
        app.poll_debugger(&ctx);
        app.view.debug_open = true;
        app.editor_services
            .structure
            .breakpoints
            .push_back(FileBreakpoints {
                workspace: app.view.workspace.clone(),
                path: "main.rs".into(),
                identity: (1, 0),
                dirty: false,
                rows: [1].into_iter().collect(),
                disabled: BTreeSet::new(),
            });
        app.editor_services
            .structure
            .breakpoints
            .push_back(FileBreakpoints {
                workspace: app.view.workspace.clone(),
                path: "draft.rs".into(),
                identity: (2, 0),
                dirty: true,
                rows: [2].into_iter().collect(),
                disabled: BTreeSet::new(),
            });
        app.editor_services
            .structure
            .breakpoints
            .push_back(FileBreakpoints {
                workspace: "another-project".into(),
                path: "private.rs".into(),
                identity: (3, 0),
                dirty: false,
                rows: [0].into_iter().collect(),
                disabled: BTreeSet::new(),
            });
        let rows = app.debug_sidebar_breakpoints();
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .any(|marker| marker.path == "draft.rs" && marker.dirty && marker.line == 3));
        assert_eq!(app.debug_source_breakpoints().len(), 1);
        let action = |scope: &str, path: &str, line: u32| UiAction::Clicked {
            id: format!(
                "debug_breakpoint:{}",
                serde_json::to_string(&(scope, (path, line))).unwrap()
            ),
        };
        let current_scope = app.view.debug.scope.clone();
        for (scope, path, line) in [
            ("stale", "main.rs", 2),
            (current_scope.as_str(), "draft.rs", 3),
            (current_scope.as_str(), "main.rs", 999),
            (current_scope.as_str(), "../private.rs", 1),
        ] {
            let action = action(scope, path, line);
            assert!(app.apply_debug_action(&action, &ctx));
            assert!(app.editor_services.pending_navigation.is_none());
        }
        let action = action(&app.view.debug.scope, "main.rs", 2);
        assert!(app.apply_debug_action(&action, &ctx));
        assert_eq!(
            app.editor_services
                .pending_navigation
                .as_ref()
                .map(|(path, position)| (path.as_path(), position.line)),
            Some((std::path::Path::new("main.rs"), 1))
        );
    }
}
