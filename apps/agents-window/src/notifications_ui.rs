//! Bell and list for [`crate::notifications::NotificationCenter`].

use std::time::SystemTime;

use openide_ui::{
    ButtonVariant, Color, Component, ControlSize, Element, Icon, IconButton, ListRow,
    Popover, PopoverPlacement, Text, TextRole, TooltipPlacement, TooltipSpec,
};

use crate::notifications::{Notification, NotificationCenter, Severity};
use crate::{theme, ui_sizing};

const VISIBLE: usize = 8;

pub(crate) fn age(at: SystemTime) -> String {
    let minutes = SystemTime::now()
        .duration_since(at)
        .map_or(0, |elapsed| elapsed.as_secs() / 60);
    match minutes {
        0 => "ahora".into(),
        1..=59 => format!("hace {minutes} min"),
        60..=1439 => format!("hace {} h", minutes / 60),
        _ => format!("hace {} d", minutes / 1440),
    }
}

/// The part of a notification that opens it.
fn open_row(item: &Notification) -> Element {
    let ink = match (item.severity, item.read) {
        (Severity::Error, _) => theme::DESTRUCTIVE,
        (_, false) => theme::FOREGROUND,
        (_, true) => theme::FOREGROUND_SUBTLE,
    };
    let mut row = ListRow::new(
        format!("notification_open:{}", item.id),
        &item.title,
        theme::component_palette(),
    )
    .title_ink(ink)
    // An empty detail keeps its line, so every row of the list is as tall.
    .subtitle(if item.detail.is_empty() { " " } else { &item.detail })
    .sizing(ui_sizing::NOTIFICATION);
    if item.count > 1 {
        row = row.meta_element(
            Text::new(format!("×{}", item.count), theme::component_palette())
                .role(TextRole::Small)
                .truncate()
                .render(),
        );
    }
    row.meta_element(
        Text::new(age(item.at), theme::component_palette())
            .role(TextRole::Small)
            .muted()
            .truncate()
            .render(),
    )
    .render()
}

fn row(item: &Notification) -> Element {
    Element::row(vec![
        open_row(item).flex(1.0),
        IconButton::new(
            format!("notification_remove:{}", item.id),
            Icon::Close,
            theme::menu_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(TooltipSpec::new("Descartar").placement(TooltipPlacement::Below))
        .render(),
    ])
    .gap(2.0)
}

fn list(center: &NotificationCenter) -> Element {
    let items = center.items();
    let mut tools = Vec::new();
    if !items.is_empty() {
        tools.push(theme::interactive_surface(Element::clickable(
            "notifications_clear",
            Text::new("Limpiar", theme::component_palette())
                .font_size(11.5)
                .muted()
                .truncate()
                .render()
                .padding(4.0),
        )));
    }
    let mut rows = vec![
        openide_ui::PanelHeader::new("Notificaciones", theme::menu_palette())
            .trailing(tools)
            .sizing(crate::ui_sizing::NOTIFICATIONS_HEADER)
            .render(),
    ];
    if items.is_empty() {
        rows.push(
            Text::new("Sin notificaciones", theme::component_palette())
                .muted()
                .truncate()
                .render()
                .padding(6.0),
        );
    }
    rows.extend(items.iter().take(VISIBLE).map(row));
    if items.len() > VISIBLE {
        rows.push(
            Text::new(format!("y {} más", items.len() - VISIBLE), theme::component_palette())
                .font_size(11.5)
                .muted()
                .truncate()
                .render()
                .padding(6.0),
        );
    }
    Element::column(rows).gap(2.0).padding(6.0)
}

/// A notification's card: what kind it is, its copy, its buttons and how far
/// its work has gone.
fn card(item: &Notification) -> Element {
    let (tone, icon) = match item.severity {
        Severity::Info => (openide_ui::AlertTone::Neutral, Icon::InfoCircle),
        Severity::Attention => (openide_ui::AlertTone::Warning, Icon::WarningTriangle),
        Severity::Error => (openide_ui::AlertTone::Error, Icon::ErrorCircle),
    };
    let mut toast = openide_ui::Toast::new(item.title.clone(), theme::menu_palette())
        .id(format!("notification:{}", item.id))
        .description(item.detail.clone())
        .tone(tone)
        .icon(icon)
        .dismissible(format!("notification_toast_dismiss:{}", item.id));
    // The card opens what it is about; one with buttons leaves that to them.
    if item.target.is_some() || item.actions.is_empty() {
        toast = toast.action(format!("notification_open:{}", item.id));
    }
    for action in &item.actions {
        toast = toast.button(action.id.clone(), action.label.clone());
    }
    if let Some((done, total)) = item.progress.filter(|(_, total)| *total > 0) {
        toast = toast.progress(done as f32 / total as f32);
    }
    toast.render()
}

/// Cards in the bottom right corner of `base`: the result of the last
/// Settings change and the newest notifications, the newest lowest.
pub(crate) fn toast_layer(state: &crate::view::ViewState, base: Element) -> Element {
    let mut cards = Vec::new();
    cards.extend(crate::tour_ui::invitation(state));
    if let Some(message) = &state.settings_notice {
        cards.push(
            openide_ui::Toast::new(message.clone(), theme::menu_palette())
                .tone(if state.settings_notice_error {
                    openide_ui::AlertTone::Error
                } else {
                    openide_ui::AlertTone::Neutral
                })
                .dismissible("settings_notice_dismiss")
                .width(340.0)
                .render(),
        );
    }
    if let Some(copied) = &state.link_copied {
        // A long address is cut: the toast names the link, it is not read from.
        let mut address: String = copied.url.chars().take(56).collect();
        if address.len() < copied.url.len() {
            address.push('…');
        }
        cards.push(
            openide_ui::Toast::new("Enlace copiado", theme::menu_palette())
                .description(address)
                .width(340.0)
                .render(),
        );
    }
    cards.extend(state.notifications.toasts(std::time::Instant::now()).into_iter().map(card));
    openide_ui::Toast::layer(base, cards, 36.0)
}

/// Switches for [`crate::notifications::NotificationSettings`].
pub(crate) fn settings_group(center: &NotificationCenter, compact: bool) -> Element {
    let row = |name: &str, title: &str, description: &str| {
        crate::settings_parts::settings_row(
            title,
            description,
            openide_ui::Switch::new(
                format!("notification_setting:{name}"),
                center.settings.get(name),
                theme::component_palette(),
            )
            .render(),
            compact,
        )
    };
    crate::settings_parts::settings_group(
        "Notificaciones",
        vec![
            row(
                "turn_completed",
                "Turnos completados",
                "Cuando un agente termina fuera de tu vista.",
            ),
            row(
                "turn_failed",
                "Turnos fallidos",
                "Cuando un turno se interrumpe por un error.",
            ),
            row(
                "approvals",
                "Permisos pendientes",
                "Cuando un agente o una CLI externa espera tu aprobación.",
            ),
            row(
                "pull_requests",
                "Pull requests vigilados",
                "Cuando falla el CI o llega un comentario en un PR que vigila una tarea.",
            ),
            row(
                "toasts",
                "Avisos en la ventana",
                "Una tarjeta breve en la esquina inferior derecha.",
            ),
            row(
                "system",
                "Avisos del sistema",
                "Notificación del escritorio cuando openADE no tiene el foco.",
            ),
            row("sound", "Sonido", "Un sonido breve con cada aviso."),
            row(
                "do_not_disturb",
                "No molestar",
                "Los eventos se siguen listando en la campana, sin avisos.",
            ),
        ],
    )
}

/// Struck through while notices are silenced. What is unread is said by
/// the count beside it, not by the glyph.
pub(crate) fn bell_icon(center: &NotificationCenter) -> Icon {
    if center.settings.do_not_disturb {
        Icon::BellSlash
    } else {
        Icon::Bell
    }
}

/// The bell with its unread count. The list opens beneath it, or above it
/// when the bell sits at the bottom of the window.
pub(crate) fn bell(center: &NotificationCenter, surface: Color, size: f32, above: bool) -> Element {
    let unread = center.unread();
    let button = IconButton::new(
        "notifications_toggle",
        bell_icon(center),
        theme::action_palette(
            if center.open { theme::SELECTED } else { surface },
            theme::FOREGROUND,
        ),
    )
    .variant(ButtonVariant::Subtle)
    .size(ControlSize::Dense)
    .tooltip_with(
        TooltipSpec::new(match unread {
            0 => "Notificaciones".to_owned(),
            1 => "1 notificación nueva".to_owned(),
            count => format!("{count} notificaciones nuevas"),
        })
        .placement(if above {
            TooltipPlacement::Above
        } else {
            TooltipPlacement::Below
        }),
    )
    .render()
    .size(Some(size), Some(size));
    let mut trigger = vec![button];
    if unread > 0 {
        trigger.insert(
            0,
            openide_ui::Chip::new(unread.min(99).to_string(), theme::component_palette())
                .height(16.0)
                .render()
                .centered_y(),
        );
    }
    popover(center, Element::row(trigger).gap(2.0), above)
}

/// Opens the list from `trigger`, a control that dispatches `notifications_toggle`.
pub(crate) fn popover(center: &NotificationCenter, trigger: Element, above: bool) -> Element {
    if !center.open {
        return trigger;
    }
    Popover::new(
        trigger,
        list(center),
        true,
        "notifications_dismiss",
        theme::menu_palette(),
    )
    .placement(if above {
        PopoverPlacement::TopEnd
    } else {
        PopoverPlacement::BottomEnd
    })
    .width(320.0)
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::Target;
    use openide_ui::{DrawCommand, Size, UiAction, UiEvent, UiTree};

    fn texts(frame: &openide_ui::Frame) -> String {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[test]
    fn bell_shows_the_unread_count_and_the_open_list_dispatches_each_action() {
        let mut center = NotificationCenter::default();
        let closed = UiTree::new(bell(&center, theme::SIDEBAR, 24.0, false)).layout(Size::new(400.0, 300.0));
        assert!(texts(&closed).is_empty(), "no count without unread entries");
        assert!(closed.regions.iter().any(|region| region.id == "notifications_toggle"));

        center.push(
            Severity::Error,
            "Turno fallido",
            "Arreglar el login",
            Some(Target::Session("s1".into())),
        );
        center.push(Severity::Info, "Turno completado", "Otra tarea", None);
        center.push(Severity::Info, "Turno completado", "Otra tarea", None);
        let counted = UiTree::new(bell(&center, theme::SIDEBAR, 24.0, false)).layout(Size::new(400.0, 300.0));
        assert_eq!(texts(&counted), "2");

        center.toggle();
        let mut tree = UiTree::new(bell(&center, theme::SIDEBAR, 24.0, false));
        let open = tree.layout(Size::new(400.0, 300.0));
        let shown = texts(&open);
        for expected in [
            "Notificaciones",
            "Limpiar",
            "Turno completado",
            "×2",
            "Otra tarea",
            "Turno fallido",
            "Arreglar el login",
            "ahora",
        ] {
            assert!(shown.contains(expected), "{expected}: {shown}");
        }
        let failed = center.items()[1].id;
        for id in [
            format!("notification_open:{failed}"),
            format!("notification_remove:{failed}"),
            "notifications_clear".to_owned(),
        ] {
            let rect = open
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
                .rect;
            let point = openide_ui::Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                [UiAction::Clicked { id }]
            );
        }

        center.clear();
        let empty = UiTree::new(bell(&center, theme::SIDEBAR, 24.0, false)).layout(Size::new(400.0, 300.0));
        let shown = texts(&empty);
        assert!(shown.contains("Sin notificaciones") && !shown.contains("Limpiar"), "{shown}");
    }

    #[test]
    fn cards_stack_newest_lowest_with_their_buttons_and_progress() {
        use crate::notifications::Notice;
        let mut state = crate::view::tests::empty_state();
        state.notifications.push(Severity::Error, "Turno fallido", "Migrar la base", Some(Target::Session("s".into())));
        state.notifications.post(
            Notice::new(Severity::Info, "Descargando openADE 0.4.0", "50 % de 180 MB")
                .key("app_update")
                .sticky()
                .progress(90, 180),
        );
        let base = Element::spacer().size(Some(900.0), Some(600.0));
        let mut tree = UiTree::new(toast_layer(&state, base));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(900.0, 600.0));
        let ink = |label: &str| {
            frame.commands.iter().find_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text == label => Some(position.y),
                _ => None,
            })
        };
        assert!(ink("Turno fallido").unwrap() < ink("Descargando openADE 0.4.0").unwrap(), "{}", texts(&frame));
        assert!(frame.commands.iter().any(|command| matches!(command, DrawCommand::Icon { icon: Icon::ErrorCircle, .. })));
        assert!(frame.commands.iter().any(|command| matches!(command, DrawCommand::Icon { icon: Icon::InfoCircle, .. })));

        state.notifications.post(
            Notice::new(Severity::Info, "openADE 0.4.0 está lista", "Se instala al reiniciar.")
                .key("app_update")
                .sticky()
                .action("update_restart", "Reiniciar ahora")
                .action("update_notes", "Novedades"),
        );
        let ready = state.notifications.by_key("app_update").unwrap().id;
        let base = Element::spacer().size(Some(900.0), Some(600.0));
        let frame = UiTree::new(toast_layer(&state, base)).layout(Size::new(900.0, 600.0));
        let shown = texts(&frame);
        assert!(shown.contains("Reiniciar ahora") && shown.contains("Novedades"), "{shown}");
        let ids: Vec<_> = frame.regions.iter().map(|region| region.id.as_str()).collect();
        assert!(ids.contains(&"update_restart") && ids.contains(&"update_notes"), "{ids:?}");
        assert!(ids.contains(&format!("notification_toast_dismiss:{ready}").as_str()));
        assert!(!ids.contains(&format!("notification_open:{ready}").as_str()), "its buttons say what it does");
    }

    #[test]
    fn a_copied_link_is_announced_with_its_address_cut_short() {
        let mut state = crate::view::tests::empty_state();
        state.link_copied = Some(crate::view::LinkCopied {
            url: format!("https://example.com/{}", "a".repeat(80)),
            by: "",
            at: std::time::Instant::now(),
        });
        let base = Element::spacer().size(Some(800.0), Some(500.0));
        let frame = UiTree::new(toast_layer(&state, base)).layout(Size::new(800.0, 500.0));
        let shown = texts(&frame);
        assert!(shown.contains("Enlace copiado") && shown.contains("https://example.com/"), "{shown}");
        assert!(shown.contains('…') && !shown.contains(&"a".repeat(80)), "{shown}");
    }

    #[test]
    fn the_card_sits_in_the_bottom_right_corner_and_the_settings_list_every_switch() {
        let mut center = NotificationCenter::default();
        let base = || Element::clickable("base", Element::spacer()).size(Some(800.0), Some(500.0));
        let size = Size::new(800.0, 500.0);
        let mut state = crate::view::tests::empty_state();
        let quiet = UiTree::new(toast_layer(&state, base())).layout(size);
        assert!(texts(&quiet).is_empty());

        center.push(Severity::Error, "Turno fallido", "Migrar la base", Some(Target::Session("s".into())));
        let id = center.items()[0].id;
        state.notifications = center.clone();
        let mut tree = UiTree::new(toast_layer(&state, base()));
        let frame = tree.layout(size);
        let shown = texts(&frame);
        assert!(shown.contains("Turno fallido") && shown.contains("Migrar la base"), "{shown}");
        let open = frame
            .regions
            .iter()
            .find(|region| region.id == format!("notification_open:{id}"))
            .unwrap()
            .rect;
        assert!(open.x > 400.0 && open.y > 380.0, "{open:?}");
        assert!(open.x + open.width <= 800.0 - 12.0 && open.y + open.height <= 500.0 - 36.0);
        assert_eq!(
            frame.hit_test(openide_ui::Point::new(100.0, 100.0)),
            Some("base"),
            "the rest of the window stays usable"
        );
        let close = frame
            .regions
            .iter()
            .find(|region| region.id == format!("notification_toast_dismiss:{id}"))
            .unwrap()
            .rect;
        let point = openide_ui::Point::new(close.x + close.width / 2.0, close.y + close.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            [UiAction::Clicked { id: format!("notification_toast_dismiss:{id}") }]
        );

        let settings = UiTree::new(settings_group(&center, false)).layout(Size::new(700.0, 500.0));
        for name in crate::notifications::NotificationSettings::NAMES {
            assert!(
                settings
                    .regions
                    .iter()
                    .any(|region| region.id == format!("notification_setting:{name}")),
                "{name}"
            );
        }
        assert!(texts(&settings).contains("No molestar"));
    }
}
