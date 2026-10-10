//! Read-only projection of authorized native desktop sessions, with narrowly
//! scoped controls for a worker that explicitly advertises a live owner.
use crate::{
    config::{Config, Project},
    host::{ApiError, ApiResult, CancelTurn, ResolvePermission, SessionView, SteerTurn},
    projection::{self, EventPage},
    storage,
};
use openide_core::{
    AgentEventKind, PermissionDecision, Session, SessionStore, StoreError, StoredSession,
};
use openide_harness::{native_remote_owner, request_native_remote_cancel};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

pub struct DesktopSessions {
    sessions: SessionStore,
    projects: Vec<Project>,
    operations: PathBuf,
    mutation: Mutex<()>,
    skipped_records: AtomicUsize,
}
#[derive(Serialize, Deserialize)]
struct Operation {
    digest: String,
    response: Option<Value>,
}
fn internal(_: impl std::fmt::Display) -> ApiError {
    ApiError::unavailable()
}
fn valid_id(id: &str) -> ApiResult<()> {
    storage::validate_id(id).map_err(|_| ApiError::bad("Invalid identifier"))
}

impl DesktopSessions {
    pub fn open(config: &Config, gateway_root: &Path) -> anyhow::Result<Option<Self>> {
        let Some(root) = &config.provider.desktop_data_root else {
            return Ok(None);
        };
        let path = root.join("sessions");
        if !path.exists() {
            return Ok(None);
        }
        if !root.is_absolute()
            || fs::canonicalize(root)? != *root
            || fs::canonicalize(&path)? != path
            || !fs::symlink_metadata(&path)?.is_dir()
        {
            anyhow::bail!("desktop session root must be an existing canonical directory");
        }
        let operations = gateway_root.join("desktop-operations");
        storage::prepare_directory(&operations)?;
        Ok(Some(Self {
            sessions: SessionStore::open(path)?,
            projects: config.projects.clone(),
            operations,
            mutation: Mutex::new(()),
            skipped_records: AtomicUsize::new(0),
        }))
    }

    fn authorized_project(&self, session: &Session) -> ApiResult<&Project> {
        if session.backend != "native" {
            return Err(ApiError::not_found());
        }
        let project = self
            .projects
            .iter()
            .find(|p| p.path == session.workspace)
            .ok_or_else(ApiError::not_found)?;
        if fs::canonicalize(&project.path).map_err(internal)? != project.path {
            return Err(ApiError::not_found());
        }
        Ok(project)
    }

    fn load(&self, id: &str) -> ApiResult<(String, StoredSession)> {
        valid_id(id)?;
        // Authorize bounded metadata before reading any conversation content.
        let path = self
            .sessions
            .storage_root()
            .join(format!("{id}.session.json"));
        let bytes = read_regular(&path, openide_core::MAX_SESSION_METADATA_BYTES)?
            .ok_or_else(ApiError::not_found)?;
        let metadata: Session = serde_json::from_slice(&bytes).map_err(internal)?;
        if metadata.id != id {
            return Err(ApiError::unavailable());
        }
        let project_id = self.authorized_project(&metadata)?.id.clone();
        // The desktop journal is observed strictly: never repair/truncate it.
        // Bound aggregate memory too, independently of core's per-record cap.
        let journal = self.sessions.storage_root().join(format!("{id}.jsonl"));
        validate_regular(&journal, 64 * 1024 * 1024)?;
        let stored = self
            .sessions
            .load_strict(id)
            .map_err(internal)?
            .ok_or_else(ApiError::not_found)?;
        if self.authorized_project(&stored.session)?.id != project_id {
            return Err(ApiError::not_found());
        }
        Ok((project_id, stored))
    }

    fn active_turn(stored: &StoredSession) -> Option<String> {
        let mut active = None;
        for event in &stored.events {
            match &event.kind {
                AgentEventKind::TurnAdmitted { turn_id, .. }
                | AgentEventKind::TurnAdmittedWithImages { turn_id, .. } => {
                    active = Some(turn_id.clone())
                }
                AgentEventKind::TurnCompleted { .. }
                | AgentEventKind::TurnFailed { .. }
                | AgentEventKind::TurnCancelled => active = None,
                _ => {}
            }
        }
        active
    }

    fn controllable(&self, stored: &StoredSession, turn_id: &str) -> ApiResult<bool> {
        if stored.session.native_automation.is_some()
            || stored.session.subagent.is_some()
            || native_remote_owner(&self.sessions, &stored.session.id, turn_id)
                .map_err(internal)?
                .is_none()
        {
            return Ok(false);
        }
        // Fresh heartbeat alone is insufficient after a crash. The OS lease
        // must still be held by the owning native worker.
        self.lease_busy(&stored.session.id)
    }

    fn lease_busy(&self, session_id: &str) -> ApiResult<bool> {
        match self.sessions.try_acquire_native_lease(session_id) {
            Ok(lease) => {
                drop(lease);
                Ok(false)
            }
            Err(StoreError::NativeSessionBusy(_)) => Ok(true),
            Err(error) => Err(internal(error)),
        }
    }

    fn view(&self, project_id: String, stored: &StoredSession) -> ApiResult<SessionView> {
        let active = Self::active_turn(stored);
        let can_control = active
            .as_deref()
            .map(|turn| self.controllable(stored, turn))
            .transpose()?
            .unwrap_or(false);
        let pending = if can_control {
            projection::pending_permissions(&stored.events)
        } else {
            Vec::new()
        };
        let status = if can_control {
            if pending.is_empty() {
                "running"
            } else {
                "waiting_permission"
            }
        } else if active.is_some() && self.lease_busy(&stored.session.id)? {
            // Older/resumed/automation workers can be alive without opting in
            // to remote controls. Show their activity without enabling actions.
            "running"
        } else if active.is_some() {
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
            project_id,
            title: projection::clip(
                stored
                    .session
                    .title
                    .as_deref()
                    .unwrap_or("Tarea del escritorio"),
                256,
            ),
            archived: stored.session.archived,
            favorite: stored.session.pinned,
            status,
            updated_at_ms: stored
                .events
                .last()
                .map_or(stored.session.updated_at_ms, |e| e.timestamp_ms)
                .max(stored.session.updated_at_ms),
            active_turn_id: active,
            pending_permissions: pending,
            origin: "desktop",
            can_start_turn: false,
            can_control_active: can_control,
        })
    }

    pub fn list(&self, project_id: Option<&str>) -> ApiResult<Vec<SessionView>> {
        if project_id.is_some_and(|id| !self.projects.iter().any(|p| p.id == id)) {
            return Err(ApiError::not_found());
        }
        let mut result = Vec::new();
        let mut skipped = 0;
        for entry in fs::read_dir(self.sessions.storage_root()).map_err(internal)? {
            let entry = entry.map_err(internal)?;
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|s| s.strip_suffix(".session.json")) else {
                continue;
            };
            let (project, stored) = match self.load(id) {
                Ok(value) => value,
                Err(error) if error.status == axum::http::StatusCode::NOT_FOUND => continue,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            if project_id.is_none_or(|id| id == project) {
                match self.view(project, &stored) {
                    Ok(view) => result.push(view),
                    Err(_) => skipped += 1,
                }
            }
        }
        // An unrelated damaged journal must not hide healthy sessions. Keep
        // individual GET fail-closed, and emit only an aggregate diagnostic.
        if self.skipped_records.swap(skipped, Ordering::Relaxed) != skipped {
            eprintln!("Desktop session projection skipped {skipped} unreadable records; individual requests fail closed");
        }
        result.sort_by(|a, b| (b.updated_at_ms, &b.id).cmp(&(a.updated_at_ms, &a.id)));
        Ok(result)
    }
    /// The most recent listing omitted unreadable records. API callers should
    /// expose this as a generic partial-result flag, without journal metadata.
    pub fn has_unreadable_records(&self) -> bool {
        self.skipped_records.load(Ordering::Relaxed) != 0
    }

    pub fn session(&self, id: &str) -> ApiResult<SessionView> {
        let (project, stored) = self.load(id)?;
        self.view(project, &stored)
    }
    pub fn events(&self, id: &str, after: u64, limit: usize) -> ApiResult<EventPage> {
        let (_, stored) = self.load(id)?;
        if after > stored.events.last().map_or(0, |e| e.sequence) {
            return Err(ApiError::conflict("Cursor is ahead of the journal"));
        }
        Ok(projection::project_events(
            &stored.events,
            after,
            limit.clamp(1, 200),
        ))
    }
    fn require_owner(&self, stored: &StoredSession, turn: &str) -> ApiResult<()> {
        if Self::active_turn(stored).as_deref() != Some(turn) {
            return Err(ApiError::stale_turn());
        }
        if !self.controllable(stored, turn)? {
            return Err(ApiError::conflict(
                "The desktop worker has no live remote control owner",
            ));
        }
        Ok(())
    }
    fn operation(
        &self,
        scope: &str,
        request_id: &str,
        request: &impl Serialize,
    ) -> ApiResult<(PathBuf, String, Option<Value>)> {
        valid_id(request_id)?;
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(request).map_err(internal)?)
        );
        let key = format!("{:x}", Sha256::digest(format!("{scope}:{request_id}")));
        let path = self.operations.join(format!("{key}.json"));
        if path.exists() {
            let prior: Operation = storage::read_json(&path).map_err(internal)?;
            if prior.digest != digest {
                return Err(ApiError::idempotency_conflict());
            }
            let response = prior.response.ok_or_else(|| ApiError::uncertain("Desktop control outcome needs inspection before submitting a different request"))?;
            return Ok((path, digest, Some(response)));
        }
        Ok((path, digest, None))
    }
    pub fn cancel(&self, id: &str, request: CancelTurn) -> ApiResult<Value> {
        valid_id(&request.turn_id)?;
        let _lock = self.mutation.lock().map_err(internal)?;
        let (_, stored) = self.load(id)?;
        let (path, digest, prior) =
            self.operation(&format!("cancel:{id}"), &request.request_id, &request)?;
        if let Some(value) = prior {
            return Ok(value);
        }
        self.require_owner(&stored, &request.turn_id)?;
        storage::write_json(
            &path,
            &Operation {
                digest: digest.clone(),
                response: None,
            },
        )
        .map_err(internal)?;
        request_native_remote_cancel(&self.sessions, id, &request.turn_id)
            .map_err(|_| ApiError::uncertain("Desktop cancellation could not be confirmed"))?;
        let response = json!({"status":"cancelling"});
        storage::write_json(
            &path,
            &Operation {
                digest,
                response: Some(response.clone()),
            },
        )
        .map_err(internal)?;
        Ok(response)
    }
    pub fn steer(&self, id: &str, request: SteerTurn) -> ApiResult<Value> {
        valid_id(&request.turn_id)?;
        if request.prompt.trim().is_empty() || request.prompt.len() > 32 * 1024 {
            return Err(ApiError::bad("Prompt must contain 1–32768 bytes"));
        }
        let _lock = self.mutation.lock().map_err(internal)?;
        let (_, stored) = self.load(id)?;
        let (path, digest, prior) =
            self.operation(&format!("steer:{id}"), &request.request_id, &request)?;
        if let Some(value) = prior {
            return Ok(value);
        }
        self.require_owner(&stored, &request.turn_id)?;
        storage::write_json(
            &path,
            &Operation {
                digest: digest.clone(),
                response: None,
            },
        )
        .map_err(internal)?;
        self.sessions
            .enqueue_native_steer(id, &request.turn_id, &request.request_id, &request.prompt)
            .map_err(|_| ApiError::uncertain("Desktop steering could not be confirmed"))?;
        let response = json!({"status":"queued","turn_id":request.turn_id});
        storage::write_json(
            &path,
            &Operation {
                digest,
                response: Some(response.clone()),
            },
        )
        .map_err(internal)?;
        Ok(response)
    }
    pub fn permission(
        &self,
        id: &str,
        request_id: &str,
        request: ResolvePermission,
    ) -> ApiResult<Value> {
        valid_id(request_id)?;
        let decision = match request.decision.as_str() {
            "allow" => PermissionDecision::Allow,
            "deny" => PermissionDecision::Deny,
            _ => return Err(ApiError::bad("decision must be allow or deny")),
        };
        let _lock = self.mutation.lock().map_err(internal)?;
        let (_, stored) = self.load(id)?;
        if let Some(existing) = self
            .sessions
            .load_permission_decisions(id)
            .map_err(internal)?
            .iter()
            .find(|d| d.request_id == request_id)
        {
            if existing.decision != decision || existing.revoked_at_ms.is_some() {
                return Err(ApiError::conflict(
                    "Permission already has a different or revoked decision",
                ));
            }
            return Ok(json!({"status":"recorded"}));
        }
        let turn = Self::active_turn(&stored).ok_or_else(ApiError::stale_turn)?;
        self.require_owner(&stored, &turn)?;
        let pending = projection::pending_permissions(&stored.events)
            .into_iter()
            .find(|p| p.request_id == request_id)
            .ok_or_else(|| ApiError::conflict("Permission is no longer pending in this turn"))?;
        if decision == PermissionDecision::Allow && pending.arguments_truncated {
            return Err(ApiError::conflict(
                "Cannot approve arguments too large to inspect on mobile",
            ));
        }
        self.sessions
            .record_permission_decision(id, request_id, decision)
            .map_err(internal)?;
        // The owning worker consumes this durable decision. A successful write
        // means recorded, not proof that the tool already ran.
        Ok(json!({"status":"recorded"}))
    }
}

fn validate_regular(path: &Path, cap: u64) -> ApiResult<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= cap => Ok(true),
        Ok(_) => Err(ApiError::unavailable()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(internal(error)),
    }
}
fn read_regular(path: &Path, cap: u64) -> ApiResult<Option<Vec<u8>>> {
    if !validate_regular(path, cap)? {
        return Ok(None);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(internal)?;
    let meta = file.metadata().map_err(internal)?;
    if !meta.is_file() || meta.len() > cap {
        return Err(ApiError::unavailable());
    }
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(internal)?;
    if bytes.len() as u64 > cap {
        return Err(ApiError::unavailable());
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::{AgentEvent, ToolCall, TurnMode};
    use openide_harness::NativeRemoteControlGuard;
    use std::{sync::Arc, time::Duration};
    use tokio_util::sync::CancellationToken;

    fn bridge(root: &Path, workspace: &Path) -> DesktopSessions {
        let operations = root.join("operations");
        storage::prepare_directory(&operations).unwrap();
        DesktopSessions {
            sessions: SessionStore::open(root.join("desktop-sessions")).unwrap(),
            projects: vec![Project {
                id: "authorized".into(),
                name: "Project".into(),
                path: workspace.to_path_buf(),
            }],
            operations,
            mutation: Mutex::new(()),
            skipped_records: AtomicUsize::new(0),
        }
    }
    #[test]
    fn only_native_allowlisted_sessions_are_projected_without_internal_metadata() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let forbidden = tempfile::tempdir().unwrap();
        let bridge = bridge(root.path(), workspace.path());
        let native = bridge.sessions.create(workspace.path(), "native").unwrap();
        let other = bridge.sessions.create(forbidden.path(), "native").unwrap();
        let legacy = bridge.sessions.create(workspace.path(), "codex").unwrap();
        let list = bridge.list(None).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, native.id);
        assert_eq!(list[0].origin, "desktop");
        assert!(!list[0].can_start_turn && !list[0].can_control_active);
        let json = serde_json::to_string(&list[0]).unwrap();
        assert!(!json.contains(workspace.path().to_str().unwrap()));
        assert!(bridge.session(&other.id).is_err());
        assert!(bridge.events(&legacy.id, 0, 10).is_err());
        assert!(bridge.session("../escape").is_err());
    }
    #[test]
    fn live_owner_controls_are_turn_scoped_idempotent_and_use_durable_permissions() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let bridge = bridge(root.path(), workspace.path());
        let owner_store = Arc::new(SessionStore::open(bridge.sessions.storage_root()).unwrap());
        let session = owner_store.create(workspace.path(), "native").unwrap();
        let lease = owner_store.try_acquire_native_lease(&session.id).unwrap();
        owner_store
            .admit_native_turn(&session.id, "turn", "Run", "model", TurnMode::Build)
            .unwrap();
        owner_store
            .append_event(
                &session.id,
                &AgentEvent {
                    session_id: session.id.clone(),
                    sequence: 2,
                    timestamp_ms: 2,
                    kind: AgentEventKind::PermissionRequested {
                        request_id: "permission".into(),
                        tool_call: ToolCall {
                            id: "call".into(),
                            name: "command".into(),
                            arguments: json!({"command":"echo ok"}),
                        },
                    },
                },
            )
            .unwrap();
        assert!(
            !bridge.session(&session.id).unwrap().can_control_active,
            "busy lease without explicit owner is insufficient"
        );
        assert_eq!(
            bridge.session(&session.id).unwrap().status,
            "running",
            "a worker without bridge is still alive"
        );
        let token = CancellationToken::new();
        let (sender, receiver) = std::sync::mpsc::channel();
        let guard = NativeRemoteControlGuard::start(
            owner_store.clone(),
            &lease,
            "turn",
            token.clone(),
            move |id, choice| {
                sender.send((id.to_owned(), choice)).unwrap();
                true
            },
        )
        .unwrap();
        let view = bridge.session(&session.id).unwrap();
        assert!(view.can_control_active);
        assert_eq!(view.status, "waiting_permission");
        bridge
            .permission(
                &session.id,
                "permission",
                ResolvePermission {
                    decision: "allow".into(),
                },
            )
            .unwrap();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap().0,
            "permission"
        );
        assert!(bridge
            .permission(
                &session.id,
                "permission",
                ResolvePermission {
                    decision: "deny".into()
                }
            )
            .is_err());
        let steer = || SteerTurn {
            request_id: "input".into(),
            turn_id: "turn".into(),
            prompt: "Second instruction".into(),
        };
        bridge.steer(&session.id, steer()).unwrap();
        bridge.steer(&session.id, steer()).unwrap();
        assert_eq!(
            owner_store
                .pending_native_steers(&session.id, "turn")
                .unwrap()
                .len(),
            1
        );
        assert!(bridge
            .steer(
                &session.id,
                SteerTurn {
                    prompt: "Changed payload".into(),
                    ..steer()
                }
            )
            .is_err());
        assert!(bridge
            .cancel(
                &session.id,
                CancelTurn {
                    request_id: "bad".into(),
                    turn_id: "old".into()
                }
            )
            .is_err());
        let request = || CancelTurn {
            request_id: "cancel".into(),
            turn_id: "turn".into(),
        };
        bridge.cancel(&session.id, request()).unwrap();
        bridge.cancel(&session.id, request()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !token.is_cancelled() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(lease);
        assert!(
            !bridge.session(&session.id).unwrap().can_control_active,
            "fresh heartbeat without OS lease is insufficient"
        );
        assert_eq!(bridge.session(&session.id).unwrap().status, "interrupted");
        drop(guard);
    }
    #[test]
    fn observing_a_partial_journal_never_repairs_it() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let bridge = bridge(root.path(), workspace.path());
        let session = bridge.sessions.create(workspace.path(), "native").unwrap();
        let path = bridge
            .sessions
            .storage_root()
            .join(format!("{}.jsonl", session.id));
        fs::write(&path, b"{\"partial\":").unwrap();
        assert!(bridge.events(&session.id, 0, 10).is_err());
        assert_eq!(fs::read(path).unwrap(), b"{\"partial\":");
    }
    #[test]
    fn corrupt_and_oversized_records_do_not_hide_healthy_sessions() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let bridge = bridge(root.path(), workspace.path());
        let healthy = bridge.sessions.create(workspace.path(), "native").unwrap();
        let corrupt = bridge.sessions.create(workspace.path(), "native").unwrap();
        let oversized = bridge.sessions.create(workspace.path(), "native").unwrap();
        fs::write(
            bridge
                .sessions
                .storage_root()
                .join(format!("{}.jsonl", corrupt.id)),
            b"{partial",
        )
        .unwrap();
        let oversized_file = fs::File::create(
            bridge
                .sessions
                .storage_root()
                .join(format!("{}.jsonl", oversized.id)),
        )
        .unwrap();
        oversized_file.set_len(64 * 1024 * 1024 + 1).unwrap();
        fs::write(
            bridge.sessions.storage_root().join("broken.session.json"),
            b"not json",
        )
        .unwrap();
        let list = bridge.list(None).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, healthy.id);
        assert_eq!(bridge.skipped_records.load(Ordering::Relaxed), 3);
        assert!(bridge.has_unreadable_records());
        assert!(bridge.events(&corrupt.id, 0, 10).is_err());
        assert!(bridge.events(&oversized.id, 0, 10).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_metadata_and_journals_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let bridge = bridge(root.path(), workspace.path());
        let session = bridge.sessions.create(workspace.path(), "native").unwrap();
        let journal = bridge
            .sessions
            .storage_root()
            .join(format!("{}.jsonl", session.id));
        if journal.exists() {
            fs::remove_file(&journal).unwrap();
        }
        std::os::unix::fs::symlink("/dev/zero", &journal).unwrap();
        assert!(bridge.session(&session.id).is_err());
        fs::remove_file(journal).unwrap();
        let meta = bridge
            .sessions
            .storage_root()
            .join(format!("{}.session.json", session.id));
        fs::remove_file(&meta).unwrap();
        std::os::unix::fs::symlink("/dev/zero", &meta).unwrap();
        assert!(bridge.session(&session.id).is_err());
    }
}
