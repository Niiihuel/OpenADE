//! Per-turn tool resources owned by the runner's Tokio worker runtime.
//!
//! A preparation can discover external tools before the first model request.
//! The returned session remains alive through every model and tool step, and
//! the runner awaits shutdown before recording a terminal turn event.

use crate::{AgentError, ToolExecutor};
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[async_trait]
pub trait ToolPreparation: Send + Sync {
    /// A failed preparation must clean up any resources it started before
    /// returning. Cancellation is passed through so discovery can stop early.
    async fn prepare(
        &self,
        workspace: &Path,
        base_tools: Arc<dyn ToolExecutor>,
        cancellation: &CancellationToken,
    ) -> Result<Box<dyn PreparedToolSession>, AgentError>;
}

#[async_trait]
pub trait PreparedToolSession: Send + Sync {
    /// Includes the original local tools and every discovered external tool.
    fn tools(&self) -> Arc<dyn ToolExecutor>;

    /// Exact advertised names that require confirmation before execution.
    /// Explicit Deny rules in the runner's permission policy remain Deny.
    fn ask_tool_names(&self) -> &[String];

    /// Verified runtime identity for every enabled MCP server in this turn.
    /// None leaves execution available but prevents automatic recovery of a
    /// pending MCP call. Never derive this digest from credential values; an
    /// opaque credential revision is usable only if a trusted vault can prove
    /// it still belongs to the same authorization principal.
    fn mcp_resume_identity_digest(&self) -> Option<&str> {
        None
    }

    /// What preparation did on its own that the person must see, as the
    /// `code` and `detail` of a `HostNotice` each.
    fn notices(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Called on success, failure, and cancellation while the worker's Tokio
    /// runtime is still alive. Implementations should make shutdown idempotent.
    async fn shutdown(&self) -> Result<(), AgentError>;
}
