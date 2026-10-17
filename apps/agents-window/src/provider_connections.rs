//! Provider sign-in and protected credentials, separate from preferences and
//! model catalogs. Browser authorization runs only after an explicit UI action.
//! No secret-bearing type implements Debug, and HTTP errors never echo bodies.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::Url;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::native_provider::{self, ProviderModelDiscovery};
// The remote host uses this exact credential store and refresh lock as well.
use openide_connections::{client, credential_dir, json_response, load, lock, now, random, request_error, revoke_codex, save, update_tokens, valid_secret, verify_identity, Credential, RESOURCE, TOKEN_URL};
#[cfg(test)]
use openide_connections::key_provider;
pub(crate) use openide_connections::{
    codex_token_source, connectable, read_key, refresh_due, service, supported, KEY_PROVIDERS, PROVIDERS,
};
use openide_connections::refresh_codex;


pub(crate) const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/settings/usage";
pub(crate) fn name(id: &str) -> &str {
    PROVIDERS
        .iter()
        .chain(KEY_PROVIDERS)
        .find(|row| row.0 == id)
        .map_or(id, |row| row.1)
}
pub(crate) fn account_url(id: &str) -> &'static str {
    match id {
        "codex" => CHATGPT_USAGE_URL,
        "openrouter" => "https://openrouter.ai/settings/keys",
        "opencode" => "https://opencode.ai/auth",
        "nvidia" => "https://build.nvidia.com/",
        "minimax" => "https://platform.minimax.io/console/access",
        "zai" => "https://z.ai/manage-apikey/apikey-list",
        "openai" => "https://platform.openai.com/api-keys",
        "anthropic" => "https://console.anthropic.com/settings/keys",
        "xai" => "https://console.x.ai/",
        "deepseek" => "https://platform.deepseek.com/api_keys",
        "mistral" => "https://console.mistral.ai/api-keys",
        "groq" => "https://console.groq.com/keys",
        _ => "https://openrouter.ai/",
    }
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ConnectionSummary {
    pub configured: bool,
    pub needs_sign_in: bool,
    pub account: Option<String>,
    pub service: String,
    pub oauth: bool,
    pub from_environment: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ConnectPhase {
    Entry,
    Preparing,
    Browser,
    Checking,
    Error,
    Welcome,
}

#[derive(Clone, Debug)]
pub(crate) struct ConnectView {
    pub provider_id: String,
    pub phase: ConnectPhase,
    pub service: String,
    pub key_entered: bool,
    pub error: Option<String>,
}

impl ConnectView {
    pub(crate) fn new(id: &str, service: &str) -> Self {
        Self {
            provider_id: id.into(),
            phase: ConnectPhase::Entry,
            service: service.into(),
            key_entered: false,
            error: None,
        }
    }
}

pub(crate) fn summaries(root: &Path) -> BTreeMap<String, ConnectionSummary> {
    PROVIDERS
        .iter()
        .map(|(id, _, _)| {
            let record = load(root, id);
            let summary = match record {
                Ok(Some(r)) => ConnectionSummary {
                    configured: !r.disabled && valid_secret(&r.access_token),
                    needs_sign_in: !r.disabled && r.expires_at.is_some_and(|t| t <= now() + 5),
                    account: r.email,
                    service: r.service,
                    oauth: r.oauth,
                    from_environment: false,
                },
                Ok(None) => {
                    let env = match *id {
                        "openrouter" => "OPENROUTER_API_KEY",
                        "opencode" => "OPENCODE_API_KEY",
                        "nvidia" => "NVIDIA_API_KEY",
                        "minimax" => "MINIMAX_API_KEY",
                        "zai" => "ZAI_API_KEY",
                        _ => "OPENIDE_CODEX_ACCESS_TOKEN",
                    };
                    ConnectionSummary {
                        configured: read_key(root, env).is_some(),
                        service: "zen".into(),
                        from_environment: *id != "codex",
                        ..Default::default()
                    }
                }
                Err(_) => ConnectionSummary {
                    needs_sign_in: true,
                    ..Default::default()
                },
            };
            ((*id).into(), summary)
        })
        .collect()
}

pub(crate) enum ConnectionEvent {
    Browser(String),
    Checking,
    Connected {
        provider_id: String,
        // None means the key was saved locally: this provider does not expose
        // a documented model listing. It is not a successful authentication.
        discovery: Option<ProviderModelDiscovery>,
        welcome: bool,
    },
    Disconnected {
        warning: Option<String>,
    },
    Refreshed,
    Failed(String),
}
pub(crate) struct ConnectionWorker {
    pub receiver: Receiver<ConnectionEvent>,
    cancel: Arc<AtomicBool>,
}
impl Drop for ConnectionWorker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
impl ConnectionWorker {
    fn spawn(
        work: impl FnOnce(Sender<ConnectionEvent>, Arc<AtomicBool>) -> Result<ConnectionEvent, String>
            + Send
            + 'static,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let task_cancel = cancel.clone();
        thread::spawn(move || {
            let result = work(sender.clone(), task_cancel.clone());
            if !task_cancel.load(Ordering::Relaxed) {
                let _ = sender.send(result.unwrap_or_else(ConnectionEvent::Failed));
            }
        });
        Self { receiver, cancel }
    }
    pub(crate) fn oauth(root: PathBuf, id: String) -> Self {
        Self::spawn(move |sender, cancel| oauth(&root, &id, &sender, &cancel))
    }
    pub(crate) fn api_key(root: PathBuf, id: String, key: String, service: String) -> Self {
        Self::spawn(move |sender, cancel| {
            if !matches!(
                id.as_str(),
                "opencode" | "nvidia" | "openrouter" | "minimax" | "zai" | "openai" | "anthropic" | "xai" | "deepseek" | "mistral" | "groq"
            ) || !valid_secret(key.trim())
                || key.len() > 4096
            {
                return Err("Introduce una clave API válida".into());
            }
            let _guard = lock(&root, &id, &cancel)?;
            let mut adapter = if id == "opencode" {
                native_provider::ProviderRegistry::builtin()
                    .adapter(&id)
                    .cloned()
            } else {
                native_provider::adapter_at(&root, &id)?
            }
            .ok_or("Proveedor no disponible")?;
            if id == "opencode" && service == "go" {
                adapter.use_opencode_go();
            }
            let _ = sender.send(ConnectionEvent::Checking);
            if id == "openrouter" {
                validate_openrouter_key(key.trim())?;
            }
            let discovery = if adapter.model_list_path.is_some() {
                Some(native_provider::discover_connected_models_with_key(&adapter, Some(key.trim()))?)
            } else {
                None
            };
            if cancel.load(Ordering::Relaxed) {
                return Err("Conexión cancelada".into());
            }
            save(
                &root,
                &Credential {
                    version: 1,
                    provider_id: id.clone(),
                    access_token: key.trim().into(),
                    service,
                    ..Default::default()
                },
            )?;
            Ok(ConnectionEvent::Connected {
                provider_id: id,
                discovery,
                welcome: false,
            })
        })
    }
    pub(crate) fn refresh(root: PathBuf) -> Self {
        Self::spawn(move |_, cancel| {
            refresh_codex(&root, &cancel)?;
            Ok(ConnectionEvent::Refreshed)
        })
    }
    pub(crate) fn disconnect(root: PathBuf, id: String) -> Self {
        Self::spawn(move |_, cancel| {
            let _guard = lock(&root, &id, &cancel)?;
            let mut record = load(&root, &id)?.unwrap_or_else(|| Credential {
                version: 1,
                provider_id: id.clone(),
                ..Default::default()
            });
            let warning = if id == "codex" && record.refresh_token.is_some() {
                revoke_codex(&record).err().map(|_| "Se desconectó este equipo, pero no se pudo confirmar la revocación. Puedes retirar el acceso en ChatGPT.".to_owned())
            } else {
                None
            };
            record.access_token.clear();
            record.refresh_token = None;
            record.id_token = None;
            record.expires_at = None;
            record.disabled = true;
            save(&root, &record)?;
            Ok(ConnectionEvent::Disconnected { warning })
        })
    }
}

fn host_id(root: &Path) -> Result<String, String> {
    let dir = credential_dir(root)?;
    let path = dir.join("host-id");
    if let Ok(value) = fs::read_to_string(&path) {
        let value = value.trim();
        if uuid::Uuid::parse_str(value.trim_start_matches("urn:uuid:")).is_ok()
            && value.starts_with("urn:uuid:")
        {
            return Ok(value.into());
        }
        return Err("El identificador de este equipo es inválido".into());
    }
    let value = uuid::Uuid::new_v4().urn().to_string();
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "No se pudo identificar este equipo")?;
    file.write_all(value.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|_| "No se pudo identificar este equipo")?;
    Ok(value)
}

fn oauth(
    root: &Path,
    id: &str,
    sender: &Sender<ConnectionEvent>,
    cancel: &AtomicBool,
) -> Result<ConnectionEvent, String> {
    if !matches!(id, "codex" | "openrouter") {
        return Err("Este proveedor utiliza una clave API".into());
    }
    let _guard = lock(root, id, cancel)?;
    let old = load(root, id)?.filter(|r| !r.disabled);
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|_| "No se pudo abrir el retorno del navegador")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "No se pudo preparar el retorno del navegador")?;
    let port = listener
        .local_addr()
        .map_err(|_| "No se pudo preparar la conexión")?
        .port();
    let state = random();
    let nonce = random();
    let verifier = random();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let path = if id == "codex" {
        "/auth/callback".into()
    } else {
        format!("/oauth/openrouter/{state}")
    };
    let redirect = format!("http://127.0.0.1:{port}{path}");
    let mut url = Url::parse(if id == "codex" {
        "https://auth.openai.com/api/accounts/authorize"
    } else {
        "https://openrouter.ai/auth"
    })
    .expect("fixed authorization URL");
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        if id == "codex" {
            query
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", &redirect)
                .append_pair(
                    "scope",
                    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
                )
                .append_pair("resource", RESOURCE)
                .append_pair("state", &state)
                .append_pair("nonce", &nonce)
                .append_pair("ext_agent_host_id", &host_id(root)?);
            // A client registered under an earlier name is left behind:
            // ChatGPT shows the name only at registration.
            let current = old
                .as_ref()
                .filter(|r| r.agent_name.as_deref() == Some(openide_connections::AGENT_NAME));
            if let Some(client_id) = current.and_then(|r| r.client_id.as_deref()) {
                query.append_pair("client_id", client_id);
                // Account selection stays in the browser; identity is checked after return.
                if let Some(email) = old.as_ref().and_then(|r| r.email.as_deref()) {
                    query.append_pair("login_hint", email);
                }
            } else {
                query
                    .append_pair("client_id", "dynamic_agent_client")
                    .append_pair("agent_name_hint", "openADE");
            }
        } else {
            query
                .append_pair("callback_url", &redirect)
                .append_pair("key_label", "openADE");
        }
    }
    sender
        .send(ConnectionEvent::Browser(url.into()))
        .map_err(|_| "Conexión cancelada")?;
    let callback = wait_callback(
        listener,
        &redirect,
        &path,
        (id == "codex").then_some(state.as_str()),
        cancel,
    )?;
    if callback.contains_key("error") {
        return Err("No se autorizó la conexión. Puedes intentarlo de nuevo.".into());
    }
    let code = callback
        .get("code")
        .filter(|c| !c.is_empty() && c.len() <= 4096)
        .ok_or("El navegador no devolvió un código válido")?;
    let _ = sender.send(ConnectionEvent::Checking);
    let http = client()?;
    let mut record = if id == "openrouter" {
        let response = http
            .post("https://openrouter.ai/api/v1/auth/keys")
            .json(&json!({"code":code,"code_verifier":verifier,"code_challenge_method":"S256"}))
            .send()
            .map_err(request_error)?;
        let token = json_response(response)?;
        let key = token
            .get("key")
            .and_then(Value::as_str)
            .filter(|s| valid_secret(s) && s.len() <= 4096)
            .ok_or("OpenRouter no devolvió una credencial válida")?;
        validate_openrouter_key(key)?;
        Credential {
            version: 1,
            provider_id: id.into(),
            access_token: key.into(),
            oauth: true,
            ..Default::default()
        }
    } else {
        // Only a client registered under the current name is expected back.
        let prior_client = old
            .as_ref()
            .filter(|r| r.agent_name.as_deref() == Some(openide_connections::AGENT_NAME))
            .and_then(|r| r.client_id.as_deref());
        let issued = callback
            .get("client_id")
            .map(String::as_str)
            .or(prior_client)
            .filter(|id| {
                id.starts_with("oaiapp_") && id.len() <= 256 && !id.chars().any(char::is_whitespace)
            })
            .ok_or("ChatGPT no devolvió un registro válido para openADE")?;
        if prior_client.is_some_and(|client| client != issued) {
            return Err("La cuenta devolvió un registro diferente. Vuelve a conectar.".into());
        }
        let response = http
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", issued),
                ("code", code),
                ("code_verifier", &verifier),
                ("redirect_uri", &redirect),
                ("resource", RESOURCE),
            ])
            .send()
            .map_err(request_error)?;
        let token = json_response(response)?;
        let id_token = token
            .get("id_token")
            .and_then(Value::as_str)
            .filter(|s| valid_secret(s))
            .ok_or("ChatGPT no devolvió una identidad válida")?;
        let identity = verify_identity(&http, id_token, issued, Some(&nonce))?;
        if old
            .as_ref()
            .and_then(|r| r.subject.as_deref())
            .is_some_and(|sub| sub != identity.sub)
        {
            return Err("La cuenta elegida no coincide con la conexión guardada. Desconecta la cuenta anterior para cambiarla.".into());
        }
        let mut r = Credential {
            version: 1,
            provider_id: id.into(),
            client_id: Some(issued.into()),
            agent_name: Some(openide_connections::AGENT_NAME.into()),
            subject: Some(identity.sub),
            email: identity.email,
            id_token: Some(id_token.into()),
            oauth: true,
            welcome_seen: old.as_ref().is_some_and(|r| r.welcome_seen),
            ..Default::default()
        };
        update_tokens(&mut r, &token)?;
        r
    };
    if cancel.load(Ordering::Relaxed) {
        return Err("Conexión cancelada".into());
    }
    record.disabled = false;
    let welcome = id == "codex" && !record.welcome_seen;
    // Registration and rotating tokens must survive a later catalog outage.
    save(root, &record)?;
    let adapter = native_provider::adapter_at(root, id)?.ok_or("Proveedor no disponible")?;
    let discovery = native_provider::discover_connected_models_with_key(
        &adapter,
        Some(&record.access_token),
    )
    .map_err(|error| format!("La cuenta se autorizó y quedó guardada, pero no se pudieron consultar los modelos. {error}"))?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Conexión cancelada".into());
    }
    if welcome {
        record.welcome_seen = true;
        save(root, &record)?;
    }
    Ok(ConnectionEvent::Connected { provider_id: id.into(), discovery: Some(discovery), welcome })
}

fn respond(stream: &mut TcpStream, status: &str, text: &str) {
    let body = format!("<!doctype html><html lang=es><meta charset=utf-8><meta name=viewport content='width=device-width'><title>openADE</title><body style='background:#1e1e1e;color:#eee;font:18px system-ui;padding:48px'><h1>openADE</h1><p>{text}</p></body></html>");
    let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nConnection: close\r\n\r\n{body}", body.len());
}
fn wait_callback(
    listener: TcpListener,
    base: &str,
    expected_path: &str,
    expected_state: Option<&str>,
    cancel: &AtomicBool,
) -> Result<BTreeMap<String, String>, String> {
    let deadline = Instant::now() + Duration::from_secs(600);
    while Instant::now() < deadline && !cancel.load(Ordering::Relaxed) {
        let (mut stream, peer) = match listener.accept() {
            Ok(pair) => pair,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(80));
                continue;
            }
            Err(_) => return Err("Se interrumpió el retorno del navegador".into()),
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 1024];
        while bytes.len() <= 16 * 1024 && !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
            }
        }
        if bytes.len() > 16 * 1024 {
            respond(&mut stream, "400 Bad Request", "Solicitud inválida.");
            continue;
        }
        let Ok(request) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let mut parts = request.lines().next().unwrap_or("").split_whitespace();
        if parts.next() != Some("GET") {
            respond(&mut stream, "405 Method Not Allowed", "Solicitud inválida.");
            continue;
        }
        let target = parts.next().unwrap_or("");
        let Ok(url) = Url::parse(base).and_then(|base| base.join(target)) else {
            continue;
        };
        let expected_host = Url::parse(base).expect("callback URL");
        if url.origin() != expected_host.origin() || url.path() != expected_path {
            respond(
                &mut stream,
                "404 Not Found",
                "Esta dirección no pertenece a la conexión.",
            );
            continue;
        }
        let mut values = BTreeMap::new();
        let mut duplicate = false;
        for (key, value) in url.query_pairs().into_owned() {
            if values.insert(key, value).is_some() {
                duplicate = true;
            }
        }
        if duplicate {
            respond(&mut stream, "400 Bad Request", "Autorización inválida.");
            continue;
        }
        if expected_state.is_some_and(|s| values.get("state").map(String::as_str) != Some(s)) {
            respond(
                &mut stream,
                "400 Bad Request",
                "La autorización no corresponde a esta conexión.",
            );
            continue;
        }
        if !values.contains_key("code") && !values.contains_key("error") {
            respond(&mut stream, "400 Bad Request", "Falta la autorización.");
            continue;
        }
        respond(
            &mut stream,
            "200 OK",
            "Autorización recibida. Vuelve a openADE para continuar; puedes cerrar esta pestaña.",
        );
        return Ok(values);
    }
    Err(if cancel.load(Ordering::Relaxed) {
        "Conexión cancelada"
    } else {
        "La conexión caducó. Vuelve a intentarlo."
    }
    .into())
}

fn validate_openrouter_key(key: &str) -> Result<(), String> {
    json_response(
        client()?
            .get("https://openrouter.ai/api/v1/key")
            .bearer_auth(key)
            .send()
            .map_err(request_error)?,
    )?;
    Ok(())
}

#[cfg(test)]
mod api_provider_tests {
    use super::*;

    #[test]
    fn zai_key_save_is_local_and_does_not_claim_authenticated_models() {
        let root = tempfile::tempdir().unwrap();
        let worker = ConnectionWorker::api_key(
            root.path().into(), "zai".into(), "fixture-zai-key".into(), "api".into(),
        );
        assert!(matches!(worker.receiver.recv_timeout(Duration::from_secs(5)).unwrap(), ConnectionEvent::Checking));
        match worker.receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
            ConnectionEvent::Connected { provider_id, discovery, welcome } => {
                assert_eq!(provider_id, "zai");
                assert!(discovery.is_none(), "saving a key must not manufacture account models");
                assert!(!welcome);
            }
            ConnectionEvent::Failed(error) => panic!("local save failed: {error}"),
            _ => panic!("unexpected connection event"),
        }
        assert_eq!(read_key(root.path(), "ZAI_API_KEY").as_deref(), Some("fixture-zai-key"));
        let record = load(root.path(), "zai").unwrap().unwrap();
        assert!(!record.oauth && record.email.is_none());
        let catalog = crate::model_catalog::reviewed_seed_snapshot();
        for model in ["glm-5.3", "glm-5.2"] {
            let settings = native_provider::NativeProviderSettings {
                provider_id: "zai".into(), model_id: model.into(), ..Default::default()
            };
            let routes = crate::composer_selection::model_route_options_with_discovery_at(
                Some(&catalog), &settings, &[], root.path(),
            ).unwrap();
            let route = routes.iter().find(|route| route.provider_id == "zai" && route.model_id == model).unwrap();
            assert!(route.selectable());
            assert_eq!(route.provider_listed, None);
            assert_eq!(route.credential, crate::composer_selection::CredentialAvailability::EnvPresentUnverified);
            let connection = settings.resolve_at(root.path()).unwrap();
            assert_eq!(connection.endpoint, "https://api.z.ai/api/paas/v4");
            crate::app::native_model_backend(connection, "zai", None, None, root.path(), Some(&catalog), &[])
                .unwrap_or_else(|error| panic!("reviewed GLM route cannot execute: {error}"));
        }
        let worker = ConnectionWorker::disconnect(root.path().into(), "zai".into());
        assert!(matches!(worker.receiver.recv_timeout(Duration::from_secs(5)).unwrap(), ConnectionEvent::Disconnected { warning: None }));
        assert!(read_key(root.path(), "ZAI_API_KEY").is_none());
        assert!(!summaries(root.path())["zai"].configured);
    }

    #[test]
    fn minimax_and_zai_credentials_remain_separate_private_and_revocable() {
        let root = tempfile::tempdir().unwrap();
        // A key kept for a provider of the native agent is read back through
        // its variable, and never counts as an account.
        for (provider, env) in [
            ("openai", "OPENAI_API_KEY"),
            ("anthropic", "ANTHROPIC_API_KEY"),
            ("xai", "XAI_API_KEY"),
            ("deepseek", "DEEPSEEK_API_KEY"),
            ("mistral", "MISTRAL_API_KEY"),
            ("groq", "GROQ_API_KEY"),
        ] {
            assert!(openide_connections::key_connectable(provider) && !supported(provider), "{provider}");
            assert_eq!(key_provider(env), Some(provider));
            assert_eq!(read_key(root.path(), env), None);
            save(root.path(), &Credential {
                version: 1, provider_id: provider.into(), access_token: format!("key-{provider}"),
                service: "api".into(), ..Default::default()
            }).unwrap();
            assert_eq!(read_key(root.path(), env).as_deref(), Some(format!("key-{provider}").as_str()));
            assert!(!summaries(root.path()).contains_key(provider));
        }
        for (provider, env) in [("minimax", "MINIMAX_API_KEY"), ("zai", "ZAI_API_KEY")] {
            let secret = format!("fixture-{provider}-secret");
            save(root.path(), &Credential {
                version: 1, provider_id: provider.into(), access_token: secret.clone(),
                service: "api".into(), ..Default::default()
            }).unwrap();
            assert_eq!(key_provider(env), Some(provider));
            assert_eq!(read_key(root.path(), env), Some(secret));
            let state = summaries(root.path());
            assert!(state[provider].configured && !state[provider].oauth && !state[provider].from_environment);
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                let directory = root.path().join("provider-credentials");
                assert_eq!(fs::metadata(&directory).unwrap().permissions().mode() & 0o777, 0o700);
                assert_eq!(fs::metadata(directory.join(format!("{provider}.json"))).unwrap().permissions().mode() & 0o777, 0o600);
            }
        }
        let worker = ConnectionWorker::disconnect(root.path().into(), "minimax".into());
        assert!(matches!(worker.receiver.recv_timeout(Duration::from_secs(5)).unwrap(), ConnectionEvent::Disconnected { warning: None }));
        assert!(read_key(root.path(), "MINIMAX_API_KEY").is_none());
        assert_eq!(read_key(root.path(), "ZAI_API_KEY").as_deref(), Some("fixture-zai-secret"));
    }
}
