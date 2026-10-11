//! The links of a conversation as the native tree paints them, with the
//! bundled fonts. Fixtures are local view projections: no engine starts and
//! nothing is opened.

use super::plan_visual_tests::{export, fixture, native_tree, root};
use crate::{
    chat_links::ChatLinkMenu,
    rich_text::web_link_url_from_action,
    settings_ui::{build_settings_page, SettingsPage},
    transcript::{ActivityStatus, TranscriptItem},
    view::ViewState,
};
use openide_ui::{Element, Frame, Point, Size, UiEvent, UiTree};
use serde_json::json;
use std::{
    path::Path,
    time::{Duration, Instant},
};

const WIDE: Size = Size::new(900.0, 620.0);
const NARROW: Size = Size::new(360.0, 760.0);
const SHOTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/ui-separation/shots/chat-links");
const ANSWER: &str = "Puedes abrirlo en http://localhost:5173.\n\n\
El cambio está en https://github.com/openai/codex/pull/1234 y la referencia en \
https://developer.mozilla.org/es/docs/Web/HTTP/Headers/Content-Security-Policy. \
Seguí [la guía de despliegue](https://example.test/docs/desplegar) antes de publicar.";

fn conversation() -> ViewState {
    let mut state = fixture(false);
    state.selected_backend = "native".into();
    state.prompt.clear();
    state.transcript = vec![
        TranscriptItem::User {
            content: "Levantá el servidor y decime dónde verlo. Referencia: https://vite.dev/guide/".into(),
            images: vec![],
        },
        TranscriptItem::Tool {
            id: "cmd-1".into(),
            name: "run_command".into(),
            arguments: json!({"command": "npm run dev"}),
            output: Some("  VITE v7.1.0  ready in 312 ms\n\n  ➜  Local:   http://localhost:5173/\n  ➜  Network: use --host to expose".into()),
            is_error: false,
            activity: ActivityStatus::Completed,
        },
        TranscriptItem::Assistant { content: ANSWER.into(), streaming: false },
    ];
    state
}

/// The centre of the last painted fragment of the link to `url`.
fn link(frame: &Frame, url: &str) -> Point {
    let rect = frame
        .regions
        .iter()
        .rev()
        .find(|region| web_link_url_from_action(&region.id).as_deref() == Some(url))
        .unwrap_or_else(|| panic!("no link to {url}"))
        .rect;
    Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
}

fn hover(tree: &mut UiTree, viewport: Size, point: Point) -> Frame {
    let start = Instant::now();
    tree.handle_event_at(UiEvent::PointerMove(point), start);
    tree.layout_at(viewport, start + Duration::from_secs(1))
}

fn settings(state: &ViewState, viewport: Size) -> Element {
    Element::panel(build_settings_page(state, SettingsPage::Browser, viewport.width, false))
        .background(crate::theme::BACKGROUND)
}

#[test]
fn every_address_of_the_conversation_is_a_link_at_both_widths() {
    for viewport in [WIDE, NARROW] {
        let state = conversation();
        let (context, mut tree) = native_tree(root(&state, viewport));
        let frame = tree.layout(viewport);
        for url in [
            "https://vite.dev/guide/",
            "http://localhost:5173/",
            "https://github.com/openai/codex/pull/1234",
            "https://developer.mozilla.org/es/docs/Web/HTTP/Headers/Content-Security-Policy",
            "https://example.test/docs/desplegar",
        ] {
            let point = link(&frame, url);
            assert!(point.x > 0.0 && point.x < viewport.width, "{url} at {}", viewport.width);
        }
        for region in frame.regions.iter().filter(|region| region.id.starts_with("chat_link_open:")) {
            assert!(
                region.rect.x >= 0.0 && region.rect.x + region.rect.width <= viewport.width,
                "{:?} leaves a {} px window",
                region.rect,
                viewport.width
            );
        }
        context.end_pass().textures_delta.clear();
    }
}

#[test]
#[ignore = "writes PNG/SVG previews under docs/ui-separation/shots/chat-links"]
fn export_chat_links_conversation_visual_review() {
    let directory = Path::new(SHOTS);
    std::fs::create_dir_all(directory).unwrap();
    for viewport in [WIDE, NARROW] {
        let state = conversation();
        let (context, mut tree) = native_tree(root(&state, viewport));
        let rest = tree.layout(viewport);
        export(&rest, directory, "l1-idle", viewport);
        let named = link(&rest, "https://example.test/docs/desplegar");
        export(&hover(&mut tree, viewport, named), directory, "l2-hover-named-link", viewport);
        let local = link(&rest, "http://localhost:5173/");
        export(&hover(&mut tree, viewport, local), directory, "l3-hover-localhost", viewport);
        context.end_pass().textures_delta.clear();

        for (name, url) in [
            ("l4-menu-named-link", "https://example.test/docs/desplegar"),
            ("l5-menu-localhost", "http://localhost:5173/"),
            ("l6-menu-github", "https://github.com/openai/codex/pull/1234"),
        ] {
            let mut state = conversation();
            let point = link(&rest, url);
            let id = rest.hit_test(point).unwrap().to_owned();
            state.chat_link_menu = ChatLinkMenu::of(&rest, &id);
            let menu = state.chat_link_menu.clone().unwrap();
            let (context, mut tree) =
                native_tree(menu.attach(root(&state, viewport), state.chat_link_opening, true));
            tree.layout(viewport);
            tree.handle_event(UiEvent::ContextMenu { position: point });
            assert!(tree.context_menu_open());
            export(&tree.layout(viewport), directory, name, viewport);
            context.end_pass().textures_delta.clear();
        }

        let mut state = conversation();
        state.show_settings = true;
        state.settings_page = SettingsPage::Browser;
        let (context, mut tree) = native_tree(settings(&state, viewport));
        export(&tree.layout(viewport), directory, "l7-settings-row", viewport);
        context.end_pass().textures_delta.clear();
        state.settings_choice_menu_open = Some("chat_links".into());
        let (context, mut tree) = native_tree(settings(&state, viewport));
        export(&tree.layout(viewport), directory, "l8-settings-choices", viewport);
        context.end_pass().textures_delta.clear();
    }
}
