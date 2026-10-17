//! One running agent: JSON-RPC 2.0 over its stdio, one message per line. A
//! thread reads what the agent writes and routes it: answers go to whoever
//! asked, session updates and permission requests to the handler, called
//! on that thread in the order they came. So when a turn's answer arrives,
//! every update the agent sent before it has been handled.

use crate::schema::{
    ContentBlock, InitializeResponse, LoadSessionResponse, NewSessionResponse, PermissionRequest,
    PromptResponse, SessionNotification, SessionUpdate, StopReason, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Lines of the agent's stderr kept to explain a failure.
const STDERR_LINES: usize = 40;

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The agent is gone: it exited, or closed its output.
    Closed(String),
    /// The agent answered with a JSON-RPC error.
    Rpc { code: i64, message: String },
    /// No answer within the time given.
    Timeout(&'static str),
    /// The answer did not have the expected shape.
    Invalid(String),
    Spawn(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed(why) => write!(formatter, "el agente terminó: {why}"),
            Self::Rpc { message, .. } => write!(formatter, "{message}"),
            Self::Timeout(what) => write!(formatter, "el agente no respondió a {what} a tiempo"),
            Self::Invalid(why) => write!(formatter, "respuesta inesperada del agente: {why}"),
            Self::Spawn(why) => write!(formatter, "no se pudo iniciar el agente: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// How to run an agent.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// What the agent tells the client unasked.
#[derive(Debug)]
pub enum Event {
    Update { session_id: String, update: SessionUpdate },
    /// Answer with [`Connection::answer_permission`] and this `request`.
    Permission { request: Value, permission: PermissionRequest },
    /// Its output closed; `stderr` is the end of what it printed there.
    Closed { stderr: String },
}

/// The user's answer to a permission request.
#[derive(Clone, Debug, PartialEq)]
pub enum PermissionAnswer {
    Selected(String),
    /// The turn was cancelled before the user answered.
    Cancelled,
}

type Pending = Arc<Mutex<HashMap<u64, Sender<Result<Value, Error>>>>>;
/// Shared with the reader, which refuses the requests the client does not serve.
type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct Connection {
    writer: Writer,
    pending: Pending,
    next_id: AtomicU64,
    stderr: Arc<Mutex<VecDeque<String>>>,
    child: Option<Mutex<Child>>,
}

impl Connection {
    /// Starts the agent in `cwd`; `on_event` hears what it says unasked.
    pub fn spawn(command: &AgentCommand, cwd: &Path, on_event: impl FnMut(Event) + Send + 'static) -> Result<Self, Error> {
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .envs(command.env.iter().map(|(key, value)| (key, value)))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| Error::Spawn(format!("{}: {error}", command.program.display())))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let mut connection = Self::over(stdout, stdin, on_event);
        let tail = connection.stderr.clone();
        std::thread::Builder::new()
            .name("acp-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let mut tail = tail.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if tail.len() == STDERR_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            })
            .map_err(|error| Error::Spawn(error.to_string()))?;
        connection.child = Some(Mutex::new(child));
        Ok(connection)
    }

    /// A connection over any pair of streams: the agent's output and input.
    pub fn over(output: impl Read + Send + 'static, input: impl Write + Send + 'static, mut on_event: impl FnMut(Event) + Send + 'static) -> Self {
        let connection = Self {
            writer: Arc::new(Mutex::new(Box::new(input))),
            pending: Arc::default(),
            next_id: AtomicU64::new(1),
            stderr: Arc::default(),
            child: None,
        };
        let pending = connection.pending.clone();
        let stderr = connection.stderr.clone();
        let writer = connection.writer.clone();
        std::thread::Builder::new()
            .name("acp-read".into())
            .spawn(move || read(output, &pending, &mut on_event, &stderr, &writer))
            .expect("the ACP reader thread starts");
        connection
    }

    fn write(&self, message: &Value) -> Result<(), Error> {
        write(&self.writer, message)
    }

    fn request(&self, method: &str, params: Value, timeout: Option<Duration>) -> Result<Value, Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(id, sender);
        if let Err(error) = self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})) {
            self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&id);
            return Err(error);
        }
        let answer = match timeout {
            Some(limit) => receiver.recv_timeout(limit).map_err(|_| Error::Timeout(method_name(method))),
            None => receiver.recv().map_err(|_| Error::Closed(self.stderr_tail())),
        };
        self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&id);
        answer?
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), Error> {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// The end of what the agent printed on stderr.
    pub fn stderr_tail(&self) -> String {
        let tail = self.stderr.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// The handshake. The client serves no files and no terminals: the agent
    /// uses its own tools for both and asks permission for what it does.
    pub fn initialize(&self, client: &str, version: &str) -> Result<InitializeResponse, Error> {
        let answer = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
                "clientInfo": {"name": client, "version": version},
            }),
            Some(Duration::from_secs(60)),
        )?;
        parse(answer)
    }

    pub fn authenticate(&self, method_id: &str) -> Result<(), Error> {
        self.request("authenticate", json!({"methodId": method_id}), Some(Duration::from_secs(300)))
            .map(drop)
    }

    pub fn new_session(&self, cwd: &Path, mcp_servers: Value) -> Result<NewSessionResponse, Error> {
        let answer = self.request(
            "session/new",
            json!({"cwd": cwd, "mcpServers": mcp_servers}),
            Some(Duration::from_secs(120)),
        )?;
        parse(answer)
    }

    /// Reopens an earlier session; the agent replays it as updates first.
    pub fn load_session(&self, session_id: &str, cwd: &Path, mcp_servers: Value) -> Result<LoadSessionResponse, Error> {
        let answer = self.request(
            "session/load",
            json!({"sessionId": session_id, "cwd": cwd, "mcpServers": mcp_servers}),
            Some(Duration::from_secs(120)),
        )?;
        parse(answer)
    }

    /// Runs one turn. It returns when the agent ends it; updates arrive on
    /// the event channel meanwhile. Call it off the UI thread.
    pub fn prompt(&self, session_id: &str, prompt: &[ContentBlock]) -> Result<StopReason, Error> {
        let answer = self.request("session/prompt", json!({"sessionId": session_id, "prompt": prompt}), None)?;
        parse::<PromptResponse>(answer).map(|response| response.stop_reason)
    }

    /// Asks the agent to end the turn; `prompt` then returns `Cancelled`.
    pub fn cancel(&self, session_id: &str) -> Result<(), Error> {
        self.notify("session/cancel", json!({"sessionId": session_id}))
    }

    pub fn set_mode(&self, session_id: &str, mode_id: &str) -> Result<(), Error> {
        self.request("session/set_mode", json!({"sessionId": session_id, "modeId": mode_id}), Some(Duration::from_secs(30)))
            .map(drop)
    }

    pub fn set_config_option(&self, session_id: &str, config_id: &str, value: &str) -> Result<(), Error> {
        self.request(
            "session/set_config_option",
            json!({"sessionId": session_id, "configId": config_id, "value": value}),
            Some(Duration::from_secs(30)),
        )
        .map(drop)
    }

    pub fn answer_permission(&self, request: &Value, answer: &PermissionAnswer) -> Result<(), Error> {
        let outcome = match answer {
            PermissionAnswer::Selected(option_id) => json!({"outcome": "selected", "optionId": option_id}),
            PermissionAnswer::Cancelled => json!({"outcome": "cancelled"}),
        };
        self.write(&json!({"jsonrpc": "2.0", "id": request, "result": {"outcome": outcome}}))
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // An agent outliving its window would keep working unseen.
        if let Some(child) = &self.child {
            let mut child = child.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn method_name(method: &str) -> &'static str {
    match method {
        "initialize" => "initialize",
        "authenticate" => "authenticate",
        "session/new" => "session/new",
        "session/load" => "session/load",
        "session/set_mode" => "session/set_mode",
        "session/set_config_option" => "session/set_config_option",
        _ => "un pedido",
    }
}

fn write(writer: &Writer, message: &Value) -> Result<(), Error> {
    let mut line = serde_json::to_vec(message).map_err(|error| Error::Invalid(error.to_string()))?;
    line.push(b'\n');
    let mut writer = writer.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    writer.write_all(&line).and_then(|()| writer.flush()).map_err(|error| Error::Closed(error.to_string()))
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|error| Error::Invalid(error.to_string()))
}

fn read(
    output: impl Read,
    pending: &Pending,
    on_event: &mut dyn FnMut(Event),
    stderr: &Arc<Mutex<VecDeque<String>>>,
    writer: &Writer,
) {
    let reply = |message: Value| {
        let _ = write(writer, &message);
    };
    for line in BufReader::new(output).lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            // Some agents print logs on stdout; they are not messages.
            continue;
        };
        let id = message.get("id").cloned();
        match (message.get("method").and_then(Value::as_str), id) {
            (None, Some(id)) => {
                let Some(id) = id.as_u64() else {
                    continue;
                };
                let answer = match message.get("error") {
                    Some(error) => Err(Error::Rpc {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: error.get("message").and_then(Value::as_str).unwrap_or("error").to_owned(),
                    }),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                if let Some(waiter) = pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&id) {
                    let _ = waiter.send(answer);
                }
            }
            (Some("session/update"), None) => {
                if let Ok(notification) = serde_json::from_value::<SessionNotification>(message["params"].clone()) {
                    on_event(Event::Update { session_id: notification.session_id, update: notification.update });
                }
            }
            (Some("session/request_permission"), Some(id)) => {
                match serde_json::from_value::<PermissionRequest>(message["params"].clone()) {
                    Ok(permission) => {
                        on_event(Event::Permission { request: id, permission });
                    }
                    Err(error) => reply(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": error.to_string()}})),
                }
            }
            // The client offered no files, terminals or other services.
            (Some(_), Some(id)) => reply(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}})),
            _ => {}
        }
    }
    let waiting: Vec<_> = pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).drain().collect();
    let tail = stderr.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).iter().cloned().collect::<Vec<_>>().join("\n");
    for (_, waiter) in waiting {
        let _ = waiter.send(Err(Error::Closed(tail.clone())));
    }
    on_event(Event::Closed { stderr: tail });
}
