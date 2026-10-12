//! Real, harmless processes (`/bin/sh` in a temp workspace) started through
//! the same command manager the native agent uses. Every one is stopped.
#![cfg(unix)]
use super::*;
use crate::terminal::TerminalColor;
use openide_harness::{PersistentCommandEvent, PersistentCommandManager};

struct Fixture {
    _root: tempfile::TempDir,
    app: AgentsWindow,
    workspace: PathBuf,
    session: String,
    manager: Arc<PersistentCommandManager>,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    // A user terminal opened by a test must never start the owner's shell:
    // a path that can be saved with the preferences and names no program.
    app.settings_preferences.terminal_shell =
        TerminalShell::Custom("/nonexistent/openade-fixture-shell".into());
    let session = app
        .store
        .as_ref()
        .unwrap()
        .create(&workspace, "native")
        .unwrap()
        .id;
    app.view.workspace = workspace.to_string_lossy().into_owned();
    app.view.selected_session = Some(session.clone());
    app.view.selected_backend = "native".into();
    app.view.main_page = MainPage::Workspace;
    app.view.show_settings = false;
    let manager = app
        .native_command_sessions
        .for_session(&session, &workspace, CommandIsolation::HostUnrestricted, true)
        .unwrap()
        .unwrap();
    Fixture {
        _root: root,
        app,
        workspace,
        session,
        manager,
    }
}

impl Fixture {
    /// What the agent's `start_command` does, then one frame of the window.
    fn start(&mut self, script: &str) -> (String, u64) {
        let started = self
            .manager
            .start(
                "/bin/sh",
                &["-c".into(), script.into()],
                &CancellationToken::new(),
            )
            .unwrap();
        self.app.follow_agent_terminals();
        let tab = self
            .app
            .agent_terminals
            .iter()
            .find(|(_, agent)| agent.command_id == started.id)
            .map(|(id, _)| *id)
            .expect("the started command has a tab");
        (started.id, tab)
    }

    fn screen(&mut self, tab: u64) -> String {
        let session = self.app.terminals.get_mut(&tab).unwrap();
        session.poll();
        session.snapshot().lines.join("\n")
    }

    fn wait(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn view(&self, tab: u64) -> &crate::terminal_dock_ui::AgentTerminalView {
        crate::terminal_dock_ui::agent_terminal(&self.app.view, tab).expect("an agent tab")
    }

    fn click(&mut self, id: impl Into<String>) {
        self.app.apply_action(
            UiAction::Clicked { id: id.into() },
            &egui::Context::default(),
        );
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the test's own child still exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test]
async fn a_started_command_is_an_interactive_tab_and_the_agent_still_polls_plain_text() {
    let mut fixture = fixture();
    let (command, tab) = fixture.start(
        r"printf '\033[32mLISTO\033[0m\n'; read line; echo GOT:$line; exit 3",
    );
    // The tab is the conversation's, named by the command and marked as the
    // agent's; it is revealed without taking the keyboard.
    assert!(fixture.app.view.show_terminal_panel);
    assert!(!fixture.app.terminal_focus_pending);
    let shown = fixture.app.view.terminal_tabs.clone();
    assert_eq!(shown.len(), 1);
    assert!(shown[0].active && shown[0].title.starts_with("/bin/sh -c printf"));
    assert_eq!(fixture.view(tab).command_id, command);
    assert!(!fixture.view(tab).read_only && fixture.view(tab).ended.is_none());
    // Colour reaches the screen as colour, like in a user terminal.
    fixture.wait("the first line", |fixture| fixture.screen(tab).contains("LISTO"));
    let snapshot = fixture.app.terminals[&tab].snapshot();
    assert!(snapshot.running);
    assert!(snapshot.styled_lines.iter().flatten().any(|span| {
        span.text.contains("LISTO") && span.style.foreground == TerminalColor::Indexed(2)
    }));
    // What the user types in the tab reaches the process.
    fixture.app.terminal_write(b"hola\r");
    fixture.wait("the typed line", |fixture| fixture.screen(tab).contains("GOT:hola"));
    fixture.wait("the exit", |fixture| {
        fixture.app.follow_agent_terminals();
        fixture.view(tab).ended.is_some()
    });
    assert_eq!(
        fixture.view(tab).ended.as_deref(),
        Some("Proceso finalizado · código 3")
    );
    fixture.screen(tab);
    assert!(!fixture.app.terminals[&tab].snapshot().running);
    // The tab never consumed the agent's output, which has no escape sequences.
    let mut polled = String::new();
    loop {
        let snapshot = fixture
            .manager
            .poll(&command, Duration::from_secs(5), &CancellationToken::new())
            .await
            .unwrap();
        for event in &snapshot.events {
            if let PersistentCommandEvent::Output { bytes, .. } = event {
                polled.push_str(&String::from_utf8_lossy(bytes));
            }
        }
        if snapshot.finished {
            break;
        }
    }
    assert!(polled.starts_with("LISTO\n"), "{polled:?}");
    assert!(polled.contains("GOT:hola\n") && !polled.contains('\u{1b}'), "{polled:?}");
    // It stays with its exit status until the user closes it, without asking.
    assert_eq!(fixture.app.view.terminal_tabs.len(), 1);
    fixture.click(format!("terminal_close:{tab}"));
    assert!(fixture.app.view.terminal_controls.closing.is_none());
    assert!(fixture.app.view.terminal_tabs.is_empty());
    assert!(fixture.app.agent_terminals.is_empty() && fixture.app.terminals.is_empty());
}

#[test]
fn closing_a_running_tab_asks_and_then_stops_the_whole_tree() {
    let mut fixture = fixture();
    let (command, tab) = fixture.start("sleep 300 & echo CHILD=$!; wait");
    fixture.wait("the child's pid", |fixture| {
        fixture.screen(tab).contains("CHILD=") && fixture.screen(tab).lines().count() > 1
    });
    let pid: i32 = fixture
        .screen(tab)
        .lines()
        .find_map(|line| line.trim().strip_prefix("CHILD=")?.parse().ok())
        .unwrap();
    fixture.click(format!("terminal_close:{tab}"));
    assert_eq!(fixture.app.view.terminal_controls.closing, Some(tab));
    assert!(crate::view::dialog_open(&fixture.app.view, 900.0));
    assert!(alive(pid) && fixture.app.view.terminal_tabs.len() == 1);
    // Backing out leaves the process and its tab alone.
    fixture.click("agent_terminal_stop_cancel");
    assert!(fixture.app.view.terminal_controls.closing.is_none());
    assert!(!crate::view::dialog_open(&fixture.app.view, 900.0));
    assert!(alive(pid) && fixture.app.view.terminal_tabs.len() == 1);
    // A confirmation nobody was asked for does nothing.
    fixture.click(format!("agent_terminal_stop:{tab}"));
    assert!(alive(pid) && fixture.app.view.terminal_tabs.len() == 1);

    fixture.click(format!("terminal_close:{tab}"));
    fixture.click(format!("agent_terminal_stop:{tab}"));
    assert!(fixture.app.view.terminal_tabs.is_empty());
    assert!(fixture.app.agent_terminals.is_empty() && fixture.app.terminals.is_empty());
    fixture.wait("the descendant to die", |_| !alive(pid));
    // The agent still finds its command, stopped.
    fixture.wait("the recorded exit", |fixture| {
        fixture.manager.list_sessions().unwrap().iter().any(|info| {
            info.id == command && info.exit == Some(PersistentCommandExit::Terminated)
        })
    });
}

#[test]
fn a_pipe_backed_command_is_a_read_only_mirror_that_says_so() {
    let mut fixture = fixture();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    // What the contained profile hands the host: a command on pipes.
    let pipes = PersistentCommandManager::new(
        fixture.workspace.clone(),
        CommandIsolation::HostUnrestricted,
        Default::default(),
    )
    .unwrap();
    let started = pipes
        .start(
            "/bin/sh",
            &[
                "-c".into(),
                format!(
                    "echo UNO; echo DOS >&2; echo SIRVE http://127.0.0.1:{port}/; read line; echo GOT:$line"
                ),
            ],
            &CancellationToken::new(),
        )
        .unwrap();
    let launch = openide_harness::PersistentCommandLaunch {
        owner_session_id: Some(fixture.session.clone()),
        generation: pipes.generation().into(),
        id: started.id.clone(),
        argv: vec!["/bin/sh".into()],
        isolation: CommandIsolation::WorkspaceWriteOffline,
        terminal: pipes.attach_terminal(&started.id).unwrap(),
    };
    assert!(fixture
        .app
        .open_agent_terminal(StartedCommand::attach(launch).unwrap()));
    let tab = fixture.app.view.terminal_tabs[0].id;
    assert!(fixture.view(tab).read_only);
    fixture.wait("both streams", |fixture| {
        let screen = fixture.screen(tab);
        screen.contains("UNO") && screen.contains("DOS") && screen.contains("SIRVE")
    });
    // Each line starts at the first column although pipes send no returns.
    let screen = fixture.screen(tab);
    for line in ["UNO", "DOS"] {
        assert!(screen.lines().any(|shown| shown == line), "{screen:?}");
    }
    // A contained command has no network of the host's: an address it prints
    // that answers is another program's, and is neither recorded nor opened.
    for _ in 0..60 {
        fixture.app.follow_agent_terminals();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(fixture.app.agent_terminals[&tab].preview.is_none());
    assert!(fixture.browser_tabs().is_empty() && fixture.notices().is_empty());
    drop(listener);
    // Keys and paste go nowhere, without an error.
    fixture.app.terminal_write(b"hola\r");
    fixture.app.terminals.get_mut(&tab).unwrap().paste("hola\n").unwrap();
    assert!(fixture.app.view.terminal_error.is_none());
    std::thread::sleep(Duration::from_millis(150));
    assert!(!fixture.screen(tab).contains("GOT"));
    assert!(pipes.list_sessions().unwrap()[0].exit.is_none());
    fixture.app.view.terminal_snapshot = Some(fixture.app.terminals[&tab].snapshot());
    let mut tree = openide_ui::UiTree::new(crate::terminal_dock_ui::build(&fixture.app.view, 640.0, 300.0));
    let frame = tree.layout(Size::new(640.0, 300.0));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        openide_ui::DrawCommand::Text { text, .. } if text == crate::terminal_dock_ui::AGENT_READ_ONLY
    )));
    pipes.stop_all();
}

#[test]
fn a_new_command_never_takes_the_terminal_the_user_is_typing_in() {
    let mut fixture = fixture();
    fixture.click("terminal_new");
    let own = fixture.app.view.terminal_tabs[0].id;
    let present = |app: &mut AgentsWindow| {
        app.tree.set_root(Element::column(vec![
            crate::terminal_dock_ui::build(&app.view, 640.0, 300.0),
            Element::text_input("prompt", "", "Task").size(None, Some(36.0)),
        ]));
        app.tree.layout(Size::new(640.0, 336.0));
    };
    present(&mut fixture.app);
    assert!(fixture.app.tree.focus(&format!("terminal_tab:{own}")));
    let (_, first) = fixture.start("echo UNO; sleep 300");
    // Its tab is listed, but the one under the user's hands stays in front.
    assert_eq!(fixture.app.active_terminal_id(), Some(own));
    assert_eq!(fixture.app.view.terminal_tabs.len(), 2);
    assert!(fixture.app.view.terminal_tabs.iter().any(|tab| tab.id == first && !tab.active));
    // From the composer it comes to the front and the composer keeps the keys.
    present(&mut fixture.app);
    assert!(fixture.app.tree.focus("prompt"));
    fixture.app.terminal_focus_pending = false;
    let (second_command, second) = fixture.start("echo DOS; sleep 300");
    assert_eq!(fixture.app.active_terminal_id(), Some(second));
    assert!(!fixture.app.terminal_focus_pending);
    assert_eq!(fixture.app.tree.focused_id(), Some("prompt"));
    // The command's card leads to its tab, and that does take the keyboard.
    let result = serde_json::json!({ "session_id": second_command }).to_string();
    let offered = |app: &AgentsWindow| {
        crate::terminal_dock_ui::command_actions(
            &app.view,
            "card",
            "call-1",
            "start_command",
            Some(&result),
        )
        .is_some()
    };
    assert!(offered(&fixture.app));
    fixture.click(format!("terminal_tab:{own}"));
    fixture.app.terminal_focus_pending = false;
    fixture.click(format!("agent_terminal_show:{second_command}"));
    assert_eq!(fixture.app.active_terminal_id(), Some(second));
    assert!(fixture.app.terminal_focus_pending && fixture.app.view.show_terminal_panel);
    // A restart is the agent's to do: the tab keeps its command, never a shell.
    fixture.click("terminal_restart");
    assert!(fixture.app.is_agent_terminal(second) && fixture.app.terminals.contains_key(&second));
    for tab in [first, second] {
        fixture.click(format!("terminal_close:{tab}"));
        fixture.click(format!("agent_terminal_stop:{tab}"));
    }
    assert!(!offered(&fixture.app));
    assert_eq!(fixture.app.view.terminal_tabs.len(), 1);
}

#[test]
fn tabs_follow_their_conversation_and_a_revocation_marks_them() {
    let mut fixture = fixture();
    let other = fixture
        .app
        .store
        .as_ref()
        .unwrap()
        .create(&fixture.workspace, "native")
        .unwrap()
        .id;
    // Started while another conversation is on screen: nothing changes there.
    fixture.app.view.selected_session = Some(other.clone());
    fixture.app.sync_dock_view();
    let started = fixture
        .manager
        .start(
            "/bin/sh",
            &["-c".into(), "echo SERVIDOR; sleep 300".into()],
            &CancellationToken::new(),
        )
        .unwrap();
    assert!(!fixture.app.follow_agent_terminals());
    assert!(fixture.app.view.terminal_tabs.is_empty());
    assert!(fixture.app.view.terminal_controls.agents.is_empty());
    assert!(!fixture.app.view.show_terminal_panel);
    // Its own conversation shows it, without handing it the keyboard.
    fixture.app.view.selected_session = Some(fixture.session.clone());
    fixture.app.sync_dock_view();
    let tab = fixture.app.view.terminal_tabs[0].id;
    assert_eq!(fixture.view(tab).command_id, started.id);
    assert!(fixture.app.view.show_terminal_panel && !fixture.app.terminal_focus_pending);
    fixture.wait("the output", |fixture| fixture.screen(tab).contains("SERVIDOR"));
    // Restricting command access stops it; the tab stays and says why.
    let session = fixture.session.clone();
    fixture.app.native_command_sessions.revoke_session(&session);
    fixture.wait("the revocation", |fixture| {
        fixture.app.follow_agent_terminals();
        fixture.view(tab).ended.is_some()
    });
    assert_eq!(
        fixture.view(tab).ended.as_deref(),
        Some("Detenido · se revocaron los comandos de esta conversación")
    );
    assert_eq!(fixture.app.view.terminal_tabs.len(), 1);
}

// ---- Live preview ----------------------------------------------------------
// The "server" is a listener of the test on a loopback port and a shell that
// prints its address the way a dev server does. No browser starts: a tab's
// context only starts from a painted frame, which these tests never run.

struct Server {
    _listener: std::net::TcpListener,
    url: String,
    command: String,
    tab: u64,
}

impl Fixture {
    fn serve(&mut self) -> Server {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
        // Colour around the address, as Vite prints it.
        let (command, tab) = self.start(&format!(
            r"printf '  VITE ready\n  Local:   \033[36m{url}\033[39m\n'; sleep 300"
        ));
        Server {
            _listener: listener,
            url,
            command,
            tab,
        }
    }

    /// Frames of the window until the command's address is known to answer.
    fn answered(&mut self, server: &Server) {
        let tab = server.tab;
        self.wait("the server's address", |fixture| {
            fixture.app.follow_agent_terminals();
            fixture.app.agent_terminals[&tab].preview.is_some()
        });
        assert_eq!(
            self.app.agent_terminals[&tab].preview.as_deref(),
            Some(server.url.as_str())
        );
    }

    fn scope(&self) -> DockScope {
        self.app.dock_scope()
    }

    fn browser_tabs(&self) -> Vec<crate::dock::DockTab> {
        self.app
            .dock
            .tabs(&self.scope().workspace_id)
            .iter()
            .filter(|tab| tab.kind == DockTabKind::Browser)
            .cloned()
            .collect()
    }

    /// A window with the composer and one other field, `focused` among them.
    fn present_with_focus(&mut self, focused: &str) {
        self.app.tree.set_root(Element::column(vec![
            Element::text_input("prompt", "", "Task").size(None, Some(36.0)),
            Element::text_input("another_field", "", "Buscar").size(None, Some(36.0)),
        ]));
        self.app.tree.layout(Size::new(640.0, 200.0));
        assert!(self.app.tree.focus(focused));
    }

    fn notices(&self) -> Vec<String> {
        self.app
            .view
            .notifications
            .items()
            .iter()
            .map(|item| item.title.clone())
            .collect()
    }

    fn stop(&mut self, server: &Server) {
        self.click(format!("terminal_close:{}", server.tab));
        self.click(format!("agent_terminal_stop:{}", server.tab));
    }
}

#[test]
fn the_preview_opens_once_when_the_server_answers_and_the_composer_keeps_the_keyboard() {
    let mut fixture = fixture();
    assert_eq!(fixture.app.view.preview_opening, crate::preview::PreviewOpening::Auto);
    fixture.present_with_focus("prompt");
    let server = fixture.serve();
    fixture.answered(&server);
    // The conversation's browser tab, in front, waiting on that address.
    let [browser] = fixture.browser_tabs().try_into().unwrap();
    let scope = fixture.scope();
    assert_eq!(fixture.app.dock.active_tab(&scope).map(|tab| tab.id), Some(browser.id));
    assert!(fixture.app.view.show_side_pane && fixture.app.view.side_tab == SideTab::Browser);
    assert_eq!(
        fixture.app.browser_restart_urls.get(&browser.id.get()),
        Some(&server.url)
    );
    assert_eq!(fixture.app.view.browser.url_draft, server.url);
    assert_eq!(fixture.app.tree.focused_id(), Some("prompt"));
    assert_eq!(fixture.notices(), [format!("Vista previa abierta: {}", server.url)]);
    // Never a second time for the same command, whatever the window does.
    fixture.app.browser_restart_urls.clear();
    for _ in 0..30 {
        fixture.app.follow_agent_terminals();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(fixture.app.browser_restart_urls.is_empty());
    assert_eq!(fixture.notices().len(), 1);
    assert_eq!(fixture.app.view.notifications.items()[0].count, 1);
    // Another server of the conversation reuses the tab.
    let second = fixture.serve();
    fixture.answered(&second);
    assert_eq!(fixture.browser_tabs().len(), 1);
    assert_eq!(
        fixture.app.browser_restart_urls.get(&browser.id.get()),
        Some(&second.url)
    );
    // Only an address of this machine is ever opened this way.
    assert!(!fixture.app.open_preview(&scope, "https://example.test/"));
    assert!(!fixture.app.open_preview(&scope, "http://192.168.1.10:5173/"));
    assert_eq!(
        fixture.app.browser_restart_urls.get(&browser.id.get()),
        Some(&second.url)
    );
    fixture.stop(&server);
    fixture.stop(&second);
}

#[test]
fn a_field_the_user_types_in_keeps_its_tab_in_front_of_the_preview() {
    let mut fixture = fixture();
    let scope = fixture.scope();
    let activity = fixture.app.dock.open(&scope, DockTabKind::Activity);
    fixture.app.sync_dock_view();
    fixture.present_with_focus("another_field");
    let server = fixture.serve();
    fixture.answered(&server);
    // The preview is there to switch to, and says so; nothing was covered.
    let [browser] = fixture.browser_tabs().try_into().unwrap();
    assert_eq!(fixture.app.dock.active_tab(&scope).map(|tab| tab.id), Some(activity));
    assert_eq!(fixture.app.tree.focused_id(), Some("another_field"));
    assert_eq!(
        fixture.app.browser_restart_urls.get(&browser.id.get()),
        Some(&server.url)
    );
    assert_eq!(fixture.notices().len(), 1);
    fixture.stop(&server);
}

#[test]
fn asking_offers_the_preview_on_the_card_and_never_opens_nothing() {
    let mut fixture = fixture();
    // The choice is made in Settings and saved with the other preferences.
    fixture.app.view.show_settings = true;
    fixture.click(SettingsPage::Browser.action_id());
    fixture.click("settings_choice_toggle:preview_opening");
    assert_eq!(
        fixture.app.view.settings_choice_menu_open.as_deref(),
        Some("preview_opening")
    );
    fixture.click("preview_opening:ask");
    assert_eq!(fixture.app.view.settings_choice_menu_open, None);
    assert_eq!(fixture.app.view.preview_opening, crate::preview::PreviewOpening::Ask);
    assert_eq!(
        crate::settings_preferences::SettingsPreferences::load(&fixture.app.data_root)
            .preview_opening,
        crate::preview::PreviewOpening::Ask
    );
    fixture.click("preview_opening:siempre");
    assert_eq!(fixture.app.view.preview_opening, crate::preview::PreviewOpening::Ask);
    fixture.app.view.show_settings = false;

    let server = fixture.serve();
    fixture.answered(&server);
    assert!(fixture.browser_tabs().is_empty());
    assert!(fixture.notices().is_empty());
    assert_eq!(fixture.view(server.tab).preview.as_deref(), Some(server.url.as_str()));
    let result = serde_json::json!({ "session_id": server.command }).to_string();
    let offered = |app: &AgentsWindow| {
        let card = crate::terminal_dock_ui::command_actions(
            &app.view,
            "card",
            "call-1",
            "start_command",
            Some(&result),
        )
        .expect("a started command's actions");
        let frame = openide_ui::UiTree::new(card).layout(Size::new(480.0, 40.0));
        frame
            .regions
            .iter()
            .any(|region| region.id == "agent_preview_link:card:call-1")
    };
    assert!(offered(&fixture.app));
    // The user asks: the conversation's browser tab opens on that address.
    fixture.click(format!("agent_preview_open:{}", server.command));
    let [browser] = fixture.browser_tabs().try_into().unwrap();
    assert_eq!(
        fixture.app.browser_restart_urls.get(&browser.id.get()),
        Some(&server.url)
    );
    assert!(fixture.notices().is_empty());

    // "Nunca": nothing opens and nothing is offered, for this server or the next.
    fixture.click("preview_opening:never");
    assert!(fixture.view(server.tab).preview.is_none());
    assert!(!offered(&fixture.app));
    fixture.app.browser_restart_urls.clear();
    let second = fixture.serve();
    fixture.answered(&second);
    assert!(fixture.view(second.tab).preview.is_none());
    assert!(fixture.app.browser_restart_urls.is_empty());
    assert_eq!(fixture.browser_tabs().len(), 1);
    fixture.click(format!("agent_preview_open:{}", second.command));
    assert_eq!(
        fixture.app.browser_restart_urls.get(&browser.id.get()),
        Some(&second.url),
        "a click the card no longer offers still only opens the recorded address"
    );
    // A server that stopped is not offered either.
    fixture.click("preview_opening:ask");
    assert!(fixture.view(second.tab).preview.is_some());
    let manager = fixture.manager.clone();
    manager.stop(&second.command).unwrap();
    fixture.wait("the stop", |fixture| {
        fixture.app.follow_agent_terminals();
        fixture.view(second.tab).ended.is_some()
    });
    assert!(fixture.view(second.tab).preview.is_none());
    fixture.stop(&server);
}

#[tokio::test]
async fn the_agents_open_preview_defaults_to_the_conversations_server_and_shows_its_tab() {
    use crate::browser_runtime::{BrowserCommand, BrowserScope, BrowserSnapshot};
    use openide_harness::McpToolClient;
    let mut fixture = fixture();
    fixture.click("preview_opening:ask");
    let scope = fixture.scope();
    // A browser tab whose context is a fixture: no browser, no network.
    let browser = fixture.app.dock.open(&scope, DockTabKind::Browser);
    let (runtime, commands) = crate::browser_runtime::tests::fixture(
        BrowserScope {
            workspace: fixture.workspace.canonicalize().unwrap(),
            session_id: fixture.session.clone(),
        },
        BrowserSnapshot {
            started: true,
            generation: 1,
            viewport_width: 640,
            viewport_height: 480,
            ..Default::default()
        },
    );
    let client = runtime.mcp_client();
    fixture.app.browsers.insert(browser.get(), runtime);
    let activity = fixture.app.dock.open(&scope, DockTabKind::Activity);
    fixture.app.sync_dock_view();
    assert_eq!(fixture.app.dock.active_tab(&scope).map(|tab| tab.id), Some(activity));

    let server = fixture.serve();
    fixture.answered(&server);
    fixture.app.follow_agent_terminals();
    let call = tokio::spawn(async move {
        client
            .call_tool(
                crate::browser_runtime::OPEN_PREVIEW,
                serde_json::json!({}),
                &CancellationToken::new(),
            )
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let navigated = loop {
        if let Some(command) = commands.finish_next() {
            break command;
        }
        assert!(Instant::now() < deadline, "open_preview reached no page");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert!(matches!(navigated, BrowserCommand::Navigate { url } if url == server.url));
    assert!(!call.await.unwrap().unwrap().is_error);
    // The window brings the tab the agent opened forward.
    assert!(fixture.app.follow_agent_terminals());
    assert_eq!(fixture.app.dock.active_tab(&scope).map(|tab| tab.id), Some(browser));
    assert!(!fixture.app.follow_agent_terminals(), "shown once");
    fixture.stop(&server);
}
