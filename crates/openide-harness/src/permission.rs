use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::oneshot;

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
    Deny,
}

/// An additional gate around the supplied ToolExecutor. It does not make an
/// executor advertise tools that the executor itself has disabled.
#[derive(Clone, Debug)]
pub struct PermissionPolicy {
    default_mode: PermissionMode,
    tool_modes: HashMap<String, PermissionMode>,
    timeout: Duration,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        Self {
            // Preserve the executor's existing permissions when no policy is set.
            default_mode: PermissionMode::Allow,
            tool_modes: HashMap::new(),
            timeout: Duration::from_secs(60),
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

    pub(crate) fn mode_for(&self, name: &str) -> PermissionMode {
        self.tool_modes
            .get(name)
            .copied()
            .unwrap_or(self.default_mode)
    }

    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }
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
