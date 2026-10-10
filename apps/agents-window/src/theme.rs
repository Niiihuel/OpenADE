//! Semantic tokens for the Agents Window's neutral dark presentation.
//!
//! Layout and token roles are informed by ZCode's `theme-zai-dark` CSS theme
//! (Apache-2.0). The application uses an achromatic palette for native chrome;
//! the composer send button and reasoning-effort control carry local accents.

use openide_ui::{
    Button, ButtonVariant, Color, ComponentPalette, ControlColors, Element, Scheme, Surface,
};

/// Every neutral of the native chrome, by role. The palettes below are
/// derived from it, so two of them cannot disagree about a shared color.
pub const SCHEME: Scheme = Scheme {
    canvas: Color::rgb(22, 22, 22),
    panel: Color::rgb(32, 32, 32),
    raised: Color::rgb(43, 43, 43),
    hover: Color::rgb(43, 43, 43),
    selected: Color::rgb(54, 54, 54),
    border: Color::rgb(46, 46, 46),
    text: Color::rgb(212, 212, 212),
    muted: Color::rgb(136, 136, 136),
    muted_on_raised: Color::rgb(160, 160, 160),
    menu: Color::rgb(30, 30, 30),
    menu_hover: Color::rgb(48, 48, 48),
    menu_muted: Color::rgb(158, 158, 158),
    shadow: Color::rgba(0, 0, 0, 125),
    control: ControlColors {
        surface: Color::rgb(55, 55, 55),
        hover: Color::rgb(66, 66, 66),
        pressed: Color::rgb(76, 76, 76),
        border: Color::rgb(78, 78, 78),
        text: Color::rgb(235, 235, 235),
    },
};

pub const BACKGROUND: Color = SCHEME.canvas;
pub const WINDOW: Color = SCHEME.raised;
pub const SIDEBAR: Color = SCHEME.raised;
pub const PANEL: Color = SCHEME.panel;
pub const CARD: Color = SCHEME.raised;
/// Floating action menus share the composer popover's dark surface.
pub const MENU_SURFACE: Color = SCHEME.menu;
// Read only by the parity copy of the suggestion popup, until S18.
#[cfg_attr(not(test), allow(dead_code))]
pub const MENU_HOVER: Color = SCHEME.menu_hover;
pub const MENU_MUTED: Color = SCHEME.menu_muted;
pub const INPUT: Color = SCHEME.hover;
// Not a scheme role: still painted as a fill by the dock tabs and sidebar.
pub const HOVER: Color = Color::rgb(34, 34, 34);
pub const SELECTED: Color = SCHEME.selected;
pub const BORDER: Color = SCHEME.border;
pub const TREE_GUIDE: Color = Color::rgb(72, 72, 72);
pub const BRAND: Color = Color::rgb(255, 255, 255);
/// Web destinations stay distinct from neutral text and native chrome.
pub const LINK: Color = Color::rgb(120, 169, 255);
pub const FOREGROUND: Color = SCHEME.text;
pub const FOREGROUND_SUBTLE: Color = SCHEME.muted;
/// Small supporting text in Settings stays legible on cards and controls.
pub const SETTINGS_FOREGROUND_SUBTLE: Color = SCHEME.muted_on_raised;
/// The shadow under the command palette, heavier than a menu's because the
/// palette floats over the whole window.
pub const COMMAND_PALETTE_SHADOW: Color = Color::rgba(0, 0, 0, 155);
pub const PRIMARY_FOREGROUND: Color = Color::rgb(0, 0, 0);
pub const USER: Color = Color::rgb(235, 235, 235);
pub const ASSISTANT: Color = Color::rgb(212, 212, 212);
pub const TOOL_RESULT: Color = Color::rgb(162, 162, 162);
pub const DESTRUCTIVE: Color = Color::rgb(255, 92, 92);
pub const DIFF_ADDED: Color = Color::rgb(139, 207, 163);
pub const DIFF_REMOVED: Color = Color::rgb(224, 145, 145);
pub const DIFF_ADDED_BG: Color = Color::rgb(29, 49, 37);
pub const DIFF_REMOVED_BG: Color = Color::rgb(55, 35, 35);
pub const DIFF_HUNK_BG: Color = Color::rgb(39, 39, 39);
// Syntax color is confined to file contents. The surrounding native chrome
// remains achromatic, including the file preview's surface and gutter.
pub const CODE_SURFACE: Color = Color::rgb(25, 25, 25);
pub const CODE_GUTTER: Color = Color::rgb(108, 108, 108);
pub const CODE_KEYWORD: Color = Color::rgb(191, 177, 200);
pub const CODE_STRING: Color = Color::rgb(168, 192, 166);
pub const CODE_NUMBER: Color = Color::rgb(199, 182, 154);
pub const CODE_COMMENT: Color = Color::rgb(118, 118, 118);
pub const CODE_PUNCTUATION: Color = Color::rgb(159, 159, 159);
pub const ICON_PRIMARY: Color = Color::rgb(224, 224, 224);
/// Accent tones used only by the reasoning-effort control.
pub const EFFORT_BLUE: Color = Color::rgb(80, 145, 255);
pub const EFFORT_ULTRA: Color = Color::rgb(190, 126, 255);
/// A provider's quota nearing its limit.
pub const WARNING: Color = Color::rgb(224, 179, 106);
/// The context meter nearing its limit; a second amber kept as it is.
pub const CONTEXT_WARNING: Color = Color::rgb(225, 169, 65);
/// The unfilled part of a usage bar.
pub const TRACK: Color = Color::rgb(66, 66, 66);
/// Composer pills, their quiet ink and the send button's accent.
pub const PILL_SURFACE: Color = Color::rgb(38, 38, 38);
pub const MUTED_INK: Color = Color::rgb(117, 117, 117);
pub const SEND_BLUE: Color = Color::rgb(37, 113, 251);
pub const TERMINAL_SELECTION: Color = Color::rgb(61, 76, 96);
/// The edge of the focused pane in a split terminal.
pub const ACTIVE_PANE_INDICATOR: Color = Color {
    a: 140,
    ..FOREGROUND_SUBTLE
};
/// A light control on the dark canvas, such as the jump-to-latest button.
pub const INVERSE_SURFACE: Color = Color::rgb(246, 246, 246);
pub const INVERSE_INK: Color = Color::rgb(32, 32, 32);
/// Emphasized prose in rendered Markdown.
pub const EMPHASIS_INK: Color = Color::rgb(194, 194, 194);

pub const SIDEBAR_WIDTH: f32 = 264.0;
pub const UI_FONT_SIZE: f32 = 14.0;

pub fn interactive_surface(element: Element) -> Element {
    component_palette().interactive_surface(element)
}

/// Hover moves one neutral step above the control's own surface. A fixed
/// global hover color would darken the sidebar while brightening the canvas.
pub fn hover_for(surface: Color) -> Color {
    if surface == BRAND || surface == DESTRUCTIVE {
        return surface;
    }
    SCHEME.step(surface)
}

/// The palette of an action painted on `surface`. The role limn needs is
/// read here from the color, since only the app knows which of its colors
/// are flat, raised or solid.
pub fn action_palette(surface: Color, ink: Color) -> ComponentPalette {
    let role = if surface == Color::TRANSPARENT || surface == BACKGROUND || surface == PANEL {
        Surface::Flat(surface)
    } else if surface == BRAND || surface == DESTRUCTIVE {
        Surface::Solid(surface)
    } else {
        Surface::Raised(surface)
    };
    SCHEME.on(role, ink)
}

/// The default palette belongs to the canvas. Raised surfaces opt into
/// action_palette/menu_palette; using sidebar hover here made every
/// breadcrumb and shared ghost control brighter than the composer.
pub fn component_palette() -> ComponentPalette {
    SCHEME.palette()
}

/// The docks' folder tabs: the active tab is the canvas it opens onto, the
/// bar is the panel, and an inactive tab hovers to `HOVER`.
pub fn tab_strip_palette() -> ComponentPalette {
    ComponentPalette {
        surface: BACKGROUND,
        raised: PANEL,
        hover: HOVER,
        ..component_palette()
    }
}

pub fn menu_palette() -> ComponentPalette {
    SCHEME.menu()
}

/// A menu whose rows hover and press like controls on the canvas: the
/// composer's selectors and the text context menu.
pub fn selector_palette() -> ComponentPalette {
    ComponentPalette {
        hover: SCHEME.hover,
        selected: SCHEME.selected,
        ..SCHEME.menu()
    }
}

/// Filled controls inside Settings cards.
pub fn control_palette() -> ComponentPalette {
    SCHEME.on_raised()
}

/// Content laid directly on a Settings card.
pub fn card_palette() -> ComponentPalette {
    SCHEME.card()
}

/// A card filled with the panel color: a hook report, a catalogue entry.
pub fn panel_card_palette() -> ComponentPalette {
    ComponentPalette {
        raised: PANEL,
        ..component_palette()
    }
}

/// A card filled with the canvas, set on a raised page.
pub fn canvas_card_palette() -> ComponentPalette {
    ComponentPalette {
        raised: BACKGROUND,
        ..component_palette()
    }
}

/// Read-only output on the code well.
pub fn code_palette() -> ComponentPalette {
    ComponentPalette {
        surface: CODE_SURFACE,
        ..component_palette()
    }
}

/// Actions in a workspace sidebar.
pub fn sidebar_palette() -> ComponentPalette {
    action_palette(SIDEBAR, FOREGROUND)
}

/// The one filled action of a form or notice: brand fill, primary ink.
pub fn primary(id: impl Into<String>, label: impl Into<String>) -> Button {
    Button::new(id, label, action_palette(BRAND, PRIMARY_FOREGROUND)).variant(ButtonVariant::Solid)
}

pub fn segmented_palette() -> ComponentPalette {
    ComponentPalette {
        surface: SELECTED,
        raised: SELECTED,
        hover: Color::rgb(63, 63, 63),
        selected: BACKGROUND,
        border: BORDER,
        text: FOREGROUND,
        muted: FOREGROUND_SUBTLE,
        shadow: SCHEME.shadow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{Button, Component, Element, ElementKind, Tooltip, TooltipStyle};

    fn tooltip_style(element: Element) -> TooltipStyle {
        let ElementKind::Tooltip { colors, .. } = element.kind else {
            panic!("expected a shared tooltip");
        };
        colors
    }

    #[test]
    fn native_chrome_has_no_hue() {
        for color in [
            BACKGROUND,
            WINDOW,
            SIDEBAR,
            PANEL,
            CARD,
            MENU_SURFACE,
            MENU_HOVER,
            INPUT,
            HOVER,
            SELECTED,
            BORDER,
            BRAND,
            FOREGROUND,
            FOREGROUND_SUBTLE,
            PRIMARY_FOREGROUND,
            USER,
            ASSISTANT,
            TOOL_RESULT,
            ICON_PRIMARY,
        ] {
            assert_eq!((color.r, color.g), (color.g, color.b));
        }
    }

    const fn gray(value: u8) -> Color {
        Color::rgb(value, value, value)
    }

    // The values each palette function returned before it was derived from
    // SCHEME, copied as literals.
    #[test]
    fn every_palette_keeps_the_colors_it_had_before_the_scheme() {
        let shadow = Color::rgba(0, 0, 0, 125);
        let palette = |surface, raised, hover, selected, border, text, muted| ComponentPalette {
            surface,
            raised,
            hover,
            selected,
            border,
            text,
            muted,
            shadow,
        };
        let g = gray;
        assert_eq!(component_palette(), palette(g(22), g(43), g(43), g(54), g(46), g(212), g(136)));
        assert_eq!(menu_palette(), palette(g(30), g(30), g(48), g(48), g(46), g(212), g(158)));
        assert_eq!(control_palette(), palette(g(55), g(55), g(66), g(76), g(78), g(235), g(136)));
        assert_eq!(card_palette(), palette(g(43), g(43), g(43), g(54), g(46), g(212), g(160)));
        assert_eq!(selector_palette(), palette(g(30), g(30), g(43), g(54), g(46), g(212), g(158)));
        assert_eq!(segmented_palette(), palette(g(54), g(54), g(63), g(22), g(46), g(212), g(136)));
        assert_eq!(sidebar_palette(), palette(g(43), g(43), g(51), g(54), g(46), g(212), g(136)));

        let ink = Color::rgb(1, 2, 3);
        let destructive = Color::rgb(255, 92, 92);
        for (surface, hover, selected) in [
            (g(22), g(43), g(54)),
            (g(32), g(43), g(54)),
            (g(43), g(51), g(54)),
            (g(54), g(62), g(54)),
            (g(246), g(254), g(54)),
            (g(255), g(255), g(255)),
            (destructive, destructive, destructive),
            (Color::TRANSPARENT, g(43), g(54)),
        ] {
            assert_eq!(
                action_palette(surface, ink),
                palette(surface, surface, hover, selected, g(46), ink, g(136)),
                "{surface:?}"
            );
        }

        assert_eq!(HOVER, g(34));
        assert_eq!(hover_for(g(43)), g(51));
        assert_eq!(hover_for(destructive), destructive);
        assert_eq!(hover_for(Color::rgba(250, 250, 250, 9)), Color::rgba(255, 255, 255, 9));
    }

    #[test]
    fn the_solid_icon_and_skeleton_rows_match_the_helpers_limn_absorbed() {
        use openide_ui::{DrawCommand, Icon, Size, SkeletonGroup, UiTree};

        for icon in [
            Icon::PanelLeft,
            Icon::PanelRight,
            Icon::Bell,
            Icon::Folder,
            Icon::Play,
            Icon::Grid,
            Icon::Settings,
            Icon::Palette,
            Icon::Laptop,
            Icon::Terminal,
            Icon::Keyboard,
            Icon::Chat,
            Icon::Users,
            Icon::Puzzle,
            Icon::Server,
            Icon::Book,
            Icon::Clock,
            Icon::Bolt,
            Icon::Archive,
            Icon::ChartBar,
            Icon::Search,
            Icon::SettingsFill,
            Icon::PanelLeftClose,
        ] {
            assert_eq!(icon.filled(), legacy_filled_icon(icon));
        }

        let commands = |rows: Vec<Element>, width: f32| -> Vec<DrawCommand> {
            let mut tree = UiTree::new(Element::column(rows));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(width, 400.0)).commands
        };
        for width in [220.0, 760.0] {
            for (count, row_height, icon) in [(4, 30.0, false), (3, 26.0, true), (6, 34.0, true)] {
                let limn = SkeletonGroup::rows("progress", component_palette(), count)
                    .row_height(row_height)
                    .icon(icon)
                    .into_rows();
                let legacy = legacy_skeleton_rows("progress", count, row_height, icon);
                assert_eq!(commands(limn, width), commands(legacy, width));
            }
        }
    }

    // Every hand-built card shell limn's Card replaced, in both the forms it
    // was written in; delete with the final audit (S18).
    #[test]
    fn card_shells_match_the_panels_they_replaced() {
        use openide_ui::{
            ButtonVariant, Card, CardDensity, Component, ControlSize, DrawCommand, Icon,
            IconButton, RadiusRole,
        };

        // Content with a row that centers in a card taller than it.
        let content = || {
            Element::row(vec![
                Element::column(vec![
                    Element::text("Plan propuesto").foreground(FOREGROUND),
                    Element::text("2 de 5 · un texto que se parte en varias líneas cuando la tarjeta es angosta")
                        .foreground(FOREGROUND_SUBTLE),
                ])
                .gap(7.0)
                .flex(1.0),
                Button::new("card_action", "Abrir", component_palette())
                    .render()
                    .centered_y(),
            ])
            .gap(10.0)
        };
        type Finish = fn(Element) -> Element;
        let own: Finish = |card| card;
        let cases: [(&str, f32, Color, bool, f32, Option<f32>, Card, Finish); 11] = [
            ("session card", 10.0, CARD, true, 8.0, None, Card::new(content(), card_palette()), own),
            (
                "user message",
                10.0,
                CARD,
                false,
                8.0,
                None,
                Card::new(content(), card_palette()).border(false),
                own,
            ),
            (
                "automation session card",
                10.0,
                CARD,
                true,
                8.0,
                Some(300.0),
                Card::new(content(), card_palette()).width(300.0),
                own,
            ),
            (
                "hook card",
                12.0,
                PANEL,
                true,
                8.0,
                None,
                Card::new(content(), panel_card_palette()).density(CardDensity::Roomy),
                own,
            ),
            (
                "process output",
                12.0,
                PANEL,
                true,
                9.0,
                None,
                Card::new(content(), panel_card_palette()).density(CardDensity::Roomy),
                |card| card.radius(9.0),
            ),
            (
                "transcript notice",
                12.0,
                PANEL,
                true,
                10.0,
                None,
                Card::new(content(), panel_card_palette()).density(CardDensity::Roomy),
                |card| card.radius(10.0),
            ),
            (
                "marketplace item",
                12.0,
                PANEL,
                true,
                8.0,
                Some(280.0),
                Card::new(content(), panel_card_palette())
                    .density(CardDensity::Roomy)
                    .width(280.0),
                own,
            ),
            (
                "loading and comparison cards",
                12.0,
                PANEL,
                false,
                8.0,
                None,
                Card::new(content(), panel_card_palette())
                    .density(CardDensity::Roomy)
                    .border(false),
                own,
            ),
            (
                "automation card",
                10.0,
                BACKGROUND,
                true,
                10.0,
                Some(320.0),
                Card::new(content(), canvas_card_palette()).width(320.0),
                |card| card.radius(10.0),
            ),
            (
                "run card",
                10.0,
                BACKGROUND,
                true,
                9.0,
                None,
                Card::new(content(), canvas_card_palette()),
                |card| card.radius(9.0),
            ),
            (
                "approval target",
                10.0,
                PANEL,
                false,
                6.0,
                None,
                Card::new(content(), panel_card_palette())
                    .border(false)
                    .radius(RadiusRole::Surface),
                own,
            ),
        ];
        for (name, padding, fill, border, radius, width, card, finish) in cases {
            let current = finish(card.render());
            let shell = |element: Element| {
                let element = element.padding(padding).background(fill).radius(radius);
                let element = if border {
                    element.border(BORDER, 1.0)
                } else {
                    element
                };
                match width {
                    Some(width) => element.size(Some(width), None),
                    None => element,
                }
            };
            // Written both as a padded panel and as a padded row or column.
            for legacy in [shell(Element::panel(content())), shell(content())] {
                crate::icon_button_parity_tests::assert_same(name, legacy, current.clone());
            }
        }
        // A source row keeps a minimum height over its centered content, so
        // the content here is one line, shorter than the 32 px left inside.
        let line = || {
            Element::row(vec![
                Element::icon(Icon::Folder)
                    .foreground(FOREGROUND_SUBTLE)
                    .centered_y(),
                Element::truncated_text("openai/skills")
                    .foreground(FOREGROUND)
                    .flex(1.0),
                IconButton::new(
                    "source_action",
                    Icon::Trash,
                    action_palette(PANEL, FOREGROUND_SUBTLE),
                )
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .render()
                .centered_y(),
            ])
            .gap(10.0)
        };
        let source = |element: Element| element.radius(10.0).min_size(0.0, 56.0);
        let current = source(
            Card::new(line(), panel_card_palette())
                .density(CardDensity::Roomy)
                .render(),
        );
        let (frames, _) = crate::icon_button_parity_tests::states(current.clone(), 360.0);
        assert!(
            frames[0].commands.iter().any(|command| matches!(
                command,
                DrawCommand::FillRect { rect, color, .. } if *color == PANEL && rect.height == 56.0
            )),
            "the minimum height must be what sizes the row"
        );
        crate::icon_button_parity_tests::assert_same(
            "marketplace source row",
            source(line().padding(12.0).background(PANEL).border(BORDER, 1.0)),
            current,
        );
    }

    #[test]
    fn hover_progresses_from_each_surface_without_changing_geometry() {
        assert!(hover_for(SIDEBAR).r > SIDEBAR.r);
        assert!(hover_for(BACKGROUND).r > BACKGROUND.r);
        assert!(hover_for(SIDEBAR).r < SELECTED.r);
        assert_eq!(hover_for(BRAND), BRAND);
    }

    #[test]
    fn menu_palette_uses_one_composer_surface_without_changing_regular_controls() {
        let menu = menu_palette();
        assert_eq!(menu.surface, MENU_SURFACE);
        assert_eq!(menu.raised, MENU_SURFACE);
        assert_eq!(menu.hover, MENU_HOVER);
        assert_ne!(component_palette().surface, menu.surface);
        assert_ne!(component_palette().raised, menu.raised);
    }

    #[test]
    fn native_tooltip_style_is_independent_of_action_surface_and_ink() {
        let base = tooltip_style(
            Tooltip::new(
                Element::button("base", "Ayuda"),
                "base",
                "Ayuda",
                component_palette(),
            )
            .render(),
        );
        for (surface, ink) in [
            (SIDEBAR, FOREGROUND),
            (PANEL, FOREGROUND_SUBTLE),
            (BRAND, PRIMARY_FOREGROUND),
            (DESTRUCTIVE, BRAND),
        ] {
            let action = Button::new("action", "Ayuda", action_palette(surface, ink))
                .tooltip("Ayuda")
                .render();
            assert_eq!(tooltip_style(action), base);
        }
        assert!(base.background.r < PANEL.r);
        assert!(base.border.r > BORDER.r);
    }
}

/// Sans text at a size off limn's `TextRole` scale (10.5, 11.5, 12.5, 15, 17,
/// 19 and the display sizes). Text on the scale uses `openide_ui::Text`.
// Its first callers arrive with the next sections of the UI separation.
#[allow(dead_code)]
pub fn text_px(value: impl Into<String>, px: f32, ink: Color) -> Element {
    Element::text(value)
        .font_family(openide_ui::FontFamily::Sans)
        .font_size(px)
        .foreground(ink)
}

/// `SkeletonGroup::rows` as it was before limn absorbed it; kept for the
/// parity test until the final audit.
#[cfg(test)]
fn legacy_skeleton_rows(id: &str, rows: usize, row_height: f32, icon: bool) -> Vec<Element> {
    use openide_ui::{Component, Skeleton};
    (0..rows)
        .map(|row| {
            let fraction = [0.46, 0.62, 0.38, 0.54][row % 4];
            let mut cells = Vec::new();
            if icon {
                cells.push(
                    Skeleton::new(format!("{id}:{row}:icon"), component_palette())
                        .width(16.0)
                        .height(16.0)
                        .radius(4.0)
                        .render(),
                );
            }
            cells.push(
                Skeleton::new(format!("{id}:{row}"), component_palette())
                    .height(10.0)
                    .radius(5.0)
                    .render()
                    .flex(fraction),
            );
            cells.push(Element::spacer().flex(1.0 - fraction));
            Element::row(cells)
                .gap(10.0)
                .centered_y()
                .size(None, Some(row_height))
        })
        .collect()
}

/// `Icon::filled` as it was before limn absorbed it; kept for the parity
/// test until the final audit.
#[cfg(test)]
fn legacy_filled_icon(icon: openide_ui::Icon) -> openide_ui::Icon {
    use openide_ui::Icon;
    match icon {
        Icon::PanelLeft => Icon::PanelLeftOpen,
        Icon::PanelRight => Icon::PanelRightOpen,
        Icon::Bell => Icon::BellFill,
        Icon::Folder => Icon::FolderFill,
        Icon::Play => Icon::PlayFill,
        Icon::Grid => Icon::GridFill,
        Icon::Settings => Icon::SettingsFill,
        Icon::Palette => Icon::PaletteFill,
        Icon::Laptop => Icon::LaptopFill,
        Icon::Terminal => Icon::TerminalFill,
        Icon::Keyboard => Icon::KeyboardFill,
        Icon::Chat => Icon::ChatFill,
        Icon::Users => Icon::UsersFill,
        Icon::Puzzle => Icon::PuzzleFill,
        Icon::Server => Icon::ServerFill,
        Icon::Book => Icon::BookFill,
        Icon::Clock => Icon::ClockFill,
        Icon::Bolt => Icon::BoltFill,
        Icon::Archive => Icon::ArchiveFill,
        Icon::ChartBar => Icon::ChartBarFill,
        other => other,
    }
}
