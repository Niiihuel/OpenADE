//! The review export of the composer's menus, selectors, suggestion lists
//! and popover panels.

use super::*;
use openide_ui::Size;

fn review_routes() -> Vec<ModelRouteOption> {
    [
        (
            "openai",
            "OpenAI",
            "gpt-6-sol",
            "GPT-6 Sol",
            ExecutionSupport::Verified,
        ),
        (
            "openai",
            "OpenAI",
            "gpt-6-luna",
            "GPT-6 Luna",
            ExecutionSupport::ProviderAdvertisedUnverified,
        ),
        (
            "anthropic",
            "Anthropic",
            "claude-sonnet-4-5",
            "Claude Sonnet 4.5",
            ExecutionSupport::Verified,
        ),
        (
            "anthropic",
            "Anthropic",
            "claude-haiku-4-5",
            "Claude Haiku 4.5",
            ExecutionSupport::ProviderListedUnverified,
        ),
        (
            "anthropic",
            "Anthropic",
            "claude-opus-4-1",
            "Claude Opus 4.1",
            ExecutionSupport::Verified,
        ),
    ]
    .into_iter()
    .map(
        |(provider_id, provider_name, id, name, execution)| ModelRouteOption {
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
        },
    )
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
    let output =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/menus-popovers-visual-qa");
    fs::create_dir_all(&output).unwrap();
    let options = visual_qa::native_svg_options();
    let routes = review_routes();
    let efforts = vec![
        ("default".to_owned(), "Automático".to_owned()),
        ("low".to_owned(), "Bajo".to_owned()),
        ("high".to_owned(), "Alto".to_owned()),
    ];
    let projects = vec![
        (
            "/home/usuario/proyectos/openADE".to_owned(),
            "openADE".to_owned(),
        ),
        (
            "/home/usuario/proyectos/personal/limn".to_owned(),
            "limn".to_owned(),
        ),
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
        (
            "model-first-view",
            Box::new(|width| composer(&|props| props.model_menu_open = true, width)),
        ),
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
        (
            "project-selector",
            Box::new(|width| composer(&|props| props.project_menu_open = true, width)),
        ),
        (
            "suggestions-commands",
            Box::new(|width| suggestions("/", width)),
        ),
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
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            drop(context);
        }
    }
}
