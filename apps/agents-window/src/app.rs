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
    exec_policy_settings::{ExecPolicySettings, ExecPolicyUiState},
    files::FileExplorer,
    git_review::DiffStage,
    github::GithubStatus,
    github_account::{GithubAccountStatus, GithubAccountWorker},
    github_worker::{GithubRequest, GithubWorker},
    marketplace::{InstallScope, MarketplaceStore, SourceKind},
    marketplace_view::MarketplaceViewState,
    mcp_oauth_controller::{
        forget_oauth_registration, oauth_registration_changed, McpOAuthController, McpOAuthEvent,
    },
    mcp_runtime::McpToolPreparation,
    mcp_settings::{McpHttpOAuthSettings, McpSettings},
    model_catalog::{self, CatalogSnapshot},
    native_command_sessions::NativeCommandSessions,
    native_hook_settings::{HookUiState, NativeHookSettings},
    native_provider::{self, NativeProviderSettings, NativeWireApi, RuntimeConnection},
    native_skills_settings::{NativeSkillsSettings, NativeSkillsUiState},
    navigation::{NavigationHistory, NavigationTarget},
    render,
    resume_worker::{ResumeUpdate, ResumeWorkerHandle, ResumeWorkerRequest},
    review_worker::{ReviewRequest, ReviewWorker},
    search_worker::{SearchRequest, SearchResponse, SearchWorker},
    session_groups::SessionGroups,
    settings_preferences::{SettingsPreferences, TerminalShell},
    settings_ui::{self, SettingsPage},
    subagent_recovery_worker::{
        SubagentRecoveryHandle, SubagentRecoveryRequest, SubagentRecoveryUpdate,
    },
    terminal::TerminalSession,
    transcript::TranscriptProjection,
    view::{
        build_command_center, build_view, dock_layout, AbortedToolBatchView,
        ComposerAttachmentView, DockLayout, DockRecentlyClosedView, DockTabView, FilePreviewView,
        FileRowView, MainPage, McpOAuthViewPhase, McpOAuthViewStatus, PermissionView,
        ProviderModelStatus, ReviewDetailView, SessionApprovalGrantView, SessionOrganization,
        SessionPlanView, SessionRecoveryView, SessionView, SideTab, SubagentPermissionView,
        SubagentRunView, ToolVerificationView, ViewState,
    },
    voice::{self, VoiceEvent, VoiceHandle},
};
use eframe::egui;
use openide_cli::{CliProfile, CliRunHandle, CliRunner};
use openide_core::{
    replay_native, validate_user_images, AgentEvent, AgentEventKind, AgentMessage, AgentRunRequest,
    CheckpointBudgetSnapshot, CheckpointCompatibility, CheckpointFileStore, NativeReplay,
    PermissionDecision, ReplayRecovery, SessionSort, SessionStore, StoredSession, ToolCall,
    TurnMode, UserImage, UserImageMime, MAX_USER_IMAGE_BYTES,
};
use openide_harness::{
    discard_aborted_tool_batch, finalize_resolved_subagent, load_project_instructions,
    AgentRunHandle, AgentRunner, CheckpointCoordinator, CommandIsolation, ExecDecision, HookPhase,
    HostedWebSearchConfig, HostedWebSearchMode, LocalTools, ModelBackend, ModelRequestCapabilities,
    OpenAiCompatibleBackend, PermissionChoice, PermissionMode, PermissionPolicy,
    PersistentCommandManager, PersistentCommandSessionInfo, ProjectInstructionLimits,
    ResponsesBackend, ResumeDisposition, RunLimits, SubagentBridgeEvent, SubagentCoordinator,
    SubagentEventBridge, SubagentLimits, SubagentRecoveryPause, SubagentRecoveryState,
    SubagentStatus, SubagentToolExecutor, SubagentToolLimits, SubagentWorker, ToolExecutor,
    ToolOutput,
};
use openide_ui::{
    DatePicker, Easing, Element, FontFamily, FontMetrics, Frame as UiFrame, ScalarTransition, Size,
    TimePicker, UiAction, UiEvent, UiTree,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{
    mpsc::{self, Receiver, TryRecvError},
    Arc,
};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

type ComposerFolderResponse = (String, Result<Option<PathBuf>, String>);
type NativeCommandSnapshotResponse = (
    String,
    Arc<PersistentCommandManager>,
    Result<Vec<PersistentCommandSessionInfo>, String>,
);
type NativeCommandTerminationResponse = (
    String,
    String,
    Arc<PersistentCommandManager>,
    Result<(), String>,
);

enum ActiveRun {
    Native(AgentRunHandle),
    Cli(CliRunHandle),
}

fn subagent_status_label(status: &SubagentStatus) -> &'static str {
    match status {
        SubagentStatus::Ready => "pendiente",
        SubagentStatus::Running => "en ejecución",
        SubagentStatus::Interrupted => "interrumpido",
        SubagentStatus::Completed => "completado",
        SubagentStatus::Failed => "fallido",
        SubagentStatus::Cancelled => "cancelado",
    }
}

struct ActiveCompact {
    session_id: String,
    receiver: Receiver<Result<u64, String>>,
    cancel: CancellationToken,
}

fn native_model_backend(
    connection: RuntimeConnection,
    provider_id: &str,
    effort: Option<&str>,
    hosted_web_search_mode: Option<HostedWebSearchMode>,
    data_root: &Path,
    catalog: Option<&CatalogSnapshot>,
    discoveries: &[native_provider::ProviderModelDiscovery],
) -> Result<Arc<dyn ModelBackend>, String> {
    let adapter = native_provider::adapter_at(data_root, provider_id)?
        .ok_or_else(|| format!("Proveedor nativo no registrado: {provider_id}"))?;
    if adapter.wire_api != connection.wire_api {
        return Err("El protocolo del proveedor cambió durante la conexión".to_owned());
    }
    let capabilities =
        native_request_capabilities(&adapter, &connection.model_id, catalog, discoveries);
    if !capabilities.tool_calls {
        return Err(format!(
            "El proveedor declara que {} no admite herramientas en esta ruta nativa",
            connection.model_id
        ));
    }
    let effort = effort.filter(|value| *value != "default");
    if let Some(effort) = effort {
        if !capabilities
            .reasoning_efforts
            .iter()
            .any(|allowed| allowed == effort)
        {
            return Err(format!(
                "El esfuerzo '{effort}' no figura para {} en el catálogo del proveedor",
                connection.model_id
            ));
        }
    }
    match connection.wire_api {
        NativeWireApi::Responses => {
            let mut model =
                ResponsesBackend::new(connection.endpoint, connection.api_key, connection.model_id);
            if let Some(effort) = effort {
                model = model
                    .with_reasoning_effort(effort)
                    .map_err(|error| error.to_string())?;
            }
            if let Some(mode) = hosted_web_search_mode {
                model = model
                    .with_hosted_web_search(HostedWebSearchConfig::new(mode))
                    .map_err(|error| error.to_string())?;
            }
            Ok(Arc::new(model))
        }
        NativeWireApi::ChatCompletions => {
            let mut model = OpenAiCompatibleBackend::new(
                connection.endpoint,
                connection.api_key,
                connection.model_id,
            )
            .with_capabilities(capabilities)
            .map_err(|error| error.to_string())?;
            for (field, value) in adapter.chat_request_fields {
                model = model
                    .with_request_field(field, value)
                    .map_err(|error| error.to_string())?;
            }
            if let Some(effort) = effort {
                model = model
                    .with_reasoning_effort(effort)
                    .map_err(|error| error.to_string())?;
            }
            Ok(Arc::new(model))
        }
    }
}

fn native_request_capabilities(
    adapter: &native_provider::Adapter,
    model_id: &str,
    catalog: Option<&CatalogSnapshot>,
    discoveries: &[native_provider::ProviderModelDiscovery],
) -> ModelRequestCapabilities {
    let reviewed = native_provider::native_model_capabilities(&adapter.id, model_id)
        .filter(|capabilities| capabilities.wire_api == adapter.wire_api);
    let discovered = discoveries
        .iter()
        .find(|discovery| discovery.provider_id == adapter.id)
        .and_then(|discovery| discovery.models.get(model_id));
    let listed = catalog.and_then(|snapshot| snapshot.model(&adapter.id, model_id));
    let reported_tool_calls = discovered
        .and_then(|model| model.tool_call_declared)
        .or_else(|| listed.and_then(|model| model.tool_call_declared));
    // Missing provider metadata leaves an explicitly configured route free to
    // try tools. A provider that reports a tools field may explicitly deny it.
    let tool_calls = reviewed.map_or_else(
        || {
            if adapter.catalog.tool_call_path.is_some() {
                reported_tool_calls.unwrap_or(true)
            } else {
                true
            }
        },
        |capabilities| capabilities.function_tools && capabilities.streaming,
    );
    let wire_supports_effort = adapter.wire_api == NativeWireApi::Responses
        || adapter.reasoning_effort_field.as_deref() == Some("reasoning_effort");
    let raw_efforts = wire_supports_effort
        .then(|| {
            discovered
                .map(|model| model.supported_efforts.as_slice())
                .filter(|efforts| !efforts.is_empty())
                .or_else(|| {
                    listed
                        .map(|model| model.provider_efforts.as_slice())
                        .filter(|efforts| !efforts.is_empty())
                })
        })
        .flatten();
    let mut reasoning_efforts = Vec::new();
    if let Some(efforts) = raw_efforts {
        for effort in efforts {
            if !effort.is_empty()
                && effort.len() <= 64
                && !effort.chars().any(char::is_whitespace)
                && !effort.chars().any(char::is_control)
                && !reasoning_efforts.contains(effort)
            {
                reasoning_efforts.push(effort.clone());
            }
        }
    } else if let Some(reviewed) = reviewed.filter(|_| wire_supports_effort) {
        reasoning_efforts.extend(
            reviewed
                .supported_efforts
                .iter()
                .map(|effort| (*effort).to_owned()),
        );
    }
    ModelRequestCapabilities {
        tool_calls,
        reasoning_efforts,
        tool_call_reasoning_efforts: None,
        default_tool_call_effort: None,
    }
}

fn native_key_available_at(settings: &NativeProviderSettings, data_root: &Path) -> bool {
    settings
        .resolve_at(data_root)
        .is_ok_and(|connection| connection.api_key.is_some())
}

fn native_checkpoint_compatibility(
    provider_id: &str,
    model_id: &str,
    wire_api: NativeWireApi,
) -> CheckpointCompatibility {
    CheckpointCompatibility {
        provider: provider_id.to_owned(),
        model: model_id.to_owned(),
        context_format_key: match wire_api {
            NativeWireApi::Responses => "responses-v1",
            NativeWireApi::ChatCompletions => "chat-completions-v1",
        }
        .to_owned(),
    }
}

fn context_remaining_percent(used_tokens: u64, context_window_tokens: u64) -> Option<u8> {
    // Match Codex's status meaning: percentage of usable context remaining.
    // The baseline is reserved for fixed prompt/tool overhead.
    const BASELINE_TOKENS: u64 = 12_000;
    if context_window_tokens <= BASELINE_TOKENS {
        return None;
    }
    let window = u128::from(context_window_tokens - BASELINE_TOKENS);
    let used = u128::from(used_tokens.saturating_sub(BASELINE_TOKENS)).min(window);
    let rounded = ((window - used) * 100 + window / 2) / window;
    Some(rounded.min(100) as u8)
}

/// Only report a measurement tied to the currently selected verified model.
/// A checkpoint replaces the old context, so its earlier usage is no longer
/// representative until the provider commits another response.
fn committed_context_percent(
    events: &[AgentEvent],
    provider_id: &str,
    model_id: &str,
) -> Option<u8> {
    let window =
        native_provider::verified_token_budget(provider_id, model_id)?.context_window_tokens;
    let (response_index, usage) =
        events
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, event)| match &event.kind {
                AgentEventKind::ModelResponseCommitted { usage, .. } => {
                    Some((index, usage.as_ref()))
                }
                _ => None,
            })?;
    let usage = usage?;
    let request_model =
        events[..response_index]
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                AgentEventKind::ModelRequestStarted { model, .. } => Some(model.as_str()),
                _ => None,
            })?;
    if request_model != model_id
        || events[response_index + 1..]
            .iter()
            .any(|event| matches!(&event.kind, AgentEventKind::CheckpointCommitted { .. }))
    {
        return None;
    }
    context_remaining_percent(usage.total_tokens, window)
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
    fn journal_error(&self) -> Option<String> {
        match self {
            Self::Native(run) => run.journal_error(),
            Self::Cli(_) => None,
        }
    }

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
    exec_policy_settings: ExecPolicySettings,
    exec_policy_load_error: Option<String>,
    native_command_sessions: NativeCommandSessions,
    native_command_snapshot_response: Option<Receiver<NativeCommandSnapshotResponse>>,
    native_command_termination_response: Option<Receiver<NativeCommandTerminationResponse>>,
    last_native_command_snapshot: Option<Instant>,
    tree: UiTree,
    projection: TranscriptProjection,
    profiles: Vec<CliProfile>,
    store: Option<Arc<SessionStore>>,
    /// Project paths from every stored session, before sidebar archive filtering.
    known_project_paths: Vec<String>,
    active_run: Option<ActiveRun>,
    dictation: Option<VoiceHandle>,
    dictation_key: Option<(String, Option<String>)>,
    active_compact: Option<ActiveCompact>,
    active_resume: Option<ResumeWorkerHandle>,
    active_child_recovery: Option<SubagentRecoveryHandle>,
    pending_child_recovery_permission: Option<SubagentPermissionView>,
    active_mcp_oauth: Option<McpOAuthController>,
    subagent_bridge: Arc<SubagentEventBridge>,
    subagent_events: Receiver<SubagentBridgeEvent>,
    last_subagent_poll: Instant,
    subagent_poll_needed: bool,
    automation_stores: Vec<AutomationStore>,
    automation_last_poll: Instant,
    background_automation_runs: Vec<BackgroundAutomationRun>,
    automation_queue: VecDeque<(usize, AutomationDispatch)>,
    marketplace_response: Option<Receiver<MarketplaceResponse>>,
    workspace_folder_response: Option<Receiver<Result<Option<PathBuf>, String>>>,
    composer_folder_response: Option<Receiver<ComposerFolderResponse>>,
    catalog_response: Option<Receiver<Result<CatalogSnapshot, String>>>,
    catalog_last_refresh: Instant,
    model_discovery_response: Option<
        Receiver<(
            String,
            Result<native_provider::ProviderModelDiscovery, String>,
        )>,
    >,
    model_discovery_pending: usize,
    model_discoveries: Vec<native_provider::ProviderModelDiscovery>,
    codex_model_response: Option<Receiver<Result<Vec<openide_cli::DiscoveredCodexModel>, String>>>,
    codex_models: Vec<openide_cli::DiscoveredCodexModel>,
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
    composer_goals: HashMap<(String, Option<String>), String>,
    draft_dirty_at: Option<Instant>,
    draft_error: Option<String>,
    persistence_error: Option<String>,
    navigation: NavigationHistory,
    command_tree: UiTree,
    command_center_attaches_file: bool,
    command_focus_pending: bool,
    prompt_focus_pending: bool,
    composer_model_search_focus_pending: bool,
    rename_focus_pending: bool,
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
    if let Some(path) = std::env::var_os("OPENIDE_DATA_ROOT") {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return path;
        }
    }
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

fn command_center_rect(area: egui::Rect) -> egui::Rect {
    let width = (area.width() - 32.0).clamp(0.0, 560.0);
    let height = (area.height() - 48.0).clamp(0.0, 500.0);
    egui::Rect::from_center_size(area.center(), egui::vec2(width, height))
}

/// Runs off the UI thread. PowerShell hosts the Windows folder dialog without
/// opening a console; the selected path is passed back over stdout as UTF-8.
#[cfg(windows)]
fn choose_workspace_folder(initial: &Path, description: &str) -> Result<Option<PathBuf>, String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName System.Windows.Forms
$dialog = New-Object System.Windows.Forms.FolderBrowserDialog
$dialog.Description = $env:OPENIDE_PICKER_DESCRIPTION
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
        .env("OPENIDE_PICKER_DESCRIPTION", description)
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
fn choose_workspace_folder(_initial: &Path, _description: &str) -> Result<Option<PathBuf>, String> {
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
const MAX_COMPOSER_GOAL_BYTES: usize = 8 * 1024;
const MAX_COMPOSER_GOALS_FILE_BYTES: u64 = 2 * 1024 * 1024;
static COMPOSER_GOALS_SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static NATIVE_STEER_DRAFT_SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const MAX_NATIVE_STEER_DRAFT_FILE_BYTES: u64 = 128 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeSteerDraftAttachment {
    path: String,
    label: String,
    is_dir: bool,
}

impl From<&ComposerAttachmentView> for NativeSteerDraftAttachment {
    fn from(attachment: &ComposerAttachmentView) -> Self {
        Self {
            path: attachment.path.clone(),
            label: attachment.label.clone(),
            is_dir: attachment.is_dir,
        }
    }
}

impl From<NativeSteerDraftAttachment> for ComposerAttachmentView {
    fn from(attachment: NativeSteerDraftAttachment) -> Self {
        Self {
            path: attachment.path,
            label: attachment.label,
            is_dir: attachment.is_dir,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeSteerImageKey {
    mime_type: UserImageMime,
    size_bytes: u32,
    sha256: String,
}

impl From<&UserImage> for NativeSteerImageKey {
    fn from(image: &UserImage) -> Self {
        Self {
            mime_type: image.mime_type,
            size_bytes: image.size_bytes,
            sha256: image.sha256.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeSteerDraftRetry {
    turn_id: String,
    input_id: String,
    payload_text: String,
    images: Vec<NativeSteerImageKey>,
    acknowledged: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeSteerDraftState {
    version: u32,
    workspace: String,
    session_id: String,
    prompt: String,
    attachments: Vec<NativeSteerDraftAttachment>,
    retry: Option<NativeSteerDraftRetry>,
}

impl NativeSteerDraftState {
    fn for_draft(
        workspace: &Path,
        session_id: &str,
        prompt: &str,
        attachments: &[ComposerAttachmentView],
    ) -> Self {
        Self {
            version: 1,
            workspace: workspace.to_string_lossy().into_owned(),
            session_id: session_id.to_owned(),
            prompt: prompt.to_owned(),
            attachments: attachments
                .iter()
                .map(NativeSteerDraftAttachment::from)
                .collect(),
            retry: None,
        }
    }

    fn matches_draft(
        &self,
        workspace: &Path,
        prompt: &str,
        attachments: &[ComposerAttachmentView],
    ) -> bool {
        self.workspace == workspace.to_string_lossy()
            && self.prompt == prompt
            && self.attachments
                == attachments
                    .iter()
                    .map(NativeSteerDraftAttachment::from)
                    .collect::<Vec<_>>()
    }
}

fn native_steer_draft_path(root: &Path, session_id: &str) -> Result<PathBuf, String> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("Identificador de sesión inválido para el borrador".to_owned());
    }
    Ok(root
        .join("native-steer-drafts")
        .join(format!("{session_id}.json")))
}

fn load_native_steer_draft(
    root: &Path,
    session_id: &str,
) -> Result<Option<NativeSteerDraftState>, String> {
    let path = native_steer_draft_path(root, session_id)?;
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "No se pudo leer el borrador de intervención: {error}"
            ))
        }
    };
    if bytes.len() as u64 > MAX_NATIVE_STEER_DRAFT_FILE_BYTES {
        return Err("El borrador de intervención excede el límite de tamaño".to_owned());
    }
    let state: NativeSteerDraftState = serde_json::from_slice(&bytes)
        .map_err(|error| format!("El borrador de intervención no es válido: {error}"))?;
    if state.version != 1
        || state.session_id != session_id
        || state.prompt.len() > 32 * 1024
        || state.attachments.len() > MAX_COMPOSER_ATTACHMENTS
        || state.attachments.iter().any(|attachment| {
            attachment.path.is_empty() || attachment.path.len() > MAX_COMPOSER_ATTACHMENT_PATH_BYTES
        })
        || state.retry.as_ref().is_some_and(|retry| {
            retry.input_id.is_empty()
                || retry.input_id.len() > 128
                || retry.turn_id.is_empty()
                || retry.turn_id.len() > 128
                || retry.payload_text.len() > 32 * 1024
                || retry.images.len() > 8
        })
    {
        return Err("El borrador de intervención no es válido".to_owned());
    }
    Ok(Some(state))
}

fn save_native_steer_draft(root: &Path, state: &NativeSteerDraftState) -> Result<(), String> {
    let path = native_steer_draft_path(root, &state.session_id)?;
    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("No se pudo serializar el borrador de intervención: {error}"))?;
    if bytes.len() as u64 > MAX_NATIVE_STEER_DRAFT_FILE_BYTES {
        return Err("El borrador de intervención excede el límite de tamaño".to_owned());
    }
    let parent = path.parent().expect("the draft path has a parent");
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("No se pudo preparar el borrador de intervención: {error}"))?;
    let temp = parent.join(format!(
        ".{session_id}-{}-{}.tmp",
        std::process::id(),
        NATIVE_STEER_DRAFT_SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        session_id = state.session_id
    ));
    let outcome = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    outcome.map_err(|error| format!("No se pudo guardar el borrador de intervención: {error}"))
}

fn remove_native_steer_draft(root: &Path, session_id: &str) -> Result<(), String> {
    match std::fs::remove_file(native_steer_draft_path(root, session_id)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "No se pudo limpiar el borrador de intervención: {error}"
        )),
    }
}

fn sync_native_steer_draft(
    root: &Path,
    workspace: &Path,
    session_id: &str,
    prompt: &str,
    attachments: &[ComposerAttachmentView],
) -> Result<(), String> {
    let Some(mut state) = load_native_steer_draft(root, session_id)? else {
        return Ok(());
    };
    if state.matches_draft(workspace, prompt, attachments) {
        return Ok(());
    }
    state.workspace = workspace.to_string_lossy().into_owned();
    state.prompt = prompt.to_owned();
    state.attachments = attachments
        .iter()
        .map(NativeSteerDraftAttachment::from)
        .collect();
    state.retry = None;
    save_native_steer_draft(root, &state)
}

struct NativeSteerDraftSubmission<'a> {
    workspace: &'a Path,
    session_id: &'a str,
    turn_id: &'a str,
    prompt: &'a str,
    attachments: &'a [ComposerAttachmentView],
    payload_text: &'a str,
    images: &'a [UserImage],
}

fn enqueue_native_steer_draft(
    root: &Path,
    store: &SessionStore,
    submission: NativeSteerDraftSubmission<'_>,
) -> Result<String, String> {
    let NativeSteerDraftSubmission {
        workspace,
        session_id,
        turn_id,
        prompt,
        attachments,
        payload_text,
        images,
    } = submission;
    let previous = load_native_steer_draft(root, session_id)?;
    let mut state = previous
        .filter(|state| state.matches_draft(workspace, prompt, attachments))
        .unwrap_or_else(|| {
            NativeSteerDraftState::for_draft(workspace, session_id, prompt, attachments)
        });
    let image_keys = images
        .iter()
        .map(NativeSteerImageKey::from)
        .collect::<Vec<_>>();
    let retry = state
        .retry
        .take()
        .filter(|retry| {
            retry.turn_id == turn_id
                && retry.payload_text == payload_text
                && retry.images == image_keys
        })
        .unwrap_or_else(|| NativeSteerDraftRetry {
            turn_id: turn_id.to_owned(),
            input_id: Uuid::new_v4().to_string(),
            payload_text: payload_text.to_owned(),
            images: image_keys,
            acknowledged: false,
        });
    let input_id = retry.input_id.clone();
    state.retry = Some(retry);
    // Both the text draft and its attachment/nonce state must reach disk before
    // the inbox write. An uncertain enqueue can then retry with this exact ID.
    store
        .save_draft(workspace, Some(session_id), prompt)
        .map_err(|error| error.to_string())?;
    save_native_steer_draft(root, &state)?;
    store
        .enqueue_native_steer_with_images(session_id, turn_id, &input_id, payload_text, images)
        .map_err(|error| error.to_string())?;
    Ok(input_id)
}

fn acknowledge_native_steer_draft(
    root: &Path,
    store: &SessionStore,
    workspace: &Path,
    session_id: &str,
    input_id: &str,
) -> Result<(), String> {
    let mut state = load_native_steer_draft(root, session_id)?
        .ok_or_else(|| "Falta el borrador de la intervención aceptada".to_owned())?;
    let retry = state
        .retry
        .as_mut()
        .filter(|retry| retry.input_id == input_id)
        .ok_or_else(|| "El borrador de intervención cambió durante el envío".to_owned())?;
    retry.acknowledged = true;
    save_native_steer_draft(root, &state)?;
    // Clear the core draft before dropping the accepted marker. This ordering
    // also handles image-only drafts, whose core text draft is empty.
    store
        .clear_draft(workspace, Some(session_id))
        .map_err(|error| error.to_string())?;
    remove_native_steer_draft(root, session_id)
}

#[derive(Default, Serialize, Deserialize)]
struct ComposerGoalsFile {
    #[serde(default)]
    goals: Vec<ComposerGoalEntry>,
}

#[derive(Serialize, Deserialize)]
struct ComposerGoalEntry {
    workspace: String,
    session_id: Option<String>,
    objective: String,
}

fn load_composer_goals(root: &Path) -> Result<HashMap<(String, Option<String>), String>, String> {
    let path = root.join("composer-goals.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(format!("No se pudieron leer las metas: {error}")),
    };
    if bytes.len() as u64 > MAX_COMPOSER_GOALS_FILE_BYTES {
        return Err("El archivo de metas excede el límite de tamaño".to_owned());
    }
    let data: ComposerGoalsFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("El archivo de metas no es válido: {error}"))?;
    Ok(data
        .goals
        .into_iter()
        .filter(|entry| {
            !entry.objective.trim().is_empty() && entry.objective.len() <= MAX_COMPOSER_GOAL_BYTES
        })
        .map(|entry| ((entry.workspace, entry.session_id), entry.objective))
        .collect())
}

fn save_composer_goals(
    root: &Path,
    goals: &HashMap<(String, Option<String>), String>,
) -> Result<(), String> {
    let mut entries: Vec<_> = goals
        .iter()
        .map(|((workspace, session_id), objective)| ComposerGoalEntry {
            workspace: workspace.clone(),
            session_id: session_id.clone(),
            objective: objective.clone(),
        })
        .collect();
    entries.sort_by(|a, b| (&a.workspace, &a.session_id).cmp(&(&b.workspace, &b.session_id)));
    let bytes = serde_json::to_vec_pretty(&ComposerGoalsFile { goals: entries })
        .map_err(|error| format!("No se pudieron serializar las metas: {error}"))?;
    if bytes.len() as u64 > MAX_COMPOSER_GOALS_FILE_BYTES {
        return Err("Hay demasiadas metas para guardar".to_owned());
    }
    std::fs::create_dir_all(root)
        .map_err(|error| format!("No se pudo preparar el directorio de metas: {error}"))?;
    let temp = root.join(format!(
        ".composer-goals-{}-{}.tmp",
        std::process::id(),
        COMPOSER_GOALS_SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let outcome = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, root.join("composer-goals.json"))
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    outcome.map_err(|error| format!("No se pudieron guardar las metas: {error}"))
}

fn composer_goal_instruction(objective: &str) -> String {
    format!(
        "Objetivo vigente de esta tarea:\n{objective}\nTen presente este objetivo al responder a la solicitud actual."
    )
}

fn system_instructions_with_goal(skills: String, goal: Option<&str>) -> String {
    match goal {
        Some(objective) if !objective.trim().is_empty() => {
            let instruction = composer_goal_instruction(objective);
            if skills.is_empty() {
                instruction
            } else {
                format!("{skills}\n\n{instruction}")
            }
        }
        _ => skills,
    }
}

fn native_workspace_instructions(data_root: &Path, workspace: &Path) -> Result<String, String> {
    let project =
        load_project_instructions(workspace, workspace, ProjectInstructionLimits::default())
            .map_err(|error| format!("No se pudieron leer AGENTS.md: {error}"))?
            .text();
    let skills = crate::marketplace::active_skill_instructions(data_root, workspace)
        .map_err(|error| format!("No se pudieron leer las skills: {error}"))?;
    let local_skills = NativeSkillsSettings::load(data_root, workspace)?.activate(data_root)?;
    let mut sections = Vec::new();
    if !project.is_empty() {
        sections.push(format!("Instrucciones del proyecto:\n{project}"));
    }
    if !skills.is_empty() {
        sections.push(skills);
    }
    if !local_skills.selected.is_empty() {
        sections.push(format!(
            "Skills locales seleccionadas (identidad {}):\n{}",
            local_skills.sha256_hex, local_skills.instructions
        ));
    }
    Ok(sections.join("\n\n"))
}

fn native_resume_instructions(
    data_root: &Path,
    workspace: &Path,
    goal: Option<&str>,
    saved: Option<&str>,
) -> Result<String, String> {
    if let Some(saved) = saved {
        return Ok(saved.to_owned());
    }
    native_workspace_instructions(data_root, workspace)
        .map(|instructions| system_instructions_with_goal(instructions, goal))
}

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
        .map_err(|error| format!("No se pudo leer el adjunto: {error}"))?;
    let is_dir = file.is_dir();
    if !file.is_file() && !is_dir {
        return Err("Selecciona un archivo o carpeta del proyecto".to_owned());
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
    Ok(ComposerAttachmentView {
        path,
        label,
        is_dir,
    })
}

fn composer_prompt_with_attachments(
    visible_prompt: &str,
    attachments: &[ComposerAttachmentView],
) -> String {
    if attachments.is_empty() {
        return visible_prompt.to_owned();
    }
    let paths: Vec<_> = attachments
        .iter()
        .map(|entry| {
            serde_json::json!({
                "path": entry.path,
                "type": if entry.is_dir { "folder" } else { "file" },
            })
        })
        .collect();
    let listed = serde_json::to_string(&paths).expect("attachment paths serialize as JSON");
    let introduction = if visible_prompt.is_empty() {
        "Revisa los archivos adjuntos."
    } else {
        visible_prompt
    };
    format!(
        "{introduction}\n\nAdjuntos de este proyecto (rutas relativas): {listed}\nAbre los archivos y explora las carpetas pertinentes; úsalos como contexto para esta tarea."
    )
}

fn native_image_mime(path: &str) -> Option<UserImageMime> {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some(UserImageMime::Png),
        "jpg" | "jpeg" => Some(UserImageMime::Jpeg),
        "webp" => Some(UserImageMime::Webp),
        "gif" => Some(UserImageMime::Gif),
        _ => None,
    }
}

fn native_image_from_attachment(
    workspace: &Path,
    attachment: &ComposerAttachmentView,
) -> Result<UserImage, String> {
    let mime = native_image_mime(&attachment.path)
        .ok_or_else(|| "El adjunto no es una imagen compatible".to_owned())?;
    let root = workspace
        .canonicalize()
        .map_err(|error| format!("No se pudo leer el proyecto: {error}"))?;
    let path = workspace
        .join(&attachment.path)
        .canonicalize()
        .map_err(|error| format!("No se pudo leer la imagen: {error}"))?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err("La imagen debe ser un archivo del proyecto".to_owned());
    }
    let file = std::fs::File::open(&path)
        .map_err(|error| format!("No se pudo abrir la imagen: {error}"))?;
    if file
        .metadata()
        .map_err(|error| format!("No se pudo inspeccionar la imagen: {error}"))?
        .len()
        > MAX_USER_IMAGE_BYTES as u64
    {
        return Err("La imagen supera el límite de 8 MiB".to_owned());
    }
    let mut bytes = Vec::new();
    file.take(MAX_USER_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("No se pudo leer la imagen: {error}"))?;
    UserImage::from_bytes(mime, &bytes)
        .map_err(|error| format!("Imagen adjunta no válida: {error}"))
}

fn native_prompt_with_images(
    visible_prompt: &str,
    other_attachments: &[ComposerAttachmentView],
    image_attachments: &[ComposerAttachmentView],
) -> String {
    let introduction = if visible_prompt.is_empty() && !image_attachments.is_empty() {
        "Analiza las imágenes adjuntas."
    } else {
        visible_prompt
    };
    let mut prompt = composer_prompt_with_attachments(introduction, other_attachments);
    if !image_attachments.is_empty() {
        let names = image_attachments
            .iter()
            .map(|attachment| attachment.label.as_str())
            .collect::<Vec<_>>();
        let names = serde_json::to_string(&names).expect("image labels serialize as JSON");
        prompt.push_str("\n\nImágenes enviadas al modelo en este orden: ");
        prompt.push_str(&names);
    }
    prompt
}

struct NativeAttachmentPayload {
    images: Vec<UserImage>,
    image_attachments: Vec<ComposerAttachmentView>,
    path_attachments: Vec<ComposerAttachmentView>,
}

fn prepare_native_attachments(
    workspace: &Path,
    attachments: &[ComposerAttachmentView],
    provider_id: &str,
    model_id: &str,
) -> Result<NativeAttachmentPayload, String> {
    let mut payload = NativeAttachmentPayload {
        images: Vec::new(),
        image_attachments: Vec::new(),
        path_attachments: Vec::new(),
    };
    for attachment in attachments {
        let current = composer_attachment_for_path(workspace, Path::new(&attachment.path))?;
        if current.path != attachment.path || current.is_dir != attachment.is_dir {
            return Err("Un adjunto cambió de ubicación o tipo".to_owned());
        }
        if !attachment.is_dir && native_image_mime(&attachment.path).is_some() {
            payload.image_attachments.push(attachment.clone());
        } else {
            payload.path_attachments.push(attachment.clone());
        }
    }
    if !payload.image_attachments.is_empty() {
        let supported = native_provider::native_model_capabilities(provider_id, model_id)
            .is_some_and(|capability| capability.image_input);
        if !supported {
            return Err(
                "El modelo nativo seleccionado no tiene entrada de imágenes verificada".to_owned(),
            );
        }
        for attachment in &payload.image_attachments {
            payload
                .images
                .push(native_image_from_attachment(workspace, attachment)?);
        }
        validate_user_images(&payload.images)
            .map_err(|error| format!("Imágenes adjuntas no válidas: {error}"))?;
    }
    Ok(payload)
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
    let verified = snapshot
        .provider_status
        .values()
        .filter(|status| {
            status.source == model_catalog::CatalogSource::OfficialApi && status.verified_at_ms > 0
        })
        .count();
    let mut summary = if snapshot.fetched_at_ms == 0 {
        format!(
            "Base local revisada: {} proveedores, {model_count} modelos; fuentes oficiales sin comprobar",
            snapshot.providers.len()
        )
    } else {
        let checked = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
            snapshot.fetched_at_ms.min(i64::MAX as u64) as i64,
        )
        .map(|date| date.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "fecha desconocida".to_owned());
        format!(
            "Última fuente verificada {checked}: {} proveedores, {model_count} modelos; {verified}/{} fuentes con copia oficial",
            snapshot.providers.len(),
            snapshot.provider_status.len()
        )
    };
    let issues: Vec<_> = snapshot
        .provider_status
        .iter()
        .filter_map(|(provider, status)| {
            status
                .last_error
                .as_ref()
                .map(|error| format!("{provider}: {error}"))
        })
        .collect();
    if !issues.is_empty() {
        summary.push_str("; ");
        summary.push_str(&issues.join("; "));
    }
    summary
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
    committed: &mut Option<AgentMessage>,
) {
    if !calls.is_empty()
        && calls.len() == results.len()
        && calls
            .iter()
            .zip(results.iter())
            .all(|(call, result)| result.tool_call_id.as_deref() == Some(call.id.as_str()))
    {
        history.push(committed.take().unwrap_or_else(|| {
            AgentMessage::assistant(std::mem::take(content), std::mem::take(calls))
        }));
        history.append(results);
    }
    content.clear();
    calls.clear();
    results.clear();
    *committed = None;
}

/// Rebuilds the exact model-visible tool exchange from durable turn events.
fn history_from_events(events: &[AgentEvent]) -> Vec<AgentMessage> {
    let mut history = Vec::new();
    let mut content = String::new();
    let mut calls = Vec::new();
    let mut results = Vec::new();
    let mut committed = None;
    let mut step_has_committed_response = false;
    let mut turn_start_len = 0;
    let mut turn_had_output = false;
    for event in events {
        match &event.kind {
            AgentEventKind::TurnStarted { prompt } => {
                turn_start_len = history.len();
                turn_had_output = false;
                committed = None;
                step_has_committed_response = false;
                history.push(AgentMessage::user(prompt.clone()));
            }
            AgentEventKind::ModelRequestStarted { step, .. } if *step > 1 => {
                flush_tool_batch(
                    &mut history,
                    &mut content,
                    &mut calls,
                    &mut results,
                    &mut committed,
                );
                step_has_committed_response = false;
            }
            AgentEventKind::ModelResponseCommitted { assistant, .. } => {
                content = assistant.content.clone();
                calls = assistant.tool_calls.clone();
                committed = Some(assistant.clone());
                step_has_committed_response = true;
                turn_had_output = true;
            }
            AgentEventKind::AssistantMessage { content: message }
                if !step_has_committed_response =>
            {
                content = message.clone();
                turn_had_output = true;
            }
            AgentEventKind::ToolStarted { tool_call } if !step_has_committed_response => {
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
                if calls.is_empty() {
                    history.push(
                        committed.take().unwrap_or_else(|| {
                            AgentMessage::assistant(response.clone(), Vec::new())
                        }),
                    );
                } else {
                    flush_tool_batch(
                        &mut history,
                        &mut content,
                        &mut calls,
                        &mut results,
                        &mut committed,
                    );
                }
            }
            AgentEventKind::TurnCancelled | AgentEventKind::TurnFailed { .. } => {
                if !calls.is_empty() {
                    flush_tool_batch(
                        &mut history,
                        &mut content,
                        &mut calls,
                        &mut results,
                        &mut committed,
                    );
                } else if let Some(message) = committed.take() {
                    history.push(message);
                }
                if !turn_had_output {
                    history.truncate(turn_start_len);
                }
            }
            _ => {}
        }
    }
    history
}

fn replay_stored_native(data_root: &Path, stored: &StoredSession) -> Result<NativeReplay, String> {
    let checkpoint_store = CheckpointFileStore::new(data_root.join("sessions"))
        .map_err(|error| format!("No se pudo abrir checkpoints: {error}"))?;
    let expected = stored
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            AgentEventKind::CheckpointCommitted { reference } => {
                Some(reference.compatibility.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| CheckpointCompatibility {
            provider: String::new(),
            model: String::new(),
            context_format_key: String::new(),
        });
    replay_native(
        &stored.session.id,
        &stored.events,
        Some(&checkpoint_store),
        &expected,
    )
    .map_err(|error| format!("No se pudo reconstruir la sesión nativa: {error}"))
}

fn recovery_status(recovery: &ReplayRecovery) -> &'static str {
    match recovery {
        ReplayRecovery::Idle => "Sesión cargada",
        ReplayRecovery::ToolOutcomeUncertain { .. } => {
            "Herramienta interrumpida: pudo haberse ejecutado; requiere resolución antes de continuar"
        }
        ReplayRecovery::PermissionPending { .. } => {
            "Permiso pendiente en turno interrumpido; requiere recuperación antes de continuar"
        }
        ReplayRecovery::AdmittedNotStarted => "Turno admitido pendiente de recuperación",
        ReplayRecovery::ModelInFlight { .. } => "Modelo interrumpido; requiere recuperación",
        ReplayRecovery::ToolReady { .. } => "Herramienta pendiente de recuperación",
        ReplayRecovery::ResponseCommitted { .. } => "Respuesta pendiente de cierre",
        ReplayRecovery::ReadyForModel { .. } => "Turno pendiente de recuperación",
        ReplayRecovery::AbortedToolBatch { .. } => {
            "Turno abortado con herramientas pendientes; requiere resolución"
        }
    }
}

fn native_session_views(
    data_root: &Path,
    stored: &StoredSession,
) -> (Option<SessionRecoveryView>, Option<SessionPlanView>) {
    if stored.session.backend != "native" || stored.events.is_empty() {
        return (None, None);
    }
    match replay_stored_native(data_root, stored) {
        Ok(replay) => {
            let recovery = match &replay.recovery {
                ReplayRecovery::Idle => None,
                ReplayRecovery::PermissionPending { .. } => Some(SessionRecoveryView::Permission),
                ReplayRecovery::ToolOutcomeUncertain { .. } => {
                    Some(SessionRecoveryView::VerifyTool)
                }
                ReplayRecovery::AbortedToolBatch { .. } => Some(SessionRecoveryView::ReviewBatch),
                _ => Some(SessionRecoveryView::Resume),
            };
            let last_plan_sequence = stored.events.iter().rev().find_map(|event| {
                matches!(event.kind, AgentEventKind::PlanProposed { .. }).then_some(event.sequence)
            });
            let can_build = last_plan_sequence.is_some_and(|sequence| {
                recovery.is_none()
                    && !stored.events.iter().any(|event| {
                        event.sequence > sequence
                            && matches!(
                                event.kind,
                                AgentEventKind::TurnAdmitted {
                                    mode: TurnMode::Build,
                                    ..
                                } | AgentEventKind::TurnAdmittedWithImages {
                                    mode: TurnMode::Build,
                                    ..
                                }
                            )
                    })
            });
            let plan = replay.latest_plan.map(|snapshot| SessionPlanView {
                snapshot,
                can_build,
            });
            (recovery, plan)
        }
        Err(error) => (
            Some(SessionRecoveryView::InvalidJournal(
                error.chars().take(160).collect(),
            )),
            None,
        ),
    }
}

impl AgentsWindow {
    fn draft_key(&self) -> (String, Option<String>) {
        (
            self.view.workspace.clone(),
            self.view.selected_session.clone(),
        )
    }

    fn set_composer_goal(&mut self, objective: Option<String>) {
        if self.view.running || self.view.background_run_selected {
            self.view.status = "Espera a que termine la tarea para cambiar la meta".to_owned();
            return;
        }
        let objective = objective.map(|value| value.trim().to_owned());
        if objective
            .as_ref()
            .is_some_and(|value| value.len() > MAX_COMPOSER_GOAL_BYTES)
        {
            self.view.status = format!("La meta no puede superar {MAX_COMPOSER_GOAL_BYTES} bytes");
            return;
        }
        let objective = objective.filter(|value| !value.is_empty());
        let key = self.draft_key();
        let mut goals = self.composer_goals.clone();
        if let Some(value) = &objective {
            goals.insert(key, value.clone());
        } else {
            goals.remove(&key);
        }
        if let Err(error) = save_composer_goals(&self.data_root, &goals) {
            self.view.status = error;
            return;
        }
        self.composer_goals = goals;
        self.view.composer_goal = objective.clone();
        self.view.composer_goal_draft = objective.unwrap_or_default();
        self.view.composer_goal_editor_open = false;
        self.view.composer_add_menu_open = false;
        self.tree
            .set_input_value("composer_goal_draft", self.view.composer_goal_draft.clone());
        self.view.status = if self.view.composer_goal.is_some() {
            "Meta guardada para esta tarea".to_owned()
        } else {
            "Meta eliminada de esta tarea".to_owned()
        };
    }

    fn transfer_draft_goal_to_session(
        &mut self,
        from: &(String, Option<String>),
        session_id: &str,
    ) -> Result<(), String> {
        let Some(objective) = self.composer_goals.get(from).cloned() else {
            return Ok(());
        };
        let mut goals = self.composer_goals.clone();
        goals.remove(from);
        goals.insert((from.0.clone(), Some(session_id.to_owned())), objective);
        save_composer_goals(&self.data_root, &goals)?;
        self.composer_goals = goals;
        Ok(())
    }

    fn save_draft(&mut self) {
        let key = self.draft_key();
        let attachments_changed = self
            .attachment_drafts
            .get(&key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            != self.view.composer_attachments.as_slice();
        if self.view.composer_attachments.is_empty() {
            self.attachment_drafts.remove(&key);
        } else {
            self.attachment_drafts
                .insert(key.clone(), self.view.composer_attachments.clone());
        }
        let changed =
            self.drafts.get(&key).map(String::as_str).unwrap_or("") != self.view.prompt.as_str();
        if !changed && !attachments_changed && self.draft_dirty_at.is_none() {
            return;
        }
        if self.view.prompt.is_empty() {
            self.drafts.remove(&key);
        } else {
            self.drafts.insert(key, self.view.prompt.clone());
        }
        self.draft_dirty_at = None;
        if let Some(store) = &self.store {
            let result = store.save_draft(
                Path::new(&self.view.workspace),
                self.view.selected_session.as_deref(),
                &self.view.prompt,
            );
            match result {
                Ok(()) => {
                    self.draft_error = self
                        .view
                        .selected_session
                        .as_deref()
                        .map(|session_id| {
                            sync_native_steer_draft(
                                &self.data_root,
                                Path::new(&self.view.workspace),
                                session_id,
                                &self.view.prompt,
                                &self.view.composer_attachments,
                            )
                        })
                        .transpose()
                        .err();
                }
                Err(error) => self.draft_error = Some(error.to_string()),
            }
        }
    }

    fn restore_draft(&mut self) {
        let key = self.draft_key();
        self.view.composer_goal = self.composer_goals.get(&key).cloned();
        self.view.composer_goal_draft = self.view.composer_goal.clone().unwrap_or_default();
        self.view.composer_goal_editor_open = false;
        self.tree
            .set_input_value("composer_goal_draft", self.view.composer_goal_draft.clone());
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
                    self.drafts.insert(key.clone(), prompt.clone());
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
        let mut prompt = prompt;
        if let Some(session_id) = key.1.as_deref() {
            match load_native_steer_draft(&self.data_root, session_id) {
                Ok(Some(state)) if state.workspace == key.0 => {
                    if state.retry.as_ref().is_some_and(|retry| retry.acknowledged) {
                        let old_draft = state.prompt == prompt;
                        let cleanup = if old_draft {
                            match &self.store {
                                Some(store) => store
                                    .clear_draft(Path::new(&key.0), Some(session_id))
                                    .map(|_| ())
                                    .map_err(|error| error.to_string()),
                                None => Err("El historial no está disponible".to_owned()),
                            }
                        } else {
                            Ok(())
                        };
                        self.draft_error = cleanup
                            .and_then(|_| remove_native_steer_draft(&self.data_root, session_id))
                            .err();
                        if old_draft {
                            self.drafts.remove(&key);
                            self.attachment_drafts.remove(&key);
                            self.view.composer_attachments.clear();
                            prompt.clear();
                        }
                    } else if state.prompt == prompt && self.view.composer_attachments.is_empty() {
                        self.view.composer_attachments =
                            state.attachments.into_iter().map(Into::into).collect();
                        if !self.view.composer_attachments.is_empty() {
                            self.attachment_drafts
                                .insert(key.clone(), self.view.composer_attachments.clone());
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => self.draft_error = Some(error),
            }
        }
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
                Ok(_) => {
                    self.draft_error = key
                        .1
                        .as_deref()
                        .map(|session_id| remove_native_steer_draft(&self.data_root, session_id))
                        .transpose()
                        .err();
                }
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
                let active_run_resolved = self
                    .active_run
                    .as_ref()
                    .is_some_and(|run| run.resolve_permission(request_id, choice));
                let active_resume_resolved = self
                    .active_resume
                    .as_ref()
                    .is_some_and(|resume| resume.resolve_permission(request_id, choice));
                if active_run_resolved || active_resume_resolved {
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
                if let Some(resume) = &self.active_resume {
                    resume.resolve_permission(request_id, PermissionChoice::Deny);
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
                if let Some(resume) = &self.active_resume {
                    resume.resolve_permission(request_id, PermissionChoice::Deny);
                }
                self.view
                    .pending_permissions
                    .retain(|permission| permission.request_id != request_id);
                self.view.status = "El historial no está disponible".to_owned();
            }
        }
    }

    fn answer_permission_for_session(&mut self, request_id: &str) {
        if !self
            .view
            .pending_permissions
            .iter()
            .any(|permission| permission.request_id == request_id)
        {
            self.view.status = "La solicitud de permiso ya no está activa".to_owned();
            return;
        }
        if self.store.is_none() || self.view.selected_session.is_none() {
            self.view.status = "El historial no está disponible para guardar el permiso".to_owned();
            return;
        }
        let choice = PermissionChoice::AllowSession;
        let resolved = self
            .active_run
            .as_ref()
            .is_some_and(|run| run.resolve_permission(request_id, choice))
            || self
                .active_resume
                .as_ref()
                .is_some_and(|resume| resume.resolve_permission(request_id, choice));
        if resolved {
            self.view
                .pending_permissions
                .retain(|permission| permission.request_id != request_id);
            self.view.status =
                "El permiso para esta sesión se guardará antes de ejecutar".to_owned();
        } else {
            self.view.status = "La solicitud de permiso ya no está activa".to_owned();
        }
    }

    fn answer_subagent_permission(
        &mut self,
        child_id: &str,
        request_id: &str,
        choice: PermissionChoice,
    ) {
        if self
            .pending_child_recovery_permission
            .as_ref()
            .is_some_and(|pending| {
                pending.child_session_id == child_id && pending.request_id == request_id
            })
        {
            let Some(active) = self.active_child_recovery.as_ref() else {
                self.view.status = "La recuperación del subagente ya no está activa".to_owned();
                return;
            };
            if active.child_id != child_id {
                self.view.status =
                    "El permiso no pertenece al subagente en recuperación".to_owned();
                return;
            }
            if choice != PermissionChoice::AllowSession {
                let Some(store) = self.store.as_ref() else {
                    self.view.status = "El historial no está disponible".to_owned();
                    return;
                };
                let decision = if choice == PermissionChoice::Allow {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny
                };
                if let Err(error) = store.record_permission_decision(child_id, request_id, decision)
                {
                    self.persistence_error = Some(error.to_string());
                    self.show_persistence_error();
                    return;
                }
            }
            let queued = active.resolve_permission(request_id, choice);
            self.pending_child_recovery_permission = None;
            self.rebuild_live_subagent_views();
            self.view.status = if queued {
                if choice == PermissionChoice::AllowSession {
                    "Guardando permiso para esta sesión; reanudando subagente".to_owned()
                } else {
                    "Decisión guardada; reanudando subagente".to_owned()
                }
            } else {
                "La recuperación del subagente ya no acepta esta decisión".to_owned()
            };
            return;
        }
        if !self
            .view
            .pending_subagent_permissions
            .iter()
            .any(|permission| {
                permission.child_session_id == child_id && permission.request_id == request_id
            })
        {
            self.view.status = "La solicitud del subagente ya no está activa".to_owned();
            return;
        }
        if choice == PermissionChoice::AllowSession {
            if self
                .subagent_bridge
                .resolve_permission(child_id, request_id, choice)
            {
                self.view.status = "Guardando permiso del subagente para esta sesión".to_owned();
                self.view.pending_subagent_permissions.retain(|permission| {
                    permission.child_session_id != child_id || permission.request_id != request_id
                });
            } else {
                self.view.status = "La solicitud del subagente ya no está activa".to_owned();
            }
            return;
        }
        let allow = choice == PermissionChoice::Allow;
        let decision = if allow {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Deny
        };
        let recorded = self
            .store
            .as_ref()
            .map(|store| store.record_permission_decision(child_id, request_id, decision));
        match recorded {
            Some(Ok(_)) => {
                if self
                    .subagent_bridge
                    .resolve_permission(child_id, request_id, choice)
                {
                    self.view.status = if allow {
                        "Permiso concedido al subagente para esta llamada".to_owned()
                    } else {
                        "Permiso denegado al subagente para esta llamada".to_owned()
                    };
                } else {
                    self.view.status = "La solicitud del subagente ya no está activa".to_owned();
                }
            }
            Some(Err(error)) => {
                self.subagent_bridge.resolve_permission(
                    child_id,
                    request_id,
                    PermissionChoice::Deny,
                );
                self.persistence_error = Some(error.to_string());
                self.show_persistence_error();
            }
            None => {
                self.subagent_bridge.resolve_permission(
                    child_id,
                    request_id,
                    PermissionChoice::Deny,
                );
                self.view.status = "El historial no está disponible".to_owned();
            }
        }
        self.view.pending_subagent_permissions.retain(|permission| {
            permission.child_session_id != child_id || permission.request_id != request_id
        });
    }

    fn cancel_subagent(&mut self, child_id: &str) {
        if self.subagent_bridge.cancel_child(child_id) {
            self.view
                .pending_subagent_permissions
                .retain(|permission| permission.child_session_id != child_id);
            self.view.status = "Cancelando subagente...".to_owned();
        } else {
            self.view.status = "El subagente ya no está en ejecución".to_owned();
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
        self.view.dock_recently_closed = self
            .dock
            .recently_closed(&scope)
            .into_iter()
            .map(|entry| DockRecentlyClosedView {
                id: entry.tab.id.get(),
                title: entry.tab.title(),
                kind: match entry.tab.kind {
                    DockTabKind::Context => SideTab::Context,
                    DockTabKind::Files => SideTab::Files,
                    DockTabKind::Activity => SideTab::Activity,
                    DockTabKind::CodeReview => SideTab::CodeReview,
                    DockTabKind::Terminal => SideTab::Terminal,
                    DockTabKind::FilePreview { .. } => SideTab::FilePreview,
                },
                restorable: entry.restorable,
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
        let (exec_policy_settings, exec_policy_load_error) = match ExecPolicySettings::load(&root) {
            Ok(settings) => (settings, None),
            Err(error) => (ExecPolicySettings::default(), Some(error)),
        };
        let sidebar_preferences = SidebarPreferences::load(&root);
        let mut composer_preferences = ComposerPreferences::load(&root);
        let native_settings = NativeProviderSettings::load(&root);
        if root.join("native-provider.json").exists() {
            let mut choice = composer_preferences.choice("native", &native_settings.model_id);
            choice.model = native_settings.model_id.clone();
            composer_preferences.set("native", choice);
        }
        let (catalog, catalog_load_error) = match model_catalog::load_cache(&root) {
            Ok(Some(snapshot)) => (Some(snapshot), None),
            Ok(None) => (Some(model_catalog::reviewed_seed_snapshot()), None),
            Err(error) => (Some(model_catalog::reviewed_seed_snapshot()), Some(error)),
        };
        let catalog_should_refresh = catalog
            .as_ref()
            .is_none_or(|snapshot| snapshot.is_stale(Duration::from_secs(3_600)));
        let catalog_status = catalog_load_error.map_or_else(
            || {
                catalog.as_ref().map_or_else(
                    || "Sin catálogo local; consultando proveedores".to_owned(),
                    catalog_status,
                )
            },
            |error| format!("Caché local inválida ({error}); usando base revisada"),
        );
        let workspace = settings_preferences
            .startup_workspace()
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let (native_skills, native_skills_load_error) =
            match NativeSkillsSettings::load(&root, Path::new(&workspace)) {
                Ok(settings) => (NativeSkillsUiState::from_settings(&settings, &root), None),
                Err(error) => (
                    NativeSkillsUiState::with_load_error(
                        Path::new(&workspace),
                        &root,
                        error.clone(),
                    ),
                    Some(error),
                ),
            };
        let (mcp_settings, mcp_load_error) = match McpSettings::load(&root, Path::new(&workspace)) {
            Ok(settings) => (settings, None),
            Err(error) => (McpSettings::default(), Some(error)),
        };
        let (store, status) = match SessionStore::open(root.join("sessions")) {
            Ok(store) => match store.recover_pending_native_deletions() {
                Ok(0) => (Some(Arc::new(store)), "Listo".to_owned()),
                Ok(count) => (
                    Some(Arc::new(store)),
                    format!("Se completaron {count} borrados nativos interrumpidos"),
                ),
                Err(error) => (
                    None,
                    format!("No se pudo recuperar un borrado nativo pendiente: {error}"),
                ),
            },
            Err(error) => (None, format!("No se pudo abrir el historial: {error}")),
        };
        let (composer_goals, composer_goals_error) = match load_composer_goals(&root) {
            Ok(goals) => (goals, None),
            Err(error) => (HashMap::new(), Some(error)),
        };
        let profiles = load_profiles(&root);
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
        let selected_backend = composer_preferences
            .selected_backend()
            .filter(|preferred| {
                backend_options
                    .iter()
                    .any(|(available, _)| available == preferred)
            })
            .map(str::to_owned)
            .unwrap_or_else(|| "native".to_owned());
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
        let mut selected_routes = composer_selection::model_route_options_with_discovery_at(
            catalog.as_ref(),
            &native_settings,
            &[],
            &root,
        )
        .unwrap_or_else(|_| {
            composer_selection::model_route_options(catalog.as_ref(), &native_settings)
        });
        selected_routes.retain(|route| {
            route.backend == "native"
                || backend_options
                    .iter()
                    .any(|(backend, _)| backend == &route.backend)
        });
        let selected_effort_options = selected_routes
            .iter()
            .find(|route| {
                route.backend == selected_backend
                    && route.model_id == selected_choice.model
                    && (selected_backend != "native"
                        || route.provider_id == native_settings.provider_id)
            })
            .map(|route| route.effort_options.clone())
            .unwrap_or_else(|| {
                composer_selection::effort_options_for_provider(
                    &selected_backend,
                    &selected_choice.model,
                    &native_settings.provider_id,
                )
            });
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
            composer_listening: false,
            composer_goal: None,
            composer_goal_editor_open: false,
            composer_goal_draft: String::new(),
            composer_project_menu_open: false,
            composer_project_options: composer_project_options(&workspace, &[]),
            endpoint: native_settings.custom_endpoint.clone(),
            model: native_choice.model,
            native_provider_id: native_settings.provider_id.clone(),
            hosted_web_search_mode: settings_preferences.hosted_web_search_mode,
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
            composer_routes: selected_routes,
            composer_provider_status: Default::default(),
            composer_model_menu_open: false,
            composer_model_list_open: false,
            composer_model_query: String::new(),
            composer_advanced_open: false,
            composer_effort: selected_effort,
            composer_effort_options: selected_effort_options,
            composer_effort_menu_open: false,
            composer_effort_preview: None,
            composer_effort_hovered: false,
            composer_context_percent: None,
            turn_mode: TurnMode::Build,
            sessions: Vec::new(),
            session_plans: Default::default(),
            session_recovery: Default::default(),
            session_approval_grants: Vec::new(),
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
            settings_notice: agent_tool_settings_error
                .clone()
                .or_else(|| exec_policy_load_error.clone())
                .or_else(|| native_skills_load_error.clone()),
            settings_notice_error: agent_tool_settings_error.is_some()
                || exec_policy_load_error.is_some()
                || native_skills_load_error.is_some(),
            native_key_available: native_key_available_at(&native_settings, &root),
            workspace_writes_enabled: agent_tool_settings.permissions().write_file
                && agent_tool_settings.permissions().run_command,
            native_tool_permissions: agent_tool_settings.permissions(),
            native_command_isolation: agent_tool_settings.command_isolation(),
            native_commands: Vec::new(),
            native_commands_error: None,
            pending_native_command_termination: None,
            native_command_terminating: None,
            native_exec_rules: {
                let mut state = ExecPolicyUiState::from_settings(&exec_policy_settings);
                state.load_error = exec_policy_load_error.clone();
                state
            },
            native_skills,
            native_hooks: HookUiState::from_load(NativeHookSettings::load(&root)),
            mcp_servers: mcp_settings.servers,
            mcp_selected_id: None,
            mcp_editor_open: false,
            pending_mcp_delete_id: None,
            mcp_draft_http: false,
            mcp_draft_id: String::new(),
            mcp_draft_url: String::new(),
            mcp_draft_http_bearer_token_env_name: String::new(),
            mcp_draft_oauth_enabled: false,
            mcp_draft_oauth_client_id: String::new(),
            mcp_draft_oauth_issuer: String::new(),
            mcp_draft_oauth_scopes: String::new(),
            mcp_oauth_status: None,
            mcp_draft_program: String::new(),
            mcp_draft_args: "[]".to_owned(),
            mcp_draft_cwd: String::new(),
            mcp_draft_env_names: String::new(),
            mcp_load_error,
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
            command_search_loading: false,
            command_search_errors: Vec::new(),
            command_search_truncated: false,
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
            dock_recently_closed: Vec::new(),
            dock_overview_query: String::new(),
            show_dock_overview: false,
            show_dock_add_menu: false,
            file_preview: None,
            git_review: None,
            review_detail: None,
            terminal_snapshot: None,
            terminal_input: String::new(),
            terminal_error: None,
            pending_permissions: Vec::new(),
            pending_subagent_permissions: Vec::new(),
            active_subagents: Vec::new(),
            recoverable_subagent: None,
            pending_tool_verification: None,
            pending_batch_discard: None,
            tool_verification_output: String::new(),
            tool_verification_is_error: false,
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
        let subagent_bridge = Arc::new(SubagentEventBridge::new());
        let subagent_events = subagent_bridge.subscribe();
        let mut app = Self {
            data_root: root.clone(),
            view,
            composer_preferences,
            settings_preferences,
            agent_tool_settings,
            exec_policy_settings,
            exec_policy_load_error,
            native_command_sessions: NativeCommandSessions::default(),
            native_command_snapshot_response: None,
            native_command_termination_response: None,
            last_native_command_snapshot: None,
            tree,
            projection: TranscriptProjection::new(),
            profiles,
            store,
            known_project_paths: Vec::new(),
            active_run: None,
            dictation: None,
            dictation_key: None,
            active_compact: None,
            active_resume: None,
            active_child_recovery: None,
            pending_child_recovery_permission: None,
            active_mcp_oauth: None,
            subagent_bridge,
            subagent_events,
            last_subagent_poll: Instant::now(),
            subagent_poll_needed: false,
            automation_stores: Vec::new(),
            automation_last_poll: Instant::now() - Duration::from_secs(31),
            background_automation_runs: Vec::new(),
            automation_queue: VecDeque::new(),
            marketplace_response: None,
            workspace_folder_response: None,
            composer_folder_response: None,
            catalog_response: None,
            catalog_last_refresh: Instant::now(),
            model_discovery_response: None,
            model_discovery_pending: 0,
            model_discoveries: Vec::new(),
            codex_model_response: None,
            codex_models: Vec::new(),
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
            composer_goals,
            draft_dirty_at: None,
            draft_error: None,
            persistence_error: None,
            navigation,
            command_tree,
            command_center_attaches_file: false,
            command_focus_pending: false,
            prompt_focus_pending: false,
            composer_model_search_focus_pending: false,
            rename_focus_pending: false,
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
        if let Some(error) = composer_goals_error {
            app.view.status = error;
        }
        app.refresh_sessions();
        if catalog_should_refresh {
            app.begin_catalog_refresh();
        }
        #[cfg(not(test))]
        {
            app.begin_model_discovery();
            app.begin_codex_model_discovery();
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
                .map(|settings| settings.resolve_at(&self.data_root))
                .transpose()?;
            let native_provider_id = native_settings
                .as_ref()
                .map(|settings| settings.provider_id.as_str())
                .unwrap_or("");
            let instructions = if dispatch.backend == "native" {
                if native_connection
                    .as_ref()
                    .is_some_and(|connection| connection.api_key.is_none())
                    && native_provider_id != "custom"
                {
                    let key_env = native_provider::adapter_at(&self.data_root, native_provider_id)?
                        .map(|adapter| adapter.key_env)
                        .unwrap_or_else(|| "API_KEY".to_owned());
                    return Err(format!("Define {key_env} para ejecutar el agente nativo"));
                }
                native_workspace_instructions(&self.data_root, &dispatch.workspace_path)?
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
            let mut request = AgentRunRequest {
                session_id: Some(session.id.clone()),
                sequence_start: 0,
                workspace: dispatch.workspace_path.clone(),
                history: Vec::new(),
                prompt: dispatch.prompt,
                mode: TurnMode::Build,
            };
            let run = if dispatch.backend == "native" {
                let connection = native_connection.expect("validated native connection");
                let model_id = connection.model_id.clone();
                let endpoint = connection.endpoint.clone();
                let wire_api = connection.wire_api;
                let model = native_model_backend(
                    connection,
                    native_provider_id,
                    None,
                    self.view.hosted_web_search_mode,
                    &self.data_root,
                    self.view.catalog.as_deref(),
                    &self.model_discoveries,
                )?;
                let token_budget =
                    native_provider::verified_token_budget(native_provider_id, &model_id);
                let auto_checkpoints = token_budget
                    .map(|_| CheckpointFileStore::new(self.data_root.join("sessions")))
                    .transpose()
                    .map_err(|error| format!("No se pudo abrir checkpoints: {error}"))?
                    .map(Arc::new);
                let lease = store
                    .try_acquire_native_lease(&session.id)
                    .map_err(|error| format!("La sesión nativa ya está en uso: {error}"))?;
                let admission = store
                    .admit_native_turn(
                        &session.id,
                        &Uuid::new_v4().to_string(),
                        &request.prompt,
                        &model_id,
                        request.mode,
                    )
                    .map_err(|error| format!("No se pudo admitir el turno: {error}"))?;
                request.sequence_start = admission.sequence;
                let tools = Arc::new(LocalTools::new().with_write_access(false));
                let mut runner = AgentRunner::new(model, tools)
                    .with_permission_policy(PermissionPolicy::new())
                    .with_system_instructions(instructions)
                    .with_native_runtime_identity(native_provider_id, endpoint)
                    .with_event_journal(store.clone());
                if let (Some(budget), Some(checkpoints)) = (token_budget, auto_checkpoints) {
                    runner = runner
                        .with_token_budget_policy(budget)
                        .with_auto_compaction(
                            store.clone(),
                            checkpoints,
                            native_checkpoint_compatibility(
                                native_provider_id,
                                &model_id,
                                wire_api,
                            ),
                        );
                }
                ActiveRun::Native(runner.start_with_lease(request, lease))
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
            let native_run = matches!(
                &self.background_automation_runs[index].run,
                ActiveRun::Native(_)
            );
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
                if !native_run {
                    if let Some(store) = &self.store {
                        if let Err(error) = store.append_event(&event.session_id, &event) {
                            self.view.automations.error = Some(error.to_string());
                            self.background_automation_runs[index].run.cancel();
                        }
                    }
                }
                if self.view.selected_session.as_deref() == Some(event.session_id.as_str()) {
                    self.last_sequence = self.last_sequence.max(event.sequence);
                    if self.projection.apply_event(&event) {
                        self.view.transcript = self.projection.items().to_vec();
                    }
                    if native_run
                        && matches!(
                            &event.kind,
                            AgentEventKind::ModelResponseCommitted { .. }
                                | AgentEventKind::CheckpointCommitted { .. }
                        )
                    {
                        self.refresh_composer_context_percent();
                    }
                }
            }
            if disconnected {
                if let Some(error) = self.background_automation_runs[index].run.journal_error() {
                    self.view.automations.error = Some(error);
                }
                if native_run
                    && self.view.selected_session.as_deref()
                        == Some(self.background_automation_runs[index].session_id.as_str())
                {
                    if let Some(store) = &self.store {
                        if let Ok(Some(stored)) =
                            store.load(&self.background_automation_runs[index].session_id)
                        {
                            self.projection = TranscriptProjection::from_events(&stored.events);
                            self.view.transcript = self.projection.items().to_vec();
                        }
                    }
                }
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

    fn sync_automation_form_inputs(&mut self) {
        let form = &self.view.automations.form;
        let (once_date, once_time) = form.once_parts();
        for (id, value) in [
            ("automation_title", form.title.as_str()),
            ("automation_prompt", form.prompt.as_str()),
            ("automation_once_date", once_date),
            ("automation_once_time", once_time),
            ("automation_interval", form.interval_minutes.as_str()),
            ("automation_time", form.time.as_str()),
        ] {
            self.tree.set_input_value(id, value);
        }
    }

    fn handle_automation_action(&mut self, id: &str) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let current_native = self.native_settings();
        let state = &mut self.view.automations;
        if let Some(date) = DatePicker::selected_date(id, "automation_once_date") {
            state.form.set_once_date(date);
            self.tree.set_input_value("automation_once_date", date);
            state.calendar_year = date[..4].parse().unwrap_or(state.calendar_year);
            state.calendar_month = date[5..7].parse().unwrap_or(state.calendar_month);
            state.error = None;
            return;
        }
        if id == "automation_once_date:month_prev" || id == "automation_once_date:month_next" {
            let delta = if id.ends_with("month_prev") { -1 } else { 1 };
            (state.calendar_year, state.calendar_month) =
                DatePicker::shift_month(state.calendar_year, state.calendar_month, delta);
            return;
        }
        if let Some(time) =
            TimePicker::adjusted_value(id, "automation_once_time", state.form.once_parts().1)
        {
            state.form.set_once_time(&time);
            self.tree.set_input_value("automation_once_time", time);
            state.error = None;
            return;
        }
        if let Some(time) = TimePicker::adjusted_value(id, "automation_time", &state.form.time) {
            state.form.time.clone_from(&time);
            self.tree.set_input_value("automation_time", time);
            state.error = None;
            return;
        }
        if let Some(filter) = AutomationFilter::from_action_id(id) {
            state.filter = filter;
            return;
        }
        if let Some(frequency) = AutomationFrequency::from_action_id(id) {
            state.form.frequency = frequency;
            state.error = None;
            return;
        }
        if let Some(step) = crate::automation_view::AutomationEditorStep::from_action_id(id) {
            if state.screen == AutomationScreen::Edit && step.index() <= state.editor_step.index() {
                state.editor_step = step;
                state.error = None;
            }
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
                let mut form =
                    crate::automation_view::AutomationForm::new(&self.view.selected_backend);
                if form.backend == "native" {
                    form.native_provider = Some(current_native);
                }
                state.open_editor(form, None);
                self.sync_automation_form_inputs();
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
            "automation_previous" => {
                if state.screen == AutomationScreen::Edit {
                    state.editor_step = state.editor_step.previous();
                    state.error = None;
                }
                return;
            }
            "automation_next" => {
                if state.screen == AutomationScreen::Edit {
                    match state.form.validate_step(state.editor_step, now_ms) {
                        Ok(()) => {
                            state.editor_step = state.editor_step.next();
                            state.error = None;
                        }
                        Err(error) => state.error = Some(error.to_string()),
                    }
                }
                return;
            }
            "automation_save" => {
                if state.screen != AutomationScreen::Edit {
                    return;
                }
                for step in [
                    crate::automation_view::AutomationEditorStep::Content,
                    crate::automation_view::AutomationEditorStep::Schedule,
                ] {
                    if let Err(error) = state.form.validate_step(step, now_ms) {
                        state.editor_step = step;
                        state.error = Some(error.to_string());
                        return;
                    }
                }
                if state.editor_step != crate::automation_view::AutomationEditorStep::Agent {
                    state.editor_step = crate::automation_view::AutomationEditorStep::Agent;
                    return;
                }
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
                let mut form = form;
                if form.backend == "native" {
                    form.native_provider = Some(current_native);
                }
                state.open_editor(form, None);
                self.sync_automation_form_inputs();
            }
        } else if let Some(selected_id) = id.strip_prefix("automation_edit:") {
            if let Some(item) = state.items.iter().find(|item| item.id == selected_id) {
                let form = crate::automation_view::AutomationForm::from_automation(item);
                state.open_editor(form, Some(selected_id.to_owned()));
                self.sync_automation_form_inputs();
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
            "marketplace_add_menu_dismiss" => state.add_menu_open = false,
            "marketplace_source_kind:local" => {
                state.add_source_kind = Some(SourceKind::Local);
                state.add_menu_open = false;
                state.source_draft.clear();
                state.status = None;
            }
            "marketplace_source_kind:github" => {
                state.add_source_kind = Some(SourceKind::Github);
                state.add_menu_open = false;
                state.source_draft.clear();
                state.status = None;
            }
            "marketplace_source_cancel" => {
                if !state.busy {
                    state.add_source_kind = None;
                    state.source_draft.clear();
                    state.status = None;
                }
            }
            "marketplace_alert_cancel" => {
                state.pending_uninstall_id = None;
                state.pending_remove_source_id = None;
            }
            "marketplace_source_submit" => {
                if state.busy {
                    return;
                }
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
                    if !state.busy
                        && state
                            .snapshot
                            .items
                            .iter()
                            .any(|entry| entry.id == item && entry.installed_scope.is_some())
                    {
                        state.pending_uninstall_id = Some(item.to_owned());
                    }
                } else if let Some(source) = id.strip_prefix("marketplace_remove_source:") {
                    if !state.busy
                        && state
                            .snapshot
                            .sources
                            .iter()
                            .any(|entry| entry.id == source)
                    {
                        state.pending_remove_source_id = Some(source.to_owned());
                    }
                } else if let Some(item) = id.strip_prefix("marketplace_uninstall_confirm:") {
                    if state.pending_uninstall_id.as_deref() == Some(item) && !state.busy {
                        state.pending_uninstall_id = None;
                        self.marketplace_operation(MarketplaceOperation::Uninstall(
                            item.to_owned(),
                        ));
                    }
                } else if let Some(source) = id.strip_prefix("marketplace_remove_source_confirm:") {
                    if state.pending_remove_source_id.as_deref() == Some(source) && !state.busy {
                        state.pending_remove_source_id = None;
                        self.marketplace_operation(MarketplaceOperation::RemoveSource(
                            source.to_owned(),
                        ));
                    }
                }
            }
        }
    }

    fn refresh_selected_session_approvals(&mut self) {
        self.view.session_approval_grants.clear();
        let (Some(store), Some(session_id)) =
            (self.store.as_ref(), self.view.selected_session.as_deref())
        else {
            return;
        };
        match store.session_approval_grants(session_id) {
            Ok(grants) => {
                let mut seen = HashSet::new();
                self.view.session_approval_grants = grants
                    .into_iter()
                    .rev()
                    .filter(|grant| seen.insert(grant.key_digest.clone()))
                    .take(32)
                    .map(|grant| SessionApprovalGrantView {
                        tool_name: grant.tool_name,
                        key_digest: grant.key_digest,
                    })
                    .collect();
            }
            Err(error) => {
                self.view.status = format!("No se pudieron leer los permisos de sesión: {error}");
            }
        }
    }

    fn refresh_sessions(&mut self) {
        if let Some(store) = self.store.clone() {
            match store.list_with_diagnostics() {
                Ok(listing) => {
                    self.view.sessions_status = (!listing.diagnostics.is_empty()).then(|| {
                        format!("{} sesiones no se pudieron leer", listing.diagnostics.len())
                    });
                    let mut recoveries = BTreeMap::new();
                    let mut plans = BTreeMap::new();
                    for record in &listing.sessions {
                        let (recovery, plan) = native_session_views(&self.data_root, record);
                        if let Some(recovery) = recovery {
                            let selected_running = self.view.running
                                && self.view.selected_session.as_deref()
                                    == Some(record.session.id.as_str());
                            let background_running = self
                                .background_automation_runs
                                .iter()
                                .any(|run| run.session_id == record.session.id);
                            if !selected_running && !background_running {
                                recoveries.insert(record.session.id.clone(), recovery);
                            }
                        }
                        if let Some(plan) = plan {
                            plans.insert(record.session.id.clone(), plan);
                        }
                    }
                    self.view.session_recovery = recoveries;
                    self.view.session_plans = plans;
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
                    let parents_with_children = listing
                        .sessions
                        .iter()
                        .filter_map(|record| {
                            record
                                .session
                                .subagent
                                .as_ref()
                                .map(|link| link.parent_session_id.clone())
                        })
                        .collect::<BTreeSet<_>>();
                    let mut child_statuses = HashMap::new();
                    if !parents_with_children.is_empty() {
                        match SubagentCoordinator::new(store.clone(), SubagentLimits::default()) {
                            Ok(coordinator) => {
                                for parent_id in parents_with_children {
                                    match coordinator.list_children_for_ui(&parent_id) {
                                        Ok(children) => {
                                            for child in children {
                                                child_statuses
                                                    .insert(child.session.id, child.status);
                                            }
                                        }
                                        Err(error) => {
                                            self.view.sessions_status = Some(format!(
                                                "No se pudo leer el estado de subagentes: {error}"
                                            ));
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                self.view.sessions_status = Some(format!(
                                    "No se pudo abrir la coordinación de subagentes: {error}"
                                ));
                            }
                        }
                    }
                    self.subagent_poll_needed = child_statuses.values().any(|status| {
                        matches!(status, SubagentStatus::Ready | SubagentStatus::Running)
                    });
                    self.view.recoverable_subagent = self
                        .view
                        .selected_session
                        .as_ref()
                        .filter(|id| child_statuses.get(*id) == Some(&SubagentStatus::Interrupted))
                        .and_then(|id| {
                            listing
                                .sessions
                                .iter()
                                .find(|record| &record.session.id == id)
                        })
                        .and_then(|record| {
                            let link = record.session.subagent.as_ref()?;
                            let title = record
                                .session
                                .title
                                .clone()
                                .or_else(|| {
                                    record.events.iter().find_map(|event| match &event.kind {
                                        AgentEventKind::TurnAdmitted { prompt, .. }
                                        | AgentEventKind::TurnAdmittedWithImages {
                                            prompt, ..
                                        } => {
                                            Some(prompt.lines().next().unwrap_or(prompt).to_owned())
                                        }
                                        _ => None,
                                    })
                                })
                                .unwrap_or_else(|| "Subagente".to_owned());
                            Some(SubagentRunView {
                                child_session_id: record.session.id.clone(),
                                parent_session_id: link.parent_session_id.clone(),
                                title,
                            })
                        });
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
                                if let AgentEventKind::TurnAdmitted { prompt, .. }
                                | AgentEventKind::TurnAdmittedWithImages { prompt, .. }
                                | AgentEventKind::TurnStarted { prompt } = &event.kind
                                {
                                    Some(prompt.lines().next().unwrap_or(prompt).trim().to_owned())
                                } else {
                                    None
                                }
                            });
                            let session = record.session;
                            let is_subagent = session.subagent.is_some();
                            let subagent_status = child_statuses
                                .get(&session.id)
                                .map(subagent_status_label)
                                .unwrap_or("estado desconocido");
                            let title = session
                                .title
                                .or_else(|| title.filter(|text| !text.is_empty()))
                                .unwrap_or_else(|| "Nueva tarea".to_owned());
                            (
                                activity_at_ms,
                                session.created_at_ms,
                                SessionView {
                                    id: session.id,
                                    title: if is_subagent {
                                        format!("↳ {title} · {subagent_status}")
                                    } else {
                                        title
                                    },
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
                    self.subagent_poll_needed = false;
                    self.view.recoverable_subagent = None;
                    self.view.session_plans.clear();
                    self.view.session_recovery.clear();
                    self.view.sessions_status = Some(format!("Error al leer sesiones: {error}"));
                    self.view.status = format!("Error al leer sesiones: {error}");
                }
            }
        }
        self.refresh_composer_projects();
        self.refresh_selected_session_approvals();
    }

    fn poll_subagent_sessions(&mut self) -> bool {
        if !self.subagent_poll_needed
            || self.last_subagent_poll.elapsed() < Duration::from_millis(500)
        {
            return false;
        }
        self.last_subagent_poll = Instant::now();
        self.refresh_sessions();
        true
    }

    fn rebuild_live_subagent_views(&mut self) {
        let Some(store) = self.store.as_ref() else {
            self.view.active_subagents.clear();
            self.view.pending_subagent_permissions.clear();
            return;
        };
        let mut active = Vec::new();
        let mut permissions = Vec::new();
        for (child_id, live_requests) in self.subagent_bridge.live_snapshot() {
            let stored = match store.load(&child_id) {
                Ok(Some(stored)) => stored,
                Ok(None) | Err(_) => {
                    for request_id in live_requests {
                        self.subagent_bridge.resolve_permission(
                            &child_id,
                            &request_id,
                            PermissionChoice::Deny,
                        );
                    }
                    self.view.status =
                        format!("No se pudo leer el historial del subagente {child_id}");
                    continue;
                }
            };
            let Some(link) = stored.session.subagent.as_ref() else {
                for request_id in live_requests {
                    self.subagent_bridge.resolve_permission(
                        &child_id,
                        &request_id,
                        PermissionChoice::Deny,
                    );
                }
                continue;
            };
            let title = stored
                .session
                .title
                .clone()
                .or_else(|| {
                    stored.events.iter().find_map(|event| match &event.kind {
                        AgentEventKind::TurnAdmitted { prompt, .. }
                        | AgentEventKind::TurnAdmittedWithImages { prompt, .. }
                        | AgentEventKind::TurnStarted { prompt } => {
                            Some(prompt.lines().next().unwrap_or(prompt).trim().to_owned())
                        }
                        _ => None,
                    })
                })
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| "Subagente".to_owned());
            active.push(SubagentRunView {
                child_session_id: child_id.clone(),
                parent_session_id: link.parent_session_id.clone(),
                title,
            });
            let live_requests: HashSet<_> = live_requests.into_iter().collect();
            if live_requests.is_empty() {
                continue;
            }
            let mut pending = HashMap::new();
            for event in &stored.events {
                match &event.kind {
                    AgentEventKind::PermissionRequested {
                        request_id,
                        tool_call,
                    } => {
                        pending.insert(request_id.clone(), tool_call);
                    }
                    AgentEventKind::PermissionResolved { request_id, .. } => {
                        pending.remove(request_id);
                    }
                    _ => {}
                }
            }
            for (request_id, tool_call) in pending {
                if live_requests.contains(&request_id) {
                    permissions.push(SubagentPermissionView {
                        child_session_id: child_id.clone(),
                        parent_session_id: link.parent_session_id.clone(),
                        request_id,
                        tool_name: tool_call.name.clone(),
                        arguments: serde_json::to_string_pretty(&tool_call.arguments)
                            .unwrap_or_default(),
                    });
                }
            }
        }
        active.sort_by(|left, right| left.child_session_id.cmp(&right.child_session_id));
        permissions.sort_by(|left, right| {
            left.child_session_id
                .cmp(&right.child_session_id)
                .then_with(|| left.request_id.cmp(&right.request_id))
        });
        if let Some(pending) = &self.pending_child_recovery_permission {
            permissions.push(pending.clone());
        }
        self.view.active_subagents = active;
        self.view.pending_subagent_permissions = permissions;
    }

    fn poll_subagent_events(&mut self) -> bool {
        let mut changed = false;
        let mut refresh_status = false;
        let mut selected_child_changed = false;
        loop {
            match self.subagent_events.try_recv() {
                Ok(SubagentBridgeEvent::Event {
                    child_session_id,
                    event,
                }) => {
                    if event.session_id != child_session_id {
                        continue;
                    }
                    changed = true;
                    selected_child_changed |=
                        self.view.selected_session.as_deref() == Some(child_session_id.as_str());
                    refresh_status |= matches!(
                        event.kind,
                        AgentEventKind::TurnStarted { .. }
                            | AgentEventKind::TurnCompleted { .. }
                            | AgentEventKind::TurnFailed { .. }
                            | AgentEventKind::TurnCancelled
                    );
                }
                Ok(SubagentBridgeEvent::Closed { .. }) => {
                    changed = true;
                    refresh_status = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.subagent_events = self.subagent_bridge.subscribe();
                    changed = true;
                    break;
                }
            }
        }
        if changed {
            self.rebuild_live_subagent_views();
            if selected_child_changed {
                if let (Some(store), Some(child_id)) = (&self.store, &self.view.selected_session) {
                    if let Ok(Some(stored)) = store.load(child_id) {
                        self.last_sequence = stored.events.last().map_or(0, |event| event.sequence);
                        self.projection = TranscriptProjection::from_events(&stored.events);
                        self.view.transcript = self.projection.items().to_vec();
                    }
                }
            }
            if refresh_status {
                self.refresh_sessions();
            }
        }
        changed
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

    fn select_command_hook(&mut self, id: &str) {
        let Some(hook) = self
            .view
            .native_hooks
            .hooks
            .iter()
            .find(|hook| hook.id == id)
            .cloned()
        else {
            self.set_settings_notice("Hook no encontrado", true);
            return;
        };
        let draft = &mut self.view.native_hooks;
        draft.clear_draft();
        draft.selected_id = Some(hook.id.clone());
        draft.draft_id = hook.id;
        draft.draft_phase = hook.phase;
        draft.draft_tool_name = hook.tool_name;
        draft.draft_executable = hook.executable.to_string_lossy().into_owned();
        draft.draft_args = serde_json::to_string_pretty(&hook.args).unwrap_or_else(|_| "[]".into());
        draft.draft_cwd = hook.cwd.to_string_lossy().into_owned();
        draft.draft_timeout_ms = hook.timeout_ms.to_string();
        draft.draft_max_output_bytes = hook.max_output_bytes.to_string();
        draft.editor_open = true;
        self.clear_settings_notice();
    }

    fn commit_command_hooks(
        &mut self,
        hooks: Vec<openide_harness::CommandHookConfig>,
        message: &str,
    ) -> bool {
        if let Some(error) = &self.view.native_hooks.load_error {
            self.set_settings_notice(
                format!("Corrige la configuración de hooks antes de guardarla: {error}"),
                true,
            );
            return false;
        }
        let settings = NativeHookSettings { hooks };
        match settings.save(&self.data_root) {
            Ok(()) => {
                self.view.native_hooks.hooks = settings.hooks;
                self.set_settings_notice(message, false);
                true
            }
            Err(error) => {
                self.set_settings_notice(error, true);
                false
            }
        }
    }

    fn save_command_hook_draft(&mut self) {
        let draft = &self.view.native_hooks;
        let args: Vec<String> = match serde_json::from_str(&draft.draft_args) {
            Ok(args) => args,
            Err(error) => {
                self.set_settings_notice(format!("Argumentos del hook no válidos: {error}"), true);
                return;
            }
        };
        let timeout_ms = match draft.draft_timeout_ms.trim().parse() {
            Ok(value) => value,
            Err(_) => {
                self.set_settings_notice(
                    "El límite de tiempo debe ser un entero en milisegundos",
                    true,
                );
                return;
            }
        };
        let max_output_bytes = match draft.draft_max_output_bytes.trim().parse() {
            Ok(value) => value,
            Err(_) => {
                self.set_settings_notice("El límite de salida debe ser un entero en bytes", true);
                return;
            }
        };
        let hook = openide_harness::CommandHookConfig {
            id: draft.draft_id.trim().to_owned(),
            phase: draft.draft_phase,
            tool_name: draft.draft_tool_name.trim().to_owned(),
            executable: PathBuf::from(draft.draft_executable.trim()),
            args,
            cwd: PathBuf::from(draft.draft_cwd.trim()),
            timeout_ms,
            max_output_bytes,
        };
        let mut hooks = draft.hooks.clone();
        if let Some(index) = draft
            .selected_id
            .as_deref()
            .and_then(|id| hooks.iter().position(|item| item.id == id))
        {
            hooks[index] = hook;
        } else {
            hooks.push(hook);
        }
        if self.commit_command_hooks(hooks, "Hook guardado") {
            self.view.native_hooks.clear_draft();
        }
    }

    fn delete_command_hook(&mut self, id: &str) {
        let mut hooks = self.view.native_hooks.hooks.clone();
        let before = hooks.len();
        hooks.retain(|hook| hook.id != id);
        if hooks.len() == before {
            self.set_settings_notice("Hook no encontrado", true);
        } else if self.commit_command_hooks(hooks, "Hook eliminado") {
            self.view.native_hooks.clear_draft();
        }
    }

    fn clear_mcp_draft(&mut self) {
        self.cancel_mcp_oauth();
        self.view.mcp_selected_id = None;
        self.view.mcp_draft_http = false;
        self.view.mcp_draft_id.clear();
        self.view.mcp_draft_url.clear();
        self.view.mcp_draft_http_bearer_token_env_name.clear();
        self.view.mcp_draft_oauth_enabled = false;
        self.view.mcp_draft_oauth_client_id.clear();
        self.view.mcp_draft_oauth_issuer.clear();
        self.view.mcp_draft_oauth_scopes.clear();
        self.view.mcp_draft_program.clear();
        self.view.mcp_draft_args = "[]".to_owned();
        self.view.mcp_draft_cwd.clear();
        self.view.mcp_draft_env_names.clear();
    }

    fn refresh_mcp_settings(&mut self) {
        match McpSettings::load(&self.data_root, Path::new(&self.view.workspace)) {
            Ok(settings) => {
                self.view.mcp_servers = settings.servers;
                self.view.mcp_load_error = None;
            }
            Err(error) => {
                self.view.mcp_servers.clear();
                self.view.mcp_load_error = Some(error);
            }
        }
        self.clear_mcp_draft();
        self.view.mcp_editor_open = false;
        self.view.pending_mcp_delete_id = None;
    }

    fn select_mcp_server(&mut self, id: &str) {
        let Some(server) = self
            .view
            .mcp_servers
            .iter()
            .find(|server| server.id == id)
            .cloned()
        else {
            self.set_settings_notice("Servidor MCP no encontrado", true);
            return;
        };
        self.cancel_mcp_oauth();
        self.view.mcp_selected_id = Some(server.id.clone());
        self.view.mcp_draft_id = server.id;
        self.view.mcp_draft_program = server.program;
        self.view.mcp_draft_args =
            serde_json::to_string_pretty(&server.args).unwrap_or_else(|_| "[]".to_owned());
        self.view.mcp_draft_cwd = server
            .cwd
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.view.mcp_draft_env_names = server.credential_env_names.join("\n");
        self.view.mcp_draft_http = server.url.is_some();
        self.view.mcp_draft_url = server.url.unwrap_or_default();
        self.view.mcp_draft_http_bearer_token_env_name =
            server.http_bearer_token_env_name.unwrap_or_default();
        self.view.mcp_draft_oauth_enabled = server.http_oauth.is_some();
        if let Some(oauth) = server.http_oauth {
            self.view.mcp_draft_oauth_client_id = oauth.client_id;
            self.view.mcp_draft_oauth_issuer = oauth.issuer;
            self.view.mcp_draft_oauth_scopes = oauth.scopes.join("\n");
        } else {
            self.view.mcp_draft_oauth_client_id.clear();
            self.view.mcp_draft_oauth_issuer.clear();
            self.view.mcp_draft_oauth_scopes.clear();
        }
        self.clear_settings_notice();
        self.view.mcp_editor_open = true;
    }

    fn commit_mcp_settings(
        &mut self,
        servers: Vec<crate::mcp_settings::McpServerSettings>,
        message: &str,
    ) -> bool {
        if let Some(error) = &self.view.mcp_load_error {
            self.set_settings_notice(
                format!("Corrige la configuración MCP antes de guardarla: {error}"),
                true,
            );
            return false;
        }
        let settings = McpSettings { servers };
        match settings.save(&self.data_root, Path::new(&self.view.workspace)) {
            Ok(()) => {
                self.view.mcp_servers = settings.servers;
                self.set_settings_notice(message, false);
                true
            }
            Err(error) => {
                self.set_settings_notice(error, true);
                false
            }
        }
    }

    fn save_mcp_draft(&mut self) {
        let args: Vec<String> = if self.view.mcp_draft_http {
            Vec::new()
        } else {
            match serde_json::from_str(&self.view.mcp_draft_args) {
                Ok(args) => args,
                Err(error) => {
                    self.set_settings_notice(format!("Argumentos MCP no válidos: {error}"), true);
                    return;
                }
            }
        };
        let selected = self.view.mcp_selected_id.clone();
        let enabled = selected
            .as_deref()
            .and_then(|id| self.view.mcp_servers.iter().find(|server| server.id == id))
            .is_some_and(|server| server.enabled);
        let server = crate::mcp_settings::McpServerSettings {
            id: self.view.mcp_draft_id.trim().to_owned(),
            enabled,
            program: if self.view.mcp_draft_http {
                String::new()
            } else {
                self.view.mcp_draft_program.trim().to_owned()
            },
            args,
            cwd: (!self.view.mcp_draft_http && !self.view.mcp_draft_cwd.trim().is_empty())
                .then(|| PathBuf::from(self.view.mcp_draft_cwd.trim())),
            credential_env_names: if self.view.mcp_draft_http {
                Vec::new()
            } else {
                self.view
                    .mcp_draft_env_names
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned)
                    .collect()
            },
            url: self
                .view
                .mcp_draft_http
                .then(|| self.view.mcp_draft_url.trim().to_owned()),
            http_bearer_token_env_name: (self.view.mcp_draft_http
                && !self.view.mcp_draft_oauth_enabled
                && !self
                    .view
                    .mcp_draft_http_bearer_token_env_name
                    .trim()
                    .is_empty())
            .then(|| {
                self.view
                    .mcp_draft_http_bearer_token_env_name
                    .trim()
                    .to_owned()
            }),
            http_oauth: (self.view.mcp_draft_http && self.view.mcp_draft_oauth_enabled).then(
                || McpHttpOAuthSettings {
                    client_id: self.view.mcp_draft_oauth_client_id.trim().to_owned(),
                    issuer: self.view.mcp_draft_oauth_issuer.trim().to_owned(),
                    scopes: self
                        .view
                        .mcp_draft_oauth_scopes
                        .lines()
                        .map(str::trim)
                        .filter(|scope| !scope.is_empty())
                        .map(str::to_owned)
                        .collect(),
                },
            ),
        };
        let mut servers = self.view.mcp_servers.clone();
        let previous = selected
            .as_deref()
            .and_then(|id| servers.iter().find(|entry| entry.id == id))
            .cloned();
        if let Some(index) = selected
            .as_deref()
            .and_then(|id| servers.iter().position(|entry| entry.id == id))
        {
            servers[index] = server.clone();
        } else {
            servers.push(server.clone());
        }
        if let Err(error) = (McpSettings {
            servers: servers.clone(),
        })
        .validate_structure()
        {
            self.set_settings_notice(error, true);
            return;
        }
        if previous.as_ref().is_some_and(|previous| {
            self.view
                .mcp_oauth_status
                .as_ref()
                .is_some_and(|status| status.server_id == previous.id)
        }) {
            self.cancel_mcp_oauth();
        }
        if let Some(previous) = &previous {
            if oauth_registration_changed(previous, &server) {
                if let Err(error) = forget_oauth_registration(previous) {
                    self.set_settings_notice(error, true);
                    return;
                }
            }
        }
        if self.commit_mcp_settings(servers, "Servidor MCP guardado") {
            self.view.mcp_editor_open = false;
            self.clear_mcp_draft();
        }
    }

    fn toggle_mcp_server(&mut self, id: &str) {
        if self
            .view
            .mcp_oauth_status
            .as_ref()
            .is_some_and(|status| status.server_id == id)
        {
            self.cancel_mcp_oauth();
        }
        let mut servers = self.view.mcp_servers.clone();
        let Some(server) = servers.iter_mut().find(|server| server.id == id) else {
            self.set_settings_notice("Servidor MCP no encontrado", true);
            return;
        };
        server.enabled = !server.enabled;
        self.commit_mcp_settings(servers, "Estado MCP guardado");
    }

    fn delete_mcp_server(&mut self, id: &str) -> bool {
        if self
            .view
            .mcp_oauth_status
            .as_ref()
            .is_some_and(|status| status.server_id == id)
        {
            self.cancel_mcp_oauth();
        }
        let mut servers = self.view.mcp_servers.clone();
        if let Some(existing) = servers.iter().find(|server| server.id == id) {
            if let Err(error) = forget_oauth_registration(existing) {
                self.set_settings_notice(error, true);
                return false;
            }
        }
        let before = servers.len();
        servers.retain(|server| server.id != id);
        if servers.len() == before {
            self.set_settings_notice("Servidor MCP no encontrado", true);
            return false;
        }
        if !self.commit_mcp_settings(servers, "Servidor MCP eliminado") {
            return false;
        }
        if self.view.mcp_selected_id.as_deref() == Some(id) {
            self.view.mcp_editor_open = false;
            self.clear_mcp_draft();
        }
        true
    }

    fn cancel_mcp_oauth(&mut self) {
        if let Some(controller) = self.active_mcp_oauth.take() {
            controller.cancel();
        }
        self.view.mcp_oauth_status = None;
    }

    fn start_mcp_oauth(&mut self, id: &str) {
        let Some(server) = self
            .view
            .mcp_servers
            .iter()
            .find(|server| server.id == id)
            .cloned()
        else {
            self.set_settings_notice("Servidor MCP no encontrado", true);
            return;
        };
        self.cancel_mcp_oauth();
        match McpOAuthController::start(server) {
            Ok(controller) => {
                self.active_mcp_oauth = Some(controller);
                self.view.mcp_oauth_status = Some(McpOAuthViewStatus {
                    server_id: id.to_owned(),
                    phase: McpOAuthViewPhase::Discovering,
                });
                self.clear_settings_notice();
            }
            Err(error) => self.set_settings_notice(error, true),
        }
    }

    fn poll_mcp_oauth(&mut self) -> bool {
        let Some(controller) = &self.active_mcp_oauth else {
            return false;
        };
        let mut changed = false;
        let mut finished = false;
        while let Some(event) = controller.try_recv() {
            changed = true;
            let (server_id, phase) = match event {
                McpOAuthEvent::NeedBrowser { server_id, url } => {
                    (server_id, McpOAuthViewPhase::AwaitingBrowser { url })
                }
                McpOAuthEvent::Authorized { server_id } => {
                    finished = true;
                    (server_id, McpOAuthViewPhase::Authorized)
                }
                McpOAuthEvent::Failed { server_id, message } => {
                    finished = true;
                    (server_id, McpOAuthViewPhase::Failed(message))
                }
            };
            self.view.mcp_oauth_status = Some(McpOAuthViewStatus { server_id, phase });
        }
        if finished {
            self.active_mcp_oauth = None;
        }
        changed
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

    fn save_exec_rule_draft(&mut self) {
        let mut next_view = self.view.native_exec_rules.clone();
        if let Err(error) = next_view.upsert_draft() {
            self.set_settings_notice(error, true);
            return;
        }
        let next = ExecPolicySettings {
            rules: next_view.rules.clone(),
        };
        if let Err(error) = next.save(&self.data_root) {
            self.set_settings_notice(error, true);
            return;
        }
        self.exec_policy_settings = next;
        self.exec_policy_load_error = None;
        self.view.native_exec_rules = next_view;
        self.set_settings_notice(
            "Reglas de comandos guardadas para los próximos turnos",
            false,
        );
    }

    fn delete_exec_rule(&mut self, index: usize) {
        let mut next_view = self.view.native_exec_rules.clone();
        if let Err(error) = next_view.remove(index) {
            self.set_settings_notice(error, true);
            return;
        }
        let next = ExecPolicySettings {
            rules: next_view.rules.clone(),
        };
        if let Err(error) = next.save(&self.data_root) {
            self.set_settings_notice(error, true);
            return;
        }
        self.exec_policy_settings = next;
        self.exec_policy_load_error = None;
        self.view.native_exec_rules = next_view;
        self.set_settings_notice("Regla de comando eliminada para los próximos turnos", false);
    }

    fn reset_invalid_exec_rules(&mut self) {
        if self.exec_policy_load_error.is_none() {
            return;
        }
        let empty = ExecPolicySettings::default();
        if let Err(error) = empty.save(&self.data_root) {
            self.set_settings_notice(error, true);
            return;
        }
        self.exec_policy_settings = empty;
        self.exec_policy_load_error = None;
        self.view.native_exec_rules = ExecPolicyUiState::from_settings(&self.exec_policy_settings);
        self.set_settings_notice("Se restablecieron las reglas de comandos", false);
    }

    fn refresh_exec_policy_settings(&mut self) {
        match ExecPolicySettings::load(&self.data_root) {
            Ok(settings) => {
                self.exec_policy_settings = settings;
                self.exec_policy_load_error = None;
                self.view.native_exec_rules =
                    ExecPolicyUiState::from_settings(&self.exec_policy_settings);
            }
            Err(error) => {
                self.exec_policy_settings = ExecPolicySettings::default();
                self.exec_policy_load_error = Some(error.clone());
                self.view.native_exec_rules.rules.clear();
                self.view.native_exec_rules.load_error = Some(error);
                self.view.native_exec_rules.clear_draft();
            }
        }
    }

    fn refresh_native_skills(&mut self) {
        let workspace = Path::new(&self.view.workspace);
        self.view.native_skills = match NativeSkillsSettings::load(&self.data_root, workspace) {
            Ok(settings) => NativeSkillsUiState::from_settings(&settings, &self.data_root),
            Err(error) => NativeSkillsUiState::with_load_error(workspace, &self.data_root, error),
        };
    }

    fn save_native_skill_selections(&mut self, selections: Vec<openide_harness::SkillSelection>) {
        let result = NativeSkillsSettings::load(&self.data_root, Path::new(&self.view.workspace))
            .and_then(|settings| {
                let updated = settings.with_selections(selections);
                updated.save(&self.data_root)?;
                Ok(updated)
            });
        match result {
            Ok(settings) => {
                self.view.native_skills =
                    NativeSkillsUiState::from_settings(&settings, &self.data_root);
                self.set_settings_notice("Selección de skills locales guardada", false);
            }
            Err(error) => {
                self.refresh_native_skills();
                self.set_settings_notice(error, true);
            }
        }
    }

    fn reset_native_skills(&mut self) {
        let result =
            NativeSkillsSettings::reset_all(&self.data_root, Path::new(&self.view.workspace));
        match result {
            Ok(settings) => {
                self.view.native_skills =
                    NativeSkillsUiState::from_settings(&settings, &self.data_root);
                self.set_settings_notice("Selección de skills locales restablecida", false);
            }
            Err(error) => self.set_settings_notice(error, true),
        }
    }

    fn commit_agent_tool_settings(&mut self, next: AgentToolSettings, message: &str) -> bool {
        if let Err(error) = next.save(&self.data_root) {
            self.set_settings_notice(error, true);
            return false;
        }
        let command_access_revoked = next.command_isolation()
            != self.agent_tool_settings.command_isolation()
            || !next.permissions().run_command;
        if command_access_revoked {
            self.native_command_sessions.revoke_all();
            self.clear_native_command_ui();
        }
        self.agent_tool_settings = next;
        self.view.native_tool_permissions = next.permissions();
        self.view.native_command_isolation = next.command_isolation();
        self.view.workspace_writes_enabled =
            next.permissions().write_file && next.permissions().run_command;
        if command_access_revoked {
            self.set_settings_notice(
                format!("{message}; los comandos persistentes quedan revocados"),
                false,
            );
        } else {
            self.set_settings_notice(message, false);
        }
        true
    }

    fn native_parent_tools(
        &mut self,
        session_id: &str,
        workspace: &Path,
    ) -> Result<Arc<dyn ToolExecutor>, String> {
        let permissions = self.agent_tool_settings.permissions();
        let isolation = self.agent_tool_settings.command_isolation();
        let commands = self.native_command_sessions.for_session(
            session_id,
            workspace,
            isolation,
            permissions.run_command,
        )?;
        let mut tools = LocalTools::new()
            .with_permissions(permissions)
            .with_command_isolation(isolation);
        if let Some(commands) = commands {
            tools = tools.with_persistent_commands(commands);
        }
        Ok(Arc::new(tools))
    }

    fn clear_native_command_ui(&mut self) {
        self.view.native_commands.clear();
        self.view.native_commands_error = None;
        self.view.pending_native_command_termination = None;
        self.view.native_command_terminating = None;
        self.native_command_snapshot_response = None;
        self.native_command_termination_response = None;
        self.last_native_command_snapshot = None;
    }

    /// A command listing reads registry state without draining stdout/stderr.
    /// Both listing and termination run off the UI thread; only small channel
    /// results are applied during a frame.
    fn poll_native_command_sessions(&mut self) -> bool {
        let mut changed = false;
        match self
            .native_command_termination_response
            .as_ref()
            .map(Receiver::try_recv)
        {
            Some(Ok((owner, command_id, manager, result))) => {
                self.native_command_termination_response = None;
                if self.view.selected_session.as_deref() == Some(owner.as_str())
                    && self
                        .native_command_sessions
                        .manager(&owner)
                        .is_some_and(|current| Arc::ptr_eq(&current, &manager))
                {
                    self.view.native_command_terminating = None;
                    self.view.status = match result {
                        Ok(()) => format!(
                            "Proceso {} terminado; la salida pendiente sigue disponible",
                            command_id.chars().take(8).collect::<String>()
                        ),
                        Err(error) => format!("No se pudo terminar el proceso: {error}"),
                    };
                    self.last_native_command_snapshot = None;
                    changed = true;
                }
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.native_command_termination_response = None;
                self.view.native_command_terminating = None;
                self.view.status = "La operación de terminar proceso se interrumpió".into();
                changed = true;
            }
            _ => {}
        }
        match self
            .native_command_snapshot_response
            .as_ref()
            .map(Receiver::try_recv)
        {
            Some(Ok((owner, manager, result))) => {
                self.native_command_snapshot_response = None;
                if self.view.selected_session.as_deref() == Some(owner.as_str())
                    && self
                        .native_command_sessions
                        .manager(&owner)
                        .is_some_and(|current| Arc::ptr_eq(&current, &manager))
                {
                    match result {
                        Ok(commands) => {
                            if self.view.native_commands != commands
                                || self.view.native_commands_error.is_some()
                            {
                                changed = true;
                            }
                            self.view.native_commands = commands;
                            self.view.native_commands_error = None;
                            if self
                                .view
                                .pending_native_command_termination
                                .as_ref()
                                .is_some_and(|id| {
                                    !self
                                        .view
                                        .native_commands
                                        .iter()
                                        .any(|command| &command.id == id && command.exit.is_none())
                                })
                            {
                                self.view.pending_native_command_termination = None;
                                changed = true;
                            }
                        }
                        Err(error) => {
                            self.view.native_commands.clear();
                            self.view.native_commands_error = Some(error);
                            self.view.pending_native_command_termination = None;
                            changed = true;
                        }
                    }
                }
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.native_command_snapshot_response = None;
                self.view.native_commands_error =
                    Some("Se interrumpió la lectura de procesos".into());
                changed = true;
            }
            _ => {}
        }
        let Some(owner) = self.view.selected_session.as_ref() else {
            return changed;
        };
        let Some(manager) = self.native_command_sessions.manager(owner) else {
            if !self.view.native_commands.is_empty() || self.view.native_commands_error.is_some() {
                self.view.native_commands.clear();
                self.view.native_commands_error = None;
                self.view.pending_native_command_termination = None;
                changed = true;
            }
            return changed;
        };
        if self.view.show_settings
            || self.view.main_page != MainPage::Workspace
            || !self.view.show_side_pane
            || self.view.side_tab != SideTab::Activity
            || self.native_command_snapshot_response.is_some()
            || self
                .last_native_command_snapshot
                .is_some_and(|last| last.elapsed() < Duration::from_millis(500))
        {
            return changed;
        }
        let owner = owner.clone();
        let (sender, receiver) = mpsc::channel();
        self.native_command_snapshot_response = Some(receiver);
        self.last_native_command_snapshot = Some(Instant::now());
        std::thread::spawn(move || {
            let result = manager.list_sessions().map_err(|error| error.to_string());
            let _ = sender.send((owner, manager, result));
        });
        changed
    }

    fn confirm_native_command_termination(&mut self, command_id: &str) {
        if self.view.pending_native_command_termination.as_deref() != Some(command_id) {
            return;
        }
        self.view.pending_native_command_termination = None;
        if self.native_command_termination_response.is_some() {
            self.view.status = "Ya se está terminando un proceso".into();
            return;
        }
        let Some(owner) = self.view.selected_session.clone() else {
            return;
        };
        if !self
            .view
            .native_commands
            .iter()
            .any(|command| command.id == command_id && command.exit.is_none())
        {
            self.view.status = "Ese proceso ya no está en ejecución".into();
            return;
        }
        let Some(manager) = self.native_command_sessions.manager(&owner) else {
            self.view.status = "La sesión de comandos ya no está disponible".into();
            return;
        };
        let command_id = command_id.to_owned();
        let (sender, receiver) = mpsc::channel();
        self.native_command_termination_response = Some(receiver);
        self.view.native_command_terminating = Some(command_id.clone());
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
                .and_then(|runtime| {
                    runtime
                        .block_on(manager.terminate(&command_id))
                        .map_err(|error| error.to_string())
                });
            let _ = sender.send((owner, command_id, manager, result));
        });
    }

    fn native_permission_policy(&self) -> Result<PermissionPolicy, String> {
        if let Some(error) = &self.exec_policy_load_error {
            return Err(format!(
                "Corregí las reglas de comandos antes de ejecutar el agente nativo: {error}"
            ));
        }
        let exec_policy = ExecPolicySettings::load(&self.data_root)?.policy()?;
        Ok(self
            .agent_tool_settings
            .permission_policy()
            .with_exec_policy(exec_policy)
            .with_tool("spawn_agent", PermissionMode::Allow)
            .with_tool("wait_agent", PermissionMode::Allow)
            .with_tool("cancel_agent", PermissionMode::Allow)
            .with_tool("send_input", PermissionMode::Allow))
    }

    fn ensure_archivable_native_session(&self, session_id: &str) -> Result<(), String> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let Some(stored) = store.load(session_id).map_err(|error| error.to_string())? else {
            return Err("La sesión ya no existe".into());
        };
        if stored.session.backend != "native" {
            return Ok(());
        }
        let coordinator = SubagentCoordinator::new(store.clone(), SubagentLimits::default())
            .map_err(|error| error.to_string())?;
        if let Some(link) = stored.session.subagent.as_ref() {
            let siblings = coordinator
                .list_children_for_ui(&link.parent_session_id)
                .map_err(|error| error.to_string())?;
            let current = siblings
                .iter()
                .find(|child| child.session.id == session_id)
                .ok_or_else(|| "No se pudo verificar el estado del subagente".to_owned())?;
            if !matches!(
                current.status,
                SubagentStatus::Completed | SubagentStatus::Failed | SubagentStatus::Cancelled
            ) {
                return Err("Recuperá o cancelá el subagente antes de archivarlo".into());
            }
            if coordinator
                .open_input_count_for_ui(session_id)
                .map_err(|error| error.to_string())?
                > 0
            {
                return Err(
                    "El subagente tiene mensajes pendientes; resolvelos antes de archivarlo".into(),
                );
            }
        }
        let mut pending = VecDeque::from([session_id.to_owned()]);
        while let Some(parent_id) = pending.pop_front() {
            for child in coordinator
                .list_children_for_ui(&parent_id)
                .map_err(|error| error.to_string())?
            {
                if !matches!(
                    child.status,
                    SubagentStatus::Completed | SubagentStatus::Failed | SubagentStatus::Cancelled
                ) {
                    return Err(
                        "Esta tarea tiene subagentes activos o interrumpidos; resolvelos antes de archivarla"
                            .into(),
                    );
                }
                if coordinator
                    .open_input_count_for_ui(&child.session.id)
                    .map_err(|error| error.to_string())?
                    > 0
                {
                    return Err(
                        "Esta tarea tiene mensajes de subagentes pendientes; resolvelos antes de archivarla"
                            .into(),
                    );
                }
                pending.push_back(child.session.id);
            }
        }
        Ok(())
    }

    fn persist_native_settings(&mut self) -> bool {
        let settings = self.native_settings();
        self.view.native_key_available = native_key_available_at(&settings, &self.data_root);
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
        } else {
            self.refresh_composer_routes();
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

    fn begin_model_discovery(&mut self) {
        if self.model_discovery_response.is_some() {
            return;
        }
        let registry = match native_provider::ProviderRegistry::load(&self.data_root) {
            Ok(registry) => registry,
            Err(error) => {
                self.view.status = format!("Registro de proveedores: {error}");
                return;
            }
        };
        let providers: Vec<String> = registry
            .adapters()
            .iter()
            .filter(|adapter| {
                adapter.model_list_path.is_some()
                    && std::env::var(&adapter.key_env).is_ok_and(|key| !key.trim().is_empty())
            })
            .map(|adapter| adapter.id.clone())
            .collect();
        if providers.is_empty() {
            return;
        }
        for provider in &providers {
            self.view
                .composer_provider_status
                .insert(provider.clone(), ProviderModelStatus::Checking);
        }
        self.model_discoveries
            .retain(|entry| !providers.contains(&entry.provider_id));
        self.refresh_composer_routes();
        let (sender, receiver) = mpsc::channel();
        self.model_discovery_pending = providers.len();
        self.model_discovery_response = Some(receiver);
        for provider in providers {
            let sender = sender.clone();
            let root = self.data_root.clone();
            std::thread::spawn(move || {
                let result = native_provider::discover_connected_models_at(&root, &provider);
                let _ = sender.send((provider, result));
            });
        }
    }

    fn poll_model_discovery(&mut self) -> bool {
        let mut changed = false;
        while let Some(receiver) = self.model_discovery_response.as_ref() {
            let result = receiver.try_recv();
            match result {
                Ok((provider, Ok(discovery))) => {
                    self.view.composer_provider_status.remove(&provider);
                    self.model_discoveries
                        .retain(|entry| entry.provider_id != provider);
                    self.model_discoveries.push(discovery);
                    self.model_discovery_pending = self.model_discovery_pending.saturating_sub(1);
                    self.refresh_composer_routes();
                    changed = true;
                }
                Ok((provider, Err(_error))) => {
                    // Failure leaves locally reviewed routes visible as
                    // key-present, but the menu asks the user to check the
                    // connection instead of claiming account availability.
                    self.view
                        .composer_provider_status
                        .insert(provider, ProviderModelStatus::CheckFailed);
                    self.model_discovery_pending = self.model_discovery_pending.saturating_sub(1);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.model_discovery_pending = 0;
                    for status in self.view.composer_provider_status.values_mut() {
                        if *status == ProviderModelStatus::Checking {
                            *status = ProviderModelStatus::CheckFailed;
                        }
                    }
                    changed = true;
                    break;
                }
            }
        }
        if self.model_discovery_pending == 0 {
            self.model_discovery_response = None;
        }
        changed
    }

    fn begin_codex_model_discovery(&mut self) {
        if self.codex_model_response.is_some() {
            return;
        }
        let Some(executable) = self
            .profiles
            .iter()
            .find(|profile| profile.id == "codex")
            .and_then(CliProfile::detect_executable)
        else {
            return;
        };
        self.codex_models.clear();
        self.view
            .composer_provider_status
            .insert("codex".to_owned(), ProviderModelStatus::Checking);
        self.refresh_composer_routes();
        let (sender, receiver) = mpsc::channel();
        self.codex_model_response = Some(receiver);
        std::thread::spawn(move || {
            let result = openide_cli::discover_codex_models_with_executable(&executable);
            let _ = sender.send(result);
        });
    }

    fn poll_codex_model_discovery(&mut self) -> bool {
        let Some(receiver) = self.codex_model_response.as_ref() else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("El proceso de consulta terminó".to_owned()),
        };
        self.codex_model_response = None;
        self.view.composer_provider_status.remove("codex");
        match result {
            Ok(models) => {
                self.codex_models = models;
                self.refresh_composer_routes();
                if self.view.selected_backend == "codex" {
                    let previous_model = self.view.composer_model.clone();
                    let previous_effort = self.view.composer_effort.clone();
                    self.refresh_composer_choice_options();
                    if self.view.composer_model != previous_model {
                        self.remember_composer_selection(true);
                        self.view.status =
                            "El modelo anterior no está disponible en tu cuenta; se seleccionó el predeterminado"
                                .to_owned();
                    } else if self.view.composer_effort != previous_effort {
                        self.remember_composer_selection(true);
                    }
                }
            }
            Err(_error) => {
                self.codex_models.clear();
                self.refresh_composer_routes();
                if self.view.selected_backend == "codex" {
                    self.refresh_composer_choice_options();
                    self.view.status =
                        "No se pudo verificar el catálogo de Codex CLI; se muestran los modelos de respaldo"
                            .to_owned();
                }
            }
        }
        true
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
                self.refresh_composer_routes();
            }
            Err(error) => {
                self.view.catalog_status =
                    format!("Sin actualización ({error}); se conserva la última copia válida");
            }
        }
        true
    }

    fn refresh_composer_selection(&mut self) {
        self.refresh_composer_routes();
        let choice = self
            .composer_preferences
            .choice(&self.view.selected_backend, &self.view.model);
        self.view.composer_model = choice.model;
        self.view.composer_effort = choice.effort;
        self.refresh_composer_choice_options();
        self.view.composer_model_menu_open = false;
        self.view.composer_model_list_open = false;
        self.view.composer_model_query.clear();
        self.composer_model_search_focus_pending = false;
        self.view.composer_advanced_open = false;
        self.view.composer_effort_menu_open = false;
        self.view.composer_effort_preview = None;
        self.view.backend_menu_open = false;
    }

    fn refresh_composer_routes(&mut self) {
        let mut routes = match composer_selection::model_route_options_with_codex_discovery_at(
            self.view.catalog.as_deref(),
            &self.native_settings(),
            &self.model_discoveries,
            &self.codex_models,
            &self.data_root,
        ) {
            Ok(routes) => routes,
            Err(error) => {
                self.view.status = format!("Registro de proveedores: {error}");
                return;
            }
        };
        routes.retain(|route| {
            route.backend == "native"
                || self
                    .view
                    .backend_options
                    .iter()
                    .any(|(backend, _)| backend == &route.backend)
        });
        self.view.composer_routes = routes;
    }

    fn refresh_composer_choice_options(&mut self) {
        let codex_live = self.view.selected_backend == "codex" && !self.codex_models.is_empty();
        self.view.composer_model_options = if codex_live {
            self.view
                .composer_routes
                .iter()
                .filter(|route| route.backend == "codex")
                .map(|route| (route.model_id.clone(), route.model_name.clone()))
                .collect()
        } else {
            composer_selection::model_options(
                &self.view.selected_backend,
                &self.view.composer_model,
            )
        };
        if codex_live
            && !self
                .view
                .composer_model_options
                .iter()
                .any(|(id, _)| id == &self.view.composer_model)
        {
            self.view.composer_model = "default".to_owned();
            self.view.composer_effort = "default".to_owned();
        }
        self.view.composer_effort_options = if codex_live {
            self.view
                .composer_routes
                .iter()
                .find(|route| {
                    route.backend == "codex" && route.model_id == self.view.composer_model
                })
                .map(|route| route.effort_options.clone())
                .unwrap_or_default()
        } else if self.view.selected_backend == "native" {
            self.view
                .composer_routes
                .iter()
                .find(|route| {
                    route.backend == "native"
                        && route.provider_id == self.view.native_provider_id
                        && route.model_id == self.view.composer_model
                })
                .map(|route| route.effort_options.clone())
                .unwrap_or_default()
        } else {
            composer_selection::effort_options_for_provider(
                &self.view.selected_backend,
                &self.view.composer_model,
                &self.view.native_provider_id,
            )
        };
        if !self
            .view
            .composer_effort_options
            .iter()
            .any(|(id, _)| id == &self.view.composer_effort)
        {
            self.view.composer_effort = self
                .view
                .composer_effort_options
                .first()
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| "default".to_owned());
        }
    }

    fn refresh_composer_context_percent(&mut self) {
        self.view.composer_context_percent = None;
        if self.view.selected_backend != "native" {
            return;
        }
        let (Some(store), Some(session_id)) = (&self.store, self.view.selected_session.as_deref())
        else {
            return;
        };
        if let Ok(Some(stored)) = store.load(session_id) {
            if stored.session.backend == "native"
                && stored.session.workspace == Path::new(&self.view.workspace)
            {
                self.view.composer_context_percent = committed_context_percent(
                    &stored.events,
                    &self.view.native_provider_id,
                    &self.view.model,
                );
            }
        }
    }

    fn remember_composer_selection(&mut self, save: bool) {
        self.composer_preferences
            .set_selected_backend(&self.view.selected_backend);
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
            self.view.composer_model_list_open = false;
            self.view.composer_model_query.clear();
            self.composer_model_search_focus_pending = false;
            self.view.composer_advanced_open = false;
            self.view.composer_effort_menu_open = false;
            self.view.composer_effort_preview = None;
            self.view.show_settings = true;
            self.view.settings_page = SettingsPage::Providers;
            self.clear_settings_notice();
            self.visit_navigation();
            return;
        }
        self.view.composer_model = model.to_owned();
        self.refresh_composer_choice_options();
        if self.view.selected_backend == "native" {
            self.view.model = model.to_owned();
            self.persist_native_settings();
        } else {
            self.remember_composer_selection(true);
        }
        self.view.composer_model_menu_open = false;
        self.view.composer_model_list_open = false;
        self.view.composer_model_query.clear();
        self.composer_model_search_focus_pending = false;
        self.view.composer_advanced_open = false;
        self.view.composer_effort_menu_open = false;
        self.view.composer_effort_preview = None;
        self.refresh_composer_context_percent();
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
        self.view.composer_model_list_open = false;
        self.view.composer_model_query.clear();
        self.composer_model_search_focus_pending = false;
        self.view.composer_advanced_open = false;
        self.view.composer_effort_preview = None;
        self.remember_composer_selection(true);
    }

    fn composer_slider_effort(&self, fraction: f32) -> Option<String> {
        let levels: Vec<_> = self
            .view
            .composer_effort_options
            .iter()
            .filter(|(id, _)| id != "default")
            .collect();
        if levels.len() < 2 {
            return None;
        }
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let index = (fraction * (levels.len() - 1) as f32).round() as usize;
        Some(levels[index].0.clone())
    }

    fn select_composer_route(&mut self, route_id: &str) {
        if self.view.running || self.view.background_run_selected {
            return;
        }
        let Some(route) =
            composer_selection::model_route_option_by_id(&self.view.composer_routes, route_id)
                .cloned()
        else {
            return;
        };
        let provider_status = self
            .view
            .composer_provider_status
            .get(&route.provider_id)
            .copied();
        if provider_status == Some(ProviderModelStatus::Checking) {
            return;
        }
        if !route.selectable() || provider_status == Some(ProviderModelStatus::CheckFailed) {
            if route.credential == composer_selection::CredentialAvailability::MissingCredential
                || provider_status == Some(ProviderModelStatus::CheckFailed)
            {
                self.view.composer_model_menu_open = false;
                self.view.composer_model_list_open = false;
                self.view.composer_model_query.clear();
                self.composer_model_search_focus_pending = false;
                self.view.composer_advanced_open = false;
                self.view.show_settings = true;
                self.view.settings_page = SettingsPage::Providers;
                self.view.catalog_browse_provider = route.provider_id.clone();
                let key_env = native_provider::adapter_at(&self.data_root, &route.provider_id)
                    .ok()
                    .flatten()
                    .map(|adapter| adapter.key_env)
                    .unwrap_or_else(|| "API_KEY".to_owned());
                self.set_settings_notice(
                    if provider_status == Some(ProviderModelStatus::CheckFailed) {
                        format!(
                            "No se pudo verificar {}. Revisa {key_env} y actualiza proveedores",
                            route.provider_name
                        )
                    } else {
                        format!(
                            "Conecta {} con {key_env} para usar sus modelos",
                            route.provider_name
                        )
                    },
                    provider_status == Some(ProviderModelStatus::CheckFailed),
                );
                self.visit_navigation();
            }
            return;
        }
        if route.backend == "native" {
            let previous_provider = self.view.native_provider_id.clone();
            let previous_model = self.view.model.clone();
            self.view.native_provider_id = route.provider_id.clone();
            self.view.model = route.model_id.clone();
            if !self.persist_native_settings() {
                self.view.native_provider_id = previous_provider;
                self.view.model = previous_model;
                self.view.native_key_available =
                    native_key_available_at(&self.native_settings(), &self.data_root);
                return;
            }
        }
        if self.view.selected_backend != route.backend {
            self.new_session();
            self.view.selected_backend = route.backend.clone();
            self.refresh_composer_selection();
        }
        if route.backend == "native" {
            self.refresh_composer_context_percent();
        } else if route.backend == "codex" {
            self.select_composer_model(&route.model_id);
        } else {
            self.view.composer_model = route.model_id.clone();
            self.refresh_composer_choice_options();
        }
        self.view.composer_model_menu_open = false;
        self.view.composer_model_list_open = false;
        self.view.composer_model_query.clear();
        self.composer_model_search_focus_pending = false;
        self.view.composer_advanced_open = false;
        self.view.composer_effort_preview = None;
        self.view.status = format!(
            "Modelo seleccionado: {} · {}",
            route.provider_name, route.model_name
        );
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
        self.view.session_approval_grants.clear();
        self.clear_native_command_ui();
        self.view.composer_context_percent = None;
        self.view.transcript.clear();
        self.view.selected_tool_id = None;
        self.view.activity_page = 0;
        self.view.pending_permissions.clear();
        self.view.recoverable_subagent = None;
        self.view.pending_tool_verification = None;
        self.view.tool_verification_output.clear();
        self.view.pending_batch_discard = None;
        self.view.turn_mode = TurnMode::Build;
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

    fn focus_composer_model_search_if_pending(&mut self) {
        if self.composer_model_search_focus_pending
            && self.view.composer_model_menu_open
            && self.view.composer_model_list_open
            && !self.view.show_command_center
            && self.tree.focus("composer_model_search")
        {
            self.composer_model_search_focus_pending = false;
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

    fn focus_rename_if_pending(&mut self) {
        if self.rename_focus_pending
            && !self.view.show_command_center
            && self.tree.focus("rename_title")
        {
            self.rename_focus_pending = false;
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
                self.view.group_menu_id = None;
            }
        } else if self
            .view
            .session_groups
            .groups
            .iter()
            .any(|group| group.id == group_id && group.title == self.view.group_rename_draft.trim())
        {
            self.view.group_rename_id = None;
            self.view.group_menu_id = None;
        } else {
            self.view.status = "Escribe un nombre de grupo válido".to_owned();
        }
    }

    fn close_session_menu(&mut self) {
        self.view.session_menu_id = None;
        self.view.renaming_session_id = None;
        self.rename_focus_pending = false;
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
                                if let AgentEventKind::TurnAdmitted { prompt, .. }
                                | AgentEventKind::TurnAdmittedWithImages { prompt, .. }
                                | AgentEventKind::TurnStarted { prompt } = &event.kind
                                {
                                    Some(prompt.lines().next().unwrap_or(prompt).trim().to_owned())
                                } else {
                                    None
                                }
                            })
                        })
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| "Nueva tarea".to_owned()),
                );
                self.view.selected_session = Some(stored.session.id.clone());
                let workspace_changed =
                    self.view.workspace != stored.session.workspace.to_string_lossy();
                self.view.workspace = stored.session.workspace.to_string_lossy().into_owned();
                self.view.workspace_draft = self.view.workspace.clone();
                if workspace_changed {
                    self.view.project_branch = project_branch_for_workspace(&self.view.workspace);
                    self.invalidate_workspace_files();
                    self.refresh_mcp_settings();
                }
                self.refresh_composer_projects();
                self.restore_draft();
                self.ensure_dock_tabs();
                if self.view.side_tab == SideTab::Files {
                    self.refresh_file_rows();
                }
                self.tree
                    .set_input_value("workspace", self.view.workspace.clone());
                self.view.selected_backend = stored.session.backend.clone();
                self.refresh_composer_selection();
                self.view.turn_mode = stored
                    .events
                    .iter()
                    .rev()
                    .find_map(|event| match &event.kind {
                        AgentEventKind::TurnAdmitted { mode, .. }
                        | AgentEventKind::TurnAdmittedWithImages { mode, .. } => Some(*mode),
                        _ => None,
                    })
                    .unwrap_or(TurnMode::Build);
                self.refresh_composer_context_percent();
                self.last_sequence = stored
                    .events
                    .iter()
                    .map(|event| event.sequence)
                    .max()
                    .unwrap_or(0);
                self.projection = TranscriptProjection::from_events(&stored.events);
                self.view.transcript = self.projection.items().to_vec();
                let replay_status = if stored.session.backend == "native" {
                    match replay_stored_native(&self.data_root, &stored) {
                        Ok(replay) => {
                            self.history = replay.history;
                            recovery_status(&replay.recovery).to_owned()
                        }
                        Err(error) => {
                            self.history.clear();
                            error
                        }
                    }
                } else {
                    self.history = history_from_events(&stored.events);
                    "Sesión cargada".to_owned()
                };
                self.tree.scroll_to_end("transcript");
                self.view.status = replay_status;
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
                self.refresh_sessions();
            }
            Ok(None) => self.view.status = "La sesión ya no existe".to_owned(),
            Err(error) => self.view.status = format!("Error al abrir sesión: {error}"),
        }
    }

    fn stop_dictation(&mut self) {
        if let Some(handle) = self.dictation.take() {
            handle.cancel();
        }
        self.dictation_key = None;
        self.view.composer_listening = false;
    }

    fn toggle_dictation(&mut self, ctx: &egui::Context) {
        if self.dictation.is_some() {
            self.stop_dictation();
            self.view.status = "Dictado detenido".to_owned();
            return;
        }
        if self.view.running || self.view.background_run_selected {
            return;
        }
        match voice::start() {
            Ok(handle) => {
                self.dictation_key = Some(self.draft_key());
                self.dictation = Some(handle);
                self.view.composer_listening = true;
                self.view.status = "Iniciando dictado…".to_owned();
                ctx.request_repaint_after(Duration::from_millis(120));
            }
            Err(error) => self.view.status = error,
        }
    }

    fn poll_dictation(&mut self, ctx: &egui::Context) -> bool {
        if self.dictation.is_none() {
            return false;
        }
        if self.dictation_key.as_ref() != Some(&self.draft_key()) {
            self.stop_dictation();
            return true;
        }
        let mut events = Vec::new();
        if let Some(handle) = &self.dictation {
            loop {
                match handle.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        events.push(VoiceEvent::Stopped);
                        break;
                    }
                }
            }
        }
        let changed = !events.is_empty();
        for event in events {
            match event {
                VoiceEvent::Listening => {
                    self.view.status = "Escuchando… Pulsa el micrófono para terminar".to_owned();
                }
                VoiceEvent::Final(text) => {
                    if self.tree.focus("prompt") {
                        for action in self.tree.handle_event(UiEvent::Text(format!("{text} "))) {
                            self.apply_action(action, ctx);
                        }
                    }
                }
                VoiceEvent::Error(error) => {
                    self.view.status = error;
                    self.stop_dictation();
                }
                VoiceEvent::Stopped => {
                    self.stop_dictation();
                }
            }
        }
        if self.dictation.is_some() {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
        changed
    }

    fn queue_native_steer(&mut self) {
        let Some(session_id) = self.view.selected_session.as_deref() else {
            self.view.status = "Selecciona la sesión activa para intervenir".to_owned();
            return;
        };
        let running_here = matches!(
            self.active_run.as_ref(),
            Some(ActiveRun::Native(run)) if run.session_id == session_id
        ) || self
            .active_resume
            .as_ref()
            .is_some_and(|resume| resume.session_id == session_id);
        if !running_here {
            self.view.status = "No hay un turno nativo activo en esta sesión".to_owned();
            return;
        }
        let visible = self.view.prompt.trim().to_owned();
        let attachments = self.view.composer_attachments.clone();
        if visible.is_empty() && attachments.is_empty() {
            return;
        }
        let workspace = PathBuf::from(self.view.workspace.trim());
        let payload = match prepare_native_attachments(
            &workspace,
            &attachments,
            &self.view.native_provider_id,
            &self.view.model,
        ) {
            Ok(payload) => payload,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let instruction = native_prompt_with_images(
            &visible,
            &payload.path_attachments,
            &payload.image_attachments,
        );
        let Some(store) = self.store.as_ref() else {
            self.view.status = "El historial no está disponible".to_owned();
            return;
        };
        let result = store
            .active_native_turn_id(session_id)
            .map_err(|error| error.to_string())
            .and_then(|turn_id| {
                let turn_id = turn_id.ok_or_else(|| "No hay un turno nativo activo".to_owned())?;
                enqueue_native_steer_draft(
                    &self.data_root,
                    store,
                    NativeSteerDraftSubmission {
                        workspace: &workspace,
                        session_id,
                        turn_id: &turn_id,
                        prompt: &self.view.prompt,
                        attachments: &attachments,
                        payload_text: &instruction,
                        images: &payload.images,
                    },
                )
            });
        match result {
            Ok(input_id) => {
                if let Err(error) = acknowledge_native_steer_draft(
                    &self.data_root,
                    store,
                    &workspace,
                    session_id,
                    &input_id,
                ) {
                    self.view.status = format!(
                        "Intervención en cola, pero no se pudo limpiar el borrador: {error}. Reintenta para confirmar."
                    );
                    return;
                }
                let draft_key = self.draft_key();
                self.view.prompt.clear();
                self.view.composer_attachments.clear();
                self.drafts.remove(&draft_key);
                self.attachment_drafts.remove(&draft_key);
                self.draft_dirty_at = None;
                self.draft_error = None;
                self.tree.set_input_value("prompt", "");
                self.view.status = "Instrucción en cola para el próximo límite seguro".to_owned();
            }
            Err(error) => {
                self.view.status = format!("No se pudo intervenir el turno: {error}");
            }
        }
    }

    fn revoke_selected_session_approval(&mut self, key_digest: &str) {
        let Some(session_id) = self.view.selected_session.as_deref() else {
            self.view.status = "Selecciona una sesión para revocar el permiso".to_owned();
            return;
        };
        let Some(store) = &self.store else {
            self.view.status = "El historial no está disponible".to_owned();
            return;
        };
        match store.revoke_session_approval_grant(session_id, key_digest) {
            Ok(true) => {
                self.view.status =
                    "Permiso de sesión revocado para las próximas herramientas".to_owned();
                self.refresh_sessions();
            }
            Ok(false) => {
                self.view.status = "Ese permiso ya no está activo".to_owned();
                self.refresh_sessions();
            }
            Err(error) => {
                self.view.status = format!("No se pudo revocar el permiso: {error}");
            }
        }
    }

    fn start_run(&mut self) {
        self.stop_dictation();
        if self.view.running {
            if self.view.selected_backend == "native" {
                self.queue_native_steer();
            }
            return;
        }
        if self.active_compact.is_some() {
            self.view.status = "Espera a que termine la compactación".to_owned();
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
        let backend = self.view.selected_backend.clone();
        let native_attachments = if backend == "native" {
            match prepare_native_attachments(
                &workspace,
                &attachments,
                &self.view.native_provider_id,
                &self.view.model,
            ) {
                Ok(payload) => Some(payload),
                Err(error) => {
                    self.view.status = error;
                    return;
                }
            }
        } else {
            for attachment in &attachments {
                match composer_attachment_for_path(&workspace, Path::new(&attachment.path)) {
                    Ok(current)
                        if current.path == attachment.path
                            && current.is_dir == attachment.is_dir => {}
                    Ok(_) => {
                        self.view.status = "Un adjunto cambió de ubicación o tipo".to_owned();
                        return;
                    }
                    Err(error) => {
                        self.view.status = format!("Adjunto no disponible: {error}");
                        return;
                    }
                }
            }
            None
        };
        let prompt = if let Some(payload) = &native_attachments {
            native_prompt_with_images(
                &visible_prompt,
                &payload.path_attachments,
                &payload.image_attachments,
            )
        } else {
            composer_prompt_with_attachments(&visible_prompt, &attachments)
        };
        let native_images = native_attachments
            .map(|payload| payload.images)
            .unwrap_or_default();
        let submitted_draft_key = self.draft_key();
        let active_goal = self.view.composer_goal.clone();
        let native_connection = if backend == "native" {
            let connection = match self.native_settings().resolve_at(&self.data_root) {
                Ok(connection) => connection,
                Err(error) => {
                    self.view.status = error;
                    return;
                }
            };
            if connection.api_key.is_none() && self.view.native_provider_id != "custom" {
                let key_env =
                    native_provider::adapter_at(&self.data_root, &self.view.native_provider_id)
                        .ok()
                        .flatten()
                        .map(|adapter| adapter.key_env)
                        .unwrap_or_else(|| "API_KEY".to_owned());
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
        let native_instructions = if backend == "native" {
            match native_workspace_instructions(&self.data_root, &workspace) {
                Ok(instructions) => instructions,
                Err(error) => {
                    self.view.status = error;
                    return;
                }
            }
        } else {
            String::new()
        };
        let native_mcp_servers = if backend == "native" && self.view.turn_mode == TurnMode::Build {
            match McpSettings::load(&self.data_root, &workspace) {
                Ok(settings) => settings.enabled_servers().cloned().collect::<Vec<_>>(),
                Err(error) => {
                    self.view.status = error;
                    return;
                }
            }
        } else {
            Vec::new()
        };
        let native_hooks = if backend == "native" {
            match NativeHookSettings::load(&self.data_root).and_then(|settings| settings.registry())
            {
                Ok(hooks) => Some(Arc::new(hooks)),
                Err(error) => {
                    self.view.status = error;
                    return;
                }
            }
        } else {
            None
        };
        let Some(store) = self.store.clone() else {
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
        if submitted_draft_key.1.is_none() {
            if let Err(error) = self.transfer_draft_goal_to_session(&submitted_draft_key, &id) {
                self.view.status = error;
                return;
            }
        }
        let mut request = AgentRunRequest {
            session_id: Some(id),
            sequence_start: self.last_sequence,
            workspace,
            history: self.history.clone(),
            prompt: prompt.clone(),
            mode: if backend == "native" {
                self.view.turn_mode
            } else {
                TurnMode::Build
            },
        };
        let active = if backend == "native" {
            let connection = native_connection.expect("validated native connection");
            let model_id = connection.model_id.clone();
            let endpoint = connection.endpoint.clone();
            let wire_api = connection.wire_api;
            let model = match native_model_backend(
                connection,
                &self.view.native_provider_id,
                Some(&self.view.composer_effort),
                self.view.hosted_web_search_mode,
                &self.data_root,
                self.view.catalog.as_deref(),
                &self.model_discoveries,
            ) {
                Ok(model) => model,
                Err(error) => {
                    self.view.status = error;
                    return;
                }
            };
            let lease = match store.try_acquire_native_lease(
                request
                    .session_id
                    .as_deref()
                    .expect("session id was assigned"),
            ) {
                Ok(lease) => lease,
                Err(error) => {
                    self.view.status = format!("La sesión nativa ya está en uso: {error}");
                    return;
                }
            };
            let stored = match store.load(
                request
                    .session_id
                    .as_deref()
                    .expect("session id was assigned"),
            ) {
                Ok(Some(stored)) => stored,
                Ok(None) => {
                    self.view.status = "La sesión nativa desapareció".to_owned();
                    return;
                }
                Err(error) => {
                    self.view.status = format!("No se pudo leer la sesión nativa: {error}");
                    return;
                }
            };
            if stored.session.subagent.is_some() {
                self.view.status =
                    "Esta sesión pertenece a un subagente; retomá el trabajo desde la sesión padre"
                        .to_owned();
                return;
            }
            let replay = match replay_stored_native(&self.data_root, &stored) {
                Ok(replay) => replay,
                Err(error) => {
                    self.view.status = error;
                    return;
                }
            };
            if !replay.can_start_new_turn {
                self.view.status = recovery_status(&replay.recovery).to_owned();
                return;
            }
            if replay.checkpoint.as_ref().is_some_and(|checkpoint| {
                checkpoint.compatibility
                    != native_checkpoint_compatibility(
                        &self.view.native_provider_id,
                        &model_id,
                        wire_api,
                    )
            }) {
                self.view.status =
                    "El checkpoint pertenece a otro proveedor o modelo; requiere migración explícita"
                        .to_owned();
                return;
            }
            if replay
                .history
                .iter()
                .any(|message| !message.provider_output_items.is_empty())
                && stored
                    .events
                    .iter()
                    .rev()
                    .find_map(|event| match &event.kind {
                        AgentEventKind::TurnAdmitted { model, .. }
                        | AgentEventKind::TurnAdmittedWithImages { model, .. } => {
                            Some(model.as_str())
                        }
                        _ => None,
                    })
                    != Some(model_id.as_str())
            {
                self.view.status =
                    "El historial Responses usa otro modelo; compactalo antes de cambiar de modelo"
                        .to_owned();
                return;
            }
            let plan_snapshot = if request.mode == TurnMode::Build {
                replay.latest_plan.clone()
            } else {
                None
            };
            request.history = replay.history;
            request.sequence_start = replay.last_sequence;
            let token_budget =
                native_provider::verified_token_budget(&self.view.native_provider_id, &model_id);
            let auto_checkpoints = if token_budget.is_some() {
                match CheckpointFileStore::new(self.data_root.join("sessions")) {
                    Ok(checkpoints) => Some(Arc::new(checkpoints)),
                    Err(error) => {
                        self.view.status = format!("No se pudo abrir checkpoints: {error}");
                        return;
                    }
                }
            } else {
                None
            };
            let base_tools = match self.native_parent_tools(
                request
                    .session_id
                    .as_deref()
                    .expect("session id was assigned"),
                &stored.session.workspace,
            ) {
                Ok(tools) => tools,
                Err(error) => {
                    self.view.status = error;
                    return;
                }
            };
            let child_tools: Arc<dyn ToolExecutor> = Arc::new(
                LocalTools::new()
                    .with_permissions(self.agent_tool_settings.permissions())
                    .with_command_isolation(self.agent_tool_settings.command_isolation()),
            );
            let policy = match self.native_permission_policy() {
                Ok(policy) => policy,
                Err(error) => {
                    self.view.status = error;
                    return;
                }
            };
            let instructions =
                system_instructions_with_goal(native_instructions, active_goal.as_deref());
            let command_hooks = native_hooks.expect("native hooks were validated");
            let coordinator =
                match SubagentCoordinator::new(store.clone(), SubagentLimits::default()) {
                    Ok(coordinator) => Arc::new(coordinator),
                    Err(error) => {
                        self.view.status =
                            format!("No se pudo iniciar la coordinación de subagentes: {error}");
                        return;
                    }
                };
            let mut child_worker = SubagentWorker::new(
                coordinator.clone(),
                store.clone(),
                model.clone(),
                child_tools,
                self.view.native_provider_id.clone(),
                endpoint.clone(),
            )
            .with_permission_policy(policy.clone())
            .with_session_approvals(self.agent_tool_settings.command_isolation())
            .with_event_bridge(self.subagent_bridge.clone())
            .with_command_hooks(command_hooks.clone())
            .with_nested_subagents(SubagentToolLimits::default())
            .with_system_instructions(instructions.clone());
            if let Some(budget) = token_budget {
                child_worker = child_worker.with_token_budget_policy(budget);
            }
            if let Some(checkpoints) = &auto_checkpoints {
                child_worker = child_worker.with_auto_compaction(
                    checkpoints.clone(),
                    native_checkpoint_compatibility(
                        &self.view.native_provider_id,
                        &model_id,
                        wire_api,
                    ),
                );
            }
            let lease = Arc::new(lease);
            let tools: Arc<dyn ToolExecutor> = match SubagentToolExecutor::new(
                base_tools,
                child_worker,
                coordinator,
                store.clone(),
                lease.clone(),
                SubagentToolLimits::default(),
            ) {
                Ok(tools) => Arc::new(tools),
                Err(error) => {
                    self.view.status =
                        format!("No se pudieron preparar las herramientas de subagentes: {error}");
                    return;
                }
            };
            let turn_id = Uuid::new_v4().to_string();
            let session_id = request
                .session_id
                .as_deref()
                .expect("session id was assigned");
            let admission = match store.admit_native_turn_with_images(
                session_id,
                &turn_id,
                &request.prompt,
                &model_id,
                request.mode,
                &native_images,
            ) {
                Ok(admission) => admission,
                Err(error) => {
                    self.view.status = format!("No se pudo admitir el turno: {error}");
                    return;
                }
            };
            request.sequence_start = admission.sequence;
            self.last_sequence = admission.sequence;
            let mut runner = AgentRunner::new(model, tools)
                .with_command_hooks(command_hooks)
                .with_permission_policy(policy)
                .with_session_approvals(store.clone(), self.agent_tool_settings.command_isolation())
                .with_plan_snapshot(plan_snapshot)
                .with_system_instructions(instructions)
                .with_native_runtime_identity(&self.view.native_provider_id, endpoint)
                .with_event_journal(store.clone())
                .with_native_steering(store.clone());
            if !native_mcp_servers.is_empty() {
                runner = runner
                    .with_tool_preparation(Arc::new(McpToolPreparation::new(native_mcp_servers)));
            }
            if let (Some(budget), Some(checkpoints)) = (token_budget, auto_checkpoints) {
                runner = runner
                    .with_token_budget_policy(budget)
                    .with_auto_compaction(
                        store.clone(),
                        checkpoints,
                        native_checkpoint_compatibility(
                            &self.view.native_provider_id,
                            &model_id,
                            wire_api,
                        ),
                    );
            }
            ActiveRun::Native(runner.start_with_shared_lease(request, lease))
        } else {
            if let Some(objective) = active_goal.as_deref() {
                request.history.insert(
                    0,
                    AgentMessage::system(composer_goal_instruction(objective)),
                );
            }
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
        let native_run =
            self.active_resume.is_some() || matches!(&self.active_run, Some(ActiveRun::Native(_)));
        let persisted = if native_run {
            true
        } else if let Some(store) = &self.store {
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
            AgentEventKind::ModelResponseCommitted { .. }
            | AgentEventKind::CheckpointCommitted { .. }
                if persisted
                    && self.view.selected_session.as_deref() == Some(event.session_id.as_str()) =>
            {
                self.refresh_composer_context_percent();
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
        let native_run = matches!(run, ActiveRun::Native(_));
        let (events, disconnected) = run.drain();
        let journal_error = disconnected.then(|| run.journal_error()).flatten();
        let mut transcript_changed = false;
        for event in events {
            transcript_changed |= self.process_event(event);
        }
        if transcript_changed {
            self.view.transcript = self.projection.items().to_vec();
        }
        if disconnected {
            if let Some(error) = journal_error {
                self.persistence_error = Some(error);
            }
            if native_run {
                if let (Some(store), Some(id)) = (&self.store, &self.view.selected_session) {
                    if let Ok(Some(stored)) = store.load(id) {
                        self.projection = TranscriptProjection::from_events(&stored.events);
                        self.view.transcript = self.projection.items().to_vec();
                    }
                }
            }
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

    fn start_resume(&mut self) {
        if self.view.running || self.active_run.is_some() || self.active_compact.is_some() {
            self.view.status = "Espera a que termine la operación activa".to_owned();
            return;
        }
        let Some(session_id) = self.view.selected_session.clone() else {
            self.view.status = "Selecciona una sesión nativa para reanudar".to_owned();
            return;
        };
        if self
            .background_automation_runs
            .iter()
            .any(|run| run.session_id == session_id)
        {
            self.view.status = "Espera a que termine la automatización".to_owned();
            return;
        }
        let Some(store) = self.store.clone() else {
            self.view.status = "El historial no está disponible".to_owned();
            return;
        };
        let stored = match store.load(&session_id) {
            Ok(Some(stored)) if stored.session.backend == "native" => stored,
            Ok(Some(_)) => {
                self.view.status = "Esta sesión usa un agente CLI".to_owned();
                return;
            }
            Ok(None) => {
                self.view.status = "La sesión ya no existe".to_owned();
                return;
            }
            Err(error) => {
                self.view.status = format!("No se pudo leer la sesión: {error}");
                return;
            }
        };
        let replay = match replay_stored_native(&self.data_root, &stored) {
            Ok(replay) => replay,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        if replay.recovery == ReplayRecovery::Idle {
            if stored.session.subagent.is_some() {
                self.finalize_resolved_child(&stored.session.id);
            } else {
                self.view.status = "La sesión no tiene un turno interrumpido".to_owned();
            }
            return;
        }
        if let ReplayRecovery::AbortedToolBatch { next_tool_call } = &replay.recovery {
            let Some(batch) = &replay.pending_tool_batch else {
                self.view.status = "El lote interrumpido no se pudo verificar".to_owned();
                return;
            };
            self.view.pending_batch_discard = Some(AbortedToolBatchView {
                session_id,
                next_call_id: next_tool_call.id.clone(),
                next_tool_name: next_tool_call.name.clone(),
                next_arguments: serde_json::to_string_pretty(&next_tool_call.arguments)
                    .unwrap_or_default(),
                completed_calls: batch.next_call_index,
                remaining_calls: batch
                    .assistant
                    .tool_calls
                    .len()
                    .saturating_sub(batch.next_call_index),
            });
            self.view.show_settings = false;
            self.visit_navigation();
            self.view.status = "Decide qué hacer con las llamadas pendientes".to_owned();
            return;
        }
        if let ReplayRecovery::ToolOutcomeUncertain { tool_call } = &replay.recovery {
            self.view.pending_tool_verification = Some(ToolVerificationView {
                session_id,
                call_id: tool_call.id.clone(),
                name: tool_call.name.clone(),
                arguments: serde_json::to_string_pretty(&tool_call.arguments).unwrap_or_default(),
                error: None,
            });
            self.view.tool_verification_output.clear();
            self.view.tool_verification_is_error = false;
            self.view.show_settings = false;
            self.visit_navigation();
            self.view.status =
                "Verifica el efecto y registra su resultado para cerrar la llamada pendiente"
                    .to_owned();
            return;
        }
        if stored.session.subagent.is_some() {
            self.start_child_resume(stored, replay);
            return;
        }
        let connection = match self.native_settings().resolve_at(&self.data_root) {
            Ok(connection) => connection,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let model_id = connection.model_id.clone();
        let endpoint = connection.endpoint.clone();
        let provider_id = self.view.native_provider_id.clone();
        let wire_api = connection.wire_api;
        let compatibility = native_checkpoint_compatibility(&provider_id, &model_id, wire_api);
        if replay
            .checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.compatibility != compatibility)
        {
            self.view.status = "El checkpoint pertenece a otro proveedor o modelo".to_owned();
            return;
        }
        if replay
            .active_turn
            .as_ref()
            .and_then(|turn| turn.model.as_deref())
            .is_some_and(|admitted_model| admitted_model != model_id)
        {
            self.view.status = "Selecciona el modelo del turno interrumpido".to_owned();
            return;
        }
        // A model without a verified window can still resume. It follows the
        // same no-auto-compaction behavior as its original native run.
        let budget_policy = native_provider::verified_token_budget(&provider_id, &model_id);
        if connection.api_key.is_none() && provider_id != "custom" {
            self.view.status = "Falta la clave API del proveedor nativo".to_owned();
            return;
        }
        let model = match native_model_backend(
            connection,
            &provider_id,
            Some(&self.view.composer_effort),
            self.view.hosted_web_search_mode,
            &self.data_root,
            self.view.catalog.as_deref(),
            &self.model_discoveries,
        ) {
            Ok(model) => model,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let workspace = &stored.session.workspace;
        let goal_key = (
            workspace.to_string_lossy().into_owned(),
            Some(session_id.clone()),
        );
        let system_instructions = match native_resume_instructions(
            &self.data_root,
            workspace,
            self.composer_goals.get(&goal_key).map(String::as_str),
            replay.active_system_instructions.as_deref(),
        ) {
            Ok(instructions) => instructions,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let tool_preparation = if replay
            .active_turn
            .as_ref()
            .is_some_and(|turn| turn.mode == TurnMode::Build)
        {
            match McpSettings::load(&self.data_root, workspace) {
                Ok(settings) => {
                    let enabled = settings.enabled_servers().cloned().collect::<Vec<_>>();
                    (!enabled.is_empty()).then(|| {
                        Arc::new(McpToolPreparation::new(enabled))
                            as Arc<dyn openide_harness::ToolPreparation>
                    })
                }
                Err(error) => {
                    self.view.status = error;
                    return;
                }
            }
        } else {
            None
        };
        let checkpoints = match CheckpointFileStore::new(self.data_root.join("sessions")) {
            Ok(checkpoints) => Arc::new(checkpoints),
            Err(error) => {
                self.view.status = format!("No se pudo abrir checkpoints: {error}");
                return;
            }
        };
        let command_hooks = match NativeHookSettings::load(&self.data_root)
            .and_then(|settings| settings.registry())
        {
            Ok(hooks) => Arc::new(hooks),
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let base_tools = match self.native_parent_tools(&session_id, workspace) {
            Ok(tools) => tools,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let permission_policy = match self.native_permission_policy() {
            Ok(policy) => policy,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let request = ResumeWorkerRequest {
            session_id,
            sessions: store,
            checkpoints,
            compatibility,
            model,
            provider: provider_id,
            endpoint,
            base_tools,
            child_tools: Arc::new(
                LocalTools::new()
                    .with_permissions(self.agent_tool_settings.permissions())
                    .with_command_isolation(self.agent_tool_settings.command_isolation()),
            ),
            event_bridge: Some(self.subagent_bridge.clone()),
            tool_preparation,
            permission_policy,
            command_isolation: self.agent_tool_settings.command_isolation(),
            command_hooks,
            budget_policy,
            system_instructions,
            limits: RunLimits::default(),
        };
        match ResumeWorkerHandle::start(request) {
            Ok(handle) => {
                self.active_resume = Some(handle);
                self.view.running = true;
                self.view.pending_permissions.clear();
                self.view.pending_tool_verification = None;
                self.view.tool_verification_output.clear();
                self.run_has_output = false;
                self.view.show_settings = false;
                self.visit_navigation();
                self.view.status = "Recuperando turno interrumpido...".to_owned();
            }
            Err(error) => self.view.status = error,
        }
    }

    fn start_child_resume(&mut self, stored: StoredSession, replay: NativeReplay) {
        let child_id = stored.session.id.clone();
        let Some(link) = stored.session.subagent.as_ref() else {
            self.view.status = "La sesión seleccionada no es un subagente".to_owned();
            return;
        };
        let admissions = stored
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::TurnAdmitted {
                    turn_id,
                    prompt,
                    model,
                    mode,
                }
                | AgentEventKind::TurnAdmittedWithImages {
                    turn_id,
                    prompt,
                    model,
                    mode,
                    ..
                } => Some((turn_id, prompt, model, *mode)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [(spawn_id, prompt, admitted_model, mode)] = admissions.as_slice() else {
            self.view.status =
                "El journal del subagente no tiene un único turno admitido".to_owned();
            return;
        };
        if *spawn_id != &link.spawn_id {
            self.view.status =
                "El turno admitido no coincide con el spawn del subagente".to_owned();
            return;
        }
        let connection = match self.native_settings().resolve_at(&self.data_root) {
            Ok(connection) => connection,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let provider = self.view.native_provider_id.clone();
        let model_id = connection.model_id.clone();
        let endpoint = connection.endpoint.clone();
        let wire_api = connection.wire_api;
        if *admitted_model != &model_id {
            self.view.status =
                "Selecciona el modelo original del subagente interrumpido".to_owned();
            return;
        }
        if connection.api_key.is_none() && provider != "custom" {
            self.view.status = "Falta la clave API del proveedor nativo".to_owned();
            return;
        }
        let model = match native_model_backend(
            connection,
            &provider,
            Some(&self.view.composer_effort),
            self.view.hosted_web_search_mode,
            &self.data_root,
            self.view.catalog.as_deref(),
            &self.model_discoveries,
        ) {
            Ok(model) => model,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let workspace = &stored.session.workspace;
        let parent_id = link.parent_session_id.clone();
        let goal_key = (
            workspace.to_string_lossy().into_owned(),
            Some(parent_id.clone()),
        );
        let instructions = match native_resume_instructions(
            &self.data_root,
            workspace,
            self.composer_goals.get(&goal_key).map(String::as_str),
            replay.active_system_instructions.as_deref(),
        ) {
            Ok(instructions) => instructions,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let Some(sessions) = self.store.clone() else {
            self.view.status = "El historial no está disponible".to_owned();
            return;
        };
        let coordinator =
            match SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()) {
                Ok(coordinator) => Arc::new(coordinator),
                Err(error) => {
                    self.view.status =
                        format!("No se pudo abrir la coordinación de subagentes: {error}");
                    return;
                }
            };
        let command_hooks = match NativeHookSettings::load(&self.data_root)
            .and_then(|settings| settings.registry())
        {
            Ok(hooks) => Arc::new(hooks),
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let child_tools: Arc<dyn ToolExecutor> = Arc::new(
            LocalTools::new()
                .with_permissions(self.agent_tool_settings.permissions())
                .with_command_isolation(self.agent_tool_settings.command_isolation()),
        );
        let policy = match self.native_permission_policy() {
            Ok(policy) => policy,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let mut worker = SubagentWorker::new(
            coordinator,
            sessions.clone(),
            model,
            child_tools,
            provider.clone(),
            endpoint,
        )
        .with_permission_policy(policy)
        .with_session_approvals(self.agent_tool_settings.command_isolation())
        .with_event_bridge(self.subagent_bridge.clone())
        .with_command_hooks(command_hooks)
        .with_nested_subagents(SubagentToolLimits::default())
        .with_system_instructions(instructions);
        if let Some(budget) = native_provider::verified_token_budget(&provider, &model_id) {
            let checkpoints = match CheckpointFileStore::new(self.data_root.join("sessions")) {
                Ok(checkpoints) => Arc::new(checkpoints),
                Err(error) => {
                    self.view.status = format!("No se pudo abrir checkpoints: {error}");
                    return;
                }
            };
            worker = worker
                .with_token_budget_policy(budget)
                .with_auto_compaction(
                    checkpoints,
                    native_checkpoint_compatibility(&provider, &model_id, wire_api),
                );
        }
        let request = SubagentRecoveryRequest {
            sessions,
            worker,
            parent_id,
            child_id: child_id.clone(),
            spawn_id: (*spawn_id).clone(),
            prompt: (*prompt).clone(),
            mode: *mode,
        };
        match SubagentRecoveryHandle::start(request) {
            Ok(handle) => {
                self.active_child_recovery = Some(handle);
                self.view.running = true;
                self.view.pending_permissions.clear();
                self.view.recoverable_subagent = None;
                self.view.show_settings = false;
                self.visit_navigation();
                self.view.status = "Recuperando subagente interrumpido...".to_owned();
            }
            Err(error) => self.view.status = error,
        }
    }

    fn finalize_resolved_child(&mut self, child_id: &str) {
        let Some(sessions) = self.store.clone() else {
            self.view.status = "El historial no está disponible".to_owned();
            return;
        };
        let stored = match sessions.load(child_id) {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                self.view.status = "La sesión hija ya no existe".to_owned();
                return;
            }
            Err(error) => {
                self.view.status = format!("No se pudo leer el subagente: {error}");
                return;
            }
        };
        let Some(link) = stored.session.subagent.as_ref() else {
            self.view.status = "La sesión seleccionada no es un subagente".to_owned();
            return;
        };
        let parent_lease = match sessions.try_acquire_native_lease(&link.parent_session_id) {
            Ok(lease) => lease,
            Err(error) => {
                self.view.status = format!("La sesión padre está en uso: {error}");
                return;
            }
        };
        let coordinator =
            match SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()) {
                Ok(coordinator) => coordinator,
                Err(error) => {
                    self.view.status =
                        format!("No se pudo abrir la coordinación de subagentes: {error}");
                    return;
                }
            };
        let result = finalize_resolved_subagent(&sessions, &coordinator, &parent_lease, child_id);
        drop(parent_lease);
        match result {
            Ok(terminal) => {
                self.refresh_sessions();
                self.view.status = format!(
                    "Subagente resuelto: {}",
                    subagent_status_label(&terminal.status)
                );
            }
            Err(error) => {
                self.refresh_sessions();
                self.view.status = format!("No se pudo cerrar el estado del subagente: {error}");
            }
        }
    }

    fn discard_aborted_batch(&mut self) {
        let Some(batch) = self.view.pending_batch_discard.clone() else {
            return;
        };
        if self.view.selected_session.as_deref() != Some(batch.session_id.as_str()) {
            self.view.pending_batch_discard = None;
            self.view.status = "La sesión cambió antes de resolver el lote".to_owned();
            return;
        }
        let Some(store) = self.store.as_ref() else {
            self.view.status = "El historial no está disponible".to_owned();
            return;
        };
        let checkpoints = match CheckpointFileStore::new(self.data_root.join("sessions")) {
            Ok(checkpoints) => checkpoints,
            Err(error) => {
                self.view.status = format!("No se pudo abrir checkpoints: {error}");
                return;
            }
        };
        let lease = match store.try_acquire_native_lease(&batch.session_id) {
            Ok(lease) => lease,
            Err(error) => {
                self.view.status = format!("La sesión nativa ya está en uso: {error}");
                return;
            }
        };
        let step =
            match discard_aborted_tool_batch(store, &checkpoints, &lease, &batch.next_call_id) {
                Ok(step) => step,
                Err(error) => {
                    self.view.status = format!("No se pudo descartar el lote: {error}");
                    return;
                }
            };
        drop(lease);
        let stored = match store.load(&batch.session_id) {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                self.view.pending_batch_discard = None;
                self.view.status = "El lote se descartó, pero la sesión ya no aparece".to_owned();
                return;
            }
            Err(error) => {
                self.view.pending_batch_discard = None;
                self.view.status =
                    format!("El lote se descartó, pero no se pudo recargar la sesión: {error}");
                return;
            }
        };
        self.last_sequence = stored.events.last().map_or(0, |event| event.sequence);
        self.projection = TranscriptProjection::from_events(&stored.events);
        self.view.transcript = self.projection.items().to_vec();
        self.history = step.replay.history;
        self.view.pending_batch_discard = None;
        self.refresh_sessions();
        if stored.session.subagent.is_some() && step.replay.can_start_new_turn {
            self.finalize_resolved_child(&batch.session_id);
        } else {
            self.view.status =
                "Llamadas pendientes descartadas; la sesión acepta otro turno".to_owned();
        }
    }

    fn verify_uncertain_tool(&mut self, pending: &ToolVerificationView, result: ToolOutput) {
        if self.view.selected_session.as_deref() != Some(pending.session_id.as_str()) {
            self.view.status = "Selecciona la sesión del resultado pendiente".to_owned();
            return;
        }
        let Some(store) = self.store.clone() else {
            self.view.status = "El historial no está disponible".to_owned();
            return;
        };
        let checkpoints = match CheckpointFileStore::new(self.data_root.join("sessions")) {
            Ok(checkpoints) => checkpoints,
            Err(error) => {
                self.view.status = format!("No se pudo abrir checkpoints: {error}");
                return;
            }
        };
        let lease = match store.try_acquire_native_lease(&pending.session_id) {
            Ok(lease) => lease,
            Err(error) => {
                self.view.status = format!("La sesión nativa ya está en uso: {error}");
                return;
            }
        };
        let step = match openide_harness::record_verified_tool_outcome(
            &store,
            &checkpoints,
            &lease,
            &pending.call_id,
            result,
            RunLimits::default().max_tool_output_chars,
        ) {
            Ok(step) => step,
            Err(error) => {
                self.view.status = format!("No se pudo registrar el resultado: {error}");
                return;
            }
        };
        drop(lease);
        let stored = match store.load(&pending.session_id) {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                self.view.status =
                    "El resultado se registró, pero la sesión ya no aparece".to_owned();
                return;
            }
            Err(error) => {
                self.view.status = format!(
                    "El resultado se registró, pero no se pudo recargar la sesión: {error}"
                );
                return;
            }
        };
        self.last_sequence = stored.events.last().map_or(0, |event| event.sequence);
        self.projection = TranscriptProjection::from_events(&stored.events);
        self.view.transcript = self.projection.items().to_vec();
        self.history = step.replay.history;
        self.view.pending_tool_verification = None;
        self.view.tool_verification_output.clear();
        self.refresh_sessions();
        if matches!(
            step.replay.recovery,
            ReplayRecovery::AbortedToolBatch { .. } | ReplayRecovery::ToolOutcomeUncertain { .. }
        ) {
            self.start_resume();
        } else if stored.session.subagent.is_some() && step.replay.can_start_new_turn {
            self.finalize_resolved_child(&pending.session_id);
        } else if step.replay.can_start_new_turn {
            self.view.status = "Resultado verificado; el turno anterior quedó cerrado".to_owned();
        } else {
            self.view.status = "Resultado verificado; reanuda para continuar el turno".to_owned();
        }
    }

    fn poll_resume(&mut self) -> bool {
        let (updates, disconnected) = {
            let Some(active) = &self.active_resume else {
                return false;
            };
            let mut updates = Vec::new();
            let disconnected = loop {
                match active.updates.try_recv() {
                    Ok(update) => updates.push(update),
                    Err(TryRecvError::Empty) => break false,
                    Err(TryRecvError::Disconnected) => break true,
                }
            };
            (updates, disconnected)
        };
        if updates.is_empty() && !disconnected {
            return false;
        }
        let mut transcript_changed = false;
        let mut finished = None;
        for update in updates {
            match update {
                ResumeUpdate::Events(events) => {
                    for event in events {
                        transcript_changed |= self.process_event(event);
                    }
                }
                ResumeUpdate::NeedsPermission {
                    request_id,
                    tool_call,
                } => {
                    if !self
                        .view
                        .pending_permissions
                        .iter()
                        .any(|permission| permission.request_id == request_id)
                    {
                        self.view.pending_permissions.push(PermissionView {
                            request_id,
                            tool_name: tool_call.name.clone(),
                            arguments: serde_json::to_string_pretty(&tool_call.arguments)
                                .unwrap_or_default(),
                        });
                    }
                    self.view.status = format!("Esperando permiso para {}", tool_call.name);
                }
                ResumeUpdate::NeedsVerification {
                    tool_call,
                    executor_error,
                } => {
                    self.view.pending_tool_verification = Some(ToolVerificationView {
                        session_id: self
                            .active_resume
                            .as_ref()
                            .expect("recovery is active while polling")
                            .session_id
                            .clone(),
                        call_id: tool_call.id.clone(),
                        name: tool_call.name.clone(),
                        arguments: serde_json::to_string_pretty(&tool_call.arguments)
                            .unwrap_or_default(),
                        error: executor_error.clone(),
                    });
                    self.view.tool_verification_output.clear();
                    self.view.tool_verification_is_error = false;
                    self.view.status = format!(
                        "{} tiene un resultado incierto ({}). Verifica el efecto antes de continuar.",
                        tool_call.name,
                        executor_error.unwrap_or_else(|| tool_call.id.clone())
                    );
                }
                ResumeUpdate::Finished(result) => finished = Some(result),
            }
        }
        if transcript_changed {
            self.view.transcript = self.projection.items().to_vec();
        }
        if finished.is_none() && !disconnected {
            // A terminal journal event may precede MCP shutdown; keep the
            // composer blocked until the worker releases its native lease.
            self.view.running = true;
            return true;
        }
        let result = finished
            .unwrap_or_else(|| Err("La recuperación terminó sin un resultado final".to_owned()));
        let active = self
            .active_resume
            .take()
            .expect("recovery was active while polling");
        self.view.running = false;
        self.view.pending_permissions.clear();
        self.view.pending_tool_verification = None;
        self.view.tool_verification_output.clear();
        if let Ok(Some(stored)) = self
            .store
            .as_ref()
            .expect("recovery requires a session store")
            .load(&active.session_id)
        {
            self.last_sequence = stored.events.last().map_or(0, |event| event.sequence);
            self.projection = TranscriptProjection::from_events(&stored.events);
            self.view.transcript = self.projection.items().to_vec();
        }
        self.rebuild_history();
        self.refresh_sessions();
        self.view.status = match result {
            Ok(ResumeDisposition::Complete) => "Turno recuperado".to_owned(),
            Ok(ResumeDisposition::AbortedTurn) => "El turno ya había terminado".to_owned(),
            Ok(ResumeDisposition::AbortedToolBatch { .. }) => {
                "Turno abortado con herramientas pendientes; requiere resolución".to_owned()
            }
            Ok(other) => format!("Recuperación pausada: {other:?}"),
            Err(error) => format!("No se pudo recuperar el turno: {error}"),
        };
        true
    }

    fn poll_child_recovery(&mut self) -> bool {
        let (updates, disconnected) = {
            let Some(active) = &self.active_child_recovery else {
                return false;
            };
            let mut updates = Vec::new();
            let disconnected = loop {
                match active.updates.try_recv() {
                    Ok(update) => updates.push(update),
                    Err(TryRecvError::Empty) => break false,
                    Err(TryRecvError::Disconnected) => break true,
                }
            };
            (updates, disconnected)
        };
        if updates.is_empty() && !disconnected {
            return false;
        }
        let mut finished = None;
        for update in updates {
            match update {
                SubagentRecoveryUpdate::NeedsPermission {
                    request_id,
                    tool_call,
                } => {
                    let active = self
                        .active_child_recovery
                        .as_ref()
                        .expect("child recovery exists while polling");
                    self.pending_child_recovery_permission = Some(SubagentPermissionView {
                        child_session_id: active.child_id.clone(),
                        parent_session_id: active.parent_id.clone(),
                        request_id,
                        tool_name: tool_call.name.clone(),
                        arguments: serde_json::to_string_pretty(&tool_call.arguments)
                            .unwrap_or_default(),
                    });
                    self.rebuild_live_subagent_views();
                    self.view.status = format!("Esperando permiso para {}", tool_call.name);
                }
                SubagentRecoveryUpdate::Finished(result) => finished = Some(result),
            }
        }
        if finished.is_none() && !disconnected {
            return true;
        }
        let result = finished.unwrap_or_else(|| {
            Err("La recuperación del subagente terminó sin resultado final".to_owned())
        });
        let active = self
            .active_child_recovery
            .take()
            .expect("child recovery exists while polling");
        self.view.running = false;
        self.pending_child_recovery_permission = None;
        self.rebuild_live_subagent_views();
        if self.view.selected_session.as_deref() == Some(active.child_id.as_str()) {
            if let Some(store) = &self.store {
                if let Ok(Some(stored)) = store.load(&active.child_id) {
                    self.last_sequence = stored.events.last().map_or(0, |event| event.sequence);
                    self.projection = TranscriptProjection::from_events(&stored.events);
                    self.view.transcript = self.projection.items().to_vec();
                    if let Ok(replay) = replay_stored_native(&self.data_root, &stored) {
                        self.history = replay.history;
                    }
                }
            }
        }
        self.refresh_sessions();
        match result {
            Ok(SubagentRecoveryState::Terminal(terminal)) => {
                self.view.status = format!(
                    "Subagente recuperado: {}",
                    subagent_status_label(&terminal.status)
                );
            }
            Ok(SubagentRecoveryState::Paused {
                reason:
                    SubagentRecoveryPause::ToolOutcomeUncertain { .. }
                    | SubagentRecoveryPause::AbortedToolBatch { .. },
                ..
            }) => self.start_resume(),
            Ok(SubagentRecoveryState::Paused {
                reason: SubagentRecoveryPause::NeedsPermission { .. },
                ..
            }) => {
                self.view.status = "El permiso del subagente sigue pendiente".to_owned();
            }
            Err(error) => {
                self.view.status = format!("No se pudo recuperar el subagente: {error}");
            }
        }
        true
    }

    fn start_compact(&mut self) {
        if self.active_compact.is_some() {
            self.view.status = "Ya se está compactando una sesión".to_owned();
            return;
        }
        if self.active_run.is_some() || self.active_resume.is_some() || self.view.running {
            self.view.status = "Espera a que termine el turno antes de compactar".to_owned();
            return;
        }
        let Some(session_id) = self.view.selected_session.clone() else {
            self.view.status = "Selecciona una sesión nativa para compactar".to_owned();
            return;
        };
        if self
            .background_automation_runs
            .iter()
            .any(|run| run.session_id == session_id)
        {
            self.view.status = "Espera a que termine la automatización".to_owned();
            return;
        }
        let Some(store) = self.store.clone() else {
            self.view.status = "El historial no está disponible".to_owned();
            return;
        };
        let stored = match store.load(&session_id) {
            Ok(Some(stored)) if stored.session.backend == "native" => stored,
            Ok(Some(_)) => {
                self.view.status = "Esta sesión usa un agente CLI".to_owned();
                return;
            }
            Ok(None) => {
                self.view.status = "La sesión ya no existe".to_owned();
                return;
            }
            Err(error) => {
                self.view.status = format!("No se pudo leer la sesión: {error}");
                return;
            }
        };
        let lease = match store.try_acquire_native_lease(&session_id) {
            Ok(lease) => lease,
            Err(error) => {
                self.view.status = format!("La sesión nativa ya está en uso: {error}");
                return;
            }
        };
        let replay = match replay_stored_native(&self.data_root, &stored) {
            Ok(replay) => replay,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        if !replay.can_start_new_turn {
            self.view.status = recovery_status(&replay.recovery).to_owned();
            return;
        }
        if replay.committed_messages.is_empty() {
            self.view.status = "No hay historial nuevo para compactar".to_owned();
            return;
        }
        let connection = match self.native_settings().resolve_at(&self.data_root) {
            Ok(connection) => connection,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let provider_id = self.view.native_provider_id.clone();
        let model_id = connection.model_id.clone();
        let wire_api = connection.wire_api;
        let compatibility = native_checkpoint_compatibility(&provider_id, &model_id, wire_api);
        if replay
            .checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.compatibility != compatibility)
        {
            self.view.status = "El checkpoint pertenece a otro proveedor o modelo".to_owned();
            return;
        }
        let policy = native_provider::verified_token_budget(&provider_id, &model_id);
        if connection.api_key.is_none() && provider_id != "custom" {
            self.view.status = "Falta la clave API del proveedor nativo".to_owned();
            return;
        }
        let model = match native_model_backend(
            connection,
            &provider_id,
            Some(&self.view.composer_effort),
            self.view.hosted_web_search_mode,
            &self.data_root,
            self.view.catalog.as_deref(),
            &self.model_discoveries,
        ) {
            Ok(model) => model,
            Err(error) => {
                self.view.status = error;
                return;
            }
        };
        let observed_input_tokens = stored.events.iter().rev().find_map(|event| {
            if let AgentEventKind::ModelResponseCommitted {
                usage: Some(usage), ..
            } = &event.kind
            {
                Some(usage.input_tokens)
            } else {
                None
            }
        });
        let estimated_input_tokens = replay
            .history
            .iter()
            .map(|message| {
                serde_json::to_vec(message)
                    .map(|encoded| encoded.len() as u64)
                    .unwrap_or(u64::MAX)
            })
            .fold(0u64, u64::saturating_add);
        let budget = match policy {
            Some(policy) => CheckpointBudgetSnapshot {
                context_window_tokens: policy.context_window_tokens,
                estimated_input_tokens,
                observed_input_tokens,
                output_reserve_tokens: policy.output_reserve_tokens,
                tool_schema_reserve_tokens: policy.tool_schema_reserve_tokens,
                limits_unverified: false,
            },
            None => {
                CheckpointBudgetSnapshot::unverified(estimated_input_tokens, observed_input_tokens)
            }
        };
        let checkpoint_root = self.data_root.join("sessions");
        let (sender, receiver) = mpsc::channel();
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let result = std::thread::Builder::new()
            .name("openide-native-compact".to_owned())
            .spawn(move || {
                let outcome = (|| {
                    let checkpoints = Arc::new(
                        CheckpointFileStore::new(checkpoint_root)
                            .map_err(|error| error.to_string())?,
                    );
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| error.to_string())?;
                    let coordinator = CheckpointCoordinator::new(
                        store,
                        checkpoints,
                        model,
                        compatibility,
                        budget,
                    );
                    runtime
                        .block_on(coordinator.compact(&lease, worker_cancel))
                        .map(|commit| commit.marker_event.sequence)
                        .map_err(|error| error.to_string())
                })();
                let _ = sender.send(outcome);
            });
        match result {
            Ok(_) => {
                self.active_compact = Some(ActiveCompact {
                    session_id,
                    receiver,
                    cancel,
                });
                self.view.status = "Compactando contexto...".to_owned();
            }
            Err(error) => self.view.status = format!("No se pudo iniciar la compactación: {error}"),
        }
    }

    fn poll_compact(&mut self) -> bool {
        let Some(active) = &self.active_compact else {
            return false;
        };
        let outcome = match active.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                Err("El proceso de compactación terminó sin resultado".to_owned())
            }
        };
        let active = self
            .active_compact
            .take()
            .expect("compact operation was active");
        self.refresh_sessions();
        if self.view.selected_session.as_deref() == Some(active.session_id.as_str()) {
            match outcome {
                Ok(_) => {
                    self.rebuild_history();
                    self.view.status = "Contexto compactado y checkpoint confirmado".to_owned();
                }
                Err(error) => self.view.status = format!("No se pudo compactar: {error}"),
            }
        }
        true
    }

    fn rebuild_history(&mut self) {
        if let (Some(store), Some(id)) = (&self.store, &self.view.selected_session) {
            match store.load(id) {
                Ok(Some(stored)) if stored.session.backend == "native" => {
                    match replay_stored_native(&self.data_root, &stored) {
                        Ok(replay) => {
                            self.history = replay.history;
                            if !replay.can_start_new_turn {
                                self.view.status = recovery_status(&replay.recovery).to_owned();
                            }
                        }
                        Err(error) => {
                            self.history.clear();
                            self.persistence_error = Some(error);
                        }
                    }
                }
                Ok(Some(stored)) => self.history = history_from_events(&stored.events),
                Ok(None) => self.history.clear(),
                Err(error) => {
                    self.history.clear();
                    self.persistence_error = Some(error.to_string());
                }
            }
        }
        self.refresh_composer_context_percent();
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
        let limit = command_center::DEFAULT_RESULT_LIMIT;
        let mut results = command_center::search(
            &self.view.command_query,
            self.view.command_scope,
            &sessions,
            &files,
            limit + 1,
        );
        let display_limited = results.len() > limit;
        results.truncate(limit);
        self.view.command_results = results;
        let scope =
            command_center::parse_query(&self.view.command_query, self.view.command_scope).scope;
        self.view.command_search_truncated = display_limited
            || self.search_response.as_ref().is_some_and(|response| {
                (matches!(scope, Scope::All | Scope::Sessions) && response.session_has_more)
                    || (matches!(scope, Scope::All | Scope::Files) && response.files_truncated)
            });
        self.view.command_selected_index = self
            .view
            .command_selected_index
            .min(self.view.command_results.len().saturating_sub(1));
    }

    fn request_command_search(&mut self) {
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_response = None;
        self.search_in_flight = false;
        let scope =
            command_center::parse_query(&self.view.command_query, self.view.command_scope).scope;
        let needs_worker = scope != Scope::Commands;
        self.search_pending_at = (needs_worker && self.search_worker.is_some()).then(Instant::now);
        self.view.command_search_loading = self.search_pending_at.is_some();
        self.view.command_search_errors = if needs_worker && self.search_worker.is_none() {
            vec!["El índice de tareas y archivos no está disponible".to_owned()]
        } else {
            Vec::new()
        };
        self.view.command_search_truncated = false;
        self.refresh_command_results();
    }

    fn apply_command_search_response(&mut self, response: SearchResponse) -> bool {
        if !response.matches(self.search_generation, Path::new(&self.view.workspace)) {
            return false;
        }
        let scope =
            command_center::parse_query(&self.view.command_query, self.view.command_scope).scope;
        let mut errors = Vec::new();
        if matches!(scope, Scope::All | Scope::Sessions) {
            if let Some(error) = &response.session_error {
                errors.push(format!("Tareas: {error}"));
            }
            if !response.session_diagnostics.is_empty() {
                errors.push(format!(
                    "{} tarea(s) no se pudieron leer",
                    response.session_diagnostics.len()
                ));
            }
        }
        if matches!(scope, Scope::All | Scope::Files) {
            if let Some(error) = &response.file_error {
                errors.push(format!("Archivos: {error}"));
            }
        }
        self.search_in_flight = false;
        self.view.command_search_loading = false;
        self.view.command_search_errors = errors;
        self.search_response = Some(response);
        self.refresh_command_results();
        true
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
                        if !self.search_in_flight {
                            self.view.command_search_loading = false;
                            self.view.command_search_errors =
                                vec!["No se pudo iniciar la búsqueda".to_owned()];
                            changed = true;
                        }
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
                changed |= self.apply_command_search_response(response);
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

    fn open_composer_folder_picker(&mut self, ctx: &egui::Context) {
        if self.composer_folder_response.is_some() || self.view.running {
            return;
        }
        self.view.composer_add_menu_open = false;
        let workspace = self.view.workspace.clone();
        let initial = PathBuf::from(&workspace);
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        match std::thread::Builder::new()
            .name("composer-folder-picker".to_owned())
            .spawn(move || {
                let result = choose_workspace_folder(&initial, "Adjuntar carpeta del proyecto");
                let _ = sender.send((workspace, result));
                repaint.request_repaint();
            }) {
            Ok(_) => self.composer_folder_response = Some(receiver),
            Err(error) => {
                self.view.status = format!("No se pudo abrir el selector de carpetas: {error}");
            }
        }
    }

    fn poll_composer_folder_picker(&mut self) -> bool {
        let Some(receiver) = &self.composer_folder_response else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                self.composer_folder_response = None;
                self.view.status = "El selector de carpetas se cerró inesperadamente".to_owned();
                return true;
            }
        };
        self.composer_folder_response = None;
        if result.0 == self.view.workspace {
            match result.1 {
                Ok(Some(path)) => self.attach_composer_file(&path),
                Ok(None) => {}
                Err(error) => self.view.status = error,
            }
        }
        true
    }

    fn attach_composer_file(&mut self, candidate: &Path) {
        if self.view.composer_attachments.len() >= MAX_COMPOSER_ATTACHMENTS {
            self.view.status = format!("Máximo de {MAX_COMPOSER_ATTACHMENTS} adjuntos");
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
                self.view.status = format!(
                    "{} adjunto: {}",
                    if file.is_dir { "Carpeta" } else { "Archivo" },
                    file.label
                );
                self.view.composer_attachments.push(file);
                let key = self.draft_key();
                self.attachment_drafts
                    .insert(key, self.view.composer_attachments.clone());
                self.draft_dirty_at = Some(Instant::now());
                self.save_draft();
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
            Target::CompactContext => self.start_compact(),
            Target::ResumeTurn => self.start_resume(),
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
            Target::OpenGitReview => {
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
        self.view.composer_context_percent = None;
        self.refresh_mcp_settings();
        self.refresh_native_skills();
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
                let _ = sender.send(choose_workspace_folder(
                    &initial,
                    "Seleccionar carpeta de proyecto",
                ));
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
            UiAction::SliderChanged {
                id,
                fraction,
                committed,
            } if id == "composer_effort_slider" => {
                if !self.view.running && self.composer_slider_effort(fraction).is_some() {
                    if committed {
                        let effort = self.composer_slider_effort(fraction).unwrap();
                        self.select_composer_effort(&effort);
                        // Keep the selector visible so the settled tier and model
                        // can be reviewed after a drag.
                        self.view.composer_model_menu_open = true;
                        self.view.composer_advanced_open = true;
                    } else {
                        self.view.composer_effort_preview = Some(if fraction.is_finite() {
                            fraction.clamp(0.0, 1.0)
                        } else {
                            0.0
                        });
                    }
                }
            }
            UiAction::Clicked { id } if id == "open_command_center" => {
                self.open_command_center();
            }
            UiAction::Clicked { id } if id == "composer_add_context" => {
                if !self.view.running && !self.view.background_run_selected {
                    self.view.composer_add_menu_open = !self.view.composer_add_menu_open;
                    if !self.view.composer_add_menu_open {
                        self.view.composer_goal_editor_open = false;
                    }
                    self.view.composer_project_menu_open = false;
                }
            }
            UiAction::Clicked { id } if id == "composer_microphone" => {
                self.toggle_dictation(ctx);
            }
            UiAction::Clicked { id } if id == "composer_add_dismiss" => {
                self.view.composer_add_menu_open = false;
                self.view.composer_goal_editor_open = false;
            }
            UiAction::Clicked { id } if id == "composer_goal_open" => {
                if !self.view.running && !self.view.background_run_selected {
                    self.view.composer_add_menu_open = true;
                    self.view.composer_goal_draft =
                        self.view.composer_goal.clone().unwrap_or_default();
                    self.tree.set_input_value(
                        "composer_goal_draft",
                        self.view.composer_goal_draft.clone(),
                    );
                    self.view.composer_goal_editor_open = true;
                }
            }
            UiAction::Clicked { id } if id == "composer_goal_save" => {
                if self.view.composer_goal_editor_open {
                    self.set_composer_goal(Some(self.view.composer_goal_draft.clone()));
                }
            }
            UiAction::Clicked { id } if id == "composer_goal_clear" => {
                self.set_composer_goal(None);
            }
            UiAction::Clicked { id } if id == "composer_goal_cancel" => {
                self.view.composer_goal_editor_open = false;
                self.view.composer_add_menu_open = false;
                self.view.composer_goal_draft = self.view.composer_goal.clone().unwrap_or_default();
            }
            UiAction::Clicked { id } if id.starts_with("composer_plugin_") => {
                let query = match id.as_str() {
                    "composer_plugin_documents" => Some("document"),
                    "composer_plugin_spreadsheets" => Some("spreadsheet"),
                    "composer_plugin_presentations" => Some("presentation"),
                    "composer_plugin_code_blocks" => Some("code"),
                    _ => None,
                };
                if let Some(query) = query {
                    self.view.composer_add_menu_open = false;
                    self.view.marketplace.query = query.to_owned();
                    self.view.marketplace.installed_only = false;
                    self.view.show_settings = false;
                    self.view.main_page = MainPage::Marketplace;
                    self.marketplace_operation(MarketplaceOperation::Load);
                    self.visit_navigation();
                }
            }
            UiAction::Clicked { id } if id == "composer_add_files" || id == "composer_add_file" => {
                if !self.view.running && !self.view.background_run_selected {
                    self.open_composer_file_picker();
                }
            }
            UiAction::Clicked { id } if id == "composer_add_folder" => {
                if !self.view.running && !self.view.background_run_selected {
                    self.open_composer_folder_picker(ctx);
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
                        self.draft_dirty_at = Some(Instant::now());
                        self.save_draft();
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
            UiAction::Clicked { id } if id.starts_with("permission_allow_session:") => {
                let request_id = id.trim_start_matches("permission_allow_session:");
                self.answer_permission_for_session(request_id);
            }
            UiAction::Clicked { id } if id.starts_with("permission_deny:") => {
                let request_id = id.trim_start_matches("permission_deny:");
                self.answer_permission(request_id, false);
            }
            UiAction::Clicked { id } if id.starts_with("subagent_permission_allow:") => {
                if let Some((child_id, request_id)) = id
                    .trim_start_matches("subagent_permission_allow:")
                    .split_once(':')
                {
                    self.answer_subagent_permission(child_id, request_id, PermissionChoice::Allow);
                }
            }
            UiAction::Clicked { id } if id.starts_with("subagent_permission_allow_session:") => {
                if let Some((child_id, request_id)) = id
                    .trim_start_matches("subagent_permission_allow_session:")
                    .split_once(':')
                {
                    self.answer_subagent_permission(
                        child_id,
                        request_id,
                        PermissionChoice::AllowSession,
                    );
                }
            }
            UiAction::Clicked { id } if id.starts_with("subagent_permission_deny:") => {
                if let Some((child_id, request_id)) = id
                    .trim_start_matches("subagent_permission_deny:")
                    .split_once(':')
                {
                    self.answer_subagent_permission(child_id, request_id, PermissionChoice::Deny);
                }
            }
            UiAction::Clicked { id } if id.starts_with("subagent_cancel:") => {
                self.cancel_subagent(id.trim_start_matches("subagent_cancel:"));
            }
            UiAction::Clicked { id } if id.starts_with("subagent_resume:") => {
                let child_id = id.trim_start_matches("subagent_resume:");
                if self.view.selected_session.as_deref() == Some(child_id)
                    && self
                        .view
                        .recoverable_subagent
                        .as_ref()
                        .is_some_and(|child| child.child_session_id == child_id)
                {
                    self.start_resume();
                } else {
                    self.view.status = "El subagente ya no está listo para recuperación".to_owned();
                }
            }
            UiAction::Clicked { id } if id == "aborted_batch_defer" => {
                if self.view.pending_batch_discard.take().is_some() {
                    self.view.status =
                        "Las llamadas pendientes siguen sin resolver en el journal".to_owned();
                }
            }
            UiAction::Clicked { id } if id == "aborted_batch_discard" => {
                self.discard_aborted_batch();
            }
            UiAction::Clicked { id } if id == "tool_verification_toggle_error" => {
                if self.view.pending_tool_verification.is_some() {
                    self.view.tool_verification_is_error = !self.view.tool_verification_is_error;
                }
            }
            UiAction::Clicked { id } if id == "tool_verification_cancel" => {
                if let Some(resume) = &self.active_resume {
                    resume.cancel();
                    self.view.status =
                        "Cerrando recuperación; el resultado incierto seguirá pendiente".to_owned();
                } else if self.view.pending_tool_verification.take().is_some() {
                    self.view.tool_verification_output.clear();
                    self.view.status =
                        "El resultado incierto seguirá pendiente en el journal".to_owned();
                }
            }
            UiAction::Clicked { id } if id == "tool_verification_confirm" => {
                if let Some(pending) = &self.view.pending_tool_verification {
                    let output = &self.view.tool_verification_output;
                    if output.trim().is_empty() {
                        self.view.status =
                            "Describe el resultado verificado antes de continuar".to_owned();
                    } else if output.chars().count() > RunLimits::default().max_tool_output_chars {
                        self.view.status =
                            "El resultado verificado supera el límite de la herramienta".to_owned();
                    } else {
                        let pending = pending.clone();
                        let result = ToolOutput {
                            content: output.to_owned(),
                            is_error: self.view.tool_verification_is_error,
                        };
                        if let Some(resume) = &self.active_resume {
                            if resume.record_verified_tool_outcome(&pending.call_id, result) {
                                self.view.pending_tool_verification = None;
                                self.view.tool_verification_output.clear();
                                self.view.status =
                                    "Resultado verificado enviado a recuperación".to_owned();
                            } else {
                                self.view.status =
                                    "La recuperación ya no acepta este resultado".to_owned();
                            }
                        } else {
                            self.verify_uncertain_tool(&pending, result);
                        }
                    }
                }
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
                    } else if let Some(resume) = &self.active_resume {
                        resume.cancel();
                        self.view.status = "Cancelando recuperación...".to_owned();
                    } else if let Some(child) = &self.active_child_recovery {
                        child.cancel();
                        self.view.status = "Cancelando recuperación del subagente...".to_owned();
                    }
                } else {
                    self.start_run();
                }
            }
            UiAction::Clicked { id } if id == "run_steer" => self.queue_native_steer(),
            UiAction::Clicked { id } if id.starts_with("session_approval_revoke:") => {
                if let Some(key_digest) = id.strip_prefix("session_approval_revoke:") {
                    self.revoke_selected_session_approval(key_digest);
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
            UiAction::Clicked { id } if id.starts_with("native_command_terminate:") => {
                let command_id = id.trim_start_matches("native_command_terminate:");
                if self.view.selected_session.is_some()
                    && self.view.native_command_terminating.is_none()
                    && self
                        .view
                        .native_commands
                        .iter()
                        .any(|command| command.id == command_id && command.exit.is_none())
                {
                    self.view.pending_native_command_termination = Some(command_id.to_owned());
                }
            }
            UiAction::Clicked { id } if id.starts_with("native_command_terminate_confirm:") => {
                let command_id = id.trim_start_matches("native_command_terminate_confirm:");
                self.confirm_native_command_termination(command_id);
            }
            UiAction::Clicked { id } if id == "native_command_terminate_cancel" => {
                self.view.pending_native_command_termination = None;
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
            UiAction::Clicked { id } if id.starts_with("web_citation_open:") => {
                if let Some(url) = id
                    .trim_start_matches("web_citation_open:")
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| self.view.transcript.get(index))
                    .and_then(|item| match item {
                        crate::transcript::TranscriptItem::WebCitation { url, .. } => Some(url),
                        _ => None,
                    })
                    .and_then(|url| reqwest::Url::parse(url).ok())
                    .filter(|url| matches!(url.scheme(), "http" | "https"))
                {
                    ctx.open_url(egui::OpenUrl::new_tab(url.to_string()));
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
            UiAction::Clicked { id } if id.starts_with("dock_move_down:") => {
                if let Ok(raw) = id.trim_start_matches("dock_move_down:").parse::<u64>() {
                    if let Some(tab_id) = DockTabId::from_raw(raw) {
                        let scope = self.dock_scope();
                        let ids: Vec<DockTabId> = self
                            .dock
                            .visible_tabs(&scope)
                            .iter()
                            .map(|tab| tab.id)
                            .collect();
                        if let Some(index) = ids.iter().position(|id| *id == tab_id) {
                            if let Some(next) = ids.get(index + 1) {
                                if self.dock.reorder(&scope, tab_id, *next) {
                                    self.sync_dock_view();
                                }
                            }
                        }
                    }
                }
            }
            UiAction::Clicked { id } if id.starts_with("dock_reopen:") => {
                if let Ok(raw) = id.trim_start_matches("dock_reopen:").parse::<u64>() {
                    if let Some(closed_id) = DockTabId::from_raw(raw) {
                        let scope = self.dock_scope();
                        if self.dock.reopen_recent(&scope, closed_id).is_some() {
                            self.view.show_dock_overview = false;
                            self.sync_dock_view();
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
                if already_open {
                    self.view.group_rename_id = None;
                    self.view.group_rename_draft.clear();
                    self.group_focus_pending = false;
                }
                self.view.pending_delete_group_id = None;
                self.close_session_menu();
            }
            UiAction::Clicked { id } if id == "session_group_menu_dismiss" => {
                self.view.group_menu_id = None;
                self.view.group_rename_id = None;
                self.view.group_rename_draft.clear();
                self.group_focus_pending = false;
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
                self.view.group_menu_id = None;
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
            UiAction::Clicked { id } if id.starts_with("session_recover:") => {
                let session_id = id.trim_start_matches("session_recover:");
                if self
                    .view
                    .session_recovery
                    .get(session_id)
                    .is_some_and(SessionRecoveryView::can_open)
                {
                    self.close_session_menu();
                    self.open_session(session_id);
                    if self.view.selected_session.as_deref() == Some(session_id) {
                        self.start_resume();
                    }
                }
            }
            UiAction::Clicked { id } if id.starts_with("session_build_plan:") => {
                let session_id = id.trim_start_matches("session_build_plan:");
                self.refresh_sessions();
                if !self.view.running
                    && self
                        .view
                        .session_plans
                        .get(session_id)
                        .is_some_and(|plan| plan.can_build)
                {
                    self.close_session_menu();
                    if self.view.selected_session.as_deref() != Some(session_id) {
                        self.open_session(session_id);
                    }
                    if self.view.selected_session.as_deref() == Some(session_id)
                        && self.view.selected_backend == "native"
                    {
                        self.view.turn_mode = TurnMode::Build;
                        self.view.prompt = "Implementa el plan aprobado para esta sesión. Actualiza su progreso con update_plan.".into();
                        self.start_run();
                    }
                }
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
                } else if let Err(error) = self.ensure_archivable_native_session(session_id) {
                    self.view.status = error;
                } else if let Some(store) = &self.store {
                    match store.set_archived(session_id, true) {
                        Ok(_) => {
                            self.native_command_sessions.revoke_session(session_id);
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
                    self.view.session_menu_id = None;
                    self.rename_focus_pending = true;
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
                            Ok(Some(record))
                                if record.session.archived && record.session.subagent.is_some() =>
                            {
                                self.view.status =
                                    "Elimina la tarea raíz archivada para borrar también sus subagentes"
                                        .to_owned();
                            }
                            Ok(Some(record)) if record.session.archived => {
                                let known_before = store.list().unwrap_or_default();
                                let deletion = if record.session.backend == "native"
                                    && record.session.subagent.is_none()
                                {
                                    store.delete_native_tree(session_id)
                                } else {
                                    store.delete(session_id)
                                };
                                match deletion {
                                    Ok(_) => {
                                        let mut selected_deleted = false;
                                        if let Ok(current) = store.list() {
                                            let remaining = current
                                                .into_iter()
                                                .map(|session| session.id)
                                                .collect::<HashSet<_>>();
                                            for removed in known_before
                                                .iter()
                                                .filter(|session| !remaining.contains(&session.id))
                                            {
                                                self.native_command_sessions
                                                    .revoke_session(&removed.id);
                                                self.navigation
                                                    .forget_task(&removed.workspace, &removed.id);
                                                selected_deleted |=
                                                    self.view.selected_session.as_deref()
                                                        == Some(removed.id.as_str());
                                            }
                                        }
                                        self.native_command_sessions.revoke_session(session_id);
                                        self.navigation
                                            .forget_task(&record.session.workspace, session_id);
                                        selected_deleted |= self.view.selected_session.as_deref()
                                            == Some(session_id);
                                        self.close_session_menu();
                                        if selected_deleted {
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
                                        self.native_command_sessions.revoke_session(session_id);
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
                    if page != SettingsPage::Mcp {
                        self.view.mcp_editor_open = false;
                        self.view.pending_mcp_delete_id = None;
                    }
                    if page != SettingsPage::Hooks {
                        self.view.native_hooks.clear_draft();
                    } else {
                        self.view.native_hooks =
                            HookUiState::from_load(NativeHookSettings::load(&self.data_root));
                    }
                    if page == SettingsPage::Skills {
                        self.refresh_native_skills();
                    }
                    if page == SettingsPage::Commands {
                        self.refresh_exec_policy_settings();
                    }
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
            UiAction::Clicked { id } if id == "exec_rule_new" => {
                if self.exec_policy_load_error.is_none() {
                    self.view.native_exec_rules.clear_draft();
                    self.view.native_exec_rules.editor_open = true;
                    self.clear_settings_notice();
                }
            }
            UiAction::Clicked { id } if id.starts_with("exec_rule_select:") => {
                if self.exec_policy_load_error.is_none() {
                    if let Ok(index) = id.trim_start_matches("exec_rule_select:").parse() {
                        if let Err(error) = self.view.native_exec_rules.select(index) {
                            self.set_settings_notice(error, true);
                        } else {
                            self.clear_settings_notice();
                        }
                    }
                }
            }
            UiAction::Clicked { id } if id.starts_with("exec_rule_delete:") => {
                if !self.view.native_exec_rules.editor_open {
                    if let Ok(index) = id.trim_start_matches("exec_rule_delete:").parse() {
                        self.delete_exec_rule(index);
                    }
                }
            }
            UiAction::Clicked { id } if id == "exec_rule_decision:ask" => {
                if self.view.native_exec_rules.editor_open {
                    self.view.native_exec_rules.draft_decision = ExecDecision::Ask;
                }
            }
            UiAction::Clicked { id } if id == "exec_rule_decision:deny" => {
                if self.view.native_exec_rules.editor_open {
                    self.view.native_exec_rules.draft_decision = ExecDecision::Deny;
                }
            }
            UiAction::Clicked { id } if id == "exec_rule_cancel" => {
                self.view.native_exec_rules.clear_draft();
                self.clear_settings_notice();
            }
            UiAction::Clicked { id } if id == "exec_rule_save" => {
                if self.view.native_exec_rules.editor_open {
                    self.save_exec_rule_draft();
                }
            }
            UiAction::Clicked { id } if id == "exec_rule_reset" => {
                if self.exec_policy_load_error.is_some() {
                    self.view.native_exec_rules.reset_confirm_open = true;
                }
            }
            UiAction::Clicked { id } if id == "exec_rule_reset_cancel" => {
                self.view.native_exec_rules.reset_confirm_open = false;
            }
            UiAction::Clicked { id } if id == "exec_rule_reset_confirm" => {
                if self.view.native_exec_rules.reset_confirm_open {
                    self.reset_invalid_exec_rules();
                }
            }
            UiAction::Clicked { id } if id == "native_skill_refresh" => {
                self.refresh_native_skills();
                if self.view.native_skills.load_error.is_none()
                    && self.view.native_skills.catalog_error.is_none()
                    && self.view.native_skills.activation_error.is_none()
                {
                    self.set_settings_notice("Catálogo de skills locales actualizado", false);
                } else {
                    self.set_settings_notice(
                        "La selección o el catálogo de skills locales requiere atención",
                        true,
                    );
                }
            }
            UiAction::Clicked { id } if id.starts_with("native_skill_toggle:") => {
                if let Ok(index) = id.trim_start_matches("native_skill_toggle:").parse() {
                    match self.view.native_skills.toggle(index) {
                        Ok(selections) => self.save_native_skill_selections(selections),
                        Err(error) => self.set_settings_notice(error, true),
                    }
                }
            }
            UiAction::Clicked { id } if id.starts_with("native_skill_remove:") => {
                if let Ok(index) = id.trim_start_matches("native_skill_remove:").parse() {
                    match self.view.native_skills.remove(index) {
                        Ok(selections) => self.save_native_skill_selections(selections),
                        Err(error) => self.set_settings_notice(error, true),
                    }
                }
            }
            UiAction::Clicked { id } if id == "native_skill_reset" => {
                if self.view.native_skills.load_error.is_some() {
                    self.view.native_skills.reset_confirm_open = true;
                }
            }
            UiAction::Clicked { id } if id == "native_skill_reset_cancel" => {
                self.view.native_skills.reset_confirm_open = false;
            }
            UiAction::Clicked { id } if id == "native_skill_reset_confirm" => {
                if self.view.native_skills.reset_confirm_open {
                    self.reset_native_skills();
                }
            }
            UiAction::Clicked { id } if id == "hook_new" => {
                if self.view.native_hooks.load_error.is_none() {
                    self.view.native_hooks.clear_draft();
                    self.view.native_hooks.editor_open = true;
                    self.clear_settings_notice();
                }
            }
            UiAction::Clicked { id } if id.starts_with("hook_select:") => {
                if self.view.native_hooks.load_error.is_none() {
                    self.select_command_hook(id.trim_start_matches("hook_select:"));
                }
            }
            UiAction::Clicked { id } if id.starts_with("hook_delete:") => {
                if !self.view.native_hooks.editor_open {
                    self.delete_command_hook(id.trim_start_matches("hook_delete:"));
                }
            }
            UiAction::Clicked { id } if id == "hook_phase:pre_tool_use" => {
                self.view.native_hooks.draft_phase = HookPhase::PreToolUse;
            }
            UiAction::Clicked { id } if id == "hook_phase:post_tool_use" => {
                self.view.native_hooks.draft_phase = HookPhase::PostToolUse;
            }
            UiAction::Clicked { id } if id == "hook_cancel" => {
                self.view.native_hooks.clear_draft();
                self.clear_settings_notice();
            }
            UiAction::Clicked { id } if id == "hook_save" => {
                if self.view.native_hooks.editor_open {
                    self.save_command_hook_draft();
                }
            }
            UiAction::Clicked { id } if id == "mcp_new" => {
                self.clear_mcp_draft();
                self.clear_settings_notice();
                self.view.mcp_editor_open = true;
            }
            UiAction::Clicked { id } if id == "mcp_transport:stdio" => {
                if self.view.mcp_editor_open {
                    self.view.mcp_draft_http = false;
                    self.view.mcp_draft_oauth_enabled = false;
                    self.clear_settings_notice();
                }
            }
            UiAction::Clicked { id } if id == "mcp_transport:http" => {
                if self.view.mcp_editor_open {
                    self.view.mcp_draft_http = true;
                    self.clear_settings_notice();
                }
            }
            UiAction::Clicked { id } if id == "mcp_auth:bearer" => {
                if self.view.mcp_editor_open {
                    self.view.mcp_draft_oauth_enabled = false;
                    self.clear_settings_notice();
                }
            }
            UiAction::Clicked { id } if id == "mcp_auth:oauth" => {
                if self.view.mcp_editor_open && self.view.mcp_draft_http {
                    self.view.mcp_draft_oauth_enabled = true;
                    self.clear_settings_notice();
                }
            }
            UiAction::Clicked { id } if id.starts_with("mcp_oauth_authorize:") => {
                self.start_mcp_oauth(id.trim_start_matches("mcp_oauth_authorize:"));
            }
            UiAction::Clicked { id } if id == "mcp_oauth_open" => {
                if let Some(McpOAuthViewStatus {
                    phase: McpOAuthViewPhase::AwaitingBrowser { url },
                    ..
                }) = &self.view.mcp_oauth_status
                {
                    ctx.open_url(egui::OpenUrl::new_tab(url.clone()));
                }
            }
            UiAction::Clicked { id } if id == "mcp_oauth_cancel" => {
                self.cancel_mcp_oauth();
            }
            UiAction::Clicked { id } if id.starts_with("mcp_select:") => {
                self.select_mcp_server(id.trim_start_matches("mcp_select:"));
            }
            UiAction::Clicked { id } if id == "mcp_cancel" => {
                self.view.mcp_editor_open = false;
                self.clear_mcp_draft();
                self.clear_settings_notice();
            }
            UiAction::Clicked { id } if id == "mcp_save" => {
                if self.view.mcp_editor_open {
                    self.save_mcp_draft();
                }
            }
            UiAction::Clicked { id } if id.starts_with("mcp_toggle:") => {
                self.toggle_mcp_server(id.trim_start_matches("mcp_toggle:"));
            }
            UiAction::Clicked { id } if id.starts_with("mcp_delete:") => {
                let target = id.trim_start_matches("mcp_delete:");
                if !self.view.mcp_editor_open
                    && self
                        .view
                        .mcp_servers
                        .iter()
                        .any(|server| server.id == target)
                {
                    self.view.pending_mcp_delete_id = Some(target.to_owned());
                    self.clear_settings_notice();
                }
            }
            UiAction::Clicked { id } if id == "mcp_delete_cancel" => {
                self.view.pending_mcp_delete_id = None;
            }
            UiAction::Clicked { id } if id.starts_with("mcp_delete_confirm:") => {
                let target = id.trim_start_matches("mcp_delete_confirm:");
                if self.view.pending_mcp_delete_id.as_deref() == Some(target)
                    && self.delete_mcp_server(target)
                {
                    self.view.pending_mcp_delete_id = None;
                }
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
                self.begin_model_discovery();
                self.begin_codex_model_discovery();
            }
            UiAction::Clicked { id } if id == "composer_routes_refresh" => {
                self.begin_model_discovery();
                self.begin_codex_model_discovery();
            }
            UiAction::Clicked { id } if id.starts_with("native_web_search:") => {
                if !self.view.running && self.view.native_provider_id == "openai" {
                    let mode = match id.as_str() {
                        "native_web_search:off" => Some(None),
                        "native_web_search:cached" => Some(Some(HostedWebSearchMode::Cached)),
                        "native_web_search:indexed" => Some(Some(HostedWebSearchMode::Indexed)),
                        "native_web_search:live" => Some(Some(HostedWebSearchMode::Live)),
                        _ => None,
                    };
                    if let Some(mode) = mode {
                        let previous = self.settings_preferences.hosted_web_search_mode;
                        self.settings_preferences.hosted_web_search_mode = mode;
                        match self.settings_preferences.save(&self.data_root) {
                            Ok(()) => {
                                self.view.hosted_web_search_mode = mode;
                                let message = if mode.is_some() {
                                    "Búsqueda web alojada activada para los próximos turnos"
                                } else {
                                    "Búsqueda web alojada desactivada"
                                };
                                self.view.status = message.to_owned();
                                self.set_settings_notice(message, false);
                            }
                            Err(error) => {
                                self.settings_preferences.hosted_web_search_mode = previous;
                                self.set_settings_notice(error, true);
                            }
                        }
                    }
                }
            }
            UiAction::Clicked { id } if id == "catalog_select_custom" => {
                if !self.view.running {
                    let previous = self.view.native_provider_id.clone();
                    self.view.native_provider_id = "custom".to_owned();
                    if !self.persist_native_settings() {
                        self.view.native_provider_id = previous;
                        self.view.native_key_available =
                            native_key_available_at(&self.native_settings(), &self.data_root);
                    } else {
                        let message = "Conexión personalizada activa: revisa el endpoint y el ID del modelo antes de ejecutar";
                        self.view.status = message.to_owned();
                        self.set_settings_notice(message, false);
                    }
                    self.refresh_composer_context_percent();
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
                        let usable = native_provider::adapter_at(&self.data_root, provider_id)
                            .ok()
                            .flatten()
                            .is_some()
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
                                self.view.native_key_available = native_key_available_at(
                                    &self.native_settings(),
                                    &self.data_root,
                                );
                            }
                            self.refresh_composer_context_percent();
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
            UiAction::Clicked { id } if id.starts_with("command_isolation:") => {
                let selected = match id.as_str() {
                    "command_isolation:workspace_write_offline" if cfg!(windows) => {
                        Some(CommandIsolation::WorkspaceWriteOffline)
                    }
                    "command_isolation:host_unrestricted" => {
                        Some(CommandIsolation::HostUnrestricted)
                    }
                    _ => None,
                };
                if let Some(selected) = selected {
                    let mut next = self.agent_tool_settings;
                    next.set_command_isolation(selected);
                    let message = match selected {
                        CommandIsolation::WorkspaceWriteOffline => {
                            "Comandos aislados al proyecto y sin red para próximos turnos"
                        }
                        CommandIsolation::HostUnrestricted => {
                            "Comandos con permisos de tu cuenta para próximos turnos; cada llamada pedirá aprobación"
                        }
                    };
                    self.commit_agent_tool_settings(next, message);
                } else {
                    self.set_settings_notice("Modo de aislamiento no disponible", true);
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
            UiAction::Clicked { id } if id == "turn_mode_toggle" => {
                if self.view.selected_backend == "native" && !self.view.running {
                    self.view.turn_mode = match self.view.turn_mode {
                        TurnMode::Plan => TurnMode::Build,
                        TurnMode::Build => TurnMode::Plan,
                    };
                }
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
                    self.remember_composer_selection(true);
                }
            }
            UiAction::Clicked { id } if id == "backend_menu_toggle" => {
                if !self.view.running {
                    self.view.backend_menu_open = !self.view.backend_menu_open;
                    self.view.composer_model_menu_open = false;
                    self.view.composer_model_list_open = false;
                    self.view.composer_model_query.clear();
                    self.composer_model_search_focus_pending = false;
                    self.view.composer_advanced_open = false;
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
                        self.remember_composer_selection(true);
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
                    self.view.composer_model_list_open = false;
                    self.view.composer_model_query.clear();
                    self.composer_model_search_focus_pending = false;
                    self.view.composer_advanced_open = false;
                    self.view.composer_effort_menu_open = false;
                    self.view.composer_effort_preview = None;
                    self.view.backend_menu_open = false;
                }
            }
            UiAction::Clicked { id } if id == "composer_model_dismiss" => {
                self.view.composer_model_menu_open = false;
                self.view.composer_model_list_open = false;
                self.view.composer_model_query.clear();
                self.composer_model_search_focus_pending = false;
                self.view.composer_advanced_open = false;
                self.view.composer_effort_menu_open = false;
                self.view.composer_effort_preview = None;
            }
            UiAction::Clicked { id } if id == "composer_model_list_toggle" => {
                if !self.view.running
                    && self.view.composer_model_menu_open
                    && !self.view.composer_advanced_open
                {
                    self.view.composer_model_list_open = !self.view.composer_model_list_open;
                    self.view.composer_model_query.clear();
                    self.composer_model_search_focus_pending = self.view.composer_model_list_open;
                    self.view.composer_effort_preview = None;
                }
            }
            UiAction::Clicked { id }
                if id == "composer_advanced_toggle" || id == "composer_advanced_chip_toggle" =>
            {
                if !self.view.running
                    && self.view.composer_model_menu_open
                    && !self.view.composer_model_list_open
                    && self
                        .view
                        .composer_effort_options
                        .iter()
                        .any(|(id, _)| id != "default")
                {
                    self.view.composer_advanced_open = true;
                    self.view.composer_effort_preview = None;
                }
            }
            UiAction::Clicked { id } if id == "composer_advanced_back" => {
                if self.view.composer_model_menu_open {
                    self.view.composer_advanced_open = false;
                    self.view.composer_effort_preview = None;
                }
            }
            UiAction::Clicked { id } if id.starts_with("composer_route:") => {
                self.select_composer_route(id.trim_start_matches("composer_route:"));
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
                    self.view.composer_model_list_open = false;
                    self.view.composer_model_query.clear();
                    self.composer_model_search_focus_pending = false;
                    self.view.composer_advanced_open = false;
                    self.view.composer_effort_menu_open = false;
                    self.view.composer_effort_preview = None;
                    self.view.backend_menu_open = false;
                }
            }
            UiAction::Clicked { id } if id == "composer_effort_dismiss" => {
                self.view.composer_effort_menu_open = false;
                self.view.composer_model_menu_open = false;
                self.view.composer_model_list_open = false;
                self.view.composer_model_query.clear();
                self.composer_model_search_focus_pending = false;
                self.view.composer_advanced_open = false;
                self.view.composer_effort_preview = None;
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
                "composer_model_search" => {
                    if self.view.composer_model_menu_open && self.view.composer_model_list_open {
                        self.view.composer_model_query = value.chars().take(128).collect();
                    }
                }
                "dock_overview_query" => self.view.dock_overview_query = value,
                "command_query" => {
                    self.view.command_query = value;
                    self.view.command_selected_index = 0;
                    self.request_command_search();
                }
                "prompt" => {
                    self.view.prompt = value;
                    self.draft_dirty_at = Some(Instant::now());
                }
                "composer_goal_draft" => self.view.composer_goal_draft = value,
                "terminal_input" => self.view.terminal_input = value,
                "tool_verification_output" => self.view.tool_verification_output = value,
                "settings_terminal_shell_path" => self.view.settings_terminal_shell_draft = value,
                "mcp_draft_id" => self.view.mcp_draft_id = value,
                "mcp_draft_url" => self.view.mcp_draft_url = value,
                "mcp_draft_http_bearer_token_env_name" => {
                    self.view.mcp_draft_http_bearer_token_env_name = value
                }
                "mcp_draft_oauth_client_id" => self.view.mcp_draft_oauth_client_id = value,
                "mcp_draft_oauth_issuer" => self.view.mcp_draft_oauth_issuer = value,
                "mcp_draft_oauth_scopes" => self.view.mcp_draft_oauth_scopes = value,
                "mcp_draft_program" => self.view.mcp_draft_program = value,
                "mcp_draft_args" => self.view.mcp_draft_args = value,
                "mcp_draft_cwd" => self.view.mcp_draft_cwd = value,
                "mcp_draft_env_names" => self.view.mcp_draft_env_names = value,
                "exec_rule_draft_pattern" => self.view.native_exec_rules.draft_pattern = value,
                "exec_rule_draft_justification" => {
                    self.view.native_exec_rules.draft_justification = value
                }
                "hook_draft_id" => self.view.native_hooks.draft_id = value,
                "hook_draft_tool_name" => self.view.native_hooks.draft_tool_name = value,
                "hook_draft_executable" => self.view.native_hooks.draft_executable = value,
                "hook_draft_args" => self.view.native_hooks.draft_args = value,
                "hook_draft_cwd" => self.view.native_hooks.draft_cwd = value,
                "hook_draft_timeout_ms" => self.view.native_hooks.draft_timeout_ms = value,
                "hook_draft_max_output_bytes" => {
                    self.view.native_hooks.draft_max_output_bytes = value
                }
                "workspace" => self.view.workspace_draft = value,
                "model" => {
                    self.view.model = value.clone();
                    if self.view.selected_backend == "native" {
                        self.view.composer_model = value;
                        self.view.composer_model_options =
                            composer_selection::model_options("native", &self.view.composer_model);
                        self.view.composer_effort_options =
                            composer_selection::effort_options_for_provider(
                                "native",
                                &self.view.composer_model,
                                &self.view.native_provider_id,
                            );
                        self.view.composer_effort = self
                            .view
                            .composer_effort_options
                            .first()
                            .map(|(effort, _)| effort.clone())
                            .unwrap_or_else(|| "default".to_owned());
                        self.remember_composer_selection(false);
                    }
                    self.persist_native_settings();
                    self.refresh_composer_context_percent();
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
                "automation_once_date" => {
                    if DatePicker::is_valid_date(&value) {
                        self.view.automations.calendar_year = value[..4]
                            .parse()
                            .unwrap_or(self.view.automations.calendar_year);
                        self.view.automations.calendar_month = value[5..7]
                            .parse()
                            .unwrap_or(self.view.automations.calendar_month);
                    }
                    self.view.automations.form.set_once_date(&value);
                }
                "automation_once_time" => self.view.automations.form.set_once_time(&value),
                "automation_interval" => self.view.automations.form.interval_minutes = value,
                "automation_time" => self.view.automations.form.time = value,
                "marketplace_search" => self.view.marketplace.query = value,
                "marketplace_source_draft" => self.view.marketplace.source_draft = value,
                _ => {}
            },
            UiAction::Submitted { id, .. } if id == "prompt" => self.start_run(),
            UiAction::Submitted { id, value } if id == "composer_goal_draft" => {
                self.view.composer_goal_draft = value;
                if self.view.composer_goal_editor_open {
                    self.set_composer_goal(Some(self.view.composer_goal_draft.clone()));
                }
            }
            UiAction::Submitted { id, value } if id == "marketplace_source_draft" => {
                self.view.marketplace.source_draft = value;
                self.handle_marketplace_action("marketplace_source_submit");
            }
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
        self.stop_dictation();
        self.cancel_mcp_oauth();
        // The CLI worker owns a Child behind an Arc. Dropping the UI receiver
        // alone does not terminate it, so stop the run before leaving the app.
        if let Some(run) = self.active_run.take() {
            run.cancel();
        }
        if let Some(compact) = self.active_compact.take() {
            compact.cancel.cancel();
        }
        if let Some(resume) = self.active_resume.take() {
            resume.cancel();
        }
        if let Some(recovery) = self.active_child_recovery.take() {
            recovery.cancel();
        }
        self.native_command_sessions.revoke_all();
        self.clear_native_command_ui();
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
        let ctx = ui.ctx().clone();
        let aux_changed = self.poll_dictation(&ctx)
            | self.poll_mcp_oauth()
            | self.poll_subagent_events()
            | self.poll_subagent_sessions()
            | self.poll_automations()
            | self.poll_compact()
            | self.poll_resume()
            | self.poll_child_recovery()
            | self.poll_native_command_sessions()
            | self.poll_marketplace()
            | self.poll_catalog()
            | self.poll_model_discovery()
            | self.poll_codex_model_discovery()
            | self.poll_workspace_folder_picker()
            | self.poll_composer_folder_picker();
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
        let workflow_modal_open = self.view.automations.screen == AutomationScreen::Edit
            || self.view.marketplace.add_source_kind.is_some()
            || self.view.pending_mcp_delete_id.is_some()
            || self.view.pending_native_command_termination.is_some()
            || (self.view.show_settings
                && self.view.settings_page == SettingsPage::Mcp
                && self.view.mcp_editor_open);
        if !terminal_focused && !workflow_modal_open && command_shortcut {
            if self.view.show_command_center {
                self.view.show_command_center = false;
            } else {
                self.open_command_center();
            }
        } else if !terminal_focused
            && !workflow_modal_open
            && escape
            && self.view.show_command_center
        {
            self.view.show_command_center = false;
        } else if !terminal_focused && !workflow_modal_open && !self.view.show_command_center {
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
                self.view.composer_model_list_open = false;
                self.view.composer_model_query.clear();
                self.composer_model_search_focus_pending = false;
                self.view.composer_advanced_open = false;
                self.view.composer_effort_menu_open = false;
                self.view.composer_effort_preview = None;
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
        let focus_was_pending = (
            self.prompt_focus_pending,
            self.composer_model_search_focus_pending,
            self.group_focus_pending,
            self.rename_focus_pending,
        );
        self.focus_prompt_if_pending();
        self.focus_composer_model_search_if_pending();
        self.focus_group_if_pending();
        self.focus_rename_if_pending();
        let mut needs_relayout = aux_changed
            || focus_was_pending
                != (
                    self.prompt_focus_pending,
                    self.composer_model_search_focus_pending,
                    self.group_focus_pending,
                    self.rename_focus_pending,
                );
        let modal = command_center_rect(area);
        let modal_width = modal.width();
        let modal_height = modal.height();
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
                if !workflow_modal_open && self.handle_dock_drag(&event, area.width()) {
                    continue;
                }
                if !workflow_modal_open && self.handle_terminal_event(&event) {
                    continue;
                }
                for action in self.tree.handle_event(event) {
                    self.apply_action(action, &ctx);
                }
            }
        }
        let effort_hovered = !self.view.show_command_center
            && self.view.composer_model_menu_open
            && self.view.composer_advanced_open
            && self.tree.hovered_id() == Some("composer_effort_slider");
        if self.view.composer_effort_hovered != effort_hovered {
            self.view.composer_effort_hovered = effort_hovered;
            needs_relayout = true;
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
            self.focus_rename_if_pending();
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
        if self.active_run.is_some()
            || self.active_compact.is_some()
            || self.active_resume.is_some()
            || self.active_child_recovery.is_some()
        {
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
            || self.active_mcp_oauth.is_some()
            || !self.view.active_subagents.is_empty()
            || self.subagent_poll_needed
            || self.marketplace_response.is_some()
            || self.catalog_response.is_some()
            || self.model_discovery_response.is_some()
            || self.codex_model_response.is_some()
            || self.workspace_folder_response.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if self.native_command_snapshot_response.is_some()
            || self.native_command_termination_response.is_some()
            || (self.view.show_side_pane
                && self.view.side_tab == SideTab::Activity
                && self
                    .view
                    .selected_session
                    .as_ref()
                    .is_some_and(|id| self.native_command_sessions.manager(id).is_some()))
        {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        ctx.request_repaint_after(Duration::from_secs(30));
        let animation_interval = [
            dock_animating.then_some(Duration::from_millis(16)),
            self.tree.animation_repaint_after(),
            self.command_tree.animation_repaint_after(),
        ]
        .into_iter()
        .flatten()
        .min();
        if let Some(interval) = animation_interval {
            ctx.request_repaint_after(interval);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::{MessageRole, ModelTokenUsage};
    use openide_harness::{LocalTool, PermissionMode};
    use openide_ui::{DrawCommand, Key};
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn native_capabilities_follow_route_and_exact_provider_metadata() {
        let mut adapter = native_provider::ProviderRegistry::builtin()
            .adapter("custom")
            .unwrap()
            .clone();
        let model_id = "gpt-6-astra";
        let unknown = native_request_capabilities(&adapter, model_id, None, &[]);
        assert!(unknown.tool_calls, "an explicit custom route may try tools");
        assert!(unknown.reasoning_efforts.is_empty());

        let discovered = native_provider::ProviderModelDiscovery {
            provider_id: adapter.id.clone(),
            models: [(
                model_id.to_owned(),
                native_provider::DiscoveredModel {
                    id: model_id.to_owned(),
                    name: model_id.to_owned(),
                    provider_reports_tools: true,
                    tool_call_declared: Some(true),
                    text_input_output: true,
                    supported_efforts: vec!["turbo".to_owned()],
                },
            )]
            .into(),
        };
        let no_wire_effort = native_request_capabilities(
            &adapter,
            model_id,
            None,
            std::slice::from_ref(&discovered),
        );
        assert!(no_wire_effort.reasoning_efforts.is_empty());
        adapter.reasoning_effort_field = Some("reasoning_effort".to_owned());
        let enabled = native_request_capabilities(
            &adapter,
            model_id,
            None,
            std::slice::from_ref(&discovered),
        );
        assert_eq!(enabled.reasoning_efforts, ["turbo"]);

        adapter.catalog.tool_call_path = Some("tools".to_owned());
        let mut denied = discovered;
        denied.models.get_mut(model_id).unwrap().tool_call_declared = Some(false);
        assert!(!native_request_capabilities(&adapter, model_id, None, &[denied]).tool_calls);
    }

    #[test]
    fn native_checkpoint_format_depends_on_protocol_not_provider_name() {
        let chat = native_checkpoint_compatibility(
            "custom-provider",
            "model",
            NativeWireApi::ChatCompletions,
        );
        let responses =
            native_checkpoint_compatibility("custom-provider", "model", NativeWireApi::Responses);
        assert_ne!(chat.context_format_key, responses.context_format_key);
    }

    fn committed_usage_events(usage: Option<ModelTokenUsage>) -> Vec<AgentEvent> {
        vec![
            AgentEvent::new(
                "session",
                1,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-4o".into(),
                },
            ),
            AgentEvent::new(
                "session",
                2,
                AgentEventKind::ModelResponseCommitted {
                    step: 1,
                    assistant: AgentMessage::assistant("Respuesta", vec![]),
                    response_id: None,
                    usage,
                },
            ),
        ]
    }

    #[test]
    fn composer_context_percent_uses_committed_total_and_verified_window() {
        let events = committed_usage_events(Some(ModelTokenUsage {
            input_tokens: 60_000,
            output_tokens: 4_000,
            total_tokens: 64_000,
            reasoning_tokens: None,
            cached_input_tokens: None,
        }));
        assert_eq!(
            committed_context_percent(&events, "openai", "gpt-4o"),
            Some(55)
        );
        assert_eq!(context_remaining_percent(65_000, 128_000), Some(54));
        assert_eq!(context_remaining_percent(256_000, 128_000), Some(0));
    }

    #[test]
    fn composer_context_percent_hides_unknown_usage_or_budget() {
        let events = committed_usage_events(Some(ModelTokenUsage {
            input_tokens: 60_000,
            output_tokens: 4_000,
            total_tokens: 64_000,
            reasoning_tokens: None,
            cached_input_tokens: None,
        }));
        assert_eq!(committed_context_percent(&events, "custom", "gpt-4o"), None);
        assert_eq!(committed_context_percent(&events, "openai", "gpt-5"), None);
        assert_eq!(
            committed_context_percent(&committed_usage_events(None), "openai", "gpt-4o"),
            None
        );
        assert_eq!(context_remaining_percent(1, 0), None);
    }

    #[test]
    fn command_center_is_compact_and_centered_at_desktop_and_narrow_sizes() {
        for (area, expected_width, expected_height) in [
            (
                egui::Rect::from_min_size(egui::pos2(100.0, 40.0), egui::vec2(1440.0, 900.0)),
                560.0,
                500.0,
            ),
            (
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(480.0, 320.0)),
                448.0,
                272.0,
            ),
        ] {
            let modal = command_center_rect(area);
            assert_eq!(modal.size(), egui::vec2(expected_width, expected_height));
            assert_eq!(modal.center(), area.center());
            assert!(area.contains_rect(modal));
        }
    }

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
    fn composer_goal_is_saved_per_draft_and_follows_the_created_session() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let workspace = app.view.workspace.clone();
        let ctx = egui::Context::default();
        app.apply_action(clicked("composer_goal_open"), &ctx);
        assert!(app.view.composer_add_menu_open);
        assert!(app.view.composer_goal_editor_open);
        app.apply_action(
            UiAction::InputChanged {
                id: "composer_goal_draft".into(),
                value: "  Reparar el editor  ".into(),
            },
            &ctx,
        );
        app.apply_action(clicked("composer_goal_save"), &ctx);
        assert_eq!(app.view.composer_goal.as_deref(), Some("Reparar el editor"));
        assert!(!app.view.composer_goal_editor_open);

        let draft_key = (workspace.clone(), None);
        app.transfer_draft_goal_to_session(&draft_key, "session-1")
            .unwrap();
        app.view.selected_session = Some("session-1".into());
        app.restore_draft();
        assert_eq!(app.view.composer_goal.as_deref(), Some("Reparar el editor"));

        app.view.selected_session = None;
        app.restore_draft();
        assert_eq!(app.view.composer_goal, None);
        app.view.selected_session = Some("session-1".into());
        app.restore_draft();
        assert_eq!(app.view.composer_goal.as_deref(), Some("Reparar el editor"));

        let saved = load_composer_goals(root.path()).unwrap();
        assert_eq!(
            saved
                .get(&(workspace, Some("session-1".into())))
                .map(String::as_str),
            Some("Reparar el editor")
        );
    }

    #[test]
    fn goal_context_is_separate_from_visible_prompt_and_plugin_opens_real_catalog() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        app.view.prompt = "Corregir el error".into();
        app.set_composer_goal(Some("Restaurar la compilación".into()));
        let context = system_instructions_with_goal(
            "Skill: pruebas".into(),
            app.view.composer_goal.as_deref(),
        );
        assert!(context.contains("Restaurar la compilación"));
        assert!(context.contains("Skill: pruebas"));
        assert_eq!(app.view.prompt, "Corregir el error");

        app.apply_action(
            clicked("composer_plugin_spreadsheets"),
            &egui::Context::default(),
        );
        assert_eq!(app.view.main_page, MainPage::Marketplace);
        assert_eq!(app.view.marketplace.query, "spreadsheet");
    }

    #[test]
    fn native_workspace_instructions_include_project_file_before_goal() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("AGENTS.md"), "Seguir reglas locales").unwrap();
        let instructions = native_workspace_instructions(root.path(), &workspace).unwrap();
        let full = system_instructions_with_goal(instructions, Some("Terminar el harness"));
        assert!(full.contains("Seguir reglas locales"));
        assert!(full.find("Seguir reglas locales") < full.find("Terminar el harness"));
    }

    #[test]
    fn resume_uses_saved_instructions_when_project_document_changes() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("AGENTS.md"), "Primera versión").unwrap();
        let saved =
            native_resume_instructions(root.path(), &workspace, Some("Terminar el harness"), None)
                .unwrap();
        std::fs::write(workspace.join("AGENTS.md"), [0xff]).unwrap();
        assert_eq!(
            native_resume_instructions(root.path(), &workspace, None, Some(&saved)).unwrap(),
            saved
        );
        assert!(native_resume_instructions(root.path(), &workspace, None, None).is_err());
    }

    #[test]
    fn local_skill_selection_reaches_native_context_and_changed_content_requires_reselection() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let workspace = root.path().join("project");
        let skill = workspace.join(".agents/skills/review/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::write(
            &skill,
            "---\nname: review\ndescription: Review skill\n---\nReview version one",
        )
        .unwrap();
        let mut app = AgentsWindow::with_data_root(data.clone());
        app.change_workspace(workspace.to_string_lossy().into_owned());
        assert_eq!(
            app.view.native_skills.available.len(),
            1,
            "{:?}",
            app.view.native_skills
        );
        assert!(!native_workspace_instructions(&data, &workspace)
            .unwrap()
            .contains("Review version one"));

        let ctx = egui::Context::default();
        app.apply_action(clicked("native_skill_toggle:0"), &ctx);
        assert!(
            !app.view.settings_notice_error,
            "{:?}",
            app.view.settings_notice
        );
        let original = native_workspace_instructions(&data, &workspace).unwrap();
        assert!(original.contains("Review version one"));
        assert!(original.contains("Skills locales seleccionadas (identidad "));
        let mut reopened = AgentsWindow::with_data_root(data.clone());
        reopened.change_workspace(workspace.to_string_lossy().into_owned());
        assert_eq!(reopened.view.native_skills.selected.len(), 1);

        std::fs::write(
            &skill,
            "---\nname: review\ndescription: Review skill\n---\nReview version two",
        )
        .unwrap();
        assert!(native_workspace_instructions(&data, &workspace).is_err());
        app.apply_action(clicked("native_skill_refresh"), &ctx);
        assert!(app.view.native_skills.activation_error.is_some());
        app.apply_action(clicked("native_skill_toggle:0"), &ctx);
        assert!(
            !app.view.settings_notice_error,
            "{:?}",
            app.view.settings_notice
        );
        let updated = native_workspace_instructions(&data, &workspace).unwrap();
        assert!(updated.contains("Review version two"));
        assert_ne!(updated, original);
    }

    #[test]
    fn corrupt_local_skill_selection_blocks_native_context_until_confirmed_reset() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let workspace = root.path().join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("native-local-skills.json"), "{broken").unwrap();
        let mut app = AgentsWindow::with_data_root(data.clone());
        app.change_workspace(workspace.to_string_lossy().into_owned());
        assert!(app.view.native_skills.load_error.is_some());
        assert!(native_workspace_instructions(&data, &workspace).is_err());
        let ctx = egui::Context::default();
        app.apply_action(clicked("native_skill_reset"), &ctx);
        assert!(app.view.native_skills.reset_confirm_open);
        app.apply_action(clicked("native_skill_reset_confirm"), &ctx);
        assert!(app.view.native_skills.load_error.is_none());
        assert!(native_workspace_instructions(&data, &workspace).is_ok());
    }

    #[test]
    fn marketplace_removal_waits_for_alert_confirmation() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        app.view
            .marketplace
            .snapshot
            .sources
            .push(crate::marketplace::MarketplaceSource {
                id: "source".into(),
                name: "Origen".into(),
                location: "C:/source".into(),
                kind: SourceKind::Local,
            });
        app.apply_action(clicked("marketplace_remove_source:source"), &ctx);
        assert_eq!(
            app.view.marketplace.pending_remove_source_id.as_deref(),
            Some("source")
        );
        assert!(!app.view.marketplace.busy);
        app.apply_action(clicked("marketplace_alert_cancel"), &ctx);
        assert!(app.view.marketplace.pending_remove_source_id.is_none());
        app.apply_action(clicked("marketplace_remove_source_confirm:source"), &ctx);
        assert!(!app.view.marketplace.busy);
    }

    #[test]
    fn marketplace_source_modal_clears_stale_status_and_keeps_busy_draft() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        app.view.marketplace.status = Some("Error anterior".into());
        app.apply_action(clicked("marketplace_source_kind:local"), &ctx);
        assert_eq!(
            app.view.marketplace.add_source_kind,
            Some(SourceKind::Local)
        );
        assert!(app.view.marketplace.status.is_none());
        app.apply_action(
            UiAction::Submitted {
                id: "marketplace_source_draft".into(),
                value: String::new(),
            },
            &ctx,
        );
        assert_eq!(
            app.view.marketplace.status.as_deref(),
            Some("Indica la carpeta o URL del origen")
        );
        assert!(!app.view.marketplace.busy);

        app.view.marketplace.source_draft = "C:/skills".into();
        app.view.marketplace.busy = true;
        app.apply_action(clicked("marketplace_source_cancel"), &ctx);
        assert_eq!(
            app.view.marketplace.add_source_kind,
            Some(SourceKind::Local)
        );
        assert_eq!(app.view.marketplace.source_draft, "C:/skills");

        app.view.marketplace.busy = false;
        app.apply_action(clicked("marketplace_source_cancel"), &ctx);
        assert!(app.view.marketplace.add_source_kind.is_none());
        assert!(app.view.marketplace.source_draft.is_empty());
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
        assert!(!file.is_dir);
        let folder = composer_attachment_for_path(&project, Path::new("src")).unwrap();
        assert_eq!(folder.path, "src");
        assert!(folder.is_dir);
        assert!(composer_attachment_for_path(&project, Path::new("../private.txt")).is_err());
    }

    #[test]
    fn native_image_attachment_is_inline_and_keeps_other_paths_as_context() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("screen.png"), b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let image_attachment =
            composer_attachment_for_path(&project, Path::new("screen.png")).unwrap();
        let image = native_image_from_attachment(&project, &image_attachment).unwrap();
        assert_eq!(image.mime_type, UserImageMime::Png);
        assert_eq!(image.verified_bytes().unwrap(), b"\x89PNG\r\n\x1a\nfixture");
        let prepared = prepare_native_attachments(
            &project,
            std::slice::from_ref(&image_attachment),
            "openai",
            "gpt-4.1",
        )
        .unwrap();
        assert_eq!(prepared.images, vec![image]);
        assert!(prepare_native_attachments(
            &project,
            std::slice::from_ref(&image_attachment),
            "deepseek",
            "deepseek-flash",
        )
        .is_err());
        let other = ComposerAttachmentView {
            path: "src/main.rs".into(),
            label: "main.rs".into(),
            is_dir: false,
        };
        let prompt = native_prompt_with_images("Revisa ambos", &[other], &[image_attachment]);
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("Imágenes enviadas al modelo"));
        assert!(!prompt.contains("\"path\":\"screen.png\""));
    }

    #[test]
    fn session_approval_revoke_action_updates_durable_grants() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        let store = app.store.as_ref().unwrap().clone();
        let session = store.create(root.path(), "native").unwrap();
        let grant = openide_core::SessionApprovalGrantRecord {
            version: 1,
            session_id: session.id.clone(),
            tool_name: "run_command".into(),
            effective_policy_digest: "a".repeat(64),
            key_digest: "b".repeat(64),
        };
        store
            .record_permission_decision_with_grant(
                &session.id,
                "request-1",
                PermissionDecision::Allow,
                Some(grant.clone()),
            )
            .unwrap();
        app.view.selected_session = Some(session.id.clone());
        app.refresh_selected_session_approvals();
        assert_eq!(app.view.session_approval_grants.len(), 1);
        app.apply_action(
            clicked(format!("session_approval_revoke:{}", grant.key_digest)),
            &egui::Context::default(),
        );
        assert!(store
            .session_approval_grants(&session.id)
            .unwrap()
            .is_empty());
        assert!(app.view.session_approval_grants.is_empty());
        assert!(app.view.status.contains("revocado"));
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
            is_dir: false,
        }];
        let prompt = composer_prompt_with_attachments("Mejora la UI", &attachments);
        assert!(prompt.starts_with("Mejora la UI\n\n"));
        assert!(prompt.contains("\"path\":\"src/main.rs\""));
        assert!(prompt.contains("\"type\":\"file\""));
        assert!(prompt.contains("explora las carpetas"));
        assert_eq!(attachments[0].label, "main.rs");
    }

    #[test]
    fn composer_attachment_draft_survives_context_switch_and_failed_run() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        let file = ComposerAttachmentView {
            path: "src/main.rs".into(),
            label: "main.rs".into(),
            is_dir: false,
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

    #[cfg(windows)]
    #[tokio::test]
    async fn activity_termination_requires_confirmation_and_preserves_model_output() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        let session = app
            .store
            .as_ref()
            .unwrap()
            .create(&workspace, "native")
            .unwrap();
        app.view.selected_session = Some(session.id.clone());
        app.view.selected_backend = "native".into();
        app.view.main_page = MainPage::Workspace;
        app.view.show_settings = false;
        app.view.show_side_pane = true;
        app.view.side_tab = SideTab::Activity;
        let manager = app
            .native_command_sessions
            .for_session(
                &session.id,
                &workspace,
                CommandIsolation::HostUnrestricted,
                true,
            )
            .unwrap()
            .unwrap();
        let command = std::env::var_os("COMSPEC")
            .unwrap_or_else(|| "cmd.exe".into())
            .to_string_lossy()
            .into_owned();
        let started = manager
            .start(
                &command,
                &[
                    "/C".into(),
                    "echo READY & ping -n 30 127.0.0.1 > NUL".into(),
                ],
                &CancellationToken::new(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.poll_native_command_sessions();
            if app
                .view
                .native_commands
                .iter()
                .any(|command| command.id == started.id && command.has_output)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "command did not appear in Activity"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let ctx = egui::Context::default();
        let other = app
            .store
            .as_ref()
            .unwrap()
            .create(&workspace, "native")
            .unwrap();
        app.view.selected_session = Some(other.id);
        app.poll_native_command_sessions();
        assert!(app.view.native_commands.is_empty());
        app.apply_action(
            clicked(format!("native_command_terminate:{}", started.id)),
            &ctx,
        );
        assert!(app.view.pending_native_command_termination.is_none());
        app.view.selected_session = Some(session.id.clone());
        app.last_native_command_snapshot = None;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.poll_native_command_sessions();
            if app
                .view
                .native_commands
                .iter()
                .any(|command| command.id == started.id)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "command did not return to its task"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        app.apply_action(
            clicked(format!("native_command_terminate:{}", started.id)),
            &ctx,
        );
        assert_eq!(
            app.view.pending_native_command_termination.as_deref(),
            Some(started.id.as_str())
        );
        assert!(manager
            .list_sessions()
            .unwrap()
            .iter()
            .any(|command| command.id == started.id && command.exit.is_none()));
        app.apply_action(clicked("native_command_terminate_cancel"), &ctx);
        assert!(app.view.pending_native_command_termination.is_none());
        assert!(manager
            .list_sessions()
            .unwrap()
            .iter()
            .any(|command| command.id == started.id && command.exit.is_none()));

        app.apply_action(
            clicked(format!("native_command_terminate:{}", started.id)),
            &ctx,
        );
        app.apply_action(
            clicked(format!("native_command_terminate_confirm:{}", started.id)),
            &ctx,
        );
        assert!(app.native_command_termination_response.is_some());
        assert_eq!(
            app.view.native_command_terminating.as_deref(),
            Some(started.id.as_str())
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.poll_native_command_sessions();
            if app.native_command_termination_response.is_none() {
                break;
            }
            assert!(Instant::now() < deadline, "termination did not finish");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(app.view.status.contains("terminado"), "{}", app.view.status);
        assert!(manager.list_sessions().unwrap().iter().any(|command| {
            command.id == started.id
                && command.exit == Some(openide_harness::PersistentCommandExit::Terminated)
        }));
        let final_output = manager
            .poll(&started.id, Duration::ZERO, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            final_output.exit,
            Some(openide_harness::PersistentCommandExit::Terminated)
        );
        assert!(final_output.events.iter().any(|event| matches!(
            event,
            openide_harness::PersistentCommandEvent::Output { bytes, .. }
                if String::from_utf8_lossy(bytes).contains("READY")
        )));
    }

    #[test]
    fn command_hook_settings_validate_persist_and_change_runtime_digest() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("trusted-hook.bin");
        std::fs::write(&executable, b"first version").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        app.apply_action(clicked("settings_page:hooks"), &ctx);
        app.apply_action(clicked("hook_new"), &ctx);
        for (id, value) in [
            ("hook_draft_id", "review-write".to_owned()),
            ("hook_draft_tool_name", "write_file".to_owned()),
            (
                "hook_draft_executable",
                executable.to_string_lossy().into_owned(),
            ),
            ("hook_draft_args", "[\"--check\"]".to_owned()),
            ("hook_draft_cwd", root.path().to_string_lossy().into_owned()),
        ] {
            app.apply_action(
                UiAction::InputChanged {
                    id: id.into(),
                    value,
                },
                &ctx,
            );
        }
        app.apply_action(clicked("hook_save"), &ctx);
        assert!(
            !app.view.settings_notice_error,
            "{:?}",
            app.view.settings_notice
        );
        assert_eq!(app.view.native_hooks.hooks.len(), 1);
        let saved = NativeHookSettings::load(root.path()).unwrap();
        assert_eq!(saved.hooks[0].id, "review-write");
        let old_digest = saved.registry().unwrap().digest_hex().unwrap();

        let reopened = AgentsWindow::with_data_root(root.path().to_path_buf());
        assert_eq!(reopened.view.native_hooks.hooks.len(), 1);
        std::fs::write(&executable, b"changed version").unwrap();
        let changed_digest = NativeHookSettings::load(root.path())
            .unwrap()
            .registry()
            .unwrap()
            .digest_hex()
            .unwrap();
        assert_ne!(old_digest, changed_digest);

        app.apply_action(clicked("hook_select:review-write"), &ctx);
        app.apply_action(
            UiAction::InputChanged {
                id: "hook_draft_executable".into(),
                value: "relative-hook".into(),
            },
            &ctx,
        );
        app.apply_action(clicked("hook_save"), &ctx);
        assert!(app.view.settings_notice_error);
        assert_eq!(
            NativeHookSettings::load(root.path()).unwrap().hooks.len(),
            1
        );
        app.apply_action(clicked("hook_cancel"), &ctx);
        app.apply_action(clicked("hook_delete:review-write"), &ctx);
        assert!(NativeHookSettings::load(root.path())
            .unwrap()
            .hooks
            .is_empty());
    }

    #[test]
    fn command_rules_ui_persists_effective_policy_and_corruption_blocks_native_runs() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let baseline = app.native_permission_policy().unwrap().digest_hex();
        let ctx = egui::Context::default();
        app.apply_action(clicked("exec_rule_new"), &ctx);
        app.apply_action(
            UiAction::InputChanged {
                id: "exec_rule_draft_pattern".into(),
                value: "[\"git\",\"push\"]".into(),
            },
            &ctx,
        );
        app.apply_action(clicked("exec_rule_decision:deny"), &ctx);
        app.apply_action(clicked("exec_rule_save"), &ctx);
        assert!(
            !app.view.settings_notice_error,
            "{:?}",
            app.view.settings_notice
        );
        let saved = app.native_permission_policy().unwrap();
        assert_ne!(saved.digest_hex(), baseline);
        let call = ToolCall {
            id: "push".into(),
            name: "run_command".into(),
            arguments: json!({"command":"git","args":["push","origin","main"]}),
        };
        assert_eq!(
            saved.exec_policy().evaluate_call(&call).unwrap().decision,
            Some(ExecDecision::Deny)
        );

        let reopened = AgentsWindow::with_data_root(root.path().to_path_buf());
        assert_eq!(
            reopened.native_permission_policy().unwrap().digest_hex(),
            saved.digest_hex()
        );
        std::fs::write(root.path().join("native-exec-rules.json"), "{broken").unwrap();
        assert!(app.native_permission_policy().is_err());
        let mut corrupt = AgentsWindow::with_data_root(root.path().to_path_buf());
        assert!(corrupt.native_permission_policy().is_err());
        assert!(corrupt.view.native_exec_rules.load_error.is_some());
        corrupt.apply_action(clicked("exec_rule_reset"), &ctx);
        corrupt.apply_action(clicked("exec_rule_reset_confirm"), &ctx);
        assert_eq!(
            corrupt.native_permission_policy().unwrap().digest_hex(),
            baseline
        );
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
    fn command_isolation_choice_persists_as_a_host_setting() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        app.apply_action(clicked("command_isolation:host_unrestricted"), &ctx);
        assert_eq!(
            app.view.native_command_isolation,
            CommandIsolation::HostUnrestricted
        );
        assert_eq!(
            AgentToolSettings::load(root.path())
                .unwrap()
                .command_isolation(),
            CommandIsolation::HostUnrestricted
        );
        if cfg!(windows) {
            app.apply_action(clicked("command_isolation:workspace_write_offline"), &ctx);
            assert_eq!(
                app.view.native_command_isolation,
                CommandIsolation::WorkspaceWriteOffline
            );
        }
        let previous = app.view.native_command_isolation;
        app.apply_action(clicked("command_isolation:unknown"), &ctx);
        assert_eq!(app.view.native_command_isolation, previous);
        assert!(app.view.settings_notice_error);
    }

    #[test]
    fn hosted_web_search_choice_persists_for_openai_responses() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        app.view.native_provider_id = "openai".into();
        let ctx = egui::Context::default();
        app.apply_action(clicked("native_web_search:indexed"), &ctx);
        assert_eq!(
            app.view.hosted_web_search_mode,
            Some(HostedWebSearchMode::Indexed)
        );
        assert_eq!(
            SettingsPreferences::load(root.path()).hosted_web_search_mode,
            Some(HostedWebSearchMode::Indexed)
        );
        app.apply_action(clicked("native_web_search:off"), &ctx);
        assert_eq!(app.view.hosted_web_search_mode, None);
    }

    #[test]
    fn mcp_settings_actions_round_trip_precise_arguments_and_enabled_state() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.change_workspace(workspace.to_string_lossy().into_owned());
        let ctx = egui::Context::default();
        app.apply_action(clicked("mcp_new"), &ctx);
        assert!(app.view.mcp_editor_open);
        for (id, value) in [
            ("mcp_draft_id", "example"),
            ("mcp_draft_program", "mock-mcp"),
            ("mcp_draft_args", "[\"\",\"--verbose\"]"),
            ("mcp_draft_env_names", "MCP_TOKEN"),
        ] {
            app.apply_action(
                UiAction::InputChanged {
                    id: id.into(),
                    value: value.into(),
                },
                &ctx,
            );
        }
        app.apply_action(clicked("mcp_save"), &ctx);
        assert!(!app.view.mcp_editor_open);
        assert_eq!(app.view.mcp_servers.len(), 1);
        assert_eq!(app.view.mcp_servers[0].args, ["", "--verbose"]);
        assert!(!app.view.mcp_servers[0].enabled);
        assert_eq!(
            app.view.settings_notice.as_deref(),
            Some("Servidor MCP guardado")
        );
        app.apply_action(clicked("mcp_toggle:example"), &ctx);
        assert!(app.view.mcp_servers[0].enabled);
        let persisted = McpSettings::load(&app.data_root, &workspace).unwrap();
        assert_eq!(persisted.servers, app.view.mcp_servers);
        app.apply_action(clicked("mcp_select:example"), &ctx);
        assert!(app.view.mcp_editor_open);
        assert_eq!(app.view.mcp_draft_args, "[\n  \"\",\n  \"--verbose\"\n]");
        app.apply_action(clicked("mcp_cancel"), &ctx);
        assert!(!app.view.mcp_editor_open);
        assert!(app.view.mcp_selected_id.is_none());
        app.apply_action(clicked("mcp_delete_confirm:example"), &ctx);
        assert_eq!(app.view.mcp_servers.len(), 1);
        app.apply_action(clicked("mcp_delete:example"), &ctx);
        assert_eq!(app.view.pending_mcp_delete_id.as_deref(), Some("example"));
        assert_eq!(app.view.mcp_servers.len(), 1);
        app.apply_action(clicked("mcp_delete_cancel"), &ctx);
        assert!(app.view.pending_mcp_delete_id.is_none());
        assert_eq!(app.view.mcp_servers.len(), 1);
        app.apply_action(clicked("mcp_delete:example"), &ctx);
        app.apply_action(clicked("mcp_delete_confirm:example"), &ctx);
        assert!(app.view.pending_mcp_delete_id.is_none());
        assert!(McpSettings::load(&app.data_root, &workspace)
            .unwrap()
            .servers
            .is_empty());
    }

    #[test]
    fn mcp_http_settings_store_only_token_env_name() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.change_workspace(workspace.to_string_lossy().into_owned());
        let ctx = egui::Context::default();
        app.apply_action(clicked("mcp_new"), &ctx);
        app.apply_action(clicked("mcp_transport:http"), &ctx);
        assert!(app.view.mcp_draft_http);
        for (id, value) in [
            ("mcp_draft_id", "remote"),
            ("mcp_draft_url", "https://example.com/mcp"),
            ("mcp_draft_http_bearer_token_env_name", "MCP_REMOTE_TOKEN"),
        ] {
            app.apply_action(
                UiAction::InputChanged {
                    id: id.into(),
                    value: value.into(),
                },
                &ctx,
            );
        }
        app.apply_action(clicked("mcp_save"), &ctx);
        assert!(!app.view.mcp_editor_open, "{:?}", app.view.settings_notice);
        let settings = McpSettings::load(&app.data_root, &workspace).unwrap();
        assert_eq!(settings.servers.len(), 1);
        let server = &settings.servers[0];
        assert_eq!(server.url.as_deref(), Some("https://example.com/mcp"));
        assert_eq!(
            server.http_bearer_token_env_name.as_deref(),
            Some("MCP_REMOTE_TOKEN")
        );
        assert!(server.program.is_empty());
        assert!(server.args.is_empty());
        assert!(server.credential_env_names.is_empty());
        app.apply_action(clicked("mcp_select:remote"), &ctx);
        assert!(app.view.mcp_draft_http);
        assert_eq!(app.view.mcp_draft_url, "https://example.com/mcp");
        app.apply_action(clicked("mcp_transport:stdio"), &ctx);
        assert!(!app.view.mcp_draft_http);
    }

    #[test]
    fn mcp_oauth_settings_round_trip_public_registration_without_bearer_env() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.change_workspace(workspace.to_string_lossy().into_owned());
        let ctx = egui::Context::default();
        app.apply_action(clicked("mcp_new"), &ctx);
        app.apply_action(clicked("mcp_transport:http"), &ctx);
        app.apply_action(clicked("mcp_auth:oauth"), &ctx);
        for (id, value) in [
            ("mcp_draft_id", "protected"),
            ("mcp_draft_url", "https://resource.example/mcp"),
            ("mcp_draft_oauth_client_id", "openide-public-client"),
            ("mcp_draft_oauth_issuer", "https://auth.example"),
            ("mcp_draft_oauth_scopes", "read\nwrite"),
            ("mcp_draft_http_bearer_token_env_name", "SHOULD_BE_IGNORED"),
        ] {
            app.apply_action(
                UiAction::InputChanged {
                    id: id.into(),
                    value: value.into(),
                },
                &ctx,
            );
        }
        app.apply_action(clicked("mcp_save"), &ctx);
        assert!(!app.view.mcp_editor_open, "{:?}", app.view.settings_notice);
        let server = McpSettings::load(&app.data_root, &workspace)
            .unwrap()
            .servers
            .remove(0);
        assert!(server.http_bearer_token_env_name.is_none());
        let oauth = server.http_oauth.unwrap();
        assert_eq!(oauth.client_id, "openide-public-client");
        assert_eq!(oauth.issuer, "https://auth.example");
        assert_eq!(oauth.scopes, ["read", "write"]);
        app.apply_action(clicked("mcp_select:protected"), &ctx);
        assert!(app.view.mcp_draft_oauth_enabled);
        assert_eq!(app.view.mcp_draft_oauth_scopes, "read\nwrite");
    }

    #[test]
    fn sidebar_projects_subagent_status_without_parent_lease() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        let store = app.store.clone().unwrap();
        let parent = store.create(&workspace, "native").unwrap();
        let parent_lease = store.try_acquire_native_lease(&parent.id).unwrap();
        let coordinator = SubagentCoordinator::new(store, SubagentLimits::default()).unwrap();
        let child = coordinator
            .create_child(
                &parent_lease,
                "sidebar-child",
                &openide_harness::SubagentSpec {
                    prompt: "Inspect".into(),
                    model: "fixture".into(),
                    mode: TurnMode::Build,
                    context_digest: "a".repeat(64),
                },
            )
            .unwrap();
        app.refresh_sessions();
        let row = app
            .view
            .sessions
            .iter()
            .find(|row| row.id == child.session.id)
            .unwrap();
        assert!(row.title.starts_with("↳ "));
        assert!(row.title.ends_with("pendiente"));
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        app.refresh_sessions();
        let row = app
            .view
            .sessions
            .iter()
            .find(|row| row.id == child.session.id)
            .unwrap();
        assert!(row.title.ends_with("en ejecución"));
        assert!(app.subagent_poll_needed);
        coordinator
            .finish_child(&run, openide_harness::SubagentOutcome::Completed)
            .unwrap();
        app.refresh_sessions();
        let row = app
            .view
            .sessions
            .iter()
            .find(|row| row.id == child.session.id)
            .unwrap();
        assert!(row.title.ends_with("completado"));
        assert!(!app.subagent_poll_needed);
    }

    #[test]
    fn archive_waits_for_nonterminal_descendant() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        let store = app.store.as_ref().unwrap().clone();
        let parent = store.create(&workspace, "native").unwrap();
        let parent_lease = store.try_acquire_native_lease(&parent.id).unwrap();
        let coordinator =
            SubagentCoordinator::new(store.clone(), SubagentLimits::default()).unwrap();
        let child = coordinator
            .create_child(
                &parent_lease,
                "archive-child",
                &openide_harness::SubagentSpec {
                    prompt: "Inspect".into(),
                    model: "fixture".into(),
                    mode: TurnMode::Build,
                    context_digest: "a".repeat(64),
                },
            )
            .unwrap();
        let ctx = egui::Context::default();
        app.apply_action(clicked(format!("session_archive:{}", parent.id)), &ctx);
        app.apply_action(clicked(format!("archive_confirm:{}", parent.id)), &ctx);
        assert!(!store.load(&parent.id).unwrap().unwrap().session.archived);
        assert!(app.view.status.contains("subagentes activos"));

        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        coordinator
            .finish_child(&run, openide_harness::SubagentOutcome::Completed)
            .unwrap();
        drop(run);
        app.apply_action(clicked(format!("archive_confirm:{}", parent.id)), &ctx);
        assert!(store.load(&parent.id).unwrap().unwrap().session.archived);
    }

    #[test]
    fn archived_native_root_deletion_removes_terminal_child_and_selected_route() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        let store = app.store.as_ref().unwrap().clone();
        let parent = store.create(&workspace, "native").unwrap();
        let parent_lease = store.try_acquire_native_lease(&parent.id).unwrap();
        let coordinator =
            SubagentCoordinator::new(store.clone(), SubagentLimits::default()).unwrap();
        let child = coordinator
            .create_child(
                &parent_lease,
                "delete-child",
                &openide_harness::SubagentSpec {
                    prompt: "Inspect".into(),
                    model: "fixture".into(),
                    mode: TurnMode::Build,
                    context_digest: "b".repeat(64),
                },
            )
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        coordinator
            .finish_child(&run, openide_harness::SubagentOutcome::Completed)
            .unwrap();
        drop(run);
        drop(parent_lease);
        store.set_archived(&parent.id, true).unwrap();
        app.view.show_archived = true;
        app.view.selected_session = Some(child.session.id.clone());
        app.refresh_sessions();
        let ctx = egui::Context::default();
        app.apply_action(clicked(format!("session_delete:{}", parent.id)), &ctx);
        app.apply_action(clicked(format!("delete_confirm:{}", parent.id)), &ctx);
        assert!(store.load(&parent.id).unwrap().is_none());
        assert!(store.load(&child.session.id).unwrap().is_none());
        assert!(app.view.selected_session.is_none());
    }

    #[test]
    fn interrupted_child_recovery_card_and_manual_verification_close_child_once() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.native_provider_id = "custom".into();
        app.view.model = "different-current-model".into();
        let store = app.store.as_ref().unwrap().clone();
        let parent = store.create(&workspace, "native").unwrap();
        let parent_lease = store.try_acquire_native_lease(&parent.id).unwrap();
        let coordinator =
            SubagentCoordinator::new(store.clone(), SubagentLimits::default()).unwrap();
        let child = coordinator
            .create_child(
                &parent_lease,
                "spawn-child-1",
                &openide_harness::SubagentSpec {
                    prompt: "Check a file".into(),
                    model: "former-model".into(),
                    mode: TurnMode::Build,
                    context_digest: "a".repeat(64),
                },
            )
            .unwrap();
        let child_id = child.session.id;
        let admission = store
            .admit_native_turn(
                &child_id,
                "spawn-child-1",
                "Check a file",
                "former-model",
                TurnMode::Build,
            )
            .unwrap();
        let run = coordinator.start_child(&parent_lease, &child_id).unwrap();
        let call = ToolCall {
            id: "write-1".into(),
            name: "write_file".into(),
            arguments: json!({"path":"note.txt","content":"hello"}),
        };
        let mut sequence = admission.sequence;
        for kind in [
            AgentEventKind::TurnStarted {
                prompt: "Check a file".into(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "former-model".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("", vec![call.clone()]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::ToolStarted { tool_call: call },
            AgentEventKind::TurnFailed {
                error: "reply lost".into(),
            },
        ] {
            sequence += 1;
            store
                .append_native_event(&AgentEvent::new(&child_id, sequence, kind))
                .unwrap();
        }
        drop(run);
        drop(parent_lease);
        app.open_session(&child_id);
        assert_eq!(
            app.view
                .recoverable_subagent
                .as_ref()
                .map(|child| child.child_session_id.as_str()),
            Some(child_id.as_str())
        );
        let frame = UiTree::new(build_view(&app.view, 1000.0)).layout(Size::new(1000.0, 760.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == format!("subagent_resume:{child_id}")));
        app.apply_action(
            clicked(format!("subagent_resume:{child_id}")),
            &egui::Context::default(),
        );
        assert_eq!(
            app.view
                .pending_tool_verification
                .as_ref()
                .map(|pending| pending.call_id.as_str()),
            Some("write-1")
        );
        assert!(app.active_child_recovery.is_none());
        app.view.tool_verification_output = "Checked note.txt externally".into();
        app.apply_action(
            clicked("tool_verification_confirm"),
            &egui::Context::default(),
        );
        assert!(
            app.view.pending_tool_verification.is_none(),
            "{}",
            app.view.status
        );
        assert_eq!(
            coordinator.list_children_for_ui(&parent.id).unwrap()[0].status,
            SubagentStatus::Failed,
            "{}",
            app.view.status
        );
        let events = store.load(&child_id).unwrap().unwrap().events;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ToolFinished { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn interrupted_child_aborted_batch_is_discarded_without_provider_credentials() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.native_provider_id = "custom".into();
        app.view.model = "different-current-model".into();
        let store = app.store.as_ref().unwrap().clone();
        let parent = store.create(&workspace, "native").unwrap();
        let parent_lease = store.try_acquire_native_lease(&parent.id).unwrap();
        let coordinator =
            SubagentCoordinator::new(store.clone(), SubagentLimits::default()).unwrap();
        let child = coordinator
            .create_child(
                &parent_lease,
                "spawn-batch",
                &openide_harness::SubagentSpec {
                    prompt: "Read then write".into(),
                    model: "former-model".into(),
                    mode: TurnMode::Build,
                    context_digest: "b".repeat(64),
                },
            )
            .unwrap();
        let child_id = child.session.id;
        let admission = store
            .admit_native_turn(
                &child_id,
                "spawn-batch",
                "Read then write",
                "former-model",
                TurnMode::Build,
            )
            .unwrap();
        let run = coordinator.start_child(&parent_lease, &child_id).unwrap();
        let first = ToolCall {
            id: "read-1".into(),
            name: "read_file".into(),
            arguments: json!({"path":"note.txt"}),
        };
        let second = ToolCall {
            id: "write-2".into(),
            name: "write_file".into(),
            arguments: json!({"path":"note.txt","content":"new"}),
        };
        let mut sequence = admission.sequence;
        for kind in [
            AgentEventKind::TurnStarted {
                prompt: "Read then write".into(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "former-model".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("", vec![first.clone(), second.clone()]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::ToolStarted {
                tool_call: first.clone(),
            },
            AgentEventKind::ToolFinished {
                tool_call_id: first.id,
                output: "read complete".into(),
                is_error: false,
            },
            AgentEventKind::TurnCancelled,
        ] {
            sequence += 1;
            store
                .append_native_event(&AgentEvent::new(&child_id, sequence, kind))
                .unwrap();
        }
        drop(run);
        drop(parent_lease);
        app.open_session(&child_id);
        app.start_resume();
        assert_eq!(
            app.view
                .pending_batch_discard
                .as_ref()
                .map(|batch| batch.next_call_id.as_str()),
            Some(second.id.as_str())
        );
        app.apply_action(clicked("aborted_batch_discard"), &egui::Context::default());
        assert!(
            app.view.pending_batch_discard.is_none(),
            "{}",
            app.view.status
        );
        assert_eq!(
            coordinator.list_children_for_ui(&parent.id).unwrap()[0].status,
            SubagentStatus::Cancelled,
            "{}",
            app.view.status
        );
        let events = store.load(&child_id).unwrap().unwrap().events;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ToolBatchDiscarded { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn invalid_mcp_arguments_do_not_overwrite_saved_configuration() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.change_workspace(workspace.to_string_lossy().into_owned());
        let ctx = egui::Context::default();
        app.apply_action(clicked("mcp_new"), &ctx);
        app.apply_action(
            UiAction::InputChanged {
                id: "mcp_draft_args".into(),
                value: "[7]".into(),
            },
            &ctx,
        );
        app.apply_action(clicked("mcp_save"), &ctx);
        assert!(app.view.settings_notice_error);
        assert!(app.view.mcp_editor_open);
        assert_eq!(app.view.mcp_draft_args, "[7]");
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Mcp;
        let frame = UiTree::new(build_view(&app.view, 1100.0)).layout(Size::new(1100.0, 720.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "popover_surface:mcp_cancel"));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text.contains("Argumentos MCP no válidos")
        )));
        assert!(app.view.mcp_servers.is_empty());
        assert!(!app.data_root.join("mcp-servers.json").exists());
    }

    #[test]
    fn mcp_http_editor_preserves_url_and_token_when_editing() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.change_workspace(workspace.to_string_lossy().into_owned());
        let ctx = egui::Context::default();

        app.apply_action(clicked("mcp_new"), &ctx);
        app.apply_action(clicked("mcp_transport:http"), &ctx);
        assert!(app.view.mcp_draft_http);
        for (id, value) in [
            ("mcp_draft_id", "remote"),
            ("mcp_draft_url", "https://example.com/mcp"),
            ("mcp_draft_http_bearer_token_env_name", "MCP_HTTP_TOKEN"),
        ] {
            app.apply_action(
                UiAction::InputChanged {
                    id: id.into(),
                    value: value.into(),
                },
                &ctx,
            );
        }
        app.apply_action(clicked("mcp_save"), &ctx);
        assert!(!app.view.mcp_editor_open);
        assert_eq!(
            app.view.mcp_servers[0].url.as_deref(),
            Some("https://example.com/mcp")
        );
        assert_eq!(
            app.view.mcp_servers[0]
                .http_bearer_token_env_name
                .as_deref(),
            Some("MCP_HTTP_TOKEN")
        );
        assert!(app.view.mcp_servers[0].program.is_empty());
        app.apply_action(clicked("mcp_toggle:remote"), &ctx);
        assert!(app.view.mcp_servers[0].enabled);

        app.apply_action(clicked("mcp_select:remote"), &ctx);
        assert!(app.view.mcp_draft_http);
        assert_eq!(app.view.mcp_draft_url, "https://example.com/mcp");
        assert_eq!(
            app.view.mcp_draft_http_bearer_token_env_name,
            "MCP_HTTP_TOKEN"
        );
        app.apply_action(clicked("mcp_save"), &ctx);
        assert!(!app.view.mcp_editor_open);
        let persisted = McpSettings::load(&app.data_root, &workspace).unwrap();
        assert_eq!(persisted.servers[0], app.view.mcp_servers[0]);
        assert!(persisted.servers[0].enabled);
        assert_eq!(
            persisted.servers[0].url.as_deref(),
            Some("https://example.com/mcp")
        );
        assert_eq!(
            persisted.servers[0].http_bearer_token_env_name.as_deref(),
            Some("MCP_HTTP_TOKEN")
        );

        app.apply_action(clicked("mcp_select:remote"), &ctx);
        app.apply_action(
            UiAction::InputChanged {
                id: "mcp_draft_url".into(),
                value: "not-a-url".into(),
            },
            &ctx,
        );
        app.apply_action(clicked("mcp_save"), &ctx);
        assert!(app.view.mcp_editor_open);
        assert!(app.view.settings_notice_error);
        assert_eq!(
            McpSettings::load(&app.data_root, &workspace)
                .unwrap()
                .servers,
            persisted.servers
        );
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
    fn explicitly_selected_native_backend_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().join("data");
        let mut preferences = ComposerPreferences::default();
        preferences.set_selected_backend("native");
        preferences.save(&data_root).unwrap();
        let reopened = AgentsWindow::with_data_root(data_root);
        assert_eq!(reopened.view.selected_backend, "native");
    }

    #[test]
    fn fresh_workspace_uses_native_backend_without_cli_fallback() {
        let root = tempfile::tempdir().unwrap();
        let app = AgentsWindow::with_data_root(root.path().join("data"));
        assert_eq!(app.view.selected_backend, "native");
    }

    #[test]
    fn selecting_codex_route_persists_backend_and_model_together() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        if !app
            .view
            .backend_options
            .iter()
            .any(|(backend, _)| backend == "codex")
        {
            app.view
                .backend_options
                .push(("codex".into(), "Codex".into()));
        }
        app.view.selected_backend = "native".into();
        app.refresh_composer_routes();
        let route = app
            .view
            .composer_routes
            .iter()
            .find(|route| route.backend == "codex" && route.model_id != "default")
            .unwrap()
            .clone();
        app.select_composer_route(&route.route_id);
        let preferences = ComposerPreferences::load(root.path());
        assert_eq!(app.view.selected_backend, "codex");
        assert_eq!(app.view.composer_model, route.model_id);
        assert_eq!(preferences.selected_backend(), Some("codex"));
        assert_eq!(preferences.choice("codex", "").model, route.model_id);
    }

    #[test]
    fn configured_native_endpoint_takes_initial_backend_priority() {
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().join("data");
        NativeProviderSettings::default().save(&data_root).unwrap();
        let app = AgentsWindow::with_data_root(data_root);
        assert_eq!(app.view.selected_backend, "native");
    }

    #[test]
    fn codex_account_catalog_replaces_stale_choice_and_effort_levels() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        app.view
            .backend_options
            .push(("codex".into(), "Codex".into()));
        app.view.selected_backend = "codex".into();
        app.refresh_composer_selection();
        app.select_composer_model("gpt-6-sol");
        app.select_composer_effort("ultra");

        let (sender, receiver) = mpsc::channel();
        app.codex_model_response = Some(receiver);
        app.view
            .composer_provider_status
            .insert("codex".into(), ProviderModelStatus::Checking);
        sender
            .send(Ok(vec![openide_cli::DiscoveredCodexModel {
                id: "gpt-6-luna".into(),
                display_name: "Luna disponible".into(),
                supported_efforts: vec!["low".into(), "medium".into(), "high".into()],
                is_default: true,
            }]))
            .unwrap();

        assert!(app.poll_codex_model_discovery());
        assert_eq!(app.view.composer_model, "default");
        assert_eq!(app.view.composer_effort, "default");
        assert_eq!(app.view.composer_model_options.len(), 2);
        assert!(app
            .view
            .composer_effort_options
            .iter()
            .any(|(effort, _)| effort == "high"));
        assert!(!app
            .view
            .composer_effort_options
            .iter()
            .any(|(effort, _)| effort == "ultra"));
        assert!(app.view.composer_routes.iter().any(|route| {
            route.model_id == "gpt-6-luna"
                && route.source == composer_selection::RouteSource::CliLiveCatalog
        }));
        assert_eq!(
            ComposerPreferences::load(root.path())
                .choice("codex", "")
                .model,
            "default"
        );
        app.select_composer_model("gpt-6-luna");
        app.select_composer_effort("high");
        assert_eq!(app.view.composer_effort, "high");
    }

    #[test]
    fn failed_provider_save_keeps_the_open_session_and_backend() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.selected_backend = "codex".into();
        app.view.selected_session = Some("still-open".into());
        app.view.native_provider_id = "custom".into();
        app.view.endpoint = "http://127.0.0.1:1234/v1".into();
        app.view.model = "local-before".into();
        app.refresh_composer_routes();
        let mut route = app
            .view
            .composer_routes
            .iter()
            .find(|route| route.provider_id == "custom")
            .unwrap()
            .clone();
        assert!(route.selectable());
        route.route_id = "native:custom:local-after".into();
        route.model_id = "local-after".into();
        app.view.composer_routes.push(route);
        let blocker = root.path().join("file-instead-of-directory");
        std::fs::write(&blocker, "occupied").unwrap();
        app.data_root = blocker;

        app.select_composer_route("native:custom:local-after");
        assert_eq!(app.view.selected_backend, "codex");
        assert_eq!(app.view.selected_session.as_deref(), Some("still-open"));
        assert_eq!(app.view.native_provider_id, "custom");
        assert_eq!(app.view.model, "local-before");
    }

    #[test]
    fn effort_slider_previews_then_snaps_to_the_models_advertised_tiers() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        app.view.selected_backend = "codex".to_owned();
        app.refresh_composer_selection();
        app.select_composer_model("gpt-6-sol");
        let last = app
            .view
            .composer_effort_options
            .iter()
            .rfind(|(id, _)| id != "default")
            .unwrap()
            .0
            .clone();
        let before = app.view.composer_effort.clone();
        app.view.composer_model_menu_open = true;

        app.apply_action(
            UiAction::SliderChanged {
                id: "composer_effort_slider".into(),
                fraction: 1.0,
                committed: false,
            },
            &ctx,
        );
        assert_eq!(app.view.composer_effort, before);
        assert_eq!(app.view.composer_effort_preview, Some(1.0));
        app.apply_action(
            UiAction::SliderChanged {
                id: "composer_effort_slider".into(),
                fraction: 1.0,
                committed: true,
            },
            &ctx,
        );
        assert_eq!(app.view.composer_effort, last);
        assert_eq!(app.view.composer_effort_preview, None);
        assert!(app.view.composer_model_menu_open);
        assert!(app.view.composer_advanced_open);
        assert_eq!(
            ComposerPreferences::load(root.path())
                .choice("codex", "")
                .effort,
            last
        );
    }

    #[test]
    fn model_selector_navigation_keeps_advanced_and_provider_list_distinct() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        app.view.composer_model_menu_open = true;
        app.view.composer_effort_options = vec![
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
        ];

        app.apply_action(
            UiAction::Clicked {
                id: "composer_advanced_toggle".into(),
            },
            &ctx,
        );
        assert!(app.view.composer_advanced_open);
        assert!(!app.view.composer_model_list_open);
        app.apply_action(
            UiAction::Clicked {
                id: "composer_model_list_toggle".into(),
            },
            &ctx,
        );
        assert!(!app.view.composer_model_list_open);

        app.apply_action(
            UiAction::Clicked {
                id: "composer_advanced_back".into(),
            },
            &ctx,
        );
        assert!(!app.view.composer_advanced_open);
        app.apply_action(
            UiAction::Clicked {
                id: "composer_advanced_chip_toggle".into(),
            },
            &ctx,
        );
        assert!(app.view.composer_advanced_open);
        app.apply_action(
            UiAction::Clicked {
                id: "composer_advanced_back".into(),
            },
            &ctx,
        );
        assert!(!app.view.composer_advanced_open);
        app.apply_action(
            UiAction::Clicked {
                id: "composer_model_list_toggle".into(),
            },
            &ctx,
        );
        assert!(app.view.composer_model_list_open);
        app.apply_action(
            UiAction::Clicked {
                id: "composer_model_dismiss".into(),
            },
            &ctx,
        );
        assert!(!app.view.composer_model_menu_open);
        assert!(!app.view.composer_model_list_open);
        assert!(!app.view.composer_advanced_open);
    }

    #[test]
    fn model_search_is_transient_and_preserves_provider_routes() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        let routes_before = app.view.composer_routes.clone();

        app.apply_action(clicked("composer_model_toggle"), &ctx);
        app.apply_action(clicked("composer_model_list_toggle"), &ctx);
        assert!(app.view.composer_model_list_open);
        assert!(app.composer_model_search_focus_pending);
        app.apply_action(
            UiAction::InputChanged {
                id: "composer_model_search".into(),
                value: "Anthropic Claude".into(),
            },
            &ctx,
        );
        assert_eq!(app.view.composer_model_query, "Anthropic Claude");
        assert_eq!(app.view.composer_routes, routes_before);

        app.apply_action(clicked("composer_model_list_toggle"), &ctx);
        assert!(app.view.composer_model_query.is_empty());
        assert!(!app.composer_model_search_focus_pending);
        app.apply_action(clicked("composer_model_list_toggle"), &ctx);
        assert!(app.composer_model_search_focus_pending);
        app.apply_action(
            UiAction::InputChanged {
                id: "composer_model_search".into(),
                value: "GPT".into(),
            },
            &ctx,
        );
        app.apply_action(clicked("composer_model_dismiss"), &ctx);
        assert!(app.view.composer_model_query.is_empty());
        assert!(!app.composer_model_search_focus_pending);
    }

    #[test]
    fn disconnected_provider_opens_connection_settings_without_changing_model() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        let mut route = app
            .view
            .composer_routes
            .iter()
            .find(|route| route.provider_id == "openai" && route.backend == "native")
            .unwrap()
            .clone();
        route.credential = composer_selection::CredentialAvailability::MissingCredential;
        app.view.composer_routes = vec![route.clone()];
        let previous_model = app.view.model.clone();
        let previous_provider = app.view.native_provider_id.clone();
        app.apply_action(clicked(format!("composer_route:{}", route.route_id)), &ctx);
        assert!(app.view.show_settings);
        assert_eq!(app.view.settings_page, SettingsPage::Providers);
        assert_eq!(app.view.catalog_browse_provider, "openai");
        assert_eq!(app.view.model, previous_model);
        assert_eq!(app.view.native_provider_id, previous_provider);
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
        app.apply_action(clicked("automation_save"), &ctx);
        assert_eq!(app.view.automations.screen, AutomationScreen::Edit);
        assert!(app.view.automations.error.is_some());
        assert!(app.view.automations.items.is_empty());
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
        app.apply_action(clicked("automation_next"), &ctx);
        assert_eq!(
            app.view.automations.editor_step,
            crate::automation_view::AutomationEditorStep::Schedule
        );
        app.apply_action(clicked("automation_next"), &ctx);
        assert_eq!(
            app.view.automations.editor_step,
            crate::automation_view::AutomationEditorStep::Agent
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
    fn dock_overview_reorders_and_reopens_a_recent_tab() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        let first = app.view.dock_tabs[0].id;
        let second = app.view.dock_tabs[1].id;
        app.apply_action(clicked(format!("dock_move_down:{first}")), &ctx);
        assert_eq!(app.view.dock_tabs[0].id, second);
        assert_eq!(app.view.dock_tabs[1].id, first);

        app.apply_action(clicked("dock_close_all"), &ctx);
        let closed = app
            .view
            .dock_recently_closed
            .iter()
            .find(|tab| tab.restorable)
            .expect("closed dock tab is available to restore")
            .id;
        app.apply_action(clicked("dock_overview_toggle"), &ctx);
        assert!(app.view.show_dock_overview);
        app.apply_action(
            UiAction::InputChanged {
                id: "dock_overview_query".into(),
                value: "contexto".into(),
            },
            &ctx,
        );
        assert_eq!(app.view.dock_overview_query, "contexto");
        app.apply_action(clicked(format!("dock_reopen:{closed}")), &ctx);
        assert!(!app.view.show_dock_overview);
        assert_eq!(app.view.dock_tabs.len(), 1);
        assert!(app.view.dock_tabs[0].active);
        assert_ne!(app.view.dock_tabs[0].id, closed);
        assert!(!app
            .view
            .dock_recently_closed
            .iter()
            .any(|tab| tab.id == closed));
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
        app.view.command_results[0].target = Target::OpenGitReview;
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
        assert!(app.view.session_menu_id.is_none());
        app.tree.set_root(build_view(&app.view, 1200.0));
        app.tree.layout(Size::new(1200.0, 720.0));
        app.focus_rename_if_pending();
        assert_eq!(app.tree.focused_id(), Some("rename_title"));
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
    fn resumed_history_uses_committed_model_items_once() {
        let call = ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: json!({"path":"src/main.rs"}),
        };
        let mut assistant = AgentMessage::assistant("Inspecting", vec![call.clone()]);
        assistant.provider_output_items = vec![
            json!({"type":"reasoning","encrypted_content":"opaque"}),
            json!({"type":"function_call","call_id":"call-1","name":"read_file","arguments":"{\"path\":\"src/main.rs\"}"}),
        ];
        let kinds = [
            AgentEventKind::TurnStarted {
                prompt: "Inspect".into(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "test".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: assistant.clone(),
                response_id: Some("resp-1".into()),
                usage: None,
            },
            AgentEventKind::AssistantMessage {
                content: "Inspecting".into(),
            },
            AgentEventKind::ToolStarted { tool_call: call },
            AgentEventKind::ToolFinished {
                tool_call_id: "call-1".into(),
                output: "fn main() {}".into(),
                is_error: false,
            },
            AgentEventKind::ModelRequestStarted {
                step: 2,
                model: "test".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 2,
                assistant: AgentMessage::assistant("Reviewed", Vec::new()),
                response_id: Some("resp-2".into()),
                usage: None,
            },
            AgentEventKind::AssistantMessage {
                content: "Reviewed".into(),
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
        assert_eq!(history[1], assistant);
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
    fn native_submit_admits_selected_mode_before_clearing_draft() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.view.selected_backend = "native".into();
        app.view.native_provider_id = "custom".into();
        app.view.endpoint = "http://127.0.0.1:9".into();
        app.view.model = "test-model".into();
        app.view.composer_effort = "default".into();
        app.view.prompt = "Inspect this project".into();
        let ctx = egui::Context::default();
        app.apply_action(clicked("turn_mode_toggle"), &ctx);
        assert_eq!(app.view.turn_mode, TurnMode::Plan);
        app.start_run();

        let session_id = app.view.selected_session.as_deref().unwrap().to_owned();
        let events = app
            .store
            .as_ref()
            .unwrap()
            .load(&session_id)
            .unwrap()
            .unwrap()
            .events;
        assert!(!events.is_empty(), "{}", app.view.status);
        assert!(matches!(
            &events[0].kind,
            AgentEventKind::TurnAdmitted { prompt, model, mode: TurnMode::Plan, .. }
                if prompt == "Inspect this project" && model == "test-model"
        ));
        assert!(app.view.prompt.is_empty());
        app.active_run.as_ref().unwrap().cancel();
        for _ in 0..100 {
            app.poll_run();
            if app.active_run.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.active_run.is_none());
        let saved = app
            .store
            .as_ref()
            .unwrap()
            .load(&session_id)
            .unwrap()
            .unwrap()
            .events;
        let sequences = saved
            .iter()
            .map(|event| event.sequence)
            .collect::<HashSet<_>>();
        assert_eq!(
            saved.len(),
            sequences.len(),
            "native UI duplicated journal events"
        );
    }

    #[test]
    fn native_steer_draft_retries_same_id_after_reopen_and_rotates_on_change() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let data_root = root.path().join("data");
        let store = SessionStore::open(data_root.join("sessions")).unwrap();
        let session = store.create(&workspace, "native").unwrap();
        store
            .admit_native_turn(&session.id, "turn-1", "First", "model", TurnMode::Build)
            .unwrap();
        let first = enqueue_native_steer_draft(
            &data_root,
            &store,
            NativeSteerDraftSubmission {
                workspace: &workspace,
                session_id: &session.id,
                turn_id: "turn-1",
                prompt: "Follow up",
                attachments: &[],
                payload_text: "Follow up",
                images: &[],
            },
        )
        .unwrap();

        // Simulate an ambiguous acknowledgement: the inbox write completed,
        // but the UI did not observe it. A new process must use the same ID.
        let reopened = SessionStore::open(data_root.join("sessions")).unwrap();
        let retry = enqueue_native_steer_draft(
            &data_root,
            &reopened,
            NativeSteerDraftSubmission {
                workspace: &workspace,
                session_id: &session.id,
                turn_id: "turn-1",
                prompt: "Follow up",
                attachments: &[],
                payload_text: "Follow up",
                images: &[],
            },
        )
        .unwrap();
        assert_eq!(retry, first);
        assert_eq!(
            reopened
                .pending_native_steers(&session.id, "turn-1")
                .unwrap()
                .len(),
            1
        );

        let changed = enqueue_native_steer_draft(
            &data_root,
            &reopened,
            NativeSteerDraftSubmission {
                workspace: &workspace,
                session_id: &session.id,
                turn_id: "turn-1",
                prompt: "Changed",
                attachments: &[],
                payload_text: "Changed",
                images: &[],
            },
        )
        .unwrap();
        assert_ne!(changed, first);
        assert_eq!(
            reopened
                .pending_native_steers(&session.id, "turn-1")
                .unwrap()
                .len(),
            2
        );
        acknowledge_native_steer_draft(&data_root, &reopened, &workspace, &session.id, &changed)
            .unwrap();
        assert!(load_native_steer_draft(&data_root, &session.id)
            .unwrap()
            .is_none());
        assert!(reopened
            .load_draft(&workspace, Some(&session.id))
            .unwrap()
            .is_none());
    }

    #[test]
    fn native_steer_image_only_draft_restores_attachment_and_changes_id_with_image_bytes() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let data_root = root.path().join("data");
        let store = SessionStore::open(data_root.join("sessions")).unwrap();
        let session = store.create(&workspace, "native").unwrap();
        store
            .admit_native_turn(&session.id, "turn-1", "First", "model", TurnMode::Build)
            .unwrap();
        let attachment = ComposerAttachmentView {
            path: "image.png".into(),
            label: "image.png".into(),
            is_dir: false,
        };
        let image_a = UserImage::from_bytes(UserImageMime::Png, b"\x89PNG\r\n\x1a\nfirst").unwrap();
        let image_b =
            UserImage::from_bytes(UserImageMime::Png, b"\x89PNG\r\n\x1a\nsecond").unwrap();
        let first = enqueue_native_steer_draft(
            &data_root,
            &store,
            NativeSteerDraftSubmission {
                workspace: &workspace,
                session_id: &session.id,
                turn_id: "turn-1",
                prompt: "",
                attachments: std::slice::from_ref(&attachment),
                payload_text: "Analiza la imagen adjunta",
                images: std::slice::from_ref(&image_a),
            },
        )
        .unwrap();

        let mut reopened_app = AgentsWindow::with_data_root(data_root.clone());
        reopened_app.view.workspace = workspace.to_string_lossy().into_owned();
        reopened_app.view.selected_session = Some(session.id.clone());
        reopened_app.restore_draft();
        assert!(reopened_app.view.prompt.is_empty());
        assert_eq!(
            reopened_app.view.composer_attachments,
            vec![attachment.clone()]
        );
        let reopened = SessionStore::open(data_root.join("sessions")).unwrap();
        let retry = enqueue_native_steer_draft(
            &data_root,
            &reopened,
            NativeSteerDraftSubmission {
                workspace: &workspace,
                session_id: &session.id,
                turn_id: "turn-1",
                prompt: "",
                attachments: std::slice::from_ref(&attachment),
                payload_text: "Analiza la imagen adjunta",
                images: std::slice::from_ref(&image_a),
            },
        )
        .unwrap();
        assert_eq!(retry, first);
        let changed_image = enqueue_native_steer_draft(
            &data_root,
            &reopened,
            NativeSteerDraftSubmission {
                workspace: &workspace,
                session_id: &session.id,
                turn_id: "turn-1",
                prompt: "",
                attachments: std::slice::from_ref(&attachment),
                payload_text: "Analiza la imagen adjunta",
                images: std::slice::from_ref(&image_b),
            },
        )
        .unwrap();
        assert_ne!(changed_image, first);
        assert_eq!(
            reopened
                .pending_native_steers(&session.id, "turn-1")
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn accepted_native_steer_marker_suppresses_stale_text_draft_after_restart() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let data_root = root.path().join("data");
        let store = SessionStore::open(data_root.join("sessions")).unwrap();
        let session = store.create(&workspace, "native").unwrap();
        store
            .admit_native_turn(&session.id, "turn-1", "First", "model", TurnMode::Build)
            .unwrap();
        let id = enqueue_native_steer_draft(
            &data_root,
            &store,
            NativeSteerDraftSubmission {
                workspace: &workspace,
                session_id: &session.id,
                turn_id: "turn-1",
                prompt: "Follow up",
                attachments: &[],
                payload_text: "Follow up",
                images: &[],
            },
        )
        .unwrap();
        let mut state = load_native_steer_draft(&data_root, &session.id)
            .unwrap()
            .unwrap();
        assert_eq!(state.retry.as_ref().unwrap().input_id, id);
        state.retry.as_mut().unwrap().acknowledged = true;
        save_native_steer_draft(&data_root, &state).unwrap();
        let accepted_retry = enqueue_native_steer_draft(
            &data_root,
            &store,
            NativeSteerDraftSubmission {
                workspace: &workspace,
                session_id: &session.id,
                turn_id: "turn-1",
                prompt: "Follow up",
                attachments: &[],
                payload_text: "Follow up",
                images: &[],
            },
        )
        .unwrap();
        assert_eq!(accepted_retry, id);
        assert_eq!(
            store
                .pending_native_steers(&session.id, "turn-1")
                .unwrap()
                .len(),
            1
        );
        // Simulate a crash after the accepted marker but before cleanup.
        let mut reopened = AgentsWindow::with_data_root(data_root.clone());
        reopened.view.workspace = workspace.to_string_lossy().into_owned();
        reopened.view.selected_session = Some(session.id.clone());
        reopened.restore_draft();
        assert!(reopened.view.prompt.is_empty());
        assert!(load_native_steer_draft(&data_root, &session.id)
            .unwrap()
            .is_none());
        assert!(store
            .load_draft(&workspace, Some(&session.id))
            .unwrap()
            .is_none());
    }

    #[test]
    fn native_run_commits_saved_hook_digest_and_invalid_hooks_block_admission() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let data_root = root.path().join("data");
        let executable = root.path().join("trusted-hook.bin");
        std::fs::write(&executable, b"trusted hook").unwrap();
        let hook = openide_harness::CommandHookConfig {
            id: "review".into(),
            phase: HookPhase::PreToolUse,
            tool_name: "write_file".into(),
            executable: executable.clone(),
            args: vec![],
            cwd: workspace.clone(),
            timeout_ms: 1000,
            max_output_bytes: 4096,
        };
        NativeHookSettings { hooks: vec![hook] }
            .save(&data_root)
            .unwrap();
        let expected_digest = NativeHookSettings::load(&data_root)
            .unwrap()
            .registry()
            .unwrap()
            .digest_hex()
            .unwrap();
        let mut app = AgentsWindow::with_data_root(data_root.clone());
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.view.selected_backend = "native".into();
        app.view.native_provider_id = "custom".into();
        app.view.endpoint = "http://127.0.0.1:9".into();
        app.view.model = "test-model".into();
        app.view.composer_effort = "default".into();
        app.view.prompt = "Inspect".into();
        app.start_run();
        let session_id = app.view.selected_session.clone().unwrap();
        for _ in 0..100 {
            app.poll_run();
            if app.active_run.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let events = app
            .store
            .as_ref()
            .unwrap()
            .load(&session_id)
            .unwrap()
            .unwrap()
            .events;
        assert!(events.iter().any(|event| matches!(
            &event.kind, AgentEventKind::HooksCommitted { digest, .. } if digest == &expected_digest
        )), "{:?}", app.view.status);
        if let Some(active) = app.active_run.as_ref() {
            active.cancel();
        }
        for _ in 0..100 {
            app.poll_run();
            if app.active_run.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.active_run.is_none());

        std::fs::write(&executable, b"changed hook").unwrap();
        std::fs::remove_file(&executable).unwrap();
        app.view.selected_session = None;
        app.view.prompt = "Keep this draft".into();
        app.start_run();
        assert!(app.active_run.is_none());
        assert_eq!(app.view.prompt, "Keep this draft");
        assert!(app.view.status.contains("hooks"));
    }

    #[test]
    fn failed_native_replay_keeps_unsent_prompt() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.view.selected_backend = "native".into();
        app.view.native_provider_id = "custom".into();
        app.view.endpoint = "http://127.0.0.1:9".into();
        app.view.model = "test-model".into();
        app.view.composer_effort = "default".into();
        app.view.prompt = "Keep this draft".into();
        let session = app
            .store
            .as_ref()
            .unwrap()
            .create(&workspace, "native")
            .unwrap();
        app.view.selected_session = Some(session.id.clone());
        std::fs::create_dir(
            root.path()
                .join("data/sessions")
                .join(format!("{}.jsonl", session.id)),
        )
        .unwrap();

        app.start_run();
        assert_eq!(app.view.prompt, "Keep this draft");
        assert!(app.active_run.is_none());
        assert!(
            app.view.status.contains("No se pudo leer la sesión nativa"),
            "{}",
            app.view.status
        );
    }

    #[test]
    fn reopened_sessions_expose_recovery_and_invalid_journal_cannot_auto_retry() {
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().join("data");
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(data_root.clone());
        let store = app.store.as_ref().unwrap().clone();
        let pending = store.create(&workspace, "native").unwrap();
        store
            .admit_native_turn(&pending.id, "pending", "Continue", "model", TurnMode::Build)
            .unwrap();
        let corrupt = store.create(&workspace, "native").unwrap();
        let admission = store
            .admit_native_turn(&corrupt.id, "corrupt", "Check", "model", TurnMode::Build)
            .unwrap();
        store
            .append_native_event(&AgentEvent::new(
                &corrupt.id,
                admission.sequence + 1,
                AgentEventKind::ToolFinished {
                    tool_call_id: "never-started".into(),
                    output: String::new(),
                    is_error: false,
                },
            ))
            .unwrap();
        app.refresh_sessions();
        assert_eq!(
            app.view.session_recovery.get(&pending.id),
            Some(&SessionRecoveryView::Resume)
        );
        assert!(matches!(
            app.view.session_recovery.get(&corrupt.id),
            Some(SessionRecoveryView::InvalidJournal(_))
        ));
        drop(app);

        let mut reopened = AgentsWindow::with_data_root(data_root);
        assert_eq!(
            reopened.view.session_recovery.get(&pending.id),
            Some(&SessionRecoveryView::Resume)
        );
        reopened.apply_action(
            clicked(format!("session_recover:{}", corrupt.id)),
            &egui::Context::default(),
        );
        assert_ne!(
            reopened.view.selected_session.as_deref(),
            Some(corrupt.id.as_str())
        );
        reopened.apply_action(
            clicked(format!("session_recover:{}", pending.id)),
            &egui::Context::default(),
        );
        assert_eq!(
            reopened.view.selected_session.as_deref(),
            Some(pending.id.as_str())
        );
    }

    #[test]
    fn plan_handoff_is_visible_after_reopen_and_consumed_by_build_admission() {
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().join("data");
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let app = AgentsWindow::with_data_root(data_root.clone());
        let store = app.store.as_ref().unwrap().clone();
        let session = store.create(&workspace, "native").unwrap();
        let admission = store
            .admit_native_turn(
                &session.id,
                "plan",
                "Plan the work",
                "model",
                TurnMode::Plan,
            )
            .unwrap();
        let plan_text = "1. Inspect files\n2. Make the change";
        let mut sequence = admission.sequence;
        for kind in [
            AgentEventKind::TurnStarted {
                prompt: "Plan the work".into(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "model".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant(plan_text, vec![]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::AssistantMessage {
                content: plan_text.into(),
            },
            AgentEventKind::PlanProposed {
                plan: openide_core::PlanSnapshot::from_plan_response(plan_text),
            },
            AgentEventKind::TurnCompleted {
                response: plan_text.into(),
                steps: 1,
            },
        ] {
            sequence += 1;
            store
                .append_native_event(&AgentEvent::new(&session.id, sequence, kind))
                .unwrap();
        }
        drop(app);

        let mut reopened = AgentsWindow::with_data_root(data_root);
        let plan = reopened.view.session_plans.get(&session.id).unwrap();
        assert!(plan.can_build);
        assert_eq!(plan.snapshot.plan.len(), 2);
        store
            .admit_native_turn(&session.id, "build", "Build it", "model", TurnMode::Build)
            .unwrap();
        reopened.refresh_sessions();
        assert!(
            !reopened
                .view
                .session_plans
                .get(&session.id)
                .unwrap()
                .can_build
        );
        assert_eq!(
            reopened.view.session_recovery.get(&session.id),
            Some(&SessionRecoveryView::Resume)
        );
    }

    #[test]
    fn aborted_batch_can_be_discarded_without_model_credentials() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.view.selected_backend = "native".into();
        app.view.native_provider_id = "custom".into();
        app.view.model = "unverified-model".into();
        let store = app.store.as_ref().unwrap().clone();
        let session = store.create(&workspace, "native").unwrap();
        app.view.selected_session = Some(session.id.clone());
        let admission = store
            .admit_native_turn(
                &session.id,
                "aborted-turn",
                "Make two changes",
                "unverified-model",
                TurnMode::Build,
            )
            .unwrap();
        let first = ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path":"notes.txt"}),
        };
        let second = ToolCall {
            id: "call-2".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path":"notes.txt","content":"done"}),
        };
        let mut sequence = admission.sequence;
        for kind in [
            AgentEventKind::TurnStarted {
                prompt: "Make two changes".into(),
            },
            AgentEventKind::ToolCatalogCommitted {
                digest: "a".repeat(64),
            },
            AgentEventKind::ModelContextCommitted {
                version: openide_core::MODEL_CONTEXT_VERSION,
                digest: "b".repeat(64),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "unverified-model".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("", vec![first.clone(), second.clone()]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::ToolStarted {
                tool_call: first.clone(),
            },
            AgentEventKind::ToolFinished {
                tool_call_id: first.id,
                output: "confirmed read".into(),
                is_error: false,
            },
            AgentEventKind::TurnCancelled,
        ] {
            sequence += 1;
            store
                .append_native_event(&AgentEvent::new(&session.id, sequence, kind))
                .unwrap();
        }

        app.refresh_sessions();
        assert_eq!(
            app.view.session_recovery.get(&session.id),
            Some(&SessionRecoveryView::ReviewBatch)
        );
        app.apply_action(
            clicked(format!("session_recover:{}", session.id)),
            &egui::Context::default(),
        );
        let pending = app.view.pending_batch_discard.as_ref().unwrap();
        assert_eq!(pending.next_call_id, second.id);
        assert_eq!(pending.completed_calls, 1);
        assert_eq!(pending.remaining_calls, 1);
        assert!(app.active_resume.is_none());
        app.apply_action(clicked("aborted_batch_discard"), &egui::Context::default());
        assert!(
            app.view.pending_batch_discard.is_none(),
            "{}",
            app.view.status
        );
        let stored = store.load(&session.id).unwrap().unwrap();
        let replay = replay_native(
            &session.id,
            &stored.events,
            None,
            &native_checkpoint_compatibility(
                "custom",
                "unverified-model",
                NativeWireApi::ChatCompletions,
            ),
        )
        .unwrap();
        assert_eq!(replay.recovery, ReplayRecovery::Idle);
        assert!(replay.can_start_new_turn);
        assert!(stored.events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolBatchDiscarded { next_tool_call_id } if next_tool_call_id == "call-2"
        )));
    }

    #[test]
    fn uncertain_tool_can_be_verified_without_model_credentials() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.view.selected_backend = "native".into();
        app.view.native_provider_id = "custom".into();
        app.view.model = "different-current-model".into();
        let store = app.store.as_ref().unwrap().clone();
        let session = store.create(&workspace, "native").unwrap();
        app.view.selected_session = Some(session.id.clone());
        let admission = store
            .admit_native_turn(
                &session.id,
                "uncertain-turn",
                "Make a change",
                "former-model",
                TurnMode::Build,
            )
            .unwrap();
        let call = ToolCall {
            id: "write-1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path":"note.txt","content":"hello"}),
        };
        let mut sequence = admission.sequence;
        for kind in [
            AgentEventKind::TurnStarted {
                prompt: "Make a change".into(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "former-model".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("", vec![call.clone()]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::ToolStarted { tool_call: call },
            AgentEventKind::TurnFailed {
                error: "tool reply lost".into(),
            },
        ] {
            sequence += 1;
            store
                .append_native_event(&AgentEvent::new(&session.id, sequence, kind))
                .unwrap();
        }

        app.refresh_sessions();
        assert_eq!(
            app.view.session_recovery.get(&session.id),
            Some(&SessionRecoveryView::VerifyTool)
        );
        app.apply_action(
            clicked(format!("session_recover:{}", session.id)),
            &egui::Context::default(),
        );
        assert_eq!(
            app.view.pending_tool_verification.as_ref().unwrap().call_id,
            "write-1"
        );
        assert!(app.active_resume.is_none());
        app.view.tool_verification_output = "Checked note.txt on disk".into();
        app.apply_action(
            clicked("tool_verification_confirm"),
            &egui::Context::default(),
        );

        assert!(
            app.view.pending_tool_verification.is_none(),
            "{}",
            app.view.status
        );
        let stored = store.load(&session.id).unwrap().unwrap();
        let replay = replay_native(
            &session.id,
            &stored.events,
            None,
            &native_checkpoint_compatibility(
                "custom",
                "former-model",
                NativeWireApi::ChatCompletions,
            ),
        )
        .unwrap();
        assert_eq!(replay.recovery, ReplayRecovery::Idle);
        assert!(replay.can_start_new_turn);
        assert!(stored.events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { tool_call_id, output, .. }
                if tool_call_id == "write-1" && output == "Checked note.txt on disk"
        )));
    }

    #[test]
    fn native_submit_does_not_repeat_a_tool_with_uncertain_outcome() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.view.selected_backend = "native".into();
        app.view.native_provider_id = "custom".into();
        app.view.endpoint = "http://127.0.0.1:9".into();
        app.view.model = "test-model".into();
        app.view.composer_effort = "default".into();
        app.view.prompt = "Continue after interruption".into();
        let store = app.store.as_ref().unwrap().clone();
        let session = store.create(&workspace, "native").unwrap();
        app.view.selected_session = Some(session.id.clone());
        let admission = store
            .admit_native_turn(
                &session.id,
                "original-turn",
                "Original request",
                "test-model",
                TurnMode::Build,
            )
            .unwrap();
        let call = ToolCall {
            id: "write-1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path":"note.txt","content":"hello"}),
        };
        let mut sequence = admission.sequence;
        for kind in [
            AgentEventKind::TurnStarted {
                prompt: "Original request".into(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "test-model".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("", vec![call.clone()]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::ToolStarted { tool_call: call },
        ] {
            sequence += 1;
            store
                .append_native_event(&AgentEvent::new(&session.id, sequence, kind))
                .unwrap();
        }

        app.start_run();
        assert_eq!(app.view.prompt, "Continue after interruption");
        assert!(app.active_run.is_none());
        assert!(app.view.status.contains("Herramienta interrumpida"));
        let events = store.load(&session.id).unwrap().unwrap().events;
        assert_eq!(events.len() as u64, sequence);
    }

    #[test]
    fn compact_command_requires_a_selected_native_session() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.command_results = vec![command_center::ResultItem {
            target: Target::CompactContext,
            section: Scope::Commands,
            title: "Compactar contexto".into(),
            subtitle: String::new(),
        }];
        app.activate_command_result(0);
        assert_eq!(
            app.view.status,
            "Selecciona una sesión nativa para compactar"
        );
        assert!(app.active_compact.is_none());
    }

    #[test]
    fn native_build_uses_enabled_mcp_before_requesting_a_model() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.view.selected_backend = "native".into();
        app.view.native_provider_id = "custom".into();
        app.view.endpoint = "http://127.0.0.1:9".into();
        app.view.model = "test-model".into();
        app.view.composer_effort = "default".into();
        app.view.prompt = "Use configured tools".into();
        McpSettings {
            servers: vec![crate::mcp_settings::McpServerSettings {
                id: "fixture".into(),
                enabled: true,
                program: "openide_missing_mcp_fixture_executable_987654".into(),
                args: Vec::new(),
                cwd: None,
                credential_env_names: Vec::new(),
                url: None,
                http_bearer_token_env_name: None,
                http_oauth: None,
            }],
        }
        .save(&app.data_root, &workspace)
        .unwrap();

        app.start_run();
        let session_id = app.view.selected_session.clone().unwrap();
        for _ in 0..100 {
            app.poll_run();
            if app.active_run.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.active_run.is_none());
        let events = app
            .store
            .as_ref()
            .unwrap()
            .load(&session_id)
            .unwrap()
            .unwrap()
            .events;
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::TurnFailed { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ModelRequestStarted { .. })));
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

    #[test]
    fn command_search_keeps_valid_hits_when_one_source_fails_and_reports_limits() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        app.search_worker = Some(SearchWorker::spawn(root.path().join("sessions")).unwrap());
        app.view.show_command_center = true;
        app.view.command_scope = Scope::Sessions;
        app.view.command_query = "aguja".into();
        app.request_command_search();
        assert!(app.view.command_search_loading);

        let response = SearchResponse {
            workspace: PathBuf::from(&app.view.workspace),
            query: "aguja".into(),
            generation: app.search_generation,
            sessions: (0..14)
                .map(|index| crate::search_worker::SessionHit {
                    id: format!("session-{index}"),
                    title: format!("aguja {index}"),
                    backend: "native".into(),
                    workspace: PathBuf::from(&app.view.workspace),
                    snippets: Vec::new(),
                    pinned: false,
                    archived: false,
                })
                .collect(),
            session_total: 20,
            session_has_more: true,
            session_diagnostics: Vec::new(),
            session_error: None,
            files: Vec::new(),
            file_matches: 0,
            files_truncated: false,
            file_error: Some("acceso denegado".into()),
        };
        let stale = SearchResponse {
            generation: response.generation.wrapping_sub(1),
            ..response.clone()
        };
        assert!(!app.apply_command_search_response(stale));
        assert!(app.view.command_search_loading);
        assert!(app.apply_command_search_response(response));
        assert!(!app.view.command_search_loading);
        assert!(app.view.command_search_errors.is_empty());
        assert!(app.view.command_search_truncated);
        assert_eq!(
            app.view.command_results.len(),
            command_center::DEFAULT_RESULT_LIMIT
        );

        app.view.command_scope = Scope::All;
        app.request_command_search();
        let partial = SearchResponse {
            workspace: PathBuf::from(&app.view.workspace),
            query: "aguja".into(),
            generation: app.search_generation,
            sessions: Vec::new(),
            session_total: 0,
            session_has_more: false,
            session_diagnostics: Vec::new(),
            session_error: None,
            files: Vec::new(),
            file_matches: 0,
            files_truncated: false,
            file_error: Some("acceso denegado".into()),
        };
        assert!(app.apply_command_search_response(partial));
        assert_eq!(
            app.view.command_search_errors,
            ["Archivos: acceso denegado"]
        );
        assert!(!app.view.command_search_truncated);

        app.view.command_scope = Scope::Commands;
        app.request_command_search();
        assert!(!app.view.command_search_loading);
        assert!(app.view.command_search_errors.is_empty());
    }
}
