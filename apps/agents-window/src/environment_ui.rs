//! Conversation environment assembled from workspace and transcript facts.
//! Project and branch menus share the composer's snapshot-backed actions.

use std::{collections::HashSet, path::Path};

use openide_ui::{
    Button, ButtonVariant, Collapse, Color, Component, ComponentPalette, ControlSize, DiffStat,
    DropdownMenu, DropdownMenuEntry, DropdownMenuItem, Element, FontFamily, FramedCard, Icon,
    IconButton, ListRow, PopoverPanel, PopoverPlacement, RadiusRole, ScrollArea,
    ScrollbarAppearance, SectionHeader, SectionHeaderSizing, SpinnerVariant, Text, TextRole, Tooltip,
    TooltipPlacement, TooltipSpec,
};

use crate::{
    composer_ui::{branch_selector, project_selector, ComposerProps},
    theme,
    transcript::{ActivityStatus, TranscriptItem},
    view::{SideTab, ViewState},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum EnvironmentMenu {
    #[default]
    None,
    Project,
    Branch,
    Local,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionChangesSummaryView {
    pub loading: bool,
    pub available: bool,
    pub partial: bool,
    pub files: usize,
    pub additions: usize,
    pub deletions: usize,
    pub notice: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EnvironmentUiState {
    pub open: bool,
    pub menu: EnvironmentMenu,
    /// Independent popup anchored to the compact composer strip.
    pub composer_local_open: bool,
    pub project_query: String,
    pub changes: SessionChangesSummaryView,
    pub details_expanded: bool,
    pub progress_expanded: bool,
    pub subagents_expanded: bool,
    pub terminals_expanded: bool,
    pub workflow: crate::environment_git_ui::EnvironmentGitUiState,
}

/// The panel is the card, on the surface a popover has: the palette the
/// context popover gives its own card.
pub(crate) fn card_palette() -> ComponentPalette {
    theme::menu_palette()
}

/// For what an open block of the card lists: the block is filled with the
/// card's hover, so its controls hover one step above that.
pub(crate) fn block_palette() -> ComponentPalette {
    theme::action_palette(card_palette().hover, theme::FOREGROUND)
}

/// The canvas palette with the hover and press of the surface a limn button
/// stands on.
fn button_palette(surface: ComponentPalette) -> ComponentPalette {
    ComponentPalette {
        hover: surface.hover,
        selected: surface.selected,
        ..theme::component_palette()
    }
}

/// For a limn button on the environment card.
pub(crate) fn card_button_palette() -> ComponentPalette {
    button_palette(card_palette())
}

/// For a limn button inside an open block of the card.
pub(crate) fn block_button_palette() -> ComponentPalette {
    button_palette(block_palette())
}

/// The environment shares the conversation's trailing space with the utility
/// sidebar. The host makes their expanded presentations mutually exclusive.
pub(crate) fn header_control(state: &ViewState) -> Element {
    IconButton::new("environment_open", Icon::Laptop, theme::component_palette())
        .variant(if state.environment.open {
            ButtonVariant::Subtle
        } else {
            ButtonVariant::Ghost
        })
        .size(ControlSize::Dense)
        .tooltip_with(
            TooltipSpec::new(if state.environment.open {
                "Cerrar entorno del chat"
            } else {
                "Entorno del chat"
            })
            .placement(TooltipPlacement::Below),
        )
        .render()
}

/// Space reserved beside the conversation. The panel has an independent
/// scroll viewport; neither its card nor its selectors create an overlay.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EnvironmentLayout {
    pub progress: f32,
    pub stacked: bool,
    pub panel_width: f32,
    pub allocated_width: f32,
    pub chat_width: f32,
    pub panel_height: f32,
    pub allocated_height: f32,
}

pub(crate) fn layout(
    state: &ViewState,
    available_width: f32,
    available_height: f32,
) -> EnvironmentLayout {
    let progress = state
        .environment_presentation_progress
        .unwrap_or(if state.environment.open { 1.0 } else { 0.0 })
        .clamp(0.0, 1.0);
    let stacked = available_width < 616.0;
    let panel_width = if stacked {
        (available_width - 16.0).max(0.0)
    } else {
        (available_width * 0.30).clamp(280.0, 380.0)
    };
    let allocated_width = if stacked {
        0.0
    } else {
        (panel_width + 16.0) * progress
    };
    let preferred_height = if state.environment.workflow.panel
        != crate::environment_git_ui::EnvironmentGitPanel::Overview
    {
        620.0
    } else if state.environment.details_expanded
        || state.environment.progress_expanded
        || state.environment.subagents_expanded
        || state.environment.terminals_expanded
    {
        580.0
    } else {
        // One more row fits under the closed card; each one after it adds its own.
        let running = usize::from(plan(state).is_some())
            + usize::from(subagents(state).next().is_some())
            + usize::from(!state.terminal_controls.agents.is_empty())
            + usize::from(preview(state).is_some());
        344.0 + running.saturating_sub(1) as f32 * (FramedCard::ROW_HEIGHT + FramedCard::ROW_GAP)
    };
    let panel_height = if stacked {
        (available_height * 0.35 - 12.0)
            .max(0.0)
            .min(preferred_height)
    } else {
        (available_height - 64.0).max(0.0).min(preferred_height)
    };
    EnvironmentLayout {
        progress,
        stacked,
        panel_width,
        allocated_width,
        chat_width: (available_width - allocated_width).max(0.0),
        panel_height,
        allocated_height: if stacked {
            (panel_height + 12.0) * progress
        } else {
            0.0
        },
    }
}

/// What stays around the card's scrolling rows: the header under its margin,
/// the hairline and the foot.
// shortcut: the rows are capped by what is left of the panel's height, so the
// sum of what stays is written here and a test holds it; drop it when a
// panel in the flow can cap a body between a header and a foot by itself.
const CARD_CHROME: f32 = 77.0;
/// The margins a flat card keeps over its first row and under its last.
const BODY_MARGINS: f32 = 16.0;
const HEADER_HEIGHT: f32 = 28.0;
/// The project's row: its name over its path.
const PROJECT_ROW_HEIGHT: f32 = 44.0;
/// Between an open block's fill and what it lists.
const LIST_INSET: f32 = 4.0;
const LOCKED: &str = "Esperá a que termine la operación en curso.";
/// A control of the foot: this far from its fill on every side.
const FOOT_PAD: f32 = 6.0;
/// "Commit y push" with its arrow, inside its fill.
const COMMIT_WIDTH: f32 = 109.0;
/// Between the parts of the foot's summary.
const SUMMARY_GAP: f32 = 6.0;

/// A Git operation, a run or a branch change is under way.
fn locked(state: &ViewState) -> bool {
    state.running
        || state.background_run_selected
        || state.git_action_pending
        || state.composer_branches.switching()
}

/// The views the panel switches between. The project actions open inside
/// the overview.
const VIEWS: [crate::environment_git_ui::EnvironmentGitPanel; 5] = {
    use crate::environment_git_ui::EnvironmentGitPanel::*;
    [Overview, Commit, Compare, CreateBranch, CreateWorktree]
};

/// The panel is the card: the surface of a popover, with its content
/// straight on it. As tall as what it shows, up to `height`: the card's rows
/// scroll past that, between its header and its foot.
pub(crate) fn panel(state: &ViewState, width: f32, height: f32) -> Element {
    use crate::environment_git_ui::EnvironmentGitPanel;
    let workflow = &state.environment.workflow;
    // One pixel in on every side for the surface's border, as the context
    // popover's content stands.
    let (inner, room) = ((width - 2.0).max(0.0), (height - 2.0).max(0.0));
    let shown = match workflow.panel {
        EnvironmentGitPanel::Actions => EnvironmentGitPanel::Overview,
        panel => panel,
    };
    let content = if shown == EnvironmentGitPanel::Overview {
        card(state, inner, room)
    } else {
        // A Git form keeps the six pixels it always stood in from the edge.
        let form = (inner - 10.0).max(120.0);
        Element::panel(
            ScrollArea::new(
                "chat_environment",
                crate::environment_git_ui::build(workflow, form, locked(state)),
            )
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .size(Some(form), None)
            .max_height((room - 10.0).max(0.0)),
        )
        .padding(5.0)
    };
    // Each view has a slot that opens and closes on the library's spring. The
    // one shown stands first: it opens in its place while the one it replaces
    // closes under it, and the panel's height follows both.
    let slot = |view: EnvironmentGitPanel, content| {
        Collapse::new(format!("environment_view:{view:?}"), content)
            .expand()
            .render()
    };
    let mut slots = vec![slot(shown, Some(content))];
    slots.extend(
        VIEWS
            .into_iter()
            .filter(|view| *view != shown)
            .map(|view| slot(view, None)),
    );
    let surface = card_palette();
    Element::column(slots)
        .padding(1.0)
        .background(surface.raised)
        .border(surface.border, 1.0)
        .radius(RadiusRole::Surface.px())
        .size(Some(width), None)
        .clip_children()
}

fn folder_name(workspace: &str) -> &str {
    Path::new(workspace)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if workspace.is_empty() {
                "Elegir carpeta"
            } else {
                workspace
            }
        })
}

fn label(value: &str, ink: Color) -> Element {
    Element::truncated_text(value)
        .font_family(FontFamily::Sans)
        .font_size(13.0)
        .foreground(ink)
}

fn glyph(icon: Icon) -> Element {
    Element::icon(icon)
        .foreground(theme::FOREGROUND_SUBTLE)
        .size(Some(14.0), Some(14.0))
        .centered_y()
}

fn chevron(open: bool) -> Icon {
    if open {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    }
}

/// One row of the card: an icon, its copy and a mark at its end, on a hover
/// fill that reaches past them. It dispatches `action`; without one it keeps
/// its hint and takes no pointer.
fn card_row(
    id: &str,
    icon: Icon,
    copy: Element,
    mark: Icon,
    help: &str,
    action: Option<&str>,
) -> Element {
    let content = Element::row(vec![glyph(icon), copy.flex(1.0).centered_y(), glyph(mark)]).gap(10.0);
    let target = match action {
        Some(action) => {
            // Rounded for its focus ring, which follows it.
            let target =
                Element::clickable(id, content).radius(openide_ui::tokens().radius.control);
            if action == id {
                target
            } else {
                target.dispatch_as(action)
            }
        }
        None => Element::panel(content),
    };
    FramedCard::row(
        format!("{id}:fill"),
        Tooltip::new(target, id, help, theme::component_palette()).render(),
        action.map(|_| id),
        card_palette(),
    )
}

/// Content that keeps to the card's inset among its rows, whose fills reach
/// past it.
fn inset(content: Element) -> Element {
    let side = || Element::spacer().size(Some(FramedCard::REACH), None);
    Element::row(vec![side(), content.flex(1.0), side()])
}

/// What an open block lists, inside the block's fill.
fn listed(rows: Vec<Element>) -> Element {
    let side = || Element::spacer().size(Some(LIST_INSET), None);
    Element::column(vec![
        Element::row(vec![side(), Element::column(rows).gap(2.0).flex(1.0), side()]),
        Element::spacer().size(None, Some(LIST_INSET)),
    ])
}

fn counts(additions: usize, deletions: usize) -> Element {
    DiffStat::new(additions, deletions, theme::component_palette())
        .inks(theme::DIFF_ADDED, theme::DIFF_REMOVED)
        .render()
}

fn changes_help_summary(changes: &SessionChangesSummaryView) -> String {
    let summary = if changes.loading {
        if changes.partial {
            "Actualizando el registro parcial de cambios de este chat".into()
        } else {
            "Actualizando los cambios de este chat".into()
        }
    } else if changes.available {
        format!(
            "{}: {} archivos · +{} · −{}",
            if changes.partial {
                "Cambios parciales de este chat"
            } else {
                "Cambios de este chat"
            },
            changes.files,
            changes.additions,
            changes.deletions
        )
    } else {
        "Cambios de este chat: dato no disponible".into()
    };
    if let Some(notice) = changes
        .notice
        .as_deref()
        .map(str::trim)
        .filter(|notice| !notice.is_empty())
    {
        format!("{summary}\n{notice}")
    } else {
        summary
    }
}

fn section_heading(title: &str, detail: Option<String>, width: f32) -> Element {
    // Inset like the copy of the rows it stands over.
    let mut heading = SectionHeader::new(title, theme::component_palette())
        .width(width)
        .sizing(SectionHeaderSizing {
            padding: 6.0,
            ..SectionHeaderSizing::LABEL
        });
    if let Some(detail) = detail {
        heading = heading.detail(detail);
    }
    heading.render()
}

/// A row of what an open block lists: a title, a detail under it, and a
/// figure or a status beside the title.
fn action_row(
    id: &str,
    icon: Icon,
    title: &str,
    detail: &str,
    trailing: Option<Element>,
    help: &str,
    width: f32,
) -> Element {
    let mut row = ListRow::new(id, title, block_palette())
        .icon(icon)
        .subtitle(detail)
        .trailing_icon(Icon::ChevronRight)
        .width(width)
        .sizing(crate::ui_sizing::ENVIRONMENT_ACTION);
    if let Some(trailing) = trailing {
        row = row.meta_element(trailing);
    }
    Tooltip::new(row.render(), id, help, theme::component_palette()).render()
}

fn source_row(
    action: Option<String>,
    preview: Element,
    title: &str,
    detail: &str,
    help: &str,
    width: f32,
) -> Element {
    // One line, as every other row of the card: what a name leaves out is
    // in its hint.
    let row = |id: &str| {
        let row = ListRow::new(id, title, block_palette())
            .leading(preview)
            .title_ink(theme::FOREGROUND)
            .width(width)
            .sizing(crate::ui_sizing::ENVIRONMENT_ACTION);
        if detail.is_empty() { row } else { row.meta(detail) }
    };
    match action {
        Some(action) => Tooltip::new(
            row(&action).trailing_icon(Icon::ChevronRight).render(),
            action,
            help,
            theme::component_palette(),
        )
        .render(),
        None => row("").enabled(false).render(),
    }
}

/// Only completed, successful local reads enter the environment's file list.
/// The controller revalidates this projection and resolves the path on click.
pub(crate) fn transcript_file_source(item: &TranscriptItem) -> Option<&str> {
    match item {
        TranscriptItem::Tool {
            name,
            arguments,
            output: Some(_),
            is_error: false,
            activity: ActivityStatus::Completed,
            ..
        } if name == "read_file" => arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.trim().is_empty() && path.len() <= 4096),
        _ => None,
    }
}

fn image_size(bytes: u32) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", f64::from(bytes) / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KB", f64::from(bytes) / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn sources(state: &ViewState, width: f32) -> Element {
    const GROUP_LIMIT: usize = 32;
    let mut images = Vec::new();
    let mut files = Vec::new();
    let mut citations = Vec::new();
    let mut seen_files = HashSet::new();
    let mut seen_citations = HashSet::new();
    let mut message_number = 0usize;
    let mut image_count = 0usize;
    for (index, item) in state.transcript.iter().enumerate() {
        if let TranscriptItem::User {
            images: attached, ..
        } = item
        {
            message_number += 1;
            image_count += attached.len();
            for image in attached {
                if images.len() < GROUP_LIMIT {
                    images.push((message_number, image));
                }
            }
        }
        if let Some(path) = transcript_file_source(item) {
            if seen_files.insert(path) && files.len() < GROUP_LIMIT {
                files.push((index, path));
            }
        }
        if let TranscriptItem::WebCitation { title, url } = item {
            if reqwest::Url::parse(url).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
                && seen_citations.insert(url.as_str())
                && citations.len() < GROUP_LIMIT
            {
                citations.push((index, title.as_str(), url.as_str()));
            }
        }
    }
    let file_count = seen_files.len();
    let citation_count = seen_citations.len();
    // What the user gave the chat, then what the agent looked up.
    let inputs = state.composer_attachments.len() + image_count;
    let mut rows = Vec::new();
    if inputs > 0 {
        rows.push(section_heading("Entradas", Some(inputs.to_string()), width));
    }
    let mut omitted = 0usize;
    if !state.composer_attachments.is_empty() {
        for (index, attachment) in state
            .composer_attachments
            .iter()
            .take(GROUP_LIMIT)
            .enumerate()
        {
            let fallback =
                crate::composer_attachments::fallback_visual(&attachment.path, attachment.is_dir);
            let key = crate::composer_attachments::visual_key(&state.workspace, &attachment.path);
            let visual = state
                .composer_attachment_visuals
                .get(&key)
                .unwrap_or(&fallback);
            let action = if visual.is_image {
                format!("composer_attachment_open:{index}")
            } else {
                format!("source:attachment:draft:{index}")
            };
            let preview = visual.preview.as_ref().map_or_else(
                || Element::icon(visual.icon).foreground(theme::FOREGROUND_SUBTLE),
                |image| Element::raster_image(image.clone()).radius(5.0),
            );
            rows.push(source_row(
                Some(action),
                preview,
                &attachment.label,
                "Borrador",
                &attachment.path,
                width,
            ));
        }
        omitted += state.composer_attachments.len().saturating_sub(GROUP_LIMIT);
    }
    if !images.is_empty() {
        for (message, image) in images.iter().take(GROUP_LIMIT) {
            let preview = image.raster.as_ref().map_or_else(
                || Element::icon(Icon::Image).foreground(theme::FOREGROUND_SUBTLE),
                |image| Element::raster_image(image.clone()).radius(5.0),
            );
            let detail = image.raster.as_ref().map_or_else(
                || "Dato no disponible".to_owned(),
                |raster| format!("Mensaje {message} · {}", image_size(raster.size_bytes())),
            );
            rows.push(source_row(
                None, preview, &image.alt, &detail, &image.alt, width,
            ));
        }
        omitted += image_count.saturating_sub(GROUP_LIMIT);
    }
    rows.push(section_heading("Fuentes", Some((file_count + citation_count).to_string()), width));
    if file_count + citation_count == 0 {
        rows.push(theme::text_px("Aún no hay fuentes.", 11.5, theme::FOREGROUND_SUBTLE).padding(6.0));
    }
    if !files.is_empty() {
        for (index, path) in files.iter().take(GROUP_LIMIT) {
            rows.push(source_row(
                Some(format!("environment_source_file:{index}")),
                Element::icon(openide_ui::file_icon(path)).foreground(theme::FOREGROUND_SUBTLE),
                folder_name(path),
                "",
                &format!("Abrir archivo consultado\n{path}"),
                width,
            ));
        }
        omitted += file_count.saturating_sub(GROUP_LIMIT);
    }
    if !citations.is_empty() {
        for (index, title, url) in citations.iter().take(GROUP_LIMIT) {
            rows.push(source_row(
                Some(format!("web_citation_open:{index}")),
                Element::icon(Icon::Globe).foreground(theme::FOREGROUND_SUBTLE),
                if title.trim().is_empty() { url } else { title },
                "",
                url,
                width,
            ));
        }
        omitted += citation_count.saturating_sub(GROUP_LIMIT);
    }
    if omitted > 0 {
        rows.push(
            theme::text_px(
                format!("{omitted} fuentes fuera de esta vista resumida."),
                10.5,
                theme::FOREGROUND_SUBTLE,
            )
            .padding(6.0),
        );
    }
    // One scroll belongs to the entire popover; short source lists never
    // reserve a blank viewport or capture the wheel inside another scroller.
    Element::column(rows).gap(2.0).size(Some(width), None)
}

fn tab_status(state: &ViewState, kind: SideTab) -> String {
    let source = if kind == SideTab::Terminal {
        &state.terminal_tabs
    } else {
        &state.dock_tabs
    };
    let tabs = source
        .iter()
        .filter(|tab| tab.kind == kind)
        .collect::<Vec<_>>();
    let opened = tabs.len();
    if opened == 0 {
        return "Abrir".into();
    }
    if kind == SideTab::Browser {
        let current = state
            .browser
            .tab_id
            .is_some_and(|id| tabs.iter().any(|tab| tab.id == id));
        if current {
            if state.browser.error.is_some() {
                return "Error".into();
            }
            if state.browser.loading {
                return "Cargando".into();
            }
            if state.browser.started {
                return "Listo".into();
            }
        }
    }
    if kind == SideTab::Terminal && tabs.iter().any(|tab| tab.active) {
        if state.terminal_error.is_some() {
            return "Error".into();
        }
        if let Some(snapshot) = &state.terminal_snapshot {
            if snapshot.error.is_some() {
                return "Error".into();
            }
            if snapshot.running {
                return "En ejecución".into();
            }
            if let Some(code) = snapshot.exit_code {
                return format!("Finalizó · {code}");
            }
        }
    }
    format!(
        "{opened} {}",
        if opened == 1 { "abierta" } else { "abiertas" }
    )
}

fn local_menu(state: &ViewState, width: f32, locked: bool) -> Element {
    let mut entries: Vec<DropdownMenuEntry> = Vec::new();
    let available =
        |item: DropdownMenuItem, enabled: bool| if enabled { item } else { item.disabled() };
    if let Some(snapshot) = &state.environment.workflow.snapshot {
        for (index, worktree) in snapshot.worktrees.iter().enumerate() {
            let name = worktree
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Carpeta de trabajo");
            let detail = if worktree.current {
                "Actual"
            } else {
                worktree.branch.as_deref().unwrap_or("HEAD separado")
            };
            let entry = DropdownMenuItem::new(
                format!(
                    "environment_worktree_select:{}:{index}",
                    state.environment.workflow.snapshot_revision
                ),
                name,
            )
            .icon(if worktree.current {
                Icon::Check
            } else {
                Icon::Folder
            })
            .annotation(detail);
            let entry = available(entry, !locked || worktree.current);
            entries.push(entry.into());
        }
    }
    entries.push(
        available(
            DropdownMenuItem::new("environment_choose_project", "Elegir proyecto…")
                .icon(Icon::Folder),
            !locked,
        )
        .into(),
    );
    entries.push(
        DropdownMenuItem::new("environment_copy_path", "Copiar ruta")
            .icon(Icon::File)
            .into(),
    );
    entries.push(
        DropdownMenuItem::new("environment_local_files", "Explorar archivos")
            .icon(Icon::Folder)
            .into(),
    );
    entries.push(
        available(
            DropdownMenuItem::new("environment_git_open_branch", "Crear rama…")
                .icon(Icon::GitBranch),
            !locked,
        )
        .into(),
    );
    entries.push(
        available(
            DropdownMenuItem::new("environment_git_open_worktree", "Crear worktree…")
                .icon(Icon::Add),
            !locked,
        )
        .into(),
    );
    let anchor = card_row(
        "environment_local_toggle",
        Icon::Laptop,
        label("Local", theme::FOREGROUND),
        Icon::ChevronDown,
        &format!("Carpeta de trabajo\n{}", state.workspace),
        Some("environment_local_toggle"),
    );
    DropdownMenu::new(
        anchor,
        entries,
        state.environment.menu == EnvironmentMenu::Local,
        "environment_local_dismiss",
        theme::menu_palette(),
    )
    .context_menu_style()
    .placement(PopoverPlacement::BottomStart)
    .width(width)
    .render()
}

/// The card's heading: its title, the refresh tool and the close button.
fn card_header(loading: bool) -> Element {
    let palette = ComponentPalette {
        text: theme::FOREGROUND_SUBTLE,
        ..card_palette()
    };
    let tool = |id: &str, icon: Icon, hint: &str, enabled: bool| {
        IconButton::new(id, icon, palette)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .enabled(enabled)
            .tooltip_with(TooltipSpec::new(hint).placement(TooltipPlacement::Below))
            .render()
            .centered_y()
    };
    Element::row(vec![
        // The title can be selected.
        Text::new("Entorno", theme::component_palette())
            .role(TextRole::Body)
            .render()
            .flex(1.0)
            .centered_y(),
        tool("environment_git_refresh", Icon::Refresh, "Actualizar entorno", !loading),
        tool("environment_dismiss", Icon::Close, "Cerrar entorno del chat", true),
    ])
    .size(None, Some(HEADER_HEIGHT))
}

/// The card's foot. On the left, the repository's changes, which open their
/// review; on the right, the commit, while there is a repository to commit to.
fn foot(state: &ViewState, width: f32, locked: bool) -> Element {
    // The foot stands on the card's own surface, so its controls hover as
    // the rows do.
    let palette = card_palette();
    let text = |value: &str, ink| {
        Element::truncated_text(value)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(ink)
            .centered_y()
    };
    let review = state.git_review.as_ref();
    let repo = review.and_then(|review| review.as_ref().ok());
    let reading = repo.is_none() && state.environment.workflow.loading;
    let outside = !reading
        && matches!(review, Some(Err(error)) if error == crate::git_review::NOT_A_REPOSITORY);
    let commit = !reading && !outside;
    // shortcut: a row cuts a text that does not fit, and a cut count reads as
    // another count, so the counts are fitted here by widths read off the
    // app's face (at 12 px a digit or a sign advances 6.8 px, a letter 5.3 on
    // average) and left to the hint when there is no room; drop this when limn
    // can leave out a text that does not fit whole.
    let advance = |text: &str| {
        text.chars()
            .map(|c| if c.is_alphabetic() || c == ' ' { 5.3 } else { 6.8 })
            .sum::<f32>()
    };
    let room = width
        - 2.0 * FramedCard::INSET
        - 2.0 * FOOT_PAD
        - if commit { COMMIT_WIDTH } else { 0.0 };
    let mut summary = Vec::new();
    let mut figures = None;
    match repo {
        Some(review) => {
            // New files are changes too, before git tracks them.
            let files = review.files + review.untracked.len();
            let files_label = match files {
                0 => "Sin cambios".to_owned(),
                1 => "1 archivo".to_owned(),
                files => format!("{files} archivos"),
            };
            let (added, removed) = (
                format!("+{}", review.additions),
                format!("\u{2212}{}", review.deletions),
            );
            figures = Some(format!(
                "{files} archivos · {added} · {removed}{}",
                if review.truncated {
                    " · Vista parcial"
                } else {
                    ""
                }
            ));
            let fits = advance(&files_label) + advance(&added) + advance(&removed)
                + 2.0 * SUMMARY_GAP
                <= room;
            summary.push(text(&files_label, theme::FOREGROUND).shrink_to_fit());
            if files > 0 && fits {
                summary.push(text(&added, theme::DIFF_ADDED));
                summary.push(text(&removed, theme::DIFF_REMOVED));
            }
        }
        None if reading => {
            summary.push(
                Element::spinner(
                    "environment_repo_progress",
                    theme::FOREGROUND_SUBTLE,
                    SpinnerVariant::Circle,
                )
                .size(Some(12.0), Some(12.0))
                .centered_y(),
            );
            summary.push(text("Actualizando repositorio…", theme::FOREGROUND_SUBTLE).shrink_to_fit());
        }
        None => summary.push(
            text(
                if outside {
                    "Sin repositorio de Git"
                } else {
                    "Ver cambios"
                },
                theme::FOREGROUND_SUBTLE,
            )
            .shrink_to_fit(),
        ),
    }
    let pill = |target: Element| target.padding(FOOT_PAD).size(None, Some(26.0)).centered_y();
    let mut changes = Tooltip::new(
        pill(palette.interactive_surface(Element::clickable(
            "dock_open:review",
            Element::row(summary).gap(SUMMARY_GAP),
        ))),
        "dock_open:review",
        "Abrir cambios del repositorio",
        theme::component_palette(),
    );
    if let Some(figures) = figures {
        changes = changes.description(figures);
    }
    let mut foot = vec![changes.render().shrink_to_fit(), Element::spacer().flex(1.0)];
    if commit {
        let ink = if locked {
            theme::FOREGROUND_SUBTLE
        } else {
            theme::FOREGROUND
        };
        let content = Element::row(vec![
            text("Commit y push", ink),
            Element::icon(Icon::ArrowRight)
                .foreground(ink)
                .size(Some(12.0), Some(12.0))
                .centered_y(),
        ])
        .gap(4.0);
        // While it is off it says why, and takes no pointer.
        let (target, help) = if locked {
            (Element::panel(content), LOCKED)
        } else {
            (
                palette.interactive_surface(Element::clickable("environment_git_open_commit", content)),
                "Revisar archivos preparados, crear un commit y publicar la rama",
            )
        };
        foot.push(
            Tooltip::new(
                pill(target),
                "environment_git_open_commit",
                help,
                theme::component_palette(),
            )
            .render(),
        );
    }
    Element::row(foot)
}

/// The subagents the selected conversation started, each with whether it can
/// still be stopped.
pub(crate) fn subagents(
    state: &ViewState,
) -> impl Iterator<Item = (&crate::view::SubagentSessionView, bool)> {
    let selected = state.selected_session.as_deref();
    state
        .subagent_sessions
        .iter()
        .filter(move |run| selected == Some(run.parent_session_id.as_str()))
        .map(|run| {
            let live = state
                .active_subagents
                .iter()
                .any(|live| live.child_session_id == run.child_session_id);
            (run, crate::session_surface::subagent_cancellable(run, live))
        })
}

/// The newest command of this conversation that serves a preview, as the
/// browser's default address is chosen.
pub(crate) fn preview(state: &ViewState) -> Option<&crate::terminal_dock_ui::AgentTerminalView> {
    state
        .terminal_controls
        .agents
        .iter()
        .filter(|agent| agent.preview.is_some())
        .max_by_key(|agent| agent.id)
}

/// A block's title with a figure beside it: how many of what it lists are
/// still going, or how many there are once none is.
fn tallied(title: &str, going: usize, total: usize) -> Element {
    let figure = if going > 0 {
        format!("{going} en curso")
    } else {
        total.to_string()
    };
    Element::row(vec![
        label(title, theme::FOREGROUND).flex(1.0),
        theme::text_px(figure, 10.5, theme::FOREGROUND_SUBTLE).centered_y(),
    ])
    .gap(8.0)
}

/// The plan the selected conversation is following, if it has one.
fn plan(state: &ViewState) -> Option<&openide_core::PlanSnapshot> {
    let plan = state.session_plans.get(state.selected_session.as_ref()?)?;
    (!plan.snapshot.plan.is_empty()).then_some(&plan.snapshot)
}

/// What "Progreso" lists: the steps of the plan, each with how it stands.
fn progress_rows(plan: &openide_core::PlanSnapshot) -> Vec<Element> {
    plan.plan
        .iter()
        .map(|step| {
            let going = step.status == openide_core::PlanStepStatus::InProgress;
            Element::row(vec![
                crate::session_surface::plan_step_marker(step.status).centered_y(),
                label(&step.step, if going { theme::FOREGROUND } else { theme::FOREGROUND_SUBTLE }).flex(1.0),
            ])
            .gap(8.0)
            .min_size(0.0, 24.0)
        })
        .collect()
}

/// What "Subagentes" lists: each one's task and state, which opens its
/// conversation, and the way to stop those still going.
fn subagent_rows(state: &ViewState, width: f32) -> Vec<Element> {
    let mut going = 0;
    // Those still at work first, then the ones that ended, each under its count.
    let (mut active, mut done): (Vec<_>, Vec<_>) = subagents(state).partition(|(run, stoppable)| {
        *stoppable
            || !matches!(
                run.status,
                openide_harness::SubagentStatus::Completed
                    | openide_harness::SubagentStatus::Failed
                    | openide_harness::SubagentStatus::Cancelled
            )
    });
    let heading = |title: &str, count: usize| {
        theme::text_px(format!("{title} · {count}"), 10.5, theme::FOREGROUND_SUBTLE).size(None, Some(18.0))
    };
    let (active_count, done_count) = (active.len(), done.len());
    let mut rows: Vec<Element> = Vec::new();
    if active_count > 0 && done_count > 0 {
        rows.push(heading("Activos", active_count));
    }
    let ended_heading = (done_count > 0 && active_count > 0).then(|| heading("Terminados", done_count));
    let listed: Vec<Element> = active
        .drain(..)
        .map(|entry| (entry, false))
        .chain(done.drain(..).map(|entry| (entry, true)))
        .enumerate()
        .flat_map(|(index, ((run, stoppable), ended))| {
            going += usize::from(stoppable);
            let before = (ended && index == active_count).then(|| ended_heading.clone()).flatten();
            let row = {
            let status = if state.pause.sessions.contains_key(&run.child_session_id) {
                "En pausa"
            } else {
                crate::session_surface::subagent_status_label(run.status)
            };
            let detail = if run.cancellation_requested {
                format!("{status} · Cancelación solicitada")
            } else if let Some(answer) = run.final_response.as_deref().filter(|_| ended) {
                // What it answered, in a line; its conversation has the rest.
                let line: String = answer.lines().next().unwrap_or("").chars().take(120).collect();
                format!("{status} · {line}")
            } else {
                status.to_owned()
            };
            // A target of its own: the conversation's card has the same action.
            let id = format!("environment_subagent:{}", run.child_session_id);
            let row = ListRow::new(&id, &run.title, block_palette())
                .icon(Icon::Users)
                .subtitle(detail)
                .trailing_icon(Icon::ChevronRight)
                .width(width)
                .sizing(crate::ui_sizing::ENVIRONMENT_ACTION)
                .render()
                .dispatch_as(format!("subagent_open:{}", run.child_session_id));
            Tooltip::new(row, id, "Abrir subagente", theme::component_palette())
                .description(match run.final_response.as_deref().filter(|_| ended) {
                    Some(answer) => format!("{}\n\nRespuesta final:\n{answer}", run.title),
                    None => run.title.clone(),
                })
                .max_width(360.0)
                .render()
            };
            before.into_iter().chain([row])
        })
        .collect();
    rows.extend(listed);
    if going > 0 {
        rows.push(Element::row(vec![
            Button::new("environment_subagents_stop", "Detener todos", block_button_palette())
                .icon(Icon::StopFill)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .tooltip("Cancelar los subagentes en curso de esta conversación")
                .render(),
            Element::spacer().flex(1.0),
        ]));
    }
    rows
}

/// What "Terminales" lists: the commands the agent started here, each with
/// how it stands. A row shows its tab; a running one can be stopped, which
/// asks first as closing its tab does.
fn terminal_rows(state: &ViewState, width: f32) -> Vec<Element> {
    state
        .terminal_controls
        .agents
        .iter()
        .map(|agent| {
            let command = state
                .terminal_tabs
                .iter()
                .find(|tab| tab.id == agent.id)
                .map_or("Comando", |tab| tab.title.as_str());
            let row = ListRow::new(
                format!("agent_terminal_show:{}", agent.command_id),
                command,
                block_palette(),
            )
            .icon(Icon::Terminal)
            .subtitle(agent.ended.as_deref().unwrap_or("En ejecución"))
            .tooltip_with(
                TooltipSpec::new("Ver en terminal")
                    .description(command)
                    .palette(theme::component_palette()),
            )
            .sizing(crate::ui_sizing::ENVIRONMENT_ACTION)
            .render()
            .flex(1.0);
            let mut row = vec![row];
            if agent.ended.is_none() {
                row.push(
                    // The composer's stop: a solid mark on a white disc.
                    IconButton::new(
                        format!("environment_terminal_stop:{}", agent.id),
                        Icon::StopFill,
                        crate::composer_ui::round_action_palette(true),
                    )
                    .variant(ButtonVariant::Subtle)
                    .size(ControlSize::Dense)
                    .round()
                    .tooltip_with(TooltipSpec::new("Detener").placement(TooltipPlacement::Above))
                    .dispatch_as(format!("terminal_close:{}", agent.id))
                    .render()
                    .centered_y(),
                );
            }
            Element::row(row).gap(2.0).size(Some(width), None)
        })
        .collect()
}

/// What "Fuentes y herramientas" lists: the chat's changes, its sources and
/// the tools of the conversation.
fn details(state: &ViewState, width: f32) -> Vec<Element> {
    let changes = &state.environment.changes;
    let changes_detail = if changes.loading {
        "Actualizando…".into()
    } else if changes.available {
        format!(
            "{} archivos{}",
            changes.files,
            if changes.partial { " · Parcial" } else { "" }
        )
    } else {
        "Dato no disponible".into()
    };
    // What the chat made, what it was given and what it read, in the order
    // a summary of it is read; then the tools it works with.
    let mut made = vec![theme::text_px(&changes_detail, 10.5, theme::FOREGROUND_SUBTLE).centered_y()];
    if changes.available {
        made.push(counts(changes.additions, changes.deletions).centered_y());
    }
    let mut rows = vec![
        section_heading("Resultados", None, width),
        action_row(
            "environment_changes_open",
            Icon::Code,
            "Cambios del chat",
            "",
            Some(Element::row(made).gap(6.0)),
            &changes_help_summary(changes),
            width,
        ),
        sources(state, width),
        section_heading("Herramientas", None, width),
    ];
    for (id, icon, title, kind) in [
        ("terminal", Icon::Terminal, "Terminal", SideTab::Terminal),
        ("browser", Icon::Globe, "Navegador", SideTab::Browser),
        ("files", Icon::Folder, "Archivos", SideTab::Files),
        ("activity", Icon::History, "Actividad", SideTab::Activity),
    ] {
        let status = tab_status(state, kind);
        rows.push(action_row(
            &format!("dock_open:{id}"),
            icon,
            title,
            "",
            Some(theme::text_px(&status, 10.5, theme::FOREGROUND_SUBTLE).centered_y()),
            &format!("Abrir {title} de esta conversación · {status}"),
            width,
        ));
    }
    rows
}

/// The overview, straight on the panel's surface as the context summary
/// stands on its popover's. Its header and its foot stay; the rows between
/// them scroll once the card would be taller than `height`.
fn card(state: &ViewState, width: f32, height: f32) -> Element {
    use crate::environment_git_ui::EnvironmentGitPanel;
    let palette = card_palette();
    let workflow = &state.environment.workflow;
    let locked = locked(state);
    // A row's fill, and what an open block lists inside it.
    let fill_width = (width - 2.0 * (FramedCard::INSET - FramedCard::REACH)).max(120.0);
    let list_width = fill_width - 2.0 * LIST_INSET;
    // A menu is as wide as the row it opens from.
    let mut props = ComposerProps::from_state(state, fill_width + 16.0, false, None, "", None);
    props.project_menu_open =
        state.composer_project_menu_open && state.environment.menu == EnvironmentMenu::Project;
    props.branch_menu_open =
        state.composer_branches.open && state.environment.menu == EnvironmentMenu::Branch;
    let branch = state
        .project_branch
        .as_deref()
        .filter(|branch| !branch.trim().is_empty())
        .unwrap_or("Sin rama");
    let branch_detail = state
        .composer_branches
        .switching
        .as_deref()
        .or(state.composer_branches.blocked_reason)
        .unwrap_or("Cambiar rama de trabajo");
    let path = if state.workspace.is_empty() {
        "Elegir carpeta"
    } else {
        &state.workspace
    };
    let project = project_selector(
        &props,
        card_row(
            "environment_project_toggle",
            Icon::Folder,
            Element::column(vec![
                label(folder_name(&state.workspace), theme::FOREGROUND).size(None, Some(18.0)),
                Element::truncated_text(path)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0)
                    .foreground(theme::FOREGROUND_SUBTLE)
                    .size(None, Some(16.0)),
            ]),
            Icon::ChevronDown,
            path,
            (!locked).then_some("environment_project_toggle"),
        )
        .size(None, Some(PROJECT_ROW_HEIGHT)),
        PopoverPlacement::BottomStart,
        "environment_project_toggle",
    );
    let switching = state.composer_branches.switching();
    let branch = branch_selector(
        &props,
        card_row(
            "environment_branch_toggle",
            Icon::GitBranch,
            label(
                branch,
                if switching {
                    theme::FOREGROUND_SUBTLE
                } else {
                    theme::FOREGROUND
                },
            ),
            Icon::ChevronDown,
            branch_detail,
            (!switching).then_some("environment_branch_toggle"),
        ),
        PopoverPlacement::BottomStart,
        "environment_branch_toggle",
    );
    let compare = card_row(
        "environment_git_open_compare",
        Icon::Code,
        label(
            "Comparar cambios",
            if locked {
                theme::FOREGROUND_SUBTLE
            } else {
                theme::FOREGROUND
            },
        ),
        Icon::ChevronRight,
        if locked {
            LOCKED
        } else {
            "Elegir una rama base y comparar los commits confirmados"
        },
        (!locked).then_some("environment_git_open_compare"),
    );
    let actions_open = workflow.panel == EnvironmentGitPanel::Actions;
    let actions = FramedCard::block(
        "environment_actions",
        card_row(
            "project_actions_open",
            Icon::Play,
            label("Acciones del proyecto", theme::FOREGROUND),
            chevron(actions_open),
            "Configurar y ejecutar tareas del proyecto",
            // The open row closes its block, as the back button of its panel did.
            Some(if actions_open {
                "environment_git_back"
            } else {
                "project_actions_open"
            }),
        ),
        actions_open.then(|| {
            listed(vec![crate::project_actions_ui::build(
                &state.project_actions,
                list_width,
            )])
        }),
        palette,
    );
    // What the conversation has going is listed only while there is any.
    let mut running = Vec::new();
    if let Some(plan) = plan(state) {
        let open = state.environment.progress_expanded;
        let done = plan
            .plan
            .iter()
            .filter(|step| step.status == openide_core::PlanStepStatus::Completed)
            .count();
        running.push(FramedCard::block(
            "environment_progress",
            card_row(
                "environment_progress_toggle",
                Icon::Plan,
                Element::row(vec![
                    label("Progreso", theme::FOREGROUND).flex(1.0),
                    theme::text_px(format!("{done} de {}", plan.plan.len()), 10.5, theme::FOREGROUND_SUBTLE)
                        .centered_y(),
                ])
                .gap(8.0),
                chevron(open),
                "Pasos del plan de esta conversación",
                Some("environment_progress_toggle"),
            ),
            open.then(|| listed(progress_rows(plan))),
            palette,
        ));
    }
    let (going, total) = subagents(state).fold((0, 0), |(going, total), (_, stoppable)| {
        (going + usize::from(stoppable), total + 1)
    });
    if total > 0 {
        let open = state.environment.subagents_expanded;
        running.push(FramedCard::block(
            "environment_subagents",
            card_row(
                "environment_subagents_toggle",
                Icon::Users,
                tallied("Subagentes", going, total),
                chevron(open),
                "Subagentes que inició esta conversación",
                Some("environment_subagents_toggle"),
            ),
            open.then(|| listed(subagent_rows(state, list_width))),
            palette,
        ));
    }
    let agents = &state.terminal_controls.agents;
    if !agents.is_empty() {
        let open = state.environment.terminals_expanded;
        let going = agents.iter().filter(|agent| agent.ended.is_none()).count();
        running.push(FramedCard::block(
            "environment_terminals",
            card_row(
                "environment_terminals_toggle",
                Icon::Terminal,
                tallied("Terminales", going, agents.len()),
                chevron(open),
                "Comandos que el agente inició en esta conversación",
                Some("environment_terminals_toggle"),
            ),
            open.then(|| listed(terminal_rows(state, list_width))),
            palette,
        ));
    }
    if let Some((agent, url)) = preview(state).and_then(|agent| Some((agent, agent.preview.as_deref()?))) {
        running.push(card_row(
            "environment_preview",
            Icon::Globe,
            Element::row(vec![
                label("Vista previa", theme::FOREGROUND).shrink_to_fit(),
                Element::truncated_text(url)
                    .font_family(FontFamily::Sans)
                    .font_size(10.5)
                    .foreground(theme::FOREGROUND_SUBTLE)
                    .flex(1.0)
                    .centered_y(),
            ])
            .gap(8.0),
            Icon::ChevronRight,
            &format!("Abrir en el navegador integrado\n{url}"),
            Some(&format!("agent_preview_open:{}", agent.command_id)),
        ));
    }
    let details_open = state.environment.details_expanded;
    let details = FramedCard::block(
        "environment_details",
        card_row(
            "environment_details_toggle",
            Icon::Book,
            label("Fuentes y herramientas", theme::FOREGROUND),
            chevron(details_open),
            "Archivos consultados, referencias y herramientas de esta conversación",
            Some("environment_details_toggle"),
        ),
        details_open.then(|| listed(details(state, list_width))),
        palette,
    );
    let mut rows = vec![
        project,
        local_menu(state, fill_width, locked),
        branch,
        // With the gap on each side of it, what parts the selectors from the rest.
        Element::spacer().size(None, Some(2.0)),
        compare,
        actions,
    ];
    rows.extend(running);
    rows.push(details);
    // The foot already says the repository is being read for the first time.
    let said = workflow.loading && workflow.busy.is_none() && !matches!(state.git_review, Some(Ok(_)));
    let notices: Vec<_> = crate::environment_git_ui::notices(workflow)
        .into_iter()
        .skip(usize::from(said))
        .map(inset)
        .collect();
    if !notices.is_empty() {
        rows.push(Element::spacer().size(None, Some(2.0)));
        rows.extend(notices);
        rows.push(Element::spacer().size(None, Some(2.0)));
    }
    // The rail keeps to the room a row leaves beside its content.
    let scroll = |content: Element| {
        ScrollArea::new("chat_environment", content)
            .appearance(
                ScrollbarAppearance::overlay()
                    .rail_width(FramedCard::REACH)
                    .thumb_width(4.0)
                    .buttons(false)
                    .tint(theme::FOREGROUND_SUBTLE),
            )
            .render()
    };
    let (body, foot) = FramedCard::new(palette)
        .flat()
        .width(width)
        .rows(Element::column(rows).gap(FramedCard::ROW_GAP))
        .foot(foot(state, width, locked))
        .into_parts();
    // With no room for two rows between them, the header and the foot scroll
    // with the rows instead of staying.
    let pinned = height - CARD_CHROME - BODY_MARGINS >= PROJECT_ROW_HEIGHT + FramedCard::ROW_HEIGHT;
    // limn gives a flat card's body and foot apart, not its head, so the
    // header keeps the card's inset and top margin here.
    let side = || Element::spacer().size(Some(FramedCard::INSET), None);
    let header = Element::column(vec![
        Element::spacer().size(None, Some(8.0)),
        Element::row(vec![side(), card_header(workflow.loading).flex(1.0), side()]),
    ]);
    // The context summary's parts: the foot under the body, with its hairline.
    let mut card = PopoverPanel::new(theme::component_palette())
        .header(header)
        .body(if pinned { scroll(body) } else { body })
        .footer(Element::column(foot.into_iter().collect()))
        .sizing(crate::ui_sizing::CONTEXT_POPOVER);
    if pinned {
        card = card.body_max_height(height - CARD_CHROME);
    }
    let card = card.render().size(Some(width), None);
    if pinned {
        card
    } else {
        scroll(card).max_height(height.max(0.0))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::environment_git_ui::EnvironmentGitPanel;
    use openide_ui::{
        DrawCommand, Frame, HitKind, Key, Point, Rect, Size, UiAction, UiEvent, UiTree,
    };
    use std::time::{Duration, Instant};

    const SIZE: Size = Size::new(420.0, 1200.0);
    /// The panel's border, which its content stands inside.
    const EDGE: f32 = 1.0;

    fn repository(state: &mut ViewState) {
        state.git_review = Some(Ok(crate::git_review::GitReview {
            additions: 25813,
            deletions: 8475,
            files: 17,
            ..Default::default()
        }
        .into()));
    }

    /// A conversation with two subagents, two commands its agent started and
    /// a preview one of them serves; another conversation's subagent beside them.
    pub(crate) fn going(state: &mut ViewState) {
        use openide_harness::SubagentStatus;
        let selected = state.selected_session.get_or_insert_with(|| "s1".into()).clone();
        for (child, parent, title, status) in [
            ("child-1", selected.as_str(), "Actualizar pruebas", SubagentStatus::Running),
            ("child-2", selected.as_str(), "Revisar el diff", SubagentStatus::Completed),
            ("other", "another", "De otra conversación", SubagentStatus::Running),
        ] {
            state.subagent_sessions.push(crate::view::SubagentSessionView {
                child_session_id: child.into(),
                parent_session_id: parent.into(),
                title: title.into(),
                status,
                cancellation_requested: false,
                open_inputs: 0,
                inputs: vec![],
                dispatch_error: None,
                depth: 1,
                final_response: None,
            });
        }
        for (id, command, ended, preview) in [
            (7, "npm run dev", None, Some("http://localhost:5173/")),
            (8, "cargo test", Some("Proceso finalizado · código 0"), None),
        ] {
            state.terminal_tabs.push(crate::view::DockTabView {
                id,
                title: command.into(),
                full_path: None,
                active: id == 7,
                kind: SideTab::Terminal,
                closable: true,
            });
            state.terminal_controls.agents.push(crate::terminal_dock_ui::AgentTerminalView {
                id,
                command_id: format!("cmd-{id}"),
                task: "Tarea".into(),
                read_only: false,
                ended: ended.map(str::to_owned),
                preview: preview.map(str::to_owned),
            });
        }
    }

    fn saved_action(state: &mut ViewState) {
        state.project_actions.actions = vec![crate::project_actions::ProjectAction {
            id: "check".into(),
            label: "Verificar proyecto".into(),
            command: "cargo check --workspace".into(),
        }];
    }

    /// The panel as its host lays it out: at its own height, with room left over.
    fn tree(state: &ViewState, width: f32, height: f32) -> UiTree {
        let mut tree = UiTree::new(Element::row(vec![
            Element::column(vec![panel(state, width, height), Element::spacer().flex(1.0)]),
            Element::spacer().flex(1.0),
        ]));
        tree.set_reduced_motion(true);
        tree
    }

    fn frame(state: &ViewState, width: f32, height: f32) -> Frame {
        tree(state, width, height).layout(SIZE)
    }

    fn button(frame: &Frame, id: &str) -> Option<Rect> {
        frame
            .regions
            .iter()
            .find(|region| region.id == id && region.kind == HitKind::Button)
            .map(|region| region.rect)
    }

    fn texts(frame: &Frame) -> Vec<&str> {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn fills(frame: &Frame, wanted: Color) -> Vec<Rect> {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRect { rect, color, .. } if *color == wanted => Some(*rect),
                _ => None,
            })
            .collect()
    }

    /// The panel's own fill: the first thing it paints.
    fn surface(frame: &Frame) -> Rect {
        fills(frame, card_palette().raised)[0]
    }

    fn center(rect: Rect) -> Point {
        Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
    }

    fn holds(outer: Rect, inner: Rect) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.x + inner.width <= outer.x + outer.width
            && inner.y + inner.height <= outer.y + outer.height
    }

    #[test]
    fn the_card_keeps_every_action_between_its_header_and_its_foot() {
        let mut state = crate::view::tests::empty_state();
        repository(&mut state);
        let frame = frame(&state, 380.0, 344.0);
        for id in [
            "environment_git_refresh",
            "environment_dismiss",
            "environment_project_toggle",
            "environment_local_toggle",
            "environment_branch_toggle",
            "environment_git_open_compare",
            "project_actions_open",
            "environment_details_toggle",
            "dock_open:review",
            "environment_git_open_commit",
        ] {
            let rect = button(&frame, id).unwrap_or_else(|| panic!("missing {id}"));
            assert!(
                holds(Rect::new(0.0, 0.0, 380.0, 344.0), rect),
                "{id}: {rect:?}"
            );
        }
        let labels = texts(&frame);
        for label in [
            "Entorno",
            "Local",
            "Comparar cambios",
            "Acciones del proyecto",
            "Fuentes y herramientas",
            "17 archivos",
            "+25813",
            "−8475",
            "Commit y push",
        ] {
            assert!(labels.contains(&label), "{label} in {labels:?}");
        }
        assert!(!labels.contains(&"Control de versiones"));
        // The changes and the commit share the foot, under every row.
        let rect = |id: &str| button(&frame, id).unwrap();
        let (changes, commit) = (rect("dock_open:review"), rect("environment_git_open_commit"));
        let last = rect("environment_details_toggle");
        assert!(changes.y > last.y + last.height, "{changes:?} {last:?}");
        assert_eq!(changes.y, commit.y);
        assert!(changes.x + changes.width <= commit.x);
        // A closed section lists nothing.
        assert!(button(&frame, "environment_changes_open").is_none());
        assert!(button(&frame, "project_action_new").is_none());
    }

    #[test]
    fn local_menu_keeps_unique_actions_when_details_are_open() {
        let mut state = crate::view::tests::empty_state();
        state.environment.details_expanded = true;
        state.environment.menu = EnvironmentMenu::Local;
        let frame = frame(&state, 380.0, 1000.0);
        let mut ids = HashSet::new();
        for region in frame
            .regions
            .iter()
            .filter(|region| region.kind == HitKind::Button)
        {
            assert!(ids.insert(&region.id), "duplicate button {}", region.id);
        }
        assert!(button(&frame, "environment_choose_project").is_some());
        assert!(button(&frame, "environment_local_files").is_some());
    }

    #[test]
    fn every_action_of_the_card_answers_the_pointer_and_the_keyboard() {
        let mut state = crate::view::tests::empty_state();
        repository(&mut state);
        saved_action(&mut state);
        state.environment.details_expanded = true;
        // Without sources the list says so.
        assert!(texts(&frame(&state, 380.0, 1100.0)).contains(&"Aún no hay fuentes."));
        state.transcript = vec![
            TranscriptItem::Tool {
                id: "read".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "src/main.rs"}),
                output: Some("fn main() {}".into()),
                is_error: false,
                activity: ActivityStatus::Completed,
            },
            TranscriptItem::WebCitation {
                title: "Referencia".into(),
                url: "https://example.com/referencia".into(),
            },
        ];
        let check = |state: &ViewState, id: &str, action: &str| {
            let mut tree = tree(state, 380.0, 1100.0);
            let frame = tree.layout(SIZE);
            let point = center(button(&frame, id).unwrap_or_else(|| panic!("missing {id}")));
            let clicked = vec![UiAction::Clicked { id: action.into() }];
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(tree.handle_event(UiEvent::PointerUp(point)), clicked, "{id}");
            assert!(tree.focus(id), "{id} takes no focus");
            let enter = UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false,
            };
            assert_eq!(tree.handle_event(enter), clicked, "{id}");
        };
        for id in [
            "environment_git_refresh",
            "environment_dismiss",
            "environment_project_toggle",
            "environment_local_toggle",
            "environment_branch_toggle",
            "environment_git_open_compare",
            "project_actions_open",
            "environment_details_toggle",
            "environment_changes_open",
            "environment_source_file:0",
            "web_citation_open:1",
            "dock_open:terminal",
            "dock_open:browser",
            "dock_open:files",
            "dock_open:activity",
            "dock_open:review",
            "environment_git_open_commit",
        ] {
            check(&state, id, id);
        }
        // Open, the project actions list their controls, and their row closes
        // them with the action their panel's back button had.
        state.environment.workflow.panel = EnvironmentGitPanel::Actions;
        check(&state, "project_actions_open", "environment_git_back");
        for id in [
            "project_action_run:check",
            "project_action_edit:check",
            "project_action_delete:check",
            "project_action_new",
            "environment_details_toggle",
            "environment_git_open_commit",
        ] {
            check(&state, id, id);
        }
        // An empty list offers its first action once, and an open editor none.
        state.project_actions.actions.clear();
        check(&state, "project_action_new", "project_action_new");
        state.project_actions.begin_new();
        let frame = frame(&state, 380.0, 1100.0);
        assert!(button(&frame, "project_action_new").is_none());
        for id in ["project_action_label", "project_action_command", "project_action_cancel"] {
            assert!(frame.regions.iter().any(|region| region.id == id), "{id}");
        }
    }

    #[test]
    fn a_rows_hover_fill_reaches_past_its_content_and_keeps_off_the_cards_edge() {
        let state = crate::view::tests::empty_state();
        let hover = card_palette().hover;
        // From the panel's edge, inside its border.
        let (inset, reach) = (EDGE + FramedCard::INSET, EDGE + FramedCard::INSET - FramedCard::REACH);
        for width in [280.0, 380.0] {
            let mut tree = tree(&state, width, 344.0);
            let frame = tree.layout(SIZE);
            assert!(fills(&frame, hover).is_empty());
            for id in [
                "environment_project_toggle",
                "environment_local_toggle",
                "environment_branch_toggle",
                "environment_git_open_compare",
                "project_actions_open",
                "environment_details_toggle",
            ] {
                let content = button(&frame, id).unwrap();
                assert_eq!((content.x, content.x + content.width), (inset, width - inset), "{id}");
                tree.handle_event(UiEvent::PointerMove(center(content)));
                let fill = fills(&tree.layout(SIZE), hover);
                assert_eq!(fill.len(), 1, "{id}: {fill:?}");
                let fill = fill[0];
                assert_eq!((fill.x, fill.x + fill.width), (reach, width - reach), "{id}");
                assert_eq!((fill.y, fill.height), (content.y, content.height), "{id}");
                assert!(fill.x > EDGE);
            }
        }
    }

    #[test]
    fn an_open_section_and_what_it_lists_are_one_block() {
        let mut state = crate::view::tests::empty_state();
        saved_action(&mut state);
        let hover = card_palette().hover;
        let width = 320.0;
        assert!(fills(&frame(&state, width, 1100.0), hover).is_empty());
        state.environment.details_expanded = true;
        state.environment.workflow.panel = EnvironmentGitPanel::Actions;
        let frame = frame(&state, width, 1100.0);
        let rect = |id: &str| button(&frame, id).unwrap_or_else(|| panic!("missing {id}"));
        let mut blocks = fills(&frame, hover);
        blocks.sort_by(|a, b| a.y.total_cmp(&b.y));
        assert_eq!(blocks.len(), 2, "{blocks:?}");
        for (block, row, listed) in [
            (
                blocks[0],
                "project_actions_open",
                ["project_action_run:check", "project_action_new"],
            ),
            (
                blocks[1],
                "environment_details_toggle",
                ["environment_changes_open", "dock_open:activity"],
            ),
        ] {
            let reach = EDGE + FramedCard::INSET - FramedCard::REACH;
            assert_eq!((block.x, block.x + block.width), (reach, width - reach), "{row}");
            // The row is the top of the block, and what it lists is inside it.
            assert_eq!(rect(row).y, block.y, "{row}");
            for id in listed {
                assert!(holds(block, rect(id)), "{id} {:?} in {block:?}", rect(id));
            }
        }
        // Two fills never touch.
        assert!(blocks[0].y + blocks[0].height + FramedCard::ROW_GAP <= blocks[1].y);
        let above = rect("environment_git_open_compare");
        assert!(above.y + above.height + FramedCard::ROW_GAP <= blocks[0].y);
    }

    #[test]
    fn a_section_opens_and_closes_in_place_and_the_panel_follows_it() {
        let mut state = crate::view::tests::empty_state();
        let root = |state: &ViewState| {
            Element::row(vec![
                Element::column(vec![panel(state, 320.0, 1100.0), Element::spacer().flex(1.0)]),
                Element::spacer().flex(1.0),
            ])
        };
        let height = |frame: &Frame| surface(frame).height;
        let listed = |frame: &Frame| button(frame, "dock_open:activity").is_some();
        // A slot opens and closes on a spring that all but arrives in one and
        // a half moves; it is at rest well within three times that.
        let settle = Duration::from_millis(u64::from(openide_ui::tokens().motion.move_ms) * 3 / 2);
        let (enter, exit) = (settle, settle);
        let start = Instant::now() - Duration::from_secs(10);
        let mut tree = UiTree::new(root(&state));
        let closed = height(&tree.layout_at(SIZE, start));

        state.environment.details_expanded = true;
        tree.set_root(root(&state));
        // The frame it opens in has not grown: the block opens from its row.
        assert_eq!(height(&tree.layout_at(SIZE, start)), closed);
        let opening = height(&tree.layout_at(SIZE, start + enter / 2));
        let end = start + enter * 3;
        let frame = tree.layout_at(SIZE, end);
        let open = height(&frame);
        assert!(closed < opening && opening < open, "{closed} {opening} {open}");
        assert!(listed(&frame));

        state.environment.details_expanded = false;
        tree.set_root(root(&state));
        assert_eq!(height(&tree.layout_at(SIZE, end)), open);
        let frame = tree.layout_at(SIZE, end + exit / 2);
        let closing = height(&frame);
        assert!(closed < closing && closing < open, "{closed} {closing} {open}");
        // What it listed is paint only while it closes.
        assert!(!listed(&frame));
        assert_eq!(height(&tree.layout_at(SIZE, end + exit * 3)), closed);
    }

    #[test]
    fn the_foot_says_when_there_is_no_repository_or_it_is_being_read() {
        let mut state = crate::view::tests::empty_state();
        let count = |frame: &Frame, wanted: &str| {
            texts(frame).iter().filter(|text| **text == wanted).count()
        };
        let figure = |frame: &Frame| {
            texts(frame)
                .iter()
                .any(|text| text.starts_with('+') || text.starts_with('−'))
        };
        // Not read yet: no figure, and the review and the commit stay reachable.
        let unread = frame(&state, 320.0, 344.0);
        assert_eq!(count(&unread, "Ver cambios"), 1);
        assert!(button(&unread, "environment_git_open_commit").is_some());
        assert!(!figure(&unread));

        state.environment.workflow.loading = true;
        let reading = frame(&state, 320.0, 344.0);
        assert_eq!(count(&reading, "Actualizando repositorio…"), 1);
        assert_eq!(count(&reading, "Commit y push"), 0);
        assert!(!reading.regions.iter().any(|region| region.id == "environment_git_open_commit"));
        assert!(button(&reading, "environment_git_refresh").is_none());
        assert!(button(&reading, "dock_open:review").is_some());
        assert!(!figure(&reading));

        state.environment.workflow.loading = false;
        state.git_review = Some(Err(crate::git_review::NOT_A_REPOSITORY.into()));
        let outside = frame(&state, 320.0, 344.0);
        assert_eq!(count(&outside, "Sin repositorio de Git"), 1);
        assert_eq!(count(&outside, "Commit y push"), 0);
        assert!(!outside.regions.iter().any(|region| region.id == "environment_git_open_commit"));
        assert!(button(&outside, "dock_open:review").is_some());
        assert!(button(&outside, "environment_git_refresh").is_some());
        assert!(!figure(&outside));

        // A review that failed for another reason does not say there is no repository.
        state.git_review = Some(Err("fatal: index file corrupt".into()));
        let failed = frame(&state, 320.0, 344.0);
        assert_eq!(count(&failed, "Ver cambios"), 1);
        assert!(button(&failed, "environment_git_open_commit").is_some());

        // Figures already read stay while the repository is read again.
        repository(&mut state);
        state.environment.workflow.loading = true;
        let refreshing = frame(&state, 380.0, 344.0);
        assert_eq!(count(&refreshing, "17 archivos"), 1);
        assert_eq!(count(&refreshing, "Actualizando repositorio…"), 1);
        assert!(button(&refreshing, "environment_git_open_commit").is_some());

        // What Git answered stands in the card, and an operation says its name.
        state.environment.workflow.loading = false;
        state.environment.workflow.busy = Some("Enviando commits…".into());
        state.environment.workflow.error = Some("No se pudo publicar la rama".into());
        let failed = frame(&state, 320.0, 344.0);
        assert_eq!(count(&failed, "Enviando commits…"), 1);
        assert_eq!(count(&failed, "No se pudo publicar la rama"), 1);
    }

    #[test]
    fn the_counts_leave_the_foot_whole_when_they_do_not_fit_beside_the_commit() {
        // In the app's own face, which the fit is read off.
        let foot = |files: usize, additions: usize, deletions: usize, width: f32| {
            let mut state = crate::view::tests::empty_state();
            state.git_review = Some(Ok(crate::git_review::GitReview {
                additions,
                deletions,
                files,
                ..Default::default()
            }
            .into()));
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(Element::row(vec![
                Element::column(vec![panel(&state, width, 344.0), Element::spacer().flex(1.0)]),
                Element::spacer().flex(1.0),
            ]));
            let frame = tree.layout(SIZE);
            context.end_pass().textures_delta.clear();
            let rect = |id: &str| button(&frame, id).unwrap();
            let (changes, commit) = (rect("dock_open:review"), rect("environment_git_open_commit"));
            assert!(changes.x >= EDGE + FramedCard::INSET && changes.x + changes.width <= commit.x);
            assert_eq!(commit.x + commit.width, width - EDGE - FramedCard::INSET);
            assert!(commit.width <= COMMIT_WIDTH, "{commit:?}");
            texts(&frame).into_iter().map(str::to_owned).collect::<Vec<_>>()
        };
        let has = |labels: &[String], wanted: &str| labels.iter().any(|label| label == wanted);
        let narrow = foot(8, 248, 31, 280.0);
        for label in ["8 archivos", "+248", "−31", "Commit y push"] {
            assert!(has(&narrow, label), "{label} in {narrow:?}");
        }
        // Counts that would be cut are left to the hint; the files stay whole.
        let narrow = foot(68, 25813, 8475, 280.0);
        assert!(has(&narrow, "68 archivos") && has(&narrow, "Commit y push"), "{narrow:?}");
        assert!(!narrow.iter().any(|text| text.starts_with('+') || text.starts_with('−')));
        let wide = foot(68, 25813, 8475, 320.0);
        for label in ["68 archivos", "+25813", "−8475", "Commit y push"] {
            assert!(has(&wide, label), "{label} in {wide:?}");
        }
        // Nothing changed: no counts to read.
        let clean = foot(0, 0, 0, 320.0);
        assert!(has(&clean, "Sin cambios") && !has(&clean, "+0"), "{clean:?}");
    }

    #[test]
    fn a_run_under_way_turns_off_what_would_change_the_repository_and_says_why() {
        let mut state = crate::view::tests::empty_state();
        repository(&mut state);
        state.running = true;
        let mut tree = tree(&state, 340.0, 344.0);
        let started = Instant::now();
        let frame = tree.layout_at(SIZE, started);
        for id in [
            "environment_project_toggle",
            "environment_git_open_compare",
            "environment_git_open_commit",
        ] {
            assert!(button(&frame, id).is_none(), "{id}");
        }
        for id in [
            "environment_local_toggle",
            "environment_branch_toggle",
            "project_actions_open",
            "environment_details_toggle",
            "dock_open:review",
            "environment_dismiss",
        ] {
            assert!(button(&frame, id).is_some(), "{id}");
        }
        assert!(texts(&frame).contains(&"Commit y push"));
        assert!(texts(&frame).contains(&"Comparar cambios"));
        // The reason is one hover away, on both.
        for id in ["environment_git_open_commit", "environment_git_open_compare"] {
            let anchor = frame.regions.iter().find(|region| region.id == id).unwrap().rect;
            tree.handle_event_at(UiEvent::PointerMove(center(anchor)), started);
            tree.layout_at(SIZE, started);
            let hint = tree.layout_at(SIZE, started + Duration::from_secs(1));
            assert!(texts(&hint).contains(&LOCKED), "{id}: {:?}", texts(&hint));
        }
        // A branch that is changing cannot be changed again.
        state.running = false;
        state.composer_branches.switching = Some("feature/alpha".into());
        let frame = tree_frame(&state);
        assert!(button(&frame, "environment_branch_toggle").is_none());
    }

    fn tree_frame(state: &ViewState) -> Frame {
        frame(state, 340.0, 344.0)
    }

    #[test]
    fn the_panel_is_as_tall_as_its_rows_until_they_scroll_between_header_and_foot() {
        let mut state = crate::view::tests::empty_state();
        repository(&mut state);
        let short = frame(&state, 320.0, 620.0);
        let natural = surface(&short).height;
        assert!(natural < 344.0, "the closed card fits its preferred height: {natural}");
        let rows = short.regions.iter().find(|region| region.id == "chat_environment").unwrap().rect;
        assert_eq!(natural, rows.height + CARD_CHROME + 2.0 * EDGE);
        // Open, a section makes the panel taller, up to what it is given.
        state.environment.details_expanded = true;
        let open = frame(&state, 320.0, 1100.0);
        assert!(surface(&open).height > natural);
        let capped = frame(&state, 320.0, 240.0);
        assert_eq!(surface(&capped).height, 240.0);
        let rect = |id: &str| button(&capped, id).unwrap();
        for id in ["environment_dismiss", "dock_open:review", "environment_git_open_commit"] {
            assert!(holds(Rect::new(0.0, 0.0, 320.0, 240.0), rect(id)), "{id}");
        }
        // Too short for two rows between them, the header and the foot scroll too.
        let tiny = frame(&state, 320.0, 120.0);
        assert_eq!(surface(&tiny).height, 120.0);
        let scroll = tiny.regions.iter().find(|region| region.id == "chat_environment").unwrap();
        assert_eq!(scroll.rect, Rect::new(EDGE, EDGE, 320.0 - 2.0 * EDGE, 120.0 - 2.0 * EDGE));
        assert!(holds(scroll.rect, button(&tiny, "environment_dismiss").unwrap()));
        // A Git form keeps to the same limit.
        state.environment.workflow.panel = EnvironmentGitPanel::Compare;
        let form = frame(&state, 320.0, 240.0);
        assert!(surface(&form).height <= 240.0);
        assert!(button(&form, "environment_git_back").is_some());
        assert!(button(&form, "environment_dismiss").is_some());
    }

    /// The fill and the border painted on exactly `rect`.
    fn shell(frame: &Frame, rect: Rect) -> ((Color, f32), (Color, f32, f32)) {
        let fill = frame.commands.iter().find_map(|command| match command {
            DrawCommand::FillRect { rect: own, color, radius } if *own == rect => Some((*color, *radius)),
            _ => None,
        });
        let border = frame.commands.iter().find_map(|command| match command {
            DrawCommand::StrokeRect { rect: own, color, width, radius } if *own == rect => {
                Some((*color, *width, *radius))
            }
            _ => None,
        });
        (fill.expect("a fill"), border.expect("a border"))
    }

    /// A repository read from an empty one in a temporary directory.
    fn snapshot() -> crate::environment_git::GitSnapshot {
        let directory = tempfile::tempdir().unwrap();
        assert!(crate::child_process::command_no_window("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .unwrap()
            .success());
        crate::environment_git::load_snapshot(directory.path()).unwrap()
    }

    #[test]
    fn the_panel_is_the_surface_the_context_popover_stands_on() {
        // The context popover, open over its ring.
        let mut state = crate::view::tests::empty_state();
        state.selected_backend = "native".into();
        state.composer_context_open = true;
        let props = ComposerProps::from_state(&state, 400.0, false, None, "", None);
        let mut popover = UiTree::new(Element::column(vec![
            Element::spacer().flex(1.0),
            Element::row(vec![Element::spacer().flex(1.0), crate::composer_context_ui::control(&props)]),
        ]));
        popover.set_reduced_motion(true);
        let popover = popover.layout(SIZE);
        let floating = popover
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:composer_context_dismiss")
            .expect("the context popover is open")
            .rect;
        let reference = shell(&popover, floating);

        repository(&mut state);
        let shot = |state: &ViewState, height: f32| (frame(state, 320.0, height), height);
        let mut states = vec![("overview", shot(&state, 344.0))];
        state.environment.details_expanded = true;
        state.environment.workflow.panel = EnvironmentGitPanel::Actions;
        states.push(("sections", shot(&state, 1100.0)));
        states.push(("short", shot(&state, 240.0)));
        states.push(("tiny", shot(&state, 120.0)));
        state.environment.workflow.snapshot = Some(snapshot());
        for panel in [
            EnvironmentGitPanel::Commit,
            EnvironmentGitPanel::Compare,
            EnvironmentGitPanel::CreateBranch,
            EnvironmentGitPanel::CreateWorktree,
        ] {
            state.environment.workflow.panel = panel;
            states.push(("form", shot(&state, 620.0)));
        }
        let tint = card_palette().hover;
        let tint = Color::rgba(tint.r, tint.g, tint.b, tint.a / 2);
        for (name, (frame, height)) in states {
            let panel = surface(&frame);
            assert_eq!((panel.x, panel.y, panel.width), (0.0, 0.0, 320.0), "{name}");
            assert!(panel.height <= height, "{name}: {panel:?}");
            // Its fill, its border and its corners are the popover's own.
            assert_eq!(shell(&frame, panel), reference, "{name}");
            assert_eq!(reference.0, (theme::menu_palette().raised, RadiusRole::Surface.px()));
            // Nothing stands between the surface and its content: no second
            // card, and no frame tinted around one.
            assert_eq!(fills(&frame, card_palette().raised).len(), 1, "{name}");
            assert!(fills(&frame, tint).is_empty(), "{name}");
            // Its four corners stay round: nothing else is painted over them.
            for (x, y) in [(0.5, 0.5), (319.5, 0.5), (0.5, panel.height - 0.5), (319.5, panel.height - 0.5)] {
                let over = frame.commands.iter().skip(1).any(|command| match command {
                    DrawCommand::FillRect { rect, .. } | DrawCommand::FillCornerRect { rect, .. } => {
                        *rect != panel && holds(*rect, Rect::new(x, y, 0.0, 0.0))
                    }
                    _ => false,
                });
                assert!(!over, "{name}: a fill covers the corner at {x}, {y}");
            }
            // Every state keeps the way out, and a form the way back.
            assert!(button(&frame, "environment_dismiss").is_some(), "{name}");
            if name == "form" {
                assert!(button(&frame, "environment_git_back").is_some());
            } else if name != "tiny" {
                for id in ["dock_open:review", "environment_git_open_commit", "environment_git_refresh"] {
                    assert!(holds(panel, button(&frame, id).unwrap()), "{name}: {id}");
                }
            }
        }
    }

    #[test]
    fn the_foot_stays_under_its_hairline_while_the_rows_scroll() {
        let mut state = crate::view::tests::empty_state();
        repository(&mut state);
        state.environment.details_expanded = true;
        let mut tree = tree(&state, 320.0, 240.0);
        let frame = tree.layout(SIZE);
        let rect = |frame: &Frame, id: &str| button(frame, id).unwrap_or_else(|| panic!("missing {id}"));
        // The hairline the context popover has over its own foot.
        let rule = |frame: &Frame| {
            let rules: Vec<_> = fills(frame, theme::component_palette().border)
                .into_iter()
                .filter(|rect| rect.height == 1.0)
                .collect();
            assert_eq!(rules.len(), 1, "{rules:?}");
            rules[0]
        };
        let rows = frame.regions.iter().find(|region| region.id == "chat_environment").unwrap().rect;
        let (line, changes, commit, close) = (
            rule(&frame),
            rect(&frame, "dock_open:review"),
            rect(&frame, "environment_git_open_commit"),
            rect(&frame, "environment_dismiss"),
        );
        // It spans the surface inside its border, between the rows and the foot.
        assert_eq!((line.x, line.width), (EDGE, 320.0 - 2.0 * EDGE));
        assert_eq!(line.y, rows.y + rows.height);
        assert!(changes.y > line.y && commit.y > line.y);
        assert_eq!(surface(&frame).height, 240.0);
        let first = rect(&frame, "environment_project_toggle");
        // A wheel turned down moves the rows up under the header.
        tree.handle_event(UiEvent::ScrollDirect { position: center(rows), delta_y: -120.0 });
        let scrolled = tree.layout(SIZE);
        assert_eq!(tree.scroll_offset("chat_environment"), 120.0);
        assert!(button(&scrolled, "environment_project_toggle").is_none_or(|rect| rect.y < first.y));
        assert_eq!(rule(&scrolled), line);
        assert_eq!(rect(&scrolled, "dock_open:review"), changes);
        assert_eq!(rect(&scrolled, "environment_git_open_commit"), commit);
        assert_eq!(rect(&scrolled, "environment_dismiss"), close);
        assert_eq!(surface(&scrolled).height, 240.0);
    }

    #[test]
    fn a_view_opens_in_place_of_the_one_it_replaces_and_the_panel_follows_both() {
        let mut state = crate::view::tests::empty_state();
        repository(&mut state);
        let mut repository = snapshot();
        repository.staged = 1;
        state.environment.workflow.snapshot = Some(repository);
        let root = |state: &ViewState| {
            Element::row(vec![
                Element::column(vec![panel(state, 320.0, 620.0), Element::spacer().flex(1.0)]),
                Element::spacer().flex(1.0),
            ])
        };
        let height = |frame: &Frame| surface(frame).height;
        // A view's slot moves on the library's spring: it all but arrives in
        // one and a half moves and is at rest well within three times that.
        let settle = Duration::from_millis(u64::from(openide_ui::tokens().motion.move_ms) * 3 / 2);
        let start = Instant::now() - Duration::from_secs(10);
        let mut tree = UiTree::new(root(&state));
        let overview = height(&tree.layout_at(SIZE, start));

        state.environment.workflow.panel = EnvironmentGitPanel::Commit;
        tree.set_root(root(&state));
        // The frame the form arrives in has not grown.
        assert_eq!(height(&tree.layout_at(SIZE, start)), overview);
        let frame = tree.layout_at(SIZE, start + settle / 2);
        let midway = height(&frame);
        // The form's heading is where it will stay, over what it replaces.
        let back = button(&frame, "environment_git_back").expect("the form is reachable as it opens");
        assert!(texts(&frame).contains(&"Entorno"), "the overview is still fading");
        assert!(button(&frame, "environment_git_open_commit").is_none(), "what leaves is paint only");
        let end = start + settle * 3;
        let frame = tree.layout_at(SIZE, end);
        let form = height(&frame);
        assert!(overview < midway && midway < form, "{overview} {midway} {form}");
        assert_eq!(button(&frame, "environment_git_back").unwrap().y, back.y);
        assert!(!texts(&frame).contains(&"Entorno"));

        // Back, the overview opens over the form the same way.
        state.environment.workflow.panel = EnvironmentGitPanel::Overview;
        tree.set_root(root(&state));
        assert_eq!(height(&tree.layout_at(SIZE, end)), form);
        let frame = tree.layout_at(SIZE, end + settle / 2);
        let midway = height(&frame);
        assert!(overview < midway && midway < form, "{overview} {midway} {form}");
        assert!(button(&frame, "environment_dismiss").is_some());
        let frame = tree.layout_at(SIZE, end + settle * 3);
        assert_eq!(height(&frame), overview);
        assert!(button(&frame, "environment_git_open_commit").is_some());
        assert!(button(&frame, "environment_git_back").is_none());
        assert!(!tree.is_animating());
    }

    #[test]
    fn the_action_editor_opens_in_place_of_the_list_and_its_block_follows() {
        let mut state = crate::view::tests::empty_state();
        saved_action(&mut state);
        state.environment.workflow.panel = EnvironmentGitPanel::Actions;
        let root = |state: &ViewState| {
            Element::row(vec![
                Element::column(vec![panel(state, 320.0, 1100.0), Element::spacer().flex(1.0)]),
                Element::spacer().flex(1.0),
            ])
        };
        let height = |frame: &Frame| surface(frame).height;
        let has = |frame: &Frame, id: &str| frame.regions.iter().any(|region| region.id == id);
        // The same spring as a section: all but there in one and a half
        // moves, at rest well within three times that.
        let settle = Duration::from_millis(u64::from(openide_ui::tokens().motion.move_ms) * 3 / 2);
        let start = Instant::now() - Duration::from_secs(10);
        let mut tree = UiTree::new(root(&state));
        let list = height(&tree.layout_at(SIZE, start));

        state.project_actions.begin_new();
        tree.set_root(root(&state));
        assert_eq!(height(&tree.layout_at(SIZE, start)), list);
        let frame = tree.layout_at(SIZE, start + settle / 2);
        let midway = height(&frame);
        assert!(has(&frame, "project_action_label"));
        assert!(!has(&frame, "project_action_run:check"), "the list that leaves is paint only");
        let end = start + settle * 3;
        let editor = height(&tree.layout_at(SIZE, end));
        assert!(list < midway && midway < editor, "{list} {midway} {editor}");

        state.project_actions.cancel_edit();
        tree.set_root(root(&state));
        assert_eq!(height(&tree.layout_at(SIZE, end)), editor);
        let midway = height(&tree.layout_at(SIZE, end + settle / 2));
        assert!(list < midway && midway < editor, "{list} {midway} {editor}");
        let frame = tree.layout_at(SIZE, end + settle * 3);
        assert_eq!(height(&frame), list);
        assert!(has(&frame, "project_action_run:check") && !has(&frame, "project_action_label"));
        assert!(!tree.is_animating());
    }

    #[test]
    fn the_new_tab_page_suggests_what_the_conversation_has_to_show() {
        use openide_core::{PlanSnapshot, PlanStep, PlanStepStatus};
        let texts_of = |state: &ViewState| {
            let frame = UiTree::new(crate::view::tests::new_tab_page(state, 420.0)).layout(Size::new(420.0, 900.0));
            let ids: Vec<String> = frame.regions.iter().map(|region| region.id.clone()).collect();
            (texts(&frame).join(" | "), ids)
        };
        let mut state = crate::view::tests::empty_state();
        // With nothing of its own the page lists every view and suggests none.
        let (said, ids) = texts_of(&state);
        assert!(said.contains("Nueva pestaña") && !said.contains("Sugerencias"), "{said}");
        assert!(ids.iter().any(|id| id == "dock_open:browser"));

        state.environment.changes.files = 3;
        state.selected_session = Some("session".into());
        state.session_plans.insert("session".into(), crate::view::SessionPlanView {
            snapshot: PlanSnapshot {
                explanation: None,
                plan: vec![
                    PlanStep { step: "Uno".into(), status: PlanStepStatus::Completed },
                    PlanStep { step: "Dos".into(), status: PlanStepStatus::Pending },
                ],
            },
            can_build: false,
        });
        let (said, ids) = texts_of(&state);
        for text in ["Sugerencias", "Cambios del chat", "3 archivos", "Plan", "1 de 2", "Todas las vistas"] {
            assert!(said.contains(text), "{text}: {said}");
        }
        assert!(ids.iter().any(|id| id == "environment_open"));
    }

    #[test]
    fn subagents_are_listed_as_active_and_finished_with_what_each_answered() {
        let mut state = crate::view::tests::empty_state();
        repository(&mut state);
        going(&mut state);
        state.environment.subagents_expanded = true;
        let ended = |run: &crate::view::SubagentSessionView| run.status == openide_harness::SubagentStatus::Completed;
        assert!(state.subagent_sessions.iter().any(ended) && !state.subagent_sessions.iter().all(ended));
        for run in state.subagent_sessions.iter_mut().filter(|run| ended(run)) {
            run.final_response = Some("Encontré dos regresiones.\nEl detalle sigue acá.".into());
        }
        let said: Vec<String> = texts(&frame(&state, 380.0, 1100.0)).into_iter().map(str::to_owned).collect();
        let place = |start: &str| said.iter().position(|text| text.starts_with(start)).unwrap_or_else(|| panic!("{start}: {said:?}"));
        // Those at work come first, each group under its count, and one that
        // ended says what it answered in a line.
        assert!(place("Activos · 1") < place("En ejecución"));
        assert!(place("En ejecución") < place("Terminados · 1"));
        assert!(place("Terminados · 1") < place("Completado · Encontré dos"));
        assert!(!said.iter().any(|text| text.contains("El detalle sigue")), "{said:?}");

        // With only one kind there is nothing to tell apart.
        state.subagent_sessions.retain(|run| !ended(run));
        let alone = texts(&frame(&state, 380.0, 1100.0)).join(" | ");
        assert!(!alone.contains("Activos ·") && !alone.contains("Terminados ·"), "{alone}");
    }

    #[test]
    fn the_plan_of_the_conversation_shows_its_steps_as_progress() {
        use openide_core::{PlanSnapshot, PlanStep, PlanStepStatus};
        let mut state = crate::view::tests::empty_state();
        repository(&mut state);
        assert!(button(&frame(&state, 380.0, 1100.0), "environment_progress_toggle").is_none());

        let step = |step: &str, status| PlanStep { step: step.into(), status };
        let plan = crate::view::SessionPlanView {
            snapshot: PlanSnapshot {
                explanation: None,
                plan: vec![
                    step("Leer el módulo", PlanStepStatus::Completed),
                    step("Escribir la prueba", PlanStepStatus::InProgress),
                    step("Publicar", PlanStepStatus::Pending),
                ],
            },
            can_build: false,
        };
        state.selected_session = Some("session".into());
        state.session_plans.insert("session".into(), plan.clone());
        // Another conversation's plan is not this one's.
        state.session_plans.insert("other".into(), crate::view::SessionPlanView {
            snapshot: PlanSnapshot { explanation: None, plan: vec![step("De otra conversación", PlanStepStatus::Pending)] },
            can_build: false,
        });
        let closed = frame(&state, 380.0, 1100.0);
        assert!(button(&closed, "environment_progress_toggle").is_some());
        let said = texts(&closed);
        assert!(said.contains(&"Progreso") && said.contains(&"1 de 3"), "{said:?}");
        assert!(!said.contains(&"Escribir la prueba"));

        state.environment.progress_expanded = true;
        let open = frame(&state, 380.0, 1100.0);
        let said = texts(&open);
        for text in ["Leer el módulo", "Escribir la prueba", "Publicar"] {
            assert!(said.contains(&text), "{text}: {said:?}");
        }
        assert!(!said.contains(&"De otra conversación"));
    }

    #[test]
    fn what_the_conversation_has_going_is_listed_only_while_there_is_any() {
        let mut state = crate::view::tests::empty_state();
        repository(&mut state);
        let quiet = frame(&state, 380.0, 1100.0);
        for id in ["environment_subagents_toggle", "environment_terminals_toggle", "environment_preview"] {
            assert!(button(&quiet, id).is_none(), "{id}");
        }
        going(&mut state);
        let closed = frame(&state, 380.0, 1100.0);
        // Between the project's actions and the sources.
        let y = |id: &str| button(&closed, id).unwrap_or_else(|| panic!("missing {id}")).y;
        let order = [
            "project_actions_open",
            "environment_subagents_toggle",
            "environment_terminals_toggle",
            "environment_preview",
            "environment_details_toggle",
        ];
        assert!(order.windows(2).all(|pair| y(pair[0]) < y(pair[1])));
        // Each block says how many of its own are still going.
        let said = texts(&closed);
        assert_eq!(said.iter().filter(|text| **text == "1 en curso").count(), 2, "{said:?}");
        assert!(said.contains(&"http://localhost:5173/"));
        assert!(button(&closed, "environment_subagent:child-1").is_none());

        state.environment.subagents_expanded = true;
        state.environment.terminals_expanded = true;
        let open = frame(&state, 380.0, 1100.0);
        let said = texts(&open);
        for text in [
            "Actualizar pruebas",
            "En ejecución",
            "Revisar el diff",
            "Completado",
            "Detener todos",
            "npm run dev",
            "cargo test",
            "Proceso finalizado · código 0",
        ] {
            assert!(said.contains(&text), "{text}: {said:?}");
        }
        // Another conversation's subagent is not this one's.
        assert!(!said.contains(&"De otra conversación"));
        assert!(button(&open, "environment_subagent:other").is_none());
        // Only a command still running can be stopped.
        assert!(button(&open, "environment_terminal_stop:7").is_some());
        assert!(button(&open, "environment_terminal_stop:8").is_none());
    }

    #[test]
    fn what_is_going_dispatches_the_actions_the_app_already_has() {
        let mut state = crate::view::tests::empty_state();
        repository(&mut state);
        going(&mut state);
        state.environment.subagents_expanded = true;
        state.environment.terminals_expanded = true;
        for (id, action) in [
            ("environment_subagents_toggle", "environment_subagents_toggle"),
            ("environment_subagent:child-1", "subagent_open:child-1"),
            ("environment_subagent:child-2", "subagent_open:child-2"),
            ("environment_subagents_stop", "environment_subagents_stop"),
            ("environment_terminals_toggle", "environment_terminals_toggle"),
            ("agent_terminal_show:cmd-7", "agent_terminal_show:cmd-7"),
            ("agent_terminal_show:cmd-8", "agent_terminal_show:cmd-8"),
            // Closing its tab is what asks before the process is stopped.
            ("environment_terminal_stop:7", "terminal_close:7"),
            ("environment_preview", "agent_preview_open:cmd-7"),
        ] {
            let mut tree = tree(&state, 380.0, 1100.0);
            let frame = tree.layout(SIZE);
            let point = center(button(&frame, id).unwrap_or_else(|| panic!("missing {id}")));
            let clicked = vec![UiAction::Clicked { id: action.into() }];
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(tree.handle_event(UiEvent::PointerUp(point)), clicked, "{id}");
            assert!(tree.focus(id), "{id} takes no focus");
            let enter = UiEvent::Key { key: Key::Enter, shift: false, ctrl: false };
            assert_eq!(tree.handle_event(enter), clicked, "{id}");
        }
    }

    #[test]
    fn a_change_of_state_shows_on_its_row_and_in_its_blocks_figure() {
        let mut state = crate::view::tests::empty_state();
        repository(&mut state);
        going(&mut state);
        state.environment.subagents_expanded = true;
        state.environment.terminals_expanded = true;
        // Paused on a limit is its own state.
        state.pause.sessions.insert(
            "child-1".into(),
            crate::pause::PausedTurn {
                class: openide_core::TurnPauseClass::LimitReached,
                limit_key: "fixture".into(),
                resets_at_ms: None,
                detail: String::new(),
                provider_id: "fixture".into(),
                parent: Some("s1".into()),
                paused_at_ms: 0,
                pauses_in_turn: 1,
            },
        );
        assert!(texts(&frame(&state, 380.0, 1100.0)).contains(&"En pausa"));
        state.pause.sessions.clear();
        state.subagent_sessions[0].cancellation_requested = true;
        let asked = frame(&state, 380.0, 1100.0);
        assert!(texts(&asked).contains(&"En ejecución · Cancelación solicitada"));
        // Nothing is left to stop, so the way to stop them all goes.
        assert!(button(&asked, "environment_subagents_stop").is_none());
        state.subagent_sessions[0].status = openide_harness::SubagentStatus::Cancelled;
        // The command ends, and with it the preview it served.
        let served = &mut state.terminal_controls.agents[0];
        served.ended = Some("Detenido".into());
        served.preview = None;
        let ended = frame(&state, 380.0, 1100.0);
        let said = texts(&ended);
        assert!(said.contains(&"Cancelado · Cancelación solicitada") && said.contains(&"Detenido"));
        assert!(!said.contains(&"1 en curso"), "{said:?}");
        // With none going, each block counts what it lists.
        assert_eq!(said.iter().filter(|text| **text == "2").count(), 2, "{said:?}");
        assert!(button(&ended, "environment_terminal_stop:7").is_none());
        assert!(button(&ended, "environment_preview").is_none());
        assert!(button(&ended, "agent_terminal_show:cmd-7").is_some());
    }

    #[test]
    fn the_card_makes_room_for_what_is_going_and_scrolls_it_when_short() {
        let mut state = crate::view::tests::empty_state();
        repository(&mut state);
        going(&mut state);
        state.environment.open = true;
        // Closed, the three rows fit in the height the panel asks for.
        let preferred = layout(&state, 1200.0, 900.0).panel_height;
        for width in [280.0, 380.0] {
            let closed = frame(&state, width, preferred);
            let panel = surface(&closed);
            assert!(panel.height <= preferred, "{panel:?}");
            assert_eq!(tree(&state, width, preferred).layout(SIZE).regions.iter().filter(|region| region.id == "chat_environment").count(), 1);
            for id in ["environment_subagents_toggle", "environment_terminals_toggle", "environment_preview", "environment_details_toggle"] {
                assert!(holds(panel, button(&closed, id).unwrap_or_else(|| panic!("missing {id}"))), "{id}");
            }
        }
        let mut one = crate::view::tests::empty_state();
        one.environment.open = true;
        assert!(preferred > layout(&one, 1200.0, 900.0).panel_height);
        // Open in a short panel, the rows scroll between the header and the foot.
        state.environment.subagents_expanded = true;
        state.environment.terminals_expanded = true;
        let mut tree = tree(&state, 280.0, 240.0);
        let short = tree.layout(SIZE);
        let panel = surface(&short);
        assert_eq!(panel.height, 240.0);
        let rows = short.regions.iter().find(|region| region.id == "chat_environment").unwrap().rect;
        for id in ["environment_dismiss", "dock_open:review", "environment_git_open_commit"] {
            let rect = button(&short, id).unwrap_or_else(|| panic!("missing {id}"));
            assert!(holds(panel, rect) && !holds(rows, rect), "{id}");
        }
        // Every row keeps inside the panel's width.
        for region in &short.regions {
            if region.id.starts_with("environment_") || region.id.starts_with("agent_") {
                assert!(region.rect.x >= panel.x && region.rect.x + region.rect.width <= panel.x + panel.width, "{region:?}");
            }
        }
        tree.handle_event(UiEvent::ScrollDirect { position: center(rows), delta_y: -400.0 });
        tree.layout(SIZE);
        assert!(tree.scroll_offset("chat_environment") > 0.0);
    }
}

#[cfg(test)]
#[path = "environment_layout_tests.rs"]
mod layout_tests;
