//! Controlled, compact tool activity with a persistent disclosure identity.
//! The host supplies observed labels and outcomes; this view never interprets
//! an argument preview as execution or a process launch as process completion.

use crate::{
    Card, CardDensity, Color, Component, ComponentPalette, Element, FontFamily, Icon, Separator,
    Text, TextRole,
};

const HEADER_HEIGHT: f32 = 28.0;
const HEADER_PADDING: f32 = 6.0;
const HEADER_GAP: f32 = 6.0;
const ICON_SIDE: f32 = 14.0;
const TEXT_INSET: f32 = HEADER_PADDING + ICON_SIDE + HEADER_GAP;

pub struct ToolActivity {
    id: String,
    title: String,
    target: String,
    icon: Icon,
    content: Element,
    palette: ComponentPalette,
    expanded: bool,
    running: bool,
    command: bool,
    terminal: bool,
    status: Option<String>,
    header_action: Option<Element>,
    tooltip: Option<String>,
    note: Option<String>,
    ink: Option<Color>,
}

impl ToolActivity {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        content: Element,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            target: String::new(),
            icon: Icon::Code,
            content,
            palette,
            expanded: false,
            running: false,
            command: false,
            terminal: false,
            status: None,
            header_action: None,
            tooltip: None,
            note: None,
            ink: None,
        }
    }

    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = target.into();
        self
    }
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = icon;
        self
    }
    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }
    pub fn running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }
    pub fn command(mut self, command: bool) -> Self {
        self.command = command;
        self
    }
    /// A terminal keeps the host's bounded output preview visible. Expansion
    /// changes the output viewport rather than indenting a generic tool body.
    pub fn terminal(mut self, terminal: bool) -> Self {
        self.terminal = terminal;
        self
    }
    /// A short observed state, for example a pending permission or exit error.
    /// Successful completion does not need a second label next to the command.
    pub fn status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }
    /// A sibling of the terminal disclosure, so opening its inspector never
    /// also toggles output or leaves two overlapping hover surfaces.
    pub fn header_action(mut self, action: Element) -> Self {
        self.header_action = Some(action);
        self
    }
    pub fn tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
    /// An error/permission/uncertainty note remains visible with the body closed.
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// A status color from the host, such as its failure ink, for the icon,
    /// the secondary text, the chevron and the footer in place of the
    /// palette's muted.
    pub fn ink(mut self, ink: Color) -> Self {
        self.ink = Some(ink);
        self
    }

    fn terminal_element(self) -> Element {
        let centered = |element: Element| {
            Element::column(vec![
                Element::spacer().flex(1.0),
                element,
                Element::spacer().flex(1.0),
            ])
        };
        let mut header = vec![
            centered(
                Element::icon(self.icon)
                    .foreground(self.palette.muted)
                    .size(Some(ICON_SIDE), Some(ICON_SIDE)),
            )
            .size(Some(ICON_SIDE), None),
            centered(
                Text::new(self.title, self.palette)
                    .mono()
                    .truncate()
                    .render(),
            )
            .flex(1.0),
        ];
        if let Some(status) = self.status.filter(|status| !status.is_empty()) {
            header.push(centered(
                Text::new(status, self.palette)
                    .role(TextRole::Small)
                    .muted()
                    .truncate()
                    .render(),
            ));
        }
        if self.running {
            header.push(
                centered(
                    crate::Spinner::new(format!("{}:running", self.id), self.palette.muted)
                        .size(ICON_SIDE)
                        .render(),
                )
                .size(Some(ICON_SIDE), None),
            );
        }
        let header = self
            .palette
            .interactive_surface(Element::clickable(
                &self.id,
                Element::row(header).gap(HEADER_GAP).clip_children(),
            ))
            .padding(HEADER_PADDING)
            .size(None, Some(HEADER_HEIGHT));
        let header = if let Some(tooltip) = self.tooltip {
            crate::Tooltip::new(header, &self.id, tooltip, self.palette)
                .placement(crate::TooltipPlacement::Above)
                .render()
        } else {
            header
        };
        let header = if let Some(action) = self.header_action {
            Element::row(vec![
                header.flex(1.0),
                centered(action).size(Some(24.0), None),
            ])
            .gap(2.0)
            .size(None, Some(HEADER_HEIGHT))
        } else {
            header
        };
        let mut rows = vec![header];
        if let Some(note) = self.note.filter(|note| !note.is_empty()) {
            rows.push(
                Text::new(note, self.palette)
                    .role(TextRole::Small)
                    .muted()
                    .render()
                    .padding(8.0),
            );
        }
        rows.push(self.content);
        if self.expanded {
            rows.push(crate::collapse_footer(&self.id, self.palette));
        }
        // The header hovers across the whole card, so the rows bring their
        // own inset and the palette's surface is the card's fill.
        Card::new(
            Element::column(rows).clip_children(),
            ComponentPalette {
                raised: self.palette.surface,
                ..self.palette
            },
        )
        .density(CardDensity::Flush)
        .render()
    }
}

impl Component for ToolActivity {
    fn render(mut self) -> Element {
        if let Some(ink) = self.ink {
            self.palette.muted = ink;
        }
        if self.terminal {
            return self.terminal_element();
        }
        let icon = |icon| {
            Element::column(vec![
                Element::spacer().flex(1.0),
                Element::icon(icon)
                    .foreground(self.palette.muted)
                    .size(Some(ICON_SIDE), Some(ICON_SIDE)),
                Element::spacer().flex(1.0),
            ])
            .size(Some(ICON_SIDE), None)
        };
        let title = if self.running {
            Element::shimmer_truncated_text(
                format!("{}:label", self.id),
                self.title,
                crate::thinking_highlight(self.palette),
            )
        } else {
            Element::truncated_text(self.title)
        }
        .font_family(FontFamily::Sans)
        .font_size(13.0)
        .foreground(self.palette.muted)
        .flex(1.0);
        let mut header = vec![icon(self.icon), title];
        if !self.target.is_empty() {
            let target = Text::new(self.target, self.palette).truncate();
            header.push(
                if self.command { target.mono() } else { target }
                    .render()
                    .flex(2.0),
            );
        }
        header.push(icon(if self.expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        }));
        let header = self
            .palette
            .interactive_surface(Element::clickable(
                &self.id,
                Element::row(header).gap(HEADER_GAP).clip_children(),
            ))
            .padding(HEADER_PADDING)
            .size(None, Some(HEADER_HEIGHT));
        let mut rows = vec![header];
        if let Some(note) = self.note.filter(|note| !note.is_empty()) {
            rows.push(
                Element::row(vec![
                    Element::spacer().size(Some(TEXT_INSET), None),
                    Text::new(note, self.palette)
                        .role(TextRole::Small)
                        .muted()
                        .render()
                        .flex(1.0),
                    Element::spacer().size(Some(HEADER_PADDING), None),
                ])
                .clip_children(),
            );
        }
        if self.expanded {
            rows.push(Element::row(vec![
                Element::spacer().size(Some(HEADER_PADDING + ICON_SIDE / 2.0), None),
                Separator::vertical(self.palette).render(),
                Element::spacer().size(Some(ICON_SIDE / 2.0 + HEADER_GAP - 1.0), None),
                self.content.flex(1.0),
            ]));
            rows.push(crate::collapse_footer(&self.id, self.palette));
        }
        Element::column(rows).gap(4.0)
    }
}

// The bodies before the shells moved to limn, kept for the parity test.
#[cfg(test)]
impl ToolActivity {
    fn legacy_terminal_element(self) -> Element {
        let centered = |element: Element| {
            Element::column(vec![
                Element::spacer().flex(1.0),
                element,
                Element::spacer().flex(1.0),
            ])
        };
        let mut header = vec![
            centered(
                Element::icon(self.icon)
                    .foreground(self.palette.muted)
                    .size(Some(ICON_SIDE), Some(ICON_SIDE)),
            )
            .size(Some(ICON_SIDE), None),
            centered(
                Element::truncated_text(self.title)
                    .font_family(FontFamily::Monospace)
                    .font_size(12.0)
                    .foreground(self.palette.text),
            )
            .flex(1.0),
        ];
        if let Some(status) = self.status.filter(|status| !status.is_empty()) {
            header.push(centered(
                Element::truncated_text(status)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0)
                    .foreground(self.palette.muted),
            ));
        }
        if self.running {
            header.push(
                centered(
                    crate::Spinner::new(format!("{}:running", self.id), self.palette.muted)
                        .size(ICON_SIDE)
                        .render(),
                )
                .size(Some(ICON_SIDE), None),
            );
        }
        let header = self
            .palette
            .interactive_surface(Element::clickable(
                &self.id,
                Element::row(header).gap(HEADER_GAP).clip_children(),
            ))
            .padding(HEADER_PADDING)
            .size(None, Some(HEADER_HEIGHT));
        let header = if let Some(tooltip) = self.tooltip {
            crate::Tooltip::new(header, &self.id, tooltip, self.palette)
                .placement(crate::TooltipPlacement::Above)
                .render()
        } else {
            header
        };
        let header = if let Some(action) = self.header_action {
            Element::row(vec![
                header.flex(1.0),
                centered(action).size(Some(24.0), None),
            ])
            .gap(2.0)
            .size(None, Some(HEADER_HEIGHT))
        } else {
            header
        };
        let mut rows = vec![header];
        if let Some(note) = self.note.filter(|note| !note.is_empty()) {
            rows.push(
                Element::text(note)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0)
                    .foreground(self.palette.muted)
                    .padding(8.0),
            );
        }
        rows.push(self.content);
        if self.expanded {
            rows.push(crate::collapse_footer(&self.id, self.palette));
        }
        Element::column(rows)
            .background(self.palette.surface)
            .border(self.palette.border, 1.0)
            .radius(8.0)
            .clip_children()
    }

    fn legacy_render(self) -> Element {
        if self.terminal {
            return self.legacy_terminal_element();
        }
        let icon = |icon| {
            Element::column(vec![
                Element::spacer().flex(1.0),
                Element::icon(icon)
                    .foreground(self.palette.muted)
                    .size(Some(ICON_SIDE), Some(ICON_SIDE)),
                Element::spacer().flex(1.0),
            ])
            .size(Some(ICON_SIDE), None)
        };
        let title = if self.running {
            Element::shimmer_truncated_text(
                format!("{}:label", self.id),
                self.title,
                crate::thinking_highlight(self.palette),
            )
        } else {
            Element::truncated_text(self.title)
        }
        .font_family(FontFamily::Sans)
        .font_size(13.0)
        .foreground(self.palette.muted)
        .flex(1.0);
        let mut header = vec![icon(self.icon), title];
        if !self.target.is_empty() {
            header.push(
                Element::truncated_text(self.target)
                    .font_family(if self.command {
                        FontFamily::Monospace
                    } else {
                        FontFamily::Sans
                    })
                    .font_size(12.0)
                    .foreground(self.palette.text)
                    .flex(2.0),
            );
        }
        header.push(icon(if self.expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        }));
        let header = self
            .palette
            .interactive_surface(Element::clickable(
                &self.id,
                Element::row(header).gap(HEADER_GAP).clip_children(),
            ))
            .padding(HEADER_PADDING)
            .size(None, Some(HEADER_HEIGHT));
        let mut rows = vec![header];
        if let Some(note) = self.note.filter(|note| !note.is_empty()) {
            rows.push(
                Element::row(vec![
                    Element::spacer().size(Some(TEXT_INSET), None),
                    Element::text(note)
                        .font_family(FontFamily::Sans)
                        .font_size(11.0)
                        .foreground(self.palette.muted)
                        .flex(1.0),
                    Element::spacer().size(Some(HEADER_PADDING), None),
                ])
                .clip_children(),
            );
        }
        if self.expanded {
            rows.push(Element::row(vec![
                Element::spacer().size(Some(HEADER_PADDING + ICON_SIDE / 2.0), None),
                Element::spacer()
                    .size(Some(1.0), None)
                    .background(self.palette.border),
                Element::spacer().size(Some(ICON_SIDE / 2.0 + HEADER_GAP - 1.0), None),
                self.content.flex(1.0),
            ]));
            rows.push(crate::collapse_footer(&self.id, self.palette));
        }
        Element::column(rows).gap(4.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DrawCommand, Key, Size, UiAction, UiEvent, UiTree};
    use std::time::{Duration, Instant};

    #[test]
    fn the_shells_on_limn_match_the_bodies_they_replaced() {
        let ink = Color::rgb(255, 92, 92);
        let build = |terminal: bool, expanded: bool, running: bool, status_ink: bool| {
            let mut palette = ComponentPalette::neutral_dark();
            palette.surface = Color::rgb(25, 25, 25);
            let mut old_palette = palette;
            if status_ink {
                old_palette.muted = ink;
            }
            let make = |palette| {
                let mut activity = ToolActivity::new(
                    "tool:call",
                    "cargo test --workspace",
                    Element::text("salida de la herramienta").padding(8.0),
                    palette,
                )
                .terminal(terminal)
                .icon(Icon::Terminal)
                .target("crates/openide-ui")
                .command(running)
                .expanded(expanded)
                .running(running)
                .note("Permiso pendiente")
                .tooltip("cargo test --workspace");
                if !running {
                    activity = activity.status("Error").header_action(
                        crate::IconButton::new("inspect", Icon::More, palette)
                            .size(crate::ControlSize::Dense)
                            .render(),
                    );
                }
                activity
            };
            let new = make(palette);
            let new = if status_ink { new.ink(ink) } else { new };
            (make(old_palette).legacy_render(), new.render())
        };
        for width in [240.0, 720.0] {
            for case in 0..16 {
                let (old, new) = build(case & 1 != 0, case & 2 != 0, case & 4 != 0, case & 8 != 0);
                let frame = |element| {
                    let mut tree = UiTree::new(Element::column(vec![element]));
                    tree.set_reduced_motion(true);
                    tree.layout(Size::new(width, 240.0))
                };
                let (old, new) = (frame(old), frame(new));
                assert_eq!(old.commands, new.commands, "case {case} at {width}");
                assert_eq!(
                    format!("{:?}", old.regions),
                    format!("{:?}", new.regions),
                    "case {case} at {width}"
                );
            }
        }
    }

    #[test]
    fn terminal_header_keeps_output_visible_and_inspector_separate_from_disclosure() {
        for width in [180.0, 360.0, 900.0] {
            let make = |expanded| {
                ToolActivity::new(
                    "terminal:call",
                    "cargo test --workspace",
                    Element::text("output stays visible"),
                    ComponentPalette::neutral_dark(),
                )
                .terminal(true)
                .tooltip("complete command details")
                .expanded(expanded)
                .running(true)
                .icon(Icon::Terminal)
                .header_action(
                    crate::IconButton::new("inspect", Icon::More, ComponentPalette::neutral_dark())
                        .size(crate::ControlSize::Dense)
                        .render(),
                )
                .render()
            };
            let mut tree = UiTree::new(Element::column(vec![make(false)]));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 180.0));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "terminal:call:collapse"));
            assert!(frame
                .text_layouts()
                .iter()
                .any(|layout| layout.source == "output stays visible"));
            assert!(frame
                .commands
                .iter()
                .any(|command| matches!(command, DrawCommand::Spinner { .. })));
            assert!(!frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::Icon {
                    icon: Icon::ChevronRight | Icon::ChevronDown,
                    ..
                }
            )));
            let header = frame
                .regions
                .iter()
                .find(|region| region.id == "terminal:call")
                .unwrap()
                .rect;
            let inspect = frame
                .regions
                .iter()
                .find(|region| region.id == "inspect")
                .unwrap()
                .rect;
            assert_eq!(header.height, HEADER_HEIGHT);
            assert!(header.x + header.width <= inspect.x);
            assert!(inspect.x + inspect.width <= width);
            assert!((inspect.y + inspect.height / 2.0 - HEADER_HEIGHT / 2.0).abs() < 0.01);
            let point = crate::Point::new(
                inspect.x + inspect.width / 2.0,
                inspect.y + inspect.height / 2.0,
            );
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked {
                    id: "inspect".into()
                }]
            );
            assert!(tree.focus("terminal:call"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "terminal:call".into()
                }]
            );
            tree.set_root(Element::column(vec![make(true)]));
            let expanded = tree.layout(Size::new(width, 180.0));
            assert_eq!(
                expanded
                    .regions
                    .iter()
                    .find(|region| region.id == "terminal:call")
                    .unwrap()
                    .rect,
                header
            );
            assert_eq!(tree.focused_id(), Some("terminal:call"));
            let footer = expanded
                .regions
                .iter()
                .find(|region| region.id == "terminal:call:collapse")
                .expect("expanded terminal shares the icon-only footer")
                .rect;
            assert_eq!(footer.width, 24.0);
            assert_eq!(footer.height, 24.0);
            assert!(footer.y >= header.y + header.height);
            assert!(tree.focus("terminal:call:collapse"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false,
                }),
                vec![UiAction::Clicked {
                    id: "terminal:call".into()
                }]
            );
            tree.handle_event(UiEvent::PointerMove(crate::Point::new(
                header.x + 30.0,
                header.y + header.height + 8.0,
            )));
            let now = Instant::now();
            tree.layout_at(Size::new(width, 180.0), now);
            let body_hover = tree.layout_at(Size::new(width, 180.0), now + Duration::from_secs(1));
            assert!(
                !body_hover.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == "complete command details"
                )),
                "only the terminal header owns its tooltip"
            );
        }
    }

    #[test]
    fn disclosure_preserves_identity_keyboard_access_and_explicit_body_visibility() {
        let make = |expanded| {
            ToolActivity::new(
                "tool:call-1",
                "Ejecutando",
                Element::text("actual tool output"),
                ComponentPalette::neutral_dark(),
            )
            .target("cargo test --workspace")
            .icon(Icon::Terminal)
            .command(true)
            .running(true)
            .expanded(expanded)
            .note("Permiso pendiente")
            .render()
        };
        for width in [180.0, 240.0, 360.0, 900.0] {
            let start = Instant::now();
            let mut tree = UiTree::new(make(false));
            tree.set_text_measurer(|text, _, _| text.chars().count() as f32 * 7.0);
            let frame = tree.layout_at(Size::new(width, 240.0), start);
            assert!(!frame
                .text_layouts()
                .iter()
                .any(|layout| layout.source == "actual tool output"));
            assert!(frame
                .text_layouts()
                .iter()
                .any(|layout| layout.source == "Permiso pendiente"));
            let row = frame
                .regions
                .iter()
                .find(|region| region.id == "tool:call-1")
                .unwrap();
            assert_eq!(row.rect.width, width);
            assert_eq!(row.rect.height, HEADER_HEIGHT);
            for command in &frame.commands {
                if let DrawCommand::Icon { rect, .. } = command {
                    assert_eq!((rect.width, rect.height), (ICON_SIDE, ICON_SIDE));
                    assert_eq!(rect.y + rect.height / 2.0, HEADER_HEIGHT / 2.0);
                    assert!(rect.x >= HEADER_PADDING);
                    assert!(rect.x + rect.width <= width - HEADER_PADDING);
                }
            }
            assert!(tree.focus("tool:call-1"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "tool:call-1".into()
                }]
            );
            tree.set_root(make(true));
            let opened =
                tree.layout_at(Size::new(width, 240.0), start + Duration::from_millis(600));
            assert!(opened
                .text_layouts()
                .iter()
                .any(|layout| layout.source == "actual tool output"));
            assert_eq!(tree.focused_id(), Some("tool:call-1"));
            assert!(opened.commands.iter().any(|command| matches!(
                command,
                DrawCommand::Icon {
                    icon: Icon::Terminal,
                    ..
                }
            )));
            tree.set_reduced_motion(true);
            let static_frame =
                tree.layout_at(Size::new(width, 240.0), start + Duration::from_secs(1));
            let later = tree.layout_at(Size::new(width, 240.0), start + Duration::from_secs(2));
            assert_eq!(static_frame.commands, later.commands);
            assert!(!tree.is_animating());
        }
    }

    #[test]
    fn a_closed_tool_keeps_its_entire_wrapped_outcome_note_visible() {
        let note = "La operación se interrumpió mientras esperaba el permiso. El contenido recibido sigue disponible, pero todavía no hay un resultado que confirme su ejecución.";
        for width in [240.0, 360.0, 900.0] {
            let mut tree = UiTree::new(
                ToolActivity::new(
                    "tool:interrupted",
                    "Resultado sin confirmar",
                    Element::text("collapsed output"),
                    ComponentPalette::neutral_dark(),
                )
                .target("some/very/long/project/path/that/stays/inside/the/header")
                .note(note)
                .render(),
            );
            let frame = tree.layout(Size::new(width, 300.0));
            let layouts = frame.text_layouts();
            let note = layouts.iter().find(|layout| layout.source == note).unwrap();
            assert!(!note.runs.is_empty());
            assert!(note.runs.last().unwrap().text.ends_with("ejecución."));
            for run in &note.runs {
                assert!(run.rect.x >= TEXT_INSET);
                assert!(run.rect.x + run.rect.width <= width - HEADER_PADDING + 0.1);
                assert!(run.rect.y >= HEADER_HEIGHT);
                let visible = run.visible.expect("outcome note must remain visible");
                for (actual, expected) in [
                    (visible.x, run.rect.x),
                    (visible.y, run.rect.y),
                    (visible.width, run.rect.width),
                    (visible.height, run.rect.height),
                ] {
                    assert!((actual - expected).abs() < 0.001);
                }
            }
            assert!(!layouts
                .iter()
                .any(|layout| layout.source == "collapsed output"));
        }
    }
}
