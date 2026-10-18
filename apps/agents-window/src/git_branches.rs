//! Workspace branch selection. Git runs on one background worker; menu actions
//! carry a snapshot revision, never an unchecked branch name from the UI.

use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Child, Output, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Branch {
    pub reference: String,
    pub name: String,
    pub remote: bool,
    upstream: Option<String>,
    pub current: bool,
    pub other_worktree: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RepositoryHead {
    root: PathBuf,
    git_dir: PathBuf,
    reference: Option<String>,
    commit: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BranchSnapshot {
    head: RepositoryHead,
    pub label: String,
    pub branches: Vec<Branch>,
}

impl BranchSnapshot {
    pub fn worktree_root(&self) -> &Path {
        &self.head.root
    }

    pub(crate) fn head_commit(&self) -> Option<&str> {
        self.head.commit.as_deref()
    }

    pub(crate) fn head_reference(&self) -> Option<&str> {
        self.head.reference.as_deref()
    }
}

#[cfg(test)]
impl BranchSnapshot {
    /// A snapshot with no repository behind it, for a view fixture: on
    /// `branch`, or detached at a commit without one.
    pub(crate) fn fixture(branch: Option<&str>, others: &[&str]) -> Self {
        let local = |name: &str, current| Branch {
            reference: format!("refs/heads/{name}"),
            name: name.into(),
            remote: false,
            upstream: None,
            current,
            other_worktree: None,
        };
        Self {
            head: RepositoryHead {
                root: "/workspace/fixture".into(),
                git_dir: "/workspace/fixture/.git".into(),
                reference: branch.map(|name| format!("refs/heads/{name}")),
                commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            },
            label: branch.unwrap_or("HEAD separado").into(),
            branches: branch
                .map(|name| local(name, true))
                .into_iter()
                .chain(others.iter().map(|name| local(name, false)))
                .collect(),
        }
    }
}

#[derive(Default)]
pub(crate) struct BranchPickerView {
    pub workspace: Option<PathBuf>,
    pub revision: u64,
    pub open: bool,
    pub loading: bool,
    pub switching: Option<String>,
    pub query: String,
    pub error: Option<String>,
    pub blocked_reason: Option<&'static str>,
    pub snapshot: Option<BranchSnapshot>,
}

impl BranchPickerView {
    pub fn switching(&self) -> bool {
        self.switching.is_some()
    }
}

pub(crate) enum BranchOperation {
    List,
    Switch {
        snapshot: BranchSnapshot,
        branch: Branch,
    },
}

pub(crate) struct BranchRequest {
    pub workspace: PathBuf,
    pub revision: u64,
    pub operation: BranchOperation,
}

pub(crate) struct BranchResponse {
    pub workspace: PathBuf,
    pub revision: u64,
    /// Refresh even after a rejected switch, since HEAD may have changed outside
    /// the window. The switch error is kept separately from the refreshed list.
    pub snapshot: Result<BranchSnapshot, String>,
    pub switch_result: Option<Result<(), String>>,
}

pub(crate) struct BranchWorker {
    requests: SyncSender<BranchRequest>,
    responses: Receiver<BranchResponse>,
}

impl BranchWorker {
    pub fn spawn() -> io::Result<Self> {
        let (requests, request_rx) = mpsc::sync_channel::<BranchRequest>(4);
        let (response_tx, responses) = mpsc::channel();
        thread::Builder::new()
            .name("openide-git-branches".into())
            .spawn(move || {
                while let Ok(request) = request_rx.recv() {
                    let switch_result = match request.operation {
                        BranchOperation::List => None,
                        BranchOperation::Switch { snapshot, branch } => {
                            Some(switch_branch(&request.workspace, &snapshot, &branch))
                        }
                    };
                    let response = BranchResponse {
                        snapshot: load_branches(&request.workspace),
                        workspace: request.workspace,
                        revision: request.revision,
                        switch_result,
                    };
                    if response_tx.send(response).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            requests,
            responses,
        })
    }

    pub fn submit(&self, request: BranchRequest) -> Result<(), String> {
        self.requests
            .try_send(request)
            .map_err(|_| "El selector de ramas está ocupado o no está disponible".into())
    }

    pub fn poll_latest(
        &self,
        revision: u64,
        workspace: &Path,
    ) -> Result<Option<BranchResponse>, String> {
        let mut latest = None;
        loop {
            match self.responses.try_recv() {
                Ok(response)
                    if response.revision == revision && response.workspace == workspace =>
                {
                    latest = Some(response);
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => return Ok(latest),
                Err(TryRecvError::Disconnected) if latest.is_some() => return Ok(latest),
                Err(TryRecvError::Disconnected) => {
                    return Err("El proceso del selector de ramas dejó de responder".into());
                }
            }
        }
    }
}

fn repository_head(workspace: &Path) -> Result<RepositoryHead, String> {
    let root = git_text(workspace, &["rev-parse", "--show-toplevel"])?;
    let git_dir = git_text(workspace, &["rev-parse", "--absolute-git-dir"])?;
    let root = PathBuf::from(root)
        .canonicalize()
        .map_err(|error| format!("No se pudo resolver el repositorio: {error}"))?;
    let git_dir = PathBuf::from(git_dir)
        .canonicalize()
        .map_err(|error| format!("No se pudo resolver el directorio Git: {error}"))?;
    let reference = run_git(workspace, &["symbolic-ref", "--quiet", "HEAD"])?;
    let reference = if reference.status.success() {
        Some(output_text(&reference.stdout)?)
    } else if reference.status.code() == Some(1) {
        None // Detached HEAD.
    } else {
        return Err(git_error(&reference));
    };
    let commit = run_git(workspace, &["rev-parse", "--verify", "--quiet", "HEAD"])?;
    let commit = if commit.status.success() {
        Some(output_text(&commit.stdout)?)
    } else if reference.is_some() && commit.status.code() == Some(1) {
        None // Initial repository without commits.
    } else {
        return Err(git_error(&commit));
    };
    Ok(RepositoryHead {
        root,
        git_dir,
        reference,
        commit,
    })
}

pub(crate) fn load_branches(workspace: &Path) -> Result<BranchSnapshot, String> {
    let head = repository_head(workspace)?;
    let output = run_git(
        workspace,
        &[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname)%00%(symref)%00%(worktreepath)%00%(upstream)%00",
            "refs/heads/",
            "refs/remotes/",
        ],
    )?;
    if !output.status.success() {
        return Err(git_error(&output));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let fields = text.split_terminator('\0').collect::<Vec<_>>();
    let mut branches = Vec::new();
    for record in fields.chunks_exact(4) {
        let reference = record[0].trim_start_matches('\n');
        if !record[1].is_empty() {
            continue; // Remote HEAD aliases are not branches.
        }
        let (name, remote) = if let Some(name) = reference.strip_prefix("refs/heads/") {
            (name, false)
        } else if let Some(name) = reference.strip_prefix("refs/remotes/") {
            (name, true)
        } else {
            continue;
        };
        let other_worktree = (!record[2].is_empty()
            && Path::new(record[2]).canonicalize().ok().as_ref() != Some(&head.root))
        .then(|| record[2].to_owned());
        branches.push(Branch {
            reference: reference.to_owned(),
            name: name.to_owned(),
            remote,
            upstream: (!record[3].is_empty()).then(|| record[3].to_owned()),
            current: head.reference.as_deref() == Some(reference),
            other_worktree,
        });
    }
    branches.sort_by(|a, b| {
        a.remote
            .cmp(&b.remote)
            .then_with(|| b.current.cmp(&a.current))
            .then_with(|| a.name.cmp(&b.name))
    });
    let label = head
        .reference
        .as_deref()
        .and_then(|reference| reference.strip_prefix("refs/heads/"))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            head.commit
                .as_deref()
                .map(|commit| format!("HEAD · {}", commit.chars().take(7).collect::<String>()))
                .unwrap_or_else(|| "HEAD separado".into())
        });
    Ok(BranchSnapshot {
        head,
        label,
        branches,
    })
}

pub(crate) fn switch_branch(
    workspace: &Path,
    snapshot: &BranchSnapshot,
    branch: &Branch,
) -> Result<(), String> {
    if !snapshot
        .branches
        .iter()
        .any(|listed| listed.reference == branch.reference)
    {
        return Err("La rama no pertenece al listado del repositorio".into());
    }
    validate_snapshot(workspace, snapshot)?;
    // Reuse the local tracking branch if it already exists.
    let branch = if branch.remote {
        snapshot
            .branches
            .iter()
            .find(|local| {
                !local.remote && local.upstream.as_deref() == Some(branch.reference.as_str())
            })
            .unwrap_or(branch)
    } else {
        branch
    };
    if branch.current {
        return Ok(());
    }
    if branch.other_worktree.is_some() {
        return Err("Esa rama está abierta en otra carpeta de trabajo".into());
    }
    let mut args = vec![
        "switch",
        "--no-guess",
        "--no-overwrite-ignore",
        "--no-recurse-submodules",
    ];
    let target = if branch.remote {
        args.push("--track");
        branch.reference.as_str()
    } else {
        branch
            .reference
            .strip_prefix("refs/heads/")
            .ok_or("La referencia de la rama local no es válida")?
    };
    args.extend(["--", target]);
    let output = run_git(workspace, &args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_error(&output))
    }
}

pub(crate) fn validate_snapshot(workspace: &Path, snapshot: &BranchSnapshot) -> Result<(), String> {
    if repository_head(workspace)? != snapshot.head {
        return Err(
            "El repositorio o la rama activa cambió. Actualizá el estado antes de continuar."
                .into(),
        );
    }
    Ok(())
}

pub(crate) fn git_text(workspace: &Path, args: &[&str]) -> Result<String, String> {
    let output = run_git(workspace, args)?;
    if output.status.success() {
        output_text(&output.stdout)
    } else {
        Err(git_error(&output))
    }
}

fn output_text(bytes: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "Git devolvió un nombre que no se puede representar como texto")?;
    // Remove Git's line ending, preserving spaces that may belong to a path.
    let text = text.strip_suffix('\n').unwrap_or(text);
    Ok(text.strip_suffix('\r').unwrap_or(text).to_owned())
}

pub(crate) fn git_error(output: &Output) -> String {
    let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if error.is_empty() {
        "Git no pudo completar la operación".into()
    } else {
        error.chars().take(2048).collect()
    }
}

/// Bound pipe reads and the whole process, including post-checkout hooks.
/// No shell is involved, and Git's own conflict/worktree checks stay enabled.
pub(crate) fn run_git(workspace: &Path, args: &[&str]) -> Result<Output, String> {
    run_git_env(workspace, args, &[])
}

/// [`run_git`] with variables set after the inherited Git ones are removed,
/// such as a private `GIT_INDEX_FILE`.
pub(crate) fn run_git_env(
    workspace: &Path,
    args: &[&str],
    env: &[(&str, &std::ffi::OsStr)],
) -> Result<Output, String> {
    let mut command = crate::child_process::command_no_window("git");
    command
        .current_dir(workspace)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_NAMESPACE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("No se pudo ejecutar Git: {error}"))?;
    let (tx, rx) = mpsc::channel();
    let stdout = child.stdout.take().ok_or("Git no abrió su salida")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Git no abrió su salida de errores")?;
    for (index, pipe) in [
        (0, Box::new(stdout) as Box<dyn Read + Send>),
        (1, Box::new(stderr) as Box<dyn Read + Send>),
    ] {
        let tx = tx.clone();
        if let Err(error) = thread::Builder::new()
            .name("git-branch-output".into())
            .spawn(move || {
                let mut bytes = Vec::new();
                let result = pipe
                    .take(2 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| format!("No se pudo leer la salida de Git: {error}"))
                    .and_then(|_| {
                        if bytes.len() > 2 * 1024 * 1024 {
                            Err("El listado o la salida de Git excede el límite de 2 MB".into())
                        } else {
                            Ok(bytes)
                        }
                    });
                let _ = tx.send((index, result));
            })
        {
            stop_git(&mut child);
            return Err(format!("No se pudo leer la salida de Git: {error}"));
        }
    }
    drop(tx);
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut pipes = [None, None];
    let mut status = None;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(value) => status = value,
                Err(error) => {
                    stop_git(&mut child);
                    return Err(format!("No se pudo esperar a Git: {error}"));
                }
            }
        }
        if status.is_some() && pipes.iter().all(Option::is_some) {
            return Ok(Output {
                status: status.unwrap(),
                stdout: pipes[0].take().unwrap(),
                stderr: pipes[1].take().unwrap(),
            });
        }
        if Instant::now() >= deadline {
            stop_git(&mut child);
            return Err(
                "Git tardó demasiado. Actualiza el listado para comprobar la rama activa.".into(),
            );
        }
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok((index, Ok(bytes))) => pipes[index] = Some(bytes),
            Ok((_, Err(error))) => {
                stop_git(&mut child);
                return Err(error);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn stop_git(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        // Git owns a separate process group; terminate a stuck checkout hook too.
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = crate::child_process::command_no_window("taskkill")
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}
