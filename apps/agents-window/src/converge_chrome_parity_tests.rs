//! Parity tests of c04-converge-chrome-chat: each old expression beside the
//! limn call that replaced it. The same element tree, and the same draw
//! commands and regions idle and, over every target, hovered, with the hint
//! open and pressed, and the same action on click.

use std::time::{Duration, Instant};

use crate::{theme, ui_sizing};
use openide_ui::{
    Button, ButtonVariant, Component, ControlSize, DrawCommand, Element, FontFamily, Frame, Icon,
    IconButton, Markdown, Point, RadiusRole, Size, Text, TextArea, Tooltip, TooltipPlacement,
    UiAction, UiEvent, UiTree,
};

/// The idle frame, then three frames and a click for each target in it.
fn states(element: Element, width: f32) -> (Vec<Frame>, Vec<Vec<UiAction>>) {
    let size = Size::new(width, 700.0);
    let mut tree = UiTree::new(Element::column(vec![
        Element::spacer().size(None, Some(100.0)),
        element,
        Element::spacer().flex(1.0),
    ]));
    tree.set_reduced_motion(true);
    let idle = tree.layout(size);
    let targets: Vec<_> = idle.regions.iter().map(|region| region.rect).collect();
    let (mut frames, mut actions) = (vec![idle], Vec::new());
    for target in targets {
        let point = Point::new(
            target.x + target.width / 2.0,
            target.y + target.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerMove(point));
        frames.push(tree.layout(size));
        frames.push(tree.layout_at(size, Instant::now() + Duration::from_secs(2)));
        tree.handle_event(UiEvent::PointerDown(point));
        frames.push(tree.layout(size));
        actions.push(tree.handle_event(UiEvent::PointerUp(point)));
    }
    (frames, actions)
}

fn assert_same_at(name: &str, widths: &[f32], legacy: Element, current: Element) {
    // The same tree is the same frame at every width; the frames say so twice.
    assert_eq!(format!("{current:?}"), format!("{legacy:?}"), "{name}");
    let clips = true;
    let drawn = |frame: &Frame| -> Vec<DrawCommand> {
        frame
            .commands
            .iter()
            .filter(|command| {
                clips || !matches!(command, DrawCommand::PushClip(_) | DrawCommand::PopClip)
            })
            .cloned()
            .collect()
    };
    for &width in widths {
        let (legacy_frames, legacy_actions) = states(legacy.clone(), width);
        let (frames, actions) = states(current.clone(), width);
        assert_eq!(frames.len(), legacy_frames.len(), "{name} width={width}");
        for (index, (frame, legacy)) in frames.iter().zip(&legacy_frames).enumerate() {
            let (commands, legacy_commands) = (drawn(frame), drawn(legacy));
            // Name the first command that differs; a whole frame is unreadable.
            for (command, legacy) in commands.iter().zip(&legacy_commands) {
                assert_eq!(command, legacy, "{name} width={width} frame={index}");
            }
            assert_eq!(commands.len(), legacy_commands.len(), "{name} width={width} frame={index}");
            assert_eq!(frame.regions, legacy.regions, "{name} width={width} frame={index}");
            if clips {
                assert_eq!(frame, legacy, "{name} width={width} frame={index}");
            }
        }
        assert_eq!(actions, legacy_actions, "{name} width={width} clicks");
    }
}

const WIDTHS: [f32; 3] = [120.0, 360.0, 900.0];

#[test]
fn the_comparison_sees_a_radius_a_hover_fill_and_a_click() {
    let button = |role| {
        Button::new("a", "Guardar", theme::component_palette())
            .radius(role)
            .render()
    };
    let (frames, actions) = states(button(RadiusRole::Control), 360.0);
    assert_eq!(frames.len(), 4, "idle and three frames for the button");
    assert_ne!(frames[0], frames[1], "hover paints a fill");
    assert_eq!(actions[0], vec![UiAction::Clicked { id: "a".into() }]);
    assert_ne!(frames[0], states(button(RadiusRole::Surface), 360.0).0[0]);
}

#[test]
fn buttons_with_a_profile_match_their_patched_renders() {
    let palette = theme::component_palette();
    // The terminal's "Copiar", in its tooltip.
    let hint = |button: Element| {
        Tooltip::new(button, "terminal_copy", "Copiar selección o salida visible", palette)
            .shortcut("Ctrl+Shift+C")
            .placement(TooltipPlacement::Below)
            .render()
    };
    let copy = || Button::new("terminal_copy", "Copiar", palette).variant(ButtonVariant::Ghost);
    assert_same_at(
        "terminal copy",
        &WIDTHS,
        hint(theme::interactive_surface(
            copy()
                .size(ControlSize::Compact)
                .render()
                .font_size(11.0)
                .size(Some(48.0), Some(32.0)),
        )),
        hint(copy().sizing(ui_sizing::TERMINAL_COPY).width(48.0).render()),
    );
    // A closed tab of the overview.
    let reopen = || {
        Button::new(
            "dock_reopen:1",
            "Una pestaña de nombre largo",
            theme::action_palette(theme::BACKGROUND, theme::FOREGROUND),
        )
        .icon(Icon::History)
        .variant(ButtonVariant::Subtle)
    };
    assert_same_at(
        "overview reopen",
        &WIDTHS,
        reopen()
            .render()
            .font_size(12.0)
            .min_size(0.0, 0.0)
            .size(None, Some(28.0)),
        reopen().sizing(ui_sizing::OVERVIEW_REOPEN).render(),
    );
    // The settings sidebar: a destination, selected and not, and the way back.
    for active in [false, true] {
        let (fill, ink) = if active {
            (theme::SELECTED, theme::FOREGROUND)
        } else {
            (theme::SIDEBAR, theme::SETTINGS_FOREGROUND_SUBTLE)
        };
        let item = || {
            IconButton::new("settings_page:general", Icon::Settings, theme::action_palette(fill, ink))
                .label("General")
                .variant(ButtonVariant::Subtle)
        };
        assert_same_at(
            "settings destination",
            &WIDTHS,
            item()
                .size(ControlSize::Compact)
                .active(active)
                .render()
                .font_size(crate::settings_parts::LABEL_SIZE)
                .radius(6.0)
                .size(None, Some(crate::sidebar_chrome::NAV_ROW_HEIGHT)),
            item()
                .sizing(ui_sizing::SETTINGS_NAV)
                .radius(RadiusRole::Surface)
                .active(active)
                .render(),
        );
    }
    let back = || {
        IconButton::new(
            "toggle_settings",
            Icon::ArrowLeft,
            theme::action_palette(theme::SIDEBAR, theme::SETTINGS_FOREGROUND_SUBTLE),
        )
        .label("Volver a OpenIDE")
        .variant(ButtonVariant::Ghost)
    };
    assert_same_at(
        "settings back",
        &WIDTHS,
        back()
            .size(ControlSize::Compact)
            .render()
            .font_size(crate::settings_parts::LABEL_SIZE)
            .size(None, Some(crate::sidebar_chrome::NAV_ROW_HEIGHT)),
        back().sizing(ui_sizing::SETTINGS_NAV).render(),
    );
    // The composer's link chip, in its tooltip.
    let chip_palette = theme::action_palette(theme::INPUT, theme::LINK);
    for item_width in [48.0, 178.0, 280.0] {
        let chip = || {
            Button::new("chat_link_open:1:0:https://github.com/a", "github.com/a", chip_palette)
                .icon(Icon::Github)
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Dense)
        };
        let hint = |button: Element| {
            Tooltip::new(button, "chip:tooltip", "https://github.com/a", palette).render()
        };
        assert_same_at(
            "composer link",
            &WIDTHS,
            hint(
                chip()
                    .width(item_width)
                    .render()
                    .font_size(12.0)
                    .radius(6.0)
                    .size(Some(item_width), Some(24.0)),
            ),
            hint(chip().radius(RadiusRole::Surface).width(item_width).render()),
        );
    }
}

#[test]
fn buttons_on_the_canvas_palette_match_their_wrapped_renders() {
    let palette = theme::component_palette();
    let plain = |id: &str, label: &str, variant, size| {
        Button::new(id, label, palette).variant(variant).size(size)
    };
    let cases = || {
        [
        plain("terminal_restart", "Reiniciar", ButtonVariant::Ghost, ControlSize::Compact),
        plain("terminal_bottom", "Volver al prompt", ButtonVariant::Ghost, ControlSize::Compact),
        plain("open", "Ver respuesta completa", ButtonVariant::Subtle, ControlSize::Regular)
            .icon(Icon::File),
        plain("previous", "Anterior", ButtonVariant::Subtle, ControlSize::Regular),
        plain("next", "Siguiente", ButtonVariant::Subtle, ControlSize::Regular).disabled(),
        plain(
            "dock_open:review",
            "Ver cambios del repositorio",
            ButtonVariant::Outline,
            ControlSize::Compact,
        ),
        ]
    };
    for (wrapped, bare) in cases().into_iter().zip(cases()) {
        assert_same_at(
            "unwrapped",
            &WIDTHS,
            theme::interactive_surface(wrapped.render()),
            bare.render(),
        );
    }
}

#[test]
fn the_commit_box_matches_its_patched_render() {
    let area = |enabled| {
        TextArea::new(
            "environment_git_commit_message",
            "",
            "Describe los cambios preparados",
            theme::component_palette(),
        )
        .width(300.0)
        .height(80.0)
        .enabled(enabled)
    };
    for enabled in [true, false] {
        assert_same_at(
            "commit message",
            &WIDTHS,
            area(enabled).render().radius(6.0),
            area(enabled).radius(RadiusRole::Surface).render(),
        );
    }
}

#[test]
fn text_off_the_scale_matches_its_hand_written_chains() {
    let palette = theme::component_palette();
    for size in [10.5, 11.5] {
        let chain = |ink| {
            Element::truncated_text("Sesión aislada · Compartida con el agente")
                .font_family(FontFamily::Sans)
                .font_size(size)
                .foreground(ink)
        };
        assert_same_at(
            "muted line",
            &WIDTHS,
            chain(theme::FOREGROUND_SUBTLE),
            Text::new("Sesión aislada · Compartida con el agente", palette)
                .size(size)
                .muted()
                .truncate()
                .render(),
        );
        assert_same_at(
            "inked line",
            &WIDTHS,
            chain(theme::WARNING),
            Text::new("Sesión aislada · Compartida con el agente", palette)
                .size(size)
                .ink(theme::WARNING)
                .truncate()
                .render(),
        );
    }
    assert_same_at(
        "gutter number",
        &WIDTHS,
        Element::text("  42")
            .font_family(FontFamily::Monospace)
            .font_size(10.5)
            .foreground(theme::CODE_GUTTER),
        Text::new("  42", palette)
            .mono()
            .size(10.5)
            .ink(theme::CODE_GUTTER)
            .render(),
    );
}

fn legacy_web_url(value: &str) -> Option<reqwest::Url> {
    if value.len() > 2_048
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '\\')
    {
        return None;
    }
    let url = reqwest::Url::parse(value).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

fn legacy_link_icon(url: &reqwest::Url) -> Icon {
    match url.host_str() {
        Some("github.com" | "www.github.com") => Icon::Github,
        _ => Icon::Globe,
    }
}

#[test]
fn limn_reads_addresses_as_the_two_copies_did() {
    let long = format!("https://example.test/{}", "a".repeat(2_048));
    for value in [
        "https://github.com/openai/codex",
        "HTTPS://GITHUB.COM/a",
        "https://www.github.com/a",
        "https://github.com.evil.test/a",
        "https://docs.github.com/a",
        "http://localhost:3000/?url=github.com",
        "https://example.test:8080/a?x=1#part",
        "https://example.test/ñ (a)",
        "file:///tmp/a",
        "javascript:alert(1)",
        "https://github.com@evil.test/a",
        "https://user:password@github.com/a",
        "https://github.com\\@evil.test",
        "src/main.rs",
        "https://example.test/\nnext",
        "",
        long.as_str(),
    ] {
        let legacy = legacy_web_url(value);
        assert_eq!(
            Markdown::web_link(value),
            legacy.as_ref().map(|url| url.to_string()),
            "{value}"
        );
        if let Some(url) = legacy {
            assert_eq!(Markdown::link_icon(url.as_str()), legacy_link_icon(&url), "{value}");
        }
    }
}
