//! Bounded MCP 2025-11-25 client for locally spawned stdio servers.
//!
//! Each connection owns one subprocess and serializes requests over its pipes.
//! The server's stdout is exclusively JSON-RPC; stderr is discarded. HTTP,
//! OAuth, elicitation, prompts and resources are outside this transport.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{broadcast, Mutex, Notify};
use tokio::task::JoinHandle;
use tokio::time::{timeout, timeout_at, Instant};
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
use crate::process_tree;
use crate::process_tree::ProcessTreeGuard;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
pub const MAX_MCP_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_MCP_TOOLS: usize = 512;
pub const MAX_MCP_TOOL_PAGES: usize = 64;
// Discovery retains complete metadata, including outputSchema and annotations.
// The aggregate bound also bounds each definition. Model input-schema limits
// belong to ToolCatalog and are deliberately independent of this metadata.
const MAX_MCP_CATALOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_PENDING_CANCELLED_IDS: usize = 256;
const STDIO_NOTIFICATION_CAPACITY: usize = 64;

#[derive(Clone, Debug)]
pub struct McpStdioConfig {
    pub command: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// An explicit environment. Process environment is cleared by default.
    pub env: BTreeMap<OsString, OsString>,
    pub inherit_env: bool,
    pub request_timeout: Duration,
    pub shutdown_timeout: Duration,
    pub max_message_bytes: usize,
    pub client_name: String,
    pub client_version: String,
}

impl McpStdioConfig {
    pub fn new(command: impl Into<OsString>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            inherit_env: false,
            request_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(3),
            max_message_bytes: MAX_MCP_MESSAGE_BYTES,
            client_name: "openide".to_owned(),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    fn validate(&self) -> Result<(), McpError> {
        if self.command.is_empty() {
            return Err(McpError::InvalidConfig("command cannot be empty"));
        }
        if self.request_timeout.is_zero() || self.shutdown_timeout.is_zero() {
            return Err(McpError::InvalidConfig("timeouts must be positive"));
        }
        if !(1024..=MAX_MCP_MESSAGE_BYTES).contains(&self.max_message_bytes) {
            return Err(McpError::InvalidConfig("invalid message size limit"));
        }
        if self.client_name.is_empty() || self.client_version.is_empty() {
            return Err(McpError::InvalidConfig(
                "client name and version are required",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum McpError {
    #[error("invalid MCP stdio configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("MCP server process could not start: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("MCP transport I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("MCP protocol error: {0}")]
    Protocol(String),
    #[error("MCP server error {code}: {message}")]
    Remote { code: i64, message: String },
    #[error("MCP request timed out")]
    Timeout,
    #[error("MCP request cancelled")]
    Cancelled,
    #[error("MCP client is shutting down")]
    Closed,
    #[error("MCP message exceeds {limit} bytes")]
    MessageTooLarge { limit: usize },
    #[error("MCP tool catalog exceeds its limit: {0}")]
    CatalogLimit(&'static str),
    #[error("MCP tool was not in the last advertised catalog: {0}")]
    UnadvertisedTool(String),
    #[error("MCP server changed its tool catalog; rediscovery is required")]
    CatalogChanged,
    #[error("MCP authorization changed before tool dispatch; reconnect and rediscover")]
    AuthorizationChanged,
    #[error("MCP tool outcome is uncertain: {0}")]
    EffectUncertain(String),
    #[error("HTTP MCP endpoint uses the legacy initialization lifecycle")]
    LegacyHttpRequired,
    #[error("HTTP MCP session expired")]
    SessionExpired,
    #[error(
        "MCP HTTP handshake failed: {handshake}; session cleanup failed: {cleanup}; the remote session may remain active"
    )]
    HandshakeCleanupFailed {
        #[source]
        handshake: Box<McpError>,
        cleanup: Box<McpError>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct McpServerInfo {
    pub protocol_version: String,
    pub name: String,
    pub version: String,
    pub capabilities: Value,
    pub instructions: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpCallResult {
    pub content: Vec<Value>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
    #[serde(rename = "structuredContent", default)]
    pub structured_content: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// One local MCP subprocess. Requests are serialized to preserve exact IDs and
/// allow late responses to cancelled requests to be discarded safely.
pub struct McpStdioClient {
    connection: Arc<Mutex<McpConnection<ChildStdout, ChildStdin>>>,
    child: Mutex<Option<Child>>,
    tree: StdMutex<Option<ProcessTreeGuard>>,
    closed: CancellationToken,
    idle_wakeup: Arc<Notify>,
    idle_task: Mutex<Option<JoinHandle<()>>>,
    notifications: broadcast::Sender<Value>,
    catalog_changed: Arc<AtomicBool>,
    server_info: McpServerInfo,
    request_timeout: Duration,
    shutdown_timeout: Duration,
}

impl McpStdioClient {
    pub async fn connect(config: McpStdioConfig) -> Result<Self, McpError> {
        config.validate()?;
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if !config.inherit_env {
            command.env_clear();
        }
        command.envs(&config.env);
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW | process_tree::CREATE_SUSPENDED);
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        let tree = ProcessTreeGuard::new().map_err(McpError::Spawn)?;
        let mut child = command.spawn().map_err(McpError::Spawn)?;
        #[cfg(windows)]
        if let Err(error) = tree.assign_and_resume(&child) {
            let _ = child.start_kill();
            drop(tree);
            let _ = child.wait().await;
            return Err(McpError::Spawn(error));
        }
        #[cfg(unix)]
        let tree = match ProcessTreeGuard::new(child.id()) {
            Ok(tree) => tree,
            Err(error) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(McpError::Spawn(error));
            }
        };
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Protocol("spawned server has no stdin".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Protocol("spawned server has no stdout".to_owned()))?;
        let mut connection = McpConnection::new(stdout, stdin, config.max_message_bytes);
        let catalog_changed = Arc::clone(&connection.catalog_changed);
        let (notifications, _) = broadcast::channel(STDIO_NOTIFICATION_CAPACITY);
        connection.notifications = Some(notifications.clone());
        let closed = CancellationToken::new();
        let initialized = initialize(
            &mut connection,
            &config.client_name,
            &config.client_version,
            config.request_timeout,
            &closed,
        )
        .await;
        let server_info = match initialized {
            Ok(info) => info,
            Err(error) => {
                drop(tree);
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(error);
            }
        };
        let connection = Arc::new(Mutex::new(connection));
        let idle_wakeup = Arc::new(Notify::new());
        let idle_task = tokio::spawn(read_idle_messages(
            Arc::clone(&connection),
            Arc::clone(&idle_wakeup),
            closed.clone(),
            Arc::clone(&catalog_changed),
        ));
        Ok(Self {
            connection,
            child: Mutex::new(Some(child)),
            tree: StdMutex::new(Some(tree)),
            closed,
            idle_wakeup,
            idle_task: Mutex::new(Some(idle_task)),
            notifications,
            catalog_changed,
            server_info,
            request_timeout: config.request_timeout,
            shutdown_timeout: config.shutdown_timeout,
        })
    }

    pub fn server_info(&self) -> &McpServerInfo {
        &self.server_info
    }

    /// Subscribe to bounded, best-effort server notifications. A receiver can
    /// lag; `catalog_changed` is sticky and does not depend on channel space.
    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Value> {
        self.notifications.subscribe()
    }

    pub fn catalog_changed(&self) -> bool {
        self.catalog_changed.load(Ordering::Acquire)
    }

    /// Fetch a complete, bounded tool snapshot. A successful refresh replaces
    /// the names allowed by `call_tool` atomically.
    pub async fn list_tools(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Vec<McpTool>, McpError> {
        if self.closed.is_cancelled() {
            return Err(McpError::Closed);
        }
        if self.catalog_changed() {
            return Err(McpError::CatalogChanged);
        }
        if self.server_info.capabilities.get("tools").is_none() {
            return Ok(Vec::new());
        }
        let deadline = Instant::now() + self.request_timeout;
        let mut connection = self.lock_connection(deadline, cancellation).await?;
        let result = fetch_tools(&mut connection, deadline, cancellation, &self.closed).await;
        if matches!(result, Err(McpError::Cancelled | McpError::Timeout)) {
            self.abort_server().await;
        }
        result
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        cancellation: &CancellationToken,
    ) -> Result<McpCallResult, McpError> {
        if !arguments.is_object() {
            return Err(McpError::Protocol(
                "tool arguments must be a JSON object".to_owned(),
            ));
        }
        if self.closed.is_cancelled() {
            return Err(McpError::Closed);
        }
        if self.catalog_changed() {
            return Err(McpError::CatalogChanged);
        }
        if cancellation.is_cancelled() {
            return Err(McpError::Cancelled);
        }
        let deadline = Instant::now() + self.request_timeout;
        let mut connection = self.lock_connection(deadline, cancellation).await?;
        let result = call_advertised_tool(
            &mut connection,
            name,
            arguments,
            deadline,
            cancellation,
            &self.closed,
        )
        .await;
        if matches!(result, Err(McpError::Cancelled | McpError::Timeout)) {
            self.abort_server().await;
        }
        result.map_err(mark_uncertain_tool_error)
    }

    async fn abort_server(&self) {
        self.closed.cancel();
        self.idle_wakeup.notify_one();
        self.tree
            .lock()
            .expect("MCP process tree mutex poisoned")
            .take();
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.start_kill();
            let _ = timeout(Duration::from_secs(5), child.wait()).await;
        }
    }

    pub async fn shutdown(&self) -> Result<(), McpError> {
        self.closed.cancel();
        self.idle_wakeup.notify_one();
        if let Some(task) = self.idle_task.lock().await.take() {
            let _ = task.await;
        }
        {
            let mut connection = self.connection.lock().await;
            connection.stdin.take();
        }
        let tree = self
            .tree
            .lock()
            .expect("MCP process tree mutex poisoned")
            .take();
        let mut child_guard = self.child.lock().await;
        let Some(mut child) = child_guard.take() else {
            drop(tree);
            return Ok(());
        };
        match timeout(self.shutdown_timeout, child.wait()).await {
            Ok(Ok(_)) => {
                drop(tree);
                Ok(())
            }
            Ok(Err(error)) => {
                drop(tree);
                Err(McpError::Io(error))
            }
            Err(_) => {
                drop(tree);
                child.start_kill()?;
                child.wait().await?;
                Ok(())
            }
        }
    }

    async fn lock_connection(
        &self,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<tokio::sync::MutexGuard<'_, McpConnection<ChildStdout, ChildStdin>>, McpError> {
        self.idle_wakeup.notify_one();
        tokio::select! {
            biased;
            _ = self.closed.cancelled() => Err(McpError::Closed),
            _ = cancellation.cancelled() => Err(McpError::Cancelled),
            result = timeout_at(deadline, self.connection.lock()) => result.map_err(|_| McpError::Timeout),
        }
    }
}

/// A tool request can have been processed once its write begins. Even a valid
/// JSON-RPC error does not prove that the server made no changes before failing.
/// Only local catalog rejection is definitively pre-dispatch here.
pub(crate) fn mark_uncertain_tool_error(error: McpError) -> McpError {
    match error {
        McpError::UnadvertisedTool(_)
        | McpError::CatalogChanged
        | McpError::AuthorizationChanged
        | McpError::EffectUncertain(_) => error,
        other => McpError::EffectUncertain(other.to_string()),
    }
}

/// Read server messages between client requests. A request wakes this task
/// before acquiring the connection mutex, so an idle read cannot stall it.
/// `read_message` retains complete consumed bytes in `pending` across a
/// cancelled `fill_buf`, making the select cancellation safe.
async fn read_idle_messages<R, W>(
    connection: Arc<Mutex<McpConnection<R, W>>>,
    wakeup: Arc<Notify>,
    closed: CancellationToken,
    catalog_changed: Arc<AtomicBool>,
) where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    loop {
        let mut guard = tokio::select! {
            biased;
            _ = closed.cancelled() => break,
            guard = connection.lock() => guard,
        };
        let notified = wakeup.notified();
        tokio::pin!(notified);
        let result = tokio::select! {
            biased;
            _ = closed.cancelled() => break,
            _ = &mut notified => continue,
            result = guard.receive_idle() => result,
        };
        if result.is_err() {
            guard.poisoned = true;
            guard.stdin.take();
            catalog_changed.store(true, Ordering::Release);
            closed.cancel();
            break;
        }
    }
}

async fn fetch_tools<R, W>(
    connection: &mut McpConnection<R, W>,
    deadline: Instant,
    cancellation: &CancellationToken,
    closed: &CancellationToken,
) -> Result<Vec<McpTool>, McpError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut tools = Vec::new();
    let mut names = BTreeSet::new();
    let mut cursors = BTreeSet::new();
    let mut catalog_bytes = 0usize;
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_MCP_TOOL_PAGES {
        if connection.catalog_changed.load(Ordering::Acquire) {
            return Err(McpError::CatalogChanged);
        }
        let params = cursor
            .as_ref()
            .map_or_else(|| json!({}), |value| json!({"cursor": value}));
        let result = connection
            .request("tools/list", params, deadline, Some(cancellation), closed)
            .await?;
        let object = result
            .as_object()
            .ok_or_else(|| McpError::Protocol("tools/list result must be an object".to_owned()))?;
        let page = object
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| McpError::Protocol("tools/list result has no tools array".to_owned()))?;
        for raw in page {
            if tools.len() >= MAX_MCP_TOOLS {
                return Err(McpError::CatalogLimit("too many tools"));
            }
            let tool_bytes = serde_json::to_vec(raw)
                .expect("serde_json::Value always serializes")
                .len();
            catalog_bytes = catalog_bytes.saturating_add(tool_bytes);
            if catalog_bytes > MAX_MCP_CATALOG_BYTES {
                return Err(McpError::CatalogLimit("tool catalog too large"));
            }
            let tool: McpTool = serde_json::from_value(raw.clone())
                .map_err(|error| McpError::Protocol(format!("invalid tool definition: {error}")))?;
            validate_tool(&tool)?;
            if !names.insert(tool.name.clone()) {
                return Err(McpError::Protocol(format!(
                    "duplicate MCP tool: {}",
                    tool.name
                )));
            }
            tools.push(tool);
        }
        cursor = match object.get("nextCursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
            _ => return Err(McpError::Protocol("invalid nextCursor".to_owned())),
        };
        let Some(next) = &cursor else {
            if connection.catalog_changed.load(Ordering::Acquire) {
                return Err(McpError::CatalogChanged);
            }
            connection.advertised = names;
            return Ok(tools);
        };
        if !cursors.insert(next.clone()) {
            return Err(McpError::CatalogLimit("repeated pagination cursor"));
        }
    }
    Err(McpError::CatalogLimit("too many tool pages"))
}

async fn call_advertised_tool<R, W>(
    connection: &mut McpConnection<R, W>,
    name: &str,
    arguments: Value,
    deadline: Instant,
    cancellation: &CancellationToken,
    closed: &CancellationToken,
) -> Result<McpCallResult, McpError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if connection.catalog_changed.load(Ordering::Acquire) {
        return Err(McpError::CatalogChanged);
    }
    if !connection.advertised.contains(name) {
        return Err(McpError::UnadvertisedTool(name.to_owned()));
    }
    let result = connection
        .request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
            deadline,
            Some(cancellation),
            closed,
        )
        .await?;
    serde_json::from_value(result)
        .map_err(|error| McpError::Protocol(format!("invalid tools/call result: {error}")))
}

struct McpConnection<R, W> {
    stdin: Option<W>,
    stdout: BufReader<R>,
    pending: Vec<u8>,
    next_id: u64,
    max_message_bytes: usize,
    advertised: BTreeSet<String>,
    cancelled_ids: BTreeSet<u64>,
    poisoned: bool,
    notifications: Option<broadcast::Sender<Value>>,
    catalog_changed: Arc<AtomicBool>,
}

impl<R, W> McpConnection<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    fn new(stdout: R, stdin: W, max_message_bytes: usize) -> Self {
        Self {
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            pending: Vec::new(),
            next_id: 1,
            max_message_bytes,
            advertised: BTreeSet::new(),
            cancelled_ids: BTreeSet::new(),
            poisoned: false,
            notifications: None,
            catalog_changed: Arc::new(AtomicBool::new(false)),
        }
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
        cancellation: Option<&CancellationToken>,
        closed: &CancellationToken,
    ) -> Result<Value, McpError> {
        if self.poisoned {
            return Err(McpError::Closed);
        }
        let result = self
            .request_inner(method, params, deadline, cancellation, closed)
            .await;
        if matches!(
            &result,
            Err(McpError::Protocol(_)
                | McpError::MessageTooLarge { .. }
                | McpError::Io(_)
                | McpError::Timeout)
        ) {
            self.poisoned = true;
            self.stdin.take();
        }
        result
    }

    async fn request_inner(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
        cancellation: Option<&CancellationToken>,
        closed: &CancellationToken,
    ) -> Result<Value, McpError> {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Err(McpError::Cancelled);
        }
        if closed.is_cancelled() {
            return Err(McpError::Closed);
        }
        if method == "tools/call" && self.catalog_changed.load(Ordering::Acquire) {
            return Err(McpError::CatalogChanged);
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| McpError::Protocol("request ID exhausted".to_owned()))?;
        let frame = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        tokio::select! {
            biased;
            _ = closed.cancelled() => return Err(McpError::Closed),
            _ = async { if let Some(token) = cancellation { token.cancelled().await } }, if cancellation.is_some() => {
                // The write may have produced only part of a frame. Reuse would
                // risk joining the next request to that partial JSON line.
                self.poisoned = true;
                self.stdin.take();
                return Err(McpError::Cancelled);
            },
            sent = timeout_at(deadline, self.send(&frame)) => sent.map_err(|_| McpError::Timeout)??,
        }
        loop {
            let message = tokio::select! {
                biased;
                _ = closed.cancelled() => return Err(McpError::Closed),
                _ = async { if let Some(token) = cancellation { token.cancelled().await } }, if cancellation.is_some() => {
                    self.cancel(id, "client cancelled").await;
                    return Err(McpError::Cancelled);
                },
                received = timeout_at(deadline, self.receive()) => match received {
                    Ok(result) => result?,
                    Err(_) => {
                        if cancellation.is_some() { self.cancel(id, "deadline exceeded").await; }
                        return Err(McpError::Timeout);
                    },
                },
            };
            if self.handle_server_message(&message, deadline).await? {
                continue;
            }
            let object = message.as_object().expect("validated JSON-RPC object");
            let Some(response_id) = object.get("id").and_then(Value::as_u64) else {
                return Err(McpError::Protocol("response has no numeric ID".to_owned()));
            };
            if response_id < id {
                if self.cancelled_ids.remove(&response_id) {
                    continue;
                }
                return Err(McpError::Protocol(format!(
                    "unexpected stale response ID: {response_id}"
                )));
            }
            if response_id != id {
                return Err(McpError::Protocol(format!(
                    "unexpected response ID: {response_id}"
                )));
            }
            match (object.get("result"), object.get("error")) {
                (Some(result), None) => return Ok(result.clone()),
                (None, Some(error)) => return Err(parse_remote_error(error)?),
                _ => {
                    return Err(McpError::Protocol(
                        "response must contain result or error".to_owned(),
                    ));
                }
            }
        }
    }

    async fn send(&mut self, value: &Value) -> Result<(), McpError> {
        let bytes = serde_json::to_vec(value).map_err(|error| {
            McpError::Protocol(format!("cannot encode JSON-RPC message: {error}"))
        })?;
        if bytes.len() > self.max_message_bytes {
            return Err(McpError::MessageTooLarge {
                limit: self.max_message_bytes,
            });
        }
        let stdin = self.stdin.as_mut().ok_or(McpError::Closed)?;
        stdin.write_all(&bytes).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        read_message(&mut self.stdout, &mut self.pending, self.max_message_bytes).await
    }

    async fn receive_idle(&mut self) -> Result<(), McpError> {
        let message = self.receive().await?;
        if self
            .handle_server_message(&message, Instant::now() + Duration::from_secs(1))
            .await?
        {
            return Ok(());
        }
        let response_id = message
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| McpError::Protocol("idle response has no numeric ID".to_owned()))?;
        if self.cancelled_ids.remove(&response_id) {
            return Ok(());
        }
        Err(McpError::Protocol(format!(
            "unexpected idle response ID: {response_id}"
        )))
    }

    async fn handle_server_message(
        &mut self,
        message: &Value,
        deadline: Instant,
    ) -> Result<bool, McpError> {
        let object = message
            .as_object()
            .ok_or_else(|| McpError::Protocol("JSON-RPC message must be an object".to_owned()))?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(McpError::Protocol("invalid JSON-RPC version".to_owned()));
        }
        let Some(method) = object.get("method") else {
            return Ok(false);
        };
        let method = method
            .as_str()
            .ok_or_else(|| McpError::Protocol("JSON-RPC method must be a string".to_owned()))?;
        if let Some(server_id) = object.get("id") {
            timeout_at(deadline, self.reply_to_server_request(server_id, method))
                .await
                .map_err(|_| McpError::Timeout)??;
        } else {
            if method == "notifications/tools/list_changed" {
                self.catalog_changed.store(true, Ordering::Release);
                self.advertised.clear();
            }
            if let Some(notifications) = &self.notifications {
                // The durable invalidation above never depends on subscribers
                // or capacity of this best-effort notification channel.
                let _ = notifications.send(message.clone());
            }
        }
        Ok(true)
    }

    async fn cancel(&mut self, id: u64, reason: &str) {
        self.cancelled_ids.insert(id);
        if self.cancelled_ids.len() > MAX_PENDING_CANCELLED_IDS {
            let oldest = *self.cancelled_ids.first().expect("set is nonempty");
            self.cancelled_ids.remove(&oldest);
        }
        let frame = json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id,"reason":reason}});
        let _ = timeout(Duration::from_millis(250), self.send(&frame)).await;
    }

    async fn reply_to_server_request(&mut self, id: &Value, method: &str) -> Result<(), McpError> {
        let response = if method == "ping" {
            json!({"jsonrpc":"2.0","id":id,"result":{}})
        } else {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}})
        };
        self.send(&response).await
    }
}

async fn initialize<R, W>(
    connection: &mut McpConnection<R, W>,
    client_name: &str,
    client_version: &str,
    request_timeout: Duration,
    closed: &CancellationToken,
) -> Result<McpServerInfo, McpError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let result = connection
        .request(
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name":client_name,"version":client_version}
            }),
            Instant::now() + request_timeout,
            None,
            closed,
        )
        .await?;
    let server_info = parse_initialize(result)?;
    timeout(
        request_timeout,
        connection.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
    )
    .await
    .map_err(|_| McpError::Timeout)??;
    Ok(server_info)
}

pub(crate) fn parse_initialize(result: Value) -> Result<McpServerInfo, McpError> {
    let object = result
        .as_object()
        .ok_or_else(|| McpError::Protocol("initialize result must be an object".to_owned()))?;
    let version = required_string(object, "protocolVersion")?;
    // These versions use the initialize, tools/list and tools/call lifecycle.
    if !matches!(
        version,
        "2025-11-25" | "2025-06-18" | "2025-03-26" | "2024-11-05"
    ) {
        return Err(McpError::Protocol(format!(
            "unsupported MCP protocol version: {version}"
        )));
    }
    let capabilities = object
        .get("capabilities")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| {
            McpError::Protocol("initialize result has no capabilities object".to_owned())
        })?;
    let implementation = object
        .get("serverInfo")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            McpError::Protocol("initialize result has no serverInfo object".to_owned())
        })?;
    let instructions = match object.get("instructions") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        _ => return Err(McpError::Protocol("invalid server instructions".to_owned())),
    };
    Ok(McpServerInfo {
        protocol_version: version.to_owned(),
        name: required_string(implementation, "name")?.to_owned(),
        version: required_string(implementation, "version")?.to_owned(),
        capabilities,
        instructions,
    })
}

fn required_string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, McpError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| McpError::Protocol(format!("missing or invalid {key}")))
}

fn validate_tool(tool: &McpTool) -> Result<(), McpError> {
    if tool.name.is_empty()
        || tool.name.len() > 128
        || !tool
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(McpError::Protocol(format!(
            "invalid MCP tool name: {}",
            tool.name
        )));
    }
    if !tool.input_schema.is_object() {
        return Err(McpError::Protocol(format!(
            "tool {} has no object inputSchema",
            tool.name
        )));
    }
    Ok(())
}

fn parse_remote_error(value: &Value) -> Result<McpError, McpError> {
    let object = value
        .as_object()
        .ok_or_else(|| McpError::Protocol("invalid JSON-RPC error".to_owned()))?;
    let code = object
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| McpError::Protocol("JSON-RPC error has no code".to_owned()))?;
    let message = required_string(object, "message")?;
    Ok(McpError::Remote {
        code,
        message: message.to_owned(),
    })
}

async fn read_message<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    pending: &mut Vec<u8>,
    limit: usize,
) -> Result<Value, McpError> {
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            return Err(if pending.is_empty() {
                McpError::Protocol("MCP server closed stdout".to_owned())
            } else {
                McpError::Protocol("MCP server closed stdout mid-message".to_owned())
            });
        }
        let newline = bytes.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(bytes.len());
        if content_len > limit.saturating_sub(pending.len()) {
            return Err(McpError::MessageTooLarge { limit });
        }
        pending.extend_from_slice(&bytes[..content_len]);
        reader.consume(content_len + usize::from(newline.is_some()));
        if newline.is_some() {
            let line = std::mem::take(pending);
            let line = line.strip_suffix(b"\r").unwrap_or(&line);
            return serde_json::from_slice(line)
                .map_err(|error| McpError::Protocol(format!("invalid JSON-RPC message: {error}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, split, AsyncBufReadExt, BufReader};

    fn connected_pair(
        limit: usize,
    ) -> (
        McpConnection<
            tokio::io::ReadHalf<tokio::io::DuplexStream>,
            tokio::io::WriteHalf<tokio::io::DuplexStream>,
        >,
        tokio::io::DuplexStream,
    ) {
        let (client, server) = duplex(64 * 1024);
        let (read, write) = split(client);
        (McpConnection::new(read, write, limit), server)
    }

    async fn server_read(server: &mut BufReader<tokio::io::DuplexStream>) -> Value {
        let mut line = String::new();
        server.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn server_write(server: &mut BufReader<tokio::io::DuplexStream>, value: Value) {
        let mut frame = serde_json::to_vec(&value).unwrap();
        frame.push(b'\n');
        server.get_mut().write_all(&frame).await.unwrap();
    }

    #[test]
    fn local_rejection_stays_definitive_but_remote_error_is_uncertain() {
        assert!(matches!(
            mark_uncertain_tool_error(McpError::UnadvertisedTool("missing".into())),
            McpError::UnadvertisedTool(_)
        ));
        assert!(matches!(
            mark_uncertain_tool_error(McpError::Remote {
                code: -32603,
                message: "server failed after mutation".into(),
            }),
            McpError::EffectUncertain(message) if message.contains("server failed after mutation")
        ));
        assert!(matches!(
            mark_uncertain_tool_error(McpError::Timeout),
            McpError::EffectUncertain(message) if message.contains("timed out")
        ));
        assert!(matches!(
            mark_uncertain_tool_error(McpError::Cancelled),
            McpError::EffectUncertain(message) if message.contains("cancelled")
        ));
    }

    #[tokio::test]
    async fn stdio_remote_error_after_received_tool_call_is_uncertain() {
        let (mut client, server) = connected_pair(MAX_MCP_MESSAGE_BYTES);
        client.advertised.insert("mutate".to_owned());
        let server = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            let request = server_read(&mut server).await;
            assert_eq!(request["method"], "tools/call");
            server_write(
                &mut server,
                json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32603,"message":"server failed after mutation"}}),
            )
            .await;
        });
        let cancellation = CancellationToken::new();
        let closed = CancellationToken::new();
        let result = call_advertised_tool(
            &mut client,
            "mutate",
            json!({}),
            Instant::now() + Duration::from_secs(2),
            &cancellation,
            &closed,
        )
        .await
        .map_err(mark_uncertain_tool_error);
        assert!(matches!(
            result,
            Err(McpError::EffectUncertain(message)) if message.contains("server failed after mutation")
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn initialize_negotiates_then_sends_initialized_notification() {
        let (mut client, server) = connected_pair(MAX_MCP_MESSAGE_BYTES);
        let server = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            let request = server_read(&mut server).await;
            assert_eq!(request["method"], "initialize");
            assert_eq!(request["params"]["protocolVersion"], MCP_PROTOCOL_VERSION);
            server_write(&mut server, json!({"jsonrpc":"2.0","id":request["id"],"result":{"protocolVersion":MCP_PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}})).await;
            let notification = server_read(&mut server).await;
            assert_eq!(notification["method"], "notifications/initialized");
            assert!(notification.get("id").is_none());
        });
        let info = initialize(
            &mut client,
            "openide-test",
            "1",
            Duration::from_secs(2),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(info.name, "fixture");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_request_notifies_server_and_ignores_late_response() {
        let (mut client, server) = connected_pair(MAX_MCP_MESSAGE_BYTES);
        let cancellation = CancellationToken::new();
        let cancel_on_server = cancellation.clone();
        let server = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            let first = server_read(&mut server).await;
            cancel_on_server.cancel();
            let cancelled = server_read(&mut server).await;
            assert_eq!(cancelled["method"], "notifications/cancelled");
            assert_eq!(cancelled["params"]["requestId"], first["id"]);
            server_write(
                &mut server,
                json!({"jsonrpc":"2.0","id":first["id"],"result":{}}),
            )
            .await;
            let second = server_read(&mut server).await;
            server_write(
                &mut server,
                json!({"jsonrpc":"2.0","id":second["id"],"result":{"tools":[]}}),
            )
            .await;
        });
        let closed = CancellationToken::new();
        let first = client
            .request(
                "tools/call",
                json!({}),
                Instant::now() + Duration::from_secs(2),
                Some(&cancellation),
                &closed,
            )
            .await;
        assert!(matches!(first, Err(McpError::Cancelled)));
        let second = client
            .request(
                "tools/list",
                json!({}),
                Instant::now() + Duration::from_secs(2),
                None,
                &closed,
            )
            .await
            .unwrap();
        assert_eq!(second["tools"], json!([]));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn server_ping_is_answered_while_request_waits() {
        let (mut client, server) = connected_pair(MAX_MCP_MESSAGE_BYTES);
        let server = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            let request = server_read(&mut server).await;
            server_write(
                &mut server,
                json!({"jsonrpc":"2.0","id":91,"method":"ping"}),
            )
            .await;
            let pong = server_read(&mut server).await;
            assert_eq!(pong["id"], 91);
            assert_eq!(pong["result"], json!({}));
            server_write(
                &mut server,
                json!({"jsonrpc":"2.0","id":request["id"],"result":{}}),
            )
            .await;
        });
        client
            .request(
                "tools/list",
                json!({}),
                Instant::now() + Duration::from_secs(2),
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn idle_notifications_invalidate_catalog_and_reader_stops_on_close() {
        let (mut client, server) = connected_pair(MAX_MCP_MESSAGE_BYTES);
        client.advertised.insert("echo".to_owned());
        let changed = Arc::clone(&client.catalog_changed);
        let (notifications, mut receiver) = broadcast::channel(1);
        client.notifications = Some(notifications);
        let connection = Arc::new(Mutex::new(client));
        let wakeup = Arc::new(Notify::new());
        let closed = CancellationToken::new();
        let idle = tokio::spawn(read_idle_messages(
            Arc::clone(&connection),
            Arc::clone(&wakeup),
            closed.clone(),
            Arc::clone(&changed),
        ));
        let mut server = BufReader::new(server);
        server_write(
            &mut server,
            json!({"jsonrpc":"2.0","id":73,"method":"ping"}),
        )
        .await;
        let pong = timeout(Duration::from_secs(1), server_read(&mut server))
            .await
            .unwrap();
        assert_eq!(pong["id"], 73);
        assert_eq!(pong["result"], json!({}));
        server_write(
            &mut server,
            json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":1}}),
        )
        .await;
        assert_eq!(
            timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .unwrap()["method"],
            "notifications/progress"
        );
        for progress in 2..=4 {
            server_write(
                &mut server,
                json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":progress}}),
            )
            .await;
        }
        server_write(
            &mut server,
            json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}),
        )
        .await;
        timeout(Duration::from_secs(1), async {
            while !changed.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            receiver.recv().await,
            Err(broadcast::error::RecvError::Lagged(_))
        ));
        assert_eq!(
            timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .unwrap()["method"],
            "notifications/tools/list_changed"
        );
        assert!(changed.load(Ordering::Acquire));
        wakeup.notify_one();
        let mut guard = timeout(Duration::from_secs(1), connection.lock())
            .await
            .unwrap();
        assert!(guard.advertised.is_empty());
        assert!(matches!(
            call_advertised_tool(
                &mut guard,
                "echo",
                json!({}),
                Instant::now() + Duration::from_secs(1),
                &CancellationToken::new(),
                &closed,
            )
            .await,
            Err(McpError::CatalogChanged)
        ));
        drop(guard);
        closed.cancel();
        timeout(Duration::from_secs(1), idle)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn notification_during_tool_call_is_sticky_without_repeating_effect() {
        let (mut client, server) = connected_pair(MAX_MCP_MESSAGE_BYTES);
        client.advertised.insert("mutate".to_owned());
        let changed = Arc::clone(&client.catalog_changed);
        let (notifications, mut receiver) = broadcast::channel(1);
        client.notifications = Some(notifications);
        let server = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            let request = server_read(&mut server).await;
            assert_eq!(request["method"], "tools/call");
            server_write(
                &mut server,
                json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}),
            )
            .await;
            server_write(
                &mut server,
                json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[]}}),
            )
            .await;
            assert!(
                timeout(Duration::from_millis(100), server_read(&mut server))
                    .await
                    .is_err()
            );
        });
        call_advertised_tool(
            &mut client,
            "mutate",
            json!({}),
            Instant::now() + Duration::from_secs(1),
            &CancellationToken::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(changed.load(Ordering::Acquire));
        assert_eq!(
            receiver.recv().await.unwrap()["method"],
            "notifications/tools/list_changed"
        );
        assert!(matches!(
            call_advertised_tool(
                &mut client,
                "mutate",
                json!({}),
                Instant::now() + Duration::from_secs(1),
                &CancellationToken::new(),
                &CancellationToken::new(),
            )
            .await,
            Err(McpError::CatalogChanged)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_oversized_message_before_unbounded_allocation() {
        let (mut client, mut server) = connected_pair(1024);
        let server = tokio::spawn(async move {
            let mut line = String::new();
            let mut reader = BufReader::new(&mut server);
            reader.read_line(&mut line).await.unwrap();
            reader.get_mut().write_all(&vec![b'x'; 2048]).await.unwrap();
        });
        let error = client
            .request(
                "tools/list",
                json!({}),
                Instant::now() + Duration::from_secs(2),
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, McpError::MessageTooLarge { limit: 1024 }));
        let retry = client
            .request(
                "tools/list",
                json!({}),
                Instant::now() + Duration::from_secs(2),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert!(matches!(retry, Err(McpError::Closed)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn timeout_notifies_server_then_closes_unreliable_connection() {
        let (mut client, server) = connected_pair(MAX_MCP_MESSAGE_BYTES);
        let server = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            let request = server_read(&mut server).await;
            let cancelled = server_read(&mut server).await;
            assert_eq!(cancelled["method"], "notifications/cancelled");
            assert_eq!(cancelled["params"]["requestId"], request["id"]);
        });
        let error = client
            .request(
                "tools/call",
                json!({}),
                Instant::now() + Duration::from_millis(20),
                Some(&CancellationToken::new()),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, McpError::Timeout));
        assert!(client.poisoned);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn discovery_preserves_large_output_metadata_within_the_catalog_budget() {
        let (mut client, server) = connected_pair(MAX_MCP_MESSAGE_BYTES);
        let metadata = "output-metadata-".repeat(5_000);
        let raw = json!({"name":"large_metadata","description":"Small model description","inputSchema":{"type":"object"},"outputSchema":{"type":"object","description":metadata},"annotations":{"readOnlyHint":true}});
        assert!(serde_json::to_vec(&raw).unwrap().len() > 64 * 1024);
        let expected: McpTool = serde_json::from_value(raw.clone()).unwrap();
        let server = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            let request = server_read(&mut server).await;
            assert_eq!(request["method"], "tools/list");
            server_write(
                &mut server,
                json!({"jsonrpc":"2.0","id":request["id"],"result":{"tools":[raw]}}),
            )
            .await;
        });
        let tools = fetch_tools(
            &mut client,
            Instant::now() + Duration::from_secs(5),
            &CancellationToken::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(tools, vec![expected]);
        assert_eq!(
            client.advertised,
            BTreeSet::from(["large_metadata".to_owned()])
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn discovered_metadata_still_obeys_the_aggregate_budget_across_pages() {
        let (mut client, server) = connected_pair(MAX_MCP_MESSAGE_BYTES);
        client.advertised.insert("previous".into());
        let server = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            for (name, next) in [("first", Some("next")), ("second", None)] {
                let request = server_read(&mut server).await;
                assert_eq!(request["method"], "tools/list");
                let mut result = json!({"tools":[{"name":name,"inputSchema":{"type":"object"},"outputSchema":{"description":"x".repeat(MAX_MCP_CATALOG_BYTES / 2)}}]});
                if let Some(next) = next {
                    result["nextCursor"] = json!(next);
                }
                server_write(
                    &mut server,
                    json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
                )
                .await;
            }
        });
        let error = fetch_tools(
            &mut client,
            Instant::now() + Duration::from_secs(5),
            &CancellationToken::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            McpError::CatalogLimit("tool catalog too large")
        ));
        assert_eq!(client.advertised, BTreeSet::from(["previous".to_owned()]));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn paginates_catalog_and_only_calls_advertised_tool() {
        let (mut client, server) = connected_pair(MAX_MCP_MESSAGE_BYTES);
        let server = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            let first = server_read(&mut server).await;
            assert_eq!(first["method"], "tools/list");
            assert!(first["params"].get("cursor").is_none());
            server_write(&mut server, json!({"jsonrpc":"2.0","id":first["id"],"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}],"nextCursor":"page-2"}})).await;
            let second = server_read(&mut server).await;
            assert_eq!(second["params"]["cursor"], "page-2");
            server_write(&mut server, json!({"jsonrpc":"2.0","id":second["id"],"result":{"tools":[{"name":"sum","description":"Add numbers","inputSchema":{"type":"object"}}]}})).await;
            let call = server_read(&mut server).await;
            assert_eq!(call["method"], "tools/call");
            assert_eq!(
                call["params"],
                json!({"name":"sum","arguments":{"a":1,"b":2}})
            );
            server_write(&mut server, json!({"jsonrpc":"2.0","id":call["id"],"result":{"content":[{"type":"text","text":"3"}],"structuredContent":{"sum":3}}})).await;
        });
        let cancellation = CancellationToken::new();
        let closed = CancellationToken::new();
        let tools = fetch_tools(
            &mut client,
            Instant::now() + Duration::from_secs(2),
            &cancellation,
            &closed,
        )
        .await
        .unwrap();
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["echo", "sum"]
        );
        let denied = call_advertised_tool(
            &mut client,
            "hidden",
            json!({}),
            Instant::now() + Duration::from_secs(2),
            &cancellation,
            &closed,
        )
        .await;
        assert!(matches!(denied, Err(McpError::UnadvertisedTool(_))));
        let result = call_advertised_tool(
            &mut client,
            "sum",
            json!({"a":1,"b":2}),
            Instant::now() + Duration::from_secs(2),
            &cancellation,
            &closed,
        )
        .await
        .unwrap();
        assert_eq!(result.structured_content, Some(json!({"sum":3})));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn duplicate_tool_name_rejects_catalog_without_replacing_snapshot() {
        let (mut client, server) = connected_pair(MAX_MCP_MESSAGE_BYTES);
        client.advertised.insert("previous".to_owned());
        let server = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            let request = server_read(&mut server).await;
            server_write(&mut server, json!({"jsonrpc":"2.0","id":request["id"],"result":{"tools":[{"name":"repeat","inputSchema":{}},{"name":"repeat","inputSchema":{}}]}})).await;
        });
        let error = fetch_tools(
            &mut client,
            Instant::now() + Duration::from_secs(2),
            &CancellationToken::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, McpError::Protocol(message) if message.contains("duplicate")));
        assert!(client.advertised.contains("previous"));
        server.await.unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn spawned_stdio_server_can_initialize_call_and_shutdown() {
        use std::ffi::OsString;
        let directory = tempfile::tempdir().unwrap();
        let script_path = directory.path().join("mcp-fixture.ps1");
        let script = r#"
while ($null -ne ($line = [Console]::In.ReadLine())) {
    $request = $line | ConvertFrom-Json
    if ($request.method -eq 'initialize') {
        $result = @{ protocolVersion = '2025-11-25'; capabilities = @{ tools = @{} }; serverInfo = @{ name = 'fixture'; version = '1' } }
        [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $request.id; result = $result } | ConvertTo-Json -Compress -Depth 20))
    } elseif ($request.method -eq 'tools/list') {
        $result = @{ tools = @(@{ name = 'echo'; inputSchema = @{ type = 'object' } }) }
        [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $request.id; result = $result } | ConvertTo-Json -Compress -Depth 20))
    } elseif ($request.method -eq 'tools/call') {
        $result = @{ content = @(@{ type = 'text'; text = 'ok' }); isError = $false }
        [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $request.id; result = $result } | ConvertTo-Json -Compress -Depth 20))
    }
}
"#;
        std::fs::write(&script_path, script).unwrap();
        let mut config = McpStdioConfig::new("powershell.exe");
        config.args = vec![
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-ExecutionPolicy"),
            OsString::from("Bypass"),
            OsString::from("-File"),
            script_path.into_os_string(),
        ];
        config.inherit_env = true;
        config.request_timeout = Duration::from_secs(10);
        let client = McpStdioClient::connect(config).await.unwrap();
        let cancellation = CancellationToken::new();
        let tools = client.list_tools(&cancellation).await.unwrap();
        assert_eq!(tools.len(), 1);
        let output = client
            .call_tool("echo", json!({}), &cancellation)
            .await
            .unwrap();
        assert_eq!(output.content[0]["text"], "ok");
        client.shutdown().await.unwrap();
        assert!(matches!(
            client.list_tools(&cancellation).await,
            Err(McpError::Closed)
        ));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn shutdown_kills_server_that_ignores_closed_stdin() {
        use std::ffi::OsString;
        let directory = tempfile::tempdir().unwrap();
        let script_path = directory.path().join("stalled-mcp-fixture.ps1");
        let script = r#"
$request = [Console]::In.ReadLine() | ConvertFrom-Json
$result = @{ protocolVersion = '2025-11-25'; capabilities = @{}; serverInfo = @{ name = 'stalled'; version = '1' } }
[Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $request.id; result = $result } | ConvertTo-Json -Compress -Depth 20))
$null = [Console]::In.ReadLine()
while ($true) { Start-Sleep -Seconds 1 }
"#;
        std::fs::write(&script_path, script).unwrap();
        let mut config = McpStdioConfig::new("powershell.exe");
        config.args = vec![
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-ExecutionPolicy"),
            OsString::from("Bypass"),
            OsString::from("-File"),
            script_path.into_os_string(),
        ];
        config.inherit_env = true;
        config.request_timeout = Duration::from_secs(10);
        config.shutdown_timeout = Duration::from_millis(50);
        let client = McpStdioClient::connect(config).await.unwrap();
        timeout(Duration::from_secs(2), client.shutdown())
            .await
            .unwrap()
            .unwrap();
    }

    #[cfg(windows)]
    fn tree_server_config(workspace: &std::path::Path) -> McpStdioConfig {
        let source = workspace.join("mcp_tree_probe.rs");
        let binary = workspace.join("mcp_tree_probe.exe");
        std::fs::write(&source, include_str!("../tests/fixtures/mcp_tree_probe.rs")).unwrap();
        let target = if cfg!(target_env = "gnu") {
            "x86_64-pc-windows-gnu"
        } else {
            "x86_64-pc-windows-msvc"
        };
        let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let output = std::process::Command::new(compiler)
            .arg("--edition=2021")
            .arg("--target")
            .arg(target)
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "MCP tree probe compile failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut config = McpStdioConfig::new(binary);
        config.args = vec![OsString::from("parent"), workspace.as_os_str().to_owned()];
        config.cwd = Some(workspace.to_owned());
        config.request_timeout = Duration::from_secs(10);
        config.shutdown_timeout = Duration::from_millis(100);
        config
    }

    #[cfg(windows)]
    async fn wait_for_tree_marker(workspace: &std::path::Path, name: &str) {
        timeout(Duration::from_secs(10), async {
            while !workspace.join(name).exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("MCP tree marker {name} was not created"));
    }

    #[cfg(windows)]
    async fn assert_no_mcp_descendant_survival(workspace: &std::path::Path) {
        tokio::time::sleep(Duration::from_millis(3_500)).await;
        assert!(
            !workspace.join("child-survived").exists(),
            "MCP descendant survived process-tree cleanup"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cancellation_ends_stdio_server_descendants() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join(".openide-mcp-tree-test-armed"),
            b"armed",
        )
        .unwrap();
        let client = std::sync::Arc::new(
            McpStdioClient::connect(tree_server_config(directory.path()))
                .await
                .unwrap(),
        );
        wait_for_tree_marker(directory.path(), "child-started").await;
        assert_eq!(
            client
                .list_tools(&CancellationToken::new())
                .await
                .unwrap()
                .len(),
            1
        );
        let cancellation = CancellationToken::new();
        let task_client = client.clone();
        let task_cancel = cancellation.clone();
        let call =
            tokio::spawn(
                async move { task_client.call_tool("echo", json!({}), &task_cancel).await },
            );
        wait_for_tree_marker(directory.path(), "request-started").await;
        cancellation.cancel();
        assert!(matches!(
            call.await.unwrap(),
            Err(McpError::EffectUncertain(message)) if message.contains("cancelled")
        ));
        client.shutdown().await.unwrap();
        assert_no_mcp_descendant_survival(directory.path()).await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn timeout_ends_stdio_server_descendants() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join(".openide-mcp-tree-test-armed"),
            b"armed",
        )
        .unwrap();
        let mut client = McpStdioClient::connect(tree_server_config(directory.path()))
            .await
            .unwrap();
        wait_for_tree_marker(directory.path(), "child-started").await;
        assert_eq!(
            client
                .list_tools(&CancellationToken::new())
                .await
                .unwrap()
                .len(),
            1
        );
        client.request_timeout = Duration::from_secs(1);
        let cancellation = CancellationToken::new();
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::EffectUncertain(message)) if message.contains("timed out")
        ));
        assert!(directory.path().join("request-started").exists());
        client.shutdown().await.unwrap();
        assert_no_mcp_descendant_survival(directory.path()).await;
    }

    #[test]
    fn parses_tool_error_separately_from_json_rpc_error() {
        let result: McpCallResult = serde_json::from_value(
            json!({"content":[{"type":"text","text":"failed"}],"isError":true}),
        )
        .unwrap();
        assert!(result.is_error);
        assert_eq!(result.content.len(), 1);
        let remote = parse_remote_error(&json!({"code":-32601,"message":"missing"})).unwrap();
        assert!(matches!(remote, McpError::Remote { code: -32601, .. }));
    }
}
