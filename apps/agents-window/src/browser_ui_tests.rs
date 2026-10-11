use super::*;
use openide_ui::{DrawCommand, Frame, HitKind, Point, Size, UiAction, UiEvent, UiTree};

fn state() -> ViewState {
    let mut state = crate::view::tests::empty_state();
    state.browser.started = true;
    state.browser.url = "http://localhost:3000/".into();
    state.browser.url_draft = state.browser.url.clone();
    state.browser.title = "localhost".into();
    state.browser.can_go_back = true;
    state.browser.can_go_forward = true;
    state.browser.tab_id = Some(7);
    state.browser.live = true;
    state
}

fn has_image(frame: &Frame) -> bool {
    frame
        .commands
        .iter()
        .any(|command| matches!(command, DrawCommand::Surface { id, .. } if *id == surface_id(7)))
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
            Some("net::ERR_CONNECTION_REFUSED at http://localhost:3000/".into());
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
fn initial_blank_loading_and_missing_browser_states_have_no_clickable_page() {
    let mut state = state();
    state.browser.url = "about:blank".into();
    assert_eq!(surface(&state.browser), Surface::Empty);
    for mode in [Surface::Empty, Surface::Loading, Surface::Error] {
        state.browser.navigating = mode == Surface::Loading;
        // A context that found no browser never started.
        state.browser.browser_missing = mode == Surface::Error;
        state.browser.started = mode != Surface::Error;
        state.browser.error = (mode == Surface::Error)
            .then(|| "No se encontró Google Chrome, Chromium ni Microsoft Edge en este equipo.".into());
        let mut tree = UiTree::new(build(&state, 320.0));
        let frame = tree.layout(Size::new(320.0, 480.0));
        assert_eq!(surface(&state.browser), mode);
        assert!(!has_image(&frame));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "browser_viewport"));
        if mode == Surface::Error {
            // The panel says what is missing and what to do about it, and
            // offers to look again; there is nothing to install from here.
            let (title, description) = error_copy(&state.browser);
            assert_eq!(title, "No se encontró un navegador");
            for part in ["Google Chrome", "Chromium", "Microsoft Edge", "OPENIDE_BROWSER_EXECUTABLE"] {
                assert!(description.contains(part), "{description}");
            }
            assert!(texts(&frame).contains(&"Reintentar"));
            let retry = frame
                .regions
                .iter()
                .find(|region| region.id == "browser_restart" && region.kind == HitKind::Button)
                .expect("a retry that opens the context again")
                .rect;
            let point = Point::new(retry.x + retry.width / 2.0, retry.y + retry.height / 2.0);
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked { id: "browser_restart".into() }]
            );
            assert!(!frame.regions.iter().any(|region| region.id == "browser_install"));
        }
    }
}

#[test]
fn a_page_that_did_not_load_is_an_error_whatever_its_reason_reads_like() {
    let mut state = state();
    state.browser.error = Some("Timeout 10000ms exceeded.".into());
    // An input that failed keeps the page; a navigation that did not finish
    // replaces it, as a refused connection does.
    assert_eq!(surface(&state.browser), Surface::Page);
    state.browser.load_failed = true;
    assert_eq!(surface(&state.browser), Surface::Error);
    assert_eq!(error_copy(&state.browser).0, "No se pudo abrir esta página");
    let frame = UiTree::new(build(&state, 320.0)).layout(Size::new(320.0, 480.0));
    assert!(frame.regions.iter().any(|region| region.id == "browser_navigate"));
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
            DrawCommand::Surface { rect, .. } => Some(*rect),
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
