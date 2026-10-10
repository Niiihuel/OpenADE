//! Controlled Git forms used by the conversation's environment card.
//! The host owns requests, snapshots and validation; rendering never runs Git.

use crate::{
    environment_git::{BranchComparison, GitSnapshot},
    theme,
};
use openide_ui::{
    Button, ButtonVariant, Component, ControlSize, DropdownMenu, DropdownMenuEntry,
    DropdownMenuItem, Element, FontFamily, Icon, IconButton, PopoverPlacement, Separator, TextArea,
    TextField, Tooltip, TooltipPlacement, TooltipSpec,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum EnvironmentGitPanel {
    #[default]
    Overview,
    Commit,
    Compare,
    CreateBranch,
    CreateWorktree,
    Actions,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EnvironmentGitUiState {
    /// Generation of the immutable worktree list used by menu actions.
    pub snapshot_revision: u64,
    pub snapshot: Option<GitSnapshot>,
    pub loading: bool,
    pub busy: Option<String>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub panel: EnvironmentGitPanel,
    pub commit_message: String,
    pub push_remote: String,
    pub push_branch: String,
    pub compare_reference: String,
    pub comparison: Option<BranchComparison>,
    pub compare_url: Option<String>,
    pub branch_name: String,
    pub worktree_path: String,
    pub remote_menu_open: bool,
    pub compare_menu_open: bool,
}

fn text(value: impl Into<String>, muted: bool, size: f32) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(size)
        .foreground(if muted {
            theme::FOREGROUND_SUBTLE
        } else {
            theme::FOREGROUND
        })
}

fn action(id: &str, title: &str, enabled: bool, primary: bool) -> Element {
    let button = Button::new(id, title, theme::component_palette())
        .variant(if primary {
            ButtonVariant::Default
        } else {
            ButtonVariant::Ghost
        })
        .size(ControlSize::Regular)
        .enabled(enabled)
        .render();
    if primary {
        button
    } else {
        crate::environment_ui::interactive_surface(button)
    }
}

fn field(id: &str, value: &str, placeholder: &str, enabled: bool) -> Element {
    TextField::new(id, value, placeholder, theme::component_palette())
        .size(ControlSize::Regular)
        .enabled(enabled)
        .render()
}

pub(crate) fn notices(state: &EnvironmentGitUiState) -> Vec<Element> {
    let mut rows = Vec::new();
    if let Some(busy) = &state.busy {
        rows.push(text(busy, true, 11.0));
    } else if state.loading {
        rows.push(text("Actualizando repositorio…", true, 11.0));
    }
    if let Some(error) = &state.error {
        rows.push(text(error, false, 11.0).foreground(theme::DESTRUCTIVE));
    } else if let Some(notice) = &state.notice {
        rows.push(text(notice, true, 11.0));
    }
    rows
}

fn compact_action(
    id: &str,
    title: &str,
    icon: Icon,
    enabled: bool,
    primary: bool,
    width: f32,
) -> Element {
    let palette = theme::action_palette(theme::SIDEBAR, theme::FOREGROUND);
    Button::new(id, title, palette)
        .icon(icon)
        .variant(if primary {
            ButtonVariant::Default
        } else {
            ButtonVariant::Outline
        })
        .size(ControlSize::Compact)
        .enabled(enabled)
        .width(width)
        .render()
}

/// A 24 px tool set 2 px down in the 28 px commit header.
fn header_slot(control: Element) -> Element {
    Element::column(vec![Element::spacer().size(None, Some(2.0)), control])
        .size(Some(24.0), Some(28.0))
}

fn branch_context(branch: &str, width: f32) -> Element {
    Tooltip::new(
        Element::row(vec![
            Element::icon(Icon::GitBranch)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(14.0), Some(24.0)),
            Element::truncated_text(branch)
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::FOREGROUND)
                .flex(1.0),
        ])
        .gap(6.0)
        .size(Some(width), Some(24.0)),
        "environment_git_current_branch",
        branch,
        theme::component_palette(),
    )
    .placement(TooltipPlacement::Below)
    .render()
}

fn change_count(title: &str, count: usize, staged: bool, width: f32) -> Element {
    Element::row(vec![
        Element::truncated_text(title)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::FOREGROUND)
            .flex(1.0),
        Element::truncated_text(count.to_string())
            .font_family(FontFamily::Sans)
            .font_size(11.0)
            .foreground(if staged && count > 0 {
                theme::DIFF_ADDED
            } else {
                theme::FOREGROUND_SUBTLE
            })
            .center_content()
            .size(Some(44.0), Some(24.0))
            .background(theme::PANEL)
            .radius(4.0),
    ])
    .gap(8.0)
    .size(Some(width), Some(24.0))
}

fn commit_blocked_reason(
    state: &EnvironmentGitUiState,
    snapshot: &GitSnapshot,
    locked: bool,
) -> Option<String> {
    if locked {
        Some("Esperá a que termine la operación en curso.".into())
    } else if state.loading {
        Some("Actualizando los cambios del repositorio…".into())
    } else if state.busy.is_some() {
        Some("Hay una operación de Git en curso.".into())
    } else if snapshot.conflicts > 0 {
        Some(format!(
            "Resolvé {} {} antes de crear el commit.",
            snapshot.conflicts,
            if snapshot.conflicts == 1 {
                "conflicto"
            } else {
                "conflictos"
            }
        ))
    } else if snapshot.staged == 0 {
        Some("Prepará al menos un archivo para crear el commit.".into())
    } else if state.commit_message.trim().is_empty() {
        Some("Escribí un mensaje para los cambios preparados.".into())
    } else {
        None
    }
}

fn push_blocked_reason(
    state: &EnvironmentGitUiState,
    snapshot: &GitSnapshot,
    locked: bool,
) -> Option<&'static str> {
    if locked || state.loading || state.busy.is_some() {
        Some("Esperá a que termine la operación en curso.")
    } else if snapshot.conflicts > 0 {
        Some("Resolvé los conflictos antes de publicar.")
    } else if snapshot.head.is_none() {
        Some("Creá el primer commit antes de publicar.")
    } else if snapshot.current_branch().is_none() {
        Some("Cambiá a una rama antes de publicar.")
    } else if !snapshot.remotes.contains(&state.push_remote) {
        Some("Elegí un remoto de destino.")
    } else if state.push_branch.trim().is_empty() {
        Some("Indicá una rama de destino.")
    } else {
        None
    }
}

fn commit_view(state: &EnvironmentGitUiState, width: f32, locked: bool, sidebar: bool) -> Element {
    let inner = (width - 16.0).max(0.0);
    let enabled = !locked && !state.loading && state.busy.is_none();
    let input_palette = theme::action_palette(theme::BACKGROUND, theme::FOREGROUND);
    let mut header = vec![
        header_slot(
            IconButton::new(
                "environment_git_back",
                Icon::ArrowLeft,
                theme::sidebar_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .tooltip_with(
                TooltipSpec::new("Volver a los cambios").placement(TooltipPlacement::Below),
            )
            .render(),
        ),
        Element::truncated_text("Commit")
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .foreground(theme::FOREGROUND)
            .flex(1.0),
        header_slot(
            IconButton::new(
                "environment_git_refresh",
                Icon::Refresh,
                theme::sidebar_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .enabled(enabled)
            .tooltip_with(
                TooltipSpec::new("Actualizar repositorio").placement(TooltipPlacement::Below),
            )
            .render(),
        ),
    ];
    if !sidebar {
        header.push(header_slot(
            IconButton::new("environment_dismiss", Icon::Close, theme::sidebar_palette())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .tooltip_with(TooltipSpec::new("Cerrar entorno").placement(TooltipPlacement::Below))
                .render(),
        ));
    }
    let mut rows = vec![Element::row(header).gap(4.0).size(Some(inner), Some(28.0))];
    rows.extend(notices(state));
    if let Some(snapshot) = &state.snapshot {
        rows.push(branch_context(&snapshot.branches.label, inner));
        let mut changes = vec![
            change_count("Preparados", snapshot.staged, true, inner),
            change_count("Sin preparar", snapshot.unstaged, false, inner),
            change_count("Nuevos", snapshot.untracked, false, inner),
        ];
        if snapshot.conflicts > 0 {
            changes.push(
                Element::row(vec![
                    Element::icon(Icon::WarningTriangle)
                        .foreground(theme::DESTRUCTIVE)
                        .size(Some(14.0), Some(14.0)),
                    text(
                        format!(
                            "{} {} en conflicto",
                            snapshot.conflicts,
                            if snapshot.conflicts == 1 {
                                "archivo"
                            } else {
                                "archivos"
                            }
                        ),
                        false,
                        11.0,
                    )
                    .foreground(theme::DESTRUCTIVE)
                    .flex(1.0),
                ])
                .gap(6.0)
                .size(Some(inner), Some(26.0)),
            );
        }
        changes.push(
            Button::new(
                "dock_open:review",
                "Revisar y preparar archivos",
                theme::action_palette(theme::SIDEBAR, theme::FOREGROUND),
            )
            .icon(Icon::File)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .width(inner)
            .render(),
        );
        rows.push(Element::column(changes).gap(2.0));

        let reason = commit_blocked_reason(state, snapshot, locked);
        let mut commit = vec![
            text("Mensaje del commit", false, 12.0),
            TextArea::new(
                "environment_git_commit_message",
                &state.commit_message,
                "Describe los cambios preparados",
                input_palette,
            )
            .width(inner)
            .height(80.0)
            .enabled(enabled)
            .render()
            .radius(6.0),
        ];
        if let Some(reason) = &reason {
            commit.push(text(reason, true, 11.0));
        }
        commit.push(compact_action(
            "environment_git_commit",
            "Crear commit",
            Icon::Check,
            reason.is_none(),
            true,
            inner,
        ));
        rows.push(Element::column(commit).gap(6.0));
        rows.push(
            Separator::horizontal(theme::component_palette())
                .ink(theme::TREE_GUIDE)
                .render(),
        );

        let mut publish = vec![text("Publicar rama", false, 12.0)];
        if let Some(upstream) = &snapshot.upstream {
            publish.push(
                Tooltip::new(
                    Element::truncated_text(upstream)
                        .font_family(FontFamily::Sans)
                        .font_size(11.0)
                        .foreground(theme::FOREGROUND_SUBTLE)
                        .size(Some(inner), Some(18.0)),
                    "environment_git_upstream",
                    format!("Seguimiento: {upstream}"),
                    theme::component_palette(),
                )
                .render(),
            );
            publish.push(
                Element::row(vec![
                    Element::icon(Icon::ArrowUp)
                        .foreground(theme::FOREGROUND_SUBTLE)
                        .size(Some(12.0), Some(12.0)),
                    text(format!("{} por enviar", snapshot.ahead), true, 11.0).flex(1.0),
                    Element::icon(Icon::ArrowDown)
                        .foreground(theme::FOREGROUND_SUBTLE)
                        .size(Some(12.0), Some(12.0)),
                    text(format!("{} por recibir", snapshot.behind), true, 11.0).flex(1.0),
                ])
                .gap(4.0)
                .size(Some(inner), Some(20.0)),
            );
        } else {
            publish.push(text("Sin rama de seguimiento", true, 11.0));
        }
        if snapshot.remotes.is_empty() {
            publish.push(text(
                "Este repositorio todavía no tiene un remoto configurado.",
                true,
                11.0,
            ));
        } else {
            publish.push(text("Destino remoto", true, 11.0));
            let remote_width = (inner * 0.36).clamp(80.0, 116.0).min(inner);
            let branch_width = (inner - remote_width - 6.0).max(0.0);
            let remote = if state.push_remote.is_empty() {
                "Remoto"
            } else {
                &state.push_remote
            };
            let entries = snapshot
                .remotes
                .iter()
                .enumerate()
                .map(|(index, remote)| {
                    DropdownMenuItem::new(format!("environment_git_remote:{index}"), remote)
                        .icon(Icon::Globe)
                        .selected(*remote == state.push_remote)
                        .into()
                })
                .collect::<Vec<DropdownMenuEntry>>();
            let anchor = Button::new("environment_git_remote_toggle", remote, input_palette)
                .trailing_icon(Icon::ChevronDown)
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .enabled(enabled)
                .width(remote_width)
                .render();
            let remote = DropdownMenu::new(
                anchor,
                entries,
                state.remote_menu_open && enabled,
                "environment_git_remote_dismiss",
                theme::menu_palette(),
            )
            .context_menu_style()
            .width(inner)
            .max_visible_items(6)
            .placement(PopoverPlacement::BottomStart)
            .render();
            let branch = Tooltip::new(
                TextField::new(
                    "environment_git_push_branch",
                    &state.push_branch,
                    "Rama de destino",
                    input_palette,
                )
                .size(ControlSize::Compact)
                .enabled(enabled)
                .width(branch_width)
                .render(),
                "environment_git_push_branch",
                "Rama de destino",
                theme::component_palette(),
            )
            .description(&state.push_branch)
            .render();
            publish.push(
                Element::row(vec![remote, branch])
                    .gap(6.0)
                    .size(Some(inner), Some(28.0)),
            );
            let reason = push_blocked_reason(state, snapshot, locked);
            if let Some(reason) = reason {
                publish.push(text(reason, true, 11.0));
            }
            publish.push(compact_action(
                "environment_git_push",
                if snapshot.upstream.is_some() {
                    "Hacer push"
                } else {
                    "Publicar rama"
                },
                Icon::ArrowUp,
                reason.is_none(),
                false,
                inner,
            ));
        }
        rows.push(Element::column(publish).gap(6.0));
    }
    Element::column(rows)
        .gap(10.0)
        .padding(8.0)
        .size(Some(width), None)
        .clip_children()
}

/// Sidebar navigation returns to Changes; the environment-card dismiss action
/// has no role here because the IDE owns the sidebar's visibility controls.
pub(crate) fn build_sidebar(state: &EnvironmentGitUiState, width: f32, locked: bool) -> Element {
    if state.panel == EnvironmentGitPanel::Commit {
        commit_view(state, width, locked, true)
    } else {
        build(state, width, locked)
    }
}

pub(crate) fn build(state: &EnvironmentGitUiState, width: f32, locked: bool) -> Element {
    if state.panel == EnvironmentGitPanel::Commit {
        return commit_view(state, width, locked, false);
    }
    let enabled = !locked && !state.loading && state.busy.is_none();
    let title = match state.panel {
        EnvironmentGitPanel::Overview => "Git",
        EnvironmentGitPanel::Commit => "Commit o push",
        EnvironmentGitPanel::Compare => "Comparar rama",
        EnvironmentGitPanel::CreateBranch => "Crear rama",
        EnvironmentGitPanel::CreateWorktree => "Crear worktree",
        EnvironmentGitPanel::Actions => "Acciones",
    };
    let mut rows = vec![Element::row(vec![
        IconButton::new(
            "environment_git_back",
            Icon::ArrowLeft,
            theme::action_palette(theme::CARD, theme::FOREGROUND),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(TooltipSpec::new("Volver al entorno").placement(TooltipPlacement::Below))
        .render(),
        text(title, false, 14.0).flex(1.0),
        IconButton::new(
            "environment_git_refresh",
            Icon::Refresh,
            theme::action_palette(theme::CARD, theme::FOREGROUND),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .enabled(enabled)
        .tooltip_with(TooltipSpec::new("Actualizar repositorio").placement(TooltipPlacement::Below))
        .render(),
        IconButton::new(
            "environment_dismiss",
            Icon::Close,
            theme::action_palette(theme::CARD, theme::FOREGROUND),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(
            TooltipSpec::new("Cerrar entorno del chat").placement(TooltipPlacement::Below),
        )
        .render(),
    ])
    .gap(6.0)
    .size(None, Some(32.0))];
    rows.extend(notices(state));
    if locked {
        rows.push(text(
            "Esperá a que termine la operación en curso para cambiar el repositorio.",
            true,
            11.0,
        ));
    }
    let Some(snapshot) = &state.snapshot else {
        return Element::column(rows)
            .gap(10.0)
            .padding(8.0)
            .size(Some(width), None);
    };
    rows.push(text(&snapshot.branches.label, true, 11.5));
    match state.panel {
        EnvironmentGitPanel::Commit => unreachable!("commit has its compact presentation"),
        EnvironmentGitPanel::Compare => {
            rows.push(text(
                "Comparar el estado confirmado de esta rama con otra rama del repositorio.",
                true,
                11.5,
            ));
            let entries: Vec<DropdownMenuEntry> = snapshot
                .branches
                .branches
                .iter()
                .enumerate()
                .filter(|(_, branch)| !branch.current)
                .map(|(index, branch)| {
                    DropdownMenuItem::new(format!("environment_git_compare:{index}"), &branch.name)
                        .icon(Icon::GitBranch)
                        .annotation(if branch.remote { "Remota" } else { "Local" })
                        .into()
                })
                .collect();
            let selection = snapshot
                .branches
                .branches
                .iter()
                .find(|branch| branch.reference == state.compare_reference)
                .map(|branch| branch.name.as_str())
                .unwrap_or("Elegir rama base");
            let anchor = Button::new(
                "environment_git_compare_toggle",
                selection,
                theme::component_palette(),
            )
            .icon(Icon::GitBranch)
            .trailing_icon(Icon::ChevronDown)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Regular)
            .enabled(enabled && !entries.is_empty())
            .width(width - 16.0)
            .render();
            let anchor = crate::environment_ui::interactive_surface(anchor);
            rows.push(
                DropdownMenu::new(
                    anchor,
                    entries,
                    state.compare_menu_open && enabled,
                    "environment_git_compare_dismiss",
                    theme::menu_palette(),
                )
                .inline(true)
                .width(width - 16.0)
                .render(),
            );
            if let Some(comparison) = &state.comparison {
                rows.push(
                    Element::column(vec![
                        text(format!("{} archivos", comparison.files), false, 14.0),
                        Element::row(vec![
                            text(format!("+{}", comparison.additions), false, 13.0)
                                .foreground(theme::DIFF_ADDED),
                            text(format!("−{}", comparison.deletions), false, 13.0)
                                .foreground(theme::DIFF_REMOVED),
                        ])
                        .gap(10.0),
                        text(
                            format!(
                                "{} commits propios · {} commits en la base",
                                comparison.ahead, comparison.behind
                            ),
                            true,
                            11.0,
                        ),
                    ])
                    .gap(8.0)
                    .padding(12.0)
                    .background(theme::PANEL)
                    .radius(8.0),
                );
                rows.push(action(
                    "environment_git_compare_refresh",
                    "Actualizar comparación",
                    enabled,
                    false,
                ));
                if state.compare_url.is_some() {
                    rows.push(action(
                        "environment_git_compare_open_web",
                        "Ver comparación en GitHub",
                        true,
                        false,
                    ));
                }
            } else if !state.compare_reference.is_empty() {
                rows.push(action(
                    "environment_git_compare_refresh",
                    "Comparar",
                    enabled,
                    true,
                ));
            } else if snapshot
                .branches
                .branches
                .iter()
                .all(|branch| branch.current)
            {
                rows.push(text("No hay otra rama para comparar.", true, 11.0));
            }
        }
        EnvironmentGitPanel::CreateBranch | EnvironmentGitPanel::CreateWorktree => {
            let worktree = state.panel == EnvironmentGitPanel::CreateWorktree;
            rows.push(text(
                if worktree {
                    "Una carpeta de trabajo independiente para una nueva rama."
                } else {
                    "La nueva rama parte del commit actual."
                },
                true,
                11.5,
            ));
            rows.push(text("Nombre de la rama", false, 12.0));
            rows.push(field(
                "environment_git_branch_name",
                &state.branch_name,
                "feature/mi-cambio",
                enabled,
            ));
            if worktree {
                if snapshot.head.is_none() {
                    rows.push(text(
                        "Creá el primer commit antes de crear un worktree.",
                        true,
                        11.0,
                    ));
                }
                rows.push(text("Carpeta de destino", false, 12.0));
                rows.push(field(
                    "environment_git_worktree_path",
                    &state.worktree_path,
                    "/ruta/al/worktree",
                    enabled,
                ));
            }
            rows.push(action(
                if worktree {
                    "environment_git_create_worktree"
                } else {
                    "environment_git_create_branch"
                },
                if worktree {
                    "Crear worktree"
                } else {
                    "Crear rama"
                },
                enabled
                    && !state.branch_name.trim().is_empty()
                    && (!worktree
                        || (snapshot.head.is_some() && !state.worktree_path.trim().is_empty())),
                true,
            ));
        }
        EnvironmentGitPanel::Overview | EnvironmentGitPanel::Actions => {}
    }
    Element::column(rows)
        .gap(10.0)
        .padding(8.0)
        .size(Some(width), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{HitKind, Size, UiTree};

    fn snapshot() -> GitSnapshot {
        let directory = tempfile::tempdir().unwrap();
        assert!(crate::child_process::command_no_window("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .unwrap()
            .success());
        crate::environment_git::load_snapshot(directory.path()).unwrap()
    }

    fn has_button(state: &EnvironmentGitUiState, id: &str, locked: bool) -> bool {
        UiTree::new(build(state, 360.0, locked))
            .layout(Size::new(360.0, 1000.0))
            .regions
            .iter()
            .any(|region| region.id == id && region.kind == HitKind::Button)
    }

    #[test]
    fn commit_requires_prepared_files_a_message_and_an_idle_environment() {
        let mut repository = snapshot();
        repository.staged = 1;
        let mut state = EnvironmentGitUiState {
            snapshot: Some(repository),
            panel: EnvironmentGitPanel::Commit,
            ..Default::default()
        };
        assert!(!has_button(&state, "environment_git_commit", false));
        state.commit_message = "Fix selected change".into();
        assert!(has_button(&state, "environment_git_commit", false));
        assert!(!has_button(&state, "environment_git_commit", true));
        state.snapshot.as_mut().unwrap().staged = 0;
        assert!(!has_button(&state, "environment_git_commit", false));
        state.snapshot.as_mut().unwrap().staged = 1;
        state.snapshot.as_mut().unwrap().conflicts = 1;
        assert!(!has_button(&state, "environment_git_commit", false));
    }

    #[test]
    fn push_requires_an_explicit_known_remote_and_a_branch() {
        let mut repository = snapshot();
        repository.remotes = vec!["origin".into(), "backup".into()];
        repository.head = Some("0123456789abcdef0123456789abcdef01234567".into());
        let mut state = EnvironmentGitUiState {
            snapshot: Some(repository),
            panel: EnvironmentGitPanel::Commit,
            push_branch: "main".into(),
            ..Default::default()
        };
        assert!(!has_button(&state, "environment_git_push", false));
        state.push_remote = "origin".into();
        assert!(has_button(&state, "environment_git_push", false));
        state.push_branch.clear();
        assert!(!has_button(&state, "environment_git_push", false));
        state.push_branch = "main".into();
        state.busy = Some("Publicando…".into());
        assert!(!has_button(&state, "environment_git_push", false));
    }

    fn review_fixture() -> EnvironmentGitUiState {
        // Isolated repository metadata supplies the branch identity. Counts
        // below are controlled view snapshots; no files are staged or pushed.
        let mut repository = snapshot();
        repository.staged = 3;
        repository.unstaged = 120;
        repository.untracked = 771;
        repository.head = Some("0123456789abcdef0123456789abcdef01234567".into());
        repository.remotes = vec!["origin".into(), "backup".into()];
        let branch = repository.current_branch().unwrap().to_owned();
        repository.upstream = Some(format!("origin/{branch}"));
        repository.ahead = 2;
        repository.behind = 1;
        EnvironmentGitUiState {
            snapshot: Some(repository),
            panel: EnvironmentGitPanel::Commit,
            commit_message: "Ajustar distribución del editor".into(),
            push_remote: "origin".into(),
            push_branch: branch,
            ..Default::default()
        }
    }

    fn review_surface(state: &EnvironmentGitUiState, size: Size) -> Element {
        openide_ui::ScrollArea::new(
            "git_workflow_review",
            build_sidebar(state, (size.width - 14.0).max(0.0), false),
        )
        .render()
        .background(theme::SIDEBAR)
        .size(Some(size.width), Some(size.height))
    }

    #[test]
    fn compact_commit_form_keeps_primary_secondary_actions_and_remote_fields_separate() {
        use openide_ui::{DrawCommand, Point, UiAction, UiEvent};
        for width in [240.0, 320.0, 420.0] {
            let state = review_fixture();
            let size = Size::new(width, 740.0);
            let (context, mut tree) =
                crate::view::ide_visual_tests::native_tree(review_surface(&state, size));
            let frame = tree.layout(size);
            let bounds = |id: &str| {
                frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap()
                    .rect
            };
            let commit = bounds("environment_git_commit");
            let push = bounds("environment_git_push");
            let message = bounds("environment_git_commit_message");
            let remote = bounds("environment_git_remote_toggle");
            let branch = bounds("environment_git_push_branch");
            assert_eq!(
                (commit.height, push.height, remote.height, branch.height),
                (28.0, 28.0, 28.0, 28.0)
            );
            assert!(message.y + message.height <= commit.y && commit.y + commit.height < remote.y);
            assert_eq!(remote.y, branch.y);
            assert!(remote.x + remote.width + 6.0 <= branch.x + 0.01);
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::FillRect { rect, color, .. } if *rect == commit && *color == theme::FOREGROUND
            )));
            assert!(!frame.commands.iter().any(|command| matches!(command,
                DrawCommand::FillRect { rect, color, .. } if *rect == push && *color == theme::FOREGROUND
            )), "publishing stays secondary to commit");
            // Scroll clipping intersects the input's hit region; subtracting
            // native fractional y coordinates can round its height slightly.
            // The painted field must still match that region on every edge.
            assert!((message.height - 80.0).abs() < 0.001);
            assert!(
                frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::FillRect { rect, color, .. }
                        if (rect.x - message.x).abs() < 0.001
                            && (rect.y - message.y).abs() < 0.001
                            && (rect.width - message.width).abs() < 0.001
                            && (rect.height - message.height).abs() < 0.001
                            && *color == theme::BACKGROUND
                )),
                "the message field remains distinct from the gray sidebar: {message:?}"
            );
            for label in ["Preparados", "Sin preparar", "Nuevos", "3", "120", "771"] {
                assert!(
                    frame.commands.iter().any(|command| matches!(command,
                        DrawCommand::Text { text, .. } if text == label
                    )),
                    "missing complete count/label {label} at {width}px"
                );
            }
            for region in &frame.regions {
                assert!(
                    region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.01,
                    "{} exceeds {width}px: {:?}",
                    region.id,
                    region.rect
                );
            }
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "environment_dismiss"));
            for id in [
                "dock_open:review",
                "environment_git_commit",
                "environment_git_push",
                "environment_git_back",
            ] {
                let rect = bounds(id);
                let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                assert_eq!(frame.hit_test(point), Some(id));
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked { id: id.into() }]
                );
            }
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn commit_prerequisites_are_visible_and_remote_menu_preserves_scoped_host_actions() {
        use openide_ui::{Point, UiAction, UiEvent};
        let mut state = review_fixture();
        for (staged, conflicts, message, reason) in [
            (
                0,
                0,
                "",
                "Prepará al menos un archivo para crear el commit.",
            ),
            (2, 0, "", "Escribí un mensaje para los cambios preparados."),
            (
                2,
                1,
                "Resolver",
                "Resolvé 1 conflicto antes de crear el commit.",
            ),
        ] {
            state.snapshot.as_mut().unwrap().staged = staged;
            state.snapshot.as_mut().unwrap().conflicts = conflicts;
            state.commit_message = message.into();
            let frame =
                UiTree::new(build_sidebar(&state, 240.0, false)).layout(Size::new(240.0, 900.0));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "environment_git_commit"
                    && region.kind == HitKind::Button));
            assert!(frame
                .text_layouts()
                .iter()
                .any(|block| block.source == reason));
        }
        state = review_fixture();
        state.remote_menu_open = true;
        let size = Size::new(240.0, 740.0);
        let mut tree = UiTree::new(review_surface(&state, size));
        tree.set_reduced_motion(true);
        let frame = tree.layout(size);
        let target = frame
            .regions
            .iter()
            .find(|region| region.id == "environment_git_remote:1")
            .unwrap()
            .rect;
        assert!(target.x >= 0.0 && target.x + target.width <= size.width);
        let point = Point::new(
            target.x + target.width / 2.0,
            target.y + target.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "environment_git_remote:1".into()
            }]
        );
        let card = UiTree::new(build(&state, 320.0, false)).layout(Size::new(320.0, 900.0));
        assert!(card
            .regions
            .iter()
            .any(|region| region.id == "environment_dismiss"));
    }

    #[test]
    #[ignore = "opt-in native compact Git commit and publication review; isolated snapshots only"]
    fn export_git_commit_visual_review() {
        let directory = std::path::Path::new("/tmp/openade-git-commit-review");
        std::fs::create_dir_all(directory).unwrap();
        let save = |frame: &openide_ui::Frame, size: Size, name: &str| {
            let path = directory.join(format!("{name}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(
                frame,
                &path,
                size.width,
                size.height,
            )
            .unwrap();
            let svg = resvg::usvg::Tree::from_str(
                &std::fs::read_to_string(&path).unwrap(),
                &crate::settings_ui::visual_qa::native_svg_options(),
            )
            .unwrap();
            let mut pixels =
                resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
            resvg::render(
                &svg,
                resvg::tiny_skia::Transform::identity(),
                &mut pixels.as_mut(),
            );
            pixels.save_png(path.with_extension("png")).unwrap();
        };
        for width in [240.0, 320.0, 420.0] {
            for phase in [
                "unprepared",
                "ready",
                "conflicts",
                "busy",
                "remote-menu",
                "no-remote",
            ] {
                let mut state = review_fixture();
                match phase {
                    "unprepared" => {
                        state.snapshot.as_mut().unwrap().staged = 0;
                        state.commit_message.clear();
                    }
                    "conflicts" => state.snapshot.as_mut().unwrap().conflicts = 2,
                    "busy" => state.busy = Some("Creando commit…".into()),
                    "remote-menu" => state.remote_menu_open = true,
                    "no-remote" => {
                        state.snapshot.as_mut().unwrap().remotes.clear();
                        state.snapshot.as_mut().unwrap().upstream = None;
                    }
                    _ => {}
                }
                let size = Size::new(width, 740.0);
                let (context, mut tree) =
                    crate::view::ide_visual_tests::native_tree(review_surface(&state, size));
                let frame = tree.layout(size);
                save(&frame, size, &format!("git-{width:.0}-{phase}"));
                context.end_pass().textures_delta.clear();
            }
        }
        let mut workspace = crate::view::ide_visual_tests::fixture();
        workspace.ide.activity = crate::ide_workspace::IdeActivity::Git;
        workspace.ide.explorer_width = 320.0;
        workspace.environment.workflow = review_fixture();
        let size = Size::new(1440.0, 900.0);
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
            crate::view::build_window_view(&workspace, size.width, size.height),
        );
        save(&tree.layout(size), size, "git-workspace-1440");
        context.end_pass().textures_delta.clear();
    }
}
