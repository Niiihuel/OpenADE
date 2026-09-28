//! Reusable, native Settings presentation primitives.
//!
//! These functions describe UI only. Callers own values, loading state and
//! action IDs; an optional action is rendered only when it is supplied.

use openide_ui::{Color, Element, FontFamily, Icon};

use crate::theme;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityStatus {
    Ready,
    Partial,
    Planned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedbackTone {
    Success,
    Error,
}

fn label(value: impl Into<String>, color: Color, size: f32) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(size)
        .foreground(color)
}

fn hairline() -> Element {
    Element::spacer()
        .size(None, Some(1.0))
        .background(theme::BORDER)
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
    Element::panel(
        Element::row(vec![
            Element::icon(icon)
                .foreground(color)
                .size(Some(14.0), Some(14.0)),
            label(text, color, 11.0),
        ])
        .gap(6.0),
    )
    .padding(7.0)
    .background(surface)
    .border(theme::BORDER, 1.0)
    .radius(8.0)
}

/// In-page save, operation or validation feedback. `action` may be a retry or
/// dismissal control supplied by the caller; this component creates none.
pub fn feedback_banner(
    message: impl Into<String>,
    tone: FeedbackTone,
    action: Option<Element>,
    compact: bool,
) -> Element {
    let (icon, ink, surface) = match tone {
        FeedbackTone::Success => (Icon::Check, theme::FOREGROUND, theme::SELECTED),
        FeedbackTone::Error => (Icon::Stop, theme::DESTRUCTIVE, theme::DIFF_REMOVED_BG),
    };
    let copy = Element::row(vec![
        Element::icon(icon)
            .foreground(ink)
            .size(Some(16.0), Some(16.0)),
        label(message, ink, 13.0).flex(1.0),
    ])
    .gap(10.0);
    let content = match (action, compact) {
        (Some(action), true) => Element::column(vec![copy, action]).gap(12.0),
        (Some(action), false) => Element::row(vec![copy.flex(1.0), action]).gap(16.0),
        (None, _) => copy,
    };
    Element::panel(content)
        .padding(14.0)
        .background(surface)
        .border(theme::BORDER, 1.0)
        .radius(10.0)
}

/// Main heading inside a Settings route or nested resource view.
pub fn section_header(
    title: impl Into<String>,
    description: impl Into<String>,
    action: Option<Element>,
    compact: bool,
) -> Element {
    let copy = Element::column(vec![
        label(title, theme::FOREGROUND, if compact { 23.0 } else { 28.0 }),
        label(description, theme::FOREGROUND_SUBTLE, 13.0),
    ])
    .gap(8.0);
    match (action, compact) {
        (Some(action), true) => Element::column(vec![copy, action]).gap(14.0),
        (Some(action), false) => Element::row(vec![copy.flex(1.0), action]).gap(20.0),
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
        label(title, theme::FOREGROUND, 14.0),
        label(description, theme::FOREGROUND_SUBTLE, 12.0),
    ])
    .gap(6.0);
    let content = if compact {
        Element::column(vec![copy, control]).gap(12.0)
    } else {
        Element::row(vec![copy.flex(1.0), control])
            .gap(24.0)
            .min_size(0.0, 54.0)
    };
    content.padding(16.0)
}

/// A card with hairline separators between supplied rows.
pub fn settings_card(rows: Vec<Element>) -> Element {
    let mut children = Vec::with_capacity(rows.len().saturating_mul(2).saturating_sub(1));
    for row in rows {
        if !children.is_empty() {
            children.push(hairline());
        }
        children.push(row);
    }
    Element::panel(Element::column(children))
        .background(theme::CARD)
        .border(theme::BORDER, 1.0)
        .radius(12.0)
        .clip_children()
}

/// Labeled group of setting rows using the same card treatment on every page.
pub fn settings_group(title: impl Into<String>, rows: Vec<Element>) -> Element {
    Element::column(vec![
        label(title, theme::FOREGROUND_SUBTLE, 12.0).padding(8.0),
        settings_card(rows),
    ])
    .gap(4.0)
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
            .size(Some(20.0), Some(20.0)),
        label(title, theme::FOREGROUND, 15.0),
        label(description, theme::FOREGROUND_SUBTLE, 12.0),
    ];
    if let Some(action) = action {
        children.push(Element::spacer().size(None, Some(2.0)));
        children.push(action);
    }
    Element::panel(Element::column(children).gap(10.0))
        .padding(22.0)
        .min_size(0.0, 118.0)
        .background(theme::CARD)
        .border(theme::BORDER, 1.0)
        .radius(12.0)
}

/// No items exist yet. The caller can supply a real create/import action.
pub fn empty_state(
    title: impl Into<String>,
    description: impl Into<String>,
    action: Option<Element>,
) -> Element {
    state_panel(
        Icon::Grid,
        title,
        description,
        theme::FOREGROUND_SUBTLE,
        action,
    )
}

/// A named in-progress operation. No fake retry or cancel action is shown.
pub fn loading_state(title: impl Into<String>, description: impl Into<String>) -> Element {
    state_panel(
        Icon::Refresh,
        title,
        description,
        theme::FOREGROUND_SUBTLE,
        None,
    )
}

/// An operation that failed. A retry control appears only when supplied.
pub fn error_state(
    title: impl Into<String>,
    description: impl Into<String>,
    retry: Option<Element>,
) -> Element {
    state_panel(Icon::Stop, title, description, theme::DESTRUCTIVE, retry)
}

#[cfg(test)]
mod tests {
    use openide_ui::{ElementKind, Icon};

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
}
