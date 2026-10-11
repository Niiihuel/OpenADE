//! Opt-in composer layout review using synthetic drafts and the host's fonts.
//! No provider, persisted conversation, or account data is accessed.

use super::tests::{configured_effort_route, props};
use super::{
    build_composer, ComposerProps, ExecutionSupport, CARD_RADIUS, CONTROL_HEIGHT, PILL_SURFACE,
};
use eframe::egui;
use openide_ui::{
    DrawCommand, Element, FontFamily, Frame, Key, Point, Rect, Size, UiAction, UiEvent, UiTree,
};
use std::path::Path;

const VIEWPORT_HEIGHT: f32 = 480.0;
const MULTILINE: &str = "Revisá el espacio del editor.\nEl texto usa todo el ancho.\nLos controles quedan debajo.\nEl proyecto conserva su contexto.";

fn fixture<'a>(
    prompt: &'a str,
    width: f32,
    routes: &'a [crate::composer_selection::ModelRouteOption],
) -> ComposerProps<'a> {
    let mut state = props(prompt, width);
    state.home = false;
    state.workspace = "/projects/openADE";
    state.project_branch = Some("codex/ui-component-quality");
    state.native_provider_id = "codex";
    state.selected_model = "gpt-5.6-luna";
    state.model_label = "GPT-5.6 Luna";
    state.model_routes = routes;
    state.effort_options = &routes[0].effort_options;
    state.selected_effort = "max";
    state.effort_label = "Máximo";
    state.full_access = true;
    state.permission_controls = Some((
        openide_harness::ToolPermissions::default(),
        openide_harness::CommandIsolation::HostUnrestricted,
        false,
    ));
    state
}

fn dock(state: &ComposerProps<'_>) -> Element {
    Element::column(vec![Element::spacer().flex(1.0), build_composer(state)])
        .background(crate::theme::BACKGROUND)
        .size(Some(state.width), None)
}

fn native_tree(root: Element) -> (egui::Context, UiTree) {
    let context = egui::Context::default();
    crate::fonts::install(&context);
    context.begin_pass(egui::RawInput::default());
    let measure = context.clone();
    let mut tree = UiTree::new(root);
    tree.set_reduced_motion(true);
    tree.set_text_measurer(move |text, size, family| {
        measure.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    text.to_owned(),
                    egui::FontId::new(
                        size,
                        match family {
                            FontFamily::Sans | FontFamily::SansItalic => egui::FontFamily::Proportional,
                            FontFamily::Monospace => egui::FontFamily::Monospace,
                        },
                    ),
                    egui::Color32::WHITE,
                )
                .size()
                .x
        })
    });
    (context, tree)
}

fn rect(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("missing composer control {id}"))
        .rect
}

fn assert_editor_and_toolbar(frame: &Frame, viewport: Size) {
    let card = frame
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::FillRect {
                rect,
                color,
                radius,
            } if *color == PILL_SURFACE && *radius == CARD_RADIUS => Some(*rect),
            _ => None,
        })
        .expect("composer card");
    let editor = rect(frame, "prompt");
    let plus = rect(frame, "composer_add_context");
    let send = rect(frame, "run_toggle");
    assert!(
        editor.width >= card.width - 18.0,
        "editor uses the card width"
    );
    assert!(
        editor.y + editor.height <= plus.y,
        "toolbar follows the editor"
    );
    assert_eq!(plus.y, send.y);
    assert_eq!(plus.width, CONTROL_HEIGHT);
    assert_eq!(send.width, CONTROL_HEIGHT);
    let project = rect(frame, "composer_project_toggle");
    assert!(project.y + project.height <= card.y);
    let mut controls = vec![
        "prompt",
        "composer_add_context",
        "composer:permissions",
        "composer_model_toggle",
        "run_toggle",
    ];
    if viewport.width >= 380.0 {
        controls.push("composer_microphone");
    }
    for id in controls {
        let control = rect(frame, id);
        assert!(
            control.x >= card.x - 0.1
                && control.x + control.width <= card.x + card.width + 0.1
                && control.y >= card.y - 0.1
                && control.y + control.height <= card.y + card.height + 0.1,
            "{id} stays inside the composer: {control:?}, card: {card:?}"
        );
    }
    assert!(card.x >= 0.0 && card.x + card.width <= viewport.width + 0.1);
    assert!(card.y >= 0.0 && card.y + card.height <= viewport.height + 0.1);
}

#[test]
fn compact_composer_toolbar_preserves_targets_and_geometry_with_native_fonts() {
    let routes = [configured_effort_route()];
    for width in [280.0, 360.0, 480.0, 640.0, 900.0] {
        for home in [false, true] {
            for prompt in ["", MULTILINE] {
                let mut state = fixture(prompt, width, &routes);
                state.home = home;
                let (context, mut tree) = native_tree(dock(&state));
                let viewport = Size::new(width, VIEWPORT_HEIGHT);
                let frame = tree.layout(viewport);
                let editor = rect(&frame, "prompt");
                let ids = [
                    "composer_add_context",
                    "composer:permissions",
                    "composer_model_toggle",
                    "composer_microphone",
                    "run_toggle",
                ];
                let controls = ids
                    .iter()
                    .filter_map(|id| {
                        frame
                            .regions
                            .iter()
                            .find(|region| {
                                region.id == *id && region.kind == openide_ui::HitKind::Button
                            })
                            .map(|region| (*id, region.rect))
                    })
                    .collect::<Vec<_>>();
                for required in [
                    "composer_add_context",
                    "composer:permissions",
                    "composer_model_toggle",
                    "run_toggle",
                ] {
                    assert!(
                        controls.iter().any(|(id, _)| *id == required),
                        "{required} missing at {width}, home={home}"
                    );
                }
                for (index, (id, target)) in controls.iter().enumerate() {
                    assert!(
                        target.width >= 24.0 && target.height >= 24.0,
                        "small target {id}: {target:?}"
                    );
                    assert!(
                        target.x >= 0.0 && target.x + target.width <= width + 0.1,
                        "overflow {id}: {target:?}"
                    );
                    assert!(target.y >= 0.0 && target.y + target.height <= viewport.height + 0.1);
                    for (other_id, other) in controls.iter().skip(index + 1) {
                        let overlaps = target.x < other.x + other.width - 0.1
                            && target.x + target.width > other.x + 0.1
                            && target.y < other.y + other.height - 0.1
                            && target.y + target.height > other.y + 0.1;
                        assert!(
                            !overlaps,
                            "{id} overlaps {other_id} at {width}: {target:?}, {other:?}"
                        );
                    }
                    tree.handle_event(UiEvent::PointerMove(Point::new(
                        target.x + target.width / 2.0,
                        target.y + target.height / 2.0,
                    )));
                    let hovered = tree.layout(viewport);
                    assert_eq!(rect(&hovered, id), *target, "hover moves {id}");
                    assert_eq!(rect(&hovered, "prompt"), editor, "hover moves editor");
                }
                context.end_pass().textures_delta.clear();
            }
        }
    }
}

fn export(frame: &Frame, directory: &Path, name: &str, viewport: Size) {
    let svg_path = directory.join(format!("{name}-{:.0}.svg", viewport.width));
    crate::settings_ui::visual_qa::write_native_frame_svg(
        frame,
        &svg_path,
        viewport.width,
        viewport.height,
    )
    .unwrap();
    let options = crate::settings_ui::visual_qa::native_svg_options();
    let svg = std::fs::read_to_string(&svg_path).unwrap();
    let parsed = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(viewport.width as u32, viewport.height as u32).unwrap();
    resvg::render(
        &parsed,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(svg_path.with_extension("png")).unwrap();
}

#[test]
#[ignore = "opt-in synthetic composer SVG/PNG exports with the native fonts"]
fn export_composer_layout_visual_review() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("OPENIDE_COMPOSER_VISUAL_DIR")
            .unwrap_or_else(|| "/tmp/openade-composer-layout-review".into()),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let mut route = configured_effort_route();
    route.execution = ExecutionSupport::Verified;
    route.effort_options = vec![
        ("default".into(), "Automático".into()),
        ("low".into(), "Bajo".into()),
        ("medium".into(), "Medio".into()),
        ("high".into(), "Alto".into()),
        ("max".into(), "Máximo".into()),
    ];
    let routes = [route];
    for home in [false, true] {
        let viewport = Size::new(780.0, 208.0);
        let mut state = fixture("", viewport.width, &routes);
        state.home = home;
        let (context, mut tree) = native_tree(dock(&state));
        for (name, id) in [
            ("permissions", "composer:permissions"),
            ("branch", "composer_branch_toggle"),
            ("project", "composer_project_toggle"),
            ("context", "composer_context_toggle"),
        ] {
            let frame = tree.layout(viewport);
            let target = rect(&frame, id);
            tree.handle_event(UiEvent::PointerMove(Point::new(
                target.x + target.width / 2.0,
                target.y + target.height / 2.0,
            )));
            export(
                &tree.layout(viewport),
                &directory,
                &format!("{}-hover-{name}", if home { "home" } else { "chat" }),
                viewport,
            );
        }
        context.end_pass().textures_delta.clear();
    }
    let paragraph = "Revisá cómo aprovecha el editor el ancho disponible cuando escribo una tarea larga, sin desplazar ni cubrir los controles de permisos, modelo y envío. ".repeat(6);
    let long = format!(
        "{}\nFIN_DEL_BORRADOR",
        (1..=40)
            .map(|line| format!(
                "Línea {line:02}: un borrador extenso permite revisar el scroll del editor."
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
    for width in [360.0, 780.0, 1120.0] {
        let viewport = Size::new(width, VIEWPORT_HEIGHT);
        for (name, prompt) in [
            ("single-line", "Hola, de qué trata mi proyecto"),
            ("wrapped-paragraph", paragraph.as_str()),
            ("four-lines", MULTILINE),
            ("capped-draft", long.as_str()),
        ] {
            let state = fixture(prompt, width, &routes);
            let (context, mut tree) = native_tree(dock(&state));
            let frame = tree.layout(viewport);
            assert_editor_and_toolbar(&frame, viewport);
            let cap = if width < 490.0 { 128.0 } else { 180.0 };
            assert!(rect(&frame, "prompt").height <= cap + 0.1);
            export(&frame, &directory, name, viewport);
            context.end_pass().textures_delta.clear();
        }

        // Exercise real input and selection events so both growing and
        // shrinking use retained editor state rather than fresh fixtures.
        let state = fixture("", width, &routes);
        let (context, mut tree) = native_tree(dock(&state));
        let baseline = tree.layout(viewport);
        let short_height = rect(&baseline, "prompt").height;
        assert!(tree.focus("prompt"));
        assert_eq!(
            tree.handle_event(UiEvent::Text(MULTILINE.into())),
            vec![UiAction::InputChanged {
                id: "prompt".into(),
                value: MULTILINE.into(),
            }]
        );
        let grown = tree.layout(viewport);
        assert!(rect(&grown, "prompt").height > short_height);
        tree.handle_event(UiEvent::Key {
            key: Key::A,
            shift: false,
            ctrl: true,
        });
        tree.handle_event(UiEvent::Text(long.clone()));
        let capped = tree.layout(viewport);
        assert_editor_and_toolbar(&capped, viewport);
        assert!(rect(&capped, "prompt").height >= rect(&grown, "prompt").height);
        assert!(capped.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text == "FIN_DEL_BORRADOR"
        )));
        export(&capped, &directory, "capped-scrolled-to-cursor", viewport);
        tree.handle_event(UiEvent::Key {
            key: Key::A,
            shift: false,
            ctrl: true,
        });
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Backspace,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::InputChanged {
                id: "prompt".into(),
                value: String::new(),
            }]
        );
        let shrunk = tree.layout(viewport);
        assert_eq!(rect(&shrunk, "prompt").height, short_height);
        assert_editor_and_toolbar(&shrunk, viewport);
        export(&shrunk, &directory, "cleared-after-long-draft", viewport);
        context.end_pass().textures_delta.clear();
    }

    let viewport = Size::new(780.0, 208.0);
    let state = fixture(MULTILINE, viewport.width, &routes);
    let (context, mut tree) = native_tree(dock(&state));
    let frame = tree.layout(viewport);
    assert_editor_and_toolbar(&frame, viewport);
    export(&frame, &directory, "composer-closeup", viewport);
    context.end_pass().textures_delta.clear();

    for width in [780.0, 900.0] {
        let viewport = Size::new(width, VIEWPORT_HEIGHT);
        let mut state = fixture(MULTILINE, width, &routes);
        state.model_menu_open = true;
        state.advanced_open = true;
        let (context, mut tree) = native_tree(dock(&state));
        let frame = tree.layout(viewport);
        assert_editor_and_toolbar(&frame, viewport);
        let slider = rect(&frame, "composer_effort_slider");
        let trigger = rect(&frame, "composer_model_toggle");
        assert!(
            slider.y + slider.height <= trigger.y,
            "effort opens above its trigger"
        );
        assert!(slider.x >= 0.0 && slider.x + slider.width <= width + 0.1);
        // The SVG inspection backend cannot paint the animated effort shader.
        // Check its real layout here without substituting a fake slider image.
        context.end_pass().textures_delta.clear();
    }
}
