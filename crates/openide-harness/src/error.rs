use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("agent run was cancelled")]
    Cancelled,
    #[error("invalid run request: {0}")]
    InvalidRequest(String),
    #[error("model request failed: {0}")]
    Model(String),
    /// A classified provider failure. Same wording as `Model`, so hosts that
    /// only print the error keep their text.
    #[error("model request failed: {0}")]
    Provider(crate::ProviderFailure),
    #[error("tool failed: {0}")]
    Tool(String),
    #[error("tool effect is uncertain: {0}")]
    EffectUncertain(String),
    #[error("agent limit reached: {0}")]
    Limit(String),
    /// The run stopped at a model boundary after writing `TurnPaused`. It is
    /// not a failure: no terminal event follows and the turn can be resumed.
    #[error("turn paused at a model boundary")]
    Paused,
    #[error("runtime failed: {0}")]
    Runtime(String),
}
