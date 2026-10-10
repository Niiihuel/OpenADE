//! Local, turn-scoped controls for a native worker owned by another process.
//! The remote gateway never obtains the worker lease or starts a second runner.

use crate::PermissionChoice;
use openide_core::{NativeSessionLease, PermissionDecision, SessionStore};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_RECORD_BYTES: u64 = 4096;
const HEARTBEAT_VALID_MS: u64 = 4000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRemoteOwner {
    pub version: u8,
    pub session_id: String,
    pub turn_id: String,
    pub owner_id: String,
    pub refreshed_at_ms: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelRecord {
    version: u8,
    session_id: String,
    turn_id: String,
}

/// This guard lives inside the owning run, rather than the UI's handle. Its
/// heartbeat disappears on normal exit and expires after abrupt process exit.
pub struct NativeRemoteControlGuard {
    stop: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<JoinHandle<()>>,
    owner_path: PathBuf,
    owner_id: String,
}

impl NativeRemoteControlGuard {
    pub fn start(
        sessions: Arc<SessionStore>,
        lease: &NativeSessionLease,
        turn_id: &str,
        cancel: CancellationToken,
        resolve_permission: impl Fn(&str, PermissionChoice) -> bool + Send + 'static,
    ) -> io::Result<Self> {
        sessions.native_session_for_lease(lease).map_err(other)?;
        let session_id = lease.session_id().to_owned();
        validate_id(turn_id)?;
        if sessions
            .active_native_turn_id(&session_id)
            .map_err(other)?
            .as_deref()
            != Some(turn_id)
        {
            return Err(other("remote controls must bind the current admitted turn"));
        }
        let directory = directory(&sessions)?;
        let owner_path = directory.join(format!("{session_id}.owner.json"));
        let cancel_path = directory.join(format!("{session_id}.cancel.json"));
        let owner_id = Uuid::new_v4().to_string();
        let mut owner = NativeRemoteOwner {
            version: 1,
            session_id,
            turn_id: turn_id.to_owned(),
            owner_id: owner_id.clone(),
            refreshed_at_ms: now_ms(),
        };
        write_record(&owner_path, &owner)?;
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_stop = stop.clone();
        let worker_path = owner_path.clone();
        let worker = std::thread::Builder::new()
            .name("native-remote-controls".into())
            .spawn(move || {
                let mut ticks = 0u8;
                let mut applied_permissions = HashSet::new();
                loop {
                    let stopped = worker_stop.0.lock().unwrap_or_else(|e| e.into_inner());
                    if *stopped {
                        break;
                    }
                    drop(stopped);
                    let result = (|| -> io::Result<()> {
                        if let Some(record) = read_record::<CancelRecord>(&cancel_path)? {
                            if record.version != 1 || record.session_id != owner.session_id {
                                return Err(other("invalid remote cancellation record"));
                            }
                            if record.turn_id == owner.turn_id {
                                cancel.cancel();
                            }
                        }
                        // IDs are random per permission request. The receiver
                        // accepts only a request currently pending in this run.
                        // Reading does not manufacture a session-wide grant.
                        for decision in sessions
                            .load_permission_decisions(&owner.session_id)
                            .map_err(other)?
                        {
                            if decision.revoked_at_ms.is_some()
                                || applied_permissions.contains(&decision.request_id)
                            {
                                continue;
                            }
                            if resolve_permission(
                                &decision.request_id,
                                match decision.decision {
                                    PermissionDecision::Allow => PermissionChoice::Allow,
                                    PermissionDecision::Deny => PermissionChoice::Deny,
                                },
                            ) {
                                applied_permissions.insert(decision.request_id);
                            }
                        }
                        if ticks == 0 {
                            owner.refreshed_at_ms = now_ms();
                            write_record(&worker_path, &owner)?;
                        }
                        Ok(())
                    })();
                    // Losing the control journal fails closed. It never
                    // converts an unreadable decision into permission.
                    if result.is_err() {
                        cancel.cancel();
                        if read_record::<NativeRemoteOwner>(&worker_path)
                            .ok()
                            .flatten()
                            .is_some_and(|record| record.owner_id == owner.owner_id)
                        {
                            let _ = fs::remove_file(&worker_path);
                        }
                        break;
                    }
                    ticks = (ticks + 1) % 5;
                    let stopped = worker_stop.0.lock().unwrap_or_else(|e| e.into_inner());
                    let (stopped, _) = worker_stop
                        .1
                        .wait_timeout_while(stopped, Duration::from_millis(200), |v| !*v)
                        .unwrap_or_else(|e| e.into_inner());
                    if *stopped {
                        break;
                    }
                }
            })
            .inspect_err(|_| {
                if read_record::<NativeRemoteOwner>(&owner_path)
                    .ok()
                    .flatten()
                    .is_some_and(|record| record.owner_id == owner_id)
                {
                    let _ = fs::remove_file(&owner_path);
                }
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
            owner_path,
            owner_id,
        })
    }
}

impl Drop for NativeRemoteControlGuard {
    fn drop(&mut self) {
        *self.stop.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.stop.1.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if read_record::<NativeRemoteOwner>(&self.owner_path)
            .ok()
            .flatten()
            .is_some_and(|owner| owner.owner_id == self.owner_id)
        {
            let _ = fs::remove_file(&self.owner_path);
        }
    }
}

/// The owner is useful only with an independently observed busy native lease.
/// A recently crashed process may leave a fresh heartbeat, but cannot retain
/// that OS lease; the gateway must check both before offering live controls.
pub fn native_remote_owner(
    sessions: &SessionStore,
    session_id: &str,
    turn_id: &str,
) -> io::Result<Option<NativeRemoteOwner>> {
    validate_id(session_id)?;
    validate_id(turn_id)?;
    let Some(root) = existing_directory(sessions)? else {
        return Ok(None);
    };
    let Some(owner) =
        read_record::<NativeRemoteOwner>(&root.join(format!("{session_id}.owner.json")))?
    else {
        return Ok(None);
    };
    let now = now_ms();
    Ok((owner.version == 1
        && owner.session_id == session_id
        && owner.turn_id == turn_id
        && owner.refreshed_at_ms <= now
        && now.saturating_sub(owner.refreshed_at_ms) <= HEARTBEAT_VALID_MS)
        .then_some(owner))
}

/// A stale request can only cancel its exact turn. Even if completion races
/// this write, a future admission has a different ID and ignores the record.
pub fn request_native_remote_cancel(
    sessions: &SessionStore,
    session_id: &str,
    turn_id: &str,
) -> io::Result<()> {
    validate_id(session_id)?;
    validate_id(turn_id)?;
    if sessions
        .active_native_turn_id(session_id)
        .map_err(other)?
        .as_deref()
        != Some(turn_id)
    {
        return Err(other("the requested turn is no longer active"));
    }
    write_record(
        &directory(sessions)?.join(format!("{session_id}.cancel.json")),
        &CancelRecord {
            version: 1,
            session_id: session_id.into(),
            turn_id: turn_id.into(),
        },
    )
}

fn validate_id(id: &str) -> io::Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(other("invalid native control identifier"));
    }
    Ok(())
}

fn existing_directory(sessions: &SessionStore) -> io::Result<Option<PathBuf>> {
    let root = sessions.storage_root().join(".remote-controls");
    match fs::symlink_metadata(&root) {
        Ok(meta) if meta.file_type().is_dir() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o077 != 0 {
                    return Err(other("control directory is not private"));
                }
            }
            Ok(Some(root))
        }
        Ok(_) => Err(other("control directory is not a real directory")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn directory(sessions: &SessionStore) -> io::Result<PathBuf> {
    if let Some(root) = existing_directory(sessions)? {
        return Ok(root);
    }
    let root = sessions.storage_root().join(".remote-controls");
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(&root) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    existing_directory(sessions)?.ok_or_else(|| other("control directory disappeared"))
}

fn read_record<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(other("control record cannot be a symlink"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
        return Err(other("invalid control record"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(other("control record is not private"));
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(other("control record exceeds limit"));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(other)
}

fn write_record(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(other)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(other("control record exceeds limit"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| other("missing control parent"))?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::{AgentEvent, AgentEventKind, TurnMode};

    #[test]
    fn cancellation_is_scoped_and_owner_dies_with_guard() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(temp.path()).unwrap());
        let session = sessions.create(temp.path(), "native").unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        sessions
            .admit_native_turn(&session.id, "first", "test", "model", TurnMode::Plan)
            .unwrap();
        let cancel = CancellationToken::new();
        let guard = NativeRemoteControlGuard::start(
            sessions.clone(),
            &lease,
            "first",
            cancel.clone(),
            |_, _| false,
        )
        .unwrap();
        assert!(native_remote_owner(&sessions, &session.id, "first")
            .unwrap()
            .is_some());
        assert!(request_native_remote_cancel(&sessions, &session.id, "wrong").is_err());
        request_native_remote_cancel(&sessions, &session.id, "first").unwrap();
        for _ in 0..100 {
            if cancel.is_cancelled() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(cancel.is_cancelled());
        drop(guard);
        assert!(native_remote_owner(&sessions, &session.id, "first")
            .unwrap()
            .is_none());
        sessions
            .append_event(
                &session.id,
                &AgentEvent {
                    session_id: session.id.clone(),
                    sequence: 2,
                    timestamp_ms: now_ms(),
                    kind: AgentEventKind::TurnCancelled,
                },
            )
            .unwrap();
        sessions
            .admit_native_turn(&session.id, "second", "test again", "model", TurnMode::Plan)
            .unwrap();
        let next_cancel = CancellationToken::new();
        let _guard = NativeRemoteControlGuard::start(
            sessions.clone(),
            &lease,
            "second",
            next_cancel.clone(),
            |_, _| false,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(250));
        assert!(
            !next_cancel.is_cancelled(),
            "an old cancellation must not affect the new turn"
        );
    }

    #[test]
    fn expired_or_foreign_owners_are_not_advertised() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = SessionStore::open(temp.path()).unwrap();
        let root = directory(&sessions).unwrap();
        let mut owner = NativeRemoteOwner {
            version: 1,
            session_id: "session".into(),
            turn_id: "turn".into(),
            owner_id: "owner".into(),
            refreshed_at_ms: 1,
        };
        write_record(&root.join("session.owner.json"), &owner).unwrap();
        assert!(native_remote_owner(&sessions, "session", "turn")
            .unwrap()
            .is_none());
        owner.refreshed_at_ms = now_ms();
        write_record(&root.join("session.owner.json"), &owner).unwrap();
        assert!(native_remote_owner(&sessions, "session", "other-turn")
            .unwrap()
            .is_none());
        assert!(native_remote_owner(&sessions, "../escape", "turn").is_err());
    }

    #[test]
    fn permission_from_another_store_is_observed_and_heartbeat_stops_on_drop() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(temp.path()).unwrap());
        let other_store = SessionStore::open(temp.path()).unwrap();
        let session = sessions.create(temp.path(), "native").unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        sessions
            .admit_native_turn(&session.id, "turn", "test", "model", TurnMode::Plan)
            .unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let guard = NativeRemoteControlGuard::start(
            sessions.clone(),
            &lease,
            "turn",
            CancellationToken::new(),
            move |id, choice| {
                let _ = sender.send((id.to_owned(), choice));
                true
            },
        )
        .unwrap();
        other_store
            .record_permission_decision(
                &session.id,
                "permission-from-remote",
                PermissionDecision::Allow,
            )
            .unwrap();
        let (id, choice) = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(id, "permission-from-remote");
        assert!(matches!(choice, PermissionChoice::Allow));
        let owner = native_remote_owner(&other_store, &session.id, "turn")
            .unwrap()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while native_remote_owner(&other_store, &session.id, "turn")
            .unwrap()
            .unwrap()
            .refreshed_at_ms
            <= owner.refreshed_at_ms
        {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            receiver.try_recv().is_err(),
            "accepted decisions are delivered once"
        );
        drop(guard);
        std::thread::sleep(Duration::from_millis(250));
        assert!(native_remote_owner(&other_store, &session.id, "turn")
            .unwrap()
            .is_none());
    }

    #[test]
    fn permission_delivery_retries_until_the_pending_request_accepts_it() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let temp = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(temp.path()).unwrap());
        let session = sessions.create(temp.path(), "native").unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        sessions
            .admit_native_turn(&session.id, "turn", "test", "model", TurnMode::Plan)
            .unwrap();
        sessions
            .record_permission_decision(&session.id, "request", PermissionDecision::Deny)
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let guard = NativeRemoteControlGuard::start(
            sessions,
            &lease,
            "turn",
            CancellationToken::new(),
            move |_, _| observed.fetch_add(1, Ordering::SeqCst) >= 1,
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while calls.load(Ordering::SeqCst) < 2 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(450));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        drop(guard);
    }

    #[test]
    fn record_limits_and_foreign_store_lease_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(temp.path()).unwrap());
        let unrelated = tempfile::tempdir().unwrap();
        let other_store = SessionStore::open(unrelated.path()).unwrap();
        let foreign_session = other_store.create(unrelated.path(), "native").unwrap();
        let foreign_lease = other_store
            .try_acquire_native_lease(&foreign_session.id)
            .unwrap();
        assert!(NativeRemoteControlGuard::start(
            sessions.clone(),
            &foreign_lease,
            "turn",
            CancellationToken::new(),
            |_, _| false
        )
        .is_err());
        let root = directory(&sessions).unwrap();
        let path = root.join("oversized.json");
        assert!(write_record(&path, &"x".repeat(MAX_RECORD_BYTES as usize)).is_err());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).unwrap();
        file.write_all(&vec![b'x'; MAX_RECORD_BYTES as usize + 1])
            .unwrap();
        drop(file);
        assert!(read_record::<NativeRemoteOwner>(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_control_paths_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = SessionStore::open(temp.path()).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), temp.path().join(".remote-controls")).unwrap();
        assert!(directory(&sessions).is_err());
        fs::remove_file(temp.path().join(".remote-controls")).unwrap();
        let root = directory(&sessions).unwrap();
        std::os::unix::fs::symlink("/dev/zero", root.join("session.owner.json")).unwrap();
        assert!(native_remote_owner(&sessions, "session", "turn").is_err());
    }
}
