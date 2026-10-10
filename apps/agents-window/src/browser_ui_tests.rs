use super::*;
use openide_ui::{
    DrawCommand, Frame, HitKind, Point, RasterImageFormat, Size, UiAction, UiEvent, UiTree,
};
use std::sync::Arc;

fn state() -> ViewState {
    let mut state = crate::view::tests::empty_state();
    state.browser.started = true;
    state.browser.url = "http://localhost:3000/".into();
    state.browser.url_draft = state.browser.url.clone();
    state.browser.title = "localhost".into();
    state.browser.can_go_back = true;
    state.browser.can_go_forward = true;
    state.browser.image = Some(
        RasterImage::new(
            "a".repeat(64),
            RasterImageFormat::Png,
            3,
            Arc::from("AAAA"),
            "Fixture page",
        )
        .unwrap(),
    );
    state
}

fn has_image(frame: &Frame) -> bool {
    frame
        .commands
        .iter()
        .any(|command| matches!(command, DrawCommand::RasterImage { .. }))
}

fn texts(frame: &Frame) -> Vec<&str> {
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn one_toolbar_keeps_navigation_address_and_more_disjoint_at_narrow_widths() {
    for width in [200.0, 240.0, 320.0, 480.0, 900.0] {
        let state = state();
        let mut tree = UiTree::new(build(&state, width));
        let frame = tree.layout(Size::new(width, 620.0));
        let mut previous_end = 0.0;
        for id in [
            "browser_back",
            "browser_forward",
            "browser_reload",
            "browser_url",
            "browser_more_toggle",
        ] {
            let rect = frame
                .regions
                .iter()
                .find(|region| region.id == id && region.kind != HitKind::Hover)
                .unwrap_or_else(|| panic!("missing {id} at {width}"))
                .rect;
            assert!(rect.x >= previous_end, "overlap: {id} at {width}");
            assert!(rect.y >= 0.0 && rect.y + rect.height <= TOOLBAR_HEIGHT);
            assert!(rect.x + rect.width <= width, "{id} outside {width}");
            assert!(
                rect.width
                    >= if id == "browser_url" {
                        70.0
                    } else {
                        NAVIGATION_SIZE
                    }
            );
            previous_end = rect.x + rect.width;
            if id != "browser_url" {
                let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked { id: id.into() }]
                );
            }
        }
        assert!(!frame.regions.iter().any(|region| matches!(
            region.id.as_str(),
            "browser_snapshot" | "browser_mcp_config"
        )));
        assert!(
            !texts(&frame).contains(&"localhost"),
            "page title is not duplicated beneath the URL"
        );
    }
}

#[test]
fn connection_refused_uses_one_native_error_and_never_exposes_the_chrome_error_raster() {
    for width in [240.0, 320.0, 720.0] {
        let mut state = state();
        state.browser.url = "chrome-error://chromewebdata/".into();
        state.browser.error =
            Some("page.goto: net::ERR_CONNECTION_REFUSED at http://localhost:3000/".into());
        let frame = UiTree::new(build(&state, width)).layout(Size::new(width, 700.0));
        assert!(!has_image(&frame));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "browser_viewport"));
        assert_eq!(surface(&state.browser), Surface::Error);
        assert_eq!(
            error_copy(&state.browser).0,
            "No se pudo conectar con localhost:3000"
        );
        assert!(error_copy(&state.browser)
            .1
            .contains("servidor del proyecto esté iniciado"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "browser_navigate"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "browser_focus_url"));
        assert_eq!(
            frame
                .regions
                .iter()
                .filter(|region| region.id == "browser_error_detail")
                .count(),
            1
        );
    }
}

#[test]
fn initial_blank_loading_and_dependency_states_have_no_clickable_page() {
    let mut state = state();
    state.browser.url = "about:blank".into();
    assert_eq!(surface(&state.browser), Surface::Empty);
    for mode in [Surface::Empty, Surface::Loading, Surface::Error] {
        state.browser.navigating = mode == Surface::Loading;
        state.browser.dependency_missing = mode == Surface::Error;
        state.browser.error =
            (mode == Surface::Error).then(|| "Playwright no está instalado".into());
        let mut tree = UiTree::new(build(&state, 320.0));
        let frame = tree.layout(Size::new(320.0, 480.0));
        assert_eq!(surface(&state.browser), mode);
        assert!(!has_image(&frame));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "browser_viewport"));
        if mode == Surface::Error {
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "browser_install"));
        }
    }
}

#[test]
fn non_navigation_errors_keep_the_page_and_pending_input_does_not_resize_it() {
    let mut state = state();
    let size = Size::new(360.0, 500.0);
    let mut tree = UiTree::new(build(&state, size.width));
    let before = tree.layout(size);
    let viewport = before
        .regions
        .iter()
        .find(|region| region.id == "browser_viewport")
        .unwrap()
        .rect;
    state.browser.error = Some("No hay texto seleccionado".into());
    state.browser.queued_inputs = 32;
    state.browser.loading = true;
    tree.set_root(build(&state, size.width));
    let after = tree.layout(size);
    assert!(has_image(&after));
    assert_eq!(surface(&state.browser), Surface::Page);
    assert_eq!(
        after
            .regions
            .iter()
            .find(|region| region.id == "browser_viewport")
            .unwrap()
            .rect,
        viewport
    );
    assert!(after
        .regions
        .iter()
        .any(|region| region.id == "browser_reload" && region.kind == HitKind::Button));
}

#[test]
fn viewport_uses_remaining_bounds_and_host_page_scroll_cannot_scroll_away_toolbar() {
    let state = state();
    let size = Size::new(360.0, 500.0);
    let mut tree = UiTree::new(build(&state, size.width));
    let frame = tree.layout(size);
    let viewport = frame
        .regions
        .iter()
        .find(|region| region.id == "browser_viewport")
        .unwrap()
        .rect;
    assert_eq!(viewport.y, TOOLBAR_HEIGHT + 1.0);
    assert_eq!(viewport.width, size.width);
    assert_eq!(
        viewport.height,
        size.height - TOOLBAR_HEIGHT - STATUS_HEIGHT - 1.0
    );
    let painted = frame
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::RasterImage { rect, .. } => Some(*rect),
            _ => None,
        })
        .unwrap();
    assert_eq!(painted, viewport);
    assert!(!frame
        .regions
        .iter()
        .any(|region| matches!(region.kind, HitKind::Scroll | HitKind::Scrollbar)));
    tree.handle_event(UiEvent::Scroll {
        position: Point::new(180.0, 180.0),
        delta_y: -600.0,
    });
    let after = tree.layout(size);
    assert_eq!(
        after
            .regions
            .iter()
            .find(|region| region.id == "browser_url")
            .unwrap()
            .rect
            .y,
        NAVIGATION_INSET
    );
    assert_eq!(
        after
            .regions
            .iter()
            .find(|region| region.id == "browser_viewport")
            .unwrap()
            .rect,
        viewport
    );
}

#[test]
fn more_menu_exposes_real_secondary_actions_and_clamps_to_small_viewport() {
    let mut state = state();
    state.browser.more_open = true;
    let mut tree = UiTree::new(build(&state, 320.0));
    tree.set_reduced_motion(true);
    let frame = tree.layout(Size::new(320.0, 480.0));
    for id in [
        "browser_copy_url",
        "browser_snapshot",
        "browser_mcp_config",
        "browser_restart",
    ] {
        let rect = frame
            .regions
            .iter()
            .find(|region| region.id == id && region.kind == HitKind::Button)
            .unwrap_or_else(|| panic!("missing {id}"))
            .rect;
        assert!(
            rect.x >= 0.0 && rect.x + rect.width <= 320.0,
            "{id}: {rect:?}"
        );
        assert!(rect.y >= TOOLBAR_HEIGHT - 8.0 && rect.y + rect.height <= 480.0);
    }
}

#[test]
fn browser_navigation_tooltips_open_below_the_toolbar() {
    let state = state();
    for (id, title) in [
        ("browser_reload", "Recargar página"),
        ("browser_more_toggle", "Más opciones del navegador"),
    ] {
        let mut tree = UiTree::new(Element::column(vec![
            Element::spacer().size(None, Some(120.0)),
            navigation(&state.browser, 600.0),
            Element::spacer().flex(1.0),
        ]));
        tree.set_reduced_motion(true);
        let size = Size::new(600.0, 400.0);
        let frame = tree.layout(size);
        let anchor = frame
            .regions
            .iter()
            .find(|region| region.id == id)
            .unwrap()
            .rect;
        assert!(tree.focus(id));
        let focused = tree.layout(size);
        let position = focused
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text == title => Some(position),
                _ => None,
            })
            .expect("focused navigation tooltip");
        assert!(
            position.y >= anchor.y + anchor.height,
            "{id} should open below its header, not above it"
        );
    }
}
