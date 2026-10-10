//! Lazy per-tool diffs from the same confirmed journal replay as session changes.
//! No current working-tree contents are used as before/after evidence.

use crate::session_changes::{self, SessionFileChange};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionToolDiff {
    pub session_id: String,
    pub workspace: PathBuf,
    pub tool_call_id: String,
    /// Zero means no uniquely confirmed Finished exists for this requested tool.
    pub sequence: u64,
    pub files: Vec<SessionFileChange>,
    pub limited: bool,
    pub unavailable: Option<String>,
    pub coverage_notes: Vec<String>,
}

pub fn load_stored_tool_diff(
    storage_root: &Path,
    session_id: &str,
    workspace: &Path,
    tool_call_id: &str,
) -> io::Result<SessionToolDiff> {
    let projection = session_changes::load_stored_tool_diff_projection(
        storage_root,
        session_id,
        workspace,
        tool_call_id,
    )?;
    Ok(SessionToolDiff {
        session_id: session_id.into(),
        workspace: workspace.into(),
        tool_call_id: tool_call_id.into(),
        sequence: projection.sequence,
        limited: projection.unavailable.is_some()
            || !projection.coverage_notes.is_empty()
            || projection
                .files
                .iter()
                .any(|file| file.truncated || file.unavailable.is_some()),
        files: projection.files,
        unavailable: projection.unavailable,
        coverage_notes: projection.coverage_notes,
    })
}

#[derive(Clone, Debug)]
pub struct SessionToolDiffRequest {
    pub storage_root: PathBuf,
    pub workspace: PathBuf,
    pub session_id: String,
    pub tool_call_id: String,
    pub generation: u64,
}

#[derive(Debug)]
pub struct SessionToolDiffResponse {
    pub workspace: PathBuf,
    pub session_id: String,
    pub tool_call_id: String,
    pub generation: u64,
    pub result: Result<SessionToolDiff, String>,
}

impl SessionToolDiffResponse {
    pub fn matches(
        &self,
        generation: u64,
        session_id: &str,
        workspace: &Path,
        tool_call_id: &str,
    ) -> bool {
        self.generation == generation
            && self.session_id == session_id
            && self.workspace == workspace
            && self.tool_call_id == tool_call_id
    }
}

pub struct SessionToolDiffWorker {
    mailbox: Arc<Mailbox>,
}

#[derive(Default)]
struct Mailbox {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    pending: Option<SessionToolDiffRequest>,
    ready: Option<SessionToolDiffResponse>,
    latest: Option<(u64, String, PathBuf, String)>,
    stopped: bool,
}

impl SessionToolDiffWorker {
    pub fn spawn() -> io::Result<Self> {
        let mailbox = Arc::new(Mailbox::default());
        let background = mailbox.clone();
        thread::Builder::new()
            .name("openide-tool-diff".into())
            .spawn(move || {
                loop {
                    let request = {
                        let Ok(mut state) = background.state.lock() else {
                            return;
                        };
                        while state.pending.is_none() && !state.stopped {
                            let Ok(next) = background.wake.wait(state) else {
                                return;
                            };
                            state = next;
                        }
                        if state.stopped {
                            return;
                        }
                        state.pending.take().expect("checked request")
                    };
                    let result = load_stored_tool_diff(
                        &request.storage_root,
                        &request.session_id,
                        &request.workspace,
                        &request.tool_call_id,
                    )
                    .map_err(|error| error.to_string());
                    let response = SessionToolDiffResponse {
                        workspace: request.workspace,
                        session_id: request.session_id,
                        tool_call_id: request.tool_call_id,
                        generation: request.generation,
                        result,
                    };
                    let Ok(mut state) = background.state.lock() else {
                        return;
                    };
                    if state.stopped {
                        return;
                    }
                    if state.latest.as_ref().is_some_and(
                        |(generation, session, workspace, tool)| {
                            response.matches(*generation, session, workspace, tool)
                        },
                    ) {
                        state.ready = Some(response);
                    }
                }
            })?;
        Ok(Self { mailbox })
    }

    pub fn submit(&self, request: SessionToolDiffRequest) -> io::Result<()> {
        let mut state = self
            .mailbox
            .state
            .lock()
            .map_err(|_| io::Error::other("Tool diff mailbox lock failed"))?;
        state.latest = Some((
            request.generation,
            request.session_id.clone(),
            request.workspace.clone(),
            request.tool_call_id.clone(),
        ));
        state.pending = Some(request);
        state.ready = None;
        self.mailbox.wake.notify_one();
        Ok(())
    }

    pub fn poll_latest(
        &self,
        generation: u64,
        session_id: &str,
        workspace: &Path,
        tool_call_id: &str,
    ) -> Option<SessionToolDiffResponse> {
        let mut state = self.mailbox.state.lock().ok()?;
        // A consumer in an older scope must not take the newer scope's result.
        if !state
            .ready
            .as_ref()?
            .matches(generation, session_id, workspace, tool_call_id)
        {
            return None;
        }
        state.ready.take()
    }
}

impl Drop for SessionToolDiffWorker {
    fn drop(&mut self) {
        if let Ok(mut state) = self.mailbox.state.lock() {
            state.stopped = true;
            state.pending = None;
            state.ready = None;
            state.latest = None;
            self.mailbox.wake.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_review::ReviewLineKind;
    use openide_core::{AgentEvent, AgentEventKind, ToolCall};
    use serde_json::json;
    use std::fs;
    use std::time::{Duration, Instant};

    struct Fixture {
        _temp: tempfile::TempDir,
        storage: PathBuf,
        workspace: PathBuf,
        events: Vec<AgentEvent>,
    }

    impl Fixture {
        fn new(files: &[(&str, &str)]) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let workspace = temp.path().join("workspace");
            let storage = temp.path().join("store");
            fs::create_dir_all(&workspace).unwrap();
            fs::create_dir_all(&storage).unwrap();
            for (path, content) in files {
                let target = workspace.join(path);
                fs::create_dir_all(target.parent().unwrap()).unwrap();
                fs::write(target, content).unwrap();
            }
            let events = vec![AgentEvent::new(
                "session",
                1,
                AgentEventKind::SessionStarted {
                    workspace: workspace.clone(),
                },
            )];
            assert!(matches!(
                session_changes::capture_baseline(&storage, "session", &workspace, &events)
                    .unwrap(),
                session_changes::BaselineCapture::Created(_)
            ));
            Self {
                _temp: temp,
                storage,
                workspace,
                events,
            }
        }

        fn event(&mut self, kind: AgentEventKind) {
            self.events.push(AgentEvent::new(
                "session",
                self.events.len() as u64 + 1,
                kind,
            ));
        }
        fn start(&mut self, id: &str, name: &str, arguments: serde_json::Value) {
            self.event(AgentEventKind::ToolStarted {
                tool_call: ToolCall {
                    id: id.into(),
                    name: name.into(),
                    arguments,
                },
            });
        }
        fn finish(&mut self, id: &str, is_error: bool) {
            self.event(AgentEventKind::ToolFinished {
                tool_call_id: id.into(),
                output: if is_error { "failed" } else { "ok" }.into(),
                is_error,
            });
        }
        fn write(&mut self, id: &str, path: &str, content: &str) {
            self.start(id, "write_file", json!({"path":path,"content":content}));
            self.finish(id, false);
        }
        fn patch(&mut self, id: &str, patch: &str) {
            self.start(id, "apply_patch", json!({"patch":patch}));
            self.finish(id, false);
        }
        fn persist(&self) {
            let mut bytes = Vec::new();
            for event in &self.events {
                serde_json::to_writer(&mut bytes, event).unwrap();
                bytes.push(b'\n');
            }
            fs::write(self.storage.join("session.jsonl"), bytes).unwrap();
        }
        fn load(&self, id: &str) -> SessionToolDiff {
            self.persist();
            load_stored_tool_diff(&self.storage, "session", &self.workspace, id).unwrap()
        }
    }

    fn rows(diff: &SessionToolDiff, kind: ReviewLineKind) -> Vec<&str> {
        diff.files
            .iter()
            .flat_map(|file| &file.file.lines)
            .filter(|row| row.kind == kind)
            .map(|row| row.text.as_str())
            .collect()
    }

    #[test]
    fn tool_diff_write_is_per_edit_and_does_not_read_current_worktree() {
        let mut fixture = Fixture::new(&[("a.rs", "initial\n")]);
        fixture.write("first", "a.rs", "middle\n");
        fixture.write("second", "a.rs", "final\n");
        fs::write(fixture.workspace.join("a.rs"), "MANUAL DISK CHANGE\n").unwrap();
        let second = fixture.load("second");
        assert_eq!(second.sequence, 5);
        assert!(!second.limited);
        assert_eq!(rows(&second, ReviewLineKind::Removed), ["-middle"]);
        assert_eq!(rows(&second, ReviewLineKind::Added), ["+final"]);
        let first = fixture.load("first");
        assert_eq!(rows(&first, ReviewLineKind::Removed), ["-initial"]);
        assert_eq!(rows(&first, ReviewLineKind::Added), ["+middle"]);
        let cumulative = session_changes::load_stored_session_changes(
            &fixture.storage,
            "session",
            &fixture.workspace,
        )
        .unwrap();
        assert_eq!(cumulative.files.len(), 1);
        assert!(
            cumulative.files[0]
                .file
                .lines
                .iter()
                .any(|row| row.text == "+final")
        );
        assert!(
            cumulative.files[0]
                .file
                .lines
                .iter()
                .any(|row| row.text == "-initial")
        );
    }

    #[test]
    fn absolute_workspace_writes_replay_with_relative_patches_without_reading_current_files() {
        let mut fixture = Fixture::new(&[("a.rs", "before\n")]);
        let absolute = fixture.workspace.canonicalize().unwrap().join("a.rs");
        fixture.write("absolute", absolute.to_str().unwrap(), "middle\n");
        fixture.patch(
            "patch",
            "*** Begin Patch\n*** Update File: a.rs\n@@\n-middle\n+after\n*** End Patch",
        );
        fs::remove_file(&absolute).unwrap();
        let first = fixture.load("absolute");
        assert!(!first.limited);
        assert_eq!(first.files[0].file.path, "a.rs");
        assert_eq!(rows(&first, ReviewLineKind::Removed), ["-before"]);
        assert_eq!(rows(&first, ReviewLineKind::Added), ["+middle"]);
        let patch = fixture.load("patch");
        assert!(!patch.limited);
        assert_eq!(rows(&patch, ReviewLineKind::Removed), ["-middle"]);
        assert_eq!(rows(&patch, ReviewLineKind::Added), ["+after"]);
    }

    #[test]
    fn absolute_outside_prefix_siblings_parent_traversal_and_empty_paths_have_no_diff() {
        for variant in ["outside", "sibling", "parent", "empty", "root"] {
            let mut fixture = Fixture::new(&[("a.rs", "before\n")]);
            let root = fixture.workspace.canonicalize().unwrap();
            let path = match variant {
                "outside" => root.parent().unwrap().join("a.rs"),
                "sibling" => root.with_file_name("workspace-other").join("a.rs"),
                "parent" => root.join("nested").join("..").join("a.rs"),
                "empty" => PathBuf::new(),
                "root" => root,
                _ => unreachable!(),
            };
            fixture.write("invalid", path.to_str().unwrap(), "untrusted\n");
            let diff = fixture.load("invalid");
            assert!(diff.unavailable.is_some(), "{variant}");
            assert!(diff.files.is_empty(), "{variant}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn recorded_workspace_alias_is_accepted_but_an_unrecorded_external_alias_is_not() {
        let mut fixture = Fixture::new(&[("a.rs", "before\n")]);
        let alias = fixture.workspace.with_file_name("selected-alias");
        let foreign = fixture.workspace.with_file_name("unrecorded-alias");
        std::os::unix::fs::symlink(&fixture.workspace, &alias).unwrap();
        std::os::unix::fs::symlink(&fixture.workspace, &foreign).unwrap();
        fixture.events[0].kind = AgentEventKind::SessionStarted {
            workspace: alias.clone(),
        };
        fixture.write("selected", alias.join("a.rs").to_str().unwrap(), "after\n");
        fixture.write(
            "foreign",
            foreign.join("a.rs").to_str().unwrap(),
            "untrusted\n",
        );
        // A subsequent current-tree symlink cannot substitute its destination
        // for the captured before/after of the recorded successful operation.
        fs::remove_file(fixture.workspace.join("a.rs")).unwrap();
        let outside = fixture.workspace.with_file_name("outside.rs");
        fs::write(&outside, "unrelated\n").unwrap();
        std::os::unix::fs::symlink(&outside, fixture.workspace.join("a.rs")).unwrap();
        let accepted = fixture.load("selected");
        assert!(!accepted.limited);
        assert_eq!(accepted.files[0].file.path, "a.rs");
        assert_eq!(rows(&accepted, ReviewLineKind::Removed), ["-before"]);
        assert_eq!(rows(&accepted, ReviewLineKind::Added), ["+after"]);
        let rejected = fixture.load("foreign");
        assert!(rejected.unavailable.is_some());
        assert!(rejected.files.is_empty());
    }

    #[test]
    fn tool_diff_patch_replays_prior_confirmed_write_and_exact_line_numbers() {
        let mut fixture = Fixture::new(&[("a.rs", "old\n")]);
        fixture.write("write", "a.rs", "first\nsecond\n");
        fixture.patch(
            "patch",
            "*** Begin Patch\n*** Update File: a.rs\n@@\n first\n-second\n+changed\n*** End Patch",
        );
        let diff = fixture.load("patch");
        assert_eq!(rows(&diff, ReviewLineKind::Removed), ["-second"]);
        assert_eq!(rows(&diff, ReviewLineKind::Added), ["+changed"]);
        let removed = diff.files[0]
            .file
            .lines
            .iter()
            .find(|row| row.kind == ReviewLineKind::Removed)
            .unwrap();
        let added = diff.files[0]
            .file
            .lines
            .iter()
            .find(|row| row.kind == ReviewLineKind::Added)
            .unwrap();
        assert_eq!(removed.old_number, Some(2));
        assert_eq!(added.new_number, Some(2));
    }

    #[test]
    fn tool_diff_rename_pairs_old_and_new_paths_without_fake_added_deleted_files() {
        let mut fixture = Fixture::new(&[("old.rs", "old\n")]);
        fixture.patch("rename", "*** Begin Patch\n*** Update File: old.rs\n*** Move to: new.rs\n@@\n-old\n+new\n*** End Patch");
        let diff = fixture.load("rename");
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].file.path, "new.rs");
        assert_eq!(diff.files[0].file.old_path.as_deref(), Some("old.rs"));
        assert_eq!(diff.files[0].file.status, "Renombrado");
        assert_eq!(rows(&diff, ReviewLineKind::Removed), ["-old"]);
        assert_eq!(rows(&diff, ReviewLineKind::Added), ["+new"]);
    }

    #[test]
    fn tool_diff_add_delete_and_empty_files_keep_presence_semantics() {
        let mut fixture = Fixture::new(&[("gone.txt", "gone\n"), ("empty.txt", "")]);
        fixture.patch("patch", "*** Begin Patch\n*** Add File: fresh.txt\n+fresh\n*** Delete File: gone.txt\n*** Delete File: empty.txt\n*** End Patch");
        let diff = fixture.load("patch");
        assert_eq!(diff.files.len(), 3);
        assert_eq!(
            diff.files
                .iter()
                .find(|file| file.file.path == "empty.txt")
                .unwrap()
                .file
                .status,
            "Eliminado"
        );
        assert_eq!(
            diff.files
                .iter()
                .find(|file| file.file.path == "fresh.txt")
                .unwrap()
                .file
                .status,
            "Nuevo"
        );
        assert_eq!(rows(&diff, ReviewLineKind::Removed), ["-gone"]);
        assert_eq!(rows(&diff, ReviewLineKind::Added), ["+fresh"]);
    }

    #[test]
    fn tool_diff_failed_provisional_and_duplicate_tools_never_show_invented_effects() {
        let mut fixture = Fixture::new(&[("a", "before")]);
        fixture.start(
            "failed",
            "write_file",
            json!({"path":"a","content":"after"}),
        );
        fixture.finish("failed", true);
        let failed = fixture.load("failed");
        assert!(failed.unavailable.is_some());
        assert!(failed.files.is_empty());
        assert_eq!(failed.sequence, 3);
        fixture.start(
            "pending",
            "write_file",
            json!({"path":"a","content":"after"}),
        );
        let pending = fixture.load("pending");
        assert!(pending.unavailable.is_some());
        assert!(pending.files.is_empty());
        assert_eq!(pending.sequence, 0);
        fixture.finish("pending", false);
        fixture.write("pending", "a", "third");
        let duplicate = fixture.load("pending");
        assert!(duplicate.unavailable.is_some());
        assert!(duplicate.files.is_empty());
    }

    #[test]
    fn tool_diff_opaque_effect_before_target_marks_input_unknown_and_later_effects_do_not_change_history()
     {
        let mut fixture = Fixture::new(&[("a", "before\n")]);
        fixture.write("first", "a", "known\n");
        fixture.start("command", "run_command", json!({"command":"opaque"}));
        fixture.finish("command", false);
        fixture.write("second", "a", "after\n");
        let first = fixture.load("first");
        assert!(first.files[0].unavailable.is_none());
        let second = fixture.load("second");
        assert!(second.limited);
        assert!(second.files[0].unavailable.is_some());
        assert!(second.files[0].file.lines.is_empty());
    }

    #[test]
    fn tool_diff_overlapping_pending_edit_makes_before_unavailable() {
        let mut fixture = Fixture::new(&[("a", "before")]);
        fixture.start("other", "write_file", json!({"path":"a","content":"other"}));
        fixture.write("selected", "a", "selected");
        fixture.finish("other", false);
        let diff = fixture.load("selected");
        assert!(
            diff.files[0]
                .unavailable
                .as_ref()
                .unwrap()
                .contains("otra edición")
        );
        assert!(diff.files[0].file.lines.is_empty());
    }

    #[test]
    fn tool_diff_overlapping_edit_that_finishes_first_still_has_unproven_effect_order() {
        let mut fixture = Fixture::new(&[("a", "before")]);
        fixture.start(
            "selected",
            "write_file",
            json!({"path":"a","content":"selected"}),
        );
        fixture.write("other", "a", "other");
        fixture.finish("selected", false);
        let diff = fixture.load("selected");
        assert!(
            diff.files[0]
                .unavailable
                .as_ref()
                .unwrap()
                .contains("otra edición")
        );
        assert!(diff.files[0].file.lines.is_empty());
    }

    #[test]
    fn tool_diff_manual_commands_and_mcp_boundary_preserve_uncertain_evidence() {
        let mut fixture = Fixture::new(&[("a", "before\n")]);
        fixture.write("first", "a", "first\n");
        fixture.write("second", "a", "second\n");
        crate::session_changes_external::mark_external_runtime(
            &fixture.storage,
            "session",
            &fixture.workspace,
            3,
        )
        .unwrap();
        let first = fixture.load("first");
        assert!(first.files[0].unavailable.is_none());
        let second = fixture.load("second");
        assert!(second.files[0].unavailable.is_some());
        fs::write(
            fixture
                .storage
                .join("session.native-command-operations.json"),
            "{}",
        )
        .unwrap();
        let first = fixture.load("first");
        assert!(first.files[0].unavailable.is_some());
    }

    #[test]
    fn tool_diff_validates_corrupt_tail_scope_and_incomplete_record() {
        let mut fixture = Fixture::new(&[("a", "before")]);
        fixture.write("selected", "a", "after");
        fixture.event(AgentEventKind::TurnCompleted {
            response: "done".into(),
            steps: 1,
        });
        fixture.events.last_mut().unwrap().sequence = 100;
        fixture.persist();
        assert!(
            load_stored_tool_diff(&fixture.storage, "session", &fixture.workspace, "selected")
                .is_err()
        );
        fixture.events.last_mut().unwrap().sequence = 4;
        fixture.events.last_mut().unwrap().session_id = "other".into();
        fixture.persist();
        assert!(
            load_stored_tool_diff(&fixture.storage, "session", &fixture.workspace, "selected")
                .is_err()
        );
        fixture.events.pop();
        fixture.persist();
        let path = fixture.storage.join("session.jsonl");
        let mut bytes = fs::read(&path).unwrap();
        bytes.pop();
        fs::write(path, bytes).unwrap();
        assert!(
            load_stored_tool_diff(&fixture.storage, "session", &fixture.workspace, "selected")
                .is_err()
        );
    }

    #[test]
    fn tool_diff_missing_baseline_binary_and_invalid_path_are_explicit() {
        let fixture = Fixture::new(&[("a", "before")]);
        let unknown =
            load_stored_tool_diff(&fixture.storage, "unknown", &fixture.workspace, "tool").unwrap();
        assert!(unknown.unavailable.as_ref().unwrap().contains("confirmado"));
        let mut no_baseline = Fixture::new(&[("a", "before")]);
        no_baseline.write("recorded", "a", "after");
        session_changes::remove_baseline(&no_baseline.storage, "session").unwrap();
        assert!(
            no_baseline
                .load("recorded")
                .unavailable
                .as_ref()
                .unwrap()
                .contains("baseline")
        );
        let mut fixture = Fixture::new(&[("a", "before")]);
        fixture.write("binary", "a", "after\0binary");
        let diff = fixture.load("binary");
        assert!(diff.files[0].unavailable.is_some());
        fixture.write("escape", "../outside", "anything");
        let diff = fixture.load("escape");
        assert!(diff.unavailable.is_some());
        assert!(diff.files.is_empty());
    }

    #[test]
    fn tool_diff_output_caps_files_and_aggregate_lines() {
        let mut fixture = Fixture::new(&[]);
        let mut patch = "*** Begin Patch\n".to_string();
        for index in 0..20 {
            patch.push_str(&format!("*** Add File: {index:02}.txt\n"));
            for line in 0..150 {
                patch.push_str(&format!("+{index} line {line}\n"));
            }
        }
        patch.push_str("*** End Patch");
        fixture.patch("many", &patch);
        let diff = fixture.load("many");
        assert_eq!(diff.files.len(), 16);
        assert!(diff.limited);
        assert!(
            diff.files
                .iter()
                .map(|file| file.file.lines.len())
                .sum::<usize>()
                <= 2000
        );
        assert!(
            diff.files
                .iter()
                .any(|file| file.truncated || file.unavailable.is_some())
        );
    }

    #[test]
    fn tool_diff_worker_is_latest_only_and_old_poll_cannot_consume_new_scope() {
        let worker = SessionToolDiffWorker {
            mailbox: Arc::new(Mailbox::default()),
        };
        for generation in 0..100 {
            worker
                .submit(SessionToolDiffRequest {
                    storage_root: "store".into(),
                    workspace: "workspace".into(),
                    session_id: "session".into(),
                    tool_call_id: format!("tool{generation}"),
                    generation,
                })
                .unwrap();
        }
        let mut state = worker.mailbox.state.lock().unwrap();
        assert_eq!(state.pending.as_ref().unwrap().generation, 99);
        state.ready = Some(SessionToolDiffResponse {
            workspace: "workspace".into(),
            session_id: "session".into(),
            tool_call_id: "tool99".into(),
            generation: 99,
            result: Err("fixture".into()),
        });
        drop(state);
        assert!(
            worker
                .poll_latest(98, "session", Path::new("workspace"), "tool98")
                .is_none()
        );
        assert!(
            worker
                .poll_latest(99, "session", Path::new("workspace"), "tool99")
                .is_some()
        );
        assert!(
            worker
                .poll_latest(99, "session", Path::new("workspace"), "tool99")
                .is_none()
        );
    }

    #[test]
    fn tool_diff_worker_loads_without_host_io_and_rejects_workspace_and_tool_staleness() {
        let mut fixture = Fixture::new(&[("a", "before")]);
        fixture.write("selected", "a", "after");
        fixture.persist();
        let worker = SessionToolDiffWorker::spawn().unwrap();
        worker
            .submit(SessionToolDiffRequest {
                storage_root: fixture.storage.clone(),
                workspace: fixture.workspace.clone(),
                session_id: "session".into(),
                tool_call_id: "selected".into(),
                generation: 2,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            assert!(
                worker
                    .poll_latest(1, "session", &fixture.workspace, "selected")
                    .is_none()
            );
            assert!(
                worker
                    .poll_latest(2, "session", Path::new("wrong"), "selected")
                    .is_none()
            );
            assert!(
                worker
                    .poll_latest(2, "session", &fixture.workspace, "wrong")
                    .is_none()
            );
            if let Some(response) = worker.poll_latest(2, "session", &fixture.workspace, "selected")
            {
                assert_eq!(response.result.unwrap().files.len(), 1);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn tool_diff_unchanged_rename_retains_old_path_without_text_changes() {
        let mut fixture = Fixture::new(&[("old", "same\n")]);
        fixture.patch(
            "rename",
            "*** Begin Patch\n*** Update File: old\n*** Move to: new\n@@\n same\n*** End Patch",
        );
        let diff = fixture.load("rename");
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].file.old_path.as_deref(), Some("old"));
        assert_eq!(diff.files[0].file.status, "Renombrado");
        assert!(diff.files[0].file.lines.is_empty());
        assert_eq!(diff.files[0].file.additions, 0);
        assert_eq!(diff.files[0].file.deletions, 0);
    }

    #[test]
    fn tool_diff_pre_hook_unknown_input_and_post_hook_boundary_match_shared_replay() {
        for phase in ["pre_tool_use", "post_tool_use"] {
            let mut fixture = Fixture::new(&[("a", "before\n")]);
            fixture.start(
                "selected",
                "write_file",
                json!({"path":"a","content":"after\n"}),
            );
            fixture.event(AgentEventKind::HookStarted {
                tool_call_id: "selected".into(),
                hook_id: "effect".into(),
                phase: phase.into(),
            });
            fixture.event(AgentEventKind::HookFinished {
                tool_call_id: "selected".into(),
                hook_id: "effect".into(),
                phase: phase.into(),
                decision: "allow".into(),
            });
            fixture.finish("selected", false);
            let diff = fixture.load("selected");
            assert!(diff.limited);
            if phase == "pre_tool_use" {
                assert!(diff.files[0].unavailable.is_some());
            } else {
                assert!(diff.files[0].unavailable.is_none());
                assert_eq!(rows(&diff, ReviewLineKind::Removed), ["-before"]);
            }
            // A future patch still cannot reuse state after either opaque hook.
            fixture.patch(
                "next",
                "*** Begin Patch\n*** Update File: a\n@@\n-after\n+next\n*** End Patch",
            );
            assert!(fixture.load("next").files[0].unavailable.is_some());
        }
    }

    #[test]
    fn tool_diff_real_worker_discards_earlier_inflight_generation() {
        let mut fixture = Fixture::new(&[("a", "before")]);
        fixture.write("selected", "a", "after");
        fixture.persist();
        let worker = SessionToolDiffWorker::spawn().unwrap();
        for generation in 0..100 {
            worker
                .submit(SessionToolDiffRequest {
                    storage_root: fixture.storage.clone(),
                    workspace: fixture.workspace.clone(),
                    session_id: "session".into(),
                    tool_call_id: "selected".into(),
                    generation,
                })
                .unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(response) =
                worker.poll_latest(99, "session", &fixture.workspace, "selected")
            {
                assert_eq!(response.generation, 99);
                assert!(response.result.is_ok());
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            worker
                .poll_latest(0, "session", &fixture.workspace, "selected")
                .is_none()
        );
    }

    #[test]
    #[ignore = "opt-in offline timing for lazy journal projection; run with --ignored --nocapture"]
    fn benchmark_tool_diff_lazy_projection_long_journal() {
        let mut fixture = Fixture::new(&[("a", "before\n")]);
        for index in 0..9000 {
            fixture.event(AgentEventKind::ToolStarted {
                tool_call: ToolCall {
                    id: format!("read{index}"),
                    name: "read_file".into(),
                    arguments: json!({"path":"a"}),
                },
            });
            fixture.finish(&format!("read{index}"), false);
        }
        fixture.write("selected", "a", "after\n");
        fixture.persist();
        let started = Instant::now();
        for _ in 0..10 {
            assert_eq!(
                load_stored_tool_diff(&fixture.storage, "session", &fixture.workspace, "selected")
                    .unwrap()
                    .files
                    .len(),
                1
            );
        }
        eprintln!(
            "lazy tool diff offline debug: events={} loads=10 elapsed={:?}",
            fixture.events.len(),
            started.elapsed()
        );
    }
}
