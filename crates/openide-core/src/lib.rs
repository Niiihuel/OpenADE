//! Shared, transport independent types for an agent session.

mod checkpoint;
mod event;
mod image;
mod message;
mod native_automation_origin;
mod native_command_operation;
mod native_command_output;
mod native_goal;
mod native_route;
mod plan;
mod replay;
mod store;
mod tool_media;

pub use tool_media::{
    validate_tool_result_media, ToolResultMediaError, MAX_TOOL_MEDIA_BYTES, MAX_TOOL_MEDIA_IMAGES,
    MAX_TOOL_MEDIA_NOTICES, MAX_TOOL_MEDIA_NOTICE_BYTES, MAX_TOOL_MEDIA_TEXT_BYTES,
};

pub use checkpoint::{
    Checkpoint, CheckpointBudgetSnapshot, CheckpointCompatibility, CheckpointError,
    CheckpointFileStore, CheckpointReference, RetainedUserMessage, CHECKPOINT_VERSION,
    MAX_CHECKPOINT_FILE_BYTES, MAX_CHECKPOINT_SUMMARY_BYTES, MAX_RETAINED_USER_MESSAGES,
    MAX_RETAINED_USER_MESSAGE_BYTES,
};
pub use event::{
    NOTICE_CONTEXT_LIMIT, NOTICE_CONTEXT_SUMMARIZED, NOTICE_CONTEXT_TRIMMED, NOTICE_OUTPUT_LIMIT,
    system_instructions_digest, AgentEvent, AgentEventKind, ModelStreamEvent, ModelTokenUsage,
    OutputStream, PermissionResolution, TurnPauseClass, EXEC_POLICY_IDENTITY_VERSION, HOOK_CONFIG_IDENTITY_VERSION,
    MAX_SYSTEM_INSTRUCTIONS_BYTES, MCP_RUNTIME_IDENTITY_VERSION, MODEL_CONTEXT_VERSION,
    SYSTEM_INSTRUCTIONS_VERSION,
};
pub use image::{
    validate_user_images, UserImage, UserImageError, UserImageMime, MAX_USER_IMAGES_PER_MESSAGE,
    MAX_USER_IMAGE_BYTES, MAX_USER_IMAGE_MESSAGE_BYTES,
};
pub use message::{AgentMessage, AgentRunRequest, MessageRole, ToolCall, TurnMode};
pub use native_automation_origin::{
    native_automation_first_turn_id, native_automation_request_digest,
    native_automation_session_id, native_automation_snapshot_digest,
    NativeAutomationAuthorityProfile, NativeAutomationOrigin, NativeAutomationOriginError,
    MAX_NATIVE_AUTOMATION_AUTHORITY_SNAPSHOT_BYTES, NATIVE_AUTOMATION_AUTHORITY_SNAPSHOT_VERSION,
    NATIVE_AUTOMATION_ORIGIN_VERSION,
};
pub use native_command_operation::{
    NativeCommandOperationAction, NativeCommandOperationError, NativeCommandOperationOutcome,
    NativeCommandOperationPhase, NativeCommandOperationPhaseDiagnostic,
    NativeCommandOperationRequest, NativeCommandOperationSnapshot,
    NativeCommandOperationStartReceipt, NativeCommandOperationState, NativeCommandOperationStore,
    MAX_NATIVE_COMMAND_DIAGNOSTIC_BYTES, MAX_NATIVE_COMMAND_INPUT_BYTES,
    NATIVE_COMMAND_OPERATION_VERSION,
};
pub use native_command_output::{
    NativeCommandOutputAppendAck, NativeCommandOutputBinding, NativeCommandOutputChunk,
    NativeCommandOutputError, NativeCommandOutputExit, NativeCommandOutputHeader,
    NativeCommandOutputPage, NativeCommandOutputRange, NativeCommandOutputRegistration,
    NativeCommandOutputStore, NativeCommandOutputStream, NativeCommandOutputSummary,
    NativeCommandOutputTerminal, MAX_NATIVE_COMMAND_OUTPUT_APPEND_BYTES,
    MAX_NATIVE_COMMAND_OUTPUT_CHUNK_BYTES, MAX_NATIVE_COMMAND_OUTPUT_COMMANDS,
    MAX_NATIVE_COMMAND_OUTPUT_FILE_BYTES, MAX_NATIVE_COMMAND_OUTPUT_PAGE_BYTES,
    MAX_NATIVE_COMMAND_OUTPUT_PAGE_RECORDS, MAX_NATIVE_COMMAND_OUTPUT_PAGE_SERIALIZED_BYTES,
    MAX_NATIVE_COMMAND_OUTPUT_RAW_BYTES, MAX_NATIVE_COMMAND_OUTPUT_RECORDS,
    NATIVE_COMMAND_OUTPUT_VERSION,
};
pub use native_goal::{
    NativeGoal, NativeGoalCommitReceipt, NativeGoalCriterion, NativeGoalError, NativeGoalEvidence,
    NativeGoalMutation, NativeGoalRecoveryDiagnostic, NativeGoalRecoveryReport, NativeGoalStatus,
    NativeGoalStore,
};
pub use native_route::{
    model_request_profile_digest, native_model_endpoint_digest, reconcile_model_history_route,
    ModelRouteSnapshot, MAX_MODEL_ROUTE_ENDPOINT_BYTES, MAX_MODEL_ROUTE_MODEL_BYTES,
    MAX_MODEL_ROUTE_PROVIDER_BYTES, MODEL_ROUTE_VERSION,
};
pub use plan::{
    PlanSnapshot, PlanStep, PlanStepStatus, MAX_PLAN_EXPLANATION_BYTES, MAX_PLAN_SNAPSHOT_BYTES,
    MAX_PLAN_STEPS, MAX_PLAN_STEP_BYTES,
};
pub use replay::{
    ReplaySuperseded,
    replay_native, NativeReplay, ReplayError, ReplayRecovery, ReplayedMessage, ReplayedToolBatch,
    ReplayedTurn,
};
pub use store::{
    EventStore, JsonlSessionStore, MemorySessionStore, NativeEventJournal, NativeRewind, NativeRewindRefusal,
    NativeSessionLease,
    NativeSteerInput, PermissionDecision, RecordedPermissionDecision, Session,
    SessionApprovalGrantRecord, SessionDiagnostic, SessionListKind, SessionListing, SessionQuery,
    SessionQueryItem, SessionQueryResult, SessionSort, SessionStore, StoreError, StoredSession,
    SubagentLink, MAX_SESSION_METADATA_BYTES,
};
