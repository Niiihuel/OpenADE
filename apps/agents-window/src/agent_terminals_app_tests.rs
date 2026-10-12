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
    // A user terminal opened by a test must never start the owner's shell.
    app.settings_preferences.terminal_shell =
        TerminalShell::Custom("fixture-shell-relative-invalid".into());
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
                "echo UNO; echo DOS >&2; read line; echo GOT:$line".into(),
            ],
            &CancellationToken::new(),
        )
        .unwrap();
    let launch = openide_harness::PersistentCommandLaunch {
        owner_session_id: Some(fixture.session.clone()),
        generation: pipes.generation().into(),
        id: started.id.clone(),
        argv: vec!["/bin/sh".into()],
        terminal: pipes.attach_terminal(&started.id).unwrap(),
    };
    assert!(fixture
        .app
        .open_agent_terminal(StartedCommand::attach(launch).unwrap()));
    let tab = fixture.app.view.terminal_tabs[0].id;
    assert!(fixture.view(tab).read_only);
    fixture.wait("both streams", |fixture| {
        let screen = fixture.screen(tab);
        screen.contains("UNO") && screen.contains("DOS")
    });
    // Each line starts at the first column although pipes send no returns.
    assert!(fixture
        .screen(tab)
        .lines()
        .all(|line| line.is_empty() || line == "UNO" || line == "DOS"));
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
