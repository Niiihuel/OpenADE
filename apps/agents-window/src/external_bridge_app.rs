//! Runs the loopback bridge for agent CLIs started from the terminal dock and
//! brings their approval requests to the window. The server starts with the
//! first agent terminal; a plain shell never opens a port.
//!
//! A CLI's browser tools drive a browser of its own, one per terminal: the
//! first call opens its tab and starts it, and it ends with the terminal's
//! grant. It is never a conversation's browser. A native conversation's agent
//! gets its own conversation's tab the same way, opened by its first call.
use super::*;
use crate::agent_cli_definitions::{self, AgentCliDefinition};
use crate::browser_runtime::{BrowserMcpClient, ConversationBrowser};
use openide_bridge::{Approver, Bridge, Grant, Host, HostTool};
use std::ffi::OsString;
use std::sync::Mutex;

const APPROVAL_LIFETIME: Duration = Duration::from_secs(50);
const MAX_REVIEW_CHARS: usize = 60_000;
/// How long a browser call waits for its browser, which a first call has to
/// start.
const BROWSER_WAIT: Duration = Duration::from_secs(30);
const SESSION_ENDED: &str = "La sesión de esta CLI terminó y su navegador se cerró.";

/// A browser call waiting for the window to hand it its browser: a CLI's is
/// the one of its terminal, a native conversation's the tab of its own dock.
struct BrowserCall {
    /// The session of the grant the call came under, or the conversation's.
    session: String,
    /// The dock of the native conversation whose agent made the call.
    conversation: Option<DockScope>,
    reply: mpsc::Sender<Result<BrowserMcpClient, String>>,
    // The call stops waiting after this long; a later browser reaches nobody.
    expires: Instant,
    /// This call already asked for the context: one that failed to start is
    /// not started again for it.
    started: bool,
}

struct PendingApproval {
    title: String,
    detail: String,
    reply: mpsc::Sender<bool>,
    // The bridge stops waiting after this long; a later answer reaches nobody.
    expires: Instant,
}

pub(super) struct ExternalBridgeHost {
    policy: Arc<Mutex<ExternalAgentSettings>>,
    server: Option<(Bridge, String)>,
    approvals: Arc<Mutex<VecDeque<PendingApproval>>>,
    repaint: Arc<Mutex<Option<egui::Context>>>,
    agents: HashMap<u64, (&'static AgentCliDefinition, bool)>,
    /// Per terminal tab: its token, the file that holds it and its session.
    grants: HashMap<u64, (String, Option<PathBuf>, String)>,
    browser_calls: Arc<Mutex<Vec<BrowserCall>>>,
    /// A CLI's browser tab opened or closed since the dock's view was built.
    dock_stale: bool,
}

pub(super) type AgentCommand = (OsString, Vec<OsString>, Vec<(&'static str, String)>);

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn review_changes(grant: &Grant) -> Result<String, String> {
    // The summary loader alone has no diff lines; a CLI has no panel to open
    // each file in, so they are loaded here.
    let review = crate::git_review::load_git_review_with_diffs(&grant.workspace, MAX_REVIEW_CHARS)
        .map_err(|error| format!("No se pudieron leer los cambios: {error}"))?;
    let mut text = format!(
        "Rama {} · {} archivos · +{} -{}\n",
        review.branch, review.files, review.additions, review.deletions
    );
    for section in &review.sections {
        text.push_str(&format!("\n## {}\n", section.title));
        for file in &section.files {
            text.push_str(&format!(
                "\n{} {} (+{} -{})\n",
                file.status, file.path, file.additions, file.deletions
            ));
            for line in &file.lines {
                text.push_str(&line.text);
                text.push('\n');
            }
        }
    }
    if !review.untracked.is_empty() {
        text.push_str(&format!("\n## Sin seguimiento\n{}\n", review.untracked.join("\n")));
    }
    if review.truncated || text.chars().count() > MAX_REVIEW_CHARS {
        text = text.chars().take(MAX_REVIEW_CHARS).collect();
        text.push_str("\n[Cambios recortados por tamaño.]");
    }
    Ok(text)
}

/// One browser tool call of a CLI, on the bridge's thread. The window hands
/// over the browser of that CLI's terminal, which the first call starts.
fn browser_call(
    calls: &Mutex<Vec<BrowserCall>>,
    repaint: &Mutex<Option<egui::Context>>,
    grant: &Grant,
    name: &str,
    arguments: &serde_json::Value,
) -> Result<Vec<serde_json::Value>, String> {
    // A client may leave out the arguments of a tool that takes none.
    let arguments = if arguments.is_null() { serde_json::json!({}) } else { arguments.clone() };
    // Checked before anything else: a call that is refused starts no browser.
    crate::browser_runtime::browser_tool_command(name, arguments.clone())?;
    wait_for_browser(calls, repaint, grant.session.clone(), None)?.call_blocking(name, arguments)
}

/// Asks the window for the browser of `session` and waits for it, on the
/// caller's thread.
fn wait_for_browser(
    calls: &Mutex<Vec<BrowserCall>>,
    repaint: &Mutex<Option<egui::Context>>,
    session: String,
    conversation: Option<DockScope>,
) -> Result<BrowserMcpClient, String> {
    let (reply, browser) = mpsc::channel();
    locked(calls).push(BrowserCall {
        session,
        conversation,
        reply,
        expires: Instant::now() + BROWSER_WAIT,
        started: false,
    });
    if let Some(ctx) = locked(repaint).as_ref() {
        ctx.request_repaint();
    }
    browser
        .recv_timeout(BROWSER_WAIT)
        .map_err(|_| "El navegador no llegó a iniciar.".to_owned())?
}

impl ExternalBridgeHost {
    pub(super) fn new(settings: ExternalAgentSettings) -> Self {
        Self {
            policy: Arc::new(Mutex::new(settings)),
            server: None,
            approvals: Arc::default(),
            repaint: Arc::default(),
            agents: HashMap::new(),
            grants: HashMap::new(),
            browser_calls: Arc::default(),
            dock_stale: false,
        }
    }

    pub(super) fn set_policy(&self, settings: ExternalAgentSettings) {
        *locked(&self.policy) = settings;
    }

    fn revoke(&mut self, id: u64) {
        if let Some((token, config_file, _)) = self.grants.remove(&id) {
            if let Some((bridge, _)) = &self.server {
                bridge.revoke(&token);
            }
            if let Some(path) = config_file {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    fn host(&self) -> Host {
        let settings = self.policy.clone();
        let approvals = self.approvals.clone();
        let repaint = self.repaint.clone();
        let approver: Approver = Arc::new(move |tool, arguments, grant| {
            let (reply, answer) = mpsc::channel();
            locked(&approvals).push_back(PendingApproval {
                title: format!("Una CLI externa quiere usar {}", tool.name),
                detail: format!(
                    "Proyecto: {}\nArgumentos: {arguments}",
                    grant.workspace.display()
                ),
                reply,
                expires: Instant::now() + APPROVAL_LIFETIME,
            });
            if let Some(ctx) = locked(&repaint).as_ref() {
                ctx.request_repaint();
            }
            answer
        });
        let mut tools = vec![HostTool {
            name: "review_changes".into(),
            group: ExternalTool::GitReview.name(),
            description: "Devuelve los cambios sin confirmar del proyecto tal como los muestra la revisión de openADE: rama, archivos y diff por sección.".into(),
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
            run: Arc::new(|grant, _| review_changes(grant).map(openide_bridge::text)),
        }];
        // The native agent's browser tools, on a browser of the calling CLI's
        // own. `open_preview` is not among them: it opens the server a native
        // conversation started.
        for tool in crate::browser_runtime::browser_mcp_tools() {
            let (calls, repaint) = (self.browser_calls.clone(), self.repaint.clone());
            let name = tool.name.clone();
            tools.push(HostTool {
                name: tool.name,
                group: ExternalTool::Browser.name(),
                description: tool.description.unwrap_or_default(),
                input_schema: tool.input_schema,
                run: Arc::new(move |grant, arguments| {
                    browser_call(&calls, &repaint, grant, &name, arguments)
                }),
            });
        }
        Host {
            policy: Arc::new(move |group, cli_accepts_all| {
                let settings = *locked(&settings);
                crate::external_agent_settings::ALL_TOOLS
                    .into_iter()
                    .find(|tool| tool.name() == group)
                    .map_or(PermissionMode::Deny, |tool| settings.mode_for(tool, cli_accepts_all))
            }),
            approver: Some(approver),
            tools,
        }
    }

    /// Starts the server on first use. `None` keeps the CLI unconnected.
    fn server(&mut self, store: &Arc<SessionStore>) -> Option<(Bridge, String)> {
        if self.server.is_none() {
            let bridge = Bridge::new(store.clone(), self.host());
            let serving = bridge.clone();
            let (ready, bound) = mpsc::channel();
            std::thread::Builder::new()
                .name("openide-bridge".into())
                .spawn(move || {
                    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                        .enable_io()
                        .build()
                    else {
                        return;
                    };
                    runtime.block_on(async move {
                        if let Ok((addr, serve)) = serving.bind().await {
                            let _ = ready.send(addr);
                            let _ = serve.await;
                        }
                    });
                })
                .ok()?;
            let addr = bound.recv_timeout(Duration::from_secs(2)).ok()?;
            self.server = Some((bridge, format!("http://{addr}/mcp")));
        }
        self.server.clone()
    }
}

/// Runs hooks from a thread that has no async runtime of its own.
fn run_hook_event(
    hooks: &CommandHooks,
    event: openide_harness::CompatEvent,
    subject: &str,
    payload: serde_json::Value,
) -> openide_harness::EventOutcome {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return Default::default();
    };
    runtime.block_on(hooks.run_event(event, subject, payload, &CancellationToken::new()))
}

impl AgentsWindow {
    /// Runs the hooks that fire when a prompt is sent and returns the text
    /// they add for the model. `Err` is a hook's refusal of this prompt.
    ///
    /// Session-start hooks run once per conversation and per hook set; their
    /// output is kept in memory and reused, because a resumed turn replays the
    /// instructions saved with it and must not depend on a second run.
    // shortcut: these run on the UI thread and can stall it for a hook's whole timeout, move turn admission off the UI thread before raising that limit.
    pub(super) fn turn_start_hook_context(
        &mut self,
        hooks: &CommandHooks,
        prompt: &str,
    ) -> Result<(String, String, String), String> {
        use openide_harness::CompatEvent;
        let digest = hooks.digest_hex().map_err(|error| error.to_string())?;
        let key = (
            self.view.selected_session.clone(),
            self.view.workspace.clone(),
            digest,
        );
        let session_context = match &self.session_start_context {
            Some((cached, context)) if *cached == key => context.clone(),
            _ => {
                let source = if key.0.is_some() { "resume" } else { "startup" };
                let outcome = run_hook_event(
                    hooks,
                    CompatEvent::SessionStart,
                    source,
                    serde_json::json!({ "source": source, "session_id": key.0 }),
                );
                let context = outcome.context.join("\n\n");
                self.session_start_context = Some((key.clone(), context.clone()));
                context
            }
        };
        let submitted = run_hook_event(
            hooks,
            CompatEvent::UserPromptSubmit,
            "",
            serde_json::json!({ "prompt": prompt, "session_id": key.0 }),
        );
        if let Some(reason) = submitted.blocked {
            return Err(format!("Un hook rechazó este mensaje: {reason}"));
        }
        // Subagents of this turn share one worker, so their start hooks run
        // once here and not per child.
        // shortcut: a matcher on the agent type is not honoured, run these per spawn when roles need different hooks.
        let subagents = run_hook_event(
            hooks,
            CompatEvent::SubagentStart,
            "",
            serde_json::json!({ "session_id": key.0, "agent_type": "" }),
        );
        Ok((
            session_context,
            submitted.context.join("\n\n"),
            subagents.context.join("\n\n"),
        ))
    }

    /// A new conversation gets its ID after its first hooks ran; without
    /// this its second turn would look like another session start.
    pub(super) fn bind_session_start_context(&mut self, session_id: &str) {
        if let Some(((session, _, _), _)) = &mut self.session_start_context {
            if session.is_none() {
                *session = Some(session_id.to_owned());
            }
        }
    }

    pub(super) fn terminal_agent(&self, id: u64) -> Option<&'static AgentCliDefinition> {
        self.external_bridge.agents.get(&id).map(|(agent, _)| *agent)
    }

    pub(super) fn mark_agent_terminal(&mut self, id: u64, agent: (&'static AgentCliDefinition, bool)) {
        self.external_bridge.agents.insert(id, agent);
    }

    /// Where agent CLIs are looked up: the user's shell path once it is
    /// known, otherwise this process's own.
    pub(super) fn agent_search_path(&self) -> Option<OsString> {
        locked(&self.login_path).clone()
    }

    pub(super) fn resolve_agent_cli(&self, agent: &AgentCliDefinition) -> Option<PathBuf> {
        agent.resolve(
            self.agent_cli_paths.get(&agent.id),
            self.agent_search_path().as_deref(),
        )
    }

    /// Reloads the detected CLI list shown in Settings.
    pub(super) fn refresh_agent_clis(&mut self) {
        self.view.agent_clis = agent_cli_definitions::definitions()
            .iter()
            .map(|agent| crate::view::AgentCliRowView {
                id: agent.id.clone(),
                label: agent.label.clone(),
                resolved: self
                    .resolve_agent_cli(agent)
                    .map(|path| path.display().to_string()),
                manual: self
                    .agent_cli_paths
                    .get(&agent.id)
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            })
            .collect();
    }

    pub(super) fn set_agent_cli_path(&mut self, id: &str, value: &str) {
        let mut next = self.agent_cli_paths.clone();
        match next.set(id, value).and_then(|()| next.save(&self.data_root)) {
            Ok(()) => {
                self.agent_cli_paths = next;
                self.set_settings_notice(
                    if value.trim().is_empty() { "Ruta manual eliminada" } else { "Ruta manual guardada" },
                    false,
                );
            }
            Err(error) => self.set_settings_notice(error, true),
        }
        self.refresh_agent_clis();
    }

    /// The command for an agent terminal, connected to the bridge when the
    /// user has opened it in Settings. `None` means an ordinary shell.
    pub(super) fn agent_terminal_command(&mut self, id: u64) -> Option<std::io::Result<AgentCommand>> {
        let (agent, accept_all) = *self.external_bridge.agents.get(&id)?;
        // A restart must not leave the previous process's token alive, nor
        // the browser that process opened.
        self.external_bridge.dock_stale |= self.revoke_external_grant(id);
        Some((|| {
            let program = self.resolve_agent_cli(agent).ok_or_else(|| {
                std::io::Error::other(format!(
                    "No se encontró {}; indicá su ruta en Ajustes → CLIs externas",
                    agent.label
                ))
            })?;
            let mut args: Vec<OsString> = Vec::new();
            if accept_all {
                args.extend(agent.accept_all_args.iter().map(OsString::from));
            }
            let mut env = Vec::new();
            // The CLI needs the same search path it was found with (its own
            // runtime, such as node, usually sits beside it).
            if let Some(path) = self.agent_search_path().and_then(|path| path.into_string().ok()) {
                env.push(("PATH", path));
            }
            let store = self.store.clone().filter(|_| self.external_agent_settings.enabled());
            let connection = agent.bridge.zip(store).and_then(|(kind, store)| {
                self.external_bridge.server(&store).map(|server| (kind, server))
            });
            if let Some((kind, (bridge, url))) = connection {
                let config_dir = self.data_root.join("external-bridge");
                std::fs::create_dir_all(&config_dir)?;
                #[cfg(unix)]
                std::fs::set_permissions(
                    &config_dir,
                    std::os::unix::fs::PermissionsExt::from_mode(0o700),
                )?;
                // New with every start: a restarted CLI never reaches the
                // browser of the process before it.
                let session = format!("terminal:{id}:{}", uuid::Uuid::new_v4().simple());
                let token = bridge.grant(Grant {
                    workspace: PathBuf::from(&self.view.workspace),
                    // OpenIDE chose these arguments itself, so this is known.
                    cli_accepts_all: accept_all,
                    session: session.clone(),
                });
                match kind.bridge_launch(&url, &token, &config_dir) {
                    Ok(launch) => {
                        args.extend(launch.args);
                        env.extend(launch.env);
                        self.external_bridge
                            .grants
                            .insert(id, (token, launch.config_file, session));
                    }
                    Err(error) => {
                        bridge.revoke(&token);
                        return Err(error);
                    }
                }
            }
            Ok((program.into_os_string(), args, env))
        })())
    }

    /// Drops grants whose terminal tab is gone.
    pub(super) fn prune_external_grants(&mut self) {
        let closed: Vec<u64> = self
            .external_bridge
            .agents
            .keys()
            .copied()
            .filter(|id| !self.dock.contains_tab(*id))
            .collect();
        for id in closed {
            self.external_bridge.agents.remove(&id);
            self.revoke_external_grant(id);
        }
    }

    pub(super) fn revoke_all_external_grants(&mut self) {
        for id in self.external_bridge.grants.keys().copied().collect::<Vec<_>>() {
            self.revoke_external_grant(id);
        }
    }

    /// Ends the grant of terminal tab `id`: its token, its token file and the
    /// browser its CLI opened, with that browser's tab. Returns whether a
    /// tab closed.
    fn revoke_external_grant(&mut self, id: u64) -> bool {
        self.external_bridge.revoke(id);
        let kind = DockTabKind::CliBrowser { terminal: id };
        let Some(tab) = self.dock.tab_of_kind(&kind).cloned() else {
            return false;
        };
        self.dock.close(&DockScope::new(tab.workspace_id, tab.owner), tab.id);
        // The browser ends here, not when the dock is next synced.
        self.browser_starts.remove(&tab.id.get());
        self.browsers.remove(&tab.id.get());
        true
    }

    /// The session of the CLI that runs in terminal tab `id`, while its
    /// grant lives.
    pub(super) fn external_session(&self, id: u64) -> Option<&str> {
        self.external_bridge
            .grants
            .get(&id)
            .map(|(_, _, session)| session.as_str())
    }

    /// The browser the agent of native conversation `session` is given, whose
    /// first call opens the tab in `scope`, that conversation's dock.
    pub(super) fn conversation_browser(&self, scope: DockScope, session: &str) -> ConversationBrowser {
        let calls = self.external_bridge.browser_calls.clone();
        let repaint = self.external_bridge.repaint.clone();
        let session = session.to_owned();
        ConversationBrowser::new(move || {
            wait_for_browser(&calls, &repaint, session.clone(), Some(scope.clone()))
        })
    }

    /// Hands each waiting browser call its browser. Returns whether the view
    /// changed.
    pub(super) fn serve_external_browser_calls(&mut self, ctx: &egui::Context) -> bool {
        let mut calls = std::mem::take(&mut *locked(&self.external_bridge.browser_calls));
        let now = Instant::now();
        calls.retain_mut(|call| {
            if call.expires <= now {
                return false;
            }
            match self.external_browser(call).transpose() {
                None => true,
                Some(answer) => {
                    let _ = call.reply.send(answer);
                    false
                }
            }
        });
        if !calls.is_empty() {
            // Nothing else wakes the window while a hidden context starts.
            ctx.request_repaint_after(Duration::from_millis(50));
            locked(&self.external_bridge.browser_calls).extend(calls);
        }
        let stale = std::mem::take(&mut self.external_bridge.dock_stale);
        if stale {
            self.sync_dock_view();
        }
        stale
    }

    /// The browser `call` is for: the one of the terminal whose grant it came
    /// under, or the tab of the native conversation that made it. Its first
    /// call opens the tab and starts the context; `None` while that starts.
    fn external_browser(&mut self, call: &mut BrowserCall) -> Result<Option<BrowserMcpClient>, String> {
        if !self.browser_automation_available() {
            return Err("La automatización del navegador está desactivada o sus permisos requieren atención en Ajustes → Navegador.".into());
        }
        let (kind, open) = match &call.conversation {
            Some(scope) => (
                DockTabKind::Browser,
                self.dock
                    .tabs(&scope.workspace_id)
                    .iter()
                    .find(|tab| tab.kind == DockTabKind::Browser && tab.owner == scope.owner)
                    .map(|tab| tab.id),
            ),
            None => {
                // Only a live grant names a terminal: a revoked session reaches nothing.
                let terminal = self
                    .external_bridge
                    .grants
                    .iter()
                    .find(|(_, (_, _, session))| *session == call.session)
                    .map(|(id, _)| *id)
                    .ok_or(SESSION_ENDED)?;
                // The switch may have gone off after the bridge let this call through.
                if self.external_agent_settings.mode_for(ExternalTool::Browser, true) == PermissionMode::Deny {
                    return Err("Herramienta desactivada en Ajustes → CLIs externas.".into());
                }
                let kind = DockTabKind::CliBrowser { terminal };
                let open = self.dock.tab_of_kind(&kind).map(|tab| tab.id);
                (kind, open)
            }
        };
        let tab = match open {
            Some(tab) => tab,
            None => {
                if self.browsers.len() + self.browser_starts.len() >= MAX_BROWSERS {
                    return Err(BROWSER_LIMIT.into());
                }
                let scope = match (&call.conversation, &kind) {
                    (Some(scope), _) => scope.clone(),
                    // Beside its terminal, in that conversation's dock.
                    (None, DockTabKind::CliBrowser { terminal }) => {
                        let owner = self.dock.tab(*terminal).ok_or(SESSION_ENDED)?;
                        DockScope::new(owner.workspace_id.clone(), owner.owner.clone())
                    }
                    (None, _) => return Err(SESSION_ENDED.into()),
                };
                self.external_bridge.dock_stale = true;
                self.open_tab_beside_typing(&scope, kind)
            }
        }
        .get();
        let Some(runtime) = self.browsers.get(&tab) else {
            // Without the window's scale there is nothing to start it with yet.
            if self.browser_starts.contains_key(&tab) || self.browser_scale == 0.0 {
                return Ok(None);
            }
            if std::mem::replace(&mut call.started, true) {
                return Err("No se pudo iniciar el navegador.".into());
            }
            let scope = self.browser_launch_scope(tab).ok_or(SESSION_ENDED)?;
            self.start_browser(tab, scope)?;
            return Ok(None);
        };
        // What the native agent's client is checked for against its
        // conversation: this context is the calling session's and no other's.
        let own = runtime.scope().session_id == call.session
            // A tab the user opened before the conversation's first message.
            || (call.conversation.is_some() && runtime.scope().session_id == format!("draft:{tab}"));
        if !own {
            return Err("Ese navegador pertenece a otra sesión.".into());
        }
        let snapshot = runtime.snapshot();
        match snapshot.error {
            _ if snapshot.started => Ok(Some(runtime.mcp_client())),
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    /// Reloads the Claude Code hook list shown in Settings.
    pub(super) fn refresh_compat_hooks(&mut self) {
        use crate::compat_hook_settings::{discover, runs_today, CompatHookSettings};
        let loaded = CompatHookSettings::load(&self.data_root).and_then(|settings| {
            let found = match &self.compat_hook_source {
                Some(source) => discover(source)?,
                None => Vec::new(),
            };
            Ok((settings, found))
        });
        let (rows, error) = match loaded {
            Ok((settings, found)) => (
                found
                    .iter()
                    .map(|hook| crate::view::CompatHookRowView {
                        origin: hook.key.origin.clone(),
                        event: hook.key.event.clone(),
                        matcher: hook.key.matcher.clone(),
                        command: hook.key.command.clone(),
                        enabled: settings.is_enabled(hook),
                        runs_today: runs_today(hook.event),
                    })
                    .collect(),
                None,
            ),
            Err(error) => (Vec::new(), Some(error)),
        };
        self.view.compat_hooks = rows;
        self.view.compat_hooks_error = error;
    }

    /// Follows plugin updates and drops plugins Claude Code no longer has.
    /// Called before a turn reads the skill index and when Settings opens the
    /// list. Does nothing when the plugin source cannot be read: a transient
    /// read error must not switch the user's plugins off.
    pub(super) fn sync_plugin_approvals(&mut self) {
        use crate::compat_hook_settings::{discover_plugins, CompatHookSettings};
        let Some(source) = self.compat_hook_source.as_deref().filter(|path| path.is_file()) else {
            return;
        };
        let (Ok(discovered), Ok(mut settings)) =
            (discover_plugins(source), CompatHookSettings::load(&self.data_root))
        else {
            return;
        };
        let (moved, removed) = settings.sync_plugins(&discovered);
        if moved.is_empty() && removed.is_empty() {
            return;
        }
        if settings.save(&self.data_root).is_err() {
            return;
        }
        let mut parts = Vec::new();
        if !moved.is_empty() {
            parts.push(format!("se actualizó {} y sigue activo", moved.join(", ")));
        }
        if !removed.is_empty() {
            parts.push(format!("{} ya no está en Claude Code y se desactivó", removed.join(", ")));
        }
        self.view.status = format!("Plugins: {}", parts.join("; "));
    }

    /// Reloads the plugin skill list shown in Settings.
    pub(super) fn refresh_plugin_skills(&mut self) {
        use crate::compat_hook_settings::{discover_plugins, CompatHookSettings};
        self.sync_plugin_approvals();
        let settings = CompatHookSettings::load(&self.data_root).unwrap_or_default();
        self.view.plugin_skills = self
            .compat_hook_source
            .as_deref()
            .and_then(|source| discover_plugins(source).ok())
            .unwrap_or_default()
            .iter()
            .map(|plugin| crate::view::PluginSkillsRowView {
                id: plugin.id.clone(),
                skill_count: plugin.skill_count,
                enabled: settings.plugin_skills_enabled(plugin),
            })
            .collect();
    }

    /// Switches one plugin's skills on or off for the next turns.
    pub(super) fn toggle_plugin_skills(&mut self, index: usize) {
        use crate::compat_hook_settings::{discover_plugins, CompatHookSettings};
        let Some(row) = self.view.plugin_skills.get(index).cloned() else {
            return;
        };
        let result = (|| {
            let source = self.compat_hook_source.as_ref().ok_or("Sin origen de plugins")?;
            let plugins = discover_plugins(source)?;
            let plugin = plugins
                .iter()
                .find(|plugin| plugin.id == row.id)
                .ok_or("El plugin ya no está instalado o activo en Claude Code")?;
            let mut settings = CompatHookSettings::load(&self.data_root)?;
            let enabled = settings.toggle_plugin_skills(plugin);
            settings.save(&self.data_root)?;
            Ok::<bool, String>(enabled)
        })();
        self.refresh_plugin_skills();
        match result {
            Ok(true) => self.set_settings_notice("Skills del plugin ofrecidas al agente en los próximos turnos", false),
            Ok(false) => self.set_settings_notice("Skills del plugin retiradas", false),
            Err(error) => self.set_settings_notice(error, true),
        }
    }

    /// Toggles the listed hook. The row is matched against a fresh read of
    /// the source, so a stale list cannot enable a command that changed.
    pub(super) fn toggle_compat_hook(&mut self, index: usize) {
        use crate::compat_hook_settings::{discover, CompatHookSettings};
        let Some(row) = self.view.compat_hooks.get(index).cloned() else {
            return;
        };
        let result = (|| {
            let source = self.compat_hook_source.as_ref().ok_or("Sin origen de hooks")?;
            let found = discover(source)?;
            let hook = found
                .iter()
                .find(|hook| {
                    hook.key.origin == row.origin
                        && hook.key.event == row.event
                        && hook.key.matcher == row.matcher
                        && hook.key.command == row.command
                })
                .ok_or("El hook cambió en su archivo de origen; revisalo antes de activarlo")?;
            let mut settings = CompatHookSettings::load(&self.data_root)?;
            let enabled = settings.toggle(hook);
            settings.save(&self.data_root)?;
            Ok::<bool, String>(enabled)
        })();
        self.refresh_compat_hooks();
        match result {
            Ok(true) => self.set_settings_notice("Hook activado para los próximos turnos", false),
            Ok(false) => self.set_settings_notice("Hook desactivado", false),
            Err(error) => self.set_settings_notice(error, true),
        }
    }

    /// After a turn that changed something, runs the project's check off the
    /// UI thread. A check already running for an older turn is left to finish;
    /// its result is dropped by `poll_turn_check` if the session changed.
    pub(super) fn start_turn_check(&mut self) {
        let had_effects = std::mem::take(&mut self.turn_had_effects);
        self.view.turn_check_failure = None;
        let Some(session) = self.view.selected_session.clone() else {
            return;
        };
        let workspace = self.view.workspace.clone();
        if self.turn_check_run.is_some() {
            return;
        }
        // The project's own command only matters after a change; stop hooks
        // are the user's and run after every completed turn.
        let command = self
            .turn_check_settings
            .command(&workspace)
            .filter(|_| had_effects)
            .map(str::to_owned);
        let hooks = crate::native_hook_settings::NativeHookSettings::load(&self.data_root)
            .and_then(|settings| {
                settings.registry_for(
                    &self.data_root,
                    Path::new(&workspace),
                    self.compat_hook_source.as_deref(),
                )
            })
            .ok()
            .filter(|hooks| {
                hooks
                    .compat_for_event(openide_harness::CompatEvent::Stop, "")
                    .next()
                    .is_some()
            });
        if command.is_none() && hooks.is_none() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let repaint = locked(&self.external_bridge.repaint).clone();
        let spawned = std::thread::Builder::new()
            .name("openide-turn-check".into())
            .spawn(move || {
                let stopped = hooks.and_then(|hooks| {
                    run_hook_event(
                        &hooks,
                        openide_harness::CompatEvent::Stop,
                        "",
                        // Nothing continues the turn by itself, so a hook never
                        // sees its own retry.
                        serde_json::json!({ "session_id": session, "stop_hook_active": false }),
                    )
                    .blocked
                });
                let result = match (stopped, command) {
                    (Some(reason), _) => Err(format!("Un hook de fin de turno pidió seguir:\n{reason}")),
                    (None, Some(command)) => crate::turn_check::run(
                        &command,
                        Path::new(&workspace),
                        crate::turn_check::CHECK_TIMEOUT,
                    ),
                    (None, None) => Ok(()),
                };
                let _ = sender.send((session, result));
                if let Some(ctx) = repaint {
                    ctx.request_repaint();
                }
            });
        if spawned.is_ok() {
            self.turn_check_run = Some(receiver);
            self.view.status = "Turno completado · verificando el proyecto".to_owned();
        }
    }

    /// Returns whether the view changed.
    pub(super) fn poll_turn_check(&mut self) -> bool {
        let Some(receiver) = &self.turn_check_run else {
            return false;
        };
        let (session, result) = match receiver.try_recv() {
            Ok(finished) => finished,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.turn_check_run = None;
                return false;
            }
        };
        self.turn_check_run = None;
        // A result for a conversation the user left, or one that is running
        // again, would describe a state that no longer exists.
        if self.view.selected_session.as_deref() != Some(session.as_str()) || self.view.running {
            return false;
        }
        match result {
            Ok(()) => self.view.status = "Turno completado · chequeo del proyecto correcto".to_owned(),
            Err(failure) => {
                self.view.status = "Turno completado · el chequeo del proyecto falló".to_owned();
                self.view.turn_check_failure = Some(failure);
            }
        }
        true
    }

    /// Reviews the uncommitted changes in a conversation of its own: no
    /// memory of how they were made, and Plan mode, where the runner offers
    /// no tool that can change the project.
    ///
    /// Built from the same steps a user takes by hand. Leaving the current
    /// conversation saves its draft. A draft already waiting in the new
    /// conversation is set aside while the review is sent and then put back
    /// where it was, so the next new conversation still opens with it.
    pub(super) fn start_isolated_review(&mut self) {
        if self.view.running {
            self.view.status = "Detén el agente antes de iniciar una revisión".to_owned();
            return;
        }
        let review = match crate::git_review::load_git_review_with_diffs(
            Path::new(&self.view.workspace),
            crate::turn_check::MAX_REVIEW_DIFF_CHARS,
        ) {
            Ok(review) => review,
            Err(error) => {
                self.show_composer_error(format!("No se pudieron leer los cambios: {error}"));
                return;
            }
        };
        let Some(prompt) = crate::turn_check::review_prompt(&review) else {
            self.show_composer_error("No hay cambios sin confirmar para revisar");
            return;
        };
        // Accepting the command already removed its own text from the
        // composer. What is left is the user's draft for this conversation,
        // and leaving the conversation saves it.
        self.new_session();
        if self.view.selected_session.is_some() || self.view.running {
            return;
        }
        // Whatever was waiting in the new conversation: text and attachments.
        // Neither may travel with the review.
        let waiting_key = self.draft_key();
        let waiting_text = std::mem::take(&mut self.view.prompt);
        let waiting_files = std::mem::take(&mut self.view.composer_attachments);
        // The review may run on its own model. It rides on this one turn; the
        // user's selection is not touched. A manual choice that cannot be
        // used stops the review instead of quietly using another model.
        let review_model = self
            .review_model_settings
            .resolve(&self.native_settings(), self.view.catalog.as_deref());
        match review_model {
            Ok(model) => {
                self.turn_model_override = model;
                self.view.turn_mode = TurnMode::Plan;
                self.view.prompt = prompt.clone();
                self.tree.set_input_value("prompt", prompt);
                self.start_run();
                // `start_run` took it; clear in case it returned before that.
                self.turn_model_override = None;
            }
            Err(error) => self.show_composer_error(error),
        }
        if self.active_run.is_none() {
            // Not sent: give the composer back exactly what it had.
            self.view.turn_mode = TurnMode::Build;
            self.view.prompt = waiting_text.clone();
            self.tree.set_input_value("prompt", waiting_text);
            self.view.composer_attachments = waiting_files;
            return;
        }
        // Sending cleared the new conversation's draft slot; restore it there,
        // in memory and on disk, the way an edit would have saved it.
        if !waiting_files.is_empty() {
            self.attachment_drafts.insert(waiting_key.clone(), waiting_files);
        }
        if !waiting_text.is_empty() {
            if let Some(store) = &self.store {
                if let Err(error) = store.save_draft(Path::new(&waiting_key.0), None, &waiting_text) {
                    self.draft_error = Some(error.to_string());
                }
            }
            self.drafts.insert(waiting_key, waiting_text);
        }
    }

    /// Refreshes what Settings shows about the review model.
    pub(super) fn refresh_review_model(&mut self) {
        let label = |settings: &crate::native_provider::NativeProviderSettings| {
            format!("{} · {}", settings.provider_id, settings.model_id)
        };
        let automatic = self
            .view
            .catalog
            .as_deref()
            .and_then(|catalog| crate::review_model::cheaper_model(&self.native_settings(), catalog));
        self.view.review_model = crate::view::ReviewModelView {
            mode: self.review_model_settings.mode,
            manual: self
                .review_model_settings
                .manual
                .as_ref()
                .filter(|manual| !manual.model_id.is_empty())
                .map(label)
                .unwrap_or_default(),
            automatic: automatic.as_ref().map(label).unwrap_or_default(),
            manual_provider: self
                .review_model_settings
                .manual
                .as_ref()
                .map(|manual| manual.provider_id.clone())
                .unwrap_or_default(),
            manual_model: self
                .review_model_settings
                .manual
                .as_ref()
                .map(|manual| manual.model_id.clone())
                .unwrap_or_default(),
        };
    }

    fn commit_review_model(&mut self, next: crate::review_model::ReviewModelSettings, message: &str) {
        match next.save(&self.data_root) {
            Ok(()) => {
                self.review_model_settings = next;
                self.set_settings_notice(message, false);
            }
            Err(error) => self.set_settings_notice(error, true),
        }
        self.refresh_review_model();
    }

    pub(super) fn set_review_model_mode(&mut self, mode: crate::review_model::ReviewModelMode) {
        let mut next = self.review_model_settings.clone();
        next.mode = mode;
        self.commit_review_model(next, "Modelo de revisión actualizado");
    }

    /// Picks the provider of the manual review model. The model is cleared:
    /// a model ID means nothing under another provider. The choice is checked
    /// against what is connected now, so a stale menu cannot save one that is not.
    pub(super) fn pick_review_provider(&mut self, provider_id: &str) {
        let choices = crate::settings_ui::connected_model_choices(&self.view);
        if !choices.iter().any(|provider| provider.id == provider_id) {
            self.set_settings_notice("Ese proveedor no está conectado", true);
            return;
        }
        let mut next = self.review_model_settings.clone();
        let unchanged = next.manual.as_ref().is_some_and(|manual| manual.provider_id == provider_id);
        if !unchanged {
            next.manual = Some(crate::native_provider::NativeProviderSettings {
                provider_id: provider_id.to_owned(),
                model_id: String::new(),
                custom_endpoint: self.view.endpoint.clone(),
            });
        }
        self.commit_review_model(next, "Proveedor de revisión elegido; falta elegir el modelo");
    }

    /// Picks the manual review model within the provider already chosen.
    pub(super) fn pick_review_model(&mut self, model_id: &str) {
        let choices = crate::settings_ui::connected_model_choices(&self.view);
        let mut next = self.review_model_settings.clone();
        let usable = next.manual.as_ref().is_some_and(|manual| {
            choices.iter().any(|provider| {
                provider.id == manual.provider_id && provider.models.iter().any(|(id, _)| id == model_id)
            })
        });
        if !usable {
            self.set_settings_notice("Ese modelo no está disponible en el proveedor elegido", true);
            return;
        }
        if let Some(manual) = &mut next.manual {
            manual.model_id = model_id.to_owned();
        }
        self.commit_review_model(next, "Modelo manual de revisión guardado");
    }

    /// Saves the model selected in the composer as the manual review model.
    /// It was usable when the user selected it, which is the validation.
    pub(super) fn capture_review_model(&mut self) {
        let selected = self.native_settings();
        if selected.provider_id.is_empty() || selected.model_id.is_empty() {
            self.set_settings_notice("Seleccioná un modelo en el composer primero", true);
            return;
        }
        let mut next = self.review_model_settings.clone();
        next.manual = Some(selected);
        self.commit_review_model(next, "Modelo manual de revisión guardado");
    }

    /// Puts the failure in the composer for the user to send. An existing
    /// draft is kept and the request is added after it.
    pub(super) fn load_turn_check_fix(&mut self) {
        if let Some(failure) = self.view.turn_check_failure.take() {
            let prompt = crate::turn_check::merge_into_draft(&self.view.prompt, &failure);
            self.view.prompt = prompt.clone();
            self.tree.set_input_value("prompt", prompt);
        }
    }

    pub(super) fn set_turn_check_command(&mut self, command: &str) {
        let mut next = self.turn_check_settings.clone();
        next.set(&self.view.workspace, command);
        match next.save(&self.data_root) {
            Ok(()) => {
                self.turn_check_settings = next;
                self.refresh_turn_check_command();
                self.set_settings_notice(
                    if command.trim().is_empty() {
                        "Chequeo del proyecto desactivado"
                    } else {
                        "Chequeo del proyecto guardado; corre tras cada turno que cambie algo"
                    },
                    false,
                );
            }
            Err(error) => self.set_settings_notice(error, true),
        }
    }

    pub(super) fn refresh_turn_check_command(&mut self) {
        self.view.turn_check_command = self
            .turn_check_settings
            .command(&self.view.workspace)
            .unwrap_or_default()
            .to_owned();
    }

    /// Shows the oldest live request. Returns whether the view changed.
    pub(super) fn sync_external_approval(&mut self, ctx: &egui::Context) -> bool {
        locked(&self.external_bridge.repaint).get_or_insert_with(|| ctx.clone());
        let mut approvals = locked(&self.external_bridge.approvals);
        let now = Instant::now();
        approvals.retain(|pending| pending.expires > now);
        let next = approvals.front().map(|pending| crate::view::ExternalApprovalView {
            title: pending.title.clone(),
            detail: pending.detail.clone(),
        });
        if let Some(pending) = approvals.front() {
            ctx.request_repaint_after(pending.expires - now);
        }
        drop(approvals);
        let changed = self.view.external_approval != next;
        let settings = self.view.notifications.settings;
        if let Some(pending) = next.as_ref().filter(|_| {
            changed
                && !self.window_focused
                && settings.allows(crate::notifications::Event::Approval)
        }) {
            if settings.uses_system() {
                crate::notifications::notify_system(
                    "Una CLI externa espera aprobación",
                    &pending.title,
                );
            }
            self.view.notifications.push(
                crate::notifications::Severity::Attention,
                "Una CLI externa espera aprobación",
                pending.title.clone(),
                None,
            );
        }
        self.view.external_approval = next;
        changed
    }

    pub(super) fn answer_external_approval(&mut self, allow: bool) {
        if let Some(pending) = locked(&self.external_bridge.approvals).pop_front() {
            let _ = pending.reply.send(allow);
        }
        self.view.external_approval = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clicked(id: &str) -> UiAction {
        UiAction::Clicked { id: id.into() }
    }

    #[test]
    fn bridge_policy_follows_the_settings_page_and_denies_unknown_groups() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        let policy = app.external_bridge.host().policy;
        assert_eq!(policy("browser", true), PermissionMode::Deny);

        app.apply_action(clicked("external_agents_enabled"), &ctx);
        assert_eq!(policy("browser", true), PermissionMode::Allow);
        assert_eq!(policy("browser", false), PermissionMode::Ask);
        assert_eq!(policy("git_review", false), PermissionMode::Allow);
        assert_eq!(policy("conversations", true), PermissionMode::Deny);
        assert_eq!(policy("shell", true), PermissionMode::Deny);

        app.apply_action(clicked("external_permission:always_ask"), &ctx);
        assert_eq!(policy("browser", true), PermissionMode::Ask);
    }

    #[test]
    fn approval_requests_reach_the_window_and_carry_the_users_answer() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        let host = app.external_bridge.host();
        let approver = host.approver.unwrap();
        let grant = Grant { workspace: root.path().to_path_buf(), cli_accepts_all: false, session: "cli".into() };
        for (action, expected) in [("external_approval_allow", true), ("external_approval_deny", false)] {
            let answer = approver(&host.tools[0], &serde_json::json!({ "a": 1 }), &grant);
            assert!(app.sync_external_approval(&ctx));
            let shown = app.view.external_approval.clone().unwrap();
            assert!(shown.title.contains("review_changes"));
            assert!(shown.detail.contains(r#"{"a":1}"#));
            app.apply_action(clicked(action), &ctx);
            assert_eq!(answer.recv().unwrap(), expected);
            assert!(app.view.external_approval.is_none());
        }
        // An answer with nothing pending must not invent a decision.
        app.apply_action(clicked("external_approval_allow"), &ctx);
        assert!(!app.sync_external_approval(&ctx));
    }

    fn wait_for_check(app: &mut AgentsWindow) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while app.turn_check_run.is_some() && Instant::now() < deadline {
            app.poll_turn_check();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.turn_check_run.is_none(), "check did not finish");
    }

    #[cfg(unix)]
    #[test]
    fn failing_check_shows_a_notice_and_fixing_adds_to_the_draft_without_sending() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        app.view.workspace = root.path().display().to_string();
        app.view.selected_session = Some("s1".into());

        // No command saved: a turn with effects checks nothing.
        app.turn_had_effects = true;
        app.start_turn_check();
        assert!(app.turn_check_run.is_none() && !app.turn_had_effects);

        app.apply_action(
            UiAction::Submitted { id: "turn_check_command".into(), value: "echo 'error: 2 tests failed' >&2; exit 1".into() },
            &ctx,
        );
        assert!(!app.view.settings_notice_error);

        // A turn that only read is not checked.
        app.start_turn_check();
        assert!(app.turn_check_run.is_none());

        app.turn_had_effects = true;
        app.start_turn_check();
        wait_for_check(&mut app);
        let failure = app.view.turn_check_failure.clone().unwrap();
        assert!(failure.contains("error: 2 tests failed") && failure.contains("código 1"));

        // The draft survives; nothing is running and nothing was sent.
        app.view.prompt = "mi borrador".into();
        app.apply_action(clicked("turn_check_fix"), &ctx);
        assert!(app.view.prompt.starts_with("mi borrador\n\n"));
        assert!(app.view.prompt.contains("error: 2 tests failed"));
        assert!(app.view.turn_check_failure.is_none() && !app.view.running && app.active_run.is_none());

        // A passing check leaves no notice, and one can be dismissed.
        app.apply_action(
            UiAction::Submitted { id: "turn_check_command".into(), value: "true".into() },
            &ctx,
        );
        app.turn_had_effects = true;
        app.start_turn_check();
        wait_for_check(&mut app);
        assert!(app.view.turn_check_failure.is_none());
        app.view.turn_check_failure = Some("x".into());
        app.apply_action(clicked("turn_check_dismiss"), &ctx);
        assert!(app.view.turn_check_failure.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_check_result_for_a_conversation_the_user_left_is_dropped() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        app.view.workspace = root.path().display().to_string();
        app.view.selected_session = Some("s1".into());
        app.apply_action(
            UiAction::Submitted { id: "turn_check_command".into(), value: "exit 1".into() },
            &ctx,
        );
        app.turn_had_effects = true;
        app.start_turn_check();
        app.view.selected_session = Some("other".into());
        wait_for_check(&mut app);
        assert!(app.view.turn_check_failure.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn review_tool_returns_the_real_diff_of_staged_unstaged_and_untracked_changes() {
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(root.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(root.path().join("a.rs"), "one\n").unwrap();
        std::fs::write(root.path().join("b.rs"), "one\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "base"]);
        std::fs::write(root.path().join("a.rs"), "LINEA_PREPARADA\n").unwrap();
        git(&["add", "a.rs"]);
        std::fs::write(root.path().join("b.rs"), "LINEA_SIN_PREPARAR\n").unwrap();
        std::fs::write(root.path().join("nuevo.rs"), "x\n").unwrap();

        let grant = Grant { workspace: root.path().to_path_buf(), cli_accepts_all: false, session: "cli".into() };
        let text = review_changes(&grant).unwrap();
        for part in ["+LINEA_PREPARADA", "+LINEA_SIN_PREPARAR", "-one", "a.rs", "b.rs", "nuevo.rs"] {
            assert!(text.contains(part), "{part} missing in:\n{text}");
        }
    }

    /// The rule shared by a turn with its own model, a resumed turn, a
    /// recovered subagent and a compaction. Everything here is local files.
    #[test]
    fn a_codex_account_with_an_expired_token_is_still_ready_and_a_missing_key_is_not() {
        let root = tempfile::tempdir().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let settings = |provider: &str| NativeProviderSettings {
            provider_id: provider.into(),
            model_id: "any-model".into(),
            custom_endpoint: "http://127.0.0.1:9/v1".into(),
        };
        let ready = |provider: &str| {
            let connection = settings(provider).resolve_at(root.path()).unwrap();
            (connection.api_key.is_some(), provider_ready_at(root.path(), provider, &connection))
        };
        let codex = |expires_at: u64, disabled: bool| openide_connections::Credential {
            version: 1,
            provider_id: "codex".into(),
            access_token: "a".repeat(40),
            refresh_token: Some("r".repeat(40)),
            client_id: Some("client".into()),
            subject: Some("user".into()),
            expires_at: Some(expires_at),
            scopes: vec!["chatgpt.tokens.use.direct".into()],
            oauth: true,
            disabled,
            ..Default::default()
        };

        // No account at all: not ready.
        assert_eq!(ready("codex"), (false, false));

        // A valid token: a key is visible and the provider is ready.
        openide_connections::save(root.path(), &codex(now + 3600, false)).unwrap();
        assert_eq!(ready("codex"), (true, true));

        // The token expired an hour ago. The stored key reads as absent, which
        // is what the old check took for "not connected", but the account is
        // connected and renews itself on the next request.
        openide_connections::save(root.path(), &codex(now - 3600, false)).unwrap();
        assert_eq!(ready("codex"), (false, true));

        // An account the user disconnected is not ready, expired or not.
        openide_connections::save(root.path(), &codex(now + 3600, true)).unwrap();
        assert_eq!(ready("codex").1, false);

        // Other providers still need their key; a custom endpoint never does.
        assert_eq!(ready("openai"), (false, false));
        assert_eq!(ready("custom").1, true);
    }

    #[test]
    fn closing_removes_every_grant_and_its_token_file() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let file = root.path().join("claude-mcp-test.json");
        std::fs::write(&file, "{}").unwrap();
        app.external_bridge.grants.insert(7, ("token".into(), Some(file.clone()), "cli".into()));
        app.revoke_all_external_grants();
        assert!(app.external_bridge.grants.is_empty());
        assert!(!file.exists());
    }

    #[test]
    fn claude_hooks_are_listed_off_and_join_a_run_only_after_the_user_enables_one() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("claude-settings.json");
        std::fs::write(
            &source,
            r#"{"hooks":{
                "PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"exit 0"}]}],
                "SubagentStart":[{"hooks":[{"type":"command","command":"exit 0"}]}]
            }}"#,
        )
        .unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        let ctx = egui::Context::default();
        assert!(app.compat_hook_source.is_none(), "tests must not read the real settings");
        app.compat_hook_source = Some(source.clone());
        let registry = |app: &AgentsWindow| {
            crate::native_hook_settings::NativeHookSettings::load(&app.data_root)
                .unwrap()
                .registry_for(&app.data_root, root.path(), app.compat_hook_source.as_deref())
                .unwrap()
        };
        let plain_digest = registry(&app).digest_hex().unwrap();

        app.view.show_settings = true;
        app.apply_action(clicked(SettingsPage::Hooks.action_id()), &ctx);
        let rows = app.view.compat_hooks.clone();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| !row.enabled && row.runs_today));

        app.apply_action(clicked("compat_hook:0"), &ctx);
        assert!(app.view.compat_hooks[0].enabled);
        if cfg!(unix) {
            assert_ne!(registry(&app).digest_hex().unwrap(), plain_digest);
        }

        // Editing the command at its source drops it from the next run.
        std::fs::write(
            &source,
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"exit 2"}]}]}}"#,
        )
        .unwrap();
        assert_eq!(registry(&app).digest_hex().unwrap(), plain_digest);
        app.apply_action(clicked("compat_hook:0"), &ctx);
        assert!(app.view.settings_notice_error, "a stale row must not enable the new command");
        assert!(!app.view.compat_hooks[0].enabled);
    }

    #[cfg(unix)]
    #[test]
    fn agent_terminal_uses_the_manual_path_and_connects_only_what_settings_allow() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("my-claude");
        std::fs::write(&binary, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        app.view.workspace = root.path().display().to_string();
        let ctx = egui::Context::default();
        *locked(&app.login_path) = Some("/nonexistent-dir".into());

        // Unknown terminals are ordinary shells; an undetected CLI is an error, not a shell.
        assert!(app.agent_terminal_command(1).is_none());
        let claude = agent_cli_definitions::profile("claude_accept_all").unwrap();
        app.mark_agent_terminal(1, claude);
        assert!(app.agent_terminal_command(1).unwrap().is_err());

        app.apply_action(
            UiAction::Submitted { id: "agent_cli_path:claude".into(), value: binary.display().to_string() },
            &ctx,
        );
        assert_eq!(app.view.agent_clis[0].resolved.as_deref(), Some(binary.to_str().unwrap()));

        // Bridge closed: the CLI starts with its own arguments and no token.
        let (program, args, env) = app.agent_terminal_command(1).unwrap().unwrap();
        assert_eq!(program, binary.as_os_str());
        assert_eq!(args, vec![OsString::from("--dangerously-skip-permissions")]);
        assert_eq!(env, vec![("PATH", "/nonexistent-dir".to_owned())]);
        assert!(app.external_bridge.grants.is_empty());

        // Bridge open: a private config file is passed and tracked for cleanup.
        app.apply_action(clicked("external_agents_enabled"), &ctx);
        let (_, args, _) = app.agent_terminal_command(1).unwrap().unwrap();
        assert_eq!(args[1], "--mcp-config");
        let (_, config, _) = app.external_bridge.grants.get(&1).unwrap().clone();
        let config = config.unwrap();
        assert!(config.is_file() && args[2] == config.as_os_str());
        // Restarting replaces the grant; leaving removes the file.
        app.agent_terminal_command(1).unwrap().unwrap();
        assert!(!config.exists());
        app.revoke_all_external_grants();
        assert!(app.external_bridge.grants.is_empty());

        app.apply_action(
            UiAction::Submitted { id: "agent_cli_path:claude".into(), value: "relative".into() },
            &ctx,
        );
        assert!(app.view.settings_notice_error);
        assert_eq!(app.agent_cli_paths.get("claude"), Some(binary.as_path()));
    }
}

// Marks a script executable and calls the bridge on its loopback socket.
#[cfg(all(test, unix))]
#[path = "external_bridge_browser_tests.rs"]
mod browser_tests;
