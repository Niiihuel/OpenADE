use super::*;
use openide_ui::{DrawCommand, FontFamily, Key, Size, UiAction, UiEvent, UiTree};

fn actions(element: Element) -> Vec<String> {
    UiTree::new(element)
        .layout(Size::new(640.0, 2000.0))
        .regions
        .into_iter()
        .filter_map(|region| {
            region
                .id
                .starts_with("chat_link_open:")
                .then_some(region.id)
        })
        .collect()
}

#[test]
fn web_destinations_reject_unsafe_schemes_and_spoofed_github_hosts() {
    for url in [
        "https://github.com/openai/codex",
        "HTTPS://GITHUB.COM/a",
        "https://www.github.com/a",
    ] {
        assert_eq!(link_icon(&web_url(url).unwrap()), Icon::Github);
    }
    for url in [
        "https://github.com.evil.test/a",
        "https://evilgithub.com/a",
        "https://docs.github.com/a",
        "http://localhost:3000/?url=github.com",
    ] {
        assert_eq!(link_icon(&web_url(url).unwrap()), Icon::Globe);
    }
    for url in [
        "file:///tmp/a",
        "javascript:alert(1)",
        "https://github.com@evil.test/a",
        "https://user:password@github.com/a",
        "https://github.com\\@evil.test",
        "src/main.rs",
        "https://example.test/\nnext",
    ] {
        assert!(web_url(url).is_none(), "{url}");
        assert!(web_link_url_from_action(&link_action("test", 0, url)).is_none());
    }
    let target = "https://example.test:8080/a?x=1#part";
    assert_eq!(
        web_link_url_from_action(&link_action("test", 4, target)),
        Some(target.into())
    );
    assert!(web_link_url_from_action("chat_link_open:not-hex:4:https://example.test").is_none());
}

#[test]
fn partial_stream_urls_become_actionable_only_when_the_target_is_complete() {
    for source in [
        "Ver https://github.",
        "Ver https://github.co",
        "Ver https://github.com/a/b",
    ] {
        assert!(actions(render_streaming_markdown_at_width(source, 420.0)).is_empty());
    }
    let source = "Ver https://github.com/a/b ";
    let links = actions(render_streaming_markdown_at_width(source, 420.0));
    assert!(!links.is_empty());
    assert!(links
        .iter()
        .all(|id| web_link_url_from_action(id).as_deref() == Some("https://github.com/a/b")));
    assert!(!actions(render_markdown_at_width(
        "Ver https://github.com/a/b",
        420.0
    ))
    .is_empty());
    assert!(!actions(render_streaming_markdown_at_width(
        "[repo](https://github.com/a/b)",
        420.0
    ))
    .is_empty());
}

#[test]
fn bounded_composer_and_markdown_never_open_a_truncated_host() {
    // 20,000 characters is the default `MarkdownLimits::source_chars`.
    let prefix = "x".repeat(20_000 - " https://github.co".chars().count());
    let source = format!("{prefix} https://github.com/a/b");
    let draft_links = composer_web_links(&source);
    assert_eq!(draft_links.len(), 1);
    assert_eq!(draft_links[0].1.as_str(), "https://github.com/a/b");
    let preview = render_markdown_preview_at_width(&source, 400.0);
    assert!(preview.limited);
    assert!(actions(preview.element).is_empty());
}

#[test]
fn repeated_cached_links_receive_distinct_message_and_block_focus_targets() {
    let source = "[repo](https://github.com/a/b)\n\n[repo](https://github.com/a/b)";
    let first = scope_web_links(render_markdown_at_width(source, 640.0), "session-a:item-1");
    let second = scope_web_links(render_markdown_at_width(source, 640.0), "session-a:item-2");
    let mut tree = UiTree::new(Element::column(vec![first, second]));
    let frame = tree.layout(Size::new(640.0, 500.0));
    let unique = frame
        .regions
        .iter()
        .filter(|region| region.id.starts_with("chat_link_open:"))
        .map(|region| region.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), 4);
    let mut visited = std::collections::BTreeSet::new();
    for _ in 0..4 {
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        visited.insert(tree.focused_id().unwrap().to_owned());
    }
    assert_eq!(visited.len(), 4);
}

#[test]
fn composer_urls_keep_unicode_source_and_actions_fit_the_narrow_card() {
    let source = "ñ 👩‍💻 [repo](https://github.com/a/b), https://example.test/docs";
    let links = composer_web_links(source);
    let first = source
        .chars()
        .skip(links[0].0.start)
        .take(links[0].0.len())
        .collect::<String>();
    assert_eq!(first, "https://github.com/a/b");
    for width in [200.0, 280.0, 360.0] {
        let props = crate::composer_ui::tests::props(source, width);
        let mut tree = UiTree::new(crate::composer_ui::build_composer(&props));
        let frame = tree.layout(Size::new(width, 600.0));
        assert!(frame
            .regions
            .iter()
            .filter(|region| region.id.starts_with("chat_link_open:"))
            .all(
                |region| region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.01
            ));
        assert!(frame.commands.iter().any(|cmd| matches!(
            cmd,
            DrawCommand::Icon {
                icon: Icon::Github,
                ..
            }
        )));
        assert!(tree.focus("prompt"));
        tree.handle_event(UiEvent::Key {
            key: Key::A,
            shift: false,
            ctrl: true,
        });
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::C,
                shift: false,
                ctrl: true
            }),
            vec![UiAction::CopyRequested {
                text: source.into()
            }]
        );
        assert_eq!(tree.input_value("prompt"), Some(source));
    }
    let user = render_user_message(source, "session:user1");
    let frame = UiTree::new(user).layout(Size::new(640.0, 500.0));
    assert_eq!(frame.text_layouts()[0].source, source);
}

#[test]
#[ignore = "export native link and composer rendering for visual review"]
fn export_chat_links_visual_review() {
    let output = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/chat-links-visual-qa");
    std::fs::create_dir_all(&output).unwrap();
    for width in [280.0, 360.0, 640.0] {
        let prompt =
            "Revisa https://github.com/zed-industries/zed y https://code.visualstudio.com/docs";
        let props = crate::composer_ui::tests::props(prompt, width - 24.0);
        let response = "Encontré el cambio en [GitHub](https://github.com/zed-industries/zed). La [documentación](https://code.visualstudio.com/docs) explica cómo mantener el texto editable.\n\nRuta local: `src/main.rs`. Sigo revisando…";
        let root = Element::column(vec![
            Element::text("Enlaces en la respuesta")
                .foreground(theme::FOREGROUND_SUBTLE)
                .font_size(12.0),
            scope_web_links(
                render_streaming_markdown_at_width(response, width - 24.0),
                "visual:response",
            ),
            Element::spacer().size(None, Some(24.0)),
            crate::composer_ui::build_composer(&props),
        ])
        .gap(12.0)
        .padding(12.0)
        .background(theme::BACKGROUND)
        .size(Some(width), Some(520.0));
        let context = eframe::egui::Context::default();
        crate::fonts::install(&context);
        context.begin_pass(eframe::egui::RawInput::default());
        let fonts = context.clone();
        let mut tree = UiTree::new(root);
        tree.set_text_measurer(move |text, size, family| {
            fonts.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(
                        text.into(),
                        eframe::egui::FontId::new(
                            size,
                            match family {
                                FontFamily::Sans => eframe::egui::FontFamily::Proportional,
                                FontFamily::Monospace => eframe::egui::FontFamily::Monospace,
                            },
                        ),
                        eframe::egui::Color32::WHITE,
                    )
                    .size()
                    .x
            })
        });
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(width, 520.0));
        let path = output.join(format!("chat-links-{width}.svg"));
        crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, 520.0).unwrap();
        let svg = std::fs::read(&path).unwrap();
        let tree = resvg::usvg::Tree::from_data(
            &svg,
            &crate::settings_ui::visual_qa::native_svg_options(),
        )
        .unwrap();
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 520).unwrap();
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        pixmap.save_png(path.with_extension("png")).unwrap();
        let mut output = context.end_pass();
        output.textures_delta.clear();
    }
}
