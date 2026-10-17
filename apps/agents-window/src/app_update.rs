//! openADE's own updates: the newest GitHub release is found, downloaded and
//! verified in the background, and put in place of this copy when it quits.
//!
//! Only an archive from a release replaces itself (Linux x86_64, the folder
//! `install.sh` makes). Any other copy, built from source or installed by a
//! package manager, is told a newer version exists and where to get it.
//!
//! The swap is done by the new version itself, once this process is gone:
//! `<staged>/openade --apply-update <pid> <install root> [--relaunch]`. That
//! command line is a contract between releases and does not change.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use serde::Deserialize;

/// The release workflow signs each archive with the matching secret key
/// (`rsign sign -W`), with the archive's file name as the trusted comment.
pub const PUBLIC_KEY: &str = "RWTBGPfzssEDLkqVR1Ybhr7FWSSMrF1JXh3xu47NTMhaEfoxUBh04tCD";
const LATEST_RELEASE: &str = "https://api.github.com/repos/Niiihuel/OpenADE/releases/latest";
const DOWNLOADS: &str = "https://github.com/Niiihuel/OpenADE/releases/download/";
pub const APPLY_FLAG: &str = "--apply-update";
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// The first check waits for the window to settle, as VS Code does.
pub const FIRST_CHECK: Duration = Duration::from_secs(30);
const MAX_RELEASE_JSON: u64 = 2 * 1024 * 1024;
const MAX_SIGNATURE: u64 = 4 * 1024;
const MAX_ARCHIVE: u64 = 1024 * 1024 * 1024;
const LAST_ERROR: &str = "last-error";
const LOCK: &str = ".lock";

pub fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("the package version is semver")
}

/// The archive a release attaches for this platform.
pub fn archive_name(version: &semver::Version) -> String {
    format!("openADE-{version}-linux-x86_64.tar.gz")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: semver::Version,
    /// The release's page, with its notes.
    pub page: String,
    /// The archive and its signature, when the release has both for this platform.
    pub archive: Option<(String, String)>,
}

#[derive(Deserialize)]
struct ReleaseJson {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<AssetJson>,
}

#[derive(Deserialize)]
struct AssetJson {
    name: String,
    browser_download_url: String,
}

/// The release GitHub calls latest, when it is newer than `current`.
pub fn newer_release(json: &[u8], current: &semver::Version) -> Result<Option<Release>, String> {
    let release: ReleaseJson =
        serde_json::from_slice(json).map_err(|error| format!("Respuesta de GitHub inválida: {error}"))?;
    let version = semver::Version::parse(release.tag_name.trim_start_matches('v'))
        .map_err(|_| format!("La versión «{}» no es válida", release.tag_name))?;
    if release.draft || release.prerelease || !version.pre.is_empty() || version <= *current {
        return Ok(None);
    }
    let name = archive_name(&version);
    let url = |wanted: &str| {
        release
            .assets
            .iter()
            .find(|asset| asset.name == wanted)
            .map(|asset| asset.browser_download_url.clone())
            // Only this repository's downloads: a URL elsewhere is not followed.
            .filter(|url| url.starts_with(DOWNLOADS))
    };
    let archive = url(&name).zip(url(&format!("{name}.minisig")));
    let page = if release.html_url.starts_with("https://github.com/") {
        release.html_url
    } else {
        "https://github.com/Niiihuel/OpenADE/releases/latest".to_owned()
    };
    Ok(Some(Release {
        version,
        page,
        archive,
    }))
}

/// A copy of openADE that can replace itself: the folder of a release archive
/// whose parent this user can write to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Install {
    pub root: PathBuf,
}

impl Install {
    pub fn detect() -> Option<Self> {
        if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            return None;
        }
        let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
        Self::at(executable.parent()?)
    }

    pub fn at(root: &Path) -> Option<Self> {
        (is_archive(root) && writable(root.parent()?)).then(|| Self {
            root: root.to_owned(),
        })
    }

    /// A hidden folder beside the install, on its file system, so a staged
    /// version is moved into place instead of copied.
    pub fn updates(&self) -> PathBuf {
        let name = self.root.file_name().unwrap_or_default().to_string_lossy();
        self.root.with_file_name(format!(".{name}.update"))
    }

    /// A version downloaded, verified and ready to replace this one.
    pub fn staged(&self, current: &semver::Version) -> Option<(semver::Version, PathBuf)> {
        fs::read_dir(self.updates())
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let version = semver::Version::parse(entry.file_name().to_str()?).ok()?;
                (version > *current && entry.path().join("openade").is_file())
                    .then(|| (version, entry.path()))
            })
            .max_by(|a, b| a.0.cmp(&b.0))
    }

    /// Why the last attempt to put a staged version in place failed.
    pub fn last_error(&self) -> Option<String> {
        fs::read_to_string(self.updates().join(LAST_ERROR))
            .ok()
            .map(|text| text.chars().take(400).collect::<String>().trim().to_owned())
            .filter(|text| !text.is_empty())
    }

    /// Starts the staged version to put itself in place once process `pid`
    /// has exited, and to open it then when `relaunch`.
    pub fn spawn_apply(&self, staged: &Path, pid: u32, relaunch: bool) -> io::Result<()> {
        let _ = fs::remove_file(self.updates().join(LAST_ERROR));
        let mut command = std::process::Command::new(staged.join("openade"));
        command
            .arg(APPLY_FLAG)
            .arg(pid.to_string())
            .arg(&self.root)
            .args(relaunch.then_some("--relaunch"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        detach(&mut command);
        command.spawn().map(drop)
    }
}

/// The files a release archive brings and an update needs.
fn is_archive(root: &Path) -> bool {
    ["install.sh", "openade", "agents-window", "cef/libcef.so"]
        .iter()
        .all(|file| root.join(file).is_file())
}

#[cfg(unix)]
fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: a valid C string that outlives the call.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

#[cfg(not(unix))]
fn writable(_: &Path) -> bool {
    false
}

/// In a session of its own, so it outlives this process.
fn detach(command: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches no shared state.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = command;
}

/// What Settings shows about this copy's updates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    UpToDate,
    /// Newer, not downloaded.
    Available { version: String, page: String, signed: bool },
    Downloading { version: String, received: u64, total: u64 },
    /// Downloaded and verified: it goes in at the next restart or quit.
    Ready { version: String, page: String },
    Failed(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateView {
    pub status: UpdateStatus,
    /// This copy can replace itself.
    pub installable: bool,
    /// Check every few hours and download in the background.
    pub automatic: bool,
}

/// What a check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    /// Newer, and not downloaded: this copy cannot replace itself, the
    /// release has no signed archive, or the user downloads by hand.
    Available(Release),
    /// Newer, verified and staged at the path.
    Staged(Release, PathBuf),
}

#[derive(Debug)]
pub enum Message {
    Downloading {
        version: semver::Version,
        received: u64,
        total: u64,
    },
    Done(Result<Outcome, String>),
}

/// Checks for a newer release on a thread of its own and, with `install`
/// and `download`, downloads and stages it. The last message is `Done`.
pub fn spawn_check(install: Option<Install>, download: bool) -> Receiver<Message> {
    let (sender, receiver) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("app-update".into())
        .spawn({
            let sender = sender.clone();
            move || {
                let result = check(install.as_ref(), download, &sender);
                let _ = sender.send(Message::Done(result));
            }
        });
    if let Err(error) = spawned {
        let _ = sender.send(Message::Done(Err(error.to_string())));
    }
    receiver
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(Duration::from_secs(10))
        .timeout(None)
        .user_agent(concat!("openADE/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())
}

fn get(
    client: &reqwest::blocking::Client,
    url: &str,
    timeout: Duration,
) -> Result<reqwest::blocking::Response, String> {
    let request = client.get(url).timeout(timeout);
    let request = if url.starts_with("https://api.github.com/") {
        request.header("Accept", "application/vnd.github+json")
    } else {
        request
    };
    let response = request
        .send()
        .map_err(|error| format!("No se pudo conectar con GitHub: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("GitHub respondió {}", response.status()));
    }
    Ok(response)
}

fn read_limited(response: reqwest::blocking::Response, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("La respuesta de GitHub es demasiado grande".into());
    }
    Ok(bytes)
}

fn check(install: Option<&Install>, download: bool, sender: &Sender<Message>) -> Result<Outcome, String> {
    let client = client()?;
    let json = read_limited(get(&client, LATEST_RELEASE, Duration::from_secs(30))?, MAX_RELEASE_JSON)?;
    let Some(release) = newer_release(&json, &current_version())? else {
        return Ok(Outcome::UpToDate);
    };
    let (Some(install), true, Some((archive_url, signature_url))) =
        (install, download, release.archive.clone())
    else {
        return Ok(Outcome::Available(release));
    };
    let updates = install.updates();
    fs::create_dir_all(&updates).map_err(|error| error.to_string())?;
    // Another openADE running from this copy may be downloading it already.
    let lock = File::create(updates.join(LOCK)).map_err(|error| error.to_string())?;
    if fs2::FileExt::try_lock_exclusive(&lock).is_err() {
        return Ok(Outcome::Available(release));
    }
    if let Some((version, path)) = install.staged(&current_version()) {
        if version == release.version {
            return Ok(Outcome::Staged(release, path));
        }
    }
    clear(&updates);
    let signature = String::from_utf8(read_limited(
        get(&client, &signature_url, Duration::from_secs(60))?,
        MAX_SIGNATURE,
    )?)
    .map_err(|_| "La firma de la actualización no es texto".to_owned())?;
    let archive = updates.join("download.tar.gz");
    let mut response = get(&client, &archive_url, Duration::from_secs(60 * 60))?;
    let total = response.content_length().unwrap_or(0);
    if total > MAX_ARCHIVE {
        return Err("El archivo de la actualización es demasiado grande".into());
    }
    let mut file = File::create(&archive).map_err(|error| error.to_string())?;
    let mut buffer = vec![0; 256 * 1024];
    let mut received = 0u64;
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|error| format!("La descarga se interrumpió: {error}"))?;
        if read == 0 {
            break;
        }
        received += read as u64;
        if received > MAX_ARCHIVE {
            return Err("El archivo de la actualización es demasiado grande".into());
        }
        file.write_all(&buffer[..read]).map_err(|error| error.to_string())?;
        let _ = sender.send(Message::Downloading {
            version: release.version.clone(),
            received,
            total,
        });
    }
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    let staged = stage(&archive, &signature, PUBLIC_KEY, &release.version, &updates);
    let _ = fs::remove_file(&archive);
    Ok(Outcome::Staged(release, staged?))
}

/// Removes everything in the updates folder but its lock.
fn clear(updates: &Path) {
    for entry in fs::read_dir(updates).into_iter().flatten().flatten() {
        if entry.file_name() != LOCK {
            let path = entry.path();
            let _ = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
        }
    }
}

/// Verifies `archive` against `signature` and unpacks it into
/// `updates/<version>`, which is returned. Nothing is unpacked from an
/// archive whose signature does not hold.
pub fn stage(
    archive: &Path,
    signature: &str,
    public_key: &str,
    version: &semver::Version,
    updates: &Path,
) -> Result<PathBuf, String> {
    let invalid = || "La firma de la actualización no es válida".to_owned();
    let key = minisign_verify::PublicKey::from_base64(public_key).map_err(|_| invalid())?;
    let signature = minisign_verify::Signature::decode(signature).map_err(|_| invalid())?;
    let name = archive_name(version);
    // The trusted comment is signed too: an older archive cannot pass for this one.
    if signature.trusted_comment() != name {
        return Err(invalid());
    }
    let mut verifier = key.verify_stream(&signature).map_err(|_| invalid())?;
    let mut file = File::open(archive).map_err(|error| error.to_string())?;
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        verifier.update(&buffer[..read]);
    }
    verifier.finalize().map_err(|_| invalid())?;

    let unpacking = updates.join(format!("unpacking-{version}"));
    let _ = fs::remove_dir_all(&unpacking);
    let file = File::open(archive).map_err(|error| error.to_string())?;
    tar::Archive::new(flate2::read::GzDecoder::new(file))
        .unpack(&unpacking)
        .map_err(|error| format!("No se pudo descomprimir la actualización: {error}"))?;
    let unpacked = unpacking.join(name.trim_end_matches(".tar.gz"));
    if !is_archive(&unpacked) {
        let _ = fs::remove_dir_all(&unpacking);
        return Err("La actualización no tiene los archivos esperados".into());
    }
    let staged = updates.join(version.to_string());
    let _ = fs::remove_dir_all(&staged);
    let moved = fs::rename(&unpacked, &staged).map_err(|error| error.to_string());
    let _ = fs::remove_dir_all(&unpacking);
    moved.map(|()| staged)
}

/// `--apply-update <pid> <root> [--relaunch]`, run from the staged version:
/// waits for `pid` to exit, puts this version in place of `root` and opens it
/// when asked. A failure is left in the updates folder for the app to show.
pub fn apply_main(arguments: &[std::ffi::OsString]) -> i32 {
    let (Some(pid), Some(root)) = (
        arguments.first().and_then(|pid| pid.to_str()?.parse::<u32>().ok()),
        arguments.get(1).map(PathBuf::from),
    ) else {
        return 2;
    };
    let relaunch = arguments.get(2).is_some_and(|flag| flag == "--relaunch");
    let staged = match std::env::current_exe().and_then(|exe| exe.canonicalize()) {
        Ok(executable) => executable.parent().map(Path::to_owned),
        Err(_) => None,
    };
    let install = Install { root };
    let result = staged
        .ok_or_else(|| "No se encontró la versión descargada".to_owned())
        .and_then(|staged| {
            wait_for_exit(pid, Duration::from_secs(120))?;
            apply(&staged, &install.root)
        });
    match result {
        Ok(()) => {
            let _ = fs::remove_dir_all(install.updates());
            if relaunch {
                let mut command = std::process::Command::new(install.root.join("openade"));
                detach(&mut command);
                let _ = command.spawn();
            }
            0
        }
        Err(error) => {
            let _ = fs::write(install.updates().join(LAST_ERROR), &error);
            1
        }
    }
}

fn wait_for_exit(pid: u32, limit: Duration) -> Result<(), String> {
    let start = std::time::Instant::now();
    while alive(pid) {
        if start.elapsed() > limit {
            return Err("openADE no terminó de cerrarse; la actualización se instala la próxima vez".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

/// A process that exited but was not reaped yet is a zombie: gone for this purpose.
fn alive(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z'))
    })
}

/// Moves `staged` into `root`'s place. The old copy is kept aside until the
/// new one is there, and put back if it cannot be.
pub fn apply(staged: &Path, root: &Path) -> Result<(), String> {
    let name = root.file_name().unwrap_or_default().to_string_lossy();
    let old = root.with_file_name(format!(".{name}.old"));
    let _ = fs::remove_dir_all(&old);
    fs::rename(root, &old).map_err(|error| format!("No se pudo apartar la versión instalada: {error}"))?;
    if let Err(error) = fs::rename(staged, root) {
        let _ = fs::rename(&old, root);
        return Err(format!("No se pudo instalar la versión nueva: {error}"));
    }
    // A copy that cannot be removed is only disk space; the next update retries.
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

#[cfg(test)]
#[path = "app_update_tests.rs"]
mod tests;
