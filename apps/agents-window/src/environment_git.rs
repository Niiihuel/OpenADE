//! Git actions for the local environment card. One bounded worker serializes
//! commands, and effectful requests carry the exact repository/HEAD/index
//! snapshot the user reviewed. No action stages files implicitly or forces a
//! branch switch, push, or worktree replacement.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
};

use crate::git_branches::{self, Branch, BranchSnapshot};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub current: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GitSnapshot {
    pub branches: BranchSnapshot,
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub remotes: Vec<String>,
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicts: usize,
    pub ahead: usize,
    pub behind: usize,
    pub worktrees: Vec<WorktreeInfo>,
    index_tree: Option<String>,
    push_urls: BTreeMap<String, String>,
    tracking_remote: Option<String>,
    tracking_branch: Option<String>,
}

impl GitSnapshot {
    /// Use the configured tracking remote; otherwise only choose when there
    /// is a single unambiguous remote. Multiple remotes require a UI choice.
    pub fn default_remote(&self) -> Option<&str> {
        self.tracking_remote
            .as_deref()
            .filter(|configured| self.remotes.iter().any(|remote| remote == configured))
            .or_else(|| (self.remotes.len() == 1).then(|| self.remotes[0].as_str()))
    }

    pub fn current_branch(&self) -> Option<&str> {
        self.branches
            .head_reference()
            .and_then(|reference| reference.strip_prefix("refs/heads/"))
    }

    pub fn default_push_branch(&self) -> Option<&str> {
        self.tracking_branch
            .as_deref()
            .filter(|_| self.default_remote() == self.tracking_remote.as_deref())
            .or_else(|| self.current_branch())
    }

    /// Offer a GitHub link only for the exact known host and one repository.
    /// Other remotes/forks remain local comparisons instead of guessing a
    /// target repository or exposing a credential-bearing URL in the view.
    pub fn compare_url(&self, reference: &str) -> Option<String> {
        if !self
            .branches
            .branches
            .iter()
            .any(|branch| branch.reference == reference)
        {
            return None;
        }
        let remote = self.default_remote()?;
        let push_url = self.push_urls.get(remote)?;
        if push_url.lines().count() != 1 {
            return None;
        }
        let repository = crate::github::parse_remote(push_url, remote)?;
        let base = reference
            .strip_prefix("refs/heads/")
            .or_else(|| reference.strip_prefix(&format!("refs/remotes/{remote}/")))?;
        let current = self.current_branch()?;
        let mut url = reqwest::Url::parse(&repository.web_url()).ok()?;
        url.path_segments_mut()
            .ok()?
            .push("compare")
            .push(&format!("{base}...{current}"));
        Some(url.to_string())
    }
}

#[cfg(test)]
impl GitSnapshot {
    /// A snapshot with no repository behind it, for a view fixture.
    pub(crate) fn fixture(
        branch: Option<&str>,
        upstream: Option<&str>,
        remotes: &[&str],
        ahead: usize,
        behind: usize,
    ) -> Self {
        let branches = BranchSnapshot::fixture(branch, &["main"]);
        Self {
            head: branches.head_commit().map(str::to_owned),
            branches,
            upstream: upstream.map(str::to_owned),
            remotes: remotes.iter().map(|remote| (*remote).to_owned()).collect(),
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicts: 0,
            ahead,
            behind,
            worktrees: Vec::new(),
            index_tree: None,
            push_urls: BTreeMap::new(),
            tracking_remote: upstream
                .and_then(|upstream| upstream.split_once('/'))
                .map(|(remote, _)| remote.to_owned()),
            tracking_branch: upstream
                .and_then(|upstream| upstream.split_once('/'))
                .map(|(_, branch)| branch.to_owned()),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct BranchComparison {
    pub reference: String,
    pub ahead: usize,
    pub behind: usize,
    pub files: usize,
    pub additions: usize,
    pub deletions: usize,
}

pub(crate) enum Operation {
    Refresh,
    Switch {
        snapshot: GitSnapshot,
        branch: Branch,
    },
    CreateBranch {
        snapshot: GitSnapshot,
        name: String,
    },
    CreateWorktree {
        snapshot: GitSnapshot,
        name: String,
        path: PathBuf,
    },
    Commit {
        snapshot: GitSnapshot,
        message: String,
    },
    Push {
        snapshot: GitSnapshot,
        remote: String,
        branch: String,
        set_upstream: bool,
    },
    Compare {
        snapshot: GitSnapshot,
        reference: String,
    },
    /// Update the remote-tracking refs of one remote. Nothing local moves.
    Fetch {
        snapshot: GitSnapshot,
        remote: String,
    },
    /// Fast-forward the current branch to its upstream, or fail.
    Pull {
        snapshot: GitSnapshot,
    },
}

pub(crate) struct Request {
    pub workspace: PathBuf,
    pub revision: u64,
    pub operation: Operation,
}

pub(crate) struct Response {
    pub workspace: PathBuf,
    pub revision: u64,
    pub snapshot: Result<GitSnapshot, String>,
    pub outcome: Option<Result<String, String>>,
    pub comparison: Option<Result<BranchComparison, String>>,
}

pub(crate) struct Worker {
    requests: SyncSender<Request>,
    responses: Receiver<Response>,
}

impl Worker {
    pub fn spawn() -> io::Result<Self> {
        let (requests, request_rx) = mpsc::sync_channel::<Request>(2);
        let (response_tx, responses) = mpsc::channel();
        thread::Builder::new()
            .name("openide-environment-git".into())
            .spawn(move || {
                while let Ok(request) = request_rx.recv() {
                    let mut comparison = None;
                    let outcome = match request.operation {
                        Operation::Refresh => None,
                        Operation::Switch { snapshot, branch } => Some(
                            git_branches::switch_branch(
                                &request.workspace,
                                &snapshot.branches,
                                &branch,
                            )
                            .map(|()| format!("Rama activa: {}", branch.name)),
                        ),
                        Operation::CreateBranch { snapshot, name } => Some(
                            create_branch(&request.workspace, &snapshot, &name)
                                .map(|()| format!("Rama creada: {name}")),
                        ),
                        Operation::CreateWorktree {
                            snapshot,
                            name,
                            path,
                        } => Some(
                            create_worktree(&request.workspace, &snapshot, &name, &path)
                                .map(|()| format!("Worktree creado en {}", path.display())),
                        ),
                        Operation::Commit { snapshot, message } => Some(
                            commit_staged(&request.workspace, &snapshot, &message)
                                .map(|id| format!("Commit creado: {}", &id[..id.len().min(8)])),
                        ),
                        Operation::Push {
                            snapshot,
                            remote,
                            branch,
                            set_upstream,
                        } => Some(
                            push(
                                &request.workspace,
                                &snapshot,
                                &remote,
                                &branch,
                                set_upstream,
                            )
                            .map(|()| format!("Push completado: {remote}/{branch}")),
                        ),
                        Operation::Fetch { snapshot, remote } => Some(
                            fetch(&request.workspace, &snapshot, &remote)
                                .map(|()| format!("Referencias actualizadas desde {remote}")),
                        ),
                        Operation::Pull { snapshot } => Some(
                            pull(&request.workspace, &snapshot)
                                .map(|upstream| format!("Rama al día con {upstream}")),
                        ),
                        Operation::Compare {
                            snapshot,
                            reference,
                        } => {
                            comparison = Some(compare(&request.workspace, &snapshot, &reference));
                            None
                        }
                    };
                    let response = Response {
                        snapshot: load_snapshot(&request.workspace),
                        workspace: request.workspace,
                        revision: request.revision,
                        outcome,
                        comparison,
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

    pub fn submit(&self, request: Request) -> Result<(), String> {
        self.requests
            .try_send(request)
            .map_err(|_| "Git está ocupado. Esperá a que termine la operación actual.".into())
    }

    pub fn poll_latest(&self, revision: u64, workspace: &Path) -> Result<Option<Response>, String> {
        let mut latest = None;
        loop {
            match self.responses.try_recv() {
                Ok(response)
                    if response.revision == revision && response.workspace == workspace =>
                {
                    latest = Some(response)
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => return Ok(latest),
                Err(TryRecvError::Disconnected) if latest.is_some() => return Ok(latest),
                Err(TryRecvError::Disconnected) => {
                    return Err("El proceso de Git dejó de responder".into())
                }
            }
        }
    }
}

#[derive(Default)]
struct Status {
    staged: usize,
    unstaged: usize,
    untracked: usize,
    conflicts: usize,
    upstream: Option<String>,
    ahead: usize,
    behind: usize,
}

fn parse_status(bytes: &[u8]) -> Result<Status, String> {
    let mut status = Status::default();
    let mut records = bytes.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if let Some(upstream) = record.strip_prefix(b"# branch.upstream ") {
            status.upstream = Some(
                std::str::from_utf8(upstream)
                    .map_err(|_| "El upstream contiene texto no representable")?
                    .to_owned(),
            );
        } else if let Some(counts) = record.strip_prefix(b"# branch.ab ") {
            let counts = String::from_utf8_lossy(counts);
            let mut parts = counts.split_whitespace();
            status.ahead = parts
                .next()
                .and_then(|n| n.strip_prefix('+'))
                .and_then(|n| n.parse().ok())
                .ok_or("Git devolvió un contador ahead inválido")?;
            status.behind = parts
                .next()
                .and_then(|n| n.strip_prefix('-'))
                .and_then(|n| n.parse().ok())
                .ok_or("Git devolvió un contador behind inválido")?;
        } else {
            match record.first().copied() {
                Some(b'1' | b'2') if record.len() >= 4 => {
                    status.staged += usize::from(record[2] != b'.');
                    status.unstaged += usize::from(record[3] != b'.');
                    if record[0] == b'2' {
                        records.next();
                    }
                }
                Some(b'u') => status.conflicts += 1,
                Some(b'?') => status.untracked += 1,
                _ => {}
            }
        }
    }
    Ok(status)
}

pub(crate) fn load_snapshot(workspace: &Path) -> Result<GitSnapshot, String> {
    use git_branches::{git_error, git_text, run_git};
    let branches = git_branches::load_branches(workspace)?;
    let before_tree = run_git(workspace, &["write-tree"])?;
    let output = run_git(
        workspace,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
        ],
    )?;
    if !output.status.success() {
        return Err(git_error(&output));
    }
    let status = parse_status(&output.stdout)?;
    let index_tree = if status.conflicts > 0 {
        None
    } else if before_tree.status.success() {
        let before = String::from_utf8_lossy(&before_tree.stdout)
            .trim()
            .to_owned();
        let after = git_text(workspace, &["write-tree"])?;
        if before != after {
            return Err(
                "El área de preparación cambió durante la lectura. Actualizá el estado.".into(),
            );
        }
        Some(after)
    } else {
        return Err(git_error(&before_tree));
    };
    let remotes = git_text(workspace, &["remote"])?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if remotes.len() > 128 {
        return Err("El repositorio contiene demasiados remotos para esta vista".into());
    }
    let mut push_urls = BTreeMap::new();
    for remote in &remotes {
        push_urls.insert(
            remote.clone(),
            git_text(workspace, &["remote", "get-url", "--push", "--all", remote])?,
        );
    }
    let mut tracking_remote = None;
    let mut tracking_branch = None;
    if let Some(local) = branches
        .head_reference()
        .and_then(|reference| reference.strip_prefix("refs/heads/"))
    {
        tracking_remote = optional_config(workspace, &format!("branch.{local}.remote"))?;
        tracking_branch = optional_config(workspace, &format!("branch.{local}.merge"))?
            .and_then(|reference| reference.strip_prefix("refs/heads/").map(str::to_owned));
    }
    let worktree_output = run_git(workspace, &["worktree", "list", "--porcelain", "-z"])?;
    if !worktree_output.status.success() {
        return Err(git_error(&worktree_output));
    }
    let worktrees = parse_worktrees(&worktree_output.stdout, branches.worktree_root())?;
    git_branches::validate_snapshot(workspace, &branches)?;
    Ok(GitSnapshot {
        head: branches.head_commit().map(str::to_owned),
        branches,
        upstream: status.upstream,
        remotes,
        staged: status.staged,
        unstaged: status.unstaged,
        untracked: status.untracked,
        conflicts: status.conflicts,
        ahead: status.ahead,
        behind: status.behind,
        worktrees,
        index_tree,
        push_urls,
        tracking_remote,
        tracking_branch,
    })
}

fn optional_config(workspace: &Path, key: &str) -> Result<Option<String>, String> {
    let output = git_branches::run_git(workspace, &["config", "--get", key])?;
    if output.status.success() {
        Ok(Some(
            String::from_utf8(output.stdout)
                .map_err(|_| "Git devolvió configuración no representable")?
                .trim_end_matches(['\r', '\n'])
                .to_owned(),
        ))
    } else if output.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(git_branches::git_error(&output))
    }
}

fn parse_worktrees(bytes: &[u8], current: &Path) -> Result<Vec<WorktreeInfo>, String> {
    let records = std::str::from_utf8(bytes)
        .map_err(|_| "Una carpeta de worktree no puede representarse como texto")?;
    let mut entries = Vec::new();
    let mut entry: Option<WorktreeInfo> = None;
    for record in records.split('\0') {
        if let Some(path) = record.strip_prefix("worktree ") {
            if let Some(entry) = entry.take() {
                entries.push(entry);
            }
            let path = PathBuf::from(path);
            entry = Some(WorktreeInfo {
                current: path.canonicalize().ok().as_deref() == Some(current),
                path,
                branch: None,
            });
        } else if let Some(branch) = record.strip_prefix("branch refs/heads/") {
            if let Some(entry) = &mut entry {
                entry.branch = Some(branch.into());
            }
        } else if record == "bare" {
            entry = None;
        }
    }
    if let Some(entry) = entry {
        entries.push(entry);
    }
    Ok(entries)
}

fn validate_branch(workspace: &Path, name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 512 || name.starts_with('-') || name.contains('\0') {
        return Err("Escribí un nombre de rama válido".into());
    }
    let normalized = git_branches::git_text(workspace, &["check-ref-format", "--branch", name])?;
    if normalized != name {
        return Err("Usá un nombre de rama literal, sin abreviaturas de checkout".into());
    }
    Ok(())
}

fn checked_git(workspace: &Path, args: &[&str]) -> Result<(), String> {
    let output = git_branches::run_git(workspace, args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_branches::git_error(&output))
    }
}

fn create_branch(workspace: &Path, snapshot: &GitSnapshot, name: &str) -> Result<(), String> {
    validate_branch(workspace, name)?;
    git_branches::validate_snapshot(workspace, &snapshot.branches)?;
    let mut args = vec![
        "switch",
        "--no-guess",
        "--no-overwrite-ignore",
        "--no-recurse-submodules",
        "--create",
        name,
        "--",
    ];
    if let Some(head) = &snapshot.head {
        args.push(head);
    }
    checked_git(workspace, &args)
}

fn create_worktree(
    workspace: &Path,
    snapshot: &GitSnapshot,
    name: &str,
    path: &Path,
) -> Result<(), String> {
    validate_branch(workspace, name)?;
    git_branches::validate_snapshot(workspace, &snapshot.branches)?;
    let head = snapshot
        .head
        .as_deref()
        .ok_or("Creá el primer commit antes de crear un worktree")?;
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return Err("Elegí una ruta absoluta para el nuevo worktree".into());
    }
    // Git permits an empty existing directory, but the card asks for a new
    // checkout. Keep existing folders entirely outside this operation.
    if path.exists() || std::fs::symlink_metadata(path).is_ok() {
        return Err("La carpeta del worktree ya existe. Elegí una carpeta nueva.".into());
    }
    let path = path
        .to_str()
        .ok_or("La ruta del worktree no puede representarse como texto")?;
    checked_git(
        workspace,
        &["worktree", "add", "-b", name, "--", path, head],
    )
}

fn commit_staged(
    workspace: &Path,
    snapshot: &GitSnapshot,
    message: &str,
) -> Result<String, String> {
    if message.trim().is_empty() || message.len() > 64 * 1024 || message.contains('\0') {
        return Err("Escribí un mensaje de commit de hasta 64 KiB".into());
    }
    if snapshot.conflicts > 0 {
        return Err("Resolvé los conflictos antes de crear un commit".into());
    }
    if snapshot.staged == 0 {
        return Err("No hay cambios preparados para el commit".into());
    }
    git_branches::validate_snapshot(workspace, &snapshot.branches)?;
    let current_tree = git_branches::git_text(workspace, &["write-tree"])?;
    if snapshot.index_tree.as_deref() != Some(&current_tree) {
        return Err("Los cambios preparados cambiaron. Revisalos antes de crear el commit.".into());
    }
    checked_git(workspace, &["commit", "--message", message, "--"])?;
    git_branches::git_text(workspace, &["rev-parse", "--verify", "HEAD"])
}

fn push(
    workspace: &Path,
    snapshot: &GitSnapshot,
    remote: &str,
    branch: &str,
    set_upstream: bool,
) -> Result<(), String> {
    validate_branch(workspace, branch)?;
    let local = snapshot
        .branches
        .head_reference()
        .and_then(|reference| reference.strip_prefix("refs/heads/"))
        .ok_or("Creá o seleccioná una rama local antes del push")?;
    let head = snapshot
        .head
        .as_deref()
        .ok_or("Creá el primer commit antes del push")?;
    let expected_url = snapshot
        .push_urls
        .get(remote)
        .ok_or("Elegí un remoto del repositorio")?;
    git_branches::validate_snapshot(workspace, &snapshot.branches)?;
    if &git_branches::git_text(workspace, &["remote", "get-url", "--push", "--all", remote])?
        != expected_url
    {
        return Err(
            "La dirección del remoto cambió. Revisá el estado actualizado antes del push.".into(),
        );
    }
    // A pinned source prevents a concurrent checkout/commit from expanding
    // the push to code the user did not review. No force and no implicit refs.
    let refspec = format!("{head}:refs/heads/{branch}");
    let no_mirror = format!("remote.{remote}.mirror=false");
    checked_git(
        workspace,
        &[
            "-c",
            &no_mirror,
            "push",
            "--porcelain",
            "--no-follow-tags",
            "--",
            remote,
            &refspec,
        ],
    )?;
    if set_upstream {
        git_branches::validate_snapshot(workspace, &snapshot.branches).map_err(|error| {
            format!("El push se completó, pero no se configuró upstream: {error}")
        })?;
        let upstream = format!("{remote}/{branch}");
        checked_git(
            workspace,
            &["branch", "--set-upstream-to", &upstream, "--", local],
        )
        .map_err(|error| format!("El push se completó, pero no se configuró upstream: {error}"))?;
    }
    Ok(())
}

fn fetch(workspace: &Path, snapshot: &GitSnapshot, remote: &str) -> Result<(), String> {
    if !snapshot.remotes.iter().any(|known| known == remote) || remote.starts_with('-') {
        return Err("Elegí un remoto del repositorio".into());
    }
    checked_git(workspace, &["fetch", "--no-recurse-submodules", "--", remote])
}

/// Only a fast-forward: a diverged branch is left as it is, with Git's reason.
fn pull(workspace: &Path, snapshot: &GitSnapshot) -> Result<String, String> {
    let upstream = snapshot
        .upstream
        .clone()
        .ok_or("La rama no tiene una rama de seguimiento. Publicala primero.")?;
    if snapshot.conflicts > 0 {
        return Err("Resolvé los conflictos antes de recibir cambios".into());
    }
    git_branches::validate_snapshot(workspace, &snapshot.branches)?;
    checked_git(
        workspace,
        &["pull", "--ff-only", "--no-rebase", "--no-recurse-submodules"],
    )?;
    Ok(upstream)
}

fn compare(
    workspace: &Path,
    snapshot: &GitSnapshot,
    reference: &str,
) -> Result<BranchComparison, String> {
    let branch = snapshot
        .branches
        .branches
        .iter()
        .find(|branch| branch.reference == reference)
        .ok_or("Elegí una rama del listado para comparar")?;
    git_branches::validate_snapshot(workspace, &snapshot.branches)?;
    let head = snapshot
        .head
        .as_deref()
        .ok_or("Creá el primer commit antes de comparar ramas")?;
    let base = git_branches::git_text(
        workspace,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", branch.reference),
        ],
    )?;
    let range = format!("{base}...{head}");
    let counts = git_branches::git_text(
        workspace,
        &["rev-list", "--left-right", "--count", &range, "--"],
    )?;
    let mut counts = counts.split_whitespace();
    let behind = counts
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or("Git devolvió una comparación inválida")?;
    let ahead = counts
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or("Git devolvió una comparación inválida")?;
    let output = git_branches::run_git(
        workspace,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--numstat",
            "-z",
            &range,
            "--",
        ],
    )?;
    if !output.status.success() {
        return Err(git_branches::git_error(&output));
    }
    let mut comparison = BranchComparison {
        reference: reference.into(),
        ahead,
        behind,
        ..Default::default()
    };
    for record in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let mut fields = record.splitn(3, |byte| *byte == b'\t');
        let additions = fields.next().unwrap_or_default();
        let deletions = fields.next().unwrap_or_default();
        if fields.next().is_none() {
            return Err("Git devolvió un resumen de diff inválido".into());
        }
        comparison.files += 1;
        comparison.additions += std::str::from_utf8(additions)
            .ok()
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(0);
        comparison.deletions += std::str::from_utf8(deletions)
            .ok()
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(0);
    }
    Ok(comparison)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    struct Repository {
        directory: tempfile::TempDir,
        root: PathBuf,
    }

    impl Repository {
        fn new(commit: bool) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("repo");
            std::fs::create_dir(&root).unwrap();
            checked_git(&root, &["init", "--initial-branch=main"]).unwrap();
            configure_identity(&root);
            if commit {
                std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
                checked_git(&root, &["add", "--", "tracked.txt"]).unwrap();
                checked_git(&root, &["commit", "-m", "Initial"]).unwrap();
            }
            Self { directory, root }
        }

        fn git(&self, args: &[&str]) -> String {
            git_branches::git_text(&self.root, args).unwrap()
        }

        fn bare_remote(&self) -> PathBuf {
            let remote = self.directory.path().join("remote.git");
            checked_git(
                &self.root,
                &[
                    "init",
                    "--bare",
                    "--initial-branch=main",
                    remote.to_str().unwrap(),
                ],
            )
            .unwrap();
            checked_git(
                &self.root,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            )
            .unwrap();
            remote
        }
    }

    fn configure_identity(root: &Path) {
        for (key, value) in [
            ("user.name", "OpenIDE Test"),
            ("user.email", "openide-test@example.invalid"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            checked_git(root, &["config", key, value]).unwrap();
        }
    }

    /// A second clone commits `name` and pushes it to the bare remote.
    fn push_from_another_clone(repository: &Repository, remote: &Path, name: &str) {
        let clone = repository.directory.path().join(format!("clone-{name}"));
        checked_git(
            repository.directory.path(),
            &["clone", "-q", remote.to_str().unwrap(), clone.to_str().unwrap()],
        )
        .unwrap();
        configure_identity(&clone);
        std::fs::write(clone.join(name), "remote\n").unwrap();
        checked_git(&clone, &["add", "--", name]).unwrap();
        checked_git(&clone, &["commit", "-m", name]).unwrap();
        checked_git(&clone, &["push", "-q", "origin", "main"]).unwrap();
    }

    #[test]
    fn fetch_counts_what_is_behind_and_pull_only_fast_forwards() {
        let repository = Repository::new(true);
        let remote = repository.bare_remote();
        repository.git(&["push", "-q", "--set-upstream", "origin", "main"]);
        push_from_another_clone(&repository, &remote, "first.txt");
        let snapshot = load_snapshot(&repository.root).unwrap();
        assert_eq!(snapshot.behind, 0, "nothing is known before a fetch");
        assert!(fetch(&repository.root, &snapshot, "elsewhere").is_err());
        fetch(&repository.root, &snapshot, "origin").unwrap();
        assert!(!repository.root.join("first.txt").exists(), "a fetch moves nothing local");
        let snapshot = load_snapshot(&repository.root).unwrap();
        assert_eq!((snapshot.ahead, snapshot.behind), (0, 1));
        assert_eq!(pull(&repository.root, &snapshot).unwrap(), "origin/main");
        assert!(repository.root.join("first.txt").exists());
        let snapshot = load_snapshot(&repository.root).unwrap();
        assert_eq!((snapshot.ahead, snapshot.behind), (0, 0));

        // A diverged branch is refused and left where it was.
        push_from_another_clone(&repository, &remote, "second.txt");
        std::fs::write(repository.root.join("local.txt"), "local\n").unwrap();
        repository.git(&["add", "--", "local.txt"]);
        repository.git(&["commit", "-m", "Local"]);
        let head = repository.git(&["rev-parse", "HEAD"]);
        let snapshot = load_snapshot(&repository.root).unwrap();
        fetch(&repository.root, &snapshot, "origin").unwrap();
        let snapshot = load_snapshot(&repository.root).unwrap();
        assert_eq!((snapshot.ahead, snapshot.behind), (1, 1));
        assert!(pull(&repository.root, &snapshot).is_err());
        assert_eq!(repository.git(&["rev-parse", "HEAD"]), head);
        assert!(!repository.root.join("second.txt").exists());
    }

    #[test]
    fn pull_needs_an_upstream() {
        let repository = Repository::new(true);
        let snapshot = load_snapshot(&repository.root).unwrap();
        assert!(pull(&repository.root, &snapshot).unwrap_err().contains("seguimiento"));
    }

    #[test]
    fn status_handles_renames_conflicts_and_newlines_without_counting_old_paths() {
        let status = parse_status(b"# branch.upstream origin/main\0# branch.ab +2 -3\02 R. N... 100644 100644 100644 a b R100 new\nname\0old\nname\01 .M N... 100644 100644 100644 a b unstaged\0? untracked\0u UU N... 100644 100644 100644 100644 a b c conflict\0").unwrap();
        assert_eq!(
            (
                status.staged,
                status.unstaged,
                status.untracked,
                status.conflicts
            ),
            (1, 1, 1, 1)
        );
        assert_eq!((status.ahead, status.behind), (2, 3));
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
    }

    #[test]
    fn commit_includes_only_the_index_and_supports_unborn_repositories() {
        let repo = Repository::new(false);
        std::fs::write(repo.root.join("prepared.txt"), "prepared\n").unwrap();
        repo.git(&["add", "--", "prepared.txt"]);
        std::fs::write(repo.root.join("prepared.txt"), "prepared\nnot staged\n").unwrap();
        std::fs::write(repo.root.join("untracked.txt"), "not staged either\n").unwrap();
        let snapshot = load_snapshot(&repo.root).unwrap();
        assert_eq!(snapshot.head, None);
        assert_eq!(
            (snapshot.staged, snapshot.unstaged, snapshot.untracked),
            (1, 1, 1)
        );
        let id =
            commit_staged(&repo.root, &snapshot, "First commit\n\nUser supplied body").unwrap();
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), id);
        assert_eq!(repo.git(&["show", "HEAD:prepared.txt"]), "prepared");
        assert_eq!(
            repo.git(&["show", "--format=%B", "--no-patch", "HEAD"]),
            "First commit\n\nUser supplied body\n"
        );
        let after = load_snapshot(&repo.root).unwrap();
        assert_eq!((after.staged, after.unstaged, after.untracked), (0, 1, 1));
    }

    #[test]
    fn commit_rejects_changed_index_or_changed_head() {
        let repo = Repository::new(true);
        std::fs::write(repo.root.join("tracked.txt"), "first edit\n").unwrap();
        repo.git(&["add", "--", "tracked.txt"]);
        let snapshot = load_snapshot(&repo.root).unwrap();
        std::fs::write(repo.root.join("tracked.txt"), "second edit\n").unwrap();
        repo.git(&["add", "--", "tracked.txt"]);
        assert!(commit_staged(&repo.root, &snapshot, "Do not commit")
            .unwrap_err()
            .contains("preparados cambiaron"));
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), snapshot.head.unwrap());

        let snapshot = load_snapshot(&repo.root).unwrap();
        repo.git(&["switch", "-c", "external-branch"]);
        assert!(commit_staged(&repo.root, &snapshot, "Do not commit")
            .unwrap_err()
            .contains("rama activa cambió"));
        assert_eq!(repo.git(&["log", "-1", "--format=%s"]), "Initial");
    }

    #[test]
    fn create_and_switch_branches_never_overwrite_existing_branches() {
        let repo = Repository::new(true);
        let snapshot = load_snapshot(&repo.root).unwrap();
        create_branch(&repo.root, &snapshot, "feature/ui").unwrap();
        assert_eq!(repo.git(&["branch", "--show-current"]), "feature/ui");
        assert!(create_branch(&repo.root, &snapshot, "stale").is_err());
        let snapshot = load_snapshot(&repo.root).unwrap();
        assert!(create_branch(&repo.root, &snapshot, "main").is_err());
        assert!(create_branch(&repo.root, &snapshot, "--force").is_err());
        let main = snapshot
            .branches
            .branches
            .iter()
            .find(|branch| branch.name == "main")
            .unwrap();
        git_branches::switch_branch(&repo.root, &snapshot.branches, main).unwrap();
        assert_eq!(repo.git(&["branch", "--show-current"]), "main");
    }

    #[test]
    fn worktrees_are_listed_and_creation_preserves_current_checkout() {
        let repo = Repository::new(true);
        let snapshot = load_snapshot(&repo.root).unwrap();
        assert_eq!(snapshot.worktrees.len(), 1);
        assert!(snapshot.worktrees[0].current);
        let path = repo.directory.path().join("separate checkout");
        create_worktree(&repo.root, &snapshot, "parallel", &path).unwrap();
        assert_eq!(repo.git(&["branch", "--show-current"]), "main");
        let after = load_snapshot(&repo.root).unwrap();
        assert_eq!(after.worktrees.len(), 2);
        assert!(after.worktrees.iter().any(|worktree| worktree.path == path
            && worktree.branch.as_deref() == Some("parallel")
            && !worktree.current));
        assert!(create_worktree(&repo.root, &after, "other", &path).is_err());
        assert!(!after
            .branches
            .branches
            .iter()
            .any(|branch| branch.name == "other"));
    }

    #[test]
    fn branch_comparison_uses_merge_base_and_counts_only_committed_changes() {
        let repo = Repository::new(true);
        let snapshot = load_snapshot(&repo.root).unwrap();
        create_branch(&repo.root, &snapshot, "feature").unwrap();
        std::fs::write(repo.root.join("feature.txt"), "one\ntwo\n").unwrap();
        repo.git(&["add", "--", "feature.txt"]);
        repo.git(&["commit", "-m", "Feature"]);
        std::fs::write(repo.root.join("tracked.txt"), "uncommitted\n").unwrap();
        let snapshot = load_snapshot(&repo.root).unwrap();
        let comparison = compare(&repo.root, &snapshot, "refs/heads/main").unwrap();
        assert_eq!((comparison.ahead, comparison.behind), (1, 0));
        assert_eq!(
            (comparison.files, comparison.additions, comparison.deletions),
            (1, 2, 0)
        );
        assert!(compare(&repo.root, &snapshot, "--output=/tmp/invalid").is_err());
    }

    #[test]
    fn push_pins_head_sets_explicit_upstream_and_never_forces_a_divergent_remote() {
        let repo = Repository::new(true);
        let remote = repo.bare_remote();
        let snapshot = load_snapshot(&repo.root).unwrap();
        assert_eq!(snapshot.default_remote(), Some("origin"));
        push(&repo.root, &snapshot, "origin", "main", true).unwrap();
        let published = load_snapshot(&repo.root).unwrap();
        assert_eq!(published.upstream.as_deref(), Some("origin/main"));
        assert_eq!((published.ahead, published.behind), (0, 0));

        let other = repo.directory.path().join("other");
        checked_git(
            repo.directory.path(),
            &["clone", remote.to_str().unwrap(), other.to_str().unwrap()],
        )
        .unwrap();
        configure_identity(&other);
        std::fs::write(other.join("remote-only.txt"), "remote change\n").unwrap();
        checked_git(&other, &["add", "--", "remote-only.txt"]).unwrap();
        checked_git(&other, &["commit", "-m", "Remote change"]).unwrap();
        checked_git(&other, &["push", "origin", "main"]).unwrap();
        let remote_head = git_branches::git_text(&other, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(repo.root.join("local-only.txt"), "local change\n").unwrap();
        repo.git(&["add", "--", "local-only.txt"]);
        repo.git(&["commit", "-m", "Local change"]);
        let diverged = load_snapshot(&repo.root).unwrap();
        assert!(push(&repo.root, &diverged, "origin", "main", false).is_err());
        assert_eq!(
            git_branches::git_text(&remote, &["rev-parse", "refs/heads/main"]).unwrap(),
            remote_head
        );
    }

    #[test]
    fn push_rejects_stale_head_or_changed_remote_before_transmission() {
        let repo = Repository::new(true);
        let remote = repo.bare_remote();
        let snapshot = load_snapshot(&repo.root).unwrap();
        repo.git(&["switch", "-c", "different"]);
        assert!(push(&repo.root, &snapshot, "origin", "main", false)
            .unwrap_err()
            .contains("rama activa cambió"));
        repo.git(&["switch", "main"]);
        let other = repo.directory.path().join("other.git");
        repo.git(&[
            "remote",
            "set-url",
            "--push",
            "origin",
            other.to_str().unwrap(),
        ]);
        assert!(push(&repo.root, &snapshot, "origin", "main", false)
            .unwrap_err()
            .contains("dirección del remoto cambió"));
        assert!(
            !git_branches::run_git(&remote, &["rev-parse", "--verify", "refs/heads/main"])
                .unwrap()
                .status
                .success()
        );
    }

    #[test]
    fn worker_runs_local_actions_and_returns_refreshed_state() {
        let repo = Repository::new(true);
        let worker = Worker::spawn().unwrap();
        let snapshot = load_snapshot(&repo.root).unwrap();
        worker
            .submit(Request {
                workspace: repo.root.clone(),
                revision: 7,
                operation: Operation::CreateBranch {
                    snapshot,
                    name: "worker-branch".into(),
                },
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(response) = worker.poll_latest(7, &repo.root).unwrap() {
                assert!(response.outcome.unwrap().is_ok());
                assert_eq!(
                    response.snapshot.unwrap().current_branch(),
                    Some("worker-branch")
                );
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("Git worker did not respond");
    }

    #[test]
    fn github_compare_link_encodes_branch_slashes_and_rejects_lookalike_hosts() {
        let repo = Repository::new(true);
        let snapshot = load_snapshot(&repo.root).unwrap();
        create_branch(&repo.root, &snapshot, "feature/with-slash").unwrap();
        repo.git(&[
            "remote",
            "add",
            "origin",
            "git@github.com:owner/repository.git",
        ]);
        let snapshot = load_snapshot(&repo.root).unwrap();
        assert_eq!(
            snapshot.compare_url("refs/heads/main").as_deref(),
            Some("https://github.com/owner/repository/compare/main...feature%2Fwith-slash")
        );
        assert_eq!(snapshot.compare_url("refs/remotes/other/main"), None);
        repo.git(&[
            "remote",
            "set-url",
            "origin",
            "https://github.com.evil.invalid/owner/repository.git",
        ]);
        assert_eq!(
            load_snapshot(&repo.root)
                .unwrap()
                .compare_url("refs/heads/main"),
            None
        );
        repo.git(&[
            "remote",
            "set-url",
            "origin",
            "https://credential@github.com/owner/repository.git",
        ]);
        assert_eq!(
            load_snapshot(&repo.root)
                .unwrap()
                .compare_url("refs/heads/main"),
            None
        );
    }

    #[test]
    fn multiple_remotes_require_selection_unless_tracking_names_one_explicitly() {
        let repo = Repository::new(true);
        repo.git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/project.git",
        ]);
        repo.git(&[
            "remote",
            "add",
            "fork",
            "https://github.com/fork/project.git",
        ]);
        assert_eq!(load_snapshot(&repo.root).unwrap().default_remote(), None);
        repo.git(&["config", "branch.main.remote", "fork"]);
        repo.git(&["config", "branch.main.merge", "refs/heads/publish/main"]);
        let snapshot = load_snapshot(&repo.root).unwrap();
        assert_eq!(snapshot.default_remote(), Some("fork"));
        assert_eq!(snapshot.default_push_branch(), Some("publish/main"));
    }
}
