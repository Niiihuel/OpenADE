//! Full-pane native document surfaces, using the same compact resource chrome.
use crate::{
    document_preview::{MarkdownPreviewTab, PreviewMode, PreviewView},
    theme,
    view::ViewState,
};
use openide_preview::{Document, PreviewKind};
use openide_ui::{
    ButtonSize, ButtonVariant, Component, Element, FontFamily, Icon, IconButton, ScrollArea,
    Tooltip, TooltipPlacement,
};

const HEADER: f32 = crate::dock_tab_ui::HEADER_HEIGHT;

fn label(value: impl Into<String>) -> Element {
    Element::truncated_text(value)
        .font_family(FontFamily::Sans)
        .font_size(11.0)
        .foreground(theme::FOREGROUND_SUBTLE)
}

fn tool(
    view: &PreviewView,
    key: &str,
    icon: Icon,
    hint: &str,
    active: bool,
    enabled: bool,
) -> Element {
    action_tool(
        &format!("{}:{key}", view.chrome_action_prefix()),
        icon,
        hint,
        active,
        enabled,
    )
}

fn action_tool(id: &str, icon: Icon, hint: &str, active: bool, enabled: bool) -> Element {
    Tooltip::new(
        theme::interactive_surface(
            IconButton::new(id, icon, theme::component_palette())
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::IconExtraSmall)
                .active(active)
                .enabled(enabled)
                .render(),
        )
        .size(Some(24.0), Some(24.0)),
        id,
        hint,
        theme::component_palette(),
    )
    .placement(TooltipPlacement::Below)
    .render()
}

fn modes(view: &PreviewView) -> Vec<Element> {
    [
        ("source", Icon::Code, "Editar Markdown", PreviewMode::Source),
        (
            "preview",
            Icon::File,
            "Vista previa de Markdown",
            PreviewMode::Preview,
        ),
        (
            "split",
            Icon::PanelRight,
            "Código y vista previa",
            PreviewMode::Split,
        ),
    ]
    .into_iter()
    .map(|(key, icon, hint, mode)| tool(view, key, icon, hint, view.mode == mode, true))
    .collect()
}

fn preview_header(view: &PreviewView, width: f32, split: bool) -> Element {
    let mut actions = if view.kind == PreviewKind::Markdown && !split {
        modes(view)
    } else {
        Vec::new()
    };
    let mut used = actions.len() as f32 * 28.0;
    if view.kind == PreviewKind::Pdf {
        actions.push(tool(
            view,
            "previous",
            Icon::ArrowLeft,
            "Página anterior",
            false,
            !view.loading && view.page > 0,
        ));
        actions.push(
            label(if view.pages > 0 {
                format!("{} / {}", view.page + 1, view.pages)
            } else {
                "PDF".into()
            })
            .size(Some(52.0), Some(24.0))
            .center_content(),
        );
        actions.push(tool(
            view,
            "next",
            Icon::ArrowRight,
            "Página siguiente",
            false,
            !view.loading && view.page + 1 < view.pages,
        ));
        used += 112.0;
    }
    if view.kind != PreviewKind::Markdown {
        // The minus sign is textual because our icon family has no Minus glyph.
        let id = format!("{}:zoom_out", view.chrome_action_prefix());
        actions.push(
            Tooltip::new(
                theme::interactive_surface(Element::clickable(
                    &id,
                    label("−").font_size(18.0).center_content(),
                ))
                .size(Some(24.0), Some(24.0)),
                &id,
                "Alejar",
                theme::component_palette(),
            )
            .placement(TooltipPlacement::Below)
            .render(),
        );
        actions.push(tool(
            view,
            "fit",
            Icon::Expand,
            "Ajustar al panel",
            view.zoom.is_none(),
            true,
        ));
        actions.push(tool(view, "zoom_in", Icon::Add, "Acercar", false, true));
        used += 84.0;
    }
    actions.push(tool(
        view,
        "reload",
        Icon::Refresh,
        "Actualizar vista previa",
        false,
        !view.loading,
    ));
    used += 28.0;
    let path = if split {
        label("Vista previa")
            .size(Some((width - used - 12.0).max(0.0)), Some(24.0))
            .flex(1.0)
    } else {
        crate::code_editor_ui::path_breadcrumb(
            "document_preview_path",
            &view.scope.path.to_string_lossy(),
            false,
            (width - used - 12.0).max(0.0),
        )
        .flex(1.0)
    };
    let mut row = vec![path];
    row.extend(actions);
    Element::row(row)
        .gap(4.0)
        .padding(4.0)
        .size(Some(width), Some(HEADER))
        .clip_children()
}

fn preview_body(view: &PreviewView, width: f32, height: f32) -> Element {
    document_body(
        view,
        width,
        height,
        &view.action_prefix(),
        &view.scroll_id(),
    )
}

fn document_body(
    view: &PreviewView,
    width: f32,
    height: f32,
    action_prefix: &str,
    scroll_id: &str,
) -> Element {
    if let Some(error) = &view.error {
        return Element::column(vec![
            Element::spacer().flex(1.0),
            Element::text(error)
                .font_family(FontFamily::Sans)
                .font_size(13.0)
                .foreground(theme::FOREGROUND_SUBTLE)
                .padding(16.0),
            Element::spacer().flex(1.0),
        ])
        .size(Some(width), Some(height))
        .clip_children();
    }
    let Some(document) = view.document.as_deref() else {
        return Element::column(vec![
            Element::spacer().flex(1.0),
            label("Preparando vista previa…").center_content(),
            Element::spacer().flex(1.0),
        ])
        .size(Some(width), Some(height));
    };
    let content = match document {
        Document::Markdown(document) => {
            let content_width = (width - 40.0).max(0.0);
            let content = Element::panel(crate::view::document_markdown_ui::build(
                document,
                content_width,
                action_prefix,
            ))
            .padding(16.0);
            ScrollArea::new(scroll_id, content)
                .render()
                .size(Some(width), Some(height))
        }
        Document::Image(image) | Document::Pdf { image, .. } => {
            let inner_width = (width - 24.0).max(1.0);
            let inner_height = (height - 24.0).max(1.0);
            let scale = view.zoom.unwrap_or_else(|| {
                (inner_width / image.width as f32)
                    .min(inner_height / image.height as f32)
                    .min(1.0)
            });
            let image_width = (image.width as f32 * scale).max(1.0);
            let image_height = (image.height as f32 * scale).max(1.0);
            let raster = view
                .raster
                .clone()
                .map(Element::raster_image)
                .unwrap_or_else(|| label("No se pudo mostrar la imagen"))
                .size(Some(image_width), Some(image_height));
            let row = Element::row(vec![
                Element::spacer().flex(1.0),
                raster,
                Element::spacer().flex(1.0),
            ])
            .size(Some(inner_width.max(image_width)), Some(image_height));
            let canvas = Element::column(vec![
                Element::spacer().flex(1.0),
                row,
                Element::spacer().flex(1.0),
            ])
            .padding(8.0)
            .size(
                Some(inner_width.max(image_width) + 16.0),
                Some(inner_height.max(image_height) + 16.0),
            );
            ScrollArea::new(
                scroll_id,
                Element::horizontal_scroll_2d(format!("{scroll_id}_horizontal"), canvas)
                    .size(Some(width), Some(inner_height.max(image_height) + 16.0)),
            )
            .render()
            .size(Some(width), Some(height))
        }
    };
    content.background(theme::BACKGROUND).clip_children()
}

fn preview(view: &PreviewView, width: f32, height: f32, split: bool) -> Element {
    let busy = if view.loading && view.document.is_some() {
        20.0
    } else {
        0.0
    };
    let mut rows = vec![preview_header(view, width, split)];
    if busy > 0.0 {
        rows.push(label("Actualizando…").padding(4.0).size(None, Some(busy)));
    }
    rows.push(preview_body(view, width, (height - HEADER - busy).max(0.0)).flex(1.0));
    Element::column(rows)
        .size(Some(width), Some(height))
        .clip_children()
}

pub(crate) fn build(state: &ViewState, width: f32, height: f32) -> Option<Element> {
    let view = state.document_preview.as_ref()?;
    let editor = state.code_editor.as_ref();
    if view.kind == PreviewKind::Markdown && view.mode != PreviewMode::Preview {
        let Some(editor) = editor else {
            return Some(preview(view, width, height, false));
        };
        let source = |pane_width| {
            crate::code_editor_ui::build_with_preview(
                editor,
                &state.editor_workbench,
                pane_width,
                crate::ide_workspace::active(state),
                if crate::ide_workspace::active(state) {
                    Vec::new()
                } else {
                    vec![action_tool(
                        &format!("markdown_preview_open:{}", editor.tab_id),
                        Icon::PanelRight,
                        "Abrir vista previa al costado",
                        false,
                        true,
                    )]
                },
                state.keymap_profile,
                state.vim_mode,
            )
        };
        if view.mode == PreviewMode::Source {
            return Some(source(width));
        }
        let divider = Element::spacer().background(theme::BORDER);
        return Some(if width >= 640.0 {
            let half = (width - 1.0) / 2.0;
            Element::row(vec![
                source(half).size(Some(half), Some(height)),
                divider.size(Some(1.0), Some(height)),
                preview(view, half, height, true),
            ])
            .size(Some(width), Some(height))
            .clip_children()
        } else {
            let half = (height - 1.0) / 2.0;
            Element::column(vec![
                source(width).size(Some(width), Some(half)),
                divider.size(Some(width), Some(1.0)),
                preview(view, width, half, true),
            ])
            .size(Some(width), Some(height))
            .clip_children()
        });
    }
    Some(preview(view, width, height, false))
}

/// A preview is a separate editor group, including its independently closable
/// tab. Its render revision only identifies content links, never chrome.
pub(crate) fn secondary(tab: &MarkdownPreviewTab, width: f32, height: f32) -> Element {
    let prefix = tab.action_prefix();
    let path = tab.preview.scope.path.to_string_lossy();
    let name = tab
        .preview
        .scope
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let title = format!("Vista previa · {name}");
    let tab_width = (width - 62.0).clamp(0.0, 320.0);
    let preview_tab = crate::dock_tab_ui::rounded_tab(crate::dock_tab_ui::RoundedTab {
        id: format!("{prefix}:tab"),
        extent_id: format!("{prefix}:extent"),
        title: &title,
        help: &path,
        icon: Icon::File,
        active: true,
        width: tab_width,
        close: action_tool(
            &format!("{prefix}:close"),
            Icon::Close,
            "Cerrar vista previa",
            false,
            true,
        ),
        tooltip_placement: TooltipPlacement::Below,
    });
    let tools = Element::row(vec![
        action_tool(
            &format!("{prefix}:source"),
            Icon::Code,
            "Mostrar código fuente",
            false,
            true,
        ),
        action_tool(
            &format!("{prefix}:reload"),
            Icon::Refresh,
            "Actualizar vista previa",
            false,
            true,
        ),
    ])
    .gap(2.0)
    .size(Some(50.0), Some(24.0));
    let tabs = Element::row(vec![
        crate::dock_tab_ui::align_tabs(preview_tab),
        Element::spacer().flex(1.0),
    ])
    .size(None, Some(HEADER))
    .flex(1.0);
    let header = crate::dock_tab_ui::header(tabs, tools, width, 2.0, 2.0).clip_children();
    let breadcrumb = crate::code_editor_ui::path_breadcrumb(
        &format!("{prefix}:path"),
        &path,
        false,
        (width - 12.0).max(0.0),
    )
    .size(Some((width - 12.0).max(0.0)), Some(22.0));
    let breadcrumb = Element::row(vec![
        Element::spacer().size(Some(6.0), None),
        breadcrumb,
        Element::spacer().size(Some(6.0), None),
    ])
    .size(Some(width), Some(22.0))
    .clip_children();
    // Content retained during a refresh must not acquire the incoming render
    // identity: a pointer gesture spanning the response could target a new link.
    let content_prefix = if tab.preview.loading {
        format!("{prefix}:pending:{}", tab.preview.scope.generation)
    } else {
        tab.document_action_prefix()
    };
    let body = document_body(
        &tab.preview,
        width,
        (height - HEADER - 22.0).max(0.0),
        &content_prefix,
        &tab.scroll_id(),
    );
    Element::column(vec![header, breadcrumb, body.flex(1.0)])
        .size(Some(width), Some(height))
        .background(theme::BACKGROUND)
        .clip_children()
}
