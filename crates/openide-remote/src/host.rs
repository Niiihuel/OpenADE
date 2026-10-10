use crate::{
    config::{Config, Project},
    git_workspace,
    projection::{self, EventPage, PendingPermission},
    storage, workspace,
};
use axum::http::StatusCode;
use openide_core::{
    replay_native, AgentEventKind, AgentRunRequest, CheckpointCompatibility, NativeReplay,
    PermissionDecision, SessionStore, StoredSession, TurnMode,
};
use openide_harness::{
    AgentRunHandle, AgentRunner, LocalTools, ModelBackend, PermissionChoice, PermissionMode,
    PermissionPolicy, RunLimits,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

#[cfg(test)]
#[path = "host_model_tests.rs"]
mod model_tests;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}
impl ApiError {
    pub fn bad(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message: message.into(),
        }
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "conflict",
            message: message.into(),
        }
    }
    pub fn uncertain(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "operation_uncertain",
            message: message.into(),
        }
    }
    pub fn idempotency_conflict() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "idempotency_conflict",
            message: "request_id already belongs to different input".into(),
        }
    }
    pub fn stale_turn() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "stale_turn",
            message: "The active turn no longer matches this request".into(),
        }
    }
    pub fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: "Project or session is not authorized on this host".into(),
        }
    }
    pub fn unavailable() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "host_error",
            message: "The host could not persist or validate this operation".into(),
        }
    }
}
pub type ApiResult<T> = Result<T, ApiError>;
fn internal(_: impl std::fmt::Display) -> ApiError {
    ApiError::unavailable()
}
fn id(value: &str) -> ApiResult<()> {
    storage::validate_id(value).map_err(|_| ApiError::bad("Invalid identifier"))
}

#[derive(Clone, Serialize, Deserialize)]
struct SessionRecord {
    project_id: String,
    workspace: PathBuf,
    provider_digest: String,
}

#[derive(Serialize, Deserialize)]
struct OperationRecord {
    digest: String,
    response: Option<Value>,
}

#[derive(Serialize, Deserialize)]
struct TurnInputBinding {
    digest: String,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    approval_policy: Option<RemoteApprovalPolicy>,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteApprovalPolicy {
    #[default]
    Ask,
    Auto,
    FullAccess,
}

fn turn_permission_policy(
    config: &Config,
    mode: TurnMode,
    approval: RemoteApprovalPolicy,
) -> PermissionPolicy {
    let mode_for = |enabled: bool| {
        if !enabled || mode == TurnMode::Plan {
            PermissionMode::Deny
        } else if approval == RemoteApprovalPolicy::FullAccess {
            PermissionMode::Allow
        } else {
            PermissionMode::Ask
        }
    };
    PermissionPolicy::new()
        .with_default_mode(PermissionMode::Deny)
        .with_tool("read_file", PermissionMode::Allow)
        .with_tool("list_files", PermissionMode::Allow)
        .with_tool("search_files", PermissionMode::Allow)
        .with_tool("write_file", mode_for(config.allow_write))
        .with_tool("apply_patch", mode_for(config.allow_write))
        .with_tool("run_command", mode_for(config.allow_commands))
        .with_auto_workspace_edits(
            approval == RemoteApprovalPolicy::Auto && mode == TurnMode::Build,
        )
        .with_timeout(Duration::from_secs(15 * 60))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSession {
    pub project_id: String,
    pub request_id: String,
    pub title: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartTurn {
    pub request_id: String,
    pub prompt: String,
    pub mode: TurnMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<RemoteApprovalPolicy>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CancelTurn {
    pub request_id: String,
    pub turn_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SteerTurn {
    pub request_id: String,
    pub turn_id: String,
    pub prompt: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvePermission {
    pub decision: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PatchSession {
    pub request_id: String,
    pub title: Option<String>,
    pub archived: Option<bool>,
    pub favorite: Option<bool>,
}

#[derive(Deserialize, Default)]
pub struct SessionQuery {
    pub project_id: Option<String>,
    pub query: Option<String>,
    pub archived: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct SessionView {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub archived: bool,
    pub favorite: bool,
    pub status: &'static str,
    pub updated_at_ms: u64,
    pub active_turn_id: Option<String>,
    pub pending_permissions: Vec<PendingPermission>,
    pub origin: &'static str,
    pub can_start_turn: bool,
    pub can_control_active: bool,
}

struct Active {
    turn_id: String,
    handle: AgentRunHandle,
}
#[derive(Default)]
struct Runtime {
    active: HashMap<String, Active>,
    shutting_down: bool,
}

pub struct Host {
    pub config: Config,
    pub root: PathBuf,
    sessions: Arc<SessionStore>,
    model: Arc<dyn ModelBackend>,
    runtime: Mutex<Runtime>,
    workspace_mutations: Mutex<()>,
    catalog_refresh: crate::models::CatalogRefresh,
    push: Option<(Arc<crate::push::PushService>, tokio::runtime::Handle)>,
    desktop: Option<crate::desktop::DesktopSessions>,
    #[cfg(test)]
    catalog_override: Mutex<Option<crate::models::ModelCatalog>>,
    // The native lease protects each journal; this additional owner lock
    // protects the host's sidecars and command admission across processes.
    _owner_lock: File,
}

impl Host {
    pub fn open(
        root: &Path,
        config: Config,
        model: Arc<dyn ModelBackend>,
    ) -> anyhow::Result<Arc<Self>> {
        config.validate()?;
        if model.model_id() != config.provider.model {
            anyhow::bail!("backend/config model mismatch");
        }
        if config.provider.image_input && !model.supports_user_images() {
            anyhow::bail!(
                "image_input is enabled but this model transport cannot send user images"
            );
        }
        storage::prepare_directory(root)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let owner_lock = options.open(root.join("host.lock"))?;
        if !owner_lock.metadata()?.is_file() {
            anyhow::bail!("host lock is not a regular file");
        }
        owner_lock
            .try_lock()
            .map_err(|_| anyhow::anyhow!("another host owns this state directory"))?;
        let push = crate::push::PushService::open(root)?.and_then(|service| {
            tokio::runtime::Handle::try_current()
                .ok()
                .map(|handle| (Arc::new(service), handle))
        });
        let desktop = crate::desktop::DesktopSessions::open(&config, root)?;
        let catalog_refresh = crate::models::CatalogRefresh::new(&config.provider);
        let host = Arc::new(Self {
            config,
            root: root.to_owned(),
            sessions: Arc::new(SessionStore::open(root.join("sessions"))?),
            model,
            runtime: Mutex::new(Runtime::default()),
            workspace_mutations: Mutex::new(()),
            catalog_refresh,
            push,
            desktop,
            #[cfg(test)]
            catalog_override: Mutex::new(None),
            _owner_lock: owner_lock,
        });
        let weak = Arc::downgrade(&host);
        // Drain the bounded runner channel even when no mobile client exists.
        std::thread::Builder::new()
            .name("openide-remote-drain".into())
            .spawn(move || {
                while let Some(host) = weak.upgrade() {
                    host.poll();
                    drop(host);
                    std::thread::sleep(Duration::from_millis(20));
                }
            })?;
        Ok(host)
    }

    pub fn projects(&self) -> Vec<Project> {
        self.config.projects.clone()
    }

    pub fn push_enabled(&self) -> bool {
        self.push.is_some()
    }

    fn project(&self, project_id: &str) -> ApiResult<&Project> {
        let project = self
            .config
            .projects
            .iter()
            .find(|p| p.id == project_id)
            .ok_or_else(ApiError::not_found)?;
        // A symlink retarget or removed allowlisted workspace fails closed.
        if std::fs::canonicalize(&project.path).ok().as_ref() != Some(&project.path) {
            return Err(ApiError::conflict(
                "Authorized project is no longer at its registered path",
            ));
        }
        Ok(project)
    }

    fn record_path(&self, session_id: &str) -> PathBuf {
        self.root
            .join("remote-sessions")
            .join(format!("{session_id}.json"))
    }

    fn desktop_for_session(
        &self,
        session_id: &str,
    ) -> ApiResult<Option<&crate::desktop::DesktopSessions>> {
        id(session_id)?;
        match std::fs::symlink_metadata(self.record_path(session_id)) {
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(self.desktop.as_ref()),
            Err(_) => Err(ApiError::unavailable()),
        }
    }

    fn load(&self, session_id: &str) -> ApiResult<(SessionRecord, StoredSession)> {
        id(session_id)?;
        let path = self.record_path(session_id);
        if !path.exists() {
            return Err(ApiError::not_found());
        }
        let record: SessionRecord = storage::read_json(&path).map_err(internal)?;
        let project = self.project(&record.project_id)?;
        if project.path != record.workspace {
            return Err(ApiError::not_found());
        }
        let session = self
            .sessions
            .load_strict(session_id)
            .map_err(internal)?
            .ok_or_else(ApiError::not_found)?;
        if session.session.workspace != project.path || session.session.backend != "native" {
            return Err(ApiError::not_found());
        }
        Ok((record, session))
    }

    fn compatibility(&self) -> CheckpointCompatibility {
        CheckpointCompatibility {
            provider: self.config.provider.id.clone(),
            model: self.config.provider.model.clone(),
            context_format_key: "openide-remote-v1".into(),
        }
    }

    fn replay(&self, session: &StoredSession) -> ApiResult<NativeReplay> {
        replay_native(
            &session.session.id,
            &session.events,
            None,
            &self.compatibility(),
        )
        .map_err(|_| {
            ApiError::uncertain("Session journal needs local recovery; no effect has been retried")
        })
    }

    fn provider_digest(&self) -> ApiResult<String> {
        digest(&self.config.provider)
    }

    fn session_view(
        &self,
        record: &SessionRecord,
        stored: &StoredSession,
        active: Option<&Active>,
    ) -> ApiResult<SessionView> {
        let replay = self.replay(stored)?;
        let pending = if active.is_some() {
            projection::pending_permissions(&stored.events)
        } else {
            Vec::new()
        };
        let status = if active.is_some() {
            if pending.is_empty() {
                "running"
            } else {
                "waiting_permission"
            }
        } else if !replay.can_start_new_turn {
            "interrupted"
        } else {
            stored
                .events
                .iter()
                .rev()
                .find_map(|e| match e.kind {
                    AgentEventKind::TurnCompleted { .. } => Some("completed"),
                    AgentEventKind::TurnFailed { .. } => Some("failed"),
                    AgentEventKind::TurnCancelled => Some("cancelled"),
                    _ => None,
                })
                .unwrap_or("idle")
        };
        Ok(SessionView {
            id: stored.session.id.clone(),
            project_id: record.project_id.clone(),
            archived: stored.session.archived,
            favorite: stored.session.pinned,
            title: stored
                .session
                .title
                .clone()
                .unwrap_or_else(|| "Nueva tarea".into()),
            status,
            updated_at_ms: stored
                .events
                .last()
                .map_or(stored.session.updated_at_ms, |e| e.timestamp_ms),
            active_turn_id: active.map(|a| a.turn_id.clone()),
            pending_permissions: pending,
            origin: "remote",
            can_start_turn: replay.can_start_new_turn && active.is_none(),
            can_control_active: active.is_some(),
        })
    }

    pub fn session(&self, session_id: &str) -> ApiResult<SessionView> {
        if let Some(desktop) = self.desktop_for_session(session_id)? {
            return desktop.session(session_id);
        }
        let runtime = self.runtime.lock().map_err(internal)?;
        let (record, stored) = self.load(session_id)?;
        self.session_view(&record, &stored, runtime.active.get(session_id))
    }

    pub fn list_sessions(&self, project_id: Option<&str>) -> ApiResult<Vec<SessionView>> {
        if let Some(id) = project_id {
            self.project(id)?;
        }
        let runtime = self.runtime.lock().map_err(internal)?;
        let records = self.root.join("remote-sessions");
        if !records.exists() {
            return self
                .desktop
                .as_ref()
                .map_or_else(|| Ok(Vec::new()), |desktop| desktop.list(project_id));
        }
        let mut views = Vec::new();
        for entry in std::fs::read_dir(records).map_err(internal)? {
            let entry = entry.map_err(internal)?;
            if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let session_id = entry
                .path()
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned();
            // Removed projects are no longer remotely visible; corrupt journals
            // remain an error instead of silently reporting an empty history.
            let (record, stored) = match self.load(&session_id) {
                Ok(v) => v,
                Err(e) if e.status == StatusCode::NOT_FOUND => continue,
                Err(e) => return Err(e),
            };
            if project_id.is_some_and(|p| p != record.project_id) {
                continue;
            }
            views.push(self.session_view(&record, &stored, runtime.active.get(&session_id))?);
        }
        if let Some(desktop) = &self.desktop {
            let remote_ids = views
                .iter()
                .map(|view| view.id.clone())
                .collect::<std::collections::HashSet<_>>();
            views.extend(
                desktop
                    .list(project_id)?
                    .into_iter()
                    .filter(|view| !remote_ids.contains(&view.id)),
            );
        }
        views.sort_by(|a, b| (b.updated_at_ms, &b.id).cmp(&(a.updated_at_ms, &a.id)));
        Ok(views)
    }

    pub fn session_page(&self, query: SessionQuery) -> ApiResult<Value> {
        let archived = query.archived.as_deref().unwrap_or("all");
        if !matches!(archived, "all" | "true" | "false") {
            return Err(ApiError::bad("archived must be all, true or false"));
        }
        let search = query.query.as_deref().unwrap_or("").to_lowercase();
        if search.len() > 256 {
            return Err(ApiError::bad("Search exceeds 256 bytes"));
        }
        let cursor = query
            .cursor
            .as_deref()
            .map(|cursor| {
                let (time, id) = cursor
                    .split_once(':')
                    .ok_or_else(|| ApiError::bad("Invalid session cursor"))?;
                storage::validate_id(id).map_err(|_| ApiError::bad("Invalid session cursor"))?;
                Ok::<_, ApiError>((
                    time.parse::<u64>()
                        .map_err(|_| ApiError::bad("Invalid session cursor"))?,
                    id,
                ))
            })
            .transpose()?;
        let limit = query.limit.unwrap_or(100).clamp(1, 100);
        let mut sessions = self
            .list_sessions(query.project_id.as_deref())?
            .into_iter()
            .filter(|s| {
                (archived == "all" || s.archived == (archived == "true"))
                    && (search.is_empty()
                        || s.title.to_lowercase().contains(&search)
                        || s.id.contains(&search))
                    && cursor.is_none_or(|c| (s.updated_at_ms, s.id.as_str()) < c)
            })
            .collect::<Vec<_>>();
        let has_more = sessions.len() > limit;
        sessions.truncate(limit);
        let next_cursor = if has_more {
            sessions
                .last()
                .map(|s| format!("{}:{}", s.updated_at_ms, s.id))
        } else {
            None
        };
        let partial = self
            .desktop
            .as_ref()
            .is_some_and(|desktop| desktop.has_unreadable_records());
        Ok(json!({"sessions":sessions,"next_cursor":next_cursor,"partial":partial}))
    }

    pub fn patch_session(&self, session_id: &str, request: PatchSession) -> ApiResult<SessionView> {
        if request
            .title
            .as_ref()
            .is_some_and(|t| t.trim().is_empty() || t.len() > 256)
        {
            return Err(ApiError::bad("Title must contain 1–256 bytes"));
        }
        let runtime = self.runtime.lock().map_err(internal)?;
        let (record, stored) = self.load(session_id)?;
        let (path, digest, prior) = self.operation(
            &format!("session_metadata:{session_id}"),
            &request.request_id,
            &request,
        )?;
        if prior.is_some() {
            return self.session_view(&record, &stored, runtime.active.get(session_id));
        }
        storage::write_json(
            &path,
            &OperationRecord {
                digest: digest.clone(),
                response: None,
            },
        )
        .map_err(internal)?;
        if let Some(title) = &request.title {
            self.sessions
                .rename(session_id, Some(title.trim()))
                .map_err(internal)?;
        }
        if let Some(archived) = request.archived {
            self.sessions
                .set_archived(session_id, archived)
                .map_err(internal)?;
        }
        if let Some(favorite) = request.favorite {
            self.sessions
                .set_pinned(session_id, favorite)
                .map_err(internal)?;
        }
        storage::write_json(
            &path,
            &OperationRecord {
                digest,
                response: Some(json!({"session_id":session_id})),
            },
        )
        .map_err(internal)?;
        let (_, stored) = self.load(session_id)?;
        self.session_view(&record, &stored, runtime.active.get(session_id))
    }

    pub fn models(&self) -> Value {
        // Called through the HTTP blocking pool, without the runner mutex.
        self.catalog_refresh
            .refresh(&self.config.provider, &self.root);
        let mut result =
            serde_json::to_value(self.model_catalog()).expect("serializable model catalog");
        for model in result["models"].as_array_mut().expect("catalog models") {
            model["capabilities"] =
                json!({"text":true,"tools":true,"remote_images":model["supports_images"]});
        }
        result
    }

    fn model_catalog(&self) -> crate::models::ModelCatalog {
        #[cfg(test)]
        if let Some(catalog) = self
            .catalog_override
            .lock()
            .expect("test catalog lock")
            .as_ref()
        {
            return catalog.clone();
        }
        self.catalog_refresh.load(&self.config.provider, &self.root)
    }

    pub fn images_enabled(&self) -> bool {
        self.config.provider.image_input && self.model.supports_user_images()
    }

    pub fn upload_attachment(
        &self,
        device_id: &str,
        project_id: &str,
        request: crate::attachments::UploadAttachment,
    ) -> ApiResult<crate::attachments::AttachmentSummary> {
        self.project(project_id)?;
        crate::attachments::AttachmentStore::new(&self.root).upload(
            device_id,
            project_id,
            request,
            self.images_enabled(),
        )
    }

    pub fn files(&self, project_id: &str, path: &str, query: Option<&str>) -> ApiResult<Value> {
        workspace::list(&self.project(project_id)?.path, path, query)
    }
    pub fn read_file(&self, project_id: &str, path: &str) -> ApiResult<Value> {
        workspace::read(&self.project(project_id)?.path, path)
    }
    fn require_workspace_write(&self) -> ApiResult<()> {
        if !self.config.allow_write {
            return Err(ApiError {
                status: StatusCode::FORBIDDEN,
                code: "write_disabled",
                message: "Workspace mutations are disabled on this host".into(),
            });
        }
        Ok(())
    }
    fn workspace_operation(
        &self,
        scope: &str,
        request_id: &str,
        payload: &impl Serialize,
        validate: impl FnOnce() -> ApiResult<()>,
        apply: impl FnOnce() -> ApiResult<Value>,
    ) -> ApiResult<Value> {
        self.require_workspace_write()?;
        let _lock = self.workspace_mutations.lock().map_err(internal)?;
        let (path, digest, prior) = self.operation(scope, request_id, payload)?;
        if let Some(prior) = prior {
            return match prior.get("_rejected").and_then(Value::as_str) {
                Some("file_conflict") => Err(workspace::conflict()),
                Some("git_conflict") => Err(git_workspace::conflict()),
                Some(_) => Err(ApiError::bad(
                    "This operation was rejected; inspect its input before a new request",
                )),
                None => Ok(prior),
            };
        }
        validate()?;
        storage::write_json(
            &path,
            &OperationRecord {
                digest: digest.clone(),
                response: None,
            },
        )
        .map_err(internal)?;
        match apply() {
            Ok(response) => {
                storage::write_json(&path, &OperationRecord { digest, response: Some(response.clone()) }).map_err(internal)?;
                Ok(response)
            },
            Err(error) if matches!(error.code, "file_conflict" | "git_conflict") => {
                storage::write_json(&path, &OperationRecord { digest, response: Some(json!({"_rejected":error.code})) }).map_err(internal)?;
                Err(error)
            },
            Err(_) => Err(ApiError::uncertain("Operation did not return a durable result; refresh and inspect local state before recovery")),
        }
    }
    pub fn save_file(&self, project_id: &str, request: workspace::SaveFile) -> ApiResult<Value> {
        let root = &self.project(project_id)?.path;
        self.workspace_operation(
            &format!("file:{project_id}"),
            &request.request_id,
            &request,
            || workspace::validate_save(root, &request),
            || workspace::save(root, &request),
        )
    }
    pub fn git_status(&self, project_id: &str) -> ApiResult<Value> {
        serde_json::to_value(git_workspace::status(&self.project(project_id)?.path)?)
            .map_err(internal)
    }
    pub fn git_diff(&self, project_id: &str, path: &str, staged: bool) -> ApiResult<Value> {
        git_workspace::diff(&self.project(project_id)?.path, path, staged)
    }
    pub fn git_index(
        &self,
        project_id: &str,
        request: git_workspace::ChangeIndex,
        stage: bool,
    ) -> ApiResult<Value> {
        let root = &self.project(project_id)?.path;
        self.workspace_operation(
            &format!("git_index:{project_id}"),
            &request.request_id,
            &(&request, stage),
            || git_workspace::validate_change(root, &request, stage),
            || git_workspace::change_index(root, &request, stage),
        )
    }
    pub fn git_commit(&self, project_id: &str, request: git_workspace::Commit) -> ApiResult<Value> {
        let root = &self.project(project_id)?.path;
        self.workspace_operation(
            &format!("git_commit:{project_id}"),
            &request.request_id,
            &request,
            || git_workspace::validate_commit(root, &request),
            || git_workspace::commit(root, &request),
        )
    }

    fn operation(
        &self,
        scope: &str,
        request_id: &str,
        payload: &impl Serialize,
    ) -> ApiResult<(PathBuf, String, Option<Value>)> {
        id(request_id)?;
        let key = digest(&(scope, request_id))?;
        let path = self.root.join("operations").join(format!("{key}.json"));
        let payload_digest = digest(payload)?;
        if path.exists() {
            let record: OperationRecord = storage::read_json(&path).map_err(internal)?;
            if record.digest != payload_digest {
                return Err(ApiError::idempotency_conflict());
            }
            if record.response.is_none() {
                return Err(ApiError::uncertain(
                    "Previous operation outcome is uncertain; inspect the host before retrying",
                ));
            }
            return Ok((path, payload_digest, record.response));
        }
        Ok((path, payload_digest, None))
    }

    pub fn create_session(&self, request: CreateSession) -> ApiResult<SessionView> {
        if request.title.as_ref().is_some_and(|s| s.len() > 256) {
            return Err(ApiError::bad("Title exceeds 256 bytes"));
        }
        let runtime = self.runtime.lock().map_err(internal)?;
        if runtime.shutting_down {
            return Err(ApiError::conflict("Host is shutting down"));
        }
        let project = self.project(&request.project_id)?;
        let (path, digest, prior) =
            self.operation("create_session", &request.request_id, &request)?;
        if let Some(value) = prior {
            let session_id = value["session_id"]
                .as_str()
                .ok_or_else(ApiError::unavailable)?;
            let (record, stored) = self.load(session_id)?;
            return self.session_view(&record, &stored, runtime.active.get(session_id));
        }
        storage::write_json(
            &path,
            &OperationRecord {
                digest: digest.clone(),
                response: None,
            },
        )
        .map_err(internal)?;
        let session = self
            .sessions
            .create(&project.path, "native")
            .map_err(internal)?;
        if request.title.is_some() {
            self.sessions
                .rename(&session.id, request.title.as_deref())
                .map_err(internal)?;
        }
        let record = SessionRecord {
            project_id: project.id.clone(),
            workspace: project.path.clone(),
            provider_digest: self.provider_digest()?,
        };
        storage::write_json(&self.record_path(&session.id), &record).map_err(internal)?;
        storage::write_json(
            &path,
            &OperationRecord {
                digest,
                response: Some(json!({"session_id":session.id})),
            },
        )
        .map_err(internal)?;
        let (_, stored) = self.load(&session.id)?;
        self.session_view(&record, &stored, None)
    }

    pub fn start_turn(&self, session_id: &str, request: StartTurn) -> ApiResult<Value> {
        self.start_turn_for_device("local", session_id, request)
    }

    pub fn start_turn_for_device(
        &self,
        device_id: &str,
        session_id: &str,
        request: StartTurn,
    ) -> ApiResult<Value> {
        id(&request.request_id)?;
        id(device_id)?;
        if (request.prompt.trim().is_empty() && request.attachment_ids.is_empty())
            || request.prompt.len() > 32 * 1024
        {
            return Err(ApiError::bad(
                "Prompt must contain 1–32768 bytes or an attachment",
            ));
        }
        let mut runtime = self.runtime.lock().map_err(internal)?;
        if runtime.shutting_down {
            return Err(ApiError::conflict("Host is shutting down"));
        }
        let (record, stored) = self.load(session_id)?;
        let input_digest = digest(&(device_id, session_id, &request))?;
        let binding_path = self.root.join("turn-inputs").join(format!(
            "{}.json",
            digest(&(session_id, &request.request_id))?
        ));
        let binding: Option<TurnInputBinding> = if binding_path.exists() {
            Some(storage::read_json(&binding_path).map_err(internal)?)
        } else {
            None
        };
        if binding
            .as_ref()
            .is_some_and(|binding| binding.digest != input_digest)
        {
            return Err(ApiError::idempotency_conflict());
        }
        // Admission is the durable idempotency key. An uncertain response never
        // re-launches an already admitted run, including after host restart.
        for event in &stored.events {
            if let Some((turn_id, prompt, model, mode)) = match &event.kind {
                AgentEventKind::TurnAdmitted {
                    turn_id,
                    prompt,
                    model,
                    mode,
                }
                | AgentEventKind::TurnAdmittedWithImages {
                    turn_id,
                    prompt,
                    model,
                    mode,
                    ..
                } => Some((turn_id, prompt, model, mode)),
                _ => None,
            } {
                if turn_id == &request.request_id {
                    if (binding.is_none()
                        && (prompt != &request.prompt
                            || !request.attachment_ids.is_empty()
                            || request.reasoning_effort.is_some()
                            || request.approval_policy.is_some()))
                        || *mode != request.mode
                        || model
                            != binding
                                .as_ref()
                                .and_then(|value| value.model_id.as_ref())
                                .or(request.model_id.as_ref())
                                .unwrap_or(&self.config.provider.model)
                    {
                        return Err(ApiError::idempotency_conflict());
                    }
                    let mut last = "interrupted";
                    for later in stored
                        .events
                        .iter()
                        .skip_while(|e| e.sequence <= event.sequence)
                    {
                        match later.kind {
                            AgentEventKind::TurnAdmitted { .. }
                            | AgentEventKind::TurnAdmittedWithImages { .. } => break,
                            AgentEventKind::TurnCompleted { .. } => last = "completed",
                            AgentEventKind::TurnFailed { .. } => last = "failed",
                            AgentEventKind::TurnCancelled => last = "cancelled",
                            _ => {}
                        }
                    }
                    if runtime
                        .active
                        .get(session_id)
                        .is_some_and(|a| a.turn_id == *turn_id)
                    {
                        last = if projection::pending_permissions(&stored.events).is_empty() {
                            "running"
                        } else {
                            "waiting_permission"
                        };
                    }
                    return Ok(json!({"turn_id":turn_id,"status":last}));
                }
            }
        }
        if runtime.active.contains_key(session_id) {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "turn_busy",
                message: "A turn is active; wait, cancel it, or use steering with its turn_id"
                    .into(),
            });
        }
        if runtime.active.len() >= 4 {
            return Err(ApiError::conflict("Host already has four active turns"));
        }
        if record.provider_digest != self.provider_digest()? {
            return Err(ApiError::conflict("Provider configuration changed; create a new session to preserve history provenance"));
        }
        if !self.catalog_refresh.account_matches(&self.config.provider) {
            return Err(ApiError::bad("La cuenta de ChatGPT cambió o fue desconectada. Reinicia el host para usar la conexión actual."));
        }
        // Resolve only for a new admission. Retries use the frozen journal plus
        // this device/input binding, even if the upload later becomes unavailable.
        let catalog = self.model_catalog();
        let selected = catalog.select(
            request.model_id.as_deref(),
            request.reasoning_effort.as_deref(),
        )?;
        let model =
            if selected.id == self.config.provider.model && request.reasoning_effort.is_none() {
                self.model.clone()
            } else {
                crate::config::build_backend_with_options(
                    &self.config.provider,
                    &selected.id,
                    request.reasoning_effort.as_deref(),
                )
                .map_err(|_| {
                    ApiError::bad("Selected model settings cannot be constructed by this host")
                })?
            };
        let prepared = crate::attachments::AttachmentStore::new(&self.root).resolve(
            device_id,
            &record.project_id,
            &request.attachment_ids,
            selected.supports_images && model.supports_user_images(),
        )?;
        let effective_prompt = format!("{}{}", request.prompt, prepared.text_context);
        let lease = self
            .sessions
            .try_acquire_native_lease(session_id)
            .map_err(|_| ApiError::conflict("Session is already owned by another runner"))?;
        let mut replay = self.replay(&stored)?;
        if !replay.can_start_new_turn {
            return Err(ApiError::uncertain(
                "Interrupted work requires local recovery; no effect has been retried",
            ));
        }
        if !selected.supports_images
            && replay
                .history
                .iter()
                .any(|message| !message.images.is_empty())
        {
            return Err(ApiError::bad("This session contains images; choose an image-capable model or create a new session"));
        }
        let tools = LocalTools::new()
            .with_tool_permission(
                openide_harness::LocalTool::WriteFile,
                self.config.allow_write && request.mode == TurnMode::Build,
            )
            .with_tool_permission(
                openide_harness::LocalTool::RunCommand,
                self.config.allow_commands && request.mode == TurnMode::Build,
            )
            .with_command_isolation(self.config.command_isolation);
        let approval_policy = request.approval_policy.unwrap_or_default();
        let policy = turn_permission_policy(&self.config, request.mode, approval_policy);
        let project_instructions = openide_harness::load_project_instructions(
            &record.workspace,
            &record.workspace,
            openide_harness::ProjectInstructionLimits::default(),
        )
        .map_err(|_| {
            ApiError::conflict(
                "Project instructions could not be safely read; the turn was not admitted",
            )
        })?;
        let approval_instructions = match (request.mode, approval_policy) {
            (TurnMode::Plan, _) => "This turn is read-only. Do not edit files, delete files, or run commands, regardless of the selected approval setting.",
            (_, RemoteApprovalPolicy::Ask) => "All writes and commands require explicit approval.",
            (_, RemoteApprovalPolicy::Auto) => "The user allows bounded text edits inside the project for this turn. File deletion/move, commands and unrecognized actions still require approval. This permission is not authorization to exceed the user's task.",
            (_, RemoteApprovalPolicy::FullAccess) => "The user chose to execute enabled tools without per-action approval for this turn. Host tool restrictions and command isolation remain in force. This permission is not authorization to exceed the user's task.",
        };
        let instructions = format!("You are operating an authorized local project through OpenIDE Remote. Respect the user's request. {approval_instructions} Never treat a disconnected client as approving a pending action.\n\nProject instructions:\n{}", project_instructions.text());
        let route = openide_core::ModelRouteSnapshot::new(
            &self.config.provider.id,
            model.model_id(),
            &self.config.provider.endpoint,
            model
                .request_profile_fingerprint()
                .ok_or_else(|| ApiError::bad("Model has no durable request profile"))?,
        )
        .ok_or_else(|| ApiError::bad("Invalid model request profile"))?;
        openide_core::reconcile_model_history_route(
            &mut replay.history,
            replay.history_model_route.as_ref(),
            &route,
        );
        let runner = AgentRunner::new(model.clone(), Arc::new(tools))
            .with_permission_policy(policy)
            .with_system_instructions(instructions)
            .with_native_runtime_identity(&self.config.provider.id, &self.config.provider.endpoint)
            .with_event_journal(self.sessions.clone())
            .with_native_steering(self.sessions.clone())
            .with_plan_snapshot(replay.latest_plan)
            .with_limits(RunLimits {
                max_model_steps: 24,
                max_tool_calls: 64,
                max_tool_output_chars: 16_000,
            });
        if binding.is_none() {
            storage::write_json(
                &binding_path,
                &TurnInputBinding {
                    digest: input_digest,
                    model_id: Some(selected.id.clone()),
                    reasoning_effort: request.reasoning_effort.clone(),
                    approval_policy: Some(approval_policy),
                },
            )
            .map_err(internal)?;
        }
        let admission = self
            .sessions
            .admit_native_turn_with_images(
                session_id,
                &request.request_id,
                &effective_prompt,
                &selected.id,
                request.mode,
                &prepared.images,
            )
            .map_err(internal)?;
        let handle = runner.start_with_lease(
            AgentRunRequest {
                session_id: Some(session_id.into()),
                sequence_start: admission.sequence,
                workspace: record.workspace,
                history: replay.history,
                prompt: effective_prompt,
                mode: request.mode,
            },
            lease,
        );
        runtime.active.insert(
            session_id.into(),
            Active {
                turn_id: request.request_id.clone(),
                handle,
            },
        );
        Ok(json!({"turn_id":request.request_id,"status":"running"}))
    }

    pub fn steer(&self, session_id: &str, request: SteerTurn) -> ApiResult<Value> {
        if let Some(desktop) = self.desktop_for_session(session_id)? {
            return desktop.steer(session_id, request);
        }
        id(&request.request_id)?;
        id(&request.turn_id)?;
        if request.prompt.trim().is_empty() || request.prompt.len() > 32 * 1024 {
            return Err(ApiError::bad("Prompt must contain 1–32768 bytes"));
        }
        let runtime = self.runtime.lock().map_err(internal)?;
        self.load(session_id)?;
        let (path, digest, prior) = self.operation(
            &format!("steer:{session_id}"),
            &request.request_id,
            &request,
        )?;
        if let Some(prior) = prior {
            return Ok(prior);
        }
        let active = runtime
            .active
            .get(session_id)
            .ok_or_else(|| ApiError::conflict("No active turn to steer"))?;
        if active.turn_id != request.turn_id {
            return Err(ApiError::stale_turn());
        }
        storage::write_json(
            &path,
            &OperationRecord {
                digest: digest.clone(),
                response: None,
            },
        )
        .map_err(internal)?;
        self.sessions.enqueue_native_steer(session_id, &request.turn_id, &request.request_id, &request.prompt).map_err(|_| ApiError::uncertain("Steering outcome could not be confirmed; inspect the session before submitting different input"))?;
        let response = json!({"turn_id":request.turn_id,"status":"queued"});
        storage::write_json(
            &path,
            &OperationRecord {
                digest,
                response: Some(response.clone()),
            },
        )
        .map_err(internal)?;
        Ok(response)
    }

    pub fn cancel(&self, session_id: &str, request: CancelTurn) -> ApiResult<Value> {
        if let Some(desktop) = self.desktop_for_session(session_id)? {
            return desktop.cancel(session_id, request);
        }
        id(&request.turn_id)?;
        let runtime = self.runtime.lock().map_err(internal)?;
        self.load(session_id)?;
        let (path, digest, prior) = self.operation(
            &format!("cancel:{session_id}"),
            &request.request_id,
            &request,
        )?;
        if let Some(prior) = prior {
            return Ok(prior);
        }
        let active = runtime
            .active
            .get(session_id)
            .ok_or_else(|| ApiError::conflict("No active turn to cancel"))?;
        if active.turn_id != request.turn_id {
            return Err(ApiError::stale_turn());
        }
        storage::write_json(
            &path,
            &OperationRecord {
                digest: digest.clone(),
                response: None,
            },
        )
        .map_err(internal)?;
        active.handle.cancel();
        let response = json!({"status":"cancelling"});
        storage::write_json(
            &path,
            &OperationRecord {
                digest,
                response: Some(response.clone()),
            },
        )
        .map_err(internal)?;
        Ok(response)
    }

    pub fn permission(
        &self,
        session_id: &str,
        request_id: &str,
        request: ResolvePermission,
    ) -> ApiResult<Value> {
        if let Some(desktop) = self.desktop_for_session(session_id)? {
            return desktop.permission(session_id, request_id, request);
        }
        id(request_id)?;
        let (decision, choice) = match request.decision.as_str() {
            "allow" => (PermissionDecision::Allow, PermissionChoice::Allow),
            "deny" => (PermissionDecision::Deny, PermissionChoice::Deny),
            _ => return Err(ApiError::bad("decision must be allow or deny")),
        };
        let runtime = self.runtime.lock().map_err(internal)?;
        let (_, stored) = self.load(session_id)?;
        let decisions = self
            .sessions
            .load_permission_decisions(session_id)
            .map_err(internal)?;
        if let Some(existing) = decisions.iter().find(|d| d.request_id == request_id) {
            if existing.decision != decision {
                return Err(ApiError::conflict(
                    "Permission already has a different durable decision",
                ));
            }
            return Ok(json!({"status":"recorded"}));
        }
        let active = runtime.active.get(session_id).ok_or_else(|| {
            ApiError::conflict("This permission belongs to an interrupted or finished run")
        })?;
        let pending = projection::pending_permissions(&stored.events)
            .into_iter()
            .find(|p| p.request_id == request_id)
            .ok_or_else(|| {
                ApiError::conflict("Permission is expired, resolved or does not belong to this run")
            })?;
        if decision == PermissionDecision::Allow && pending.arguments_truncated {
            return Err(ApiError::conflict(
                "Cannot authorize arguments too large to inspect on mobile",
            ));
        }
        self.sessions
            .record_permission_decision(session_id, request_id, decision)
            .map_err(internal)?;
        if !active.handle.resolve_permission(request_id, choice) {
            return Err(ApiError::conflict("Decision recorded after the runner settled the request; it was not applied to another action"));
        }
        Ok(json!({"status":"recorded"}))
    }

    pub fn events(&self, session_id: &str, after: u64, limit: usize) -> ApiResult<EventPage> {
        if let Some(desktop) = self.desktop_for_session(session_id)? {
            return desktop.events(session_id, after, limit);
        }
        let (_, stored) = self.load(session_id)?;
        if after > stored.events.last().map_or(0, |e| e.sequence) {
            return Err(ApiError::conflict(
                "Cursor is ahead of the authoritative journal; refresh session history",
            ));
        }
        Ok(projection::project_events(
            &stored.events,
            after,
            limit.clamp(1, 200),
        ))
    }

    pub fn diff(&self, project_id: &str) -> ApiResult<Value> {
        let project = self.project(project_id)?;
        crate::diff::read_diff(&project.path)
    }

    fn poll(&self) {
        let Ok(mut runtime) = self.runtime.lock() else {
            return;
        };
        runtime.active.retain(|_, active| {
            for _ in 0..512 {
                match active.handle.events.try_recv() {
                    Ok(event) => {
                        // The runner already synced the journal. Push contains
                        // only an opaque session ID and a fixed generic message.
                        let kind = match event.kind {
                            AgentEventKind::PermissionRequested { .. } => {
                                Some(crate::push::PushEventKind::ApprovalRequired)
                            }
                            AgentEventKind::TurnCompleted { .. } => {
                                Some(crate::push::PushEventKind::TurnCompleted)
                            }
                            _ => None,
                        };
                        if let (Some(kind), Some((service, handle))) = (kind, &self.push) {
                            let service = service.clone();
                            let event = crate::push::PushEvent {
                                kind,
                                session_id: event.session_id,
                            };
                            handle.spawn(async move {
                                let _ = service.send_event(event).await;
                            });
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => return true,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return false,
                }
            }
            true
        });
    }

    pub fn shutdown(&self) {
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.shutting_down = true;
            for active in runtime.active.values() {
                active.handle.cancel();
            }
        }
    }

    pub fn active_count(&self) -> usize {
        self.runtime.lock().map_or(0, |r| r.active.len())
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn digest(value: &impl Serialize) -> ApiResult<String> {
    let bytes = serde_json::to_vec(value).map_err(internal)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
