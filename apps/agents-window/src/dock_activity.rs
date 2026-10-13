//! Activity inspector: the session's tool calls as a virtualized run log and
//! the detail of the selected one. Nested under `view` to share its helpers.

use super::*;
use crate::tool_presentation::{self, ToolKind};
use crate::transcript::ActivityStatus;
use openide_ui::{Chip, DiffStat, PanelHeader, Spinner};
use std::io::{self, Write};

const ROW_HEIGHT: f32 = 32.0;
/// The rail's column. Its line runs under the chevron of a turn's heading.
const RAIL: f32 = 28.0;
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

/// What the header's menu narrows the log to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KindFilter {
    Command,
    Edit,
    Explore,
    Web,
    Mcp,
    Agent,
}

impl KindFilter {
    const ALL: [Self; 6] = [
        Self::Command,
        Self::Edit,
        Self::Explore,
        Self::Web,
        Self::Mcp,
        Self::Agent,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Edit => "edit",
            Self::Explore => "explore",
            Self::Web => "web",
            Self::Mcp => "mcp",
            Self::Agent => "agent",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Command => "Comandos",
            Self::Edit => "Ediciones",
            Self::Explore => "Exploración",
            Self::Web => "Web",
            Self::Mcp => "MCP",
            Self::Agent => "Agentes",
        }
    }

    fn keeps(self, kind: ToolKind) -> bool {
        match self {
            Self::Command => kind == ToolKind::Command,
            Self::Edit => kind == ToolKind::Edit,
            Self::Explore => matches!(kind, ToolKind::File | ToolKind::Search),
            Self::Web => kind == ToolKind::Web,
            Self::Mcp => kind == ToolKind::Mcp,
            Self::Agent => kind == ToolKind::Agent,
        }
    }
}

/// The panel's own choices for the session in view. Never persisted.
#[derive(Clone, Debug, Default)]
pub(crate) struct ActivityUi {
    pub status: Option<StatusFilter>,
    pub kind: Option<KindFilter>,
    pub menu_open: bool,
    /// The session the remembered turns and groups belong to.
    owner: Option<String>,
    /// Turns and exploration groups the user opened or closed, by turn
    /// number and by the group's first call.
    turns: BTreeMap<usize, bool>,
    groups: BTreeMap<String, bool>,
    /// Whether the selected call shows its raw arguments and result.
    pub data_open: bool,
    pub grants_open: bool,
    /// The call just jumped to in the transcript, highlighted for a moment.
    pub revealed: Option<(String, std::time::Instant)>,
    /// The call whose detail was just closed: it closes under its line.
    pub closing: Option<String>,
}

/// Apply the part of a `dock_activity:` action after its prefix. Returns
/// text the host should put on the clipboard.
pub(crate) fn apply_action(state: &mut ViewState, action: &str) -> Option<String> {
    let ui = &mut state.activity;
    if ui.owner != state.selected_session {
        ui.owner = state.selected_session.clone();
        ui.turns.clear();
        ui.groups.clear();
    }
    // A disclosure says the state it asks for, as only the view knows its
    // default: "turn:3:1" opens the third turn.
    let disclosure = |rest: &str| {
        let (key, open) = rest.rsplit_once(':')?;
        Some((key.to_owned(), open == "1"))
    };
    match action.split_once(':').unwrap_or((action, "")) {
        ("filter", key) => {
            let next = StatusFilter::ALL.into_iter().find(|filter| filter.key() == key);
            ui.status = if ui.status == next { None } else { next };
        }
        ("kind", key) => {
            ui.kind = KindFilter::ALL.into_iter().find(|kind| kind.key() == key);
            ui.menu_open = false;
        }
        ("clear", _) => (ui.status, ui.kind) = (None, None),
        ("menu", _) => ui.menu_open = !ui.menu_open,
        ("menu_dismiss", _) => ui.menu_open = false,
        ("turn", rest) => {
            if let Some((Ok(turn), open)) = disclosure(rest).map(|(key, open)| (key.parse(), open)) {
                ui.turns.insert(turn, open);
            }
        }
        ("group", rest) => {
            if let Some((call, open)) = disclosure(rest) {
                ui.groups.insert(call, open);
            }
        }
        ("grants", _) => ui.grants_open = !ui.grants_open,
        ("close", _) => ui.closing = state.selected_tool_id.take(),
        ("data", _) => ui.data_open = !ui.data_open,
        ("copy", _) => {
            let selected = state.selected_tool_id.as_deref()?;
            return state.transcript.iter().find_map(|item| match item {
                TranscriptItem::Tool {
                    id,
                    name,
                    arguments,
                    output,
                    ..
                } if id == selected => Some(format!(
                    "{name}\n\n{}\n\n{}",
                    serde_json::to_string_pretty(arguments).unwrap_or_default(),
                    output.as_deref().unwrap_or_default()
                )),
                _ => None,
            });
        }
        _ => {}
    }
    None
}

/// One line of the log, at the list's single pitch.
enum Row {
    Turn {
        action: String,
        title: String,
        calls: usize,
        open: bool,
    },
    Group {
        action: String,
        title: String,
        open: bool,
        /// The turn ends here: the rail stops at its node.
        last: bool,
    },
    Call {
        row: CallRow,
        nested: bool,
        last: bool,
    },
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
    /// How long the call took, once it ended.
    duration: Option<String>,
    selected: bool,
    /// The copy in the running strip, whose spinner needs its own identity.
    pinned: bool,
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

/// The command each started session runs, by the session's identifier: a
/// later call on that session names the command, never the identifier.
type Commands = std::collections::HashMap<String, String>;

fn started_command(name: &str, arguments: &Value, output: Option<&str>) -> Option<(String, String)> {
    if name != "start_command" {
        return None;
    }
    let result: Value = serde_json::from_str(output?).ok()?;
    let session = result.get("session_id")?.as_str()?.to_owned();
    let command = tool_presentation::present(name, arguments, ActivityStatus::Completed, false, output).target;
    (!command.is_empty()).then_some((session, command))
}

fn call_row(
    state: &ViewState,
    index: usize,
    selected: Option<&str>,
    commands: &Commands,
) -> Option<CallRow> {
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
    let target = commands.get(&target).cloned().unwrap_or(target);
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
        duration: state.item_times.duration_ms(id).map(|ms| match ms {
            0..1000 => "<1 s".into(),
            _ => crate::transcript::duration_label(ms),
        }),
        selected: selected == Some(id.as_str()),
        pinned: false,
    })
}

/// The call's state as a glyph in its color: it never needs reading. A step
/// of an exploration that ended well is quiet, so the run reads as one.
fn status_icon(row: &CallRow, nested: bool) -> Element {
    let (icon, ink) = match (row.status, row.activity) {
        (Some(StatusFilter::Running), _) => {
            let place = if row.pinned { "strip" } else { "log" };
            return Spinner::new(format!("activity-running:{place}:{}", row.id), theme::BRAND)
                .size(14.0)
                .render()
                .centered()
        }
        (Some(StatusFilter::Errors), _) => (Icon::ErrorCircle, theme::DESTRUCTIVE),
        (Some(StatusFilter::Permission), _) => (crate::icons::LOCK, theme::WARNING),
        (Some(StatusFilter::Done), _) if nested => (Icon::Check, theme::FOREGROUND_SUBTLE),
        (Some(StatusFilter::Done), _) => (Icon::Check, theme::ACTIVITY_ADDED),
        (None, ActivityStatus::Interrupted) => (Icon::WarningTriangle, theme::WARNING),
        (None, _) => (Icon::CloseCircle, theme::FOREGROUND_SUBTLE),
    };
    Element::icon(icon).foreground(ink)
}

/// The rail's part of one line: the line down to its node, the node, and
/// the line on to the next one. A line with nothing above or below it on the
/// rail leaves that side out.
fn rail(node: Element, above: bool, below: bool) -> Element {
    // A third of the muted ink: a hairline's border is lost between two nodes.
    let ink = theme::FOREGROUND_SUBTLE;
    let ink = Color::rgba(ink.r, ink.g, ink.b, ink.a / 3);
    let line = |shown: bool| {
        Element::row(vec![
            Element::spacer().flex(1.0),
            Element::spacer().size(Some(1.0), None).background(if shown {
                ink
            } else {
                Color::TRANSPARENT
            }),
            Element::spacer().flex(1.0),
        ])
        .flex(1.0)
    };
    Element::column(vec![
        line(above),
        Element::row(vec![
            Element::spacer().flex(1.0),
            node.size(Some(14.0), Some(14.0)),
            Element::spacer().flex(1.0),
        ]),
        line(below),
    ])
    .gap(2.0)
    .size(Some(RAIL), Some(ROW_HEIGHT))
}

/// The rail beside what a line opened under itself: the line alone, on to
/// the next node, or nothing where the turn ends.
fn rail_through(shown: bool) -> Element {
    let ink = theme::FOREGROUND_SUBTLE;
    Element::row(vec![
        Element::spacer().flex(1.0),
        Element::spacer().size(Some(1.0), None).background(if shown {
            Color::rgba(ink.r, ink.g, ink.b, ink.a / 3)
        } else {
            Color::TRANSPARENT
        }),
        Element::spacer().flex(1.0),
    ])
    .size(Some(RAIL), None)
}

/// One line of the timeline: its node on the rail and, beside it, what it
/// says on a fill every line has at the same width. The whole line is the
/// target; the fill answers for it.
fn timeline_row(
    id: &str,
    rail: Element,
    line: Vec<Element>,
    selected: bool,
    hint: Option<(&str, Option<&str>)>,
    width: f32,
) -> Element {
    let fill = Element::row(line)
        .gap(8.0)
        .padding(8.0)
        .radius(openide_ui::tokens().radius.control)
        .size(None, Some(ROW_HEIGHT - 2.0))
        .hover_background(if selected { theme::SELECTED } else { theme::INPUT })
        .hover_surface(format!("{id}:fill"), [id])
        .flex(1.0);
    let fill = if selected {
        fill.background(theme::SELECTED)
    } else {
        fill
    };
    let target = Element::clickable(id, Element::row(vec![rail, fill.centered_y()]))
        .background(Color::TRANSPARENT)
        .transition_ms(0)
        .size(Some((width - 8.0).max(140.0)), Some(ROW_HEIGHT));
    match hint {
        Some((title, description)) => {
            let hint = Tooltip::new(target, id, title, theme::component_palette());
            match description {
                Some(description) => hint.description(description),
                None => hint,
            }
            .render()
        }
        None => target,
    }
}

fn line_text(text: &str, size: f32, family: openide_ui::FontFamily, ink: Color) -> Element {
    Element::truncated_text(text)
        .font_family(family)
        .font_size(size)
        .foreground(ink)
        .centered_y()
}

/// One call of the log: its state on the rail, what it did and to what, and
/// what it took. A call that failed says why where its target would be.
fn tool_row(row: &CallRow, nested: bool, last: bool, width: f32) -> Element {
    use openide_ui::FontFamily::{Monospace, Sans};
    let id = format!("tool:{}", row.id);
    let mut line = vec![line_text(&row.title, 12.5, Sans, theme::FOREGROUND).shrink_to_fit()];
    line.push(match &row.note {
        Some(note) => line_text(note, 11.5, Sans, theme::DESTRUCTIVE).flex(1.0),
        None if row.target.is_empty() => Element::spacer().flex(1.0),
        None => line_text(&row.target, 11.5, Monospace, theme::FOREGROUND_SUBTLE).flex(1.0),
    });
    if let Some((added, removed)) = row.stat {
        line.push(
            DiffStat::new(added, removed, theme::component_palette())
                .inks(theme::DIFF_ADDED, theme::DIFF_REMOVED)
                .render()
                .centered_y(),
        );
    }
    if let Some(duration) = &row.duration {
        line.push(small(duration, theme::FOREGROUND_SUBTLE).centered_y());
    }
    let hint = (!row.hint.is_empty() || row.note.is_some()).then(|| {
        (
            if row.hint.is_empty() {
                row.title.as_str()
            } else {
                row.hint.as_str()
            },
            row.note.as_deref(),
        )
    });
    // The pinned copy keeps the menu's ID unique in the window.
    openide_ui::ContextMenu::new(
        format!(
            "activity_row:{}:{}",
            if row.pinned { "strip" } else { "log" },
            row.id
        ),
        timeline_row(
            &id,
            // The pinned copy stands alone: no rail joins it to another.
            rail(status_icon(row, nested), !row.pinned, !last && !row.pinned),
            line,
            row.selected,
            hint,
            width,
        ),
        vec![openide_ui::ContextMenuEntry::Item(
            openide_ui::ContextMenuItem::new(
                format!("tool_reveal:{}", row.id),
                "Ver en la conversación",
            )
            .icon(Icon::Chat),
        )],
        theme::menu_palette(),
    )
    .render()
}

/// A turn's heading or an exploration group, at the pitch of a call.
fn log_row(row: &Row, width: f32) -> Element {
    match row {
        Row::Turn {
            action,
            title,
            calls,
            open,
        } => openide_ui::SectionHeader::new(title, theme::component_palette())
            .disclosure(Some(*open))
            .action(action)
            .truncate(true)
            // At the end of the line, where every turn's count is read; a long
            // title shortens before it.
            .trailing(Chip::new(calls.to_string(), theme::component_palette()).render())
            .sizing(crate::ui_sizing::DOCK_TURN)
            .width((width - 8.0).max(140.0))
            .render(),
        Row::Group {
            action,
            title,
            open,
            last,
        } => timeline_row(
            action,
            rail(
                Element::icon(if *open {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                })
                .foreground(theme::FOREGROUND_SUBTLE),
                true,
                !*last,
            ),
            vec![line_text(title, 12.5, openide_ui::FontFamily::Sans, theme::FOREGROUND).flex(1.0)],
            false,
            Some((title, None)),
            width,
        ),
        Row::Call { row, nested, last } => tool_row(row, *nested, *last, width),
    }
}

fn selected_native(state: &ViewState) -> bool {
    state.selected_session.as_deref().is_some_and(|id| {
        state.selected_backend == "native"
            || !state.native_commands.is_empty()
            || state
                .sessions
                .iter()
                .any(|session| session.id == id && session.backend == "native")
    })
}

/// The session at a glance: its calls by state, each a filter, what they
/// touched, and a menu that narrows the log to one kind of call.
fn header(state: &ViewState, counts: [usize; 4], touched: &str, width: f32) -> Element {
    let ui = &state.activity;
    let mut chips: Vec<_> = StatusFilter::ALL
        .into_iter()
        .filter(|filter| counts[*filter as usize] > 0 || ui.status == Some(*filter))
        .map(|filter| {
            let mut palette = theme::component_palette();
            if filter == StatusFilter::Errors {
                palette.text = theme::DESTRUCTIVE;
            }
            let chip = Chip::new(filter.label(counts[filter as usize]), palette);
            Element::clickable(
                format!("dock_activity:filter:{}", filter.key()),
                if ui.status == Some(filter) {
                    chip
                } else {
                    chip.outline()
                }
                .render(),
            )
        })
        .collect();
    if let Some(kind) = ui.kind {
        // The kind in force shows beside the counts; clicking it clears it.
        chips.push(Element::clickable(
            "dock_activity:kind:all",
            Chip::new(format!("{} ×", kind.label()), theme::component_palette()).render(),
        ));
    }
    let mut entries = vec![DropdownMenuEntry::Heading("Mostrar".into())];
    entries.push(
        DropdownMenuItem::new("dock_activity:kind:all", "Todo")
            .selected(ui.kind.is_none())
            .into(),
    );
    entries.extend(KindFilter::ALL.into_iter().map(|kind| {
        DropdownMenuItem::new(format!("dock_activity:kind:{}", kind.key()), kind.label())
            .selected(ui.kind == Some(kind))
            .into()
    }));
    if selected_native(state) {
        entries.push(DropdownMenuEntry::Separator);
        entries.push(
            DropdownMenuItem::new("native_command_history_open", "Historial de comandos")
                .icon(Icon::History)
                .into(),
        );
        entries.push(
            DropdownMenuItem::new("native_command_operation_history", "Operaciones")
                .icon(Icon::Terminal)
                .into(),
        );
    }
    let menu = DropdownMenu::new(
        UiIconButton::new("dock_activity:menu", Icon::Filter, theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .tooltip_with(
                TooltipSpec::new("Filtrar y procesos")
                    .placement(TooltipPlacement::Below)
                    .suppressed(ui.menu_open),
            )
            .render(),
        entries,
        ui.menu_open,
        "dock_activity:menu_dismiss",
        theme::menu_palette(),
    )
    .placement(PopoverPlacement::BottomEnd)
    .context_menu_style()
    .width(220.0)
    .render()
    .popover_return_focus("dock_activity:menu");
    let mut rows = vec![PanelHeader::new("Actividad", theme::component_palette())
        .trailing(vec![menu])
        .width(width)
        .render()];
    // Four chips do not fit a narrow dock on one line.
    for line in chips.chunks(if width < 380.0 { 2 } else { 5 }) {
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

struct Turn<'a> {
    number: usize,
    prompt: &'a str,
    /// Transcript positions of the calls the filters keep, in order.
    calls: Vec<usize>,
}

/// The log's lines, newest turn first: a heading per user message, its
/// calls in order under it while open, and each run of exploration folded
/// into one line as the transcript folds it.
fn log_rows(
    state: &ViewState,
    turns: &[Turn],
    selected: Option<usize>,
    commands: &Commands,
) -> Vec<Row> {
    let ui = &state.activity;
    let remembered = ui.owner == state.selected_session;
    let filtering = ui.status.is_some() || ui.kind.is_some();
    let selected_id = state.selected_tool_id.as_deref();
    let newest = turns.iter().rposition(|turn| !turn.calls.is_empty());
    let mut rows = Vec::new();
    for (position, turn) in turns.iter().enumerate().rev() {
        if turn.calls.is_empty() {
            continue;
        }
        let holds_selected = selected.is_some_and(|index| turn.calls.contains(&index));
        let open = ui
            .turns
            .get(&turn.number)
            .copied()
            .filter(|_| remembered)
            .unwrap_or(filtering || Some(position) == newest || holds_selected);
        let prompt = turn.prompt.lines().next().unwrap_or_default().trim();
        rows.push(Row::Turn {
            action: format!("dock_activity:turn:{}:{}", turn.number, u8::from(!open)),
            title: if turn.number == 0 {
                "Antes del primer mensaje".into()
            } else {
                format!(
                    "Turno {} · {}",
                    turn.number,
                    tool_presentation::clip_text(prompt, 40).0
                )
            },
            calls: turn.calls.len(),
            open,
        });
        if !open {
            continue;
        }
        let mut at = 0;
        while at < turn.calls.len() {
            let index = turn.calls[at];
            let item = &state.transcript[index];
            // A filter shows its matches one by one, never inside a fold.
            let run = (!filtering)
                .then(|| crate::session_surface::exploration_call(item))
                .flatten()
                .map(|call| (call, crate::session_surface::exploration_run(&state.transcript, index)))
                .filter(|(_, (_, count))| *count >= 2);
            let Some((call, (end, count))) = run else {
                rows.extend(
                    call_row(state, index, selected_id, commands)
                        .map(|row| Row::Call { row, nested: false, last: false }),
                );
                at += 1;
                continue;
            };
            let items = &state.transcript[index..end];
            let open = ui
                .groups
                .get(call)
                .copied()
                .filter(|_| remembered)
                .unwrap_or_else(|| {
                    selected.is_some_and(|selected| (index..end).contains(&selected))
                        || items.iter().any(crate::session_surface::exploration_needs_attention)
                });
            let running = items.iter().any(|item| {
                matches!(item, TranscriptItem::Tool { output: None, activity: ActivityStatus::Running, is_error: false, .. })
            });
            rows.push(Row::Group {
                action: format!("dock_activity:group:{call}:{}", u8::from(!open)),
                title: format!(
                    "{} {}",
                    if running { "Explorando" } else { "Exploró" },
                    crate::session_surface::exploration_summary(items)
                ),
                open,
                last: false,
            });
            for index in &turn.calls[at..at + count] {
                if open {
                    rows.extend(
                        call_row(state, *index, selected_id, commands)
                            .map(|row| Row::Call { row, nested: true, last: false }),
                    );
                }
            }
            at += count;
        }
        if let Some(Row::Call { last, .. } | Row::Group { last, .. }) = rows.last_mut() {
            *last = true;
        }
    }
    rows
}

/// What is under way right now, pinned over the log: calls that run or wait
/// for permission, a running command with the tail of its output, and the
/// session's processes that run or hold unread output.
fn running_strip(
    items: &mut Vec<Element>,
    state: &ViewState,
    live: &[usize],
    commands: &Commands,
    width: f32,
) {
    const SHOWN: usize = 3;
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
    }
    let processes: Vec<_> = state
        .native_commands
        .iter()
        // One that ended with output nobody read yet still needs attention.
        .filter(|command| command.exit.is_none() || command.has_output)
        .collect();
    if live.is_empty() && processes.is_empty() {
        return;
    }
    items.push(UiText::eyebrow("EN CURSO", theme::component_palette()).render());
    // The newest are the ones being waited on.
    for index in live.iter().rev().take(SHOWN) {
        let Some(mut row) = call_row(state, *index, state.selected_tool_id.as_deref(), commands)
        else {
            continue;
        };
        row.pinned = true;
        items.push(tool_row(&row, false, true, width));
        if let Some((captured, _)) =
            crate::session_surface::tool_partial_output(state, *index, &row.id)
        {
            items.push(output_block(
                &format!("activity:{}:live", row.id),
                &tool_presentation::terminal_tail(captured.trim_end(), 3, 600).0,
                width,
                theme::FOREGROUND_SUBTLE,
                60.0,
            ));
        }
    }
    if live.len() > SHOWN {
        items.push(small(
            format!("+{} más en curso", live.len() - SHOWN),
            theme::FOREGROUND_SUBTLE,
        ));
    }
    for command in processes {
        items.push(process(state, command, width));
    }
}

pub(super) fn content(state: &ViewState, width: f32, height: f32) -> Element {
    let width = (width - 24.0).max(160.0);
    let ui = &state.activity;
    // One pass over the transcript: counts, what was touched, what is under
    // way and which calls of each turn the filters keep. Rows are then built
    // only for the lines the log shows.
    let mut counts = [0usize; 4];
    let (mut tools, mut commands, mut edits) = (0usize, 0usize, 0usize);
    let mut files = BTreeSet::new();
    let (mut files_known, mut totals_known, mut totals) = (true, true, (0usize, 0usize));
    let mut turns = vec![Turn {
        number: 0,
        prompt: "",
        calls: Vec::new(),
    }];
    let mut live = Vec::new();
    let mut selected = None;
    let mut started = Commands::new();
    for (index, item) in state.transcript.iter().enumerate() {
        let (id, name, arguments, output, is_error, activity) = match item {
            TranscriptItem::User { content, .. } => {
                turns.push(Turn {
                    number: turns.len(),
                    prompt: content,
                    calls: Vec::new(),
                });
                continue;
            }
            TranscriptItem::Tool {
                id,
                name,
                arguments,
                output,
                is_error,
                activity,
            } => (id, name, arguments, output, *is_error, *activity),
            _ => continue,
        };
        tools += 1;
        started.extend(started_command(name, arguments, output.as_deref()));
        if state.selected_tool_id.as_deref() == Some(id) {
            selected.get_or_insert(index);
        }
        let status = StatusFilter::of(output, is_error, activity);
        if let Some(status) = status {
            counts[status as usize] += 1;
        }
        if status == Some(StatusFilter::Running) || activity == ActivityStatus::WaitingPermission {
            live.push(index);
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
        if ui.status.map_or(true, |filter| status == Some(filter))
            && ui.kind.map_or(true, |filter| filter.keeps(kind))
        {
            if let Some(turn) = turns.last_mut() {
                turn.calls.push(index);
            }
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
    let mut head = vec![header(state, counts, &touched.join(" · "), width)];
    running_strip(&mut head, state, &live, &started, width);
    let rows = log_rows(state, &turns, selected, &started);
    let count = rows.len();
    // The selected call's detail opens under its own line, and the one just
    // closed closes there. A call the log does not list (a folded turn, a
    // filter) shows its detail under the log instead.
    let line = |id: &str| {
        rows.iter().enumerate().find_map(|(position, row)| match row {
            Row::Call { row, last, .. } if row.id == id => Some((position, *last)),
            _ => None,
        })
    };
    let opened = match (selected, state.selected_tool_id.as_deref()) {
        (Some(index), Some(id)) => line(id).map(|at| (at, id, Some(index))),
        _ => ui.closing.as_deref().and_then(|id| Some((line(id)?, id, None))),
    };
    let selected = selected.filter(|_| opened.is_none());
    if count > 0 {
        let mut list =
            ScrollArea::uniform_list("dock_activity_tools", count, ROW_HEIGHT, move |index| {
                log_row(&rows[index], width)
            });
        if let Some(((position, last), id, index)) = opened {
            let inner = (width - 8.0 - RAIL).max(140.0);
            let detail = index.map(|index| {
                Element::column(vec![
                    Element::row(vec![
                        // The rail runs on beside it to the next line.
                        rail_through(!last),
                        inset_card(
                            detail(state, index, &state.transcript[index], inner - 16.0, false),
                            inner,
                        ),
                    ]),
                    Element::spacer().size(None, Some(6.0)),
                ])
            });
            list = list.expansion(
                position,
                openide_ui::Collapse::new(format!("dock_activity:detail:{id}"), detail)
                    .expand()
                    .render(),
            );
        }
        let list = list.render().top_fade(theme::BACKGROUND, 12.0);
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
    } else if tools > 0 {
        head.push(pane_empty_state(
            Icon::Filter,
            "Sin resultados para este filtro",
            "Ninguna llamada de esta sesión coincide con lo que elegiste.",
            Some(("dock_activity:clear", "Quitar filtro")),
        ));
    } else if head.len() == 1 {
        head.push(pane_empty_state(
            Icon::History,
            "Sin actividad todavía",
            "Las herramientas y sus resultados aparecerán aquí cuando el agente trabaje.",
            None,
        ));
    }
    let mut items = Vec::new();
    if let Some(index) = selected {
        items.push(inset_card(
            detail(state, index, &state.transcript[index], width - 16.0, true),
            width,
        ));
    }
    if !state.session_approval_grants.is_empty() {
        items.push(grants(state));
    }
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
    if !items.is_empty() {
        // The detail and what follows it scroll together under the log.
        let below = ScrollArea::new("side_pane_activity", Element::column(items).gap(8.0))
            .tint(theme::FOREGROUND_SUBTLE)
            .render();
        head.push(if selected.is_some() || count == 0 {
            below.flex(1.0)
        } else {
            below.max_height(height * 0.4)
        });
    }
    Element::column(head)
        .gap(8.0)
        .padding(12.0)
        .size(None, Some(height))
}

fn small(text: impl Into<String>, ink: Color) -> Element {
    UiText::new(text, theme::component_palette())
        .role(TextRole::Small)
        .ink(ink)
        .render()
}

/// The start of a result: whole lines within a byte budget, cut before
/// anything is shaped.
fn first_lines(text: &str, lines: usize) -> &str {
    let mut end = text.len().min(4096);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let text = &text[..end];
    match text.match_indices('\n').nth(lines.saturating_sub(1)) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

/// What a call returned, as its first lines. A structured result is data:
/// it stays under "Ver datos".
fn result_preview(
    detail: &mut Vec<Element>,
    id: &str,
    output: Option<&str>,
    failed: bool,
    width: f32,
) {
    let Some(output) = output.map(str::trim).filter(|output| !output.is_empty()) else {
        return;
    };
    if !failed && output.starts_with(['{', '[']) {
        detail.push(small(
            "Resultado estructurado · completo en Ver datos",
            theme::FOREGROUND_SUBTLE,
        ));
        return;
    }
    let preview = first_lines(output, 12);
    detail.push(output_block(
        &format!("activity:{id}:preview"),
        preview,
        width,
        if failed {
            theme::DESTRUCTIVE
        } else {
            theme::FOREGROUND
        },
        200.0,
    ));
    if preview.len() < output.len() {
        detail.push(small(
            "Primeras líneas · completo en Ver datos",
            theme::FOREGROUND_SUBTLE,
        ));
    }
}

/// The file or page a call names, as the button that opens it.
fn link(
    action: String,
    target: tool_presentation::ToolNavigationTarget,
    shown: &str,
    width: f32,
) -> Element {
    let (icon, title, whole) = match target {
        tool_presentation::ToolNavigationTarget::File(path) => {
            (openide_ui::file_icon(path), "Abrir archivo", path)
        }
        tool_presentation::ToolNavigationTarget::Url(url) => (Icon::Globe, "Abrir enlace", url),
    };
    ListRow::new(action, title, theme::component_palette())
        .icon(icon)
        // The target as the row words it, such as a path with its lines.
        .subtitle(if shown.is_empty() { whole } else { shown })
        .subtitle_mono(true)
        .trailing_icon(Icon::ArrowRight)
        .outlined()
        .width(width)
        .tooltip_with(TooltipSpec::new(whole))
        .render()
}

/// A command's result and what was captured while it ran, each bounded.
fn command_output(
    detail: &mut Vec<Element>,
    state: &ViewState,
    index: usize,
    id: &str,
    result: Option<&str>,
    failed: bool,
    activity: ActivityStatus,
    width: f32,
) {
    let capture = crate::session_surface::tool_partial_output(state, index, id);
    if let Some(result) = result.filter(|result| !result.is_empty()) {
        if capture.is_some() {
            detail.push(UiText::eyebrow("RESULTADO FINAL", theme::component_palette()).render());
        }
        detail.push(output_block(
            &format!("activity:{id}:output"),
            result,
            width,
            if failed {
                theme::DESTRUCTIVE
            } else {
                theme::FOREGROUND
            },
            200.0,
        ));
    }
    if let Some((captured, truncated)) = capture {
        detail.push(
            UiText::eyebrow(
                if result.is_some() {
                    "SALIDA CAPTURADA"
                } else if activity == ActivityStatus::Running {
                    "SALIDA EN CURSO"
                } else {
                    "SALIDA PARCIAL"
                },
                theme::component_palette(),
            )
            .render(),
        );
        if truncated {
            detail.push(small(
                "Salida limitada · parte del contenido fue omitida",
                theme::FOREGROUND_SUBTLE,
            ));
        }
        detail.push(output_block(
            &format!("activity:{id}:partial"),
            captured,
            width,
            theme::FOREGROUND,
            160.0,
        ));
    } else if result.is_none() {
        detail.push(small(
            if activity == ActivityStatus::Running {
                "Esperando resultado…"
            } else {
                "Sin resultado confirmado"
            },
            theme::FOREGROUND_SUBTLE,
        ));
    }
}

/// The selected call, shown as what it is: a command with its output, an
/// edit with its diff, a read with what it found. The raw call stays one
/// disclosure away.
/// What the selected call did, in full. Under its own line the line is its
/// heading and closes it; under the log it brings a heading of its own.
fn detail(
    state: &ViewState,
    index: usize,
    item: &TranscriptItem,
    width: f32,
    heading: bool,
) -> Element {
    let (
        Some(row),
        TranscriptItem::Tool {
            id,
            name,
            arguments,
            output,
            is_error,
            activity,
        },
    ) = (call_row(state, index, None, &Commands::new()), item)
    else {
        return Element::spacer();
    };
    let output = output.as_deref();
    let presentation = tool_presentation::present(name, arguments, *activity, *is_error, output);
    let failed = row.status == Some(StatusFilter::Errors);
    let note_ink = if failed {
        theme::DESTRUCTIVE
    } else {
        theme::FOREGROUND_SUBTLE
    };
    let mut detail = Vec::new();
    if heading {
        detail.push(
            Element::row(vec![
                status_icon(&row, false).size(Some(16.0), Some(20.0)),
                UiText::new(&row.title, theme::component_palette())
                    .role(TextRole::Label)
                    .render()
                    .flex(1.0),
                small(row.duration.as_deref().unwrap_or_default(), theme::FOREGROUND_SUBTLE).centered_y(),
                UiIconButton::new("dock_activity:close", Icon::Close, theme::component_palette())
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Dense)
                    .tooltip("Cerrar detalle")
                    .render(),
            ])
            .gap(8.0),
        );
    }
    let navigation = state.selected_session.as_deref().and_then(|owner| {
        Some(link(
            tool_presentation::navigation_action(owner, id),
            tool_presentation::navigation_target(name, arguments)?,
            &presentation.target,
            width,
        ))
    });
    let target = (!presentation.target.is_empty()).then(|| {
        UiText::new(&presentation.target, theme::component_palette())
            .role(TextRole::Small)
            .muted()
            .mono()
            .render()
    });
    // A reason the result below already states is not said twice.
    let mut note = presentation
        .note
        .clone()
        .filter(|note| !output.is_some_and(|output| output.contains(note.as_str())));
    let structured = output.and_then(|output| crate::tool_result_media::preview(name, output));
    match presentation.kind {
        ToolKind::Command => {
            let command = ["command", "cmd"]
                .into_iter()
                .find_map(|key| arguments.get(key)?.as_str())
                .filter(|command| !command.trim().is_empty());
            detail.extend(match command {
                Some(command) => Some(
                    openide_ui::CodeBlock::new(
                        format!("$ {}", tool_presentation::clip_text(command, 12_000).0),
                        theme::code_palette(),
                    )
                    .width(width)
                    .render(),
                ),
                None => target,
            });
            let parsed = output.and_then(|output| tool_presentation::terminal_result(name, output));
            // The process's own outcome, where the tool reports one.
            if let Some((_, status)) = &parsed {
                note = Some(status.clone());
            }
            detail.extend(note.take().map(|note| small(note, note_ink)));
            detail.extend(crate::terminal_dock_ui::command_actions(
                state, "activity", id, name, output,
            ));
            command_output(
                &mut detail,
                state,
                index,
                id,
                parsed.as_ref().map(|(text, _)| text.as_str()).or(output),
                failed,
                *activity,
                width,
            );
        }
        ToolKind::Edit => {
            detail.extend(navigation.or(target));
            if let Some((added, removed)) = row.stat {
                detail.push(
                    DiffStat::new(added, removed, theme::component_palette())
                        .inks(theme::DIFF_ADDED, theme::DIFF_REMOVED)
                        .render(),
                );
            }
            // Only confirmed writes and patches have a diff to rebuild.
            if row.status == Some(StatusFilter::Done)
                && matches!(name.as_str(), "write_file" | "apply_patch")
            {
                detail.push(crate::chat_diff_ui::body(state, id));
            } else {
                result_preview(&mut detail, id, output, failed, width);
            }
        }
        ToolKind::Web | ToolKind::File | ToolKind::Search => {
            detail.extend(navigation.or(target));
            let results = output.map_or_else(Vec::new, |output| {
                crate::search_activity_ui::captured_results(name, output, arguments)
            });
            match state.selected_session.as_deref() {
                Some(owner) if name == "web_search" && !results.is_empty() => {
                    detail.extend(results.iter().take(5).enumerate().map(|(index, result)| {
                        crate::search_activity_ui::result_row(owner, id, index, result, false)
                    }));
                }
                _ => result_preview(&mut detail, id, output, failed, width),
            }
        }
        ToolKind::Mcp => {
            // Top-level arguments as a short table; nested values stay data.
            for (key, value) in arguments.as_object().into_iter().flatten().take(12) {
                let plain = |value: &Value| match value {
                    Value::String(text) => Some(text.clone()),
                    Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
                    _ => None,
                };
                let value = plain(value)
                    .or_else(|| {
                        let items: Option<Vec<_>> =
                            value.as_array()?.iter().take(12).map(plain).collect();
                        Some(items?.join(", "))
                    })
                    .map_or_else(
                        || "Datos anidados · en Ver datos".to_owned(),
                        |text| tool_presentation::clip_text(&text, 200).0,
                    );
                detail.push(
                    Element::row(vec![
                        Element::truncated_text(key)
                            .font_family(FontFamily::Monospace)
                            .font_size(11.0)
                            .foreground(theme::FOREGROUND_SUBTLE)
                            .size(Some((width * 0.3).min(160.0)), None),
                        small(value, theme::FOREGROUND).flex(1.0),
                    ])
                    .gap(8.0),
                );
            }
            if structured.is_none() {
                result_preview(&mut detail, id, output, failed, width);
            }
        }
        ToolKind::Agent => {
            let child = state.subagent_sessions.iter().find(|run| {
                ["child_session_id", "agent_id", "target"]
                    .into_iter()
                    .any(|key| arguments.get(key).and_then(Value::as_str) == Some(&run.child_session_id))
                    || output.is_some_and(|output| output.contains(&run.child_session_id))
            });
            match child {
                Some(run) => {
                    detail.push(UiText::new(&run.title, theme::component_palette()).render());
                    detail.push(small(
                        crate::session_surface::subagent_status_label(run.status),
                        theme::FOREGROUND_SUBTLE,
                    ));
                    detail.push(Element::row(vec![
                        Button::new(
                            format!("subagent_open:{}", run.child_session_id),
                            "Abrir subagente",
                            theme::component_palette(),
                        )
                        .variant(ButtonVariant::Outline)
                        .size(ControlSize::Compact)
                        .render(),
                        Element::spacer().flex(1.0),
                    ]));
                }
                None => {
                    detail.extend(target);
                    result_preview(&mut detail, id, output, failed, width);
                }
            }
        }
        ToolKind::Other => {
            detail.extend(target);
            result_preview(&mut detail, id, output, failed, width);
        }
    }
    detail.extend(note.map(|note| small(note, note_ink)));
    let media = crate::session_surface::tool_media(state, index, id);
    if let Some(media) = media {
        detail.push(UiText::eyebrow("CONTENIDO MULTIMEDIA", theme::component_palette()).render());
        detail.extend(crate::session_surface::tool_media_elements(
            media,
            true,
            state.selected_session.as_deref().unwrap_or(""),
        ));
    } else if let Some(media) = structured {
        detail.push(UiText::eyebrow("CONTENIDO MULTIMEDIA", theme::component_palette()).render());
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
                width,
                theme::FOREGROUND,
                160.0,
            ));
        }
        for notice in &media.notices {
            detail.push(small(*notice, theme::FOREGROUND_SUBTLE));
        }
    }
    detail.push(Element::row(vec![
        Button::new(
            format!("tool_reveal:{id}"),
            "Ver en la conversación",
            theme::component_palette(),
        )
        .icon(Icon::Chat)
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Compact)
        .render(),
        Element::spacer().flex(1.0),
    ]));
    detail.extend(state.transcript.iter().filter_map(|item| match item {
        TranscriptItem::Hook(hook) if hook.tool_call_id == *id => {
            Some(crate::session_surface::hook_activity_card(hook))
        }
        _ => None,
    }));
    // The call as it was made and answered, for whoever needs the data.
    let mut data = Vec::new();
    if state.activity.data_open {
        data.push(
            UiText::new(format!("{name} · {id}"), theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .mono()
                .render(),
        );
        data.push(UiText::eyebrow("ARGUMENTOS", theme::component_palette()).render());
        data.push(output_block(
            &format!("activity:{id}:arguments"),
            &bounded_json(arguments),
            width - 40.0,
            theme::FOREGROUND,
            100.0,
        ));
        if let Some(output) = output {
            data.push(UiText::eyebrow("RESULTADO FINAL", theme::component_palette()).render());
            data.push(output_block(
                &format!("activity:{id}:raw"),
                output,
                width - 40.0,
                if failed {
                    theme::DESTRUCTIVE
                } else {
                    theme::FOREGROUND
                },
                200.0,
            ));
        }
        data.push(Element::row(vec![
            Button::new("dock_activity:copy", "Copiar", theme::component_palette())
                .variant(ButtonVariant::Subtle)
                .size(ControlSize::Compact)
                .render(),
            Element::spacer().flex(1.0),
        ]));
    }
    detail.push(
        openide_ui::Collapsible::new(
            "dock_activity:data",
            "Ver datos",
            Element::column(data).gap(8.0),
            theme::component_palette(),
        )
        .icon(Icon::Code)
        .expanded(state.activity.data_open)
        .render(),
    );
    Element::column(detail).gap(8.0)
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

/// A process of the session that runs or holds unread output, with its
/// controls.
fn process(state: &ViewState, command: &PersistentCommandSessionInfo, width: f32) -> Element {
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
    // A started command has no lifetime cap: say what does end it.
    let details = format!(
        "{} · {}{}{}",
        if command.pty { "PTY" } else { "Tuberías" },
        status,
        if command.has_output {
            " · salida pendiente"
        } else {
            ""
        },
        if command.exit.is_none() {
            " · Se detiene al cerrar openADE"
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
    inset_card(Element::column(process).gap(5.0), width)
}

/// What the session was allowed for good, each with its way back.
fn grants(state: &ViewState) -> Element {
    let mut rows = Vec::new();
    if state.activity.grants_open {
        for grant in &state.session_approval_grants {
            rows.push(
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
    openide_ui::Collapsible::new(
        "dock_activity:grants",
        "Permisos de sesión",
        Element::column(rows),
        theme::component_palette(),
    )
    .icon(Icon::CheckCircle)
    .detail(state.session_approval_grants.len().to_string())
    .expanded(state.activity.grants_open)
    .render()
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

    /// A third turn as a dev server leaves it: started, polled, and a poll
    /// that found the process gone.
    fn served(state: &mut ViewState) {
        use serde_json::json;
        const SESSION: &str = "5d4b6f6e-d58c-4326-9e27-41f2a0c1b7aa";
        let started = format!("{{\"session_id\":\"{SESSION}\"}}");
        state.transcript.extend([
            user("puedes iniciarme mi proyecto? y dejalo corriendo en segundo plano mientras sigo"),
            tool("start-1", "start_command", json!({"command":"npm run dev"}), Some(&started), ActivityStatus::Completed),
            tool("poll-1", "poll_command", json!({"session_id": SESSION}), Some("{\"events\":[]}"), ActivityStatus::Completed),
            tool("poll-2", "poll_command", json!({"session_id": SESSION}), Some("Proceso finalizado con código 1"), ActivityStatus::Failed),
        ]);
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
        for width in [368.0, 420.0, 620.0] {
            let mut state = review_state();
            served(&mut state);
            apply_action(&mut state, "turn:2:1");
            apply_action(&mut state, "turn:1:1");
            apply_action(&mut state, "group:read-1:1");
            shot(&state, width, "timeline");
            let mut state = review_state();
            shot(&state, width, "two-turns");
            for filter in [StatusFilter::Errors, StatusFilter::Done] {
                state.activity.status = Some(filter);
                shot(&state, width, &format!("filter-{}", filter.key()));
            }
            state.activity.status = None;
            state.activity.kind = Some(KindFilter::Agent);
            shot(&state, width, "no-results");
            state.activity.kind = Some(KindFilter::Command);
            shot(&state, width, "filter-kind-command");
            state.activity.kind = None;
            apply_action(&mut state, "menu");
            shot(&state, width, "menu");
            apply_action(&mut state, "menu_dismiss");
            apply_action(&mut state, "turn:1:1");
            shot(&state, width, "turns-open");
            apply_action(&mut state, "group:read-1:1");
            shot(&state, width, "exploration-open");
            let mut live = live_state();
            live.session_approval_grants = vec![SessionApprovalGrantView {
                tool_name: "run_command".into(),
                key_digest: "a".repeat(64),
            }];
            shot(&live, width, "running");
            live.activity.status = Some(StatusFilter::Running);
            shot(&live, width, "filter-running");
            live.activity.status = Some(StatusFilter::Permission);
            shot(&live, width, "filter-permission");
            live.activity.status = None;
            live.activity.grants_open = true;
            live.selected_tool_id = Some("live-1".into());
            shot(&live, width, "detail-running");
            for (id, name) in [
                ("cmd-1", "command"),
                ("cmd-2", "command-failed"),
                ("edit-1", "edit"),
                ("read-1", "read"),
                ("search-1", "search"),
                ("web-1", "web"),
                ("mcp-1", "mcp"),
            ] {
                state.selected_tool_id = Some(id.into());
                shot(&state, width, &format!("detail-{name}"));
            }
            state.transcript.push(tool(
                "agent-1",
                "spawn_agent",
                serde_json::json!({"title":"Revisar pruebas visuales"}),
                Some("{\"child_session_id\":\"child-1\"}"),
                ActivityStatus::Completed,
            ));
            state.transcript.push(TranscriptItem::Hook(crate::transcript::HookActivity {
                tool_call_id: "agent-1".into(),
                hook_id: "formato".into(),
                phase: "post_tool_use".into(),
                decision: Some("allow".into()),
                interrupted: false,
                reason: None,
                stdout: None,
                stderr: None,
                error: None,
            }));
            state.subagent_sessions = vec![SubagentSessionView {
                child_session_id: "child-1".into(),
                parent_session_id: OWNER.into(),
                title: "Revisar pruebas visuales".into(),
                status: SubagentStatus::Running,
                cancellation_requested: false,
                open_inputs: 0,
                inputs: vec![],
                dispatch_error: None,
                depth: 1,
                final_response: None,
            }];
            state.selected_tool_id = Some("agent-1".into());
            shot(&state, width, "detail-agent");
            state.selected_tool_id = Some("mcp-1".into());
            state.activity.data_open = true;
            shot(&state, width, "detail-data-open");
            state.activity.data_open = false;
            state.selected_tool_id = None;
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

    fn frame(state: &ViewState) -> openide_ui::Frame {
        let mut tree = UiTree::new(content(state, 620.0, 900.0));
        tree.set_reduced_motion(true);
        tree.layout(Size::new(620.0, 900.0))
    }

    fn rows(frame: &openide_ui::Frame) -> Vec<&str> {
        frame
            .regions
            .iter()
            .filter(|region| region.id.starts_with("tool:"))
            .map(|region| region.id.as_str())
            .collect()
    }

    #[test]
    fn rows_read_in_words_with_counts_that_filter_and_no_tool_identifiers() {
        let mut state = review_state();
        let first = frame(&state);
        let text = visible(&first);
        for expected in [
            "Actividad",
            "1 error",
            "7 listas",
            "1 archivo · +12 \u{2212}4 · 2 comandos",
            "Turno 2 · Ahora subí el alto de fila y volvé a cor…",
            "Turno 1 · Revisá por qué falla la suite de pruebas…",
            "Escrito",
            "Falló · Ejecución de comando",
            "github · create issue",
            "Abrió",
            // Durations of the calls that were timed: 0.3 s and 64 s.
            "<1 s",
            "1 min 4 s",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        for raw in ["run_command", "read_file", "write_file", "mcp__", "search_text", "web_fetch"] {
            assert!(!text.contains(raw), "raw name {raw}: {text}");
        }
        assert!(!text.contains("ARGUMENTOS") && !text.contains('{'), "{text}");
        // Newest turn first and open, its calls in order; the older one closed.
        assert_eq!(rows(&first), ["tool:edit-1", "tool:cmd-2", "tool:mcp-1", "tool:web-1"]);
        // The chip is the filter: only the failed call stays, and it clears.
        apply_action(&mut state, "filter:errors");
        assert_eq!(rows(&frame(&state)), ["tool:cmd-2"]);
        apply_action(&mut state, "filter:errors");
        assert_eq!(state.activity.status, None);
    }

    #[test]
    fn every_line_has_one_fill_on_one_rail_and_a_turns_count_ends_its_line() {
        let mut state = review_state();
        served(&mut state);
        for action in ["turn:2:1", "turn:1:1", "group:read-1:1"] {
            apply_action(&mut state, action);
        }
        let mut titles = Vec::new();
        for width in [368.0, 620.0] {
            let mut tree = UiTree::new(content(&state, width, 900.0));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 900.0));
            // A call, a step of an exploration and the exploration's own line
            // are the same target: nothing is narrower for hanging from another.
            let lines: Vec<_> = frame
                .regions
                .iter()
                .filter(|region| {
                    region.id.starts_with("tool:") || region.id.starts_with("dock_activity:group:")
                })
                .map(|region| (region.rect.x, region.rect.width, region.rect.height))
                .collect();
            // Three calls, four calls, and an exploration of three with a call after it.
            assert_eq!(lines.len(), 12, "{lines:?}");
            assert!(
                lines.iter().all(|line| {
                    (line.0, line.1) == (lines[0].0, lines[0].1) && (line.2 - ROW_HEIGHT).abs() < 0.01
                }),
                "{lines:?}"
            );
            // Every turn's count ends its line, in chips whose middles line up.
            let counts: Vec<_> = frame
                .text_layouts()
                .iter()
                .filter(|layout| layout.source == "3" || layout.source == "4")
                .map(|layout| layout.runs[0].rect)
                .collect();
            assert_eq!(counts.len(), 3);
            let end = lines[0].0 + lines[0].1;
            let middle = |rect: &openide_ui::Rect| rect.x + rect.width / 2.0;
            for count in &counts {
                assert!(count.x + count.width <= end, "{count:?} past {end}");
                assert!((middle(count) - middle(&counts[0])).abs() < 1.0, "{counts:?}");
            }
            // The title is what gives way to it in a narrow pane.
            let title = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    openide_ui::DrawCommand::Text { text, position, .. } if text.starts_with("Turno 3") => {
                        assert!(position.x < counts[0].x);
                        Some(text.chars().count())
                    }
                    _ => None,
                })
                .expect("the newest turn's title");
            titles.push(title);
        }
        assert!(titles[0] < titles[1], "{titles:?}");
    }

    #[test]
    fn the_selected_calls_detail_opens_under_its_line_and_closes_there() {
        let mut state = review_state();
        let top = |frame: &openide_ui::Frame, id: &str| {
            frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
                .rect
        };
        let closed = frame(&state);
        let apart = top(&closed, "tool:mcp-1").y - top(&closed, "tool:cmd-2").y;
        assert_eq!(apart, ROW_HEIGHT);

        state.selected_tool_id = Some("cmd-2".into());
        let open = frame(&state);
        let (line, close, next) = (
            top(&open, "tool:cmd-2"),
            top(&open, "dock_activity:data"),
            top(&open, "tool:mcp-1"),
        );
        // Its line is its heading: it brings no second one, nor a close button.
        assert!(!open.regions.iter().any(|region| region.id == "dock_activity:close"));
        assert_eq!(visible(&open).matches("Falló · Ejecución de comando").count(), 1);
        // Between its own line and the next one, inside the log, beside the rail.
        assert!(line.y + line.height <= close.y && close.y + close.height <= next.y, "{line:?} {close:?} {next:?}");
        let log = top(&open, "dock_activity_tools");
        assert!(log.y <= close.y && close.y + close.height <= log.y + log.height);
        assert!(close.x > line.x + RAIL);
        assert!(next.y - line.y > ROW_HEIGHT + 60.0, "the lines after it move down");
        // Nothing else holds a detail: the log has the pane to itself.
        assert!(!open.regions.iter().any(|region| region.id == "side_pane_activity"));

        // Closed, it closes where it was open.
        apply_action(&mut state, "close");
        assert_eq!((state.selected_tool_id.as_deref(), state.activity.closing.as_deref()), (None, Some("cmd-2")));
        let closed = frame(&state);
        assert_eq!(top(&closed, "tool:mcp-1").y - top(&closed, "tool:cmd-2").y, ROW_HEIGHT);
        assert!(!closed.regions.iter().any(|region| region.id == "dock_activity:data"));

        // A call the log does not list keeps its detail under the log.
        state.selected_tool_id = Some("edit-1".into());
        state.activity.status = Some(StatusFilter::Errors);
        let filtered = frame(&state);
        assert_eq!(rows(&filtered), ["tool:cmd-2"]);
        let (log, close) = (top(&filtered, "dock_activity_tools"), top(&filtered, "dock_activity:close"));
        assert!(close.y >= log.y + log.height, "{log:?} {close:?}");
    }

    #[test]
    fn a_failed_call_says_why_on_its_line_and_a_later_call_names_its_command() {
        let mut state = review_state();
        served(&mut state);
        let text = visible(&frame(&state));
        // The poll that failed gives its reason where its target would be.
        assert!(text.contains("Falló · Consulta de comando | Proceso finalizado con código 1"), "{text}");
        // Both polls are of the command that was started, not of an identifier.
        assert_eq!(text.matches("npm run dev").count(), 2, "{text}");
        assert!(!text.contains("5d4b6f6e"), "{text}");
        // Under way, a call still names what it runs.
        let live = visible(&frame(&live_state()));
        assert!(!live.contains("Proceso finalizado"), "{live}");
    }

    #[test]
    fn turns_fold_and_a_run_of_exploration_is_one_line_until_opened() {
        let mut state = review_state();
        apply_action(&mut state, "turn:2:0");
        apply_action(&mut state, "turn:1:1");
        let opened = frame(&state);
        let text = visible(&opened);
        assert!(text.contains("Exploró dock_activity.rs, 1 búsqueda, 1 listado"), "{text}");
        assert_eq!(rows(&opened), ["tool:cmd-1"]);
        assert!(opened
            .regions
            .iter()
            .any(|region| region.id == "dock_activity:group:read-1:1"));
        apply_action(&mut state, "group:read-1:1");
        let lines = frame(&state);
        assert_eq!(
            rows(&lines),
            ["tool:read-1", "tool:search-1", "tool:list-1", "tool:cmd-1"]
        );
        let text = visible(&lines);
        assert!(text.contains("Leyó") && text.contains("dock_activity.rs L10-40"), "{text}");
        // What was opened belongs to its session.
        state.selected_session = Some("another".into());
        assert!(!visible(&frame(&state)).contains("Exploró"));
    }

    #[test]
    fn kind_filter_and_empty_states() {
        let mut state = review_state();
        apply_action(&mut state, "menu");
        let menu = visible(&frame(&state));
        for entry in ["Todo", "Comandos", "Ediciones", "Exploración", "Web", "MCP", "Agentes"] {
            assert!(menu.contains(entry), "{entry}: {menu}");
        }
        apply_action(&mut state, "kind:explore");
        assert!(!state.activity.menu_open);
        // A filter shows its matches one by one, in every turn.
        assert_eq!(
            rows(&frame(&state)),
            ["tool:read-1", "tool:search-1", "tool:list-1"]
        );
        apply_action(&mut state, "kind:agent");
        let none = frame(&state);
        assert!(rows(&none).is_empty());
        assert!(visible(&none).contains("Sin resultados para este filtro"));
        assert!(none
            .regions
            .iter()
            .any(|region| region.id == "dock_activity:clear"));
        apply_action(&mut state, "clear");
        assert_eq!(rows(&frame(&state)).len(), 4);
        state.transcript.clear();
        assert!(visible(&frame(&state)).contains("Sin actividad todavía"));
    }

    fn live_state() -> ViewState {
        let mut state = review_state();
        state.transcript.extend([
            user("Corré la suite completa"),
            tool(
                "live-1",
                "run_command",
                serde_json::json!({"command":"cargo test -p agents-window --no-fail-fast"}),
                None,
                ActivityStatus::Running,
            ),
            TranscriptItem::ToolOutput {
                tool_call_id: "live-1".into(),
                text: "running 1870 tests\ntest view::dock_activity::tests::rows ... ok\ntest view::tests::selection ... ok\n".into(),
                truncated: false,
            },
            tool(
                "wait-1",
                "run_command",
                serde_json::json!({"command":"git push origin migrate-to-limn"}),
                None,
                ActivityStatus::WaitingPermission,
            ),
        ]);
        state
    }

    #[test]
    fn what_is_under_way_is_pinned_with_its_live_output() {
        let state = live_state();
        let live = frame(&state);
        let text = visible(&live);
        for expected in ["EN CURSO", "1 en curso", "1 con permiso pendiente"] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        let sources = live
            .text_layouts()
            .iter()
            .map(|layout| layout.source.clone())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(sources.contains("test view::tests::selection ... ok"), "{sources}");
        // Once in the strip and once in its turn.
        assert_eq!(
            rows(&live),
            ["tool:wait-1", "tool:live-1", "tool:live-1", "tool:wait-1"]
        );
    }

    fn detail_text(state: &mut ViewState, id: &str) -> String {
        state.selected_tool_id = Some(id.into());
        let frame = UiTree::new(content(state, 620.0, 1400.0)).layout(Size::new(620.0, 1400.0));
        let layouts = frame.text_layouts();
        format!(
            "{} | {}",
            visible(&frame),
            layouts
                .iter()
                .map(|layout| layout.source.as_str())
                .collect::<Vec<_>>()
                .join(" | ")
        )
    }

    #[test]
    fn the_detail_shows_each_kind_as_what_it_is_and_keeps_raw_data_behind_one_disclosure() {
        let mut state = review_state();
        let command = detail_text(&mut state, "cmd-1");
        assert!(command.contains("$ cargo test -p agents-window dock_activity"), "{command}");
        assert!(command.contains("test result: ok. 3 passed"), "{command}");
        let failed = detail_text(&mut state, "cmd-2");
        assert!(failed.contains("could not compile"), "{failed}");
        let edit = detail_text(&mut state, "edit-1");
        assert!(edit.contains("Abrir archivo") && edit.contains("+12"), "{edit}");
        let read = detail_text(&mut state, "read-1");
        assert!(read.contains("Abrir archivo") && read.contains("PREVIEW_BYTES"), "{read}");
        let web = detail_text(&mut state, "web-1");
        assert!(web.contains("Abrir enlace") && web.contains("one entry of a list"), "{web}");
        let mcp = detail_text(&mut state, "mcp-1");
        assert!(mcp.contains("github · create issue"), "{mcp}");
        assert!(mcp.contains("repo") && mcp.contains("Niiihuel/openADE"), "{mcp}");
        for text in [&command, &failed, &edit, &read, &web, &mcp] {
            assert!(text.contains("Ver datos"));
            assert!(!text.contains("ARGUMENTOS") && !text.contains("mcp__"), "{text}");
        }
        // Raw arguments, result and tool name appear once asked for, and copy.
        assert_eq!(apply_action(&mut state, "data"), None);
        let data = detail_text(&mut state, "mcp-1");
        assert!(data.contains("ARGUMENTOS") && data.contains("mcp__github__create_issue"), "{data}");
        assert!(data.contains("\"number\": 42"), "{data}");
        let copied = apply_action(&mut state, "copy").unwrap();
        assert!(copied.contains("mcp__github__create_issue") && copied.contains("\"labels\""));
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
