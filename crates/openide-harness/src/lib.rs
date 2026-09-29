//! Native agent loop with replaceable model and tool boundaries.

mod backend;
mod checkpoint_coordinator;
mod command;
mod compact;
mod context;
mod error;
mod exec_policy;
mod hooks;
mod mcp;
mod mcp_http;
mod mcp_identity;
mod mcp_oauth;
#[cfg(windows)]
mod mcp_oauth_windows;
mod mcp_tools;
mod native_context;
mod permission;
mod persistent_command;
mod plan_tool;
#[path = "mcp_process_tree.rs"]
mod process_tree;
mod project_instructions;
mod registry;
mod responses;
mod resume;
mod runner;
mod search;
mod skills;
mod subagent_tools;
mod subagent_worker;
mod subagents;
mod tool_preparation;
mod tool_scheduler;
mod tools;

pub use backend::{
    native_agent_supported_efforts, native_agent_supports_tool_calls, ModelBackend,
    ModelCompletion, ModelOutput, ModelTokenUsage, OpenAiCompatibleBackend,
};
pub use checkpoint_coordinator::{
    CheckpointCommit, CheckpointCoordinator, CheckpointCoordinatorError,
};
pub use command::CommandIsolation;
pub use compact::{
    CommittedMessage, CompactFileState, CompactLimits, CompactRequest, CompactService,
    CompactSummary, MAX_COMPACT_INPUT_BYTES, MAX_COMPACT_MESSAGES,
};
pub use context::{
    ContextPolicy, TokenBudgetAssessment, TokenBudgetDecision, TokenBudgetPolicy, TokenBudgetState,
};
pub use error::AgentError;
pub use exec_policy::{
    ExecDecision, ExecEvaluation, ExecPatternToken, ExecPolicy, ExecPolicyError, ExecRule,
    MAX_EXEC_JUSTIFICATION_BYTES, MAX_EXEC_PATTERN_TOKENS, MAX_EXEC_RULES, MAX_EXEC_TOKEN_BYTES,
};
pub use hooks::{
    run_command_hook, CommandHookConfig, CommandHooks, HookDecision, HookFailure, HookInvocation,
    HookPhase, HookResponse, HookRunResult, HookValidationError, MAX_HOOK_INPUT_BYTES,
    MAX_HOOK_OUTPUT_BYTES, MAX_HOOK_TIMEOUT_MS,
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
pub use native_context::native_model_context_digest;
pub use permission::{
    PermissionChoice, PermissionMode, PermissionPolicy, SessionApprovalError, SessionApprovalGrant,
    SESSION_APPROVAL_GRANT_VERSION,
};
pub use persistent_command::{
    PersistentCommandEvent, PersistentCommandExit, PersistentCommandLimits,
    PersistentCommandManager, PersistentCommandSessionInfo, PersistentCommandSnapshot,
    PersistentCommandStarted, PersistentCommandStream,
};
pub use plan_tool::update_plan_tool_definition;
pub use project_instructions::{
    load_project_instructions, load_project_instructions_for_target, LoadedProjectInstructions,
    ProjectInstructionEntry, ProjectInstructionLimits, ProjectInstructionsError,
    AGENTS_MD_FILENAME, AGENTS_OVERRIDE_FILENAME, DEFAULT_PROJECT_DOC_MAX_BYTES,
    DEFAULT_PROJECT_DOC_MAX_LEVELS,
};
pub use registry::{
    ToolCatalog, ToolCatalogError, MAX_ADVERTISED_TOOLS, MAX_CATALOG_SCHEMA_BYTES,
    MAX_TOOL_NAME_BYTES, MAX_TOOL_SCHEMA_BYTES,
};
pub use responses::{
    hosted_web_citations, hosted_web_search_calls, HostedWebCitation, HostedWebSearchAction,
    HostedWebSearchCall, HostedWebSearchConfig, HostedWebSearchContextSize,
    HostedWebSearchLocation, HostedWebSearchMode, HostedWebSource, ResponseUsage, ResponsesBackend,
    ResponsesResult,
};
pub use resume::{
    discard_aborted_tool_batch, record_verified_tool_outcome, ResumeDisposition, ResumeEngine,
    ResumeError, ResumeStep,
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
    SubagentControl, SubagentCoordinator, SubagentError, SubagentLimits, SubagentOutcome,
    SubagentRecord, SubagentRunLease, SubagentSpec, SubagentStatus,
};
pub use tool_preparation::{PreparedToolSession, ToolPreparation};
pub use tools::{
    LocalTool, LocalTools, ToolContext, ToolDefinition, ToolExecutor, ToolOutput, ToolPermissions,
};
