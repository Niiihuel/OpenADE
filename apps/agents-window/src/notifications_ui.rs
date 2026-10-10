//! Bell and list for [`crate::notifications::NotificationCenter`].

use std::time::SystemTime;

use openide_ui::{
    ButtonVariant, Color, Component, ControlSize, Element, FontFamily, Icon, IconButton, ListRow,
    Popover, PopoverPlacement, TooltipPlacement, TooltipSpec,
};

use crate::notifications::{Notification, NotificationCenter, Severity};
use crate::{theme, ui_sizing};

const VISIBLE: usize = 8;

fn text(value: impl Into<String>, size: f32, color: Color) -> Element {
    Element::truncated_text(value)
        .font_family(FontFamily::Sans)
        .font_size(size)
        .foreground(color)
}

fn age(at: SystemTime) -> String {
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

#[cfg(test)]
fn legacy_open_row(item: &Notification) -> Element {
    let ink = match (item.severity, item.read) {
        (Severity::Error, _) => theme::DESTRUCTIVE,
        (_, false) => theme::FOREGROUND,
        (_, true) => theme::FOREGROUND_SUBTLE,
    };
    let mut heading = vec![text(item.title.clone(), 12.0, ink).flex(1.0)];
    if item.count > 1 {
        heading.push(text(format!("×{}", item.count), 11.0, theme::FOREGROUND));
    }
    heading.push(text(age(item.at), 11.0, theme::FOREGROUND_SUBTLE));
    let body = Element::column(vec![
        Element::row(heading).gap(8.0),
        text(item.detail.clone(), 11.5, theme::FOREGROUND_SUBTLE),
    ])
    .gap(2.0)
    .padding(6.0);
    theme::interactive_surface(Element::clickable(
        format!("notification_open:{}", item.id),
        body,
    ))
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
        row = row.meta_element(text(format!("×{}", item.count), 11.0, theme::FOREGROUND));
    }
    row.meta_element(text(age(item.at), 11.0, theme::FOREGROUND_SUBTLE))
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
    let mut header = vec![text("Notificaciones", 12.0, theme::FOREGROUND).flex(1.0)];
    if !items.is_empty() {
        header.push(theme::interactive_surface(Element::clickable(
            "notifications_clear",
            text("Limpiar", 11.5, theme::FOREGROUND_SUBTLE).padding(4.0),
        )));
    }
    let mut rows = vec![Element::row(header).gap(6.0).size(None, Some(24.0))];
    if items.is_empty() {
        rows.push(text("Sin notificaciones", 12.0, theme::FOREGROUND_SUBTLE).padding(6.0));
    }
    rows.extend(items.iter().take(VISIBLE).map(row));
    if items.len() > VISIBLE {
        rows.push(
            text(
                format!("y {} más", items.len() - VISIBLE),
                11.5,
                theme::FOREGROUND_SUBTLE,
            )
            .padding(6.0),
        );
    }
    Element::column(rows).gap(2.0).padding(6.0)
}

/// Brief cards in the bottom right corner of `base`: the result of the last
/// Settings change and the newest notification.
pub(crate) fn toast_layer(state: &crate::view::ViewState, base: Element) -> Element {
    let mut cards = Vec::new();
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
    if let Some(item) = state.notifications.toast(std::time::Instant::now()) {
        cards.push(
            openide_ui::Toast::new(item.title.clone(), theme::menu_palette())
                .description(item.detail.clone())
                .tone(match item.severity {
                    Severity::Error => openide_ui::AlertTone::Error,
                    Severity::Info | Severity::Attention => openide_ui::AlertTone::Neutral,
                })
                .action(format!("notification_open:{}", item.id))
                .dismissible("notification_toast_dismiss")
                .render(),
        );
    }
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
                "toasts",
                "Avisos en la ventana",
                "Una tarjeta breve en la esquina inferior derecha.",
            ),
            row(
                "system",
                "Avisos del sistema",
                "Notificación del escritorio cuando OpenIDE no tiene el foco.",
            ),
            row(
                "do_not_disturb",
                "No molestar",
                "Los eventos se siguen listando en la campana, sin avisos.",
            ),
        ],
    )
}

/// Struck through while notices are silenced, solid while something is unread.
pub(crate) fn bell_icon(center: &NotificationCenter) -> Icon {
    if center.settings.do_not_disturb {
        Icon::BellSlash
    } else if center.unread() > 0 {
        Icon::BellFill
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
            text(unread.min(99).to_string(), 11.0, theme::FOREGROUND).size(None, Some(size)),
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
    fn the_open_row_matches_its_legacy_body() {
        for (severity, read, count) in [
            (Severity::Error, false, 1),
            (Severity::Info, false, 3),
            (Severity::Attention, true, 1),
        ] {
            let item = Notification {
                id: 7,
                severity,
                title: "Permiso pendiente: run_command en una tarea de nombre largo".into(),
                detail: "Arreglar el inicio de sesión del panel de administración".into(),
                target: None,
                count,
                read,
                at: SystemTime::now(),
            };
            crate::state_panel_parity_tests::assert_same(
                "notification",
                legacy_open_row(&item),
                open_row(&item),
            );
        }
    }

    #[test]
    fn an_empty_detail_keeps_its_line() {
        let item = Notification {
            id: 7,
            severity: Severity::Info,
            title: "Turno completado".into(),
            detail: String::new(),
            target: None,
            count: 1,
            read: false,
            at: SystemTime::now(),
        };
        for width in [240.0, 900.0] {
            let frame = |row: Element| UiTree::new(row).layout(Size::new(width, 200.0));
            let (legacy, current) = (frame(legacy_open_row(&item)), frame(open_row(&item)));
            assert_eq!(current.regions, legacy.regions);
            // The blank line is a space where it was an empty string: neither paints.
            let painted = |frame: &openide_ui::Frame| {
                frame
                    .commands
                    .iter()
                    .filter(|command| {
                        !matches!(command, DrawCommand::Text { text, .. } if text.trim().is_empty())
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(painted(&current), painted(&legacy));
        }
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
            .find(|region| region.id == "notification_toast_dismiss")
            .unwrap()
            .rect;
        let point = openide_ui::Point::new(close.x + close.width / 2.0, close.y + close.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            [UiAction::Clicked { id: "notification_toast_dismiss".into() }]
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
