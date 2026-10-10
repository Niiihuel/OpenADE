//! Reusable, native Settings presentation primitives.
//!
//! These functions describe UI only. Callers own values, loading state and
//! action IDs; an optional action is rendered only when it is supplied.

use openide_ui::{
    Alert, Color, Component, ComponentPalette, Element, FontFamily, Icon, PreferencesGroup, Skeleton,
};

pub use openide_ui::AlertTone as FeedbackTone;

use crate::theme;

// Logical pixels shared by Settings pages and their nested resource views.
pub const TITLE_SIZE: f32 = 18.0;
pub const LABEL_SIZE: f32 = 12.0;
pub const DETAIL_SIZE: f32 = 11.0;
pub const ROW_PADDING: f32 = 10.0;
pub const CARD_RADIUS: f32 = 8.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityStatus {
    Ready,
    Partial,
    Planned,
}

fn label(value: impl Into<String>, color: Color, size: f32) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(size)
        .foreground(color)
}

/// Small capability marker for features that are ready, limited or planned.
/// It is informational and never implies an action is available.
pub fn status_badge(status: CapabilityStatus) -> Element {
    let (icon, text, color, surface) = match status {
        CapabilityStatus::Ready => (
            Icon::Check,
            "Disponible",
            theme::FOREGROUND,
            theme::SELECTED,
        ),
        CapabilityStatus::Partial => (Icon::Clock, "Parcial", theme::FOREGROUND, theme::SELECTED),
        CapabilityStatus::Planned => (
            Icon::Clock,
            "Planificado",
            theme::FOREGROUND_SUBTLE,
            theme::PANEL,
        ),
    };
    let content = Element::row(vec![
        Element::column(vec![
            Element::spacer().flex(1.0),
            Element::icon(icon)
                .foreground(color)
                .size(Some(14.0), Some(14.0)),
            Element::spacer().flex(1.0),
        ])
        .size(Some(14.0), Some(16.0)),
        Element::column(vec![
            Element::spacer().flex(1.0),
            label(text, color, DETAIL_SIZE),
            Element::spacer().flex(1.0),
        ])
        .size(None, Some(16.0)),
    ])
    .gap(4.0)
    .size(None, Some(16.0));
    let badge = Element::panel(Element::row(vec![
        Element::spacer().size(Some(3.0), None),
        content,
        Element::spacer().size(Some(3.0), None),
    ]))
    .padding(3.0)
    .background(surface)
    .border(theme::BORDER, 1.0)
    .radius(5.0)
    .size(None, Some(22.0));
    // Keep the pill intrinsic when a compact header stacks it below its copy.
    Element::row(vec![badge, Element::spacer().flex(1.0)]).size(None, Some(22.0))
}

/// In-page save, operation or validation feedback. `action` may be a retry or
/// dismissal control supplied by the caller; this component creates none.
pub fn feedback_banner(
    message: impl Into<String>,
    tone: FeedbackTone,
    action: Option<Element>,
    compact: bool,
) -> Element {
    let title = match tone {
        FeedbackTone::Neutral => "Aviso",
        FeedbackTone::Info => "Información",
        FeedbackTone::Success => "Completado",
        FeedbackTone::Warning => "Atención",
        FeedbackTone::Error => "Error",
    };
    let mut alert = Alert::new(title, message, theme::component_palette())
        .tone(tone)
        .compact(compact);
    if let Some(action) = action {
        alert = alert.action(action);
    }
    alert.render()
}

/// Main heading inside a Settings route or nested resource view.
pub fn section_header(
    title: impl Into<String>,
    description: impl Into<String>,
    action: Option<Element>,
    compact: bool,
) -> Element {
    let copy = Element::column(vec![
        label(title, theme::FOREGROUND, TITLE_SIZE),
        label(description, theme::SETTINGS_FOREGROUND_SUBTLE, DETAIL_SIZE),
    ])
    .gap(4.0);
    match (action, compact) {
        (Some(action), true) => Element::column(vec![copy, action]).gap(8.0),
        (Some(action), false) => {
            let width = action.style.width;
            let action = Element::column(vec![
                Element::spacer().flex(1.0),
                action,
                Element::spacer().flex(1.0),
            ])
            .size(width, None);
            Element::row(vec![copy.flex(1.0), action]).gap(12.0)
        }
        (None, _) => copy,
    }
}

/// One setting's label, supporting text and caller-owned control.
pub fn settings_row(
    title: impl Into<String>,
    description: impl Into<String>,
    control: Element,
    compact: bool,
) -> Element {
    let copy = Element::column(vec![
        label(title, theme::FOREGROUND, LABEL_SIZE),
        label(description, theme::SETTINGS_FOREGROUND_SUBTLE, DETAIL_SIZE),
    ])
    .gap(4.0);
    let stack_control = compact && !control.style.width.is_some_and(|width| width <= 64.0);
    let content = if stack_control {
        Element::column(vec![copy, control]).gap(8.0)
    } else {
        let mut control = control;
        let width = control.style.width;
        let flex = control.style.flex;
        control.style.flex = 0.0;
        // Fixed-height controls need their own vertical centering; the row
        // stretches this wrapper to match wrapped descriptions and tall copy.
        let centered_control = Element::column(vec![
            Element::spacer().flex(1.0),
            control,
            Element::spacer().flex(1.0),
        ])
        .size(width, None)
        .flex(flex);
        Element::row(vec![copy.flex(1.0), centered_control])
            .gap(16.0)
            .min_size(0.0, 34.0)
    };
    content.padding(ROW_PADDING)
}

/// The colors of a Settings card: the card surface, a border that barely
/// separates it from the page, and the subtle label ink.
fn card_palette() -> ComponentPalette {
    ComponentPalette {
        surface: theme::CARD,
        raised: theme::CARD,
        border: theme::BORDER,
        text: theme::FOREGROUND,
        muted: theme::SETTINGS_FOREGROUND_SUBTLE,
        ..theme::component_palette()
    }
}

/// A card with hairline separators between supplied rows.
pub fn settings_card(rows: Vec<Element>) -> Element {
    settings_group("", rows)
}

/// Labeled group of setting rows using the same card treatment on every page.
/// This is limn's preference group in the Settings colors, so every page,
/// whatever builds its rows, shares one card.
pub fn settings_group(title: impl Into<String>, rows: Vec<Element>) -> Element {
    PreferencesGroup::new(title, rows, card_palette()).render()
}

fn state_panel(
    icon: Icon,
    title: impl Into<String>,
    description: impl Into<String>,
    accent: Color,
    action: Option<Element>,
) -> Element {
    let mut children = vec![
        Element::icon(icon)
            .foreground(accent)
            .size(Some(18.0), Some(18.0)),
        label(title, theme::FOREGROUND, LABEL_SIZE),
        label(description, theme::SETTINGS_FOREGROUND_SUBTLE, DETAIL_SIZE),
    ];
    if let Some(action) = action {
        children.push(Element::spacer().size(None, Some(2.0)));
        children.push(action);
    }
    Element::panel(Element::column(children).gap(8.0))
        .padding(12.0)
        .min_size(0.0, 88.0)
        .background(theme::CARD)
        .border(theme::BORDER, 1.0)
        .radius(CARD_RADIUS)
}

/// No items exist yet. The caller can supply a real create/import action.
pub fn empty_state(
    title: impl Into<String>,
    description: impl Into<String>,
    action: Option<Element>,
) -> Element {
    empty_state_with_icon(Icon::Grid, title, description, action)
}

/// Empty state with the icon of its own settings section.
pub fn empty_state_with_icon(
    icon: Icon,
    title: impl Into<String>,
    description: impl Into<String>,
    action: Option<Element>,
) -> Element {
    // The shared blank slate, on the same card as every other settings group.
    let mut palette = theme::component_palette();
    palette.muted = theme::SETTINGS_FOREGROUND_SUBTLE;
    let mut empty = openide_ui::Empty::new(title, description, palette)
        .icon(icon)
        .compact();
    if let Some(action) = action {
        empty = empty.action_element(action);
    }
    Element::panel(openide_ui::Component::render(empty))
        .background(theme::CARD)
        .border(theme::BORDER, 1.0)
        .radius(CARD_RADIUS)
}

/// A named in-progress operation. No fake retry or cancel action is shown.
pub fn loading_state(title: impl Into<String>, description: impl Into<String>) -> Element {
    let title = title.into();
    let palette = theme::component_palette();
    let placeholder_id = format!("settings-loading:{title}");
    state_panel(
        Icon::Refresh,
        title,
        description,
        theme::FOREGROUND_SUBTLE,
        Some(
            Element::row(vec![
                Skeleton::circle(format!("{placeholder_id}:icon"), palette, 24.0).render(),
                Element::column(vec![
                    Skeleton::line(format!("{placeholder_id}:name"), palette, 188.0).render(),
                    Skeleton::line(format!("{placeholder_id}:detail"), palette, 120.0)
                        .height(10.0)
                        .render(),
                ])
                .gap(7.0),
            ])
            .gap(12.0),
        ),
    )
}

/// An operation that failed. A retry control appears only when supplied.
pub fn error_state(
    title: impl Into<String>,
    description: impl Into<String>,
    retry: Option<Element>,
) -> Element {
    let mut alert = Alert::new(title, description, theme::component_palette())
        .tone(FeedbackTone::Error)
        .compact(true);
    if let Some(retry) = retry {
        alert = alert.action(retry);
    }
    alert.render()
}

#[cfg(test)]
mod tests {
    use openide_ui::{
        DrawCommand, ElementKind, Icon, Point, Size, Switch, UiAction, UiEvent, UiTree,
    };

    use super::*;

    fn action_ids(element: &Element, found: &mut Vec<String>) {
        match &element.kind {
            ElementKind::Button { id, .. }
            | ElementKind::IconButton { id, .. }
            | ElementKind::Clickable { id, .. } => found.push(id.clone()),
            _ => {}
        }
        match &element.kind {
            ElementKind::Column(children) | ElementKind::Row(children) => {
                for child in children {
                    action_ids(child, found);
                }
            }
            ElementKind::Panel(child) | ElementKind::Clickable { child, .. } => {
                action_ids(child, found);
            }
            _ => {}
        }
    }

    #[test]
    fn optional_actions_appear_only_when_the_caller_supplies_them() {
        let empty = empty_state("Vacío", "Agrega un elemento.", None);
        let error = error_state(
            "Error",
            "Vuelve a intentar.",
            Some(Element::button("retry_load", "Reintentar")),
        );
        let mut empty_ids = Vec::new();
        let mut error_ids = Vec::new();
        action_ids(&empty, &mut empty_ids);
        action_ids(&error, &mut error_ids);
        assert!(empty_ids.is_empty());
        assert_eq!(error_ids, vec!["retry_load".to_owned()]);
    }

    #[test]
    fn compact_row_puts_control_below_copy() {
        let compact = settings_row(
            "Tema",
            "Tema activo",
            Element::button("theme", "Oscuro"),
            true,
        );
        let wide = settings_row(
            "Tema",
            "Tema activo",
            Element::button("theme", "Oscuro"),
            false,
        );
        assert!(matches!(&compact.kind, ElementKind::Column(_)));
        assert!(matches!(&wide.kind, ElementKind::Row(_)));
        let mut ids = Vec::new();
        action_ids(&compact, &mut ids);
        assert_eq!(ids, vec!["theme".to_owned()]);
    }

    #[test]
    fn switches_center_in_settings_rows_even_when_descriptions_wrap() {
        let row_color = Color::rgb(1, 2, 3);
        for (width, compact) in [(640.0, false), (260.0, true)] {
            let row = settings_row(
                "Activar ambos permisos",
                "Personalizado: modificar archivos está activo; ejecutar comandos está desactivado. Puedes ajustar los permisos por separado.",
                Switch::new("write_mode", false, theme::component_palette()).render(),
                compact,
            )
            .background(row_color);
            let mut tree = UiTree::new(Element::column(vec![row]));
            let frame = tree.layout(Size::new(width, 400.0));
            let row = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::FillRect { rect, color, .. } if *color == row_color => Some(*rect),
                    _ => None,
                })
                .unwrap();
            let control = frame
                .regions
                .iter()
                .find(|region| region.id == "write_mode")
                .unwrap()
                .rect;
            let center = Point::new(
                control.x + control.width / 2.0,
                control.y + control.height / 2.0,
            );
            assert!((center.y - (row.y + row.height / 2.0)).abs() < 0.01);
            assert_eq!((control.width, control.height), (36.0, 20.0));
            assert!(control.x + control.width <= width - ROW_PADDING);
            tree.handle_event(UiEvent::PointerDown(center));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(center)),
                vec![UiAction::Clicked {
                    id: "write_mode".into()
                }]
            );
        }
    }

    #[test]
    fn all_status_badges_are_informational() {
        for status in [
            CapabilityStatus::Ready,
            CapabilityStatus::Partial,
            CapabilityStatus::Planned,
        ] {
            let badge = status_badge(status);
            let mut ids = Vec::new();
            action_ids(&badge, &mut ids);
            assert!(ids.is_empty());
        }
        let banner = feedback_banner(
            "Guardado",
            FeedbackTone::Success,
            Some(Element::icon_button(
                "dismiss_notice",
                Icon::Close,
                "Cerrar",
            )),
            false,
        );
        let mut ids = Vec::new();
        action_ids(&banner, &mut ids);
        assert_eq!(ids, vec!["dismiss_notice".to_owned()]);
    }

    #[test]
    fn status_badges_keep_intrinsic_size_and_center_in_wrapped_headers() {
        let header_color = Color::rgb(1, 2, 3);
        for (width, compact) in [(500.0, false), (300.0, false), (240.0, true)] {
            for status in [
                CapabilityStatus::Ready,
                CapabilityStatus::Partial,
                CapabilityStatus::Planned,
            ] {
                let header = section_header(
                    "Complementos del agente",
                    "Capacidades instaladas y extensiones ejecutables, con su configuración y disponibilidad por proyecto.",
                    Some(status_badge(status)),
                    compact,
                ).background(header_color);
                let mut tree = UiTree::new(Element::column(vec![header]));
                tree.set_sans_metrics(openide_ui::FontMetrics {
                    char_width: 7.0,
                    line_height: openide_ui::DEFAULT_FONT_SIZE * 1.3,
                });
                let frame = tree.layout(Size::new(width, 400.0));
                let header = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::FillRect { rect, color, .. } if *color == header_color => {
                            Some(*rect)
                        }
                        _ => None,
                    })
                    .unwrap();
                let badge = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::StrokeRect { rect, radius, .. } if *radius == 5.0 => {
                            Some(*rect)
                        }
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(badge.height, 22.0);
                assert!(badge.width < width / 2.0, "badge stretched: {badge:?}");
                assert!(badge.x >= 0.0 && badge.x + badge.width <= width + 0.01);
                assert!(
                    badge.y >= header.y
                        && badge.y + badge.height <= header.y + header.height + 0.01
                );
                if !compact {
                    assert!(
                        (badge.y + badge.height / 2.0 - header.y - header.height / 2.0).abs()
                            < 0.01
                    );
                }
                let icon = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::Icon { rect, .. } => Some(*rect),
                        _ => None,
                    })
                    .unwrap();
                assert!((icon.y + icon.height / 2.0 - badge.y - badge.height / 2.0).abs() < 0.01);
                let (text_y, text_size) = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::Text {
                            position,
                            text,
                            font_size,
                            ..
                        } if matches!(text.as_str(), "Disponible" | "Parcial" | "Planificado") => {
                            Some((position.y, *font_size))
                        }
                        _ => None,
                    })
                    .unwrap();
                assert!(
                    (text_y + text_size * 1.3 / 2.0 - badge.y - badge.height / 2.0).abs() < 0.01
                );
                assert!(
                    frame.regions.is_empty(),
                    "informational header must not create actions"
                );
            }
        }
    }

    #[test]
    fn loading_state_contains_passive_skeletons() {
        fn count_skeletons(element: &Element) -> usize {
            match &element.kind {
                ElementKind::Skeleton { .. } => 1,
                ElementKind::Column(children) | ElementKind::Row(children) => {
                    children.iter().map(count_skeletons).sum()
                }
                ElementKind::Panel(child) => count_skeletons(child),
                _ => 0,
            }
        }
        let loading = loading_state("Cargando", "Espera un momento");
        assert_eq!(count_skeletons(&loading), 3);
        let mut ids = Vec::new();
        action_ids(&loading, &mut ids);
        assert!(ids.is_empty());
    }
}
