use crate::{Color, Point, RasterImage, ScrollbarAppearance};
use std::sync::Arc;

pub const DEFAULT_FONT_SIZE: f32 = 14.0;
/// Canonical visual size of every chrome icon, in logical pixels.
/// The host rasterizes this size at the current display scale.
pub const ICON_SIZE: f32 = 16.0;

/// Preferred side of an anchored floating surface. The layout engine flips
/// vertically when the preferred side would leave the viewport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PopoverPlacement {
    BottomStart,
    BottomEnd,
    TopStart,
    TopEnd,
    RightStart,
    LeftStart,
    Center,
}

/// Preferred side of a non-interactive tooltip. The engine flips to the
/// opposite side when the preferred one would leave the viewport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TooltipPlacement {
    Above,
    Below,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TooltipStyle {
    pub background: Color,
    pub border: Color,
    pub keycap: Color,
    pub text: Color,
    pub muted: Color,
    pub shadow: Color,
}

/// Semantic icon names. A host chooses the concrete artwork for each name.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Icon {
    PanelLeft,
    PanelLeftOpen,
    PanelLeftClose,
    PanelRight,
    PanelRightOpen,
    PanelRightClose,
    Add,
    Microphone,
    Infinity,
    Paperclip,
    Target,
    Plan,
    ContextRing,
    Check,
    Settings,
    Github,
    Codex,
    OpenAi,
    OpenRouter,
    DeepSeek,
    Chat,
    Folder,
    Laptop,
    GitBranch,
    Grid,
    Hash,
    Code,
    Terminal,
    ArrowUp,
    Send,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ChevronDown,
    ChevronRight,
    Search,
    Stop,
    History,
    Close,
    CloseBold,
    CloseCircle,
    Refresh,
    Menu,
    More,
    Pin,
    Archive,
    ArchiveUp,
    Trash,
    Edit,
    Unread,
    Filter,
    Sort,
    Collapse,
    Expand,
    Clock,
}

/// Outline forms of the passive loading indicator.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SpinnerVariant {
    /// An incomplete circular stroke, similar to a rotating LoaderCircle.
    #[default]
    Circle,
    /// Eight short radial strokes, similar to a rotating Loader.
    Spokes,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FontFamily {
    #[default]
    Monospace,
    Sans,
}

/// One inline style run in a paragraph. Width is measured by the same host
/// text engine that paints it, so changing family or size does not require
/// callers to guess a character advance.
#[derive(Clone, Debug, PartialEq)]
pub struct TextSpan {
    pub text: String,
    pub color: Color,
    pub font_size: f32,
    pub font_family: FontFamily,
    pub background: Option<Color>,
}

impl TextSpan {
    pub fn new(
        text: impl Into<String>,
        color: Color,
        font_size: f32,
        font_family: FontFamily,
    ) -> Self {
        Self {
            text: text.into(),
            color,
            font_size: if font_size.is_finite() && font_size > 0.0 {
                font_size
            } else {
                DEFAULT_FONT_SIZE
            },
            font_family,
            background: None,
        }
    }

    pub fn background(mut self, color: Color) -> Self {
        self.background = Some(color);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
    pub color: Color,
    pub width: f32,
}

/// Visual depth for a surface. Values are logical pixels and never affect
/// layout or pointer hit bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxShadow {
    pub color: Color,
    pub offset: Point,
    pub blur: f32,
    pub spread: f32,
}

/// Paint-only selection marker for a reusable tab strip. The group ID remains
/// stable when children are rebuilt or reordered.
#[derive(Clone, Debug)]
pub struct TabSelection {
    pub group_id: String,
    pub selected_id: String,
    pub color: Color,
    pub radius: f32,
    pub animation_ms: u16,
}

impl BoxShadow {
    pub fn new(color: Color, offset: Point, blur: f32, spread: f32) -> Self {
        Self {
            color,
            offset: Point::new(
                if offset.x.is_finite() { offset.x } else { 0.0 },
                if offset.y.is_finite() { offset.y } else { 0.0 },
            ),
            blur: if blur.is_finite() {
                blur.clamp(0.0, 64.0)
            } else {
                0.0
            },
            spread: if spread.is_finite() {
                spread.clamp(0.0, 32.0)
            } else {
                0.0
            },
        }
    }
}

/// A transient visual description. Interaction state belongs to [`crate::UiTree`].
#[derive(Clone, Debug)]
pub struct Element {
    pub kind: ElementKind,
    pub style: Style,
    /// Another action whose pointer hover should also highlight this element.
    /// Useful when a row contains a more specific action such as its menu.
    pub hover_peer: Option<String>,
    /// Optional action dispatched by an interactive element whose focus ID
    /// must remain distinct from another control with the same action.
    pub action_alias: Option<String>,
}

/// A fixed-height, lazily built list. The row factory is called only for rows
/// intersecting the viewport (and for a row reached through keyboard focus).
/// Its stable `id` retains the scroll position across tree rebuilds.
#[derive(Clone)]
pub struct UniformList {
    pub id: String,
    pub row_count: usize,
    pub row_height: f32,
    pub build_row: Arc<dyn Fn(usize) -> Element + Send + Sync>,
    pub appearance: ScrollbarAppearance,
}

impl std::fmt::Debug for UniformList {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UniformList")
            .field("id", &self.id)
            .field("row_count", &self.row_count)
            .field("row_height", &self.row_height)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub enum ElementKind {
    Column(Vec<Element>),
    Row(Vec<Element>),
    /// A row of related, enabled tab actions. Arrow keys move between these
    /// actions and activate the newly focused tab.
    TabStrip {
        children: Vec<Element>,
        action_ids: Vec<String>,
        selection: Option<TabSelection>,
    },
    Panel(Box<Element>),
    /// One interactive surface whose children can contain more specific
    /// controls. Child hit regions take precedence over this row's region.
    Clickable {
        id: String,
        child: Box<Element>,
    },
    /// Paints a floating subtree above the ordinary scene. It shares the
    /// base element's bounds, but may escape clips created by ancestors.
    Overlay {
        base: Box<Element>,
        floating: Box<Element>,
    },
    /// A floating surface tied to its anchor's actual laid-out rectangle.
    /// A transparent dismissal region is inserted behind the surface.
    Popover {
        anchor: Box<Element>,
        floating: Box<Element>,
        dismiss_id: String,
        placement: PopoverPlacement,
        gap: f32,
        animation_ms: u16,
        close_animation_ms: Option<u16>,
        open_scale: f32,
        view_key: Option<String>,
        hover_open: bool,
        backdrop: bool,
        backdrop_dismiss: bool,
        scrim: Option<Color>,
        initial_focus: Option<String>,
        return_focus: Option<String>,
    },
    /// A transient, non-interactive overlay whose anchor remains in normal
    /// layout. The `anchor_id` identifies the wrapped focusable action.
    Tooltip {
        anchor: Box<Element>,
        anchor_id: String,
        title: String,
        shortcut: Option<String>,
        placement: TooltipPlacement,
        colors: TooltipStyle,
    },
    Scroll {
        id: String,
        child: Box<Element>,
        appearance: ScrollbarAppearance,
    },
    UniformList(UniformList),
    Text(String),
    /// A single measured line for navigation labels. It uses an ellipsis when
    /// the available width is shorter than the painted text.
    TruncatedText(String),
    /// A paragraph containing inline styles and explicit line breaks.
    FormattedText(Vec<TextSpan>),
    /// Bounded encoded raster image. The host validates its digest and format
    /// before decoding and exposes `alt` to its accessibility backend.
    RasterImage(RasterImage),
    Icon(Icon),
    Button {
        id: String,
        label: String,
        trailing_icon: Option<Icon>,
    },
    IconButton {
        id: String,
        icon: Icon,
        label: String,
        trailing_icon: Option<Icon>,
    },
    TextInput {
        id: String,
        initial_value: String,
        placeholder: String,
        leading_icon: Option<Icon>,
        leading_icon_tint: Option<Color>,
        shortcut: Option<String>,
        shortcut_fill: Option<Color>,
    },
    TextArea {
        id: String,
        initial_value: String,
        placeholder: String,
    },
    /// Passive loading placeholder. UiTree animates its paint by stable ID.
    Skeleton {
        id: String,
        base: Color,
        highlight: Color,
    },
    /// Passive progress indicator. Its phase is retained by stable ID in UiTree.
    Spinner {
        id: String,
        color: Color,
        variant: SpinnerVariant,
    },
    /// An interactive reasoning-effort track. `fraction` is the selected
    /// position; `steps` is the current model's number of available levels.
    EffortSlider {
        id: String,
        fraction: f32,
        steps: usize,
        /// The final advertised level is Ultra. Other providers can end at
        /// High or Max without receiving the Ultra treatment.
        ultra_available: bool,
    },
    Spacer,
}

#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub min_width: f32,
    pub min_height: f32,
    pub padding: f32,
    pub gap: f32,
    pub flex: f32,
    pub background: Option<Color>,
    /// Paint-only scrim above children, opaque at the top and transparent at
    /// its bottom edge. Useful where scrolling content passes under a header.
    pub top_fade: Option<(Color, f32)>,
    pub hover_background: Option<Color>,
    pub pressed_background: Option<Color>,
    /// Ink for an interactive control while hovered or pressed. Unlike a
    /// background treatment this never changes its hit bounds or geometry.
    pub hover_foreground: Option<Color>,
    pub pressed_foreground: Option<Color>,
    /// Border painted while an editor owns focus, including pointer focus.
    pub focus_border: Option<Border>,
    /// Duration of interactive background transitions, in milliseconds.
    pub transition_ms: u16,
    pub border: Option<Border>,
    pub dashed_border: bool,
    pub shadow: Option<BoxShadow>,
    /// Clip child paint and hit regions to the content rectangle. The parent
    /// background, border and shadow still paint at their normal bounds.
    pub clip_children: bool,
    pub center_content: bool,
    pub underline_on_hover: bool,
    pub foreground: Color,
    pub radius: f32,
    pub font_size: f32,
    pub font_family: FontFamily,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            min_width: 0.0,
            min_height: 0.0,
            padding: 0.0,
            gap: 0.0,
            flex: 0.0,
            background: None,
            top_fade: None,
            hover_background: None,
            pressed_background: None,
            hover_foreground: None,
            pressed_foreground: None,
            focus_border: None,
            transition_ms: 0,
            border: None,
            dashed_border: false,
            shadow: None,
            clip_children: false,
            center_content: false,
            underline_on_hover: false,
            foreground: Color::WHITE,
            radius: 0.0,
            font_size: DEFAULT_FONT_SIZE,
            font_family: FontFamily::Monospace,
        }
    }
}

impl Element {
    fn new(kind: ElementKind) -> Self {
        Self {
            kind,
            style: Style::default(),
            hover_peer: None,
            action_alias: None,
        }
    }

    pub fn column(children: Vec<Self>) -> Self {
        Self::new(ElementKind::Column(children))
    }

    pub fn row(children: Vec<Self>) -> Self {
        Self::new(ElementKind::Row(children))
    }

    pub fn tab_strip(children: Vec<Self>, action_ids: Vec<String>) -> Self {
        Self::new(ElementKind::TabStrip {
            children,
            action_ids,
            selection: None,
        })
    }

    /// Animate one tab's selected surface to another without changing either
    /// tab's layout or hit region.
    pub fn tab_selection(
        mut self,
        group_id: impl Into<String>,
        selected_id: impl Into<String>,
        color: Color,
        radius: f32,
        animation_ms: u16,
    ) -> Self {
        if let ElementKind::TabStrip { selection, .. } = &mut self.kind {
            *selection = Some(TabSelection {
                group_id: group_id.into(),
                selected_id: selected_id.into(),
                color,
                radius: radius.max(0.0),
                animation_ms,
            });
        }
        self
    }

    pub fn panel(child: Self) -> Self {
        Self::new(ElementKind::Panel(Box::new(child)))
    }

    pub fn clickable(id: impl Into<String>, child: Self) -> Self {
        Self::new(ElementKind::Clickable {
            id: id.into(),
            child: Box::new(child),
        })
        .transition_ms(120)
    }

    /// Keep one visual hover surface across a parent action and one nested
    /// control while their click targets stay independent.
    pub fn hover_with(mut self, action_id: impl Into<String>) -> Self {
        self.hover_peer = Some(action_id.into());
        self
    }

    /// Keep a unique focus target while dispatching an existing action ID.
    /// For example, a dialog's close icon and Cancel button can both cancel
    /// without collapsing into one keyboard stop.
    pub fn dispatch_as(mut self, action_id: impl Into<String>) -> Self {
        self.action_alias = Some(action_id.into());
        self
    }

    /// Layer a popover or modal over a base tree. The base determines layout;
    /// floating is painted after the ordinary scene and receives pointer hits
    /// first. The window viewport remains its outer clip.
    pub fn overlay(base: Self, floating: Self) -> Self {
        Self::new(ElementKind::Overlay {
            base: Box::new(base),
            floating: Box::new(floating),
        })
    }

    /// Float content above surrounding rows and scroll clips. The anchor
    /// determines normal layout; `floating` is placed against it in the
    /// viewport and receives pointer hits before the underlying tree.
    pub fn popover(
        anchor: Self,
        floating: Self,
        dismiss_id: impl Into<String>,
        placement: PopoverPlacement,
    ) -> Self {
        Self::new(ElementKind::Popover {
            anchor: Box::new(anchor),
            floating: Box::new(floating),
            dismiss_id: dismiss_id.into(),
            placement,
            gap: 5.0,
            animation_ms: 140,
            close_animation_ms: None,
            open_scale: 1.0,
            view_key: None,
            hover_open: false,
            backdrop: true,
            backdrop_dismiss: true,
            scrim: None,
            initial_focus: None,
            return_focus: None,
        })
    }

    /// Prefer a control when a modal first opens. The target must be a
    /// focusable descendant of the floating subtree.
    pub fn popover_initial_focus(mut self, id: impl Into<String>) -> Self {
        if let ElementKind::Popover { initial_focus, .. } = &mut self.kind {
            *initial_focus = Some(id.into());
        }
        self
    }

    /// Restore focus to a stable control when a modal closes, including when
    /// the action that opened it belonged to a menu that has since closed.
    pub fn popover_return_focus(mut self, id: impl Into<String>) -> Self {
        if let ElementKind::Popover { return_focus, .. } = &mut self.kind {
            *return_focus = Some(id.into());
        }
        self
    }

    /// Show a submenu while its trigger or content is hovered or focused.
    pub fn popover_on_hover(mut self) -> Self {
        if let ElementKind::Popover {
            hover_open,
            backdrop,
            gap,
            ..
        } = &mut self.kind
        {
            *hover_open = true;
            *backdrop = false;
            *gap = 0.0;
        }
        self
    }

    /// Dim the viewport behind an open modal surface.
    pub fn popover_scrim(mut self, color: Color) -> Self {
        if let ElementKind::Popover { scrim, .. } = &mut self.kind {
            *scrim = Some(color);
        }
        self
    }

    /// Keep a modal open when its backdrop is clicked while still blocking
    /// interaction with the content underneath.
    pub fn popover_backdrop_dismiss(mut self, enabled: bool) -> Self {
        if let ElementKind::Popover {
            backdrop_dismiss, ..
        } = &mut self.kind
        {
            *backdrop_dismiss = enabled;
        }
        self
    }

    /// Set the entrance animation for a floating surface. Visual movement does
    /// not change the surface's stable hit bounds or keyboard focus order.
    pub fn popover_animation_ms(mut self, duration: u16) -> Self {
        if let ElementKind::Popover { animation_ms, .. } = &mut self.kind {
            *animation_ms = duration;
        }
        self
    }

    /// Set the exit duration independently of entrance. By default exits use
    /// the established short fade (at most 110 ms).
    pub fn popover_close_animation_ms(mut self, duration: u16) -> Self {
        if let ElementKind::Popover {
            close_animation_ms, ..
        } = &mut self.kind
        {
            *close_animation_ms = Some(duration);
        }
        self
    }

    /// Start entrance at this visual scale while keeping final hit geometry.
    pub fn popover_open_scale(mut self, scale: f32) -> Self {
        if let ElementKind::Popover { open_scale, .. } = &mut self.kind {
            if scale.is_finite() {
                *open_scale = scale.clamp(0.5, 1.25);
            }
        }
        self
    }

    /// Animate content when a retained popover switches between keyed views.
    pub fn popover_view_key(mut self, key: impl Into<String>) -> Self {
        if let ElementKind::Popover { view_key, .. } = &mut self.kind {
            *view_key = Some(key.into());
        }
        self
    }

    pub(crate) fn tooltip(
        anchor: Self,
        anchor_id: impl Into<String>,
        title: impl Into<String>,
        shortcut: Option<String>,
        placement: TooltipPlacement,
        colors: TooltipStyle,
    ) -> Self {
        Self::new(ElementKind::Tooltip {
            anchor: Box::new(anchor),
            anchor_id: anchor_id.into(),
            title: title.into(),
            shortcut,
            placement,
            colors,
        })
    }

    pub fn scroll(id: impl Into<String>, child: Self) -> Self {
        Self::new(ElementKind::Scroll {
            id: id.into(),
            child: Box::new(child),
            appearance: ScrollbarAppearance::default(),
        })
    }

    /// A scrollable list for one fixed-height sequence, including section
    /// headers if desired. Build each row from its index instead of eagerly
    /// constructing every row. Set `.flex(1.0)` when it fills the remaining
    /// height of a column.
    pub fn uniform_list(
        id: impl Into<String>,
        row_count: usize,
        row_height: f32,
        build_row: impl Fn(usize) -> Self + Send + Sync + 'static,
    ) -> Self {
        Self::new(ElementKind::UniformList(UniformList {
            id: id.into(),
            row_count,
            row_height: if row_height.is_finite() {
                row_height.max(1.0)
            } else {
                1.0
            },
            build_row: Arc::new(build_row),
            appearance: ScrollbarAppearance::default(),
        }))
    }

    /// Use the same scrollbar policy for ordinary scroll content and virtual
    /// lists. The default overlay leaves content width unchanged on overflow.
    pub fn scrollbar_appearance(mut self, appearance: ScrollbarAppearance) -> Self {
        match &mut self.kind {
            ElementKind::Scroll {
                appearance: current,
                ..
            } => *current = appearance,
            ElementKind::UniformList(list) => list.appearance = appearance,
            _ => {}
        }
        self
    }

    pub fn text(value: impl Into<String>) -> Self {
        Self::new(ElementKind::Text(value.into()))
    }

    pub fn truncated_text(value: impl Into<String>) -> Self {
        Self::new(ElementKind::TruncatedText(value.into()))
    }

    pub fn formatted_text(spans: Vec<TextSpan>) -> Self {
        Self::new(ElementKind::FormattedText(spans))
    }

    pub fn raster_image(image: RasterImage) -> Self {
        Self::new(ElementKind::RasterImage(image))
    }

    pub fn icon(icon: Icon) -> Self {
        Self::new(ElementKind::Icon(icon))
    }

    pub fn button(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(ElementKind::Button {
            id: id.into(),
            label: label.into(),
            trailing_icon: None,
        })
        .padding(8.0)
        .transition_ms(120)
    }

    /// Pass an empty label for an icon-only button.
    pub fn icon_button(id: impl Into<String>, icon: Icon, label: impl Into<String>) -> Self {
        Self::new(ElementKind::IconButton {
            id: id.into(),
            icon,
            label: label.into(),
            trailing_icon: None,
        })
        .padding(4.0)
        .gap(6.0)
        .transition_ms(120)
    }

    /// Place a second canonical-size icon at the far edge of an icon button.
    /// The entire control keeps one action ID and one hit region.
    pub fn trailing_icon(mut self, icon: Icon) -> Self {
        match &mut self.kind {
            ElementKind::Button { trailing_icon, .. }
            | ElementKind::IconButton { trailing_icon, .. } => {
                *trailing_icon = Some(icon);
            }
            _ => {}
        }
        self
    }

    pub fn text_input(
        id: impl Into<String>,
        initial_value: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> Self {
        Self::new(ElementKind::TextInput {
            id: id.into(),
            initial_value: initial_value.into(),
            placeholder: placeholder.into(),
            leading_icon: None,
            leading_icon_tint: None,
            shortcut: None,
            shortcut_fill: None,
        })
        .padding(8.0)
    }

    /// Place a canonical-size icon inside an input's own hit and focus box.
    /// Text, caret and selection reserve its width; the icon is decorative.
    pub fn leading_icon(mut self, icon: Icon) -> Self {
        if let ElementKind::TextInput { leading_icon, .. } = &mut self.kind {
            *leading_icon = Some(icon);
        }
        self
    }

    pub fn leading_icon_tint(mut self, color: Color) -> Self {
        if let ElementKind::TextInput {
            leading_icon_tint, ..
        } = &mut self.kind
        {
            *leading_icon_tint = Some(color);
        }
        self
    }

    /// A decorative keyboard hint inside the input's focus and hit bounds.
    pub fn input_shortcut(mut self, value: impl Into<String>) -> Self {
        if let ElementKind::TextInput { shortcut, .. } = &mut self.kind {
            *shortcut = Some(value.into());
        }
        self
    }

    /// Background of the passive keyboard hint inside a text input.
    pub fn input_shortcut_fill(mut self, color: Color) -> Self {
        if let ElementKind::TextInput { shortcut_fill, .. } = &mut self.kind {
            *shortcut_fill = Some(color);
        }
        self
    }

    /// A multi-line editor. Enter inserts a line break; Ctrl+Enter submits.
    pub fn text_area(
        id: impl Into<String>,
        initial_value: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> Self {
        Self::new(ElementKind::TextArea {
            id: id.into(),
            initial_value: initial_value.into(),
            placeholder: placeholder.into(),
        })
        .padding(8.0)
    }

    pub fn spacer() -> Self {
        Self::new(ElementKind::Spacer)
    }

    /// A non-interactive loading surface with paint-only motion.
    pub fn skeleton(id: impl Into<String>, base: Color, highlight: Color) -> Self {
        Self::new(ElementKind::Skeleton {
            id: id.into(),
            base,
            highlight,
        })
    }

    pub fn spinner(id: impl Into<String>, color: Color, variant: SpinnerVariant) -> Self {
        Self::new(ElementKind::Spinner {
            id: id.into(),
            color,
            variant,
        })
    }

    /// Build a reasoning-effort slider with model-specific stops.
    /// Dragging previews a continuous fraction; release snaps to a stop.
    pub fn effort_slider(id: impl Into<String>, fraction: f32, steps: usize) -> Self {
        Self::new(ElementKind::EffortSlider {
            id: id.into(),
            fraction: if fraction.is_finite() {
                fraction.clamp(0.0, 1.0)
            } else {
                0.0
            },
            steps: steps.max(2),
            ultra_available: false,
        })
    }

    /// Enable the violet finish and celebration only for a model whose last
    /// advertised reasoning level is Ultra.
    pub fn effort_ultra_available(mut self, available: bool) -> Self {
        if let ElementKind::EffortSlider {
            ultra_available, ..
        } = &mut self.kind
        {
            *ultra_available = available;
        }
        self
    }

    pub fn padding(mut self, value: f32) -> Self {
        self.style.padding = value.max(0.0);
        self
    }

    pub fn gap(mut self, value: f32) -> Self {
        self.style.gap = value.max(0.0);
        self
    }

    pub fn background(mut self, color: Color) -> Self {
        self.style.background = Some(color);
        self
    }

    /// Fade content at this element's top edge without changing its layout,
    /// clipping, scroll range, or pointer regions.
    pub fn top_fade(mut self, color: Color, height: f32) -> Self {
        self.style.top_fade = (height.is_finite() && height > 0.0).then_some((color, height));
        self
    }

    /// Background shown when the pointer is over an interactive element.
    pub fn hover_background(mut self, color: Color) -> Self {
        self.style.hover_background = Some(color);
        self
    }

    /// Background shown while an interactive element is pressed.
    pub fn pressed_background(mut self, color: Color) -> Self {
        self.style.pressed_background = Some(color);
        self
    }

    pub fn hover_foreground(mut self, color: Color) -> Self {
        self.style.hover_foreground = Some(color);
        self
    }

    pub fn pressed_foreground(mut self, color: Color) -> Self {
        self.style.pressed_foreground = Some(color);
        self
    }

    pub fn focus_border(mut self, color: Color, width: f32) -> Self {
        self.style.focus_border =
            (width > 0.0 && width.is_finite()).then_some(Border { color, width });
        self
    }

    /// Smooths hover and press backgrounds without changing layout or hit bounds.
    /// Set to zero for an immediate response.
    pub fn transition_ms(mut self, duration: u16) -> Self {
        self.style.transition_ms = duration;
        self
    }

    pub fn foreground(mut self, color: Color) -> Self {
        self.style.foreground = color;
        self
    }

    pub fn center_content(mut self) -> Self {
        self.style.center_content = true;
        self
    }

    pub fn underline_on_hover(mut self) -> Self {
        self.style.underline_on_hover = true;
        self
    }

    /// Draw a border inside the element's bounds without changing its layout size.
    pub fn border(mut self, color: Color, width: f32) -> Self {
        self.style.border = (width > 0.0 && width.is_finite()).then_some(Border { color, width });
        self
    }

    pub fn dashed_border(mut self, color: Color, width: f32) -> Self {
        self.style.border = (width > 0.0 && width.is_finite()).then_some(Border { color, width });
        self.style.dashed_border = true;
        self
    }

    /// Add depth to a card or floating surface without changing its layout.
    pub fn shadow(mut self, color: Color, offset: Point, blur: f32, spread: f32) -> Self {
        self.style.shadow = (color.a > 0).then_some(BoxShadow::new(color, offset, blur, spread));
        self
    }

    /// Keep descendants inside this element's content rectangle. Useful for
    /// a dock whose width changes while its children retain intrinsic sizes.
    pub fn clip_children(mut self) -> Self {
        self.style.clip_children = true;
        self
    }

    /// Set text size in logical pixels. Hosts should measure with the same
    /// font family and size that they use to paint each run.
    pub fn font_size(mut self, size: f32) -> Self {
        if size.is_finite() && size > 0.0 {
            self.style.font_size = size;
        }
        self
    }

    pub fn font_family(mut self, family: FontFamily) -> Self {
        self.style.font_family = family;
        self
    }

    /// Kept for source compatibility while callers migrate to [`ICON_SIZE`].
    /// All icons use the same size; individual size requests are ignored.
    pub fn icon_size(self, _size: f32) -> Self {
        self
    }

    pub fn flex(mut self, factor: f32) -> Self {
        self.style.flex = factor.max(0.0);
        self
    }

    pub fn size(mut self, width: Option<f32>, height: Option<f32>) -> Self {
        self.style.width = width.map(|value| value.max(0.0));
        self.style.height = height.map(|value| value.max(0.0));
        self
    }

    pub fn min_size(mut self, width: f32, height: f32) -> Self {
        self.style.min_width = width.max(0.0);
        self.style.min_height = height.max(0.0);
        self
    }

    pub fn radius(mut self, radius: f32) -> Self {
        self.style.radius = radius.max(0.0);
        self
    }
}
