use super::*;
use openide_ui::{DrawCommand, FontFamily, Icon, Key, Size, UiAction, UiEvent, UiTree};

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
        assert_eq!(Markdown::link_icon(&Markdown::web_link(url).unwrap()), Icon::Github);
    }
    for url in [
        "https://github.com.evil.test/a",
        "https://evilgithub.com/a",
        "https://docs.github.com/a",
        "http://localhost:3000/?url=github.com",
    ] {
        assert_eq!(Markdown::link_icon(&Markdown::web_link(url).unwrap()), Icon::Globe);
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
        assert!(Markdown::web_link(url).is_none(), "{url}");
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
    let draft_links = Markdown::web_links(&source);
    assert_eq!(draft_links.len(), 1);
    assert_eq!(draft_links[0].1.as_str(), "https://github.com/a/b");
    let preview = render_markdown_preview_at_width(&source, 400.0);
    assert!(preview.limited);
    assert!(actions(preview.element).is_empty());
}

#[test]
fn composer_offers_no_chip_for_an_address_too_long_once_encoded() {
    // 400 two-byte letters fit the 2,048-byte limit as typed and pass it percent-encoded.
    let source = format!("https://example.test/{}", "ñ".repeat(400));
    let written = Markdown::web_links(&source);
    assert_eq!(written.len(), 1);
    assert!(Markdown::web_link(&written[0].1).is_none());
    let (highlights, chips) = composer_links(&source, 360.0);
    assert!(highlights.is_empty());
    assert!(chips.is_none());
    let (highlights, chips) = composer_links("https://example.test/ñ", 360.0);
    assert_eq!(highlights.len(), 1);
    assert!(chips.is_some());
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
    let links = Markdown::web_links(source);
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
                                FontFamily::Sans | FontFamily::SansItalic => eframe::egui::FontFamily::Proportional,
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

/// Every span of a rendered tree, through the hints wrapped around its
/// paragraphs.
fn spans(element: &Element, out: &mut Vec<openide_ui::TextSpan>) {
    match &element.kind {
        ElementKind::Column(children) | ElementKind::Row(children) => {
            children.iter().for_each(|child| spans(child, out))
        }
        ElementKind::Panel(child) => spans(child, out),
        ElementKind::Tooltip { anchor, .. } => spans(anchor, out),
        ElementKind::FormattedText(found) => out.extend(found.iter().cloned()),
        _ => {}
    }
}

/// The links of a settled response: their words and where they open.
fn links(source: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    spans(&scope_web_links(render_markdown_at_width(source, 640.0), "t"), &mut found);
    found
        .into_iter()
        .filter_map(|span| Some((span.text, web_link_url_from_action(span.action.as_deref()?)?)))
        .collect()
}

#[test]
fn detection_table_of_a_settled_response() {
    let long = format!("https://example.test/{}", "a".repeat(1_900));
    let too_long = format!("https://example.test/{}", "a".repeat(2_100));
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        // Markdown links: the words stay, the destination is the address.
        ("[guía](https://example.test/docs)", vec![("guía", "https://example.test/docs")]),
        ("[wiki](https://es.wikipedia.org/wiki/Rust_(lenguaje))", vec![("wiki", "https://es.wikipedia.org/wiki/Rust_(lenguaje)")]),
        // An autolink keeps its angle brackets as text around the link.
        ("<https://example.test/a>", vec![("https://example.test/a", "https://example.test/a")]),
        // Bare addresses, and the punctuation of the sentence around them.
        ("Puedes abrirlo en http://localhost:5173.", vec![("http://localhost:5173", "http://localhost:5173/")]),
        ("Mira https://example.test/a, https://example.test/b; y https://example.test/c!", vec![
            ("https://example.test/a", "https://example.test/a"),
            ("https://example.test/b", "https://example.test/b"),
            ("https://example.test/c", "https://example.test/c"),
        ]),
        ("(ver https://example.test/a)", vec![("https://example.test/a", "https://example.test/a")]),
        ("[ver https://example.test/a]:", vec![("https://example.test/a", "https://example.test/a")]),
        ("https://es.wikipedia.org/wiki/Rust_(lenguaje).", vec![("https://es.wikipedia.org/wiki/Rust_(lenguaje)", "https://es.wikipedia.org/wiki/Rust_(lenguaje)")]),
        ("http://127.0.0.1:8080/x?y=1#z y http://[::1]:3000/", vec![
            ("http://127.0.0.1:8080/x?y=1#z", "http://127.0.0.1:8080/x?y=1#z"),
            ("http://[::1]:3000/", "http://[::1]:3000/"),
        ]),
        // Unicode: the path is percent-encoded and the host goes to punycode.
        ("https://example.test/año", vec![("https://example.test/año", "https://example.test/a%C3%B1o")]),
        ("https://ñandú.test/", vec![("https://ñandú.test/", "https://xn--and-6ma2c.test/")]),
        ("HTTPS://GitHub.com/a", vec![("HTTPS://GitHub.com/a", "https://github.com/a")]),
        (long.as_str(), vec![(long.as_str(), long.as_str())]),
        // Never a link: code, other schemes, credentials, an address glued
        // to a word, one past the size limit, and a host with no scheme
        // (the parser has no rule for it; see the module notes).
        ("`https://example.test/a`", vec![]),
        ("```\nhttps://example.test/a\n```", vec![]),
        ("    https://example.test/indentado", vec![("https://example.test/indentado", "https://example.test/indentado")]),
        ("[x](javascript:alert(1)) [y](file:///etc/passwd) [z](vscode://a) ftp://example.test mailto:a@b.test", vec![]),
        ("https://user:clave@example.test/", vec![]),
        ("xhttps://example.test/a", vec![]),
        (too_long.as_str(), vec![]),
        ("localhost:5173, 127.0.0.1:8080 y www.example.test", vec![]),
        ("src/main.rs:42 y C:\\repo\\a.rs", vec![]),
    ];
    for (source, expected) in cases {
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(text, url)| (text.to_owned(), url.to_owned()))
            .collect();
        let shown = &source[..source.len().min(80)];
        assert_eq!(links(source), expected, "{shown}");
    }
    // A user's message is literal text; the same addresses are its links.
    let mut found = Vec::new();
    spans(&render_user_message("mirá [esto](https://example.test/a) y `https://example.test/b`", "u"), &mut found);
    let user: Vec<_> = found.iter().filter(|span| span.action.is_some()).map(|span| span.text.as_str()).collect();
    assert_eq!(user, ["https://example.test/a", "https://example.test/b"]);
}

#[test]
fn an_address_that_streams_in_never_goes_back_to_plain_text() {
    let full = "Puedes abrirlo en http://localhost:5173/app. Listo";
    let start = full.find("http").unwrap();
    let end = full.find(". Listo").unwrap();
    let (mut was_link, mut was_open) = (false, false);
    let mut previous_width = 0.0;
    for (cut, _) in full.char_indices().skip(1).chain([(full.len(), ' ')]) {
        let mut found = Vec::new();
        spans(&render_streaming_markdown_at_width(&full[..cut], 640.0), &mut found);
        let address: Vec<_> = found.iter().filter(|span| span.color == theme::LINK).collect();
        let is_link = !address.is_empty();
        let is_open = address.iter().any(|span| span.action.is_some());
        assert!(!was_link || is_link, "back to plain text at {:?}", &full[..cut]);
        assert!(!was_open || is_open, "lost its action at {:?}", &full[..cut]);
        // Link ink from the moment the scheme is whole; before that the
        // letters are prose like any other.
        assert_eq!(is_link, cut >= start + "http://l".len(), "{:?}", &full[..cut]);
        // It opens only once something after it says where it ends.
        assert_eq!(is_open, cut > end + 1, "{:?}", &full[..cut]);
        if is_link {
            assert_eq!(address.len(), 1);
            assert!(full[start..].starts_with(&address[0].text), "{:?}", address[0].text);
            assert_eq!(address[0].icon.is_some(), is_open, "the glyph arrives with the action");
        }
        let width = UiTree::new(render_streaming_markdown_at_width(&full[..cut], 640.0))
            .layout(Size::new(640.0, 200.0))
            .text_layouts()
            .iter()
            .flat_map(|block| block.runs.iter().map(|run| run.rect.x + run.rect.width))
            .fold(0.0, f32::max);
        assert!(width >= previous_width, "the line shrank at {:?}", &full[..cut]);
        (was_link, was_open, previous_width) = (is_link, is_open, width);
    }
    assert!(was_open);
}

#[test]
fn links_wrap_inside_a_narrow_column_and_hint_their_destination() {
    let source = format!(
        "Abrí [la guía de instalación completa](https://example.test/docs/instalar) o \
         https://example.test/{}?paso=2 y http://localhost:5173.",
        "ruta-muy-larga/".repeat(12)
    );
    for width in [360.0_f32, 900.0] {
        let column = width - 32.0;
        let mut tree = UiTree::new(
            scope_web_links(render_markdown_at_width(&source, column), "wrap").size(Some(column), None),
        );
        let start = std::time::Instant::now();
        let frame = tree.layout_at(Size::new(column, 900.0), start);
        let link_regions: Vec<_> = frame.regions.iter().filter(|region| region.id.starts_with("chat_link_open:")).collect();
        assert!(link_regions.len() >= 3);
        for region in &link_regions {
            assert!(region.rect.x >= 0.0 && region.rect.x + region.rect.width <= column + 0.5, "{width}: {:?}", region.rect);
        }
        for run in frame.text_layouts().iter().flat_map(|block| block.runs.clone()) {
            assert!(run.rect.x + run.rect.width <= column + 0.5, "{width}: {:?}", run.text);
        }
        // The hint is the address itself, not the words of the link.
        let named = link_regions.iter().find(|region| region.id.ends_with("/docs/instalar")).unwrap();
        let over = openide_ui::Point::new(named.rect.x + named.rect.width / 2.0, named.rect.y + named.rect.height / 2.0);
        tree.handle_event_at(UiEvent::PointerMove(over), start);
        let hovered = tree.layout_at(Size::new(column, 900.0), start + std::time::Duration::from_secs(1));
        let painted: Vec<_> = hovered.commands.iter().filter_map(|command| match command {
            DrawCommand::Text { text, .. } => Some(text.as_str()),
            _ => None,
        }).collect();
        assert!(painted.contains(&"https://example.test/docs/instalar"), "{width}: {painted:?}");
        assert!(!painted.iter().any(|text| text.contains("localhost:5173/")), "one hint at a time");
    }
}
