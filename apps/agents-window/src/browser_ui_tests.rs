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
        // A server of this machine that is not there: the question a
        // stopped dev server needs, with the retry below.
        assert_eq!(
            error_copy(&state.browser).0,
            "No se pudo conectar con localhost:3000 — ¿sigue corriendo el servidor?"
        );
        assert!(error_copy(&state.browser)
            .1
            .contains("Iniciá el servidor del proyecto"));
        for (address, local) in [
            ("http://0.0.0.0:5173/", true),
            ("http://127.0.0.1:5173/", true),
            ("http://[::1]:5173/", true),
            ("http://127.example.test:5173/", false),
            ("https://example.test/", false),
        ] {
            let mut other = state.browser.clone();
            other.url_draft = address.into();
            assert_eq!(
                error_copy(&other).0.contains("¿sigue corriendo el servidor?"),
                local,
                "{address}"
            );
        }
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
fn initial_blank_loading_and_failed_start_states_have_no_clickable_page() {
    let mut state = state();
    state.browser.url = "about:blank".into();
    assert_eq!(surface(&state.browser), Surface::Empty);
    for mode in [Surface::Empty, Surface::Loading, Surface::Error] {
        state.browser.navigating = mode == Surface::Loading;
        // A context whose page could not be opened never started.
        state.browser.started = mode != Surface::Error;
        state.browser.error = (mode == Surface::Error).then(|| "CEF no creó la página a tiempo.".into());
        let mut tree = UiTree::new(build(&state, 320.0));
        let frame = tree.layout(Size::new(320.0, 480.0));
        assert_eq!(surface(&state.browser), mode);
        assert!(!has_image(&frame));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "browser_viewport"));
        if mode == Surface::Error {
            // Another context may start: the panel offers to open one.
            assert_eq!(error_copy(&state.browser).0, "El navegador no pudo iniciarse");
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
        }
    }
}

#[test]
fn an_embedded_browser_that_cannot_start_says_so_with_its_reason_and_offers_no_retry() {
    const REASON: &str = "Falta icudtl.dat junto a libcef.so. El navegador embebido se inicia una sola vez por ejecución: cerrá OpenIDE y volvé a abrirlo para reintentar.";
    for width in [240.0, 320.0, 640.0] {
        let mut state = state();
        state.browser.started = false;
        state.browser.engine_failed = true;
        state.browser.error = Some(REASON.into());
        assert_eq!(surface(&state.browser), Surface::Error);
        // The reason is the card's own text, not a detail behind a hover.
        assert_eq!(error_copy(&state.browser), ("El navegador embebido no pudo iniciarse".into(), REASON.into()));
        let frame = UiTree::new(build(&state, width)).layout(Size::new(width, 620.0));
        assert!(!has_image(&frame));
        let said = texts(&frame).concat();
        assert!(said.contains("El navegador embebido no pudo iniciarse") && said.contains("Falta icudtl.dat"), "{said}");
        // Chromium starts once per run: nothing here could start it again.
        for absent in ["browser_restart", "browser_navigate", "browser_focus_url", "browser_error_detail", "browser_viewport"] {
            assert!(!frame.regions.iter().any(|region| region.id == absent), "{absent} at {width}");
        }
        // The same failure for another reason is told the same way.
        state.browser.error = Some("Esta versión de OpenIDE sólo incluye su navegador embebido para Linux.".into());
        assert_eq!(error_copy(&state.browser).1, "Esta versión de OpenIDE sólo incluye su navegador embebido para Linux.");
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
fn the_strip_of_tabs_shows_with_a_second_tab_and_the_page_takes_what_is_left() {
    use crate::browser_runtime::BrowserTabView;
    let tab = |id, title: &str, url: &str, active| BrowserTabView { id, title: title.into(), url: url.into(), active, temporary: false };
    let size = Size::new(360.0, 500.0);
    let has = |frame: &Frame, id: &str| frame.regions.iter().any(|region| region.id == id);
    let top = |frame: &Frame| frame.regions.iter().find(|region| region.id == "browser_viewport").unwrap().rect.y;

    // One tab needs no strip: the page starts under the toolbar, as before.
    let mut state = state();
    state.browser.pages = vec![tab(1, "Inicio", "http://localhost:3000/", true)];
    let alone = UiTree::new(build(&state, size.width)).layout(size);
    assert!(!has(&alone, "browser_tab_select:1") && !has(&alone, "browser_tab_new"));
    assert_eq!(top(&alone), TOOLBAR_HEIGHT + 1.0);

    state.browser.pages.push(tab(2, "", "about:blank", false));
    state.browser.pages[0].active = false;
    state.browser.pages[1].active = true;
    let frame = UiTree::new(build(&state, size.width)).layout(size);
    for id in ["browser_tab_select:1", "browser_tab_close:1", "browser_tab_select:2", "browser_tab_close:2", "browser_tab_new"] {
        assert!(has(&frame, id), "{id}");
    }
    let said = texts(&frame);
    assert!(said.contains(&"Inicio") && said.contains(&"Nueva pestaña"), "{said:?}");
    assert_eq!(top(&frame), TOOLBAR_HEIGHT + 1.0 + TABS_HEIGHT);
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
fn the_address_has_a_copy_button_that_shows_a_check_once_it_copied() {
    use openide_ui::{DrawCommand, Icon};
    let icons = |state: &ViewState| -> Vec<Icon> {
        let frame = UiTree::new(build(state, 320.0)).layout(Size::new(320.0, 480.0));
        let copy = frame
            .regions
            .iter()
            .find(|region| region.id == COPY_URL && region.kind == HitKind::Button)
            .map(|region| region.rect);
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Icon { icon, rect, .. }
                    if copy.is_some_and(|copy| copy.intersection(*rect).is_some()) =>
                {
                    Some(*icon)
                }
                _ => None,
            })
            .collect()
    };
    let mut state = state();
    // With no address there is nothing to copy, and no button.
    state.browser.url.clear();
    state.browser.url_draft.clear();
    assert!(icons(&state).is_empty());
    state.browser.url_draft = "http://localhost:3000/".into();
    assert_eq!(icons(&state), [Icon::Copy]);
    state.link_copied = Some(crate::view::LinkCopied {
        url: "http://localhost:3000/".into(),
        by: COPY_URL,
        at: std::time::Instant::now(),
    });
    assert_eq!(icons(&state), [Icon::Check]);
    // A link copied elsewhere leaves this button as it was.
    state.link_copied.as_mut().unwrap().by = "";
    assert_eq!(icons(&state), [Icon::Copy]);
}

#[test]
fn more_menu_exposes_real_secondary_actions_and_clamps_to_small_viewport() {
    let mut state = state();
    state.browser.more_open = true;
    let mut tree = UiTree::new(build(&state, 300.0));
    tree.set_reduced_motion(true);
    let frame = tree.layout(Size::new(320.0, 480.0));
    for id in [
        // Too narrow for the button beside the address: the menu copies.
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
            navigation(&state.browser, false, 600.0),
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
