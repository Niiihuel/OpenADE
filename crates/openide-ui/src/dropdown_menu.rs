//! Shared action menu built on the retained popover and color transitions.

use crate::components::{lighter, Component, ComponentPalette, ControlState, Popover};
use crate::{Element, FontFamily, Icon, MenuMotion, PopoverPlacement, ScrollArea, ICON_SIZE};

/// One action in a menu. The caller handles `action_id` from `UiAction`.
pub struct DropdownMenuItem {
    pub action_id: String,
    pub label: String,
    pub icon: Option<Icon>,
    pub shortcut: Option<String>,
    pub selected: bool,
    pub state: ControlState,
}

impl DropdownMenuItem {
    pub fn new(action_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            action_id: action_id.into(),
            label: label.into(),
            icon: None,
            shortcut: None,
            selected: false,
            state: ControlState::Enabled,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
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
}

/// Static entries do not enter keyboard focus order.
pub enum DropdownMenuEntry {
    Item(DropdownMenuItem),
    Heading(String),
    Separator,
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

        let item_count = self
            .entries
            .iter()
            .filter(|entry| matches!(entry, DropdownMenuEntry::Item(_)))
            .count();
        let reserve_icon = self
            .entries
            .iter()
            .any(|entry| matches!(entry, DropdownMenuEntry::Item(item) if item.icon.is_some()));
        let reserve_check = self
            .entries
            .iter()
            .any(|entry| matches!(entry, DropdownMenuEntry::Item(item) if item.selected));
        let shortcut_width = self
            .entries
            .iter()
            .filter_map(|entry| match entry {
                DropdownMenuEntry::Item(item) => item.shortcut.as_ref(),
                _ => None,
            })
            .map(|shortcut| shortcut.chars().count() as f32 * 7.0 + 8.0)
            .fold(0.0, f32::max)
            .min(100.0);
        let row_width = self.width - 12.0;
        let palette = self.palette;
        let rows = self
            .entries
            .into_iter()
            .map(|entry| match entry {
                DropdownMenuEntry::Heading(label) => Element::text(label)
                    .foreground(palette.muted)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0)
                    .padding(8.0)
                    .size(Some(row_width), Some(28.0)),
                DropdownMenuEntry::Separator => Element::spacer()
                    .background(palette.border)
                    .size(Some(row_width), Some(1.0)),
                DropdownMenuEntry::Item(item) => {
                    let enabled = item.state == ControlState::Enabled;
                    let mut children = Vec::with_capacity(4);
                    if reserve_icon {
                        children.push(
                            if let Some(icon) = item.icon {
                                Element::icon(icon).foreground(palette.muted)
                            } else {
                                Element::spacer()
                            }
                            .size(Some(ICON_SIZE), Some(18.0)),
                        );
                    }
                    children.push(
                        Element::truncated_text(item.label)
                            .foreground(if enabled { palette.text } else { palette.muted })
                            .font_family(FontFamily::Sans)
                            .font_size(13.0)
                            .flex(1.0)
                            .size(None, Some(18.0)),
                    );
                    if shortcut_width > 0.0 {
                        children.push(
                            Element::text(item.shortcut.unwrap_or_default())
                                .foreground(palette.muted)
                                .font_family(FontFamily::Sans)
                                .font_size(11.0)
                                .size(Some(shortcut_width), Some(18.0)),
                        );
                    }
                    if reserve_check {
                        children.push(
                            if item.selected {
                                Element::icon(Icon::Check).foreground(palette.text)
                            } else {
                                Element::spacer()
                            }
                            .size(Some(ICON_SIZE), Some(18.0)),
                        );
                    }
                    let fill = if item.selected {
                        palette.selected
                    } else {
                        palette.raised
                    };
                    let visual = Element::row(children).gap(9.0);
                    let row = if enabled {
                        Element::clickable(item.action_id, visual)
                    } else {
                        Element::panel(visual)
                    };
                    row.background(fill)
                        .hover_background(if item.selected {
                            lighter(fill, 8)
                        } else {
                            palette.hover
                        })
                        .pressed_background(if item.selected {
                            lighter(fill, 14)
                        } else {
                            palette.selected
                        })
                        .transition_ms(self.motion.item_ms)
                        .padding(9.0)
                        .radius(7.0)
                        .size(Some(row_width), Some(36.0))
                }
            })
            .collect();
        let list = Element::column(rows).gap(2.0);
        let content = if item_count > self.max_visible_items {
            ScrollArea::new(format!("{}:options", self.dismiss_id), list)
                .tint(palette.muted)
                .render()
                .size(
                    Some(row_width),
                    Some(self.max_visible_items as f32 * 38.0 - 2.0),
                )
        } else {
            list
        };
        Popover::new(self.anchor, content, true, self.dismiss_id, palette)
            .width(self.width)
            .placement(self.placement)
            .animation_ms(self.motion.open_ms)
            .render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Key, Point, Size, UiAction, UiEvent, UiTree};

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
}
