use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use unicode_segmentation::UnicodeSegmentation;

use crate::element::UniformList;
use crate::motion::{ColorTransition, ScalarTransition};
use crate::typography::{truncate_measured, wrap_measured};
use crate::{
    BoxShadow, Color, Element, ElementKind, FontFamily, Icon, Point, PopoverPlacement, Rect, Size,
    Style, TextSpan, TooltipPlacement, TooltipStyle, DEFAULT_FONT_SIZE, ICON_SIZE,
};

const TOOLTIP_HOVER_DELAY: Duration = Duration::from_millis(350);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontMetrics {
    /// Advance of one monospace character in logical pixels.
    pub char_width: f32,
    pub line_height: f32,
}

impl Default for FontMetrics {
    fn default() -> Self {
        Self {
            char_width: 8.0,
            line_height: 18.0,
        }
    }
}

impl FontMetrics {
    fn at_size(self, size: f32) -> Self {
        let scale = size / DEFAULT_FONT_SIZE;
        Self {
            char_width: self.char_width * scale,
            line_height: self.line_height * scale,
        }
    }
}

type TextMeasurer = dyn Fn(&str, f32, FontFamily) -> f32 + Send + Sync;

fn measured_width(
    measurer: Option<&TextMeasurer>,
    text: &str,
    size: f32,
    family: FontFamily,
    fallback_char_width: f32,
) -> f32 {
    let fallback = text.chars().count() as f32 * fallback_char_width;
    measurer
        .map(|measure| measure(text, size, family))
        .filter(|width| width.is_finite() && *width >= 0.0)
        .unwrap_or(fallback)
}

#[derive(Clone, Debug)]
struct FormattedFragment {
    text: String,
    color: Color,
    font_size: f32,
    font_family: FontFamily,
    background: Option<Color>,
    width: f32,
    line_height: f32,
}

#[derive(Debug)]
struct FormattedLine {
    fragments: Vec<FormattedFragment>,
    width: f32,
    height: f32,
}

impl FormattedLine {
    fn new(height: f32) -> Self {
        Self {
            fragments: Vec::new(),
            width: 0.0,
            height,
        }
    }
}

fn same_span_style(fragment: &FormattedFragment, span: &TextSpan) -> bool {
    fragment.color == span.color
        && fragment.font_size == span.font_size
        && fragment.font_family == span.font_family
        && fragment.background == span.background
}

fn formatted_advance(
    line: &FormattedLine,
    span: &TextSpan,
    text: &str,
    measure: &mut impl FnMut(&str, &TextSpan) -> f32,
) -> f32 {
    let inset = if span.background.is_some() { 6.0 } else { 0.0 };
    if let Some(last) = line
        .fragments
        .last()
        .filter(|last| last.text.len() <= 256 && same_span_style(last, span))
    {
        let mut combined = last.text.clone();
        combined.push_str(text);
        measure(&combined, span) + inset - last.width
    } else {
        measure(text, span) + inset
    }
}

fn push_formatted_fragment(
    line: &mut FormattedLine,
    span: &TextSpan,
    text: &str,
    measure: &mut impl FnMut(&str, &TextSpan) -> f32,
    line_height: &mut impl FnMut(&TextSpan) -> f32,
) {
    let inset = if span.background.is_some() { 6.0 } else { 0.0 };
    let height = line_height(span);
    if let Some(last) = line
        .fragments
        .last_mut()
        .filter(|last| last.text.len() <= 256 && same_span_style(last, span))
    {
        let previous_width = last.width;
        last.text.push_str(text);
        last.width = measure(&last.text, span) + inset;
        line.width += last.width - previous_width;
        line.height = line.height.max(height);
        return;
    }
    let width = measure(text, span) + inset;
    line.fragments.push(FormattedFragment {
        text: text.to_owned(),
        color: span.color,
        font_size: span.font_size,
        font_family: span.font_family,
        background: span.background,
        width,
        line_height: height,
    });
    line.width += width;
    line.height = line.height.max(height);
}

/// Flow styled runs in one paragraph. Tokens share a line until the painter's
/// measured width reaches the available width; oversized words are split only
/// at Unicode grapheme boundaries. A small background inset is included in
/// measurement and paint so inline code does not overlap adjacent text.
fn layout_formatted(
    spans: &[TextSpan],
    max_width: f32,
    default_line_height: f32,
    mut measure: impl FnMut(&str, &TextSpan) -> f32,
    mut line_height: impl FnMut(&TextSpan) -> f32,
) -> Vec<FormattedLine> {
    let limit = if max_width.is_finite() {
        max_width.max(0.0)
    } else {
        f32::INFINITY
    };
    let mut lines = Vec::new();
    let mut current = FormattedLine::new(default_line_height);
    for span in spans {
        for (paragraph_index, paragraph) in span.text.split('\n').enumerate() {
            if paragraph_index > 0 {
                lines.push(std::mem::replace(
                    &mut current,
                    FormattedLine::new(default_line_height),
                ));
            }
            let tokens: Vec<&str> = if span.background.is_some() {
                vec![paragraph]
            } else {
                paragraph
                    .split_word_bound_indices()
                    .map(|(_, token)| token)
                    .collect()
            };
            for token in tokens.into_iter().filter(|token| !token.is_empty()) {
                let inset = if span.background.is_some() { 6.0 } else { 0.0 };
                let token_advance = formatted_advance(&current, span, token, &mut measure);
                if current.width + token_advance > limit && !current.fragments.is_empty() {
                    lines.push(std::mem::replace(
                        &mut current,
                        FormattedLine::new(default_line_height),
                    ));
                }
                if measure(token, span) + inset <= limit || token.graphemes(true).count() == 1 {
                    push_formatted_fragment(
                        &mut current,
                        span,
                        token,
                        &mut measure,
                        &mut line_height,
                    );
                    continue;
                }
                let mut piece = String::new();
                for grapheme in token.graphemes(true) {
                    let mut candidate = piece.clone();
                    candidate.push_str(grapheme);
                    if !piece.is_empty() && measure(&candidate, span) + inset > limit {
                        push_formatted_fragment(
                            &mut current,
                            span,
                            &piece,
                            &mut measure,
                            &mut line_height,
                        );
                        lines.push(std::mem::replace(
                            &mut current,
                            FormattedLine::new(default_line_height),
                        ));
                        piece.clear();
                    }
                    piece.push_str(grapheme);
                }
                if !piece.is_empty() {
                    push_formatted_fragment(
                        &mut current,
                        span,
                        &piece,
                        &mut measure,
                        &mut line_height,
                    );
                }
            }
        }
    }
    lines.push(current);
    lines
}

#[derive(Clone, Debug, PartialEq)]
pub enum DrawCommand {
    Shadow {
        rect: Rect,
        shadow: BoxShadow,
        radius: f32,
    },
    FillRect {
        rect: Rect,
        color: Color,
        radius: f32,
    },
    FillVerticalGradient {
        rect: Rect,
        top: Color,
        bottom: Color,
    },
    StrokeRect {
        rect: Rect,
        color: Color,
        width: f32,
        radius: f32,
    },
    FillTriangle {
        points: [Point; 3],
        color: Color,
    },
    Text {
        position: Point,
        text: String,
        color: Color,
        font_size: f32,
        font_family: FontFamily,
    },
    Icon {
        rect: Rect,
        icon: Icon,
        tint: Color,
    },
    PushClip(Rect),
    PopClip,
}

/// Move and fade a floating surface's paint only. Its layout, pointer regions,
/// scroll geometry, and focus targets stay at their final coordinates, so a
/// short entrance cannot make a menu's click target drift under the pointer.
fn animate_popover_command(command: &mut DrawCommand, opacity: f32, slide_y: f32) {
    fn fade(color: &mut Color, opacity: f32) {
        color.a = (f32::from(color.a) * opacity.clamp(0.0, 1.0)).round() as u8;
    }
    match command {
        DrawCommand::Shadow { rect, shadow, .. } => {
            rect.y += slide_y;
            fade(&mut shadow.color, opacity);
        }
        DrawCommand::FillRect { rect, color, .. } | DrawCommand::StrokeRect { rect, color, .. } => {
            rect.y += slide_y;
            fade(color, opacity);
        }
        DrawCommand::FillVerticalGradient { rect, top, bottom } => {
            rect.y += slide_y;
            fade(top, opacity);
            fade(bottom, opacity);
        }
        DrawCommand::FillTriangle { points, color } => {
            for point in points {
                point.y += slide_y;
            }
            fade(color, opacity);
        }
        DrawCommand::Text {
            position, color, ..
        } => {
            position.y += slide_y;
            fade(color, opacity);
        }
        DrawCommand::Icon { rect, tint, .. } => {
            rect.y += slide_y;
            fade(tint, opacity);
        }
        DrawCommand::PushClip(rect) => rect.y += slide_y,
        DrawCommand::PopClip => {}
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HitKind {
    Button,
    TextInput,
    TextArea,
    /// A non-activating floating surface that blocks backdrop dismissal.
    Surface,
    Scroll,
    Scrollbar,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HitRegion {
    pub id: String,
    pub rect: Rect,
    pub kind: HitKind,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub commands: Vec<DrawCommand>,
    pub regions: Vec<HitRegion>,
    scroll_extents: HashMap<String, f32>,
    scrollbars: HashMap<String, ScrollbarGeometry>,
    scroll_focus_targets: HashMap<String, ScrollFocusTarget>,
    text_blocks: Vec<SelectableTextBlock>,
    popover_focus: Vec<PopoverFocusScope>,
    tab_focus: Vec<TabFocusScope>,
    virtual_lists: HashMap<String, VirtualListFocus>,
}

#[derive(Clone, Debug, PartialEq)]
struct VirtualListFocus {
    first_visible: usize,
    end_visible: usize,
    row_count: usize,
    rows: Vec<(usize, Vec<(String, HitKind)>)>,
}

impl VirtualListFocus {
    fn focusables(&self) -> impl Iterator<Item = &(String, HitKind)> {
        self.rows.iter().flat_map(|(_, focusables)| focusables)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct PopoverFocusScope {
    dismiss_id: String,
    trigger_id: Option<String>,
    focusables: Vec<(String, HitKind)>,
}

#[derive(Clone, Debug, PartialEq)]
struct ScrollFocusTarget {
    scroll_id: String,
    rect: Rect,
    viewport: Rect,
    layout_offset: f32,
}

struct ScrollFocusContext {
    id: String,
    viewport: Rect,
    offset: f32,
}

#[derive(Clone, Debug, PartialEq)]
struct TabFocusScope {
    action_ids: Vec<String>,
}

#[derive(Debug)]
struct TabIndicatorState {
    selected_id: String,
    x: ScalarTransition,
    width: ScalarTransition,
}

/// Final-position paint for a floating surface. It is deliberately separate
/// from regions and focus scopes so closing can animate visually without
/// leaving any invisible interactive target behind.
struct PopoverSnapshot {
    commands: Vec<DrawCommand>,
    animation_ms: u16,
    slide_sign: f32,
}

struct PopoverExit {
    paint: PopoverSnapshot,
    opacity: ScalarTransition,
}

#[derive(Clone, Debug, PartialEq)]
struct SelectableTextRun {
    start: usize,
    end: usize,
    text: String,
    rect: Rect,
    visible: Option<Rect>,
    font_size: f32,
    font_family: FontFamily,
}

#[derive(Clone, Debug, PartialEq)]
struct SelectableTextBlock {
    source: String,
    runs: Vec<SelectableTextRun>,
}

impl SelectableTextBlock {
    fn contains(&self, point: Point) -> bool {
        self.runs
            .iter()
            .any(|run| run.visible.is_some_and(|rect| rect.contains(point)))
    }

    fn offset_at(
        &self,
        point: Point,
        measure: Option<&TextMeasurer>,
        metrics: FontMetrics,
        sans_metrics: FontMetrics,
    ) -> usize {
        let Some(row) = self.runs.iter().min_by(|left, right| {
            vertical_distance(left.rect, point.y).total_cmp(&vertical_distance(right.rect, point.y))
        }) else {
            return 0;
        };
        let run = self
            .runs
            .iter()
            .filter(|run| run.rect.y == row.rect.y)
            .min_by(|left, right| {
                horizontal_distance(left.rect, point.x)
                    .total_cmp(&horizontal_distance(right.rect, point.x))
            })
            .unwrap_or(row);
        let char_width = match run.font_family {
            FontFamily::Monospace => metrics,
            FontFamily::Sans => sans_metrics,
        }
        .at_size(run.font_size)
        .char_width;
        let x = (point.x - run.rect.x).max(0.0);
        let offset = run.start
            + nearest_grapheme(&run.text, x, |text| {
                measured_width(measure, text, run.font_size, run.font_family, char_width)
            });
        nearest_source_grapheme_boundary(&self.source, offset)
    }
}

fn horizontal_distance(rect: Rect, x: f32) -> f32 {
    if x < rect.x {
        rect.x - x
    } else {
        (x - (rect.x + rect.width)).max(0.0)
    }
}

fn vertical_distance(rect: Rect, y: f32) -> f32 {
    if y < rect.y {
        rect.y - y
    } else {
        (y - (rect.y + rect.height)).max(0.0)
    }
}

fn nearest_source_grapheme_boundary(source: &str, char_offset: usize) -> usize {
    let mut previous = 0;
    for grapheme in source.graphemes(true) {
        let next = previous + grapheme.chars().count();
        if char_offset <= next {
            return if char_offset - previous <= next - char_offset {
                previous
            } else {
                next
            };
        }
        previous = next;
    }
    previous
}

#[derive(Clone, Debug)]
struct StaticSelection {
    block: usize,
    source: String,
    anchor: usize,
    cursor: usize,
    dragging: bool,
}

impl StaticSelection {
    fn range(&self) -> Option<(usize, usize)> {
        (self.anchor != self.cursor)
            .then_some((self.anchor.min(self.cursor), self.anchor.max(self.cursor)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScrollbarGeometry {
    track: Rect,
    thumb: Rect,
    max_offset: f32,
    placement: crate::ScrollbarPlacement,
}

impl ScrollbarGeometry {
    fn new(
        viewport: Rect,
        max_offset: f32,
        offset: f32,
        appearance: crate::ScrollbarAppearance,
    ) -> Option<Self> {
        if viewport.width <= 0.0 || viewport.height <= 0.0 || max_offset <= 0.0 {
            return None;
        }
        let gutter = appearance.effective_rail_width(viewport.width);
        let track = Rect::new(
            viewport.x + viewport.width - gutter,
            viewport.y,
            gutter,
            viewport.height,
        );
        let fraction = viewport.height / (viewport.height + max_offset);
        let minimum = (if appearance.min_thumb_length.is_finite() {
            appearance.min_thumb_length
        } else {
            crate::ScrollbarAppearance::default().min_thumb_length
        })
        .max(1.0)
        .min((track.height / 2.0).max(1.0));
        let thumb_height = (track.height * fraction).clamp(minimum, track.height);
        let travel = track.height - thumb_height;
        let thumb_width = (if appearance.thumb_width.is_finite() {
            appearance.thumb_width
        } else {
            crate::ScrollbarAppearance::default().thumb_width
        })
        .max(1.0)
        .min((gutter - 2.0).max(1.0))
        .min(gutter);
        let thumb = Rect::new(
            track.x + (gutter - thumb_width) / 2.0,
            track.y + offset.clamp(0.0, max_offset) / max_offset * travel,
            thumb_width,
            thumb_height,
        );
        Some(Self {
            track,
            thumb,
            max_offset,
            placement: appearance.placement,
        })
    }

    fn offset_at(self, pointer_y: f32, grab_y: f32) -> f32 {
        let travel = self.track.height - self.thumb.height;
        if travel <= 0.0 {
            return 0.0;
        }
        ((pointer_y - self.track.y - grab_y) / travel).clamp(0.0, 1.0) * self.max_offset
    }
}

struct ScrollDrag {
    id: String,
    grab_y: f32,
}

impl Frame {
    /// Returns the topmost interactive element at a point.
    pub fn hit_test(&self, point: Point) -> Option<&str> {
        self.hit_region(point).map(|region| region.id.as_str())
    }

    pub fn hit_region(&self, point: Point) -> Option<&HitRegion> {
        for (index, region) in self.regions.iter().enumerate().rev() {
            if !region.rect.contains(point) {
                continue;
            }
            if region.kind == HitKind::Scrollbar
                && self.scrollbars.get(&region.id).is_some_and(|bar| {
                    bar.placement == crate::ScrollbarPlacement::Overlay
                        && !bar.thumb.contains(point)
                })
                && self.regions[..index].iter().any(|underneath| {
                    underneath.rect.contains(point)
                        && matches!(
                            underneath.kind,
                            HitKind::Button | HitKind::TextInput | HitKind::TextArea
                        )
                })
            {
                // A transparent overlay track should not swallow controls at
                // the content edge. The visible thumb still wins for dragging.
                continue;
            }
            return Some(region);
        }
        None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Escape,
    Tab,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Home,
    End,
    Backspace,
    Delete,
    Enter,
    Space,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiEvent {
    PointerMove(Point),
    PointerLeave,
    PointerDown(Point),
    PointerUp(Point),
    Scroll { position: Point, delta_y: f32 },
    Text(String),
    Key { key: Key, shift: bool, ctrl: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiAction {
    Clicked { id: String },
    InputChanged { id: String, value: String },
    Submitted { id: String, value: String },
    CopyRequested { text: String },
}

struct InputState {
    value: String,
    cursor: usize,
    anchor: Option<usize>,
    view_start: usize,
    content_x: f32,
    content_y: f32,
    char_width: f32,
    content_width: f32,
    font_size: f32,
    font_family: FontFamily,
    measurer: Option<Arc<TextMeasurer>>,
    line_height: f32,
    visible_lines: usize,
    scroll_line: usize,
    keep_cursor_visible: bool,
    desired_column: Option<usize>,
}

impl InputState {
    fn new(value: String) -> Self {
        let cursor = value.chars().count();
        Self {
            value,
            cursor,
            anchor: None,
            view_start: 0,
            content_x: 0.0,
            content_y: 0.0,
            char_width: FontMetrics::default().char_width,
            content_width: 8.0,
            font_size: DEFAULT_FONT_SIZE,
            font_family: FontFamily::Monospace,
            measurer: None,
            line_height: FontMetrics::default().line_height,
            visible_lines: 1,
            scroll_line: 0,
            keep_cursor_visible: true,
            desired_column: None,
        }
    }

    fn selected_range(&self) -> Option<(usize, usize)> {
        self.anchor
            .filter(|anchor| *anchor != self.cursor)
            .map(|anchor| (anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    fn width(&self, value: &str) -> f32 {
        measured_width(
            self.measurer.as_deref(),
            value,
            self.font_size,
            self.font_family,
            self.char_width,
        )
    }

    fn visual_lines(&self) -> Vec<VisualLine> {
        visual_lines_measured(&self.value, self.content_width, |value| self.width(value))
    }

    fn move_cursor(&mut self, next: usize, shift: bool) {
        if shift {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = next.min(self.value.chars().count());
        self.desired_column = None;
        self.keep_cursor_visible = true;
    }

    fn move_vertical(&mut self, direction: i32, shift: bool) {
        let lines = self.visual_lines();
        let (row, column) = visual_cursor(&lines, self.cursor);
        let desired = self.desired_column.unwrap_or(column);
        let next_row =
            (row as i64 + direction as i64).clamp(0, lines.len().saturating_sub(1) as i64) as usize;
        let next = lines[next_row].start + desired.min(lines[next_row].end - lines[next_row].start);
        self.move_cursor(next, shift);
        self.desired_column = Some(desired);
    }

    fn move_line_edge(&mut self, end: bool, shift: bool) {
        let lines = self.visual_lines();
        let (row, _) = visual_cursor(&lines, self.cursor);
        let next = if end {
            lines[row].end
        } else {
            lines[row].start
        };
        self.move_cursor(next, shift);
    }

    fn place_cursor_at(&mut self, point: Point) {
        let lines = self.visual_lines();
        let row = ((point.y - self.content_y).max(0.0) / self.line_height).floor() as usize;
        let row = (self.scroll_line + row).min(lines.len() - 1);
        let column = nearest_char(
            &lines[row].text,
            (point.x - self.content_x).max(0.0),
            |value| self.width(value),
        );
        let next = lines[row].start + column.min(lines[row].end - lines[row].start);
        self.move_cursor(next, false);
    }

    fn scroll_lines(&mut self, delta_y: f32) {
        if delta_y == 0.0 {
            return;
        }
        let maximum = self.visual_lines().len().saturating_sub(self.visible_lines);
        let steps = (delta_y.abs() / self.line_height).ceil().max(1.0) as usize;
        self.scroll_line = if delta_y > 0.0 {
            self.scroll_line.saturating_sub(steps)
        } else {
            self.scroll_line.saturating_add(steps).min(maximum)
        };
        self.keep_cursor_visible = false;
    }

    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selected_range() else {
            return false;
        };
        let start_byte = byte_index(&self.value, start);
        let end_byte = byte_index(&self.value, end);
        self.value.replace_range(start_byte..end_byte, "");
        self.cursor = start;
        self.anchor = None;
        self.keep_cursor_visible = true;
        true
    }

    fn insert(&mut self, text: &str, multiline: bool) -> bool {
        let sanitized = if multiline {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.replace(['\r', '\n'], "")
        };
        if sanitized.is_empty() {
            return false;
        }
        self.delete_selection();
        let byte = byte_index(&self.value, self.cursor);
        self.value.insert_str(byte, &sanitized);
        self.cursor += sanitized.chars().count();
        self.keep_cursor_visible = true;
        self.desired_column = None;
        true
    }

    fn backspace(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.cursor == 0 {
            return false;
        }
        let end = byte_index(&self.value, self.cursor);
        let start = byte_index(&self.value, self.cursor - 1);
        self.value.replace_range(start..end, "");
        self.cursor -= 1;
        self.keep_cursor_visible = true;
        self.desired_column = None;
        true
    }

    fn delete_forward(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.cursor >= self.value.chars().count() {
            return false;
        }
        let start = byte_index(&self.value, self.cursor);
        let end = byte_index(&self.value, self.cursor + 1);
        self.value.replace_range(start..end, "");
        self.keep_cursor_visible = true;
        self.desired_column = None;
        true
    }
}

fn byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map_or(value.len(), |(index, _)| index)
}

fn char_range(value: &str, start: usize, end: usize) -> &str {
    &value[byte_index(value, start)..byte_index(value, end)]
}

fn fitting_chars(value: &str, max_width: f32, mut measure: impl FnMut(&str) -> f32) -> usize {
    let boundaries = value
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(value.len()))
        .collect::<Vec<_>>();
    let mut lo = 0;
    let mut hi = boundaries.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if measure(&value[..boundaries[mid]]) <= max_width {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo.saturating_sub(1)
}

#[derive(Clone, Debug)]
struct VisualLine {
    start: usize,
    end: usize,
    text: String,
}

/// The text editor shares the same measured line breaks as painted labels.
/// Indices remain Unicode scalar positions because input events use char IDs.
fn visual_lines_measured(
    value: &str,
    width: f32,
    mut measure: impl FnMut(&str) -> f32,
) -> Vec<VisualLine> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for paragraph in value.split('\n') {
        for text in wrap_measured(paragraph, width, &mut measure) {
            let count = text.chars().count();
            lines.push(VisualLine {
                start,
                end: start + count,
                text: text.to_owned(),
            });
            start += count;
        }
        start += 1;
    }
    lines
}

fn nearest_char(value: &str, x: f32, mut measure: impl FnMut(&str) -> f32) -> usize {
    if x <= 0.0 {
        return 0;
    }
    let boundaries = value
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(value.len()))
        .collect::<Vec<_>>();
    let mut lo = 0;
    let mut hi = boundaries.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if measure(&value[..boundaries[mid]]) < x {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let after = lo.min(boundaries.len() - 1);
    if after == 0 {
        return 0;
    }
    let before = after - 1;
    let left = measure(&value[..boundaries[before]]);
    let right = measure(&value[..boundaries[after]]);
    if x - left <= right - x {
        before
    } else {
        after
    }
}

fn nearest_grapheme(value: &str, x: f32, mut measure: impl FnMut(&str) -> f32) -> usize {
    if x <= 0.0 {
        return 0;
    }
    let boundaries = std::iter::once(0)
        .chain(
            value
                .grapheme_indices(true)
                .map(|(index, part)| index + part.len()),
        )
        .collect::<Vec<_>>();
    let mut lo = 0;
    let mut hi = boundaries.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if measure(&value[..boundaries[mid]]) < x {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let after = lo.min(boundaries.len() - 1);
    if after == 0 {
        return 0;
    }
    let before = after - 1;
    let left = measure(&value[..boundaries[before]]);
    let right = measure(&value[..boundaries[after]]);
    let boundary = if x - left <= right - x {
        boundaries[before]
    } else {
        boundaries[after]
    };
    value[..boundary].chars().count()
}

fn visual_cursor(lines: &[VisualLine], cursor: usize) -> (usize, usize) {
    for (row, line) in lines.iter().enumerate() {
        let wraps_here = lines.get(row + 1).is_some_and(|next| next.start == cursor);
        if cursor < line.end || (cursor == line.end && !wraps_here) {
            return (row, cursor.saturating_sub(line.start));
        }
    }
    let row = lines.len() - 1;
    (row, cursor.saturating_sub(lines[row].start))
}

/// The retained state and last laid-out frame of an application surface.
pub struct UiTree {
    root: Element,
    metrics: FontMetrics,
    sans_metrics: FontMetrics,
    text_measurer: Option<Arc<TextMeasurer>>,
    frame: Frame,
    inputs: HashMap<String, InputState>,
    scroll_offsets: HashMap<String, f32>,
    scroll_to_end: HashSet<String>,
    scroll_to_index: HashMap<String, usize>,
    scroll_drag: Option<ScrollDrag>,
    hovered_scrollbar: Option<String>,
    focus_order: Vec<(String, HitKind)>,
    focused: Option<String>,
    pending_scroll_focus: Option<String>,
    hovered: Option<String>,
    pointer: Option<Point>,
    tooltip_hover_since: Option<(String, Instant)>,
    keyboard_focus: bool,
    keyboard_tooltip_focus: bool,
    tooltip_triggers: HashSet<String>,
    pressed: Option<String>,
    transitions: HashMap<String, ColorTransition>,
    popover_transitions: HashMap<String, ScalarTransition>,
    popover_snapshots: HashMap<String, PopoverSnapshot>,
    popover_exits: HashMap<String, PopoverExit>,
    tab_indicators: HashMap<String, TabIndicatorState>,
    reduced_motion: bool,
    static_selection: Option<StaticSelection>,
}

impl UiTree {
    pub fn new(root: Element) -> Self {
        Self {
            root,
            metrics: FontMetrics::default(),
            sans_metrics: FontMetrics::default(),
            text_measurer: None,
            frame: Frame::default(),
            inputs: HashMap::new(),
            scroll_offsets: HashMap::new(),
            scroll_to_end: HashSet::new(),
            scroll_to_index: HashMap::new(),
            scroll_drag: None,
            hovered_scrollbar: None,
            focus_order: Vec::new(),
            focused: None,
            pending_scroll_focus: None,
            hovered: None,
            pointer: None,
            tooltip_hover_since: None,
            keyboard_focus: false,
            keyboard_tooltip_focus: false,
            tooltip_triggers: HashSet::new(),
            pressed: None,
            transitions: HashMap::new(),
            popover_transitions: HashMap::new(),
            popover_snapshots: HashMap::new(),
            popover_exits: HashMap::new(),
            tab_indicators: HashMap::new(),
            reduced_motion: false,
            static_selection: None,
        }
    }

    pub fn with_metrics(mut self, metrics: FontMetrics) -> Self {
        self.set_metrics(metrics);
        self
    }

    /// Updates the dimensions used for text layout and cursor positioning.
    /// Match these values to the font selected by the host renderer.
    pub fn set_metrics(&mut self, metrics: FontMetrics) {
        self.metrics = FontMetrics {
            char_width: metrics.char_width.max(1.0),
            line_height: metrics.line_height.max(1.0),
        };
    }

    /// Calibrate the proportional UI font at [`DEFAULT_FONT_SIZE`]. Text width
    /// is estimated with one representative character because glyphs vary.
    pub fn set_sans_metrics(&mut self, metrics: FontMetrics) {
        self.sans_metrics = FontMetrics {
            char_width: metrics.char_width.max(1.0),
            line_height: metrics.line_height.max(1.0),
        };
    }

    /// Install the host painter's text width function. It must use the same
    /// font family and size as `DrawCommand::Text`. The callback is retained
    /// between frames; a missing callback uses deterministic font metrics.
    pub fn set_text_measurer(
        &mut self,
        measure: impl Fn(&str, f32, FontFamily) -> f32 + Send + Sync + 'static,
    ) {
        self.text_measurer = Some(Arc::new(measure));
    }

    pub fn clear_text_measurer(&mut self) {
        self.text_measurer = None;
    }

    /// Replaces the visual tree without discarding input, focus or scroll state.
    /// Reuse IDs for widgets that represent the same logical control.
    pub fn set_root(&mut self, root: Element) {
        self.root = root;
    }

    pub fn input_value(&self, id: &str) -> Option<&str> {
        self.inputs.get(id).map(|state| state.value.as_str())
    }

    /// Text selected by dragging over one static text or formatted-text block.
    /// The copied slice preserves source newlines and Unicode boundaries.
    pub fn selected_text(&self) -> Option<&str> {
        let selection = self.static_selection.as_ref()?;
        let (start, end) = selection.range()?;
        Some(char_range(&selection.source, start, end))
    }

    pub fn set_input_value(&mut self, id: &str, value: impl Into<String>) {
        let state = self
            .inputs
            .entry(id.to_owned())
            .or_insert_with(|| InputState::new(String::new()));
        state.value = value.into();
        state.cursor = state.value.chars().count();
        state.anchor = None;
        state.view_start = 0;
        state.scroll_line = 0;
        state.keep_cursor_visible = true;
        state.desired_column = None;
    }

    pub fn focused_id(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    /// Focus an interactive element after layout. Returns false for an unknown ID.
    /// Text inputs place the caret at the end so command launchers can accept
    /// typing immediately after opening.
    pub fn focus(&mut self, id: &str) -> bool {
        if !self.focus_order.iter().any(|(candidate, kind)| {
            candidate == id
                && matches!(
                    kind,
                    HitKind::Button | HitKind::TextInput | HitKind::TextArea
                )
        }) {
            return false;
        }
        self.request_scroll_focus(id);
        self.focused = Some(id.to_owned());
        self.keyboard_focus = true;
        self.keyboard_tooltip_focus = true;
        if let Some(input) = self.inputs.get_mut(id) {
            input.cursor = input.value.chars().count();
            input.anchor = None;
            input.keep_cursor_visible = true;
        }
        true
    }

    /// Disable decorative color transitions when the user requests reduced motion.
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        if reduced {
            self.transitions.clear();
            self.popover_transitions.clear();
            self.popover_snapshots.clear();
            self.popover_exits.clear();
            self.tab_indicators.clear();
        }
        self.reduced_motion = reduced;
    }

    /// Hosts should schedule a short repaint while this is true.
    pub fn is_animating(&self) -> bool {
        let now = Instant::now();
        self.transitions.values().any(|state| state.is_active(now))
            || self
                .popover_transitions
                .values()
                .any(|state| state.is_active(now))
            || self
                .popover_exits
                .values()
                .any(|state| state.opacity.is_active(now))
            || self
                .tab_indicators
                .values()
                .any(|state| state.x.is_active(now) || state.width.is_active(now))
            || self
                .tooltip_hover_since
                .as_ref()
                .is_some_and(|(id, since)| {
                    self.tooltip_triggers.contains(id)
                        && self.pressed.as_deref() != Some(id)
                        && now.saturating_duration_since(*since) < TOOLTIP_HOVER_DELAY
                })
    }

    pub fn scroll_offset(&self, id: &str) -> f32 {
        self.scroll_offsets.get(id).copied().unwrap_or(0.0)
    }

    /// Checks the last layout's scroll extent. Call before changing content to
    /// decide whether incoming content should keep the viewport pinned to bottom.
    pub fn is_scroll_at_end(&self, id: &str, tolerance: f32) -> bool {
        self.frame
            .scroll_extents
            .get(id)
            .is_some_and(|maximum| maximum - self.scroll_offset(id) <= tolerance.max(0.0))
    }

    /// Scrolls to the bottom on the next layout, after new content is measured.
    pub fn scroll_to_end(&mut self, id: &str) {
        self.scroll_to_index.remove(id);
        self.scroll_to_end.insert(id.to_owned());
    }

    /// Keeps one fixed-height list row in view on the next layout. The scroll
    /// position is unchanged if the row is already fully visible.
    pub fn scroll_to_index(&mut self, id: &str, index: usize) {
        self.scroll_to_end.remove(id);
        self.scroll_to_index.insert(id.to_owned(), index);
    }

    pub fn layout(&mut self, viewport: Size) -> Frame {
        self.layout_at(viewport, Instant::now())
    }

    fn layout_at(&mut self, viewport: Size, now: Instant) -> Frame {
        self.layout_at_with_hover_refresh(viewport, now, true)
    }

    fn layout_at_with_hover_refresh(
        &mut self,
        viewport: Size,
        now: Instant,
        refresh_hover_paint: bool,
    ) -> Frame {
        let viewport = Size::new(viewport.width.max(0.0), viewport.height.max(0.0));
        if self.pending_scroll_focus.as_deref() != self.focused.as_deref() {
            self.pending_scroll_focus = None;
        }
        let previous_popovers = self.frame.popover_focus.clone();
        let mut focus_order = Vec::new();
        let mut seen = HashSet::new();
        collect_focus_order(&self.root, &mut focus_order, &mut seen);
        let mut focus_path = HashSet::new();
        if let Some(id) = self.pending_scroll_focus.as_deref() {
            find_focus_path(&self.root, id, &mut focus_path);
        }
        let mut observed_buttons = HashSet::new();
        let mut observed_popovers = HashSet::new();
        let mut current_popover_snapshots = HashMap::new();
        let mut observed_tabs = HashSet::new();
        let mut observed_tooltips = HashSet::new();
        let viewport_rect = Rect::new(0.0, 0.0, viewport.width, viewport.height);
        let (mut frame, overlay_start) = {
            let mut builder = LayoutBuilder {
                frame: Frame::default(),
                overlays: VecDeque::new(),
                scroll_focus_context: Vec::new(),
                focus_path: &focus_path,
                focus_probe: self.pending_scroll_focus.as_deref(),
                metrics: self.metrics,
                sans_metrics: self.sans_metrics,
                text_measurer: self.text_measurer.clone(),
                width_cache: RefCell::new(HashMap::new()),
                inputs: &mut self.inputs,
                scroll_offsets: &mut self.scroll_offsets,
                scroll_to_end: &mut self.scroll_to_end,
                scroll_to_index: &mut self.scroll_to_index,
                hovered_scrollbar: self.hovered_scrollbar.as_deref(),
                dragging_scrollbar: self.scroll_drag.as_ref().map(|drag| drag.id.as_str()),
                focused: self.focused.as_deref(),
                hovered: self.hovered.as_deref(),
                tooltip_hover_since: self.tooltip_hover_since.as_ref(),
                keyboard_focus: self.keyboard_focus,
                keyboard_tooltip_focus: self.keyboard_tooltip_focus,
                pressed: self.pressed.as_deref(),
                selection: self.static_selection.as_ref(),
                transitions: &mut self.transitions,
                popover_transitions: &mut self.popover_transitions,
                popover_exits: &self.popover_exits,
                tab_indicators: &mut self.tab_indicators,
                observed_buttons: &mut observed_buttons,
                observed_popovers: &mut observed_popovers,
                popover_snapshots: &mut current_popover_snapshots,
                observed_tabs: &mut observed_tabs,
                observed_tooltips: &mut observed_tooltips,
                reduced_motion: self.reduced_motion,
                now,
            };
            builder.place(&self.root, viewport_rect, None);
            let overlay_start = builder.frame.commands.len();
            // Deferred painting gives popovers one stable top layer even when
            // their anchor lives inside a clipped scroll container. Nested
            // overlays are appended and painted above their parent overlay.
            while let Some(overlay) = builder.overlays.pop_front() {
                match overlay {
                    DeferredOverlay::Free { floating, area } => {
                        builder.place(&floating, area, Some(viewport_rect));
                    }
                    DeferredOverlay::Anchored {
                        floating,
                        anchor,
                        dismiss_id,
                        placement,
                        gap,
                        trigger_id,
                        focusables,
                        animation_ms,
                    } => {
                        builder.frame.regions.push(HitRegion {
                            id: dismiss_id.clone(),
                            rect: viewport_rect,
                            kind: HitKind::Button,
                        });
                        builder.frame.popover_focus.push(PopoverFocusScope {
                            dismiss_id: dismiss_id.clone(),
                            trigger_id,
                            focusables,
                        });
                        let measured = builder.measure(
                            &floating,
                            Size::new((viewport.width - 16.0).max(0.0), f32::INFINITY),
                        );
                        let rect = place_popover(anchor, measured, viewport_rect, placement, gap);
                        builder.observed_popovers.insert(dismiss_id.clone());
                        builder.frame.regions.push(HitRegion {
                            id: format!("popover_surface:{dismiss_id}"),
                            rect,
                            kind: HitKind::Surface,
                        });
                        let progress = if builder.reduced_motion || animation_ms == 0 {
                            1.0
                        } else {
                            let transition = builder
                                .popover_transitions
                                .entry(dismiss_id.clone())
                                .or_insert_with(|| {
                                    ScalarTransition::new(
                                        builder
                                            .popover_exits
                                            .get(&dismiss_id)
                                            .map(|exit| exit.opacity.value(now))
                                            .unwrap_or(0.0),
                                        now,
                                    )
                                });
                            transition.retarget(
                                1.0,
                                now,
                                Duration::from_millis(u64::from(animation_ms)),
                            );
                            transition.value(now)
                        };
                        let command_start = builder.frame.commands.len();
                        builder.place(&floating, rect, Some(viewport_rect));
                        let slide_sign = if rect.y >= anchor.y + anchor.height {
                            1.0
                        } else {
                            -1.0
                        };
                        builder.popover_snapshots.insert(
                            dismiss_id.clone(),
                            PopoverSnapshot {
                                commands: builder.frame.commands[command_start..].to_vec(),
                                animation_ms,
                                slide_sign,
                            },
                        );
                        if progress < 1.0 {
                            let slide = (1.0 - progress) * slide_sign * 4.0;
                            for command in &mut builder.frame.commands[command_start..] {
                                animate_popover_command(command, progress, slide);
                            }
                        }
                    }
                    DeferredOverlay::Tooltip {
                        anchor,
                        title,
                        shortcut,
                        placement,
                        colors,
                    } => builder.paint_tooltip(
                        anchor,
                        &title,
                        shortcut.as_deref(),
                        placement,
                        colors,
                        viewport_rect,
                    ),
                }
            }
            (builder.frame, overlay_start)
        };
        if self.reduced_motion {
            self.popover_exits.clear();
        } else {
            for (id, paint) in std::mem::take(&mut self.popover_snapshots) {
                if observed_popovers.contains(&id) || paint.animation_ms == 0 {
                    continue;
                }
                let starting_opacity = self
                    .popover_transitions
                    .get(&id)
                    .map(|transition| transition.value(now))
                    .unwrap_or(1.0);
                let mut opacity = ScalarTransition::new(starting_opacity, now);
                opacity.retarget(
                    0.0,
                    now,
                    Duration::from_millis(u64::from(paint.animation_ms.min(110))),
                );
                self.popover_exits
                    .insert(id, PopoverExit { paint, opacity });
            }
            self.popover_exits
                .retain(|id, exit| !observed_popovers.contains(id) && exit.opacity.is_active(now));
            let mut exit_commands = Vec::new();
            for exit in self.popover_exits.values() {
                let opacity = exit.opacity.value(now);
                let slide = (1.0 - opacity) * exit.paint.slide_sign * 4.0;
                for command in &exit.paint.commands {
                    let mut command = command.clone();
                    animate_popover_command(&mut command, opacity, slide);
                    exit_commands.push(command);
                }
            }
            // Above the ordinary scene, below every currently open popover.
            frame
                .commands
                .splice(overlay_start..overlay_start, exit_commands);
        }
        self.popover_snapshots = current_popover_snapshots;
        self.focus_order = focus_order
            .into_iter()
            .flat_map(|(id, kind)| {
                if kind == HitKind::Scroll && id.starts_with(VIRTUAL_FOCUS_PREFIX) {
                    frame
                        .virtual_lists
                        .get(&id[VIRTUAL_FOCUS_PREFIX.len()..])
                        .map(|scope| scope.focusables().cloned().collect::<Vec<_>>())
                        .unwrap_or_default()
                } else {
                    vec![(id, kind)]
                }
            })
            .collect();
        if let Some(id) = self.focused.as_ref().filter(|id| {
            !self
                .focus_order
                .iter()
                .any(|(candidate, _)| candidate == *id)
        }) {
            self.focused = previous_popovers
                .iter()
                .rev()
                .find(|scope| {
                    scope
                        .focusables
                        .iter()
                        .any(|(candidate, _)| candidate == id)
                })
                .and_then(|scope| scope.trigger_id.as_ref())
                .filter(|trigger| {
                    self.focus_order
                        .iter()
                        .any(|(candidate, _)| candidate == *trigger)
                })
                .cloned();
        }
        self.transitions
            .retain(|id, _| observed_buttons.contains(id));
        self.popover_transitions
            .retain(|id, _| observed_popovers.contains(id));
        self.tab_indicators
            .retain(|id, _| observed_tabs.contains(id));
        self.tooltip_triggers = observed_tooltips;
        self.frame = frame;
        if let Some(id) = self.pending_scroll_focus.take() {
            let previous_offset = self.frame.scroll_focus_targets.get(&id).map(|target| {
                self.scroll_offsets
                    .get(&target.scroll_id)
                    .copied()
                    .unwrap_or_default()
            });
            self.reveal_scroll_focus(&id);
            if self.frame.scroll_focus_targets.get(&id).map(|target| {
                self.scroll_offsets
                    .get(&target.scroll_id)
                    .copied()
                    .unwrap_or_default()
            }) != previous_offset
            {
                return self.layout_at_with_hover_refresh(viewport, now, refresh_hover_paint);
            }
        }
        if self.pressed.as_ref().is_some_and(|id| {
            !self
                .frame
                .regions
                .iter()
                .any(|region| region.kind == HitKind::Button && region.id == *id)
        }) {
            self.pressed = None;
        }
        self.hovered_scrollbar = self.pointer.and_then(|point| {
            self.frame
                .hit_region(point)
                .filter(|region| region.kind == HitKind::Scrollbar)
                .map(|region| region.id.clone())
        });
        // Reconcile pointer state with the new layout: a scroll, navigation,
        // or popover can move the target without any physical pointer motion.
        if let Some(point) = self.pointer {
            let target = self.frame.hit_test(point).map(str::to_owned);
            if target != self.hovered {
                self.set_hovered(target, now);
                if refresh_hover_paint {
                    return self.layout_at_with_hover_refresh(viewport, now, false);
                }
            }
        }
        if self.static_selection.as_ref().is_some_and(|selection| {
            self.frame
                .text_blocks
                .get(selection.block)
                .is_none_or(|block| block.source != selection.source)
        }) {
            self.static_selection = None;
        }
        if self
            .scroll_drag
            .as_ref()
            .is_some_and(|drag| !self.frame.scrollbars.contains_key(&drag.id))
        {
            self.scroll_drag = None;
        }
        self.frame.clone()
    }

    fn set_hovered(&mut self, target: Option<String>, now: Instant) {
        if target != self.hovered {
            self.tooltip_hover_since = target.as_ref().map(|id| (id.clone(), now));
            self.hovered = target;
        }
    }

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    pub fn handle_event(&mut self, event: UiEvent) -> Vec<UiAction> {
        let mut actions = Vec::new();
        match event {
            UiEvent::PointerMove(point) => {
                self.pointer = Some(point);
                let target = self.frame.hit_test(point).map(str::to_owned);
                if target.as_deref() != self.focused.as_deref() {
                    self.keyboard_tooltip_focus = false;
                }
                self.set_hovered(target, Instant::now());
                self.hovered_scrollbar = self
                    .frame
                    .hit_region(point)
                    .filter(|region| region.kind == HitKind::Scrollbar)
                    .map(|region| region.id.clone());
                if let Some(drag) = self.scroll_drag.as_ref() {
                    if let Some(geometry) = self.frame.scrollbars.get(&drag.id) {
                        self.scroll_offsets
                            .insert(drag.id.clone(), geometry.offset_at(point.y, drag.grab_y));
                    }
                }
                if let Some(selection) = self.static_selection.as_mut().filter(|s| s.dragging) {
                    if let Some(block) = self.frame.text_blocks.get(selection.block) {
                        if block.source == selection.source {
                            selection.cursor = block.offset_at(
                                point,
                                self.text_measurer.as_deref(),
                                self.metrics,
                                self.sans_metrics,
                            );
                        }
                    }
                }
            }
            UiEvent::PointerLeave => {
                self.pointer = None;
                self.set_hovered(None, Instant::now());
                self.hovered_scrollbar = None;
                self.pressed = None;
                self.scroll_drag = None;
                self.keyboard_tooltip_focus = false;
                if let Some(selection) = self.static_selection.as_mut() {
                    selection.dragging = false;
                }
            }
            UiEvent::PointerDown(point) => {
                self.pointer = Some(point);
                self.set_hovered(
                    self.frame.hit_test(point).map(str::to_owned),
                    Instant::now(),
                );
                self.keyboard_focus = false;
                self.keyboard_tooltip_focus = false;
                self.tooltip_hover_since = None;
                let hit = self.frame.hit_region(point).cloned();
                let dismiss_scope = hit.as_ref().and_then(|region| {
                    self.frame
                        .popover_focus
                        .iter()
                        .rev()
                        .find(|scope| scope.dismiss_id == region.id)
                });
                let dismiss_trigger = dismiss_scope.and_then(|scope| scope.trigger_id.clone());
                self.static_selection = None;
                if let Some(region) = hit
                    .as_ref()
                    .filter(|region| region.kind == HitKind::Scrollbar)
                {
                    if let Some(geometry) = self.frame.scrollbars.get(&region.id) {
                        let grab_y = if geometry.thumb.contains(point) {
                            point.y - geometry.thumb.y
                        } else {
                            geometry.thumb.height / 2.0
                        };
                        self.scroll_offsets
                            .insert(region.id.clone(), geometry.offset_at(point.y, grab_y));
                        self.scroll_drag = Some(ScrollDrag {
                            id: region.id.clone(),
                            grab_y,
                        });
                        self.hovered_scrollbar = Some(region.id.clone());
                    }
                    self.pressed = None;
                    return actions;
                }
                if !hit.as_ref().is_some_and(|region| {
                    matches!(
                        region.kind,
                        HitKind::Button | HitKind::TextInput | HitKind::TextArea
                    )
                }) {
                    if let Some((index, block)) = self
                        .frame
                        .text_blocks
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(_, block)| block.contains(point))
                    {
                        let offset = block.offset_at(
                            point,
                            self.text_measurer.as_deref(),
                            self.metrics,
                            self.sans_metrics,
                        );
                        self.static_selection = Some(StaticSelection {
                            block: index,
                            source: block.source.clone(),
                            anchor: offset,
                            cursor: offset,
                            dragging: true,
                        });
                    }
                }
                self.pressed = hit
                    .as_ref()
                    .filter(|region| region.kind == HitKind::Button)
                    .map(|region| region.id.clone());
                self.focused = hit
                    .as_ref()
                    .filter(|region| {
                        matches!(
                            region.kind,
                            HitKind::Button | HitKind::TextInput | HitKind::TextArea
                        ) && dismiss_scope.is_none()
                    })
                    .map(|region| region.id.clone())
                    .or(dismiss_trigger);
                if let Some(region) = hit
                    .filter(|region| matches!(region.kind, HitKind::TextInput | HitKind::TextArea))
                {
                    if let Some(input) = self.inputs.get_mut(&region.id) {
                        if region.kind == HitKind::TextArea {
                            input.place_cursor_at(point);
                        } else {
                            let x = (point.x - input.content_x).max(0.0);
                            let shown = input
                                .value
                                .chars()
                                .skip(input.view_start)
                                .collect::<String>();
                            let character = nearest_char(&shown, x, |part| input.width(part));
                            input.move_cursor(input.view_start + character, false);
                        }
                    }
                }
            }
            UiEvent::PointerUp(point) => {
                self.pointer = Some(point);
                self.set_hovered(
                    self.frame.hit_test(point).map(str::to_owned),
                    Instant::now(),
                );
                self.tooltip_hover_since =
                    self.hovered.as_ref().map(|id| (id.clone(), Instant::now()));
                if let Some(selection) = self.static_selection.as_mut().filter(|s| s.dragging) {
                    if let Some(block) = self.frame.text_blocks.get(selection.block) {
                        if block.source == selection.source {
                            selection.cursor = block.offset_at(
                                point,
                                self.text_measurer.as_deref(),
                                self.metrics,
                                self.sans_metrics,
                            );
                        }
                    }
                    selection.dragging = false;
                }
                if self.scroll_drag.take().is_some() {
                    return actions;
                }
                if let Some(pressed) = self.pressed.take() {
                    if self.frame.hit_test(point) == Some(pressed.as_str()) {
                        actions.push(UiAction::Clicked { id: pressed });
                    }
                }
            }
            UiEvent::Scroll { position, delta_y } => {
                self.pointer = Some(position);
                for region in self.frame.regions.iter().rev().filter(|region| {
                    matches!(region.kind, HitKind::Scroll | HitKind::TextArea)
                        && region.rect.contains(position)
                }) {
                    if region.kind == HitKind::TextArea {
                        if let Some(input) = self.inputs.get_mut(&region.id) {
                            let before = input.scroll_line;
                            input.scroll_lines(delta_y);
                            if input.scroll_line != before {
                                break;
                            }
                        }
                    } else {
                        let max = self.scroll_max(&region.id);
                        let offset = self.scroll_offsets.entry(region.id.clone()).or_default();
                        let next = (*offset - delta_y).clamp(0.0, max);
                        if next != *offset {
                            *offset = next;
                            break;
                        }
                    }
                }
            }
            UiEvent::Text(text) => {
                if let Some(id) = self.focused.as_ref() {
                    let multiline = self
                        .focus_order
                        .iter()
                        .any(|(candidate, kind)| candidate == id && *kind == HitKind::TextArea);
                    if let Some(input) = self.inputs.get_mut(id) {
                        if input.insert(&text, multiline) {
                            actions.push(UiAction::InputChanged {
                                id: id.clone(),
                                value: input.value.clone(),
                            });
                        }
                    }
                }
            }
            UiEvent::Key { key, shift, ctrl } => {
                if key == Key::Escape {
                    if let Some(scope) = self.active_popover_scope() {
                        let dismiss_id = scope.dismiss_id.clone();
                        let trigger_id = scope.trigger_id.clone();
                        actions.push(UiAction::Clicked { id: dismiss_id });
                        self.focused = trigger_id;
                        self.keyboard_focus = true;
                        self.keyboard_tooltip_focus = true;
                    }
                } else if key == Key::C && ctrl {
                    let input_text = self.focused.as_ref().and_then(|id| {
                        let input = self.inputs.get(id)?;
                        let (start, end) = input.selected_range()?;
                        Some(char_range(&input.value, start, end).to_owned())
                    });
                    if let Some(text) =
                        input_text.or_else(|| self.selected_text().map(str::to_owned))
                    {
                        actions.push(UiAction::CopyRequested { text });
                    }
                } else if key == Key::Tab {
                    self.step_focus(shift);
                    self.keyboard_focus = true;
                    self.keyboard_tooltip_focus = true;
                } else if !shift
                    && !ctrl
                    && matches!(key, Key::ArrowLeft | Key::ArrowRight | Key::Home | Key::End)
                    && self.step_tab(key, &mut actions)
                {
                    // A tab strip activates the newly focused tab.
                    self.keyboard_focus = true;
                    self.keyboard_tooltip_focus = true;
                } else if !shift
                    && !ctrl
                    && matches!(key, Key::ArrowDown | Key::ArrowUp | Key::Home | Key::End)
                    && self.step_popover_option(key)
                {
                    // Menu navigation consumes arrow/home/end keys only while
                    // its trigger or one of its buttons owns focus.
                    self.keyboard_focus = true;
                    self.keyboard_tooltip_focus = true;
                } else if let Some(id) = self.focused.as_ref() {
                    let kind = self
                        .focus_order
                        .iter()
                        .find(|(candidate, _)| candidate == id)
                        .map(|(_, kind)| *kind);
                    if kind == Some(HitKind::Button) {
                        if matches!(key, Key::Enter | Key::Space) {
                            self.keyboard_focus = true;
                            self.keyboard_tooltip_focus = true;
                            actions.push(UiAction::Clicked { id: id.clone() });
                        }
                    } else if matches!(kind, Some(HitKind::TextInput | HitKind::TextArea)) {
                        let multiline = kind == Some(HitKind::TextArea);
                        let Some(input) = self.inputs.get_mut(id) else {
                            return actions;
                        };
                        let changed = match key {
                            Key::Escape | Key::Tab | Key::Space => false,
                            Key::ArrowLeft => {
                                input.move_cursor(input.cursor.saturating_sub(1), shift);
                                false
                            }
                            Key::ArrowRight => {
                                input.move_cursor(input.cursor.saturating_add(1), shift);
                                false
                            }
                            Key::ArrowUp => {
                                if multiline {
                                    input.move_vertical(-1, shift);
                                }
                                false
                            }
                            Key::ArrowDown => {
                                if multiline {
                                    input.move_vertical(1, shift);
                                }
                                false
                            }
                            Key::Home => {
                                if multiline && !ctrl {
                                    input.move_line_edge(false, shift);
                                } else {
                                    input.move_cursor(0, shift);
                                }
                                false
                            }
                            Key::End => {
                                if multiline && !ctrl {
                                    input.move_line_edge(true, shift);
                                } else {
                                    input.move_cursor(input.value.chars().count(), shift);
                                }
                                false
                            }
                            Key::Backspace => input.backspace(),
                            Key::Delete => input.delete_forward(),
                            Key::A if ctrl => {
                                input.anchor = Some(0);
                                input.cursor = input.value.chars().count();
                                input.keep_cursor_visible = true;
                                false
                            }
                            Key::Enter => {
                                if multiline && !ctrl {
                                    input.insert("\n", true)
                                } else {
                                    actions.push(UiAction::Submitted {
                                        id: id.clone(),
                                        value: input.value.clone(),
                                    });
                                    false
                                }
                            }
                            Key::A => false,
                            _ => false,
                        };
                        if changed {
                            actions.push(UiAction::InputChanged {
                                id: id.clone(),
                                value: input.value.clone(),
                            });
                        }
                    }
                }
            }
        }
        actions
    }

    fn scroll_max(&self, id: &str) -> f32 {
        // The layout builder records scroll extents separately in the frame.
        self.frame.scroll_extents.get(id).copied().unwrap_or(0.0)
    }

    fn request_scroll_focus(&mut self, id: &str) {
        if self.frame.scroll_focus_targets.contains_key(id) {
            self.reveal_scroll_focus(id);
            self.pending_scroll_focus = None;
        } else {
            self.pending_scroll_focus = Some(id.to_owned());
        }
    }

    fn step_focus(&mut self, reverse: bool) {
        if let Some(scope) = self.active_popover_scope().cloned() {
            let reachable = self.reachable_popover_focusables(&scope);
            if !reachable.is_empty() {
                let current = self
                    .focused
                    .as_ref()
                    .and_then(|id| reachable.iter().position(|(candidate, _)| candidate == id));
                let next = match (current, reverse) {
                    (Some(index), false) => (index + 1) % reachable.len(),
                    (Some(0), true) => reachable.len() - 1,
                    (Some(index), true) => index - 1,
                    (None, false) => 0,
                    (None, true) => reachable.len() - 1,
                };
                let id = reachable[next].0.clone();
                self.request_scroll_focus(&id);
                self.focused = Some(id);
                return;
            }
            if let Some(trigger) = scope.trigger_id.as_ref() {
                self.focused = Some(trigger.clone());
                return;
            }
        }
        if self.step_virtual_list_focus(reverse) {
            return;
        }
        if self.focus_order.is_empty() {
            self.focused = None;
            return;
        }
        let current = self.focused.as_ref().and_then(|id| {
            self.focus_order
                .iter()
                .position(|(candidate, _)| candidate == id)
        });
        let next = match (current, reverse) {
            (Some(index), false) => (index + 1) % self.focus_order.len(),
            (Some(0), true) => self.focus_order.len() - 1,
            (Some(index), true) => index - 1,
            (None, false) => 0,
            (None, true) => self.focus_order.len() - 1,
        };
        let id = self.focus_order[next].0.clone();
        self.request_scroll_focus(&id);
        self.focused = Some(id);
    }

    fn step_virtual_list_focus(&mut self, reverse: bool) -> bool {
        let Some(focused) = self.focused.as_ref() else {
            return false;
        };
        let Some((list_id, scope)) = self.frame.virtual_lists.iter().find(|(_, scope)| {
            let edge = if reverse {
                scope.focusables().next()
            } else {
                scope.focusables().last()
            };
            edge.is_some_and(|(id, _)| id == focused)
        }) else {
            return false;
        };
        let list_id = list_id.clone();
        let scope = scope.clone();
        let Some(list) = find_uniform_list(&self.root, &list_id).cloned() else {
            return false;
        };
        let candidates: Box<dyn Iterator<Item = usize>> = if reverse {
            Box::new((0..scope.first_visible).rev())
        } else {
            Box::new(scope.end_visible..scope.row_count)
        };
        for index in candidates {
            let row = (list.build_row)(index);
            let mut focusables = Vec::new();
            collect_focus_order(&row, &mut focusables, &mut HashSet::new());
            let target = if reverse {
                focusables.last()
            } else {
                focusables.first()
            };
            if let Some((id, _)) = target {
                self.scroll_to_index(&list_id, index);
                self.focused = Some(id.clone());
                return true;
            }
        }
        false
    }

    fn reachable_popover_focusables(&self, scope: &PopoverFocusScope) -> Vec<(String, HitKind)> {
        scope
            .focusables
            .iter()
            .filter(|(id, kind)| {
                self.frame.scroll_focus_targets.contains_key(id)
                    || self
                        .frame
                        .regions
                        .iter()
                        .any(|region| region.id == *id && region.kind == *kind)
            })
            .cloned()
            .collect()
    }

    fn reveal_scroll_focus(&mut self, id: &str) {
        let Some(target) = self.frame.scroll_focus_targets.get(id) else {
            return;
        };
        let max_offset = self.scroll_max(&target.scroll_id);
        let offset = self
            .scroll_offsets
            .entry(target.scroll_id.clone())
            .or_default();
        let top = target.rect.y - (*offset - target.layout_offset);
        let bottom = top + target.rect.height;
        let viewport_bottom = target.viewport.y + target.viewport.height;
        if top < target.viewport.y {
            *offset = (*offset - (target.viewport.y - top)).clamp(0.0, max_offset);
        } else if bottom > viewport_bottom {
            *offset = (*offset + bottom - viewport_bottom).clamp(0.0, max_offset);
        }
    }

    fn active_popover_scope(&self) -> Option<&PopoverFocusScope> {
        let focused = self.focused.as_deref();
        self.frame
            .popover_focus
            .iter()
            .rev()
            .find(|scope| {
                focused.is_some_and(|id| {
                    scope.trigger_id.as_deref() == Some(id)
                        || scope
                            .focusables
                            .iter()
                            .any(|(candidate, _)| candidate == id)
                })
            })
            .or_else(|| self.frame.popover_focus.last())
    }

    fn step_tab(&mut self, key: Key, actions: &mut Vec<UiAction>) -> bool {
        let Some(focused) = self.focused.as_deref() else {
            return false;
        };
        let Some(scope) = self
            .frame
            .tab_focus
            .iter()
            .find(|scope| scope.action_ids.iter().any(|id| id == focused))
        else {
            return false;
        };
        let visible: Vec<_> = scope
            .action_ids
            .iter()
            .filter(|id| {
                self.frame
                    .regions
                    .iter()
                    .any(|region| region.id == **id && region.kind == HitKind::Button)
            })
            .collect();
        let Some(index) = visible.iter().position(|id| id.as_str() == focused) else {
            return false;
        };
        let next = match key {
            Key::ArrowLeft => (index + visible.len() - 1) % visible.len(),
            Key::ArrowRight => (index + 1) % visible.len(),
            Key::Home => 0,
            Key::End => visible.len() - 1,
            _ => return false,
        };
        if next != index {
            let id = visible[next].to_string();
            self.focused = Some(id.clone());
            actions.push(UiAction::Clicked { id });
        }
        true
    }

    fn step_popover_option(&mut self, key: Key) -> bool {
        let Some(scope) = self.active_popover_scope().cloned() else {
            return false;
        };
        let buttons: Vec<_> = self
            .reachable_popover_focusables(&scope)
            .into_iter()
            .filter(|(_, kind)| *kind == HitKind::Button)
            .collect();
        if buttons.is_empty() {
            return false;
        }
        let current = self
            .focused
            .as_ref()
            .and_then(|id| buttons.iter().position(|(candidate, _)| candidate == id));
        let trigger_focused = self.focused.is_none()
            || self
                .focused
                .as_ref()
                .is_some_and(|id| scope.trigger_id.as_ref() == Some(id));
        if current.is_none() && !trigger_focused {
            return false;
        }
        let next = match key {
            Key::ArrowDown => current.map_or(0, |index| (index + 1) % buttons.len()),
            Key::ArrowUp => current.map_or(buttons.len() - 1, |index| {
                (index + buttons.len() - 1) % buttons.len()
            }),
            Key::Home => 0,
            Key::End => buttons.len() - 1,
            _ => return false,
        };
        let id = buttons[next].0.clone();
        self.request_scroll_focus(&id);
        self.focused = Some(id);
        true
    }
}

const VIRTUAL_FOCUS_PREFIX: &str = "\0uniform_list:";

fn find_uniform_list<'a>(element: &'a Element, id: &str) -> Option<&'a UniformList> {
    match &element.kind {
        ElementKind::UniformList(list) if list.id == id => Some(list),
        ElementKind::Column(children)
        | ElementKind::Row(children)
        | ElementKind::TabStrip { children, .. } => children
            .iter()
            .find_map(|child| find_uniform_list(child, id)),
        ElementKind::Panel(child)
        | ElementKind::Clickable { child, .. }
        | ElementKind::Scroll { child, .. } => find_uniform_list(child, id),
        ElementKind::Overlay { base, floating } => {
            find_uniform_list(base, id).or_else(|| find_uniform_list(floating, id))
        }
        ElementKind::Popover {
            anchor, floating, ..
        } => find_uniform_list(anchor, id).or_else(|| find_uniform_list(floating, id)),
        ElementKind::Tooltip { anchor, .. } => find_uniform_list(anchor, id),
        _ => None,
    }
}

fn collect_focus_order(
    element: &Element,
    order: &mut Vec<(String, HitKind)>,
    seen: &mut HashSet<String>,
) {
    match &element.kind {
        ElementKind::Column(children)
        | ElementKind::Row(children)
        | ElementKind::TabStrip { children, .. } => {
            for child in children {
                collect_focus_order(child, order, seen);
            }
        }
        ElementKind::Panel(child) | ElementKind::Scroll { child, .. } => {
            collect_focus_order(child, order, seen);
        }
        ElementKind::UniformList(list) => {
            // Layout replaces this marker with focusable controls from the
            // visible rows. No row factory is called during this tree walk.
            order.push((
                format!("{VIRTUAL_FOCUS_PREFIX}{}", list.id),
                HitKind::Scroll,
            ));
        }
        ElementKind::Clickable { id, child } => {
            if seen.insert(id.clone()) {
                order.push((id.clone(), HitKind::Button));
            }
            collect_focus_order(child, order, seen);
        }
        ElementKind::Overlay { base, floating } => {
            collect_focus_order(base, order, seen);
            collect_focus_order(floating, order, seen);
        }
        ElementKind::Popover {
            anchor, floating, ..
        } => {
            collect_focus_order(anchor, order, seen);
            collect_focus_order(floating, order, seen);
        }
        ElementKind::Tooltip { anchor, .. } => collect_focus_order(anchor, order, seen),
        ElementKind::Button { id, .. } | ElementKind::IconButton { id, .. }
            if seen.insert(id.clone()) =>
        {
            order.push((id.clone(), HitKind::Button));
        }
        ElementKind::TextInput { id, .. } if seen.insert(id.clone()) => {
            order.push((id.clone(), HitKind::TextInput));
        }
        ElementKind::TextArea { id, .. } if seen.insert(id.clone()) => {
            order.push((id.clone(), HitKind::TextArea));
        }
        ElementKind::Button { .. }
        | ElementKind::IconButton { .. }
        | ElementKind::TextInput { .. }
        | ElementKind::TextArea { .. }
        | ElementKind::Text(_)
        | ElementKind::TruncatedText(_)
        | ElementKind::FormattedText(_)
        | ElementKind::Icon(_)
        | ElementKind::Spacer => {}
    }
}

fn focusable_id(element: &Element) -> Option<&str> {
    match &element.kind {
        ElementKind::Button { id, .. }
        | ElementKind::IconButton { id, .. }
        | ElementKind::Clickable { id, .. }
        | ElementKind::TextInput { id, .. }
        | ElementKind::TextArea { id, .. } => Some(id),
        _ => None,
    }
}

/// Record only the ancestors of the requested control. The normal layout can
/// skip every other clipped branch, even while locating an offscreen target.
fn find_focus_path(element: &Element, id: &str, path: &mut HashSet<usize>) -> bool {
    let found = if focusable_id(element) == Some(id) {
        true
    } else {
        match &element.kind {
            ElementKind::Column(children)
            | ElementKind::Row(children)
            | ElementKind::TabStrip { children, .. } => children
                .iter()
                .any(|child| find_focus_path(child, id, path)),
            ElementKind::Panel(child)
            | ElementKind::Clickable { child, .. }
            | ElementKind::Scroll { child, .. } => find_focus_path(child, id, path),
            ElementKind::Overlay { base, floating } => {
                find_focus_path(base, id, path) || find_focus_path(floating, id, path)
            }
            ElementKind::Popover {
                anchor, floating, ..
            } => find_focus_path(anchor, id, path) || find_focus_path(floating, id, path),
            ElementKind::Tooltip { anchor, .. } => find_focus_path(anchor, id, path),
            _ => false,
        }
    };
    if found {
        path.insert(element as *const Element as usize);
    }
    found
}

struct LayoutBuilder<'a> {
    frame: Frame,
    overlays: VecDeque<DeferredOverlay>,
    scroll_focus_context: Vec<ScrollFocusContext>,
    focus_path: &'a HashSet<usize>,
    focus_probe: Option<&'a str>,
    metrics: FontMetrics,
    sans_metrics: FontMetrics,
    text_measurer: Option<Arc<TextMeasurer>>,
    width_cache: RefCell<HashMap<(String, u32, u8), f32>>,
    inputs: &'a mut HashMap<String, InputState>,
    scroll_offsets: &'a mut HashMap<String, f32>,
    scroll_to_end: &'a mut HashSet<String>,
    scroll_to_index: &'a mut HashMap<String, usize>,
    hovered_scrollbar: Option<&'a str>,
    dragging_scrollbar: Option<&'a str>,
    focused: Option<&'a str>,
    hovered: Option<&'a str>,
    tooltip_hover_since: Option<&'a (String, Instant)>,
    keyboard_focus: bool,
    keyboard_tooltip_focus: bool,
    pressed: Option<&'a str>,
    selection: Option<&'a StaticSelection>,
    transitions: &'a mut HashMap<String, ColorTransition>,
    popover_transitions: &'a mut HashMap<String, ScalarTransition>,
    popover_exits: &'a HashMap<String, PopoverExit>,
    tab_indicators: &'a mut HashMap<String, TabIndicatorState>,
    observed_buttons: &'a mut HashSet<String>,
    observed_popovers: &'a mut HashSet<String>,
    popover_snapshots: &'a mut HashMap<String, PopoverSnapshot>,
    observed_tabs: &'a mut HashSet<String>,
    observed_tooltips: &'a mut HashSet<String>,
    reduced_motion: bool,
    now: Instant,
}

enum DeferredOverlay {
    Free {
        floating: Element,
        area: Rect,
    },
    Anchored {
        floating: Element,
        anchor: Rect,
        dismiss_id: String,
        placement: PopoverPlacement,
        gap: f32,
        trigger_id: Option<String>,
        focusables: Vec<(String, HitKind)>,
        animation_ms: u16,
    },
    Tooltip {
        anchor: Rect,
        title: String,
        shortcut: Option<String>,
        placement: TooltipPlacement,
        colors: TooltipStyle,
    },
}

fn place_popover(
    anchor: Rect,
    desired: Size,
    viewport: Rect,
    placement: PopoverPlacement,
    gap: f32,
) -> Rect {
    let inset = 8.0;
    let width = desired
        .width
        .max(0.0)
        .min((viewport.width - inset * 2.0).max(0.0));
    let height = desired
        .height
        .max(0.0)
        .min((viewport.height - inset * 2.0).max(0.0));
    let start = matches!(
        placement,
        PopoverPlacement::BottomStart | PopoverPlacement::TopStart
    );
    let prefer_below = matches!(
        placement,
        PopoverPlacement::BottomStart | PopoverPlacement::BottomEnd
    );
    let x = if start {
        anchor.x
    } else {
        anchor.x + anchor.width - width
    };
    let x = x.clamp(
        viewport.x + inset,
        (viewport.x + viewport.width - inset - width).max(viewport.x + inset),
    );
    let below = anchor.y + anchor.height + gap;
    let above = anchor.y - gap - height;
    let fits_below = below + height <= viewport.y + viewport.height - inset;
    let fits_above = above >= viewport.y + inset;
    let y = if prefer_below {
        if fits_below || !fits_above {
            below
        } else {
            above
        }
    } else if fits_above || !fits_below {
        above
    } else {
        below
    };
    let y = y.clamp(
        viewport.y + inset,
        (viewport.y + viewport.height - inset - height).max(viewport.y + inset),
    );
    Rect::new(x, y, width, height)
}

fn place_tooltip(
    anchor: Rect,
    desired: Size,
    viewport: Rect,
    preferred: TooltipPlacement,
) -> (Rect, TooltipPlacement) {
    let inset = 8.0;
    let gap = 8.0;
    let width = desired.width.min((viewport.width - inset * 2.0).max(0.0));
    let height = desired.height.min((viewport.height - inset * 2.0).max(0.0));
    let x = (anchor.x + anchor.width / 2.0 - width / 2.0).clamp(
        viewport.x + inset,
        (viewport.x + viewport.width - inset - width).max(viewport.x + inset),
    );
    let above = anchor.y - gap - height;
    let below = anchor.y + anchor.height + gap;
    let fits_above = above >= viewport.y + inset;
    let fits_below = below + height <= viewport.y + viewport.height - inset;
    let placement = match preferred {
        TooltipPlacement::Above if fits_above => TooltipPlacement::Above,
        TooltipPlacement::Below if fits_below => TooltipPlacement::Below,
        _ if fits_above => TooltipPlacement::Above,
        _ if fits_below => TooltipPlacement::Below,
        _ if anchor.y - viewport.y > viewport.y + viewport.height - anchor.y - anchor.height => {
            TooltipPlacement::Above
        }
        _ => TooltipPlacement::Below,
    };
    let y = match placement {
        TooltipPlacement::Above => above,
        TooltipPlacement::Below => below,
    }
    .clamp(
        viewport.y + inset,
        (viewport.y + viewport.height - inset - height).max(viewport.y + inset),
    );
    (Rect::new(x, y, width, height), placement)
}

impl LayoutBuilder<'_> {
    fn paint_tooltip(
        &mut self,
        anchor: Rect,
        title: &str,
        shortcut: Option<&str>,
        preferred: TooltipPlacement,
        colors: TooltipStyle,
        viewport: Rect,
    ) {
        let max_width = (viewport.width - 16.0).min(360.0);
        if max_width < 32.0 || viewport.height < 32.0 {
            return;
        }
        let title_style = Style {
            font_size: 12.5,
            font_family: FontFamily::Sans,
            ..Style::default()
        };
        let shortcut_style = Style {
            font_size: 11.0,
            font_family: FontFamily::Monospace,
            ..Style::default()
        };
        let content_max = max_width - 24.0;
        let shortcut = shortcut.filter(|value| !value.is_empty());
        let keycap_width = shortcut
            .map(|value| self.text_width(value, shortcut_style) + 12.0)
            .filter(|width| content_max - width - 8.0 >= 24.0);
        let title_budget = content_max - keycap_width.map_or(0.0, |width| width + 8.0);
        let title = truncate_measured(title, title_budget, |part| {
            self.text_width(part, title_style)
        });
        let text_width = self.text_width(&title, title_style);
        let body_width = (24.0 + text_width + keycap_width.map_or(0.0, |width| width + 8.0))
            .max(32.0)
            .min(max_width);
        let (body, placement) =
            place_tooltip(anchor, Size::new(body_width, 32.0), viewport, preferred);
        if body.width < 32.0 || body.height < 28.0 {
            return;
        }
        let tip_x = (anchor.x + anchor.width / 2.0).clamp(
            body.x + 12.0,
            (body.x + body.width - 12.0).max(body.x + 12.0),
        );
        let (outline, fill) = match placement {
            TooltipPlacement::Above => {
                let base = body.y + body.height - 1.0;
                (
                    [
                        Point::new(tip_x - 7.0, base),
                        Point::new(tip_x + 7.0, base),
                        Point::new(tip_x, base + 7.0),
                    ],
                    [
                        Point::new(tip_x - 5.5, base - 1.0),
                        Point::new(tip_x + 5.5, base - 1.0),
                        Point::new(tip_x, base + 5.0),
                    ],
                )
            }
            TooltipPlacement::Below => {
                let base = body.y + 1.0;
                (
                    [
                        Point::new(tip_x - 7.0, base),
                        Point::new(tip_x + 7.0, base),
                        Point::new(tip_x, base - 7.0),
                    ],
                    [
                        Point::new(tip_x - 5.5, base + 1.0),
                        Point::new(tip_x + 5.5, base + 1.0),
                        Point::new(tip_x, base - 5.0),
                    ],
                )
            }
        };
        self.frame.commands.push(DrawCommand::Shadow {
            rect: body,
            shadow: BoxShadow::new(colors.shadow, Point::new(0.0, 3.0), 10.0, 0.0),
            radius: 7.0,
        });
        self.frame.commands.push(DrawCommand::FillRect {
            rect: body,
            color: colors.background,
            radius: 7.0,
        });
        self.frame.commands.push(DrawCommand::StrokeRect {
            rect: body,
            color: colors.border,
            width: 1.0,
            radius: 7.0,
        });
        // Paint the pointer last so its fill covers the body border at the
        // join. The two nested triangles leave a crisp one-pixel outline.
        self.frame.commands.push(DrawCommand::FillTriangle {
            points: outline,
            color: colors.border,
        });
        self.frame.commands.push(DrawCommand::FillTriangle {
            points: fill,
            color: colors.background,
        });
        let text_y = body.y + (body.height - self.metrics_for(title_style).line_height) / 2.0;
        self.frame.commands.push(DrawCommand::Text {
            position: Point::new(body.x + 12.0, text_y),
            text: title,
            color: colors.text,
            font_size: title_style.font_size,
            font_family: title_style.font_family,
        });
        if let (Some(shortcut), Some(width)) = (shortcut, keycap_width) {
            let keycap = Rect::new(
                body.x + body.width - 12.0 - width,
                body.y + 5.0,
                width,
                22.0,
            );
            self.frame.commands.push(DrawCommand::FillRect {
                rect: keycap,
                color: colors.keycap,
                radius: 4.0,
            });
            self.frame.commands.push(DrawCommand::Text {
                position: Point::new(
                    keycap.x + 6.0,
                    keycap.y + (keycap.height - self.metrics_for(shortcut_style).line_height) / 2.0,
                ),
                text: shortcut.to_owned(),
                color: colors.muted,
                font_size: shortcut_style.font_size,
                font_family: shortcut_style.font_family,
            });
        }
    }

    fn paint_static_run(
        &mut self,
        block_index: usize,
        source: &str,
        mut run: SelectableTextRun,
        position: Point,
        color: Color,
        clip: Option<Rect>,
    ) {
        run.visible = clip
            .and_then(|area| run.rect.intersection(area))
            .or_else(|| clip.is_none().then_some(run.rect));
        if let Some(selection) = self
            .selection
            .filter(|selection| selection.block == block_index && selection.source == source)
        {
            if let Some((start, end)) = selection.range() {
                let start = start.max(run.start).min(run.end);
                let end = end.max(run.start).min(run.end);
                if end > start {
                    let style = Style {
                        font_size: run.font_size,
                        font_family: run.font_family,
                        ..Style::default()
                    };
                    let left = self.text_width(char_range(&run.text, 0, start - run.start), style);
                    let right = self.text_width(char_range(&run.text, 0, end - run.start), style);
                    self.frame.commands.push(DrawCommand::FillRect {
                        rect: Rect::new(
                            run.rect.x + left,
                            run.rect.y,
                            (right - left).max(0.0),
                            run.rect.height,
                        ),
                        color: Color::rgba(180, 180, 180, 84),
                        radius: 0.0,
                    });
                }
            }
        }
        self.frame.commands.push(DrawCommand::Text {
            position,
            text: run.text.clone(),
            color,
            font_size: run.font_size,
            font_family: run.font_family,
        });
        self.frame.text_blocks[block_index].runs.push(run);
    }

    fn metrics_for(&self, style: Style) -> FontMetrics {
        match style.font_family {
            FontFamily::Monospace => self.metrics,
            FontFamily::Sans => self.sans_metrics,
        }
        .at_size(style.font_size)
    }

    fn text_width(&self, value: &str, style: Style) -> f32 {
        let key = (
            value.to_owned(),
            style.font_size.to_bits(),
            u8::from(style.font_family == FontFamily::Sans),
        );
        if value.len() <= 256 {
            if let Some(width) = self.width_cache.borrow().get(&key) {
                return *width;
            }
        }
        let width = measured_width(
            self.text_measurer.as_deref(),
            value,
            style.font_size,
            style.font_family,
            self.metrics_for(style).char_width,
        );
        if value.len() <= 256 && self.width_cache.borrow().len() < 4096 {
            self.width_cache.borrow_mut().insert(key, width);
        }
        width
    }

    fn wrapped_lines<'t>(&self, value: &'t str, width: f32, style: Style) -> Vec<&'t str> {
        wrap_measured(value, width, |part| self.text_width(part, style))
    }

    fn formatted_lines(&self, spans: &[TextSpan], width: f32, style: Style) -> Vec<FormattedLine> {
        layout_formatted(
            spans,
            width,
            self.metrics_for(style).line_height,
            |text, span| {
                self.text_width(
                    text,
                    Style {
                        font_size: span.font_size,
                        font_family: span.font_family,
                        ..style
                    },
                )
            },
            |span| {
                self.metrics_for(Style {
                    font_size: span.font_size,
                    font_family: span.font_family,
                    ..style
                })
                .line_height
            },
        )
    }

    fn draw_focus_ring(&mut self, rect: Rect, radius: f32) {
        let edge = 2.0_f32.min(rect.width / 2.0).min(rect.height / 2.0);
        let ink = Color::rgb(170, 170, 170);
        if edge > 0.0 {
            self.frame.commands.push(DrawCommand::StrokeRect {
                rect,
                color: ink,
                width: edge,
                radius,
            });
        }
    }

    fn draw_icon(&mut self, icon: Icon, rect: Rect, style: Style) {
        self.frame.commands.push(DrawCommand::Icon {
            rect,
            icon,
            tint: style.foreground,
        });
    }

    fn paint_scrollbar(
        &mut self,
        id: &str,
        geometry: ScrollbarGeometry,
        content: Rect,
        clip: Option<Rect>,
        ink: Color,
        appearance: crate::ScrollbarAppearance,
    ) {
        let visible = clip
            .and_then(|outer| outer.intersection(content))
            .or_else(|| clip.is_none().then_some(content));
        let Some(visible) = visible else {
            return;
        };
        let ink = appearance.tint.unwrap_or(ink);
        let alpha = |opacity: u8| {
            Color::rgba(
                ink.r,
                ink.g,
                ink.b,
                ((u16::from(ink.a) * u16::from(opacity)) / 255) as u8,
            )
        };
        let active = self.dragging_scrollbar == Some(id);
        let hovered = self.hovered_scrollbar == Some(id);
        self.frame.commands.push(DrawCommand::PushClip(visible));
        // The overlay rail never paints a permanent gray stripe behind
        // content. Its track appears only while interacting with it.
        if active || hovered {
            self.frame.commands.push(DrawCommand::FillRect {
                rect: geometry.track,
                color: alpha(appearance.hover_track_opacity),
                radius: geometry.track.width / 2.0,
            });
        }
        self.frame.commands.push(DrawCommand::FillRect {
            rect: geometry.thumb,
            color: alpha(if active {
                appearance.active_thumb_opacity
            } else if hovered {
                appearance.hover_thumb_opacity
            } else {
                appearance.idle_thumb_opacity
            }),
            radius: geometry.thumb.width / 2.0,
        });
        self.frame.commands.push(DrawCommand::PopClip);
        self.add_region(id, geometry.track, HitKind::Scrollbar, Some(visible));
        self.frame.scrollbars.insert(id.to_owned(), geometry);
    }

    fn measure(&self, element: &Element, available: Size) -> Size {
        let style = element.style;
        let metrics = self.metrics_for(style);
        let padding = style.padding * 2.0;
        let inner = Size::new(
            (available.width - padding).max(0.0),
            (available.height - padding).max(0.0),
        );
        let natural = match &element.kind {
            ElementKind::Text(value) => {
                let lines = self.wrapped_lines(value, inner.width, style);
                let longest = lines
                    .iter()
                    .map(|line| self.text_width(line, style))
                    .fold(0.0, f32::max);
                Size::new(
                    longest + padding,
                    lines.len() as f32 * metrics.line_height + padding,
                )
            }
            ElementKind::TruncatedText(value) => Size::new(
                self.text_width(value, style) + padding,
                metrics.line_height + padding,
            ),
            ElementKind::FormattedText(spans) => {
                let width = style.width.unwrap_or(available.width);
                let lines = self.formatted_lines(spans, (width - padding).max(0.0), style);
                Size::new(
                    lines.iter().map(|line| line.width).fold(0.0, f32::max) + padding,
                    lines.iter().map(|line| line.height).sum::<f32>() + padding,
                )
            }
            ElementKind::Icon(_) => Size::new(ICON_SIZE + padding, ICON_SIZE + padding),
            ElementKind::Button {
                label,
                trailing_icon,
                ..
            } => Size::new(
                self.text_width(label, style)
                    + if trailing_icon.is_some() {
                        style.gap + ICON_SIZE
                    } else {
                        0.0
                    }
                    + padding,
                metrics.line_height.max(if trailing_icon.is_some() {
                    ICON_SIZE
                } else {
                    0.0
                }) + padding,
            ),
            ElementKind::IconButton {
                label,
                trailing_icon,
                ..
            } => Size::new(
                ICON_SIZE
                    + if label.is_empty() {
                        0.0
                    } else {
                        style.gap + self.text_width(label, style)
                    }
                    + if trailing_icon.is_some() {
                        style.gap + ICON_SIZE
                    } else {
                        0.0
                    }
                    + padding,
                ICON_SIZE.max(metrics.line_height) + padding,
            ),
            ElementKind::TextInput { .. } => Size::new(
                if available.width.is_finite() {
                    available.width
                } else {
                    240.0
                },
                metrics.line_height + padding,
            ),
            ElementKind::TextArea { .. } => Size::new(
                if available.width.is_finite() {
                    available.width
                } else {
                    240.0
                },
                metrics.line_height * 4.0 + padding,
            ),
            ElementKind::Spacer => Size::new(0.0, 0.0),
            ElementKind::Panel(child) | ElementKind::Clickable { child, .. } => {
                let child = self.measure(child, inner);
                Size::new(child.width + padding, child.height + padding)
            }
            ElementKind::Overlay { base, .. } => {
                let base = self.measure(base, inner);
                Size::new(base.width + padding, base.height + padding)
            }
            ElementKind::Popover { anchor, .. } => {
                let anchor = self.measure(anchor, inner);
                Size::new(anchor.width + padding, anchor.height + padding)
            }
            ElementKind::Tooltip { anchor, .. } => {
                let anchor = self.measure(anchor, inner);
                Size::new(anchor.width + padding, anchor.height + padding)
            }
            ElementKind::Scroll { child, .. } => {
                let height = if available.height.is_finite() {
                    available.height
                } else {
                    self.measure(child, Size::new(inner.width, f32::INFINITY))
                        .height
                        + padding
                };
                Size::new(available.width, height)
            }
            ElementKind::UniformList(list) => {
                let content_height = (list.row_count as f32 * list.row_height).min(f32::MAX / 4.0);
                let height = if available.height.is_finite() {
                    available.height
                } else {
                    content_height + padding
                };
                Size::new(available.width, height)
            }
            ElementKind::Column(children) => {
                let sizes: Vec<_> = children
                    .iter()
                    .map(|child| self.measure(child, Size::new(inner.width, f32::INFINITY)))
                    .collect();
                let width = sizes.iter().map(|size| size.width).fold(0.0_f32, f32::max);
                let height = sizes.iter().map(|size| size.height).sum::<f32>()
                    + style.gap * children.len().saturating_sub(1) as f32;
                Size::new(width + padding, height + padding)
            }
            ElementKind::Row(children) | ElementKind::TabStrip { children, .. } => {
                let sizes: Vec<_> = children
                    .iter()
                    .map(|child| self.measure(child, Size::new(f32::INFINITY, inner.height)))
                    .collect();
                let width = sizes.iter().map(|size| size.width).sum::<f32>()
                    + style.gap * children.len().saturating_sub(1) as f32;
                let height = sizes.iter().map(|size| size.height).fold(0.0_f32, f32::max);
                Size::new(width + padding, height + padding)
            }
        };
        let fill_width = matches!(
            element.kind,
            ElementKind::Column(_)
                | ElementKind::Row(_)
                | ElementKind::TabStrip { .. }
                | ElementKind::Panel(_)
                | ElementKind::Clickable { .. }
                | ElementKind::Overlay { .. }
                | ElementKind::Popover { .. }
                | ElementKind::Tooltip { .. }
                | ElementKind::Scroll { .. }
                | ElementKind::UniformList(_)
        );
        let width = style.width.unwrap_or_else(|| {
            if fill_width && available.width.is_finite() {
                available.width
            } else {
                natural.width
            }
        });
        Size::new(
            width.max(style.min_width),
            style.height.unwrap_or(natural.height).max(style.min_height),
        )
    }

    fn place(&mut self, element: &Element, rect: Rect, clip: Option<Rect>) {
        // Retain the un-clipped bounds of directly laid-out controls in a
        // scroll area. A menu can then focus its next option and reveal it,
        // even when that option has no visible hit region yet.
        self.record_scroll_focus_target(element, rect);
        if clip.is_some_and(|clip| rect.intersection(clip).is_none()) {
            if self.focus_probe.is_some()
                && self
                    .focus_path
                    .contains(&(element as *const Element as usize))
            {
                self.probe_scroll_focus(element, rect, clip);
            }
            return;
        }
        let mut style = element.style;
        if let ElementKind::Button { id, .. }
        | ElementKind::IconButton { id, .. }
        | ElementKind::Clickable { id, .. }
        | ElementKind::TextInput { id, .. }
        | ElementKind::TextArea { id, .. } = &element.kind
        {
            let hovered = self.hovered == Some(id.as_str())
                || element.hover_peer.as_deref() == self.hovered && self.hovered.is_some();
            if self.pressed == Some(id.as_str()) && hovered {
                style.background = style
                    .pressed_background
                    .or(style.hover_background)
                    .or(style.background);
                style.foreground = style
                    .pressed_foreground
                    .or(style.hover_foreground)
                    .unwrap_or(style.foreground);
            } else if hovered {
                style.background = style.hover_background.or(style.background);
                style.foreground = style.hover_foreground.unwrap_or(style.foreground);
            }
            if self.focused == Some(id.as_str())
                && matches!(
                    &element.kind,
                    ElementKind::TextInput { .. } | ElementKind::TextArea { .. }
                )
            {
                style.border = style.focus_border.or(style.border);
            }
            if !self.reduced_motion && style.transition_ms > 0 {
                self.observed_buttons.insert(id.clone());
                let target = style.background.unwrap_or(Color::TRANSPARENT);
                let state = self
                    .transitions
                    .entry(id.clone())
                    .or_insert_with(|| ColorTransition::new(target, self.now));
                state.retarget(
                    target,
                    self.now,
                    Duration::from_millis(u64::from(style.transition_ms)),
                );
                let color = state.color(self.now);
                style.background = (color.a > 0).then_some(color);
            }
        }
        let metrics = self.metrics_for(style);
        if let Some(shadow) = style.shadow {
            self.frame.commands.push(DrawCommand::Shadow {
                rect,
                shadow,
                radius: style.radius,
            });
        }
        if let Some(color) = style.background {
            self.frame.commands.push(DrawCommand::FillRect {
                rect,
                color,
                radius: style.radius,
            });
        }
        if let Some(border) = style.border {
            self.frame.commands.push(DrawCommand::StrokeRect {
                rect,
                color: border.color,
                width: border.width,
                radius: style.radius,
            });
        }
        let content = rect.inset(style.padding);
        let clip = if style.clip_children {
            let child_clip = clip
                .and_then(|outer| outer.intersection(content))
                .unwrap_or_else(|| {
                    if clip.is_some() {
                        Rect::new(content.x, content.y, 0.0, 0.0)
                    } else {
                        content
                    }
                });
            self.frame.commands.push(DrawCommand::PushClip(child_clip));
            Some(child_clip)
        } else {
            clip
        };
        match &element.kind {
            ElementKind::Column(children) => {
                self.place_flex(children, content, style.gap, false, clip);
            }
            ElementKind::Row(children) => {
                self.place_flex(children, content, style.gap, true, clip);
            }
            ElementKind::TabStrip {
                children,
                action_ids,
                selection,
            } => {
                let command_start = self.frame.commands.len();
                let rects = self.place_flex(children, content, style.gap, true, clip);
                if let Some(selection) = selection {
                    let index = children.iter().position(|child| match &child.kind {
                        ElementKind::Button { id, .. } | ElementKind::IconButton { id, .. } => {
                            id == &selection.selected_id
                        }
                        _ => false,
                    });
                    if let Some(target) = index.and_then(|index| rects.get(index)).copied() {
                        self.observed_tabs.insert(selection.group_id.clone());
                        let state = self
                            .tab_indicators
                            .entry(selection.group_id.clone())
                            .or_insert_with(|| TabIndicatorState {
                                selected_id: selection.selected_id.clone(),
                                x: ScalarTransition::new(target.x, self.now),
                                width: ScalarTransition::new(target.width, self.now),
                            });
                        if self.reduced_motion || selection.animation_ms == 0 {
                            state.x.snap_to(target.x, self.now);
                            state.width.snap_to(target.width, self.now);
                            state.selected_id.clone_from(&selection.selected_id);
                        } else if state.selected_id != selection.selected_id {
                            let duration = Duration::from_millis(u64::from(selection.animation_ms));
                            state.x.retarget(target.x, self.now, duration);
                            state.width.retarget(target.width, self.now, duration);
                            state.selected_id.clone_from(&selection.selected_id);
                        } else if state.x.target() != target.x
                            || state.width.target() != target.width
                        {
                            // Resizing a dock or window moves the entire strip;
                            // only changing selection should leave a sliding trail.
                            state.x.snap_to(target.x, self.now);
                            state.width.snap_to(target.width, self.now);
                        }
                        self.frame.commands.insert(
                            command_start,
                            DrawCommand::FillRect {
                                rect: Rect::new(
                                    state.x.value(self.now),
                                    target.y,
                                    state.width.value(self.now),
                                    target.height,
                                ),
                                color: selection.color,
                                radius: selection.radius,
                            },
                        );
                    }
                }
                self.frame.tab_focus.push(TabFocusScope {
                    action_ids: action_ids.clone(),
                });
            }
            ElementKind::Panel(child) => self.place(child, content, clip),
            ElementKind::Clickable { id, child } => {
                self.add_region(id, rect, HitKind::Button, clip);
                self.place(child, content, clip);
                if self.keyboard_focus && self.focused == Some(id.as_str()) {
                    self.draw_focus_ring(rect, style.radius);
                }
            }
            ElementKind::Overlay { base, floating } => {
                self.place(base, content, clip);
                self.overlays.push_back(DeferredOverlay::Free {
                    floating: floating.as_ref().clone(),
                    area: content,
                });
            }
            ElementKind::Popover {
                anchor,
                floating,
                dismiss_id,
                placement,
                gap,
                animation_ms,
            } => {
                self.place(anchor, content, clip);
                let mut seen = HashSet::new();
                let mut anchor_focus = Vec::new();
                collect_focus_order(anchor, &mut anchor_focus, &mut seen);
                let trigger_id = anchor_focus.first().map(|(id, _)| id.clone());
                let mut focusables = Vec::new();
                collect_focus_order(floating, &mut focusables, &mut seen);
                self.overlays.push_back(DeferredOverlay::Anchored {
                    floating: floating.as_ref().clone(),
                    anchor: content,
                    dismiss_id: dismiss_id.clone(),
                    placement: *placement,
                    gap: *gap,
                    trigger_id,
                    focusables,
                    animation_ms: *animation_ms,
                });
            }
            ElementKind::Tooltip {
                anchor,
                anchor_id,
                title,
                shortcut,
                placement,
                colors,
            } => {
                self.place(anchor, content, clip);
                self.observed_tooltips.insert(anchor_id.clone());
                let keyboard_focused =
                    self.keyboard_tooltip_focus && self.focused == Some(anchor_id);
                let hovered_long_enough = !self.keyboard_tooltip_focus
                    && self.hovered == Some(anchor_id)
                    && self.pressed != Some(anchor_id)
                    && self.tooltip_hover_since.is_some_and(|(id, since)| {
                        id == anchor_id
                            && self.now.saturating_duration_since(*since) >= TOOLTIP_HOVER_DELAY
                    });
                if keyboard_focused || hovered_long_enough {
                    self.overlays.push_back(DeferredOverlay::Tooltip {
                        anchor: content,
                        title: title.clone(),
                        shortcut: shortcut.clone(),
                        placement: *placement,
                        colors: *colors,
                    });
                }
            }
            ElementKind::Scroll {
                id,
                child,
                appearance,
            } => {
                self.add_region(id, rect, HitKind::Scroll, clip);
                let full_width_height = self
                    .measure(child, Size::new(content.width, f32::INFINITY))
                    .height;
                let child_width =
                    appearance.content_width(content.width, full_width_height > content.height);
                let child_height = if child_width < content.width {
                    self.measure(child, Size::new(child_width, f32::INFINITY))
                        .height
                } else {
                    full_width_height
                };
                let max_offset = (child_height - content.height).max(0.0);
                self.frame.scroll_extents.insert(id.clone(), max_offset);
                let offset = self.scroll_offsets.entry(id.clone()).or_default();
                if self.scroll_to_end.remove(id) {
                    *offset = max_offset;
                }
                *offset = offset.clamp(0.0, max_offset);
                let current_offset = *offset;
                let scrollbar =
                    ScrollbarGeometry::new(content, max_offset, current_offset, *appearance);
                let child_rect = Rect::new(
                    content.x,
                    content.y - current_offset,
                    child_width,
                    child_height.max(content.height),
                );
                let new_clip = clip
                    .and_then(|outer| outer.intersection(content))
                    .or_else(|| clip.is_none().then_some(content));
                if let Some(new_clip) = new_clip {
                    self.frame.commands.push(DrawCommand::PushClip(new_clip));
                    self.scroll_focus_context.push(ScrollFocusContext {
                        id: id.clone(),
                        viewport: new_clip,
                        offset: current_offset,
                    });
                    self.place(child, child_rect, Some(new_clip));
                    self.scroll_focus_context.pop();
                    self.frame.commands.push(DrawCommand::PopClip);
                }
                if let Some(scrollbar) = scrollbar {
                    self.paint_scrollbar(
                        id,
                        scrollbar,
                        content,
                        clip,
                        style.foreground,
                        *appearance,
                    );
                }
            }
            ElementKind::UniformList(list) => {
                let id = list.id.as_str();
                self.add_region(id, rect, HitKind::Scroll, clip);
                let content_height = (list.row_count as f32 * list.row_height).min(f32::MAX / 4.0);
                let max_offset = (content_height - content.height).max(0.0);
                self.frame
                    .scroll_extents
                    .insert(list.id.clone(), max_offset);
                let offset = self.scroll_offsets.entry(list.id.clone()).or_default();
                if self.scroll_to_end.remove(id) {
                    *offset = max_offset;
                }
                if let Some(index) = self.scroll_to_index.remove(id) {
                    if list.row_count > 0 {
                        let index = index.min(list.row_count - 1);
                        let top = index as f32 * list.row_height;
                        let bottom = top + list.row_height;
                        if top < *offset {
                            *offset = top;
                        } else if bottom > *offset + content.height {
                            *offset = bottom - content.height;
                        }
                    }
                }
                *offset = offset.clamp(0.0, max_offset);
                let current_offset = *offset;
                let scrollbar =
                    ScrollbarGeometry::new(content, max_offset, current_offset, list.appearance);
                let row_width = list
                    .appearance
                    .content_width(content.width, scrollbar.is_some());
                let first_visible = (current_offset / list.row_height).floor() as usize;
                let end_visible =
                    ((current_offset + content.height) / list.row_height).ceil() as usize;
                let first_visible = first_visible.min(list.row_count);
                let end_visible = end_visible.min(list.row_count);
                let visible = clip
                    .and_then(|outer| outer.intersection(content))
                    .or_else(|| clip.is_none().then_some(content));
                let mut rows = Vec::new();
                if let Some(visible) = visible {
                    self.frame.commands.push(DrawCommand::PushClip(visible));
                    for index in first_visible..end_visible {
                        let row = (list.build_row)(index);
                        let mut focusables = Vec::new();
                        let mut seen = HashSet::new();
                        collect_focus_order(&row, &mut focusables, &mut seen);
                        rows.push((index, focusables));
                        self.place(
                            &row,
                            Rect::new(
                                content.x,
                                content.y + index as f32 * list.row_height - current_offset,
                                row_width,
                                list.row_height,
                            ),
                            Some(visible),
                        );
                    }
                    self.frame.commands.push(DrawCommand::PopClip);
                }
                self.frame.virtual_lists.insert(
                    list.id.clone(),
                    VirtualListFocus {
                        first_visible,
                        end_visible,
                        row_count: list.row_count,
                        rows,
                    },
                );
                if let Some(scrollbar) = scrollbar {
                    self.paint_scrollbar(
                        id,
                        scrollbar,
                        content,
                        clip,
                        style.foreground,
                        list.appearance,
                    );
                }
            }
            ElementKind::TruncatedText(value) => {
                let shown =
                    truncate_measured(value, content.width, |part| self.text_width(part, style));
                self.frame.commands.push(DrawCommand::Text {
                    position: Point::new(
                        content.x,
                        content.y + (content.height - metrics.line_height) / 2.0,
                    ),
                    text: shown,
                    color: style.foreground,
                    font_size: style.font_size,
                    font_family: style.font_family,
                });
            }
            ElementKind::Text(value) => {
                let block_index = self.frame.text_blocks.len();
                self.frame.text_blocks.push(SelectableTextBlock {
                    source: value.clone(),
                    runs: Vec::new(),
                });
                let mut source_byte = 0;
                let mut source_char = 0;
                for (index, line) in self
                    .wrapped_lines(value, content.width, style)
                    .into_iter()
                    .enumerate()
                {
                    if index > 0 && value[source_byte..].starts_with('\n') {
                        source_byte += 1;
                        source_char += 1;
                    }
                    debug_assert!(value[source_byte..].starts_with(line));
                    let count = line.chars().count();
                    let y = content.y + index as f32 * metrics.line_height;
                    self.paint_static_run(
                        block_index,
                        value,
                        SelectableTextRun {
                            start: source_char,
                            end: source_char + count,
                            text: line.to_owned(),
                            rect: Rect::new(
                                content.x,
                                y,
                                self.text_width(line, style),
                                metrics.line_height,
                            ),
                            visible: None,
                            font_size: style.font_size,
                            font_family: style.font_family,
                        },
                        Point::new(content.x, y),
                        style.foreground,
                        clip,
                    );
                    source_byte += line.len();
                    source_char += count;
                }
            }
            ElementKind::FormattedText(spans) => {
                let source = spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>();
                let block_index = self.frame.text_blocks.len();
                self.frame.text_blocks.push(SelectableTextBlock {
                    source: source.clone(),
                    runs: Vec::new(),
                });
                let mut source_byte = 0;
                let mut source_char = 0;
                let mut y = content.y;
                for (line_index, line) in self
                    .formatted_lines(spans, content.width, style)
                    .into_iter()
                    .enumerate()
                {
                    if line_index > 0 && source[source_byte..].starts_with('\n') {
                        source_byte += 1;
                        source_char += 1;
                    }
                    let mut x = content.x;
                    for fragment in line.fragments {
                        if let Some(background) = fragment.background {
                            self.frame.commands.push(DrawCommand::FillRect {
                                rect: Rect::new(x, y, fragment.width, line.height),
                                color: background,
                                radius: 3.0,
                            });
                            x += 3.0;
                        }
                        debug_assert!(source[source_byte..].starts_with(&fragment.text));
                        let count = fragment.text.chars().count();
                        self.paint_static_run(
                            block_index,
                            &source,
                            SelectableTextRun {
                                start: source_char,
                                end: source_char + count,
                                text: fragment.text.clone(),
                                rect: Rect::new(
                                    x,
                                    y,
                                    fragment.width
                                        - if fragment.background.is_some() {
                                            6.0
                                        } else {
                                            0.0
                                        },
                                    line.height,
                                ),
                                visible: None,
                                font_size: fragment.font_size,
                                font_family: fragment.font_family,
                            },
                            Point::new(x, y + line.height - fragment.line_height),
                            fragment.color,
                            clip,
                        );
                        source_byte += fragment.text.len();
                        source_char += count;
                        x += fragment.width
                            - if fragment.background.is_some() {
                                3.0
                            } else {
                                0.0
                            };
                    }
                    y += line.height;
                }
            }
            ElementKind::Icon(icon) => {
                let side = ICON_SIZE.min(content.width).min(content.height).max(0.0);
                let icon_rect = Rect::new(
                    content.x + (content.width - side) / 2.0,
                    content.y + (content.height - side) / 2.0,
                    side,
                    side,
                );
                self.draw_icon(*icon, icon_rect, style);
            }
            ElementKind::Button {
                id,
                label,
                trailing_icon,
            } => {
                self.add_region(id, rect, HitKind::Button, clip);
                if self.keyboard_focus && self.focused == Some(id.as_str()) {
                    self.draw_focus_ring(rect, style.radius);
                }
                let trailing_side = if trailing_icon.is_some() {
                    ICON_SIZE.min(content.width).min(content.height).max(0.0)
                } else {
                    0.0
                };
                let label_width = (content.width
                    - if trailing_icon.is_some() {
                        trailing_side + style.gap
                    } else {
                        0.0
                    })
                .max(0.0);
                let shown =
                    truncate_measured(label, label_width, |part| self.text_width(part, style));
                self.frame.commands.push(DrawCommand::Text {
                    position: Point::new(
                        content.x,
                        content.y + (content.height - metrics.line_height) / 2.0,
                    ),
                    text: shown,
                    color: style.foreground,
                    font_size: style.font_size,
                    font_family: style.font_family,
                });
                if let Some(icon) = trailing_icon {
                    if trailing_side > 0.0 {
                        self.draw_icon(
                            *icon,
                            Rect::new(
                                content.x + content.width - trailing_side,
                                content.y + (content.height - trailing_side) / 2.0,
                                trailing_side,
                                trailing_side,
                            ),
                            style,
                        );
                    }
                }
            }
            ElementKind::IconButton {
                id,
                icon,
                label,
                trailing_icon,
            } => {
                self.add_region(id, rect, HitKind::Button, clip);
                if self.keyboard_focus && self.focused == Some(id.as_str()) {
                    self.draw_focus_ring(rect, style.radius);
                }
                let side = ICON_SIZE.min(content.width).min(content.height).max(0.0);
                let icon_rect = Rect::new(
                    if label.is_empty() && trailing_icon.is_none() {
                        content.x + (content.width - side) / 2.0
                    } else {
                        content.x
                    },
                    content.y + (content.height - side) / 2.0,
                    side,
                    side,
                );
                self.draw_icon(*icon, icon_rect, style);
                let trailing_side = if trailing_icon.is_some() {
                    ICON_SIZE
                        .min((content.width - side - style.gap).max(0.0))
                        .min(content.height)
                } else {
                    0.0
                };
                if !label.is_empty() {
                    let shown = truncate_measured(
                        label,
                        (content.width
                            - side
                            - style.gap
                            - if trailing_icon.is_some() {
                                trailing_side + style.gap
                            } else {
                                0.0
                            })
                        .max(0.0),
                        |part| self.text_width(part, style),
                    );
                    self.frame.commands.push(DrawCommand::Text {
                        position: Point::new(
                            content.x + side + style.gap,
                            content.y + (content.height - metrics.line_height) / 2.0,
                        ),
                        text: shown,
                        color: style.foreground,
                        font_size: style.font_size,
                        font_family: style.font_family,
                    });
                }
                if let Some(trailing_icon) = trailing_icon {
                    if trailing_side > 0.0 {
                        self.draw_icon(
                            *trailing_icon,
                            Rect::new(
                                content.x + content.width - trailing_side,
                                content.y + (content.height - trailing_side) / 2.0,
                                trailing_side,
                                trailing_side,
                            ),
                            style,
                        );
                    }
                }
            }
            ElementKind::TextInput {
                id,
                initial_value,
                placeholder,
                leading_icon,
                leading_icon_tint,
            } => {
                self.add_region(id, rect, HitKind::TextInput, clip);
                let content = if let Some(icon) = leading_icon {
                    let side = ICON_SIZE.min(content.width).min(content.height).max(0.0);
                    self.draw_icon(
                        *icon,
                        Rect::new(
                            content.x,
                            content.y + (content.height - side) / 2.0,
                            side,
                            side,
                        ),
                        Style {
                            foreground: if self.focused == Some(id.as_str()) {
                                style.foreground
                            } else {
                                leading_icon_tint.unwrap_or(style.foreground)
                            },
                            ..style
                        },
                    );
                    let inset = (ICON_SIZE + 8.0).min(content.width);
                    Rect::new(
                        content.x + inset,
                        content.y,
                        content.width - inset,
                        content.height,
                    )
                } else {
                    content
                };
                let state = self
                    .inputs
                    .entry(id.clone())
                    .or_insert_with(|| InputState::new(initial_value.clone()));
                state.content_x = content.x;
                state.char_width = metrics.char_width;
                state.content_width = content.width;
                state.font_size = style.font_size;
                state.font_family = style.font_family;
                state.measurer = self.text_measurer.clone();
                if state.cursor < state.view_start {
                    state.view_start = state.cursor;
                }
                if state.width(char_range(&state.value, state.view_start, state.cursor))
                    > content.width - 2.0
                {
                    let mut lo = state.view_start;
                    let mut hi = state.cursor;
                    while lo < hi {
                        let mid = lo + (hi - lo) / 2;
                        if state.width(char_range(&state.value, mid, state.cursor))
                            <= content.width - 2.0
                        {
                            hi = mid;
                        } else {
                            lo = mid + 1;
                        }
                    }
                    state.view_start = lo;
                }
                let remaining =
                    char_range(&state.value, state.view_start, state.value.chars().count());
                let visible_chars =
                    fitting_chars(remaining, content.width, |part| state.width(part));
                let visible_end = state.view_start + visible_chars;
                let is_focused = self.focused == Some(id.as_str());
                let shown = if state.value.is_empty() && !is_focused {
                    placeholder.clone()
                } else {
                    char_range(&state.value, state.view_start, visible_end).to_owned()
                };
                let input_clip = clip
                    .and_then(|outer| outer.intersection(content))
                    .or_else(|| clip.is_none().then_some(content));
                if let Some(input_clip) = input_clip {
                    self.frame.commands.push(DrawCommand::PushClip(input_clip));
                    if let Some((selection_start, selection_end)) = state.selected_range() {
                        let start = selection_start.clamp(state.view_start, visible_end);
                        let end = selection_end.clamp(state.view_start, visible_end);
                        if end > start {
                            let left =
                                state.width(char_range(&state.value, state.view_start, start));
                            let right =
                                state.width(char_range(&state.value, state.view_start, end));
                            self.frame.commands.push(DrawCommand::FillRect {
                                rect: Rect::new(
                                    content.x + left,
                                    content.y,
                                    right - left,
                                    metrics.line_height,
                                ),
                                color: Color::rgba(75, 130, 210, 130),
                                radius: 0.0,
                            });
                        }
                    }
                    self.frame.commands.push(DrawCommand::Text {
                        position: Point::new(content.x, content.y),
                        text: shown,
                        color: if state.value.is_empty() && !is_focused {
                            Color::rgba(
                                style.foreground.r,
                                style.foreground.g,
                                style.foreground.b,
                                110,
                            )
                        } else {
                            style.foreground
                        },
                        font_size: style.font_size,
                        font_family: style.font_family,
                    });
                    if is_focused {
                        let cursor_x =
                            state.width(char_range(&state.value, state.view_start, state.cursor));
                        self.frame.commands.push(DrawCommand::FillRect {
                            rect: Rect::new(
                                content.x + cursor_x,
                                content.y,
                                1.5,
                                metrics.line_height,
                            ),
                            color: style.foreground,
                            radius: 0.0,
                        });
                    }
                    self.frame.commands.push(DrawCommand::PopClip);
                }
            }
            ElementKind::TextArea {
                id,
                initial_value,
                placeholder,
            } => {
                self.add_region(id, rect, HitKind::TextArea, clip);
                let state = self.inputs.entry(id.clone()).or_insert_with(|| {
                    InputState::new(initial_value.replace("\r\n", "\n").replace('\r', "\n"))
                });
                if state.value.contains('\r') {
                    state.value = state.value.replace("\r\n", "\n").replace('\r', "\n");
                    state.cursor = state.value.chars().count();
                    state.anchor = None;
                }
                state.content_x = content.x;
                state.content_y = content.y;
                state.char_width = metrics.char_width;
                state.line_height = metrics.line_height;
                state.content_width = content.width;
                state.font_size = style.font_size;
                state.font_family = style.font_family;
                state.measurer = self.text_measurer.clone();
                state.visible_lines =
                    (content.height / metrics.line_height).floor().max(1.0) as usize;
                let lines = state.visual_lines();
                let (cursor_row, cursor_column) = visual_cursor(&lines, state.cursor);
                let max_scroll = lines.len().saturating_sub(state.visible_lines);
                state.scroll_line = state.scroll_line.min(max_scroll);
                if state.keep_cursor_visible {
                    if cursor_row < state.scroll_line {
                        state.scroll_line = cursor_row;
                    } else if cursor_row >= state.scroll_line + state.visible_lines {
                        state.scroll_line = cursor_row + 1 - state.visible_lines;
                    }
                    state.keep_cursor_visible = false;
                }

                let is_focused = self.focused == Some(id.as_str());
                let is_placeholder = state.value.is_empty() && !is_focused;
                let placeholder_lines;
                let shown_lines = if is_placeholder {
                    placeholder_lines =
                        visual_lines_measured(placeholder, content.width, |part| state.width(part));
                    &placeholder_lines
                } else {
                    &lines
                };
                let shown_start = if is_placeholder { 0 } else { state.scroll_line };
                let input_clip = clip
                    .and_then(|outer| outer.intersection(content))
                    .or_else(|| clip.is_none().then_some(content));
                if let Some(input_clip) = input_clip {
                    self.frame.commands.push(DrawCommand::PushClip(input_clip));
                    for (visible_row, line) in shown_lines
                        .iter()
                        .skip(shown_start)
                        .take(state.visible_lines)
                        .enumerate()
                    {
                        let y = content.y + visible_row as f32 * metrics.line_height;
                        if !is_placeholder {
                            if let Some((selection_start, selection_end)) = state.selected_range() {
                                let first = selection_start.max(line.start).min(line.end);
                                let last = selection_end.min(line.end).max(first);
                                if last > first {
                                    let left =
                                        state.width(char_range(&line.text, 0, first - line.start));
                                    let right =
                                        state.width(char_range(&line.text, 0, last - line.start));
                                    self.frame.commands.push(DrawCommand::FillRect {
                                        rect: Rect::new(
                                            content.x + left,
                                            y,
                                            right - left,
                                            metrics.line_height,
                                        ),
                                        color: Color::rgba(75, 130, 210, 130),
                                        radius: 0.0,
                                    });
                                }
                            }
                        }
                        self.frame.commands.push(DrawCommand::Text {
                            position: Point::new(content.x, y),
                            text: line.text.clone(),
                            color: if is_placeholder {
                                Color::rgba(
                                    style.foreground.r,
                                    style.foreground.g,
                                    style.foreground.b,
                                    110,
                                )
                            } else {
                                style.foreground
                            },
                            font_size: style.font_size,
                            font_family: style.font_family,
                        });
                    }
                    if is_focused && cursor_row >= state.scroll_line {
                        let visible_row = cursor_row - state.scroll_line;
                        if visible_row < state.visible_lines {
                            self.frame.commands.push(DrawCommand::FillRect {
                                rect: Rect::new(
                                    content.x
                                        + state.width(char_range(
                                            &lines[cursor_row].text,
                                            0,
                                            cursor_column,
                                        )),
                                    content.y + visible_row as f32 * metrics.line_height,
                                    1.5,
                                    metrics.line_height,
                                ),
                                color: style.foreground,
                                radius: 0.0,
                            });
                        }
                    }
                    self.frame.commands.push(DrawCommand::PopClip);
                }
            }
            ElementKind::Spacer => {}
        }
        if let Some((color, height)) = style.top_fade {
            let height = height.min(content.height);
            if content.width > 0.0 && height > 0.0 {
                self.frame.commands.push(DrawCommand::FillVerticalGradient {
                    rect: Rect::new(content.x, content.y, content.width, height),
                    top: color,
                    bottom: Color { a: 0, ..color },
                });
            }
        }
        if style.clip_children {
            self.frame.commands.push(DrawCommand::PopClip);
        }
    }

    fn record_scroll_focus_target(&mut self, element: &Element, rect: Rect) {
        if let (Some(scroll), Some(id)) = (self.scroll_focus_context.last(), focusable_id(element))
        {
            self.frame.scroll_focus_targets.insert(
                id.to_owned(),
                ScrollFocusTarget {
                    scroll_id: scroll.id.clone(),
                    rect,
                    viewport: scroll.viewport,
                    layout_offset: scroll.offset,
                },
            );
        }
    }

    /// Resolve the requested control's geometry inside a clipped branch.
    /// This does not paint, add hit regions, or visit unrelated descendants.
    fn probe_scroll_focus(&mut self, element: &Element, rect: Rect, clip: Option<Rect>) {
        if focusable_id(element) == self.focus_probe {
            self.record_scroll_focus_target(element, rect);
            return;
        }
        let style = element.style;
        let content = rect.inset(style.padding);
        let clip = if style.clip_children {
            Some(
                clip.and_then(|outer| outer.intersection(content))
                    .unwrap_or_else(|| {
                        if clip.is_some() {
                            Rect::new(content.x, content.y, 0.0, 0.0)
                        } else {
                            content
                        }
                    }),
            )
        } else {
            clip
        };
        match &element.kind {
            ElementKind::Column(children)
            | ElementKind::Row(children)
            | ElementKind::TabStrip { children, .. } => {
                let row = !matches!(&element.kind, ElementKind::Column(_));
                for (child, child_rect) in children
                    .iter()
                    .zip(self.flex_rects(children, content, style.gap, row))
                {
                    if self
                        .focus_path
                        .contains(&(child as *const Element as usize))
                    {
                        self.probe_scroll_focus(child, child_rect, clip);
                        break;
                    }
                }
            }
            ElementKind::Panel(child)
            | ElementKind::Clickable { child, .. }
            | ElementKind::Tooltip { anchor: child, .. } => {
                self.probe_scroll_focus(child, content, clip);
            }
            ElementKind::Scroll {
                id,
                child,
                appearance,
            } => {
                let full_width_height = self
                    .measure(child, Size::new(content.width, f32::INFINITY))
                    .height;
                let child_width =
                    appearance.content_width(content.width, full_width_height > content.height);
                let child_height = if child_width < content.width {
                    self.measure(child, Size::new(child_width, f32::INFINITY))
                        .height
                } else {
                    full_width_height
                };
                let max_offset = (child_height - content.height).max(0.0);
                self.frame.scroll_extents.insert(id.clone(), max_offset);
                let offset = self.scroll_offsets.entry(id.clone()).or_default();
                *offset = offset.clamp(0.0, max_offset);
                let current_offset = *offset;
                let child_rect = Rect::new(
                    content.x,
                    content.y - current_offset,
                    child_width,
                    child_height.max(content.height),
                );
                let new_clip = clip
                    .and_then(|outer| outer.intersection(content))
                    .or_else(|| clip.is_none().then_some(content));
                if let Some(new_clip) = new_clip {
                    self.scroll_focus_context.push(ScrollFocusContext {
                        id: id.clone(),
                        viewport: new_clip,
                        offset: current_offset,
                    });
                    self.probe_scroll_focus(child, child_rect, Some(new_clip));
                    self.scroll_focus_context.pop();
                } else {
                    // The outer scroll area must reveal this nested area first.
                    self.probe_scroll_focus(child, child_rect, clip);
                }
            }
            ElementKind::Overlay { base, .. }
                if self
                    .focus_path
                    .contains(&(base.as_ref() as *const Element as usize)) =>
            {
                self.probe_scroll_focus(base, content, clip);
            }
            ElementKind::Popover { anchor, .. }
                if self
                    .focus_path
                    .contains(&(anchor.as_ref() as *const Element as usize)) =>
            {
                self.probe_scroll_focus(anchor, content, clip);
            }
            _ => {}
        }
    }

    fn place_flex(
        &mut self,
        children: &[Element],
        content: Rect,
        gap: f32,
        row: bool,
        clip: Option<Rect>,
    ) -> Vec<Rect> {
        let rects = self.flex_rects(children, content, gap, row);
        for (child, rect) in children.iter().zip(&rects) {
            self.place(child, *rect, clip);
        }
        rects
    }

    fn flex_rects(
        &mut self,
        children: &[Element],
        content: Rect,
        gap: f32,
        row: bool,
    ) -> Vec<Rect> {
        let count = children.len();
        if count == 0 {
            return Vec::new();
        }
        let available_main = if row { content.width } else { content.height };
        let available_cross = if row { content.height } else { content.width };
        let mut fixed = gap * count.saturating_sub(1) as f32;
        let mut weights = 0.0;
        let mut sizes = Vec::with_capacity(count);
        for child in children {
            let measured = if row {
                self.measure(child, Size::new(f32::INFINITY, available_cross))
            } else {
                self.measure(child, Size::new(available_cross, f32::INFINITY))
            };
            if child.style.flex > 0.0 {
                weights += child.style.flex;
            } else {
                fixed += if row { measured.width } else { measured.height };
            }
            sizes.push(measured);
        }
        let flexible = (available_main - fixed).max(0.0);
        let mut offset = 0.0;
        let mut rects = Vec::with_capacity(count);
        for (child, measured) in children.iter().zip(sizes) {
            let main = if child.style.flex > 0.0 && weights > 0.0 {
                flexible * child.style.flex / weights
            } else if row {
                measured.width
            } else {
                measured.height
            };
            let cross = if row {
                child
                    .style
                    .height
                    .unwrap_or(available_cross)
                    .min(available_cross)
            } else {
                child
                    .style
                    .width
                    .unwrap_or(available_cross)
                    .min(available_cross)
            };
            let rect = if row {
                Rect::new(content.x + offset, content.y, main.max(0.0), cross.max(0.0))
            } else {
                Rect::new(content.x, content.y + offset, cross.max(0.0), main.max(0.0))
            };
            rects.push(rect);
            offset += main + gap;
        }
        rects
    }

    fn add_region(&mut self, id: &str, rect: Rect, kind: HitKind, clip: Option<Rect>) {
        let visible = match clip {
            Some(clip) => rect.intersection(clip),
            None => Some(rect),
        };
        if let Some(rect) = visible {
            self.frame.regions.push(HitRegion {
                id: id.to_owned(),
                rect,
                kind,
            });
        }
    }
}

#[cfg(test)]
fn wrap_lines(value: &str, width: f32, char_width: f32) -> Vec<String> {
    wrap_measured(value, width, |part| {
        part.chars().count() as f32 * char_width
    })
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Component, ComponentPalette, Tab, Tabs, Tooltip};

    #[test]
    fn clip_children_limits_descendant_hitboxes_without_clipping_parent_surface() {
        let parent_color = Color::rgb(29, 31, 34);
        let clipped = Element::panel(Element::row(vec![
            Element::button("wide_child", "Action").size(Some(150.0), Some(28.0))
        ]))
        .background(parent_color)
        .border(Color::rgb(70, 70, 70), 1.0)
        .padding(4.0)
        .clip_children()
        .size(Some(80.0), Some(40.0));
        let frame = UiTree::new(clipped).layout(Size::new(80.0, 40.0));
        let child = frame
            .regions
            .iter()
            .find(|region| region.id == "wide_child")
            .unwrap();
        assert_eq!(child.rect, Rect::new(4.0, 4.0, 72.0, 28.0));
        assert_eq!(frame.hit_test(Point::new(75.0, 16.0)), Some("wide_child"));
        assert_eq!(frame.hit_test(Point::new(90.0, 16.0)), None);
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, .. }
                if *rect == Rect::new(0.0, 0.0, 80.0, 40.0) && *color == parent_color
        )));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::PushClip(rect) if *rect == Rect::new(4.0, 4.0, 72.0, 32.0)
        )));
    }

    #[test]
    fn tab_indicator_moves_without_moving_click_regions_and_reduced_motion_snaps() {
        let palette = ComponentPalette::neutral_dark();
        let make_tabs = |active: &str| {
            Tabs::new(
                vec![
                    Tab::new("group", "Grupo", 80.0).active(active == "group"),
                    Tab::new("project", "Proyecto", 96.0).active(active == "project"),
                ],
                palette,
            )
            .id("session-grouping")
            .render()
        };
        let start = Instant::now();
        let mut tree = UiTree::new(make_tabs("group"));
        let viewport = Size::new(240.0, 32.0);
        let initial = tree.layout_at(viewport, start);
        let indicator = |frame: &Frame| {
            frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::FillRect { rect, color, .. } if *color == palette.selected => {
                        Some(*rect)
                    }
                    _ => None,
                })
                .expect("selected tab must have a painted indicator")
        };
        let start_rect = indicator(&initial);
        let project_region = initial
            .regions
            .iter()
            .find(|region| region.id == "project")
            .unwrap()
            .rect;
        tree.set_root(make_tabs("project"));
        let switched = tree.layout_at(viewport, start + Duration::from_millis(10));
        assert_eq!(indicator(&switched).x, start_rect.x);
        let halfway = tree.layout_at(viewport, start + Duration::from_millis(95));
        assert!(indicator(&halfway).x > start_rect.x);
        assert!(indicator(&halfway).x < project_region.x);
        assert_eq!(
            halfway.hit_test(Point::new(project_region.x + 5.0, 16.0)),
            Some("project")
        );
        let complete = tree.layout_at(viewport, start + Duration::from_millis(180));
        assert_eq!(indicator(&complete).x, project_region.x);
        tree.set_reduced_motion(true);
        tree.set_root(make_tabs("group"));
        assert_eq!(
            indicator(&tree.layout_at(viewport, start + Duration::from_millis(190))).x,
            start_rect.x
        );
    }

    #[test]
    fn popover_entrance_fades_paint_but_keeps_stable_targets() {
        let popup_color = Color::rgb(71, 75, 81);
        let make_open = || {
            Element::popover(
                Element::button("trigger", "Abrir").size(Some(92.0), Some(30.0)),
                Element::panel(Element::button("option", "Opción").size(Some(112.0), Some(34.0)))
                    .background(popup_color)
                    .size(Some(124.0), Some(42.0)),
                "dismiss",
                PopoverPlacement::BottomStart,
            )
        };
        let start = Instant::now();
        let viewport = Size::new(300.0, 200.0);
        let mut tree = UiTree::new(make_open());
        let first = tree.layout_at(viewport, start);
        let target = first
            .regions
            .iter()
            .find(|region| region.id == "option")
            .unwrap()
            .rect;
        let popup_fill = |frame: &Frame| {
            frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::FillRect { rect, color, .. }
                        if (color.r, color.g, color.b)
                            == (popup_color.r, popup_color.g, popup_color.b) =>
                    {
                        Some((*rect, *color))
                    }
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(popup_fill(&first).1.a, 0);
        let halfway = tree.layout_at(viewport, start + Duration::from_millis(70));
        let (animated_rect, animated_color) = popup_fill(&halfway);
        assert!(animated_color.a > 0 && animated_color.a < 255);
        assert_eq!(
            halfway.hit_test(Point::new(target.x + 8.0, target.y + 8.0)),
            Some("option")
        );
        let complete = tree.layout_at(viewport, start + Duration::from_millis(140));
        assert_eq!(popup_fill(&complete).1.a, 255);
        assert!((animated_rect.y - popup_fill(&complete).0.y).abs() > 0.1);
        assert!(tree.focus("option"));
        tree.set_root(Element::button("trigger", "Abrir"));
        let closed = tree.layout_at(viewport, start + Duration::from_millis(150));
        assert!(tree.popover_transitions.is_empty());
        assert_eq!(tree.focused_id(), Some("trigger"));
        assert_eq!(popup_fill(&closed).1.a, 255);
        assert!(!closed.regions.iter().any(|region| {
            region.id == "option"
                || region.id == "dismiss"
                || region.id == "popover_surface:dismiss"
        }));
        assert_ne!(
            closed.hit_test(Point::new(target.x + 8.0, target.y + 8.0)),
            Some("option")
        );
        assert!(tree
            .handle_event(UiEvent::Key {
                key: Key::Escape,
                shift: false,
                ctrl: false,
            })
            .is_empty());
        let exiting = tree.layout_at(viewport, start + Duration::from_millis(205));
        assert!(popup_fill(&exiting).1.a > 0 && popup_fill(&exiting).1.a < 255);
        let gone = tree.layout_at(viewport, start + Duration::from_millis(260));
        assert!(!gone.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. }
                if (color.r, color.g, color.b)
                    == (popup_color.r, popup_color.g, popup_color.b)
        )));
        tree.set_reduced_motion(true);
        tree.set_root(make_open());
        let reduced = tree.layout_at(viewport, start + Duration::from_millis(270));
        assert_eq!(popup_fill(&reduced).1.a, 255);
        tree.set_root(Element::button("trigger", "Abrir"));
        let reduced_close = tree.layout_at(viewport, start + Duration::from_millis(280));
        assert!(!reduced_close.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. }
                if (color.r, color.g, color.b)
                    == (popup_color.r, popup_color.g, popup_color.b)
        )));
    }

    #[test]
    fn closing_during_popover_entrance_preserves_current_opacity() {
        let start = Instant::now();
        let viewport = Size::new(300.0, 200.0);
        let popup_color = Color::rgb(111, 112, 113);
        let open = || {
            Element::popover(
                Element::button("trigger", "Open"),
                Element::panel(Element::text("Item"))
                    .background(popup_color)
                    .size(Some(100.0), Some(40.0)),
                "dismiss",
                PopoverPlacement::BottomStart,
            )
        };
        let mut tree = UiTree::new(open());
        tree.layout_at(viewport, start);
        let entering = tree.layout_at(viewport, start + Duration::from_millis(40));
        let opacity = |frame: &Frame| {
            frame.commands.iter().find_map(|command| match command {
                DrawCommand::FillRect { color, .. }
                    if (color.r, color.g, color.b)
                        == (popup_color.r, popup_color.g, popup_color.b) =>
                {
                    Some(color.a)
                }
                _ => None,
            })
        };
        let visible = opacity(&entering).unwrap();
        assert!(visible > 0 && visible < 255);
        tree.set_root(Element::button("trigger", "Open"));
        let closing = tree.layout_at(viewport, start + Duration::from_millis(40));
        assert_eq!(opacity(&closing), Some(visible));
        let fading = tree.layout_at(viewport, start + Duration::from_millis(80));
        let fading_alpha = opacity(&fading).unwrap();
        assert!(fading_alpha < visible);
        tree.set_root(open());
        let reopened = tree.layout_at(viewport, start + Duration::from_millis(80));
        assert_eq!(opacity(&reopened), Some(fading_alpha));
        assert!(tree.popover_exits.is_empty());
    }

    #[test]
    fn tooltip_preserves_anchor_bounds_and_has_no_hit_region() {
        let palette = ComponentPalette::neutral_dark();
        let anchor =
            Element::icon_button("toggle", Icon::PanelLeft, "").size(Some(32.0), Some(32.0));
        let baseline = UiTree::new(anchor.clone()).layout(Size::new(200.0, 100.0));
        let wrapped = Tooltip::new(anchor, "toggle", "Toggle sidebar", palette)
            .shortcut("Ctrl+B")
            .render();
        let mut tree = UiTree::new(wrapped);
        let frame = tree.layout(Size::new(200.0, 100.0));
        assert_eq!(frame.regions, baseline.regions);
        assert!(!frame
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::FillTriangle { .. })));
        assert!(tree.focus("toggle"));
        let focused = tree.layout(Size::new(200.0, 100.0));
        assert_eq!(focused.regions, baseline.regions);
        assert_eq!(
            focused
                .commands
                .iter()
                .filter(|command| matches!(command, DrawCommand::FillTriangle { .. }))
                .count(),
            2,
        );
        assert!(focused.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text == "Toggle sidebar"
        )));
        assert!(focused.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text == "Ctrl+B"
        )));
    }

    #[test]
    fn tooltip_hover_waits_and_keyboard_focus_is_immediate() {
        let palette = ComponentPalette::neutral_dark();
        let mut tree = UiTree::new(
            Tooltip::new(
                Element::icon_button("toggle", Icon::PanelLeft, "").size(Some(32.0), Some(32.0)),
                "toggle",
                "Toggle sidebar",
                palette,
            )
            .render(),
        );
        tree.layout(Size::new(200.0, 100.0));
        tree.handle_event(UiEvent::PointerMove(Point::new(16.0, 16.0)));
        let started = tree.tooltip_hover_since.as_ref().unwrap().1;
        let early = tree.layout_at(Size::new(200.0, 100.0), started + TOOLTIP_HOVER_DELAY / 2);
        assert!(!early
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::FillTriangle { .. })));
        let late = tree.layout_at(Size::new(200.0, 100.0), started + TOOLTIP_HOVER_DELAY);
        assert!(late
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::FillTriangle { .. })));
        tree.handle_event(UiEvent::PointerMove(Point::new(150.0, 80.0)));
        let left = tree.layout(Size::new(200.0, 100.0));
        assert!(!left
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::FillTriangle { .. })));
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        let focused = tree.layout(Size::new(200.0, 100.0));
        assert!(focused
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::FillTriangle { .. })));
    }

    #[test]
    fn pointer_hover_and_keyboard_focus_do_not_show_two_tooltips() {
        let palette = ComponentPalette::neutral_dark();
        let tooltip = |id, title| {
            Tooltip::new(
                Element::icon_button(id, Icon::Add, "").size(Some(32.0), Some(32.0)),
                id,
                title,
                palette,
            )
            .render()
        };
        let mut tree = UiTree::new(Element::row(vec![
            tooltip("first", "First tooltip"),
            tooltip("second", "Second tooltip"),
        ]));
        tree.layout(Size::new(160.0, 80.0));
        tree.handle_event(UiEvent::PointerMove(Point::new(48.0, 16.0)));
        tree.tooltip_hover_since = Some(("second".into(), Instant::now() - TOOLTIP_HOVER_DELAY));
        assert!(tree.focus("first"));
        let frame = tree.layout(Size::new(160.0, 80.0));
        let titles: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. }
                    if matches!(text.as_str(), "First tooltip" | "Second tooltip") =>
                {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(titles, vec!["First tooltip"]);

        tree.handle_event(UiEvent::PointerMove(Point::new(49.0, 16.0)));
        let frame = tree.layout(Size::new(160.0, 80.0));
        let titles: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. }
                    if matches!(text.as_str(), "First tooltip" | "Second tooltip") =>
                {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(titles, vec!["Second tooltip"]);
    }

    #[test]
    fn tooltip_flips_from_top_and_clamps_to_viewport() {
        let viewport = Rect::new(0.0, 0.0, 180.0, 100.0);
        let anchor = Rect::new(154.0, 4.0, 24.0, 24.0);
        let (body, actual) = place_tooltip(
            anchor,
            Size::new(140.0, 32.0),
            viewport,
            TooltipPlacement::Above,
        );
        assert_eq!(actual, TooltipPlacement::Below);
        assert!(body.x >= 8.0);
        assert!(body.x + body.width <= viewport.width - 8.0);
        assert!(body.y >= anchor.y + anchor.height);
        assert!(body.y + body.height <= viewport.height - 8.0);
    }

    #[test]
    fn anchored_popover_flips_and_keeps_its_own_surface_interactive() {
        let trigger = Element::button("trigger", "Abrir").size(Some(100.0), Some(26.0));
        let popup =
            Element::panel(Element::button("option", "Opción").size(Some(100.0), Some(28.0)))
                .padding(8.0)
                .size(Some(130.0), Some(48.0))
                .background(Color::rgb(40, 40, 40));
        let root = Element::column(vec![
            Element::spacer().size(None, Some(145.0)),
            Element::popover(trigger, popup, "dismiss", PopoverPlacement::BottomStart),
        ]);
        let mut tree = UiTree::new(root);
        let frame = tree.layout(Size::new(240.0, 190.0));
        let anchor = frame
            .regions
            .iter()
            .find(|region| region.id == "trigger")
            .unwrap();
        let option = frame
            .regions
            .iter()
            .find(|region| region.id == "option")
            .unwrap();
        assert!(
            option.rect.y < anchor.rect.y,
            "must flip above viewport edge"
        );
        assert_eq!(frame.hit_test(Point::new(228.0, 176.0)), Some("dismiss"));
        assert_eq!(
            frame.hit_test(Point::new(anchor.rect.x + 10.0, anchor.rect.y + 10.0)),
            Some("dismiss"),
            "an open popover's backdrop closes before the anchor can retrigger"
        );
        let popup_surface = frame
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:dismiss")
            .unwrap();
        assert_eq!(
            frame.hit_test(Point::new(
                popup_surface.rect.x + 3.0,
                popup_surface.rect.y + 3.0
            )),
            Some("popover_surface:dismiss")
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
    fn clickable_session_row_keeps_nested_menu_action_and_escape_focus() {
        let row = |open| {
            let menu = Element::icon_button("more", Icon::More, "").size(Some(24.0), Some(28.0));
            let menu = if open {
                Element::popover(
                    menu,
                    Element::column(vec![
                        Element::button("pin", "Fijar").size(Some(90.0), Some(28.0)),
                        Element::button("archive", "Archivar").size(Some(90.0), Some(28.0)),
                    ])
                    .size(Some(90.0), Some(56.0)),
                    "dismiss",
                    PopoverPlacement::BottomEnd,
                )
            } else {
                menu
            };
            Element::clickable(
                "session",
                Element::row(vec![
                    Element::truncated_text("Tarea").size(Some(130.0), Some(28.0)),
                    menu,
                ])
                .size(Some(160.0), Some(28.0)),
            )
            .size(Some(160.0), Some(28.0))
        };
        let mut tree = UiTree::new(row(false));
        let frame = tree.layout(Size::new(240.0, 180.0));
        assert_eq!(frame.hit_test(Point::new(5.0, 14.0)), Some("session"));
        assert_eq!(frame.hit_test(Point::new(142.0, 14.0)), Some("more"));
        for (point, expected) in [
            (Point::new(5.0, 14.0), "session"),
            (Point::new(142.0, 14.0), "more"),
        ] {
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked {
                    id: expected.into()
                }]
            );
        }
        assert!(tree.focus("more"));
        tree.set_root(row(true));
        tree.layout(Size::new(240.0, 180.0));
        let key = |key| UiEvent::Key {
            key,
            shift: false,
            ctrl: false,
        };
        tree.handle_event(key(Key::ArrowDown));
        assert_eq!(tree.focused_id(), Some("pin"));
        tree.set_root(row(false));
        tree.layout(Size::new(240.0, 180.0));
        assert_eq!(tree.focused_id(), Some("more"));
        tree.set_root(row(true));
        tree.layout(Size::new(240.0, 180.0));
        tree.handle_event(key(Key::End));
        assert_eq!(tree.focused_id(), Some("archive"));
        assert_eq!(
            tree.handle_event(key(Key::Escape)),
            vec![UiAction::Clicked {
                id: "dismiss".into()
            }]
        );
        assert_eq!(tree.focused_id(), Some("more"));
        tree.set_root(row(false));
        tree.layout(Size::new(240.0, 180.0));
        assert_eq!(tree.focused_id(), Some("more"));
    }

    #[test]
    fn anchored_popover_escapes_scroll_clip() {
        let trigger = Element::button("trigger", "Abrir").size(Some(80.0), Some(26.0));
        let popup = Element::button("option", "Elegir").size(Some(95.0), Some(30.0));
        let scrolled = Element::scroll(
            "list",
            Element::column(vec![
                Element::spacer().size(None, Some(77.0)),
                Element::popover(trigger, popup, "dismiss", PopoverPlacement::BottomStart),
            ]),
        )
        .size(Some(160.0), Some(105.0));
        let mut tree = UiTree::new(Element::column(vec![scrolled]));
        let frame = tree.layout(Size::new(220.0, 200.0));
        let option = frame
            .regions
            .iter()
            .find(|region| region.id == "option")
            .unwrap();
        assert!(option.rect.y > 105.0, "floating option escapes scroll clip");
        assert_eq!(
            frame.hit_test(Point::new(option.rect.x + 10.0, option.rect.y + 10.0)),
            Some("option")
        );
    }

    #[test]
    fn button_labels_truncate_at_painted_width_without_splitting_graphemes() {
        let mut tree = UiTree::new(Element::column(vec![
            Element::button("plain", "WWii").size(Some(30.0), Some(28.0)),
            Element::icon_button("with_icon", Icon::Folder, "👩‍💻WWii").size(Some(50.0), Some(28.0)),
        ]));
        tree.set_text_measurer(|text, _, _| {
            text.graphemes(true)
                .map(|part| match part {
                    "W" => 10.0,
                    "i" => 2.0,
                    "…" => 4.0,
                    "👩‍💻" => 14.0,
                    _ => 5.0,
                })
                .sum()
        });
        let frame = tree.layout(Size::new(70.0, 70.0));
        let labels: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec!["W…", "👩‍💻…"]);
    }

    #[test]
    fn formatted_spans_use_painter_width_across_font_families() {
        let spans = vec![
            TextSpan::new("ii ", Color::WHITE, 14.0, FontFamily::Sans),
            TextSpan::new("W", Color::rgb(250, 100, 100), 14.0, FontFamily::Monospace),
            TextSpan::new("i", Color::WHITE, 14.0, FontFamily::Sans),
        ];
        let mut tree = UiTree::new(Element::formatted_text(spans.clone()).size(Some(9.0), None));
        tree.set_text_measurer(|part, _, family| {
            part.chars()
                .map(|character| match (character, family) {
                    ('W', FontFamily::Monospace) => 5.0,
                    ('i', _) | (' ', _) => 1.0,
                    _ => 2.0,
                })
                .sum()
        });
        let frame = tree.layout(Size::new(9.0, 100.0));
        let runs: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text {
                    position,
                    text,
                    font_family,
                    color,
                    ..
                } => Some((position.x, position.y, text.as_str(), *font_family, *color)),
                _ => None,
            })
            .collect();
        assert_eq!(runs.iter().map(|run| run.2).collect::<String>(), "ii Wi");
        assert!(runs.iter().all(|run| run.1 == 0.0));
        assert!(runs.iter().any(|run| run.0 == 3.0
            && run.2 == "W"
            && run.3 == FontFamily::Monospace
            && run.4 == Color::rgb(250, 100, 100)));
        assert!(runs.iter().any(|run| run.0 == 8.0 && run.2 == "i"));

        tree.set_root(Element::formatted_text(spans).size(Some(6.0), None));
        let frame = tree.layout(Size::new(6.0, 100.0));
        let w_y = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { position, text, .. } if text == "W" => Some(position.y),
                _ => None,
            })
            .unwrap();
        assert!(
            w_y > 0.0,
            "narrow layout should wrap the measured wide glyph"
        );
    }

    #[test]
    fn formatted_wrap_keeps_graphemes_and_explicit_line_breaks() {
        let source = "a👩‍💻e\u{301}b\n🦀";
        let mut tree = UiTree::new(
            Element::formatted_text(vec![TextSpan::new(
                source,
                Color::WHITE,
                14.0,
                FontFamily::Sans,
            )])
            .size(Some(5.0), None),
        );
        tree.set_text_measurer(|part, _, _| {
            part.graphemes(true)
                .map(|part| if part == "👩‍💻" { 5.0 } else { 1.0 })
                .sum()
        });
        let frame = tree.layout(Size::new(5.0, 100.0));
        let runs: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { position, text, .. } => Some((position.y, text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            runs.iter().map(|(_, text)| *text).collect::<String>(),
            source.replace('\n', "")
        );
        assert!(runs.iter().any(|(_, text)| *text == "👩‍💻"));
        assert!(runs.iter().any(|(_, text)| text.contains("e\u{301}")));
        assert!(runs.last().unwrap().0 > runs.first().unwrap().0);
    }

    #[test]
    fn static_selection_crosses_styled_spans_without_splitting_emoji() {
        let spans = vec![
            TextSpan::new("hi ", Color::WHITE, 14.0, FontFamily::Sans),
            TextSpan::new("👩‍💻", Color::rgb(180, 180, 180), 14.0, FontFamily::Sans),
            TextSpan::new(" fin", Color::WHITE, 14.0, FontFamily::Sans),
        ];
        let mut tree = UiTree::new(Element::formatted_text(spans).size(Some(200.0), None));
        tree.set_text_measurer(|text, _, _| text.graphemes(true).count() as f32 * 10.0);
        tree.layout(Size::new(200.0, 40.0));
        tree.handle_event(UiEvent::PointerDown(Point::new(1.0, 5.0)));
        tree.handle_event(UiEvent::PointerMove(Point::new(43.0, 5.0)));
        tree.handle_event(UiEvent::PointerUp(Point::new(43.0, 5.0)));
        assert_eq!(tree.selected_text(), Some("hi 👩‍💻"));
        let frame = tree.layout(Size::new(200.0, 40.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, .. }
                if *color == Color::rgba(180, 180, 180, 84)
                    && (rect.x - 30.0).abs() < 0.01
                    && (rect.width - 10.0).abs() < 0.01
        )));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::C,
                shift: false,
                ctrl: true,
            }),
            vec![UiAction::CopyRequested {
                text: "hi 👩‍💻".into(),
            }]
        );
    }

    #[test]
    fn pointer_leave_finishes_static_selection_drag() {
        let mut tree = UiTree::new(Element::text("abcd").size(Some(100.0), None));
        tree.layout(Size::new(100.0, 30.0));
        tree.handle_event(UiEvent::PointerDown(Point::new(1.0, 5.0)));
        tree.handle_event(UiEvent::PointerMove(Point::new(18.0, 5.0)));
        let before_leave = tree.selected_text().map(str::to_owned);
        assert!(before_leave.is_some());
        tree.handle_event(UiEvent::PointerLeave);
        assert!(!tree.static_selection.as_ref().unwrap().dragging);
        tree.handle_event(UiEvent::PointerMove(Point::new(80.0, 5.0)));
        assert_eq!(tree.selected_text(), before_leave.as_deref());
    }

    #[test]
    fn static_selection_preserves_source_newline_and_soft_wraps() {
        let mut tree = UiTree::new(Element::formatted_text(vec![
            TextSpan::new("α\n", Color::WHITE, 14.0, FontFamily::Sans),
            TextSpan::new("β", Color::WHITE, 14.0, FontFamily::Sans),
        ]));
        tree.set_text_measurer(|text, _, _| text.graphemes(true).count() as f32 * 10.0);
        tree.layout(Size::new(100.0, 50.0));
        tree.handle_event(UiEvent::PointerDown(Point::new(1.0, 5.0)));
        tree.handle_event(UiEvent::PointerUp(Point::new(10.0, 23.0)));
        assert_eq!(tree.selected_text(), Some("α\nβ"));

        tree.set_root(Element::text("abcd").size(Some(20.0), None));
        tree.layout(Size::new(20.0, 50.0));
        assert_eq!(tree.selected_text(), None);
        tree.handle_event(UiEvent::PointerDown(Point::new(1.0, 5.0)));
        tree.handle_event(UiEvent::PointerUp(Point::new(20.0, 23.0)));
        assert_eq!(tree.selected_text(), Some("abcd"));
    }

    #[test]
    fn selection_does_not_cut_a_grapheme_across_span_boundaries() {
        let mut tree = UiTree::new(Element::formatted_text(vec![
            TextSpan::new("e", Color::WHITE, 14.0, FontFamily::Sans),
            TextSpan::new("\u{301}", Color::rgb(180, 180, 180), 14.0, FontFamily::Sans),
            TextSpan::new("x", Color::WHITE, 14.0, FontFamily::Sans),
        ]));
        tree.set_text_measurer(|text, _, _| text.graphemes(true).count() as f32 * 10.0);
        tree.layout(Size::new(100.0, 30.0));
        tree.handle_event(UiEvent::PointerDown(Point::new(1.0, 5.0)));
        tree.handle_event(UiEvent::PointerUp(Point::new(20.0, 5.0)));
        assert_eq!(tree.selected_text(), Some("e\u{301}"));
    }

    #[test]
    fn wrapping_prefers_word_boundaries_and_preserves_newlines() {
        let lines = wrap_lines("El agente trabaja en este proyecto.\nSiguiente", 22.0, 1.0);
        assert_eq!(
            lines,
            ["El agente trabaja en ", "este proyecto.", "Siguiente"]
        );
    }

    #[test]
    fn painter_measurement_controls_proportional_wrapping_and_font_size() {
        let root = Element::text("iiii WWWW")
            .font_family(FontFamily::Sans)
            .font_size(28.0);
        let mut tree = UiTree::new(root);
        tree.set_text_measurer(|part, size, _| {
            part.chars()
                .map(|ch| if ch == 'W' { 7.0 } else { 2.0 })
                .sum::<f32>()
                * size
                / 14.0
        });
        let frame = tree.layout(Size::new(40.0, 100.0));
        let lines = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(lines, ["iiii ", "WW", "WW"]);
    }

    #[test]
    fn input_caret_and_selection_follow_painter_advances() {
        let mut tree = UiTree::new(
            Element::text_input("text", "iW", "")
                .font_family(FontFamily::Sans)
                .padding(0.0)
                .size(Some(120.0), Some(30.0)),
        );
        tree.set_text_measurer(|part, _, _| {
            part.chars()
                .map(|ch| if ch == 'W' { 20.0 } else { 2.0 })
                .sum()
        });
        tree.layout(Size::new(120.0, 30.0));
        assert!(tree.focus("text"));
        let focused = tree.layout(Size::new(120.0, 30.0));
        assert!(focused.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, .. } if (rect.x - 22.0).abs() < 0.01 && rect.width == 1.5
        )));
        tree.handle_event(UiEvent::PointerDown(Point::new(4.0, 4.0)));
        let clicked = tree.layout(Size::new(120.0, 30.0));
        assert!(clicked.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, .. } if (rect.x - 2.0).abs() < 0.01 && rect.width == 1.5
        )));
        tree.handle_event(UiEvent::Key {
            key: Key::A,
            shift: false,
            ctrl: true,
        });
        let selected = tree.layout(Size::new(120.0, 30.0));
        assert!(selected.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, .. }
                if *color == Color::rgba(75, 130, 210, 130)
                    && (rect.width - 22.0).abs() < 0.01
        )));
    }

    #[test]
    fn text_area_uses_measured_wraps_for_paint_and_vertical_navigation() {
        let mut tree = UiTree::new(
            Element::text_area("area", "iWWW", "")
                .font_family(FontFamily::Sans)
                .padding(0.0)
                .size(Some(20.0), Some(60.0)),
        );
        tree.set_text_measurer(|part, _, _| {
            part.chars()
                .map(|ch| if ch == 'W' { 10.0 } else { 2.0 })
                .sum()
        });
        let frame = tree.layout(Size::new(20.0, 60.0));
        let lines = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(lines, ["iW", "WW"]);
        assert!(tree.focus("area"));
        tree.handle_event(UiEvent::Key {
            key: Key::ArrowLeft,
            shift: false,
            ctrl: false,
        });
        tree.handle_event(UiEvent::Key {
            key: Key::ArrowUp,
            shift: false,
            ctrl: false,
        });
        let moved = tree.layout(Size::new(20.0, 60.0));
        assert!(moved.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, .. }
                if (rect.x - 2.0).abs() < 0.01 && rect.y == 0.0 && rect.width == 1.5
        )));
    }

    #[test]
    fn flex_layout_distributes_remaining_height() {
        let root = Element::column(vec![
            Element::text("header").size(None, Some(20.0)),
            Element::panel(Element::text("body")).flex(1.0),
            Element::text("footer").size(None, Some(10.0)),
        ]);
        let frame = UiTree::new(root).layout(Size::new(300.0, 100.0));
        let body = frame.commands.iter().find_map(|command| match command {
            DrawCommand::Text { position, text, .. } if text == "body" => Some(position.y),
            _ => None,
        });
        let footer = frame.commands.iter().find_map(|command| match command {
            DrawCommand::Text { position, text, .. } if text == "footer" => Some(position.y),
            _ => None,
        });
        assert_eq!(body, Some(20.0));
        assert_eq!(footer, Some(90.0));
    }

    #[test]
    fn typography_and_border_affect_scene_without_changing_hit_bounds() {
        let accent = Color::rgb(34, 145, 210);
        let root = Element::column(vec![
            Element::text("Title")
                .font_family(FontFamily::Sans)
                .font_size(28.0),
            Element::button("open", "Open")
                .background(Color::rgb(20, 20, 20))
                .border(accent, 1.0)
                .radius(8.0)
                .size(None, Some(32.0)),
        ]);
        let mut tree = UiTree::new(root);
        tree.set_sans_metrics(FontMetrics {
            char_width: 9.0,
            line_height: 20.0,
        });
        let frame = tree.layout(Size::new(200.0, 100.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text {
                text,
                position,
                font_size: 28.0,
                font_family: FontFamily::Sans,
                ..
            } if text == "Title" && position.y == 0.0
        )));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::StrokeRect {
                color,
                width: 1.0,
                radius: 8.0,
                ..
            } if *color == accent
        )));
        // The title's calibrated line height is 40 at 28px.
        assert_eq!(frame.hit_test(Point::new(5.0, 41.0)), Some("open"));
        assert_eq!(frame.hit_test(Point::new(5.0, 39.0)), None);
    }

    #[test]
    fn shadow_precedes_surface_and_does_not_expand_hit_bounds() {
        let ink = Color::rgba(0, 0, 0, 90);
        let surface = Color::rgb(35, 35, 35);
        let frame = UiTree::new(Element::column(vec![Element::button("card", "Card")
            .background(surface)
            .shadow(ink, Point::new(0.0, 5.0), 16.0, 2.0)
            .radius(12.0)
            .size(Some(120.0), Some(42.0))]))
        .layout(Size::new(200.0, 80.0));
        assert!(matches!(
            frame.commands.first(),
            Some(DrawCommand::Shadow {
                rect: Rect { width: 120.0, height: 42.0, .. },
                shadow: BoxShadow { color, blur: 16.0, spread: 2.0, .. },
                radius: 12.0,
            }) if *color == ink
        ));
        assert!(matches!(
            frame.commands.get(1),
            Some(DrawCommand::FillRect { color, .. }) if *color == surface
        ));
        assert_eq!(frame.hit_test(Point::new(5.0, 40.0)), Some("card"));
        assert_eq!(frame.hit_test(Point::new(5.0, 45.0)), None);
    }

    #[test]
    fn invalid_shadow_measurements_are_sanitized() {
        let element = Element::panel(Element::spacer()).shadow(
            Color::rgba(0, 0, 0, 80),
            Point::new(f32::NAN, f32::INFINITY),
            -5.0,
            f32::INFINITY,
        );
        assert_eq!(
            element.style.shadow,
            Some(BoxShadow::new(
                Color::rgba(0, 0, 0, 80),
                Point::new(0.0, 0.0),
                0.0,
                0.0,
            ))
        );
    }

    #[test]
    fn overlay_paints_after_base_and_owns_topmost_hit() {
        let frame = UiTree::new(Element::overlay(
            Element::button("base", "base").size(None, Some(40.0)),
            Element::button("floating", "floating").size(None, Some(40.0)),
        ))
        .layout(Size::new(120.0, 40.0));
        let labels = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(labels, ["base", "floating"]);
        assert_eq!(frame.hit_test(Point::new(8.0, 8.0)), Some("floating"));
    }

    #[test]
    fn overlay_escapes_scroll_clip_but_stays_inside_viewport() {
        let scroll_contents = Element::column(vec![
            Element::spacer().size(None, Some(35.0)),
            Element::overlay(
                Element::button("anchor", "anchor").size(None, Some(50.0)),
                Element::button("popup", "popup")
                    .background(Color::rgb(40, 40, 40))
                    .size(None, Some(50.0)),
            ),
        ]);
        let root = Element::column(vec![
            Element::scroll("list", scroll_contents).size(None, Some(40.0)),
            Element::spacer().flex(1.0),
        ]);
        let frame = UiTree::new(root).layout(Size::new(120.0, 100.0));
        let pop_clip = frame
            .commands
            .iter()
            .position(|command| matches!(command, DrawCommand::PopClip))
            .unwrap();
        let popup = frame
            .commands
            .iter()
            .position(
                |command| matches!(command, DrawCommand::Text { text, .. } if text == "popup"),
            )
            .unwrap();
        assert!(popup > pop_clip);
        assert_eq!(frame.hit_test(Point::new(8.0, 60.0)), Some("popup"));
        assert_eq!(frame.hit_test(Point::new(8.0, 98.0)), None);
    }

    #[test]
    fn icon_button_uses_one_theme_tint_and_keeps_button_interaction() {
        let tint = Color::rgb(219, 234, 254);
        let mut tree = UiTree::new(
            Element::icon_button("new", Icon::Add, "Nuevo")
                .icon_size(20.0)
                .foreground(tint)
                .size(Some(100.0), Some(36.0)),
        );
        let frame = tree.layout(Size::new(100.0, 36.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon {
                rect: Rect {
                    width: 16.0,
                    height: 16.0,
                    ..
                },
                ..
            }
        )));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon {
                icon: Icon::Add,
                tint: actual,
                ..
            } if *actual == tint
        )));
        assert_eq!(frame.hit_test(Point::new(12.0, 12.0)), Some("new"));
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked { id: "new".into() }]
        );
    }

    #[test]
    fn trailing_icon_shares_one_click_target_and_leaves_room_for_label() {
        let mut tree = UiTree::new(
            Element::icon_button("backend", Icon::Chat, "Codex agent")
                .trailing_icon(Icon::ArrowDown)
                .size(Some(120.0), Some(32.0)),
        );
        tree.set_text_measurer(|text, _, _| text.chars().count() as f32 * 8.0);
        let frame = tree.layout(Size::new(120.0, 32.0));
        let icons: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Icon { icon, rect, .. } => Some((*icon, *rect)),
                _ => None,
            })
            .collect();
        assert_eq!(icons.len(), 2);
        assert_eq!(icons[0].0, Icon::Chat);
        assert_eq!(icons[1].0, Icon::ArrowDown);
        assert!(icons[0].1.x + icons[0].1.width < icons[1].1.x);
        assert_eq!(frame.hit_test(Point::new(110.0, 16.0)), Some("backend"));
        let label = frame.commands.iter().find_map(|command| match command {
            DrawCommand::Text { text, .. } => Some(text.as_str()),
            _ => None,
        });
        assert!(label.unwrap().ends_with('…'));
    }

    #[test]
    fn icon_only_button_fits_canonical_icon_in_compact_target() {
        let mut tree = UiTree::new(
            Element::icon_button("close", Icon::Close, "").size(Some(24.0), Some(24.0)),
        );
        let frame = tree.layout(Size::new(24.0, 24.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon {
                rect: Rect {
                    width: 16.0,
                    height: 16.0,
                    ..
                },
                ..
            }
        )));
    }

    #[test]
    fn icon_only_glyph_and_hover_stay_centered_in_square_hit_target() {
        let hover = Color::rgb(41, 44, 47);
        let pressed = Color::rgb(57, 60, 63);
        for size in [24.0, 26.0, 28.0, 32.0, 36.0] {
            let control = Element::icon_button("toolbar", Icon::Settings, "")
                .padding(4.0)
                .background(Color::TRANSPARENT)
                .hover_background(hover)
                .pressed_background(pressed)
                .transition_ms(0)
                .size(Some(size), Some(size));
            let mut tree = UiTree::new(control);
            let viewport = Size::new(size, size);
            let base = tree.layout(viewport);
            let target = base
                .regions
                .iter()
                .find(|region| region.id == "toolbar")
                .expect("icon-only hit region")
                .rect;
            assert_eq!(target, Rect::new(0.0, 0.0, size, size));
            let glyph = base
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Icon {
                        icon: Icon::Settings,
                        rect,
                        ..
                    } => Some(*rect),
                    _ => None,
                })
                .expect("icon glyph");
            assert_eq!(
                glyph,
                Rect::new((size - 16.0) / 2.0, (size - 16.0) / 2.0, 16.0, 16.0)
            );
            tree.handle_event(UiEvent::PointerMove(Point::new(size / 2.0, size / 2.0)));
            let hovered = tree.layout(viewport);
            assert_eq!(hovered.regions, base.regions);
            assert!(hovered.commands.iter().any(|command| matches!(
                command,
                DrawCommand::FillRect { rect, color, .. } if *rect == target && *color == hover
            )));
            tree.handle_event(UiEvent::PointerDown(Point::new(size / 2.0, size / 2.0)));
            let pressed_frame = tree.layout(viewport);
            assert_eq!(pressed_frame.regions, base.regions);
            assert!(pressed_frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::FillRect { rect, color, .. } if *rect == target && *color == pressed
            )));
        }
    }

    #[test]
    fn text_area_edits_lines_and_submits_only_on_ctrl_enter() {
        let area = || Element::text_area("prompt", "stale", "Ask").size(None, Some(88.0));
        let mut tree = UiTree::new(area());
        tree.layout(Size::new(240.0, 88.0));
        tree.handle_event(UiEvent::PointerDown(Point::new(10.0, 10.0)));
        tree.set_input_value("prompt", "");
        tree.handle_event(UiEvent::Text("uno".into()));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::InputChanged {
                id: "prompt".into(),
                value: "uno\n".into(),
            }]
        );
        tree.handle_event(UiEvent::Text("dos".into()));
        tree.set_root(area());
        tree.layout(Size::new(240.0, 88.0));
        assert_eq!(tree.input_value("prompt"), Some("uno\ndos"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: true,
            }),
            vec![UiAction::Submitted {
                id: "prompt".into(),
                value: "uno\ndos".into(),
            }]
        );
        assert_eq!(tree.input_value("prompt"), Some("uno\ndos"));
    }

    #[test]
    fn button_uses_hover_and_pressed_fill_without_changing_hit_region() {
        let base = Color::rgb(20, 20, 20);
        let hover = Color::rgb(40, 40, 40);
        let pressed = Color::rgb(60, 60, 60);
        let button = Element::icon_button("action", Icon::Add, "Action")
            .background(base)
            .hover_background(hover)
            .pressed_background(pressed)
            .transition_ms(0)
            .size(Some(120.0), Some(32.0));
        let mut tree = UiTree::new(button);
        tree.layout(Size::new(120.0, 32.0));
        tree.handle_event(UiEvent::PointerMove(Point::new(10.0, 10.0)));
        let frame = tree.layout(Size::new(120.0, 32.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == hover
        )));
        tree.handle_event(UiEvent::PointerDown(Point::new(10.0, 10.0)));
        let frame = tree.layout(Size::new(120.0, 32.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == pressed
        )));
        assert_eq!(frame.hit_test(Point::new(10.0, 10.0)), Some("action"));
    }

    #[test]
    fn pointer_focus_does_not_draw_keyboard_ring() {
        let mut tree =
            UiTree::new(Element::button("action", "Action").size(Some(100.0), Some(28.0)));
        tree.layout(Size::new(100.0, 28.0));
        tree.handle_event(UiEvent::PointerDown(Point::new(5.0, 5.0)));
        assert_eq!(tree.focused_id(), Some("action"));
        assert!(!tree
            .layout(Size::new(100.0, 28.0))
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::StrokeRect { .. })));

        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        assert!(tree
            .layout(Size::new(100.0, 28.0))
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::StrokeRect { .. })));
        // Pointer motion changes hover, but it is not a focus change.
        tree.handle_event(UiEvent::PointerMove(Point::new(101.0, 5.0)));
        assert!(tree
            .layout(Size::new(100.0, 28.0))
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::StrokeRect { .. })));
    }

    #[test]
    fn stationary_pointer_retargets_hover_after_tree_rebuild() {
        let left_hover = Color::rgb(90, 10, 10);
        let right_hover = Color::rgb(10, 90, 10);
        let row = |first: bool| {
            let left = Element::button("left", "Left")
                .hover_background(left_hover)
                .transition_ms(0)
                .size(Some(50.0), Some(28.0));
            let right = Element::button("right", "Right")
                .hover_background(right_hover)
                .transition_ms(0)
                .size(Some(50.0), Some(28.0));
            Element::row(if first {
                vec![left, right]
            } else {
                vec![right, left]
            })
        };
        let mut tree = UiTree::new(row(true));
        tree.layout(Size::new(100.0, 28.0));
        tree.handle_event(UiEvent::PointerMove(Point::new(10.0, 10.0)));
        assert_eq!(tree.hovered.as_deref(), Some("left"));

        tree.set_root(row(false));
        let frame = tree.layout(Size::new(100.0, 28.0));
        assert_eq!(tree.hovered.as_deref(), Some("right"));
        assert!(
            frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::FillRect { color, rect, .. }
                    if *color == right_hover && rect.x == 0.0
            )),
            "commands={:?}; regions={:?}; hovered={:?}",
            frame.commands,
            frame.regions,
            tree.hovered
        );
        assert!(!frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == left_hover
        )));
    }

    #[test]
    fn pointer_leave_clears_hover_and_nested_icon_has_priority() {
        let row_hover = Color::rgb(50, 50, 50);
        let icon_hover = Color::rgb(90, 90, 90);
        let row = Element::clickable(
            "row",
            Element::row(vec![
                Element::spacer().size(Some(72.0), Some(28.0)),
                Element::icon_button("more", Icon::More, "")
                    .hover_background(icon_hover)
                    .transition_ms(0)
                    .size(Some(28.0), Some(28.0)),
            ]),
        )
        .hover_background(row_hover)
        .transition_ms(0)
        .size(Some(100.0), Some(28.0));
        let mut tree = UiTree::new(row);
        tree.layout(Size::new(100.0, 28.0));
        tree.handle_event(UiEvent::PointerMove(Point::new(86.0, 10.0)));
        let frame = tree.layout(Size::new(100.0, 28.0));
        assert_eq!(tree.hovered.as_deref(), Some("more"));
        assert!(
            frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::FillRect { color, .. } if *color == icon_hover
            )),
            "commands={:?}; regions={:?}; hovered={:?}",
            frame.commands,
            frame.regions,
            tree.hovered
        );
        assert!(!frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == row_hover
        )));

        tree.handle_event(UiEvent::PointerLeave);
        let frame = tree.layout(Size::new(100.0, 28.0));
        assert_eq!(tree.hovered, None);
        assert!(!frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == icon_hover
        )));
    }

    #[test]
    fn nested_action_can_share_parent_hover_surface_without_sharing_click_target() {
        let row_hover = Color::rgb(50, 50, 50);
        let row = Element::clickable(
            "row",
            Element::row(vec![
                Element::spacer().size(Some(72.0), Some(28.0)),
                Element::icon_button("more", Icon::More, "")
                    .background(Color::TRANSPARENT)
                    .hover_background(Color::TRANSPARENT)
                    .transition_ms(0)
                    .size(Some(28.0), Some(28.0)),
            ]),
        )
        .hover_with("more")
        .hover_background(row_hover)
        .transition_ms(0)
        .size(Some(100.0), Some(28.0));
        let mut tree = UiTree::new(row);
        tree.layout(Size::new(100.0, 28.0));
        tree.handle_event(UiEvent::PointerMove(Point::new(86.0, 10.0)));
        let frame = tree.layout(Size::new(100.0, 28.0));
        assert_eq!(frame.hit_test(Point::new(86.0, 10.0)), Some("more"));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, rect, .. }
                if *color == row_hover && rect.width == 100.0
        )));
        tree.handle_event(UiEvent::PointerMove(Point::new(20.0, 10.0)));
        let frame = tree.layout(Size::new(100.0, 28.0));
        assert_eq!(frame.hit_test(Point::new(20.0, 10.0)), Some("row"));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, rect, .. }
                if *color == row_hover && rect.width == 100.0
        )));
    }

    #[test]
    fn hover_fill_animates_without_moving_the_button() {
        let start = Instant::now();
        let base = Color::rgb(20, 20, 20);
        let hover = Color::rgb(80, 80, 80);
        let button = Element::button("action", "Action")
            .background(base)
            .hover_background(hover)
            .size(Some(120.0), Some(32.0));
        let mut tree = UiTree::new(button);
        tree.layout_at(Size::new(120.0, 32.0), start);
        tree.handle_event(UiEvent::PointerMove(Point::new(10.0, 10.0)));

        let first = tree.layout_at(Size::new(120.0, 32.0), start);
        assert!(first.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == base
        )));
        let middle = tree.layout_at(Size::new(120.0, 32.0), start + Duration::from_millis(60));
        assert!(middle.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if color.r > base.r && color.r < hover.r
        )));
        let end = tree.layout_at(Size::new(120.0, 32.0), start + Duration::from_millis(120));
        assert!(end.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == hover
        )));
        assert_eq!(first.regions, end.regions);

        tree.set_reduced_motion(true);
        tree.handle_event(UiEvent::PointerMove(Point::new(121.0, 10.0)));
        let reduced = tree.layout_at(Size::new(120.0, 32.0), start);
        assert!(reduced.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == base
        )));
    }

    #[test]
    fn focus_by_id_accepts_text_and_preserves_focus_on_unknown_id() {
        let mut tree = UiTree::new(Element::text_input("query", "foo", "Search"));
        tree.layout(Size::new(240.0, 38.0));
        assert!(tree.focus("query"));
        assert!(!tree.focus("missing"));
        assert_eq!(tree.focused_id(), Some("query"));
        assert_eq!(
            tree.handle_event(UiEvent::Text("bar".into())),
            vec![UiAction::InputChanged {
                id: "query".into(),
                value: "foobar".into(),
            }]
        );
    }

    #[test]
    fn text_area_selection_replaces_line_breaks_and_moves_vertically() {
        let mut tree = UiTree::new(Element::text_area("prompt", "one\ntwo", ""));
        tree.layout(Size::new(240.0, 88.0));
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        tree.handle_event(UiEvent::Key {
            key: Key::A,
            shift: false,
            ctrl: true,
        });
        let selected = tree.layout(Size::new(240.0, 88.0));
        assert_eq!(
            selected
                .commands
                .iter()
                .filter(|command| matches!(command, DrawCommand::FillRect { color, .. } if *color == Color::rgba(75, 130, 210, 130)))
                .count(),
            2
        );
        tree.handle_event(UiEvent::Text("x\ny".into()));
        tree.handle_event(UiEvent::Key {
            key: Key::ArrowUp,
            shift: true,
            ctrl: false,
        });
        tree.handle_event(UiEvent::Text("!".into()));
        assert_eq!(tree.input_value("prompt"), Some("x!"));
    }

    #[test]
    fn text_area_wheel_scrolls_wrapped_lines_inside_its_clip() {
        let mut tree =
            UiTree::new(Element::text_area("prompt", "abcdefghi", "").size(Some(40.0), Some(52.0)));
        let bottom = tree.layout(Size::new(40.0, 52.0));
        assert!(bottom
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::Text { text, .. } if text == "ghi")));
        tree.handle_event(UiEvent::Scroll {
            position: Point::new(10.0, 10.0),
            delta_y: 18.0,
        });
        let top = tree.layout(Size::new(40.0, 52.0));
        assert!(top
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::Text { text, .. } if text == "abc")));
        assert!(!top
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::Text { text, .. } if text == "ghi")));
    }

    #[test]
    fn hit_testing_respects_scroll_clipping() {
        let child = Element::column(vec![
            Element::button("one", "one").size(None, Some(30.0)),
            Element::button("two", "two").size(None, Some(30.0)),
        ]);
        let mut tree = UiTree::new(Element::scroll("list", child));
        let frame = tree.layout(Size::new(100.0, 30.0));
        assert_eq!(frame.hit_test(Point::new(5.0, 5.0)), Some("one"));
        assert_eq!(frame.hit_test(Point::new(5.0, 31.0)), None);
        tree.handle_event(UiEvent::Scroll {
            position: Point::new(5.0, 5.0),
            delta_y: -30.0,
        });
        let frame = tree.layout(Size::new(100.0, 30.0));
        assert_eq!(frame.hit_test(Point::new(5.0, 5.0)), Some("two"));
    }

    fn scroll_rows(count: usize) -> Element {
        Element::scroll(
            "list",
            Element::column(
                (0..count)
                    .map(|index| {
                        Element::button(format!("row:{index}"), "Row").size(None, Some(20.0))
                    })
                    .collect(),
            ),
        )
    }

    #[test]
    fn scrollbar_tracks_overflow_and_preserves_child_hit_regions() {
        let mut tree = UiTree::new(scroll_rows(10));
        let frame = tree.layout(Size::new(100.0, 100.0));
        let bar = frame.scrollbars["list"];
        assert_eq!(bar.track, Rect::new(92.0, 0.0, 8.0, 100.0));
        assert_eq!(bar.thumb.height, 50.0);
        assert_eq!(bar.max_offset, 100.0);
        assert_eq!(
            frame
                .regions
                .iter()
                .find(|region| region.id == "row:0")
                .unwrap()
                .rect
                .width,
            100.0,
            "an overlay must not shrink the content when it overflows"
        );
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, .. } if *rect == bar.thumb
        )));
        assert!(
            frame.commands.iter().all(|command| !matches!(command,
                DrawCommand::FillRect { rect, .. } if *rect == bar.track
            )),
            "the idle track must be transparent"
        );
        assert_eq!(frame.hit_test(Point::new(82.0, 10.0)), Some("row:0"));
        assert_eq!(frame.hit_test(Point::new(94.0, 10.0)), Some("list"));
        assert_eq!(
            frame.hit_region(Point::new(94.0, 10.0)).map(|hit| hit.kind),
            Some(HitKind::Scrollbar)
        );
        tree.handle_event(UiEvent::PointerDown(Point::new(82.0, 10.0)));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(Point::new(82.0, 10.0))),
            vec![UiAction::Clicked { id: "row:0".into() }]
        );

        tree.set_root(scroll_rows(2));
        let frame = tree.layout(Size::new(100.0, 100.0));
        assert!(!frame.scrollbars.contains_key("list"));
        assert_eq!(frame.hit_test(Point::new(94.0, 10.0)), Some("row:0"));
        assert_eq!(
            frame
                .regions
                .iter()
                .find(|region| region.id == "row:0")
                .unwrap()
                .rect
                .width,
            100.0,
            "content width must stay stable when overflow disappears"
        );
    }

    #[test]
    fn overlay_rail_leaves_a_trailing_action_clickable() {
        let rows = Element::column(
            (0..10)
                .map(|index| {
                    Element::row(vec![
                        Element::spacer().flex(1.0),
                        Element::button(format!("action:{index}"), "···")
                            .padding(0.0)
                            .size(Some(20.0), Some(20.0)),
                    ])
                    .size(None, Some(20.0))
                })
                .collect(),
        );
        let mut tree = UiTree::new(Element::scroll("list", rows));
        let frame = tree.layout(Size::new(100.0, 100.0));
        assert_eq!(frame.hit_test(Point::new(90.0, 10.0)), Some("action:0"));
        assert_eq!(
            frame.hit_test(Point::new(96.0, 70.0)),
            Some("action:3"),
            "the transparent track must not take clicks from a trailing action"
        );
        assert_eq!(
            frame.hit_test(Point::new(96.0, 10.0)),
            Some("list"),
            "the visible thumb remains draggable"
        );
        tree.handle_event(UiEvent::PointerDown(Point::new(90.0, 10.0)));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(Point::new(90.0, 10.0))),
            vec![UiAction::Clicked {
                id: "action:0".into()
            }]
        );
        tree.handle_event(UiEvent::PointerDown(Point::new(96.0, 70.0)));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(Point::new(96.0, 70.0))),
            vec![UiAction::Clicked {
                id: "action:3".into()
            }]
        );
        tree.handle_event(UiEvent::PointerDown(Point::new(96.0, 25.0)));
        tree.handle_event(UiEvent::PointerMove(Point::new(96.0, 50.0)));
        assert_eq!(tree.scroll_offset("list"), 50.0);
    }

    #[test]
    fn focusing_nested_offscreen_action_reveals_it_without_offscreen_hits() {
        let rows = Element::column(
            (0..10)
                .map(|index| {
                    Element::panel(
                        Element::row(vec![
                            Element::spacer().flex(1.0),
                            Element::button(format!("action:{index}"), "Open")
                                .size(Some(40.0), Some(20.0)),
                        ])
                        .size(None, Some(20.0)),
                    )
                    .size(None, Some(20.0))
                })
                .collect(),
        );
        let mut tree = UiTree::new(Element::scroll("list", rows));
        let initial = tree.layout(Size::new(100.0, 60.0));
        assert!(!initial.regions.iter().any(|hit| hit.id == "action:6"));
        assert!(!initial.scroll_focus_targets.contains_key("action:6"));

        assert!(tree.focus("action:6"));
        let revealed = tree.layout(Size::new(100.0, 60.0));
        assert_eq!(tree.scroll_offset("list"), 80.0);
        assert!(revealed.regions.iter().any(|hit| hit.id == "action:6"));
        assert!(!revealed.regions.iter().any(|hit| hit.id == "action:0"));
        assert_eq!(revealed.hit_test(Point::new(80.0, 50.0)), Some("action:6"));
    }

    #[test]
    fn reserved_rail_remains_available_for_dense_right_edge_content() {
        let rows = Element::column(
            (0..10)
                .map(|index| Element::button(format!("row:{index}"), "Row").size(None, Some(20.0)))
                .collect(),
        );
        let mut tree = UiTree::new(
            Element::scroll("list", rows)
                .scrollbar_appearance(crate::ScrollbarAppearance::reserved()),
        );
        let frame = tree.layout(Size::new(100.0, 100.0));
        assert_eq!(frame.scrollbars["list"].track.width, 12.0);
        assert_eq!(frame.hit_test(Point::new(94.0, 10.0)), Some("list"));
        assert_eq!(frame.hit_test(Point::new(82.0, 10.0)), Some("row:0"));
    }

    #[test]
    fn reserved_scrollbar_drag_and_track_click_move_retained_scroll_offset() {
        let mut tree = UiTree::new(
            scroll_rows(10).scrollbar_appearance(crate::ScrollbarAppearance::reserved()),
        );
        tree.layout(Size::new(100.0, 100.0));
        assert!(tree
            .handle_event(UiEvent::PointerDown(Point::new(94.0, 25.0)))
            .is_empty());
        tree.handle_event(UiEvent::PointerMove(Point::new(94.0, 50.0)));
        assert_eq!(tree.scroll_offset("list"), 50.0);
        assert!(tree
            .handle_event(UiEvent::PointerUp(Point::new(94.0, 50.0)))
            .is_empty());
        let frame = tree.layout(Size::new(100.0, 100.0));
        assert_eq!(frame.scrollbars["list"].thumb.y, 25.0);

        tree.handle_event(UiEvent::PointerDown(Point::new(94.0, 90.0)));
        tree.handle_event(UiEvent::PointerUp(Point::new(94.0, 90.0)));
        assert_eq!(tree.scroll_offset("list"), 100.0);
        tree.set_root(scroll_rows(15).scrollbar_appearance(crate::ScrollbarAppearance::reserved()));
        let frame = tree.layout(Size::new(100.0, 100.0));
        assert_eq!(tree.scroll_offset("list"), 100.0);
        assert_eq!(frame.scrollbars["list"].max_offset, 200.0);
    }

    #[test]
    fn scrollbar_is_bounded_in_narrow_and_clipped_nested_viewports() {
        let mut tree = UiTree::new(scroll_rows(4));
        let frame = tree.layout(Size::new(12.0, 40.0));
        let bar = frame.scrollbars["list"];
        assert_eq!(bar.track, Rect::new(8.0, 0.0, 4.0, 40.0));
        assert!(bar.thumb.width > 0.0 && bar.thumb.height > 0.0);
        assert_eq!(frame.hit_test(Point::new(2.0, 10.0)), Some("row:0"));
        assert_eq!(frame.hit_test(Point::new(10.0, 10.0)), Some("list"));

        let inner = Element::scroll(
            "inner",
            Element::column(
                (0..10)
                    .map(|index| {
                        Element::button(format!("inner:{index}"), "I").size(None, Some(20.0))
                    })
                    .collect(),
            ),
        )
        .size(None, Some(80.0));
        let mut nested = UiTree::new(Element::scroll("outer", inner));
        let frame = nested.layout(Size::new(100.0, 40.0));
        assert!(frame.scrollbars.contains_key("inner"));
        assert_eq!(frame.hit_test(Point::new(82.0, 41.0)), None);
        assert_eq!(
            frame.hit_region(Point::new(96.0, 10.0)).map(|hit| hit.kind),
            Some(HitKind::Scrollbar)
        );
    }

    #[test]
    fn nested_wheel_bubbles_when_inner_scroll_reaches_its_end() {
        let inner = Element::scroll(
            "inner",
            Element::column(
                (0..10)
                    .map(|index| {
                        Element::button(format!("inner:{index}"), "Row").size(None, Some(20.0))
                    })
                    .collect(),
            ),
        )
        .size(None, Some(80.0));
        let outer = Element::scroll(
            "outer",
            Element::column(vec![inner, Element::spacer().size(None, Some(200.0))]),
        );
        let mut tree = UiTree::new(outer);
        tree.layout(Size::new(100.0, 40.0));
        tree.handle_event(UiEvent::Scroll {
            position: Point::new(10.0, 10.0),
            delta_y: -50.0,
        });
        assert_eq!(tree.scroll_offset("inner"), 50.0);
        assert_eq!(tree.scroll_offset("outer"), 0.0);
        tree.handle_event(UiEvent::Scroll {
            position: Point::new(10.0, 10.0),
            delta_y: -500.0,
        });
        assert_eq!(tree.scroll_offset("inner"), 120.0);
        tree.handle_event(UiEvent::Scroll {
            position: Point::new(10.0, 10.0),
            delta_y: -30.0,
        });
        assert_eq!(tree.scroll_offset("outer"), 30.0);
    }

    #[test]
    fn input_state_survives_tree_rebuild_and_edits_unicode() {
        let mut tree = UiTree::new(Element::text_input("prompt", "", "Ask"));
        tree.layout(Size::new(200.0, 40.0));
        tree.handle_event(UiEvent::PointerDown(Point::new(10.0, 10.0)));
        assert_eq!(
            tree.handle_event(UiEvent::Text("a🦀b".into())),
            vec![UiAction::InputChanged {
                id: "prompt".into(),
                value: "a🦀b".into()
            }]
        );
        tree.handle_event(UiEvent::Key {
            key: Key::ArrowLeft,
            shift: true,
            ctrl: false,
        });
        tree.handle_event(UiEvent::Key {
            key: Key::Backspace,
            shift: false,
            ctrl: false,
        });
        assert_eq!(tree.input_value("prompt"), Some("a🦀"));
        tree.set_root(Element::text_input("prompt", "stale", "Ask"));
        tree.layout(Size::new(200.0, 40.0));
        assert_eq!(tree.focused_id(), Some("prompt"));
        assert_eq!(tree.input_value("prompt"), Some("a🦀"));
    }

    #[test]
    fn click_requires_release_inside_same_button() {
        let mut tree = UiTree::new(Element::button("send", "Send"));
        tree.layout(Size::new(100.0, 40.0));
        tree.handle_event(UiEvent::PointerDown(Point::new(5.0, 5.0)));
        assert!(tree
            .handle_event(UiEvent::PointerUp(Point::new(120.0, 5.0)))
            .is_empty());
        tree.handle_event(UiEvent::PointerDown(Point::new(5.0, 5.0)));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(Point::new(5.0, 5.0))),
            vec![UiAction::Clicked { id: "send".into() }]
        );
    }

    #[test]
    fn scroll_to_end_uses_new_content_height() {
        let rows = |count: usize| {
            Element::scroll(
                "transcript",
                Element::column(
                    (0..count)
                        .map(|index| Element::text(index.to_string()).size(None, Some(20.0)))
                        .collect(),
                ),
            )
        };
        let mut tree = UiTree::new(rows(2));
        tree.layout(Size::new(100.0, 20.0));
        tree.set_root(rows(4));
        tree.scroll_to_end("transcript");
        let frame = tree.layout(Size::new(100.0, 20.0));
        assert_eq!(tree.scroll_offset("transcript"), 60.0);
        assert!(frame
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::Text { text, .. } if text == "3")));
        assert!(!frame
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::Text { text, .. } if text == "0")));
    }

    #[test]
    fn uniform_list_builds_only_visible_rows_and_reaches_last_of_one_thousand() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let built = Arc::new(AtomicUsize::new(0));
        let list = |built: Arc<AtomicUsize>| {
            Element::uniform_list("sessions", 1_000, 20.0, move |index| {
                built.fetch_add(1, Ordering::SeqCst);
                Element::button(format!("session:{index}"), format!("Session {index}"))
            })
        };
        let mut tree = UiTree::new(list(built.clone()));
        let frame = tree.layout(Size::new(160.0, 100.0));
        assert_eq!(built.load(Ordering::SeqCst), 5);
        assert_eq!(frame.scroll_extents["sessions"], 19_900.0);
        assert_eq!(
            frame
                .regions
                .iter()
                .filter(|hit| hit.id.starts_with("session:"))
                .count(),
            5
        );
        assert_eq!(frame.hit_test(Point::new(30.0, 90.0)), Some("session:4"));
        assert_eq!(frame.hit_test(Point::new(30.0, 101.0)), None);
        assert_eq!(frame.hit_test(Point::new(155.0, 90.0)), Some("session:4"));
        assert_eq!(frame.hit_test(Point::new(155.0, 10.0)), Some("sessions"));
        assert_eq!(
            frame
                .regions
                .iter()
                .find(|region| region.id == "session:4")
                .unwrap()
                .rect
                .width,
            160.0,
            "virtualized rows keep the full width with an overlay scrollbar"
        );

        tree.scroll_to_index("sessions", 999);
        let frame = tree.layout(Size::new(160.0, 100.0));
        assert_eq!(tree.scroll_offset("sessions"), 19_900.0);
        assert_eq!(built.load(Ordering::SeqCst), 10);
        assert_eq!(frame.hit_test(Point::new(30.0, 90.0)), Some("session:999"));
        assert!(!frame.regions.iter().any(|hit| hit.id == "session:0"));

        tree.set_root(list(built.clone()));
        tree.layout(Size::new(160.0, 100.0));
        assert_eq!(tree.scroll_offset("sessions"), 19_900.0);
        assert_eq!(built.load(Ordering::SeqCst), 15);
    }

    #[test]
    fn uniform_list_tab_focus_continues_across_viewport_boundary() {
        let list = Element::uniform_list("sessions", 10, 20.0, |index| {
            Element::clickable(
                format!("row:{index}"),
                Element::row(vec![
                    Element::truncated_text(format!("Task {index}")).size(Some(58.0), Some(20.0)),
                    Element::button(format!("menu:{index}"), "⋯").size(Some(20.0), Some(20.0)),
                ]),
            )
        });
        let mut tree = UiTree::new(list);
        tree.layout(Size::new(120.0, 60.0));
        assert!(tree.focus("menu:2"));
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("row:3"));
        let frame = tree.layout(Size::new(120.0, 60.0));
        assert_eq!(tree.scroll_offset("sessions"), 20.0);
        assert!(frame.regions.iter().any(|hit| hit.id == "row:3"));

        assert!(tree.focus("row:1"));
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: true,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("menu:0"));
        let frame = tree.layout(Size::new(120.0, 60.0));
        assert_eq!(tree.scroll_offset("sessions"), 0.0);
        assert!(frame.regions.iter().any(|hit| hit.id == "menu:0"));
    }

    #[test]
    fn empty_uniform_list_has_no_rows_or_scrollbar() {
        let mut tree = UiTree::new(Element::uniform_list("empty", 0, 20.0, |_| {
            panic!("empty list must never build a row")
        }));
        tree.scroll_to_index("empty", 42);
        let frame = tree.layout(Size::new(100.0, 60.0));
        assert!(!frame.scrollbars.contains_key("empty"));
        assert_eq!(frame.scroll_extents["empty"], 0.0);
        assert!(tree.focus_order.is_empty());
    }

    #[test]
    fn selected_text_can_be_replaced() {
        let mut tree = UiTree::new(Element::text_input("input", "hello", ""));
        tree.layout(Size::new(200.0, 40.0));
        tree.handle_event(UiEvent::PointerDown(Point::new(48.0, 10.0)));
        tree.handle_event(UiEvent::Key {
            key: Key::A,
            shift: false,
            ctrl: true,
        });
        tree.handle_event(UiEvent::Text("rust".into()));
        assert_eq!(tree.input_value("input"), Some("rust"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Submitted {
                id: "input".into(),
                value: "rust".into()
            }]
        );
    }

    #[test]
    fn tab_navigation_wraps_and_survives_tree_rebuild() {
        let controls = || {
            Element::row(vec![
                Element::button("first", "First"),
                Element::text_input("prompt", "", "Prompt"),
                Element::button("last", "Last"),
            ])
        };
        let mut tree = UiTree::new(controls());
        tree.layout(Size::new(400.0, 40.0));
        let tab = |shift| UiEvent::Key {
            key: Key::Tab,
            shift,
            ctrl: false,
        };
        tree.handle_event(tab(false));
        assert_eq!(tree.focused_id(), Some("first"));
        tree.handle_event(tab(false));
        assert_eq!(tree.focused_id(), Some("prompt"));

        tree.set_root(controls());
        tree.layout(Size::new(400.0, 40.0));
        assert_eq!(tree.focused_id(), Some("prompt"));
        tree.handle_event(tab(false));
        assert_eq!(tree.focused_id(), Some("last"));
        tree.handle_event(tab(false));
        assert_eq!(tree.focused_id(), Some("first"));
        tree.handle_event(tab(true));
        assert_eq!(tree.focused_id(), Some("last"));
    }

    #[test]
    fn enter_and_space_activate_focused_button() {
        let mut tree = UiTree::new(Element::button("send", "Send"));
        tree.layout(Size::new(100.0, 40.0));
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        for key in [Key::Enter, Key::Space] {
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key,
                    shift: false,
                    ctrl: false,
                }),
                vec![UiAction::Clicked { id: "send".into() }]
            );
        }
        let frame = tree.layout(Size::new(100.0, 40.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::StrokeRect { color, width: 2.0, .. }
                if *color == Color::rgb(170, 170, 170)
        )));
    }

    #[test]
    fn removing_focused_control_clears_focus() {
        let mut tree = UiTree::new(Element::button("old", "Old"));
        tree.layout(Size::new(100.0, 40.0));
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        tree.set_root(Element::button("new", "New"));
        tree.layout(Size::new(100.0, 40.0));
        assert_eq!(tree.focused_id(), None);
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: true,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("new"));
    }

    #[test]
    fn top_fade_paints_after_scroll_content_without_changing_interaction() {
        let content = || {
            Element::scroll(
                "scroll",
                Element::button("row", "A scrollable row").size(None, Some(36.0)),
            )
            .size(None, Some(60.0))
        };
        let plain = UiTree::new(content()).layout(Size::new(220.0, 60.0));
        let faded = UiTree::new(content().top_fade(Color::rgb(20, 20, 20), 24.0))
            .layout(Size::new(220.0, 60.0));
        assert_eq!(plain.regions, faded.regions);
        let fade_index = faded
            .commands
            .iter()
            .position(|command| matches!(command, DrawCommand::FillVerticalGradient { .. }))
            .unwrap();
        assert!(faded.commands[..fade_index].iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text == "A scrollable row"
        )));
        assert!(matches!(
            faded.commands[fade_index],
            DrawCommand::FillVerticalGradient {
                rect: Rect {
                    y: 0.0,
                    height: 24.0,
                    ..
                },
                top: Color { a: 255, .. },
                bottom: Color { a: 0, .. },
            }
        ));
    }
}
