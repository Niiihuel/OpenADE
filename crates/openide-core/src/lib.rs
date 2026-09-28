//! Shared, transport independent types for an agent session.

mod event;
mod message;
mod store;

pub use event::{AgentEvent, AgentEventKind, OutputStream, PermissionResolution};
pub use message::{AgentMessage, AgentRunRequest, MessageRole, ToolCall};
pub use store::{
    EventStore, JsonlSessionStore, MemorySessionStore, PermissionDecision,
    RecordedPermissionDecision, Session, SessionDiagnostic, SessionListKind, SessionListing,
    SessionQuery, SessionQueryItem, SessionQueryResult, SessionSort, SessionStore, StoreError,
    StoredSession,
};
