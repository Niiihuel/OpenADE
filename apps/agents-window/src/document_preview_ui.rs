//! Full-pane native document surfaces, using the same compact resource chrome.
use crate::{
    document_preview::{MarkdownPreviewTab, PreviewMode, PreviewView},
    theme,
    view::ViewState,
};
use openide_preview::{Document, PreviewKind};
use openide_ui::{
    ActiveHover, ButtonVariant, Color, Component, ControlSize, Element, FontFamily, Icon,
    IconButton, Pager, PanelHeader, ScrollArea, Separator, TabItem, TabStrip, Text, TextRole,
    TooltipPlacement, TooltipSpec,
};

const HEADER: f32 = crate::dock_tab_ui::HEADER_HEIGHT;

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
    .map(|(key, icon, hint, mode)| {
        IconButton::new(
            format!("{}:{key}", view.chrome_action_prefix()),
            icon,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .active(view.mode == mode)
        .active_hover(ActiveHover::Palette)
        .tooltip_with(TooltipSpec::new(hint).placement(TooltipPlacement::Below))
        .render()
    })
    .collect()
}

/// Previous page, the position and next page of a PDF.
fn pdf_pager(view: &PreviewView) -> Element {
    Pager::new(
        if view.pages > 0 {
            format!("{} / {}", view.page + 1, view.pages)
        } else {
            "PDF".into()
        },
        theme::component_palette(),
    )
    .previous(
        format!("{}:previous", view.chrome_action_prefix()),
        !view.loading && view.page > 0,
    )
    .next(
        format!("{}:next", view.chrome_action_prefix()),
        !view.loading && view.page + 1 < view.pages,
    )
    .tooltips(
        TooltipSpec::new("Página anterior").placement(TooltipPlacement::Below),
        TooltipSpec::new("Página siguiente").placement(TooltipPlacement::Below),
    )
    .label_width(52.0)
    .render()
}

fn preview_header(view: &PreviewView, width: f32, split: bool) -> Element {
    let mut actions = if view.kind == PreviewKind::Markdown && !split {
        modes(view)
    } else {
        Vec::new()
    };
    let mut used = actions.len() as f32 * 28.0;
    if view.kind == PreviewKind::Pdf {
        actions.push(pdf_pager(view));
        used += 112.0;
    }
    if view.kind != PreviewKind::Markdown {
        actions.push(
            IconButton::new(
                format!("{}:zoom_out", view.chrome_action_prefix()),
                Icon::Minus,
                // The text "−" this replaced was drawn in the label ink.
                theme::action_palette(Color::TRANSPARENT, theme::FOREGROUND_SUBTLE),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .tooltip_with(
                TooltipSpec::new("Alejar")
                    .placement(TooltipPlacement::Below)
                    .palette(theme::component_palette()),
            )
            .render(),
        );
        actions.push(
            IconButton::new(
                format!("{}:fit", view.chrome_action_prefix()),
                Icon::Expand,
                theme::component_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .active(view.zoom.is_none())
            .active_hover(ActiveHover::Palette)
            .tooltip_with(TooltipSpec::new("Ajustar al panel").placement(TooltipPlacement::Below))
            .render(),
        );
        actions.push(
            IconButton::new(
                format!("{}:zoom_in", view.chrome_action_prefix()),
                Icon::Add,
                theme::component_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .tooltip_with(TooltipSpec::new("Acercar").placement(TooltipPlacement::Below))
            .render(),
        );
        used += 84.0;
    }
    actions.push(
        IconButton::new(
            format!("{}:reload", view.chrome_action_prefix()),
            Icon::Refresh,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .enabled(!view.loading)
        .tooltip_with(
            TooltipSpec::new("Actualizar vista previa").placement(TooltipPlacement::Below),
        )
        .render(),
    );
    used += 28.0;
    let path = if split {
        Text::new("Vista previa", theme::component_palette())
            .role(TextRole::Small)
            .muted()
            .truncate()
            .render()
            .size(Some((width - used - 12.0).max(0.0)), Some(24.0))
    } else {
        crate::code_editor_ui::path_breadcrumb(
            "document_preview_path",
            &view.scope.path.to_string_lossy(),
            false,
            (width - used - 12.0).max(0.0),
        )
    };
    PanelHeader::new("", theme::component_palette())
        .leading(path)
        .trailing(actions)
        .width(width)
        .sizing(crate::ui_sizing::PANE_HEADER)
        .render()
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
        return Element::text(error)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .foreground(theme::FOREGROUND_SUBTLE)
            .padding(16.0)
            .centered_y()
            .size(Some(width), Some(height))
            .clip_children();
    }
    let Some(document) = view.document.as_deref() else {
        return Text::new("Preparando vista previa…", theme::component_palette())
            .role(TextRole::Small)
            .muted()
            .truncate()
            .render()
            .center_content()
            .centered_y()
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
                .unwrap_or_else(|| {
                    Text::new("No se pudo mostrar la imagen", theme::component_palette())
                        .role(TextRole::Small)
                        .muted()
                        .truncate()
                        .render()
                })
                .size(Some(image_width), Some(image_height));
            let row = raster
                .centered_x()
                .size(Some(inner_width.max(image_width)), Some(image_height));
            let canvas = row.centered_y().padding(8.0).size(
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
        rows.push(
            Text::new("Actualizando…", theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .truncate()
                .render()
                .padding(4.0)
                .size(None, Some(busy)),
        );
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
            crate::code_editor_ui::build_with(
                editor,
                &state.editor_workbench,
                pane_width,
                crate::code_editor_ui::EditorPresentation::beside_preview(
                    crate::ide_workspace::active(state),
                    if crate::ide_workspace::active(state) {
                        Vec::new()
                    } else {
                        vec![IconButton::new(
                            &format!("markdown_preview_open:{}", editor.tab_id),
                            Icon::PanelRight,
                            theme::component_palette(),
                        )
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Dense)
                        .tooltip_with(
                            TooltipSpec::new("Abrir vista previa al costado")
                                .placement(TooltipPlacement::Below),
                        )
                        .render()]
                    },
                ),
                state.keymap_profile,
                state.vim_mode,
            )
        };
        if view.mode == PreviewMode::Source {
            return Some(source(width));
        }
        let palette = theme::component_palette();
        return Some(if width >= 640.0 {
            let half = (width - 1.0) / 2.0;
            Element::row(vec![
                source(half).size(Some(half), Some(height)),
                Separator::vertical(palette).length(height).render(),
                preview(view, half, height, true),
            ])
            .size(Some(width), Some(height))
            .clip_children()
        } else {
            let half = (height - 1.0) / 2.0;
            Element::column(vec![
                source(width).size(Some(width), Some(half)),
                Separator::horizontal(palette).length(width).render(),
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
    let preview_tab = TabItem::new(
        format!("{prefix}:tab"),
        format!("{prefix}:extent"),
        Icon::File,
        title,
        tab_width,
    )
    .tooltip(&*path)
    .tooltip_placement(TooltipPlacement::Below)
    .active(true)
    .close(
        IconButton::new(
            &format!("{prefix}:close"),
            Icon::Close,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .sizing(crate::ui_sizing::TAB_CLOSE)
        .tooltip_with(TooltipSpec::new("Cerrar vista previa").placement(TooltipPlacement::Below))
        .render(),
    )
    .render(theme::tab_strip_palette());
    let tools = Element::row(vec![
        IconButton::new(
            &format!("{prefix}:source"),
            Icon::Code,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(TooltipSpec::new("Mostrar código fuente").placement(TooltipPlacement::Below))
        .render(),
        IconButton::new(
            &format!("{prefix}:reload"),
            Icon::Refresh,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(
            TooltipSpec::new("Actualizar vista previa").placement(TooltipPlacement::Below),
        )
        .render(),
    ])
    .gap(2.0)
    .size(Some(50.0), Some(24.0));
    let tabs = Element::row(vec![
        TabStrip::align(preview_tab),
        Element::spacer().flex(1.0),
    ])
    .size(None, Some(HEADER))
    .flex(1.0);
    let header = TabStrip::header(tabs, tools, width, theme::tab_strip_palette())
        .side_inset(2.0)
        .gap(2.0)
        .render()
        .clip_children();
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
