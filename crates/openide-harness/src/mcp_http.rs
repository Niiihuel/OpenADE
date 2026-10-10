//! Bounded dual-era MCP Streamable HTTP client for tool discovery and calls.
//!
//! It prefers stateless 2026-07-28 and negotiates the 2025 handshake lifecycle
//! with older Streamable HTTP endpoints. Tool calls are never replayed after
//! an uncertain network outcome.

use crate::mcp::{mark_uncertain_tool_error, parse_initialize};
use crate::mcp_identity::McpHttpAuthIdentity;
use crate::mcp_oauth::OAuthSession;
use crate::mcp_tools::McpToolClient;
use crate::{
    McpCallResult, McpError, McpHttpServerIdentity, McpServerInfo, McpTool, MAX_MCP_MESSAGE_BYTES,
    MAX_MCP_TOOLS, MAX_MCP_TOOL_PAGES, MCP_PROTOCOL_VERSION,
};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, StatusCode, Url};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tokio::time::{timeout_at, Instant};
use tokio_util::sync::CancellationToken;

pub const MCP_HTTP_PROTOCOL_VERSION: &str = "2026-07-28";
const MAX_TOOL_BYTES: usize = 64 * 1024;
const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_SSE_EVENTS: usize = 256;
const MAX_SSE_RESUMES: usize = 8;
const MAX_SSE_CURSOR_BYTES: usize = 1024;
const DEFAULT_SSE_RETRY: Duration = Duration::from_millis(100);
const MAX_SSE_RETRY: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_SESSION_ID_BYTES: usize = 512;

#[derive(Clone)]
pub struct McpHttpConfig {
    pub url: String,
    /// Resolved only at launch; never saved in the MCP settings document.
    pub bearer_token: Option<String>,
    pub request_timeout: Duration,
    pub max_message_bytes: usize,
    pub client_name: String,
    pub client_version: String,
}

impl McpHttpConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            bearer_token: None,
            request_timeout: Duration::from_secs(30),
            max_message_bytes: MAX_MCP_MESSAGE_BYTES,
            client_name: "openide".to_owned(),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    pub fn validate(&self) -> Result<(), McpError> {
        self.validated_endpoint().map(|_| ())
    }

    fn validated_endpoint(&self) -> Result<(Url, Option<HeaderValue>), McpError> {
        let url = Url::parse(&self.url)
            .map_err(|error| McpError::Protocol(format!("invalid MCP HTTP URL: {error}")))?;
        if url.host_str().is_none()
            || url.fragment().is_some()
            || url.query().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(McpError::Protocol(
                "MCP HTTP URL needs a host and cannot contain credentials, query, or fragment"
                    .into(),
            ));
        }
        if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback_host(&url)) {
            return Err(McpError::Protocol(
                "MCP HTTP requires HTTPS, except for a loopback endpoint".into(),
            ));
        }
        if self.request_timeout.is_zero()
            || !(1024..=MAX_MCP_MESSAGE_BYTES).contains(&self.max_message_bytes)
            || self.client_name.is_empty()
            || self.client_version.is_empty()
        {
            return Err(McpError::Protocol(
                "invalid MCP HTTP limits or client identity".into(),
            ));
        }
        let auth = self
            .bearer_token
            .as_ref()
            .map(|token| {
                if token.is_empty()
                    || !token.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric()
                            || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'=')
                    })
                {
                    return Err(McpError::Protocol("invalid MCP HTTP bearer token".into()));
                }
                HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| McpError::Protocol("invalid MCP HTTP bearer token".into()))
                    .map(|mut value| {
                        value.set_sensitive(true);
                        value
                    })
            })
            .transpose()?;
        Ok((url, auth))
    }
}

pub(crate) fn is_loopback_host(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[derive(Clone)]
struct HeaderSpec {
    header: HeaderName,
    path: Vec<String>,
}

#[derive(Clone)]
struct CatalogEntry {
    tool: McpTool,
    headers: Vec<HeaderSpec>,
}

enum HttpEra {
    Modern,
    Legacy {
        version: RwLock<String>,
        session_id: RwLock<Option<String>>,
    },
}

/// State shared by each connection of one SSE stream. Only a blank line
/// dispatches an event and commits its cursor; a truncated frame is discarded.
struct SseDecoder {
    line: Vec<u8>,
    skip_lf: bool,
    data: Vec<u8>,
    has_data_line: bool,
    pending_id: Option<Option<String>>,
    pending_retry: Option<Duration>,
    last_event_id: Option<String>,
    retry: Duration,
    bytes: usize,
    events: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SseAccounting {
    Request,
    Notification,
}

impl SseDecoder {
    fn new() -> Self {
        Self {
            line: Vec::new(),
            skip_lf: false,
            data: Vec::new(),
            has_data_line: false,
            pending_id: None,
            pending_retry: None,
            last_event_id: None,
            retry: DEFAULT_SSE_RETRY,
            bytes: 0,
            events: 0,
        }
    }

    fn reset_connection(&mut self, accounting: SseAccounting) {
        self.line.clear();
        self.skip_lf = false;
        self.data.clear();
        self.has_data_line = false;
        self.pending_id = None;
        self.pending_retry = None;
        if accounting == SseAccounting::Notification {
            self.bytes = 0;
        }
    }

    fn push(
        &mut self,
        chunk: &[u8],
        limit: usize,
        accounting: SseAccounting,
    ) -> Result<Vec<Value>, McpError> {
        let mut values = Vec::new();
        for &byte in chunk {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    if accounting == SseAccounting::Request {
                        if self.bytes >= limit {
                            return Err(McpError::MessageTooLarge { limit });
                        }
                        self.bytes += 1;
                    }
                    continue;
                }
            }
            if self.bytes >= limit {
                return Err(McpError::MessageTooLarge { limit });
            }
            self.bytes += 1;
            if byte != b'\n' && byte != b'\r' {
                self.line.push(byte);
                if self.line.len() > limit {
                    return Err(McpError::MessageTooLarge { limit });
                }
                continue;
            }
            self.skip_lf = byte == b'\r';
            if self.line.is_empty() {
                if self.has_data_line || self.pending_id.is_some() || self.pending_retry.is_some() {
                    self.events += 1;
                    if self.events > MAX_SSE_EVENTS {
                        return Err(McpError::Protocol("too many MCP SSE events".into()));
                    }
                    if let Some(id) = self.pending_id.take() {
                        self.last_event_id = id;
                    }
                    if let Some(retry) = self.pending_retry.take() {
                        self.retry = retry;
                    }
                    if self.has_data_line && !self.data.is_empty() {
                        values.push(serde_json::from_slice(&self.data).map_err(|error| {
                            McpError::Protocol(format!("invalid MCP SSE JSON: {error}"))
                        })?);
                        if accounting == SseAccounting::Notification {
                            self.events = 0;
                        }
                    }
                    self.data.clear();
                    self.has_data_line = false;
                }
                if accounting == SseAccounting::Notification {
                    self.bytes = 0;
                }
            } else {
                let (field, raw_value) = match self.line.iter().position(|byte| *byte == b':') {
                    Some(colon) => (&self.line[..colon], &self.line[colon + 1..]),
                    None => (self.line.as_slice(), b"".as_slice()),
                };
                let value = raw_value.strip_prefix(b" ").unwrap_or(raw_value);
                match field {
                    b"data" => {
                        if self.has_data_line {
                            self.data.push(b'\n');
                        }
                        self.has_data_line = true;
                        self.data.extend_from_slice(value);
                    }
                    b"id" => {
                        if value.len() > MAX_SSE_CURSOR_BYTES || value.contains(&0) {
                            return Err(McpError::Protocol("invalid MCP SSE event ID".into()));
                        }
                        let id = std::str::from_utf8(value)
                            .map_err(|_| McpError::Protocol("invalid MCP SSE event ID".into()))?;
                        self.pending_id = Some((!id.is_empty()).then(|| id.to_owned()));
                    }
                    b"retry" if !value.is_empty() && value.iter().all(u8::is_ascii_digit) => {
                        if let Ok(milliseconds) = std::str::from_utf8(value).unwrap().parse::<u64>()
                        {
                            let retry = Duration::from_millis(milliseconds);
                            if retry > MAX_SSE_RETRY {
                                return Err(McpError::Protocol(
                                    "MCP SSE retry delay exceeds supported limit".into(),
                                ));
                            }
                            self.pending_retry = Some(retry);
                        }
                    }
                    _ => {}
                }
            }
            self.line.clear();
        }
        Ok(values)
    }
}

enum SseReadOutcome {
    Final(Value),
    Disconnected { received_message: bool },
}

pub struct McpHttpClient {
    client: Client,
    endpoint: Url,
    auth: Option<HeaderValue>,
    oauth: Option<Arc<OAuthSession>>,
    resume_authorization: Option<McpHttpAuthIdentity>,
    config: McpHttpConfig,
    server_info: RwLock<McpServerInfo>,
    era: HttpEra,
    legacy_handshake: RwLock<()>,
    next_id: AtomicU64,
    catalog: RwLock<BTreeMap<String, CatalogEntry>>,
    catalog_discovered: AtomicBool,
    catalog_changed: AtomicBool,
    closed: CancellationToken,
}

impl McpHttpClient {
    pub async fn connect(
        config: McpHttpConfig,
        cancellation: &CancellationToken,
    ) -> Result<Self, McpError> {
        Self::connect_inner(config, None, cancellation).await
    }

    /// Connect with a token bound to this resource/issuer/client registration.
    /// Refresh happens before each HTTP dispatch; a rejected tools/call is
    /// never retried automatically, because the remote effect may be unknown.
    pub async fn connect_with_oauth(
        config: McpHttpConfig,
        oauth: Arc<OAuthSession>,
        cancellation: &CancellationToken,
    ) -> Result<Self, McpError> {
        Self::connect_inner(config, Some(oauth), cancellation).await
    }

    async fn connect_inner(
        config: McpHttpConfig,
        oauth: Option<Arc<OAuthSession>>,
        cancellation: &CancellationToken,
    ) -> Result<Self, McpError> {
        let (endpoint, auth) = config.validated_endpoint()?;
        if let Some(session) = &oauth {
            if auth.is_some() || endpoint.as_str() != session.resource_url() {
                return Err(McpError::Protocol(
                    "MCP OAuth resource mismatch or conflicting bearer token".into(),
                ));
            }
        }
        let resume_authorization = if let Some(session) = &oauth {
            session
                .resume_authorization_identity(cancellation)
                .await
                .ok()
                .flatten()
                .map(McpHttpAuthIdentity::OAuth)
        } else if let Some(token) = config.bearer_token.as_ref() {
            #[cfg(windows)]
            {
                crate::mcp_oauth_windows::bearer_resume_identity(token.clone())
                    .await
                    .map(McpHttpAuthIdentity::Bearer)
            }
            #[cfg(not(windows))]
            {
                let _ = token;
                None
            }
        } else {
            None
        };
        // A protocol NACK is not proof that a POST, especially tools/call,
        // had no effect. Reqwest otherwise retries such requests by default.
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        // System proxy discovery can route localhost through HTTP_PROXY and
        // expose the bearer token. A loopback MCP endpoint must stay local.
        if is_loopback_host(&endpoint) {
            builder = builder.no_proxy();
        }
        let client = builder.build().map_err(http_error)?;
        let mut connection = Self {
            client,
            endpoint,
            auth,
            oauth,
            resume_authorization,
            config,
            server_info: RwLock::new(McpServerInfo {
                protocol_version: MCP_HTTP_PROTOCOL_VERSION.to_owned(),
                name: String::new(),
                version: String::new(),
                capabilities: json!({}),
                instructions: None,
            }),
            era: HttpEra::Modern,
            legacy_handshake: RwLock::new(()),
            next_id: AtomicU64::new(1),
            catalog: RwLock::new(BTreeMap::new()),
            catalog_discovered: AtomicBool::new(false),
            catalog_changed: AtomicBool::new(false),
            closed: CancellationToken::new(),
        };
        match connection
            .request("server/discover", json!({}), None, &[], cancellation)
            .await
        {
            Ok(result) => *connection.server_info.write().await = parse_discovery(result)?,
            Err(McpError::LegacyHttpRequired) => {
                connection.era = HttpEra::Legacy {
                    version: RwLock::new(String::new()),
                    session_id: RwLock::new(None),
                };
                connection.initialize_legacy(cancellation).await?;
            }
            Err(error) => return Err(error),
        }
        Ok(connection)
    }

    async fn authorization_header(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Option<HeaderValue>, McpError> {
        let Some(oauth) = &self.oauth else {
            return Ok(self.auth.clone());
        };
        let token = oauth.access_token(cancellation).await.map_err(|error| {
            if self.resume_authorization.is_some() {
                McpError::AuthorizationChanged
            } else {
                McpError::Protocol(format!("MCP OAuth: {error}"))
            }
        })?;
        if let Some(McpHttpAuthIdentity::OAuth(expected)) = &self.resume_authorization {
            let current = oauth
                .resume_authorization_identity(cancellation)
                .await
                .map_err(|_| McpError::AuthorizationChanged)?;
            if current.as_deref() != Some(expected.as_str()) {
                return Err(McpError::AuthorizationChanged);
            }
        }
        let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| McpError::Protocol("invalid OAuth bearer token".into()))?;
        header.set_sensitive(true);
        Ok(Some(header))
    }

    pub async fn server_info(&self) -> McpServerInfo {
        self.server_info.read().await.clone()
    }

    /// Durable pre-dispatch identity for a stateless remote HTTPS endpoint.
    /// Authenticated connections require a credential-store grant revision or
    /// a keyed bearer fingerprint; no token value is journaled. A changed
    /// authorization, loopback HTTP, legacy session, or incomplete discovery
    /// cannot authorize automatic MCP recovery.
    pub async fn resume_server_identity(
        &self,
        id: impl Into<String>,
    ) -> Option<McpHttpServerIdentity> {
        if !self.is_modern()
            || self.endpoint.scheme() != "https"
            || is_loopback_host(&self.endpoint)
            || self.closed.is_cancelled()
            || self.catalog_changed()
            || !self.catalog_discovered.load(Ordering::Acquire)
        {
            return None;
        }
        match (
            self.auth.is_some(),
            self.oauth.is_some(),
            &self.resume_authorization,
        ) {
            (false, false, None)
            | (true, false, Some(McpHttpAuthIdentity::Bearer(_)))
            | (false, true, Some(McpHttpAuthIdentity::OAuth(_))) => {}
            _ => return None,
        }
        if self.auth.is_some() != self.config.bearer_token.is_some() {
            return None;
        }
        if let (Some(oauth), Some(McpHttpAuthIdentity::OAuth(expected))) =
            (&self.oauth, &self.resume_authorization)
        {
            let current = oauth
                .resume_authorization_identity(&CancellationToken::new())
                .await
                .ok()
                .flatten();
            if current.as_deref() != Some(expected.as_str()) {
                return None;
            }
        }
        #[cfg(windows)]
        if let (Some(token), Some(McpHttpAuthIdentity::Bearer(expected))) =
            (&self.config.bearer_token, &self.resume_authorization)
        {
            if crate::mcp_oauth_windows::bearer_resume_identity(token.clone())
                .await
                .as_deref()
                != Some(expected.as_str())
            {
                return None;
            }
        }
        let info = self.server_info.read().await.clone();
        if info.name.is_empty() || info.version.is_empty() {
            return None;
        }
        Some(McpHttpServerIdentity::new(
            id.into(),
            self.endpoint.to_string(),
            (
                self.config.client_name.clone(),
                self.config.client_version.clone(),
            ),
            (
                self.config.request_timeout.as_millis(),
                self.config.max_message_bytes,
            ),
            self.resume_authorization.clone(),
            info,
        ))
    }

    /// True for the negotiated 2025 Streamable HTTP transport, whose GET
    /// notification stream is optional. False for stateless MCP 2026-07-28.
    pub fn supports_legacy_notifications(&self) -> bool {
        !self.is_modern()
    }

    /// Sticky for this connection, including when a notification receiver is
    /// full or absent. A new turn opens a new client and discovers a new tool
    /// snapshot; this client never silently replaces an advertised catalog.
    pub fn catalog_changed(&self) -> bool {
        self.catalog_changed.load(Ordering::Acquire)
    }

    pub async fn shutdown(&self) -> Result<(), McpError> {
        let _guard = match &self.era {
            HttpEra::Legacy { .. } => Some(self.legacy_handshake.write().await),
            HttpEra::Modern => None,
        };
        let result = self.delete_legacy_session().await;
        self.closed.cancel();
        result
    }

    async fn delete_legacy_session(&self) -> Result<(), McpError> {
        let HttpEra::Legacy { session_id, .. } = &self.era else {
            return Ok(());
        };
        if session_id.read().await.is_none() {
            return Ok(());
        }
        // Cleanup remains possible after the handshake caller cancels. Bound
        // authorization (which may refresh OAuth) and DELETE together, so a
        // failed connection cannot wait indefinitely before returning its error.
        let cleanup = CancellationToken::new();
        timeout_at(Instant::now() + self.config.request_timeout, async {
            let headers = self.legacy_headers(&cleanup).await?;
            let response = self
                .client
                .delete(self.endpoint.clone())
                .headers(headers)
                .send()
                .await
                .map_err(http_error)?;
            if !response.status().is_success()
                && response.status() != StatusCode::METHOD_NOT_ALLOWED
                && response.status() != StatusCode::NOT_FOUND
            {
                return Err(McpError::Protocol(format!(
                    "MCP session DELETE returned HTTP {}",
                    response.status()
                )));
            }
            *session_id.write().await = None;
            Ok(())
        })
        .await
        .map_err(|_| McpError::Timeout)?
    }

    fn is_modern(&self) -> bool {
        matches!(&self.era, HttpEra::Modern)
    }

    async fn initialize_legacy(&self, cancellation: &CancellationToken) -> Result<(), McpError> {
        let _guard = self.legacy_handshake.write().await;
        let HttpEra::Legacy {
            version,
            session_id,
        } = &self.era
        else {
            return Err(McpError::Protocol(
                "legacy HTTP initialization on modern client".into(),
            ));
        };
        *session_id.write().await = None;
        // Until the response negotiates a supported version, cleanup uses the
        // version offered by initialize. The initialize POST itself omits it.
        *version.write().await = MCP_PROTOCOL_VERSION.to_owned();
        self.catalog_discovered.store(false, Ordering::Release);
        let mut catalog = self.catalog.write().await;
        if !catalog.is_empty() {
            // A replacement session has not advertised the same tools, even
            // when the old session never sent list_changed.
            self.catalog_changed.store(true, Ordering::Release);
        }
        catalog.clear();
        drop(catalog);
        let handshake = self.finish_legacy_handshake(cancellation).await;
        if let Err(handshake) = handshake {
            // We hold the handshake lock, so calling shutdown would deadlock.
            // A partial session is never exposed as a usable client, including
            // a failed replacement session after SessionExpired.
            let cleanup = self.delete_legacy_session().await;
            self.closed.cancel();
            return Err(match cleanup {
                Ok(()) => handshake,
                Err(cleanup) => McpError::HandshakeCleanupFailed {
                    handshake: Box::new(handshake),
                    cleanup: Box::new(cleanup),
                },
            });
        }
        Ok(())
    }

    async fn finish_legacy_handshake(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), McpError> {
        let (result, _) = self
            .request_with_session(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": self.config.client_name,
                        "version": self.config.client_version,
                    }
                }),
                None,
                &[],
                cancellation,
            )
            .await?;
        let info = parse_initialize(result)?;
        if !matches!(
            info.protocol_version.as_str(),
            "2025-11-25" | "2025-06-18" | "2025-03-26"
        ) {
            return Err(McpError::Protocol(format!(
                "HTTP endpoint negotiated unsupported MCP version {}",
                info.protocol_version
            )));
        }
        if let HttpEra::Legacy { version, .. } = &self.era {
            *version.write().await = info.protocol_version.clone();
        }
        *self.server_info.write().await = info;
        self.send_initialized(cancellation).await
    }

    async fn send_initialized(&self, cancellation: &CancellationToken) -> Result<(), McpError> {
        self.send_legacy_message(
            json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
            cancellation,
            self.config.request_timeout,
        )
        .await
    }

    async fn legacy_headers(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<HeaderMap, McpError> {
        let HttpEra::Legacy {
            version,
            session_id,
        } = &self.era
        else {
            return Err(McpError::Protocol("legacy headers on modern client".into()));
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("mcp-protocol-version"),
            HeaderValue::from_str(&version.read().await)
                .map_err(|_| McpError::Protocol("invalid MCP protocol version".into()))?,
        );
        if let Some(session) = session_id.read().await.as_ref() {
            headers.insert(
                HeaderName::from_static("mcp-session-id"),
                session_header_value(session)?,
            );
        }
        if let Some(auth) = self.authorization_header(cancellation).await? {
            headers.insert(AUTHORIZATION, auth);
        }
        Ok(headers)
    }

    async fn legacy_get(
        &self,
        cursor: Option<&str>,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<Option<reqwest::Response>, McpError> {
        let mut headers = self.legacy_headers(cancellation).await?;
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.remove(CONTENT_TYPE);
        if let Some(cursor) = cursor {
            let mut value = HeaderValue::from_str(cursor)
                .map_err(|_| McpError::Protocol("invalid MCP SSE cursor header".into()))?;
            value.set_sensitive(true);
            headers.insert(HeaderName::from_static("last-event-id"), value);
        }
        let send = self
            .client
            .get(self.endpoint.clone())
            .headers(headers)
            .send();
        let response = tokio::select! {
            biased;
            _ = self.closed.cancelled() => return Err(McpError::Closed),
            _ = cancellation.cancelled() => return Err(McpError::Cancelled),
            result = timeout_at(deadline, send) => result.map_err(|_| McpError::Timeout)?.map_err(http_error)?,
        };
        match response.status() {
            StatusCode::METHOD_NOT_ALLOWED => return Ok(None),
            StatusCode::NOT_FOUND => {
                if let HttpEra::Legacy { session_id, .. } = &self.era {
                    if session_id.read().await.is_some() {
                        return Err(McpError::SessionExpired);
                    }
                }
            }
            StatusCode::OK => {}
            status => {
                return Err(McpError::Protocol(format!(
                    "MCP legacy SSE GET returned HTTP {status}"
                )));
            }
        }
        if response.status() != StatusCode::OK {
            return Err(McpError::Protocol(format!(
                "MCP legacy SSE GET returned HTTP {}",
                response.status()
            )));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if !content_type.eq_ignore_ascii_case("text/event-stream") {
            return Err(McpError::Protocol(
                "MCP legacy SSE GET requires text/event-stream".into(),
            ));
        }
        Ok(Some(response))
    }

    async fn notification_get(
        &self,
        cursor: Option<&str>,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<Option<reqwest::Response>, McpError> {
        // A request-scoped resume already holds this read lock. An independent
        // listener acquires it only while starting each GET, so a concurrent
        // session reinitialization cannot produce mixed version/session headers.
        let _guard = self.legacy_handshake.read().await;
        self.legacy_get(cursor, deadline, cancellation).await
    }

    async fn notification_get_recovering_session(
        &self,
        cursor: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<(Option<reqwest::Response>, bool), McpError> {
        let deadline = Instant::now() + self.config.request_timeout;
        match self.notification_get(cursor, deadline, cancellation).await {
            Err(McpError::SessionExpired) => {
                // An old cursor cannot identify a stream in the new session.
                // Reinitialize without replaying any tools/call and open a new
                // independent notification stream.
                self.initialize_legacy(cancellation).await?;
                let new_deadline = Instant::now() + self.config.request_timeout;
                self.notification_get(None, new_deadline, cancellation)
                    .await
                    .map(|response| (response, true))
            }
            other => other.map(|response| (response, false)),
        }
    }

    /// Listen to the optional legacy GET notification stream. A 405 response
    /// means the server does not provide one. The caller must start this task;
    /// the app runtime does not yet wire unsolicited notifications into its UI.
    /// Each GET opening has request_timeout; an established healthy stream
    /// stays open until cancelled. Each frame is byte-bounded and at most
    /// MAX_SSE_RESUMES consecutive disconnects without a message are allowed.
    /// A lost stream without a cursor fails rather than silently losing data.
    pub async fn listen_legacy_notifications(
        &self,
        sender: &mpsc::Sender<Value>,
        cancellation: &CancellationToken,
    ) -> Result<(), McpError> {
        if self.is_modern() {
            return Err(McpError::Protocol(
                "MCP 2026-07-28 does not support notification GET streams".into(),
            ));
        }
        let mut decoder = SseDecoder::new();
        let (initial, _) = self
            .notification_get_recovering_session(None, cancellation)
            .await?;
        let Some(mut response) = initial else {
            return Ok(());
        };
        let mut consecutive_resumes = 0usize;
        loop {
            let received_message = match self
                .consume_sse(
                    &mut response,
                    None,
                    Some(sender),
                    &mut decoder,
                    None,
                    cancellation,
                )
                .await?
            {
                SseReadOutcome::Final(_) => unreachable!("notification stream has no response"),
                SseReadOutcome::Disconnected { received_message } => received_message,
            };
            if received_message {
                consecutive_resumes = 0;
            }
            if consecutive_resumes == MAX_SSE_RESUMES {
                return Err(McpError::Protocol("MCP SSE resume limit reached".into()));
            }
            consecutive_resumes += 1;
            let cursor = decoder.last_event_id.as_deref().ok_or_else(|| {
                McpError::Protocol(
                    "MCP SSE notification stream ended without a resume cursor".into(),
                )
            })?;
            self.wait_sse_retry(decoder.retry, None, cancellation)
                .await?;
            let (next, new_session) = self
                .notification_get_recovering_session(Some(cursor), cancellation)
                .await?;
            if new_session {
                decoder = SseDecoder::new();
                consecutive_resumes = 0;
            }
            response =
                next.ok_or_else(|| McpError::Protocol("MCP SSE resume GET was rejected".into()))?;
        }
    }

    async fn send_legacy_message(
        &self,
        message: Value,
        cancellation: &CancellationToken,
        max_duration: Duration,
    ) -> Result<(), McpError> {
        let body = serde_json::to_vec(&message)
            .map_err(|error| McpError::Protocol(format!("cannot encode MCP message: {error}")))?;
        if body.len() > self.config.max_message_bytes {
            return Err(McpError::MessageTooLarge {
                limit: self.config.max_message_bytes,
            });
        }
        let deadline = Instant::now() + max_duration;
        let headers = tokio::select! {
            biased;
            _ = self.closed.cancelled() => return Err(McpError::Closed),
            _ = cancellation.cancelled() => return Err(McpError::Cancelled),
            result = timeout_at(deadline, self.legacy_headers(cancellation)) => {
                result.map_err(|_| McpError::Timeout)??
            }
        };
        let send = self
            .client
            .post(self.endpoint.clone())
            .headers(headers)
            .body(body)
            .send();
        let response = tokio::select! {
            biased;
            _ = self.closed.cancelled() => return Err(McpError::Closed),
            _ = cancellation.cancelled() => return Err(McpError::Cancelled),
            result = timeout_at(deadline, send) => result.map_err(|_| McpError::Timeout)?.map_err(http_error)?,
        };
        if response.status() != StatusCode::ACCEPTED {
            return Err(McpError::Protocol(format!(
                "MCP legacy message returned HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }

    async fn reply_legacy_server_request(
        &self,
        request: &Value,
        cancellation: &CancellationToken,
    ) -> Result<(), McpError> {
        let id = request
            .get("id")
            .filter(|id| id.is_string() || id.is_number())
            .ok_or_else(|| McpError::Protocol("MCP server request has invalid ID".into()))?;
        let response = if request.get("method").and_then(Value::as_str) == Some("ping") {
            json!({"jsonrpc":"2.0", "id":id, "result":{}})
        } else {
            json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32601,"message":"Method not found"}})
        };
        self.send_legacy_message(response, cancellation, self.config.request_timeout)
            .await
    }

    async fn cancel_legacy_request(&self, id: u64, reason: &str) {
        if self.is_modern() {
            return;
        }
        let fresh = CancellationToken::new();
        let _ = self.send_legacy_message(
            json!({"jsonrpc":"2.0", "method":"notifications/cancelled", "params":{"requestId":id,"reason":reason}}),
            &fresh,
            self.config.request_timeout.min(Duration::from_secs(1)),
        ).await;
    }

    pub async fn list_tools(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Vec<McpTool>, McpError> {
        match self.list_tools_once(cancellation).await {
            Err(McpError::SessionExpired) => {
                self.initialize_legacy(cancellation).await?;
                self.list_tools_once(cancellation).await
            }
            other => other,
        }
    }

    async fn list_tools_once(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Vec<McpTool>, McpError> {
        if self.closed.is_cancelled() {
            return Err(McpError::Closed);
        }
        if self.catalog_changed() {
            return Err(McpError::CatalogChanged);
        }
        if self
            .server_info
            .read()
            .await
            .capabilities
            .get("tools")
            .is_none()
        {
            self.catalog.write().await.clear();
            self.catalog_discovered.store(true, Ordering::Release);
            return Ok(Vec::new());
        }
        let mut entries = BTreeMap::new();
        let mut seen_names = BTreeSet::new();
        let mut cursors = BTreeSet::new();
        let mut total_bytes = 0usize;
        let mut total_tools = 0usize;
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_MCP_TOOL_PAGES {
            if self.catalog_changed() {
                return Err(McpError::CatalogChanged);
            }
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |value| json!({"cursor": value}));
            let result = self
                .request("tools/list", params, None, &[], cancellation)
                .await?;
            require_complete(&result, "tools/list", self.is_modern())?;
            let object = result
                .as_object()
                .ok_or_else(|| McpError::Protocol("tools/list result must be an object".into()))?;
            let page = object
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| McpError::Protocol("tools/list result has no tools array".into()))?;
            for raw in page {
                total_tools += 1;
                if total_tools > MAX_MCP_TOOLS {
                    return Err(McpError::CatalogLimit("too many tools"));
                }
                let tool_bytes = serde_json::to_vec(raw)
                    .expect("JSON values serialize")
                    .len();
                if tool_bytes > MAX_TOOL_BYTES {
                    return Err(McpError::CatalogLimit("tool definition too large"));
                }
                total_bytes = total_bytes.saturating_add(tool_bytes);
                if total_bytes > MAX_CATALOG_BYTES {
                    return Err(McpError::CatalogLimit("tool catalog too large"));
                }
                let tool: McpTool = serde_json::from_value(raw.clone()).map_err(|error| {
                    McpError::Protocol(format!("invalid tool definition: {error}"))
                })?;
                validate_tool(&tool)?;
                if !seen_names.insert(tool.name.clone()) {
                    return Err(McpError::Protocol(format!(
                        "duplicate MCP tool: {}",
                        tool.name
                    )));
                }
                // The modern HTTP spec requires invalid x-mcp-header tools to
                // be excluded individually; legacy tools do not use these headers.
                let headers = if self.is_modern() {
                    collect_header_specs(&tool.input_schema)
                } else {
                    Ok(Vec::new())
                };
                if let Ok(headers) = headers {
                    entries.insert(tool.name.clone(), CatalogEntry { tool, headers });
                }
            }
            cursor = match object.get("nextCursor") {
                None | Some(Value::Null) => None,
                Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
                _ => return Err(McpError::Protocol("invalid nextCursor".into())),
            };
            let Some(next) = &cursor else {
                if self.catalog_changed() {
                    return Err(McpError::CatalogChanged);
                }
                let tools = entries.values().map(|entry| entry.tool.clone()).collect();
                let mut catalog = self.catalog.write().await;
                if self.catalog_discovered.load(Ordering::Acquire)
                    && (catalog.len() != entries.len()
                        || catalog.iter().any(|(name, previous)| {
                            entries
                                .get(name)
                                .is_none_or(|current| current.tool != previous.tool)
                        }))
                {
                    self.catalog_changed.store(true, Ordering::Release);
                    return Err(McpError::CatalogChanged);
                }
                *catalog = entries;
                self.catalog_discovered.store(true, Ordering::Release);
                return Ok(tools);
            };
            if !cursors.insert(next.clone()) {
                return Err(McpError::CatalogLimit("repeated pagination cursor"));
            }
        }
        Err(McpError::CatalogLimit("too many tool pages"))
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        cancellation: &CancellationToken,
    ) -> Result<McpCallResult, McpError> {
        if !arguments.is_object() {
            return Err(McpError::Protocol(
                "tool arguments must be a JSON object".into(),
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
        let entry = self
            .catalog
            .read()
            .await
            .get(name)
            .cloned()
            .ok_or_else(|| McpError::UnadvertisedTool(name.to_owned()))?;
        let headers = extract_tool_headers(&entry.headers, &arguments)?;
        if self.catalog_changed() {
            return Err(McpError::CatalogChanged);
        }
        let result = self
            .request(
                "tools/call",
                json!({"name": name, "arguments": arguments}),
                Some(name),
                &headers,
                cancellation,
            )
            .await;
        let result = match result {
            Err(McpError::SessionExpired) => {
                // The 2025 transport calls for a fresh session after 404. The
                // tool call itself might have run, so never replay it.
                let recovery = self.initialize_legacy(cancellation).await;
                let mut message = "MCP HTTP session expired during tools/call".to_owned();
                if let Err(error) = recovery {
                    message.push_str(&format!("; session reinitialization failed: {error}"));
                }
                return Err(McpError::EffectUncertain(message));
            }
            other => other.map_err(mark_uncertain_tool_error)?,
        };
        require_complete(&result, "tools/call", self.is_modern())
            .map_err(mark_uncertain_tool_error)?;
        serde_json::from_value(result).map_err(|error| {
            mark_uncertain_tool_error(McpError::Protocol(format!(
                "invalid tools/call result: {error}"
            )))
        })
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
        name: Option<&str>,
        extra_headers: &[(HeaderName, HeaderValue)],
        cancellation: &CancellationToken,
    ) -> Result<Value, McpError> {
        let _guard = match &self.era {
            HttpEra::Legacy { .. } => Some(self.legacy_handshake.read().await),
            HttpEra::Modern => None,
        };
        self.request_with_session(method, params, name, extra_headers, cancellation)
            .await
            .map(|(result, _)| result)
    }

    async fn request_with_session(
        &self,
        method: &str,
        mut params: Value,
        name: Option<&str>,
        extra_headers: &[(HeaderName, HeaderValue)],
        cancellation: &CancellationToken,
    ) -> Result<(Value, Option<String>), McpError> {
        if self.closed.is_cancelled() {
            return Err(McpError::Closed);
        }
        if cancellation.is_cancelled() {
            return Err(McpError::Cancelled);
        }
        if method == "tools/call" && self.catalog_changed() {
            return Err(McpError::CatalogChanged);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let params_object = params
            .as_object_mut()
            .ok_or_else(|| McpError::Protocol("MCP request params must be an object".into()))?;
        if self.is_modern() {
            params_object.insert(
                "_meta".to_owned(),
                json!({
                    "io.modelcontextprotocol/protocolVersion": MCP_HTTP_PROTOCOL_VERSION,
                    "io.modelcontextprotocol/clientInfo": {
                        "name": self.config.client_name,
                        "version": self.config.client_version,
                    },
                    "io.modelcontextprotocol/clientCapabilities": {},
                }),
            );
        }
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let bytes = serde_json::to_vec(&message)
            .map_err(|error| McpError::Protocol(format!("cannot encode MCP request: {error}")))?;
        if bytes.len() > self.config.max_message_bytes {
            return Err(McpError::MessageTooLarge {
                limit: self.config.max_message_bytes,
            });
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        match &self.era {
            HttpEra::Modern => {
                headers.insert(
                    HeaderName::from_static("mcp-protocol-version"),
                    HeaderValue::from_static(MCP_HTTP_PROTOCOL_VERSION),
                );
                headers.insert(
                    HeaderName::from_static("mcp-method"),
                    HeaderValue::from_str(method)
                        .map_err(|_| McpError::Protocol("invalid MCP method header".into()))?,
                );
                if let Some(name) = name {
                    headers.insert(
                        HeaderName::from_static("mcp-name"),
                        encode_header_value(name)?,
                    );
                }
                for (header, value) in extra_headers {
                    headers.insert(header.clone(), value.clone());
                }
            }
            HttpEra::Legacy {
                version,
                session_id,
            } if method != "initialize" => {
                headers.insert(
                    HeaderName::from_static("mcp-protocol-version"),
                    HeaderValue::from_str(&version.read().await)
                        .map_err(|_| McpError::Protocol("invalid MCP protocol version".into()))?,
                );
                if let Some(session) = session_id.read().await.as_ref() {
                    headers.insert(
                        HeaderName::from_static("mcp-session-id"),
                        session_header_value(session)?,
                    );
                }
            }
            HttpEra::Legacy { .. } => {}
        }
        if let Some(auth) = self.authorization_header(cancellation).await? {
            headers.insert(AUTHORIZATION, auth);
        }
        if method == "tools/call" && self.catalog_changed() {
            return Err(McpError::CatalogChanged);
        }
        let deadline = Instant::now() + self.config.request_timeout;
        let send = self
            .client
            .post(self.endpoint.clone())
            .headers(headers)
            .body(bytes)
            .send();
        let response = tokio::select! {
            biased;
            _ = self.closed.cancelled() => Err(McpError::Closed),
            _ = cancellation.cancelled() => Err(McpError::Cancelled),
            result = timeout_at(deadline, send) => result.map_err(|_| McpError::Timeout).and_then(|response| response.map_err(http_error)),
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                if matches!(error, McpError::Cancelled | McpError::Timeout) {
                    self.cancel_legacy_request(id, &error.to_string()).await;
                }
                return Err(error);
            }
        };
        let session = if method == "initialize" {
            parse_session_id(response.headers())?
        } else {
            None
        };
        if method == "initialize" {
            if let HttpEra::Legacy { session_id, .. } = &self.era {
                // The server already allocated this session. Keep ownership
                // even if the response body is malformed, oversized or cut
                // off, so initialization failure can still issue DELETE.
                *session_id.write().await = session.clone();
            }
        }
        let result = self
            .read_response(response, id, method, deadline, cancellation)
            .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                if matches!(error, McpError::Cancelled | McpError::Timeout) {
                    self.cancel_legacy_request(id, &error.to_string()).await;
                }
                return Err(error);
            }
        };
        Ok((result, session))
    }

    async fn read_response(
        &self,
        mut response: reqwest::Response,
        id: u64,
        method: &str,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<Value, McpError> {
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !status.is_success() {
            if status == StatusCode::NOT_FOUND {
                if let HttpEra::Legacy { session_id, .. } = &self.era {
                    if session_id.read().await.is_some() {
                        return Err(McpError::SessionExpired);
                    }
                }
            }
            let bytes = self
                .read_bounded(&mut response, MAX_ERROR_BODY_BYTES, deadline, cancellation)
                .await?;
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                if method == "server/discover" {
                    if value.get("jsonrpc").and_then(Value::as_str) == Some("2.0") {
                        if let Some(raw_error) = value.get("error") {
                            if let Ok(error @ McpError::Remote { code, .. }) =
                                parse_remote_error(raw_error)
                            {
                                if (-32022..=-32020).contains(&code)
                                    || (status == StatusCode::NOT_FOUND && code == -32601)
                                {
                                    return Err(error);
                                }
                            }
                        }
                    }
                    if matches!(
                        status,
                        StatusCode::BAD_REQUEST
                            | StatusCode::NOT_FOUND
                            | StatusCode::METHOD_NOT_ALLOWED
                    ) {
                        return Err(McpError::LegacyHttpRequired);
                    }
                }
                if let Err(error @ McpError::Remote { .. }) = parse_rpc_response(value, id) {
                    return Err(error);
                }
            }
            if method == "server/discover"
                && matches!(
                    status,
                    StatusCode::BAD_REQUEST
                        | StatusCode::NOT_FOUND
                        | StatusCode::METHOD_NOT_ALLOWED
                )
            {
                return Err(McpError::LegacyHttpRequired);
            }
            return Err(McpError::Protocol(format!("MCP HTTP status {status}")));
        }
        if status != StatusCode::OK {
            return Err(McpError::Protocol(format!(
                "unexpected MCP HTTP status {status}"
            )));
        }
        match content_type.as_str() {
            "application/json" => {
                let bytes = self
                    .read_bounded(
                        &mut response,
                        self.config.max_message_bytes,
                        deadline,
                        cancellation,
                    )
                    .await?;
                let value = serde_json::from_slice(&bytes).map_err(|error| {
                    McpError::Protocol(format!("invalid MCP JSON response: {error}"))
                })?;
                match parse_rpc_response(value, id) {
                    Err(McpError::Remote { code: -32601, .. }) if method == "server/discover" => {
                        Err(McpError::LegacyHttpRequired)
                    }
                    result => result,
                }
            }
            "text/event-stream" => {
                self.read_sse(response, id, method, deadline, cancellation)
                    .await
            }
            _ => Err(McpError::Protocol(format!(
                "unsupported MCP HTTP Content-Type: {content_type}"
            ))),
        }
    }

    async fn read_bounded(
        &self,
        response: &mut reqwest::Response,
        limit: usize,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, McpError> {
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(McpError::MessageTooLarge { limit });
        }
        let mut bytes = Vec::new();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = self.closed.cancelled() => return Err(McpError::Closed),
                _ = cancellation.cancelled() => return Err(McpError::Cancelled),
                result = timeout_at(deadline, response.chunk()) => result.map_err(|_| McpError::Timeout)?.map_err(http_error)?,
            };
            let Some(chunk) = chunk else {
                return Ok(bytes);
            };
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err(McpError::MessageTooLarge { limit });
            }
            bytes.extend_from_slice(&chunk);
        }
    }

    async fn read_sse(
        &self,
        mut response: reqwest::Response,
        id: u64,
        method: &str,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<Value, McpError> {
        let mut decoder = SseDecoder::new();
        for attempt in 0..=MAX_SSE_RESUMES {
            match self
                .consume_sse(
                    &mut response,
                    Some(id),
                    None,
                    &mut decoder,
                    Some(deadline),
                    cancellation,
                )
                .await?
            {
                SseReadOutcome::Final(value) => return Ok(value),
                SseReadOutcome::Disconnected { .. } => {}
            }
            if self.is_modern() || method == "initialize" {
                return Err(McpError::Protocol(
                    "MCP SSE ended before its final response".into(),
                ));
            }
            if attempt == MAX_SSE_RESUMES {
                return Err(McpError::Protocol("MCP SSE resume limit reached".into()));
            }
            let cursor = decoder.last_event_id.as_deref().ok_or_else(|| {
                McpError::Protocol("MCP SSE ended without a resume cursor or final response".into())
            })?;
            self.wait_sse_retry(decoder.retry, Some(deadline), cancellation)
                .await?;
            response = self
                .legacy_get(Some(cursor), deadline, cancellation)
                .await?
                .ok_or_else(|| McpError::Protocol("MCP SSE resume GET was rejected".into()))?;
        }
        unreachable!()
    }

    async fn wait_sse_retry(
        &self,
        retry: Duration,
        deadline: Option<Instant>,
        cancellation: &CancellationToken,
    ) -> Result<(), McpError> {
        let sleep = async {
            match deadline {
                Some(deadline) => timeout_at(deadline, tokio::time::sleep(retry))
                    .await
                    .map_err(|_| McpError::Timeout),
                None => {
                    tokio::time::sleep(retry).await;
                    Ok(())
                }
            }
        };
        tokio::select! {
            biased;
            _ = self.closed.cancelled() => Err(McpError::Closed),
            _ = cancellation.cancelled() => Err(McpError::Cancelled),
            result = sleep => result,
        }
    }

    async fn consume_sse(
        &self,
        response: &mut reqwest::Response,
        expected_id: Option<u64>,
        sender: Option<&mpsc::Sender<Value>>,
        decoder: &mut SseDecoder,
        deadline: Option<Instant>,
        cancellation: &CancellationToken,
    ) -> Result<SseReadOutcome, McpError> {
        let accounting = if sender.is_some() {
            SseAccounting::Notification
        } else {
            SseAccounting::Request
        };
        let mut received_message = false;
        loop {
            let read = async {
                match deadline {
                    Some(deadline) => timeout_at(deadline, response.chunk())
                        .await
                        .map_err(|_| McpError::Timeout)?,
                    None => response.chunk().await,
                }
                .map_err(http_error)
            };
            let chunk = tokio::select! {
                biased;
                _ = self.closed.cancelled() => return Err(McpError::Closed),
                _ = cancellation.cancelled() => return Err(McpError::Cancelled),
                result = read => result,
            };
            let chunk = match chunk {
                Ok(Some(chunk)) => chunk,
                Ok(None) | Err(_) => {
                    decoder.reset_connection(accounting);
                    return Ok(SseReadOutcome::Disconnected { received_message });
                }
            };
            for event in decoder.push(&chunk, self.config.max_message_bytes, accounting)? {
                received_message = true;
                if event.get("method").is_some() {
                    if event.get("id").is_some() {
                        if self.is_modern() {
                            return Err(McpError::Protocol(
                                "server-initiated request is invalid in MCP 2026-07-28".into(),
                            ));
                        }
                        self.reply_legacy_server_request(&event, cancellation)
                            .await?;
                    } else {
                        if event.get("method").and_then(Value::as_str)
                            == Some("notifications/tools/list_changed")
                        {
                            // Invalidate before forwarding to a bounded
                            // channel, which may stall or lose its receiver.
                            self.catalog_changed.store(true, Ordering::Release);
                        }
                        if let Some(sender) = sender {
                            let send = async {
                                match deadline {
                                    Some(deadline) => timeout_at(deadline, sender.send(event))
                                        .await
                                        .map_err(|_| McpError::Timeout)?,
                                    None => sender.send(event).await,
                                }
                                .map_err(|_| McpError::Closed)
                            };
                            tokio::select! {
                                biased;
                                _ = self.closed.cancelled() => return Err(McpError::Closed),
                                _ = cancellation.cancelled() => return Err(McpError::Cancelled),
                                result = send => result?,
                            }
                        }
                    }
                } else if let Some(id) = expected_id {
                    return parse_rpc_response(event, id).map(SseReadOutcome::Final);
                } else {
                    return Err(McpError::Protocol(
                        "unsolicited JSON-RPC response on MCP notification stream".into(),
                    ));
                }
            }
        }
    }
}

#[async_trait]
impl McpToolClient for McpHttpClient {
    fn catalog_changed(&self) -> bool {
        McpHttpClient::catalog_changed(self)
    }

    async fn list_tools(&self, cancellation: &CancellationToken) -> Result<Vec<McpTool>, McpError> {
        McpHttpClient::list_tools(self, cancellation).await
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        cancellation: &CancellationToken,
    ) -> Result<McpCallResult, McpError> {
        McpHttpClient::call_tool(self, name, arguments, cancellation).await
    }
}

fn http_error(error: reqwest::Error) -> McpError {
    McpError::Protocol(format!("MCP HTTP transport error: {error}"))
}

fn parse_session_id(headers: &HeaderMap) -> Result<Option<String>, McpError> {
    let Some(value) = headers.get(HeaderName::from_static("mcp-session-id")) else {
        return Ok(None);
    };
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_SESSION_ID_BYTES
        || !bytes.iter().all(|byte| matches!(*byte, b'!'..=b'~'))
    {
        return Err(McpError::Protocol("invalid MCP HTTP session ID".into()));
    }
    Ok(Some(
        value.to_str().expect("visible ASCII session ID").to_owned(),
    ))
}

fn session_header_value(session: &str) -> Result<HeaderValue, McpError> {
    let mut value = HeaderValue::from_str(session)
        .map_err(|_| McpError::Protocol("invalid MCP session ID".into()))?;
    value.set_sensitive(true);
    Ok(value)
}

fn parse_remote_error(value: &Value) -> Result<McpError, McpError> {
    let code = value
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| McpError::Protocol("MCP error has no numeric code".into()))?;
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| McpError::Protocol("MCP error has no message".into()))?;
    Ok(McpError::Remote {
        code,
        message: message.to_owned(),
    })
}

fn parse_rpc_response(value: Value, id: u64) -> Result<Value, McpError> {
    let object = value
        .as_object()
        .ok_or_else(|| McpError::Protocol("MCP response must be an object".into()))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || object.get("id").and_then(Value::as_u64) != Some(id)
    {
        return Err(McpError::Protocol(
            "invalid MCP response version or ID".into(),
        ));
    }
    match (object.get("result"), object.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) => Err(parse_remote_error(error)?),
        _ => Err(McpError::Protocol(
            "MCP response needs a result or error".into(),
        )),
    }
}

fn require_complete(result: &Value, method: &str, modern: bool) -> Result<(), McpError> {
    match result.get("resultType").and_then(Value::as_str) {
        Some("complete") => Ok(()),
        None if !modern => Ok(()),
        Some("input_required") => Err(McpError::Protocol(format!(
            "{method} requires additional input; this client cannot complete MCP multi-round-trip requests"
        ))),
        _ => Err(McpError::Protocol(format!(
            "unsupported {method} resultType"
        ))),
    }
}

fn parse_discovery(result: Value) -> Result<McpServerInfo, McpError> {
    require_complete(&result, "server/discover", true)?;
    let object = result
        .as_object()
        .ok_or_else(|| McpError::Protocol("server/discover result must be an object".into()))?;
    let versions = object
        .get("supportedVersions")
        .and_then(Value::as_array)
        .ok_or_else(|| McpError::Protocol("server/discover omitted supportedVersions".into()))?;
    if !versions
        .iter()
        .any(|value| value.as_str() == Some(MCP_HTTP_PROTOCOL_VERSION))
    {
        return Err(McpError::Protocol(
            "HTTP server did not advertise support for MCP 2026-07-28".into(),
        ));
    }
    let capabilities = object
        .get("capabilities")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| McpError::Protocol("server/discover omitted capabilities".into()))?;
    let info = object
        .get("_meta")
        .and_then(|value| value.get("io.modelcontextprotocol/serverInfo"))
        .and_then(Value::as_object);
    let name = info
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("MCP HTTP server")
        .to_owned();
    let version = info
        .and_then(|value| value.get("version"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let instructions = object
        .get("instructions")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(McpServerInfo {
        protocol_version: MCP_HTTP_PROTOCOL_VERSION.to_owned(),
        name,
        version,
        capabilities,
        instructions,
    })
}

fn validate_tool(tool: &McpTool) -> Result<(), McpError> {
    if tool.name.is_empty()
        || tool.name.chars().count() > 128
        || tool.name.chars().any(char::is_control)
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

fn collect_header_specs(schema: &Value) -> Result<Vec<HeaderSpec>, ()> {
    fn walk(
        value: &Value,
        path: &mut Vec<String>,
        reachable: bool,
        headers: &mut Vec<HeaderSpec>,
        names: &mut BTreeSet<String>,
    ) -> Result<(), ()> {
        match value {
            Value::Object(object) => {
                if let Some(name) = object.get("x-mcp-header") {
                    if !reachable || path.is_empty() {
                        return Err(());
                    }
                    let name = name.as_str().filter(|name| !name.is_empty()).ok_or(())?;
                    if !names.insert(name.to_ascii_lowercase()) {
                        return Err(());
                    }
                    if !matches!(
                        object.get("type").and_then(Value::as_str),
                        Some("string" | "integer" | "boolean")
                    ) {
                        return Err(());
                    }
                    let header = HeaderName::from_bytes(format!("Mcp-Param-{name}").as_bytes())
                        .map_err(|_| ())?;
                    headers.push(HeaderSpec {
                        header,
                        path: path.clone(),
                    });
                }
                for (key, child) in object {
                    if key == "x-mcp-header" {
                        continue;
                    }
                    if key == "properties" && reachable {
                        let properties = child.as_object().ok_or(())?;
                        for (property, schema) in properties {
                            path.push(property.clone());
                            walk(schema, path, true, headers, names)?;
                            path.pop();
                        }
                    } else {
                        walk(child, path, false, headers, names)?;
                    }
                }
            }
            Value::Array(values) => {
                for child in values {
                    walk(child, path, false, headers, names)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut headers = Vec::new();
    walk(
        schema,
        &mut Vec::new(),
        true,
        &mut headers,
        &mut BTreeSet::new(),
    )?;
    Ok(headers)
}

fn extract_tool_headers(
    specs: &[HeaderSpec],
    arguments: &Value,
) -> Result<Vec<(HeaderName, HeaderValue)>, McpError> {
    let mut headers = Vec::new();
    for spec in specs {
        let mut value = arguments;
        for segment in &spec.path {
            let Some(next) = value.get(segment) else {
                value = &Value::Null;
                break;
            };
            value = next;
        }
        if value.is_null() {
            continue;
        }
        let string = match value {
            Value::String(value) => value.clone(),
            Value::Bool(value) => value.to_string(),
            Value::Number(value)
                if value
                    .as_i64()
                    .is_some_and(|n| n.unsigned_abs() <= MAX_SAFE_INTEGER)
                    || value.as_u64().is_some_and(|n| n <= MAX_SAFE_INTEGER) =>
            {
                value.to_string()
            }
            _ => {
                return Err(McpError::Protocol(
                    "invalid x-mcp-header argument type".into(),
                ));
            }
        };
        headers.push((spec.header.clone(), encode_header_value(&string)?));
    }
    Ok(headers)
}

fn encode_header_value(value: &str) -> Result<HeaderValue, McpError> {
    let bytes = value.as_bytes();
    let printable = bytes
        .iter()
        .all(|byte| matches!(*byte, b'\t' | b' '..=b'~'));
    let edge_whitespace = bytes
        .first()
        .into_iter()
        .chain(bytes.last())
        .any(|byte| matches!(*byte, b' ' | b'\t'));
    let already_encoded = value.starts_with("=?base64?") && value.ends_with("?=");
    let plain = printable && !edge_whitespace && !already_encoded;
    let encoded = if plain {
        value.to_owned()
    } else {
        format!("=?base64?{}?=", BASE64.encode(bytes))
    };
    HeaderValue::from_str(&encoded)
        .map_err(|_| McpError::Protocol("invalid MCP HTTP header value".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::sync::Arc;
    use std::thread;

    struct Request {
        method: String,
        headers: BTreeMap<String, String>,
        body: Value,
    }

    enum Reply {
        Json(Value),
        Sse(String),
        DelayedSse {
            first: String,
            last: String,
            first_flushed: Option<tokio::sync::oneshot::Sender<()>>,
        },
        TooLarge,
        Close,
        Custom {
            status: &'static str,
            content_type: Option<&'static str>,
            headers: Vec<(String, String)>,
            body: Vec<u8>,
        },
    }

    fn local_server<F>(count: usize, reply: F) -> (String, thread::JoinHandle<Vec<Request>>)
    where
        F: Fn(usize, &Request) -> Reply + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            for index in 0..count {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "HTTP test server timed out"
                            );
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("HTTP test accept failed: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let request = read_request(&mut stream);
                let response = reply(index, &request);
                requests.push(request);
                write_reply(&mut stream, response);
            }
            requests
        });
        (format!("http://{address}/mcp"), handle)
    }

    fn read_request(stream: &mut TcpStream) -> Request {
        let mut bytes = Vec::new();
        let headers_end = loop {
            let mut chunk = [0u8; 4096];
            let size = stream.read(&mut chunk).unwrap();
            assert!(size > 0, "client closed before HTTP headers");
            bytes.extend_from_slice(&chunk[..size]);
            assert!(bytes.len() < 100_000, "HTTP test request too large");
            if let Some(at) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                break at + 4;
            }
        };
        let head = std::str::from_utf8(&bytes[..headers_end]).unwrap();
        let method = head.split_whitespace().next().unwrap().to_owned();
        assert!(
            head.starts_with(&format!("{method} /mcp HTTP/1.1")),
            "{head}"
        );
        assert!(
            matches!(method.as_str(), "POST" | "GET" | "DELETE"),
            "{head}"
        );
        let mut headers = BTreeMap::new();
        for line in head.split("\r\n").skip(1) {
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        let length: usize = headers
            .get("content-length")
            .map_or(Ok(0), |value| value.parse())
            .unwrap();
        while bytes.len() - headers_end < length {
            let mut chunk = [0u8; 4096];
            let size = stream.read(&mut chunk).unwrap();
            assert!(size > 0, "client closed before HTTP body");
            bytes.extend_from_slice(&chunk[..size]);
        }
        Request {
            method,
            headers,
            body: if length == 0 {
                Value::Null
            } else {
                serde_json::from_slice(&bytes[headers_end..headers_end + length]).unwrap()
            },
        }
    }

    fn write_reply(stream: &mut TcpStream, reply: Reply) {
        match reply {
            Reply::Json(value) => {
                let body = serde_json::to_vec(&value).unwrap();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
            }
            Reply::Sse(body) => {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
                    .unwrap();
                stream.write_all(body.as_bytes()).unwrap();
            }
            Reply::DelayedSse {
                first,
                last,
                first_flushed,
            } => {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
                    .unwrap();
                stream.write_all(first.as_bytes()).unwrap();
                stream.flush().unwrap();
                if let Some(first_flushed) = first_flushed {
                    let _ = first_flushed.send(());
                }
                thread::sleep(Duration::from_millis(250));
                let _ = stream.write_all(last.as_bytes());
            }
            Reply::TooLarge => {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 9999999\r\nConnection: close\r\n\r\n")
                    .unwrap();
            }
            Reply::Close => {}
            Reply::Custom {
                status,
                content_type,
                headers,
                body,
            } => {
                let mut head = format!("HTTP/1.1 {status}\r\n");
                if let Some(content_type) = content_type {
                    head.push_str(&format!("Content-Type: {content_type}\r\n"));
                }
                for (name, value) in headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str(&format!(
                    "Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                ));
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
            }
        }
    }

    fn json_reply(status: &'static str, value: Value, session: Option<&str>) -> Reply {
        Reply::Custom {
            status,
            content_type: Some("application/json"),
            headers: session
                .map(|value| vec![("MCP-Session-Id".to_owned(), value.to_owned())])
                .unwrap_or_default(),
            body: serde_json::to_vec(&value).unwrap(),
        }
    }

    fn empty_reply(status: &'static str) -> Reply {
        Reply::Custom {
            status,
            content_type: None,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    #[test]
    fn loopback_endpoint_bypasses_environment_proxy() {
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
        proxy.set_nonblocking(true).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let hit = Arc::new(AtomicBool::new(false));
        let proxy_done = done.clone();
        let proxy_hit = hit.clone();
        let proxy_thread = thread::spawn(move || {
            while !proxy_done.load(AtomicOrdering::SeqCst) {
                match proxy.accept() {
                    Ok((mut stream, _)) => {
                        proxy_hit.store(true, AtomicOrdering::SeqCst);
                        let _ = stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("proxy fixture failed: {error}"),
                }
            }
        });
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "mcp_http::tests::loopback_proxy_child_helper",
                "--nocapture",
            ])
            .env("OPENIDE_MCP_PROXY_TEST", "1")
            .env("HTTP_PROXY", &proxy_url)
            .env("http_proxy", &proxy_url)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy")
            .output()
            .unwrap();
        done.store(true, AtomicOrdering::SeqCst);
        proxy_thread.join().unwrap();
        assert!(
            output.status.success(),
            "loopback proxy child failed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            !hit.load(AtomicOrdering::SeqCst),
            "loopback request reached proxy"
        );
    }

    #[test]
    #[ignore = "spawned only by loopback proxy test"]
    fn loopback_proxy_child_helper() {
        if std::env::var_os("OPENIDE_MCP_PROXY_TEST").is_none() {
            return;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (url, server) = local_server(1, |_index, request| discovery(request));
            let mut config = McpHttpConfig::new(url);
            config.bearer_token = Some("test-secret".into());
            let client = McpHttpClient::connect(config, &CancellationToken::new())
                .await
                .unwrap();
            client.shutdown().await.unwrap();
            let requests = server.join().unwrap();
            assert_eq!(requests[0].headers["authorization"], "Bearer test-secret");
        });
    }

    fn rpc(request: &Request, result: Value) -> Value {
        json!({"jsonrpc":"2.0", "id": request.body["id"], "result": result})
    }

    fn discovery(request: &Request) -> Reply {
        Reply::Json(rpc(
            request,
            json!({
                "resultType":"complete",
                "supportedVersions":[MCP_HTTP_PROTOCOL_VERSION],
                "capabilities":{"tools":{}},
                "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"1"}},
            }),
        ))
    }

    fn tool_list(request: &Request) -> Reply {
        Reply::Json(rpc(
            request,
            json!({
                "resultType":"complete",
                "tools":[
                    {"name":"echo","inputSchema":{"type":"object","properties":{"note":{"type":"string","x-mcp-header":"Note"}}}},
                    {"name":"invalid","inputSchema":{"type":"object","$defs":{"bad":{"type":"string","x-mcp-header":"Bad"}}}}
                ]
            }),
        ))
    }

    #[tokio::test]
    async fn local_http_and_rotated_bearer_never_certify_pending_call_resume() {
        let (url, server) = local_server(6, |index, request| {
            if index % 2 == 0 {
                discovery(request)
            } else {
                tool_list(request)
            }
        });
        let cancellation = CancellationToken::new();
        for token in [Some("before-rotation"), Some("after-rotation"), None] {
            let mut config = McpHttpConfig::new(&url);
            config.bearer_token = token.map(str::to_owned);
            let client = McpHttpClient::connect(config, &cancellation).await.unwrap();
            assert!(client.resume_server_identity("remote").await.is_none());
            client.list_tools(&cancellation).await.unwrap();
            assert!(client.resume_server_identity("remote").await.is_none());
            client.shutdown().await.unwrap();
        }
        let requests = server.join().unwrap();
        assert_eq!(
            requests[0].headers["authorization"],
            "Bearer before-rotation"
        );
        assert_eq!(
            requests[2].headers["authorization"],
            "Bearer after-rotation"
        );
        assert!(!requests[4].headers.contains_key("authorization"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn bearer_identity_survives_new_client_and_detects_rotated_token() {
        use crate::{mcp_runtime_digest, McpServerRuntimeIdentity};

        let (url, server) = local_server(6, |index, request| {
            if index % 2 == 0 {
                discovery(request)
            } else {
                tool_list(request)
            }
        });
        let cancellation = CancellationToken::new();
        let mut digests = Vec::new();
        for token in ["same-bearer", "same-bearer", "rotated-bearer"] {
            let mut config = McpHttpConfig::new(&url);
            config.bearer_token = Some(token.into());
            let mut client = McpHttpClient::connect(config, &cancellation).await.unwrap();
            client.list_tools(&cancellation).await.unwrap();
            // Local fixture exercises the HTTP wire. Substitute a remote TLS
            // authority only when checking the separate recovery identity.
            client.endpoint = Url::parse("https://example.com/mcp").unwrap();
            let identity = client.resume_server_identity("remote").await.unwrap();
            digests.push(
                mcp_runtime_digest(&[McpServerRuntimeIdentity::Http(identity)], &"a".repeat(64))
                    .unwrap(),
            );
            client.shutdown().await.unwrap();
        }
        assert_eq!(digests[0], digests[1]);
        assert_ne!(digests[0], digests[2]);
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 6);
        assert_eq!(requests[0].headers["authorization"], "Bearer same-bearer");
        assert_eq!(
            requests[4].headers["authorization"],
            "Bearer rotated-bearer"
        );
    }

    #[tokio::test]
    async fn https_identity_requires_discovery_and_drops_on_catalog_change_or_close() {
        let (url, server) = local_server(2, |index, request| {
            if index == 0 {
                discovery(request)
            } else {
                tool_list(request)
            }
        });
        let cancellation = CancellationToken::new();
        let mut client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        // The HTTP test fixture runs locally. Substitute an HTTPS authority
        // after the wire handshake to exercise the identity gate itself.
        assert!(client.resume_server_identity("remote").await.is_none());
        client.list_tools(&cancellation).await.unwrap();
        client.endpoint = Url::parse("https://example.com/mcp").unwrap();
        assert!(client.resume_server_identity("remote").await.is_some());
        client.auth = Some(HeaderValue::from_static("Bearer rotated"));
        assert!(client.resume_server_identity("remote").await.is_none());
        client.auth = None;
        client.catalog_changed.store(true, Ordering::Release);
        assert!(client.resume_server_identity("remote").await.is_none());
        client.closed.cancel();
        assert!(client.resume_server_identity("remote").await.is_none());
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
    }

    #[tokio::test]
    async fn relisting_changed_http_tools_invalidates_frozen_catalog() {
        let (url, server) = local_server(3, |index, request| match index {
            0 => discovery(request),
            1 => tool_list(request),
            2 => Reply::Json(rpc(
                request,
                json!({"resultType":"complete","tools":[{"name":"replacement","inputSchema":{"type":"object"}}]}),
            )),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        client.list_tools(&cancellation).await.unwrap();
        assert!(matches!(
            client.list_tools(&cancellation).await,
            Err(McpError::CatalogChanged)
        ));
        assert!(client.catalog_changed());
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::CatalogChanged)
        ));
        client.shutdown().await.unwrap();
        assert_eq!(server.join().unwrap().len(), 3);
    }

    fn legacy_probe(request: &Request) -> Reply {
        json_reply(
            "400 Bad Request",
            json!({"jsonrpc":"2.0","id":request.body["id"],"error":{"code":-32601,"message":"Method not found"}}),
            None,
        )
    }

    fn legacy_initialize(request: &Request, version: &str, session: Option<&str>) -> Reply {
        json_reply(
            "200 OK",
            rpc(
                request,
                json!({
                    "protocolVersion":version,
                    "serverInfo":{"name":"legacy fixture","version":"1"},
                    "capabilities":{"tools":{}}
                }),
            ),
            session,
        )
    }

    fn legacy_tool_list(request: &Request) -> Reply {
        Reply::Json(rpc(
            request,
            json!({
                "tools":[{"name":"echo","inputSchema":{"type":"object"}}]
            }),
        ))
    }

    struct PartialHandshakeServer {
        url: String,
        requests: thread::JoinHandle<Vec<Request>>,
        initialized_started: tokio::sync::oneshot::Receiver<()>,
        delete_started: tokio::sync::oneshot::Receiver<()>,
        release_delete: std::sync::mpsc::Sender<()>,
    }

    /// Keep initialized open while accepting DELETE on a separate connection.
    /// The explicit DELETE gate verifies connect does not return before cleanup
    /// finishes; None holds DELETE until its independent timeout has elapsed.
    fn partial_handshake_server(delete_reply: Option<Reply>) -> PartialHandshakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let (initialized_tx, initialized_started) = tokio::sync::oneshot::channel();
        let (delete_tx, delete_started) = tokio::sync::oneshot::channel();
        let (release_delete, release_rx) = std::sync::mpsc::channel();
        let requests = thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let next = || {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(std::time::Instant::now() < deadline, "missing HTTP request");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("HTTP accept failed: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let request = read_request(&mut stream);
                (stream, request)
            };
            let mut requests = Vec::new();
            for index in 0..2 {
                let (mut stream, request) = next();
                let reply = if index == 0 {
                    legacy_probe(&request)
                } else {
                    legacy_initialize(&request, "2025-06-18", Some("partial-owned"))
                };
                write_reply(&mut stream, reply);
                requests.push(request);
            }
            let (initialized_stream, request) = next();
            assert_eq!(request.body["method"], "notifications/initialized");
            requests.push(request);
            initialized_tx.send(()).unwrap();
            let (mut delete_stream, request) = next();
            assert_eq!(request.method, "DELETE");
            requests.push(request);
            delete_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            if let Some(reply) = delete_reply {
                write_reply(&mut delete_stream, reply);
            }
            drop(delete_stream);
            drop(initialized_stream);
            requests
        });
        PartialHandshakeServer {
            url,
            requests,
            initialized_started,
            delete_started,
            release_delete,
        }
    }

    fn assert_partial_session_cleanup(requests: &[Request]) {
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0].body["method"], "server/discover");
        assert_eq!(requests[1].body["method"], "initialize");
        assert_eq!(requests[2].body["method"], "notifications/initialized");
        assert_eq!(requests[3].method, "DELETE");
        assert!(requests[3].body.is_null());
        for request in &requests[2..] {
            assert_eq!(request.headers["mcp-session-id"], "partial-owned");
            assert_eq!(request.headers["mcp-protocol-version"], "2025-06-18");
        }
        assert!(requests
            .iter()
            .all(|request| request.body["method"] != "tools/call"));
    }

    #[tokio::test]
    async fn failed_legacy_initialized_closes_owned_session_before_returning() {
        let (url, server) = local_server(4, |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-06-18", Some("partial-owned")),
            2 => empty_reply("500 Internal Server Error"),
            3 => empty_reply("204 No Content"),
            _ => unreachable!(),
        });
        let mut config = McpHttpConfig::new(url);
        config.bearer_token = Some("partial-fixture-secret".into());
        let error = McpHttpClient::connect(config, &CancellationToken::new())
            .await
            .err()
            .unwrap();
        assert!(matches!(&error, McpError::Protocol(message) if message.contains("500")));
        assert!(!error.to_string().contains("partial-fixture-secret"));
        let requests = server.join().unwrap();
        assert_partial_session_cleanup(&requests);
        for request in &requests {
            assert_eq!(
                request.headers["authorization"],
                "Bearer partial-fixture-secret"
            );
        }
    }

    #[tokio::test]
    async fn cancelled_legacy_initialized_still_waits_for_session_delete() {
        let server = partial_handshake_server(Some(empty_reply("204 No Content")));
        let mut config = McpHttpConfig::new(server.url);
        config.request_timeout = Duration::from_secs(5);
        let cancellation = CancellationToken::new();
        let task_token = cancellation.clone();
        let connect =
            tokio::spawn(async move { McpHttpClient::connect(config, &task_token).await });
        server.initialized_started.await.unwrap();
        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(5), server.delete_started)
            .await
            .unwrap()
            .unwrap();
        assert!(
            !connect.is_finished(),
            "connect returned before DELETE completed"
        );
        server.release_delete.send(()).unwrap();
        assert!(matches!(connect.await.unwrap(), Err(McpError::Cancelled)));
        assert_partial_session_cleanup(&server.requests.join().unwrap());
    }

    #[tokio::test]
    async fn timed_out_legacy_initialized_still_waits_for_session_delete() {
        let server = partial_handshake_server(Some(empty_reply("204 No Content")));
        let mut config = McpHttpConfig::new(server.url);
        config.request_timeout = Duration::from_secs(2);
        let connect =
            tokio::spawn(
                async move { McpHttpClient::connect(config, &CancellationToken::new()).await },
            );
        server.initialized_started.await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), server.delete_started)
            .await
            .unwrap()
            .unwrap();
        assert!(
            !connect.is_finished(),
            "connect returned before DELETE completed"
        );
        server.release_delete.send(()).unwrap();
        assert!(matches!(connect.await.unwrap(), Err(McpError::Timeout)));
        assert_partial_session_cleanup(&server.requests.join().unwrap());
    }

    #[tokio::test]
    async fn failed_legacy_handshake_reports_delete_failure_without_losing_cause() {
        let (url, server) = local_server(4, |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-06-18", Some("partial-owned")),
            2 => empty_reply("500 Internal Server Error"),
            3 => empty_reply("503 Service Unavailable"),
            _ => unreachable!(),
        });
        let error = McpHttpClient::connect(McpHttpConfig::new(url), &CancellationToken::new())
            .await
            .err()
            .unwrap();
        let McpError::HandshakeCleanupFailed { handshake, cleanup } = &error else {
            panic!("missing cleanup diagnostic: {error}");
        };
        assert!(
            matches!(handshake.as_ref(), McpError::Protocol(message) if message.contains("500"))
        );
        assert!(matches!(cleanup.as_ref(), McpError::Protocol(message) if message.contains("503")));
        assert!(std::error::Error::source(&error)
            .unwrap()
            .to_string()
            .contains("500"));
        assert!(error
            .to_string()
            .contains("remote session may remain active"));
        assert_partial_session_cleanup(&server.join().unwrap());
    }

    #[tokio::test]
    async fn cancelled_legacy_handshake_reports_bounded_delete_timeout() {
        let server = partial_handshake_server(None);
        let mut config = McpHttpConfig::new(server.url);
        config.request_timeout = Duration::from_secs(1);
        let cancellation = CancellationToken::new();
        let task_token = cancellation.clone();
        let connect =
            tokio::spawn(async move { McpHttpClient::connect(config, &task_token).await });
        server.initialized_started.await.unwrap();
        cancellation.cancel();
        server.delete_started.await.unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), connect)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap();
        assert!(
            matches!(error, McpError::HandshakeCleanupFailed { handshake, cleanup }
            if matches!(handshake.as_ref(), McpError::Cancelled)
                && matches!(cleanup.as_ref(), McpError::Timeout))
        );
        server.release_delete.send(()).unwrap();
        assert_partial_session_cleanup(&server.requests.join().unwrap());
    }

    #[tokio::test]
    async fn malformed_legacy_initialize_still_closes_allocated_session() {
        for case in 0..3 {
            let (url, server) = local_server(3, move |index, request| match index {
                0 => legacy_probe(request),
                1 if case == 0 => Reply::Custom {
                    status: "200 OK",
                    content_type: Some("application/json"),
                    headers: vec![("MCP-Session-Id".into(), "partial-owned".into())],
                    body: b"not JSON".to_vec(),
                },
                1 if case == 1 => json_reply(
                    "200 OK",
                    rpc(request, json!({"protocolVersion":"2025-06-18"})),
                    Some("partial-owned"),
                ),
                1 => legacy_initialize(request, "2099-01-01", Some("partial-owned")),
                2 => empty_reply("204 No Content"),
                _ => unreachable!(),
            });
            let error = McpHttpClient::connect(McpHttpConfig::new(url), &CancellationToken::new())
                .await
                .err()
                .unwrap();
            assert!(matches!(error, McpError::Protocol(_)), "case {case}");
            let requests = server.join().unwrap();
            assert_eq!(requests.len(), 3);
            assert_eq!(requests[2].method, "DELETE");
            assert_eq!(requests[2].headers["mcp-session-id"], "partial-owned");
            assert_eq!(
                requests[2].headers["mcp-protocol-version"],
                MCP_PROTOCOL_VERSION
            );
            assert!(requests
                .iter()
                .all(|request| request.body["method"] != "notifications/initialized"));
        }
    }

    #[tokio::test]
    async fn failed_legacy_replacement_handshake_closes_session_and_revokes_client() {
        let (url, server) = local_server(7, |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", Some("session-old")),
            2 => empty_reply("202 Accepted"),
            3 => empty_reply("404 Not Found"),
            4 => legacy_initialize(request, "2025-06-18", Some("partial-owned")),
            5 => empty_reply("500 Internal Server Error"),
            6 => empty_reply("204 No Content"),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        assert!(matches!(client.list_tools(&cancellation).await,
            Err(McpError::Protocol(message)) if message.contains("500")));
        assert!(matches!(
            client.list_tools(&cancellation).await,
            Err(McpError::Closed)
        ));
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::Closed)
        ));
        client.shutdown().await.unwrap(); // DELETE is not repeated after acknowledgement.
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 7);
        assert_eq!(requests[6].method, "DELETE");
        assert_eq!(requests[6].headers["mcp-session-id"], "partial-owned");
        assert_eq!(requests[6].headers["mcp-protocol-version"], "2025-06-18");
        assert!(requests
            .iter()
            .all(|request| request.body["method"] != "tools/call"));
    }

    #[tokio::test]
    async fn expired_tool_with_failed_replacement_cleanup_keeps_both_diagnostics_without_replay() {
        let (url, server) = local_server(8, |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", Some("session-old")),
            2 => empty_reply("202 Accepted"),
            3 => legacy_tool_list(request),
            4 => empty_reply("404 Not Found"),
            5 => legacy_initialize(request, "2025-06-18", Some("partial-owned")),
            6 => empty_reply("500 Internal Server Error"),
            7 => empty_reply("503 Service Unavailable"),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        client.list_tools(&cancellation).await.unwrap();
        assert!(
            matches!(client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::EffectUncertain(message))
                if message.contains("session expired during tools/call")
                    && message.contains("500")
                    && message.contains("DELETE returned HTTP 503")
                    && message.contains("remote session may remain active"))
        );
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::Closed)
        ));
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 8);
        assert_eq!(requests[7].method, "DELETE");
        assert_eq!(requests[7].headers["mcp-session-id"], "partial-owned");
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.body["method"] == "tools/call")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn legacy_session_negotiates_version_and_closes_with_delete() {
        let (url, server) = local_server(6, |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", Some("session-1")),
            2 => empty_reply("202 Accepted"),
            3 => legacy_tool_list(request),
            4 => Reply::Json(rpc(
                request,
                json!({"content":[{"type":"text","text":"legacy ok"}]}),
            )),
            5 => empty_reply("204 No Content"),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        let info = client.server_info().await;
        assert_eq!(info.name, "legacy fixture");
        assert_eq!(info.protocol_version, "2025-11-25");
        assert_eq!(client.list_tools(&cancellation).await.unwrap().len(), 1);
        let result = client
            .call_tool("echo", json!({}), &cancellation)
            .await
            .unwrap();
        assert_eq!(result.content[0]["text"], "legacy ok");
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(
            requests[0].headers["mcp-protocol-version"],
            MCP_HTTP_PROTOCOL_VERSION
        );
        assert_eq!(requests[0].body["method"], "server/discover");
        assert_eq!(requests[1].body["method"], "initialize");
        assert_eq!(requests[1].body["params"]["protocolVersion"], "2025-11-25");
        assert!(!requests[1].headers.contains_key("mcp-session-id"));
        assert!(!requests[1].headers.contains_key("mcp-protocol-version"));
        assert_eq!(requests[2].body["method"], "notifications/initialized");
        for request in &requests[2..] {
            assert_eq!(request.headers["mcp-session-id"], "session-1");
            assert_eq!(request.headers["mcp-protocol-version"], "2025-11-25");
        }
        assert_eq!(requests[3].body["method"], "tools/list");
        assert_eq!(requests[4].body["method"], "tools/call");
        assert!(requests[4].body["params"].get("_meta").is_none());
        assert_eq!(requests[5].method, "DELETE");
    }

    #[tokio::test]
    async fn legacy_stateless_server_uses_negotiated_version_without_delete() {
        let (url, server) = local_server(4, |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-06-18", None),
            2 => empty_reply("202 Accepted"),
            3 => legacy_tool_list(request),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        assert_eq!(client.server_info().await.protocol_version, "2025-06-18");
        assert_eq!(client.list_tools(&cancellation).await.unwrap().len(), 1);
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 4);
        for request in &requests[2..] {
            assert_eq!(request.headers["mcp-protocol-version"], "2025-06-18");
            assert!(!request.headers.contains_key("mcp-session-id"));
        }
    }

    #[tokio::test]
    async fn legacy_json_rpc_method_error_at_http_200_triggers_handshake() {
        let (url, server) = local_server(3, |index, request| match index {
            0 => json_reply(
                "200 OK",
                json!({"jsonrpc":"2.0","id":request.body["id"],"error":{"code":-32601,"message":"Unknown method"}}),
                None,
            ),
            1 => legacy_initialize(request, "2025-11-25", None),
            2 => empty_reply("202 Accepted"),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        assert_eq!(client.server_info().await.protocol_version, "2025-11-25");
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests[1].body["method"], "initialize");
    }

    #[tokio::test]
    async fn expired_session_after_tool_dispatch_reinitializes_without_replay() {
        let (url, server) = local_server(8, |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", Some("session-old")),
            2 => empty_reply("202 Accepted"),
            3 => legacy_tool_list(request),
            4 => empty_reply("404 Not Found"),
            5 => legacy_initialize(request, "2025-11-25", Some("session-new")),
            6 => empty_reply("202 Accepted"),
            7 => empty_reply("204 No Content"),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        client.list_tools(&cancellation).await.unwrap();
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::EffectUncertain(message)) if message.contains("session expired")
        ));
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.body["method"] == "tools/call")
                .count(),
            1
        );
        assert_eq!(requests[4].headers["mcp-session-id"], "session-old");
        assert!(!requests[5].headers.contains_key("mcp-session-id"));
        assert_eq!(requests[6].headers["mcp-session-id"], "session-new");
        assert_eq!(requests[7].headers["mcp-session-id"], "session-new");
    }

    #[tokio::test]
    async fn expired_session_during_read_only_list_reinitializes_and_retries() {
        let (url, server) = local_server(8, |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", Some("session-old")),
            2 => empty_reply("202 Accepted"),
            3 => empty_reply("404 Not Found"),
            4 => legacy_initialize(request, "2025-11-25", Some("session-new")),
            5 => empty_reply("202 Accepted"),
            6 => legacy_tool_list(request),
            7 => empty_reply("204 No Content"),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        assert_eq!(client.list_tools(&cancellation).await.unwrap().len(), 1);
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests[3].body["method"], "tools/list");
        assert_eq!(requests[3].headers["mcp-session-id"], "session-old");
        assert_eq!(requests[6].body["method"], "tools/list");
        assert_eq!(requests[6].headers["mcp-session-id"], "session-new");
    }

    #[tokio::test]
    async fn modern_protocol_error_does_not_fall_back_to_legacy_initialize() {
        let (url, server) = local_server(1, |_index, request| {
            json_reply(
                "400 Bad Request",
                json!({"jsonrpc":"2.0","id":request.body["id"],"error":{"code":-32020,"message":"missing required header"}}),
                None,
            )
        });
        let cancellation = CancellationToken::new();
        assert!(matches!(
            McpHttpClient::connect(McpHttpConfig::new(url), &cancellation).await,
            Err(McpError::Remote { code: -32020, .. })
        ));
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body["method"], "server/discover");

        let (url, server) = local_server(1, |_index, request| {
            json_reply(
                "404 Not Found",
                json!({"jsonrpc":"2.0","id":request.body["id"],"error":{"code":-32601,"message":"Method not found"}}),
                None,
            )
        });
        assert!(matches!(
            McpHttpClient::connect(McpHttpConfig::new(url), &cancellation).await,
            Err(McpError::Remote { code: -32601, .. })
        ));
        assert_eq!(server.join().unwrap().len(), 1);

        let (url, server) = local_server(1, |_index, _request| {
            json_reply(
                "400 Bad Request",
                json!({"jsonrpc":"2.0","error":{"code":-32022,"message":"Unsupported protocol version","data":{"supported":["2025-11-25"]}}}),
                None,
            )
        });
        assert!(matches!(
            McpHttpClient::connect(McpHttpConfig::new(url), &cancellation).await,
            Err(McpError::Remote { code: -32022, .. })
        ));
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn legacy_sse_server_ping_gets_a_separate_posted_response() {
        let (url, server) = local_server(5, |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", None),
            2 => empty_reply("202 Accepted"),
            3 => {
                let final_response = rpc(
                    request,
                    json!({
                        "tools":[{"name":"echo","inputSchema":{"type":"object"}}]
                    }),
                );
                Reply::Sse(format!(
                    "data: {{\"jsonrpc\":\"2.0\",\"id\":\"ping-1\",\"method\":\"ping\"}}\r\n\r\ndata: {final_response}\r\n\r\n"
                ))
            }
            4 => empty_reply("202 Accepted"),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        assert_eq!(client.list_tools(&cancellation).await.unwrap().len(), 1);
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(
            requests[4].body,
            json!({"jsonrpc":"2.0","id":"ping-1","result":{}})
        );
        assert_eq!(requests[4].headers["mcp-protocol-version"], "2025-11-25");
    }

    #[tokio::test]
    async fn legacy_call_resumes_get_with_cursor_without_replaying_post() {
        let call_id = Arc::new(std::sync::Mutex::new(Value::Null));
        let captured_id = call_id.clone();
        let (url, server) = local_server(7, move |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", Some("sse-session")),
            2 => empty_reply("202 Accepted"),
            3 => legacy_tool_list(request),
            4 => {
                *captured_id.lock().unwrap() = request.body["id"].clone();
                // The apparent response has no terminating blank line, so it
                // cannot become a confirmed tool result.
                let partial = rpc(
                    request,
                    json!({"content":[{"type":"text","text":"partial"}]}),
                );
                Reply::Sse(format!("id: call-1\nretry: 0\ndata:\n\ndata: {partial}\n"))
            }
            5 => Reply::Sse(format!(
                "id: call-2\ndata: {}\n\n",
                json!({"jsonrpc":"2.0","id":*captured_id.lock().unwrap(),"result":{"content":[{"type":"text","text":"resumed"}]}})
            )),
            6 => empty_reply("204 No Content"),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        client.list_tools(&cancellation).await.unwrap();
        let result = client
            .call_tool("echo", json!({}), &cancellation)
            .await
            .unwrap();
        assert_eq!(result.content[0]["text"], "resumed");
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests[5].method, "GET");
        assert_eq!(requests[5].headers["last-event-id"], "call-1");
        assert_eq!(requests[5].headers["mcp-session-id"], "sse-session");
        assert_eq!(requests[5].headers["mcp-protocol-version"], "2025-11-25");
        assert_eq!(requests[5].headers["accept"], "text/event-stream");
        assert!(!requests[5].headers.contains_key("content-type"));
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.body["method"] == "tools/call")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn failed_resume_get_keeps_dispatched_call_uncertain() {
        let (url, server) = local_server(6, |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", None),
            2 => empty_reply("202 Accepted"),
            3 => legacy_tool_list(request),
            4 => Reply::Sse(format!(
                "id: confirmed-cursor\nretry: 0\ndata:\n\ndata: {}\n",
                rpc(request, json!({"content":[]}))
            )),
            5 => empty_reply("405 Method Not Allowed"),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        client.list_tools(&cancellation).await.unwrap();
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::EffectUncertain(message)) if message.contains("resume GET")
        ));
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests[5].method, "GET");
        assert_eq!(requests[5].headers["last-event-id"], "confirmed-cursor");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.body["method"] == "tools/call")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn call_without_completed_cursor_does_not_retry_or_confirm_partial_sse() {
        let (url, server) = local_server(5, |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", None),
            2 => empty_reply("202 Accepted"),
            3 => legacy_tool_list(request),
            4 => Reply::Sse(format!(
                "id: uncommitted\ndata: {}\n",
                rpc(
                    request,
                    json!({"content":[{"type":"text","text":"not final"}]})
                )
            )),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        client.list_tools(&cancellation).await.unwrap();
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::EffectUncertain(message)) if message.contains("without a resume cursor")
        ));
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 5);
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.body["method"] == "tools/call")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn legacy_notification_get_resumes_with_last_event_id() {
        let (url, server) = local_server(6, |index, request| {
            match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", Some("notify-session")),
            2 => empty_reply("202 Accepted"),
            3 => Reply::Sse("id: note-1\nretry: 10\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n".into()),
            4 => Reply::Sse("id: note-2\nretry: 1000\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/resources/list_changed\"}\n\n".into()),
            5 => empty_reply("204 No Content"),
            _ => unreachable!(),
        }
        });
        let cancellation = CancellationToken::new();
        let client = Arc::new(
            McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
                .await
                .unwrap(),
        );
        let (sender, mut receiver) = mpsc::channel(2);
        let listen_cancel = CancellationToken::new();
        let listen_client = client.clone();
        let task_cancel = listen_cancel.clone();
        let listener = tokio::spawn(async move {
            listen_client
                .listen_legacy_notifications(&sender, &task_cancel)
                .await
        });
        assert_eq!(
            receiver.recv().await.unwrap()["method"],
            "notifications/tools/list_changed"
        );
        assert!(client.catalog_changed());
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::CatalogChanged)
        ));
        assert!(matches!(
            client.list_tools(&cancellation).await,
            Err(McpError::CatalogChanged)
        ));
        assert_eq!(
            receiver.recv().await.unwrap()["method"],
            "notifications/resources/list_changed"
        );
        listen_cancel.cancel();
        assert!(matches!(listener.await.unwrap(), Err(McpError::Cancelled)));
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests[3].method, "GET");
        assert_eq!(requests[4].method, "GET");
        assert_eq!(requests[4].headers["last-event-id"], "note-1");
        assert_eq!(requests[4].headers["mcp-session-id"], "notify-session");
        assert_eq!(requests[5].method, "DELETE");
    }

    #[tokio::test]
    async fn list_changed_in_request_sse_discards_discovery_and_stays_sticky() {
        let (url, server) = local_server(2, |index, request| match index {
            0 => discovery(request),
            1 => Reply::Sse(format!(
                "data: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}}\n\n\
                 data: {}\n\n",
                rpc(
                    request,
                    json!({"resultType":"complete","tools":[{"name":"echo","inputSchema":{"type":"object"}}]})
                )
            )),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        assert!(matches!(
            client.list_tools(&cancellation).await,
            Err(McpError::CatalogChanged)
        ));
        assert!(client.catalog_changed());
        assert!(matches!(
            client.list_tools(&cancellation).await,
            Err(McpError::CatalogChanged)
        ));
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::CatalogChanged)
        ));
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests[1].body["method"], "tools/list");
    }

    #[tokio::test]
    async fn legacy_catalog_invalidation_does_not_wait_for_notification_consumer() {
        let (url, server) = local_server(5, |index, request| {
            match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", Some("notify-session")),
            2 => empty_reply("202 Accepted"),
            3 => Reply::Sse(
                "id: note-1\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/resources/list_changed\"}\n\
                 \nid: note-2\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n"
                    .into(),
            ),
            4 => empty_reply("204 No Content"),
            _ => unreachable!(),
        }
        });
        let cancellation = CancellationToken::new();
        let client = Arc::new(
            McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
                .await
                .unwrap(),
        );
        let (sender, _receiver) = mpsc::channel(1);
        let listen_cancel = CancellationToken::new();
        let listen_client = Arc::clone(&client);
        let task_cancel = listen_cancel.clone();
        let listener = tokio::spawn(async move {
            listen_client
                .listen_legacy_notifications(&sender, &task_cancel)
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !client.catalog_changed() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("list_changed must invalidate before bounded channel send");
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::CatalogChanged)
        ));
        listen_cancel.cancel();
        assert!(matches!(listener.await.unwrap(), Err(McpError::Cancelled)));
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 5);
        assert_eq!(requests[3].method, "GET");
        assert_eq!(requests[4].method, "DELETE");
    }

    #[tokio::test]
    async fn notification_get_reinitializes_expired_session_without_old_cursor() {
        let (url, server) = local_server(8, |index, request| {
            match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", Some("old-session")),
            2 => empty_reply("202 Accepted"),
            3 => empty_reply("404 Not Found"),
            4 => legacy_initialize(request, "2025-11-25", Some("new-session")),
            5 => empty_reply("202 Accepted"),
            6 => Reply::Sse("id: fresh-1\nretry: 1000\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n".into()),
            7 => empty_reply("204 No Content"),
            _ => unreachable!(),
        }
        });
        let cancellation = CancellationToken::new();
        let client = Arc::new(
            McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
                .await
                .unwrap(),
        );
        assert!(client.supports_legacy_notifications());
        let (sender, mut receiver) = mpsc::channel(1);
        let listen_cancel = CancellationToken::new();
        let listen_client = client.clone();
        let task_cancel = listen_cancel.clone();
        let listener = tokio::spawn(async move {
            listen_client
                .listen_legacy_notifications(&sender, &task_cancel)
                .await
        });
        assert_eq!(
            receiver.recv().await.unwrap()["method"],
            "notifications/tools/list_changed"
        );
        listen_cancel.cancel();
        assert!(matches!(listener.await.unwrap(), Err(McpError::Cancelled)));
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests[3].method, "GET");
        assert_eq!(requests[3].headers["mcp-session-id"], "old-session");
        assert_eq!(requests[4].body["method"], "initialize");
        assert!(!requests[4].headers.contains_key("mcp-session-id"));
        assert_eq!(requests[6].method, "GET");
        assert_eq!(requests[6].headers["mcp-session-id"], "new-session");
        assert!(!requests[6].headers.contains_key("last-event-id"));
    }

    #[tokio::test]
    async fn replaced_legacy_session_invalidates_advertised_tools_without_notification() {
        let (url, server) = local_server(9, |index, request| {
            match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", Some("old-session")),
            2 => empty_reply("202 Accepted"),
            3 => legacy_tool_list(request),
            4 => empty_reply("404 Not Found"),
            5 => legacy_initialize(request, "2025-11-25", Some("new-session")),
            6 => empty_reply("202 Accepted"),
            7 => Reply::Sse(
                "id: fresh-1\nretry: 1000\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/resources/list_changed\"}\n\n"
                    .into(),
            ),
            8 => empty_reply("204 No Content"),
            _ => unreachable!(),
        }
        });
        let cancellation = CancellationToken::new();
        let client = Arc::new(
            McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
                .await
                .unwrap(),
        );
        client.list_tools(&cancellation).await.unwrap();
        let (sender, mut receiver) = mpsc::channel(1);
        let listen_cancel = CancellationToken::new();
        let listen_client = Arc::clone(&client);
        let task_cancel = listen_cancel.clone();
        let listener = tokio::spawn(async move {
            listen_client
                .listen_legacy_notifications(&sender, &task_cancel)
                .await
        });
        assert_eq!(
            receiver.recv().await.unwrap()["method"],
            "notifications/resources/list_changed"
        );
        assert!(client.catalog_changed());
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::CatalogChanged)
        ));
        listen_cancel.cancel();
        assert!(matches!(listener.await.unwrap(), Err(McpError::Cancelled)));
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 9);
        assert_eq!(requests[5].body["method"], "initialize");
        assert_eq!(requests[7].method, "GET");
        assert!(!requests[7].headers.contains_key("last-event-id"));
    }

    #[tokio::test]
    async fn quiet_notification_get_stays_open_past_request_timeout_until_cancelled() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started = std::sync::Mutex::new(Some(started_tx));
        let (url, server) = local_server(4, move |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", None),
            2 => empty_reply("202 Accepted"),
            3 => Reply::DelayedSse {
                first: ": keepalive\n\n".into(),
                last: "".into(),
                first_flushed: Some(started.lock().unwrap().take().expect("one SSE request")),
            },
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let mut config = McpHttpConfig::new(url);
        config.request_timeout = Duration::from_millis(50);
        let client = Arc::new(McpHttpClient::connect(config, &cancellation).await.unwrap());
        let (sender, _receiver) = mpsc::channel(1);
        let listen_cancel = CancellationToken::new();
        let listen_client = client.clone();
        let task_cancel = listen_cancel.clone();
        let listener = tokio::spawn(async move {
            listen_client
                .listen_legacy_notifications(&sender, &task_cancel)
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), started_rx)
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(!listener.is_finished(), "healthy quiet GET timed out");
        listen_cancel.cancel();
        assert!(matches!(listener.await.unwrap(), Err(McpError::Cancelled)));
        client.shutdown().await.unwrap();
        assert_eq!(server.join().unwrap()[3].method, "GET");
    }

    #[tokio::test]
    async fn sse_resume_count_is_bounded_without_post_replay() {
        let (url, server) = local_server(5 + MAX_SSE_RESUMES, |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", None),
            2 => empty_reply("202 Accepted"),
            3 => legacy_tool_list(request),
            _ => Reply::Sse(format!("id: cursor-{index}\nretry: 0\ndata:\n\n")),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        client.list_tools(&cancellation).await.unwrap();
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::EffectUncertain(message)) if message.contains("resume limit")
        ));
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.body["method"] == "tools/call")
                .count(),
            1
        );
        assert_eq!(
            requests.iter().filter(|r| r.method == "GET").count(),
            MAX_SSE_RESUMES
        );
    }

    #[tokio::test]
    async fn sse_retry_obeys_total_deadline_and_does_not_get_or_repost() {
        let (url, server) = local_server(6, |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", None),
            2 => empty_reply("202 Accepted"),
            3 => legacy_tool_list(request),
            4 => Reply::Sse("id: slow-cursor\nretry: 1000\ndata:\n\n".into()),
            5 => empty_reply("202 Accepted"),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let mut config = McpHttpConfig::new(url);
        config.request_timeout = Duration::from_millis(100);
        let client = McpHttpClient::connect(config, &cancellation).await.unwrap();
        client.list_tools(&cancellation).await.unwrap();
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::EffectUncertain(message)) if message.contains("timed out")
        ));
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests[5].body["method"], "notifications/cancelled");
        assert_eq!(requests.iter().filter(|r| r.method == "GET").count(), 0);
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.body["method"] == "tools/call")
                .count(),
            1
        );
    }

    #[test]
    fn sse_byte_limit_applies_across_reconnected_streams() {
        let mut decoder = SseDecoder::new();
        decoder
            .push(b"id: first\ndata:\n\n", 32, SseAccounting::Request)
            .unwrap();
        assert_eq!(decoder.last_event_id.as_deref(), Some("first"));
        decoder.reset_connection(SseAccounting::Request);
        assert!(matches!(
            decoder.push(b"id: second\ndata:\n\n", 32, SseAccounting::Request),
            Err(McpError::MessageTooLarge { limit: 32 })
        ));
    }

    #[test]
    fn sse_decoder_accepts_cr_lines_and_empty_fields_only_after_dispatch() {
        let mut decoder = SseDecoder::new();
        assert!(decoder
            .push(b"id: cursor\rdata\r", 1024, SseAccounting::Request)
            .unwrap()
            .is_empty());
        assert!(decoder.last_event_id.is_none());
        assert!(decoder
            .push(b"\r", 1024, SseAccounting::Request)
            .unwrap()
            .is_empty());
        assert_eq!(decoder.last_event_id.as_deref(), Some("cursor"));
        let values = decoder
            .push(
                b"id: next\r\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/test\"}\r\n\r\n",
                1024,
                SseAccounting::Request,
            )
            .unwrap();
        assert_eq!(values[0]["method"], "notifications/test");
        assert_eq!(decoder.last_event_id.as_deref(), Some("next"));
        decoder
            .push(b"id\n\n", 1024, SseAccounting::Request)
            .unwrap();
        assert!(decoder.last_event_id.is_none());
    }

    #[tokio::test]
    async fn local_server_json_discovery_and_sse_call_mirror_required_headers() {
        let (url, server) = local_server(3, |index, request| match index {
            0 => discovery(request),
            1 => tool_list(request),
            _ => {
                let final_response = rpc(
                    request,
                    json!({"resultType":"complete","content":[{"type":"text","text":"ok"}]}),
                );
                Reply::Sse(format!(
                    ": keepalive\r\n\r\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}}\r\n\r\ndata: {}\r\n\r\n",
                    final_response
                ))
            }
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        assert_eq!(client.server_info().await.name, "fixture");
        assert!(!client.supports_legacy_notifications());
        let tools = client.list_tools(&cancellation).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert!(matches!(
            client.call_tool("unknown", json!({}), &cancellation).await,
            Err(McpError::UnadvertisedTool(_))
        ));
        let result = client
            .call_tool("echo", json!({"note":" ¡hola "}), &cancellation)
            .await
            .unwrap();
        assert_eq!(result.content[0]["text"], "ok");
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3);
        for (index, request) in requests.iter().enumerate() {
            assert_eq!(
                request.headers["mcp-protocol-version"],
                MCP_HTTP_PROTOCOL_VERSION
            );
            assert_eq!(request.headers["mcp-method"], request.body["method"]);
            assert!(request.headers["accept"].contains("application/json"));
            assert!(request.headers["accept"].contains("text/event-stream"));
            assert_eq!(
                request.body["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
                MCP_HTTP_PROTOCOL_VERSION
            );
            assert!(
                request.body["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]
                    .is_object()
            );
            if index == 2 {
                assert_eq!(request.headers["mcp-name"], "echo");
                assert_eq!(
                    request.headers["mcp-param-note"],
                    encode_header_value(" ¡hola ").unwrap().to_str().unwrap()
                );
            }
        }
    }

    #[tokio::test]
    async fn cancellation_closes_request_scoped_sse_without_retry() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let sender = std::sync::Mutex::new(Some(started_tx));
        let (url, server) = local_server(3, move |index, request| match index {
            0 => discovery(request),
            1 => tool_list(request),
            _ => Reply::DelayedSse {
                first: ": waiting\r\n\r\n".into(),
                last: "data: {}\r\n\r\n".into(),
                first_flushed: Some(sender.lock().unwrap().take().expect("one SSE request")),
            },
        });
        let cancellation = CancellationToken::new();
        let client = Arc::new(
            McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
                .await
                .unwrap(),
        );
        client.list_tools(&cancellation).await.unwrap();
        let call_cancel = CancellationToken::new();
        let task_client = client.clone();
        let task_cancel = call_cancel.clone();
        let call = tokio::spawn(async move {
            task_client
                .call_tool("echo", json!({"note":"x"}), &task_cancel)
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), started_rx)
            .await
            .unwrap()
            .unwrap();
        call_cancel.cancel();
        assert!(matches!(
            call.await.unwrap(),
            Err(McpError::EffectUncertain(message)) if message.contains("cancelled")
        ));
        assert_eq!(server.join().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn legacy_cancellation_posts_notification_with_session() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let sender = std::sync::Mutex::new(Some(started_tx));
        let (url, server) = local_server(7, move |index, request| match index {
            0 => legacy_probe(request),
            1 => legacy_initialize(request, "2025-11-25", Some("cancel-session")),
            2 => empty_reply("202 Accepted"),
            3 => legacy_tool_list(request),
            4 => Reply::DelayedSse {
                first: ": waiting\r\n\r\n".into(),
                last: "data: {}\r\n\r\n".into(),
                first_flushed: Some(sender.lock().unwrap().take().expect("one SSE request")),
            },
            5 => empty_reply("202 Accepted"),
            6 => empty_reply("204 No Content"),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = Arc::new(
            McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
                .await
                .unwrap(),
        );
        client.list_tools(&cancellation).await.unwrap();
        let call_cancel = CancellationToken::new();
        let task_client = client.clone();
        let task_cancel = call_cancel.clone();
        let call =
            tokio::spawn(
                async move { task_client.call_tool("echo", json!({}), &task_cancel).await },
            );
        tokio::time::timeout(Duration::from_secs(5), started_rx)
            .await
            .unwrap()
            .unwrap();
        call_cancel.cancel();
        assert!(matches!(
            call.await.unwrap(),
            Err(McpError::EffectUncertain(message)) if message.contains("cancelled")
        ));
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests[5].body["method"], "notifications/cancelled");
        assert_eq!(
            requests[5].body["params"]["requestId"],
            requests[4].body["id"]
        );
        assert_eq!(requests[5].headers["mcp-session-id"], "cancel-session");
        assert_eq!(requests[6].method, "DELETE");
    }

    #[tokio::test]
    async fn oversized_response_and_uncertain_call_are_never_replayed() {
        let (url, server) = local_server(2, |index, request| {
            if index == 0 {
                discovery(request)
            } else {
                Reply::TooLarge
            }
        });
        let cancellation = CancellationToken::new();
        let mut config = McpHttpConfig::new(url);
        config.max_message_bytes = 1024;
        let client = McpHttpClient::connect(config, &cancellation).await.unwrap();
        assert!(matches!(
            client.list_tools(&cancellation).await,
            Err(McpError::MessageTooLarge { .. })
        ));
        assert_eq!(server.join().unwrap().len(), 2);

        let (url, server) = local_server(3, |index, request| match index {
            0 => discovery(request),
            1 => tool_list(request),
            _ => Reply::Close,
        });
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        client.list_tools(&cancellation).await.unwrap();
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::EffectUncertain(_))
        ));
        assert_eq!(server.join().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn remote_tool_error_is_uncertain_but_is_error_result_is_definitive() {
        let (url, server) = local_server(4, |index, request| match index {
            0 => discovery(request),
            1 => tool_list(request),
            2 => Reply::Json(
                json!({"jsonrpc":"2.0","id":request.body["id"],"error":{"code":-32603,"message":"failed after mutation"}}),
            ),
            3 => Reply::Json(rpc(
                request,
                json!({
                    "resultType":"complete",
                    "content":[{"type":"text","text":"tool reported an error"}],
                    "isError":true
                }),
            )),
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let client = McpHttpClient::connect(McpHttpConfig::new(url), &cancellation)
            .await
            .unwrap();
        client.list_tools(&cancellation).await.unwrap();
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(McpError::EffectUncertain(message)) if message.contains("failed after mutation")
        ));
        let result = client
            .call_tool("echo", json!({}), &cancellation)
            .await
            .unwrap();
        assert!(result.is_error);
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.body["method"] == "tools/call")
                .count(),
            2
        );
    }

    #[test]
    fn header_values_encode_unsafe_text_and_sentinel() {
        assert_eq!(encode_header_value("simple").unwrap(), "simple");
        assert_eq!(encode_header_value("a b").unwrap(), "a b");
        assert_eq!(encode_header_value(" a ").unwrap(), "=?base64?IGEg?=");
        assert_eq!(encode_header_value("écho").unwrap(), "=?base64?w6ljaG8=?=");
        assert_eq!(
            encode_header_value("=?base64?x?=").unwrap(),
            "=?base64?PT9iYXNlNjQ/eD89?="
        );
        assert!(McpHttpConfig::new("https://example.com/mcp?token=x")
            .validate()
            .is_err());
        assert!(McpHttpConfig::new("http://example.com/mcp")
            .validate()
            .is_err());
        assert!(McpHttpConfig::new("http://127.0.0.1/mcp")
            .validate()
            .is_ok());
        assert!(McpHttpConfig::new("http://127.0.0.2/mcp")
            .validate()
            .is_ok());
        assert!(McpHttpConfig::new("http://localhost./mcp")
            .validate()
            .is_ok());
        assert!(is_loopback_host(&Url::parse("https://[::1]/mcp").unwrap()));
    }

    #[test]
    fn custom_headers_follow_only_static_property_paths() {
        let schema = json!({
            "type":"object",
            "properties": {
                "tenant": {"type":"string","x-mcp-header":"Tenant"},
                "nested": {"type":"object","properties": {
                    "count": {"type":"integer","x-mcp-header":"Count"},
                    "active": {"type":"boolean","x-mcp-header":"Active"}
                }}
            }
        });
        let specs = collect_header_specs(&schema).unwrap();
        let headers = extract_tool_headers(
            &specs,
            &json!({"tenant":"acme","nested":{"count":42,"active":false}}),
        )
        .unwrap();
        let headers = headers
            .into_iter()
            .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(headers["mcp-param-tenant"], "acme");
        assert_eq!(headers["mcp-param-count"], "42");
        assert_eq!(headers["mcp-param-active"], "false");
        assert!(collect_header_specs(&json!({
            "type":"object",
            "allOf":[{"properties":{"secret":{"type":"string","x-mcp-header":"Secret"}}}]
        }))
        .is_err());
    }
}
