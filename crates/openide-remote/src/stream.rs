//! One stream per phone: the task list whenever it changes and, for the task
//! the phone has open, its new events read from the journal as they land.
//! The journal is the replay buffer: a phone that comes back names the last
//! event it has and gets exactly what followed.

use crate::bridge::{Bridge, Connection};
use crate::projection::{self, PublicEvent};
use crate::tasks::{self, Overview};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use openide_core::AgentEvent;
use serde::Serialize;
use serde_json::json;
use std::convert::Infallible;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How often the open task's journal is looked at.
const JOURNAL_POLL: Duration = Duration::from_millis(150);
/// A list whose only change is activity times is sent this often at most.
const ACTIVITY_INTERVAL: Duration = Duration::from_secs(5);
/// How often the list is rebuilt when the window has published nothing new.
const LIST_INTERVAL: Duration = Duration::from_secs(1);

/// The events of a journal, read as it grows.
struct Tail {
    path: PathBuf,
    offset: u64,
    /// The file read so far: a journal replaced by another one is rewritten.
    file: Option<u64>,
    events: Vec<AgentEvent>,
}

enum Growth {
    Same,
    Grew,
    /// It was rewritten (a rewind): what was read no longer holds.
    Rewritten,
}

impl Tail {
    fn new(path: PathBuf) -> Self {
        Self { path, offset: 0, file: None, events: Vec::new() }
    }

    fn read(&mut self) -> Growth {
        let Ok(mut file) = std::fs::File::open(&self.path) else { return Growth::Same };
        let Ok(metadata) = file.metadata() else { return Growth::Same };
        let len = metadata.len();
        #[cfg(unix)]
        let identity = Some(std::os::unix::fs::MetadataExt::ino(&metadata));
        #[cfg(not(unix))]
        let identity = None;
        let replaced = self.file.is_some() && identity != self.file;
        self.file = identity;
        if len < self.offset || replaced {
            self.offset = 0;
            self.events.clear();
            return Growth::Rewritten;
        }
        if len == self.offset || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return Growth::Same;
        }
        let mut bytes = Vec::new();
        if file.take(len - self.offset).read_to_end(&mut bytes).is_err() {
            return Growth::Same;
        }
        // A line still being written waits for its end.
        let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else { return Growth::Same };
        for line in bytes[..=end].split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
            if let Ok(event) = serde_json::from_slice::<AgentEvent>(line) {
                self.events.push(event);
            }
        }
        self.offset += end as u64 + 1;
        Growth::Grew
    }
}

/// The parts of the list a person notices, without activity times.
fn shape(overview: &Overview) -> Overview {
    let mut shape = overview.clone();
    for task in &mut shape.tasks {
        task.updated_at_ms = 0;
    }
    shape
}

fn event(name: &str, data: &impl Serialize) -> Event {
    Event::default().event(name).data(serde_json::to_string(data).unwrap_or_default())
}

pub(crate) fn open(
    bridge: Arc<Bridge>,
    connection: Connection,
    sessions: PathBuf,
    watch: Option<String>,
    after: u64,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (sender, mut receiver) = mpsc::channel::<Event>(64);
    tokio::spawn(async move {
        let _connection = connection;
        let _ = run(bridge, &sessions, watch, after, sender).await;
    });
    let stream = futures_util::stream::poll_fn(move |context| {
        receiver.poll_recv(context).map(|event| event.map(Ok))
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping"))
}

async fn run(
    bridge: Arc<Bridge>,
    sessions: &Path,
    watch: Option<String>,
    mut cursor: u64,
    sender: mpsc::Sender<Event>,
) -> Result<(), mpsc::error::SendError<Event>> {
    let mut changes = bridge.subscribe();
    let mut tail = watch.as_ref().map(|id| Tail::new(sessions.join(format!("{id}.jsonl"))));
    let mut sent: Option<(Overview, Instant)> = None;
    let mut pending = None;
    let mut listed: Option<Instant> = None;
    let mut published = true;
    loop {
        if published || listed.is_none_or(|at| at.elapsed() >= LIST_INTERVAL) {
            listed = Some(Instant::now());
            let overview = tasks::overview(sessions, &bridge.live());
            let due = match &sent {
                None => true,
                Some((last, at)) => {
                    shape(last) != shape(&overview) || (*last != overview && at.elapsed() >= ACTIVITY_INTERVAL)
                }
            };
            if due {
                sender.send(event("overview", &overview)).await?;
                sent = Some((overview, Instant::now()));
            }
        }
        if let (Some(tail), Some(task)) = (tail.as_mut(), watch.as_deref()) {
            match tail.read() {
                Growth::Same => {}
                Growth::Rewritten => {
                    cursor = 0;
                    sender.send(event("reset", &json!({"task": task}))).await?;
                }
                Growth::Grew => {
                    let page = projection::project_events(&tail.events, cursor, usize::MAX);
                    if !page.events.is_empty() {
                        let events: Vec<PublicEvent> = page.events;
                        sender.send(event("events", &json!({"task": task, "events": events}))).await?;
                    }
                    cursor = cursor.max(page.next_cursor);
                    let now = projection::pending_permissions(&tail.events);
                    let keys: Vec<String> = now.iter().map(|item| item.request_id.clone()).collect();
                    if pending.as_ref() != Some(&keys) {
                        sender.send(event("pending", &json!({"task": task, "pending": now}))).await?;
                        pending = Some(keys);
                    }
                }
            }
        }
        if sender.is_closed() {
            return Ok(());
        }
        published = tokio::select! {
            _ = changes.changed() => true,
            _ = tokio::time::sleep(JOURNAL_POLL) => false,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::AgentEventKind;
    use std::io::Write;

    fn line(sequence: u64, kind: AgentEventKind) -> String {
        let event = AgentEvent { session_id: "s".into(), sequence, timestamp_ms: sequence, kind };
        format!("{}\n", serde_json::to_string(&event).unwrap())
    }

    #[test]
    fn a_journal_is_read_as_it_grows_and_a_half_written_line_waits() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("s.jsonl");
        let first = line(1, AgentEventKind::TurnStarted { prompt: "hola".into() });
        let second = line(2, AgentEventKind::AssistantDelta { content: "Hola".into() });
        std::fs::write(&path, &first).unwrap();
        let mut tail = Tail::new(path.clone());
        assert!(matches!(tail.read(), Growth::Grew));
        assert!(matches!(tail.read(), Growth::Same));
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&second.as_bytes()[..10]).unwrap();
        assert!(matches!(tail.read(), Growth::Same));
        file.write_all(&second.as_bytes()[10..]).unwrap();
        assert!(matches!(tail.read(), Growth::Grew));
        assert_eq!(tail.events.len(), 2);
        std::fs::write(&path, &first).unwrap();
        assert!(matches!(tail.read(), Growth::Rewritten));
        assert!(matches!(tail.read(), Growth::Grew));
        assert_eq!(tail.events.len(), 1);
    }
}
