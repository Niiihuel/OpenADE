//! openADE Remote in the window: the server's settings and life, what the
//! phone asks for, and what the phone is told about tasks.
//!
//! A command acts on its task the way a person at the window would: the
//! task is opened, the same control is used, and what was open before is
//! opened again. A turn that keeps running goes on off screen as any other.

use super::*;
use openide_remote::bridge::{Agent, Bridge, Command, Decision, Delivery, Live, Notice, Request, TaskRow};
use openide_remote::pairing::PairingInvitation;
use openide_remote::server::{Options, Server};
use base64::Engine as _;
use sha2::Digest as _;
use std::sync::Arc;

const SETTINGS_FILE: &str = "remote-settings.json";
pub(crate) const DEFAULT_PORT: u16 = 7787;
/// How often what the phone sees is rebuilt while nothing asks for it.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct RemoteSettings {
    pub enabled: bool,
    pub port: u16,
    /// A name or address for a port the person opened on their router.
    pub public_address: String,
    /// Ask the router to open the port (UPnP). Off unless asked: it puts the
    /// server on the internet.
    pub upnp: bool,
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self { enabled: false, port: DEFAULT_PORT, public_address: String::new(), upnp: false }
    }
}

impl RemoteSettings {
    pub(crate) fn load(root: &Path) -> Self {
        std::fs::read(root.join(SETTINGS_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, root: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        std::fs::write(root.join(SETTINGS_FILE), bytes).map_err(|error| format!("No se pudo guardar el remoto: {error}"))
    }
}

pub(crate) struct Remote {
    pub(crate) settings: RemoteSettings,
    bridge: Arc<Bridge>,
    /// Where the bridge's wakes go once the window's loop gives its waker.
    waker: Arc<std::sync::Mutex<Option<crate::background::Waker>>>,
    pub(crate) server: Option<Server>,
    pub(crate) error: Option<String>,
    /// The code on screen for a phone to scan, drawn.
    pub(crate) invitation: Option<(PairingInvitation, openide_ui::RasterImage)>,
    published_at: Option<Instant>,
    /// When Settings → Remoto was last read.
    viewed_at: Option<Instant>,
}

impl Remote {
    pub(crate) fn new(root: &Path) -> Self {
        let waker: Arc<std::sync::Mutex<Option<crate::background::Waker>>> = Arc::default();
        let wakes = waker.clone();
        Self {
            settings: RemoteSettings::load(root),
            bridge: Bridge::new(move || {
                if let Some(waker) = wakes.lock().unwrap_or_else(|error| error.into_inner()).as_ref() {
                    waker.wake();
                }
            }),
            waker,
            server: None,
            error: None,
            invitation: None,
            published_at: None,
            viewed_at: None,
        }
    }

    /// Raises a notice for the phone; the server leaves it out while the
    /// person is at the PC.
    pub(crate) fn notify(&self, event: crate::notifications::Event, title: &str, task: &str) {
        let notice = match event {
            crate::notifications::Event::TurnCompleted => Notice::Completed,
            crate::notifications::Event::TurnFailed => Notice::Failed,
            crate::notifications::Event::Approval if title == "Esperando tu respuesta" => Notice::Question,
            crate::notifications::Event::Approval => Notice::Approval,
            crate::notifications::Event::PullRequest => return,
        };
        if self.server.is_some() {
            self.bridge.notify(notice, task);
        }
    }

    pub(crate) fn connected_devices(&self) -> BTreeMap<String, usize> {
        self.bridge.connected_devices()
    }
}

/// The code as a PNG, eight pixels a module inside the white border a
/// scanner needs: drawn as rectangles it would get seams between rows.
pub(super) fn qr_image(width: usize, modules: &[bool]) -> Option<openide_ui::RasterImage> {
    const SCALE: usize = 8;
    const QUIET: usize = 4;
    let side = (width + 2 * QUIET) * SCALE;
    let image = image::GrayImage::from_fn(side as u32, side as u32, |x, y| {
        let (column, row) = (x as usize / SCALE, y as usize / SCALE);
        let dark = (QUIET..QUIET + width).contains(&column)
            && (QUIET..QUIET + width).contains(&row)
            && modules.get((row - QUIET) * width + column - QUIET).copied().unwrap_or(false);
        image::Luma([if dark { 0 } else { 255 }])
    });
    let mut bytes = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).ok()?;
    openide_ui::RasterImage::new(
        format!("{:x}", sha2::Sha256::digest(&bytes)),
        openide_ui::RasterImageFormat::Png,
        bytes.len() as u32,
        Arc::from(base64::engine::general_purpose::STANDARD.encode(&bytes)),
        "Código para emparejar el iPhone",
    )
}

fn host_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "openADE".to_owned())
}

impl AgentsWindow {
    /// Starts or stops the server to match the settings.
    pub(super) fn apply_remote_settings(&mut self) {
        self.remote.server = None;
        self.remote.invitation = None;
        self.remote.error = None;
        if !self.remote.settings.enabled {
            return;
        }
        let public_address = Some(self.remote.settings.public_address.clone()).filter(|value| !value.trim().is_empty());
        let options = Options {
            root: self.data_root.join("remote"),
            sessions: self.data_root.join("sessions"),
            port: self.remote.settings.port,
            name: host_name(),
            public_address,
            upnp: self.remote.settings.upnp,
        };
        match Server::start(options, self.remote.bridge.clone()) {
            Ok(server) => self.remote.server = Some(server),
            Err(error) => self.remote.error = Some(format!("No se pudo iniciar el remoto: {error:#}")),
        }
    }

    pub(super) fn set_remote_settings(&mut self, settings: RemoteSettings) {
        if let Err(error) = settings.save(&self.data_root) {
            self.remote.error = Some(error);
            return;
        }
        self.remote.settings = settings;
        self.apply_remote_settings();
    }

    /// A new code for a phone to pair with.
    pub(super) fn start_remote_pairing(&mut self) {
        let Some(server) = &self.remote.server else { return };
        let drawn = server.invite(&host_name()).map_err(|error| format!("{error:#}")).and_then(|invitation| {
            let (width, modules) = invitation.qr_modules().map_err(|error| format!("{error:#}"))?;
            let image = qr_image(width, &modules).ok_or("no se pudo dibujar")?;
            Ok((invitation, image))
        });
        match drawn {
            Ok(invitation) => self.remote.invitation = Some(invitation),
            Err(error) => self.remote.error = Some(format!("No se pudo crear el código: {error}")),
        }
    }

    /// Settings → Remoto, read again every second while it is shown.
    pub(super) fn poll_remote_view(&mut self) -> bool {
        let shown = self.view.show_settings && self.view.settings_page == crate::settings_ui::SettingsPage::Remote;
        if !shown {
            return false;
        }
        if self.remote.viewed_at.is_some_and(|at| at.elapsed() < Duration::from_secs(1)) {
            return false;
        }
        let first = self.remote.viewed_at.is_none();
        self.remote.viewed_at = Some(Instant::now());
        let settings = &self.remote.settings;
        let connected = self.remote.connected_devices();
        let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
        let mut view = crate::view::RemoteView {
            enabled: settings.enabled,
            port: if first { settings.port.to_string() } else { self.view.remote.port.clone() },
            public_address: if first { settings.public_address.clone() } else { self.view.remote.public_address.clone() },
            error: self.remote.error.clone(),
            ..Default::default()
        };
        if let Some(server) = &self.remote.server {
            view.listening = Some(server.port());
            view.fingerprint = openide_remote::tls::short_fingerprint(server.fingerprint());
            view.routes = server.routes();
            view.push = server.push_enabled();
            view.upnp = self.remote.settings.upnp.then(|| server.upnp_state().describe());
            view.devices = server
                .devices()
                .unwrap_or_default()
                .into_iter()
                .map(|device| crate::view::RemoteDeviceView {
                    connected: connected.contains_key(&device.id),
                    seen: if connected.contains_key(&device.id) {
                        "Conectado ahora".to_owned()
                    } else {
                        format!("Visto {}", crate::provider_usage_ui::elapsed_label(device.last_seen_ms, now_ms))
                    },
                    id: device.id,
                    name: device.name,
                })
                .collect();
        }
        let now_s = now_ms / 1000;
        if self.remote.invitation.as_ref().is_some_and(|(invitation, _)| invitation.expires_at <= now_s) {
            self.remote.invitation = None;
        }
        view.qr = self.remote.invitation.as_ref().map(|(invitation, image)| (image.clone(), invitation.expires_at));
        let changed = view != self.view.remote;
        self.view.remote = view;
        changed
    }

    /// The page's own controls; `false` for any other action.
    pub(super) fn apply_remote_action(&mut self, action: &UiAction) -> bool {
        match action {
            UiAction::Clicked { id } if id == "remote_enabled" => {
                let mut settings = self.remote.settings.clone();
                settings.enabled = !settings.enabled;
                self.set_remote_settings(settings);
            }
            UiAction::InputChanged { id, value } if id == "remote_port" => self.view.remote.port = value.clone(),
            UiAction::InputChanged { id, value } if id == "remote_public" => {
                self.view.remote.public_address = value.clone();
            }
            UiAction::Submitted { id, .. } if id == "remote_port" || id == "remote_public" => {
                let mut settings = self.remote.settings.clone();
                match self.view.remote.port.trim().parse::<u16>() {
                    Ok(port) if port >= 1024 => settings.port = port,
                    _ => {
                        self.remote.error = Some("El puerto debe ser un número entre 1024 y 65535".into());
                        self.remote.viewed_at = None;
                        return true;
                    }
                }
                settings.public_address = self.view.remote.public_address.trim().to_owned();
                if settings != self.remote.settings {
                    self.set_remote_settings(settings);
                }
            }
            UiAction::Clicked { id } if id == "remote_upnp" => {
                let mut settings = self.remote.settings.clone();
                settings.upnp = !settings.upnp;
                self.set_remote_settings(settings);
            }
            UiAction::Clicked { id } if id == "remote_pair" => self.start_remote_pairing(),
            UiAction::Clicked { id } if id == "remote_pair_close" => self.remote.invitation = None,
            UiAction::Clicked { id } if id.starts_with("remote_revoke:") => {
                if let Some(server) = &self.remote.server {
                    if let Err(error) = server.revoke(&id["remote_revoke:".len()..]) {
                        self.remote.error = Some(format!("No se pudo quitar el acceso: {error:#}"));
                    }
                }
            }
            _ => return false,
        }
        self.remote.viewed_at = None;
        true
    }

    /// Serves what the phone asked for and tells it what changed. Called
    /// from every frame and every background tick.
    pub(super) fn serve_remote(&mut self) -> bool {
        if self.remote.server.is_none() {
            return false;
        }
        {
            let mut waker = self.remote.waker.lock().unwrap_or_else(|error| error.into_inner());
            if waker.is_none() {
                *waker = self.background.waker.clone();
            }
        }
        let requests = self.remote.bridge.take_commands();
        let served = !requests.is_empty();
        for request in requests {
            self.serve_remote_request(request);
        }
        let drained = self.drain_off_screen_queues();
        let due = self.remote.published_at.is_none_or(|at| at.elapsed() >= PUBLISH_INTERVAL);
        if served || drained || due {
            self.remote.bridge.publish(self.remote_live());
            self.remote.published_at = Some(Instant::now());
        }
        served || drained
    }

    fn serve_remote_request(&mut self, request: Request) {
        let result = match request.command.clone() {
            Command::CreateTask { project, backend, prompt, files, plan } => {
                self.remote_create_task(&project, &backend, &prompt, &files, plan)
            }
            Command::Send { task, prompt, files, delivery } => self.remote_send(&task, &prompt, &files, delivery),
            Command::Stop { task } => self.remote_stop(&task),
            Command::Permission { task, request_id, decision } => self.remote_permission(&task, &request_id, decision),
            Command::Answer { task, request_id, answer } => self.remote_answer(&task, &request_id, answer),
            Command::Patch { task, title, pinned, archived } => self.remote_patch(&task, title, pinned, archived),
            Command::Read { task } => self.remote_read(&task),
        };
        request.answer(result);
    }

    /// What the phone sees now: the sidebar's tasks with their turns.
    fn remote_live(&self) -> Live {
        let tasks = self
            .view
            .sessions
            .iter()
            .map(|session| {
                let selected = self.view.selected_session.as_deref() == Some(session.id.as_str());
                let waiting = self.view.off_screen_turns.get(&session.id) == Some(&true)
                    || (selected && !self.view.pending_permissions.is_empty());
                TaskRow {
                    id: session.id.clone(),
                    title: session.title.clone(),
                    project: PathBuf::from(&session.project),
                    backend: session.backend.clone(),
                    pinned: session.pinned,
                    unread: session.unread,
                    running: waiting || self.remote_task_busy(&session.id),
                    waiting,
                    queued: self.turn_queue.get(&session.id).map_or(0, |queue| queue.items.len()),
                }
            })
            .collect();
        let mut agents = vec![Agent { backend: "native".into(), name: "openADE".into(), startable: true }];
        agents.extend(self.view.acp_agents.iter().map(|offer| Agent {
            backend: format!("{}{}", crate::acp_run::ACP_BACKEND, offer.id),
            name: offer.name.clone(),
            startable: true,
        }));
        agents.extend(self.view.agent_clis.iter().map(|cli| Agent {
            backend: cli.id.clone(),
            name: cli.label.clone(),
            startable: false,
        }));
        let mut projects: Vec<PathBuf> = vec![PathBuf::from(&self.view.workspace)];
        projects.extend(self.known_project_paths.iter().map(PathBuf::from));
        Live { tasks, agents, projects, desktop_focused: self.background.focused }
    }

    /// The task has a turn going on, on screen or off it.
    fn remote_task_busy(&self, task: &str) -> bool {
        let selected = self.view.selected_session.as_deref() == Some(task);
        (selected && (self.view.running || self.active_run.is_some() || self.view.background_run_selected))
            || self.parked_runs.iter().any(|parked| parked.run.session_id() == task && !parked.ended)
            || self.background_automation_runs.iter().any(|run| run.session_id == task)
            || self.background_resumes.iter().any(|resume| resume.holds(task))
            || self.view.off_screen_turns.contains_key(task)
    }

    /// Opens `task`, does `act` there, and opens again what was open.
    fn on_task<T>(&mut self, task: &str, act: impl FnOnce(&mut Self) -> Result<T, String>) -> Result<T, String> {
        let previous = self.view.selected_session.clone();
        let draft = (self.view.prompt.clone(), self.view.composer_attachments.clone());
        let here = previous.as_deref() == Some(task);
        if !here {
            self.open_session(task);
            if self.view.selected_session.as_deref() != Some(task) {
                return Err(self.view.status.clone());
            }
        }
        let result = act(self);
        if !here {
            match &previous {
                Some(id) => self.open_session(id),
                None => {
                    self.new_session();
                    (self.view.prompt, self.view.composer_attachments) = draft;
                    self.save_draft();
                }
            }
        }
        result
    }

    /// Phone uploads as attachments of `workspace`, copied into its
    /// attachments folder as pasted files are.
    fn remote_attachments(workspace: &Path, files: &[PathBuf]) -> Result<Vec<ComposerAttachmentView>, String> {
        let root = workspace.canonicalize().map_err(|error| error.to_string())?;
        files
            .iter()
            .map(|file| {
                let (path, _) = crate::composer_attachments::import_file(&root, file)?;
                let relative = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                let label = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                Ok(ComposerAttachmentView { path: relative, label, is_dir: false, selection: None })
            })
            .collect()
    }

    fn remote_create_task(
        &mut self,
        project: &Path,
        backend: &str,
        prompt: &str,
        files: &[PathBuf],
        plan: bool,
    ) -> Result<Value, String> {
        let attachments = Self::remote_attachments(project, files)?;
        let workspace = self.view.workspace.clone();
        let previous = self.view.selected_session.clone();
        let draft = (self.view.prompt.clone(), self.view.composer_attachments.clone());
        let target = project.to_string_lossy().into_owned();
        if workspace != target {
            self.change_workspace(target.clone());
            if self.view.workspace != target {
                return Err("openADE no pudo abrir ese proyecto ahora".into());
            }
        }
        let before = self.view.selected_session.clone();
        if let Some(agent) = backend.strip_prefix(crate::acp_run::ACP_BACKEND) {
            self.start_acp_task(agent);
        } else {
            self.new_session();
            self.view.selected_backend = backend.to_owned();
        }
        let opened = if backend.starts_with(crate::acp_run::ACP_BACKEND) {
            self.view.selected_session != before
        } else {
            self.view.selected_session.is_none()
        };
        let started = opened && {
            self.view.turn_mode = if plan { TurnMode::Plan } else { TurnMode::Build };
            self.view.prompt = prompt.to_owned();
            self.view.composer_attachments = attachments;
            self.start_run();
            self.active_run.is_some()
        };
        let task = self.view.selected_session.clone().filter(|_| started);
        let refusal = self.view.status.clone();
        if self.view.workspace != workspace {
            self.change_workspace(workspace);
        }
        match previous {
            Some(id) => self.open_session(&id),
            None => {
                self.new_session();
                (self.view.prompt, self.view.composer_attachments) = draft;
                self.save_draft();
            }
        }
        self.refresh_sessions();
        match task {
            Some(task) => Ok(serde_json::json!({"task": task})),
            None if refusal.is_empty() => Err("openADE no pudo iniciar la tarea".into()),
            None => Err(refusal),
        }
    }

    fn remote_send(&mut self, task: &str, prompt: &str, files: &[PathBuf], delivery: Delivery) -> Result<Value, String> {
        let session = self.view.sessions.iter().find(|session| session.id == task).ok_or("Esa tarea ya no existe")?;
        if !openide_remote::tasks::has_chat(&session.backend) {
            return Err("Esa tarea corre en una terminal del PC; escribile desde ahí".into());
        }
        let attachments = Self::remote_attachments(Path::new(&session.project), files)?;
        if delivery == Delivery::Queue || self.remote_task_busy(task) {
            let queue = self.turn_queue.entry(task.to_owned()).or_default();
            if queue.items.len() >= MAX_QUEUED_TURNS {
                return Err(format!("La cola ya tiene {MAX_QUEUED_TURNS} mensajes"));
            }
            queue.items.push(QueuedTurn { id: self.turn_queue_next, text: prompt.to_owned(), attachments });
            queue.paused = false;
            self.turn_queue_next += 1;
            self.save_turn_queues();
            self.refresh_queued_messages();
            return Ok(serde_json::json!({"delivered": "queued"}));
        }
        self.start_turn_off_screen(task, prompt, attachments)?;
        Ok(serde_json::json!({"delivered": "started"}))
    }

    /// Sends the first queued message of each task that is off screen and no
    /// longer busy; the task on screen drains its own.
    fn drain_off_screen_queues(&mut self) -> bool {
        let ready: Vec<String> = self
            .turn_queue
            .iter()
            .filter(|(task, queue)| {
                !queue.paused
                    && !queue.items.is_empty()
                    && self.view.selected_session.as_deref() != Some(task.as_str())
            })
            .map(|(task, _)| task.clone())
            .filter(|task| !self.remote_task_busy(task))
            .collect();
        let mut sent = false;
        for task in ready {
            let Some(queue) = self.turn_queue.get_mut(&task) else { continue };
            let turn = queue.items.remove(0);
            if self.start_turn_off_screen(&task, &turn.text, turn.attachments.clone()).is_err() {
                let queue = self.turn_queue.entry(task).or_default();
                queue.items.insert(0, turn);
                queue.paused = true;
            }
            sent = true;
        }
        if sent {
            self.save_turn_queues();
            self.refresh_queued_messages();
        }
        sent
    }

    fn remote_stop(&mut self, task: &str) -> Result<Value, String> {
        if !self.remote_task_busy(task) {
            return Err("La tarea no está corriendo".into());
        }
        self.on_task(task, |window| {
            if window.cancel_selected_run() {
                Ok(serde_json::json!({"stopped": true}))
            } else {
                Err("openADE no pudo detener la tarea".into())
            }
        })
    }

    fn remote_permission(&mut self, task: &str, request_id: &str, decision: Decision) -> Result<Value, String> {
        // Only for a repaint request: a window not drawn yet has none.
        let ctx = self.background.ctx.clone().unwrap_or_default();
        self.on_task(task, |window| {
            let Some(permission) =
                window.view.pending_permissions.iter().find(|permission| permission.request_id == request_id)
            else {
                return Err("Ya se respondió desde el PC".into());
            };
            let choice = match decision {
                Decision::Allow => "allow_once",
                Decision::AllowAlways
                    if crate::approval_ui::command_prefix(&permission.tool_name, &permission.arguments).is_some() =>
                {
                    "allow_command"
                }
                Decision::AllowAlways => "allow_session",
                Decision::Deny => "deny",
            };
            let target = (task, window.view.workspace.clone(), None::<String>, request_id, choice);
            let action = UiAction::Clicked {
                id: format!("approval_decide:{}", serde_json::to_string(&target).map_err(|error| error.to_string())?),
            };
            window.apply_approval_action(&action, &ctx);
            if window.view.pending_permissions.iter().any(|permission| permission.request_id == request_id) {
                Err(window.view.status.clone())
            } else {
                Ok(serde_json::json!({"decision": choice}))
            }
        })
    }

    fn remote_answer(&mut self, task: &str, request_id: &str, answer: String) -> Result<Value, String> {
        self.on_task(task, |window| {
            if !window.view.pending_permissions.iter().any(|permission| permission.request_id == request_id) {
                return Err("Ya se respondió desde el PC".into());
            }
            window.answer_question(request_id, Some((answer, Vec::new())));
            Ok(serde_json::json!({"answered": true}))
        })
    }

    fn remote_patch(
        &mut self,
        task: &str,
        title: Option<String>,
        pinned: Option<bool>,
        archived: Option<bool>,
    ) -> Result<Value, String> {
        let store = self.store.clone().ok_or("El historial no está disponible")?;
        let result = (|| {
            if let Some(title) = &title {
                store.rename(task, Some(title.trim()))?;
            }
            if let Some(pinned) = pinned {
                store.set_pinned(task, pinned)?;
            }
            if let Some(archived) = archived {
                store.set_archived(task, archived)?;
            }
            Ok::<_, openide_core::StoreError>(())
        })();
        self.refresh_sessions();
        result.map_err(|error| error.to_string())?;
        Ok(serde_json::json!({"updated": true}))
    }

    fn remote_read(&mut self, task: &str) -> Result<Value, String> {
        let store = self.store.clone().ok_or("El historial no está disponible")?;
        let marker = store.load(task).ok().flatten().and_then(|stored| stored.session.unread_at_ms);
        if let Some(marker) = marker {
            store.clear_unread_if_matches(task, marker).map_err(|error| error.to_string())?;
            self.refresh_sessions();
        }
        Ok(serde_json::json!({"read": true}))
    }
}
