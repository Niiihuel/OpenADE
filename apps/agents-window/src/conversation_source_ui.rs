//! Paged access to complete assistant text without unbounded Markdown layout.

use openide_ui::{Button, ButtonVariant, Component, ControlSize, Element, Icon, Modal};

use crate::{rich_text, theme, transcript::TranscriptItem, view::ViewState};

fn action_id(state: &ViewState, index: usize, start: usize, action: &str) -> String {
    format!(
        "conversation_source:{}:{index}:{start}:{action}",
        state.selected_session.as_deref().unwrap_or("draft")
    )
}

/// The viewer references an existing row. Wait for the live projection to
/// settle so provisional rows cannot shift its index while it is being read.
pub(crate) fn available(state: &ViewState) -> bool {
    !state.running
        && !state.background_run_selected
        && !state
            .active_subagents
            .iter()
            .any(|run| state.selected_session.as_deref() == Some(run.child_session_id.as_str()))
}

fn source(state: &ViewState, index: usize) -> Option<&str> {
    if !available(state) {
        return None;
    }
    match state.transcript.get(index) {
        Some(TranscriptItem::Assistant {
            content,
            streaming: false,
        }) => Some(content),
        _ => None,
    }
}

pub(crate) fn open_button(state: &ViewState, index: usize) -> Element {
    theme::interactive_surface(
        Button::new(
            action_id(state, index, 0, "open"),
            "Ver respuesta completa",
            theme::component_palette(),
        )
        .icon(Icon::File)
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Regular)
        .render(),
    )
}

/// A stale navigation action cannot move another page or another session.
pub(crate) fn apply_action(state: &mut ViewState, id: &str) -> bool {
    let prefix = format!(
        "conversation_source:{}:",
        state.selected_session.as_deref().unwrap_or("draft")
    );
    let Some(parts) = id.strip_prefix(&prefix) else {
        return false;
    };
    let mut parts = parts.split(':');
    let (Some(index), Some(start), Some(action)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    let (Ok(index), Ok(start)) = (index.parse::<usize>(), start.parse::<usize>()) else {
        return false;
    };
    state
        .conversation_details
        .sync(state.selected_session.as_deref());
    if action == "close" {
        if state.conversation_details.source_view() != Some((index, start)) {
            return false;
        }
        state.conversation_details.close_source();
        return true;
    }
    let Some(content) = source(state, index) else {
        return false;
    };
    if action == "open" && start == 0 {
        state.conversation_details.open_source(index);
        return true;
    }
    if state.conversation_details.source_view() != Some((index, start)) {
        return false;
    }
    let page = rich_text::markdown_source_page(content, start);
    if page.start_byte != start {
        return false;
    }
    let next = match action {
        "previous" => page.previous_byte,
        "next" => page.next_byte,
        _ => None,
    };
    if let Some(next) = next {
        state.conversation_details.move_source(index, start, next);
        return true;
    }
    false
}

pub(crate) fn scroll_id(state: &ViewState) -> Option<String> {
    let (index, start) = state.conversation_details.source_view()?;
    Some(format!(
        "modal_body:{}",
        action_id(state, index, start, "close")
    ))
}

pub(crate) fn modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    if state.show_settings {
        return base;
    }
    let Some((index, start)) = state.conversation_details.source_view() else {
        return base;
    };
    let Some(content) = source(state, index) else {
        return base;
    };
    let page = rich_text::markdown_source_page(content, start);
    let width = (viewport_width - 24.0).min(860.0);
    let body_width = (width - 54.0).max(48.0);
    let navigation = |action: &str, label: &str, enabled: bool| {
        let mut button = Button::new(
            action_id(state, index, start, action),
            label,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Regular);
        if !enabled {
            button = button.disabled();
        }
        theme::interactive_surface(button.render())
    };
    let footer = Element::row(vec![
        navigation("previous", "Anterior", page.previous_byte.is_some()),
        Element::text(if page.end_byte == content.len() {
            "Fin de la respuesta"
        } else if page.start_byte == 0 {
            "Inicio de la respuesta"
        } else {
            "Continuación"
        })
        .foreground(theme::FOREGROUND_SUBTLE)
        .font_size(11.0)
        .flex(1.0),
        navigation("next", "Siguiente", page.next_byte.is_some()),
    ])
    .gap(8.0)
    .size(None, Some(32.0));
    let close_id = action_id(state, index, start, "close");
    Modal::new(
        base,
        true,
        "Respuesta completa",
        "Fuente Markdown · lectura por páginas",
        &close_id,
        theme::component_palette(),
    )
    .width(width)
    .height(560.0)
    .body(rich_text::render_markdown_source_page_at_width(
        &page, body_width,
    ))
    .footer(footer)
    .initial_focus(format!("{close_id}:close"))
    .return_focus(action_id(state, index, 0, "open"))
    .backdrop_dismiss(true)
    .render()
}
