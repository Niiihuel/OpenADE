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
fn parse_remote(value: &str, remote_name: &str) -> Option<GithubRepository> {
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
            })
        })
        .collect()
}

fn fetch_pulls(repository: &GithubRepository, branch: &str) -> Result<Vec<GithubPull>, String> {
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
    if let Some(token) = crate::github_account::auth_token() {
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
    Ok(parse_pulls(&value, repository, branch))
}

pub fn load_overview(workspace: &Path) -> Option<GithubOverview> {
    let repository = discover_repository(workspace)?;
    let branch = current_branch(workspace).unwrap_or_else(|| "HEAD separado".to_owned());
    let (pulls, notice) = if branch == "HEAD separado" {
        (
            Vec::new(),
            Some("HEAD separado: no hay una rama para buscar PR".to_owned()),
        )
    } else {
        match fetch_pulls(&repository, &branch) {
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
