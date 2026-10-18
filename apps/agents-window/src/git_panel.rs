//! The repository's changes as groups of files, with the controls of the
//! source control sidebar, and the diff of one file. The list is virtualized
//! and never embeds patch contents; rendering never runs Git.
use super::*;
use crate::git_review::{ChangeGroup, NOT_A_REPOSITORY};
use std::path::PathBuf;
use openide_ui::{
    Breadcrumb, BreadcrumbItem, CodeDiff, CodeLanguage, Collapse, Component, ComponentPalette,
    ContextMenu, ContextMenuEntry, ContextMenuItem, DiffStat, Notice, NoticeSizing, SectionHeader,
    Separator, Spinner, TextArea,
};

/// A row, and the row with the 2 px that set it off from the next.
const ROW_HEIGHT: f32 = 26.0;
const ROW_PITCH: f32 = ROW_HEIGHT + 2.0;
const ACTION: f32 = crate::sidebar_chrome::TOOL_SIZE;
const SCROLL_RAIL_WIDTH: f32 = 14.0;
/// The column of the status letter at the end of a file row.
const LETTER: f32 = 12.0;
/// Between a row's line counts and its status letter, besides the row's gap.
const STAT_END: f32 = 4.0;
/// Below this a file row has room for two actions; from it, for three.
const WIDE_ROW: f32 = 250.0;

/// The state of the actions on the pull request shown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PullControls {
    /// The comment being written, while its field is open.
    pub comment: Option<String>,
    pub merge_confirm: bool,
    /// The selected task watches it; `Some(true)` also wakes its agent.
    pub watch: Option<bool>,
    /// A task is selected that could watch it.
    pub watchable: bool,
}

/// What the panel keeps between frames besides the repository itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GitPanelView {
    /// Folded groups, by `ChangeGroup as usize`.
    pub collapsed: [bool; 4],
    /// The rows under the pointer and under the keyboard focus, as
    /// [`reveal_key`] names them: those show their actions.
    pub revealed: [Option<String>; 2],
    /// The dock tab under the pointer or the focus: there the mark of
    /// unsaved changes gives its place to the close button.
    pub tab_revealed: Option<u64>,
    pub confirm: Option<GitConfirmView>,
    /// The changes the user marked as viewed, as [`viewed_key`] names them.
    pub viewed: std::collections::BTreeSet<String>,
    /// The block of the open diff that its tools act on, counted from zero.
    pub hunk: usize,
    /// The pull request being written, while its form is open.
    pub pull: Option<crate::github::PullDraft>,
    /// What can be done to the pull request shown.
    pub controls: PullControls,
    /// What the repository tab lists the changes of.
    pub origin: ReviewOrigin,
}

/// Where the changes under review come from. The first three are the
/// repository's and are listed in its tab; the last two are the chat's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ReviewOrigin {
    #[default]
    Uncommitted,
    Branch,
    Commit,
    Chat,
    Turn,
}

/// The one selector both tabs of changes carry: it says which origin is in
/// view and takes to any other.
pub(crate) fn origin_selector(active: ReviewOrigin) -> Element {
    use ReviewOrigin::*;
    let origins = [
        (Uncommitted, "uncommitted", "Sin commitear"),
        (Branch, "branch", "Rama"),
        (Commit, "commit", "Commit"),
        (Chat, "chat", "Chat"),
        (Turn, "turn", "Último turno"),
    ];
    let button = |(origin, id, label): (ReviewOrigin, &str, &str)| {
        Button::new(format!("review_origin:{id}"), label, theme::action_palette(theme::BACKGROUND, theme::FOREGROUND))
            .variant(if origin == active { ButtonVariant::Subtle } else { ButtonVariant::Ghost })
            .sizing(crate::ui_sizing::SMALL_LABEL_BUTTON)
            .render()
    };
    // The repository's on one line and the chat's on the next: together
    // they do not fit the narrowest sidebar.
    let (repository, chat) = origins.split_at(3);
    Element::column(vec![
        Element::row(repository.iter().copied().map(button).collect()).gap(2.0),
        Element::row(chat.iter().copied().map(button).collect()).gap(2.0),
    ])
    .gap(2.0)
    .padding(4.0)
}

/// The files a branch or one commit changed, each opening its diff; before
/// there is a commit in view, the commits to choose from.
fn range_review(state: &ViewState, review: &GitReview, width: f32) -> Element {
    let commit = state.git_panel.origin == ReviewOrigin::Commit;
    let workflow = &state.environment.workflow;
    let palette = theme::action_palette(theme::BACKGROUND, theme::FOREGROUND);
    let note = |text: &str| UiText::new(text, theme::component_palette()).role(TextRole::Small).muted().render().padding(8.0);
    let button = |id: &str, label: &str| {
        Button::new(id, label, palette).variant(ButtonVariant::Ghost).sizing(crate::ui_sizing::SMALL_LABEL_BUTTON).render()
    };
    let mut rows = Vec::new();
    if let Some(error) = &workflow.error {
        rows.push(UiText::new(error, theme::component_palette()).role(TextRole::Small).ink(theme::DESTRUCTIVE).render().padding(8.0));
    }
    let Some(comparison) = workflow.comparison.as_ref().filter(|comparison| comparison.head.is_some() == commit) else {
        if workflow.loading {
            rows.push(note("Comparando…"));
        } else if !commit {
            rows.push(note("Elegí con qué rama comparar la actual."));
            rows.push(button("environment_git_open_compare", "Comparar con una rama…"));
        } else if review.recent.is_empty() {
            rows.push(note("Esta rama todavía no tiene commits."));
        } else {
            let recent = review.recent.clone();
            rows.push(
                ScrollArea::uniform_list("review_commits", recent.len(), ROW_PITCH, move |index| {
                    let (hash, subject) = &recent[index];
                    openide_ui::ListRow::new(format!("review_commit:{index}"), subject, theme::component_palette())
                        .meta(&hash[..hash.len().min(7)])
                        .width(width - 8.0)
                        .render()
                })
                .render()
                .flex(1.0),
            );
        }
        return Element::column(rows).gap(2.0).flex(1.0);
    };
    let title = match &comparison.head {
        Some(hash) => format!("Commit {}", &hash[..hash.len().min(7)]),
        None => format!("Contra {}", comparison.reference),
    };
    rows.push(Element::row(vec![
        UiText::new(format!("{title} · {} archivos", comparison.files), theme::component_palette())
            .role(TextRole::Small)
            .muted()
            .truncate()
            .render()
            .centered_y()
            .flex(1.0),
        DiffStat::new(comparison.additions, comparison.deletions, theme::component_palette())
            .inks(theme::DIFF_ADDED, theme::DIFF_REMOVED)
            .render()
            .centered_y(),
        if commit { button("review_commit_back", "Commits") } else { button("environment_git_open_compare", "Cambiar") },
    ])
    .gap(6.0)
    .padding(4.0));
    let changed = comparison.changed.clone();
    rows.push(
        ScrollArea::uniform_list("review_range_files", changed.len(), ROW_PITCH, move |index| {
            let file = &changed[index];
            openide_ui::ListRow::new(format!("environment_git_compare_file:{index}"), &file.path, theme::component_palette())
                .icon(openide_ui::file_icon(&file.path))
                .meta(format!("+{} −{}", file.additions, file.deletions))
                .width(width - 8.0)
                .render()
        })
        .render()
        .flex(1.0),
    );
    Element::column(rows).gap(2.0).flex(1.0)
}

/// A change as it was when it was marked viewed: its file, its section and
/// its size. One that changed again since is another, not yet viewed.
pub(crate) fn viewed_key(path: &str, staged: bool, additions: usize, deletions: usize) -> String {
    format!("{}:{additions}:{deletions}:{path}", if staged { "staged" } else { "local" })
}

/// Changes about to be thrown away, waiting for the user's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitConfirmView {
    pub workspace: String,
    /// Untracked files go to the trash; tracked ones are restored.
    pub trash: bool,
    pub paths: Vec<PathBuf>,
    /// Only this block of the one file, counted from zero, is thrown away.
    pub hunk: Option<usize>,
}

/// The row an action ID belongs to: `section:file` or `n:index` for a file,
/// `g:key` for a group's heading.
pub(crate) fn reveal_key(id: &str) -> Option<String> {
    let id = id.strip_suffix(":status").unwrap_or(id);
    let (operation, target) = id.split_once(':')?;
    match operation {
        "review_untracked_open" | "review_stage_untracked" => Some(format!("n:{target}")),
        "review_file_open" | "review_stage" | "review_unstage" | "review_discard"
        | "review_open_file" => Some(target.to_owned()),
        "review_group_toggle" | "review_stage_all" | "review_unstage_all"
        | "review_discard_all" => Some(format!("g:{target}")),
        _ => None,
    }
}

/// What every row of one frame's list is built from. Rows are built only
/// while in view, from the shared snapshot.
#[derive(Clone)]
struct ListContext {
    review: Arc<GitReview>,
    width: f32,
    /// A Git operation is running: actions that change files wait.
    pending: bool,
    surface: Color,
    collapsed: [bool; 4],
    revealed: [Option<String>; 2],
    /// The file whose diff is open.
    selected: Option<(String, DiffStage)>,
    viewed: Arc<std::collections::BTreeSet<String>>,
}

impl ListContext {
    fn reveals(&self, key: &str) -> bool {
        self.revealed.iter().flatten().any(|revealed| revealed == key)
    }
}

fn group_title(group: ChangeGroup) -> &'static str {
    match group {
        ChangeGroup::Conflicts => "Conflictos",
        ChangeGroup::Staged => "Preparados",
        ChangeGroup::Changes => "Cambios",
        ChangeGroup::Untracked => "Sin seguimiento",
    }
}

fn list_len(review: &GitReview, collapsed: &[bool; 4]) -> usize {
    ChangeGroup::ALL
        .into_iter()
        .map(|group| match review.group(group).len {
            0 => 0,
            _ if collapsed[group as usize] => 1,
            len => len + 1,
        })
        .sum()
}

/// What the list shows at `row`: a group's heading, or one of its files.
fn list_entry(
    review: &GitReview,
    collapsed: &[bool; 4],
    mut row: usize,
) -> Option<(ChangeGroup, Option<usize>)> {
    for group in ChangeGroup::ALL {
        let len = review.group(group).len;
        if len == 0 {
            continue;
        }
        if row == 0 {
            return Some((group, None));
        }
        row -= 1;
        if !collapsed[group as usize] {
            if row < len {
                return Some((group, Some(row)));
            }
            row -= len;
        }
    }
    None
}

/// A 24 px tool at the end of a row or of a group's heading.
fn row_tool(
    id: &str,
    icon: Icon,
    hint: &str,
    palette: ComponentPalette,
    enabled: bool,
) -> Element {
    UiIconButton::new(id, icon, palette)
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .radius(RadiusRole::Square)
        .enabled(enabled)
        .tooltip_with(
            TooltipSpec::new(hint)
                .placement(TooltipPlacement::Below)
                .palette(theme::component_palette()),
        )
        .render()
        .centered_y()
        .size(Some(ACTION), Some(ROW_HEIGHT))
}

/// A group's heading: it folds the group, and shows what can be done to all
/// of its files while the pointer or the focus is on it.
fn group_header(context: &ListContext, group: ChangeGroup) -> Element {
    let key = group.key();
    let palette = theme::action_palette(context.surface, theme::FOREGROUND);
    let actions: &[(&str, Icon, &str)] = match group {
        ChangeGroup::Conflicts => &[],
        ChangeGroup::Staged => &[("review_unstage_all", Icon::Minus, "Quitar todo de preparados")],
        ChangeGroup::Changes => &[
            ("review_discard_all", crate::icons::DISCARD, "Descartar todos los cambios…"),
            ("review_stage_all", Icon::Add, "Preparar todos los cambios"),
        ],
        ChangeGroup::Untracked => &[
            ("review_discard_all", Icon::Trash, "Enviar todos a la papelera…"),
            ("review_stage_all", Icon::Add, "Preparar todos"),
        ],
    };
    // The heading keeps its width whether or not its tools show.
    let reserved = ACTION * 2.0;
    let mut row = vec![SectionHeader::new(
        group_title(group),
        ComponentPalette {
            text: if group == ChangeGroup::Conflicts {
                theme::DESTRUCTIVE
            } else {
                theme::FOREGROUND_SUBTLE
            },
            ..palette
        },
    )
    .count(context.review.group(group).len)
    .disclosure(Some(!context.collapsed[group as usize]))
    .action(format!("review_group_toggle:{key}"))
    .truncate(true)
    .width((context.width - reserved).max(0.0))
    .sizing(crate::ui_sizing::CHANGE_SECTION)
    .render()];
    if context.reveals(&format!("g:{key}")) {
        row.push(Element::spacer().size(
            Some(reserved - ACTION * actions.len() as f32),
            Some(ROW_HEIGHT),
        ));
        row.extend(actions.iter().map(|(operation, icon, hint)| {
            row_tool(
                &format!("{operation}:{key}"),
                *icon,
                hint,
                palette,
                !context.pending,
            )
        }));
    }
    Element::row(row)
        .gap(0.0)
        .size(Some(context.width), Some(ROW_HEIGHT))
}

/// One changed file. At rest the end of the row holds its line counts and
/// its status letter; under the pointer or the focus the counts give their
/// place to the row's actions, and nothing else moves.
fn file_row(context: &ListContext, group: ChangeGroup, index: usize) -> Element {
    let review = &context.review;
    let range = review.group(group);
    let untracked = group == ChangeGroup::Untracked;
    let file = (!untracked)
        .then(|| review.sections.get(range.section)?.files.get(range.start + index))
        .flatten();
    let (path, status, additions, deletions) = match file {
        Some(file) => (
            file.path.as_str(),
            file.status.as_str(),
            file.additions,
            file.deletions,
        ),
        None => (
            review.untracked.get(index).map_or("", String::as_str),
            "Sin seguimiento",
            0,
            0,
        ),
    };
    let target = review.target(group, index);
    let open = if untracked {
        format!("review_untracked_open:{index}")
    } else {
        format!("review_file_open:{target}")
    };
    let stage = match group {
        ChangeGroup::Untracked => format!("review_stage_untracked:{index}"),
        ChangeGroup::Staged => format!("review_unstage:{target}"),
        _ => format!("review_stage:{target}"),
    };
    let (stage_icon, stage_label) = match group {
        ChangeGroup::Conflicts => (Icon::Check, "Marcar como resuelto"),
        ChangeGroup::Staged => (Icon::Minus, "Quitar de preparados"),
        _ => (Icon::Add, "Preparar archivo"),
    };
    let discard = match group {
        ChangeGroup::Changes => Some((crate::icons::DISCARD, "Descartar cambios…")),
        ChangeGroup::Untracked => Some((Icon::Trash, "Enviar a la papelera…")),
        _ => None,
    };
    let deleted = status == "Eliminado";
    let open_file = format!("review_open_file:{target}");
    let discard_id = format!("review_discard:{target}");

    let stage_of_diff = if group == ChangeGroup::Staged {
        DiffStage::Staged
    } else {
        DiffStage::Unstaged
    };
    let selected = context
        .selected
        .as_ref()
        .is_some_and(|(selected, stage)| selected == path && *stage == stage_of_diff);
    let mut palette = theme::action_palette(context.surface, theme::FOREGROUND);
    if selected {
        palette.surface = theme::SELECTED;
    }
    let sizing = crate::ui_sizing::CHANGE_ROW;
    let inner_height = ROW_HEIGHT - sizing.padding * 2.0;
    let slots = if context.width >= WIDE_ROW { 3.0 } else { 2.0 };
    let zone = ACTION * slots;

    let file_path = Path::new(path);
    let filename = file_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    let parent = file_path
        .parent()
        .and_then(|parent| parent.to_str())
        .filter(|parent| !parent.is_empty());
    let (code, color) = crate::git_decorations::status_mark(group, status);
    let letter = || {
        Element::truncated_text(code)
            .font_family(FontFamily::Monospace)
            .foreground(color)
            .font_size(11.0)
            .size(Some(LETTER), Some(inner_height))
    };
    let mut row = ListRow::new(&open, filename, palette)
        .icon(openide_ui::file_icon(path))
        .icon_ink(theme::ICON_PRIMARY)
        .filled();
    // A change already viewed steps back; the ones left to read stand out.
    let viewed = context.viewed.contains(&viewed_key(path, group == ChangeGroup::Staged, additions, deletions));
    if deleted || viewed {
        row = row.title_ink(theme::FOREGROUND_SUBTLE);
    }
    if let Some(parent) = parent.filter(|_| context.width >= 320.0) {
        row = row.meta_element(
            Element::truncated_text(parent)
                .font_family(FontFamily::Sans)
                .font_size(11.0)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some((context.width * 0.28).min(160.0)), Some(inner_height)),
        );
    }
    if context.reveals(&target) {
        let mut tools = Vec::new();
        if !deleted {
            tools.push((open_file.as_str(), Icon::File, "Abrir archivo", true));
        }
        if let Some((icon, hint)) = discard {
            tools.push((discard_id.as_str(), icon, hint, !context.pending));
        }
        tools.push((stage.as_str(), stage_icon, stage_label, !context.pending));
        // The narrow row keeps what changes the file: opening it is the
        // row's own click, and the menu has the rest.
        let skip = tools.len().saturating_sub(slots as usize);
        let tools = &tools[skip..];
        let lead = zone - ACTION * tools.len() as f32;
        if lead > 0.0 {
            // Inside the row's own target, so no part of the row is dead.
            row = row.meta_element(
                Element::spacer().size(Some(lead - sizing.meta_gap), Some(inner_height)),
            );
        }
        for (id, icon, hint, enabled) in tools {
            // The row's shared surface is the tool's hover.
            row = row.trailing_action(*id, row_tool(id, *icon, hint, theme::in_row(palette), *enabled));
        }
        // The letter stays where it is at rest: 2 from the row's end.
        let status_id = format!("{open}:status");
        row = row.trailing_action(
            status_id.clone(),
            Tooltip::new(
                Element::row(vec![
                    Element::spacer()
                        .size(Some(sizing.meta_gap * 2.0 - sizing.padding), Some(ROW_HEIGHT)),
                    letter().centered_y(),
                    Element::spacer().size(Some(sizing.padding), Some(ROW_HEIGHT)),
                ])
                .gap(0.0)
                .size(Some(LETTER + sizing.meta_gap * 2.0), Some(ROW_HEIGHT)),
                status_id,
                status,
                theme::component_palette(),
            )
            .placement(TooltipPlacement::Below)
            .render(),
        );
    } else {
        // Counts that do not fit the zone stay in the hint.
        let figures = additions.to_string().len() + deletions.to_string().len() + 2;
        let stat = if additions + deletions > 0 && figures as f32 * 7.0 + 5.0 + STAT_END <= zone {
            Element::row(vec![
                Element::spacer().flex(1.0),
                DiffStat::new(additions, deletions, theme::component_palette())
                    .inks(theme::DIFF_ADDED, theme::DIFF_REMOVED)
                    .render()
                    .centered_y(),
                // The letter is a column of its own, not a third figure.
                Element::spacer().size(Some(STAT_END), None),
            ])
        } else {
            Element::spacer()
        };
        row = row
            .meta_element(stat.size(Some(zone), Some(inner_height)))
            .meta_element(letter());
    }
    let mut details = Vec::new();
    if let Some(parent) = parent {
        details.push(parent.to_owned());
    }
    details.push(if additions + deletions > 0 {
        format!("{status} · +{additions} −{deletions}")
    } else {
        status.to_owned()
    });
    let row = row
        .tooltip_with(
            TooltipSpec::new(filename)
                .description(details.join("\n"))
                .placement(if context.surface == theme::SIDEBAR {
                    TooltipPlacement::Right
                } else {
                    TooltipPlacement::Left
                })
                .palette(theme::component_palette()),
        )
        .sizing(sizing)
        .render()
        .size(Some(context.width), Some(ROW_HEIGHT));

    let item = |id: &str, label: &str, enabled: bool| {
        ContextMenuEntry::Item(ContextMenuItem::new(id, label).enabled(enabled))
    };
    let mut entries = vec![
        item(&open, "Abrir cambios", true),
        item(&open_file, "Abrir archivo", !deleted),
        ContextMenuEntry::Separator,
        item(&stage, stage_label, !context.pending),
    ];
    if let Some((_, label)) = discard {
        entries.push(item(&discard_id, label, !context.pending));
    }
    entries.extend([
        ContextMenuEntry::Separator,
        item(&format!("review_copy_path:{target}"), "Copiar ruta", true),
        item(
            &format!("review_copy_relative:{target}"),
            "Copiar ruta relativa",
            true,
        ),
    ]);
    ContextMenu::new(
        format!("review_menu:{target}"),
        row,
        entries,
        theme::menu_palette(),
    )
    .render()
}

fn change_list(context: ListContext) -> Element {
    let count = list_len(&context.review, &context.collapsed);
    let surface = context.surface;
    let context = ListContext {
        width: (context.width - SCROLL_RAIL_WIDTH).max(0.0),
        ..context
    };
    ScrollArea::uniform_list("git_change_files", count, ROW_PITCH, move |index| {
        let row = match list_entry(&context.review, &context.collapsed, index) {
            Some((group, None)) => group_header(&context, group),
            Some((group, Some(file))) => file_row(&context, group, file),
            None => Element::spacer(),
        };
        // A list without overflow gives its row the full viewport. Keep the
        // rail budget stable there too.
        Element::row(vec![row, Element::spacer().flex(1.0)])
    })
    .tint(theme::FOREGROUND_SUBTLE)
    .render()
    .top_fade(surface, 12.0)
    .flex(1.0)
}

/// The first files of a confirmation, one per line, and how many more.
fn named_files(paths: &[PathBuf]) -> String {
    let mut names: Vec<String> = paths
        .iter()
        .take(5)
        .map(|path| format!("«{}»", path.display()))
        .collect();
    if paths.len() > 5 {
        names.push(format!("y {} más", paths.len() - 5));
    }
    names.join("\n")
}

/// Throwing changes away asks first, naming the files.
pub(super) fn discard_dialog(state: &ViewState, base: Element, width: f32) -> Element {
    let Some(confirm) = state
        .git_panel
        .confirm
        .as_ref()
        .filter(|confirm| confirm.workspace == state.workspace && !confirm.paths.is_empty())
    else {
        return base;
    };
    let count = confirm.paths.len();
    let names = named_files(&confirm.paths);
    let (title, description, label) = match (confirm.trash, count) {
        (false, 1) if confirm.hunk.is_some() => (
            "¿Descartar este bloque?".to_owned(),
            format!(
                "Se perderá el bloque {} de los cambios sin preparar de {names}. No se puede deshacer.",
                confirm.hunk.unwrap_or(0) + 1
            ),
            "Descartar bloque",
        ),
        (true, 1) => (
            "¿Enviar a la papelera?".to_owned(),
            format!("{names} no tiene seguimiento de Git e irá a la papelera del sistema."),
            "Enviar a la papelera",
        ),
        (true, _) => (
            format!("¿Enviar {count} archivos a la papelera?"),
            format!("No tienen seguimiento de Git e irán a la papelera del sistema:\n{names}"),
            "Enviar a la papelera",
        ),
        (false, 1) => (
            "¿Descartar los cambios?".to_owned(),
            format!("Se perderán los cambios sin preparar de {names}. No se puede deshacer."),
            "Descartar",
        ),
        (false, _) => (
            format!("¿Descartar los cambios de {count} archivos?"),
            format!("Se perderán sus cambios sin preparar. No se puede deshacer.\n{names}"),
            "Descartar todo",
        ),
    };
    openide_ui::AlertDialog::new(
        base,
        true,
        title,
        description,
        "review_discard_confirm",
        "review_discard_cancel",
        theme::component_palette(),
    )
    .width((width - 24.0).min(440.0))
    .confirm_label(label)
    .cancel_label("Cancelar")
    .destructive(theme::DESTRUCTIVE)
    .render()
}

/// What the repository can exchange with its remote right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SyncStep {
    /// The branch has no upstream and there is a remote to publish it on.
    Publish,
    Pull { behind: usize, ahead: usize },
    Push { ahead: usize },
    /// Nothing known to exchange: look for news.
    Fetch,
}

/// The one exchange the sidebar offers first, or none without a remote.
pub(super) fn sync_step(state: &ViewState, review: &GitReview) -> Option<SyncStep> {
    let snapshot = state.environment.workflow.snapshot.as_ref();
    let (ahead, behind) = match (snapshot, &review.tracking) {
        (Some(snapshot), _) if snapshot.upstream.is_some() => (snapshot.ahead, snapshot.behind),
        (None, Some(tracking)) => (tracking.ahead, tracking.behind),
        _ => (0, 0),
    };
    let tracked = snapshot.map_or(review.tracking.is_some(), |snapshot| snapshot.upstream.is_some());
    if !tracked {
        // Without the snapshot nothing says there is a remote.
        let snapshot = snapshot?;
        return (!snapshot.remotes.is_empty()).then(|| {
            if snapshot.current_branch().is_some() && snapshot.head.is_some() {
                SyncStep::Publish
            } else {
                SyncStep::Fetch
            }
        });
    }
    Some(if behind > 0 {
        SyncStep::Pull { behind, ahead }
    } else if ahead > 0 {
        SyncStep::Push { ahead }
    } else {
        SyncStep::Fetch
    })
}

fn commits(count: usize) -> String {
    format!("{count} {}", if count == 1 { "commit" } else { "commits" })
}

/// Why nothing that changes the repository can start now.
fn locked_reason(state: &ViewState) -> Option<&'static str> {
    if state.running || state.background_run_selected {
        Some("El agente está trabajando")
    } else if state.composer_branches.switching() {
        Some("Cambiando de rama…")
    } else if state.git_action_pending || state.environment.workflow.busy.is_some() {
        Some("Operación de Git en curso")
    } else {
        None
    }
}

/// The branch as a control, and beside it the next exchange with the remote.
fn branch_bar(state: &ViewState, review: &GitReview, width: f32) -> Element {
    let palette = theme::sidebar_palette();
    let workflow = &state.environment.workflow;
    let snapshot = workflow.snapshot.as_ref();
    let upstream = snapshot
        .and_then(|snapshot| snapshot.upstream.clone())
        .or_else(|| review.tracking.as_ref().map(|tracking| tracking.upstream.clone()));
    let remote = snapshot
        .and_then(|snapshot| snapshot.default_remote())
        .unwrap_or("el remoto");
    let locked = locked_reason(state);
    let push_blocked = snapshot.and_then(|snapshot| {
        crate::environment_git_ui::push_blocked_reason(workflow, snapshot, locked.is_some())
    });
    let step = sync_step(state, review);
    let sync = step.as_ref().map(|step| {
        let hint = |title: String, description: String| {
            TooltipSpec::new(title)
                .description(description)
                .placement(TooltipPlacement::Below)
                .palette(theme::component_palette())
        };
        let upstream = upstream.as_deref().unwrap_or(remote);
        let (action, icon, label, title, blocked) = match step {
            SyncStep::Publish => (
                "environment_git_push",
                Icon::ArrowUp,
                "Publicar".to_owned(),
                format!("Publicar la rama en {remote}"),
                push_blocked,
            ),
            SyncStep::Push { ahead } => (
                "environment_git_push",
                Icon::ArrowUp,
                format!("Enviar {ahead}"),
                format!("Enviar {} a {upstream}", commits(*ahead)),
                push_blocked,
            ),
            SyncStep::Pull { behind, .. } => (
                "environment_git_pull",
                Icon::ArrowDown,
                format!("Recibir {behind}"),
                format!("Recibir {} de {upstream}", commits(*behind)),
                locked,
            ),
            SyncStep::Fetch => (
                "environment_git_fetch",
                crate::icons::SYNC,
                String::new(),
                format!("Buscar cambios en {remote}"),
                (workflow.busy.is_some() || state.git_action_pending)
                    .then_some("Operación de Git en curso"),
            ),
        };
        let description = match (blocked, step) {
            (Some(reason), _) => reason.to_owned(),
            (None, SyncStep::Pull { ahead, .. }) if *ahead > 0 => format!(
                "Solo avance rápido: la rama también tiene {} por enviar.",
                commits(*ahead)
            ),
            (None, SyncStep::Pull { .. }) => "Solo avance rápido (fast-forward).".to_owned(),
            (None, SyncStep::Fetch) if step == &SyncStep::Fetch && snapshot.is_some() => {
                match snapshot.and_then(|snapshot| snapshot.upstream.as_deref()) {
                    Some(upstream) => format!("Al día con {upstream}."),
                    None => "Actualiza las referencias remotas.".to_owned(),
                }
            }
            _ => String::new(),
        };
        let enabled = blocked.is_none() && snapshot.is_some();
        if label.is_empty() {
            UiIconButton::new("ide_git_sync", icon, palette)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .enabled(enabled)
                .dispatch_as(action)
                .tooltip_with(hint(title, description))
                .render()
                .centered_y()
        } else {
            Button::new("ide_git_sync", label, palette)
                .icon(icon)
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Dense)
                .enabled(enabled)
                .dispatch_as(action)
                .tooltip_with(hint(title, description))
                .render()
                .centered_y()
        }
    });
    let branch = Button::new("ide_git_branch_toggle", &review.branch, palette)
        .icon(Icon::GitBranch)
        .trailing_icon(Icon::ChevronDown)
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Compact)
        .enabled(!state.composer_branches.switching())
        .tooltip_with(
            TooltipSpec::new(&review.branch)
                .description(match &upstream {
                    Some(upstream) => format!("Sigue a {upstream}. Cambiar de rama…"),
                    None => "Sin rama de seguimiento. Cambiar de rama…".to_owned(),
                })
                .placement(TooltipPlacement::Below)
                .palette(theme::component_palette())
                .suppressed(state.composer_branches.open),
        )
        .render()
        .flex(1.0);
    // The picker is the composer's: one snapshot, one list of branches.
    let mut props = ComposerProps::from_state(state, width.max(320.0) + 16.0, false, None, "", None);
    props.branch_menu_open = state.composer_branches.open
        && state.environment.menu == crate::environment_ui::EnvironmentMenu::Branch
        && !state.environment.open;
    let branch = crate::composer_ui::branch_selector(
        &props,
        branch,
        PopoverPlacement::BottomStart,
        "ide_git_branch_toggle",
    )
    .flex(1.0);
    Element::row(std::iter::once(branch).chain(sync).collect())
        .gap(6.0)
        .size(Some(width), Some(ControlSize::Compact.height()))
}

/// Why the commit button is off, in a few words; `None` when it can commit.
pub(super) fn commit_blocked(state: &ViewState, review: &GitReview) -> Option<String> {
    let workflow = &state.environment.workflow;
    if let Some(reason) = locked_reason(state) {
        Some(reason.to_owned())
    } else if review.conflicts > 0 {
        Some(format!(
            "Resolvé {} {}",
            review.conflicts,
            if review.conflicts == 1 { "conflicto" } else { "conflictos" }
        ))
    } else if review.group(ChangeGroup::Staged).len == 0 {
        Some("Sin archivos preparados".to_owned())
    } else if workflow.commit_message.trim().is_empty() {
        Some("Falta el mensaje".to_owned())
    } else if workflow.snapshot.is_none() {
        Some("Leyendo el repositorio…".to_owned())
    } else {
        None
    }
}

/// The message of the next commit and the button that creates it.
fn commit_composer(state: &ViewState, review: &GitReview, width: f32) -> Element {
    let workflow = &state.environment.workflow;
    let staged = review.group(ChangeGroup::Staged).len;
    let blocked = commit_blocked(state, review);
    // While Git works the status line says so; here stays what will go in.
    let working = workflow.busy.is_some() || state.git_action_pending;
    let summary = blocked.clone().filter(|_| !working || staged == 0).unwrap_or_else(|| {
        format!(
            "{staged} {}",
            if staged == 1 { "archivo preparado" } else { "archivos preparados" }
        )
    });
    let commit = Button::new("ide_git_commit", "Commit", theme::sidebar_palette())
        .icon(Icon::Check)
        .size(ControlSize::Dense)
        .enabled(blocked.is_none())
        .dispatch_as("environment_git_commit")
        .tooltip_with(
            TooltipSpec::new(match &blocked {
                Some(reason) => reason.clone(),
                None => format!("Crear un commit en {}", review.branch),
            })
            .shortcut("Ctrl+Enter")
            .placement(TooltipPlacement::Below)
            .palette(theme::component_palette()),
        )
        .render();
    Element::column(vec![
        TextArea::new(
            "environment_git_commit_message",
            &workflow.commit_message,
            "Mensaje del commit",
            theme::action_palette(theme::BACKGROUND, theme::FOREGROUND),
        )
        .width(width)
        .height(76.0)
        .radius(RadiusRole::Surface)
        .enabled(workflow.busy.is_none())
        .render(),
        Element::row(vec![
            UiText::new(summary, theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .truncate()
                .render()
                .centered_y()
                .flex(1.0),
            commit,
        ])
        .gap(6.0)
        .size(Some(width), Some(ControlSize::Dense.height())),
    ])
    .gap(6.0)
}

/// What Git is doing, or what it last said: one slot over the list.
fn status_line(state: &ViewState, width: f32) -> Option<Element> {
    let workflow = &state.environment.workflow;
    let palette = theme::component_palette();
    let dismiss = || {
        UiIconButton::new("review_error_dismiss", Icon::Close, theme::sidebar_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .tooltip_with(TooltipSpec::new("Descartar aviso").placement(TooltipPlacement::Below))
            .render()
    };
    let busy = workflow.busy.as_deref().or(state
        .git_action_pending
        .then_some("Actualizando archivos…"));
    if let Some(busy) = busy {
        return Some(
            Element::row(vec![
                Spinner::new("review_busy", theme::FOREGROUND_SUBTLE)
                    .size(14.0)
                    .render()
                    .centered_y(),
                UiText::new(busy, palette)
                    .role(TextRole::Small)
                    .muted()
                    .truncate()
                    .render()
                    .centered_y()
                    .flex(1.0),
            ])
            .gap(8.0)
            .padding(4.0)
            .size(Some(width), Some(ACTION + 8.0)),
        );
    }
    let error = state.git_action_error.as_deref().or(workflow.error.as_deref());
    if let Some(error) = error {
        // Git's own words, bounded: a hook can print pages.
        let text: String = error.trim().chars().take(600).collect();
        return Some(
            Notice::new("Git no completó la operación", palette)
                .description(text)
                .icon(Icon::WarningTriangle)
                .ink(theme::DESTRUCTIVE)
                .trailing(dismiss())
                .sizing(NoticeSizing::COMPACT)
                .width(width)
                .render(),
        );
    }
    workflow.notice.as_deref().map(|notice| {
        Notice::new(notice, palette)
            .icon(Icon::CheckCircle)
            .trailing(dismiss())
            .sizing(NoticeSizing::COMPACT)
            .width(width)
            .render()
    })
}

/// Merge, approve, comment and watch, for the pull request shown.
fn pull_controls(number: u64, controls: &PullControls, palette: openide_ui::ComponentPalette) -> Vec<Element> {
    let button = |id: &str, label: &str| {
        Button::new(id, label, palette)
            .variant(ButtonVariant::Ghost)
            .sizing(crate::ui_sizing::SMALL_LABEL_BUTTON)
            .render()
    };
    let switch = |id: &str, label: &str, on: bool| {
        Element::row(vec![
            openide_ui::Switch::new(id, on, theme::component_palette()).render(),
            UiText::new(label, theme::component_palette()).role(TextRole::Micro).render().centered_y(),
        ])
        .gap(6.0)
    };
    let mut rows = Vec::new();
    if controls.merge_confirm {
        rows.push(
            UiText::new(format!("¿Fusionar #{number}?"), theme::component_palette())
                .role(TextRole::Micro)
                .render(),
        );
        rows.push(Element::row(vec![
            button("github_pr_merge_squash", "Squash"),
            button("github_pr_merge_commit", "Commit de merge"),
            button("github_pr_merge_cancel", "Cancelar"),
        ]));
    } else {
        rows.push(Element::row(vec![
            button("github_pr_merge", "Fusionar"),
            button("github_pr_approve", "Aprobar"),
            button("github_pr_comment", "Comentar"),
        ]));
    }
    if let Some(comment) = &controls.comment {
        rows.push(
            TextArea::new(
                "github_comment_body",
                comment,
                "Comentario para el pull request",
                theme::action_palette(theme::BACKGROUND, theme::FOREGROUND),
            )
            .height(64.0)
            .radius(RadiusRole::Surface)
            .render(),
        );
        rows.push(Element::row(vec![
            button("github_comment_submit", "Publicar"),
            button("github_comment_cancel", "Cancelar"),
        ]));
    }
    if controls.watchable {
        rows.push(switch("github_pr_watch", "Vigilar CI y comentarios desde esta tarea", controls.watch.is_some()));
        if let Some(wake) = controls.watch {
            rows.push(switch("github_pr_watch_wake", "Pasarle las novedades al agente", wake));
        }
    }
    rows
}

fn github_footer(
    status: &GithubStatus,
    pull: Option<&crate::github::PullDraft>,
    controls: &PullControls,
    surface: Color,
) -> Option<Element> {
    let palette = theme::action_palette(surface, theme::FOREGROUND);
    match status {
        GithubStatus::Ready(github) => {
            let mut rows = vec![Button::new(
                "github_repo_open",
                github.repository.full_name(),
                palette,
            )
            .icon(Icon::Github)
            .variant(ButtonVariant::Ghost)
            .sizing(crate::ui_sizing::SMALL_LABEL_BUTTON)
            .tooltip_with(
                TooltipSpec::new("Abrir repositorio en GitHub").palette(theme::component_palette()),
            )
            .render()];
            if let Some(pull) = github.pulls.first() {
                rows.push(
                    Button::new(
                        format!("github_pr_open:{}", pull.number),
                        format!("#{} {}", pull.number, shorten(&pull.title, 46)),
                        palette,
                    )
                    .variant(ButtonVariant::Ghost)
                    .sizing(crate::ui_sizing::SMALL_LABEL_BUTTON)
                    .render(),
                );
                if let Some(checks) = pull.checks {
                    use crate::github::PullChecks;
                    rows.push(
                        UiText::new(checks.label(), theme::component_palette())
                            .role(TextRole::Micro)
                            .ink(match checks {
                                PullChecks::Failing => theme::DESTRUCTIVE,
                                PullChecks::Passing => theme::DIFF_ADDED,
                                PullChecks::Pending | PullChecks::None => theme::FOREGROUND_SUBTLE,
                            })
                            .render(),
                    );
                }
                rows.extend(pull_controls(pull.number, controls, palette));
            } else if github.branch != "HEAD separado" {
                let button = |id: &str, label: &str| {
                    Button::new(id, label, palette)
                        .variant(ButtonVariant::Ghost)
                        .sizing(crate::ui_sizing::SMALL_LABEL_BUTTON)
                        .render()
                };
                match pull {
                    None => rows.push(button("github_pr_create", "Crear pull request")),
                    // Whatever is left empty is filled in when it is sent.
                    Some(pull) => {
                        let field = |id: &str, value: &str, hint: &str| {
                            openide_ui::TextField::new(id, value, hint, theme::component_palette()).size(ControlSize::Regular).render()
                        };
                        rows.push(field("github_pr_title", &pull.title, "Título (vacío: el del último commit)"));
                        rows.push(
                            TextArea::new(
                                "github_pr_body",
                                &pull.body,
                                "Descripción (vacía: la del último commit)",
                                theme::action_palette(theme::BACKGROUND, theme::FOREGROUND),
                            )
                            .height(64.0)
                            .radius(RadiusRole::Surface)
                            .render(),
                        );
                        rows.push(field("github_pr_base", &pull.base, "Rama base (vacía: la principal)"));
                        rows.push(Element::row(vec![
                            button("github_pr_submit", "Crear"),
                            button("github_pr_submit_draft", "Crear borrador"),
                            button("github_pr_cancel", "Cancelar"),
                        ]));
                    }
                }
            }
            // Keep repository access issues visible without making them the
            // first thing above local changes, which work independently.
            if let Some(notice) = &github.notice {
                rows.push(
                    UiText::new(notice, theme::component_palette())
                        .role(TextRole::Micro)
                        .muted()
                        .render(),
                );
            }
            Some(Element::column(rows).gap(3.0).padding(8.0))
        }
        GithubStatus::Loading => Some(
            UiText::new("Consultando GitHub…", theme::component_palette())
                .role(TextRole::Micro)
                .muted()
                .render()
                .padding(8.0),
        ),
        GithubStatus::Unavailable(error) => Some(
            UiText::new(error, theme::component_palette())
                .role(TextRole::Micro)
                .muted()
                .render()
                .padding(8.0),
        ),
        GithubStatus::NoRemote => None,
    }
}

#[cfg(test)]
fn detail_content(detail: &ReviewDetailView, width: f32) -> Element {
    detail_view(detail, width, true, &Default::default())
}

/// `back` adds the way out to the list of changes. The IDE keeps that list in
/// its sidebar, so there a diff is a document on its own.
fn detail_view(detail: &ReviewDetailView, width: f32, back: bool, panel: &GitPanelView) -> Element {
    let viewed = &panel.viewed;
    let stage = match detail.stage {
        DiffStage::Staged => "Preparado",
        DiffStage::Unstaged => "Local",
        DiffStage::Branch => "Rama",
    };
    let summary = match &detail.content {
        Some(Ok(Some(diff))) => format!(
            "{} · {} · +{} −{}{}",
            diff.file.status,
            stage,
            diff.file.additions,
            diff.file.deletions,
            diff.file
                .old_path
                .as_ref()
                .filter(|old| *old != &diff.file.path)
                .map(|old| format!(" · Desde {old}"))
                .unwrap_or_default()
        ),
        _ => stage.to_owned(),
    };
    let mut path_items: Vec<_> = Path::new(&detail.path)
        .components()
        .map(|component| {
            BreadcrumbItem::new(component.as_os_str().to_string_lossy()).tooltip(&detail.path)
        })
        .collect();
    if let Some(item) = path_items.pop() {
        path_items.push(
            item.icon(openide_ui::file_icon(&detail.path))
                .tooltip(format!("{} · {summary}", detail.path)),
        );
    }
    let tool_size = crate::dock_tab_ui::TOOL_SIZE;
    let stats = detail.content.as_ref().and_then(|content| match content {
        Ok(Some(diff)) => Some((
            format!("+{}", diff.file.additions),
            format!("−{}", diff.file.deletions),
        )),
        _ => None,
    });
    // Reserve a conservative budget, but let native font measurements hug the
    // path and metadata. Spare space belongs before the toolbar, not between
    // a filename and its change counts.
    let fixed_width =
        8.0 + if back { 56.0 + 14.0 } else { 0.0 } + tool_size * 2.0 + 4.0 * 5.0;
    let stats_width = stats.as_ref().map_or(0.0, |(added, removed)| {
        (stage.len() + added.chars().count() + removed.chars().count()) as f32 * 7.8 + 16.0
    });
    let show_stats = stats.is_some() && width - fixed_width - 17.0 - stats_width >= 96.0;
    let path_width = (width - fixed_width
        - if show_stats { stats_width + 17.0 } else { 0.0 })
        .max(0.0);
    let mut header = Vec::new();
    if back {
        // This navigates out of the diff, rather than into a path ancestor.
        // Keep it reachable when the filename consumes the narrow header.
        header.push(
            Breadcrumb::new(
                "review_detail_navigation",
                vec![BreadcrumbItem::new("Cambios")
                    .action("review_detail_back")
                    .tooltip("Volver a los cambios del repositorio")],
                theme::component_palette(),
            )
            .width(56.0)
            .height(crate::dock_tab_ui::TOOL_SIZE)
            .render(),
        );
        header.push(
            Element::icon(Icon::ChevronRight)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(14.0), Some(tool_size)),
        );
    }
    header.push(
        Breadcrumb::new("review_detail_path", path_items, theme::component_palette())
            .width(path_width)
            .height(tool_size)
            .render()
            .size(None, Some(tool_size))
            .max_width(path_width),
    );
    if show_stats {
        if let Some((added, removed)) = stats {
            header.push(
                Separator::vertical(theme::component_palette())
                    .length(12.0)
                    .centered_in(tool_size)
                    .render()
                    .padding(4.0)
                    .size(Some(9.0), Some(tool_size)),
            );
            let text = |value, color, family| {
                Element::truncated_text(value)
                    .foreground(color)
                    .font_family(family)
                    .font_size(12.0)
                    .size(None, Some(tool_size))
            };
            header.push(
                Element::row(vec![
                    text(stage.to_owned(), theme::FOREGROUND_SUBTLE, FontFamily::Sans),
                    text(added, theme::DIFF_ADDED, FontFamily::Monospace),
                    text(removed, theme::DIFF_REMOVED, FontFamily::Monospace),
                ])
                .gap(8.0)
                .size(None, Some(tool_size))
                .max_width(stats_width)
                .clip_children(),
            );
        }
    }
    header.push(Element::spacer().flex(1.0));
    // Once its diff is read, a change can be marked viewed: it steps back in the list.
    if let Some(Ok(Some(diff))) = &detail.content {
        let seen = viewed.contains(&viewed_key(&detail.path, detail.stage == DiffStage::Staged, diff.file.additions, diff.file.deletions));
        header.push(
            UiIconButton::new(
                "review_detail_viewed",
                Icon::Check,
                theme::action_palette(theme::BACKGROUND, if seen { theme::DIFF_ADDED } else { theme::FOREGROUND_SUBTLE }),
            )
            .variant(ButtonVariant::Subtle)
            .size(ControlSize::Dense)
            .tooltip_with(
                TooltipSpec::new(if seen { "Marcar como no visto" } else { "Marcar como visto" }).placement(TooltipPlacement::Below),
            )
            .render(),
        );
    }
    header.extend([
        UiIconButton::new(
            "review_detail_open_editor",
            Icon::Code,
            theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Dense)
        .tooltip_with(TooltipSpec::new("Abrir en editor").placement(TooltipPlacement::Below))
        .render(),
        UiIconButton::new(
            "review_detail_retry",
            Icon::Refresh,
            theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Dense)
        .tooltip_with(TooltipSpec::new("Actualizar diff").placement(TooltipPlacement::Below))
        .render(),
    ]);
    let mut children = vec![Element::panel(
        Element::row(header)
            .gap(4.0)
            .size(None, Some(crate::dock_tab_ui::TOOL_SIZE)),
    )
    .padding(4.0)
    .size(None, Some(crate::dock_tab_ui::HEADER_HEIGHT))];
    match &detail.content {
        None => children.push(pane_loading_state("review_detail", "Cargando diff", width)),
        Some(Err(error)) => children.push(pane_empty_state(
            Icon::Code,
            "No se pudo abrir el diff",
            error,
            Some(("review_detail_retry", "Reintentar")),
        )),
        Some(Ok(None)) => children.push(pane_empty_state(
            Icon::Check,
            "Sin diferencias",
            "El archivo ya no tiene cambios en esta sección.",
            Some(("review_detail_back", "Ver cambios")),
        )),
        Some(Ok(Some(diff))) => {
            let file = &diff.file;
            if diff.editor_lines.is_empty() {
                children.push(pane_empty_state(
                    Icon::File,
                    &file.status,
                    "Este cambio no contiene diferencias de texto.",
                    None,
                ));
                children.push(Element::spacer().flex(1.0));
            } else {
                // The blocks of an unstaged change can be thrown away one at a time.
                let blocks: Vec<&str> = file
                    .lines
                    .iter()
                    .filter(|line| line.kind == crate::git_review::ReviewLineKind::Hunk)
                    .map(|line| line.text.as_str())
                    .collect();
                if detail.stage == DiffStage::Unstaged && !blocks.is_empty() && !diff.truncated {
                    let current = panel.hunk.min(blocks.len() - 1);
                    let tool = |id: &str, icon, hint: &str, enabled| {
                        UiIconButton::new(id, icon, theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE))
                            .variant(ButtonVariant::Subtle)
                            .size(ControlSize::Dense)
                            .enabled(enabled)
                            .tooltip_with(TooltipSpec::new(hint).placement(TooltipPlacement::Below))
                            .render()
                    };
                    children.push(
                        Element::row(vec![
                            tool("review_hunk_previous", Icon::ArrowLeft, "Bloque anterior", current > 0),
                            tool("review_hunk_next", Icon::ArrowRight, "Bloque siguiente", current + 1 < blocks.len()),
                            UiText::new(format!("Bloque {} de {}", current + 1, blocks.len()), theme::component_palette())
                                .role(TextRole::Small)
                                .render()
                                .centered_y(),
                            Element::truncated_text(blocks[current])
                                .font_family(FontFamily::Monospace)
                                .font_size(11.0)
                                .foreground(theme::FOREGROUND_SUBTLE)
                                .flex(1.0)
                                .centered_y(),
                            Button::new("review_hunk_discard", "Descartar bloque…", theme::action_palette(theme::BACKGROUND, theme::FOREGROUND))
                                .variant(ButtonVariant::Ghost)
                                .sizing(crate::ui_sizing::SMALL_LABEL_BUTTON)
                                .render(),
                        ])
                        .gap(6.0)
                        .padding(4.0)
                        .size(None, Some(crate::dock_tab_ui::HEADER_HEIGHT)),
                    );
                }
                let language = CodeLanguage::from_extension(
                    Path::new(&file.path)
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .unwrap_or_default(),
                );
                children.push(
                    CodeDiff::new(
                        format!("git-diff:{:?}:{}", detail.stage, detail.path),
                        diff.editor_lines.clone(),
                    )
                    .language(language)
                    .palette(theme::component_palette())
                    .font_size(crate::appearance_preferences::code_font_size())
                        .diff_backgrounds(crate::appearance_preferences::themed_diff_backgrounds())
                        .render()
                    .background(theme::BACKGROUND)
                    .size(None, None)
                    .min_size(0.0, 80.0)
                    .flex(1.0),
                );
            }
            if diff.truncated {
                children.push(
                    UiText::new(
                        "Vista parcial: el archivo supera el límite de tamaño.",
                        theme::component_palette(),
                    )
                    .role(TextRole::Micro)
                    .muted()
                    .render(),
                );
            }
        }
    }
    Element::column(children)
        .gap(0.0)
        .background(theme::BACKGROUND)
        .flex(1.0)
}

pub(super) fn content(state: &ViewState, width: f32) -> Element {
    let ide = crate::ide_workspace::active(state);
    if let Some(detail) = &state.review_detail {
        return detail_view(detail, width, !ide, &state.git_panel);
    }
    if ide {
        // The list of changes lives in the source control sidebar.
        return UiText::new(
            "Elige un archivo en Control de versiones para ver sus cambios.",
            theme::component_palette(),
        )
        .muted()
        .render()
        .centered();
    }
    navigator(state, width)
}

/// The IDE keeps the repository navigator visible while a diff opens in the
/// center dock. It must not follow the selected detail into the sidebar.
pub(super) fn navigator(state: &ViewState, width: f32) -> Element {
    let inner_width = (width - crate::sidebar_chrome::INSET * 2.0).max(0.0);
    Element::column(vec![
        openide_ui::PanelHeader::new("Cambios", theme::sidebar_palette())
            .trailing(vec![UiIconButton::new(
                "review_refresh",
                Icon::Refresh,
                theme::sidebar_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .tooltip_with(
                TooltipSpec::new("Actualizar cambios").placement(TooltipPlacement::Below),
            )
            .render()])
            .width(inner_width)
            .sizing(crate::ui_sizing::SIDEBAR_HEADER)
            .render(),
        navigator_body(state, inner_width),
    ])
    .gap(crate::sidebar_chrome::GAP)
    .padding(crate::sidebar_chrome::INSET)
    .flex(1.0)
}

/// The branch and the totals of the pane that has no controls of its own.
fn repository_summary(review: &GitReview) -> Element {
    let count = review.unique_files() + review.untracked.len();
    let mut row = vec![
        Element::icon(Icon::GitBranch)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(16.0), Some(16.0))
            .centered_y(),
        UiText::new(&review.branch, theme::component_palette())
            .truncate()
            .tooltip(&review.branch)
            .render()
            .centered_y()
            .flex(1.0),
        UiText::new(
            format!("{count} {}", if count == 1 { "archivo" } else { "archivos" }),
            theme::component_palette(),
        )
        .role(TextRole::Small)
        .muted()
        .render()
        .centered_y(),
    ];
    if review.additions + review.deletions > 0 {
        row.push(
            DiffStat::new(review.additions, review.deletions, theme::component_palette())
                .inks(theme::DIFF_ADDED, theme::DIFF_REMOVED)
                .render()
                .centered_y(),
        );
    }
    Element::row(row).gap(6.0).padding(4.0).size(None, Some(28.0))
}

/// Headerless body for an IDE activity that owns its shared sidebar chrome.
/// In the IDE it carries the branch and commit controls; in the dock it is
/// the list alone, under a line that names the branch.
pub(super) fn navigator_body(state: &ViewState, width: f32) -> Element {
    let ide = state.ide.mode == crate::ide_workspace::WindowMode::Ide;
    let surface = if ide { theme::SIDEBAR } else { theme::BACKGROUND };
    let review = match &state.git_review {
        None => {
            return Element::column(vec![pane_loading_state(
                "review_changes",
                "Cargando cambios",
                width,
            )])
            .flex(1.0)
        }
        Some(Err(error)) => {
            let empty = if error == NOT_A_REPOSITORY {
                pane_empty_state(
                    Icon::GitBranch,
                    "Sin repositorio",
                    "Esta carpeta no es un repositorio de Git.",
                    None,
                )
            } else {
                pane_empty_state(
                    Icon::GitBranch,
                    "No se pudieron cargar los cambios",
                    error,
                    Some(("review_refresh", "Reintentar")),
                )
            };
            return Element::column(vec![empty, Element::spacer().flex(1.0)]).flex(1.0);
        }
        Some(Ok(review)) => review,
    };
    // One column without a gap: the status slot closes to nothing, so each
    // block carries the space under it.
    let below = |element: Element, space: f32| {
        Element::column(vec![element, Element::spacer().size(None, Some(space))]).gap(0.0)
    };
    let mut children = Vec::new();
    if ide {
        children.push(below(branch_bar(state, review, width), 6.0));
        children.push(below(commit_composer(state, review, width), 6.0));
    } else {
        children.push(repository_summary(review));
    }
    children.push(
        Collapse::new(
            "review_status",
            status_line(state, width).map(|status| below(status, 6.0)),
        )
        .expand()
        .render(),
    );
    children.insert(0, origin_selector(state.git_panel.origin));
    if state.git_panel.origin != ReviewOrigin::Uncommitted {
        children.push(range_review(state, review, width));
        return Element::column(children).gap(0.0).flex(1.0);
    }
    if review.branch == "HEAD separado" {
        children.push(below(
            Notice::new("HEAD separado", theme::component_palette())
                .description("Los commits nuevos no quedan en ninguna rama.")
                .icon(Icon::InfoCircle)
                .sizing(NoticeSizing::COMPACT)
                .trailing(
                    Button::new(
                        "review_detached_branch",
                        "Crear rama…",
                        theme::sidebar_palette(),
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Dense)
                    .dispatch_as("environment_git_open_branch")
                    .render(),
                )
                .width(width)
                .render(),
            6.0,
        ));
    }
    if !ide {
        children.push(below(
            Separator::horizontal(theme::component_palette()).render(),
            4.0,
        ));
    }
    if review.files == 0 && review.untracked.is_empty() {
        children.push(pane_empty_state(
            Icon::CheckCircle,
            "Sin cambios",
            match &review.last_commit {
                Some((hash, subject)) => format!("Último commit: {hash} {subject}"),
                None => "El árbol de trabajo está limpio.".to_owned(),
            },
            None,
        ));
        children.push(Element::spacer().flex(1.0));
    } else {
        children.push(change_list(ListContext {
            review: review.clone(),
            width,
            pending: state.git_action_pending || state.environment.workflow.busy.is_some(),
            surface,
            collapsed: state.git_panel.collapsed,
            revealed: state.git_panel.revealed.clone(),
            selected: state
                .review_detail
                .as_ref()
                .map(|detail| (detail.path.clone(), detail.stage)),
            viewed: Arc::new(state.git_panel.viewed.clone()),
        }));
    }
    if review.truncated {
        children.push(
            UiText::new(
                "Listado parcial por tamaño. Los archivos visibles se pueden inspeccionar completos.",
                theme::component_palette(),
            )
            .role(TextRole::Micro)
            .muted()
            .render(),
        );
    }
    if let Some(footer) = github_footer(&review.github, state.git_panel.pull.as_ref(), &state.git_panel.controls, surface) {
        children.push(Separator::horizontal(theme::component_palette()).render());
        children.push(footer);
    }
    Element::column(children).gap(0.0).flex(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, Point, Size, UiAction, UiEvent, UiTree};

    fn text_detail() -> ReviewDetailView {
        ReviewDetailView {
            path: "src/main.rs".into(),
            stage: DiffStage::Unstaged,
            page_index: 0,
            content: Some(Ok(Some(crate::git_review::FileDiffDetail {
                stage: DiffStage::Unstaged,
                file: ReviewFile {
                    path: "src/main.rs".into(),
                    status: "Modificado".into(),
                    additions: 1,
                    deletions: 1,
                    ..Default::default()
                },
                editor_lines: vec![
                    openide_ui::CodeDiffLine {
                        kind: openide_ui::CodeDiffLineKind::Context,
                        old_line: Some(1),
                        new_line: Some(1),
                        text: "fn main() {".into(),
                    },
                    openide_ui::CodeDiffLine {
                        kind: openide_ui::CodeDiffLineKind::Removed,
                        old_line: Some(2),
                        new_line: None,
                        text: "    println!(\"Hello\");".into(),
                    },
                    openide_ui::CodeDiffLine {
                        kind: openide_ui::CodeDiffLineKind::Added,
                        old_line: None,
                        new_line: Some(2),
                        text: "    println!(\"Hola, mundo\");".into(),
                    },
                    openide_ui::CodeDiffLine {
                        kind: openide_ui::CodeDiffLineKind::Context,
                        old_line: Some(3),
                        new_line: Some(3),
                        text: "}".into(),
                    },
                ]
                .into(),
                truncated: false,
            }))),
        }
    }

    #[test]
    fn a_change_is_marked_viewed_as_it_was_and_is_another_once_it_changes() {
        let detail = text_detail();
        let Some(Ok(Some(diff))) = &detail.content else { unreachable!() };
        let (added, removed) = (diff.file.additions, diff.file.deletions);
        let key = viewed_key(&detail.path, detail.stage == DiffStage::Staged, added, removed);
        assert_ne!(key, viewed_key(&detail.path, detail.stage != DiffStage::Staged, added, removed));
        assert_ne!(key, viewed_key(&detail.path, detail.stage == DiffStage::Staged, added + 1, removed));
        assert_ne!(key, viewed_key("otro.rs", detail.stage == DiffStage::Staged, added, removed));
        let offer = |viewed: &std::collections::BTreeSet<String>| {
            let panel = GitPanelView { viewed: viewed.clone(), ..Default::default() };
            let mut tree = UiTree::new(detail_view(&detail, 620.0, true, &panel));
            tree.layout(Size::new(620.0, 300.0));
            assert!(tree.focus("review_detail_viewed"));
            let frame = tree.layout(Size::new(620.0, 300.0));
            texts(&frame).into_iter().map(str::to_owned).collect::<Vec<_>>()
        };
        assert!(offer(&Default::default()).contains(&"Marcar como visto".to_owned()));
        assert!(offer(&[key].into()).contains(&"Marcar como no visto".to_owned()));
        // A change whose diff is not there yet has nothing to have been viewed.
        let loading = ReviewDetailView { content: None, ..detail.clone() };
        let frame = UiTree::new(detail_view(&loading, 620.0, true, &Default::default())).layout(Size::new(620.0, 300.0));
        assert!(!frame.regions.iter().any(|region| region.id == "review_detail_viewed"));
    }

    #[test]
    fn the_blocks_of_an_unstaged_change_are_walked_and_offered_to_discard() {
        let mut detail = text_detail();
        let has = |detail: &ReviewDetailView, panel: &GitPanelView, id: &str| {
            let frame = UiTree::new(detail_view(detail, 720.0, true, panel)).layout(Size::new(720.0, 400.0));
            (frame.regions.iter().any(|region| region.id == id && region.kind == openide_ui::HitKind::Button), texts(&frame).join(" | "))
        };
        let blocks = 2;
        if let Some(Ok(Some(diff))) = &mut detail.content {
            diff.file.lines = ["@@ -1,3 +1,3 @@", "@@ -20 +20 @@ fn otra()"]
                .into_iter()
                .map(|text| crate::git_review::ReviewLine {
                    kind: crate::git_review::ReviewLineKind::Hunk,
                    text: text.into(),
                    old_number: None,
                    new_number: None,
                })
                .collect();
        }
        detail.stage = DiffStage::Unstaged;
        let panel = GitPanelView::default();
        let (offered, said) = has(&detail, &panel, "review_hunk_discard");
        assert_eq!(offered, blocks > 0, "{said}");
        if blocks > 0 {
            assert!(said.contains(&format!("Bloque 1 de {blocks}")) && said.contains("@@ -1,3 +1,3 @@"), "{said}");
            // A place past the last block is the last block.
            let far = GitPanelView { hunk: 99, ..Default::default() };
            let last = has(&detail, &far, "review_hunk_discard").1;
            assert!(last.contains(&format!("Bloque {blocks} de {blocks}")) && last.contains("fn otra()"), "{last}");
        }
        // What is staged is not thrown away from here.
        detail.stage = DiffStage::Staged;
        assert!(!has(&detail, &panel, "review_hunk_discard").0);
    }

    #[test]
    fn a_diff_only_offers_the_way_back_where_the_list_shares_its_tab() {
        let detail = text_detail();
        for back in [true, false] {
            let frame = UiTree::new(detail_view(&detail, 620.0, back, &Default::default())).layout(Size::new(620.0, 300.0));
            let shows_crumb = frame.commands.iter().any(|command| {
                matches!(command, DrawCommand::Text { text, .. } if text == "Cambios")
            });
            assert_eq!(shows_crumb, back);
        }
    }

    #[test]
    fn diff_document_uses_one_editor_header_and_the_full_document_width() {
        for width in [260.0, 360.0, 520.0, 900.0] {
            let detail = text_detail();
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                detail_content(&detail, width),
            );
            let frame = tree.layout(Size::new(width, 300.0));
            let document = frame
                .regions
                .iter()
                .find(|r| r.id.starts_with("git-diff:"))
                .unwrap();
            assert_eq!(document.rect.x, 0.0);
            assert_eq!(document.rect.y, crate::dock_tab_ui::HEADER_HEIGHT);
            assert_eq!(document.rect.width, width);
            assert_eq!(
                document.rect.height,
                300.0 - crate::dock_tab_ui::HEADER_HEIGHT
            );
            let retry = frame
                .regions
                .iter()
                .find(|r| r.id == "review_detail_retry")
                .unwrap();
            assert_eq!(retry.rect.height, crate::dock_tab_ui::TOOL_SIZE);
            assert_eq!(retry.rect.y, 4.0);
            if width >= 360.0 {
                let file = frame.regions.iter()
                    .find(|region| region.id == "review_detail_path:item:1")
                    .unwrap().rect;
                let mut metadata = Vec::new();
                for command in &frame.commands {
                    if let DrawCommand::Text { text, position, font_size, .. } = command {
                        if ["Local", "+1", "−1"].contains(&text.as_str()) {
                            assert_eq!(*font_size, 12.0);
                            assert!(position.y > 4.0 && position.y < 16.0,
                                "metadata is centered inside the 24px breadcrumb cell: {position:?}");
                            metadata.push(*position);
                        }
                    }
                }
                assert_eq!(metadata.len(), 3);
                assert!(metadata[0].x >= file.x + file.width);
                assert!(metadata[0].x - file.x - file.width < 24.0,
                    "metadata follows the filename, not the right edge of the header");
                assert!(metadata[2].x < retry.rect.x - crate::dock_tab_ui::TOOL_SIZE);
            }
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    #[ignore = "export full Git diff resource headers and documents for visual review"]
    fn export_git_diff_editor_visual_review() {
        let directory = std::env::var_os("OPENIDE_GIT_DIFF_VISUAL_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp/openide-git-diff-visual-qa"));
        std::fs::create_dir_all(&directory).unwrap();
        for width in [260.0, 360.0, 520.0, 900.0] {
            let mut detail = text_detail();
            detail.path = "README.md".into();
            if let Some(Ok(Some(diff))) = detail.content.as_mut() {
                diff.file.path = detail.path.clone();
                diff.file.additions = 62;
                diff.file.deletions = 5;
            }
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                detail_content(&detail, width),
            );
            tree.set_reduced_motion(true);
            let started = std::time::Instant::now();
            let frame = loop {
                let frame = tree.layout(Size::new(width, 320.0));
                if !tree.is_animating() {
                    break frame;
                }
                assert!(started.elapsed() < std::time::Duration::from_secs(3));
                std::thread::sleep(std::time::Duration::from_millis(1));
            };
            let svg_path = directory.join(format!("git-diff-{width:.0}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &svg_path, width, 320.0)
                .unwrap();
            let options = crate::settings_ui::visual_qa::native_svg_options();
            let parsed =
                resvg::usvg::Tree::from_str(&std::fs::read_to_string(svg_path).unwrap(), &options)
                    .unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 320).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap
                .save_png(directory.join(format!("git-diff-{width:.0}.png")))
                .unwrap();
            context.end_pass().textures_delta.clear();
        }
    }

    fn file(path: &str, status: &str, additions: usize, deletions: usize) -> ReviewFile {
        ReviewFile {
            path: path.into(),
            status: status.into(),
            additions,
            deletions,
            ..Default::default()
        }
    }

    /// One file of every group: section 0 is staged, section 1 a conflict
    /// and then a change.
    fn review() -> GitReview {
        let mut review = GitReview {
            sections: vec![
                crate::git_review::ReviewSection {
                    title: "CAMBIOS PREPARADOS".into(),
                    files: vec![file("src/staged.rs", "Nuevo", 4, 0)],
                    ..Default::default()
                },
                crate::git_review::ReviewSection {
                    title: "CAMBIOS SIN PREPARAR".into(),
                    files: vec![
                        file("apps/agents-window/src/dock_chrome.rs", "Modificado", 12, 3),
                        file("src/merge.rs", "Conflicto", 5, 5),
                    ],
                    ..Default::default()
                },
            ],
            untracked: vec!["docs/notes.md".into()],
            files: 3,
            ..Default::default()
        };
        review.group_conflicts();
        review
    }

    fn context(review: GitReview, width: f32, surface: Color) -> ListContext {
        ListContext {
            viewed: Default::default(),
            review: Arc::new(review),
            width,
            pending: false,
            surface,
            collapsed: [false; 4],
            revealed: [None, None],
            selected: None,
        }
    }

    fn texts(frame: &openide_ui::Frame) -> Vec<&str> {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn text_at(frame: &openide_ui::Frame, label: &str) -> Point {
        frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text == label => Some(*position),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no text {label:?}"))
    }

    fn has(frame: &openide_ui::Frame, id: &str) -> bool {
        frame.regions.iter().any(|region| region.id == id)
    }

    #[test]
    fn ten_thousand_changes_render_only_visible_rows_and_scroll_to_the_last_file() {
        let review = GitReview {
            untracked: (0..10_000)
                .map(|index| format!("file-{index:05}.rs"))
                .collect(),
            ..Default::default()
        };
        let mut tree = UiTree::new(change_list(context(review, 360.0, theme::BACKGROUND)));
        tree.set_reduced_motion(true);
        let viewport = Size::new(360.0, 600.0);
        let first = tree.layout(viewport);
        assert!(has(&first, "review_untracked_open:0"));
        assert!(has(&first, "review_group_toggle:n"));
        assert!(
            first.commands.len() < 400,
            "{} draw commands",
            first.commands.len()
        );
        assert!(!has(&first, "review_untracked_open:9999"));
        tree.handle_event(UiEvent::ScrollDirect {
            position: Point::new(180.0, 300.0),
            delta_y: -1_000_000.0,
        });
        let last = tree.layout(viewport);
        assert!(has(&last, "review_untracked_open:9999"));
        assert!(last.commands.len() < 400);
    }

    #[test]
    fn list_does_not_render_inline_patch_contents() {
        let review = GitReview {
            sections: vec![crate::git_review::ReviewSection {
                title: "CAMBIOS PREPARADOS".into(),
                files: vec![ReviewFile {
                    lines: vec![ReviewLine {
                        text: "+secret code shown only on demand".into(),
                        kind: ReviewLineKind::Added,
                        old_number: None,
                        new_number: Some(1),
                    }],
                    ..file("src/main.rs", "Modificado", 1, 0)
                }],
                additions: 1,
                ..Default::default()
            }],
            files: 1,
            ..Default::default()
        };
        let frame = UiTree::new(change_list(context(review, 360.0, theme::BACKGROUND)))
            .layout(Size::new(360.0, 600.0));
        assert!(has(&frame, "review_file_open:0:0"));
        assert!(!texts(&frame).iter().any(|text| text.contains("secret code")));
    }

    #[test]
    fn groups_come_in_order_with_their_counts_and_fold() {
        let mut context = context(review(), 360.0, theme::SIDEBAR);
        let order: Vec<_> = (0..list_len(&context.review, &context.collapsed))
            .map(|row| list_entry(&context.review, &context.collapsed, row).unwrap())
            .collect();
        use ChangeGroup::{Changes, Conflicts, Staged, Untracked};
        assert_eq!(
            order,
            vec![
                (Conflicts, None),
                (Conflicts, Some(0)),
                (Staged, None),
                (Staged, Some(0)),
                (Changes, None),
                (Changes, Some(0)),
                (Untracked, None),
                (Untracked, Some(0)),
            ]
        );
        let frame = UiTree::new(change_list(context.clone())).layout(Size::new(360.0, 600.0));
        let painted = texts(&frame);
        let position = |label: &str| painted.iter().position(|text| *text == label).unwrap();
        assert!(position("Conflictos") < position("Preparados"));
        assert!(position("Preparados") < position("Cambios"));
        assert!(position("Cambios") < position("Sin seguimiento"));
        // The rows keep the names their host actions already answer.
        for id in [
            "review_file_open:1:0",
            "review_file_open:0:0",
            "review_file_open:1:1",
            "review_untracked_open:0",
        ] {
            assert!(has(&frame, id), "{id}");
        }
        // 26 px rows, 2 px apart.
        let conflicts = frame.regions.iter().find(|r| r.id == "review_file_open:1:0").unwrap();
        let heading = frame.regions.iter().find(|r| r.id == "review_group_toggle:s").unwrap();
        assert_eq!(conflicts.rect.height, ROW_HEIGHT);
        assert_eq!(heading.rect.y - conflicts.rect.y, ROW_PITCH);

        context.collapsed[Changes as usize] = true;
        assert_eq!(list_len(&context.review, &context.collapsed), 7);
        let frame = UiTree::new(change_list(context)).layout(Size::new(360.0, 600.0));
        assert!(has(&frame, "review_group_toggle:u"));
        assert!(!has(&frame, "review_file_open:1:1"));
        assert!(has(&frame, "review_untracked_open:0"));
    }

    #[test]
    fn a_group_heading_offers_its_actions_only_while_revealed_and_keeps_its_width() {
        let mut context = context(review(), 360.0, theme::SIDEBAR);
        let viewport = Size::new(360.0, 40.0);
        for (group, key, actions) in [
            (ChangeGroup::Conflicts, "c", &[][..]),
            (ChangeGroup::Staged, "s", &["review_unstage_all:s"][..]),
            (
                ChangeGroup::Changes,
                "u",
                &["review_discard_all:u", "review_stage_all:u"][..],
            ),
            (
                ChangeGroup::Untracked,
                "n",
                &["review_discard_all:n", "review_stage_all:n"][..],
            ),
        ] {
            context.revealed = [None, None];
            let rest = UiTree::new(group_header(&context, group)).layout(viewport);
            let toggle = format!("review_group_toggle:{key}");
            let heading = rest.regions.iter().find(|r| r.id == toggle).unwrap().rect;
            assert_eq!(rest.regions.len(), 1, "{key}");
            context.revealed = [Some(format!("g:{key}")), None];
            let revealed = UiTree::new(group_header(&context, group)).layout(viewport);
            assert_eq!(
                revealed.regions.iter().find(|r| r.id == toggle).unwrap().rect,
                heading
            );
            let mut edge = heading.x + heading.width;
            for action in actions {
                let tool = revealed.regions.iter().find(|r| r.id == *action).unwrap().rect;
                assert!(tool.x >= edge, "{action} overlaps what is before it");
                assert_eq!((tool.width, tool.height), (ACTION, ACTION));
                edge = tool.x + tool.width;
            }
            assert!(edge <= 360.0);
            assert_eq!(revealed.regions.len(), 1 + actions.len(), "{key}");
        }
    }

    #[test]
    fn a_file_row_shows_its_name_counts_and_letter_and_its_folder_from_320() {
        for (width, folder) in [(226.0, false), (300.0, false), (320.0, true), (480.0, true)] {
            let context = context(review(), width, theme::BACKGROUND);
            let frame = UiTree::new(file_row(&context, ChangeGroup::Changes, 0))
                .layout(Size::new(width, ROW_HEIGHT));
            let painted = texts(&frame);
            assert!(painted.contains(&"dock_chrome.rs"), "{painted:?}");
            assert_eq!(
                painted.iter().any(|text| text.starts_with("apps/agents")),
                folder,
                "{width}: {painted:?}"
            );
            assert!(painted.contains(&"+12") && painted.contains(&"−3"), "{painted:?}");
            assert!(painted.contains(&"M"));
            // At rest the row is one target: no action is in the way.
            assert_eq!(frame.regions.len(), 1);
            assert_eq!(frame.regions[0].rect.width, width);
            assert!(text_at(&frame, "M").x + LETTER <= width);
        }
    }

    #[test]
    fn counts_too_long_for_their_zone_stay_in_the_hint() {
        let mut review = review();
        review.sections[1].files[1].additions = 110_000;
        review.sections[1].files[1].deletions = 32_000;
        let narrow = context(review, 226.0, theme::BACKGROUND);
        let frame = UiTree::new(file_row(&narrow, ChangeGroup::Changes, 0))
            .layout(Size::new(226.0, ROW_HEIGHT));
        assert!(!texts(&frame).contains(&"+110000"));
        assert!(texts(&frame).contains(&"dock_chrome.rs"));
    }

    #[test]
    fn revealing_a_row_swaps_counts_for_actions_and_moves_nothing_else() {
        for width in [214.0, 254.0, 320.0, 420.0, 480.0] {
            for (group, name, letter, target, narrow, wide) in [
                (
                    ChangeGroup::Conflicts,
                    "merge.rs",
                    "!",
                    "1:0",
                    &["review_open_file:1:0", "review_stage:1:0"][..],
                    &["review_open_file:1:0", "review_stage:1:0"][..],
                ),
                (
                    ChangeGroup::Staged,
                    "staged.rs",
                    "A",
                    "0:0",
                    &["review_open_file:0:0", "review_unstage:0:0"][..],
                    &["review_open_file:0:0", "review_unstage:0:0"][..],
                ),
                (
                    ChangeGroup::Changes,
                    "dock_chrome.rs",
                    "M",
                    "1:1",
                    &["review_discard:1:1", "review_stage:1:1"][..],
                    &["review_open_file:1:1", "review_discard:1:1", "review_stage:1:1"][..],
                ),
                (
                    ChangeGroup::Untracked,
                    "notes.md",
                    "U",
                    "n:0",
                    &["review_discard:n:0", "review_stage_untracked:0"][..],
                    &["review_open_file:n:0", "review_discard:n:0", "review_stage_untracked:0"][..],
                ),
            ] {
                let viewport = Size::new(width, ROW_HEIGHT);
                let mut context = context(review(), width, theme::SIDEBAR);
                let rest = UiTree::new(file_row(&context, group, 0)).layout(viewport);
                context.revealed = [None, Some(target.into())];
                let revealed = UiTree::new(file_row(&context, group, 0)).layout(viewport);
                assert_eq!(text_at(&rest, name), text_at(&revealed, name), "{name} at {width}");
                assert_eq!(
                    text_at(&rest, letter),
                    text_at(&revealed, letter),
                    "{letter} at {width}"
                );
                if width >= 320.0 {
                    let folder = |frame: &openide_ui::Frame| {
                        frame.commands.iter().find_map(|command| match command {
                            DrawCommand::Text { text, position, .. }
                                if text.contains('/') || text == "src" || text == "docs" =>
                            {
                                Some(*position)
                            }
                            _ => None,
                        })
                    };
                    assert_eq!(folder(&rest), folder(&revealed), "folder of {name} at {width}");
                }
                let expected = if width >= WIDE_ROW { wide } else { narrow };
                let tools: Vec<_> = revealed
                    .regions
                    .iter()
                    .filter(|region| region.kind == openide_ui::HitKind::Button)
                    .skip(1)
                    .collect();
                assert_eq!(
                    tools.iter().map(|region| region.id.as_str()).collect::<Vec<_>>(),
                    expected,
                    "{name} at {width}"
                );
                // The row's target, its tools and the letter leave no gap.
                let mut edge = revealed.regions.iter().find(|r| r.id.contains("_open:")).unwrap().rect;
                edge.width += 0.0;
                let mut x = edge.x + edge.width;
                for tool in &tools {
                    assert!((tool.rect.x - x).abs() < 0.01, "{} at {width}", tool.id);
                    assert_eq!((tool.rect.width, tool.rect.height), (ACTION, ACTION));
                    x = tool.rect.x + tool.rect.width;
                }
                let status = revealed
                    .regions
                    .iter()
                    .find(|region| region.id.ends_with(":status"))
                    .unwrap();
                assert_eq!(status.kind, openide_ui::HitKind::Hover);
                assert!((status.rect.x - x).abs() < 0.01);
                assert!((status.rect.x + status.rect.width - width).abs() < 0.01);
                // Wherever the pointer is on the row, it is still this row.
                for region in &revealed.regions {
                    assert_eq!(reveal_key(&region.id).as_deref(), Some(target), "{}", region.id);
                }
                assert_eq!(reveal_key(&rest.regions[0].id).as_deref(), Some(target));
            }
        }
    }

    #[test]
    fn a_deleted_file_cannot_be_opened_and_a_running_operation_holds_the_changes() {
        let mut review = review();
        review.sections[1].files[1].status = "Eliminado".into();
        let mut context = context(review, 420.0, theme::SIDEBAR);
        context.revealed = [Some("1:1".into()), None];
        let frame = UiTree::new(file_row(&context, ChangeGroup::Changes, 0))
            .layout(Size::new(420.0, ROW_HEIGHT));
        assert!(!has(&frame, "review_open_file:1:1"));
        assert!(has(&frame, "review_discard:1:1") && has(&frame, "review_stage:1:1"));
        context.pending = true;
        let frame = UiTree::new(file_row(&context, ChangeGroup::Changes, 0))
            .layout(Size::new(420.0, ROW_HEIGHT));
        for id in ["review_discard:1:1", "review_stage:1:1"] {
            let region = frame.regions.iter().find(|region| region.id == id).unwrap();
            assert_eq!(region.kind, openide_ui::HitKind::Hover, "{id} keeps only its hint");
        }
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "review_file_open:1:1" && r.kind == openide_ui::HitKind::Button));
    }

    #[test]
    fn reveal_keys_name_rows_and_headings_and_nothing_else() {
        for (id, key) in [
            ("review_file_open:1:2", Some("1:2")),
            ("review_file_open:1:2:status", Some("1:2")),
            ("review_stage:1:2", Some("1:2")),
            ("review_unstage:0:0", Some("0:0")),
            ("review_discard:n:4", Some("n:4")),
            ("review_open_file:n:4", Some("n:4")),
            ("review_untracked_open:4", Some("n:4")),
            ("review_untracked_open:4:status", Some("n:4")),
            ("review_stage_untracked:4", Some("n:4")),
            ("review_group_toggle:u", Some("g:u")),
            ("review_discard_all:n", Some("g:n")),
            ("review_refresh", None),
            ("ide_git_commit", None),
            ("review_copy_path:1:2", None),
        ] {
            assert_eq!(reveal_key(id).as_deref(), key, "{id}");
        }
    }

    #[test]
    fn the_file_whose_diff_is_open_is_marked_in_its_own_group_only() {
        let mut context = context(review(), 360.0, theme::SIDEBAR);
        context.selected = Some(("apps/agents-window/src/dock_chrome.rs".into(), DiffStage::Unstaged));
        let fills = |group| {
            UiTree::new(file_row(&context, group, 0))
                .layout(Size::new(360.0, ROW_HEIGHT))
                .commands
                .iter()
                .any(|command| matches!(command, DrawCommand::FillRect { color, .. } if *color == theme::SELECTED))
        };
        assert!(fills(ChangeGroup::Changes));
        assert!(!fills(ChangeGroup::Staged));
    }

    #[test]
    fn a_row_menu_has_the_row_actions_and_the_paths() {
        for (group, labels) in [
            (
                ChangeGroup::Changes,
                vec![
                    "Abrir cambios",
                    "Abrir archivo",
                    "Preparar archivo",
                    "Descartar cambios…",
                    "Copiar ruta",
                    "Copiar ruta relativa",
                ],
            ),
            (
                ChangeGroup::Staged,
                vec![
                    "Abrir cambios",
                    "Abrir archivo",
                    "Quitar de preparados",
                    "Copiar ruta",
                    "Copiar ruta relativa",
                ],
            ),
            (
                ChangeGroup::Untracked,
                vec![
                    "Abrir cambios",
                    "Abrir archivo",
                    "Preparar archivo",
                    "Enviar a la papelera…",
                    "Copiar ruta",
                    "Copiar ruta relativa",
                ],
            ),
            (
                ChangeGroup::Conflicts,
                vec![
                    "Abrir cambios",
                    "Abrir archivo",
                    "Marcar como resuelto",
                    "Copiar ruta",
                    "Copiar ruta relativa",
                ],
            ),
        ] {
            let context = context(review(), 360.0, theme::SIDEBAR);
            let viewport = Size::new(360.0, 400.0);
            let mut tree = UiTree::new(Element::column(vec![
                file_row(&context, group, 0),
                Element::spacer().flex(1.0),
            ]));
            tree.set_reduced_motion(true);
            tree.set_context_menu_palette(theme::menu_palette());
            tree.layout(viewport);
            tree.handle_event(UiEvent::ContextMenu {
                position: Point::new(40.0, 12.0),
            });
            let frame = tree.layout(viewport);
            assert!(has(&frame, "__context_menu:surface"));
            let painted = texts(&frame);
            for label in &labels {
                assert!(painted.contains(label), "{group:?}: {label} in {painted:?}");
            }
            for other in ["Descartar cambios…", "Enviar a la papelera…"] {
                assert_eq!(painted.contains(&other), labels.contains(&other), "{group:?}");
            }
        }
    }

    #[test]
    fn git_sidebar_file_hover_is_visible_on_the_sidebar_surface() {
        let context = context(review(), 300.0, theme::SIDEBAR);
        let viewport = Size::new(300.0, 100.0);
        let mut tree = UiTree::new(file_row(&context, ChangeGroup::Changes, 0));
        tree.set_reduced_motion(true);
        let frame = tree.layout(viewport);
        let region = frame.regions.iter().find(|r| r.id == "review_file_open:1:1").unwrap();
        tree.handle_event(UiEvent::PointerMove(Point::new(
            region.rect.x + 10.0,
            region.rect.y + 10.0,
        )));
        assert_eq!(tree.hovered_id().and_then(reveal_key).as_deref(), Some("1:1"));
        let hovered = tree.layout(viewport);
        let expected = theme::action_palette(theme::SIDEBAR, theme::FOREGROUND).hover;
        assert_ne!(expected, theme::SIDEBAR);
        assert!(hovered
            .commands
            .iter()
            .any(|c| matches!(c,DrawCommand::FillRect {color,..} if *color==expected)));
    }

    #[test]
    fn discarding_asks_first_and_names_the_files() {
        let mut state = super::super::tests::empty_state();
        state.workspace = "/workspace/project".into();
        let dialog = |state: &ViewState| {
            let mut tree = UiTree::new(discard_dialog(state, Element::spacer(), 800.0));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(800.0, 600.0));
            let text = frame
                .text_layouts()
                .iter()
                .map(|block| block.source.clone())
                .chain(texts(&frame).into_iter().map(str::to_owned))
                .collect::<Vec<_>>()
                .join("\n");
            (frame, text)
        };
        assert!(dialog(&state).0.regions.is_empty());
        let confirm = |trash, count: usize| GitConfirmView {
            workspace: "/workspace/project".into(),
            trash,
            paths: (0..count).map(|index| format!("src/file-{index}.rs").into()).collect(),
            hunk: None,
        };
        state.git_panel.confirm = Some(confirm(false, 1));
        let (frame, text) = dialog(&state);
        assert!(has(&frame, "review_discard_confirm") && has(&frame, "review_discard_cancel"));
        assert!(text.contains("¿Descartar los cambios?"), "{text}");
        assert!(text.contains("«src/file-0.rs»") && text.contains("No se puede deshacer"));
        assert!(text.contains("Descartar") && text.contains("Cancelar"));

        state.git_panel.confirm = Some(confirm(false, 8));
        let (_, text) = dialog(&state);
        assert!(text.contains("¿Descartar los cambios de 8 archivos?"), "{text}");
        assert!(text.contains("«src/file-4.rs»") && text.contains("y 3 más"));
        assert!(!text.contains("«src/file-5.rs»"));

        state.git_panel.confirm = Some(confirm(true, 1));
        let (_, text) = dialog(&state);
        assert!(text.contains("¿Enviar a la papelera?") && text.contains("papelera del sistema"));
        state.git_panel.confirm = Some(confirm(true, 2));
        assert!(dialog(&state).1.contains("¿Enviar 2 archivos a la papelera?"));

        // One block of one file says which.
        state.git_panel.confirm = Some(GitConfirmView { hunk: Some(2), ..confirm(false, 1) });
        let (frame, text) = dialog(&state);
        assert!(has(&frame, "review_discard_confirm"));
        assert!(text.contains("¿Descartar este bloque?") && text.contains("el bloque 3") && text.contains("«src/file-0.rs»"), "{text}");

        // Asked in another project, it is not this project's question.
        state.workspace = "/workspace/other".into();
        assert!(dialog(&state).0.regions.is_empty());
    }

    #[test]
    fn the_selector_names_every_origin_and_marks_the_one_in_view() {
        let frame = UiTree::new(origin_selector(ReviewOrigin::Commit)).layout(Size::new(240.0, 80.0));
        for origin in ["uncommitted", "branch", "commit", "chat", "turn"] {
            assert!(frame.regions.iter().any(|region| region.id == format!("review_origin:{origin}")), "{origin}");
        }
        assert_eq!(texts(&frame), ["Sin commitear", "Rama", "Commit", "Chat", "Último turno"]);
    }

    #[test]
    fn a_branch_without_a_pull_request_offers_to_open_one() {
        let offered = |branch: &str, pulls: Vec<crate::github::GithubPull>| {
            let status = GithubStatus::Ready(crate::github::GithubOverview {
                repository: crate::github::parse_remote("git@github.com:team/project.git", "origin").unwrap(),
                branch: branch.into(),
                pulls,
                notice: None,
            });
            let form = Some(crate::github::PullDraft::default());
            let ids = |form: Option<&crate::github::PullDraft>| {
                let frame = UiTree::new(github_footer(&status, form, &Default::default(), theme::BACKGROUND).unwrap()).layout(Size::new(320.0, 320.0));
                let ids: Vec<String> = frame.regions.iter().map(|region| region.id.to_string()).collect();
                ids
            };
            // Closed it offers to open one; open it asks what to open it with.
            let asked = ["github_pr_title", "github_pr_body", "github_pr_base", "github_pr_submit", "github_pr_submit_draft", "github_pr_cancel"];
            [ids(None).iter().any(|id| id == "github_pr_create"), asked.iter().all(|id| ids(form.as_ref()).iter().any(|known| known == id))]
        };
        assert_eq!(offered("topic", vec![]), [true, true]);
        assert_eq!(offered("HEAD separado", vec![]), [false, false]);
        let open = crate::github::GithubPull {
            number: 7,
            title: "Listo".into(),
            url: "https://github.com/team/project/pull/7".into(),
            author: "ana".into(),
            base: "main".into(),
            draft: false,
            head: "abc123".into(),
            checks: Some(crate::github::PullChecks::Failing),
        };
        assert_eq!(offered("topic", vec![open.clone()]), [false, false]);
        // The pull request says how its checks stand.
        let status = GithubStatus::Ready(crate::github::GithubOverview {
            repository: crate::github::parse_remote("git@github.com:team/project.git", "origin").unwrap(),
            branch: "topic".into(),
            pulls: vec![open],
            notice: None,
        });
        let frame = UiTree::new(github_footer(&status, None, &Default::default(), theme::BACKGROUND).unwrap()).layout(Size::new(320.0, 200.0));
        assert!(texts(&frame).contains(&"Checks fallando"), "{:?}", texts(&frame));
    }

    #[test]
    fn an_open_pull_request_offers_merge_approve_comment_and_watch() {
        let status = GithubStatus::Ready(crate::github::GithubOverview {
            repository: crate::github::parse_remote("git@github.com:team/project.git", "origin").unwrap(),
            branch: "topic".into(),
            pulls: vec![crate::github::GithubPull {
                number: 7,
                title: "Listo".into(),
                url: "https://github.com/team/project/pull/7".into(),
                author: "ana".into(),
                base: "main".into(),
                draft: false,
                head: "abc123".into(),
                checks: None,
            }],
            notice: None,
        });
        let ids = |controls: &PullControls| -> Vec<String> {
            let frame = UiTree::new(github_footer(&status, None, controls, theme::BACKGROUND).unwrap())
                .layout(Size::new(360.0, 400.0));
            frame.regions.iter().map(|region| region.id.to_string()).collect()
        };
        let plain = ids(&PullControls::default());
        for id in ["github_pr_merge", "github_pr_approve", "github_pr_comment"] {
            assert!(plain.iter().any(|known| known == id), "{id} in {plain:?}");
        }
        assert!(!plain.iter().any(|id| id == "github_pr_watch"), "no task, no watch");
        let open = ids(&PullControls { comment: Some(String::new()), merge_confirm: true, watch: Some(false), watchable: true });
        for id in ["github_pr_merge_squash", "github_pr_merge_commit", "github_comment_body", "github_comment_submit", "github_pr_watch", "github_pr_watch_wake"] {
            assert!(open.iter().any(|known| known == id), "{id} in {open:?}");
        }
    }

    #[test]
    fn github_footer_icon_is_centered_inside_the_repository_button() {
        let status = GithubStatus::Ready(crate::github::GithubOverview {
            repository: crate::github::GithubRepository {
                owner: "team".into(),
                name: "project".into(),
                remote_name: "origin".into(),
                head_owner: "team".into(),
            },
            branch: "main".into(),
            pulls: vec![],
            notice: None,
        });
        let mut tree = UiTree::new(github_footer(&status, None, &Default::default(), theme::BACKGROUND).unwrap());
        let frame = tree.layout(Size::new(320.0, 44.0));
        let target = frame
            .regions
            .iter()
            .find(|region| region.id == "github_repo_open")
            .unwrap();
        let icon = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Icon {
                    icon: Icon::Github,
                    rect,
                    ..
                } => Some(rect),
                _ => None,
            })
            .unwrap();
        assert!(
            ((icon.y + icon.height / 2.0) - (target.rect.y + target.rect.height / 2.0)).abs()
                < 0.01
        );
        assert_eq!(
            frame.hit_test(Point::new(
                icon.x + icon.width / 2.0,
                icon.y + icon.height / 2.0
            )),
            Some("github_repo_open")
        );
        assert!(tree.focus("github_repo_open"));
        let focused = tree.layout(Size::new(320.0, 120.0));
        assert!(focused.commands.iter().any(|command| matches!(
            command, DrawCommand::Text { text, .. } if text == "Abrir repositorio en GitHub"
        )));
    }

    #[test]
    fn diff_header_breadcrumb_preserves_navigation_and_toolbar_tooltips() {
        let detail = ReviewDetailView {
            path: "apps/agents-window/src/git_panel.rs".into(),
            stage: DiffStage::Unstaged,
            page_index: 0,
            content: None,
        };
        for width in [260.0, 360.0, 520.0] {
            let viewport = Size::new(width, 300.0);
            let mut tree = UiTree::new(detail_content(&detail, width));
            let frame = tree.layout(viewport);
            for action in [
                "review_detail_back",
                "review_detail_open_editor",
                "review_detail_retry",
            ] {
                let region = frame
                    .regions
                    .iter()
                    .find(|region| region.id == action)
                    .unwrap();
                assert!(region.rect.x >= 0.0);
                assert!(region.rect.x + region.rect.width <= width + 0.01);
            }
            assert!(tree.focus("review_detail_back"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: openide_ui::Key::Enter,
                    shift: false,
                    ctrl: false,
                }),
                vec![UiAction::Clicked {
                    id: "review_detail_back".into(),
                }]
            );
            assert!(frame.commands.iter().any(|command| matches!(
                command, DrawCommand::Text { text, .. } if text == "git_panel.rs"
            )));
            assert!(frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::Icon {
                    icon: Icon::ChevronRight,
                    ..
                }
            )));
            assert!(!frame.commands.iter().any(|command| matches!(
                command, DrawCommand::Text { text, .. } if text == "/"
            )));
            for (action, hint) in [
                ("review_detail_open_editor", "Abrir en editor"),
                ("review_detail_retry", "Actualizar diff"),
                ("review_detail_back", "Volver a los cambios del repositorio"),
            ] {
                assert!(tree.focus(action));
                let focused = tree.layout(viewport);
                // The breadcrumb's help is a wrapped description beneath
                // its title on narrow panes, so it can span text commands.
                let text = focused
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .flat_map(str::split_whitespace)
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(text.contains(hint), "{action} at {width}px: {text:?}");
                assert!(focused
                    .commands
                    .iter()
                    .any(|command| matches!(command, DrawCommand::Shadow { radius, .. } if *radius == 4.0)));
            }
        }
    }
}

#[cfg(test)]
#[path = "git_panel_visual_tests.rs"]
mod visual_tests;
