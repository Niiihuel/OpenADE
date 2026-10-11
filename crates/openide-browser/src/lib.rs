//! A browser driver that talks to Chrome through its DevTools protocol.
//!
//! One `Driver` owns one isolated browser context with one authoritative page,
//! shared by the user's live view and the agent's tools. It is driven from a
//! plain worker thread, one blocking request at a time; no async runtime and
//! no JavaScript runtime sit between the caller and Chrome.
//!
//! Page content is untrusted: every value that originates in the page is
//! bounded before it is returned, and only HTTP, HTTPS and `about:blank` are
//! ever navigated to.
mod actions;
pub mod cdp;
#[cfg(unix)]
mod chrome;
mod driver;
mod keys;
mod observer;
mod scripts;
mod snapshot;
mod stream;
#[cfg(test)]
mod testing;
pub mod transport;

pub use actions::MouseButton;
pub use driver::{
    Capture, Driver, Evaluated, PageInfo, Png, Snapshot, Target, MAX_ARIA_BYTES, MAX_EVALUATE_BYTES,
    MAX_INPUT_TEXT_BYTES, MAX_SCREENSHOT_BYTES, MAX_SELECTION_BYTES, MAX_URL_BYTES, TIMEOUT,
};
pub use observer::{
    ConsoleEntry, ConsoleLevel, NetworkEntry, Report, MAX_CONSOLE_TEXT_BYTES, MAX_EVENTS, MAX_REPORT_BYTES,
    MAX_REQUEST_URL_BYTES,
};
pub use snapshot::SnapshotOptions;
pub use stream::{image_size, Frame, FrameFormat, FrameStream, MAX_FRAME_BYTES};

use std::fmt;
use std::path::PathBuf;

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
    /// Chrome could not be started.
    Launch,
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

#[derive(Clone, Debug)]
pub struct LaunchOptions {
    /// Chrome or Chromium. Never searched for: the host decides which one.
    pub executable: PathBuf,
    /// Viewport in CSS pixels.
    pub width: u32,
    pub height: u32,
    /// Device pixels per CSS pixel. Frames of the live view are
    /// `width × scale` by `height × scale`.
    pub scale: f64,
    /// Where the private directory is created. Falls back to the inherited
    /// temporary directory and then `/tmp` when it is missing or too deep.
    pub temp_root: Option<PathBuf>,
}
