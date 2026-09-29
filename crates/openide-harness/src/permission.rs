use crate::{CommandIsolation, ExecDecision, ExecPolicy};
use openide_core::{SessionApprovalGrantRecord, ToolCall};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::oneshot;

pub const SESSION_APPROVAL_GRANT_VERSION: u16 = 1;
const MAX_SESSION_APPROVAL_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_SESSION_APPROVAL_ARGUMENT_NODES: usize = 100_000;
const MAX_SESSION_APPROVAL_ARGUMENT_DEPTH: usize = 64;

/// A grant for one exact tool call shape in one session. This record contains
/// only digests, never raw arguments, and can be stored alongside the approval
/// decision. A trusted caller must issue it only after the user explicitly
/// chooses to allow the call for the session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionApprovalGrant {
    pub version: u16,
    pub session_id: String,
    pub tool_name: String,
    pub effective_policy_digest: String,
    pub key_digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionApprovalGrantWire {
    version: u16,
    session_id: String,
    tool_name: String,
    effective_policy_digest: String,
    key_digest: String,
}

impl<'de> Deserialize<'de> for SessionApprovalGrant {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = SessionApprovalGrantWire::deserialize(deserializer)?;
        let grant = Self {
            version: wire.version,
            session_id: wire.session_id,
            tool_name: wire.tool_name,
            effective_policy_digest: wire.effective_policy_digest,
            key_digest: wire.key_digest,
        };
        grant.validate().map_err(serde::de::Error::custom)?;
        Ok(grant)
    }
}

impl SessionApprovalGrant {
    /// Validate a record from any persistence backend before considering it.
    pub fn validate(&self) -> Result<(), SessionApprovalError> {
        if self.version != SESSION_APPROVAL_GRANT_VERSION {
            return Err(SessionApprovalError::UnsupportedVersion);
        }
        validate_session_id(&self.session_id)?;
        validate_tool_name(&self.tool_name)?;
        validate_digest(&self.effective_policy_digest)?;
        validate_digest(&self.key_digest)?;
        Ok(())
    }
}

impl From<SessionApprovalGrantRecord> for SessionApprovalGrant {
    fn from(record: SessionApprovalGrantRecord) -> Self {
        Self {
            version: record.version,
            session_id: record.session_id,
            tool_name: record.tool_name,
            effective_policy_digest: record.effective_policy_digest,
            key_digest: record.key_digest,
        }
    }
}

impl From<SessionApprovalGrant> for SessionApprovalGrantRecord {
    fn from(grant: SessionApprovalGrant) -> Self {
        Self {
            version: grant.version,
            session_id: grant.session_id,
            tool_name: grant.tool_name,
            effective_policy_digest: grant.effective_policy_digest,
            key_digest: grant.key_digest,
        }
    }
}

#[derive(Debug, Error)]
pub enum SessionApprovalError {
    #[error("the effective tool permission is not Ask")]
    NotAwaitingApproval,
    #[error("invalid session id for an approval grant")]
    InvalidSessionId,
    #[error("invalid tool name for an approval grant")]
    InvalidToolName,
    #[error("invalid approval policy digest")]
    InvalidDigest,
    #[error("unsupported session approval grant version")]
    UnsupportedVersion,
    #[error("approval arguments must be a JSON object")]
    InvalidArguments,
    #[error("approval arguments exceed the canonicalization limit")]
    ArgumentsTooLarge,
    #[error("approval arguments exceed the nesting limit")]
    ArgumentsTooDeep,
    #[error("cannot canonicalize approval workspace: {0}")]
    Workspace(#[source] std::io::Error),
    #[error("cannot canonicalize approval working directory: {0}")]
    WorkingDirectory(#[source] std::io::Error),
    #[error("approval working directory is outside the workspace")]
    WorkingDirectoryOutsideWorkspace,
}

/// The runner's decision before invoking a tool. `Ask` pauses the run until the
/// owner of its handle responds, the timeout expires, or the run is cancelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionMode {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionChoice {
    Allow,
    /// Persist one exact call approval for later turns in the same session.
    AllowSession,
    Deny,
}

/// An additional gate around the supplied ToolExecutor. It does not make an
/// executor advertise tools that the executor itself has disabled.
#[derive(Clone, Debug)]
pub struct PermissionPolicy {
    default_mode: PermissionMode,
    tool_modes: HashMap<String, PermissionMode>,
    timeout: Duration,
    exec_policy: ExecPolicy,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        Self {
            // Preserve the executor's existing permissions when no policy is set.
            default_mode: PermissionMode::Allow,
            tool_modes: HashMap::new(),
            timeout: Duration::from_secs(60),
            exec_policy: ExecPolicy::default(),
        }
    }
}

impl PermissionPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_mode(mut self, mode: PermissionMode) -> Self {
        self.default_mode = mode;
        self
    }

    pub fn with_tool(mut self, name: impl Into<String>, mode: PermissionMode) -> Self {
        self.tool_modes.insert(name.into(), mode);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Attach validated argv rules for run_command and start_command. Allow rules never grant
    /// more than the host's existing per-tool permission mode.
    pub fn with_exec_policy(mut self, policy: ExecPolicy) -> Self {
        self.exec_policy = policy;
        self
    }

    pub fn exec_policy(&self) -> &ExecPolicy {
        &self.exec_policy
    }

    pub(crate) fn mode_for(&self, name: &str) -> PermissionMode {
        self.tool_modes
            .get(name)
            .copied()
            .unwrap_or(self.default_mode)
    }

    pub(crate) fn mode_for_call(&self, call: &ToolCall) -> PermissionMode {
        let host_mode = self.mode_for(&call.name);
        if call.name == "write_stdin" && !self.exec_policy.rules().is_empty() {
            // An interactive process can interpret later input as commands.
            // No argv is available here for a trustworthy prefix decision.
            return if self.exec_policy.has_deny_rule() || host_mode == PermissionMode::Deny {
                PermissionMode::Deny
            } else {
                PermissionMode::Ask
            };
        }
        if !matches!(call.name.as_str(), "run_command" | "start_command") {
            return host_mode;
        }
        let evaluation = match self.exec_policy.evaluate_call(call) {
            Ok(evaluation) => evaluation,
            Err(_) => return PermissionMode::Deny,
        };
        match (host_mode, evaluation.decision) {
            (PermissionMode::Deny, _) | (_, Some(ExecDecision::Deny)) => PermissionMode::Deny,
            (PermissionMode::Ask, _) | (_, Some(ExecDecision::Ask)) => PermissionMode::Ask,
            _ => PermissionMode::Allow,
        }
    }

    /// Create a durable grant after an explicit "allow for this session"
    /// decision. The caller-supplied digest must cover policy outside this
    /// type, including command isolation and tool-specific host settings.
    pub fn issue_session_grant(
        &self,
        session_id: &str,
        workspace: &Path,
        cwd: &Path,
        effective_policy_digest: &str,
        call: &ToolCall,
    ) -> Result<SessionApprovalGrant, SessionApprovalError> {
        if self.mode_for_call(call) != PermissionMode::Ask {
            return Err(SessionApprovalError::NotAwaitingApproval);
        }
        let key_digest =
            self.session_grant_key(session_id, workspace, cwd, effective_policy_digest, call)?;
        Ok(SessionApprovalGrant {
            version: SESSION_APPROVAL_GRANT_VERSION,
            session_id: session_id.to_owned(),
            tool_name: call.name.clone(),
            effective_policy_digest: effective_policy_digest.to_owned(),
            key_digest,
        })
    }

    /// An invalid or stale grant leaves `Ask` in place. A `Deny` from the
    /// current policy always wins, including command rules and host settings.
    pub fn mode_for_call_with_session_grants(
        &self,
        session_id: &str,
        workspace: &Path,
        cwd: &Path,
        effective_policy_digest: &str,
        call: &ToolCall,
        grants: &[SessionApprovalGrant],
    ) -> PermissionMode {
        let mode = self.mode_for_call(call);
        if mode != PermissionMode::Ask || grants.is_empty() {
            return mode;
        }
        let Ok(key_digest) =
            self.session_grant_key(session_id, workspace, cwd, effective_policy_digest, call)
        else {
            return PermissionMode::Ask;
        };
        if grants.iter().any(|grant| {
            grant.validate().is_ok()
                && grant.session_id == session_id
                && grant.tool_name == call.name
                && grant.effective_policy_digest == effective_policy_digest
                && grant.key_digest == key_digest
        }) {
            PermissionMode::Allow
        } else {
            PermissionMode::Ask
        }
    }

    fn session_grant_key(
        &self,
        session_id: &str,
        workspace: &Path,
        cwd: &Path,
        effective_policy_digest: &str,
        call: &ToolCall,
    ) -> Result<String, SessionApprovalError> {
        validate_session_id(session_id)?;
        validate_tool_name(&call.name)?;
        validate_digest(effective_policy_digest)?;
        if !call.arguments.is_object() {
            return Err(SessionApprovalError::InvalidArguments);
        }
        let workspace = workspace
            .canonicalize()
            .map_err(SessionApprovalError::Workspace)?;
        let cwd = cwd
            .canonicalize()
            .map_err(SessionApprovalError::WorkingDirectory)?;
        if !cwd.starts_with(&workspace) || !workspace.is_dir() || !cwd.is_dir() {
            return Err(SessionApprovalError::WorkingDirectoryOutsideWorkspace);
        }
        let mut hasher = Sha256::new();
        hasher.update(b"openide-session-approval-v1\0");
        hash_part(&mut hasher, session_id.as_bytes());
        hash_part(&mut hasher, call.name.as_bytes());
        hash_canonical_arguments(&call.arguments, &mut hasher)?;
        hash_path(&mut hasher, &workspace);
        hash_path(&mut hasher, &cwd);
        hash_part(&mut hasher, self.digest_hex().as_bytes());
        hash_part(&mut hasher, effective_policy_digest.as_bytes());
        Ok(format!("{:x}", hasher.finalize()))
    }

    /// Bind the complete effective host policy to the native journal. The
    /// tool map is sorted to make this independent of HashMap iteration order.
    pub fn digest_hex(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"openide-permission-policy-v1\0");
        hasher.update([mode_byte(self.default_mode)]);
        hasher.update(self.timeout.as_nanos().to_be_bytes());
        let mut tools = self.tool_modes.iter().collect::<Vec<_>>();
        tools.sort_by_key(|(name, _)| *name);
        hasher.update((tools.len() as u64).to_be_bytes());
        for (name, mode) in tools {
            hasher.update((name.len() as u64).to_be_bytes());
            hasher.update(name.as_bytes());
            hasher.update([mode_byte(*mode)]);
        }
        hasher.update(self.exec_policy.digest_hex().as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Require confirmation for a discovered external tool while preserving
    /// an explicit user Deny rule. The default mode does not override this
    /// protection, even when local tools default to Allow or Deny.
    pub fn require_ask_unless_denied(&mut self, name: &str) {
        if self.tool_modes.get(name) != Some(&PermissionMode::Deny) {
            self.tool_modes.insert(name.to_owned(), PermissionMode::Ask);
        }
    }

    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }
}

fn mode_byte(mode: PermissionMode) -> u8 {
    match mode {
        PermissionMode::Allow => 0,
        PermissionMode::Ask => 1,
        PermissionMode::Deny => 2,
    }
}

fn validate_session_id(id: &str) -> Result<(), SessionApprovalError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(SessionApprovalError::InvalidSessionId);
    }
    Ok(())
}

fn validate_tool_name(name: &str) -> Result<(), SessionApprovalError> {
    if name.is_empty()
        || name.len() > 256
        || name.trim() != name
        || name.chars().any(char::is_control)
    {
        return Err(SessionApprovalError::InvalidToolName);
    }
    Ok(())
}

fn validate_digest(digest: &str) -> Result<(), SessionApprovalError> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SessionApprovalError::InvalidDigest);
    }
    Ok(())
}

fn hash_part(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

/// Stable identity for every host-controlled condition that can change the
/// meaning of a tool call. The call key additionally binds arguments,
/// canonical workspace and working directory.
pub(crate) fn effective_session_approval_policy_digest(
    catalog_digest: &str,
    mcp_runtime_digest: Option<&str>,
    hooks_digest: &str,
    isolation: CommandIsolation,
    policy: &PermissionPolicy,
) -> Result<String, SessionApprovalError> {
    validate_digest(catalog_digest)?;
    validate_digest(hooks_digest)?;
    if let Some(digest) = mcp_runtime_digest {
        validate_digest(digest)?;
    }
    let mut hasher = Sha256::new();
    hasher.update(b"openide-session-approval-policy-v1\0");
    hash_part(&mut hasher, catalog_digest.as_bytes());
    hash_part(&mut hasher, mcp_runtime_digest.unwrap_or("none").as_bytes());
    hash_part(&mut hasher, hooks_digest.as_bytes());
    hash_part(
        &mut hasher,
        match isolation {
            CommandIsolation::HostUnrestricted => b"host_unrestricted",
            CommandIsolation::WorkspaceWriteOffline => b"workspace_write_offline",
        },
    );
    hash_part(&mut hasher, policy.digest_hex().as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_path(hasher: &mut Sha256, path: &Path) {
    // The OS encoding preserves non-Unicode path distinctions. Grants are
    // intentionally local to the platform and canonical filesystem path.
    hash_part(hasher, path.as_os_str().as_encoded_bytes());
}

fn hash_canonical_arguments(
    value: &Value,
    hasher: &mut Sha256,
) -> Result<(), SessionApprovalError> {
    let mut remaining = MAX_SESSION_APPROVAL_ARGUMENT_BYTES;
    let mut nodes = 0usize;
    hash_json_value(value, hasher, 0, &mut remaining, &mut nodes)
}

fn hash_json_value(
    value: &Value,
    hasher: &mut Sha256,
    depth: usize,
    remaining: &mut usize,
    nodes: &mut usize,
) -> Result<(), SessionApprovalError> {
    if depth > MAX_SESSION_APPROVAL_ARGUMENT_DEPTH {
        return Err(SessionApprovalError::ArgumentsTooDeep);
    }
    *nodes = nodes
        .checked_add(1)
        .ok_or(SessionApprovalError::ArgumentsTooLarge)?;
    if *nodes > MAX_SESSION_APPROVAL_ARGUMENT_NODES {
        return Err(SessionApprovalError::ArgumentsTooLarge);
    }
    match value {
        Value::Null => hasher.update([0]),
        Value::Bool(boolean) => hasher.update([1, u8::from(*boolean)]),
        Value::Number(number) => {
            hasher.update([2]);
            let encoded = number.to_string();
            debit_argument_bytes(remaining, encoded.len())?;
            hash_part(hasher, encoded.as_bytes());
        }
        Value::String(string) => {
            hasher.update([3]);
            debit_argument_bytes(remaining, string.len())?;
            hash_part(hasher, string.as_bytes());
        }
        Value::Array(items) => {
            if items.len() > MAX_SESSION_APPROVAL_ARGUMENT_NODES {
                return Err(SessionApprovalError::ArgumentsTooLarge);
            }
            hasher.update([4]);
            hasher.update((items.len() as u64).to_be_bytes());
            for item in items {
                hash_json_value(item, hasher, depth + 1, remaining, nodes)?;
            }
        }
        Value::Object(object) => {
            if object.len() > MAX_SESSION_APPROVAL_ARGUMENT_NODES {
                return Err(SessionApprovalError::ArgumentsTooLarge);
            }
            hasher.update([5]);
            hasher.update((object.len() as u64).to_be_bytes());
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(key, _)| *key);
            for (key, item) in entries {
                debit_argument_bytes(remaining, key.len())?;
                hash_part(hasher, key.as_bytes());
                hash_json_value(item, hasher, depth + 1, remaining, nodes)?;
            }
        }
    }
    Ok(())
}

fn debit_argument_bytes(remaining: &mut usize, bytes: usize) -> Result<(), SessionApprovalError> {
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or(SessionApprovalError::ArgumentsTooLarge)?;
    Ok(())
}

/// Pending requests exist only for one run. A stale or repeated reply has no
/// effect, and dropping the receiving run closes the reply channel.
#[derive(Default)]
pub(crate) struct PendingPermissions {
    requests: Mutex<HashMap<String, oneshot::Sender<PermissionChoice>>>,
}

impl PendingPermissions {
    pub(crate) fn insert(&self, id: String, sender: oneshot::Sender<PermissionChoice>) {
        self.requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(id, sender);
    }

    pub(crate) fn resolve(&self, id: &str, choice: PermissionChoice) -> bool {
        self.requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(id)
            .is_some_and(|sender| sender.send(choice).is_ok())
    }

    pub(crate) fn remove(&self, id: &str) {
        self.requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecPatternToken, ExecRule};
    use serde_json::json;

    fn command(program: &str, args: &[&str]) -> ToolCall {
        ToolCall {
            id: "call".into(),
            name: "run_command".into(),
            arguments: json!({"command":program,"args":args}),
        }
    }

    fn policy(decision: ExecDecision) -> ExecPolicy {
        ExecPolicy::new(vec![ExecRule::new(
            vec![ExecPatternToken::Literal("git".into())],
            decision,
            None,
        )
        .unwrap()])
        .unwrap()
    }

    #[test]
    fn argv_rules_only_tighten_host_permissions() {
        let call = command("git", &["status"]);
        let host_deny = PermissionPolicy::new()
            .with_tool("run_command", PermissionMode::Deny)
            .with_exec_policy(policy(ExecDecision::Allow));
        assert_eq!(host_deny.mode_for_call(&call), PermissionMode::Deny);
        let host_ask = PermissionPolicy::new()
            .with_tool("run_command", PermissionMode::Ask)
            .with_exec_policy(policy(ExecDecision::Allow));
        assert_eq!(host_ask.mode_for_call(&call), PermissionMode::Ask);
        let raised = PermissionPolicy::new().with_exec_policy(policy(ExecDecision::Deny));
        assert_eq!(raised.mode_for_call(&call), PermissionMode::Deny);
        let mut persistent = call.clone();
        persistent.name = "start_command".into();
        assert_eq!(raised.mode_for_call(&persistent), PermissionMode::Deny);
        let interactive = ToolCall {
            id: "input".into(),
            name: "write_stdin".into(),
            arguments: json!({"id":"process", "stdin":"git status\n"}),
        };
        assert_eq!(raised.mode_for_call(&interactive), PermissionMode::Deny);
        assert_eq!(
            PermissionPolicy::new()
                .with_exec_policy(policy(ExecDecision::Ask))
                .mode_for_call(&interactive),
            PermissionMode::Ask
        );
        let malformed = ToolCall {
            arguments: json!({"command":"git", "args":[false]}),
            ..call
        };
        assert_eq!(raised.mode_for_call(&malformed), PermissionMode::Deny);
    }

    #[test]
    fn effective_policy_digest_covers_host_modes_and_rules() {
        let first = PermissionPolicy::new().with_exec_policy(policy(ExecDecision::Ask));
        let changed = first.clone().with_tool("run_command", PermissionMode::Deny);
        assert_ne!(first.digest_hex(), changed.digest_hex());
        let same = PermissionPolicy::new().with_exec_policy(policy(ExecDecision::Ask));
        assert_eq!(first.digest_hex(), same.digest_hex());
    }

    #[test]
    fn session_approval_identity_covers_catalog_mcp_hooks_isolation_and_policy() {
        let policy = PermissionPolicy::new().with_default_mode(PermissionMode::Ask);
        let catalog = "a".repeat(64);
        let mcp = "b".repeat(64);
        let hooks = "c".repeat(64);
        let digest = |catalog: &str,
                      mcp: Option<&str>,
                      hooks: &str,
                      isolation,
                      policy: &PermissionPolicy| {
            effective_session_approval_policy_digest(catalog, mcp, hooks, isolation, policy)
                .unwrap()
        };
        let original = digest(
            &catalog,
            Some(&mcp),
            &hooks,
            CommandIsolation::HostUnrestricted,
            &policy,
        );
        assert_eq!(original.len(), 64);
        assert_ne!(
            original,
            digest(
                &"d".repeat(64),
                Some(&mcp),
                &hooks,
                CommandIsolation::HostUnrestricted,
                &policy,
            )
        );
        assert_ne!(
            original,
            digest(
                &catalog,
                None,
                &hooks,
                CommandIsolation::HostUnrestricted,
                &policy,
            )
        );
        assert_ne!(
            original,
            digest(
                &catalog,
                Some(&mcp),
                &"d".repeat(64),
                CommandIsolation::HostUnrestricted,
                &policy,
            )
        );
        assert_ne!(
            original,
            digest(
                &catalog,
                Some(&mcp),
                &hooks,
                CommandIsolation::WorkspaceWriteOffline,
                &policy,
            )
        );
        assert_ne!(
            original,
            digest(
                &catalog,
                Some(&mcp),
                &hooks,
                CommandIsolation::HostUnrestricted,
                &policy.clone().with_timeout(Duration::from_secs(61)),
            )
        );
    }

    #[test]
    fn session_grant_is_exact_serializable_and_ignores_json_key_order() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("nested");
        std::fs::create_dir(&cwd).unwrap();
        let policy = PermissionPolicy::new().with_default_mode(PermissionMode::Ask);
        let call = ToolCall {
            id: "one".into(),
            name: "edit_file".into(),
            arguments: serde_json::from_str(r#"{"path":"a.rs","edit":{"old":"a","new":"b"}}"#)
                .unwrap(),
        };
        let effective = "a".repeat(64);
        let grant = policy
            .issue_session_grant("session_1", root.path(), &cwd, &effective, &call)
            .unwrap();
        let encoded = serde_json::to_vec(&grant).unwrap();
        let restored: SessionApprovalGrant = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored, grant);
        assert_eq!(restored.version, SESSION_APPROVAL_GRANT_VERSION);
        let reordered = ToolCall {
            id: "different-model-call-id".into(),
            arguments: serde_json::from_str(r#"{"edit":{"new":"b","old":"a"},"path":"a.rs"}"#)
                .unwrap(),
            ..call.clone()
        };
        assert_eq!(
            policy.mode_for_call_with_session_grants(
                "session_1",
                &root.path().join("."),
                &cwd.join("."),
                &effective,
                &reordered,
                std::slice::from_ref(&restored),
            ),
            PermissionMode::Allow
        );
        let changed_args = ToolCall {
            arguments: json!({"path":"b.rs","edit":{"old":"a","new":"b"}}),
            ..call.clone()
        };
        for (session, current_cwd, current_effective, current_call) in [
            ("session_2", cwd.as_path(), effective.as_str(), &call),
            ("session_1", root.path(), effective.as_str(), &call),
            (
                "session_1",
                cwd.as_path(),
                effective.as_str(),
                &changed_args,
            ),
        ] {
            assert_eq!(
                policy.mode_for_call_with_session_grants(
                    session,
                    root.path(),
                    current_cwd,
                    current_effective,
                    current_call,
                    std::slice::from_ref(&restored),
                ),
                PermissionMode::Ask
            );
        }
        assert_eq!(
            policy.mode_for_call_with_session_grants(
                "session_1",
                root.path(),
                &cwd,
                &"b".repeat(64),
                &call,
                std::slice::from_ref(&restored),
            ),
            PermissionMode::Ask
        );
        let another_workspace = tempfile::tempdir().unwrap();
        assert_eq!(
            policy.mode_for_call_with_session_grants(
                "session_1",
                another_workspace.path(),
                another_workspace.path(),
                &effective,
                &call,
                std::slice::from_ref(&restored),
            ),
            PermissionMode::Ask
        );
        let another_tool = ToolCall {
            name: "delete_file".into(),
            ..call.clone()
        };
        assert_eq!(
            policy.mode_for_call_with_session_grants(
                "session_1",
                root.path(),
                &cwd,
                &effective,
                &another_tool,
                std::slice::from_ref(&restored),
            ),
            PermissionMode::Ask
        );
        let changed_policy = policy.clone().with_timeout(Duration::from_secs(61));
        assert_eq!(
            changed_policy.mode_for_call_with_session_grants(
                "session_1",
                root.path(),
                &cwd,
                &effective,
                &call,
                &[restored],
            ),
            PermissionMode::Ask
        );
    }

    #[test]
    fn current_deny_wins_and_unverifiable_paths_never_use_grants() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let policy = PermissionPolicy::new().with_default_mode(PermissionMode::Ask);
        let call = ToolCall {
            id: "one".into(),
            name: "edit_file".into(),
            arguments: json!({"path":"a.rs"}),
        };
        let effective = "a".repeat(64);
        let grant = policy
            .issue_session_grant("session_1", root.path(), root.path(), &effective, &call)
            .unwrap();
        let denied = policy.clone().with_tool("edit_file", PermissionMode::Deny);
        assert!(matches!(
            denied.issue_session_grant("session_1", root.path(), root.path(), &effective, &call),
            Err(SessionApprovalError::NotAwaitingApproval)
        ));
        assert_eq!(
            denied.mode_for_call_with_session_grants(
                "session_1",
                root.path(),
                root.path(),
                &effective,
                &call,
                std::slice::from_ref(&grant),
            ),
            PermissionMode::Deny
        );
        assert!(matches!(
            policy.issue_session_grant("session_1", root.path(), other.path(), &effective, &call),
            Err(SessionApprovalError::WorkingDirectoryOutsideWorkspace)
        ));
        assert_eq!(
            policy.mode_for_call_with_session_grants(
                "session_1",
                root.path(),
                other.path(),
                &effective,
                &call,
                std::slice::from_ref(&grant),
            ),
            PermissionMode::Ask
        );
        assert_eq!(
            policy.mode_for_call_with_session_grants(
                "session_1",
                root.path(),
                &root.path().join("missing"),
                &effective,
                &call,
                &[grant],
            ),
            PermissionMode::Ask
        );
    }

    #[test]
    fn malformed_records_and_oversized_arguments_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let policy = PermissionPolicy::new().with_default_mode(PermissionMode::Ask);
        let call = ToolCall {
            id: "one".into(),
            name: "edit_file".into(),
            arguments: json!({"path":"a.rs"}),
        };
        let effective = "a".repeat(64);
        let grant = policy
            .issue_session_grant("session_1", root.path(), root.path(), &effective, &call)
            .unwrap();
        for invalid in [
            json!({"version":2,"session_id":grant.session_id,"tool_name":grant.tool_name,"effective_policy_digest":effective,"key_digest":grant.key_digest}),
            json!({"version":1,"session_id":"../escape","tool_name":"edit_file","effective_policy_digest":"a".repeat(64),"key_digest":grant.key_digest}),
            json!({"version":1,"session_id":"session_1","tool_name":"edit_file","effective_policy_digest":"INVALID","key_digest":grant.key_digest}),
        ] {
            assert!(serde_json::from_value::<SessionApprovalGrant>(invalid).is_err());
        }
        let oversized = ToolCall {
            arguments: json!({"value":"x".repeat(MAX_SESSION_APPROVAL_ARGUMENT_BYTES + 1)}),
            ..call.clone()
        };
        assert!(matches!(
            policy.issue_session_grant(
                "session_1",
                root.path(),
                root.path(),
                &effective,
                &oversized
            ),
            Err(SessionApprovalError::ArgumentsTooLarge)
        ));
        let mut nested = Value::Null;
        for _ in 0..=MAX_SESSION_APPROVAL_ARGUMENT_DEPTH {
            nested = json!({"nested": nested});
        }
        let too_deep = ToolCall {
            arguments: nested,
            ..call.clone()
        };
        assert!(matches!(
            policy.issue_session_grant(
                "session_1",
                root.path(),
                root.path(),
                &effective,
                &too_deep
            ),
            Err(SessionApprovalError::ArgumentsTooDeep)
        ));
        assert!(matches!(
            policy.issue_session_grant("session_1", root.path(), root.path(), "invalid", &call),
            Err(SessionApprovalError::InvalidDigest)
        ));
    }
}
