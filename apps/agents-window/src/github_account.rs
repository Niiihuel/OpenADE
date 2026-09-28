//! GitHub identity for the application chrome. Git Credential Manager owns the
//! interactive browser sign-in and the OS credential store; this module only
//! reads a token into a short-lived worker request and never persists it.

use std::io::{self, Write};
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::child_process::command_no_window;

const GITHUB_API_USER: &str = "https://api.github.com/user";
const GITHUB_URL: &str = "https://github.com";
const MAX_CREDENTIAL_OUTPUT: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GithubIdentity {
    pub login: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum GithubAccountStatus {
    #[default]
    Checking,
    Disconnected,
    Connecting,
    Verifying(String),
    Unverified {
        accounts: Vec<String>,
        notice: Option<String>,
    },
    Connected(GithubIdentity),
    Unavailable(String),
}

enum Request {
    Refresh,
    Connect,
    Verify(String),
}

pub struct GithubAccountWorker {
    request_tx: Sender<Request>,
    result_rx: Receiver<(bool, GithubAccountStatus)>,
}

impl GithubAccountWorker {
    pub fn spawn() -> io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        thread::Builder::new()
            .name("openide-github-account".to_owned())
            .spawn(move || {
                while let Ok(request) = request_rx.recv() {
                    let initial_probe = matches!(&request, Request::Refresh);
                    let result = match request {
                        Request::Refresh => discover_account(),
                        Request::Connect => connect_account(),
                        Request::Verify(account) => verify_account(&account),
                    };
                    if result_tx.send((initial_probe, result)).is_err() {
                        break;
                    }
                }
            })?;
        let worker = Self {
            request_tx,
            result_rx,
        };
        let _ = worker.request_tx.send(Request::Refresh);
        Ok(worker)
    }

    pub fn connect(&self) -> bool {
        self.request_tx.send(Request::Connect).is_ok()
    }

    pub fn verify(&self, account: &str) -> bool {
        self.request_tx
            .send(Request::Verify(account.to_owned()))
            .is_ok()
    }

    pub fn poll_latest(&self) -> Option<(bool, GithubAccountStatus)> {
        let mut latest = None;
        while let Ok(result) = self.result_rx.try_recv() {
            latest = Some(result);
        }
        latest
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

fn gcm_accounts() -> Option<Vec<String>> {
    let output = command_no_window("git")
        .args([
            "credential-manager",
            "github",
            "list",
            "--url",
            GITHUB_URL,
            "--no-ui",
        ])
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.len() > MAX_CREDENTIAL_OUTPUT {
        return None;
    }
    Some(normalize_accounts(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(account_name),
    ))
}

fn gcm_token(username: &str) -> Option<String> {
    let mut child = command_no_window("git")
        .args(["credential-manager", "get", "--no-ui"])
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut request = "protocol=https\nhost=github.com\n".to_owned();
    if !username.is_empty() {
        request.push_str(&format!("username={username}\n"));
    }
    request.push('\n');
    child.stdin.take()?.write_all(request.as_bytes()).ok()?;
    let output = child.wait_with_output().ok()?;
    output
        .status
        .success()
        .then(|| credential_password(&output.stdout))
        .flatten()
}

fn gh_token() -> Option<String> {
    let output = command_no_window("gh")
        .args(["auth", "token"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.len() > MAX_CREDENTIAL_OUTPUT {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!token.is_empty()).then_some(token)
}

/// The token is only returned to background GitHub requests. Callers must not
/// log it or put it in `ViewState`, session data or app preferences.
pub fn auth_token() -> Option<String> {
    for name in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Some(token) = std::env::var(name)
            .ok()
            .filter(|token| !token.trim().is_empty())
        {
            return Some(token);
        }
    }
    if let Some(token) = gh_token() {
        return Some(token);
    }
    let accounts = gcm_accounts()?;
    if accounts.len() == 1 {
        return gcm_token(&accounts[0]);
    }
    None
}

fn identity_for_token(token: &str) -> Option<GithubIdentity> {
    let client = reqwest::blocking::Client::builder()
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
    let value: Value = response.json().ok()?;
    parse_identity(&value)
}

fn parse_identity(value: &Value) -> Option<GithubIdentity> {
    let login = value.get("login")?.as_str()?;
    github_login(login).then(|| GithubIdentity {
        login: login.to_owned(),
    })
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
        if let Some(identity) = identity_for_token(&token) {
            return GithubAccountStatus::Connected(identity);
        }
    }
    unverified_accounts(gcm_accounts().unwrap_or_default(), None)
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
    if let Some(identity) = gcm_token(account).and_then(|token| identity_for_token(&token)) {
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

fn connect_account() -> GithubAccountStatus {
    // This is the only path that may open a browser or write an OS credential.
    // Status discovery above is explicitly non-interactive.
    let before = gcm_accounts().unwrap_or_default();
    let result = command_no_window("git")
        .args(["credential-manager", "github", "login", "--browser"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match result {
        Ok(status) if status.success() => {
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
                    Some("Elige la cuenta que quieres usar en OpenIDE.".to_owned()),
                )
            }
        }
        _ => GithubAccountStatus::Unavailable(
            "No se pudo abrir el inicio de sesión de GitHub con Git Credential Manager.".to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                login: "nihue".into()
            })
        );
        assert!(parse_identity(&serde_json::json!({"login": "../wrong"})).is_none());
        assert!(parse_identity(&serde_json::json!({"name": "nihue"})).is_none());
    }
}
