//! Immutable provenance for a native automation's original read-only request.
//!
//! The host owns the versioned authority JSON schema. Core validates its exact
//! bytes and binds the snapshot to session metadata and the first admission;
//! it never interprets the snapshot as permission to execute an effect.

use crate::{Session, TurnMode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

pub const NATIVE_AUTOMATION_ORIGIN_VERSION: u16 = 1;
pub const NATIVE_AUTOMATION_AUTHORITY_SNAPSHOT_VERSION: u16 = 1;
pub const MAX_NATIVE_AUTOMATION_AUTHORITY_SNAPSHOT_BYTES: usize = 512 * 1024;
const MAX_FIRST_PROMPT_BYTES: usize = 64 * 1024;
const MAX_RUN_ID_BYTES: usize = 256;
pub(crate) const NATIVE_AUTOMATION_SESSION_PREFIX: &str = "auto-";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAutomationAuthorityProfile {
    ReadOnlyV1,
    /// Edits inside the workspace and commands in the host's sandbox, as
    /// the host's snapshot of the profile records.
    WorkspaceWriteV1,
}

/// Private metadata, retained independently of the bounded scheduler ledger.
/// The host must exclude bearer credentials, enforce its own snapshot schema,
/// and reconstruct only this version's original read-only authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAutomationOrigin {
    pub version: u16,
    pub automation_id: String,
    pub run_id: String,
    pub scheduled_at_ms: i64,
    pub manual: bool,
    pub authority_profile: NativeAutomationAuthorityProfile,
    pub authority_snapshot_version: u16,
    pub authority_snapshot_digest: String,
    pub authority_snapshot_json: String,
    pub first_turn_id: String,
    pub first_request_digest: String,
}

#[derive(Debug, Error)]
pub enum NativeAutomationOriginError {
    #[error("native automation origin version {0} is unsupported")]
    UnsupportedVersion(u16),
    #[error("native automation authority snapshot version {0} is unsupported")]
    UnsupportedSnapshotVersion(u16),
    #[error("native automation ID, occurrence or schedule is invalid")]
    InvalidOccurrence,
    #[error("native automation authority JSON must be a bounded object")]
    InvalidSnapshot,
    #[error("native automation authority snapshot digest does not match its exact bytes")]
    SnapshotDigestMismatch,
    #[error("native automation first request is invalid or exceeds its bounds")]
    InvalidFirstRequest,
    #[error("native automation first turn ID does not match its occurrence")]
    FirstTurnMismatch,
    #[error("native automation workspace must be an absolute normalized Unicode path")]
    InvalidWorkspace,
    #[error("native automation origin belongs to a different session or workspace")]
    SessionBindingMismatch,
}

impl NativeAutomationOrigin {
    pub fn new(
        automation_id: impl Into<String>,
        run_id: impl Into<String>,
        scheduled_at_ms: i64,
        manual: bool,
        authority_snapshot_json: impl Into<String>,
        prompt: &str,
        model: &str,
    ) -> Result<Self, NativeAutomationOriginError> {
        Self::new_with_profile(
            NativeAutomationAuthorityProfile::ReadOnlyV1,
            automation_id,
            run_id,
            scheduled_at_ms,
            manual,
            authority_snapshot_json,
            prompt,
            model,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_profile(
        authority_profile: NativeAutomationAuthorityProfile,
        automation_id: impl Into<String>,
        run_id: impl Into<String>,
        scheduled_at_ms: i64,
        manual: bool,
        authority_snapshot_json: impl Into<String>,
        prompt: &str,
        model: &str,
    ) -> Result<Self, NativeAutomationOriginError> {
        let run_id = run_id.into();
        let authority_snapshot_json = authority_snapshot_json.into();
        let origin = Self {
            version: NATIVE_AUTOMATION_ORIGIN_VERSION,
            automation_id: automation_id.into(),
            first_turn_id: native_automation_first_turn_id(&run_id)?,
            run_id,
            scheduled_at_ms,
            manual,
            authority_profile,
            authority_snapshot_version: NATIVE_AUTOMATION_AUTHORITY_SNAPSHOT_VERSION,
            authority_snapshot_digest: native_automation_snapshot_digest(&authority_snapshot_json)?,
            authority_snapshot_json,
            first_request_digest: native_automation_request_digest(prompt, model, TurnMode::Build)?,
        };
        origin.validate()?;
        Ok(origin)
    }

    pub fn validate(&self) -> Result<(), NativeAutomationOriginError> {
        if self.version != NATIVE_AUTOMATION_ORIGIN_VERSION {
            return Err(NativeAutomationOriginError::UnsupportedVersion(
                self.version,
            ));
        }
        if self.authority_snapshot_version != NATIVE_AUTOMATION_AUTHORITY_SNAPSHOT_VERSION {
            return Err(NativeAutomationOriginError::UnsupportedSnapshotVersion(
                self.authority_snapshot_version,
            ));
        }
        if !canonical_random_uuid(&self.automation_id) || self.scheduled_at_ms <= 0 {
            return Err(NativeAutomationOriginError::InvalidOccurrence);
        }
        validate_run_id(&self.run_id)?;
        if self.manual {
            let prefix = format!("{}:manual:", self.automation_id);
            if self
                .run_id
                .strip_prefix(&prefix)
                .is_none_or(|id| !canonical_random_uuid(id))
            {
                return Err(NativeAutomationOriginError::InvalidOccurrence);
            }
        } else if self.run_id != format!("{}:{}", self.automation_id, self.scheduled_at_ms) {
            return Err(NativeAutomationOriginError::InvalidOccurrence);
        }
        if self.first_turn_id != native_automation_first_turn_id(&self.run_id)? {
            return Err(NativeAutomationOriginError::FirstTurnMismatch);
        }
        if !valid_digest(&self.first_request_digest) {
            return Err(NativeAutomationOriginError::InvalidFirstRequest);
        }
        if !valid_digest(&self.authority_snapshot_digest)
            || self.authority_snapshot_digest
                != native_automation_snapshot_digest(&self.authority_snapshot_json)?
        {
            return Err(NativeAutomationOriginError::SnapshotDigestMismatch);
        }
        Ok(())
    }

    /// Validate immutable metadata without rereading workspace files. A removed
    /// project remains readable; execution must separately canonicalize and
    /// verify the workspace before using it.
    pub fn validate_session(&self, session: &Session) -> Result<(), NativeAutomationOriginError> {
        self.validate()?;
        if session.backend != "native"
            || session.subagent.is_some()
            || session.native_automation.as_ref() != Some(self)
            || session.id != native_automation_session_id(&session.workspace, &self.run_id)?
        {
            return Err(NativeAutomationOriginError::SessionBindingMismatch);
        }
        Ok(())
    }
}

/// Stable admission ID; the scheduler's colon-containing run ID itself is not
/// a valid native turn ID. This helper does not consult the scheduler ledger.
pub fn native_automation_first_turn_id(
    run_id: &str,
) -> Result<String, NativeAutomationOriginError> {
    validate_run_id(run_id)?;
    Ok(format!(
        "automation-{}",
        digest(
            b"openide-native-automation-first-turn-v1\0",
            &[run_id.as_bytes()]
        )
    ))
}

/// Bind exact text and model bytes to the original text-only Build request.
pub fn native_automation_request_digest(
    prompt: &str,
    model: &str,
    mode: TurnMode,
) -> Result<String, NativeAutomationOriginError> {
    if mode != TurnMode::Build
        || prompt.trim().is_empty()
        || prompt.len() > MAX_FIRST_PROMPT_BYTES
        || model.trim().is_empty()
        || model.len() > crate::MAX_MODEL_ROUTE_MODEL_BYTES
        || model.chars().any(char::is_control)
    {
        return Err(NativeAutomationOriginError::InvalidFirstRequest);
    }
    Ok(digest(
        b"openide-native-automation-first-request-v1\0",
        &[prompt.as_bytes(), model.as_bytes(), b"build", b"text-only"],
    ))
}

/// Hash exact serialized host snapshot bytes, including intentional whitespace.
/// A checksum is not host schema validation or permission to execute an effect.
pub fn native_automation_snapshot_digest(
    snapshot_json: &str,
) -> Result<String, NativeAutomationOriginError> {
    if snapshot_json.is_empty()
        || snapshot_json.len() > MAX_NATIVE_AUTOMATION_AUTHORITY_SNAPSHOT_BYTES
        || serde_json::from_str::<serde_json::Value>(snapshot_json)
            .ok()
            .is_none_or(|value| !value.is_object())
    {
        return Err(NativeAutomationOriginError::InvalidSnapshot);
    }
    Ok(digest(
        b"openide-native-automation-authority-snapshot-v1\0",
        &[snapshot_json.as_bytes()],
    ))
}

/// Identity from the stored canonical workspace spelling and occurrence. This
/// helper performs no filesystem access, preserving history after relocation.
pub fn native_automation_session_id(
    workspace: &Path,
    run_id: &str,
) -> Result<String, NativeAutomationOriginError> {
    validate_run_id(run_id)?;
    let workspace_text = workspace
        .to_str()
        .ok_or(NativeAutomationOriginError::InvalidWorkspace)?;
    let normalized = workspace.components().collect::<PathBuf>();
    if !workspace.is_absolute()
        || normalized.as_os_str() != workspace.as_os_str()
        || workspace
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err(NativeAutomationOriginError::InvalidWorkspace);
    }
    Ok(format!(
        "{NATIVE_AUTOMATION_SESSION_PREFIX}{}",
        digest(
            b"openide-native-automation-session-v1\0",
            &[workspace_text.as_bytes(), run_id.as_bytes()]
        )
    ))
}

fn validate_run_id(run_id: &str) -> Result<(), NativeAutomationOriginError> {
    if run_id.is_empty()
        || run_id.len() > MAX_RUN_ID_BYTES
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b':' | b'_'))
    {
        return Err(NativeAutomationOriginError::InvalidOccurrence);
    }
    Ok(())
}

fn canonical_random_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| {
        id.get_version() == Some(uuid::Version::Random) && id.hyphenated().to_string() == value
    })
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn digest(domain: &[u8], fields: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
#[path = "native_automation_origin_tests.rs"]
mod tests;
