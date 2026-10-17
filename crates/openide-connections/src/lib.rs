//! Shared private provider storage and OAuth renewal for desktop and the remote host.
//! Tokens never implement Debug, errors omit provider response bodies, and both
//! processes use the same file lock before rotating a refresh token.
#[cfg(test)]
mod tests;
use fs2::FileExt;
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};
use reqwest::{
    blocking::{Client, Response},
    redirect::Policy,
    Url,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub const PROVIDERS: &[(&str, &str, &str)] = &[
    ("codex", "Codex", "Usa tu cuenta y plan de ChatGPT"),
    (
        "openrouter",
        "OpenRouter",
        "Una cuenta para modelos de distintos proveedores",
    ),
    ("opencode", "OpenCode", "Modelos de OpenCode Zen y Go"),
    (
        "nvidia",
        "NVIDIA NIM",
        "Modelos alojados en NVIDIA API Catalog",
    ),
    ("minimax", "MiniMax", "Modelos MiniMax con tu clave de API"),
    (
        "zai",
        "GLM · Z.ai",
        "Modelos GLM con la API general de Z.ai",
    ),
];

/// Providers of the native agent that Settings connects with an API key
/// kept here, read back through their key variable: the agent's routes
/// still see them as providers with a key, not as accounts.
pub const KEY_PROVIDERS: &[(&str, &str, &str)] = &[
    ("openai", "OpenAI", "Modelos GPT con tu clave de API"),
    ("anthropic", "Anthropic", "Modelos Claude con tu clave de API"),
    ("xai", "xAI", "Modelos Grok con tu clave de API"),
    ("deepseek", "DeepSeek", "Modelos DeepSeek con tu clave de API"),
    ("mistral", "Mistral", "Modelos Mistral con tu clave de API"),
    ("groq", "Groq", "Modelos abiertos servidos por Groq con tu clave de API"),
];

pub fn key_connectable(id: &str) -> bool {
    KEY_PROVIDERS.iter().any(|row| row.0 == id)
}

/// Whether Settings offers a connection for `id`: an account, or a key.
pub fn connectable(id: &str) -> bool {
    supported(id) || key_connectable(id)
}

pub const AUTH_ORIGIN: &str = "https://auth.openai.com";
pub const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
pub const RESOURCE: &str = "https://api.openai.com/v1";
const MAX_RECORD: u64 = 96 * 1024;
const MAX_HTTP: u64 = 256 * 1024;

pub fn supported(id: &str) -> bool {
    PROVIDERS.iter().any(|row| row.0 == id)
}

pub fn key_provider(env: &str) -> Option<&'static str> {
    match env {
        "OPENIDE_CODEX_ACCESS_TOKEN" => Some("codex"),
        "OPENROUTER_API_KEY" => Some("openrouter"),
        "OPENCODE_API_KEY" => Some("opencode"),
        "NVIDIA_API_KEY" => Some("nvidia"),
        "MINIMAX_API_KEY" => Some("minimax"),
        "ZAI_API_KEY" => Some("zai"),
        "OPENAI_API_KEY" => Some("openai"),
        "ANTHROPIC_API_KEY" => Some("anthropic"),
        "XAI_API_KEY" => Some("xai"),
        "DEEPSEEK_API_KEY" => Some("deepseek"),
        "MISTRAL_API_KEY" => Some("mistral"),
        "GROQ_API_KEY" => Some("groq"),
        _ => None,
    }
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn random() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
pub fn valid_secret(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32 * 1024
        && value.is_ascii()
        && !value.chars().any(char::is_whitespace)
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Credential {
    pub version: u32,
    pub provider_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub client_id: Option<String>,
    pub subject: Option<String>,
    pub email: Option<String>,
    pub expires_at: Option<u64>,
    pub scopes: Vec<String>,
    pub service: String,
    pub oauth: bool,
    pub disabled: bool,
    pub welcome_seen: bool,
}

pub fn credential_dir(root: &Path) -> Result<PathBuf, String> {
    let dir = root.join("provider-credentials");
    if fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("La carpeta de credenciales no puede ser un enlace".into());
    }
    fs::create_dir_all(&dir).map_err(|_| "No se pudo crear la carpeta de credenciales")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| "No se pudo proteger la carpeta de credenciales")?;
    }
    Ok(dir)
}

pub fn load(root: &Path, id: &str) -> Result<Option<Credential>, String> {
    if !connectable(id) {
        return Err("Proveedor no disponible".into());
    }
    let dir = root.join("provider-credentials");
    let directory = match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => meta,
        Ok(_) => return Err("Carpeta de credenciales inválida".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("No se pudo leer la conexión guardada".into()),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if directory.permissions().mode() & 0o077 != 0 {
            return Err("La carpeta de credenciales no tiene permisos privados".into());
        }
    }
    #[cfg(not(unix))]
    let _ = directory;
    let path = dir.join(format!("{id}.json"));
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("No se pudo leer la conexión guardada".into()),
    };
    let meta = match file.metadata() {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("No se pudo leer la conexión guardada".into()),
    };
    if !meta.is_file() || meta.len() > MAX_RECORD {
        return Err("Conexión guardada inválida".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("Las credenciales no tienen permisos privados".into());
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "No se pudo leer la conexión guardada")?;
    if bytes.len() as u64 > MAX_RECORD {
        return Err("Conexión guardada inválida".into());
    }
    let record: Credential =
        serde_json::from_slice(&bytes).map_err(|_| "Conexión guardada inválida")?;
    if record.version != 1 || record.provider_id != id {
        return Err("Conexión guardada inválida".into());
    }
    Ok(Some(record))
}

pub fn save(root: &Path, record: &Credential) -> Result<(), String> {
    if !connectable(&record.provider_id) || record.version != 1 {
        return Err("Conexión guardada inválida".into());
    }
    let dir = credential_dir(root)?;
    let payload = serde_json::to_vec(record).map_err(|_| "No se pudo preparar la conexión")?;
    if payload.len() as u64 > MAX_RECORD {
        return Err("La conexión excede el tamaño permitido".into());
    }
    let temp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| -> Result<(), String> {
        let mut file = options
            .open(&temp)
            .map_err(|_| "No se pudo proteger la conexión")?;
        file.write_all(&payload)
            .and_then(|()| file.sync_all())
            .map_err(|_| "No se pudo guardar la conexión")?;
        drop(file);
        replace_settings_atomically(&temp, &dir.join(format!("{}.json", record.provider_id)))
            .map_err(|_| "No se pudo guardar la conexión")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

pub struct RecordLock(File);
impl Drop for RecordLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
pub fn lock(root: &Path, id: &str, cancel: &AtomicBool) -> Result<RecordLock, String> {
    if !connectable(id) {
        return Err("Proveedor no disponible".into());
    }
    let dir = credential_dir(root)?;
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts
        .open(dir.join(format!("{id}.lock")))
        .map_err(|_| "No se pudo abrir la conexión")?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Conexión cancelada".into());
        }
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(RecordLock(file)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(80))
            }
            Err(_) => return Err("Otra instancia está actualizando esta conexión".into()),
        }
    }
}

/// Local, bounded reads only. UI threads never refresh credentials or send HTTP.
pub fn read_key(root: &Path, env: &str) -> Option<String> {
    if let Some(id) = key_provider(env) {
        match load(root, id) {
            Ok(Some(record)) => {
                if record.disabled
                    || record.expires_at.is_some_and(|expiry| expiry <= now() + 5)
                    || (id == "codex"
                        && !record
                            .scopes
                            .iter()
                            .any(|s| s == "chatgpt.tokens.use.direct"))
                {
                    return None;
                }
                return valid_secret(&record.access_token).then_some(record.access_token);
            }
            Err(_) => return None,
            Ok(None) => {
                if id == "codex" {
                    return None;
                }
            }
        }
    }
    std::env::var(env)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| valid_secret(s))
}

pub fn service(root: &Path, id: &str) -> String {
    load(root, id)
        .ok()
        .flatten()
        .map_or_else(|| "zen".into(), |r| r.service)
}

pub fn client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(Policy::none())
        .user_agent("OpenIDE/0.1")
        .build()
        .map_err(|_| "No se pudo preparar la conexión".into())
}
pub fn json_response(mut response: Response) -> Result<Value, String> {
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            401 | 403 => {
                "No se pudo autorizar la conexión. Vuelve a iniciar sesión o revisa la clave."
            }
            429 => "El proveedor recibió demasiadas solicitudes. Inténtalo más tarde.",
            _ => "El proveedor no pudo completar la conexión. Inténtalo de nuevo.",
        }
        .into());
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(MAX_HTTP + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "No se pudo leer la respuesta del proveedor")?;
    if bytes.len() as u64 > MAX_HTTP {
        return Err("Respuesta del proveedor demasiado grande".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Respuesta del proveedor inválida".into())
}
pub fn request_error(_: reqwest::Error) -> String {
    "No se pudo contactar al proveedor. Revisa la conexión a Internet.".into()
}

#[derive(Deserialize)]
pub struct Identity {
    pub sub: String,
    pub email: Option<String>,
    pub nonce: Option<String>,
    pub iat: u64,
}
pub fn verify_identity(
    http: &Client,
    token: &str,
    audience: &str,
    nonce: Option<&str>,
) -> Result<Identity, String> {
    let header = decode_header(token).map_err(|_| "Identidad de ChatGPT inválida")?;
    if header.alg != Algorithm::RS256 {
        return Err("Firma de ChatGPT no admitida".into());
    }
    let kid = header
        .kid
        .ok_or("Identidad de ChatGPT sin clave de firma")?;
    let document = json_response(
        http.get("https://auth.openai.com/.well-known/jwks.json")
            .send()
            .map_err(request_error)?,
    )?;
    let keys: JwkSet =
        serde_json::from_value(document).map_err(|_| "Claves de firma de ChatGPT inválidas")?;
    let key = keys
        .find(&kid)
        .ok_or("No se pudo verificar la firma de ChatGPT")?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[audience]);
    validation.set_issuer(&[AUTH_ORIGIN]);
    validation.set_required_spec_claims(&["sub", "exp", "iat", "aud", "iss"]);
    validation.leeway = 5;
    let claims = decode::<Identity>(
        token,
        &DecodingKey::from_jwk(key).map_err(|_| "Clave de firma inválida")?,
        &validation,
    )
    .map_err(|_| "No se pudo verificar la identidad de ChatGPT")?
    .claims;
    if claims.sub.is_empty()
        || claims.sub.len() > 256
        || claims.iat > now() + 5
        || nonce.is_some_and(|n| claims.nonce.as_deref() != Some(n))
        || claims
            .email
            .as_ref()
            .is_some_and(|email| email.len() > 320 || email.chars().any(char::is_control))
    {
        return Err("La identidad no corresponde a esta autorización".into());
    }
    Ok(claims)
}
pub fn update_tokens(record: &mut Credential, token: &Value) -> Result<(), String> {
    if token
        .get("token_type")
        .and_then(Value::as_str)
        .is_none_or(|s| !s.eq_ignore_ascii_case("bearer"))
    {
        return Err("Tipo de credencial no admitido".into());
    }
    let access = token
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| valid_secret(s))
        .ok_or("El proveedor no devolvió una credencial válida")?;
    let expiry = token
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|s| (30..=86400).contains(s))
        .ok_or("La credencial no contiene una caducidad válida")?;
    if let Some(scope) = token.get("scope").and_then(Value::as_str) {
        record.scopes = scope.split_whitespace().map(str::to_owned).collect();
    }
    if !record
        .scopes
        .iter()
        .any(|s| s == "chatgpt.tokens.use.direct")
    {
        return Err(
            "No se autorizó el uso del plan de ChatGPT. Vuelve a conectar y habilita ese permiso."
                .into(),
        );
    }
    record.access_token = access.into();
    record.expires_at = Some(now().saturating_add(expiry));
    if let Some(refresh) = token.get("refresh_token").and_then(Value::as_str) {
        if !valid_secret(refresh) {
            return Err("Credencial de renovación inválida".into());
        }
        record.refresh_token = Some(refresh.into());
    }
    if record.refresh_token.is_none() {
        return Err("ChatGPT no autorizó la renovación de esta conexión".into());
    }
    Ok(())
}

pub fn refresh_due(root: &Path) -> bool {
    load(root, "codex").ok().flatten().is_some_and(|r| {
        !r.disabled && r.refresh_token.is_some() && r.expires_at.is_some_and(|t| t <= now() + 300)
    })
}

pub fn codex_token_source(
    root: PathBuf,
) -> Result<impl Fn() -> Result<String, String> + Send + Sync, String> {
    let original = load(&root, "codex")?.ok_or("Conecta tu cuenta de ChatGPT para continuar")?;
    if original.disabled
        || !original.oauth
        || original.subject.is_none()
        || original.client_id.is_none()
    {
        return Err("Conecta tu cuenta de ChatGPT para continuar".into());
    }
    let subject = original.subject;
    let client_id = original.client_id;
    Ok(move || {
        let current = load(&root, "codex")?.ok_or("Vuelve a conectar tu cuenta de ChatGPT")?;
        if current.disabled || current.subject != subject || current.client_id != client_id {
            return Err(
                "La conexión de ChatGPT cambió. Vuelve a seleccionar la cuenta y el modelo.".into(),
            );
        }
        if refresh_due(&root) {
            refresh_codex(&root, &AtomicBool::new(false))?;
        }
        let current = load(&root, "codex")?.ok_or("Vuelve a conectar tu cuenta de ChatGPT")?;
        if current.disabled || current.subject != subject || current.client_id != client_id {
            return Err("La conexión de ChatGPT cambió durante el turno".into());
        }
        read_key(&root, "OPENIDE_CODEX_ACCESS_TOKEN")
            .ok_or_else(|| "La sesión de ChatGPT venció. Vuelve a conectarla.".into())
    })
}

pub fn refresh_codex(root: &Path, cancel: &AtomicBool) -> Result<(), String> {
    let _guard = lock(root, "codex", cancel)?;
    let Some(mut record) = load(root, "codex")? else {
        return Ok(());
    };
    if record.disabled || record.expires_at.is_none_or(|t| t > now() + 300) {
        return Ok(());
    }
    let refresh = record
        .refresh_token
        .as_deref()
        .ok_or("Vuelve a iniciar sesión con ChatGPT")?;
    let client_id = record
        .client_id
        .as_deref()
        .ok_or("Vuelve a iniciar sesión con ChatGPT")?;
    let token = json_response(
        client()?
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("refresh_token", refresh),
                ("resource", RESOURCE),
            ])
            .send()
            .map_err(request_error)?,
    )?;
    if let Some(id_token) = token.get("id_token").and_then(Value::as_str) {
        let identity = verify_identity(&client()?, id_token, client_id, None)?;
        if record.subject.as_deref() != Some(&identity.sub) {
            return Err("La identidad de la conexión cambió. Vuelve a iniciar sesión.".into());
        }
        record.id_token = Some(id_token.into());
    }
    update_tokens(&mut record, &token)?;
    // Rotating refresh tokens must be committed even if the UI closes now.
    save(root, &record)
}
pub fn revoke_codex(record: &Credential) -> Result<(), String> {
    let http = client()?;
    let discovery = json_response(
        http.get("https://auth.openai.com/.well-known/openid-configuration")
            .send()
            .map_err(request_error)?,
    )?;
    let endpoint = discovery
        .get("revocation_endpoint")
        .and_then(Value::as_str)
        .ok_or("No se pudo retirar el acceso")?;
    let url = Url::parse(endpoint).map_err(|_| "No se pudo retirar el acceso")?;
    if url.scheme() != "https"
        || url.host_str() != Some("auth.openai.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Dirección de revocación inválida".into());
    }
    let response = http
        .post(url)
        .form(&[
            (
                "token",
                record
                    .refresh_token
                    .as_deref()
                    .ok_or("Conexión sin sesión renovable")?,
            ),
            ("token_type_hint", "refresh_token"),
            (
                "client_id",
                record.client_id.as_deref().ok_or("Conexión sin registro")?,
            ),
        ])
        .send()
        .map_err(request_error)?;
    if response.status().as_u16() != 200 {
        return Err("No se pudo confirmar la revocación".into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_settings_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)?;
    if let Some(parent) = destination.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}
#[cfg(windows)]
fn replace_settings_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
