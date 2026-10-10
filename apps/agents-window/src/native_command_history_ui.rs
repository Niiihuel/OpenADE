//! Read-only, paginated durable command output, independent of live managers.

use crate::{
    theme,
    view::{MainPage, SideTab, ViewState},
};
use base64::Engine;
use openide_core::{
    NativeCommandOutputBinding, NativeCommandOutputExit, NativeCommandOutputPage,
    NativeCommandOutputStream, NativeCommandOutputSummary,
};
use openide_ui::{
    Button, ButtonVariant, Component, ControlSize, DropdownOption, Element,
    FontFamily, Icon, Modal, PopoverPlacement, Select, Tooltip,
};
use std::path::{Path, PathBuf};

pub(crate) const PAGE_BYTES: usize = 16 * 1024;
pub(crate) const PAGE_RECORDS: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HistoryScope {
    pub session_id: String,
    pub workspace: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NativeCommandHistoryUiState {
    pub scope: Option<HistoryScope>,
    /// The spelling verified against `scope.workspace` when the inspector was
    /// opened. Painting and polling must not canonicalize a path every frame.
    pub workspace_alias: Option<PathBuf>,
    pub request_id: Option<String>,
    pub loading: bool,
    pub summaries: Vec<NativeCommandOutputSummary>,
    pub selected: Option<NativeCommandOutputBinding>,
    pub cursor: u64,
    pub previous_cursors: Vec<u64>,
    pub page: Option<NativeCommandOutputPage>,
    pub page_json: String,
    pub error: Option<String>,
    pub selector_open: bool,
    pub details_open: bool,
    pub exact_data_open: bool,
}

impl NativeCommandHistoryUiState {
    pub(crate) fn matches_scope(&self, owner: Option<&str>, workspace: &Path) -> bool {
        self.scope.as_ref().is_some_and(|scope| {
            owner == Some(scope.session_id.as_str())
                && (workspace == scope.workspace
                    || self.workspace_alias.as_deref() == Some(workspace))
        })
    }

    pub(crate) fn can_next(&self) -> bool {
        !self.loading
            && self.page.as_ref().is_some_and(|page| {
                page.next_cursor > self.cursor && page.next_cursor < page.summary.observed_end
            })
    }
}

pub(crate) fn page_json(page: &NativeCommandOutputPage) -> String {
    let chunks = page.chunks.iter().map(|chunk| {
        let stream = match chunk.stream {
            NativeCommandOutputStream::Stdout => "stdout",
            NativeCommandOutputStream::Stderr => "stderr",
            NativeCommandOutputStream::Terminal => "terminal",
        };
        match std::str::from_utf8(&chunk.bytes) {
            Ok(text) => serde_json::json!({"start":chunk.start,"end":chunk.end,"stream":stream,"encoding":"utf8","text":text}),
            Err(_) => serde_json::json!({"start":chunk.start,"end":chunk.end,"stream":stream,"encoding":"base64","data":base64::engine::general_purpose::STANDARD.encode(&chunk.bytes)}),
        }
    }).collect::<Vec<_>>();
    serde_json::to_string_pretty(&serde_json::json!({"chunks":chunks,"omitted_ranges":page.omitted_ranges.iter().map(|range| serde_json::json!({"start":range.start,"end":range.end})).collect::<Vec<_>>()}))
        .expect("durable output page JSON serializes")
}

fn text(value: impl Into<String>) -> Element {
    Element::text(value)
        .foreground(theme::FOREGROUND)
        .font_family(FontFamily::Sans)
        .font_size(12.0)
}

fn note(value: impl Into<String>) -> Element {
    Element::text(value)
        .foreground(theme::FOREGROUND_SUBTLE)
        .font_family(FontFamily::Sans)
        .font_size(11.0)
}

fn command_label(summary: &NativeCommandOutputSummary) -> String {
    // Keep familiar command text in the picker; full argv and immutable IDs
    // remain available in Details. Limit labels before joining large argv.
    let mut result = String::new();
    for character in summary
        .header
        .argv
        .iter()
        .flat_map(|arg| arg.chars().chain([' ']))
        .take(120)
    {
        result.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
    let result = result.trim();
    if result.is_empty() {
        "Comando".into()
    } else {
        result.into()
    }
}

/// Terminal controls are data, never styling or actions. Keep readable line
/// breaks and make the other controls explicit; exact bytes remain in JSON.
fn readable_text(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    for character in value.chars() {
        if character == '\n' {
            text.push(character);
        } else if character.is_control() {
            use std::fmt::Write;
            write!(&mut text, "\\u{{{:04x}}}", u32::from(character))
                .expect("string writing succeeds");
        } else {
            text.push(character);
        }
    }
    text
}

fn readable_page(page: &NativeCommandOutputPage) -> Vec<Element> {
    let mut body = Vec::new();
    for chunk in &page.chunks {
        let stream = match chunk.stream {
            NativeCommandOutputStream::Stdout => "stdout",
            NativeCommandOutputStream::Stderr => "stderr",
            NativeCommandOutputStream::Terminal => "terminal",
        };
        let (encoding, value) = match std::str::from_utf8(&chunk.bytes) {
            Ok(value) => ("UTF-8", readable_text(value)),
            Err(_) => (
                "Base64 · bytes binarios o UTF-8 partido",
                base64::engine::general_purpose::STANDARD.encode(&chunk.bytes),
            ),
        };
        body.push(note(format!(
            "{stream} · bytes {}–{} · {encoding}",
            chunk.start, chunk.end
        )));
        body.push(text(value).font_family(FontFamily::Monospace));
    }
    for range in &page.omitted_ranges {
        body.push(text(format!(
            "Omisión · bytes {}–{} · {} bytes",
            range.start,
            range.end,
            range.end.saturating_sub(range.start)
        )));
    }
    if page.chunks.is_empty() && page.omitted_ranges.is_empty() {
        body.push(note("Esta página no contiene salida guardada."));
    }
    body
}

pub(crate) fn modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    let local = &state.native_command_history;
    if state.show_settings
        || state.main_page != MainPage::Workspace
        || !state.show_side_pane
        || state.side_tab != SideTab::Activity
        || !local.matches_scope(
            state.selected_session.as_deref(),
            Path::new(&state.workspace),
        )
    {
        return base;
    }
    let has_output = (local.loading
        && local
            .summaries
            .iter()
            .any(|summary| summary.observed_end > 0))
        || local.page.as_ref().is_some_and(|page| {
            !page.chunks.is_empty()
                || !page.omitted_ranges.is_empty()
                || page.summary.terminal.as_ref().is_some_and(|terminal| {
                    terminal.reader_error.is_some()
                        || matches!(
                            terminal.exit_kind,
                            Some(NativeCommandOutputExit::WaitFailed { .. })
                        )
                })
        });
    let has_details = local.page.is_some() && (local.details_open || local.exact_data_open);
    let width = Modal::fit_width(
        viewport_width,
        if local.summaries.is_empty() {
            480.0
        } else {
            560.0
        },
    );
    let body_width = crate::ui_sizing::form_width(width).max(0.0);
    let mut body = Vec::new();
    if local.summaries.len() > 1 {
        let options = local
            .summaries
            .iter()
            .enumerate()
            .map(|(index, summary)| {
                let binding = &summary.header.binding;
                DropdownOption::new(
                    format!(
                        "native_command_history_select:{}",
                        serde_json::to_string(&(
                            binding.manager_generation.as_str(),
                            binding.command_id.as_str()
                        ))
                        .expect("history identity serializes")
                    ),
                    format!("{} · {}", index + 1, command_label(summary)),
                )
                .selected(local.selected.as_ref() == Some(binding))
            })
            .collect();
        let selected_label = local
            .summaries
            .iter()
            .find(|summary| local.selected.as_ref() == Some(&summary.header.binding))
            .map(command_label)
            .unwrap_or_else(|| format!("{} comandos guardados", local.summaries.len()));
        body.push(
            Select::new(
                "native_command_history_selector_toggle",
                selected_label,
                options,
                local.selector_open,
                "native_command_history_selector_dismiss",
                theme::component_palette(),
            )
            .menu_palette(theme::menu_palette())
            .icon(Icon::Terminal)
            .width(body_width)
            .placement(PopoverPlacement::BottomStart)
            .render(),
        );
    } else if let Some(summary) = local.summaries.first() {
        body.push(
            Tooltip::new(
                Element::row(vec![
                    Element::icon(Icon::Terminal)
                        .foreground(theme::FOREGROUND_SUBTLE)
                        .size(Some(16.0), Some(16.0)),
                    Element::truncated_text(command_label(summary))
                        .font_family(FontFamily::Monospace)
                        .foreground(theme::FOREGROUND)
                        .font_size(12.0)
                        .flex(1.0),
                ])
                .gap(8.0)
                .size(Some(body_width), Some(24.0)),
                "native_command_history_command",
                "Comando guardado",
                theme::component_palette(),
            )
            .description(command_label(summary))
            .render(),
        );
    }
    if local.loading {
        body.push(note("Leyendo salida guardada…"));
    }
    if let Some(error) = &local.error {
        body.push(text(format!("No se pudo leer el historial: {error}")));
    }
    if let Some(page) = &local.page {
        let summary = &page.summary;
        let status = match &summary.terminal {
            None => "Captura incompleta: no hay cierre guardado.".to_owned(),
            Some(terminal) if terminal.reader_error.is_some() => {
                "Captura cerrada con error de lectura; la salida está incompleta.".to_owned()
            }
            Some(terminal) if !terminal.exit_observed => {
                "Captura cerrada sin salida del proceso verificada.".to_owned()
            }
            Some(terminal) => format!(
                "Captura cerrada · {}{}",
                match &terminal.exit_kind {
                    Some(NativeCommandOutputExit::Exited) => format!(
                        "finalizado · código {}",
                        terminal
                            .exit_code
                            .map_or_else(|| "no disponible".into(), |code| code.to_string())
                    ),
                    Some(NativeCommandOutputExit::Terminated) => "terminado".into(),
                    Some(NativeCommandOutputExit::TimedOut) => "tiempo agotado".into(),
                    Some(NativeCommandOutputExit::WaitFailed { .. }) =>
                        "estado de salida incierto".into(),
                    None => "sin tipo de salida guardado".into(),
                },
                if summary.omitted_bytes > 0 {
                    " · contiene omisiones"
                } else {
                    ""
                }
            ),
        };
        body.push(note(status));
        body.push(
            Element::row(vec![
                Button::new(
                    "native_command_history_details_toggle",
                    if local.details_open {
                        "Ocultar detalles"
                    } else {
                        "Detalles"
                    },
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render()
                .flex(1.0),
                Button::new(
                    "native_command_history_exact_toggle",
                    if local.exact_data_open {
                        "Ver salida"
                    } else {
                        "Ver datos exactos"
                    },
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render()
                .flex(1.0),
            ])
            .gap(8.0),
        );
        if local.details_open {
            body.push(note(format!(
                "{} bytes observados · {} guardados · {} omitidos\nRango: {}–{} · revisión {}",
                summary.observed_end,
                summary.retained_bytes,
                summary.omitted_bytes,
                local.cursor,
                page.next_cursor,
                summary.revision,
            )));
            let header = &summary.header;
            body.push(text(format!("Conversación: {}\nProyecto: {}\nManager: {}\nComando: {}\nAislamiento: {} · {}\nRegistrado: {} Unix ms\nArgv: {}", header.binding.session_id, header.binding.workspace.display(), header.binding.manager_generation, header.binding.command_id, header.isolation, if header.pty { "PTY" } else { "Tuberías" }, header.registered_at_ms, serde_json::to_string(&header.argv).expect("argv serializes"))).font_family(FontFamily::Monospace));
        }
        if let Some(error) = summary
            .terminal
            .as_ref()
            .and_then(|terminal| terminal.reader_error.as_ref())
        {
            body.push(text(format!("Error completo de captura: {error}")));
        }
        if let Some(NativeCommandOutputExit::WaitFailed { error }) = summary
            .terminal
            .as_ref()
            .and_then(|terminal| terminal.exit_kind.as_ref())
        {
            body.push(text(format!(
                "Error completo al esperar el proceso: {error}"
            )));
        }
        if local.exact_data_open {
            body.push(note("Datos exactos de esta página"));
            body.push(note("Base64 conserva bytes binarios o UTF-8 partido; los controles se muestran como datos."));
            body.push(text(&local.page_json).font_family(FontFamily::Monospace));
        } else {
            body.extend(readable_page(page));
        }
    } else if !local.loading && local.error.is_none() {
        body.push(
            Element::row(vec![
                Element::icon(Icon::Terminal)
                    .foreground(theme::FOREGROUND_SUBTLE)
                    .size(Some(20.0), Some(20.0)),
                Element::column(vec![
                    text("Todavía no hay salida guardada."),
                    note("Los comandos de esta conversación aparecerán aquí."),
                ])
                .gap(5.0)
                .flex(1.0),
            ])
            .gap(12.0)
            .padding(8.0),
        );
    }
    let paginated = !local.previous_cursors.is_empty() || local.can_next();
    let refresh = Button::new(
        "native_command_history_refresh",
        "Actualizar",
        theme::component_palette(),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Compact)
    .enabled(!local.loading)
    .render();
    let mut footer = Vec::new();
    if paginated {
        footer.push(
            Element::row(vec![
                Button::new(
                    "native_command_history_previous",
                    "Anterior",
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .enabled(!local.loading && !local.previous_cursors.is_empty())
                .render(),
                note(format!("Página {}", local.previous_cursors.len() + 1)),
                Button::new(
                    "native_command_history_next",
                    "Siguiente",
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .enabled(local.can_next())
                .render(),
            ])
            .gap(8.0),
        );
    }
    let footer = if width < 420.0 && paginated {
        footer.push(Element::row(vec![Element::spacer().flex(1.0), refresh]));
        Element::column(footer).gap(8.0)
    } else {
        footer.extend([Element::spacer().flex(1.0), refresh]);
        Element::row(footer).gap(8.0)
    };
    Modal::new(
        base,
        true,
        "Historial de salida",
        "",
        "native_command_history_dismiss",
        theme::component_palette(),
    )
    .width(width)
    .height(if has_output || has_details {
        470.0
    } else if local.page.is_some() {
        300.0
    } else if local.error.is_some() {
        240.0
    } else {
        196.0
    })
    .body(Element::column(body).gap(9.0).size(Some(body_width), None))
    .footer(footer)
    .initial_focus("native_command_history_dismiss:close")
    .return_focus("native_command_history_open")
    .backdrop_dismiss(true)
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, HitKind, Key, Point, Size, UiAction, UiEvent, UiTree};

    fn empty_history() -> ViewState {
        let mut state = crate::view::tests::empty_state();
        state.workspace = "/history-fixture/not-present".into();
        state.selected_session = Some("session".into());
        state.main_page = MainPage::Workspace;
        state.show_settings = false;
        state.show_side_pane = true;
        state.side_tab = SideTab::Activity;
        state.native_command_history.scope = Some(HistoryScope {
            session_id: "session".into(),
            workspace: PathBuf::from(&state.workspace),
        });
        state
    }

    #[test]
    fn empty_history_is_compact_with_one_useful_footer_action_and_sans_copy() {
        let state = empty_history();
        for width in [320.0, 900.0] {
            let mut tree = UiTree::new(modal(
                &state,
                Element::button("native_command_history_open", "Historial"),
                width,
            ));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 400.0));
            let card = frame
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:native_command_history_dismiss")
                .unwrap()
                .rect;
            assert!(card.height <= 200.0);
            assert!(card.width <= 480.0 && card.x >= 8.0 && card.x + card.width <= width - 8.0);
            assert_eq!(
                tree.focused_id(),
                Some("native_command_history_dismiss:close")
            );
            for id in [
                "native_command_history_previous",
                "native_command_history_next",
                "native_command_history_selector_toggle",
            ] {
                assert!(
                    !frame
                        .regions
                        .iter()
                        .any(|region| region.id == id && region.kind == HitKind::Button),
                    "unnecessary control {id}"
                );
            }
            assert!(
                !frame.regions.iter().any(|region| {
                    region.id == "native_command_history_dismiss"
                        && region.kind == HitKind::Button
                        && region.rect.height <= 36.0
                }),
                "the backdrop dismissal must not be mistaken for a redundant footer button"
            );
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "native_command_history_refresh"));
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, font_family: FontFamily::Sans, .. } if text.contains("Todavía no hay salida")
            )));
        }
    }

    #[test]
    fn history_escape_mouse_and_backdrop_share_dismissal_and_restore_focus() {
        for method in ["escape", "mouse", "backdrop"] {
            let mut state = empty_history();
            let base = || Element::button("native_command_history_open", "Historial");
            let viewport = Size::new(800.0, 500.0);
            let mut tree = UiTree::new(base());
            tree.set_reduced_motion(true);
            tree.layout(viewport);
            assert!(tree.focus("native_command_history_open"));
            tree.set_root(modal(&state, base(), viewport.width));
            let frame = tree.layout(viewport);
            let actions = if method == "escape" {
                tree.handle_event(UiEvent::Key {
                    key: Key::Escape,
                    shift: false,
                    ctrl: false,
                })
            } else {
                let point = if method == "mouse" {
                    let rect = frame
                        .regions
                        .iter()
                        .find(|region| region.id == "native_command_history_dismiss:close")
                        .unwrap()
                        .rect;
                    Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
                } else {
                    Point::new(4.0, 4.0)
                };
                let mut actions = tree.handle_event(UiEvent::PointerDown(point));
                actions.extend(tree.handle_event(UiEvent::PointerUp(point)));
                actions
            };
            assert_eq!(
                actions,
                vec![UiAction::Clicked {
                    id: "native_command_history_dismiss".into()
                }],
                "{method}"
            );
            state.native_command_history = Default::default();
            tree.set_root(modal(&state, base(), viewport.width));
            tree.layout(viewport);
            assert_eq!(
                tree.focused_id(),
                Some("native_command_history_open"),
                "{method}"
            );
        }
    }

    #[test]
    fn bound_history_scope_uses_only_the_verified_workspace_and_alias() {
        let mut state = empty_history();
        state.native_command_history.workspace_alias =
            Some(PathBuf::from("/verified-project-link"));
        let local = &state.native_command_history;
        assert!(local.matches_scope(Some("session"), Path::new(&state.workspace)));
        assert!(local.matches_scope(Some("session"), Path::new("/verified-project-link")));
        assert!(!local.matches_scope(Some("other-session"), Path::new(&state.workspace)));
        assert!(!local.matches_scope(Some("session"), Path::new("/another-project")));
    }
}
