//! Whether a task's CLI is working, waiting for the person or done.
//!
//! A CLI task is a program in a terminal, so its state is read from what the
//! program itself reports: Claude Code runs hooks that write one word to a
//! file named by `OPENADE_CLI_STATUS`; Codex sends desktop notifications
//! (OSC 9) when it asks for approval or ends a turn. Keys sent to the
//! terminal, and a terminal that has gone quiet, fill the gaps.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// The variable naming the file a hook writes to.
pub(crate) const STATUS_VARIABLE: &str = "OPENADE_CLI_STATUS";

/// A working CLI animates its spinner; one silent this long is not working.
pub(crate) const QUIET_LIMIT: Duration = Duration::from_secs(5);

/// How a CLI reports its state, named in `agent-cli-definitions.json`.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StatusReport {
    ClaudeHooks,
    CodexNotifications,
}

impl StatusReport {
    /// The arguments that make the CLI report, appended to its command line.
    pub(crate) fn args(self) -> Vec<OsString> {
        match self {
            Self::ClaudeHooks => vec!["--settings".into(), claude_settings().into()],
            Self::CodexNotifications => [
                "tui.notifications=true",
                "tui.notification_method=\"osc9\"",
                "tui.notification_condition=\"always\"",
            ]
            .into_iter()
            .flat_map(|value| ["-c".into(), value.into()])
            .collect(),
        }
    }
}

/// Claude Code's hooks, merged with the person's own. A task's terminal names
/// the file; a CLI started any other way writes nothing.
fn claude_settings() -> String {
    let write = |word: &str| {
        serde_json::json!({
            "type": "command",
            "command": format!("[ -z \"${STATUS_VARIABLE}\" ] || printf {word} > \"${STATUS_VARIABLE}\""),
        })
    };
    serde_json::json!({
        "hooks": {
            "UserPromptSubmit": [{ "hooks": [write("working")] }],
            "PostToolUse": [{ "matcher": "*", "hooks": [write("working")] }],
            "PreToolUse": [{ "matcher": "AskUserQuestion", "hooks": [write("input")] }],
            // Not `idle_prompt`: that one comes a minute after the turn ended.
            "Notification": [{ "matcher": "permission_prompt|elicitation_dialog", "hooks": [write("input")] }],
            "Stop": [{ "hooks": [write("done")] }],
        }
    })
    .to_string()
}

/// Where the hooks of `session`'s CLI write.
pub(crate) fn status_file(data_root: &Path, session: &str) -> PathBuf {
    data_root.join("cli-status").join(format!("{session}.status"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    Working,
    Input,
    Done,
}

/// The word a hook wrote.
pub(crate) fn from_file(text: &str) -> Option<Signal> {
    match text.trim() {
        "working" => Some(Signal::Working),
        "input" => Some(Signal::Input),
        "done" => Some(Signal::Done),
        _ => None,
    }
}

/// A Codex notification: its approvals and questions start with these
/// words; a finished turn's carries the reply itself.
pub(crate) fn from_notice(text: &str) -> Signal {
    const ASKS: [&str; 5] = [
        "Approval requested",
        "Codex wants to",
        "Plan mode prompt",
        "Question requested",
        "Input requested",
    ];
    if ASKS.iter().any(|prefix| text.starts_with(prefix)) {
        Signal::Input
    } else {
        Signal::Done
    }
}

/// What the sidebar shows for a task: `None` idle, `Some(false)` working,
/// `Some(true)` waiting for the person.
pub(crate) type Status = Option<bool>;

/// What happened since the last look at a task's terminal.
#[derive(Default)]
pub(crate) struct Observed {
    /// `Some(true)` when Enter was among the keys sent.
    pub input: Option<bool>,
    pub signals: Vec<Signal>,
    pub quiet_for: Option<Duration>,
    pub running: bool,
}

/// The next status, and whether the turn ended with it (worth a notice).
pub(crate) fn next(
    status: Status,
    report: StatusReport,
    observed: &Observed,
) -> (Status, Option<Signal>) {
    let mut status = status;
    let mut notice = None;
    match observed.input {
        // Answering a question or an approval is typed in the terminal.
        Some(_) if status == Some(true) => status = Some(false),
        // Codex reports no prompt; Enter is its start. Claude's hook says so.
        Some(true) if report == StatusReport::CodexNotifications => status = Some(false),
        _ => {}
    }
    for signal in &observed.signals {
        match signal {
            Signal::Working => status = Some(false),
            Signal::Input => {
                if status != Some(true) {
                    notice = Some(Signal::Input);
                }
                status = Some(true);
            }
            Signal::Done => {
                if status.is_some() {
                    notice = Some(Signal::Done);
                }
                status = None;
            }
        }
    }
    // Claude's hooks say when its turn ends; Codex says nothing while it
    // works, so a terminal gone quiet is its end of work, without a notice.
    let quiet = report == StatusReport::CodexNotifications
        && status == Some(false)
        && observed.quiet_for.is_some_and(|quiet| quiet >= QUIET_LIMIT);
    if !observed.running || quiet {
        status = None;
    }
    (status, notice)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(input: Option<bool>, signals: &[Signal]) -> Observed {
        Observed { input, signals: signals.to_vec(), quiet_for: Some(Duration::ZERO), running: true }
    }

    #[test]
    fn claude_turns_go_from_working_to_a_question_and_to_done() {
        let claude = StatusReport::ClaudeHooks;
        // Enter alone starts nothing: the hook says when a prompt was taken.
        assert_eq!(next(None, claude, &observed(Some(true), &[])), (None, None));
        let (status, notice) = next(None, claude, &observed(None, &[Signal::Working]));
        assert_eq!((status, notice), (Some(false), None));
        let (status, notice) = next(status, claude, &observed(None, &[Signal::Input]));
        assert_eq!((status, notice), (Some(true), Some(Signal::Input)));
        // Choosing an option is a key in the terminal.
        let (status, notice) = next(status, claude, &observed(Some(true), &[]));
        assert_eq!((status, notice), (Some(false), None));
        assert_eq!(next(status, claude, &observed(None, &[Signal::Done])), (None, Some(Signal::Done)));
        // A stale "done" with nothing running is no news.
        assert_eq!(next(None, claude, &observed(None, &[Signal::Done])), (None, None));
    }

    #[test]
    fn codex_starts_on_enter_and_reads_its_notifications() {
        let codex = StatusReport::CodexNotifications;
        let (status, _) = next(None, codex, &observed(Some(true), &[]));
        assert_eq!(status, Some(false));
        let ask = from_notice("Approval requested: cargo test");
        assert_eq!(next(status, codex, &observed(None, &[ask])), (Some(true), Some(Signal::Input)));
        assert_eq!(
            next(Some(false), codex, &observed(None, &[from_notice("Listo, los tests pasan.")])),
            (None, Some(Signal::Done))
        );
    }

    #[test]
    fn a_quiet_or_ended_terminal_is_not_working() {
        let mut quiet = observed(None, &[]);
        quiet.quiet_for = Some(QUIET_LIMIT);
        assert_eq!(next(Some(false), StatusReport::CodexNotifications, &quiet), (None, None));
        // Claude's own hook ends its turn; a question waits however long it takes.
        assert_eq!(next(Some(false), StatusReport::ClaudeHooks, &quiet), (Some(false), None));
        assert_eq!(next(Some(true), StatusReport::CodexNotifications, &quiet), (Some(true), None));
        let mut ended = observed(None, &[]);
        ended.running = false;
        assert_eq!(next(Some(true), StatusReport::ClaudeHooks, &ended), (None, None));
    }

    #[test]
    fn report_arguments_and_words() {
        let claude = StatusReport::ClaudeHooks.args();
        assert_eq!(claude[0], "--settings");
        let settings: serde_json::Value = serde_json::from_str(claude[1].to_str().unwrap()).unwrap();
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            "[ -z \"$OPENADE_CLI_STATUS\" ] || printf done > \"$OPENADE_CLI_STATUS\""
        );
        assert_eq!(StatusReport::CodexNotifications.args().len(), 6);
        assert_eq!(from_file("input\n"), Some(Signal::Input));
        assert_eq!(from_file(""), None);
    }
}
