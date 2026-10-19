//! OAuth 2.1 client support for MCP Streamable HTTP.
//!
//! This module intentionally owns no durable credential storage or browser UI.
//! The host supplies a TokenStore and opens PendingAuthorization::url in a
//! browser. Tokens and PKCE material never enter the agent journal.

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::header::{HeaderMap, ACCEPT, CONTENT_TYPE};
use reqwest::{Client, StatusCode, Url};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{timeout_at, Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_METADATA_BYTES: usize = 64 * 1024;
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_CALLBACK_BYTES: usize = 8 * 1024;
const MAX_CALLBACK_ATTEMPTS: usize = 8;
const MAX_CALLBACK_CONNECTION_TIME: Duration = Duration::from_secs(2);
const MAX_SCOPE_COUNT: usize = 64;
const MAX_SCOPE_BYTES: usize = 4096;
const EXPIRY_SKEW: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OAuthError {
    #[error("OAuth configuration or metadata is invalid: {0}")]
    Invalid(&'static str),
    #[error("OAuth metadata is unavailable")]
    MetadataUnavailable,
    #[error("OAuth network request failed")]
    Network,
    #[error("OAuth request timed out")]
    Timeout,
    #[error("OAuth operation was cancelled")]
    Cancelled,
    #[error("OAuth authorization is required")]
    AuthorizationRequired,
    #[error("OAuth authorization was denied")]
    Denied,
    #[error("OAuth token store failed")]
    Store,
}

/// A pre-registered ID is bound to one issuer. A client ID metadata document
/// is an HTTPS URL with a path and can be used with different issuers.
#[derive(Clone)]
pub enum OAuthRegistration {
    PreRegistered { client_id: String, issuer: String },
    ClientIdMetadataDocument { client_id_url: String },
}

pub struct OAuthConfig {
    pub resource_url: String,
    pub registration: OAuthRegistration,
    pub request_timeout: Duration,
    pub callback_timeout: Duration,
}

impl OAuthConfig {
    pub fn new(resource_url: impl Into<String>, registration: OAuthRegistration) -> Self {
        Self {
            resource_url: resource_url.into(),
            registration,
            request_timeout: Duration::from_secs(15),
            callback_timeout: Duration::from_secs(180),
        }
    }
}

/// Stored tokens are keyed by all three security principals. No serialization
/// or Debug implementation is provided for token values.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct TokenKey {
    pub resource: String,
    pub issuer: String,
    pub client_id: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_unix_seconds: Option<u64>,
    pub scopes: Vec<String>,
}

#[async_trait]
pub trait TokenStore: Send + Sync {
    async fn load(&self, key: &TokenKey) -> Result<Option<OAuthTokens>, OAuthError>;
    async fn save(&self, key: &TokenKey, tokens: OAuthTokens) -> Result<(), OAuthError>;
    async fn delete(&self, key: &TokenKey) -> Result<(), OAuthError>;

    /// Opaque, durable identity of one authorization grant and its scopes.
    /// Implementations without an atomic grant revision return None, which
    /// excludes their OAuth connections from automatic MCP tool recovery.
    async fn resume_grant_identity(&self, _key: &TokenKey) -> Result<Option<String>, OAuthError> {
        Ok(None)
    }

    /// Replace tokens only if the grant still contains `previous`. Stores
    /// must implement an atomic compare-and-replace; the default refuses
    /// refresh rather than trusting a custom store to preserve a grant safely.
    async fn save_refreshed(
        &self,
        _key: &TokenKey,
        _previous: &OAuthTokens,
        _tokens: OAuthTokens,
    ) -> Result<(), OAuthError> {
        Err(OAuthError::Store)
    }

    /// A rejected refresh must not erase a newer authorization saved by
    /// another process while the token endpoint was being contacted.
    async fn delete_if_current(
        &self,
        _key: &TokenKey,
        _previous: &OAuthTokens,
    ) -> Result<bool, OAuthError> {
        Err(OAuthError::Store)
    }
}

struct MemoryGrant {
    tokens: OAuthTokens,
    revision: String,
}

/// Volatile reference store. Production persistence requires an OS credential
/// store implementation; storing plaintext tokens in settings is unsupported.
#[derive(Default)]
pub struct MemoryTokenStore {
    tokens: Mutex<BTreeMap<TokenKey, MemoryGrant>>,
}

#[async_trait]
impl TokenStore for MemoryTokenStore {
    async fn load(&self, key: &TokenKey) -> Result<Option<OAuthTokens>, OAuthError> {
        Ok(self
            .tokens
            .lock()
            .await
            .get(key)
            .map(|grant| grant.tokens.clone()))
    }

    async fn save(&self, key: &TokenKey, tokens: OAuthTokens) -> Result<(), OAuthError> {
        self.tokens.lock().await.insert(
            key.clone(),
            MemoryGrant {
                tokens,
                revision: random_urlsafe(),
            },
        );
        Ok(())
    }

    async fn delete(&self, key: &TokenKey) -> Result<(), OAuthError> {
        self.tokens.lock().await.remove(key);
        Ok(())
    }

    async fn resume_grant_identity(&self, key: &TokenKey) -> Result<Option<String>, OAuthError> {
        Ok(self
            .tokens
            .lock()
            .await
            .get(key)
            .map(|grant| grant_identity_digest(key, &grant.revision, &grant.tokens.scopes)))
    }

    async fn save_refreshed(
        &self,
        key: &TokenKey,
        previous: &OAuthTokens,
        tokens: OAuthTokens,
    ) -> Result<(), OAuthError> {
        let mut grants = self.tokens.lock().await;
        let grant = grants
            .get_mut(key)
            .ok_or(OAuthError::AuthorizationRequired)?;
        if &grant.tokens != previous {
            return Err(OAuthError::AuthorizationRequired);
        }
        grant.tokens = tokens;
        Ok(())
    }

    async fn delete_if_current(
        &self,
        key: &TokenKey,
        previous: &OAuthTokens,
    ) -> Result<bool, OAuthError> {
        let mut grants = self.tokens.lock().await;
        if grants
            .get(key)
            .is_some_and(|grant| &grant.tokens == previous)
        {
            grants.remove(key);
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

pub(crate) fn grant_identity_digest(key: &TokenKey, revision: &str, scopes: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"openide-mcp-oauth-grant-v1\0");
    for value in [&key.resource, &key.issuer, &key.client_id, revision] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    let mut scopes = scopes.to_vec();
    scopes.sort();
    scopes.dedup();
    for scope in scopes {
        hasher.update((scope.len() as u64).to_be_bytes());
        hasher.update(scope.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub struct OAuthSession {
    client: Client,
    resource: Url,
    issuer: String,
    authorization_endpoint: Url,
    token_endpoint: Url,
    client_id: String,
    initial_scopes: Vec<String>,
    response_iss_required: bool,
    request_timeout: Duration,
    callback_timeout: Duration,
    store: Arc<dyn TokenStore>,
    refresh_guard: Mutex<()>,
}

pub struct PendingAuthorization {
    session: Arc<OAuthSession>,
    listener: TcpListener,
    authorization_url: Url,
    redirect_uri: String,
    state: String,
    verifier: String,
    scopes: Vec<String>,
}

impl PendingAuthorization {
    pub fn url(&self) -> &Url {
        &self.authorization_url
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }
}

impl OAuthSession {
    /// Discover one authorization server for the resource. A WWW-Authenticate
    /// challenge, when available, takes precedence over constructed paths.
    pub async fn discover(
        config: OAuthConfig,
        challenge: Option<&str>,
        store: Arc<dyn TokenStore>,
        cancellation: &CancellationToken,
    ) -> Result<Self, OAuthError> {
        if config.request_timeout.is_zero() || config.callback_timeout.is_zero() {
            return Err(OAuthError::Invalid("timeouts must be positive"));
        }
        let resource = validate_resource_url(&config.resource_url)?;
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        if is_loopback(&resource) {
            builder = builder.no_proxy();
        }
        let client = builder.build().map_err(|_| OAuthError::Network)?;
        let challenge = challenge.map(parse_bearer_challenge).transpose()?;
        let metadata_urls = if let Some(url) = challenge
            .as_ref()
            .and_then(|challenge| challenge.resource_metadata.as_deref())
        {
            let metadata = validate_resource_metadata_url(url, &resource)?;
            vec![metadata]
        } else {
            protected_resource_metadata_urls(&resource)
        };
        let mut protected = None;
        for metadata_url in metadata_urls {
            match fetch_json(
                &client,
                &metadata_url,
                config.request_timeout,
                MAX_METADATA_BYTES,
                cancellation,
            )
            .await?
            {
                Some(value) => {
                    protected = Some(parse_protected_metadata(&value, &resource)?);
                    break;
                }
                None => continue,
            }
        }
        let protected = protected.ok_or(OAuthError::MetadataUnavailable)?;
        // Each listed issuer is independent. The first one is used in this
        // initial UI-less API; a future UI may let the user choose another.
        let selected_issuer = match &config.registration {
            OAuthRegistration::PreRegistered { issuer, .. } => protected
                .issuers
                .iter()
                .find(|(listed, _)| listed == issuer),
            OAuthRegistration::ClientIdMetadataDocument { .. } => protected.issuers.first(),
        };
        let (issuer, issuer_url) = selected_issuer.ok_or(OAuthError::Invalid(
            "client registration belongs to another authorization server",
        ))?;
        let issuer = issuer.clone();
        let metadata = discover_authorization_server(
            &client,
            &issuer,
            issuer_url,
            config.request_timeout,
            cancellation,
        )
        .await?;
        let client_id = match config.registration {
            OAuthRegistration::PreRegistered {
                client_id,
                issuer: registered_issuer,
            } => {
                if registered_issuer != issuer {
                    return Err(OAuthError::Invalid(
                        "client registration belongs to another issuer",
                    ));
                }
                validate_client_id(&client_id)?;
                client_id
            }
            OAuthRegistration::ClientIdMetadataDocument { client_id_url } => {
                let url = Url::parse(&client_id_url)
                    .map_err(|_| OAuthError::Invalid("invalid client ID metadata URL"))?;
                if url.scheme() != "https"
                    || url.host_str().is_none()
                    || url.path() == "/"
                    || url.query().is_some()
                    || url.fragment().is_some()
                    || !url.username().is_empty()
                    || url.password().is_some()
                {
                    return Err(OAuthError::Invalid(
                        "client ID metadata document requires an HTTPS URL with a path",
                    ));
                }
                if !metadata.client_id_metadata_document_supported {
                    return Err(OAuthError::Invalid(
                        "authorization server does not advertise client ID metadata documents",
                    ));
                }
                client_id_url
            }
        };
        let initial_scopes = if let Some(scopes) = challenge.and_then(|value| value.scopes) {
            scopes
        } else {
            protected.scopes
        };
        Ok(Self {
            client,
            resource,
            issuer,
            authorization_endpoint: metadata.authorization_endpoint,
            token_endpoint: metadata.token_endpoint,
            client_id,
            initial_scopes,
            response_iss_required: metadata.response_iss_required,
            request_timeout: config.request_timeout,
            callback_timeout: config.callback_timeout,
            store,
            refresh_guard: Mutex::new(()),
        })
    }

    pub fn resource_url(&self) -> &str {
        self.resource.as_str()
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub fn initial_scopes(&self) -> &[String] {
        &self.initial_scopes
    }

    /// A grant revision, not a hash of an access or refresh token. The store
    /// must keep it stable only across refresh of the exact same grant.
    pub async fn resume_authorization_identity(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Option<String>, OAuthError> {
        let key = self.token_key();
        let revision = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(OAuthError::Cancelled),
            result = timeout_at(Instant::now() + self.request_timeout, self.store.resume_grant_identity(&key)) => result.map_err(|_| OAuthError::Timeout)?,
        }?;
        let Some(revision) = revision else {
            return Ok(None);
        };
        if revision.len() != 64
            || !revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Ok(None);
        }
        let mut hasher = Sha256::new();
        hasher.update(b"openide-mcp-oauth-runtime-v1\0");
        for value in [
            self.resource.as_str(),
            &self.issuer,
            self.authorization_endpoint.as_str(),
            self.token_endpoint.as_str(),
            &self.client_id,
            &revision,
        ] {
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        hasher.update([u8::from(self.response_iss_required)]);
        for scope in &self.initial_scopes {
            hasher.update((scope.len() as u64).to_be_bytes());
            hasher.update(scope.as_bytes());
        }
        Ok(Some(format!("{:x}", hasher.finalize())))
    }

    fn token_key(&self) -> TokenKey {
        TokenKey {
            resource: self.resource.as_str().to_owned(),
            issuer: self.issuer.clone(),
            client_id: self.client_id.clone(),
        }
    }
}

struct ProtectedMetadata {
    issuers: Vec<(String, Url)>,
    scopes: Vec<String>,
}

struct AuthorizationMetadata {
    authorization_endpoint: Url,
    token_endpoint: Url,
    response_iss_required: bool,
    client_id_metadata_document_supported: bool,
}

struct BearerChallenge {
    resource_metadata: Option<String>,
    scopes: Option<Vec<String>>,
}

fn is_loopback(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    )
}

fn validate_resource_url(raw: &str) -> Result<Url, OAuthError> {
    let url = Url::parse(raw).map_err(|_| OAuthError::Invalid("invalid resource URL"))?;
    if url.host_str().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || !(url.scheme() == "https" || (url.scheme() == "http" && is_loopback(&url)))
    {
        return Err(OAuthError::Invalid(
            "resource must be HTTPS or HTTP loopback without URL credentials",
        ));
    }
    Ok(url)
}

fn validate_as_url(raw: &str) -> Result<Url, OAuthError> {
    let url =
        Url::parse(raw).map_err(|_| OAuthError::Invalid("invalid authorization server URL"))?;
    let secure = url.scheme() == "https";
    #[cfg(test)]
    let secure = secure || (url.scheme() == "http" && is_loopback(&url));
    if !secure
        || url.host_str().is_none()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(OAuthError::Invalid(
            "authorization server endpoints require HTTPS without URL credentials",
        ));
    }
    Ok(url)
}

fn validate_issuer(raw: &str) -> Result<Url, OAuthError> {
    let url = validate_as_url(raw)?;
    if url.query().is_some() {
        return Err(OAuthError::Invalid(
            "authorization server issuer cannot contain a query",
        ));
    }
    Ok(url)
}

fn validate_resource_metadata_url(raw: &str, resource: &Url) -> Result<Url, OAuthError> {
    let url = validate_resource_url(raw)?;
    if url.origin() != resource.origin()
        || !url
            .path()
            .starts_with("/.well-known/oauth-protected-resource")
    {
        return Err(OAuthError::Invalid(
            "resource metadata URL must be on the resource origin",
        ));
    }
    Ok(url)
}

fn protected_resource_metadata_urls(resource: &Url) -> Vec<Url> {
    let mut specific = resource.clone();
    let path = resource.path().trim_start_matches('/');
    specific.set_path(&format!("/.well-known/oauth-protected-resource/{path}"));
    let mut root = resource.clone();
    root.set_path("/.well-known/oauth-protected-resource");
    if path.is_empty() {
        vec![root]
    } else {
        vec![specific, root]
    }
}

fn authorization_metadata_urls(issuer: &Url) -> Vec<Url> {
    let suffix = issuer.path().trim_matches('/');
    let mut oauth = issuer.clone();
    let mut oidc_inserted = issuer.clone();
    oauth.set_path(&format!("/.well-known/oauth-authorization-server/{suffix}"));
    oidc_inserted.set_path(&format!("/.well-known/openid-configuration/{suffix}"));
    if suffix.is_empty() {
        oauth.set_path("/.well-known/oauth-authorization-server");
        oidc_inserted.set_path("/.well-known/openid-configuration");
        vec![oauth, oidc_inserted]
    } else {
        let mut oidc_appended = issuer.clone();
        oidc_appended.set_path(&format!("/{suffix}/.well-known/openid-configuration"));
        vec![oauth, oidc_inserted, oidc_appended]
    }
}

fn validate_client_id(value: &str) -> Result<(), OAuthError> {
    if value.is_empty() || value.len() > 2048 || value.bytes().any(|b| b.is_ascii_control()) {
        return Err(OAuthError::Invalid("invalid OAuth client ID"));
    }
    Ok(())
}

fn parse_scope_string(raw: &str) -> Result<Vec<String>, OAuthError> {
    if raw.len() > MAX_SCOPE_BYTES {
        return Err(OAuthError::Invalid("too many OAuth scopes"));
    }
    let mut scopes = BTreeSet::new();
    for scope in raw.split_ascii_whitespace() {
        if scope.len() > 256
            || !scope
                .bytes()
                .all(|b| matches!(b, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
        {
            return Err(OAuthError::Invalid("invalid OAuth scope"));
        }
        scopes.insert(scope.to_owned());
        if scopes.len() > MAX_SCOPE_COUNT {
            return Err(OAuthError::Invalid("too many OAuth scopes"));
        }
    }
    Ok(scopes.into_iter().collect())
}

fn parse_scope_array(value: Option<&Value>) -> Result<Vec<String>, OAuthError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or(OAuthError::Invalid("invalid scopes_supported"))?;
    let mut scopes = BTreeSet::new();
    for item in array {
        let scope = item
            .as_str()
            .ok_or(OAuthError::Invalid("invalid scopes_supported"))?;
        let parsed = parse_scope_string(scope)?;
        if parsed.len() != 1 {
            return Err(OAuthError::Invalid("invalid scopes_supported"));
        }
        scopes.insert(parsed[0].clone());
        if scopes.len() > MAX_SCOPE_COUNT {
            return Err(OAuthError::Invalid("too many OAuth scopes"));
        }
    }
    Ok(scopes.into_iter().collect())
}

fn parse_bearer_challenge(header: &str) -> Result<BearerChallenge, OAuthError> {
    if header.len() > 8192 {
        return Err(OAuthError::Invalid("authorization challenge is too large"));
    }
    let lower = header.to_ascii_lowercase();
    let offset = lower
        .match_indices("bearer")
        .find(|(at, _)| {
            (*at == 0
                || header.as_bytes()[at - 1] == b','
                || header.as_bytes()[at - 1].is_ascii_whitespace())
                && header
                    .as_bytes()
                    .get(*at + "bearer".len())
                    .is_none_or(|next| *next == b',' || next.is_ascii_whitespace())
        })
        .map(|(at, _)| at)
        .ok_or(OAuthError::Invalid("missing Bearer challenge"))?;
    let mut input = &header[offset + "bearer".len()..];
    let mut resource_metadata = None;
    let mut scopes = None;
    loop {
        input = input.trim_start_matches(|c: char| c == ',' || c.is_ascii_whitespace());
        if input.is_empty() {
            break;
        }
        let key_len = input
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
            .count();
        if key_len == 0 {
            return Err(OAuthError::Invalid("invalid Bearer challenge"));
        }
        let key = &input[..key_len];
        input = input[key_len..].trim_start_matches(char::is_whitespace);
        let Some(rest) = input.strip_prefix('=') else {
            break;
        };
        input = rest.trim_start_matches(char::is_whitespace);
        let value;
        if let Some(rest) = input.strip_prefix('"') {
            let mut decoded = String::new();
            let mut escaped = false;
            let mut end = None;
            for (at, character) in rest.char_indices() {
                if escaped {
                    decoded.push(character);
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    end = Some(at);
                    break;
                } else {
                    decoded.push(character);
                }
            }
            let end = end.ok_or(OAuthError::Invalid("unterminated Bearer challenge value"))?;
            value = decoded;
            input = &rest[end + 1..];
        } else {
            let end = input
                .find(|c: char| c == ',' || c.is_ascii_whitespace())
                .unwrap_or(input.len());
            value = input[..end].to_owned();
            input = &input[end..];
        }
        match key.to_ascii_lowercase().as_str() {
            "resource_metadata" if resource_metadata.is_some() => {
                return Err(OAuthError::Invalid(
                    "duplicate resource_metadata challenge parameter",
                ))
            }
            "resource_metadata" => resource_metadata = Some(value),
            "scope" if scopes.is_some() => {
                return Err(OAuthError::Invalid("duplicate scope challenge parameter"))
            }
            "scope" => scopes = Some(parse_scope_string(&value)?),
            _ => {}
        }
    }
    Ok(BearerChallenge {
        resource_metadata,
        scopes,
    })
}

fn parse_protected_metadata(
    value: &Value,
    resource: &Url,
) -> Result<ProtectedMetadata, OAuthError> {
    let metadata_resource = value
        .get("resource")
        .and_then(Value::as_str)
        .ok_or(OAuthError::Invalid("missing protected resource identifier"))?;
    if validate_resource_url(metadata_resource)? != *resource {
        return Err(OAuthError::Invalid(
            "protected resource metadata identifies a different resource",
        ));
    }
    let servers = value
        .get("authorization_servers")
        .and_then(Value::as_array)
        .ok_or(OAuthError::Invalid("missing authorization_servers"))?;
    if servers.is_empty() || servers.len() > 8 {
        return Err(OAuthError::Invalid("invalid authorization_servers"));
    }
    let mut issuers = Vec::new();
    for item in servers {
        let raw = item
            .as_str()
            .ok_or(OAuthError::Invalid("invalid authorization server issuer"))?;
        issuers.push((raw.to_owned(), validate_issuer(raw)?));
    }
    let scopes = parse_scope_array(value.get("scopes_supported"))?;
    Ok(ProtectedMetadata { issuers, scopes })
}

async fn discover_authorization_server(
    client: &Client,
    issuer: &str,
    issuer_url: &Url,
    request_timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<AuthorizationMetadata, OAuthError> {
    for metadata_url in authorization_metadata_urls(issuer_url) {
        let Some(value) = fetch_json(
            client,
            &metadata_url,
            request_timeout,
            MAX_METADATA_BYTES,
            cancellation,
        )
        .await?
        else {
            continue;
        };
        if value.get("issuer").and_then(Value::as_str) != Some(issuer) {
            return Err(OAuthError::Invalid(
                "authorization metadata issuer mismatch",
            ));
        }
        let authorization_endpoint = validate_as_url(
            value
                .get("authorization_endpoint")
                .and_then(Value::as_str)
                .ok_or(OAuthError::Invalid("missing authorization_endpoint"))?,
        )?;
        let token_endpoint = validate_as_url(
            value
                .get("token_endpoint")
                .and_then(Value::as_str)
                .ok_or(OAuthError::Invalid("missing token_endpoint"))?,
        )?;
        let methods = value
            .get("code_challenge_methods_supported")
            .and_then(Value::as_array)
            .ok_or(OAuthError::Invalid(
                "authorization server does not advertise PKCE",
            ))?;
        if !methods.iter().any(|method| method.as_str() == Some("S256")) {
            return Err(OAuthError::Invalid(
                "authorization server does not support PKCE S256",
            ));
        }
        if let Some(grants) = value.get("grant_types_supported").and_then(Value::as_array) {
            if !grants
                .iter()
                .any(|grant| grant.as_str() == Some("authorization_code"))
            {
                return Err(OAuthError::Invalid(
                    "authorization server does not support authorization_code",
                ));
            }
        }
        return Ok(AuthorizationMetadata {
            authorization_endpoint,
            token_endpoint,
            response_iss_required: optional_metadata_bool(
                &value,
                "authorization_response_iss_parameter_supported",
            )?,
            client_id_metadata_document_supported: optional_metadata_bool(
                &value,
                "client_id_metadata_document_supported",
            )?,
        });
    }
    Err(OAuthError::MetadataUnavailable)
}

fn optional_metadata_bool(value: &Value, key: &str) -> Result<bool, OAuthError> {
    match value.get(key) {
        None => Ok(false),
        Some(value) => value.as_bool().ok_or(OAuthError::Invalid(
            "invalid authorization metadata boolean",
        )),
    }
}

async fn fetch_json(
    client: &Client,
    url: &Url,
    timeout: Duration,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Option<Value>, OAuthError> {
    let deadline = Instant::now() + timeout;
    let request = client
        .get(url.clone())
        .header(ACCEPT, "application/json")
        .send();
    let mut response = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(OAuthError::Cancelled),
        result = timeout_at(deadline, request) => result.map_err(|_| OAuthError::Timeout)?.map_err(|_| OAuthError::Network)?,
    };
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if response.status() != StatusCode::OK {
        return Err(OAuthError::Invalid("metadata endpoint rejected request"));
    }
    if !json_content_type(response.headers()) {
        return Err(OAuthError::Invalid("metadata response is not JSON"));
    }
    let bytes = read_bounded(&mut response, deadline, limit, cancellation).await?;
    let value =
        serde_json::from_slice(&bytes).map_err(|_| OAuthError::Invalid("invalid metadata JSON"))?;
    Ok(Some(value))
}

fn json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
}

async fn read_bounded(
    response: &mut reqwest::Response,
    deadline: Instant,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, OAuthError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(OAuthError::Invalid("OAuth response exceeds size limit"));
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(OAuthError::Cancelled),
            result = timeout_at(deadline, response.chunk()) => result.map_err(|_| OAuthError::Timeout)?.map_err(|_| OAuthError::Network)?,
        };
        let Some(chunk) = chunk else {
            return Ok(bytes);
        };
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(OAuthError::Invalid("OAuth response exceeds size limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
}

fn random_urlsafe() -> String {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn token_is_current(tokens: &OAuthTokens) -> bool {
    tokens
        .expires_at_unix_seconds
        .is_none_or(|expiry| expiry > unix_now().saturating_add(EXPIRY_SKEW.as_secs()))
}

impl OAuthSession {
    /// Returns a token for this exact resource, issuer and client ID. A
    /// refresh occurs before dispatch; this never retries an MCP tool call.
    pub async fn access_token(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<String, OAuthError> {
        let key = self.token_key();
        let mut tokens = self
            .store_load(&key, cancellation)
            .await?
            .ok_or(OAuthError::AuthorizationRequired)?;
        if token_is_current(&tokens) {
            return Ok(tokens.access_token);
        }
        let _guard = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(OAuthError::Cancelled),
            result = timeout_at(Instant::now() + self.request_timeout, self.refresh_guard.lock()) => result.map_err(|_| OAuthError::Timeout)?,
        };
        tokens = self
            .store_load(&key, cancellation)
            .await?
            .ok_or(OAuthError::AuthorizationRequired)?;
        if token_is_current(&tokens) {
            return Ok(tokens.access_token);
        }
        let refresh_token = tokens
            .refresh_token
            .as_deref()
            .ok_or(OAuthError::AuthorizationRequired)?;
        let params = vec![
            ("grant_type", "refresh_token".to_owned()),
            ("refresh_token", refresh_token.to_owned()),
            ("client_id", self.client_id.clone()),
            ("resource", self.resource.as_str().to_owned()),
        ];
        let result = self.post_token(&params, cancellation).await;
        let value = match result {
            Ok(value) => value,
            Err(OAuthError::AuthorizationRequired) => {
                self.store_delete_if_current(&key, &tokens, cancellation)
                    .await?;
                return Err(OAuthError::AuthorizationRequired);
            }
            Err(error) => return Err(error),
        };
        let refreshed = parse_token_response(&value, Some(&tokens), &tokens.scopes)?;
        let access = refreshed.access_token.clone();
        self.store_save_refreshed(&key, &tokens, refreshed, cancellation)
            .await?;
        Ok(access)
    }

    /// Reserve a private 127.0.0.1 callback before the browser is opened.
    /// `additional_scopes` are unioned with the initial challenge and any
    /// previously granted scopes for step-up authorization.
    pub async fn begin_authorization(
        self: &Arc<Self>,
        additional_scopes: &[String],
        cancellation: &CancellationToken,
    ) -> Result<PendingAuthorization, OAuthError> {
        if cancellation.is_cancelled() {
            return Err(OAuthError::Cancelled);
        }
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| OAuthError::Network)?;
        let port = listener
            .local_addr()
            .map_err(|_| OAuthError::Network)?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let state = random_urlsafe();
        let verifier = random_urlsafe();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut scopes = BTreeSet::new();
        for scope in &self.initial_scopes {
            scopes.insert(scope.clone());
        }
        if let Some(tokens) = self.store_load(&self.token_key(), cancellation).await? {
            for scope in tokens.scopes {
                scopes.insert(scope);
            }
        }
        for scope in additional_scopes {
            let parsed = parse_scope_string(scope)?;
            if parsed.len() != 1 {
                return Err(OAuthError::Invalid(
                    "each additional scope must be one token",
                ));
            }
            scopes.insert(parsed[0].clone());
        }
        if scopes.len() > MAX_SCOPE_COUNT
            || scopes.iter().map(String::len).sum::<usize>() > MAX_SCOPE_BYTES
        {
            return Err(OAuthError::Invalid("too many OAuth scopes"));
        }
        let scopes: Vec<_> = scopes.into_iter().collect();
        let mut authorization_url = self.authorization_endpoint.clone();
        let reserved = [
            "response_type",
            "client_id",
            "redirect_uri",
            "state",
            "code_challenge",
            "code_challenge_method",
            "scope",
            "resource",
        ];
        if authorization_url
            .query_pairs()
            .any(|(name, _)| reserved.contains(&name.as_ref()))
        {
            return Err(OAuthError::Invalid(
                "authorization endpoint has conflicting query parameters",
            ));
        }
        {
            let mut query = authorization_url.query_pairs_mut();
            query.append_pair("response_type", "code");
            query.append_pair("client_id", &self.client_id);
            query.append_pair("redirect_uri", &redirect_uri);
            query.append_pair("state", &state);
            query.append_pair("code_challenge", &challenge);
            query.append_pair("code_challenge_method", "S256");
            query.append_pair("resource", self.resource.as_str());
            if !scopes.is_empty() {
                query.append_pair("scope", &scopes.join(" "));
            }
        }
        Ok(PendingAuthorization {
            session: self.clone(),
            listener,
            authorization_url,
            redirect_uri,
            state,
            verifier,
            scopes,
        })
    }

    #[cfg(test)]
    /// Parse an insufficient-scope Bearer challenge and request its scopes in
    /// addition to those already granted. The caller must obtain user consent
    /// in a browser and must not replay an uncertain tools/call automatically.
    pub async fn begin_step_up(
        self: &Arc<Self>,
        bearer_challenge: &str,
        cancellation: &CancellationToken,
    ) -> Result<PendingAuthorization, OAuthError> {
        let challenge = parse_bearer_challenge(bearer_challenge)?;
        let scopes = challenge
            .scopes
            .ok_or(OAuthError::Invalid("scope challenge is missing scopes"))?;
        if let Some(url) = challenge.resource_metadata {
            validate_resource_metadata_url(&url, &self.resource)?;
        }
        self.begin_authorization(&scopes, cancellation).await
    }

    async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
        scopes: &[String],
        cancellation: &CancellationToken,
    ) -> Result<(), OAuthError> {
        let params = vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", code.to_owned()),
            ("redirect_uri", redirect_uri.to_owned()),
            ("client_id", self.client_id.clone()),
            ("code_verifier", verifier.to_owned()),
            ("resource", self.resource.as_str().to_owned()),
        ];
        let response = self.post_token(&params, cancellation).await?;
        let tokens = parse_token_response(&response, None, scopes)?;
        self.store_save(&self.token_key(), tokens, cancellation)
            .await
    }

    async fn post_token(
        &self,
        params: &[(&str, String)],
        cancellation: &CancellationToken,
    ) -> Result<Value, OAuthError> {
        let mut form = Url::parse("http://localhost/").expect("constant URL");
        {
            let mut query = form.query_pairs_mut();
            for (name, value) in params {
                query.append_pair(name, value);
            }
        }
        let body = form.query().unwrap_or("").to_owned();
        if body.len() > MAX_TOKEN_BYTES {
            return Err(OAuthError::Invalid("token request exceeds size limit"));
        }
        let deadline = Instant::now() + self.request_timeout;
        let request = self
            .client
            .post(self.token_endpoint.clone())
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body)
            .send();
        let mut response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(OAuthError::Cancelled),
            result = timeout_at(deadline, request) => result.map_err(|_| OAuthError::Timeout)?.map_err(|_| OAuthError::Network)?,
        };
        if response.status() != StatusCode::OK {
            return Err(match response.status() {
                StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    OAuthError::AuthorizationRequired
                }
                _ => OAuthError::Network,
            });
        }
        if !json_content_type(response.headers()) {
            return Err(OAuthError::Invalid("token response is not JSON"));
        }
        let bytes = read_bounded(&mut response, deadline, MAX_TOKEN_BYTES, cancellation).await?;
        serde_json::from_slice(&bytes)
            .map_err(|_| OAuthError::Invalid("invalid token response JSON"))
    }

    async fn store_load(
        &self,
        key: &TokenKey,
        cancellation: &CancellationToken,
    ) -> Result<Option<OAuthTokens>, OAuthError> {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(OAuthError::Cancelled),
            result = timeout_at(Instant::now() + self.request_timeout, self.store.load(key)) => result.map_err(|_| OAuthError::Timeout)?,
        }
    }

    async fn store_save(
        &self,
        key: &TokenKey,
        tokens: OAuthTokens,
        cancellation: &CancellationToken,
    ) -> Result<(), OAuthError> {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(OAuthError::Cancelled),
            result = timeout_at(Instant::now() + self.request_timeout, self.store.save(key, tokens)) => result.map_err(|_| OAuthError::Timeout)?,
        }
    }

    async fn store_save_refreshed(
        &self,
        key: &TokenKey,
        previous: &OAuthTokens,
        tokens: OAuthTokens,
        cancellation: &CancellationToken,
    ) -> Result<(), OAuthError> {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(OAuthError::Cancelled),
            result = timeout_at(Instant::now() + self.request_timeout, self.store.save_refreshed(key, previous, tokens)) => result.map_err(|_| OAuthError::Timeout)?,
        }
    }

    async fn store_delete_if_current(
        &self,
        key: &TokenKey,
        previous: &OAuthTokens,
        cancellation: &CancellationToken,
    ) -> Result<bool, OAuthError> {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(OAuthError::Cancelled),
            result = timeout_at(Instant::now() + self.request_timeout, self.store.delete_if_current(key, previous)) => result.map_err(|_| OAuthError::Timeout)?,
        }
    }
}

fn parse_token_response(
    value: &Value,
    previous: Option<&OAuthTokens>,
    requested_scopes: &[String],
) -> Result<OAuthTokens, OAuthError> {
    let access = value
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or(OAuthError::Invalid("missing access token"))?;
    if access.is_empty()
        || access.len() > MAX_TOKEN_BYTES
        || !access.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'=')
        })
    {
        return Err(OAuthError::Invalid("invalid access token"));
    }
    if value
        .get("token_type")
        .and_then(Value::as_str)
        .is_none_or(|kind| !kind.eq_ignore_ascii_case("Bearer"))
    {
        return Err(OAuthError::Invalid("token response is not Bearer"));
    }
    let refresh_token = match value.get("refresh_token") {
        Some(Value::String(token))
            if !token.is_empty()
                && token.len() <= MAX_TOKEN_BYTES
                && !token.bytes().any(|b| b.is_ascii_control()) =>
        {
            Some(token.clone())
        }
        Some(_) => return Err(OAuthError::Invalid("invalid refresh token")),
        None => previous.and_then(|tokens| tokens.refresh_token.clone()),
    };
    let expires_at_unix_seconds = match value.get("expires_in") {
        Some(Value::Number(number)) => Some(
            unix_now().saturating_add(
                number
                    .as_u64()
                    .ok_or(OAuthError::Invalid("invalid expires_in"))?,
            ),
        ),
        Some(_) => return Err(OAuthError::Invalid("invalid expires_in")),
        None => None,
    };
    let scopes = match value.get("scope") {
        Some(Value::String(scope)) => parse_scope_string(scope)?,
        Some(_) => return Err(OAuthError::Invalid("invalid token scope")),
        None => requested_scopes.to_vec(),
    };
    Ok(OAuthTokens {
        access_token: access.to_owned(),
        refresh_token,
        expires_at_unix_seconds,
        scopes,
    })
}

enum CallbackResult {
    Code(String),
    Denied,
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.bytes().zip(right.bytes()) {
        difference |= a ^ b;
    }
    difference == 0
}

fn parse_callback(
    request: &[u8],
    redirect_uri: &str,
    state: &str,
    issuer: &str,
    issuer_required: bool,
) -> Result<CallbackResult, OAuthError> {
    let head =
        std::str::from_utf8(request).map_err(|_| OAuthError::Invalid("callback is not UTF-8"))?;
    let mut lines = head.split("\r\n");
    let line = lines
        .next()
        .ok_or(OAuthError::Invalid("missing callback request line"))?;
    let mut parts = line.split(' ');
    if parts.next() != Some("GET") || parts.next().is_none() {
        return Err(OAuthError::Invalid("invalid callback request"));
    }
    let target = line
        .split(' ')
        .nth(1)
        .ok_or(OAuthError::Invalid("invalid callback target"))?;
    if parts.next() != Some("HTTP/1.1")
        || parts.next().is_some()
        || !target.starts_with('/')
        || target.starts_with("//")
    {
        return Err(OAuthError::Invalid("invalid callback request"));
    }
    let redirect =
        Url::parse(redirect_uri).map_err(|_| OAuthError::Invalid("invalid callback redirect"))?;
    let expected_host = format!(
        "127.0.0.1:{}",
        redirect
            .port()
            .ok_or(OAuthError::Invalid("missing callback port"))?
    );
    let mut host = None;
    let mut origin = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or(OAuthError::Invalid("invalid callback header"))?;
        match name.to_ascii_lowercase().as_str() {
            "host" if host.is_some() => return Err(OAuthError::Invalid("duplicate callback Host")),
            "host" => host = Some(value.trim()),
            "origin" if origin.is_some() => {
                return Err(OAuthError::Invalid("duplicate callback Origin"))
            }
            "origin" => origin = Some(value.trim()),
            _ => {}
        }
    }
    if host != Some(expected_host.as_str())
        || origin.is_some_and(|value| value != format!("http://{expected_host}"))
    {
        return Err(OAuthError::Invalid("callback origin mismatch"));
    }
    let callback = Url::parse(&format!("http://{expected_host}{target}"))
        .map_err(|_| OAuthError::Invalid("invalid callback URL"))?;
    if callback.path() != "/callback" || callback.fragment().is_some() {
        return Err(OAuthError::Invalid("callback redirect mismatch"));
    }
    let mut params = BTreeMap::new();
    for (name, value) in callback.query_pairs() {
        if matches!(name.as_ref(), "state" | "iss" | "code" | "error")
            && params
                .insert(name.into_owned(), value.into_owned())
                .is_some()
        {
            return Err(OAuthError::Invalid("duplicate callback parameter"));
        }
    }
    let returned_state = params
        .get("state")
        .ok_or(OAuthError::Invalid("missing callback state"))?;
    if !constant_time_eq(returned_state, state) {
        return Err(OAuthError::Invalid("callback state mismatch"));
    }
    match params.get("iss") {
        Some(returned_issuer) if returned_issuer != issuer => {
            return Err(OAuthError::Invalid("callback issuer mismatch"))
        }
        None if issuer_required => return Err(OAuthError::Invalid("callback issuer missing")),
        _ => {}
    }
    if params.contains_key("error") {
        return Ok(CallbackResult::Denied);
    }
    let code = params
        .get("code")
        .ok_or(OAuthError::Invalid("missing authorization code"))?;
    if code.is_empty()
        || code.len() > MAX_CALLBACK_BYTES
        || code.bytes().any(|b| b.is_ascii_control())
    {
        return Err(OAuthError::Invalid("invalid authorization code"));
    }
    Ok(CallbackResult::Code(code.clone()))
}

async fn read_callback_request(
    stream: &mut TcpStream,
    deadline: Instant,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, OAuthError> {
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0u8; 1024];
        let count = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(OAuthError::Cancelled),
            result = timeout_at(deadline, stream.read(&mut buffer)) => result.map_err(|_| OAuthError::Timeout)?.map_err(|_| OAuthError::Network)?,
        };
        if count == 0 {
            return Err(OAuthError::Invalid(
                "callback connection ended before headers",
            ));
        }
        if count > MAX_CALLBACK_BYTES.saturating_sub(bytes.len()) {
            return Err(OAuthError::Invalid("callback request too large"));
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            bytes.truncate(end + 4);
            return Ok(bytes);
        }
    }
}

async fn callback_reply(stream: &mut TcpStream, success: bool) {
    let (status, body) = if success {
        (
            "200 OK",
            "Authorization complete. You can return to the app.",
        )
    } else {
        ("400 Bad Request", "Authorization callback was rejected.")
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = timeout_at(
        Instant::now() + Duration::from_secs(1),
        stream.write_all(response.as_bytes()),
    )
    .await;
}

impl PendingAuthorization {
    /// Wait for one valid browser redirect and exchange its code exactly once.
    /// Invalid local requests are rejected without consuming the flow. The
    /// entire callback window and number of attempts are bounded.
    pub async fn finish(self, cancellation: &CancellationToken) -> Result<(), OAuthError> {
        let deadline = Instant::now() + self.session.callback_timeout;
        for _ in 0..MAX_CALLBACK_ATTEMPTS {
            let (mut stream, remote) = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(OAuthError::Cancelled),
                result = timeout_at(deadline, self.listener.accept()) => result.map_err(|_| OAuthError::Timeout)?.map_err(|_| OAuthError::Network)?,
            };
            if !remote.ip().is_loopback() {
                callback_reply(&mut stream, false).await;
                continue;
            }
            let connection_deadline = deadline.min(Instant::now() + MAX_CALLBACK_CONNECTION_TIME);
            let request =
                match read_callback_request(&mut stream, connection_deadline, cancellation).await {
                    Ok(request) => request,
                    Err(OAuthError::Cancelled) => return Err(OAuthError::Cancelled),
                    Err(OAuthError::Timeout) if Instant::now() >= deadline => {
                        return Err(OAuthError::Timeout)
                    }
                    Err(OAuthError::Timeout) => {
                        callback_reply(&mut stream, false).await;
                        continue;
                    }
                    Err(_) => {
                        callback_reply(&mut stream, false).await;
                        continue;
                    }
                };
            match parse_callback(
                &request,
                &self.redirect_uri,
                &self.state,
                &self.session.issuer,
                self.session.response_iss_required,
            ) {
                Ok(CallbackResult::Code(code)) => {
                    let result = self
                        .session
                        .exchange_code(
                            &code,
                            &self.redirect_uri,
                            &self.verifier,
                            &self.scopes,
                            cancellation,
                        )
                        .await;
                    callback_reply(&mut stream, result.is_ok()).await;
                    return result;
                }
                Ok(CallbackResult::Denied) => {
                    callback_reply(&mut stream, false).await;
                    return Err(OAuthError::Denied);
                }
                Err(_) => callback_reply(&mut stream, false).await,
            }
        }
        Err(OAuthError::Invalid("too many invalid OAuth callbacks"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{McpHttpClient, McpHttpConfig};
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::{TcpListener as StdTcpListener, TcpStream as StdTcpStream};
    use std::thread;

    struct Request {
        method: String,
        path: String,
        headers: BTreeMap<String, String>,
        body: String,
    }

    struct Reply {
        status: &'static str,
        body: Value,
    }

    fn local_server<F>(count: usize, reply: F) -> (String, thread::JoinHandle<Vec<Request>>)
    where
        F: Fn(usize, &Request, &str) -> Reply + Send + 'static,
    {
        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server_base = base.clone();
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
                                "OAuth test server timed out"
                            );
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("OAuth fixture accept failed: {error}"),
                    }
                };
                // Windows inherits a nonblocking listener's mode on accepted
                // sockets. The fixture reads a complete HTTP request with
                // blocking I/O, so set that mode explicitly before reading.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let request = read_request(&mut stream);
                let response = reply(index, &request, &server_base);
                requests.push(request);
                let body = serde_json::to_vec(&response.body).unwrap();
                let head = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.status,
                    body.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
            }
            requests
        });
        (base, handle)
    }

    fn read_request(stream: &mut StdTcpStream) -> Request {
        let mut bytes = Vec::new();
        let end = loop {
            let mut buffer = [0u8; 2048];
            let size = stream.read(&mut buffer).unwrap();
            assert!(size > 0, "OAuth client closed before headers");
            bytes.extend_from_slice(&buffer[..size]);
            assert!(bytes.len() < 100_000);
            if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let head = std::str::from_utf8(&bytes[..end]).unwrap();
        let mut first = head.lines().next().unwrap().split_whitespace();
        let method = first.next().unwrap().to_owned();
        let path = first.next().unwrap().to_owned();
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
        while bytes.len() - end < length {
            let mut buffer = [0u8; 2048];
            let size = stream.read(&mut buffer).unwrap();
            assert!(size > 0);
            bytes.extend_from_slice(&buffer[..size]);
        }
        Request {
            method,
            path,
            headers,
            body: String::from_utf8(bytes[end..end + length].to_vec()).unwrap(),
        }
    }

    #[test]
    fn discovery_uri_order_matches_mcp_spec() {
        let resource = Url::parse("https://mcp.example.com/public/mcp").unwrap();
        let urls = protected_resource_metadata_urls(&resource);
        assert_eq!(
            urls[0].as_str(),
            "https://mcp.example.com/.well-known/oauth-protected-resource/public/mcp"
        );
        assert_eq!(
            urls[1].as_str(),
            "https://mcp.example.com/.well-known/oauth-protected-resource"
        );
        let issuer = Url::parse("https://auth.example.com/tenant1").unwrap();
        let urls = authorization_metadata_urls(&issuer);
        assert_eq!(
            urls[0].as_str(),
            "https://auth.example.com/.well-known/oauth-authorization-server/tenant1"
        );
        assert_eq!(
            urls[1].as_str(),
            "https://auth.example.com/.well-known/openid-configuration/tenant1"
        );
        assert_eq!(
            urls[2].as_str(),
            "https://auth.example.com/tenant1/.well-known/openid-configuration"
        );
    }

    #[test]
    fn malformed_security_metadata_is_rejected() {
        assert_eq!(
            optional_metadata_bool(
                &json!({"authorization_response_iss_parameter_supported": "false"}),
                "authorization_response_iss_parameter_supported"
            ),
            Err(OAuthError::Invalid(
                "invalid authorization metadata boolean"
            ))
        );
        assert_eq!(
            optional_metadata_bool(&json!({}), "authorization_response_iss_parameter_supported"),
            Ok(false)
        );
        assert!(matches!(
            parse_bearer_challenge("BearerX scope=write"),
            Err(OAuthError::Invalid("missing Bearer challenge"))
        ));
    }

    #[test]
    fn callback_rejects_state_issuer_origin_and_duplicate_values() {
        let redirect = "http://127.0.0.1:41234/callback";
        let valid = b"GET /callback?state=abc&iss=https%3A%2F%2Fauth.example&code=ok HTTP/1.1\r\nHost: 127.0.0.1:41234\r\n\r\n";
        assert!(
            matches!(parse_callback(valid, redirect, "abc", "https://auth.example", true), Ok(CallbackResult::Code(code)) if code == "ok")
        );
        for request in [
            b"GET /callback?state=wrong&iss=https%3A%2F%2Fauth.example&code=ok HTTP/1.1\r\nHost: 127.0.0.1:41234\r\n\r\n".as_slice(),
            b"GET /callback?state=abc&iss=https%3A%2F%2Fevil.example&code=ok HTTP/1.1\r\nHost: 127.0.0.1:41234\r\n\r\n",
            b"GET /callback?state=abc&code=ok HTTP/1.1\r\nHost: 127.0.0.1:41234\r\n\r\n",
            b"GET /callback?state=abc&state=abc&iss=https%3A%2F%2Fauth.example&code=ok HTTP/1.1\r\nHost: 127.0.0.1:41234\r\n\r\n",
            b"GET /callback?state=abc&iss=https%3A%2F%2Fauth.example&code=ok HTTP/1.1\r\nHost: 127.0.0.1:41234\r\nOrigin: https://evil.example\r\n\r\n",
        ] {
            assert!(parse_callback(request, redirect, "abc", "https://auth.example", true).is_err());
        }
    }

    #[tokio::test]
    async fn challenge_metadata_takes_precedence_and_mismatched_issuer_is_rejected() {
        let (base, server) = local_server(2, |index, request, base| match index {
            0 => {
                assert_eq!(request.path, "/.well-known/oauth-protected-resource");
                Reply {
                    status: "200 OK",
                    body: json!({
                        "resource": format!("{base}/mcp"),
                        "authorization_servers": [format!("{base}/issuer")]
                    }),
                }
            }
            1 => {
                assert_eq!(
                    request.path,
                    "/.well-known/oauth-authorization-server/issuer"
                );
                Reply {
                    status: "200 OK",
                    body: json!({
                        "issuer": "https://different.example",
                        "authorization_endpoint": format!("{base}/authorize"),
                        "token_endpoint": format!("{base}/token"),
                        "code_challenge_methods_supported": ["S256"]
                    }),
                }
            }
            _ => unreachable!(),
        });
        let config = OAuthConfig::new(
            format!("{base}/mcp"),
            OAuthRegistration::PreRegistered {
                client_id: "native-client".into(),
                issuer: format!("{base}/issuer"),
            },
        );
        let challenge = format!(
            "Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource\", scope=\"write\""
        );
        assert!(matches!(
            OAuthSession::discover(
                config,
                Some(&challenge),
                Arc::new(MemoryTokenStore::default()),
                &CancellationToken::new()
            )
            .await,
            Err(OAuthError::Invalid(
                "authorization metadata issuer mismatch"
            ))
        ));
        assert_eq!(server.join().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn transient_refresh_error_preserves_stored_tokens() {
        let (base, server) = local_server(3, |index, request, base| match index {
            0 => Reply {
                status: "200 OK",
                body: json!({
                    "resource": format!("{base}/mcp"),
                    "authorization_servers": [format!("{base}/issuer")]
                }),
            },
            1 => Reply {
                status: "200 OK",
                body: json!({
                    "issuer": format!("{base}/issuer"),
                    "authorization_endpoint": format!("{base}/authorize"),
                    "token_endpoint": format!("{base}/token"),
                    "code_challenge_methods_supported": ["S256"]
                }),
            },
            2 => {
                assert_eq!(request.method, "POST");
                assert_eq!(request.path, "/token");
                Reply {
                    status: "503 Service Unavailable",
                    body: json!({"error":"temporarily_unavailable"}),
                }
            }
            _ => unreachable!(),
        });
        let store = Arc::new(MemoryTokenStore::default());
        let cancellation = CancellationToken::new();
        let session = OAuthSession::discover(
            OAuthConfig::new(
                format!("{base}/mcp"),
                OAuthRegistration::PreRegistered {
                    client_id: "native-client".into(),
                    issuer: format!("{base}/issuer"),
                },
            ),
            None,
            store.clone(),
            &cancellation,
        )
        .await
        .unwrap();
        let key = session.token_key();
        store
            .save(
                &key,
                OAuthTokens {
                    access_token: "expired".into(),
                    refresh_token: Some("refresh".into()),
                    expires_at_unix_seconds: Some(unix_now().saturating_sub(1)),
                    scopes: vec!["read".into()],
                },
            )
            .await
            .unwrap();
        assert_eq!(
            session.access_token(&cancellation).await,
            Err(OAuthError::Network)
        );
        assert!(store.load(&key).await.unwrap().is_some());
        assert_eq!(server.join().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn local_discovery_code_exchange_refresh_and_mcp_auth_header() {
        let (base, server) = local_server(7, |index, request, base| match index {
            0 => {
                assert_eq!(request.path, "/.well-known/oauth-protected-resource/mcp");
                Reply {
                    status: "200 OK",
                    body: json!({
                        "resource": format!("{base}/mcp"),
                        "authorization_servers": [format!("{base}/issuer")],
                        "scopes_supported": ["read"]
                    }),
                }
            }
            1 => {
                assert_eq!(
                    request.path,
                    "/.well-known/oauth-authorization-server/issuer"
                );
                Reply {
                    status: "200 OK",
                    body: json!({
                        "issuer": format!("{base}/issuer"),
                        "authorization_endpoint": format!("{base}/authorize"),
                        "token_endpoint": format!("{base}/token"),
                        "code_challenge_methods_supported": ["S256"],
                        "authorization_response_iss_parameter_supported": true
                    }),
                }
            }
            2 => {
                assert_eq!(request.method, "POST");
                assert_eq!(request.path, "/token");
                Reply {
                    status: "200 OK",
                    body: json!({"access_token":"access1", "token_type":"Bearer", "expires_in":3600, "refresh_token":"refresh1", "scope":"write"}),
                }
            }
            3 => {
                assert_eq!(request.path, "/token");
                Reply {
                    status: "200 OK",
                    body: json!({"access_token":"access2", "token_type":"Bearer", "expires_in":3600, "refresh_token":"refresh2", "scope":"write"}),
                }
            }
            4 => {
                assert_eq!(request.path, "/mcp");
                assert_eq!(request.headers["authorization"], "Bearer access2");
                let id = serde_json::from_str::<Value>(&request.body).unwrap()["id"].clone();
                Reply {
                    status: "200 OK",
                    body: json!({"jsonrpc":"2.0","id":id,"result":{
                        "resultType":"complete", "supportedVersions":["2026-07-28"],
                        "capabilities":{"tools":{}}, "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"oauth fixture","version":"1"}}
                    }}),
                }
            }
            5 => {
                let message: Value = serde_json::from_str(&request.body).unwrap();
                assert_eq!(message["method"], "tools/list");
                Reply {
                    status: "200 OK",
                    body: json!({"jsonrpc":"2.0","id":message["id"],"result":{
                        "resultType":"complete", "tools":[{"name":"echo","inputSchema":{"type":"object"}}]
                    }}),
                }
            }
            6 => {
                let message: Value = serde_json::from_str(&request.body).unwrap();
                assert_eq!(message["method"], "tools/call");
                assert_eq!(request.headers["authorization"], "Bearer access2");
                Reply {
                    status: "401 Unauthorized",
                    body: json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"expired token"}}),
                }
            }
            _ => unreachable!(),
        });
        let cancellation = CancellationToken::new();
        let store = Arc::new(MemoryTokenStore::default());
        let resource = format!("{base}/mcp");
        let mut config = OAuthConfig::new(
            resource.clone(),
            OAuthRegistration::PreRegistered {
                client_id: "native-client".into(),
                issuer: format!("{base}/issuer"),
            },
        );
        config.callback_timeout = Duration::from_secs(3);
        let session = Arc::new(
            OAuthSession::discover(
                config,
                Some("Bearer scope=\"write\""),
                store.clone(),
                &cancellation,
            )
            .await
            .unwrap(),
        );
        assert_eq!(session.initial_scopes(), &["write"]);
        let pending = session
            .begin_authorization(&[], &cancellation)
            .await
            .unwrap();
        let authorization_url = pending.url().clone();
        let redirect_uri = pending.redirect_uri().to_owned();
        let params: BTreeMap<_, _> = authorization_url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(params["resource"], resource);
        assert_eq!(params["scope"], "write");
        assert_eq!(params["code_challenge_method"], "S256");
        let state = params["state"].clone();
        let finish_cancel = cancellation.clone();
        let finish = tokio::spawn(async move { pending.finish(&finish_cancel).await });
        let browser = Client::builder().no_proxy().build().unwrap();
        let mut wrong = Url::parse(&redirect_uri).unwrap();
        wrong
            .query_pairs_mut()
            .append_pair("state", "wrong")
            .append_pair("iss", &format!("{base}/issuer"))
            .append_pair("code", "code1");
        assert_eq!(
            browser.get(wrong).send().await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        let mut wrong_issuer = Url::parse(&redirect_uri).unwrap();
        wrong_issuer
            .query_pairs_mut()
            .append_pair("state", &state)
            .append_pair("iss", "https://attacker.example")
            .append_pair("code", "code1");
        assert_eq!(
            browser.get(wrong_issuer).send().await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        let mut correct = Url::parse(&redirect_uri).unwrap();
        correct
            .query_pairs_mut()
            .append_pair("state", &state)
            .append_pair("iss", &format!("{base}/issuer"))
            .append_pair("code", "code1");
        assert_eq!(
            browser.get(correct).send().await.unwrap().status(),
            StatusCode::OK
        );
        finish.await.unwrap().unwrap();
        let key = TokenKey {
            resource: resource.clone(),
            issuer: format!("{base}/issuer"),
            client_id: "native-client".into(),
        };
        let mut saved = store.load(&key).await.unwrap().unwrap();
        assert_eq!(saved.access_token, "access1");
        saved.expires_at_unix_seconds = Some(0);
        store.save(&key, saved).await.unwrap();
        let grant_before_refresh = session
            .resume_authorization_identity(&cancellation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            session.access_token(&cancellation).await.unwrap(),
            "access2"
        );
        assert_eq!(
            session
                .resume_authorization_identity(&cancellation)
                .await
                .unwrap(),
            Some(grant_before_refresh)
        );
        let step_up = session
            .begin_step_up(
                "Bearer error=\"insufficient_scope\", scope=\"admin\"",
                &cancellation,
            )
            .await
            .unwrap();
        let step_up_scopes = step_up
            .url()
            .query_pairs()
            .find(|(name, _)| name == "scope")
            .unwrap()
            .1
            .into_owned();
        assert_eq!(step_up_scopes, "admin write");
        drop(step_up);
        let client =
            McpHttpClient::connect_with_oauth(McpHttpConfig::new(resource), session, &cancellation)
                .await
                .unwrap();
        assert_eq!(client.server_info().await.name, "oauth fixture");
        assert_eq!(client.list_tools(&cancellation).await.unwrap().len(), 1);
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(crate::McpError::EffectUncertain(_))
        ));
        assert_eq!(
            store
                .load(&key)
                .await
                .unwrap()
                .unwrap()
                .refresh_token
                .as_deref(),
            Some("refresh2")
        );
        // A fresh authorization can happen in Settings while this client is
        // still open. The next call must stop before POST, even though the
        // new token would also be accepted by the server.
        store
            .save(
                &key,
                OAuthTokens {
                    access_token: "replacement".into(),
                    refresh_token: Some("replacement-refresh".into()),
                    expires_at_unix_seconds: Some(unix_now().saturating_add(3600)),
                    scopes: vec!["write".into()],
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            client.call_tool("echo", json!({}), &cancellation).await,
            Err(crate::McpError::AuthorizationChanged)
        ));
        client.shutdown().await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| {
                    serde_json::from_str::<Value>(&request.body)
                        .ok()
                        .is_some_and(|value| value["method"] == "tools/call")
                })
                .count(),
            1
        );
        let code: BTreeMap<_, _> = Url::parse(&format!("http://localhost/?{}", requests[2].body))
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(code["resource"], format!("{base}/mcp"));
        assert_eq!(code["redirect_uri"], redirect_uri);
        assert_eq!(code["client_id"], "native-client");
        assert_eq!(code["code"], "code1");
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(code["code_verifier"].as_bytes())),
            params["code_challenge"]
        );
        let refresh: BTreeMap<_, _> =
            Url::parse(&format!("http://localhost/?{}", requests[3].body))
                .unwrap()
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
        assert_eq!(refresh["refresh_token"], "refresh1");
        assert_eq!(refresh["resource"], format!("{base}/mcp"));
        assert_eq!(
            store
                .load(&key)
                .await
                .unwrap()
                .unwrap()
                .refresh_token
                .as_deref(),
            Some("replacement-refresh")
        );
    }

    #[tokio::test]
    async fn grant_identity_survives_refresh_but_rotates_on_new_authorization() {
        let store = MemoryTokenStore::default();
        let key = TokenKey {
            resource: "https://mcp.example.test/mcp".into(),
            issuer: "https://issuer.example.test".into(),
            client_id: "client".into(),
        };
        let first = OAuthTokens {
            access_token: "access-one".into(),
            refresh_token: Some("refresh-one".into()),
            expires_at_unix_seconds: Some(100),
            scopes: vec!["read".into()],
        };
        store.save(&key, first.clone()).await.unwrap();
        let before = store.resume_grant_identity(&key).await.unwrap().unwrap();
        let refreshed = OAuthTokens {
            access_token: "access-two".into(),
            refresh_token: Some("refresh-two".into()),
            expires_at_unix_seconds: Some(200),
            scopes: vec!["read".into()],
        };
        store
            .save_refreshed(&key, &first, refreshed.clone())
            .await
            .unwrap();
        assert_eq!(
            store.resume_grant_identity(&key).await.unwrap(),
            Some(before.clone())
        );
        assert_eq!(
            store.save_refreshed(&key, &first, refreshed.clone()).await,
            Err(OAuthError::AuthorizationRequired)
        );
        let expanded = OAuthTokens {
            scopes: vec!["read".into(), "write".into()],
            ..refreshed.clone()
        };
        store
            .save_refreshed(&key, &refreshed, expanded.clone())
            .await
            .unwrap();
        assert_ne!(
            store.resume_grant_identity(&key).await.unwrap(),
            Some(before.clone())
        );
        store.save(&key, expanded).await.unwrap();
        assert_ne!(
            store.resume_grant_identity(&key).await.unwrap(),
            Some(before)
        );
        assert!(!store.delete_if_current(&key, &first).await.unwrap());
        assert!(store.resume_grant_identity(&key).await.unwrap().is_some());
        let current = store.load(&key).await.unwrap().unwrap();
        assert!(store.delete_if_current(&key, &current).await.unwrap());
        assert_eq!(store.resume_grant_identity(&key).await.unwrap(), None);
    }
}
