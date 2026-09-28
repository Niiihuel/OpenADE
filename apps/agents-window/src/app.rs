use crate::{
    agent_tool_settings::AgentToolSettings,
    automation_view::{
        template_form, AutomationFilter, AutomationFrequency, AutomationScreen, AutomationViewState,
    },
    automations::{AutomationDispatch, AutomationStore},
    command_center::{self, FileCandidate, Scope, SessionCandidate, Target},
    composer_selection::{self, ComposerChoice, ComposerPreferences},
    detail_worker::{DetailKind, DetailRequest, DetailResult, DetailWorker},
    dock::{DockOwner, DockScope, DockState, DockTabId, DockTabKind},
    files::FileExplorer,
    git_review::DiffStage,
    github::GithubStatus,
    github_account::{GithubAccountStatus, GithubAccountWorker},
    github_worker::{GithubRequest, GithubWorker},
    marketplace::{InstallScope, MarketplaceStore, SourceKind},
    marketplace_view::MarketplaceViewState,
    model_catalog::{self, CatalogSnapshot},
    native_provider::{self, NativeProviderSettings},
    navigation::{NavigationHistory, NavigationTarget},
    render,
    review_worker::{ReviewRequest, ReviewWorker},
    search_worker::{SearchRequest, SearchResponse, SearchWorker},
    session_groups::SessionGroups,
    settings_preferences::{SettingsPreferences, TerminalShell},
    settings_ui::{self, SettingsPage},
    terminal::TerminalSession,
    transcript::TranscriptProjection,
    view::{
        build_command_center, build_view, dock_layout, ComposerAttachmentView, DockLayout,
        DockTabView, FilePreviewView, FileRowView, MainPage, PermissionView, ReviewDetailView,
        SessionOrganization, SessionView, SideTab, ViewState,
    },
};
use eframe::egui;
use openide_cli::{CliProfile, CliRunHandle, CliRunner};
use openide_core::{
    AgentEvent, AgentEventKind, AgentMessage, AgentRunRequest, PermissionDecision, SessionSort,
    SessionStore, ToolCall,
};
use openide_harness::{
    AgentRunHandle, AgentRunner, LocalTools, OpenAiCompatibleBackend, PermissionChoice,
    PermissionPolicy,
};
use openide_ui::{
    Easing, Element, FontFamily, FontMetrics, Frame as UiFrame, ScalarTransition, Size, UiAction,
    UiEvent, UiTree,
};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{
    mpsc::{self, Receiver, TryRecvError},
    Arc,
};
use std::time::{Duration, Instant};

enum ActiveRun {
    Native(AgentRunHandle),
    Cli(CliRunHandle),
}

struct BackgroundAutomationRun {
    automation_id: String,
    session_id: String,
    run: ActiveRun,
}

enum MarketplaceOperation {
    Load,
    Refresh,
    AddLocal(PathBuf),
    AddGithub(String),
    Install(String, InstallScope),
    SetEnabled(String, bool),
    Uninstall(String),
    RemoveSource(String),
}

type MarketplaceResponse = (
    PathBuf,
    Result<(crate::marketplace::MarketplaceSnapshot, String), String>,
);

fn install_text_measurer(tree: &mut UiTree, context: &egui::Context) {
    let context = context.clone();
    tree.set_text_measurer(move |value, size, family| {
        let family = match family {
            FontFamily::Monospace => egui::FontFamily::Monospace,
            FontFamily::Sans => egui::FontFamily::Proportional,
        };
        context.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    value.to_owned(),
                    egui::FontId::new(size, family),
                    egui::Color32::WHITE,
                )
                .size()
                .x
        })
    });
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum DockDrag {
    Left,
    Right,
}

/// Layout widths stay in the host, not in a rebuilt visual element. This
/// keeps painting and hit testing on the same geometry throughout a toggle.
struct DockMotion {
    left: ScalarTransition,
    right: ScalarTransition,
    viewport_width: f32,
}

impl DockMotion {
    const DURATION: Duration = Duration::from_millis(200);

    fn new(left: f32, right: f32, viewport_width: f32, now: Instant) -> Self {
        Self {
            left: ScalarTransition::new(left, now).easing(Easing::OutCubic),
            right: ScalarTransition::new(right, now).easing(Easing::OutCubic),
            viewport_width,
        }
    }

    fn update(
        &mut self,
        target_left: f32,
        target_right: f32,
        viewport_width: f32,
        now: Instant,
        reduced_motion: bool,
    ) -> (Option<f32>, Option<f32>) {
        let resized = (self.viewport_width - viewport_width).abs() > 0.5;
        self.viewport_width = viewport_width;
        for (transition, target) in [
            (&mut self.left, target_left),
            (&mut self.right, target_right),
        ] {
            let visibility_changed = (transition.target() > 0.5) != (target > 0.5);
            if visibility_changed {
                transition.retarget_with_motion(target, now, Self::DURATION, reduced_motion);
            } else if resized || (transition.target() - target).abs() > 0.5 {
                // Resizing or dragging a divider follows the pointer immediately.
                transition.snap_to(target, now);
            }
        }
        (
            self.left.is_active(now).then(|| self.left.value(now)),
            self.right.is_active(now).then(|| self.right.value(now)),
        )
    }

    fn is_active(&self, now: Instant) -> bool {
        self.left.is_active(now) || self.right.is_active(now)
    }
}

/// Keep the already measured frame on idle or animation-only ticks. Building
/// and measuring the entire tree again is only necessary when an action or
/// asynchronous response changes it after the hit-test layout.
fn relayout_after_updates(
    tree: &mut UiTree,
    current: UiFrame,
    viewport: Size,
    changed: bool,
    rebuild: impl FnOnce() -> Element,
) -> UiFrame {
    if !changed {
        return current;
    }
    tree.set_root(rebuild());
    tree.layout(viewport)
}

impl ActiveRun {
    fn resolve_permission(&self, request_id: &str, choice: PermissionChoice) -> bool {
        match self {
            Self::Native(run) => run.resolve_permission(request_id, choice),
            Self::Cli(_) => false,
        }
    }

    fn cancel(&self) {
        match self {
            Self::Native(run) => run.cancel(),
            Self::Cli(run) => run.cancel(),
        }
    }

    fn drain(&self) -> (Vec<AgentEvent>, bool) {
        let receiver = match self {
            Self::Native(run) => &run.events,
            Self::Cli(run) => &run.events,
        };
        let mut events = Vec::new();
        loop {
            match receiver.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => return (events, false),
                Err(TryRecvError::Disconnected) => return (events, true),
            }
        }
    }
}

pub struct AgentsWindow {
    data_root: PathBuf,
    view: ViewState,
    composer_preferences: ComposerPreferences,
    settings_preferences: SettingsPreferences,
    agent_tool_settings: AgentToolSettings,
    tree: UiTree,
    projection: TranscriptProjection,
    profiles: Vec<CliProfile>,
    store: Option<SessionStore>,
    /// Project paths from every stored session, before sidebar archive filtering.
    known_project_paths: Vec<String>,
    active_run: Option<ActiveRun>,
    automation_stores: Vec<AutomationStore>,
    automation_last_poll: Instant,
    background_automation_runs: Vec<BackgroundAutomationRun>,
    automation_queue: VecDeque<(usize, AutomationDispatch)>,
    marketplace_response: Option<Receiver<MarketplaceResponse>>,
    workspace_folder_response: Option<Receiver<Result<Option<PathBuf>, String>>>,
    catalog_response: Option<Receiver<Result<CatalogSnapshot, String>>>,
    catalog_last_refresh: Instant,
    history: Vec<AgentMessage>,
    last_sequence: u64,
    dock_drag: Option<DockDrag>,
    dock_motion: Option<DockMotion>,
    reduced_motion: bool,
    pending_prompt: Option<String>,
    pending_attachments: Vec<ComposerAttachmentView>,
    run_has_output: bool,
    viewport_width: f32,
    viewport_height: f32,
    dock: DockState,
    terminals: HashMap<u64, TerminalSession>,
    dock_seeded_workspaces: HashSet<String>,
    file_explorer: Option<FileExplorer>,
    drafts: HashMap<(String, Option<String>), String>,
    attachment_drafts: HashMap<(String, Option<String>), Vec<ComposerAttachmentView>>,
    draft_dirty_at: Option<Instant>,
    draft_error: Option<String>,
    persistence_error: Option<String>,
    navigation: NavigationHistory,
    command_tree: UiTree,
    command_center_attaches_file: bool,
    command_focus_pending: bool,
    prompt_focus_pending: bool,
    group_focus_pending: bool,
    search_worker: Option<SearchWorker>,
    search_generation: u64,
    search_pending_at: Option<Instant>,
    search_response: Option<SearchResponse>,
    search_in_flight: bool,
    review_worker: Option<ReviewWorker>,
    review_generation: u64,
    review_active_workspace: Option<PathBuf>,
    review_in_flight: bool,
    detail_worker: Option<DetailWorker>,
    detail_generation: u64,
    detail_request: Option<DetailRequest>,
    github_worker: Option<GithubWorker>,
    github_status: GithubStatus,
    github_in_flight: bool,
    github_account_worker: Option<GithubAccountWorker>,
}

fn data_root() -> PathBuf {
    if let Some(base) = std::env::var_os("APPDATA") {
        return PathBuf::from(base).join("OpenIDE");
    }
    if let Some(base) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(base).join("openide");
    }
    if let Some(base) = std::env::var_os("HOME") {
        return PathBuf::from(base).join(".local/share/openide");
    }
    std::env::current_dir().unwrap_or_default().join("data")
}

/// Runs off the UI thread. PowerShell hosts the Windows folder dialog without
/// opening a console; the selected path is passed back over stdout as UTF-8.
#[cfg(windows)]
fn choose_workspace_folder(initial: &Path) -> Result<Option<PathBuf>, String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName System.Windows.Forms
$dialog = New-Object System.Windows.Forms.FolderBrowserDialog
$dialog.Description = 'Seleccionar carpeta de proyecto'
$dialog.ShowNewFolderButton = $true
if ([System.IO.Directory]::Exists($env:OPENIDE_INITIAL_FOLDER)) {
    $dialog.SelectedPath = $env:OPENIDE_INITIAL_FOLDER
}
try {
    if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
        [Console]::Out.Write($dialog.SelectedPath)
    }
} finally {
    $dialog.Dispose()
}
"#;

    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Sta",
            "-WindowStyle",
            "Hidden",
            "-Command",
            SCRIPT,
        ])
        .env("OPENIDE_INITIAL_FOLDER", initial)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("No se pudo abrir el selector de carpetas: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "No se pudo abrir el selector de carpetas: {}",
            detail.trim()
        ));
    }
    let selected = String::from_utf8(output.stdout)
        .map_err(|_| "El selector devolvió una ruta con codificación inválida".to_owned())?;
    if selected.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(selected)))
    }
}

#[cfg(not(windows))]
fn choose_workspace_folder(_initial: &Path) -> Result<Option<PathBuf>, String> {
    Err("El selector de carpetas del sistema no está disponible aquí".to_owned())
}

fn validate_workspace_folder(path: &Path) -> Result<String, String> {
    if !path.is_absolute() || !path.is_dir() {
        return Err("Selecciona una carpeta local existente".to_owned());
    }
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "La ruta de la carpeta no se puede mostrar".to_owned())
}

fn project_branch_for_workspace(workspace: &str) -> Option<String> {
    let branch = crate::git_review::read_branch(Path::new(workspace));
    (branch != "Rama desconocida").then_some(branch)
}

const MAX_COMPOSER_ATTACHMENTS: usize = 12;
const MAX_COMPOSER_ATTACHMENT_PATH_BYTES: usize = 2_048;
const MAX_COMPOSER_PROJECTS: usize = 12;

fn composer_attachment_for_path(
    workspace: &Path,
    candidate: &Path,
) -> Result<ComposerAttachmentView, String> {
    let root = workspace
        .canonicalize()
        .map_err(|error| format!("No se pudo leer el proyecto: {error}"))?;
    let file = workspace
        .join(candidate)
        .canonicalize()
        .map_err(|error| format!("No se pudo leer el archivo: {error}"))?;
    if !file.is_file() {
        return Err("Selecciona un archivo del proyecto".to_owned());
    }
    let relative = file
        .strip_prefix(&root)
        .map_err(|_| "El archivo está fuera del proyecto".to_owned())?;
    let path = relative.to_string_lossy().replace('\\', "/");
    if path.is_empty() || path.len() > MAX_COMPOSER_ATTACHMENT_PATH_BYTES {
        return Err("La ruta del archivo es demasiado larga".to_owned());
    }
    let label = relative
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&path)
        .to_owned();
    Ok(ComposerAttachmentView { path, label })
}

fn composer_prompt_with_attachments(
    visible_prompt: &str,
    attachments: &[ComposerAttachmentView],
) -> String {
    if attachments.is_empty() {
        return visible_prompt.to_owned();
    }
    let paths: Vec<&str> = attachments.iter().map(|file| file.path.as_str()).collect();
    let listed = serde_json::to_string(&paths).expect("file paths serialize as JSON strings");
    let introduction = if visible_prompt.is_empty() {
        "Revisa los archivos adjuntos."
    } else {
        visible_prompt
    };
    format!(
        "{introduction}\n\nArchivos adjuntos de este proyecto (rutas relativas): {listed}\nAbre estos archivos y úsalos como contexto para esta tarea."
    )
}

fn composer_project_options(workspace: &str, project_paths: &[String]) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut options = Vec::new();
    for path in std::iter::once(workspace).chain(project_paths.iter().map(String::as_str)) {
        if path.is_empty() {
            continue;
        }
        let key = path.replace('\\', "/");
        let key = if cfg!(windows) {
            key.to_lowercase()
        } else {
            key
        };
        if !seen.insert(key) {
            continue;
        }
        let label = Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(path)
            .to_owned();
        options.push((path.to_owned(), label));
        if options.len() >= MAX_COMPOSER_PROJECTS {
            break;
        }
    }
    let mut name_counts = HashMap::new();
    for (_, label) in &options {
        *name_counts.entry(label.clone()).or_insert(0usize) += 1;
    }
    for (path, label) in &mut options {
        if name_counts.get(label).is_some_and(|count| *count > 1) {
            let parent = Path::new(path)
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or(path);
            *label = format!("{label} · {parent}");
        }
    }
    let mut labels = HashSet::new();
    for (path, label) in &mut options {
        if !labels.insert(label.clone()) {
            *label = path.clone();
        }
    }
    options
}

fn terminal_key_bytes(key: openide_ui::Key, shift: bool, ctrl: bool) -> Option<&'static [u8]> {
    use openide_ui::Key;
    if ctrl {
        let sequence = match key {
            Key::A => Some(b"\x01".as_slice()),
            Key::B => Some(b"\x02".as_slice()),
            Key::C => Some(b"\x03".as_slice()),
            Key::D => Some(b"\x04".as_slice()),
            Key::E => Some(b"\x05".as_slice()),
            Key::F => Some(b"\x06".as_slice()),
            Key::G => Some(b"\x07".as_slice()),
            Key::H => Some(b"\x08".as_slice()),
            Key::I => Some(b"\x09".as_slice()),
            Key::J => Some(b"\x0a".as_slice()),
            Key::K => Some(b"\x0b".as_slice()),
            Key::L => Some(b"\x0c".as_slice()),
            Key::M => Some(b"\x0d".as_slice()),
            Key::N => Some(b"\x0e".as_slice()),
            Key::O => Some(b"\x0f".as_slice()),
            Key::P => Some(b"\x10".as_slice()),
            Key::Q => Some(b"\x11".as_slice()),
            Key::R => Some(b"\x12".as_slice()),
            Key::S => Some(b"\x13".as_slice()),
            Key::T => Some(b"\x14".as_slice()),
            Key::U => Some(b"\x15".as_slice()),
            Key::V => Some(b"\x16".as_slice()),
            Key::W => Some(b"\x17".as_slice()),
            Key::X => Some(b"\x18".as_slice()),
            Key::Y => Some(b"\x19".as_slice()),
            Key::Z => Some(b"\x1a".as_slice()),
            _ => None,
        };
        if sequence.is_some() {
            return sequence;
        }
    }
    match key {
        Key::Enter => Some(b"\r"),
        Key::Tab if !shift => Some(b"\t"),
        Key::Tab => None, // Keep Shift+Tab available for focus navigation.
        Key::Escape => Some(b"\x1b"),
        Key::Backspace => Some(b"\x7f"),
        Key::Delete => Some(b"\x1b[3~"),
        Key::ArrowUp => Some(b"\x1b[A"),
        Key::ArrowDown => Some(b"\x1b[B"),
        Key::ArrowRight => Some(b"\x1b[C"),
        Key::ArrowLeft => Some(b"\x1b[D"),
        Key::Home => Some(b"\x1b[H"),
        Key::End => Some(b"\x1b[F"),
        _ => None,
    }
}

fn catalog_status(snapshot: &CatalogSnapshot) -> String {
    let model_count: usize = snapshot
        .providers
        .values()
        .map(|provider| provider.models.len())
        .sum();
    let checked = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
        snapshot.fetched_at_ms.min(i64::MAX as u64) as i64,
    )
    .map(|date| date.format("%Y-%m-%d %H:%M UTC").to_string())
    .unwrap_or_else(|| "fecha desconocida".to_owned());
    format!(
        "Comprobado {checked}: {} proveedores, {model_count} modelos",
        snapshot.providers.len()
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SidebarPreferences {
    organization: SessionOrganization,
    sort: SessionSort,
    collapsed_projects: BTreeSet<String>,
    sidebar_collapsed: bool,
}

static SIDEBAR_PREFERENCES_SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl SidebarPreferences {
    fn from_value(value: &serde_json::Value) -> Self {
        let organization = match value.get("organization").and_then(|value| value.as_str()) {
            Some("recent") => SessionOrganization::Recent,
            Some("group") => SessionOrganization::Group,
            _ => SessionOrganization::Project,
        };
        let sort = match value.get("sort").and_then(|value| value.as_str()) {
            Some("created") => SessionSort::Created,
            _ => SessionSort::Updated,
        };
        let collapsed_projects = value
            .get("collapsed_projects")
            .and_then(|value| value.as_array())
            .into_iter()
            .flat_map(|paths| paths.iter().take(512))
            .filter_map(|path| path.as_str())
            .filter(|path| {
                !path.is_empty() && path.len() <= 4096 && !path.chars().any(char::is_control)
            })
            .map(str::to_owned)
            .collect();
        let sidebar_collapsed = value
            .get("sidebar_collapsed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        Self {
            organization,
            sort,
            collapsed_projects,
            sidebar_collapsed,
        }
    }

    fn load(root: &Path) -> Self {
        let path = root.join("sidebar-preferences.json");
        if !std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= 256 * 1024) {
            return Self::default();
        }
        std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .map(|value| Self::from_value(&value))
            .unwrap_or_default()
    }

    fn save(&self, root: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(root)?;
        let value = serde_json::json!({
            "version": 1,
            "organization": match self.organization {
                SessionOrganization::Project => "project",
                SessionOrganization::Recent => "recent",
                SessionOrganization::Group => "group",
            },
            "sort": match self.sort {
                SessionSort::Updated => "updated",
                SessionSort::Created => "created",
            },
            "collapsed_projects": self.collapsed_projects,
            "sidebar_collapsed": self.sidebar_collapsed,
        });
        let payload = serde_json::to_vec_pretty(&value)?;
        let temp = root.join(format!(
            ".sidebar-preferences-{}-{}.tmp",
            std::process::id(),
            SIDEBAR_PREFERENCES_SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&payload)?;
            file.sync_all()?;
            std::fs::rename(&temp, root.join("sidebar-preferences.json"))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
}

fn sort_session_views(
    sessions: &mut [(u64, u64, SessionView)],
    organization: SessionOrganization,
    sort: SessionSort,
    archived: bool,
) {
    let timestamp = |entry: &(u64, u64, SessionView)| match sort {
        SessionSort::Updated => entry.0,
        SessionSort::Created => entry.1,
    };
    let mut project_order: HashMap<String, u64> = HashMap::new();
    for entry in sessions.iter() {
        project_order
            .entry(entry.2.project.clone())
            .and_modify(|current| *current = (*current).max(timestamp(entry)))
            .or_insert_with(|| timestamp(entry));
    }
    sessions.sort_by(|left, right| {
        right.2.pinned.cmp(&left.2.pinned).then_with(|| {
            let group_order = if archived || organization != SessionOrganization::Project {
                std::cmp::Ordering::Equal
            } else {
                project_order[&right.2.project]
                    .cmp(&project_order[&left.2.project])
                    .then_with(|| left.2.project.cmp(&right.2.project))
            };
            group_order
                .then_with(|| timestamp(right).cmp(&timestamp(left)))
                .then_with(|| right.0.cmp(&left.0))
                .then_with(|| left.2.id.cmp(&right.2.id))
        })
    });
}

fn load_profiles(root: &Path) -> Vec<CliProfile> {
    let mut profiles = CliProfile::builtins();
    if let Ok(data) = std::fs::read_to_string(root.join("cli-profiles.json")) {
        if let Ok(custom) = serde_json::from_str::<Vec<CliProfile>>(&data) {
            for profile in custom {
                if let Some(existing) = profiles.iter_mut().find(|entry| entry.id == profile.id) {
                    *existing = profile;
                } else {
                    profiles.push(profile);
                }
            }
        }
    }
    profiles
}

fn flush_tool_batch(
    history: &mut Vec<AgentMessage>,
    content: &mut String,
    calls: &mut Vec<ToolCall>,
    results: &mut Vec<AgentMessage>,
) {
    if !calls.is_empty() && calls.len() == results.len() {
        history.push(AgentMessage::assistant(
            std::mem::take(content),
            std::mem::take(calls),
        ));
        history.append(results);
    } else {
        content.clear();
        calls.clear();
        results.clear();
    }
}

/// Rebuilds the exact model-visible tool exchange from durable turn events.
fn history_from_events(events: &[AgentEvent]) -> Vec<AgentMessage> {
    let mut history = Vec::new();
    let mut content = String::new();
    let mut calls = Vec::new();
    let mut results = Vec::new();
    let mut turn_start_len = 0;
    let mut turn_had_output = false;
    for event in events {
        match &event.kind {
            AgentEventKind::TurnStarted { prompt } => {
                turn_start_len = history.len();
                turn_had_output = false;
                history.push(AgentMessage::user(prompt.clone()));
            }
            AgentEventKind::ModelRequestStarted { step, .. } if *step > 1 => {
                flush_tool_batch(&mut history, &mut content, &mut calls, &mut results);
            }
            AgentEventKind::AssistantMessage { content: message } => {
                content = message.clone();
                turn_had_output = true;
            }
            AgentEventKind::ToolStarted { tool_call } => {
                calls.push(tool_call.clone());
                turn_had_output = true;
            }
            AgentEventKind::AssistantDelta { .. } | AgentEventKind::ProcessOutput { .. } => {
                turn_had_output = true;
            }
            AgentEventKind::ToolFinished {
                tool_call_id,
                output,
                ..
            } => {
                if calls.iter().any(|call| call.id == *tool_call_id) {
                    results.push(AgentMessage::tool(tool_call_id.clone(), output.clone()));
                }
            }
            AgentEventKind::TurnCompleted { response, .. } => {
                flush_tool_batch(&mut history, &mut content, &mut calls, &mut results);
                history.push(AgentMessage::assistant(response.clone(), Vec::new()));
            }
            AgentEventKind::TurnCancelled | AgentEventKind::TurnFailed { .. } => {
                flush_tool_batch(&mut history, &mut content, &mut calls, &mut results);
                if !turn_had_output {
                    history.truncate(turn_start_len);
                }
            }
            _ => {}
        }
    }
    history
}

impl AgentsWindow {
    fn draft_key(&self) -> (String, Option<String>) {
        (
            self.view.workspace.clone(),
            self.view.selected_session.clone(),
        )
    }

    fn save_draft(&mut self) {
        let key = self.draft_key();
        if self.view.composer_attachments.is_empty() {
            self.attachment_drafts.remove(&key);
        } else {
            self.attachment_drafts
                .insert(key.clone(), self.view.composer_attachments.clone());
        }
        let changed =
            self.drafts.get(&key).map(String::as_str).unwrap_or("") != self.view.prompt.as_str();
        if !changed && self.draft_dirty_at.is_none() {
            return;
        }
        if self.view.prompt.is_empty() {
            self.drafts.remove(&key);
        } else {
            self.drafts.insert(key, self.view.prompt.clone());
        }
        self.draft_dirty_at = None;
        if let Some(store) = &self.store {
            match store.save_draft(
                Path::new(&self.view.workspace),
                self.view.selected_session.as_deref(),
                &self.view.prompt,
            ) {
                Ok(()) => self.draft_error = None,
                Err(error) => self.draft_error = Some(error.to_string()),
            }
        }
    }

    fn restore_draft(&mut self) {
        let key = self.draft_key();
        self.view.composer_attachments = self
            .attachment_drafts
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let prompt = if let Some(prompt) = self.drafts.get(&key) {
            prompt.clone()
        } else if let Some(store) = &self.store {
            match store.load_draft(Path::new(&key.0), key.1.as_deref()) {
                Ok(Some(prompt)) => {
                    self.drafts.insert(key, prompt.clone());
                    self.draft_error = None;
                    prompt
                }
                Ok(None) => String::new(),
                Err(error) => {
                    self.draft_error = Some(error.to_string());
                    String::new()
                }
            }
        } else {
            String::new()
        };
        self.view.prompt = prompt.clone();
        self.tree.set_input_value("prompt", prompt);
        self.draft_dirty_at = None;
    }

    fn show_persistence_error(&mut self) {
        if let Some(error) = &self.persistence_error {
            self.view.status = format!("Error de guardado: {error}");
        } else if let Some(error) = &self.draft_error {
            self.view.status = format!("Error al guardar borrador: {error}");
        }
    }

    fn clear_draft_key(&mut self, key: &(String, Option<String>)) {
        self.drafts.remove(key);
        self.attachment_drafts.remove(key);
        if let Some(store) = &self.store {
            match store.clear_draft(Path::new(&key.0), key.1.as_deref()) {
                Ok(_) => self.draft_error = None,
                Err(error) => self.draft_error = Some(error.to_string()),
            }
        }
    }

    fn answer_permission(&mut self, request_id: &str, allow: bool) {
        if !self
            .view
            .pending_permissions
            .iter()
            .any(|permission| permission.request_id == request_id)
        {
            self.view.status = "La solicitud de permiso ya no está activa".to_owned();
            return;
        }
        let Some(session_id) = self.view.selected_session.as_deref() else {
            self.view.status = "No hay sesión activa para registrar el permiso".to_owned();
            return;
        };
        let decision = if allow {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Deny
        };
        let recorded = self
            .store
            .as_ref()
            .map(|store| store.record_permission_decision(session_id, request_id, decision));
        match recorded {
            Some(Ok(_)) => {
                let choice = if allow {
                    PermissionChoice::Allow
                } else {
                    PermissionChoice::Deny
                };
                if self
                    .active_run
                    .as_ref()
                    .is_some_and(|run| run.resolve_permission(request_id, choice))
                {
                    self.view
                        .pending_permissions
                        .retain(|permission| permission.request_id != request_id);
                    self.view.status = if allow {
                        "Permiso concedido para esta llamada".to_owned()
                    } else {
                        "Permiso denegado para esta llamada".to_owned()
                    };
                } else {
                    self.view.status = "La solicitud de permiso ya no está activa".to_owned();
                }
            }
            Some(Err(error)) => {
                if let Some(run) = &self.active_run {
                    run.resolve_permission(request_id, PermissionChoice::Deny);
                }
                self.view
                    .pending_permissions
                    .retain(|permission| permission.request_id != request_id);
                self.persistence_error = Some(error.to_string());
                self.show_persistence_error();
            }
            None => {
                if let Some(run) = &self.active_run {
                    run.resolve_permission(request_id, PermissionChoice::Deny);
                }
                self.view
                    .pending_permissions
                    .retain(|permission| permission.request_id != request_id);
                self.view.status = "El historial no está disponible".to_owned();
            }
        }
    }

    fn dock_scope(&self) -> DockScope {
        DockScope::for_owner(
            Path::new(&self.view.workspace),
            self.view.selected_session.as_deref().unwrap_or(""),
        )
    }

    fn active_terminal_id(&self) -> Option<u64> {
        let scope = self.dock_scope();
        self.dock
            .active_tab(&scope)
            .filter(|tab| matches!(tab.kind, DockTabKind::Terminal))
            .map(|tab| tab.id.get())
    }

    fn terminal_dimensions(&self) -> (u16, u16) {
        let width = self.view.right_dock_width.unwrap_or(420.0);
        let columns = ((width - 42.0).max(40.0) / 7.3).floor() as u16;
        let rows = ((self.viewport_height - 182.0).max(128.0) / 16.0).floor() as u16;
        (columns.clamp(12, 240), rows.clamp(8, 80))
    }

    fn ensure_terminal_session(&mut self, id: u64) -> bool {
        if self.terminals.contains_key(&id) {
            return false;
        }
        // A failed launch should not spawn a process again on every repaint.
        // Reopening the tab or pressing Retry clears this error and tries again.
        if self.view.terminal_error.is_some() {
            return false;
        }
        let (columns, rows) = self.terminal_dimensions();
        let launch = match self.settings_preferences.terminal_command() {
            Ok(None) => TerminalSession::spawn(Path::new(&self.view.workspace), columns, rows),
            Ok(Some((program, args))) => TerminalSession::spawn_with_command(
                Path::new(&self.view.workspace),
                columns,
                rows,
                &program,
                &args,
            ),
            Err(error) => Err(std::io::Error::other(error)),
        };
        match launch {
            Ok(session) => {
                self.view.terminal_snapshot = Some(session.snapshot());
                self.view.terminal_error = None;
                self.terminals.insert(id, session);
            }
            Err(error) => {
                self.view.terminal_snapshot = None;
                self.view.terminal_error = Some(format!("No se pudo iniciar la terminal: {error}"));
            }
        }
        true
    }

    fn update_terminal(&mut self) -> bool {
        let active = self.active_terminal_id();
        for (id, session) in &mut self.terminals {
            if Some(*id) != active {
                session.poll();
            }
        }
        let Some(id) = active else {
            return false;
        };
        let mut changed = self.ensure_terminal_session(id);
        let (columns, rows) = self.terminal_dimensions();
        if let Some(session) = self.terminals.get_mut(&id) {
            match session.resize(columns, rows) {
                Ok(resized) => changed |= resized,
                Err(error) => {
                    self.view.terminal_error =
                        Some(format!("No se pudo ajustar la terminal: {error}"));
                    changed = true;
                }
            }
            changed |= session.poll();
            if changed {
                self.view.terminal_snapshot = Some(session.snapshot());
            }
        }
        changed
    }

    fn send_terminal_command(&mut self, command: &str) {
        let Some(id) = self.active_terminal_id() else {
            return;
        };
        let command = command.trim_end_matches(['\r', '\n']);
        if let Some(session) = self.terminals.get_mut(&id) {
            match session.send_command(command) {
                Ok(()) => {
                    self.view.terminal_input.clear();
                    self.tree.set_input_value("terminal_input", "");
                    self.view.terminal_error = None;
                }
                Err(error) => {
                    self.view.terminal_error = Some(format!("No se pudo escribir: {error}"));
                }
            }
        }
    }

    fn terminal_write(&mut self, bytes: &[u8]) {
        let Some(id) = self.active_terminal_id() else {
            return;
        };
        if let Some(session) = self.terminals.get_mut(&id) {
            if let Err(error) = session.write(bytes) {
                self.view.terminal_error = Some(format!("No se pudo escribir: {error}"));
            }
        }
    }

    fn terminal_scroll(&mut self, rows: i32) {
        let Some(id) = self.active_terminal_id() else {
            return;
        };
        if let Some(session) = self.terminals.get_mut(&id) {
            if session.scroll(rows) {
                self.view.terminal_snapshot = Some(session.snapshot());
            }
        }
    }

    fn handle_terminal_event(&mut self, event: &UiEvent) -> bool {
        if self.active_terminal_id().is_none() || !self.view.show_side_pane {
            return false;
        }
        if let UiEvent::Scroll { position, delta_y } = event {
            if self.tree.frame().hit_test(*position) == Some("terminal_screen") {
                let rows = (*delta_y / 22.0).round() as i32;
                self.terminal_scroll(rows.clamp(-24, 24));
                return true;
            }
        }
        if self.tree.focused_id() != Some("terminal_screen") {
            return false;
        }
        match event {
            UiEvent::Text(value) => {
                self.terminal_write(value.as_bytes());
                true
            }
            UiEvent::Key { key, shift, ctrl } => {
                if let Some(bytes) = terminal_key_bytes(*key, *shift, *ctrl) {
                    self.terminal_write(bytes);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn nominal_dock_layout(&mut self) -> DockLayout {
        let left = self.view.left_presentation_width.take();
        let right = self.view.right_presentation_width.take();
        let layout = dock_layout(&self.view, self.viewport_width);
        self.view.left_presentation_width = left;
        self.view.right_presentation_width = right;
        layout
    }

    fn ensure_dock_tabs(&mut self) {
        self.view.show_dock_add_menu = false;
        let scope = self.dock_scope();
        if self
            .dock_seeded_workspaces
            .insert(scope.workspace_id.clone())
        {
            let collapsed = self.dock.is_collapsed(&scope);
            let workspace_scope = DockScope::new(scope.workspace_id.clone(), DockOwner::Workspace);
            let context = self.dock.open(&workspace_scope, DockTabKind::Context);
            self.dock.open(&workspace_scope, DockTabKind::Files);
            self.dock.open(&scope, DockTabKind::Activity);
            self.dock.open(&workspace_scope, DockTabKind::CodeReview);
            self.dock.activate(&scope, context);
            self.dock.set_collapsed(&scope, collapsed);
        }
        self.sync_dock_view();
    }

    fn sync_dock_view(&mut self) {
        let scope = self.dock_scope();
        let live_terminals: HashSet<u64> = self
            .dock
            .visible_tabs(&scope)
            .into_iter()
            .filter(|tab| matches!(tab.kind, DockTabKind::Terminal))
            .map(|tab| tab.id.get())
            .collect();
        self.terminals.retain(|id, _| live_terminals.contains(id));
        let active = self.dock.active_tab(&scope).cloned();
        let previous_terminal = self
            .view
            .dock_tabs
            .iter()
            .find(|tab| tab.active && matches!(tab.kind, SideTab::Terminal))
            .map(|tab| tab.id);
        let next_terminal = active
            .as_ref()
            .filter(|tab| matches!(tab.kind, DockTabKind::Terminal))
            .map(|tab| tab.id.get());
        if previous_terminal != next_terminal {
            self.view.terminal_input.clear();
            self.tree.set_input_value("terminal_input", "");
        }
        let active_review_workspace = active.as_ref().and_then(|tab| {
            matches!(tab.kind, DockTabKind::CodeReview).then(|| PathBuf::from(&scope.workspace_id))
        });
        let review_changed = active_review_workspace != self.review_active_workspace;
        self.view.dock_tabs = self
            .dock
            .visible_tabs(&scope)
            .into_iter()
            .map(|tab| DockTabView {
                id: tab.id.get(),
                title: tab.title(),
                active: active
                    .as_ref()
                    .is_some_and(|selected| selected.id == tab.id),
                kind: match &tab.kind {
                    DockTabKind::Context => SideTab::Context,
                    DockTabKind::Files => SideTab::Files,
                    DockTabKind::Activity => SideTab::Activity,
                    DockTabKind::CodeReview => SideTab::CodeReview,
                    DockTabKind::Terminal => SideTab::Terminal,
                    DockTabKind::FilePreview { .. } => SideTab::FilePreview,
                },
                closable: matches!(
                    tab.kind,
                    DockTabKind::FilePreview { .. } | DockTabKind::Terminal
                ),
            })
            .collect();
        self.view.show_side_pane = !self.dock.is_collapsed(&scope);
        let available = self.viewport_width - self.nominal_dock_layout().left - 12.0;
        self.view.right_dock_width = Some(self.dock.width_px(&scope.workspace_id, available));
        let previous_preview = self.view.file_preview.take();
        self.view.terminal_snapshot = None;
        self.view.terminal_error = None;
        if active_review_workspace.is_none() {
            self.review_active_workspace = None;
            self.review_in_flight = false;
            self.github_status = GithubStatus::Loading;
            self.github_in_flight = false;
            self.view.git_review = None;
            self.view.review_detail = None;
            if self
                .detail_request
                .as_ref()
                .is_some_and(|request| matches!(request.kind, DetailKind::Diff { .. }))
            {
                self.detail_request = None;
            }
        }
        if let Some(tab) = active {
            self.view.side_tab = match &tab.kind {
                DockTabKind::Context => SideTab::Context,
                DockTabKind::Files => SideTab::Files,
                DockTabKind::Activity => SideTab::Activity,
                DockTabKind::CodeReview => SideTab::CodeReview,
                DockTabKind::Terminal => SideTab::Terminal,
                DockTabKind::FilePreview { path } => {
                    let path_text = path.to_string_lossy().into_owned();
                    self.view.file_preview = match previous_preview {
                        Some(previous)
                            if previous.tab_id == tab.id.get() && previous.path == path_text =>
                        {
                            Some(previous)
                        }
                        _ => Some(FilePreviewView {
                            tab_id: tab.id.get(),
                            path: path_text,
                            page_index: 0,
                            content: None,
                        }),
                    };
                    if self.view.file_preview.as_ref().is_some_and(|preview| preview.content.is_none())
                        && self.detail_request.as_ref().is_none_or(|request| !matches!(&request.kind, DetailKind::Preview { path: requested, page } if requested == path && *page == self.view.file_preview.as_ref().unwrap().page_index))
                    {
                        self.request_file_preview();
                    }
                    SideTab::FilePreview
                }
            };
        }
        if let Some(id) = self.active_terminal_id() {
            self.ensure_terminal_session(id);
            if let Some(session) = self.terminals.get(&id) {
                self.view.terminal_snapshot = Some(session.snapshot());
            }
        }
        if review_changed {
            if let Some(workspace) = active_review_workspace {
                self.request_git_review(workspace);
            }
        }
    }

    fn queue_detail(&mut self, kind: DetailKind) {
        self.detail_generation = self.detail_generation.wrapping_add(1);
        let request = DetailRequest {
            workspace: PathBuf::from(&self.view.workspace),
            generation: self.detail_generation,
            kind,
        };
        if self
            .detail_worker
            .as_ref()
            .is_some_and(|worker| worker.submit(request.clone()).is_ok())
        {
            self.detail_request = Some(request);
        } else {
            self.detail_request = None;
            match request.kind {
                DetailKind::Preview { .. } => {
                    if let Some(preview) = &mut self.view.file_preview {
                        preview.content =
                            Some(Err("No se pudo iniciar la lectura del archivo".to_owned()));
                    }
                }
                DetailKind::Diff { .. } => {
                    if let Some(detail) = &mut self.view.review_detail {
                        detail.content =
                            Some(Err("No se pudo iniciar la lectura del diff".to_owned()));
                    }
                }
            }
        }
    }

    fn request_file_preview(&mut self) {
        let Some(preview) = &mut self.view.file_preview else {
            return;
        };
        preview.content = None;
        let path = PathBuf::from(&preview.path);
        let page = preview.page_index;
        self.queue_detail(DetailKind::Preview { path, page });
    }

    fn request_review_detail(&mut self) {
        let Some(detail) = &mut self.view.review_detail else {
            return;
        };
        detail.content = None;
        let path = PathBuf::from(&detail.path);
        let stage = detail.stage;
        self.queue_detail(DetailKind::Diff { path, stage });
    }

    fn update_detail(&mut self, ctx: &egui::Context) -> bool {
        let Some(request) = self.detail_request.as_ref() else {
            return false;
        };
        let Some(response) = self
            .detail_worker
            .as_ref()
            .and_then(|worker| worker.poll_latest(request))
        else {
            ctx.request_repaint_after(Duration::from_millis(80));
            return false;
        };
        self.detail_request = None;
        match response.result {
            DetailResult::Preview(result) => {
                if let Some(preview) = &mut self.view.file_preview {
                    preview.content = Some(
                        result.map_err(|error| format!("No se pudo mostrar el archivo: {error}")),
                    );
                }
            }
            DetailResult::Diff(result) => {
                if let Some(detail) = &mut self.view.review_detail {
                    detail.content = Some(
                        result.map_err(|error| format!("No se pudo mostrar el diff: {error}")),
                    );
                }
            }
        }
        true
    }

    fn open_files_dock(&mut self) {
        self.view.show_dock_add_menu = false;
        self.refresh_file_rows();
        let scope = self.dock_scope();
        let workspace_scope = DockScope::new(scope.workspace_id.clone(), DockOwner::Workspace);
        let tab = self.dock.open(&workspace_scope, DockTabKind::Files);
        self.dock.activate(&scope, tab);
        self.dock.set_collapsed(&scope, false);
        self.view.show_dock_overview = false;
        self.sync_dock_view();
        if !self.view.sidebar_collapsed && self.nominal_dock_layout().right.is_none() {
            self.view.sidebar_collapsed = true;
            if self.nominal_dock_layout().right.is_some() {
                self.sync_dock_view();
            } else {
                self.view.sidebar_collapsed = false;
                self.view.status =
                    "Amplía la ventana para ver el explorador de archivos".to_owned();
            }
        }
    }

    fn request_git_review(&mut self, workspace: PathBuf) {
        self.view.review_detail = None;
        self.review_generation = self.review_generation.wrapping_add(1);
        self.review_active_workspace = Some(workspace.clone());
        self.view.git_review = None;
        self.view.review_page = 0;
        self.github_status = GithubStatus::Loading;
        self.github_in_flight = self.github_worker.as_ref().is_some_and(|worker| {
            worker
                .submit(GithubRequest {
                    workspace: workspace.clone(),
                    generation: self.review_generation,
                })
                .is_ok()
        });
        if !self.github_in_flight {
            self.github_status =
                GithubStatus::Unavailable("No se pudo iniciar la consulta de GitHub".to_owned());
        }
        self.review_in_flight = match &self.review_worker {
            Some(worker) => worker
                .submit(ReviewRequest {
                    workspace,
                    generation: self.review_generation,
                })
                .is_ok(),
            None => false,
        };
        if !self.review_in_flight {
            self.view.git_review = Some(Err("No se pudo iniciar la revisión de Git".to_owned()));
        }
    }

    fn update_git_review(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        if let Some(worker) = &self.github_worker {
            let workspace = self
                .review_active_workspace
                .as_deref()
                .unwrap_or_else(|| Path::new(""));
            if let Some(response) = worker.poll_latest(self.review_generation, workspace) {
                self.github_in_flight = false;
                self.github_status = response
                    .overview
                    .map(GithubStatus::Ready)
                    .unwrap_or(GithubStatus::NoRemote);
                if let Some(Ok(review)) = &mut self.view.git_review {
                    review.github = self.github_status.clone();
                    changed = true;
                }
            }
        }
        if let Some(worker) = &self.review_worker {
            let workspace = self
                .review_active_workspace
                .as_deref()
                .unwrap_or_else(|| Path::new(""));
            if let Some(response) = worker.poll_latest(self.review_generation, workspace) {
                if self.review_active_workspace.is_some()
                    && self.view.side_tab == SideTab::CodeReview
                {
                    changed = true;
                    self.view.git_review = Some(response.result.map(|mut review| {
                        review.github = self.github_status.clone();
                        review
                    }));
                    if let Some(Ok(review)) = &self.view.git_review {
                        self.view.project_branch = Some(review.branch.clone());
                    }
                    self.review_in_flight = false;
                }
            }
        }
        if self.review_in_flight || self.github_in_flight {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        changed
    }

    fn update_github_account(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        if let Some(worker) = &self.github_account_worker {
            if let Some((initial_probe, status)) = worker.poll_latest() {
                // A startup probe may finish after the user has already
                // started browser sign-in or explicit verification. It must
                // not replace that in-flight state with a cached-account list.
                if !(initial_probe
                    && matches!(
                        self.view.github_account,
                        GithubAccountStatus::Connecting | GithubAccountStatus::Verifying(_)
                    ))
                {
                    let connected = matches!(&status, GithubAccountStatus::Connected(_));
                    changed = self.view.github_account != status
                        || self.view.github_account_menu_open
                        || (connected && self.review_active_workspace.is_some());
                    self.view.github_account = status;
                    self.view.github_account_menu_open = false;
                    if connected {
                        if let Some(workspace) = self.review_active_workspace.clone() {
                            self.request_git_review(workspace);
                        }
                    }
                }
            }
            if matches!(
                self.view.github_account,
                GithubAccountStatus::Checking
                    | GithubAccountStatus::Connecting
                    | GithubAccountStatus::Verifying(_)
            ) {
                ctx.request_repaint_after(Duration::from_millis(80));
            }
        }
        changed
    }

    fn refresh_file_rows(&mut self) {
        let workspace = Path::new(&self.view.workspace);
        let result = match &mut self.file_explorer {
            Some(explorer) => explorer.set_workspace(workspace).map(|_| ()),
            None => FileExplorer::new(workspace).map(|explorer| {
                self.file_explorer = Some(explorer);
            }),
        };
        if let Err(error) = result {
            self.file_explorer = None;
            self.view.file_rows.clear();
            self.view.file_root_can_load_more = false;
            self.view.files_status = format!("No se pudo abrir el proyecto: {error}");
            return;
        }
        if let Some(explorer) = &self.file_explorer {
            self.view.file_rows = explorer
                .rows()
                .iter()
                .map(|row| FileRowView {
                    name: row.name.clone(),
                    depth: row.depth,
                    is_dir: row.is_dir,
                    expanded: row.expanded,
                    can_load_more: row.is_dir
                        && row.expanded
                        && explorer.directory_can_load_more(&row.relative),
                })
                .collect();
            self.view.file_root_can_load_more = explorer.directory_can_load_more(Path::new(""));
            self.view.files_status = if explorer.rows_truncated() || explorer.root_truncated() {
                "Lista parcial; abre una carpeta más específica".to_owned()
            } else {
                "Este proyecto no tiene archivos visibles".to_owned()
            };
        }
    }

    fn invalidate_workspace_files(&mut self) {
        self.file_explorer = None;
        self.view.file_rows.clear();
        self.view.files_status.clear();
        self.view.file_root_can_load_more = false;
        self.view.review_page = 0;
        if self.view.show_command_center {
            self.request_command_search();
        }
    }

    pub fn new() -> Self {
        Self::with_data_root(data_root())
    }

    fn with_data_root(root: PathBuf) -> Self {
        let settings_preferences = SettingsPreferences::load(&root);
        let (agent_tool_settings, agent_tool_settings_error) = match AgentToolSettings::load(&root)
        {
            Ok(settings) => (settings, None),
            Err(error) => (AgentToolSettings::default(), Some(error)),
        };
        let sidebar_preferences = SidebarPreferences::load(&root);
        let mut composer_preferences = ComposerPreferences::load(&root);
        let native_settings = NativeProviderSettings::load(&root);
        if root.join("native-provider.json").exists() {
            let mut choice = composer_preferences.choice("native", &native_settings.model_id);
            choice.model = native_settings.model_id.clone();
            composer_preferences.set("native", choice);
        }
        let catalog = model_catalog::load_cache(&root).ok().flatten();
        let catalog_should_refresh = catalog
            .as_ref()
            .is_none_or(|snapshot| snapshot.is_stale(Duration::from_secs(3_600)));
        let catalog_status = catalog.as_ref().map_or_else(
            || "Sin copia local; buscando el catálogo de models.dev".to_owned(),
            catalog_status,
        );
        let workspace = settings_preferences
            .startup_workspace()
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let (store, status) = match SessionStore::open(root.join("sessions")) {
            Ok(store) => (Some(store), "Listo".to_owned()),
            Err(error) => (None, format!("No se pudo abrir el historial: {error}")),
        };
        let profiles = load_profiles(&root);
        let selected_backend = profiles
            .iter()
            .find(|profile| profile.id == "codex" && profile.detect_executable().is_some())
            .map(|profile| profile.id.clone())
            .unwrap_or_else(|| "native".to_owned());
        let mut backend_options = vec![("native".to_owned(), "Nativo".to_owned())];
        backend_options.extend(profiles.iter().filter_map(|profile| {
            profile.detect_executable()?;
            let short = match profile.id.as_str() {
                "codex" => "Codex",
                "claude" => "Claude",
                "gemini" => "Gemini",
                "zcode" => "ZCode",
                _ => &profile.label,
            };
            Some((profile.id.clone(), short.to_owned()))
        }));
        let native_model = if root.join("native-provider.json").exists() {
            native_settings.model_id.clone()
        } else {
            composer_preferences
                .choice("native", &native_settings.model_id)
                .model
        };
        let native_choice = composer_preferences.choice("native", &native_model);
        let selected_choice = composer_preferences.choice(&selected_backend, &native_choice.model);
        let selected_model_options =
            composer_selection::model_options(&selected_backend, &selected_choice.model);
        let selected_effort_options =
            composer_selection::effort_options(&selected_backend, &selected_choice.model);
        let selected_effort = if selected_effort_options
            .iter()
            .any(|(effort, _)| effort == &selected_choice.effort)
        {
            selected_choice.effort
        } else {
            selected_effort_options
                .first()
                .map(|(effort, _)| effort.clone())
                .unwrap_or_else(|| "default".to_owned())
        };
        let view = ViewState {
            workspace: workspace.clone(),
            workspace_draft: workspace.clone(),
            project_branch: project_branch_for_workspace(&workspace),
            prompt: String::new(),
            composer_attachments: Vec::new(),
            composer_add_menu_open: false,
            composer_project_menu_open: false,
            composer_project_options: composer_project_options(&workspace, &[]),
            endpoint: native_settings.custom_endpoint.clone(),
            model: native_choice.model,
            native_provider_id: native_settings.provider_id.clone(),
            catalog: catalog.map(Arc::new),
            catalog_status,
            catalog_refreshing: false,
            catalog_browse_provider: if native_settings.provider_id == "custom" {
                "openai".to_owned()
            } else {
                native_settings.provider_id.clone()
            },
            catalog_provider_query: String::new(),
            catalog_model_query: String::new(),
            backend_options: backend_options.clone(),
            selected_backend,
            backend_menu_open: false,
            composer_model: selected_choice.model,
            composer_model_options: selected_model_options,
            composer_model_menu_open: false,
            composer_effort: selected_effort,
            composer_effort_options: selected_effort_options,
            composer_effort_menu_open: false,
            sessions: Vec::new(),
            sessions_status: None,
            selected_session: None,
            selected_session_title: None,
            file_rows: Vec::new(),
            files_status: String::new(),
            file_root_can_load_more: false,
            transcript: Vec::new(),
            running: false,
            background_run_selected: false,
            status,
            settings_notice: agent_tool_settings_error.clone(),
            settings_notice_error: agent_tool_settings_error.is_some(),
            native_key_available: native_settings.key_available(),
            workspace_writes_enabled: agent_tool_settings.permissions().write_file
                && agent_tool_settings.permissions().run_command,
            native_tool_permissions: agent_tool_settings.permissions(),
            settings_reduced_motion: settings_preferences.reduced_motion,
            settings_reopen_last_workspace: settings_preferences.reopen_last_workspace,
            settings_terminal_shell: settings_preferences.terminal_shell.clone(),
            settings_terminal_shell_draft: match &settings_preferences.terminal_shell {
                TerminalShell::Custom(path) => path.to_string_lossy().into_owned(),
                _ => String::new(),
            },
            show_settings: false,
            main_page: MainPage::Workspace,
            automations: AutomationViewState::new(
                workspace.clone(),
                backend_options.clone(),
                chrono::Utc::now().timestamp_millis(),
            ),
            marketplace: MarketplaceViewState::default(),
            settings_page: SettingsPage::from_action_id(&settings_preferences.last_settings_page)
                .unwrap_or(SettingsPage::General),
            settings_nav_open: false,
            can_go_back: false,
            can_go_forward: false,
            show_command_center: false,
            command_query: String::new(),
            command_scope: Scope::All,
            command_results: Vec::new(),
            command_selected_index: 0,
            show_side_pane: false,
            sidebar_collapsed: sidebar_preferences.sidebar_collapsed,
            left_dock_width: None,
            right_dock_width: None,
            left_presentation_width: None,
            right_presentation_width: None,
            side_tab: SideTab::Context,
            activity_page: 0,
            review_page: 0,
            dock_tabs: Vec::new(),
            show_dock_overview: false,
            show_dock_add_menu: false,
            file_preview: None,
            git_review: None,
            review_detail: None,
            terminal_snapshot: None,
            terminal_input: String::new(),
            terminal_error: None,
            pending_permissions: Vec::new(),
            selected_tool_id: None,
            show_jump_to_bottom: false,
            show_archived: false,
            session_organization: sidebar_preferences.organization,
            session_sort: sidebar_preferences.sort,
            session_sort_menu_open: false,
            collapsed_projects: sidebar_preferences.collapsed_projects,
            session_groups: SessionGroups::load(&root),
            group_create_open: false,
            group_create_draft: String::new(),
            group_menu_id: None,
            group_rename_id: None,
            group_rename_draft: String::new(),
            pending_delete_group_id: None,
            session_menu_id: None,
            renaming_session_id: None,
            rename_draft: String::new(),
            pending_archive_session_id: None,
            pending_delete_session_id: None,
            github_account: GithubAccountStatus::Checking,
            github_account_menu_open: false,
        };
        let mut dock = DockState::default();
        let draft_scope = DockScope::for_owner(Path::new(&view.workspace), "");
        let workspace_scope =
            DockScope::new(draft_scope.workspace_id.clone(), DockOwner::Workspace);
        let context_tab = dock.open(&workspace_scope, DockTabKind::Context);
        dock.open(&workspace_scope, DockTabKind::Files);
        dock.open(&draft_scope, DockTabKind::Activity);
        dock.open(&workspace_scope, DockTabKind::CodeReview);
        dock.activate(&draft_scope, context_tab);
        dock.set_collapsed(&draft_scope, true);
        let mut navigation = NavigationHistory::default();
        navigation.visit(NavigationTarget::Draft {
            workspace: PathBuf::from(&view.workspace),
        });
        let reduced_motion = settings_preferences.reduced_motion;
        let mut tree = UiTree::new(Element::spacer());
        tree.set_reduced_motion(reduced_motion);
        let mut command_tree = UiTree::new(Element::spacer());
        command_tree.set_reduced_motion(reduced_motion);
        let seeded_workspace = view.workspace.clone();
        let mut app = Self {
            data_root: root.clone(),
            view,
            composer_preferences,
            settings_preferences,
            agent_tool_settings,
            tree,
            projection: TranscriptProjection::new(),
            profiles,
            store,
            known_project_paths: Vec::new(),
            active_run: None,
            automation_stores: Vec::new(),
            automation_last_poll: Instant::now() - Duration::from_secs(31),
            background_automation_runs: Vec::new(),
            automation_queue: VecDeque::new(),
            marketplace_response: None,
            workspace_folder_response: None,
            catalog_response: None,
            catalog_last_refresh: Instant::now(),
            history: Vec::new(),
            last_sequence: 0,
            dock_drag: None,
            dock_motion: None,
            reduced_motion,
            pending_prompt: None,
            pending_attachments: Vec::new(),
            run_has_output: false,
            viewport_width: 900.0,
            viewport_height: 620.0,
            dock,
            terminals: HashMap::new(),
            dock_seeded_workspaces: HashSet::from([seeded_workspace]),
            file_explorer: None,
            drafts: HashMap::new(),
            attachment_drafts: HashMap::new(),
            draft_dirty_at: None,
            draft_error: None,
            persistence_error: None,
            navigation,
            command_tree,
            command_center_attaches_file: false,
            command_focus_pending: false,
            prompt_focus_pending: false,
            group_focus_pending: false,
            search_worker: SearchWorker::spawn(root.join("sessions")).ok(),
            search_generation: 0,
            search_pending_at: None,
            search_response: None,
            search_in_flight: false,
            review_worker: ReviewWorker::spawn().ok(),
            review_generation: 0,
            review_active_workspace: None,
            review_in_flight: false,
            detail_worker: DetailWorker::spawn().ok(),
            detail_generation: 0,
            detail_request: None,
            github_worker: GithubWorker::spawn().ok(),
            github_status: GithubStatus::Loading,
            github_in_flight: false,
            github_account_worker: GithubAccountWorker::spawn().ok(),
        };
        if app.github_account_worker.is_none() {
            app.view.github_account = GithubAccountStatus::Unavailable(
                "No se pudo consultar la cuenta de GitHub".to_owned(),
            );
        }
        app.sync_dock_view();
        app.load_automation_stores();
        app.refresh_automation_view();
        app.restore_draft();
        app.refresh_sessions();
        if catalog_should_refresh {
            app.begin_catalog_refresh();
        }
        app
    }

    fn load_automation_stores(&mut self) {
        match AutomationStore::open_all(&self.data_root) {
            Ok(stores) => {
                for result in stores {
                    match result {
                        Ok(store) => self.automation_stores.push(store),
                        Err(error) => self.view.automations.error = Some(error.to_string()),
                    }
                }
            }
            Err(error) => self.view.automations.error = Some(error.to_string()),
        }
        self.ensure_current_automation_store();
    }

    fn current_automation_store_index(&self) -> Option<usize> {
        let workspace = Path::new(&self.view.workspace);
        let workspace =
            std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        self.automation_stores
            .iter()
            .position(|store| store.workspace_path() == workspace)
    }

    fn ensure_current_automation_store(&mut self) -> Option<usize> {
        if let Some(index) = self.current_automation_store_index() {
            return Some(index);
        }
        match AutomationStore::open(&self.data_root, Path::new(&self.view.workspace)) {
            Ok(store) => {
                self.automation_stores.push(store);
                Some(self.automation_stores.len() - 1)
            }
            Err(error) => {
                self.view.automations.error = Some(error.to_string());
                None
            }
        }
    }

    fn refresh_automation_view(&mut self) {
        self.view
            .automations
            .workspace
            .clone_from(&self.view.workspace);
        self.view.automations.now_ms = chrono::Utc::now().timestamp_millis();
        self.view
            .automations
            .backends
            .clone_from(&self.view.backend_options);
        if let Some(index) = self.ensure_current_automation_store() {
            self.view.automations.items = self.automation_stores[index].automations().to_vec();
            self.view.automations.runs = self.automation_stores[index].runs().to_vec();
            self.view.automations.busy_id = self
                .background_automation_runs
                .iter()
                .map(|run| &run.automation_id)
                .chain(
                    self.automation_queue
                        .iter()
                        .map(|(_, dispatch)| &dispatch.automation_id),
                )
                .find(|id| {
                    self.view
                        .automations
                        .items
                        .iter()
                        .any(|item| &item.id == *id)
                })
                .cloned();
        } else {
            self.view.automations.items.clear();
            self.view.automations.runs.clear();
        }
    }

    fn marketplace_operation(&mut self, operation: MarketplaceOperation) {
        if self.marketplace_response.is_some() {
            return;
        }
        let workspace = PathBuf::from(&self.view.workspace);
        let store = MarketplaceStore::new(self.data_root.clone(), workspace.clone());
        let (sender, receiver) = mpsc::channel();
        self.view.marketplace.busy = true;
        self.view.marketplace.status = Some("Actualizando Marketplace…".to_owned());
        self.marketplace_response = Some(receiver);
        std::thread::spawn(move || {
            let operation_result = match operation {
                MarketplaceOperation::Load => Ok("Catálogo cargado".to_owned()),
                MarketplaceOperation::Refresh => {
                    store.refresh_github_sources().map(|diagnostics| {
                        if diagnostics.is_empty() {
                            "Catálogo actualizado".to_owned()
                        } else {
                            format!("Actualización parcial: {}", diagnostics.join(" · "))
                        }
                    })
                }
                MarketplaceOperation::AddLocal(path) => store
                    .add_local_source(&path)
                    .map(|_| "Origen local agregado".to_owned()),
                MarketplaceOperation::AddGithub(url) => store
                    .add_github_source(&url)
                    .map(|_| "Repositorio agregado".to_owned()),
                MarketplaceOperation::Install(id, scope) => store
                    .install(&id, scope)
                    .map(|_| "Skill instalada".to_owned()),
                MarketplaceOperation::SetEnabled(id, enabled) => {
                    store.set_enabled(&id, enabled).map(|_| {
                        if enabled {
                            "Skill activada"
                        } else {
                            "Skill desactivada"
                        }
                        .to_owned()
                    })
                }
                MarketplaceOperation::Uninstall(id) => store
                    .uninstall(&id)
                    .map(|_| "Skill desinstalada".to_owned()),
                MarketplaceOperation::RemoveSource(id) => store
                    .remove_source(&id)
                    .map(|_| "Origen eliminado".to_owned()),
            }
            .and_then(|message| store.snapshot().map(|snapshot| (snapshot, message)))
            .map_err(|error| error.to_string());
            let _ = sender.send((workspace, operation_result));
        });
    }

    fn poll_marketplace(&mut self) -> bool {
        let Some(receiver) = &self.marketplace_response else {
            return false;
        };
        let response = match receiver.try_recv() {
            Ok(response) => Some(response),
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                self.view.marketplace.busy = false;
                self.view.marketplace.status = Some("No se pudo completar la operación".to_owned());
                self.marketplace_response = None;
                return true;
            }
        };
        self.marketplace_response = None;
        if let Some((workspace, result)) = response {
            self.view.marketplace.busy = false;
            if workspace == Path::new(&self.view.workspace) {
                match result {
                    Ok((snapshot, message)) => {
                        self.view.marketplace.snapshot = snapshot;
                        self.view.marketplace.status = Some(message);
                        self.view.marketplace.add_source_kind = None;
                        self.view.marketplace.source_draft.clear();
                        self.view.marketplace.add_menu_open = false;
                    }
                    Err(error) => self.view.marketplace.status = Some(error),
                }
            } else if self.view.main_page == MainPage::Marketplace {
                self.marketplace_operation(MarketplaceOperation::Load);
            }
        }
        true
    }

    fn dispatch_automation(&mut self, store_index: usize, dispatch: AutomationDispatch) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let automation_id = dispatch.automation_id.clone();
        let result = (|| -> Result<String, String> {
            if !dispatch.workspace_path.is_dir() {
                return Err("El proyecto ya no está disponible".to_owned());
            }
            let native_settings = if dispatch.backend == "native" {
                Some(
                    dispatch
                        .native_provider
                        .clone()
                        .unwrap_or_else(|| self.native_settings()),
                )
            } else {
                None
            };
            let native_connection = native_settings
                .as_ref()
                .map(NativeProviderSettings::resolve)
                .transpose()?;
            let native_provider_id = native_settings
                .as_ref()
                .map(|settings| settings.provider_id.as_str())
                .unwrap_or("");
            let skills = if dispatch.backend == "native" {
                if native_connection
                    .as_ref()
                    .is_some_and(|connection| connection.api_key.is_none())
                    && native_provider_id != "custom"
                {
                    let key_env = native_provider::adapter(native_provider_id)
                        .map(|adapter| adapter.key_env)
                        .unwrap_or("API_KEY");
                    return Err(format!("Define {key_env} para ejecutar el agente nativo"));
                }
                crate::marketplace::active_skill_instructions(
                    &self.data_root,
                    &dispatch.workspace_path,
                )
                .map_err(|error| format!("No se pudieron leer las skills: {error}"))?
            } else {
                let profile = self
                    .profiles
                    .iter()
                    .find(|profile| profile.id == dispatch.backend)
                    .ok_or("Perfil de agente desconocido")?;
                if profile.detect_executable().is_none() {
                    return Err(format!("Instala {} o configura su ruta", profile.label));
                }
                String::new()
            };
            let store = self
                .store
                .as_ref()
                .ok_or("El historial no está disponible")?;
            let session = store
                .create(&dispatch.workspace_path, &dispatch.backend)
                .map_err(|error| error.to_string())?;
            let request = AgentRunRequest {
                session_id: Some(session.id.clone()),
                sequence_start: 0,
                workspace: dispatch.workspace_path.clone(),
                history: Vec::new(),
                prompt: dispatch.prompt,
            };
            let run = if dispatch.backend == "native" {
                let connection = native_connection.expect("validated native connection");
                let mut model = OpenAiCompatibleBackend::new(
                    connection.endpoint,
                    connection.api_key,
                    connection.model_id,
                );
                if native_provider_id == "deepseek" {
                    model = model.with_deepseek_non_thinking();
                }
                let model = Arc::new(model);
                let tools = Arc::new(LocalTools::new().with_write_access(false));
                ActiveRun::Native(
                    AgentRunner::new(model, tools)
                        .with_permission_policy(PermissionPolicy::new())
                        .with_system_instructions(skills)
                        .start(request),
                )
            } else {
                let profile = self
                    .profiles
                    .iter()
                    .find(|profile| profile.id == dispatch.backend)
                    .cloned()
                    .ok_or("Perfil de agente desconocido")?;
                if profile.detect_executable().is_none() {
                    return Err(format!("Instala {} o configura su ruta", profile.label));
                }
                ActiveRun::Cli(CliRunner::start(profile, request))
            };
            self.background_automation_runs
                .push(BackgroundAutomationRun {
                    automation_id,
                    session_id: session.id.clone(),
                    run,
                });
            Ok(session.id)
        })();
        if let Err(error) =
            self.automation_stores[store_index].complete_run(&dispatch.run_id, result, now_ms)
        {
            self.view.automations.error = Some(error.to_string());
        }
        self.refresh_automation_view();
        self.refresh_sessions();
    }

    fn poll_automations(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        if now.duration_since(self.automation_last_poll) >= Duration::from_secs(30) {
            self.automation_last_poll = now;
            let now_ms = chrono::Utc::now().timestamp_millis();
            let mut dispatches = Vec::new();
            for (index, store) in self.automation_stores.iter_mut().enumerate() {
                match store.claim_due(now_ms) {
                    Ok(claims) => dispatches.extend(claims.into_iter().map(|claim| (index, claim))),
                    Err(error) => self.view.automations.error = Some(error.to_string()),
                }
            }
            changed = !dispatches.is_empty();
            self.automation_queue.extend(dispatches);
            if self.view.main_page == MainPage::Automations {
                self.refresh_automation_view();
                changed = true;
            }
        }
        let mut index = 0;
        while index < self.background_automation_runs.len() {
            let (events, disconnected) = self.background_automation_runs[index].run.drain();
            if !events.is_empty() {
                changed = true;
            }
            for event in events {
                if let AgentEventKind::PermissionRequested { request_id, .. } = &event.kind {
                    self.background_automation_runs[index]
                        .run
                        .resolve_permission(request_id, PermissionChoice::Deny);
                }
                if let Some(store) = &self.store {
                    if let Err(error) = store.append_event(&event.session_id, &event) {
                        self.view.automations.error = Some(error.to_string());
                        self.background_automation_runs[index].run.cancel();
                    }
                }
                if self.view.selected_session.as_deref() == Some(event.session_id.as_str()) {
                    self.last_sequence = self.last_sequence.max(event.sequence);
                    if self.projection.apply_event(&event) {
                        self.view.transcript = self.projection.items().to_vec();
                    }
                }
            }
            if disconnected {
                self.background_automation_runs.remove(index);
                self.rebuild_history();
                self.refresh_sessions();
                changed = true;
            } else {
                index += 1;
            }
        }
        while self.background_automation_runs.len() < 2 {
            let Some((store_index, dispatch)) = self.automation_queue.pop_front() else {
                break;
            };
            self.dispatch_automation(store_index, dispatch);
            changed = true;
        }
        changed |= self.sync_background_selected();
        if changed && self.view.main_page == MainPage::Automations {
            self.refresh_automation_view();
        }
        changed
    }

    fn sync_background_selected(&mut self) -> bool {
        let selected = self.view.selected_session.as_deref();
        let running = selected.is_some_and(|selected| {
            self.background_automation_runs
                .iter()
                .any(|run| run.session_id == selected)
        });
        let changed = self.view.background_run_selected != running;
        self.view.background_run_selected = running;
        if changed && running {
            self.view.status = "Automatización en curso".to_owned();
        } else if changed && !running && self.view.status == "Automatización en curso" {
            self.view.status = "Automatización finalizada".to_owned();
        }
        changed
    }

    fn handle_automation_action(&mut self, id: &str) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let current_native = self.native_settings();
        let state = &mut self.view.automations;
        if let Some(filter) = AutomationFilter::from_action_id(id) {
            state.filter = filter;
            return;
        }
        if let Some(frequency) = AutomationFrequency::from_action_id(id) {
            state.form.frequency = frequency;
            return;
        }
        if let Some(day) = id
            .strip_prefix("automation_weekday:")
            .and_then(|day| day.parse::<u8>().ok())
        {
            if day < 7 {
                state.form.weekday = day;
            }
            return;
        }
        if let Some(backend) = id.strip_prefix("automation_backend:") {
            if state.backends.iter().any(|item| item.0 == backend) {
                state.form.backend = backend.to_owned();
                if backend == "native" {
                    state.form.native_provider.get_or_insert(current_native);
                }
            }
            return;
        }
        if id == "automation_native_current" {
            state.form.native_provider = Some(current_native);
            return;
        }
        match id {
            "automation_new" => {
                state.form =
                    crate::automation_view::AutomationForm::new(&self.view.selected_backend);
                if state.form.backend == "native" {
                    state.form.native_provider = Some(current_native);
                }
                state.selected_id = None;
                state.screen = AutomationScreen::Edit;
                state.error = None;
                return;
            }
            "automation_back" | "automation_cancel" => {
                state.screen = AutomationScreen::List;
                state.selected_id = None;
                state.error = None;
                state.pending_delete_id = None;
                return;
            }
            "automation_delete_cancel" => {
                state.pending_delete_id = None;
                return;
            }
            "automation_save" => {
                if state.form.backend == "native" {
                    state.form.native_provider.get_or_insert(current_native);
                }
                let draft = match state.form.to_draft(now_ms) {
                    Ok(draft) => draft,
                    Err(error) => {
                        state.error = Some(error.to_string());
                        return;
                    }
                };
                let selected_id = state.selected_id.clone();
                let Some(index) = self.ensure_current_automation_store() else {
                    return;
                };
                let result = if let Some(id) = selected_id {
                    self.automation_stores[index].update(&id, draft, now_ms)
                } else {
                    self.automation_stores[index]
                        .create(draft, now_ms)
                        .map(|_| ())
                };
                match result {
                    Ok(()) => {
                        self.view.automations.screen = AutomationScreen::List;
                        self.view.automations.selected_id = None;
                        self.view.automations.error = None;
                        self.view.automations.notice = Some("Automatización guardada".to_owned());
                        self.refresh_automation_view();
                    }
                    Err(error) => self.view.automations.error = Some(error.to_string()),
                }
                return;
            }
            _ => {}
        }
        if let Some(template) = id.strip_prefix("automation_template:") {
            if let Some(form) = template_form(template, &self.view.selected_backend) {
                state.form = form;
                if state.form.backend == "native" {
                    state.form.native_provider = Some(current_native);
                }
                state.selected_id = None;
                state.screen = AutomationScreen::Edit;
                state.error = None;
            }
        } else if let Some(selected_id) = id.strip_prefix("automation_edit:") {
            if let Some(item) = state.items.iter().find(|item| item.id == selected_id) {
                state.form = crate::automation_view::AutomationForm::from_automation(item);
                state.selected_id = Some(selected_id.to_owned());
                state.screen = AutomationScreen::Edit;
                state.error = None;
            }
        } else if let Some(selected_id) = id.strip_prefix("automation_history:") {
            if state.items.iter().any(|item| item.id == selected_id) {
                state.selected_id = Some(selected_id.to_owned());
                state.screen = AutomationScreen::History;
            }
        } else if let Some(selected_id) = id.strip_prefix("automation_delete_request:") {
            state.pending_delete_id = Some(selected_id.to_owned());
        } else if let Some(selected_id) = id.strip_prefix("automation_delete_confirm:") {
            if state.pending_delete_id.as_deref() != Some(selected_id) {
                return;
            }
            let Some(index) = self.ensure_current_automation_store() else {
                return;
            };
            match self.automation_stores[index].delete(selected_id) {
                Ok(()) => {
                    self.view.automations.pending_delete_id = None;
                    self.view.automations.notice = Some("Automatización eliminada".to_owned());
                    self.refresh_automation_view();
                }
                Err(error) => self.view.automations.error = Some(error.to_string()),
            }
        } else if let Some(selected_id) = id.strip_prefix("automation_toggle:") {
            let enabled = state
                .items
                .iter()
                .find(|item| item.id == selected_id)
                .map(|item| !item.enabled);
            let Some(enabled) = enabled else { return };
            let Some(index) = self.ensure_current_automation_store() else {
                return;
            };
            match self.automation_stores[index].set_enabled(selected_id, enabled, now_ms) {
                Ok(()) => self.refresh_automation_view(),
                Err(error) => self.view.automations.error = Some(error.to_string()),
            }
        } else if let Some(selected_id) = id.strip_prefix("automation_run:") {
            if self
                .background_automation_runs
                .iter()
                .any(|run| run.automation_id == selected_id)
                || self
                    .automation_queue
                    .iter()
                    .any(|(_, dispatch)| dispatch.automation_id == selected_id)
            {
                self.view.automations.notice =
                    Some("Esta automatización ya se está ejecutando".to_owned());
                return;
            }
            let Some(index) = self.ensure_current_automation_store() else {
                return;
            };
            match self.automation_stores[index].claim_manual(selected_id, now_ms) {
                Ok(dispatch) => {
                    self.automation_queue.push_back((index, dispatch));
                    self.refresh_automation_view();
                }
                Err(error) => self.view.automations.error = Some(error.to_string()),
            }
        } else if let Some(session_id) = id.strip_prefix("automation_open_session:") {
            self.open_session(session_id);
            if self.view.selected_session.as_deref() == Some(session_id) {
                self.view.main_page = MainPage::Workspace;
                self.view.show_settings = false;
                self.visit_navigation();
            }
        }
    }

    fn handle_marketplace_action(&mut self, id: &str) {
        let state = &mut self.view.marketplace;
        match id {
            "marketplace_filter_all" => state.installed_only = false,
            "marketplace_filter_installed" => state.installed_only = true,
            "marketplace_scope_user" => state.install_scope = InstallScope::User,
            "marketplace_scope_workspace" => state.install_scope = InstallScope::Workspace,
            "marketplace_add_menu" | "marketplace_empty_add_menu" => {
                state.add_menu_open = !state.add_menu_open;
            }
            "marketplace_source_kind:local" => {
                state.add_source_kind = Some(SourceKind::Local);
                state.add_menu_open = false;
                state.source_draft.clear();
            }
            "marketplace_source_kind:github" => {
                state.add_source_kind = Some(SourceKind::Github);
                state.add_menu_open = false;
                state.source_draft.clear();
            }
            "marketplace_source_cancel" => {
                state.add_source_kind = None;
                state.source_draft.clear();
            }
            "marketplace_source_submit" => {
                let value = state.source_draft.trim().to_owned();
                if value.is_empty() {
                    state.status = Some("Indica la carpeta o URL del origen".to_owned());
                    return;
                }
                let operation = match state.add_source_kind {
                    Some(SourceKind::Local) => MarketplaceOperation::AddLocal(PathBuf::from(value)),
                    Some(SourceKind::Github) => MarketplaceOperation::AddGithub(value),
                    None => return,
                };
                self.marketplace_operation(operation);
            }
            "marketplace_refresh" => self.marketplace_operation(MarketplaceOperation::Refresh),
            _ => {
                if let Some(item) = id.strip_prefix("marketplace_install:") {
                    let scope = state.install_scope;
                    self.marketplace_operation(MarketplaceOperation::Install(
                        item.to_owned(),
                        scope,
                    ));
                } else if let Some(item) = id.strip_prefix("marketplace_toggle:") {
                    if let Some(enabled) = state
                        .snapshot
                        .items
                        .iter()
                        .find(|entry| entry.id == item)
                        .map(|entry| !entry.enabled)
                    {
                        self.marketplace_operation(MarketplaceOperation::SetEnabled(
                            item.to_owned(),
                            enabled,
                        ));
                    }
                } else if let Some(item) = id.strip_prefix("marketplace_uninstall:") {
                    self.marketplace_operation(MarketplaceOperation::Uninstall(item.to_owned()));
                } else if let Some(source) = id.strip_prefix("marketplace_remove_source:") {
                    self.marketplace_operation(MarketplaceOperation::RemoveSource(
                        source.to_owned(),
                    ));
                }
            }
        }
    }

    fn refresh_sessions(&mut self) {
        if let Some(store) = &self.store {
            match store.list_with_diagnostics() {
                Ok(listing) => {
                    self.view.sessions_status = (!listing.diagnostics.is_empty()).then(|| {
                        format!("{} sesiones no se pudieron leer", listing.diagnostics.len())
                    });
                    if listing.diagnostics.is_empty() {
                        let previous = self.view.session_groups.clone();
                        if self.view.session_groups.prune(
                            listing
                                .sessions
                                .iter()
                                .map(|record| record.session.id.as_str()),
                        ) {
                            self.commit_groups(previous);
                        }
                    }
                    let mut projects: Vec<(u64, String)> = listing
                        .sessions
                        .iter()
                        .map(|record| {
                            let updated_at_ms = record
                                .events
                                .last()
                                .map(|event| event.timestamp_ms.max(record.session.created_at_ms))
                                .unwrap_or(record.session.created_at_ms);
                            (
                                updated_at_ms,
                                record.session.workspace.to_string_lossy().into_owned(),
                            )
                        })
                        .collect();
                    projects.sort_by(|left, right| {
                        right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1))
                    });
                    self.known_project_paths = projects.into_iter().map(|(_, path)| path).collect();
                    let mut sessions: Vec<(u64, u64, SessionView)> = listing
                        .sessions
                        .into_iter()
                        .filter(|record| {
                            if self.view.show_archived {
                                record.session.archived || record.session.pinned
                            } else {
                                !record.session.archived
                            }
                        })
                        .map(|record| {
                            let activity_at_ms = record
                                .events
                                .last()
                                .map(|event| event.timestamp_ms.max(record.session.created_at_ms))
                                .unwrap_or(record.session.created_at_ms);
                            let title = record.events.iter().find_map(|event| {
                                if let AgentEventKind::TurnStarted { prompt } = &event.kind {
                                    Some(prompt.lines().next().unwrap_or(prompt).trim().to_owned())
                                } else {
                                    None
                                }
                            });
                            let session = record.session;
                            (
                                activity_at_ms,
                                session.created_at_ms,
                                SessionView {
                                    id: session.id,
                                    title: session
                                        .title
                                        .or_else(|| title.filter(|text| !text.is_empty()))
                                        .unwrap_or_else(|| "Nueva tarea".to_owned()),
                                    project: session.workspace.to_string_lossy().into_owned(),
                                    backend: session.backend,
                                    pinned: session.pinned,
                                    archived: session.archived,
                                    unread: session.unread_at_ms.is_some(),
                                },
                            )
                        })
                        .collect();
                    sort_session_views(
                        &mut sessions,
                        self.view.session_organization,
                        self.view.session_sort,
                        self.view.show_archived,
                    );
                    self.view.sessions = sessions
                        .into_iter()
                        .map(|(_, _, session)| session)
                        .collect();
                    if let Some(selected) = self.view.selected_session.as_ref() {
                        if let Some(session) =
                            self.view.sessions.iter().find(|item| &item.id == selected)
                        {
                            self.view.selected_session_title = Some(session.title.clone());
                        }
                    }
                }
                Err(error) => {
                    self.view.sessions_status = Some(format!("Error al leer sesiones: {error}"));
                    self.view.status = format!("Error al leer sesiones: {error}");
                }
            }
        }
        self.refresh_composer_projects();
    }

    fn refresh_composer_projects(&mut self) {
        // Index-based menu actions must retain their targets while the menu is
        // open, even if a background session refresh changes sidebar ordering.
        if self.view.composer_project_menu_open {
            return;
        }
        self.view.composer_project_options =
            composer_project_options(&self.view.workspace, &self.known_project_paths);
    }

    fn persist_sidebar_preferences(&mut self) {
        let preferences = SidebarPreferences {
            organization: self.view.session_organization,
            sort: self.view.session_sort,
            collapsed_projects: self.view.collapsed_projects.clone(),
            sidebar_collapsed: self.view.sidebar_collapsed,
        };
        if let Err(error) = preferences.save(&self.data_root) {
            self.view.status = format!("No se pudieron guardar preferencias de sesiones: {error}");
        }
    }

    fn native_settings(&self) -> NativeProviderSettings {
        NativeProviderSettings {
            provider_id: self.view.native_provider_id.clone(),
            model_id: self.view.model.clone(),
            custom_endpoint: self.view.endpoint.clone(),
        }
    }

    fn set_settings_notice(&mut self, message: impl Into<String>, is_error: bool) {
        self.view.settings_notice = Some(message.into());
        self.view.settings_notice_error = is_error;
    }

    fn clear_settings_notice(&mut self) {
        self.view.settings_notice = None;
        self.view.settings_notice_error = false;
    }

    fn commit_settings_preferences(&mut self, next: SettingsPreferences, message: &str) -> bool {
        if let Err(error) = next.save(&self.data_root) {
            self.set_settings_notice(error, true);
            return false;
        }
        self.settings_preferences = next;
        self.set_settings_notice(message, false);
        true
    }

    fn commit_agent_tool_settings(&mut self, next: AgentToolSettings, message: &str) -> bool {
        if let Err(error) = next.save(&self.data_root) {
            self.set_settings_notice(error, true);
            return false;
        }
        self.agent_tool_settings = next;
        self.view.native_tool_permissions = next.permissions();
        self.view.workspace_writes_enabled =
            next.permissions().write_file && next.permissions().run_command;
        self.set_settings_notice(message, false);
        true
    }

    fn persist_native_settings(&mut self) -> bool {
        let settings = self.native_settings();
        self.view.native_key_available = settings.key_available();
        if let Err(error) = settings.save(&self.data_root) {
            let message = format!("No se pudo guardar el proveedor: {error}");
            self.view.status = message.clone();
            self.set_settings_notice(message, true);
            return false;
        }
        self.clear_settings_notice();
        let mut choice = self.composer_preferences.choice("native", &self.view.model);
        choice.model = self.view.model.clone();
        self.composer_preferences.set("native", choice);
        // native-provider.json owns this model choice. Composer preferences
        // are synchronized in memory and need no second disk commit.
        if self.view.selected_backend == "native" {
            self.refresh_composer_selection();
        }
        true
    }

    fn begin_catalog_refresh(&mut self) {
        if self.catalog_response.is_some() {
            return;
        }
        let root = self.data_root.clone();
        let previous = self.view.catalog.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = model_catalog::refresh(&root, previous.as_deref());
            let _ = sender.send(result);
        });
        self.catalog_response = Some(receiver);
        self.catalog_last_refresh = Instant::now();
        self.view.catalog_refreshing = true;
    }

    fn poll_catalog(&mut self) -> bool {
        if self.catalog_response.is_none()
            && self.catalog_last_refresh.elapsed() >= Duration::from_secs(3_600)
        {
            self.begin_catalog_refresh();
        }
        let Some(receiver) = &self.catalog_response else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                self.catalog_response = None;
                self.view.catalog_refreshing = false;
                self.view.catalog_status =
                    "No se pudo actualizar; se conserva la copia local".to_owned();
                return true;
            }
        };
        self.catalog_response = None;
        self.view.catalog_refreshing = false;
        match result {
            Ok(snapshot) => {
                self.view.catalog_status = catalog_status(&snapshot);
                self.view.catalog = Some(Arc::new(snapshot));
            }
            Err(error) => {
                self.view.catalog_status =
                    format!("Sin actualización ({error}); se conserva la última copia válida");
            }
        }
        true
    }

    fn refresh_composer_selection(&mut self) {
        let choice = self
            .composer_preferences
            .choice(&self.view.selected_backend, &self.view.model);
        self.view.composer_model = choice.model;
        self.view.composer_model_options = composer_selection::model_options(
            &self.view.selected_backend,
            &self.view.composer_model,
        );
        self.view.composer_effort_options = composer_selection::effort_options(
            &self.view.selected_backend,
            &self.view.composer_model,
        );
        self.view.composer_effort = if self
            .view
            .composer_effort_options
            .iter()
            .any(|(effort, _)| effort == &choice.effort)
        {
            choice.effort
        } else {
            self.view
                .composer_effort_options
                .first()
                .map(|(effort, _)| effort.clone())
                .unwrap_or_else(|| "default".to_owned())
        };
        self.view.composer_model_menu_open = false;
        self.view.composer_effort_menu_open = false;
        self.view.backend_menu_open = false;
    }

    fn remember_composer_selection(&mut self, save: bool) {
        self.composer_preferences.set(
            &self.view.selected_backend,
            ComposerChoice {
                model: self.view.composer_model.clone(),
                effort: self.view.composer_effort.clone(),
            },
        );
        if save {
            if let Err(error) = self.composer_preferences.save(&self.data_root) {
                self.view.status = format!("No se pudo guardar el selector de modelo: {error}");
            }
        }
    }

    fn select_composer_model(&mut self, model: &str) {
        if self.view.running
            || !self
                .view
                .composer_model_options
                .iter()
                .any(|(candidate, _)| candidate == model)
        {
            return;
        }
        if self.view.selected_backend == "native" && model == "openide:configure-model" {
            self.view.composer_model_menu_open = false;
            self.view.composer_effort_menu_open = false;
            self.view.show_settings = true;
            self.view.settings_page = SettingsPage::Providers;
            self.clear_settings_notice();
            self.visit_navigation();
            return;
        }
        self.view.composer_model = model.to_owned();
        self.view.composer_effort_options =
            composer_selection::effort_options(&self.view.selected_backend, model);
        if !self
            .view
            .composer_effort_options
            .iter()
            .any(|(candidate, _)| candidate == &self.view.composer_effort)
        {
            self.view.composer_effort = self
                .view
                .composer_effort_options
                .first()
                .map(|(effort, _)| effort.clone())
                .unwrap_or_else(|| "default".to_owned());
        }
        if self.view.selected_backend == "native" {
            self.view.model = model.to_owned();
            self.persist_native_settings();
        } else {
            self.remember_composer_selection(true);
        }
        self.view.composer_model_menu_open = false;
        self.view.composer_effort_menu_open = false;
    }

    fn select_composer_effort(&mut self, effort: &str) {
        if self.view.running
            || !self
                .view
                .composer_effort_options
                .iter()
                .any(|(candidate, _)| candidate == effort)
        {
            return;
        }
        self.view.composer_effort = effort.to_owned();
        self.view.composer_effort_menu_open = false;
        self.view.composer_model_menu_open = false;
        self.remember_composer_selection(true);
    }

    fn new_session(&mut self) {
        if self.view.running {
            self.view.status = "Detén el agente antes de cambiar de sesión".to_owned();
            return;
        }
        self.save_draft();
        self.view.main_page = MainPage::Workspace;
        self.view.background_run_selected = false;
        self.view.selected_session = None;
        self.view.selected_session_title = None;
        self.view.transcript.clear();
        self.view.selected_tool_id = None;
        self.view.activity_page = 0;
        self.view.pending_permissions.clear();
        self.close_session_menu();
        self.projection = TranscriptProjection::new();
        self.history.clear();
        self.last_sequence = 0;
        self.pending_prompt = None;
        self.pending_attachments.clear();
        self.view.composer_add_menu_open = false;
        self.view.composer_project_menu_open = false;
        self.run_has_output = false;
        self.persistence_error = None;
        self.restore_draft();
        self.tree.scroll_to_end("transcript");
        self.view.status = "Nueva sesión".to_owned();
        self.ensure_dock_tabs();
        self.prompt_focus_pending = true;
        self.show_persistence_error();
    }

    fn focus_prompt_if_pending(&mut self) {
        if self.prompt_focus_pending && !self.view.show_command_center && self.tree.focus("prompt")
        {
            self.prompt_focus_pending = false;
        }
    }

    fn focus_group_if_pending(&mut self) {
        if !self.group_focus_pending || self.view.show_command_center {
            return;
        }
        let target = if self.view.group_create_open {
            "group_create_draft"
        } else {
            "group_rename_draft"
        };
        if self.tree.focus(target) {
            self.group_focus_pending = false;
        }
    }

    fn commit_groups(&mut self, previous: SessionGroups) -> bool {
        if let Err(error) = self.view.session_groups.save(&self.data_root) {
            self.view.session_groups = previous;
            self.view.status = format!("No se pudieron guardar los grupos: {error}");
            false
        } else {
            true
        }
    }

    fn commit_group_create(&mut self) {
        if !self.view.group_create_open {
            return;
        }
        let previous = self.view.session_groups.clone();
        if self
            .view
            .session_groups
            .create(&self.view.group_create_draft)
            .is_none()
        {
            self.view.status = "Escribe un nombre de grupo válido".to_owned();
            return;
        }
        if self.commit_groups(previous) {
            self.view.group_create_open = false;
            self.view.group_create_draft.clear();
        }
    }

    fn commit_group_rename(&mut self, group_id: &str) {
        if self.view.group_rename_id.as_deref() != Some(group_id) {
            return;
        }
        let previous = self.view.session_groups.clone();
        if self
            .view
            .session_groups
            .rename(group_id, &self.view.group_rename_draft)
        {
            if self.commit_groups(previous) {
                self.view.group_rename_id = None;
                self.view.group_rename_draft.clear();
            }
        } else if self
            .view
            .session_groups
            .groups
            .iter()
            .any(|group| group.id == group_id && group.title == self.view.group_rename_draft.trim())
        {
            self.view.group_rename_id = None;
        } else {
            self.view.status = "Escribe un nombre de grupo válido".to_owned();
        }
    }

    fn close_session_menu(&mut self) {
        self.view.session_menu_id = None;
        self.view.renaming_session_id = None;
        self.view.pending_archive_session_id = None;
        self.view.pending_delete_session_id = None;
    }

    fn commit_session_rename(&mut self, session_id: &str) {
        if self.view.renaming_session_id.as_deref() != Some(session_id) {
            return;
        }
        let title = self.view.rename_draft.trim();
        if title.is_empty() {
            self.view.status = "Escribe un título antes de guardar".to_owned();
            return;
        }
        if self
            .view
            .sessions
            .iter()
            .any(|session| session.id == session_id && session.title == title)
        {
            self.close_session_menu();
            return;
        }
        if let Some(store) = &self.store {
            match store.rename(session_id, Some(title)) {
                Ok(_) => {
                    self.close_session_menu();
                    self.refresh_sessions();
                }
                Err(error) => self.view.status = format!("No se pudo renombrar: {error}"),
            }
        }
    }

    fn open_session(&mut self, id: &str) {
        if self.view.running {
            self.view.status = "Detén el agente antes de cambiar de sesión".to_owned();
            return;
        }
        self.prompt_focus_pending = false;
        let Some(store) = &self.store else { return };
        let loaded = store.load(id);
        match loaded {
            Ok(Some(stored)) => {
                self.new_session();
                self.view.selected_session_title = Some(
                    stored
                        .session
                        .title
                        .clone()
                        .or_else(|| {
                            stored.events.iter().find_map(|event| {
                                if let AgentEventKind::TurnStarted { prompt } = &event.kind {
                                    Some(prompt.lines().next().unwrap_or(prompt).trim().to_owned())
                                } else {
                                    None
                                }
                            })
                        })
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| "Nueva tarea".to_owned()),
                );
                self.view.selected_session = Some(stored.session.id);
                let workspace_changed =
                    self.view.workspace != stored.session.workspace.to_string_lossy();
                self.view.workspace = stored.session.workspace.to_string_lossy().into_owned();
                self.view.workspace_draft = self.view.workspace.clone();
                if workspace_changed {
                    self.view.project_branch = project_branch_for_workspace(&self.view.workspace);
                    self.invalidate_workspace_files();
                }
                self.refresh_composer_projects();
                self.restore_draft();
                self.ensure_dock_tabs();
                if self.view.side_tab == SideTab::Files {
                    self.refresh_file_rows();
                }
                self.tree
                    .set_input_value("workspace", self.view.workspace.clone());
                self.view.selected_backend = stored.session.backend;
                self.refresh_composer_selection();
                self.last_sequence = stored
                    .events
                    .iter()
                    .map(|event| event.sequence)
                    .max()
                    .unwrap_or(0);
                self.projection = TranscriptProjection::from_events(&stored.events);
                self.view.transcript = self.projection.items().to_vec();
                self.history = history_from_events(&stored.events);
                self.tree.scroll_to_end("transcript");
                self.view.status = "Sesión cargada".to_owned();
                if let Some(marker) = stored.session.unread_at_ms {
                    match self
                        .store
                        .as_ref()
                        .expect("session store still open")
                        .clear_unread_if_matches(id, marker)
                    {
                        Ok(_) => self.refresh_sessions(),
                        Err(error) => {
                            self.view.status = format!("No se pudo marcar como leída: {error}");
                        }
                    }
                }
                self.show_persistence_error();
                self.sync_background_selected();
            }
            Ok(None) => self.view.status = "La sesión ya no existe".to_owned(),
            Err(error) => self.view.status = format!("Error al abrir sesión: {error}"),
        }
    }

    fn start_run(&mut self) {
        if self.view.running {
            return;
        }
        if self
            .view
            .selected_session
            .as_deref()
            .is_some_and(|selected| {
                self.background_automation_runs
                    .iter()
                    .any(|run| run.session_id == selected)
            })
        {
            self.view.status = "Espera a que termine la automatización de esta sesión".to_owned();
            return;
        }
        if self.active_run.is_some() {
            self.view.status = "Finalizando turno anterior...".to_owned();
            return;
        }
        if self.persistence_error.is_some() {
            self.show_persistence_error();
            return;
        }
        let visible_prompt = self.view.prompt.trim().to_owned();
        let attachments = self.view.composer_attachments.clone();
        if visible_prompt.is_empty() && attachments.is_empty() {
            return;
        }
        let workspace = PathBuf::from(self.view.workspace.trim());
        if !workspace.is_dir() {
            self.view.status = "El directorio de trabajo no existe".to_owned();
            return;
        }
        for attachment in &attachments {
            match composer_attachment_for_path(&workspace, Path::new(&attachment.path)) {
                Ok(current) if current.path == attachment.path => {}
                Ok(_) => {
                    self.view.status = "Un archivo adjunto cambió de ubicación".to_owned();
                    return;
                }
                Err(error) => {
                    self.view.status = format!("Archivo adjunto no disponible: {error}");
                    return;
                }
            }
        }
        let prompt = composer_prompt_with_attachments(&visible_prompt, &attachments);
        let backend = self.view.selected_backend.clone();
        let submitted_draft_key = self.draft_key();
        let native_connection = if backend == "native" {
            let connection = match self.native_settings().resolve() {
                Ok(connection) => connection,
                Err(error) => {
                    self.view.status = error;
                    return;
                }
            };
            if connection.api_key.is_none() && self.view.native_provider_id != "custom" {
                let key_env = native_provider::adapter(&self.view.native_provider_id)
                    .map(|adapter| adapter.key_env)
                    .unwrap_or("API_KEY");
                self.view.status = format!("Define {key_env} para usar el harness nativo");
                return;
            }
            Some(connection)
        } else {
            None
        };
        let cli_profile = if backend == "native" {
            None
        } else {
            let Some(mut profile) = self
                .profiles
                .iter()
                .find(|profile| profile.id == backend)
                .cloned()
            else {
                self.view.status = "Perfil de agente desconocido".to_owned();
                return;
            };
            if profile.detect_executable().is_none() {
                self.view.status = format!("Instala {} o configura su ruta", profile.label);
                return;
            }
            if backend == "codex" {
                if self.view.composer_model != "default" {
                    profile = match profile.with_model(self.view.composer_model.clone()) {
                        Ok(profile) => profile,
                        Err(error) => {
                            self.view.status = error;
                            return;
                        }
                    };
                }
                if self.view.composer_effort != "default" {
                    profile = match profile.with_reasoning_effort(self.view.composer_effort.clone())
                    {
                        Ok(profile) => profile,
                        Err(error) => {
                            self.view.status = error;
                            return;
                        }
                    };
                }
            }
            Some(profile)
        };
        let native_skills = if backend == "native" {
            match crate::marketplace::active_skill_instructions(&self.data_root, &workspace) {
                Ok(skills) => skills,
                Err(error) => {
                    self.view.status = format!("No se pudieron leer las skills: {error}");
                    return;
                }
            }
        } else {
            String::new()
        };
        let Some(store) = &self.store else {
            self.view.status = "El historial no está disponible".to_owned();
            return;
        };
        let id = match &self.view.selected_session {
            Some(id) => id.clone(),
            None => match store.create(&workspace, &backend) {
                Ok(session) => {
                    self.view.selected_session = Some(session.id.clone());
                    session.id
                }
                Err(error) => {
                    self.view.status = format!("No se pudo crear la sesión: {error}");
                    return;
                }
            },
        };
        let request = AgentRunRequest {
            session_id: Some(id),
            sequence_start: self.last_sequence,
            workspace,
            history: self.history.clone(),
            prompt: prompt.clone(),
        };
        let active = if backend == "native" {
            let connection = native_connection.expect("validated native connection");
            let mut model = OpenAiCompatibleBackend::new(
                connection.endpoint,
                connection.api_key,
                connection.model_id,
            );
            if self.view.native_provider_id == "deepseek" {
                model = model.with_deepseek_non_thinking();
            }
            if self.view.composer_effort != "default" {
                model = match model.with_reasoning_effort(self.view.composer_effort.clone()) {
                    Ok(model) => model,
                    Err(error) => {
                        self.view.status = error.to_string();
                        return;
                    }
                };
            }
            let model = Arc::new(model);
            let tools = Arc::new(
                LocalTools::new().with_permissions(self.agent_tool_settings.permissions()),
            );
            let policy = self.agent_tool_settings.permission_policy();
            ActiveRun::Native(
                AgentRunner::new(model, tools)
                    .with_permission_policy(policy)
                    .with_system_instructions(native_skills)
                    .start(request),
            )
        } else {
            ActiveRun::Cli(CliRunner::start(
                cli_profile.expect("validated CLI profile"),
                request,
            ))
        };
        self.active_run = Some(active);
        if let Some(session_id) = &self.view.selected_session {
            self.dock.promote_draft(&self.view.workspace, session_id);
        }
        let scope = self.dock_scope();
        self.dock.set_collapsed(&scope, false);
        self.ensure_dock_tabs();
        self.visit_navigation();
        self.pending_prompt = Some(visible_prompt);
        self.pending_attachments = attachments;
        self.run_has_output = false;
        self.view.prompt.clear();
        self.view.composer_attachments.clear();
        self.view.composer_add_menu_open = false;
        self.view.composer_project_menu_open = false;
        self.clear_draft_key(&submitted_draft_key);
        self.draft_dirty_at = None;
        self.tree.set_input_value("prompt", "");
        self.tree.scroll_to_end("transcript");
        self.view.running = true;
        self.view.status = format!("Ejecutando {backend}...");
        self.refresh_sessions();
        self.show_persistence_error();
    }

    fn process_event(&mut self, event: AgentEvent) -> bool {
        let follow_tail = self.tree.is_scroll_at_end("transcript", 24.0);
        self.last_sequence = self.last_sequence.max(event.sequence);
        let persisted = if let Some(store) = &self.store {
            if let Err(error) = store.append_event(&event.session_id, &event) {
                self.persistence_error = Some(error.to_string());
                false
            } else {
                true
            }
        } else {
            false
        };
        if !persisted {
            if let AgentEventKind::PermissionRequested { request_id, .. } = &event.kind {
                if let Some(run) = &self.active_run {
                    run.resolve_permission(request_id, PermissionChoice::Deny);
                }
            }
        }
        let changed = self.projection.apply_event(&event);
        if matches!(
            &event.kind,
            AgentEventKind::AssistantDelta { .. }
                | AgentEventKind::AssistantMessage { .. }
                | AgentEventKind::ToolStarted { .. }
                | AgentEventKind::ProcessOutput { .. }
        ) {
            self.run_has_output = true;
        }
        match &event.kind {
            AgentEventKind::PermissionRequested {
                request_id,
                tool_call,
            } => {
                if persisted
                    && !self
                        .view
                        .pending_permissions
                        .iter()
                        .any(|permission| permission.request_id == *request_id)
                {
                    self.view.pending_permissions.push(PermissionView {
                        request_id: request_id.clone(),
                        tool_name: tool_call.name.clone(),
                        arguments: serde_json::to_string_pretty(&tool_call.arguments)
                            .unwrap_or_default(),
                    });
                }
                self.view.status = format!("Esperando permiso para {}", tool_call.name);
            }
            AgentEventKind::PermissionResolved { request_id, .. } => {
                self.view
                    .pending_permissions
                    .retain(|permission| permission.request_id != *request_id);
            }
            AgentEventKind::TurnStarted { prompt }
                if self
                    .view
                    .selected_session_title
                    .as_deref()
                    .is_none_or(|title| title == "Nueva tarea") =>
            {
                let title = prompt.lines().next().unwrap_or(prompt).trim();
                if !title.is_empty() {
                    self.view.selected_session_title = Some(title.to_owned());
                }
            }
            AgentEventKind::ModelRequestStarted { step, model } => {
                self.view.status = format!("{model} · paso {step}");
            }
            AgentEventKind::TurnCompleted { .. } => {
                self.view.running = false;
                self.view.status = "Turno completado".to_owned();
                self.pending_prompt = None;
                self.pending_attachments.clear();
            }
            AgentEventKind::TurnFailed { .. } => {
                self.view.running = false;
                self.view.status = "Turno fallido".to_owned();
                self.restore_pending_prompt();
            }
            AgentEventKind::TurnCancelled => {
                self.view.running = false;
                self.view.status = "Turno cancelado".to_owned();
                self.restore_pending_prompt();
            }
            _ => {}
        }
        self.show_persistence_error();
        if changed && follow_tail {
            self.tree.scroll_to_end("transcript");
        }
        changed
    }

    fn poll_run(&mut self) {
        let Some(run) = &self.active_run else { return };
        let (events, disconnected) = run.drain();
        let mut transcript_changed = false;
        for event in events {
            transcript_changed |= self.process_event(event);
        }
        if transcript_changed {
            self.view.transcript = self.projection.items().to_vec();
        }
        if disconnected {
            self.view.pending_permissions.clear();
            if self.view.running {
                self.view.running = false;
                self.view.status = "El proceso terminó sin evento final".to_owned();
                self.restore_pending_prompt();
            }
            self.active_run = None;
            self.rebuild_history();
            self.refresh_sessions();
            self.show_persistence_error();
        }
    }

    fn rebuild_history(&mut self) {
        if let (Some(store), Some(id)) = (&self.store, &self.view.selected_session) {
            if let Ok(Some(stored)) = store.load(id) {
                self.history = history_from_events(&stored.events);
            }
        }
    }

    fn restore_pending_prompt(&mut self) {
        let attachments = std::mem::take(&mut self.pending_attachments);
        if let Some(prompt) = self.pending_prompt.take() {
            if !self.run_has_output {
                if self.view.prompt.is_empty() {
                    self.view.prompt = prompt.clone();
                    self.tree.set_input_value("prompt", prompt);
                }
                for file in attachments {
                    if !self
                        .view
                        .composer_attachments
                        .iter()
                        .any(|existing| existing.path == file.path)
                    {
                        self.view.composer_attachments.push(file);
                    }
                }
                let key = self.draft_key();
                if !self.view.composer_attachments.is_empty() {
                    self.attachment_drafts
                        .insert(key, self.view.composer_attachments.clone());
                }
                self.draft_dirty_at = Some(Instant::now());
            }
        }
    }

    fn navigation_target(&self) -> NavigationTarget {
        let workspace = PathBuf::from(&self.view.workspace);
        if self.view.show_settings {
            NavigationTarget::Settings { workspace }
        } else if self.view.main_page == MainPage::Automations {
            NavigationTarget::Automations { workspace }
        } else if self.view.main_page == MainPage::Marketplace {
            NavigationTarget::Marketplace { workspace }
        } else if let Some(session_id) = &self.view.selected_session {
            NavigationTarget::Task {
                workspace,
                session_id: session_id.clone(),
            }
        } else {
            NavigationTarget::Draft { workspace }
        }
    }

    fn sync_navigation_controls(&mut self) {
        self.view.can_go_back = self.navigation.can_back();
        self.view.can_go_forward = self.navigation.can_forward();
    }

    fn refresh_command_results(&mut self) {
        let mut session_rows: Vec<(String, String, String)> = self
            .view
            .sessions
            .iter()
            .map(|session| {
                (
                    session.id.clone(),
                    session.title.clone(),
                    session.project.clone(),
                )
            })
            .collect();
        let mut file_rows: Vec<(PathBuf, String, bool)> = self
            .file_explorer
            .as_ref()
            .map(|explorer| {
                explorer
                    .rows()
                    .iter()
                    .map(|row| (row.relative.clone(), row.name.clone(), row.is_dir))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(response) = &self.search_response {
            for hit in &response.sessions {
                if !session_rows.iter().any(|(id, _, _)| id == &hit.id) {
                    let subtitle = hit
                        .snippets
                        .first()
                        .cloned()
                        .unwrap_or_else(|| hit.workspace.to_string_lossy().into_owned());
                    session_rows.push((hit.id.clone(), hit.title.clone(), subtitle));
                }
            }
            for path in &response.files {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("")
                    .to_owned();
                file_rows.push((path.clone(), name, false));
            }
        }
        let sessions: Vec<_> = session_rows
            .iter()
            .map(|(id, title, subtitle)| SessionCandidate {
                id,
                title,
                subtitle,
            })
            .collect();
        let files: Vec<_> = file_rows
            .iter()
            .map(|(path, name, is_dir)| FileCandidate {
                path,
                name,
                is_dir: *is_dir,
            })
            .collect();
        self.view.command_results = command_center::search(
            &self.view.command_query,
            self.view.command_scope,
            &sessions,
            &files,
            command_center::DEFAULT_RESULT_LIMIT,
        );
        self.view.command_selected_index = self
            .view
            .command_selected_index
            .min(self.view.command_results.len().saturating_sub(1));
    }

    fn request_command_search(&mut self) {
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_response = None;
        self.search_in_flight = false;
        self.search_pending_at = Some(Instant::now());
        self.refresh_command_results();
    }

    fn update_command_search(&mut self, ctx: &egui::Context) -> bool {
        if !self.view.show_command_center {
            return false;
        }
        let mut changed = false;
        if let Some(queued_at) = self.search_pending_at {
            let debounce = Duration::from_millis(160);
            if queued_at.elapsed() >= debounce {
                self.search_pending_at = None;
                let query =
                    command_center::parse_query(&self.view.command_query, self.view.command_scope);
                if query.scope != Scope::Commands {
                    if let Some(worker) = &self.search_worker {
                        self.search_in_flight = worker
                            .submit(SearchRequest {
                                workspace: PathBuf::from(&self.view.workspace),
                                query: query.text,
                                generation: self.search_generation,
                            })
                            .is_ok();
                    }
                }
            } else {
                ctx.request_repaint_after(debounce - queued_at.elapsed());
            }
        }
        if let Some(worker) = &self.search_worker {
            if let Some(response) =
                worker.poll_latest(self.search_generation, Path::new(&self.view.workspace))
            {
                changed = true;
                self.search_response = Some(response);
                self.search_in_flight = false;
                self.refresh_command_results();
            }
        }
        if self.search_in_flight {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        changed
    }

    fn open_command_center(&mut self) {
        self.command_center_attaches_file = false;
        self.view.composer_add_menu_open = false;
        self.view.composer_project_menu_open = false;
        self.view.show_command_center = true;
        self.view.command_query.clear();
        self.view.command_scope = Scope::All;
        self.view.command_selected_index = 0;
        self.command_tree.set_input_value("command_query", "");
        self.request_command_search();
        self.command_focus_pending = true;
    }

    fn open_composer_file_picker(&mut self) {
        self.open_command_center();
        self.command_center_attaches_file = true;
        self.view.command_scope = Scope::Files;
        self.request_command_search();
    }

    fn attach_composer_file(&mut self, candidate: &Path) {
        if self.view.composer_attachments.len() >= MAX_COMPOSER_ATTACHMENTS {
            self.view.status = format!("Máximo de {MAX_COMPOSER_ATTACHMENTS} archivos adjuntos");
            return;
        }
        match composer_attachment_for_path(Path::new(&self.view.workspace), candidate) {
            Ok(file) => {
                if self
                    .view
                    .composer_attachments
                    .iter()
                    .any(|existing| existing.path == file.path)
                {
                    self.view.status = "El archivo ya está adjunto".to_owned();
                    return;
                }
                self.view.status = format!("Archivo adjunto: {}", file.label);
                self.view.composer_attachments.push(file);
                let key = self.draft_key();
                self.attachment_drafts
                    .insert(key, self.view.composer_attachments.clone());
                self.prompt_focus_pending = true;
            }
            Err(error) => self.view.status = error,
        }
    }

    fn activate_command_result(&mut self, index: usize) {
        let Some(target) = self
            .view
            .command_results
            .get(index)
            .map(|result| result.target.clone())
        else {
            return;
        };
        self.view.show_command_center = false;
        let attach_file = std::mem::take(&mut self.command_center_attaches_file);
        match target {
            Target::NewTask => {
                self.new_session();
                if !self.view.running {
                    self.view.show_settings = false;
                    self.visit_navigation();
                }
            }
            Target::OpenSessions => {
                self.view.show_settings = false;
                self.view.main_page = MainPage::Workspace;
                self.visit_navigation();
            }
            Target::OpenFiles => {
                let changed_page =
                    self.view.show_settings || self.view.main_page != MainPage::Workspace;
                self.view.show_settings = false;
                self.view.main_page = MainPage::Workspace;
                self.open_files_dock();
                if changed_page {
                    self.visit_navigation();
                }
            }
            Target::OpenSettings => {
                self.view.show_settings = true;
                self.visit_navigation();
            }
            Target::OpenAutomations => {
                self.view.show_settings = false;
                self.view.main_page = MainPage::Automations;
                self.refresh_automation_view();
                self.visit_navigation();
            }
            Target::OpenMarketplace => {
                self.view.show_settings = false;
                self.view.main_page = MainPage::Marketplace;
                self.marketplace_operation(MarketplaceOperation::Load);
                self.visit_navigation();
            }
            Target::OpenCodeReview => {
                let changed_page =
                    self.view.show_settings || self.view.main_page != MainPage::Workspace;
                self.view.show_settings = false;
                self.view.main_page = MainPage::Workspace;
                let scope = self.dock_scope();
                let workspace_scope =
                    DockScope::new(scope.workspace_id.clone(), DockOwner::Workspace);
                let id = self.dock.open(&workspace_scope, DockTabKind::CodeReview);
                self.dock.activate(&scope, id);
                self.dock.set_collapsed(&scope, false);
                self.sync_dock_view();
                if changed_page {
                    self.visit_navigation();
                }
            }
            Target::ToggleRightDock => {
                let changed_page =
                    self.view.show_settings || self.view.main_page != MainPage::Workspace;
                self.view.show_settings = false;
                self.view.main_page = MainPage::Workspace;
                let scope = self.dock_scope();
                self.dock.set_collapsed(&scope, self.view.show_side_pane);
                self.sync_dock_view();
                if changed_page {
                    self.visit_navigation();
                }
            }
            Target::OpenSession(id) => {
                self.open_session(&id);
                if self.view.selected_session.as_deref() == Some(id.as_str()) {
                    self.view.show_settings = false;
                    self.visit_navigation();
                }
            }
            Target::OpenFile(path) => {
                if attach_file {
                    self.attach_composer_file(&path);
                    return;
                }
                let workspace = Path::new(&self.view.workspace);
                let owner = self.view.selected_session.as_deref().unwrap_or("");
                if self.dock.open_file(workspace, owner, &path).is_some() {
                    let changed_page =
                        self.view.show_settings || self.view.main_page != MainPage::Workspace;
                    self.view.show_settings = false;
                    self.view.main_page = MainPage::Workspace;
                    let scope = self.dock_scope();
                    self.dock.set_collapsed(&scope, false);
                    self.sync_dock_view();
                    if changed_page {
                        self.visit_navigation();
                    }
                }
            }
        }
    }

    fn visit_navigation(&mut self) {
        self.navigation.visit(self.navigation_target());
        self.sync_navigation_controls();
    }

    fn restore_navigation(&mut self, target: &NavigationTarget) -> bool {
        let desired_session = match target {
            NavigationTarget::Task { session_id, .. } => Some(session_id.as_str()),
            NavigationTarget::Draft { .. }
            | NavigationTarget::Settings { .. }
            | NavigationTarget::Automations { .. }
            | NavigationTarget::Marketplace { .. } => None,
        };
        let changes_context = Path::new(&self.view.workspace) != target.workspace()
            || (desired_session.is_some()
                && desired_session != self.view.selected_session.as_deref())
            || (matches!(target, NavigationTarget::Draft { .. })
                && self.view.selected_session.is_some());
        if self.view.running && changes_context {
            self.view.status = "Detén el agente antes de cambiar de tarea".to_owned();
            return false;
        }
        if let Some(session_id) = desired_session {
            if self.view.selected_session.as_deref() != Some(session_id)
                || Path::new(&self.view.workspace) != target.workspace()
            {
                self.open_session(session_id);
            }
            if self.view.selected_session.as_deref() != Some(session_id) {
                return false;
            }
        } else if matches!(target, NavigationTarget::Draft { .. }) {
            if self.view.selected_session.is_some() {
                self.new_session();
            }
            if Path::new(&self.view.workspace) != target.workspace() {
                self.change_workspace(target.workspace().to_string_lossy().into_owned());
            }
        } else if Path::new(&self.view.workspace) != target.workspace() {
            self.change_workspace(target.workspace().to_string_lossy().into_owned());
        }
        self.view.show_settings = matches!(target, NavigationTarget::Settings { .. });
        self.clear_settings_notice();
        self.view.main_page = match target {
            NavigationTarget::Automations { .. } => MainPage::Automations,
            NavigationTarget::Marketplace { .. } => MainPage::Marketplace,
            NavigationTarget::Task { .. } | NavigationTarget::Draft { .. } => MainPage::Workspace,
            NavigationTarget::Settings { .. } => self.view.main_page,
        };
        match self.view.main_page {
            MainPage::Automations => self.refresh_automation_view(),
            MainPage::Marketplace => self.marketplace_operation(MarketplaceOperation::Load),
            MainPage::Workspace => {}
        }
        Path::new(&self.view.workspace) == target.workspace()
    }

    fn go_back(&mut self) {
        if let Some(target) = self.navigation.back().cloned() {
            if !self.restore_navigation(&target) {
                self.navigation.forward();
            }
        }
        self.sync_navigation_controls();
    }

    fn go_forward(&mut self) {
        if let Some(target) = self.navigation.forward().cloned() {
            if !self.restore_navigation(&target) {
                self.navigation.back();
            }
        }
        self.sync_navigation_controls();
    }

    fn change_workspace(&mut self, value: String) {
        if self.view.running {
            self.view.status = "Detén el agente antes de cambiar de proyecto".to_owned();
            self.view.workspace_draft = self.view.workspace.clone();
            self.tree
                .set_input_value("workspace", self.view.workspace.clone());
            return;
        }
        if self.view.workspace == value {
            return;
        }
        if self.view.selected_session.is_some() {
            self.new_session();
        } else {
            self.save_draft();
        }
        self.view.workspace = value;
        self.view.workspace_draft = self.view.workspace.clone();
        self.view.project_branch = project_branch_for_workspace(&self.view.workspace);
        self.view.composer_project_menu_open = false;
        self.refresh_composer_projects();
        self.refresh_automation_view();
        self.invalidate_workspace_files();
        self.restore_draft();
        if Path::new(&self.view.workspace).is_dir() {
            let mut preferences = self.settings_preferences.clone();
            if preferences
                .set_last_workspace(Path::new(&self.view.workspace))
                .is_ok()
            {
                if let Err(error) = preferences.save(&self.data_root) {
                    self.set_settings_notice(error, true);
                } else {
                    self.settings_preferences = preferences;
                }
            }
            self.ensure_dock_tabs();
            if self.view.side_tab == SideTab::Files {
                self.refresh_file_rows();
            }
        }
        self.show_persistence_error();
    }

    fn open_workspace_folder_picker(&mut self, ctx: &egui::Context) {
        if self.workspace_folder_response.is_some() || self.view.running {
            return;
        }
        self.view.composer_project_menu_open = false;
        let initial = Path::new(&self.view.workspace)
            .ancestors()
            .find(|path| path.is_dir())
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        match std::thread::Builder::new()
            .name("workspace-folder-picker".to_owned())
            .spawn(move || {
                let _ = sender.send(choose_workspace_folder(&initial));
                repaint.request_repaint();
            }) {
            Ok(_) => self.workspace_folder_response = Some(receiver),
            Err(error) => {
                self.view.status = format!("No se pudo abrir el selector de carpetas: {error}");
            }
        }
    }

    fn apply_workspace_folder_selection(&mut self, selection: Result<Option<PathBuf>, String>) {
        match selection {
            Ok(None) => {}
            Ok(Some(path)) => match validate_workspace_folder(&path) {
                Ok(path) if path != self.view.workspace => {
                    self.change_workspace(path);
                    if !self.view.running {
                        self.visit_navigation();
                    }
                }
                Ok(_) => {}
                Err(error) => self.view.status = error,
            },
            Err(error) => self.view.status = error,
        }
    }

    fn poll_workspace_folder_picker(&mut self) -> bool {
        let Some(receiver) = &self.workspace_folder_response else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                Err("El selector de carpetas se cerró inesperadamente".to_owned())
            }
        };
        self.workspace_folder_response = None;
        self.apply_workspace_folder_selection(result);
        true
    }

    fn apply_action(&mut self, action: UiAction, ctx: &egui::Context) {
        match action {
            UiAction::CopyRequested { text } => ctx.copy_text(text),
            UiAction::Clicked { id } if id == "open_command_center" => {
                self.open_command_center();
            }
            UiAction::Clicked { id } if id == "composer_add_context" => {
                if !self.view.running && !self.view.background_run_selected {
                    self.view.composer_add_menu_open = !self.view.composer_add_menu_open;
                    self.view.composer_project_menu_open = false;
                }
            }
            UiAction::Clicked { id } if id == "composer_add_dismiss" => {
                self.view.composer_add_menu_open = false;
            }
            UiAction::Clicked { id } if id == "composer_add_file" => {
                if !self.view.running && !self.view.background_run_selected {
                    self.open_composer_file_picker();
                }
            }
            UiAction::Clicked { id } if id == "composer_add_browse" => {
                if !self.view.running && !self.view.background_run_selected {
                    self.open_command_center();
                    self.view.command_scope = Scope::Files;
                    self.request_command_search();
                }
            }
            UiAction::Clicked { id } if id.starts_with("composer_attachment_remove:") => {
                if let Ok(index) = id
                    .trim_start_matches("composer_attachment_remove:")
                    .parse::<usize>()
                {
                    if index < self.view.composer_attachments.len() {
                        self.view.composer_attachments.remove(index);
                        let key = self.draft_key();
                        if self.view.composer_attachments.is_empty() {
                            self.attachment_drafts.remove(&key);
                        } else {
                            self.attachment_drafts
                                .insert(key, self.view.composer_attachments.clone());
                        }
                    }
                }
            }
            UiAction::Clicked { id } if id == "open_automations" => {
                self.view.show_settings = false;
                self.view.main_page = MainPage::Automations;
                self.refresh_automation_view();
                self.visit_navigation();
            }
            UiAction::Clicked { id } if id == "open_marketplace" => {
                self.view.show_settings = false;
                self.view.main_page = MainPage::Marketplace;
                self.marketplace_operation(MarketplaceOperation::Load);
                self.visit_navigation();
            }
            UiAction::Clicked { id } if id.starts_with("automation_") => {
                self.handle_automation_action(&id);
            }
            UiAction::Clicked { id } if id.starts_with("marketplace_") => {
                self.handle_marketplace_action(&id);
            }
            UiAction::Clicked { id } if id == "command_close" => {
                self.view.show_command_center = false;
            }
            UiAction::Clicked { id } if id.starts_with("command_scope:") => {
                self.view.command_scope = match id.trim_start_matches("command_scope:") {
                    "commands" => Scope::Commands,
                    "sessions" => Scope::Sessions,
                    "files" => Scope::Files,
                    _ => Scope::All,
                };
                self.view.command_selected_index = 0;
                self.request_command_search();
            }
            UiAction::Clicked { id } if id.starts_with("command_result:") => {
                if let Ok(index) = id.trim_start_matches("command_result:").parse::<usize>() {
                    self.activate_command_result(index);
                }
            }
            UiAction::Clicked { id } if id.starts_with("permission_allow:") => {
                let request_id = id.trim_start_matches("permission_allow:");
                self.answer_permission(request_id, true);
            }
            UiAction::Clicked { id } if id.starts_with("permission_deny:") => {
                let request_id = id.trim_start_matches("permission_deny:");
                self.answer_permission(request_id, false);
            }
            UiAction::Clicked { id } if id == "run_toggle" => {
                if self.view.background_run_selected {
                    if let Some(selected) = self.view.selected_session.as_deref() {
                        if let Some(run) = self
                            .background_automation_runs
                            .iter()
                            .find(|run| run.session_id == selected)
                        {
                            run.run.cancel();
                            self.view.status = "Cancelando automatización…".to_owned();
                        }
                    }
                } else if self.view.running {
                    if let Some(run) = &self.active_run {
                        run.cancel();
                        self.view.status = "Cancelando...".to_owned();
                    }
                } else {
                    self.start_run();
                }
            }
            UiAction::Clicked { id }
                if matches!(
                    id.as_str(),
                    "new_session" | "new_session_sidebar" | "new_session_header"
                ) =>
            {
                self.new_session();
                if !self.view.running {
                    self.view.show_archived = false;
                    self.view.show_settings = false;
                    self.refresh_sessions();
                    self.visit_navigation();
                }
            }
            UiAction::Clicked { id } if id == "files_refresh" => {
                if let Some(explorer) = &mut self.file_explorer {
                    if let Err(error) = explorer.refresh() {
                        self.view.files_status = format!("No se pudo actualizar: {error}");
                    }
                }
                self.refresh_file_rows();
            }
            UiAction::Clicked { id } if id == "file_more_root" || id.starts_with("file_more:") => {
                let relative = if id == "file_more_root" {
                    Some(PathBuf::new())
                } else {
                    id.strip_prefix("file_more:")
                        .and_then(|index| index.parse::<usize>().ok())
                        .and_then(|index| self.file_explorer.as_ref()?.rows().get(index))
                        .filter(|row| row.is_dir && row.expanded)
                        .map(|row| row.relative.clone())
                };
                if let Some(relative) = relative {
                    let result = self
                        .file_explorer
                        .as_mut()
                        .map(|explorer| explorer.load_more(&relative));
                    self.refresh_file_rows();
                    if let Some(Err(error)) = result {
                        self.view.files_status =
                            format!("No se pudieron cargar más archivos: {error}");
                    }
                }
            }
            UiAction::Clicked { id } if id == "activity_page_previous" => {
                self.view.activity_page = self.view.activity_page.saturating_sub(1);
            }
            UiAction::Clicked { id } if id == "activity_page_next" => {
                self.view.activity_page = self.view.activity_page.saturating_add(1);
            }
            UiAction::Clicked { id } if id == "review_page_previous" => {
                self.view.review_page = self.view.review_page.saturating_sub(1);
            }
            UiAction::Clicked { id } if id == "review_page_next" => {
                self.view.review_page = self.view.review_page.saturating_add(1);
            }
            UiAction::Clicked { id } if id.starts_with("review_file_open:") => {
                let indices: Vec<usize> = id
                    .trim_start_matches("review_file_open:")
                    .split(':')
                    .filter_map(|value| value.parse().ok())
                    .collect();
                if let [section_index, file_index] = indices.as_slice() {
                    let selection = self
                        .view
                        .git_review
                        .as_ref()
                        .and_then(|result| result.as_ref().ok())
                        .and_then(|review| review.sections.get(*section_index))
                        .and_then(|section| {
                            let stage = match section.title.as_str() {
                                "CAMBIOS PREPARADOS" => DiffStage::Staged,
                                "CAMBIOS SIN PREPARAR" => DiffStage::Unstaged,
                                _ => return None,
                            };
                            section
                                .files
                                .get(*file_index)
                                .map(|file| (file.path.clone(), stage))
                        });
                    if let Some((path, stage)) = selection {
                        self.view.review_detail = Some(ReviewDetailView {
                            path,
                            stage,
                            page_index: 0,
                            content: None,
                        });
                        self.request_review_detail();
                    }
                }
            }
            UiAction::Clicked { id } if id == "review_detail_back" => {
                self.view.review_detail = None;
                if self
                    .detail_request
                    .as_ref()
                    .is_some_and(|request| matches!(request.kind, DetailKind::Diff { .. }))
                {
                    self.detail_request = None;
                }
            }
            UiAction::Clicked { id } if id == "review_detail_retry" => {
                self.request_review_detail();
            }
            UiAction::Clicked { id } if id == "review_detail_previous" => {
                if let Some(detail) = &mut self.view.review_detail {
                    detail.page_index = detail.page_index.saturating_sub(1);
                }
            }
            UiAction::Clicked { id } if id == "review_detail_next" => {
                if let Some(detail) = &mut self.view.review_detail {
                    if let Some(Ok(Some(diff))) = &detail.content {
                        if (detail.page_index + 1).saturating_mul(100) < diff.file.lines.len() {
                            detail.page_index += 1;
                        }
                    }
                }
            }
            UiAction::Clicked { id } if id == "file_preview_previous" => {
                if let Some(preview) = &mut self.view.file_preview {
                    preview.page_index = preview.page_index.saturating_sub(1);
                }
                self.request_file_preview();
            }
            UiAction::Clicked { id } if id == "file_preview_next" => {
                let can_advance = self.view.file_preview.as_ref().is_some_and(
                    |preview| matches!(&preview.content, Some(Ok(page)) if page.has_more),
                );
                if can_advance {
                    if let Some(preview) = &mut self.view.file_preview {
                        preview.page_index = preview.page_index.saturating_add(1);
                    }
                    self.request_file_preview();
                }
            }
            UiAction::Clicked { id } if id == "file_preview_retry" => {
                self.request_file_preview();
            }
            UiAction::Clicked { id } if id == "review_refresh" => {
                if let Some(workspace) = self.review_active_workspace.clone() {
                    self.request_git_review(workspace);
                }
            }
            UiAction::Clicked { id } if id == "github_account_connect" => {
                self.view.github_account_menu_open = false;
                self.view.github_account = if self
                    .github_account_worker
                    .as_ref()
                    .is_some_and(GithubAccountWorker::connect)
                {
                    GithubAccountStatus::Connecting
                } else {
                    GithubAccountStatus::Unavailable(
                        "No se pudo iniciar la conexión con GitHub".to_owned(),
                    )
                };
            }
            UiAction::Clicked { id } if id == "github_account_menu_toggle" => {
                self.view.github_account_menu_open = !self.view.github_account_menu_open;
            }
            UiAction::Clicked { id } if id == "github_account_menu_dismiss" => {
                self.view.github_account_menu_open = false;
            }
            UiAction::Clicked { id } if id.starts_with("github_account_verify:") => {
                let account = id.trim_start_matches("github_account_verify:");
                let listed = matches!(
                    &self.view.github_account,
                    GithubAccountStatus::Unverified { accounts, .. }
                        if accounts.iter().any(|known| known.eq_ignore_ascii_case(account))
                );
                if listed {
                    self.view.github_account_menu_open = false;
                    self.view.github_account = if self
                        .github_account_worker
                        .as_ref()
                        .is_some_and(|worker| worker.verify(account))
                    {
                        GithubAccountStatus::Verifying(account.to_owned())
                    } else {
                        GithubAccountStatus::Unavailable(
                            "No se pudo verificar la cuenta de GitHub".to_owned(),
                        )
                    };
                }
            }
            UiAction::Clicked { id } if id == "github_account_open" => {
                if let GithubAccountStatus::Connected(identity) = &self.view.github_account {
                    ctx.open_url(egui::OpenUrl::new_tab(format!(
                        "https://github.com/{}",
                        identity.login
                    )));
                }
            }
            UiAction::Clicked { id } if id == "github_repo_open" => {
                if let GithubStatus::Ready(github) = &self.github_status {
                    let url = github.repository.web_url();
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
            }
            UiAction::Clicked { id } if id.starts_with("github_pr_open:") => {
                let number = id
                    .strip_prefix("github_pr_open:")
                    .and_then(|number| number.parse::<u64>().ok());
                if let Some(url) = number.and_then(|number| {
                    let GithubStatus::Ready(github) = &self.github_status else {
                        return None;
                    };
                    github
                        .pulls
                        .iter()
                        .find(|pull| pull.number == number)
                        .map(|pull| pull.url.clone())
                }) {
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
            }
            UiAction::Clicked { id } if id.starts_with("file_row:") => {
                if let Ok(index) = id.trim_start_matches("file_row:").parse::<usize>() {
                    let row = self
                        .file_explorer
                        .as_ref()
                        .and_then(|explorer| explorer.rows().get(index))
                        .cloned();
                    if let Some(row) = row {
                        if row.is_dir {
                            if let Some(explorer) = &mut self.file_explorer {
                                if let Err(error) = explorer.toggle(&row.relative) {
                                    self.view.files_status =
                                        format!("No se pudo abrir carpeta: {error}");
                                }
                            }
                            self.refresh_file_rows();
                        } else {
                            let workspace = Path::new(&self.view.workspace);
                            let owner = self.view.selected_session.as_deref().unwrap_or("");
                            if self
                                .dock
                                .open_file(workspace, owner, &row.relative)
                                .is_some()
                            {
                                self.view.show_dock_overview = false;
                                self.sync_dock_view();
                            }
                        }
                    }
                }
            }
            UiAction::Clicked { id } if id.starts_with("dock_open:") => {
                let kind = id.trim_start_matches("dock_open:");
                self.view.show_dock_add_menu = false;
                if kind == "files" {
                    self.open_files_dock();
                } else {
                    let scope = self.dock_scope();
                    let workspace_scope =
                        DockScope::new(scope.workspace_id.clone(), DockOwner::Workspace);
                    let tab = match kind {
                        "context" => Some(self.dock.open(&workspace_scope, DockTabKind::Context)),
                        "activity" => Some(self.dock.open(&scope, DockTabKind::Activity)),
                        "review" => Some(self.dock.open(&workspace_scope, DockTabKind::CodeReview)),
                        "terminal" => Some(self.dock.open(&scope, DockTabKind::Terminal)),
                        _ => None,
                    };
                    if let Some(tab) = tab {
                        self.dock.activate(&scope, tab);
                        self.dock.set_collapsed(&scope, false);
                        self.sync_dock_view();
                    }
                }
                self.view.show_dock_overview = false;
            }
            UiAction::Clicked { id }
                if matches!(id.as_str(), "dock_tab_page_previous" | "dock_tab_page_next") =>
            {
                let width = self.view.right_dock_width.unwrap_or(320.0);
                let page_size = crate::view::dock_tab_page_size(width);
                let active_index = self
                    .view
                    .dock_tabs
                    .iter()
                    .position(|tab| tab.active)
                    .unwrap_or(0);
                let page = active_index / page_size;
                let target_page = if id == "dock_tab_page_previous" {
                    page.checked_sub(1)
                } else {
                    Some(page + 1)
                };
                let target = target_page
                    .and_then(|page| self.view.dock_tabs.get(page * page_size))
                    .and_then(|tab| DockTabId::from_raw(tab.id));
                if let Some(target) = target {
                    let scope = self.dock_scope();
                    if self.dock.activate(&scope, target) {
                        if self
                            .dock
                            .active_tab(&scope)
                            .is_some_and(|tab| tab.kind == DockTabKind::Files)
                        {
                            self.refresh_file_rows();
                        }
                        self.sync_dock_view();
                    }
                }
            }
            UiAction::Clicked { id } if id.starts_with("dock_tab:") => {
                self.view.show_dock_add_menu = false;
                if let Ok(raw) = id.trim_start_matches("dock_tab:").parse::<u64>() {
                    if let Some(tab_id) = DockTabId::from_raw(raw) {
                        let scope = self.dock_scope();
                        if self.dock.activate(&scope, tab_id) {
                            if self
                                .dock
                                .active_tab(&scope)
                                .is_some_and(|tab| tab.kind == DockTabKind::Files)
                            {
                                self.refresh_file_rows();
                            }
                            self.view.show_dock_overview = false;
                            self.sync_dock_view();
                        }
                    }
                }
            }
            UiAction::Clicked { id } if id == "dock_close_all" => {
                self.view.show_dock_add_menu = false;
                let scope = self.dock_scope();
                self.dock.close_all_visible(&scope);
                self.view.show_dock_overview = false;
                self.sync_dock_view();
            }
            UiAction::Clicked { id } if id.starts_with("dock_close_others:") => {
                if let Ok(raw) = id.trim_start_matches("dock_close_others:").parse::<u64>() {
                    if let Some(tab_id) = DockTabId::from_raw(raw) {
                        let scope = self.dock_scope();
                        self.dock.close_others(&scope, tab_id);
                        self.view.show_dock_overview = false;
                        self.sync_dock_view();
                    }
                }
            }
            UiAction::Clicked { id } if id.starts_with("dock_move_up:") => {
                if let Ok(raw) = id.trim_start_matches("dock_move_up:").parse::<u64>() {
                    if let Some(tab_id) = DockTabId::from_raw(raw) {
                        let scope = self.dock_scope();
                        let ids: Vec<DockTabId> = self
                            .dock
                            .visible_tabs(&scope)
                            .iter()
                            .map(|tab| tab.id)
                            .collect();
                        if let Some(index) = ids.iter().position(|id| *id == tab_id) {
                            if index > 0 && self.dock.reorder(&scope, tab_id, ids[index - 1]) {
                                self.sync_dock_view();
                            }
                        }
                    }
                }
            }
            UiAction::Clicked { id } if id.starts_with("dock_close:") => {
                if let Ok(raw) = id.trim_start_matches("dock_close:").parse::<u64>() {
                    if let Some(tab_id) = DockTabId::from_raw(raw) {
                        let scope = self.dock_scope();
                        if self.dock.close(&scope, tab_id).is_some() {
                            self.sync_dock_view();
                        }
                    }
                }
            }
            UiAction::Clicked { id } if id == "dock_overview_toggle" => {
                self.view.show_dock_overview = !self.view.show_dock_overview;
                self.view.show_dock_add_menu = false;
            }
            UiAction::Clicked { id } if id == "dock_add_tab" => {
                self.view.show_dock_add_menu = !self.view.show_dock_add_menu;
                self.view.show_dock_overview = false;
            }
            UiAction::Clicked { id } if id == "dock_add_dismiss" => {
                self.view.show_dock_add_menu = false;
            }
            UiAction::Clicked { id } if id == "terminal_send" => {
                let command = self.view.terminal_input.clone();
                self.send_terminal_command(&command);
            }
            UiAction::Clicked { id } if id == "terminal_interrupt" => {
                self.terminal_write(&[3]);
            }
            UiAction::Clicked { id } if id == "terminal_clear" => {
                self.terminal_write(&[12]);
            }
            UiAction::Clicked { id } if id == "terminal_scroll_up" => {
                self.terminal_scroll(8);
            }
            UiAction::Clicked { id } if id == "terminal_scroll_down" => {
                self.terminal_scroll(-8);
            }
            UiAction::Clicked { id } if id == "terminal_restart" => {
                if let Some(id) = self.active_terminal_id() {
                    self.terminals.remove(&id);
                    self.view.terminal_error = None;
                    self.ensure_terminal_session(id);
                }
            }
            UiAction::Clicked { id } if id == "toggle_archived_view" => {
                self.view.show_archived = !self.view.show_archived;
                self.view.session_sort_menu_open = false;
                self.view.group_menu_id = None;
                self.view.pending_delete_group_id = None;
                self.view.group_create_open = false;
                self.view.group_rename_id = None;
                self.close_session_menu();
                self.refresh_sessions();
            }
            UiAction::Clicked { id } if id == "session_organize:project" => {
                self.view.session_organization = SessionOrganization::Project;
                self.view.session_sort_menu_open = false;
                self.view.group_create_open = false;
                self.view.group_rename_id = None;
                self.view.group_menu_id = None;
                self.close_session_menu();
                self.refresh_sessions();
                self.persist_sidebar_preferences();
            }
            UiAction::Clicked { id } if id == "session_organize:group" => {
                self.view.session_organization = SessionOrganization::Group;
                self.view.session_sort_menu_open = false;
                self.close_session_menu();
                self.refresh_sessions();
                self.persist_sidebar_preferences();
            }
            UiAction::Clicked { id } if id == "session_organize:recent" => {
                self.view.session_organization = SessionOrganization::Recent;
                self.view.session_sort_menu_open = false;
                self.view.group_create_open = false;
                self.view.group_rename_id = None;
                self.view.group_menu_id = None;
                self.close_session_menu();
                self.refresh_sessions();
                self.persist_sidebar_preferences();
            }
            UiAction::Clicked { id } if id == "session_sort_toggle" => {
                self.close_session_menu();
                self.view.session_sort_menu_open = !self.view.session_sort_menu_open;
            }
            UiAction::Clicked { id } if id == "session_sort_dismiss" => {
                self.view.session_sort_menu_open = false;
            }
            UiAction::Clicked { id } if id.starts_with("session_sort:") => {
                let sort = match id.trim_start_matches("session_sort:") {
                    "updated" => Some(SessionSort::Updated),
                    "created" => Some(SessionSort::Created),
                    _ => None,
                };
                if let Some(sort) = sort {
                    self.view.session_sort = sort;
                    self.view.session_sort_menu_open = false;
                    self.refresh_sessions();
                    self.persist_sidebar_preferences();
                }
            }
            UiAction::Clicked { id } if id == "session_expand_all" => {
                self.view.session_sort_menu_open = false;
                if !self.view.show_archived
                    && self.view.session_organization == SessionOrganization::Group
                {
                    let previous = self.view.session_groups.clone();
                    let all_collapsed = !self.view.session_groups.groups.is_empty()
                        && self
                            .view
                            .session_groups
                            .groups
                            .iter()
                            .all(|group| group.collapsed);
                    if self.view.session_groups.collapse_all(!all_collapsed) {
                        self.commit_groups(previous);
                    }
                } else if !self.view.show_archived
                    && self.view.session_organization == SessionOrganization::Project
                {
                    let projects: BTreeSet<_> = self
                        .view
                        .sessions
                        .iter()
                        .filter(|session| !session.archived && !session.pinned)
                        .map(|session| session.project.clone())
                        .collect();
                    if !projects.is_empty() {
                        let all_collapsed = projects
                            .iter()
                            .all(|project| self.view.collapsed_projects.contains(project));
                        for project in projects {
                            if all_collapsed {
                                self.view.collapsed_projects.remove(&project);
                            } else {
                                self.view.collapsed_projects.insert(project);
                            }
                        }
                        self.persist_sidebar_preferences();
                    }
                }
            }
            UiAction::Clicked { id } if id.starts_with("session_project_toggle:") => {
                let project = id.trim_start_matches("session_project_toggle:");
                if !self.view.show_archived
                    && self.view.session_organization == SessionOrganization::Project
                    && self.view.sessions.iter().any(|session| {
                        !session.archived && !session.pinned && session.project == project
                    })
                {
                    if !self.view.collapsed_projects.remove(project) {
                        self.view.collapsed_projects.insert(project.to_owned());
                    }
                    self.persist_sidebar_preferences();
                }
            }
            UiAction::Clicked { id } if id == "session_group_new" => {
                if self.view.session_organization == SessionOrganization::Group
                    && !self.view.show_archived
                {
                    self.view.group_create_open = !self.view.group_create_open;
                    self.view.group_create_draft.clear();
                    self.group_focus_pending = self.view.group_create_open;
                    self.view.group_menu_id = None;
                }
            }
            UiAction::Clicked { id } if id == "session_group_create_cancel" => {
                self.view.group_create_open = false;
                self.view.group_create_draft.clear();
                self.group_focus_pending = false;
            }
            UiAction::Clicked { id } if id == "session_group_create_commit" => {
                self.commit_group_create();
            }
            UiAction::Clicked { id } if id.starts_with("session_group_toggle:") => {
                let group_id = id.trim_start_matches("session_group_toggle:");
                let previous = self.view.session_groups.clone();
                if self.view.session_organization == SessionOrganization::Group
                    && !self.view.show_archived
                    && self.view.session_groups.toggle_collapsed(group_id)
                {
                    self.commit_groups(previous);
                }
            }
            UiAction::Clicked { id } if id.starts_with("session_group_menu:") => {
                let group_id = id.trim_start_matches("session_group_menu:");
                let already_open = self.view.group_menu_id.as_deref() == Some(group_id);
                self.view.group_menu_id = (!already_open).then(|| group_id.to_owned());
                self.view.pending_delete_group_id = None;
                self.close_session_menu();
            }
            UiAction::Clicked { id } if id == "session_group_menu_dismiss" => {
                self.view.group_menu_id = None;
                self.view.pending_delete_group_id = None;
            }
            UiAction::Clicked { id } if id.starts_with("session_group_rename:") => {
                let group_id = id.trim_start_matches("session_group_rename:");
                if let Some(group) = self
                    .view
                    .session_groups
                    .groups
                    .iter()
                    .find(|group| group.id == group_id)
                {
                    self.view.group_rename_id = Some(group.id.clone());
                    self.view.group_rename_draft = group.title.clone();
                    self.view.group_menu_id = None;
                    self.group_focus_pending = true;
                }
            }
            UiAction::Clicked { id } if id == "session_group_rename_cancel" => {
                self.view.group_rename_id = None;
                self.view.group_rename_draft.clear();
                self.group_focus_pending = false;
            }
            UiAction::Clicked { id } if id.starts_with("session_group_rename_commit:") => {
                self.commit_group_rename(id.trim_start_matches("session_group_rename_commit:"));
            }
            UiAction::Clicked { id } if id.starts_with("session_group_delete:") => {
                let group_id = id.trim_start_matches("session_group_delete:");
                if self
                    .view
                    .session_groups
                    .groups
                    .iter()
                    .any(|group| group.id == group_id)
                {
                    self.view.pending_delete_group_id = Some(group_id.to_owned());
                    self.view.group_menu_id = Some(group_id.to_owned());
                }
            }
            UiAction::Clicked { id } if id.starts_with("session_group_delete_confirm:") => {
                let group_id = id.trim_start_matches("session_group_delete_confirm:");
                if self.view.pending_delete_group_id.as_deref() == Some(group_id) {
                    let previous = self.view.session_groups.clone();
                    if self.view.session_groups.delete(group_id) && self.commit_groups(previous) {
                        self.view.group_menu_id = None;
                        self.view.pending_delete_group_id = None;
                    }
                }
            }
            UiAction::Clicked { id }
                if id.starts_with("session_group_move_up:")
                    || id.starts_with("session_group_move_down:") =>
            {
                let up = id.starts_with("session_group_move_up:");
                let group_id = id.split_once(':').map(|(_, id)| id).unwrap_or_default();
                if let Some(index) = self
                    .view
                    .session_groups
                    .groups
                    .iter()
                    .position(|group| group.id == group_id)
                {
                    let target = if up {
                        index.saturating_sub(1)
                    } else {
                        index + 1
                    };
                    let previous = self.view.session_groups.clone();
                    if self.view.session_groups.move_group(group_id, target) {
                        self.commit_groups(previous);
                    }
                }
                self.view.group_menu_id = None;
            }
            UiAction::Clicked { id } if id.starts_with("session_group_assign:") => {
                let payload = id.trim_start_matches("session_group_assign:");
                if let Some((group_id, session_id)) = payload.split_once(':') {
                    if self
                        .view
                        .sessions
                        .iter()
                        .any(|session| session.id == session_id && !session.archived)
                    {
                        let previous = self.view.session_groups.clone();
                        if self.view.session_groups.assign(session_id, group_id) {
                            self.commit_groups(previous);
                        }
                    }
                }
                self.close_session_menu();
            }
            UiAction::Clicked { id } if id.starts_with("session_group_unassign:") => {
                let session_id = id.trim_start_matches("session_group_unassign:");
                let previous = self.view.session_groups.clone();
                if self.view.session_groups.unassign(session_id) {
                    self.commit_groups(previous);
                }
                self.close_session_menu();
            }
            UiAction::Clicked { id } if id.starts_with("session_menu:") => {
                let session_id = id.trim_start_matches("session_menu:");
                let already_open = self.view.session_menu_id.as_deref() == Some(session_id);
                self.close_session_menu();
                self.view.session_sort_menu_open = false;
                if !already_open {
                    self.view.session_menu_id = Some(session_id.to_owned());
                }
            }
            UiAction::Clicked { id } if id == "session_menu_dismiss" => {
                self.close_session_menu();
            }
            UiAction::Clicked { id } if id.starts_with("session_pin:") => {
                let session_id = id.trim_start_matches("session_pin:");
                if let (Some(store), Some(session)) = (
                    &self.store,
                    self.view
                        .sessions
                        .iter()
                        .find(|session| session.id == session_id),
                ) {
                    match store.set_pinned(session_id, !session.pinned) {
                        Ok(_) => {
                            self.close_session_menu();
                            self.refresh_sessions();
                        }
                        Err(error) => self.view.status = format!("No se pudo fijar: {error}"),
                    }
                }
            }
            UiAction::Clicked { id } if id.starts_with("session_mark_unread:") => {
                let session_id = id.trim_start_matches("session_mark_unread:");
                if let Some(store) = &self.store {
                    match store.mark_unread(session_id) {
                        Ok(_) => {
                            self.close_session_menu();
                            self.refresh_sessions();
                        }
                        Err(error) => {
                            self.view.status = format!("No se pudo marcar no leído: {error}")
                        }
                    }
                }
            }
            UiAction::Clicked { id } if id.starts_with("session_archive:") => {
                let session_id = id.trim_start_matches("session_archive:");
                if self.view.running && self.view.selected_session.as_deref() == Some(session_id) {
                    self.view.status = "Detén el agente antes de archivar esta tarea".to_owned();
                } else {
                    self.view.pending_archive_session_id = Some(session_id.to_owned());
                    self.view.renaming_session_id = None;
                    self.view.pending_delete_session_id = None;
                }
            }
            UiAction::Clicked { id } if id.starts_with("archive_confirm:") => {
                let session_id = id.trim_start_matches("archive_confirm:");
                if self.view.pending_archive_session_id.as_deref() != Some(session_id) {
                    return;
                }
                if self.view.running && self.view.selected_session.as_deref() == Some(session_id) {
                    self.view.status = "Detén el agente antes de archivar esta tarea".to_owned();
                } else if let Some(store) = &self.store {
                    match store.set_archived(session_id, true) {
                        Ok(_) => {
                            if self.view.selected_session.as_deref() == Some(session_id) {
                                self.new_session();
                                self.visit_navigation();
                            }
                            self.close_session_menu();
                            self.refresh_sessions();
                        }
                        Err(error) => self.view.status = format!("No se pudo archivar: {error}"),
                    }
                }
            }
            UiAction::Clicked { id } if id == "archive_cancel" => self.close_session_menu(),
            UiAction::Clicked { id } if id.starts_with("session_restore:") => {
                let session_id = id.trim_start_matches("session_restore:");
                if let Some(store) = &self.store {
                    match store.set_archived(session_id, false) {
                        Ok(_) => {
                            self.close_session_menu();
                            self.refresh_sessions();
                        }
                        Err(error) => self.view.status = format!("No se pudo restaurar: {error}"),
                    }
                }
            }
            UiAction::Clicked { id } if id.starts_with("session_rename:") => {
                let session_id = id.trim_start_matches("session_rename:");
                if let Some(session) = self.view.sessions.iter().find(|item| item.id == session_id)
                {
                    self.view.renaming_session_id = Some(session_id.to_owned());
                    self.view.pending_archive_session_id = None;
                    self.view.pending_delete_session_id = None;
                    self.view.rename_draft = session.title.clone();
                    self.tree
                        .set_input_value("rename_title", self.view.rename_draft.clone());
                }
            }
            UiAction::Clicked { id } if id.starts_with("rename_commit:") => {
                let session_id = id.trim_start_matches("rename_commit:");
                self.commit_session_rename(session_id);
            }
            UiAction::Clicked { id } if id == "rename_cancel" => {
                self.close_session_menu();
            }
            UiAction::Clicked { id } if id.starts_with("session_delete:") => {
                let session_id = id.trim_start_matches("session_delete:");
                if self.view.show_archived {
                    self.view.pending_delete_session_id = Some(session_id.to_owned());
                    self.view.renaming_session_id = None;
                    self.view.pending_archive_session_id = None;
                }
            }
            UiAction::Clicked { id } if id.starts_with("delete_confirm:") => {
                let session_id = id.trim_start_matches("delete_confirm:");
                if self.view.pending_delete_session_id.as_deref() == Some(session_id) {
                    if self.view.running
                        && self.view.selected_session.as_deref() == Some(session_id)
                    {
                        self.view.status =
                            "Detén el agente antes de eliminar esta tarea".to_owned();
                    } else if let Some(store) = &self.store {
                        match store.load(session_id) {
                            Ok(Some(record)) if record.session.archived => {
                                match store.delete(session_id) {
                                    Ok(_) => {
                                        self.close_session_menu();
                                        self.navigation
                                            .forget_task(&record.session.workspace, session_id);
                                        if self.view.selected_session.as_deref() == Some(session_id)
                                        {
                                            self.new_session();
                                            self.visit_navigation();
                                        }
                                        self.refresh_sessions();
                                        self.sync_navigation_controls();
                                    }
                                    Err(error) => {
                                        self.view.status = format!("No se pudo eliminar: {error}");
                                    }
                                }
                            }
                            Ok(None)
                                if self.view.sessions.iter().any(|session| {
                                    session.id == session_id && session.archived
                                }) =>
                            {
                                match store.delete(session_id) {
                                    Ok(_) => {
                                        self.close_session_menu();
                                        if let Some(session) = self
                                            .view
                                            .sessions
                                            .iter()
                                            .find(|session| session.id == session_id)
                                        {
                                            self.navigation.forget_task(
                                                Path::new(&session.project),
                                                session_id,
                                            );
                                        }
                                        self.refresh_sessions();
                                        self.sync_navigation_controls();
                                    }
                                    Err(error) => {
                                        self.view.status = format!("No se pudo eliminar: {error}");
                                    }
                                }
                            }
                            _ => self.view.status = "Sólo se eliminan tareas archivadas".to_owned(),
                        }
                    }
                }
            }
            UiAction::Clicked { id } if id == "delete_cancel" => {
                self.close_session_menu();
            }
            UiAction::Clicked { id }
                if matches!(id.as_str(), "toggle_settings" | "toggle_settings_header") =>
            {
                self.view.show_settings = !self.view.show_settings;
                if !self.view.show_settings {
                    self.clear_settings_notice();
                }
                self.visit_navigation();
            }
            UiAction::Clicked { id }
                if id == "home_workspace_settings" || id == "composer_project_toggle" =>
            {
                if !self.view.running {
                    self.refresh_composer_projects();
                    self.view.composer_project_menu_open = !self.view.composer_project_menu_open;
                    self.view.composer_add_menu_open = false;
                }
            }
            UiAction::Clicked { id } if id == "composer_project_dismiss" => {
                self.view.composer_project_menu_open = false;
            }
            UiAction::Clicked { id } if id == "composer_project_browse" => {
                self.open_workspace_folder_picker(ctx);
            }
            UiAction::Clicked { id } if id.starts_with("composer_project_select:") => {
                if !self.view.running {
                    if let Ok(index) = id
                        .trim_start_matches("composer_project_select:")
                        .parse::<usize>()
                    {
                        if let Some((path, _)) =
                            self.view.composer_project_options.get(index).cloned()
                        {
                            self.view.composer_project_menu_open = false;
                            if Path::new(&path).is_dir() {
                                self.change_workspace(path);
                                self.visit_navigation();
                            } else {
                                self.view.status = "La carpeta del proyecto no existe".to_owned();
                            }
                        }
                    }
                }
            }
            UiAction::Clicked { id } if id == "composer_project_configure" => {
                self.view.composer_project_menu_open = false;
                self.view.show_settings = true;
                self.view.settings_page = SettingsPage::General;
                self.clear_settings_notice();
                self.visit_navigation();
            }
            UiAction::Clicked { id } if id == "workspace_settings" => {
                self.view.show_settings = true;
                self.clear_settings_notice();
                self.visit_navigation();
            }
            UiAction::Clicked { id } if id == "nav_back" => self.go_back(),
            UiAction::Clicked { id } if id == "nav_forward" => self.go_forward(),
            UiAction::Clicked { id }
                if matches!(id.as_str(), "toggle_sidebar" | "toggle_sidebar_reopen") =>
            {
                self.toggle_left_sidebar();
            }
            UiAction::Clicked { id } if id.starts_with("settings_page:") => {
                if let Some(page) = SettingsPage::from_action_id(&id) {
                    self.view.settings_page = page;
                    self.view.settings_nav_open = false;
                    self.clear_settings_notice();
                    let mut next = self.settings_preferences.clone();
                    if next.set_last_settings_page(page.action_id()).is_ok() {
                        if let Err(error) = next.save(&self.data_root) {
                            self.set_settings_notice(error, true);
                        } else {
                            self.settings_preferences = next;
                        }
                    }
                }
            }
            UiAction::Clicked { id } if id == "settings_nav_toggle" => {
                self.view.settings_nav_open = !self.view.settings_nav_open;
            }
            UiAction::Clicked { id } if id == "settings_reduced_motion" => {
                let mut next = self.settings_preferences.clone();
                next.reduced_motion = !next.reduced_motion;
                let reduced = next.reduced_motion;
                if self.commit_settings_preferences(next, "Preferencia de movimiento guardada") {
                    self.reduced_motion = reduced;
                    self.view.settings_reduced_motion = reduced;
                    self.tree.set_reduced_motion(reduced);
                    self.command_tree.set_reduced_motion(reduced);
                }
            }
            UiAction::Clicked { id } if id == "settings_reopen_last_workspace" => {
                let mut next = self.settings_preferences.clone();
                next.reopen_last_workspace = !next.reopen_last_workspace;
                let selected = next.reopen_last_workspace;
                let result = if selected {
                    next.set_last_workspace(Path::new(&self.view.workspace))
                } else {
                    Ok(())
                };
                match result {
                    Ok(()) => {
                        if self
                            .commit_settings_preferences(next, "Preferencia de proyecto guardada")
                        {
                            self.view.settings_reopen_last_workspace = selected;
                        }
                    }
                    Err(error) => self.set_settings_notice(error, true),
                }
            }
            UiAction::Clicked { id } if id.starts_with("settings_terminal_shell:") => {
                let choice = match id.trim_start_matches("settings_terminal_shell:") {
                    "system" => Some(TerminalShell::System),
                    "powershell" => Some(TerminalShell::PowerShell),
                    "pwsh" => Some(TerminalShell::PowerShellCore),
                    "cmd" => Some(TerminalShell::CommandPrompt),
                    "bash" => Some(TerminalShell::Bash),
                    "zsh" => Some(TerminalShell::Zsh),
                    "fish" => Some(TerminalShell::Fish),
                    "custom" => Some(TerminalShell::Custom(PathBuf::from(
                        self.view.settings_terminal_shell_draft.trim(),
                    ))),
                    _ => None,
                };
                if let Some(choice) = choice {
                    let mut next = self.settings_preferences.clone();
                    match next.set_terminal_shell(choice.clone()) {
                        Ok(()) => {
                            if self.commit_settings_preferences(
                                next,
                                "Shell guardado. Se aplicará a las nuevas terminales.",
                            ) {
                                self.view.settings_terminal_shell = choice;
                            }
                        }
                        Err(error) => self.set_settings_notice(error, true),
                    }
                }
            }
            UiAction::Clicked { id } if id == "catalog_refresh" => {
                self.clear_settings_notice();
                self.begin_catalog_refresh();
            }
            UiAction::Clicked { id } if id == "catalog_select_custom" => {
                if !self.view.running {
                    let previous = self.view.native_provider_id.clone();
                    self.view.native_provider_id = "custom".to_owned();
                    if !self.persist_native_settings() {
                        self.view.native_provider_id = previous;
                        self.view.native_key_available = self.native_settings().key_available();
                    } else {
                        let message = "Conexión personalizada activa: revisa el endpoint y el ID del modelo antes de ejecutar";
                        self.view.status = message.to_owned();
                        self.set_settings_notice(message, false);
                    }
                }
            }
            UiAction::Clicked { id } if id.starts_with("catalog_browse:") => {
                let provider_id = id.trim_start_matches("catalog_browse:");
                if self
                    .view
                    .catalog
                    .as_ref()
                    .is_some_and(|catalog| catalog.providers.contains_key(provider_id))
                {
                    self.view.catalog_browse_provider = provider_id.to_owned();
                    self.view.catalog_model_query.clear();
                    self.tree.set_input_value("catalog_model_query", "");
                }
            }
            UiAction::Clicked { id } if id.starts_with("catalog_select:") => {
                if !self.view.running {
                    let selected = id.trim_start_matches("catalog_select:");
                    if let Some((provider_id, model_id)) = selected.split_once('/') {
                        let usable = native_provider::adapter(provider_id).is_some()
                            && self
                                .view
                                .catalog
                                .as_ref()
                                .and_then(|catalog| catalog.providers.get(provider_id))
                                .and_then(|provider| provider.models.get(model_id))
                                .is_some_and(|model| {
                                    settings_ui::catalog_model_usable(provider_id, model)
                                });
                        if usable {
                            let previous_provider = self.view.native_provider_id.clone();
                            let previous_model = self.view.model.clone();
                            self.view.native_provider_id = provider_id.to_owned();
                            self.view.model = model_id.to_owned();
                            if self.persist_native_settings() {
                                let message =
                                    format!("Modelo nativo seleccionado: {provider_id}/{model_id}");
                                self.view.status = message.clone();
                                self.set_settings_notice(message, false);
                            } else {
                                self.view.native_provider_id = previous_provider;
                                self.view.model = previous_model;
                                self.view.native_key_available =
                                    self.native_settings().key_available();
                            }
                        }
                    }
                }
            }
            UiAction::Clicked { id }
                if matches!(id.as_str(), "toggle_side_pane" | "toggle_side_pane_header") =>
            {
                self.view.show_side_pane = !self.view.show_side_pane;
                let scope = self.dock_scope();
                self.dock.set_collapsed(&scope, !self.view.show_side_pane);
                self.sync_dock_view();
            }
            UiAction::Clicked { id } if id == "jump_to_bottom" => {
                self.tree.scroll_to_end("transcript");
                self.view.show_jump_to_bottom = false;
            }
            UiAction::Clicked { id } if id == "side_tab:context" => {
                self.view.side_tab = SideTab::Context;
                let scope = self.dock_scope();
                let workspace_scope =
                    DockScope::new(scope.workspace_id.clone(), DockOwner::Workspace);
                let tab = self.dock.open(&workspace_scope, DockTabKind::Context);
                self.dock.activate(&scope, tab);
                self.sync_dock_view();
            }
            UiAction::Clicked { id } if id == "side_tab:activity" => {
                self.view.side_tab = SideTab::Activity;
                let scope = self.dock_scope();
                self.dock.open(&scope, DockTabKind::Activity);
                self.sync_dock_view();
            }
            UiAction::Clicked { id } if id.starts_with("tool:") => {
                let tool_id = id.trim_start_matches("tool:");
                self.view.selected_tool_id = if self.viewport_width < 800.0
                    && self.view.selected_tool_id.as_deref() == Some(tool_id)
                {
                    None
                } else {
                    Some(tool_id.to_owned())
                };
                self.view.show_side_pane = true;
                self.view.side_tab = SideTab::Activity;
                let scope = self.dock_scope();
                self.dock.open(&scope, DockTabKind::Activity);
                self.sync_dock_view();
            }
            UiAction::Clicked { id } if id.starts_with("agent_tool:") => {
                if let Some(tool) = AgentToolSettings::tool_from_action_id(&id) {
                    let mut next = self.agent_tool_settings;
                    let enabled = next.toggle(tool);
                    let message = format!(
                        "Herramienta {} {} para los próximos turnos",
                        tool.name(),
                        if enabled { "activada" } else { "desactivada" }
                    );
                    self.commit_agent_tool_settings(next, &message);
                }
            }
            UiAction::Clicked { id } if id == "write_mode" => {
                let enabled = !self.view.workspace_writes_enabled;
                let mut next = self.agent_tool_settings;
                next.set(openide_harness::LocalTool::WriteFile, enabled);
                next.set(openide_harness::LocalTool::RunCommand, enabled);
                let message = if enabled {
                    "Escritura y comandos activos; cada ejecución pedirá aprobación"
                } else {
                    "Escritura y comandos desactivados para el agente nativo"
                };
                self.commit_agent_tool_settings(next, message);
            }
            UiAction::Clicked { id } if id == "workspace_apply" => {
                let desired = self.view.workspace_draft.trim().to_owned();
                if self.view.running {
                    let message = "Detén el agente antes de cambiar de proyecto";
                    self.view.status = message.to_owned();
                    self.set_settings_notice(message, true);
                } else if Path::new(&desired).is_dir() {
                    self.clear_settings_notice();
                    self.change_workspace(desired);
                    if !self.view.settings_notice_error {
                        let message = "Proyecto actualizado";
                        self.view.status = message.to_owned();
                        self.set_settings_notice(message, false);
                    }
                    self.visit_navigation();
                } else {
                    let message = "La carpeta del proyecto no existe";
                    self.view.status = message.to_owned();
                    self.set_settings_notice(message, true);
                }
            }
            UiAction::Clicked { id } if id == "backend_next" => {
                if !self.view.running && !self.view.backend_options.is_empty() {
                    let current = self
                        .view
                        .backend_options
                        .iter()
                        .position(|(backend, _)| backend == &self.view.selected_backend)
                        .unwrap_or(0);
                    let next = (current + 1) % self.view.backend_options.len();
                    let backend = self.view.backend_options[next].0.clone();
                    self.new_session();
                    self.view.selected_backend = backend;
                    self.refresh_composer_selection();
                }
            }
            UiAction::Clicked { id } if id == "backend_menu_toggle" => {
                if !self.view.running {
                    self.view.backend_menu_open = !self.view.backend_menu_open;
                    self.view.composer_model_menu_open = false;
                    self.view.composer_effort_menu_open = false;
                }
            }
            UiAction::Clicked { id } if id == "backend_menu_dismiss" => {
                self.view.backend_menu_open = false;
            }
            UiAction::Clicked { id } if id.starts_with("backend:") => {
                self.view.backend_menu_open = false;
                let backend = id.trim_start_matches("backend:");
                if self.view.selected_backend != backend
                    && self
                        .view
                        .backend_options
                        .iter()
                        .any(|(candidate, _)| candidate == backend)
                {
                    self.new_session();
                    if !self.view.running {
                        self.view.selected_backend = backend.to_owned();
                        self.refresh_composer_selection();
                    }
                }
            }
            UiAction::Clicked { id } if id == "composer_model_toggle" => {
                if !self.view.running
                    && (!self.view.composer_model_options.is_empty()
                        || !self.view.composer_effort_options.is_empty())
                {
                    self.view.composer_model_menu_open = !(self.view.composer_model_menu_open
                        || self.view.composer_effort_menu_open);
                    self.view.composer_effort_menu_open = false;
                    self.view.backend_menu_open = false;
                }
            }
            UiAction::Clicked { id } if id == "composer_model_dismiss" => {
                self.view.composer_model_menu_open = false;
                self.view.composer_effort_menu_open = false;
            }
            UiAction::Clicked { id } if id.starts_with("composer_model:") => {
                self.select_composer_model(id.trim_start_matches("composer_model:"));
            }
            UiAction::Clicked { id } if id == "composer_effort_toggle" => {
                if !self.view.running
                    && (!self.view.composer_model_options.is_empty()
                        || !self.view.composer_effort_options.is_empty())
                {
                    self.view.composer_model_menu_open = !(self.view.composer_model_menu_open
                        || self.view.composer_effort_menu_open);
                    self.view.composer_effort_menu_open = false;
                    self.view.backend_menu_open = false;
                }
            }
            UiAction::Clicked { id } if id == "composer_effort_dismiss" => {
                self.view.composer_effort_menu_open = false;
                self.view.composer_model_menu_open = false;
            }
            UiAction::Clicked { id } if id.starts_with("composer_effort:") => {
                self.select_composer_effort(id.trim_start_matches("composer_effort:"));
            }
            UiAction::Clicked { id } if id.starts_with("session:") => {
                self.open_session(id.trim_start_matches("session:"));
                if !self.view.running {
                    self.view.show_settings = false;
                    self.visit_navigation();
                }
            }
            UiAction::InputChanged { id, value } => match id.as_str() {
                "command_query" => {
                    self.view.command_query = value;
                    self.view.command_selected_index = 0;
                    self.request_command_search();
                }
                "prompt" => {
                    self.view.prompt = value;
                    self.draft_dirty_at = Some(Instant::now());
                }
                "terminal_input" => self.view.terminal_input = value,
                "settings_terminal_shell_path" => self.view.settings_terminal_shell_draft = value,
                "workspace" => self.view.workspace_draft = value,
                "model" => {
                    self.view.model = value.clone();
                    if self.view.selected_backend == "native" {
                        self.view.composer_model = value;
                        self.view.composer_model_options =
                            composer_selection::model_options("native", &self.view.composer_model);
                        self.view.composer_effort_options =
                            composer_selection::effort_options("native", &self.view.composer_model);
                        self.view.composer_effort = self
                            .view
                            .composer_effort_options
                            .first()
                            .map(|(effort, _)| effort.clone())
                            .unwrap_or_else(|| "default".to_owned());
                        self.remember_composer_selection(false);
                    }
                    self.persist_native_settings();
                }
                "endpoint" => {
                    self.view.endpoint = value;
                    self.persist_native_settings();
                }
                "catalog_provider_query" => self.view.catalog_provider_query = value,
                "catalog_model_query" => self.view.catalog_model_query = value,
                "rename_title" => self.view.rename_draft = value,
                "group_create_draft" => self.view.group_create_draft = value,
                "group_rename_draft" => self.view.group_rename_draft = value,
                "automation_query" => self.view.automations.query = value,
                "automation_title" => self.view.automations.form.title = value,
                "automation_prompt" => self.view.automations.form.prompt = value,
                "automation_once_at" => self.view.automations.form.once_at = value,
                "automation_interval" => self.view.automations.form.interval_minutes = value,
                "automation_time" => self.view.automations.form.time = value,
                "marketplace_search" => self.view.marketplace.query = value,
                "marketplace_source_draft" => self.view.marketplace.source_draft = value,
                _ => {}
            },
            UiAction::Submitted { id, .. } if id == "prompt" => self.start_run(),
            UiAction::Submitted { id, value } if id == "terminal_input" => {
                self.send_terminal_command(&value);
            }
            UiAction::Submitted { id, value } if id == "group_create_draft" => {
                self.view.group_create_draft = value;
                self.commit_group_create();
            }
            UiAction::Submitted { id, value } if id == "group_rename_draft" => {
                self.view.group_rename_draft = value;
                if let Some(group_id) = self.view.group_rename_id.clone() {
                    self.commit_group_rename(&group_id);
                }
            }
            UiAction::Submitted { id, value } if id == "rename_title" => {
                self.view.rename_draft = value;
                if let Some(session_id) = self.view.renaming_session_id.clone() {
                    self.commit_session_rename(&session_id);
                }
            }
            UiAction::Submitted { id, .. } if id == "command_query" => {
                self.activate_command_result(self.view.command_selected_index);
            }
            _ => {}
        }
    }

    fn toggle_left_sidebar(&mut self) {
        let right_was_visible = self.nominal_dock_layout().right.is_some();
        self.view.sidebar_collapsed = !self.view.sidebar_collapsed;
        // Responsive hiding is not a request to open the right dock. If the
        // left toggle alone would reveal it, keep the center dock growing and
        // let the explicit right-dock action open it afterward.
        let right_appeared = !right_was_visible && self.nominal_dock_layout().right.is_some();
        if right_appeared {
            let scope = self.dock_scope();
            self.dock.set_collapsed(&scope, true);
            self.sync_dock_view();
        }
        self.dock_drag = None;
        self.persist_sidebar_preferences();
    }

    fn update_dock_motion(&mut self, viewport_width: f32, now: Instant) -> bool {
        // Compute destinations without presentation overrides. The same
        // dock_layout function is then used to lay out the animated frame.
        self.viewport_width = viewport_width;
        let nominal = self.nominal_dock_layout();
        let left_target = nominal.left;
        let right_target = nominal.right.unwrap_or(0.0);
        if self.dock_drag.is_some() {
            self.dock_motion = None;
            self.view.left_presentation_width = None;
            self.view.right_presentation_width = None;
            return false;
        }
        let motion = self
            .dock_motion
            .get_or_insert_with(|| DockMotion::new(left_target, right_target, viewport_width, now));
        let (left, right) = motion.update(
            left_target,
            right_target,
            viewport_width,
            now,
            self.reduced_motion,
        );
        self.view.left_presentation_width = left;
        self.view.right_presentation_width = right;
        motion.is_active(now)
    }

    fn handle_dock_drag(&mut self, event: &UiEvent, viewport_width: f32) -> bool {
        let layout = dock_layout(&self.view, viewport_width);
        match event {
            UiEvent::PointerDown(point) => {
                self.dock_drag = if layout
                    .left_split_x
                    .is_some_and(|x| (point.x - x).abs() <= 5.0)
                {
                    Some(DockDrag::Left)
                } else if layout
                    .right_split_x
                    .is_some_and(|x| (point.x - x).abs() <= 5.0)
                {
                    Some(DockDrag::Right)
                } else {
                    None
                };
                self.dock_drag.is_some()
            }
            UiEvent::PointerMove(point) => match self.dock_drag {
                Some(DockDrag::Left) => {
                    self.view.left_dock_width = Some(point.x - 1.5 * 4.0);
                    true
                }
                Some(DockDrag::Right) => {
                    // The shared workspace has a one-pixel center/right divider.
                    // Measure from its midpoint plus the four-pixel outer inset.
                    self.view.right_dock_width = Some(viewport_width - point.x - 4.5);
                    if let Some(width) = self.view.right_dock_width {
                        self.dock.set_width_px(&self.view.workspace, width);
                    }
                    true
                }
                None => false,
            },
            UiEvent::PointerUp(_) if self.dock_drag.take().is_some() => true,
            _ => false,
        }
    }
}

impl eframe::App for AgentsWindow {
    fn on_exit(&mut self) {
        // The CLI worker owns a Child behind an Arc. Dropping the UI receiver
        // alone does not terminate it, so stop the run before leaving the app.
        if let Some(run) = self.active_run.take() {
            run.cancel();
        }
        for run in self.background_automation_runs.drain(..) {
            run.run.cancel();
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        for (store_index, dispatch) in self.automation_queue.drain(..) {
            let _ = self.automation_stores[store_index].complete_run(
                &dispatch.run_id,
                Err("La aplicación se cerró antes de iniciar el agente".to_owned()),
                now_ms,
            );
        }
        self.save_draft();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_run();
        let aux_changed = self.poll_automations()
            | self.poll_marketplace()
            | self.poll_catalog()
            | self.poll_workspace_folder_picker();
        let ctx = ui.ctx().clone();
        let (
            command_shortcut,
            new_task_shortcut,
            sidebar_shortcut,
            effort_shortcut,
            back_shortcut,
            forward_shortcut,
            escape,
            up,
            down,
        ) = ctx.input(|input| {
            (
                input.modifiers.command && input.key_pressed(egui::Key::K),
                input.modifiers.command && input.key_pressed(egui::Key::N),
                input.modifiers.command && input.key_pressed(egui::Key::B),
                input.modifiers.command && input.modifiers.shift && input.key_pressed(egui::Key::M),
                input.modifiers.alt && input.key_pressed(egui::Key::ArrowLeft),
                input.modifiers.alt && input.key_pressed(egui::Key::ArrowRight),
                input.key_pressed(egui::Key::Escape),
                input.key_pressed(egui::Key::ArrowUp),
                input.key_pressed(egui::Key::ArrowDown),
            )
        });
        let terminal_focused = self.view.side_tab == SideTab::Terminal
            && self.tree.focused_id() == Some("terminal_screen");
        if !terminal_focused && command_shortcut {
            if self.view.show_command_center {
                self.view.show_command_center = false;
            } else {
                self.open_command_center();
            }
        } else if !terminal_focused && escape && self.view.show_command_center {
            self.view.show_command_center = false;
        } else if !terminal_focused && !self.view.show_command_center {
            if escape && self.view.group_create_open {
                self.view.group_create_open = false;
                self.view.group_create_draft.clear();
                self.group_focus_pending = false;
            } else if escape && self.view.group_rename_id.is_some() {
                self.view.group_rename_id = None;
                self.view.group_rename_draft.clear();
                self.group_focus_pending = false;
            } else if effort_shortcut
                && self.view.main_page == MainPage::Workspace
                && !self.view.show_settings
                && !self.view.running
                && (!self.view.composer_model_options.is_empty()
                    || !self.view.composer_effort_options.is_empty())
            {
                self.view.composer_model_menu_open =
                    !(self.view.composer_model_menu_open || self.view.composer_effort_menu_open);
                self.view.composer_effort_menu_open = false;
                self.view.backend_menu_open = false;
            } else if sidebar_shortcut {
                self.toggle_left_sidebar();
            } else if new_task_shortcut {
                self.new_session();
                if !self.view.running {
                    self.view.show_settings = false;
                    self.visit_navigation();
                }
            } else if back_shortcut {
                self.go_back();
            } else if forward_shortcut {
                self.go_forward();
            }
        }
        let font = egui::FontId::monospace(14.0);
        let metrics = ctx.fonts_mut(|fonts| FontMetrics {
            char_width: fonts.glyph_width(&font, 'M'),
            line_height: fonts.row_height(&font),
        });
        self.tree.set_metrics(metrics);
        let sans_font = egui::FontId::proportional(14.0);
        let sans_metrics = ctx.fonts_mut(|fonts| FontMetrics {
            char_width: "abcdefghijklmnopqrstuvwxyz"
                .chars()
                .map(|character| fonts.glyph_width(&sans_font, character))
                .sum::<f32>()
                / 26.0,
            line_height: fonts.row_height(&sans_font),
        });
        self.tree.set_sans_metrics(sans_metrics);
        self.command_tree.set_metrics(metrics);
        self.command_tree.set_sans_metrics(sans_metrics);
        install_text_measurer(&mut self.tree, &ctx);
        install_text_measurer(&mut self.command_tree, &ctx);
        let area = ui.max_rect();
        self.viewport_width = area.width();
        self.viewport_height = area.height();
        let mut dock_animating = self.update_dock_motion(area.width(), Instant::now());
        self.view.show_jump_to_bottom =
            !self.view.transcript.is_empty() && !self.tree.is_scroll_at_end("transcript", 24.0);
        self.tree.set_root(build_view(&self.view, area.width()));
        let mut frame = self.tree.layout(Size::new(area.width(), area.height()));
        let focus_was_pending = (self.prompt_focus_pending, self.group_focus_pending);
        self.focus_prompt_if_pending();
        self.focus_group_if_pending();
        let mut needs_relayout = aux_changed
            || focus_was_pending != (self.prompt_focus_pending, self.group_focus_pending);
        let modal_width = (area.width() - 28.0).clamp(0.0, 720.0);
        let modal_height = (area.height() - 28.0).clamp(0.0, 620.0);
        let modal = egui::Rect::from_min_size(
            egui::pos2(
                area.min.x + (area.width() - modal_width) / 2.0,
                area.min.y + ((area.height() - modal_height) * 0.18).min(100.0),
            ),
            egui::vec2(modal_width, modal_height),
        );
        let mut command_frame = None;
        if self.view.show_command_center {
            if down {
                self.view.command_selected_index = (self.view.command_selected_index + 1)
                    .min(self.view.command_results.len().saturating_sub(1));
            } else if up {
                self.view.command_selected_index =
                    self.view.command_selected_index.saturating_sub(1);
            }
            self.command_tree
                .set_root(build_command_center(&self.view, modal_width, modal_height));
            command_frame = Some(
                self.command_tree
                    .layout(Size::new(modal_width, modal_height)),
            );
            if self.command_focus_pending {
                needs_relayout |= self.command_tree.focus("command_query");
                self.command_focus_pending = false;
            }
            for event in render::events(&ctx, modal.min) {
                needs_relayout = true;
                if matches!(
                    &event,
                    UiEvent::Key {
                        key: openide_ui::Key::ArrowUp | openide_ui::Key::ArrowDown,
                        ..
                    }
                ) {
                    continue;
                }
                if matches!(&event, UiEvent::PointerDown(point) if point.x < 0.0 || point.y < 0.0 || point.x > modal_width || point.y > modal_height)
                {
                    self.view.show_command_center = false;
                    break;
                }
                for action in self.command_tree.handle_event(event) {
                    self.apply_action(action, &ctx);
                }
            }
        } else {
            for event in render::events(&ctx, area.min) {
                needs_relayout = true;
                if self.handle_dock_drag(&event, area.width()) {
                    continue;
                }
                if self.handle_terminal_event(&event) {
                    continue;
                }
                for action in self.tree.handle_event(event) {
                    self.apply_action(action, &ctx);
                }
            }
        }
        needs_relayout |= self.update_command_search(&ctx);
        needs_relayout |= self.update_git_review(&ctx);
        needs_relayout |= self.update_detail(&ctx);
        needs_relayout |= self.update_terminal();
        needs_relayout |= self.update_github_account(&ctx);
        if let Some(changed_at) = self.draft_dirty_at {
            let debounce = Duration::from_millis(350);
            if changed_at.elapsed() >= debounce {
                self.save_draft();
                self.show_persistence_error();
                needs_relayout = true;
            } else {
                ctx.request_repaint_after(debounce - changed_at.elapsed());
            }
        }
        // The first layout is already the current animated frame. Only rebuild
        // after input, focus, or an asynchronous result changed its contents.
        // This avoids measuring and cloning the full workspace twice on every
        // animation tick, which is costly for a large file tree or diff.
        if needs_relayout {
            dock_animating = self.update_dock_motion(area.width(), Instant::now());
        }
        frame = relayout_after_updates(
            &mut self.tree,
            frame,
            Size::new(area.width(), area.height()),
            needs_relayout,
            || build_view(&self.view, area.width()),
        );
        if needs_relayout {
            self.focus_prompt_if_pending();
            self.focus_group_if_pending();
        }
        render::paint(ui, &frame);
        if self.view.show_command_center {
            ui.painter()
                .rect_filled(area, 0.0, egui::Color32::from_black_alpha(180));
            if needs_relayout || command_frame.is_none() {
                self.command_tree.set_root(build_command_center(
                    &self.view,
                    modal_width,
                    modal_height,
                ));
                command_frame = Some(
                    self.command_tree
                        .layout(Size::new(modal_width, modal_height)),
                );
            }
            if let Some(frame) = &command_frame {
                render::paint_at(ui, frame, modal.min);
            }
        }
        let dock = dock_layout(&self.view, area.width());
        let pointer_x =
            ctx.input(|input| input.pointer.hover_pos().map(|point| point.x - area.min.x));
        let near_split = pointer_x.is_some_and(|x| {
            dock.left_split_x
                .is_some_and(|left| (x - left).abs() <= 5.0)
                || dock
                    .right_split_x
                    .is_some_and(|right| (x - right).abs() <= 5.0)
        });
        if near_split || self.dock_drag.is_some() {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if self.active_run.is_some() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
        if self.view.show_side_pane
            && self.view.side_tab == SideTab::Terminal
            && self
                .view
                .terminal_snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.running)
        {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        if !self.background_automation_runs.is_empty()
            || !self.automation_queue.is_empty()
            || self.marketplace_response.is_some()
            || self.catalog_response.is_some()
            || self.workspace_folder_response.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        ctx.request_repaint_after(Duration::from_secs(30));
        if dock_animating || self.tree.is_animating() || self.command_tree.is_animating() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::MessageRole;
    use openide_harness::{LocalTool, PermissionMode};
    use openide_ui::Key;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn terminal_keys_forward_control_and_navigation_sequences() {
        assert_eq!(
            terminal_key_bytes(Key::Enter, false, false),
            Some(&b"\r"[..])
        );
        assert_eq!(terminal_key_bytes(Key::C, false, true), Some(&b"\x03"[..]));
        assert_eq!(terminal_key_bytes(Key::D, false, true), Some(&b"\x04"[..]));
        assert_eq!(terminal_key_bytes(Key::K, false, true), Some(&b"\x0b"[..]));
        assert_eq!(
            terminal_key_bytes(Key::ArrowUp, false, false),
            Some(&b"\x1b[A"[..])
        );
        assert_eq!(terminal_key_bytes(Key::Tab, false, false), Some(&b"\t"[..]));
        assert_eq!(terminal_key_bytes(Key::Tab, true, false), None);
    }

    #[test]
    fn idle_frame_reuses_layout_until_state_changes() {
        let measures = Arc::new(AtomicUsize::new(0));
        let measured = Arc::clone(&measures);
        let mut tree = UiTree::new(Element::text("First"));
        tree.set_text_measurer(move |text, _, _| {
            measured.fetch_add(1, Ordering::Relaxed);
            text.len() as f32 * 8.0
        });
        let viewport = Size::new(200.0, 40.0);
        let first = tree.layout(viewport);
        let first_measure_count = measures.load(Ordering::Relaxed);
        assert!(first_measure_count > 0);

        let idle = relayout_after_updates(&mut tree, first.clone(), viewport, false, || {
            panic!("an idle frame must not rebuild the view")
        });
        assert_eq!(idle, first);
        assert_eq!(measures.load(Ordering::Relaxed), first_measure_count);

        let updated =
            relayout_after_updates(&mut tree, idle, viewport, true, || Element::text("Updated"));
        assert_ne!(updated, first);
        assert!(measures.load(Ordering::Relaxed) > first_measure_count);
    }

    fn clicked(id: impl Into<String>) -> UiAction {
        UiAction::Clicked { id: id.into() }
    }

    #[test]
    fn invalid_saved_tool_permissions_start_read_only_and_show_settings_error() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("agent-tool-settings.json"), "{broken").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        assert!(!app.view.native_tool_permissions.write_file);
        assert!(!app.view.native_tool_permissions.run_command);
        assert!(app.view.settings_notice_error);
        let ctx = egui::Context::default();
        app.apply_action(clicked("toggle_settings"), &ctx);
        assert!(app.view.show_settings);
        assert!(app.view.settings_notice_error);
        assert!(app
            .view
            .settings_notice
            .as_deref()
            .is_some_and(|message| message.contains("permisos del agente")));
    }

    #[test]
    fn composer_attachment_uses_workspace_path_and_rejects_escape() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(project.join("src")).unwrap();
        std::fs::write(project.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.path().join("private.txt"), "outside").unwrap();
        let file = composer_attachment_for_path(&project, Path::new("src/main.rs")).unwrap();
        assert_eq!(file.path, "src/main.rs");
        assert_eq!(file.label, "main.rs");
        assert!(composer_attachment_for_path(&project, Path::new("../private.txt")).is_err());
    }

    #[test]
    fn composer_attachment_picker_adds_and_removes_file_from_message() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("note.md"), "context").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.change_workspace(project.to_string_lossy().into_owned());
        let ctx = egui::Context::default();

        app.apply_action(clicked("composer_add_context"), &ctx);
        assert!(app.view.composer_add_menu_open);
        app.apply_action(clicked("composer_add_file"), &ctx);
        assert!(!app.view.composer_add_menu_open);
        assert!(app.command_center_attaches_file);
        assert_eq!(app.view.command_scope, Scope::Files);

        app.view.command_results = vec![command_center::ResultItem {
            target: Target::OpenFile(PathBuf::from("note.md")),
            section: Scope::Files,
            title: "note.md".into(),
            subtitle: String::new(),
        }];
        app.activate_command_result(0);
        assert_eq!(app.view.composer_attachments.len(), 1);
        assert_eq!(app.view.composer_attachments[0].path, "note.md");
        assert!(!app.view.show_command_center);
        app.apply_action(clicked("composer_attachment_remove:0"), &ctx);
        assert!(app.view.composer_attachments.is_empty());
    }

    #[test]
    fn composer_prompt_names_verified_relative_files_without_changing_visible_text() {
        let attachments = vec![ComposerAttachmentView {
            path: "src/main.rs".into(),
            label: "main.rs".into(),
        }];
        let prompt = composer_prompt_with_attachments("Mejora la UI", &attachments);
        assert!(prompt.starts_with("Mejora la UI\n\n"));
        assert!(prompt.contains("[\"src/main.rs\"]"));
        assert!(prompt.contains("Abre estos archivos"));
        assert_eq!(attachments[0].label, "main.rs");
    }

    #[test]
    fn composer_attachment_draft_survives_context_switch_and_failed_run() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        let file = ComposerAttachmentView {
            path: "src/main.rs".into(),
            label: "main.rs".into(),
        };
        app.view.composer_attachments.push(file.clone());
        app.save_draft();
        app.view.composer_attachments.clear();
        app.restore_draft();
        assert_eq!(app.view.composer_attachments, vec![file.clone()]);

        app.view.composer_attachments.clear();
        app.pending_prompt = Some("Revisa esto".into());
        app.pending_attachments = vec![file.clone()];
        app.run_has_output = false;
        app.restore_pending_prompt();
        assert_eq!(app.view.prompt, "Revisa esto");
        assert_eq!(app.view.composer_attachments, vec![file]);
    }

    #[test]
    fn composer_project_picker_switches_workspace_and_disambiguates_names() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("one").join("shared");
        let second = root.path().join("two").join("shared");
        let third = root.path().join("three").join("other");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::create_dir_all(&third).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.change_workspace(first.to_string_lossy().into_owned());
        let store = app.store.as_ref().unwrap();
        let archived = store.create(&second, "native").unwrap();
        store.set_archived(&archived.id, true).unwrap();
        store.create(&third, "native").unwrap();
        app.refresh_sessions();
        assert!(app
            .view
            .sessions
            .iter()
            .all(|session| Path::new(&session.project) != second));
        assert_eq!(app.view.composer_project_options.len(), 3);
        app.view.show_archived = true;
        app.refresh_sessions();
        assert!(app
            .view
            .sessions
            .iter()
            .all(|session| Path::new(&session.project) != third));
        assert_eq!(app.view.composer_project_options.len(), 3);
        let ctx = egui::Context::default();
        app.apply_action(clicked("home_workspace_settings"), &ctx);
        assert!(app.view.composer_project_menu_open);
        let second_index = app
            .view
            .composer_project_options
            .iter()
            .position(|(path, _)| Path::new(path) == second)
            .unwrap();
        assert_ne!(
            app.view.composer_project_options[0].1,
            app.view.composer_project_options[second_index].1
        );
        assert!(!app.view.show_settings);
        app.apply_action(
            clicked(format!("composer_project_select:{second_index}")),
            &ctx,
        );
        assert_eq!(Path::new(&app.view.workspace), second);
        assert!(!app.view.composer_project_menu_open);
    }

    #[test]
    fn browsed_folder_switches_workspace_and_remembers_it() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second folder");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let data_root = root.path().join("data");
        let mut app = AgentsWindow::with_data_root(data_root.clone());
        app.change_workspace(first.to_string_lossy().into_owned());
        app.view.composer_project_menu_open = true;

        let (sender, receiver) = mpsc::channel();
        app.workspace_folder_response = Some(receiver);
        sender.send(Ok(Some(second.clone()))).unwrap();
        assert!(app.poll_workspace_folder_picker());

        assert_eq!(Path::new(&app.view.workspace), second);
        assert!(!app.view.composer_project_menu_open);
        assert!(app.workspace_folder_response.is_none());
        assert!(app.view.can_go_back);
        assert_eq!(
            SettingsPreferences::load(&data_root).last_workspace,
            Some(second)
        );
    }

    #[test]
    fn browsed_folder_cancel_and_invalid_selection_preserve_workspace() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let file = root.path().join("file.txt");
        std::fs::write(&file, "not a folder").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.change_workspace(workspace.to_string_lossy().into_owned());

        app.apply_workspace_folder_selection(Ok(None));
        assert_eq!(Path::new(&app.view.workspace), workspace);
        app.apply_workspace_folder_selection(Ok(Some(file)));
        assert_eq!(Path::new(&app.view.workspace), workspace);
        assert_eq!(app.view.status, "Selecciona una carpeta local existente");

        app.apply_workspace_folder_selection(Ok(Some(root.path().join("missing"))));
        assert_eq!(Path::new(&app.view.workspace), workspace);
        assert_eq!(app.view.status, "Selecciona una carpeta local existente");
    }

    #[test]
    fn settings_reduced_motion_action_takes_effect_and_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let original = app.reduced_motion;
        let ctx = egui::Context::default();

        app.apply_action(clicked("settings_reduced_motion"), &ctx);
        assert_eq!(app.reduced_motion, !original);
        assert_eq!(app.view.settings_reduced_motion, !original);
        assert_eq!(
            SettingsPreferences::load(root.path()).reduced_motion,
            !original
        );

        let reopened = AgentsWindow::with_data_root(root.path().to_path_buf());
        assert_eq!(reopened.reduced_motion, !original);
        assert_eq!(reopened.view.settings_reduced_motion, !original);
    }

    #[test]
    fn settings_reopen_last_workspace_action_restores_selected_project() {
        let root = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();

        app.apply_action(
            UiAction::InputChanged {
                id: "workspace".into(),
                value: project.path().to_string_lossy().into_owned(),
            },
            &ctx,
        );
        app.apply_action(clicked("workspace_apply"), &ctx);
        assert_eq!(
            Path::new(&app.view.workspace).canonicalize().unwrap(),
            project.path().canonicalize().unwrap()
        );
        assert!(!app.view.settings_reopen_last_workspace);

        app.apply_action(clicked("settings_reopen_last_workspace"), &ctx);
        assert!(app.view.settings_reopen_last_workspace);
        let saved = SettingsPreferences::load(root.path());
        assert!(saved.reopen_last_workspace);
        assert_eq!(
            saved.startup_workspace().unwrap().canonicalize().unwrap(),
            project.path().canonicalize().unwrap()
        );

        let reopened = AgentsWindow::with_data_root(root.path().to_path_buf());
        assert_eq!(
            Path::new(&reopened.view.workspace).canonicalize().unwrap(),
            project.path().canonicalize().unwrap()
        );
        assert!(reopened.view.settings_reopen_last_workspace);
    }

    #[test]
    fn settings_terminal_shell_action_persists_valid_choice_and_rejects_missing_file() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        let executable = std::env::current_exe().unwrap();

        app.apply_action(
            UiAction::InputChanged {
                id: "settings_terminal_shell_path".into(),
                value: executable.to_string_lossy().into_owned(),
            },
            &ctx,
        );
        app.apply_action(clicked("settings_terminal_shell:custom"), &ctx);
        assert_eq!(
            app.view.settings_terminal_shell,
            TerminalShell::Custom(executable.clone())
        );
        let saved = SettingsPreferences::load(root.path());
        assert_eq!(
            saved.terminal_shell,
            TerminalShell::Custom(executable.clone())
        );
        let (program, args) = saved.terminal_command().unwrap().unwrap();
        assert_eq!(PathBuf::from(program), executable.canonicalize().unwrap());
        assert!(args.is_empty());

        app.view.settings_terminal_shell_draft = root
            .path()
            .join("missing-shell.exe")
            .to_string_lossy()
            .into_owned();
        app.apply_action(clicked("settings_terminal_shell:custom"), &ctx);
        assert!(app.view.settings_notice_error);
        assert_eq!(
            SettingsPreferences::load(root.path()).terminal_shell,
            TerminalShell::Custom(executable.clone())
        );
        let reopened = AgentsWindow::with_data_root(root.path().to_path_buf());
        assert_eq!(
            reopened.view.settings_terminal_shell,
            TerminalShell::Custom(executable)
        );
    }

    #[test]
    fn settings_tool_actions_keep_native_permissions_independent_and_persistent() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        assert!(app.agent_tool_settings.enabled(LocalTool::ReadFile));
        assert!(!app.agent_tool_settings.enabled(LocalTool::WriteFile));
        assert!(!app.agent_tool_settings.enabled(LocalTool::RunCommand));

        app.apply_action(clicked("agent_tool:write_file"), &ctx);
        assert!(app.view.native_tool_permissions.write_file);
        assert!(!app.view.native_tool_permissions.run_command);
        assert!(!app.view.workspace_writes_enabled);
        assert_eq!(
            app.agent_tool_settings.mode_for(LocalTool::WriteFile),
            PermissionMode::Ask
        );
        app.apply_action(clicked("agent_tool:read_file"), &ctx);
        assert!(!app.view.native_tool_permissions.read_file);
        assert_eq!(
            app.agent_tool_settings.mode_for(LocalTool::ReadFile),
            PermissionMode::Deny
        );
        app.apply_action(clicked("agent_tool:run_command"), &ctx);
        assert!(app.view.native_tool_permissions.run_command);
        assert!(app.view.workspace_writes_enabled);
        assert_eq!(
            app.agent_tool_settings.mode_for(LocalTool::RunCommand),
            PermissionMode::Ask
        );
        assert_eq!(
            AgentToolSettings::load(root.path()).unwrap(),
            app.agent_tool_settings
        );

        let reopened = AgentsWindow::with_data_root(root.path().to_path_buf());
        assert_eq!(
            reopened.view.native_tool_permissions,
            app.view.native_tool_permissions
        );
        assert!(reopened.view.workspace_writes_enabled);
    }

    #[test]
    fn settings_page_action_remembers_last_section_and_ignores_unknown_route() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();

        app.apply_action(clicked(SettingsPage::Mcp.action_id()), &ctx);
        assert_eq!(app.view.settings_page, SettingsPage::Mcp);
        assert_eq!(
            SettingsPreferences::load(root.path()).last_settings_page,
            SettingsPage::Mcp.action_id()
        );
        app.apply_action(clicked("settings_page:unknown"), &ctx);
        assert_eq!(app.view.settings_page, SettingsPage::Mcp);
        let reopened = AgentsWindow::with_data_root(root.path().to_path_buf());
        assert_eq!(reopened.view.settings_page, SettingsPage::Mcp);
    }

    #[test]
    fn composer_model_and_effort_actions_are_validated_and_persisted() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        app.view.selected_backend = "codex".to_owned();
        app.refresh_composer_selection();

        app.apply_action(clicked("composer_model_toggle"), &ctx);
        assert!(app.view.composer_model_menu_open);
        app.apply_action(clicked("composer_model:gpt-6-sol"), &ctx);
        assert_eq!(app.view.composer_model, "gpt-6-sol");
        assert!(!app.view.composer_model_menu_open);

        app.apply_action(clicked("composer_effort:ultra"), &ctx);
        assert_eq!(app.view.composer_effort, "ultra");
        app.apply_action(clicked("composer_effort:invalid"), &ctx);
        assert_eq!(app.view.composer_effort, "ultra");

        let stored = ComposerPreferences::load(root.path());
        assert_eq!(
            stored.choice("codex", ""),
            ComposerChoice {
                model: "gpt-6-sol".into(),
                effort: "ultra".into(),
            }
        );

        app.apply_action(clicked("composer_model:gpt-6-luna"), &ctx);
        assert_eq!(app.view.composer_model, "gpt-6-luna");
        assert_eq!(app.view.composer_effort, "default");
    }

    #[test]
    fn automation_editor_actions_persist_and_pause_a_real_workspace_task() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.refresh_automation_view();
        let ctx = egui::Context::default();

        app.apply_action(clicked("open_automations"), &ctx);
        app.apply_action(clicked("automation_new"), &ctx);
        app.apply_action(
            UiAction::InputChanged {
                id: "automation_title".into(),
                value: "Resumen diario".into(),
            },
            &ctx,
        );
        app.apply_action(
            UiAction::InputChanged {
                id: "automation_prompt".into(),
                value: "Resume los cambios del proyecto".into(),
            },
            &ctx,
        );
        app.apply_action(clicked("automation_save"), &ctx);

        assert_eq!(app.view.main_page, MainPage::Automations);
        assert_eq!(app.view.automations.items.len(), 1);
        assert_eq!(app.view.automations.screen, AutomationScreen::List);
        let id = app.view.automations.items[0].id.clone();
        app.apply_action(clicked(format!("automation_toggle:{id}")), &ctx);
        assert!(!app.view.automations.items[0].enabled);
        let reopened = AutomationStore::open(&app.data_root, &workspace).unwrap();
        assert_eq!(reopened.automations()[0].title, "Resumen diario");
        assert!(!reopened.automations()[0].enabled);
    }

    #[test]
    fn closed_right_dock_tabs_stay_empty_across_tasks_until_reopened() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        app.viewport_width = 1400.0;
        let scope = app.dock_scope();
        app.dock.set_collapsed(&scope, false);
        app.sync_dock_view();
        assert!(!app.view.dock_tabs.is_empty());

        app.apply_action(clicked("dock_close_all"), &ctx);
        assert!(app.view.dock_tabs.is_empty());
        assert!(app.view.show_side_pane);
        app.view.selected_session = Some("task-two".to_owned());
        app.ensure_dock_tabs();
        assert!(app.view.dock_tabs.is_empty());

        app.apply_action(clicked("dock_open:context"), &ctx);
        assert_eq!(app.view.dock_tabs.len(), 1);
        assert_eq!(app.view.side_tab, SideTab::Context);
    }

    #[test]
    fn add_tab_button_toggles_menu_and_selection_reuses_a_tab() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        let original_count = app.view.dock_tabs.len();
        app.apply_action(clicked("dock_add_tab"), &ctx);
        assert!(app.view.show_dock_add_menu);
        assert!(!app.view.show_dock_overview);
        app.apply_action(clicked("dock_add_dismiss"), &ctx);
        assert!(!app.view.show_dock_add_menu);

        app.apply_action(clicked("dock_add_tab"), &ctx);
        app.apply_action(clicked("dock_open:context"), &ctx);
        assert!(!app.view.show_dock_add_menu);
        assert_eq!(app.view.dock_tabs.len(), original_count);
        assert_eq!(app.view.side_tab, SideTab::Context);

        app.apply_action(clicked("dock_add_tab"), &ctx);
        app.apply_action(clicked("dock_overview_toggle"), &ctx);
        assert!(!app.view.show_dock_add_menu);
        assert!(app.view.show_dock_overview);
    }

    #[test]
    fn right_dock_page_actions_navigate_real_tabs() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        app.viewport_width = 1400.0;
        let scope = app.dock_scope();
        app.dock.set_width_px(&scope.workspace_id, 500.0);
        let first = DockTabId::from_raw(app.view.dock_tabs[0].id).unwrap();
        app.dock.activate(&scope, first);
        app.sync_dock_view();
        assert_eq!(
            app.view.dock_tabs.iter().position(|tab| tab.active),
            Some(0)
        );
        app.apply_action(clicked("dock_tab_page_next"), &ctx);
        assert_eq!(
            app.view.dock_tabs.iter().position(|tab| tab.active),
            Some(2)
        );
        app.apply_action(clicked("dock_tab_page_previous"), &ctx);
        assert_eq!(
            app.view.dock_tabs.iter().position(|tab| tab.active),
            Some(0)
        );
    }

    fn sample_session(id: &str, project: &str) -> SessionView {
        SessionView {
            id: id.into(),
            title: id.into(),
            project: project.into(),
            backend: "native".into(),
            pinned: false,
            archived: false,
            unread: false,
        }
    }

    #[test]
    fn command_center_opens_files_in_right_dock_while_left_remains_sessions() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("README.md"), "# Project").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.viewport_width = 1200.0;
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.view.workspace_draft = app.view.workspace.clone();
        app.view.show_settings = true;
        app.visit_navigation();
        app.view.command_results = vec![command_center::ResultItem {
            target: Target::OpenFiles,
            section: Scope::Commands,
            title: "Archivos".to_owned(),
            subtitle: String::new(),
        }];
        app.activate_command_result(0);
        assert!(!app.view.show_settings);
        assert!(matches!(
            app.navigation.current(),
            Some(NavigationTarget::Draft { .. })
        ));
        assert_eq!(app.view.side_tab, SideTab::Files);
        assert!(app.view.show_side_pane);
        assert!(app.view.file_rows.iter().any(|row| row.name == "README.md"));
        let frame = UiTree::new(build_view(&app.view, 1200.0)).layout(Size::new(1200.0, 720.0));
        assert!(frame.regions.iter().any(|region| region.id == "file_row:0"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "sidebar_files"));
        app.view.show_settings = true;
        app.view.command_results[0].target = Target::OpenCodeReview;
        app.activate_command_result(0);
        assert!(!app.view.show_settings);
        assert_eq!(app.view.side_tab, SideTab::CodeReview);
        app.view.show_settings = true;
        app.view.command_results[0].target = Target::OpenFile(PathBuf::from("README.md"));
        app.activate_command_result(0);
        assert!(!app.view.show_settings);
        assert_eq!(app.view.side_tab, SideTab::FilePreview);
        app.viewport_width = 800.0;
        app.view.sidebar_collapsed = false;
        app.open_files_dock();
        assert!(app.view.sidebar_collapsed);
        assert!(dock_layout(&app.view, 800.0).right.is_some());
        app.view.sidebar_collapsed = false;
        app.view.right_presentation_width = Some(80.0);
        app.open_files_dock();
        assert!(app.view.sidebar_collapsed);
        assert!(app.nominal_dock_layout().right.is_some());
    }

    #[test]
    fn changing_workspace_discards_file_candidates_from_previous_project() {
        let root = tempfile::tempdir().unwrap();
        let previous = root.path().join("previous");
        let next = root.path().join("next");
        std::fs::create_dir(&previous).unwrap();
        std::fs::create_dir(&next).unwrap();
        std::fs::write(previous.join("old.rs"), "old").unwrap();
        std::fs::write(next.join("new.rs"), "new").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.change_workspace(previous.to_string_lossy().into_owned());
        app.refresh_file_rows();
        app.view.command_scope = Scope::Files;
        app.view.command_query = "old".to_owned();
        app.refresh_command_results();
        assert!(app
            .view
            .command_results
            .iter()
            .any(|result| result.target == Target::OpenFile(PathBuf::from("old.rs"))));
        app.change_workspace(next.to_string_lossy().into_owned());
        app.refresh_command_results();
        assert!(!app
            .view
            .command_results
            .iter()
            .any(|result| result.target == Target::OpenFile(PathBuf::from("old.rs"))));
        app.open_files_dock();
        assert!(app.view.file_rows.iter().any(|row| row.name == "new.rs"));
    }

    #[test]
    fn session_sort_respects_activity_or_creation_without_losing_project_identity() {
        let source = || {
            vec![
                (100, 10, sample_session("alpha-old", "C:/work/alpha")),
                (80, 20, sample_session("beta", "D:/work/beta")),
                (90, 30, sample_session("alpha-new", "E:/work/alpha")),
            ]
        };
        let mut by_activity = source();
        sort_session_views(
            &mut by_activity,
            SessionOrganization::Recent,
            SessionSort::Updated,
            false,
        );
        assert_eq!(
            by_activity
                .iter()
                .map(|entry| entry.2.id.as_str())
                .collect::<Vec<_>>(),
            ["alpha-old", "alpha-new", "beta"]
        );
        let mut by_creation = source();
        sort_session_views(
            &mut by_creation,
            SessionOrganization::Project,
            SessionSort::Created,
            false,
        );
        assert_eq!(
            by_creation
                .iter()
                .map(|entry| entry.2.id.as_str())
                .collect::<Vec<_>>(),
            ["alpha-new", "beta", "alpha-old"]
        );
    }

    #[test]
    fn sidebar_preferences_validate_and_survive_restart() {
        let root = tempfile::tempdir().unwrap();
        let preferences = SidebarPreferences {
            organization: SessionOrganization::Recent,
            sort: SessionSort::Created,
            collapsed_projects: ["C:/work/alpha".to_owned(), "D:/work/alpha".to_owned()]
                .into_iter()
                .collect(),
            sidebar_collapsed: true,
        };
        preferences.save(root.path()).unwrap();
        assert_eq!(SidebarPreferences::load(root.path()), preferences);
        std::fs::write(
            root.path().join("sidebar-preferences.json"),
            br#"{"organization":"unknown","sort":"created","collapsed_projects":["C:/work/alpha",42,"", "bad\npath"]}"#,
        )
        .unwrap();
        let loaded = SidebarPreferences::load(root.path());
        assert_eq!(loaded.organization, SessionOrganization::Project);
        assert_eq!(loaded.sort, SessionSort::Created);
        assert!(!loaded.sidebar_collapsed);
        assert_eq!(
            loaded.collapsed_projects,
            ["C:/work/alpha".to_owned()].into()
        );
    }

    #[test]
    fn session_organization_actions_sort_and_collapse_real_projects() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let store = app.store.as_ref().unwrap();
        let one = store.create("C:/work/alpha", "native").unwrap();
        let two = store.create("D:/work/beta", "native").unwrap();
        app.refresh_sessions();
        let ctx = egui::Context::default();
        app.apply_action(clicked("session_project_toggle:C:/work/alpha"), &ctx);
        assert!(app.view.collapsed_projects.contains("C:/work/alpha"));
        assert!(!app.view.collapsed_projects.contains("D:/work/beta"));
        app.apply_action(clicked("session_expand_all"), &ctx);
        assert_eq!(app.view.collapsed_projects.len(), 2);
        app.apply_action(clicked("session_expand_all"), &ctx);
        assert!(app.view.collapsed_projects.is_empty());
        app.apply_action(clicked("session_sort:created"), &ctx);
        app.apply_action(clicked("session_organize:recent"), &ctx);
        assert_eq!(app.view.session_sort, SessionSort::Created);
        assert_eq!(app.view.session_organization, SessionOrganization::Recent);
        assert!(app.view.sessions.iter().any(|session| session.id == one.id));
        assert!(app.view.sessions.iter().any(|session| session.id == two.id));
        let loaded = SidebarPreferences::load(root.path());
        assert_eq!(loaded.sort, SessionSort::Created);
        assert_eq!(loaded.organization, SessionOrganization::Recent);
    }

    #[test]
    fn left_toggle_does_not_auto_open_responsive_right_dock() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let session = app
            .store
            .as_ref()
            .unwrap()
            .create("C:/work/alpha", "native")
            .unwrap();
        app.view.selected_session = Some(session.id);
        app.viewport_width = 1000.0;
        app.ensure_dock_tabs();
        let scope = app.dock_scope();
        app.dock.set_collapsed(&scope, false);
        app.sync_dock_view();
        let before = dock_layout(&app.view, app.viewport_width);
        assert!(app.view.show_side_pane);
        assert!(before.right.is_none(), "the right dock is hidden by width");

        let ctx = egui::Context::default();
        app.apply_action(clicked("toggle_sidebar"), &ctx);
        let after = dock_layout(&app.view, app.viewport_width);
        assert!(app.view.sidebar_collapsed);
        assert!(!app.view.show_side_pane);
        assert!(after.right.is_none());
        assert!(after.center > before.center);
        let frame = UiTree::new(build_view(&app.view, app.viewport_width))
            .layout(Size::new(app.viewport_width, 700.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "toggle_side_pane_header"));

        app.apply_action(clicked("toggle_side_pane_header"), &ctx);
        assert!(app.view.show_side_pane);
        assert!(dock_layout(&app.view, app.viewport_width).right.is_some());
    }

    #[test]
    fn sidebar_toggle_and_manual_group_actions_persist_without_changing_sessions() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let session = app
            .store
            .as_ref()
            .unwrap()
            .create("C:/work/alpha", "native")
            .unwrap();
        app.refresh_sessions();
        let ctx = egui::Context::default();
        app.apply_action(clicked("toggle_sidebar"), &ctx);
        assert!(app.view.sidebar_collapsed);
        assert!(SidebarPreferences::load(root.path()).sidebar_collapsed);
        app.apply_action(clicked("toggle_sidebar_reopen"), &ctx);
        assert!(!app.view.sidebar_collapsed);
        app.apply_action(clicked("session_organize:group"), &ctx);
        app.apply_action(clicked("session_group_new"), &ctx);
        assert!(app.view.group_create_open);
        app.apply_action(
            UiAction::InputChanged {
                id: "group_create_draft".into(),
                value: "Trabajo".into(),
            },
            &ctx,
        );
        app.apply_action(clicked("session_group_create_commit"), &ctx);
        let group_id = app.view.session_groups.groups[0].id.clone();
        assert_eq!(app.view.session_groups.groups[0].title, "Trabajo");
        app.apply_action(
            clicked(format!("session_group_assign:{group_id}:{}", session.id)),
            &ctx,
        );
        assert_eq!(
            app.view.session_groups.group_for(&session.id).unwrap().id,
            group_id
        );
        app.apply_action(clicked(format!("session_group_toggle:{group_id}")), &ctx);
        assert!(app.view.session_groups.groups[0].collapsed);
        app.apply_action(clicked(format!("session_group_rename:{group_id}")), &ctx);
        app.apply_action(
            UiAction::Submitted {
                id: "group_rename_draft".into(),
                value: "Entrega".into(),
            },
            &ctx,
        );
        assert_eq!(app.view.session_groups.groups[0].title, "Entrega");
        assert_eq!(SessionGroups::load(root.path()), app.view.session_groups);
        assert_eq!(
            SidebarPreferences::load(root.path()).organization,
            SessionOrganization::Group
        );
        app.apply_action(clicked(format!("session_group_delete:{group_id}")), &ctx);
        assert_eq!(app.view.session_groups.groups.len(), 1);
        app.apply_action(
            clicked(format!("session_group_delete_confirm:{group_id}")),
            &ctx,
        );
        assert!(app.view.session_groups.groups.is_empty());
        assert!(app.view.session_groups.group_for(&session.id).is_none());
        assert!(app
            .store
            .as_ref()
            .unwrap()
            .load(&session.id)
            .unwrap()
            .is_some());
    }

    #[test]
    fn sidebar_session_actions_require_confirmation_and_persist_unread() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let (first, second) = {
            let store = app.store.as_ref().unwrap();
            (
                store.create("C:/work/alpha", "native").unwrap(),
                store.create("C:/work/alpha", "native").unwrap(),
            )
        };
        app.refresh_sessions();
        let ctx = egui::Context::default();
        app.apply_action(clicked(format!("session_mark_unread:{}", first.id)), &ctx);
        assert!(app
            .store
            .as_ref()
            .unwrap()
            .load(&first.id)
            .unwrap()
            .unwrap()
            .session
            .unread_at_ms
            .is_some());
        app.apply_action(clicked(format!("session_menu:{}", first.id)), &ctx);
        app.apply_action(clicked(format!("session_rename:{}", first.id)), &ctx);
        assert_eq!(
            app.view.renaming_session_id.as_deref(),
            Some(first.id.as_str())
        );
        app.apply_action(clicked(format!("session_menu:{}", second.id)), &ctx);
        assert_eq!(
            app.view.session_menu_id.as_deref(),
            Some(second.id.as_str())
        );
        assert!(app.view.renaming_session_id.is_none());
        app.apply_action(clicked(format!("session_rename:{}", second.id)), &ctx);
        app.apply_action(
            UiAction::Submitted {
                id: "rename_title".into(),
                value: "  ".into(),
            },
            &ctx,
        );
        assert!(app.view.renaming_session_id.is_some());
        assert!(app
            .store
            .as_ref()
            .unwrap()
            .load(&second.id)
            .unwrap()
            .unwrap()
            .session
            .title
            .is_none());
        app.apply_action(
            UiAction::Submitted {
                id: "rename_title".into(),
                value: "Título editado".into(),
            },
            &ctx,
        );
        assert_eq!(
            app.store
                .as_ref()
                .unwrap()
                .load(&second.id)
                .unwrap()
                .unwrap()
                .session
                .title
                .as_deref(),
            Some("Título editado")
        );
        app.apply_action(clicked(format!("session_archive:{}", first.id)), &ctx);
        assert_eq!(
            app.view.pending_archive_session_id.as_deref(),
            Some(first.id.as_str())
        );
        assert!(
            !app.store
                .as_ref()
                .unwrap()
                .load(&first.id)
                .unwrap()
                .unwrap()
                .session
                .archived
        );
        app.apply_action(clicked("session_menu_dismiss"), &ctx);
        assert!(app.view.pending_archive_session_id.is_none());
        assert!(
            !app.store
                .as_ref()
                .unwrap()
                .load(&first.id)
                .unwrap()
                .unwrap()
                .session
                .archived
        );
        app.apply_action(clicked(format!("session_archive:{}", first.id)), &ctx);
        app.apply_action(clicked(format!("archive_confirm:{}", first.id)), &ctx);
        assert!(
            app.store
                .as_ref()
                .unwrap()
                .load(&first.id)
                .unwrap()
                .unwrap()
                .session
                .archived
        );
        assert!(app.view.pending_archive_session_id.is_none());
    }

    #[test]
    fn new_task_moves_keyboard_focus_into_composer() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        app.apply_action(clicked("new_session_sidebar"), &ctx);
        app.tree.set_root(build_view(&app.view, 1200.0));
        app.tree.layout(Size::new(1200.0, 720.0));
        app.focus_prompt_if_pending();
        assert_eq!(app.tree.focused_id(), Some("prompt"));
        assert!(!app.prompt_focus_pending);
    }

    #[test]
    fn resumed_history_keeps_tool_call_and_result_together() {
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            arguments: json!({ "path": "src/main.rs" }),
        };
        let kinds = [
            AgentEventKind::TurnStarted {
                prompt: "Inspect".into(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "test".into(),
            },
            AgentEventKind::ToolStarted {
                tool_call: call.clone(),
            },
            AgentEventKind::ToolFinished {
                tool_call_id: call.id.clone(),
                output: "fn main() {}".into(),
                is_error: false,
            },
            AgentEventKind::ModelRequestStarted {
                step: 2,
                model: "test".into(),
            },
            AgentEventKind::TurnCompleted {
                response: "Reviewed".into(),
                steps: 2,
            },
        ];
        let events = kinds
            .into_iter()
            .enumerate()
            .map(|(index, kind)| AgentEvent::new("session", index as u64 + 1, kind))
            .collect::<Vec<_>>();
        let history = history_from_events(&events);
        assert_eq!(history.len(), 4);
        assert_eq!(history[0].role, MessageRole::User);
        assert_eq!(history[1].tool_calls, vec![call]);
        assert_eq!(history[2].role, MessageRole::Tool);
        assert_eq!(history[2].tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(history[3].content, "Reviewed");
    }

    #[test]
    fn failed_turn_without_output_does_not_duplicate_prompt_on_retry() {
        let kinds = [
            AgentEventKind::TurnStarted {
                prompt: "Intento fallido".into(),
            },
            AgentEventKind::TurnFailed {
                error: "No se pudo iniciar".into(),
            },
            AgentEventKind::TurnStarted {
                prompt: "Intento fallido".into(),
            },
            AgentEventKind::TurnCompleted {
                response: "Hecho".into(),
                steps: 1,
            },
        ];
        let events = kinds
            .into_iter()
            .enumerate()
            .map(|(index, kind)| AgentEvent::new("session", index as u64 + 1, kind))
            .collect::<Vec<_>>();
        let history = history_from_events(&events);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content, "Intento fallido");
        assert_eq!(history[1].content, "Hecho");
    }

    #[test]
    fn dock_motion_retargets_without_jumping_and_snaps_during_resize() {
        let start = Instant::now();
        let mut motion = DockMotion::new(280.0, 360.0, 1400.0, start);
        let (left, right) = motion.update(0.0, 360.0, 1400.0, start, false);
        assert_eq!(left, Some(280.0));
        assert_eq!(right, None);
        let midway = start + Duration::from_millis(80);
        let (left, _) = motion.update(0.0, 360.0, 1400.0, midway, false);
        let midway_width = left.unwrap();
        assert!(midway_width > 0.0 && midway_width < 280.0);
        let (left, _) = motion.update(280.0, 360.0, 1400.0, midway, false);
        assert!((left.unwrap() - midway_width).abs() < 0.01);
        let (left, _) = motion.update(280.0, 360.0, 1200.0, midway, false);
        assert_eq!(left, None);
        assert_eq!(motion.left.value(midway), 280.0);
    }

    #[test]
    fn dock_motion_honors_reduced_motion() {
        let now = Instant::now();
        let mut motion = DockMotion::new(280.0, 0.0, 1400.0, now);
        let (left, right) = motion.update(0.0, 360.0, 1400.0, now, true);
        assert_eq!((left, right), (None, None));
        assert_eq!(motion.left.value(now), 0.0);
        assert_eq!(motion.right.value(now), 360.0);
    }

    #[test]
    fn animated_dock_width_reflows_center_until_collapse_finishes() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        app.view.show_side_pane = true;
        let start = Instant::now();
        app.update_dock_motion(1400.0, start);
        let full = dock_layout(&app.view, 1400.0);
        assert!(full.left > 0.0 && full.right.is_some());
        app.view.sidebar_collapsed = true;
        app.update_dock_motion(1400.0, start);
        app.update_dock_motion(1400.0, start + Duration::from_millis(80));
        let midway = dock_layout(&app.view, 1400.0);
        assert!(midway.left > 0.0 && midway.left < full.left);
        assert!(midway.center > full.center);
        app.update_dock_motion(1400.0, start + Duration::from_millis(220));
        let finished = dock_layout(&app.view, 1400.0);
        assert_eq!(finished.left, 0.0);
        assert!(finished.center > midway.center);
        assert_eq!(app.view.left_presentation_width, None);
        app.view.sidebar_collapsed = false;
        app.update_dock_motion(1400.0, start + Duration::from_millis(220));
        app.update_dock_motion(1400.0, start + Duration::from_millis(260));
        assert!(app.view.left_presentation_width.is_some());
        app.dock_drag = Some(DockDrag::Left);
        app.update_dock_motion(1400.0, start + Duration::from_millis(260));
        assert_eq!(app.view.left_presentation_width, None);
        assert_eq!(app.view.right_presentation_width, None);
    }

    #[test]
    fn grabbing_right_divider_does_not_change_its_width() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        app.view.show_side_pane = true;
        app.view.right_dock_width = Some(360.0);
        let viewport_width = 1400.0;
        let split = dock_layout(&app.view, viewport_width)
            .right_split_x
            .expect("right dock is visible");
        let pointer = openide_ui::Point::new(split, 120.0);
        assert!(app.handle_dock_drag(&UiEvent::PointerDown(pointer), viewport_width));
        assert!(app.handle_dock_drag(&UiEvent::PointerMove(pointer), viewport_width));
        assert_eq!(app.view.right_dock_width, Some(360.0));
    }
}
