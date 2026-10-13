//! Runtime ownership for explicitly enabled stdio and Streamable HTTP MCP servers.
//!
//! The runner prepares this environment inside its own Tokio worker runtime.
//! Names returned by `mcp_tool_names` automatically require confirmation.

use crate::mcp_oauth_controller::discover_oauth_session;
use crate::mcp_settings::{McpServerSettings, McpSettings};
use async_trait::async_trait;
use openide_harness::{
    inspect_mcp_stdio_launch, mcp_runtime_digest, AgentError, McpCompositeTools, McpError,
    McpHttpClient, McpServerBinding, McpServerRuntimeIdentity, McpStdioClient,
    McpStdioServerIdentity, McpToolClient, PreparedToolSession, ToolExecutor, ToolPreparation,
};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub(crate) struct McpRuntime {
    tools: Arc<McpCompositeTools>,
    mcp_tool_names: Vec<String>,
    resume_identity_digest: Option<String>,
    /// Servers that did not start, each with why: the turn runs without them.
    skipped: Vec<(String, String)>,
    clients: Vec<(String, McpClient)>,
    notification_cancel: CancellationToken,
    notification_tasks: Mutex<Vec<JoinHandle<()>>>,
    notification_errors: Arc<Mutex<Vec<String>>>,
}

enum McpClient {
    Stdio(Arc<McpStdioClient>),
    Http(Arc<McpHttpClient>),
}

enum McpStartupError {
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for McpStartupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("Inicio MCP cancelado"),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl From<String> for McpStartupError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl McpClient {
    fn tool_client(&self) -> Arc<dyn McpToolClient> {
        match self {
            Self::Stdio(client) => client.clone(),
            Self::Http(client) => client.clone(),
        }
    }

    async fn shutdown(&self) -> Result<(), String> {
        match self {
            Self::Stdio(client) => client.shutdown().await.map_err(|error| error.to_string()),
            Self::Http(client) => client.shutdown().await.map_err(|error| error.to_string()),
        }
    }
}

/// Installed on an AgentRunner; discovery starts only when its Build worker
/// has entered the Tokio runtime that will execute the entire turn.
pub(crate) struct McpToolPreparation {
    servers: Vec<McpServerSettings>,
}

impl McpToolPreparation {
    pub fn new(servers: Vec<McpServerSettings>) -> Self {
        Self { servers }
    }
}

#[async_trait]
impl ToolPreparation for McpToolPreparation {
    async fn prepare(
        &self,
        workspace: &Path,
        base_tools: Arc<dyn ToolExecutor>,
        cancellation: &CancellationToken,
    ) -> Result<Box<dyn PreparedToolSession>, AgentError> {
        let runtime =
            discover_mcp_runtime_inner(&self.servers, workspace, base_tools, cancellation)
                .await
                .map_err(|error| match error {
                    McpStartupError::Cancelled => AgentError::Cancelled,
                    McpStartupError::Failed(message) => AgentError::Runtime(message),
                })?;
        Ok(Box::new(runtime))
    }
}

#[async_trait]
impl PreparedToolSession for McpRuntime {
    fn tools(&self) -> Arc<dyn ToolExecutor> {
        McpRuntime::tools(self)
    }

    fn ask_tool_names(&self) -> &[String] {
        McpRuntime::mcp_tool_names(self)
    }

    fn mcp_resume_identity_digest(&self) -> Option<&str> {
        self.resume_identity_digest.as_deref()
    }

    fn notices(&self) -> Vec<(String, String)> {
        self.skipped
            .iter()
            .map(|(id, why)| (openide_core::NOTICE_MCP_UNAVAILABLE.to_owned(), format!("{id}\n{why}")))
            .collect()
    }

    async fn shutdown(&self) -> Result<(), AgentError> {
        McpRuntime::shutdown(self)
            .await
            .map_err(AgentError::Runtime)
    }
}

impl McpRuntime {
    pub fn tools(&self) -> Arc<dyn ToolExecutor> {
        self.tools.clone()
    }

    pub fn mcp_tool_names(&self) -> &[String] {
        &self.mcp_tool_names
    }

    /// Close every owned server. Idempotent client shutdown also allows a
    /// cancelled or failed run to stop processes without dropping the runtime.
    pub async fn shutdown(&self) -> Result<(), String> {
        self.notification_cancel.cancel();
        let tasks = std::mem::take(
            &mut *self
                .notification_tasks
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        );
        let mut first_error = None;
        for task in tasks {
            if let Err(error) = task.await {
                first_error.get_or_insert_with(|| format!("Listener MCP falló: {error}"));
            }
        }
        if let Err(error) = shutdown_all(&self.clients).await {
            first_error.get_or_insert(error);
        }
        if let Some(error) = self
            .notification_errors
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .first()
        {
            first_error.get_or_insert_with(|| error.clone());
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// Start enabled servers, discover complete tool snapshots, then compose them
/// with the supplied local tools. A server that fails is closed and left out,
/// and the others serve the turn; a cancelled startup closes every server
/// already started by this call.
#[cfg(test)]
pub(crate) async fn discover_mcp_runtime(
    servers: &[McpServerSettings],
    workspace: &Path,
    local_tools: Arc<dyn ToolExecutor>,
    cancellation: &CancellationToken,
) -> Result<McpRuntime, String> {
    discover_mcp_runtime_inner(servers, workspace, local_tools, cancellation)
        .await
        .map_err(|error| error.to_string())
}

async fn discover_mcp_runtime_inner(
    servers: &[McpServerSettings],
    workspace: &Path,
    local_tools: Arc<dyn ToolExecutor>,
    cancellation: &CancellationToken,
) -> Result<McpRuntime, McpStartupError> {
    if cancellation.is_cancelled() {
        return Err(McpStartupError::Cancelled);
    }
    let enabled = servers
        .iter()
        .filter(|server| server.enabled)
        .cloned()
        .collect::<Vec<_>>();
    McpSettings {
        servers: enabled.clone(),
    }
    .validate(workspace)?;

    let mut clients = Vec::new();
    let mut bindings = Vec::new();
    let mut server_identities = Vec::new();
    let mut all_resume_safe = true;
    let mut skipped = Vec::new();
    for server in &enabled {
        if cancellation.is_cancelled() {
            return Err(failed_startup(McpStartupError::Cancelled, &clients).await);
        }
        let mut launch_recheck = None;
        let connected = if server.url.is_some() {
            match server.runtime_http_config() {
                Ok(config) if server.http_oauth.is_some() => {
                    match discover_oauth_session(server, cancellation).await {
                        // Await cooperative cancellation: dropping connect in
                        // an outer select would skip partial-session DELETE.
                        Ok(oauth) => McpHttpClient::connect_with_oauth(config, oauth, cancellation)
                            .await
                            .map(|client| McpClient::Http(Arc::new(client)))
                            .map_err(|error| http_startup_error(&server.id, true, error)),
                        Err(_) if cancellation.is_cancelled() => Err(McpStartupError::Cancelled),
                        Err(error) => Err(McpStartupError::Failed(format!(
                            "MCP HTTP {}: {error}",
                            server.id
                        ))),
                    }
                }
                Ok(config) => McpHttpClient::connect(config, cancellation)
                    .await
                    .map(|client| McpClient::Http(Arc::new(client)))
                    .map_err(|error| http_startup_error(&server.id, false, error)),
                Err(error) => Err(McpStartupError::Failed(error)),
            }
        } else {
            match server.runtime_config(workspace) {
                Ok(mut config) => {
                    if server.credential_env_names.is_empty() && server.env.is_empty() {
                        if let Some(identity) = inspect_mcp_stdio_launch(&config) {
                            config.command = identity.canonical_command.into_os_string();
                            launch_recheck = Some((config.clone(), identity.digest));
                        }
                    }
                    tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => Err(McpStartupError::Cancelled),
                    result = McpStdioClient::connect(config) => {
                        result.map(|client| McpClient::Stdio(Arc::new(client)))
                            .map_err(|error| McpStartupError::Failed(format!("No se pudo iniciar MCP {}: {error}", server.id)))
                    }
                    }
                }
                Err(error) => Err(McpStartupError::Failed(error)),
            }
        };
        let client = match connected {
            Ok(client) => client,
            // A failure while being cancelled is the cancellation's, not the server's.
            Err(McpStartupError::Failed(why)) if !cancellation.is_cancelled() => {
                all_resume_safe = false;
                skipped.push((server.id.clone(), why));
                continue;
            }
            Err(error) => {
                return Err(failed_startup(error, &clients).await);
            }
        };
        let verified_stdio_identity = match (&client, launch_recheck) {
            (McpClient::Stdio(client), Some((config, before))) => inspect_mcp_stdio_launch(&config)
                .filter(|after| after.digest == before)
                .map(|_| McpStdioServerIdentity {
                    id: server.id.clone(),
                    launch_digest: before,
                    server_info: client.server_info().clone(),
                }),
            _ => None,
        };
        let tool_client = client.tool_client();
        clients.push((server.id.clone(), client));
        let binding =
            match McpServerBinding::discover(server.id.clone(), tool_client, cancellation).await {
                Ok(binding) => binding,
                Err(error) => {
                    let error = if matches!(
                        error,
                        openide_harness::McpToolAdapterError::Discovery {
                            source: McpError::Cancelled,
                            ..
                        }
                    ) {
                        McpStartupError::Cancelled
                    } else {
                        McpStartupError::Failed(format!(
                            "No se pudo descubrir MCP {}: {error}",
                            server.id
                        ))
                    };
                    if let (McpStartupError::Failed(why), false) = (&error, cancellation.is_cancelled()) {
                        let why = why.clone();
                        let _ = shutdown_all(&clients[clients.len() - 1..]).await;
                        clients.pop();
                        all_resume_safe = false;
                        skipped.push((server.id.clone(), why));
                        continue;
                    }
                    return Err(failed_startup(error, &clients).await);
                }
            };
        let verified_identity = match &clients.last().expect("connected MCP client").1 {
            McpClient::Stdio(_) => verified_stdio_identity.map(McpServerRuntimeIdentity::Stdio),
            McpClient::Http(client) => client
                .resume_server_identity(server.id.clone())
                .await
                .map(McpServerRuntimeIdentity::Http),
        };
        match verified_identity {
            Some(identity) => server_identities.push(identity),
            None => all_resume_safe = false,
        }
        bindings.push(binding);
    }
    if cancellation.is_cancelled() {
        return Err(failed_startup(McpStartupError::Cancelled, &clients).await);
    }
    let tools = match McpCompositeTools::new(local_tools, bindings) {
        Ok(tools) => Arc::new(tools),
        Err(error) => {
            return Err(failed_startup(
                McpStartupError::Failed(format!("Catálogo MCP no válido: {error}")),
                &clients,
            )
            .await);
        }
    };
    let mcp_tool_names = tools.mcp_tool_names().map(str::to_owned).collect();
    let resume_identity_digest = all_resume_safe
        .then(|| mcp_runtime_digest(&server_identities, &tools.catalog().digest_hex()))
        .flatten();
    let notification_cancel = cancellation.child_token();
    let notification_errors = Arc::new(Mutex::new(Vec::new()));
    let mut notification_tasks = Vec::new();
    for (id, client) in &clients {
        if let McpClient::Stdio(client) = client {
            let mut receiver = client.subscribe_notifications();
            if client.catalog_changed() {
                tools.invalidate_server_catalog(id);
            }
            let id = id.clone();
            let client = Arc::clone(client);
            let tools = Arc::clone(&tools);
            let cancel = notification_cancel.child_token();
            notification_tasks.push(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        received = receiver.recv() => match received {
                            Ok(notification) => {
                                if notification.get("method").and_then(serde_json::Value::as_str)
                                    == Some("notifications/tools/list_changed")
                                {
                                    tools.invalidate_server_catalog(&id);
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                // The client's sticky flag survives a full channel.
                                if client.catalog_changed() {
                                    tools.invalidate_server_catalog(&id);
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        },
                    }
                }
            }));
            continue;
        }
        let McpClient::Http(client) = client else {
            continue;
        };
        if !client.supports_legacy_notifications() {
            continue;
        }
        let id = id.clone();
        let client = Arc::clone(client);
        let tools = Arc::clone(&tools);
        let errors = Arc::clone(&notification_errors);
        let cancel = notification_cancel.child_token();
        notification_tasks.push(tokio::spawn(async move {
            let (sender, mut receiver) = mpsc::channel(32);
            let listener = client.listen_legacy_notifications(&sender, &cancel);
            tokio::pin!(listener);
            loop {
                tokio::select! {
                    result = &mut listener => {
                        if let Err(error) = result {
                            if !cancel.is_cancelled() {
                                tools.invalidate_server_catalog(&id);
                                errors.lock()
                                    .unwrap_or_else(|poison| poison.into_inner())
                                    .push(format!("Notificaciones MCP {id}: {error}"));
                            }
                        }
                        break;
                    }
                    Some(notification) = receiver.recv() => {
                        if notification.get("method").and_then(serde_json::Value::as_str)
                            == Some("notifications/tools/list_changed")
                        {
                            tools.invalidate_server_catalog(&id);
                        }
                    }
                }
            }
        }));
    }
    Ok(McpRuntime {
        tools,
        mcp_tool_names,
        resume_identity_digest,
        skipped,
        clients,
        notification_cancel,
        notification_tasks: Mutex::new(notification_tasks),
        notification_errors,
    })
}

fn http_startup_error(id: &str, oauth: bool, error: McpError) -> McpStartupError {
    if matches!(error, McpError::Cancelled) {
        return McpStartupError::Cancelled;
    }
    let message = if oauth {
        format!(
            "No se pudo conectar MCP HTTP {id} con OAuth: {error}. Si el servidor rechazó la credencial, autorizá de nuevo en Settings > MCP."
        )
    } else {
        format!("No se pudo conectar MCP HTTP {id}: {error}")
    };
    McpStartupError::Failed(message)
}

async fn failed_startup(
    error: McpStartupError,
    clients: &[(String, McpClient)],
) -> McpStartupError {
    match shutdown_all(clients).await {
        Ok(()) => error,
        Err(cleanup) => McpStartupError::Failed(format!("{error}; {cleanup}")),
    }
}

async fn shutdown_all(clients: &[(String, McpClient)]) -> Result<(), String> {
    let mut first_error = None;
    for (id, client) in clients {
        if let Err(error) = client.shutdown().await {
            if first_error.is_none() {
                first_error = Some(format!("No se pudo cerrar MCP {id}: {error}"));
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::ToolCall;
    use openide_harness::{LocalTools, ToolContext};
    use serde_json::json;
    #[cfg(windows)]
    use std::fs;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn discovers_http_server_routes_tool_and_closes_client() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            let mut methods = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(10);
            for _ in 0..3 {
                let stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "HTTP MCP fixture timed out");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("HTTP MCP fixture accept failed: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut content_length = 0usize;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(value) = lower.strip_prefix("content-length:") {
                        content_length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).unwrap();
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let method = request["method"].as_str().unwrap().to_owned();
                methods.push(method.clone());
                let result = match method.as_str() {
                    "server/discover" => {
                        json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}}})
                    }
                    "tools/list" => {
                        json!({"resultType":"complete","tools":[{"name":"echo","inputSchema":{"type":"object"}}]})
                    }
                    "tools/call" => {
                        json!({"resultType":"complete","content":[{"type":"text","text":"remote-ok"}]})
                    }
                    _ => panic!("unexpected HTTP MCP method"),
                };
                let response = serde_json::to_vec(
                    &json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
                )
                .unwrap();
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len()
                );
                reader.get_mut().write_all(header.as_bytes()).unwrap();
                reader.get_mut().write_all(&response).unwrap();
            }
            methods
        });
        let workspace = tempfile::tempdir().unwrap();
        let setting = McpServerSettings {
            id: "remote".into(),
            enabled: true,
            program: String::new(),
            args: Vec::new(),
            cwd: None,
            credential_env_names: Vec::new(),
            env: Default::default(),
            url: Some(format!("http://{address}/mcp")),
            http_bearer_token_env_name: None,
            http_oauth: None,
            http_env_headers: Vec::new(),
        };
        let runtime = discover_mcp_runtime(
            &[setting],
            workspace.path(),
            Arc::new(LocalTools::new()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(runtime.mcp_tool_names(), &["mcp__remote__echo".to_owned()]);
        assert!(runtime.mcp_resume_identity_digest().is_none());
        let output = runtime
            .tools()
            .execute(
                &ToolCall {
                    id: "call-1".into(),
                    name: "mcp__remote__echo".into(),
                    arguments: json!({}),
                },
                &ToolContext {
                    workspace: workspace.path().to_path_buf(),
                    cancel: CancellationToken::new(),
                },
            )
            .await
            .unwrap();
        assert!(output.content.contains("remote-ok"));
        runtime.shutdown().await.unwrap();
        assert_eq!(
            server.join().unwrap(),
            ["server/discover", "tools/list", "tools/call"]
        );
    }

    #[tokio::test]
    async fn legacy_list_change_notification_blocks_stale_tool_calls() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = Arc::clone(&calls);
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "legacy MCP fixture timed out");
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("legacy MCP accept failed: {error}"),
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(socket);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let method = first.split_whitespace().next().unwrap().to_owned();
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).unwrap();
                let request: serde_json::Value = if body.is_empty() {
                    json!({})
                } else {
                    serde_json::from_slice(&body).unwrap()
                };
                if method == "GET" {
                    let body = "id: catalog-1\nretry: 500\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n";
                    write!(
                        reader.get_mut(),
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .unwrap();
                    reader.get_mut().flush().unwrap();
                    continue;
                }
                if method == "DELETE" {
                    write!(
                        reader.get_mut(),
                        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                    reader.get_mut().flush().unwrap();
                    break;
                }
                let rpc_method = request["method"].as_str().unwrap();
                let (status, result) = match rpc_method {
                    "server/discover" => (
                        "400 Bad Request",
                        json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"Method not found"}}),
                    ),
                    "initialize" => (
                        "200 OK",
                        json!({"jsonrpc":"2.0","id":request["id"],"result":{"protocolVersion":"2025-11-25","serverInfo":{"name":"legacy fixture","version":"1"},"capabilities":{"tools":{}}}}),
                    ),
                    "notifications/initialized" => ("202 Accepted", json!({})),
                    "tools/list" => (
                        "200 OK",
                        json!({"jsonrpc":"2.0","id":request["id"],"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}}),
                    ),
                    "tools/call" => {
                        server_calls.fetch_add(1, Ordering::SeqCst);
                        (
                            "200 OK",
                            json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[{"type":"text","text":"unexpected dispatch"}]}}),
                        )
                    }
                    other => panic!("unexpected legacy MCP method {other}"),
                };
                let bytes = if status == "202 Accepted" {
                    Vec::new()
                } else {
                    serde_json::to_vec(&result).unwrap()
                };
                write!(
                    reader.get_mut(),
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nMcp-Session-Id: legacy-session\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                )
                .unwrap();
                reader.get_mut().write_all(&bytes).unwrap();
                reader.get_mut().flush().unwrap();
            }
        });
        let workspace = tempfile::tempdir().unwrap();
        let runtime = discover_mcp_runtime(
            &[McpServerSettings {
                id: "legacy".into(),
                enabled: true,
                program: String::new(),
                args: Vec::new(),
                cwd: None,
                credential_env_names: Vec::new(),
                env: Default::default(),
                url: Some(format!("http://{address}/mcp")),
                http_bearer_token_env_name: None,
                http_oauth: None,
                http_env_headers: Vec::new(),
            }],
            workspace.path(),
            Arc::new(LocalTools::new()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !runtime.tools.is_server_catalog_invalidated("legacy") {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let output = runtime
            .tools()
            .execute(
                &ToolCall {
                    id: "call-stale".into(),
                    name: "mcp__legacy__echo".into(),
                    arguments: json!({}),
                },
                &ToolContext {
                    workspace: workspace.path().to_path_buf(),
                    cancel: CancellationToken::new(),
                },
            )
            .await
            .unwrap();
        assert!(output.is_error);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        runtime.shutdown().await.unwrap();
        server.join().unwrap();
    }

    #[test]
    fn local_only_runtime_skips_disabled_servers_and_reports_no_mcp_names() {
        let workspace = tempfile::tempdir().unwrap();
        let disabled = McpServerSettings {
            id: "disabled".to_owned(),
            enabled: false,
            program: "not-an-installed-program".to_owned(),
            args: Vec::new(),
            cwd: None,
            credential_env_names: Vec::new(),
            env: Default::default(),
            url: None,
            http_bearer_token_env_name: None,
            http_oauth: None,
            http_env_headers: Vec::new(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(discover_mcp_runtime(
                &[disabled],
                workspace.path(),
                Arc::new(LocalTools::new()),
                &CancellationToken::new(),
            ))
            .unwrap();
        assert!(runtime.mcp_tool_names().is_empty());
        assert_eq!(runtime.tools().definitions().len(), 3);
    }

    #[tokio::test]
    async fn cancellation_before_start_prevents_discovery() {
        let workspace = tempfile::tempdir().unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = discover_mcp_runtime(
            &[],
            workspace.path(),
            Arc::new(LocalTools::new()),
            &cancellation,
        )
        .await;
        assert!(result.is_err_and(|error| error.contains("cancelado")));
    }

    #[tokio::test]
    async fn runtime_cancellation_during_legacy_initialize_waits_for_partial_session_delete() {
        for delete_succeeds in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let (initialized_tx, initialized_rx) = tokio::sync::oneshot::channel();
            let (delete_tx, delete_rx) = tokio::sync::oneshot::channel();
            let (release_delete, release_rx) = std::sync::mpsc::channel();
            let server = thread::spawn(move || {
                let mut methods = Vec::new();
                let mut held_initialized = None;
                let mut initialized_tx = Some(initialized_tx);
                let mut delete_tx = Some(delete_tx);
                let deadline = Instant::now() + Duration::from_secs(10);
                for index in 0..4 {
                    let stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(
                                    Instant::now() < deadline,
                                    "missing partial-session cleanup"
                                );
                                thread::sleep(Duration::from_millis(5));
                            }
                            Err(error) => panic!("HTTP fixture accept failed: {error}"),
                        }
                    };
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let http_method = line.split_whitespace().next().unwrap().to_owned();
                    let mut headers = std::collections::BTreeMap::new();
                    loop {
                        line.clear();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':') {
                            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
                        }
                    }
                    let content_length = headers
                        .get("content-length")
                        .map_or(0, |value| value.parse().unwrap());
                    let mut body = vec![0; content_length];
                    reader.read_exact(&mut body).unwrap();
                    let request: serde_json::Value = if body.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::from_slice(&body).unwrap()
                    };
                    let expected = [
                        "server/discover",
                        "initialize",
                        "notifications/initialized",
                        "DELETE",
                    ][index];
                    if index == 3 {
                        assert_eq!(http_method, "DELETE");
                        assert!(request.is_null());
                    } else {
                        assert_eq!(http_method, "POST");
                        assert_eq!(request["method"], expected);
                    }
                    methods.push(expected.to_owned());
                    if index >= 2 {
                        assert_eq!(headers["mcp-session-id"], "runtime-partial");
                        assert_eq!(headers["mcp-protocol-version"], "2025-06-18");
                    }
                    if index == 2 {
                        held_initialized = Some(reader);
                        initialized_tx.take().unwrap().send(()).unwrap();
                        continue;
                    }
                    let (status, body, session) = match index {
                        0 => (
                            "400 Bad Request",
                            json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"Method not found"}}),
                            "",
                        ),
                        1 => (
                            "200 OK",
                            json!({"jsonrpc":"2.0","id":request["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"partial runtime","version":"1"},"capabilities":{"tools":{}}}}),
                            "Mcp-Session-Id: runtime-partial\r\n",
                        ),
                        3 => {
                            delete_tx.take().unwrap().send(()).unwrap();
                            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                            (
                                if delete_succeeds {
                                    "204 No Content"
                                } else {
                                    "503 Service Unavailable"
                                },
                                serde_json::Value::Null,
                                "",
                            )
                        }
                        _ => unreachable!(),
                    };
                    let bytes = if body.is_null() {
                        Vec::new()
                    } else {
                        serde_json::to_vec(&body).unwrap()
                    };
                    write!(reader.get_mut(), "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{session}Content-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
                    reader.get_mut().write_all(&bytes).unwrap();
                    reader.get_mut().flush().unwrap();
                }
                drop(held_initialized);
                methods
            });
            let workspace = tempfile::tempdir().unwrap();
            let workspace_path = workspace.path().to_path_buf();
            let preparation = McpToolPreparation::new(vec![McpServerSettings {
                id: "partial".into(),
                enabled: true,
                program: String::new(),
                args: Vec::new(),
                cwd: None,
                credential_env_names: Vec::new(),
                env: Default::default(),
                url: Some(format!("http://{address}/mcp")),
                http_bearer_token_env_name: None,
                http_oauth: None,
                http_env_headers: Vec::new(),
            }]);
            let cancellation = CancellationToken::new();
            let task_token = cancellation.clone();
            let startup = tokio::spawn(async move {
                preparation
                    .prepare(&workspace_path, Arc::new(LocalTools::new()), &task_token)
                    .await
            });
            initialized_rx.await.unwrap();
            cancellation.cancel();
            tokio::time::timeout(Duration::from_secs(5), delete_rx)
                .await
                .unwrap()
                .unwrap();
            assert!(
                !startup.is_finished(),
                "runtime dropped connect before cleanup acknowledgement"
            );
            release_delete.send(()).unwrap();
            let error = startup.await.unwrap().err().unwrap();
            if delete_succeeds {
                assert!(matches!(error, AgentError::Cancelled));
            } else {
                assert!(matches!(error, AgentError::Runtime(message)
                    if message.contains("request cancelled")
                        && message.contains("DELETE returned HTTP 503")
                        && message.contains("remote session may remain active")));
            }
            assert_eq!(
                server.join().unwrap(),
                [
                    "server/discover",
                    "initialize",
                    "notifications/initialized",
                    "DELETE"
                ]
            );
        }
    }

    #[cfg(windows)]
    fn mock_server(script_path: &Path, stopped_path: &Path, id: &str) -> McpServerSettings {
        McpServerSettings {
            id: id.to_owned(),
            enabled: true,
            program: "powershell.exe".to_owned(),
            args: vec![
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-ExecutionPolicy".to_owned(),
                "Bypass".to_owned(),
                "-File".to_owned(),
                script_path.to_string_lossy().into_owned(),
                stopped_path.to_string_lossy().into_owned(),
            ],
            cwd: None,
            credential_env_names: Vec::new(),
            env: Default::default(),
            url: None,
            http_bearer_token_env_name: None,
            http_oauth: None,
            http_env_headers: Vec::new(),
        }
    }

    #[cfg(windows)]
    fn write_mock_script(path: &Path) {
        let script = r#"
param([string]$stopFile)
while ($null -ne ($line = [Console]::In.ReadLine())) {
    $request = $line | ConvertFrom-Json
    if ($request.method -eq 'initialize') {
        $result = @{ protocolVersion = '2025-11-25'; capabilities = @{ tools = @{} }; serverInfo = @{ name = 'mock'; version = '1' } }
        [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $request.id; result = $result } | ConvertTo-Json -Compress -Depth 20))
    } elseif ($request.method -eq 'tools/list') {
        $result = @{ tools = @(@{ name = 'echo'; inputSchema = @{ type = 'object' } }) }
        [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $request.id; result = $result } | ConvertTo-Json -Compress -Depth 20))
    } elseif ($request.method -eq 'tools/call') {
        $result = @{ content = @(@{ type = 'text'; text = 'mock-ok' }); isError = $false }
        [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $request.id; result = $result } | ConvertTo-Json -Compress -Depth 20))
    }
}
[System.IO.File]::WriteAllText($stopFile, 'stopped')
"#;
        fs::write(path, script).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn discovers_mock_stdio_server_routes_tool_and_closes_it() {
        let workspace = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("mock-mcp.ps1");
        let stopped = directory.path().join("stopped.txt");
        write_mock_script(&script);
        let settings = mock_server(&script, &stopped, "mock");
        let runtime = discover_mcp_runtime(
            &[settings],
            workspace.path(),
            Arc::new(LocalTools::new()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(runtime.mcp_tool_names(), &["mcp__mock__echo".to_owned()]);
        // A PowerShell script is an indirect launcher, even with an unchanged
        // tool schema. It remains usable live but not automatically resumable.
        assert!(runtime.mcp_resume_identity_digest().is_none());
        let output = runtime
            .tools()
            .execute(
                &ToolCall {
                    id: "call-1".to_owned(),
                    name: "mcp__mock__echo".to_owned(),
                    arguments: json!({}),
                },
                &ToolContext {
                    workspace: workspace.path().to_path_buf(),
                    cancel: CancellationToken::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(output.content, "mock-ok");
        runtime.shutdown().await.unwrap();
        assert_eq!(fs::read_to_string(stopped).unwrap(), "stopped");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn idle_stdio_list_change_invalidates_runtime_without_dispatch() {
        let workspace = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("changing-mcp.ps1");
        let stopped = directory.path().join("stopped.txt");
        let called = directory.path().join("stopped.txt.called");
        let source = r#"
param([string]$stopFile)
while ($null -ne ($line = [Console]::In.ReadLine())) {
    $request = $line | ConvertFrom-Json
    if ($request.method -eq 'initialize') {
        $result = @{ protocolVersion = '2025-11-25'; capabilities = @{ tools = @{} }; serverInfo = @{ name = 'changing'; version = '1' } }
        [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $request.id; result = $result } | ConvertTo-Json -Compress -Depth 20))
    } elseif ($request.method -eq 'tools/list') {
        $result = @{ tools = @(@{ name = 'echo'; inputSchema = @{ type = 'object' } }) }
        [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $request.id; result = $result } | ConvertTo-Json -Compress -Depth 20))
        Start-Sleep -Milliseconds 50
        [Console]::Out.WriteLine('{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}')
    } elseif ($request.method -eq 'tools/call') {
        [System.IO.File]::WriteAllText(($stopFile + '.called'), 'called')
    }
}
[System.IO.File]::WriteAllText($stopFile, 'stopped')
"#;
        fs::write(&script, source).unwrap();
        let runtime = discover_mcp_runtime(
            &[mock_server(&script, &stopped, "changing")],
            workspace.path(),
            Arc::new(LocalTools::new()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !runtime.tools.is_server_catalog_invalidated("changing") {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let output = runtime
            .tools()
            .execute(
                &ToolCall {
                    id: "call-1".to_owned(),
                    name: "mcp__changing__echo".to_owned(),
                    arguments: json!({}),
                },
                &ToolContext {
                    workspace: workspace.path().to_path_buf(),
                    cancel: CancellationToken::new(),
                },
            )
            .await
            .unwrap();
        assert!(output.is_error);
        runtime.shutdown().await.unwrap();
        assert_eq!(fs::read_to_string(stopped).unwrap(), "stopped");
        assert!(!called.exists());
    }

    #[tokio::test]
    async fn a_server_that_cannot_start_is_left_out_and_the_turn_is_told() {
        let workspace = tempfile::tempdir().unwrap();
        let missing = McpServerSettings {
            id: "missing".to_owned(),
            enabled: true,
            program: "openide_missing_mcp_fixture_executable_123456".to_owned(),
            args: Vec::new(),
            cwd: None,
            credential_env_names: Vec::new(),
            env: Default::default(),
            url: None,
            http_bearer_token_env_name: None,
            http_oauth: None,
            http_env_headers: Vec::new(),
        };
        let runtime = discover_mcp_runtime(&[missing], workspace.path(), Arc::new(LocalTools::new()), &CancellationToken::new())
            .await
            .unwrap();
        assert!(runtime.mcp_tool_names().is_empty());
        assert!(runtime.mcp_resume_identity_digest().is_none());
        let notices = PreparedToolSession::notices(&runtime);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].0, openide_core::NOTICE_MCP_UNAVAILABLE);
        assert!(notices[0].1.starts_with("missing\nNo se pudo iniciar MCP missing"), "{:?}", notices[0].1);
        runtime.shutdown().await.unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_server_that_fails_to_start_is_left_out_and_the_earlier_one_serves() {
        let workspace = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("mock-mcp.ps1");
        let stopped = directory.path().join("stopped.txt");
        write_mock_script(&script);
        let first = mock_server(&script, &stopped, "first");
        let second = McpServerSettings {
            id: "second".to_owned(),
            enabled: true,
            program: "openide_missing_mcp_fixture_executable_123456".to_owned(),
            args: Vec::new(),
            cwd: None,
            credential_env_names: Vec::new(),
            env: Default::default(),
            url: None,
            http_bearer_token_env_name: None,
            http_oauth: None,
            http_env_headers: Vec::new(),
        };
        let runtime = discover_mcp_runtime(
            &[first, second],
            workspace.path(),
            Arc::new(LocalTools::new()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(runtime.skipped.len(), 1);
        assert!(runtime.mcp_tool_names().iter().any(|name| name.starts_with("mcp__first__")));
        assert!(runtime.mcp_resume_identity_digest().is_none());
        runtime.shutdown().await.unwrap();
        assert_eq!(fs::read_to_string(stopped).unwrap(), "stopped");
    }

    #[cfg(windows)]
    #[test]
    fn runner_keeps_mcp_process_alive_through_tool_call_and_closes_before_terminal() {
        use openide_core::{AgentEventKind, AgentMessage, AgentRunRequest, TurnMode};
        use openide_harness::{
            AgentError, AgentRunner, ModelBackend, ModelOutput, PermissionChoice, ToolDefinition,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        struct CycleModel(AtomicUsize);

        #[async_trait]
        impl ModelBackend for CycleModel {
            fn model_id(&self) -> &str {
                "mcp-test-model"
            }

            async fn complete(
                &self,
                messages: &[AgentMessage],
                tools: &[ToolDefinition],
                _cancel: CancellationToken,
            ) -> Result<ModelOutput, AgentError> {
                assert!(tools.iter().any(|tool| tool.name == "mcp__mock__echo"));
                if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(ModelOutput {
                        content: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "mcp-call".into(),
                            name: "mcp__mock__echo".into(),
                            arguments: json!({}),
                        }],
                    })
                } else {
                    assert_eq!(messages.last().unwrap().content, "mock-ok");
                    Ok(ModelOutput {
                        content: "MCP completed".into(),
                        tool_calls: vec![],
                    })
                }
            }
        }

        let workspace = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("mock-mcp.ps1");
        let stopped = directory.path().join("stopped.txt");
        write_mock_script(&script);
        let runner = AgentRunner::new(
            Arc::new(CycleModel(AtomicUsize::new(0))),
            Arc::new(LocalTools::new()),
        )
        .with_tool_preparation(Arc::new(McpToolPreparation::new(vec![mock_server(
            &script, &stopped, "mock",
        )])));
        let handle = runner.start(AgentRunRequest {
            session_id: Some("mcp-runner-test".into()),
            sequence_start: 0,
            workspace: workspace.path().to_path_buf(),
            history: vec![],
            prompt: "Call MCP".into(),
            mode: TurnMode::Build,
        });
        let mut kinds = Vec::new();
        loop {
            let event = handle
                .events
                .recv_timeout(Duration::from_secs(10))
                .expect("MCP runner terminal event");
            let terminal = matches!(
                event.kind,
                AgentEventKind::TurnCompleted { .. }
                    | AgentEventKind::TurnFailed { .. }
                    | AgentEventKind::TurnCancelled | AgentEventKind::TurnSuperseded { .. }
            );
            if let AgentEventKind::PermissionRequested { request_id, .. } = &event.kind {
                assert!(handle.resolve_permission(request_id, PermissionChoice::Allow));
            }
            kinds.push(event.kind);
            if terminal {
                break;
            }
        }
        assert!(matches!(
            kinds.last().unwrap(),
            AgentEventKind::TurnCompleted { response, .. } if response == "MCP completed"
        ));
        let catalog_position = kinds
            .iter()
            .position(|kind| matches!(kind, AgentEventKind::ToolCatalogCommitted { .. }))
            .unwrap();
        let model_position = kinds
            .iter()
            .position(|kind| matches!(kind, AgentEventKind::ModelRequestStarted { .. }))
            .unwrap();
        assert!(catalog_position < model_position);
        assert_eq!(fs::read_to_string(stopped).unwrap(), "stopped");
    }
}
