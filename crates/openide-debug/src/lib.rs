//! Bounded DAP over an explicitly started adapter's standard streams.
//!
//! No filesystem/network/process work occurs in `send`, `drain`, or `Drop`.
//! Source coordinates are DAP one-based lines/columns. Object references and
//! responses carry `stop_id`; a host must not reuse them after execution resumes.
mod protocol;
mod session;
mod worker;

use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use session::DebugSession;
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

pub type RequestId = u64;

#[derive(Clone, Debug)]
pub struct DebugConfig {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub workspace: PathBuf,
    pub adapter_id: String,
    /// Adapter-specific launch arguments, passed without a shell or interpolation.
    pub launch: Value,
    pub breakpoints: Vec<SourceBreakpoints>,
    pub request_timeout: Duration,
    pub shutdown_timeout: Duration,
}
impl DebugConfig {
    pub fn new(
        executable: impl Into<PathBuf>,
        workspace: impl Into<PathBuf>,
        launch: Value,
    ) -> Self {
        Self {
            executable: executable.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            workspace: workspace.into(),
            adapter_id: "generic".into(),
            launch,
            breakpoints: Vec::new(),
            request_timeout: Duration::from_secs(20),
            shutdown_timeout: Duration::from_millis(1200),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceBreakpoints {
    pub path: PathBuf,
    /// One-based source lines. Replaces the complete set for this source.
    pub lines: Vec<u32>,
}

#[derive(Clone, Debug)]
pub enum DebugCommand {
    SetBreakpoints(SourceBreakpoints),
    Threads,
    StackTrace {
        thread_id: i64,
        start_frame: u32,
        levels: u32,
    },
    Scopes {
        frame_id: i64,
    },
    Variables {
        variables_reference: i64,
        start: u32,
        count: u32,
    },
    Continue {
        thread_id: i64,
    },
    Pause {
        thread_id: i64,
    },
    Next {
        thread_id: i64,
    },
    StepIn {
        thread_id: i64,
    },
    StepOut {
        thread_id: i64,
    },
    Stop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DebugState {
    Starting,
    Initializing,
    Configuring,
    Running,
    Stopped,
    Stopping,
    Disconnected,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugCapabilities {
    #[serde(default)]
    pub supports_configuration_done_request: bool,
    #[serde(default)]
    pub supports_terminate_request: bool,
    #[serde(default)]
    pub support_terminate_debuggee: bool,
    #[serde(default)]
    pub supports_single_thread_execution_requests: bool,
    #[serde(default)]
    pub supports_value_formatting_options: bool,
    #[serde(default)]
    pub supports_delayed_stack_trace_loading: bool,
    #[serde(flatten)]
    pub additional: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub source_reference: Option<i64>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Breakpoint {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub column: Option<u32>,
    /// Original request line; adapters can move the actual `line`.
    #[serde(default)]
    pub requested_line: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Thread {
    pub id: i64,
    pub name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub source: Option<Source>,
    pub line: u32,
    pub column: u32,
    #[serde(default)]
    pub end_line: Option<u32>,
    #[serde(default)]
    pub end_column: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub name: String,
    pub variables_reference: i64,
    #[serde(default)]
    pub expensive: bool,
    #[serde(default)]
    pub named_variables: Option<u32>,
    #[serde(default)]
    pub indexed_variables: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub name: String,
    pub value: String,
    #[serde(rename = "type", default)]
    pub type_name: Option<String>,
    pub variables_reference: i64,
    #[serde(default)]
    pub evaluate_name: Option<String>,
    #[serde(default)]
    pub named_variables: Option<u32>,
    #[serde(default)]
    pub indexed_variables: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub enum DebugEvent {
    State(DebugState),
    Capabilities(DebugCapabilities),
    Stopped {
        stop_id: u64,
        reason: String,
        description: Option<String>,
        thread_id: Option<i64>,
        all_threads_stopped: bool,
    },
    Continued {
        thread_id: Option<i64>,
        all_threads_continued: bool,
    },
    /// Cached inspection data was retired by the adapter. The host should
    /// discard pending inspection IDs and request a fresh stack if stopped.
    Invalidated {
        areas: Vec<String>,
        thread_id: Option<i64>,
        stack_frame_id: Option<i64>,
    },
    Breakpoints {
        request_id: Option<RequestId>,
        source: PathBuf,
        generation: u64,
        breakpoints: Vec<Breakpoint>,
    },
    BreakpointChanged {
        reason: String,
        breakpoint: Breakpoint,
    },
    Threads {
        request_id: Option<RequestId>,
        threads: Vec<Thread>,
    },
    StackTrace {
        request_id: RequestId,
        stop_id: u64,
        thread_id: i64,
        frames: Vec<StackFrame>,
        total_frames: Option<u32>,
    },
    Scopes {
        request_id: RequestId,
        stop_id: u64,
        frame_id: i64,
        scopes: Vec<Scope>,
    },
    Variables {
        request_id: RequestId,
        stop_id: u64,
        variables_reference: i64,
        variables: Vec<Variable>,
    },
    Output {
        category: String,
        output: String,
    },
    RequestFailed {
        request_id: Option<RequestId>,
        command: String,
        message: String,
    },
    Exited {
        exit_code: Option<i64>,
    },
    Terminated,
    Disconnected {
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugError {
    pub message: String,
}
impl std::fmt::Display for DebugError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for DebugError {}
fn error(message: impl Into<String>) -> DebugError {
    DebugError {
        message: message.into(),
    }
}
