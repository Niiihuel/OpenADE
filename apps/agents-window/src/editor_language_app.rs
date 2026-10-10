//! Versioned language features for the native editor. All edits remain undoable buffers.
use super::*;
use crate::editor_workbench_ui::LanguageResult;
use openide_editor::{BufferSnapshot, EditKind, EditorCommand, Selection, TextEdit};
use openide_language::lsp::{self, Event, LanguageServer, Request, RequestKind, ServerConfig};
use openide_language::{
    offset_to_utf16, utf16_to_offset, AnalysisWorker, Language, Position, Range,
};
use serde_json::{json, Value};
use std::sync::{mpsc, Arc};

struct EditTarget {
    path: PathBuf,
    snapshot: BufferSnapshot,
    controller: openide_ui::EditorController,
    edits: Vec<TextEdit>,
}
struct Startup {
    scope: (PathBuf, Language),
    epoch: u64,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    receiver: mpsc::Receiver<Result<LanguageServer, String>>,
}
struct WorkspaceEdit {
    source: (u64, u64),
    source_path: PathBuf,
    workspace: PathBuf,
    changes: BTreeMap<PathBuf, Vec<Value>>,
    versions: BTreeMap<PathBuf, i32>,
    expected: BTreeMap<PathBuf, (u64, u64)>,
    loaded: Vec<EditTarget>,
    awaiting: VecDeque<PathBuf>,
    ready: bool,
}
pub(super) struct EditorLanguageHost {
    worker: AnalysisWorker,
    analyzed: Option<(u64, u64, Language)>,
    /// The text the folds currently refer to.
    folded_snapshot: Option<BufferSnapshot>,
    due: Option<Instant>,
    parsing: bool,
    pub(super) server: Option<LanguageServer>,
    startup: Option<Startup>,
    overrides: HashMap<(PathBuf, Language), PathBuf>,
    connection_epoch: u64,
    root_language: Option<(PathBuf, Language)>,
    opened: BTreeMap<PathBuf, lsp::DocumentVersion>,
    requests: HashMap<u64, (u64, u64)>,
    edit: Option<WorkspaceEdit>,
}
impl Default for EditorLanguageHost {
    fn default() -> Self {
        Self {
            worker: AnalysisWorker::new(),
            analyzed: None,
            folded_snapshot: None,
            due: None,
            parsing: false,
            server: None,
            startup: None,
            overrides: HashMap::new(),
            connection_epoch: 0,
            root_language: None,
            opened: BTreeMap::new(),
            requests: HashMap::new(),
            edit: None,
        }
    }
}

fn start_language_server(
    root: PathBuf,
    language: Language,
    executable: Option<PathBuf>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<LanguageServer, String> {
    if language == Language::Plain {
        return Err("Texto sin formato · sin servidor de lenguaje".into());
    }
    let candidates = executable
        .map(|path| vec![path])
        .unwrap_or_else(|| lsp::server_candidates(language));
    if candidates.is_empty() {
        return Err(format!(
            "Análisis de sintaxis activo · servidor {} no instalado",
            language.lsp_id()
        ));
    }
    let mut failures = Vec::new();
    for executable in candidates {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err("Conexión cancelada".into());
        }
        // Some servers enter stdio even for --version. Initialize is the authoritative probe.
        let mut config = ServerConfig::new(&executable, &root);
        config.args = lsp::stdio_args(&executable);
        let server = match LanguageServer::spawn(config) {
            Ok(server) => server,
            Err(error) => {
                failures.push(error.to_string());
                continue;
            }
        };
        loop {
            if cancel.load(std::sync::atomic::Ordering::Acquire) {
                return Err("Conexión cancelada".into());
            }
            if server.is_ready() {
                return Ok(server);
            }
            if let Some(Event::Stopped { reason }) = server.try_recv() {
                failures.push(reason);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Err(format!(
        "Servidor de lenguaje no disponible: {}",
        failures
            .last()
            .map(String::as_str)
            .unwrap_or("no pudo iniciar")
    ))
}
fn retire_server(server: LanguageServer) {
    static REAPER: std::sync::OnceLock<mpsc::SyncSender<LanguageServer>> =
        std::sync::OnceLock::new();
    let sender = REAPER.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel::<LanguageServer>(4);
        std::thread::Builder::new()
            .name("editor-lsp-close".into())
            .spawn(move || {
                while let Ok(server) = receiver.recv() {
                    drop(server);
                }
            })
            .expect("LSP cleanup worker");
        sender
    });
    // At most one active and one initializing server belong to this host. A bounded
    // reaper keeps process waits out of normal frames; overload still closes, never leaks.
    if let Err(error) = sender.try_send(server) {
        match error {
            mpsc::TrySendError::Full(server) | mpsc::TrySendError::Disconnected(server) => {
                drop(server)
            }
        }
    }
}
fn completion_caret(edits: &[TextEdit]) -> Result<usize, String> {
    let primary = edits.first().ok_or("Sugerencia sin edición")?;
    let mut target = primary
        .range
        .start
        .checked_add(primary.replacement.chars().count())
        .ok_or("Posición demasiado grande")?;
    for edit in &edits[1..] {
        if edit.range.end <= primary.range.start {
            target = target
                .checked_sub(edit.range.len())
                .and_then(|value| value.checked_add(edit.replacement.chars().count()))
                .ok_or("Posición de sugerencia inválida")?;
        }
    }
    Ok(target)
}
fn completion_start(snapshot: &BufferSnapshot, offset: usize) -> Result<usize, String> {
    let mut cursor = offset;
    for _ in 0..1024 {
        if cursor == 0 {
            return Ok(0);
        }
        let start = snapshot
            .prev_grapheme(cursor)
            .map_err(|error| error.to_string())?;
        let cluster = snapshot
            .slice(start..cursor)
            .map_err(|error| error.to_string())?;
        if !cluster
            .chars()
            .any(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$'))
        {
            return Ok(cursor);
        }
        cursor = start;
    }
    Err("El prefijo es demasiado largo; el servidor debe proporcionar un rango explícito".into())
}
fn valid_source(
    edit: &WorkspaceEdit,
    current_workspace: &Path,
    controller: Option<openide_ui::EditorController>,
) -> Result<(), String> {
    if edit.workspace != current_workspace {
        return Err("El proyecto cambió; solicitá la acción otra vez".into());
    }
    let snapshot = controller
        .ok_or("El archivo de origen se cerró")?
        .snapshot()?;
    if (snapshot.document_id(), snapshot.revision()) != edit.source {
        return Err("El archivo de origen cambió; solicitá la acción otra vez".into());
    }
    Ok(())
}
const TRUNCATED_TEXT: &str = "… [truncado]";
fn bounded_text(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}{}", &text[..end], TRUNCATED_TEXT),
        None => text.to_owned(),
    }
}
fn excerpt(value: &Value) -> String {
    fn append_text(text: &str, output: &mut String, budget: &mut usize, truncated: &mut bool) {
        let mut chars = text.chars();
        for ch in chars.by_ref().take(*budget) {
            output.push(ch);
            *budget -= 1;
        }
        if chars.next().is_some() {
            *truncated = true;
        }
    }
    fn append(value: &Value, output: &mut String, budget: &mut usize, truncated: &mut bool) {
        match value {
            Value::String(text) => append_text(text, output, budget, truncated),
            Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    if *budget == 0 {
                        *truncated |= index < values.len();
                        break;
                    }
                    if index > 0 {
                        append_text("\n\n", output, budget, truncated);
                    }
                    append(value, output, budget, truncated);
                }
            }
            Value::Object(_) => {
                if let Some(value) = value.get("value").or_else(|| value.get("contents")) {
                    append(value, output, budget, truncated);
                }
            }
            _ => {}
        }
    }
    let mut output = String::new();
    let mut budget = 6000;
    let mut truncated = false;
    append(value, &mut output, &mut budget, &mut truncated);
    if truncated {
        output.push_str(TRUNCATED_TEXT);
    }
    output
}
fn decode_range(snapshot: &BufferSnapshot, edit: &Value) -> Result<TextEdit, String> {
    let range: Range = serde_json::from_value(
        edit.get("range")
            .or_else(|| edit.get("replace"))
            .cloned()
            .ok_or("Edición sin rango")?,
    )
    .map_err(|e| e.to_string())?;
    let start =
        utf16_to_offset(snapshot, range.start).ok_or("Inicio de edición fuera del archivo")?;
    let end = utf16_to_offset(snapshot, range.end).ok_or("Fin de edición fuera del archivo")?;
    if start > end {
        return Err("Rango de edición invertido".into());
    }
    Ok(TextEdit {
        range: start..end,
        replacement: edit
            .get("newText")
            .and_then(Value::as_str)
            .ok_or("Edición sin texto")?
            .to_owned(),
    })
}

impl AgentsWindow {
    pub(super) fn editor_language_scope(&self) -> Option<(PathBuf, Language)> {
        let editor = self.view.code_editor.as_ref().filter(|editor| !editor.loading)?;
        let language = self.view.editor_workbench.language_override
            .unwrap_or_else(|| crate::editor_workbench_ui::language_for_path(&editor.path));
        Some((PathBuf::from(&self.view.workspace), language))
    }

    /// The services field follows the active language, including an unsaved
    /// language override. Reading it never probes or launches an executable.
    pub(super) fn editor_language_server_path(&self) -> String {
        let Some(scope) = self.editor_language_scope() else { return String::new() };
        self.editor_services.language.overrides.get(&scope)
            .map(|path| path.to_string_lossy().into_owned())
            .or_else(|| self.editor_server_executable_preference(scope.1))
            .unwrap_or_default()
    }

    pub(super) fn editor_language_connecting(&self) -> bool {
        let Some(scope) = self.editor_language_scope() else { return false };
        let host = &self.editor_services.language;
        host.startup.as_ref().is_some_and(|startup| {
            startup.scope == scope
                && startup.epoch == host.connection_epoch
                && !startup.cancel.load(std::sync::atomic::Ordering::Acquire)
        }) || (!self.view.editor_workbench.language_ready
            && self.view.editor_workbench.language_status == "Conectando lenguaje…"
            && host.root_language.as_ref().is_none_or(|current| current == &scope))
    }

    /// A path is data, never a shell command. Validation and process startup happen off-frame.
    pub(super) fn restart_editor_language(&mut self, server_path: &str) -> Result<(), String> {
        let (_, path, _, _) = self.active_editor().ok_or("No hay un archivo abierto")?;
        let language = self
            .view
            .editor_workbench
            .language_override
            .unwrap_or_else(|| crate::editor_workbench_ui::language_for_path(&path));
        if language == Language::Plain {
            return Err("Elegí un lenguaje antes de conectar un servidor".into());
        }
        let scope = (PathBuf::from(&self.view.workspace), language);
        let host = &mut self.editor_services.language;
        let path = server_path.trim();
        if path.len() > 4096 || path.contains(['\0', '\r', '\n']) {
            return Err("Ruta de servidor inválida".into());
        }
        if path.is_empty() {
            host.overrides.remove(&scope);
        } else {
            host.overrides.insert(scope, PathBuf::from(path));
        }
        host.connection_epoch = host.connection_epoch.wrapping_add(1);
        if let Some(startup) = &host.startup {
            startup
                .cancel
                .store(true, std::sync::atomic::Ordering::Release);
        }
        if let Some(server) = host.server.take() {
            retire_server(server);
        }
        host.opened.clear();
        host.requests.clear();
        host.edit = None;
        host.root_language = None;
        self.view.editor_workbench.language_ready = false;
        self.view.editor_workbench.language_status = "Conectando lenguaje…".into();
        Ok(())
    }
    pub(super) fn poll_editor_language(&mut self, ctx: &egui::Context) -> bool {
        let Some((_, path, _, snapshot)) = self.active_editor() else {
            let host = &mut self.editor_services.language;
            let changed = host.analyzed.take().is_some() || host.edit.is_some();
            host.worker.cancel();
            host.due = None;
            host.parsing = false;
            host.edit = None;
            if let Some(server) = host.server.take() {
                retire_server(server);
            }
            host.opened.clear();
            host.requests.clear();
            host.root_language = None;
            if let Some(startup) = &host.startup {
                startup
                    .cancel
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            if let Some(result) =
                host.startup
                    .as_ref()
                    .and_then(|startup| match startup.receiver.try_recv() {
                        Ok(result) => Some(result),
                        Err(mpsc::TryRecvError::Disconnected) => {
                            Some(Err("El inicio de lenguaje se interrumpió".into()))
                        }
                        Err(mpsc::TryRecvError::Empty) => None,
                    })
            {
                if let Ok(server) = result {
                    retire_server(server);
                }
                host.startup = None;
            }
            if host.startup.is_some() {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            return changed;
        };
        let root = PathBuf::from(&self.view.workspace);
        let absolute = root.join(&path);
        let language = self
            .view
            .editor_workbench
            .language_override
            .unwrap_or_else(|| crate::editor_workbench_ui::language_for_path(&path));
        let key = (snapshot.document_id(), snapshot.revision(), language);
        let mut changed = false;
        if self.editor_services.language.analyzed != Some(key) {
            let host = &mut self.editor_services.language;
            host.analyzed = Some(key);
            host.worker.cancel();
            host.parsing = false;
            host.due = Some(Instant::now() + Duration::from_millis(90));
            self.view.editor_workbench.diagnostics_current = false;
            // Folds follow the text through an edit. They are dropped when the
            // document changes or the rows cannot be carried over.
            let folds = host
                .folded_snapshot
                .as_ref()
                .filter(|_| !self.view.editor_workbench.folds.is_empty())
                .and_then(|previous| {
                    openide_editor::remap_rows(previous, &snapshot, &self.view.editor_workbench.folds).ok()
                })
                .unwrap_or_default();
            self.view.editor_workbench.folds = folds.into();
            host.folded_snapshot = Some(snapshot.clone());
            self.view.editor_workbench.results.clear();
            self.view.editor_workbench.hover = None;
            host.requests.retain(|id, owner| {
                if *owner == (snapshot.document_id(), snapshot.revision()) {
                    true
                } else {
                    if let Some(server) = &host.server {
                        server.cancel(*id);
                    }
                    false
                }
            });
            changed = true;
        }
        let retained = self
            .code_editor_documents()
            .into_iter()
            .filter(|document| document.workspace == root)
            .map(|document| root.join(document.path))
            .collect::<BTreeSet<_>>();
        let host = &mut self.editor_services.language;
        if let Some(server) = &host.server {
            host.opened.retain(|path, _| {
                if retained.contains(path) {
                    true
                } else {
                    let _ = server.close(path);
                    false
                }
            });
            host.requests.retain(|id, owner| {
                if host
                    .opened
                    .values()
                    .any(|version| (version.document_id, version.revision) == *owner)
                {
                    true
                } else {
                    server.cancel(*id);
                    false
                }
            });
        }
        if host.due.is_some_and(|due| Instant::now() >= due) {
            if host.worker.submit(snapshot.clone(), language) {
                host.due = None;
                host.parsing = true;
            }
        }
        if let Some(result) = host.worker.try_recv() {
            host.parsing = false;
            match result {
                Ok(analysis)
                    if analysis.document_id == snapshot.document_id()
                        && analysis.revision == snapshot.revision()
                        && analysis.language == language =>
                {
                    self.view.editor_workbench.highlight_spans = analysis
                        .highlights
                        .iter()
                        .map(|span| openide_ui::CodeHighlight {
                            range: span.range.clone(),
                            color: match span.kind {
                                openide_language::HighlightKind::Comment => {
                                    openide_ui::Color::rgb(126, 151, 119)
                                }
                                openide_language::HighlightKind::String => {
                                    openide_ui::Color::rgb(206, 163, 127)
                                }
                                openide_language::HighlightKind::Keyword => {
                                    openide_ui::Color::rgb(184, 143, 216)
                                }
                                openide_language::HighlightKind::Number => {
                                    openide_ui::Color::rgb(151, 193, 169)
                                }
                                openide_language::HighlightKind::Type => {
                                    openide_ui::Color::rgb(225, 196, 131)
                                }
                                openide_language::HighlightKind::Function => {
                                    openide_ui::Color::rgb(143, 185, 218)
                                }
                                _ => crate::theme::FOREGROUND,
                            },
                        })
                        .collect::<Vec<_>>()
                        .into();
                    self.view.editor_workbench.analysis = Some(Arc::new(analysis));
                    changed = true;
                }
                Err(error) => {
                    self.view.editor_workbench.error = Some(error);
                    changed = true;
                }
                _ => {}
            }
        }
        let desired = (root.clone(), language);
        if host.root_language.as_ref() != Some(&desired) {
            if let Some(server) = host.server.take() {
                retire_server(server);
            }
            host.opened.clear();
            host.requests.clear();
            host.edit = None;
            if let Some(startup) = &host.startup {
                startup
                    .cancel
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            host.root_language = Some(desired.clone());
            self.view.editor_workbench.language_ready = false;
            self.view.editor_workbench.language_status = "Conectando lenguaje…".into();
            changed = true;
        }
        if let Some(result) =
            host.startup
                .as_ref()
                .and_then(|startup| match startup.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err("El inicio de lenguaje se interrumpió".into()))
                    }
                    Err(mpsc::TryRecvError::Empty) => None,
                })
        {
            let startup = host.startup.take().unwrap();
            let scope = startup.scope;
            let epoch = startup.epoch;
            match result {
                Ok(server) if scope == desired && epoch == host.connection_epoch => {
                    let repaint = ctx.clone();
                    server.set_waker(Arc::new(move || repaint.request_repaint()));
                    host.server = Some(server)
                }
                Ok(server) => retire_server(server),
                Err(error) if scope == desired && epoch == host.connection_epoch => {
                    self.view.editor_workbench.language_status = error
                }
                Err(_) => {}
            }
            changed = true;
        }
        // One startup at a time even when the user rapidly switches languages/projects.
        if host.server.is_none()
            && host.startup.is_none()
            && self.view.editor_workbench.language_status == "Conectando lenguaje…"
        {
            let (sender, receiver) = mpsc::sync_channel(1);
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            host.startup = Some(Startup {
                scope: desired.clone(),
                epoch: host.connection_epoch,
                cancel: cancel.clone(),
                receiver,
            });
            let executable = host.overrides.get(&desired).cloned();
            let repaint = ctx.clone();
            let result = std::thread::Builder::new()
                .name("editor-lsp-start".into())
                .spawn(move || {
                    let result = start_language_server(root, language, executable, &cancel);
                    let _ = sender.send(result);
                    repaint.request_repaint();
                });
            if let Err(error) = result {
                host.startup = None;
                self.view.editor_workbench.language_status = error.to_string();
            }
        }
        let mut events = Vec::new();
        if let Some(server) = &host.server {
            for _ in 0..32 {
                let Some(event) = server.try_recv() else {
                    break;
                };
                events.push(event);
            }
            if server.is_ready() {
                self.view.editor_workbench.language_ready = true;
                self.view.editor_workbench.language_status =
                    format!("{} · conectado", language.lsp_id());
                // Changes coalesce until the typing debounce, but explicit requests synchronize immediately.
                let current = host
                    .opened
                    .get(&absolute)
                    .map(|value| (value.document_id, value.revision));
                if current != Some((snapshot.document_id(), snapshot.revision()))
                    && host.due.is_none()
                {
                    if current.is_some_and(|c| c.0 != snapshot.document_id()) {
                        let _ = server.close(&absolute);
                        host.opened.remove(&absolute);
                    }
                    let result = if host.opened.contains_key(&absolute) {
                        server.change(&absolute, &snapshot)
                    } else {
                        server.open(&absolute, &snapshot, language)
                    };
                    match result {
                        Ok(version) => {
                            host.opened.insert(absolute.clone(), version);
                        }
                        Err(error) => {
                            self.view.editor_workbench.language_status = error.to_string()
                        }
                    }
                }
            }
        }
        if host.parsing || host.due.is_some() {
            ctx.request_repaint_after(Duration::from_millis(32));
        } else if !host.requests.is_empty() {
            // Incoming messages wake immediately. This deadline only drives request timeouts.
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        for event in events {
            changed = true;
            match event {
                Event::Initialized { .. } => {}
                Event::Stopped { reason } => {
                    self.view.editor_workbench.language_ready = false;
                    self.view.editor_workbench.language_status = reason;
                    if let Some(server) = self.editor_services.language.server.take() {
                        retire_server(server);
                    }
                    self.editor_services.language.requests.clear();
                }
                Event::Diagnostics { document, items }
                    if (document.document_id, document.revision)
                        == (snapshot.document_id(), snapshot.revision()) =>
                {
                    self.view.editor_workbench.diagnostic_spans = items
                        .iter()
                        .take(4096)
                        .filter_map(|diagnostic| {
                            let start = utf16_to_offset(&snapshot, diagnostic.range.start)?;
                            let end = utf16_to_offset(&snapshot, diagnostic.range.end)?;
                            (start <= end).then(|| openide_ui::CodeDiagnostic {
                                range: start..end,
                                message: bounded_text(&diagnostic.message, 4096),
                                color: match diagnostic.severity {
                                    Some(1) => crate::theme::DESTRUCTIVE,
                                    Some(2) => openide_ui::Color::rgb(226, 190, 102),
                                    _ => crate::theme::EFFORT_BLUE,
                                },
                            })
                        })
                        .collect::<Vec<_>>()
                        .into();
                    self.view.editor_workbench.diagnostic_revision =
                        Some((document.document_id, document.revision));
                    self.view.editor_workbench.diagnostics = items;
                    self.view.editor_workbench.diagnostics_current = true;
                }
                Event::UnversionedDiagnostics { uri, items }
                    if lsp::file_uri(&absolute).ok().as_ref() == Some(&uri) =>
                {
                    self.view.editor_workbench.diagnostic_spans = Arc::from([]);
                    self.view.editor_workbench.diagnostic_revision = None;
                    self.view.editor_workbench.diagnostics = items;
                    self.view.editor_workbench.diagnostics_current = false;
                }
                Event::Response {
                    id,
                    document,
                    kind,
                    result,
                } => {
                    self.editor_services.language.requests.remove(&id);
                    if (document.document_id, document.revision)
                        != (snapshot.document_id(), snapshot.revision())
                    {
                        continue;
                    }
                    match result {
                        Ok(result) => {
                            if let Err(error) =
                                self.present_editor_language(kind, result, &snapshot)
                            {
                                self.view.editor_workbench.error = Some(error);
                            }
                        }
                        Err(error) => self.view.editor_workbench.error = Some(error.to_string()),
                    }
                }
                _ => {}
            }
        }
        if self
            .editor_services
            .language
            .edit
            .as_ref()
            .is_some_and(|edit| !edit.ready)
        {
            if let Err(error) = self.poll_workspace_edit() {
                self.editor_services.language.edit = None;
                self.view.editor_workbench.error = Some(error);
            }
            ctx.request_repaint_after(Duration::from_millis(16));
            changed = true;
        }
        changed
    }
    pub(super) fn editor_language_request(
        &mut self,
        action: &str,
        value: Option<String>,
    ) -> Result<(), String> {
        let (_, path, _, snapshot) = self.active_editor().ok_or("No hay un archivo abierto")?;
        let language = self
            .view
            .editor_workbench
            .language_override
            .unwrap_or_else(|| crate::editor_workbench_ui::language_for_path(&path));
        let absolute = PathBuf::from(&self.view.workspace).join(path);
        let position = offset_to_utf16(&snapshot, snapshot.selection().head)
            .ok_or("Posición de cursor inválida")?;
        let state = &self.view.editor_workbench;
        let request = match action {
            "completion" => Request::Completion(position),
            "hover" => Request::Hover(position),
            "definition" => Request::Definition(position),
            "references" => Request::References {
                position,
                include_declaration: true,
            },
            "rename" => {
                let name = value
                    .filter(|s| !s.trim().is_empty())
                    .ok_or("Ingresá el nombre nuevo")?;
                Request::Rename {
                    position,
                    new_name: name,
                }
            }
            "format" => Request::Formatting {
                tab_size: state.indent.width() as u32,
                insert_spaces: matches!(state.indent, openide_editor::IndentStyle::Spaces(_)),
            },
            "code_actions" => {
                let selection = snapshot.selection().range();
                Request::CodeActions {
                    range: Range {
                        start: offset_to_utf16(&snapshot, selection.start)
                            .ok_or("Selección inválida")?,
                        end: offset_to_utf16(&snapshot, selection.end)
                            .ok_or("Selección inválida")?,
                    },
                    diagnostics: if state.diagnostics_current {
                        state
                            .diagnostics
                            .iter()
                            .filter_map(|d| serde_json::to_value(d).ok())
                            .collect()
                    } else {
                        Vec::new()
                    },
                }
            }
            _ => return Err("Acción de lenguaje desconocida".into()),
        };
        let host = &mut self.editor_services.language;
        let server = host
            .server
            .as_ref()
            .filter(|s| s.is_ready())
            .ok_or("El servidor de lenguaje todavía no está disponible")?;
        if host
            .opened
            .get(&absolute)
            .is_some_and(|v| v.document_id != snapshot.document_id())
        {
            server.close(&absolute).map_err(|e| e.to_string())?;
            host.opened.remove(&absolute);
        }
        let version = if host.opened.contains_key(&absolute) {
            server.change(&absolute, &snapshot)
        } else {
            server.open(&absolute, &snapshot, language)
        }
        .map_err(|e| e.to_string())?;
        host.opened.insert(absolute.clone(), version);
        let id = server
            .request(&absolute, &snapshot, request)
            .map_err(|e| e.to_string())?;
        host.requests
            .insert(id, (snapshot.document_id(), snapshot.revision()));
        self.view.editor_workbench.notice = Some("Consultando el servidor de lenguaje…".into());
        Ok(())
    }
    fn present_editor_language(
        &mut self,
        kind: RequestKind,
        value: Value,
        snapshot: &BufferSnapshot,
    ) -> Result<(), String> {
        self.view.editor_workbench.notice = None;
        self.view.editor_workbench.results.clear();
        self.view.editor_workbench.hover = None;
        self.view.editor_workbench.result_document =
            Some((snapshot.document_id(), snapshot.revision()));
        match kind {
            RequestKind::Hover => {
                self.view.editor_workbench.results_title = "Documentación".into();
                self.view.editor_workbench.hover = Some(if value.is_null() {
                    "No hay documentación para este símbolo".into()
                } else {
                    excerpt(&value)
                });
            }
            RequestKind::Completion => {
                self.view.editor_workbench.results_title = "Completar código".into();
                let items = value
                    .as_array()
                    .or_else(|| value.get("items").and_then(Value::as_array));
                if let Some(items) = items {
                    self.view.editor_workbench.results = items
                        .iter()
                        .take(80)
                        .map(|item| LanguageResult {
                            label: item
                                .get("label")
                                .and_then(Value::as_str)
                                .unwrap_or("Completar")
                                .into(),
                            detail: item.get("detail").map(excerpt).unwrap_or_default(),
                            value: json!({"completion":item}),
                        })
                        .collect();
                }
            }
            RequestKind::Definition | RequestKind::References => {
                self.view.editor_workbench.results_title = if kind == RequestKind::Definition {
                    "Definiciones"
                } else {
                    "Referencias"
                }
                .into();
                let items = value.as_array().cloned().unwrap_or_else(|| {
                    if value.is_null() {
                        Vec::new()
                    } else {
                        vec![value]
                    }
                });
                for item in items.into_iter().take(80) {
                    let uri = item
                        .get("uri")
                        .or_else(|| item.get("targetUri"))
                        .and_then(Value::as_str)
                        .ok_or("Destino LSP sin URI")?;
                    let range = item
                        .get("range")
                        .or_else(|| item.get("targetSelectionRange"))
                        .cloned()
                        .ok_or("Destino LSP sin rango")?;
                    let range: Range = serde_json::from_value(range).map_err(|e| e.to_string())?;
                    let file = lsp::uri_path(uri).map_err(|e| e.to_string())?;
                    let relative = file
                        .strip_prefix(&self.view.workspace)
                        .map_err(|_| "La referencia está fuera del proyecto abierto")?;
                    self.view.editor_workbench.results.push(LanguageResult {
                        label: format!(
                            "{}:{}:{}",
                            relative.display(),
                            range.start.line + 1,
                            range.start.character + 1
                        ),
                        detail: file.display().to_string(),
                        value: json!({"navigate":relative,"position":range.start}),
                    });
                }
            }
            RequestKind::Formatting => {
                if value.is_null() {
                    self.view.editor_workbench.notice = Some("No hay cambios de formato".into());
                    return Ok(());
                }
                let (_, path, _, _) = self.active_editor().ok_or("Archivo cerrado")?;
                let uri = lsp::file_uri(&PathBuf::from(&self.view.workspace).join(path))
                    .map_err(|e| e.to_string())?;
                self.prepare_workspace_edit(json!({"changes":{uri:value}}), snapshot)?;
            }
            RequestKind::Rename => self.prepare_workspace_edit(value, snapshot)?,
            RequestKind::CodeActions => {
                self.view.editor_workbench.results_title = "Acciones de código".into();
                if let Some(items) = value.as_array() {
                    for item in items
                        .iter()
                        .filter(|i| i.get("edit").is_some() && i.get("disabled").is_none())
                        .take(80)
                    {
                        self.view.editor_workbench.results.push(LanguageResult {
                            label: item
                                .get("title")
                                .and_then(Value::as_str)
                                .unwrap_or("Aplicar cambio")
                                .into(),
                            detail: "Revisar los cambios antes de aplicar".into(),
                            value: json!({"workspace_edit":item["edit"]}),
                        });
                    }
                    if self.view.editor_workbench.results.is_empty() {
                        self.view.editor_workbench.notice=Some("El servidor no ofreció ediciones aplicables; los comandos externos no se ejecutan automáticamente".into());
                    }
                }
            }
            RequestKind::Symbols => {}
        }
        Ok(())
    }
    pub(super) fn show_editor_diagnostics(&mut self) {
        let Some((_, path, _, snapshot)) = self.active_editor() else {
            return;
        };
        let state = &mut self.view.editor_workbench;
        state.results_title = "Problemas del archivo".into();
        state.hover = None;
        state.results.clear();
        state.result_document = None;
        let current = (snapshot.document_id(), snapshot.revision());
        if !state.diagnostics_current || state.diagnostic_revision != Some(current) {
            state.hover = Some(if state.diagnostics.is_empty() {
                "Todavía no hay diagnósticos para esta versión del archivo.".into()
            } else {
                // Keep old reports available as text, without assigning their
                // positions to a newer revision or exposing navigation actions.
                let reports = state
                    .diagnostics
                    .iter()
                    .take(80)
                    .map(|d| format!("• {}", bounded_text(&d.message, 4096)))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("Diagnósticos de una versión anterior. La navegación estará disponible cuando se actualicen.\n\n{reports}")
            });
            return;
        }
        state.result_document = Some(current);
        if state.diagnostics.is_empty() {
            state.hover = Some("No hay problemas en este archivo.".into());
            return;
        }
        state.results = state
            .diagnostics
            .iter()
            .take(80)
            .map(|d| LanguageResult {
                label: format!(
                    "{}:{} · {}",
                    d.range.start.line + 1,
                    d.range.start.character + 1,
                    bounded_text(&d.message, 4096)
                ),
                detail: d
                    .source
                    .as_deref()
                    .map(|source| bounded_text(source, 256))
                    .unwrap_or_default(),
                value: json!({"navigate":path,"position":d.range.start}),
            })
            .collect();
    }
    pub(super) fn apply_editor_language_result(
        &mut self,
        index: &str,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let result = self
            .view
            .editor_workbench
            .results
            .get(index.parse::<usize>().map_err(|_| "Resultado inválido")?)
            .cloned()
            .ok_or("Resultado no disponible")?;
        let (_, _, controller, snapshot) = self.active_editor().ok_or("Archivo cerrado")?;
        if self.view.editor_workbench.result_document
            != Some((snapshot.document_id(), snapshot.revision()))
        {
            return Err("El archivo cambió; solicitá el resultado otra vez".into());
        }
        let value = result.value;
        if value.get("apply_edits").is_some() {
            return self.commit_workspace_edit(ctx);
        }
        if let Some(edit) = value.get("workspace_edit") {
            return self.prepare_workspace_edit(edit.clone(), &snapshot);
        }
        if let Some(relative) = value.get("navigate").and_then(Value::as_str) {
            let relative = PathBuf::from(relative);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|p| !matches!(p, std::path::Component::Normal(_)))
            {
                return Err("Destino fuera del proyecto".into());
            }
            let position: Position =
                serde_json::from_value(value["position"].clone()).map_err(|e| e.to_string())?;
            let scope = self.dock_scope();
            self.dock.open(
                &scope,
                DockTabKind::FilePreview {
                    path: relative.clone(),
                },
            );
            self.dock.set_collapsed(&scope, false);
            self.sync_dock_view();
            self.editor_services.pending_navigation = Some((relative, position));
            return Ok(());
        }
        if let Some(item) = value.get("completion") {
            let mut edits = Vec::new();
            let snippet = item.get("insertTextFormat").and_then(Value::as_u64) == Some(2);
            if let Some(edit) = item.get("textEdit") {
                edits.push(decode_range(&snapshot, edit)?);
            } else {
                let selected = snapshot.selection().range();
                let start = if selected.is_empty() {
                    completion_start(&snapshot, selected.start)?
                } else {
                    selected.start
                };
                let replacement = item
                    .get("insertText")
                    .or_else(|| item.get("label"))
                    .and_then(Value::as_str)
                    .ok_or("Sugerencia sin texto")?
                    .to_owned();
                edits.push(TextEdit {
                    range: start..selected.end,
                    replacement,
                });
            }
            if let Some(additional) = item.get("additionalTextEdits").and_then(Value::as_array) {
                for edit in additional {
                    edits.push(decode_range(&snapshot, edit)?);
                }
            }
            if snippet && edits.len() == 1 {
                let edit = edits.pop().unwrap();
                self.editor_command(
                    EditorCommand::SetSelection(Selection {
                        anchor: edit.range.start,
                        head: edit.range.end,
                    }),
                    ctx,
                )?;
                self.editor_command(EditorCommand::InsertSnippet(edit.replacement), ctx)?;
            } else {
                if snippet {
                    return Err(
                        "El servidor envió un snippet con ediciones adicionales no compatibles"
                            .into(),
                    );
                }
                let target = completion_caret(&edits)?;
                let shared = controller.shared_buffer();
                shared
                    .lock()
                    .map_err(|_| "Editor no disponible")?
                    .apply_transaction(&edits, Selection::caret(target), EditKind::Replace, 0)
                    .map_err(|e| e.to_string())?;
                self.editor_command(EditorCommand::SetSelection(Selection::caret(target)), ctx)?;
            }
            self.view.editor_workbench.results.clear();
            return Ok(());
        }
        Err("Resultado sin acción compatible".into())
    }
    fn prepare_workspace_edit(
        &mut self,
        value: Value,
        snapshot: &BufferSnapshot,
    ) -> Result<(), String> {
        let (_, source_path, _, current) =
            self.active_editor().ok_or("Archivo de origen cerrado")?;
        if (snapshot.document_id(), snapshot.revision())
            != (current.document_id(), current.revision())
        {
            return Err("El archivo cambió; solicitá la acción otra vez".into());
        }
        let workspace = PathBuf::from(&self.view.workspace);
        let mut changes = BTreeMap::<PathBuf, Vec<Value>>::new();
        let mut versions = BTreeMap::new();
        let mut add = |uri: &str, edits: &Value, version: Option<i32>| -> Result<(), String> {
            let absolute = lsp::uri_path(uri).map_err(|e| e.to_string())?;
            let path = absolute
                .strip_prefix(&workspace)
                .map_err(|_| "La edición sale del proyecto abierto")?
                .to_path_buf();
            if path.as_os_str().is_empty()
                || path
                    .components()
                    .any(|p| !matches!(p, std::path::Component::Normal(_)))
            {
                return Err("Ruta de edición inválida".into());
            }
            let entries = edits.as_array().ok_or("Ediciones LSP inválidas")?;
            if entries.len() > 4096
                || changes
                    .values()
                    .map(Vec::len)
                    .sum::<usize>()
                    .saturating_add(entries.len())
                    > 4096
            {
                return Err("La acción supera el límite de ediciones".into());
            }
            if let Some(version) = version {
                if versions
                    .insert(path.clone(), version)
                    .is_some_and(|previous| previous != version)
                {
                    return Err("La acción mezcla versiones del mismo archivo".into());
                }
            }
            changes.entry(path).or_default().extend(entries.clone());
            if changes.len() > 16 {
                return Err("La acción supera el límite de archivos".into());
            }
            Ok(())
        };
        if let Some(edits) = value.get("changes").and_then(Value::as_object) {
            for (uri, edits) in edits {
                add(uri, edits, None)?;
            }
        }
        if let Some(edits) = value.get("documentChanges").and_then(Value::as_array) {
            for edit in edits {
                if edit.get("kind").is_some() {
                    return Err("La acción contiene crear/renombrar/eliminar archivos; se requiere una operación de archivos explícita".into());
                }
                let uri = edit["textDocument"]["uri"]
                    .as_str()
                    .ok_or("Documento LSP sin URI")?;
                let version = match edit["textDocument"].get("version") {
                    None | Some(Value::Null) => None,
                    Some(version) => Some(
                        version
                            .as_i64()
                            .and_then(|version| i32::try_from(version).ok())
                            .filter(|version| *version >= 0)
                            .ok_or("Versión LSP inválida")?,
                    ),
                };
                add(uri, &edit["edits"], version)?;
            }
        }
        if changes.is_empty() {
            self.view.editor_workbench.notice = Some("No hay cambios que aplicar".into());
            return Ok(());
        }
        let mut expected = BTreeMap::new();
        for path in changes.keys() {
            if let Some(controller) = self.code_editor_controller(path) {
                let snapshot = controller.snapshot()?;
                let identity = (snapshot.document_id(), snapshot.revision());
                let synchronized = self
                    .editor_services
                    .language
                    .opened
                    .get(&workspace.join(path));
                if synchronized
                    .is_some_and(|version| (version.document_id, version.revision) != identity)
                    || (snapshot.is_dirty() && synchronized.is_none())
                {
                    return Err("Un archivo tiene cambios que el servidor todavía no conoce; sincronizalo y solicitá la acción otra vez".into());
                }
                expected.insert(path.clone(), identity);
            }
            if let Some(version) = versions.get(path) {
                let synced = self
                    .editor_services
                    .language
                    .opened
                    .get(&workspace.join(path))
                    .ok_or(
                        "La acción requiere una versión de archivo que ya no está sincronizada",
                    )?;
                if synced.version != *version
                    || expected.get(path) != Some(&(synced.document_id, synced.revision))
                {
                    return Err("El servidor editó una versión anterior del archivo; solicitá la acción otra vez".into());
                }
            }
        }
        let awaiting = changes.keys().cloned().collect();
        self.editor_services.language.edit = Some(WorkspaceEdit {
            source: (snapshot.document_id(), snapshot.revision()),
            source_path: PathBuf::from(source_path),
            workspace,
            changes,
            versions,
            expected,
            loaded: Vec::new(),
            awaiting,
            ready: false,
        });
        self.view.editor_workbench.notice = Some("Preparando la revisión de cambios…".into());
        Ok(())
    }
    fn poll_workspace_edit(&mut self) -> Result<(), String> {
        let Some(mut edit) = self.editor_services.language.edit.take() else {
            return Ok(());
        };
        valid_source(
            &edit,
            Path::new(&self.view.workspace),
            self.code_editor_controller(&edit.source_path),
        )?;
        if let Some(path) = edit.awaiting.front().cloned() {
            if let Some(controller) = self.ensure_code_editor_loaded(&path)? {
                let snapshot = controller.snapshot()?;
                if edit.expected.get(&path).is_some_and(|expected| {
                    *expected != (snapshot.document_id(), snapshot.revision())
                }) {
                    return Err("Un archivo cambió mientras se preparaba la acción".into());
                }
                if let Some(version) = edit.versions.get(&path) {
                    let synced = self
                        .editor_services
                        .language
                        .opened
                        .get(&edit.workspace.join(&path))
                        .ok_or("El documento dejó de estar sincronizado")?;
                    if synced.version != *version
                        || (synced.document_id, synced.revision)
                            != (snapshot.document_id(), snapshot.revision())
                    {
                        return Err("La versión LSP dejó de coincidir durante la revisión".into());
                    }
                }
                let edits = lsp::text_edits(&snapshot, &Value::Array(edit.changes[&path].clone()))
                    .map_err(|e| e.to_string())?;
                controller
                    .shared_buffer()
                    .lock()
                    .map_err(|_| "Editor no disponible")?
                    .validate_transaction(&edits, Selection::caret(0))
                    .map_err(|e| e.to_string())?;
                edit.loaded.push(EditTarget {
                    path,
                    snapshot,
                    controller,
                    edits,
                });
                edit.awaiting.pop_front();
            }
        }
        if edit.awaiting.is_empty() {
            valid_source(
                &edit,
                Path::new(&self.view.workspace),
                self.code_editor_controller(&edit.source_path),
            )?;
            edit.ready = true;
            let state = &mut self.view.editor_workbench;
            state.results_title = "Revisar cambios del lenguaje".into();
            state.result_document = Some(edit.source);
            state.results = vec![LanguageResult {
                label: format!(
                    "Aplicar {} ediciones en {} archivos",
                    edit.loaded.iter().map(|t| t.edits.len()).sum::<usize>(),
                    edit.loaded.len()
                ),
                detail: "Los cambios quedan sin guardar y se pueden deshacer en cada archivo"
                    .into(),
                value: json!({"apply_edits":true}),
            }];
            state.hover = Some(
                edit.loaded
                    .iter()
                    .map(|target| {
                        format!(
                            "{}\n{}",
                            target.path.display(),
                            target
                                .edits
                                .iter()
                                .take(12)
                                .map(|e| format!(
                                    "L{}\n− {}\n+ {}",
                                    target
                                        .snapshot
                                        .position_of(e.range.start)
                                        .map_or(1, |p| p.row + 1),
                                    target
                                        .snapshot
                                        .slice(e.range.clone())
                                        .map(|slice| slice.chars().take(160).collect::<String>())
                                        .unwrap_or_default(),
                                    e.replacement.chars().take(160).collect::<String>()
                                ))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            );
            state.notice = None;
        }
        self.editor_services.language.edit = Some(edit);
        Ok(())
    }
    fn commit_workspace_edit(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let edit = self
            .editor_services
            .language
            .edit
            .take()
            .ok_or("No hay cambios preparados")?;
        if !edit.ready {
            return Err("Los cambios todavía están cargando".into());
        }
        let source = self
            .code_editor_controller(&edit.source_path)
            .ok_or("El archivo de origen se cerró")?;
        valid_source(&edit, Path::new(&self.view.workspace), Some(source.clone()))?;
        let mut buffers = BTreeMap::new();
        buffers.insert(edit.source.0, source.shared_buffer());
        for target in &edit.loaded {
            let current = self
                .code_editor_controller(&target.path)
                .ok_or("Un archivo de destino se cerró")?;
            let current = current.snapshot()?;
            if (current.document_id(), current.revision())
                != (target.snapshot.document_id(), target.snapshot.revision())
            {
                return Err(
                    "Un archivo cambió desde la revisión; solicitá la acción otra vez".into(),
                );
            }
            buffers.insert(
                target.snapshot.document_id(),
                target.controller.shared_buffer(),
            );
        }
        let mut locks = BTreeMap::new();
        for (id, buffer) in &buffers {
            locks.insert(*id, buffer.lock().map_err(|_| "Editor no disponible")?);
        }
        if locks
            .get(&edit.source.0)
            .is_none_or(|buffer| buffer.revision() != edit.source.1)
        {
            return Err("El origen cambió durante la revisión".into());
        }
        for target in &edit.loaded {
            let buffer = locks
                .get(&target.snapshot.document_id())
                .ok_or("Destino no disponible")?;
            if buffer.revision() != target.snapshot.revision() {
                return Err(
                    "Un archivo cambió desde la revisión; solicitá la acción otra vez".into(),
                );
            }
            buffer
                .validate_transaction(&target.edits, Selection::caret(0))
                .map_err(|e| e.to_string())?;
        }
        // Locks cover source and every target; all real buffer limits/revisions were validated.
        for target in &edit.loaded {
            locks
                .get_mut(&target.snapshot.document_id())
                .unwrap()
                .apply_transaction(&target.edits, Selection::caret(0), EditKind::Replace, 0)
                .map_err(|e| e.to_string())?;
        }
        drop(locks);
        self.view.editor_workbench.results.clear();
        self.view.editor_workbench.hover = None;
        self.view.editor_workbench.notice = Some(format!(
            "Cambios aplicados en {} archivos; guardalos cuando termines la revisión",
            edit.loaded.len()
        ));
        self.refresh_code_editor_titles();
        ctx.request_repaint();
        Ok(())
    }
}

#[cfg(test)]
#[path = "editor_language_e2e_tests.rs"]
mod tests;
