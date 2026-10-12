//! Starts one private headless Chrome that speaks DevTools over a pipe.
//!
//! The browser gets a fresh profile inside a directory only this user can
//! read, a reduced environment and its own process group. Dropping `Chrome`
//! kills the group, reaps the browser and removes the directory. If this
//! process dies instead, Chrome sees its command pipe close and exits, and the
//! directory is collected by the next launch.

use crate::transport::Transport;
use crate::{DriverError, ErrorKind, LaunchOptions};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// Chromium creates Unix sockets below its temporary root (about 45 more
/// bytes), and `sockaddr_un` holds 108. A deep data directory is a valid path
/// and still too long, so the whole owned root stays within this length.
const MAX_DIRECTORY_BYTES: usize = 48;
const DIRECTORY_PREFIX: &str = "oid-";
const LOCK_NAME: &str = "driver.lock";
/// A directory younger than this may belong to a launch that has not locked
/// it yet, so the sweep leaves it alone.
const SWEEP_MIN_AGE: Duration = Duration::from_secs(60);
const STDERR_TAIL: usize = 2048;

/// Everything Chrome needs to behave like the single, quiet page of a panel.
/// The sandbox is never disabled.
const SWITCHES: &[&str] = &[
    "--remote-debugging-pipe",
    "--headless",
    "--no-first-run",
    "--no-default-browser-check",
    "--no-startup-window",
    "--no-service-autorun",
    "--enable-automation",
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-default-apps",
    "--disable-extensions",
    // Chrome's own background extensions (Hangouts, the network speech
    // service) would each cost a process.
    "--disable-component-extensions-with-background-pages",
    "--disable-client-side-phishing-detection",
    "--disable-search-engine-choice-screen",
    "--disable-sync",
    "--disable-breakpad",
    "--disable-hang-monitor",
    "--disable-popup-blocking",
    "--disable-prompt-on-repost",
    "--metrics-recording-only",
    "--mute-audio",
    "--hide-scrollbars",
    "--password-store=basic",
    "--use-mock-keychain",
    "--force-color-profile=srgb",
    // A desktop pointer, as a page's hover media queries would see on the user's screen.
    "--blink-settings=primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4",
    // A page nobody is looking at still has to run for the agent.
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding",
    // History navigation always loads a document, so its events are uniform.
    "--disable-back-forward-cache",
    "--disable-features=Translate,MediaRouter,OptimizationHints,AcceptCHFrame,DialMediaRouteProvider",
    // Without it a viewport capture waits three frames instead of one.
    "--enable-features=CDPScreenshotNewSurface",
];

/// What the browser inherits. Provider keys, startup hooks and broker tokens
/// of the host never reach Chromium or the pages it runs.
const ENVIRONMENT: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "XDG_RUNTIME_DIR",
];

pub(crate) struct Chrome {
    child: Child,
    directory: PrivateDirectory,
    stderr: Arc<Mutex<Option<Vec<u8>>>>,
}

impl Chrome {
    pub(crate) fn launch(options: &LaunchOptions) -> Result<(Self, Transport), DriverError> {
        let failed = |error: io::Error| DriverError::new(ErrorKind::Launch, error.to_string());
        let directory = PrivateDirectory::create(options.temp_root.as_deref()).map_err(|_| {
            DriverError::new(
                ErrorKind::Launch,
                "No se pudo crear el directorio temporal del navegador. Cerrá esta pestaña y volvé a abrirla.",
            )
        })?;
        let (browser_reads, commands) = io::pipe().map_err(failed)?;
        let (replies, browser_writes) = io::pipe().map_err(failed)?;
        // Above the two descriptors the browser expects, so neither `dup2`
        // below can overwrite the other end or be a no-op that keeps CLOEXEC.
        let browser_reads = above_standard(browser_reads.into()).map_err(failed)?;
        let browser_writes = above_standard(browser_writes.into()).map_err(failed)?;
        let (read_fd, write_fd) = (browser_reads.as_raw_fd(), browser_writes.as_raw_fd());

        let mut command = Command::new(&options.executable);
        command
            .args(SWITCHES)
            .arg(format!("--user-data-dir={}", directory.path.join("profile").display()))
            .arg(format!("--force-device-scale-factor={}", options.scale))
            .arg(format!("--window-size={},{}", options.width, options.height))
            .current_dir(&directory.path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .env_clear()
            .process_group(0);
        for name in ENVIRONMENT {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        // The host may outlive the shell that started it: never hand its
        // temporary directory, which can already be gone, to Chromium.
        for name in ["TMPDIR", "TMP", "TEMP"] {
            command.env(name, &directory.path);
        }
        // Only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(read_fd, 3) < 0 || libc::dup2(write_fd, 4) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|error| {
            DriverError::new(
                ErrorKind::Launch,
                format!(
                    "No se pudo iniciar el navegador {}: {error}",
                    options.executable.display()
                ),
            )
        })?;
        // The browser holds its own copies; ours would hide its exit.
        drop((browser_reads, browser_writes));

        let stderr = Arc::new(Mutex::new(Some(Vec::new())));
        if let Some(mut stream) = child.stderr.take() {
            let tail = stderr.clone();
            let _ = std::thread::Builder::new()
                .name("browser-stderr".into())
                .spawn(move || {
                    let mut scratch = [0u8; 4096];
                    while let Ok(count @ 1..) = stream.read(&mut scratch) {
                        let mut tail = tail.lock().unwrap_or_else(|poison| poison.into_inner());
                        // Kept only while starting; later output may quote page data.
                        if let Some(tail) = tail.as_mut() {
                            tail.extend_from_slice(&scratch[..count]);
                            let extra = tail.len().saturating_sub(STDERR_TAIL);
                            tail.drain(..extra);
                        }
                    }
                });
        }
        Ok((
            Self {
                child,
                directory,
                stderr,
            },
            Transport::nul_delimited(
                File::from(OwnedFd::from(commands)),
                File::from(OwnedFd::from(replies)),
            ),
        ))
    }

    /// What Chrome printed while starting, for a launch that failed. Stops
    /// the retention either way.
    pub(crate) fn startup_output(&self) -> String {
        let tail = self
            .stderr
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        String::from_utf8_lossy(&tail.unwrap_or_default())
            .trim()
            .to_owned()
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory.path
    }

    pub(crate) fn process_id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Chrome {
    fn drop(&mut self) {
        // Not reaped yet, so the id still names this browser's group: the
        // zygotes, renderers and GPU process go with it.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        // The directory goes next, with the field that owns it.
    }
}

impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        // A dying renderer may still be writing; give the group a moment.
        for _ in 0..50 {
            match fs::remove_dir_all(&self.path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => break,
            }
        }
    }
}

fn above_standard(descriptor: OwnedFd) -> io::Result<OwnedFd> {
    let duplicate = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

/// A directory only this user can read, short enough for the sockets
/// Chromium creates below it. Dropping it removes it; one whose owner died is
/// collected by the next `create` under the same root.
pub struct PrivateDirectory {
    path: PathBuf,
    /// Held locked for as long as the directory is in use; the kernel releases
    /// it if this process dies, which is how a later sweep recognises a leak.
    #[allow(dead_code)]
    lock: File,
}

impl PrivateDirectory {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Under `preferred` when that is short enough and exists, else under the
    /// inherited temporary directory, else under `/tmp`.
    pub fn create(preferred: Option<&Path>) -> io::Result<Self> {
        let inherited = ["TMPDIR", "TMP", "TEMP"]
            .into_iter()
            .filter_map(|name| std::env::var_os(name).map(PathBuf::from))
            .chain([std::env::temp_dir()]);
        let candidates: Vec<PathBuf> = match preferred {
            Some(path) => vec![path.to_owned()],
            None => inherited.collect(),
        };
        for base in candidates.iter().map(PathBuf::as_path).chain([Path::new("/tmp")]) {
            let length = base.as_os_str().len() + 1 + DIRECTORY_PREFIX.len() + 6;
            // A base that vanished (a closed nix-shell) is not recreated.
            if !base.is_absolute() || length > MAX_DIRECTORY_BYTES || !base.is_dir() {
                continue;
            }
            sweep(base);
            for _ in 0..16 {
                let path = base.join(format!("{DIRECTORY_PREFIX}{}", random_suffix()));
                match fs::DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => match lock_file(&path.join(LOCK_NAME), true) {
                        Ok(lock) => return Ok(Self { path, lock }),
                        Err(_) => {
                            let _ = fs::remove_dir_all(&path);
                            break;
                        }
                    },
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    // Try another existing root; the sandbox is never the price.
                    Err(_) => break,
                }
            }
        }
        Err(io::ErrorKind::NotFound.into())
    }
}

fn lock_file(path: &Path, create: bool) -> io::Result<File> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

/// Removes directories left by drivers of this user that died without
/// cleaning up. Only a directory carrying this crate's lock file, old enough
/// and no longer locked by a living process, is touched.
fn sweep(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    let owner = unsafe { libc::geteuid() };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(DIRECTORY_PREFIX) || name.len() != DIRECTORY_PREFIX.len() + 6 {
            continue;
        }
        let path = entry.path();
        let ours = fs::symlink_metadata(&path)
            .is_ok_and(|metadata| metadata.is_dir() && metadata.uid() == owner);
        let lock = path.join(LOCK_NAME);
        let old = fs::symlink_metadata(&lock)
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| {
                SystemTime::now()
                    .duration_since(modified)
                    .is_ok_and(|age| age >= SWEEP_MIN_AGE)
            });
        if ours && old && lock_file(&lock, false).is_ok() {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

/// Six unpredictable characters without a randomness dependency: the standard
/// hasher is keyed from the operating system for every `RandomState`.
fn random_suffix() -> String {
    use std::hash::{BuildHasher, Hasher};
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut bits = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    (0..6)
        .map(|_| {
            let character = ALPHABET[(bits % ALPHABET.len() as u64) as usize] as char;
            bits /= ALPHABET.len() as u64;
            character
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(label: &str) -> PathBuf {
        // Directly under /tmp: a nix-shell TMPDIR would already use up the length budget.
        let path = Path::new("/tmp").join(format!("obt-{label}-{}", random_suffix()));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn the_directory_is_private_short_and_inside_the_requested_root() {
        let base = scratch("priv");
        let directory = PrivateDirectory::create(Some(&base)).unwrap();
        assert_eq!(directory.path.parent(), Some(base.as_path()));
        assert!(directory.path.as_os_str().len() <= MAX_DIRECTORY_BYTES);
        let mode = fs::metadata(&directory.path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(directory.path.join(LOCK_NAME).is_file());
        let path = directory.path.clone();
        drop(directory);
        assert!(!path.exists(), "dropping it removes it");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_deep_or_missing_root_falls_back_to_a_short_one() {
        let base = scratch("deep");
        let deep = base.join("browser-runtime").join("scope-".repeat(12)).join("tmp");
        fs::create_dir_all(&deep).unwrap();
        for unusable in [deep.clone(), base.join("deleted-nix-shell")] {
            let directory = PrivateDirectory::create(Some(&unusable)).unwrap();
            assert!(directory.path.starts_with("/tmp"), "{:?}", directory.path);
            assert!(directory.path.as_os_str().len() <= MAX_DIRECTORY_BYTES);
        }
        assert_eq!(fs::read_dir(&deep).unwrap().count(), 0, "nothing lands in the long path");
        assert!(!base.join("deleted-nix-shell").exists(), "a deleted root is not recreated");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn the_sweep_removes_only_abandoned_directories_of_this_crate() {
        let base = scratch("sweep");
        let aged = |path: &Path| {
            let old = SystemTime::now() - SWEEP_MIN_AGE - Duration::from_secs(5);
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        };
        // Abandoned: lock file present, old, and nobody holds it.
        let abandoned = base.join("oid-aaaaaa");
        fs::create_dir(&abandoned).unwrap();
        fs::write(abandoned.join(LOCK_NAME), b"").unwrap();
        fs::create_dir(abandoned.join("profile")).unwrap();
        aged(&abandoned.join(LOCK_NAME));
        // In use: old, but a living driver holds the lock.
        let live = base.join("oid-bbbbbb");
        fs::create_dir(&live).unwrap();
        let held = lock_file(&live.join(LOCK_NAME), true).unwrap();
        aged(&live.join(LOCK_NAME));
        // Just created: unlocked for an instant, but too young to judge.
        let young = base.join("oid-cccccc");
        fs::create_dir(&young).unwrap();
        fs::write(young.join(LOCK_NAME), b"").unwrap();
        // Someone else's: the Node bridge uses the same prefix without a lock.
        let foreign = base.join("oid-dddddd");
        fs::create_dir(&foreign).unwrap();
        let unrelated = base.join("oid-not-ours");
        fs::create_dir(&unrelated).unwrap();
        fs::write(unrelated.join(LOCK_NAME), b"").unwrap();
        aged(&unrelated.join(LOCK_NAME));

        sweep(&base);
        assert!(!abandoned.exists());
        for kept in [&live, &young, &foreign, &unrelated] {
            assert!(kept.exists(), "{kept:?}");
        }
        drop(held);
        sweep(&base);
        assert!(!live.exists(), "released by its owner, it is collected too");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn suffixes_are_six_characters_and_differ() {
        let first = random_suffix();
        assert_eq!(first.len(), 6);
        assert!(first.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        assert!((0..8).any(|_| random_suffix() != first));
    }
}
