//! What the server and the window share. The server reads the journals on
//! disk by itself; whatever changes a task goes to the window as a command,
//! since the window owns the turns, and the window publishes what is going
//! on now.

use crate::error::{ApiError, ApiResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

/// How long a command may wait for the window before the phone is told.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
/// Answers kept for repeated request ids.
const REMEMBERED: usize = 256;

/// A task as the window's sidebar shows it, with its turn now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TaskRow {
    pub id: String,
    pub title: String,
    pub project: PathBuf,
    /// `native`, `acp:<id>` or a CLI's id.
    pub backend: String,
    pub pinned: bool,
    pub unread: bool,
    pub running: bool,
    /// It waits for the person: a permission, a question or, for a CLI, input.
    pub waiting: bool,
    /// Messages queued for when the turn ends.
    pub queued: usize,
}

/// An agent that can be told to start a task, or one tasks may come from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Agent {
    /// The session backend it runs as: `native`, `acp:<id>` or a CLI's id.
    pub backend: String,
    pub name: String,
    /// A phone can start a task with it (CLIs run in a terminal on the PC).
    pub startable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Live {
    /// In the sidebar's order, archived ones left out.
    pub tasks: Vec<TaskRow>,
    pub agents: Vec<Agent>,
    /// Projects a task can be started in, besides the ones tasks already have.
    pub projects: Vec<PathBuf>,
    /// The person is at the PC: phone notices would repeat what they see.
    pub desktop_focused: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Start a turn when idle, queue it after the running one otherwise.
    Auto,
    Queue,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    /// Allow, and keep allowing this command in the project.
    AllowAlways,
    Deny,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub enum Command {
    CreateTask { project: PathBuf, backend: String, prompt: String, files: Vec<PathBuf>, plan: bool },
    Send { task: String, prompt: String, files: Vec<PathBuf>, delivery: Delivery },
    Stop { task: String },
    Permission { task: String, request_id: String, decision: Decision },
    Answer { task: String, request_id: String, answer: String },
    Patch { task: String, title: Option<String>, pinned: Option<bool>, archived: Option<bool> },
    Read { task: String },
}

/// A command waiting for the window, with where its answer goes.
pub struct Request {
    pub command: Command,
    reply: oneshot::Sender<Result<Value, String>>,
}

impl Request {
    pub fn answer(self, result: Result<Value, String>) {
        let _ = self.reply.send(result);
    }
}

/// What the window raises for the phone's notices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    Approval,
    Question,
    Completed,
    Failed,
}

pub struct Bridge {
    queue: Mutex<Vec<Request>>,
    live: watch::Sender<Live>,
    wake: Box<dyn Fn() + Send + Sync>,
    answers: Mutex<(HashMap<String, (String, Value)>, VecDeque<String>)>,
    notices: mpsc::UnboundedSender<(Notice, String)>,
    notices_out: Mutex<Option<mpsc::UnboundedReceiver<(Notice, String)>>>,
    /// Devices with an open stream, and how many.
    connected: Mutex<BTreeMap<String, usize>>,
}

impl Bridge {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        let (notices, notices_out) = mpsc::unbounded_channel();
        Arc::new(Self {
            queue: Mutex::default(),
            live: watch::channel(Live::default()).0,
            wake: Box::new(wake),
            answers: Mutex::default(),
            notices,
            notices_out: Mutex::new(Some(notices_out)),
            connected: Mutex::default(),
        })
    }

    /// The window takes what the phone asked for.
    pub fn take_commands(&self) -> Vec<Request> {
        std::mem::take(&mut *lock(&self.queue))
    }

    /// The window says what is going on; the same state again wakes no one.
    pub fn publish(&self, live: Live) {
        self.live.send_if_modified(|current| {
            let changed = *current != live;
            if changed {
                *current = live;
            }
            changed
        });
    }

    pub fn live(&self) -> Live {
        self.live.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Live> {
        self.live.subscribe()
    }

    /// The window raises a notice for the phone about `task`.
    pub fn notify(&self, notice: Notice, task: &str) {
        let _ = self.notices.send((notice, task.to_owned()));
    }

    pub(crate) fn take_notices(&self) -> Option<mpsc::UnboundedReceiver<(Notice, String)>> {
        lock(&self.notices_out).take()
    }

    pub fn connected_devices(&self) -> BTreeMap<String, usize> {
        lock(&self.connected).clone()
    }

    pub(crate) fn connect(self: &Arc<Self>, device: &str) -> Connection {
        *lock(&self.connected).entry(device.to_owned()).or_default() += 1;
        Connection { bridge: self.clone(), device: device.to_owned() }
    }

    /// Hands `command` to the window and waits for its answer. A request id
    /// seen before with the same command gets the same answer again.
    pub async fn call(&self, request_id: &str, command: Command) -> ApiResult<Value> {
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&command).unwrap_or_default()));
        if let Some((seen, answer)) = lock(&self.answers).0.get(request_id) {
            return if *seen == digest {
                Ok(answer.clone())
            } else {
                Err(ApiError::conflict("Ese pedido ya se usó para otra cosa"))
            };
        }
        let (reply, answer) = oneshot::channel();
        lock(&self.queue).push(Request { command, reply });
        (self.wake)();
        let value = match tokio::time::timeout(COMMAND_TIMEOUT, answer).await {
            Ok(Ok(Ok(value))) => value,
            Ok(Ok(Err(message))) => return Err(ApiError::conflict(message)),
            Ok(Err(_)) => return Err(ApiError::unavailable()),
            Err(_) => return Err(ApiError::busy()),
        };
        let mut answers = lock(&self.answers);
        let (map, order) = &mut *answers;
        if map.insert(request_id.to_owned(), (digest, value.clone())).is_none() {
            order.push_back(request_id.to_owned());
            if order.len() > REMEMBERED {
                if let Some(oldest) = order.pop_front() {
                    map.remove(&oldest);
                }
            }
        }
        Ok(value)
    }
}

/// An open stream; dropping it marks the device as gone.
pub(crate) struct Connection {
    bridge: Arc<Bridge>,
    device: String,
}

impl Drop for Connection {
    fn drop(&mut self) {
        let mut connected = lock(&self.bridge.connected);
        if let Some(count) = connected.get_mut(&self.device) {
            *count -= 1;
            if *count == 0 {
                connected.remove(&self.device);
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_repeated_request_gets_the_first_answer_and_a_reused_id_is_refused() {
        let bridge = Bridge::new(|| {});
        let window = bridge.clone();
        let serve = tokio::spawn(async move {
            loop {
                for request in window.take_commands() {
                    request.answer(Ok(serde_json::json!({"started": true})));
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        let stop = Command::Stop { task: "t".into() };
        let first = bridge.call("r1", stop.clone()).await.unwrap();
        assert_eq!(bridge.call("r1", stop).await.unwrap(), first);
        let other = bridge.call("r1", Command::Read { task: "t".into() }).await;
        assert_eq!(other.unwrap_err().status, axum::http::StatusCode::CONFLICT);
        serve.abort();
    }

    #[test]
    fn the_same_state_published_again_is_not_a_change() {
        let bridge = Bridge::new(|| {});
        let mut changes = bridge.subscribe();
        bridge.publish(Live { desktop_focused: true, ..Live::default() });
        assert!(changes.has_changed().unwrap());
        changes.mark_unchanged();
        bridge.publish(Live { desktop_focused: true, ..Live::default() });
        assert!(!changes.has_changed().unwrap());
    }
}
