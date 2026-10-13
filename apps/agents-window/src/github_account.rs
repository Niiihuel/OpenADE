//! Verified GitHub identity and cancellable browser authentication. Registered
//! OAuth helpers own the credential store; tokens never enter UI/session data.

use std::io::{self, BufRead, Cursor, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use image::{ImageFormat, ImageReader};
use openide_core::{UserImage, UserImageMime};
use openide_ui::{RasterImage, RasterImageFormat};
use serde_json::Value;

use crate::child_process::command_no_window;
use crate::settings_preferences::GithubAccountPreference;

const GITHUB_API_USER: &str = "https://api.github.com/user";
const GITHUB_URL: &str = "https://github.com";
const MAX_CREDENTIAL_OUTPUT: usize = 16 * 1024;
pub const DEVICE_VERIFICATION_URL: &str = "https://github.com/login/device";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GithubIdentity {
    pub login: String,
    pub avatar: Option<RasterImage>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum GithubAccountStatus {
    #[default]
    Checking,
    Disconnected,
    Verifying(String),
    Unverified {
        accounts: Vec<String>,
        notice: Option<String>,
    },
    Connected(GithubIdentity),
    Unavailable(String),
}

enum Request {
    // Automatic account discovery is disabled in host fixtures.
    #[cfg_attr(test, allow(dead_code))]
    Refresh(GithubAccountPreference),
    Connect,
    Verify(String),
}

#[derive(Clone, Debug)]
pub enum GithubAccountEvent {
    Status(GithubAccountStatus),
    DeviceCode(String),
    BrowserSignIn,
}

#[derive(Clone, Debug)]
pub enum GithubSignInView {
    Preparing,
    Code {
        code: String,
        continued: bool,
    },
    Browser,
    /// A helper found several accounts. Choosing one starts verification;
    /// the currently connected identity remains active until that succeeds.
    ChooseAccount {
        accounts: Vec<String>,
        notice: Option<String>,
    },
    Error(String),
}

pub struct GithubAccountWorker {
    request_tx: Sender<(u64, Request)>,
    result_rx: Receiver<(u64, bool, GithubAccountEvent)>,
    generation: Arc<AtomicU64>,
}

impl GithubAccountWorker {
    /// Threadless transport used by host regressions. It can never start a
    /// credential helper, contact GitHub, or consume the user's real account.
    #[cfg(test)]
    pub(crate) fn test_fixture() -> (Self, GithubAccountTestDriver) {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        (
            Self {
                request_tx,
                result_rx,
                generation: Arc::new(AtomicU64::new(0)),
            },
            GithubAccountTestDriver {
                request_rx,
                result_tx,
            },
        )
    }

    pub fn spawn(preference: GithubAccountPreference) -> io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);
        thread::Builder::new()
            .name("openide-github-account".to_owned())
            .spawn(move || {
                while let Ok((revision, request)) = request_rx.recv() {
                    if worker_generation.load(Ordering::Acquire) != revision {
                        continue;
                    }
                    let initial_probe = matches!(&request, Request::Refresh(_));
                    let result = match request {
                        Request::Refresh(preference) => restore_account(&preference),
                        Request::Connect => {
                            connect_account(revision, &worker_generation, &result_tx)
                        }
                        Request::Verify(account) => verify_account(&account),
                    };
                    if result_tx
                        .send((revision, initial_probe, GithubAccountEvent::Status(result)))
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        let worker = Self {
            request_tx,
            result_rx,
            generation,
        };
        // Host fixtures must not query the user's saved account or public
        // services simply by constructing a window. Explicit requests still
        // exercise the same worker in tests.
        #[cfg(not(test))]
        let _ = worker.request_tx.send((0, Request::Refresh(preference)));
        #[cfg(test)]
        let _ = preference;
        Ok(worker)
    }

    pub fn connect(&self) -> bool {
        self.request(Request::Connect)
    }

    pub fn verify(&self, account: &str) -> bool {
        self.request(Request::Verify(account.to_owned()))
    }

    pub fn refresh(&self, preference: GithubAccountPreference) -> bool {
        self.request(Request::Refresh(preference))
    }

    fn request(&self, request: Request) -> bool {
        let revision = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.request_tx.send((revision, request)).is_ok()
    }

    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn poll_latest(&self) -> Option<(bool, GithubAccountEvent)> {
        let mut latest = None;
        while let Ok((revision, initial_probe, result)) = self.result_rx.try_recv() {
            if self.generation.load(Ordering::Acquire) == revision {
                latest = Some((initial_probe, result));
            }
        }
        latest
    }
}

impl Drop for GithubAccountWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
pub(crate) struct GithubAccountTestDriver {
    request_rx: Receiver<(u64, Request)>,
    result_tx: Sender<(u64, bool, GithubAccountEvent)>,
}

#[cfg(test)]
impl GithubAccountTestDriver {
    pub(crate) fn request(&self) -> (u64, &'static str) {
        let (revision, request) = self.request_rx.try_recv().expect("an explicit request");
        let kind = match request {
            Request::Connect => "connect",
            Request::Verify(_) => "verify",
            Request::Refresh(GithubAccountPreference::Disconnected) => "refresh_disconnected",
            Request::Refresh(_) => "refresh",
        };
        (revision, kind)
    }

    pub(crate) fn respond(&self, revision: u64, event: GithubAccountEvent) {
        self.result_tx.send((revision, false, event)).unwrap();
    }
}

fn github_login(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 39
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn account_name(line: &str) -> Option<String> {
    let line = line.trim();
    let candidate = line
        .strip_prefix("https://")
        .and_then(|url| url.split_once("@github.com").map(|(name, _)| name))
        .unwrap_or(line);
    github_login(candidate).then(|| candidate.to_owned())
}

fn normalize_accounts(accounts: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut accounts: Vec<String> = accounts
        .into_iter()
        .filter(|account| github_login(account))
        .collect();
    accounts.sort_by_key(|account| account.to_ascii_lowercase());
    accounts.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    accounts.truncate(8);
    accounts
}

fn unverified_accounts(accounts: Vec<String>, notice: Option<String>) -> GithubAccountStatus {
    let accounts = normalize_accounts(accounts);
    if accounts.is_empty() {
        GithubAccountStatus::Disconnected
    } else {
        GithubAccountStatus::Unverified { accounts, notice }
    }
}

fn credential_password(output: &[u8]) -> Option<String> {
    if output.len() > MAX_CREDENTIAL_OUTPUT {
        return None;
    }
    let output = std::str::from_utf8(output).ok()?;
    output.lines().find_map(|line| {
        line.strip_prefix("password=")
            .filter(|token| !token.is_empty() && token.len() < 8192)
            .map(str::to_owned)
    })
}

/// Non-interactive credential reads have bounded time and output. In
/// particular, an unavailable credential store must not stall reconnection.
fn credential_output(command: &mut Command, input: Option<&[u8]>) -> Option<Vec<u8>> {
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(input) = input {
        let written = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(input).is_ok());
        if !written {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    }
    let stdout = child.stdout.take()?;
    let (output_tx, output_rx) = mpsc::channel();
    let reader = thread::Builder::new()
        .name("openide-github-credential-read".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout
                .take((MAX_CREDENTIAL_OUTPUT + 1) as u64)
                .read_to_end(&mut bytes);
            let _ = output_tx.send(result.ok().map(|_| bytes));
        });
    if reader.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let deadline = Instant::now() + Duration::from_secs(7);
    let success = loop {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break false;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) => thread::sleep(Duration::from_millis(40)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    if !success {
        return None;
    }
    output_rx
        .recv_timeout(Duration::from_millis(200))
        .ok()
        .flatten()
        .filter(|output| output.len() <= MAX_CREDENTIAL_OUTPUT)
}

fn gcm_accounts() -> Option<Vec<String>> {
    let output = credential_output(
        command_no_window("git")
            .args([
                "credential-manager",
                "github",
                "list",
                "--url",
                GITHUB_URL,
                "--no-ui",
            ])
            .env("GCM_INTERACTIVE", "never")
            .env("GIT_TERMINAL_PROMPT", "0"),
        None,
    )?;
    Some(normalize_accounts(
        String::from_utf8_lossy(&output)
            .lines()
            .filter_map(account_name),
    ))
}

fn gcm_token(username: &str) -> Option<String> {
    let mut request = "protocol=https\nhost=github.com\n".to_owned();
    if !username.is_empty() {
        request.push_str(&format!("username={username}\n"));
    }
    request.push('\n');
    let output = credential_output(
        command_no_window("git")
            .args(["credential-manager", "get", "--no-ui"])
            .env("GCM_INTERACTIVE", "never")
            .env("GIT_TERMINAL_PROMPT", "0"),
        Some(request.as_bytes()),
    )?;
    credential_password(&output)
}

fn gh_token() -> Option<String> {
    gh_token_for(None)
}

fn gh_token_for(login: Option<&str>) -> Option<String> {
    let mut command = command_no_window("gh");
    command
        .args(["auth", "token", "--hostname", "github.com"])
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN");
    if let Some(login) = login {
        if !github_login(login) {
            return None;
        }
        command.args(["--user", login]);
    }
    let output = credential_output(&mut command, None)?;
    let token = String::from_utf8(output).ok()?.trim().to_owned();
    (!token.is_empty()).then_some(token)
}

/// Resolve credentials for an already verified, explicitly selected account.
/// This permits its own GCM credential even when several accounts are stored.
/// A token for another login is never used for the account's personal calendar.
pub fn auth_token_for_account(login: &str) -> Option<String> {
    if !github_login(login) {
        return None;
    }
    let environment = ["GH_TOKEN", "GITHUB_TOKEN"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok());
    let gh = std::iter::once_with(|| gh_token_for(Some(login))).flatten();
    let gcm = std::iter::once_with(|| {
        let account = gcm_accounts()?
            .into_iter()
            .find(|account| account.eq_ignore_ascii_case(login))?;
        gcm_token(&account)
    })
    .flatten();
    matching_account_token(login, environment.chain(gh).chain(gcm), identity_for_token)
}

fn matching_account_token(
    login: &str,
    candidates: impl IntoIterator<Item = String>,
    mut identity: impl FnMut(&str) -> Option<GithubIdentity>,
) -> Option<String> {
    if !github_login(login) {
        return None;
    }
    candidates
        .into_iter()
        .filter(|token| !token.trim().is_empty())
        .find(|token| {
            identity(token).is_some_and(|identity| identity.login.eq_ignore_ascii_case(login))
        })
}

fn identity_for_token(token: &str) -> Option<GithubIdentity> {
    request_identity(token, false)
}

fn profile_for_token(token: &str) -> Option<GithubIdentity> {
    request_identity(token, true)
}

fn request_identity(token: &str, photo: bool) -> Option<GithubIdentity> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(7))
        .build()
        .ok()?;
    let response = client
        .get(GITHUB_API_USER)
        .bearer_auth(token)
        .header(reqwest::header::USER_AGENT, "OpenIDE-Agent-Window")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut bytes = Vec::new();
    response.take(32 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 32 * 1024 {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let mut identity = parse_identity(&value)?;
    if photo {
        identity.avatar = value
            .get("avatar_url")
            .and_then(Value::as_str)
            .and_then(|url| load_avatar(&client, url, &identity.login));
    }
    Some(identity)
}

fn parse_identity(value: &Value) -> Option<GithubIdentity> {
    let login = value.get("login")?.as_str()?;
    github_login(login).then(|| GithubIdentity {
        login: login.to_owned(),
        avatar: None,
    })
}

fn load_avatar(client: &reqwest::blocking::Client, url: &str, login: &str) -> Option<RasterImage> {
    const MAX_AVATAR_BYTES: usize = 2 * 1024 * 1024;
    let mut url = reqwest::Url::parse(url).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("avatars.githubusercontent.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    // GitHub serves a small rendition; no bearer credential is sent to its CDN.
    url.set_query(Some("s=128"));
    let response = client.get(url).send().ok()?.error_for_status().ok()?;
    let mut bytes = Vec::new();
    response
        .take((MAX_AVATAR_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_AVATAR_BYTES {
        return None;
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(32 * 1024 * 1024);
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    let side = decoded.width().min(decoded.height());
    let photo = decoded
        .crop_imm(
            (decoded.width() - side) / 2,
            (decoded.height() - side) / 2,
            side,
            side,
        )
        .thumbnail(128, 128);
    let mut encoded = Cursor::new(Vec::new());
    photo.write_to(&mut encoded, ImageFormat::Png).ok()?;
    let image = UserImage::from_bytes(UserImageMime::Png, &encoded.into_inner()).ok()?;
    RasterImage::new(
        image.sha256,
        RasterImageFormat::Png,
        image.size_bytes,
        Arc::from(image.data_base64),
        format!("Foto de perfil de {login}"),
    )
}

fn discover_account() -> GithubAccountStatus {
    // Account probing must never request a GCM credential: `get` may rotate a
    // stored token. A listed account stays unverified until an explicit click.
    for token in [
        std::env::var("GH_TOKEN").ok(),
        std::env::var("GITHUB_TOKEN").ok(),
        gh_token(),
    ]
    .into_iter()
    .flatten()
    .filter(|token| !token.trim().is_empty())
    {
        if let Some(identity) = profile_for_token(&token) {
            return GithubAccountStatus::Connected(identity);
        }
    }
    unverified_accounts(gcm_accounts().unwrap_or_default(), None)
}

fn restore_account(preference: &GithubAccountPreference) -> GithubAccountStatus {
    match preference {
        GithubAccountPreference::Automatic => discover_account(),
        GithubAccountPreference::Disconnected => GithubAccountStatus::Disconnected,
        GithubAccountPreference::Selected(login) => auth_token_for_account(login)
            .and_then(|token| profile_for_token(&token))
            .filter(|identity| identity.login.eq_ignore_ascii_case(login))
            .map(GithubAccountStatus::Connected)
            .unwrap_or_else(|| GithubAccountStatus::Unavailable(format!(
                "No se pudo verificar la cuenta {login}. Actualiza la conexión o reconecta con GitHub."
            ))),
    }
}

fn verify_account(account: &str) -> GithubAccountStatus {
    let accounts = gcm_accounts().unwrap_or_default();
    if !accounts
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(account))
    {
        return unverified_accounts(
            accounts,
            Some("La cuenta seleccionada ya no está disponible en Git.".to_owned()),
        );
    }
    if let Some(identity) = gcm_token(account).and_then(|token| profile_for_token(&token)) {
        if identity.login.eq_ignore_ascii_case(account) {
            return GithubAccountStatus::Connected(identity);
        }
    }
    unverified_accounts(
        accounts,
        Some("No se pudo verificar esa cuenta. Reconéctala con GitHub.".to_owned()),
    )
}

fn account_to_verify<'a>(before: &[String], after: &'a [String]) -> Option<&'a str> {
    if after.len() == 1 {
        return Some(&after[0]);
    }
    let added: Vec<&String> = after
        .iter()
        .filter(|account| {
            !before
                .iter()
                .any(|known| known.eq_ignore_ascii_case(account))
        })
        .collect();
    (added.len() == 1).then(|| added[0].as_str())
}

fn device_code(line: &str) -> Option<String> {
    let (_, code) = line.split_once("one-time code:")?;
    let code = code.trim();
    (code.len() == 9
        && code.as_bytes()[4] == b'-'
        && code
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || byte.is_ascii_uppercase() || byte.is_ascii_digit()))
    .then(|| code.to_owned())
}

fn connect_account(
    revision: u64,
    generation: &AtomicU64,
    events: &Sender<(u64, bool, GithubAccountEvent)>,
) -> GithubAccountStatus {
    // Piped streams make gh non-interactive: it returns the public one-time
    // code without opening a browser. The native alert owns Copy & Continue.
    // We use the installed helper's registered OAuth app, not VS Code's IDs.
    let mut command = command_no_window("gh");
    command
        .args([
            "auth",
            "login",
            "--hostname",
            "github.com",
            "--web",
            "--skip-ssh-key",
            "--scopes",
            "read:user",
            "--clipboard=false",
        ])
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .env("GH_ACCESSIBLE_COLORS", "disabled")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return connect_with_gcm(revision, generation, events);
        }
        Err(_) => {
            return GithubAccountStatus::Unavailable(
                "No se pudo iniciar el acceso a GitHub. Vuelve a intentarlo.".to_owned(),
            )
        }
    };
    let (line_tx, line_rx) = mpsc::sync_channel(8);
    let reader = child.stderr.take().and_then(|stderr| {
        thread::Builder::new()
            .name("openide-github-login-output".to_owned())
            .spawn(move || {
                for line in io::BufReader::new(stderr.take(MAX_CREDENTIAL_OUTPUT as u64)).lines() {
                    let Ok(line) = line else {
                        break;
                    };
                    // Only a validated public code crosses into the UI.
                    if let Some(code) = device_code(&line) {
                        let _ = line_tx.try_send(code);
                    }
                }
            })
            .ok()
    });
    if reader.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        return GithubAccountStatus::Unavailable("No se pudo preparar el inicio de sesión.".into());
    }
    let deadline = Instant::now() + Duration::from_secs(16 * 60);
    let status = loop {
        while let Ok(code) = line_rx.try_recv() {
            let _ = events.send((revision, false, GithubAccountEvent::DeviceCode(code)));
        }
        if generation.load(Ordering::Acquire) != revision || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => thread::sleep(Duration::from_millis(80)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    if let Some(reader) = reader {
        let _ = reader.join();
    }
    if generation.load(Ordering::Acquire) != revision {
        return GithubAccountStatus::Disconnected;
    }
    if !status.is_some_and(|status| status.success()) {
        return GithubAccountStatus::Unavailable(
            "No se completó la autorización de GitHub. El código pudo vencer o el acceso fue rechazado. Vuelve a intentarlo.".to_owned());
    }
    gh_token().and_then(|token| profile_for_token(&token))
        .map(GithubAccountStatus::Connected)
        .unwrap_or_else(|| GithubAccountStatus::Unavailable(
            "GitHub autorizó el acceso, pero no se pudo verificar el perfil. Vuelve a intentarlo.".into()))
}

fn wait_for_login(command: &mut Command, revision: u64, generation: &AtomicU64) -> bool {
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(5 * 60);
    loop {
        if generation.load(Ordering::Acquire) != revision || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => thread::sleep(Duration::from_millis(80)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn connect_with_gcm(
    revision: u64,
    generation: &AtomicU64,
    events: &Sender<(u64, bool, GithubAccountEvent)>,
) -> GithubAccountStatus {
    let before = gcm_accounts().unwrap_or_default();
    if generation.load(Ordering::Acquire) != revision {
        return GithubAccountStatus::Disconnected;
    }
    let _ = events.send((revision, false, GithubAccountEvent::BrowserSignIn));
    let result = wait_for_login(
        command_no_window("git")
            .args(["credential-manager", "github", "login", "--browser"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
        revision,
        generation,
    );
    match result {
        true => {
            let accounts = gcm_accounts().unwrap_or_default();
            if let Some(account) = account_to_verify(&before, &accounts) {
                verify_account(account)
            } else if accounts.is_empty() {
                GithubAccountStatus::Unavailable(
                    "GitHub no confirmó la conexión. Vuelve a intentarlo.".to_owned(),
                )
            } else {
                unverified_accounts(
                    accounts,
                    Some("Elige la cuenta que quieres usar en openADE.".to_owned()),
                )
            }
        }
        false => GithubAccountStatus::Unavailable(
            "No se pudo iniciar sesión. Instala GitHub CLI o Git Credential Manager para conectar tu cuenta desde el navegador.".to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_token_source_must_match_the_selected_api_identity() {
        let candidates = [
            "environment-other",
            "cli-other",
            "gcm-other",
            "gcm-selected",
            "unused",
        ]
        .into_iter()
        .map(str::to_owned);
        let mut checked = Vec::new();
        let token = matching_account_token("Alice", candidates, |token| {
            checked.push(token.to_owned());
            Some(GithubIdentity {
                login: if token == "gcm-selected" {
                    "alice"
                } else {
                    "bob"
                }
                .into(),
                avatar: None,
            })
        });
        assert_eq!(token.as_deref(), Some("gcm-selected"));
        assert_eq!(
            checked,
            [
                "environment-other",
                "cli-other",
                "gcm-other",
                "gcm-selected"
            ]
        );
        assert!(
            matching_account_token("alice", ["gcm-other".to_owned()], |_| {
                Some(GithubIdentity {
                    login: "bob".into(),
                    avatar: None,
                })
            })
            .is_none()
        );
        assert!(matching_account_token(
            "../bad",
            std::iter::once_with(|| panic!("must not read credentials")),
            |_| None
        )
        .is_none());
    }

    #[test]
    fn disconnected_restore_is_local_and_cancel_discards_all_late_events() {
        assert_eq!(
            restore_account(&GithubAccountPreference::Disconnected),
            GithubAccountStatus::Disconnected
        );
        let (worker, driver) = GithubAccountWorker::test_fixture();
        assert!(worker.connect());
        let (revision, kind) = driver.request();
        assert_eq!(kind, "connect");
        worker.cancel();
        for event in [
            GithubAccountEvent::DeviceCode("ABCD-1234".into()),
            GithubAccountEvent::BrowserSignIn,
            GithubAccountEvent::Status(GithubAccountStatus::Connected(GithubIdentity {
                login: "alice".into(),
                avatar: None,
            })),
        ] {
            driver.respond(revision, event);
        }
        assert!(worker.poll_latest().is_none());
    }

    #[test]
    fn account_name_accepts_only_a_github_login() {
        assert_eq!(account_name("nihue"), Some("nihue".to_owned()));
        assert_eq!(
            account_name("https://nihue@github.com"),
            Some("nihue".to_owned())
        );
        for bad in [
            "",
            "github.com",
            "../evil",
            "user name",
            "-invalid",
            "invalid-",
        ] {
            assert!(account_name(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn credential_parser_only_extracts_password_field() {
        let output = b"protocol=https\nhost=github.com\nusername=nihue\npassword=secret\n";
        assert_eq!(credential_password(output), Some("secret".to_owned()));
        assert_eq!(credential_password(b"username=nihue\n"), None);
    }

    #[test]
    fn cached_gcm_accounts_are_explicitly_unverified() {
        let status =
            unverified_accounts(vec!["Other".into(), "nihue".into(), "NIHUE".into()], None);
        assert_eq!(
            status,
            GithubAccountStatus::Unverified {
                accounts: vec!["nihue".into(), "Other".into()],
                notice: None,
            }
        );
        assert_eq!(
            unverified_accounts(Vec::new(), None),
            GithubAccountStatus::Disconnected
        );
    }

    #[test]
    fn multiple_cached_accounts_never_select_the_first_implicitly() {
        let before = vec!["alice".to_owned(), "bob".to_owned()];
        assert_eq!(account_to_verify(&before, &before), None);
        let after = vec!["alice".to_owned(), "bob".to_owned(), "charlie".to_owned()];
        assert_eq!(account_to_verify(&before, &after), Some("charlie"));
        let after = vec![
            "alice".to_owned(),
            "bob".to_owned(),
            "charlie".to_owned(),
            "dora".to_owned(),
        ];
        assert_eq!(account_to_verify(&before, &after), None);
    }

    #[test]
    fn only_api_identity_with_a_valid_login_can_be_connected() {
        assert_eq!(
            parse_identity(&serde_json::json!({"login": "nihue"})),
            Some(GithubIdentity {
                login: "nihue".into(),
                avatar: None,
            })
        );
        assert!(parse_identity(&serde_json::json!({"login": "../wrong"})).is_none());
        assert!(parse_identity(&serde_json::json!({"name": "nihue"})).is_none());
    }
}
