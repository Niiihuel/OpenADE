//! Host coordination for explicit editor actions and last-wins background search.
use super::*;
use crate::editor_workbench_ui::{EditorForm, EditorWorkbenchView};
use openide_editor::{
    BufferSnapshot, EditorCommand, IndentStyle, LineEnding, Position, SearchOptions, SearchResults,
    Selection,
};
use openide_ui::{CodeLanguage, EditorController, Key};
use std::sync::{Arc, Condvar, Mutex};

#[path = "editor_structure_app.rs"]
mod editor_structure_app;

#[cfg(feature = "editor-e2e")]
#[path = "desktop_e2e_observer.rs"]
mod desktop_e2e_observer;

struct SearchJob {
    generation: u64,
    snapshot: BufferSnapshot,
    query: String,
    options: SearchOptions,
}
#[derive(Default)]
struct SearchSlot {
    stop: bool,
    job: Option<SearchJob>,
    result: Option<(u64, Result<SearchResults, String>)>,
}
struct SearchWorker {
    slot: Arc<(Mutex<SearchSlot>, Condvar)>,
    generation: u64,
}
impl SearchWorker {
    fn new() -> Result<Self, String> {
        let slot = Arc::new((Mutex::new(SearchSlot::default()), Condvar::new()));
        let shared = slot.clone();
        std::thread::Builder::new()
            .name("editor-search".into())
            .spawn(move || loop {
                let job = {
                    let (lock, wake) = &*shared;
                    let Ok(mut state) = lock.lock() else {
                        return;
                    };
                    while state.job.is_none() && !state.stop {
                        let Ok(next) = wake.wait(state) else {
                            return;
                        };
                        state = next;
                    }
                    if state.stop {
                        return;
                    }
                    state.job.take().unwrap()
                };
                let result = job
                    .snapshot
                    .search(&job.query, &job.options)
                    .map_err(|e| e.to_string());
                if let Ok(mut state) = shared.0.lock() {
                    if !state.stop {
                        state.result = Some((job.generation, result));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            slot,
            generation: 0,
        })
    }
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Ok(mut state) = self.slot.0.lock() {
            state.result = None;
            state.job = None;
        }
    }
    fn submit(&mut self, snapshot: BufferSnapshot, query: String, options: SearchOptions) {
        self.generation = self.generation.wrapping_add(1);
        if let Ok(mut state) = self.slot.0.lock() {
            state.result = None;
            state.job = Some(SearchJob {
                generation: self.generation,
                snapshot,
                query,
                options,
            });
            self.slot.1.notify_one();
        }
    }
    fn poll(&self) -> Option<Result<SearchResults, String>> {
        self.slot
            .0
            .lock()
            .ok()?
            .result
            .take()
            .and_then(|(generation, result)| (generation == self.generation).then_some(result))
    }
}
impl Drop for SearchWorker {
    fn drop(&mut self) {
        if let Ok(mut state) = self.slot.0.lock() {
            state.stop = true;
            state.job = None;
            self.slot.1.notify_all();
        }
    }
}

pub(super) struct EditorServices {
    #[cfg(feature = "editor-e2e")]
    e2e_observer: Option<desktop_e2e_observer::Observer>,
    search: Option<SearchWorker>,
    preferences: crate::editor_preferences::Prefs,
    preferences_writer: Option<crate::editor_preferences::PreferencesWriter>,
    preferences_pending: Option<u64>,
    search_key: Option<(u64, u64, String, SearchOptions)>,
    search_due: Option<Instant>,
    pub(super) language: super::editor_language_app::EditorLanguageHost,
    pub(super) pending_navigation: Option<(PathBuf, openide_language::Position)>,
    pub(super) focus_next: Option<String>,
    /// The selection the floating control waits on, and since when.
    pub(super) selection_seen: Option<((u64, openide_editor::Selection), Instant)>,
    structure: editor_structure_app::EditorStructureHost,
}
impl Default for EditorServices {
    fn default() -> Self {
        Self {
            #[cfg(feature = "editor-e2e")]
            e2e_observer: None,
            search: None,
            preferences: Default::default(),
            preferences_writer: None,
            preferences_pending: None,
            search_key: None,
            search_due: None,
            language: Default::default(),
            pending_navigation: None,
            focus_next: None,
            selection_seen: None,
            structure: Default::default(),
        }
    }
}

impl AgentsWindow {
    pub(super) fn init_editor_preferences(&mut self) {
        let path = self.data_root.join("editor-preferences.json");
        let prefs = crate::editor_preferences::Prefs::load(&path);
        self.view.editor_workbench.soft_wrap = prefs.soft_wrap;
        self.view.editor_workbench.minimap = prefs.minimap;
        self.view.editor_workbench.indent = if prefs.use_tabs {
            IndentStyle::Tabs(prefs.tab_width)
        } else {
            IndentStyle::Spaces(prefs.tab_width)
        };
        self.view.editor_workbench.auto_pairs = prefs.auto_pairs;
        self.editor_services.preferences = prefs;
    }
    pub(super) fn persist_editor_preferences(&mut self, server: bool) {
        let state = &self.view.editor_workbench;
        let prefs = &mut self.editor_services.preferences;
        prefs.soft_wrap = state.soft_wrap;
        prefs.minimap = state.minimap;
        prefs.tab_width = state.indent.width();
        prefs.use_tabs = matches!(state.indent, IndentStyle::Tabs(_));
        prefs.auto_pairs = state.auto_pairs;
        if server {
            if let Some(view) = &self.view.code_editor {
                let language = state
                    .language_override
                    .unwrap_or_else(|| openide_ui::CodeLanguage::from_path(&view.path));
                let entries = prefs
                    .server_executables
                    .entry(self.view.workspace.clone())
                    .or_default();
                if state.server_path.trim().is_empty() {
                    entries.remove(language.lsp_id());
                } else {
                    entries.insert(language.lsp_id().into(), state.server_path.trim().into());
                }
            }
        }
        if self.editor_services.preferences_writer.is_none() {
            self.editor_services.preferences_writer =
                crate::editor_preferences::PreferencesWriter::new(
                    self.data_root.join("editor-preferences.json"),
                )
                .ok();
        }
        if let Some(writer) = &self.editor_services.preferences_writer {
            match writer.submit(prefs.clone()) {
                Ok(sequence) => self.editor_services.preferences_pending = Some(sequence),
                Err(error) => self.view.editor_workbench.error = Some(error),
            }
        } else {
            self.view.editor_workbench.error =
                Some("No se pudieron guardar las preferencias del editor".into());
        }
    }

    pub(super) fn editor_server_executable_preference(&self, language: openide_ui::CodeLanguage) -> Option<String> {
        self.editor_services.preferences.server_executables
            .get(&self.view.workspace)
            .and_then(|languages| languages.get(language.lsp_id()))
            .cloned()
    }

    pub(super) fn editor_preferences_sequence(&self) -> Option<u64> {
        self.editor_services.preferences_pending
    }

    #[cfg(feature = "editor-e2e")]
    pub(crate) fn editor_e2e(root: &std::path::Path) -> Self {
        assert!(
            root.join(".openide-editor-fixture").is_file(),
            "refusing a non-fixture workspace"
        );
        let mut app = Self::with_data_root(root.join("state"));
        app.view.workspace = root
            .join("project")
            .canonicalize()
            .expect("fixture project")
            .to_string_lossy()
            .into_owned();
        app.view.workspace_draft = app.view.workspace.clone();
        app.view.sidebar_collapsed = true;
        app.view.show_settings = false;
        app.view.main_page = MainPage::Workspace;
        app.view.project_branch = None;
        let scope = app.dock_scope();
        let tab = app.dock.open(
            &scope,
            DockTabKind::FilePreview {
                path: "main.rs".into(),
            },
        );
        app.dock.set_collapsed(&scope, false);
        app.dock.set_width_px(&scope.workspace_id, 920.0);
        app.sync_dock_view();
        app.editor_services.focus_next = Some(crate::code_editor_ui::view_id(tab.get()));
        app.prepare_context_e2e(root);
        app.prepare_usage_e2e(root);
        app.prepare_marketplace_e2e(root);
        app.editor_services.e2e_observer = Some(desktop_e2e_observer::Observer::new(root));
        app
    }
    /// Feature-gated desktop fixture: only an explicitly marked, private test
    /// directory and a loopback HTTP server can bootstrap this browser path.
    #[cfg(feature = "editor-e2e")]
    pub(crate) fn browser_e2e(root: &std::path::Path) -> Self {
        let mut app = Self::editor_e2e(root);
        let url = std::fs::read_to_string(root.join("browser-url")).expect("fixture URL");
        let parsed = reqwest::Url::parse(url.trim()).expect("fixture URL");
        assert_eq!(parsed.scheme(), "http");
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert!(parsed.username().is_empty() && parsed.password().is_none());
        let scope = app.dock_scope();
        let tab = app.dock.open(&scope, DockTabKind::Browser);
        app.browser_restart_urls
            .insert(tab.get(), parsed.to_string());
        app.sync_dock_view();
        // The desktop test focuses the viewport through a real pointer click.
        app.editor_services.focus_next = None;
        app
    }
    pub(super) fn active_editor(&self) -> Option<(u64, String, EditorController, BufferSnapshot)> {
        let view = self.view.code_editor.as_ref()?;
        let controller = view.controller.clone()?;
        let snapshot = controller.snapshot().ok()?;
        Some((view.tab_id, view.path.clone(), controller, snapshot))
    }
    pub(super) fn editor_command(
        &mut self,
        command: EditorCommand,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        if self
            .view
            .document_preview
            .as_ref()
            .is_some_and(|view| !view.is_source_visible())
        {
            return Err("Volvé a Código para editar el documento.".into());
        }
        let (tab, _, controller, _) = self.active_editor().ok_or("No hay un archivo abierto")?;
        let snapshot = controller.execute(command)?;
        self.apply_code_editor_action(
            &UiAction::EditorChanged {
                id: crate::code_editor_ui::view_id(tab),
                document_id: snapshot.document_id(),
                revision: snapshot.revision(),
            },
            ctx,
        );
        self.tree
            .reveal_code_caret(&crate::code_editor_ui::view_id(tab));
        ctx.request_repaint();
        Ok(())
    }
    fn editor_error(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.view.editor_workbench.error = Some(error);
        }
    }
    pub(super) fn poll_editor_workbench(&mut self, ctx: &egui::Context) -> bool {
        #[cfg(feature = "editor-e2e")]
        self.observe_editor_e2e(ctx);
        let mut changed = false;
        if let Some(outcome) = self
            .editor_services
            .preferences_writer
            .as_ref()
            .and_then(|writer| writer.poll())
        {
            if self
                .editor_services
                .preferences_pending
                .is_some_and(|sequence| outcome.sequence >= sequence)
            {
                self.editor_services.preferences_pending = None;
            }
            let services = self.editor_services_popover_current()
                && self.view.ide_status.service_preferences_sequence == Some(outcome.sequence);
            if services {
                self.view.ide_status.service_preferences_sequence = None;
            }
            if let Err(error) = outcome.result {
                if services {
                    self.view.ide_status.service_error = Some(error);
                } else {
                    self.view.editor_workbench.error = Some(error);
                }
                changed = true;
            }
        }
        if self.editor_services.preferences_pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(20));
        }
        let Some((tab, path, _, snapshot)) = self.active_editor() else {
            self.editor_services.search_due = None;
            self.editor_services.search_key = None;
            self.editor_services.search = None;
            if self.view.code_editor.is_none() {
                self.editor_services.focus_next = None;
            }
            self.view.editor_workbench.form = None;
            self.view.editor_workbench.search_pending = false;
            return changed | self.poll_editor_language(ctx);
        };
        if self.view.editor_workbench.document_id != Some(snapshot.document_id()) {
            let previous = &self.view.editor_workbench;
            self.view.editor_workbench = EditorWorkbenchView {
                document_id: Some(snapshot.document_id()),
                soft_wrap: previous.soft_wrap,
                minimap: previous.minimap,
                indent: previous.indent,
                auto_pairs: previous.auto_pairs,
                search_open: previous.search_open,
                replace_open: previous.replace_open,
                query: previous.query.clone(),
                replacement: previous.replacement.clone(),
                search_options: SearchOptions {
                    scope: None,
                    ..previous.search_options.clone()
                },
                ..Default::default()
            };
            self.editor_services.search_key = None;
            let language = openide_ui::CodeLanguage::from_path(&path);
            if let Some(executable) = self
                .editor_services
                .preferences
                .server_executables
                .get(&self.view.workspace)
                .and_then(|languages| languages.get(language.lsp_id()))
                .cloned()
            {
                self.view.editor_workbench.server_path = executable.clone();
                let result = self.restart_editor_language(&executable);
                self.editor_error(result);
            }
            changed = true;
        }
        let state = &mut self.view.editor_workbench;
        if state.search_open {
            let key = (
                snapshot.document_id(),
                snapshot.revision(),
                state.query.clone(),
                state.search_options.clone(),
            );
            if self.editor_services.search_key.as_ref() != Some(&key) {
                if let Some(worker) = &mut self.editor_services.search {
                    worker.invalidate();
                }
                self.editor_services.search_key = Some(key);
                self.editor_services.search_due = Some(Instant::now() + Duration::from_millis(70));
                state.search_pending = true;
                state.search = None;
                state.search_error = None;
                changed = true;
            }
            if self
                .editor_services
                .search_due
                .is_some_and(|due| Instant::now() >= due)
            {
                if self.editor_services.search.is_none() {
                    self.editor_services.search = SearchWorker::new().ok();
                }
                if let Some(worker) = &mut self.editor_services.search {
                    worker.submit(
                        snapshot.clone(),
                        state.query.clone(),
                        state.search_options.clone(),
                    );
                } else {
                    state.search_error = Some("No se pudo iniciar la búsqueda".into());
                    state.search_pending = false;
                }
                self.editor_services.search_due = None;
            }
            if let Some(result) = self
                .editor_services
                .search
                .as_ref()
                .and_then(SearchWorker::poll)
            {
                state.search_pending = false;
                match result {
                    Ok(result)
                        if result.document_id == snapshot.document_id()
                            && result.revision == snapshot.revision() =>
                    {
                        state.search = Some(result);
                        state.search_error = None;
                    }
                    Err(error) => state.search_error = Some(error),
                    _ => {}
                }
                changed = true;
            }
            if state.search_pending || self.editor_services.search_due.is_some() {
                ctx.request_repaint_after(Duration::from_millis(16));
            }
        }
        if let Some((target, position)) = self.editor_services.pending_navigation.clone() {
            if target == PathBuf::from(&path) {
                self.reveal_document_preview_source();
                self.editor_services.pending_navigation = None;
                if let Some(offset) = openide_language::utf16_to_offset(&snapshot, position) {
                    let result = self
                        .editor_command(EditorCommand::SetSelection(Selection::caret(offset)), ctx);
                    self.editor_error(result);
                    self.editor_services.focus_next = Some(crate::code_editor_ui::view_id(tab));
                    changed = true;
                }
            }
        }
        if let Some(id) = self.editor_services.focus_next.clone() {
            if self.tree.focus(&id) {
                self.editor_services.focus_next = None;
                changed = true;
            } else {
                ctx.request_repaint();
            }
        }
        changed | self.poll_editor_language(ctx) | self.sync_editor_structure()
    }
    fn editor_search_step(&mut self, previous: bool, ctx: &egui::Context) -> Result<(), String> {
        let (_, _, _, snapshot) = self.active_editor().ok_or("No hay un archivo abierto")?;
        let result = self
            .view
            .editor_workbench
            .search
            .as_ref()
            .filter(|r| {
                r.document_id == snapshot.document_id() && r.revision == snapshot.revision()
            })
            .ok_or("Esperá a que termine la búsqueda")?;
        let matches = &result.matches;
        if matches.is_empty() {
            return Ok(());
        }
        let selection = snapshot.selection().range();
        let index = matches.iter().position(|range| *range == selection);
        let next = if previous {
            index
                .map(|i| (i + matches.len() - 1) % matches.len())
                .unwrap_or_else(|| {
                    matches
                        .iter()
                        .rposition(|r| r.start < selection.start)
                        .unwrap_or(matches.len() - 1)
                })
        } else {
            index.map(|i| (i + 1) % matches.len()).unwrap_or_else(|| {
                matches
                    .iter()
                    .position(|r| r.start >= selection.start)
                    .unwrap_or(0)
            })
        };
        let range = matches[next].clone();
        self.editor_command(
            EditorCommand::SetSelection(Selection {
                anchor: range.start,
                head: range.end,
            }),
            ctx,
        )
    }
    fn open_editor_form(&mut self, form: EditorForm) {
        self.view.editor_workbench.form = Some(form);
        self.view.editor_workbench.form_value = String::new();
        self.tree.set_input_value("editor_form_value", "");
        self.editor_services.focus_next = Some("editor_form_value".into());
    }
    fn apply_editor_form(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let state = &self.view.editor_workbench;
        let value = state.form_value.trim().to_owned();
        let result = match state.form {
            Some(EditorForm::NewFile) => {
                self.code_editor_create(PathBuf::from(value), String::new())
            }
            Some(EditorForm::SaveAs) => self.code_editor_save_as(PathBuf::from(value)),
            Some(EditorForm::Rename) => self.editor_language_request("rename", Some(value)),
            Some(EditorForm::Snippet) => self.editor_command(
                EditorCommand::InsertSnippet(self.view.editor_workbench.form_value.clone()),
                ctx,
            ),
            Some(EditorForm::Goto) => {
                let mut parts = value.split(':');
                let row = parts
                    .next()
                    .unwrap_or("")
                    .parse::<usize>()
                    .ok()
                    .filter(|r| *r > 0)
                    .ok_or("Ingresá una línea mayor que cero")?;
                let column = parts
                    .next()
                    .map_or(Ok(1), str::parse::<usize>)
                    .map_err(|_| "Columna inválida")?;
                if column == 0 || parts.next().is_some() {
                    return Err("Usá línea:columna, empezando en 1".into());
                }
                self.editor_command(
                    EditorCommand::Goto(Position {
                        row: row - 1,
                        column: column - 1,
                    }),
                    ctx,
                )
            }
            None => Ok(()),
        };
        if result.is_ok() {
            self.view.editor_workbench.form = None;
            self.focus_active_editor();
        }
        result
    }
    fn focus_active_editor(&mut self) {
        if let Some(view) = &self.view.code_editor {
            self.editor_services.focus_next = Some(crate::code_editor_ui::view_id(view.tab_id));
        }
    }
    pub(super) fn apply_editor_workbench_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        // A preview keeps its source buffer alive, but stale editor controls
        // must not mutate that buffer while it is absent from the UI.
        if self
            .view
            .document_preview
            .as_ref()
            .is_some_and(|view| !view.is_source_visible())
        {
            let id = match action {
                UiAction::Clicked { id }
                | UiAction::InputChanged { id, .. }
                | UiAction::Submitted { id, .. } => Some(id.as_str()),
                _ => None,
            };
            if id.is_some_and(|id| id.starts_with("editor_") || id.starts_with("code_editor_path:"))
            {
                return true;
            }
        }
        if let UiAction::Clicked { id } = action {
            if self.apply_editor_structure_action(id, ctx) {
                return true;
            }
            if id.starts_with("code_editor_path:") {
                let Some(editor) = &self.view.code_editor else {
                    return true;
                };
                let prefix = format!("code_editor_path:{}:", editor.tab_id);
                if id == &format!("{prefix}overflow_toggle") {
                    self.view.editor_workbench.breadcrumb_open =
                        !self.view.editor_workbench.breadcrumb_open;
                } else if id == &format!("{prefix}overflow_dismiss") {
                    self.view.editor_workbench.breadcrumb_open = false;
                }
                ctx.request_repaint();
                return true;
            }
        }
        match action {
            UiAction::InputChanged { id, value } if id.starts_with("editor_") => {
                match id.as_str() {
                    "editor_query" => self.view.editor_workbench.query = value.clone(),
                    "editor_replacement" => self.view.editor_workbench.replacement = value.clone(),
                    "editor_form_value" => self.view.editor_workbench.form_value = value.clone(),
                    "editor_server_path" => self.view.editor_workbench.server_path = value.clone(),
                    _ => return false,
                }
                return true;
            }
            UiAction::Submitted { id, .. } if id == "editor_query" => {
                let result = self.editor_search_step(false, ctx);
                self.view.editor_workbench.search_error = result.err();
                return true;
            }
            UiAction::Submitted { id, .. } if id == "editor_replacement" => {
                // Enter follows the same availability as the replace button:
                // only a selected match from the current search can be edited.
                let state = &self.view.editor_workbench;
                let can_replace = !state.search_pending
                    && self.active_editor().is_some_and(|active| {
                        state.search.as_ref().is_some_and(|results| {
                            results.document_id == active.3.document_id()
                                && results.revision == active.3.revision()
                                && results.matches.contains(&active.3.selection().range())
                        })
                    });
                if can_replace {
                    self.apply_editor_workbench_action(
                        &UiAction::Clicked {
                            id: "editor_replace_one".into(),
                        },
                        ctx,
                    );
                    self.editor_services.focus_next = (!self.tree.focus("editor_replacement"))
                        .then(|| "editor_replacement".into());
                }
                return true;
            }
            UiAction::Submitted { id, .. } if id == "editor_form_value" => {
                let result = self.apply_editor_form(ctx);
                self.editor_error(result);
                return true;
            }
            _ => {}
        }
        let UiAction::Clicked { id } = action else {
            return false;
        };
        let Some(command) = id.strip_prefix("editor_") else {
            return false;
        };
        if command == "add_selection" {
            self.add_editor_selection_to_chat();
            return true;
        }
        self.view.editor_workbench.error = None;
        if command != "menu" {
            self.view.editor_workbench.menu_open = false;
        }
        self.view.editor_workbench.breadcrumb_open = false;
        let active = self.active_editor();
        let state = &mut self.view.editor_workbench;
        let edit = match command {
            "menu" => {
                state.menu_open = !state.menu_open;
                None
            }
            "menu_close" => {
                state.menu_open = false;
                None
            }
            "find" | "replace" => {
                state.form = None;
                state.settings_open = false;
                state.search_open = true;
                state.replace_open = command == "replace";
                self.editor_services.focus_next = Some("editor_query".into());
                None
            }
            "replace_toggle" => {
                state.replace_open = !state.replace_open;
                self.editor_services.focus_next = Some(
                    if state.replace_open && !state.query.is_empty() {
                        "editor_replacement"
                    } else {
                        "editor_query"
                    }
                    .into(),
                );
                None
            }
            "search_close" => {
                state.search_open = false;
                self.focus_active_editor();
                None
            }
            "search_next" | "search_prev" => {
                let result = self.editor_search_step(command == "search_prev", ctx);
                self.view.editor_workbench.search_error = result.err();
                // Pointer navigation should leave the query ready for typing and
                // Enter / Shift+Enter, just like navigation from the input.
                self.editor_services.focus_next =
                    (!self.tree.focus("editor_query")).then(|| "editor_query".into());
                None
            }
            "case" => {
                state.search_options.case_sensitive = !state.search_options.case_sensitive;
                None
            }
            "word" => {
                state.search_options.whole_word = !state.search_options.whole_word;
                None
            }
            "regex" => {
                state.search_options.regex = !state.search_options.regex;
                None
            }
            "scope" => {
                state.search_options.scope = if state.search_options.scope.is_some() {
                    None
                } else {
                    active
                        .as_ref()
                        .map(|a| a.3.selection().range())
                        .filter(|r| !r.is_empty())
                };
                None
            }
            "replace_all" => Some(EditorCommand::ReplaceAll {
                query: state.query.clone(),
                replacement: state.replacement.clone(),
                options: state.search_options.clone(),
            }),
            "replace_one" => active.as_ref().map(|a| EditorCommand::ReplaceMatch {
                query: state.query.clone(),
                replacement: state.replacement.clone(),
                options: state.search_options.clone(),
                range: a.3.selection().range(),
            }),
            "wrap" => {
                state.soft_wrap = !state.soft_wrap;
                None
            }
            "minimap" => {
                state.minimap = !state.minimap;
                None
            }
            "settings" => {
                state.settings_open = !state.settings_open;
                None
            }
            "outline" => {
                state.outline_open = !state.outline_open;
                None
            }
            "indent_kind" => {
                state.indent = match state.indent {
                    IndentStyle::Spaces(n) => IndentStyle::Tabs(n),
                    IndentStyle::Tabs(n) => IndentStyle::Spaces(n),
                };
                None
            }
            "indent_width" => {
                let n = match state.indent.width() {
                    2 => 4,
                    4 => 8,
                    _ => 2,
                };
                state.indent = match state.indent {
                    IndentStyle::Spaces(_) => IndentStyle::Spaces(n),
                    IndentStyle::Tabs(_) => IndentStyle::Tabs(n),
                };
                None
            }
            "pairs" => {
                state.auto_pairs = !state.auto_pairs;
                None
            }
            "language" => {
                state.language_menu_open = !state.language_menu_open;
                None
            }
            "language_close" => {
                state.language_menu_open = false;
                None
            }
            _ if command.starts_with("language:") => {
                state.language_override = match &command[9..] {
                    "rust" => Some(CodeLanguage::Rust),
                    "javascript" => Some(CodeLanguage::JavaScript),
                    "typescript" => Some(CodeLanguage::TypeScript),
                    "tsx" => Some(CodeLanguage::Tsx),
                    "python" => Some(CodeLanguage::Python),
                    "json" => Some(CodeLanguage::Json),
                    "toml" => Some(CodeLanguage::Toml),
                    "css" => Some(CodeLanguage::Css),
                    "html" => Some(CodeLanguage::Html),
                    "markdown" => Some(CodeLanguage::Markdown),
                    "plain" => Some(CodeLanguage::Plain),
                    _ => None,
                };
                state.language_menu_open = false;
                None
            }
            "server_connect" => {
                let path = state.server_path.clone();
                let result = self.restart_editor_language(&path);
                self.editor_error(result);
                None
            }
            "eol" => active.as_ref().map(|a| {
                EditorCommand::SetLineEnding(if a.3.preferred_line_ending() == LineEnding::CrLf {
                    LineEnding::Lf
                } else {
                    LineEnding::CrLf
                })
            }),
            "snippet" => {
                self.open_editor_form(EditorForm::Snippet);
                None
            }
            "new" => {
                self.open_editor_form(EditorForm::NewFile);
                None
            }
            "save_as" => {
                self.open_editor_form(EditorForm::SaveAs);
                None
            }
            "goto" => {
                self.open_editor_form(EditorForm::Goto);
                None
            }
            "rename" => {
                self.open_editor_form(EditorForm::Rename);
                None
            }
            "form_cancel" => {
                state.form = None;
                self.focus_active_editor();
                None
            }
            "form_apply" => {
                let result = self.apply_editor_form(ctx);
                self.editor_error(result);
                None
            }
            "undo" => Some(EditorCommand::Undo),
            "redo" => Some(EditorCommand::Redo),
            "duplicate" => Some(EditorCommand::DuplicateLines),
            "move_up" => Some(EditorCommand::MoveLines { down: false }),
            "move_down" => Some(EditorCommand::MoveLines { down: true }),
            "delete_lines" => Some(EditorCommand::DeleteLines),
            "comment" => active.as_ref().map(|a| {
                let (prefix, suffix) = state
                    .language_override
                    .unwrap_or_else(|| openide_ui::CodeLanguage::from_path(&a.1))
                    .comment_markers_for(&a.1);
                EditorCommand::ToggleLineComment {
                    prefix: prefix.into(),
                    suffix: suffix.into(),
                }
            }),
            "select_next" | "select_all_matches" => Some(EditorCommand::SelectNextOccurrence {
                all: command == "select_all_matches",
            }),
            "cursor_above" | "cursor_below" => Some(EditorCommand::AddCursorVertical {
                below: command == "cursor_below",
                tab_width: state.indent.width(),
            }),
            "fold" => {
                let target = if let (Some(analysis), Some((_, _, _, snapshot))) =
                    (&state.analysis, &active)
                {
                    if analysis.document_id == snapshot.document_id()
                        && analysis.revision == snapshot.revision()
                    {
                        let row = snapshot
                            .position_of(snapshot.selection().head)
                            .map_or(0, |p| p.row);
                        analysis
                            .folds
                            .iter()
                            .filter(|f| f.rows.contains(&row))
                            .min_by_key(|f| f.rows.len())
                            .map(|f| f.rows.clone())
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(fold) = target {
                    let mut folds = state.folds.to_vec();
                    if !folds.contains(&fold) {
                        folds.push(fold.clone());
                        state.folds = folds.into();
                    }
                    Some(EditorCommand::Goto(Position {
                        row: fold.start,
                        column: 0,
                    }))
                } else {
                    state.notice = Some("No hay un bloque plegable en esta línea".into());
                    None
                }
            }
            "unfold" => {
                state.folds = Arc::from([]);
                None
            }
            "conflict" => {
                if let Some(conflict) = self.code_editor_conflict() {
                    self.view.editor_workbench.conflict_disk = conflict.disk;
                    self.view.editor_workbench.conflict_open = true;
                }
                None
            }
            "conflict_close" => {
                state.conflict_open = false;
                state.conflict_disk = None;
                None
            }
            "copy_path" | "copy_relative" => {
                if let Some((_, path, _, _)) = &active {
                    ctx.copy_text(if command == "copy_path" {
                        PathBuf::from(&self.view.workspace)
                            .join(path)
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        path.clone()
                    });
                }
                None
            }
            "completion" | "hover" | "definition" | "references" | "format" | "code_actions" => {
                let result = self.editor_language_request(command, None);
                self.editor_error(result);
                None
            }
            "diagnostics" => {
                self.show_editor_diagnostics();
                None
            }
            "results_close" => {
                state.results.clear();
                state.hover = None;
                None
            }
            _ if command.starts_with("symbol:") => {
                let index = command[7..].parse::<usize>().ok();
                index
                    .and_then(|i| {
                        let analysis = state.analysis.as_ref()?;
                        let snapshot = &active.as_ref()?.3;
                        if (analysis.document_id, analysis.revision)
                            != (snapshot.document_id(), snapshot.revision())
                        {
                            return None;
                        }
                        analysis.symbols.get(i)
                    })
                    .map(|s| EditorCommand::SetSelection(Selection::caret(s.selection.start)))
            }
            _ if command.starts_with("result:") => {
                let result = self.apply_editor_language_result(&command[7..], ctx);
                self.editor_error(result);
                None
            }
            _ if command.starts_with("folder:") => {
                let path = PathBuf::from(&command[7..]);
                let result = self.reveal_editor_folder(&path);
                self.editor_error(result);
                None
            }
            _ => return false,
        };
        if let Some(edit) = edit {
            let result = self.editor_command(edit, ctx);
            if matches!(command, "replace_one" | "replace_all") {
                self.view.editor_workbench.search_error = result.err();
            } else {
                self.editor_error(result);
            }
        }
        if matches!(
            command,
            "wrap" | "minimap" | "indent_kind" | "indent_width" | "pairs" | "server_connect"
        ) {
            self.persist_editor_preferences(command == "server_connect");
        }
        ctx.request_repaint();
        true
    }
    pub(super) fn reveal_editor_folder(&mut self, path: &std::path::Path) -> Result<(), String> {
        if path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err("Ruta fuera del proyecto".into());
        }
        self.refresh_file_rows();
        if let Some(explorer) = &mut self.file_explorer {
            explorer.reveal(path).map_err(|e| e.to_string())?;
        }
        if self.ide_workspace_active() {
            self.open_ide_explorer();
        } else {
            let scope = self.dock_scope();
            self.dock.open(&scope, DockTabKind::Files);
            self.sync_dock_view();
            self.refresh_file_rows();
        }
        Ok(())
    }
    pub(super) fn handle_editor_workbench_key(
        &mut self,
        event: &UiEvent,
        ctx: &egui::Context,
    ) -> bool {
        if matches!(
            event,
            UiEvent::Key {
                key: Key::S,
                ctrl: true,
                shift: false
            }
        ) && self.view.main_page == MainPage::Workspace
            && !self.view.show_settings
            && self.resource_pane_visible()
            && self.view.side_tab == SideTab::FilePreview
            && self
                .tree
                .focused_id()
                .is_none_or(|id| id.starts_with("document_preview:") || id.starts_with("document_preview_view:"))
        {
            if let Some(tab) = self
                .view
                .document_preview
                .as_ref()
                .filter(|view| {
                    view.kind == openide_preview::PreviewKind::Markdown && !view.is_source_visible()
                })
                .map(|view| view.scope.tab_id)
            {
                // Save remains meaningful in preview; it must not move focus
                // to the source editor that is currently absent from the tree.
                return self.apply_code_editor_action(
                    &UiAction::Clicked {
                        id: format!("code_editor_save:{tab}"),
                    },
                    ctx,
                );
            }
        }
        if let UiEvent::PointerDownEditor {
            position,
            ctrl: true,
            alt: false,
            ..
        } = event
        {
            if self
                .tree
                .frame()
                .hit_test(*position)
                .is_some_and(|id| id.starts_with("code_editor:"))
            {
                let actions = self.tree.handle_event(event.clone());
                for action in actions {
                    self.apply_action(action, ctx);
                }
                let result = self.editor_language_request("definition", None);
                self.editor_error(result);
                return true;
            }
        }
        let UiEvent::Key { key, shift, ctrl } = event else {
            return false;
        };
        // Modal movement owns Ctrl+D/F/etc. before modeless editor commands.
        // Escape still dismisses an open search/results surface first.
        if *key != Key::Escape && !*shift && self.tree.focused_code_editor_vim_consumes(*key, *ctrl) {
            return false;
        }
        if *key == Key::F9 && self.view.ide.menu_open.is_some() {
            return false;
        }
        let focused = self.tree.focused_id().unwrap_or("");
        let active_id = self
            .view
            .code_editor
            .as_ref()
            .map(|view| crate::code_editor_ui::view_id(view.tab_id));
        let editable = active_id.as_deref() == Some(focused);
        let input = matches!(
            focused,
            "editor_query" | "editor_replacement" | "editor_form_value" | "editor_server_path"
        );
        let search_control = self.view.editor_workbench.search_open
            && (focused.starts_with("editor_search_")
                || matches!(
                    focused,
                    "editor_query"
                        | "editor_replacement"
                        | "editor_case"
                        | "editor_word"
                        | "editor_regex"
                        | "editor_scope"
                        | "editor_replace_toggle"
                        | "editor_replace_one"
                        | "editor_replace_all"
                ));
        if !editable && !input && !search_control {
            return false;
        }
        if !editable && !matches!(key, Key::Escape | Key::Enter | Key::F | Key::H | Key::G) {
            return false;
        }
        let command = match (key, shift, ctrl) {
            (Key::F9, false, false) => "breakpoint",
            (Key::F12, false, _) => "definition",
            (Key::F12, true, _) => "references",
            (Key::F2, _, _) => "rename",
            (Key::F, _, true) => "find",
            (Key::H, _, true) => "replace",
            (Key::G, _, true) => "goto",
            (Key::D, false, true) => "select_next",
            (Key::L, true, true) => "select_all_matches",
            (Key::L, false, true) => "add_selection",
            (Key::K, true, true) => "delete_lines",
            (Key::Space, _, true) => "completion",
            (Key::Escape, _, _) if search_control => "search_close",
            (Key::Escape, _, _) if self.view.editor_workbench.form.is_some() => "form_cancel",
            (Key::Escape, _, _)
                if !self.view.editor_workbench.results.is_empty()
                    || self.view.editor_workbench.hover.is_some() =>
            {
                "results_close"
            }
            (Key::Escape, _, _) if self.view.editor_workbench.search_open => "search_close",
            (Key::Enter, true, false) if focused == "editor_query" => "search_prev",
            _ => return false,
        };
        self.apply_editor_workbench_action(
            &UiAction::Clicked {
                id: format!("editor_{command}"),
            },
            ctx,
        )
    }
}

#[cfg(test)]
mod search_worker_tests {
    use super::*;
    #[test]
    fn invalidating_query_rejects_in_flight_result_before_next_submit() {
        let mut worker = SearchWorker::new().unwrap();
        worker.generation = 41;
        worker.invalidate();
        // An old worker can finish after invalidation but before the new debounce.
        worker.slot.0.lock().unwrap().result = Some((41, Err("stale query".into())));
        assert!(worker.poll().is_none());
        worker.slot.0.lock().unwrap().result = Some((42, Err("current query".into())));
        assert_eq!(worker.poll().unwrap().unwrap_err(), "current query");
    }
}
