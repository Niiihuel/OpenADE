use super::*;
use crate::terminal::{TerminalColor, TerminalSnapshot, TerminalSpan, TerminalStyle};
use crate::view::SideTab;
use openide_ui::{DrawCommand, Frame, Key, Point, Rect, Size, UiAction, UiEvent, UiTree};

fn fixture() -> ViewState {
    let mut state = crate::view::tests::empty_state();
    state.workspace = "/workspace/openADE".into();
    state.terminal_tabs = (1..=8)
        .map(|id| DockTabView {
            id,
            title: format!("Terminal {id}"),
            full_path: Some(state.workspace.clone()),
            active: id == 1,
            kind: SideTab::Terminal,
            closable: true,
        })
        .collect();
    let output = [
        ("$ cargo run -p agents-window", TerminalColor::Default),
        ("   Compiling agents-window", TerminalColor::Indexed(2)),
        (
            "    Finished dev profile in 1.42s",
            TerminalColor::Indexed(2),
        ),
        ("     Running OpenIDE", TerminalColor::Indexed(6)),
        ("$ ", TerminalColor::Default),
    ];
    state.terminal_snapshot = Some(TerminalSnapshot {
        lines: output.iter().map(|(line, _)| line.to_string()).collect(),
        styled_lines: output
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
        cursor: (4, 2),
        cols: 100,
        rows: 12,
        scrollback_position: 0,
        running: true,
        exit_code: None,
        error: None,
        revision: 1,
        shell: "bash".into(),
        alternate_screen: false,
        selection: None,
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

fn center(rect: Rect) -> Point {
    Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
}

fn click(tree: &mut UiTree, id: &str) -> Vec<UiAction> {
    let point = center(rect(tree.frame(), id));
    assert_eq!(tree.frame().hit_test(point), Some(id));
    tree.handle_event(UiEvent::PointerDown(point));
    tree.handle_event(UiEvent::PointerUp(point))
}

fn clicked(actions: &[UiAction], expected: &str) -> bool {
    actions
        .iter()
        .any(|action| matches!(action, UiAction::Clicked { id } if id == expected))
}

#[test]
fn ide_sections_share_one_header_without_losing_terminal_session_actions() {
    let mut state = fixture();
    state.ide.mode = crate::ide_workspace::WindowMode::Ide;
    for width in [272.0, 320.0, 640.0, 1100.0] {
        let sections_width = if width >= 640.0 { 280.0 } else { 104.0 };
        let sections = || {
            Element::clickable("ide_bottom_terminal", Element::text("Terminal"))
                .size(Some(sections_width), Some(24.0))
        };
        let viewport = Size::new(width, 300.0);
        let mut tree = UiTree::new(build_ide(
            &state,
            width,
            viewport.height,
            sections(),
            sections_width,
        ));
        tree.set_reduced_motion(true);
        let frame = tree.layout(viewport);
        assert_eq!(rect(&frame, "terminal_screen").y, HEADER_HEIGHT);
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("terminal_tab:")));
        for id in [
            "ide_bottom_terminal",
            "terminal_new",
            "terminal_profiles_toggle",
            "terminal_sessions_toggle",
            "terminal_menu_toggle",
            "terminal_panel_hide",
        ] {
            let bounds = rect(&frame, id);
            assert!(
                bounds.x >= 0.0 && bounds.x + bounds.width <= width,
                "{id}: {bounds:?}"
            );
            assert!(bounds.y >= 0.0 && bounds.y + bounds.height <= HEADER_HEIGHT);
            assert_eq!(frame.hit_test(center(bounds)), Some(id));
        }
        assert!(clicked(
            &click(&mut tree, "terminal_sessions_toggle"),
            "terminal_sessions_toggle"
        ));
        state.terminal_controls.sessions_open = true;
        tree.set_root(build_ide(
            &state,
            width,
            viewport.height,
            sections(),
            sections_width,
        ));
        tree.layout(viewport);
        assert!(clicked(
            &click(&mut tree, "terminal_tab_menu:2"),
            "terminal_tab_menu:2"
        ));
        state.terminal_controls.sessions_open = false;
    }
}

#[test]
fn animated_terminal_viewports_clip_header_and_hits_without_growing_their_allocation() {
    let state = fixture();
    for ide in [false, true] {
        for height in [0.0, 1.0, 12.0, 31.0, 32.0, 120.0] {
            let width = 640.0;
            let panel = if ide {
                build_ide(
                    &state,
                    width,
                    height,
                    Element::clickable("fixture_sections", Element::text("Terminal"))
                        .size(Some(280.0), Some(HEADER_HEIGHT)),
                    280.0,
                )
            } else {
                build(&state, width, height)
            };
            let mut tree = UiTree::new(Element::column(vec![
                panel,
                Element::clickable("after_panel", Element::spacer()).size(Some(width), Some(20.0)),
            ]));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, height + 20.0));
            assert_eq!(
                rect(&frame, "after_panel"),
                Rect::new(0.0, height, width, 20.0)
            );
            for target in frame
                .regions
                .iter()
                .filter(|target| target.id != "after_panel")
            {
                assert!(
                    target.rect.y >= 0.0 && target.rect.y + target.rect.height <= height,
                    "{ide:?} at {height}px: {} escaped animated viewport: {:?}",
                    target.id,
                    target.rect
                );
            }
            for x in [8.0, width / 2.0, width - 8.0] {
                assert_eq!(
                    frame.hit_test(Point::new(x, height + 1.0)),
                    Some("after_panel")
                );
            }
            if height > HEADER_HEIGHT {
                assert_eq!(
                    rect(&frame, "terminal_screen"),
                    Rect::new(0.0, HEADER_HEIGHT, width, height - HEADER_HEIGHT)
                );
            }
            if height >= 28.0 {
                assert_eq!(
                    rect(&frame, "terminal_new").height,
                    TOOL_SIZE,
                    "header controls retain their normal size until clipped"
                );
            }
        }
    }
}

#[test]
fn compact_panel_keeps_sessions_actions_and_viewport_reachable_at_every_size() {
    let state = fixture();
    for size in [
        Size::new(320.0, 140.0),
        Size::new(620.0, 280.0),
        Size::new(1100.0, 360.0),
    ] {
        let mut tree = UiTree::new(build(&state, size.width, size.height));
        tree.set_reduced_motion(true);
        let frame = tree.layout(size);
        let screen = rect(&frame, "terminal_screen");
        assert_eq!(
            screen,
            Rect::new(0.0, HEADER_HEIGHT, size.width, size.height - HEADER_HEIGHT)
        );
        for id in [
            "terminal_sessions_toggle",
            "terminal_profiles_toggle",
            "terminal_new",
            "terminal_menu_toggle",
            "terminal_panel_hide",
        ] {
            let bounds = rect(&frame, id);
            assert!(
                bounds.x >= 0.0 && bounds.x + bounds.width <= size.width + 0.1,
                "{id}: {bounds:?}"
            );
            assert!(
                bounds.y >= 0.0 && bounds.y + bounds.height <= HEADER_HEIGHT,
                "{id}: {bounds:?}"
            );
        }
        for id in [
            "terminal_sessions_toggle",
            "terminal_profiles_toggle",
            "terminal_new",
            "terminal_menu_toggle",
            "terminal_panel_hide",
        ] {
            assert_eq!(rect(&frame, id).width, TOOL_SIZE);
            assert_eq!(rect(&frame, id).height, TOOL_SIZE);
        }
        let first_tab = rect(&frame, "terminal_tab_extent:1");
        assert_eq!(first_tab.x, HEADER_SIDE_INSET);
        let last_control = rect(&frame, "terminal_panel_hide");
        assert_eq!(
            last_control.x + last_control.width,
            size.width - HEADER_SIDE_INSET
        );
        assert_eq!(first_tab.height, crate::dock_tab_ui::TAB_HEIGHT);
        assert_eq!(
            (
                rect(&frame, "terminal_close:1").width,
                rect(&frame, "terminal_close:1").height
            ),
            (20.0, 20.0)
        );
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillCornerRect { rect, color, radii } if *rect == first_tab && *color == theme::BACKGROUND && *radii == openide_ui::CornerRadii::top(8.0)
        )));
        assert!(!frame.regions.iter().any(|region| [
            "terminal_kill_active",
            "terminal_clear",
            "terminal_copy",
            "terminal_input"
        ]
        .contains(&region.id.as_str())));
        assert!(!frame.commands.iter().any(|command| matches!(command, DrawCommand::Text { text, .. } if text.contains("Ctrl+Shift+V") || text.contains("Sesión local"))));
        assert!(clicked(&click(&mut tree, "terminal_new"), "terminal_new"));
        assert!(clicked(
            &click(&mut tree, "terminal_panel_hide"),
            "terminal_panel_hide"
        ));
    }
}

#[test]
fn terminal_rounded_tabs_select_by_keyboard_reveal_and_keep_close_independent() {
    let mut state = fixture();
    let size = Size::new(620.0, 320.0);
    let mut tree = UiTree::new(build(&state, size.width, size.height));
    tree.set_reduced_motion(true);
    let first = tree.layout(size);
    let toolbar = rect(&first, "terminal_new");
    assert!(tree.focus("terminal_tab:1"));
    for (key, target) in [(Key::ArrowRight, 2), (Key::End, 8)] {
        let actions = tree.handle_event(UiEvent::Key {
            key,
            ctrl: false,
            shift: false,
        });
        assert!(
            clicked(&actions, &format!("terminal_tab:{target}")),
            "{actions:?}"
        );
        for tab in &mut state.terminal_tabs {
            tab.active = tab.id == target;
        }
        tree.set_root(build(&state, size.width, size.height));
        let frame = tree.layout(size);
        assert_eq!(
            tree.focused_id(),
            Some(format!("terminal_tab:{target}").as_str())
        );
        assert_eq!(rect(&frame, "terminal_new"), toolbar);
        for id in [
            format!("terminal_tab:{target}"),
            format!("terminal_close:{target}"),
        ] {
            let bounds = rect(&frame, &id);
            assert!(bounds.x >= HEADER_SIDE_INSET && bounds.x + bounds.width <= toolbar.x - 8.0);
        }
    }
    let close = rect(tree.frame(), "terminal_close:8");
    assert_eq!((close.width, close.height), (20.0, 20.0));
    let actions = click(&mut tree, "terminal_close:8");
    assert!(clicked(&actions, "terminal_close:8"));
    assert!(!clicked(&actions, "terminal_tab:8"));
}

#[test]
fn terminal_overflow_sessions_menu_has_unique_ids_and_retains_keyboard_navigation() {
    let mut state = fixture();
    let size = Size::new(620.0, 420.0);
    let mut tree = UiTree::new(build(&state, size.width, size.height));
    tree.set_reduced_motion(true);
    tree.layout(size);
    assert!(clicked(
        &click(&mut tree, "terminal_sessions_toggle"),
        "terminal_sessions_toggle"
    ));
    state.terminal_controls.sessions_open = true;
    tree.set_root(build(&state, size.width, size.height));
    let frame = tree.layout(size);
    let mut ids = std::collections::HashSet::new();
    // ScrollArea deliberately shares one ID between its Scroll viewport and
    // Scrollbar hit region. Only actionable buttons must be globally unique:
    // a menu row must never alias the retained tab beneath the popover.
    for region in frame
        .regions
        .iter()
        .filter(|region| region.kind == openide_ui::HitKind::Button)
    {
        assert!(
            ids.insert(region.id.clone()),
            "duplicate actionable ID {}",
            region.id
        );
    }
    for id in [
        "terminal_tab:1",
        "terminal_tab_menu:1",
        "terminal_tab_menu:2",
    ] {
        assert!(ids.contains(id), "missing independent control {id}");
    }
    assert!(frame
        .regions
        .iter()
        .any(|region| region.id == "terminal_tab:1"));
    assert!(tree.focus("terminal_tab_menu:1"));
    tree.handle_event(UiEvent::Key {
        key: Key::ArrowDown,
        ctrl: false,
        shift: false,
    });
    assert!(clicked(
        &tree.handle_event(UiEvent::Key {
            key: Key::Enter,
            ctrl: false,
            shift: false
        }),
        "terminal_tab_menu:2"
    ));
    assert_eq!(
        (
            rect(&frame, "terminal_sessions_toggle").width,
            rect(&frame, "terminal_sessions_toggle").height
        ),
        (TOOL_SIZE, TOOL_SIZE)
    );
}

fn session_list_fixture(count: usize) -> ViewState {
    let mut state = fixture();
    state.terminal_tabs = (1..=count as u64)
        .map(|id| DockTabView {
            id,
            title: match id {
                1 => "bash 1".into(),
                2 => "bash 2".into(),
                3 => "fish 3".into(),
                _ => format!("Terminal {id}"),
            },
            full_path: Some(state.workspace.clone()),
            active: id == 2,
            kind: SideTab::Terminal,
            closable: true,
        })
        .collect();
    state.terminal_controls.sessions_open = true;
    state
}

#[test]
fn terminal_session_rows_keep_active_marker_and_independent_close_actions_in_narrow_menus() {
    use std::time::{Duration, Instant};
    for width in [240.0, 320.0, 640.0] {
        let state = session_list_fixture(3);
        let size = Size::new(width, 420.0);
        let (context, mut tree) =
            crate::view::ide_visual_tests::native_tree(build(&state, size.width, size.height));
        let now = Instant::now();
        let frame = tree.layout_at(size, now);
        for id in 1..=3 {
            let select = rect(&frame, &format!("terminal_tab_menu:{id}"));
            let close = rect(&frame, &format!("terminal_session_close:{id}"));
            assert_eq!((close.width, close.height), (24.0, 24.0));
            assert_eq!(select.height, 26.0);
            assert!((center(select).y - center(close).y).abs() < 0.01);
            assert!(select.x >= 0.0 && select.x + select.width + 2.0 <= close.x + 0.01);
            assert!(close.x + close.width <= width && close.y >= 0.0);
            assert_eq!(
                frame.hit_test(center(select)),
                Some(format!("terminal_tab_menu:{id}").as_str())
            );
            assert_eq!(
                frame.hit_test(center(close)),
                Some(format!("terminal_session_close:{id}").as_str())
            );
        }
        assert!(
            frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == "Activa"
            )),
            "the active row stays identifiable beside its always-visible close"
        );

        let active_row = rect(&frame, "terminal_tab_menu:2");
        let text_center = |label: &str| {
            let (position, font_size) = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Text {
                        text,
                        position,
                        font_size,
                        font_family: FontFamily::Sans,
                        ..
                    } if text == label
                        && position.y >= active_row.y
                        && position.y < active_row.y + active_row.height =>
                    {
                        Some((*position, *font_size))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing {label} in active session row"));
            let native_height = context.fonts_mut(|fonts| {
                fonts.row_height(&eframe::egui::FontId::proportional(font_size))
            });
            position.y + native_height / 2.0
        };
        let label_center = text_center("bash 2");
        let marker_center = text_center("Activa");
        assert!((label_center - marker_center).abs() <= 1.0,
            "native label and active marker must share vertical center: {label_center} vs {marker_center}");
        for text_center in [label_center, marker_center] {
            assert!(
                (text_center - center(active_row).y).abs() <= 1.0,
                "session text must be vertically centered inside its 26px action"
            );
        }

        let close_id = "terminal_session_close:2";
        tree.handle_event_at(UiEvent::PointerMove(center(rect(&frame, close_id))), now);
        let hover = tree.layout_at(size, now + Duration::from_millis(700));
        assert!(hover.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Cerrar terminal"
        )));
        assert_eq!(rect(&hover, close_id), rect(&frame, close_id));
        assert_eq!(
            click(&mut tree, close_id),
            vec![UiAction::Clicked {
                id: "terminal_close:2".into()
            }]
        );
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn terminal_session_list_arrows_skip_close_buttons_but_tab_and_activation_keep_them_accessible() {
    let state = session_list_fixture(12);
    let size = Size::new(320.0, 420.0);
    let mut tree = UiTree::new(build(&state, size.width, size.height));
    tree.set_reduced_motion(true);
    tree.layout(size);
    assert!(tree.focus("terminal_tab_menu:1"));
    let key = |key, shift| UiEvent::Key {
        key,
        shift,
        ctrl: false,
    };
    assert!(tree.handle_event(key(Key::ArrowDown, false)).is_empty());
    assert_eq!(tree.focused_id(), Some("terminal_tab_menu:2"));
    tree.handle_event(key(Key::Tab, false));
    assert_eq!(tree.focused_id(), Some("terminal_session_close:2"));
    assert_eq!(
        tree.handle_event(key(Key::Enter, false)),
        vec![UiAction::Clicked {
            id: "terminal_close:2".into()
        }]
    );
    tree.handle_event(key(Key::Tab, true));
    assert_eq!(tree.focused_id(), Some("terminal_tab_menu:2"));
    tree.handle_event(key(Key::Tab, false));
    tree.handle_event(key(Key::ArrowDown, false));
    assert_eq!(tree.focused_id(), Some("terminal_tab_menu:3"));
    tree.handle_event(key(Key::End, false));
    let frame = tree.layout(size);
    assert_eq!(tree.focused_id(), Some("terminal_tab_menu:12"));
    let final_row = rect(&frame, "terminal_tab_menu:12");
    assert_eq!(
        frame.hit_test(center(final_row)),
        Some("terminal_tab_menu:12")
    );
    tree.handle_event(key(Key::Tab, false));
    tree.layout(size);
    assert_eq!(tree.focused_id(), Some("terminal_session_close:12"));
    assert_eq!(
        tree.handle_event(key(Key::Space, false)),
        vec![UiAction::Clicked {
            id: "terminal_close:12".into()
        }]
    );
    tree.handle_event(key(Key::Home, false));
    tree.layout(size);
    assert_eq!(tree.focused_id(), Some("terminal_tab_menu:1"));
}

#[test]
fn terminal_tabs_scroll_continuously_with_passive_fades_and_fixed_toolbar_at_320px() {
    let mut state = fixture();
    let size = Size::new(320.0, 320.0);
    let mut tree = UiTree::new(build(&state, size.width, size.height));
    tree.set_reduced_motion(true);
    let first = tree.layout(size);
    let toolbar = rect(&first, "terminal_new");
    let strip = rect(&first, "terminal_tabs");
    assert!(strip.x >= HEADER_SIDE_INSET && strip.x + strip.width <= toolbar.x - 8.0);
    let fades = |frame: &Frame| {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRect { rect, color, .. }
                    if color.r == crate::dock_tab_ui::STRIP_BACKGROUND.r
                        && color.g == crate::dock_tab_ui::STRIP_BACKGROUND.g
                        && color.b == crate::dock_tab_ui::STRIP_BACKGROUND.b
                        && color.a < 255
                        && rect.height == crate::dock_tab_ui::TAB_HEIGHT =>
                {
                    Some(*rect)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert!(!fades(&first).is_empty());
    assert!(fades(&first)
        .iter()
        .all(|fade| fade.x >= strip.x && fade.x + fade.width <= strip.x + strip.width));
    tree.handle_event(UiEvent::Scroll2D {
        position: center(strip),
        delta_x: -80.0,
        delta_y: 0.0,
        shift: false,
        direct: true,
    });
    let moved = tree.layout(size);
    assert_eq!(rect(&moved, "terminal_new"), toolbar);
    assert!(rect(&moved, "terminal_tab:2").x < rect(&first, "terminal_tab:2").x - 60.0);
    tree.set_root(build(&state, size.width, size.height));
    let repeated = tree.layout(size);
    assert_eq!(
        rect(&repeated, "terminal_tab:2"),
        rect(&moved, "terminal_tab:2")
    );
    for tab in &mut state.terminal_tabs {
        tab.active = tab.id == 7;
    }
    tree.set_root(build(&state, size.width, size.height));
    let selected = tree.layout(size);
    let close = rect(&selected, "terminal_close:7");
    assert!(close.x >= strip.x + 16.0);
    assert!(close.x + close.width <= strip.x + strip.width - 16.0);
    assert_eq!(selected.hit_test(center(close)), Some("terminal_close:7"));
    assert_eq!(rect(&selected, "terminal_new"), toolbar);
}

#[test]
fn terminal_profiles_and_rename_use_controlled_menus_with_real_action_ids() {
    let mut state = fixture();
    state.terminal_controls.profiles_open = true;
    state.terminal_controls.profiles = vec![
        TerminalProfileView {
            id: "default".into(),
            label: "Bash".into(),
            is_default: true,
        },
        TerminalProfileView {
            id: "fish".into(),
            label: "Fish".into(),
            is_default: false,
        },
    ];
    let size = Size::new(360.0, 320.0);
    let mut tree = UiTree::new(build(&state, size.width, size.height));
    tree.set_reduced_motion(true);
    tree.layout(size);
    assert!(clicked(
        &click(&mut tree, "terminal_new_profile:fish"),
        "terminal_new_profile:fish"
    ));
    state.terminal_controls.profiles_open = false;
    state.terminal_controls.rename = Some(TerminalRenameView {
        id: 1,
        name: "Build".into(),
        error: None,
    });
    tree.set_root(build(&state, size.width, size.height));
    tree.layout(size);
    assert_eq!(
        tree.focused_id(),
        Some("terminal_rename_input:1"),
        "the explicit rename target must autofocus when its popover opens"
    );
    let actions = tree.handle_event(UiEvent::Text(" local".into()));
    assert!(actions.iter().any(|action| matches!(action, UiAction::InputChanged { id, value } if id == "terminal_rename_input:1" && value == "Build local")));
    assert!(tree.handle_event(UiEvent::Key { key: Key::Enter, ctrl: false, shift: false }).iter()
        .any(|action| matches!(action, UiAction::Submitted { id, .. } if id == "terminal_rename_input:1")));
    assert!(clicked(
        &tree.handle_event(UiEvent::Key {
            key: Key::Escape,
            ctrl: false,
            shift: false
        }),
        "terminal_rename_dismiss"
    ));
}

#[test]
fn terminal_actions_menu_to_rename_focuses_once_and_returns_to_session_selector() {
    let mut state = fixture();
    let size = Size::new(900.0, 420.0);
    let mut tree = UiTree::new(build(&state, size.width, size.height));
    tree.set_reduced_motion(true);
    tree.layout(size);
    assert!(clicked(
        &click(&mut tree, "terminal_menu_toggle"),
        "terminal_menu_toggle"
    ));
    state.terminal_menu_open = true;
    tree.set_root(build(&state, size.width, size.height));
    tree.layout(size);
    assert!(clicked(
        &click(&mut tree, "terminal_rename:1"),
        "terminal_rename:1"
    ));
    state.terminal_menu_open = false;
    state.terminal_controls.rename = Some(TerminalRenameView {
        id: 1,
        name: "Build".into(),
        error: None,
    });
    tree.set_root(build(&state, size.width, size.height));
    tree.layout(size);
    assert_eq!(tree.focused_id(), Some("terminal_rename_input:1"));
    assert!(tree.handle_event(UiEvent::Text(" local".into())).iter().any(|action| matches!(action,
        UiAction::InputChanged { id, value } if id == "terminal_rename_input:1" && value == "Build local"
    )));
    assert!(tree.focus("terminal_rename_confirm:1"));
    tree.layout(size);
    assert_eq!(
        tree.focused_id(),
        Some("terminal_rename_confirm:1"),
        "a later frame must not steal focus back into the field"
    );
    assert!(clicked(
        &tree.handle_event(UiEvent::Key {
            key: Key::Escape,
            ctrl: false,
            shift: false
        }),
        "terminal_rename_dismiss"
    ));
    state.terminal_controls.rename = None;
    tree.set_root(build(&state, size.width, size.height));
    tree.layout(size);
    assert_eq!(tree.focused_id(), Some("terminal_sessions_toggle"));
}

#[test]
fn secondary_menu_opens_from_click_and_dispatches_real_terminal_actions() {
    let mut state = fixture();
    let size = Size::new(360.0, 320.0);
    let mut tree = UiTree::new(build(&state, size.width, size.height));
    tree.set_reduced_motion(true);
    tree.layout(size);
    assert!(clicked(
        &click(&mut tree, "terminal_menu_toggle"),
        "terminal_menu_toggle"
    ));
    state.terminal_menu_open = true;
    tree.set_root(build(&state, size.width, size.height));
    let frame = tree.layout(size);
    for id in ["terminal_copy", "terminal_clear", "terminal_interrupt"] {
        let bounds = rect(&frame, id);
        assert!(bounds.x >= 0.0 && bounds.x + bounds.width <= size.width + 0.1);
        assert!(bounds.y >= 0.0 && bounds.y + bounds.height <= size.height + 0.1);
    }
    assert!(clicked(
        &click(&mut tree, "terminal_clear"),
        "terminal_clear"
    ));
    let actions = tree.handle_event(UiEvent::Key {
        key: Key::Escape,
        ctrl: false,
        shift: false,
    });
    assert!(clicked(&actions, "terminal_menu_dismiss"));
}

#[test]
fn narrow_menu_names_the_actual_copy_operation_without_truncation() {
    for selection in [None, Some("selected output".to_owned())] {
        let mut state = fixture();
        state.terminal_menu_open = true;
        let expected = if selection.is_some() {
            "Copiar selección"
        } else {
            "Copiar salida"
        };
        state.terminal_snapshot.as_mut().unwrap().selection = selection;
        let mut tree = UiTree::new(build(&state, 320.0, 320.0));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(320.0, 320.0));
        let bounds = rect(&frame, "terminal_copy");
        assert!(bounds.x >= 0.0 && bounds.x + bounds.width <= 320.0);
        assert!(
            frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, position, .. }
                    if text == expected && bounds.contains(*position)
            )),
            "the complete copy label must remain visible at 320px"
        );
    }
}

#[test]
fn history_and_exit_notices_keep_the_same_terminal_grid_geometry() {
    let mut state = fixture();
    let size = Size::new(320.0, 220.0);
    let mut tree = UiTree::new(build(&state, size.width, size.height));
    tree.set_reduced_motion(true);
    let live = rect(&tree.layout(size), "terminal_screen");
    state
        .terminal_snapshot
        .as_mut()
        .unwrap()
        .scrollback_position = 12;
    tree.set_root(build(&state, size.width, size.height));
    assert_eq!(rect(&tree.layout(size), "terminal_screen"), live);
    assert!(clicked(
        &click(&mut tree, "terminal_bottom"),
        "terminal_bottom"
    ));
    let snapshot = state.terminal_snapshot.as_mut().unwrap();
    snapshot.running = false;
    snapshot.exit_code = Some(7);
    tree.set_root(build(&state, size.width, size.height));
    let frame = tree.layout(size);
    assert_eq!(rect(&frame, "terminal_screen"), live);
    assert!(frame.commands.iter().any(
        |command| matches!(command, DrawCommand::Text { text, .. } if text.contains("cargo run"))
    ));
    assert!(clicked(
        &click(&mut tree, "terminal_restart"),
        "terminal_restart"
    ));
}

#[test]
fn terminal_context_menu_keeps_focus_and_routes_selection_to_its_session() {
    let mut state = fixture();
    state.terminal_snapshot.as_mut().unwrap().selection = Some("cargo run".into());
    let size = Size::new(520.0, 320.0);
    let mut tree = UiTree::new(build(&state, size.width, size.height));
    tree.set_reduced_motion(true);
    let frame = tree.layout(size);
    let screen = rect(&frame, "terminal_screen");
    tree.handle_event(UiEvent::ContextMenu {
        position: Point::new(screen.x + 20.0, screen.y + 30.0),
    });
    tree.layout(size);
    assert_eq!(tree.focused_id(), Some("terminal_screen"));
    assert!(clicked(
        &click(&mut tree, "__context_menu:item:0"),
        "terminal_context:1:copy_selection"
    ));
    assert_eq!(
        state
            .terminal_snapshot
            .as_ref()
            .unwrap()
            .selection
            .as_deref(),
        Some("cargo run")
    );
}

#[test]
fn terminal_split_has_stable_independent_viewports_and_scoped_recovery_actions() {
    let mut state = fixture();
    state.terminal_controls.panes = [1, 2]
        .into_iter()
        .map(|id| TerminalPaneView {
            id,
            snapshot: state.terminal_snapshot.clone(),
            error: None,
        })
        .collect();
    for width in [320.0, 620.0, 1100.0] {
        let size = Size::new(width, 280.0);
        let mut tree = UiTree::new(build(&state, width, size.height));
        tree.set_reduced_motion(true);
        let frame = tree.layout(size);
        let left = rect(&frame, "terminal_screen:1");
        let right = rect(&frame, "terminal_screen:2");
        assert_eq!(left.width, right.width);
        assert_eq!(left.x + left.width + 1.0, right.x);
        assert_eq!(right.x + right.width, width);
        assert_eq!(left.y, HEADER_HEIGHT);
        assert_eq!(left.height, right.height);
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "terminal_screen"));
        tree.handle_event(UiEvent::ContextMenu {
            position: center(right),
        });
        tree.layout(size);
        assert_eq!(tree.focused_id(), Some("terminal_screen:2"));
        assert!(clicked(
            &click(&mut tree, "__context_menu:item:1"),
            "terminal_context:2:copy_output"
        ));
        if width >= 580.0 {
            tree.dismiss_context_menu();
            tree.layout(size);
            let split = rect(tree.frame(), "terminal_split");
            assert_eq!((split.width, split.height), (TOOL_SIZE, TOOL_SIZE));
            assert!(clicked(
                &click(&mut tree, "terminal_split"),
                "terminal_unsplit"
            ));
        }
    }
    state.terminal_controls.panes[1]
        .snapshot
        .as_mut()
        .unwrap()
        .running = false;
    let mut tree = UiTree::new(build(&state, 900.0, 280.0));
    tree.set_reduced_motion(true);
    tree.layout(Size::new(900.0, 280.0));
    assert!(clicked(
        &click(&mut tree, "terminal_restart:2"),
        "terminal_restart:2"
    ));
}

#[test]
fn terminal_active_pane_indicator_is_passive_and_preserves_both_viewport_rects() {
    for width in [320.0, 900.0] {
        let mut state = fixture();
        state.terminal_controls.panes = [1, 2]
            .into_iter()
            .map(|id| TerminalPaneView {
                id,
                snapshot: state.terminal_snapshot.clone(),
                error: None,
            })
            .collect();
        let size = Size::new(width, 280.0);
        let mut tree = UiTree::new(build_panes(&state, width, size.height));
        tree.set_reduced_motion(true);
        let before = tree.layout(size);
        let left = rect(&before, "terminal_screen:1");
        let right = rect(&before, "terminal_screen:2");
        let marker = |frame: &Frame| {
            frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::FillRect { rect, color, .. }
                        if *color == ACTIVE_PANE_INDICATOR =>
                    {
                        Some(*rect)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            marker(&before),
            vec![Rect::new(left.x, left.y, left.width, 2.0)]
        );
        assert_eq!(
            before.hit_test(Point::new(left.x + 20.0, left.y + 1.0)),
            Some("terminal_screen:1")
        );
        for tab in &mut state.terminal_tabs {
            tab.active = tab.id == 2;
        }
        tree.set_root(build_panes(&state, width, size.height));
        let after = tree.layout(size);
        assert_eq!(rect(&after, "terminal_screen:1"), left);
        assert_eq!(rect(&after, "terminal_screen:2"), right);
        assert_eq!(
            after.regions.len(),
            before.regions.len(),
            "passive marker adds no pointer region"
        );
        assert_eq!(
            marker(&after),
            vec![Rect::new(right.x, right.y, right.width, 2.0)]
        );
        assert_eq!(
            after.hit_test(Point::new(right.x + 20.0, right.y + 1.0)),
            Some("terminal_screen:2")
        );
        assert!(
            !after.commands.iter().any(|command| matches!(command,
                DrawCommand::StrokeRect { rect, .. } if *rect == left || *rect == right
            )),
            "active feedback must not reintroduce the outer terminal focus outline"
        );
    }
}

#[test]
#[ignore = "opt-in real-font compact bottom terminal panel visual review"]
fn export_bottom_terminal_visual_review() {
    use eframe::egui;
    let destination = std::path::Path::new("/tmp/openade-bottom-terminal-review");
    std::fs::create_dir_all(destination).unwrap();
    for width in [320.0, 620.0, 1100.0] {
        for mode in [
            "live", "overflow", "history", "menu", "sessions", "profiles", "rename", "split",
            "exited",
        ] {
            let mut state = fixture();
            if mode == "overflow" {
                for tab in &mut state.terminal_tabs {
                    tab.active = tab.id == 7;
                }
                state.terminal_tabs[6].title = "Compilación y pruebas del proyecto".into();
            }
            state.terminal_menu_open = mode == "menu";
            state.terminal_controls.sessions_open = mode == "sessions";
            state.terminal_controls.profiles_open = mode == "profiles";
            state.terminal_controls.profiles = vec![
                TerminalProfileView {
                    id: "default".into(),
                    label: "Bash".into(),
                    is_default: true,
                },
                TerminalProfileView {
                    id: "fish".into(),
                    label: "Fish".into(),
                    is_default: false,
                },
            ];
            if mode == "split" {
                state.terminal_controls.panes = [1, 2]
                    .into_iter()
                    .map(|id| TerminalPaneView {
                        id,
                        snapshot: state.terminal_snapshot.clone(),
                        error: None,
                    })
                    .collect();
            }
            if mode == "rename" {
                state.terminal_controls.rename = Some(TerminalRenameView {
                    id: 1,
                    name: "Build local".into(),
                    error: None,
                });
            }
            if mode == "history" {
                state
                    .terminal_snapshot
                    .as_mut()
                    .unwrap()
                    .scrollback_position = 12;
            }
            if mode == "exited" {
                state.terminal_snapshot.as_mut().unwrap().running = false;
                state.terminal_snapshot.as_mut().unwrap().exit_code = Some(1);
            }
            let height = 320.0;
            let ctx = egui::Context::default();
            crate::fonts::install(&ctx);
            ctx.set_pixels_per_point(2.0);
            ctx.begin_pass(Default::default());
            let mut tree = UiTree::new(build(&state, width, height));
            tree.set_reduced_motion(true);
            let font_context = ctx.clone();
            tree.set_text_measurer(move |text, size, family| {
                font_context.fonts_mut(|fonts| {
                    let family = match family {
                        FontFamily::Sans | FontFamily::SansItalic | FontFamily::SansMedium | FontFamily::SansBold => egui::FontFamily::Proportional,
                        FontFamily::Monospace => egui::FontFamily::Monospace,
                    };
                    fonts
                        .layout_no_wrap(
                            text.to_owned(),
                            egui::FontId::new(size, family),
                            egui::Color32::WHITE,
                        )
                        .size()
                        .x
                })
            });
            let frame = tree.layout(Size::new(width, height));
            let path = destination.join(format!("terminal-{width:.0}-{mode}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height)
                .unwrap();
            let parsed = resvg::usvg::Tree::from_str(
                &std::fs::read_to_string(&path).unwrap(),
                &crate::settings_ui::visual_qa::native_svg_options(),
            )
            .unwrap();
            let mut image = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut image.as_mut(),
            );
            image.save_png(path.with_extension("png")).unwrap();
            ctx.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn terminal_header_tooltips_follow_normal_and_maximized_panel_placement() {
    for maximized in [false, true] {
        let mut state = fixture();
        state.terminal_maximized = maximized;
        let mut tree = UiTree::new(Element::column(vec![
            Element::spacer().size(None, Some(120.0)),
            header(&state, 900.0),
            Element::spacer().flex(1.0),
        ]));
        tree.set_reduced_motion(true);
        let size = Size::new(900.0, 400.0);
        let frame = tree.layout(size);
        let anchor = rect(&frame, "terminal_new");
        assert!(tree.focus("terminal_new"));
        let focused = tree.layout(size);
        let position = focused
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text == "Nueva terminal" => {
                    Some(position)
                }
                _ => None,
            })
            .expect("focused new-terminal tooltip");
        if maximized {
            assert!(position.y >= anchor.y + anchor.height);
        } else {
            assert!(position.y < anchor.y);
        }
    }
}

#[test]
#[ignore = "opt-in real-font terminal session list and independent close review"]
fn export_terminal_sessions_visual_review() {
    use std::time::{Duration, Instant};
    let destination = std::path::Path::new("/tmp/openade-terminal-sessions-review");
    std::fs::create_dir_all(destination).unwrap();
    for width in [320.0, 640.0] {
        for (mode, count) in [
            ("three", 3),
            ("close-hover", 3),
            ("close-focus", 3),
            ("overflow", 12),
        ] {
            let state = session_list_fixture(count);
            let size = Size::new(width, 420.0);
            let (context, mut tree) =
                crate::view::ide_visual_tests::native_tree(build(&state, size.width, size.height));
            let now = Instant::now();
            let frame = tree.layout_at(size, now);
            if mode == "close-hover" {
                tree.handle_event_at(
                    UiEvent::PointerMove(center(rect(&frame, "terminal_session_close:2"))),
                    now,
                );
            } else if mode == "close-focus" {
                assert!(tree.focus("terminal_session_close:2"));
            } else if mode == "overflow" {
                assert!(tree.focus("terminal_tab_menu:1"));
                tree.handle_event(UiEvent::Key {
                    key: Key::End,
                    ctrl: false,
                    shift: false,
                });
            }
            let frame = tree.layout_at(size, now + Duration::from_millis(700));
            let path = destination.join(format!("terminal-sessions-{width:.0}-{mode}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(
                &frame,
                &path,
                size.width,
                size.height,
            )
            .unwrap();
            let parsed = resvg::usvg::Tree::from_str(
                &std::fs::read_to_string(&path).unwrap(),
                &crate::settings_ui::visual_qa::native_svg_options(),
            )
            .unwrap();
            let mut image =
                resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut image.as_mut(),
            );
            image.save_png(path.with_extension("png")).unwrap();
            context.end_pass().textures_delta.clear();
        }
    }
}
