//! Shared action menu built on the retained popover and color transitions.

use crate::components::{lighter, Component, ComponentPalette, ControlState, Kbd, Popover};
use crate::{
    Color, Element, FontFamily, Icon, MenuMotion, PopoverPlacement, ScrollArea,
    ScrollbarAppearance, ICON_SIZE,
};

// The scroll viewport remains the full menu content width. Only its children
// give up this much room, so the scrollbar never paints over a row or label.
const SCROLLBAR_GUTTER: f32 = 16.0;

/// One action in a menu. The caller handles `action_id` from `UiAction`.
pub struct DropdownMenuItem {
    pub action_id: String,
    pub label: String,
    pub icon: Option<Icon>,
    pub shortcut: Option<String>,
    pub annotation: Option<String>,
    pub selected: bool,
    pub state: ControlState,
    pub submenu: Option<Vec<DropdownMenuEntry>>,
    pub destructive: Option<Color>,
}

impl DropdownMenuItem {
    pub fn new(action_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            action_id: action_id.into(),
            label: label.into(),
            icon: None,
            shortcut: None,
            annotation: None,
            selected: false,
            state: ControlState::Enabled,
            submenu: None,
            destructive: None,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self.annotation = None;
        self
    }

    /// Plain trailing status text, separate from keyboard shortcuts.
    pub fn annotation(mut self, annotation: impl Into<String>) -> Self {
        self.annotation = Some(annotation.into());
        self.shortcut = None;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.state = ControlState::Disabled;
        self
    }

    /// Opens a second menu beside this row on pointer hover or keyboard focus.
    pub fn submenu(mut self, entries: Vec<DropdownMenuEntry>) -> Self {
        self.submenu = Some(entries);
        self
    }

    pub fn destructive(mut self, ink: Color) -> Self {
        self.destructive = Some(ink);
        self
    }
}

/// Static entries do not enter keyboard focus order.
pub enum DropdownMenuEntry {
    Item(DropdownMenuItem),
    Heading(String),
    Separator,
    /// An application-specific control inside the shared menu surface.
    Custom(Element),
}

impl From<DropdownMenuItem> for DropdownMenuEntry {
    fn from(item: DropdownMenuItem) -> Self {
        Self::Item(item)
    }
}

/// Caller-owned open state with one retained, animated popover. Arrow keys,
/// Home/End, Tab, Escape and viewport-aware placement come from `UiTree`.
pub struct DropdownMenu {
    anchor: Element,
    entries: Vec<DropdownMenuEntry>,
    open: bool,
    dismiss_id: String,
    width: f32,
    placement: PopoverPlacement,
    palette: ComponentPalette,
    motion: MenuMotion,
    close_animation_ms: Option<u16>,
    open_scale: f32,
    view_key: Option<String>,
    max_visible_items: usize,
}

impl DropdownMenu {
    pub fn new(
        anchor: Element,
        entries: Vec<DropdownMenuEntry>,
        open: bool,
        dismiss_id: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            anchor,
            entries,
            open,
            dismiss_id: dismiss_id.into(),
            width: 240.0,
            placement: PopoverPlacement::BottomStart,
            palette,
            motion: MenuMotion::default(),
            close_animation_ms: None,
            open_scale: 1.0,
            view_key: None,
            max_visible_items: 8,
        }
    }

    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = width.max(96.0);
        }
        self
    }

    pub fn placement(mut self, placement: PopoverPlacement) -> Self {
        self.placement = placement;
        self
    }

    pub fn motion(mut self, motion: MenuMotion) -> Self {
        self.motion = motion;
        self
    }

    pub fn close_animation_ms(mut self, duration: u16) -> Self {
        self.close_animation_ms = Some(duration);
        self
    }

    pub fn open_scale(mut self, scale: f32) -> Self {
        if scale.is_finite() {
            self.open_scale = scale.clamp(0.5, 1.25);
        }
        self
    }

    pub fn view_key(mut self, key: impl Into<String>) -> Self {
        self.view_key = Some(key.into());
        self
    }

    pub fn max_visible_items(mut self, count: usize) -> Self {
        self.max_visible_items = count.max(1);
        self
    }
}

impl Component for DropdownMenu {
    fn render(self) -> Element {
        if !self.open {
            return self.anchor;
        }

        // Headings, separators and custom content occupy vertical space too.
        // Count them for the scroll threshold so a long mixed menu stays
        // bounded and every option remains reachable.
        let row_count = self.entries.len();
        let reserve_icon = self
            .entries
            .iter()
            .any(|entry| matches!(entry, DropdownMenuEntry::Item(item) if item.icon.is_some()));
        let reserve_check = self
            .entries
            .iter()
            .any(|entry| matches!(entry, DropdownMenuEntry::Item(item) if item.selected));
        let scrolls = row_count > self.max_visible_items;
        let viewport_width = self.width - 12.0;
        let row_width = viewport_width - if scrolls { SCROLLBAR_GUTTER } else { 0.0 };
        let palette = self.palette;
        let rows = self
            .entries
            .into_iter()
            .map(|entry| match entry {
                DropdownMenuEntry::Heading(label) => Element::truncated_text(label)
                    .foreground(palette.muted)
                    .font_family(FontFamily::Sans)
                    .font_size(12.0)
                    .padding(8.0)
                    .size(Some(row_width), Some(28.0)),
                DropdownMenuEntry::Separator => Element::spacer()
                    .background(palette.border)
                    .size(Some(row_width), Some(1.0)),
                DropdownMenuEntry::Custom(element) => element,
                DropdownMenuEntry::Item(item) => {
                    let enabled = item.state == ControlState::Enabled;
                    let has_submenu = enabled && item.submenu.is_some();
                    // Flex children with a natural text width can paint past a
                    // fixed row. Budget the trailing text against the row's
                    // real inner width, including reserved icons and gaps.
                    let control_count = usize::from(reserve_icon)
                        + usize::from(reserve_check)
                        + usize::from(has_submenu);
                    let inner_width = (row_width - 14.0).max(0.0);
                    let control_width = control_count as f32 * ICON_SIZE;
                    let gap = if control_count == 0 {
                        9.0
                    } else {
                        9.0_f32.min(((inner_width - control_width) / control_count as f32).max(0.0))
                    };
                    let main_width =
                        (inner_width - control_width - control_count as f32 * gap).max(0.0);
                    let trailing_width = item
                        .shortcut
                        .as_ref()
                        .or(item.annotation.as_ref())
                        .map(|value| {
                            (value.chars().count() as f32 * 7.0 + 12.0)
                                .min(140.0)
                                .min((main_width - 40.0 - gap).max(0.0))
                        })
                        .filter(|width| *width >= 28.0);
                    let submenu_id = item.action_id.clone();
                    let submenu = if enabled { item.submenu } else { None };
                    let ink = item.destructive.unwrap_or(palette.text);
                    let mut children = Vec::with_capacity(5);
                    if reserve_icon {
                        children.push(
                            if let Some(icon) = item.icon {
                                Element::icon(icon).foreground(if enabled {
                                    ink
                                } else {
                                    palette.muted
                                })
                            } else {
                                Element::spacer()
                            }
                            .size(Some(ICON_SIZE), Some(22.0)),
                        );
                    }
                    children.push(
                        Element::truncated_text(item.label)
                            .foreground(if enabled { ink } else { palette.muted })
                            .font_family(FontFamily::Sans)
                            .font_size(13.0)
                            .flex(1.0)
                            .size(None, Some(22.0)),
                    );
                    if let Some(trailing_width) = trailing_width {
                        let trailing = if let Some(shortcut) = item.shortcut {
                            Kbd::new(shortcut, palette)
                                .render()
                                .size(Some(trailing_width), Some(22.0))
                        } else if let Some(annotation) = item.annotation {
                            Element::truncated_text(annotation)
                                .foreground(palette.muted)
                                .font_family(FontFamily::Sans)
                                .font_size(12.0)
                                .size(Some(trailing_width), Some(22.0))
                        } else {
                            Element::spacer()
                        };
                        children.push(trailing);
                    }
                    if reserve_check {
                        children.push(
                            if item.selected {
                                Element::icon(Icon::Check).foreground(palette.text)
                            } else {
                                Element::spacer()
                            }
                            .size(Some(ICON_SIZE), Some(22.0)),
                        );
                    }
                    if has_submenu {
                        children.push(
                            Element::icon(Icon::ChevronRight)
                                .foreground(palette.muted)
                                .size(Some(ICON_SIZE), Some(22.0)),
                        );
                    }
                    let visual = Element::row(children).gap(gap).clip_children();
                    let row = if enabled {
                        Element::clickable(item.action_id, visual)
                    } else {
                        Element::panel(visual)
                    };
                    // The popover paints one continuous surface. Resting rows
                    // are transparent so their fills cannot stack with that
                    // surface while the whole menu fades in or out.
                    let row = if item.selected {
                        row.background(palette.selected)
                    } else {
                        row
                    };
                    let row = row
                        .hover_background(if item.selected {
                            lighter(palette.selected, 8)
                        } else {
                            palette.hover
                        })
                        .pressed_background(if item.selected {
                            lighter(palette.selected, 14)
                        } else {
                            palette.selected
                        })
                        .transition_ms(self.motion.item_ms)
                        .padding(7.0)
                        .radius(7.0)
                        .size(Some(row_width), Some(36.0));
                    if let Some(entries) = submenu {
                        DropdownMenu::new(
                            row,
                            entries,
                            true,
                            format!("{}:{submenu_id}", self.dismiss_id),
                            palette,
                        )
                        .width(self.width)
                        .placement(PopoverPlacement::RightStart)
                        .motion(self.motion)
                        .max_visible_items(self.max_visible_items)
                        .render()
                        .popover_on_hover()
                    } else {
                        row
                    }
                }
            })
            .collect();
        let list = Element::column(rows).gap(2.0);
        let content = if scrolls {
            ScrollArea::new(format!("{}:options", self.dismiss_id), list)
                .appearance(ScrollbarAppearance::reserved().rail_width(SCROLLBAR_GUTTER))
                .tint(palette.muted)
                .render()
                .size(
                    Some(viewport_width),
                    Some(self.max_visible_items as f32 * 38.0 - 2.0),
                )
        } else {
            list
        };
        let mut popover = Popover::new(
            self.anchor,
            content,
            true,
            self.dismiss_id,
            ComponentPalette {
                raised: palette.surface,
                ..palette
            },
        )
        .width(self.width)
        .placement(self.placement)
        .animation_ms(self.motion.open_ms)
        .open_scale(self.open_scale);
        if let Some(duration) = self.close_animation_ms {
            popover = popover.close_animation_ms(duration);
        }
        if let Some(key) = self.view_key {
            popover = popover.view_key(key);
        }
        popover.render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DrawCommand, HitKind, Key, Point, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn menu_skips_static_and_disabled_rows_and_reuses_popover_motion() {
        let palette = ComponentPalette::neutral_dark();
        let entries = vec![
            DropdownMenuEntry::Heading("Proyecto".into()),
            DropdownMenuItem::new("open", "Abrir")
                .icon(Icon::Folder)
                .shortcut("Ctrl+O")
                .into(),
            DropdownMenuEntry::Separator,
            DropdownMenuItem::new("blocked", "No disponible")
                .disabled()
                .into(),
            DropdownMenuItem::new("settings", "Configurar").into(),
        ];
        let menu = DropdownMenu::new(
            Element::button("trigger", "Proyecto"),
            entries,
            true,
            "dismiss",
            palette,
        )
        .motion(MenuMotion {
            open_ms: 180,
            item_ms: 90,
        })
        .render();
        let mut tree = UiTree::new(menu);
        let frame = tree.layout(Size::new(360.0, 280.0));
        assert!(frame.regions.iter().any(|region| region.id == "open"));
        assert!(frame.regions.iter().any(|region| region.id == "settings"));
        assert!(!frame.regions.iter().any(|region| region.id == "blocked"));
        assert!(tree.focus("trigger"));
        tree.handle_event(UiEvent::Key {
            key: Key::ArrowDown,
            shift: false,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("open"));
        tree.handle_event(UiEvent::Key {
            key: Key::ArrowDown,
            shift: false,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("settings"));
        let option = frame
            .regions
            .iter()
            .find(|region| region.id == "settings")
            .unwrap();
        assert_eq!(
            frame.hit_test(Point::new(option.rect.x + 5.0, option.rect.y + 5.0)),
            Some("settings")
        );
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Escape,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked {
                id: "dismiss".into()
            }]
        );
    }

    #[test]
    fn menu_shortcut_uses_the_shared_capsule_and_annotation_stays_plain() {
        let palette = ComponentPalette::neutral_dark();
        let menu = DropdownMenu::new(
            Element::button("trigger", "Menú"),
            vec![
                DropdownMenuItem::new("new", "Nueva tarea")
                    .shortcut("Ctrl+N")
                    .into(),
                DropdownMenuItem::new("current", "Sesión")
                    .annotation("Abierta")
                    .into(),
            ],
            true,
            "dismiss",
            palette,
        )
        .render();
        let frame = UiTree::new(menu).layout(Size::new(360.0, 220.0));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, font_family: FontFamily::Monospace, .. }
                if text == "Ctrl+N"
        )));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { color, radius, rect }
                if (color.r, color.g, color.b) == {
                    let fill = crate::shortcut::cap_fill(palette.selected);
                    (fill.r, fill.g, fill.b)
                }
                    && *radius == crate::shortcut::RADIUS
                    && rect.height == crate::shortcut::HEIGHT
        )));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, font_family: FontFamily::Sans, .. }
                if text == "Abierta"
        )));
    }

    #[test]
    fn menu_motion_snaps_when_reduced_motion_is_enabled() {
        let menu = DropdownMenu::new(
            Element::button("trigger", "Abrir"),
            vec![DropdownMenuItem::new("option", "Opción").into()],
            true,
            "dismiss",
            ComponentPalette::neutral_dark(),
        )
        .motion(MenuMotion {
            open_ms: 2_000,
            item_ms: 2_000,
        })
        .render();
        let mut tree = UiTree::new(menu);
        tree.layout(Size::new(320.0, 220.0));
        assert!(tree.is_animating());
        tree.set_reduced_motion(true);
        tree.layout(Size::new(320.0, 220.0));
        assert!(!tree.is_animating());
    }

    #[test]
    fn submenu_opens_on_hover_and_stays_interactive() {
        let menu = DropdownMenu::new(
            Element::button("trigger", "Menú"),
            vec![
                DropdownMenuEntry::Heading("Menú principal".into()),
                DropdownMenuItem::new("settings", "Configuración")
                    .submenu(vec![
                        DropdownMenuItem::new("account", "Cuenta").into(),
                        DropdownMenuItem::new("privacy", "Privacidad").into(),
                    ])
                    .into(),
                DropdownMenuEntry::Separator,
                DropdownMenuItem::new("home", "Inicio").into(),
            ],
            true,
            "dismiss",
            ComponentPalette::neutral_dark(),
        )
        .render();
        let mut tree = UiTree::new(menu);
        let frame = tree.layout(Size::new(640.0, 360.0));
        assert!(!frame.regions.iter().any(|region| region.id == "account"));
        let trigger = frame
            .regions
            .iter()
            .find(|region| region.id == "settings")
            .unwrap();
        let point = Point::new(trigger.rect.x + 12.0, trigger.rect.y + 12.0);
        tree.handle_event(UiEvent::PointerMove(point));
        let frame = tree.layout(Size::new(640.0, 360.0));
        let child = frame
            .regions
            .iter()
            .find(|region| region.id == "account")
            .unwrap();
        assert!(child.rect.x >= trigger.rect.x + trigger.rect.width - 1.0);
        let child_point = Point::new(child.rect.x + 12.0, child.rect.y + 12.0);
        tree.handle_event(UiEvent::PointerMove(child_point));
        let frame = tree.layout(Size::new(640.0, 360.0));
        assert_eq!(frame.hit_test(child_point), Some("account"));
        tree.handle_event(UiEvent::PointerMove(Point::new(10.0, 300.0)));
        let frame = tree.layout(Size::new(640.0, 360.0));
        assert!(!frame.regions.iter().any(|region| region.id == "account"));
        assert!(tree.focus("settings"));
        tree.layout(Size::new(640.0, 360.0));
        tree.handle_event(UiEvent::Key {
            key: Key::ArrowRight,
            shift: false,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("account"));
        tree.handle_event(UiEvent::Key {
            key: Key::ArrowLeft,
            shift: false,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("settings"));
    }

    #[test]
    fn disabled_parent_never_opens_its_submenu() {
        let menu = DropdownMenu::new(
            Element::button("trigger", "Menú"),
            vec![DropdownMenuItem::new("settings", "Settings")
                .submenu(vec![DropdownMenuItem::new("account", "Account").into()])
                .disabled()
                .into()],
            true,
            "dismiss",
            ComponentPalette::neutral_dark(),
        )
        .render();
        let mut tree = UiTree::new(menu);
        let frame = tree.layout(Size::new(420.0, 280.0));
        assert!(!frame.regions.iter().any(|region| region.id == "settings"));
        let label = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text == "Settings" => Some(*position),
                _ => None,
            })
            .expect("disabled parent is visible");
        tree.handle_event(UiEvent::PointerMove(Point::new(
            label.x + 4.0,
            label.y + 4.0,
        )));
        let frame = tree.layout(Size::new(420.0, 280.0));
        assert!(!frame.regions.iter().any(|region| region.id == "account"));
    }

    #[test]
    fn headings_and_separators_count_toward_scroll_limit() {
        let menu = DropdownMenu::new(
            Element::button("trigger", "Menú"),
            vec![
                DropdownMenuEntry::Heading("General".into()),
                DropdownMenuItem::new("first", "Primero").into(),
                DropdownMenuEntry::Separator,
                DropdownMenuItem::new("last", "Último").into(),
            ],
            true,
            "dismiss",
            ComponentPalette::neutral_dark(),
        )
        .max_visible_items(2)
        .render();
        let frame = UiTree::new(menu).layout(Size::new(400.0, 320.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "dismiss:options"));
    }

    #[test]
    fn scrolling_menu_keeps_rows_clear_of_reserved_scrollbar() {
        let menu = DropdownMenu::new(
            Element::button("trigger", "Modelos"),
            vec![
                DropdownMenuItem::new("first", "Primero").into(),
                DropdownMenuItem::new("second", "Segundo").into(),
                DropdownMenuItem::new("third", "Tercero").into(),
                DropdownMenuItem::new("fourth", "Cuarto").into(),
            ],
            true,
            "dismiss",
            ComponentPalette::neutral_dark(),
        )
        .width(280.0)
        .max_visible_items(2)
        .render();
        let frame = UiTree::new(menu).layout(Size::new(480.0, 320.0));
        let scroll = frame
            .regions
            .iter()
            .find(|region| region.id == "dismiss:options" && region.kind == HitKind::Scroll)
            .expect("full-width scroll viewport");
        let rail = frame
            .regions
            .iter()
            .find(|region| region.id == "dismiss:options" && region.kind == HitKind::Scrollbar)
            .expect("reserved scrollbar");
        let first = frame
            .regions
            .iter()
            .find(|region| region.id == "first")
            .expect("first row");
        assert_eq!(scroll.rect.width, 280.0 - 12.0);
        assert_eq!(rail.rect.width, SCROLLBAR_GUTTER);
        assert_eq!(first.rect.width, scroll.rect.width - SCROLLBAR_GUTTER);
        assert!(first.rect.x + first.rect.width <= rail.rect.x);
    }

    #[test]
    fn annotation_only_uses_space_in_its_own_row() {
        let menu = DropdownMenu::new(
            Element::button("trigger", "Modelos"),
            vec![
                DropdownMenuItem::new("plain", "Modelo de prueba largo").into(),
                DropdownMenuItem::new("annotated", "Otro")
                    .annotation("En tu cuenta")
                    .into(),
            ],
            true,
            "dismiss",
            ComponentPalette::neutral_dark(),
        )
        .width(225.0)
        .render();
        let frame = UiTree::new(menu).layout(Size::new(400.0, 260.0));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Modelo de prueba largo"
        )));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "En tu cuenta"
        )));
    }

    #[test]
    fn narrow_rows_keep_labels_annotations_and_shortcuts_inside_hover_bounds() {
        let menu = DropdownMenu::new(
            Element::button("trigger", "Abrir"),
            vec![
                DropdownMenuItem::new("annotated", "Descripción del modelo muy extensa")
                    .icon(Icon::Folder)
                    .annotation("Estado de cuenta demasiado extenso")
                    .selected(true)
                    .into(),
                DropdownMenuItem::new("shortcut", "Otra descripción larga")
                    .shortcut("Ctrl+Shift+Alt+N")
                    .into(),
                DropdownMenuItem::new("extra", "Tercera opción").into(),
            ],
            true,
            "dismiss",
            ComponentPalette::neutral_dark(),
        )
        .width(184.0)
        .max_visible_items(2)
        .render();
        let mut tree = UiTree::new(menu);
        tree.set_reduced_motion(true);
        tree.set_text_measurer(|text, _, _| text.chars().count() as f32 * 9.0);
        let frame = tree.layout(Size::new(400.0, 320.0));

        for id in ["annotated", "shortcut"] {
            let row = frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .expect("visible menu row");
            let texts: Vec<_> = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, position, .. }
                        if position.y >= row.rect.y
                            && position.y < row.rect.y + row.rect.height =>
                    {
                        Some((text, position))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                texts.len(),
                2,
                "label and trailing hint stay visible in {id}"
            );
            assert!(texts.iter().any(|(text, _)| text.ends_with('…')));
            for (text, position) in texts {
                let right = position.x + text.chars().count() as f32 * 9.0;
                assert!(position.x >= row.rect.x + 7.0, "{id}: {text}");
                assert!(right <= row.rect.x + row.rect.width - 7.0, "{id}: {text}");
            }
        }
    }
}
