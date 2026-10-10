//! Durable native objectives, separate from prompt text and turn history.
use crate::{AgentEventKind, NativeSessionLease, SessionStore, StoreError};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    sync::Arc,
};
use thiserror::Error;
use uuid::Uuid;

const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_GOALS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeGoalStatus {
    Active,
    Paused,
    Complete,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeGoalEvidence {
    pub turn_id: String,
    pub summary: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeGoalCriterion {
    pub id: String,
    pub description: String,
    pub evidence: Option<NativeGoalEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeGoal {
    pub id: String,
    pub objective: String,
    pub criteria: Vec<NativeGoalCriterion>,
    pub status: NativeGoalStatus,
    pub revision: u64,
    changes: Vec<GoalChange>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeGoalMutation {
    PauseByUser,
    ResumeByUser,
    RecordEvidence {
        criterion_id: String,
        evidence: NativeGoalEvidence,
    },
    Complete,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct GoalChange {
    id: String,
    expected_revision: u64,
    mutation: NativeGoalMutation,
}

#[derive(Serialize, Deserialize)]
struct Ledger {
    version: u32,
    session_id: String,
    goals: Vec<NativeGoal>,
}

#[derive(Debug, Error)]
pub enum NativeGoalError {
    #[error("native goal store failed: {0}")]
    Store(#[from] StoreError),
    #[error("native goal I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("native goal encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid native goal or ledger")]
    Invalid,
    #[error("goal identity, revision or operation conflicts")]
    Conflict,
    #[error("another unfinished native goal exists")]
    Unfinished,
    #[error("goal evidence requires a completed native turn")]
    UnverifiedTurn,
    #[error("every goal criterion requires evidence before completion")]
    MissingEvidence,
}

pub struct NativeGoalStore {
    sessions: Arc<SessionStore>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativeGoalRecoveryReport {
    pub removed_temporaries: usize,
    pub busy_session_ids: Vec<String>,
    pub diagnostics: Vec<NativeGoalRecoveryDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeGoalRecoveryDiagnostic {
    pub session_id: String,
    pub error: String,
}

/// Proof of one exact published mutation, not a synthesized tool result or a
/// claim that the goal's evidence independently verifies its objective.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeGoalCommitReceipt {
    pub goal_id: String,
    pub operation_id: String,
    pub committed_revision: u64,
    pub current_revision: u64,
}

impl NativeGoalStore {
    pub fn new(sessions: Arc<SessionStore>) -> Self {
        Self { sessions }
    }

    /// Startup maintenance inventories only goal-owned filenames, without
    /// loading every session transcript. Active workers retain their files;
    /// corrupt or orphaned ledgers are reported and preserved per session.
    pub fn recover_idle_sessions(&self) -> Result<NativeGoalRecoveryReport, NativeGoalError> {
        let mut candidates = BTreeSet::new();
        for entry in fs::read_dir(self.sessions.storage_root())? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let session_id = name.strip_suffix(".native-goals.json").or_else(|| {
                let (session, suffix) = name.strip_prefix('.')?.rsplit_once(".native-goals.")?;
                let id = suffix.strip_suffix(".tmp")?;
                let uuid = Uuid::parse_str(id).ok()?;
                (uuid.get_version() == Some(uuid::Version::Random)
                    && uuid.hyphenated().to_string() == id)
                    .then_some(session)
            });
            if let Some(id) = session_id.filter(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            }) {
                candidates.insert(id.to_owned());
            }
        }
        let mut report = NativeGoalRecoveryReport::default();
        for session_id in candidates {
            match self.sessions.try_acquire_native_lease(&session_id) {
                Ok(lease) => match self.recover_temporaries(&lease) {
                    Ok(count) => report.removed_temporaries += count,
                    Err(error) => report.diagnostics.push(NativeGoalRecoveryDiagnostic {
                        session_id,
                        error: error.to_string(),
                    }),
                },
                Err(StoreError::NativeSessionBusy(_)) => report.busy_session_ids.push(session_id),
                Err(error) => report.diagnostics.push(NativeGoalRecoveryDiagnostic {
                    session_id,
                    error: error.to_string(),
                }),
            }
        }
        Ok(report)
    }

    /// Recover only abandoned publication files for this leased session. The
    /// published ledger remains authoritative, including an absent first goal.
    pub fn recover_temporaries(
        &self,
        lease: &NativeSessionLease,
    ) -> Result<usize, NativeGoalError> {
        self.sessions.native_session_for_lease(lease)?;
        let _lock = self.lock(lease.session_id())?;
        self.read(lease.session_id())?;
        self.cleanup_temporaries(lease.session_id())
    }

    // Caller holds the goal writer lock and native session lease.
    fn cleanup_temporaries(&self, session_id: &str) -> Result<usize, NativeGoalError> {
        let prefix = format!(".{session_id}.native-goals.");
        let mut removed = 0;
        for entry in fs::read_dir(self.sessions.storage_root())? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(id) = name
                .strip_prefix(&prefix)
                .and_then(|name| name.strip_suffix(".tmp"))
            else {
                continue;
            };
            let Ok(uuid) = Uuid::parse_str(id) else {
                continue;
            };
            if uuid.get_version() != Some(uuid::Version::Random)
                || uuid.hyphenated().to_string() != id
                || !entry.file_type()?.is_file()
            {
                continue;
            }
            fs::remove_file(entry.path())?;
            removed += 1;
        }
        #[cfg(unix)]
        if removed > 0 {
            File::open(self.sessions.storage_root())?.sync_all()?;
        }
        Ok(removed)
    }

    /// Read-only projection. Completed objectives remain in the bounded ledger.
    pub fn list(&self, session_id: &str) -> Result<Vec<NativeGoal>, NativeGoalError> {
        let session = self
            .sessions
            .load(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.into()))?;
        if session.session.backend != "native" {
            return Err(StoreError::NativeTurnRequiresNativeSession.into());
        }
        Ok(self.read(session_id)?.goals)
    }

    /// Read-only recovery lookup, including historical goals. Absence does not
    /// authorize replay; callers still need the native journal's recovery gate.
    /// An operation ID with different arguments conflicts instead of proving
    /// that the requested mutation committed. Never cleans temporary files.
    pub fn committed_update(
        &self,
        lease: &NativeSessionLease,
        goal_id: &str,
        operation_id: &str,
        expected_revision: u64,
        mutation: &NativeGoalMutation,
    ) -> Result<Option<NativeGoalCommitReceipt>, NativeGoalError> {
        self.sessions.native_session_for_lease(lease)?;
        if !valid_id(goal_id) || !valid_id(operation_id) {
            return Err(NativeGoalError::Invalid);
        }
        let _lock = self.lock(lease.session_id())?;
        let ledger = self.read(lease.session_id())?;
        let Some(goal) = ledger.goals.iter().find(|goal| goal.id == goal_id) else {
            return Ok(None);
        };
        let Some(change) = goal.changes.iter().find(|change| change.id == operation_id) else {
            return Ok(None);
        };
        if change.expected_revision != expected_revision || &change.mutation != mutation {
            return Err(NativeGoalError::Conflict);
        }
        Ok(Some(NativeGoalCommitReceipt {
            goal_id: goal.id.clone(),
            operation_id: operation_id.into(),
            committed_revision: change.expected_revision + 1,
            current_revision: goal.revision,
        }))
    }

    /// Called only for an explicit user goal request. Objective/criteria are
    /// immutable. Retrying the same request ID never replaces current state.
    pub fn create(
        &self,
        lease: &NativeSessionLease,
        id: &str,
        objective: &str,
        criteria: Vec<NativeGoalCriterion>,
    ) -> Result<NativeGoal, NativeGoalError> {
        self.sessions.native_session_for_lease(lease)?;
        let _lock = self.lock(lease.session_id())?;
        let mut ledger = self.read(lease.session_id())?;
        let candidate = NativeGoal {
            id: id.into(),
            objective: objective.into(),
            criteria,
            status: NativeGoalStatus::Active,
            revision: 0,
            changes: vec![],
        };
        validate_goal(&candidate)?;
        if candidate.criteria.iter().any(|c| c.evidence.is_some()) {
            return Err(NativeGoalError::Invalid);
        }
        if let Some(saved) = ledger.goals.iter().find(|goal| goal.id == id) {
            if saved.objective == candidate.objective
                && saved
                    .criteria
                    .iter()
                    .map(|c| (&c.id, &c.description))
                    .eq(candidate.criteria.iter().map(|c| (&c.id, &c.description)))
            {
                return Ok(saved.clone());
            }
            return Err(NativeGoalError::Conflict);
        }
        if ledger
            .goals
            .last()
            .is_some_and(|goal| goal.status != NativeGoalStatus::Complete)
        {
            return Err(NativeGoalError::Unfinished);
        }
        if ledger.goals.len() >= MAX_GOALS {
            return Err(NativeGoalError::Invalid);
        }
        ledger.goals.push(candidate.clone());
        self.write(&ledger)?;
        Ok(candidate)
    }

    /// Host entry points must expose Pause/Resume only to user actions. Model
    /// tools may record evidence and request completion, never manufacture a
    /// user pause/resume. Operation IDs bind retries to exact mutation data.
    pub fn update(
        &self,
        lease: &NativeSessionLease,
        goal_id: &str,
        operation_id: &str,
        expected_revision: u64,
        mutation: NativeGoalMutation,
    ) -> Result<NativeGoal, NativeGoalError> {
        self.sessions.native_session_for_lease(lease)?;
        if !valid_id(operation_id) {
            return Err(NativeGoalError::Invalid);
        }
        let _lock = self.lock(lease.session_id())?;
        let mut ledger = self.read(lease.session_id())?;
        let goal = ledger
            .goals
            .last_mut()
            .filter(|goal| goal.id == goal_id)
            .ok_or(NativeGoalError::Conflict)?;
        let change = GoalChange {
            id: operation_id.into(),
            expected_revision,
            mutation: mutation.clone(),
        };
        if let Some(previous) = goal.changes.iter().find(|change| change.id == operation_id) {
            return if previous == &change {
                Ok(goal.clone())
            } else {
                Err(NativeGoalError::Conflict)
            };
        }
        if goal.changes.len() >= 256 {
            return Err(NativeGoalError::Invalid);
        }
        if goal.revision != expected_revision || goal.status == NativeGoalStatus::Complete {
            return Err(NativeGoalError::Conflict);
        }
        match &mutation {
            NativeGoalMutation::PauseByUser => goal.status = NativeGoalStatus::Paused,
            NativeGoalMutation::ResumeByUser => goal.status = NativeGoalStatus::Active,
            NativeGoalMutation::RecordEvidence {
                criterion_id,
                evidence,
            } => {
                self.verify_turn(lease.session_id(), evidence)?;
                let criterion = goal
                    .criteria
                    .iter_mut()
                    .find(|c| &c.id == criterion_id)
                    .ok_or(NativeGoalError::Invalid)?;
                criterion.evidence = Some(evidence.clone());
            }
            NativeGoalMutation::Complete => {
                if goal.status != NativeGoalStatus::Active {
                    return Err(NativeGoalError::Conflict);
                }
                for criterion in &goal.criteria {
                    self.verify_turn(
                        lease.session_id(),
                        criterion
                            .evidence
                            .as_ref()
                            .ok_or(NativeGoalError::MissingEvidence)?,
                    )?;
                }
                goal.status = NativeGoalStatus::Complete;
            }
        }
        goal.revision = goal
            .revision
            .checked_add(1)
            .ok_or(NativeGoalError::Invalid)?;
        goal.changes.push(change);
        validate_goal(goal)?;
        let result = goal.clone();
        self.write(&ledger)?;
        Ok(result)
    }

    fn verify_turn(
        &self,
        session_id: &str,
        evidence: &NativeGoalEvidence,
    ) -> Result<(), NativeGoalError> {
        if !valid_id(&evidence.turn_id)
            || evidence.summary.trim().is_empty()
            || evidence.summary.len() > 16 * 1024
        {
            return Err(NativeGoalError::Invalid);
        }
        let stored = self
            .sessions
            .load(session_id)?
            .ok_or(NativeGoalError::UnverifiedTurn)?;
        let mut current = None;
        for event in &stored.events {
            match &event.kind {
                AgentEventKind::TurnAdmitted { turn_id, .. }
                | AgentEventKind::TurnAdmittedWithImages { turn_id, .. } => {
                    current = Some(turn_id.as_str())
                }
                AgentEventKind::TurnCompleted { .. }
                    if current == Some(evidence.turn_id.as_str()) =>
                {
                    return Ok(())
                }
                _ => {}
            }
        }
        Err(NativeGoalError::UnverifiedTurn)
    }
    fn lock(&self, id: &str) -> Result<File, NativeGoalError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(
                self.sessions
                    .storage_root()
                    .join(format!(".{id}.native-goals.lock")),
            )?;
        file.lock()?;
        Ok(file)
    }
    fn read(&self, id: &str) -> Result<Ledger, NativeGoalError> {
        let path = self
            .sessions
            .storage_root()
            .join(format!("{id}.native-goals.json"));
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Ledger {
                    version: 1,
                    session_id: id.into(),
                    goals: vec![],
                })
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = vec![];
        file.take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(NativeGoalError::Invalid);
        }
        let ledger: Ledger = serde_json::from_slice(&bytes)?;
        if ledger.version != 1 || ledger.session_id != id || ledger.goals.len() > MAX_GOALS {
            return Err(NativeGoalError::Invalid);
        }
        let mut ids = HashSet::new();
        for (index, goal) in ledger.goals.iter().enumerate() {
            validate_goal(goal)?;
            if !ids.insert(&goal.id)
                || (index + 1 < ledger.goals.len() && goal.status != NativeGoalStatus::Complete)
            {
                return Err(NativeGoalError::Invalid);
            }
        }
        Ok(ledger)
    }
    fn write(&self, ledger: &Ledger) -> Result<(), NativeGoalError> {
        let bytes = serde_json::to_vec(ledger)?;
        if bytes.len() + 1 > MAX_FILE_BYTES {
            return Err(NativeGoalError::Invalid);
        }
        let root = self.sessions.storage_root();
        self.cleanup_temporaries(&ledger.session_id)?;
        let temporary = root.join(format!(
            ".{}.native-goals.{}.tmp",
            ledger.session_id,
            Uuid::new_v4()
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            #[cfg(test)]
            goal_publication_cut(ledger, "temporary-synced");
            fs::rename(
                &temporary,
                root.join(format!("{}.native-goals.json", ledger.session_id)),
            )?;
            #[cfg(test)]
            goal_publication_cut(ledger, "renamed");
            #[cfg(unix)]
            File::open(root)?.sync_all()?;
            #[cfg(test)]
            goal_publication_cut(ledger, "directory-synced");
            Ok::<(), NativeGoalError>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}
#[cfg(test)]
fn goal_publication_cut(ledger: &Ledger, stage: &str) {
    if std::env::var("OPENIDE_GOAL_CUT_STAGE").as_deref() != Ok(stage) {
        return;
    }
    let Some(goal) = ledger.goals.last() else {
        return;
    };
    let target = if std::env::var("OPENIDE_GOAL_CUT_ACTION").as_deref() == Ok("create") {
        goal.id == "publication"
    } else {
        goal.changes
            .last()
            .is_some_and(|change| change.id == "publication")
    };
    if target {
        std::process::exit(86);
    }
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}
fn validate_goal(goal: &NativeGoal) -> Result<(), NativeGoalError> {
    let mut ids = HashSet::new();
    let mut changes = HashSet::new();
    if goal.changes.len() > 256 || goal.revision != goal.changes.len() as u64 {
        return Err(NativeGoalError::Invalid);
    }
    for (revision, change) in goal.changes.iter().enumerate() {
        if !valid_id(&change.id)
            || !changes.insert(&change.id)
            || change.expected_revision != revision as u64
        {
            return Err(NativeGoalError::Invalid);
        }
    }
    if !valid_id(&goal.id)
        || goal.objective.trim().is_empty()
        || goal.objective.len() > 64 * 1024
        || goal.criteria.is_empty()
        || goal.criteria.len() > 32
    {
        return Err(NativeGoalError::Invalid);
    }
    for c in &goal.criteria {
        if !valid_id(&c.id)
            || !ids.insert(&c.id)
            || c.description.trim().is_empty()
            || c.description.len() > 16 * 1024
        {
            return Err(NativeGoalError::Invalid);
        }
        if let Some(e) = &c.evidence {
            if !valid_id(&e.turn_id) || e.summary.trim().is_empty() || e.summary.len() > 16 * 1024 {
                return Err(NativeGoalError::Invalid);
            }
        }
    }
    let mut projected = goal.criteria.clone();
    for criterion in &mut projected {
        criterion.evidence = None;
    }
    let mut status = NativeGoalStatus::Active;
    for change in &goal.changes {
        if status == NativeGoalStatus::Complete {
            return Err(NativeGoalError::Invalid);
        }
        match &change.mutation {
            NativeGoalMutation::PauseByUser => status = NativeGoalStatus::Paused,
            NativeGoalMutation::ResumeByUser => status = NativeGoalStatus::Active,
            NativeGoalMutation::RecordEvidence {
                criterion_id,
                evidence,
            } => {
                if !valid_id(&evidence.turn_id)
                    || evidence.summary.trim().is_empty()
                    || evidence.summary.len() > 16 * 1024
                {
                    return Err(NativeGoalError::Invalid);
                }
                let criterion = projected
                    .iter_mut()
                    .find(|c| &c.id == criterion_id)
                    .ok_or(NativeGoalError::Invalid)?;
                criterion.evidence = Some(evidence.clone());
            }
            NativeGoalMutation::Complete => {
                if status != NativeGoalStatus::Active
                    || projected.iter().any(|c| c.evidence.is_none())
                {
                    return Err(NativeGoalError::Invalid);
                }
                status = NativeGoalStatus::Complete;
            }
        }
    }
    if status != goal.status || projected != goal.criteria {
        return Err(NativeGoalError::Invalid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn criteria() -> Vec<NativeGoalCriterion> {
        vec![NativeGoalCriterion {
            id: "acceptance".into(),
            description: "Verify behavior".into(),
            evidence: None,
        }]
    }
    fn cut_mutation(action: &str) -> (u64, NativeGoalMutation) {
        match action {
            "pause" => (0, NativeGoalMutation::PauseByUser),
            "resume" => (1, NativeGoalMutation::ResumeByUser),
            "evidence" => (
                0,
                NativeGoalMutation::RecordEvidence {
                    criterion_id: "acceptance".into(),
                    evidence: NativeGoalEvidence {
                        turn_id: "fixture-turn".into(),
                        summary: "Durable terminal marker fixture".into(),
                    },
                },
            ),
            "complete" => (1, NativeGoalMutation::Complete),
            _ => panic!("unknown action"),
        }
    }

    #[test]
    fn goal_publication_process_cuts_reopen_exact_state_and_retry_one_revision() {
        for action in ["create", "pause", "resume", "evidence", "complete"] {
            for stage in ["temporary-synced", "renamed", "directory-synced"] {
                let root = tempfile::tempdir().unwrap();
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--ignored",
                        "--exact",
                        "native_goal::tests::goal_publication_cut_child",
                        "--nocapture",
                    ])
                    .env("OPENIDE_GOAL_CUT_ROOT", root.path())
                    .env("OPENIDE_GOAL_CUT_ACTION", action)
                    .env("OPENIDE_GOAL_CUT_STAGE", stage)
                    .output()
                    .unwrap();
                assert_eq!(
                    output.status.code(),
                    Some(86),
                    "{action}/{stage}: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
                let listing = sessions.list_with_diagnostics().unwrap();
                assert!(listing.diagnostics.is_empty());
                assert_eq!(listing.sessions.len(), 1);
                let session = &listing.sessions[0].session;
                let journal = listing.sessions[0].events.clone();
                let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
                let goals = NativeGoalStore::new(sessions.clone());
                let published = stage != "temporary-synced";
                let ledger_path = root
                    .path()
                    .join(format!("{}.native-goals.json", session.id));
                let published_bytes = fs::read(&ledger_path).ok();
                assert_eq!(
                    goals.recover_temporaries(&lease).unwrap(),
                    usize::from(!published)
                );
                assert_eq!(goals.recover_temporaries(&lease).unwrap(), 0);
                assert_eq!(fs::read(&ledger_path).ok(), published_bytes);
                let before = goals.list(&session.id).unwrap();
                let after = if action == "create" {
                    assert_eq!(before.len(), usize::from(published));
                    goals
                        .create(
                            &lease,
                            "publication",
                            "Complete the whole objective",
                            criteria(),
                        )
                        .unwrap()
                } else {
                    let (revision, mutation) = cut_mutation(action);
                    assert_eq!(before.len(), 1);
                    assert_eq!(before[0].revision, revision + u64::from(published));
                    assert_eq!(
                        before[0].status,
                        match (action, published) {
                            ("pause", true) | ("resume", false) => NativeGoalStatus::Paused,
                            ("complete", true) => NativeGoalStatus::Complete,
                            _ => NativeGoalStatus::Active,
                        }
                    );
                    assert_eq!(
                        before[0].criteria[0].evidence.is_some(),
                        action == "complete" || (action == "evidence" && published)
                    );
                    let receipt = goals
                        .committed_update(&lease, "goal-cut", "publication", revision, &mutation)
                        .unwrap();
                    assert_eq!(receipt.is_some(), published);
                    if let Some(receipt) = receipt {
                        assert_eq!(receipt.committed_revision, revision + 1);
                        assert_eq!(receipt.current_revision, revision + 1);
                    }
                    assert_eq!(fs::read(&ledger_path).ok(), published_bytes);
                    goals
                        .update(&lease, "goal-cut", "publication", revision, mutation)
                        .unwrap()
                };
                let path = root
                    .path()
                    .join(format!("{}.native-goals.json", session.id));
                let bytes = fs::read(&path).unwrap();
                let retry = if action == "create" {
                    goals
                        .create(
                            &lease,
                            "publication",
                            "Complete the whole objective",
                            criteria(),
                        )
                        .unwrap()
                } else {
                    let (revision, mutation) = cut_mutation(action);
                    goals
                        .update(&lease, "goal-cut", "publication", revision, mutation)
                        .unwrap()
                };
                assert_eq!(retry, after);
                assert_eq!(fs::read(&path).unwrap(), bytes);
                assert_eq!(sessions.load(&session.id).unwrap().unwrap().events, journal);
                drop(goals);
                drop(lease);
                let reopened =
                    NativeGoalStore::new(Arc::new(SessionStore::open(root.path()).unwrap()));
                assert_eq!(reopened.list(&session.id).unwrap(), vec![after]);
            }
        }
    }

    #[test]
    #[ignore = "subprocess helper for native goal publication cuts"]
    fn goal_publication_cut_child() {
        let Some(root) = std::env::var_os("OPENIDE_GOAL_CUT_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let action = std::env::var("OPENIDE_GOAL_CUT_ACTION").unwrap();
        let sessions = Arc::new(SessionStore::open(&root).unwrap());
        let session = sessions.create(&root, "native").unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let goals = NativeGoalStore::new(sessions.clone());
        if action == "create" {
            goals
                .create(
                    &lease,
                    "publication",
                    "Complete the whole objective",
                    criteria(),
                )
                .unwrap();
            panic!("cut not reached");
        }
        goals
            .create(
                &lease,
                "goal-cut",
                "Complete the whole objective",
                criteria(),
            )
            .unwrap();
        if action == "resume" {
            goals
                .update(
                    &lease,
                    "goal-cut",
                    "seed-pause",
                    0,
                    NativeGoalMutation::PauseByUser,
                )
                .unwrap();
        }
        if action == "evidence" || action == "complete" {
            // A committed core terminal journal fixture. Actual model execution
            // is covered separately by the harness integration acceptance.
            let admission = sessions
                .admit_native_turn(
                    &session.id,
                    "fixture-turn",
                    "Fixture",
                    "fixture",
                    crate::TurnMode::Build,
                )
                .unwrap();
            sessions
                .append_native_event(&crate::AgentEvent::new(
                    &session.id,
                    admission.sequence + 1,
                    AgentEventKind::TurnStarted {
                        prompt: "Fixture".into(),
                    },
                ))
                .unwrap();
            sessions
                .append_native_event(&crate::AgentEvent::new(
                    &session.id,
                    admission.sequence + 2,
                    AgentEventKind::TurnCompleted {
                        response: "Fixture".into(),
                        steps: 1,
                    },
                ))
                .unwrap();
        }
        if action == "complete" {
            let (_, mutation) = cut_mutation("evidence");
            goals
                .update(&lease, "goal-cut", "seed-evidence", 0, mutation)
                .unwrap();
        }
        let (revision, mutation) = cut_mutation(&action);
        goals
            .update(&lease, "goal-cut", "publication", revision, mutation)
            .unwrap();
        panic!("cut not reached");
    }

    #[test]
    fn startup_goal_recovery_skips_busy_reports_each_bad_store_and_continues() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
        let goals = NativeGoalStore::new(sessions.clone());
        let good = sessions.create(root.path(), "native").unwrap();
        let busy = sessions.create(root.path(), "native").unwrap();
        let corrupt = sessions.create(root.path(), "native").unwrap();
        for session in [&good, &busy, &corrupt] {
            let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
            goals
                .create(&lease, "goal", "Objective", criteria())
                .unwrap();
        }
        let temporary = |id: &str| {
            let path = root
                .path()
                .join(format!(".{id}.native-goals.{}.tmp", Uuid::new_v4()));
            fs::write(&path, b"unpublished").unwrap();
            path
        };
        let good_temp = temporary(&good.id);
        let busy_temp = temporary(&busy.id);
        let corrupt_temp = temporary(&corrupt.id);
        let absent_temp = temporary("missing-session");
        let path = root
            .path()
            .join(format!("{}.native-goals.json", corrupt.id));
        let valid = fs::read(&path).unwrap();
        fs::write(&path, b"{broken").unwrap();
        let nonnative = sessions.create(root.path(), "fixture-external").unwrap();
        let nonnative_temp = temporary(&nonnative.id);
        let busy_lease = sessions.try_acquire_native_lease(&busy.id).unwrap();
        let report = goals.recover_idle_sessions().unwrap();
        assert_eq!(report.removed_temporaries, 1);
        assert_eq!(report.busy_session_ids, vec![busy.id.clone()]);
        assert_eq!(report.diagnostics.len(), 3);
        for id in [
            corrupt.id.as_str(),
            nonnative.id.as_str(),
            "missing-session",
        ] {
            assert!(report
                .diagnostics
                .iter()
                .any(|d| d.session_id == id && !d.error.is_empty()));
        }
        assert!(!good_temp.exists());
        assert!(busy_temp.exists());
        assert!(corrupt_temp.exists());
        assert!(absent_temp.exists());
        assert!(nonnative_temp.exists());
        drop(busy_lease);
        fs::write(&path, &valid).unwrap();
        let recovered = NativeGoalStore::new(Arc::new(SessionStore::open(root.path()).unwrap()))
            .recover_idle_sessions()
            .unwrap();
        assert_eq!(recovered.removed_temporaries, 2);
        assert!(recovered.busy_session_ids.is_empty());
        assert_eq!(recovered.diagnostics.len(), 2);
        assert!(!busy_temp.exists());
        assert!(!corrupt_temp.exists());
        assert_eq!(fs::read(path).unwrap(), valid);
        assert_eq!(
            goals.recover_idle_sessions().unwrap().removed_temporaries,
            0
        );
    }

    #[test]
    fn goal_orphan_recovery_is_session_scoped_and_preserves_corrupt_evidence() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
        let session = sessions.create(root.path(), "native").unwrap();
        let other = sessions.create(root.path(), "native").unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let goals = NativeGoalStore::new(sessions.clone());
        goals
            .create(&lease, "goal", "Objective", criteria())
            .unwrap();
        let orphan = root.path().join(format!(
            ".{}.native-goals.{}.tmp",
            session.id,
            Uuid::new_v4()
        ));
        fs::write(&orphan, b"unpublished").unwrap();
        let foreign =
            root.path()
                .join(format!(".{}.native-goals.{}.tmp", other.id, Uuid::new_v4()));
        fs::write(&foreign, b"other session").unwrap();
        let notes = root
            .path()
            .join(format!(".{}.native-goals.notes.tmp", session.id));
        fs::write(&notes, b"notes").unwrap();
        let directory = root.path().join(format!(
            ".{}.native-goals.{}.tmp",
            session.id,
            Uuid::new_v4()
        ));
        fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        let link = {
            let link = root.path().join(format!(
                ".{}.native-goals.{}.tmp",
                session.id,
                Uuid::new_v4()
            ));
            std::os::unix::fs::symlink(&foreign, &link).unwrap();
            link
        };
        let path = root
            .path()
            .join(format!("{}.native-goals.json", session.id));
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"{broken").unwrap();
        assert!(goals.recover_temporaries(&lease).is_err());
        assert!(orphan.exists());
        fs::write(&path, &original).unwrap();
        assert_eq!(goals.recover_temporaries(&lease).unwrap(), 1);
        assert!(!orphan.exists());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(foreign.exists());
        assert!(notes.exists());
        assert!(directory.is_dir());
        #[cfg(unix)]
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
        fs::write(&orphan, b"another interrupted publication").unwrap();
        goals
            .update(&lease, "goal", "pause", 0, NativeGoalMutation::PauseByUser)
            .unwrap();
        assert!(!orphan.exists());
        assert_eq!(goals.recover_temporaries(&lease).unwrap(), 0);
    }

    #[test]
    fn goals_reopen_immutable_pause_resume_and_retries_without_new_journal_events() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
        let session = sessions.create(root.path(), "native").unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let store = NativeGoalStore::new(sessions.clone());
        let goal = store
            .create(&lease, "goal", "Complete the whole objective", criteria())
            .unwrap();
        assert!(matches!(
            store.create(&lease, "other", "Other", criteria()),
            Err(NativeGoalError::Unfinished)
        ));
        assert!(matches!(
            store.create(&lease, "goal", "Changed scope", criteria()),
            Err(NativeGoalError::Conflict)
        ));
        store
            .update(&lease, "goal", "pause", 0, NativeGoalMutation::PauseByUser)
            .unwrap();
        let active = store
            .update(
                &lease,
                "goal",
                "resume",
                1,
                NativeGoalMutation::ResumeByUser,
            )
            .unwrap();
        assert_eq!(active.revision, 2);
        let path = root
            .path()
            .join(format!("{}.native-goals.json", session.id));
        let bytes = fs::read(&path).unwrap();
        let other = NativeGoalStore::new(Arc::new(SessionStore::open(root.path()).unwrap()));
        assert_eq!(
            other
                .update(&lease, "goal", "pause", 0, NativeGoalMutation::PauseByUser)
                .unwrap(),
            active
        );
        assert!(matches!(
            other.update(&lease, "goal", "pause", 2, NativeGoalMutation::PauseByUser),
            Err(NativeGoalError::Conflict)
        ));
        assert_eq!(
            other
                .create(&lease, "goal", &goal.objective, criteria())
                .unwrap(),
            active
        );
        let receipt = other
            .committed_update(&lease, "goal", "pause", 0, &NativeGoalMutation::PauseByUser)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.committed_revision, 1);
        assert_eq!(receipt.current_revision, 2);
        assert!(other
            .committed_update(
                &lease,
                "goal",
                "absent",
                2,
                &NativeGoalMutation::PauseByUser
            )
            .unwrap()
            .is_none());
        assert!(matches!(
            other.committed_update(
                &lease,
                "goal",
                "pause",
                0,
                &NativeGoalMutation::ResumeByUser
            ),
            Err(NativeGoalError::Conflict)
        ));
        assert_eq!(other.list(&session.id).unwrap(), vec![active]);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(sessions
            .load(&session.id)
            .unwrap()
            .unwrap()
            .events
            .is_empty());
        drop(lease);
        sessions.delete(&session.id).unwrap();
        assert!(!path.exists());
    }
    #[test]
    fn goal_revision_race_has_one_winner_and_foreign_lease_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
        let session = sessions.create(root.path(), "native").unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let store = NativeGoalStore::new(sessions.clone());
        store
            .create(&lease, "goal", "Objective", criteria())
            .unwrap();
        let other = NativeGoalStore::new(Arc::new(SessionStore::open(root.path()).unwrap()));
        let barrier = std::sync::Barrier::new(2);
        let results = std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                barrier.wait();
                store.update(&lease, "goal", "a", 0, NativeGoalMutation::PauseByUser)
            });
            let b = scope.spawn(|| {
                barrier.wait();
                other.update(&lease, "goal", "b", 0, NativeGoalMutation::ResumeByUser)
            });
            [a.join().unwrap(), b.join().unwrap()]
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Err(NativeGoalError::Conflict)))
                .count(),
            1
        );
        let foreign_store =
            NativeGoalStore::new(Arc::new(SessionStore::open(foreign.path()).unwrap()));
        assert!(matches!(
            foreign_store.create(&lease, "goal", "Objective", criteria()),
            Err(NativeGoalError::Store(StoreError::NativeLeaseStoreMismatch))
        ));
    }
    #[test]
    fn invalid_goal_criteria_and_corrupt_ledger_do_not_mutate_store() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
        let session = sessions.create(root.path(), "native").unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let store = NativeGoalStore::new(sessions);
        assert!(matches!(
            store.create(&lease, "goal", "Objective", vec![]),
            Err(NativeGoalError::Invalid)
        ));
        assert!(matches!(
            store.create(
                &lease,
                "goal",
                "Objective",
                vec![criteria()[0].clone(), criteria()[0].clone()]
            ),
            Err(NativeGoalError::Invalid)
        ));
        let path = root
            .path()
            .join(format!("{}.native-goals.json", session.id));
        fs::write(&path, b"{broken").unwrap();
        assert!(store
            .create(&lease, "goal", "Objective", criteria())
            .is_err());
        assert!(store
            .committed_update(&lease, "goal", "pause", 0, &NativeGoalMutation::PauseByUser)
            .is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{broken");
        fs::remove_file(&path).unwrap();
        store
            .create(&lease, "goal", "Objective", criteria())
            .unwrap();
        let mut ledger: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        ledger["goals"][0]["status"] = serde_json::json!("paused");
        let bytes = serde_json::to_vec(&ledger).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(
            store.list(&session.id),
            Err(NativeGoalError::Invalid)
        ));
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}
