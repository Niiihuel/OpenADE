//! A browser driver that talks to Chromium through its DevTools protocol.
//!
//! One `Driver` drives the one authoritative page of a browser context, which
//! whoever embeds Chromium creates and hands over as a `Transport`. The page
//! is shared by the user's live view and the agent's tools. It is driven from
//! a plain worker thread, one blocking request at a time; no async runtime
//! and no JavaScript runtime sit between the caller and Chromium.
//!
//! Page content is untrusted: every value that originates in the page is
//! bounded before it is returned, and only HTTP, HTTPS and `about:blank` are
//! ever navigated to.
mod actions;
pub mod cdp;
#[cfg(unix)]
mod directory;
mod driver;
mod keys;
mod observer;
mod scripts;
mod snapshot;
#[cfg(test)]
mod testing;
pub mod transport;

pub use actions::MouseButton;
/// Bits of the modifier keys held with a pointer or wheel event.
pub mod modifiers {
    pub use crate::keys::{ALT, CONTROL, META, SHIFT};
}
#[cfg(unix)]
pub use directory::PrivateDirectory;
pub use driver::{
    ActionWatch, Capture, Cue, Driver, Evaluated, PageInfo, Png, Snapshot, Target, MAX_ARIA_BYTES, MAX_EVALUATE_BYTES,
    MAX_INPUT_TEXT_BYTES, MAX_SCREENSHOT_BYTES, MAX_SELECTION_BYTES, MAX_URL_BYTES, TIMEOUT,
};
pub use observer::{
    ConsoleEntry, ConsoleLevel, NetworkEntry, Report, MAX_CONSOLE_TEXT_BYTES, MAX_EVENTS, MAX_REPORT_BYTES,
    MAX_REQUEST_URL_BYTES,
};
pub use snapshot::{element_of, SnapshotOptions};

use std::fmt;

/// What a failure means for the caller, which is more than its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The request is not acceptable as written. Nothing reached the page.
    Invalid,
    /// The tool's own answer: a stale ref, an ambiguous target, a script that
    /// threw, a wait that ran out. Nothing uncertain happened to the page.
    Tool,
    /// The request reached the page and failed or timed out. If it was an
    /// action, its effect is unknown and must not be repeated blindly.
    Failed,
    /// The page could not be loaded. The driver stays usable.
    Navigation,
    /// The browser or its page is gone. The driver must be dropped.
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverError {
    pub kind: ErrorKind,
    pub message: String,
}

impl DriverError {
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for DriverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DriverError {}
