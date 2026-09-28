use crate::{AgentEvent, AgentEventKind};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("invalid session id")]
    InvalidSessionId,
    #[error("session store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("session event encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session store lock is poisoned")]
    PoisonedLock,
    #[error("session event belongs to a different session")]
    SessionMismatch,
    #[error("unknown session: {0}")]
    UnknownSession(String),
    #[error("session title cannot be blank")]
    InvalidTitle,
    #[error("session search query is too long")]
    SearchQueryTooLong,
    #[error("session search query could not be compiled: {0}")]
    SearchPattern(#[from] regex::Error),
    #[error("unsupported composer draft format version: {0}")]
    UnsupportedDraftVersion(u32),
    #[error("unsupported permission journal format version: {0}")]
    UnsupportedPermissionJournalVersion(u32),
    #[error("invalid permission request id")]
    InvalidPermissionRequestId,
    #[error("permission decision conflicts with an existing decision for request {0}")]
    ConflictingPermissionDecision(String),
    #[error("permission journal contains a duplicate request id: {0}")]
    DuplicatePermissionRequestId(String),
}

pub trait EventStore: Send + Sync {
    fn append(&self, event: &AgentEvent) -> Result<(), StoreError>;
    fn load(&self, session_id: &str) -> Result<Vec<AgentEvent>, StoreError>;
}

#[derive(Default)]
pub struct MemorySessionStore {
    events: Mutex<HashMap<String, Vec<AgentEvent>>>,
}

impl MemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl EventStore for MemorySessionStore {
    fn append(&self, event: &AgentEvent) -> Result<(), StoreError> {
        self.events
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?
            .entry(event.session_id.clone())
            .or_default()
            .push(event.clone());
        Ok(())
    }

    fn load(&self, session_id: &str) -> Result<Vec<AgentEvent>, StoreError> {
        Ok(self
            .events
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?
            .get(session_id)
            .cloned()
            .unwrap_or_default())
    }
}

/// One JSON record per line. The lock protects simultaneous appends in this process.
pub struct JsonlSessionStore {
    root: PathBuf,
    write_lock: Mutex<()>,
}

impl JsonlSessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            write_lock: Mutex::new(()),
        })
    }

    fn path(&self, session_id: &str) -> Result<PathBuf, StoreError> {
        validate_session_id(session_id)?;
        Ok(self.root.join(format!("{session_id}.jsonl")))
    }

    /// Return IDs with the most recently modified event log first.
    pub fn list_sessions(&self) -> Result<Vec<String>, StoreError> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if validate_session_id(id).is_err() {
                continue;
            }
            let modified = entry.metadata()?.modified().unwrap_or(UNIX_EPOCH);
            files.push((id.to_owned(), modified));
        }
        files.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        Ok(files.into_iter().map(|(id, _)| id).collect())
    }
}

fn validate_session_id(session_id: &str) -> Result<(), StoreError> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(StoreError::InvalidSessionId);
    }
    Ok(())
}

fn validate_permission_request_id(request_id: &str) -> Result<(), StoreError> {
    if request_id.trim().is_empty()
        || request_id.len() > 256
        || request_id.chars().any(char::is_control)
    {
        return Err(StoreError::InvalidPermissionRequestId);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub workspace: PathBuf,
    pub backend: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread_at_ms: Option<u64>,
    #[serde(default)]
    pub last_unread_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredSession {
    pub session: Session,
    pub events: Vec<AgentEvent>,
}

/// A session that could not be read while listing the rest of the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDiagnostic {
    pub session_id: String,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionListing {
    pub sessions: Vec<StoredSession>,
    pub diagnostics: Vec<SessionDiagnostic>,
}

/// The same mutually exclusive list scopes used by the sessions dock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionListKind {
    /// Active tasks except those in the global pinned section.
    #[default]
    ActiveUnpinned,
    Pinned,
    Archived,
    All,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionSort {
    #[default]
    Updated,
    Created,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionQuery {
    /// Empty means every workspace. Paths are compared exactly as persisted.
    pub workspaces: Vec<PathBuf>,
    pub kind: SessionListKind,
    pub sort: SessionSort,
    /// A literal, case-insensitive substring in the title or visible chat text.
    pub search: Option<String>,
    pub offset: usize,
    pub limit: usize,
}

impl Default for SessionQuery {
    fn default() -> Self {
        Self {
            workspaces: Vec::new(),
            kind: SessionListKind::default(),
            sort: SessionSort::default(),
            search: None,
            offset: 0,
            limit: 20,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionQueryItem {
    pub session: Session,
    pub title: String,
    pub search_snippets: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionQueryResult {
    pub items: Vec<SessionQueryItem>,
    /// Number of readable matches before pagination.
    pub total: usize,
    pub has_more: bool,
    pub diagnostics: Vec<SessionDiagnostic>,
}

/// The user's choice, recorded before asking the runner to resolve a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedPermissionDecision {
    pub session_id: String,
    pub request_id: String,
    pub decision: PermissionDecision,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct PermissionJournal {
    version: u32,
    entries: Vec<RecordedPermissionDecision>,
}

impl Default for PermissionJournal {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct DraftFile {
    version: u32,
    entries: Vec<DraftEntry>,
}

impl Default for DraftFile {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct DraftEntry {
    workspace: PathBuf,
    session_id: Option<String>,
    text: String,
    updated_at_ms: u64,
}

/// Session metadata plus a per-session append-only event log.
pub struct SessionStore {
    events: JsonlSessionStore,
    operation_lock: Mutex<()>,
}

impl SessionStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Ok(Self {
            events: JsonlSessionStore::new(root)?,
            operation_lock: Mutex::new(()),
        })
    }

    fn metadata_path(&self, id: &str) -> Result<PathBuf, StoreError> {
        validate_session_id(id)?;
        Ok(self.events.root.join(format!("{id}.session.json")))
    }

    fn draft_path(&self) -> PathBuf {
        self.events.root.join("composer-drafts.json")
    }

    fn permission_journal_path(&self, session_id: &str) -> Result<PathBuf, StoreError> {
        validate_session_id(session_id)?;
        Ok(self
            .events
            .root
            .join(format!("{session_id}.permissions.json")))
    }

    fn read_permission_journal(&self, session_id: &str) -> Result<PermissionJournal, StoreError> {
        let path = self.permission_journal_path(session_id)?;
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PermissionJournal::default());
            }
            Err(error) => return Err(error.into()),
        };
        let journal: PermissionJournal = serde_json::from_reader(file)?;
        if journal.version != 1 {
            return Err(StoreError::UnsupportedPermissionJournalVersion(
                journal.version,
            ));
        }
        let mut seen = HashSet::new();
        for entry in &journal.entries {
            if entry.session_id != session_id {
                return Err(StoreError::SessionMismatch);
            }
            validate_permission_request_id(&entry.request_id)?;
            if !seen.insert(&entry.request_id) {
                return Err(StoreError::DuplicatePermissionRequestId(
                    entry.request_id.clone(),
                ));
            }
        }
        Ok(journal)
    }

    fn write_permission_journal_atomically(
        &self,
        session_id: &str,
        journal: &PermissionJournal,
    ) -> Result<(), StoreError> {
        self.write_json_atomically(
            self.permission_journal_path(session_id)?,
            &format!("{session_id}.permissions"),
            journal,
        )
    }

    fn read_drafts(&self) -> Result<DraftFile, StoreError> {
        let file = match File::open(self.draft_path()) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DraftFile::default());
            }
            Err(error) => return Err(error.into()),
        };
        let drafts: DraftFile = serde_json::from_reader(file)?;
        if drafts.version != 1 {
            return Err(StoreError::UnsupportedDraftVersion(drafts.version));
        }
        Ok(drafts)
    }

    fn write_drafts_atomically(&self, drafts: &DraftFile) -> Result<(), StoreError> {
        self.write_json_atomically(self.draft_path(), "composer-drafts", drafts)
    }

    fn write_json_atomically<T: Serialize>(
        &self,
        path: PathBuf,
        temporary_stem: &str,
        value: &T,
    ) -> Result<(), StoreError> {
        let temporary = self
            .events
            .root
            .join(format!(".{temporary_stem}.{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<(), StoreError> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            serde_json::to_writer(&mut file, value)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &path)?;
            OpenOptions::new().write(true).open(&path)?.sync_all()?;
            #[cfg(unix)]
            File::open(&self.events.root)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn read_metadata(&self, id: &str) -> Result<Option<Session>, StoreError> {
        let path = self.metadata_path(id)?;
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let session: Session = serde_json::from_reader(file)?;
        if session.id != id {
            return Err(StoreError::SessionMismatch);
        }
        Ok(Some(session))
    }

    /// Replace a metadata file only after a complete, synced copy exists beside it.
    fn write_metadata_atomically(&self, session: &Session) -> Result<(), StoreError> {
        self.write_json_atomically(self.metadata_path(&session.id)?, &session.id, session)
    }

    fn update_metadata(
        &self,
        id: &str,
        change: impl FnOnce(&mut Session),
    ) -> Result<Session, StoreError> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let mut session = self
            .read_metadata(id)?
            .ok_or_else(|| StoreError::UnknownSession(id.to_owned()))?;
        change(&mut session);
        session.updated_at_ms = current_time_ms().max(session.updated_at_ms.saturating_add(1));
        self.write_metadata_atomically(&session)?;
        Ok(session)
    }

    pub fn create(
        &self,
        workspace: impl Into<PathBuf>,
        backend: impl Into<String>,
    ) -> Result<Session, StoreError> {
        let now = current_time_ms();
        let session = Session {
            id: Uuid::new_v4().to_string(),
            workspace: workspace.into(),
            backend: backend.into(),
            created_at_ms: now,
            updated_at_ms: now,
            title: None,
            pinned: false,
            archived: false,
            unread_at_ms: None,
            last_unread_at_ms: 0,
        };
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        self.write_metadata_atomically(&session)?;
        Ok(session)
    }

    pub fn append_event(&self, session_id: &str, event: &AgentEvent) -> Result<(), StoreError> {
        if session_id != event.session_id {
            return Err(StoreError::SessionMismatch);
        }
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        if !self.metadata_path(session_id)?.exists() {
            return Err(StoreError::UnknownSession(session_id.to_owned()));
        }
        self.events.append(event)?;
        if matches!(
            &event.kind,
            crate::AgentEventKind::PermissionRequested { .. }
        ) {
            let path = self.events.path(session_id)?;
            OpenOptions::new().write(true).open(path)?.sync_all()?;
        }
        Ok(())
    }

    /// Override the display title, or clear the override to use a derived title.
    pub fn rename(&self, session_id: &str, title: Option<&str>) -> Result<Session, StoreError> {
        validate_session_id(session_id)?;
        let title = title
            .map(str::trim)
            .map(|value| {
                if value.is_empty() {
                    Err(StoreError::InvalidTitle)
                } else {
                    Ok(value.to_owned())
                }
            })
            .transpose()?;
        self.update_metadata(session_id, |session| session.title = title)
    }

    pub fn set_pinned(&self, session_id: &str, pinned: bool) -> Result<Session, StoreError> {
        self.update_metadata(session_id, |session| session.pinned = pinned)
    }

    pub fn set_archived(&self, session_id: &str, archived: bool) -> Result<Session, StoreError> {
        self.update_metadata(session_id, |session| session.archived = archived)
    }

    /// Allocate a strictly increasing unread marker, even when two changes
    /// happen in the same millisecond or a previous marker was cleared.
    pub fn mark_unread(&self, session_id: &str) -> Result<Session, StoreError> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let mut session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        let watermark = session
            .last_unread_at_ms
            .max(session.unread_at_ms.unwrap_or(0));
        let next = current_time_ms().max(watermark.saturating_add(1));
        session.unread_at_ms = Some(next);
        session.last_unread_at_ms = next;
        self.write_metadata_atomically(&session)?;
        Ok(session)
    }

    /// Clear only the unread state the caller actually observed. A newer
    /// marker survives a delayed navigation or notification acknowledgement.
    pub fn clear_unread_if_matches(
        &self,
        session_id: &str,
        expected_unread_at_ms: u64,
    ) -> Result<(Session, bool), StoreError> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let mut session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        if session.unread_at_ms != Some(expected_unread_at_ms) {
            return Ok((session, false));
        }
        session.unread_at_ms = None;
        self.write_metadata_atomically(&session)?;
        Ok((session, true))
    }

    /// Durably record the user's choice before passing it to the runner.
    /// Retrying the same request and choice returns the original record;
    /// a conflicting choice is rejected so the audit trail stays unambiguous.
    pub fn record_permission_decision(
        &self,
        session_id: &str,
        request_id: &str,
        decision: PermissionDecision,
    ) -> Result<RecordedPermissionDecision, StoreError> {
        validate_session_id(session_id)?;
        validate_permission_request_id(request_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        if self.read_metadata(session_id)?.is_none() {
            return Err(StoreError::UnknownSession(session_id.to_owned()));
        }
        let mut journal = self.read_permission_journal(session_id)?;
        if let Some(existing) = journal
            .entries
            .iter()
            .find(|entry| entry.request_id == request_id)
        {
            return if existing.decision == decision {
                Ok(existing.clone())
            } else {
                Err(StoreError::ConflictingPermissionDecision(
                    request_id.to_owned(),
                ))
            };
        }
        let record = RecordedPermissionDecision {
            session_id: session_id.to_owned(),
            request_id: request_id.to_owned(),
            decision,
            recorded_at_ms: current_time_ms(),
        };
        journal.entries.push(record.clone());
        self.write_permission_journal_atomically(session_id, &journal)?;
        Ok(record)
    }

    pub fn load_permission_decisions(
        &self,
        session_id: &str,
    ) -> Result<Vec<RecordedPermissionDecision>, StoreError> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        Ok(self.read_permission_journal(session_id)?.entries)
    }

    /// Delete metadata first, making the session invisible even if log cleanup fails.
    /// Retrying also removes orphaned event and permission logs.
    pub fn delete(&self, session_id: &str) -> Result<bool, StoreError> {
        let metadata = self.metadata_path(session_id)?;
        let events = self.events.path(session_id)?;
        let permissions = self.permission_journal_path(session_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let mut drafts = self.read_drafts()?;
        let previous_draft_count = drafts.entries.len();
        drafts
            .entries
            .retain(|entry| entry.session_id.as_deref() != Some(session_id));
        let removed_draft = drafts.entries.len() != previous_draft_count;
        if removed_draft {
            self.write_drafts_atomically(&drafts)?;
        }
        let removed_metadata = remove_if_exists(metadata)?;
        let removed_events = remove_if_exists(events)?;
        let removed_permissions = remove_if_exists(permissions)?;
        Ok(removed_draft || removed_metadata || removed_events || removed_permissions)
    }

    /// Persist a plain-text composer draft for a workspace and optional
    /// session. `None` addresses the new-session draft for that workspace.
    /// Saving blank text clears the draft.
    pub fn save_draft(
        &self,
        workspace: &Path,
        session_id: Option<&str>,
        text: &str,
    ) -> Result<(), StoreError> {
        if let Some(id) = session_id {
            validate_session_id(id)?;
        }
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let mut drafts = self.read_drafts()?;
        drafts.entries.retain(|entry| {
            entry.workspace != workspace || entry.session_id.as_deref() != session_id
        });
        if !text.trim().is_empty() {
            drafts.entries.push(DraftEntry {
                workspace: workspace.to_path_buf(),
                session_id: session_id.map(str::to_owned),
                text: text.to_owned(),
                updated_at_ms: current_time_ms(),
            });
        }
        self.write_drafts_atomically(&drafts)
    }

    pub fn load_draft(
        &self,
        workspace: &Path,
        session_id: Option<&str>,
    ) -> Result<Option<String>, StoreError> {
        if let Some(id) = session_id {
            validate_session_id(id)?;
        }
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        Ok(self
            .read_drafts()?
            .entries
            .into_iter()
            .find(|entry| entry.workspace == workspace && entry.session_id.as_deref() == session_id)
            .map(|entry| entry.text))
    }

    /// Returns whether a draft existed. Removing a missing draft is harmless.
    pub fn clear_draft(
        &self,
        workspace: &Path,
        session_id: Option<&str>,
    ) -> Result<bool, StoreError> {
        if let Some(id) = session_id {
            validate_session_id(id)?;
        }
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let mut drafts = self.read_drafts()?;
        let before = drafts.entries.len();
        drafts.entries.retain(|entry| {
            entry.workspace != workspace || entry.session_id.as_deref() != session_id
        });
        if before == drafts.entries.len() {
            return Ok(false);
        }
        self.write_drafts_atomically(&drafts)?;
        Ok(true)
    }

    pub fn list(&self) -> Result<Vec<Session>, StoreError> {
        Ok(self
            .list_with_events()?
            .into_iter()
            .map(|record| record.session)
            .collect())
    }

    pub fn list_active(&self) -> Result<Vec<Session>, StoreError> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|session| !session.archived)
            .collect())
    }

    pub fn list_archived(&self) -> Result<Vec<Session>, StoreError> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|session| session.archived)
            .collect())
    }

    pub fn list_active_with_events(&self) -> Result<Vec<StoredSession>, StoreError> {
        Ok(self
            .list_with_events()?
            .into_iter()
            .filter(|record| !record.session.archived)
            .collect())
    }

    pub fn list_archived_with_events(&self) -> Result<Vec<StoredSession>, StoreError> {
        Ok(self
            .list_with_events()?
            .into_iter()
            .filter(|record| record.session.archived)
            .collect())
    }

    /// Lists sessions together with their event logs so callers can derive
    /// display titles without reading each log a second time.
    pub fn list_with_events(&self) -> Result<Vec<StoredSession>, StoreError> {
        Ok(self.list_with_diagnostics()?.sessions)
    }

    /// Keep readable sessions available when one session's metadata or event
    /// log is malformed. Directory and other filesystem failures still fail
    /// the listing, so they cannot silently look like an empty store.
    pub fn list_with_diagnostics(&self) -> Result<SessionListing, StoreError> {
        let mut sessions = Vec::new();
        let mut diagnostics = Vec::new();
        for entry in fs::read_dir(&self.events.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(id) = name.strip_suffix(".session.json") else {
                continue;
            };
            if validate_session_id(id).is_err() {
                diagnostics.push(SessionDiagnostic {
                    session_id: id.to_owned(),
                    error: StoreError::InvalidSessionId.to_string(),
                });
                continue;
            }
            match self.load(id) {
                Ok(Some(record)) => sessions.push(record),
                Ok(None) => {} // The file disappeared between read_dir and load.
                Err(error) if is_session_corruption(&error) => {
                    diagnostics.push(SessionDiagnostic {
                        session_id: id.to_owned(),
                        error: error.to_string(),
                    })
                }
                Err(error) => return Err(error),
            }
        }
        sessions.sort_by(|left, right| {
            right
                .session
                .updated_at_ms
                .cmp(&left.session.updated_at_ms)
                .then_with(|| left.session.id.cmp(&right.session.id))
        });
        diagnostics.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        Ok(SessionListing {
            sessions,
            diagnostics,
        })
    }

    /// Query the sessions dock and command-center search with deterministic
    /// filtering, ordering and pagination. This initial implementation scans
    /// readable JSONL logs; a dedicated index can replace it without changing
    /// the result contract.
    pub fn query_sessions(&self, query: &SessionQuery) -> Result<SessionQueryResult, StoreError> {
        let search = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if search.is_some_and(|s| s.chars().count() > 256) {
            return Err(StoreError::SearchQueryTooLong);
        }
        let pattern = search
            .map(|s| {
                RegexBuilder::new(&regex::escape(s))
                    .case_insensitive(true)
                    .build()
            })
            .transpose()?;
        let listing = self.list_with_diagnostics()?;
        let mut matches = Vec::new();
        for record in listing.sessions {
            let session = &record.session;
            if !query.workspaces.is_empty() && !query.workspaces.contains(&session.workspace) {
                continue;
            }
            let in_scope = match query.kind {
                SessionListKind::ActiveUnpinned => !session.archived && !session.pinned,
                SessionListKind::Pinned => !session.archived && session.pinned,
                SessionListKind::Archived => session.archived,
                SessionListKind::All => true,
            };
            if !in_scope {
                continue;
            }
            let title = display_title(&record);
            let snippets = if let Some(pattern) = &pattern {
                let visible_text = visible_search_text(&record.events);
                let title_matches = pattern.is_match(&title);
                if !title_matches && !pattern.is_match(&visible_text) {
                    continue;
                }
                search_snippets(&visible_text, pattern, title_matches)
            } else {
                Vec::new()
            };
            matches.push(SessionQueryItem {
                session: record.session,
                title,
                search_snippets: snippets,
            });
        }
        matches.sort_by(|left, right| {
            let left_time = match query.sort {
                SessionSort::Updated => left.session.updated_at_ms,
                SessionSort::Created => left.session.created_at_ms,
            };
            let right_time = match query.sort {
                SessionSort::Updated => right.session.updated_at_ms,
                SessionSort::Created => right.session.created_at_ms,
            };
            right_time
                .cmp(&left_time)
                .then_with(|| right.session.created_at_ms.cmp(&left.session.created_at_ms))
                .then_with(|| left.session.id.cmp(&right.session.id))
        });
        let total = matches.len();
        let items = matches
            .into_iter()
            .skip(query.offset)
            .take(query.limit)
            .collect::<Vec<_>>();
        let has_more = total > query.offset.saturating_add(items.len());
        Ok(SessionQueryResult {
            items,
            total,
            has_more,
            diagnostics: listing.diagnostics,
        })
    }

    pub fn load(&self, session_id: &str) -> Result<Option<StoredSession>, StoreError> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let Some(mut session) = self.read_metadata(session_id)? else {
            return Ok(None);
        };
        let events = self.events.load(session_id)?;
        session.updated_at_ms = session.updated_at_ms.max(
            events
                .last()
                .map(|event| event.timestamp_ms.max(session.created_at_ms))
                .unwrap_or(session.created_at_ms),
        );
        Ok(Some(StoredSession { session, events }))
    }
}

fn display_title(record: &StoredSession) -> String {
    record
        .session
        .title
        .as_deref()
        .or_else(|| {
            record.events.iter().find_map(|event| match &event.kind {
                AgentEventKind::TurnStarted { prompt } => {
                    Some(prompt.lines().next().unwrap_or(prompt).trim())
                }
                _ => None,
            })
        })
        .filter(|title| !title.is_empty())
        .unwrap_or("Nueva tarea")
        .to_owned()
}

/// Mirrors the text a user sees in the transcript. Streaming deltas and tool
/// input/output are excluded, so private or duplicated implementation detail
/// does not leak into search results.
fn visible_search_text(events: &[AgentEvent]) -> String {
    const MAX_SEARCH_TEXT_CHARS: usize = 200_000;
    let mut text = String::new();
    let mut remaining = MAX_SEARCH_TEXT_CHARS;
    for event in events {
        let part = match &event.kind {
            AgentEventKind::TurnStarted { prompt } => prompt.as_str(),
            AgentEventKind::AssistantMessage { content } => content.as_str(),
            AgentEventKind::TurnCompleted { response, .. } => response.as_str(),
            AgentEventKind::ProcessOutput { text, .. } => text.as_str(),
            _ => continue,
        };
        if part.trim().is_empty() || remaining == 0 {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
            remaining -= 1;
        }
        if remaining == 0 {
            break;
        }
        let capped = part.chars().take(remaining).collect::<String>();
        remaining -= capped.chars().count();
        text.push_str(&capped);
    }
    text
}

fn search_snippets(text: &str, pattern: &Regex, title_matches: bool) -> Vec<String> {
    const MAX_SNIPPETS: usize = 4;
    const MAX_SNIPPET_CHARS: usize = 140;
    let mut snippets = Vec::new();
    let mut ranges = Vec::new();
    for found in pattern.find_iter(text) {
        let start = text[..found.start()]
            .char_indices()
            .rev()
            .nth(19)
            .map_or(0, |(index, _)| index);
        let end = text[found.end()..]
            .char_indices()
            .nth(72)
            .map_or(text.len(), |(index, _)| found.end() + index);
        if ranges
            .iter()
            .any(|&(old_start, old_end)| start < old_end && old_start < end)
        {
            continue;
        }
        let raw = format!(
            "{}{}{}",
            if start > 0 { "…" } else { "" },
            &text[start..end],
            if end < text.len() { "…" } else { "" }
        );
        let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        let snippet = normalized
            .chars()
            .take(MAX_SNIPPET_CHARS)
            .collect::<String>();
        if !snippet.is_empty() {
            snippets.push(snippet);
            ranges.push((start, end));
        }
        if snippets.len() == MAX_SNIPPETS {
            break;
        }
    }
    if snippets.is_empty() && title_matches {
        let fallback = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let fallback = fallback.chars().take(MAX_SNIPPET_CHARS).collect::<String>();
        if !fallback.is_empty() {
            snippets.push(fallback);
        }
    }
    snippets
}

fn is_session_corruption(error: &StoreError) -> bool {
    matches!(error, StoreError::Json(_) | StoreError::SessionMismatch)
}

fn remove_if_exists(path: PathBuf) -> Result<bool, StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

impl EventStore for SessionStore {
    fn append(&self, event: &AgentEvent) -> Result<(), StoreError> {
        self.append_event(&event.session_id, event)
    }

    fn load(&self, session_id: &str) -> Result<Vec<AgentEvent>, StoreError> {
        Ok(SessionStore::load(self, session_id)?
            .map(|record| record.events)
            .unwrap_or_default())
    }
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

impl EventStore for JsonlSessionStore {
    fn append(&self, event: &AgentEvent) -> Result<(), StoreError> {
        let path = self.path(&event.session_id)?;
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }

    fn load(&self, session_id: &str) -> Result<Vec<AgentEvent>, StoreError> {
        let path = self.path(session_id)?;
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut events = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            events.push(serde_json::from_slice::<AgentEvent>(&line)?);
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentEventKind;
    use std::sync::Arc;

    #[test]
    fn jsonl_round_trip_and_session_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::new(dir.path()).unwrap();
        let event = AgentEvent::new("session_1", 1, AgentEventKind::TurnCancelled);
        store.append(&event).unwrap();
        assert_eq!(store.load("session_1").unwrap(), vec![event]);
        assert!(store.load("session_2").unwrap().is_empty());
        assert!(matches!(
            store.load("../escape"),
            Err(StoreError::InvalidSessionId)
        ));
        assert_eq!(store.list_sessions().unwrap(), vec!["session_1"]);
    }

    #[test]
    fn session_store_lists_and_loads_events() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("/workspace", "mock-model").unwrap();
        let event = AgentEvent::new(&session.id, 1, AgentEventKind::TurnCancelled);
        store.append_event(&session.id, &event).unwrap();
        let loaded = store.load(&session.id).unwrap().unwrap();
        assert_eq!(loaded.session.id, session.id);
        assert_eq!(loaded.events, vec![event]);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn create_publishes_complete_metadata_and_cleans_failed_temporary_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let created = store.create("/workspace", "native").unwrap();
        let metadata_path = dir.path().join(format!("{}.session.json", created.id));
        let metadata = fs::read_to_string(metadata_path).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&metadata).unwrap(), created);

        let blocked = Session {
            id: "blocked".to_owned(),
            workspace: PathBuf::from("/workspace"),
            backend: "native".to_owned(),
            created_at_ms: 1,
            updated_at_ms: 1,
            title: None,
            pinned: false,
            archived: false,
            unread_at_ms: None,
            last_unread_at_ms: 0,
        };
        let blocked_path = dir.path().join("blocked.session.json");
        fs::create_dir(&blocked_path).unwrap();
        assert!(matches!(
            store.write_metadata_atomically(&blocked),
            Err(StoreError::Io(_))
        ));
        assert!(blocked_path.is_dir());
        assert!(!fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn listing_reports_corrupt_metadata_and_event_log_without_losing_good_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let good = store.create("/workspace", "native").unwrap();
        let broken_log = store.create("/workspace", "native").unwrap();
        fs::write(
            dir.path().join("broken_metadata.session.json"),
            b"{\"id\":\"broken_metadata\"",
        )
        .unwrap();
        fs::write(
            dir.path().join(format!("{}.jsonl", broken_log.id)),
            b"{\"session_id\":",
        )
        .unwrap();

        let listing = store.list_with_diagnostics().unwrap();
        assert_eq!(listing.sessions.len(), 1);
        assert_eq!(listing.sessions[0].session.id, good.id);
        assert_eq!(listing.diagnostics.len(), 2);
        assert!(listing
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.session_id == "broken_metadata"));
        assert!(listing
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.session_id == broken_log.id));
        assert!(listing
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.error.contains("encoding")));
        assert_eq!(store.list_with_events().unwrap().len(), 1);
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.list_with_diagnostics().unwrap().diagnostics.len(),
            2
        );
        assert_eq!(reopened.list().unwrap().len(), 1);
    }

    #[test]
    fn listing_does_not_hide_general_filesystem_failures() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        store.create("/workspace", "native").unwrap();
        fs::create_dir(dir.path().join("inaccessible.session.json")).unwrap();
        assert!(matches!(
            store.list_with_diagnostics(),
            Err(StoreError::Io(_))
        ));
        assert!(matches!(store.list_with_events(), Err(StoreError::Io(_))));
    }

    #[test]
    fn composer_drafts_persist_by_workspace_and_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let one = Path::new("C:/work/one");
        let two = Path::new("C:/work/two");
        let session = store.create(one, "native").unwrap();
        store.save_draft(one, None, "new task draft").unwrap();
        store
            .save_draft(one, Some(&session.id), "session follow-up")
            .unwrap();
        store.save_draft(two, None, "other workspace").unwrap();
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.load_draft(one, None).unwrap().as_deref(),
            Some("new task draft")
        );
        assert_eq!(
            reopened
                .load_draft(one, Some(&session.id))
                .unwrap()
                .as_deref(),
            Some("session follow-up")
        );
        assert_eq!(
            reopened.load_draft(two, None).unwrap().as_deref(),
            Some("other workspace")
        );
        assert_eq!(reopened.load_draft(two, Some(&session.id)).unwrap(), None);

        reopened.save_draft(one, None, "  ").unwrap();
        assert_eq!(reopened.load_draft(one, None).unwrap(), None);
        assert!(reopened.clear_draft(one, Some(&session.id)).unwrap());
        assert!(!reopened.clear_draft(one, Some(&session.id)).unwrap());
        assert_eq!(
            reopened.load_draft(two, None).unwrap().as_deref(),
            Some("other workspace")
        );
        assert!(matches!(
            reopened.save_draft(one, Some("../escape"), "bad"),
            Err(StoreError::InvalidSessionId)
        ));
    }

    #[test]
    fn composer_draft_write_failure_cleans_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let path = store.draft_path();
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            store.write_drafts_atomically(&DraftFile::default()),
            Err(StoreError::Io(_))
        ));
        assert!(path.is_dir());
        assert!(!fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn deletion_cleans_session_draft_but_keeps_new_task_and_other_session_drafts() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let workspace = Path::new("C:/work/project");
        let removed = store.create(workspace, "native").unwrap();
        let kept = store.create(workspace, "native").unwrap();
        store.save_draft(workspace, None, "new task").unwrap();
        store
            .save_draft(workspace, Some(&removed.id), "remove me")
            .unwrap();
        store
            .save_draft(workspace, Some(&kept.id), "keep me")
            .unwrap();

        assert!(store.delete(&removed.id).unwrap());
        assert_eq!(
            store.load_draft(workspace, Some(&removed.id)).unwrap(),
            None
        );
        assert_eq!(
            store.load_draft(workspace, None).unwrap().as_deref(),
            Some("new task")
        );
        assert_eq!(
            store
                .load_draft(workspace, Some(&kept.id))
                .unwrap()
                .as_deref(),
            Some("keep me")
        );
        store
            .save_draft(workspace, Some(&removed.id), "orphaned draft")
            .unwrap();
        assert!(store.delete(&removed.id).unwrap());
        assert!(!store.delete(&removed.id).unwrap());
    }

    #[test]
    fn failed_draft_cleanup_leaves_session_intact_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let workspace = Path::new("C:/work/project");
        let session = store.create(workspace, "native").unwrap();
        store
            .save_draft(workspace, Some(&session.id), "unsent")
            .unwrap();
        let draft_path = store.draft_path();
        fs::write(&draft_path, b"{\"version\":1,\"entries\":[").unwrap();

        assert!(matches!(
            store.delete(&session.id),
            Err(StoreError::Json(_))
        ));
        assert!(store.load(&session.id).unwrap().is_some());
    }

    #[test]
    fn legacy_metadata_defaults_survive_atomic_updates_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let id = "legacy_session";
        let old_metadata = serde_json::json!({
            "id": id,
            "workspace": "C:/legacy/project",
            "backend": "native",
            "created_at_ms": 10,
            "updated_at_ms": 10
        });
        fs::write(
            dir.path().join(format!("{id}.session.json")),
            old_metadata.to_string(),
        )
        .unwrap();

        let store = SessionStore::open(dir.path()).unwrap();
        let loaded = store.load(id).unwrap().unwrap().session;
        assert_eq!(loaded.title, None);
        assert!(!loaded.pinned);
        assert!(!loaded.archived);
        assert_eq!(loaded.unread_at_ms, None);
        assert_eq!(loaded.last_unread_at_ms, 0);
        store.rename(id, Some("  Legacy renamed  ")).unwrap();
        store.set_pinned(id, true).unwrap();
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        let loaded = reopened.load(id).unwrap().unwrap().session;
        assert_eq!(loaded.title.as_deref(), Some("Legacy renamed"));
        assert!(loaded.pinned);
        assert!(!loaded.archived);
        assert_eq!(loaded.workspace, PathBuf::from("C:/legacy/project"));
        assert_eq!(loaded.backend, "native");
        assert_eq!(loaded.unread_at_ms, None);
        assert_eq!(reopened.list_active().unwrap().len(), 1);
    }

    #[test]
    fn unread_compare_and_clear_preserves_newer_notification() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let created = store.create("/workspace", "native").unwrap();
        let first = store.mark_unread(&created.id).unwrap();
        let first_marker = first.unread_at_ms.unwrap();
        assert_eq!(first.updated_at_ms, created.updated_at_ms);
        let second = store.mark_unread(&created.id).unwrap();
        let second_marker = second.unread_at_ms.unwrap();
        assert!(second_marker > first_marker);
        let (unchanged, cleared) = store
            .clear_unread_if_matches(&created.id, first_marker)
            .unwrap();
        assert!(!cleared);
        assert_eq!(unchanged.unread_at_ms, Some(second_marker));

        let (cleared_session, cleared) = store
            .clear_unread_if_matches(&created.id, second_marker)
            .unwrap();
        assert!(cleared);
        assert_eq!(cleared_session.unread_at_ms, None);
        assert_eq!(cleared_session.last_unread_at_ms, second_marker);
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        let third = reopened.mark_unread(&created.id).unwrap();
        assert!(third.unread_at_ms.unwrap() > second_marker);
        assert_eq!(third.updated_at_ms, created.updated_at_ms);
    }

    #[test]
    fn permission_decisions_are_durable_idempotent_and_separate_from_agent_events() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path()).unwrap());
        let session = store.create("/workspace", "native").unwrap();
        let first = store
            .record_permission_decision(&session.id, "request-1", PermissionDecision::Allow)
            .unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let store = Arc::clone(&store);
                let session_id = &session.id;
                let expected = &first;
                scope.spawn(move || {
                    assert_eq!(
                        store
                            .record_permission_decision(
                                session_id,
                                "request-1",
                                PermissionDecision::Allow,
                            )
                            .unwrap(),
                        *expected
                    );
                });
            }
        });
        assert!(matches!(
            store.record_permission_decision(&session.id, "request-1", PermissionDecision::Deny),
            Err(StoreError::ConflictingPermissionDecision(_))
        ));
        let second = store
            .record_permission_decision(&session.id, "request-2", PermissionDecision::Deny)
            .unwrap();
        assert_eq!(
            store.load_permission_decisions(&session.id).unwrap(),
            vec![first.clone(), second.clone()]
        );
        assert!(store.load(&session.id).unwrap().unwrap().events.is_empty());
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.load_permission_decisions(&session.id).unwrap(),
            vec![first, second]
        );
        assert!(matches!(
            reopened.record_permission_decision(&session.id, " ", PermissionDecision::Allow),
            Err(StoreError::InvalidPermissionRequestId)
        ));
        assert!(reopened.delete(&session.id).unwrap());
        assert!(reopened
            .load_permission_decisions(&session.id)
            .unwrap()
            .is_empty());
        assert!(!reopened.delete(&session.id).unwrap());
    }

    #[test]
    fn permission_journal_publish_failure_leaves_session_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let journal_path = store.permission_journal_path(&session.id).unwrap();
        fs::create_dir(&journal_path).unwrap();
        assert!(matches!(
            store.record_permission_decision(&session.id, "request-1", PermissionDecision::Allow),
            Err(StoreError::Io(_))
        ));
        assert!(store.load(&session.id).unwrap().is_some());
        assert!(!fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
        fs::remove_dir(journal_path).unwrap();
        assert!(store
            .record_permission_decision(&session.id, "request-1", PermissionDecision::Allow)
            .is_ok());
    }

    #[test]
    fn session_management_persists_and_deletion_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("C:/work/project", "codex").unwrap();
        let event = AgentEvent::new(&session.id, 1, AgentEventKind::TurnCancelled);
        store.append_event(&session.id, &event).unwrap();
        let renamed = store.rename(&session.id, Some("A better title")).unwrap();
        assert!(renamed.updated_at_ms > session.updated_at_ms);
        store.set_pinned(&session.id, true).unwrap();
        store.set_archived(&session.id, true).unwrap();
        assert!(store.list_active().unwrap().is_empty());
        assert!(store.list_active_with_events().unwrap().is_empty());
        assert_eq!(store.list_archived().unwrap().len(), 1);
        assert_eq!(
            store.list_archived_with_events().unwrap()[0].events,
            vec![event.clone()]
        );
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        let loaded = reopened.load(&session.id).unwrap().unwrap();
        assert_eq!(loaded.session.title.as_deref(), Some("A better title"));
        assert!(loaded.session.pinned);
        assert!(loaded.session.archived);
        assert_eq!(loaded.events, vec![event.clone()]);

        reopened.rename(&session.id, None).unwrap();
        reopened.set_archived(&session.id, false).unwrap();
        assert_eq!(reopened.list_active().unwrap().len(), 1);
        assert_eq!(
            reopened.load(&session.id).unwrap().unwrap().session.title,
            None
        );
        assert!(reopened.delete(&session.id).unwrap());
        assert!(!reopened.delete(&session.id).unwrap());
        assert!(reopened.load(&session.id).unwrap().is_none());
        assert!(reopened.list().unwrap().is_empty());
        let log_path = dir.path().join(format!("{}.jsonl", session.id));
        assert!(!log_path.exists());
        // A retry still cleans up a log left behind by an interrupted delete.
        fs::write(&log_path, b"orphaned log\n").unwrap();
        assert!(reopened.delete(&session.id).unwrap());
        assert!(!log_path.exists());
        assert!(matches!(
            reopened.append_event(&session.id, &event),
            Err(StoreError::UnknownSession(_))
        ));
        assert!(!fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn metadata_mutations_validate_inputs_and_preserve_independent_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path()).unwrap());
        let session = store.create("C:/work/project", "native").unwrap();
        assert!(matches!(
            store.rename(&session.id, Some("  ")),
            Err(StoreError::InvalidTitle)
        ));
        assert!(matches!(
            store.set_pinned("../escape", true),
            Err(StoreError::InvalidSessionId)
        ));
        assert!(matches!(
            store.delete("../escape"),
            Err(StoreError::InvalidSessionId)
        ));
        assert!(matches!(
            store.set_archived("unknown", true),
            Err(StoreError::UnknownSession(_))
        ));

        std::thread::scope(|scope| {
            let renamed = Arc::clone(&store);
            let id = &session.id;
            scope.spawn(move || {
                for index in 0..8 {
                    renamed.rename(id, Some(&format!("Title {index}"))).unwrap();
                }
            });
            let pinned = Arc::clone(&store);
            scope.spawn(move || {
                for index in 0..8 {
                    pinned.set_pinned(id, index % 2 == 1).unwrap();
                }
            });
        });
        let loaded = store.load(&session.id).unwrap().unwrap().session;
        assert_eq!(loaded.title.as_deref(), Some("Title 7"));
        assert!(loaded.pinned);
    }

    #[test]
    fn session_query_filters_searches_and_paginates_visible_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let main = store.create("C:/work/main", "native").unwrap();
        store
            .append_event(
                &main.id,
                &AgentEvent::new(
                    &main.id,
                    1,
                    AgentEventKind::TurnStarted {
                        prompt: "Investiga navegación\ncon detalle".into(),
                    },
                ),
            )
            .unwrap();
        store
            .append_event(
                &main.id,
                &AgentEvent::new(
                    &main.id,
                    2,
                    AgentEventKind::AssistantMessage {
                        content: "El résumé de las sesiones está listo".into(),
                    },
                ),
            )
            .unwrap();
        store
            .append_event(
                &main.id,
                &AgentEvent::new(
                    &main.id,
                    3,
                    AgentEventKind::ToolFinished {
                        tool_call_id: "tool-1".into(),
                        output: "secreto-interno".into(),
                        is_error: false,
                    },
                ),
            )
            .unwrap();
        let pinned = store.create("C:/work/main", "codex").unwrap();
        store.set_pinned(&pinned.id, true).unwrap();
        store.rename(&pinned.id, Some("Tarea fijada")).unwrap();
        let archived = store.create("C:/work/other", "native").unwrap();
        store.set_archived(&archived.id, true).unwrap();
        store
            .rename(&archived.id, Some("Navegación antigua"))
            .unwrap();

        let base = SessionQuery::default();
        let regular = store.query_sessions(&base).unwrap();
        assert_eq!(regular.total, 1);
        assert_eq!(regular.items[0].session.id, main.id);
        assert_eq!(regular.items[0].title, "Investiga navegación");
        assert!(regular.items[0].search_snippets.is_empty());

        let body = store
            .query_sessions(&SessionQuery {
                search: Some("RÉSUMÉ".into()),
                ..base.clone()
            })
            .unwrap();
        assert_eq!(body.total, 1);
        assert!(body.items[0].search_snippets[0].contains("résumé"));
        assert_eq!(
            store
                .query_sessions(&SessionQuery {
                    search: Some("secreto-interno".into()),
                    ..base.clone()
                })
                .unwrap()
                .total,
            0
        );

        let pinned_result = store
            .query_sessions(&SessionQuery {
                kind: SessionListKind::Pinned,
                search: Some("FIJADA".into()),
                ..base.clone()
            })
            .unwrap();
        assert_eq!(pinned_result.total, 1);
        assert_eq!(pinned_result.items[0].session.id, pinned.id);
        let archived_result = store
            .query_sessions(&SessionQuery {
                kind: SessionListKind::Archived,
                workspaces: vec![PathBuf::from("C:/work/other")],
                search: Some("ANTIGUA".into()),
                ..base.clone()
            })
            .unwrap();
        assert_eq!(archived_result.total, 1);
        assert_eq!(archived_result.items[0].session.id, archived.id);

        let first_page = store
            .query_sessions(&SessionQuery {
                kind: SessionListKind::All,
                limit: 1,
                ..base.clone()
            })
            .unwrap();
        let next_page = store
            .query_sessions(&SessionQuery {
                kind: SessionListKind::All,
                offset: 1,
                limit: 1,
                ..base.clone()
            })
            .unwrap();
        assert_eq!(first_page.total, 3);
        assert!(first_page.has_more);
        assert_ne!(
            first_page.items[0].session.id,
            next_page.items[0].session.id
        );
        assert!(matches!(
            store.query_sessions(&SessionQuery {
                search: Some("x".repeat(257)),
                ..base
            }),
            Err(StoreError::SearchQueryTooLong)
        ));
    }

    #[test]
    fn session_query_reports_corrupt_records_and_limits_snippets() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let prompt = (0..8)
            .map(|index| format!("Coincidencia {index} {}", "x".repeat(150)))
            .collect::<Vec<_>>()
            .join("\n");
        store
            .append_event(
                &session.id,
                &AgentEvent::new(&session.id, 1, AgentEventKind::TurnStarted { prompt }),
            )
            .unwrap();
        fs::write(dir.path().join("broken.session.json"), b"not-json").unwrap();
        let result = store
            .query_sessions(&SessionQuery {
                search: Some("coincidencia".into()),
                ..SessionQuery::default()
            })
            .unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.items[0].search_snippets.len(), 4);
    }
}
