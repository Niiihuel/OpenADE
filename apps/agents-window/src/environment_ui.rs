//! Conversation environment assembled from workspace and transcript facts.
//! Project and branch menus share the composer's snapshot-backed actions.

use std::{collections::HashSet, path::Path};

use openide_ui::{
    ButtonVariant, Component, ControlSize, DiffStat, DropdownMenu, DropdownMenuEntry,
    DropdownMenuItem, Element, FontFamily, HeaderSizing, Icon, IconButton, ListRow, PanelHeader,
    ScrollArea, SectionHeader, SectionSizing, Separator, SpinnerVariant, Text, TextRole, Tooltip,
    TooltipPlacement, TooltipSpec,
};

use crate::{
    composer_ui::{branch_selector_inline, project_selector_inline, ComposerProps},
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

pub(crate) fn interactive_surface(element: Element) -> Element {
    theme::action_palette(theme::CARD, theme::FOREGROUND).interactive_surface(element)
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
        || state.environment.menu != EnvironmentMenu::None
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

pub(crate) fn panel(state: &ViewState, width: f32, height: f32) -> Element {
    Element::panel(
        ScrollArea::new("chat_environment", build(state, (width - 12.0).max(0.0)))
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .size(
                Some((width - 12.0).max(0.0)),
                Some((height - 12.0).max(0.0)),
            ),
    )
    .padding(6.0)
    .background(theme::CARD)
    .border(theme::BORDER, 1.0)
    .radius(12.0)
    .size(Some(width), Some(height))
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

/// Hand-built on purpose: in the card its chevron falls outside the clip,
/// and a `ListRow` would fit it in. See the ledger (s09a).
fn selector_trigger(
    id: &str,
    icon: Icon,
    label: &str,
    detail: &str,
    width: f32,
    enabled: bool,
) -> Element {
    let label_width = (width - 54.0).max(0.0);
    let visual = Element::row(vec![
        Element::icon(icon)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(18.0), Some(36.0)),
        Element::column(vec![
            Text::new(label, theme::component_palette())
                .role(TextRole::Label)
                .truncate()
                .render()
                .size(Some(label_width.max(0.0)), Some(18.0)),
            Text::new(detail, theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .truncate()
                .render()
                .size(Some(label_width.max(0.0)), Some(18.0)),
        ])
        .size(Some(label_width), Some(36.0)),
        Element::icon(Icon::ChevronDown)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(16.0), Some(36.0)),
    ])
    .gap(6.0)
    .clip_children();
    let trigger = interactive_surface(if enabled {
        Element::clickable(id, visual)
    } else {
        Element::panel(visual)
    })
    .padding(4.0)
    .size(Some(width), Some(44.0));
    Tooltip::new(trigger, id, detail, theme::component_palette()).render()
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
    let mut heading = SectionHeader::new(title, theme::component_palette())
        .width(width)
        .sizing(SectionSizing::LABEL);
    if let Some(detail) = detail {
        heading = heading.detail(detail);
    }
    heading.render()
}

/// Hand-built on purpose, like `selector_trigger`: the card lays it out
/// narrower than `width`, cutting its counts and hiding its chevron.
fn action_row(
    id: &str,
    icon: Icon,
    title: &str,
    detail: &str,
    trailing: Option<Element>,
    help: &str,
    width: f32,
) -> Element {
    let label_width = (width - 48.0).max(0.0);
    let mut main = vec![Element::truncated_text(title)
        .font_family(FontFamily::Sans)
        .font_size(12.5)
        .foreground(theme::FOREGROUND)
        .size(Some(label_width.max(0.0)), Some(18.0))
        .flex(1.0)];
    if let Some(trailing) = trailing {
        main.push(trailing);
    }
    let mut lines = vec![Element::row(main)
        .gap(6.0)
        .size(Some(label_width), Some(18.0))];
    if !detail.is_empty() {
        lines.push(
            Element::truncated_text(detail)
                .font_family(FontFamily::Sans)
                .font_size(10.5)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(label_width.max(0.0)), Some(18.0)),
        );
    }
    let height = if detail.is_empty() { 32.0 } else { 48.0 };
    let row = Element::row(vec![
        Element::icon(icon)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(16.0), Some(height - 12.0)),
        Element::column(lines).size(Some(label_width), Some(height - 12.0)),
        Element::icon(Icon::ChevronRight)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(12.0), Some(height - 12.0)),
    ])
    .gap(4.0)
    .clip_children();
    Tooltip::new(
        interactive_surface(Element::clickable(id, row))
            .padding(6.0)
            .size(Some(width), Some(height)),
        id,
        help,
        theme::component_palette(),
    )
    .render()
}

fn source_row(
    action: Option<String>,
    preview: Element,
    title: &str,
    detail: &str,
    help: &str,
    width: f32,
) -> Element {
    let label_width = (width - 56.0).max(0.0);
    let mut columns = vec![
        preview.size(Some(30.0), Some(30.0)),
        Element::column(vec![
            Text::new(title, theme::component_palette())
                .truncate()
                .render()
                .size(Some(label_width.max(0.0)), Some(18.0)),
            Element::truncated_text(detail)
                .font_family(FontFamily::Sans)
                .font_size(10.5)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(label_width.max(0.0)), Some(18.0)),
        ])
        .size(Some(label_width), Some(36.0)),
    ];
    if action.is_some() {
        columns.push(
            Element::icon(Icon::ChevronRight)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(10.0), Some(36.0)),
        );
    }
    let content = Element::row(columns).gap(5.0).clip_children();
    if let Some(action) = action {
        Tooltip::new(
            interactive_surface(Element::clickable(&action, content))
                .padding(5.0)
                .size(Some(width), Some(48.0)),
            action,
            help,
            theme::component_palette(),
        )
        .render()
    } else {
        Element::panel(content)
            .padding(5.0)
            .size(Some(width), Some(48.0))
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
            theme::text_px("Sin fuentes registradas.", 11.5, theme::FOREGROUND_SUBTLE).padding(5.0),
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
            .padding(5.0),
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
            if state.browser.installing {
                return "Instalando".into();
            }
            if state.browser.dependency_missing {
                return "Requiere instalación".into();
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

/// A one-line row of the card: the card's hover step, and a hint in the
/// page's colors. The caller adds its trailing element.
fn compact_row(
    id: &str,
    icon: Icon,
    title: &str,
    help: &str,
    width: f32,
    enabled: bool,
) -> ListRow {
    ListRow::new(
        id,
        title,
        theme::action_palette(theme::CARD, theme::FOREGROUND),
    )
    .icon(icon)
    .icon_ink(theme::ICON_PRIMARY)
    .enabled(enabled)
    .width(width)
    .tooltip_with(TooltipSpec::new(help).palette(theme::component_palette()))
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
    let anchor = compact_row(
        "environment_local_toggle",
        Icon::Laptop,
        "Local",
        &format!("Carpeta de trabajo\n{}", state.workspace),
        width,
        true,
    )
    .trailing_icon(Icon::ChevronDown)
    .render();
    DropdownMenu::new(
        anchor,
        entries,
        state.environment.menu == EnvironmentMenu::Local,
        "environment_local_dismiss",
        theme::menu_palette(),
    )
    .inline(true)
    .width(width)
    .render()
}

/// The card's heading: its title, the refresh tool and the close button.
fn card_header(loading: bool, width: f32) -> Element {
    let palette = theme::action_palette(theme::CARD, theme::FOREGROUND);
    PanelHeader::new("", palette)
        // The title wraps and can be selected, which a header's own cannot.
        .leading(
            Text::new("Entorno", theme::component_palette())
                .role(TextRole::Body)
                .render(),
        )
        .trailing(vec![IconButton::new(
            "environment_git_refresh",
            Icon::Refresh,
            palette,
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .enabled(!loading)
        .tooltip_with(TooltipSpec::new("Actualizar entorno").placement(TooltipPlacement::Below))
        .render()])
        .dismiss("environment_dismiss", "Cerrar entorno del chat")
        .width(width)
        .sizing(HeaderSizing::CARD)
        .render()
}

/// The heading of the project actions: back, the title, "new" and close.
fn actions_header(new_action: Option<bool>, width: f32) -> Element {
    let palette = theme::action_palette(theme::CARD, theme::FOREGROUND);
    let tools = new_action
        .map(|enabled| {
            IconButton::new("project_action_new", Icon::Add, palette)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .enabled(enabled)
                .tooltip_with(TooltipSpec::new("Crear acción").placement(TooltipPlacement::Below))
                .render()
        })
        .into_iter()
        .collect();
    PanelHeader::new("", palette)
        .back("environment_git_back", "Volver al entorno")
        .leading(
            Text::new("Acciones del proyecto", theme::component_palette())
                .role(TextRole::Body)
                .render(),
        )
        .trailing(tools)
        .dismiss("environment_dismiss", "Cerrar entorno del chat")
        .width(width)
        .sizing(crate::ui_sizing::TALL_HEADER)
        .render()
}

pub(crate) fn build(state: &ViewState, width: f32) -> Element {
    use crate::environment_git_ui::EnvironmentGitPanel;
    let width = width.max(120.0);
    let inner_width = (width - 16.0).max(0.0);
    let workflow = &state.environment.workflow;
    let locked = state.running
        || state.background_run_selected
        || state.git_action_pending
        || state.composer_branches.switching();
    if workflow.panel == EnvironmentGitPanel::Actions {
        let new_action = (!state.project_actions.actions.is_empty()
            && !state.project_actions.editor_open)
            .then_some(!state.project_actions.saving);
        return Element::column(vec![
            actions_header(new_action, inner_width),
            crate::project_actions_ui::build(&state.project_actions, inner_width),
        ])
        .gap(10.0)
        .padding(8.0)
        .size(Some(width), None);
    }
    if workflow.panel != EnvironmentGitPanel::Overview {
        return crate::environment_git_ui::build(workflow, width, locked);
    }
    let group_width = inner_width;
    let mut props = ComposerProps::from_state(state, group_width, false, None, "", None);
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
    let repo = state
        .git_review
        .as_ref()
        .and_then(|review| review.as_ref().ok());
    let changes_trailing = repo
        .map(|review| counts(review.additions, review.deletions))
        .or_else(|| {
            workflow.loading.then(|| {
                Element::spinner(
                    "environment_repo_progress",
                    theme::FOREGROUND_SUBTLE,
                    SpinnerVariant::Circle,
                )
                .size(Some(16.0), Some(20.0))
            })
        });
    let changes_label = repo.map_or_else(
        || "Cambios del repositorio".into(),
        |review| {
            if review.files == 0 {
                "Sin cambios".into()
            } else {
                format!("{} archivos", review.files)
            }
        },
    );
    let changes_help = repo.map_or_else(
        || "Abrir cambios del repositorio".into(),
        |review| {
            format!(
                "{} archivos · +{} · −{}{}",
                review.files,
                review.additions,
                review.deletions,
                if review.truncated {
                    " · Vista parcial"
                } else {
                    ""
                }
            )
        },
    );
    let project = project_selector_inline(
        &props,
        selector_trigger(
            "environment_project_toggle",
            Icon::Folder,
            folder_name(&state.workspace),
            if state.workspace.is_empty() {
                "Elegir carpeta"
            } else {
                &state.workspace
            },
            group_width,
            !locked,
        ),
    );
    let branch = branch_selector_inline(
        &props,
        compact_row(
            "environment_branch_toggle",
            Icon::GitBranch,
            branch,
            branch_detail,
            group_width,
            !state.composer_branches.switching(),
        )
        .trailing_icon(Icon::ChevronDown)
        .render(),
    );
    let action_width = (group_width - 6.0) / 2.0;
    // Rows sit directly on the card; thin rules separate the sections.
    let group = |rows| {
        Element::column(rows)
            .gap(2.0)
            .size(Some(inner_width), None)
    };
    let git_action = |id: &str, icon: Icon, label: &str, help: &str| {
        openide_ui::Component::render(
            openide_ui::Button::new(id, label, crate::settings_ui::control_palette())
                .icon(icon)
                .variant(openide_ui::ButtonVariant::Outline)
                .size(openide_ui::ControlSize::Compact)
                .width(action_width)
                .enabled(!locked)
                .tooltip(help),
        )
    };
    let mut rows = vec![
        card_header(workflow.loading, inner_width),
        group(vec![
            project,
            local_menu(state, group_width, locked),
            branch,
        ]),
        Separator::horizontal(theme::component_palette())
            .length(inner_width)
            .render(),
        section_heading("Control de versiones", None, inner_width),
        group(vec![
            {
                let changes = compact_row(
                    "dock_open:review",
                    Icon::Code,
                    &changes_label,
                    &changes_help,
                    group_width,
                    true,
                );
                match changes_trailing {
                    Some(trailing) => changes.trailing(trailing),
                    None => changes,
                }
                .render()
            },
            Element::row(vec![
                git_action(
                    "environment_git_open_commit",
                    Icon::Check,
                    "Commit y push",
                    "Revisar archivos preparados, crear un commit y publicar la rama",
                ),
                git_action(
                    "environment_git_open_compare",
                    Icon::GitBranch,
                    "Comparar",
                    "Elegir una rama base y comparar los commits confirmados",
                ),
            ])
            .gap(6.0)
            .size(Some(group_width), None),
        ]),
        Separator::horizontal(theme::component_palette())
            .length(inner_width)
            .render(),
        compact_row(
            "project_actions_open",
            Icon::Play,
            "Acciones del proyecto",
            "Configurar y ejecutar tareas del proyecto",
            inner_width,
            true,
        )
        .trailing_icon(Icon::ChevronRight)
        .render(),
        compact_row(
            "environment_details_toggle",
            Icon::Book,
            "Fuentes y herramientas",
            "Archivos consultados, referencias y herramientas de esta conversación",
            inner_width,
            true,
        )
        .trailing_icon(if state.environment.details_expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        })
        .render(),
    ];
    rows.extend(crate::environment_git_ui::notices(workflow));
    if state.environment.details_expanded {
        rows.push(
            Separator::horizontal(theme::component_palette())
                .length(inner_width)
                .render(),
        );
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
        rows.push(action_row(
            "environment_changes_open",
            Icon::Code,
            "Cambios del chat",
            &changes_detail,
            changes
                .available
                .then(|| counts(changes.additions, changes.deletions)),
            &changes_help_summary(changes),
            inner_width,
        ));
        rows.push(sources(state, inner_width));
        rows.push(
            Separator::horizontal(theme::component_palette())
                .length(inner_width)
                .render(),
        );
        rows.push(section_heading("Herramientas", None, inner_width));
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
                Some(theme::text_px(&status, 10.5, theme::FOREGROUND_SUBTLE)),
                &format!("Abrir {title} de esta conversación · {status}"),
                inner_width,
            ));
        }
    }
    Element::column(rows)
        .gap(3.0)
        .padding(8.0)
        .size(Some(width), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, HitKind, Size, UiTree};

    #[test]
    fn compact_environment_keeps_primary_actions_in_one_short_card() {
        let mut state = crate::view::tests::empty_state();
        state.git_review = Some(Ok(crate::git_review::GitReview {
            additions: 25813,
            deletions: 8475,
            files: 17,
            ..Default::default()
        }
        .into()));
        let frame = UiTree::new(build(&state, 340.0)).layout(Size::new(340.0, 332.0));
        for id in [
            "environment_project_toggle",
            "dock_open:review",
            "environment_local_toggle",
            "environment_branch_toggle",
            "environment_git_open_commit",
            "environment_git_open_compare",
            "environment_details_toggle",
            "project_actions_open",
        ] {
            let region = frame
                .regions
                .iter()
                .find(|region| region.id == id && region.kind == HitKind::Button)
                .unwrap_or_else(|| panic!("missing {id}"));
            assert!(
                region.rect.y + region.rect.height <= 332.0,
                "{id}: {:?}",
                region.rect
            );
        }
        let labels: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(labels.contains(&"+25813"));
        assert!(labels.contains(&"−8475"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "environment_changes_open"));
    }

    #[test]
    fn local_menu_keeps_unique_actions_when_details_are_open() {
        let mut state = crate::view::tests::empty_state();
        state.environment.details_expanded = true;
        state.environment.menu = EnvironmentMenu::Local;
        let frame = UiTree::new(build(&state, 380.0)).layout(Size::new(380.0, 1000.0));
        let mut ids = HashSet::new();
        for region in frame
            .regions
            .iter()
            .filter(|region| region.kind == HitKind::Button)
        {
            assert!(ids.insert(&region.id), "duplicate button {}", region.id);
        }
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "environment_choose_project"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "environment_local_files"));
    }
}

#[cfg(test)]
#[path = "environment_layout_tests.rs"]
mod layout_tests;
