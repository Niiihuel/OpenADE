//! DAP ownership, asynchronous configuration IO and revision-scoped projection.
use super::*;
use crate::ide_workspace::{IdeActivity, IdeBottomPanel, IdePanel};
use crate::{debug_config, debug_ui::*};
use openide_debug::{
    DebugCommand, DebugEvent, DebugSession, DebugState, SourceBreakpoints, StackFrame,
};
use openide_ui::{CodeBreakpoint, CodeBreakpointState, CodeExecutionLine};
use std::sync::mpsc::{self, Receiver};

struct ConfigResult {
    workspace: String,
    result: Result<Vec<debug_config::Configuration>, String>,
    open_file: bool,
}
struct VariableNode {
    view: DebugVariableView,
    reference: i64,
    children: Vec<String>,
    /// Frame, scope and names down to this variable: what a value is compared
    /// by across stops, where adapter references do not survive.
    path: String,
    /// The adapter had more children than the page that was asked for.
    more: bool,
}

#[derive(Default)]
pub(super) struct DebugHost {
    workspace: String,
    epoch: u64,
    inspection: u64,
    stop_id: u64,
    session: Option<DebugSession>,
    config_job: Option<Receiver<ConfigResult>>,
    configurations: Vec<debug_config::Configuration>,
    selected: usize,
    sources: BTreeMap<PathBuf, Vec<u32>>,
    verified: BTreeMap<PathBuf, Vec<openide_debug::Breakpoint>>,
    breakpoint_generation: u64,
    thread: Option<i64>,
    frame: Option<i64>,
    frames: Vec<StackFrame>,
    stack_request: Option<u64>,
    scope_request: Option<u64>,
    variable_requests: BTreeMap<u64, String>,
    control_request: Option<u64>,
    breakpoint_requests: BTreeMap<PathBuf, u64>,
    output_dirty: bool,
    close_after_stop: bool,
    restart_after_stop: bool,
    /// Values of the previous stop by `VariableNode::path`.
    previous_values: BTreeMap<String, String>,
    nodes: BTreeMap<String, VariableNode>,
    roots: Vec<String>,
    output: VecDeque<DebugOutputView>,
    projection: Option<(u64, u64, u64, u64, bool)>,
    pending_navigation: Option<(String, PathBuf, openide_language::Position)>,
}

fn short(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.into();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
fn source_relative(workspace: &str, path: &std::path::Path) -> Option<PathBuf> {
    let path = if path.is_absolute() {
        path.strip_prefix(workspace).ok()?
    } else {
        path
    };
    (!path.as_os_str().is_empty()
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_))))
    .then(|| path.to_path_buf())
}

impl AgentsWindow {
    /// Navigation from a frame or breakpoint can outlive asynchronous file IO.
    /// Cancel only our still-pending target before the editor consumes it in a
    /// different workspace; a replacement issued by another view is preserved.
    pub(super) fn guard_debug_navigation(&mut self) -> bool {
        let Some((workspace, path, position)) = self.debugger.pending_navigation.as_ref() else {
            return false;
        };
        let same = self
            .editor_services
            .pending_navigation
            .as_ref()
            .is_some_and(|(current_path, current_position)| {
                current_path == path
                    && current_position.line == position.line
                    && current_position.character == position.character
            });
        if !same {
            self.debugger.pending_navigation = None;
            return false;
        }
        if workspace != &self.view.workspace {
            self.editor_services.pending_navigation = None;
            self.debugger.pending_navigation = None;
            return true;
        }
        false
    }

    fn queue_debug_navigation(&mut self, path: PathBuf, position: openide_language::Position) {
        self.debugger.pending_navigation =
            Some((self.view.workspace.clone(), path.clone(), position));
        self.editor_services.pending_navigation = Some((path, position));
    }

    pub(super) fn guard_debugger_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|input| input.viewport().close_requested())
            && (!self.view.code_editor_quit_confirm || self.code_editor.close_authorized())
            && self.debugger.session.is_some()
        {
            self.request_close_after_debugger(ctx);
        }
    }

    /// An authorized editor decision can finish after the viewport close
    /// guard ran. Gate its programmatic Close here as well as native events.
    pub(super) fn request_close_after_debugger(&mut self, ctx: &egui::Context) {
        if self.debugger.session.is_some() {
            // Keep the event loop alive until the worker has disconnected and
            // reaped its process; no process wait runs on the UI thread.
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.debugger.close_after_stop = true;
            self.debug_operation("stop");
            ctx.request_repaint_after(Duration::from_millis(24));
        } else {
            self.debugger.close_after_stop = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    pub(super) fn cancel_close_after_debugger(&mut self) {
        self.debugger.close_after_stop = false;
    }
    fn reset_debug_workspace(&mut self) {
        if self.debugger.workspace == self.view.workspace {
            return;
        }
        self.guard_debug_navigation();
        let epoch = self.debugger.epoch.wrapping_add(1);
        // Dropping a worker asks it to disconnect and reap off the UI thread.
        self.debugger = DebugHost {
            workspace: self.view.workspace.clone(),
            epoch,
            ..Default::default()
        };
        self.view.debug = DebugView::default();
        self.sync_debug_scope();
        if self.view.debug_open && self.ide_workspace_active() {
            self.load_debug_config(false);
        }
    }
    fn sync_debug_scope(&mut self) {
        self.view.debug.scope = serde_json::to_string(&(
            &self.debugger.workspace,
            self.debugger.epoch,
            self.debugger.stop_id,
            self.debugger.inspection,
        ))
        .unwrap();
    }
    fn load_debug_config(&mut self, open_file: bool) {
        if self.debugger.config_job.is_some() || self.debugger.session.is_some() {
            return;
        }
        let workspace = self.view.workspace.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        self.debugger.config_job = Some(rx);
        self.view.debug.message = Some("Leyendo .openide/debug.json…".into());
        let spawned = std::thread::Builder::new()
            .name("debug-configuration".into())
            .spawn(move || {
                let root = PathBuf::from(&workspace);
                let result = if open_file {
                    debug_config::ensure_template(&root)
                } else {
                    Ok(())
                }
                .and_then(|()| debug_config::read(&root));
                let _ = tx.send(ConfigResult {
                    workspace,
                    result,
                    open_file,
                });
            });
        if let Err(error) = spawned {
            self.debugger.config_job = None;
            self.view.debug.message = Some(error.to_string());
        }
    }
    fn open_debug_panel(&mut self) {
        if !self.ide_workspace_active() {
            return;
        }
        self.reset_debug_workspace();
        self.select_ide_activity(IdeActivity::Debug, false);
        self.view.debug_open = true;
        self.view.show_dock_add_menu = false;
        self.view.debug.breakpoints = self.debug_breakpoint_views().into();
        if self.debugger.configurations.is_empty() {
            self.load_debug_config(false);
        }
        self.refresh_debug_controls();
    }
    fn hide_debug_panel(&mut self) {
        self.view.debug_open = false;
        self.view.debug.configuration_menu = false;
        // Its answers are scoped to the open panel: left open, it could not close.
        self.view.debug.confirm_clear_breakpoints = false;
        if self.view.ide.activity == IdeActivity::Debug {
            self.view.ide.explorer_open = false;
            if self.view.ide.compact_panel == Some(IdePanel::Explorer) {
                self.view.ide.compact_panel = None;
            }
            self.persist_ide_workspace();
        }
    }
    fn debug_sidebar_visible(&self) -> bool {
        self.ide_workspace_active()
            && self.view.ide.activity == IdeActivity::Debug
            && (self.view.ide.explorer_open
                || self.view.ide.compact_panel == Some(IdePanel::Explorer))
    }
    fn reveal_debug_console_after_start(&mut self, operation: &str, ctx: &egui::Context) {
        if operation == "start"
            && self.view.debug.status == DebugStatus::Starting
            && self.debugger.session.is_some()
        {
            self.open_ide_bottom(IdeBottomPanel::DebugConsole, ctx);
        }
    }
    fn refresh_debug_controls(&mut self) {
        let state = self.view.debug.status;
        let running = state == DebugStatus::Running;
        let stopped = state == DebugStatus::Stopped
            && self.debugger.thread.is_some()
            && self.debugger.control_request.is_none();
        let active = self.debugger.session.is_some();
        self.view.debug.controls = DebugControls {
            start: !active
                && self.debugger.config_job.is_none()
                && !self.debugger.configurations.is_empty()
                && !self.code_editor.has_pending_changes(),
            resume: stopped,
            pause: running && self.debugger.thread.is_some(),
            step_over: stopped,
            step_into: stopped,
            step_out: stopped,
            restart: active && !matches!(state, DebugStatus::Stopping),
            stop: active && !matches!(state, DebugStatus::Stopping),
        };
    }
    fn debug_send(&mut self, command: DebugCommand) -> Option<u64> {
        match self.debugger.session.as_ref()?.send(command) {
            Ok(id) => Some(id),
            Err(error) => {
                self.view.debug.message = Some(error.to_string());
                None
            }
        }
    }
    fn clear_debug_inspection(&mut self) {
        if !self.debugger.nodes.is_empty() {
            self.debugger.previous_values = self
                .debugger
                .nodes
                .values()
                .map(|node| (node.path.clone(), node.view.value.clone()))
                .collect();
        }
        self.debugger.inspection = self.debugger.inspection.wrapping_add(1);
        self.debugger.frame = None;
        self.debugger.frames.clear();
        self.debugger.stack_request = None;
        self.debugger.scope_request = None;
        self.debugger.variable_requests.clear();
        self.debugger.nodes.clear();
        self.debugger.roots.clear();
        self.debugger.projection = None;
        self.view.debug.frames = Arc::from([]);
        self.view.debug.variables = Arc::from([]);
        self.sync_debug_scope();
    }
    fn select_debug_thread(&mut self, thread: i64) {
        self.clear_debug_inspection();
        self.debugger.thread = Some(thread);
        let mut threads = self.view.debug.threads.to_vec();
        for t in &mut threads {
            t.selected = t.id == thread;
        }
        self.view.debug.threads = threads.into();
        self.debugger.stack_request = self.debug_send(DebugCommand::StackTrace {
            thread_id: thread,
            start_frame: 0,
            levels: PAGE as u32,
        });
    }
    fn select_debug_frame(&mut self, frame: i64) {
        if !self.debugger.frames.iter().any(|f| f.id == frame) {
            return;
        }
        self.debugger.inspection = self.debugger.inspection.wrapping_add(1);
        self.debugger.frame = Some(frame);
        self.debugger.nodes.clear();
        self.debugger.roots.clear();
        self.debugger.variable_requests.clear();
        self.debugger.projection = None;
        self.view.debug.variables = Arc::from([]);
        self.sync_debug_scope();
        self.view.debug.frames = self
            .debugger
            .frames
            .iter()
            .map(|f| DebugFrameView {
                id: f.id,
                name: short(&f.name, 4096),
                path: f
                    .source
                    .as_ref()
                    .and_then(|s| s.path.as_ref())
                    .map(|p| p.to_string_lossy().into_owned()),
                line: Some(f.line as u64),
                selected: f.id == frame,
            })
            .collect::<Vec<_>>()
            .into();
        self.debugger.scope_request = self.debug_send(DebugCommand::Scopes { frame_id: frame });
        self.reveal_debug_frame(frame);
    }
    /// Opens the frame's source at its line. Inspection is left as it is.
    fn reveal_debug_frame(&mut self, frame: i64) {
        // A running IDE debug session survives switching to Agents, but its
        // asynchronous stops must not replace the file or selection in chat.
        if !self.ide_workspace_active() {
            return;
        }
        let Some(selected) = self.debugger.frames.iter().find(|f| f.id == frame).cloned() else {
            return;
        };
        if let Some(path) = selected
            .source
            .and_then(|source| source.path)
            .and_then(|path| source_relative(&self.view.workspace, &path))
        {
            let scope = self.dock_scope();
            self.dock
                .open(&scope, DockTabKind::FilePreview { path: path.clone() });
            self.dock.set_collapsed(&scope, false);
            self.sync_dock_view();
            self.queue_debug_navigation(
                path,
                openide_language::Position {
                    line: selected.line.saturating_sub(1),
                    character: 0,
                },
            );
        }
    }
    fn select_debug_breakpoint(&mut self, path: &str, line: u32) {
        if line == 0
            || !self
                .debug_sidebar_breakpoints()
                .iter()
                .any(|marker| marker.path == path && marker.line == line && !marker.dirty)
        {
            return;
        }
        let Some(path) = source_relative(&self.view.workspace, std::path::Path::new(path)) else {
            return;
        };
        let scope = self.dock_scope();
        self.dock
            .open(&scope, DockTabKind::FilePreview { path: path.clone() });
        self.dock.set_collapsed(&scope, false);
        self.sync_dock_view();
        self.queue_debug_navigation(
            path,
            openide_language::Position {
                line: line - 1,
                character: 0,
            },
        );
        if self.view.ide.compact_panel == Some(IdePanel::Explorer) {
            self.view.ide.compact_panel = None;
        }
    }
    fn refresh_debug_variables(&mut self) {
        fn visit(
            key: &str,
            nodes: &BTreeMap<String, VariableNode>,
            rows: &mut Vec<DebugVariableView>,
            depth: usize,
        ) {
            if rows.len() >= 1024 || depth > 8 {
                return;
            }
            if let Some(node) = nodes.get(key) {
                rows.push(node.view.clone());
                if node.view.expanded {
                    for child in &node.children {
                        visit(child, nodes, rows, depth + 1);
                    }
                    if node.more {
                        rows.push(DebugVariableView {
                            key: format!("{key}/more"),
                            name: format!("Se muestran los primeros {PAGE}"),
                            depth: node.view.depth + 1,
                            note: true,
                            ..Default::default()
                        });
                    }
                }
            }
        }
        let mut rows = Vec::new();
        for key in &self.debugger.roots {
            visit(key, &self.debugger.nodes, &mut rows, 0);
        }
        self.view.debug.variables = rows.into();
    }
    fn load_debug_variables(&mut self, key: &str) {
        let Some(node) = self.debugger.nodes.get_mut(key) else {
            return;
        };
        if node.reference <= 0 || node.view.depth >= 8 || node.view.loading {
            return;
        }
        let reference = node.reference;
        node.view.expanded = true;
        node.view.error = None;
        node.view.loading = true;
        if let Some(request) = self.debug_send(DebugCommand::Variables {
            variables_reference: reference,
            start: 0,
            count: PAGE as u32,
        }) {
            self.debugger.variable_requests.insert(request, key.into());
        } else if let Some(node) = self.debugger.nodes.get_mut(key) {
            node.view.loading = false;
        }
    }
    fn start_debugger(&mut self) {
        self.refresh_debug_controls();
        if !self.view.debug.controls.start {
            if self.code_editor.has_pending_changes() {
                self.view.debug.message =
                    Some("Guarda los archivos abiertos antes de iniciar la depuración".into());
            }
            return;
        }
        let Some(configuration) = self
            .debugger
            .configurations
            .get(self.debugger.selected)
            .cloned()
        else {
            return;
        };
        self.debugger.epoch = self.debugger.epoch.wrapping_add(1);
        self.debugger.stop_id = 0;
        self.clear_debug_inspection();
        self.debugger.verified.clear();
        self.debugger.breakpoint_requests.clear();
        self.debugger.control_request = None;
        self.debugger.output.clear();
        self.view.debug.output = Arc::from([]);
        self.view.debug.threads = Arc::from([]);
        self.view.debug.message = None;
        self.view.debug.stop_reason = None;
        self.debugger.previous_values.clear();
        self.debugger.sources = self
            .debug_source_breakpoints()
            .into_iter()
            .map(|s| (s.path, s.lines))
            .collect();
        let mut config = configuration.launch(&self.view.workspace);
        config.breakpoints = self
            .debugger
            .sources
            .iter()
            .map(|(path, lines)| SourceBreakpoints {
                path: path.clone(),
                lines: lines.clone(),
            })
            .collect();
        match DebugSession::start(config) {
            Ok(session) => {
                self.debugger.session = Some(session);
                self.view.debug.status = DebugStatus::Starting;
            }
            Err(error) => {
                self.view.debug.status = DebugStatus::Failed;
                self.view.debug.message = Some(error.to_string());
            }
        }
        self.sync_debug_scope();
        self.refresh_debug_controls();
    }
    fn handle_debug_event(&mut self, event: DebugEvent) {
        match event {
            DebugEvent::State(state) => {
                self.view.debug.status = match state {
                    DebugState::Starting | DebugState::Initializing | DebugState::Configuring => {
                        DebugStatus::Starting
                    }
                    DebugState::Running => DebugStatus::Running,
                    DebugState::Stopped => DebugStatus::Stopped,
                    DebugState::Stopping => DebugStatus::Stopping,
                    DebugState::Disconnected => self.view.debug.status,
                };
                if state == DebugState::Running {
                    self.debugger.control_request = None;
                    self.clear_debug_inspection();
                    if self.debugger.thread.is_none() {
                        self.debug_send(DebugCommand::Threads);
                    }
                }
            }
            DebugEvent::Stopped {
                stop_id,
                reason,
                description,
                thread_id,
                ..
            } => {
                self.view.debug.status = DebugStatus::Stopped;
                self.debugger.stop_id = stop_id;
                self.debugger.control_request = None;
                self.view.debug.message = None;
                self.view.debug.stop_reason = Some(description.unwrap_or_else(|| {
                    match reason.as_str() {
                        "breakpoint" => "punto de interrupción",
                        "step" => "paso",
                        "pause" => "pausa manual",
                        "exception" => "excepción",
                        "entry" => "entrada del programa",
                        other => other,
                    }
                    .to_owned()
                }));
                self.clear_debug_inspection();
                self.debugger.thread = thread_id;
                self.debug_send(DebugCommand::Threads);
                if let Some(thread) = thread_id {
                    self.select_debug_thread(thread);
                }
            }
            DebugEvent::Continued { .. } => {
                self.debugger.control_request = None;
                self.view.debug.status = DebugStatus::Running;
                self.view.debug.message = None;
                self.view.debug.stop_reason = None;
                self.clear_debug_inspection();
            }
            DebugEvent::Threads { threads, .. } => {
                if self
                    .debugger
                    .thread
                    .is_none_or(|id| !threads.iter().any(|t| t.id == id))
                {
                    self.debugger.thread = threads.first().map(|t| t.id);
                }
                self.view.debug.threads = threads
                    .into_iter()
                    .take(64)
                    .map(|t| DebugThreadView {
                        id: t.id,
                        name: short(&t.name, 4096),
                        selected: Some(t.id) == self.debugger.thread,
                        stopped: self.view.debug.status == DebugStatus::Stopped,
                    })
                    .collect::<Vec<_>>()
                    .into();
                if self.view.debug.status == DebugStatus::Stopped
                    && self.debugger.stack_request.is_none()
                    && self.debugger.frames.is_empty()
                {
                    if let Some(thread) = self.debugger.thread {
                        self.select_debug_thread(thread);
                    }
                }
            }
            DebugEvent::StackTrace {
                request_id,
                stop_id,
                thread_id,
                frames,
                ..
            } if self.view.debug.status == DebugStatus::Stopped
                && stop_id == self.debugger.stop_id
                && Some(request_id) == self.debugger.stack_request
                && Some(thread_id) == self.debugger.thread =>
            {
                self.debugger.stack_request = None;
                self.debugger.frames = frames.into_iter().take(PAGE).collect();
                if let Some(frame) = self.debugger.frames.first().map(|f| f.id) {
                    self.select_debug_frame(frame);
                }
            }
            DebugEvent::Scopes {
                request_id,
                stop_id,
                frame_id,
                scopes,
            } if self.view.debug.status == DebugStatus::Stopped
                && stop_id == self.debugger.stop_id
                && Some(request_id) == self.debugger.scope_request
                && Some(frame_id) == self.debugger.frame =>
            {
                self.debugger.scope_request = None;
                let frame = self
                    .debugger
                    .frames
                    .iter()
                    .find(|frame| frame.id == frame_id)
                    .map_or_else(String::new, |frame| frame.name.clone());
                for (index, scope) in scopes.into_iter().take(32).enumerate() {
                    let key = format!("scope:{index}");
                    self.debugger.roots.push(key.clone());
                    self.debugger.nodes.insert(
                        key.clone(),
                        VariableNode {
                            reference: scope.variables_reference,
                            children: vec![],
                            path: format!("{frame}\u{1f}{}", scope.name),
                            more: false,
                            view: DebugVariableView {
                                key: key.clone(),
                                name: short(&scope.name, 4096),
                                value: String::new(),
                                type_name: None,
                                depth: 0,
                                has_children: scope.variables_reference > 0,
                                ..Default::default()
                            },
                        },
                    );
                    if !scope.expensive && index < 2 {
                        self.load_debug_variables(&key);
                    }
                }
                self.refresh_debug_variables();
            }
            DebugEvent::Variables {
                request_id,
                stop_id,
                variables_reference,
                variables,
            } if self.view.debug.status == DebugStatus::Stopped
                && stop_id == self.debugger.stop_id =>
            {
                let Some(key) = self.debugger.variable_requests.remove(&request_id) else {
                    return;
                };
                let Some(node) = self
                    .debugger
                    .nodes
                    .get_mut(&key)
                    .filter(|n| n.reference == variables_reference)
                else {
                    return;
                };
                node.view.loading = false;
                // A full page is taken as "there may be more": the host asks
                // for one page and says so instead of hiding the rest.
                node.more = variables.len() >= PAGE;
                let parent = node.path.clone();
                let depth = node.view.depth + 1;
                let remaining = 1024usize.saturating_sub(self.debugger.nodes.len());
                let mut children = Vec::new();
                for (index, var) in variables.into_iter().take(PAGE.min(remaining)).enumerate() {
                    let child = format!("{key}/{index}");
                    children.push(child.clone());
                    let path = format!("{parent}\u{1f}{}", var.name);
                    let changed = self
                        .debugger
                        .previous_values
                        .get(&path)
                        .is_some_and(|previous| *previous != short(&var.value, 4096));
                    self.debugger.nodes.insert(
                        child.clone(),
                        VariableNode {
                            reference: var.variables_reference,
                            children: vec![],
                            path,
                            more: false,
                            view: DebugVariableView {
                                key: child,
                                name: short(&var.name, 4096),
                                value: short(&var.value, 4096),
                                type_name: var.type_name.map(|s| short(&s, 1024)),
                                depth,
                                has_children: var.variables_reference > 0 && depth < 8,
                                changed,
                                ..Default::default()
                            },
                        },
                    );
                }
                self.debugger.nodes.get_mut(&key).unwrap().children = children;
                self.refresh_debug_variables();
            }
            DebugEvent::Breakpoints {
                request_id,
                source,
                breakpoints,
                ..
            } => {
                if self.debugger.breakpoint_requests.get(&source).copied() == request_id {
                    self.debugger.verified.insert(source, breakpoints);
                    self.debugger.projection = None;
                }
            }
            DebugEvent::BreakpointChanged { reason, breakpoint } => {
                for marks in self.debugger.verified.values_mut() {
                    if let Some(mark) = marks
                        .iter_mut()
                        .find(|m| m.id.is_some() && m.id == breakpoint.id)
                    {
                        let line = mark.requested_line;
                        *mark = breakpoint.clone();
                        mark.requested_line = line;
                        if reason == "removed" {
                            mark.verified = false;
                            mark.message = Some("El adaptador eliminó este breakpoint".into());
                            mark.id = None;
                        }
                    }
                }
                self.debugger.projection = None;
            }
            DebugEvent::Output { category, output } => {
                for line in output.lines().take(512) {
                    self.debugger.output.push_back(DebugOutputView {
                        category: category.clone(),
                        text: short(line, 4096),
                    });
                }
                while self.debugger.output.len() > 512 {
                    self.debugger.output.pop_front();
                }
                self.debugger.output_dirty = true;
            }
            DebugEvent::RequestFailed {
                request_id,
                command,
                message,
            } => {
                let current = match command.as_str() {
                    "stackTrace" => request_id == self.debugger.stack_request,
                    "scopes" => request_id == self.debugger.scope_request,
                    "variables" => request_id
                        .is_some_and(|id| self.debugger.variable_requests.contains_key(&id)),
                    "continue" | "next" | "stepIn" | "stepOut" => {
                        request_id == self.debugger.control_request
                    }
                    "setBreakpoints" => {
                        request_id.is_none()
                            || request_id.is_some_and(|id| {
                                self.debugger
                                    .breakpoint_requests
                                    .values()
                                    .any(|current| *current == id)
                            })
                    }
                    _ => true,
                };
                if !current {
                    return;
                }
                let error = format!("{command}: {}", short(&message, 4096));
                self.view.debug.message = Some(error.clone());
                if request_id.is_some() && request_id == self.debugger.control_request {
                    self.debugger.control_request = None;
                }
                if request_id.is_some() && request_id == self.debugger.stack_request {
                    self.debugger.stack_request = None;
                }
                if request_id.is_some() && request_id == self.debugger.scope_request {
                    self.debugger.scope_request = None;
                }
                if let Some(key) =
                    request_id.and_then(|r| self.debugger.variable_requests.remove(&r))
                {
                    if let Some(node) = self.debugger.nodes.get_mut(&key) {
                        node.view.loading = false;
                        node.view.error = Some(error);
                    }
                    self.refresh_debug_variables();
                }
            }
            DebugEvent::Invalidated { thread_id, .. }
                if self.view.debug.status == DebugStatus::Stopped =>
            {
                if let Some(thread) = thread_id.or(self.debugger.thread) {
                    self.select_debug_thread(thread);
                } else {
                    self.clear_debug_inspection();
                    self.debug_send(DebugCommand::Threads);
                }
            }
            DebugEvent::Exited { exit_code } => {
                self.view.debug.message = Some(format!(
                    "Programa finalizado{}",
                    exit_code.map_or(String::new(), |c| format!(" · código {c}"))
                ));
            }
            DebugEvent::Terminated => {
                self.clear_debug_inspection();
                self.view.debug.status = DebugStatus::Stopping;
                self.debug_send(DebugCommand::Stop);
            }
            DebugEvent::Disconnected { reason } => {
                let failed = matches!(self.view.debug.status, DebugStatus::Starting);
                self.debugger.session = None;
                self.debugger.verified.clear();
                self.clear_debug_inspection();
                self.view.debug.stop_reason = None;
                self.view.debug.status = if failed {
                    DebugStatus::Failed
                } else {
                    DebugStatus::Ended
                };
                if self.view.debug.message.is_none() {
                    self.view.debug.message = Some(short(&reason, 4096));
                }
                // Restart is a stop followed by a start of the same program.
                if std::mem::take(&mut self.debugger.restart_after_stop) && !failed {
                    self.start_debugger();
                }
            }
            _ => {}
        }
    }
    pub(super) fn poll_debugger(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = self.debugger.workspace != self.view.workspace;
        self.reset_debug_workspace();
        self.view.debug.sections = self.view.ide.debug_sections;
        if self.debug_sidebar_visible() && !self.view.debug_open {
            // Restore persisted activity content without launching a process.
            self.view.debug_open = true;
            if self.debugger.configurations.is_empty() {
                self.load_debug_config(false);
            }
            self.view.debug.breakpoints = self.debug_breakpoint_views().into();
            changed = true;
        }
        let ready = match self.debugger.config_job.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(ready)) => Some(ready),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.debugger.config_job = None;
                self.view.debug.message =
                    Some("No se pudo leer la configuración; vuelve a cargarla".into());
                changed = true;
                None
            }
            _ => None,
        };
        if let Some(ready) = ready {
            self.debugger.config_job = None;
            if ready.workspace == self.view.workspace {
                self.debugger.configurations = match ready.result {
                    Ok(configs) if configs.is_empty() => {
                        self.view.debug.status = DebugStatus::Unconfigured;
                        self.view.debug.message = None;
                        configs
                    }
                    Ok(configs) => {
                        self.view.debug.status = DebugStatus::Idle;
                        self.view.debug.message = None;
                        configs
                    }
                    Err(error) => {
                        self.view.debug.status = DebugStatus::Failed;
                        self.view.debug.message = Some(error);
                        vec![]
                    }
                };
                self.debugger.selected = self
                    .debugger
                    .selected
                    .min(self.debugger.configurations.len().saturating_sub(1));
                self.sync_debug_configuration();
                if ready.open_file && self.ide_workspace_active() {
                    let scope = self.dock_scope();
                    self.dock.open(
                        &scope,
                        DockTabKind::FilePreview {
                            path: debug_config::CONFIG_PATH.into(),
                        },
                    );
                    self.sync_dock_view();
                }
                changed = true;
            }
        }
        let generation = self.debug_breakpoint_generation();
        if generation != self.debugger.breakpoint_generation {
            self.debugger.breakpoint_generation = generation;
            let sources: BTreeMap<_, _> = self
                .debug_source_breakpoints()
                .into_iter()
                .map(|s| (s.path, s.lines))
                .collect();
            let paths: BTreeSet<_> = sources
                .keys()
                .chain(self.debugger.sources.keys())
                .cloned()
                .collect();
            for path in paths {
                if sources.get(&path) != self.debugger.sources.get(&path) {
                    self.debugger.verified.remove(&path);
                    if let Some(request) =
                        self.debug_send(DebugCommand::SetBreakpoints(SourceBreakpoints {
                            path: path.clone(),
                            lines: sources.get(&path).cloned().unwrap_or_default(),
                        }))
                    {
                        self.debugger
                            .breakpoint_requests
                            .insert(path.clone(), request);
                    }
                }
            }
            self.debugger.sources = sources;
            self.debugger.projection = None;
            changed = true;
        }
        let events = self
            .debugger
            .session
            .as_ref()
            .map_or(Vec::new(), |session| session.drain());
        for event in events {
            self.handle_debug_event(event);
            changed = true;
        }
        if changed {
            self.view.debug.breakpoints = self.debug_breakpoint_views().into();
            if self.view.debug.breakpoints.is_empty() {
                self.view.debug.confirm_clear_breakpoints = false;
            }
        }
        if self.debugger.output_dirty {
            self.view.debug.output = self
                .debugger
                .output
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .into();
            self.debugger.output_dirty = false;
        }
        changed |= self.project_debug_editor();
        let previous = self.view.debug.controls.start;
        self.refresh_debug_controls();
        changed |= previous != self.view.debug.controls.start;
        if self.debugger.close_after_stop {
            // Retry a Stop rejected by a temporarily full command queue,
            // or release the close only after Disconnected removed the owner.
            self.request_close_after_debugger(ctx);
        }
        if self.debugger.session.is_some() || self.debugger.config_job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(24));
        }
        changed
    }
    /// The sidebar's marks with what the running adapter answered for each.
    fn debug_breakpoint_views(&self) -> Vec<DebugBreakpointView> {
        let mut marks = self.debug_sidebar_breakpoints();
        for mark in marks.iter_mut().filter(|mark| mark.enabled && !mark.dirty) {
            let absolute = PathBuf::from(&self.view.workspace).join(&mark.path);
            if let Some(answer) = self.debugger.verified.get(&absolute).and_then(|answers| {
                answers
                    .iter()
                    .find(|answer| answer.requested_line == Some(mark.line))
            }) {
                mark.verified = Some(answer.verified);
                mark.message = answer.message.clone().or_else(|| {
                    answer
                        .line
                        .filter(|line| answer.verified && *line != mark.line)
                        .map(|line| format!("El adaptador detendrá en la línea {line}"))
                });
            }
        }
        marks
    }
    fn project_debug_editor(&mut self) -> bool {
        let Some((_, path, _, snapshot)) = self.active_editor() else {
            return false;
        };
        let key = (
            snapshot.document_id(),
            snapshot.revision(),
            self.debugger.epoch,
            self.debugger.inspection,
            snapshot.is_dirty(),
        );
        if self.debugger.projection == Some(key) {
            return false;
        }
        self.debugger.projection = Some(key);
        let absolute = PathBuf::from(&self.view.workspace).join(&path);
        let marks = self.debugger.verified.get(&absolute);
        let requested = self.view.editor_workbench.breakpoint_rows.clone();
        self.view.editor_workbench.debug_breakpoints = requested
            .iter()
            .map(|row| {
                let verified = marks.and_then(|marks| {
                    marks
                        .iter()
                        .find(|m| m.requested_line == Some((*row + 1) as u32))
                });
                let (state, message) = if snapshot.is_dirty() {
                    (
                        CodeBreakpointState::Disabled,
                        Some("Guarda el archivo antes de depurar esta línea".into()),
                    )
                } else if !self.debug_breakpoint_enabled(&path, *row) {
                    (
                        CodeBreakpointState::Disabled,
                        Some("Punto de interrupción desactivado".into()),
                    )
                } else if let Some(mark) = verified {
                    (
                        if mark.verified {
                            CodeBreakpointState::Verified
                        } else {
                            CodeBreakpointState::Rejected
                        },
                        mark.message.clone().or_else(|| {
                            mark.line
                                .filter(|l| *l != (*row + 1) as u32)
                                .map(|l| format!("El adaptador detendrá en la línea {l}"))
                        }),
                    )
                } else {
                    (CodeBreakpointState::Pending, None)
                };
                let display_row = verified
                    .filter(|m| m.verified && !snapshot.is_dirty())
                    .and_then(|m| m.line)
                    .filter(|line| *line > 0 && (*line as usize) <= snapshot.line_count())
                    .map_or(*row, |line| line as usize - 1);
                CodeBreakpoint {
                    row: display_row,
                    state,
                    message,
                }
            })
            .collect::<Vec<_>>()
            .into();
        self.view.editor_workbench.debug_execution =
            if self.view.debug.status == DebugStatus::Stopped && !snapshot.is_dirty() {
                self.debugger
                    .frames
                    .iter()
                    .find(|f| Some(f.id) == self.debugger.frame)
                    .filter(|f| {
                        f.source
                            .as_ref()
                            .and_then(|s| s.path.as_ref())
                            .and_then(|p| source_relative(&self.view.workspace, p))
                            == Some(PathBuf::from(&path))
                    })
                    .filter(|f| f.line > 0 && f.line as usize <= snapshot.line_count())
                    .map(|f| CodeExecutionLine {
                        row: f.line.saturating_sub(1) as usize,
                        primary: self
                            .debugger
                            .frames
                            .first()
                            .is_some_and(|first| first.id == f.id),
                    })
            } else {
                None
            };
        true
    }
    pub(super) fn debug_requested_breakpoint_row(&self, path: &str, row: usize) -> usize {
        let absolute = PathBuf::from(&self.view.workspace).join(path);
        self.debugger
            .verified
            .get(&absolute)
            .and_then(|marks| {
                marks
                    .iter()
                    .find(|mark| mark.verified && mark.line == u32::try_from(row + 1).ok())
            })
            .and_then(|mark| mark.requested_line)
            .filter(|line| *line > 0)
            .map_or(row, |line| line as usize - 1)
    }

    fn sync_debug_configuration(&mut self) {
        self.view.debug.configurations = self
            .debugger
            .configurations
            .iter()
            .map(|c| c.name.clone())
            .collect::<Vec<_>>()
            .into();
        self.view.debug.selected_configuration = self.debugger.selected;
        let selected = self.debugger.configurations.get(self.debugger.selected);
        self.view.debug.configuration =
            selected.map_or_else(|| "Configura un adaptador DAP".into(), |c| c.name.clone());
        self.view.debug.adapter = selected.map(|c| c.adapter_name());
    }
    /// Menu commands are an explicit entry point even when the inspector is
    /// hidden. Ordinary inspector actions still require their visible scope.
    pub(super) fn apply_debug_menu_action(&mut self, id: &str, ctx: &egui::Context) -> bool {
        let Some(data) = id.strip_prefix("debug_control:") else {
            return false;
        };
        if !self.ide_workspace_active() {
            return true;
        }
        self.reset_debug_workspace();
        let Ok((scope, operation)) = serde_json::from_str::<(String, String)>(data) else {
            return true;
        };
        if scope != self.view.debug.scope {
            return true;
        }
        self.refresh_debug_controls();
        let controls = &self.view.debug.controls;
        let enabled = match operation.as_str() {
            "start" => controls.start,
            "resume" => controls.resume,
            "pause" => controls.pause,
            "step_over" => controls.step_over,
            "step_into" => controls.step_into,
            "step_out" => controls.step_out,
            "restart" => controls.restart,
            "stop" => controls.stop,
            _ => false,
        };
        if enabled {
            self.open_debug_panel();
            self.debug_operation(&operation);
            self.reveal_debug_console_after_start(&operation, ctx);
            ctx.request_repaint();
        }
        true
    }

    pub(super) fn apply_debug_action(&mut self, action: &UiAction, ctx: &egui::Context) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if matches!(id.as_str(), "dock_open:debugger" | "ide_debug_toggle") {
            if !self.ide_workspace_active() {
                return true;
            }
            if id == "ide_debug_toggle" && self.view.debug_open && self.debug_sidebar_visible() {
                self.hide_debug_panel();
            } else {
                self.open_debug_panel();
            }
            ctx.request_repaint();
            return true;
        }
        let Some((kind, data)) = id.split_once(':').filter(|(k, _)| k.starts_with("debug_")) else {
            return false;
        };
        self.reset_debug_workspace();
        let Ok((scope, value)) = serde_json::from_str::<(String, serde_json::Value)>(data) else {
            return true;
        };
        if scope != self.view.debug.scope || !self.view.debug_open || !self.ide_workspace_active() {
            return true;
        }
        match kind {
            "debug_control" => {
                if let Some(op) = value.as_str() {
                    if op == "console" {
                        self.open_ide_bottom(IdeBottomPanel::DebugConsole, ctx);
                    }
                    self.debug_operation(op);
                    self.reveal_debug_console_after_start(op, ctx);
                }
            }
            "debug_breakpoint_toggle" | "debug_breakpoint_remove" => {
                if let Ok((path, line)) = serde_json::from_value::<(String, u32)>(value) {
                    let row = line.saturating_sub(1) as usize;
                    let remove = kind == "debug_breakpoint_remove";
                    self.edit_debug_breakpoints(|entry| {
                        if entry.path != path || !entry.rows.contains(&row) {
                            return;
                        }
                        if remove {
                            entry.rows.remove(&row);
                            entry.disabled.remove(&row);
                        } else if !entry.disabled.remove(&row) {
                            entry.disabled.insert(row);
                        }
                    });
                }
            }
            "debug_reveal" if self.view.debug.status == DebugStatus::Stopped => {
                if let Some(frame) = value.as_i64() {
                    self.reveal_debug_frame(frame);
                }
            }
            "debug_copy" => {
                if let Ok((key, what)) = serde_json::from_value::<(String, String)>(value) {
                    if let Some(node) = self.debugger.nodes.get(&key) {
                        ctx.copy_text(if what == "name" {
                            node.view.name.clone()
                        } else {
                            node.view.value.clone()
                        });
                    }
                }
            }
            "debug_configuration" if self.debugger.session.is_none() => {
                if let Some(index) = value
                    .as_u64()
                    .filter(|i| *i < self.debugger.configurations.len() as u64)
                {
                    self.debugger.selected = index as usize;
                    self.view.debug.configuration_menu = false;
                    self.sync_debug_configuration();
                }
            }
            "debug_breakpoint" => {
                if let Ok((path, line)) = serde_json::from_value::<(String, u32)>(value) {
                    self.select_debug_breakpoint(&path, line);
                }
            }
            "debug_thread" if self.view.debug.status == DebugStatus::Stopped => {
                if let Some(thread) = value
                    .as_i64()
                    .filter(|id| self.view.debug.threads.iter().any(|t| t.id == *id))
                {
                    self.select_debug_thread(thread);
                }
            }
            "debug_frame" if self.view.debug.status == DebugStatus::Stopped => {
                if let Some(frame) = value.as_i64() {
                    self.select_debug_frame(frame);
                }
            }
            "debug_expand" if self.view.debug.status == DebugStatus::Stopped => {
                if let Some(key) = value.as_str() {
                    if let Some(node) = self.debugger.nodes.get_mut(key) {
                        if node.view.expanded {
                            node.view.expanded = false;
                        } else if !node.children.is_empty() {
                            node.view.expanded = true;
                        } else {
                            self.load_debug_variables(key);
                        }
                        self.refresh_debug_variables();
                    }
                }
            }
            "debug_section" => {
                let sections = &mut self.view.ide.debug_sections;
                match value.as_str() {
                    Some("stack") => sections.stack ^= true,
                    Some("variables") => sections.variables ^= true,
                    Some("breakpoints") => sections.breakpoints ^= true,
                    _ => {}
                }
                self.view.debug.sections = *sections;
                self.persist_ide_workspace();
            }
            _ => {}
        }
        self.refresh_debug_controls();
        ctx.request_repaint();
        true
    }
    fn debug_operation(&mut self, op: &str) {
        self.refresh_debug_controls();
        let controls = self.view.debug.controls.clone();
        match op {
            "hide" => self.hide_debug_panel(),
            "configure" if self.debugger.session.is_none() => self.load_debug_config(true),
            "reload" if self.debugger.session.is_none() => self.load_debug_config(false),
            "configurations" if self.debugger.session.is_none() => {
                self.view.debug.configuration_menu ^= true
            }
            "dismiss_configurations" => self.view.debug.configuration_menu = false,
            "console" => {}
            "toggle_breakpoints" => {
                let disable = self.view.debug.breakpoints.iter().any(|mark| mark.enabled);
                self.edit_debug_breakpoints(|entry| {
                    entry.disabled = if disable {
                        entry.rows.clone()
                    } else {
                        Default::default()
                    };
                });
            }
            "clear_breakpoints" => {
                self.view.debug.confirm_clear_breakpoints = !self.view.debug.breakpoints.is_empty()
            }
            "clear_breakpoints_cancel" => self.view.debug.confirm_clear_breakpoints = false,
            "clear_breakpoints_confirm" if self.view.debug.confirm_clear_breakpoints => {
                self.view.debug.confirm_clear_breakpoints = false;
                self.edit_debug_breakpoints(|entry| entry.rows.clear());
            }
            "start" => self.start_debugger(),
            "stop" | "restart" if controls.stop => {
                if self.debug_send(DebugCommand::Stop).is_some() {
                    self.debugger.restart_after_stop = op == "restart";
                    self.view.debug.status = DebugStatus::Stopping;
                    self.clear_debug_inspection();
                }
            }
            "stop" | "restart" => {}
            _ => {
                let Some(thread_id) = self.debugger.thread else {
                    return;
                };
                let command = match op {
                    "resume" if controls.resume => DebugCommand::Continue { thread_id },
                    "pause" if controls.pause => DebugCommand::Pause { thread_id },
                    "step_over" if controls.step_over => DebugCommand::Next { thread_id },
                    "step_into" if controls.step_into => DebugCommand::StepIn { thread_id },
                    "step_out" if controls.step_out => DebugCommand::StepOut { thread_id },
                    _ => return,
                };
                if let Some(request) = self.debug_send(command) {
                    if op != "pause" {
                        self.debugger.control_request = Some(request);
                    }
                }
            }
        }
    }
    pub(super) fn handle_debug_key(&mut self, event: &UiEvent, ctx: &egui::Context) -> bool {
        let UiEvent::Key { key, ctrl, shift } = event else {
            return false;
        };
        if !self.ide_workspace_active()
            || self.tree.context_menu_open()
            || self.view.ide.menu_open.is_some()
        {
            return false;
        }
        let op = match (key, shift) {
            (openide_ui::Key::F5, true) if *ctrl => "restart",
            _ if *ctrl => return false,
            (openide_ui::Key::F5, false) => {
                if self.view.debug.status == DebugStatus::Stopped {
                    "resume"
                } else {
                    "start"
                }
            }
            (openide_ui::Key::F5, true) => "stop",
            (openide_ui::Key::F10, false) => "step_over",
            (openide_ui::Key::F11, false) => "step_into",
            (openide_ui::Key::F11, true) => "step_out",
            _ => return false,
        };
        if !self.view.debug_open || !self.debug_sidebar_visible() {
            self.open_debug_panel();
        }
        self.debug_operation(op);
        self.reveal_debug_console_after_start(op, ctx);
        ctx.request_repaint();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restored_debug_activity_loads_configuration_without_running_or_reopening_after_hide() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("state"));
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.view.main_page = MainPage::Workspace;
        app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
        app.view.ide.activity = IdeActivity::Debug;
        app.view.ide.explorer_open = true;
        app.view.debug_open = false;
        let ctx = egui::Context::default();
        assert!(app.poll_debugger(&ctx));
        assert!(app.view.debug_open);
        assert!(app.debugger.session.is_none());
        let hide = UiAction::Clicked {
            id: action(&app.view.debug.scope, "hide"),
        };
        assert!(app.apply_debug_action(&hide, &ctx));
        assert!(!app.view.ide.explorer_open);
        assert!(!app.view.debug_open);
        app.poll_debugger(&ctx);
        assert!(!app.view.debug_open);
        app.handle_debug_event(DebugEvent::Stopped {
            stop_id: 1,
            reason: "pause".into(),
            description: None,
            thread_id: Some(7),
            all_threads_stopped: true,
        });
        assert_eq!(app.view.debug.status, DebugStatus::Stopped);
        assert!(
            !app.view.debug_open,
            "asynchronous stops preserve the hidden sidebar"
        );
        assert!(app.debugger.session.is_none());
    }
    #[test]
    fn source_navigation_is_workspace_scoped() {
        assert_eq!(
            source_relative("/workspace", std::path::Path::new("/workspace/src/main.rs")),
            Some("src/main.rs".into())
        );
        for path in ["/other/file.rs", "../secret", "src/../../secret", ""] {
            assert!(source_relative("/workspace", std::path::Path::new(path)).is_none());
        }
    }
    #[test]
    fn output_truncation_preserves_unicode() {
        assert_eq!(short("ééé", 3), "é…");
    }
}

#[cfg(test)]
#[path = "debug_host_tests.rs"]
mod host_tests;
