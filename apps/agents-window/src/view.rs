use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};

use openide_core::{OutputStream, PlanSnapshot, SessionSort, TurnMode};
use openide_harness::{
    CommandIsolation, HostedWebSearchMode, PersistentCommandExit, PersistentCommandSessionInfo,
    SubagentStatus, ToolPermissions,
};
use openide_ui::{
    ActiveHover, AlertDialog, Button, ButtonVariant, Color, Component, ControlSize, DropdownList,
    DropdownMenu, DropdownMenuEntry, DropdownMenuItem, DropdownOption, Element, Empty, FontFamily,
    Icon, IconButton as UiIconButton, Kbd, PopoverPlacement, ScrollArea, Search, SegmentedControl,
    Separator, Skeleton, Tab, Text as UiText, TextRole, TextSpan, Tooltip, TooltipPlacement, TooltipSpec, TreeGuides,
    TreeRow, ICON_SIZE,
};
use serde_json::Value;

#[cfg(test)]
use crate::git_review::{ReviewFile, ReviewLine, ReviewLineKind};

use crate::{
    automation_view::{self, AutomationViewState},
    command_center::{ResultItem, Scope},
    composer_selection::ModelRouteOption,
    composer_ui::{build_composer, ComposerProps},
    files::PreviewPage,
    git_review::{DiffStage, FileDiffDetail, GitReview},
    github::GithubStatus,
    github_account::{GithubAccountStatus, GithubSignInView},
    github_contributions::GithubContributionsStatus,
    home,
    marketplace_view::{self, MarketplaceViewState},
    model_catalog::CatalogSnapshot,
    provider_settings::{CatalogModelsFilter, CatalogSettingsTab, ProviderConnectionView},
    session_groups::{SessionGroup, SessionGroups},
    settings_preferences::TerminalShell,
    settings_ui::{self, SettingsPage},
    terminal::TerminalSnapshot,
    theme,
    transcript::TranscriptItem,
};

mod command_center;
#[path = "dock_activity.rs"]
mod dock_activity;
#[path = "dock_chrome.rs"]
pub(crate) mod dock_chrome;
#[cfg(test)]
#[path = "dock_visual_tests.rs"]
mod dock_visual_tests;
#[path = "document_markdown_ui.rs"]
pub(crate) mod document_markdown_ui;
#[path = "git_panel.rs"]
mod git_panel;
#[path = "ide_activity_ui.rs"]
mod ide_activity_ui;
#[path = "ide_search_ui.rs"]
mod ide_search_ui;
#[path = "ide_ui.rs"]
mod ide_ui;
#[cfg(test)]
#[path = "ide_visual_tests.rs"]
pub(crate) mod ide_visual_tests;
#[cfg(test)]
#[path = "agent_chrome_visual_tests.rs"]
mod agent_chrome_visual_tests;
#[cfg(test)]
#[path = "tab_bar_visual_tests.rs"]
mod tab_bar_visual_tests;
pub(crate) use command_center::{build_command_center, command_result_action, command_result_row};
use crate::sidebar_chrome::{NAV_ROW_HEIGHT as SIDEBAR_ROW_HEIGHT, TOOL_SIZE as SIDEBAR_TOOL_SIZE};

const GAP: f32 = 4.0;
const HEADER: f32 = 48.0;
const RIGHT_HEADER_HEIGHT: f32 = crate::dock_tab_ui::HEADER_HEIGHT;
/// The center and the right dock are two panes of the same workspace surface.
/// Keep their divider independent of the gap around the separate left dock.
const RIGHT_DIVIDER: f32 = 1.0;
const MIN_CENTER_WITH_RIGHT: f32 = 480.0;
const MIN_CENTER_COMPACT: f32 = 320.0;
const MIN_RIGHT_DOCK: f32 = 320.0;
const WORKSPACE_RADIUS: f32 = 12.0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SideTab {
    #[default]
    Context,
    Files,
    Activity,
    CodeReview,
    SessionChanges,
    FilePreview,
    CodeSnapshot,
    Terminal,
    Browser,
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
    pub full_path: Option<String>,
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
pub struct SubagentInputEditView {
    pub child_session_id: String,
    pub input_id: String,
    pub expected_revision: u64,
    pub prompt: String,
}

#[derive(Clone, Debug)]
pub struct SubagentSessionView {
    pub child_session_id: String,
    pub parent_session_id: String,
    pub title: String,
    pub status: SubagentStatus,
    pub cancellation_requested: bool,
    pub open_inputs: usize,
    pub inputs: Vec<openide_harness::SubagentInputSnapshot>,
    pub dispatch_error: Option<String>,
    pub depth: u8,
}

#[derive(Clone, Debug)]
pub struct ToolVerificationView {
    pub goal_receipt: Option<crate::resume_worker::GoalReceiptInspection>,
    pub session_id: String,
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    pub error: Option<String>,
    pub hooks: Vec<crate::transcript::HookActivity>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeAutomationSessionView {
    pub automation_id: String,
    pub run_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub manual: bool,
    pub scheduled_at_ms: i64,
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
    /// Display-only origin fields; frozen authority and credentials stay private.
    pub native_automation: Option<NativeAutomationSessionView>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionRecoveryView {
    StartAutomation,
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
            Self::StartAutomation => "Iniciar ejecución registrada",
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

/// One known agent CLI and where OpenIDE found it, if anywhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentCliRowView {
    pub id: String,
    pub label: String,
    pub resolved: Option<String>,
    /// The path the user typed, empty when detection is automatic.
    pub manual: String,
}

/// The review model choice as Settings shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewModelView {
    pub mode: crate::review_model::ReviewModelMode,
    /// The saved manual choice as "provider · model", empty when none.
    pub manual: String,
    /// What automatic mode would use right now, empty when it would keep the
    /// selected model.
    pub automatic: String,
    /// The provider and model of the saved manual choice, either may be empty.
    pub manual_provider: String,
    pub manual_model: String,
}

/// One enabled Claude Code plugin that ships skills.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginSkillsRowView {
    pub id: String,
    pub skill_count: usize,
    pub enabled: bool,
}

/// One Claude Code hook found on this machine and whether OpenIDE runs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatHookRowView {
    /// The plugin that defines it, empty for the user's own settings.
    pub origin: String,
    pub event: String,
    pub matcher: String,
    pub command: String,
    pub enabled: bool,
    /// False for events OpenIDE does not act on yet; the switch is disabled.
    pub runs_today: bool,
}

/// One pending tool call from an external CLI, awaiting the user's decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalApprovalView {
    pub title: String,
    pub detail: String,
}

pub struct ViewState {
    pub(crate) ide: crate::ide_workspace::IdeWorkspaceState,
    pub(crate) ide_search: crate::ide_search::IdeSearchState,
    pub(crate) document_preview: Option<crate::document_preview::PreviewView>,
    pub(crate) markdown_preview: Option<crate::document_preview::MarkdownPreviewTab>,
    pub(crate) ide_status: crate::ide_status_ui::IdeStatusView,
    pub workspace: String,
    pub workspace_draft: String,
    /// Cached for the composer so opening the Git review tab is not required.
    pub project_branch: Option<String>,
    pub(crate) composer_branches: crate::git_branches::BranchPickerView,
    pub prompt: String,
    pub composer_attachments: Vec<ComposerAttachmentView>,
    pub(crate) composer_attachment_visuals:
        BTreeMap<String, crate::composer_attachments::AttachmentVisual>,
    pub composer_attachment_loading: bool,
    pub(crate) composer_image_viewer: Option<crate::composer_attachments::ImageGalleryView>,
    pub composer_add_menu_open: bool,
    pub composer_listening: bool,
    /// Temporary editor presentation; the draft itself remains unchanged.
    pub composer_expanded: bool,
    /// Persistent objective for the selected project and session draft.
    pub composer_goal: Option<String>,
    pub composer_goal_editor_open: bool,
    pub composer_goal_draft: String,
    pub(crate) native_goal: crate::native_goal_ui::NativeGoalUiState,
    pub(crate) native_instructions: crate::native_instruction_ui::NativeInstructionUiState,
    pub(crate) native_context: crate::native_context_ui::NativeContextUiState,
    pub composer_permissions_open: bool,
    pub(crate) composer_full_access_confirm:
        Option<crate::composer_permissions_ui::FullAccessConfirmation>,
    pub(crate) composer_suggestions: crate::composer_suggestions::ComposerSuggestionsState,
    pub composer_project_menu_open: bool,
    pub(crate) environment: crate::environment_ui::EnvironmentUiState,
    pub(crate) project_actions: crate::project_actions::ProjectActionsState,
    /// Absolute workspace path and display name, in stable menu order.
    pub composer_project_options: Vec<(String, String)>,
    pub endpoint: String,
    pub model: String,
    pub native_provider_id: String,
    pub native_output_limit_draft: String,
    pub native_output_limit_field: String,
    pub native_output_limit_responses: bool,
    pub native_output_limit_messages: bool,
    pub native_output_limit_load_error: Option<String>,
    pub hosted_web_search_mode: Option<HostedWebSearchMode>,
    pub catalog: Option<Arc<CatalogSnapshot>>,
    pub catalog_status: String,
    pub catalog_refreshing: bool,
    pub(crate) provider_connections:
        BTreeMap<String, crate::provider_connections::ConnectionSummary>,
    pub(crate) provider_connect: Option<crate::provider_connections::ConnectView>,
    pub(crate) provider_usage: BTreeMap<String, crate::provider_usage::ProviderUsageSnapshot>,
    pub(crate) provider_usage_open: bool,
    pub(crate) provider_usage_refreshing: bool,
    pub catalog_browse_provider: String,
    pub catalog_provider_query: String,
    pub catalog_model_query: String,
    pub(crate) catalog_models_filter: CatalogModelsFilter,
    pub(crate) catalog_settings_tab: CatalogSettingsTab,
    pub(crate) catalog_expanded_model: Option<String>,
    pub(crate) catalog_provider_menu_open: bool,
    pub(crate) native_provider_definitions: BTreeMap<String, ProviderConnectionView>,
    pub(crate) native_provider_registry_error: Option<String>,
    pub backend_options: Vec<(String, String)>,
    pub selected_backend: String,
    pub backend_menu_open: bool,
    pub composer_model: String,
    pub composer_model_options: Vec<(String, String)>,
    pub composer_routes: Vec<ModelRouteOption>,
    pub(crate) composer_model_metadata: BTreeMap<String, crate::model_metadata::ModelMetadataView>,
    pub composer_provider_status: BTreeMap<String, ProviderModelStatus>,
    pub composer_model_menu_open: bool,
    pub composer_model_list_open: bool,
    /// Transient filter for the model list; never persisted with provider settings.
    pub composer_model_query: String,
    /// Provider sections retain their collapsed state across menu openings.
    pub composer_collapsed_model_providers: std::collections::BTreeSet<String>,
    /// Search results initially expand their groups without overwriting preferences.
    pub composer_search_collapsed_model_providers: std::collections::BTreeSet<String>,
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
    pub composer_context_open: bool,
    pub composer_context_details_open: bool,
    /// A failed submission shown by the composer even before a turn starts.
    pub composer_error: Option<String>,
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
    pub native_full_access: bool,
    pub native_command_isolation: CommandIsolation,
    pub external_agents: crate::external_agent_settings::ExternalAgentSettings,
    pub external_approval: Option<ExternalApprovalView>,
    pub compat_hooks: Vec<CompatHookRowView>,
    pub plugin_skills: Vec<PluginSkillsRowView>,
    pub agent_clis: Vec<AgentCliRowView>,
    /// The check command saved for the open project, shown in Settings.
    pub turn_check_command: String,
    pub turn_check_failure: Option<String>,
    pub review_model: ReviewModelView,
    pub compat_hooks_error: Option<String>,
    pub session_approval_grants: Vec<SessionApprovalGrantView>,
    pub(crate) native_commands: Vec<PersistentCommandSessionInfo>,
    pub(crate) native_commands_error: Option<String>,
    pub(crate) native_command_control:
        crate::native_command_control_ui::NativeCommandControlUiState,
    pub(crate) native_command_output: crate::native_command_output_ui::NativeCommandOutputUiState,
    pub(crate) native_command_history:
        crate::native_command_history_ui::NativeCommandHistoryUiState,
    pub(crate) pending_native_command_termination: Option<String>,
    pub(crate) native_command_terminating: Option<String>,
    pub(crate) native_hooks: crate::native_hook_settings::HookUiState,
    pub(crate) native_exec_rules: crate::exec_policy_settings::ExecPolicyUiState,
    pub(crate) native_skills: crate::native_skills_settings::NativeSkillsUiState,
    pub(crate) plugins: crate::plugins_ui::PluginsUiState,
    pub keymap_profile: crate::keymap::KeymapProfile,
    pub vim_mode: bool,
    pub settings_keymap_menu_open: bool,
    pub settings_reduced_motion: bool,
    pub settings_reopen_last_workspace: bool,
    pub settings_terminal_shell: TerminalShell,
    pub settings_terminal_shell_draft: String,
    pub settings_terminal_shell_menu_open: bool,
    pub(crate) settings_choice_menu_open: Option<String>,
    pub settings_terminal_shell_editing: bool,
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
    pub(crate) mcp_inspection: crate::mcp_inspection::McpInspectionUiState,
    pub(crate) conversation_activity: crate::conversation_activity::ConversationActivityUi,
    pub(crate) conversation_details: crate::conversation_activity::ConversationDetailsUi,
    pub(crate) stream_presentation: crate::stream_presentation::StreamingPresentation,
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
    pub settings_nav_query: String,
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
    /// Transient panel height; the saved preference and visibility stay logical.
    pub bottom_presentation_height: Option<f32>,
    /// Transient in-flow environment reveal; None uses its open state.
    pub environment_presentation_progress: Option<f32>,
    pub side_tab: SideTab,
    /// Pages are independent so switching tabs preserves the user's place.
    pub activity_page: usize,
    pub review_page: usize,
    pub dock_tabs: Vec<DockTabView>,
    pub dock_recently_closed: Vec<DockRecentlyClosedView>,
    pub dock_overview_query: String,
    pub show_dock_overview: bool,
    pub show_dock_add_menu: bool,
    pub notifications: crate::notifications::NotificationCenter,
    pub show_dock_tabs_menu: bool,
    pub file_preview: Option<FilePreviewView>,
    pub(crate) code_editor: Option<crate::code_editor_ui::CodeEditorView>,
    pub(crate) editor_workbench: crate::editor_workbench_ui::EditorWorkbenchView,
    pub(crate) debug: crate::debug_ui::DebugView,
    pub(crate) debug_open: bool,
    pub(crate) code_snapshot: Option<crate::code_snapshot_ui::CodeSnapshotView>,
    pub(crate) code_editor_quit_confirm: bool,
    pub(crate) code_editor_pending_saves: bool,
    pub(crate) chat_diffs: crate::chat_diff_ui::ChatDiffUiState,
    pub(crate) code_editor_reload_confirm: Option<crate::code_editor_files::EditorFileScope>,
    pub git_review: Option<Result<Arc<GitReview>, String>>,
    pub git_action_pending: bool,
    pub git_action_error: Option<String>,
    pub(crate) session_changes: crate::session_changes_ui::SessionChangesUiState,
    pub(crate) browser: crate::browser_ui::BrowserUiState,
    pub review_detail: Option<ReviewDetailView>,
    pub terminal_tabs: Vec<DockTabView>,
    pub terminal_menu_open: bool,
    pub terminal_controls: crate::terminal_dock_ui::TerminalControlsView,
    pub terminal_maximized: bool,
    pub show_terminal_panel: bool,
    pub terminal_panel_height: f32,
    pub terminal_snapshot: Option<TerminalSnapshot>,
    pub terminal_input: String,
    pub terminal_error: Option<String>,
    pub pending_permissions: Vec<PermissionView>,
    pub approval_ui: crate::approval_ui::ApprovalUiState,
    pub composer_pending: Option<crate::composer_pending::PendingComposerSelection>,
    pub pending_subagent_permissions: Vec<SubagentPermissionView>,
    pub active_subagents: Vec<SubagentRunView>,
    pub subagent_sessions: Vec<SubagentSessionView>,
    pub expanded_subagent_inputs: Option<String>,
    pub subagent_input_edit: Option<SubagentInputEditView>,
    pub recoverable_subagent: Option<SubagentRunView>,
    pub pending_tool_verification: Option<ToolVerificationView>,
    pub pending_batch_discard: Option<AbortedToolBatchView>,
    pub tool_verification_output: String,
    pub tool_verification_is_error: bool,
    pub selected_tool_id: Option<String>,
    pub show_jump_to_bottom: bool,
    pub github_account: GithubAccountStatus,
    pub github_account_menu_open: bool,
    pub github_account_refreshing: bool,
    pub github_account_notice: Option<String>,
    pub github_sign_in: Option<GithubSignInView>,
    pub github_contributions: GithubContributionsStatus,
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

/// A labelled navigation button with a leading icon.
fn icon_button(
    id: impl Into<String>,
    icon: Icon,
    title: impl Into<String>,
    fill: Color,
    ink: Color,
) -> Element {
    UiIconButton::new(id, icon, theme::action_palette(fill, ink))
        .label(title)
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
        .min_size(0.0, 30.0)
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
    if crate::ide_workspace::active(state) {
        return crate::ide_workspace::layout(state, viewport);
    }
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
    let row_width = (width - 18.0).max(0.0);
    let menu_width = row_width.max(246.0);
    let recovery = state.session_recovery.get(&session.id);
    let status_marker = if recovery.is_some() {
        Element::icon(Icon::Refresh).foreground(theme::BRAND)
    } else if session.unread {
        Element::spacer()
            .size(Some(6.0), Some(6.0))
            .background(theme::BRAND)
            .radius(3.0)
    } else if selected && state.running {
        Element::icon(Icon::Clock).foreground(theme::FOREGROUND)
    } else if session.pinned {
        Element::icon(Icon::Pin).foreground(theme::FOREGROUND_SUBTLE)
    } else if let Some(agent) = session.backend.strip_prefix(CLI_BACKEND) {
        Element::icon(crate::provider_icons::icon_for_agent_cli(agent))
            .foreground(theme::FOREGROUND_SUBTLE)
    } else {
        Element::spacer()
    };
    // The status slot gives the 16px icon a 4px left inset, inside the
    // rounded hover surface. Reserving it also keeps idle/running titles fixed.
    let status_marker = status_marker
        .size(
            Some(if session.unread && recovery.is_none() {
                6.0
            } else {
                16.0
            }),
            Some(if session.unread && recovery.is_none() {
                6.0
            } else {
                16.0
            }),
        )
        .centered()
        .size(Some(SIDEBAR_TOOL_SIZE), Some(SIDEBAR_ROW_HEIGHT));
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
    actions.push(DropdownMenuEntry::Separator);
    actions.push(session_menu_action(
        format!("session_delete:{}", session.id),
        "Eliminar…",
        Icon::Trash,
        true,
    ));
    // The menu owns its popover. The parent row remains one full-width action,
    // while its last child has a more specific hit region and takes precedence.
    let menu_action_id = format!("session_menu:{}", session.id);
    let menu_button = UiIconButton::new(
        menu_action_id.clone(),
        Icon::More,
        theme::action_palette(Color::TRANSPARENT, theme::FOREGROUND_SUBTLE),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Dense)
    .render()
    .background(Color::TRANSPARENT)
    .hover_background(Color::TRANSPARENT)
    .pressed_background(Color::TRANSPARENT)
    .transition_ms(0)
    .padding(4.0)
    .min_size(0.0, 0.0)
    .size(Some(SIDEBAR_TOOL_SIZE), Some(SIDEBAR_TOOL_SIZE));
    let menu_button = if menu_open {
        menu_button
    } else {
        Tooltip::new(
            menu_button,
            menu_action_id.clone(),
            "Más acciones",
            theme::component_palette(),
        )
        .placement(TooltipPlacement::Right)
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
    .context_menu_style()
    .width(menu_width)
    .render();
    let menu_button = menu_button
        .centered_y()
        .size(Some(SIDEBAR_TOOL_SIZE), Some(SIDEBAR_ROW_HEIGHT));
    let row = Element::clickable(
        format!("session:{}", session.id),
        Element::row(vec![
            status_marker,
            Element::faded_text(&session.title, 24.0)
                .foreground(if selected || session.unread {
                    theme::BRAND
                } else {
                    theme::FOREGROUND
                })
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .min_size(0.0, 0.0)
                .size(None, Some(SIDEBAR_ROW_HEIGHT))
                .flex(1.0),
            menu_button,
        ])
        .gap(4.0)
        .size(Some(row_width), Some(SIDEBAR_ROW_HEIGHT)),
    )
    .size(Some(row_width), Some(SIDEBAR_ROW_HEIGHT))
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
    .radius(8.0);
    theme::action_palette(
        if selected || menu_open {
            theme::SELECTED
        } else {
            theme::SIDEBAR
        },
        theme::FOREGROUND,
    )
    .interactive_surface(row)
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
        .size(Some(ICON_SIZE), Some(SIDEBAR_ROW_HEIGHT)),
    ])
    .gap(6.0)
    .size(None, Some(SIDEBAR_ROW_HEIGHT));
    TreeRow::new(
        move |content| Element::clickable(action_id, content),
        Element::icon(openide_ui::folder_icon(project, !collapsed))
            .foreground(theme::FOREGROUND_SUBTLE),
        Element::truncated_text(workspace_name(project))
            .foreground(theme::FOREGROUND)
            .font_family(FontFamily::Sans)
            .font_size(12.0),
        TreeGuides::root(),
        theme::action_palette(theme::SIDEBAR, theme::FOREGROUND),
    )
    .trailing(trailing)
    .width(row_width)
    .height(SIDEBAR_ROW_HEIGHT)
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
    .size(ControlSize::Dense)
    .width(SIDEBAR_TOOL_SIZE)
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
        .placement(TooltipPlacement::Right)
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
    .context_menu_style()
    .width(202.0)
    .render();
    let row = Element::clickable(
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
        .padding(2.0)
        .size(Some(row_width), Some(SIDEBAR_ROW_HEIGHT)),
    )
    .size(Some(row_width), Some(SIDEBAR_ROW_HEIGHT))
    .background(theme::SIDEBAR)
    .hover_background(theme::hover_for(theme::SIDEBAR))
    .pressed_background(theme::SELECTED)
    .radius(7.0);
    theme::action_palette(theme::SIDEBAR, theme::FOREGROUND).interactive_surface(row)
}

fn sidebar_nav_action(
    id: &str,
    icon: Icon,
    title: &str,
    shortcut: &str,
    width: f32,
    selected: bool,
) -> Element {
    let icon = Element::icon(icon)
        .foreground(if selected {
            theme::FOREGROUND
        } else {
            theme::FOREGROUND_SUBTLE
        })
        .size(Some(16.0), Some(16.0));
    let kbd = Kbd::new(shortcut, theme::component_palette())
        .max_width((width - 92.0).max(0.0))
        .render();
    let centered = |element: Element| element.centered_y().size(None, Some(SIDEBAR_ROW_HEIGHT));
    let row = Element::clickable(
        id,
        Element::row(vec![
            Element::spacer().size(Some(4.0), None),
            Element::row(vec![
                centered(icon).size(Some(16.0), Some(SIDEBAR_ROW_HEIGHT)),
                Element::truncated_text(title)
                    .foreground(theme::FOREGROUND)
                    .font_family(FontFamily::Sans)
                    .font_size(13.0)
                    .flex(1.0),
                centered(kbd),
            ])
            .gap(8.0)
            .flex(1.0),
            Element::spacer().size(Some(8.0), None),
        ])
        .size(Some(width), Some(SIDEBAR_ROW_HEIGHT)),
    )
    .size(Some(width), Some(SIDEBAR_ROW_HEIGHT))
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
    .radius(8.0);
    theme::action_palette(
        if selected {
            theme::SELECTED
        } else {
            theme::SIDEBAR
        },
        theme::FOREGROUND,
    )
    .interactive_surface(row)
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
    ScrollArea::uniform_list(
        "sessions_list",
        count,
        SIDEBAR_ROW_HEIGHT,
        move |index| match &rows[index] {
            SessionListRow::Section(title) => Element::panel(eyebrow(*title))
                .padding(6.0)
                .size(Some(row_width), Some(SIDEBAR_ROW_HEIGHT)),
            SessionListRow::Warning(message) => Element::panel(
                Element::truncated_text(message)
                    .foreground(theme::DESTRUCTIVE)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0),
            )
            .padding(6.0)
            .size(Some(row_width), Some(SIDEBAR_ROW_HEIGHT)),
            SessionListRow::Empty(message) => {
                Element::panel(
                    UiText::new(*message, theme::component_palette())
                        .role(TextRole::Body)
                        .muted()
                        .render(),
                )
                    .padding(6.0)
                    .size(Some(row_width), Some(SIDEBAR_ROW_HEIGHT))
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
        },
    )
    .tint(theme::FOREGROUND_SUBTLE)
    .render()
    .padding(6.0)
    .flex(1.0)
}

pub(crate) fn sidebar_account_footer(state: &ViewState, width: f32) -> Element {
    use crate::sidebar_chrome::{ACCOUNT_FOOTER_HEIGHT, GAP, INSET, TOOL_SIZE};
    let account_width = (width
        - INSET * 2.0
        - if state.show_settings {
            0.0
        } else {
            TOOL_SIZE + GAP
        })
    .max(0.0);
    let account = crate::github_account_menu_ui::build(state, account_width, width);
    let settings = UiIconButton::new(
        "toggle_settings",
        Icon::Settings,
        theme::action_palette(theme::SIDEBAR, theme::FOREGROUND_SUBTLE),
    )
    .variant(ButtonVariant::Subtle)
    .size(ControlSize::Dense)
    .tooltip_with(TooltipSpec::new("Configuración").placement(TooltipPlacement::Right))
    .render();
    let mut footer = vec![account.flex(1.0)];
    if !state.show_settings {
        footer.push(settings);
    }
    Element::panel(Element::row(footer).gap(GAP))
        .padding(INSET)
        .size(None, Some(ACCOUNT_FOOTER_HEIGHT))
}

#[cfg(test)]
fn sidebar(state: &ViewState, width: f32) -> Element {
    sidebar_for_height(state, width, 720.0)
}

fn sidebar_for_height(state: &ViewState, width: f32, height: f32) -> Element {
    let header = Element::panel(Element::row(vec![
        Element::spacer().size(Some(2.0), None),
        Element::row(vec![
            UiIconButton::new(
                "toggle_sidebar",
                Icon::PanelLeftClose,
                theme::sidebar_palette(),
            )
            .variant(ButtonVariant::Subtle)
            .size(ControlSize::Dense)
            .tooltip_with(
                TooltipSpec::new("Ocultar barra lateral")
                    .shortcut(
                        crate::keymap::KeymapProfile::default()
                            .shortcut(crate::keymap::ShortcutAction::ToggleSidebar),
                    )
                    .placement(TooltipPlacement::Below),
            )
            .render(),
            // TruncatedText vertically centers its line inside the 24 px
            // header row; Text paints at the top and sat above the toggle.
            Element::truncated_text("OpenIDE")
                .foreground(theme::FOREGROUND)
                .font_family(FontFamily::Sans)
                .font_size(13.0),
            Element::spacer().flex(1.0),
            crate::notifications_ui::bell(&state.notifications, theme::SIDEBAR, SIDEBAR_TOOL_SIZE, false),
        ])
        .gap(4.0)
        .flex(1.0),
        Element::spacer().size(Some(2.0), None),
    ]))
    .padding(4.0)
    .size(None, Some(crate::dock_tab_ui::HEADER_HEIGHT));
    let primary_nav = Element::column(vec![
        sidebar_nav_action(
            "new_session_sidebar",
            Icon::Add,
            "Nueva tarea",
            state.keymap_profile.shortcut(crate::keymap::ShortcutAction::NewTask),
            width - 12.0,
            false,
        ),
        sidebar_nav_action(
            "open_command_center",
            Icon::Search,
            "Buscar",
            state.keymap_profile.shortcut(crate::keymap::ShortcutAction::CommandPalette),
            width - 12.0,
            false,
        ),
        sidebar_nav_action(
            "open_automations",
            Icon::Clock,
            "Automatizaciones",
            "",
            width - 12.0,
            state.main_page == MainPage::Automations,
        ),
        sidebar_nav_action(
            "open_marketplace",
            Icon::Grid,
            "Marketplace",
            "",
            width - 12.0,
            state.main_page == MainPage::Marketplace,
        ),
    ])
    .gap(2.0)
    .padding(6.0);
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
    let sort_trigger = UiIconButton::new(
        "session_sort_toggle",
        Icon::Filter,
        theme::action_palette(theme::SIDEBAR, theme::FOREGROUND_SUBTLE),
    )
    .variant(ButtonVariant::Subtle)
    .size(ControlSize::Dense)
    .active(
        state.session_sort_menu_open || state.session_organization == SessionOrganization::Recent,
    )
    .tooltip_with(
        TooltipSpec::new("Filtrar y ordenar tareas")
            .placement(TooltipPlacement::Below)
            .suppressed(state.session_sort_menu_open),
    )
    .render();
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
        UiIconButton::new(
            "session_group_new",
            Icon::Hash,
            theme::action_palette(theme::SIDEBAR, theme::FOREGROUND_SUBTLE),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Dense)
        .active(state.group_create_open)
        .tooltip_with(TooltipSpec::new("Nuevo grupo").placement(TooltipPlacement::Below))
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
        .size(ControlSize::Dense);
        if project_paths.is_empty() || state.session_organization == SessionOrganization::Recent {
            expand = expand.disabled();
        }
        Tooltip::new(
            expand
                .render()
                .size(Some(SIDEBAR_TOOL_SIZE), Some(SIDEBAR_TOOL_SIZE)),
            "session_expand_all",
            if all_projects_collapsed {
                "Expandir proyectos"
            } else {
                "Contraer proyectos"
            },
            theme::component_palette(),
        )
        .placement(TooltipPlacement::Below)
        .render()
    };
    let archive_control = UiIconButton::new(
        "toggle_archived_view",
        if state.show_archived {
            Icon::Close
        } else {
            Icon::Archive
        },
        theme::action_palette(theme::SIDEBAR, theme::FOREGROUND_SUBTLE),
    )
    .variant(ButtonVariant::Subtle)
    .size(ControlSize::Dense)
    .active(state.show_archived)
    .tooltip_with(
        TooltipSpec::new(if state.show_archived {
            "Cerrar archivo"
        } else {
            "Ver archivo"
        })
        .placement(TooltipPlacement::Below),
    )
    .render();
    let align_tool = |control: Element| {
        control
            .centered_y()
            .size(Some(SIDEBAR_TOOL_SIZE), Some(SIDEBAR_ROW_HEIGHT))
    };
    let context_action = align_tool(context_action);
    let sort_control = align_tool(sort_control);
    let archive_control = align_tool(archive_control);
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
                UiIconButton::new(
                    "session_group_create_commit",
                    Icon::Check,
                    theme::sidebar_palette(),
                )
                .variant(ButtonVariant::Subtle)
                .size(ControlSize::Compact)
                .active(true)
                .tooltip_with(TooltipSpec::new("Crear grupo").placement(TooltipPlacement::Below))
                .render(),
                UiIconButton::new(
                    "session_group_create_cancel",
                    Icon::Close,
                    theme::action_palette(theme::SIDEBAR, theme::FOREGROUND_SUBTLE),
                )
                .variant(ButtonVariant::Subtle)
                .size(ControlSize::Compact)
                .tooltip_with(TooltipSpec::new("Cancelar grupo").placement(TooltipPlacement::Below))
                .render(),
            ])
            .gap(3.0)
            .size(None, Some(28.0)),
        );
    }
    let session_toolbar = Element::column(toolbar).gap(4.0).padding(6.0);
    let compact_usage = height < 440.0;
    let footer = if compact_usage {
        let usage_slot = crate::sidebar_chrome::ACCOUNT_FOOTER_HEIGHT;
        Element::row(vec![
            sidebar_account_footer(state, (width - usage_slot - 4.0).max(0.0)).flex(1.0),
            Element::panel(crate::provider_usage_ui::sidebar_compact(state))
                .padding(crate::sidebar_chrome::INSET)
                .size(Some(usage_slot), Some(usage_slot)),
        ])
        .gap(4.0)
        .size(None, Some(crate::sidebar_chrome::ACCOUNT_FOOTER_HEIGHT))
    } else {
        sidebar_account_footer(state, width)
    };
    let mut items = vec![
        header,
        Separator::horizontal(theme::component_palette()).render(),
        primary_nav,
        session_toolbar,
        sessions_virtual_list(state, width),
        Separator::horizontal(theme::component_palette()).render(),
    ];
    if !compact_usage {
        items.push(crate::provider_usage_ui::sidebar_card(state, width));
    }
    items.push(footer);
    Element::column(items)
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

fn file_tree_content(state: &ViewState, width: f32) -> Element {
    crate::files_panel_ui::build(state, width)
}

fn tool_status(
    output: &Option<String>,
    is_error: bool,
    activity: crate::transcript::ActivityStatus,
) -> &'static str {
    if output.is_none() {
        match activity {
            crate::transcript::ActivityStatus::Running => "En curso",
            crate::transcript::ActivityStatus::WaitingPermission => "Esperando permiso",
            crate::transcript::ActivityStatus::PermissionInterrupted => {
                "Permiso pendiente en turno interrumpido"
            }
            crate::transcript::ActivityStatus::Failed => "Falló sin resultado",
            crate::transcript::ActivityStatus::Cancelled => "Cancelada",
            _ => "Resultado incierto",
        }
    } else if is_error {
        "Error"
    } else {
        "Completada"
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
        TranscriptItem::Reasoning { .. }
        | TranscriptItem::ToolPreview { .. }
        | TranscriptItem::ModelUsage { .. } => {
            crate::session_surface::model_stream_item(item, item_index, state, 780.0)
        }
        TranscriptItem::PlanSnapshot { .. } => {
            crate::session_surface::plan_snapshot_item(item, item_index, state)
        }
        TranscriptItem::WebSearch { .. } => crate::search_activity_ui::hosted(item, state),
        TranscriptItem::Hook(hook) => crate::session_surface::hook_activity_card(hook),
        TranscriptItem::Activity(activity) => crate::session_surface::activity_marker(activity),
        TranscriptItem::WebCitation { title, url } => {
            theme::interactive_surface(Element::clickable(
                format!("web_citation_open:{item_index}"),
                Element::column(vec![
                    eyebrow("FUENTE WEB").foreground(theme::BRAND),
                    label(shorten(title, 100), theme::FOREGROUND),
                    mono(shorten(url, 140), theme::FOREGROUND_SUBTLE).font_size(11.0),
                ])
                .gap(5.0)
                .padding(10.0),
            ))
            .background(theme::PANEL)
            .border(theme::BORDER, 1.0)
        }
        TranscriptItem::Tool { id, .. } => crate::session_surface::tool_line_with_output(
            item,
            state,
            !side_visible,
            crate::session_surface::tool_partial_output(state, item_index, id),
            crate::session_surface::tool_media(state, item_index, id),
        ),
        TranscriptItem::ToolOutput { .. } | TranscriptItem::ToolMedia { .. } => Element::spacer(),
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
                    crate::session_surface::conversation_notice_message(message, *is_error),
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
        if matches!(
            item,
            TranscriptItem::ToolOutput { .. } | TranscriptItem::ToolMedia { .. }
        ) {
            continue;
        }
        items.push(transcript_item(item, index, state, side_visible));
    }
    items.extend(crate::session_surface::host_activity_markers(state));
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
    .bottom_fade(
        theme::BACKGROUND,
        crate::session_surface::TRANSCRIPT_BOTTOM_FADE_HEIGHT,
    )
}

fn composer(state: &ViewState, center_width: f32) -> Element {
    let card_width = (center_width - 32.0).clamp(0.0, 1120.0);
    let card = build_composer(&ComposerProps::from_state(
        state,
        card_width,
        false,
        None,
        "Ask me anything",
        Some(&state.status),
    ));
    card.centered_x().padding(16.0)
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
    if sidebar_hidden {
        actions.push(crate::notifications_ui::bell(
            &state.notifications,
            theme::PANEL,
            SIDEBAR_TOOL_SIZE,
            false,
        ));
    }
    if width >= 300.0 {
        actions.push(crate::environment_ui::header_control(state));
    }
    if available && !visible {
        actions.push(
            Tooltip::new(
                icon_button(
                    "toggle_side_pane",
                    Icon::PanelRightOpen,
                    "Panel",
                    theme::HOVER,
                    theme::FOREGROUND,
                )
                .size(Some(74.0), Some(32.0)),
                "toggle_side_pane",
                "Mostrar panel derecho: archivos, cambios, terminal y navegador",
                theme::component_palette(),
            )
            .placement(TooltipPlacement::Below)
            .render(),
        );
    }
    actions.push(
        Tooltip::new(
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
            "toggle_settings",
            if state.show_settings {
                "Volver al chat"
            } else {
                "Abrir configuración de OpenIDE"
            },
            theme::component_palette(),
        )
        .placement(TooltipPlacement::Below)
        .render(),
    );
    Element::column(vec![
        Element::panel(Element::row(actions).gap(6.0))
            .padding(8.0)
            .background(theme::PANEL)
            .size(None, Some(HEADER - 1.0)),
        Separator::horizontal(theme::component_palette()).render(),
    ])
    .size(None, Some(HEADER))
}

/// The same action is used by every center surface. A collapsed sidebar
/// never inserts a separate row above the center dock.
pub(super) fn sidebar_reopen_control(background: Color) -> Element {
    UiIconButton::new(
        "toggle_sidebar_reopen",
        Icon::PanelLeftOpen,
        theme::action_palette(background, theme::FOREGROUND),
    )
    .variant(ButtonVariant::Subtle)
    .size(ControlSize::Dense)
    .tooltip_with(
        TooltipSpec::new("Mostrar barra lateral")
            .shortcut(
                crate::keymap::KeymapProfile::default()
                    .shortcut(crate::keymap::ShortcutAction::ToggleSidebar),
            )
            .placement(TooltipPlacement::Below),
    )
    .render()
}

#[cfg(test)]
fn activity_content(state: &ViewState) -> Element {
    dock_activity::content(state, state.right_dock_width.unwrap_or(400.0))
}

#[cfg(test)]
fn compact_review_line(line: &ReviewLine, max_chars: usize) -> String {
    if line.kind == ReviewLineKind::Hunk {
        return shorten(&line.text, max_chars);
    }
    let old = line.old_number.map_or_else(String::new, |n| n.to_string());
    let new = line.new_number.map_or_else(String::new, |n| n.to_string());
    let code = line.text.replace('\t', "    ");
    shorten(&format!("{old:>4} {new:>4} {code}"), max_chars)
}

fn code_review_content(state: &ViewState, width: f32) -> Element {
    git_panel::content(state, width)
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
        crate::code_editor_ui::path_breadcrumb(
            "file_preview_path",
            &preview.path,
            false,
            surface_width,
        ),
        Separator::horizontal(theme::component_palette()).render(),
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
                        Separator::vertical(theme::component_palette()).render(),
                        Element::formatted_text(spans).size(Some(code_width), None),
                    ])
                    .gap(6.0)
                    .padding(3.0)
                    .size(Some(surface_width - 12.0), None),
                );
            }
            if code_rows.is_empty() {
                code_rows.push(
                    UiText::new("Archivo vacío", theme::component_palette())
                        .role(TextRole::Small)
                        .muted()
                        .render()
                        .padding(16.0),
                );
            }
            if page.truncated || shortened_lines > 0 {
                let notice = match shortened_lines {
                    0 => "Vista acotada por tamaño de archivo".to_owned(),
                    1 => "Vista acotada · 1 línea larga abreviada".to_owned(),
                    count => format!("Vista acotada · {count} líneas largas abreviadas"),
                };
                rows.push(
                    UiText::new(notice, theme::component_palette())
                        .role(TextRole::Micro)
                        .muted()
                        .render(),
                );
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
                    UiIconButton::new(
                        "file_preview_previous",
                        Icon::ArrowLeft,
                        theme::action_palette(theme::BACKGROUND, theme::FOREGROUND),
                    )
                    .variant(ButtonVariant::Subtle)
                    .size(ControlSize::Compact)
                    .tooltip_with(
                        TooltipSpec::new("Página anterior").placement(TooltipPlacement::Above),
                    )
                    .render()
                } else {
                    Element::spacer().size(Some(28.0), Some(28.0))
                };
                let next = if page.has_more {
                    UiIconButton::new(
                        "file_preview_next",
                        Icon::ArrowRight,
                        theme::action_palette(theme::BACKGROUND, theme::FOREGROUND),
                    )
                    .variant(ButtonVariant::Subtle)
                    .size(ControlSize::Compact)
                    .tooltip_with(
                        TooltipSpec::new("Página siguiente").placement(TooltipPlacement::Above),
                    )
                    .render()
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

fn dock_open_actions(width: f32) -> Element {
    let action_width = width.max(150.0);
    Element::column(vec![
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
        icon_button(
            "dock_open:browser",
            Icon::Globe,
            "Navegador",
            theme::CARD,
            theme::FOREGROUND,
        )
        .size(Some(action_width), Some(38.0)),
        icon_button(
            "dock_open:changes",
            Icon::Code,
            "Cambios del chat",
            theme::CARD,
            theme::FOREGROUND,
        )
        .size(Some(action_width), Some(38.0)),
    ])
    .gap(6.0)
}

fn dock_add_menu(state: &ViewState) -> Element {
    let entries = [
        (
            "terminal",
            "Nueva terminal",
            Icon::Terminal,
            SideTab::Terminal,
        ),
        ("browser", "Navegador", Icon::Globe, SideTab::Browser),
        (
            "changes",
            "Cambios del chat",
            Icon::Code,
            SideTab::SessionChanges,
        ),
        ("files", "Archivos", Icon::Folder, SideTab::Files),
        ("activity", "Actividad", Icon::History, SideTab::Activity),
        ("review", "Cambios de Git", Icon::Code, SideTab::CodeReview),
    ];
    let mut menu: Vec<DropdownMenuEntry> = entries
        .into_iter()
        .map(|(id, title, icon, kind)| {
            let already_open = state.dock_tabs.iter().any(|tab| tab.kind == kind);
            let mut item = DropdownMenuItem::new(format!("dock_open:{id}"), title).icon(icon);
            if already_open && kind != SideTab::Terminal {
                item = item.annotation("Abierta");
            }
            item.into()
        })
        .collect();
    if crate::ide_workspace::active(state) {
        menu.push(
            DropdownMenuItem::new("dock_open:debugger", "Depurador")
                .icon(Icon::Code)
                .into(),
        );
    }
    let anchor = UiIconButton::new(
        "dock_add_tab",
        Icon::Add,
        theme::action_palette(Color::TRANSPARENT, theme::FOREGROUND),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Dense)
    .active(state.show_dock_add_menu)
    .tooltip_with(
        TooltipSpec::new("Abrir pestaña")
            .placement(TooltipPlacement::Below)
            .suppressed(state.show_dock_add_menu),
    )
    .render();
    DropdownMenu::new(
        anchor,
        menu,
        state.show_dock_add_menu,
        "dock_add_dismiss",
        theme::menu_palette(),
    )
    .placement(PopoverPlacement::BottomEnd)
    .context_menu_style()
    .width(224.0)
    .render()
}

fn dock_empty_content(width: f32) -> Element {
    let content_width = (width - 48.0).clamp(150.0, 320.0);
    let content = Element::column(vec![
        label("Abrir pestaña", theme::FOREGROUND).font_size(19.0),
        UiText::new("Elige una vista para este panel.", theme::component_palette())
            .muted()
            .render(),
        Element::spacer().size(None, Some(8.0)),
        dock_open_actions(content_width),
    ])
    .gap(8.0)
    .size(Some(content_width), None);
    content.centered().padding(20.0).flex(1.0)
}

fn terminal_content(state: &ViewState, width: f32) -> Element {
    crate::terminal_ui::build(state, width)
}

#[cfg(test)]
fn side_pane(state: &ViewState, width: f32) -> Element {
    side_pane_sized(state, width, 600.0)
}

fn side_pane_sized(state: &ViewState, width: f32, height: f32) -> Element {
    let Some(preview) = state
        .markdown_preview
        .as_ref()
        .filter(|_| !state.show_dock_overview)
    else {
        return side_pane_primary_sized(state, width, height);
    };
    if dock_chrome::visible_tabs(state).is_empty() {
        return crate::document_preview_ui::secondary(preview, width, height);
    }
    let palette = theme::component_palette();
    if width >= 640.0 {
        let half = (width - 1.0) / 2.0;
        Element::row(vec![
            side_pane_primary_sized(state, half, height),
            Separator::vertical(palette).length(height).render(),
            crate::document_preview_ui::secondary(preview, half, height),
        ])
        .size(Some(width), Some(height))
        .clip_children()
    } else {
        // Small windows keep both groups usable instead of squeezing the source
        // and tab close buttons into columns narrower than their controls.
        let half = (height - 1.0).max(0.0) / 2.0;
        Element::column(vec![
            side_pane_primary_sized(state, width, half),
            Separator::horizontal(palette).length(width).render(),
            crate::document_preview_ui::secondary(preview, width, half),
        ])
        .size(Some(width), Some(height))
        .clip_children()
    }
}

fn side_pane_primary_sized(state: &ViewState, width: f32, height: f32) -> Element {
    if crate::ide_workspace::active(state)
        && !state.show_dock_overview
        && dock_chrome::visible_tabs(state).is_empty()
    {
        return crate::ide_editor_empty::build(state.keymap_profile, width, height)
            .size(Some(width), Some(height));
    }
    let header = dock_chrome::header(state, width);
    // A single document status row belongs to this editor group. It does not
    // consume the scroll viewport or appear under a preview-only resource.
    let document_status = !crate::ide_workspace::active(state)
        && !state.show_dock_overview
        && crate::ide_status_ui::document_scope(state).is_some();
    let status_height = if document_status {
        crate::ide_workspace::STATUSBAR_HEIGHT
    } else {
        0.0
    };
    let body = if crate::ide_workspace::active(state)
        && !state.show_dock_overview
        && (state.dock_tabs.is_empty()
            || matches!(state.side_tab, SideTab::Files | SideTab::Context))
    {
        crate::ide_editor_empty::build(state.keymap_profile, width, (height - crate::dock_tab_ui::HEADER_HEIGHT).max(0.0))
    } else if state.dock_tabs.is_empty() && !state.show_dock_overview {
        dock_empty_content(width)
    } else if state.show_dock_overview {
        let query = state.dock_overview_query.trim().to_lowercase();
        let matches = |title: &str, kind: SideTab| {
            let kind_name = match kind {
                SideTab::Context => "entorno",
                SideTab::Files => "archivos",
                SideTab::Activity => "actividad",
                SideTab::CodeReview => "repositorio git",
                SideTab::SessionChanges => "cambios chat conversación",
                SideTab::Browser => "navegador web",
                SideTab::Terminal => "terminal",
                SideTab::FilePreview => "vista de archivo",
                SideTab::CodeSnapshot => "vista previa capturada",
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
                UiText::new(
                    if state.dock_tabs.is_empty() {
                        "No hay pestañas abiertas"
                    } else {
                        "Sin coincidencias abiertas"
                    },
                    theme::component_palette(),
                )
                .role(TextRole::Small)
                .muted()
                .render()
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
                        UiIconButton::new(
                            format!("dock_move_up:{}", tab.id),
                            Icon::ArrowUp,
                            theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
                        )
                        .variant(ButtonVariant::Subtle)
                        .size(ControlSize::Dense)
                        .tooltip_with(TooltipSpec::new("Subir").placement(TooltipPlacement::Above))
                        .render()
                    },
                    if last_id == Some(tab.id) {
                        Element::icon(Icon::ArrowDown)
                            .foreground(theme::BORDER)
                            .size(Some(24.0), Some(24.0))
                    } else {
                        UiIconButton::new(
                            format!("dock_move_down:{}", tab.id),
                            Icon::ArrowDown,
                            theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
                        )
                        .variant(ButtonVariant::Subtle)
                        .size(ControlSize::Dense)
                        .tooltip_with(TooltipSpec::new("Bajar").placement(TooltipPlacement::Above))
                        .render()
                    },
                    if tab.closable {
                        UiIconButton::new(
                            format!("dock_close:{}", tab.id),
                            Icon::Close,
                            theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
                        )
                        .variant(ButtonVariant::Subtle)
                        .size(ControlSize::Compact)
                        .tooltip_with(
                            TooltipSpec::new("Cerrar pestaña").placement(TooltipPlacement::Above),
                        )
                        .render()
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
                UiText::new(
                    if state.dock_recently_closed.is_empty() {
                        "No hay pestañas recientes"
                    } else {
                        "Sin coincidencias recientes"
                    },
                    theme::component_palette(),
                )
                .role(TextRole::Small)
                .muted()
                .render()
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
            SideTab::Context => dock_empty_content(width),
            SideTab::Files => file_tree_content(state, width),
            SideTab::Activity => dock_activity::content(state, width),
            SideTab::CodeReview => code_review_content(state, width),
            SideTab::SessionChanges => crate::session_changes_ui::build(state, width),
            SideTab::Browser => crate::browser_ui::build(state, width),
            SideTab::Terminal => terminal_content(state, width),
            SideTab::CodeSnapshot => state.code_snapshot.as_ref().map_or_else(
                || {
                    label(
                        "Selecciona una vista previa del chat",
                        theme::FOREGROUND_SUBTLE,
                    )
                    .padding(12.0)
                },
                |preview| crate::code_snapshot_ui::build(preview, width),
            ),
            SideTab::FilePreview => {
                if let Some(preview) = crate::document_preview_ui::build(
                    state,
                    width,
                    (height - RIGHT_HEADER_HEIGHT - status_height).max(0.0),
                ) {
                    preview
                } else if let Some(editor) = &state.code_editor {
                    if crate::ide_workspace::active(state) {
                        crate::code_editor_ui::build_ide(editor, &state.editor_workbench, width, state.keymap_profile, state.vim_mode)
                    } else {
                        crate::code_editor_ui::build_with_debug(
                            editor,
                            &state.editor_workbench,
                            width,
                            false,
                            state.keymap_profile,
                            state.vim_mode,
                        )
                    }
                } else if let Some(preview) = &state.file_preview {
                    file_preview_content(preview, width)
                } else {
                    UiText::new("Selecciona un archivo", theme::component_palette())
                        .role(TextRole::Body)
                        .muted()
                        .render()
                        .padding(12.0)
                }
            }
        }
    };
    let body = if !state.show_dock_overview
        && (state.dock_tabs.is_empty()
            || matches!(
                state.side_tab,
                SideTab::Files
                    | SideTab::Browser
                    | SideTab::Terminal
                    | SideTab::CodeReview
                    | SideTab::SessionChanges
                    | SideTab::CodeSnapshot
            )
            || (state.side_tab == SideTab::FilePreview
                && (state.code_editor.is_some() || state.document_preview.is_some())))
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
                SideTab::SessionChanges => "side_pane_session_changes",
                SideTab::Browser => "side_pane_browser",
                SideTab::FilePreview => "side_pane_preview",
                SideTab::CodeSnapshot => "side_pane_code_snapshot",
                SideTab::Files | SideTab::Terminal => unreachable!(),
            }
        };
        ScrollArea::new(scroll_id, body)
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .flex(1.0)
    };
    // Each scrolling viewport owns its fade. Applying it to the whole pane
    // would paint over resource headers, breadcrumbs and refresh controls.
    let mut rows = vec![header, body];
    if document_status {
        rows.push(crate::ide_status_ui::document_footer(state, width));
    }
    Element::column(rows).size(Some(width), Some(height))
}

/// One outer frame for chat, editor and bottom terminal. The shared finishing
/// pass masks opaque child corners and draws the border before deferred menus.
fn workspace_frame(content: Element) -> Element {
    content
        .background(theme::BACKGROUND)
        .border(theme::BORDER, 1.0)
        .radius(WORKSPACE_RADIUS)
        .corner_mask(theme::WINDOW)
        .clip_children()
        .flex(1.0)
}

pub fn build_view(state: &ViewState, viewport_width: f32) -> Element {
    build_view_sized(state, viewport_width, 720.0)
}

pub(crate) fn terminal_panel_height(state: &ViewState, viewport_height: f32) -> f32 {
    if cli_task_in_center(state) {
        return 0.0;
    }
    if !state.show_settings && state.main_page == MainPage::Workspace {
        if let Some(height) = state.bottom_presentation_height {
            let chrome = if crate::ide_workspace::active(state) {
                crate::ide_workspace::TITLEBAR_HEIGHT + crate::ide_workspace::STATUSBAR_HEIGHT
            } else { 0.0 };
            return height.clamp(0.0, (viewport_height - 2.0 * GAP - chrome).max(0.0));
        }
    }
    nominal_terminal_panel_height(state, viewport_height)
}

/// Prefix of the backend of a task that runs an agent CLI in a terminal.
pub(crate) const CLI_BACKEND: &str = "cli:";

/// The selected task is an agent CLI: its terminal is the whole task.
pub(crate) fn cli_task(state: &ViewState) -> bool {
    state.selected_session.is_some() && state.selected_backend.starts_with(CLI_BACKEND)
}

/// A CLI task shows its terminal in the center, not in the bottom panel.
fn cli_task_in_center(state: &ViewState) -> bool {
    cli_task(state)
        && !state.show_settings
        && state.main_page == MainPage::Workspace
        && !crate::ide_workspace::active(state)
}

pub(crate) fn nominal_terminal_panel_height(state: &ViewState, viewport_height: f32) -> f32 {
    if cli_task_in_center(state) {
        return 0.0;
    }
    // In the IDE a CLI task's terminal lives in the agent panel, so it alone
    // does not open the bottom panel.
    let visible = (state.show_terminal_panel && !cli_task(state)) || state.ide.bottom_open;
    if !visible || state.show_settings || state.main_page != MainPage::Workspace {
        return 0.0;
    }
    let ide = crate::ide_workspace::active(state);
    let chrome = if ide {
        crate::ide_workspace::TITLEBAR_HEIGHT + crate::ide_workspace::STATUSBAR_HEIGHT
    } else {
        0.0
    };
    let available = (viewport_height - 2.0 * GAP - chrome).max(0.0);
    // Reserve space on the same timeline as Environment. A binary open/closed
    // budget makes a maximized terminal jump by 180 px at either end of the
    // animation. No progress means a settled state (also reduced motion).
    // The stored preference remains untouched throughout the transition.
    let environment_progress = if ide {
        0.0
    } else {
        state
            .environment_presentation_progress
            .unwrap_or(if state.environment.open { 1.0 } else { 0.0 })
            .clamp(0.0, 1.0)
    };
    let minimum_workspace = 180.0 + 180.0 * environment_progress;
    let maximum = (available - minimum_workspace).max(0.0);
    if state.terminal_maximized {
        maximum
    } else {
        state
            .terminal_panel_height
            .max(120.0)
            .min(available * 0.65)
            .min(maximum)
    }
}

pub(crate) fn build_view_sized(state: &ViewState, viewport_width: f32, viewport_height: f32) -> Element {
    if crate::ide_workspace::active(state) {
        return workspace_modals(
            state,
            ide_ui::build(state, viewport_width, viewport_height),
            viewport_width,
            viewport_height,
        );
    }
    let layout = dock_layout(state, viewport_width);
    let terminal_height = terminal_panel_height(state, viewport_height);
    let workspace_height = (viewport_height - terminal_height).max(0.0);
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
    let is_home = state.main_page == MainPage::Workspace
        && state.selected_session.is_none()
        && state.transcript.is_empty()
        && state.pending_permissions.is_empty()
        && !state.show_settings;
    let is_session = state.main_page == MainPage::Workspace
        && !state.show_settings
        && state.selected_session.is_some();
    let environment = (state.main_page == MainPage::Workspace && !state.show_settings)
        .then(|| {
            crate::environment_ui::layout(
                state,
                layout.center,
                (workspace_height - 2.0 * GAP).max(0.0),
            )
        })
        .filter(|layout| layout.progress > 0.001);
    let center_width = environment.map_or(layout.center, |environment| environment.chat_width);
    let session_height = (workspace_height
        - 2.0 * GAP
        - environment
            .filter(|environment| environment.stacked)
            .map_or(0.0, |environment| {
                environment.allocated_height
                    + if is_home {
                        48.0 * environment.progress
                    } else {
                        0.0
                    }
            }))
    .max(0.0);
    let compact = center_width < 460.0;
    let center = if !state.show_settings && state.main_page == MainPage::Automations {
        automation_view::build_automations(&state.automations, center_width)
    } else if !state.show_settings && state.main_page == MainPage::Marketplace {
        marketplace_view::marketplace_page_with_navigation(
            &state.marketplace,
            center_width,
            (!left_visible).then(|| sidebar_reopen_control(theme::BACKGROUND)),
        )
    } else if is_home {
        home::build_home_sized(state, center_width, session_height)
    } else if is_session && cli_task_in_center(state) {
        // The task's header, then its terminal; the side panel stays beside it.
        let header_height = crate::dock_tab_ui::HEADER_HEIGHT;
        Element::column(vec![
            crate::session_surface::header(state, center_width, side_visible, side_available)
                .size(Some(center_width), Some(header_height)),
            crate::terminal_dock_ui::build_panes(
                state,
                center_width,
                (session_height - header_height).max(0.0),
            ),
        ])
        .background(theme::BACKGROUND)
        .flex(1.0)
    } else if is_session {
        if environment.is_some() {
            crate::session_surface::build_session_body_sized(
                state,
                center_width,
                session_height,
                side_visible,
                side_available,
            )
        } else {
            crate::session_surface::build_session_surface_sized(
                state,
                center_width,
                session_height,
                side_visible,
                side_available,
            )
        }
    } else {
        let mut center_items = if state.show_settings || environment.is_some() {
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
                    crate::session_surface::jump_to_bottom_control()
                        .centered_x()
                        .padding(8.0)
                        .size(None, Some(52.0)),
                );
            }
        }
        for permission in state.pending_permissions.iter().take(3) {
            center_items.push(
                crate::approval_ui::inline_root(permission, state).padding(8.0),
            );
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
                sidebar_for_height(state, left, viewport_height - 2.0 * GAP)
            }
            .corner_mask(theme::WINDOW)
            .clip_children(),
        );
    }
    let center = if let Some(environment) = environment {
        let panel =
            crate::environment_ui::panel(state, environment.panel_width, environment.panel_height);
        if environment.stacked {
            let environment_panel = Element::column(vec![
                Element::spacer().size(None, Some(if is_home { 54.0 } else { 6.0 })),
                Element::row(vec![
                    Element::spacer().size(Some(8.0), None),
                    panel,
                    Element::spacer().size(Some(8.0), None),
                ]),
            ])
            .size(
                Some(layout.center),
                Some(
                    environment.allocated_height
                        + if is_home {
                            48.0 * environment.progress
                        } else {
                            0.0
                        },
                ),
            )
            .clip_children();
            Element::column(vec![environment_panel, center]).flex(1.0)
        } else {
            let environment_panel = Element::column(vec![
                Element::spacer().size(None, Some(if is_home { 48.0 } else { 8.0 })),
                Element::row(vec![
                    Element::spacer().size(Some(8.0), None),
                    panel,
                    Element::spacer().size(Some(8.0), None),
                ]),
                Element::spacer().flex(1.0),
            ])
            .size(Some(environment.allocated_width), None)
            .clip_children();
            Element::row(vec![center, environment_panel]).flex(1.0)
        }
    } else {
        center
    };
    let center = if environment.is_some() && !is_home && !cli_task_in_center(state) {
        let header = if is_session {
            crate::session_surface::header(state, layout.center, side_visible, side_available)
        } else {
            workspace_header(state, side_available, side_visible, layout.center)
        };
        Element::column(vec![header, center]).flex(1.0)
    } else {
        center
    };
    let center = if is_home {
        let toggle = UiIconButton::new(
            "toggle_side_pane_header",
            Icon::PanelRightOpen,
            theme::action_palette(theme::BACKGROUND, theme::FOREGROUND),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Dense)
        .tooltip_with(
            TooltipSpec::new("Mostrar panel derecho: archivos, cambios, terminal y navegador")
                .placement(TooltipPlacement::Below),
        )
        .render();
        let mut actions = vec![
            window_mode_control(state),
            crate::environment_ui::header_control(state),
        ];
        if !side_visible && side_available {
            actions.push(toggle);
        }
        Element::overlay(
            center,
            Element::column(vec![
                Element::row(vec![
                    Element::spacer().flex(1.0),
                    Element::row(actions).gap(4.0),
                ])
                .padding(4.0)
                .size(None, Some(crate::dock_tab_ui::HEADER_HEIGHT)),
                Element::spacer().flex(1.0),
            ]),
        )
        .flex(1.0)
    } else {
        center
    };
    let center = if !left_visible
        && !state.show_settings
        && (is_home || matches!(state.main_page, MainPage::Automations))
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
            .size(Some(crate::dock_tab_ui::HEADER_HEIGHT), Some(crate::dock_tab_ui::HEADER_HEIGHT))]),
        )
        .flex(1.0)
    } else {
        center
    };
    let mut workspace = vec![center];
    if let Some(right) = layout.right {
        workspace.push(Separator::vertical(theme::component_palette()).render());
        workspace.push(side_pane_sized(state, right, workspace_height).clip_children());
    }
    let top = Element::row(workspace).flex(1.0).clip_children();
    let workspace = if terminal_height > 0.0 {
        let width = layout.center + layout.right.map_or(0.0, |right| right + RIGHT_DIVIDER);
        Element::column(vec![
            top,
            ide_ui::bottom::build(state, width, terminal_height)
                .size(None, Some(terminal_height)),
        ])
    } else {
        Element::column(vec![top])
    };
    docks.push(workspace_frame(workspace));
    let base = Element::row(docks)
        .gap(GAP)
        .padding(GAP)
        .background(theme::WINDOW);
    workspace_modals(state, base, viewport_width, viewport_height)
}

pub(crate) fn window_mode_control(state: &ViewState) -> Element {
    ide_ui::mode_button(state)
}

fn workspace_modals(
    state: &ViewState,
    base: Element,
    viewport_width: f32,
    viewport_height: f32,
) -> Element {
    let base = crate::notifications_ui::toast_layer(state, base);
    let base = confirmation_dialog(state, base, viewport_width);
    let base = crate::github_account_ui::sign_in_alert(state, base, viewport_width);
    let base = settings_ui::provider_connect_modal(state, base, viewport_width);
    let base = settings_ui::mcp_editor_modal(state, base, viewport_width);
    let base = settings_ui::hook_editor_modal(state, base, viewport_width);
    let base = settings_ui::exec_rule_editor_modal(state, base, viewport_width);
    let base = settings_ui::exec_rule_reset_modal(state, base, viewport_width);
    let base = settings_ui::external_approval_modal(state, base, viewport_width);
    let base = settings_ui::native_skill_reset_modal(state, base, viewport_width);
    let base = settings_ui::native_skill_preview_modal(state, base, viewport_width);
    let base = crate::plugins_ui::modal(state, base, viewport_width);
    let base = settings_ui::native_skill_clear_workspace_modal(state, base, viewport_width);
    let base = settings_ui::native_instructions_modal(state, base, viewport_width);
    let base = crate::native_command_output_ui::modal(state, base, viewport_width);
    let base = crate::native_command_control_ui::modal(state, base, viewport_width);
    let base = crate::native_command_history_ui::modal(state, base, viewport_width);
    let base = crate::conversation_source_ui::modal(state, base, viewport_width);
    let base = settings_ui::provider_model_details_modal(state, base, viewport_width);
    let base = crate::composer_permissions_ui::full_access_dialog(state, base, viewport_width);
    let base = crate::approval_ui::modal(state, base, viewport_width, viewport_height);
    let base = crate::code_editor_ui::confirmation(state, base, viewport_width);
    ide_ui::new_file_modal(state, base, viewport_width)
}

pub fn build_window_view(state: &ViewState, width: f32, height: f32) -> Element {
    let base = build_view_sized(state, width, height);
    let Some(gallery) = &state.composer_image_viewer else {
        return base;
    };
    if !gallery.is_current(state) {
        return base;
    }
    let title = if let Some(source) = &gallery.tool_source {
        format!(
            "Imagen de herramienta {} de {}",
            gallery.selected + 1,
            source.images.len()
        )
    } else {
        let Some(file) = gallery.attachments.get(gallery.selected) else {
            return base;
        };
        file.label.clone()
    };
    let mut viewer = openide_ui::ImageViewer::new(
        base,
        true,
        title,
        "composer_image_close",
        theme::component_palette(),
    )
    .viewport(openide_ui::Size::new(width, height))
    .gallery(gallery.selected, gallery.image_count())
    .previous_action("composer_image_previous")
    .next_action("composer_image_next")
    .return_focus("prompt");
    if let Some(image) = &gallery.image {
        viewer = viewer.image(image.clone());
    }
    if let Some(error) = &gallery.error {
        viewer = viewer.error(error);
    }
    viewer.render()
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
                openide_ui::TextField::new("rename_title", &state.rename_draft, "Nombre de la tarea", theme::component_palette())
                    .size(ControlSize::Compact)
                    .render(),
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
                openide_ui::TextField::new(
                    "group_rename_draft",
                    &state.group_rename_draft,
                    "Nombre del grupo",
                    theme::component_palette(),
                )
                .size(ControlSize::Compact)
                .render(),
            )
            .render();
        }
    }
    let dialog = if let Some(id) = state.pending_mcp_delete_id.as_deref() {
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
                "¿Eliminar conversación?".to_owned(),
                format!("«{}»\n\nSe eliminará esta conversación, sus subagentes y su historial. Los archivos del proyecto se conservarán. Esta acción no se puede deshacer.", session.title),
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
pub(crate) mod tests {
    #[test]
    fn provider_usage_remains_reachable_in_the_short_workspace_sidebar() {
        let mut state = empty_state();
        state.show_side_pane = false;
        state.github_account = GithubAccountStatus::Disconnected;
        for (width, height) in [(520.0, 320.0), (900.0, 320.0), (900.0, 720.0)] {
            let mut tree = UiTree::new(build_view_sized(&state, width, height));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, height));
            for action in ["usage_toggle", "toggle_settings", "github_account_connect"] {
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == action)
                    .unwrap_or_else(|| panic!("missing {action} at {width}x{height}"))
                    .rect;
                assert!(
                    rect.y >= 0.0 && rect.y + rect.height <= height + 0.1,
                    "{action} unreachable at {width}x{height}: {rect:?}"
                );
            }
            if height < 440.0 {
                let bounds = |id: &str| frame.regions.iter().find(|r| r.id == id).unwrap().rect;
                let usage = bounds("usage_toggle");
                let settings = bounds("toggle_settings");
                assert_eq!(usage.height, settings.height);
                assert_eq!(usage.y, settings.y, "footer tools share one baseline");
                assert!(settings.x + settings.width < usage.x);
            }
        }
    }
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

    pub(crate) fn empty_state() -> ViewState {
        ViewState {
            ide: crate::ide_workspace::IdeWorkspaceState::default(),
            ide_search: crate::ide_search::IdeSearchState::default(),
            document_preview: None,
            markdown_preview: None,
            ide_status: Default::default(),
            sessions_status: None,
            session_plans: BTreeMap::new(),
            session_recovery: BTreeMap::new(),
            pending_permissions: Vec::new(),
            approval_ui: Default::default(),
            composer_pending: None,
            pending_subagent_permissions: Vec::new(),
            active_subagents: Vec::new(),
            subagent_sessions: Vec::new(),
            expanded_subagent_inputs: None,
            subagent_input_edit: None,
            recoverable_subagent: None,
            pending_tool_verification: None,
            pending_batch_discard: None,
            tool_verification_output: String::new(),
            tool_verification_is_error: false,
            workspace: "C:/project".into(),
            workspace_draft: "C:/project".into(),
            project_branch: None,
            composer_branches: Default::default(),
            prompt: String::new(),
            composer_attachments: Vec::new(),
            composer_attachment_visuals: Default::default(),
            composer_attachment_loading: false,
            composer_image_viewer: None,
            composer_add_menu_open: false,
            composer_listening: false,
            composer_expanded: false,
            composer_goal: None,
            composer_goal_editor_open: false,
            composer_goal_draft: String::new(),
            native_goal: Default::default(),
            native_instructions: Default::default(),
            native_context: Default::default(),
            composer_permissions_open: false,
            composer_full_access_confirm: None,
            composer_suggestions: Default::default(),
            composer_project_menu_open: false,
            environment: Default::default(),
            project_actions: Default::default(),
            composer_project_options: Vec::new(),
            endpoint: String::new(),
            model: String::new(),
            native_provider_id: "custom".into(),
            native_output_limit_draft: String::new(),
            native_output_limit_field: String::new(),
            native_output_limit_responses: false,
            native_output_limit_messages: false,
            native_output_limit_load_error: None,
            hosted_web_search_mode: None,
            catalog: None,
            catalog_status: String::new(),
            catalog_refreshing: false,
            provider_connections: BTreeMap::new(),
            provider_usage: BTreeMap::new(),
            provider_usage_open: false,
            provider_usage_refreshing: false,
            provider_connect: None,
            catalog_browse_provider: "openai".into(),
            catalog_provider_query: String::new(),
            catalog_model_query: String::new(),
            catalog_models_filter: CatalogModelsFilter::All,
            catalog_settings_tab: CatalogSettingsTab::Models,
            catalog_expanded_model: None,
            catalog_provider_menu_open: false,
            native_provider_definitions: crate::provider_settings::connection_views_with_registry(
                &crate::native_provider::ProviderRegistry::builtin(),
                &crate::native_provider::NativeProviderSettings::default(),
                |_| false,
            ),
            native_provider_registry_error: None,
            backend_options: vec![("native".into(), "Nativo".into())],
            selected_backend: "native".into(),
            backend_menu_open: false,
            composer_model: "gpt-4.1".into(),
            composer_model_options: vec![("gpt-4.1".into(), "gpt-4.1".into())],
            composer_routes: Vec::new(),
            composer_model_metadata: Default::default(),
            composer_provider_status: BTreeMap::new(),
            composer_model_menu_open: false,
            composer_model_list_open: false,
            composer_model_query: String::new(),
            composer_collapsed_model_providers: Default::default(),
            composer_search_collapsed_model_providers: Default::default(),
            composer_advanced_open: false,
            composer_effort: "default".into(),
            composer_effort_options: Vec::new(),
            composer_effort_menu_open: false,
            composer_effort_preview: None,
            composer_effort_hovered: false,
            composer_context_percent: None,
            composer_context_open: false,
            composer_context_details_open: false,
            composer_error: None,
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
            native_full_access: false,
            native_command_isolation: if cfg!(windows) {
                CommandIsolation::WorkspaceWriteOffline
            } else {
                CommandIsolation::HostUnrestricted
            },
            external_agents: Default::default(),
            external_approval: None,
            compat_hooks: Vec::new(),
            plugin_skills: Vec::new(),
            agent_clis: Vec::new(),
            turn_check_command: String::new(),
            turn_check_failure: None,
            review_model: Default::default(),
            compat_hooks_error: None,
            session_approval_grants: Vec::new(),
            native_commands: Vec::new(),
            native_commands_error: None,
            native_command_control: Default::default(),
            native_command_output: Default::default(),
            native_command_history: Default::default(),
            pending_native_command_termination: None,
            native_command_terminating: None,
            native_hooks: Default::default(),
            native_exec_rules: Default::default(),
            native_skills: Default::default(),
            plugins: Default::default(),
            keymap_profile: crate::keymap::KeymapProfile::default(),
            vim_mode: false,
            settings_keymap_menu_open: false,
            settings_reduced_motion: false,
            settings_reopen_last_workspace: false,
            settings_terminal_shell: TerminalShell::System,
            settings_terminal_shell_draft: String::new(),
            settings_terminal_shell_menu_open: false,
            settings_choice_menu_open: None,
            settings_terminal_shell_editing: false,
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
            mcp_inspection: Default::default(),
            conversation_activity: Default::default(),
            conversation_details: Default::default(),
            stream_presentation: Default::default(),
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
            settings_nav_query: String::new(),
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
            bottom_presentation_height: None,
            environment_presentation_progress: None,
            side_tab: SideTab::Context,
            activity_page: 0,
            review_page: 0,
            dock_tabs: vec![
                DockTabView {
                    full_path: None,
                    id: 1,
                    title: "Contexto".into(),
                    active: true,
                    kind: SideTab::Context,
                    closable: false,
                },
                DockTabView {
                    full_path: None,
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
            notifications: Default::default(),
            show_dock_tabs_menu: false,
            file_preview: None,
            code_editor: None,
            editor_workbench: Default::default(),
            debug: Default::default(),
            debug_open: false,
            code_snapshot: None,
            code_editor_quit_confirm: false,
            code_editor_pending_saves: false,
            chat_diffs: Default::default(),
            code_editor_reload_confirm: None,
            git_review: None,
            git_action_pending: false,
            git_action_error: None,
            session_changes: Default::default(),
            browser: Default::default(),
            review_detail: None,
            terminal_tabs: Vec::new(),
            terminal_menu_open: false,
            terminal_controls: Default::default(),
            terminal_maximized: false,
            show_terminal_panel: false,
            terminal_panel_height: 240.0,
            terminal_snapshot: None,
            terminal_input: String::new(),
            terminal_error: None,
            selected_tool_id: None,
            show_jump_to_bottom: false,
            github_account: GithubAccountStatus::Disconnected,
            github_account_menu_open: false,
            github_account_refreshing: false,
            github_account_notice: None,
            github_sign_in: None,
            github_contributions: GithubContributionsStatus::Idle,
        }
    }

    /// Export the same tree/paint commands used by the host. This does not
    /// replace OS rendering acceptance, and never reads account data.
    #[test]
    #[ignore = "export native chat and compact selectors for visual inspection"]
    fn export_streaming_and_compact_selectors_visual_review() {
        use crate::composer_selection::{
            CredentialAvailability, ExecutionSupport, ModelRouteOption, RouteSource,
        };
        use crate::transcript::{ActivityStatus, ModelStreamStatus, TranscriptItem};
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/streaming-selectors-visual-qa");
        std::fs::create_dir_all(&directory).unwrap();
        let mut state = empty_state();
        state.workspace = "/test/project".into();
        state.project_branch = Some("main".into());
        state.show_side_pane = false;
        state.native_provider_id = "codex".into();
        state.model = "fixture-model".into();
        state.composer_model = state.model.clone();
        state.composer_routes = (0..12)
            .map(|index| ModelRouteOption {
                route_id: format!("native:codex:fixture-model-{index}"),
                backend: "native".into(),
                provider_id: "codex".into(),
                provider_name: "Codex".into(),
                model_id: format!("fixture-model-{index}"),
                model_name: format!("Modelo de prueba {index}"),
                credential: CredentialAvailability::EnvPresentUnverified,
                execution: ExecutionSupport::Verified,
                source: RouteSource::AdapterReview,
                catalog_present: true,
                provider_listed: Some(true),
                effort_options: Vec::new(),
            })
            .collect();
        state.composer_model_options = state
            .composer_routes
            .iter()
            .map(|route| (route.model_id.clone(), route.model_name.clone()))
            .collect();
        state.composer_model = state.composer_routes[0].model_id.clone();
        let export = |state: &ViewState, name: &str, width: f32, height: f32| {
            let mut tree = UiTree::new(build_view(state, width));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, height));
            let path = directory.join(format!("{name}-{width:.0}.svg"));
            settings_ui::visual_qa::write_frame_svg(&frame, &path, width, height).unwrap();
            let svg = std::fs::read_to_string(&path).unwrap();
            let mut options = resvg::usvg::Options::default();
            options.fontdb_mut().load_system_fonts();
            let svg_tree = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
            resvg::render(
                &svg_tree,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
        };
        export(&state, "home", 1400.0, 900.0);
        state.prompt = "Primera línea\nSegunda línea\n".into();
        export(&state, "composer-multiline", 1400.0, 900.0);
        state.composer_permissions_open = true;
        export(&state, "permissions", 1400.0, 900.0);
        state.composer_permissions_open = false;
        state.composer_full_access_confirm =
            Some(crate::composer_permissions_ui::FullAccessConfirmation::new(
                &state,
                crate::agent_tool_settings::AgentToolSettings::default(),
            ));
        export(&state, "full-access-confirmation", 1400.0, 900.0);
        export(&state, "full-access-confirmation", 360.0, 720.0);
        state.composer_full_access_confirm = None;
        state.composer_model_menu_open = true;
        state.composer_model_list_open = true;
        export(&state, "models", 1400.0, 900.0);
        state.composer_model_menu_open = false;
        state.composer_model_list_open = false;
        state.prompt.clear();
        state.selected_session = Some("visual-fixture".into());
        state.selected_session_title = Some("Revisar el proyecto".into());
        state.transcript = vec![
            TranscriptItem::User {
                content: "Explica el proyecto sin modificar archivos.".into(),
                images: Vec::new(),
            },
            TranscriptItem::Assistant {
                content: "Voy a revisar la estructura del proyecto.".into(),
                streaming: false,
            },
            TranscriptItem::Reasoning {
                id: "reasoning-fixture".into(),
                step: 1,
                content: "Resumen público: comprobar los módulos principales.".into(),
                status: ModelStreamStatus::Committed,
                truncated: false,
            },
            TranscriptItem::Tool {
                id: "read-fixture".into(),
                name: "read_file".into(),
                arguments: json!({"path":"README.md"}),
                output: Some("# Proyecto\nAplicación Rust".into()),
                is_error: false,
                activity: ActivityStatus::Completed,
            },
            TranscriptItem::Assistant {
                content: "## Estructura\n\nLa aplicación usa **Rust** y componentes propios.\n\n- Motor: eventos tipados y journal.\n- Interfaz: openide-ui.\n\n```rust\nfn main() {}\n```".into(),
                streaming: false,
            },
        ];
        export(&state, "timeline", 1400.0, 900.0);
        state.sidebar_collapsed = true;
        export(&state, "timeline", 480.0, 760.0);
        state.transcript.push(TranscriptItem::Assistant {
            content: "## Fuente extensa\n\nLectura íntegra con UTF-8: ñ🙂漢字\n".repeat(600),
            streaming: false,
        });
        let index = state.transcript.len() - 1;
        let id = format!("conversation_source:visual-fixture:{index}:0:open");
        assert!(crate::conversation_source_ui::apply_action(&mut state, &id));
        export(&state, "complete-response", 480.0, 760.0);
        export(&state, "complete-response", 1400.0, 900.0);
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
                    } if *color == Color::rgb(38, 38, 38) && *radius == 10.0 => Some(*rect),
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
                avatar: None,
            }),
        ] {
            state.github_account = status;
            let frame = UiTree::new(sidebar_account_footer(&state, 264.0)).layout(Size::new(
                264.0,
                crate::sidebar_chrome::ACCOUNT_FOOTER_HEIGHT,
            ));
            let account_id = if matches!(state.github_account, GithubAccountStatus::Connected(_)) {
                "github_account_menu_toggle"
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
            assert_eq!(settings.rect.width, 24.0);
            assert_eq!(settings.rect.height, 24.0);
            let gear = frame.commands.iter().find_map(|command| match command {
                DrawCommand::Icon {
                    icon: Icon::Settings,
                    rect,
                    ..
                } => Some(rect),
                _ => None,
            });
            let gear = gear.expect("settings icon");
            assert_eq!((gear.width, gear.height), (16.0, 16.0));
            assert!((gear.x + gear.width / 2.0 - settings.rect.x - 12.0).abs() < 1.0);
            assert!((gear.y + gear.height / 2.0 - settings.rect.y - 12.0).abs() < 1.0);
        }
    }

    #[test]
    fn sidebar_shortcuts_fit_compact_navigation_without_empty_badges() {
        for (width, expected_keys) in [(132.0, 0), (182.0, 2), (246.0, 2)] {
            let mut tree = UiTree::new(sidebar_nav_action(
                "new_session_sidebar",
                Icon::Add,
                "Nueva tarea",
                "Ctrl+N",
                width,
                false,
            ));
            tree.set_text_measurer(|text, _, _| text.chars().count() as f32 * 7.0);
            let frame = tree.layout(Size::new(width, 28.0));
            assert_eq!(frame.regions.len(), 1, "keycaps are passive");
            let keys: Vec<_> = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text {
                        text,
                        position,
                        font_family: FontFamily::Sans,
                        ..
                    } if ["Ctrl", "N"].contains(&text.as_str()) => Some((text, position)),
                    _ => None,
                })
                .collect();
            assert_eq!(keys.len(), expected_keys, "at {width}px");
            for (key, position) in keys {
                assert!(position.x >= 8.0);
                assert!(position.x + key.chars().count() as f32 * 7.0 <= width - 8.0);
                assert!(position.y >= 0.0 && position.y + 11.0 <= 28.0);
            }
            for command in &frame.commands {
                if let DrawCommand::FillRect { rect, radius, .. } = command {
                    if *radius == 3.0 && rect.height == 18.0 {
                        assert!(rect.x >= 8.0 && rect.x + rect.width <= width - 8.0);
                        assert!(rect.y >= 0.0 && rect.y + rect.height <= 28.0);
                    }
                }
            }
        }
        let frame = UiTree::new(sidebar_nav_action(
            "open_automations",
            Icon::Clock,
            "Automatizaciones",
            "",
            246.0,
            false,
        ))
        .layout(Size::new(246.0, 28.0));
        assert!(
            !frame.commands.iter().any(|command| matches!(command,
                DrawCommand::FillRect { radius, rect, .. } if *radius == 3.0 && rect.height == 18.0
            )),
            "actions without shortcuts do not show empty badges"
        );
    }

    #[test]
    fn command_center_hints_stay_inside_compact_card_and_clear_scrollbar() {
        let mut state = empty_state();
        state.command_results = crate::command_center::search(
            "",
            Scope::All,
            &[],
            &[],
            crate::command_center::DEFAULT_RESULT_LIMIT,
        );
        for width in [300.0, 560.0] {
            let mut tree = UiTree::new(build_command_center(&state, width, 420.0));
            tree.set_text_measurer(|text, _, _| text.chars().count() as f32 * 7.0);
            let frame = tree.layout(Size::new(width, 420.0));
            for key in [
                if cfg!(target_os = "macos") {
                    "⌃"
                } else {
                    "Ctrl"
                },
                "N",
                "Enter",
            ] {
                assert!(
                    frame.commands.iter().any(|command| matches!(command,
                        DrawCommand::Text { text, font_family: FontFamily::Sans, .. } if text == key
                    )),
                    "missing {key} at {width}px"
                );
            }
            for arrow in [Icon::ArrowUp, Icon::ArrowDown] {
                assert!(
                    frame.commands.iter().any(|command| matches!(command,
                        DrawCommand::Icon { icon, rect, .. } if *icon == arrow
                            && rect.x >= 10.0 && rect.x + rect.width <= width - 10.0
                            && rect.y >= 10.0 && rect.y + rect.height <= 410.0
                    )),
                    "native navigation hint must fit within the card"
                );
            }
            let result = frame
                .regions
                .iter()
                .find(|region| region.id == "command_result:0")
                .unwrap();
            let rail = frame
                .regions
                .iter()
                .find(|region| region.kind == openide_ui::HitKind::Scrollbar)
                .unwrap();
            let mut cap_count = 0;
            for command in &frame.commands {
                if let DrawCommand::FillRect { rect, radius, .. } = command {
                    if *radius == 3.0 && rect.height == 18.0 {
                        cap_count += 1;
                        assert!(rect.x >= 10.0 && rect.x + rect.width <= width - 10.0);
                        assert!(rect.y >= 10.0 && rect.y + rect.height <= 410.0);
                        if rect.y >= result.rect.y && rect.y < result.rect.y + result.rect.height {
                            assert!(rect.x + rect.width < rail.rect.x);
                        }
                    }
                }
            }
            assert!(
                cap_count >= 4,
                "escape, enter and both navigation caps remain visible"
            );
        }
    }

    #[test]
    fn shortcut_settings_keep_long_key_combinations_within_compact_cards() {
        let state = empty_state();
        for width in [360.0, 1300.0] {
            let mut tree = UiTree::new(settings_ui::build_settings_page(
                &state,
                SettingsPage::Shortcuts,
                width,
                true,
            ));
            tree.set_text_measurer(|text, _, _| text.chars().count() as f32 * 7.0);
            tree.layout(Size::new(width, 760.0));
            tree.scroll_to_end("settings_page:Shortcuts");
            let frame = tree.layout(Size::new(width, 760.0));
            for key in ["Ctrl", "Shift", "M"] {
                assert!(
                    frame.commands.iter().any(|command| matches!(command,
                        DrawCommand::Text { text, font_family: FontFamily::Sans, .. } if text == key
                    )),
                    "missing {key} at {width}px"
                );
            }
            let right = frame
                .regions
                .iter()
                .find(|region| region.kind == openide_ui::HitKind::Scrollbar)
                .map_or(width - 14.0, |region| region.rect.x);
            for command in &frame.commands {
                if let DrawCommand::FillRect { rect, radius, .. } = command {
                    if *radius == 3.0 && rect.height == 18.0 {
                        assert!(rect.x >= 14.0 && rect.x + rect.width < right);
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "export native keyboard shortcut surfaces for manual visual review"]
    fn export_keyboard_shortcut_visual_review() {
        let output = std::path::PathBuf::from(format!(
            "{}/../../target/kbd-visual-qa",
            env!("CARGO_MANIFEST_DIR")
        ));
        std::fs::create_dir_all(&output).unwrap();
        let mut state = empty_state();
        state.command_results = crate::command_center::search(
            "",
            Scope::All,
            &[],
            &[],
            crate::command_center::DEFAULT_RESULT_LIMIT,
        );
        let mut cases = Vec::new();
        for width in [300.0, 560.0] {
            cases.push((
                format!("command-center-{width:.0}"),
                width,
                520.0,
                build_command_center(&state, width, 520.0),
            ));
        }
        for width in [176.0, 264.0] {
            cases.push((
                format!("sidebar-{width:.0}"),
                width,
                720.0,
                sidebar(&state, width),
            ));
        }
        {
            use crate::notifications::{Severity, Target};
            let task = |id: &str| Some(Target::Session(id.into()));
            let center = &mut state.notifications;
            center.push(Severity::Info, "Turno completado", "Ordenar las importaciones", task("a"));
            center.push(Severity::Error, "Turno fallido", "Migrar la base de datos", task("b"));
            center.push(Severity::Attention, "Permiso pendiente: run_command", "Arreglar el login", task("c"));
            center.push(Severity::Attention, "Permiso pendiente: run_command", "Arreglar el login", task("c"));
            cases.push(("sidebar-bell-264".into(), 264.0, 420.0, sidebar(&state, 264.0)));
            state.notifications.toggle();
            cases.push(("sidebar-notifications-420".into(), 420.0, 420.0, sidebar(&state, 264.0)));
            state.notifications.close();
            cases.push((
                "toast-560".into(),
                560.0,
                300.0,
                crate::notifications_ui::toast_layer(
                    &state,
                    Element::spacer()
                        .background(theme::BACKGROUND)
                        .size(Some(560.0), Some(300.0)),
                ),
            ));
            cases.push((
                "settings-notifications-700".into(),
                700.0,
                420.0,
                crate::notifications_ui::settings_group(&state.notifications, false),
            ));
            state.notifications.clear();
        }
        {
            state.agent_clis = vec![
                AgentCliRowView {
                    id: "claude".into(),
                    label: "Claude Code".into(),
                    resolved: Some("/usr/bin/claude".into()),
                    manual: String::new(),
                },
                AgentCliRowView {
                    id: "codex".into(),
                    label: "Codex".into(),
                    resolved: Some("/usr/bin/codex".into()),
                    manual: String::new(),
                },
            ];
            cases.push(("home-cli-1100".into(), 1100.0, 700.0, build_view_sized(&state, 1100.0, 700.0)));
            state.selected_session = Some("cli-task".into());
            state.selected_session_title = Some("Claude Code".into());
            state.selected_backend = "cli:claude".into();
            state.show_terminal_panel = true;
            cases.push(("cli-task-1100".into(), 1100.0, 700.0, build_view_sized(&state, 1100.0, 700.0)));
            let task = |id: &str, title: &str, backend: &str| SessionView {
                id: id.into(),
                title: title.into(),
                project: state.workspace.clone(),
                backend: backend.into(),
                pinned: false,
                archived: false,
                unread: false,
                native_automation: None,
            };
            let tasks = vec![
                task("cli-task", "Claude Code", "cli:claude"),
                task("chat", "Revisar el editor", "native"),
                task("codex-task", "Codex", "cli:codex"),
            ];
            state.sessions = tasks;
            state.ide.mode = crate::ide_workspace::WindowMode::Ide;
            state.ide.agent_tabs = vec!["cli-task".into(), "chat".into()];
            cases.push(("ide-agent-tabs-1400".into(), 1400.0, 760.0, build_view_sized(&state, 1400.0, 760.0)));
            state.ide.agent_list_open = true;
            state.ide.agent_width = 620.0;
            cases.push(("ide-agent-list-1400".into(), 1400.0, 760.0, build_view_sized(&state, 1400.0, 760.0)));
            state.ide = Default::default();
            state.sessions.clear();
            cases.push((
                "environment-360".into(),
                360.0,
                420.0,
                crate::environment_ui::panel(&state, 360.0, 420.0),
            ));
            state.selected_session = None;
            state.selected_backend = "native".into();
            state.show_terminal_panel = false;
        }
        for page in [SettingsPage::Usage, SettingsPage::Mcp, SettingsPage::Automations] {
            cases.push((
                format!("settings-{page:?}-1000"),
                1000.0,
                1100.0,
                settings_ui::build_settings_page(&state, page, 1000.0, true),
            ));
        }
        for page in [SettingsPage::Shortcuts, SettingsPage::WorkspaceSearch] {
            for width in [360.0, 1300.0] {
                cases.push((
                    format!("settings-{page:?}-{width:.0}"),
                    width,
                    760.0,
                    settings_ui::build_settings_page(&state, page, width, true),
                ));
            }
        }
        for (name, width, height, root) in cases {
            let mut tree = UiTree::new(root);
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, height));
            let path = output.join(format!("{name}.svg"));
            settings_ui::visual_qa::write_frame_svg(&frame, &path, width, height).unwrap();
            let svg = std::fs::read_to_string(&path).unwrap();
            let mut options = resvg::usvg::Options::default();
            options.fontdb_mut().load_system_fonts();
            let svg_tree = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
            resvg::render(
                &svg_tree,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            eprintln!("{}", path.display());
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
            let nav = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Text { text, position, .. } if text.starts_with("Nue") => {
                        Some(*position)
                    }
                    _ => None,
                })
                .expect("new task navigation label");
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
                avatar: None,
            });
        let frame =
            UiTree::new(sidebar_account_footer(&state, 176.0)).layout(Size::new(176.0, 52.0));
        let account = frame
            .regions
            .iter()
            .find(|region| region.id == "github_account_menu_toggle")
            .unwrap()
            .rect;
        let github = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Icon {
                    icon: Icon::ChevronDown,
                    rect,
                    ..
                } => Some(*rect),
                _ => None,
            })
            .expect("connected account menu indicator");
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
        }
        .into()));
        let frame = UiTree::new(code_review_content(&state, 360.0)).layout(Size::new(360.0, 800.0));
        let labels: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(labels.contains(&"Preparados"), "{labels:?}");
        assert!(labels.contains(&"main.rs"));
        assert!(labels.contains(&"src"));
        assert!(
            !labels.iter().any(|label| label.contains("+fn main()")),
            "summary must not render a patch before selection"
        );
    }

    #[test]
    fn review_scroll_reaches_changes_past_the_initial_viewport() {
        let mut state = empty_state();
        state.git_review = Some(Ok(GitReview {
            branch: "main".into(),
            untracked: (0..1000)
                .map(|index| format!("file-{index:04}.rs"))
                .collect(),
            ..GitReview::default()
        }
        .into()));
        let viewport = Size::new(360.0, 600.0);
        let mut tree = UiTree::new(code_review_content(&state, 360.0));
        tree.set_reduced_motion(true);
        let first = tree.layout(viewport);
        assert!(first
            .regions
            .iter()
            .any(|region| region.id == "review_untracked_open:0"));
        assert!(
            first
                .regions
                .iter()
                .filter(|region| region.id.starts_with("review_untracked_open:"))
                .count()
                < 30
        );
        assert!(!first
            .regions
            .iter()
            .any(|region| region.id.starts_with("review_page_")));
        tree.handle_event(UiEvent::ScrollDirect {
            position: Point::new(150.0, 250.0),
            delta_y: -100_000.0,
        });
        let end = tree.layout(viewport);
        assert!(end
            .regions
            .iter()
            .any(|region| region.id == "review_untracked_open:999"));
        assert!(!end
            .regions
            .iter()
            .any(|region| region.id == "review_untracked_open:0"));
    }

    #[test]
    fn empty_right_dock_exposes_only_real_views() {
        let mut state = empty_state();
        state.dock_tabs.clear();
        let frame = UiTree::new(side_pane(&state, 420.0)).layout(Size::new(420.0, 600.0));
        for id in [
            "dock_open:files",
            "dock_open:activity",
            "dock_open:review",
            "dock_open:terminal",
            "dock_open:browser",
            "dock_open:changes",
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
            full_path: None,
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
                activity: crate::transcript::ActivityStatus::Completed,
            });
            state.dock_tabs.push(DockTabView {
                full_path: None,
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
    fn right_header_shrinks_tabs_without_overlapping_actions_at_compact_widths() {
        let mut state = empty_state();
        state.dock_tabs = (1..=5)
            .map(|id| DockTabView {
                id,
                full_path: None,
                title: format!("Archivo {id}"),
                active: id == 1,
                kind: SideTab::FilePreview,
                closable: true,
            })
            .collect();
        for width in [320.0, 430.0, 540.0] {
            let frame = UiTree::new(side_pane(&state, width)).layout(Size::new(width, 360.0));
            let regions: Vec<_> = frame
                .regions
                .iter()
                .filter(|region| {
                    region.id.starts_with("dock_tab:")
                        || region.id.starts_with("dock_close:")
                        || ["dock_tabs_menu", "dock_add_tab", "toggle_side_pane"]
                            .contains(&region.id.as_str())
                })
                .collect();
            for region in &regions {
                let rect = region.rect;
                assert!(
                    rect.x >= 0.0 && rect.x + rect.width <= width + 0.1,
                    "{}: {rect:?}",
                    region.id
                );
                assert!(rect.y >= 0.0 && rect.y + rect.height <= RIGHT_HEADER_HEIGHT);
                if let Some(tab_id) = region.id.strip_prefix("dock_close:") {
                    let parent_id = format!("dock_tab:{tab_id}");
                    let parent = regions
                        .iter()
                        .find(|candidate| candidate.id == parent_id)
                        .expect("every visible close belongs to a visible tab")
                        .rect;
                    assert!(
                        rect.x >= parent.x
                            && rect.y >= parent.y
                            && rect.x + rect.width <= parent.x + parent.width + 0.1
                            && rect.y + rect.height <= parent.y + parent.height + 0.1,
                        "close must stay inside its own tab: {}",
                        region.id
                    );
                    assert_eq!(
                        frame.hit_test(Point::new(
                            rect.x + rect.width / 2.0,
                            rect.y + rect.height / 2.0,
                        )),
                        Some(region.id.as_str()),
                        "the close action must take priority over its parent tab"
                    );
                }
            }
            let is_own_close = |parent: &str, child: &str| match (
                parent.strip_prefix("dock_tab:"),
                child.strip_prefix("dock_close:"),
            ) {
                (Some(parent), Some(child)) => parent == child,
                _ => false,
            };
            for (index, left) in regions.iter().enumerate() {
                for right in regions.iter().skip(index + 1) {
                    if is_own_close(&left.id, &right.id) || is_own_close(&right.id, &left.id) {
                        continue;
                    }
                    assert!(
                        left.rect.x + left.rect.width <= right.rect.x + 0.1
                            || right.rect.x + right.rect.width <= left.rect.x + 0.1,
                        "overlap: {} {}",
                        left.id,
                        right.id
                    );
                }
            }
            assert!(regions.iter().any(|region| region.id == "dock_tab:1"));
            assert!(regions.iter().any(|region| region.id == "dock_close:1"));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id.starts_with("dock_tab_page")));
        }
    }

    #[test]
    fn dock_add_tab_uses_a_compact_anchored_popover_with_real_views() {
        let mut state = empty_state();
        state.side_tab = SideTab::Files;
        state.dock_tabs.clear();
        state.dock_tabs.push(DockTabView {
            full_path: None,
            id: 1,
            title: "Archivos".into(),
            active: true,
            kind: SideTab::Files,
            closable: true,
        });
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
        assert!(popup.height < 290.0, "six real views fit a compact menu");
        for id in [
            "dock_open:terminal",
            "dock_open:files",
            "dock_open:activity",
            "dock_open:review",
            "dock_open:browser",
            "dock_open:changes",
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
            24.0,
            theme::hover_for(theme::SIDEBAR),
        );
        assert_dock_icon_hover_geometry(
            sidebar(&state, 280.0),
            Size::new(280.0, 500.0),
            "session_sort_toggle",
            Icon::Filter,
            24.0,
            theme::hover_for(theme::SIDEBAR),
        );
        assert_dock_icon_hover_geometry(
            sidebar(&state, 280.0),
            Size::new(280.0, 500.0),
            "toggle_settings",
            Icon::Settings,
            24.0,
            theme::hover_for(theme::SIDEBAR),
        );
        assert_dock_icon_hover_geometry(
            side_pane(&state, 430.0),
            Size::new(430.0, 500.0),
            "dock_tabs_menu",
            Icon::Menu,
            crate::dock_tab_ui::TOOL_SIZE,
            theme::component_palette().hover,
        );
        assert_dock_icon_hover_geometry(
            side_pane(&state, 430.0),
            Size::new(430.0, 500.0),
            "toggle_side_pane",
            Icon::PanelRightClose,
            crate::dock_tab_ui::TOOL_SIZE,
            theme::component_palette().hover,
        );
    }

    #[test]
    fn dock_icon_tooltips_keep_keyboard_actions_and_shared_hints() {
        let mut state = empty_state();
        let cases = [
            (
                side_pane(&state, 430.0),
                "dock_tabs_menu",
                "Todas las pestañas",
            ),
            (
                code_review_content(&state, 430.0),
                "review_refresh",
                "Actualizar cambios",
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
            if id == "dock_tabs_menu" {
                for key in ["Ctrl", "Tab"] {
                    assert!(focused.commands.iter().any(|command| matches!(command,
                        DrawCommand::Text { text, .. } if text == key)));
                }
            }
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
        state.dock_tabs = vec![DockTabView {
            full_path: None,
            id: 7,
            title: "example.rs".into(),
            active: true,
            kind: SideTab::FilePreview,
            closable: true,
        }];
        let mut tree = UiTree::new(dock_chrome::header(&state, 430.0));
        let viewport = Size::new(430.0, 500.0);
        tree.layout(viewport);
        assert!(tree.focus("dock_close:7"));
        let focused = tree.layout(viewport);
        assert!(focused.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text == "Cerrar example.rs"
        )));
    }

    #[test]
    fn home_offers_right_dock_toggle_without_moving_the_composer() {
        let mut state = empty_state();
        let viewport = Size::new(1200.0, 700.0);
        let visible =
            UiTree::new(build_view_sized(&state, viewport.width, viewport.height)).layout(viewport);
        assert!(!visible
            .regions
            .iter()
            .any(|region| region.id == "toggle_side_pane_header"));
        assert!(visible.regions.iter().any(|region| region.id == "prompt"));
        state.show_side_pane = false;
        let collapsed =
            UiTree::new(build_view_sized(&state, viewport.width, viewport.height)).layout(viewport);
        let toggle = collapsed
            .regions
            .iter()
            .find(|region| region.id == "toggle_side_pane_header")
            .expect("right dock reopen action")
            .rect;
        assert!(collapsed.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon { icon: Icon::PanelRightOpen, rect, tint }
                if *tint == theme::FOREGROUND
                    && (rect.x + rect.width / 2.0 - toggle.x - toggle.width / 2.0).abs() < 0.01
                    && (rect.y + rect.height / 2.0 - toggle.y - toggle.height / 2.0).abs() < 0.01
        )), "the available dock action must not use muted ink");
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
        let standalone = UiTree::new(home::build_home_sized(
            &state,
            center_width,
            viewport.height - 2.0 * GAP,
        ))
        .layout(Size::new(center_width, viewport.height - 2.0 * GAP));
        let standalone_prompt = standalone
            .regions
            .iter()
            .find(|region| region.id == "prompt")
            .expect("standalone home composer")
            .rect;
        assert!((collapsed_prompt.y - standalone_prompt.y - GAP).abs() < 0.01);
        let narrow =
            UiTree::new(build_view_sized(&state, 900.0, 700.0)).layout(Size::new(900.0, 700.0));
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
        let settings = rect(&collapsed, "toggle_settings_header");
        assert!(reopen.x + reopen.width <= settings.x);
        assert_eq!(reopen.y, settings.y);
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

    fn settings_test_control_rect(frame: &openide_ui::Frame, id: &str) -> openide_ui::Rect {
        frame
            .regions
            .iter()
            .find(|region| region.id == id)
            .unwrap_or_else(|| panic!("missing Settings control {id}"))
            .rect
    }

    fn settings_test_control_fill(frame: &openide_ui::Frame, id: &str) -> Color {
        let bounds = settings_test_control_rect(frame, id);
        frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::FillRect { rect, color, .. }
                    if (rect.x - bounds.x).abs() < 0.01
                        && (rect.y - bounds.y).abs() < 0.01
                        && (rect.width - bounds.width).abs() < 0.01
                        && (rect.height - bounds.height).abs() < 0.01 =>
                {
                    Some(*color)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing Settings fill for {id}: {bounds:?}"))
    }

    #[test]
    fn settings_body_links_and_sidebar_have_independent_animated_hover_state() {
        use std::time::{Duration, Instant};
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::General;
        state.settings_reduced_motion = false;
        for width in [1300.0, 1412.0] {
            let start = Instant::now();
            let viewport = Size::new(width, 830.0);
            let mut tree = UiTree::new(build_view(&state, width));
            tree.set_reduced_motion(false);
            for milliseconds in [0, 16, 60, 140, 320, 600] {
                let frame = tree.layout_at(viewport, start + Duration::from_millis(milliseconds));
                assert_eq!(
                    settings_test_control_fill(&frame, "settings_page:general"),
                    theme::SELECTED
                );
                for suffix in ["appearance", "shortcuts"] {
                    assert_eq!(
                        settings_test_control_fill(&frame, &format!("settings_page:{suffix}")),
                        theme::SIDEBAR,
                        "idle sidebar {suffix} at {milliseconds}ms, width {width}"
                    );
                }
                for suffix in ["appearance", "data", "shortcuts"] {
                    assert_eq!(
                        settings_test_control_fill(&frame, &format!("settings_link:{suffix}")),
                        Color::rgb(55, 55, 55)
                    );
                }
            }
            let frame = tree.layout_at(viewport, start + Duration::from_millis(600));
            let center = |rect: openide_ui::Rect| {
                Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
            };
            let appearance_link = center(settings_test_control_rect(
                &frame,
                "settings_link:appearance",
            ));
            let shortcuts_link = center(settings_test_control_rect(
                &frame,
                "settings_link:shortcuts",
            ));
            let appearance_nav = center(settings_test_control_rect(
                &frame,
                "settings_page:appearance",
            ));
            tree.handle_event_at(
                UiEvent::PointerMove(appearance_link),
                start + Duration::from_millis(600),
            );
            tree.layout_at(viewport, start + Duration::from_millis(600));
            let middle = tree.layout_at(viewport, start + Duration::from_millis(660));
            let fill = settings_test_control_fill(&middle, "settings_link:appearance");
            assert!(
                fill.r > 55 && fill.r < 66,
                "the body control still animates"
            );
            assert_eq!(
                settings_test_control_fill(&middle, "settings_page:appearance"),
                theme::SIDEBAR
            );
            let hovered = tree.layout_at(viewport, start + Duration::from_millis(740));
            assert_eq!(
                settings_test_control_fill(&hovered, "settings_link:appearance"),
                Color::rgb(66, 66, 66)
            );
            assert_eq!(
                settings_test_control_fill(&hovered, "settings_page:appearance"),
                theme::SIDEBAR
            );
            tree.set_root(build_view(&state, width));
            let rebuilt = tree.layout_at(viewport, start + Duration::from_millis(740));
            assert_eq!(
                settings_test_control_fill(&rebuilt, "settings_link:appearance"),
                Color::rgb(66, 66, 66)
            );
            assert_eq!(
                settings_test_control_fill(&rebuilt, "settings_page:appearance"),
                theme::SIDEBAR
            );

            tree.handle_event_at(
                UiEvent::PointerMove(appearance_nav),
                start + Duration::from_millis(800),
            );
            tree.layout_at(viewport, start + Duration::from_millis(800));
            let nav_hovered = tree.layout_at(viewport, start + Duration::from_millis(940));
            assert_eq!(
                settings_test_control_fill(&nav_hovered, "settings_page:appearance"),
                theme::hover_for(theme::SIDEBAR)
            );
            assert_eq!(
                settings_test_control_fill(&nav_hovered, "settings_link:appearance"),
                Color::rgb(55, 55, 55)
            );
            assert_eq!(
                settings_test_control_fill(&nav_hovered, "settings_link:shortcuts"),
                Color::rgb(55, 55, 55)
            );

            tree.handle_event_at(
                UiEvent::PointerMove(shortcuts_link),
                start + Duration::from_millis(1000),
            );
            tree.layout_at(viewport, start + Duration::from_millis(1000));
            let shortcuts_hovered = tree.layout_at(viewport, start + Duration::from_millis(1140));
            assert_eq!(
                settings_test_control_fill(&shortcuts_hovered, "settings_link:shortcuts"),
                Color::rgb(66, 66, 66)
            );
            assert_eq!(
                settings_test_control_fill(&shortcuts_hovered, "settings_page:shortcuts"),
                theme::SIDEBAR
            );
            assert_eq!(
                settings_test_control_fill(&shortcuts_hovered, "settings_page:appearance"),
                theme::SIDEBAR
            );
            tree.handle_event_at(UiEvent::PointerLeave, start + Duration::from_millis(1200));
            tree.layout_at(viewport, start + Duration::from_millis(1200));
            let idle = tree.layout_at(viewport, start + Duration::from_millis(1340));
            assert_eq!(
                settings_test_control_fill(&idle, "settings_page:general"),
                theme::SELECTED
            );
            for suffix in ["appearance", "shortcuts"] {
                assert_eq!(
                    settings_test_control_fill(&idle, &format!("settings_page:{suffix}")),
                    theme::SIDEBAR
                );
                assert_eq!(
                    settings_test_control_fill(&idle, &format!("settings_link:{suffix}")),
                    Color::rgb(55, 55, 55)
                );
            }
            tree.scroll_to_end("settings_navigation");
            let bottom = tree.layout_at(viewport, start + Duration::from_millis(1340));
            assert_eq!(
                settings_test_control_fill(&bottom, "settings_page:data"),
                theme::SIDEBAR
            );
            assert_eq!(
                settings_test_control_fill(&bottom, "settings_link:data"),
                Color::rgb(55, 55, 55)
            );
        }
    }

    #[test]
    fn settings_body_links_have_unique_focus_targets_and_dispatch_original_routes() {
        use std::time::{Duration, Instant};
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::General;
        for suffix in ["appearance", "data", "shortcuts"] {
            let start = Instant::now();
            let viewport = Size::new(1412.0, 830.0);
            let mut tree = UiTree::new(build_view(&state, viewport.width));
            tree.set_reduced_motion(false);
            let frame = tree.layout_at(viewport, start);
            let mut ids = std::collections::HashSet::new();
            for region in frame.regions.iter().filter(|region| {
                matches!(
                    region.kind,
                    openide_ui::HitKind::Button
                        | openide_ui::HitKind::TextInput
                        | openide_ui::HitKind::TextArea
                )
            }) {
                assert!(ids.insert(&region.id), "duplicate widget ID {}", region.id);
            }
            let link = format!("settings_link:{suffix}");
            let route = format!("settings_page:{suffix}");
            assert!(tree.focus(&link));
            assert_eq!(tree.focused_id(), Some(link.as_str()));
            let focused = tree.layout_at(viewport, start + Duration::from_millis(140));
            assert_eq!(
                settings_test_control_fill(&focused, "settings_page:general"),
                theme::SELECTED
            );
            assert_eq!(
                tree.handle_event_at(
                    UiEvent::Key {
                        key: Key::Enter,
                        shift: false,
                        ctrl: false
                    },
                    start + Duration::from_millis(140)
                ),
                vec![UiAction::Clicked { id: route.clone() }]
            );
            let rect = settings_test_control_rect(&focused, &link);
            let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            tree.handle_event_at(
                UiEvent::PointerDown(point),
                start + Duration::from_millis(160),
            );
            assert_eq!(
                tree.handle_event_at(
                    UiEvent::PointerUp(point),
                    start + Duration::from_millis(170)
                ),
                vec![UiAction::Clicked { id: route.clone() }]
            );
            assert_eq!(tree.focused_id(), Some(link.as_str()));
            assert!(tree.focus(&route));
            assert_eq!(tree.focused_id(), Some(route.as_str()));
            tree.layout_at(viewport, start + Duration::from_millis(320));
            assert_eq!(
                tree.handle_event_at(
                    UiEvent::Key {
                        key: Key::Enter,
                        shift: false,
                        ctrl: false
                    },
                    start + Duration::from_millis(320)
                ),
                vec![UiAction::Clicked { id: route }]
            );
        }
    }

    fn settings_action_reachable(tree: &mut UiTree, action: &str, width: f32, height: f32) -> bool {
        // Smooth-scroll reachability must depend on elapsed UI time, not on
        // how expensive the machine's layout implementation happens to be.
        let start = std::time::Instant::now();
        for step in 0..120 {
            let now = start + std::time::Duration::from_secs_f64(step as f64 / 120.0);
            let frame = tree.layout_at(Size::new(width, height), now);
            if frame.regions.iter().any(|region| region.id == action) {
                return true;
            }
            tree.handle_event_at(
                UiEvent::Scroll {
                    position: Point::new(width * 0.65, height * 0.5),
                    delta_y: -180.0,
                },
                now,
            );
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
            (SettingsPage::Terminal, "settings_terminal_shell_toggle"),
            (SettingsPage::WorkspaceSearch, "open_command_center"),
            (SettingsPage::Mcp, "mcp_new"),
            (SettingsPage::Skills, "open_marketplace"),
            (SettingsPage::Automations, "open_automations"),
            (SettingsPage::Data, "session_organize:recent"),
            (SettingsPage::Github, "github_account_connect"),
        ] {
            state.settings_page = page;
            state.settings_choice_menu_open = None;
            let mut tree = UiTree::new(build_view(&state, 1100.0));
            let menu = match action {
                "command_isolation:host_unrestricted" => Some("isolation"),
                "session_organize:recent" => Some("organization"),
                _ => None,
            };
            if let Some(menu) = menu {
                assert!(settings_action_reachable(
                    &mut tree,
                    &format!("settings_choice_toggle:{menu}"),
                    1100.0,
                    720.0,
                ));
                state.settings_choice_menu_open = Some(menu.into());
                tree.set_root(build_view(&state, 1100.0));
            }
            assert!(
                settings_action_reachable(&mut tree, action, 1100.0, 720.0),
                "{page:?} missing {action}"
            );
        }
    }

    #[test]
    fn settings_terminal_shell_selector_and_conditional_editor_fit_and_accept_keyboard() {
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Terminal;
        for width in [360.0, 620.0, 1300.0] {
            state.settings_terminal_shell_menu_open = false;
            state.settings_terminal_shell_editing = false;
            state.settings_terminal_shell_draft.clear();
            let mut tree = UiTree::new(build_view(&state, width));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 900.0));
            let trigger = frame
                .regions
                .iter()
                .find(|region| region.id == "settings_terminal_shell_toggle")
                .unwrap()
                .rect;
            assert!(trigger.x >= 0.0 && trigger.x + trigger.width <= width);
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "settings_terminal_shell_path"));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id.starts_with("settings_terminal_shell:")));
            let center = Point::new(
                trigger.x + trigger.width / 2.0,
                trigger.y + trigger.height / 2.0,
            );
            tree.handle_event(UiEvent::PointerDown(center));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(center)),
                vec![UiAction::Clicked {
                    id: "settings_terminal_shell_toggle".into()
                }]
            );

            state.settings_terminal_shell_menu_open = true;
            tree.set_root(build_view(&state, width));
            let frame = tree.layout(Size::new(width, 900.0));
            for id in [
                "settings_terminal_shell:system",
                "settings_terminal_shell_edit",
            ] {
                let option = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap()
                    .rect;
                assert!(
                    option.x >= 0.0 && option.x + option.width <= width,
                    "{id} at {width}px"
                );
            }
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Escape,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "settings_terminal_shell_dismiss".into()
                }]
            );
            assert!(tree.focus("settings_terminal_shell:system"));
            tree.handle_event(UiEvent::Key {
                key: Key::End,
                shift: false,
                ctrl: false,
            });
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "settings_terminal_shell_edit".into()
                }]
            );

            state.settings_terminal_shell_menu_open = false;
            state.settings_terminal_shell_editing = true;
            for draft in ["", "relative-shell"] {
                state.settings_terminal_shell_draft = draft.into();
                tree.set_root(build_view(&state, width));
                let frame = tree.layout(Size::new(width, 900.0));
                for id in [
                    "settings_terminal_shell_path",
                    "settings_terminal_shell_cancel",
                ] {
                    let control = frame
                        .regions
                        .iter()
                        .find(|region| region.id == id)
                        .unwrap()
                        .rect;
                    assert!(control.x >= 0.0 && control.x + control.width <= width);
                }
                assert!(
                    !tree.focus("settings_terminal_shell:custom"),
                    "invalid draft must not submit"
                );
            }
            state.settings_terminal_shell_draft = std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            tree.set_root(build_view(&state, width));
            let frame = tree.layout(Size::new(width, 900.0));
            let save = frame
                .regions
                .iter()
                .find(|region| region.id == "settings_terminal_shell:custom")
                .unwrap()
                .rect;
            assert!(save.x >= 0.0 && save.x + save.width <= width);
            assert!(tree.focus("settings_terminal_shell:custom"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "settings_terminal_shell:custom".into()
                }]
            );
        }
        state.settings_terminal_shell_editing = false;
        let saved_path = std::env::current_exe().unwrap().parent().unwrap()
            .join("very-long-custom-shell-directory-for-a-workspace/another-directory-with-a-long-name/shell-executable");
        state.settings_terminal_shell = TerminalShell::Custom(saved_path.clone());
        state.settings_terminal_shell_draft = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut tree = UiTree::new(build_view(&state, 360.0));
        assert!(settings_action_reachable(
            &mut tree,
            "settings_terminal_shell_path",
            360.0,
            900.0
        ));
        let frame = tree.layout(Size::new(360.0, 900.0));
        let save = frame
            .regions
            .iter()
            .find(|region| region.id == "settings_terminal_shell:custom")
            .unwrap()
            .rect;
        let path_lines: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { position, text, .. }
                    if text.starts_with("Ruta guardada:")
                        || text.contains("very-long-custom-shell")
                        || text.contains("another-directory-with")
                        || text.contains("shell-executable") =>
                {
                    Some(*position)
                }
                _ => None,
            })
            .collect();
        assert!(
            !path_lines.is_empty(),
            "saved path stays discoverable while draft changes"
        );
        for line in path_lines {
            assert!(
                line.y >= save.y + save.height && line.x >= 0.0 && line.x < 360.0,
                "saved path overlaps actions: {line:?}"
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
                instructions: "---\nname: review\ndescription: Review changes\n---\nReview the complete changes.".into(),
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
    #[ignore = "export native scroll frames for manual motion and scrollbar review"]
    fn export_scroll_area_visual_review() {
        let output = std::path::PathBuf::from(format!(
            "{}/../../target/scroll-area-visual-qa",
            env!("CARGO_MANIFEST_DIR")
        ));
        std::fs::create_dir_all(&output).unwrap();
        let mut options = resvg::usvg::Options::default();
        options.fontdb_mut().load_system_fonts();
        let save_frame = |frame: &openide_ui::Frame, name: &str, width: f32| {
            let path = output.join(format!("{name}.svg"));
            settings_ui::visual_qa::write_frame_svg(frame, &path, width, 520.0).unwrap();
            let svg = std::fs::read_to_string(&path).unwrap();
            let svg_tree = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 520).unwrap();
            resvg::render(
                &svg_tree,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            eprintln!("{}", path.display());
        };
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Agent;
        state.native_provider_id = "openai".into();
        state.model = "gpt-4.1".into();
        let mut cases = Vec::new();
        for width in [360.0, 1300.0] {
            cases.push((
                format!("agent-{width:.0}"),
                width,
                settings_ui::build_settings_page(&state, SettingsPage::Agent, width, true)
                    .background(theme::BACKGROUND),
            ));
        }
        cases.push((
            "settings-sidebar-264".into(),
            264.0,
            settings_ui::build_settings_sidebar(&state, 264.0).background(theme::SIDEBAR),
        ));
        for (name, width, root) in cases {
            let start = std::time::Instant::now();
            let viewport = Size::new(width, 520.0);
            let mut tree = UiTree::new(root);
            let initial = tree.layout_at(viewport, start);
            assert!(
                initial
                    .regions
                    .iter()
                    .any(|region| region.kind == openide_ui::HitKind::Scrollbar),
                "{name} must overflow for scroll review"
            );
            let point = Point::new(20.0, 300.0);
            tree.handle_event_at(UiEvent::PointerMove(point), start);
            tree.handle_event_at(
                UiEvent::Scroll {
                    position: point,
                    delta_y: -240.0,
                },
                start,
            );
            for index in 0..=9 {
                let at = start + std::time::Duration::from_millis(index * 16);
                let frame = tree.layout_at(viewport, at);
                save_frame(&frame, &format!("{name}-frame-{index:02}"), width);
            }
            let at = start + std::time::Duration::from_millis(160);
            let settled = tree.layout_at(viewport, at);
            let rail = settled
                .regions
                .iter()
                .find(|region| region.kind == openide_ui::HitKind::Scrollbar)
                .unwrap();
            let arrow = Point::new(
                rail.rect.x + rail.rect.width / 2.0,
                rail.rect.y + rail.rect.height - 7.0,
            );
            tree.handle_event_at(UiEvent::PointerMove(arrow), at);
            save_frame(
                &tree.layout_at(viewport, at + std::time::Duration::from_millis(120)),
                &format!("{name}-arrow-hover"),
                width,
            );
        }
    }

    #[test]
    #[ignore = "export native Settings paint commands for manual visual review"]
    fn export_settings_visual_review() {
        let output = std::path::PathBuf::from(
            std::env::var("OPENIDE_SETTINGS_VISUAL_QA_DIR").unwrap_or_else(|_| {
                format!(
                    "{}/../../target/settings-visual-qa",
                    env!("CARGO_MANIFEST_DIR")
                )
            }),
        );
        std::fs::create_dir_all(&output).unwrap();
        let mut state = empty_state();
        state.show_settings = true;
        state.native_provider_id = "openai".into();
        state.model = "gpt-4.1".into();
        state.catalog_browse_provider = "openai".into();
        state.catalog = Some(Arc::new(crate::model_catalog::reviewed_seed_snapshot()));
        state.native_output_limit_responses = true;
        state.native_output_limit_field = "max_output_tokens".into();
        let pages = [
            SettingsPage::General,
            SettingsPage::Appearance,
            SettingsPage::Agent,
            SettingsPage::Providers,
            SettingsPage::Browser,
            SettingsPage::ComputerUse,
            SettingsPage::Terminal,
            SettingsPage::WorkspaceSearch,
            SettingsPage::Memory,
            SettingsPage::Subagents,
            SettingsPage::ExternalAgents,
            SettingsPage::Plugins,
            SettingsPage::Mcp,
            SettingsPage::Skills,
            SettingsPage::Commands,
            SettingsPage::Automations,
            SettingsPage::Hooks,
            SettingsPage::Data,
            SettingsPage::Usage,
            SettingsPage::Github,
            SettingsPage::Shortcuts,
        ];
        for width in [360.0, 620.0, 1300.0] {
            for page in pages {
                state.settings_page = page;
                let mut tree = UiTree::new(
                    settings_ui::build_settings_page(&state, page, width, true)
                        .background(theme::BACKGROUND),
                );
                tree.set_reduced_motion(true);
                let frame = tree.layout(Size::new(width, 900.0));
                settings_ui::visual_qa::write_frame_svg(
                    &frame,
                    &output.join(format!("{page:?}-{width}.svg")),
                    width,
                    900.0,
                )
                .unwrap();
            }
        }
        state.settings_page = SettingsPage::Terminal;
        for (name, menu_open, editing, custom_saved) in [
            ("Terminal-menu", true, false, false),
            ("Terminal-custom-draft", false, true, false),
            ("Terminal-custom-saved-changing", false, true, true),
        ] {
            state.settings_terminal_shell_menu_open = menu_open;
            state.settings_terminal_shell_editing = editing;
            state.settings_terminal_shell = if custom_saved {
                TerminalShell::Custom(std::env::current_exe().unwrap().parent().unwrap().join("very-long-custom-shell-directory-for-a-workspace/another-directory-with-a-long-name/shell-executable"))
            } else {
                TerminalShell::System
            };
            state.settings_terminal_shell_draft = if custom_saved {
                std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            } else {
                String::new()
            };
            for width in [360.0, 1300.0] {
                let mut tree = UiTree::new(
                    settings_ui::build_settings_page(&state, SettingsPage::Terminal, width, true)
                        .background(theme::BACKGROUND),
                );
                tree.set_reduced_motion(true);
                settings_ui::visual_qa::write_frame_svg(
                    &tree.layout(Size::new(width, 900.0)),
                    &output.join(format!("{name}-{width}.svg")),
                    width,
                    900.0,
                )
                .unwrap();
            }
        }
        state.settings_terminal_shell_menu_open = false;
        state.settings_terminal_shell_editing = false;
        state.settings_page = SettingsPage::Providers;
        for width in [360.0, 620.0, 1300.0] {
            state.catalog_expanded_model = Some("gpt-4.1".into());
            let mut tree = UiTree::new(build_view(&state, width));
            tree.set_reduced_motion(true);
            settings_ui::visual_qa::write_frame_svg(
                &tree.layout(Size::new(width, 900.0)),
                &output.join(format!("Model-details-{width}.svg")),
                width,
                900.0,
            )
            .unwrap();
        }
        state.catalog_expanded_model = None;
        let mut tree = UiTree::new(build_view(&state, 1300.0));
        tree.set_reduced_motion(true);
        settings_ui::visual_qa::write_frame_svg(
            &tree.layout(Size::new(1300.0, 900.0)),
            &output.join("Providers-workspace.svg"),
            1300.0,
            900.0,
        )
        .unwrap();
        state.catalog_settings_tab = crate::provider_settings::CatalogSettingsTab::Connection;
        for width in [360.0, 1300.0] {
            let mut tree = UiTree::new(build_view(&state, width));
            tree.set_reduced_motion(true);
            settings_ui::visual_qa::write_frame_svg(
                &tree.layout(Size::new(width, 900.0)),
                &output.join(format!("Provider-connection-{width}.svg")),
                width,
                900.0,
            )
            .unwrap();
        }
    }

    #[test]
    fn mcp_settings_notes_wrap_without_covering_rows_or_clipping_copy() {
        let state = empty_state();
        for width in [360.0, 620.0, 1300.0] {
            let mut tree = UiTree::new(settings_ui::build_settings_page(
                &state,
                SettingsPage::Mcp,
                width,
                true,
            ));
            tree.set_reduced_motion(true);
            tree.set_sans_metrics(openide_ui::FontMetrics::default());
            let frame = tree.layout(Size::new(width, 900.0));
            let text_runs: Vec<_> = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text {
                        position,
                        text,
                        font_size,
                        ..
                    } => Some((position, text.as_str(), font_size)),
                    _ => None,
                })
                .collect();
            let connection_index = text_runs
                .iter()
                .position(|(_, text, _)| *text == "Conexión al ejecutar")
                .unwrap();
            let approval_index = text_runs
                .iter()
                .position(|(_, text, _)| *text == "Aprobación de herramientas")
                .unwrap();
            assert!(approval_index > connection_index + 1);
            for (position, _, font_size) in &text_runs[connection_index + 1..approval_index] {
                let line_height = openide_ui::FontMetrics::default().line_height * **font_size
                    / openide_ui::DEFAULT_FONT_SIZE;
                assert!(
                    position.y + line_height < text_runs[approval_index].0.y,
                    "MCP notes overlap at {width}px"
                );
            }
            let mut clips = Vec::new();
            let mut note_active = false;
            for command in &frame.commands {
                match command {
                    DrawCommand::PushClip(rect) => clips.push(*rect),
                    DrawCommand::PopClip => {
                        clips.pop();
                    }
                    DrawCommand::Text {
                        position,
                        text,
                        font_size,
                        ..
                    } => {
                        match text.as_str() {
                            "Sin servidores"
                            | "Conexión al ejecutar"
                            | "Aprobación de herramientas" => {
                                note_active = true;
                                continue;
                            }
                            "Uso" => note_active = false,
                            _ => {}
                        }
                        if !note_active {
                            continue;
                        }
                        let line_height = openide_ui::FontMetrics::default().line_height
                            * *font_size
                            / openide_ui::DEFAULT_FONT_SIZE;
                        for clip in &clips {
                            assert!(
                                position.y + line_height <= clip.y + clip.height + 0.01,
                                "MCP note clipped at {width}px: {}",
                                text
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn every_settings_page_keeps_controls_inside_small_and_large_viewports() {
        let mut state = empty_state();
        state.catalog = Some(Arc::new(crate::model_catalog::reviewed_seed_snapshot()));
        let pages = [
            SettingsPage::General,
            SettingsPage::Appearance,
            SettingsPage::Agent,
            SettingsPage::Providers,
            SettingsPage::Browser,
            SettingsPage::ComputerUse,
            SettingsPage::Terminal,
            SettingsPage::WorkspaceSearch,
            SettingsPage::Memory,
            SettingsPage::Subagents,
            SettingsPage::ExternalAgents,
            SettingsPage::Plugins,
            SettingsPage::Mcp,
            SettingsPage::Skills,
            SettingsPage::Commands,
            SettingsPage::Automations,
            SettingsPage::Hooks,
            SettingsPage::Data,
            SettingsPage::Usage,
            SettingsPage::Github,
            SettingsPage::Shortcuts,
        ];
        for width in [360.0, 620.0, 1300.0] {
            for page in pages {
                let mut tree = UiTree::new(settings_ui::build_settings_page(
                    &state,
                    page,
                    width,
                    width >= 760.0,
                ));
                for _ in 0..16 {
                    let frame = tree.layout(Size::new(width, 720.0));
                    for region in &frame.regions {
                        assert!(
                            region.rect.x >= -0.5
                                && region.rect.x + region.rect.width <= width + 0.5,
                            "{page:?} at {width}: {} {:?}",
                            region.id,
                            region.rect
                        );
                    }
                    tree.handle_event(UiEvent::Scroll {
                        position: Point::new(width * 0.65, 360.0),
                        delta_y: -200.0,
                    });
                }
            }
        }
    }

    #[test]
    fn catalog_virtual_list_reaches_last_exact_model_and_preserves_keyboard_actions() {
        let mut state = public_catalog_settings_fixture("");
        let mut catalog = state.catalog.as_deref().unwrap().clone();
        let provider = catalog.providers.get_mut("openrouter").unwrap();
        let template = provider.models.values().next().unwrap().clone();
        provider.models.clear();
        for index in 0..300 {
            let mut model = template.clone();
            model.id = format!("vendor/fixture-{index:03}:exact");
            model.name = format!("Fixture {index:03}");
            provider.models.insert(model.id.clone(), model);
        }
        state.catalog = Some(Arc::new(catalog));
        for width in [360.0, 620.0, 1300.0] {
            let mut tree = UiTree::new(settings_ui::build_settings_page(
                &state,
                SettingsPage::Providers,
                width,
                true,
            ));
            tree.set_reduced_motion(true);
            assert!(settings_action_reachable(
                &mut tree,
                "catalog_select:openrouter/vendor/fixture-000:exact",
                width,
                720.0
            ));
            tree.scroll_to_index("catalog_models:openrouter", 299);
            assert!(
                settings_action_reachable(
                    &mut tree,
                    "catalog_select:openrouter/vendor/fixture-299:exact",
                    width,
                    720.0
                ),
                "last model at {width}"
            );
            let frame = tree.layout(Size::new(width, 720.0));
            let visible = frame
                .regions
                .iter()
                .filter(|region| region.id.starts_with("catalog_select:"))
                .count();
            assert!(visible < 12, "only visible rows should be built");
            assert!(tree.focus("catalog_select:openrouter/vendor/fixture-299:exact"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "catalog_select:openrouter/vendor/fixture-299:exact".into()
                }]
            );
        }
    }

    fn public_catalog_settings_fixture(active_model: &str) -> ViewState {
        let mut state = empty_state();
        state.show_settings = true;
        state.settings_page = SettingsPage::Providers;
        state.selected_backend = "native".into();
        state.native_provider_id = "openrouter".into();
        state.model = active_model.into();
        state.catalog_browse_provider = "openrouter".into();
        state.native_provider_definitions =
            crate::provider_settings::connection_views_with_registry(
                &crate::native_provider::ProviderRegistry::builtin(),
                &crate::native_provider::NativeProviderSettings {
                    provider_id: "openrouter".into(),
                    model_id: active_model.into(),
                    ..Default::default()
                },
                |_| false,
            );
        // These renderer fixtures describe a configured connection and API
        // metadata. They neither authenticate nor verify external generation.
        state.provider_connections.insert(
            "openrouter".into(),
            crate::provider_connections::ConnectionSummary {
                configured: true,
                ..Default::default()
            },
        );
        state.catalog = Some(Arc::new(serde_json::from_value(json!({
            "version":3,"fetched_at_ms":1,
            "provider_status":{"openrouter":{"source":"official_api","attempted_at_ms":1,"verified_at_ms":1}},
            "providers":{"openrouter":{"id":"openrouter","name":"OpenRouter","models":{
                "vendor/exact-model:v1":{"id":"vendor/exact-model:v1","name":"Exact model fixture",
                    "tool_call":true,"tool_call_declared":true,
                    "modalities":{"input":["text"],"output":["text"]},
                    "limit":{"context":131072,"input":120000,"output":8192},
                    "provider_limits_declared":true,"capabilities_declared":true,
                    "declared_capabilities":{"image_input":false,"thinking":true,"structured_outputs":true},
                    "cost":{"input":1.25,"output":4.5,"cache_read":0.25}}}}}
        })).unwrap()));
        state
    }

    #[test]
    fn model_details_modal_keeps_exact_metadata_scroll_and_restores_focus() {
        let mut state = public_catalog_settings_fixture("");
        for width in [360.0, 620.0, 1300.0] {
            state.catalog_expanded_model = None;
            let mut tree = UiTree::new(build_view(&state, width));
            tree.set_reduced_motion(true);
            assert!(settings_action_reachable(
                &mut tree,
                "catalog_model_details:vendor/exact-model:v1",
                width,
                720.0
            ));
            assert!(tree.focus("catalog_model_details:vendor/exact-model:v1"));
            state.catalog_expanded_model = Some("vendor/exact-model:v1".into());
            tree.set_root(build_view(&state, width));
            let frame = tree.layout(Size::new(width, 720.0));
            let modal = frame
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:catalog_model_details_dismiss")
                .unwrap()
                .rect;
            assert!((modal.x + modal.width / 2.0 - width / 2.0).abs() < 1.0);
            assert!(modal.x >= 0.0 && modal.x + modal.width <= width);
            assert!(frame.commands.iter().any(
                |command| matches!(command, DrawCommand::Text { text, .. } if text == "vendor/exact-model:v1")
            ));
            assert!(settings_action_reachable(
                &mut tree,
                "provider_details_select",
                width,
                720.0
            ));
            let scroll_id = "modal_body:catalog_model_details_dismiss";
            let body = frame
                .regions
                .iter()
                .find(|region| region.id == scroll_id && region.kind == openide_ui::HitKind::Scroll)
                .unwrap()
                .rect;
            let mut observed = std::collections::HashSet::new();
            for _ in 0..12 {
                for command in tree.layout(Size::new(width, 720.0)).commands {
                    if let DrawCommand::Text { text, .. } = command {
                        observed.insert(text);
                    }
                }
                tree.handle_event(UiEvent::Scroll {
                    position: Point::new(body.x + body.width * 0.5, body.y + body.height * 0.5),
                    delta_y: -180.0,
                });
            }
            assert!(
                tree.scroll_offset(scroll_id) > 0.0,
                "metadata must scroll at {width}"
            );
            for exact in [
                "131072 tokens",
                "120000 tokens",
                "8192 tokens",
                "US$ 1.2500",
                "US$ 4.5000",
                "US$ 0.2500",
            ] {
                assert!(observed.contains(exact), "{exact} at {width}");
            }
            assert!(tree.focus("provider_details_select"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "catalog_select:openrouter/vendor/exact-model:v1".into()
                }]
            );
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Escape,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "catalog_model_details_dismiss".into()
                }]
            );
            state.catalog_expanded_model = None;
            tree.set_root(build_view(&state, width));
            tree.layout(Size::new(width, 720.0));
            assert_eq!(
                tree.focused_id(),
                Some("catalog_model_details:vendor/exact-model:v1")
            );
        }
    }
    #[test]
    fn chart_settings_render_account_and_task_data_at_multiple_widths() {
        use crate::github_contributions::{GithubContributionCalendar, GithubContributionDay};
        use chrono::Datelike;

        let mut state = empty_state();
        state.github_account =
            GithubAccountStatus::Connected(crate::github_account::GithubIdentity {
                login: "octocat".into(),
                avatar: None,
            });
        let first = chrono::NaiveDate::from_ymd_opt(2025, 9, 29).unwrap();
        let mut weeks = vec![[None; 7]];
        let mut total = 0;
        for offset in 0..366 {
            let date = first + chrono::Duration::days(offset);
            let row = date.weekday().num_days_from_sunday() as usize;
            if row == 0 {
                weeks.push([None; 7]);
            }
            let level = if offset > 280 && offset % 5 == 0 {
                (offset % 4 + 1) as u8
            } else {
                0
            };
            let count = u32::from(level);
            total += count;
            weeks.last_mut().unwrap()[row] = Some(GithubContributionDay { date, count, level });
        }
        state.github_contributions = GithubContributionsStatus::Ready(GithubContributionCalendar {
            login: "octocat".into(),
            total,
            weeks,
        });
        state.backend_options = vec![
            ("native".into(), "Agente Rust".into()),
            ("codex".into(), "Codex".into()),
            ("claude".into(), "Claude Code".into()),
        ];
        state.sessions = (0..15)
            .map(|index| SessionView {
                id: index.to_string(),
                title: "Tarea".into(),
                project: "/project".into(),
                backend: if index < 8 {
                    "native"
                } else if index < 13 {
                    "codex"
                } else {
                    "claude"
                }
                .into(),
                pinned: false,
                archived: false,
                unread: false,
                native_automation: None,
            })
            .collect();
        for width in [480.0, 1100.0] {
            for (page, name, prefix, observations) in [
                (SettingsPage::Github, "github", "github-contributions:", 366),
                (SettingsPage::Usage, "usage", "usage-tasks:", 3),
            ] {
                let content = settings_ui::build_settings_page(&state, page, width, true);
                let mut tree = UiTree::new(Element::panel(content).background(theme::BACKGROUND));
                tree.set_reduced_motion(true);
                let mut frame = tree.layout(Size::new(width, 760.0));
                // Account usage now leads the Usage page. The existing local
                // activity chart remains reachable through the page's scroll.
                for _ in 0..24 {
                    if frame
                        .regions
                        .iter()
                        .filter(|region| region.id.starts_with(prefix))
                        .count()
                        == observations
                    {
                        break;
                    }
                    tree.handle_event(UiEvent::Scroll {
                        position: Point::new(width / 2.0, 380.0),
                        delta_y: -100.0,
                    });
                    frame = tree.layout(Size::new(width, 760.0));
                }
                let regions: Vec<_> = frame
                    .regions
                    .iter()
                    .filter(|region| region.id.starts_with(prefix))
                    .collect();
                assert_eq!(regions.len(), observations, "{name} at {width}");
                assert!(regions.iter().all(|region| region.rect.x >= 0.0
                    && region.rect.x + region.rect.width <= width + 0.01));
                if let Some(directory) = std::env::var_os("OPENIDE_CHART_QA_DIR") {
                    let directory = std::path::PathBuf::from(directory);
                    std::fs::create_dir_all(&directory).unwrap();
                    let path = directory.join(format!("{name}-{width:.0}.svg"));
                    crate::settings_ui::visual_qa::write_frame_svg(&frame, &path, width, 760.0)
                        .unwrap();
                    let svg = std::fs::read_to_string(&path).unwrap();
                    let mut options = resvg::usvg::Options::default();
                    options.fontdb_mut().load_system_fonts();
                    let svg_tree = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
                    let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 760).unwrap();
                    resvg::render(
                        &svg_tree,
                        resvg::tiny_skia::Transform::identity(),
                        &mut pixmap.as_mut(),
                    );
                    pixmap.save_png(path.with_extension("png")).unwrap();
                }
            }
        }
        state.github_account = GithubAccountStatus::Disconnected;
        let frame = UiTree::new(settings_ui::build_settings_page(
            &state,
            SettingsPage::Github,
            1100.0,
            true,
        ))
        .layout(Size::new(1100.0, 760.0));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("github-contributions:")));
    }

    #[test]
    fn settings_search_filters_navigation_and_preserves_route_keyboard_focus() {
        let mut state = empty_state();
        state.settings_nav_query = "proveedores".into();
        let mut tree = UiTree::new(settings_ui::build_settings_sidebar(&state, 260.0));
        let frame = tree.layout(Size::new(260.0, 720.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "settings_search"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "settings_page:providers"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "settings_page:terminal"));
        assert!(tree.focus("settings_page:providers"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked {
                id: "settings_page:providers".into()
            }]
        );
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
            (SettingsPage::ExternalAgents, "CLIs externas"),
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
            state.settings_choice_menu_open = None;
            let mut host = UiTree::new(build_view(&state, width));
            assert!(settings_action_reachable(
                &mut host,
                "settings_choice_toggle:isolation",
                width,
                720.0,
            ));
            state.settings_choice_menu_open = Some("isolation".into());
            host.set_root(build_view(&state, width));
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
            assert_eq!(
                settings_action_reachable(
                    &mut host,
                    "command_isolation:workspace_write_offline",
                    width,
                    720.0,
                ),
                openide_harness::CommandIsolation::WorkspaceWriteOffline.is_available_on_host(),
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
        // Activating a saved server means it will connect during a native
        // turn. The badge must remain reachable without claiming a live link.
        for (enabled, label) in [(true, "Activado para turnos"), (false, "Desactivado")] {
            state.mcp_servers[0].enabled = enabled;
            for width in [520.0, 1100.0] {
                let mut tree = UiTree::new(build_view(&state, width));
                let visible = (0..30).any(|_| {
                    let frame = tree.layout(Size::new(width, 720.0));
                    if frame.commands.iter().any(|command| {
                        matches!(
                            command,
                            DrawCommand::Text { text, .. } if text == label
                        )
                    }) {
                        return true;
                    }
                    tree.handle_event(UiEvent::Scroll {
                        position: Point::new(width * 0.65, 360.0),
                        delta_y: -180.0,
                    });
                    false
                });
                assert!(visible, "MCP status {label} unreachable at {width}px");
            }
        }
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
                if reachable {
                    let frame = tree.layout(Size::new(width, 720.0));
                    let button = frame
                        .regions
                        .iter()
                        .find(|region| region.id == id)
                        .unwrap()
                        .rect;
                    assert!(
                        button.x >= 0.0 && button.x + button.width <= width,
                        "OAuth action is horizontally clipped: {id} at {width}px"
                    );
                    let point = Point::new(
                        button.x + button.width / 2.0,
                        button.y + button.height / 2.0,
                    );
                    tree.handle_event(UiEvent::PointerDown(point));
                    assert_eq!(
                        tree.handle_event(UiEvent::PointerUp(point)),
                        vec![UiAction::Clicked { id: id.to_owned() }],
                        "OAuth pointer action at {width}px"
                    );
                    assert!(tree.focus(id));
                    assert_eq!(
                        tree.handle_event(UiEvent::Key {
                            key: Key::Enter,
                            shift: false,
                            ctrl: false,
                        }),
                        vec![UiAction::Clicked { id: id.to_owned() }],
                        "OAuth keyboard action at {width}px"
                    );
                }
            }
            // The entire temporary URL wraps naturally and remains
            // selectable, including when the resource card needs scrolling.
            let url = "https://auth.example.test/authorize?state=transient";
            let mut tree = UiTree::new(build_view(&state, width));
            let mut selection = None;
            for _ in 0..30 {
                let frame = tree.layout(Size::new(width, 720.0));
                let mut rendered = String::new();
                let mut first = None;
                let mut last = None;
                for command in &frame.commands {
                    if let DrawCommand::Text {
                        position,
                        text,
                        font_family,
                        ..
                    } = command
                    {
                        if *font_family == FontFamily::Monospace
                            && !text.is_empty()
                            && url[rendered.len()..].starts_with(text)
                        {
                            rendered.push_str(text);
                            first.get_or_insert(*position);
                            last = Some(*position);
                            if rendered == url {
                                break;
                            }
                        }
                    }
                }
                if rendered == url {
                    selection = Some((first.unwrap(), last.unwrap()));
                    break;
                }
                tree.handle_event(UiEvent::Scroll {
                    position: Point::new(width * 0.65, 360.0),
                    delta_y: -180.0,
                });
            }
            let (first, last) = selection
                .unwrap_or_else(|| panic!("Full OAuth authorization URL unreachable at {width}px"));
            tree.handle_event(UiEvent::PointerDown(Point::new(first.x, first.y + 4.0)));
            tree.handle_event(UiEvent::PointerMove(Point::new(width - 16.0, last.y + 4.0)));
            tree.handle_event(UiEvent::PointerUp(Point::new(width - 16.0, last.y + 4.0)));
            assert_eq!(
                tree.selected_text(),
                Some(url),
                "OAuth URL selection at {width}px"
            );
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::C,
                    shift: false,
                    ctrl: true
                }),
                vec![UiAction::CopyRequested {
                    text: url.to_owned()
                }],
            );
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
        // Advance a simulated 120 Hz clock while preserving smooth scroll.
        // Reaching a route must not depend on CPU time spent laying out frames.
        let start = std::time::Instant::now();
        let mut tick = 0_u32;
        for page in [
            SettingsPage::General,
            SettingsPage::Appearance,
            SettingsPage::Providers,
            SettingsPage::Browser,
            SettingsPage::Terminal,
            SettingsPage::Shortcuts,
            SettingsPage::Agent,
            SettingsPage::Memory,
            SettingsPage::Subagents,
            SettingsPage::ExternalAgents,
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
            for _ in 0..120 {
                let now = start + std::time::Duration::from_secs_f64(f64::from(tick) / 120.0);
                tick += 1;
                let frame = tree.layout_at(Size::new(width, height), now);
                if frame
                    .regions
                    .iter()
                    .any(|region| region.id == page.action_id())
                {
                    visible = true;
                    break;
                }
                tree.handle_event_at(
                    UiEvent::Scroll {
                        position: Point::new(sidebar_x, 430.0),
                        delta_y: -120.0,
                    },
                    now,
                );
            }
            assert!(visible, "sidebar could not reach {page:?}");
        }
    }

    #[test]
    fn settings_shows_save_and_error_notices_as_a_card_over_the_page() {
        let mut state = empty_state();
        state.show_settings = true;
        for (message, is_error) in [
            ("Proyecto actualizado", false),
            ("La carpeta del proyecto no existe", true),
        ] {
            state.settings_notice = Some(message.into());
            state.settings_notice_error = is_error;
            let mut tree = UiTree::new(build_view(&state, 1100.0));
            // The notice is checked settled, not partway through its entrance.
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(1100.0, 720.0));
            let ink = frame.commands.iter().find_map(|command| match command {
                DrawCommand::Text { text, color, position, .. } if text == message => {
                    Some((*color, *position))
                }
                _ => None,
            });
            let (ink, position) = ink.expect("the notice is on screen");
            assert!(position.x > 600.0 && position.y > 500.0, "bottom right: {position:?}");
            let error = openide_ui::AlertColors::for_tone(
                openide_ui::AlertTone::Error,
                theme::menu_palette(),
            )
            .accent;
            assert_eq!(ink == error, is_error);
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "settings_notice_dismiss"));
        }
    }

    #[test]
    fn output_limit_controls_are_reachable_and_require_an_explicit_chat_parameter() {
        for width in [360.0, 620.0, 1300.0] {
            let mut state = public_catalog_settings_fixture("vendor/exact-model:v1");
            state.native_output_limit_draft = "4096".into();
            for parameter in ["max_tokens", "max_completion_tokens"] {
                state.native_output_limit_field = parameter.into();
                for id in [
                    format!("native_output_limit_field:{parameter}"),
                    "native_output_limit_save".into(),
                    "native_output_limit_reset".into(),
                    "native_output_limit_tokens".into(),
                ] {
                    let mut tree = UiTree::new(build_view(&state, width));
                    tree.set_reduced_motion(true);
                    assert!(
                        settings_action_reachable(&mut tree, &id, width, 720.0),
                        "{id} at {width}px"
                    );
                }
            }
            state.native_output_limit_field.clear();
            let mut tree = UiTree::new(build_view(&state, width));
            tree.set_reduced_motion(true);
            assert!(!settings_action_reachable(
                &mut tree,
                "native_output_limit_save",
                width,
                720.0
            ));
            // OpenCode's registered exact gpt-5.4 route uses Responses. Codex
            // account-managed output controls are intentionally not editable.
            let responses = crate::native_provider::adapter("opencode")
                .unwrap()
                .for_model("gpt-5.4")
                .unwrap();
            assert_eq!(
                responses.wire_api,
                crate::native_provider::NativeWireApi::Responses
            );
            state.native_provider_id = "opencode".into();
            state.catalog_browse_provider = "opencode".into();
            state.model = "gpt-5.4".into();
            state
                .native_provider_definitions
                .get_mut("opencode")
                .unwrap()
                .wire_api = responses.wire_api;
            state.provider_connections.insert(
                "opencode".into(),
                crate::provider_connections::ConnectionSummary {
                    configured: true,
                    service: "zen".into(),
                    ..Default::default()
                },
            );
            state.native_output_limit_responses = true;
            state.native_output_limit_field = "max_output_tokens".into();
            let mut tree = UiTree::new(build_view(&state, width));
            tree.set_reduced_motion(true);
            assert!(settings_action_reachable(
                &mut tree,
                "native_output_limit_save",
                width,
                720.0
            ));
            state.running = true;
            let mut tree = UiTree::new(build_view(&state, width));
            tree.set_reduced_motion(true);
            assert!(!settings_action_reachable(
                &mut tree,
                "native_output_limit_save",
                width,
                720.0
            ));
        }
    }

    #[test]
    fn model_catalog_public_navigation_and_model_rows_fit_each_settings_width() {
        let mut state = public_catalog_settings_fixture("");
        for viewport in [360.0, 620.0, 1300.0] {
            state.catalog_browse_provider.clear();
            let mut provider_tree = UiTree::new(build_view(&state, viewport));
            provider_tree.set_reduced_motion(true);
            assert!(settings_action_reachable(
                &mut provider_tree,
                "catalog_browse:openrouter",
                viewport,
                720.0
            ));
            let overview = provider_tree.layout(Size::new(viewport, 720.0));
            for region in &overview.regions {
                assert!(
                    region.rect.x >= -0.5 && region.rect.x + region.rect.width <= viewport + 0.5,
                    "{} at {viewport}",
                    region.id
                );
            }
            assert!(provider_tree.focus("catalog_browse:openrouter"));
            assert_eq!(
                provider_tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "catalog_browse:openrouter".into()
                }]
            );
            state.catalog_browse_provider = "openrouter".into();
            let mut tree = UiTree::new(build_view(&state, viewport));
            tree.set_reduced_motion(true);
            assert!(
                settings_action_reachable(
                    &mut tree,
                    "catalog_select:openrouter/vendor/exact-model:v1",
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
                .find(|region| region.id == "catalog_select:openrouter/vendor/exact-model:v1")
                .unwrap()
                .rect;
            // List labels use TruncatedText, which paints a single measured
            // line without a selectable TextLayoutBlock. Its details action
            // still carries the complete ID, and the modal must display it.
            let name = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Text {
                        text,
                        position,
                        font_size,
                        font_family,
                        ..
                    } if *font_family == FontFamily::Monospace
                        && "vendor/exact-model:v1".starts_with(text.trim_end_matches('…'))
                        && text.starts_with("vendor/") =>
                    {
                        assert!(text == "vendor/exact-model:v1" || text.ends_with('…'));
                        let metrics = openide_ui::FontMetrics::default();
                        let scale = *font_size / openide_ui::DEFAULT_FONT_SIZE;
                        Some(openide_ui::Rect::new(
                            position.x,
                            position.y,
                            text.chars().count() as f32 * metrics.char_width * scale,
                            metrics.line_height * scale,
                        ))
                    }
                    _ => None,
                })
                .expect("the exact model ID or its measured prefix remains visible");
            assert!(model.x >= 0.0 && model.x + model.width <= viewport);
            if viewport >= 620.0 {
                assert!(
                    model.x >= name.x + name.width - 0.5,
                    "horizontal row at {viewport}: {name:?}, {model:?}"
                );
            } else {
                assert!(
                    model.y >= name.y + name.height - 0.5,
                    "stacked row at {viewport}: {name:?}, {model:?}"
                );
            }
            assert!(tree.focus("catalog_model_details:vendor/exact-model:v1"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "catalog_model_details:vendor/exact-model:v1".into()
                }]
            );
            state.catalog_expanded_model = Some("vendor/exact-model:v1".into());
            tree.set_root(build_view(&state, viewport));
            assert!(
                tree.layout(Size::new(viewport, 720.0))
                    .text_layouts()
                    .iter()
                    .any(|block| block.source == "vendor/exact-model:v1"),
                "the full exact model ID must be accessible at {viewport}px"
            );
            state.catalog_expanded_model = None;
            tree.set_root(build_view(&state, viewport));
            // Return through the provider page's actual scroll surface. The
            // forward reachability helper only scrolls down.
            for _ in 0..30 {
                if tree
                    .layout(Size::new(viewport, 720.0))
                    .regions
                    .iter()
                    .any(|region| region.id == "catalog_overview")
                {
                    break;
                }
                tree.handle_event(UiEvent::Scroll {
                    position: Point::new(viewport * 0.65, 360.0),
                    delta_y: 180.0,
                });
            }
            assert!(tree
                .layout(Size::new(viewport, 720.0))
                .regions
                .iter()
                .any(|region| region.id == "catalog_overview"));
            assert!(tree.focus("catalog_overview"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "catalog_overview".into()
                }]
            );
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
                if *radius == 12.0 && *color == theme::BORDER
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
                native_automation: None,
            },
            SessionView {
                id: "b".into(),
                title: "Sin grupo".into(),
                project: "C:/project".into(),
                backend: "native".into(),
                pinned: false,
                archived: false,
                unread: false,
                native_automation: None,
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
                native_automation: None,
            },
            SessionView {
                id: "two".into(),
                title: "Segunda tarea".into(),
                project: "C:/work/alpha".into(),
                backend: "native".into(),
                pinned: false,
                archived: false,
                unread: false,
                native_automation: None,
            },
            SessionView {
                id: "three".into(),
                title: "Otra tarea".into(),
                project: "D:/work/beta".into(),
                backend: "codex".into(),
                pinned: false,
                archived: false,
                unread: false,
                native_automation: None,
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
                native_automation: None,
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
        assert!(tree.scroll_offset("sessions_list") > 900.0 * SIDEBAR_ROW_HEIGHT);
        assert!(visible_count(&last) < 20);
    }

    #[test]
    fn session_row_keeps_unicode_title_status_and_menu_in_separate_bounds() {
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
            native_automation: None,
        };
        for width in [176.0, 264.0] {
            let frame = UiTree::new(session_row(&state, &session, width))
                .layout(Size::new(width - 18.0, 28.0));
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
            assert_eq!(action.rect.height, 28.0);
            assert_eq!(menu.rect.height, 24.0);
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
            assert!(!labels.contains(&"Codex"), "{labels:?}");
            assert!(!labels.contains(&"Nativo"), "{labels:?}");
            assert!(labels.contains(&session.title.as_str()), "{labels:?}");
            assert!(
                labels.iter().all(|title| !title.ends_with('…')),
                "{labels:?}"
            );
            assert!(frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::Text { text, color, .. }
                    if text == &session.title && color.a < theme::BRAND.a
            )));
            assert!(frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::FillRect { rect, color, .. }
                    if rect.width == 6.0 && rect.height == 6.0 && *color == theme::BRAND
            )));
        }
        let mut tree = UiTree::new(session_row(&state, &session, 264.0));
        tree.layout(Size::new(246.0, 28.0));
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
    fn running_session_clock_has_inset_and_keeps_title_clear() {
        let mut state = empty_state();
        state.selected_session = Some("running".into());
        state.running = true;
        let session = SessionView {
            id: "running".into(),
            title: "Investigá la terminal y el entorno del proyecto con un título largo".into(),
            project: "/project".into(),
            backend: "native".into(),
            pinned: false,
            archived: false,
            unread: false,
            native_automation: None,
        };
        for width in [176.0, 264.0] {
            let mut tree = UiTree::new(session_row(&state, &session, width));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(width - 18.0, 28.0));
            tree.handle_event(UiEvent::PointerMove(Point::new(40.0, 16.0)));
            let frame = tree.layout(Size::new(width - 18.0, 28.0));
            let clock = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Icon {
                        rect,
                        icon: Icon::Clock,
                        ..
                    } => Some(*rect),
                    _ => None,
                })
                .unwrap();
            assert_eq!((clock.width, clock.height), (16.0, 16.0));
            assert!(clock.x >= 4.0 && clock.y >= 6.0, "{clock:?}");
            let menu = frame
                .regions
                .iter()
                .find(|region| region.id == "session_menu:running")
                .unwrap()
                .rect;
            for command in &frame.commands {
                if let DrawCommand::Text { position, text, .. } = command {
                    if text == &session.title {
                        assert!(position.x >= clock.x + clock.width + 4.0);
                        assert!(position.x < menu.x);
                    }
                }
            }
            assert!(tree.focus("session_menu:running"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "session_menu:running".into()
                }]
            );
        }
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
            native_automation: None,
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
            native_automation: None,
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
            full_path: None,
            id: 3,
            title: "Archivos".into(),
            active: true,
            kind: SideTab::Files,
            closable: false,
        });
        state.dock_tabs.push(DockTabView {
            full_path: None,
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
        let review = crate::approval_ui::ApprovalTarget {
            owner_session_id: state.selected_session.clone().unwrap_or_default(),
            workspace: state.workspace.clone(),
            child_session_id: None,
            request_id: "req-1".into(),
        }
        .review_action();
        for id in [
            "file_row:0",
            "dock_tab:3",
            format!("{review}:inline").as_str(),
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
            .any(|region| region.id == format!("{review}:inline")));
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
        let review = crate::approval_ui::ApprovalTarget {
            owner_session_id: "parent-1".into(),
            workspace: state.workspace.clone(),
            child_session_id: Some("child-1".into()),
            request_id: "request-1".into(),
        }
        .review_action();
        for id in [
            format!("{review}:inline").as_str(),
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
        let row_height = frame
            .regions
            .iter()
            .find(|region| region.id == "file_row:1")
            .expect("nested row is visible")
            .rect
            .height;
        assert_eq!(row_height, 24.0);
        assert!(strokes
            .iter()
            .any(|rect| rect.width == 1.0 && rect.height == row_height));
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
            .any(|rect| rect.width == 1.0 && rect.height == row_height && rect.y == file_row_y));
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
            .layout(Size::new(246.0, 28.0));
        let action = frame
            .regions
            .iter()
            .find(|region| region.id == "session_project_toggle:C:/work/openIDE")
            .unwrap();
        assert_eq!(action.rect.width, 246.0);
        assert_eq!(action.rect.height, 28.0);
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "openIDE"
        )));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "3"
        )));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Icon { icon, rect, .. } if *icon == openide_ui::folder_icon("C:/work/openIDE", true) && rect.y == 6.0 && rect.width == 16.0
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
                native_automation: None,
            },
            SessionView {
                id: "active".into(),
                title: "Activa".into(),
                project: "C:/work/alpha".into(),
                backend: "codex".into(),
                pinned: false,
                archived: false,
                unread: false,
                native_automation: None,
            },
            SessionView {
                id: "archived".into(),
                title: "Archivada".into(),
                project: "D:/work/beta".into(),
                backend: "native".into(),
                pinned: false,
                archived: true,
                unread: false,
                native_automation: None,
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
        assert_eq!(count(&active, "session_delete:active"), 1);
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
            native_automation: None,
        }];
        state.session_menu_id = Some("interrupted".into());
        for recovery in [
            SessionRecoveryView::StartAutomation,
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
                native_automation: None,
            },
            SessionView {
                id: "second".into(),
                title: "Segunda tarea".into(),
                project: "C:/project".into(),
                backend: "native".into(),
                pinned: false,
                archived: false,
                unread: false,
                native_automation: None,
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
        state.selected_backend = "native".into();
        state.native_provider_id = "openai".into();
        state.composer_model = "gpt-6-sol".into();
        state.composer_routes = vec![ModelRouteOption {
            route_id: "native:openai:gpt-6-sol".into(),
            backend: "native".into(),
            provider_id: "openai".into(),
            provider_name: "OpenAI".into(),
            model_id: "gpt-6-sol".into(),
            model_name: "GPT-6 Sol".into(),
            credential: crate::composer_selection::CredentialAvailability::EnvPresentUnverified,
            execution: crate::composer_selection::ExecutionSupport::Verified,
            source: crate::composer_selection::RouteSource::AdapterReview,
            catalog_present: true,
            provider_listed: Some(true),
            effort_options: Vec::new(),
        }];
        state.composer_model_menu_open = true;
        state.composer_model_list_open = true;
        let open = UiTree::new(build_view(&state, 1000.0)).layout(Size::new(1000.0, 700.0));
        let option = open
            .regions
            .iter()
            .find(|region| region.id == "composer_route:native:openai:gpt-6-sol")
            .unwrap();
        assert!(option.rect.x > theme::SIDEBAR_WIDTH);
        assert!(open
            .regions
            .iter()
            .any(|region| region.id == "composer_model_dismiss"));
    }

    #[test]
    fn rename_and_delete_require_explicit_commit() {
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
            native_automation: None,
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
        for archived in [false, true] {
            state.sessions[0].archived = archived;
            state.show_archived = archived;
            let delete = UiTree::new(build_view(&state, 900.0)).layout(Size::new(900.0, 620.0));
            for id in ["delete_confirm:old", "delete_cancel"] {
                assert!(delete.regions.iter().any(|region| region.id == id));
            }
            assert!(!delete
                .regions
                .iter()
                .any(|region| region.id == "session_delete:old"));
            assert_eq!(delete.hit_test(Point::new(4.0, 4.0)), Some("delete_cancel"));
            let text = delete
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .flat_map(str::split_whitespace)
                .collect::<Vec<_>>()
                .join(" ");
            assert!(text.contains("¿Eliminar conversación?"));
            assert!(text.contains("«Vieja tarea»"));
            assert!(text.contains("Los archivos del proyecto se conservarán."));
        }
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
            native_automation: None,
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
        state.prompt = "Una tarea con instrucciones".into();
        let single =
            UiTree::new(build_view_sized(&state, 520.0, 320.0)).layout(Size::new(520.0, 320.0));
        let single_height = single
            .regions
            .iter()
            .find(|region| region.id == "prompt")
            .unwrap()
            .rect
            .height;
        state.prompt = "Una tarea con instrucciones\nen varias líneas".into();
        let frame =
            UiTree::new(build_view_sized(&state, 520.0, 320.0)).layout(Size::new(520.0, 320.0));
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
        assert!(prompt.rect.height > single_height);
        assert!(submit.rect.y > prompt.rect.y + prompt.rect.height);
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
            activity: crate::transcript::ActivityStatus::Completed,
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
                >= 1
        );
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "conversation_detail:draft:tool:call-1"));
    }

    #[test]
    fn activity_inspector_tracks_only_the_selected_commands_partial_output() {
        let mut state = empty_state();
        state.selected_tool_id = Some("running-call".into());
        state.transcript = vec![
            TranscriptItem::Tool {
                id: "running-call".into(),
                name: "run_command".into(),
                arguments: json!({"command":"cargo test"}),
                output: None,
                is_error: false,
                activity: crate::transcript::ActivityStatus::Running,
            },
            TranscriptItem::ToolOutput {
                tool_call_id: "running-call".into(),
                text: "PARTIAL_PROCESS_OUTPUT".into(),
                truncated: true,
            },
        ];
        let frame = UiTree::new(activity_content(&state)).layout(Size::new(600.0, 700.0));
        let visible = frame
            .text_layouts()
            .iter()
            .map(|layout| layout.source.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(visible.contains("PARTIAL_PROCESS_OUTPUT"));
        assert!(visible.contains("SALIDA EN CURSO"));
        assert!(visible.contains("contenido fue omitida"));
        if let TranscriptItem::ToolOutput { tool_call_id, .. } = &mut state.transcript[1] {
            *tool_call_id = "another-call".into();
        }
        let frame = UiTree::new(activity_content(&state)).layout(Size::new(600.0, 700.0));
        let visible = frame
            .text_layouts()
            .iter()
            .map(|layout| layout.source.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!visible.contains("PARTIAL_PROCESS_OUTPUT"));
    }

    #[test]
    fn native_command_activity_shows_state_and_one_control_and_recovery_entry() {
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
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "native_command_terminate:running-1"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "native_command_terminate:finished-1"));
        for command in ["running-1", "finished-1"] {
            let id = format!(
                "native_command_control_open:{}",
                serde_json::to_string(&("native-1", command)).unwrap()
            );
            assert_eq!(
                frame
                    .regions
                    .iter()
                    .filter(|region| region.id == id)
                    .count(),
                1
            );
        }
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "native_command_operation_history"));
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
        assert!(!confirm
            .regions
            .iter()
            .any(|region| region.id == "native_command_terminate_confirm:running-1"));
        assert!(!confirm
            .regions
            .iter()
            .any(|region| region.id == "native_command_terminate_cancel"));
        state.native_commands.clear();
        let no_manager = UiTree::new(build_view(&state, 1200.0)).layout(Size::new(1200.0, 720.0));
        assert!(no_manager
            .regions
            .iter()
            .any(|region| region.id == "native_command_operation_history"));
    }

    #[test]
    fn narrow_timeline_expands_only_explicitly_opened_tool() {
        let mut state = empty_state();
        for id in ["one", "two"] {
            state.transcript.push(TranscriptItem::Tool {
                id: id.into(),
                name: "read_file".into(),
                arguments: json!({"path": id}),
                output: Some(format!("result-{id}")),
                is_error: false,
                activity: crate::transcript::ActivityStatus::Completed,
            });
        }
        state.selected_tool_id = Some("one".into());
        state.conversation_details.toggle("tool:two", false);
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
        assert!(!frame
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
            "loading placeholders should pulse"
        );
        let result_viewport = loading_frame
            .regions
            .iter()
            .find(|region| {
                region.id == "command_results" && region.kind == openide_ui::HitKind::Scroll
            })
            .expect("search result viewport")
            .rect;
        assert!(!loading_frame
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::Spinner { .. })));
        let placeholders: Vec<_> = loading_frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRect { rect, radius, .. }
                    if *radius == 5.0 && rect.height == 10.0 =>
                {
                    Some(*rect)
                }
                _ => None,
            })
            .collect();
        assert!(placeholders.len() >= 3, "{placeholders:?}");
        for (index, row) in placeholders.iter().enumerate() {
            assert!(row.x >= result_viewport.x && row.x + row.width <= result_viewport.x + result_viewport.width);
            assert!(row.y >= result_viewport.y && row.y + row.height <= result_viewport.y + result_viewport.height);
            assert!((row.y - placeholders[0].y - index as f32 * 30.0).abs() < 0.5);
        }
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
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, font_family: FontFamily::Sans, .. } if text == "Esc"
        )));
        assert!(!frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon {
                icon: Icon::CloseCircle | Icon::CloseBold,
                ..
            }
        )));
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
