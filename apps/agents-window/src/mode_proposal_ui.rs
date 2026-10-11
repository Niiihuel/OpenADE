//! The card for a mode change the agent proposed with `switch_mode`. The
//! agent can only propose entering or leaving Plan mode; the user decides,
//! and an accepted mode is set through the composer's own mode selection
//! once the running turn ends.

use openide_core::TurnMode;
use openide_ui::{
    Button, ButtonGroup, ButtonVariant, Card, Component, ControlSize, Element, Icon, Text,
};
use serde_json::Value;

use crate::{theme, tool_card_skeleton, view::PermissionView};

pub(crate) const TOOL: &str = openide_harness::SWITCH_MODE_TOOL;
const BUTTON_HEIGHT: f32 = 28.0;

/// The proposed mode, or `None` for anything that is not exactly one of the
/// two modes: nothing else can be asked for through this card.
pub(crate) fn proposed(arguments: &Value) -> Option<TurnMode> {
    match arguments.get("mode")?.as_str()? {
        "plan" => Some(TurnMode::Plan),
        "build" => Some(TurnMode::Build),
        _ => None,
    }
}

pub(crate) fn action(request_id: &str, verb: &str) -> String {
    format!("mode_proposal_{verb}:{request_id}")
}

fn title(mode: TurnMode) -> &'static str {
    match mode {
        TurnMode::Plan => "El agente propone pasar a Modo Plan",
        TurnMode::Build => "El agente propone salir de Modo Plan",
    }
}

fn frame(title: Element, reason: Element, buttons: Element) -> Element {
    Card::new(
        Element::column(vec![
            Element::row(vec![
                Element::panel(
                    Element::icon(Icon::Plan)
                        .foreground(theme::FOREGROUND_SUBTLE)
                        .size(Some(14.0), Some(14.0)),
                )
                .size(Some(14.0), Some(18.0))
                .center_content(),
                title.flex(1.0),
            ])
            .min_size(0.0, 18.0)
            .gap(8.0),
            reason,
            buttons,
        ])
        .gap(8.0),
        theme::card_palette(),
    )
    .render()
}

/// The pending proposal with its two decisions.
pub(crate) fn card(permission: &PermissionView) -> Element {
    let arguments = serde_json::from_str(&permission.arguments).unwrap_or(Value::Null);
    let Some(mode) = proposed(&arguments) else {
        return Element::spacer();
    };
    let request = permission.request_id.as_str();
    let reason = arguments
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    frame(
        Text::new(title(mode), theme::component_palette())
            .render()
            .centered_y(),
        Element::column(vec![
            Text::new(reason, theme::component_palette())
                .muted()
                .render(),
            Text::new(
                "El modo cambia a partir de tu próximo mensaje. Los permisos no cambian.",
                theme::component_palette(),
            )
            .role(openide_ui::TextRole::Small)
            .muted()
            .render(),
        ])
        .gap(2.0),
        ButtonGroup::new(vec![
            Button::new(action(request, "reject"), "Rechazar", theme::component_palette())
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render(),
            theme::primary(action(request, "accept"), "Aceptar")
                .size(ControlSize::Compact)
                .render(),
        ])
        .align_end()
        .render(),
    )
}

/// The card while the model is still writing the call.
pub(crate) fn skeleton(scope: &str, arguments: &Value) -> Element {
    let bar = |index: usize, fraction: f32| {
        tool_card_skeleton::bar(
            format!("tool-skeleton:{scope}:{index}"),
            fraction,
            tool_card_skeleton::on_card(),
        )
    };
    let title = match proposed(arguments) {
        Some(mode) => Text::new(title(mode), theme::component_palette())
            .render()
            .centered_y(),
        // Real text sizes the row; the bar holds the mode's place.
        None => Element::row(vec![
            Text::new("El agente propone", theme::component_palette()).render(),
            bar(0, 0.3).flex(1.0),
        ])
        .gap(6.0),
    };
    frame(
        title,
        Element::column(vec![
            Element::row(vec![
                Text::new("·", theme::component_palette()).muted().render(),
                bar(1, 0.64).flex(1.0),
            ])
            .gap(6.0),
            Element::row(vec![
                Text::new("·", theme::component_palette())
                    .role(openide_ui::TextRole::Small)
                    .muted()
                    .render(),
                bar(2, 0.42).flex(1.0),
            ])
            .gap(6.0),
        ])
        .gap(2.0),
        Element::spacer().size(None, Some(BUTTON_HEIGHT)),
    )
}

/// The decided proposal: one flat line, like the other finished tools.
pub(crate) fn resolved(arguments: &Value, output: &str) -> Element {
    let plan = proposed(arguments) == Some(TurnMode::Plan);
    let text = match (openide_harness::switch_mode_accepted(output), plan) {
        (true, true) => "Modo Plan aceptado · próximo mensaje",
        (true, false) => "Modo Agente aceptado · próximo mensaje",
        (false, true) => "Propuesta de Modo Plan rechazada",
        (false, false) => "Propuesta de salir de Modo Plan rechazada",
    };
    Element::row(vec![
        Element::icon(Icon::Plan)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(14.0), Some(14.0))
            .centered_y(),
        Element::truncated_text(text)
            .foreground(theme::FOREGROUND_SUBTLE)
            .font_family(openide_ui::FontFamily::Sans)
            .font_size(13.0)
            .centered_y()
            .flex(1.0),
    ])
    .gap(6.0)
    .padding(6.0)
    .size(None, Some(28.0))
}

#[cfg(test)]
#[path = "mode_proposal_ui_tests.rs"]
mod tests;
