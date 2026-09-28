//! Native agent loop with replaceable model and tool boundaries.

mod backend;
mod command;
mod context;
mod error;
mod permission;
mod runner;
mod search;
mod tools;

pub use backend::{
    native_agent_supported_efforts, native_agent_supports_tool_calls, ModelBackend, ModelOutput,
    OpenAiCompatibleBackend,
};
pub use context::ContextPolicy;
pub use error::AgentError;
pub use permission::{PermissionChoice, PermissionMode, PermissionPolicy};
pub use runner::{AgentRunHandle, AgentRunner, RunLimits};
pub use tools::{
    LocalTool, LocalTools, ToolContext, ToolDefinition, ToolExecutor, ToolOutput, ToolPermissions,
};
