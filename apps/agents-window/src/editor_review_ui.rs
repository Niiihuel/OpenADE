//! Controlled review actions for the ordinary editor.
//! The host owns baseline, revision checks and writes; rendering only emits scoped actions.
//! Old OpenIDE's ReviewHeaderWidget reserves breadcrumb space, while its
//! ReviewBlockWidget provides text actions for the active change inside the editor.
use crate::theme;
use openide_ui::{
    Button, ButtonVariant, Component, ControlSize, DropdownMenu, DropdownMenuEntry,
    DropdownMenuItem, Element, Icon, IconButton, Pager, PopoverPlacement, Separator,
    TooltipPlacement, TooltipSpec,
};

pub(crate) const HEIGHT: f32 = 28.0;
const CONTROL: f32 = 24.0;
const GAP: f32 = 4.0;
const COUNT_WIDTH: f32 = 40.0;
const FILE_COUNT_WIDTH: f32 = 48.0;
const HUNKS_WIDTH: f32 = CONTROL * 2.0 + COUNT_WIDTH;
const FILES_WIDTH: f32 = CONTROL * 2.0 + FILE_COUNT_WIDTH;
const SEPARATOR_WIDTH: f32 = 9.0;

#[derive(Clone, Debug, Default)]
pub(crate) struct EditorReviewView {
    /// Opaque identity of this review snapshot; never infer an owner from the active tab.
    pub token: String,
    pub document_id: u64,
    pub revision: u64,
    pub hunks: std::sync::Arc<[openide_ui::CodeReviewHunk]>,
    pub actions: std::sync::Arc<[openide_ui::CodeReviewHunkActions]>,
    pub hunk_count: usize,
    pub current_hunk: usize,
    pub file_count: usize,
    pub current_file: usize,
    pub busy: bool,
    pub menu_open: bool,
    pub can_accept: bool,
    pub can_reject: bool,
    pub reject_reason: Option<String>,
    pub reason: Option<String>,
}

pub(crate) fn action(token: &str, operation: &str) -> String {
    format!("editor_review:{operation}:{token}")
}

fn decision_widths(pane_width: f32) -> (f32, f32) {
    if pane_width < 220.0 {
        (72.0, 76.0)
    } else {
        (80.0, 80.0)
    }
}

/// Share a single row with the filename. Navigation yields to the menu before
/// the resource loses its space; decisions always retain complete text labels.
pub(crate) fn controls_budget(available: f32) -> f32 {
    let filename = (available * 0.28).clamp(52.0, 160.0);
    (available - filename).max(180.0).min(available.max(0.0))
}

#[derive(Clone, Copy)]
struct Navigation {
    hunks: bool,
    files: bool,
    overflow: bool,
}

impl Navigation {
    fn width(self) -> f32 {
        (if self.hunks {
            HUNKS_WIDTH + SEPARATOR_WIDTH
        } else {
            0.0
        }) + (if self.files {
            FILES_WIDTH + SEPARATOR_WIDTH
        } else {
            0.0
        }) + (if self.overflow { CONTROL + GAP } else { 0.0 })
    }
}

fn navigation(review: &EditorReviewView, pane_width: f32, extra_menu: bool) -> Navigation {
    let (reject, accept) = decision_widths(pane_width);
    let budget = pane_width - reject - GAP - accept;
    let full = Navigation {
        hunks: review.hunk_count > 0,
        files: review.file_count > 1,
        overflow: extra_menu,
    };
    if full.width() <= budget {
        return full;
    }
    let compact = Navigation {
        files: false,
        overflow: true,
        ..full
    };
    if compact.width() <= budget {
        return compact;
    }
    Navigation {
        hunks: false,
        files: false,
        overflow: true,
    }
}

/// Reserve the controls before asking the shared breadcrumb to truncate.
pub(crate) fn width(review: &EditorReviewView, pane_width: f32) -> f32 {
    width_with_menu(review, pane_width, false)
}

pub(crate) fn width_with_menu(review: &EditorReviewView, pane_width: f32, extra_menu: bool) -> f32 {
    let (reject, accept) = decision_widths(pane_width);
    (navigation(review, pane_width, extra_menu).width() + reject + GAP + accept)
        .min(pane_width.max(0.0))
}

#[cfg(test)]
pub(crate) fn height(_review: &EditorReviewView, _pane_width: f32) -> f32 {
    HEIGHT
}

fn button(
    review: &EditorReviewView,
    operation: &str,
    text: &str,
    title: &str,
    width: f32,
    enabled: bool,
    variant: ButtonVariant,
) -> Element {
    let id = action(&review.token, operation);
    let reason = if operation == "reject_file" {
        review.reject_reason.as_deref().or(review.reason.as_deref())
    } else {
        review.reason.as_deref()
    };
    let tip = if !enabled {
        reason.unwrap_or(title)
    } else {
        title
    };
    let shortcut = match (cfg!(target_os = "macos"), operation == "reject_file") {
        (true, true) => "Cmd+Shift+Backspace",
        (true, false) => "Cmd+Enter",
        (false, true) => "Ctrl+Shift+Backspace",
        (false, false) => "Ctrl+Enter",
    };
    Button::new(id, text, theme::component_palette())
        .variant(variant)
        .size(ControlSize::Dense)
        .width(width)
        .enabled(enabled)
        .tooltip_with(
            TooltipSpec::new(tip)
                .shortcut(shortcut)
                .placement(TooltipPlacement::Below),
        )
        .render()
        .centered_y()
        .size(Some(width), Some(CONTROL))
}

fn navigation_shortcut(operation: &str) -> Option<&'static str> {
    match operation {
        "previous" => Some("Shift+F7"),
        "next" => Some("F7"),
        _ => None,
    }
}

/// Arrows around "position/count" for one of the strip's two sequences;
/// the counter's hint spells the position out.
fn pager(
    review: &EditorReviewView,
    operation: &str,
    position: usize,
    count: usize,
    width: f32,
    kind: &str,
) -> Pager {
    let position = if count == 0 {
        0
    } else {
        position.min(count - 1) + 1
    };
    Pager::new(format!("{position}/{count}"), theme::component_palette())
        .label_width(width)
        .label_tooltip(
            action(&review.token, operation),
            TooltipSpec::new(format!("{kind} {position} de {count}"))
                .placement(TooltipPlacement::Below),
        )
        .sizing(crate::ui_sizing::REVIEW_PAGER)
}

fn hunk_pager(review: &EditorReviewView) -> Element {
    pager(
        review,
        "position",
        review.current_hunk,
        review.hunk_count,
        COUNT_WIDTH,
        "Cambio",
    )
    .previous(
        action(&review.token, "previous"),
        review.current_hunk > 0 && review.hunk_count > 0 && !review.busy,
    )
    .next(
        action(&review.token, "next"),
        review.current_hunk.saturating_add(1) < review.hunk_count && !review.busy,
    )
    .icons(Icon::ArrowUp, Icon::ArrowDown)
    .tooltips(
        TooltipSpec::new("Cambio anterior")
            .shortcut(navigation_shortcut("previous").unwrap_or_default())
            .placement(TooltipPlacement::Below),
        TooltipSpec::new("Cambio siguiente")
            .shortcut(navigation_shortcut("next").unwrap_or_default())
            .placement(TooltipPlacement::Below),
    )
    .render()
}

fn file_pager(review: &EditorReviewView) -> Element {
    pager(
        review,
        "file_position",
        review.current_file,
        review.file_count,
        FILE_COUNT_WIDTH,
        "Archivo",
    )
    .previous(
        action(&review.token, "previous_file"),
        review.can_accept && review.current_file > 0 && !review.busy,
    )
    .next(
        action(&review.token, "next_file"),
        review.can_accept
            && review.current_file.saturating_add(1) < review.file_count
            && !review.busy,
    )
    .tooltips(
        TooltipSpec::new("Archivo anterior de esta conversación")
            .placement(TooltipPlacement::Below),
        TooltipSpec::new("Archivo siguiente de esta conversación")
            .placement(TooltipPlacement::Below),
    )
    .render()
}

fn navigation_menu(
    review: &EditorReviewView,
    layout: Navigation,
    extra: Vec<DropdownMenuEntry>,
    available_width: f32,
) -> Element {
    let item = |operation: &str, label: &str, icon: Icon, enabled: bool| {
        let mut item = DropdownMenuItem::new(action(&review.token, operation), label).icon(icon);
        if let Some(shortcut) = navigation_shortcut(operation) {
            item = item.shortcut(shortcut);
        }
        DropdownMenuEntry::Item(if enabled && !review.busy {
            item
        } else {
            item.disabled()
        })
    };
    let mut entries = Vec::new();
    if !layout.hunks && review.hunk_count > 0 {
        entries.extend([
            DropdownMenuEntry::Heading(format!(
                "Cambio {} de {}",
                review.current_hunk.min(review.hunk_count - 1) + 1,
                review.hunk_count
            )),
            item(
                "previous",
                "Cambio anterior",
                Icon::ArrowUp,
                review.current_hunk > 0,
            ),
            item(
                "next",
                "Cambio siguiente",
                Icon::ArrowDown,
                review.current_hunk.saturating_add(1) < review.hunk_count,
            ),
        ]);
    }
    if !layout.files && review.file_count > 1 {
        entries.extend([
            DropdownMenuEntry::Heading(format!(
                "Archivo {} de {}",
                review.current_file.min(review.file_count - 1) + 1,
                review.file_count
            )),
            item(
                "previous_file",
                "Archivo anterior",
                Icon::ArrowLeft,
                review.can_accept && review.current_file > 0,
            ),
            item(
                "next_file",
                "Archivo siguiente",
                Icon::ArrowRight,
                review.can_accept && review.current_file.saturating_add(1) < review.file_count,
            ),
        ]);
    }
    let has_editor_actions = !extra.is_empty();
    if has_editor_actions {
        if !entries.is_empty() {
            entries.push(DropdownMenuEntry::Separator);
        }
        entries.extend(extra);
    }
    let anchor = IconButton::new(
        action(&review.token, "menu_toggle"),
        Icon::More,
        theme::component_palette(),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Dense)
    .active(review.menu_open)
    .tooltip_with(
        TooltipSpec::new(if has_editor_actions {
            "Cambios y acciones del editor"
        } else {
            "Navegar por los cambios"
        })
        .placement(TooltipPlacement::Below),
    )
    .render();
    DropdownMenu::new(
        anchor,
        entries,
        review.menu_open,
        action(&review.token, "menu_dismiss"),
        theme::menu_palette(),
    )
    .context_menu_style()
    .width((if has_editor_actions { 352.0_f32 } else { 232.0 }).min(available_width))
    .hints_in_tooltips(has_editor_actions && available_width < 320.0)
    .placement(PopoverPlacement::BottomEnd)
    .render()
}

pub(crate) fn controls(review: &EditorReviewView, pane_width: f32) -> Element {
    controls_with_menu(review, pane_width, Vec::new(), 232.0)
}

pub(crate) fn controls_with_menu(
    review: &EditorReviewView,
    pane_width: f32,
    extra: Vec<DropdownMenuEntry>,
    menu_width: f32,
) -> Element {
    let extra_menu = !extra.is_empty();
    let layout = navigation(review, pane_width, extra_menu);
    let mut navigation = Vec::new();
    if layout.hunks {
        navigation.push(hunk_pager(review));
        navigation.push(
            Separator::vertical(theme::component_palette())
                .length(14.0)
                .centered_in(CONTROL)
                .margin(4.0)
                .render()
                .size(Some(SEPARATOR_WIDTH), Some(CONTROL)),
        );
    }
    if layout.files {
        navigation.push(file_pager(review));
        navigation.push(
            Separator::vertical(theme::component_palette())
                .length(14.0)
                .centered_in(CONTROL)
                .margin(4.0)
                .render()
                .size(Some(SEPARATOR_WIDTH), Some(CONTROL)),
        );
    }
    if layout.overflow {
        navigation.push(navigation_menu(review, layout, extra, menu_width));
        navigation.push(Element::spacer().size(Some(GAP), None));
    }
    let (reject_width, accept_width) = decision_widths(pane_width);
    let reject = button(
        review,
        "reject_file",
        "Deshacer",
        "Deshacer los cambios pendientes de este archivo",
        reject_width,
        review.can_reject && !review.busy,
        ButtonVariant::Outline,
    );
    let accept = button(
        review,
        "accept_file",
        "Conservar",
        "Conservar todos los cambios pendientes de este archivo",
        accept_width,
        review.can_accept && !review.busy,
        ButtonVariant::Default,
    );
    let decisions = vec![reject, Element::spacer().size(Some(GAP), None), accept];
    let controls_width = width_with_menu(review, pane_width, extra_menu);
    navigation.extend(decisions);
    Element::row(navigation)
        .size(None, Some(CONTROL))
        .centered_y()
        .size(Some(controls_width), Some(HEIGHT))
}
