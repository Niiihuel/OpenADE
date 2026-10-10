//! The hand-built project row, menu headers and menu messages that limn's
//! `MenuItem`, `MenuHeader` and `DropdownMenuEntry::Message` replaced, kept
//! so the replacements stay provably identical. Delete with the final audit
//! (S18).

use super::*;
use openide_ui::{Frame, Size, UiTree};

const WIDTHS: [f32; 2] = [236.0, 320.0];

fn frame(element: Element, width: f32) -> Frame {
    let mut tree = UiTree::new(
        Element::column(vec![Element::spacer().size(None, Some(420.0)), element])
            .size(Some(width), None),
    );
    tree.set_reduced_motion(true);
    tree.layout(Size::new(width, 900.0))
}

fn menu(header: Element, entries: Vec<DropdownMenuEntry>, width: f32) -> Element {
    DropdownMenu::new(
        Element::button("trigger", "Abrir").size(Some(80.0), Some(28.0)),
        entries,
        true,
        "dismiss",
        theme::menu_palette(),
    )
    .header(header)
    .context_menu_style()
    .width(width)
    .render()
}

fn legacy_project_row(
    index: usize,
    label: &str,
    path: &str,
    selected: bool,
    row_width: f32,
) -> Element {
    let sizing = SelectorSizing::CATALOG;
    let body_height = sizing.row_height;
    let row_padding = (sizing.detail_row_height - body_height) / 2.0;
    let text_width =
        (row_width - row_padding * 2.0 - sizing.icon_size * 2.0 - sizing.gap * 2.0).max(40.0);
    let visual = Element::row(vec![
        Element::icon(Icon::Folder)
            .foreground(theme::FOREGROUND)
            .size(Some(sizing.icon_size), Some(body_height)),
        Element::column(vec![
            Element::truncated_text(label)
                .font_family(FontFamily::Sans)
                .font_size(sizing.label_size)
                .foreground(theme::FOREGROUND)
                .size(Some(text_width), Some(body_height / 2.0)),
            Element::truncated_text(path)
                .font_family(FontFamily::Sans)
                .font_size(sizing.detail_size)
                .foreground(theme::MENU_MUTED)
                .size(Some(text_width), Some(body_height / 2.0)),
        ])
        .size(Some(text_width), Some(body_height)),
        if selected {
            Element::icon(Icon::Check).foreground(theme::FOREGROUND)
        } else {
            Element::spacer()
        }
        .size(Some(sizing.icon_size), Some(body_height)),
    ])
    .gap(sizing.gap)
    .clip_children();
    let row = theme::menu_palette()
        .interactive_surface(Element::clickable(
            format!("composer_project_select:{index}"),
            visual,
        ))
        .padding(row_padding)
        .size(Some(row_width), Some(sizing.detail_row_height));
    let row = row.action_menu_item().transition_ms(0).radius(4.0);
    Tooltip::new(
        row,
        format!("composer_project_select:{index}"),
        label,
        theme::menu_palette(),
    )
    .description(path)
    .placement(TooltipPlacement::Left)
    .max_width(sizing.tooltip_width)
    .render()
    .size(Some(row_width), Some(sizing.detail_row_height))
}

fn legacy_project_header(query: &str, inner_width: f32) -> Element {
    let sizing = SelectorSizing::CATALOG;
    Element::column(vec![
        Element::text("Proyectos")
            .font_family(FontFamily::Sans)
            .font_size(sizing.label_size)
            .foreground(theme::FOREGROUND)
            .padding(sizing.padding)
            .size(Some(inner_width), Some(sizing.header_height)),
        SearchField::new(
            "composer_project_search",
            query,
            "Buscar proyecto o ruta",
            theme::menu_palette(),
        )
        .size(ControlSize::Compact)
        .width(inner_width)
        .render(),
    ])
    .gap(2.0)
}

fn legacy_local_header() -> Element {
    Element::text("Entorno local")
        .font_family(FontFamily::Sans)
        .font_size(12.0)
        .foreground(theme::FOREGROUND)
        .padding(6.0)
        .size(None, Some(28.0))
}

fn legacy_message(text: &str) -> DropdownMenuEntry {
    DropdownMenuEntry::custom(
        Element::text(text)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::MENU_MUTED)
            .padding(6.0),
    )
}

/// The projects of the selector under test, the second one current.
fn projects() -> Vec<(String, String)> {
    vec![
        ("/home/usuario/proyectos/openADE".into(), "openADE".into()),
        (
            "/home/usuario/proyectos/un-proyecto-de-nombre-muy-largo/limn".into(),
            "limn".into(),
        ),
    ]
}

#[test]
fn the_project_selector_matches_its_hand_built_rows_and_header() {
    let state = crate::view::tests::empty_state();
    let options = projects();
    let workspace = options[1].0.clone();
    for query in ["", "lim"] {
        for width in WIDTHS {
            let mut props = ComposerProps::from_state(&state, width, false, None, "", None);
            props.project_menu_open = true;
            props.project_query = query;
            props.project_options = &options;
            props.workspace = &workspace;
            let trigger = || Element::button("trigger", "Abrir").size(Some(80.0), Some(28.0));
            let current =
                project_selector(&props, trigger(), PopoverPlacement::TopStart, "trigger");
            // The same menu from the hand-built parts.
            let sizing = SelectorSizing::CATALOG;
            let menu_width = sizing.fit_width(width - 16.0);
            let inner_width = menu_width - 12.0;
            let row_width = inner_width - sizing.scrollbar_gutter;
            let mut entries: Vec<DropdownMenuEntry> = projects()
                .iter()
                .enumerate()
                .filter(|(_, (path, label))| label.contains(query) || path.contains(query))
                .map(|(index, (path, label))| {
                    DropdownMenuEntry::custom(legacy_project_row(
                        index,
                        label,
                        path,
                        *path == workspace,
                        row_width,
                    ))
                })
                .collect();
            entries.push(DropdownMenuEntry::Separator);
            entries.push(
                DropdownMenuItem::new("composer_project_browse", "Abrir otra carpeta…")
                    .icon(Icon::Folder)
                    .into(),
            );
            let legacy = DropdownMenu::new(
                trigger(),
                entries,
                true,
                "composer_project_dismiss",
                theme::menu_palette(),
            )
            .header(legacy_project_header(query, inner_width))
            .context_menu_style()
            .sizing(sizing)
            .width(menu_width)
            .placement(PopoverPlacement::TopStart)
            .max_visible_items(sizing.max_visible_items)
            .render()
            .popover_initial_focus("composer_project_search")
            .popover_return_focus("trigger");
            assert!(
                frame(current, width) == frame(legacy, width),
                "{query:?} {width}"
            );
        }
    }
}

#[test]
fn the_local_menu_header_and_messages_match_their_hand_built_text() {
    for width in WIDTHS {
        for text in [
            "Actualizando carpetas de trabajo…",
            "No se pudo consultar Git en esta carpeta.",
            "Trabaja en esta carpeta local.",
        ] {
            let item = || DropdownMenuItem::new("environment_git_refresh", "Actualizar").into();
            let current = menu(
                MenuHeader::new(theme::menu_palette())
                    .title("Entorno local")
                    .render(),
                vec![DropdownMenuEntry::Message(text.into()), item()],
                width,
            );
            let legacy = menu(
                legacy_local_header(),
                vec![legacy_message(text), item()],
                width,
            );
            assert!(
                frame(current, width) == frame(legacy, width),
                "{text} {width}"
            );
        }
    }
}
