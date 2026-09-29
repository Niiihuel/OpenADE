use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};

use openide_core::{OutputStream, PlanSnapshot, SessionSort, TurnMode};
use openide_harness::{
    CommandIsolation, HostedWebSearchMode, PersistentCommandExit, PersistentCommandSessionInfo,
    ToolPermissions,
};
use openide_ui::{
    guides_for_depths, AlertDialog, Button, ButtonVariant, Color, Component, ControlSize,
    DropdownList, DropdownMenu, DropdownMenuEntry, DropdownMenuItem, DropdownOption, Element,
    Empty, FontFamily, Icon, IconButton as UiIconButton, Kbd, PopoverPlacement, ScrollArea, Search,
    SegmentedControl, Skeleton, Tab, Tabs, TextSpan, Tooltip, TooltipPlacement, TreeGuides,
    TreeRow, ICON_SIZE,
};
use serde_json::Value;

use crate::{
    automation_view::{self, AutomationViewState},
    command_center::{ResultItem, Scope},
    composer_selection::ModelRouteOption,
    composer_ui::{build_composer, ComposerProps},
    files::PreviewPage,
    git_review::{DiffStage, FileDiffDetail, GitReview, ReviewFile, ReviewLine, ReviewLineKind},
    github::GithubStatus,
    github_account::GithubAccountStatus,
    home,
    marketplace_view::{self, MarketplaceViewState},
    model_catalog::CatalogSnapshot,
    session_groups::{SessionGroup, SessionGroups},
    settings_preferences::TerminalShell,
    settings_ui::{self, SettingsPage},
    terminal::{TerminalColor, TerminalSnapshot},
    theme,
    transcript::TranscriptItem,
};

mod command_center;
pub(crate) use command_center::build_command_center;

const GAP: f32 = 4.0;
const HEADER: f32 = 48.0;
const RIGHT_HEADER_HEIGHT: f32 = 64.0;
/// The center and the right dock are two panes of the same workspace surface.
/// Keep their divider independent of the gap around the separate left dock.
const RIGHT_DIVIDER: f32 = 1.0;
const MIN_CENTER_WITH_RIGHT: f32 = 480.0;
const MIN_CENTER_COMPACT: f32 = 320.0;
const MIN_RIGHT_DOCK: f32 = 320.0;
const WORKSPACE_RADIUS: f32 = 12.0;
const ACTIVITY_PAGE_SIZE: usize = 20;
const REVIEW_PAGE_SIZE: usize = 20;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SideTab {
    #[default]
    Context,
    Files,
    Activity,
    CodeReview,
    FilePreview,
    Terminal,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SessionOrganization {
    #[default]
    Project,
    Recent,
    Group,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MainPage {
    #[default]
    Workspace,
    Automations,
    Marketplace,
}

#[derive(Clone, Debug)]
pub struct FileRowView {
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub can_load_more: bool,
}

#[derive(Clone, Debug)]
pub struct DockTabView {
    pub id: u64,
    pub title: String,
    pub active: bool,
    pub kind: SideTab,
    pub closable: bool,
}

#[derive(Clone, Debug)]
pub struct DockRecentlyClosedView {
    pub id: u64,
    pub title: String,
    pub kind: SideTab,
    pub restorable: bool,
}

#[derive(Clone, Debug)]
pub struct FilePreviewView {
    pub tab_id: u64,
    pub path: String,
    pub page_index: usize,
    pub content: Option<Result<PreviewPage, String>>,
}

#[derive(Clone, Debug)]
pub struct ReviewDetailView {
    pub path: String,
    pub stage: DiffStage,
    pub page_index: usize,
    pub content: Option<Result<Option<FileDiffDetail>, String>>,
}

#[derive(Clone, Debug)]
pub struct PermissionView {
    pub request_id: String,
    pub tool_name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionApprovalGrantView {
    pub tool_name: String,
    pub key_digest: String,
}

#[derive(Clone, Debug)]
pub struct SubagentPermissionView {
    pub child_session_id: String,
    pub parent_session_id: String,
    pub request_id: String,
    pub tool_name: String,
    pub arguments: String,
}

#[derive(Clone, Debug)]
pub struct SubagentRunView {
    pub child_session_id: String,
    pub parent_session_id: String,
    pub title: String,
}

#[derive(Clone, Debug)]
pub struct ToolVerificationView {
    pub session_id: String,
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AbortedToolBatchView {
    pub session_id: String,
    pub next_call_id: String,
    pub next_tool_name: String,
    pub next_arguments: String,
    pub completed_calls: usize,
    pub remaining_calls: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComposerAttachmentView {
    /// Workspace-relative path using `/` as a display and prompt separator.
    pub path: String,
    pub label: String,
    pub is_dir: bool,
}

#[derive(Clone)]
pub struct SessionView {
    pub id: String,
    pub title: String,
    pub project: String,
    pub backend: String,
    pub pinned: bool,
    pub archived: bool,
    pub unread: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionRecoveryView {
    Resume,
    Permission,
    VerifyTool,
    ReviewBatch,
    InvalidJournal(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionPlanView {
    pub snapshot: PlanSnapshot,
    pub can_build: bool,
}

impl SessionRecoveryView {
    pub fn label(&self) -> &str {
        match self {
            Self::Resume => "Reanudar turno",
            Self::Permission => "Resolver permiso y reanudar",
            Self::VerifyTool => "Verificar herramienta",
            Self::ReviewBatch => "Revisar llamadas pendientes",
            Self::InvalidJournal(_) => "Journal no verificable",
        }
    }

    pub fn can_open(&self) -> bool {
        !matches!(self, Self::InvalidJournal(_))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderModelStatus {
    Checking,
    CheckFailed,
}

/// Local, transient UI state. The browser URL contains a one-time OAuth state
/// and must not be serialized into settings, logs, or the run journal.
#[derive(Clone, Eq, PartialEq)]
pub(crate) enum McpOAuthViewPhase {
    Discovering,
    AwaitingBrowser { url: String },
    Authorized,
    Failed(String),
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct McpOAuthViewStatus {
    pub server_id: String,
    pub phase: McpOAuthViewPhase,
}

pub struct ViewState {
    pub workspace: String,
    pub workspace_draft: String,
    /// Cached for the composer so opening the Git review tab is not required.
    pub project_branch: Option<String>,
    pub prompt: String,
    pub composer_attachments: Vec<ComposerAttachmentView>,
    pub composer_add_menu_open: bool,
    pub composer_listening: bool,
    /// Persistent objective for the selected project and session draft.
    pub composer_goal: Option<String>,
    pub composer_goal_editor_open: bool,
    pub composer_goal_draft: String,
    pub composer_project_menu_open: bool,
    /// Absolute workspace path and display name, in stable menu order.
    pub composer_project_options: Vec<(String, String)>,
    pub endpoint: String,
    pub model: String,
    pub native_provider_id: String,
    pub hosted_web_search_mode: Option<HostedWebSearchMode>,
    pub catalog: Option<Arc<CatalogSnapshot>>,
    pub catalog_status: String,
    pub catalog_refreshing: bool,
    pub catalog_browse_provider: String,
    pub catalog_provider_query: String,
    pub catalog_model_query: String,
    pub backend_options: Vec<(String, String)>,
    pub selected_backend: String,
    pub backend_menu_open: bool,
    pub composer_model: String,
    pub composer_model_options: Vec<(String, String)>,
    pub composer_routes: Vec<ModelRouteOption>,
    pub composer_provider_status: BTreeMap<String, ProviderModelStatus>,
    pub composer_model_menu_open: bool,
    pub composer_model_list_open: bool,
    /// Transient filter for the model list; never persisted with provider settings.
    pub composer_model_query: String,
    pub composer_advanced_open: bool,
    pub composer_effort: String,
    pub composer_effort_options: Vec<(String, String)>,
    pub composer_effort_menu_open: bool,
    /// Transient slider position while dragging; persisted only after release.
    pub composer_effort_preview: Option<f32>,
    pub composer_effort_hovered: bool,
    /// Last provider-reported context use for the selected native session.
    /// Unknown usage or an unverified model limit leaves the badge hidden.
    pub composer_context_percent: Option<u8>,
    pub turn_mode: TurnMode,
    pub sessions: Vec<SessionView>,
    pub session_plans: BTreeMap<String, SessionPlanView>,
    pub session_recovery: BTreeMap<String, SessionRecoveryView>,
    pub sessions_status: Option<String>,
    pub selected_session: Option<String>,
    pub selected_session_title: Option<String>,
    pub file_rows: Vec<FileRowView>,
    pub files_status: String,
    pub file_root_can_load_more: bool,
    pub show_archived: bool,
    pub session_organization: SessionOrganization,
    pub session_sort: SessionSort,
    pub session_sort_menu_open: bool,
    pub collapsed_projects: BTreeSet<String>,
    pub session_groups: SessionGroups,
    pub group_create_open: bool,
    pub group_create_draft: String,
    pub group_menu_id: Option<String>,
    pub group_rename_id: Option<String>,
    pub group_rename_draft: String,
    pub pending_delete_group_id: Option<String>,
    pub session_menu_id: Option<String>,
    pub renaming_session_id: Option<String>,
    pub rename_draft: String,
    pub pending_archive_session_id: Option<String>,
    pub pending_delete_session_id: Option<String>,
    pub transcript: Vec<TranscriptItem>,
    pub running: bool,
    pub background_run_selected: bool,
    pub status: String,
    pub settings_notice: Option<String>,
    pub settings_notice_error: bool,
    pub native_key_available: bool,
    pub workspace_writes_enabled: bool,
    pub native_tool_permissions: ToolPermissions,
    pub native_command_isolation: CommandIsolation,
    pub session_approval_grants: Vec<SessionApprovalGrantView>,
    pub(crate) native_commands: Vec<PersistentCommandSessionInfo>,
    pub(crate) native_commands_error: Option<String>,
    pub(crate) pending_native_command_termination: Option<String>,
    pub(crate) native_command_terminating: Option<String>,
    pub(crate) native_hooks: crate::native_hook_settings::HookUiState,
    pub(crate) native_exec_rules: crate::exec_policy_settings::ExecPolicyUiState,
    pub(crate) native_skills: crate::native_skills_settings::NativeSkillsUiState,
    pub settings_reduced_motion: bool,
    pub settings_reopen_last_workspace: bool,
    pub settings_terminal_shell: TerminalShell,
    pub settings_terminal_shell_draft: String,
    pub(crate) mcp_servers: Vec<crate::mcp_settings::McpServerSettings>,
    /// The saved ID being edited; `None` means the form creates a new server.
    pub(crate) mcp_selected_id: Option<String>,
    pub(crate) mcp_editor_open: bool,
    pub(crate) pending_mcp_delete_id: Option<String>,
    pub(crate) mcp_draft_id: String,
    pub(crate) mcp_draft_http: bool,
    pub(crate) mcp_draft_url: String,
    /// Name of the bearer token environment variable; never the token value.
    pub(crate) mcp_draft_http_bearer_token_env_name: String,
    pub(crate) mcp_draft_oauth_enabled: bool,
    pub(crate) mcp_draft_oauth_client_id: String,
    pub(crate) mcp_draft_oauth_issuer: String,
    /// One requested OAuth scope per line; no token or client secret.
    pub(crate) mcp_draft_oauth_scopes: String,
    pub(crate) mcp_oauth_status: Option<McpOAuthViewStatus>,
    pub(crate) mcp_draft_program: String,
    /// JSON array of literal process arguments, preserving empty strings.
    pub(crate) mcp_draft_args: String,
    /// Workspace-relative directory, or empty to use the workspace root.
    pub(crate) mcp_draft_cwd: String,
    /// Credential variable names only, one per line; values never enter ViewState.
    pub(crate) mcp_draft_env_names: String,
    pub(crate) mcp_load_error: Option<String>,
    pub show_settings: bool,
    pub main_page: MainPage,
    pub automations: AutomationViewState,
    pub marketplace: MarketplaceViewState,
    pub settings_page: SettingsPage,
    pub settings_nav_open: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub show_command_center: bool,
    pub command_query: String,
    pub command_scope: Scope,
    pub command_results: Vec<ResultItem>,
    pub command_selected_index: usize,
    pub command_search_loading: bool,
    pub command_search_errors: Vec<String>,
    pub command_search_truncated: bool,
    pub show_side_pane: bool,
    pub sidebar_collapsed: bool,
    pub left_dock_width: Option<f32>,
    pub right_dock_width: Option<f32>,
    /// Transient visible widths while a dock opens or closes. Normal layout
    /// keeps these at `None` and uses the persisted dock preferences.
    pub left_presentation_width: Option<f32>,
    pub right_presentation_width: Option<f32>,
    pub side_tab: SideTab,
    /// Pages are independent so switching tabs preserves the user's place.
    pub activity_page: usize,
    pub review_page: usize,
    pub dock_tabs: Vec<DockTabView>,
    pub dock_recently_closed: Vec<DockRecentlyClosedView>,
    pub dock_overview_query: String,
    pub show_dock_overview: bool,
    pub show_dock_add_menu: bool,
    pub file_preview: Option<FilePreviewView>,
    pub git_review: Option<Result<GitReview, String>>,
    pub review_detail: Option<ReviewDetailView>,
    pub terminal_snapshot: Option<TerminalSnapshot>,
    pub terminal_input: String,
    pub terminal_error: Option<String>,
    pub pending_permissions: Vec<PermissionView>,
    pub pending_subagent_permissions: Vec<SubagentPermissionView>,
    pub active_subagents: Vec<SubagentRunView>,
    pub recoverable_subagent: Option<SubagentRunView>,
    pub pending_tool_verification: Option<ToolVerificationView>,
    pub pending_batch_discard: Option<AbortedToolBatchView>,
    pub tool_verification_output: String,
    pub tool_verification_is_error: bool,
    pub selected_tool_id: Option<String>,
    pub show_jump_to_bottom: bool,
    pub github_account: GithubAccountStatus,
    pub github_account_menu_open: bool,
}

fn label(value: impl Into<String>, color: Color) -> Element {
    Element::text(value)
        .foreground(color)
        .font_family(FontFamily::Sans)
        .font_size(theme::UI_FONT_SIZE)
}

fn mono(value: impl Into<String>, color: Color) -> Element {
    label(value, color).font_family(FontFamily::Monospace)
}

fn eyebrow(value: impl Into<String>) -> Element {
    mono(value, theme::FOREGROUND_SUBTLE).font_size(10.0)
}

fn button(id: impl Into<String>, title: impl Into<String>, fill: Color, ink: Color) -> Element {
    Button::new(id, title, theme::action_palette(fill, ink))
        .variant(if fill == theme::BRAND || fill == theme::DESTRUCTIVE {
            ButtonVariant::Solid
        } else {
            ButtonVariant::Subtle
        })
        .size(ControlSize::Regular)
        .active(fill == theme::SELECTED)
        .render()
        .font_size(theme::UI_FONT_SIZE)
        .padding(5.0)
        .radius(8.0)
        .min_size(0.0, 30.0)
}

fn icon_button(
    id: impl Into<String>,
    icon: Icon,
    title: impl Into<String>,
    fill: Color,
    ink: Color,
) -> Element {
    let title = title.into();
    let icon_only = title.is_empty();
    let control = UiIconButton::new(id, icon, theme::action_palette(fill, ink))
        .label(title)
        .variant(if fill == theme::BRAND || fill == theme::DESTRUCTIVE {
            ButtonVariant::Solid
        } else {
            ButtonVariant::Subtle
        })
        .size(ControlSize::Regular)
        .active(fill == theme::SELECTED)
        .render()
        .font_size(theme::UI_FONT_SIZE);
    if icon_only {
        // The caller may request a 24/26/28 px toolbar target. A 30 px
        // minimum made its hover fill and hit region exceed the row, while a
        // text-button inset could leave the glyph visually off-center.
        control.padding(4.0).radius(7.0).min_size(0.0, 0.0)
    } else {
        control.padding(5.0).radius(8.0).min_size(0.0, 30.0)
    }
}

fn hinted_icon_button(
    id: impl Into<String>,
    icon: Icon,
    hint: &str,
    fill: Color,
    ink: Color,
    size: f32,
) -> Element {
    let id = id.into();
    Tooltip::new(
        icon_button(id.clone(), icon, "", fill, ink).size(Some(size), Some(size)),
        id,
        hint,
        theme::component_palette(),
    )
    .render()
}

fn hinted_shortcut_icon_button(
    id: impl Into<String>,
    icon: Icon,
    hint: &str,
    shortcut: &str,
    fill: Color,
    ink: Color,
    size: f32,
) -> Element {
    let id = id.into();
    Tooltip::new(
        icon_button(id.clone(), icon, "", fill, ink).size(Some(size), Some(size)),
        id,
        hint,
        theme::component_palette(),
    )
    .shortcut(shortcut)
    .render()
}

fn field(id: &str, value: &str, placeholder: &str) -> Element {
    Element::text_input(id, value, placeholder)
        .background(theme::INPUT)
        .foreground(theme::FOREGROUND)
        .font_family(FontFamily::Sans)
        .radius(8.0)
        .border(theme::BORDER, 1.0)
        .min_size(80.0, 32.0)
}

fn divider() -> Element {
    Element::spacer()
        .size(None, Some(1.0))
        .background(theme::BORDER)
}

fn shorten(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn workspace_name(workspace: &str) -> &str {
    Path::new(workspace)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Elegir proyecto")
}

fn workspace_hint(workspace: &str) -> String {
    let path = Path::new(workspace);
    let name = workspace_name(workspace);
    match path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
    {
        Some(parent) => format!("…/{}/{}", shorten(parent, 12), shorten(name, 14)),
        None => shorten(workspace, 28),
    }
}

fn backend_name(state: &ViewState) -> &str {
    state
        .backend_options
        .iter()
        .find(|(id, _)| id == &state.selected_backend)
        .map(|(_, title)| title.as_str())
        .unwrap_or("Sin agente")
}

fn session_title(state: &ViewState) -> &str {
    if state.selected_session.is_some() {
        if let Some(title) = state.selected_session_title.as_deref() {
            return title;
        }
    }
    state
        .selected_session
        .as_ref()
        .and_then(|id| state.sessions.iter().find(|session| &session.id == id))
        .map(|session| session.title.as_str())
        .unwrap_or("Nueva tarea")
}

fn sidebar_width(viewport: f32) -> f32 {
    if viewport >= 1200.0 {
        theme::SIDEBAR_WIDTH
    } else if viewport >= 800.0 {
        200.0
    } else {
        176.0
    }
}

fn side_width(viewport: f32, left: f32, left_visible: bool) -> f32 {
    if viewport < 1100.0 {
        MIN_RIGHT_DOCK
    } else {
        let chrome = (2.0 + f32::from(left_visible)) * GAP + RIGHT_DIVIDER;
        ((viewport - left - chrome) * 0.45).clamp(MIN_RIGHT_DOCK, 480.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DockLayout {
    pub left: f32,
    pub right: Option<f32>,
    pub center: f32,
    pub left_split_x: Option<f32>,
    pub right_split_x: Option<f32>,
}

pub fn dock_layout(state: &ViewState, viewport: f32) -> DockLayout {
    let left_visible = !(state.show_settings && viewport < 760.0)
        && state
            .left_presentation_width
            .map_or(!state.sidebar_collapsed, |width| width > 1.0);
    let center_min_with_right = if state.side_tab == SideTab::Files {
        MIN_CENTER_COMPACT
    } else {
        MIN_CENTER_WITH_RIGHT
    };
    let desired_left = state
        .left_dock_width
        .unwrap_or_else(|| sidebar_width(viewport));
    // The right dock returns at its previous width when enough space exists.
    // Until then, keep the transcript and composer at a useful width.
    let left_for_threshold = state
        .left_presentation_width
        .unwrap_or(if left_visible { 200.0 } else { 0.0 })
        .max(0.0);
    let right_for_threshold = state
        .right_presentation_width
        .unwrap_or(MIN_RIGHT_DOCK)
        .max(0.0);
    let side_threshold = (2.0 + f32::from(left_visible)) * GAP
        + RIGHT_DIVIDER
        + left_for_threshold
        + right_for_threshold
        + center_min_with_right;
    let side_requested = state
        .right_presentation_width
        .map_or(state.show_side_pane, |width| width > 1.0);
    let side_visible = side_requested
        && !state.show_settings
        && state.main_page == MainPage::Workspace
        && viewport >= side_threshold;
    let gaps =
        (2.0 + f32::from(left_visible)) * GAP + if side_visible { RIGHT_DIVIDER } else { 0.0 };
    let left_max = if side_visible {
        (viewport - gaps - right_for_threshold - center_min_with_right).clamp(0.0, 320.0)
    } else {
        (viewport - gaps - MIN_CENTER_COMPACT).clamp(0.0, 320.0)
    };
    let left = if left_visible {
        if let Some(width) = state.left_presentation_width {
            width.clamp(0.0, left_max)
        } else {
            desired_left.clamp(
                (if side_visible { 200.0_f32 } else { 176.0_f32 }).min(left_max),
                left_max,
            )
        }
    } else {
        0.0
    };
    let right = side_visible.then(|| {
        let max_width = (viewport - gaps - left - center_min_with_right).max(0.0);
        if let Some(width) = state.right_presentation_width {
            width.clamp(0.0, max_width)
        } else {
            state
                .right_dock_width
                .unwrap_or_else(|| side_width(viewport, left, left_visible))
                .clamp(MIN_RIGHT_DOCK.min(max_width), max_width)
        }
    });
    let center = viewport - gaps - left - right.unwrap_or(0.0);
    DockLayout {
        left,
        right,
        center,
        left_split_x: left_visible.then_some(GAP + left + GAP / 2.0),
        right_split_x: right.map(|width| viewport - GAP - width - RIGHT_DIVIDER / 2.0),
    }
}

fn session_menu_action(
    id: String,
    title: &str,
    icon: Icon,
    destructive: bool,
) -> DropdownMenuEntry {
    let action = DropdownMenuItem::new(id, title).icon(icon);
    if destructive {
        action.destructive(theme::DESTRUCTIVE).into()
    } else {
        action.into()
    }
}

#[derive(Clone)]
struct SessionRowContext {
    selected_session: Option<String>,
    running: bool,
    session_menu_id: Option<String>,
    backend_options: Vec<(String, String)>,
    session_groups: SessionGroups,
    group_menu_id: Option<String>,
    session_plans: BTreeMap<String, SessionPlanView>,
    session_recovery: BTreeMap<String, SessionRecoveryView>,
}

impl From<&ViewState> for SessionRowContext {
    fn from(state: &ViewState) -> Self {
        Self {
            selected_session: state.selected_session.clone(),
            running: state.running,
            session_menu_id: state.session_menu_id.clone(),
            backend_options: state.backend_options.clone(),
            session_groups: state.session_groups.clone(),
            group_menu_id: state.group_menu_id.clone(),
            session_plans: state.session_plans.clone(),
            session_recovery: state.session_recovery.clone(),
        }
    }
}

#[cfg(test)]
fn session_row(state: &ViewState, session: &SessionView, width: f32) -> Element {
    session_row_with_context(&SessionRowContext::from(state), session, width)
}

fn session_row_with_context(
    state: &SessionRowContext,
    session: &SessionView,
    width: f32,
) -> Element {
    let selected = state.selected_session.as_deref() == Some(session.id.as_str());
    let menu_open = state.session_menu_id.as_deref() == Some(session.id.as_str());
    let backend = state
        .backend_options
        .iter()
        .find(|(id, _)| id == &session.backend)
        .map(|(_, name)| name.as_str())
        .unwrap_or(&session.backend)
        .split_whitespace()
        .next()
        .unwrap_or("CLI");
    let row_width = (width - 18.0).max(0.0);
    let menu_width = row_width.max(246.0);
    let recovery = state.session_recovery.get(&session.id);
    let status_marker = if recovery.is_some() {
        Element::icon(Icon::Refresh)
            .foreground(theme::BRAND)
            .size(Some(16.0), Some(32.0))
    } else if session.unread {
        Element::column(vec![
            Element::spacer().size(None, Some(13.0)),
            Element::row(vec![
                Element::spacer().size(Some(5.0), Some(6.0)),
                Element::spacer()
                    .size(Some(6.0), Some(6.0))
                    .background(theme::BRAND)
                    .radius(3.0),
            ]),
        ])
        .size(Some(16.0), Some(32.0))
    } else if selected && state.running {
        Element::icon(Icon::Clock)
            .foreground(theme::FOREGROUND)
            .size(Some(16.0), Some(32.0))
    } else if session.pinned {
        Element::icon(Icon::Pin)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(16.0), Some(32.0))
    } else {
        Element::spacer().size(Some(16.0), Some(32.0))
    };
    let mut actions = Vec::new();
    if state
        .session_plans
        .get(&session.id)
        .is_some_and(|plan| plan.can_build)
    {
        actions.push(session_menu_action(
            format!("session_build_plan:{}", session.id),
            "Construir este plan",
            Icon::ArrowRight,
            false,
        ));
        actions.push(DropdownMenuEntry::Separator);
    }
    if let Some(recovery) = recovery.filter(|recovery| recovery.can_open()) {
        actions.push(session_menu_action(
            format!("session_recover:{}", session.id),
            recovery.label(),
            Icon::Refresh,
            false,
        ));
        actions.push(DropdownMenuEntry::Separator);
    }
    actions.push(session_menu_action(
        format!("session_rename:{}", session.id),
        "Renombrar",
        Icon::Edit,
        false,
    ));
    if session.archived {
        actions.push(session_menu_action(
            format!("session_restore:{}", session.id),
            "Restaurar",
            Icon::ArchiveUp,
            false,
        ));
        actions.push(DropdownMenuEntry::Separator);
        actions.push(session_menu_action(
            format!("session_delete:{}", session.id),
            "Eliminar…",
            Icon::Trash,
            true,
        ));
    } else {
        actions.push(session_menu_action(
            format!("session_pin:{}", session.id),
            if session.pinned { "Desfijar" } else { "Fijar" },
            Icon::Pin,
            false,
        ));
        if !session.unread {
            actions.push(session_menu_action(
                format!("session_mark_unread:{}", session.id),
                "Marcar no leído",
                Icon::Unread,
                false,
            ));
        }
        actions.push(session_menu_action(
            format!("session_archive:{}", session.id),
            "Archivar",
            Icon::Archive,
            false,
        ));
        if !state.session_groups.groups.is_empty() {
            actions.push(DropdownMenuEntry::Separator);
            if state.session_groups.group_for(&session.id).is_some() {
                actions.push(session_menu_action(
                    format!("session_group_unassign:{}", session.id),
                    "Quitar del grupo",
                    Icon::Hash,
                    false,
                ));
            }
            for group in &state.session_groups.groups {
                actions.push(session_menu_action(
                    format!("session_group_assign:{}:{}", group.id, session.id),
                    &format!("Mover a {}", shorten(&group.title, 20)),
                    Icon::Hash,
                    false,
                ));
            }
        }
    }
    // The menu owns its popover. The parent row remains one full-width action,
    // while its last child has a more specific hit region and takes precedence.
    let menu_action_id = format!("session_menu:{}", session.id);
    let menu_button = UiIconButton::new(
        menu_action_id.clone(),
        Icon::More,
        theme::action_palette(Color::TRANSPARENT, theme::FOREGROUND_SUBTLE),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Regular)
    .render()
    .background(Color::TRANSPARENT)
    .hover_background(Color::TRANSPARENT)
    .pressed_background(Color::TRANSPARENT)
    .transition_ms(0)
    .padding(8.0)
    .min_size(0.0, 0.0)
    .size(Some(32.0), Some(32.0));
    let menu_button = if menu_open {
        menu_button
    } else {
        Tooltip::new(
            menu_button,
            menu_action_id.clone(),
            "Más acciones",
            theme::component_palette(),
        )
        .render()
    };
    let menu_button = DropdownMenu::new(
        menu_button,
        actions,
        menu_open,
        "session_menu_dismiss",
        theme::menu_palette(),
    )
    .placement(PopoverPlacement::BottomEnd)
    .width(menu_width)
    .render();
    Element::clickable(
        format!("session:{}", session.id),
        Element::row(vec![
            status_marker,
            Element::truncated_text(&session.title)
                .foreground(if selected || session.unread {
                    theme::BRAND
                } else {
                    theme::FOREGROUND
                })
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .min_size(0.0, 0.0)
                .size(None, Some(32.0))
                .flex(1.0),
            Element::panel(
                Element::truncated_text(backend)
                    .foreground(if selected {
                        theme::FOREGROUND
                    } else {
                        theme::FOREGROUND_SUBTLE
                    })
                    .font_family(FontFamily::Sans)
                    .font_size(10.0),
            )
            .padding(4.0)
            .size(Some(44.0), Some(32.0)),
            menu_button,
        ])
        .gap(2.0)
        .size(Some(row_width), Some(32.0)),
    )
    .size(Some(row_width), Some(32.0))
    .background(if selected || menu_open {
        theme::SELECTED
    } else {
        theme::SIDEBAR
    })
    .hover_background(if selected || menu_open {
        theme::hover_for(theme::SELECTED)
    } else {
        theme::hover_for(theme::SIDEBAR)
    })
    .pressed_background(theme::SELECTED)
    .hover_with(menu_action_id)
    .radius(8.0)
}

fn project_header(project: &str, count: usize, collapsed: bool, width: f32) -> Element {
    let row_width = (width - 18.0).max(0.0);
    let action_id = format!("session_project_toggle:{project}");
    let trailing = Element::row(vec![
        Element::truncated_text(count.to_string())
            .foreground(theme::FOREGROUND_SUBTLE)
            .font_family(FontFamily::Sans)
            .font_size(10.0),
        Element::panel(
            Element::icon(if collapsed {
                Icon::ChevronRight
            } else {
                Icon::ChevronDown
            })
            .foreground(theme::FOREGROUND_SUBTLE),
        )
        .size(Some(ICON_SIZE), Some(32.0)),
    ])
    .gap(6.0)
    .size(None, Some(32.0));
    TreeRow::new(
        move |content| Element::clickable(action_id, content),
        Element::icon(Icon::Folder).foreground(theme::FOREGROUND_SUBTLE),
        Element::truncated_text(workspace_name(project))
            .foreground(theme::FOREGROUND)
            .font_family(FontFamily::Sans)
            .font_size(12.0),
        TreeGuides::root(),
        theme::component_palette(),
    )
    .trailing(trailing)
    .width(row_width)
    .height(32.0)
    .render()
}

fn group_header(
    state: &SessionRowContext,
    group: &SessionGroup,
    index: usize,
    count: usize,
    width: f32,
) -> Element {
    let row_width = (width - 18.0).max(0.0);
    let menu_open = state.group_menu_id.as_deref() == Some(group.id.as_str());
    let mut menu_actions = vec![session_menu_action(
        format!("session_group_rename:{}", group.id),
        "Renombrar",
        Icon::Edit,
        false,
    )];
    if index > 0 {
        menu_actions.push(session_menu_action(
            format!("session_group_move_up:{}", group.id),
            "Mover arriba",
            Icon::ArrowUp,
            false,
        ));
    }
    if index + 1 < state.session_groups.groups.len() {
        menu_actions.push(session_menu_action(
            format!("session_group_move_down:{}", group.id),
            "Mover abajo",
            Icon::ArrowDown,
            false,
        ));
    }
    menu_actions.push(DropdownMenuEntry::Separator);
    menu_actions.push(session_menu_action(
        format!("session_group_delete:{}", group.id),
        "Eliminar grupo…",
        Icon::Trash,
        true,
    ));
    let more_anchor = UiIconButton::new(
        format!("session_group_menu:{}", group.id),
        Icon::More,
        theme::component_palette(),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Compact)
    .width(28.0)
    .active(menu_open)
    .render();
    let more_anchor = if menu_open {
        more_anchor
    } else {
        Tooltip::new(
            more_anchor,
            format!("session_group_menu:{}", group.id),
            "Opciones del grupo",
            theme::component_palette(),
        )
        .render()
    };
    let more = DropdownMenu::new(
        more_anchor,
        menu_actions,
        menu_open,
        "session_group_menu_dismiss",
        theme::menu_palette(),
    )
    .placement(PopoverPlacement::BottomEnd)
    .width(202.0)
    .render();
    Element::clickable(
        format!("session_group_toggle:{}", group.id),
        Element::row(vec![
            Element::icon(Icon::Hash)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(16.0), Some(16.0)),
            Element::truncated_text(&group.title)
                .foreground(theme::FOREGROUND)
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .flex(1.0),
            label(count.to_string(), theme::FOREGROUND_SUBTLE).font_size(10.0),
            Element::icon(if group.collapsed {
                Icon::ChevronRight
            } else {
                Icon::ChevronDown
            })
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(16.0), Some(16.0)),
            more,
        ])
        .gap(4.0)
        .padding(3.0)
        .size(Some(row_width), Some(32.0)),
    )
    .size(Some(row_width), Some(32.0))
    .background(theme::SIDEBAR)
    .hover_background(theme::hover_for(theme::SIDEBAR))
    .pressed_background(theme::SELECTED)
    .radius(7.0)
}

fn sidebar_nav_action(
    id: &str,
    icon: Icon,
    title: &str,
    shortcut: &str,
    width: f32,
    selected: bool,
) -> Element {
    Element::clickable(
        id,
        Element::row(vec![
            Element::icon(icon)
                .foreground(if selected {
                    theme::FOREGROUND
                } else {
                    theme::FOREGROUND_SUBTLE
                })
                .size(Some(16.0), Some(16.0)),
            label(title, theme::FOREGROUND).font_size(13.0),
            Element::spacer().flex(1.0),
            Kbd::new(shortcut, theme::component_palette()).render(),
        ])
        .gap(10.0)
        .padding(8.0)
        .size(Some(width), Some(32.0)),
    )
    .size(Some(width), Some(32.0))
    .background(if selected {
        theme::SELECTED
    } else {
        theme::SIDEBAR
    })
    .hover_background(if selected {
        theme::SELECTED
    } else {
        theme::hover_for(theme::SIDEBAR)
    })
    .pressed_background(theme::SELECTED)
    .radius(8.0)
}

#[derive(Clone)]
enum SessionListRow {
    Section(&'static str),
    Warning(String),
    Empty(&'static str),
    Project {
        path: String,
        count: usize,
        collapsed: bool,
    },
    Group {
        group: SessionGroup,
        index: usize,
        count: usize,
    },
    Session(SessionView),
}

fn session_list_rows(state: &ViewState) -> Vec<SessionListRow> {
    let mut rows = Vec::new();
    let pinned = state
        .sessions
        .iter()
        .filter(|session| session.pinned && !session.archived);
    let mut has_pinned = false;
    for session in pinned {
        if !has_pinned {
            rows.push(SessionListRow::Section("FIJADAS"));
            has_pinned = true;
        }
        rows.push(SessionListRow::Session(session.clone()));
    }
    rows.push(SessionListRow::Section(if state.show_archived {
        "ARCHIVADAS"
    } else if state.session_organization == SessionOrganization::Group
        && !state.session_groups.groups.is_empty()
    {
        "GRUPOS"
    } else {
        "TAREAS"
    }));
    if let Some(warning) = &state.sessions_status {
        rows.push(SessionListRow::Warning(warning.clone()));
    }
    let visible: Vec<_> = state
        .sessions
        .iter()
        .filter(|session| {
            if state.show_archived {
                session.archived
            } else {
                !session.archived && !session.pinned
            }
        })
        .collect();
    if state.session_organization == SessionOrganization::Group && !state.show_archived {
        let group_ids: BTreeSet<&str> = state
            .session_groups
            .groups
            .iter()
            .map(|group| group.id.as_str())
            .collect();
        let mut grouped: std::collections::BTreeMap<&str, Vec<&SessionView>> =
            std::collections::BTreeMap::new();
        let mut ungrouped = Vec::new();
        for session in &visible {
            match state
                .session_groups
                .members
                .get(&session.id)
                .map(String::as_str)
                .filter(|id| group_ids.contains(*id))
            {
                Some(id) => grouped.entry(id).or_default().push(*session),
                None => ungrouped.push(*session),
            }
        }
        for (index, group) in state.session_groups.groups.iter().enumerate() {
            let members = grouped.remove(group.id.as_str()).unwrap_or_default();
            rows.push(SessionListRow::Group {
                group: group.clone(),
                index,
                count: members.len(),
            });
            if !group.collapsed {
                rows.extend(members.into_iter().cloned().map(SessionListRow::Session));
            }
        }
        if !ungrouped.is_empty() && !state.session_groups.groups.is_empty() {
            rows.push(SessionListRow::Section("SIN GRUPO"));
        }
        rows.extend(ungrouped.into_iter().cloned().map(SessionListRow::Session));
        if visible.is_empty() && state.session_groups.groups.is_empty() {
            rows.push(SessionListRow::Empty("Crea un grupo o una tarea"));
        }
    } else if visible.is_empty() {
        rows.push(SessionListRow::Empty(if state.show_archived {
            "Archivo vacío"
        } else {
            "Todavía no hay tareas"
        }));
    } else if state.show_archived || state.session_organization == SessionOrganization::Recent {
        rows.extend(visible.into_iter().cloned().map(SessionListRow::Session));
    } else {
        let mut index = 0;
        while index < visible.len() {
            let path = visible[index].project.as_str();
            let end = visible[index..]
                .iter()
                .position(|session| session.project != path)
                .map_or(visible.len(), |offset| index + offset);
            let collapsed = state.collapsed_projects.contains(path);
            rows.push(SessionListRow::Project {
                path: path.to_owned(),
                count: end - index,
                collapsed,
            });
            if !collapsed {
                rows.extend(
                    visible[index..end]
                        .iter()
                        .cloned()
                        .cloned()
                        .map(SessionListRow::Session),
                );
            }
            index = end;
        }
    }
    rows
}

fn sessions_virtual_list(state: &ViewState, width: f32) -> Element {
    let rows = Arc::new(session_list_rows(state));
    let context = Arc::new(SessionRowContext::from(state));
    let count = rows.len();
    // Keep a small, stable end inset so the overlay scrollbar never covers
    // the menu actions on project and session rows.
    let item_width = (width - 8.0).max(0.0);
    let row_width = (width - 26.0).max(0.0);
    ScrollArea::uniform_list("sessions_list", count, 32.0, move |index| {
        match &rows[index] {
            SessionListRow::Section(title) => Element::panel(eyebrow(*title))
                .padding(9.0)
                .size(Some(row_width), Some(32.0)),
            SessionListRow::Warning(message) => Element::panel(
                Element::truncated_text(message)
                    .foreground(theme::DESTRUCTIVE)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0),
            )
            .padding(8.0)
            .size(Some(row_width), Some(32.0)),
            SessionListRow::Empty(message) => {
                Element::panel(label(*message, theme::FOREGROUND_SUBTLE))
                    .padding(8.0)
                    .size(Some(row_width), Some(32.0))
            }
            SessionListRow::Project {
                path,
                count,
                collapsed,
            } => project_header(path, *count, *collapsed, item_width),
            SessionListRow::Group {
                group,
                index,
                count,
            } => group_header(&context, group, *index, *count, item_width),
            SessionListRow::Session(session) => {
                session_row_with_context(&context, session, item_width)
            }
        }
    })
    .tint(theme::FOREGROUND_SUBTLE)
    .render()
    .padding(8.0)
    .flex(1.0)
}

pub(crate) fn sidebar_account_footer(state: &ViewState, width: f32) -> Element {
    let account_width = (width - if state.show_settings { 16.0 } else { 58.0 }).max(80.0);
    let account = match &state.github_account {
        GithubAccountStatus::Connected(identity) => {
            let initial = identity
                .login
                .chars()
                .next()
                .unwrap_or('G')
                .to_ascii_uppercase()
                .to_string();
            let badge = Element::panel(
                label(initial, theme::FOREGROUND)
                    .font_size(12.0)
                    .font_family(FontFamily::Sans)
                    .foreground(theme::FOREGROUND)
                    .size(Some(12.0), Some(12.0)),
            )
            .padding(8.0)
            .size(Some(28.0), Some(28.0))
            .background(theme::SELECTED)
            .radius(14.0);
            let anchor = Element::clickable(
                "github_account_open",
                Element::row(vec![
                    badge,
                    Element::truncated_text(&identity.login)
                        .font_family(FontFamily::Sans)
                        .font_size(13.0)
                        .foreground(theme::FOREGROUND)
                        .flex(1.0),
                    Element::panel(
                        Element::icon(Icon::Github).foreground(theme::FOREGROUND_SUBTLE),
                    )
                    .size(Some(28.0), Some(28.0)),
                ])
                .gap(8.0)
                .padding(4.0)
                .size(Some(account_width), Some(36.0)),
            )
            .size(Some(account_width), Some(36.0))
            .background(theme::SIDEBAR)
            .hover_background(theme::hover_for(theme::SIDEBAR))
            .pressed_background(theme::SELECTED)
            .transition_ms(120)
            .radius(9.0);
            Tooltip::new(
                anchor,
                "github_account_open",
                format!("Abrir perfil de {} en GitHub", identity.login),
                theme::component_palette(),
            )
            .render()
        }
        GithubAccountStatus::Unverified { accounts, notice } => {
            let many = accounts.len() > 1;
            let reconnect = !many && notice.is_some();
            let action_id = if many {
                "github_account_menu_toggle".to_owned()
            } else if reconnect {
                "github_account_connect".to_owned()
            } else {
                format!("github_account_verify:{}", accounts[0])
            };
            let title = if many {
                if width < 230.0 {
                    "Elegir cuenta"
                } else {
                    "Elegir cuenta GitHub"
                }
            } else if reconnect {
                if width < 230.0 {
                    "Reconectar"
                } else {
                    "Reconectar GitHub"
                }
            } else if width < 230.0 {
                "Verificar"
            } else {
                "Verificar GitHub"
            };
            let mut button = Button::new(
                action_id.clone(),
                title,
                theme::action_palette(theme::SIDEBAR, theme::FOREGROUND),
            )
            .icon(Icon::Github)
            .variant(ButtonVariant::Subtle)
            .size(ControlSize::Spacious)
            .width(account_width);
            if many {
                button = button.trailing_icon(Icon::ChevronDown);
            }
            let hint = notice.as_deref().unwrap_or(if many {
                "Hay varias cuentas guardadas. Elige cuál verificar."
            } else {
                "Cuenta guardada sin verificar. Comprueba la conexión."
            });
            let anchor = Tooltip::new(
                button.render().padding(8.0).gap(10.0),
                action_id,
                hint,
                theme::component_palette(),
            )
            .render();
            if many {
                let mut options: Vec<DropdownOption> = accounts
                    .iter()
                    .map(|account| {
                        DropdownOption::new(
                            format!("github_account_verify:{account}"),
                            format!("@{account}"),
                        )
                        .icon(Icon::Github)
                    })
                    .collect();
                options.push(
                    DropdownOption::new("github_account_connect", "Reconectar con GitHub")
                        .icon(Icon::Refresh),
                );
                DropdownList::new(
                    anchor,
                    options,
                    state.github_account_menu_open,
                    "github_account_menu_dismiss",
                    theme::menu_palette(),
                )
                .placement(PopoverPlacement::TopStart)
                .width(232.0)
                .render()
            } else {
                anchor
            }
        }
        status => {
            let (title, hint, disabled) = match status {
                GithubAccountStatus::Checking => {
                    ("Buscando GitHub…", "Buscando una cuenta de GitHub", true)
                }
                GithubAccountStatus::Connecting => (
                    "Conectando…",
                    "Completa el inicio de sesión en el navegador",
                    true,
                ),
                GithubAccountStatus::Verifying(_) => {
                    ("Verificando…", "Validando la cuenta con GitHub", true)
                }
                GithubAccountStatus::Unavailable(reason) => {
                    ("Conectar con GitHub", reason.as_str(), false)
                }
                GithubAccountStatus::Disconnected => {
                    ("Conectar con GitHub", "Iniciar sesión con GitHub", false)
                }
                GithubAccountStatus::Connected(_) => unreachable!(),
                GithubAccountStatus::Unverified { .. } => unreachable!(),
            };
            let title = if width < 230.0 && !disabled {
                "Conectar"
            } else {
                title
            };
            let mut button = Button::new(
                "github_account_connect",
                title,
                theme::action_palette(theme::SIDEBAR, theme::FOREGROUND),
            )
            .icon(Icon::Github)
            .variant(ButtonVariant::Subtle)
            .size(ControlSize::Spacious)
            .width(account_width);
            if disabled {
                button = button.disabled();
            }
            Tooltip::new(
                button.render().padding(8.0).gap(10.0),
                "github_account_connect",
                hint,
                theme::component_palette(),
            )
            .render()
        }
    };
    let settings = Tooltip::new(
        UiIconButton::new(
            "toggle_settings",
            Icon::Settings,
            theme::action_palette(theme::SIDEBAR, theme::FOREGROUND_SUBTLE),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Spacious)
        .render(),
        "toggle_settings",
        "Configuración",
        theme::component_palette(),
    )
    .render();
    let mut footer = vec![account.flex(1.0)];
    if !state.show_settings {
        footer.push(settings);
    }
    Element::panel(Element::row(footer).gap(6.0))
        .padding(8.0)
        .size(None, Some(52.0))
}

fn sidebar(state: &ViewState, width: f32) -> Element {
    let header = Element::panel(
        Element::row(vec![
            Tooltip::new(
                icon_button(
                    "toggle_sidebar",
                    Icon::PanelLeftClose,
                    "",
                    theme::SIDEBAR,
                    theme::FOREGROUND,
                )
                .size(Some(32.0), Some(32.0)),
                "toggle_sidebar",
                "Ocultar barra lateral",
                theme::component_palette(),
            )
            .shortcut("Ctrl+B")
            .placement(TooltipPlacement::Below)
            .render(),
            // TruncatedText vertically centers its line inside the 32 px
            // header row; Text paints at the top and sat above the toggle.
            Element::truncated_text("OpenIDE")
                .foreground(theme::FOREGROUND)
                .font_family(FontFamily::Sans)
                .font_size(14.0),
            Element::spacer().flex(1.0),
        ])
        .gap(2.0),
    )
    .padding(8.0)
    .size(None, Some(HEADER));
    let primary_nav = Element::column(vec![
        sidebar_nav_action(
            "new_session_sidebar",
            Icon::Add,
            "Nueva tarea",
            "Ctrl+N",
            width - 18.0,
            false,
        ),
        sidebar_nav_action(
            "open_command_center",
            Icon::Search,
            "Buscar",
            "Ctrl+K",
            width - 18.0,
            false,
        ),
        sidebar_nav_action(
            "open_automations",
            Icon::Clock,
            "Automatizaciones",
            "",
            width - 18.0,
            state.main_page == MainPage::Automations,
        ),
        sidebar_nav_action(
            "open_marketplace",
            Icon::Grid,
            "Marketplace",
            "",
            width - 18.0,
            state.main_page == MainPage::Marketplace,
        ),
    ])
    .gap(4.0)
    .padding(8.0);
    let project_paths: BTreeSet<_> = state
        .sessions
        .iter()
        .filter(|session| !session.archived && !session.pinned)
        .map(|session| session.project.as_str())
        .collect();
    let all_projects_collapsed = !project_paths.is_empty()
        && project_paths
            .iter()
            .all(|project| state.collapsed_projects.contains(*project));
    let all_groups_collapsed = !state.session_groups.groups.is_empty()
        && state
            .session_groups
            .groups
            .iter()
            .all(|group| group.collapsed);
    let sort_trigger = icon_button(
        "session_sort_toggle",
        Icon::Filter,
        "",
        if state.session_sort_menu_open || state.session_organization == SessionOrganization::Recent
        {
            theme::SELECTED
        } else {
            theme::SIDEBAR
        },
        theme::FOREGROUND_SUBTLE,
    )
    .size(Some(28.0), Some(28.0));
    let sort_trigger = if state.session_sort_menu_open {
        sort_trigger
    } else {
        Tooltip::new(
            sort_trigger,
            "session_sort_toggle",
            "Filtrar y ordenar tareas",
            theme::component_palette(),
        )
        .render()
    };
    let sort_control = DropdownList::new(
        sort_trigger,
        {
            let mut options = vec![
                DropdownOption::new("session_organize:project", "Por proyecto")
                    .icon(Icon::Folder)
                    .selected(state.session_organization == SessionOrganization::Project),
                DropdownOption::new("session_organize:recent", "Cronológica")
                    .icon(Icon::Clock)
                    .selected(state.session_organization == SessionOrganization::Recent),
                DropdownOption::new("session_sort:updated", "Actualizadas")
                    .icon(Icon::Sort)
                    .selected(state.session_sort == SessionSort::Updated),
                DropdownOption::new("session_sort:created", "Creadas")
                    .icon(Icon::History)
                    .selected(state.session_sort == SessionSort::Created),
            ];
            if state.session_organization == SessionOrganization::Group
                && !state.session_groups.groups.is_empty()
            {
                options.push(
                    DropdownOption::new(
                        "session_expand_all",
                        if all_groups_collapsed {
                            "Expandir grupos"
                        } else {
                            "Contraer grupos"
                        },
                    )
                    .icon(if all_groups_collapsed {
                        Icon::Expand
                    } else {
                        Icon::Collapse
                    }),
                );
            }
            options
        },
        state.session_sort_menu_open,
        "session_sort_dismiss",
        theme::menu_palette(),
    )
    .width(180.0)
    .placement(PopoverPlacement::BottomEnd)
    .render();
    let context_action = if state.session_organization == SessionOrganization::Group {
        Tooltip::new(
            icon_button(
                "session_group_new",
                Icon::Hash,
                "",
                if state.group_create_open {
                    theme::SELECTED
                } else {
                    theme::SIDEBAR
                },
                theme::FOREGROUND_SUBTLE,
            )
            .size(Some(28.0), Some(28.0)),
            "session_group_new",
            "Nuevo grupo",
            theme::component_palette(),
        )
        .render()
    } else {
        let mut expand = UiIconButton::new(
            "session_expand_all",
            if all_projects_collapsed {
                Icon::Expand
            } else {
                Icon::Collapse
            },
            theme::action_palette(theme::SIDEBAR, theme::FOREGROUND_SUBTLE),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Compact);
        if project_paths.is_empty() || state.session_organization == SessionOrganization::Recent {
            expand = expand.disabled();
        }
        Tooltip::new(
            expand.render().size(Some(28.0), Some(28.0)),
            "session_expand_all",
            if all_projects_collapsed {
                "Expandir proyectos"
            } else {
                "Contraer proyectos"
            },
            theme::component_palette(),
        )
        .render()
    };
    let archive_control = Tooltip::new(
        icon_button(
            "toggle_archived_view",
            if state.show_archived {
                Icon::Close
            } else {
                Icon::Archive
            },
            "",
            if state.show_archived {
                theme::SELECTED
            } else {
                theme::SIDEBAR
            },
            theme::FOREGROUND_SUBTLE,
        )
        .size(Some(28.0), Some(28.0)),
        "toggle_archived_view",
        if state.show_archived {
            "Cerrar archivo"
        } else {
            "Ver archivo"
        },
        theme::component_palette(),
    )
    .render();
    let mut toolbar = Vec::new();
    if !state.show_archived {
        let selector = SegmentedControl::new(
            vec![
                Tab::new("session_organize:group", "Grupo", 70.0)
                    .icon(Icon::Hash)
                    .active(state.session_organization == SessionOrganization::Group),
                Tab::new("session_organize:project", "Proyecto", 78.0)
                    .icon(Icon::Folder)
                    .active(state.session_organization != SessionOrganization::Group),
            ],
            theme::segmented_palette(),
        )
        .render();
        if width >= 256.0 {
            toolbar.push(
                Element::row(vec![
                    selector,
                    Element::spacer().flex(1.0),
                    context_action,
                    sort_control,
                    archive_control,
                ])
                .gap(2.0)
                .size(None, Some(28.0)),
            );
        } else if width >= 200.0 {
            toolbar.push(
                Element::row(vec![selector, Element::spacer().flex(1.0), archive_control])
                    .gap(2.0)
                    .size(None, Some(28.0)),
            );
            toolbar.push(
                Element::row(vec![
                    Element::spacer().flex(1.0),
                    context_action,
                    sort_control,
                ])
                .gap(2.0)
                .size(None, Some(28.0)),
            );
        } else {
            toolbar.push(Element::row(vec![selector]).size(None, Some(28.0)));
            toolbar.push(
                Element::row(vec![
                    Element::spacer().flex(1.0),
                    context_action,
                    sort_control,
                    archive_control,
                ])
                .gap(2.0)
                .size(None, Some(28.0)),
            );
        }
    } else {
        toolbar.push(
            Element::row(vec![
                label("Archivo", theme::FOREGROUND).font_size(12.0),
                Element::spacer().flex(1.0),
                sort_control,
                archive_control,
            ])
            .gap(2.0)
            .size(None, Some(28.0)),
        );
    }
    if state.group_create_open
        && state.session_organization == SessionOrganization::Group
        && !state.show_archived
    {
        toolbar.push(
            Element::row(vec![
                field(
                    "group_create_draft",
                    &state.group_create_draft,
                    "Nombre del grupo",
                )
                .font_size(12.0)
                .min_size(0.0, 0.0)
                .flex(1.0)
                .size(None, Some(28.0)),
                hinted_icon_button(
                    "session_group_create_commit",
                    Icon::Check,
                    "Crear grupo",
                    theme::SELECTED,
                    theme::FOREGROUND,
                    28.0,
                ),
                hinted_icon_button(
                    "session_group_create_cancel",
                    Icon::Close,
                    "Cancelar grupo",
                    theme::SIDEBAR,
                    theme::FOREGROUND_SUBTLE,
                    28.0,
                ),
            ])
            .gap(3.0)
            .size(None, Some(28.0)),
        );
    }
    let session_toolbar = Element::column(toolbar).gap(4.0).padding(8.0);
    let footer = sidebar_account_footer(state, width);
    Element::column(vec![
        header,
        divider(),
        primary_nav,
        session_toolbar,
        sessions_virtual_list(state, width),
        divider(),
        footer,
    ])
    .size(Some(width), None)
    .background(theme::SIDEBAR)
    .border(theme::BORDER, 1.0)
    .radius(6.0)
}

fn pane_empty_state(
    icon: Icon,
    title: impl Into<String>,
    description: impl Into<String>,
    action: Option<(&str, &str)>,
) -> Element {
    let mut palette = theme::component_palette();
    palette.surface = theme::PANEL;
    let mut empty = Empty::new(title, description, palette).icon(icon).compact();
    if let Some((id, title)) = action {
        empty = empty.action(id, title, ButtonVariant::Secondary);
    }
    empty.render()
}

fn pane_loading_state(id: &str, title: &str, width: f32) -> Element {
    let palette = theme::component_palette();
    let block_width = (width - 64.0).clamp(80.0, 250.0);
    Element::panel(
        Element::column(vec![
            label(title, theme::FOREGROUND_SUBTLE).font_size(12.0),
            Skeleton::block(format!("{id}_body"), palette, block_width, 125.0).render(),
            Skeleton::line(format!("{id}_line_1"), palette, block_width)
                .height(14.0)
                .render(),
            Skeleton::line(format!("{id}_line_2"), palette, block_width * 0.8)
                .height(14.0)
                .render(),
        ])
        .gap(8.0),
    )
    .padding(16.0)
    .background(theme::PANEL)
    .border(theme::BORDER, 1.0)
    .radius(10.0)
    .size(None, Some(250.0))
}

fn page_bounds(total: usize, requested_page: usize, page_size: usize) -> (usize, usize, usize) {
    if total == 0 {
        return (0, 0, 0);
    }
    let page = requested_page.min((total - 1) / page_size);
    let start = page * page_size;
    (page, start, (start + page_size).min(total))
}

fn pane_pagination(id: &str, total: usize, page: usize, start: usize, end: usize) -> Element {
    let previous = if page > 0 {
        hinted_icon_button(
            format!("{id}_previous"),
            Icon::ArrowLeft,
            "Página anterior",
            theme::BACKGROUND,
            theme::FOREGROUND,
            28.0,
        )
    } else {
        Element::spacer().size(Some(28.0), Some(28.0))
    };
    let next = if end < total {
        hinted_icon_button(
            format!("{id}_next"),
            Icon::ArrowRight,
            "Página siguiente",
            theme::BACKGROUND,
            theme::FOREGROUND,
            28.0,
        )
    } else {
        Element::spacer().size(Some(28.0), Some(28.0))
    };
    Element::row(vec![
        mono(
            format!("{}–{} de {total}", start + 1, end),
            theme::FOREGROUND_SUBTLE,
        )
        .font_size(11.0),
        Element::spacer().flex(1.0),
        previous,
        next,
    ])
    .gap(4.0)
    .padding(5.0)
    .background(theme::PANEL)
    .radius(7.0)
    .size(None, Some(34.0))
}

fn file_tree_content(state: &ViewState, width: f32) -> Element {
    let mut content = vec![Element::row(vec![
        eyebrow(format!(
            "ARCHIVOS · {}",
            shorten(workspace_name(&state.workspace), 20)
        )),
        Element::spacer().flex(1.0),
        Tooltip::new(
            icon_button(
                "files_refresh",
                Icon::Refresh,
                "",
                theme::BACKGROUND,
                theme::FOREGROUND_SUBTLE,
            )
            .size(Some(28.0), Some(28.0)),
            "files_refresh",
            "Actualizar archivos",
            theme::component_palette(),
        )
        .render(),
    ])
    .gap(6.0)
    .size(None, Some(32.0))];
    if state.file_rows.is_empty() {
        let failed = state.files_status.starts_with("No se pudo");
        content.push(pane_empty_state(
            if failed { Icon::Refresh } else { Icon::Folder },
            if failed {
                "No se pudieron cargar los archivos"
            } else {
                "Carpeta vacía"
            },
            if state.files_status.is_empty() {
                "Abre un proyecto para ver sus archivos aquí.".to_owned()
            } else {
                state.files_status.clone()
            },
            Some(("files_refresh", "Volver a cargar")),
        ));
    } else {
        if state.files_status.starts_with("Lista parcial") {
            content.push(label(&state.files_status, theme::FOREGROUND_SUBTLE).font_size(11.0));
        }
        let rows = Arc::new(state.file_rows.clone());
        let guides = Arc::new(guides_for_depths(
            &rows.iter().map(|entry| entry.depth).collect::<Vec<_>>(),
        ));
        let row_count = rows.len() + usize::from(state.file_root_can_load_more);
        content.push(
            ScrollArea::uniform_list("file_tree", row_count, 28.0, move |index| {
                if index == rows.len() {
                    return Button::new(
                        "file_more_root",
                        "Cargar más archivos…",
                        theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Compact)
                    .render()
                    .font_family(FontFamily::Sans)
                    .font_size(12.0)
                    .size(Some((width - 16.0).max(120.0)), Some(28.0));
                }
                let entry = &rows[index];
                let icon = Element::icon(if entry.is_dir {
                    Icon::Folder
                } else {
                    Icon::Code
                })
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(ICON_SIZE), Some(ICON_SIZE));
                let title = Element::truncated_text(&entry.name)
                    .font_family(FontFamily::Sans)
                    .font_size(12.0)
                    .foreground(theme::FOREGROUND);
                let mut row = TreeRow::new(
                    move |content| Element::clickable(format!("file_row:{index}"), content),
                    icon,
                    title,
                    guides[index].clone(),
                    theme::component_palette(),
                )
                .width((width - 16.0).max(120.0))
                .guide_color(theme::TREE_GUIDE);
                if entry.is_dir {
                    let chevron = Element::icon(if entry.expanded {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    })
                    .foreground(theme::FOREGROUND_SUBTLE)
                    .size(Some(ICON_SIZE), Some(ICON_SIZE));
                    row = row.trailing(if entry.expanded && entry.can_load_more {
                        Element::row(vec![
                            Button::new(
                                format!("file_more:{index}"),
                                "Más",
                                theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
                            )
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Compact)
                            .render()
                            .font_family(FontFamily::Sans)
                            .font_size(11.0)
                            .padding(3.0)
                            .radius(5.0)
                            .size(Some(36.0), Some(22.0)),
                            chevron,
                        ])
                        .gap(4.0)
                    } else {
                        chevron
                    });
                }
                row.render()
            })
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .flex(1.0),
        );
    }
    Element::column(content).gap(6.0).padding(8.0).flex(1.0)
}

fn tool_status(output: &Option<String>, is_error: bool) -> &'static str {
    if output.is_none() {
        "En curso"
    } else if is_error {
        "Error"
    } else {
        "Completada"
    }
}

fn json_text(value: &Value) -> String {
    if value.is_null() {
        "Sin argumentos".to_owned()
    } else {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    }
}

fn transcript_item(
    item: &TranscriptItem,
    item_index: usize,
    state: &ViewState,
    side_visible: bool,
) -> Element {
    match item {
        TranscriptItem::User { content, images } => {
            let mut contents = vec![
                eyebrow("TÚ").foreground(theme::USER),
                label(content, theme::FOREGROUND),
            ];
            for image in images {
                contents.push(crate::session_surface::user_image(image));
            }
            Element::panel(Element::column(contents).gap(8.0))
                .padding(14.0)
                .background(theme::CARD)
                .border(theme::BORDER, 1.0)
                .radius(12.0)
        }
        TranscriptItem::Assistant { content, streaming } => Element::panel(
            Element::column(vec![
                eyebrow(if *streaming {
                    "AGENTE · ESCRIBIENDO"
                } else {
                    "AGENTE"
                })
                .foreground(theme::ASSISTANT),
                label(content, theme::FOREGROUND),
            ])
            .gap(8.0),
        )
        .padding(14.0),
        TranscriptItem::WebCitation { title, url } => Element::clickable(
            format!("web_citation_open:{item_index}"),
            Element::column(vec![
                eyebrow("FUENTE WEB").foreground(theme::BRAND),
                label(shorten(title, 100), theme::FOREGROUND),
                mono(shorten(url, 140), theme::FOREGROUND_SUBTLE).font_size(11.0),
            ])
            .gap(5.0)
            .padding(10.0),
        )
        .background(theme::PANEL)
        .hover_background(theme::HOVER)
        .border(theme::BORDER, 1.0)
        .radius(10.0),
        TranscriptItem::Tool {
            id,
            name,
            arguments,
            output,
            is_error,
        } => {
            let selected = state.selected_tool_id.as_deref() == Some(id.as_str());
            let ink = if *is_error {
                theme::DESTRUCTIVE
            } else {
                theme::TOOL
            };
            let mut rows = vec![icon_button(
                format!("tool:{id}"),
                Icon::Code,
                format!(
                    "{} · {}",
                    shorten(if name.is_empty() { "Herramienta" } else { name }, 24),
                    tool_status(output, *is_error)
                ),
                if selected {
                    theme::SELECTED
                } else {
                    theme::PANEL
                },
                ink,
            )
            .size(None, Some(34.0))];
            // On a narrow surface, the selected tool expands in place.
            if !side_visible && selected {
                rows.push(mono(json_text(arguments), theme::FOREGROUND_SUBTLE).font_size(12.0));
                if let Some(output) = output {
                    rows.push(
                        mono(
                            output,
                            if *is_error {
                                theme::DESTRUCTIVE
                            } else {
                                theme::FOREGROUND
                            },
                        )
                        .font_size(12.0),
                    );
                }
            } else if selected {
                rows.push(
                    label("Detalles en Actividad →", theme::FOREGROUND_SUBTLE).font_size(12.0),
                );
            }
            Element::panel(Element::column(rows).gap(6.0))
                .padding(8.0)
                .background(theme::PANEL)
                .border(theme::BORDER, 1.0)
                .radius(10.0)
        }
        TranscriptItem::ProcessOutput { stream, text } => {
            let (title, ink) = match stream {
                OutputStream::Stdout => ("SALIDA DEL PROCESO", theme::TOOL_RESULT),
                OutputStream::Stderr => ("DIAGNÓSTICOS DEL PROCESO", theme::FOREGROUND_SUBTLE),
            };
            Element::panel(
                Element::column(vec![
                    eyebrow(title).foreground(ink),
                    mono(
                        if side_visible {
                            shorten(text, 1000)
                        } else {
                            text.clone()
                        },
                        theme::FOREGROUND_SUBTLE,
                    )
                    .font_size(12.0),
                ])
                .gap(6.0),
            )
            .padding(12.0)
            .background(theme::PANEL)
            .border(theme::BORDER, 1.0)
            .radius(10.0)
        }
        TranscriptItem::Notice { message, is_error } => Element::panel(
            Element::column(vec![
                eyebrow(if *is_error { "ERROR" } else { "AVISO" }).foreground(if *is_error {
                    theme::DESTRUCTIVE
                } else {
                    theme::FOREGROUND_SUBTLE
                }),
                label(
                    message,
                    if *is_error {
                        theme::DESTRUCTIVE
                    } else {
                        theme::FOREGROUND
                    },
                ),
            ])
            .gap(6.0),
        )
        .padding(12.0)
        .background(theme::PANEL)
        .border(theme::BORDER, 1.0)
        .radius(10.0),
    }
}

fn timeline(state: &ViewState, side_visible: bool, compact: bool) -> Element {
    let mut items = Vec::new();
    if state.transcript.is_empty() {
        items.extend([
            Element::spacer().size(None, Some(if compact { 20.0 } else { 48.0 })),
            label("Nueva tarea", theme::BRAND).font_size(18.0),
            label(
                "Describe lo que quieres hacer. El agente trabajará en este proyecto.",
                theme::FOREGROUND_SUBTLE,
            ),
        ]);
    }
    for (index, item) in state.transcript.iter().enumerate() {
        items.push(transcript_item(item, index, state, side_visible));
    }
    ScrollArea::new(
        "transcript",
        Element::column(items)
            .gap(if compact { 10.0 } else { 14.0 })
            .padding(if compact { 12.0 } else { 20.0 }),
    )
    .tint(theme::FOREGROUND_SUBTLE)
    .render()
    .flex(1.0)
    .background(theme::BACKGROUND)
}

fn composer(state: &ViewState, center_width: f32) -> Element {
    let compact = center_width < 470.0;
    let card_width = (center_width - if compact { 20.0 } else { 32.0 }).clamp(0.0, 1120.0);
    let card = build_composer(&ComposerProps::from_state(
        state,
        card_width,
        false,
        None,
        "Ask me anything",
        Some(&state.status),
    ));
    Element::row(vec![
        Element::spacer().flex(1.0),
        card,
        Element::spacer().flex(1.0),
    ])
    .padding(10.0)
}

fn permission_card(permission: &PermissionView, compact: bool) -> Element {
    Element::panel(
        Element::column(vec![
            eyebrow("PERMISO REQUERIDO"),
            label(
                format!("El agente quiere usar {}", permission.tool_name),
                theme::FOREGROUND,
            )
            .font_size(13.0),
            ScrollArea::new(
                format!("permission_args:{}", permission.request_id),
                mono(&permission.arguments, theme::FOREGROUND_SUBTLE)
                    .font_size(11.0)
                    .padding(6.0),
            )
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .size(None, Some(if compact { 64.0 } else { 86.0 }))
            .background(theme::PANEL)
            .border(theme::BORDER, 1.0)
            .radius(6.0),
            Element::row(vec![
                button(
                    format!("permission_deny:{}", permission.request_id),
                    "Denegar",
                    theme::HOVER,
                    theme::FOREGROUND,
                )
                .size(Some(90.0), Some(30.0)),
                button(
                    format!("permission_allow:{}", permission.request_id),
                    "Permitir una vez",
                    theme::BRAND,
                    theme::PRIMARY_FOREGROUND,
                )
                .size(Some(140.0), Some(30.0)),
            ])
            .gap(6.0),
        ])
        .gap(6.0),
    )
    .padding(10.0)
    .background(theme::CARD)
    .border(theme::BORDER, 1.0)
    .radius(10.0)
}

fn workspace_header(state: &ViewState, available: bool, visible: bool, width: f32) -> Element {
    let sidebar_hidden = state
        .left_presentation_width
        .map_or(state.sidebar_collapsed, |width| width <= 1.0);
    let reopen_width = if sidebar_hidden { 40.0 } else { 0.0 };
    let title_max = ((width - reopen_width - if available && !visible { 180.0 } else { 115.0 })
        / 7.0)
        .floor()
        .clamp(12.0, 48.0) as usize;
    let mut actions = Vec::new();
    if sidebar_hidden {
        actions.push(sidebar_reopen_control(theme::PANEL));
    }
    actions.extend([
        label(shorten(session_title(state), title_max), theme::FOREGROUND),
        Element::spacer().flex(1.0),
    ]);
    if available && !visible {
        actions.push(
            icon_button(
                "toggle_side_pane",
                Icon::PanelRightOpen,
                "Panel",
                theme::HOVER,
                theme::FOREGROUND,
            )
            .size(Some(74.0), Some(32.0)),
        );
    }
    actions.push(
        icon_button(
            "toggle_settings",
            if state.show_settings {
                Icon::Close
            } else {
                Icon::Settings
            },
            if state.show_settings {
                "Volver"
            } else {
                "Opciones"
            },
            theme::HOVER,
            theme::FOREGROUND,
        )
        .size(Some(96.0), Some(32.0)),
    );
    Element::column(vec![
        Element::panel(Element::row(actions).gap(6.0))
            .padding(8.0)
            .background(theme::PANEL)
            .size(None, Some(HEADER - 1.0)),
        divider(),
    ])
    .size(None, Some(HEADER))
}

/// The same action is used by every center surface. A collapsed sidebar
/// never inserts a separate row above the center dock.
pub(super) fn sidebar_reopen_control(background: Color) -> Element {
    Tooltip::new(
        icon_button(
            "toggle_sidebar_reopen",
            Icon::PanelLeftOpen,
            "",
            background,
            theme::FOREGROUND,
        )
        .size(Some(32.0), Some(32.0)),
        "toggle_sidebar_reopen",
        "Mostrar barra lateral",
        theme::component_palette(),
    )
    .shortcut("Ctrl+B")
    .placement(TooltipPlacement::Below)
    .render()
}

fn context_content(state: &ViewState) -> Element {
    if state.selected_session.is_none() && state.transcript.is_empty() {
        return Element::column(vec![
            pane_empty_state(
                Icon::Chat,
                "Sin tarea activa",
                "Inicia una tarea para ver aquí su contexto y el trabajo del agente.",
                Some(("new_session_header", "Nueva tarea")),
            ),
            Element::spacer().size(None, Some(12.0)),
            eyebrow("PROYECTO ACTUAL"),
            label(workspace_name(&state.workspace), theme::FOREGROUND),
            mono(workspace_hint(&state.workspace), theme::FOREGROUND_SUBTLE).font_size(11.0),
        ])
        .gap(6.0)
        .padding(12.0);
    }
    let latest_user = state.transcript.iter().rev().find_map(|item| match item {
        TranscriptItem::User { content, .. } => Some(content.as_str()),
        _ => None,
    });
    let latest_assistant = state.transcript.iter().rev().find_map(|item| match item {
        TranscriptItem::Assistant { content, .. } => Some(content.as_str()),
        _ => None,
    });
    let tool_count = state
        .transcript
        .iter()
        .filter(|item| matches!(item, TranscriptItem::Tool { .. }))
        .count();
    let mut items = vec![
        eyebrow("PROYECTO"),
        label(workspace_name(&state.workspace), theme::FOREGROUND).font_size(15.0),
        mono(workspace_hint(&state.workspace), theme::FOREGROUND_SUBTLE).font_size(11.0),
        Element::spacer().size(None, Some(6.0)),
        eyebrow("SESIÓN"),
        label(session_title(state), theme::FOREGROUND),
        Element::spacer().size(None, Some(6.0)),
        eyebrow("AGENTE"),
        label(backend_name(state), theme::FOREGROUND),
    ];
    if state.selected_backend == "native" {
        items.push(mono(&state.model, theme::FOREGROUND_SUBTLE).font_size(12.0));
    }
    items.extend([
        Element::spacer().size(None, Some(6.0)),
        eyebrow("ESTADO"),
        label(&state.status, theme::FOREGROUND_SUBTLE),
        label(
            format!("{tool_count} herramientas en esta sesión"),
            theme::FOREGROUND_SUBTLE,
        )
        .font_size(12.0),
    ]);
    if let Some(prompt) = latest_user {
        items.extend([
            Element::spacer().size(None, Some(8.0)),
            eyebrow("ÚLTIMA SOLICITUD"),
            label(shorten(prompt, 500), theme::FOREGROUND),
        ]);
    }
    if let Some(response) = latest_assistant {
        items.extend([
            Element::spacer().size(None, Some(8.0)),
            eyebrow("ÚLTIMA RESPUESTA"),
            label(shorten(response, 500), theme::FOREGROUND_SUBTLE),
        ]);
    }
    Element::column(items).gap(5.0).padding(12.0)
}

fn activity_content(state: &ViewState) -> Element {
    let tools: Vec<_> = state
        .transcript
        .iter()
        .filter(|item| matches!(item, TranscriptItem::Tool { .. }))
        .collect();
    let selected = state
        .selected_tool_id
        .as_deref()
        .and_then(|selected_id| {
            tools
                .iter()
                .copied()
                .find(|item| matches!(item, TranscriptItem::Tool { id, .. } if id == selected_id))
        })
        .or_else(|| tools.last().copied());
    let mut items = vec![eyebrow("ACTIVIDAD DE LA SESIÓN")];
    let selected_native = state.selected_session.as_deref().is_some_and(|id| {
        state.selected_backend == "native"
            || !state.native_commands.is_empty()
            || state
                .sessions
                .iter()
                .any(|session| session.id == id && session.backend == "native")
    });
    if selected_native {
        items.push(eyebrow(format!(
            "PERMISOS DE SESIÓN · {}",
            state.session_approval_grants.len()
        )));
        if state.session_approval_grants.is_empty() {
            items.push(
                label(
                    "No hay permisos vigentes en esta tarea.",
                    theme::FOREGROUND_SUBTLE,
                )
                .font_size(11.0),
            );
        }
        for grant in &state.session_approval_grants {
            items.push(
                Element::row(vec![
                    Element::column(vec![
                        label(&grant.tool_name, theme::FOREGROUND).font_size(11.0),
                        mono(shorten(&grant.key_digest, 12), theme::FOREGROUND_SUBTLE)
                            .font_size(10.0),
                    ])
                    .gap(3.0)
                    .flex(1.0),
                    button(
                        format!("session_approval_revoke:{}", grant.key_digest),
                        "Revocar",
                        theme::HOVER,
                        theme::DESTRUCTIVE,
                    )
                    .font_size(10.0)
                    .size(Some(76.0), Some(26.0)),
                ])
                .gap(8.0)
                .padding(8.0)
                .background(theme::PANEL)
                .border(theme::BORDER, 1.0)
                .radius(7.0),
            );
        }
        items.push(Element::spacer().size(None, Some(8.0)));
        items.push(eyebrow(format!(
            "COMANDOS PERSISTENTES · {}",
            state.native_commands.len()
        )));
        if let Some(error) = &state.native_commands_error {
            items.push(label(error, theme::DESTRUCTIVE).font_size(11.0));
        } else if state.native_commands.is_empty() {
            items.push(
                label(
                    "No hay procesos registrados en esta tarea.",
                    theme::FOREGROUND_SUBTLE,
                )
                .font_size(11.0),
            );
        }
        for command in &state.native_commands {
            let status = match &command.exit {
                None => "En ejecución".to_owned(),
                Some(PersistentCommandExit::Exited { code }) => {
                    format!(
                        "Finalizado · código {}",
                        code.map_or_else(|| "?".into(), |code| code.to_string())
                    )
                }
                Some(PersistentCommandExit::Terminated) => "Terminado".to_owned(),
                Some(PersistentCommandExit::TimedOut) => "Tiempo agotado".to_owned(),
                Some(PersistentCommandExit::WaitFailed(_)) => "Estado incierto".to_owned(),
            };
            let mut details = format!(
                "{} · {}",
                if command.pty { "PTY" } else { "Tuberías" },
                status
            );
            if command.has_output {
                details.push_str(" · salida pendiente");
            }
            let action = if command.exit.is_none()
                && state.native_command_terminating.as_deref() != Some(command.id.as_str())
            {
                button(
                    format!("native_command_terminate:{}", command.id),
                    "Terminar",
                    theme::HOVER,
                    theme::DESTRUCTIVE,
                )
                .font_size(10.0)
                .min_size(0.0, 0.0)
                .size(Some(72.0), Some(26.0))
            } else {
                label(
                    if state.native_command_terminating.as_deref() == Some(command.id.as_str()) {
                        "Terminando…"
                    } else {
                        ""
                    },
                    theme::FOREGROUND_SUBTLE,
                )
                .font_size(10.0)
                .size(Some(72.0), Some(26.0))
            };
            items.push(
                Element::column(vec![
                    Element::row(vec![
                        mono(shorten(&command.id, 18), theme::FOREGROUND)
                            .font_size(11.0)
                            .flex(1.0),
                        action,
                    ])
                    .gap(6.0),
                    label(details, theme::FOREGROUND_SUBTLE).font_size(10.0),
                ])
                .gap(3.0)
                .padding(8.0)
                .background(theme::PANEL)
                .border(theme::BORDER, 1.0)
                .radius(7.0),
            );
        }
        items.push(Element::spacer().size(None, Some(8.0)));
    }
    if let Some(TranscriptItem::Tool {
        name,
        arguments,
        output,
        is_error,
        ..
    }) = selected
    {
        items.push(
            label(
                format!(
                    "{} · {}",
                    if name.is_empty() { "Herramienta" } else { name },
                    tool_status(output, *is_error)
                ),
                if *is_error {
                    theme::DESTRUCTIVE
                } else {
                    theme::TOOL
                },
            )
            .font_size(16.0),
        );
        items.push(eyebrow("ARGUMENTOS"));
        items.push(mono(json_text(arguments), theme::FOREGROUND).font_size(12.0));
        items.push(eyebrow("RESULTADO"));
        items.push(
            mono(
                output.as_deref().unwrap_or("Esperando resultado…"),
                if *is_error {
                    theme::DESTRUCTIVE
                } else {
                    theme::FOREGROUND
                },
            )
            .font_size(12.0),
        );
    } else {
        items.push(pane_empty_state(
            Icon::History,
            "Sin actividad todavía",
            "Las herramientas y sus resultados aparecerán aquí cuando el agente trabaje.",
            None,
        ));
    }
    if !tools.is_empty() {
        let tool_count = tools.len();
        let (page, start, end) = page_bounds(tool_count, state.activity_page, ACTIVITY_PAGE_SIZE);
        items.extend([
            Element::spacer().size(None, Some(8.0)),
            eyebrow(format!("HERRAMIENTAS · {tool_count}")),
        ]);
        for item in tools.into_iter().rev().skip(start).take(end - start) {
            if let TranscriptItem::Tool {
                id,
                name,
                output,
                is_error,
                ..
            } = item
            {
                items.push(
                    icon_button(
                        format!("tool:{id}"),
                        Icon::Code,
                        format!(
                            "{} · {}",
                            shorten(if name.is_empty() { "Herramienta" } else { name }, 16),
                            tool_status(output, *is_error)
                        ),
                        if state.selected_tool_id.as_deref() == Some(id.as_str()) {
                            theme::SELECTED
                        } else {
                            theme::BACKGROUND
                        },
                        if *is_error {
                            theme::DESTRUCTIVE
                        } else {
                            theme::FOREGROUND_SUBTLE
                        },
                    )
                    .size(None, Some(28.0)),
                );
            }
        }
        if start > 0 || end < tool_count {
            items.push(pane_pagination(
                "activity_page",
                tool_count,
                page,
                start,
                end,
            ));
        }
    }
    if let Some((stream, output)) = state.transcript.iter().rev().find_map(|item| match item {
        TranscriptItem::ProcessOutput { stream, text } => Some((stream, text)),
        _ => None,
    }) {
        items.extend([
            Element::spacer().size(None, Some(8.0)),
            eyebrow(match stream {
                OutputStream::Stdout => "SALIDA DEL PROCESO",
                OutputStream::Stderr => "DIAGNÓSTICOS DEL PROCESO",
            }),
            mono(output, theme::FOREGROUND_SUBTLE).font_size(12.0),
        ]);
    }
    Element::column(items).gap(8.0).padding(12.0)
}

fn compact_review_line(line: &ReviewLine, max_chars: usize) -> String {
    if line.kind == ReviewLineKind::Hunk {
        return shorten(&line.text, max_chars);
    }
    let old = line.old_number.map_or_else(String::new, |n| n.to_string());
    let new = line.new_number.map_or_else(String::new, |n| n.to_string());
    let code = line.text.replace('\t', "    ");
    shorten(&format!("{old:>4} {new:>4} {code}"), max_chars)
}

fn review_file_card(
    file: &ReviewFile,
    section_index: usize,
    file_index: usize,
    width: f32,
    remaining: &mut usize,
) -> Element {
    let usable = (width - 42.0).max(100.0);
    let line_chars = (usable / 6.8).max(12.0) as usize;
    let path_chars = ((usable - 110.0) / 6.5).max(12.0) as usize;
    let filename = if file.path.is_empty() {
        "Archivo".to_owned()
    } else {
        shorten(&file.path, path_chars)
    };
    let mut content = vec![Element::row(vec![
        Element::icon(Icon::Code)
            .foreground(theme::ICON_PRIMARY)
            .size(Some(16.0), Some(20.0)),
        mono(filename, theme::FOREGROUND).font_size(11.0).flex(1.0),
        mono(format!("+{}", file.additions), theme::DIFF_ADDED).font_size(10.0),
        mono(format!("−{}", file.deletions), theme::DIFF_REMOVED).font_size(10.0),
    ])
    .gap(5.0)
    .padding(8.0)
    .size(None, Some(34.0))];
    if !file.status.is_empty() {
        content.push(
            label(&file.status, theme::FOREGROUND_SUBTLE)
                .font_size(10.0)
                .padding(7.0),
        );
    }
    if !file.lines.is_empty() && *remaining > 0 {
        content.push(divider());
    }
    let visible = file.lines.len().min((*remaining).min(60));
    for line in file.lines.iter().take(visible) {
        let (ink, fill) = match line.kind {
            ReviewLineKind::Added => (theme::DIFF_ADDED, theme::DIFF_ADDED_BG),
            ReviewLineKind::Removed => (theme::DIFF_REMOVED, theme::DIFF_REMOVED_BG),
            ReviewLineKind::Hunk => (theme::FOREGROUND_SUBTLE, theme::DIFF_HUNK_BG),
            _ => (theme::FOREGROUND_SUBTLE, theme::PANEL),
        };
        content.push(
            mono(compact_review_line(line, line_chars), ink)
                .font_size(11.0)
                .padding(3.0)
                .background(fill)
                .size(Some(usable), Some(21.0)),
        );
    }
    *remaining -= visible;
    if file.lines.len() > visible {
        content.push(
            label(
                format!("{} líneas más en este archivo", file.lines.len() - visible),
                theme::FOREGROUND_SUBTLE,
            )
            .font_size(10.0)
            .padding(7.0),
        );
    } else if file.lines.is_empty() {
        content.push(
            label(
                if file.status == "Binario" {
                    "El contenido binario no se muestra"
                } else {
                    "Sin líneas de código para mostrar"
                },
                theme::FOREGROUND_SUBTLE,
            )
            .font_size(10.0)
            .padding(7.0),
        );
    }
    Element::clickable(
        format!("review_file_open:{section_index}:{file_index}"),
        Element::column(content)
            .background(theme::PANEL)
            .border(theme::BORDER, 1.0)
            .radius(8.0),
    )
}

const REVIEW_DETAIL_PAGE_SIZE: usize = 100;

fn review_detail_content(detail: &ReviewDetailView, width: f32) -> Element {
    let mut rows = vec![
        Element::row(vec![
            hinted_icon_button(
                "review_detail_back",
                Icon::ArrowLeft,
                "Volver a cambios",
                theme::BACKGROUND,
                theme::FOREGROUND,
                28.0,
            ),
            mono(
                shorten(&detail.path, ((width - 90.0) / 7.0).max(8.0) as usize),
                theme::FOREGROUND,
            )
            .font_size(12.0)
            .flex(1.0),
        ])
        .gap(8.0),
        eyebrow(match detail.stage {
            DiffStage::Staged => "CAMBIOS PREPARADOS",
            DiffStage::Unstaged => "CAMBIOS SIN PREPARAR",
        }),
        divider(),
    ];
    match &detail.content {
        None => rows.push(pane_loading_state("review_detail", "Cargando diff", width)),
        Some(Err(error)) => rows.push(pane_empty_state(
            Icon::Code,
            "No se pudo abrir el diff",
            error,
            Some(("review_detail_retry", "Reintentar")),
        )),
        Some(Ok(None)) => rows.push(pane_empty_state(
            Icon::Check,
            "Sin diff disponible",
            "Puede que el archivo haya cambiado desde la última actualización.",
            Some(("review_refresh", "Actualizar cambios")),
        )),
        Some(Ok(Some(diff))) => {
            let file = &diff.file;
            rows.push(
                Element::row(vec![
                    label(&file.status, theme::FOREGROUND_SUBTLE)
                        .font_size(11.0)
                        .flex(1.0),
                    mono(format!("+{}", file.additions), theme::DIFF_ADDED).font_size(11.0),
                    mono(format!("−{}", file.deletions), theme::DIFF_REMOVED).font_size(11.0),
                ])
                .gap(8.0)
                .padding(8.0)
                .background(theme::PANEL)
                .radius(7.0),
            );
            if let Some(old_path) = &file.old_path {
                rows.push(
                    mono(format!("Desde {old_path}"), theme::FOREGROUND_SUBTLE).font_size(10.0),
                );
            }
            let total = file.lines.len();
            if total == 0 {
                rows.push(
                    label(
                        "No hay líneas de texto para mostrar.",
                        theme::FOREGROUND_SUBTLE,
                    )
                    .font_size(11.0),
                );
            } else {
                let (page, start, end) =
                    page_bounds(total, detail.page_index, REVIEW_DETAIL_PAGE_SIZE);
                let line_chars = ((width - 95.0) / 6.7).max(12.0) as usize;
                for line in &file.lines[start..end] {
                    let (ink, fill) = match line.kind {
                        ReviewLineKind::Added => (theme::DIFF_ADDED, theme::DIFF_ADDED_BG),
                        ReviewLineKind::Removed => (theme::DIFF_REMOVED, theme::DIFF_REMOVED_BG),
                        ReviewLineKind::Hunk => (theme::FOREGROUND_SUBTLE, theme::DIFF_HUNK_BG),
                        _ => (theme::FOREGROUND_SUBTLE, theme::CODE_SURFACE),
                    };
                    rows.push(
                        mono(compact_review_line(line, line_chars), ink)
                            .font_size(11.0)
                            .padding(4.0)
                            .background(fill)
                            .size(Some((width - 26.0).max(100.0)), Some(22.0)),
                    );
                }
                if start > 0 || end < total {
                    rows.push(pane_pagination("review_detail", total, page, start, end));
                }
            }
            if diff.truncated {
                rows.push(label("Diff limitado por tamaño. Abre el archivo en el editor para verlo completo.", theme::FOREGROUND_SUBTLE).font_size(10.0));
            }
        }
    }
    Element::column(rows).gap(5.0).padding(12.0)
}

fn code_review_content(state: &ViewState, width: f32) -> Element {
    if let Some(detail) = &state.review_detail {
        return review_detail_content(detail, width);
    }
    let mut rows = vec![Element::row(vec![
        label("Cambios de Git", theme::FOREGROUND).font_size(15.0),
        Element::spacer().flex(1.0),
        hinted_icon_button(
            "review_refresh",
            Icon::Refresh,
            "Actualizar cambios de Git",
            theme::BACKGROUND,
            theme::FOREGROUND_SUBTLE,
            28.0,
        ),
    ])
    .gap(5.0)];
    match &state.git_review {
        Some(Ok(review)) => {
            rows.push(
                Element::row(vec![
                    mono(
                        shorten(&review.branch, ((width - 150.0) / 6.8).max(8.0) as usize),
                        theme::FOREGROUND,
                    )
                    .font_size(11.0),
                    Element::spacer().flex(1.0),
                    mono(format!("+{}", review.additions), theme::DIFF_ADDED).font_size(11.0),
                    mono(format!("−{}", review.deletions), theme::DIFF_REMOVED).font_size(11.0),
                ])
                .gap(8.0)
                .padding(7.0)
                .background(theme::PANEL)
                .radius(6.0),
            );
            rows.push(
                mono(
                    format!(
                        "{} archivos · {} diffs · {} nuevos",
                        review.unique_files(),
                        review.files,
                        review.untracked.len(),
                    ),
                    theme::FOREGROUND_SUBTLE,
                )
                .font_size(11.0),
            );
            if let Some(tracking) = &review.tracking {
                rows.push(
                    mono(
                        format!(
                            "{} · ↑{}  ↓{}",
                            tracking.upstream, tracking.ahead, tracking.behind
                        ),
                        theme::FOREGROUND_SUBTLE,
                    )
                    .font_size(10.0),
                );
            }
            rows.push(divider());
            rows.push(
                Element::row(vec![
                    eyebrow("GITHUB"),
                    Element::spacer().flex(1.0),
                    match &review.github {
                        GithubStatus::Ready(github) => {
                            mono(&github.repository.remote_name, theme::FOREGROUND_SUBTLE)
                                .font_size(10.0)
                        }
                        _ => Element::spacer(),
                    },
                ])
                .padding(4.0),
            );
            match &review.github {
                GithubStatus::Loading => rows.push(
                    label("Consultando PR de la rama…", theme::FOREGROUND_SUBTLE).font_size(11.0),
                ),
                GithubStatus::NoRemote => rows.push(
                    label(
                        "Sin remoto de GitHub en este proyecto",
                        theme::FOREGROUND_SUBTLE,
                    )
                    .font_size(11.0),
                ),
                GithubStatus::Unavailable(error) => {
                    rows.push(label(error, theme::FOREGROUND_SUBTLE).font_size(11.0))
                }
                GithubStatus::Ready(github) => {
                    rows.push(
                        Button::new(
                            "github_repo_open",
                            github.repository.full_name(),
                            theme::action_palette(theme::CARD, theme::FOREGROUND),
                        )
                        .variant(ButtonVariant::Outline)
                        .size(ControlSize::Compact)
                        .render()
                        .font_family(FontFamily::Sans)
                        .font_size(12.0)
                        .background(theme::CARD)
                        .border(theme::BORDER, 1.0)
                        .radius(6.0)
                        .size(Some((width - 24.0).max(80.0)), Some(32.0)),
                    );
                    for pull in github.pulls.iter().take(4) {
                        let title = shorten(&pull.title, ((width - 90.0) / 6.5).max(12.0) as usize);
                        let draft = if pull.draft { " · Borrador" } else { "" };
                        rows.push(
                            Button::new(
                                format!("github_pr_open:{}", pull.number),
                                format!("#{} {}{}", pull.number, title, draft),
                                theme::action_palette(theme::PANEL, theme::FOREGROUND),
                            )
                            .variant(ButtonVariant::Outline)
                            .size(ControlSize::Compact)
                            .render()
                            .font_family(FontFamily::Sans)
                            .font_size(11.0)
                            .background(theme::PANEL)
                            .border(theme::BORDER, 1.0)
                            .radius(6.0)
                            .size(Some((width - 24.0).max(80.0)), Some(32.0)),
                        );
                    }
                    if github.pulls.is_empty() && github.notice.is_none() {
                        rows.push(
                            label("Sin PR abierto para esta rama", theme::FOREGROUND_SUBTLE)
                                .font_size(11.0),
                        );
                    }
                    if let Some(notice) = &github.notice {
                        rows.push(label(notice, theme::FOREGROUND_SUBTLE).font_size(11.0));
                    }
                }
            }
            rows.push(divider());
            let tracked_count: usize = review
                .sections
                .iter()
                .map(|section| section.files.len())
                .sum();
            let total = tracked_count + review.untracked.len();
            if total == 0 {
                rows.push(pane_empty_state(
                    Icon::Check,
                    "Sin cambios pendientes",
                    "Los archivos del proyecto están al día.",
                    None,
                ));
            } else {
                let (page, start, end) = page_bounds(total, state.review_page, REVIEW_PAGE_SIZE);
                let mut remaining_lines = 240;
                let mut offset = 0;
                for (section_index, section) in review.sections.iter().enumerate() {
                    let files = section.files.len();
                    let local_start = start.saturating_sub(offset).min(files);
                    let local_end = end.saturating_sub(offset).min(files);
                    offset += files;
                    if local_start == local_end {
                        continue;
                    }
                    rows.push(
                        Element::row(vec![
                            eyebrow(&section.title),
                            Element::spacer().flex(1.0),
                            mono(format!("{files}"), theme::FOREGROUND_SUBTLE).font_size(10.0),
                        ])
                        .padding(4.0),
                    );
                    for (local_index, file) in
                        section.files[local_start..local_end].iter().enumerate()
                    {
                        rows.push(review_file_card(
                            file,
                            section_index,
                            local_start + local_index,
                            width,
                            &mut remaining_lines,
                        ));
                    }
                }
                if page == 0 {
                    for (title, empty_text) in [
                        ("CAMBIOS PREPARADOS", "Sin cambios preparados"),
                        ("CAMBIOS SIN PREPARAR", "Sin cambios sin preparar"),
                    ] {
                        if !review.sections.iter().any(|section| section.title == title) {
                            rows.push(eyebrow(title).padding(4.0));
                            rows.push(label(empty_text, theme::FOREGROUND_SUBTLE).font_size(11.0));
                        }
                    }
                }
                let untracked_start = start
                    .saturating_sub(tracked_count)
                    .min(review.untracked.len());
                let untracked_end = end
                    .saturating_sub(tracked_count)
                    .min(review.untracked.len());
                if untracked_start < untracked_end {
                    rows.push(
                        Element::row(vec![
                            eyebrow("SIN SEGUIMIENTO"),
                            Element::spacer().flex(1.0),
                            mono(
                                format!("{}", review.untracked.len()),
                                theme::FOREGROUND_SUBTLE,
                            )
                            .font_size(10.0),
                        ])
                        .padding(4.0),
                    );
                    let path_chars = ((width - 75.0) / 6.5).max(12.0) as usize;
                    for path in review.untracked[untracked_start..untracked_end].iter() {
                        rows.push(
                            Element::row(vec![
                                Element::icon(Icon::Code)
                                    .foreground(theme::ICON_PRIMARY)
                                    .size(Some(16.0), Some(16.0)),
                                mono(shorten(path, path_chars), theme::FOREGROUND).font_size(11.0),
                            ])
                            .gap(7.0)
                            .padding(8.0)
                            .background(theme::PANEL)
                            .border(theme::BORDER, 1.0)
                            .radius(6.0),
                        );
                    }
                }
                if start > 0 || end < total {
                    rows.push(pane_pagination("review_page", total, page, start, end));
                }
                if review.truncated || remaining_lines == 0 {
                    rows.push(
                        label(
                            "El diff se ha acotado para mantener el panel fluido; actualiza o abre Git para inspeccionar el resto.",
                            theme::FOREGROUND_SUBTLE,
                        )
                        .font_size(11.0),
                    );
                }
            }
        }
        Some(Err(error)) => {
            rows.push(pane_empty_state(
                Icon::Code,
                "No se pudieron cargar los cambios",
                error,
                Some(("review_refresh", "Reintentar")),
            ));
        }
        None => rows.push(pane_loading_state(
            "review_changes",
            "Cargando cambios",
            width,
        )),
    }
    Element::column(rows).gap(5.0).padding(12.0)
}

const PREVIEW_VISIBLE_CHARS_PER_LINE: usize = 280;
const PREVIEW_FONT_SIZE: f32 = 11.5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreviewLanguage {
    Rust,
    TypeScript,
    JavaScript,
    Json,
    Markdown,
    Text,
}

impl PreviewLanguage {
    fn for_path(path: &str) -> Self {
        let extension = Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default();
        if extension.eq_ignore_ascii_case("rs") {
            Self::Rust
        } else if extension.eq_ignore_ascii_case("ts") || extension.eq_ignore_ascii_case("tsx") {
            Self::TypeScript
        } else if ["js", "jsx", "mjs", "cjs"]
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        {
            Self::JavaScript
        } else if extension.eq_ignore_ascii_case("json") || extension.eq_ignore_ascii_case("jsonc")
        {
            Self::Json
        } else if extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        {
            Self::Markdown
        } else {
            Self::Text
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::TypeScript => "TypeScript",
            Self::JavaScript => "JavaScript",
            Self::Json => "JSON",
            Self::Markdown => "Markdown",
            Self::Text => "Texto",
        }
    }
}

#[derive(Default)]
struct PreviewLexState {
    block_comment: bool,
    markdown_fence: bool,
}

fn preview_span(spans: &mut Vec<TextSpan>, text: &str, color: Color, font_size: f32) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.color == color && last.font_size == font_size && last.background.is_none() {
            last.text.push_str(text);
            return;
        }
    }
    spans.push(TextSpan::new(text, color, font_size, FontFamily::Monospace));
}

fn preview_char(value: &str, byte_index: usize) -> Option<(char, usize)> {
    value
        .get(byte_index..)?
        .chars()
        .next()
        .map(|ch| (ch, ch.len_utf8()))
}

fn preview_keyword(value: &str, language: PreviewLanguage) -> bool {
    match language {
        PreviewLanguage::Rust => matches!(
            value,
            "as" | "async"
                | "await"
                | "break"
                | "const"
                | "continue"
                | "crate"
                | "dyn"
                | "else"
                | "enum"
                | "extern"
                | "false"
                | "fn"
                | "for"
                | "if"
                | "impl"
                | "in"
                | "let"
                | "loop"
                | "match"
                | "mod"
                | "move"
                | "mut"
                | "pub"
                | "ref"
                | "return"
                | "self"
                | "Self"
                | "static"
                | "struct"
                | "super"
                | "trait"
                | "true"
                | "type"
                | "use"
                | "where"
                | "while"
        ),
        PreviewLanguage::TypeScript | PreviewLanguage::JavaScript => matches!(
            value,
            "as" | "async"
                | "await"
                | "break"
                | "case"
                | "catch"
                | "class"
                | "const"
                | "continue"
                | "default"
                | "delete"
                | "do"
                | "else"
                | "export"
                | "extends"
                | "false"
                | "finally"
                | "for"
                | "from"
                | "function"
                | "if"
                | "import"
                | "in"
                | "instanceof"
                | "interface"
                | "let"
                | "new"
                | "null"
                | "of"
                | "return"
                | "static"
                | "super"
                | "switch"
                | "this"
                | "throw"
                | "true"
                | "try"
                | "type"
                | "typeof"
                | "undefined"
                | "var"
                | "void"
                | "while"
                | "yield"
        ),
        PreviewLanguage::Json => matches!(value, "true" | "false" | "null"),
        PreviewLanguage::Markdown | PreviewLanguage::Text => false,
    }
}

fn preview_markdown_spans(line: &str, state: &mut PreviewLexState) -> Vec<TextSpan> {
    let mut spans = Vec::new();
    let trimmed = line.trim_start();
    let indentation = line.len() - trimmed.len();
    if indentation > 0 {
        preview_span(
            &mut spans,
            &line[..indentation],
            theme::FOREGROUND,
            PREVIEW_FONT_SIZE,
        );
    }
    if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
        state.markdown_fence = !state.markdown_fence;
        preview_span(
            &mut spans,
            trimmed,
            theme::CODE_PUNCTUATION,
            PREVIEW_FONT_SIZE,
        );
    } else if state.markdown_fence {
        preview_span(&mut spans, trimmed, theme::CODE_STRING, PREVIEW_FONT_SIZE);
    } else if let Some(marker) = trimmed
        .find(' ')
        .filter(|&space| space > 0 && space <= 6 && trimmed[..space].bytes().all(|b| b == b'#'))
    {
        preview_span(
            &mut spans,
            &trimmed[..marker],
            theme::CODE_PUNCTUATION,
            PREVIEW_FONT_SIZE,
        );
        preview_span(&mut spans, &trimmed[marker..], theme::FOREGROUND, 13.0);
    } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("> ") {
        preview_span(
            &mut spans,
            &trimmed[..1],
            theme::CODE_PUNCTUATION,
            PREVIEW_FONT_SIZE,
        );
        preview_span(
            &mut spans,
            &trimmed[1..],
            theme::FOREGROUND,
            PREVIEW_FONT_SIZE,
        );
    } else {
        preview_span(&mut spans, trimmed, theme::FOREGROUND, PREVIEW_FONT_SIZE);
    }
    spans
}

fn preview_source_spans(
    line: &str,
    language: PreviewLanguage,
    state: &mut PreviewLexState,
) -> Vec<TextSpan> {
    if language == PreviewLanguage::Markdown {
        return preview_markdown_spans(line, state);
    }
    if language == PreviewLanguage::Text {
        return vec![TextSpan::new(
            if line.is_empty() { " " } else { line },
            theme::FOREGROUND,
            PREVIEW_FONT_SIZE,
            FontFamily::Monospace,
        )];
    }
    let mut spans = Vec::new();
    let mut cursor = 0;
    while cursor < line.len() {
        let rest = &line[cursor..];
        if state.block_comment {
            let end = rest
                .find("*/")
                .map_or(line.len(), |offset| cursor + offset + 2);
            preview_span(
                &mut spans,
                &line[cursor..end],
                theme::CODE_COMMENT,
                PREVIEW_FONT_SIZE,
            );
            state.block_comment = end == line.len() && !rest.contains("*/");
            cursor = end;
            continue;
        }
        if rest.starts_with("//") && language != PreviewLanguage::Json {
            preview_span(&mut spans, rest, theme::CODE_COMMENT, PREVIEW_FONT_SIZE);
            break;
        }
        if rest.starts_with("/*") && language != PreviewLanguage::Json {
            state.block_comment = true;
            continue;
        }
        let (ch, ch_len) = preview_char(line, cursor).expect("cursor is a character boundary");
        if ch == '"' || (language != PreviewLanguage::Json && (ch == '\'' || ch == '`')) {
            let mut end = cursor + ch_len;
            let mut escaped = false;
            while let Some((next, len)) = preview_char(line, end) {
                end += len;
                if escaped {
                    escaped = false;
                } else if next == '\\' {
                    escaped = true;
                } else if next == ch {
                    break;
                }
            }
            preview_span(
                &mut spans,
                &line[cursor..end],
                theme::CODE_STRING,
                PREVIEW_FONT_SIZE,
            );
            cursor = end;
            continue;
        }
        if ch.is_ascii_digit() {
            let mut end = cursor + ch_len;
            while let Some((next, len)) = preview_char(line, end) {
                if next.is_ascii_alphanumeric() || next == '_' || next == '.' {
                    end += len;
                } else {
                    break;
                }
            }
            preview_span(
                &mut spans,
                &line[cursor..end],
                theme::CODE_NUMBER,
                PREVIEW_FONT_SIZE,
            );
            cursor = end;
            continue;
        }
        if ch.is_alphabetic() || ch == '_' || ch == '$' {
            let mut end = cursor + ch_len;
            while let Some((next, len)) = preview_char(line, end) {
                if next.is_alphanumeric() || next == '_' || next == '$' {
                    end += len;
                } else {
                    break;
                }
            }
            let word = &line[cursor..end];
            preview_span(
                &mut spans,
                word,
                if preview_keyword(word, language) {
                    theme::CODE_KEYWORD
                } else {
                    theme::FOREGROUND
                },
                PREVIEW_FONT_SIZE,
            );
            cursor = end;
            continue;
        }
        preview_span(
            &mut spans,
            &line[cursor..cursor + ch_len],
            if "{}[]():;,.<>".contains(ch) {
                theme::CODE_PUNCTUATION
            } else {
                theme::FOREGROUND
            },
            PREVIEW_FONT_SIZE,
        );
        cursor += ch_len;
    }
    if spans.is_empty() {
        preview_span(&mut spans, " ", theme::FOREGROUND, PREVIEW_FONT_SIZE);
    }
    spans
}

fn bounded_preview_line(source: &str) -> (String, bool) {
    let mut display = String::new();
    let mut chars = source.trim_end_matches('\r').chars();
    for ch in chars.by_ref().take(PREVIEW_VISIBLE_CHARS_PER_LINE) {
        match ch {
            '\t' => display.push_str("    "),
            ch if ch.is_control() => display.push('�'),
            ch => display.push(ch),
        }
    }
    (display, chars.next().is_some())
}

fn file_preview_content(preview: &FilePreviewView, width: f32) -> Element {
    let language = PreviewLanguage::for_path(&preview.path);
    let surface_width = (width - 38.0).max(120.0);
    let mut rows = vec![
        Element::row(vec![
            Element::icon(Icon::Code)
                .foreground(theme::ICON_PRIMARY)
                .size(Some(16.0), Some(22.0)),
            mono(
                shorten(&preview.path, ((width - 88.0) / 6.8).max(8.0) as usize),
                theme::FOREGROUND,
            )
            .font_size(11.0)
            .flex(1.0),
            hinted_icon_button(
                format!("dock_close:{}", preview.tab_id),
                Icon::Close,
                "Cerrar archivo",
                theme::BACKGROUND,
                theme::FOREGROUND_SUBTLE,
                26.0,
            ),
        ])
        .gap(6.0)
        .size(None, Some(28.0)),
        divider(),
    ];
    match &preview.content {
        None => rows.push(pane_loading_state(
            "file_preview",
            "Cargando archivo",
            width,
        )),
        Some(Ok(page)) => {
            let page_lines = page.end_line.saturating_sub(page.start_line);
            let total_label = page.total_lines.map_or_else(
                || format!("{}+ líneas", page.end_line),
                |total| format!("{total} líneas"),
            );
            rows.push(
                Element::row(vec![
                    label(language.title(), theme::FOREGROUND)
                        .font_size(10.0)
                        .padding(5.0)
                        .background(theme::SELECTED)
                        .radius(4.0),
                    mono(total_label, theme::FOREGROUND_SUBTLE).font_size(10.0),
                ])
                .gap(8.0),
            );
            let digits = page.end_line.max(1).to_string().len();
            let gutter_width = (digits as f32 * 7.0 + 14.0).max(30.0);
            let code_width = (surface_width - gutter_width - 31.0).max(40.0);
            let mut code_rows = Vec::new();
            let mut lex_state = PreviewLexState::default();
            let mut shortened_lines = 0;
            for (index, source) in page.text.lines().take(page_lines).enumerate() {
                let (display, shortened) = bounded_preview_line(source);
                shortened_lines += usize::from(shortened);
                let mut spans = preview_source_spans(&display, language, &mut lex_state);
                if shortened {
                    preview_span(
                        &mut spans,
                        " …",
                        theme::FOREGROUND_SUBTLE,
                        PREVIEW_FONT_SIZE,
                    );
                }
                code_rows.push(
                    Element::row(vec![
                        mono(
                            format!("{:>digits$}", page.start_line + index + 1),
                            theme::CODE_GUTTER,
                        )
                        .font_size(10.5)
                        .size(Some(gutter_width), Some(16.0)),
                        Element::spacer()
                            .size(Some(1.0), None)
                            .background(theme::BORDER),
                        Element::formatted_text(spans).size(Some(code_width), None),
                    ])
                    .gap(6.0)
                    .padding(3.0)
                    .size(Some(surface_width - 12.0), None),
                );
            }
            if code_rows.is_empty() {
                code_rows.push(
                    label("Archivo vacío", theme::FOREGROUND_SUBTLE)
                        .font_size(11.0)
                        .padding(16.0),
                );
            }
            if page.truncated || shortened_lines > 0 {
                let notice = match shortened_lines {
                    0 => "Vista acotada por tamaño de archivo".to_owned(),
                    1 => "Vista acotada · 1 línea larga abreviada".to_owned(),
                    count => format!("Vista acotada · {count} líneas largas abreviadas"),
                };
                rows.push(label(notice, theme::FOREGROUND_SUBTLE).font_size(10.0));
            }
            rows.push(
                Element::column(code_rows)
                    .gap(0.0)
                    .padding(6.0)
                    .background(theme::CODE_SURFACE)
                    .border(theme::BORDER, 1.0)
                    .radius(7.0)
                    .size(Some(surface_width), None),
            );
            if preview.page_index > 0 || page.has_more {
                let previous = if preview.page_index > 0 {
                    hinted_icon_button(
                        "file_preview_previous",
                        Icon::ArrowLeft,
                        "Página anterior",
                        theme::BACKGROUND,
                        theme::FOREGROUND,
                        28.0,
                    )
                } else {
                    Element::spacer().size(Some(28.0), Some(28.0))
                };
                let next = if page.has_more {
                    hinted_icon_button(
                        "file_preview_next",
                        Icon::ArrowRight,
                        "Página siguiente",
                        theme::BACKGROUND,
                        theme::FOREGROUND,
                        28.0,
                    )
                } else {
                    Element::spacer().size(Some(28.0), Some(28.0))
                };
                rows.push(
                    Element::row(vec![
                        mono(
                            format!("{}–{}", page.start_line.saturating_add(1), page.end_line),
                            theme::FOREGROUND_SUBTLE,
                        )
                        .font_size(11.0),
                        Element::spacer().flex(1.0),
                        previous,
                        next,
                    ])
                    .gap(4.0)
                    .padding(5.0)
                    .background(theme::PANEL)
                    .radius(7.0),
                );
            }
        }
        Some(Err(error)) => rows.push(pane_empty_state(
            Icon::Code,
            "No se pudo abrir el archivo",
            error,
            Some(("file_preview_retry", "Reintentar")),
        )),
    }
    Element::column(rows).gap(8.0).padding(12.0)
}

pub(crate) fn dock_tab_page_size(width: f32) -> usize {
    if width < 430.0 {
        1
    } else if width < 540.0 {
        2
    } else {
        3
    }
}

fn dock_open_actions(width: f32) -> Element {
    let action_width = width.max(150.0);
    Element::column(vec![
        icon_button(
            "dock_open:context",
            Icon::PanelRight,
            "Contexto",
            theme::CARD,
            theme::FOREGROUND,
        )
        .size(Some(action_width), Some(38.0)),
        icon_button(
            "dock_open:files",
            Icon::Folder,
            "Archivos",
            theme::CARD,
            theme::FOREGROUND,
        )
        .size(Some(action_width), Some(38.0)),
        icon_button(
            "dock_open:activity",
            Icon::History,
            "Actividad",
            theme::CARD,
            theme::FOREGROUND,
        )
        .size(Some(action_width), Some(38.0)),
        icon_button(
            "dock_open:review",
            Icon::Code,
            "Cambios de Git",
            theme::CARD,
            theme::FOREGROUND,
        )
        .size(Some(action_width), Some(38.0)),
        icon_button(
            "dock_open:terminal",
            Icon::Terminal,
            "Terminal",
            theme::CARD,
            theme::FOREGROUND,
        )
        .size(Some(action_width), Some(38.0)),
    ])
    .gap(6.0)
}

fn dock_add_menu(state: &ViewState) -> Element {
    let entries = [
        ("terminal", "Terminal", Icon::Terminal, SideTab::Terminal),
        ("context", "Contexto", Icon::PanelRight, SideTab::Context),
        ("files", "Archivos", Icon::Folder, SideTab::Files),
        ("activity", "Actividad", Icon::History, SideTab::Activity),
        ("review", "Cambios de Git", Icon::Code, SideTab::CodeReview),
    ];
    let menu = entries
        .into_iter()
        .map(|(id, title, icon, kind)| {
            let already_open = state.dock_tabs.iter().any(|tab| tab.kind == kind);
            let mut item = DropdownMenuItem::new(format!("dock_open:{id}"), title).icon(icon);
            if already_open {
                item = item.annotation("Abierta");
            }
            item.into()
        })
        .collect();
    let anchor = if state.show_dock_add_menu {
        icon_button(
            "dock_add_tab",
            Icon::Add,
            "",
            theme::SELECTED,
            theme::FOREGROUND,
        )
        .size(Some(32.0), Some(32.0))
    } else {
        hinted_icon_button(
            "dock_add_tab",
            Icon::Add,
            "Abrir pestaña",
            theme::BACKGROUND,
            theme::FOREGROUND_SUBTLE,
            32.0,
        )
    };
    DropdownMenu::new(
        anchor,
        menu,
        state.show_dock_add_menu,
        "dock_add_dismiss",
        theme::menu_palette(),
    )
    .placement(PopoverPlacement::BottomEnd)
    .width(224.0)
    .render()
}

fn dock_empty_content(width: f32) -> Element {
    let content_width = (width - 48.0).clamp(150.0, 320.0);
    let content = Element::column(vec![
        label("Abrir pestaña", theme::FOREGROUND).font_size(19.0),
        label("Elige una vista para este panel.", theme::FOREGROUND_SUBTLE).font_size(12.0),
        Element::spacer().size(None, Some(8.0)),
        dock_open_actions(content_width),
    ])
    .gap(8.0)
    .size(Some(content_width), None);
    Element::column(vec![
        Element::spacer().flex(1.0),
        Element::row(vec![
            Element::spacer().flex(1.0),
            content,
            Element::spacer().flex(1.0),
        ]),
        Element::spacer().flex(1.0),
    ])
    .padding(20.0)
    .flex(1.0)
}

fn terminal_color(color: TerminalColor, default: Color) -> Color {
    match color {
        TerminalColor::Default => default,
        TerminalColor::Rgb(red, green, blue) => Color::rgb(red, green, blue),
        TerminalColor::Indexed(index) if index < 16 => {
            const ANSI: [(u8, u8, u8); 16] = [
                (38, 38, 38),
                (205, 79, 79),
                (126, 190, 137),
                (207, 183, 118),
                (115, 157, 208),
                (188, 142, 198),
                (112, 185, 190),
                (214, 214, 214),
                (100, 100, 100),
                (238, 108, 108),
                (152, 216, 163),
                (229, 208, 135),
                (147, 185, 230),
                (208, 168, 218),
                (144, 207, 211),
                (245, 245, 245),
            ];
            let (red, green, blue) = ANSI[index as usize];
            Color::rgb(red, green, blue)
        }
        TerminalColor::Indexed(index) if index < 232 => {
            let cube = index - 16;
            let channel = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            Color::rgb(
                channel(cube / 36),
                channel((cube / 6) % 6),
                channel(cube % 6),
            )
        }
        TerminalColor::Indexed(index) => {
            let gray = 8 + (index - 232) * 10;
            Color::rgb(gray, gray, gray)
        }
    }
}

fn terminal_row(snapshot: &TerminalSnapshot, index: usize, width: f32) -> Element {
    let spans = snapshot
        .styled_lines
        .get(index)
        .into_iter()
        .flatten()
        .map(|run| {
            let mut foreground = terminal_color(run.style.foreground, theme::FOREGROUND);
            let mut background = match run.style.background {
                TerminalColor::Default => None,
                color => Some(terminal_color(color, theme::CODE_SURFACE)),
            };
            if run.style.inverse {
                let swapped = background.unwrap_or(theme::CODE_SURFACE);
                background = Some(foreground);
                foreground = swapped;
            }
            if run.style.bold && run.style.foreground == TerminalColor::Default {
                foreground = theme::BRAND;
            }
            if run.style.dim {
                foreground = Color::rgb(
                    foreground.r.saturating_sub(45),
                    foreground.g.saturating_sub(45),
                    foreground.b.saturating_sub(45),
                );
            }
            let mut span = TextSpan::new(&run.text, foreground, 11.5, FontFamily::Monospace);
            if let Some(background) = background {
                span = span.background(background);
            }
            span
        })
        .collect::<Vec<_>>();
    Element::formatted_text(if spans.is_empty() {
        vec![TextSpan::new(
            " ",
            theme::FOREGROUND,
            11.5,
            FontFamily::Monospace,
        )]
    } else {
        spans
    })
    .size(Some(width), Some(16.0))
}

fn terminal_content(state: &ViewState, width: f32) -> Element {
    let content_width = (width - 24.0).max(140.0);
    let header = Element::row(vec![
        Element::icon(Icon::Terminal)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(18.0), Some(18.0)),
        label("Terminal", theme::FOREGROUND).font_size(14.0),
        Element::spacer().flex(1.0),
        hinted_icon_button(
            "terminal_scroll_up",
            Icon::ArrowUp,
            "Subir en el historial",
            theme::BACKGROUND,
            theme::FOREGROUND_SUBTLE,
            28.0,
        ),
        hinted_icon_button(
            "terminal_scroll_down",
            Icon::ArrowDown,
            "Bajar en el historial",
            theme::BACKGROUND,
            theme::FOREGROUND_SUBTLE,
            28.0,
        ),
        hinted_shortcut_icon_button(
            "terminal_clear",
            Icon::Trash,
            "Limpiar pantalla",
            "Ctrl+L",
            theme::BACKGROUND,
            theme::FOREGROUND_SUBTLE,
            28.0,
        ),
        hinted_shortcut_icon_button(
            "terminal_interrupt",
            Icon::Stop,
            "Interrumpir",
            "Ctrl+C",
            theme::BACKGROUND,
            theme::FOREGROUND_SUBTLE,
            28.0,
        ),
    ])
    .gap(5.0)
    .size(Some(content_width), Some(32.0));
    let Some(snapshot) = &state.terminal_snapshot else {
        let description = state
            .terminal_error
            .as_deref()
            .unwrap_or("Preparando una sesión local en el proyecto…");
        return Element::column(vec![
            header,
            pane_empty_state(
                Icon::Terminal,
                "Terminal no disponible",
                description,
                Some(("terminal_restart", "Reintentar")),
            ),
        ])
        .gap(12.0)
        .padding(12.0);
    };
    let mut rows = Vec::with_capacity(snapshot.styled_lines.len());
    for index in 0..snapshot.styled_lines.len() {
        rows.push(terminal_row(snapshot, index, content_width - 18.0));
    }
    let screen = Element::column(rows)
        .gap(0.0)
        .padding(9.0)
        .background(theme::CODE_SURFACE)
        .border(theme::BORDER, 1.0)
        .radius(9.0)
        .size(Some(content_width), None);
    let screen = Element::clickable("terminal_screen", screen);
    let status = if let Some(error) = &state.terminal_error {
        error.clone()
    } else if let Some(error) = &snapshot.error {
        error.clone()
    } else if snapshot.running {
        format!(
            "Sesión local · {} × {} · clic en pantalla para interactuar",
            snapshot.cols, snapshot.rows
        )
    } else {
        format!(
            "Proceso finalizado · código {}",
            snapshot.exit_code.unwrap_or(0)
        )
    };
    let mut content = vec![
        header,
        screen,
        mono(status, theme::FOREGROUND_SUBTLE).font_size(10.0),
    ];
    if snapshot.running {
        let input = Element::text_input(
            "terminal_input",
            &state.terminal_input,
            "Escribe un comando y pulsa Enter",
        )
        .font_family(FontFamily::Monospace)
        .font_size(12.0)
        .background(theme::CARD)
        .foreground(theme::FOREGROUND)
        .border(theme::BORDER, 1.0)
        .radius(8.0)
        .size(Some((content_width - 43.0).max(90.0)), Some(36.0));
        content.push(
            Element::row(vec![
                input,
                hinted_icon_button(
                    "terminal_send",
                    Icon::ArrowRight,
                    "Enviar comando · Enter",
                    theme::SELECTED,
                    theme::FOREGROUND,
                    36.0,
                ),
            ])
            .gap(7.0)
            .size(Some(content_width), Some(36.0)),
        );
    } else {
        content.push(
            button(
                "terminal_restart",
                "Nueva sesión",
                theme::SELECTED,
                theme::FOREGROUND,
            )
            .size(Some(126.0), Some(32.0)),
        );
    }
    Element::column(content)
        .gap(9.0)
        .padding(12.0)
        .size(Some(width), None)
}

fn side_pane(state: &ViewState, width: f32) -> Element {
    let page_size = dock_tab_page_size(width);
    let active_index = state
        .dock_tabs
        .iter()
        .position(|tab| tab.active)
        .unwrap_or(0);
    let tab_page = active_index / page_size;
    let tab_page_count = state.dock_tabs.len().div_ceil(page_size);
    let visible_tabs: Vec<&DockTabView> = state
        .dock_tabs
        .iter()
        .skip(tab_page * page_size)
        .take(page_size)
        .collect();
    // Reserve both trailing actions, their gaps, and the header's inset before
    // sizing the tab strip. At the minimum dock width all controls stay in the
    // same top row rather than leaking into the transcript pane.
    let reserved = if tab_page_count > 1 { 200.0 } else { 146.0 };
    let tab_width = ((width - reserved) / visible_tabs.len().max(1) as f32).clamp(44.0, 110.0);
    let tabs = Tabs::new(
        visible_tabs
            .into_iter()
            .map(|tab| {
                Tab::new(format!("dock_tab:{}", tab.id), &tab.title, tab_width)
                    .icon(match tab.kind {
                        SideTab::Context => Icon::PanelRight,
                        SideTab::Files => Icon::Folder,
                        SideTab::Activity => Icon::History,
                        SideTab::CodeReview | SideTab::FilePreview => Icon::Code,
                        SideTab::Terminal => Icon::Terminal,
                    })
                    .active(tab.active)
            })
            .collect(),
        theme::segmented_palette(),
    )
    .id("right_dock_tabs")
    .height(32.0)
    .render();
    let mut tab_controls = vec![tabs, Element::spacer().flex(1.0)];
    if tab_page_count > 1 {
        tab_controls.push(if tab_page > 0 {
            hinted_icon_button(
                "dock_tab_page_previous",
                Icon::ArrowLeft,
                "Pestañas anteriores",
                theme::BACKGROUND,
                theme::FOREGROUND_SUBTLE,
                28.0,
            )
        } else {
            Element::spacer().size(Some(28.0), Some(28.0))
        });
        tab_controls.push(if tab_page + 1 < tab_page_count {
            hinted_icon_button(
                "dock_tab_page_next",
                Icon::ArrowRight,
                "Más pestañas",
                theme::BACKGROUND,
                theme::FOREGROUND_SUBTLE,
                28.0,
            )
        } else {
            Element::spacer().size(Some(28.0), Some(28.0))
        });
    }
    tab_controls.push(dock_add_menu(state));
    tab_controls.push(hinted_icon_button(
        "dock_overview_toggle",
        Icon::Menu,
        if state.show_dock_overview {
            "Cerrar vista de pestañas"
        } else {
            "Ver todas las pestañas"
        },
        if state.show_dock_overview {
            theme::SELECTED
        } else {
            theme::BACKGROUND
        },
        theme::FOREGROUND_SUBTLE,
        32.0,
    ));
    tab_controls.push(hinted_icon_button(
        "toggle_side_pane",
        Icon::PanelRightClose,
        "Ocultar panel derecho",
        theme::BACKGROUND,
        theme::FOREGROUND_SUBTLE,
        32.0,
    ));
    let header = Element::panel(Element::row(tab_controls).gap(4.0))
        .padding(16.0)
        .size(None, Some(RIGHT_HEADER_HEIGHT));
    let body = if state.dock_tabs.is_empty() && !state.show_dock_overview {
        dock_empty_content(width)
    } else if state.show_dock_overview {
        let query = state.dock_overview_query.trim().to_lowercase();
        let matches = |title: &str, kind: SideTab| {
            let kind_name = match kind {
                SideTab::Context => "contexto",
                SideTab::Files => "archivos",
                SideTab::Activity => "actividad",
                SideTab::CodeReview => "cambios",
                SideTab::Terminal => "terminal",
                SideTab::FilePreview => "vista de archivo",
            };
            query.is_empty() || title.to_lowercase().contains(&query) || kind_name.contains(&query)
        };
        let filtered_tabs: Vec<_> = state
            .dock_tabs
            .iter()
            .filter(|tab| matches(&tab.title, tab.kind))
            .collect();
        let filtered_recent: Vec<_> = state
            .dock_recently_closed
            .iter()
            .filter(|tab| matches(&tab.title, tab.kind))
            .collect();
        let mut tabs = vec![
            Search::new(
                "dock_overview_query",
                &state.dock_overview_query,
                "Buscar pestañas…",
                theme::component_palette(),
            )
            .render()
            .size(None, Some(34.0)),
            Element::row(vec![
                eyebrow("PESTAÑAS ABIERTAS"),
                Element::spacer().flex(1.0),
                if state.dock_tabs.is_empty() {
                    Element::spacer().size(Some(86.0), Some(24.0))
                } else {
                    button(
                        "dock_close_all",
                        "Cerrar todas",
                        theme::HOVER,
                        theme::FOREGROUND_SUBTLE,
                    )
                    .font_size(11.0)
                    .min_size(0.0, 0.0)
                    .size(Some(86.0), Some(24.0))
                },
            ])
            .size(None, Some(24.0)),
        ];
        if filtered_tabs.is_empty() {
            tabs.push(
                label(
                    if state.dock_tabs.is_empty() {
                        "No hay pestañas abiertas"
                    } else {
                        "Sin coincidencias abiertas"
                    },
                    theme::FOREGROUND_SUBTLE,
                )
                .font_size(11.0)
                .padding(8.0),
            );
        }
        let first_id = state.dock_tabs.first().map(|tab| tab.id);
        let last_id = state.dock_tabs.last().map(|tab| tab.id);
        for tab in filtered_tabs {
            tabs.push(
                Element::row(vec![
                    button(
                        format!("dock_tab:{}", tab.id),
                        shorten(&tab.title, 25),
                        if tab.active {
                            theme::SELECTED
                        } else {
                            theme::BACKGROUND
                        },
                        theme::FOREGROUND,
                    )
                    .font_size(12.0)
                    .min_size(0.0, 0.0)
                    .flex(1.0)
                    .size(None, Some(28.0)),
                    button(
                        format!("dock_close_others:{}", tab.id),
                        "Solo",
                        theme::BACKGROUND,
                        theme::FOREGROUND_SUBTLE,
                    )
                    .font_size(10.0)
                    .min_size(0.0, 0.0)
                    .size(Some(34.0), Some(28.0)),
                    if first_id == Some(tab.id) {
                        Element::icon(Icon::ArrowUp)
                            .foreground(theme::BORDER)
                            .size(Some(24.0), Some(24.0))
                    } else {
                        hinted_icon_button(
                            format!("dock_move_up:{}", tab.id),
                            Icon::ArrowUp,
                            "Subir",
                            theme::BACKGROUND,
                            theme::FOREGROUND_SUBTLE,
                            24.0,
                        )
                    },
                    if last_id == Some(tab.id) {
                        Element::icon(Icon::ArrowDown)
                            .foreground(theme::BORDER)
                            .size(Some(24.0), Some(24.0))
                    } else {
                        hinted_icon_button(
                            format!("dock_move_down:{}", tab.id),
                            Icon::ArrowDown,
                            "Bajar",
                            theme::BACKGROUND,
                            theme::FOREGROUND_SUBTLE,
                            24.0,
                        )
                    },
                    if tab.closable {
                        hinted_icon_button(
                            format!("dock_close:{}", tab.id),
                            Icon::Close,
                            "Cerrar pestaña",
                            theme::BACKGROUND,
                            theme::FOREGROUND_SUBTLE,
                            28.0,
                        )
                    } else {
                        Element::spacer().size(Some(28.0), Some(28.0))
                    },
                ])
                .gap(4.0)
                .size(None, Some(28.0)),
            );
        }
        tabs.push(Element::spacer().size(None, Some(8.0)));
        tabs.push(eyebrow("RECIENTES (MÁX. 8)"));
        if filtered_recent.is_empty() {
            tabs.push(
                label(
                    if state.dock_recently_closed.is_empty() {
                        "No hay pestañas recientes"
                    } else {
                        "Sin coincidencias recientes"
                    },
                    theme::FOREGROUND_SUBTLE,
                )
                .font_size(11.0)
                .padding(8.0),
            );
        }
        for tab in filtered_recent {
            let row = if tab.restorable {
                Button::new(
                    format!("dock_reopen:{}", tab.id),
                    shorten(&tab.title, 25),
                    theme::action_palette(theme::BACKGROUND, theme::FOREGROUND),
                )
                .icon(Icon::History)
                .variant(ButtonVariant::Subtle)
                .render()
                .font_size(12.0)
                .min_size(0.0, 0.0)
                .size(None, Some(28.0))
            } else {
                label(
                    format!("{} · sesión cerrada", shorten(&tab.title, 25)),
                    theme::FOREGROUND_SUBTLE,
                )
                .font_size(11.0)
                .size(None, Some(28.0))
            };
            tabs.push(row);
        }
        tabs.push(Element::spacer().size(None, Some(8.0)));
        tabs.push(eyebrow("ABRIR PESTAÑA"));
        tabs.push(dock_open_actions((width - 24.0).max(150.0)));
        Element::column(tabs).gap(5.0).padding(12.0)
    } else {
        match state.side_tab {
            SideTab::Context => context_content(state),
            SideTab::Files => file_tree_content(state, width),
            SideTab::Activity => activity_content(state),
            SideTab::CodeReview => code_review_content(state, width),
            SideTab::Terminal => terminal_content(state, width),
            SideTab::FilePreview => {
                if let Some(preview) = &state.file_preview {
                    file_preview_content(preview, width)
                } else {
                    label("Selecciona un archivo", theme::FOREGROUND_SUBTLE).padding(12.0)
                }
            }
        }
    };
    let body = if (state.dock_tabs.is_empty() && !state.show_dock_overview)
        || (!state.show_dock_overview
            && matches!(state.side_tab, SideTab::Files | SideTab::Terminal))
    {
        // Files owns its virtual list; Terminal displays a bounded PTY screen
        // and keeps its command line visible below the screen.
        body.flex(1.0)
    } else {
        // Each surface keeps its own scroll offset. In particular, opening
        // the overview must reveal its search field even after scrolling a
        // long activity or preview pane.
        let scroll_id = if state.show_dock_overview {
            "side_pane_overview"
        } else {
            match state.side_tab {
                SideTab::Context => "side_pane_context",
                SideTab::Activity => "side_pane_activity",
                SideTab::CodeReview => "side_pane_review",
                SideTab::FilePreview => "side_pane_preview",
                SideTab::Files | SideTab::Terminal => unreachable!(),
            }
        };
        ScrollArea::new(scroll_id, body)
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .flex(1.0)
    };
    Element::column(vec![header, body]).size(Some(width), None)
}

/// The center and right dock share one frame. Their entire separation is a
/// single rule, while the draggable hit target extends on either side of it.
fn workspace_divider() -> Element {
    Element::spacer()
        .size(Some(RIGHT_DIVIDER), None)
        .background(theme::BORDER)
}

pub fn build_view(state: &ViewState, viewport_width: f32) -> Element {
    let layout = dock_layout(state, viewport_width);
    let left = layout.left;
    let left_visible = left > 1.0;
    let center_min_with_right = if state.side_tab == SideTab::Files {
        MIN_CENTER_COMPACT
    } else {
        MIN_CENTER_WITH_RIGHT
    };
    let side_available = viewport_width
        >= if state.sidebar_collapsed {
            2.0 * GAP + RIGHT_DIVIDER + MIN_RIGHT_DOCK + center_min_with_right
        } else {
            3.0 * GAP + RIGHT_DIVIDER + 200.0 + MIN_RIGHT_DOCK + center_min_with_right
        };
    let side_visible = layout.right.is_some();
    let center_width = layout.center;
    let compact = center_width < 460.0;
    let is_home = state.main_page == MainPage::Workspace
        && state.selected_session.is_none()
        && state.transcript.is_empty()
        && state.pending_permissions.is_empty()
        && !state.show_settings;
    let is_session = state.main_page == MainPage::Workspace
        && !state.show_settings
        && state.selected_session.is_some();
    let center = if !state.show_settings && state.main_page == MainPage::Automations {
        automation_view::build_automations(&state.automations, center_width)
    } else if !state.show_settings && state.main_page == MainPage::Marketplace {
        marketplace_view::marketplace_page(&state.marketplace, center_width)
    } else if is_home {
        home::build_home(state, center_width)
    } else if is_session {
        crate::session_surface::build_session_surface(
            state,
            center_width,
            side_visible,
            side_available,
        )
    } else {
        let mut center_items = if state.show_settings {
            Vec::new()
        } else {
            vec![workspace_header(
                state,
                side_available,
                side_visible,
                center_width,
            )]
        };
        if state.show_settings {
            center_items.push(settings_ui::build_settings_page(
                state,
                state.settings_page,
                center_width,
                left_visible,
            ));
        } else {
            center_items.push(timeline(state, side_visible, compact));
            if state.show_jump_to_bottom {
                center_items.push(
                    Element::row(vec![
                        Element::spacer().flex(1.0),
                        icon_button(
                            "jump_to_bottom",
                            Icon::ArrowDown,
                            "Volver al final",
                            theme::CARD,
                            theme::FOREGROUND,
                        )
                        .size(Some(130.0), Some(28.0)),
                    ])
                    .padding(8.0)
                    .size(None, Some(36.0)),
                );
            }
        }
        for permission in state.pending_permissions.iter().take(3) {
            center_items.push(permission_card(permission, compact).padding(8.0));
        }
        if let Some(cards) = crate::session_surface::related_subagent_cards(state, compact) {
            center_items.push(cards);
        }
        if !state.show_settings {
            center_items.push(composer(state, center_width));
        }
        Element::column(center_items)
    }
    .flex(1.0);
    let mut docks = Vec::new();
    if left_visible {
        docks.push(
            if state.show_settings {
                settings_ui::build_settings_sidebar(state, left)
            } else {
                sidebar(state, left)
            }
            .clip_children(),
        );
    }
    let center = if is_home && !side_visible && side_available {
        let toggle = Tooltip::new(
            icon_button(
                "toggle_side_pane_header",
                Icon::PanelRightOpen,
                "",
                theme::BACKGROUND,
                theme::FOREGROUND_SUBTLE,
            )
            .size(Some(32.0), Some(32.0)),
            "toggle_side_pane_header",
            "Mostrar panel derecho",
            theme::component_palette(),
        )
        .placement(TooltipPlacement::Below)
        .render();
        Element::overlay(
            center,
            Element::column(vec![
                Element::row(vec![Element::spacer().flex(1.0), toggle])
                    .padding(8.0)
                    .size(None, Some(48.0)),
                Element::spacer().flex(1.0),
            ]),
        )
        .flex(1.0)
    } else {
        center
    };
    let center = if !left_visible
        && !state.show_settings
        && (is_home || state.main_page != MainPage::Workspace)
    {
        // A free overlay shares the center dock's full bounds. Its small
        // top-left action has no height in normal layout, so closing the
        // sidebar only changes the dock's width.
        Element::overlay(
            center,
            Element::column(vec![Element::panel(sidebar_reopen_control(
                theme::BACKGROUND,
            ))
            .padding(4.0)
            .size(Some(40.0), Some(40.0))]),
        )
        .flex(1.0)
    } else {
        center
    };
    let mut workspace = vec![center];
    if let Some(right) = layout.right {
        workspace.push(workspace_divider());
        workspace.push(side_pane(state, right).clip_children());
    }
    docks.push(
        Element::row(workspace)
            .background(theme::BACKGROUND)
            .border(theme::BORDER, 1.0)
            .radius(WORKSPACE_RADIUS)
            .flex(1.0),
    );
    let base = Element::row(docks)
        .gap(GAP)
        .padding(GAP)
        .background(theme::WINDOW);
    let base = confirmation_dialog(state, base, viewport_width);
    let base = settings_ui::mcp_editor_modal(state, base, viewport_width);
    let base = settings_ui::hook_editor_modal(state, base, viewport_width);
    let base = settings_ui::exec_rule_editor_modal(state, base, viewport_width);
    let base = settings_ui::exec_rule_reset_modal(state, base, viewport_width);
    settings_ui::native_skill_reset_modal(state, base, viewport_width)
}

fn confirmation_dialog(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    let width = (viewport_width - 24.0).min(440.0);
    if let Some(id) = state.renaming_session_id.as_deref() {
        if state.sessions.iter().any(|session| session.id == id) {
            return AlertDialog::new(
                base,
                true,
                "Renombrar tarea",
                "Escribe un título para esta tarea.",
                format!("rename_commit:{id}"),
                "rename_cancel",
                theme::component_palette(),
            )
            .width(width)
            .confirm_label("Guardar")
            .body(
                field("rename_title", &state.rename_draft, "Nombre de la tarea")
                    .font_size(14.0)
                    .size(Some((width - 48.0).max(0.0)), Some(40.0)),
            )
            .render();
        }
    }
    if let Some(id) = state.group_rename_id.as_deref() {
        if state
            .session_groups
            .groups
            .iter()
            .any(|group| group.id == id)
        {
            return AlertDialog::new(
                base,
                true,
                "Renombrar grupo",
                "Escribe un nombre para este grupo.",
                format!("session_group_rename_commit:{id}"),
                "session_group_rename_cancel",
                theme::component_palette(),
            )
            .width(width)
            .confirm_label("Guardar")
            .body(
                field(
                    "group_rename_draft",
                    &state.group_rename_draft,
                    "Nombre del grupo",
                )
                .font_size(14.0)
                .size(Some((width - 48.0).max(0.0)), Some(40.0)),
            )
            .render();
        }
    }
    let dialog = if let Some(id) = state.pending_native_command_termination.as_deref() {
        state.native_commands.iter().find(|command| command.id == id && command.exit.is_none()).map(|command| {
            (
                "¿Terminar este comando?".to_owned(),
                format!("Se cerrará el proceso {} y sus descendientes. La salida ya producida quedará disponible para el agente.", shorten(&command.id, 18)),
                format!("native_command_terminate_confirm:{id}"),
                "native_command_terminate_cancel".to_owned(),
                "Terminar proceso".to_owned(),
                true,
            )
        })
    } else if let Some(id) = state.pending_mcp_delete_id.as_deref() {
        state.mcp_servers.iter().find(|server| server.id == id).map(|server| {
            let mut description = "Se quitará de la configuración de este proyecto. Podrás volver a agregarlo más adelante.".to_owned();
            if state.settings_notice_error {
                if let Some(error) = &state.settings_notice {
                    description.push_str("\n\n");
                    description.push_str(error);
                }
            }
            (
                format!("¿Eliminar el servidor MCP «{}»?", server.id),
                description,
                format!("mcp_delete_confirm:{id}"),
                "mcp_delete_cancel".to_owned(),
                "Eliminar servidor".to_owned(),
                true,
            )
        })
    } else if let Some(id) = state.marketplace.pending_uninstall_id.as_deref() {
        state.marketplace.snapshot.items.iter().find(|item| item.id == id).map(|item| {
            (
                format!("¿Desinstalar «{}»?", item.name),
                "La skill se quitará de su ubicación instalada. Podrás volver a instalarla desde Marketplace.".to_owned(),
                format!("marketplace_uninstall_confirm:{id}"),
                "marketplace_alert_cancel".to_owned(),
                "Desinstalar".to_owned(),
                true,
            )
        })
    } else if let Some(id) = state.marketplace.pending_remove_source_id.as_deref() {
        state.marketplace.snapshot.sources.iter().find(|source| source.id == id).map(|source| {
            (
                format!("¿Quitar el origen «{}»?", source.name),
                "Este origen dejará de aparecer en Marketplace. Podrás volver a agregarlo más adelante.".to_owned(),
                format!("marketplace_remove_source_confirm:{id}"),
                "marketplace_alert_cancel".to_owned(),
                "Quitar origen".to_owned(),
                true,
            )
        })
    } else if let Some(id) = state.automations.pending_delete_id.as_deref() {
        state
            .automations
            .items
            .iter()
            .find(|item| item.id == id)
            .map(|item| {
                (
                    format!("¿Eliminar «{}»?", item.title),
                    "Se eliminará esta automatización y su programación. Esta acción no se puede deshacer.".to_owned(),
                    format!("automation_delete_confirm:{id}"),
                    "automation_delete_cancel".to_owned(),
                    "Eliminar".to_owned(),
                    true,
                )
            })
    } else if let Some(id) = state.pending_delete_session_id.as_deref() {
        state.sessions.iter().find(|session| session.id == id).map(|session| {
            (
                format!("¿Eliminar «{}»?", session.title),
                "Se eliminará la tarea archivada, sus subagentes y su historial. Esta acción no se puede deshacer.".to_owned(),
                format!("delete_confirm:{id}"),
                "delete_cancel".to_owned(),
                "Eliminar".to_owned(),
                true,
            )
        })
    } else if let Some(id) = state.pending_delete_group_id.as_deref() {
        state.session_groups.groups.iter().find(|group| group.id == id).map(|group| {
            (
                format!("¿Eliminar el grupo «{}»?", group.title),
                "Las tareas del grupo seguirán disponibles sin agrupar. Esta acción no se puede deshacer.".to_owned(),
                format!("session_group_delete_confirm:{id}"),
                "session_group_menu_dismiss".to_owned(),
                "Eliminar grupo".to_owned(),
                true,
            )
        })
    } else if let Some(id) = state.pending_archive_session_id.as_deref() {
        state
            .sessions
            .iter()
            .find(|session| session.id == id)
            .map(|session| {
                (
                    format!("¿Archivar «{}»?", session.title),
                    "La tarea saldrá de la lista activa. Podrás restaurarla desde Archivadas."
                        .to_owned(),
                    format!("archive_confirm:{id}"),
                    "archive_cancel".to_owned(),
                    "Archivar".to_owned(),
                    false,
                )
            })
    } else {
        None
    };
    let Some((title, description, confirm_id, cancel_id, confirm_label, destructive)) = dialog
    else {
        return base;
    };
    let mut alert = AlertDialog::new(
        base,
        true,
        title,
        description,
        confirm_id,
        cancel_id,
        theme::component_palette(),
    )
    .width((viewport_width - 24.0).min(440.0))
    .confirm_label(confirm_label);
    if destructive {
        alert = alert.destructive(theme::DESTRUCTIVE);
    }
    alert.render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_review::ReviewSection;
    use openide_ui::{DrawCommand, Key, Point, Size, UiAction, UiEvent, UiTree};
    use serde_json::json;

    #[test]
    fn activity_lists_and_can_revoke_session_approval_grants() {
        let mut state = empty_state();
        state.selected_session = Some("session-1".into());
        state.selected_backend = "native".into();
        let digest = "a".repeat(64);
        state.session_approval_grants = vec![SessionApprovalGrantView {
            tool_name: "run_command".into(),
            key_digest: digest.clone(),
        }];
        let frame = UiTree::new(activity_content(&state)).layout(Size::new(360.0, 700.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| { region.id == format!("session_approval_revoke:{digest}") }));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "run_command"
        )));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Revocar"
        )));
    }

    fn empty_state() -> ViewState {
        ViewState {
            sessions_status: None,
            session_plans: BTreeMap::new(),
            session_recovery: BTreeMap::new(),
            pending_permissions: Vec::new(),
            pending_subagent_permissions: Vec::new(),
            active_subagents: Vec::new(),
            recoverable_subagent: None,
            pending_tool_verification: None,
            pending_batch_discard: None,
            tool_verification_output: String::new(),
            tool_verification_is_error: false,
            workspace: "C:/project".into(),
            workspace_draft: "C:/project".into(),
            project_branch: None,
            prompt: String::new(),
            composer_attachments: Vec::new(),
            composer_add_menu_open: false,
            composer_listening: false,
            composer_goal: None,
            composer_goal_editor_open: false,
            composer_goal_draft: String::new(),
            composer_project_menu_open: false,
            composer_project_options: Vec::new(),
            endpoint: String::new(),
            model: String::new(),
            native_provider_id: "custom".into(),
            hosted_web_search_mode: None,
            catalog: None,
            catalog_status: String::new(),
            catalog_refreshing: false,
            catalog_browse_provider: "openai".into(),
            catalog_provider_query: String::new(),
            catalog_model_query: String::new(),
            backend_options: vec![("native".into(), "Nativo".into())],
            selected_backend: "native".into(),
            backend_menu_open: false,
            composer_model: "gpt-4.1".into(),
            composer_model_options: vec![("gpt-4.1".into(), "gpt-4.1".into())],
            composer_routes: Vec::new(),
            composer_provider_status: BTreeMap::new(),
            composer_model_menu_open: false,
            composer_model_list_open: false,
            composer_model_query: String::new(),
            composer_advanced_open: false,
            composer_effort: "default".into(),
            composer_effort_options: Vec::new(),
            composer_effort_menu_open: false,
            composer_effort_preview: None,
            composer_effort_hovered: false,
            composer_context_percent: None,
            turn_mode: TurnMode::Build,
            sessions: Vec::new(),
            selected_session: None,
            selected_session_title: None,
            file_rows: Vec::new(),
            files_status: String::new(),
            file_root_can_load_more: false,
            show_archived: false,
            session_organization: SessionOrganization::Project,
            session_sort: SessionSort::Updated,
            session_sort_menu_open: false,
            collapsed_projects: BTreeSet::new(),
            session_groups: SessionGroups::default(),
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
            transcript: Vec::new(),
            running: false,
            background_run_selected: false,
            status: "Listo".into(),
            settings_notice: None,
            settings_notice_error: false,
            native_key_available: false,
            workspace_writes_enabled: false,
            native_tool_permissions: ToolPermissions::default(),
            native_command_isolation: if cfg!(windows) {
                CommandIsolation::WorkspaceWriteOffline
            } else {
                CommandIsolation::HostUnrestricted
            },
            session_approval_grants: Vec::new(),
            native_commands: Vec::new(),
            native_commands_error: None,
            pending_native_command_termination: None,
            native_command_terminating: None,
            native_hooks: Default::default(),
            native_exec_rules: Default::default(),
            native_skills: Default::default(),
            settings_reduced_motion: false,
            settings_reopen_last_workspace: false,
            settings_terminal_shell: TerminalShell::System,
            settings_terminal_shell_draft: String::new(),
            mcp_servers: Vec::new(),
            mcp_selected_id: None,
            mcp_editor_open: false,
            pending_mcp_delete_id: None,
            mcp_draft_id: String::new(),
            mcp_draft_http: false,
            mcp_draft_url: String::new(),
            mcp_draft_http_bearer_token_env_name: String::new(),
            mcp_draft_oauth_enabled: false,
            mcp_draft_oauth_client_id: String::new(),
            mcp_draft_oauth_issuer: String::new(),
            mcp_draft_oauth_scopes: String::new(),
            mcp_oauth_status: None,
            mcp_draft_program: String::new(),
            mcp_draft_args: "[]".into(),
            mcp_draft_cwd: String::new(),
            mcp_draft_env_names: String::new(),
            mcp_load_error: None,
            show_settings: false,
            main_page: MainPage::Workspace,
            automations: AutomationViewState::new(
                "C:/project",
                vec![("native".into(), "Nativo".into())],
                1_000_000,
            ),
            marketplace: MarketplaceViewState::default(),
            settings_page: SettingsPage::General,
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
            show_side_pane: true,
            sidebar_collapsed: false,
            left_dock_width: None,
            right_dock_width: None,
            left_presentation_width: None,
            right_presentation_width: None,
            side_tab: SideTab::Context,
            activity_page: 0,
            review_page: 0,
            dock_tabs: vec![
                DockTabView {
                    id: 1,
                    title: "Contexto".into(),
                    active: true,
                    kind: SideTab::Context,
                    closable: false,
                },
                DockTabView {
                    id: 2,
                    title: "Actividad".into(),
                    active: false,
                    kind: SideTab::Activity,
                    closable: false,
                },
            ],
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
            selected_tool_id: None,
            show_jump_to_bottom: false,
            github_account: GithubAccountStatus::Disconnected,
            github_account_menu_open: false,
        }
    }

    #[test]
    fn fallback_transcript_uses_shared_composer_at_narrow_and_wide_widths() {
        let mut state = empty_state();
        state.transcript.push(TranscriptItem::User {
            content: "Primer mensaje".into(),
            images: Vec::new(),
        });
        state.prompt = "Siguiente paso".into();
        for viewport in [620.0, 1000.0] {
            let frame =
                UiTree::new(build_view(&state, viewport)).layout(Size::new(viewport, 700.0));
            let card = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::FillRect {
                        rect,
                        color,
                        radius,
                    } if *color == Color::rgb(38, 38, 38) && *radius == 26.0 => Some(*rect),
                    _ => None,
                })
                .expect("shared composer card");
            for id in [
                "prompt",
                "run_toggle",
                "composer_model_toggle",
                "composer_add_context",
            ] {
                let region = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap_or_else(|| panic!("missing {id} at {viewport}"));
                assert!(region.rect.x >= card.x, "{id} at {viewport}");
                assert!(
                    region.rect.x + region.rect.width <= card.x + card.width,
                    "{id} at {viewport}"
                );
            }
        }
    }

    #[test]
    fn github_footer_keeps_account_and_settings_hitboxes_separate() {
        let mut state = empty_state();
        for status in [
            GithubAccountStatus::Disconnected,
            GithubAccountStatus::Connected(crate::github_account::GithubIdentity {
                login: "nihue".to_owned(),
            }),
        ] {
            state.github_account = status;
            let frame =
                UiTree::new(sidebar_account_footer(&state, 264.0)).layout(Size::new(264.0, 52.0));
            let account_id = if matches!(state.github_account, GithubAccountStatus::Connected(_)) {
                "github_account_open"
            } else {
                "github_account_connect"
            };
            let account = frame
                .regions
                .iter()
                .find(|region| region.id == account_id)
                .expect("account control");
            let settings = frame
                .regions
                .iter()
                .find(|region| region.id == "toggle_settings")
                .expect("settings control");
            assert!(account.rect.x + account.rect.width <= settings.rect.x);
            assert_eq!(settings.rect.width, 36.0);
            assert_eq!(settings.rect.height, 36.0);
            let gear = frame.commands.iter().find_map(|command| match command {
                DrawCommand::Icon {
                    icon: Icon::Settings,
                    rect,
                    ..
                } => Some(rect),
                _ => None,
            });
            let gear = gear.expect("settings icon");
            assert!((gear.x + gear.width / 2.0 - settings.rect.x - 18.0).abs() < 1.0);
            assert!((gear.y + gear.height / 2.0 - settings.rect.y - 18.0).abs() < 1.0);
        }
    }

    #[test]
    fn sidebar_brand_and_github_align_with_navigation_at_all_widths() {
        let mut state = empty_state();
        for width in [176.0, 200.0, 264.0] {
            state.github_account = GithubAccountStatus::Disconnected;
            let frame = UiTree::new(sidebar(&state, width)).layout(Size::new(width, 620.0));
            let text_position = |value: &str| {
                frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::Text { text, position, .. } if text == value => {
                            Some(*position)
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("missing {value} at {width}"))
            };
            let title = text_position("OpenIDE");
            let nav = text_position("Nueva tarea");
            let account_label = text_position(if width < 230.0 {
                "Conectar"
            } else {
                "Conectar con GitHub"
            });
            assert!((title.x - nav.x).abs() <= 1.0, "brand x at {width}");
            assert!(
                (account_label.x - nav.x).abs() <= 1.0,
                "account label x at {width}"
            );
            let region = |id: &str| {
                frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap()
                    .rect
            };
            let toggle = region("toggle_sidebar");
            let account = region("github_account_connect");
            let github = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Icon {
                        icon: Icon::Github,
                        rect,
                        ..
                    } => Some(*rect),
                    _ => None,
                })
                .expect("GitHub icon");
            assert!(
                (title.y + 9.0 - (toggle.y + toggle.height / 2.0)).abs() <= 1.0,
                "brand y at {width}"
            );
            assert!(
                (github.x + github.width / 2.0 - (toggle.x + toggle.width / 2.0)).abs() <= 1.0,
                "GitHub x at {width}"
            );
            assert!(
                (github.y + github.height / 2.0 - (account.y + account.height / 2.0)).abs() <= 1.0,
                "GitHub y at {width}"
            );
        }
        state.github_account =
            GithubAccountStatus::Connected(crate::github_account::GithubIdentity {
                login: "nihue".to_owned(),
            });
        let frame =
            UiTree::new(sidebar_account_footer(&state, 176.0)).layout(Size::new(176.0, 52.0));
        let account = frame
            .regions
            .iter()
            .find(|region| region.id == "github_account_open")
            .unwrap()
            .rect;
        let github = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Icon {
                    icon: Icon::Github,
                    rect,
                    ..
                } => Some(*rect),
                _ => None,
            })
            .expect("connected GitHub icon");
        assert!((github.y + github.height / 2.0 - (account.y + account.height / 2.0)).abs() <= 1.0);
    }

    #[test]
    fn github_footer_requires_an_explicit_choice_for_multiple_cached_accounts() {
        let mut state = empty_state();
        state.github_account = GithubAccountStatus::Unverified {
            accounts: vec!["alice".into(), "bob".into()],
            notice: None,
        };
        state.github_account_menu_open = true;
        let frame =
            UiTree::new(sidebar_account_footer(&state, 264.0)).layout(Size::new(264.0, 300.0));
        for id in [
            "github_account_menu_toggle",
            "github_account_verify:alice",
            "github_account_verify:bob",
            "github_account_connect",
        ] {
            assert!(frame.regions.iter().any(|region| region.id == id), "{id}");
        }
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "github_account_open"));
    }

    #[test]
    fn review_line_preserves_gutters_and_clips_to_narrow_dock_width() {
        let line = ReviewLine {
            kind: ReviewLineKind::Added,
            text: "+a very long line of source code".into(),
            old_number: None,
            new_number: Some(120),
        };
        let rendered = compact_review_line(&line, 21);
        assert!(rendered.chars().count() <= 22);
        assert!(rendered.contains("120 +"), "{rendered:?}");
        assert!(rendered.ends_with('…'));
    }

    #[test]
    fn code_review_groups_changes_by_stage_and_file() {
        let mut state = empty_state();
        state.git_review = Some(Ok(GitReview {
            branch: "main".into(),
            files: 1,
            additions: 1,
            sections: vec![ReviewSection {
                title: "CAMBIOS PREPARADOS".into(),
                additions: 1,
                files: vec![ReviewFile {
                    path: "src/main.rs".into(),
                    additions: 1,
                    lines: vec![ReviewLine {
                        kind: ReviewLineKind::Added,
                        text: "+fn main() {}".into(),
                        new_number: Some(1),
                        old_number: None,
                    }],
                    ..ReviewFile::default()
                }],
                ..ReviewSection::default()
            }],
            ..GitReview::default()
        }));
        let frame = UiTree::new(code_review_content(&state, 360.0)).layout(Size::new(360.0, 800.0));
        let labels: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(labels.contains(&"CAMBIOS PREPARADOS"), "{labels:?}");
        assert!(labels.contains(&"CAMBIOS SIN PREPARAR"));
        assert!(labels.contains(&"src/main.rs"));
        assert!(labels.iter().any(|label| label.contains("+fn main()")));
    }

    #[test]
    fn review_pages_reach_changes_past_the_initial_limit() {
        let mut state = empty_state();
        state.git_review = Some(Ok(GitReview {
            branch: "main".into(),
            untracked: (0..45).map(|index| format!("file-{index:02}.rs")).collect(),
            ..GitReview::default()
        }));
        let first = UiTree::new(code_review_content(&state, 360.0)).layout(Size::new(360.0, 900.0));
        assert!(first
            .regions
            .iter()
            .any(|region| region.id == "review_page_next"));
        assert!(!first
            .regions
            .iter()
            .any(|region| region.id == "review_page_previous"));
        let first_text: Vec<_> = first
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(first_text.contains(&"file-00.rs"));
        assert!(!first_text.contains(&"file-20.rs"));

        state.review_page = 1;
        let second =
            UiTree::new(code_review_content(&state, 360.0)).layout(Size::new(360.0, 900.0));
        let second_text: Vec<_> = second
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(second_text.contains(&"file-20.rs"));
        assert!(!second_text.contains(&"file-00.rs"));
        assert!(second
            .regions
            .iter()
            .any(|region| region.id == "review_page_previous"));
    }

    #[test]
    fn empty_right_dock_exposes_only_real_views() {
        let mut state = empty_state();
        state.dock_tabs.clear();
        let frame = UiTree::new(side_pane(&state, 420.0)).layout(Size::new(420.0, 600.0));
        for id in [
            "dock_open:context",
            "dock_open:files",
            "dock_open:activity",
            "dock_open:review",
        ] {
            let rect = frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap()
                .rect;
            assert!(rect.y > RIGHT_HEADER_HEIGHT + 50.0);
            assert!(rect.y + rect.height < 600.0);
        }
    }

    #[test]
    fn dock_overview_filters_open_and_recent_tabs_and_keeps_reopen_reachable() {
        let mut state = empty_state();
        state.show_dock_overview = true;
        state.dock_tabs.push(DockTabView {
            id: 3,
            title: "Terminal".into(),
            active: false,
            kind: SideTab::Terminal,
            closable: true,
        });
        state.dock_recently_closed = vec![
            DockRecentlyClosedView {
                id: 7,
                title: "README.md".into(),
                kind: SideTab::FilePreview,
                restorable: true,
            },
            DockRecentlyClosedView {
                id: 8,
                title: "Terminal".into(),
                kind: SideTab::Terminal,
                restorable: false,
            },
        ];
        state.dock_overview_query = "readme".into();
        let filtered = UiTree::new(side_pane(&state, 340.0)).layout(Size::new(340.0, 600.0));
        assert!(filtered
            .regions
            .iter()
            .any(|region| region.id == "dock_reopen:7"));
        assert!(!filtered
            .regions
            .iter()
            .any(|region| region.id == "dock_move_down:3"));
        assert!(!filtered
            .regions
            .iter()
            .any(|region| region.id == "dock_reopen:8"));

        state.dock_tabs.clear();
        state.dock_overview_query.clear();
        let empty_open = UiTree::new(side_pane(&state, 340.0)).layout(Size::new(340.0, 600.0));
        assert!(empty_open
            .regions
            .iter()
            .any(|region| region.id == "dock_reopen:7"));
        assert!(empty_open
            .regions
            .iter()
            .any(|region| region.id == "dock_overview_query"));
    }

    #[test]
    fn dock_overview_search_starts_visible_after_scrolling_activity_with_twenty_tabs() {
        let mut state = empty_state();
        state.side_tab = SideTab::Activity;
        for index in 0..20 {
            state.transcript.push(TranscriptItem::Tool {
                id: format!("call-{index}"),
                name: "read_file".into(),
                arguments: json!({"path": format!("src/file-{index}.rs")}),
                output: Some(format!("result-{index}")),
                is_error: false,
            });
            state.dock_tabs.push(DockTabView {
                id: index + 10,
                title: format!("file-{index}.rs"),
                active: false,
                kind: SideTab::FilePreview,
                closable: true,
            });
        }
        let viewport = Size::new(340.0, 360.0);
        let mut tree = UiTree::new(side_pane(&state, viewport.width));
        tree.layout(viewport);
        for _ in 0..5 {
            tree.handle_event(UiEvent::Scroll {
                position: Point::new(170.0, 220.0),
                delta_y: -120.0,
            });
            tree.layout(viewport);
        }
        assert!(tree.scroll_offset("side_pane_activity") > 0.0);

        state.show_dock_overview = true;
        tree.set_root(side_pane(&state, viewport.width));
        let overview = tree.layout(viewport);
        assert_eq!(tree.scroll_offset("side_pane_overview"), 0.0);
        let search = overview
            .regions
            .iter()
            .find(|region| region.id == "dock_overview_query")
            .expect("overview search remains visible");
        assert!(search.rect.y >= RIGHT_HEADER_HEIGHT);
        assert!(search.rect.y + search.rect.height <= 160.0);
    }

    #[test]
    fn desktop_width_shows_three_docks_and_keeps_submit_visible() {
        let mut state = empty_state();
        state.prompt = "Iniciar tarea".into();
        let frame = UiTree::new(build_view(&state, 1200.0)).layout(Size::new(1200.0, 620.0));
        let region = |id: &str| frame.regions.iter().find(|region| region.id == id).unwrap();
        let sidebar = region("new_session_sidebar");
        let prompt = region("prompt");
        let submit = region("run_toggle");
        let side_tab = region("dock_tab:1");
        assert!(sidebar.rect.x < prompt.rect.x);
        assert!(prompt.rect.x < side_tab.rect.x);
        assert!(submit.rect.x + submit.rect.width < side_tab.rect.x);
        assert!(submit.rect.y + submit.rect.height <= 620.0);
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("starter:")));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, rect, .. }
                if *color == theme::SIDEBAR && rect.x == 4.0 && rect.width == 264.0
        )));
    }

    #[test]
    fn dock_widths_resize_with_center_minimum_and_collapse_on_narrow_viewport() {
        let mut state = empty_state();
        state.left_dock_width = Some(320.0);
        state.right_dock_width = Some(400.0);
        let desktop = dock_layout(&state, 1200.0);
        assert_eq!(desktop.left, 320.0);
        assert_eq!(desktop.right, Some(387.0));
        assert_eq!(desktop.center, MIN_CENTER_WITH_RIGHT);
        let reduced = dock_layout(
            &state,
            3.0 * GAP + RIGHT_DIVIDER + 200.0 + MIN_RIGHT_DOCK + MIN_CENTER_WITH_RIGHT,
        );
        assert_eq!(reduced.left, 200.0);
        assert_eq!(reduced.right, Some(MIN_RIGHT_DOCK));
        assert_eq!(reduced.center, MIN_CENTER_WITH_RIGHT);
        let two_dock = dock_layout(&state, 900.0);
        assert_eq!(two_dock.left, 320.0);
        assert_eq!(two_dock.right, None);
        assert_eq!(two_dock.center, 568.0);
        state.side_tab = SideTab::Files;
        let files_dock = dock_layout(&state, 900.0);
        assert!(files_dock.right.is_some());
        assert!(files_dock.center >= MIN_CENTER_COMPACT);
        state.side_tab = SideTab::Context;
        let compact = dock_layout(&state, 520.0);
        assert_eq!(compact.left, 188.0);
        assert_eq!(compact.right, None);
        assert_eq!(compact.center, 320.0);
    }

    #[test]
    fn center_and_right_share_one_rounded_frame_with_one_pixel_divider() {
        let state = empty_state();
        let viewport = Size::new(1200.0, 700.0);
        let layout = dock_layout(&state, viewport.width);
        let right = layout.right.expect("right dock fits at desktop width");
        let frame = UiTree::new(build_view(&state, viewport.width)).layout(viewport);
        let workspace_x = GAP + layout.left + GAP;
        let workspace_width = layout.center + RIGHT_DIVIDER + right;
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::StrokeRect { rect, color, radius, .. }
                if *color == theme::BORDER
                    && (*radius - WORKSPACE_RADIUS).abs() < 0.01
                    && (rect.x - workspace_x).abs() < 0.01
                    && (rect.width - workspace_width).abs() < 0.01
        )));
        let split = layout.right_split_x.expect("visible divider");
        let right_edge = viewport.width - GAP - right;
        assert!((split + RIGHT_DIVIDER / 2.0 - right_edge).abs() < 0.01);
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, .. }
                if *color == theme::BORDER
                    && (rect.x - (split - 0.5)).abs() < 0.01
                    && (rect.width - 1.0).abs() < 0.01
                    && (rect.height - (viewport.height - 2.0 * GAP)).abs() < 0.01
        )));
        assert!(!frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::StrokeRect { rect, radius, .. }
                if (*radius - 6.0).abs() < 0.01
                    && (rect.x - (viewport.width - GAP - right)).abs() < 0.01
                    && (rect.width - right).abs() < 0.01
        )));
    }

    #[test]
    fn right_header_pages_tabs_without_overlapping_actions_at_compact_widths() {
        let mut state = empty_state();
        state.dock_tabs.insert(
            1,
            DockTabView {
                id: 3,
                title: "Archivos".into(),
                active: false,
                kind: SideTab::Files,
                closable: false,
            },
        );
        state.dock_tabs.push(DockTabView {
            id: 4,
            title: "Cambios".into(),
            active: false,
            kind: SideTab::CodeReview,
            closable: false,
        });
        for (width, visible) in [(320.0, 1), (430.0, 2), (540.0, 3)] {
            let frame = UiTree::new(side_pane(&state, width)).layout(Size::new(width, 360.0));
            for id in [
                "dock_overview_toggle",
                "dock_add_tab",
                "toggle_side_pane",
                "dock_tab_page_next",
            ] {
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap_or_else(|| panic!("missing right header action: {id}"))
                    .rect;
                assert!(
                    rect.x >= 0.0 && rect.x + rect.width <= width,
                    "{id}: {rect:?}"
                );
                assert!(rect.y >= 0.0 && rect.y + rect.height <= RIGHT_HEADER_HEIGHT);
            }
            for (index, tab) in state.dock_tabs.iter().enumerate() {
                assert_eq!(
                    frame
                        .regions
                        .iter()
                        .any(|region| region.id == format!("dock_tab:{}", tab.id)),
                    index < visible,
                    "width={width} tab={} index={index}",
                    tab.id,
                );
            }
        }
        let width = MIN_RIGHT_DOCK;
        let frame = UiTree::new(side_pane(&state, width)).layout(Size::new(width, 360.0));
        // The shared rounded workspace paints the strip. An opaque child
        // rectangle here would square off its top-right corner.
        assert!(!frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, .. }
                if *color == theme::BACKGROUND
                    && rect.x == 0.0
                    && rect.y == 0.0
                    && (rect.width - width).abs() < 0.01
                    && (rect.height - RIGHT_HEADER_HEIGHT).abs() < 0.01
        )));
        assert!(!frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, .. }
                if *color == theme::BORDER
                    && rect.y > 0.0
                    && rect.y < RIGHT_HEADER_HEIGHT
                    && rect.height <= 1.0
                    && rect.width >= width - 2.0
        )));
    }

    #[test]
    fn dock_add_tab_uses_a_compact_anchored_popover_with_real_views() {
        let mut state = empty_state();
        let viewport = Size::new(320.0, 440.0);
        let closed = UiTree::new(side_pane(&state, viewport.width)).layout(viewport);
        assert!(closed
            .regions
            .iter()
            .any(|region| region.id == "dock_add_tab"));
        assert!(!closed
            .regions
            .iter()
            .any(|region| region.id == "dock_open:terminal"));

        state.show_dock_add_menu = true;
        let mut tree = UiTree::new(side_pane(&state, viewport.width));
        let opened = tree.layout(viewport);
        let anchor = opened
            .regions
            .iter()
            .find(|region| region.id == "dock_add_tab")
            .expect("add-tab button")
            .rect;
        let popup = opened
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:dock_add_dismiss")
            .expect("anchored popover")
            .rect;
        assert!(popup.y >= anchor.y + anchor.height);
        assert!(popup.x >= 0.0 && popup.x + popup.width <= viewport.width);
        assert!(popup.height < 230.0, "add menu should not become a page");
        for id in [
            "dock_open:terminal",
            "dock_open:context",
            "dock_open:files",
            "dock_open:activity",
            "dock_open:review",
        ] {
            let item = opened
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap_or_else(|| panic!("missing add-menu view: {id}"));
            assert!(
                item.rect.x >= popup.x && item.rect.x + item.rect.width <= popup.x + popup.width
            );
        }
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Escape,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked {
                id: "dock_add_dismiss".to_owned(),
            }]
        );
    }

    fn assert_dock_icon_hover_geometry(
        root: Element,
        viewport: Size,
        id: &str,
        icon: Icon,
        target_side: f32,
        hover_color: Color,
    ) {
        let mut tree = UiTree::new(root);
        tree.set_reduced_motion(true);
        let base = tree.layout(viewport);
        let region = base
            .regions
            .iter()
            .find(|region| region.id == id)
            .unwrap_or_else(|| panic!("missing dock icon action: {id}"))
            .rect;
        assert!(
            (region.width - target_side).abs() < 0.01,
            "{id}: {region:?}"
        );
        assert!(
            (region.height - target_side).abs() < 0.01,
            "{id}: {region:?}"
        );
        let glyph = base
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Icon {
                    icon: actual, rect, ..
                } if *actual == icon
                    && rect.x >= region.x
                    && rect.y >= region.y
                    && rect.x + rect.width <= region.x + region.width
                    && rect.y + rect.height <= region.y + region.height =>
                {
                    Some(*rect)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing centered glyph for {id}"));
        assert!(
            (glyph.x + glyph.width / 2.0 - region.x - region.width / 2.0).abs() < 0.01,
            "horizontal center: {id}, region={region:?}, glyph={glyph:?}"
        );
        assert!(
            (glyph.y + glyph.height / 2.0 - region.y - region.height / 2.0).abs() < 0.01,
            "vertical center: {id}, region={region:?}, glyph={glyph:?}"
        );
        let center = Point::new(
            region.x + region.width / 2.0,
            region.y + region.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerMove(center));
        let hovered = tree.layout(viewport);
        assert_eq!(hovered.hit_test(center), Some(id));
        assert!(
            hovered.commands.iter().any(|command| matches!(
                command,
                DrawCommand::FillRect { rect, color, .. }
                    if *rect == region && *color == hover_color
            )),
            "hover fill differs from hitbox for {id}"
        );
        tree.handle_event(UiEvent::PointerDown(center));
        let pressed = tree.layout(viewport);
        assert_eq!(pressed.hit_test(center), Some(id));
        assert!(
            pressed.commands.iter().any(|command| matches!(
                command,
                DrawCommand::FillRect { rect, color, .. }
                    if *rect == region && *color == theme::SELECTED
            )),
            "pressed fill differs from hitbox for {id}"
        );
    }

    #[test]
    fn icon_only_hover_and_hitbox_center_in_both_sidebar_docks() {
        let state = empty_state();
        assert_dock_icon_hover_geometry(
            sidebar(&state, 280.0),
            Size::new(280.0, 500.0),
            "toggle_sidebar",
            Icon::PanelLeftClose,
            32.0,
            theme::hover_for(theme::SIDEBAR),
        );
        assert_dock_icon_hover_geometry(
            sidebar(&state, 280.0),
            Size::new(280.0, 500.0),
            "session_sort_toggle",
            Icon::Filter,
            28.0,
            theme::hover_for(theme::SIDEBAR),
        );
        assert_dock_icon_hover_geometry(
            sidebar(&state, 280.0),
            Size::new(280.0, 500.0),
            "toggle_settings",
            Icon::Settings,
            36.0,
            theme::hover_for(theme::SIDEBAR),
        );
        assert_dock_icon_hover_geometry(
            side_pane(&state, 430.0),
            Size::new(430.0, 500.0),
            "dock_overview_toggle",
            Icon::Menu,
            32.0,
            theme::hover_for(theme::BACKGROUND),
        );
        assert_dock_icon_hover_geometry(
            side_pane(&state, 430.0),
            Size::new(430.0, 500.0),
            "toggle_side_pane",
            Icon::PanelRightClose,
            32.0,
            theme::hover_for(theme::BACKGROUND),
        );
    }

    #[test]
    fn dock_icon_tooltips_keep_keyboard_actions_and_shared_hints() {
        let mut state = empty_state();
        let cases = [
            (
                side_pane(&state, 430.0),
                "dock_overview_toggle",
                "Ver todas las pestañas",
            ),
            (
                code_review_content(&state, 430.0),
                "review_refresh",
                "Actualizar cambios de Git",
            ),
        ];
        for (root, id, title) in cases {
            let mut tree = UiTree::new(root);
            let viewport = Size::new(430.0, 500.0);
            tree.layout(viewport);
            assert!(tree.focus(id), "{id} should remain focusable");
            let focused = tree.layout(viewport);
            assert!(
                focused.commands.iter().any(|command| matches!(
                    command,
                    DrawCommand::Text { text, .. } if text == title
                )),
                "focused {id} should show its tooltip"
            );
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false,
                }),
                vec![UiAction::Clicked { id: id.to_owned() }],
            );
        }

        state.file_preview = Some(FilePreviewView {
            tab_id: 7,
            path: "C:/project/example.rs".into(),
            page_index: 0,
            content: None,
        });
        let mut tree = UiTree::new(file_preview_content(
            state.file_preview.as_ref().unwrap(),
            430.0,
        ));
        let viewport = Size::new(430.0, 500.0);
        tree.layout(viewport);
        assert!(tree.focus("dock_close:7"));
        let focused = tree.layout(viewport);
        assert!(focused.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text == "Cerrar archivo"
        )));
    }

    #[test]
    fn home_offers_right_dock_toggle_without_moving_the_composer() {
        let mut state = empty_state();
        let viewport = Size::new(1200.0, 700.0);
        let visible = UiTree::new(build_view(&state, viewport.width)).layout(viewport);
        assert!(!visible
            .regions
            .iter()
            .any(|region| region.id == "toggle_side_pane_header"));
        assert!(visible.regions.iter().any(|region| region.id == "prompt"));
        state.show_side_pane = false;
        let collapsed = UiTree::new(build_view(&state, viewport.width)).layout(viewport);
        let toggle = collapsed
            .regions
            .iter()
            .find(|region| region.id == "toggle_side_pane_header")
            .expect("right dock reopen action")
            .rect;
        let collapsed_prompt = collapsed
            .regions
            .iter()
            .find(|region| region.id == "prompt")
            .expect("home composer")
            .rect;
        assert!(toggle.x > collapsed_prompt.x);
        assert!(
            toggle.y + toggle.height < collapsed_prompt.y,
            "toggle={toggle:?}, composer={collapsed_prompt:?}"
        );
        let center_width = dock_layout(&state, viewport.width).center;
        let standalone = UiTree::new(home::build_home(&state, center_width))
            .layout(Size::new(center_width, viewport.height - 2.0 * GAP));
        let standalone_prompt = standalone
            .regions
            .iter()
            .find(|region| region.id == "prompt")
            .expect("standalone home composer")
            .rect;
        assert!((collapsed_prompt.y - standalone_prompt.y - GAP).abs() < 0.01);
        let narrow = UiTree::new(build_view(&state, 900.0)).layout(Size::new(900.0, 700.0));
        assert!(!narrow
            .regions
            .iter()
            .any(|region| region.id == "toggle_side_pane_header"));
    }

    #[test]
    fn animated_dock_widths_clip_controls_inside_their_presentation_bounds() {
        for width in [1.25, 24.0, 80.0] {
            let mut state = empty_state();
            state.sidebar_collapsed = true;
            state.show_side_pane = false;
            state.left_presentation_width = Some(width);
            state.right_presentation_width = Some(width);
            let viewport = Size::new(1200.0, 700.0);
            let layout = dock_layout(&state, viewport.width);
            assert!((layout.left - width).abs() < 0.01);
            assert_eq!(layout.right, Some(width));
            let frame = UiTree::new(build_view(&state, viewport.width)).layout(viewport);
            let left_edge = GAP + width;
            let right_edge = viewport.width - GAP - width;
            for region in frame.regions.iter().filter(|region| {
                matches!(
                    region.id.as_str(),
                    "toggle_sidebar" | "new_session_sidebar" | "open_command_center"
                )
            }) {
                assert!(
                    region.rect.x + region.rect.width <= left_edge + 0.01,
                    "{} escapes left dock at width {width}",
                    region.id,
                );
            }
            for region in frame.regions.iter().filter(|region| {
                region.id.starts_with("dock_tab:")
                    || region.id == "toggle_side_pane"
                    || region.id == "dock_overview_toggle"
            }) {
                assert!(
                    region.rect.x >= right_edge - 0.01
                        && region.rect.x + region.rect.width <= viewport.width - GAP + 0.01,
                    "{} escapes right dock at width {width}",
                    region.id,
                );
            }
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "toggle_sidebar_reopen"));
        }
    }

    #[test]
    fn sidebar_toggle_replaces_visible_history_and_reopens_after_collapse() {
        let mut state = empty_state();
        let expanded = UiTree::new(build_view(&state, 1200.0)).layout(Size::new(1200.0, 700.0));
        assert!(expanded
            .regions
            .iter()
            .any(|region| region.id == "toggle_sidebar"));
        assert!(!expanded
            .regions
            .iter()
            .any(|region| region.id == "nav_back" || region.id == "nav_forward"));
        state.sidebar_collapsed = true;
        state.show_side_pane = false;
        let layout = dock_layout(&state, 1200.0);
        assert_eq!(layout.left, 0.0);
        assert!(layout.left_split_x.is_none());
        assert!(layout.center > 900.0);
        let collapsed = UiTree::new(build_view(&state, 1200.0)).layout(Size::new(1200.0, 700.0));
        assert!(collapsed
            .regions
            .iter()
            .any(|region| region.id == "toggle_sidebar_reopen"));
        assert!(!collapsed
            .regions
            .iter()
            .any(|region| region.id == "toggle_sidebar"));
    }

    #[test]
    fn collapsed_sidebar_preserves_home_vertical_layout_and_expands_center_dock() {
        let mut state = empty_state();
        state.prompt = "Iniciar tarea".into();
        state.show_side_pane = false;
        let viewport = Size::new(1200.0, 700.0);
        let expanded = UiTree::new(build_view(&state, viewport.width)).layout(viewport);
        state.sidebar_collapsed = true;
        let collapsed_layout = dock_layout(&state, viewport.width);
        let collapsed = UiTree::new(build_view(&state, viewport.width)).layout(viewport);
        let rect = |frame: &openide_ui::Frame, id: &str| {
            frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap()
                .rect
        };
        let before = rect(&expanded, "prompt");
        let after = rect(&collapsed, "prompt");
        assert_eq!(
            before.y, after.y,
            "the home composer must not jump vertically"
        );
        assert_eq!(
            rect(&expanded, "run_toggle").y,
            rect(&collapsed, "run_toggle").y
        );
        assert_eq!(collapsed_layout.center, viewport.width - 2.0 * GAP);
        assert!(
            after.x < before.x,
            "the centered content follows the wider dock"
        );
        let reopen = rect(&collapsed, "toggle_sidebar_reopen");
        assert!(reopen.y + reopen.height < after.y);
        assert!(reopen.x + reopen.width < after.x);
    }

    #[test]
    fn collapsed_sidebar_keeps_active_session_composer_at_bottom_and_header_in_place() {
        let mut state = empty_state();
        state.prompt = "Siguiente paso".into();
        state.selected_session = Some("active".into());
        state.selected_session_title = Some("Tarea activa".into());
        state.show_side_pane = false;
        let viewport = Size::new(1000.0, 700.0);
        let expanded = UiTree::new(build_view(&state, viewport.width)).layout(viewport);
        state.sidebar_collapsed = true;
        let collapsed = UiTree::new(build_view(&state, viewport.width)).layout(viewport);
        let rect = |frame: &openide_ui::Frame, id: &str| {
            frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap()
                .rect
        };
        let before = rect(&expanded, "prompt");
        let after = rect(&collapsed, "prompt");
        assert!(after.x < before.x);
        assert!(after.width >= before.width);
        assert_eq!(before.y, after.y, "the composer retains its bottom anchor");
        let submit = rect(&collapsed, "run_toggle");
        assert!(submit.y + submit.height <= viewport.height - GAP);
        assert!(submit.y > viewport.height / 2.0);
        let reopen = rect(&collapsed, "toggle_sidebar_reopen");
        let new_task = rect(&collapsed, "new_session_header");
        assert!(reopen.x + reopen.width <= new_task.x);
        assert_eq!(reopen.y, new_task.y);
    }

    #[test]
    fn collapsing_left_dock_preserves_open_right_dock_and_expands_session_center() {
        let mut state = empty_state();
        state.prompt = "Siguiente paso".into();
        state.selected_session = Some("active".into());
        state.show_side_pane = true;
        let viewport = Size::new(1200.0, 700.0);
        let before_layout = dock_layout(&state, viewport.width);
        let before = UiTree::new(build_view(&state, viewport.width)).layout(viewport);
        state.sidebar_collapsed = true;
        let after_layout = dock_layout(&state, viewport.width);
        let after = UiTree::new(build_view(&state, viewport.width)).layout(viewport);
        let rect = |frame: &openide_ui::Frame, id: &str| {
            frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap()
                .rect
        };
        assert!(before_layout.right.is_some() && after_layout.right.is_some());
        assert!(after_layout.center > before_layout.center);
        assert!(rect(&after, "prompt").width > rect(&before, "prompt").width);
        let before_submit = rect(&before, "run_toggle");
        let after_submit = rect(&after, "run_toggle");
        assert!(
            ((before_submit.y + before_submit.height) - (after_submit.y + after_submit.height))
                .abs()
                <= 3.0,
            "the responsive composer retains its bottom anchor within its padding change"
        );
        assert!(after.regions.iter().any(|region| region.id == "dock_tab:1"));
    }

    #[test]
    fn narrow_settings_uses_inline_navigation_and_workspace_return() {
        let mut state = empty_state();
        state.show_settings = true;
        assert_eq!(dock_layout(&state, 520.0).left, 0.0);
        let mut tree = UiTree::new(build_view(&state, 520.0));
        let frame = tree.layout(Size::new(520.0, 640.0));
        for id in ["toggle_settings", "settings_nav_toggle"] {
            assert!(
                frame.regions.iter().any(|region| region.id == id),
                "missing {id}"
            );
        }
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "toggle_sidebar_reopen"));
        assert!(settings_action_reachable(
            &mut tree,
            "workspace_apply",
            520.0,
            640.0
        ));
        state.settings_nav_open = true;
        let mut expanded = UiTree::new(build_view(&state, 520.0));
        for id in ["settings_page:providers", "settings_page:skills"] {
            assert!(settings_action_reachable(&mut expanded, id, 520.0, 640.0));
        }
    }

    fn settings_action_reachable(tree: &mut UiTree, action: &str, width: f32, height: f32) -> bool {
        for _ in 0..30 {
            let frame = tree.layout(Size::new(width, height));
            if frame.regions.iter().any(|region| region.id == action) {
                return true;
            }
            tree.handle_event(UiEvent::Scroll {
                position: Point::new(width * 0.65, height * 0.5),
                delta_y: -180.0,
            });
        }
        false
    }

    #[test]
    fn settings_routes_expose_their_existing_actions() {
        let mut state = empty_state();
        state.show_settings = true;
        for (page, action) in [
            (SettingsPage::General, "workspace_apply"),
            (SettingsPage::Appearance, "settings_reduced_motion"),
            (SettingsPage::Agent, "write_mode"),
            (SettingsPage::Agent, "agent_tool:read_file"),
            (SettingsPage::Agent, "command_isolation:host_unrestricted"),
            (SettingsPage::Providers, "catalog_refresh"),
            (SettingsPage::Terminal, "settings_terminal_shell:system"),
            (SettingsPage::WorkspaceSearch, "composer_add_context"),
            (SettingsPage::Mcp, "mcp_new"),
            (SettingsPage::Skills, "open_marketplace"),
            (SettingsPage::Automations, "open_automations"),
            (SettingsPage::Data, "session_organize:recent"),
            (SettingsPage::Github, "github_account_connect"),
        ] {
            state.settings_page = page;
            let mut tree = UiTree::new(build_view(&state, 1100.0));
            assert!(
                settings_action_reachable(&mut tree, action, 1100.0, 720.0),
                "{page:?} missing {action}"
            );
        }
    }

    #[test]
    fn command_rules_have_reachable_list_and_editor_controls() {
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Commands;
        state.native_exec_rules.rules = vec![openide_harness::ExecRule::new(
            vec![
                openide_harness::ExecPatternToken::Literal("git".into()),
                openide_harness::ExecPatternToken::Literal("push".into()),
            ],
            openide_harness::ExecDecision::Ask,
            None,
        )
        .unwrap()];
        for width in [520.0, 1100.0] {
            let mut tree = UiTree::new(build_view(&state, width));
            assert!(settings_action_reachable(
                &mut tree,
                "exec_rule_new",
                width,
                720.0
            ));
            let mut tree = UiTree::new(build_view(&state, width));
            assert!(settings_action_reachable(
                &mut tree,
                "exec_rule_select:0",
                width,
                720.0
            ));
            state.native_exec_rules.editor_open = true;
            let mut tree = UiTree::new(build_view(&state, width));
            for id in [
                "exec_rule_draft_pattern",
                "exec_rule_draft_justification",
                "exec_rule_decision:ask",
                "exec_rule_decision:deny",
                "exec_rule_save",
                "exec_rule_cancel",
            ] {
                assert!(
                    settings_action_reachable(&mut tree, id, width, 720.0),
                    "command rule editor missing {id} at {width}px"
                );
            }
            state.native_exec_rules.editor_open = false;
        }
    }

    #[test]
    fn invalid_command_rules_offer_confirmed_reset() {
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Commands;
        state.native_exec_rules.load_error = Some("versión no compatible".into());
        for width in [520.0, 1100.0] {
            let mut tree = UiTree::new(build_view(&state, width));
            assert!(settings_action_reachable(
                &mut tree,
                "exec_rule_reset",
                width,
                720.0
            ));
            state.native_exec_rules.reset_confirm_open = true;
            let mut tree = UiTree::new(build_view(&state, width));
            for id in ["exec_rule_reset_confirm", "exec_rule_reset_cancel"] {
                assert!(
                    settings_action_reachable(&mut tree, id, width, 720.0),
                    "reset modal missing {id} at {width}px"
                );
            }
            state.native_exec_rules.reset_confirm_open = false;
        }
    }

    #[test]
    fn local_skills_have_reachable_selection_and_recovery_controls() {
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Skills;
        state.native_skills.workspace = "C:/project".into();
        let selection = openide_harness::SkillSelection {
            name: "review".into(),
            provenance: openide_harness::SkillProvenance {
                origin: openide_harness::SkillOrigin::Project,
                root: "C:/project/.agents/skills".into(),
                skill_directory: "C:/project/.agents/skills/review".into(),
                document: "C:/project/.agents/skills/review/SKILL.md".into(),
                sha256_hex: "a".repeat(64),
            },
        };
        state.native_skills.available =
            vec![crate::native_skills_settings::NativeSkillCatalogEntry {
                name: selection.name.clone(),
                description: "Review changes".into(),
                origin: openide_harness::SkillOrigin::Project,
                document: selection.provenance.document.clone(),
                selection: selection.clone(),
            }];
        state.native_skills.selected = vec![selection];
        for width in [520.0, 1100.0] {
            for id in [
                "native_skill_refresh",
                "native_skill_toggle:0",
                "native_skill_remove:0",
            ] {
                let mut tree = UiTree::new(build_view(&state, width));
                assert!(
                    settings_action_reachable(&mut tree, id, width, 720.0),
                    "local skill selector missing {id} at {width}px"
                );
            }
            state.native_skills.load_error = Some("archivo inválido".into());
            let mut tree = UiTree::new(build_view(&state, width));
            assert!(settings_action_reachable(
                &mut tree,
                "native_skill_reset",
                width,
                720.0
            ));
            state.native_skills.reset_confirm_open = true;
            let mut tree = UiTree::new(build_view(&state, width));
            for id in ["native_skill_reset_confirm", "native_skill_reset_cancel"] {
                assert!(settings_action_reachable(&mut tree, id, width, 720.0));
            }
            state.native_skills.reset_confirm_open = false;
            state.native_skills.load_error = None;
        }
    }

    #[test]
    fn every_settings_page_has_a_native_content_surface_at_wide_and_narrow_widths() {
        let state = empty_state();
        let pages = [
            (SettingsPage::General, "General"),
            (SettingsPage::Appearance, "Apariencia"),
            (SettingsPage::Providers, "Proveedores y modelos"),
            (SettingsPage::Browser, "Navegador"),
            (SettingsPage::ComputerUse, "Control del equipo"),
            (SettingsPage::Terminal, "Terminal"),
            (SettingsPage::Shortcuts, "Atajos de teclado"),
            (SettingsPage::WorkspaceSearch, "Búsqueda de archivos"),
            (SettingsPage::Agent, "Agente"),
            (SettingsPage::Memory, "Memoria"),
            (SettingsPage::Subagents, "Subagentes"),
            (SettingsPage::Plugins, "Plugins"),
            (SettingsPage::Mcp, "MCP"),
            (SettingsPage::Skills, "Skills"),
            (SettingsPage::Commands, "Comandos"),
            (SettingsPage::Automations, "Automatizaciones"),
            (SettingsPage::Hooks, "Hooks"),
            (SettingsPage::Data, "Datos y sesiones"),
            (SettingsPage::Usage, "Uso"),
            (SettingsPage::Github, "GitHub"),
        ];
        for width in [520.0, 1100.0] {
            for (page, title) in pages {
                let content =
                    crate::settings_ui::build_settings_page(&state, page, width, width >= 760.0);
                let frame = UiTree::new(content).layout(Size::new(width, 720.0));
                assert!(
                    frame.commands.iter().any(|command| matches!(
                        command,
                        DrawCommand::Text { text, .. } if text == title
                    )),
                    "{page:?} missing its heading at {width}px"
                );
            }
        }
    }

    #[test]
    fn command_isolation_choices_match_host_support_at_both_widths() {
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Agent;
        for width in [520.0, 1100.0] {
            let mut host = UiTree::new(build_view(&state, width));
            let host_reachable = settings_action_reachable(
                &mut host,
                "command_isolation:host_unrestricted",
                width,
                720.0,
            );
            assert!(
                host_reachable,
                "host choice unreachable at {width}px, offset {}",
                host.scroll_offset("settings_page:Agent")
            );
            let mut offline = UiTree::new(build_view(&state, width));
            assert_eq!(
                settings_action_reachable(
                    &mut offline,
                    "command_isolation:workspace_write_offline",
                    width,
                    720.0,
                ),
                cfg!(windows),
            );
        }
    }

    #[test]
    fn mcp_settings_keeps_list_visible_and_opens_a_centered_scrollable_editor() {
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Mcp;
        state.mcp_servers = vec![crate::mcp_settings::McpServerSettings {
            id: "filesystem".into(),
            enabled: true,
            program: "npx".into(),
            args: vec!["-y".into()],
            cwd: None,
            credential_env_names: vec!["MCP_API_KEY".into()],
            url: None,
            http_bearer_token_env_name: None,
            http_oauth: None,
        }];
        state.mcp_selected_id = Some("filesystem".into());
        state.mcp_draft_id = "filesystem".into();
        state.mcp_draft_program = "npx".into();
        state.mcp_draft_args = "[\"-y\"]".into();
        state.mcp_draft_env_names = "MCP_API_KEY".into();
        for width in [520.0, 1100.0] {
            for id in [
                "mcp_new",
                "mcp_select:filesystem",
                "mcp_toggle:filesystem",
                "mcp_delete:filesystem",
            ] {
                let mut tree = UiTree::new(build_view(&state, width));
                assert!(
                    settings_action_reachable(&mut tree, id, width, 720.0),
                    "MCP missing {id} at {width}px"
                );
            }
            let closed = UiTree::new(build_view(&state, width)).layout(Size::new(width, 720.0));
            assert!(!closed
                .regions
                .iter()
                .any(|region| region.id == "mcp_draft_id"));

            state.mcp_editor_open = true;
            let mut modal_tree = UiTree::new(build_view(&state, width));
            let modal = modal_tree.layout(Size::new(width, 720.0));
            let card = modal
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:mcp_cancel")
                .expect("MCP modal")
                .rect;
            assert!((card.x + card.width / 2.0 - width / 2.0).abs() < 1.0);
            assert!((card.y + card.height / 2.0 - 360.0).abs() < 1.0);
            assert_eq!(modal_tree.focused_id(), Some("mcp_draft_id"));
            assert!(modal
                .regions
                .iter()
                .any(|region| region.id == "modal_body:mcp_cancel"));
            let save = modal
                .regions
                .iter()
                .find(|region| region.id == "mcp_save")
                .expect("fixed modal action")
                .rect;
            assert!(save.y >= card.y);
            assert!(save.y + save.height <= card.y + card.height);
            for id in [
                "mcp_draft_id",
                "mcp_draft_program",
                "mcp_draft_args",
                "mcp_draft_cwd",
                "mcp_draft_env_names",
                "mcp_cancel",
            ] {
                let mut tree = UiTree::new(build_view(&state, width));
                assert!(
                    settings_action_reachable(&mut tree, id, width, 720.0),
                    "MCP modal missing {id} at {width}px"
                );
            }
            state.mcp_editor_open = false;
        }
        let frame = UiTree::new(build_view(&state, 1100.0)).layout(Size::new(1100.0, 720.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text == "Activado"
        )));
        state.mcp_servers[0].enabled = false;
        let disabled = UiTree::new(build_view(&state, 1100.0)).layout(Size::new(1100.0, 720.0));
        assert!(disabled.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text == "Desactivado"
        )));
    }

    #[test]
    fn mcp_http_editor_shows_endpoint_and_token_name_without_stdio_fields() {
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Mcp;
        state.mcp_editor_open = true;
        state.mcp_draft_http = true;
        state.mcp_draft_url = "https://example.com/mcp".into();
        state.mcp_draft_http_bearer_token_env_name = "MCP_HTTP_TOKEN".into();
        for width in [520.0, 1100.0] {
            let mut tree = UiTree::new(build_view(&state, width));
            for id in [
                "mcp_transport:stdio",
                "mcp_transport:http",
                "mcp_draft_url",
                "mcp_auth:bearer",
                "mcp_draft_http_bearer_token_env_name",
                "mcp_save",
            ] {
                assert!(
                    settings_action_reachable(&mut tree, id, width, 720.0),
                    "HTTP editor missing {id} at {width}px"
                );
            }
            let frame = UiTree::new(build_view(&state, width)).layout(Size::new(width, 720.0));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "mcp_draft_program"));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "mcp_draft_oauth_client_id"));
        }
    }

    #[test]
    fn mcp_oauth_editor_and_authorization_state_are_visible() {
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Mcp;
        state.mcp_editor_open = true;
        state.mcp_draft_http = true;
        state.mcp_draft_oauth_enabled = true;
        state.mcp_draft_oauth_client_id = "public-client".into();
        state.mcp_draft_oauth_issuer = "https://auth.example.test".into();
        state.mcp_draft_oauth_scopes = "read\nwrite".into();
        for width in [520.0, 1100.0] {
            let mut tree = UiTree::new(build_view(&state, width));
            for id in [
                "mcp_auth:bearer",
                "mcp_auth:oauth",
                "mcp_draft_oauth_client_id",
                "mcp_draft_oauth_issuer",
                "mcp_draft_oauth_scopes",
                "mcp_save",
            ] {
                assert!(
                    settings_action_reachable(&mut tree, id, width, 720.0),
                    "OAuth editor missing {id} at {width}px"
                );
            }
        }
        state.mcp_editor_open = false;
        state.mcp_servers = vec![crate::mcp_settings::McpServerSettings {
            id: "remote".into(),
            enabled: true,
            program: String::new(),
            args: Vec::new(),
            cwd: None,
            credential_env_names: Vec::new(),
            url: Some("https://mcp.example.test/mcp".into()),
            http_bearer_token_env_name: None,
            http_oauth: Some(crate::mcp_settings::McpHttpOAuthSettings {
                client_id: "public-client".into(),
                issuer: "https://auth.example.test".into(),
                scopes: vec!["read".into()],
            }),
        }];
        state.mcp_oauth_status = Some(McpOAuthViewStatus {
            server_id: "remote".into(),
            phase: McpOAuthViewPhase::AwaitingBrowser {
                url: "https://auth.example.test/authorize?state=transient".into(),
            },
        });
        for width in [520.0, 1100.0] {
            let mut tree = UiTree::new(build_view(&state, width));
            for id in [
                "mcp_oauth_authorize:remote",
                "mcp_oauth_open",
                "mcp_oauth_cancel",
            ] {
                let reachable = settings_action_reachable(&mut tree, id, width, 720.0);
                assert_eq!(
                    reachable,
                    id != "mcp_oauth_authorize:remote",
                    "{id} at {width}px"
                );
            }
            let frame = UiTree::new(build_view(&state, width)).layout(Size::new(width, 720.0));
            assert!(frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::Text { text, .. } if text.contains("auth.example.test/authorize")
            )));
        }
    }

    #[test]
    fn mcp_load_error_is_visible_on_settings_page() {
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Mcp;
        state.mcp_load_error = Some("archivo corrupto".into());
        let frame = UiTree::new(build_view(&state, 1100.0)).layout(Size::new(1100.0, 720.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text.contains("archivo corrupto")
        )));
    }

    #[test]
    fn native_hooks_page_and_editor_expose_bounded_host_configuration() {
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Hooks;
        for width in [520.0, 1100.0] {
            let mut tree = UiTree::new(build_view(&state, width));
            assert!(settings_action_reachable(
                &mut tree, "hook_new", width, 720.0
            ));
        }
        state.native_hooks.editor_open = true;
        for width in [520.0, 1100.0] {
            let mut tree = UiTree::new(build_view(&state, width));
            for id in [
                "hook_draft_id",
                "hook_phase:pre_tool_use",
                "hook_phase:post_tool_use",
                "hook_draft_tool_name",
                "hook_draft_executable",
                "hook_draft_args",
                "hook_draft_cwd",
                "hook_draft_timeout_ms",
                "hook_draft_max_output_bytes",
                "hook_save",
                "hook_cancel",
            ] {
                assert!(
                    settings_action_reachable(&mut tree, id, width, 720.0),
                    "Hook editor missing {id} at {width}px"
                );
            }
        }
    }

    #[test]
    fn mcp_removal_uses_a_centered_confirmation_and_escape_cancels() {
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Mcp;
        state.mcp_servers = vec![crate::mcp_settings::McpServerSettings {
            id: "filesystem".into(),
            enabled: false,
            program: "npx".into(),
            args: vec![],
            cwd: None,
            credential_env_names: vec![],
            url: None,
            http_bearer_token_env_name: None,
            http_oauth: None,
        }];
        state.pending_mcp_delete_id = Some("filesystem".into());
        for width in [520.0, 1100.0] {
            let mut tree = UiTree::new(build_view(&state, width));
            let frame = tree.layout(Size::new(width, 720.0));
            let card = frame
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:mcp_delete_cancel")
                .expect("confirmation surface")
                .rect;
            assert!((card.x + card.width / 2.0 - width / 2.0).abs() < 1.0);
            assert!((card.y + card.height / 2.0 - 360.0).abs() < 1.0);
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "mcp_delete_confirm:filesystem"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Escape,
                    shift: false,
                    ctrl: false,
                }),
                vec![UiAction::Clicked {
                    id: "mcp_delete_cancel".into()
                }]
            );
        }
    }

    #[test]
    fn desktop_settings_navigation_can_scroll_to_every_route() {
        let mut state = empty_state();
        state.show_settings = true;
        let width = 1100.0;
        let height = 720.0;
        let sidebar_x = dock_layout(&state, width).left / 2.0;
        let mut tree = UiTree::new(build_view(&state, width));
        for page in [
            SettingsPage::General,
            SettingsPage::Appearance,
            SettingsPage::Providers,
            SettingsPage::Browser,
            SettingsPage::ComputerUse,
            SettingsPage::Terminal,
            SettingsPage::Shortcuts,
            SettingsPage::WorkspaceSearch,
            SettingsPage::Agent,
            SettingsPage::Memory,
            SettingsPage::Subagents,
            SettingsPage::Plugins,
            SettingsPage::Mcp,
            SettingsPage::Skills,
            SettingsPage::Commands,
            SettingsPage::Automations,
            SettingsPage::Hooks,
            SettingsPage::Data,
            SettingsPage::Usage,
            SettingsPage::Github,
        ] {
            let mut visible = false;
            for _ in 0..30 {
                let frame = tree.layout(Size::new(width, height));
                if frame
                    .regions
                    .iter()
                    .any(|region| region.id == page.action_id())
                {
                    visible = true;
                    break;
                }
                tree.handle_event(UiEvent::Scroll {
                    position: Point::new(sidebar_x, 430.0),
                    delta_y: -120.0,
                });
            }
            assert!(visible, "sidebar could not reach {page:?}");
        }
    }

    #[test]
    fn settings_shows_save_and_error_notices_inside_the_page() {
        let mut state = empty_state();
        state.show_settings = true;
        for (message, is_error) in [
            ("Proyecto actualizado", false),
            ("La carpeta del proyecto no existe", true),
        ] {
            state.settings_notice = Some(message.into());
            state.settings_notice_error = is_error;
            let frame = UiTree::new(build_view(&state, 1100.0)).layout(Size::new(1100.0, 720.0));
            assert!(frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::Text { text, .. } if text == message
            )));
            if is_error {
                assert!(frame.commands.iter().any(|command| matches!(
                    command,
                    DrawCommand::FillRect { color, .. } if *color == theme::DIFF_REMOVED_BG
                )));
            }
        }
    }

    #[test]
    fn model_catalog_uses_side_by_side_lists_only_when_settings_has_room() {
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Providers;
        state.native_provider_id = "openai".into();
        state.model = "gpt-5.4".into();
        state.catalog_browse_provider = "openai".into();
        state.catalog = Some(std::sync::Arc::new(
            serde_json::from_value(json!({
                "fetched_at_ms": 1,
                "providers": {
                    "openai": {
                        "id": "openai", "name": "OpenAI",
                        "models": {
                            "gpt-5.4": {
                                "id": "gpt-5.4", "name": "GPT-5.4",
                                "tool_call": true,
                                "modalities": {"input": ["text"], "output": ["text"]}
                            }
                        }
                    }
                }
            }))
            .unwrap(),
        ));
        for (viewport, side_by_side) in [(1300.0, true), (620.0, false)] {
            let mut tree = UiTree::new(build_view(&state, viewport));
            assert!(
                settings_action_reachable(
                    &mut tree,
                    "catalog_select:openai/gpt-5.4",
                    viewport,
                    720.0,
                ),
                "catalog model unreachable at {viewport}px, settings offset {}",
                tree.scroll_offset("settings_page:Providers")
            );
            let frame = tree.layout(Size::new(viewport, 720.0));
            let model = frame
                .regions
                .iter()
                .find(|region| region.id == "catalog_select:openai/gpt-5.4")
                .unwrap()
                .rect;
            let mut provider_tree = UiTree::new(build_view(&state, viewport));
            assert!(settings_action_reachable(
                &mut provider_tree,
                "catalog_browse:openai",
                viewport,
                720.0
            ));
            let provider_frame = provider_tree.layout(Size::new(viewport, 720.0));
            let provider = provider_frame
                .regions
                .iter()
                .find(|region| region.id == "catalog_browse:openai")
                .unwrap()
                .rect;
            if side_by_side {
                assert!(model.x >= provider.x + provider.width);
            } else {
                assert!(model.x < provider.x + provider.width);
            }
        }
    }

    #[test]
    fn settings_keeps_shared_frame_radius_and_keyboard_actions() {
        let mut state = empty_state();
        let home = UiTree::new(build_view(&state, 1000.0)).layout(Size::new(1000.0, 720.0));
        state.show_settings = true;
        let mut tree = UiTree::new(build_view(&state, 1000.0));
        let frame = tree.layout(Size::new(1000.0, 720.0));
        let frame_radius = |commands: &[DrawCommand]| {
            commands.iter().any(|command| {
                matches!(
                    command,
                    DrawCommand::StrokeRect { rect, radius, color, .. }
                        if *radius == WORKSPACE_RADIUS
                            && *color == theme::BORDER
                            && rect.width > 500.0
                )
            })
        };
        assert!(frame_radius(&home.commands));
        assert!(frame_radius(&frame.commands));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::StrokeRect { radius, color, .. }
                if *radius == 14.0 && *color == theme::BORDER
        )));
        assert!(tree.focus("settings_page:agent"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked {
                id: "settings_page:agent".into()
            }]
        );
        assert!(tree.focus("workspace"));
        assert!(tree.focus("workspace_apply"));
    }

    #[test]
    fn grouped_sidebar_keeps_empty_groups_and_places_members_under_their_header() {
        let mut state = empty_state();
        state.session_organization = SessionOrganization::Group;
        let first = state.session_groups.create("Trabajo").unwrap();
        state.session_groups.create("Vacío").unwrap();
        state.sessions = vec![
            SessionView {
                id: "a".into(),
                title: "Asignada".into(),
                project: "C:/project".into(),
                backend: "native".into(),
                pinned: false,
                archived: false,
                unread: false,
            },
            SessionView {
                id: "b".into(),
                title: "Sin grupo".into(),
                project: "C:/project".into(),
                backend: "native".into(),
                pinned: false,
                archived: false,
                unread: false,
            },
        ];
        assert!(state.session_groups.assign("a", &first));
        let rows = session_list_rows(&state);
        let labels: Vec<_> = rows
            .iter()
            .map(|row| match row {
                SessionListRow::Section(title) => *title,
                SessionListRow::Group { group, .. } => group.title.as_str(),
                SessionListRow::Session(session) => session.title.as_str(),
                _ => "other",
            })
            .collect();
        assert_eq!(
            labels,
            [
                "GRUPOS",
                "Vacío",
                "Trabajo",
                "Asignada",
                "SIN GRUPO",
                "Sin grupo"
            ]
        );
        for width in [176.0, 200.0, 264.0, 320.0] {
            let mut tree = UiTree::new(sidebar(&state, width));
            let frame = tree.layout(Size::new(width, 640.0));
            for id in [
                "session_organize:group",
                "session_organize:project",
                "session_group_new",
                "session_sort_toggle",
                "toggle_archived_view",
            ] {
                let region = frame.regions.iter().find(|region| region.id == id).unwrap();
                assert!(
                    region.rect.x + region.rect.width <= width,
                    "{id} overflows the {width}px sidebar"
                );
            }
        }
        state.session_menu_id = Some("a".into());
        let mut tree = UiTree::new(sidebar(&state, 264.0));
        let frame = tree.layout(Size::new(264.0, 640.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "session_group_unassign:a"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == format!("session_group_assign:{first}:a")));
        assert!(state.session_groups.toggle_collapsed(&first));
        assert!(!session_list_rows(&state)
            .iter()
            .any(|row| matches!(row, SessionListRow::Session(session) if session.id == "a")));
    }

    #[test]
    fn session_dock_toggle_is_only_offered_when_three_columns_can_fit() {
        let mut state = empty_state();
        state.selected_session = Some("session-1".into());
        state.show_side_pane = false;
        let narrow = UiTree::new(build_view(&state, 900.0)).layout(Size::new(900.0, 620.0));
        assert!(!narrow
            .regions
            .iter()
            .any(|region| region.id == "toggle_side_pane_header"));
        let wide = UiTree::new(build_view(&state, 1200.0)).layout(Size::new(1200.0, 620.0));
        assert!(wide
            .regions
            .iter()
            .any(|region| region.id == "toggle_side_pane_header"));
    }

    #[test]
    fn sidebar_groups_contiguous_sessions_by_full_project_path() {
        let mut state = empty_state();
        state.sessions = vec![
            SessionView {
                id: "one".into(),
                title: "Primera tarea".into(),
                project: "C:/work/alpha".into(),
                backend: "native".into(),
                pinned: false,
                archived: false,
                unread: false,
            },
            SessionView {
                id: "two".into(),
                title: "Segunda tarea".into(),
                project: "C:/work/alpha".into(),
                backend: "native".into(),
                pinned: false,
                archived: false,
                unread: false,
            },
            SessionView {
                id: "three".into(),
                title: "Otra tarea".into(),
                project: "D:/work/beta".into(),
                backend: "codex".into(),
                pinned: false,
                archived: false,
                unread: false,
            },
        ];
        let frame = UiTree::new(build_view(&state, 1200.0)).layout(Size::new(1200.0, 620.0));
        let labels = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(labels.iter().filter(|text| **text == "alpha").count(), 1);
        assert_eq!(labels.iter().filter(|text| **text == "beta").count(), 1);
        for id in ["session:one", "session:two", "session:three"] {
            assert!(frame.regions.iter().any(|region| region.id == id));
        }
    }

    #[test]
    fn sessions_sidebar_virtualizes_a_thousand_rows_and_reaches_the_last_task() {
        let mut state = empty_state();
        state.sessions = (0..1_000)
            .map(|index| SessionView {
                id: format!("task-{index}"),
                title: format!("Tarea {index}"),
                project: "C:/work/alpha".into(),
                backend: "native".into(),
                pinned: false,
                archived: false,
                unread: false,
            })
            .collect();
        let mut tree = UiTree::new(build_view(&state, 1200.0));
        let first = tree.layout(Size::new(1200.0, 620.0));
        let visible_count = |frame: &openide_ui::Frame| {
            frame
                .regions
                .iter()
                .filter(|region| region.id.starts_with("session:task-"))
                .count()
        };
        assert!(visible_count(&first) > 0);
        assert!(visible_count(&first) < 20);
        tree.scroll_to_index("sessions_list", 1_001);
        let last = tree.layout(Size::new(1200.0, 620.0));
        assert!(last
            .regions
            .iter()
            .any(|region| region.id == "session:task-999"));
        assert!(tree.scroll_offset("sessions_list") > 30_000.0);
        assert!(visible_count(&last) < 20);
    }

    #[test]
    fn session_row_keeps_unicode_title_agent_and_menu_in_separate_bounds() {
        let mut state = empty_state();
        state
            .backend_options
            .push(("codex".into(), "Codex CLI".into()));
        let session = SessionView {
            id: "unicode".into(),
            title: "Analiza 🦀 este proyecto con títulos muy largos".into(),
            project: "C:/project".into(),
            backend: "codex".into(),
            pinned: false,
            archived: false,
            unread: true,
        };
        for width in [176.0, 264.0] {
            let frame = UiTree::new(session_row(&state, &session, width))
                .layout(Size::new(width - 18.0, 32.0));
            let action = frame
                .regions
                .iter()
                .find(|region| region.id == "session:unicode")
                .unwrap();
            let menu = frame
                .regions
                .iter()
                .find(|region| region.id == "session_menu:unicode")
                .unwrap();
            assert_eq!(action.rect.height, 32.0);
            assert_eq!(menu.rect.height, 32.0);
            assert!(action.rect.x <= menu.rect.x);
            assert!(action.rect.x + action.rect.width >= menu.rect.x + menu.rect.width);
            assert!(menu.rect.x + menu.rect.width <= width - 18.0);
            for point in [
                Point::new(action.rect.x + 3.0, action.rect.y + 14.0),
                Point::new(menu.rect.x - 22.0, menu.rect.y + 14.0),
            ] {
                assert_eq!(frame.hit_test(point), Some("session:unicode"));
            }
            assert_eq!(
                frame.hit_test(Point::new(menu.rect.x + 14.0, menu.rect.y + 14.0)),
                Some("session_menu:unicode")
            );
            let labels = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(labels.contains(&"Codex"), "{labels:?}");
            assert!(labels.iter().any(|title| title.starts_with("Analiza")));
            assert!(!labels.contains(&session.title.as_str()), "{labels:?}");
            assert!(frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::FillRect { rect, color, .. }
                    if rect.width == 6.0 && rect.height == 6.0 && *color == theme::BRAND
            )));
        }
        let mut tree = UiTree::new(session_row(&state, &session, 264.0));
        tree.layout(Size::new(246.0, 32.0));
        assert!(tree.focus("session:unicode"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked {
                id: "session:unicode".into()
            }]
        );
    }

    #[test]
    fn session_row_hover_spans_title_and_more_without_a_separate_icon_patch() {
        let state = empty_state();
        let session = SessionView {
            id: "hover".into(),
            title: "Tarea con título".into(),
            project: "C:/project".into(),
            backend: "native".into(),
            pinned: false,
            archived: false,
            unread: false,
        };
        let mut tree = UiTree::new(session_row(&state, &session, 264.0));
        tree.set_reduced_motion(true);
        let initial = tree.layout(Size::new(246.0, 32.0));
        let more = initial
            .regions
            .iter()
            .find(|region| region.id == "session_menu:hover")
            .unwrap()
            .rect;
        for (point, id) in [
            (Point::new(30.0, 16.0), "session:hover"),
            (
                Point::new(more.x + more.width / 2.0, more.y + more.height / 2.0),
                "session_menu:hover",
            ),
        ] {
            tree.handle_event(UiEvent::PointerMove(point));
            let frame = tree.layout(Size::new(246.0, 32.0));
            assert_eq!(frame.hit_test(point), Some(id));
            assert_eq!(
                frame
                    .commands
                    .iter()
                    .filter(|command| matches!(
                        command,
                        DrawCommand::FillRect { rect, color, .. }
                            if rect.width == 246.0
                                && rect.height == 32.0
                                && *color == theme::hover_for(theme::SIDEBAR)
                    ))
                    .count(),
                1,
            );
        }
    }

    #[test]
    fn selected_session_row_changes_paint_without_moving_actions() {
        let mut state = empty_state();
        let session = SessionView {
            id: "selected".into(),
            title: "Tarea".into(),
            project: "C:/project".into(),
            backend: "native".into(),
            pinned: false,
            archived: false,
            unread: false,
        };
        let base = UiTree::new(session_row(&state, &session, 264.0)).layout(Size::new(246.0, 32.0));
        state.selected_session = Some(session.id.clone());
        let selected =
            UiTree::new(session_row(&state, &session, 264.0)).layout(Size::new(246.0, 32.0));
        for id in ["session:selected", "session_menu:selected"] {
            let rect = |frame: &openide_ui::Frame| {
                frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap()
                    .rect
            };
            assert_eq!(rect(&base), rect(&selected));
        }
        assert!(selected.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, .. }
                if rect.width == 246.0 && rect.height == 32.0 && *color == theme::SELECTED
        )));
    }

    #[test]
    fn selected_title_survives_switching_the_sidebar_to_archive() {
        let mut state = empty_state();
        state.selected_session = Some("active".into());
        state.selected_session_title = Some("Título manual".into());
        state.show_archived = true;
        assert_eq!(session_title(&state), "Título manual");
        let frame = UiTree::new(build_view(&state, 900.0)).layout(Size::new(900.0, 620.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text == "Título manual"
        )));
    }

    #[test]
    fn files_preview_and_permission_actions_remain_reachable_in_three_docks() {
        let mut state = empty_state();
        state.file_rows = vec![FileRowView {
            name: "README.md".into(),
            depth: 0,
            is_dir: false,
            expanded: false,
            can_load_more: false,
        }];
        state.dock_tabs.push(DockTabView {
            id: 3,
            title: "Archivos".into(),
            active: true,
            kind: SideTab::Files,
            closable: false,
        });
        state.dock_tabs.push(DockTabView {
            id: 4,
            title: "README.md".into(),
            active: false,
            kind: SideTab::FilePreview,
            closable: true,
        });
        for tab in &mut state.dock_tabs {
            tab.active = tab.id == 3;
        }
        state.side_tab = SideTab::Files;
        state.file_preview = Some(FilePreviewView {
            tab_id: 4,
            path: "README.md".into(),
            page_index: 0,
            content: Some(Ok(PreviewPage {
                text: "# OpenIDE".into(),
                start_line: 0,
                end_line: 1,
                total_lines: Some(1),
                has_more: false,
                truncated: false,
            })),
        });
        state.pending_permissions.push(PermissionView {
            request_id: "req-1".into(),
            tool_name: "write_file".into(),
            arguments: "{\"path\":\"README.md\"}".into(),
        });
        let frame = UiTree::new(build_view(&state, 1100.0)).layout(Size::new(1100.0, 720.0));
        for id in [
            "file_row:0",
            "dock_tab:3",
            "permission_allow:req-1",
            "permission_deny:req-1",
            "prompt",
        ] {
            assert!(
                frame.regions.iter().any(|region| region.id == id),
                "missing {id}"
            );
        }
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "sidebar_files"));
        state.side_tab = SideTab::FilePreview;
        for tab in &mut state.dock_tabs {
            tab.active = tab.id == 4;
        }
        let preview = UiTree::new(build_view(&state, 1100.0)).layout(Size::new(1100.0, 720.0));
        assert!(preview
            .regions
            .iter()
            .any(|region| region.id == "dock_close:4"));
        assert!(preview.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text.contains("README.md")
        )));
        state.show_settings = true;
        let settings_frame =
            UiTree::new(build_view(&state, 1100.0)).layout(Size::new(1100.0, 720.0));
        assert!(settings_frame
            .regions
            .iter()
            .any(|region| region.id == "permission_deny:req-1"));
    }

    #[test]
    fn fallback_view_keeps_related_subagent_action_ids() {
        let mut state = empty_state();
        state.selected_session = Some("parent-1".into());
        // Settings uses the workspace fallback branch rather than the session
        // surface, so it must retain the same child action IDs.
        state.show_settings = true;
        state
            .pending_subagent_permissions
            .push(SubagentPermissionView {
                child_session_id: "child-1".into(),
                parent_session_id: "parent-1".into(),
                request_id: "request-1".into(),
                tool_name: "write_file".into(),
                arguments: "{}".into(),
            });
        state.active_subagents.push(SubagentRunView {
            child_session_id: "child-1".into(),
            parent_session_id: "parent-1".into(),
            title: "Actualizar pruebas".into(),
        });
        let frame = UiTree::new(build_view(&state, 1100.0)).layout(Size::new(1100.0, 850.0));
        for id in [
            "subagent_permission_allow:child-1:request-1",
            "subagent_permission_deny:child-1:request-1",
            "subagent_cancel:child-1",
        ] {
            assert!(
                frame.regions.iter().any(|region| region.id == id),
                "missing {id}"
            );
        }
    }

    #[test]
    fn nested_file_tree_renders_continuous_guides_without_losing_row_actions() {
        let mut state = empty_state();
        state.file_rows = vec![
            FileRowView {
                name: "src".into(),
                depth: 0,
                is_dir: true,
                expanded: true,
                can_load_more: false,
            },
            FileRowView {
                name: "ui".into(),
                depth: 1,
                is_dir: true,
                expanded: true,
                can_load_more: false,
            },
            FileRowView {
                name: "tree.rs".into(),
                depth: 2,
                is_dir: false,
                expanded: false,
                can_load_more: false,
            },
            FileRowView {
                name: "lib.rs".into(),
                depth: 1,
                is_dir: false,
                expanded: false,
                can_load_more: false,
            },
            FileRowView {
                name: "README.md".into(),
                depth: 0,
                is_dir: false,
                expanded: false,
                can_load_more: false,
            },
        ];
        let frame = UiTree::new(file_tree_content(&state, 320.0)).layout(Size::new(320.0, 620.0));
        for index in 0..state.file_rows.len() {
            let id = format!("file_row:{index}");
            assert_eq!(
                frame
                    .regions
                    .iter()
                    .filter(|region| region.id == id)
                    .count(),
                1
            );
        }
        let guide = theme::TREE_GUIDE;
        let strokes: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRect { rect, color, .. } if *color == guide => Some(*rect),
                _ => None,
            })
            .collect();
        assert!(strokes
            .iter()
            .any(|rect| rect.width == 1.0 && rect.height == 28.0));
        assert!(
            strokes
                .iter()
                .filter(|rect| rect.width == 5.0 && rect.height == 1.0)
                .count()
                >= 3
        );
        let file_row_y = frame
            .regions
            .iter()
            .find(|region| region.id == "file_row:2")
            .unwrap()
            .rect
            .y;
        assert!(strokes
            .iter()
            .any(|rect| rect.width == 1.0 && rect.height == 28.0 && rect.y == file_row_y));
    }

    #[test]
    fn file_tree_virtualizes_large_directories_and_reaches_last_row() {
        let mut state = empty_state();
        state.file_rows = (0..1_000)
            .map(|index| FileRowView {
                name: format!("file-{index:04}.rs"),
                depth: 0,
                is_dir: false,
                expanded: false,
                can_load_more: false,
            })
            .collect();
        let mut tree = UiTree::new(file_tree_content(&state, 320.0));
        let first = tree.layout(Size::new(320.0, 360.0));
        let first_count = first
            .regions
            .iter()
            .filter(|region| region.id.starts_with("file_row:"))
            .count();
        assert!(
            first_count > 0 && first_count < 30,
            "built {first_count} file rows"
        );
        assert!(!first
            .regions
            .iter()
            .any(|region| region.id == "file_row:999"));
        tree.scroll_to_index("file_tree", 999);
        let last = tree.layout(Size::new(320.0, 360.0));
        assert!(last
            .regions
            .iter()
            .any(|region| region.id == "file_row:999"));
    }

    #[test]
    fn project_tree_header_reuses_tree_row_and_keeps_toggle_action() {
        let frame = UiTree::new(project_header("C:/work/openIDE", 3, false, 264.0))
            .layout(Size::new(246.0, 32.0));
        let action = frame
            .regions
            .iter()
            .find(|region| region.id == "session_project_toggle:C:/work/openIDE")
            .unwrap();
        assert_eq!(action.rect.width, 246.0);
        assert_eq!(action.rect.height, 32.0);
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "openIDE"
        )));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "3"
        )));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Icon { icon: Icon::Folder, rect, .. } if rect.y == 8.0 && rect.width == 16.0
        )));
    }

    #[test]
    fn pinned_sessions_are_global_and_archive_has_separate_actions() {
        let mut state = empty_state();
        state.sessions = vec![
            SessionView {
                id: "pinned".into(),
                title: "Fijada".into(),
                project: "C:/work/alpha".into(),
                backend: "native".into(),
                pinned: true,
                archived: false,
                unread: false,
            },
            SessionView {
                id: "active".into(),
                title: "Activa".into(),
                project: "C:/work/alpha".into(),
                backend: "codex".into(),
                pinned: false,
                archived: false,
                unread: false,
            },
            SessionView {
                id: "archived".into(),
                title: "Archivada".into(),
                project: "D:/work/beta".into(),
                backend: "native".into(),
                pinned: false,
                archived: true,
                unread: false,
            },
        ];
        state.session_menu_id = Some("active".into());
        let active = UiTree::new(build_view(&state, 900.0)).layout(Size::new(900.0, 620.0));
        let count = |frame: &openide_ui::Frame, id: &str| {
            frame
                .regions
                .iter()
                .filter(|region| region.id == id)
                .count()
        };
        assert_eq!(count(&active, "session:pinned"), 1);
        assert_eq!(count(&active, "session:active"), 1);
        assert_eq!(count(&active, "session:archived"), 0);
        assert_eq!(count(&active, "session_pin:active"), 1);
        assert_eq!(count(&active, "session_archive:active"), 1);
        assert_eq!(count(&active, "session_delete:active"), 0);
        assert_eq!(count(&active, "toggle_archived_view"), 1);

        state.show_archived = true;
        state.session_menu_id = Some("archived".into());
        let archived = UiTree::new(build_view(&state, 900.0)).layout(Size::new(900.0, 620.0));
        assert_eq!(count(&archived, "session:pinned"), 1);
        assert_eq!(count(&archived, "session:active"), 0);
        assert_eq!(count(&archived, "session:archived"), 1);
        assert_eq!(count(&archived, "session_restore:archived"), 1);
        assert_eq!(count(&archived, "session_delete:archived"), 1);
        assert_eq!(count(&archived, "session_pin:archived"), 0);
    }

    #[test]
    fn interrupted_native_session_has_sidebar_recovery_action() {
        let mut state = empty_state();
        state.sessions = vec![SessionView {
            id: "interrupted".into(),
            title: "Trabajo pendiente".into(),
            project: "C:/project".into(),
            backend: "native".into(),
            pinned: false,
            archived: false,
            unread: false,
        }];
        state.session_menu_id = Some("interrupted".into());
        for recovery in [
            SessionRecoveryView::Resume,
            SessionRecoveryView::Permission,
            SessionRecoveryView::VerifyTool,
            SessionRecoveryView::ReviewBatch,
        ] {
            state
                .session_recovery
                .insert("interrupted".into(), recovery);
            let frame = UiTree::new(build_view(&state, 900.0)).layout(Size::new(900.0, 620.0));
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "session_recover:interrupted"));
        }
        state.session_recovery.insert(
            "interrupted".into(),
            SessionRecoveryView::InvalidJournal("bad event".into()),
        );
        let frame = UiTree::new(build_view(&state, 900.0)).layout(Size::new(900.0, 620.0));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "session_recover:interrupted"));
    }

    #[test]
    fn session_popover_does_not_move_sibling_rows_and_model_list_stays_in_composer() {
        let mut state = empty_state();
        state.sessions = vec![
            SessionView {
                id: "first".into(),
                title: "Primera tarea de trabajo".into(),
                project: "C:/project".into(),
                backend: "codex".into(),
                pinned: false,
                archived: false,
                unread: false,
            },
            SessionView {
                id: "second".into(),
                title: "Segunda tarea".into(),
                project: "C:/project".into(),
                backend: "native".into(),
                pinned: false,
                archived: false,
                unread: false,
            },
        ];
        let closed = UiTree::new(build_view(&state, 1000.0)).layout(Size::new(1000.0, 700.0));
        let sibling_y = closed
            .regions
            .iter()
            .find(|region| region.id == "session:second")
            .unwrap()
            .rect
            .y;
        state.session_menu_id = Some("first".into());
        let open = UiTree::new(build_view(&state, 1000.0)).layout(Size::new(1000.0, 700.0));
        assert_eq!(
            open.regions
                .iter()
                .find(|region| region.id == "session:second")
                .unwrap()
                .rect
                .y,
            sibling_y
        );
        assert!(open
            .regions
            .iter()
            .any(|region| region.id == "session_pin:first"));
        let menu = open
            .regions
            .iter()
            .find(|region| region.id == "session_menu:first")
            .unwrap();
        let pin = open
            .regions
            .iter()
            .find(|region| region.id == "session_pin:first")
            .unwrap();
        assert!(pin.rect.x <= menu.rect.x);
        assert!(pin.rect.x + pin.rect.width >= menu.rect.x + menu.rect.width);
        assert!(pin.rect.x >= 8.0 && pin.rect.x + pin.rect.width <= 1000.0 - 8.0);
        assert!(open
            .regions
            .iter()
            .any(|region| region.id == "session_menu_dismiss"));

        state.session_menu_id = None;
        state
            .composer_model_options
            .push(("gpt-6-sol".into(), "GPT-6 Sol".into()));
        state.composer_model_menu_open = true;
        state.composer_model_list_open = true;
        let open = UiTree::new(build_view(&state, 1000.0)).layout(Size::new(1000.0, 700.0));
        let option = open
            .regions
            .iter()
            .find(|region| region.id == "composer_model:gpt-6-sol")
            .unwrap();
        assert!(option.rect.x > theme::SIDEBAR_WIDTH);
        assert!(open
            .regions
            .iter()
            .any(|region| region.id == "composer_model_dismiss"));
    }

    #[test]
    fn rename_and_archived_delete_require_explicit_commit() {
        let mut state = empty_state();
        state.show_archived = true;
        state.sessions.push(SessionView {
            id: "old".into(),
            title: "Vieja tarea".into(),
            project: "C:/work/alpha".into(),
            backend: "native".into(),
            pinned: false,
            archived: true,
            unread: false,
        });
        state.renaming_session_id = Some("old".into());
        state.rename_draft = "Nuevo nombre".into();
        let rename = UiTree::new(build_view(&state, 900.0)).layout(Size::new(900.0, 620.0));
        for id in ["rename_title", "rename_commit:old", "rename_cancel"] {
            assert!(rename.regions.iter().any(|region| region.id == id));
        }
        let card = rename
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:rename_cancel")
            .unwrap();
        assert!((card.rect.x + card.rect.width / 2.0 - 450.0).abs() < 1.0);
        assert!((card.rect.y + card.rect.height / 2.0 - 310.0).abs() < 1.0);
        assert_eq!(rename.hit_test(Point::new(4.0, 4.0)), Some("rename_cancel"));
        assert!(!rename
            .regions
            .iter()
            .any(|region| region.id == "delete_confirm:old"));

        state.renaming_session_id = None;
        state.pending_delete_session_id = Some("old".into());
        let delete = UiTree::new(build_view(&state, 900.0)).layout(Size::new(900.0, 620.0));
        for id in ["delete_confirm:old", "delete_cancel"] {
            assert!(delete.regions.iter().any(|region| region.id == id));
        }
        assert!(!delete
            .regions
            .iter()
            .any(|region| region.id == "session_delete:old"));
        assert_eq!(delete.hit_test(Point::new(4.0, 4.0)), Some("delete_cancel"));
    }

    #[test]
    fn group_rename_opens_a_centered_alert_dialog() {
        let mut state = empty_state();
        state.session_organization = SessionOrganization::Group;
        state.session_groups.groups.push(SessionGroup {
            id: "team".into(),
            title: "Equipo".into(),
            collapsed: false,
        });
        state.group_rename_id = Some("team".into());
        state.group_rename_draft = "Equipo nuevo".into();
        let frame = UiTree::new(build_view(&state, 900.0)).layout(Size::new(900.0, 620.0));
        for id in [
            "session_group_toggle:team",
            "session_group_menu:team",
            "group_rename_draft",
            "session_group_rename_commit:team",
            "session_group_rename_cancel",
        ] {
            assert!(
                frame.regions.iter().any(|region| region.id == id),
                "missing {id}"
            );
        }
        let card = frame
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:session_group_rename_cancel")
            .unwrap();
        assert!((card.rect.x + card.rect.width / 2.0 - 450.0).abs() < 1.0);
        assert!((card.rect.y + card.rect.height / 2.0 - 310.0).abs() < 1.0);
        assert_eq!(
            frame.hit_test(Point::new(4.0, 4.0)),
            Some("session_group_rename_cancel")
        );
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "session_group_delete:team"));
    }

    #[test]
    fn archive_and_group_removal_use_the_shared_alert_dialog() {
        let mut state = empty_state();
        state.sessions.push(SessionView {
            id: "task".into(),
            title: "Tarea".into(),
            project: "C:/work".into(),
            backend: "native".into(),
            pinned: false,
            archived: false,
            unread: false,
        });
        state.pending_archive_session_id = Some("task".into());
        let archive = UiTree::new(build_view(&state, 900.0)).layout(Size::new(900.0, 620.0));
        assert!(archive
            .regions
            .iter()
            .any(|region| region.id == "archive_confirm:task"));
        assert_eq!(
            archive.hit_test(Point::new(4.0, 4.0)),
            Some("archive_cancel")
        );

        state.pending_archive_session_id = None;
        state.session_groups.groups.push(SessionGroup {
            id: "group".into(),
            title: "Equipo".into(),
            collapsed: false,
        });
        state.pending_delete_group_id = Some("group".into());
        let group = UiTree::new(build_view(&state, 900.0)).layout(Size::new(900.0, 620.0));
        assert!(group
            .regions
            .iter()
            .any(|region| region.id == "session_group_delete_confirm:group"));
        assert_eq!(
            group.hit_test(Point::new(4.0, 4.0)),
            Some("session_group_menu_dismiss")
        );
    }

    #[test]
    fn narrow_window_hides_right_dock_and_keeps_multiline_composer() {
        let mut state = empty_state();
        state.prompt = "Una tarea con instrucciones\nen varias líneas".into();
        let frame = UiTree::new(build_view(&state, 520.0)).layout(Size::new(520.0, 320.0));
        assert!(!frame.regions.iter().any(|region| region.id == "dock_tab:1"));
        let prompt = frame
            .regions
            .iter()
            .find(|region| region.id == "prompt")
            .unwrap();
        let submit = frame
            .regions
            .iter()
            .find(|region| region.id == "run_toggle")
            .unwrap();
        assert!(prompt.rect.height >= 60.0);
        assert!(submit.rect.x + submit.rect.width <= 520.0);
        assert!(submit.rect.y + submit.rect.height <= 320.0);
    }

    #[test]
    fn selected_tool_exposes_real_arguments_and_output_in_activity() {
        let mut state = empty_state();
        state.side_tab = SideTab::Activity;
        state.selected_tool_id = Some("call-1".into());
        state.transcript.push(TranscriptItem::Tool {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: json!({"path": "src/main.rs"}),
            output: Some("fn main() {}".into()),
            is_error: false,
        });
        let frame = UiTree::new(build_view(&state, 1200.0)).layout(Size::new(1200.0, 620.0));
        let visible = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(visible.contains("src/main.rs"), "{visible}");
        assert!(visible.contains("fn main() {}"), "{visible}");
        assert!(
            frame
                .regions
                .iter()
                .filter(|region| region.id == "tool:call-1")
                .count()
                >= 2
        );
    }

    #[test]
    fn native_command_activity_shows_state_and_confirmed_termination() {
        let mut state = empty_state();
        state.selected_session = Some("native-1".into());
        state.selected_backend = "native".into();
        state.side_tab = SideTab::Activity;
        state.native_commands = vec![
            PersistentCommandSessionInfo {
                id: "running-1".into(),
                pty: true,
                exit: None,
                has_output: true,
            },
            PersistentCommandSessionInfo {
                id: "finished-1".into(),
                pty: false,
                exit: Some(PersistentCommandExit::Exited { code: Some(0) }),
                has_output: false,
            },
        ];
        let frame = UiTree::new(build_view(&state, 1200.0)).layout(Size::new(1200.0, 720.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "native_command_terminate:running-1"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "native_command_terminate:finished-1"));
        let text = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("PTY · En ejecución · salida pendiente"),
            "{text}"
        );
        assert!(text.contains("Tuberías · Finalizado · código 0"), "{text}");

        state.pending_native_command_termination = Some("running-1".into());
        let confirm = UiTree::new(build_view(&state, 1200.0)).layout(Size::new(1200.0, 720.0));
        assert!(confirm
            .regions
            .iter()
            .any(|region| region.id == "native_command_terminate_confirm:running-1"));
        assert!(confirm
            .regions
            .iter()
            .any(|region| region.id == "native_command_terminate_cancel"));
    }

    #[test]
    fn narrow_timeline_expands_only_selected_tool() {
        let mut state = empty_state();
        for id in ["one", "two"] {
            state.transcript.push(TranscriptItem::Tool {
                id: id.into(),
                name: "read_file".into(),
                arguments: json!({"path": id}),
                output: Some(format!("result-{id}")),
                is_error: false,
            });
        }
        state.selected_tool_id = Some("two".into());
        let frame = UiTree::new(build_view(&state, 520.0)).layout(Size::new(520.0, 620.0));
        let text = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("result-two"));
        assert!(!text.contains("result-one"));
    }

    #[test]
    fn preview_syntax_preserves_unicode_and_tracks_comments_across_lines() {
        let mut state = PreviewLexState::default();
        let first = preview_source_spans(
            "pub fn mostrar() { /* nota",
            PreviewLanguage::Rust,
            &mut state,
        );
        assert_eq!(
            first
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>(),
            "pub fn mostrar() { /* nota"
        );
        assert!(first
            .iter()
            .any(|span| span.text == "pub" && span.color == theme::CODE_KEYWORD));
        assert!(state.block_comment);
        let second = preview_source_spans(
            "más */ let título = \"áé漢🙂\";",
            PreviewLanguage::Rust,
            &mut state,
        );
        assert!(!state.block_comment);
        assert!(
            second
                .iter()
                .any(|span| span.text == " let" && span.color == theme::CODE_KEYWORD)
                || second
                    .iter()
                    .any(|span| span.text == "let" && span.color == theme::CODE_KEYWORD)
        );
        assert!(second
            .iter()
            .any(|span| span.text == "\"áé漢🙂\"" && span.color == theme::CODE_STRING));

        let json = preview_source_spans(
            "{\"clave\": 42, \"ok\": true}",
            PreviewLanguage::Json,
            &mut PreviewLexState::default(),
        );
        assert!(json
            .iter()
            .any(|span| span.text == "42" && span.color == theme::CODE_NUMBER));
        assert!(json
            .iter()
            .any(|span| span.text == "true" && span.color == theme::CODE_KEYWORD));
    }

    #[test]
    fn preview_bounds_long_unicode_lines_and_uses_a_number_gutter() {
        let source = format!("{}\nconst x = 2;", "á漢🙂".repeat(150));
        let (bounded, truncated) = bounded_preview_line(source.lines().next().unwrap());
        assert!(truncated);
        assert_eq!(bounded.chars().count(), PREVIEW_VISIBLE_CHARS_PER_LINE);
        let preview = FilePreviewView {
            tab_id: 7,
            path: "src/página.ts".into(),
            page_index: 0,
            content: Some(Ok(PreviewPage {
                text: source,
                start_line: 0,
                end_line: 2,
                total_lines: Some(2),
                has_more: false,
                truncated: true,
            })),
        };
        let frame =
            UiTree::new(file_preview_content(&preview, 240.0)).layout(Size::new(240.0, 480.0));
        let painted = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { position, text, .. } => {
                    assert!(position.x >= 0.0 && position.x < 240.0);
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(painted.contains("á漢🙂"));
        assert!(painted.contains('1'));
        assert!(painted.contains('2'));
        // The renderer may split a wrapped label into multiple text commands.
        assert!(painted.contains("línea larga"));
        assert!(painted.contains("abreviada"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "dock_close:7"));
    }

    #[test]
    fn markdown_preview_distinguishes_heading_and_fenced_code() {
        let mut state = PreviewLexState::default();
        let heading = preview_source_spans("# Título", PreviewLanguage::Markdown, &mut state);
        assert!(heading
            .iter()
            .any(|span| span.text == "#" && span.color == theme::CODE_PUNCTUATION));
        assert!(heading
            .iter()
            .any(|span| span.text == " Título" && span.font_size == 13.0));
        preview_source_spans("```rust", PreviewLanguage::Markdown, &mut state);
        assert!(state.markdown_fence);
        let code = preview_source_spans("fn main()", PreviewLanguage::Markdown, &mut state);
        assert_eq!(code[0].color, theme::CODE_STRING);
        preview_source_spans("```", PreviewLanguage::Markdown, &mut state);
        assert!(!state.markdown_fence);
    }

    #[test]
    fn command_center_distinguishes_loading_error_truncation_and_no_matches() {
        let mut state = empty_state();
        state.show_command_center = true;
        state.command_query = "aguja".into();
        let painted = |state: &ViewState| {
            UiTree::new(build_command_center(state, 520.0, 380.0))
                .layout(Size::new(520.0, 380.0))
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        };

        let empty = painted(&state);
        assert!(empty.contains("No hay resultados"), "{empty}");
        assert!(empty.contains("Sin resultados"), "{empty}");
        assert!(empty.contains("Navegar"), "{empty}");
        assert!(empty.contains("Abrir"), "{empty}");
        state.command_search_loading = true;
        let loading = painted(&state);
        assert!(!loading.contains("Buscando"), "{loading}");
        assert!(!loading.contains("No hay resultados"), "{loading}");
        let mut loading_tree = UiTree::new(build_command_center(&state, 520.0, 380.0));
        let loading_frame = loading_tree.layout(Size::new(520.0, 380.0));
        assert!(
            loading_tree.is_animating(),
            "loading spinner should repaint"
        );
        let result_viewport = loading_frame
            .regions
            .iter()
            .find(|region| {
                region.id == "command_results" && region.kind == openide_ui::HitKind::Scroll
            })
            .expect("search result viewport")
            .rect;
        let spinner = loading_frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Spinner { rect, .. } => Some(*rect),
                _ => None,
            })
            .expect("centered search spinner");
        let spinner_center_x = spinner.x + spinner.width / 2.0;
        let spinner_center_y = spinner.y + spinner.height / 2.0;
        assert!((spinner_center_x - result_viewport.x - result_viewport.width / 2.0).abs() < 4.0);
        assert!((spinner_center_y - result_viewport.y - result_viewport.height / 2.0).abs() < 4.0);
        assert!(!loading_frame.regions.iter().any(|region| {
            region.id == "command_results" && region.kind == openide_ui::HitKind::Scrollbar
        }));

        state.command_search_loading = false;
        state.command_search_errors = vec!["Archivos: acceso denegado".into()];
        let failed = painted(&state);
        assert!(
            failed.contains("No se pudo completar la búsqueda"),
            "{failed}"
        );
        assert!(!failed.contains("No hay resultados"), "{failed}");

        state.command_results.push(ResultItem {
            target: crate::command_center::Target::OpenSettings,
            section: Scope::Commands,
            title: "Configuración".into(),
            subtitle: "Preferencias".into(),
        });
        let partial = painted(&state);
        assert!(partial.contains("Resultados parciales"), "{partial}");
        assert!(partial.contains("Configuración"), "{partial}");
        assert!(partial.contains("1 resultado"), "{partial}");

        state.command_search_errors.clear();
        state.command_search_truncated = true;
        let truncated = painted(&state);
        assert!(truncated.contains("Se muestran los primeros resultados"));
        assert!(truncated.contains("1+ resultados"), "{truncated}");
    }

    #[test]
    fn command_center_uses_compact_search_and_tabs_within_modal() {
        let state = empty_state();
        let viewport = Size::new(560.0, 500.0);
        let frame = UiTree::new(build_command_center(
            &state,
            viewport.width,
            viewport.height,
        ))
        .layout(viewport);
        let search = frame
            .regions
            .iter()
            .find(|region| region.id == "command_query")
            .expect("search input");
        assert!(search.rect.height <= 34.0);
        assert!(search.rect.x >= 10.0);
        assert!(search.rect.x + search.rect.width <= viewport.width - 10.0);
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "command_close"));
        assert!(frame.commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::Icon {
                    icon: Icon::CloseCircle,
                    ..
                }
            )
        }));
        for id in [
            "command_scope:all",
            "command_scope:commands",
            "command_scope:sessions",
            "command_scope:files",
        ] {
            let tab = frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap_or_else(|| panic!("missing {id}"));
            assert!(tab.rect.height <= 28.0);
            assert!(tab.rect.x + tab.rect.width <= viewport.width * 0.65);
        }
        let text_y = |target: &str| {
            frame.commands.iter().find_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text == target => Some(position.y),
                _ => None,
            })
        };
        let key_y = text_y("Enter").expect("footer key cap");
        for caption in ["Sin resultados", "Navegar", "Abrir"] {
            let caption_y = text_y(caption).expect("footer caption");
            assert!(
                (caption_y - key_y).abs() < 2.0,
                "{caption} y={caption_y}, key cap y={key_y}"
            );
        }
    }

    #[test]
    fn command_result_hover_surface_stops_before_scrollbar() {
        let mut state = empty_state();
        state.command_results = (0..20)
            .map(|index| ResultItem {
                target: crate::command_center::Target::OpenSettings,
                section: Scope::Commands,
                title: format!("Acción {index}"),
                subtitle: "Abrir preferencias".into(),
            })
            .collect();
        let frame =
            UiTree::new(build_command_center(&state, 560.0, 420.0)).layout(Size::new(560.0, 420.0));
        let rail = frame
            .regions
            .iter()
            .find(|region| {
                region.id == "command_results" && region.kind == openide_ui::HitKind::Scrollbar
            })
            .expect("result scrollbar");
        let row = frame
            .regions
            .iter()
            .find(|region| region.id == "command_result:0")
            .expect("first result row");
        assert!(row.rect.x + row.rect.width < rail.rect.x);
    }
}
