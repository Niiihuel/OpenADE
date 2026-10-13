//! Explicit, temporary MCP inspection. Every client belongs to a background
//! worker and closes before the UI receives its result. This never executes a
//! tool or replaces a turn's frozen catalog.

use crate::mcp_oauth_controller::{discover_oauth_session_with_store, platform_token_store};
use crate::mcp_settings::{McpServerSettings, McpSettings};
use openide_harness::{
    McpHttpClient, McpServerInfo, McpStdioClient, McpTool, OAuthError, OAuthTokens, TokenKey,
    TokenStore,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

static NEXT_INSPECTION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct McpInspectionScope {
    pub request_id: u64,
    pub workspace: PathBuf,
    pub server: McpServerSettings,
}

impl McpInspectionScope {
    /// Both the selected workspace and the freshly loaded saved registration
    /// must still match. A cached UI row alone is never sufficient authority.
    pub fn matches_saved(&self, workspace: &Path, saved: &McpSettings) -> bool {
        workspace
            .canonicalize()
            .is_ok_and(|path| path == self.workspace)
            && saved.servers.iter().any(|server| server == &self.server)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct McpInspectionSnapshot {
    pub server_info: McpServerInfo,
    pub tools: Vec<McpTool>,
    pub observed_at_unix_ms: u128,
    /// Hash of received metadata, not a journal's execution catalog digest.
    pub metadata_digest: String,
    pub details_json: String,
    pub catalog_changed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum McpInspectionPhase {
    #[default]
    Idle,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
    Discarded,
}

/// How the last connection to a server went, by its id. Connections are
/// made for a turn or for an inspection, so this is the newest one seen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum McpServerStatus {
    Connected { tools: usize },
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct McpInspectionUiState {
    /// Kept across inspections; forgotten for a server whose settings change.
    pub last: std::collections::BTreeMap<String, McpServerStatus>,
    pub scope: Option<McpInspectionScope>,
    pub phase: McpInspectionPhase,
    pub snapshot: Option<McpInspectionSnapshot>,
    pub error: Option<String>,
    pub shutdown_error: Option<String>,
}

impl McpInspectionUiState {
    pub fn is_busy(&self) -> bool {
        matches!(
            self.phase,
            McpInspectionPhase::Running | McpInspectionPhase::Cancelling
        )
    }

    pub fn begin(&mut self, scope: McpInspectionScope) {
        *self = Self {
            last: std::mem::take(&mut self.last),
            scope: Some(scope),
            phase: McpInspectionPhase::Running,
            ..Self::default()
        };
    }

    pub fn apply_result(
        &mut self,
        result: McpInspectionResult,
        workspace: &Path,
        saved: &McpSettings,
    ) -> bool {
        if self.scope.as_ref() != Some(&result.scope) {
            return false;
        }
        if !result.scope.matches_saved(workspace, saved) {
            self.phase = McpInspectionPhase::Discarded;
            self.snapshot = None;
            self.error = Some("La configuración guardada o el proyecto cambió durante la inspección. Repetí la consulta para el alcance actual.".to_owned());
            self.shutdown_error = result.shutdown_error;
            return true;
        }
        self.phase = if result.cancelled {
            McpInspectionPhase::Cancelled
        } else if result.error.is_some() || result.shutdown_error.is_some() {
            McpInspectionPhase::Failed
        } else {
            McpInspectionPhase::Completed
        };
        self.snapshot = if result.cancelled {
            None
        } else {
            result.snapshot
        };
        self.error = result.error;
        self.shutdown_error = result.shutdown_error;
        // A cancelled inspection says nothing about the server.
        let status = match (self.phase, &self.snapshot) {
            (McpInspectionPhase::Completed, Some(snapshot)) => Some(McpServerStatus::Connected { tools: snapshot.tools.len() }),
            (McpInspectionPhase::Failed, _) => Some(McpServerStatus::Failed(
                self.error.clone().or_else(|| self.shutdown_error.clone()).unwrap_or_default(),
            )),
            _ => None,
        };
        if let Some(status) = status {
            self.last.insert(result.scope.server.id.clone(), status);
        }
        true
    }
}

pub(crate) struct McpInspectionResult {
    pub scope: McpInspectionScope,
    pub snapshot: Option<McpInspectionSnapshot>,
    pub error: Option<String>,
    pub shutdown_error: Option<String>,
    pub cancelled: bool,
}

#[derive(Clone, Copy)]
struct InspectionLimits {
    request_timeout: Duration,
    total_timeout: Duration,
    shutdown_timeout: Duration,
}

impl Default for InspectionLimits {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            total_timeout: Duration::from_secs(45),
            shutdown_timeout: Duration::from_secs(35),
        }
    }
}

pub(crate) struct McpInspectionController {
    scope: McpInspectionScope,
    cancellation: CancellationToken,
    results: Receiver<McpInspectionResult>,
    worker: Option<JoinHandle<()>>,
    terminal_seen: bool,
}

impl McpInspectionController {
    pub fn start(server: McpServerSettings, workspace: &Path) -> Result<Self, String> {
        Self::start_with_limits(server, workspace, InspectionLimits::default())
    }

    fn start_with_limits(
        server: McpServerSettings,
        workspace: &Path,
        limits: InspectionLimits,
    ) -> Result<Self, String> {
        let workspace = workspace
            .canonicalize()
            .map_err(|error| format!("Proyecto MCP no disponible: {error}"))?;
        McpSettings {
            servers: vec![server.clone()],
        }
        .validate(&workspace)?;
        if !server.enabled {
            return Err("Activá el servidor guardado antes de inspeccionarlo".to_owned());
        }
        let scope = McpInspectionScope {
            request_id: NEXT_INSPECTION.fetch_add(1, Ordering::Relaxed),
            workspace,
            server,
        };
        let worker_scope = scope.clone();
        let cancellation = CancellationToken::new();
        let worker_cancel = cancellation.clone();
        let (sender, results) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("mcp-inspection".to_owned())
            .spawn(move || {
                let result = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(inspect(worker_scope, &worker_cancel, limits)),
                    Err(_) => McpInspectionResult {
                        scope: worker_scope,
                        snapshot: None,
                        error: Some("No se pudo iniciar el worker de inspección MCP".to_owned()),
                        shutdown_error: None,
                        cancelled: worker_cancel.is_cancelled(),
                    },
                };
                let _ = sender.send(result);
            })
            .map_err(|_| "No se pudo iniciar la inspección MCP".to_owned())?;
        Ok(Self {
            scope,
            cancellation,
            results,
            worker: Some(worker),
            terminal_seen: false,
        })
    }

    pub fn scope(&self) -> &McpInspectionScope {
        &self.scope
    }

    /// Cancellation is nonblocking. The UI keeps polling until client shutdown
    /// finishes; dropping the controller also cancels without joining a live worker.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn try_recv(&mut self) -> Option<McpInspectionResult> {
        if self.terminal_seen {
            return None;
        }
        let result = match self.results.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(McpInspectionResult {
                scope: self.scope.clone(),
                snapshot: None,
                error: Some("El worker MCP terminó sin resultado".to_owned()),
                shutdown_error: None,
                cancelled: self.cancellation.is_cancelled(),
            }),
        };
        self.terminal_seen = result.is_some();
        result
    }
}

impl Drop for McpInspectionController {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(worker) = self.worker.take() {
            if worker.is_finished() {
                let _ = worker.join();
            }
            // A live worker retains its own token and client and shuts down in
            // the background. UI navigation never waits for a process or HTTP.
        }
    }
}

enum InspectionClient {
    Stdio(McpStdioClient),
    Http(McpHttpClient),
}

impl InspectionClient {
    async fn server_info(&self) -> McpServerInfo {
        match self {
            Self::Stdio(client) => client.server_info().clone(),
            Self::Http(client) => client.server_info().await,
        }
    }

    async fn list_tools(&self, cancellation: &CancellationToken) -> Result<Vec<McpTool>, String> {
        match self {
            Self::Stdio(client) => client.list_tools(cancellation).await,
            Self::Http(client) => client.list_tools(cancellation).await,
        }
        .map_err(|error| error.to_string())
    }

    fn catalog_changed(&self) -> bool {
        match self {
            Self::Stdio(client) => client.catalog_changed(),
            Self::Http(client) => client.catalog_changed(),
        }
    }

    async fn shutdown(&self) -> Result<(), String> {
        match self {
            Self::Stdio(client) => client.shutdown().await,
            Self::Http(client) => client.shutdown().await,
        }
        .map_err(|error| error.to_string())
    }
}

#[derive(Default)]
struct SecretValues {
    values: Vec<String>,
    bytes: usize,
    overflowed: bool,
}

#[derive(Clone, Default)]
struct InspectionSecrets(Arc<Mutex<SecretValues>>);

impl InspectionSecrets {
    fn add(&self, value: impl Into<String>) {
        let value = value.into();
        let mut values = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        if value.is_empty() || values.values.contains(&value) {
            return;
        }
        if values.values.len() == 256 || values.bytes.saturating_add(value.len()) > 8 * 1024 * 1024
        {
            values.overflowed = true;
            return;
        }
        values.bytes += value.len();
        values.values.push(value);
    }

    fn sanitize(&self, mut message: String) -> String {
        let values = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        if values.overflowed {
            return "El registro temporal de credenciales superó su límite; los diagnósticos recibidos no se conservan".to_owned();
        }
        for value in &values.values {
            message = message.replace(value, "[credencial]");
        }
        message
    }

    fn appears_in(&self, value: &Value) -> bool {
        let values = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        values.overflowed || contains_secret(value, &values.values)
    }
}

fn contains_secret(value: &Value, secrets: &[String]) -> bool {
    match value {
        Value::String(text) => secrets.iter().any(|secret| text.contains(secret)),
        Value::Array(values) => values.iter().any(|value| contains_secret(value, secrets)),
        Value::Object(values) => values.iter().any(|(key, value)| {
            secrets.iter().any(|secret| key.contains(secret)) || contains_secret(value, secrets)
        }),
        _ => false,
    }
}

/// This observer does not introduce persistence or change grant authority.
/// Every store operation, including refresh CAS, delegates to the same store.
struct InspectionTokenStore {
    inner: Arc<dyn TokenStore>,
    secrets: InspectionSecrets,
}

impl InspectionTokenStore {
    fn record(&self, tokens: &OAuthTokens) {
        self.secrets.add(tokens.access_token.clone());
        if let Some(refresh) = &tokens.refresh_token {
            self.secrets.add(refresh.clone());
        }
    }
}

#[async_trait::async_trait]
impl TokenStore for InspectionTokenStore {
    async fn load(&self, key: &TokenKey) -> Result<Option<OAuthTokens>, OAuthError> {
        let tokens = self.inner.load(key).await?;
        if let Some(tokens) = &tokens {
            self.record(tokens);
        }
        Ok(tokens)
    }

    async fn save(&self, key: &TokenKey, tokens: OAuthTokens) -> Result<(), OAuthError> {
        self.record(&tokens);
        self.inner.save(key, tokens).await
    }

    async fn delete(&self, key: &TokenKey) -> Result<(), OAuthError> {
        self.inner.delete(key).await
    }

    async fn resume_grant_identity(&self, key: &TokenKey) -> Result<Option<String>, OAuthError> {
        self.inner.resume_grant_identity(key).await
    }

    async fn save_refreshed(
        &self,
        key: &TokenKey,
        previous: &OAuthTokens,
        tokens: OAuthTokens,
    ) -> Result<(), OAuthError> {
        self.record(previous);
        self.record(&tokens);
        self.inner.save_refreshed(key, previous, tokens).await
    }

    async fn delete_if_current(
        &self,
        key: &TokenKey,
        previous: &OAuthTokens,
    ) -> Result<bool, OAuthError> {
        self.record(previous);
        self.inner.delete_if_current(key, previous).await
    }
}

async fn connect(
    scope: &McpInspectionScope,
    cancellation: &CancellationToken,
    limits: InspectionLimits,
    secrets: &InspectionSecrets,
) -> Result<InspectionClient, String> {
    let server = &scope.server;
    if server.url.is_some() {
        let mut config = server.runtime_http_config()?;
        config.request_timeout = limits.request_timeout;
        if let Some(token) = &config.bearer_token {
            secrets.add(token.clone());
        }
        if server.http_oauth.is_some() {
            let store: Arc<dyn TokenStore> = Arc::new(InspectionTokenStore {
                inner: platform_token_store()?,
                secrets: secrets.clone(),
            });
            let session = discover_oauth_session_with_store(server, cancellation, store).await?;
            McpHttpClient::connect_with_oauth(config, session, cancellation)
                .await
                .map(InspectionClient::Http)
                .map_err(|error| error.to_string())
        } else {
            McpHttpClient::connect(config, cancellation)
                .await
                .map(InspectionClient::Http)
                .map_err(|error| error.to_string())
        }
    } else {
        let mut config = server.runtime_config(&scope.workspace)?;
        config.request_timeout = limits.request_timeout;
        for name in &server.credential_env_names {
            if let Some(value) = config.env.get(std::ffi::OsStr::new(name)) {
                secrets.add(value.to_string_lossy().into_owned());
            }
        }
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err("Inspección MCP cancelada".to_owned()),
            result = McpStdioClient::connect(config) => result.map(InspectionClient::Stdio).map_err(|error| error.to_string()),
        }
    }
}

fn make_snapshot(
    info: McpServerInfo,
    tools: Vec<McpTool>,
    secrets: &InspectionSecrets,
) -> Result<McpInspectionSnapshot, String> {
    let metadata = json!({
        "serverInfo": { "protocolVersion": info.protocol_version, "name": info.name,
            "version": info.version, "capabilities": info.capabilities, "instructions": info.instructions },
        "tools": tools,
    });
    if secrets.appears_in(&metadata) {
        return Err("El servidor incluyó una credencial en sus metadatos. El listado y su huella no se conservan.".to_owned());
    }
    let bytes = serde_json::to_vec(&metadata)
        .map_err(|_| "No se pudieron representar los metadatos MCP".to_owned())?;
    let mut digest = Sha256::new();
    digest.update(b"openide-mcp-inspection-v1\0");
    digest.update(&bytes);
    Ok(McpInspectionSnapshot {
        server_info: info,
        tools,
        observed_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        metadata_digest: format!("{:x}", digest.finalize()),
        details_json: serde_json::to_string_pretty(&metadata)
            .map_err(|_| "No se pudieron representar los metadatos MCP".to_owned())?,
        catalog_changed: false,
    })
}

async fn inspect(
    scope: McpInspectionScope,
    cancellation: &CancellationToken,
    limits: InspectionLimits,
) -> McpInspectionResult {
    let mut result = McpInspectionResult {
        scope: scope.clone(),
        snapshot: None,
        error: None,
        shutdown_error: None,
        cancelled: false,
    };
    let deadline = tokio::time::Instant::now() + limits.total_timeout;
    let secrets = InspectionSecrets::default();
    // Cancelling a connect future by dropping it can skip a client's cleanup
    // after a legacy initialize has already created a remote session. Signal
    // its token first and allow the existing connect API to finish cleanup.
    let connect_cancel = cancellation.child_token();
    let (connected, exceeded_deadline) = {
        let connection = connect(&scope, &connect_cancel, limits, &secrets);
        tokio::pin!(connection);
        tokio::select! {
            result = &mut connection => (Some(result), false),
            _ = tokio::time::sleep_until(deadline) => {
                connect_cancel.cancel();
                (tokio::time::timeout(limits.shutdown_timeout, &mut connection).await.ok(), true)
            }
        }
    };
    let client = match connected {
        Some(Ok(client)) => client,
        Some(Err(error)) => {
            result.error = Some(secrets.sanitize(if exceeded_deadline {
                format!("La inspección MCP superó su tiempo límite. {error}")
            } else {
                error
            }));
            result.cancelled = cancellation.is_cancelled();
            return result;
        }
        None => {
            result.error = Some("La inspección MCP superó su tiempo límite".to_owned());
            result.shutdown_error = Some("La conexión no terminó dentro del plazo de cierre; no se pudo confirmar la liberación de una sesión remota parcial".to_owned());
            result.cancelled = cancellation.is_cancelled();
            return result;
        }
    };
    inspect_connected(scope, cancellation, limits, deadline, client, secrets).await
}

async fn inspect_connected(
    scope: McpInspectionScope,
    cancellation: &CancellationToken,
    limits: InspectionLimits,
    deadline: tokio::time::Instant,
    client: InspectionClient,
    secrets: InspectionSecrets,
) -> McpInspectionResult {
    let mut result = McpInspectionResult {
        scope,
        snapshot: None,
        error: None,
        shutdown_error: None,
        cancelled: false,
    };
    let pending_snapshot =
        match tokio::time::timeout_at(deadline, client.list_tools(cancellation)).await {
            Ok(Ok(tools)) => Some((client.server_info().await, tools)),
            Ok(Err(error)) => {
                result.error = Some(error);
                None
            }
            Err(_) => {
                result.error = Some("La inspección MCP superó su tiempo límite".to_owned());
                None
            }
        };
    result.shutdown_error = match tokio::time::timeout(limits.shutdown_timeout, client.shutdown()).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(_) => Some("El cierre MCP superó su tiempo límite; no se pudo confirmar la liberación de la conexión remota".to_owned()),
    };
    result.cancelled = cancellation.is_cancelled();
    if !result.cancelled {
        if let Some((info, tools)) = pending_snapshot {
            match make_snapshot(info, tools, &secrets) {
                Ok(mut snapshot) => {
                    snapshot.catalog_changed = client.catalog_changed();
                    result.snapshot = Some(snapshot);
                }
                Err(error) => result.error = Some(error),
            }
        }
    }
    result.error = result.error.map(|error| secrets.sanitize(error));
    result.shutdown_error = result.shutdown_error.map(|error| secrets.sanitize(error));
    result
}

#[cfg(test)]
#[path = "mcp_inspection_tests.rs"]
pub(crate) mod tests;
