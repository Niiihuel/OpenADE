//! The agent's commands in the terminal dock, from view projections only:
//! no process starts here. The export writes the review captures.
use super::*;
use crate::ide_workspace::WindowMode;
use crate::terminal::{TerminalColor, TerminalSnapshot, TerminalSpan, TerminalStyle};
use crate::transcript::{ActivityStatus, TranscriptItem};
use crate::view::{ide_visual_tests, SideTab};
use openide_ui::{DrawCommand, Frame, Rect, Size, UiAction, UiEvent, UiTree};

const SERVER: &str = "11111111-1111-4111-8111-111111111111";
const BUILD: &str = "22222222-2222-4222-8222-222222222222";

fn screen(lines: &[(&str, TerminalColor)], running: bool) -> TerminalSnapshot {
    TerminalSnapshot {
        lines: lines.iter().map(|(line, _)| line.to_string()).collect(),
        styled_lines: lines
            .iter()
            .map(|(line, foreground)| {
                vec![TerminalSpan {
                    text: line.to_string(),
                    style: TerminalStyle {
                        foreground: *foreground,
                        ..Default::default()
                    },
                }]
            })
            .collect(),
        cursor: (lines.len() as u16, 0),
        cols: 100,
        rows: 12,
        scrollback_position: 0,
        running,
        exit_code: (!running).then_some(0),
        error: None,
        revision: 1,
        shell: "npm".into(),
        alternate_screen: false,
        selection: None,
    }
}

/// One shell of the user's and two commands the agent started: a server
/// that runs and a build that ended. `active` is the tab on screen.
pub(crate) fn fixture(mode: WindowMode, active: u64) -> ViewState {
    let mut state = ide_visual_tests::fixture();
    state.ide.mode = mode;
    state.show_terminal_panel = true;
    state.terminal_tabs = [(81, "fish 1"), (82, "npm run dev"), (83, "cargo build --release")]
        .into_iter()
        .map(|(id, title)| DockTabView {
            id,
            title: title.into(),
            full_path: Some(state.workspace.clone()),
            active: id == active,
            kind: SideTab::Terminal,
            closable: true,
        })
        .collect();
    state.terminal_controls.agents = vec![
        AgentTerminalView {
            id: 82,
            command_id: SERVER.into(),
            task: "Revisar el workspace".into(),
            read_only: false,
            ended: None,
        },
        AgentTerminalView {
            id: 83,
            command_id: BUILD.into(),
            task: "Revisar el workspace".into(),
            read_only: false,
            ended: Some("Proceso finalizado · código 0".into()),
        },
    ];
    state.terminal_snapshot = Some(match active {
        83 => screen(
            &[
                ("   Compiling openide-editor v0.1.0", TerminalColor::Indexed(2)),
                ("    Finished `release` profile in 48.20s", TerminalColor::Indexed(2)),
            ],
            false,
        ),
        _ => screen(
            &[
                ("> web@0.1.0 dev", TerminalColor::Default),
                ("> vite", TerminalColor::Default),
                ("", TerminalColor::Default),
                ("  VITE v7.1.2  ready in 312 ms", TerminalColor::Indexed(2)),
                ("", TerminalColor::Default),
                ("  ➜  Local:   http://localhost:5173/", TerminalColor::Indexed(6)),
                ("  ➜  Network: use --host to expose", TerminalColor::Indexed(8)),
            ],
            true,
        ),
    });
    state.transcript.push(TranscriptItem::Tool {
        id: "call-server".into(),
        name: "start_command".into(),
        arguments: serde_json::json!({"command": "npm", "args": ["run", "dev"]}),
        output: Some(serde_json::json!({ "session_id": SERVER }).to_string()),
        is_error: false,
        activity: ActivityStatus::Completed,
    });
    state
}

fn rect(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("missing {id}"))
        .rect
}

fn icon_in(frame: &Frame, bounds: Rect) -> Icon {
    frame
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::Icon { icon, rect, .. }
                if rect.x >= bounds.x
                    && rect.x + rect.width <= bounds.x + bounds.width
                    && rect.y >= bounds.y
                    && rect.y + rect.height <= bounds.y + bounds.height =>
            {
                Some(*icon)
            }
            _ => None,
        })
        .expect("a session icon")
}

fn has_text(frame: &Frame, expected: &str) -> bool {
    frame.commands.iter().any(|command| {
        matches!(command, DrawCommand::Text { text, .. } if text.contains(expected))
    })
}

const SIZE: Size = Size::new(1200.0, 720.0);

/// The window laid out with the bundled fonts, so widths are the real ones;
/// the transcript rests at its end, where a new card appears.
fn window(state: &ViewState) -> (eframe::egui::Context, UiTree, Frame) {
    let (context, mut tree) = ide_visual_tests::native_tree(crate::view::build_window_view(
        state,
        SIZE.width,
        SIZE.height,
    ));
    tree.layout(SIZE);
    tree.scroll_to_end("transcript");
    let frame = tree.layout(SIZE);
    (context, tree, frame)
}

#[test]
fn agent_sessions_carry_the_agent_mark_in_both_windows_and_never_offer_a_restart() {
    for mode in [WindowMode::Agents, WindowMode::Ide] {
        let mut state = fixture(mode, 82);
        // Where there is room every session is a flat tab beside the
        // sections; the IDE's narrower panel names the one on screen and
        // lists them all. The agent's two carry its mark, not the terminal's.
        let entry = if mode == WindowMode::Ide {
            state.terminal_controls.sessions_open = true;
            "terminal_tab_menu"
        } else {
            "terminal_tab"
        };
        let (_context, _tree, frame) = window(&state);
        if mode == WindowMode::Ide {
            let named = rect(&frame, "terminal_sessions_toggle");
            assert_eq!(icon_in(&frame, named), Icon::Chat);
        }
        for (id, icon) in [(81, Icon::Terminal), (82, Icon::Chat), (83, Icon::Chat)] {
            let session = rect(&frame, &format!("{entry}:{id}"));
            assert_eq!(icon_in(&frame, session), icon, "{mode:?} {id}");
        }
        assert!(has_text(&frame, "npm run dev") && has_text(&frame, "localhost:5173"));
        // A running command has neither the read-only bar nor an exit bar.
        assert!(!has_text(&frame, "Proceso finalizado") && !has_text(&frame, "Solo lectura"));

        // The one that ended keeps its status and is not restarted from here.
        let mut state = fixture(mode, 83);
        state.terminal_menu_open = true;
        let (_context, _tree, frame) = window(&state);
        assert!(has_text(&frame, "Proceso finalizado · código 0"), "{mode:?}");
        assert!(!frame.regions.iter().any(|region| region.id.starts_with("terminal_restart")));
        assert!(frame.regions.iter().any(|region| region.id == "terminal_close:83"));
        // A shell that ended still offers its restart.
        state.terminal_controls.agents.clear();
        let (_context, _tree, frame) = window(&state);
        assert!(frame.regions.iter().any(|region| region.id == "terminal_restart"));
    }
    let state = fixture(WindowMode::Agents, 82);
    let tab = |id: u64| state.terminal_tabs.iter().find(|tab| tab.id == id).unwrap();
    assert_eq!(
        session_origin(&state, tab(82)),
        "Iniciada por el agente · Revisar el workspace"
    );
    assert_eq!(session_origin(&state, tab(81)), "Directorio inicial: /workspace/openADE");
}

#[test]
fn the_command_card_leads_to_its_tab_while_the_tab_is_open() {
    let mut state = fixture(WindowMode::Agents, 81);
    let (_context, mut tree, frame) = window(&state);
    let link = rect(&frame, "agent_terminal_link:card:call-server");
    assert!(has_text(&frame, "Ver en terminal"));
    let point = openide_ui::Point::new(link.x + link.width / 2.0, link.y + link.height / 2.0);
    tree.handle_event(UiEvent::PointerDown(point));
    assert_eq!(
        tree.handle_event(UiEvent::PointerUp(point)),
        vec![UiAction::Clicked {
            id: format!("agent_terminal_show:{SERVER}")
        }]
    );
    // Once the user closed the tab there is nowhere to go.
    state.terminal_controls.agents.remove(0);
    let (_context, _tree, frame) = window(&state);
    assert!(!has_text(&frame, "Ver en terminal"));
}

#[test]
fn closing_a_running_command_shows_what_will_stop() {
    let mut state = fixture(WindowMode::Agents, 82);
    state.terminal_controls.closing = Some(82);
    let (_context, _tree, frame) = window(&state);
    assert!(has_text(&frame, "Detener npm run dev"));
    for id in ["agent_terminal_stop:82", "agent_terminal_stop_cancel"] {
        assert!(frame.regions.iter().any(|region| region.id == id), "{id}");
    }
    // Nothing asks about a tab that is not an agent's running command.
    state.terminal_controls.closing = Some(81);
    assert!(!crate::view::dialog_open(&state, SIZE.width));
}

#[test]
#[ignore = "writes the agent terminals captures under docs/ui-separation/shots/agent-terminals"]
fn export_agent_terminals_visual_review() {
    let shot = |state: &ViewState, name: &str| {
        let (context, _tree, frame) = window(state);
        ide_visual_tests::export_shot(&frame, SIZE, "agent-terminals", name);
        context.end_pass().textures_delta.clear();
    };
    for (mode, name) in [(WindowMode::Agents, "agents"), (WindowMode::Ide, "ide")] {
        shot(&fixture(mode, 82), &format!("s1-sessions-{name}-running"));
        shot(&fixture(mode, 83), &format!("s1-sessions-{name}-exited"));
    }
    let mut state = fixture(WindowMode::Ide, 82);
    state.terminal_controls.sessions_open = true;
    shot(&state, "s1-sessions-ide-list");
    let mut state = fixture(WindowMode::Agents, 81);
    state.terminal_snapshot = Some(screen(&[("openADE $ ", TerminalColor::Default)], true));
    shot(&state, "s1-command-card");
    let mut state = fixture(WindowMode::Agents, 82);
    state.terminal_controls.closing = Some(82);
    shot(&state, "s1-close-confirm");
    let mut state = fixture(WindowMode::Agents, 82);
    state.terminal_controls.agents[0].read_only = true;
    shot(&state, "s1-read-only");
}
