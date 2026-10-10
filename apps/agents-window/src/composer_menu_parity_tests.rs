//! The hand-built rows, headers and footer of the model menu that limn's
//! `DropdownMenuItem`, `MenuItem`, `DropdownMenuSection`, `MenuHeader` and
//! `DropdownMenu::footer` replaced, kept so the replacements stay provably
//! identical. Delete with the final audit (S18).

use super::*;
use openide_ui::{Button, DrawCommand, ElementKind, Frame, SearchField, Size, UiTree};

const WIDTHS: [f32; 2] = [236.0, 320.0];

fn frame(element: Element, width: f32) -> Frame {
    let mut tree = UiTree::new(
        Element::column(vec![Element::spacer().size(None, Some(420.0)), element])
            .size(Some(width), None),
    );
    tree.set_reduced_motion(true);
    tree.layout(Size::new(width, 900.0))
}

fn menu(entries: Vec<DropdownMenuEntry>, width: f32) -> DropdownMenu {
    DropdownMenu::new(
        Element::button("trigger", "Abrir").size(Some(80.0), Some(28.0)),
        entries,
        true,
        "dismiss",
        theme::selector_palette(),
    )
    .context_menu_style()
    .width(width)
    .max_visible_items(8)
}

fn legacy_selector_row(id: &str, title: &str, value: &str, icon: Icon, width: f32) -> Element {
    let sizing = SelectorSizing::COMPACT;
    let body_height = sizing.detail_row_height - sizing.padding * 2.0;
    theme::selector_palette()
        .interactive_surface(
            Element::clickable(
                id,
                Element::row(vec![
                    Element::icon(icon)
                        .foreground(theme::MENU_MUTED)
                        .size(Some(sizing.icon_size), Some(body_height)),
                    Element::column(vec![
                        Element::truncated_text(title)
                            .font_family(FontFamily::Sans)
                            .font_size(sizing.label_size)
                            .foreground(theme::FOREGROUND)
                            .size(None, Some(body_height / 2.0)),
                        Element::truncated_text(value)
                            .font_family(FontFamily::Sans)
                            .font_size(sizing.detail_size)
                            .foreground(theme::MENU_MUTED)
                            .size(None, Some(body_height / 2.0)),
                    ])
                    .flex(1.0),
                    Element::icon(Icon::ChevronRight)
                        .foreground(theme::MENU_MUTED)
                        .size(Some(sizing.icon_size), Some(body_height)),
                ])
                .gap(sizing.gap),
            )
            .action_menu_item(),
        )
        .padding(sizing.padding)
        .size(Some(width), Some(sizing.detail_row_height))
}


fn legacy_model_provider_section(
    provider_id: &str,
    label: &str,
    count: usize,
    collapsed: bool,
    width: f32,
) -> Element {
    let palette = theme::selector_palette();
    let sizing = SelectorSizing::CATALOG;
    palette
        .interactive_surface(
            Element::clickable(
                format!("composer_model_provider_toggle:{provider_id}"),
                Element::row(vec![
                    Element::icon(icon_for_provider(provider_id))
                        .foreground(palette.muted)
                        .size(Some(sizing.icon_size), Some(sizing.icon_size)),
                    Element::truncated_text(label)
                        .font_family(FontFamily::Sans)
                        .font_size(sizing.label_size)
                        .foreground(palette.muted)
                        .flex(1.0),
                    Element::text(count.to_string())
                        .font_family(FontFamily::Sans)
                        .font_size(sizing.detail_size)
                        .foreground(palette.muted),
                    Element::icon(if collapsed {
                        Icon::ChevronRight
                    } else {
                        Icon::ChevronDown
                    })
                    .foreground(palette.muted)
                    .size(Some(sizing.icon_size), Some(sizing.icon_size)),
                ])
                .gap(sizing.gap),
            )
            .action_menu_item(),
        )
        .padding(sizing.padding)
        .size(Some(width), Some(sizing.row_height))
}

struct LegacyModelPickerRow<'a> {
    action_id: &'a str,
    title: &'a str,
    detail: &'a str,
    status: &'a str,
    icon: Icon,
    width: f32,
    selected: bool,
    enabled: bool,
}

impl LegacyModelPickerRow<'_> {
    fn render(self) -> Element {
        let palette = theme::selector_palette();
        let sizing = SelectorSizing::CATALOG;
        let body_height = sizing.row_height;
        let row_padding = (sizing.detail_row_height - body_height) / 2.0;
        let text_width =
            (self.width - row_padding * 2.0 - sizing.icon_size * 2.0 - sizing.gap * 2.0).max(0.0);
        let status_width = if self.status.is_empty() {
            0.0
        } else {
            (self.status.chars().count() as f32 * 5.8 + 10.0).min(text_width * 0.46)
        };
        let mut detail = vec![
            Element::truncated_text(self.detail)
                .font_family(FontFamily::Monospace)
                .font_size(sizing.detail_size)
                .foreground(palette.muted)
                .flex(1.0)
                .size(None, Some(body_height / 2.0)),
        ];
        if status_width > 0.0 {
            detail.push(
                Element::truncated_text(self.status)
                    .font_family(FontFamily::Sans)
                    .font_size(sizing.detail_size)
                    .foreground(palette.muted)
                    .size(Some(status_width), Some(body_height / 2.0)),
            );
        }
        let visual = Element::row(vec![
            Element::icon(self.icon)
                .foreground(if self.enabled {
                    palette.text
                } else {
                    palette.muted
                })
                .size(Some(sizing.icon_size), Some(body_height)),
            Element::column(vec![
                Element::truncated_text(self.title)
                    .font_family(FontFamily::Sans)
                    .font_size(sizing.label_size)
                    .foreground(if self.enabled {
                        palette.text
                    } else {
                        palette.muted
                    })
                    .size(Some(text_width), Some(body_height / 2.0)),
                Element::row(detail)
                    .gap(sizing.gap)
                    .size(Some(text_width), Some(body_height / 2.0)),
            ])
            .size(Some(text_width), Some(body_height)),
            if self.selected {
                Element::icon(Icon::Check).foreground(palette.text)
            } else {
                Element::spacer()
            }
            .size(Some(sizing.icon_size), Some(body_height)),
        ])
        .gap(sizing.gap)
        .clip_children();
        let row = if self.enabled {
            Element::clickable(self.action_id, visual)
                .action_menu_item()
                .transition_ms(0)
        } else {
            Element::panel(visual)
        };
        palette
            .interactive_surface(row)
            .transition_ms(0)
            .background(Color::TRANSPARENT)
            .padding(row_padding)
            .size(Some(self.width), Some(sizing.detail_row_height))
    }
}


fn legacy_model_list_header(width: f32) -> Element {
    let sizing = SelectorSizing::CATALOG;
    Element::row(vec![
        IconButton::new(
            "composer_model_list_toggle",
            Icon::ArrowLeft,
            theme::selector_palette(),
        )
        .label("Modelo")
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Compact)
        .width((width - ControlSize::Compact.height() - sizing.gap).max(0.0))
        .render(),
        IconButton::new("composer_routes_refresh", Icon::Refresh, theme::selector_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .tooltip("Actualizar listado de proveedores")
            .tooltip_placement(TooltipPlacement::Below)
            .render()
            .size(
                Some(ControlSize::Compact.height()),
                Some(ControlSize::Compact.height()),
            ),
    ])
    .gap(sizing.gap)
    .size(Some(width), Some(sizing.header_height))
}

fn legacy_model_list_fixed_header(props: &ComposerProps<'_>, width: f32) -> Element {
    let mut palette = theme::selector_palette();
    palette.raised = theme::INPUT;
    Element::column(vec![
        legacy_model_list_header(width),
        SearchField::new(
            "composer_model_search",
            props.model_query,
            "Buscar modelos o proveedores",
            palette,
        )
        .size(ControlSize::Compact)
        .width(width)
        .render(),
    ])
    .gap(2.0)
}


#[test]
fn the_model_header_matches_its_hand_built_row_and_search() {
    let mut state = crate::view::tests::empty_state();
    for query in ["", "son"] {
        state.composer_model_query = query.into();
        for width in WIDTHS {
            let props = ComposerProps::from_state(&state, width, false, None, "", None);
            assert_eq!(
                frame(model_list_fixed_header(&props, width - 8.0), width),
                frame(legacy_model_list_fixed_header(&props, width - 8.0), width),
                "{query:?} {width}"
            );
        }
    }
}

#[test]
fn a_model_row_matches_its_hand_built_body_in_every_state() {
    for width in WIDTHS {
        for (selected, enabled, status) in [
            (false, true, ""),
            (true, true, "Revisado"),
            (false, false, "No compatible"),
            (true, false, "Sin verificar"),
        ] {
            let inner = width - 24.0;
            let current = ModelPickerRow {
                action_id: "composer_route:a",
                title: "Claude Sonnet 4.5 con un nombre largo de catálogo",
                detail: "anthropic/claude-sonnet-4-5-20250929",
                status,
                icon: Icon::Brain,
                width: inner,
                selected,
                enabled,
            }
            .render();
            let legacy = LegacyModelPickerRow {
                action_id: "composer_route:a",
                title: "Claude Sonnet 4.5 con un nombre largo de catálogo",
                detail: "anthropic/claude-sonnet-4-5-20250929",
                status,
                icon: Icon::Brain,
                width: inner,
                selected,
                enabled,
            }
            .render();
            // The hand-built row set a transparent background, which paints
            // a fill nobody can see; the limn row sets none.
            let mut legacy = frame(menu(vec![DropdownMenuEntry::custom(legacy)], width).render(), width);
            legacy.commands.retain(|command| {
                !matches!(command, DrawCommand::FillRect { color, .. } if color.a == 0)
            });
            let current = frame(menu(vec![DropdownMenuEntry::custom(current)], width).render(), width);
            assert!(current == legacy, "{width} {selected} {enabled} {status:?}");
        }
    }
}

#[test]
fn a_provider_section_matches_its_hand_built_heading() {
    for width in WIDTHS {
        for collapsed in [false, true] {
            let inner = width - 24.0;
            let current = model_provider_section("anthropic", "Anthropic · En tu cuenta", 12, collapsed, inner);
            let legacy = DropdownMenuEntry::custom(legacy_model_provider_section(
                "anthropic",
                "Anthropic · En tu cuenta",
                12,
                collapsed,
                inner,
            ));
            let row = || DropdownMenuItem::new("composer_plugin_documents", "Documentos").into();
            assert_eq!(
                frame(menu(vec![current, row()], width).render(), width),
                frame(menu(vec![legacy, row()], width).render(), width),
                "{width} {collapsed}"
            );
        }
    }
}

/// The frame without its clip brackets. The limn row clips its content as
/// every other catalog row does; the hand-built one let it overflow.
fn unclipped(frame: Frame) -> (Vec<DrawCommand>, Vec<String>) {
    (
        frame
            .commands
            .into_iter()
            .filter(|command| !matches!(command, DrawCommand::PushClip(_) | DrawCommand::PopClip))
            .collect(),
        frame.regions.iter().map(|region| format!("{region:?}")).collect(),
    )
}

#[test]
fn the_first_view_rows_match_their_hand_built_bodies() {
    for width in WIDTHS {
        let current = vec![
            DropdownMenuEntry::Heading("Para el próximo mensaje".into()),
            selector_row("composer_model_list_toggle", "Modelo", "Claude Sonnet 4.5", Icon::Brain),
            DropdownMenuEntry::Separator,
            selector_row("composer_advanced_toggle", "Esfuerzo", "Automático", Icon::Brain),
        ];
        let legacy = vec![
            DropdownMenuEntry::Heading("Para el próximo mensaje".into()),
            DropdownMenuEntry::custom(legacy_selector_row(
                "composer_model_list_toggle",
                "Modelo",
                "Claude Sonnet 4.5",
                Icon::Brain,
                width - 8.0,
            )),
            DropdownMenuEntry::Separator,
            DropdownMenuEntry::custom(legacy_selector_row(
                "composer_advanced_toggle",
                "Esfuerzo",
                "Automático",
                Icon::Brain,
                width - 8.0,
            )),
        ];
        assert_eq!(
            unclipped(frame(menu(current, width).render(), width)),
            unclipped(frame(menu(legacy, width).render(), width)),
            "{width}"
        );
    }
}

fn legacy_footer(mut menu: Element, menu_width: f32) -> Element {
    let sizing = SelectorSizing::CATALOG;
    if let ElementKind::Popover { floating, .. } = &mut menu.kind {
        if let ElementKind::Panel(content) = &mut floating.kind {
            let body = std::mem::replace(content.as_mut(), Element::spacer());
            **content = Element::column(vec![
                body.flex(1.0),
                Element::spacer()
                    .background(theme::BORDER)
                    .size(None, Some(1.0)),
                Button::new(
                    "composer_connect_provider",
                    "Conectar proveedor",
                    theme::selector_palette(),
                )
                .icon(Icon::Add)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .width(menu_width - 8.0)
                .render(),
            ])
            .gap(sizing.gap);
        }
    }
    menu
}

#[test]
fn the_connect_footer_matches_the_one_injected_into_the_popover() {
    let state = crate::view::tests::empty_state();
    for width in WIDTHS {
        for rows in [2, 30] {
            let props = ComposerProps::from_state(&state, width, false, None, "", None);
            let catalog = |footer: bool| {
                let entries = (0..rows)
                    .map(|index| {
                        DropdownMenuEntry::custom(
                            ModelPickerRow {
                                action_id: &format!("composer_route:{index}"),
                                title: "Modelo",
                                detail: "proveedor/modelo",
                                status: "",
                                icon: Icon::Brain,
                                width: width - 24.0,
                                selected: index == 1,
                                enabled: true,
                            }
                            .render(),
                        )
                    })
                    .collect();
                let menu = menu(entries, width).header(model_list_fixed_header(&props, width - 8.0));
                if footer {
                    menu.footer(connect_provider_action(width - 8.0))
                } else {
                    menu
                }
            };
            assert_eq!(
                frame(catalog(true).render(), width),
                frame(legacy_footer(catalog(false).render(), width), width),
                "{width} {rows}"
            );
        }
    }
}

fn review_routes() -> Vec<ModelRouteOption> {
    [
        ("openai", "OpenAI", "gpt-6-sol", "GPT-6 Sol", ExecutionSupport::Verified),
        ("openai", "OpenAI", "gpt-6-luna", "GPT-6 Luna", ExecutionSupport::ProviderAdvertisedUnverified),
        ("anthropic", "Anthropic", "claude-sonnet-4-5", "Claude Sonnet 4.5", ExecutionSupport::Verified),
        ("anthropic", "Anthropic", "claude-haiku-4-5", "Claude Haiku 4.5", ExecutionSupport::ProviderListedUnverified),
        ("anthropic", "Anthropic", "claude-opus-4-1", "Claude Opus 4.1", ExecutionSupport::Verified),
    ]
    .into_iter()
    .map(|(provider_id, provider_name, id, name, execution)| ModelRouteOption {
        route_id: format!("native:{provider_id}:{id}"),
        backend: "native".into(),
        provider_id: provider_id.into(),
        provider_name: provider_name.into(),
        model_id: id.into(),
        model_name: name.into(),
        credential: CredentialAvailability::EnvPresentUnverified,
        execution,
        source: RouteSource::AdapterReview,
        catalog_present: true,
        provider_listed: Some(true),
        effort_options: Vec::new(),
    })
    .collect()
}

/// Writes one PNG per menu and width under `target/menus-popovers-visual-qa`.
#[test]
#[ignore = "export the menus, selectors, suggestion lists and popover panels"]
fn export_menus_popovers_visual_review() {
    use crate::{
        composer_suggestions::{self, ComposerSuggestionsState},
        settings_ui::visual_qa,
    };
    use std::{fs, path::Path};

    const HEIGHT: f32 = 620.0;
    let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/menus-popovers-visual-qa");
    fs::create_dir_all(&output).unwrap();
    let options = visual_qa::native_svg_options();
    let routes = review_routes();
    let efforts = vec![
        ("default".to_owned(), "Automático".to_owned()),
        ("low".to_owned(), "Bajo".to_owned()),
        ("high".to_owned(), "Alto".to_owned()),
    ];
    let projects = vec![
        ("/home/usuario/proyectos/openADE".to_owned(), "openADE".to_owned()),
        ("/home/usuario/proyectos/personal/limn".to_owned(), "limn".to_owned()),
    ];
    // Leaked so the props, which borrow for any lifetime, can hold it.
    let collapsed: &'static BTreeSet<String> = Box::leak(Box::new(["openai".to_owned()].into()));
    let composer = |edit: &dyn Fn(&mut ComposerProps<'_>), width: f32| {
        let mut props = super::tests::props("", width);
        props.model_routes = &routes;
        props.effort_options = &efforts;
        props.effort_label = "Alto";
        props.selected_effort = "high";
        props.project_options = &projects;
        edit(&mut props);
        build_composer(&props)
    };
    let suggestions = |query: &str, width: f32| {
        let mut state = ComposerSuggestionsState::default();
        state.sync(
            Path::new("/workspace"),
            composer_suggestions::trigger_at_cursor(query, query.len()),
        );
        state.populate_commands(&Default::default());
        composer_suggestions::popup(
            Element::text_input("composer_input", query, "").size(Some(width - 16.0), Some(40.0)),
            &state,
            width - 16.0,
        )
    };
    let scenes: Vec<(&str, Box<dyn Fn(f32) -> Element + '_>)> = vec![
        ("model-first-view", Box::new(|width| composer(&|props| props.model_menu_open = true, width))),
        (
            "model-catalog",
            Box::new(|width| {
                composer(
                    &|props| {
                        props.model_menu_open = true;
                        props.model_list_open = true;
                    },
                    width,
                )
            }),
        ),
        (
            "model-catalog-collapsed",
            Box::new(|width| {
                composer(
                    &|props| {
                        props.model_menu_open = true;
                        props.model_list_open = true;
                        props.collapsed_model_providers = Some(collapsed);
                    },
                    width,
                )
            }),
        ),
        ("project-selector", Box::new(|width| composer(&|props| props.project_menu_open = true, width))),
        ("suggestions-commands", Box::new(|width| suggestions("/", width))),
        (
            "usage-popover",
            Box::new(|width| {
                let mut state = crate::view::tests::empty_state();
                state.provider_usage_open = true;
                crate::provider_usage_ui::sidebar_card(&state, width.min(264.0))
            }),
        ),
    ];
    for (name, scene) in scenes {
        for width in [360.0_f32, 900.0] {
            let root = Element::column(vec![Element::spacer().flex(1.0), scene(width)])
                .background(theme::BACKGROUND)
                .size(Some(width), Some(HEIGHT));
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
            let frame = tree.layout(Size::new(width, HEIGHT));
            let path = output.join(format!("{name}-{width}.svg"));
            visual_qa::write_native_frame_svg(&frame, &path, width, HEIGHT).unwrap();
            let parsed =
                resvg::usvg::Tree::from_str(&fs::read_to_string(&path).unwrap(), &options).unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, HEIGHT as u32).unwrap();
            resvg::render(&parsed, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
            pixmap.save_png(path.with_extension("png")).unwrap();
            drop(context);
        }
    }
}
