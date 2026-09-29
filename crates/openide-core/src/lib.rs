//! Shared, transport independent types for an agent session.

mod checkpoint;
mod event;
mod image;
mod message;
mod plan;
mod replay;
mod store;

pub use checkpoint::{
    Checkpoint, CheckpointBudgetSnapshot, CheckpointCompatibility, CheckpointError,
    CheckpointFileStore, CheckpointReference, RetainedUserMessage, CHECKPOINT_VERSION,
    MAX_CHECKPOINT_FILE_BYTES, MAX_CHECKPOINT_SUMMARY_BYTES, MAX_RETAINED_USER_MESSAGES,
    MAX_RETAINED_USER_MESSAGE_BYTES,
};
pub use event::{
    system_instructions_digest, AgentEvent, AgentEventKind, ModelTokenUsage, OutputStream,
    PermissionResolution, EXEC_POLICY_IDENTITY_VERSION, HOOK_CONFIG_IDENTITY_VERSION,
    MAX_SYSTEM_INSTRUCTIONS_BYTES, MCP_RUNTIME_IDENTITY_VERSION, MODEL_CONTEXT_VERSION,
    SYSTEM_INSTRUCTIONS_VERSION,
};
pub use image::{
    validate_user_images, UserImage, UserImageError, UserImageMime, MAX_USER_IMAGES_PER_MESSAGE,
    MAX_USER_IMAGE_BYTES, MAX_USER_IMAGE_MESSAGE_BYTES,
};
pub use message::{AgentMessage, AgentRunRequest, MessageRole, ToolCall, TurnMode};
pub use plan::{
    PlanSnapshot, PlanStep, PlanStepStatus, MAX_PLAN_EXPLANATION_BYTES, MAX_PLAN_SNAPSHOT_BYTES,
    MAX_PLAN_STEPS, MAX_PLAN_STEP_BYTES,
};
pub use replay::{
    replay_native, NativeReplay, ReplayError, ReplayRecovery, ReplayedMessage, ReplayedToolBatch,
    ReplayedTurn,
};
pub use store::{
    EventStore, JsonlSessionStore, MemorySessionStore, NativeEventJournal, NativeSessionLease,
    NativeSteerInput, PermissionDecision, RecordedPermissionDecision, Session,
    SessionApprovalGrantRecord, SessionDiagnostic, SessionListKind, SessionListing, SessionQuery,
    SessionQueryItem, SessionQueryResult, SessionSort, SessionStore, StoreError, StoredSession,
    SubagentLink,
};
