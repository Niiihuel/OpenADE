//! Activity inspector: a bounded, virtualized event list and one selected result.
//! This module is nested under `view` to share its presentational helpers.

use super::*;
use std::io::{self, Write};

const ROW_HEIGHT: f32 = 38.0;
const PREVIEW_BYTES: usize = 48 * 1024;

struct ToolRow {
    id: String,
    title: String,
    status: &'static str,
    icon: Icon,
    ink: Color,
    selected: bool,
}

/// A tool call in the list: its state, its name and what it is doing. The
/// selected one keeps its fill under the pointer.
fn tool_row(row: &ToolRow, width: f32) -> Element {
    let palette = openide_ui::ComponentPalette {
        surface: if row.selected {
            theme::SELECTED
        } else {
            theme::BACKGROUND
        },
        hover: if row.selected {
            theme::SELECTED
        } else {
            theme::INPUT
        },
        ..theme::component_palette()
    };
    ListRow::new(format!("tool:{}", row.id), &row.title, palette)
        .icon(row.icon)
        .icon_ink(row.ink)
        .trailing(
            Element::truncated_text(row.status)
                .font_family(FontFamily::Sans)
                .font_size(10.5)
                .foreground(row.ink)
                .size(Some((width * 0.35).min(130.0)), Some(24.0)),
        )
        .filled()
        .sizing(crate::ui_sizing::DOCK_TOOL)
        .render()
        .size(Some((width - 8.0).max(140.0)), Some(ROW_HEIGHT))
}

pub(super) fn content(state: &ViewState, width: f32) -> Element {
    let width = (width - 24.0).max(160.0);
    let tools: Vec<_> = state
        .transcript
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            matches!(item, TranscriptItem::Tool { .. }).then_some((index, item))
        })
        .collect();
    let selected = state
        .selected_tool_id
        .as_deref()
        .and_then(|selected_id| {
            tools.iter().copied().find(
                |(_, item)| matches!(item, TranscriptItem::Tool { id, .. } if id == selected_id),
            )
        })
        .or_else(|| tools.last().copied());
    let running = tools
        .iter()
        .filter(|(_, item)| {
            matches!(
                item,
                TranscriptItem::Tool {
                    activity: crate::transcript::ActivityStatus::Running,
                    output: None,
                    ..
                }
            )
        })
        .count();
    let mut items = vec![
        Element::row(vec![
            Element::faded_text("Actividad", 12.0)
                .font_family(FontFamily::Sans)
                .foreground(theme::FOREGROUND)
                .font_size(14.0)
                .size(None, Some(28.0))
                .flex(1.0),
            Element::faded_text(
                if running > 0 {
                    format!("{running} en curso")
                } else {
                    format!("{} herramientas", tools.len())
                },
                12.0,
            )
            .font_family(FontFamily::Sans)
            .foreground(theme::FOREGROUND_SUBTLE)
            .font_size(11.0)
            .size(None, Some(28.0)),
        ])
        .gap(8.0)
        .size(None, Some(28.0)),
    ];
    if !tools.is_empty() {
        let selected_id = selected.and_then(|(_, item)| match item {
            TranscriptItem::Tool { id, .. } => Some(id.as_str()),
            _ => None,
        });
        let rows: Vec<_> = tools
            .iter()
            .rev()
            .filter_map(|(_, item)| {
                let TranscriptItem::Tool {
                    id,
                    name,
                    output,
                    is_error,
                    activity,
                    ..
                } = item
                else {
                    return None;
                };
                let active =
                    output.is_none() && *activity == crate::transcript::ActivityStatus::Running;
                Some(ToolRow {
                    id: id.clone(),
                    title: if name.is_empty() {
                        "Herramienta".to_owned()
                    } else {
                        name.clone()
                    },
                    status: tool_status(output, *is_error, *activity),
                    icon: if *is_error {
                        Icon::WarningTriangle
                    } else if active {
                        Icon::History
                    } else if output.is_some() {
                        Icon::CheckCircle
                    } else {
                        Icon::Code
                    },
                    ink: if *is_error {
                        theme::DESTRUCTIVE
                    } else if active {
                        theme::BRAND
                    } else {
                        theme::FOREGROUND_SUBTLE
                    },
                    selected: selected_id == Some(id.as_str()),
                })
            })
            .collect();
        let count = rows.len();
        items.push(
            ScrollArea::uniform_list("dock_activity_tools", count, ROW_HEIGHT, move |index| {
                tool_row(&rows[index], width)
            })
            .render()
            .top_fade(theme::BACKGROUND, 12.0)
            .size(
                Some(width),
                Some((count.min(5) as f32 * ROW_HEIGHT).max(ROW_HEIGHT)),
            ),
        );
    }
    if let Some((
        index,
        TranscriptItem::Tool {
            id,
            name,
            arguments,
            output,
            is_error,
            activity,
        },
    )) = selected
    {
        let mut detail = vec![
            Element::row(vec![
                UiText::new(
                    if name.is_empty() { "Herramienta" } else { name },
                    theme::component_palette(),
                )
                .role(TextRole::Label)
                .render()
                .flex(1.0),
                theme::text_px(
                    tool_status(output, *is_error, *activity),
                    10.5,
                    if *is_error {
                        theme::DESTRUCTIVE
                    } else {
                        theme::FOREGROUND_SUBTLE
                    },
                ),
            ])
            .gap(8.0),
            UiText::eyebrow("ARGUMENTOS", theme::component_palette()).render(),
            output_block(
                &format!("activity:{id}:arguments"),
                &bounded_json(arguments),
                width - 16.0,
                theme::FOREGROUND,
                100.0,
            ),
        ];
        let media = crate::session_surface::tool_media(state, index, id);
        if let Some(media) = media {
            detail
                .push(UiText::eyebrow("CONTENIDO MULTIMEDIA", theme::component_palette()).render());
            detail.extend(crate::session_surface::tool_media_elements(
                media,
                true,
                state.selected_session.as_deref().unwrap_or(""),
            ));
        } else if let Some(media) = output
            .as_deref()
            .and_then(|output| crate::tool_result_media::preview(name, output))
        {
            detail
                .push(UiText::eyebrow("CONTENIDO MULTIMEDIA", theme::component_palette()).render());
            for image in &media.images {
                detail.push(
                    Element::raster_image(image.clone())
                        .size(None, Some(180.0))
                        .radius(6.0),
                );
            }
            if !media.text.is_empty() {
                detail.push(output_block(
                    &format!("activity:{id}:media"),
                    &media.text,
                    width - 16.0,
                    theme::FOREGROUND,
                    160.0,
                ));
            }
            for notice in &media.notices {
                detail.push(
                    UiText::new(*notice, theme::component_palette())
                        .role(TextRole::Small)
                        .muted()
                        .render(),
                );
            }
        }
        if let Some(output) = output {
            detail.push(UiText::eyebrow("RESULTADO FINAL", theme::component_palette()).render());
            detail.push(output_block(
                &format!("activity:{id}:output"),
                output,
                width - 16.0,
                if *is_error {
                    theme::DESTRUCTIVE
                } else {
                    theme::FOREGROUND
                },
                200.0,
            ));
        }
        if let Some((captured, truncated)) =
            crate::session_surface::tool_partial_output(state, index, id)
        {
            detail.push(
                UiText::eyebrow(
                    if output.is_some() {
                        "SALIDA CAPTURADA"
                    } else if *activity == crate::transcript::ActivityStatus::Running {
                        "SALIDA EN CURSO"
                    } else {
                        "SALIDA PARCIAL"
                    },
                    theme::component_palette(),
                )
                .render(),
            );
            if truncated {
                detail.push(
                    UiText::new(
                        "Salida limitada · parte del contenido fue omitida",
                        theme::component_palette(),
                    )
                    .role(TextRole::Small)
                    .muted()
                    .render(),
                );
            }
            detail.push(output_block(
                &format!("activity:{id}:partial"),
                &captured,
                width - 16.0,
                theme::FOREGROUND,
                160.0,
            ));
        } else if output.is_none() {
            detail.push(
                UiText::new(
                    if *activity == crate::transcript::ActivityStatus::Running {
                        "Esperando resultado…"
                    } else {
                        "Sin resultado confirmado"
                    },
                    theme::component_palette(),
                )
                .muted()
                .render(),
            );
        }
        items.push(inset_card(Element::column(detail).gap(8.0), width));
    } else if state.native_commands.is_empty() {
        items.push(pane_empty_state(
            Icon::History,
            "Sin actividad todavía",
            "Las herramientas y sus resultados aparecerán aquí cuando el agente trabaje.",
            None,
        ));
    }
    append_processes(&mut items, state, width);
    if let Some((stream, output)) = state.transcript.iter().rev().find_map(|item| match item {
        TranscriptItem::ProcessOutput { stream, text } => Some((stream, text)),
        _ => None,
    }) {
        items.push(
            UiText::eyebrow(
                match stream {
                    OutputStream::Stdout => "SALIDA DEL PROCESO",
                    OutputStream::Stderr => "DIAGNÓSTICOS DEL PROCESO",
                },
                theme::component_palette(),
            )
            .render(),
        );
        items.push(output_block(
            "activity:process-output",
            output,
            width,
            theme::FOREGROUND_SUBTLE,
            160.0,
        ));
    }
    Element::column(items).gap(8.0).padding(12.0)
}

/// A borderless panel-colored group inside the inspector.
fn inset_card(content: Element, width: f32) -> Element {
    Card::new(content, theme::panel_card_palette())
        .density(CardDensity::Compact)
        .border(false)
        .radius(RadiusRole::Surface)
        .width(width)
        .render()
}

fn append_processes(items: &mut Vec<Element>, state: &ViewState, width: f32) {
    let selected_native = state.selected_session.as_deref().is_some_and(|id| {
        state.selected_backend == "native"
            || !state.native_commands.is_empty()
            || state
                .sessions
                .iter()
                .any(|session| session.id == id && session.backend == "native")
    });
    if !selected_native {
        return;
    }
    items.push(Separator::horizontal(theme::component_palette()).render());
    items.push(
        Element::row(vec![
            UiText::new("Procesos", theme::component_palette())
                .render()
                .flex(1.0),
            Button::new(
                "native_command_history_open",
                "Historial",
                theme::component_palette(),
            )
            .icon(Icon::History)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .render(),
            Button::new(
                "native_command_operation_history",
                "Operaciones",
                theme::component_palette(),
            )
            .icon(Icon::Terminal)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .render(),
        ])
        .gap(4.0)
        .size(None, Some(30.0)),
    );
    if let Some(error) = &state.native_commands_error {
        items.push(
            openide_ui::Alert::new(
                "No se pudieron cargar los procesos",
                error,
                theme::component_palette(),
            )
            .tone(openide_ui::AlertTone::Error)
            .compact(true)
            .render(),
        );
    } else if state.native_commands.is_empty() {
        items.push(
            UiText::new(
                "No hay procesos registrados en esta tarea.",
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .muted()
            .render(),
        );
    }
    for command in &state.native_commands {
        let status = match &command.exit {
            None => "En ejecución".to_owned(),
            Some(PersistentCommandExit::Exited { code }) => format!(
                "Finalizado · código {}",
                code.map_or_else(|| "?".into(), |code| code.to_string())
            ),
            Some(PersistentCommandExit::Terminated) => "Terminado".to_owned(),
            Some(PersistentCommandExit::TimedOut) => "Tiempo agotado".to_owned(),
            Some(PersistentCommandExit::WaitFailed(_)) => "Estado incierto".to_owned(),
        };
        let details = format!(
            "{} · {}{}",
            if command.pty { "PTY" } else { "Tuberías" },
            status,
            if command.has_output {
                " · salida pendiente"
            } else {
                ""
            }
        );
        let mut process = vec![
            Element::row(vec![
                Element::icon(Icon::Terminal)
                    .foreground(theme::FOREGROUND_SUBTLE)
                    .size(Some(16.0), Some(18.0)),
                UiText::new(shorten(&command.id, 28), theme::component_palette())
                    .role(TextRole::Small)
                    .mono()
                    .render()
                    .flex(1.0),
            ])
            .gap(6.0),
            theme::text_px(details, 10.5, theme::FOREGROUND_SUBTLE),
        ];
        if let Some(owner) = state.selected_session.as_deref() {
            let identity =
                serde_json::to_string(&(owner, &command.id)).expect("command identity serializes");
            process.push(
                Element::row(vec![
                    Button::new(
                        format!("native_command_history_live:{identity}"),
                        "Guardada",
                        theme::component_palette(),
                    )
                    .icon(Icon::History)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Compact)
                    .render()
                    .flex(1.0),
                    Button::new(
                        format!("native_command_output_open:{identity}"),
                        "Salida",
                        theme::component_palette(),
                    )
                    .icon(Icon::Terminal)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Compact)
                    .render()
                    .flex(1.0),
                    Button::new(
                        format!("native_command_control_open:{identity}"),
                        "Controlar",
                        theme::component_palette(),
                    )
                    .icon(Icon::Settings)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Compact)
                    .render()
                    .flex(1.0),
                ])
                .gap(3.0),
            );
        }
        items.push(inset_card(Element::column(process).gap(5.0), width));
    }
    if !state.session_approval_grants.is_empty() {
        items.push(
            UiText::eyebrow(
                format!(
                    "PERMISOS DE SESIÓN · {}",
                    state.session_approval_grants.len()
                ),
                theme::component_palette(),
            )
            .render(),
        );
        for grant in &state.session_approval_grants {
            items.push(
                Element::row(vec![
                    Element::icon(Icon::CheckCircle)
                        .foreground(theme::FOREGROUND_SUBTLE)
                        .size(Some(16.0), Some(28.0)),
                    Element::truncated_text(&grant.tool_name)
                        .font_family(FontFamily::Sans)
                        .font_size(11.0)
                        .foreground(theme::FOREGROUND)
                        .flex(1.0),
                    Button::new(
                        format!("session_approval_revoke:{}", grant.key_digest),
                        "Revocar",
                        theme::component_palette(),
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Compact)
                    .ink(theme::DESTRUCTIVE)
                    .hover_ink(theme::FOREGROUND)
                    .render()
                    .font_size(11.0)
                    .size(Some(72.0), Some(32.0)),
                ])
                .gap(6.0)
                .size(None, Some(32.0)),
            );
        }
    }
}

/// Results can contain megabytes of output; limn bounds the projection and
/// builds only the visible rows.
fn output_block(id: &str, source: &str, width: f32, ink: Color, max_height: f32) -> Element {
    OutputBlock::new(id, source, theme::code_palette())
        .ink(ink)
        .max_height(max_height)
        .width(width)
        .truncation_note("Vista parcial · resultado demasiado extenso")
        .render()
        // Radius 5 is this block's own; see docs/limn-migration-ledger.md.
        .radius(5.0)
}

fn bounded_json(value: &Value) -> String {
    if value.is_null() {
        return "Sin argumentos".to_owned();
    }
    struct LimitedWriter {
        bytes: Vec<u8>,
    }
    impl Write for LimitedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let remaining = PREVIEW_BYTES.saturating_sub(self.bytes.len());
            if buffer.len() > remaining {
                self.bytes.extend_from_slice(&buffer[..remaining]);
                return Err(io::Error::other("preview limit"));
            }
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = LimitedWriter {
        bytes: Vec::with_capacity(1024),
    };
    let truncated = serde_json::to_writer_pretty(&mut writer, value).is_err();
    let mut text = String::from_utf8_lossy(&writer.bytes).into_owned();
    if truncated {
        text.push_str("\n… Vista parcial de argumentos");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{Size, UiTree};

    #[test]
    #[ignore = "export the Activity inspector with long output for manual visual review"]
    fn export_activity_output_visual_review() {
        let directory = std::path::PathBuf::from(format!(
            "{}/../../target/openade-activity-output-review",
            env!("CARGO_MANIFEST_DIR")
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let mut state = super::super::tests::empty_state();
        state.selected_tool_id = Some("t1".into());
        state.transcript = vec![
            TranscriptItem::Tool {
                id: "t1".into(),
                name: "run_command".into(),
                arguments: serde_json::json!({"command": "cargo test --workspace", "timeout": 600}),
                output: Some((0..100_000).map(|n| format!("test case_{n} ... ok\n")).collect()),
                is_error: false,
                activity: crate::transcript::ActivityStatus::Completed,
            },
            TranscriptItem::ProcessOutput {
                stream: OutputStream::Stderr,
                text: "warning: unused variable\n\nerror: aborting due to a previous error".into(),
            },
        ];
        for width in [300.0, 420.0] {
            let size = Size::new(width, 640.0);
            let frame = UiTree::new(content(&state, width).background(theme::BACKGROUND)).layout(size);
            crate::settings_ui::visual_qa::write_native_frame_svg(
                &frame,
                &directory.join(format!("activity-output-{width:.0}.svg")),
                size.width,
                size.height,
            )
            .unwrap();
        }
    }

    #[test]
    fn large_outputs_build_only_visible_rows() {
        let source = (0..100_000)
            .map(|n| format!("line {n}\n"))
            .collect::<String>();
        let frame = UiTree::new(output_block(
            "output",
            &source,
            380.0,
            theme::FOREGROUND,
            120.0,
        ))
        .layout(Size::new(400.0, 180.0));
        assert!(frame.commands.len() < 100);
        assert!(
            frame
                .text_layouts()
                .iter()
                .any(|layout| layout.source.contains("Vista parcial"))
        );
    }

    #[test]
    fn oversized_unicode_arguments_are_bounded_before_shaping() {
        let preview = bounded_json(&serde_json::json!({"text": "á漢🙂".repeat(100_000)}));
        assert!(preview.len() < PREVIEW_BYTES + 100);
        assert!(preview.ends_with("Vista parcial de argumentos"));
    }
}
