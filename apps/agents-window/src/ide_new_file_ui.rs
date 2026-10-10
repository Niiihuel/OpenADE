//! File creation is a workspace action, including before its first editor exists.
use super::*;
use openide_ui::{ButtonGroup, FormField, Modal};

pub(super) fn modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    let Some(draft) = state.ide.new_file.as_ref() else {
        return base;
    };
    if !crate::ide_workspace::active(state) {
        return base;
    }
    let width = Modal::fit_width(viewport_width, 440.0).max(240.0);
    let body_width = crate::ui_sizing::form_width(width).max(0.0);
    // Hand-built input: 36 tall on the canvas' own fill; see the ledger.
    let mut path = FormField::new(
        "Ruta dentro del proyecto",
        field("ide_new_file_path", &draft.path, "src/nuevo.rs")
            .font_family(FontFamily::Monospace)
            .size(Some(body_width), Some(36.0)),
        theme::component_palette(),
    )
    .sizing(crate::ui_sizing::NEW_FILE_FIELD);
    if let Some(error) = &draft.error {
        path = path.error(error, theme::DESTRUCTIVE);
    }
    let cancel = Button::new(
        "ide_new_file_cancel",
        "Cancelar",
        theme::component_palette(),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Regular)
    .render();
    let create = theme::primary("ide_new_file_submit", "Crear")
        .size(ControlSize::Regular)
        .enabled(!draft.path.trim().is_empty() && draft.workspace == state.workspace)
        .render();
    Modal::new(
        base,
        true,
        "Nuevo archivo",
        format!("Crear en {}", workspace_name(&draft.workspace)),
        "ide_new_file_cancel",
        theme::component_palette(),
    )
    .width(width)
    .body(path.render().size(Some(body_width), None))
    .footer(
        ButtonGroup::new(vec![cancel, create])
            .align_end()
            .sizing(crate::ui_sizing::MODAL_FOOTER)
            .render(),
    )
    .initial_focus("ide_new_file_path")
    .return_focus(if viewport_width - 2.0 * OUTER_INSET >= 1200.0 {
        "ide_menu:file"
    } else {
        "ide_menu:all"
    })
    .render()
}
