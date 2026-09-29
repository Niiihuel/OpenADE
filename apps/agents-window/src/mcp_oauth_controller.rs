//! Interactive OAuth setup for configured MCP HTTP servers.
//!
//! A 401 challenge is discovered with a read-only Streamable HTTP GET. The
//! authorization URL remains in the local UI state and never enters settings
//! or an agent journal. The loopback callback persists tokens in the Windows
//! user vault before reporting success.

use crate::mcp_settings::McpServerSettings;
use openide_harness::{McpHttpConfig, OAuthError, OAuthSession, TokenKey, TokenStore};
use reqwest::header::{ACCEPT, WWW_AUTHENTICATE};
use reqwest::{Client, StatusCode, Url};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CHALLENGE_BYTES: usize = 8192;

/// The UI displays these events only for the matching saved server ID.
/// Deliberately has no Debug implementation: the browser URL contains state.
pub(crate) enum McpOAuthEvent {
    NeedBrowser { server_id: String, url: String },
    Authorized { server_id: String },
    Failed { server_id: String, message: String },
}

pub(crate) struct McpOAuthController {
    server_id: String,
    cancel: CancellationToken,
    events: mpsc::Receiver<McpOAuthEvent>,
    terminal_seen: AtomicBool,
    worker: Option<JoinHandle<()>>,
}

impl McpOAuthController {
    pub fn start(server: McpServerSettings) -> Result<Self, String> {
        if server.oauth_config()?.is_none() {
            return Err("El servidor MCP no tiene OAuth configurado".to_owned());
        }
        let store = platform_token_store()?;
        let server_id = server.id.clone();
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let (sender, events) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("mcp-oauth".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                let result = match runtime {
                    Ok(runtime) => {
                        runtime.block_on(authorize(&server, store, &sender, &worker_cancel))
                    }
                    Err(_) => Err("No se pudo iniciar la autorización MCP".to_owned()),
                };
                match result {
                    Ok(()) if !worker_cancel.is_cancelled() => {
                        let _ = sender.send(McpOAuthEvent::Authorized {
                            server_id: server.id,
                        });
                    }
                    Err(error) if !worker_cancel.is_cancelled() => {
                        let _ = sender.send(McpOAuthEvent::Failed {
                            server_id: server.id,
                            message: error,
                        });
                    }
                    _ => {}
                }
            })
            .map_err(|_| "No se pudo iniciar la autorización MCP".to_owned())?;
        Ok(Self {
            server_id,
            cancel,
            events,
            terminal_seen: AtomicBool::new(false),
            worker: Some(worker),
        })
    }

    pub fn try_recv(&self) -> Option<McpOAuthEvent> {
        match self.events.try_recv() {
            Ok(event) => {
                if matches!(
                    &event,
                    McpOAuthEvent::Authorized { .. } | McpOAuthEvent::Failed { .. }
                ) {
                    self.terminal_seen.store(true, Ordering::Release);
                }
                Some(event)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected)
                if !self.terminal_seen.swap(true, Ordering::AcqRel) =>
            {
                Some(McpOAuthEvent::Failed {
                    server_id: self.server_id.clone(),
                    message: "La autorización MCP terminó sin resultado".to_owned(),
                })
            }
            Err(mpsc::TryRecvError::Disconnected) => None,
        }
    }

    /// Wait for the worker, including any Credential Manager write already
    /// dispatched, before the caller removes this registration's token.
    pub fn cancel(mut self) {
        self.cancel.cancel();
        self.join_worker();
    }

    fn join_worker(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for McpOAuthController {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.join_worker();
    }
}

async fn authorize(
    server: &McpServerSettings,
    store: Arc<dyn TokenStore>,
    sender: &mpsc::Sender<McpOAuthEvent>,
    cancellation: &CancellationToken,
) -> Result<(), String> {
    let config = server
        .oauth_config()?
        .ok_or_else(|| "El servidor MCP no tiene OAuth configurado".to_owned())?;
    let challenge = probe_bearer_challenge(&config.resource_url, cancellation).await?;
    let session = Arc::new(
        OAuthSession::discover(config, challenge.as_deref(), store, cancellation)
            .await
            .map_err(oauth_error)?,
    );
    let requested = &server.http_oauth.as_ref().expect("validated OAuth").scopes;
    let pending = session
        .begin_authorization(requested, cancellation)
        .await
        .map_err(oauth_error)?;
    sender
        .send(McpOAuthEvent::NeedBrowser {
            server_id: server.id.clone(),
            url: pending.url().as_str().to_owned(),
        })
        .map_err(|_| "La ventana MCP dejó de esperar autorización".to_owned())?;
    pending.finish(cancellation).await.map_err(oauth_error)
}

/// GET is the Streamable HTTP notification request and has no tool effect.
/// A server may reject it with 405; OAuthSession then searches the required
/// well-known metadata paths instead.
pub(crate) async fn probe_bearer_challenge(
    resource_url: &str,
    cancellation: &CancellationToken,
) -> Result<Option<String>, String> {
    McpHttpConfig::new(resource_url)
        .validate()
        .map_err(|_| "URL MCP HTTP no válida".to_owned())?;
    let endpoint = Url::parse(resource_url).map_err(|_| "URL MCP HTTP no válida".to_owned())?;
    let mut builder = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(PROBE_TIMEOUT);
    if matches!(
        endpoint.host_str(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    ) {
        builder = builder.no_proxy();
    }
    let client = builder
        .build()
        .map_err(|_| "No se pudo preparar el descubrimiento OAuth MCP".to_owned())?;
    let request = client
        .get(endpoint)
        .header(ACCEPT, "text/event-stream")
        .send();
    let response = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err("Autorización MCP cancelada".to_owned()),
        response = request => response.map_err(|_| "No se pudo consultar el servidor MCP para OAuth".to_owned())?,
    };
    if response.status() != StatusCode::UNAUTHORIZED {
        return Ok(None);
    }
    for header in response.headers().get_all(WWW_AUTHENTICATE) {
        let Ok(value) = header.to_str() else { continue };
        if value.len() > MAX_CHALLENGE_BYTES {
            return Err("La respuesta de autorización MCP excede el límite".to_owned());
        }
        if value.to_ascii_lowercase().contains("bearer") {
            return Ok(Some(value.to_owned()));
        }
    }
    Ok(None)
}

pub(crate) async fn discover_oauth_session(
    server: &McpServerSettings,
    cancellation: &CancellationToken,
) -> Result<Arc<OAuthSession>, String> {
    let config = server
        .oauth_config()?
        .ok_or_else(|| "El servidor MCP no tiene OAuth configurado".to_owned())?;
    let challenge = probe_bearer_challenge(&config.resource_url, cancellation).await?;
    let store = platform_token_store()?;
    let inspect_store = Arc::clone(&store);
    let session = Arc::new(
        OAuthSession::discover(config, challenge.as_deref(), store, cancellation)
            .await
            .map_err(oauth_error)?,
    );
    // Refresh before connecting, and fail visibly when the user must authorize.
    session
        .access_token(cancellation)
        .await
        .map_err(oauth_error)?;
    let oauth = server.http_oauth.as_ref().expect("validated OAuth");
    if !oauth.scopes.is_empty() {
        let key = TokenKey {
            resource: session.resource_url().to_owned(),
            issuer: session.issuer().to_owned(),
            client_id: oauth.client_id.clone(),
        };
        let stored = inspect_store
            .load(&key)
            .await
            .map_err(oauth_error)?
            .ok_or_else(|| oauth_error(OAuthError::AuthorizationRequired))?;
        if oauth
            .scopes
            .iter()
            .any(|scope| !stored.scopes.contains(scope))
        {
            return Err("La credencial MCP no incluye los scopes configurados. Autorizá de nuevo en Settings > MCP.".to_owned());
        }
    }
    Ok(session)
}

fn oauth_error(error: OAuthError) -> String {
    match error {
        OAuthError::AuthorizationRequired => {
            "Autorización MCP necesaria. Abrí Settings > MCP y elegí Autorizar.".to_owned()
        }
        OAuthError::Cancelled => "Autorización MCP cancelada".to_owned(),
        OAuthError::Denied => "El usuario denegó la autorización MCP".to_owned(),
        OAuthError::Timeout => "La autorización MCP agotó el tiempo".to_owned(),
        OAuthError::Store => "No se pudo acceder a Credential Manager para MCP".to_owned(),
        _ => format!("No se pudo completar OAuth MCP: {error}"),
    }
}

fn platform_token_store() -> Result<Arc<dyn TokenStore>, String> {
    #[cfg(windows)]
    {
        Ok(Arc::new(openide_harness::WindowsCredentialTokenStore::new()))
    }
    #[cfg(not(windows))]
    {
        Err("OAuth MCP persistente requiere Credential Manager en Windows".to_owned())
    }
}

fn oauth_token_key(server: &McpServerSettings) -> Result<Option<TokenKey>, String> {
    let Some(oauth) = &server.http_oauth else {
        return Ok(None);
    };
    let _ = server.oauth_config()?;
    let raw = server
        .url
        .as_ref()
        .ok_or_else(|| "El servidor MCP OAuth no tiene URL".to_owned())?;
    let resource = Url::parse(raw)
        .map_err(|_| "La URL del servidor MCP OAuth no es válida".to_owned())?
        .as_str()
        .to_owned();
    Ok(Some(TokenKey {
        resource,
        issuer: oauth.issuer.clone(),
        client_id: oauth.client_id.clone(),
    }))
}

/// Scopes can change without invalidating the principal's vault entry. A
/// change of resource, issuer, or client ID creates a different credential
/// target; callers should remove the old one before committing the edit.
pub(crate) fn oauth_registration_changed(
    before: &McpServerSettings,
    after: &McpServerSettings,
) -> bool {
    match (
        &before.url,
        &before.http_oauth,
        &after.url,
        &after.http_oauth,
    ) {
        (Some(old_url), Some(old), Some(new_url), Some(new)) => {
            let resource_changed = match (Url::parse(old_url), Url::parse(new_url)) {
                (Ok(old_url), Ok(new_url)) => old_url != new_url,
                _ => true,
            };
            resource_changed || old.issuer != new.issuer || old.client_id != new.client_id
        }
        (_, Some(_), _, _) => true,
        _ => false,
    }
}

/// Idempotently remove the old registration's token before deleting or
/// replacing its settings. This waits for Credential Manager to finish so a
/// failure can stop the settings mutation and be shown to the user.
pub(crate) fn forget_oauth_registration(server: &McpServerSettings) -> Result<(), String> {
    let Some(key) = oauth_token_key(server)? else {
        return Ok(());
    };
    #[cfg(windows)]
    {
        let store = platform_token_store()?;
        std::thread::Builder::new()
            .name("mcp-oauth-forget".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "No se pudo iniciar el borrado de credenciales MCP".to_owned())?;
                runtime.block_on(store.delete(&key)).map_err(oauth_error)
            })
            .map_err(|_| "No se pudo iniciar el borrado de credenciales MCP".to_owned())?
            .join()
            .map_err(|_| "Falló el borrado de credenciales MCP".to_owned())?
    }
    #[cfg(not(windows))]
    {
        // The current host has no Windows Credential Manager entry to remove.
        let _ = key;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_settings::McpHttpOAuthSettings;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn cancel_waits_for_worker_before_credential_cleanup_can_continue() {
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = Arc::clone(&finished);
        let worker = std::thread::spawn(move || {
            while !worker_cancel.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            worker_finished.store(true, Ordering::Release);
        });
        let (_, events) = mpsc::channel();
        let controller = McpOAuthController {
            server_id: "test".to_owned(),
            cancel,
            events,
            terminal_seen: AtomicBool::new(false),
            worker: Some(worker),
        };
        controller.cancel();
        assert!(finished.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn local_probe_reads_bearer_challenge_without_following_redirects_or_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = vec![0u8; 4096];
            let count = stream.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..count]);
            assert!(request.starts_with("GET /mcp HTTP/1.1"));
            assert!(request
                .to_ascii_lowercase()
                .contains("accept: text/event-stream"));
            stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer resource_metadata=\"http://127.0.0.1/.well-known/oauth-protected-resource\", scope=\"read\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let challenge = probe_bearer_challenge(&url, &CancellationToken::new())
            .await
            .unwrap();
        assert!(challenge.unwrap().contains("scope=\"read\""));
        server.join().unwrap();
    }

    #[test]
    fn registration_changes_only_when_credential_principal_changes() {
        let before = McpServerSettings {
            id: "remote".into(),
            enabled: true,
            program: String::new(),
            args: Vec::new(),
            cwd: None,
            credential_env_names: Vec::new(),
            url: Some("https://mcp.example.test".into()),
            http_bearer_token_env_name: None,
            http_oauth: Some(McpHttpOAuthSettings {
                client_id: "public-client".into(),
                issuer: "https://auth.example.test".into(),
                scopes: vec!["read".into()],
            }),
        };
        let key = oauth_token_key(&before).unwrap().unwrap();
        assert_eq!(key.resource, "https://mcp.example.test/");
        let mut after = before.clone();
        after.enabled = false;
        after.http_oauth.as_mut().unwrap().scopes = vec!["write".into()];
        assert!(!oauth_registration_changed(&before, &after));
        after.url = Some("https://other.example.test/mcp".into());
        assert!(oauth_registration_changed(&before, &after));
        after = before.clone();
        after.http_oauth.as_mut().unwrap().issuer = "https://another.example.test".into();
        assert!(oauth_registration_changed(&before, &after));
        after = before.clone();
        after.http_oauth.as_mut().unwrap().client_id = "another-client".into();
        assert!(oauth_registration_changed(&before, &after));
        after.http_oauth = None;
        assert!(oauth_registration_changed(&before, &after));
    }
}
