//! OpenIDE's Linux command containment. Bubblewrap only supplies OS primitives;
//! the filesystem policy, syscall filter, launch protocol and supervision are ours.
//!
//! The target cannot run until a trusted shell inside the completed mount/PID/
//! network namespaces and seccomp filter reports readiness and receives permission.
//! No host execution fallback, inherited credential environment, host /proc, host
//! /dev tree, or arbitrary read-only host root is admitted.

use crate::{AgentError, ToolContext, ToolOutput};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::unix::AsyncFd;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const READY: &[u8] = b"openide-linux-sandbox-v1\n";
const WRAPPER: &str = "printf 'openide-linux-sandbox-v1\\n' >&3 || exit 125; IFS= read -r permit <&4 || exit 125; exec 3>&- 4<&-; [ \"$permit\" = launch ] || exit 125; exec \"$@\"";
const MAX_WORKSPACE_ENTRIES: usize = 100_000;
const MAX_WORKSPACE_DEPTH: usize = 64;
// These are system runtime roots, not a general host read grant. /etc, /home,
// /root, host /tmp and host /run daemons are absent. Read-only system runtime
// may itself contain build-time configuration; this is not total confidentiality.
const RUNTIME_ROOTS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/nix/store",
    "/run/current-system/sw",
];

pub(crate) fn helper_available() -> bool {
    discover_helper(None).is_ok()
}

/// Prepared filesystem descriptors are held until spawn, preventing replacement
/// of the helper or workspace pathname from selecting another inode at exec/mount.
pub(crate) struct PreparedLaunch {
    command: Command,
    _helper: File,
    inherited: [OwnedFd; 5],
    ready: OwnedFd,
    permit: OwnedFd,
}

impl PreparedLaunch {
    pub(crate) fn command(&mut self) -> &mut Command {
        &mut self.command
    }

    /// Setup failures and cancellation before permission cannot execute the
    /// target. After permission, errors are conservatively effect-uncertain.
    pub(crate) async fn spawn(
        self,
        cancel: &CancellationToken,
    ) -> Result<(Child, LinuxSandboxGuard), AgentError> {
        let Self {
            mut command,
            _helper,
            inherited,
            ready,
            permit,
        } = self;
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let mut child = command
            .spawn()
            .map_err(|error| before_spawn(format!("cannot start Bubblewrap: {error}")))?;
        drop(inherited);
        drop(_helper);
        let guard = match LinuxSandboxGuard::new(child.id()) {
            Ok(guard) => guard,
            Err(error) => {
                let _ = child.kill().await;
                return Err(before_spawn(format!(
                    "cannot supervise sandbox before target launch: {error}"
                )));
            }
        };
        let ready = AsyncFd::new(ready)
            .map_err(|error| before_spawn(format!("cannot observe sandbox setup: {error}")))?;
        let setup = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(AgentError::Cancelled),
            result = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_ready(&ready)) => match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(before_spawn(format!("sandbox setup did not reach the target gate: {error}"))),
                Err(_) => Err(before_spawn("sandbox setup timed out before target launch".into())),
            }
        };
        if let Err(error) = setup {
            drop(permit);
            drop(guard);
            let _ = child.kill().await;
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            // Bounded setup stderr is diagnostic only; target was still gated.
            let detail = match child.stderr.take() {
                Some(stderr) => match tokio::time::timeout(
                    Duration::from_secs(1),
                    super::super::read_capped(stderr),
                )
                .await
                {
                    Ok(Ok((text, _))) => text,
                    _ => String::new(),
                },
                None => String::new(),
            };
            return Err(match error {
                AgentError::Tool(text) if !detail.is_empty() => {
                    AgentError::Tool(format!("{text}; Bubblewrap: {detail}"))
                }
                error => error,
            });
        }
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        // PIPE_BUF-sized write is atomic and cannot block: the wrapper is waiting.
        let bytes = b"launch\n";
        let wrote = unsafe { libc::write(permit.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
        if wrote != bytes.len() as isize {
            return Err(AgentError::EffectUncertain(format!(
                "sandbox launch permission delivery failed: {}",
                io::Error::last_os_error()
            )));
        }
        drop(permit);
        Ok((child, guard))
    }
}

pub(crate) struct LinuxSandboxGuard {
    supervisor: OwnedFd,
}

impl LinuxSandboxGuard {
    fn new(pid: Option<u32>) -> io::Result<Self> {
        let pid = pid.ok_or_else(|| io::Error::other("Bubblewrap PID is unavailable"))?;
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            supervisor: unsafe { OwnedFd::from_raw_fd(fd as RawFd) },
        })
    }
    /// Kill the supervisor by pidfd, avoiding PID reuse. Bubblewrap's parent-death
    /// linkage terminates PID 1; Linux then kills every descendant in that PID
    /// namespace, including descendants that called setsid().
    pub(crate) fn kill(&self) -> io::Result<()> {
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.supervisor.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}
impl Drop for LinuxSandboxGuard {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

#[cfg(test)]
pub(crate) fn prepare(
    program: &str,
    args: &[String],
    workspace: &Path,
) -> Result<PreparedLaunch, AgentError> {
    prepare_with_cancel(program, args, workspace, &CancellationToken::new())
}

pub(crate) fn prepare_with_cancel(
    program: &str,
    args: &[String],
    workspace: &Path,
    cancel: &CancellationToken,
) -> Result<PreparedLaunch, AgentError> {
    if cancel.is_cancelled() {
        return Err(AgentError::Cancelled);
    }
    validate_executable(program)?;
    let helper = discover_helper(Some(workspace))?;
    prepare_with_helper_and_cancel(program, args, workspace, &helper, cancel)
}

#[cfg(test)]
fn prepare_with_helper(
    program: &str,
    args: &[String],
    workspace: &Path,
    helper: &Path,
) -> Result<PreparedLaunch, AgentError> {
    prepare_with_helper_and_cancel(program, args, workspace, helper, &CancellationToken::new())
}

fn prepare_with_helper_and_cancel(
    program: &str,
    args: &[String],
    workspace: &Path,
    helper: &Path,
    cancel: &CancellationToken,
) -> Result<PreparedLaunch, AgentError> {
    validate_executable(program)?;
    let canonical = workspace
        .canonicalize()
        .map_err(|error| before_spawn(format!("cannot resolve workspace: {error}")))?;
    if canonical != workspace
        || !canonical.is_dir()
        || canonical.parent().is_none()
        || RUNTIME_ROOTS
            .iter()
            .any(|root| canonical.starts_with(root) || Path::new(root).starts_with(&canonical))
        || ["/proc", "/dev", "/run", "/etc", "/sys"]
            .iter()
            .any(|root| canonical.starts_with(root) || Path::new(root).starts_with(&canonical))
    {
        return Err(before_spawn("workspace must be a canonical non-root directory outside reserved system runtime paths".into()));
    }
    validate_workspace_links(&canonical, cancel)?;
    validate_nested_mounts(&canonical)?;
    let helper_file = open_helper(helper, Some(&canonical))?;
    let workspace_file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&canonical)
        .map_err(|error| before_spawn(format!("cannot pin workspace directory: {error}")))?;
    let filter = seccomp_file()?;
    let (ready_read, ready_write) = pipe()?;
    let (permit_read, permit_write) = pipe()?;
    let inherited = [
        duplicate(&ready_write)?,
        duplicate(&permit_read)?,
        duplicate(&filter)?,
        duplicate(&workspace_file)?,
        duplicate(&helper_file)?,
    ];
    let sources = inherited.each_ref().map(AsRawFd::as_raw_fd);
    set_nonblocking(&ready_read)?;
    let mut command = Command::new("/proc/self/fd/7");
    command.args([
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
    ]);
    for root in RUNTIME_ROOTS {
        if Path::new(root).exists() {
            command.arg("--ro-bind").arg(root).arg(root);
        }
    }
    command.args([
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/tmp/openide-home",
    ]);
    command
        .arg("--bind-fd")
        .arg("6")
        .arg(&canonical)
        .args(["--remount-ro", "/"])
        .arg("--chdir")
        .arg(&canonical);
    command
        .args([
            "--seccomp",
            "5",
            "--",
            "/bin/sh",
            "-c",
            WRAPPER,
            "openide-sandbox-gate",
        ])
        .arg(program)
        .args(args);
    command.env_clear();
    // Trusted bootstrap only. Host home/config/tool-cache directories are not
    // implicitly granted merely because an environment variable names them.
    for key in super::super::HOST_BOOTSTRAP_ENV_KEYS {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("HOME", "/tmp/openide-home")
        .env("TMPDIR", "/tmp")
        .env("TMP", "/tmp")
        .env("TEMP", "/tmp");
    command
        .current_dir("/")
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Every non-stdio inherited descriptor is made CLOEXEC. Only this launch's
    // four descriptors are deliberately passed to bwrap; it consumes 5/6 and
    // our trusted wrapper closes 3/4 before executing model-controlled argv.
    unsafe {
        command.pre_exec(move || {
            if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 4u32) != 0 {
                return Err(io::Error::last_os_error());
            }
            for (source, target) in sources.iter().zip(3..=6) {
                if libc::dup3(*source, target, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            // ELF execution resolves this fd before CLOEXEC closes it. This
            // fixed slot also avoids source/helper collisions with slots 3..6.
            if libc::dup3(sources[4], 7, libc::O_CLOEXEC) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(PreparedLaunch {
        command,
        _helper: helper_file,
        inherited,
        ready: ready_read,
        permit: permit_write,
    })
}

fn validate_executable(program: &str) -> Result<(), AgentError> {
    // POSIX sh's exec does not portably support '--' (dash treats it as an
    // executable). Reject ambiguous leading options before setup instead of
    // interpreting a model's executable as a trusted wrapper builtin option.
    if program.starts_with('-') {
        return Err(before_spawn(
            "command executable starts with '-'; use an explicit literal path such as './-name'"
                .into(),
        ));
    }
    Ok(())
}

fn discover_helper(workspace: Option<&Path>) -> Result<PathBuf, AgentError> {
    for root in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        if !root.is_absolute() || workspace.is_some_and(|workspace| root.starts_with(workspace)) {
            continue;
        }
        let candidate = root.join("bwrap");
        let Ok(canonical) = candidate.canonicalize() else {
            continue;
        };
        if open_helper(&canonical, workspace).is_ok() {
            return Ok(canonical);
        }
    }
    Err(before_spawn("Bubblewrap is not installed at a trusted absolute executable PATH location; working unprivileged user namespaces, seccomp, pidfd and close_range are also required".into()))
}

fn open_helper(path: &Path, workspace: Option<&Path>) -> Result<File, AgentError> {
    if !path.is_absolute() || workspace.is_some_and(|workspace| path.starts_with(workspace)) {
        return Err(before_spawn(
            "Bubblewrap helper may not come from the writable workspace".into(),
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| before_spawn(format!("cannot open trusted Bubblewrap helper: {error}")))?;
    let metadata = file
        .metadata()
        .map_err(|error| before_spawn(error.to_string()))?;
    let mut elf = [0; 4];
    if !metadata.is_file()
        || metadata.mode() & 0o111 == 0
        || metadata.mode() & 0o6022 != 0
        || file.read_exact(&mut elf).is_err()
        || elf != *b"\x7fELF"
    {
        return Err(before_spawn("Bubblewrap helper must be a regular, executable, non-setuid/non-setgid ELF file, not group/world writable".into()));
    }
    Ok(file)
}

fn validate_workspace_links(
    workspace: &Path,
    cancel: &CancellationToken,
) -> Result<(), AgentError> {
    let mut stack = vec![(workspace.to_path_buf(), 0)];
    let mut count = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if depth > MAX_WORKSPACE_DEPTH {
            return Err(before_spawn(format!(
                "workspace link validation exceeded depth {MAX_WORKSPACE_DEPTH} at {}",
                directory.display()
            )));
        }
        for entry in std::fs::read_dir(&directory).map_err(|error| {
            before_spawn(format!(
                "cannot validate workspace {}: {error}",
                directory.display()
            ))
        })? {
            let entry = entry.map_err(|error| before_spawn(error.to_string()))?;
            count += 1;
            if count & 63 == 0 && cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            if count > MAX_WORKSPACE_ENTRIES {
                return Err(before_spawn(format!(
                    "workspace link validation exceeded {MAX_WORKSPACE_ENTRIES} entries at {}",
                    entry.path().display()
                )));
            }
            let metadata = entry
                .path()
                .symlink_metadata()
                .map_err(|error| before_spawn(error.to_string()))?;
            if metadata.is_file() && metadata.nlink() > 1 {
                return Err(before_spawn(format!("workspace contains a hard-linked file; external inode aliases cannot be safely excluded: {}", entry.path().display())));
            }
            if metadata.is_dir() {
                stack.push((entry.path(), depth + 1));
            }
        }
    }
    Ok(())
}

fn validate_nested_mounts(workspace: &Path) -> Result<(), AgentError> {
    let file = File::open("/proc/self/mountinfo")
        .map_err(|error| before_spawn(format!("cannot validate workspace mounts: {error}")))?;
    let mut mountinfo = String::new();
    file.take(4 * 1024 * 1024 + 1)
        .read_to_string(&mut mountinfo)
        .map_err(|error| before_spawn(error.to_string()))?;
    if mountinfo.len() > 4 * 1024 * 1024 {
        return Err(before_spawn(
            "workspace mount validation exceeded 4 MiB mountinfo limit".into(),
        ));
    }
    for line in mountinfo.lines() {
        let Some(path) = line.split_whitespace().nth(4) else {
            return Err(before_spawn("invalid kernel mountinfo entry".into()));
        };
        let decoded = path
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\");
        let path = Path::new(&decoded);
        if path != workspace && path.starts_with(workspace) {
            return Err(before_spawn(format!(
                "workspace contains a nested mount with possible external aliases: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn before_spawn(message: String) -> AgentError {
    AgentError::Tool(format!(
        "Linux workspace-write/offline isolation: {message}; target command was not started"
    ))
}

fn duplicate(fd: &impl AsRawFd) -> Result<OwnedFd, AgentError> {
    let raw = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    if raw < 0 {
        Err(before_spawn(io::Error::last_os_error().to_string()))
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }
}
fn pipe() -> Result<(OwnedFd, OwnedFd), AgentError> {
    let mut fds = [-1; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(before_spawn(io::Error::last_os_error().to_string()));
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}
fn set_nonblocking(fd: &OwnedFd) -> Result<(), AgentError> {
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return Err(before_spawn(io::Error::last_os_error().to_string()));
    }
    Ok(())
}
async fn read_ready(fd: &AsyncFd<OwnedFd>) -> io::Result<()> {
    let mut bytes = Vec::new();
    while bytes.len() < READY.len() {
        let mut readiness = fd.readable().await?;
        let mut chunk = [0u8; 64];
        match readiness.try_io(|fd| {
            let count = unsafe {
                libc::read(
                    fd.get_ref().as_raw_fd(),
                    chunk.as_mut_ptr().cast(),
                    chunk.len(),
                )
            };
            if count < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(count as usize)
            }
        }) {
            Ok(Ok(0)) => {
                return Err(io::Error::other(
                    "ready pipe closed before setup confirmation",
                ))
            }
            Ok(Ok(count)) => bytes.extend_from_slice(&chunk[..count]),
            Ok(Err(error)) => return Err(error),
            Err(_) => continue,
        }
    }
    if bytes == READY {
        Ok(())
    } else {
        Err(io::Error::other("invalid sandbox readiness receipt"))
    }
}

fn seccomp_file() -> Result<File, AgentError> {
    let name = c"openide-linux-command-seccomp-v1";
    let raw =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if raw < 0 {
        return Err(before_spawn(io::Error::last_os_error().to_string()));
    }
    let mut file = unsafe { File::from_raw_fd(raw) };
    let filter = seccomp_filter();
    for instruction in filter {
        file.write_all(&instruction.code.to_ne_bytes())
            .and_then(|()| file.write_all(&[instruction.jt, instruction.jf]))
            .and_then(|()| file.write_all(&instruction.k.to_ne_bytes()))
            .map_err(|error| before_spawn(error.to_string()))?;
    }
    if unsafe { libc::lseek(raw, 0, libc::SEEK_SET) } < 0
        || unsafe {
            libc::fcntl(
                raw,
                libc::F_ADD_SEALS,
                libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE,
            )
        } < 0
    {
        return Err(before_spawn(io::Error::last_os_error().to_string()));
    }
    Ok(file)
}

fn seccomp_filter() -> Vec<libc::sock_filter> {
    fn ins(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
        libc::sock_filter { code, jt, jf, k }
    }
    let architecture = if cfg!(target_arch = "x86_64") {
        0xc000003e
    } else {
        0xc00000b7
    };
    let mut filter = vec![
        ins(0x20, 0, 0, 4),
        ins(0x15, 1, 0, architecture),
        ins(0x06, 0, 0, libc::SECCOMP_RET_KILL_PROCESS),
        ins(0x20, 0, 0, 0),
    ];
    #[cfg(target_arch = "x86_64")]
    filter.extend([
        ins(0x45, 0, 1, 0x40000000),
        ins(0x06, 0, 0, libc::SECCOMP_RET_KILL_PROCESS),
    ]);
    // Deny creation/use of every socket family, including pathname/abstract
    // UNIX and VSOCK, and io_uring networking. No ambient sockets reach target.
    for syscall in [
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_sendto,
        libc::SYS_sendmsg,
        libc::SYS_sendmmsg,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_pidfd_getfd,
        libc::SYS_kcmp,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_open_by_handle_at,
        libc::SYS_bpf,
        libc::SYS_userfaultfd,
        libc::SYS_perf_event_open,
    ] {
        filter.extend([
            ins(0x15, 0, 1, syscall as u32),
            ins(0x06, 0, 0, libc::SECCOMP_RET_ERRNO | libc::EPERM as u32),
        ]);
    }
    // clone3's pointed arguments cannot be inspected by classic BPF. ENOSYS
    // lets libc fall back to clone, whose namespace flags are checked below.
    filter.extend([
        ins(0x15, 0, 1, libc::SYS_clone3 as u32),
        ins(0x06, 0, 0, libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32),
    ]);
    let namespaces = (libc::CLONE_NEWUSER
        | libc::CLONE_NEWNS
        | libc::CLONE_NEWPID
        | libc::CLONE_NEWNET
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWUTS
        | libc::CLONE_NEWCGROUP) as u32;
    filter.extend([
        ins(0x15, 0, 3, libc::SYS_clone as u32),
        ins(0x20, 0, 0, 16),
        ins(0x45, 0, 1, namespaces),
        ins(0x06, 0, 0, libc::SECCOMP_RET_ERRNO | libc::EPERM as u32),
        ins(0x06, 0, 0, libc::SECCOMP_RET_ALLOW),
    ]);
    filter
}

#[cfg(test)]
pub(crate) async fn run(
    program: &str,
    args: &[&str],
    timeout_ms: u64,
    context: &ToolContext,
) -> Result<ToolOutput, AgentError> {
    run_streaming(program, args, timeout_ms, context, None).await
}

pub(crate) async fn run_streaming(
    program: &str,
    args: &[&str],
    timeout_ms: u64,
    context: &ToolContext,
    output: Option<crate::ToolOutputSender>,
) -> Result<ToolOutput, AgentError> {
    let owned = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let program = program.to_owned();
    let workspace = context.workspace.clone();
    let cancel = context.cancel.clone();
    // Filesystem validation can involve slow mounts. Keep it off the async
    // executor; cancellation is checked between bounded entries, not a claim
    // that blocking kernel filesystem IO itself has a wall-clock bound.
    let launch = tokio::task::spawn_blocking(move || {
        prepare_with_cancel(&program, &owned, &workspace, &cancel)
    })
    .await
    .map_err(|error| before_spawn(format!("sandbox preparation worker failed: {error}")))??;
    let (mut child, guard) = launch.spawn(&context.cancel).await?;
    let stdout = child.stdout.take().ok_or_else(|| {
        AgentError::EffectUncertain(
            "sandbox command stdout pipe is unavailable after target launch".into(),
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        AgentError::EffectUncertain(
            "sandbox command stderr pipe is unavailable after target launch".into(),
        )
    })?;
    let stdout_task = tokio::spawn(super::super::read_capped_streaming(
        stdout,
        output.clone(),
        openide_core::OutputStream::Stdout,
    ));
    let stderr_task = tokio::spawn(super::super::read_capped_streaming(
        stderr,
        output,
        openide_core::OutputStream::Stderr,
    ));
    let result = tokio::select! {
        _ = context.cancel.cancelled() => Err(AgentError::Cancelled),
        result = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()) => match result {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(error)) => Err(AgentError::EffectUncertain(format!("sandbox command wait failed after launch: {error}"))),
            Err(_) => Err(AgentError::EffectUncertain(format!("sandbox command timed out after {timeout_ms} ms; it may have changed files before termination"))),
        }
    };
    drop(guard);
    let status = match result {
        Ok(status) => status,
        Err(error) => {
            stdout_task.abort();
            stderr_task.abort();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            return Err(error);
        }
    };
    let (stdout, stdout_truncated) = stdout_task
        .await
        .map_err(|error| AgentError::EffectUncertain(error.to_string()))?
        .map_err(|error| AgentError::EffectUncertain(error.to_string()))?;
    let (stderr, stderr_truncated) = stderr_task
        .await
        .map_err(|error| AgentError::EffectUncertain(error.to_string()))?
        .map_err(|error| AgentError::EffectUncertain(error.to_string()))?;
    let mut content = format!(
        "exit code: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        status
            .code()
            .map_or_else(|| "unknown".into(), |code| code.to_string())
    );
    if stdout_truncated || stderr_truncated {
        content.push_str("\n[command output truncated]");
    }
    Ok(ToolOutput {
        content,
        is_error: !status.success(),
    })
}

#[cfg(test)]
#[path = "command_sandbox_linux_tests.rs"]
mod tests;
