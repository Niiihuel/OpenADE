//! The hand-built sidebar rows that limn's `ListRow` and `TreeRow` replaced,
//! kept so each replacement stays provably identical: same draw commands
//! idle, hovered, with the hint open and pressed, and the same action on
//! click. Delete with the final audit (S18).

use super::*;
use crate::icon_button_parity_tests::assert_same;
use openide_ui::{Point, Size, UiEvent, UiTree, ICON_SIZE};

fn legacy_session_row(state: &SessionRowContext, session: &SessionView, width: f32) -> Element {
    let selected = state.selected_session.as_deref() == Some(session.id.as_str());
    let menu_open = state.session_menu_id.as_deref() == Some(session.id.as_str());
    let row_width = (width - 18.0).max(0.0);
    let (status_marker, menu_button) = session_row_parts(state, session, row_width);
    let menu_action_id = format!("session_menu:{}", session.id);
    let row = Element::clickable(
        format!("session:{}", session.id),
        Element::row(vec![
            status_marker,
            Element::faded_text(&session.title, 24.0)
                .foreground(if selected || session.unread {
                    theme::BRAND
                } else {
                    theme::FOREGROUND
                })
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .min_size(0.0, 0.0)
                .size(None, Some(SIDEBAR_ROW_HEIGHT))
                .flex(1.0),
            menu_button,
        ])
        .gap(4.0)
        .size(Some(row_width), Some(SIDEBAR_ROW_HEIGHT)),
    )
    .size(Some(row_width), Some(SIDEBAR_ROW_HEIGHT))
    .background(if selected || menu_open {
        theme::SELECTED
    } else {
        theme::SIDEBAR
    })
    .hover_background(if selected || menu_open {
        theme::hover_for(theme::SELECTED)
    } else {
        theme::hover_for(theme::SIDEBAR)
    })
    .pressed_background(theme::SELECTED)
    .hover_with(menu_action_id)
    .radius(8.0);
    theme::action_palette(
        if selected || menu_open {
            theme::SELECTED
        } else {
            theme::SIDEBAR
        },
        theme::FOREGROUND,
    )
    .interactive_surface(row)
}

fn legacy_project_header(project: &str, count: usize, collapsed: bool, width: f32) -> Element {
    let row_width = (width - 18.0).max(0.0);
    let action_id = format!("session_project_toggle:{project}");
    let trailing = Element::row(vec![
        Element::truncated_text(count.to_string())
            .foreground(theme::FOREGROUND_SUBTLE)
            .font_family(FontFamily::Sans)
            .font_size(10.0),
        Element::panel(
            Element::icon(if collapsed {
                Icon::ChevronRight
            } else {
                Icon::ChevronDown
            })
            .foreground(theme::FOREGROUND_SUBTLE),
        )
        .size(Some(ICON_SIZE), Some(SIDEBAR_ROW_HEIGHT)),
    ])
    .gap(6.0)
    .size(None, Some(SIDEBAR_ROW_HEIGHT));
    TreeRow::new(
        move |content| Element::clickable(action_id, content),
        Element::icon(openide_ui::folder_icon(project, !collapsed))
            .foreground(theme::FOREGROUND_SUBTLE),
        Element::truncated_text(workspace_name(project))
            .foreground(theme::FOREGROUND)
            .font_family(FontFamily::Sans)
            .font_size(12.0),
        TreeGuides::root(),
        theme::action_palette(theme::SIDEBAR, theme::FOREGROUND),
    )
    .trailing(trailing)
    .width(row_width)
    .height(SIDEBAR_ROW_HEIGHT)
    .render()
}

fn legacy_sidebar_nav_action(
    id: &str,
    icon: Icon,
    title: &str,
    shortcut: &str,
    width: f32,
    selected: bool,
) -> Element {
    let icon = Element::icon(icon)
        .foreground(if selected {
            theme::FOREGROUND
        } else {
            theme::FOREGROUND_SUBTLE
        })
        .size(Some(16.0), Some(16.0));
    let kbd = Kbd::new(shortcut, theme::component_palette())
        .max_width((width - 92.0).max(0.0))
        .render();
    let centered = |element: Element| element.centered_y().size(None, Some(SIDEBAR_ROW_HEIGHT));
    let row = Element::clickable(
        id,
        Element::row(vec![
            Element::spacer().size(Some(4.0), None),
            Element::row(vec![
                centered(icon).size(Some(16.0), Some(SIDEBAR_ROW_HEIGHT)),
                Element::truncated_text(title)
                    .foreground(theme::FOREGROUND)
                    .font_family(FontFamily::Sans)
                    .font_size(13.0)
                    .flex(1.0),
                centered(kbd),
            ])
            .gap(8.0)
            .flex(1.0),
            Element::spacer().size(Some(8.0), None),
        ])
        .size(Some(width), Some(SIDEBAR_ROW_HEIGHT)),
    )
    .size(Some(width), Some(SIDEBAR_ROW_HEIGHT))
    .background(if selected {
        theme::SELECTED
    } else {
        theme::SIDEBAR
    })
    .hover_background(if selected {
        theme::SELECTED
    } else {
        theme::hover_for(theme::SIDEBAR)
    })
    .pressed_background(theme::SELECTED)
    .radius(8.0);
    theme::action_palette(
        if selected {
            theme::SELECTED
        } else {
            theme::SIDEBAR
        },
        theme::FOREGROUND,
    )
    .interactive_surface(row)
}

fn session(title: &str) -> SessionView {
    SessionView {
        id: "s1".into(),
        title: title.into(),
        project: "C:/work/openIDE".into(),
        backend: "native".into(),
        pinned: false,
        archived: false,
        unread: false,
        native_automation: None,
    }
}

#[test]
fn session_rows_match_their_legacy_body() {
    let long = "Una tarea con un título tan largo que no entra en ninguna barra lateral razonable";
    for width in [200.0, 264.0, 420.0] {
        for title in ["Corta", long] {
            let idle = session(title);
            let unread = SessionView { unread: true, ..idle.clone() };
            let pinned = SessionView { pinned: true, ..idle.clone() };
            let cli = SessionView { backend: format!("{CLI_BACKEND}codex"), ..idle.clone() };
            let cases: [(&str, &SessionView, fn(&mut ViewState)); 7] = [
                ("idle", &idle, |_| {}),
                ("selected", &idle, |state| state.selected_session = Some("s1".into())),
                ("running", &idle, |state| {
                    state.selected_session = Some("s1".into());
                    state.running = true;
                }),
                ("unread", &unread, |_| {}),
                ("pinned", &pinned, |_| {}),
                ("cli", &cli, |_| {}),
                ("menu open", &idle, |state| state.session_menu_id = Some("s1".into())),
            ];
            for (name, session, prepare) in cases {
                let mut state = tests::empty_state();
                prepare(&mut state);
                let context = SessionRowContext::from(&state);
                let name = format!("session row {name} {title:?} at {width}");
                let (legacy, current) = (
                    legacy_session_row(&context, session, width),
                    session_row_with_context(&context, session, width),
                );
                assert_same(&name, legacy.clone(), current.clone());
                // The row stays hovered under its menu button.
                let over_menu = |row: Element| {
                    let mut tree = UiTree::new(Element::column(vec![row]));
                    tree.set_reduced_motion(true);
                    let size = Size::new(480.0, 400.0);
                    let menu = tree
                        .layout(size)
                        .regions
                        .iter()
                        .find(|region| region.id == "session_menu:s1")
                        .unwrap()
                        .rect;
                    let point = Point::new(menu.x + menu.width / 2.0, menu.y + menu.height / 2.0);
                    tree.handle_event(UiEvent::PointerMove(point));
                    tree.layout(size)
                };
                assert_eq!(over_menu(current), over_menu(legacy), "{name} over its menu");
            }
        }
    }
}

#[test]
fn project_headers_match_their_legacy_body() {
    for width in [200.0, 264.0] {
        for (count, collapsed) in [(3, false), (128, true)] {
            assert_same(
                &format!("project header {count} collapsed={collapsed} at {width}"),
                legacy_project_header("C:/work/openIDE", count, collapsed, width),
                project_header("C:/work/openIDE", count, collapsed, width),
            );
        }
    }
}

#[test]
fn sidebar_nav_actions_match_their_legacy_body() {
    for width in [120.0, 228.0, 400.0] {
        for selected in [false, true] {
            for (title, shortcut) in [("Nueva tarea", "Ctrl+N"), ("Complementos y más cosas", "")] {
                assert_same(
                    &format!("nav {title:?} selected={selected} at {width}"),
                    legacy_sidebar_nav_action("nav", Icon::Add, title, shortcut, width, selected),
                    sidebar_nav_action("nav", Icon::Add, title, shortcut, width, selected),
                );
            }
        }
    }
}
