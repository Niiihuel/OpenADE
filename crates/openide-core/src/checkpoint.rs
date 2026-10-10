//! A bounded checkpoint candidate store. A candidate becomes authoritative
//! only after a separate event-journal commit references its digest.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use thiserror::Error;
use uuid::Uuid;

pub const CHECKPOINT_VERSION: u32 = 1;
pub const MAX_CHECKPOINT_FILE_BYTES: u64 = 1024 * 1024;
pub const MAX_CHECKPOINT_SUMMARY_BYTES: usize = 128 * 1024;
pub const MAX_RETAINED_USER_MESSAGES: usize = 16;
pub const MAX_RETAINED_USER_MESSAGE_BYTES: usize = 32 * 1024;
const MAX_ID_BYTES: usize = 128;
const MAX_PROVIDER_OR_MODEL_BYTES: usize = 256;
const MAX_FORMAT_KEY_BYTES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointCompatibility {
    pub provider: String,
    pub model: String,
    /// Changes when the model input format or summary contract changes.
    pub context_format_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedUserMessage {
    pub event_sequence: u64,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointBudgetSnapshot {
    /// Independent maximum input; omitted to preserve legacy checkpoint digests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limit_tokens: Option<u64>,
    pub context_window_tokens: u64,
    pub estimated_input_tokens: u64,
    pub observed_input_tokens: Option<u64>,
    pub output_reserve_tokens: u64,
    pub tool_schema_reserve_tokens: u64,
    /// Manual compaction may be requested for a model without published
    /// limits. Zeroes in the limit fields mean "unknown" only in this case.
    /// Omitted when false so existing checkpoint payload digests stay valid.
    #[serde(default, skip_serializing_if = "is_false")]
    pub limits_unverified: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl CheckpointBudgetSnapshot {
    pub fn unverified(estimated_input_tokens: u64, observed_input_tokens: Option<u64>) -> Self {
        Self {
            input_limit_tokens: None,
            context_window_tokens: 0,
            estimated_input_tokens,
            observed_input_tokens,
            output_reserve_tokens: 0,
            tool_schema_reserve_tokens: 0,
            limits_unverified: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub session_id: String,
    pub checkpoint_id: String,
    /// Inclusive last event summarized by this checkpoint.
    pub covered_event_sequence: u64,
    pub compatibility: CheckpointCompatibility,
    pub summary: String,
    pub retained_user_messages: Vec<RetainedUserMessage>,
    pub budget: CheckpointBudgetSnapshot,
}

/// Data to record in the event journal after `write_candidate` succeeds.
/// Its digest binds the journal marker to one exact checkpoint payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointReference {
    pub session_id: String,
    pub checkpoint_id: String,
    pub covered_event_sequence: u64,
    pub compatibility: CheckpointCompatibility,
    pub content_sha256: String,
}

#[derive(Debug, Error)]
pub enum CheckpointError {
    #[error("checkpoint I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("checkpoint encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid checkpoint field: {0}")]
    InvalidField(&'static str),
    #[error("checkpoint limit exceeded: {0}")]
    Limit(&'static str),
    #[error("unsupported checkpoint format version: {0}")]
    UnsupportedVersion(u32),
    #[error("checkpoint checksum does not match its payload")]
    ChecksumMismatch,
    #[error("checkpoint does not match the committed reference")]
    ReferenceMismatch,
    #[error("checkpoint named by a committed reference is missing")]
    MissingReferencedCheckpoint,
    #[error("checkpoint id already contains different content")]
    ConflictingCheckpoint,
    #[error("checkpoint file is a symbolic link")]
    SymbolicLink,
    #[error("checkpoint store lock is poisoned")]
    PoisonedLock,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointEnvelope {
    version: u32,
    content_sha256: String,
    payload: Checkpoint,
}

#[derive(Deserialize)]
struct CheckpointVersion {
    version: u32,
}

/// Files are kept under `root/checkpoints/<session>/<checkpoint>.json`.
/// Temporary files are never scanned or adopted as checkpoints.
pub struct CheckpointFileStore {
    root: PathBuf,
    write_lock: Mutex<()>,
}

impl CheckpointFileStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, CheckpointError> {
        let root = root.into().join("checkpoints");
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            write_lock: Mutex::new(()),
        })
    }

    /// Sync a complete candidate and publish its final name atomically.
    /// The returned reference must be committed to the event journal before
    /// this checkpoint can be used for replay. Repeating an identical write
    /// is safe; reusing an ID with different content is rejected.
    pub fn write_candidate(
        &self,
        checkpoint: &Checkpoint,
    ) -> Result<CheckpointReference, CheckpointError> {
        validate_checkpoint(checkpoint)?;
        let payload_bytes = serde_json::to_vec(checkpoint)?;
        let digest = sha256_hex(&payload_bytes);
        let reference = CheckpointReference {
            session_id: checkpoint.session_id.clone(),
            checkpoint_id: checkpoint.checkpoint_id.clone(),
            covered_event_sequence: checkpoint.covered_event_sequence,
            compatibility: checkpoint.compatibility.clone(),
            content_sha256: digest.clone(),
        };
        let encoded = serde_json::to_vec(&CheckpointEnvelope {
            version: CHECKPOINT_VERSION,
            content_sha256: digest,
            payload: checkpoint.clone(),
        })?;
        if encoded.len() as u64 > MAX_CHECKPOINT_FILE_BYTES {
            return Err(CheckpointError::Limit("checkpoint file bytes"));
        }

        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| CheckpointError::PoisonedLock)?;
        let path = self.path(&checkpoint.session_id, &checkpoint.checkpoint_id)?;
        let parent = path
            .parent()
            .ok_or(CheckpointError::InvalidField("checkpoint path"))?;
        fs::create_dir_all(parent)?;
        if let Some(existing) = read_bounded(&path)? {
            if existing == encoded {
                return Ok(reference);
            }
            return Err(CheckpointError::ConflictingCheckpoint);
        }

        let temporary = parent.join(format!(
            ".{}.{}.tmp",
            checkpoint.checkpoint_id,
            Uuid::new_v4()
        ));
        let result = (|| -> Result<(), CheckpointError> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            drop(file);

            // A hard link publishes a fully written file without replacing a
            // checkpoint concurrently created by another process.
            match fs::hard_link(&temporary, &path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if read_bounded(&path)?.as_deref() != Some(encoded.as_slice()) {
                        return Err(CheckpointError::ConflictingCheckpoint);
                    }
                }
                Err(error) => return Err(error.into()),
            }
            fs::remove_file(&temporary)?;
            #[cfg(unix)]
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        Ok(reference)
    }

    /// Load only the candidate named by a journal-confirmed reference.
    /// `None` means replay found no commit and deliberately ignores all
    /// orphaned checkpoint files. A corrupt referenced file is an error;
    /// callers must not silently replay from its incomplete contents.
    pub fn load_after_commit(
        &self,
        committed_reference: Option<&CheckpointReference>,
    ) -> Result<Option<Checkpoint>, CheckpointError> {
        let Some(reference) = committed_reference else {
            return Ok(None);
        };
        validate_reference(reference)?;
        let path = self.path(&reference.session_id, &reference.checkpoint_id)?;
        let bytes = read_bounded(&path)?.ok_or(CheckpointError::MissingReferencedCheckpoint)?;
        let version: CheckpointVersion = serde_json::from_slice(&bytes)?;
        if version.version != CHECKPOINT_VERSION {
            return Err(CheckpointError::UnsupportedVersion(version.version));
        }
        let envelope: CheckpointEnvelope = serde_json::from_slice(&bytes)?;
        validate_checkpoint(&envelope.payload)?;
        let digest = sha256_hex(&serde_json::to_vec(&envelope.payload)?);
        if envelope.content_sha256 != digest {
            return Err(CheckpointError::ChecksumMismatch);
        }
        let actual_reference = CheckpointReference {
            session_id: envelope.payload.session_id.clone(),
            checkpoint_id: envelope.payload.checkpoint_id.clone(),
            covered_event_sequence: envelope.payload.covered_event_sequence,
            compatibility: envelope.payload.compatibility.clone(),
            content_sha256: digest,
        };
        if &actual_reference != reference {
            return Err(CheckpointError::ReferenceMismatch);
        }
        Ok(Some(envelope.payload))
    }

    fn path(&self, session_id: &str, checkpoint_id: &str) -> Result<PathBuf, CheckpointError> {
        validate_id(session_id, "session_id")?;
        validate_id(checkpoint_id, "checkpoint_id")?;
        Ok(self
            .root
            .join(session_id)
            .join(format!("{checkpoint_id}.json")))
    }
}

fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, CheckpointError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(CheckpointError::SymbolicLink);
    }
    if metadata.len() > MAX_CHECKPOINT_FILE_BYTES {
        return Err(CheckpointError::Limit("checkpoint file bytes"));
    }
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_CHECKPOINT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CHECKPOINT_FILE_BYTES {
        return Err(CheckpointError::Limit("checkpoint file bytes"));
    }
    Ok(Some(bytes))
}

fn validate_checkpoint(checkpoint: &Checkpoint) -> Result<(), CheckpointError> {
    validate_id(&checkpoint.session_id, "session_id")?;
    validate_id(&checkpoint.checkpoint_id, "checkpoint_id")?;
    validate_compatibility(&checkpoint.compatibility)?;
    if checkpoint.covered_event_sequence == 0 {
        return Err(CheckpointError::InvalidField("covered_event_sequence"));
    }
    if checkpoint.summary.len() > MAX_CHECKPOINT_SUMMARY_BYTES {
        return Err(CheckpointError::Limit("summary bytes"));
    }
    if checkpoint.retained_user_messages.len() > MAX_RETAINED_USER_MESSAGES {
        return Err(CheckpointError::Limit("retained user message count"));
    }
    let mut previous_sequence = 0;
    for message in &checkpoint.retained_user_messages {
        if message.content.len() > MAX_RETAINED_USER_MESSAGE_BYTES {
            return Err(CheckpointError::Limit("retained user message bytes"));
        }
        if message.event_sequence <= previous_sequence
            || message.event_sequence > checkpoint.covered_event_sequence
        {
            return Err(CheckpointError::InvalidField(
                "retained user message event sequence",
            ));
        }
        previous_sequence = message.event_sequence;
    }
    if checkpoint.budget.limits_unverified {
        if checkpoint.budget.context_window_tokens != 0
            || checkpoint.budget.input_limit_tokens.is_some()
            || checkpoint.budget.output_reserve_tokens != 0
            || checkpoint.budget.tool_schema_reserve_tokens != 0
        {
            return Err(CheckpointError::InvalidField("unverified budget limits"));
        }
    } else {
        if checkpoint.budget.input_limit_tokens == Some(0) {
            return Err(CheckpointError::InvalidField("input_limit_tokens"));
        }
        if checkpoint.budget.context_window_tokens == 0
            && checkpoint.budget.input_limit_tokens.is_none()
        {
            return Err(CheckpointError::InvalidField("context_window_tokens"));
        }
        if checkpoint.budget.output_reserve_tokens == 0 {
            return Err(CheckpointError::InvalidField("output_reserve_tokens"));
        }
    }
    Ok(())
}

fn validate_reference(reference: &CheckpointReference) -> Result<(), CheckpointError> {
    validate_id(&reference.session_id, "session_id")?;
    validate_id(&reference.checkpoint_id, "checkpoint_id")?;
    validate_compatibility(&reference.compatibility)?;
    if reference.covered_event_sequence == 0 {
        return Err(CheckpointError::InvalidField("covered_event_sequence"));
    }
    if reference.content_sha256.len() != 64
        || !reference
            .content_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(CheckpointError::InvalidField("content_sha256"));
    }
    Ok(())
}

fn validate_id(value: &str, name: &'static str) -> Result<(), CheckpointError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(CheckpointError::InvalidField(name));
    }
    Ok(())
}

fn validate_compatibility(value: &CheckpointCompatibility) -> Result<(), CheckpointError> {
    for (name, text, limit) in [
        (
            "provider",
            value.provider.as_str(),
            MAX_PROVIDER_OR_MODEL_BYTES,
        ),
        ("model", value.model.as_str(), MAX_PROVIDER_OR_MODEL_BYTES),
        (
            "context_format_key",
            value.context_format_key.as_str(),
            MAX_FORMAT_KEY_BYTES,
        ),
    ] {
        if text.trim().is_empty() || text.len() > limit || text.chars().any(char::is_control) {
            return Err(CheckpointError::InvalidField(name));
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(checkpoint_id: &str) -> Checkpoint {
        Checkpoint {
            session_id: "session-1".into(),
            checkpoint_id: checkpoint_id.into(),
            covered_event_sequence: 42,
            compatibility: CheckpointCompatibility {
                provider: "openai".into(),
                model: "gpt-example".into(),
                context_format_key: "responses-v1".into(),
            },
            summary: "Decisions and unresolved work.".into(),
            retained_user_messages: vec![
                RetainedUserMessage {
                    event_sequence: 4,
                    content: "Initial task".into(),
                },
                RetainedUserMessage {
                    event_sequence: 31,
                    content: "Latest correction".into(),
                },
            ],
            budget: CheckpointBudgetSnapshot {
                input_limit_tokens: None,
                context_window_tokens: 128_000,
                estimated_input_tokens: 93_000,
                observed_input_tokens: Some(24_000),
                output_reserve_tokens: 4_096,
                tool_schema_reserve_tokens: 1_024,
                limits_unverified: false,
            },
        }
    }

    #[test]
    fn independent_input_limit_persists_without_inventing_a_context_window() {
        let dir = tempfile::tempdir().unwrap();
        let mut checkpoint = sample("input-limit");
        checkpoint.budget.context_window_tokens = 0;
        checkpoint.budget.input_limit_tokens = Some(2_000);
        checkpoint.budget.output_reserve_tokens = 4_096;
        let store = CheckpointFileStore::new(dir.path()).unwrap();
        let reference = store.write_candidate(&checkpoint).unwrap();
        drop(store);
        let reopened = CheckpointFileStore::new(dir.path()).unwrap();
        assert_eq!(
            reopened.load_after_commit(Some(&reference)).unwrap(),
            Some(checkpoint.clone())
        );
        checkpoint.checkpoint_id = "bad-zero-input".into();
        checkpoint.budget.input_limit_tokens = Some(0);
        assert!(reopened.write_candidate(&checkpoint).is_err());
        checkpoint.budget.input_limit_tokens = None;
        assert!(reopened.write_candidate(&checkpoint).is_err());
        checkpoint.budget = CheckpointBudgetSnapshot::unverified(100, None);
        checkpoint.budget.input_limit_tokens = Some(2_000);
        assert!(reopened.write_candidate(&checkpoint).is_err());
    }

    #[test]
    fn round_trip_and_idempotent_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = CheckpointFileStore::new(dir.path()).unwrap();
        let checkpoint = sample("checkpoint-1");
        let reference = store.write_candidate(&checkpoint).unwrap();
        assert_eq!(store.write_candidate(&checkpoint).unwrap(), reference);
        assert_eq!(
            store.load_after_commit(Some(&reference)).unwrap(),
            Some(checkpoint.clone())
        );
        let mut different = checkpoint;
        different.summary.push_str(" changed");
        assert!(matches!(
            store.write_candidate(&different),
            Err(CheckpointError::ConflictingCheckpoint)
        ));
    }

    #[test]
    fn unverified_limits_round_trip_without_changing_legacy_payloads() {
        let legacy = sample("legacy");
        let legacy_json = serde_json::to_string(&legacy).unwrap();
        assert!(!legacy_json.contains("limits_unverified"));
        assert_eq!(
            serde_json::from_str::<Checkpoint>(&legacy_json).unwrap(),
            legacy
        );

        let dir = tempfile::tempdir().unwrap();
        let store = CheckpointFileStore::new(dir.path()).unwrap();
        let mut unknown = sample("unknown");
        unknown.budget = CheckpointBudgetSnapshot::unverified(93_000, Some(24_000));
        let reference = store.write_candidate(&unknown).unwrap();
        assert_eq!(
            store.load_after_commit(Some(&reference)).unwrap(),
            Some(unknown.clone())
        );
        unknown.checkpoint_id = "invalid-mixed".into();
        unknown.budget.context_window_tokens = 128_000;
        assert!(matches!(
            store.write_candidate(&unknown),
            Err(CheckpointError::InvalidField("unverified budget limits"))
        ));
    }

    #[test]
    fn rejects_corrupt_or_mismatched_committed_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let store = CheckpointFileStore::new(dir.path()).unwrap();
        let checkpoint = sample("checkpoint-1");
        let reference = store.write_candidate(&checkpoint).unwrap();
        let path = store
            .path(&checkpoint.session_id, &checkpoint.checkpoint_id)
            .unwrap();
        let original = fs::read_to_string(&path).unwrap();
        fs::write(&path, original.replace("Decisions", "Alteredxx")).unwrap();
        assert!(matches!(
            store.load_after_commit(Some(&reference)),
            Err(CheckpointError::ChecksumMismatch)
        ));

        fs::write(&path, original).unwrap();
        let mut wrong_model = reference.clone();
        wrong_model.compatibility.model = "other-model".into();
        assert!(matches!(
            store.load_after_commit(Some(&wrong_model)),
            Err(CheckpointError::ReferenceMismatch)
        ));
        fs::write(&path, b"{").unwrap();
        assert!(matches!(
            store.load_after_commit(Some(&reference)),
            Err(CheckpointError::Json(_))
        ));
    }

    #[test]
    fn orphan_files_are_not_selected_without_a_commit_reference() {
        let dir = tempfile::tempdir().unwrap();
        let store = CheckpointFileStore::new(dir.path()).unwrap();
        let orphan = sample("orphan");
        store.write_candidate(&orphan).unwrap();
        let committed = sample("committed");
        let reference = store.write_candidate(&committed).unwrap();
        let parent = store.path("session-1", "committed").unwrap();
        fs::write(
            parent.parent().unwrap().join(".interrupted.tmp"),
            b"garbage",
        )
        .unwrap();

        assert!(store.load_after_commit(None).unwrap().is_none());
        assert_eq!(
            store.load_after_commit(Some(&reference)).unwrap(),
            Some(committed)
        );
        let mut missing = reference;
        missing.checkpoint_id = "missing".into();
        assert!(matches!(
            store.load_after_commit(Some(&missing)),
            Err(CheckpointError::MissingReferencedCheckpoint)
        ));
    }

    #[test]
    fn enforces_content_and_file_bounds_before_loading() {
        let dir = tempfile::tempdir().unwrap();
        let store = CheckpointFileStore::new(dir.path()).unwrap();
        let mut checkpoint = sample("checkpoint-1");
        checkpoint.summary = "x".repeat(MAX_CHECKPOINT_SUMMARY_BYTES + 1);
        assert!(matches!(
            store.write_candidate(&checkpoint),
            Err(CheckpointError::Limit("summary bytes"))
        ));
        checkpoint.summary.clear();
        checkpoint.retained_user_messages = (1..=MAX_RETAINED_USER_MESSAGES + 1)
            .map(|sequence| RetainedUserMessage {
                event_sequence: sequence as u64,
                content: "message".into(),
            })
            .collect();
        assert!(matches!(
            store.write_candidate(&checkpoint),
            Err(CheckpointError::Limit("retained user message count"))
        ));
        let checkpoint = sample("checkpoint-1");
        let reference = store.write_candidate(&checkpoint).unwrap();
        let path = store
            .path(&checkpoint.session_id, &checkpoint.checkpoint_id)
            .unwrap();
        let file = File::options().write(true).open(path).unwrap();
        file.set_len(MAX_CHECKPOINT_FILE_BYTES + 1).unwrap();
        assert!(matches!(
            store.load_after_commit(Some(&reference)),
            Err(CheckpointError::Limit("checkpoint file bytes"))
        ));
    }

    #[test]
    fn rejects_invalid_sequences_ids_and_future_versions() {
        let dir = tempfile::tempdir().unwrap();
        let store = CheckpointFileStore::new(dir.path()).unwrap();
        let mut checkpoint = sample("../escape");
        assert!(matches!(
            store.write_candidate(&checkpoint),
            Err(CheckpointError::InvalidField("checkpoint_id"))
        ));
        checkpoint.checkpoint_id = "checkpoint-1".into();
        checkpoint.retained_user_messages[1].event_sequence = 2;
        assert!(matches!(
            store.write_candidate(&checkpoint),
            Err(CheckpointError::InvalidField(
                "retained user message event sequence"
            ))
        ));

        let checkpoint = sample("checkpoint-1");
        let reference = store.write_candidate(&checkpoint).unwrap();
        let path = store
            .path(&checkpoint.session_id, &checkpoint.checkpoint_id)
            .unwrap();
        let original = fs::read_to_string(&path).unwrap();
        fs::write(path, original.replacen("\"version\":1", "\"version\":2", 1)).unwrap();
        assert!(matches!(
            store.load_after_commit(Some(&reference)),
            Err(CheckpointError::UnsupportedVersion(2))
        ));
    }
}
