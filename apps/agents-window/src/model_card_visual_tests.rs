//! Review export of the model list and its info card.

use super::*;
use openide_ui::{DrawCommand, Point, Size, UiEvent, UiTree};
use std::time::{Duration, Instant};

fn route(provider: (&str, &str), id: &str, name: &str, efforts: &[(&str, &str)]) -> ModelRouteOption {
    ModelRouteOption {
        route_id: format!("native:{}:{id}", provider.0),
        backend: "native".into(),
        provider_id: provider.0.into(),
        provider_name: provider.1.into(),
        model_id: id.into(),
        model_name: name.into(),
        credential: CredentialAvailability::EnvPresentUnverified,
        execution: ExecutionSupport::ProviderListedUnverified,
        source: RouteSource::ProviderDiscovery,
        catalog_present: true,
        provider_listed: Some(true),
        effort_options: efforts.iter().map(|(id, label)| ((*id).into(), (*label).into())).collect(),
    }
}

/// What each provider's own listing reported, as the catalog cache keeps it.
/// Fixture ids and figures only; nothing here belongs to an account.
fn listings() -> crate::model_catalog::CatalogSnapshot {
    let status = serde_json::json!({"source":"official_api","attempted_at_ms":1,"verified_at_ms":1});
    serde_json::from_value(serde_json::json!({
        "version": 3,
        "fetched_at_ms": 1,
        "provider_status": {"openrouter": status, "anthropic": status, "codex": status},
        "providers": {
            "openrouter": {"id":"openrouter","name":"OpenRouter","models":{"example/atlas-pro":{
                "id":"example/atlas-pro","name":"Atlas Pro",
                "description":"Example general model for long agentic coding sessions with tools.",
                "tool_call":true,"tool_call_declared":true,"provider_limits_declared":true,
                "limit":{"context":400000,"output":64000},
                "modalities":{"input":["text","image","pdf"],"output":["text"]},
                "cost":{"input":3.0,"output":15.0}
            }}},
            "anthropic": {"id":"anthropic","name":"Anthropic","models":{"claude-opus-5-5":{
                "id":"claude-opus-5-5","name":"Claude Opus 5.5","provider_limits_declared":true,
                "limit":{"input":1000000,"output":128000},
                "capabilities_declared":true,
                "declared_capabilities":{"image_input":true,"pdf_input":true,"thinking":true,"structured_outputs":true}
            }}},
            "codex": {"id":"codex","name":"Codex","models":{"gpt-6-astra":{
                "id":"gpt-6-astra","name":"GPT-6-Astra","provider_limits_declared":true,
                "description":"Frontier intelligence for the most demanding work.",
                "limit":{"context":272000},
                "modalities":{"input":["text","image"],"output":[]}
            }}}
        }
    }))
    .unwrap()
}

fn text_of(frame: &openide_ui::Frame) -> String {
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn hovered_card(tree: &mut UiTree, route: &ModelRouteOption, viewport: Size) -> String {
    let start = Instant::now();
    let frame = tree.layout_at(viewport, start);
    let id = format!("composer_route:{}", route.route_id);
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .expect("model row")
        .rect;
    tree.handle_event_at(
        UiEvent::PointerMove(Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)),
        start,
    );
    tree.layout_at(viewport, start + Duration::from_millis(1));
    text_of(&tree.layout_at(viewport, start + Duration::from_millis(900)))
}

#[test]
fn card_shows_only_known_facts_with_their_source_and_the_row_shows_the_window() {
    let efforts = [("default", "Automático"), ("low", "Bajo"), ("high", "Alto")];
    let routes = vec![
        route(("openrouter", "OpenRouter"), "example/atlas-pro", "Atlas Pro", &[]),
        route(("anthropic", "Anthropic"), "claude-opus-5-5", "Claude Opus 5.5", &efforts),
        route(("codex", "Codex"), "nova-preview", "Nova Preview", &[]),
        route(("codex", "Codex"), "gpt-6-astra", "GPT-6-Astra", &efforts),
    ];
    let listings = listings();
    let metadata = crate::model_metadata::for_routes(&routes, Some(&listings), &[], &[]);
    let viewport = Size::new(900.0, 620.0);
    let mut state = super::tests::props("", viewport.width);
    state.model_routes = &routes;
    state.model_metadata = Some(&metadata);
    state.model_menu_open = true;
    state.model_list_open = true;
    let card = |index: usize| {
        let mut tree = UiTree::new(build_composer(&state));
        tree.set_reduced_motion(true);
        let frame = hovered_card(&mut tree, &routes[index], viewport);
        // The card is painted after the menu, whose last line is its footer.
        frame.split_once("Conectar proveedor\n").expect("card").1.to_owned()
    };
    let flat = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let list = text_of(&UiTree::new(build_composer(&state)).layout(viewport));
    for shown in ["400K", "1M", "272K"] {
        assert!(list.lines().any(|line| line == shown), "{shown}: {list}");
    }
    assert!(!list.contains("Sin verificar"));

    // A capability glyph names itself under the pointer and still belongs to
    // its row: a click on it chooses the model.
    let mut tree = UiTree::new(build_composer(&state));
    tree.set_reduced_motion(true);
    let start = Instant::now();
    let frame = tree.layout_at(viewport, start);
    let row = format!("composer_route:{}", routes[0].route_id);
    let glyph = frame
        .regions
        .iter()
        .find(|region| region.id == format!("model_capability:{row}:Herramientas"))
        .expect("capability glyph")
        .rect;
    let point = Point::new(glyph.x + glyph.width / 2.0, glyph.y + glyph.height / 2.0);
    tree.handle_event_at(UiEvent::PointerMove(point), start);
    let hint = text_of(&tree.layout_at(viewport, start + Duration::from_secs(2)));
    let hint = hint.split_once("Conectar proveedor\n").expect("hint").1;
    assert_eq!(hint.trim(), "Herramientas", "{hint}");
    let mut actions = tree.handle_event(UiEvent::PointerDown(point));
    actions.extend(tree.handle_event(UiEvent::PointerUp(point)));
    assert!(
        actions.iter().any(|action| matches!(action, openide_ui::UiAction::Clicked { id } if id == &row)),
        "{actions:?}"
    );

    // Everything from the provider: one source, no catalog mention.
    let api = card(0);
    // Capabilities are a row of icons; their names are not spelled in the card.
    for hidden in ["Herramientas", "Visión", "PDF"] {
        assert!(!api.lines().any(|line| line == hidden), "{hidden}: {api}");
    }
    for shown in ["Admite", "400K", "64K", "Entrada $3 · Salida $15"] {
        assert!(api.contains(shown), "{shown}: {api}");
    }
    assert!(api.lines().any(|line| line == "Fuente: API del proveedor"), "{api}");
    assert!(!api.contains("models.dev") && !api.contains("Conocimiento"));

    // Both: the provider's fields are named apart from the catalog's.
    let mixed = card(1);
    let source = flat(&mixed);
    assert!(source.contains("Fuente: catálogo models.dev ("), "{mixed}");
    assert!(source.contains("API del proveedor: visión, razonamiento, salida estructurada"), "{mixed}");
    for shown in ["128K", "Conocimiento", "jun 2026", "Bajo", "Alto", "Precio de referencia"] {
        assert!(mixed.contains(shown), "{shown}: {mixed}");
    }

    // A plan model: no per-token price, and nothing invented for an unknown id.
    let listed = card(3);
    assert!(listed.contains("272K") && listed.contains("Incluido en tu plan"), "{listed}");
    assert!(!listed.contains("Precio de referencia") && !listed.contains("1.05M"), "{listed}");
    let unknown = flat(&card(2));
    assert!(unknown.contains("Ni el proveedor ni el catálogo describen este modelo."), "{unknown}");
    for absent in ["Contexto", "Admite", "Salida máx.", "Dato no disponible", "$"] {
        for text in [&api, &mixed, &listed, &unknown] {
            assert!(!text.contains("Dato no disponible"));
        }
        assert!(!unknown.contains(absent), "{absent}: {unknown}");
    }
}

/// The list at both widths, then the card of each kind of model: described by
/// its provider, by the bundled catalog, by both, and a plan model nobody
/// describes. PNGs go to `docs/ui-separation/shots/model-metadata`.
#[test]
#[ignore = "export the model list and info card for manual visual review"]
fn export_model_card_visual_review() {
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/ui-separation/shots/model-metadata");
    std::fs::create_dir_all(&output).unwrap();
    let efforts = [
        ("default", "Automático"),
        ("low", "Bajo"),
        ("medium", "Medio"),
        ("high", "Alto"),
        ("xhigh", "Muy alto"),
        ("max", "Máximo"),
        ("ultra", "Ultra"),
    ];
    let codex = ("codex", "Codex");
    let routes = vec![
        route(("openrouter", "OpenRouter"), "example/atlas-pro", "Atlas Pro", &[]),
        route(("opencode", "OpenCode Zen"), "gpt-5.4", "GPT-5.4", &efforts[..5]),
        route(("anthropic", "Anthropic"), "claude-opus-5-5", "Claude Opus 5.5", &efforts[..6]),
        route(codex, "nova-preview", "Nova Preview", &[]),
        route(codex, "gpt-6-astra", "GPT-6-Astra", &efforts),
        route(codex, "gpt-6.1-sol", "GPT-6.1-Sol", &efforts),
        route(codex, "gpt-6-luna", "GPT-6-Luna", &efforts),
        route(codex, "gpt-5.6-terra", "GPT-5.6-Terra", &efforts),
    ];
    let listings = listings();
    let metadata = crate::model_metadata::for_routes(&routes, Some(&listings), &[], &[]);
    let cases = [
        ("card-provider-api", 0, "Fuente: API del proveedor"),
        ("card-catalog", 1, "Fuente: catálogo models.dev"),
        ("card-mixed", 2, "Fuente: "),
        ("card-subscription-unknown", 3, "Incluido en tu plan"),
        ("card-subscription-listed", 4, "Incluido en tu plan"),
    ];
    for width in [360.0_f32, 900.0] {
        let viewport = Size::new(width, 620.0);
        let mut state = super::tests::props("", width);
        state.model_routes = &routes;
        state.native_provider_id = "codex";
        state.selected_model = "gpt-6-luna";
        state.model_metadata = Some(&metadata);
        state.model_menu_open = true;
        state.model_list_open = true;
        let context = eframe::egui::Context::default();
        crate::fonts::install(&context);
        context.begin_pass(eframe::egui::RawInput::default());
        let export = |frame: &openide_ui::Frame, name: &str| {
            let path = output.join(format!("{name}-{width}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(frame, &path, width, viewport.height).unwrap();
            let svg = resvg::usvg::Tree::from_data(
                &std::fs::read(&path).unwrap(),
                &crate::settings_ui::visual_qa::native_svg_options(),
            )
            .unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, viewport.height as u32).unwrap();
            resvg::render(&svg, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
            pixmap.save_png(path.with_extension("png")).unwrap();
            std::fs::remove_file(path).unwrap();
        };
        let native_tree = || {
            let fonts = context.clone();
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_text_measurer(move |text, size, family| {
                fonts.fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(
                            text.into(),
                            eframe::egui::FontId::new(size, match family {
                                FontFamily::Sans | FontFamily::SansItalic | FontFamily::SansMedium | FontFamily::SansBold => eframe::egui::FontFamily::Proportional,
                                FontFamily::Monospace => eframe::egui::FontFamily::Monospace,
                            }),
                            eframe::egui::Color32::WHITE,
                        )
                        .size()
                        .x
                })
            });
            tree.set_reduced_motion(true);
            tree
        };
        let start = Instant::now();
        let list = native_tree().layout_at(viewport, start);
        let listed = text_of(&list);
        assert!(listed.contains("400K") && !listed.contains("Sin verificar"), "{listed}");
        export(&list, "list");
        for (name, index, expected) in cases {
            let mut tree = native_tree();
            let mut frame = tree.layout_at(viewport, start);
            let id = format!("composer_route:{}", routes[index].route_id);
            let find = |frame: &openide_ui::Frame, id: &str| {
                frame.regions.iter().find(|region| region.id == id).map(|region| region.rect)
            };
            if find(&frame, &id).is_none() {
                // A short window lists the last providers below the fold.
                let list = find(&frame, "composer_model_dismiss:options").expect("model results");
                tree.handle_event(UiEvent::Scroll {
                    position: Point::new(list.x + 12.0, list.y + list.height / 2.0),
                    delta_y: -10_000.0,
                });
                frame = tree.layout_at(viewport, start);
            }
            let rect = find(&frame, &id).expect("model row");
            tree.handle_event_at(
                UiEvent::PointerMove(Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)),
                start,
            );
            tree.layout_at(viewport, start + Duration::from_millis(1));
            let card = tree.layout_at(viewport, start + Duration::from_millis(900));
            let text = text_of(&card);
            assert!(text.contains(expected), "{name}: {text}");
            assert!(!text.contains("Dato no disponible"), "{name}: {text}");
            export(&card, name);
        }
        context.end_pass().textures_delta.clear();
    }
}
