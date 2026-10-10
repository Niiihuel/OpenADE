//! A retained file operation: identity, observed state and inline content.
//!
//! The host supplies snapshots and action IDs. This component does not read
//! files, calculate diffs, infer success, or treat streamed arguments as writes.

use crate::{
    Card, CardDensity, Color, Component, ComponentPalette, Element, FontFamily, Icon, Separator,
    Text, TextRole, Tooltip, TooltipPlacement,
};

const HEADER_HEIGHT: f32 = 28.0;
const COMPACT_HEADER_HEIGHT: f32 = 28.0;
const HEADER_INSET: f32 = 6.0;
const CONTROL_SIDE: f32 = 24.0;
const FILENAME_PADDING: f32 = 3.0;
const ICON_SIDE: f32 = 14.0;
/// The inks of the counts until the host passes its own with `diff_inks`.
const ADDED: Color = Color::rgb(105, 210, 145);
const REMOVED: Color = Color::rgb(240, 125, 130);

/// One controlled disclosure for one file operation. `id` is the toggle action
/// ID, not a path: preserve it across streaming revisions and distinguish calls
/// that happen to touch the same file. The host owns `expanded` and the body.
pub struct FileActivity {
    id: String,
    filename: String,
    path: String,
    status: String,
    content: Element,
    palette: ComponentPalette,
    expanded: bool,
    compact: bool,
    running: bool,
    stats: Option<(usize, usize)>,
    diff_inks: (Color, Color),
    body_inset: bool,
    actions: Vec<Element>,
    open_action: Option<(String, String)>,
    note: Option<String>,
    detail: Option<String>,
    icon: Icon,
}

impl FileActivity {
    /// The palette's surface is also the code surface. Supply a body without
    /// another file title or outer card so there is a single integrated frame.
    pub fn new(
        toggle_action_id: impl Into<String>,
        filename: impl Into<String>,
        content: Element,
        palette: ComponentPalette,
    ) -> Self {
        let filename = filename.into();
        let icon = crate::file_icon(&filename);
        Self {
            id: toggle_action_id.into(),
            filename,
            path: String::new(),
            status: String::new(),
            content,
            palette,
            expanded: false,
            compact: false,
            running: false,
            stats: None,
            diff_inks: (ADDED, REMOVED),
            body_inset: false,
            actions: Vec::new(),
            open_action: None,
            note: None,
            detail: None,
            icon,
        }
    }

    /// Parent directory retained in the filename tooltip without competing with its name.
    pub fn path(mut self, parent: impl Into<String>) -> Self {
        self.path = parent.into();
        self
    }

    /// An observed, localized state such as "Leyendo" or "Editado".
    pub fn status(mut self, label: impl Into<String>) -> Self {
        self.status = label.into();
        self
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    /// Reading uses a quiet, single-line row when collapsed, and one framed
    /// surface for its header and content when expanded. The full parent
    /// directory stays in the filename tooltip; the file has priority over status.
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    /// Animate only the state label. The icon is static; UiTree handles reduced
    /// motion, offscreen suspension and a stable phase across rebuilt trees.
    pub fn running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }

    /// Counts are supplied by the host, never inferred from a partial preview.
    pub fn stats(mut self, additions: usize, deletions: usize) -> Self {
        self.stats = Some((additions, deletions));
        self
    }

    /// The host's colors for the added and the removed count.
    pub fn diff_inks(mut self, added: Color, removed: Color) -> Self {
        self.diff_inks = (added, removed);
        self
    }

    /// Hold the body off the frame with [`activity_body_inset`], for content
    /// that brings no inset of its own.
    pub fn body_inset(mut self, inset: bool) -> Self {
        self.body_inset = inset;
        self
    }

    /// Compact 24px controls, usually one or two icon buttons. Their action IDs
    /// are independent of the disclosure and remain available when it is closed.
    pub fn actions(mut self, actions: Vec<Element>) -> Self {
        self.actions = actions;
        self
    }

    /// Give the filename an independent navigation action. It renders as an
    /// underlined-on-hover link; the rest of the header keeps the disclosure ID.
    /// Without this action the filename remains a second way to toggle details.
    pub fn open_action(mut self, action_id: impl Into<String>, hint: impl Into<String>) -> Self {
        self.open_action = Some((action_id.into(), hint.into()));
        self
    }

    /// Errors, pending permission and uncertain outcomes remain visible even
    /// when the preview is collapsed. Long notes wrap at the available width.
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Secondary evidence details belong in the disclosure tooltip. Errors and
    /// pending authorization must use `note` so they remain visible.
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = icon;
        self
    }
}

/// The inset of an activity's body: 8 above and beside it. The disclosure
/// footer supplies its own inset, so only 2 below.
pub fn activity_body_inset(content: Element) -> Element {
    Element::column(vec![
        Element::spacer().size(None, Some(8.0)),
        Element::row(vec![
            Element::spacer().size(Some(8.0), None),
            content.flex(1.0),
            Element::spacer().size(Some(8.0), None),
        ]),
        Element::spacer().size(None, Some(2.0)),
    ])
}

fn centered_icon(icon: Icon, color: Color) -> Element {
    Element::column(vec![
        Element::spacer().flex(1.0),
        Element::icon(icon)
            .foreground(color)
            .size(Some(ICON_SIDE), Some(ICON_SIDE)),
        Element::spacer().flex(1.0),
    ])
    .size(Some(ICON_SIDE), None)
}

fn count_label(count: usize) -> String {
    // Bound the chrome's width, while the tooltip retains the exact count.
    if count < 10_000 {
        count.to_string()
    } else if count < 1_000_000 {
        format!("{}k", count / 1_000)
    } else if count < 1_000_000_000 {
        format!("{}m", count / 1_000_000)
    } else {
        "999m+".into()
    }
}

fn status_label(id: &str, label: &str, running: bool, palette: ComponentPalette) -> Element {
    let label = if running && !label.is_empty() {
        Element::shimmer_truncated_text(
            format!("{id}:status"),
            label,
            crate::thinking_highlight(palette),
        )
    } else {
        Element::faded_text(label, 16.0)
    };
    label
        .font_family(FontFamily::Sans)
        .font_size(11.0)
        .foreground(palette.muted)
        .size(None, Some(16.0))
        .flex(1.0)
}

impl Component for FileActivity {
    fn render(self) -> Element {
        let disclosure_hint = if self.expanded {
            "Ocultar contenido"
        } else {
            "Mostrar contenido"
        };
        let navigates = self.open_action.is_some();
        let (filename_id, filename_action) = match self.open_action {
            Some((id, _)) => (id.clone(), id),
            None => (format!("{}:filename", self.id), self.id.clone()),
        };
        let full_path = if self.path.is_empty() {
            self.filename.clone()
        } else {
            format!(
                "{}/{}",
                self.path.trim_end_matches(['/', '\\']),
                self.filename
            )
        };
        let mut filename = Element::button(&filename_id, &self.filename)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(self.palette.text)
            .dispatch_as(filename_action)
            .padding(FILENAME_PADDING)
            .size(None, Some(CONTROL_SIDE));
        if navigates {
            filename = filename.underline_on_hover();
        }
        let filename = Tooltip::new(filename, &filename_id, "", self.palette)
            .description(full_path)
            .placement(TooltipPlacement::Right)
            .max_width(360.0)
            .render();
        // The title remains intrinsic inside a bounded slot. Spare space,
        // parent path and state belong to the surrounding disclosure target.
        let title_slot = Element::row(vec![filename, Element::spacer().flex(1.0)])
            .clip_children()
            .min_size(0.0, 0.0)
            .flex(3.0);
        let mut identity = vec![centered_icon(self.icon, self.palette.muted), title_slot];
        if let Some((additions, deletions)) = self.stats {
            let counts = [
                (additions, "+", self.diff_inks.0),
                (deletions, "−", self.diff_inks.1),
            ]
            .into_iter()
            .filter(|(count, _, _)| *count > 0)
            .map(|(count, sign, color)| {
                Text::new(format!("{sign}{}", count_label(count)), self.palette)
                    .role(TextRole::Small)
                    .mono()
                    .ink(color)
                    .render()
            })
            .collect();
            identity.push(
                Element::column(vec![
                    Element::spacer().flex(1.0),
                    Element::row(counts).gap(6.0).size(None, Some(16.0)),
                    Element::spacer().flex(1.0),
                ])
                .size(None, Some(CONTROL_SIDE)),
            );
        } else if !self.status.is_empty() {
            // Size short states to their text so they don't take space away
            // from the filename in a narrow card. The cap still bounds long
            // permission/error labels; the gutter keeps both roles distinct.
            identity.push(Element::spacer().size(Some(4.0), None));
            identity.push(
                Element::column(vec![
                    Element::spacer().flex(1.0),
                    status_label(&self.id, &self.status, self.running, self.palette).flex(0.0),
                    Element::spacer().flex(1.0),
                ])
                .size(None, Some(CONTROL_SIDE))
                .max_width(104.0),
            );
        }
        let mut header = vec![Element::row(identity).gap(2.0).clip_children().flex(1.0)];
        if !self.actions.is_empty() {
            header.push(Element::row(self.actions).gap(2.0));
        }
        let disclosure = Element::panel(
            Element::icon(if self.expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            })
            .foreground(self.palette.muted),
        )
        .padding((CONTROL_SIDE - ICON_SIDE) / 2.0)
        .size(Some(CONTROL_SIDE), Some(CONTROL_SIDE));
        header.push(disclosure);
        let header_height = if self.compact {
            COMPACT_HEADER_HEIGHT
        } else {
            HEADER_HEIGHT
        };
        let header_padding = (header_height - CONTROL_SIDE) / 2.0;
        let rows = vec![Element::column(vec![
            Element::spacer().size(None, Some(header_padding)),
            Element::row(vec![
                Element::spacer().size(Some(HEADER_INSET), None),
                Element::row(header)
                    .gap(2.0)
                    .size(None, Some(CONTROL_SIDE))
                    .flex(1.0),
                Element::spacer().size(Some(HEADER_INSET), None),
            ])
            .size(None, Some(CONTROL_SIDE)),
            Element::spacer().size(None, Some(header_padding)),
        ])
        .size(None, Some(header_height))
        .clip_children()];
        // UiTree resolves the innermost target once: filename/actions win over
        // this disclosure, and only that resolved target receives hover paint.
        let header = self.palette.interactive_surface(Element::clickable(
            &self.id,
            Element::column(rows).clip_children(),
        ));
        let mut header = Tooltip::new(header, &self.id, disclosure_hint, self.palette);
        if let Some(detail) = self.detail {
            header = header.description(detail);
        } else if let Some((additions, deletions)) = self.stats {
            header = header.description(format!("Cambios: +{additions} · −{deletions}"));
        } else if !self.status.is_empty() {
            header = header.description(&self.status);
        }
        let mut rows = vec![header.render()];
        if let Some(note) = self.note.filter(|note| !note.is_empty()) {
            rows.push(
                Text::new(note, self.palette)
                    .role(TextRole::Small)
                    .muted()
                    .render()
                    .padding(8.0),
            );
        }
        if self.expanded {
            rows.push(Separator::horizontal(self.palette).render());
            rows.push(if self.body_inset {
                activity_body_inset(self.content)
            } else {
                self.content
            });
            rows.push(crate::collapse_footer(&self.id, self.palette));
        }
        let element = Element::column(rows).clip_children();
        if self.compact && !self.expanded {
            element
        } else {
            // The one pixel keeps the header's hover inside the hairline; the
            // palette's surface is the card's fill.
            Card::new(
                element.padding(1.0),
                ComponentPalette {
                    raised: self.palette.surface,
                    ..self.palette
                },
            )
            .density(CardDensity::Flush)
            .render()
        }
    }
}

// The body before the shell moved to limn, kept for the parity test.
#[cfg(test)]
impl FileActivity {
    fn legacy_render(self) -> Element {
        let disclosure_hint = if self.expanded {
            "Ocultar contenido"
        } else {
            "Mostrar contenido"
        };
        let navigates = self.open_action.is_some();
        let (filename_id, filename_action) = match self.open_action {
            Some((id, _)) => (id.clone(), id),
            None => (format!("{}:filename", self.id), self.id.clone()),
        };
        let full_path = if self.path.is_empty() {
            self.filename.clone()
        } else {
            format!(
                "{}/{}",
                self.path.trim_end_matches(['/', '\\']),
                self.filename
            )
        };
        let mut filename = Element::button(&filename_id, &self.filename)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(self.palette.text)
            .dispatch_as(filename_action)
            .padding(FILENAME_PADDING)
            .size(None, Some(CONTROL_SIDE));
        if navigates {
            filename = filename.underline_on_hover();
        }
        let filename = Tooltip::new(filename, &filename_id, "", self.palette)
            .description(full_path)
            .placement(TooltipPlacement::Right)
            .max_width(360.0)
            .render();
        // The title remains intrinsic inside a bounded slot. Spare space,
        // parent path and state belong to the surrounding disclosure target.
        let title_slot = Element::row(vec![filename, Element::spacer().flex(1.0)])
            .clip_children()
            .min_size(0.0, 0.0)
            .flex(3.0);
        let mut identity = vec![centered_icon(self.icon, self.palette.muted), title_slot];
        if let Some((additions, deletions)) = self.stats {
            let counts = [(additions, "+", ADDED), (deletions, "−", REMOVED)]
                .into_iter()
                .filter(|(count, _, _)| *count > 0)
                .map(|(count, sign, color)| {
                    Element::text(format!("{sign}{}", count_label(count)))
                        .font_family(FontFamily::Monospace)
                        .font_size(11.0)
                        .foreground(color)
                })
                .collect();
            identity.push(
                Element::column(vec![
                    Element::spacer().flex(1.0),
                    Element::row(counts).gap(6.0).size(None, Some(16.0)),
                    Element::spacer().flex(1.0),
                ])
                .size(None, Some(CONTROL_SIDE)),
            );
        } else if !self.status.is_empty() {
            // Size short states to their text so they don't take space away
            // from the filename in a narrow card. The cap still bounds long
            // permission/error labels; the gutter keeps both roles distinct.
            identity.push(Element::spacer().size(Some(4.0), None));
            identity.push(
                Element::column(vec![
                    Element::spacer().flex(1.0),
                    status_label(&self.id, &self.status, self.running, self.palette).flex(0.0),
                    Element::spacer().flex(1.0),
                ])
                .size(None, Some(CONTROL_SIDE))
                .max_width(104.0),
            );
        }
        let mut header = vec![Element::row(identity).gap(2.0).clip_children().flex(1.0)];
        if !self.actions.is_empty() {
            header.push(Element::row(self.actions).gap(2.0));
        }
        let disclosure = Element::panel(
            Element::icon(if self.expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            })
            .foreground(self.palette.muted),
        )
        .padding((CONTROL_SIDE - ICON_SIDE) / 2.0)
        .size(Some(CONTROL_SIDE), Some(CONTROL_SIDE));
        header.push(disclosure);
        let header_height = if self.compact {
            COMPACT_HEADER_HEIGHT
        } else {
            HEADER_HEIGHT
        };
        let header_padding = (header_height - CONTROL_SIDE) / 2.0;
        let rows = vec![Element::column(vec![
            Element::spacer().size(None, Some(header_padding)),
            Element::row(vec![
                Element::spacer().size(Some(HEADER_INSET), None),
                Element::row(header)
                    .gap(2.0)
                    .size(None, Some(CONTROL_SIDE))
                    .flex(1.0),
                Element::spacer().size(Some(HEADER_INSET), None),
            ])
            .size(None, Some(CONTROL_SIDE)),
            Element::spacer().size(None, Some(header_padding)),
        ])
        .size(None, Some(header_height))
        .clip_children()];
        // UiTree resolves the innermost target once: filename/actions win over
        // this disclosure, and only that resolved target receives hover paint.
        let header = self.palette.interactive_surface(Element::clickable(
            &self.id,
            Element::column(rows).clip_children(),
        ));
        let mut header = Tooltip::new(header, &self.id, disclosure_hint, self.palette);
        if let Some(detail) = self.detail {
            header = header.description(detail);
        } else if let Some((additions, deletions)) = self.stats {
            header = header.description(format!("Cambios: +{additions} · −{deletions}"));
        } else if !self.status.is_empty() {
            header = header.description(&self.status);
        }
        let mut rows = vec![header.render()];
        if let Some(note) = self.note.filter(|note| !note.is_empty()) {
            rows.push(
                Element::text(note)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0)
                    .foreground(self.palette.muted)
                    .padding(8.0),
            );
        }
        if self.expanded {
            rows.push(
                Element::spacer()
                    .size(None, Some(1.0))
                    .background(self.palette.border),
            );
            rows.push(self.content);
            rows.push(crate::collapse_footer(&self.id, self.palette));
        }
        let element = Element::column(rows).clip_children();
        if self.compact && !self.expanded {
            element
        } else {
            element
                .padding(1.0)
                .background(self.palette.surface)
                .border(self.palette.border, 1.0)
                .radius(8.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DrawCommand, HitKind, Key, Point, Size, UiAction, UiEvent, UiTree};
    use std::time::{Duration, Instant};

    fn card(expanded: bool, running: bool) -> Element {
        FileActivity::new(
            "file:call-7:0",
            "session_surface_with_a_long_name.rs",
            Element::column(vec![
                Element::text("old and new code in the same preview"),
                Element::button("preview:copy", "Copiar"),
            ])
            .padding(8.0),
            ComponentPalette::neutral_dark(),
        )
        .path("apps/agents-window/src/conversation/native")
        .status("Editando")
        .running(running)
        .expanded(expanded)
        .stats(123, 14)
        .open_action("file:open_title", "Abrir en el editor")
        .actions(vec![
            Element::button("file:open", "O").size(Some(CONTROL_SIDE), Some(CONTROL_SIDE)),
            Element::button("file:review", "R").size(Some(CONTROL_SIDE), Some(CONTROL_SIDE)),
        ])
        .render()
    }

    #[test]
    fn the_shell_on_limn_matches_the_body_it_replaced() {
        let content = || Element::text("contenido leído");
        let build = |expanded: bool, compact: bool, stats: bool, inset: bool| {
            let make = |content| {
                let mut palette = ComponentPalette::neutral_dark();
                palette.surface = Color::rgb(25, 25, 25);
                let mut card = FileActivity::new("file:call", "saludo.rs", content, palette)
                    .path("src/conversation")
                    .status("Leyendo")
                    .expanded(expanded)
                    .compact(compact)
                    .running(!stats)
                    .note("Permiso pendiente");
                if stats {
                    card = card
                        .stats(12_345, 3)
                        .detail("Evidencia confirmada")
                        .actions(vec![Element::button("file:open", "O")
                            .size(Some(CONTROL_SIDE), Some(CONTROL_SIDE))]);
                }
                card
            };
            let old = make(if inset {
                Element::column(vec![
                    Element::spacer().size(None, Some(8.0)),
                    Element::row(vec![
                        Element::spacer().size(Some(8.0), None),
                        content().flex(1.0),
                        Element::spacer().size(Some(8.0), None),
                    ]),
                    Element::spacer().size(None, Some(2.0)),
                ])
            } else {
                content()
            });
            (
                old.legacy_render(),
                make(content()).body_inset(inset).render(),
            )
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
    fn the_counts_take_the_inks_of_the_host() {
        let (added, removed) = (Color::rgb(1, 2, 3), Color::rgb(4, 5, 6));
        let mut tree = UiTree::new(
            FileActivity::new(
                "file:call",
                "saludo.rs",
                Element::spacer(),
                ComponentPalette::neutral_dark(),
            )
            .stats(7, 2)
            .diff_inks(added, removed)
            .render(),
        );
        let frame = tree.layout(Size::new(360.0, 80.0));
        for (label, ink) in [("+7", added), ("−2", removed)] {
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, color, .. } if text == label && *color == ink)));
        }
    }

    #[test]
    fn narrow_file_headers_preserve_independent_controls_and_keyboard_disclosure() {
        for width in [240.0, 360.0, 900.0] {
            let mut tree = UiTree::new(card(false, false));
            tree.set_text_measurer(|text, font_size, _| {
                text.chars().count() as f32 * font_size * 0.54
            });
            let frame = tree.layout(Size::new(width, 300.0));
            let region = |id: &str| {
                frame
                    .regions
                    .iter()
                    .find(|region| region.id == id && region.kind == HitKind::Button)
                    .unwrap()
                    .rect
            };
            let toggle = region("file:call-7:0");
            let title = region("file:open_title");
            let open = region("file:open");
            let review = region("file:review");
            assert_eq!(toggle.width, width - 2.0);
            assert_eq!(toggle.height, HEADER_HEIGHT);
            assert_eq!(title.height, CONTROL_SIDE);
            assert_eq!(title.y + title.height / 2.0, 1.0 + HEADER_HEIGHT / 2.0);
            assert!(title.width > 0.0);
            assert!(title.x + title.width <= open.x);
            assert!(open.x + open.width <= review.x);
            assert!(review.x + review.width < toggle.x + toggle.width - CONTROL_SIDE);
            for bounds in [title, open, review] {
                assert!(bounds.x >= toggle.x && bounds.y >= toggle.y);
                assert!(bounds.y + bounds.height <= HEADER_HEIGHT + 1.0);
            }
            let file_icon = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Icon { icon, rect, .. }
                        if *icon == crate::file_icon("session_surface_with_a_long_name.rs") =>
                    {
                        Some(*rect)
                    }
                    _ => None,
                })
                .unwrap();
            assert_eq!((file_icon.width, file_icon.height), (ICON_SIDE, ICON_SIDE));
            assert!(
                (file_icon.y + file_icon.height / 2.0 - (title.y + title.height / 2.0)).abs() < 0.1
            );
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "preview:copy"));
            assert!(!frame
                .text_layouts()
                .iter()
                .any(|layout| layout.source.contains("old and new code")));
            for id in [
                "file:open_title",
                "file:open",
                "file:review",
                "file:call-7:0",
            ] {
                assert!(tree.focus(id));
                for key in [Key::Enter, Key::Space] {
                    assert_eq!(
                        tree.handle_event(UiEvent::Key {
                            key,
                            shift: false,
                            ctrl: false
                        }),
                        vec![UiAction::Clicked { id: id.into() }]
                    );
                }
            }
            let point = Point::new(open.x + open.width / 2.0, open.y + open.height / 2.0);
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked {
                    id: "file:open".into()
                }]
            );
            assert!(tree.focus("file:call-7:0"));
            tree.set_root(card(true, false));
            let expanded = tree.layout(Size::new(width, 300.0));
            assert_eq!(tree.focused_id(), Some("file:call-7:0"));
            assert!(expanded
                .regions
                .iter()
                .any(|region| region.id == "preview:copy"));
            assert!(expanded
                .text_layouts()
                .iter()
                .any(|layout| layout.source.contains("old and new code")));
        }
    }

    #[test]
    fn notes_remain_visible_and_large_counts_do_not_take_the_filename_space() {
        for (width, compact) in [240.0, 360.0, 900.0]
            .into_iter()
            .flat_map(|width| [false, true].map(|compact| (width, compact)))
        {
            let note = "La operación se interrumpió; esta vista previa no confirma una escritura.";
            let mut tree = UiTree::new(
                FileActivity::new(
                    "file",
                    "main.rs",
                    Element::text("private preview"),
                    ComponentPalette::neutral_dark(),
                )
                .compact(compact)
                .status("Resultado sin confirmar")
                .stats(usize::MAX, usize::MAX)
                .note(note)
                .render(),
            );
            let frame = tree.layout(Size::new(width, 300.0));
            let layouts = frame.text_layouts();
            let note_layout = layouts.iter().find(|layout| layout.source == note).unwrap();
            assert!(!note_layout.runs.is_empty());
            assert!(note_layout
                .runs
                .last()
                .unwrap()
                .text
                .ends_with("escritura."));
            for run in &note_layout.runs {
                assert!(run.rect.x >= 8.0);
                assert!(run.rect.x + run.rect.width <= width - 8.0 + 0.1);
                assert!(
                    run.rect.y
                        >= if compact {
                            COMPACT_HEADER_HEIGHT
                        } else {
                            HEADER_HEIGHT
                        }
                );
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
            assert!(!frame
                .text_layouts()
                .iter()
                .any(|layout| layout.source == "private preview"));
            for command in &frame.commands {
                if let DrawCommand::Text { text, position, .. } = command {
                    if text == "+999m+" || text == "−999m+" {
                        assert_eq!(frame.hit_test(*position), Some("file"));
                    }
                }
            }
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == "main.rs"
            )));
        }
    }

    #[test]
    fn filename_is_independent_and_all_other_header_regions_toggle_without_double_hover() {
        let palette = ComponentPalette::neutral_dark();
        let mut tree = UiTree::new(
            FileActivity::new("details", "prueba-raiz.md", Element::spacer(), palette)
                .open_action("open-file", "Abrir en el editor")
                .status("Cancelada · sin ejecutar")
                .render(),
        );
        tree.set_reduced_motion(true);
        tree.set_text_measurer(|text, size, _| text.chars().count() as f32 * size * 0.5);
        let size = Size::new(900.0, 160.0);
        let frame = tree.layout(size);
        let title = frame
            .regions
            .iter()
            .find(|region| region.id == "open-file")
            .unwrap()
            .rect;
        assert!(
            title.width < 130.0,
            "a short title must not fill the header"
        );
        let point = Point::new(title.x + title.width / 2.0, title.y + title.height / 2.0);
        tree.handle_event(UiEvent::PointerMove(point));
        let hover = tree.layout(size);
        let fills: Vec<_> = hover
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRect { rect, color, .. } if *color == palette.hover => Some(*rect),
                _ => None,
            })
            .collect();
        assert!(fills.is_empty(), "filename links never paint a pill hover");
        assert!(
            hover.commands.iter().any(|command| matches!(command,
                DrawCommand::FillRect { rect, color, radius }
                    if *color == palette.text && rect.height == 1.0 && *radius == 0.0
                        && rect.x >= title.x && rect.x + rect.width <= title.x + title.width
                        && rect.y >= title.y && rect.y < title.y + title.height
            )),
            "filename hover underlines its visible text"
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "open-file".into()
            }]
        );
        let blank = Point::new(450.0, point.y);
        let disclosure = frame
            .regions
            .iter()
            .find(|region| region.id == "details")
            .unwrap()
            .rect;
        let icon_points: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Icon { rect, .. } => Some(Point::new(
                    rect.x + rect.width / 2.0,
                    rect.y + rect.height / 2.0,
                )),
                _ => None,
            })
            .collect();
        for point in icon_points.into_iter().chain([
            blank,
            Point::new(disclosure.x + 2.0, disclosure.y + 2.0),
            Point::new(
                disclosure.x + disclosure.width / 2.0,
                disclosure.y + disclosure.height - 8.0,
            ),
        ]) {
            assert_eq!(frame.hit_test(point), Some("details"));
            tree.handle_event(UiEvent::PointerMove(point));
            let hovered = tree.layout(size);
            let fills: Vec<_> = hovered
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::FillRect { rect, color, .. } if *color == palette.hover => {
                        Some(*rect)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(fills, vec![disclosure], "only the row receives hover");
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked {
                    id: "details".into()
                }]
            );
        }
    }

    #[test]
    fn running_sweeps_only_status_and_respects_reduced_motion_and_completion() {
        let start = Instant::now();
        let size = Size::new(360.0, 280.0);
        let running_card = |running| {
            FileActivity::new(
                "file:running",
                "main.rs",
                Element::spacer(),
                ComponentPalette::neutral_dark(),
            )
            .status("Editando")
            .running(running)
            .render()
        };
        let mut tree = UiTree::new(running_card(true));
        let first = tree.layout_at(size, start);
        let moving = tree.layout_at(size, start + Duration::from_millis(650));
        let icons = |frame: &crate::Frame| {
            frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Icon { .. } => Some(command.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(icons(&first), icons(&moving));
        assert!(!moving
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::Spinner { .. })));
        assert_ne!(first.commands, moving.commands);
        assert!(tree.is_animating());
        tree.set_root(running_card(true));
        let rebuilt = tree.layout_at(size, start + Duration::from_millis(650));
        assert_eq!(
            moving.commands, rebuilt.commands,
            "stream revisions preserve sweep phase"
        );
        tree.set_reduced_motion(true);
        let quiet = tree.layout_at(size, start + Duration::from_secs(1));
        let quiet_later = tree.layout_at(size, start + Duration::from_secs(2));
        assert_eq!(quiet.commands, quiet_later.commands);
        assert!(!tree.is_animating());
        tree.set_reduced_motion(false);
        tree.set_root(running_card(false));
        let completed = tree.layout_at(size, start + Duration::from_secs(3));
        let completed_later = tree.layout_at(size, start + Duration::from_secs(4));
        assert_eq!(completed.commands, completed_later.commands);
        assert!(!tree.is_animating());
    }

    #[test]
    fn compact_reading_keeps_file_identity_and_state_in_a_single_quiet_row() {
        for width in [240.0, 360.0, 900.0] {
            let make = |expanded| {
                FileActivity::new(
                    "read:call-1",
                    "config.rs",
                    Element::text("numbered read preview"),
                    ComponentPalette::neutral_dark(),
                )
                .path("a/long/directory/that/remains/in/the/tooltip")
                .status("Leyendo")
                .compact(true)
                .expanded(expanded)
                .running(true)
                .render()
            };
            let mut tree = UiTree::new(make(false));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 240.0));
            let toggle = frame
                .regions
                .iter()
                .find(|region| region.id == "read:call-1")
                .unwrap();
            assert_eq!(toggle.rect.height, COMPACT_HEADER_HEIGHT);
            assert_eq!(toggle.rect.width, width);
            assert_eq!(
                toggle.rect.y + toggle.rect.height / 2.0,
                COMPACT_HEADER_HEIGHT / 2.0
            );
            for command in &frame.commands {
                if let DrawCommand::Icon { rect, .. } = command {
                    assert_eq!((rect.width, rect.height), (ICON_SIDE, ICON_SIDE));
                    assert_eq!(rect.y + rect.height / 2.0, COMPACT_HEADER_HEIGHT / 2.0);
                }
            }
            // Ghost controls retain a transparent base command when motion
            // is reduced. Only an actually painted surface would break the
            // quiet reading row.
            assert!(!frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::FillRect { color, .. } | DrawCommand::StrokeRect { color, .. }
                    if color.a > 0
            )));
            assert!(frame.commands.iter().any(
                |command| matches!(command, DrawCommand::Text { text, .. } if text == "config.rs")
            ));
            assert!(frame.commands.iter().any(
                |command| matches!(command, DrawCommand::Text { text, .. } if text == "Leyendo")
            ));
            assert!(!tree.is_animating());
            tree.set_root(make(true));
            let expanded = tree.layout(Size::new(width, 240.0));
            let layouts = expanded.text_layouts();
            let preview = layouts
                .iter()
                .find(|layout| layout.source == "numbered read preview")
                .unwrap();
            assert_eq!(
                preview.runs.first().unwrap().rect.y,
                COMPACT_HEADER_HEIGHT + 2.0
            );
            assert!(expanded.commands.iter().any(|command| matches!(
                command,
                DrawCommand::StrokeRect { color, .. } if color.a > 0
            )));
        }
    }

    #[test]
    fn parent_stays_out_of_the_header_and_the_tooltip_preserves_every_segment() {
        for depth in [4, 80] {
            let parent = format!("src/{}", "nested-module/".repeat(depth));
            let parent = parent.trim_end_matches('/');
            let full_path = format!("{parent}/main.rs");
            let mut tree = UiTree::new(
                FileActivity::new(
                    "long-file",
                    "main.rs",
                    Element::spacer(),
                    ComponentPalette::neutral_dark(),
                )
                .path(parent)
                .status("Leído")
                .open_action("open-long-file", "Abrir en el editor")
                .render(),
            );
            tree.set_text_measurer(|text, font_size, _| {
                text.chars().count() as f32 * font_size * 0.5
            });
            let start = Instant::now();
            let size = Size::new(360.0, 1200.0);
            let frame = tree.layout_at(size, start);
            let header = frame
                .regions
                .iter()
                .find(|region| region.id == "long-file")
                .unwrap()
                .rect;
            let mut clips = Vec::new();
            let mut parent_y = None;
            let mut bands = 0;
            for command in &frame.commands {
                match command {
                    DrawCommand::PushClip(rect) => clips.push(*rect),
                    DrawCommand::PopClip => {
                        clips.pop();
                    }
                    DrawCommand::Text { text, position, .. } if text == parent => {
                        let y = *parent_y.get_or_insert(position.y);
                        assert_eq!(position.y, y, "alpha bands must reuse the same shaped line");
                        let clip = clips.last().expect("the parent path must be clipped");
                        assert!(clip.x >= 1.0 && clip.x + clip.width <= size.width - 1.0);
                        assert!(
                            clip.y >= header.y && clip.y + clip.height <= header.y + header.height
                        );
                        bands += 1;
                    }
                    _ => {}
                }
            }
            assert_eq!(
                bands, 0,
                "parent paths belong in the tooltip, not a second header label"
            );
            assert!(tree.focus("open-long-file"));
            let focused = tree.layout_at(size, start + Duration::from_millis(400));
            // Tooltip placement may fall back on narrow surfaces. Its overlay
            // follows the shadow, independently of the header's clipped bands.
            let tooltip_start = focused
                .commands
                .iter()
                .rposition(|command| matches!(command, DrawCommand::Shadow { .. }))
                .expect("focused filename tooltip");
            let tooltip_text: String = focused
                .commands
                .iter()
                .skip(tooltip_start)
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            assert_eq!(
                tooltip_text, full_path,
                "only the full path at depth {depth}"
            );
        }
    }

    #[test]
    fn filename_tooltip_prefers_a_lateral_position_and_only_shows_the_path() {
        let mut tree = UiTree::new(
            FileActivity::new(
                "details",
                "main.rs",
                Element::spacer(),
                ComponentPalette::neutral_dark(),
            )
            .path("src")
            .status("Leído")
            .open_action("open-file", "Abrir en el editor")
            .render(),
        );
        let start = Instant::now();
        let size = Size::new(900.0, 240.0);
        let frame = tree.layout_at(size, start);
        let title = frame
            .regions
            .iter()
            .find(|region| region.id == "open-file")
            .unwrap()
            .rect;
        assert!(tree.focus("open-file"));
        let focused = tree.layout_at(size, start + Duration::from_millis(400));
        let (index, tooltip) = focused
            .commands
            .iter()
            .enumerate()
            .find_map(|(index, command)| {
                if let DrawCommand::Shadow { rect, .. } = command {
                    Some((index, *rect))
                } else {
                    None
                }
            })
            .expect("filename tooltip");
        assert!(tooltip.x >= title.x + title.width + 7.0);
        let text_top = focused.commands[index..]
            .iter()
            .find_map(|command| {
                if let DrawCommand::Text { position, .. } = command {
                    Some(position.y)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(
            text_top < tooltip.y + tooltip.height / 2.0,
            "path must not follow an empty title row"
        );
        let text = focused.commands[index..]
            .iter()
            .filter_map(|command| {
                if let DrawCommand::Text { text, .. } = command {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(text, "src/main.rs");
    }
}
