use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("agent run was cancelled")]
    Cancelled,
    #[error("invalid run request: {0}")]
    InvalidRequest(String),
    #[error("model request failed: {0}")]
    Model(String),
    #[error("tool failed: {0}")]
    Tool(String),
    #[error("agent limit reached: {0}")]
    Limit(String),
    #[error("runtime failed: {0}")]
    Runtime(String),
}
