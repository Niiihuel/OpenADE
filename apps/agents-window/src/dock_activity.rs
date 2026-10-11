//! Activity inspector: the session's tool calls as a virtualized run log and
//! the detail of the selected one. Nested under `view` to share its helpers.

use super::*;
use crate::tool_presentation::{self, ToolKind};
use crate::transcript::ActivityStatus;
use openide_ui::{Chip, DiffStat, PanelHeader, Spinner};
use std::io::{self, Write};

const ROW_HEIGHT: f32 = 40.0;
const PREVIEW_BYTES: usize = 48 * 1024;

/// What a header chip counts and, once clicked, shows alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusFilter {
    Running,
    Errors,
    Permission,
    Done,
}

impl StatusFilter {
    const ALL: [Self; 4] = [Self::Running, Self::Errors, Self::Permission, Self::Done];

    fn of(output: &Option<String>, is_error: bool, activity: ActivityStatus) -> Option<Self> {
        if is_error || activity == ActivityStatus::Failed {
            return Some(Self::Errors);
        }
        match activity {
            ActivityStatus::Running if output.is_none() => Some(Self::Running),
            ActivityStatus::WaitingPermission | ActivityStatus::PermissionInterrupted => {
                Some(Self::Permission)
            }
            ActivityStatus::Running | ActivityStatus::Completed => Some(Self::Done),
            _ => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Errors => "errors",
            Self::Permission => "permission",
            Self::Done => "done",
        }
    }

    fn label(self, count: usize) -> String {
        match (self, count) {
            (Self::Running, _) => format!("{count} en curso"),
            (Self::Errors, 1) => "1 error".into(),
            (Self::Errors, _) => format!("{count} errores"),
            (Self::Permission, _) => format!("{count} con permiso pendiente"),
            (Self::Done, 1) => "1 lista".into(),
            (Self::Done, _) => format!("{count} listas"),
        }
    }
}

/// The panel's own choices for the session in view. Never persisted.
#[derive(Clone, Debug, Default)]
pub(crate) struct ActivityUi {
    pub status: Option<StatusFilter>,
}

/// Apply the part of a `dock_activity:` action after its prefix.
pub(crate) fn apply_action(state: &mut ViewState, action: &str) {
    let ui = &mut state.activity;
    if let ("filter", key) = action.split_once(':').unwrap_or((action, "")) {
        let next = StatusFilter::ALL.into_iter().find(|filter| filter.key() == key);
        ui.status = if ui.status == next { None } else { next };
    }
}

struct CallRow {
    id: String,
    title: String,
    target: String,
    /// The whole target, for the hint over a shortened one.
    hint: String,
    status: Option<StatusFilter>,
    activity: ActivityStatus,
    stat: Option<(usize, usize)>,
    note: Option<String>,
    selected: bool,
}

/// A server's tool reads as "servidor · herramienta"; a tool with no entry
/// of its own keeps its name in words. Neither shows an identifier.
fn human_title(name: &str, title: &str, kind: ToolKind) -> String {
    if name.is_empty() || !matches!(kind, ToolKind::Mcp | ToolKind::Other) {
        return title.to_owned();
    }
    let words = match kind {
        ToolKind::Mcp => name
            .trim_start_matches("mcp")
            .trim_start_matches(['_', ':'])
            .split("__")
            .map(|part| part.replace('_', " "))
            .collect::<Vec<_>>()
            .join(" · "),
        _ => name.replace('_', " "),
    };
    title.replace(name, &words)
}

fn call_row(state: &ViewState, index: usize, selected: Option<&str>) -> Option<CallRow> {
    let item = &state.transcript[index];
    let TranscriptItem::Tool {
        id,
        name,
        arguments,
        output,
        is_error,
        activity,
    } = item
    else {
        return None;
    };
    let presentation =
        tool_presentation::present(name, arguments, *activity, *is_error, output.as_deref());
    // Reads, searches and listings are worded as the transcript words them.
    let (title, target) = if crate::session_surface::exploration_call(item).is_some() {
        let (verb, target) = crate::session_surface::exploration_words(
            name,
            arguments,
            &presentation,
            crate::session_surface::exploration_needs_attention(item),
            *activity == ActivityStatus::Running,
        );
        (verb.to_owned(), target.into_owned())
    } else {
        (
            human_title(name, &presentation.title, presentation.kind),
            presentation.target.clone(),
        )
    };
    let status = StatusFilter::of(output, *is_error, *activity);
    Some(CallRow {
        id: id.clone(),
        title,
        target,
        hint: presentation.target,
        status,
        activity: *activity,
        stat: (presentation.kind == ToolKind::Edit)
            .then(|| crate::chat_diff_ui::entry(state, id))
            .flatten()
            .map(|entry| {
                entry.files.iter().fold((0, 0), |(added, removed), file| {
                    (added + file.additions, removed + file.deletions)
                })
            }),
        note: presentation
            .note
            .filter(|_| status == Some(StatusFilter::Errors)),
        selected: selected == Some(id.as_str()),
    })
}

/// The call's state as a glyph in its color: it never needs reading.
fn status_icon(row: &CallRow) -> Element {
    let (icon, ink) = match (row.status, row.activity) {
        (Some(StatusFilter::Running), _) => {
            return Spinner::new(format!("activity-running:{}", row.id), theme::BRAND)
                .size(14.0)
                .render()
                .centered()
        }
        (Some(StatusFilter::Errors), _) => (Icon::ErrorCircle, theme::DESTRUCTIVE),
        (Some(StatusFilter::Permission), _) => (crate::icons::LOCK, theme::WARNING),
        (Some(StatusFilter::Done), _) => (Icon::Check, theme::ACTIVITY_ADDED),
        (None, ActivityStatus::Interrupted) => (Icon::WarningTriangle, theme::WARNING),
        (None, _) => (Icon::CloseCircle, theme::FOREGROUND_SUBTLE),
    };
    Element::icon(icon).foreground(ink)
}

/// One call of the log: its state, what it did and to what. The selected
/// one keeps its fill under the pointer.
fn tool_row(row: &CallRow, width: f32) -> Element {
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
    let mut trailing = Vec::new();
    if let Some((added, removed)) = row.stat {
        trailing.push(
            DiffStat::new(added, removed, palette)
                .inks(theme::DIFF_ADDED, theme::DIFF_REMOVED)
                .render(),
        );
    }
    if let Some(note) = &row.note {
        trailing.push(
            UiText::new(note, palette)
                .size(10.5)
                .ink(theme::DESTRUCTIVE)
                .truncate()
                .render()
                .size(Some((width * 0.35).min(220.0)), None),
        );
    }
    let mut list_row = ListRow::new(format!("tool:{}", row.id), &row.title, palette)
        .leading(status_icon(row))
        .subtitle(&row.target)
        .subtitle_mono(true)
        .filled()
        .sizing(crate::ui_sizing::DOCK_TOOL);
    if !trailing.is_empty() {
        list_row = list_row.trailing(Element::row(trailing).gap(8.0).centered_y());
    }
    if !row.hint.is_empty() || row.note.is_some() {
        let mut hint = TooltipSpec::new(if row.hint.is_empty() {
            &row.title
        } else {
            &row.hint
        });
        if let Some(note) = &row.note {
            hint = hint.description(note);
        }
        list_row = list_row.tooltip_with(hint);
    }
    list_row
        .render()
        .size(Some((width - 8.0).max(140.0)), Some(ROW_HEIGHT))
}

/// The session at a glance: its calls by state, each a filter, and what
/// they touched.
fn header(state: &ViewState, counts: [usize; 4], touched: &str, width: f32) -> Element {
    let chips: Vec<_> = StatusFilter::ALL
        .into_iter()
        .filter(|filter| counts[*filter as usize] > 0 || state.activity.status == Some(*filter))
        .map(|filter| {
            let active = state.activity.status == Some(filter);
            let mut palette = theme::component_palette();
            if filter == StatusFilter::Errors {
                palette.text = theme::DESTRUCTIVE;
            }
            let chip = Chip::new(filter.label(counts[filter as usize]), palette);
            Element::clickable(
                format!("dock_activity:filter:{}", filter.key()),
                if active { chip } else { chip.outline() }.render(),
            )
        })
        .collect();
    let mut rows = vec![PanelHeader::new("Actividad", theme::component_palette())
        .width(width)
        .render()];
    // Four chips do not fit a narrow dock on one line.
    for line in chips.chunks(if width < 380.0 { 2 } else { 4 }) {
        rows.push(Element::row(line.to_vec()).gap(6.0).size(None, Some(22.0)));
    }
    if !touched.is_empty() {
        rows.push(
            UiText::new(touched, theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .truncate()
                .render(),
        );
    }
    Element::column(rows).gap(6.0)
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

pub(super) fn content(state: &ViewState, width: f32, height: f32) -> Element {
    let width = (width - 24.0).max(160.0);
    // One pass over the transcript: counts, what was touched and which calls
    // the filter keeps. Rows are only built for the calls the log shows.
    let mut counts = [0usize; 4];
    let (mut commands, mut edits) = (0usize, 0usize);
    let mut files = BTreeSet::new();
    let (mut files_known, mut totals_known, mut totals) = (true, true, (0usize, 0usize));
    let mut matching = Vec::new();
    for (index, item) in state.transcript.iter().enumerate() {
        let TranscriptItem::Tool {
            id,
            name,
            arguments,
            output,
            is_error,
            activity,
        } = item
        else {
            continue;
        };
        let status = StatusFilter::of(output, *is_error, *activity);
        if let Some(status) = status {
            counts[status as usize] += 1;
        }
        let kind = tool_presentation::kind(name);
        commands += usize::from(kind == ToolKind::Command);
        if kind == ToolKind::Edit && status == Some(StatusFilter::Done) {
            edits += 1;
            let path = ["path", "file_path"]
                .into_iter()
                .find_map(|key| arguments.get(key)?.as_str());
            match crate::chat_diff_ui::entry(state, id) {
                Some(entry) => {
                    for file in &entry.files {
                        files.insert(file.path.as_str());
                        totals.0 += file.additions;
                        totals.1 += file.deletions;
                    }
                }
                None => {
                    totals_known = false;
                    files_known &= path.is_some();
                    files.extend(path);
                }
            }
        }
        if state
            .activity
            .status
            .map_or(true, |filter| status == Some(filter))
        {
            matching.push(index);
        }
    }
    // Only what is known is said: no file count when a patch's files are
    // not loaded, no line totals until every edit's diff is.
    let mut touched = Vec::new();
    if edits > 0 {
        touched.push(if files_known {
            plural(files.len(), "archivo", "archivos")
        } else {
            plural(edits, "edición", "ediciones")
        });
        if totals_known {
            touched.push(format!("+{} \u{2212}{}", totals.0, totals.1));
        }
    }
    if commands > 0 {
        touched.push(plural(commands, "comando", "comandos"));
    }
    let tool_at = |index: usize| match &state.transcript[index] {
        item @ TranscriptItem::Tool { id, .. } => Some((index, item, id.as_str())),
        _ => None,
    };
    let selected = state
        .selected_tool_id
        .as_deref()
        .and_then(|selected_id| {
            state
                .transcript
                .iter()
                .position(|item| matches!(item, TranscriptItem::Tool { id, .. } if id == selected_id))
        })
        .or_else(|| {
            state
                .transcript
                .iter()
                .rposition(|item| matches!(item, TranscriptItem::Tool { .. }))
        })
        .and_then(tool_at);
    let selected_id = selected.map(|(_, _, id)| id);
    let mut head = vec![header(state, counts, &touched.join(" · "), width)];
    let rows: Vec<_> = matching
        .iter()
        .rev()
        .filter_map(|index| call_row(state, *index, selected_id))
        .collect();
    let count = rows.len();
    if count > 0 {
        let list =
            ScrollArea::uniform_list("dock_activity_tools", count, ROW_HEIGHT, move |index| {
                tool_row(&rows[index], width)
            })
            .render()
            .top_fade(theme::BACKGROUND, 12.0);
        // With a detail under it the log keeps to its rows, up to two fifths
        // of the pane; alone it fills the pane.
        head.push(if selected.is_some() {
            list.size(
                Some(width),
                Some((count as f32 * ROW_HEIGHT).min((height * 0.4).max(ROW_HEIGHT * 3.0))),
            )
        } else {
            list.size(Some(width), None).flex(1.0)
        });
    }
    let mut items = Vec::new();
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
        _,
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
    // The detail and what follows it scroll together under the log.
    head.push(
        ScrollArea::new("side_pane_activity", Element::column(items).gap(8.0))
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .flex(1.0),
    );
    Element::column(head)
        .gap(8.0)
        .padding(12.0)
        .size(None, Some(height))
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
                    .sizing(crate::ui_sizing::GRANT_ACTION)
                    .width(72.0)
                    .ink(theme::DESTRUCTIVE)
                    .hover_ink(theme::FOREGROUND)
                    .render(),
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
            let frame = UiTree::new(content(&state, width, 640.0).background(theme::BACKGROUND)).layout(size);
            crate::settings_ui::visual_qa::write_native_frame_svg(
                &frame,
                &directory.join(format!("activity-output-{width:.0}.svg")),
                size.width,
                size.height,
            )
            .unwrap();
        }
    }

    const OWNER: &str = "activity-review";

    fn tool(
        id: &str,
        name: &str,
        arguments: Value,
        output: Option<&str>,
        activity: ActivityStatus,
    ) -> TranscriptItem {
        TranscriptItem::Tool {
            id: id.into(),
            name: name.into(),
            arguments,
            output: output.map(str::to_owned),
            is_error: activity == ActivityStatus::Failed,
            activity,
        }
    }

    fn user(content: &str) -> TranscriptItem {
        TranscriptItem::User {
            content: content.into(),
            images: vec![],
        }
    }

    /// Two turns: commands that passed and failed, an edit with its diff, a
    /// run of exploration and a call to a server's tool.
    fn review_state() -> ViewState {
        use serde_json::json;
        let mut state = super::super::dock_visual_tests::scrolling_dock_fixture(SideTab::Activity);
        state.selected_session = Some(OWNER.into());
        state.selected_backend = "native".into();
        state.selected_tool_id = None;
        state.chat_diffs.workspace = Some(state.workspace.clone().into());
        state.chat_diffs.session_id = Some(OWNER.into());
        state.chat_diffs.entries.insert(
            "edit-1".into(),
            crate::chat_diff_ui::ToolDiffEntry {
                files: vec![crate::chat_diff_ui::DiffFileView {
                    path: "apps/agents-window/src/dock_activity.rs".into(),
                    old_path: None,
                    additions: 12,
                    deletions: 4,
                    truncated: false,
                    unavailable: None,
                    can_open: true,
                    lines: Arc::from([
                        openide_ui::CodeDiffLine {
                            kind: openide_ui::CodeDiffLineKind::Removed,
                            old_line: Some(7),
                            new_line: None,
                            text: "const ROW_HEIGHT: f32 = 38.0;".into(),
                        },
                        openide_ui::CodeDiffLine {
                            kind: openide_ui::CodeDiffLineKind::Added,
                            old_line: None,
                            new_line: Some(7),
                            text: "const ROW_HEIGHT: f32 = 40.0;".into(),
                        },
                    ]),
                }],
                unavailable: None,
                notes: vec![],
                limited: false,
            },
        );
        let done = ActivityStatus::Completed;
        state.transcript = vec![
            user("Revisá por qué falla la suite de pruebas del panel de actividad"),
            tool("read-1", "read_file", json!({"path":"apps/agents-window/src/dock_activity.rs","start_line":10,"end_line":40}), Some("const ROW_HEIGHT: f32 = 38.0;\nconst PREVIEW_BYTES: usize = 48 * 1024;\n"), done),
            tool("search-1", "search_text", json!({"query":"ROW_HEIGHT"}), Some("src/dock_activity.rs:7: const ROW_HEIGHT"), done),
            tool("list-1", "list_files", json!({"path":"apps/agents-window/src"}), Some("dock_activity.rs\nview.rs"), done),
            tool("cmd-1", "run_command", json!({"command":"cargo test -p agents-window dock_activity"}), Some("running 3 tests\ntest result: ok. 3 passed; 0 failed"), done),
            user("Ahora subí el alto de fila y volvé a correr todo"),
            tool("edit-1", "write_file", json!({"path":"apps/agents-window/src/dock_activity.rs","content":"const ROW_HEIGHT: f32 = 40.0;"}), Some("ok"), done),
            tool("cmd-2", "run_command", json!({"command":"cargo test -p agents-window --no-fail-fast -- --include-ignored view::dock_visual_tests"}), Some("error[E0425]: cannot find value `ROW` in this scope\nerror: could not compile `agents-window`"), ActivityStatus::Failed),
            tool("mcp-1", "mcp__github__create_issue", json!({"repo":"Niiihuel/openADE","title":"Fila de actividad a 40 px","labels":["ui"]}), Some("{\"number\": 42}"), done),
            tool("web-1", "web_fetch", json!({"url":"https://docs.rs/limn/latest/limn/struct.ListRow.html"}), Some("ListRow: one entry of a list."), done),
        ];
        for (id, start, end) in [("cmd-1", 1_000, 13_000), ("cmd-2", 20_000, 84_000), ("edit-1", 15_000, 15_300)] {
            state.item_times.record(id, start, Some(end));
        }
        state
    }

    fn shot(state: &ViewState, width: f32, name: &str) {
        let name = format!("{name}-{width:.0}");
        super::super::dock_visual_tests::export(side_pane(state, width), width, 600.0, &name);
        let directory = std::path::PathBuf::from(format!(
            "{}/../../docs/ui-separation/shots/activity",
            env!("CARGO_MANIFEST_DIR")
        ));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::copy(
            format!("/tmp/openade-dock-review/{name}.png"),
            directory.join(format!("{name}.png")),
        )
        .unwrap();
    }

    #[test]
    #[ignore = "export the Activity run log in each of its states for manual visual review"]
    fn export_activity_visual_review() {
        for width in [420.0, 620.0] {
            let mut state = review_state();
            shot(&state, width, "two-turns");
            for filter in StatusFilter::ALL {
                state.activity.status = Some(filter);
                shot(&state, width, &format!("filter-{}", filter.key()));
            }
            state.activity.status = None;
            state.transcript.clear();
            shot(&state, width, "empty");
        }
    }

    fn visible(frame: &openide_ui::Frame) -> String {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                openide_ui::DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[test]
    fn rows_read_in_words_with_counts_that_filter_and_no_tool_identifiers() {
        let mut state = review_state();
        let frame = UiTree::new(content(&state, 620.0, 900.0)).layout(Size::new(620.0, 900.0));
        let text = visible(&frame);
        for expected in [
            "Actividad",
            "1 error",
            "7 listas",
            "1 archivo · +12 \u{2212}4 · 2 comandos",
            "Ejecutado",
            "Falló · Ejecución de comando",
            "github · create issue",
            "Leyó",
            "dock_activity.rs L10-40",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        for raw in ["run_command", "read_file", "write_file", "mcp__", "search_text"] {
            assert!(!text.contains(raw), "raw name {raw}: {text}");
        }
        // The chip is the filter: only the failed call stays, and it clears.
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "dock_activity:filter:errors"));
        apply_action(&mut state, "filter:errors");
        let frame = UiTree::new(content(&state, 620.0, 900.0)).layout(Size::new(620.0, 900.0));
        let rows: Vec<_> = frame
            .regions
            .iter()
            .filter(|region| region.id.starts_with("tool:"))
            .map(|region| region.id.as_str())
            .collect();
        assert_eq!(rows, ["tool:cmd-2"]);
        apply_action(&mut state, "filter:errors");
        assert_eq!(state.activity.status, None);
    }

    #[test]
    fn the_log_stays_virtual_for_hundreds_of_calls() {
        let mut state = review_state();
        state.transcript = (0..800)
            .map(|index| {
                tool(
                    &format!("call-{index}"),
                    "run_command",
                    serde_json::json!({"command": format!("echo {index}")}),
                    Some("ok"),
                    ActivityStatus::Completed,
                )
            })
            .collect();
        let frame = UiTree::new(content(&state, 420.0, 700.0)).layout(Size::new(420.0, 700.0));
        let rows = frame
            .regions
            .iter()
            .filter(|region| region.id.starts_with("tool:"))
            .count();
        assert!((1..40).contains(&rows), "{rows}");
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
