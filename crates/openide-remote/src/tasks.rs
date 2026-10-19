//! The task list and a task's conversation, as the phone reads them.

use crate::bridge::Live;
use crate::error::{ApiError, ApiResult};
use crate::projection::{self, PendingPermission, PublicEvent};
use openide_core::{AgentEventKind, SessionStore};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// User prompts a first page of a conversation goes back to.
const HISTORY_PROMPTS: usize = 10;
/// Events a page holds at most, whatever the prompts.
const HISTORY_EVENTS: usize = 400;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TaskView {
    pub id: String,
    pub title: String,
    pub project: String,
    pub agent: String,
    pub backend: String,
    pub updated_at_ms: u64,
    pub pinned: bool,
    pub unread: bool,
    /// `idle`, `running` or `waiting` (for the person).
    pub state: &'static str,
    pub queued: usize,
    /// Its conversation can be read: CLI tasks live in a terminal.
    pub chat: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AgentView {
    pub backend: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Overview {
    pub tasks: Vec<TaskView>,
    pub projects: Vec<ProjectView>,
    /// The agents a new task can be started with.
    pub agents: Vec<AgentView>,
}

/// A folder a project may be: absolute and not the filesystem's root.
fn usable(path: &Path) -> bool {
    path.is_absolute() && path.components().count() > 1
}

pub fn project_id(path: &Path) -> String {
    format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()))[..16].to_owned()
}

fn project_view(path: &Path) -> ProjectView {
    ProjectView {
        id: project_id(path),
        name: path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned()),
        path: path.display().to_string(),
    }
}

fn agent_name(live: &Live, backend: &str) -> String {
    if let Some(agent) = live.agents.iter().find(|agent| agent.backend == backend) {
        return agent.name.clone();
    }
    match backend {
        "native" => "openADE".to_owned(),
        other => other.strip_prefix("acp:").unwrap_or(other).to_owned(),
    }
}

pub fn has_chat(backend: &str) -> bool {
    backend == "native" || backend.starts_with("acp:")
}

/// When the task last did something: its journal's last write.
fn updated_at_ms(sessions: &Path, id: &str) -> u64 {
    std::fs::metadata(sessions.join(format!("{id}.jsonl")))
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_millis() as u64)
}

pub fn overview(sessions: &Path, live: &Live) -> Overview {
    let tasks: Vec<TaskView> = live
        .tasks
        .iter()
        .map(|row| TaskView {
            id: row.id.clone(),
            title: row.title.clone(),
            project: project_id(&row.project),
            agent: agent_name(live, &row.backend),
            backend: row.backend.clone(),
            updated_at_ms: updated_at_ms(sessions, &row.id),
            pinned: row.pinned,
            unread: row.unread,
            state: if row.waiting {
                "waiting"
            } else if row.running {
                "running"
            } else {
                "idle"
            },
            queued: row.queued,
            chat: has_chat(&row.backend),
        })
        .collect();
    let mut projects: Vec<ProjectView> = Vec::new();
    for path in live.tasks.iter().map(|row| &row.project).chain(&live.projects) {
        if usable(path) && !projects.iter().any(|known| Path::new(&known.path) == path) {
            projects.push(project_view(path));
        }
    }
    let agents = live
        .agents
        .iter()
        .filter(|agent| agent.startable)
        .map(|agent| AgentView { backend: agent.backend.clone(), name: agent.name.clone() })
        .collect();
    Overview { tasks, projects, agents }
}

/// The folder of the project the phone names, if the PC knows it.
pub fn project_path(live: &Live, id: &str) -> Option<PathBuf> {
    live.tasks
        .iter()
        .map(|row| &row.project)
        .chain(&live.projects)
        .find(|path| usable(path) && project_id(path) == id)
        .cloned()
}

#[derive(Debug, Serialize)]
pub struct History {
    pub events: Vec<PublicEvent>,
    /// Permissions and questions the task waits on now.
    pub pending: Vec<PendingPermission>,
    /// Where the stream goes on from: the journal's last event.
    pub cursor: u64,
    /// Older events remain before this page.
    pub has_more: bool,
}

/// A user's message: what a page of the conversation goes back to.
fn prompt(kind: &AgentEventKind) -> bool {
    matches!(
        kind,
        AgentEventKind::TurnAdmitted { .. }
            | AgentEventKind::TurnAdmittedWithImages { .. }
            | AgentEventKind::TurnSteered { .. }
            | AgentEventKind::TurnSteeredWithImages { .. }
    )
}

/// The end of a task's conversation, or the part before `before`: back to
/// its last few prompts, so a long task opens fast.
pub fn history(store: &SessionStore, id: &str, before: Option<u64>) -> ApiResult<History> {
    // Read as it is: a reader never repairs a journal another one writes.
    let stored = store.load_strict(id).map_err(|_| ApiError::unavailable())?.ok_or_else(ApiError::not_found)?;
    let bound = before.unwrap_or(u64::MAX);
    let events = &stored.events[..stored.events.partition_point(|event| event.sequence < bound)];
    let prompts: Vec<usize> = events.iter().enumerate().filter(|(_, event)| prompt(&event.kind)).map(|(index, _)| index).collect();
    let from_prompts = prompts.len().checked_sub(HISTORY_PROMPTS).map_or(0, |index| prompts[index]);
    let start = from_prompts.max(events.len().saturating_sub(HISTORY_EVENTS));
    // Projection walks the whole journal for each event's turn; it starts
    // emitting after the event before the page.
    let mut cursor = if start == 0 { 0 } else { events[start - 1].sequence };
    let mut projected = Vec::new();
    loop {
        let page = projection::project_events(events, cursor, usize::MAX);
        projected.extend(page.events);
        cursor = page.next_cursor;
        if !page.has_more {
            break;
        }
    }
    Ok(History {
        events: projected,
        pending: projection::pending_permissions(&stored.events),
        cursor: stored.events.last().map_or(0, |event| event.sequence),
        has_more: start > 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{Agent, TaskRow};

    #[test]
    fn a_long_task_opens_at_its_last_prompts_and_pages_back_without_gaps() {
        use openide_core::{AgentEvent, TurnMode};
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let session = store.create("/w/aurora", "native").unwrap();
        let mut sequence = 0;
        let mut append = |kind| {
            sequence += 1;
            store.append_event(&session.id, &AgentEvent::new(&session.id, sequence, kind)).unwrap();
        };
        for turn in 0..25 {
            append(AgentEventKind::TurnAdmitted {
                turn_id: format!("t{turn}"),
                prompt: format!("pregunta {turn}"),
                model: "m".into(),
                mode: TurnMode::Build,
            });
            // A long answer: a page of events goes past projection's byte cap.
            append(AgentEventKind::AssistantMessage { content: "x".repeat(100 * 1024) });
        }
        let first = history(&store, &session.id, None).unwrap();
        let prompts: Vec<_> = first.events.iter().filter_map(|event| event.text.as_deref()).filter(|text| text.starts_with("pregunta")).collect();
        assert_eq!(prompts.first(), Some(&"pregunta 15"));
        assert_eq!(prompts.last(), Some(&"pregunta 24"));
        assert_eq!(first.events.len(), 20, "every event of the last ten turns, past the byte cap");
        assert_eq!(first.cursor, 50);
        assert!(first.has_more);
        let older = history(&store, &session.id, Some(first.events[0].seq)).unwrap();
        assert_eq!(older.events.last().unwrap().seq + 1, first.events[0].seq);
        assert_eq!(older.events.len(), 20);
        assert!(older.has_more);
    }

    #[test]
    fn rows_become_tasks_with_their_project_agent_and_state() {
        let live = Live {
            tasks: vec![
                TaskRow {
                    id: "a".into(),
                    title: "Arreglar el login".into(),
                    project: "/w/aurora".into(),
                    backend: "acp:opencode".into(),
                    running: true,
                    waiting: true,
                    ..TaskRow::default()
                },
                TaskRow { id: "b".into(), title: "Otra".into(), project: "/w/rune".into(), backend: "claude".into(), ..TaskRow::default() },
            ],
            agents: vec![Agent { backend: "acp:opencode".into(), name: "OpenCode".into(), startable: true }],
            projects: vec!["/w/aurora".into(), "/w/nuevo".into()],
            desktop_focused: false,
        };
        let overview = overview(Path::new("/nowhere"), &live);
        assert_eq!(overview.tasks[0].agent, "OpenCode");
        assert_eq!(overview.tasks[0].state, "waiting");
        assert!(overview.tasks[0].chat && !overview.tasks[1].chat);
        assert_eq!(overview.tasks[1].state, "idle");
        let names: Vec<_> = overview.projects.iter().map(|project| project.name.as_str()).collect();
        assert_eq!(names, ["aurora", "rune", "nuevo"]);
        assert_eq!(project_path(&live, &overview.projects[2].id), Some(PathBuf::from("/w/nuevo")));
        assert_eq!(overview.agents.len(), 1);
    }
}
