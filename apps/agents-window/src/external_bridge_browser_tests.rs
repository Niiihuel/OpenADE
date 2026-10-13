//! The browser an external CLI drives through the bridge. Calls travel the
//! real path: this process's loopback bridge, its token, the Settings policy
//! and the window's approval. The CLI is a script no test runs, and every
//! page is the in-memory browser fixture: no process, browser or network.
use super::*;
use crate::browser_runtime::tests::{fixture as page, BrowserCommands};
use crate::browser_runtime::{
    browser_mcp_tools, browser_tool_command, BrowserCommand, BrowserScope, BrowserSnapshot,
    BrowserToolOutput, ConversationBrowser,
};
use openide_harness::McpToolClient;
use serde_json::{json, Value};

const SWITCHED_OFF: &str = "Herramienta desactivada en Ajustes → CLIs externas.";

struct Fixture {
    _root: tempfile::TempDir,
    app: AgentsWindow,
    ctx: egui::Context,
    /// The pages of the CLI browsers, by tab: what a worker would answer.
    pages: HashMap<u64, BrowserCommands>,
    /// Every command that reached a CLI browser's page, and which.
    ran: Vec<(u64, BrowserCommand)>,
}

/// A window on one conversation with the bridge open and Claude Code found.
fn fixture() -> Fixture {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("my-claude");
    std::fs::write(&binary, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = workspace.canonicalize().unwrap().display().to_string();
    app.view.selected_session = Some("conversation".into());
    app.view.selected_backend = "native".into();
    app.view.main_page = MainPage::Workspace;
    app.view.show_settings = false;
    let ctx = egui::Context::default();
    *locked(&app.login_path) = Some("/nonexistent-dir".into());
    app.apply_action(
        UiAction::Submitted { id: "agent_cli_path:claude".into(), value: binary.display().to_string() },
        &ctx,
    );
    app.apply_action(clicked("external_agents_enabled"), &ctx);
    Fixture { _root: root, app, ctx, pages: HashMap::new(), ran: Vec::new() }
}

fn clicked(id: &str) -> UiAction {
    UiAction::Clicked { id: id.into() }
}

fn started() -> BrowserSnapshot {
    BrowserSnapshot { started: true, ..Default::default() }
}

/// One request over the bridge's loopback socket, as a CLI sends it.
fn post(url: &str, token: &str, method: &str, params: Value) -> reqwest::blocking::Response {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(url)
        .bearer_auth(token)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .send()
        .unwrap()
}

/// A tool call: its content blocks and whether it failed.
fn call(url: &str, token: &str, name: &str, arguments: Value) -> (Vec<Value>, bool) {
    let reply: Value = post(url, token, "tools/call", json!({ "name": name, "arguments": arguments }))
        .json()
        .unwrap();
    (
        reply["result"]["content"].as_array().unwrap().clone(),
        reply["result"]["isError"].as_bool().unwrap(),
    )
}

impl Fixture {
    /// A terminal tab of the conversation with the grant its CLI starts
    /// with. The panel stays hidden, so the script is never run.
    fn open_cli(&mut self, profile: &str) -> (u64, String) {
        let scope = self.app.dock_scope();
        let id = self.app.dock.new_terminal(&scope).get();
        self.app.dock.set_terminal_collapsed(&scope, true);
        self.app.mark_agent_terminal(id, agent_cli_definitions::profile(profile).unwrap());
        self.app.agent_terminal_command(id).unwrap().unwrap();
        (id, self.app.external_bridge.grants[&id].0.clone())
    }

    fn url(&self) -> String {
        self.app.external_bridge.server.as_ref().unwrap().1.clone()
    }

    /// The CLI browser tabs, as (terminal, tab).
    fn cli_tabs(&self) -> Vec<(u64, u64)> {
        self.app
            .dock
            .tabs(&self.app.view.workspace)
            .iter()
            .filter_map(|tab| match tab.kind {
                DockTabKind::CliBrowser { terminal } => Some((terminal, tab.id.get())),
                _ => None,
            })
            .collect()
    }

    fn cli_tab(&self, terminal: u64) -> u64 {
        let tabs = self.cli_tabs();
        let found: Vec<_> = tabs.iter().filter(|(owner, _)| *owner == terminal).collect();
        assert_eq!(found.len(), 1, "one browser tab for terminal {terminal}: {tabs:?}");
        found[0].1
    }

    /// Stands in for the browser the window would start for a CLI tab that
    /// has none: the fixture, owned by whoever the window would start it for.
    fn attach(&mut self) {
        for (_, tab) in self.cli_tabs() {
            if !self.app.browsers.contains_key(&tab) {
                let (runtime, commands) = page(self.app.browser_launch_scope(tab).unwrap(), started());
                self.app.browsers.insert(tab, runtime);
                self.pages.insert(tab, commands);
            }
        }
    }

    /// One frame of the window with every CLI page answering.
    fn frame(&mut self) {
        self.app.sync_external_approval(&self.ctx);
        self.app.serve_external_browser_calls(&self.ctx);
        self.attach();
        for (tab, page) in &self.pages {
            self.ran.extend(page.finish_next().map(|command| (*tab, command)));
        }
    }

    /// Runs `call` as a CLI would, while this thread is the window: `frame`
    /// is what happens in each of its frames until the call returns.
    fn during<T: Send>(
        &mut self,
        call: impl FnOnce() -> T + Send,
        mut frame: impl FnMut(&mut Self),
    ) -> T {
        std::thread::scope(|threads| {
            let call = threads.spawn(call);
            let deadline = Instant::now() + Duration::from_secs(25);
            while !call.is_finished() {
                assert!(Instant::now() < deadline, "the call never returned");
                frame(self);
                std::thread::sleep(Duration::from_millis(2));
            }
            call.join().unwrap()
        })
    }

    /// A call the window serves without anyone answering an approval.
    fn run(&mut self, token: &str, name: &str, arguments: Value) -> (Vec<Value>, bool) {
        let url = self.url();
        self.during(|| call(&url, token, name, arguments), Self::frame)
    }

    /// A call whose approval, if the window shows one, gets `answer`: an
    /// action of the dialog, or nothing until the request expires. Returns
    /// the result and how many times the window asked.
    fn run_answering(
        &mut self,
        token: &str,
        name: &str,
        arguments: Value,
        answer: Option<&str>,
    ) -> ((Vec<Value>, bool), usize) {
        let url = self.url();
        let (tabs, ran) = (self.cli_tabs().len(), self.ran.len());
        let mut asked = 0;
        let result = self.during(
            || call(&url, token, name, arguments),
            |fixture| {
                fixture.app.sync_external_approval(&fixture.ctx);
                if let Some(shown) = fixture.app.view.external_approval.clone() {
                    assert!(shown.title.contains(name), "{}", shown.title);
                    assert!(
                        fixture.cli_tabs().len() == tabs && fixture.ran.len() == ran,
                        "nothing starts or runs before the answer"
                    );
                    asked += 1;
                    match answer {
                        Some(action) => fixture.app.apply_action(clicked(action), &fixture.ctx),
                        // Nobody answers in time: the window drops the request.
                        None => {
                            locked(&fixture.app.external_bridge.approvals)[0].expires = Instant::now();
                            fixture.app.sync_external_approval(&fixture.ctx);
                        }
                    }
                }
                fixture.frame();
            },
        );
        (result, asked)
    }
}

#[test]
fn the_bridge_offers_the_native_browser_tools_and_refuses_what_their_validation_refuses() {
    let mut fixture = fixture();
    // The native agent's definitions as they are. `open_preview`, which
    // opens the server a native conversation started, is not among them.
    let offered: Vec<_> = fixture
        .app
        .external_bridge
        .host()
        .tools
        .into_iter()
        .filter(|tool| tool.group == "browser")
        .collect();
    let native = browser_mcp_tools();
    assert_eq!(offered.len(), 27);
    for (tool, native) in offered.iter().zip(&native) {
        assert_eq!(tool.name, native.name);
        assert_eq!(tool.input_schema, native.input_schema, "{}", tool.name);
        assert_eq!(Some(&tool.description), native.description.as_ref(), "{}", tool.name);
    }
    assert!(offered.iter().all(|tool| tool.name != crate::browser_runtime::OPEN_PREVIEW));

    let (_, token) = fixture.open_cli("claude_accept_all");
    for (name, arguments) in [
        ("browser_navigate", json!({ "url": "file:///etc/passwd" })),
        ("browser_navigate", json!({ "url": "javascript:alert(1)" })),
        ("browser_navigate", json!({ "url": "http://user:secret@localhost/" })),
        ("browser_click", json!({ "x": 5000, "y": 1 })),
        ("browser_click", json!({ "ref": "e1", "selector": "#a" })),
        ("browser_evaluate", json!({ "expression": "" })),
        ("browser_type", json!({ "text": "x".repeat(17 * 1024) })),
        ("browser_wait_for", json!({ "seconds": 60 })),
        ("browser_snapshot", json!({ "type": "navigate", "url": "http://localhost/" })),
        ("browser_snapshot", json!({ "session": "terminal:1:another" })),
        ("browser_reload", json!([])),
    ] {
        let refusal = browser_tool_command(name, arguments.clone()).unwrap_err();
        let (content, error) = fixture.run(&token, name, arguments);
        assert!(error, "{name}");
        assert_eq!(content, [json!({ "type": "text", "text": refusal })], "{name}");
    }
    // A refused call starts nothing.
    assert!(fixture.cli_tabs().is_empty() && fixture.app.browser_starts.is_empty());
    // A name outside the catalog is not a tool of the bridge at all.
    let reply: Value = post(&fixture.url(), &token, "tools/call", json!({ "name": "open_preview", "arguments": {} }))
        .json()
        .unwrap();
    assert_eq!(reply["error"]["code"], -32602);
    // A tool that takes no arguments may be called without them.
    assert!(!fixture.run(&token, "browser_reload", Value::Null).1);
    assert!(matches!(fixture.ran.as_slice(), [(_, BrowserCommand::Reload)]));
}

#[test]
fn with_the_browser_switch_off_its_tools_are_refused_and_no_browser_starts() {
    let mut fixture = fixture();
    let (_, token) = fixture.open_cli("claude_accept_all");
    let listed = |fixture: &Fixture| {
        let reply: Value = post(&fixture.url(), &token, "tools/list", Value::Null).json().unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap().clone();
        tools.iter().filter(|tool| tool["name"].as_str().unwrap().starts_with("browser_")).count()
    };
    let navigate = || json!({ "url": "http://localhost:3000/" });
    assert_eq!(listed(&fixture), 27);

    fixture.app.apply_action(clicked("external_tool:browser"), &fixture.ctx);
    assert!(!fixture.app.view.external_agents.allows(ExternalTool::Browser));
    // Not advertised, and refused when called all the same.
    assert_eq!(listed(&fixture), 0);
    let (content, error) = fixture.run(&token, "browser_navigate", navigate());
    assert!(error);
    assert_eq!(content[0]["text"], SWITCHED_OFF);
    assert!(locked(&fixture.app.external_bridge.browser_calls).is_empty());
    assert!(fixture.cli_tabs().is_empty() && fixture.app.browsers.is_empty());

    // Switched off while a call the bridge let through still waits for its
    // browser: the window starts none for it either.
    fixture.app.apply_action(clicked("external_tool:browser"), &fixture.ctx);
    let url = fixture.url();
    let mut off = false;
    let (content, error) = fixture.during(
        || call(&url, &token, "browser_navigate", navigate()),
        |fixture| {
            if !off && !locked(&fixture.app.external_bridge.browser_calls).is_empty() {
                fixture.app.apply_action(clicked("external_tool:browser"), &fixture.ctx);
                off = true;
            }
            if off {
                fixture.frame();
            }
        },
    );
    assert!(error);
    assert_eq!(content[0]["text"], SWITCHED_OFF);
    assert!(fixture.cli_tabs().is_empty() && fixture.app.browsers.is_empty());
    assert!(fixture.app.browser_starts.is_empty() && fixture.ran.is_empty());
}

#[test]
fn a_browser_call_asks_under_a_normal_grant_and_follows_the_settings_under_accept_all() {
    let mut fixture = fixture();
    let (_, normal) = fixture.open_cli("claude");
    let navigate = || json!({ "url": "http://localhost:3000/" });

    // Refused, or never answered: no browser starts and nothing runs.
    for answer in [Some("external_approval_deny"), None] {
        let ((content, error), asked) =
            fixture.run_answering(&normal, "browser_navigate", navigate(), answer);
        assert!(error && asked == 1);
        assert_eq!(content[0]["text"], "El usuario no aprobó esta llamada en OpenIDE.");
        assert!(fixture.cli_tabs().is_empty() && fixture.ran.is_empty());
    }
    // Allowed: the browser starts and the page gets the command.
    let ((_, error), asked) =
        fixture.run_answering(&normal, "browser_navigate", navigate(), Some("external_approval_allow"));
    assert!(!error && asked == 1);
    assert!(matches!(
        fixture.ran.as_slice(),
        [(_, BrowserCommand::Navigate { url })] if url == "http://localhost:3000/"
    ));
    // Each call asks again, a reading one too.
    let ((_, error), asked) =
        fixture.run_answering(&normal, "browser_snapshot", json!({}), Some("external_approval_allow"));
    assert!(!error && asked == 1);

    // A CLI the user started in accept-all mode is not asked...
    let (_, all) = fixture.open_cli("claude_accept_all");
    let ((_, error), asked) = fixture.run_answering(&all, "browser_navigate", navigate(), None);
    assert!(!error && asked == 0);
    // ...unless Settings say OpenIDE always asks.
    fixture.app.apply_action(clicked("external_permission:always_ask"), &fixture.ctx);
    let ran = fixture.ran.len();
    let ((_, error), asked) =
        fixture.run_answering(&all, "browser_navigate", navigate(), Some("external_approval_deny"));
    assert!(error && asked == 1 && fixture.ran.len() == ran);
}

#[test]
fn each_terminal_gets_one_browser_of_its_own_and_reaches_no_other() {
    let mut fixture = fixture();
    // The conversation's own browser, which no CLI may reach.
    let scope = fixture.app.dock_scope();
    let own = fixture.app.dock.open(&scope, DockTabKind::Browser);
    let (runtime, conversation) = page(
        BrowserScope {
            workspace: PathBuf::from(&fixture.app.view.workspace),
            session_id: "conversation".into(),
        },
        started(),
    );
    fixture.app.browsers.insert(own.get(), runtime);
    let (first, first_token) = fixture.open_cli("claude_accept_all");
    let (second, second_token) = fixture.open_cli("claude_accept_all");
    assert!(fixture.cli_tabs().is_empty(), "a terminal alone starts no browser");

    // Its first call starts its browser, and the next ones use that one.
    for _ in 0..2 {
        assert!(!fixture.run(&first_token, "browser_snapshot", json!({})).1);
    }
    assert_eq!(fixture.cli_tabs().len(), 1);
    let first_tab = fixture.cli_tab(first);
    let session = fixture.app.external_session(first).unwrap().to_owned();
    assert_eq!(fixture.app.browsers[&first_tab].scope().session_id, session);
    assert_eq!(fixture.app.browsers[&first_tab].scope().workspace, PathBuf::from(&fixture.app.view.workspace));
    assert!(session != "conversation" && Some(session.as_str()) != fixture.app.external_session(second));

    assert!(!fixture.run(&second_token, "browser_reload", json!({})).1);
    let second_tab = fixture.cli_tab(second);
    assert_eq!(
        fixture.app.browsers[&second_tab].scope().session_id,
        fixture.app.external_session(second).unwrap()
    );
    assert_eq!(fixture.app.browsers.len(), 3);
    // Every call reached its own terminal's page; the conversation's got none.
    assert_eq!(
        fixture.ran.iter().map(|(tab, _)| *tab).collect::<Vec<_>>(),
        [first_tab, first_tab, second_tab]
    );
    assert!(conversation.finish_next().is_none());

    // A context that is not the caller's is never handed over, whichever
    // tab holds it.
    let (one, other) = (
        fixture.app.browsers.remove(&first_tab).unwrap(),
        fixture.app.browsers.remove(&second_tab).unwrap(),
    );
    fixture.app.browsers.insert(first_tab, other);
    fixture.app.browsers.insert(second_tab, one);
    let (content, error) = fixture.run(&first_token, "browser_reload", json!({}));
    assert!(error);
    assert_eq!(content[0]["text"], "Ese navegador pertenece a otra sesión.");
    assert_eq!(fixture.ran.len(), 3);

    // The native agent gets its conversation's browser and, without one,
    // none: never a CLI's.
    assert!(fixture.app.browser_client_for_current_scope().is_some());
    fixture.app.dock.close(&scope, own);
    fixture.app.sync_dock_view();
    assert_eq!(fixture.app.browsers.len(), 2);
    assert!(fixture.app.browser_client_for_current_scope().is_none());
    // And opening "Navegador" by hand makes the conversation's, not a CLI's.
    let reopened = fixture.app.dock.open(&scope, DockTabKind::Browser).get();
    assert!(reopened != first_tab && reopened != second_tab);
}

#[test]
fn the_browser_and_its_tab_end_with_the_terminals_grant() {
    let mut fixture = fixture();
    let scope = fixture.app.dock_scope();
    let open = |fixture: &mut Fixture| {
        let (terminal, token) = fixture.open_cli("claude_accept_all");
        assert!(!fixture.run(&token, "browser_snapshot", json!({})).1);
        let tab = fixture.cli_tab(terminal);
        (terminal, token, tab, fixture.app.browsers[&tab].mcp_client())
    };
    let (closed, _, closed_tab, closed_browser) = open(&mut fixture);
    let (restarted, old_token, old_tab, old_browser) = open(&mut fixture);
    let (left, _, left_tab, left_browser) = open(&mut fixture);
    assert_eq!(fixture.app.browsers.len(), 3);
    let listed = |fixture: &Fixture, tab: u64| fixture.app.view.dock_tabs.iter().any(|shown| shown.id == tab);
    assert!(listed(&fixture, closed_tab) && listed(&fixture, old_tab) && listed(&fixture, left_tab));

    // The user closes the terminal's tab.
    fixture.app.apply_action(clicked(&format!("terminal_close:{closed}")), &fixture.ctx);
    assert!(!fixture.app.dock.contains_tab(closed) && fixture.app.external_session(closed).is_none());
    assert!(!fixture.app.dock.contains_tab(closed_tab) && !fixture.app.browsers.contains_key(&closed_tab));
    assert!(closed_browser.catalog_changed(), "its browser is closed");
    assert!(!listed(&fixture, closed_tab) && listed(&fixture, old_tab));

    // The terminal starts its CLI again: a new grant, and nothing of the old.
    let old_session = fixture.app.external_session(restarted).unwrap().to_owned();
    fixture.app.agent_terminal_command(restarted).unwrap().unwrap();
    assert!(!fixture.app.dock.contains_tab(old_tab) && !fixture.app.browsers.contains_key(&old_tab));
    assert!(old_browser.catalog_changed());
    assert_eq!(post(&fixture.url(), &old_token, "ping", Value::Null).status().as_u16(), 401);
    fixture.frame();
    assert!(!listed(&fixture, old_tab), "the view follows");
    let new_token = fixture.app.external_bridge.grants[&restarted].0.clone();
    assert!(!fixture.run(&new_token, "browser_snapshot", json!({})).1);
    let new_tab = fixture.cli_tab(restarted);
    let new_session = fixture.app.browsers[&new_tab].scope().session_id.clone();
    assert!(new_tab != old_tab && new_session != old_session);
    let new_browser = fixture.app.browsers[&new_tab].mcp_client();

    // A call of a session that holds no grant any more gets no browser.
    let (reply, answer) = mpsc::channel();
    locked(&fixture.app.external_bridge.browser_calls).push(BrowserCall {
        session: old_session,
        conversation: None,
        reply,
        expires: Instant::now() + BROWSER_WAIT,
        started: false,
    });
    fixture.frame();
    assert_eq!(answer.recv().unwrap().err().as_deref(), Some(SESSION_ENDED));

    // The application exits.
    fixture.app.revoke_all_external_grants();
    assert!(fixture.app.external_bridge.grants.is_empty() && fixture.cli_tabs().is_empty());
    assert!(fixture.app.browsers.is_empty());
    assert!(left_browser.catalog_changed() && new_browser.catalog_changed());
    assert!(!fixture.app.dock.contains_tab(left_tab) && fixture.app.dock.contains_tab(left));
    // Closing a browser leaves the conversation's other tabs alone.
    assert_eq!(fixture.app.dock.terminal_tabs(&scope).len(), 2);
}

#[test]
fn the_cli_browser_tab_never_takes_the_keyboard_from_a_field() {
    let mut fixture = fixture();
    let scope = fixture.app.dock_scope();
    let activity = fixture.app.dock.open(&scope, DockTabKind::Activity);
    fixture.app.sync_dock_view();
    fixture.app.tree.set_root(Element::column(vec![
        Element::text_input("prompt", "", "Task").size(None, Some(36.0)),
        Element::text_input("another_field", "", "Buscar").size(None, Some(36.0)),
    ]));
    fixture.app.tree.layout(Size::new(640.0, 200.0));
    let active = |fixture: &Fixture| fixture.app.dock.active_tab(&scope).map(|tab| tab.id.get());

    // Typing in a field the tab could cover: it is listed, named after its
    // CLI, and nothing moves.
    assert!(fixture.app.tree.focus("another_field"));
    let (first, token) = fixture.open_cli("claude_accept_all");
    assert!(!fixture.run(&token, "browser_snapshot", json!({})).1);
    let tab = fixture.cli_tab(first);
    assert_eq!(active(&fixture), Some(activity.get()));
    assert_eq!(fixture.app.tree.focused_id(), Some("another_field"));
    assert!(fixture
        .app
        .view
        .dock_tabs
        .iter()
        .any(|shown| shown.id == tab && shown.title == "Navegador · Claude Code" && !shown.active));

    // From the composer, which the dock does not cover, the tab comes
    // forward and the composer keeps the keyboard.
    assert!(fixture.app.tree.focus("prompt"));
    let (second, token) = fixture.open_cli("claude_accept_all");
    assert!(!fixture.run(&token, "browser_snapshot", json!({})).1);
    assert_eq!(active(&fixture), Some(fixture.cli_tab(second)));
    assert!(fixture.app.view.show_side_pane && fixture.app.view.side_tab == SideTab::Browser);
    assert_eq!(fixture.app.view.browser.tab_id, Some(fixture.cli_tab(second)));
    assert_eq!(fixture.app.tree.focused_id(), Some("prompt"));
}

#[test]
fn a_screenshot_reaches_the_cli_as_an_mcp_image_block_within_the_native_image_limits() {
    use base64::Engine;
    let mut fixture = fixture();
    let (_, token) = fixture.open_cli("claude_accept_all");
    let url = fixture.url();
    let answered = |fixture: &mut Fixture, output: BrowserToolOutput| {
        fixture.during(
            || call(&url, &token, "browser_take_screenshot", json!({})),
            |fixture| {
                fixture.app.serve_external_browser_calls(&fixture.ctx);
                fixture.attach();
                for page in fixture.pages.values() {
                    page.finish_next_with_output(output.clone());
                }
            },
        )
    };
    // A capture as the runtime hands it over: a PNG, base64 encoded.
    let capture = |width: u32, height: u32| {
        let mut png = Vec::new();
        image::RgbImage::new(width, height)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(&png);
        (BrowserToolOutput { image: Some(Arc::from(data.as_str())), ..Default::default() }, data)
    };

    // The image travels as MCP image content, never inside a text field.
    let (output, data) = capture(64, 48);
    let (content, error) = answered(&mut fixture, output);
    assert!(!error);
    assert_eq!(content.len(), 2);
    assert!(content[0]["type"] == "text" && !content[0]["text"].as_str().unwrap().contains(&data));
    assert_eq!(content[1], json!({ "type": "image", "mimeType": "image/png", "data": data }));

    // One the native runner would not give its model does not reach a CLI's.
    let (output, _) = capture(8001, 1);
    let (content, error) = answered(&mut fixture, output);
    assert!(!error);
    assert!(content.iter().all(|block| block["type"] == "text"));
    let note = content[1]["text"].as_str().unwrap();
    assert!(note.starts_with("[image omitted (image/png, ") && note.contains("8000 px"), "{note}");

    // The tool's own failure is an error the CLI's model reads.
    let (content, error) = answered(
        &mut fixture,
        BrowserToolOutput { error: Some("Tomá una captura nueva con browser_snapshot".into()), ..Default::default() },
    );
    assert!(error);
    assert_eq!(content, [json!({ "type": "text", "text": "Tomá una captura nueva con browser_snapshot" })]);
}

#[test]
fn the_limit_of_open_browsers_holds_for_a_cli_too() {
    let mut fixture = fixture();
    let workspace = PathBuf::from(&fixture.app.view.workspace);
    let _pages: Vec<_> = (0..MAX_BROWSERS)
        .map(|index| {
            let session = format!("conversation-{index}");
            let tab = fixture
                .app
                .dock
                .open(&DockScope::for_owner(&workspace, &session), DockTabKind::Browser);
            let (runtime, commands) =
                page(BrowserScope { workspace: workspace.clone(), session_id: session }, started());
            fixture.app.browsers.insert(tab.get(), runtime);
            commands
        })
        .collect();
    let (_, token) = fixture.open_cli("claude_accept_all");
    let (content, error) = fixture.run(&token, "browser_snapshot", json!({}));
    assert!(error);
    assert_eq!(content[0]["text"], BROWSER_LIMIT);
    assert!(fixture.cli_tabs().is_empty() && fixture.app.browsers.len() == MAX_BROWSERS);
}

#[test]
fn a_native_agents_first_call_opens_its_conversations_tab_and_again_once_closed() {
    let mut fixture = fixture();
    let scope = fixture.app.dock_scope();
    // A CLI's browser is already open in this very dock.
    let (terminal, token) = fixture.open_cli("claude_accept_all");
    assert!(!fixture.run(&token, "browser_snapshot", json!({})).1);
    let cli_tab = fixture.cli_tab(terminal);
    let own_tabs = |fixture: &Fixture| -> Vec<DockTabId> {
        fixture
            .app
            .dock
            .tabs(&fixture.app.view.workspace)
            .iter()
            .filter(|tab| tab.kind == DockTabKind::Browser)
            .map(|tab| tab.id)
            .collect()
    };
    assert!(own_tabs(&fixture).is_empty(), "the conversation has no browser tab");

    let browser: ConversationBrowser = fixture.app.conversation_browser(scope.clone(), "conversation");
    let snapshot = || {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(browser.call_tool("browser_snapshot", json!({}), &CancellationToken::new()))
            .unwrap()
    };
    // The page the window would start for the tab the call opened.
    let frame = |fixture: &mut Fixture| {
        fixture.frame();
        for tab in own_tabs(fixture).into_iter().map(DockTabId::get) {
            if !fixture.app.browsers.contains_key(&tab) {
                let (runtime, commands) = page(fixture.app.browser_launch_scope(tab).unwrap(), started());
                fixture.app.browsers.insert(tab, runtime);
                fixture.pages.insert(tab, commands);
            }
        }
    };

    let ran = fixture.ran.len();
    assert!(!fixture.during(snapshot, frame).is_error);
    let first = own_tabs(&fixture);
    assert_eq!(first.len(), 1, "the call opened the conversation's tab");
    assert_eq!(fixture.app.dock.active_tab(&scope).map(|tab| tab.id), Some(first[0]));
    assert_eq!(fixture.app.browsers[&first[0].get()].scope().session_id, "conversation");
    // It ran on that page, not on the CLI's.
    let pages: Vec<u64> = fixture.ran[ran..].iter().map(|(tab, _)| *tab).collect();
    assert_eq!(pages, [first[0].get()]);
    assert!(first[0].get() != cli_tab);

    // The next call finds it open.
    assert!(!fixture.during(snapshot, frame).is_error);
    assert_eq!(own_tabs(&fixture), first);

    // The user closes the tab; the agent's next call opens it again.
    fixture.app.dock.close(&scope, first[0]);
    fixture.app.sync_dock_view();
    assert!(own_tabs(&fixture).is_empty());
    assert!(!fixture.during(snapshot, frame).is_error);
    let second = own_tabs(&fixture);
    assert!(second.len() == 1 && second != first);
    assert_eq!(fixture.ran.last().map(|(tab, _)| *tab), Some(second[0].get()));
}
