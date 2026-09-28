//! Reusable tree rows and hierarchy guides for file, project, and task trees.
//!
//! The caller owns the tree model and action element. This module owns only the
//! presentation of depth, continuation lines, hit area, and row spacing.

use crate::{Color, Component, ComponentPalette, ControlSize, Element, ICON_SIZE};

const DEFAULT_STEP: f32 = 12.0;
const GUIDE_OFFSET: f32 = 7.0;
const GUIDE_WIDTH: f32 = 1.0;

/// Guide state for one visible row in a depth-first tree. A depth-zero row has
/// no guides. At depth `n`, there are `n - 1` ancestor lanes plus the row's
/// own lane, which ends at its icon and continues below when a sibling follows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeGuides {
    pub depth: usize,
    pub ancestor_continuations: Vec<bool>,
    pub next_sibling: bool,
}

impl TreeGuides {
    pub fn root() -> Self {
        Self {
            depth: 0,
            ancestor_continuations: Vec::new(),
            next_sibling: false,
        }
    }
}

/// Compute guides from visible depth-first row depths. A shallower row closes
/// deeper sibling chains, so a new branch never inherits lines from a prior
/// parent. The result can be reused with any tree data model.
pub fn guides_for_depths(depths: &[usize]) -> Vec<TreeGuides> {
    let mut next_sibling = vec![false; depths.len()];
    let mut next_at_depth: Vec<Option<usize>> = Vec::new();
    for (index, &depth) in depths.iter().enumerate().rev() {
        if next_at_depth.len() <= depth {
            next_at_depth.resize(depth + 1, None);
        }
        next_sibling[index] = next_at_depth[depth].is_some();
        next_at_depth.truncate(depth + 1);
        next_at_depth[depth] = Some(index);
    }

    let mut ancestors: Vec<Option<usize>> = Vec::new();
    let mut result = Vec::with_capacity(depths.len());
    for (index, &depth) in depths.iter().enumerate() {
        ancestors.truncate(depth);
        let ancestor_continuations = (1..depth)
            .map(|ancestor_depth| {
                ancestors
                    .get(ancestor_depth)
                    .and_then(|index| *index)
                    .is_some_and(|index| next_sibling[index])
            })
            .collect();
        result.push(TreeGuides {
            depth,
            ancestor_continuations,
            next_sibling: next_sibling[index],
        });
        ancestors.resize(depth, None);
        ancestors.push(Some(index));
    }
    result
}

/// A full-width action row assembled from caller-owned content. The icon,
/// title, and trailing element can be any `Element`; nested applications may
/// share this geometry without sharing file-explorer-specific data or actions.
pub struct TreeRow<F: FnOnce(Element) -> Element> {
    action: F,
    icon: Element,
    title: Element,
    trailing: Option<Element>,
    guides: TreeGuides,
    palette: ComponentPalette,
    guide_color: Color,
    width: Option<f32>,
    height: f32,
    max_guide_depth: usize,
    selected: bool,
}

impl<F: FnOnce(Element) -> Element> TreeRow<F> {
    pub fn new(
        action: F,
        icon: Element,
        title: Element,
        guides: TreeGuides,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            action,
            icon,
            title,
            trailing: None,
            guides,
            palette,
            guide_color: palette.border,
            width: None,
            height: ControlSize::Compact.height(),
            max_guide_depth: 6,
            selected: false,
        }
    }

    pub fn trailing(mut self, element: Element) -> Self {
        self.trailing = Some(element);
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width.max(0.0));
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        if height.is_finite() && height >= 18.0 {
            self.height = height;
        }
        self
    }

    pub fn max_guide_depth(mut self, depth: usize) -> Self {
        self.max_guide_depth = depth;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn guide_color(mut self, color: Color) -> Self {
        self.guide_color = color;
        self
    }
}

impl<F: FnOnce(Element) -> Element> Component for TreeRow<F> {
    fn render(self) -> Element {
        let visible_depth = self.guides.depth.min(self.max_guide_depth);
        let hidden_depth = self.guides.depth - visible_depth;
        let mut gutter = Vec::with_capacity(visible_depth + 1);
        gutter.push(Element::spacer().size(Some(4.0), Some(self.height)));
        for lane in 0..visible_depth {
            let last = lane + 1 == visible_depth;
            let continues = if last {
                self.guides.next_sibling
            } else {
                self.guides
                    .ancestor_continuations
                    .get(hidden_depth + lane)
                    .copied()
                    .unwrap_or(false)
            };
            gutter.push(guide_lane(self.height, self.guide_color, last, continues));
        }
        let gutter_width = 4.0 + visible_depth as f32 * DEFAULT_STEP;
        let mut children = Vec::with_capacity(visible_depth + 7);
        children.push(
            Element::row(gutter)
                .gap(0.0)
                .size(Some(gutter_width), Some(self.height)),
        );
        children.push(Element::panel(self.icon).size(Some(ICON_SIZE), Some(self.height)));
        children.push(Element::spacer().size(Some(6.0), Some(self.height)));
        children.push(self.title.min_size(0.0, 0.0).flex(1.0));
        if let Some(trailing) = self.trailing {
            children.push(Element::spacer().size(Some(6.0), Some(self.height)));
            children.push(Element::panel(trailing).size(None, Some(self.height)));
        }
        children.push(Element::spacer().size(Some(8.0), Some(self.height)));
        let content = Element::row(children)
            .gap(0.0)
            .size(self.width, Some(self.height));
        let mut row = (self.action)(content)
            .size(self.width, Some(self.height))
            .hover_background(if self.selected {
                self.palette.selected
            } else {
                self.palette.hover
            })
            .pressed_background(self.palette.selected)
            .radius(6.0);
        if self.selected {
            row = row.background(self.palette.selected);
        }
        row
    }
}

fn guide_lane(height: f32, color: Color, elbow: bool, continues: bool) -> Element {
    if !elbow {
        return if continues {
            vertical_segment(height, color)
        } else {
            Element::spacer().size(Some(DEFAULT_STEP), Some(height))
        };
    }
    let upper = (height / 2.0).floor();
    let lower = height - upper - GUIDE_WIDTH;
    Element::column(vec![
        vertical_segment(upper, color),
        Element::row(vec![
            Element::spacer().size(Some(GUIDE_OFFSET), Some(GUIDE_WIDTH)),
            Element::spacer()
                .size(Some(DEFAULT_STEP - GUIDE_OFFSET), Some(GUIDE_WIDTH))
                .background(color),
        ])
        .gap(0.0)
        .size(Some(DEFAULT_STEP), Some(GUIDE_WIDTH)),
        if continues {
            vertical_segment(lower, color)
        } else {
            Element::spacer().size(Some(DEFAULT_STEP), Some(lower))
        },
    ])
    .gap(0.0)
    .size(Some(DEFAULT_STEP), Some(height))
}

fn vertical_segment(height: f32, color: Color) -> Element {
    Element::row(vec![
        Element::spacer().size(Some(GUIDE_OFFSET), Some(height)),
        Element::spacer()
            .size(Some(GUIDE_WIDTH), Some(height))
            .background(color),
        Element::spacer().size(
            Some(DEFAULT_STEP - GUIDE_OFFSET - GUIDE_WIDTH),
            Some(height),
        ),
    ])
    .gap(0.0)
    .size(Some(DEFAULT_STEP), Some(height))
}

#[cfg(test)]
mod tests {
    use crate::{DrawCommand, Icon, Size, UiTree};

    use super::*;

    #[test]
    fn guides_follow_depth_first_branches_without_crossing_roots() {
        let guides = guides_for_depths(&[0, 1, 2, 2, 1, 0, 1]);
        assert_eq!(
            guides[0],
            TreeGuides {
                depth: 0,
                ancestor_continuations: vec![],
                next_sibling: true
            }
        );
        assert_eq!(
            guides[1],
            TreeGuides {
                depth: 1,
                ancestor_continuations: vec![],
                next_sibling: true
            }
        );
        assert_eq!(
            guides[2],
            TreeGuides {
                depth: 2,
                ancestor_continuations: vec![true],
                next_sibling: true
            }
        );
        assert_eq!(
            guides[3],
            TreeGuides {
                depth: 2,
                ancestor_continuations: vec![true],
                next_sibling: false
            }
        );
        assert_eq!(
            guides[4],
            TreeGuides {
                depth: 1,
                ancestor_continuations: vec![],
                next_sibling: false
            }
        );
        assert_eq!(
            guides[5],
            TreeGuides {
                depth: 0,
                ancestor_continuations: vec![],
                next_sibling: false
            }
        );
        assert_eq!(
            guides[6],
            TreeGuides {
                depth: 1,
                ancestor_continuations: vec![],
                next_sibling: false
            }
        );
    }

    #[test]
    fn tree_row_keeps_one_hit_target_and_draws_terminating_elbow() {
        let palette = ComponentPalette::neutral_dark();
        let row = TreeRow::new(
            |content| Element::clickable("tree:item", content),
            Element::icon(Icon::Folder).size(Some(16.0), Some(16.0)),
            Element::truncated_text("Nested item"),
            TreeGuides {
                depth: 1,
                ancestor_continuations: vec![],
                next_sibling: false,
            },
            palette,
        )
        .trailing(Element::icon(Icon::ChevronRight).size(Some(16.0), Some(16.0)))
        .width(180.0)
        .render();
        let frame = UiTree::new(row).layout(Size::new(180.0, 28.0));
        assert_eq!(
            frame
                .regions
                .iter()
                .filter(|region| region.id == "tree:item")
                .count(),
            1
        );
        let icons: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Icon { rect, .. } => Some(*rect),
                _ => None,
            })
            .collect();
        assert_eq!(icons.len(), 2);
        assert!(icons
            .iter()
            .all(|rect| rect.y == 6.0 && rect.width == 16.0 && rect.height == 16.0));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, color, .. }
                if *color == palette.border && rect.width == DEFAULT_STEP - GUIDE_OFFSET && rect.height == GUIDE_WIDTH
        )));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, color, .. }
                if *color == palette.border && rect.width == GUIDE_WIDTH && rect.height == 14.0
        )));
        assert!(!frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, color, .. }
                if *color == palette.border && rect.width == GUIDE_WIDTH && rect.y >= 15.0
        )));
    }

    #[test]
    fn continuing_ancestor_lane_paints_through_the_entire_row() {
        let palette = ComponentPalette::neutral_dark();
        let row = TreeRow::new(
            |content| Element::clickable("tree:grandchild", content),
            Element::icon(Icon::Code).size(Some(16.0), Some(16.0)),
            Element::text("grandchild"),
            TreeGuides {
                depth: 2,
                ancestor_continuations: vec![true],
                next_sibling: false,
            },
            palette,
        )
        .width(180.0)
        .render();
        let frame = UiTree::new(row).layout(Size::new(180.0, 28.0));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, color, .. }
                if *color == palette.border && rect.width == GUIDE_WIDTH && rect.height == 28.0
        )));
        let vertical_positions: Vec<f32> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRect { rect, color, .. }
                    if *color == palette.border && rect.width == GUIDE_WIDTH =>
                {
                    Some(rect.x)
                }
                _ => None,
            })
            .collect();
        assert!(vertical_positions.iter().any(|x| (*x - 11.0).abs() < 0.01));
        assert!(vertical_positions.iter().any(|x| (*x - 23.0).abs() < 0.01));
    }

    #[test]
    fn ended_ancestor_lane_does_not_leak_into_a_new_branch() {
        let palette = ComponentPalette::neutral_dark();
        let row = TreeRow::new(
            |content| Element::clickable("tree:last-grandchild", content),
            Element::icon(Icon::Code).size(Some(16.0), Some(16.0)),
            Element::text("leaf"),
            TreeGuides {
                depth: 2,
                ancestor_continuations: vec![false],
                next_sibling: false,
            },
            palette,
        )
        .width(180.0)
        .render();
        let frame = UiTree::new(row).layout(Size::new(180.0, 28.0));
        let vertical_positions: Vec<f32> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRect { rect, color, .. }
                    if *color == palette.border && rect.width == GUIDE_WIDTH =>
                {
                    Some(rect.x)
                }
                _ => None,
            })
            .collect();
        assert_eq!(vertical_positions, vec![23.0]);
    }
}
