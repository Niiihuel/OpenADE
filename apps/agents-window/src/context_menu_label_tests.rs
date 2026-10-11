//! Native-font regression coverage for context-menu label budgets and scroll.
use eframe::egui;
use openide_ui::{
    Component, ContextMenu, ContextMenuEntry, ContextMenuItem, DrawCommand, Element, FontFamily,
    FontMetrics, Icon, Key, Point, Size, UiAction, UiEvent, UiTree,
};

fn native_tree(entries: Vec<ContextMenuEntry>) -> (egui::Context, UiTree) {
    let ctx = egui::Context::default();
    crate::fonts::install(&ctx);
    ctx.begin_pass(egui::RawInput::default());
    let mut tree = UiTree::new(
        ContextMenu::new(
            "owner",
            Element::button("owner", "Editor").size(Some(120.0), Some(32.0)),
            entries,
            crate::theme::menu_palette(),
        )
        .render(),
    );
    tree.set_reduced_motion(true);
    let font = egui::FontId::proportional(14.0);
    tree.set_sans_metrics(ctx.fonts_mut(|fonts| FontMetrics {
        char_width: fonts.glyph_width(&font, 'M'),
        line_height: fonts.row_height(&font),
    }));
    let measurer = ctx.clone();
    tree.set_text_measurer(move |text, size, family| {
        measurer.fonts_mut(|fonts| {
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
    (ctx, tree)
}
fn key(key: Key) -> UiEvent {
    UiEvent::Key {
        key,
        shift: false,
        ctrl: false,
    }
}

#[test]
fn native_context_labels_are_complete_and_accelerators_never_displace_them() {
    let labels = [
        "Copiar salida completa",
        "Guardar todos los cambios del archivo seleccionado en la carpeta de trabajo",
        "Renombrar símbolo…",
    ];
    for width in [180.0, 520.0] {
        let entries = labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                ContextMenuEntry::Item(
                    ContextMenuItem::new(format!("action:{index}"), *label)
                        .shortcut("Ctrl+Shift+Z")
                        .icon(Icon::File),
                )
            })
            .collect();
        let (ctx, mut tree) = native_tree(entries);
        let size = Size::new(width, 900.0);
        tree.layout(size);
        tree.focus("owner");
        tree.handle_event(UiEvent::ContextMenuKeyboard);
        let frame = tree.layout(size);
        let menu = frame
            .regions
            .iter()
            .find(|r| r.id == "__context_menu:surface")
            .unwrap()
            .rect;
        assert_eq!(menu.width, 352.0_f32.min(width - 16.0));
        assert!(menu.height <= 384.0);
        let painted = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text {
                    text, font_size, ..
                } if *font_size == 13.0 => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(
            painted,
            labels.concat(),
            "native glyphs must retain every label at {width}px"
        );
        let wrapped = frame
            .regions
            .iter()
            .find(|r| r.id == "__context_menu:item:1")
            .unwrap()
            .rect;
        assert!(wrapped.height > 32.0);
        let keys: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text {
                    text,
                    font_size,
                    position,
                    ..
                } if *font_size == 11.0 && wrapped.contains(*position) && text != "+" => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect();
        if width > 200.0 {
            assert_eq!(
                keys,
                if cfg!(target_os = "macos") {
                    vec!["⌃", "⇧", "Z"]
                } else {
                    vec!["Ctrl", "Shift", "Z"]
                },
                "a wrapped label retains the complete shortcut at normal menu width"
            );
        } else {
            assert!(
                keys.is_empty(),
                "the narrow viewport omits the entire chord"
            );
        }
        let caps: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRect { rect, radius, .. }
                    if *radius == 3.0
                        && rect.height == 18.0
                        && wrapped.contains(Point::new(rect.x, rect.y)) =>
                {
                    Some(rect)
                }
                _ => None,
            })
            .collect();
        assert_eq!(caps.len(), if width > 200.0 { 3 } else { 0 });
        assert!(caps
            .iter()
            .all(|cap| cap.x + cap.width <= wrapped.x + wrapped.width
                && cap.y + cap.height <= wrapped.y + wrapped.height));
        assert_eq!(tree.focused_id(), Some("owner"));
        tree.handle_event(key(Key::End));
        tree.layout(size);
        assert_eq!(
            tree.handle_event(key(Key::Enter)),
            vec![UiAction::Clicked {
                id: "action:2".into()
            }]
        );
        ctx.end_pass().textures_delta.clear();
    }
}

#[test]
fn native_wrapped_context_rows_keep_last_action_reachable_with_wheel_and_keyboard() {
    let entries = (0..18).map(|index| ContextMenuEntry::Item(ContextMenuItem::new(
        format!("action:{index}"), format!("Guardar todos los cambios del archivo seleccionado en la carpeta de trabajo {index}"),
    ).shortcut("Ctrl+Shift+Z"))).collect();
    let (ctx, mut tree) = native_tree(entries);
    let size = Size::new(360.0, 320.0);
    tree.layout(size);
    tree.focus("owner");
    tree.handle_event(UiEvent::ContextMenuKeyboard);
    let initial = tree.layout(size);
    let menu = initial
        .regions
        .iter()
        .find(|r| r.id == "__context_menu:surface")
        .unwrap()
        .rect;
    assert!(menu.height <= size.height * 0.75);
    tree.handle_event(key(Key::End));
    let last = tree.layout(size);
    let target = last
        .regions
        .iter()
        .find(|r| r.id == "__context_menu:item:17")
        .unwrap()
        .rect;
    assert!(menu.contains(Point::new(target.x + 4.0, target.y + target.height / 2.0)));
    assert_eq!(
        tree.handle_event(key(Key::Enter)),
        vec![UiAction::Clicked {
            id: "action:17".into()
        }]
    );
    tree.layout(size);
    tree.handle_event(UiEvent::ContextMenuKeyboard);
    tree.layout(size);
    tree.handle_event(UiEvent::ScrollDirect {
        position: Point::new(menu.x + 20.0, menu.y + 20.0),
        delta_y: -10_000.0,
    });
    let last = tree.layout(size);
    let target = last
        .regions
        .iter()
        .find(|r| r.id == "__context_menu:item:17")
        .unwrap()
        .rect;
    let point = Point::new(target.x + 4.0, target.y + target.height / 2.0);
    assert_eq!(last.hit_test(point), Some("__context_menu:item:17"));
    tree.handle_event(UiEvent::PointerDown(point));
    assert_eq!(
        tree.handle_event(UiEvent::PointerUp(point)),
        vec![UiAction::Clicked {
            id: "action:17".into()
        }]
    );
    ctx.end_pass().textures_delta.clear();
}
