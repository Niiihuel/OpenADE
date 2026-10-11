//! GitHub account controls. Credentials remain owned by the system helpers;
//! disconnecting this menu only disconnects OpenIDE's selected account.

use crate::github_account::{GithubAccountStatus, GithubIdentity};
use crate::{theme, view::ViewState};
use openide_ui::{
    Button, ButtonVariant, Component, ControlSize, DropdownMenu, DropdownMenuEntry,
    DropdownMenuItem, Element, Icon, PopoverPlacement, Separator, Text, TextRole, Tooltip,
};

const MENU_WIDTH: f32 = 288.0;
const TOGGLE: &str = "github_account_menu_toggle";
const DISMISS: &str = "github_account_menu_dismiss";

fn busy(state: &ViewState) -> bool {
    state.github_account_refreshing || state.github_sign_in.is_some()
}

fn menu_header(state: &ViewState, identity: Option<&GithubIdentity>) -> Element {
    let (title, subtitle) = if let Some(identity) = identity {
        (
            format!("@{}", identity.login),
            if state.github_account_refreshing {
                "Actualizando conexión…"
            } else if state.github_account_notice.is_some() {
                "No se pudo actualizar la conexión"
            } else {
                "Perfil verificado · GitHub"
            },
        )
    } else {
        (
            "Cuentas guardadas".to_owned(),
            if state.github_account_refreshing {
                "Actualizando cuentas…"
            } else {
                "Elige una cuenta para verificarla"
            },
        )
    };
    let avatar = if let Some(identity) = identity {
        crate::github_account_ui::avatar(identity, 28.0)
    } else {
        Element::panel(Element::icon(Icon::Github).foreground(theme::FOREGROUND_SUBTLE))
            .padding(6.0)
            .size(Some(28.0), Some(28.0))
            .background(theme::CARD)
            .radius(6.0)
    };
    Element::column(vec![
        Element::row(vec![
            avatar,
            Element::column(vec![
                Text::new(title, theme::component_palette())
                    .role(TextRole::Label)
                    .truncate()
                    .render(),
                Text::new(subtitle, theme::component_palette())
                    .role(TextRole::Small)
                    .muted()
                    .truncate()
                    .render(),
            ])
            .gap(3.0)
            .flex(1.0),
        ])
        .gap(8.0)
        .padding(8.0),
        Separator::horizontal(theme::component_palette()).render(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{
        DrawCommand, FontFamily, Frame, HitKind, Key, Point, Rect, Size, UiAction, UiEvent, UiTree,
    };

    fn connected() -> ViewState {
        let mut state = crate::view::tests::empty_state();
        state.github_account = GithubAccountStatus::Connected(GithubIdentity {
            login: "nihuel".into(),
            avatar: None,
        });
        state.github_account_menu_open = true;
        state
    }

    fn root(state: &ViewState, width: f32, height: f32) -> Element {
        Element::column(vec![
            Element::spacer().size(None, Some((height - 42.0).max(0.0))),
            crate::view::sidebar_account_footer(state, width.min(264.0)),
        ])
        .size(Some(width.min(264.0)), Some(height))
        .background(theme::SIDEBAR)
    }

    fn tree(state: &ViewState, width: f32, height: f32) -> UiTree {
        let mut tree = UiTree::new(root(state, width, height));
        tree.set_reduced_motion(true);
        tree
    }

    fn rect(frame: &Frame, id: &str) -> Rect {
        frame
            .regions
            .iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("missing {id}"))
            .rect
    }

    fn labels(frame: &Frame) -> Vec<&str> {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn key(tree: &mut UiTree, key: Key) -> Vec<UiAction> {
        tree.handle_event(UiEvent::Key {
            key,
            shift: false,
            ctrl: false,
        })
    }

    #[test]
    fn github_account_menu_is_bounded_compact_and_routes_real_actions() {
        for width in [280.0, 360.0, 620.0] {
            let state = connected();
            let mut tree = tree(&state, width, 500.0);
            let frame = tree.layout(Size::new(width, 500.0));
            let anchor = rect(&frame, TOGGLE);
            assert_eq!(anchor.height, 24.0);
            let mut previous_bottom = 0.0;
            for id in [
                "github_account_manage",
                "github_account_open",
                "github_account_refresh",
                "github_account_connect",
                "github_account_disconnect",
            ] {
                let row = rect(&frame, id);
                assert!(
                    row.x >= 0.0 && row.x + row.width <= width + 0.1,
                    "{width}: {id} {row:?}"
                );
                assert!(
                    row.y >= previous_bottom && row.y + row.height < anchor.y,
                    "{id} overlaps"
                );
                assert_eq!(row.height, 26.0, "{id}");
                previous_bottom = row.y + row.height;
                let point = Point::new(row.x + 8.0, row.y + row.height / 2.0);
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked { id: id.into() }]
                );
            }
            assert!(labels(&frame).contains(&"@nihuel"));
            assert!(labels(&frame).contains(&"Perfil verificado · GitHub"));
            assert!(
                !labels(&frame)
                    .iter()
                    .any(|s| s.contains("Cerrar sesión") || s.contains("Cambiar cuenta"))
            );
        }
    }

    #[test]
    fn github_account_menu_escape_outside_and_keyboard_preserve_footer_focus() {
        let mut state = connected();
        let mut tree = tree(&state, 620.0, 500.0);
        tree.layout(Size::new(620.0, 500.0));
        assert!(tree.focus("github_account_manage"));
        key(&mut tree, Key::ArrowDown);
        assert_eq!(tree.focused_id(), Some("github_account_open"));
        assert_eq!(
            key(&mut tree, Key::Enter),
            vec![UiAction::Clicked {
                id: "github_account_open".into()
            }]
        );
        assert_eq!(
            key(&mut tree, Key::Escape),
            vec![UiAction::Clicked { id: DISMISS.into() }]
        );
        state.github_account_menu_open = false;
        tree.set_root(root(&state, 620.0, 500.0));
        tree.layout(Size::new(620.0, 500.0));
        assert_eq!(tree.focused_id(), Some(TOGGLE));
        state.github_account_menu_open = true;
        tree.set_root(root(&state, 620.0, 500.0));
        tree.layout(Size::new(620.0, 500.0));
        let outside = Point::new(500.0, 80.0);
        tree.handle_event(UiEvent::PointerDown(outside));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(outside)),
            vec![UiAction::Clicked { id: DISMISS.into() }]
        );
    }

    #[test]
    fn github_account_menu_refresh_keeps_identity_and_excludes_busy_actions() {
        let mut state = connected();
        for modal in [false, true] {
            state.github_account_refreshing = !modal;
            state.github_sign_in =
                modal.then_some(crate::github_account::GithubSignInView::Preparing);
            let mut tree = tree(&state, 360.0, 500.0);
            let frame = tree.layout(Size::new(360.0, 500.0));
            assert!(labels(&frame).contains(&"@nihuel"));
            for id in ["github_account_refresh", "github_account_connect"] {
                assert!(
                    !frame
                        .regions
                        .iter()
                        .any(|r| r.id == id && r.kind == HitKind::Button),
                    "busy action {id}"
                );
            }
            assert!(
                frame
                    .regions
                    .iter()
                    .any(|r| r.id == "github_account_manage")
            );
            assert!(
                frame
                    .regions
                    .iter()
                    .any(|r| r.id == "github_account_disconnect")
            );
        }
        state.github_account_refreshing = false;
        state.github_sign_in = None;
        state.github_account_notice =
            Some("raw HTTP transport URL should stay out of this menu".into());
        let frame = tree(&state, 360.0, 500.0).layout(Size::new(360.0, 500.0));
        assert!(labels(&frame).contains(&"No se pudo actualizar la conexión"));
        assert!(!labels(&frame).iter().any(|s| s.contains("raw HTTP")));
    }

    #[test]
    fn github_account_menu_cached_accounts_require_verification_without_profile_actions() {
        let mut state = connected();
        state.github_account = GithubAccountStatus::Unverified {
            accounts: vec!["alice".into(), "bob".into()],
            notice: None,
        };
        let mut tree = tree(&state, 360.0, 500.0);
        let frame = tree.layout(Size::new(360.0, 500.0));
        for id in [
            "github_account_verify:alice",
            "github_account_verify:bob",
            "github_account_manage",
            "github_account_refresh",
            "github_account_connect",
        ] {
            assert!(frame.regions.iter().any(|r| r.id == id), "{id}");
        }
        for id in ["github_account_open", "github_account_disconnect"] {
            assert!(!frame.regions.iter().any(|r| r.id == id), "{id}");
        }
        assert!(tree.focus("github_account_verify:alice"));
        assert_eq!(
            key(&mut tree, Key::Enter),
            vec![UiAction::Clicked {
                id: "github_account_verify:alice".into()
            }]
        );
    }

    #[test]
    fn github_account_footer_handles_empty_cached_accounts_and_unavailable_states() {
        let mut state = connected();
        for status in [
            GithubAccountStatus::Disconnected,
            GithubAccountStatus::Unverified {
                accounts: vec![],
                notice: None,
            },
            GithubAccountStatus::Unavailable("technical error".into()),
        ] {
            state.github_account = status;
            let frame = tree(&state, 360.0, 500.0).layout(Size::new(360.0, 500.0));
            assert!(
                frame
                    .regions
                    .iter()
                    .any(|r| r.id == "github_account_connect")
            );
            assert!(
                !frame
                    .regions
                    .iter()
                    .any(|r| r.id.starts_with("github_account_verify:"))
            );
            assert!(!labels(&frame).contains(&"technical error"));
        }
        for status in [
            GithubAccountStatus::Checking,
            GithubAccountStatus::Verifying("alice".into()),
        ] {
            state.github_account = status;
            let frame = tree(&state, 360.0, 500.0).layout(Size::new(360.0, 500.0));
            assert!(
                !frame
                    .regions
                    .iter()
                    .any(|r| r.id == "github_account_connect" && r.kind == HitKind::Button)
            );
        }
    }

    #[test]
    fn github_account_menu_short_viewport_keeps_last_action_reachable() {
        let state = connected();
        let mut tree = tree(&state, 280.0, 240.0);
        tree.layout(Size::new(280.0, 240.0));
        assert!(tree.focus("github_account_manage"));
        key(&mut tree, Key::End);
        let frame = tree.layout(Size::new(280.0, 240.0));
        assert_eq!(tree.focused_id(), Some("github_account_disconnect"));
        let row = rect(&frame, "github_account_disconnect");
        let anchor = rect(&frame, TOGGLE);
        assert!(row.y >= 0.0 && row.y + row.height < anchor.y);
        assert_eq!(
            key(&mut tree, Key::Enter),
            vec![UiAction::Clicked {
                id: "github_account_disconnect".into()
            }]
        );
    }

    fn native_tree(element: Element) -> (eframe::egui::Context, UiTree) {
        use eframe::egui;
        let context = egui::Context::default();
        crate::fonts::install(&context);
        context.begin_pass(egui::RawInput::default());
        let fonts = context.clone();
        let mut tree = UiTree::new(element);
        tree.set_reduced_motion(true);
        tree.set_text_measurer(move |text, size, family| {
            fonts.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(
                        text.into(),
                        egui::FontId::new(
                            size,
                            match family {
                                FontFamily::Sans | FontFamily::SansItalic => egui::FontFamily::Proportional,
                                FontFamily::Monospace => egui::FontFamily::Monospace,
                            },
                        ),
                        egui::Color32::WHITE,
                    )
                    .size()
                    .x
            })
        });
        (context, tree)
    }

    #[test]
    #[ignore = "GitHub account menu native-font visual artifacts"]
    fn export_github_account_menu_native_visual_review() {
        let directory = std::path::Path::new("/tmp/openide-github-account-review");
        std::fs::create_dir_all(directory).unwrap();
        for width in [280.0, 360.0, 620.0] {
            for name in [
                "connected",
                "refreshing",
                "refresh-error",
                "cached",
                "disconnected",
                "error",
            ] {
                let mut state = connected();
                match name {
                    "refreshing" => state.github_account_refreshing = true,
                    "refresh-error" => state.github_account_notice = Some("fixture".into()),
                    "cached" => {
                        state.github_account = GithubAccountStatus::Unverified {
                            accounts: vec!["alice".into(), "bob".into()],
                            notice: None,
                        }
                    }
                    "disconnected" => state.github_account = GithubAccountStatus::Disconnected,
                    "error" => {
                        state.github_account = GithubAccountStatus::Unavailable("fixture".into())
                    }
                    _ => (),
                }
                let (context, mut tree) = native_tree(root(&state, width, 420.0));
                let frame = tree.layout(Size::new(width, 420.0));
                let path = directory.join(format!("menu-{name}-{width}.svg"));
                crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, 420.0)
                    .unwrap();
                let svg = std::fs::read(&path).unwrap();
                let svg_tree = resvg::usvg::Tree::from_data(
                    &svg,
                    &crate::settings_ui::visual_qa::native_svg_options(),
                )
                .unwrap();
                let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 420).unwrap();
                resvg::render(
                    &svg_tree,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixmap.as_mut(),
                );
                pixmap.save_png(path.with_extension("png")).unwrap();
                context.end_pass().textures_delta.clear();
            }
        }
    }
}

fn menu(
    state: &ViewState,
    anchor: Element,
    identity: Option<&GithubIdentity>,
    entries: Vec<DropdownMenuEntry>,
) -> Element {
    DropdownMenu::new(
        anchor,
        entries,
        state.github_account_menu_open,
        DISMISS,
        theme::menu_palette(),
    )
    .context_menu_style()
    .width(MENU_WIDTH)
    .header(menu_header(state, identity))
    .placement(PopoverPlacement::TopStart)
    .render()
    .popover_return_focus(TOGGLE)
}

fn connection_button(
    id: &str,
    title: &str,
    hint: &str,
    width: f32,
    loading: bool,
    dropdown: bool,
) -> Element {
    let mut button = Button::new(
        id,
        title,
        theme::action_palette(theme::SIDEBAR, theme::FOREGROUND),
    )
    .icon(Icon::Github)
    .variant(ButtonVariant::Subtle)
    .size(ControlSize::Dense)
    .width(width)
    .loading(loading);
    if dropdown {
        button = button.trailing_icon(Icon::ChevronDown);
    }
    // Busy, the spinner keeps the Dense gap it always had.
    if !loading {
        button = button.gap(8.0);
    }
    Tooltip::new(button.render(), id, hint, theme::component_palette()).render()
}

pub(crate) fn build(state: &ViewState, width: f32, sidebar_width: f32) -> Element {
    let narrow = sidebar_width < 230.0;
    match &state.github_account {
        GithubAccountStatus::Connected(identity) => {
            let anchor = Element::clickable(
                TOGGLE,
                Element::row(vec![
                    crate::github_account_ui::avatar(identity, 20.0),
                    Text::new(&identity.login, theme::component_palette())
                        .role(TextRole::Label)
                        .truncate()
                        .render()
                        .flex(1.0),
                    Element::panel(
                        Element::icon(Icon::ChevronDown).foreground(theme::FOREGROUND_SUBTLE),
                    )
                    .size(Some(16.0), Some(20.0)),
                ])
                .gap(6.0)
                .padding(2.0)
                .size(Some(width), Some(24.0)),
            )
            .size(Some(width), Some(24.0))
            .background(theme::SIDEBAR)
            .hover_background(theme::hover_for(theme::SIDEBAR))
            .pressed_background(theme::SELECTED)
            .transition_ms(120)
            .radius(6.0);
            let anchor = Tooltip::new(
                theme::action_palette(theme::SIDEBAR, theme::FOREGROUND)
                    .interactive_surface(anchor),
                TOGGLE,
                format!("Cuenta de GitHub: {}", identity.login),
                theme::component_palette(),
            )
            .render();
            let busy = busy(state);
            menu(
                state,
                anchor,
                Some(identity),
                vec![
                    DropdownMenuItem::new("github_account_manage", "Administrar GitHub")
                        .icon(Icon::Settings)
                        .into(),
                    DropdownMenuItem::new("github_account_open", "Ver perfil en GitHub")
                        .icon(Icon::ArrowRight)
                        .into(),
                    DropdownMenuEntry::Separator,
                    DropdownMenuItem::new("github_account_refresh", "Actualizar conexión")
                        .icon(Icon::Refresh)
                        .enabled(!busy)
                        .into(),
                    DropdownMenuItem::new("github_account_connect", "Conectar otra cuenta…")
                        .icon(Icon::Add)
                        .enabled(!busy)
                        .into(),
                    DropdownMenuEntry::Separator,
                    DropdownMenuItem::new("tour_start:agents", "Recorrido guiado")
                        .icon(Icon::InfoCircle)
                        .into(),
                    DropdownMenuItem::new("github_account_disconnect", "Desconectar de OpenIDE")
                        .icon(Icon::CloseCircle)
                        .into(),
                ],
            )
        }
        GithubAccountStatus::Unverified { accounts, notice } if !accounts.is_empty() => {
            let many = accounts.len() > 1;
            let reconnect = !many && notice.is_some();
            let action_id = if many {
                TOGGLE.to_owned()
            } else if reconnect {
                "github_account_connect".to_owned()
            } else {
                format!("github_account_verify:{}", accounts[0])
            };
            let title = match (many, reconnect, narrow) {
                (true, _, true) => "Elegir cuenta",
                (true, _, false) => "Elegir cuenta GitHub",
                (_, true, true) => "Reconectar",
                (_, true, false) => "Reconectar GitHub",
                (_, _, true) => "Verificar",
                _ => "Verificar GitHub",
            };
            let hint = if notice.is_some() {
                "No se pudo verificar la cuenta. Reconecta con GitHub."
            } else if many {
                "Hay varias cuentas guardadas. Elige cuál verificar."
            } else {
                "Cuenta guardada sin verificar. Comprueba la conexión."
            };
            let anchor =
                connection_button(&action_id, title, hint, width, busy(state) && !many, many);
            if !many {
                return anchor;
            }
            let mut entries: Vec<_> = accounts
                .iter()
                .map(|login| {
                    DropdownMenuItem::new(
                        format!("github_account_verify:{login}"),
                        format!("@{login}"),
                    )
                    .icon(Icon::Github)
                    .enabled(!busy(state))
                    .into()
                })
                .collect();
            entries.extend([
                DropdownMenuEntry::Separator,
                DropdownMenuItem::new("github_account_manage", "Administrar GitHub")
                    .icon(Icon::Settings)
                    .into(),
                DropdownMenuItem::new("github_account_refresh", "Actualizar cuentas")
                    .icon(Icon::Refresh)
                    .enabled(!busy(state))
                    .into(),
                DropdownMenuItem::new("github_account_connect", "Conectar otra cuenta…")
                    .icon(Icon::Add)
                    .enabled(!busy(state))
                    .into(),
            ]);
            menu(state, anchor, None, entries)
        }
        status => {
            let (hint, loading) = match status {
                GithubAccountStatus::Checking => ("Buscando una cuenta de GitHub", true),
                GithubAccountStatus::Verifying(_) => ("Validando la cuenta con GitHub", true),
                GithubAccountStatus::Unavailable(_) => (
                    "No se pudo conectar con GitHub. Inicia sesión para volver a intentarlo.",
                    false,
                ),
                _ => ("Iniciar sesión con GitHub", false),
            };
            connection_button(
                "github_account_connect",
                if narrow {
                    "Conectar"
                } else {
                    "Conectar con GitHub"
                },
                hint,
                width,
                loading || busy(state),
                false,
            )
        }
    }
}
