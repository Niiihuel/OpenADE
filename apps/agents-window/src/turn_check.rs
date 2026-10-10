//! The project's own check command, run after a turn that changed something.
//!
//! A failing check is shown to the user with one action that puts the failure
//! into the composer. Nothing starts a turn by itself and a draft is never
//! replaced, so the worst a wrong command can do is show a notice.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "turn-check.json";
const VERSION: u32 = 1;
/// The failure is usually at the end of the output, so the tail is kept.
const MAX_FAILURE_CHARS: usize = 4_000;
const MAX_CAPTURE_BYTES: u64 = 1024 * 1024;
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(120);

/// Tools whose use means the turn may have changed the project. Reads and
/// planning never trigger a check: running a test suite after a question
/// would only cost time.
pub(crate) fn tool_has_effects(name: &str) -> bool {
    matches!(
        name,
        "write_file"
            | "edit_file"
            | "apply_patch"
            | "delete_file"
            | "run_command"
            | "start_command"
            | "write_stdin"
    )
}

/// One check command per project folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TurnCheckSettings {
    version: u32,
    commands: BTreeMap<String, String>,
}

impl TurnCheckSettings {
    pub fn load(root: &Path) -> Result<Self, String> {
        let bytes = match std::fs::read(root.join(FILE_NAME)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(format!("No se pudo leer el chequeo del proyecto: {error}")),
        };
        let settings: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("El chequeo del proyecto no es válido: {error}"))?;
        if settings.version != VERSION {
            return Err(format!("Versión de chequeo no compatible: {}", settings.version));
        }
        Ok(settings)
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let document = Self { version: VERSION, commands: self.commands.clone() };
        let temporary = root.join(format!(".{FILE_NAME}-{}.tmp", std::process::id()));
        std::fs::create_dir_all(root)
            .and_then(|()| std::fs::write(&temporary, serde_json::to_vec_pretty(&document).unwrap_or_default()))
            .and_then(|()| crate::agent_tool_settings::replace_atomically(&temporary, &root.join(FILE_NAME)))
            .map_err(|error| {
                let _ = std::fs::remove_file(&temporary);
                format!("No se pudo guardar el chequeo del proyecto: {error}")
            })
    }

    pub fn command(&self, workspace: &str) -> Option<&str> {
        self.commands.get(workspace).map(String::as_str)
    }

    /// An empty command removes the check for that project.
    pub fn set(&mut self, workspace: &str, command: &str) {
        let command = command.trim();
        if command.is_empty() {
            self.commands.remove(workspace);
        } else {
            self.commands.insert(workspace.to_owned(), command.to_owned());
        }
    }
}

/// Runs the command through `sh -c` in the project folder. `Ok` means it
/// exited successfully; `Err` carries what to show: the end of its output, or
/// why it could not be run.
// shortcut: no shell-free launcher on Windows, so the check reports that instead of running.
pub(crate) fn run(command: &str, workspace: &Path, timeout: Duration) -> Result<(), String> {
    if cfg!(not(unix)) {
        return Err("El chequeo del proyecto todavía no está disponible en este sistema".into());
    }
    let mut process = Command::new("/bin/sh");
    process
        .args(["-c", command])
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut process, 0);
    let mut child = process
        .spawn()
        .map_err(|error| format!("No se pudo iniciar el chequeo: {error}"))?;
    let mut readers = Vec::new();
    for pipe in [
        child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
        child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        readers.push(std::thread::spawn(move || {
            let mut output = Vec::new();
            let _ = pipe.take(MAX_CAPTURE_BYTES).read_to_end(&mut output);
            output
        }));
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => break None,
        }
    };
    if status.is_none() {
        // SAFETY: the child was started as the leader of its own process group.
        #[cfg(unix)]
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL)
        };
        let _ = child.wait();
    }
    let output: String = readers
        .into_iter()
        .filter_map(|reader| reader.join().ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    match status {
        Some(status) if status.success() => Ok(()),
        Some(status) => Err(format!(
            "`{command}` terminó con código {}.\n{}",
            status.code().map_or_else(|| "desconocido".into(), |code| code.to_string()),
            tail(&output)
        )),
        None => Err(format!(
            "`{command}` no terminó en {} segundos y fue detenido.\n{}",
            timeout.as_secs(),
            tail(&output)
        )),
    }
}

fn tail(output: &str) -> String {
    let output = output.trim();
    let skipped = output.chars().count().saturating_sub(MAX_FAILURE_CHARS);
    let kept: String = output.chars().skip(skipped).collect();
    if skipped > 0 {
        format!("[... se omitió el comienzo ...]\n{kept}")
    } else {
        kept
    }
}

/// The message the "fix" action places in the composer.
pub(crate) fn fix_prompt(failure: &str) -> String {
    format!("El chequeo del proyecto falló después de tus cambios. Corregí la causa y volvé a verificarlo.\n\n{failure}")
}

/// Adds the fix request to whatever the user already typed.
pub(crate) fn merge_into_draft(draft: &str, failure: &str) -> String {
    if draft.trim().is_empty() {
        fix_prompt(failure)
    } else {
        format!("{}\n\n{}", draft.trim_end(), fix_prompt(failure))
    }
}

/// What a subagent is told: everything its parent was told, plus what the
/// subagent-start hooks added. A child already inherits the session-start
/// context through the parent, so a hook that repeats exactly that text for
/// subagents adds nothing.
pub(crate) fn child_instructions(parent: &str, subagent_context: &str) -> String {
    let context = subagent_context.trim();
    if context.is_empty() || parent.contains(context) {
        return parent.to_owned();
    }
    format!("{parent}\n\nContexto de hooks de inicio de subagente:\n{context}")
}

/// The diff a reviewer is given is cut here; a larger change is reviewed
/// from the file list and the reviewer's own reads.
pub(crate) const MAX_REVIEW_DIFF_CHARS: usize = 40_000;

/// The prompt for a review in its own conversation. It carries the change
/// itself, because the reviewer starts with no memory of how it was made.
pub(crate) fn review_prompt(review: &crate::git_review::GitReview) -> Option<String> {
    if review.files == 0 && review.untracked.is_empty() {
        return None;
    }
    let mut prompt = String::from(
        "Review the uncommitted changes in this project. You did not write them and have no context beyond what is below and what you read yourself.\n\n\
Look for defects, in this order: behaviour that is wrong or that breaks an existing caller; missing or weakened tests for changed logic; security and data-loss risks; error handling that hides failures. Ignore style and naming unless they cause a bug.\n\n\
Read the surrounding code before judging a change; a diff alone hides callers. Report only what you can point to. For each finding give: file and line, what goes wrong and when, how sure you are, and the smallest fix. If you find nothing, say what you checked and what you did not.\n\n\
Do not change anything.\n",
    );
    prompt.push_str(&format!(
        "\nBranch {} · {} files · +{} -{}\n",
        review.branch, review.files, review.additions, review.deletions
    ));
    for section in &review.sections {
        for file in &section.files {
            prompt.push_str(&format!(
                "{} {} (+{} -{}) [{}]\n",
                file.status, file.path, file.additions, file.deletions, section.title
            ));
        }
    }
    for path in &review.untracked {
        prompt.push_str(&format!("untracked {path}\n"));
    }
    let mut diff = String::new();
    for file in review.sections.iter().flat_map(|section| &section.files) {
        for line in &file.lines {
            diff.push_str(&line.text);
            diff.push('\n');
        }
    }
    if !diff.is_empty() {
        let total = diff.chars().count();
        let kept: String = diff.chars().take(MAX_REVIEW_DIFF_CHARS).collect();
        prompt.push_str("\nDiff:\n");
        prompt.push_str(&kept);
        if total > MAX_REVIEW_DIFF_CHARS || review.truncated {
            prompt.push_str("\n[Diff cut for size. Read the remaining files yourself.]\n");
        }
    }
    Some(prompt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn children_inherit_the_parent_and_get_subagent_context_once() {
        assert_eq!(child_instructions("PARENT", "  "), "PARENT");
        let extended = child_instructions("PARENT", "ONLY FOR CHILDREN");
        assert!(extended.starts_with("PARENT\n\n") && extended.ends_with("ONLY FOR CHILDREN"));
        // The same rules injected at session start are not repeated.
        assert_eq!(child_instructions("PARENT\nRULES", "RULES\n"), "PARENT\nRULES");
    }

    #[test]
    fn review_prompt_carries_the_change_and_is_absent_without_one() {
        use crate::git_review::{GitReview, ReviewFile, ReviewLine, ReviewLineKind, ReviewSection};
        assert_eq!(review_prompt(&GitReview::default()), None);
        let line = |text: &str| ReviewLine {
            kind: ReviewLineKind::Added,
            text: text.into(),
            old_number: None,
            new_number: Some(1),
        };
        let mut review = GitReview {
            branch: "feat/x".into(),
            files: 1,
            additions: 2,
            sections: vec![ReviewSection {
                title: "Unstaged".into(),
                files: vec![ReviewFile {
                    path: "src/parser.rs".into(),
                    status: "M".into(),
                    lines: vec![line("+fn parse() {}"), line("+// MARKER_LINE")],
                    additions: 2,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            untracked: vec!["notes.md".into()],
            ..Default::default()
        };
        let prompt = review_prompt(&review).unwrap();
        for part in ["Do not change anything", "feat/x", "M src/parser.rs (+2 -0) [Unstaged]", "untracked notes.md", "+// MARKER_LINE"] {
            assert!(prompt.contains(part), "{part} missing");
        }
        assert!(!prompt.contains("Diff cut for size"));

        // An oversized diff is cut and says so; the file list still names everything.
        review.sections[0].files[0].lines = vec![line(&"+x".repeat(MAX_REVIEW_DIFF_CHARS))];
        let cut = review_prompt(&review).unwrap();
        assert!(cut.contains("Diff cut for size") && cut.contains("src/parser.rs"));
        assert!(cut.chars().count() < MAX_REVIEW_DIFF_CHARS + 2_000);

        // Only untracked files is still a change worth reviewing.
        let only_new = GitReview { untracked: vec!["new.rs".into()], ..Default::default() };
        assert!(review_prompt(&only_new).unwrap().contains("untracked new.rs"));
    }

    #[test]
    fn only_tools_that_can_change_the_project_trigger_a_check() {
        for name in ["write_file", "apply_patch", "run_command", "start_command"] {
            assert!(tool_has_effects(name), "{name}");
        }
        for name in ["read_file", "list_files", "search_files", "update_plan", "wait_agent", ""] {
            assert!(!tool_has_effects(name), "{name}");
        }
    }

    #[test]
    fn commands_are_kept_per_project_and_an_empty_one_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = TurnCheckSettings::load(dir.path()).unwrap();
        assert_eq!(settings.command("/a"), None);
        settings.set("/a", "  cargo test  ");
        settings.set("/b", "npm test");
        settings.save(dir.path()).unwrap();
        let mut reloaded = TurnCheckSettings::load(dir.path()).unwrap();
        assert_eq!(reloaded.command("/a"), Some("cargo test"));
        assert_eq!(reloaded.command("/b"), Some("npm test"));
        reloaded.set("/a", "   ");
        assert_eq!(reloaded.command("/a"), None);

        std::fs::write(dir.path().join(FILE_NAME), r#"{"version":9,"commands":{}}"#).unwrap();
        assert!(TurnCheckSettings::load(dir.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn run_passes_reports_the_end_of_a_failure_and_stops_a_hanging_command() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run("true", dir.path(), CHECK_TIMEOUT), Ok(()));
        // The command runs in the project folder.
        std::fs::write(dir.path().join("marker"), "x").unwrap();
        assert_eq!(run("test -f marker", dir.path(), CHECK_TIMEOUT), Ok(()));

        let noisy = "i=0; while [ $i -lt 3000 ]; do echo passing line; i=$((i+1)); done; echo 'error: real failure' >&2; exit 3";
        let failure = run(noisy, dir.path(), CHECK_TIMEOUT).unwrap_err();
        assert!(failure.contains("código 3"), "{failure}");
        assert!(failure.trim_end().ends_with("error: real failure"), "tail lost: {}", &failure[failure.len() - 80..]);
        assert!(failure.contains("se omitió el comienzo"));
        assert!(failure.chars().count() < MAX_FAILURE_CHARS + 200);

        let started = Instant::now();
        let hung = run("echo started; sleep 30", dir.path(), Duration::from_millis(200)).unwrap_err();
        assert!(hung.contains("fue detenido") && hung.contains("started"), "{hung}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_fix_request_is_added_to_a_draft_and_never_replaces_it() {
        assert_eq!(merge_into_draft("  ", "boom"), fix_prompt("boom"));
        let merged = merge_into_draft("mi borrador\n", "boom");
        assert!(merged.starts_with("mi borrador\n\n") && merged.ends_with("boom"));
    }
}
