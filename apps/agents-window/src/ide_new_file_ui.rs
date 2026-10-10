//! File creation is a workspace action, including before its first editor exists.
use super::*;
use openide_ui::Modal;

pub(super) fn modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    let Some(draft) = state.ide.new_file.as_ref() else {
        return base;
    };
    if !crate::ide_workspace::active(state) {
        return base;
    }
    let width = (viewport_width - 24.0).clamp(240.0, 440.0);
    let body_width = (width - 54.0).max(0.0);
    let mut body = vec![
        label("Ruta dentro del proyecto", theme::FOREGROUND).font_size(13.0),
        field("ide_new_file_path", &draft.path, "src/nuevo.rs")
            .font_family(FontFamily::Monospace)
            .size(Some(body_width), Some(36.0)),
    ];
    if let Some(error) = &draft.error {
        body.push(label(error, theme::DESTRUCTIVE).font_size(12.0));
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
    .body(Element::column(body).gap(8.0).size(Some(body_width), None))
    .footer(Element::row(vec![Element::spacer().flex(1.0), cancel, create]).gap(8.0))
    .initial_focus("ide_new_file_path")
    .return_focus(if viewport_width - 2.0 * OUTER_INSET >= 1200.0 {
        "ide_menu:file"
    } else {
        "ide_menu:all"
    })
    .render()
}
