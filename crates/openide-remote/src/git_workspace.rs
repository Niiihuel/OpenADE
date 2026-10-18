//! Typed Git operations; no shell, hooks, filters, or caller-supplied arguments.
use crate::{
    error::{ApiError, ApiResult},
    workspace,
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const MAX_OUTPUT: usize = 2 * 1024 * 1024;
pub fn conflict() -> ApiError {
    ApiError {
        status: StatusCode::CONFLICT,
        code: "git_conflict",
        message: "Git state changed; refresh and review before applying the operation".into(),
    }
}
fn git_error() -> ApiError {
    ApiError::bad(
        "Git could not perform this operation; check repository state and local Git identity",
    )
}

fn run(
    root: &Path,
    args: &[&str],
    input: Option<Vec<u8>>,
    index: Option<&Path>,
) -> ApiResult<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args([
            "--no-optional-locks",
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "diff.external=",
            "-c",
            "core.pager=cat",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Do not let parent environment redirect repository/index identity.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(key);
    }
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let mut child = command.spawn().map_err(|_| git_error())?;
    let reader = child.stdout.take().ok_or_else(git_error)?;
    let output = thread::spawn(move || {
        let mut bytes = Vec::new();
        reader
            .take(MAX_OUTPUT as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let writer = input.map(|input| {
        let mut stdin = child.stdin.take().expect("piped stdin");
        thread::spawn(move || stdin.write_all(&input))
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait().map_err(|_| git_error())? {
            Some(status) => break status,
            None if started.elapsed() < Duration::from_secs(15) => {
                thread::sleep(Duration::from_millis(10))
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(git_error());
            }
        }
    };
    if let Some(writer) = writer {
        writer
            .join()
            .map_err(|_| git_error())?
            .map_err(|_| git_error())?;
    }
    let bytes = output
        .join()
        .map_err(|_| git_error())?
        .map_err(|_| git_error())?;
    if !status.success() || bytes.len() > MAX_OUTPUT {
        return Err(git_error());
    }
    Ok(bytes)
}

fn text(root: &Path, args: &[&str]) -> ApiResult<String> {
    String::from_utf8(run(root, args, None, None)?)
        .map(|v| v.trim().to_owned())
        .map_err(|_| git_error())
}

#[derive(Serialize)]
pub struct GitFile {
    pub path: String,
    pub index_status: String,
    pub worktree_status: String,
    pub staged: bool,
    pub untracked: bool,
}
#[derive(Serialize)]
pub struct GitStatus {
    pub revision: String,
    pub branch: String,
    pub files: Vec<GitFile>,
    pub clean: bool,
    pub commit: Option<String>,
}

fn repository(root: &Path) -> ApiResult<()> {
    let top = text(root, &["rev-parse", "--show-toplevel"])?;
    if fs::canonicalize(top).ok().as_deref() != Some(root) {
        return Err(ApiError::bad("Project root must be the Git worktree root"));
    }
    Ok(())
}

fn head(root: &Path) -> String {
    text(root, &["rev-parse", "--verify", "HEAD"]).unwrap_or_default()
}
fn index_path(root: &Path) -> ApiResult<PathBuf> {
    let value = text(
        root,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )?;
    let path = PathBuf::from(value);
    if !path.is_absolute() || path.file_name().and_then(|v| v.to_str()) != Some("index") {
        return Err(git_error());
    }
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(git_error());
    }
    Ok(path)
}

fn read_index(path: &Path) -> ApiResult<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| git_error())?;
    let metadata = file.metadata().map_err(|_| git_error())?;
    if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
        return Err(ApiError::bad("Git index exceeds the supported size"));
    }
    let mut bytes = Vec::new();
    file.take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| git_error())?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(git_error());
    }
    Ok(bytes)
}

pub fn status(root: &Path) -> ApiResult<GitStatus> {
    repository(root)?;
    let raw = run(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
        None,
        None,
    )?;
    let mut files = Vec::new();
    let mut fingerprint = Vec::new();
    fingerprint.extend_from_slice(head(root).as_bytes());
    fingerprint.extend_from_slice(&raw);
    let index = index_path(root)?;
    if index.exists() {
        let data = read_index(&index)?;
        fingerprint.extend_from_slice(workspace::hash(&data).as_bytes());
    }
    for record in raw.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        if record.len() < 4 || files.len() >= 2000 {
            return Err(ApiError::bad(
                "Git status exceeds 2000 files or contains an unsupported path",
            ));
        }
        let path = std::str::from_utf8(&record[3..])
            .map_err(|_| git_error())?
            .to_owned();
        workspace::validate_path(&path, false)?;
        // Include working contents so a second edit to an already-dirty file
        // changes the revision even when porcelain status stays identical.
        match workspace::bytes(root, &path) {
            Ok(bytes) => fingerprint.extend_from_slice(workspace::hash(&bytes).as_bytes()),
            Err(e) if e.code == "file_not_found" => fingerprint.extend_from_slice(b"missing"),
            Err(_) => {
                // Large/binary/symlink rows remain visible but cannot be staged
                // through this API. Metadata makes ordinary changes observable.
                let m = fs::symlink_metadata(root.join(&path)).map_err(|_| git_error())?;
                fingerprint.extend_from_slice(
                    format!("unsupported:{}:{:?}", m.len(), m.modified().ok()).as_bytes(),
                );
            }
        }
        files.push(GitFile {
            path,
            index_status: char::from(record[0]).to_string(),
            worktree_status: char::from(record[1]).to_string(),
            staged: !matches!(record[0], b' ' | b'?'),
            untracked: &record[..2] == b"??",
        });
    }
    let branch = text(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .unwrap_or_else(|_| "(detached)".into());
    Ok(GitStatus {
        revision: workspace::hash(&fingerprint),
        branch,
        clean: files.is_empty(),
        files,
        commit: None,
    })
}

pub fn diff(root: &Path, path: &str, staged: bool) -> ApiResult<Value> {
    repository(root)?;
    workspace::validate_path(path, false)?;
    let state = status(root)?;
    let entry = state.files.iter().find(|f| f.path == path);
    let bytes = if !staged && entry.is_some_and(|f| f.untracked) {
        let bytes = workspace::bytes(root, path)?;
        let content =
            String::from_utf8(bytes).map_err(|_| ApiError::bad("Binary diff is not available"))?;
        if content.contains('\0') {
            return Err(ApiError::bad("Binary diff is not available"));
        }
        format!(
            "--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n{}",
            content.lines().count(),
            content
                .lines()
                .map(|l| format!("+{l}\n"))
                .collect::<String>()
        )
        .into_bytes()
    } else {
        let mut args = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
        ];
        if staged {
            args.push("--cached");
        }
        args.extend(["--", path]);
        run(root, &args, None, None)?
    };
    let truncated = bytes.len() > 512 * 1024;
    let result = String::from_utf8_lossy(&bytes[..bytes.len().min(512 * 1024)]);
    Ok(json!({"path":path,"diff":result,"truncated":truncated}))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeIndex {
    pub request_id: String,
    pub paths: Vec<String>,
    pub expected_revision: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub request_id: String,
    pub message: String,
    pub expected_revision: String,
}

pub fn validate_revision(root: &Path, expected: &str) -> ApiResult<GitStatus> {
    let state = status(root)?;
    if state.revision != expected {
        return Err(conflict());
    }
    if state.files.iter().any(|f| {
        f.index_status == "U"
            || f.worktree_status == "U"
            || matches!(
                (f.index_status.as_str(), f.worktree_status.as_str()),
                ("A", "A") | ("D", "D")
            )
    }) {
        return Err(ApiError::bad(
            "Resolve Git conflicts locally before using this operation",
        ));
    }
    Ok(state)
}

struct IndexGuard {
    path: PathBuf,
    lock_path: PathBuf,
    index: PathBuf,
}
impl IndexGuard {
    fn acquire(root: &Path) -> ApiResult<Self> {
        let index = index_path(root)?;
        let lock_path = index.with_file_name("index.lock");
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let _lock = opts.open(&lock_path).map_err(|_| conflict())?;
        let path = index.with_file_name(format!("eas-index-{}", uuid::Uuid::new_v4()));
        let guard = Self {
            path,
            lock_path,
            index,
        };
        if guard.index.exists() {
            let bytes = read_index(&guard.index)?;
            let mut file = opts.open(&guard.path).map_err(|_| git_error())?;
            file.write_all(&bytes).map_err(|_| git_error())?;
            file.sync_all().map_err(|_| git_error())?;
        } else {
            // Keep the real index.lock held while initializing a separate index.
            run(root, &["read-tree", "--empty"], None, Some(&guard.path))?;
        }
        Ok(guard)
    }
    fn publish(self) -> ApiResult<()> {
        File::open(&self.path)
            .and_then(|f| f.sync_all())
            .map_err(|_| git_error())?;
        fs::rename(&self.path, &self.index).map_err(|_| git_error())?;
        Ok(())
    }
}
impl Drop for IndexGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(&self.lock_path);
    }
}

pub fn change_index(root: &Path, request: &ChangeIndex, stage: bool) -> ApiResult<Value> {
    validate_change(root, request, stage)?;
    let guard = IndexGuard::acquire(root)?;
    validate_revision(root, &request.expected_revision)?;
    for path in &request.paths {
        if stage {
            match workspace::bytes(root, path) {
                Ok(bytes) => {
                    let object = String::from_utf8(run(
                        root,
                        &["hash-object", "-w", "--stdin", "--no-filters"],
                        Some(bytes),
                        None,
                    )?)
                    .map_err(|_| git_error())?;
                    #[cfg(unix)]
                    let mode = {
                        use std::os::unix::fs::PermissionsExt;
                        if fs::metadata(root.join(path))
                            .map_err(|_| git_error())?
                            .permissions()
                            .mode()
                            & 0o111
                            != 0
                        {
                            "100755"
                        } else {
                            "100644"
                        }
                    };
                    #[cfg(not(unix))]
                    let mode = "100644";
                    run(
                        root,
                        &[
                            "update-index",
                            "--add",
                            "--cacheinfo",
                            mode,
                            object.trim(),
                            path,
                        ],
                        None,
                        Some(&guard.path),
                    )?;
                }
                Err(e) if e.code == "file_not_found" => {
                    run(
                        root,
                        &["update-index", "--force-remove", "--", path],
                        None,
                        Some(&guard.path),
                    )?;
                }
                Err(e) => return Err(e),
            }
        } else {
            let tree = if head(root).is_empty() {
                Vec::new()
            } else {
                run(root, &["ls-tree", "-z", "HEAD", "--", path], None, None)?
            };
            if tree.is_empty() {
                run(
                    root,
                    &["update-index", "--force-remove", "--", path],
                    None,
                    Some(&guard.path),
                )?;
            } else {
                let entry = std::str::from_utf8(&tree).map_err(|_| git_error())?;
                let fields = entry.split_whitespace().take(3).collect::<Vec<_>>();
                if fields.len() != 3 || !matches!(fields[0], "100644" | "100755") {
                    return Err(ApiError::bad(
                        "Only regular files can be unstaged through this API",
                    ));
                }
                run(
                    root,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        fields[0],
                        fields[2],
                        path,
                    ],
                    None,
                    Some(&guard.path),
                )?;
            }
        }
    }
    validate_revision(root, &request.expected_revision)?;
    guard.publish()?;
    serde_json::to_value(status(root)?).map_err(|_| ApiError::unavailable())
}

pub fn validate_change(root: &Path, request: &ChangeIndex, stage: bool) -> ApiResult<()> {
    if request.paths.is_empty() || request.paths.len() > 200 {
        return Err(ApiError::bad("Select between 1 and 200 file paths"));
    }
    for path in &request.paths {
        workspace::validate_path(path, false)?;
    }
    validate_revision(root, &request.expected_revision)?;
    for path in &request.paths {
        if stage {
            match workspace::bytes(root, path) {
                Ok(_) => {}
                Err(e) if e.code == "file_not_found" => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

pub fn validate_commit(root: &Path, request: &Commit) -> ApiResult<()> {
    if request.message.trim().is_empty()
        || request.message.len() > 16 * 1024
        || request.message.contains('\0')
    {
        return Err(ApiError::bad("Commit message must contain 1–16384 bytes"));
    }
    let state = validate_revision(root, &request.expected_revision)?;
    if !state.files.iter().any(|f| f.staged) {
        return Err(ApiError::bad("There are no staged changes to commit"));
    }
    Ok(())
}
pub fn commit(root: &Path, request: &Commit) -> ApiResult<Value> {
    validate_commit(root, request)?;
    let guard = IndexGuard::acquire(root)?;
    validate_revision(root, &request.expected_revision)?;
    let prior = head(root);
    let tree = String::from_utf8(run(root, &["write-tree"], None, Some(&guard.path))?)
        .map_err(|_| git_error())?;
    let tree = tree.trim();
    let mut args = vec!["commit-tree", tree];
    if !prior.is_empty() {
        args.extend(["-p", &prior]);
    }
    let oid = String::from_utf8(run(
        root,
        &args,
        Some(request.message.as_bytes().to_vec()),
        None,
    )?)
    .map_err(|_| git_error())?;
    let oid = oid.trim();
    validate_revision(root, &request.expected_revision)?;
    let expected = if prior.is_empty() {
        "0".repeat(oid.len())
    } else {
        prior
    };
    // A process timeout may occur after a successful ref update. Preserve the
    // uncertain ledger in that case rather than claiming a no-effect conflict.
    run(
        root,
        &[
            "update-ref",
            "-m",
            "EAS remote commit",
            "HEAD",
            oid,
            &expected,
        ],
        None,
        None,
    )?;
    let mut state = status(root)?;
    state.commit = Some(oid.to_owned());
    serde_json::to_value(state).map_err(|_| ApiError::unavailable())
}
