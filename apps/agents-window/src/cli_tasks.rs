//! What openADE keeps of a task run by an agent CLI in a terminal: the
//! permissions it starts with, and the CLI's own session, so that starting
//! the CLI again (after a restart, or from the task's Reiniciar) resumes the
//! conversation instead of opening a new one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_cli_definitions::{AgentCliDefinition, SessionDiscovery};

const FILE: &str = "cli-tasks.json";
/// How long a CLI gets to create its session before the search gives up.
const DISCOVERY_PATIENCE: Duration = Duration::from_secs(120);
const DISCOVERY_PERIOD: Duration = Duration::from_secs(2);
/// Clocks the CLI and this process read may disagree by this much.
const CLOCK_SLACK_MS: u64 = 5_000;

/// The permissions a CLI is started with. "Sólo lectura" is the CLI's own
/// plan or read-only mode; "Permitir todo" is its accept-all flag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CliMode {
    #[default]
    Ask,
    ReadOnly,
    FullAccess,
}

impl CliMode {
    pub(crate) const ALL: [Self; 3] = [Self::Ask, Self::ReadOnly, Self::FullAccess];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Ask => "Pedir aprobación",
            Self::ReadOnly => "Sólo lectura",
            Self::FullAccess => "Permitir todo",
        }
    }

    pub(crate) fn action(self) -> &'static str {
        match self {
            Self::Ask => "cli_mode:ask",
            Self::ReadOnly => "cli_mode:read_only",
            Self::FullAccess => "cli_mode:full_access",
        }
    }

    pub(crate) fn from_action(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.action() == id)
    }

    /// Whether `agent` has a flag for this mode; Ask is how every CLI starts.
    pub(crate) fn offered_by(self, agent: &AgentCliDefinition) -> bool {
        match self {
            Self::Ask => true,
            Self::ReadOnly => !agent.read_only_args.is_empty(),
            Self::FullAccess => !agent.accept_all_args.is_empty(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CliTask {
    #[serde(default)]
    pub mode: CliMode,
    /// The CLI's own session, once known: what a start resumes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// When the CLI was last started without a session to resume, in ms
    /// since the epoch: the session it created then is the one to find.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
}

pub(crate) fn load(root: &Path) -> BTreeMap<String, CliTask> {
    std::fs::read(root.join(FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn get(root: &Path, session_id: &str) -> CliTask {
    load(root).remove(session_id).unwrap_or_default()
}

pub(crate) fn save(root: &Path, session_id: &str, task: &CliTask) -> Result<(), String> {
    let mut all = load(root);
    all.insert(session_id.to_owned(), task.clone());
    let bytes = serde_json::to_vec_pretty(&all).map_err(|error| error.to_string())?;
    openide_harness::atomic_write(&root.join(FILE), &bytes)
        .map_err(|error| format!("No se pudo guardar el estado de la tarea CLI: {error}"))
}

/// Where the CLIs keep their sessions on this machine.
#[derive(Clone, Debug)]
pub(crate) struct Homes {
    /// `CLAUDE_CONFIG_DIR`, or `~/.claude`.
    pub claude: Option<PathBuf>,
    /// `CODEX_HOME`, or `~/.codex`.
    pub codex: Option<PathBuf>,
}

impl Homes {
    pub(crate) fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        Self {
            claude: std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|home| home.join(".claude"))),
            codex: crate::composer_selection::codex_home(),
        }
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis().min(u128::from(u64::MAX)) as u64)
}

/// The arguments that make the CLI open the task's conversation: its own
/// session when one is known, otherwise a session openADE names for it, if
/// the CLI takes a name. `task` is updated with what was decided.
pub(crate) fn session_args(agent: &AgentCliDefinition, homes: &Homes, task: &mut CliTask) -> Vec<String> {
    let with = |template: &[String], session: &str| -> Vec<String> {
        template.iter().map(|arg| arg.replace("{session}", session)).collect()
    };
    match agent.sessions {
        Some(SessionDiscovery::ClaudeProjects) if !agent.new_session_args.is_empty() => {
            let session = task
                .session
                .get_or_insert_with(|| uuid::Uuid::new_v4().to_string())
                .clone();
            // A name the CLI never wrote a conversation under is given again:
            // resuming it would only fail.
            if homes.claude.as_deref().is_some_and(|home| claude_session_exists(home, &session)) {
                with(&agent.resume_args, &session)
            } else {
                with(&agent.new_session_args, &session)
            }
        }
        _ => match &task.session {
            Some(session) if !agent.resume_args.is_empty() => with(&agent.resume_args, session),
            _ => {
                task.started_at_ms = Some(now_ms());
                Vec::new()
            }
        },
    }
}

/// Claude Code writes `<session>.jsonl` under the project's folder; the
/// folder's name encodes the path, so every project is looked at.
fn claude_session_exists(home: &Path, session: &str) -> bool {
    let Ok(projects) = std::fs::read_dir(home.join("projects")) else {
        return false;
    };
    let file = format!("{session}.jsonl");
    projects
        .flatten()
        .take(10_000)
        .any(|project| project.path().join(&file).is_file())
}

/// Looks, for a while, for the session the CLI created after `started_at_ms`
/// in `workspace`, skipping `taken` (other tasks' sessions). Answers once:
/// the session, or nothing when the CLI never made one.
pub(crate) fn discover(
    discovery: SessionDiscovery,
    homes: &Homes,
    program: PathBuf,
    search_path: Option<std::ffi::OsString>,
    workspace: PathBuf,
    started_at_ms: u64,
    taken: Vec<String>,
) -> Receiver<Option<String>> {
    let (sender, receiver) = mpsc::channel();
    let codex = homes.codex.clone();
    std::thread::spawn(move || {
        let deadline = Instant::now() + DISCOVERY_PATIENCE;
        loop {
            let found = match discovery {
                SessionDiscovery::CodexRollouts => {
                    codex.as_deref().and_then(|home| codex_session(home, &workspace, started_at_ms, &taken))
                }
                SessionDiscovery::OpencodeList => opencode_session(&program, search_path.as_deref(), &workspace, started_at_ms, &taken),
                SessionDiscovery::ClaudeProjects => None,
            };
            if found.is_some() || Instant::now() >= deadline {
                let _ = sender.send(found);
                return;
            }
            std::thread::sleep(DISCOVERY_PERIOD);
        }
    });
    receiver
}

/// The earliest Codex session started in `workspace` at or after
/// `started_at_ms`, from its rollout files (`sessions/YYYY/MM/DD/`).
pub(crate) fn codex_session(home: &Path, workspace: &Path, started_at_ms: u64, taken: &[String]) -> Option<String> {
    let since = SystemTime::UNIX_EPOCH + Duration::from_millis(started_at_ms.saturating_sub(CLOCK_SLACK_MS));
    let mut pending = vec![(home.join("sessions"), 0usize)];
    let mut visited = 0;
    let mut best: Option<(u64, String)> = None;
    while let Some((directory, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > 50_000 {
                return best.map(|(_, id)| id);
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if kind.is_dir() && depth < 3 {
                pending.push((entry.path(), depth + 1));
            } else if kind.is_file()
                && name.starts_with("rollout-")
                && name.ends_with(".jsonl")
                && entry.metadata().and_then(|file| file.modified()).is_ok_and(|modified| modified >= since)
            {
                if let Some((at, id)) = codex_session_meta(&entry.path(), workspace, started_at_ms) {
                    if !taken.contains(&id) && best.as_ref().is_none_or(|(best_at, _)| at < *best_at) {
                        best = Some((at, id));
                    }
                }
            }
        }
    }
    best.map(|(_, id)| id)
}

/// The id and start of the session a rollout file records, when it is one
/// of `workspace` started at or after `started_at_ms`.
fn codex_session_meta(path: &Path, workspace: &Path, started_at_ms: u64) -> Option<(u64, String)> {
    use std::io::{BufRead, BufReader, Read};
    let file = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    BufReader::new(file.take(64 * 1024)).read_line(&mut first).ok()?;
    let line: Value = serde_json::from_str(first.trim()).ok()?;
    if line["type"] != "session_meta" {
        return None;
    }
    let meta = &line["payload"];
    if Path::new(meta["cwd"].as_str()?) != workspace {
        return None;
    }
    let at = chrono::DateTime::parse_from_rfc3339(meta["timestamp"].as_str()?).ok()?.timestamp_millis().max(0) as u64;
    if at + CLOCK_SLACK_MS < started_at_ms {
        return None;
    }
    let id = meta["id"].as_str().or_else(|| meta["session_id"].as_str())?;
    Some((at, id.to_owned()))
}

/// Asks opencode for the project's sessions and takes the earliest one
/// created at or after `started_at_ms`.
fn opencode_session(
    program: &Path,
    search_path: Option<&std::ffi::OsStr>,
    workspace: &Path,
    started_at_ms: u64,
    taken: &[String],
) -> Option<String> {
    let mut command = std::process::Command::new(program);
    command
        .args(["session", "list", "--format", "json", "--max-count", "20"])
        .current_dir(workspace)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(path) = search_path {
        command.env("PATH", path);
    }
    let output = command.output().ok()?;
    opencode_session_from_list(&String::from_utf8_lossy(&output.stdout), workspace, started_at_ms, taken)
}

pub(crate) fn opencode_session_from_list(list: &str, workspace: &Path, started_at_ms: u64, taken: &[String]) -> Option<String> {
    let sessions: Vec<Value> = serde_json::from_str(list.trim()).ok()?;
    sessions
        .iter()
        .filter_map(|session| {
            let id = session["id"].as_str()?;
            let created = session["created"].as_u64()?;
            (Path::new(session["directory"].as_str()?) == workspace
                && created + CLOCK_SLACK_MS >= started_at_ms
                && !taken.iter().any(|other| other == id))
                .then(|| (created, id.to_owned()))
        })
        .min()
        .map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rollout(home: &Path, day: &str, name: &str, cwd: &str, timestamp: &str) {
        let directory = home.join("sessions").join(day);
        std::fs::create_dir_all(&directory).unwrap();
        let meta = serde_json::json!({"timestamp": timestamp, "type": "session_meta", "payload": {
            "id": name, "session_id": name, "timestamp": timestamp, "cwd": cwd, "originator": "codex-tui"}});
        std::fs::write(directory.join(format!("rollout-{name}.jsonl")), format!("{meta}\n{{\"type\":\"other\"}}\n")).unwrap();
    }

    #[test]
    fn the_codex_session_of_a_start_is_the_earliest_of_its_workspace_after_it_and_not_another_tasks() {
        let home = tempfile::tempdir().unwrap();
        let started = chrono::DateTime::parse_from_rfc3339("2026-10-17T12:00:00Z").unwrap().timestamp_millis() as u64;
        rollout(home.path(), "2026/10/17", "before", "/w", "2026-10-17T11:00:00Z");
        rollout(home.path(), "2026/10/17", "elsewhere", "/other", "2026-10-17T12:00:01Z");
        rollout(home.path(), "2026/10/17", "mine", "/w", "2026-10-17T12:00:02Z");
        rollout(home.path(), "2026/10/17", "later", "/w", "2026-10-17T12:05:00Z");
        assert_eq!(codex_session(home.path(), Path::new("/w"), started, &[]).as_deref(), Some("mine"));
        assert_eq!(codex_session(home.path(), Path::new("/w"), started, &["mine".into()]).as_deref(), Some("later"));
        assert_eq!(codex_session(home.path(), Path::new("/w"), started + 3_600_000, &[]), None);
    }

    #[test]
    fn the_opencode_session_comes_from_its_list_by_directory_and_time() {
        let list = r#"[
          {"id": "ses_new", "title": "t", "updated": 2, "created": 1792236954763, "projectId": "p", "directory": "/w"},
          {"id": "ses_old", "title": "t", "updated": 1, "created": 1700000000000, "projectId": "p", "directory": "/w"},
          {"id": "ses_other", "title": "t", "updated": 3, "created": 1792236954763, "projectId": "q", "directory": "/other"}
        ]"#;
        assert_eq!(opencode_session_from_list(list, Path::new("/w"), 1792236950000, &[]).as_deref(), Some("ses_new"));
        assert_eq!(opencode_session_from_list(list, Path::new("/w"), 1792236950000, &["ses_new".into()]), None);
        assert_eq!(opencode_session_from_list("no es json", Path::new("/w"), 0, &[]), None);
    }

    #[test]
    fn claude_is_named_a_session_until_it_has_written_it_and_resumes_it_after() {
        let root = tempfile::tempdir().unwrap();
        let homes = Homes { claude: Some(root.path().join("claude")), codex: None };
        let claude = crate::agent_cli_definitions::profile("claude").unwrap().0;
        let mut task = CliTask::default();
        let first = session_args(claude, &homes, &mut task);
        let session = task.session.clone().expect("a session is named");
        assert_eq!(first, ["--session-id", session.as_str()]);
        assert_eq!(session_args(claude, &homes, &mut task), first, "named again while nothing was written");
        let project = root.path().join("claude/projects/-w");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join(format!("{session}.jsonl")), "{}\n").unwrap();
        assert_eq!(session_args(claude, &homes, &mut task), ["--resume", session.as_str()]);
        // Codex gets no name: its session is found after the start.
        let codex = crate::agent_cli_definitions::profile("codex").unwrap().0;
        let mut task = CliTask::default();
        assert!(session_args(codex, &homes, &mut task).is_empty());
        assert!(task.started_at_ms.is_some());
        task.session = Some("abc".into());
        assert_eq!(session_args(codex, &homes, &mut task), ["resume", "abc"]);
        save(root.path(), "task", &task).unwrap();
        assert_eq!(get(root.path(), "task"), task);
        assert_eq!(get(root.path(), "other"), CliTask::default());
    }
}
