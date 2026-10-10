//! A caller-owned permissions menu. The host supplies the choices and their
//! exact meaning; this component does not change tool or sandbox policies.

use crate::{
    Component, ComponentPalette, Element, FontFamily, Icon, Popover, PopoverPlacement, ScrollArea,
    ScrollbarAppearance, SelectorSizing, Tooltip, TooltipPlacement,
};

/// One policy choice with a short explanation and a single complete hit target.
pub struct PermissionOption {
    action_id: String,
    label: String,
    description: String,
    icon: Option<Icon>,
    selected: bool,
    enabled: bool,
    separator_before: bool,
}

impl PermissionOption {
    pub fn new(
        action_id: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            action_id: action_id.into(),
            label: label.into(),
            description: description.into(),
            icon: None,
            selected: false,
            enabled: true,
            separator_before: false,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Visually separate this choice from the preceding policy group.
    pub fn separator_before(mut self) -> Self {
        self.separator_before = true;
        self
    }
}

/// A compact permissions popover. Explanations appear in lateral tooltips;
/// the host owns the open state, selected choices, actions and policy.
pub struct PermissionSelector {
    anchor: Element,
    options: Vec<PermissionOption>,
    open: bool,
    dismiss_id: String,
    heading: String,
    description: Option<String>,
    width: f32,
    placement: PopoverPlacement,
    palette: ComponentPalette,
}

impl PermissionSelector {
    pub fn new(
        anchor: Element,
        options: Vec<PermissionOption>,
        open: bool,
        dismiss_id: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            anchor,
            options,
            open,
            dismiss_id: dismiss_id.into(),
            heading: "Permisos".to_owned(),
            description: None,
            width: SelectorSizing::COMPACT.preferred_width,
            placement: PopoverPlacement::TopStart,
            palette,
        }
    }

    pub fn heading(mut self, heading: impl Into<String>) -> Self {
        self.heading = heading.into();
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = width.clamp(
                SelectorSizing::COMPACT.min_width,
                SelectorSizing::COMPACT.preferred_width,
            );
        }
        self
    }

    pub fn placement(mut self, placement: PopoverPlacement) -> Self {
        self.placement = placement;
        self
    }
}

impl Component for PermissionSelector {
    fn render(self) -> Element {
        if !self.open {
            return self.anchor;
        }
        let palette = self.palette;
        let sizing = SelectorSizing::COMPACT;
        // Selection is a check, separate from the single active hover/focus.
        // The gap is outside every option's complete hit target.
        let row_gap = sizing.padding / 2.0;
        // Popover adds six pixels of inset on each side. Let ScrollArea reserve
        // its rail only when the actual viewport height requires scrolling.
        // Flexible rows keep the check column inside either resulting width.
        let viewport_width = self.width - 12.0;
        let rail_width = sizing.scrollbar_gutter;
        let header = if self.heading.trim().is_empty() {
            None
        } else {
            let title = Element::truncated_text(&self.heading)
                .font_family(FontFamily::Sans)
                .font_size(sizing.detail_size)
                .foreground(palette.muted)
                .padding(sizing.padding)
                .size(Some(viewport_width), Some(sizing.header_height));
            Some(if let Some(description) = self.description {
                Tooltip::new(
                    title,
                    format!("{}:heading", self.dismiss_id),
                    self.heading,
                    palette,
                )
                .description(description)
                .max_width(sizing.tooltip_width)
                .placement(TooltipPlacement::Left)
                .render()
            } else {
                title
            })
        };
        let mut rows = Vec::with_capacity(self.options.len());
        let mut content_height = 0.0;
        for option in self.options {
            if option.separator_before && !rows.is_empty() {
                content_height += 1.0;
                rows.push(
                    Element::spacer()
                        .background(palette.border)
                        .size(None, Some(1.0)),
                );
            }
            let ink = if option.enabled {
                palette.text
            } else {
                palette.muted
            };
            let copy = Element::truncated_text(&option.label)
                .font_family(FontFamily::Sans)
                .font_size(sizing.label_size)
                .foreground(ink)
                .flex(1.0)
                .size(None, Some(sizing.icon_size));
            let icon = option
                .icon
                .map(|icon| Element::icon(icon).foreground(ink))
                .unwrap_or_else(Element::spacer)
                .size(Some(sizing.icon_size), Some(sizing.icon_size));
            let check = if option.selected {
                Element::icon(Icon::Check).foreground(ink)
            } else {
                Element::spacer()
            }
            .size(Some(sizing.icon_size), Some(sizing.icon_size));
            let content = Element::row(vec![center(icon), copy, center(check)]).gap(sizing.gap);
            let row = if option.enabled {
                palette
                    .interactive_surface(Element::clickable(&option.action_id, content))
                    .action_menu_item()
                    .transition_ms(0)
            } else {
                Element::panel(content)
            };
            let row = row
                .background(crate::Color::TRANSPARENT)
                .padding(sizing.padding)
                .size(None, Some(sizing.row_height))
                .radius(crate::tokens().radius.control);
            rows.push(
                Tooltip::new(row, option.action_id, option.label, palette)
                    .description(option.description)
                    .max_width(sizing.tooltip_width)
                    .placement(TooltipPlacement::Left)
                    .render(),
            );
            content_height += sizing.row_height;
        }
        content_height += rows.len().saturating_sub(1) as f32 * row_gap;
        // Leave short menus intrinsic and cap longer lists at eight rows,
        // including separators in their measured content height. As a flex
        // child, the scroll viewport
        // receives only the remaining height after UiTree clamps the popover
        // to a short window. Offscreen options remain reachable by wheel,
        // scrollbar and keyboard focus; the explanatory header stays fixed.
        let max_height = sizing.max_visible_items as f32 * sizing.row_height
            + sizing.max_visible_items.saturating_sub(1) as f32 * row_gap;
        let preferred_height = (content_height > max_height).then_some(max_height);
        let options = ScrollArea::new(
            format!("{}:options", self.dismiss_id),
            Element::column(rows).gap(row_gap),
        )
        .appearance(
            ScrollbarAppearance::reserved()
                .rail_width(rail_width)
                .tint(palette.muted),
        )
        .render()
        .size(Some(viewport_width), preferred_height)
        .flex(1.0);
        let mut content = Vec::with_capacity(2);
        if let Some(header) = header {
            content.push(header);
        }
        content.push(options);
        Popover::new(
            self.anchor,
            Element::column(content),
            true,
            self.dismiss_id,
            palette,
        )
        .width(self.width)
        .placement(self.placement)
        .animation_ms(120)
        .render()
    }
}

fn center(child: Element) -> Element {
    Element::column(vec![
        Element::spacer().flex(1.0),
        child,
        Element::spacer().flex(1.0),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DrawCommand, HitKind, Key, Point, Size, UiAction, UiEvent, UiTree};
    use std::time::{Duration, Instant};

    #[test]
    fn closed_selector_preserves_only_its_trigger() {
        let frame = UiTree::new(
            PermissionSelector::new(
                Element::button("trigger", "Permisos"),
                vec![PermissionOption::new("policy", "Política", "Explicación")],
                false,
                "dismiss",
                ComponentPalette::neutral_dark(),
            )
            .render(),
        )
        .layout(Size::new(320.0, 200.0));
        assert!(frame.regions.iter().any(|region| region.id == "trigger"));
        assert!(!frame.regions.iter().any(|region| region.id == "policy"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.contains("dismiss")));
    }

    #[test]
    fn compact_rows_keep_selected_checks_and_move_explanations_to_lateral_tooltips() {
        let sizing = SelectorSizing::COMPACT;
        let palette = ComponentPalette::neutral_dark();
        let mut tree = UiTree::new(Element::column(vec![
            Element::spacer().size(None, Some(120.0)),
            Element::row(vec![
                Element::spacer().size(Some(520.0), None),
                PermissionSelector::new(
                    Element::button("trigger", "Permisos"),
                    vec![
                        PermissionOption::new("plan", "Plan", "Prepara un plan antes de ejecutar.")
                            .icon(Icon::Plan)
                            .selected(true),
                        PermissionOption::new(
                            "read",
                            "Sólo lectura",
                            "Permite leer archivos sin modificarlos.",
                        )
                        .icon(Icon::File)
                        .selected(true)
                        .separator_before(),
                        PermissionOption::new(
                            "ask",
                            "Pedir aprobación",
                            "Solicita aprobación antes de modificar archivos.",
                        )
                        .icon(Icon::Settings),
                    ],
                    true,
                    "dismiss",
                    palette,
                )
                .render(),
            ]),
        ]));
        tree.set_reduced_motion(true);
        let viewport = Size::new(1_000.0, 420.0);
        let start = Instant::now();
        let initial = tree.layout_at(viewport, start);
        for id in ["plan", "read", "ask"] {
            let row = initial
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap()
                .rect;
            assert_eq!(row.height, sizing.row_height);
            assert_eq!(row.width, sizing.preferred_width - 12.0);
        }
        assert!(!initial
            .regions
            .iter()
            .any(|region| region.kind == HitKind::Scrollbar));
        let options = initial
            .regions
            .iter()
            .find(|region| region.id == "dismiss:options" && region.kind == HitKind::Scroll)
            .unwrap()
            .rect;
        let read = initial
            .regions
            .iter()
            .find(|region| region.id == "read" && region.kind == HitKind::Button)
            .unwrap()
            .rect;
        assert_eq!(read.x + read.width, options.x + options.width);
        assert_eq!(
            initial.hit_test(Point::new(
                read.x + read.width - 2.0,
                read.y + read.height / 2.0
            )),
            Some("read")
        );
        assert_eq!(
            initial
                .commands
                .iter()
                .filter(|command| matches!(
                    command,
                    DrawCommand::Icon {
                        icon: Icon::Check,
                        ..
                    }
                ))
                .count(),
            2
        );
        assert!(initial.commands.iter().any(|command| matches!(command,
            DrawCommand::Text {text,font_size,..}
                if text == "Plan" && *font_size == sizing.label_size
        )));
        assert!(!initial.commands.iter().any(|command| matches!(command,
            DrawCommand::Text {text,..} if text.contains("Permite leer")
        )));
        assert!(tree.focus("read"));
        let focused = tree.layout_at(viewport, start + Duration::from_secs(1));
        let anchor = focused
            .regions
            .iter()
            .find(|region| region.id == "read")
            .unwrap()
            .rect;
        assert!(focused.commands.iter().any(|command| matches!(command,
            DrawCommand::Text {text,..} if text.contains("Permite leer")
        )));
        assert!(
            focused.commands.iter().any(|command| matches!(command,
                DrawCommand::Shadow {rect,..}
                    if rect.width <= sizing.tooltip_width && rect.x+rect.width <= anchor.x
            )),
            "permission explanation fits to the left of the menu"
        );
        let point = Point::new(
            anchor.x + anchor.width / 2.0,
            anchor.y + anchor.height / 2.0,
        );
        assert_eq!(focused.hit_test(point), Some("read"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked { id: "read".into() }]
        );
    }

    #[test]
    fn checked_and_hovered_neighbors_keep_a_non_interactive_gap_and_one_highlight() {
        let sizing = SelectorSizing::COMPACT;
        let palette = ComponentPalette::neutral_dark();
        for viewport in [Size::new(220.0, 300.0), Size::new(1_000.0, 420.0)] {
            let mut tree = UiTree::new(
                PermissionSelector::new(
                    Element::button("trigger", "Permisos"),
                    vec![
                        PermissionOption::new("plan", "Plan", "Preparar un plan.").icon(Icon::Plan),
                        PermissionOption::new("read", "Sólo lectura", "Leer archivos.")
                            .icon(Icon::File)
                            .selected(true)
                            .separator_before(),
                        PermissionOption::new("ask", "Pedir aprobación", "Pedir confirmación.")
                            .icon(Icon::Settings),
                        PermissionOption::new("all", "Permitir todo", "Habilitar herramientas.")
                            .icon(Icon::Check),
                    ],
                    true,
                    "dismiss",
                    palette,
                )
                .width(sizing.fit_width(viewport.width - 16.0))
                .render(),
            );
            tree.set_reduced_motion(true);
            let start = Instant::now();
            let initial = tree.layout_at(viewport, start);
            let row = |frame: &crate::Frame, id: &str| {
                frame
                    .regions
                    .iter()
                    .find(|region| region.id == id && region.kind == HitKind::Button)
                    .unwrap()
                    .rect
            };
            let plan = row(&initial, "plan");
            let selected = row(&initial, "read");
            let hovered = row(&initial, "ask");
            let last = row(&initial, "all");
            let gap = sizing.padding / 2.0;
            assert_eq!(selected.y - (plan.y + plan.height), gap * 2.0 + 1.0);
            assert_eq!(hovered.y - (selected.y + selected.height), gap);
            assert_eq!(last.y - (hovered.y + hovered.height), gap);
            for rect in [plan, selected, hovered, last] {
                assert_eq!(rect.height, sizing.row_height);
            }

            let hover_point = Point::new(
                hovered.x + hovered.width / 2.0,
                hovered.y + hovered.height / 2.0,
            );
            tree.handle_event(UiEvent::PointerMove(hover_point));
            let painted = tree.layout_at(viewport, start + Duration::from_millis(100));
            assert_eq!(tree.hovered_id(), Some("ask"));
            assert!(painted.commands.iter().any(|command| matches!(command,
                DrawCommand::FillRect { rect, color, .. }
                    if *rect == hovered && *color == palette.hover
            )));
            assert!(!painted.commands.iter().any(|command| matches!(command,
                DrawCommand::FillRect { rect, color, .. }
                    if *rect == selected && (*color == palette.selected || *color == palette.hover)
            )));
            assert!(painted.commands.iter().any(|command| matches!(command,
                DrawCommand::Icon { icon: Icon::Check, rect, .. }
                    if rect.y >= selected.y && rect.y + rect.height <= selected.y + selected.height
            )));

            let gap_point = Point::new(
                hovered.x + hovered.width / 2.0,
                selected.y + selected.height + 2.0,
            );
            assert_ne!(painted.hit_region(gap_point).unwrap().kind, HitKind::Button);
            tree.handle_event(UiEvent::PointerMove(gap_point));
            tree.handle_event(UiEvent::PointerDown(gap_point));
            assert!(tree.handle_event(UiEvent::PointerUp(gap_point)).is_empty());
            let gap_frame = tree.layout_at(viewport, start + Duration::from_millis(200));
            assert_ne!(tree.hovered_id(), Some("read"));
            assert_ne!(tree.hovered_id(), Some("ask"));
            assert!(!gap_frame.commands.iter().any(|command| matches!(command,
                DrawCommand::FillRect { rect, color, .. }
                    if *rect == hovered && *color == palette.hover
            )));
        }
    }

    #[test]
    fn compact_short_selector_keeps_header_fixed_and_last_enabled_policy_keyboard_reachable() {
        for viewport in [Size::new(220.0, 148.0), Size::new(320.0, 180.0)] {
            let sizing = SelectorSizing::COMPACT;
            let mut tree = UiTree::new(
                PermissionSelector::new(
                    Element::button("trigger", "Permisos"),
                    (0..12)
                        .map(|index| {
                            let option = PermissionOption::new(
                                format!("policy:{index}"),
                                format!("Política {index}"),
                                format!("Explicación {index}"),
                            )
                            .icon(Icon::File)
                            .enabled(index != 1)
                            .selected(index == 11);
                            if index == 2 {
                                option.separator_before()
                            } else {
                                option
                            }
                        })
                        .collect(),
                    true,
                    "dismiss",
                    ComponentPalette::neutral_dark(),
                )
                .width(sizing.fit_width(viewport.width - 16.0))
                .render(),
            );
            tree.set_reduced_motion(true);
            let initial = tree.layout(viewport);
            let shell = initial
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:dismiss")
                .unwrap()
                .rect;
            let options = initial
                .regions
                .iter()
                .find(|region| region.id == "dismiss:options" && region.kind == HitKind::Scroll)
                .unwrap()
                .rect;
            let rail = initial
                .regions
                .iter()
                .find(|region| region.id == "dismiss:options" && region.kind == HitKind::Scrollbar)
                .unwrap()
                .rect;
            assert_eq!(rail.width, sizing.scrollbar_gutter);
            assert!(shell.x >= 0.0 && shell.y >= 0.0);
            assert!(
                shell.x + shell.width <= viewport.width
                    && shell.y + shell.height <= viewport.height
            );
            assert!(options.y > shell.y + sizing.header_height);
            assert!(!initial
                .regions
                .iter()
                .any(|region| region.id == "policy:1" && region.kind == HitKind::Button));
            assert!(!tree.focus("policy:1"));
            assert!(tree.focus("policy:0"));
            tree.handle_event(UiEvent::Key {
                key: Key::End,
                shift: false,
                ctrl: false,
            });
            let final_frame = tree.layout(viewport);
            assert_eq!(tree.focused_id(), Some("policy:11"));
            assert!(tree.scroll_offset("dismiss:options") > 0.0);
            let last = final_frame
                .regions
                .iter()
                .find(|region| region.id == "policy:11")
                .unwrap()
                .rect;
            assert!(last.y >= options.y && last.y + last.height <= options.y + options.height);
            assert!(last.x + last.width <= rail.x);
            assert!(final_frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Icon { icon: Icon::Check, rect, .. }
                    if rect.x >= last.x && rect.x + rect.width <= rail.x
                        && rect.y >= last.y && rect.y + rect.height <= last.y + last.height
            )));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "policy:11".into()
                }]
            );
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Escape,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "dismiss".into()
                }]
            );
        }
    }
}
