//! Turns of an external agent driven over the Agent Client Protocol, told to
//! the window as the same events the native harness sends: the transcript,
//! the approval cards and Stop work for them unchanged.

use openide_acp::schema::{
    ConfigOption, ConfigValue, ContentBlock, PermissionOption, PermissionOptionKind, PermissionRequest, PlanEntryStatus, SessionUpdate, StopReason,
    ToolCallContent, ToolCallStatus, ToolKind,
};
use openide_acp::registry::{self, BinaryTarget, Launch, Registry};
use openide_acp::{AgentCommand, Connection, Event, PermissionAnswer};
use openide_core::{
    AgentEvent, AgentEventKind, ModelStreamEvent, OutputStream, PermissionResolution, PlanSnapshot, PlanStep,
    PlanStepStatus, ToolCall,
};
use openide_harness::PermissionChoice;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};

/// A session whose backend starts with this runs that registry agent.
pub(crate) const ACP_BACKEND: &str = "acp:";
const SESSIONS_FILE: &str = "acp-sessions.json";
const PACKAGES_FILE: &str = "acp-packages.json";
/// Tool output kept for the transcript; the agent keeps the whole of it.
const MAX_TOOL_OUTPUT: usize = 64 * 1024;

pub(crate) fn agent_id(backend: &str) -> Option<&str> {
    backend.strip_prefix(ACP_BACKEND)
}

/// A registry agent this machine can run now, and how.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AcpOffer {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) command: AgentCommand,
}

/// How a registry agent can run here, as Settings tells it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AcpSource {
    /// The user's own install, at this path.
    Own(PathBuf),
    /// The vendor's build, downloaded by openADE.
    Downloaded,
    /// Through `npx` or `uvx`, which fetch the package on first use; it is
    /// offered once the user turns it on.
    Package { runner: String, package: String, on: bool },
    /// The vendor's build, still to download into `dir`.
    Download { target: BinaryTarget, dir: PathBuf },
    /// Nothing on this machine runs it, for this reason.
    Missing(&'static str),
}

/// One agent of the registry and how it runs here.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AcpEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) version: String,
    pub(crate) source: AcpSource,
    command: Option<AgentCommand>,
}

impl AcpEntry {
    /// The agent as the menus offer it, when it runs here now.
    pub(crate) fn offer(&self) -> Option<AcpOffer> {
        let command = self.command.clone()?;
        Some(AcpOffer { id: self.id.clone(), name: self.name.clone(), command })
    }
}

/// The registry's agents and how each runs here, with the login shell's
/// `search_path` so an agent's own commands find the user's tools. Those a
/// package runner fetches are offered only when listed in `packages_on`.
pub(crate) fn catalog(registry: &Registry, search_path: Option<&OsStr>, installs: &Path, packages_on: &[String]) -> Vec<AcpEntry> {
    let find = |name: &str| search_path.and_then(|path| registry::find_in(path, name));
    registry
        .agents
        .iter()
        .map(|agent| {
            let (source, command) = match agent.launch(&find, installs) {
                Some(Launch::Local(command)) if command.program.starts_with(installs) => (AcpSource::Downloaded, Some(command)),
                Some(Launch::Local(command)) => (AcpSource::Own(command.program.clone()), Some(command)),
                Some(Launch::Package(command)) => {
                    let runner = command.program.file_name().and_then(OsStr::to_str).unwrap_or("npx").to_owned();
                    let target = if runner == "uvx" { &agent.distribution.uvx } else { &agent.distribution.npx };
                    let package = target.as_ref().map(|target| target.package.clone()).unwrap_or_default();
                    let on = packages_on.contains(&agent.id);
                    (AcpSource::Package { runner, package, on }, on.then_some(command))
                }
                Some(Launch::Binary { target, dir }) => (AcpSource::Download { target, dir }, None),
                None if agent.distribution.npx.is_some() => (AcpSource::Missing("Necesita Node.js (npx) en tu PATH"), None),
                None if agent.distribution.uvx.is_some() => (AcpSource::Missing("Necesita uv (uvx) en tu PATH"), None),
                None => (AcpSource::Missing("No hay una versión para este sistema"), None),
            };
            let command = command.map(|mut command| {
                if let Some(path) = search_path.and_then(OsStr::to_str) {
                    command.env.push(("PATH".into(), path.to_owned()));
                }
                command
            });
            AcpEntry {
                id: agent.id.clone(),
                name: agent.name.clone(),
                description: agent.description.clone(),
                version: agent.version.clone(),
                source,
                command,
            }
        })
        .collect()
}

/// The agents run through `npx` or `uvx` that the user turned on.
pub(crate) fn packages_on(root: &Path) -> Vec<String> {
    std::fs::read(root.join(PACKAGES_FILE)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

pub(crate) fn set_package_on(root: &Path, id: &str, on: bool) -> Result<(), String> {
    let mut ids = packages_on(root);
    ids.retain(|known| known != id);
    if on {
        ids.push(id.to_owned());
    }
    let bytes = serde_json::to_vec_pretty(&ids).map_err(|error| error.to_string())?;
    openide_harness::atomic_write(&root.join(PACKAGES_FILE), &bytes).map_err(|error| error.to_string())
}

/// The session's option of `category` (`model`, `thought_level`): its
/// current value and every value it takes, with its name.
pub(crate) fn choices(config: &[ConfigOption], category: &str) -> Option<(String, Vec<(String, String)>)> {
    let option = config.iter().find(|option| option.category.as_deref() == Some(category))?;
    let ConfigValue::Select { current_value, options } = &option.value else {
        return None;
    };
    Some((current_value.clone(), options.all().into_iter().map(|choice| (choice.value.clone(), choice.name.clone())).collect()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Which ACP session of which agent each openADE session continues, so a
/// restart reopens it with `session/load`.
pub(crate) fn remembered_session(root: &Path, session: &str) -> Option<String> {
    let map: BTreeMap<String, String> =
        std::fs::read(root.join(SESSIONS_FILE)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())?;
    map.get(session).cloned()
}

fn remember_session(root: &Path, session: &str, acp_session: &str) {
    let path = root.join(SESSIONS_FILE);
    let mut map: BTreeMap<String, String> =
        std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    map.insert(session.to_owned(), acp_session.to_owned());
    if let Ok(bytes) = serde_json::to_vec_pretty(&map) {
        let _ = openide_harness::atomic_write(&path, &bytes);
    }
}

/// What a tool call has shown so far.
#[derive(Debug)]
struct Tool {
    name: String,
    arguments: Value,
    /// Output already sent as deltas.
    shown: usize,
    finished: bool,
}

/// The running turn's side of the agent: its events and what they refer to.
struct Turn {
    sender: Sender<AgentEvent>,
    session_id: String,
    sequence: u64,
    /// Agents may reuse a tool call's id in a later turn; the transcript
    /// keys its rows by id across the whole session.
    prefix: String,
    text: String,
    tools: HashMap<String, Tool>,
    tool_order: Vec<String>,
    /// Open permission requests: the agent's request and its choices.
    permissions: HashMap<String, (Value, Vec<PermissionOption>, String)>,
    asked: u64,
}

impl Turn {
    fn emit(&mut self, kind: AgentEventKind) {
        self.sequence += 1;
        let _ = self.sender.send(AgentEvent::new(self.session_id.clone(), self.sequence, kind));
    }

    fn update(&mut self, update: SessionUpdate) {
        match update {
            SessionUpdate::AgentMessageChunk { content } => {
                if let Some(text) = content.text().filter(|text| !text.is_empty()) {
                    self.text.push_str(text);
                    self.emit(AgentEventKind::AssistantDelta { content: text.to_owned() });
                }
            }
            SessionUpdate::AgentThoughtChunk { content } => {
                if let Some(text) = content.text().filter(|text| !text.is_empty()) {
                    self.emit(AgentEventKind::ModelStreamUpdate {
                        step: 1,
                        update: ModelStreamEvent::ReasoningDelta { item_id: "acp-thought".into(), content: text.to_owned() },
                    });
                }
            }
            SessionUpdate::ToolCall(call) => {
                let (name, arguments) = tool_identity(call.kind, &call.title, call.raw_input.as_ref(), call.locations.first().map(|at| at.path.as_str()));
                self.start_tool(&call.tool_call_id, name, arguments);
                self.progress_tool(&call.tool_call_id, &call.content, call.raw_output.as_ref(), call.status);
            }
            SessionUpdate::ToolCallUpdate(update) => {
                if !self.tools.contains_key(&update.tool_call_id) {
                    let title = update.title.as_deref().unwrap_or("Herramienta");
                    let (name, arguments) = tool_identity(
                        update.kind.unwrap_or_default(),
                        title,
                        update.raw_input.as_ref(),
                        update.locations.as_ref().and_then(|at| at.first()).map(|at| at.path.as_str()),
                    );
                    self.start_tool(&update.tool_call_id, name, arguments);
                }
                self.progress_tool(
                    &update.tool_call_id,
                    update.content.as_deref().unwrap_or_default(),
                    update.raw_output.as_ref(),
                    update.status.unwrap_or(ToolCallStatus::InProgress),
                );
            }
            SessionUpdate::Plan { entries } => {
                let plan = PlanSnapshot {
                    explanation: None,
                    plan: entries
                        .into_iter()
                        .take(openide_core::MAX_PLAN_STEPS)
                        .map(|entry| PlanStep {
                            step: clip(entry.content.trim(), openide_core::MAX_PLAN_STEP_BYTES).to_owned(),
                            status: match entry.status {
                                PlanEntryStatus::Pending => PlanStepStatus::Pending,
                                PlanEntryStatus::InProgress => PlanStepStatus::InProgress,
                                PlanEntryStatus::Completed => PlanStepStatus::Completed,
                            },
                        })
                        .filter(|step| !step.step.is_empty())
                        .collect(),
                };
                if plan.is_valid() {
                    self.emit(AgentEventKind::PlanUpdated { tool_call_id: "acp-plan".into(), plan });
                }
            }
            _ => {}
        }
    }

    fn tool_id(&self, id: &str) -> String {
        format!("{}{id}", self.prefix)
    }

    fn start_tool(&mut self, id: &str, name: String, arguments: Value) {
        if self.tools.contains_key(id) {
            return;
        }
        self.tools.insert(id.to_owned(), Tool { name: name.clone(), arguments: arguments.clone(), shown: 0, finished: false });
        self.tool_order.push(id.to_owned());
        let id = self.tool_id(id);
        self.emit(AgentEventKind::ToolStarted { tool_call: ToolCall { id, name, arguments } });
    }

    fn progress_tool(&mut self, id: &str, content: &[ToolCallContent], raw_output: Option<&Value>, status: ToolCallStatus) {
        let mut output = tool_output(content);
        if output.is_empty() {
            if let Some(raw) = raw_output {
                output = raw.as_str().map_or_else(|| raw.to_string(), str::to_owned);
            }
        }
        let output = clip(&output, MAX_TOOL_OUTPUT).to_owned();
        let Some(tool) = self.tools.get_mut(id) else {
            return;
        };
        if tool.finished {
            return;
        }
        // Output the agent replaces rather than extends is shown when it ends.
        let fresh = (output.len() > tool.shown && output.is_char_boundary(tool.shown)).then(|| output[tool.shown..].to_owned());
        let finished = matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed);
        if finished {
            tool.finished = true;
            let tool_call_id = self.tool_id(id);
            self.emit(AgentEventKind::ToolFinished { tool_call_id, output, is_error: status == ToolCallStatus::Failed });
        } else if let Some(text) = fresh {
            tool.shown += text.len();
            let tool_call_id = self.tool_id(id);
            self.emit(AgentEventKind::ToolOutputDelta { tool_call_id, stream: OutputStream::Stdout, text });
        }
    }

    fn ask(&mut self, request: Value, permission: PermissionRequest) {
        let call = &permission.tool_call;
        if !self.tools.contains_key(&call.tool_call_id) {
            let (name, arguments) = tool_identity(
                call.kind.unwrap_or_default(),
                call.title.as_deref().unwrap_or("Herramienta"),
                call.raw_input.as_ref(),
                call.locations.as_ref().and_then(|at| at.first()).map(|at| at.path.as_str()),
            );
            self.start_tool(&call.tool_call_id, name, arguments);
        }
        let tool = &self.tools[&call.tool_call_id];
        let tool_call = ToolCall { id: self.tool_id(&call.tool_call_id), name: tool.name.clone(), arguments: tool.arguments.clone() };
        self.asked += 1;
        let request_id = format!("{}permission-{}", self.prefix, self.asked);
        self.permissions.insert(request_id.clone(), (request, permission.options, tool_call.id.clone()));
        self.emit(AgentEventKind::PermissionRequested { request_id, tool_call });
    }

    /// Ends tools the agent left open, so none keeps spinning after the turn.
    fn close_tools(&mut self, why: &str) {
        let open: Vec<String> = self.tool_order.iter().filter(|id| !self.tools[*id].finished).cloned().collect();
        for id in open {
            if let Some(tool) = self.tools.get_mut(&id) {
                tool.finished = true;
            }
            let tool_call_id = self.tool_id(&id);
            self.emit(AgentEventKind::ToolFinished { tool_call_id, output: why.to_owned(), is_error: true });
        }
    }
}

/// The name and arguments the transcript knows how to show for a call of
/// this kind: a file read, an edit, a command, a search, a fetch.
fn tool_identity(kind: ToolKind, title: &str, raw_input: Option<&Value>, path: Option<&str>) -> (String, Value) {
    let input = raw_input.and_then(Value::as_object);
    let field = |keys: &[&str]| input.and_then(|input| keys.iter().find_map(|key| input.get(*key)?.as_str().map(str::to_owned)));
    let path = path.map(str::to_owned).or_else(|| field(&["path", "file_path", "filePath"]));
    match kind {
        ToolKind::Read => ("read_file".into(), json!({"path": path.unwrap_or_else(|| title.to_owned())})),
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => ("edit_file".into(), json!({"path": path.unwrap_or_else(|| title.to_owned())})),
        ToolKind::Execute => ("run_command".into(), json!({"command": field(&["command", "cmd"]).unwrap_or_else(|| title.to_owned())})),
        ToolKind::Search => ("search_text".into(), json!({"query": field(&["query", "pattern"]).unwrap_or_else(|| title.to_owned())})),
        ToolKind::Fetch => ("web_fetch".into(), json!({"url": field(&["url"]).unwrap_or_else(|| title.to_owned())})),
        ToolKind::Think | ToolKind::SwitchMode | ToolKind::Other => {
            (title.to_owned(), raw_input.cloned().unwrap_or_else(|| json!({})))
        }
    }
}

/// A tool's content as text: its messages, and its diffs line by line.
fn tool_output(content: &[ToolCallContent]) -> String {
    let mut text = String::new();
    for item in content {
        match item {
            ToolCallContent::Content { content: ContentBlock::Text { text: part } } => {
                text.push_str(part);
                text.push('\n');
            }
            ToolCallContent::Diff { path, old_text, new_text } => {
                text.push_str(&format!("--- {path}\n+++ {path}\n"));
                for line in old_text.as_deref().unwrap_or_default().lines() {
                    text.push_str(&format!("-{line}\n"));
                }
                for line in new_text.lines() {
                    text.push_str(&format!("+{line}\n"));
                }
            }
            _ => {}
        }
    }
    text
}

fn clip(text: &str, bytes: usize) -> &str {
    if text.len() <= bytes {
        return text;
    }
    let mut end = bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The option of `options` that answers `choice`, nearest kind first.
fn option_for(options: &[PermissionOption], choice: PermissionChoice) -> Option<String> {
    let order: &[PermissionOptionKind] = match choice {
        PermissionChoice::Allow => &[PermissionOptionKind::AllowOnce, PermissionOptionKind::AllowAlways],
        PermissionChoice::AllowSession => &[PermissionOptionKind::AllowAlways, PermissionOptionKind::AllowOnce],
        PermissionChoice::Deny => &[PermissionOptionKind::RejectOnce, PermissionOptionKind::RejectAlways],
    };
    order.iter().find_map(|kind| options.iter().find(|option| option.kind == *kind)).map(|option| option.option_id.clone())
}

/// One agent process and its ACP session, kept for the next turns.
pub(crate) struct AcpAgent {
    connection: Arc<Connection>,
    acp_session: String,
    turn: Arc<Mutex<Option<Turn>>>,
    alive: Arc<AtomicBool>,
    /// The session's options as the agent last told them.
    config: Arc<Mutex<Vec<ConfigOption>>>,
    pub(crate) name: String,
}

impl AcpAgent {
    /// Starts the agent, reopens `resume` when it can and otherwise opens a
    /// new session, signing in once if the agent asks for it.
    fn start(command: &AgentCommand, name: &str, cwd: &Path, resume: Option<&str>) -> Result<Self, String> {
        let turn: Arc<Mutex<Option<Turn>>> = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let config: Arc<Mutex<Vec<ConfigOption>>> = Arc::default();
        let relay = (turn.clone(), alive.clone(), config.clone());
        let connection = Connection::spawn(command, cwd, move |event| relay_event(event, &relay.0, &relay.1, &relay.2))
            .map_err(|error| error.to_string())?;
        let initialized = connection.initialize("openADE", env!("CARGO_PKG_VERSION")).map_err(|error| error.to_string())?;
        let reopened = resume.filter(|_| initialized.agent_capabilities.load_session).and_then(|session| {
            connection.load_session(session, cwd, json!([])).ok().map(|loaded| (session.to_owned(), loaded.config_options))
        });
        let (acp_session, options) = match reopened {
            Some(reopened) => reopened,
            None => match connection.new_session(cwd, json!([])) {
                Ok(session) => (session.session_id, session.config_options),
                Err(openide_acp::Error::Rpc { message, .. }) if message.to_lowercase().contains("auth") => {
                    let method = initialized
                        .auth_methods
                        .iter()
                        .find(|method| method.kind.as_deref().is_none_or(|kind| kind == "agent"))
                        .ok_or_else(|| format!("{name} pide iniciar sesión: hazlo desde su CLI y vuelve a intentar"))?;
                    connection.authenticate(&method.id).map_err(|error| error.to_string())?;
                    let session = connection.new_session(cwd, json!([])).map_err(|error| error.to_string())?;
                    (session.session_id, session.config_options)
                }
                Err(error) => return Err(error.to_string()),
            },
        };
        if let Some(options) = options {
            *lock(&config) = options;
        }
        Ok(Self { connection: Arc::new(connection), acp_session, turn, alive, config, name: name.to_owned() })
    }

    /// Sets the option of each category to its value, unless it has it
    /// already or the agent does not offer that value.
    fn choose(&self, choices: &[(&str, String)]) -> Result<(), String> {
        for (category, value) in choices {
            let option = lock(&self.config).iter().find(|option| option.category.as_deref() == Some(*category)).cloned();
            let Some(option) = option else {
                continue;
            };
            let ConfigValue::Select { current_value, options } = &option.value else {
                continue;
            };
            if current_value == value || !options.all().iter().any(|choice| choice.value == *value) {
                continue;
            }
            let updated = self
                .connection
                .set_config_option(&self.acp_session, &option.id, value)
                .map_err(|error| format!("{} no aceptó el cambio de {}: {error}", self.name, option.name.to_lowercase()))?;
            if !updated.is_empty() {
                *lock(&self.config) = updated;
            }
        }
        Ok(())
    }
}

/// Passes the agent's messages to the turn that is running; between turns
/// there is nobody to tell, as when a reopened session replays its history.
fn relay_event(event: Event, turn: &Mutex<Option<Turn>>, alive: &AtomicBool, config: &Mutex<Vec<ConfigOption>>) {
    let mut slot = lock(turn);
    match (event, slot.as_mut()) {
        (Event::Update { update: SessionUpdate::ConfigOptionUpdate { config_options }, .. }, _) => *lock(config) = config_options,
        (Event::Update { update, .. }, Some(turn)) => turn.update(update),
        (Event::Permission { request, permission }, Some(turn)) => turn.ask(request, permission),
        (Event::Closed { .. }, _) => alive.store(false, Ordering::Release),
        _ => {}
    }
}

/// A running turn, as the window holds it.
pub(crate) struct AcpRunHandle {
    pub(crate) session_id: String,
    pub(crate) events: Receiver<AgentEvent>,
    slot: Arc<AgentSlot>,
    cancelled: Arc<AtomicBool>,
}

/// Where a session's agent lives between turns: the window keeps one per
/// session, started ahead of the first turn or by it.
#[derive(Default)]
pub(crate) struct AgentSlot {
    agent: Mutex<Option<Arc<AcpAgent>>>,
    /// Held while the agent starts, so a turn and an early start never
    /// start two.
    starting: Mutex<()>,
}

impl AgentSlot {
    pub(crate) fn agent(&self) -> Option<Arc<AcpAgent>> {
        lock(&self.agent).clone().filter(|agent| agent.alive.load(Ordering::Acquire))
    }

    /// The running agent's options, or none before it starts.
    pub(crate) fn config(&self) -> Vec<ConfigOption> {
        self.agent().map(|agent| lock(&agent.config).clone()).unwrap_or_default()
    }

    /// The running agent, started first when there is none.
    pub(crate) fn ensure(&self, start: &AgentStart) -> Result<Arc<AcpAgent>, String> {
        let _starting = lock(&self.starting);
        if let Some(agent) = self.agent() {
            return Ok(agent);
        }
        let resume = remembered_session(&start.data_root, &start.session_id);
        let agent = Arc::new(AcpAgent::start(&start.command, &start.agent_name, &start.workspace, resume.as_deref())?);
        remember_session(&start.data_root, &start.session_id, &agent.acp_session);
        *lock(&self.agent) = Some(agent.clone());
        Ok(agent)
    }
}

/// What starting a session's agent takes.
#[derive(Clone)]
pub(crate) struct AgentStart {
    pub(crate) session_id: String,
    pub(crate) workspace: PathBuf,
    pub(crate) command: AgentCommand,
    pub(crate) agent_name: String,
    pub(crate) data_root: PathBuf,
}

pub(crate) struct TurnRequest {
    pub(crate) start: AgentStart,
    pub(crate) sequence_start: u64,
    pub(crate) prompt: String,
    /// The value wanted for the option of each category, applied first.
    pub(crate) choices: Vec<(&'static str, String)>,
}

impl AcpRunHandle {
    pub(crate) fn start(request: TurnRequest, slot: Arc<AgentSlot>) -> Self {
        let (sender, events) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let handle = Self { session_id: request.start.session_id.clone(), events, slot: slot.clone(), cancelled: cancelled.clone() };
        std::thread::Builder::new()
            .name("acp-turn".into())
            .spawn(move || run_turn(request, &slot, sender, &cancelled))
            .expect("the ACP turn thread starts");
        handle
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        let Some(agent) = self.slot.agent() else {
            return;
        };
        // The protocol asks for every open request to be answered as cancelled.
        let mut turn = lock(&agent.turn);
        if let Some(turn) = turn.as_mut() {
            for (request_id, (request, _, tool_call_id)) in std::mem::take(&mut turn.permissions) {
                let _ = agent.connection.answer_permission(&request, &PermissionAnswer::Cancelled);
                turn.emit(AgentEventKind::PermissionResolved { request_id, tool_call_id, resolution: PermissionResolution::Cancelled });
            }
        }
        drop(turn);
        let _ = agent.connection.cancel(&agent.acp_session);
    }

    pub(crate) fn resolve_permission(&self, request_id: &str, choice: PermissionChoice) -> bool {
        let Some(agent) = self.slot.agent() else {
            return false;
        };
        let mut turn = lock(&agent.turn);
        let Some(turn) = turn.as_mut() else {
            return false;
        };
        let Some((request, options, tool_call_id)) = turn.permissions.remove(request_id) else {
            return false;
        };
        let answer = option_for(&options, choice).map_or(PermissionAnswer::Cancelled, PermissionAnswer::Selected);
        let resolution = match (&answer, choice) {
            (PermissionAnswer::Selected(_), PermissionChoice::Allow | PermissionChoice::AllowSession) => PermissionResolution::Allowed,
            _ => PermissionResolution::Denied,
        };
        let sent = agent.connection.answer_permission(&request, &answer).is_ok();
        turn.emit(AgentEventKind::PermissionResolved { request_id: request_id.to_owned(), tool_call_id, resolution });
        sent
    }
}

fn run_turn(request: TurnRequest, slot: &AgentSlot, sender: Sender<AgentEvent>, cancelled: &AtomicBool) {
    let mut opening = Turn {
        sender,
        session_id: request.start.session_id.clone(),
        sequence: request.sequence_start,
        prefix: format!("acp-{}-", request.sequence_start),
        text: String::new(),
        tools: HashMap::new(),
        tool_order: Vec::new(),
        permissions: HashMap::new(),
        asked: 0,
    };
    opening.emit(AgentEventKind::SessionStarted { workspace: request.start.workspace.clone() });
    opening.emit(AgentEventKind::TurnStarted { prompt: request.prompt.clone() });
    let agent = match slot.ensure(&request.start).and_then(|agent| agent.choose(&request.choices).map(|()| agent)) {
        Ok(agent) => agent,
        Err(error) => {
            opening.emit(AgentEventKind::TurnFailed { error });
            return;
        }
    };
    if cancelled.load(Ordering::Acquire) {
        opening.emit(AgentEventKind::TurnCancelled);
        return;
    }
    opening.emit(AgentEventKind::ModelRequestStarted { step: 1, model: agent.name.clone() });
    *lock(&agent.turn) = Some(opening);
    let stopped = agent.connection.prompt(&agent.acp_session, &[ContentBlock::Text { text: request.prompt }]);
    // Taken out before the last event: once it is dropped the window sees
    // the channel close, and the agent's late messages reach no one.
    let Some(mut turn) = lock(&agent.turn).take() else {
        return;
    };
    match stopped {
        Ok(StopReason::Cancelled) => {
            turn.close_tools("Interrumpido");
            turn.emit(AgentEventKind::TurnCancelled);
        }
        Ok(StopReason::Refusal) => {
            turn.close_tools("Interrumpido");
            turn.emit(AgentEventKind::TurnFailed { error: format!("{} se negó a continuar", agent.name) });
        }
        Ok(reason) => {
            turn.close_tools("Sin resultado");
            let response = std::mem::take(&mut turn.text);
            if matches!(reason, StopReason::MaxTokens | StopReason::MaxTurnRequests) {
                turn.emit(AgentEventKind::HostNotice { code: openide_core::NOTICE_OUTPUT_LIMIT.into(), detail: agent.name.clone() });
            }
            turn.emit(AgentEventKind::TurnCompleted { response, steps: 1 });
        }
        Err(error) => {
            turn.close_tools("Interrumpido");
            let detail = agent.connection.stderr_tail();
            let error = if detail.is_empty() { error.to_string() } else { format!("{error}\n{detail}") };
            turn.emit(AgentEventKind::TurnFailed { error });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_acp::schema::{PermissionOption, ToolCallUpdate};

    fn turn() -> (Turn, Receiver<AgentEvent>) {
        let (sender, events) = mpsc::channel();
        let turn = Turn {
            sender,
            session_id: "s".into(),
            sequence: 10,
            prefix: String::new(),
            text: String::new(),
            tools: HashMap::new(),
            tool_order: Vec::new(),
            permissions: HashMap::new(),
            asked: 0,
        };
        (turn, events)
    }

    fn update(value: Value) -> SessionUpdate {
        serde_json::from_value(value).unwrap()
    }

    fn kinds(events: &Receiver<AgentEvent>) -> Vec<AgentEventKind> {
        events.try_iter().map(|event| event.kind).collect()
    }

    #[test]
    fn text_thoughts_and_plans_become_the_native_events() {
        let (mut turn, events) = turn();
        turn.update(update(json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "pienso"}})));
        turn.update(update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hola"}})));
        turn.update(update(json!({"sessionUpdate": "plan", "entries": [
            {"content": "Leer", "priority": "high", "status": "completed"}, {"content": "Editar", "priority": "low", "status": "in_progress"}]})));
        let kinds = kinds(&events);
        assert!(matches!(&kinds[0], AgentEventKind::ModelStreamUpdate { update: ModelStreamEvent::ReasoningDelta { content, .. }, .. } if content == "pienso"));
        assert_eq!(kinds[1], AgentEventKind::AssistantDelta { content: "Hola".into() });
        let AgentEventKind::PlanUpdated { plan, .. } = &kinds[2] else {
            panic!("a plan");
        };
        assert_eq!(plan.plan[1], PlanStep { step: "Editar".into(), status: PlanStepStatus::InProgress });
        assert_eq!(turn.text, "Hola");
    }

    #[test]
    fn a_tool_starts_streams_and_finishes_once_under_a_known_name() {
        let (mut turn, events) = turn();
        turn.update(update(json!({"sessionUpdate": "tool_call", "toolCallId": "t", "title": "cargo test", "kind": "execute",
            "status": "in_progress", "rawInput": {"command": "cargo test"}})));
        turn.update(update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "t",
            "content": [{"type": "content", "content": {"type": "text", "text": "running"}}]})));
        turn.update(update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "t", "status": "completed",
            "content": [{"type": "content", "content": {"type": "text", "text": "running 3 tests ok"}}]})));
        turn.update(update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "t", "status": "completed"})));
        let kinds = kinds(&events);
        assert_eq!(kinds.len(), 3, "{kinds:?}");
        let AgentEventKind::ToolStarted { tool_call } = &kinds[0] else {
            panic!("a start");
        };
        assert_eq!((tool_call.name.as_str(), &tool_call.arguments), ("run_command", &json!({"command": "cargo test"})));
        assert!(matches!(&kinds[1], AgentEventKind::ToolOutputDelta { text, .. } if text == "running\n"));
        assert!(matches!(&kinds[2], AgentEventKind::ToolFinished { output, is_error: false, .. } if output == "running 3 tests ok\n"));
    }

    #[test]
    fn an_edit_shows_its_diff_and_open_tools_close_with_the_turn() {
        let (mut turn, events) = turn();
        turn.update(update(json!({"sessionUpdate": "tool_call", "toolCallId": "e", "title": "Edit", "kind": "edit", "status": "completed",
            "locations": [{"path": "/w/a.rs"}], "content": [{"type": "diff", "path": "/w/a.rs", "oldText": "x", "newText": "y"}]})));
        turn.update(update(json!({"sessionUpdate": "tool_call", "toolCallId": "r", "title": "Read", "kind": "read", "status": "pending"})));
        turn.close_tools("Interrumpido");
        let kinds = kinds(&events);
        assert!(matches!(&kinds[0], AgentEventKind::ToolStarted { tool_call } if tool_call.name == "edit_file" && tool_call.arguments["path"] == "/w/a.rs"));
        assert!(matches!(&kinds[1], AgentEventKind::ToolFinished { output, .. } if output == "--- /w/a.rs\n+++ /w/a.rs\n-x\n+y\n"));
        assert!(matches!(&kinds[3], AgentEventKind::ToolFinished { tool_call_id, is_error: true, .. } if tool_call_id == "r"));
        assert_eq!(kinds.len(), 4);
    }

    #[test]
    fn a_permission_request_is_shown_on_its_tool_and_choices_map_to_the_agents_options() {
        let (mut turn, events) = turn();
        let options = vec![
            PermissionOption { option_id: "once".into(), name: "Sí".into(), kind: PermissionOptionKind::AllowOnce },
            PermissionOption { option_id: "always".into(), name: "Siempre".into(), kind: PermissionOptionKind::AllowAlways },
            PermissionOption { option_id: "no".into(), name: "No".into(), kind: PermissionOptionKind::RejectOnce },
        ];
        turn.ask(
            json!("req-1"),
            PermissionRequest {
                session_id: "x".into(),
                tool_call: ToolCallUpdate { tool_call_id: "t".into(), title: Some("rm -rf build".into()), kind: Some(ToolKind::Execute), ..Default::default() },
                options: options.clone(),
            },
        );
        let kinds = kinds(&events);
        assert!(matches!(&kinds[0], AgentEventKind::ToolStarted { tool_call } if tool_call.name == "run_command"));
        assert!(matches!(&kinds[1], AgentEventKind::PermissionRequested { request_id, tool_call } if request_id == "permission-1" && tool_call.id == "t"));
        assert_eq!(turn.permissions["permission-1"].0, json!("req-1"));
        assert_eq!(option_for(&options, PermissionChoice::Allow).as_deref(), Some("once"));
        assert_eq!(option_for(&options, PermissionChoice::AllowSession).as_deref(), Some("always"));
        assert_eq!(option_for(&options, PermissionChoice::Deny).as_deref(), Some("no"));
        assert_eq!(option_for(&options[..2], PermissionChoice::Deny), None);
    }

    #[test]
    fn the_catalog_offers_what_runs_here_and_packages_only_once_turned_on() {
        let bin = tempfile::tempdir().unwrap();
        for name in ["grok", "npx"] {
            std::fs::write(bin.path().join(name), "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(bin.path().join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let installs = tempfile::tempdir().unwrap();
        let registry: Registry = serde_json::from_str(include_str!("../../../crates/openide-acp/testdata/registry.json")).unwrap();
        let path = Some(bin.path().as_os_str());
        let found = catalog(&registry, path, installs.path(), &[]);
        let entry = |catalog: &[AcpEntry], id: &str| catalog.iter().find(|entry| entry.id == id).cloned().unwrap();
        let offers: Vec<_> = found.iter().filter_map(AcpEntry::offer).collect();
        assert_eq!(offers.len(), 1, "{offers:?}");
        assert_eq!((offers[0].id.as_str(), offers[0].name.as_str()), ("grok-build", "Grok Build"));
        assert_eq!(offers[0].command.args, ["agent", "stdio"]);
        assert!(offers[0].command.env.contains(&("PATH".into(), bin.path().to_str().unwrap().to_owned())));
        assert_eq!(entry(&found, "grok-build").source, AcpSource::Own(bin.path().join("grok")));
        assert!(matches!(&entry(&found, "gemini").source, AcpSource::Package { runner, on: false, .. } if runner == "npx"));
        assert!(matches!(entry(&found, "fast-agent").source, AcpSource::Missing(_)));
        let turned_on = catalog(&registry, path, installs.path(), &["gemini".into()]);
        assert!(turned_on.iter().filter_map(AcpEntry::offer).any(|offer| offer.id == "gemini"));
        assert!(catalog(&registry, None, installs.path(), &[]).iter().all(|entry| entry.offer().is_none()));
        if registry::platform() == "linux-x86_64" {
            let AcpSource::Download { target, dir } = entry(&found, "antigravity-acp").source else {
                panic!("Google's ACP server to download");
            };
            let program = dir.join(&target.cmd);
            std::fs::create_dir_all(program.parent().unwrap()).unwrap();
            std::fs::write(&program, "").unwrap();
            let installed = catalog(&registry, path, installs.path(), &[]);
            assert_eq!(entry(&installed, "antigravity-acp").source, AcpSource::Downloaded);
            assert!(entry(&installed, "antigravity-acp").offer().is_some());
        }
    }

    #[test]
    fn an_agents_options_are_read_by_category() {
        let config: Vec<ConfigOption> = serde_json::from_value(json!([
            {"id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "a", "options": [{"value": "a", "name": "A"}]},
            {"id": "reasoning_effort", "name": "Effort", "category": "thought_level", "type": "select", "currentValue": "high",
             "options": [{"value": "low", "name": "Low"}, {"value": "high", "name": "High"}]}
        ]))
        .unwrap();
        assert_eq!(choices(&config, "thought_level"), Some(("high".into(), vec![("low".into(), "Low".into()), ("high".into(), "High".into())])));
        assert_eq!(choices(&config, "mode"), None);
    }

    #[test]
    fn sessions_are_remembered_for_the_next_start() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(remembered_session(root.path(), "a"), None);
        remember_session(root.path(), "a", "acp-1");
        remember_session(root.path(), "b", "acp-2");
        assert_eq!(remembered_session(root.path(), "a").as_deref(), Some("acp-1"));
        assert_eq!(agent_id("acp:grok-build"), Some("grok-build"));
        set_package_on(root.path(), "gemini", true).unwrap();
        set_package_on(root.path(), "cline", true).unwrap();
        set_package_on(root.path(), "gemini", false).unwrap();
        assert_eq!(packages_on(root.path()), ["cline"]);
        assert_eq!(agent_id("native"), None);
    }
}
