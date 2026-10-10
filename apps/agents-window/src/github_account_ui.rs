//! Native account chrome and the browser sign-in alert.

use openide_ui::{
    AlertDialog, Avatar, Button, ButtonVariant, Component, ControlSize, Element, FontFamily, Modal,
    Spinner,
};

use crate::{
    github_account::{GithubIdentity, GithubSignInView},
    theme,
    view::ViewState,
};

pub fn avatar(identity: &GithubIdentity, size: f32) -> Element {
    let mut avatar = Avatar::new(&identity.login, theme::component_palette()).size(size);
    if let Some(photo) = &identity.avatar {
        avatar = avatar.image(photo.clone());
    }
    avatar.render()
}

pub fn sign_in_alert(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    let Some(sign_in) = &state.github_sign_in else {
        return base;
    };
    let width = Modal::fit_width(viewport_width, 440.0);
    let text = |value: &str| {
        Element::text(value)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::SETTINGS_FOREGROUND_SUBTLE)
    };
    let progress = |id: &str, message: &str| {
        Element::row(vec![
            Spinner::new(id, theme::SETTINGS_FOREGROUND_SUBTLE).render(),
            text(message).flex(1.0),
        ])
        .gap(8.0)
    };
    if let GithubSignInView::ChooseAccount { accounts, notice } = sign_in {
        let mut rows = Vec::new();
        if let Some(notice) = notice {
            rows.push(text(notice));
        }
        for login in accounts {
            rows.push(
                Button::new(
                    format!("github_account_verify:{login}"),
                    format!("@{login}"),
                    theme::menu_palette(),
                )
                .icon(openide_ui::Icon::Github)
                .trailing_icon(openide_ui::Icon::ChevronRight)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .render(),
            );
        }
        let focus = accounts
            .first()
            .map(|login| format!("github_account_verify:{login}"))
            .unwrap_or_else(|| "github_sign_in_cancel".into());
        return Modal::new(
            base,
            true,
            "Elige una cuenta de GitHub",
            "",
            "github_sign_in_cancel",
            theme::component_palette(),
        )
        .width(width)
        .body(Element::column(rows).gap(6.0))
        .footer(Element::row(vec![
            Element::spacer().flex(1.0),
            Button::new(
                "github_sign_in_cancel",
                "Cancelar",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .render(),
        ]))
        .initial_focus(focus)
        .backdrop_dismiss(false)
        .render();
    }
    let (description, body, confirm_id, confirm_label, enabled) = match sign_in {
        GithubSignInView::ChooseAccount { .. } => unreachable!("account choice has its own dialog"),
        GithubSignInView::Preparing => (
            "Preparando el acceso seguro a tu cuenta",
            Element::column(vec![progress(
                "github_sign_in_prepare_progress",
                "En un momento aparecerá tu código de un solo uso.",
            )])
            .gap(10.0),
            "github_sign_in_continue",
            "Preparando",
            false,
        ),
        GithubSignInView::Code { code, continued } => {
            let code_card = Element::panel(
                Element::row(vec![
                    Element::spacer().flex(1.0),
                    Element::text(code)
                        .font_family(FontFamily::Monospace)
                        .font_size(22.0)
                        .foreground(theme::FOREGROUND),
                    Element::spacer().flex(1.0),
                    Button::new("github_sign_in_copy", "Copiar", theme::component_palette())
                        .variant(ButtonVariant::Subtle)
                        .size(ControlSize::Compact)
                        .render(),
                ])
                .gap(10.0),
            )
            .padding(10.0)
            .background(theme::INPUT)
            .border(theme::TREE_GUIDE, 1.0)
            .radius(6.0);
            (
                if *continued {
                    "Completa la autorización en GitHub. Esta ventana se cerrará al verificar tu cuenta."
                } else {
                    "Copia este código y pégalo en GitHub para autorizar el acceso a tu cuenta."
                },
                Element::column(vec![
                    code_card,
                    text("1. Abre github.com/login/device en el navegador.\n2. Introduce el código y autoriza OpenIDE."),
                    if *continued {
                        progress(
                            "github_sign_in_authorize_progress",
                            "Esperando tu autorización en el navegador",
                        )
                    } else {
                        text("El código es temporal y sólo se puede usar una vez.")
                    },
                ])
                .gap(10.0),
                "github_sign_in_continue",
                if *continued {
                    "Abrir navegador"
                } else {
                    "Copiar y abrir navegador"
                },
                true,
            )
        }
        GithubSignInView::Browser => (
            "Completa el inicio de sesión en el navegador. Verificaremos tu cuenta al volver.",
            Element::column(vec![progress(
                "github_sign_in_browser_progress",
                "Esperando la autorización de GitHub",
            )])
            .gap(10.0),
            "github_sign_in_continue",
            "Esperando",
            false,
        ),
        GithubSignInView::Error(reason) => (
            "No se pudo completar la conexión con GitHub.",
            Element::formatted_text(vec![openide_ui::TextSpan::new(
                reason,
                theme::SETTINGS_FOREGROUND_SUBTLE,
                12.0,
                FontFamily::Sans,
            )]),
            "github_sign_in_retry",
            "Reintentar",
            true,
        ),
    };
    let body = if let crate::github_account::GithubAccountStatus::Connected(identity) =
        &state.github_account
    {
        Element::column(vec![
            body,
            text(&format!(
                "@{} seguirá conectado hasta verificar la nueva cuenta.",
                identity.login
            )),
        ])
        .gap(10.0)
    } else {
        body
    };
    let alert = AlertDialog::new(
        base,
        true,
        "Conectar con GitHub",
        description,
        confirm_id,
        "github_sign_in_cancel",
        theme::component_palette(),
    )
    .width(width)
    .body(body)
    .confirm_label(confirm_label)
    .confirm_enabled(enabled)
    .cancel_label("Cancelar")
    .render();
    alert.popover_initial_focus(if enabled {
        confirm_id
    } else {
        "github_sign_in_cancel"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github_account::GithubAccountStatus;
    use openide_ui::{HitKind, Key, Size, UiAction, UiEvent, UiTree};

    fn states() -> Vec<(&'static str, GithubSignInView)> {
        vec![
            ("preparing", GithubSignInView::Preparing),
            (
                "code",
                GithubSignInView::Code {
                    code: "ABCD-1234".into(),
                    continued: false,
                },
            ),
            (
                "waiting",
                GithubSignInView::Code {
                    code: "ABCD-1234".into(),
                    continued: true,
                },
            ),
            ("browser", GithubSignInView::Browser),
            (
                "choice",
                GithubSignInView::ChooseAccount {
                    accounts: vec!["alice".into(), "bob".into()],
                    notice: Some("Elige la cuenta que quieres conectar.".into()),
                },
            ),
            (
                "error",
                GithubSignInView::Error(
                    "El código venció. Solicita uno nuevo para continuar.".into(),
                ),
            ),
        ]
    }
    fn scene(state: &ViewState, width: f32) -> Element {
        sign_in_alert(
            state,
            Element::panel(Element::spacer())
                .size(Some(width), Some(520.0))
                .background(theme::BACKGROUND),
            width,
        )
    }
    #[test]
    fn github_sign_in_keeps_cancel_accessible_and_only_exposes_ready_actions() {
        for width in [280.0, 360.0, 620.0] {
            for (name, sign_in) in states() {
                let mut state = crate::view::tests::empty_state();
                state.github_sign_in = Some(sign_in);
                let mut tree = UiTree::new(scene(&state, width));
                tree.set_reduced_motion(true);
                let frame = tree.layout(Size::new(width, 520.0));
                let cancel = frame
                    .regions
                    .iter()
                    .find(|r| r.id == "github_sign_in_cancel" && r.kind == HitKind::Button)
                    .unwrap();
                assert!(
                    cancel.rect.x >= 0.0
                        && (cancel.rect.x + cancel.rect.width) <= width
                        && (cancel.rect.y + cancel.rect.height) <= 520.0,
                    "{name}"
                );
                let action = match name {
                    "choice" => "github_account_verify:alice",
                    "error" => "github_sign_in_retry",
                    _ => "github_sign_in_continue",
                };
                assert_eq!(
                    frame
                        .regions
                        .iter()
                        .any(|r| r.id == action && r.kind == HitKind::Button),
                    !matches!(name, "preparing" | "browser")
                );
                assert_eq!(
                    tree.handle_event(UiEvent::Key {
                        key: Key::Escape,
                        shift: false,
                        ctrl: false
                    }),
                    vec![UiAction::Clicked {
                        id: "github_sign_in_cancel".into()
                    }]
                );
            }
        }
    }
    #[test]
    #[ignore = "native GitHub sign-in and management visual artifacts"]
    fn export_github_management_visual_review() {
        let output = std::path::Path::new("/tmp/openide-github-account-review");
        std::fs::create_dir_all(output).unwrap();
        for width in [360.0, 900.0] {
            let mut cases = Vec::new();
            for (name, sign_in) in states() {
                let mut state = crate::view::tests::empty_state();
                state.github_account = GithubAccountStatus::Connected(GithubIdentity {
                    login: "alice".into(),
                    avatar: None,
                });
                state.github_sign_in = Some(sign_in);
                cases.push((format!("signin-{name}"), scene(&state, width), 520.0));
            }
            let mut state = crate::view::tests::empty_state();
            state.github_account = GithubAccountStatus::Connected(GithubIdentity {
                login: "alice".into(),
                avatar: None,
            });
            state.show_settings = true;
            cases.push((
                "settings-connected".into(),
                crate::settings_ui::build_settings_page(
                    &state,
                    crate::settings_ui::SettingsPage::Github,
                    width,
                    false,
                ),
                900.0,
            ));
            for (name, scene, height) in cases {
                let (context, mut tree) = crate::view::ide_visual_tests::native_tree(scene);
                let frame = tree.layout(Size::new(width, height));
                let path = output.join(format!("{name}-{width}.svg"));
                crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height)
                    .unwrap();
                let parsed = resvg::usvg::Tree::from_data(
                    &std::fs::read(&path).unwrap(),
                    &crate::settings_ui::visual_qa::native_svg_options(),
                )
                .unwrap();
                let mut pixmap =
                    resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
                resvg::render(
                    &parsed,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixmap.as_mut(),
                );
                pixmap.save_png(path.with_extension("png")).unwrap();
                context.end_pass().textures_delta.clear();
            }
        }
    }
}
