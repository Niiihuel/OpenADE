//! Provider retries and limit pauses as the window shows them.

use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn wall_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// A provider request waiting to be sent again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModelRetry {
    pub attempt: u32,
    pub max_attempts: u32,
    pub until_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PauseView {
    /// Wall clock of the last frame that had a countdown to show.
    pub clock_ms: u64,
    pub retry: Option<ModelRetry>,
}

impl PauseView {
    /// Whether a countdown is visible, so the frame loop keeps the clock.
    pub fn counting(&self) -> bool {
        self.retry.is_some()
    }

    fn seconds_until(&self, at_ms: u64) -> u64 {
        at_ms.saturating_sub(self.clock_ms).div_ceil(1000)
    }

    pub fn retry_label(&self) -> Option<String> {
        self.retry.map(|retry| {
            format!(
                "Reintentando en {}s ({}/{})",
                self.seconds_until(retry.until_ms),
                retry.attempt,
                retry.max_attempts
            )
        })
    }
}
