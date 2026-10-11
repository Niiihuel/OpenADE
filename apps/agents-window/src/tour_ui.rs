//! The guided tour's bubble, its invitation and its settings rows.

use openide_ui::{
    Button, ButtonVariant, CoachMark, Component, ControlSize, Element, Point, Text, TextRole,
};

use crate::settings_parts;
use crate::tour::{self, TourId};
use crate::view::ViewState;
use crate::theme;

/// Something else needs the user first: a dialog, the command palette, an
/// image or a request for permission. The tour steps aside until it is gone.
fn blocked(state: &ViewState, viewport_width: f32) -> bool {
    state.show_command_center
        || state.composer_image_viewer.is_some()
        || !state.pending_permissions.is_empty()
        || !state.pending_subagent_permissions.is_empty()
        || crate::view::dialog_open(state, viewport_width)
}

/// The step on screen, if the tour is neither over nor stepping aside.
fn current(state: &ViewState, viewport_width: f32) -> Option<(TourId, usize)> {
    let active = state.tour.active?;
    (active.tour.mode() == state.ide.mode
        && active.step < active.tour.steps().len()
        && !blocked(state, viewport_width))
    .then_some((active.tour, active.step))
}

/// While the bubble shows, the window behind it takes no input.
pub(crate) fn showing(state: &ViewState, viewport_width: f32) -> bool {
    current(state, viewport_width).is_some()
}

/// A tour may start: nothing is waiting for an answer.
pub(crate) fn can_start(state: &ViewState, viewport_width: f32) -> bool {
    !blocked(state, viewport_width)
}

pub(crate) fn layer(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    let Some((tour, index)) = current(state, viewport_width) else {
        return base;
    };
    let steps = tour.steps();
    let step = &steps[index];
    let mut mark = CoachMark::new(base, tour::ID, step.targets[0], theme::component_palette())
        .title(step.title)
        .body(step.body)
        .placement(step.side, step.align)
        .step(index, steps.len())
        .accent(theme::INVERSE_SURFACE, theme::INVERSE_INK)
        .skip("Omitir recorrido")
        .key(format!("{}:{}", tour.key(), step.id));
    for target in &step.targets[1..] {
        mark = mark.also(*target);
    }
    mark.render()
}

/// The quiet offer in the corner, for the window mode on screen.
pub(crate) fn invitation(state: &ViewState) -> Option<Element> {
    let tour = TourId::for_mode(state.ide.mode);
    if state.tour.active.is_some() || !state.tour.invites.contains(&tour) {
        return None;
    }
    let palette = theme::menu_palette();
    let copy = Element::column(vec![
        Text::new("¿Un recorrido rápido?", palette).role(TextRole::Label).render(),
        Text::new(format!("Conoce {} en un minuto.", tour.subject()), palette)
            .muted()
            .render(),
    ])
    .gap(3.0);
    let actions = Element::row(vec![
        Element::spacer().flex(1.0),
        Button::new("tour_invite_decline", "Ahora no", palette)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .render(),
        Button::new(tour.start_action(), "Empezar", palette)
            .size(ControlSize::Compact)
            .render(),
    ])
    .gap(6.0);
    Some(
        Element::column(vec![copy, actions])
            .gap(10.0)
            .padding(12.0)
            .background(palette.raised)
            .border(palette.border, 1.0)
            .radius(openide_ui::tokens().radius.surface)
            .shadow(palette.shadow, Point::new(0.0, 3.0), 10.0, 0.0)
            .size(Some(300.0), None)
            .animate_presence("tour_invite"),
    )
}

/// Settings → General: see a tour again, or have both offered again.
pub(crate) fn settings_group(compact: bool) -> Element {
    let start = |tour: TourId, label: &str| {
        crate::settings_ui::outline(tour.start_action(), label).render()
    };
    settings_parts::settings_group(
        "Recorridos",
        vec![
            settings_parts::settings_row(
                "Ventana de agentes",
                "Tareas, compositor, entorno y paneles, en nueve pasos.",
                start(TourId::Agents, "Ver recorrido de agentes"),
                compact,
            ),
            settings_parts::settings_row(
                "IDE",
                "Explorador, editor, panel inferior y agente, en ocho pasos.",
                start(TourId::Ide, "Ver recorrido del IDE"),
                compact,
            ),
            settings_parts::settings_row(
                "Invitaciones",
                "Vuelve a ofrecer cada recorrido al abrir su ventana.",
                crate::settings_ui::outline("tour_reset", "Restablecer").render(),
                compact,
            ),
        ],
    )
}
