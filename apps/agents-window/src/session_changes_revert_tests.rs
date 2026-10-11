//! Reverting against real captures, made by the snapshot executor over
//! `LocalTools` in a temporary directory.
use super::*;
use openide_harness::{LocalTools, ToolContext, ToolOutput};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct Fixture {
    _temp: tempfile::TempDir,
    storage: PathBuf,
    workspace: PathBuf,
    events: Vec<AgentEvent>,
}

fn write(id: &str, path: &str, content: &str) -> ToolCall {
    ToolCall { id: id.into(), name: "write_file".into(), arguments: json!({"path": path, "content": content}) }
}

fn patch(id: &str, body: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "apply_patch".into(),
        arguments: json!({"patch": format!("*** Begin Patch\n{body}*** End Patch\n")}),
    }
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let storage = temp.path().join("store");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&storage).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        let events = vec![AgentEvent::new(
            "session",
            1,
            AgentEventKind::SessionStarted { workspace: workspace.clone() },
        )];
        Self { _temp: temp, storage, workspace, events }
    }

    fn event(&mut self, kind: AgentEventKind) {
        self.events.push(AgentEvent::new("session", self.events.len() as u64 + 1, kind));
        let mut bytes = Vec::new();
        for event in &self.events {
            serde_json::to_writer(&mut bytes, event).unwrap();
            bytes.push(b'\n');
        }
        fs::write(self.storage.join("session.jsonl"), bytes).unwrap();
    }

    /// Where a rewind that removes everything from here on would cut.
    fn cut(&self) -> u64 {
        self.events.len() as u64 + 1
    }

    fn run(&mut self, call: ToolCall) -> ToolOutput {
        self.event(AgentEventKind::ToolStarted { tool_call: call.clone() });
        let executor = with_file_snapshots(
            Arc::new(LocalTools::new().with_write_access(true)),
            self.storage.clone(),
            "session".into(),
            self.workspace.clone(),
        );
        let context = ToolContext { workspace: self.workspace.clone(), cancel: CancellationToken::new() };
        let result = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(executor.execute(&call, &context))
            .unwrap_or_else(|error| ToolOutput::error(error.to_string()));
        self.event(AgentEventKind::ToolFinished {
            tool_call_id: call.id,
            output: result.content.clone(),
            is_error: result.is_error,
        });
        result
    }

    /// A call whose effect is not recorded; nothing runs.
    fn opaque(&mut self, id: &str, name: &str) {
        self.event(AgentEventKind::ToolStarted {
            tool_call: ToolCall { id: id.into(), name: name.into(), arguments: json!({}) },
        });
        self.event(AgentEventKind::ToolFinished { tool_call_id: id.into(), output: String::new(), is_error: false });
    }

    fn preflight(&self, cut: u64) -> RevertPreflight {
        self.preflight_with(cut, &[])
    }

    fn preflight_with(&self, cut: u64, unsaved: &[&str]) -> RevertPreflight {
        let unsaved = unsaved.iter().map(|path| (*path).to_owned()).collect();
        revert_preflight(&self.storage, "session", &self.workspace, cut, &unsaved).unwrap()
    }

    fn put(&self, path: &str, content: &str) {
        fs::write(self.workspace.join(path), content).unwrap();
    }

    fn disk(&self, path: &str) -> Option<String> {
        fs::read_to_string(self.workspace.join(path)).ok()
    }

    fn revert(&self, files: &[RevertFile]) -> Result<(), (String, Vec<String>)> {
        revert_files(&self.storage, &self.workspace, files)
    }
}

fn paths(files: &[RevertFile]) -> Vec<&str> {
    files.iter().map(|file| file.path.as_str()).collect()
}

#[test]
fn edits_creations_and_deletions_return_to_the_cut_and_come_back_on_reapply() {
    let mut fixture = Fixture::new();
    fixture.put("edited.txt", "original\n");
    fixture.put("twice.txt", "zero\n");
    fixture.put("gone.txt", "kept\n");
    fixture.put("untouched.txt", "same\n");
    // An edit before the cut is not part of what a revert takes back.
    assert!(!fixture.run(write("early", "twice.txt", "one\n")).is_error);
    let cut = fixture.cut();
    assert!(!fixture.run(write("w1", "edited.txt", "changed\n")).is_error);
    assert!(!fixture.run(write("w2", "twice.txt", "two\n")).is_error);
    // A failed call in the chain changed nothing and does not break it.
    assert!(fixture.run(patch("bad", "*** Update File: twice.txt\n@@\n-not there\n+x\n")).is_error);
    assert!(!fixture.run(write("w3", "twice.txt", "three\n")).is_error);
    assert!(!fixture.run(write("w4", "created.txt", "new\n")).is_error);
    assert!(!fixture.run(patch("p1", "*** Delete File: gone.txt\n")).is_error);
    // Written and written back: nothing to restore.
    assert!(!fixture.run(write("w5", "untouched.txt", "other\n")).is_error);
    assert!(!fixture.run(write("w6", "untouched.txt", "same\n")).is_error);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(fixture.workspace.join("edited.txt"), fs::Permissions::from_mode(0o640)).unwrap();
    }

    let preflight = fixture.preflight(cut);
    assert_eq!(preflight.skipped, vec![]);
    assert_eq!(paths(&preflight.restorable), ["created.txt", "edited.txt", "gone.txt", "twice.txt"]);
    fixture.revert(&preflight.restorable).unwrap();
    assert_eq!(fixture.disk("edited.txt").as_deref(), Some("original\n"));
    assert_eq!(fixture.disk("twice.txt").as_deref(), Some("one\n"), "the first removed call's before");
    assert_eq!(fixture.disk("gone.txt").as_deref(), Some("kept\n"));
    assert_eq!(fixture.disk("created.txt"), None);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(fixture.workspace.join("edited.txt")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
    }
    assert_eq!(fs::read_dir(&fixture.workspace).unwrap().count(), 4, "no temporary file is left");

    // The undo: a path changed since the revert keeps its content.
    fixture.put("edited.txt", "the replacement turn wrote this\n");
    assert_eq!(
        reapply_reverted_files(&fixture.storage, &fixture.workspace, &preflight.restorable),
        ["edited.txt"]
    );
    assert_eq!(fixture.disk("edited.txt").as_deref(), Some("the replacement turn wrote this\n"));
    assert_eq!(fixture.disk("twice.txt").as_deref(), Some("three\n"));
    assert_eq!(fixture.disk("created.txt").as_deref(), Some("new\n"));
    assert_eq!(fixture.disk("gone.txt"), None);
}

#[test]
fn paths_that_cannot_be_trusted_are_skipped_with_their_reason_and_never_written() {
    let mut fixture = Fixture::new();
    for path in ["manual.txt", "unsaved.txt", "gap.txt", "between.txt", "ok.txt"] {
        fixture.put(path, "original\n");
    }
    // Larger than a capture keeps.
    fixture.put("big.txt", &"x".repeat(MAX_FILE_BYTES + 1));
    let cut = fixture.cut();
    for path in ["manual.txt", "unsaved.txt", "between.txt", "ok.txt"] {
        assert!(!fixture.run(write(path, path, "agent\n")).is_error);
    }
    // Its captures are lost: whatever the disk holds, the chain has a gap.
    let captures = fixture.storage.join("session-tool-snapshots");
    let kept: Vec<_> = fs::read_dir(&captures).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert!(!fixture.run(write("gap-1", "gap.txt", "agent\n")).is_error);
    for entry in fs::read_dir(&captures).unwrap() {
        let path = entry.unwrap().path();
        if !kept.contains(&path) && path.to_string_lossy().ends_with(".after.json") {
            fs::remove_file(path).unwrap();
        }
    }
    assert!(!fixture.run(write("gap-2", "gap.txt", "agent again\n")).is_error);
    assert!(!fixture.run(write("big", "big.txt", "small\n")).is_error);
    // Something else wrote between two recorded edits.
    fixture.put("between.txt", "a command wrote this\n");
    assert!(!fixture.run(write("between-2", "between.txt", "agent again\n")).is_error);
    // Outside the project by name: never reaches the tool, never listed as restorable.
    let outside = fixture.workspace.parent().unwrap().join("outside.txt");
    fs::write(&outside, "outside\n").unwrap();
    fixture.run(write("escape", "../outside.txt", "agent\n"));
    fixture.opaque("c1", "run_command");
    fixture.opaque("c2", "run_command");
    fixture.opaque("m1", "mcp__github__create_issue");
    fixture.opaque("r1", "read_file");

    fixture.put("manual.txt", "edited by hand\n");
    let preflight = fixture.preflight_with(cut, &["unsaved.txt"]);
    assert_eq!(paths(&preflight.restorable), ["ok.txt"]);
    let mut skipped = preflight.skipped.clone();
    skipped.sort();
    assert_eq!(
        skipped,
        [
            ("../outside.txt".to_owned(), RevertSkip::OutsideProject),
            ("between.txt".to_owned(), RevertSkip::ModifiedLater),
            ("big.txt".to_owned(), RevertSkip::IncompleteCapture),
            ("gap.txt".to_owned(), RevertSkip::IncompleteCapture),
            ("manual.txt".to_owned(), RevertSkip::ModifiedLater),
            ("unsaved.txt".to_owned(), RevertSkip::UnsavedInEditor),
        ]
    );
    assert_eq!(preflight.opaque, OpaqueEffects { commands: 2, mcp: 1, other: 0 });

    fixture.revert(&preflight.restorable).unwrap();
    assert_eq!(fixture.disk("manual.txt").as_deref(), Some("edited by hand\n"));
    assert_eq!(fixture.disk("unsaved.txt").as_deref(), Some("agent\n"));
    assert_eq!(fixture.disk("gap.txt").as_deref(), Some("agent again\n"));
    assert_eq!(fixture.disk("between.txt").as_deref(), Some("agent again\n"));
    assert_eq!(fixture.disk("big.txt").as_deref(), Some("small\n"));
    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside\n");
    assert_eq!(fixture.disk("ok.txt").as_deref(), Some("original\n"));
}

#[test]
fn bytes_the_guarded_write_would_not_reproduce_are_skipped() {
    let mut fixture = Fixture::new();
    fs::write(fixture.workspace.join("latin.txt"), [b'a', 0xff, b'\n']).unwrap();
    let cut = fixture.cut();
    assert!(!fixture.run(write("w", "latin.txt", "text\n")).is_error);
    let preflight = fixture.preflight(cut);
    assert_eq!(preflight.restorable, vec![]);
    assert_eq!(preflight.skipped, [("latin.txt".to_owned(), RevertSkip::NotText)]);
}

#[cfg(unix)]
#[test]
fn a_path_reached_through_a_link_is_outside_the_project_and_is_never_written() {
    let mut fixture = Fixture::new();
    fs::create_dir(fixture.workspace.join("dir")).unwrap();
    fixture.put("dir/a.txt", "original\n");
    let cut = fixture.cut();
    assert!(!fixture.run(write("w", "dir/a.txt", "agent\n")).is_error);
    let mut files = fixture.preflight(cut).restorable;
    assert_eq!(paths(&files), ["dir/a.txt"]);

    // The folder becomes a link to one outside holding the same content.
    let outside = fixture.workspace.parent().unwrap().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("a.txt"), "agent\n").unwrap();
    fs::remove_dir_all(fixture.workspace.join("dir")).unwrap();
    std::os::unix::fs::symlink(&outside, fixture.workspace.join("dir")).unwrap();
    let preflight = fixture.preflight(cut);
    assert_eq!(preflight.restorable, vec![]);
    assert_eq!(preflight.skipped, [("dir/a.txt".to_owned(), RevertSkip::OutsideProject)]);
    // Even handed a stale preflight, the write itself refuses.
    assert!(fixture.revert(&files).is_err());
    files[0].path = "../outside/a.txt".into();
    assert!(fixture.revert(&files).is_err());
    assert_eq!(fs::read_to_string(outside.join("a.txt")).unwrap(), "agent\n");
}

#[test]
fn a_failed_write_puts_back_the_ones_already_done() {
    let mut fixture = Fixture::new();
    fixture.put("a.txt", "original a\n");
    fixture.put("b.txt", "original b\n");
    let cut = fixture.cut();
    assert!(!fixture.run(write("a", "a.txt", "agent a\n")).is_error);
    assert!(!fixture.run(write("b", "b.txt", "agent b\n")).is_error);
    let files = fixture.preflight(cut).restorable;
    // The disk changes after the preflight: the guard refuses the second.
    fixture.put("b.txt", "edited by hand\n");
    let (error, left) = fixture.revert(&files).unwrap_err();
    assert!(error.starts_with("b.txt: "), "{error}");
    assert_eq!(left, Vec::<String>::new());
    assert_eq!(fixture.disk("a.txt").as_deref(), Some("agent a\n"));
    assert_eq!(fixture.disk("b.txt").as_deref(), Some("edited by hand\n"));
}
