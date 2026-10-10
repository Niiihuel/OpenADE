//! A single terminal surface inside the shared dock tab strip. The shell
//! owns input and history; the dock supplies focus and selection, rather
//! than placing a second command editor below the PTY.

use openide_ui::{
    Breadcrumb, BreadcrumbItem, Button, ButtonSize, ButtonVariant, Color, Component, ContextMenu,
    ContextMenuEntry, ContextMenuItem, ControlSize, Element, FontFamily, Icon, IconButton,
    Separator, Tooltip, TooltipPlacement,
};

use crate::{
    terminal::{
        TerminalColor, TerminalSnapshot, FONT_SIZE, LINE_HEIGHT, SCREEN_PADDING_X, SCREEN_PADDING_Y,
    },
    theme,
    view::ViewState,
};

fn text(value: impl Into<String>, color: Color) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(11.0)
        .foreground(color)
}

fn tool(id: &str, icon: Icon, hint: &str, shortcut: &str) -> Element {
    Tooltip::new(
        theme::interactive_surface(
            IconButton::new(id, icon, theme::component_palette())
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::IconSmall)
                .render(),
        ),
        id,
        hint,
        theme::component_palette(),
    )
    .shortcut(shortcut)
    .placement(TooltipPlacement::Below)
    .render()
}

fn terminal_color(color: TerminalColor, default: Color) -> Color {
    match color {
        TerminalColor::Default => default,
        TerminalColor::Rgb(red, green, blue) => Color::rgb(red, green, blue),
        TerminalColor::Indexed(index) if index < 16 => {
            const ANSI: [(u8, u8, u8); 16] = [
                (38, 38, 38),
                (205, 79, 79),
                (126, 190, 137),
                (207, 183, 118),
                (115, 157, 208),
                (188, 142, 198),
                (112, 185, 190),
                (214, 214, 214),
                (100, 100, 100),
                (238, 108, 108),
                (152, 216, 163),
                (229, 208, 135),
                (147, 185, 230),
                (208, 168, 218),
                (144, 207, 211),
                (245, 245, 245),
            ];
            let (red, green, blue) = ANSI[index as usize];
            Color::rgb(red, green, blue)
        }
        TerminalColor::Indexed(index) if index < 232 => {
            let cube = index - 16;
            let channel = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            Color::rgb(
                channel(cube / 36),
                channel((cube / 6) % 6),
                channel(cube % 6),
            )
        }
        TerminalColor::Indexed(index) => {
            let gray = 8 + (index - 232) * 10;
            Color::rgb(gray, gray, gray)
        }
    }
}

fn terminal_row(snapshot: &TerminalSnapshot, index: usize, width: f32) -> Element {
    if snapshot.styled_lines.get(index).is_none_or(Vec::is_empty) {
        return Element::spacer().size(Some(width), Some(LINE_HEIGHT));
    }
    let spans = snapshot
        .styled_lines
        .get(index)
        .into_iter()
        .flatten()
        .map(|run| {
            let mut foreground = terminal_color(run.style.foreground, theme::FOREGROUND);
            let mut background = match run.style.background {
                TerminalColor::Default => None,
                color => Some(terminal_color(color, theme::BACKGROUND)),
            };
            if run.style.inverse {
                let swapped = background.unwrap_or(theme::BACKGROUND);
                background = Some(foreground);
                foreground = swapped;
            }
            if run.style.bold && run.style.foreground == TerminalColor::Default {
                foreground = theme::BRAND;
            }
            if run.style.dim {
                foreground = Color::rgb(
                    foreground.r.saturating_sub(45),
                    foreground.g.saturating_sub(45),
                    foreground.b.saturating_sub(45),
                );
            }
            if run.style.selected {
                background = Some(theme::TERMINAL_SELECTION);
                foreground = theme::BRAND;
            }
            // PTY rows are already laid out in cells. Keep each styled run on
            // that single line: paragraph wrapping during a viewport resize
            // would paint a stale wide snapshot over the following row.
            let span = Element::faded_text(&run.text, 0.0)
                .foreground(foreground)
                .font_size(FONT_SIZE)
                .font_family(FontFamily::Monospace)
                .size(None, Some(LINE_HEIGHT));
            if let Some(background) = background {
                span.background(background)
            } else {
                span
            }
        })
        .collect::<Vec<_>>();
    Element::row(spans)
        .gap(0.0)
        .size(Some(width), Some(LINE_HEIGHT))
        .clip_children()
}

/// PTY viewport without a second toolbar or permanent instructions. The host
/// owns panel placement/session selection; rendering never starts a process.
pub(crate) fn build_body(state: &ViewState, width: f32, height: f32) -> Element {
    let id = state
        .terminal_tabs
        .iter()
        .find(|tab| tab.active)
        .or_else(|| {
            state
                .dock_tabs
                .iter()
                .find(|tab| tab.active && tab.kind == crate::view::SideTab::Terminal)
        })
        .map_or(0, |tab| tab.id);
    build_pane(
        state.terminal_snapshot.as_ref(),
        state.terminal_error.as_deref(),
        id,
        "terminal_screen",
        width,
        height,
    )
}

pub(crate) fn build_pane(
    snapshot: Option<&TerminalSnapshot>,
    terminal_error: Option<&str>,
    id: u64,
    screen_id: &str,
    width: f32,
    height: f32,
) -> Element {
    let scoped = screen_id != "terminal_screen";
    let restart = if scoped {
        format!("terminal_restart:{id}")
    } else {
        "terminal_restart".into()
    };
    let bottom = if scoped {
        format!("terminal_bottom:{id}")
    } else {
        "terminal_bottom".into()
    };
    let width = width.max(0.0);
    let height = height.max(0.0);
    let Some(snapshot) = snapshot else {
        let mut content = vec![
            Element::spacer().flex(1.0),
            Element::row(vec![
                Element::icon(if terminal_error.is_some() {
                    Icon::WarningTriangle
                } else {
                    Icon::Terminal
                })
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(16.0), Some(24.0)),
                Element::truncated_text(terminal_error.unwrap_or("Iniciando terminal…"))
                    .font_family(FontFamily::Sans)
                    .font_size(12.0)
                    .foreground(theme::FOREGROUND_SUBTLE)
                    .flex(1.0),
            ])
            .gap(8.0)
            .size(None, Some(24.0)),
        ];
        if terminal_error.is_some() {
            content.push(theme::interactive_surface(
                Button::new(&restart, "Reintentar", theme::component_palette())
                    .variant(ButtonVariant::Ghost)
                    .render()
                    .size(Some(110.0), Some(32.0)),
            ));
        }
        content.push(Element::spacer().flex(1.0));
        return Element::column(content)
            .padding(12.0)
            .gap(8.0)
            .background(theme::BACKGROUND)
            .clip_children()
            .size(Some(width), Some(height));
    };
    let screen = terminal_screen(snapshot, width, Some(height), id, screen_id);
    let error = terminal_error.or(snapshot.error.as_deref());
    let notice = if error.is_some() || !snapshot.running {
        let status = error.map(str::to_owned).unwrap_or_else(|| {
            snapshot.exit_code.map_or_else(
                || "Sesión finalizada".to_owned(),
                |code| format!("Proceso finalizado · código {code}"),
            )
        });
        Some(
            Element::row(vec![
                Element::truncated_text(status)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0)
                    .foreground(if error.is_some() {
                        theme::DESTRUCTIVE
                    } else {
                        theme::FOREGROUND_SUBTLE
                    })
                    .flex(1.0),
                theme::interactive_surface(
                    Button::new(&restart, "Reiniciar", theme::component_palette())
                        .variant(ButtonVariant::Ghost)
                        .render()
                        .size(Some(88.0), Some(32.0)),
                ),
            ])
            .gap(8.0)
            .padding(4.0)
            .background(theme::PANEL)
            .radius(8.0)
            .size(None, Some(40.0)),
        )
    } else if snapshot.scrollback_position > 0 {
        Some(
            Element::row(vec![
                Element::spacer().flex(1.0),
                theme::interactive_surface(
                    Button::new(&bottom, "Volver al prompt", theme::component_palette())
                        .icon(Icon::ArrowDown)
                        .variant(ButtonVariant::Subtle)
                        .render()
                        .size(Some(164.0), Some(32.0)),
                ),
            ])
            .size(None, Some(32.0)),
        )
    } else {
        None
    };
    if let Some(notice) = notice {
        Element::overlay(
            screen,
            Element::column(vec![Element::spacer().flex(1.0), notice])
                .padding(8.0)
                .size(Some(width), Some(height)),
        )
        .clip_children()
        .size(Some(width), Some(height))
    } else {
        screen
    }
}

/// Compatibility for the former side-tab host. The bottom dock uses
/// `build_body` and supplies exactly one session/header row itself.
pub(crate) fn build(state: &ViewState, width: f32) -> Element {
    let Some(snapshot) = &state.terminal_snapshot else {
        return Element::column(vec![
            Element::spacer().flex(1.0),
            Element::icon(Icon::Terminal)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(24.0), Some(24.0)),
            text("Terminal local", theme::FOREGROUND).font_size(14.0),
            text(
                state
                    .terminal_error
                    .as_deref()
                    .unwrap_or("Iniciando el shell del proyecto…"),
                theme::FOREGROUND_SUBTLE,
            ),
            theme::interactive_surface(
                Button::new("terminal_restart", "Reintentar", theme::component_palette())
                    .variant(ButtonVariant::Subtle)
                    .render()
                    .size(Some(110.0), Some(32.0)),
            ),
            Element::spacer().flex(1.0),
        ])
        .padding(20.0)
        .gap(12.0)
        .flex(1.0);
    };
    build_snapshot(
        snapshot,
        state.terminal_error.as_deref(),
        &state.workspace,
        width,
        state
            .dock_tabs
            .iter()
            .find(|tab| tab.active && tab.kind == crate::view::SideTab::Terminal)
            .map_or(0, |tab| tab.id),
    )
}

fn terminal_screen(
    snapshot: &TerminalSnapshot,
    width: f32,
    height: Option<f32>,
    terminal_id: u64,
    screen_id: &str,
) -> Element {
    let visible_rows = height.map_or(snapshot.styled_lines.len(), |height| {
        ((height - SCREEN_PADDING_Y * 2.0).max(0.0) / LINE_HEIGHT).ceil() as usize
    });
    let rows = (0..snapshot.styled_lines.len().min(visible_rows))
        .map(|index| terminal_row(snapshot, index, (width - SCREEN_PADDING_X * 2.0).max(0.0)))
        .collect();
    let screen = Element::clickable(
        screen_id,
        Element::row(vec![
            Element::spacer().size(Some(SCREEN_PADDING_X - SCREEN_PADDING_Y), None),
            Element::column(rows).gap(0.0).clip_children().flex(1.0),
            Element::spacer().size(Some(SCREEN_PADDING_X - SCREEN_PADDING_Y), None),
        ])
        .gap(0.0)
        .padding(SCREEN_PADDING_Y)
        .background(theme::BACKGROUND)
        .clip_children()
        .flex(1.0),
    )
    .focus_ring(false)
    .clip_children()
    .flex(1.0);
    let context_id = format!("terminal_context:{terminal_id}");
    let screen = ContextMenu::new(
        &context_id,
        screen,
        vec![
            ContextMenuEntry::Item(
                ContextMenuItem::new(format!("{context_id}:copy_selection"), "Copiar selección")
                    .shortcut("Ctrl+Shift+C")
                    .enabled(
                        snapshot
                            .selection
                            .as_ref()
                            .is_some_and(|text| !text.is_empty()),
                    ),
            ),
            ContextMenuEntry::Item(ContextMenuItem::new(
                format!("{context_id}:copy_output"),
                "Copiar salida",
            )),
            ContextMenuEntry::Item(
                ContextMenuItem::new(format!("{context_id}:paste"), "Pegar")
                    .shortcut("Ctrl+Shift+V")
                    .enabled(snapshot.running),
            ),
            ContextMenuEntry::Separator,
            ContextMenuEntry::Item(
                ContextMenuItem::new(format!("{context_id}:clear"), "Limpiar terminal")
                    .shortcut("Ctrl+L")
                    .enabled(snapshot.running),
            ),
        ],
        theme::menu_palette(),
    )
    .render()
    .flex(1.0);

    if let Some(height) = height {
        screen.size(Some(width), Some(height)).flex(0.0)
    } else {
        screen
    }
}

fn build_snapshot(
    snapshot: &TerminalSnapshot,
    terminal_error: Option<&str>,
    workspace: &str,
    width: f32,
    terminal_id: u64,
) -> Element {
    let workspace_name = std::path::Path::new(workspace)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(workspace);
    let status_dot = Element::spacer()
        .background(if snapshot.running {
            theme::DIFF_ADDED
        } else {
            theme::FOREGROUND_SUBTLE
        })
        .radius(3.0)
        .size(Some(6.0), Some(6.0))
        .centered_y()
        .size(Some(8.0), Some(32.0));
    let header = Element::row(vec![
        Tooltip::new(
            status_dot,
            "terminal_shell_status",
            if snapshot.running {
                "Shell en ejecución"
            } else {
                "Shell finalizado"
            },
            theme::component_palette(),
        )
        .placement(TooltipPlacement::Below)
        .render(),
        Breadcrumb::new(
            "terminal_path",
            vec![
                BreadcrumbItem::new(&snapshot.shell).tooltip(format!("Shell: {}", snapshot.shell)),
                BreadcrumbItem::new(workspace_name).tooltip(workspace),
            ],
            theme::component_palette(),
        )
        .width((width - 152.0).max(0.0))
        .height(32.0)
        .render()
        .flex(1.0),
        Tooltip::new(
            theme::interactive_surface(
                Button::new("terminal_copy", "Copiar", theme::component_palette())
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Compact)
                    .render()
                    .font_size(11.0)
                    .size(Some(48.0), Some(32.0)),
            ),
            "terminal_copy",
            "Copiar selección o salida visible",
            theme::component_palette(),
        )
        .shortcut("Ctrl+Shift+C")
        .placement(TooltipPlacement::Below)
        .render(),
        tool("terminal_clear", Icon::Trash, "Limpiar pantalla", "Ctrl+L"),
        tool(
            "terminal_interrupt",
            Icon::Stop,
            "Interrumpir proceso",
            "Ctrl+C",
        ),
    ])
    .padding(4.0)
    .gap(6.0)
    .size(Some(width), Some(40.0));

    let screen = terminal_screen(snapshot, width, None, terminal_id, "terminal_screen");

    let error = terminal_error.or(snapshot.error.as_deref());
    let status = if let Some(error) = error {
        error.to_owned()
    } else if !snapshot.running {
        format!("Finalizó · código {}", snapshot.exit_code.unwrap_or(0))
    } else if snapshot.selection.is_some() {
        "Ctrl+Shift+C · copiar selección".to_owned()
    } else if snapshot.alternate_screen {
        "Aplicación interactiva".to_owned()
    } else {
        "Ctrl+Shift+V · pegar".to_owned()
    };
    let action = if !snapshot.running {
        theme::interactive_surface(
            Button::new("terminal_restart", "Reiniciar", theme::component_palette())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .render(),
        )
    } else if snapshot.scrollback_position > 0 {
        theme::interactive_surface(
            Button::new(
                "terminal_bottom",
                "Volver al prompt",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .render(),
        )
    } else {
        Element::spacer().size(Some(0.0), Some(0.0))
    };
    let footer = Element::row(vec![
        Element::truncated_text(status)
            .font_size(10.0)
            .foreground(if error.is_some() {
                theme::DESTRUCTIVE
            } else {
                theme::FOREGROUND_SUBTLE
            })
            .flex(1.0),
        action,
    ])
    .gap(8.0)
    .padding(4.0)
    .size(Some(width), Some(36.0));

    Element::column(vec![
        header,
        Separator::horizontal(theme::component_palette()).render(),
        screen,
        Separator::horizontal(theme::component_palette()).render(),
        footer,
    ])
    .background(theme::BACKGROUND)
    .size(Some(width), None)
    .flex(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{Size, UiTree};

    fn grid_snapshot(lines: &[&str]) -> TerminalSnapshot {
        TerminalSnapshot {
            lines: lines.iter().map(|line| (*line).into()).collect(),
            styled_lines: lines
                .iter()
                .map(|line| {
                    if line.is_empty() {
                        Vec::new()
                    } else {
                        vec![crate::terminal::TerminalSpan {
                            text: (*line).into(),
                            style: crate::terminal::TerminalStyle::default(),
                        }]
                    }
                })
                .collect(),
            cursor: (0, 0),
            cols: 80,
            rows: lines.len() as u16,
            scrollback_position: 0,
            running: true,
            exit_code: None,
            error: None,
            revision: 1,
            shell: "bash".into(),
            alternate_screen: false,
            selection: None,
        }
    }

    fn grid_text_position(frame: &openide_ui::Frame, value: &str) -> openide_ui::Point {
        frame
            .commands
            .iter()
            .find_map(|command| match command {
                openide_ui::DrawCommand::Text { text, position, .. } if text == value => {
                    Some(*position)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing terminal row: {value}"))
    }

    #[test]
    fn terminal_grid_uses_compact_vertical_padding_and_the_same_split_geometry() {
        use openide_ui::{DrawCommand, Point, Rect};

        let snapshot = grid_snapshot(&["first row", "second row"]);
        let height = 44.0;
        for width in [32.0, 260.0] {
            for screen_id in ["terminal_screen", "terminal_screen:42"] {
                let mut tree = UiTree::new(build_pane(
                    Some(&snapshot),
                    None,
                    42,
                    screen_id,
                    width,
                    height,
                ));
                let frame = tree.layout(Size::new(width, height));
                let screen = frame
                    .regions
                    .iter()
                    .find(|region| region.id == screen_id)
                    .unwrap();
                assert_eq!(screen.rect, Rect::new(0.0, 0.0, width, height));
                assert_eq!(frame.hit_test(Point::new(13.0, 5.0)), Some(screen_id));
                for y in [4.0, 22.0] {
                    assert!(
                        frame.commands.iter().any(|command| matches!(command,
                            DrawCommand::PushClip(rect)
                                if *rect == Rect::new(12.0, y, width - 24.0, 18.0)
                        )),
                        "grid row must retain its exact inset and cell height at {width}px: {y}"
                    );
                }
                let first = grid_text_position(&frame, "first row");
                let second = grid_text_position(&frame, "second row");
                assert_eq!(first.x, 12.0);
                assert_eq!(second.x, 12.0);
                assert!((second.y - first.y - 18.0).abs() < 0.01);
                assert!(
                    tree.focus(screen_id),
                    "the split viewport retains its own focus target"
                );
            }
        }
    }

    #[test]
    fn terminal_short_viewports_clip_grid_rows_without_compressing_them() {
        use openide_ui::{DrawCommand, Rect};

        let snapshot = grid_snapshot(&["first row", "second row", "third row"]);
        let mut tree = UiTree::new(build_pane(
            Some(&snapshot),
            None,
            7,
            "terminal_screen:7",
            120.0,
            62.0,
        ));
        let full = tree.layout(Size::new(120.0, 62.0));
        for (height, last_row_y, last_row_text) in
            [(35.0, 22.0, "second row"), (17.0, 4.0, "first row")]
        {
            tree.set_root(build_pane(
                Some(&snapshot),
                None,
                7,
                "terminal_screen:7",
                120.0,
                height,
            ));
            let frame = tree.layout(Size::new(120.0, height));
            assert_eq!(
                grid_text_position(&frame, "first row"),
                grid_text_position(&full, "first row")
            );
            assert_eq!(
                grid_text_position(&frame, last_row_text),
                grid_text_position(&full, last_row_text)
            );
            assert!(
                frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::PushClip(rect)
                        if *rect == Rect::new(12.0, last_row_y, 96.0, 9.0)
                )),
                "only the visible half of the last row is clipped"
            );
            assert!(
                !frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == "third row"
                )),
                "rows outside the viewport must not be painted"
            );
        }
    }

    #[test]
    fn compact_terminal_preserves_blank_lines_emitted_before_the_shell_prompt() {
        let mut tree = UiTree::new(build_pane(
            Some(&grid_snapshot(&["prompt", "output"])),
            None,
            1,
            "terminal_screen",
            240.0,
            62.0,
        ));
        let without_blank = tree.layout(Size::new(240.0, 62.0));
        tree.set_root(build_pane(
            Some(&grid_snapshot(&["", "prompt", "output"])),
            None,
            1,
            "terminal_screen",
            240.0,
            62.0,
        ));
        let with_blank = tree.layout(Size::new(240.0, 62.0));
        for value in ["prompt", "output"] {
            let before = grid_text_position(&without_blank, value);
            let after = grid_text_position(&with_blank, value);
            assert_eq!(before.x, after.x);
            assert!(
                (after.y - before.y - 18.0).abs() < 0.01,
                "reducing chrome padding must not trim shell output"
            );
        }
    }

    #[test]
    fn terminal_wide_snapshot_stays_on_its_rows_while_viewport_shrinks() {
        use crate::terminal::{TerminalSpan, TerminalStyle};
        use openide_ui::{DrawCommand, FontMetrics};
        let first = "cargo test -p openide-editor while the PTY resize is pending";
        let second = "Compiling";
        let mut snapshot = TerminalSnapshot {
            lines: vec![first.into(), second.into()],
            styled_lines: vec![
                vec![TerminalSpan {
                    text: first.into(),
                    style: TerminalStyle::default(),
                }],
                vec![TerminalSpan {
                    text: second.into(),
                    style: TerminalStyle::default(),
                }],
            ],
            cursor: (1, 0),
            cols: 88,
            rows: 2,
            scrollback_position: 0,
            running: true,
            exit_code: None,
            error: None,
            revision: 1,
            shell: "bash".into(),
            alternate_screen: false,
            selection: None,
        };
        for width in [72.0, 248.0, 620.0] {
            let mut tree = UiTree::new(
                Element::column(vec![
                    terminal_row(&snapshot, 0, width),
                    terminal_row(&snapshot, 1, width),
                ])
                .gap(0.0),
            );
            tree.set_metrics(FontMetrics {
                char_width: 8.4,
                line_height: 21.0,
            });
            let frame = tree.layout(Size::new(width, LINE_HEIGHT * 2.0));
            let runs = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text {
                        text,
                        position,
                        font_size,
                        ..
                    } => Some((text.as_str(), *position, *font_size)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                runs.len(),
                2,
                "PTY rows must never create wrapped fragments"
            );
            assert_eq!(runs[0].0, first);
            assert_eq!(runs[1].0, second);
            assert_eq!(runs[0].2, FONT_SIZE);
            assert_eq!(runs[1].2, FONT_SIZE);
            assert!((runs[1].1.y - runs[0].1.y - LINE_HEIGHT).abs() < 0.01);
            for y in [0.0, LINE_HEIGHT] {
                assert!(frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::PushClip(rect) if rect.x == 0.0 && rect.y == y && rect.width == width && rect.height == LINE_HEIGHT
                )), "each row clips paint to its own grid rectangle");
            }
        }
        snapshot.styled_lines[0] = vec![
            TerminalSpan {
                text: "abcd".into(),
                style: TerminalStyle {
                    background: TerminalColor::Indexed(1),
                    ..Default::default()
                },
            },
            TerminalSpan {
                text: "EF".into(),
                style: TerminalStyle::default(),
            },
        ];
        let mut tree = UiTree::new(terminal_row(&snapshot, 0, 100.0));
        tree.set_text_measurer(|text, size, _| text.chars().count() as f32 * size * 0.6);
        let frame = tree.layout(Size::new(100.0, LINE_HEIGHT));
        let x = |target: &str| {
            frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Text { text, position, .. } if text == target => Some(position.x),
                    _ => None,
                })
                .unwrap()
        };
        assert!(
            (x("EF") - x("abcd") - 4.0 * FONT_SIZE * 0.6).abs() < 0.01,
            "ANSI backgrounds must not add paragraph padding between cells"
        );
    }

    #[test]
    fn terminal_keeps_toolbar_and_history_action_visible_while_resizing() {
        let snapshot = TerminalSnapshot {
            lines: vec![String::new(); 60],
            styled_lines: vec![Vec::new(); 60],
            cursor: (0, 0),
            cols: 80,
            rows: 60,
            scrollback_position: 12,
            running: true,
            exit_code: None,
            error: None,
            revision: 0,
            shell: "bash".into(),
            alternate_screen: false,
            selection: None,
        };
        for (width, height) in [(320.0, 400.0), (480.0, 760.0)] {
            let mut tree = UiTree::new(build_snapshot(&snapshot, None, "openADE", width, 1));
            let frame = tree.layout(Size::new(width, height));
            for id in ["terminal_clear", "terminal_interrupt"] {
                let region = frame.regions.iter().find(|region| region.id == id).unwrap();
                assert_eq!(region.rect.width, 32.0);
                assert_eq!(region.rect.height, 32.0);
                assert_eq!(
                    frame.hit_test(openide_ui::Point::new(
                        region.rect.x + 16.0,
                        region.rect.y + 16.0,
                    )),
                    Some(id),
                );
            }
            for id in [
                "terminal_screen",
                "terminal_copy",
                "terminal_interrupt",
                "terminal_bottom",
            ] {
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap()
                    .rect;
                if matches!(id, "terminal_clear" | "terminal_interrupt") {
                    assert_eq!((rect.width, rect.height), (32.0, 32.0), "{id} hit target");
                }
                assert!(rect.x >= 0.0 && rect.y >= 0.0, "{id}: {rect:?}");
                assert!(rect.x + rect.width <= width + 1.0, "{id}: {rect:?}");
                assert!(rect.y + rect.height <= height + 1.0, "{id}: {rect:?}");
            }
            assert!(frame
                .regions
                .iter()
                .all(|region| region.id != "terminal_input"));
            let screen = frame
                .regions
                .iter()
                .find(|region| region.id == "terminal_screen")
                .unwrap()
                .rect;
            assert!(tree.focus("terminal_screen"));
            let focused = tree.layout(Size::new(width, height));
            assert_eq!(tree.focused_id(), Some("terminal_screen"));
            assert!(
                !focused.commands.iter().any(|command| matches!(command,
                    openide_ui::DrawCommand::StrokeRect { rect, .. } if *rect == screen
                )),
                "terminal focus must use its caret, not an outer rectangle"
            );
        }
    }
}
