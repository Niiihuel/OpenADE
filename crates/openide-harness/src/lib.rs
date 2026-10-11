//! Native agent loop with replaceable model and tool boundaries.

mod anthropic;
mod backend;
mod checkpoint_coordinator;
mod command;
mod command_elevation;
mod command_line;
pub use command_line::{definition_before_command_revision, split_command_arguments};
mod compact;
mod context;
mod error;
mod limit_gate;
mod provider_failure;
mod exec_policy;
mod goal_tools;
mod hooks;
mod manual_command;
mod compat_hooks;
mod output_condense;
mod subagent_roles;
mod mcp;
mod mcp_http;
mod mcp_identity;
mod mcp_oauth;
#[cfg(windows)]
mod mcp_oauth_windows;
mod mcp_tools;
mod model_stream;
mod native_command_registry;
mod native_context;
mod permission;
mod persistent_command;
mod plan_tool;
mod user_interaction;
#[path = "mcp_process_tree.rs"]
mod process_tree;
mod project_instructions;
mod registry;
mod remote_control;
mod responses;
mod resume;
mod runner;
mod search;
mod skills;
mod subagent_tools;
mod subagent_worker;
mod subagents;
mod tool_media;
mod tool_output;
mod tool_preparation;
mod tool_scheduler;
mod tools;

pub use anthropic::{AnthropicMessagesBackend, DEFAULT_ANTHROPIC_MAX_OUTPUT_TOKENS};
pub use backend::{
    set_model_retry_sleep_divisor,
    ModelBackend, ModelCompletion, ModelOutput, ModelRequestCapabilities, ModelTokenUsage,
    OpenAiCompatibleBackend,
};
pub use checkpoint_coordinator::{
    CheckpointCommit, CheckpointCoordinator, CheckpointCoordinatorError,
};
pub use command::CommandIsolation;
pub use command_elevation::{
    linux_administrator_commands_available, run_elevated_command_helper_from_env,
};
pub use compact::{
    CommittedMessage, CompactFileState, CompactLimits, CompactRequest, CompactService,
    CompactSummary, MAX_COMPACT_INPUT_BYTES, MAX_COMPACT_MESSAGES,
};
pub use context::{
    ContextPolicy, TokenBudgetAssessment, TokenBudgetDecision, TokenBudgetPolicy, TokenBudgetState,
};
pub use error::AgentError;
pub use limit_gate::{limit_gate_block, limit_gate_check, limit_gate_clear, LimitBlock};
pub use provider_failure::{FailureClass, ProviderFailure};
pub use exec_policy::{
    ExecDecision, ExecEvaluation, ExecPatternToken, ExecPolicy, ExecPolicyError, ExecRule,
    MAX_EXEC_JUSTIFICATION_BYTES, MAX_EXEC_PATTERN_TOKENS, MAX_EXEC_RULES, MAX_EXEC_TOKEN_BYTES,
};
pub use goal_tools::NativeGoalToolExecutor;
pub use compat_hooks::{
    claude_tool, native_input, run_compat_hook, CompatEvent, CompatHookConfig, EventOutcome,
};
pub use hooks::{
    run_command_hook, run_manual_command_hook, CommandHookConfig, CommandHooks, HookDecision,
    HookFailure, HookInvocation, HookPhase, HookResponse, HookRunResult, HookValidationError,
    ManualCommandHookInvocation, MAX_HOOK_INPUT_BYTES, MAX_HOOK_OUTPUT_BYTES, MAX_HOOK_TIMEOUT_MS,
};
pub use manual_command::{
    NativeManualCommandActor, NativeManualCommandObserver, NativeManualCommandPolicy,
    NativeManualCommandPolicySource,
};
pub use mcp::{
    McpCallResult, McpError, McpServerInfo, McpStdioClient, McpStdioConfig, McpTool,
    MAX_MCP_MESSAGE_BYTES, MAX_MCP_TOOLS, MAX_MCP_TOOL_PAGES, MCP_PROTOCOL_VERSION,
};
pub use mcp_http::{McpHttpClient, McpHttpConfig, MCP_HTTP_PROTOCOL_VERSION};
pub use mcp_identity::{
    inspect_mcp_stdio_launch, mcp_runtime_digest, mcp_stdio_runtime_digest, McpHttpServerIdentity,
    McpServerRuntimeIdentity, McpStdioLaunchIdentity, McpStdioServerIdentity,
};
pub use mcp_oauth::{
    MemoryTokenStore, OAuthConfig, OAuthError, OAuthRegistration, OAuthSession, OAuthTokens,
    PendingAuthorization, TokenKey, TokenStore,
};
#[cfg(windows)]
pub use mcp_oauth_windows::WindowsCredentialTokenStore;
pub use mcp_tools::{McpCompositeTools, McpServerBinding, McpToolAdapterError, McpToolClient};
pub use model_stream::{
    model_event_channel, ModelEventSender, MAX_MODEL_EVENT_BYTES, MODEL_EVENT_CHANNEL_CAPACITY,
};
pub use native_command_registry::{
    NativeCommandProfile, NativeCommandProfileOptions, NativeCommandRegistry,
    PersistentCommandToolExecutor,
};
pub use native_context::{
    native_model_context_digest, native_model_context_digest_for_recorded_workspace,
};
pub use permission::{
    PermissionChoice, PermissionMode, PermissionPolicy, SessionApprovalError, SessionApprovalGrant,
    SESSION_APPROVAL_GRANT_VERSION,
};
pub use persistent_command::{
    PersistentCommandControlInspection, PersistentCommandDimensions, PersistentCommandEvent,
    PersistentCommandExit, PersistentCommandInspection, PersistentCommandLimits,
    PersistentTerminal, PersistentTerminalEvent,
    PersistentCommandManager, PersistentCommandSessionInfo, PersistentCommandSnapshot,
    PersistentCommandStarted, PersistentCommandStdinState, PersistentCommandStream,
};
pub use plan_tool::update_plan_tool_definition;
pub use user_interaction::{
    ask_user_tool_definition, catalogs_with_user_interaction, is_user_interaction_tool,
    switch_mode_accepted, switch_mode_tool_definition, ASK_USER_TOOL, SWITCH_MODE_TOOL,
};
pub use project_instructions::{
    load_project_instructions, load_project_instructions_for_target,
    project_instruction_metadata_sequences, LoadedProjectInstructions, ProjectInstructionEntry,
    ProjectInstructionLimits, ProjectInstructionsError, AGENTS_MD_FILENAME,
    AGENTS_OVERRIDE_FILENAME, DEFAULT_PROJECT_DOC_MAX_BYTES, DEFAULT_PROJECT_DOC_MAX_LEVELS,
};
pub use registry::{
    ToolCatalog, ToolCatalogError, MAX_ADVERTISED_TOOLS, MAX_CATALOG_SCHEMA_BYTES,
    MAX_TOOL_NAME_BYTES, MAX_TOOL_SCHEMA_BYTES,
};
pub use remote_control::{
    native_remote_owner, request_native_remote_cancel, NativeRemoteControlGuard, NativeRemoteOwner,
};
pub use responses::{
    hosted_web_citations, hosted_web_search_calls, hosted_web_search_presentation,
    HostedWebCitation, HostedWebSearchAction, HostedWebSearchCall, HostedWebSearchConfig,
    HostedWebSearchContextSize, HostedWebSearchLocation, HostedWebSearchMode, HostedWebSource,
    ResponseUsage, ResponsesBackend, ResponsesResult,
};
pub use resume::{cancel_paused_turn, 
    discard_aborted_tool_batch, record_verified_tool_outcome, ResumeDisposition, ResumeEngine,
    ResumeError, ResumeEventObserver, ResumeStep,
};
pub use runner::{AgentRunHandle, AgentRunner, RunLimits};
pub use skills::{
    activate_skills, discover_skills, ActivatedSkills, DiscoveredSkill, SkillActivationError,
    SkillDiscoveryError, SkillOrigin, SkillProvenance, SkillRoot, SkillSelection,
    MAX_ACTIVE_SKILLS, MAX_ACTIVE_SKILL_INSTRUCTIONS_BYTES, MAX_DISCOVERED_SKILLS,
    MAX_SKILL_DOCUMENT_BYTES, MAX_SKILL_ENTRIES_PER_ROOT, MAX_SKILL_ROOTS,
};
pub use subagent_tools::{SubagentToolExecutor, SubagentToolLimits};
pub use subagent_worker::{
    finalize_resolved_subagent, SubagentBridgeEvent, SubagentEventBridge, SubagentRecoveryPause,
    SubagentRecoveryState, SubagentTurnResult, SubagentWorker, SubagentWorkerError,
};
pub use subagents::{
    SubagentControl, SubagentCoordinator, SubagentError, SubagentInputSnapshot,
    SubagentInputStatus, SubagentLimits, SubagentOutcome, SubagentRecord, SubagentRunLease,
    SubagentSpec, SubagentStatus, SubagentUiSnapshot,
};
pub use tool_output::{ToolOutputSender, MAX_TOOL_STREAM_BYTES, MAX_TOOL_STREAM_EVENTS};
pub use tool_preparation::{PreparedToolSession, ToolPreparation};
pub use tools::{
    LocalTool, LocalTools, ToolContext, ToolDefinition, ToolExecutor, ToolOutput, ToolPermissions,
};

mod goal_context;
pub use goal_context::{native_goal_base_instructions, native_goal_system_instructions};
