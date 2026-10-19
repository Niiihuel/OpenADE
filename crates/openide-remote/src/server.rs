//! The HTTPS server the window runs for the phone.

use crate::auth::{Device, DeviceStore};
use crate::bridge::{Bridge, Command, Decision, Delivery, Notice};
use crate::error::{ApiError, ApiResult};
use crate::pairing::{PairRequest, PairingError, PairingInvitation, PairingStore};
use crate::push::{PushEvent, PushEventKind, PushRegistrationRequest, PushRegistry, PushService};
use crate::{git_workspace, storage, tasks, tls, workspace};
use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, FromRequest, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use openide_core::SessionStore;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
pub use crate::upnp::UpnpState;
use uuid::Uuid;

/// What a phone may upload at once.
const MAX_UPLOAD: usize = 25 * 1024 * 1024;
/// Everything uploaded and not yet cleaned up, together.
const UPLOADS_QUOTA: u64 = 200 * 1024 * 1024;
/// Pairing attempts one address may make in a minute.
const PAIRING_ATTEMPTS: u32 = 10;
/// Uploads older than this are deleted when the server starts.
const UPLOAD_LIFETIME: Duration = Duration::from_secs(7 * 24 * 3600);

pub struct Options {
    /// The server's own folder: identity, devices, pairing, uploads, push.
    pub root: PathBuf,
    /// Where the window keeps its session journals.
    pub sessions: PathBuf,
    pub port: u16,
    /// What the phone calls this PC.
    pub name: String,
    /// A name or address the person gave for a port opened on their router.
    pub public_address: Option<String>,
    /// Ask the router to open the port (UPnP).
    pub upnp: bool,
}

struct Shared {
    bridge: Arc<Bridge>,
    root: PathBuf,
    sessions: PathBuf,
    store: SessionStore,
    devices: DeviceStore,
    pairing: PairingStore,
    push: Option<PushService>,
    fingerprint: String,
    server_id: String,
    name: String,
    port: u16,
    public_address: Option<String>,
    upnp: Arc<Mutex<UpnpState>>,
    /// Edits of a project's files and index, one at a time.
    writes: Mutex<()>,
    /// Uploads written at once.
    uploads: tokio::sync::Semaphore,
    /// Pairing attempts per address in the current minute.
    pairing_attempts: Mutex<HashMap<IpAddr, (std::time::Instant, u32)>>,
}

impl Shared {
    fn routes(&self) -> Vec<String> {
        let mut routes = crate::network::routes(self.port, self.public_address.as_deref());
        let upnp = self.upnp.lock().unwrap_or_else(|error| error.into_inner()).route(self.port);
        routes.extend(upnp.filter(|route| !routes.contains(route)));
        routes
    }
}

type AppState = Arc<Shared>;

pub struct Server {
    shared: AppState,
    stop: tokio::sync::watch::Sender<bool>,
    /// Answers once the port is no longer listened on.
    released: std::sync::mpsc::Receiver<()>,
}

impl Server {
    /// Listens on `options.port` on every address, IPv4 and IPv6.
    pub fn start(options: Options, bridge: Arc<Bridge>) -> Result<Self> {
        storage::prepare_directory(&options.root)?;
        let identity = tls::Identity::load_or_create(&options.root)?;
        let tls = tokio_rustls::TlsAcceptor::from(identity.server_config()?);
        let server_id = server_id(&options.root)?;
        clean_uploads(&options.root);
        let listener = bind(options.port)?;
        let port = listener.local_addr()?.port();
        let shared = Arc::new(Shared {
            store: SessionStore::open(&options.sessions).context("cannot open the session journals")?,
            devices: DeviceStore::new(&options.root),
            pairing: PairingStore::new(&options.root),
            push: PushService::open(&options.root).ok().flatten(),
            fingerprint: identity.fingerprint(),
            server_id,
            name: options.name,
            port,
            public_address: options.public_address,
            upnp: Arc::new(Mutex::new(UpnpState::Off)),
            root: options.root,
            sessions: options.sessions,
            bridge,
            writes: Mutex::new(()),
            uploads: tokio::sync::Semaphore::new(2),
            pairing_attempts: Mutex::default(),
        });
        // Codes from before and tokens no phone acknowledged are forgotten.
        let _ = shared.pairing.purge_expired();
        let _ = shared.pairing.cancel_unused();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let (release, released) = std::sync::mpsc::channel();
        let app = router(shared.clone());
        let notices = shared.clone();
        let upnp = options.upnp.then(|| (shared.upnp.clone(), stopped.clone()));
        std::thread::Builder::new().name("openade-remote".into()).spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                listener.set_nonblocking(true).ok();
                let Ok(listener) = TcpListener::from_std(listener) else { return };
                tokio::spawn(send_notices(notices));
                let router = upnp.map(|(state, stop)| tokio::spawn(crate::upnp::keep_open(port, state, stop)));
                serve(listener, tls, app, stopped).await;
                let _ = release.send(());
                // The router's port is closed after the window moved on.
                if let Some(router) = router {
                    let _ = router.await;
                }
            });
        })?;
        Ok(Self { shared, stop, released })
    }

    pub fn upnp_state(&self) -> UpnpState {
        self.shared.upnp.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }

    pub fn port(&self) -> u16 {
        self.shared.port
    }

    pub fn fingerprint(&self) -> &str {
        &self.shared.fingerprint
    }

    pub fn routes(&self) -> Vec<String> {
        self.shared.routes()
    }

    pub fn push_enabled(&self) -> bool {
        self.shared.push.is_some()
    }

    /// A one-use code for a phone to pair with, good for five minutes. A
    /// code shown before stops working.
    pub fn invite(&self, label: &str) -> Result<PairingInvitation> {
        self.shared.pairing.cancel_unused()?;
        self.shared.pairing.start(&self.routes(), &self.shared.fingerprint, &self.shared.server_id, label)
    }

    /// The code on screen is closed: it stops working.
    pub fn withdraw_invitations(&self) -> Result<()> {
        self.shared.pairing.cancel_unused()
    }

    pub fn devices(&self) -> Result<Vec<Device>> {
        Ok(self.shared.devices.list()?.into_iter().filter(|device| !device.revoked).collect())
    }

    /// The device loses access at once, open streams included.
    pub fn revoke(&self, id: &str) -> Result<()> {
        self.shared.devices.revoke(id)?;
        self.shared.bridge.revoked();
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.shared.pairing.cancel_unused();
        let _ = self.stop.send(true);
        // A server started next on the same port needs it free.
        let _ = self.released.recv_timeout(Duration::from_secs(3));
    }
}

fn bind(port: u16) -> Result<std::net::TcpListener> {
    // `[::]` takes IPv4 too where the system allows it; elsewhere IPv4 alone.
    std::net::TcpListener::bind(("::", port))
        .or_else(|_| std::net::TcpListener::bind(("0.0.0.0", port)))
        .with_context(|| format!("el puerto {port} está ocupado"))
}

fn server_id(root: &std::path::Path) -> Result<String> {
    let path = root.join("server-id");
    if let Ok(bytes) = storage::read_private(&path, 64) {
        let id = String::from_utf8_lossy(&bytes).trim().to_owned();
        if Uuid::parse_str(&id).is_ok() {
            return Ok(id);
        }
    }
    let id = Uuid::new_v4().to_string();
    storage::write_private(&path, id.as_bytes())?;
    Ok(id)
}

fn clean_uploads(root: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(root.join("uploads")) else { return };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified.elapsed().unwrap_or_default() > UPLOAD_LIFETIME);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Connections served at once, and from one address.
const MAX_CONNECTIONS: usize = 64;
const MAX_PER_ADDRESS: usize = 8;

/// The address a request came from, for limits that are per address.
#[derive(Clone, Copy)]
struct Peer(IpAddr);

/// One connection's share of its address's allowance.
struct AddressSlot {
    addresses: Arc<Mutex<HashMap<IpAddr, usize>>>,
    address: IpAddr,
}

impl AddressSlot {
    fn take(addresses: &Arc<Mutex<HashMap<IpAddr, usize>>>, address: IpAddr) -> Option<Self> {
        let mut open = addresses.lock().unwrap_or_else(|error| error.into_inner());
        let count = open.entry(address).or_default();
        if *count >= MAX_PER_ADDRESS {
            return None;
        }
        *count += 1;
        Some(Self { addresses: addresses.clone(), address })
    }
}

impl Drop for AddressSlot {
    fn drop(&mut self) {
        let mut open = self.addresses.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(count) = open.get_mut(&self.address) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.address);
            }
        }
    }
}

async fn serve(
    listener: TcpListener,
    tls: tokio_rustls::TlsAcceptor,
    app: Router,
    mut stopped: tokio::sync::watch::Receiver<bool>,
) {
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    let addresses: Arc<Mutex<HashMap<IpAddr, usize>>> = Arc::default();
    loop {
        let (tcp, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            _ = stopped.changed() => return,
        };
        // Past either limit the connection is dropped before any TLS work.
        let Ok(permit) = slots.clone().try_acquire_owned() else { continue };
        let Some(slot) = AddressSlot::take(&addresses, peer.ip()) else { continue };
        let (tls, app) = (tls.clone(), app.clone());
        tokio::spawn(async move {
            let _held = (permit, slot);
            let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(10), tls.accept(tcp)).await else {
                return;
            };
            let h2 = stream.get_ref().1.alpn_protocol() == Some(b"h2".as_slice());
            let service = hyper_util::service::TowerToHyperService::new(app.layer(Extension(Peer(peer.ip()))));
            let io = hyper_util::rt::TokioIo::new(stream);
            // Without a timer hyper waits on a silent client for ever.
            if h2 {
                let _ = hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .timer(hyper_util::rt::TokioTimer::new())
                    .keep_alive_interval(Some(Duration::from_secs(30)))
                    .keep_alive_timeout(Duration::from_secs(10))
                    .max_concurrent_streams(32)
                    .serve_connection(io, service)
                    .await;
            } else {
                let _ = hyper::server::conn::http1::Builder::new()
                    .timer(hyper_util::rt::TokioTimer::new())
                    .header_read_timeout(Duration::from_secs(10))
                    .serve_connection(io, service)
                    .await;
            }
        });
    }
}

async fn send_notices(shared: AppState) {
    let Some(mut notices) = shared.bridge.take_notices() else { return };
    while let Some((notice, task)) = notices.recv().await {
        let Some(push) = &shared.push else { continue };
        let live = shared.bridge.live();
        // The person is looking at the PC, which shows the same thing.
        if live.desktop_focused {
            continue;
        }
        // The lock screen shows it: no project or task names.
        let title = "openADE".to_owned();
        let kind = match notice {
            Notice::Approval => PushEventKind::ApprovalRequired,
            Notice::Question => PushEventKind::QuestionAsked,
            Notice::Completed => PushEventKind::TurnCompleted,
            Notice::Failed => PushEventKind::TurnFailed,
        };
        let _ = push.send_event(PushEvent { kind, session_id: task, title }).await;
    }
}

fn router(shared: AppState) -> Router {
    Router::new()
        .route("/.well-known/openade", get(identity))
        .route("/v2/pair", post(pair))
        .route("/v2/pair/{id}/ack", post(pair_ack))
        .route("/v2/overview", get(overview))
        .route("/v2/stream", get(stream))
        .route("/v2/tasks", post(create_task))
        .route("/v2/tasks/{id}", axum::routing::patch(patch_task))
        .route("/v2/tasks/{id}/history", get(history))
        .route("/v2/tasks/{id}/messages", post(send_message))
        .route("/v2/tasks/{id}/stop", post(stop))
        .route("/v2/tasks/{id}/read", post(read))
        .route("/v2/tasks/{id}/permissions/{request}", post(permission))
        .route("/v2/tasks/{id}/answers/{request}", post(answer))
        .route("/v2/uploads", post(upload).layer(DefaultBodyLimit::max(MAX_UPLOAD)))
        .route("/v2/notifications/devices", post(push_register).delete(push_remove))
        .route("/v2/projects/{id}/files", get(files))
        .route("/v2/projects/{id}/file", get(file).put(save_file).layer(DefaultBodyLimit::max(7 * 1024 * 1024)))
        .route("/v2/projects/{id}/diff", get(diff))
        .route("/v2/projects/{id}/git/status", get(git_status))
        .route("/v2/projects/{id}/git/diff", get(git_diff))
        .route("/v2/projects/{id}/git/stage", post(git_stage))
        .route("/v2/projects/{id}/git/unstage", post(git_unstage))
        .route("/v2/projects/{id}/git/commit", post(git_commit))
        .fallback(|| async { ApiError::not_found() })
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(shared.clone(), authenticate))
        .with_state(shared)
}

/// What anyone may ask: who this is, so a phone checks it found its PC
/// before it sends a token.
fn public(path: &str) -> bool {
    path == "/.well-known/openade" || path == "/v2/pair"
}

async fn authenticate(State(shared): State<AppState>, mut request: axum::extract::Request, next: Next) -> Response {
    if !public(request.uri().path()) {
        let token = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or_default()
            .to_owned();
        let devices = shared.devices.clone();
        let device = tokio::task::spawn_blocking(move || {
            let device = devices.authenticate_device(&token)?;
            let _ = devices.touch(&device.id);
            Some(device)
        })
        .await
        .ok()
        .flatten();
        let Some(device) = device else {
            return ApiError {
                status: StatusCode::UNAUTHORIZED,
                code: "unauthorized",
                message: "Este teléfono ya no tiene acceso; emparejalo de nuevo desde openADE".into(),
            }
            .into_response();
        };
        request.extensions_mut().insert(device);
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

struct ApiJson<T>(T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiError;
    async fn from_request(request: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|error| ApiError { status: error.status(), code: "invalid_request", message: "Pedido inválido".into() })
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> ApiResult<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(work).await.map_err(|_| ApiError::unavailable())?
}

async fn identity(State(shared): State<AppState>) -> Json<Value> {
    // The certificate already identifies the PC; its name waits for a token.
    Json(json!({
        "api": 2,
        "server_id": shared.server_id,
        "fingerprint": shared.fingerprint,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pair {
    pairing_id: String,
    secret: String,
    request_id: String,
    device_name: String,
}

async fn pair(
    State(shared): State<AppState>,
    Extension(Peer(address)): Extension<Peer>,
    ApiJson(request): ApiJson<Pair>,
) -> ApiResult<Json<Value>> {
    {
        let mut attempts = shared.pairing_attempts.lock().unwrap_or_else(|error| error.into_inner());
        attempts.retain(|_, (since, _)| since.elapsed() < Duration::from_secs(60));
        let (_, count) = attempts.entry(address).or_insert((std::time::Instant::now(), 0));
        if *count >= PAIRING_ATTEMPTS {
            return Err(ApiError {
                status: StatusCode::TOO_MANY_REQUESTS,
                code: "pairing_rate_limited",
                message: "Demasiados intentos; esperá un minuto".into(),
            });
        }
        *count += 1;
    }
    blocking(move || {
        let result = shared
            .pairing
            .complete(PairRequest {
                pairing_id: request.pairing_id,
                secret: request.secret,
                request_id: request.request_id,
                device_name: request.device_name,
            })
            .map_err(pairing_error)?;
        shared.bridge.paired(&result.device_name);
        Ok(Json(json!({
            "device_id": result.device_id,
            "device_name": result.device_name,
            "token": result.token,
            "server_id": shared.server_id,
            "name": shared.name,
            "fingerprint": shared.fingerprint,
            "routes": shared.routes(),
        })))
    })
    .await
}

fn pairing_error(error: anyhow::Error) -> ApiError {
    let (status, code, message) = match error.downcast_ref::<PairingError>() {
        Some(PairingError::Unauthorized) => (StatusCode::UNAUTHORIZED, "pairing_invalid", "Ese código no es válido"),
        Some(PairingError::Expired) => (StatusCode::GONE, "pairing_expired", "El código venció; generá otro en openADE"),
        Some(PairingError::AlreadyUsed | PairingError::Conflict) => {
            (StatusCode::CONFLICT, "pairing_used", "Ese código ya se usó")
        }
        Some(PairingError::InvalidRequest) => (StatusCode::BAD_REQUEST, "invalid_request", "Pedido inválido"),
        None => return ApiError::unavailable(),
    };
    ApiError { status, code, message: message.into() }
}

async fn pair_ack(
    State(shared): State<AppState>,
    Path(id): Path<String>,
    Extension(device): Extension<Device>,
) -> ApiResult<Json<Value>> {
    blocking(move || {
        shared.pairing.ack(&id, &device.id).map_err(pairing_error)?;
        Ok(Json(json!({"status": "acknowledged"})))
    })
    .await
}

async fn overview(State(shared): State<AppState>) -> Json<Value> {
    let overview = tasks::overview(&shared.sessions, &shared.bridge.live());
    Json(json!({
        "server": {"name": shared.name, "server_id": shared.server_id, "routes": shared.routes(), "push": shared.push.is_some()},
        "tasks": overview.tasks,
        "projects": overview.projects,
        "agents": overview.agents,
    }))
}

#[derive(Deserialize)]
struct StreamQuery {
    task: Option<String>,
    #[serde(default)]
    after: u64,
}

async fn stream(
    State(shared): State<AppState>,
    Extension(device): Extension<Device>,
    Query(query): Query<StreamQuery>,
) -> ApiResult<Response> {
    if let Some(task) = &query.task {
        storage::validate_id(task).map_err(|_| ApiError::not_found())?;
    }
    let connection = shared.bridge.connect(&device.id).ok_or(ApiError {
        status: StatusCode::TOO_MANY_REQUESTS,
        code: "too_many_streams",
        message: "Este teléfono ya tiene demasiadas conexiones abiertas".into(),
    })?;
    let access = crate::stream::Access { devices: shared.devices.clone(), device: device.id };
    Ok(crate::stream::open(shared.bridge.clone(), connection, access, shared.sessions.clone(), query.task, query.after)
        .into_response())
}

#[derive(Deserialize)]
struct HistoryQuery {
    before: Option<u64>,
}

async fn history(
    State(shared): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<tasks::History>> {
    storage::validate_id(&id).map_err(|_| ApiError::not_found())?;
    blocking(move || tasks::history(&shared.store, &id, query.before).map(Json)).await
}

/// Files uploaded before, by id, as paths the window can attach.
fn uploaded(shared: &Shared, ids: &[String]) -> ApiResult<Vec<PathBuf>> {
    if ids.len() > 8 {
        return Err(ApiError::bad("Hasta ocho archivos por mensaje"));
    }
    ids.iter()
        .map(|id| {
            Uuid::parse_str(id).map_err(|_| ApiError::bad("Archivo desconocido"))?;
            let folder = shared.root.join("uploads").join(id);
            std::fs::read_dir(&folder)
                .ok()
                .and_then(|mut entries| entries.next())
                .and_then(|entry| entry.ok())
                .map(|entry| entry.path())
                .ok_or_else(|| ApiError::bad("Ese archivo ya no está en el PC; volvé a adjuntarlo"))
        })
        .collect()
}

fn known_task(shared: &Shared, id: &str) -> ApiResult<()> {
    storage::validate_id(id).map_err(|_| ApiError::not_found())?;
    if shared.bridge.live().tasks.iter().any(|row| row.id == id) {
        Ok(())
    } else {
        Err(ApiError::not_found())
    }
}

fn prompt(text: String) -> ApiResult<String> {
    if text.trim().is_empty() || text.len() > 64 * 1024 {
        return Err(ApiError::bad("El mensaje está vacío o es demasiado largo"));
    }
    Ok(text)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateTask {
    request_id: String,
    project: String,
    backend: String,
    prompt: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    plan: bool,
}

async fn create_task(
    State(shared): State<AppState>,
    Extension(device): Extension<Device>,
    ApiJson(request): ApiJson<CreateTask>,
) -> ApiResult<Json<Value>> {
    let live = shared.bridge.live();
    let project = tasks::project_path(&live, &request.project).ok_or_else(ApiError::not_found)?;
    if !live.agents.iter().any(|agent| agent.startable && agent.backend == request.backend) {
        return Err(ApiError::bad("Ese agente no está disponible en el PC"));
    }
    let command = Command::CreateTask {
        project,
        backend: request.backend,
        prompt: prompt(request.prompt)?,
        files: uploaded(&shared, &request.files)?,
        plan: request.plan,
    };
    shared.bridge.call(&device.id, &request.request_id, command).await.map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendMessage {
    request_id: String,
    prompt: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default = "auto")]
    delivery: Delivery,
}

fn auto() -> Delivery {
    Delivery::Auto
}

async fn send_message(
    State(shared): State<AppState>,
    Extension(device): Extension<Device>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<SendMessage>,
) -> ApiResult<Json<Value>> {
    known_task(&shared, &id)?;
    let command = Command::Send {
        task: id,
        prompt: prompt(request.prompt)?,
        files: uploaded(&shared, &request.files)?,
        delivery: request.delivery,
    };
    shared.bridge.call(&device.id, &request.request_id, command).await.map(Json)
}

#[derive(Deserialize)]
struct RequestId {
    request_id: String,
}

async fn stop(
    State(shared): State<AppState>,
    Extension(device): Extension<Device>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<RequestId>,
) -> ApiResult<Json<Value>> {
    known_task(&shared, &id)?;
    shared.bridge.call(&device.id, &request.request_id, Command::Stop { task: id }).await.map(Json)
}

async fn read(State(shared): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    known_task(&shared, &id)?;
    shared.bridge.call_once(Command::Read { task: id }).await.map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionAnswer {
    /// `allow`, `allow_session`, `allow_always` or `deny`.
    decision: String,
    /// For `allow_always`: the prefix the phone showed, which must still be
    /// what the request would allow.
    always: Option<String>,
}

async fn permission(
    State(shared): State<AppState>,
    Extension(device): Extension<Device>,
    Path((id, request_id)): Path<(String, String)>,
    ApiJson(request): ApiJson<PermissionAnswer>,
) -> ApiResult<Json<Value>> {
    known_task(&shared, &id)?;
    let decision = match (request.decision.as_str(), request.always) {
        ("allow", None) => Decision::Allow,
        ("allow_session", None) => Decision::AllowSession,
        ("allow_always", Some(prefix)) if !prefix.trim().is_empty() => Decision::AllowAlways(prefix),
        ("deny", None) => Decision::Deny,
        _ => return Err(ApiError::bad("Decisión inválida")),
    };
    // A refused decision leaves the request open for another one.
    let key = format!("permission:{id}:{request_id}:{decision:?}");
    let command = Command::Permission { task: id, request_id, decision };
    shared.bridge.call(&device.id, &key, command).await.map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionAnswer {
    answer: String,
}

async fn answer(
    State(shared): State<AppState>,
    Extension(device): Extension<Device>,
    Path((id, request_id)): Path<(String, String)>,
    ApiJson(request): ApiJson<QuestionAnswer>,
) -> ApiResult<Json<Value>> {
    known_task(&shared, &id)?;
    let key = format!("answer:{id}:{request_id}:{:x}", sha2::Digest::finalize(<sha2::Sha256 as sha2::Digest>::new_with_prefix(request.answer.as_bytes())));
    let command = Command::Answer { task: id, request_id, answer: prompt(request.answer)? };
    shared.bridge.call(&device.id, &key, command).await.map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchTask {
    request_id: String,
    title: Option<String>,
    pinned: Option<bool>,
    archived: Option<bool>,
}

async fn patch_task(
    State(shared): State<AppState>,
    Extension(device): Extension<Device>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<PatchTask>,
) -> ApiResult<Json<Value>> {
    known_task(&shared, &id)?;
    if request.title.as_ref().is_some_and(|title| title.trim().is_empty() || title.len() > 200) {
        return Err(ApiError::bad("Título inválido"));
    }
    let command = Command::Patch { task: id, title: request.title, pinned: request.pinned, archived: request.archived };
    shared.bridge.call(&device.id, &request.request_id, command).await.map(Json)
}

/// A file the phone attaches, sent as the request body.
async fn upload(State(shared): State<AppState>, headers: HeaderMap, body: axum::body::Bytes) -> ApiResult<Json<Value>> {
    let name = headers
        .get("x-filename")
        .and_then(|value| value.to_str().ok())
        .map(|value| percent_decode(value))
        .unwrap_or_default();
    let name: String = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|character| !character.is_control())
        .take(120)
        .collect();
    let name = name.trim().trim_start_matches('.').to_owned();
    if name.is_empty() || body.is_empty() {
        return Err(ApiError::bad("Archivo vacío o sin nombre"));
    }
    let writing = shared.clone();
    let _writing = writing.uploads.acquire().await.map_err(|_| ApiError::unavailable())?;
    blocking(move || {
        if uploads_size(&shared.root.join("uploads")) + body.len() as u64 > UPLOADS_QUOTA {
            return Err(ApiError::bad("Hay demasiados archivos subidos sin usar; probá más tarde"));
        }
        let id = Uuid::new_v4().to_string();
        let folder = shared.root.join("uploads").join(&id);
        storage::prepare_directory(&folder).map_err(|_| ApiError::unavailable())?;
        storage::write_private(&folder.join(&name), &body).map_err(|_| ApiError::unavailable())?;
        Ok(Json(json!({"id": id, "filename": name, "size_bytes": body.len()})))
    })
    .await
}

/// What the uploads folder holds: a folder per upload, a file in each.
fn uploads_size(folder: &std::path::Path) -> u64 {
    let Ok(uploads) = std::fs::read_dir(folder) else { return 0 };
    uploads
        .flatten()
        .filter_map(|upload| std::fs::read_dir(upload.path()).ok())
        .flatten()
        .flatten()
        .filter_map(|file| file.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn percent_decode(value: &str) -> String {
    url::form_urlencoded::parse(format!("n={value}").as_bytes())
        .next()
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
}

async fn push_register(
    State(shared): State<AppState>,
    Extension(device): Extension<Device>,
    ApiJson(request): ApiJson<PushRegistrationRequest>,
) -> ApiResult<Json<Value>> {
    if shared.push.is_none() {
        return Err(ApiError::bad("Este PC no tiene configurados los avisos"));
    }
    blocking(move || {
        let status = PushRegistry::new(&shared.root)
            .register(&device.id, request)
            .map_err(|_| ApiError::bad("No se pudo registrar el teléfono para avisos"))?;
        Ok(Json(serde_json::to_value(status).map_err(|_| ApiError::unavailable())?))
    })
    .await
}

async fn push_remove(State(shared): State<AppState>, Extension(device): Extension<Device>) -> ApiResult<Json<Value>> {
    blocking(move || {
        let status = PushRegistry::new(&shared.root).remove(&device.id).map_err(|_| ApiError::unavailable())?;
        Ok(Json(serde_json::to_value(status).map_err(|_| ApiError::unavailable())?))
    })
    .await
}

fn project(shared: &Shared, id: &str) -> ApiResult<PathBuf> {
    tasks::project_path(&shared.bridge.live(), id).ok_or_else(ApiError::not_found)
}

#[derive(Deserialize)]
struct FilesQuery {
    #[serde(default)]
    path: String,
    query: Option<String>,
}

async fn files(
    State(shared): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FilesQuery>,
) -> ApiResult<Json<Value>> {
    let root = project(&shared, &id)?;
    blocking(move || workspace::list(&root, &query.path, query.query.as_deref()).map(Json)).await
}

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

async fn file(
    State(shared): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Json<Value>> {
    let root = project(&shared, &id)?;
    blocking(move || workspace::read(&root, &query.path).map(Json)).await
}

async fn save_file(
    State(shared): State<AppState>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<workspace::SaveFile>,
) -> ApiResult<Json<Value>> {
    let root = project(&shared, &id)?;
    blocking(move || {
        let _write = shared.writes.lock().unwrap_or_else(|error| error.into_inner());
        workspace::validate_save(&root, &request)?;
        workspace::save(&root, &request).map(Json)
    })
    .await
}

async fn diff(State(shared): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let root = project(&shared, &id)?;
    blocking(move || crate::diff::read_diff(&root).map(Json)).await
}

async fn git_status(State(shared): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let root = project(&shared, &id)?;
    blocking(move || {
        let status = git_workspace::status(&root)?;
        Ok(Json(serde_json::to_value(status).map_err(|_| ApiError::unavailable())?))
    })
    .await
}

#[derive(Deserialize)]
struct GitDiffQuery {
    path: String,
    #[serde(default)]
    staged: bool,
}

async fn git_diff(
    State(shared): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<GitDiffQuery>,
) -> ApiResult<Json<Value>> {
    let root = project(&shared, &id)?;
    blocking(move || git_workspace::diff(&root, &query.path, query.staged).map(Json)).await
}

async fn change_index(shared: AppState, id: String, request: git_workspace::ChangeIndex, stage: bool) -> ApiResult<Json<Value>> {
    let root = project(&shared, &id)?;
    blocking(move || {
        let _write = shared.writes.lock().unwrap_or_else(|error| error.into_inner());
        git_workspace::validate_change(&root, &request, stage)?;
        git_workspace::change_index(&root, &request, stage).map(Json)
    })
    .await
}

async fn git_stage(
    State(shared): State<AppState>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<git_workspace::ChangeIndex>,
) -> ApiResult<Json<Value>> {
    change_index(shared, id, request, true).await
}

async fn git_unstage(
    State(shared): State<AppState>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<git_workspace::ChangeIndex>,
) -> ApiResult<Json<Value>> {
    change_index(shared, id, request, false).await
}

async fn git_commit(
    State(shared): State<AppState>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<git_workspace::Commit>,
) -> ApiResult<Json<Value>> {
    let root = project(&shared, &id)?;
    blocking(move || {
        let _write = shared.writes.lock().unwrap_or_else(|error| error.into_inner());
        git_workspace::validate_commit(&root, &request)?;
        git_workspace::commit(&root, &request).map(Json)
    })
    .await
}
