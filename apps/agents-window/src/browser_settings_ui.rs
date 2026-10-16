//! Native browser settings describe only capabilities enforced by the host.
use crate::{
    browser_sites::SiteAccess, settings_parts, settings_ui, theme, ui_sizing::NOTE_ROW,
    view::ViewState,
};
use openide_ui::{
    ActionRow, Button, ButtonVariant, Component, ControlSize, DropdownOption, Element, Icon,
    IconButton, Select, SelectVariant, SelectorSizing, TextField,
};

fn note(title: &str, description: &str, icon: Icon) -> Element {
    ActionRow::new(title, theme::card_palette())
        .subtitle(description)
        .icon(icon)
        .icon_muted(true)
        .align_top(true)
        .wrap_title(true)
        .sizing(NOTE_ROW)
        .render()
}

fn access_label(access: SiteAccess) -> &'static str {
    match access {
        SiteAccess::Allow => "Permitir siempre",
        SiteAccess::Ask => "Permisos del agente",
        SiteAccess::Block => "Bloquear",
    }
}

fn access_selector(
    state: &ViewState,
    index: Option<usize>,
    access: SiteAccess,
    width: f32,
) -> Element {
    let key = index.map_or_else(
        || "browser_site_draft".into(),
        |index| format!("browser_site:{index}"),
    );
    Select::new(
        format!("settings_choice_toggle:{key}"),
        access_label(access),
        SiteAccess::ALL
            .into_iter()
            .map(|choice| {
                let id = index.map_or_else(
                    || format!("browser_site_draft_access:{}", choice.key()),
                    |index| format!("browser_site_set:{index}:{}", choice.key()),
                );
                DropdownOption::new(id, access_label(choice)).selected(access == choice)
            })
            .collect(),
        state.settings_choice_menu_open.as_deref() == Some(key.as_str()),
        "settings_choice_dismiss",
        theme::control_palette(),
    )
    .variant(SelectVariant::Field)
    .sizing(SelectorSizing::COMPACT)
    .width(width.clamp(0.0, 240.0))
    .menu_width(280.0)
    .accessibility_label(index.map_or_else(
        || "Permiso del nuevo sitio".into(),
        |index| format!("Permiso de {}", state.browser_sites[index].origin),
    ))
    .menu_palette(theme::menu_palette())
    .enabled(state.browser_sites_error.is_none())
    .render()
}

pub(crate) fn page(state: &ViewState, compact: bool, width: f32) -> Vec<Element> {
    let editable = state.browser_sites_error.is_none();
    let control_width = (width - settings_parts::ROW_PADDING * 2.0).max(0.0);
    let mut groups = vec![settings_parts::settings_group(
        "Automatización del navegador",
        vec![
            settings_parts::switch_row(
                "browser_automation",
                "Permitir automatización",
                "Ofrece las herramientas de navegador al agente. Desactivarla detiene nuevas acciones de automatización, incluidas las de clientes ya conectados; podés seguir usando el navegador manualmente.",
                state.browser_automation_enabled,
                true,
                compact,
            ),
            note(
                "Alcance de los permisos",
                "Las herramientas leen la pestaña de esta conversación. Las acciones siguen los permisos del agente y los permisos por sitio; un sitio bloqueado permanece cerrado al agente incluso con «Permitir todo».",
                Icon::InfoCircle,
            ),
        ],
    )];
    groups.push(settings_parts::settings_group("Enlaces", vec![settings_parts::settings_row(
        "Abrir enlaces del chat en",
        "Automático abre en el navegador integrado las direcciones de este equipo, como localhost, y el resto en el del sistema. Ctrl+clic usa el otro; el clic derecho ofrece ambos.",
        settings_ui::settings_choice_selector(state, "chat_links", state.chat_link_opening.label(), state.chat_link_opening.options()), compact,
    )]));
    let mut sites = vec![note(
        "Predeterminado",
        "Los sitios sin permiso propio siguen los permisos del agente: la lectura no pregunta y las acciones requieren aprobación salvo con «Permitir todo».",
        Icon::CheckCircle,
    )];
    if state
        .browser_sites
        .iter()
        .any(|site| site.access == SiteAccess::Block)
    {
        sites.push(note("Datos de todo el navegador", "Mientras haya sitios bloqueados, el agente no puede leer la lista de descargas ni el portapapeles del navegador porque no tienen un origen verificable. Vos podés seguir usándolos.", Icon::InfoCircle));
    }
    if let Some(error) = &state.browser_sites_error {
        sites.push(note(
            "Automatización bloqueada",
            &format!("No se pudieron cargar los permisos por sitio. {error}"),
            Icon::WarningTriangle,
        ));
        sites.push(note("Conservar las reglas", "Podés corregir el archivo y recargarlo, o restablecer los permisos guardando una copia del archivo original. Mientras tanto, el navegador manual sigue disponible.", Icon::InfoCircle));
        let recovery = vec![
            settings_ui::outline("browser_site_reload", "Recargar archivo").render(),
            settings_ui::outline("browser_site_reset", "Restablecer permisos").render(),
        ];
        sites.push(
            if compact {
                Element::column(recovery)
            } else {
                Element::row(recovery)
            }
            .gap(6.0)
            .padding(settings_parts::ROW_PADDING),
        );
        if state.browser_sites_reset_confirm {
            sites.push(note("¿Restablecer los permisos por sitio?", "Se conservará el archivo original como una copia en la misma carpeta. La lista quedará vacía y los sitios volverán a seguir los permisos del agente.", Icon::WarningTriangle));
            let buttons = vec![
                settings_ui::outline("browser_site_reset_cancel", "Cancelar").render(),
                settings_ui::outline("browser_site_reset_confirm", "Guardar copia y restablecer")
                    .render(),
            ];
            sites.push(
                if compact {
                    Element::column(buttons).gap(6.0)
                } else {
                    Element::row(buttons).gap(6.0)
                }
                .padding(settings_parts::ROW_PADDING),
            );
        }
    }
    for (index, site) in state.browser_sites.iter().enumerate() {
        let control = Element::row(vec![
            access_selector(
                state,
                Some(index),
                site.access,
                control_width - ControlSize::Compact.height() - 6.0,
            ),
            IconButton::new(
                format!("browser_site_remove:{index}"),
                Icon::Close,
                theme::control_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .enabled(editable)
            .tooltip(format!("Quitar el permiso propio de {}", site.origin))
            .render(),
        ])
        .gap(6.0);
        sites.push(settings_parts::settings_row(&site.origin, match site.access {
            SiteAccess::Allow => "El agente actúa en este sitio sin preguntar.",
            SiteAccess::Ask => "Sigue los permisos del agente. Con «Permitir todo», las acciones no preguntan.",
            SiteAccess::Block => "El agente no puede leer ni actuar en este sitio. El navegador manual sigue disponible.",
        }, control, compact));
    }
    sites.push(settings_parts::settings_row("Agregar permiso para un sitio",
        "Un origen como https://example.com. Elegí el permiso antes de agregarlo; sólo se guarda el origen, sin rutas.",
        Element::column(vec![
            TextField::new("browser_site_draft", &state.browser_site_draft, "https://example.com", theme::control_palette()).size(ControlSize::Compact).width(control_width.min(240.0)).enabled(editable)
                .accessibility_label("Dirección del sitio").render(),
            access_selector(state, None, state.browser_site_draft_access, control_width),
            Button::new("browser_site_add", "Agregar", theme::control_palette()).variant(ButtonVariant::Outline)
                .size(ControlSize::Compact).enabled(editable).render(),
        ]).gap(6.0), compact));
    groups.push(settings_parts::settings_group("Permisos por sitio", sites));
    groups.push(settings_parts::settings_group("Vista previa", vec![settings_parts::settings_row(
        "Abrir la vista previa de los servidores que inicia el agente",
        "Cuando un comando del agente publica una dirección de este equipo, como localhost:5173, y ese puerto responde. Preguntar la ofrece en la tarjeta del comando. Nunca se abre sola una dirección de otro equipo.",
        settings_ui::settings_choice_selector(state, "preview_opening", state.preview_opening.label(), state.preview_opening.options()), compact,
    )]));
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{HitKind, Role, Size, UiTree};

    #[test]
    fn browser_site_choices_are_native_and_fit_narrow_and_wide_pages() {
        let mut state = crate::view::tests::empty_state();
        state.browser_sites = vec![crate::browser_sites::Site {
            origin: "https://example.test".into(),
            access: SiteAccess::Block,
        }];
        for width in [320.0, 360.0, 900.0] {
            let mut tree = UiTree::new(
                Element::column(page(&state, width < 640.0, width - 24.0)).padding(12.0),
            );
            let frame = tree.layout(Size::new(width, 2000.0));
            for id in [
                "settings_choice_toggle:browser_site:0",
                "settings_choice_toggle:browser_site_draft",
            ] {
                let node = frame.access.iter().find(|node| node.id == id).unwrap();
                assert_eq!(node.role, Role::ComboBox);
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap()
                    .rect;
                assert!(
                    rect.x >= 0.0 && rect.x + rect.width <= width + 0.01,
                    "{width}: {rect:?}"
                );
            }
            assert!(frame
                .regions
                .iter()
                .filter(|region| region.kind == HitKind::Button)
                .all(|region| region.rect.x >= 0.0
                    && region.rect.x + region.rect.width <= width + 0.01));
        }
        state.browser_sites_error = Some("invalid policy".into());
        let frame = UiTree::new(Element::column(page(&state, true, 360.0)))
            .layout(Size::new(360.0, 2000.0));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "browser_site_add"));
        assert!(
            frame
                .access
                .iter()
                .find(|node| node.id == "browser_site_add")
                .unwrap()
                .disabled
        );
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "browser_site_reset"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "browser_site_reset_confirm"));
    }

    #[test]
    fn browser_controls_and_recovery_fit_the_actual_settings_column_with_native_fonts() {
        use openide_ui::{DrawCommand, Key, Point, UiAction, UiEvent};
        for width in [320.0, 360.0, 900.0] {
            for recovery in [false, true] {
                let mut state = crate::view::tests::empty_state();
                state.show_settings = true;
                state.settings_page = settings_ui::SettingsPage::Browser;
                state.browser_sites = vec![crate::browser_sites::Site {
                    origin: "https://example.test".into(),
                    access: SiteAccess::Ask,
                }];
                state.browser_site_draft_access = SiteAccess::Ask;
                if recovery {
                    state.browser_sites_error = Some("El archivo de permisos no es válido".into());
                    state.browser_sites_reset_confirm = true;
                }
                let root = settings_ui::build_settings_page(
                    &state,
                    settings_ui::SettingsPage::Browser,
                    width,
                    false,
                );
                let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
                tree.set_reduced_motion(true);
                let viewport = Size::new(width, 900.0);
                let ids: &[&str] = if recovery {
                    &[
                        "browser_site_reload",
                        "browser_site_reset",
                        "browser_site_reset_cancel",
                        "browser_site_reset_confirm",
                    ]
                } else {
                    &[
                        "settings_choice_toggle:browser_site:0",
                        "browser_site_remove:0",
                        "browser_site_draft",
                        "settings_choice_toggle:browser_site_draft",
                        "browser_site_add",
                    ]
                };
                tree.layout(viewport);
                for id in ids {
                    assert!(tree.focus(id), "missing control {id} at {width}");
                    let frame = tree.layout(viewport);
                    let rect = frame
                        .regions
                        .iter()
                        .find(|region| region.id == *id)
                        .unwrap()
                        .rect;
                    assert!(
                        rect.x >= 10.0 && rect.x + rect.width <= width - 10.0 + 0.01,
                        "control escaped the settings gutter: {id} at {width}: {rect:?}",
                    );
                    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                    assert_eq!(frame.hit_test(point), Some(*id));
                    if *id == "settings_choice_toggle:browser_site:0" {
                        assert!(
                            frame.commands.iter().any(|command| matches!(command,
                                DrawCommand::Text { position, text, .. }
                                if text == access_label(SiteAccess::Ask)
                                    && position.x >= rect.x
                                    && position.x < rect.x + rect.width
                                    && position.y >= rect.y
                                    && position.y < rect.y + rect.height
                            )),
                            "selected permission was shortened at {width}"
                        );
                    }
                    if *id != "browser_site_draft" {
                        tree.handle_event(UiEvent::PointerDown(point));
                        assert_eq!(
                            tree.handle_event(UiEvent::PointerUp(point)),
                            vec![UiAction::Clicked { id: (*id).into() }]
                        );
                    }
                }
                context.end_pass().textures_delta.clear();
                if !recovery {
                    state.settings_choice_menu_open = Some("browser_site:0".into());
                    let root = settings_ui::build_settings_page(
                        &state,
                        settings_ui::SettingsPage::Browser,
                        width,
                        false,
                    );
                    let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
                    let frame = tree.layout(viewport);
                    let choice = frame
                        .access
                        .iter()
                        .find(|node| node.id == "settings_choice_toggle:browser_site:0")
                        .unwrap();
                    assert_eq!(choice.expanded, Some(true));
                    assert_eq!(
                        tree.handle_event(UiEvent::Key {
                            key: Key::Escape,
                            shift: false,
                            ctrl: false,
                        }),
                        vec![UiAction::Clicked {
                            id: "settings_choice_dismiss".into()
                        }]
                    );
                    context.end_pass().textures_delta.clear();
                }
            }
        }
    }
}
