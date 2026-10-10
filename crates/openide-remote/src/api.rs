use crate::{
    auth::{Device, DeviceStore},
    host::{
        ApiError, ApiResult, CancelTurn, CreateSession, Host, PatchSession, ResolvePermission,
        SessionQuery, StartTurn, SteerTurn,
    },
};
use axum::{
    extract::{DefaultBodyLimit, FromRequest, Path, Query, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::json;
use std::sync::Arc;

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error":{"code":self.code,"message":self.message}})),
        )
            .into_response()
    }
}

pub fn router(host: Arc<Host>) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/projects", get(projects))
        .route("/v1/projects/{id}/diff", get(diff))
        .route(
            "/v1/projects/{id}/attachments",
            post(upload_attachment).layer(DefaultBodyLimit::max(
                crate::attachments::MAX_UPLOAD_BODY_BYTES,
            )),
        )
        .route("/v1/projects/{id}/files", get(files))
        .route(
            "/v1/projects/{id}/file",
            get(file)
                .put(save_file)
                .layer(DefaultBodyLimit::max(7 * 1024 * 1024)),
        )
        .route("/v1/projects/{id}/git/status", get(git_status))
        .route("/v1/projects/{id}/git/diff", get(git_diff))
        .route("/v1/projects/{id}/git/stage", post(git_stage))
        .route("/v1/projects/{id}/git/unstage", post(git_unstage))
        .route("/v1/projects/{id}/git/commit", post(git_commit))
        .route("/v1/models", get(models))
        .route("/v1/pairing/complete", post(pairing_complete))
        .route("/v1/pairing/{id}/ack", post(pairing_ack))
        .route(
            "/v1/notifications/devices",
            post(push_register).delete(push_remove),
        )
        .route("/v1/sessions", get(sessions).post(create))
        .route("/v1/sessions/{id}", get(session).patch(patch_session))
        .route("/v1/sessions/{id}/turns", post(turn))
        .route("/v1/sessions/{id}/steer", post(steer))
        .route("/v1/sessions/{id}/cancel", post(cancel))
        .route(
            "/v1/sessions/{id}/permissions/{request_id}",
            post(permission),
        )
        .route("/v1/sessions/{id}/events", get(events))
        .fallback(|| async { ApiError::not_found() })
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(host.clone(), authenticate))
        .with_state(host)
}

struct ApiJson<T>(T);
impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiError;
    async fn from_request(
        request: axum::extract::Request,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|error| ApiError {
                status: error.status(),
                code: "invalid_request",
                message: "Invalid or oversized JSON request body".into(),
            })
    }
}

async fn authenticate(
    State(host): State<Arc<Host>>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    if request.method() == axum::http::Method::POST
        && request.uri().path() == "/v1/pairing/complete"
    {
        let mut response = next.run(request).await;
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
        return response;
    }
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned();
    let devices = DeviceStore::new(&host.root);
    let device = tokio::task::spawn_blocking(move || devices.authenticate_device(&token))
        .await
        .unwrap_or(None);
    let Some(device) = device else {
        return ApiError {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "Pair this device on the host or replace its revoked token".into(),
        }
        .into_response();
    };
    request.extensions_mut().insert(device);
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

fn pairing_error(error: anyhow::Error) -> ApiError {
    use crate::pairing::PairingError;
    match error.downcast_ref::<PairingError>() {
        Some(PairingError::Unauthorized) => ApiError {
            status: StatusCode::UNAUTHORIZED,
            code: "pairing_invalid",
            message: "Pairing invitation is invalid".into(),
        },
        Some(PairingError::Expired) => ApiError {
            status: StatusCode::GONE,
            code: "pairing_expired",
            message: "Pairing invitation expired; create a new one on the host".into(),
        },
        Some(PairingError::AlreadyUsed | PairingError::Conflict) => ApiError {
            status: StatusCode::CONFLICT,
            code: "pairing_used",
            message: "Pairing invitation was already redeemed by another request".into(),
        },
        Some(PairingError::InvalidRequest) => ApiError::bad("Invalid pairing request"),
        Some(PairingError::RateLimited) => ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "pairing_rate_limited",
            message: "Too many pairing attempts; wait a minute before retrying".into(),
        },
        None => ApiError::unavailable(),
    }
}
async fn pairing_complete(
    State(host): State<Arc<Host>>,
    ApiJson(request): ApiJson<crate::pairing::PairRequest>,
) -> ApiResult<Json<crate::pairing::PairingResult>> {
    blocking(move || {
        crate::pairing::PairingStore::new(&host.root)
            .complete(request)
            .map_err(pairing_error)
    })
    .await
}
async fn pairing_ack(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
    Extension(device): Extension<Device>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || {
        crate::pairing::PairingStore::new(&host.root)
            .ack(&id, &device.id)
            .map_err(pairing_error)?;
        Ok(json!({"status":"acknowledged"}))
    })
    .await
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> ApiResult<T> + Send + 'static,
) -> ApiResult<Json<T>> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| ApiError::unavailable())?
        .map(Json)
}

async fn health(State(host): State<Arc<Host>>) -> Json<serde_json::Value> {
    let mut capabilities = vec![
        "sessions",
        "activity",
        "approvals",
        "diff",
        "steering",
        "sessions.metadata",
        "models.list",
        "turn.options",
        "turn.approval_policy",
        "attachments.text",
    ];
    if cfg!(unix) {
        capabilities.extend(["workspace.files.read", "workspace.git.read"]);
        if host.config.allow_write {
            capabilities.extend([
                "workspace.files.write",
                "workspace.git.stage",
                "workspace.git.commit",
            ]);
        }
    }
    if host.push_enabled() {
        capabilities.push("notifications.push");
    }
    if host.images_enabled() {
        capabilities.push("attachments.images");
    }
    Json(
        json!({"version":"1","model":host.config.provider.model,"provider":host.config.provider.id,"name":"OpenIDE Remote","capabilities":capabilities,"terminal_input":false,"allow_write":host.config.allow_write,"allow_commands":host.config.allow_commands,"command_isolation":host.config.command_isolation}),
    )
}

async fn push_register(
    State(host): State<Arc<Host>>,
    Extension(device): Extension<Device>,
    ApiJson(request): ApiJson<crate::push::PushRegistrationRequest>,
) -> ApiResult<Json<crate::push::RegistrationStatus>> {
    blocking(move || {
        if !host.push_enabled() {
            return Err(ApiError::bad(
                "Push notifications are not configured on this host",
            ));
        }
        crate::push::PushRegistry::new(&host.root)
            .register(&device.id, request)
            .map_err(|_| ApiError::bad("Invalid push registration or private storage unavailable"))
    })
    .await
}
async fn push_remove(
    State(host): State<Arc<Host>>,
    Extension(device): Extension<Device>,
) -> ApiResult<Json<crate::push::RegistrationStatus>> {
    blocking(move || {
        crate::push::PushRegistry::new(&host.root)
            .remove(&device.id)
            .map_err(|_| ApiError::unavailable())
    })
    .await
}
async fn projects(State(host): State<Arc<Host>>) -> Json<serde_json::Value> {
    Json(json!({"projects":host.projects()}))
}

async fn sessions(
    State(host): State<Arc<Host>>,
    Query(query): Query<SessionQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.session_page(query)).await
}

async fn patch_session(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<PatchSession>,
) -> ApiResult<Json<crate::host::SessionView>> {
    blocking(move || host.patch_session(&id, request)).await
}
async fn models(State(host): State<Arc<Host>>) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || Ok(host.models())).await
}
#[derive(Deserialize)]
struct FilesQuery {
    #[serde(default)]
    path: String,
    query: Option<String>,
}
#[derive(Deserialize)]
struct FileQuery {
    path: String,
}
#[derive(Deserialize)]
struct GitDiffQuery {
    path: String,
    #[serde(default)]
    staged: bool,
}
async fn files(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
    Query(query): Query<FilesQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.files(&id, &query.path, query.query.as_deref())).await
}
async fn file(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.read_file(&id, &query.path)).await
}
async fn save_file(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<crate::workspace::SaveFile>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.save_file(&id, request)).await
}
async fn git_status(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.git_status(&id)).await
}
async fn git_diff(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
    Query(query): Query<GitDiffQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.git_diff(&id, &query.path, query.staged)).await
}
async fn git_stage(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<crate::git_workspace::ChangeIndex>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.git_index(&id, request, true)).await
}
async fn git_unstage(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<crate::git_workspace::ChangeIndex>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.git_index(&id, request, false)).await
}
async fn git_commit(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<crate::git_workspace::Commit>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.git_commit(&id, request)).await
}
async fn session(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
) -> ApiResult<Json<crate::host::SessionView>> {
    blocking(move || host.session(&id)).await
}
async fn create(
    State(host): State<Arc<Host>>,
    ApiJson(request): ApiJson<CreateSession>,
) -> ApiResult<Json<crate::host::SessionView>> {
    blocking(move || host.create_session(request)).await
}
async fn turn(
    State(host): State<Arc<Host>>,
    Extension(device): Extension<Device>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<StartTurn>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.start_turn_for_device(&device.id, &id, request)).await
}

async fn upload_attachment(
    State(host): State<Arc<Host>>,
    Extension(device): Extension<Device>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<crate::attachments::UploadAttachment>,
) -> ApiResult<Json<crate::attachments::AttachmentSummary>> {
    blocking(move || host.upload_attachment(&device.id, &id, request)).await
}
async fn steer(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<SteerTurn>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.steer(&id, request)).await
}
async fn cancel(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<CancelTurn>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.cancel(&id, request)).await
}
async fn permission(
    State(host): State<Arc<Host>>,
    Path((id, request_id)): Path<(String, String)>,
    ApiJson(request): ApiJson<ResolvePermission>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.permission(&id, &request_id, request)).await
}
#[derive(Deserialize)]
struct EventQuery {
    #[serde(default)]
    after: u64,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    100
}
async fn events(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
    Query(query): Query<EventQuery>,
) -> ApiResult<Json<crate::projection::EventPage>> {
    blocking(move || host.events(&id, query.after, query.limit)).await
}
async fn diff(
    State(host): State<Arc<Host>>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || host.diff(&id)).await
}
