//! Conversation environment assembled from workspace and transcript facts.
//! Project and branch menus share the composer's snapshot-backed actions.

use std::{collections::HashSet, path::Path};

use openide_ui::{
    ButtonVariant, Color, Component, ComponentPalette, ControlSize, DiffStat, DropdownMenu,
    DropdownMenuEntry, DropdownMenuItem, Element, FontFamily, FramedCard, Icon, IconButton, ListRow,
    PopoverPlacement, ScrollArea, ScrollbarAppearance, SectionHeader, SectionSizing, SpinnerVariant,
    Text, TextRole, Tooltip, TooltipPlacement, TooltipSpec,
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
    pub workflow: crate::environment_git_ui::EnvironmentGitUiState,
}

/// The card's palette: its rows hover one step above it.
fn card_palette() -> ComponentPalette {
    theme::action_palette(theme::CARD, theme::FOREGROUND)
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
    } else if state.environment.details_expanded {
        580.0
    } else {
        344.0
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

/// What the card puts around its rows: the frame, its margins above and
/// below, the header and the foot.
// shortcut: a framed card on limn does not scroll its rows under a height, so
// the sum of what it adds is written here and a test holds it; drop it when
// the card can keep its header and its foot while its rows scroll.
const CARD_CHROME: f32 = 86.0;
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

/// As tall as what it shows, up to `height`: the card's rows scroll past
/// that, between its header and its foot.
pub(crate) fn panel(state: &ViewState, width: f32, height: f32) -> Element {
    use crate::environment_git_ui::EnvironmentGitPanel;
    let workflow = &state.environment.workflow;
    let content = if matches!(
        workflow.panel,
        EnvironmentGitPanel::Overview | EnvironmentGitPanel::Actions
    ) {
        card(state, width, height)
    } else {
        // A Git form stands on the panel's own surface.
        let inner = (width - 12.0).max(120.0);
        Element::panel(
            ScrollArea::new(
                "chat_environment",
                crate::environment_git_ui::build(workflow, inner, locked(state)),
            )
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .size(Some(inner), None)
            .max_height((height - 12.0).max(0.0)),
        )
        .padding(6.0)
    };
    Element::panel(content)
        .background(theme::CARD)
        .border(theme::BORDER, 1.0)
        .radius(16.0)
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
        .sizing(SectionSizing {
            padding: 6.0,
            ..SectionSizing::LABEL
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
    let row = |id: &str| {
        ListRow::new(id, title, block_palette())
            .leading(preview)
            .subtitle(detail)
            .title_ink(theme::FOREGROUND)
            .width(width)
            .sizing(crate::ui_sizing::ENVIRONMENT_SOURCE)
    };
    match action {
        Some(action) => Tooltip::new(
            row(&action)
                // Centered on the two lines of copy, not on the row.
                .trailing(
                    Element::icon(Icon::ChevronRight)
                        .foreground(theme::FOREGROUND_SUBTLE)
                        .size(Some(10.0), Some(36.0)),
                )
                .render(),
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
    let total = state.composer_attachments.len() + image_count + file_count + citation_count;
    let mut rows = vec![section_heading("Fuentes", Some(total.to_string()), width)];
    if total == 0 {
        rows.push(
            theme::text_px("Sin fuentes registradas.", 11.5, theme::FOREGROUND_SUBTLE).padding(6.0),
        );
        return Element::column(rows).gap(2.0).size(Some(width), None);
    }
    let mut omitted = 0usize;
    if !state.composer_attachments.is_empty() {
        rows.push(section_heading(
            "Borrador",
            Some(state.composer_attachments.len().to_string()),
            width,
        ));
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
                &attachment.path,
                &attachment.path,
                width,
            ));
        }
        omitted += state.composer_attachments.len().saturating_sub(GROUP_LIMIT);
    }
    if !images.is_empty() {
        rows.push(section_heading(
            "Imágenes enviadas",
            Some(image_count.to_string()),
            width,
        ));
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
    if !files.is_empty() {
        rows.push(section_heading(
            "Archivos consultados",
            Some(file_count.to_string()),
            width,
        ));
        for (index, path) in files.iter().take(GROUP_LIMIT) {
            rows.push(source_row(
                Some(format!("environment_source_file:{index}")),
                Element::icon(openide_ui::file_icon(path)).foreground(theme::FOREGROUND_SUBTLE),
                folder_name(path),
                path,
                &format!("Abrir archivo consultado\n{path}"),
                width,
            ));
        }
        omitted += file_count.saturating_sub(GROUP_LIMIT);
    }
    if !citations.is_empty() {
        rows.push(section_heading(
            "Referencias web",
            Some(citation_count.to_string()),
            width,
        ));
        for (index, title, url) in citations.iter().take(GROUP_LIMIT) {
            rows.push(source_row(
                Some(format!("web_citation_open:{index}")),
                Element::icon(Icon::Globe).foreground(theme::FOREGROUND_SUBTLE),
                if title.trim().is_empty() { url } else { title },
                url,
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
            if state.browser.browser_missing {
                return "Falta un navegador".into();
            }
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
    // The frame is a shade above the card, so its controls hover as those of
    // an open block do.
    let palette = block_palette();
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
    let mut rows = vec![
        action_row(
            "environment_changes_open",
            Icon::Code,
            "Cambios del chat",
            &changes_detail,
            changes
                .available
                .then(|| counts(changes.additions, changes.deletions).centered_y()),
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

/// The card on the panel's frame. Its header and its foot stay; the rows
/// between them scroll once the card would be taller than `height`.
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
        details,
    ];
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
    let scroll = |content: Element, height: f32| {
        ScrollArea::new("chat_environment", content)
            .appearance(
                ScrollbarAppearance::overlay()
                    .rail_width(FramedCard::REACH)
                    .thumb_width(4.0)
                    .buttons(false)
                    .tint(theme::FOREGROUND_SUBTLE),
            )
            .render()
            .max_height(height.max(0.0))
    };
    let rows = Element::column(rows).gap(FramedCard::ROW_GAP);
    // With no room for two rows between them, the header and the foot scroll
    // with the rows instead of staying.
    let pinned = height - CARD_CHROME >= PROJECT_ROW_HEIGHT + FramedCard::ROW_HEIGHT;
    let card = FramedCard::new(palette)
        .width(width)
        .section(card_header(workflow.loading))
        .rows(if pinned {
            scroll(rows, height - CARD_CHROME)
        } else {
            rows
        })
        .foot(foot(state, width, locked))
        .render();
    if pinned {
        card
    } else {
        scroll(card, height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment_git_ui::EnvironmentGitPanel;
    use openide_ui::{
        DrawCommand, Frame, HitKind, Key, Point, Rect, Size, UiAction, UiEvent, UiTree,
    };
    use std::time::{Duration, Instant};

    const SIZE: Size = Size::new(420.0, 1200.0);

    fn repository(state: &mut ViewState) {
        state.git_review = Some(Ok(crate::git_review::GitReview {
            additions: 25813,
            deletions: 8475,
            files: 17,
            ..Default::default()
        }
        .into()));
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
        assert!(texts(&frame(&state, 380.0, 1100.0)).contains(&"Sin fuentes registradas."));
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
        let (inset, reach) = (FramedCard::INSET, FramedCard::INSET - FramedCard::REACH);
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
                assert!(fill.x > FramedCard::FRAME + 1.0);
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
            let reach = FramedCard::INSET - FramedCard::REACH;
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
        let height = |frame: &Frame| fills(frame, theme::CARD)[0].height;
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
            assert!(changes.x >= FramedCard::INSET && changes.x + changes.width <= commit.x);
            assert_eq!(commit.x + commit.width, width - FramedCard::INSET);
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
        let surface = |frame: &Frame| fills(frame, theme::CARD)[0];
        let short = frame(&state, 320.0, 620.0);
        let natural = surface(&short).height;
        assert!(natural < 344.0, "the closed card fits its preferred height: {natural}");
        let rows = short.regions.iter().find(|region| region.id == "chat_environment").unwrap().rect;
        assert_eq!(natural, rows.height + CARD_CHROME);
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
        assert_eq!(scroll.rect, Rect::new(0.0, 0.0, 320.0, 120.0));
        assert!(holds(scroll.rect, button(&tiny, "environment_dismiss").unwrap()));
        // A Git form keeps to the same limit.
        state.environment.workflow.panel = EnvironmentGitPanel::Compare;
        let form = frame(&state, 320.0, 240.0);
        assert!(surface(&form).height <= 240.0);
        assert!(button(&form, "environment_git_back").is_some());
        assert!(button(&form, "environment_dismiss").is_some());
    }
}

#[cfg(test)]
#[path = "environment_layout_tests.rs"]
mod layout_tests;
