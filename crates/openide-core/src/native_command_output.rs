//! Bounded, durable observations of command output. Registration never proves
//! that a process started, and reopening a capture never authorizes execution.
use crate::{Session, SessionStore, StoreError};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use uuid::Uuid;

pub const NATIVE_COMMAND_OUTPUT_VERSION: u32 = 1;
pub const MAX_NATIVE_COMMAND_OUTPUT_RAW_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NATIVE_COMMAND_OUTPUT_RECORDS: usize = 4096;
pub const MAX_NATIVE_COMMAND_OUTPUT_COMMANDS: usize = 64;
pub const MAX_NATIVE_COMMAND_OUTPUT_CHUNK_BYTES: usize = 8 * 1024;
pub const MAX_NATIVE_COMMAND_OUTPUT_APPEND_BYTES: usize = 64 * 1024;
pub const MAX_NATIVE_COMMAND_OUTPUT_FILE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_NATIVE_COMMAND_OUTPUT_PAGE_BYTES: usize = 256 * 1024;
pub const MAX_NATIVE_COMMAND_OUTPUT_PAGE_RECORDS: usize = 256;
pub const MAX_NATIVE_COMMAND_OUTPUT_PAGE_SERIALIZED_BYTES: usize = 1024 * 1024;
const MAX_ARGV_BYTES: usize = 32 * 1024;
const MAX_ARGV_ITEMS: usize = 257;
const MAX_HEADER_BYTES: usize = 256 * 1024;
const MAX_ERROR_BYTES: usize = 4 * 1024;
const TERMINAL_RESERVATION_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommandOutputBinding {
    pub session_id: String,
    pub workspace: PathBuf,
    pub manager_generation: String,
    pub command_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeCommandOutputStream {
    Stdout,
    Stderr,
    Terminal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeCommandOutputExit {
    Exited,
    Terminated,
    TimedOut,
    WaitFailed { error: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommandOutputTerminal {
    /// A capture failure may be saved without claiming an observed process exit.
    pub exit_observed: bool,
    pub exit_kind: Option<NativeCommandOutputExit>,
    pub exit_code: Option<i32>,
    pub reader_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommandOutputHeader {
    pub binding: NativeCommandOutputBinding,
    pub argv: Vec<String>,
    pub isolation: String,
    pub pty: bool,
    pub registered_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommandOutputRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommandOutputChunk {
    pub start: u64,
    pub end: u64,
    pub stream: NativeCommandOutputStream,
    #[serde(with = "raw_bytes")]
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeCommandOutputSummary {
    pub header: NativeCommandOutputHeader,
    pub revision: u64,
    /// Published byte observations, including the durable omitted tail. Bytes
    /// read without a publication ACK cannot be inferred after reopening.
    pub observed_end: u64,
    pub retained_bytes: u64,
    pub omitted_bytes: u64,
    pub terminal: Option<NativeCommandOutputTerminal>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeCommandOutputRegistration {
    pub summary: NativeCommandOutputSummary,
    /// Only this first ACK permits the original caller to attempt its spawn.
    pub newly_registered: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommandOutputAppendAck {
    pub revision: u64,
    pub observed_end: u64,
    /// Retained prefix length of this append, not the total retained capture.
    pub retained_bytes: usize,
    pub omitted_bytes: usize,
    pub newly_appended: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeCommandOutputPage {
    pub summary: NativeCommandOutputSummary,
    pub chunks: Vec<NativeCommandOutputChunk>,
    pub omitted_ranges: Vec<NativeCommandOutputRange>,
    /// May advance across an omitted range without returning any raw bytes.
    pub next_cursor: u64,
}

#[derive(Debug, Error)]
pub enum NativeCommandOutputError {
    #[error("native automation read-only profile forbids command registration")]
    AutomationReadOnly,
    #[error("native command output store failed: {0}")]
    Store(#[from] StoreError),
    #[error("native command output I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("native command output encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid, linked, corrupt, foreign, or over-limit native command output")]
    Invalid,
    #[error("native command output binding, arguments, or cursor conflict")]
    Conflict,
    #[error("native command output capacity is exhausted before registration or publication")]
    Quota,
}

#[derive(Clone)]
pub struct NativeCommandOutputStore {
    sessions: Arc<SessionStore>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    header: NativeCommandOutputHeader,
    revision: u64,
    observed_end: u64,
    chunks: Vec<NativeCommandOutputChunk>,
    omitted: Option<NativeCommandOutputRange>,
    terminal: Option<NativeCommandOutputTerminal>,
    last_append: Option<AppendReceipt>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppendReceipt {
    expected_cursor: u64,
    stream: NativeCommandOutputStream,
    input_bytes: usize,
    digest: String,
    ack: NativeCommandOutputAppendAck,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    session_id: String,
    commands: Vec<Capture>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    checksum: String,
    payload: Payload,
}
#[derive(Serialize)]
struct EnvelopeRef<'a> {
    version: u32,
    checksum: &'a str,
    payload: &'a Payload,
}

impl Capture {
    fn retained_end(&self) -> u64 {
        self.chunks.last().map_or(0, |chunk| chunk.end)
    }
    fn summary(&self) -> NativeCommandOutputSummary {
        let retained_bytes = self.retained_end();
        NativeCommandOutputSummary {
            header: self.header.clone(),
            revision: self.revision,
            observed_end: self.observed_end,
            retained_bytes,
            omitted_bytes: self.observed_end - retained_bytes,
            terminal: self.terminal.clone(),
        }
    }
}

impl NativeCommandOutputStore {
    pub fn new(sessions: Arc<SessionStore>) -> Self {
        Self { sessions }
    }

    /// Sync the descriptor and reserve its bounded terminal before spawning.
    /// An exact retry ACK never grants another process execution.
    pub fn register(
        &self,
        binding: &NativeCommandOutputBinding,
        argv: &[String],
        isolation: &str,
        pty: bool,
    ) -> Result<NativeCommandOutputRegistration, NativeCommandOutputError> {
        validate_binding(binding)?;
        validate_descriptor(argv, isolation)?;
        self.transaction(&binding.session_id, |session, payload| {
            if session.native_automation.is_some() {
                return Err(NativeCommandOutputError::AutomationReadOnly);
            }
            validate_owner(binding, session)?;
            if let Some(saved) = find_capture(payload, binding) {
                return if saved.header.argv == argv
                    && saved.header.isolation == isolation
                    && saved.header.pty == pty
                {
                    Ok((
                        NativeCommandOutputRegistration {
                            summary: saved.summary(),
                            newly_registered: false,
                        },
                        false,
                    ))
                } else {
                    Err(NativeCommandOutputError::Conflict)
                };
            }
            if session.archived {
                return Err(NativeCommandOutputError::Conflict);
            }
            if payload.commands.len() >= MAX_NATIVE_COMMAND_OUTPUT_COMMANDS {
                return Err(NativeCommandOutputError::Quota);
            }
            let capture = Capture {
                header: NativeCommandOutputHeader {
                    binding: binding.clone(),
                    argv: argv.to_vec(),
                    isolation: isolation.into(),
                    pty,
                    registered_at_ms: now_ms()?,
                },
                revision: 0,
                observed_end: 0,
                chunks: vec![],
                omitted: None,
                terminal: None,
                last_append: None,
            };
            if serde_json::to_vec(&capture.header)?.len() > MAX_HEADER_BYTES {
                return Err(NativeCommandOutputError::Quota);
            }
            let summary = capture.summary();
            payload.commands.push(capture);
            ensure_capacity(payload)?;
            Ok((
                NativeCommandOutputRegistration {
                    summary,
                    newly_registered: true,
                },
                true,
            ))
        })
    }

    pub fn append(
        &self,
        binding: &NativeCommandOutputBinding,
        expected_cursor: u64,
        stream: NativeCommandOutputStream,
        bytes: &[u8],
    ) -> Result<NativeCommandOutputAppendAck, NativeCommandOutputError> {
        validate_binding(binding)?;
        if bytes.is_empty() || bytes.len() > MAX_NATIVE_COMMAND_OUTPUT_APPEND_BYTES {
            return Err(NativeCommandOutputError::Invalid);
        }
        let digest = hex_digest(bytes);
        self.transaction(&binding.session_id, |session, payload| {
            validate_owner(binding, session)?;
            let index = capture_index(payload, binding)?;
            let saved = &payload.commands[index];
            validate_stream(saved.header.pty, stream)?;
            if let Some(last) = &saved.last_append {
                if last.expected_cursor == expected_cursor {
                    return if last.stream == stream
                        && last.input_bytes == bytes.len()
                        && last.digest == digest
                    {
                        let mut ack = last.ack.clone();
                        ack.newly_appended = false;
                        Ok((ack, false))
                    } else {
                        Err(NativeCommandOutputError::Conflict)
                    };
                }
            }
            if saved.observed_end != expected_cursor || saved.terminal.is_some() {
                return Err(NativeCommandOutputError::Conflict);
            }
            expected_cursor
                .checked_add(bytes.len() as u64)
                .ok_or(NativeCommandOutputError::Invalid)?;
            let retained_total = payload.commands.iter().try_fold(0usize, |sum, capture| {
                sum.checked_add(capture.retained_end() as usize)
                    .ok_or(NativeCommandOutputError::Invalid)
            })?;
            let record_count: usize = payload.commands.iter().map(|c| c.chunks.len()).sum();
            let max_retained = if saved.omitted.is_some() {
                0
            } else {
                bytes
                    .len()
                    .min(MAX_NATIVE_COMMAND_OUTPUT_RAW_BYTES - retained_total)
                    .min(
                        (MAX_NATIVE_COMMAND_OUTPUT_RECORDS - record_count)
                            * MAX_NATIVE_COMMAND_OUTPUT_CHUNK_BYTES,
                    )
            };
            let before = payload.commands[index].clone();
            let mut retain = max_retained;
            apply_append(
                &mut payload.commands[index],
                &before,
                expected_cursor,
                stream,
                bytes,
                &digest,
                retain,
            )?;
            if !has_capacity(payload)? {
                // Prefix retention is monotonic while this append has an
                // omitted tail. Serialized capacity is independent of raw/data
                // quotas, and each candidate preserves terminal reservations.
                let mut low = 0;
                let mut high = max_retained;
                apply_append(
                    &mut payload.commands[index],
                    &before,
                    expected_cursor,
                    stream,
                    bytes,
                    &digest,
                    0,
                )?;
                ensure_capacity(payload)?;
                while low < high {
                    let middle = low + (high - low).div_ceil(2);
                    apply_append(
                        &mut payload.commands[index],
                        &before,
                        expected_cursor,
                        stream,
                        bytes,
                        &digest,
                        middle,
                    )?;
                    if has_capacity(payload)? {
                        low = middle;
                    } else {
                        high = middle - 1;
                    }
                }
                retain = low;
                apply_append(
                    &mut payload.commands[index],
                    &before,
                    expected_cursor,
                    stream,
                    bytes,
                    &digest,
                    retain,
                )?;
            }
            let ack = payload.commands[index]
                .last_append
                .as_ref()
                .expect("append receipt")
                .ack
                .clone();
            Ok((ack, true))
        })
    }

    /// Save an observed terminal without incrementing the byte cursor. Exact
    /// retries reacknowledge the saved terminal, never replace it.
    pub fn finish(
        &self,
        binding: &NativeCommandOutputBinding,
        expected_cursor: u64,
        terminal: &NativeCommandOutputTerminal,
    ) -> Result<NativeCommandOutputSummary, NativeCommandOutputError> {
        validate_binding(binding)?;
        validate_terminal(terminal)?;
        self.transaction(&binding.session_id, |session, payload| {
            validate_owner(binding, session)?;
            let index = capture_index(payload, binding)?;
            let capture = &mut payload.commands[index];
            if capture.observed_end != expected_cursor {
                return Err(NativeCommandOutputError::Conflict);
            }
            if let Some(saved) = &capture.terminal {
                return if saved == terminal {
                    Ok((capture.summary(), false))
                } else {
                    Err(NativeCommandOutputError::Conflict)
                };
            }
            capture.revision = capture
                .revision
                .checked_add(1)
                .ok_or(NativeCommandOutputError::Invalid)?;
            capture.terminal = Some(terminal.clone());
            let summary = capture.summary();
            ensure_capacity(payload)?;
            Ok((summary, true))
        })
    }

    pub fn list(
        &self,
        session_id: &str,
        workspace: &Path,
    ) -> Result<Vec<NativeCommandOutputSummary>, NativeCommandOutputError> {
        self.transaction(session_id, |session, payload| {
            if workspace != fs::canonicalize(workspace)?
                || workspace != fs::canonicalize(&session.workspace)?
            {
                return Err(NativeCommandOutputError::Conflict);
            }
            Ok((
                payload.commands.iter().map(Capture::summary).collect(),
                false,
            ))
        })
    }

    pub fn read(
        &self,
        binding: &NativeCommandOutputBinding,
        cursor: u64,
        max_bytes: usize,
        max_records: usize,
    ) -> Result<NativeCommandOutputPage, NativeCommandOutputError> {
        validate_binding(binding)?;
        if max_bytes == 0
            || max_bytes > MAX_NATIVE_COMMAND_OUTPUT_PAGE_BYTES
            || max_records == 0
            || max_records > MAX_NATIVE_COMMAND_OUTPUT_PAGE_RECORDS
        {
            return Err(NativeCommandOutputError::Invalid);
        }
        self.transaction(&binding.session_id, |session, payload| {
            validate_owner(binding, session)?;
            let capture = &payload.commands[capture_index(payload, binding)?];
            if cursor > capture.observed_end {
                return Err(NativeCommandOutputError::Conflict);
            }
            let mut page = NativeCommandOutputPage {
                summary: capture.summary(),
                chunks: vec![],
                omitted_ranges: vec![],
                next_cursor: cursor,
            };
            let mut remaining = max_bytes;
            for chunk in &capture.chunks {
                if chunk.end <= page.next_cursor {
                    continue;
                }
                if remaining == 0 || page.chunks.len() >= max_records {
                    break;
                }
                let start = page.next_cursor.max(chunk.start);
                let offset = (start - chunk.start) as usize;
                let count = (chunk.bytes.len() - offset).min(remaining);
                let end = start + count as u64;
                page.chunks.push(NativeCommandOutputChunk {
                    start,
                    end,
                    stream: chunk.stream,
                    bytes: chunk.bytes[offset..offset + count].to_vec(),
                });
                page.next_cursor = end;
                remaining -= count;
            }
            if let Some(gap) = &capture.omitted {
                if page.next_cursor >= gap.start
                    && page.next_cursor < gap.end
                    && page.chunks.len() < max_records
                {
                    page.omitted_ranges.push(NativeCommandOutputRange {
                        start: page.next_cursor,
                        end: gap.end,
                    });
                    page.next_cursor = gap.end;
                }
            }
            if serde_json::to_vec(&page)?.len() > MAX_NATIVE_COMMAND_OUTPUT_PAGE_SERIALIZED_BYTES {
                return Err(NativeCommandOutputError::Invalid);
            }
            Ok((page, false))
        })
    }

    fn transaction<T>(
        &self,
        session_id: &str,
        change: impl FnOnce(&Session, &mut Payload) -> Result<(T, bool), NativeCommandOutputError>,
    ) -> Result<T, NativeCommandOutputError> {
        self.sessions
            .with_native_command_output_transaction(session_id, |session| {
                let _guard = lock(self.sessions.storage_root(), session_id)?;
                let mut payload = read_payload(self.sessions.storage_root(), session)?;
                let (result, changed) = change(session, &mut payload)?;
                if changed {
                    write_payload(self.sessions.storage_root(), &payload)?;
                }
                Ok(result)
            })
    }
}

fn find_capture<'a>(
    payload: &'a Payload,
    binding: &NativeCommandOutputBinding,
) -> Option<&'a Capture> {
    payload
        .commands
        .iter()
        .find(|c| c.header.binding == *binding)
}
fn capture_index(
    payload: &Payload,
    binding: &NativeCommandOutputBinding,
) -> Result<usize, NativeCommandOutputError> {
    payload
        .commands
        .iter()
        .position(|c| c.header.binding == *binding)
        .ok_or(NativeCommandOutputError::Conflict)
}
fn apply_append(
    capture: &mut Capture,
    before: &Capture,
    expected_cursor: u64,
    stream: NativeCommandOutputStream,
    bytes: &[u8],
    digest: &str,
    retain: usize,
) -> Result<(), NativeCommandOutputError> {
    *capture = before.clone();
    let mut cursor = expected_cursor;
    for part in bytes[..retain].chunks(MAX_NATIVE_COMMAND_OUTPUT_CHUNK_BYTES) {
        let end = cursor
            .checked_add(part.len() as u64)
            .ok_or(NativeCommandOutputError::Invalid)?;
        capture.chunks.push(NativeCommandOutputChunk {
            start: cursor,
            end,
            stream,
            bytes: part.to_vec(),
        });
        cursor = end;
    }
    capture.observed_end = expected_cursor
        .checked_add(bytes.len() as u64)
        .ok_or(NativeCommandOutputError::Invalid)?;
    if retain < bytes.len() {
        let start = capture.omitted.as_ref().map_or(cursor, |gap| gap.start);
        capture.omitted = Some(NativeCommandOutputRange {
            start,
            end: capture.observed_end,
        });
    }
    capture.revision = before
        .revision
        .checked_add(1)
        .ok_or(NativeCommandOutputError::Invalid)?;
    capture.last_append = Some(AppendReceipt {
        expected_cursor,
        stream,
        input_bytes: bytes.len(),
        digest: digest.into(),
        ack: NativeCommandOutputAppendAck {
            revision: capture.revision,
            observed_end: capture.observed_end,
            retained_bytes: retain,
            omitted_bytes: bytes.len() - retain,
            newly_appended: true,
        },
    });
    Ok(())
}

fn encoded(payload: &Payload) -> Result<Vec<u8>, NativeCommandOutputError> {
    let checksum = payload_checksum(payload)?;
    Ok(serde_json::to_vec(&EnvelopeRef {
        version: NATIVE_COMMAND_OUTPUT_VERSION,
        checksum: &checksum,
        payload,
    })?)
}
fn payload_checksum(payload: &Payload) -> Result<String, NativeCommandOutputError> {
    // Explicit recursive sorting canonicalizes keys under feature unification.
    // The checksum is
    // independent of struct field order and cannot be confused with an append
    // digest or a different sidecar version. It is integrity, not authentication.
    let mut value = serde_json::to_value(payload)?;
    value.sort_all_objects();
    let mut bytes = b"OpenIDE.NativeCommandOutput.v1\0".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(&value)?);
    Ok(hex_digest(&bytes))
}
fn has_capacity(payload: &Payload) -> Result<bool, NativeCommandOutputError> {
    let reservation = payload
        .commands
        .iter()
        .filter(|c| c.terminal.is_none())
        .count()
        * TERMINAL_RESERVATION_BYTES;
    // A digest is always 64 ASCII hex characters. Capacity can count the exact
    // serialized envelope without recomputing a full checksum for candidates.
    Ok(serde_json::to_vec(&EnvelopeRef {
        version: NATIVE_COMMAND_OUTPUT_VERSION,
        checksum: "0000000000000000000000000000000000000000000000000000000000000000",
        payload,
    })?
    .len()
    .checked_add(reservation)
    .and_then(|n| n.checked_add(1))
    .is_some_and(|n| n <= MAX_NATIVE_COMMAND_OUTPUT_FILE_BYTES))
}
fn ensure_capacity(payload: &Payload) -> Result<(), NativeCommandOutputError> {
    if has_capacity(payload)? {
        Ok(())
    } else {
        Err(NativeCommandOutputError::Quota)
    }
}

fn validate_binding(binding: &NativeCommandOutputBinding) -> Result<(), NativeCommandOutputError> {
    if !valid_session_id(&binding.session_id)
        || !valid_uuid(&binding.manager_generation)
        || !valid_uuid(&binding.command_id)
        || !binding.workspace.is_absolute()
        || binding.workspace.to_str().is_none()
    {
        return Err(NativeCommandOutputError::Invalid);
    }
    Ok(())
}
fn validate_descriptor(argv: &[String], isolation: &str) -> Result<(), NativeCommandOutputError> {
    if argv.is_empty()
        || argv.len() > MAX_ARGV_ITEMS
        || argv[0].is_empty()
        || argv.iter().any(|s| s.contains('\0'))
        || argv
            .iter()
            .try_fold(0usize, |n, s| n.checked_add(s.len()))
            .is_none_or(|n| n > MAX_ARGV_BYTES)
        || isolation.is_empty()
        || isolation.len() > 128
        || isolation.contains('\0')
    {
        return Err(NativeCommandOutputError::Invalid);
    }
    Ok(())
}
fn validate_owner(
    binding: &NativeCommandOutputBinding,
    session: &Session,
) -> Result<(), NativeCommandOutputError> {
    if binding.session_id != session.id
        || binding.workspace != fs::canonicalize(&session.workspace)?
        || binding.workspace != fs::canonicalize(&binding.workspace)?
    {
        return Err(NativeCommandOutputError::Conflict);
    }
    Ok(())
}
fn validate_terminal(
    terminal: &NativeCommandOutputTerminal,
) -> Result<(), NativeCommandOutputError> {
    let valid_error = |error: &str| !error.is_empty() && error.len() <= MAX_ERROR_BYTES;
    if terminal
        .reader_error
        .as_ref()
        .is_some_and(|e| !valid_error(e))
        || terminal.exit_observed != terminal.exit_kind.is_some()
        || (!terminal.exit_observed
            && (terminal.exit_code.is_some() || terminal.reader_error.is_none()))
        || (terminal.exit_code.is_some()
            && !matches!(terminal.exit_kind, Some(NativeCommandOutputExit::Exited)))
        || matches!(&terminal.exit_kind, Some(NativeCommandOutputExit::WaitFailed { error }) if !valid_error(error))
    {
        return Err(NativeCommandOutputError::Invalid);
    }
    Ok(())
}
fn validate_stream(
    pty: bool,
    stream: NativeCommandOutputStream,
) -> Result<(), NativeCommandOutputError> {
    if pty == (stream == NativeCommandOutputStream::Terminal) {
        Ok(())
    } else {
        Err(NativeCommandOutputError::Invalid)
    }
}
fn valid_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}
fn valid_session_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn now_ms() -> Result<u64, NativeCommandOutputError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| NativeCommandOutputError::Invalid)
        .and_then(|d| u64::try_from(d.as_millis()).map_err(|_| NativeCommandOutputError::Invalid))
}
fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn ledger_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.native-command-output.json"))
}
fn safe_open(path: &Path, create: bool, write: bool) -> Result<File, NativeCommandOutputError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(NativeCommandOutputError::Invalid);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(NativeCommandOutputError::Invalid);
            }
        }
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(write)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(NativeCommandOutputError::Invalid);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if file.metadata()?.file_attributes() & 0x400 != 0 {
            return Err(NativeCommandOutputError::Invalid);
        }
    }
    Ok(file)
}
fn lock(root: &Path, id: &str) -> Result<File, NativeCommandOutputError> {
    if !valid_session_id(id) {
        return Err(NativeCommandOutputError::Invalid);
    }
    let file = safe_open(
        &root.join(format!(".{id}.native-command-output.lock")),
        true,
        true,
    )?;
    file.lock()?;
    Ok(file)
}
fn read_payload(root: &Path, session: &Session) -> Result<Payload, NativeCommandOutputError> {
    let file = match safe_open(&ledger_path(root, &session.id), false, false) {
        Ok(file) => file,
        Err(NativeCommandOutputError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(Payload {
                session_id: session.id.clone(),
                commands: vec![],
            });
        }
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take((MAX_NATIVE_COMMAND_OUTPUT_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_NATIVE_COMMAND_OUTPUT_FILE_BYTES {
        return Err(NativeCommandOutputError::Invalid);
    }
    let envelope: Envelope = serde_json::from_slice(&bytes)?;
    if envelope.version != NATIVE_COMMAND_OUTPUT_VERSION
        || envelope.checksum != payload_checksum(&envelope.payload)?
    {
        return Err(NativeCommandOutputError::Invalid);
    }
    validate_payload(&envelope.payload, session)?;
    ensure_capacity(&envelope.payload).map_err(|_| NativeCommandOutputError::Invalid)?;
    Ok(envelope.payload)
}
fn validate_payload(payload: &Payload, session: &Session) -> Result<(), NativeCommandOutputError> {
    if payload.session_id != session.id
        || payload.commands.len() > MAX_NATIVE_COMMAND_OUTPUT_COMMANDS
    {
        return Err(NativeCommandOutputError::Invalid);
    }
    let mut ids = HashSet::new();
    let mut raw = 0usize;
    let mut records = 0usize;
    for capture in &payload.commands {
        validate_binding(&capture.header.binding)?;
        validate_owner(&capture.header.binding, session)?;
        validate_descriptor(&capture.header.argv, &capture.header.isolation)?;
        if capture.header.registered_at_ms == 0
            || serde_json::to_vec(&capture.header)?.len() > MAX_HEADER_BYTES
            || !ids.insert((
                capture.header.binding.manager_generation.clone(),
                capture.header.binding.command_id.clone(),
            ))
        {
            return Err(NativeCommandOutputError::Invalid);
        }
        let mut end = 0u64;
        for chunk in &capture.chunks {
            validate_stream(capture.header.pty, chunk.stream)?;
            if chunk.start != end
                || chunk.bytes.is_empty()
                || chunk.bytes.len() > MAX_NATIVE_COMMAND_OUTPUT_CHUNK_BYTES
                || chunk.end
                    != chunk
                        .start
                        .checked_add(chunk.bytes.len() as u64)
                        .ok_or(NativeCommandOutputError::Invalid)?
            {
                return Err(NativeCommandOutputError::Invalid);
            }
            end = chunk.end;
            raw = raw
                .checked_add(chunk.bytes.len())
                .ok_or(NativeCommandOutputError::Invalid)?;
            records = records
                .checked_add(1)
                .ok_or(NativeCommandOutputError::Invalid)?;
        }
        match &capture.omitted {
            Some(gap)
                if gap.start == end && gap.end > gap.start && gap.end == capture.observed_end => {}
            None if end == capture.observed_end => {}
            _ => return Err(NativeCommandOutputError::Invalid),
        }
        if let Some(terminal) = &capture.terminal {
            validate_terminal(terminal)?;
        }
        if let Some(last) = &capture.last_append {
            validate_stream(capture.header.pty, last.stream)?;
            if last.input_bytes == 0
                || last.input_bytes > MAX_NATIVE_COMMAND_OUTPUT_APPEND_BYTES
                || last.digest.len() != 64
                || !last
                    .digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                || last.expected_cursor.checked_add(last.input_bytes as u64)
                    != Some(capture.observed_end)
                || last.ack.observed_end != capture.observed_end
                || !last.ack.newly_appended
                || last.ack.retained_bytes.checked_add(last.ack.omitted_bytes)
                    != Some(last.input_bytes)
                || capture.retained_end().saturating_sub(last.expected_cursor)
                    != last.ack.retained_bytes as u64
                || last
                    .ack
                    .revision
                    .checked_add(u64::from(capture.terminal.is_some()))
                    != Some(capture.revision)
                || last.ack.revision == 0
            {
                return Err(NativeCommandOutputError::Invalid);
            }
            let mut retained_suffix = Vec::with_capacity(last.ack.retained_bytes);
            for chunk in capture
                .chunks
                .iter()
                .filter(|chunk| chunk.end > last.expected_cursor)
            {
                if chunk.stream != last.stream {
                    return Err(NativeCommandOutputError::Invalid);
                }
                let offset = last.expected_cursor.saturating_sub(chunk.start) as usize;
                retained_suffix.extend_from_slice(&chunk.bytes[offset..]);
            }
            if last.ack.omitted_bytes == 0 && hex_digest(&retained_suffix) != last.digest {
                return Err(NativeCommandOutputError::Invalid);
            }
        } else if capture.observed_end != 0
            || capture.revision != u64::from(capture.terminal.is_some())
        {
            return Err(NativeCommandOutputError::Invalid);
        }
    }
    if raw > MAX_NATIVE_COMMAND_OUTPUT_RAW_BYTES || records > MAX_NATIVE_COMMAND_OUTPUT_RECORDS {
        return Err(NativeCommandOutputError::Invalid);
    }
    Ok(())
}

fn cleanup_temporaries(root: &Path, id: &str) -> Result<bool, NativeCommandOutputError> {
    let prefix = format!(".{id}.native-command-output.");
    let mut removed = false;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(token) = name
            .to_str()
            .and_then(|n| n.strip_prefix(&prefix))
            .and_then(|n| n.strip_suffix(".tmp"))
        else {
            continue;
        };
        if !valid_uuid(token) {
            continue;
        }
        if !entry.file_type()?.is_file() {
            return Err(NativeCommandOutputError::Invalid);
        }
        fs::remove_file(entry.path())?;
        removed = true;
    }
    #[cfg(unix)]
    if removed {
        File::open(root)?.sync_all()?;
    }
    Ok(removed)
}
fn write_payload(root: &Path, payload: &Payload) -> Result<(), NativeCommandOutputError> {
    ensure_capacity(payload)?;
    let bytes = encoded(payload)?;
    cleanup_temporaries(root, &payload.session_id)?;
    let path = ledger_path(root, &payload.session_id);
    if fs::symlink_metadata(&path).is_ok() {
        let _ = safe_open(&path, false, false)?;
    }
    let temporary = root.join(format!(
        ".{}.native-command-output.{}.tmp",
        payload.session_id,
        Uuid::new_v4()
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        #[cfg(test)]
        publication_cut(payload, "temporary-synced");
        fs::rename(&temporary, path)?;
        #[cfg(test)]
        publication_cut(payload, "renamed");
        #[cfg(unix)]
        File::open(root)?.sync_all()?;
        #[cfg(test)]
        publication_cut(payload, "directory-synced");
        Ok::<_, NativeCommandOutputError>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Caller owns the session journal transaction. Output comes after the
/// command-operation lock in destructive paths, and is held through deletion.
pub(crate) fn lock_and_validate(
    root: &Path,
    session: &Session,
) -> Result<File, NativeCommandOutputError> {
    let guard = lock(root, &session.id)?;
    read_payload(root, session)?;
    Ok(guard)
}
pub(crate) fn has_sidecar(root: &Path, id: &str) -> Result<bool, NativeCommandOutputError> {
    match fs::symlink_metadata(ledger_path(root, id)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let prefix = format!(".{id}.native-command-output.");
            for entry in fs::read_dir(root)? {
                let entry = entry?;
                let name = entry.file_name();
                if name
                    .to_str()
                    .and_then(|n| n.strip_prefix(&prefix))
                    .and_then(|n| n.strip_suffix(".tmp"))
                    .is_some_and(valid_uuid)
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}
/// The journal and output lock remain held; remove output before metadata.
pub(crate) fn remove_files(root: &Path, id: &str) -> Result<bool, NativeCommandOutputError> {
    let temporaries = cleanup_temporaries(root, id)?;
    let removed = match fs::remove_file(ledger_path(root, id)) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
    };
    #[cfg(unix)]
    if removed {
        File::open(root)?.sync_all()?;
    }
    Ok(removed || temporaries)
}

mod raw_bytes {
    use super::*;
    pub fn serialize<S: serde::Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > MAX_NATIVE_COMMAND_OUTPUT_CHUNK_BYTES.div_ceil(3) * 4 {
            return Err(serde::de::Error::custom("output chunk exceeds byte limit"));
        }
        STANDARD.decode(encoded).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
fn publication_cut(payload: &Payload, stage: &str) {
    if std::env::var("OPENIDE_TEST_OUTPUT_CUT_STAGE")
        .ok()
        .as_deref()
        == Some(stage)
        && std::env::var("OPENIDE_TEST_OUTPUT_CUT_SESSION")
            .ok()
            .as_deref()
            == Some(payload.session_id.as_str())
    {
        if let Ok(ready) = std::env::var("OPENIDE_TEST_OUTPUT_CUT_READY") {
            fs::write(ready, stage).expect("test publication barrier");
            loop {
                std::thread::park();
            }
        }
        std::process::exit(86);
    }
}

#[cfg(test)]
#[path = "native_command_output_tests.rs"]
mod tests;
