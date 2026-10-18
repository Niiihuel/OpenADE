//! GitHub context for a local checkout. Discovery uses Git remotes and public
//! read-only REST requests; a GitHub CLI, Git Credential Manager or environment
//! token grants access to private repositories. Credentials stay outside views.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;

use crate::child_process::command_no_window;

const MAX_RESPONSE_BYTES: u64 = 512 * 1024;
const MAX_PULLS: usize = 20;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GithubRepository {
    pub owner: String,
    pub name: String,
    pub remote_name: String,
    /// Owner of the branch pushed from this checkout. For a fork this may
    /// differ from the repository that receives the pull request.
    pub head_owner: String,
}

impl GithubRepository {
    pub fn web_url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.name)
    }

    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    pub fn pull_url(&self, number: u64) -> String {
        format!("{}/pull/{number}", self.web_url())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GithubPull {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub base: String,
    pub draft: bool,
    /// The commit the pull request is at, for asking about its checks.
    pub head: String,
    /// How its checks stand, once they could be read.
    pub checks: Option<PullChecks>,
}

/// How the checks of a pull request's last commit stand, all together.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PullChecks {
    Passing,
    Failing,
    Pending,
    /// The repository runs no check on it.
    None,
}

impl PullChecks {
    pub fn label(self) -> &'static str {
        match self {
            Self::Passing => "Checks exitosos",
            Self::Failing => "Checks fallando",
            Self::Pending => "Checks pendientes",
            Self::None => "Sin checks de CI",
        }
    }
}

/// One verdict from GitHub's two reports on a commit: its check runs and the
/// combined status older integrations still set. A failure anywhere fails
/// it; otherwise anything unfinished keeps it pending.
fn checks_of(check_runs: &Value, status: &Value) -> PullChecks {
    let mut seen = Vec::new();
    for run in check_runs.get("check_runs").and_then(Value::as_array).into_iter().flatten() {
        seen.push(match (run.get("status").and_then(Value::as_str), run.get("conclusion").and_then(Value::as_str)) {
            (Some("completed"), Some("success" | "neutral" | "skipped")) => PullChecks::Passing,
            (Some("completed"), _) => PullChecks::Failing,
            _ => PullChecks::Pending,
        });
    }
    // The combined status says `pending` also when nothing ever reported.
    if status.get("statuses").and_then(Value::as_array).is_some_and(|statuses| !statuses.is_empty()) {
        seen.push(match status.get("state").and_then(Value::as_str) {
            Some("success") => PullChecks::Passing,
            Some("failure" | "error") => PullChecks::Failing,
            _ => PullChecks::Pending,
        });
    }
    [PullChecks::Failing, PullChecks::Pending, PullChecks::Passing]
        .into_iter()
        .find(|state| seen.contains(state))
        .unwrap_or(PullChecks::None)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GithubOverview {
    pub repository: GithubRepository,
    pub branch: String,
    pub pulls: Vec<GithubPull>,
    pub notice: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum GithubStatus {
    #[default]
    Loading,
    NoRemote,
    Ready(GithubOverview),
    Unavailable(String),
}

fn valid_slug(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment.len() <= 100
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

/// Only recognize github.com exactly. A local or lookalike remote must never
/// turn into an API request or a browser link labelled as GitHub.
pub(crate) fn parse_remote(value: &str, remote_name: &str) -> Option<GithubRepository> {
    let value = value.trim();
    let path = value
        .strip_prefix("https://github.com/")
        .or_else(|| value.strip_prefix("ssh://git@github.com/"))
        .or_else(|| value.strip_prefix("git@github.com:"))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    if name.contains('/') || !valid_slug(owner) || !valid_slug(name) {
        return None;
    }
    Some(GithubRepository {
        owner: owner.to_owned(),
        name: name.to_owned(),
        remote_name: remote_name.to_owned(),
        head_owner: owner.to_owned(),
    })
}

fn git_output(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = command_no_window("git")
        .args(args)
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.len() > 16 * 1024 {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn discover_repository(workspace: &Path) -> Option<GithubRepository> {
    let remote_names = git_output(workspace, &["remote"])?;
    let mut names: Vec<&str> = remote_names.lines().take(32).collect();
    names.sort_by_key(|name| if *name == "origin" { 0 } else { 1 });
    let mut repositories = Vec::new();
    for name in names {
        if let Some(url) = git_output(workspace, &["remote", "get-url", name]) {
            if let Some(repository) = parse_remote(&url, name) {
                repositories.push(repository);
            }
        }
    }
    let origin = repositories
        .iter()
        .find(|repository| repository.remote_name == "origin");
    let upstream = repositories
        .iter()
        .find(|repository| repository.remote_name == "upstream");
    if let (Some(origin), Some(upstream)) = (origin, upstream) {
        if origin.full_name() != upstream.full_name() {
            let mut base = upstream.clone();
            base.head_owner = origin.owner.clone();
            return Some(base);
        }
    }
    origin.cloned().or_else(|| repositories.into_iter().next())
}

fn current_branch(workspace: &Path) -> Option<String> {
    let branch = git_output(workspace, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    (!branch.is_empty() && branch.len() <= 256).then_some(branch)
}

fn parse_pulls(value: &Value, repository: &GithubRepository, branch: &str) -> Vec<GithubPull> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_PULLS)
        .filter_map(|entry| {
            let head = entry.get("head")?.get("ref")?.as_str()?;
            if head != branch {
                return None;
            }
            if let Some(owner) = entry
                .get("head")
                .and_then(|head| head.get("user"))
                .and_then(|user| user.get("login"))
                .and_then(Value::as_str)
            {
                if !owner.eq_ignore_ascii_case(&repository.head_owner) {
                    return None;
                }
            }
            let number = entry.get("number")?.as_u64()?;
            let title: String = entry
                .get("title")?
                .as_str()?
                .chars()
                .filter(|ch| !ch.is_control())
                .take(240)
                .collect();
            Some(GithubPull {
                number,
                title,
                url: repository.pull_url(number),
                author: entry
                    .get("user")
                    .and_then(|user| user.get("login"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(100)
                    .collect(),
                base: entry
                    .get("base")
                    .and_then(|base| base.get("ref"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(100)
                    .collect(),
                draft: entry.get("draft").and_then(Value::as_bool).unwrap_or(false),
                head: entry
                    .get("head")
                    .and_then(|head| head.get("sha"))
                    .and_then(Value::as_str)
                    .filter(|sha| sha.len() <= 64 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .unwrap_or("")
                    .to_owned(),
                checks: None,
            })
        })
        .collect()
}

fn fetch_pulls(
    repository: &GithubRepository,
    branch: &str,
    account: Option<&str>,
) -> Result<Vec<GithubPull>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(7))
        .build()
        .map_err(|_| "No se pudo preparar la conexión con GitHub".to_owned())?;
    let url = format!(
        "https://api.github.com/repos/{}/{}/pulls",
        repository.owner, repository.name
    );
    let head = format!("{}:{branch}", repository.head_owner);
    let mut request = client
        .get(url)
        .query(&[("state", "open"), ("head", &head), ("per_page", "20")])
        .header(reqwest::header::USER_AGENT, "OpenIDE-Agent-Window")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json");
    if let Some(login) = account {
        let token = crate::github_account::auth_token_for_account(login)
            .ok_or_else(|| format!("Reconecta la cuenta {login} para consultar GitHub."))?;
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .map_err(|_| "No se pudo consultar GitHub".to_owned())?;
    if !response.status().is_success() {
        let status = response.status();
        return Err(match status.as_u16() {
            401 | 403 => "GitHub requiere acceso o se agotó el límite de consultas".to_owned(),
            404 => "El repositorio no está disponible con las credenciales actuales".to_owned(),
            _ => format!("GitHub respondió con HTTP {}", status.as_u16()),
        });
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "No se pudo leer la respuesta de GitHub".to_owned())?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("La respuesta de GitHub excede el límite de tamaño".to_owned());
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "GitHub devolvió una respuesta no válida".to_owned())?;
    let mut pulls = parse_pulls(&value, repository, branch);
    // Only the pull request the panel shows; a failure to read its checks
    // leaves it without a verdict rather than without the pull request.
    if let Some(pull) = pulls.first_mut().filter(|pull| !pull.head.is_empty()) {
        let token = account.and_then(crate::github_account::auth_token_for_account);
        pull.checks = commit_checks(&client, repository, &pull.head, token.as_deref());
    }
    Ok(pulls)
}

/// How the checks of commit `sha` stand; `None` when they could not be read.
pub(crate) fn commit_checks(
    client: &reqwest::blocking::Client,
    repository: &GithubRepository,
    sha: &str,
    token: Option<&str>,
) -> Option<PullChecks> {
    let report = |path: &str| -> Option<Value> {
        let mut request = client
            .get(format!("https://api.github.com/repos/{}/{}/commits/{sha}/{path}", repository.owner, repository.name))
            .query(&[("per_page", "100")])
            .header(reqwest::header::USER_AGENT, "OpenIDE-Agent-Window")
            .header(reqwest::header::ACCEPT, "application/vnd.github+json");
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request.send().ok().filter(|response| response.status().is_success())?;
        let mut bytes = Vec::new();
        response.take(MAX_RESPONSE_BYTES + 1).read_to_end(&mut bytes).ok()?;
        (bytes.len() as u64 <= MAX_RESPONSE_BYTES).then(|| serde_json::from_slice(&bytes).ok()).flatten()
    };
    report("check-runs").zip(report("status")).map(|(runs, status)| checks_of(&runs, &status))
}

/// What GitHub is asked to open: the branch into `base`, described by the
/// branch's last commit.
fn pull_payload(
    repository: &GithubRepository,
    branch: &str,
    base: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> Value {
    serde_json::json!({
        "title": title,
        "body": body,
        "head": format!("{}:{branch}", repository.head_owner),
        "base": base,
        "draft": draft,
    })
}

/// Why GitHub refused to open the pull request, in the user's terms where
/// the reason is a known one.
fn pull_refusal(status: u16, body: &[u8]) -> String {
    let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let first = value.get("errors").and_then(|errors| errors.get(0));
    let reason = first
        .and_then(|error| error.get("message"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let field = first.and_then(|error| error.get("field")).and_then(Value::as_str);
    match status {
        401 | 403 => "La cuenta de GitHub no tiene permiso para crear el pull request".to_owned(),
        404 => "El repositorio no está disponible con las credenciales actuales".to_owned(),
        422 if reason.starts_with("A pull request already exists") => {
            "Ya existe un pull request para esta rama".to_owned()
        }
        422 if reason.starts_with("No commits between") => {
            "La rama no tiene commits nuevos para un pull request".to_owned()
        }
        422 if field == Some("head") => "Publicá la rama antes de crear el pull request".to_owned(),
        _ => {
            let reason: String = reason.chars().filter(|ch| !ch.is_control()).take(200).collect();
            format!("GitHub no creó el pull request (HTTP {status}). {reason}")
        }
    }
}

pub(crate) fn api_json(request: reqwest::blocking::RequestBuilder, token: &str) -> Result<(u16, Vec<u8>), String> {
    let response = request
        .bearer_auth(token)
        .header(reqwest::header::USER_AGENT, "OpenIDE-Agent-Window")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .map_err(|_| "No se pudo consultar GitHub".to_owned())?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "No se pudo leer la respuesta de GitHub".to_owned())?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("La respuesta de GitHub excede el límite de tamaño".to_owned());
    }
    Ok((status, bytes))
}

/// Opens a pull request for the current branch into the repository's default
/// branch. The branch must already be on GitHub: nothing is pushed from here.
/// A pull request as the user wrote it. What is left empty is filled in:
/// title and description from the last commit, base with the default branch.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PullDraft {
    pub title: String,
    pub body: String,
    pub base: String,
    pub draft: bool,
}

pub fn create_pull(workspace: &Path, account: &str, draft: &PullDraft) -> Result<GithubPull, String> {
    let repository =
        discover_repository(workspace).ok_or("Este proyecto no tiene un remoto de GitHub")?;
    let branch = current_branch(workspace)
        .ok_or("HEAD separado: elegí una rama antes de crear el pull request")?;
    let written = |text: &str| Some(text.trim().to_owned()).filter(|text| !text.is_empty());
    let title = written(&draft.title)
        .or_else(|| git_output(workspace, &["log", "-1", "--format=%s"]).filter(|title| !title.is_empty()))
        .ok_or("Creá un commit antes de crear el pull request")?;
    let body = written(&draft.body)
        .unwrap_or_else(|| git_output(workspace, &["log", "-1", "--format=%b"]).unwrap_or_default());
    let token = crate::github_account::auth_token_for_account(account)
        .ok_or_else(|| format!("Reconecta la cuenta {account} para crear el pull request."))?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "No se pudo preparar la conexión con GitHub".to_owned())?;
    let api = format!(
        "https://api.github.com/repos/{}/{}",
        repository.owner, repository.name
    );
    let base = match written(&draft.base) {
        Some(base) => base,
        None => {
            let (status, bytes) = api_json(client.get(&api), &token)?;
            serde_json::from_slice::<Value>(&bytes)
                .ok()
                .filter(|_| status == 200)
                .and_then(|value| value.get("default_branch")?.as_str().map(str::to_owned))
                .ok_or_else(|| pull_refusal(status, &bytes))?
        }
    };
    if base == branch && repository.head_owner.eq_ignore_ascii_case(&repository.owner) {
        return Err("Cambiá a una rama de trabajo antes de crear el pull request".to_owned());
    }
    let payload = pull_payload(&repository, &branch, &base, &title, &body, draft.draft);
    let (status, bytes) = api_json(client.post(format!("{api}/pulls")).json(&payload), &token)?;
    if status != 201 {
        return Err(pull_refusal(status, &bytes));
    }
    let created: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "GitHub devolvió una respuesta no válida".to_owned())?;
    parse_pulls(&Value::Array(vec![created]), &repository, &branch)
        .pop()
        .ok_or_else(|| "GitHub devolvió una respuesta no válida".to_owned())
}

/// What the person does to the pull request the review panel shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PullAction {
    /// Squash its commits into one, or keep them under a merge commit.
    Merge { number: u64, squash: bool },
    Comment { number: u64, body: String },
    Approve { number: u64 },
}

/// GitHub's reason for refusing an action, in the user's terms where known.
fn action_refusal(status: u16, body: &[u8]) -> String {
    let reason: String = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("message")?.as_str().map(str::to_owned))
        .unwrap_or_default()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(200)
        .collect();
    match status {
        401 | 403 => "La cuenta de GitHub no tiene permiso para esto".to_owned(),
        404 => "El pull request no está disponible con las credenciales actuales".to_owned(),
        405 => format!("GitHub todavía no permite fusionarlo. {reason}"),
        409 => "La rama cambió mientras tanto: actualizá y probá de nuevo".to_owned(),
        422 if reason.contains("own pull request") => "GitHub no deja aprobar un pull request propio".to_owned(),
        _ => format!("GitHub respondió HTTP {status}. {reason}"),
    }
}

/// Does `action` on GitHub and says what happened.
pub fn act_on_pull(workspace: &Path, account: &str, action: &PullAction) -> Result<String, String> {
    let repository = discover_repository(workspace).ok_or("Este proyecto no tiene un remoto de GitHub")?;
    let token = crate::github_account::auth_token_for_account(account)
        .ok_or_else(|| format!("Reconecta la cuenta {account} para actuar en GitHub."))?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "No se pudo preparar la conexión con GitHub".to_owned())?;
    let api = format!("https://api.github.com/repos/{}/{}", repository.owner, repository.name);
    let (request, expected, done) = match action {
        PullAction::Merge { number, squash } => (
            client
                .put(format!("{api}/pulls/{number}/merge"))
                .json(&serde_json::json!({ "merge_method": if *squash { "squash" } else { "merge" } })),
            200,
            format!("Pull request #{number} fusionado"),
        ),
        PullAction::Comment { number, body } => (
            client.post(format!("{api}/issues/{number}/comments")).json(&serde_json::json!({ "body": body })),
            201,
            format!("Comentario publicado en #{number}"),
        ),
        PullAction::Approve { number } => (
            client.post(format!("{api}/pulls/{number}/reviews")).json(&serde_json::json!({ "event": "APPROVE" })),
            200,
            format!("Pull request #{number} aprobado"),
        ),
    };
    let (status, bytes) = api_json(request, &token)?;
    if status == expected {
        Ok(done)
    } else {
        Err(action_refusal(status, &bytes))
    }
}

pub fn load_overview(workspace: &Path, account: Option<&str>) -> Option<GithubOverview> {
    let repository = discover_repository(workspace)?;
    let branch = current_branch(workspace).unwrap_or_else(|| "HEAD separado".to_owned());
    let (pulls, notice) = if branch == "HEAD separado" {
        (
            Vec::new(),
            Some("HEAD separado: no hay una rama para buscar PR".to_owned()),
        )
    } else {
        match fetch_pulls(&repository, &branch, account) {
            Ok(pulls) => (pulls, None),
            Err(error) => (Vec::new(), Some(error)),
        }
    };
    Some(GithubOverview {
        repository,
        branch,
        pulls,
        notice,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checks_of_a_commit_are_one_verdict_and_a_failure_decides_it() {
        let runs = |runs: &[(&str, Option<&str>)]| {
            serde_json::json!({"check_runs": runs.iter().map(|(status, conclusion)| serde_json::json!({"status": status, "conclusion": conclusion})).collect::<Vec<_>>()})
        };
        let none = serde_json::json!({"state": "pending", "statuses": []});
        let old = |state: &str| serde_json::json!({"state": state, "statuses": [{"state": state}]});
        let (done, ok, bad) = ("completed", Some("success"), Some("failure"));
        assert_eq!(checks_of(&runs(&[]), &none), PullChecks::None);
        assert_eq!(checks_of(&runs(&[(done, ok), (done, Some("skipped")), (done, Some("neutral"))]), &none), PullChecks::Passing);
        assert_eq!(checks_of(&runs(&[(done, ok), ("in_progress", None)]), &none), PullChecks::Pending);
        assert_eq!(checks_of(&runs(&[(done, ok), ("queued", None), (done, bad)]), &none), PullChecks::Failing);
        assert_eq!(checks_of(&runs(&[(done, Some("timed_out"))]), &none), PullChecks::Failing);
        // What an older integration reported counts the same.
        assert_eq!(checks_of(&runs(&[]), &old("success")), PullChecks::Passing);
        assert_eq!(checks_of(&runs(&[(done, ok)]), &old("failure")), PullChecks::Failing);
        assert_eq!(checks_of(&runs(&[(done, ok)]), &old("pending")), PullChecks::Pending);
        // A commit of a pull request is asked about by its own hash only.
        let repository = parse_remote("git@github.com:owner/repo.git", "origin").unwrap();
        let pull = |sha: &str| serde_json::json!([{"number": 3, "title": "T", "head": {"ref": "topic", "sha": sha}}]);
        assert_eq!(parse_pulls(&pull("0123abcd"), &repository, "topic")[0].head, "0123abcd");
        assert_eq!(parse_pulls(&pull("../../otro"), &repository, "topic")[0].head, "");
    }

    #[test]
    fn a_new_pull_names_the_forks_branch_and_a_refusal_is_told_in_the_users_terms() {
        let mut repository = parse_remote("git@github.com:owner/repo.git", "origin").unwrap();
        repository.head_owner = "fork".into();
        let payload = pull_payload(&repository, "topic", "main", "Título", "Cuerpo", true);
        assert_eq!(
            payload,
            serde_json::json!({"title":"Título","body":"Cuerpo","head":"fork:topic","base":"main","draft":true})
        );
        let refused = |status, body: &str| pull_refusal(status, body.as_bytes());
        assert_eq!(
            refused(422, r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for fork:topic."}]}"#),
            "Ya existe un pull request para esta rama"
        );
        assert_eq!(
            refused(422, r#"{"errors":[{"message":"No commits between main and topic"}]}"#),
            "La rama no tiene commits nuevos para un pull request"
        );
        assert_eq!(
            refused(422, r#"{"errors":[{"resource":"PullRequest","field":"head","code":"invalid"}]}"#),
            "Publicá la rama antes de crear el pull request"
        );
        assert_eq!(refused(403, "{}"), "La cuenta de GitHub no tiene permiso para crear el pull request");
        // An unknown reason is shown bounded, without control characters.
        let odd = refused(500, &format!(r#"{{"message":"a\nb{}"}}"#, "x".repeat(400)));
        assert!(odd.starts_with("GitHub no creó el pull request (HTTP 500). ab") && odd.chars().count() < 260, "{odd}");
    }

    #[test]
    fn remote_parser_accepts_supported_github_forms_only() {
        for remote in [
            "https://github.com/owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
            "git@github.com:owner/repo.git",
        ] {
            let repository = parse_remote(remote, "origin").unwrap();
            assert_eq!(repository.full_name(), "owner/repo");
            assert_eq!(repository.web_url(), "https://github.com/owner/repo");
        }
        for remote in [
            "https://github.com.evil.test/owner/repo",
            "https://evil.test/owner/repo",
            "https://github.com/owner/repo/extra",
            "https://github.com/owner/../repo",
            "https://github.com/owner/..",
            "https://github.com/../repo",
            "https://user:secret@github.com/owner/repo",
        ] {
            assert!(parse_remote(remote, "origin").is_none(), "{remote}");
        }
    }

    #[test]
    fn pull_projection_stays_on_current_branch_and_constructs_safe_urls() {
        let repository = parse_remote("git@github.com:example/app.git", "origin").unwrap();
        let value = serde_json::json!([
            {"number": 7, "title": "Mejora", "head": {"ref": "main", "user": {"login": "example"}}, "base": {"ref": "dev"}, "user": {"login": "ana"}, "draft": false, "html_url": "https://evil.test/"},
            {"number": 8, "title": "Otra", "head": {"ref": "other", "user": {"login": "example"}}},
            {"number": 9, "title": "Ajena", "head": {"ref": "main", "user": {"login": "someone-else"}}}
        ]);
        let pulls = parse_pulls(&value, &repository, "main");
        assert_eq!(pulls.len(), 1);
        assert_eq!(pulls[0].url, "https://github.com/example/app/pull/7");
        assert_eq!(pulls[0].base, "dev");
    }

    #[test]
    fn discovery_prefers_github_origin_and_falls_back_to_another_remote() {
        let temp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = command_no_window("git")
                .args(args)
                .current_dir(temp.path())
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", "https://example.test/other.git"]);
        git(&[
            "remote",
            "add",
            "upstream",
            "git@github.com:team/project.git",
        ]);
        assert_eq!(
            discover_repository(temp.path()).unwrap().remote_name,
            "upstream"
        );
        git(&[
            "remote",
            "set-url",
            "origin",
            "https://github.com/me/fork.git",
        ]);
        assert_eq!(
            discover_repository(temp.path()).unwrap().full_name(),
            "team/project"
        );
        assert_eq!(discover_repository(temp.path()).unwrap().head_owner, "me");
    }
}
