//! Composable, controlled controls built from the same backend-neutral tree.
//! State such as the selected tab or whether a menu is open belongs to the
//! caller; stable action IDs let [`crate::UiTree`] retain focus and hover while
//! these controls are rebuilt on every frame.

use crate::{
    Color, Element, FontFamily, Icon, Point, PopoverPlacement, TooltipPlacement, TooltipStyle,
    ICON_SIZE,
};
use crate::{DropdownMenu, DropdownMenuEntry, DropdownMenuItem, MenuMotion};

/// A component can render to an ordinary [`Element`] and be used anywhere in
/// the tree. App-specific views and generic controls follow the same contract.
pub trait Component {
    fn render(self) -> Element;
}

/// A small, viewport-aware hint anchored to an existing focusable control.
/// It appears after a short hover delay or immediately with keyboard focus,
/// without adding layout size or stealing pointer/keyboard input.
pub struct Tooltip {
    anchor: Element,
    anchor_id: String,
    title: String,
    shortcut: Option<String>,
    placement: TooltipPlacement,
    palette: ComponentPalette,
}

impl Tooltip {
    pub fn new(
        anchor: Element,
        anchor_id: impl Into<String>,
        title: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            anchor,
            anchor_id: anchor_id.into(),
            title: title.into(),
            shortcut: None,
            placement: TooltipPlacement::Above,
            palette,
        }
    }

    pub fn shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub fn placement(mut self, placement: TooltipPlacement) -> Self {
        self.placement = placement;
        self
    }
}

impl Component for Tooltip {
    fn render(self) -> Element {
        Element::tooltip(
            self.anchor,
            self.anchor_id,
            self.title,
            self.shortcut,
            self.placement,
            self.palette.tooltip_style(),
        )
    }
}

/// Shared control heights in logical pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlSize {
    Dense,
    Compact,
    Regular,
    Spacious,
}

impl ControlSize {
    pub const fn height(self) -> f32 {
        match self {
            Self::Dense => 24.0,
            Self::Compact => 28.0,
            Self::Regular => 32.0,
            Self::Spacious => 36.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ControlState {
    #[default]
    Enabled,
    Disabled,
}

/// Semantic colors shared by tabs, dropdowns, and floating surfaces.
#[derive(Clone, Copy, Debug)]
pub struct ComponentPalette {
    pub surface: Color,
    pub raised: Color,
    pub hover: Color,
    pub selected: Color,
    pub border: Color,
    pub text: Color,
    pub muted: Color,
    pub shadow: Color,
}

impl ComponentPalette {
    pub const fn neutral_dark() -> Self {
        Self {
            surface: Color::rgb(24, 24, 24),
            raised: Color::rgb(38, 38, 38),
            hover: Color::rgb(48, 48, 48),
            selected: Color::rgb(58, 58, 58),
            border: Color::rgb(63, 63, 63),
            text: Color::rgb(225, 225, 225),
            muted: Color::rgb(153, 153, 153),
            shadow: Color::rgba(0, 0, 0, 100),
        }
    }

    fn tooltip_style(self) -> TooltipStyle {
        // Buttons may use an action palette with a light fill or muted ink.
        // The shared tooltip still belongs to the surrounding theme: its
        // border and muted text are stable semantic tokens across actions.
        // Dark surfaces need a clearer edge and a darker interior than a
        // regular raised control. Light themes retain their own colors.
        let dark_theme =
            u16::from(self.border.r) + u16::from(self.border.g) + u16::from(self.border.b)
                < 3 * 128;
        if dark_theme {
            TooltipStyle {
                background: darker(self.border, 19),
                border: lighter(self.border, 18),
                keycap: lighter(self.border, 10),
                text: lighter(self.muted, 76),
                muted: self.muted,
                shadow: self.shadow,
            }
        } else {
            TooltipStyle {
                background: self.raised,
                border: self.border,
                keycap: crate::shortcut::cap_fill(self.selected),
                text: self.text,
                muted: self.muted,
                shadow: self.shadow,
            }
        }
    }
}

/// Paint treatment for an action. All variants use the same geometry for a
/// given [`ControlSize`]; the visual state only changes color and border.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ButtonVariant {
    /// Light primary action with dark text.
    #[default]
    Default,
    /// Quiet filled action on a raised surface.
    Secondary,
    /// Filled action for irreversible operations.
    Destructive,
    /// Filled action for cautionary operations.
    Warning,
    /// Filled action for a successful or affirmative operation.
    Success,
    /// A toolbar action without a persistent fill.
    Ghost,
    /// A text action with an underline on hover and keyboard focus.
    Link,
    /// A navigation row on its parent surface.
    Subtle,
    /// A control raised above its parent surface.
    Raised,
    /// An outlined control on its parent surface.
    Outline,
    /// A prominent, neutral action.
    Solid,
}

/// Sizes matching the reference button family. Existing `ControlSize` values
/// remain available for compact chrome and navigation controls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ButtonSize {
    #[default]
    Default,
    Small,
    Large,
    Icon,
    IconSmall,
    IconExtraSmall,
}

impl ButtonSize {
    pub const fn sizing(self) -> ButtonSizing {
        let (height, font_size, horizontal_padding) = match self {
            Self::Default => (36.0, 14.0, 16.0),
            Self::Small => (32.0, 13.0, 12.0),
            Self::Large => (40.0, 14.0, 24.0),
            Self::Icon => (36.0, 14.0, 10.0),
            Self::IconSmall => (32.0, 13.0, 8.0),
            Self::IconExtraSmall => (24.0, 12.0, 4.0),
        };
        ButtonSizing {
            height,
            font_size,
            icon_size: ICON_SIZE,
            horizontal_padding,
            inner_spacing: 8.0,
            radius: 8.0,
        }
    }
}

/// The visible content of one action. This keeps icon-only, text-only, and
/// combined buttons on the same rendering and interaction path.
#[derive(Clone, Debug)]
pub enum ButtonContent {
    Label(String),
    Icon(Icon),
    IconAndLabel(Icon, String),
}

impl ButtonContent {
    fn parts(self) -> (Option<Icon>, String) {
        match self {
            Self::Label(label) => (None, label),
            Self::Icon(icon) => (Some(icon), String::new()),
            Self::IconAndLabel(icon, label) => (Some(icon), label),
        }
    }
}

/// Shared button geometry in logical pixels. The current renderer uses one
/// canonical 16 px icon size, while density controls the hit box and type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonSizing {
    pub height: f32,
    pub font_size: f32,
    pub icon_size: f32,
    pub horizontal_padding: f32,
    pub inner_spacing: f32,
    pub radius: f32,
}

impl ControlSize {
    pub const fn button_sizing(self) -> ButtonSizing {
        let height = self.height();
        ButtonSizing {
            height,
            font_size: match self {
                Self::Dense | Self::Compact => 12.0,
                Self::Regular => 13.0,
                Self::Spacious => 14.0,
            },
            icon_size: ICON_SIZE,
            horizontal_padding: (height - ICON_SIZE) / 2.0,
            inner_spacing: 6.0,
            radius: match self {
                Self::Dense | Self::Compact => 6.0,
                Self::Regular => 7.0,
                Self::Spacious => 8.0,
            },
        }
    }
}

/// A semantic action with one stable ID. Selection and availability belong to
/// the caller, while hover/press/focus are retained by [`crate::UiTree`].
pub struct Button {
    action_id: String,
    content: ButtonContent,
    trailing_icon: Option<Icon>,
    variant: ButtonVariant,
    size: ControlSize,
    button_size: Option<ButtonSize>,
    width: Option<f32>,
    active: bool,
    state: ControlState,
    hover_ink: Option<Color>,
    pressed_ink: Option<Color>,
    palette: ComponentPalette,
    tooltip_title: Option<String>,
    tooltip_shortcut: Option<String>,
    tooltip_placement: TooltipPlacement,
}

impl Button {
    pub fn new(
        action_id: impl Into<String>,
        label: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self::with_content(action_id, ButtonContent::Label(label.into()), palette)
    }

    pub fn with_content(
        action_id: impl Into<String>,
        content: ButtonContent,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            action_id: action_id.into(),
            content,
            trailing_icon: None,
            variant: ButtonVariant::default(),
            size: ControlSize::Regular,
            button_size: None,
            width: None,
            active: false,
            state: ControlState::Enabled,
            hover_ink: None,
            pressed_ink: None,
            palette,
            tooltip_title: None,
            tooltip_shortcut: None,
            tooltip_placement: TooltipPlacement::Above,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        let (_, label) = self.content.parts();
        self.content = if label.is_empty() {
            ButtonContent::Icon(icon)
        } else {
            ButtonContent::IconAndLabel(icon, label)
        };
        self
    }

    pub fn tooltip(mut self, title: impl Into<String>) -> Self {
        self.tooltip_title = Some(title.into());
        self
    }

    pub fn tooltip_shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.tooltip_shortcut = Some(shortcut.into());
        self
    }

    pub fn tooltip_placement(mut self, placement: TooltipPlacement) -> Self {
        self.tooltip_placement = placement;
        self
    }

    pub fn trailing_icon(mut self, icon: Icon) -> Self {
        self.trailing_icon = Some(icon);
        self
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self.button_size = None;
        self
    }

    pub fn button_size(mut self, size: ButtonSize) -> Self {
        self.button_size = Some(size);
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = Some(width.max(0.0));
        }
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.state = ControlState::Disabled;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.state = if enabled {
            ControlState::Enabled
        } else {
            ControlState::Disabled
        };
        self
    }

    pub fn hover_ink(mut self, color: Color) -> Self {
        self.hover_ink = Some(color);
        self
    }

    pub fn pressed_ink(mut self, color: Color) -> Self {
        self.pressed_ink = Some(color);
        self
    }

    fn colors(&self) -> (Color, Color, Color, Color, bool) {
        let palette = self.palette;
        let (base, hover, pressed, ink) = match self.variant {
            ButtonVariant::Default => (
                palette.text,
                darker(palette.text, 16),
                darker(palette.text, 30),
                palette.surface,
            ),
            ButtonVariant::Secondary => (
                palette.raised,
                lighter(palette.raised, 10),
                lighter(palette.raised, 18),
                palette.text,
            ),
            ButtonVariant::Destructive => (
                Color::rgb(161, 67, 71),
                Color::rgb(178, 77, 81),
                Color::rgb(139, 54, 58),
                Color::WHITE,
            ),
            ButtonVariant::Warning => (
                Color::rgb(209, 154, 55),
                Color::rgb(225, 169, 65),
                Color::rgb(183, 132, 42),
                Color::rgb(32, 27, 19),
            ),
            ButtonVariant::Success => (
                Color::rgb(47, 140, 94),
                Color::rgb(57, 157, 105),
                Color::rgb(36, 120, 78),
                Color::WHITE,
            ),
            ButtonVariant::Ghost => (
                Color::TRANSPARENT,
                palette.hover,
                palette.selected,
                palette.text,
            ),
            ButtonVariant::Link => (
                Color::TRANSPARENT,
                Color::TRANSPARENT,
                Color::TRANSPARENT,
                palette.text,
            ),
            ButtonVariant::Subtle | ButtonVariant::Outline => (
                palette.surface,
                palette.hover,
                palette.selected,
                palette.text,
            ),
            ButtonVariant::Raised => (
                palette.raised,
                palette.hover,
                palette.selected,
                palette.text,
            ),
            ButtonVariant::Solid => (
                palette.selected,
                lighter(palette.selected, 10),
                lighter(palette.selected, 18),
                palette.text,
            ),
        };
        let (base, hover, pressed) = if self.active
            && matches!(
                self.variant,
                ButtonVariant::Subtle
                    | ButtonVariant::Raised
                    | ButtonVariant::Outline
                    | ButtonVariant::Ghost
            ) {
            (
                palette.selected,
                lighter(palette.selected, 10),
                lighter(palette.selected, 18),
            )
        } else if self.active {
            (hover, pressed, pressed)
        } else {
            (base, hover, pressed)
        };
        // A muted label on a light or saturated fill loses contrast. Disabled
        // controls use the quiet raised surface while retaining their size.
        let base = if self.state == ControlState::Disabled
            && matches!(
                self.variant,
                ButtonVariant::Default
                    | ButtonVariant::Destructive
                    | ButtonVariant::Warning
                    | ButtonVariant::Success
            ) {
            palette.raised
        } else {
            base
        };
        let ink = if self.state == ControlState::Disabled {
            palette.muted
        } else {
            ink
        };
        (
            base,
            hover,
            pressed,
            ink,
            self.variant == ButtonVariant::Outline,
        )
    }
}

impl Component for Button {
    fn render(self) -> Element {
        let (base, hover, pressed, ink, outline) = self.colors();
        let sizing = self
            .button_size
            .map(ButtonSize::sizing)
            .unwrap_or_else(|| self.size.button_sizing());
        let padding = sizing.horizontal_padding.max(4.0);
        let (icon, label) = self.content.parts();
        let icon_only = icon.is_some() && label.is_empty();
        let width = self.width.or(icon_only.then_some(sizing.height));
        let height = sizing.height;
        let radius = sizing.radius;

        if self.state == ControlState::Disabled {
            // The engine's Button element always registers a hit target. A
            // disabled action is instead ordinary painted content: it remains
            // visible, but cannot be focused or activated by any input path.
            let has_label = !label.is_empty();
            let icon_space = if icon.is_some() {
                sizing.icon_size + sizing.inner_spacing
            } else {
                0.0
            };
            let trailing_space = if self.trailing_icon.is_some() {
                sizing.icon_size + sizing.inner_spacing
            } else {
                0.0
            };
            let label_width =
                width.map(|value| (value - padding * 2.0 - icon_space - trailing_space).max(0.0));
            let label = Element::truncated_text(label)
                .foreground(ink)
                .font_family(FontFamily::Sans)
                .font_size(sizing.font_size)
                .size(label_width, Some((height - padding * 2.0).max(0.0)));
            let visual = if let Some(icon) = icon {
                let mut children = vec![Element::icon(icon).foreground(ink)];
                if has_label {
                    children.push(label);
                }
                if let Some(trailing) = self.trailing_icon {
                    children.push(Element::icon(trailing).foreground(ink));
                }
                Element::row(children).gap(sizing.inner_spacing)
            } else if let Some(trailing) = self.trailing_icon {
                Element::row(vec![label, Element::icon(trailing).foreground(ink)])
                    .gap(sizing.inner_spacing)
            } else {
                label
            };
            let mut element = Element::panel(visual)
                .padding(padding)
                .background(base)
                .radius(radius)
                .size(width, Some(height));
            if outline {
                element = element.border(self.palette.border, 1.0);
            }
            return element;
        }

        let mut element = if let Some(icon) = icon {
            Element::icon_button(self.action_id.clone(), icon, label)
        } else {
            Element::button(self.action_id.clone(), label)
        };
        if let Some(trailing) = self.trailing_icon {
            element = element.trailing_icon(trailing);
        }
        element = element
            .background(base)
            .hover_background(hover)
            .pressed_background(pressed)
            .hover_foreground(self.hover_ink.unwrap_or(ink))
            .pressed_foreground(self.pressed_ink.or(self.hover_ink).unwrap_or(ink))
            .transition_ms(120)
            .foreground(ink)
            .font_family(FontFamily::Sans)
            .font_size(sizing.font_size)
            .padding(padding)
            .gap(sizing.inner_spacing)
            .radius(radius)
            .size(width, Some(height));
        if matches!(
            self.variant,
            ButtonVariant::Default
                | ButtonVariant::Secondary
                | ButtonVariant::Destructive
                | ButtonVariant::Warning
                | ButtonVariant::Success
                | ButtonVariant::Ghost
                | ButtonVariant::Outline
                | ButtonVariant::Link
        ) {
            element = element.center_content();
        }
        if self.variant == ButtonVariant::Link {
            element = element.underline_on_hover();
        }
        if outline {
            element = element.border(self.palette.border, 1.0);
        }
        if let Some(title) = self.tooltip_title {
            let mut tooltip = Tooltip::new(element, self.action_id, title, self.palette)
                .placement(self.tooltip_placement);
            if let Some(shortcut) = self.tooltip_shortcut {
                tooltip = tooltip.shortcut(shortcut);
            }
            tooltip.render()
        } else {
            element
        }
    }
}

/// A button whose leading icon is mandatory. Without a label it uses a square
/// hit target, while the glyph remains 16 logical pixels at every density.
pub struct IconButton(Button);

impl IconButton {
    pub fn new(action_id: impl Into<String>, icon: Icon, palette: ComponentPalette) -> Self {
        Self(Button::with_content(
            action_id,
            ButtonContent::Icon(icon),
            palette,
        ))
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        let (icon, _) = self.0.content.parts();
        self.0.content = ButtonContent::IconAndLabel(icon.expect("IconButton icon"), label.into());
        self
    }

    pub fn tooltip(mut self, title: impl Into<String>) -> Self {
        self.0 = self.0.tooltip(title);
        self
    }

    pub fn tooltip_shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.0 = self.0.tooltip_shortcut(shortcut);
        self
    }

    pub fn tooltip_placement(mut self, placement: TooltipPlacement) -> Self {
        self.0 = self.0.tooltip_placement(placement);
        self
    }

    pub fn trailing_icon(mut self, icon: Icon) -> Self {
        self.0.trailing_icon = Some(icon);
        self
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.0.variant = variant;
        self
    }

    pub fn size(mut self, size: ControlSize) -> Self {
        self.0 = self.0.size(size);
        self
    }

    pub fn button_size(mut self, size: ButtonSize) -> Self {
        self.0 = self.0.button_size(size);
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.0 = self.0.width(width);
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.0.active = active;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.0.state = ControlState::Disabled;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.0 = self.0.enabled(enabled);
        self
    }

    pub fn hover_ink(mut self, color: Color) -> Self {
        self.0.hover_ink = Some(color);
        self
    }

    pub fn pressed_ink(mut self, color: Color) -> Self {
        self.0.pressed_ink = Some(color);
        self
    }
}

impl Component for IconButton {
    fn render(self) -> Element {
        self.0.render()
    }
}

/// A keyboard shortcut shown in the same compact capsule as tooltip hints.
pub struct Kbd {
    keys: String,
    palette: ComponentPalette,
}

impl Kbd {
    pub fn new(keys: impl Into<String>, palette: ComponentPalette) -> Self {
        Self {
            keys: keys.into(),
            palette,
        }
    }
}

impl Component for Kbd {
    fn render(self) -> Element {
        Element::truncated_text(crate::shortcut::label(&self.keys))
            .foreground(self.palette.muted)
            .font_family(FontFamily::Monospace)
            .font_size(crate::shortcut::FONT_SIZE)
            .padding(crate::shortcut::HORIZONTAL_PADDING)
            .background(crate::shortcut::cap_fill(self.palette.selected))
            .radius(crate::shortcut::RADIUS)
            .size(None, Some(crate::shortcut::HEIGHT))
    }
}

/// A single-line input with the same density, focus treatment and palette as
/// the shared search field. The host owns its value through `InputChanged`.
pub struct TextField {
    id: String,
    value: String,
    placeholder: String,
    palette: ComponentPalette,
    size: ControlSize,
    width: Option<f32>,
    state: ControlState,
}

impl TextField {
    pub fn new(
        id: impl Into<String>,
        value: impl Into<String>,
        placeholder: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            id: id.into(),
            value: value.into(),
            placeholder: placeholder.into(),
            palette,
            size: ControlSize::Regular,
            width: None,
            state: ControlState::Enabled,
        }
    }

    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = Some(width.max(0.0));
        }
        self
    }

    pub fn disabled(mut self) -> Self {
        self.state = ControlState::Disabled;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.state = if enabled {
            ControlState::Enabled
        } else {
            ControlState::Disabled
        };
        self
    }
}

impl Component for TextField {
    fn render(self) -> Element {
        if self.state == ControlState::Disabled {
            let text = if self.value.is_empty() {
                self.placeholder
            } else {
                self.value
            };
            return Element::panel(
                Element::truncated_text(text)
                    .foreground(self.palette.muted)
                    .font_family(FontFamily::Sans)
                    .font_size(13.0),
            )
            .padding(8.0)
            .background(self.palette.raised)
            .border(self.palette.border, 1.0)
            .radius(input_radius(self.size))
            .size(self.width, Some(self.size.height().max(28.0)));
        }
        input_surface(
            Element::text_input(self.id, self.value, self.placeholder),
            self.palette,
            self.size,
            self.width,
        )
    }
}

/// A multi-line editor using the same palette and focus treatment as
/// [`TextField`]. The host owns its text through `InputChanged`.
pub struct TextArea {
    id: String,
    value: String,
    placeholder: String,
    palette: ComponentPalette,
    width: Option<f32>,
    height: f32,
    state: ControlState,
}

impl TextArea {
    pub fn new(
        id: impl Into<String>,
        value: impl Into<String>,
        placeholder: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            id: id.into(),
            value: value.into(),
            placeholder: placeholder.into(),
            palette,
            width: None,
            height: 96.0,
            state: ControlState::Enabled,
        }
    }

    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = Some(width.max(0.0));
        }
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        if height.is_finite() {
            self.height = height.max(64.0);
        }
        self
    }

    pub fn disabled(mut self) -> Self {
        self.state = ControlState::Disabled;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.state = if enabled {
            ControlState::Enabled
        } else {
            ControlState::Disabled
        };
        self
    }
}

impl Component for TextArea {
    fn render(self) -> Element {
        if self.state == ControlState::Disabled {
            let text = if self.value.is_empty() {
                self.placeholder
            } else {
                self.value
            };
            return Element::panel(
                Element::text(text)
                    .foreground(self.palette.muted)
                    .font_family(FontFamily::Sans)
                    .font_size(13.0),
            )
            .padding(8.0)
            .background(self.palette.raised)
            .border(self.palette.border, 1.0)
            .radius(8.0)
            .size(self.width, Some(self.height));
        }
        Element::text_area(self.id, self.value, self.placeholder)
            .background(self.palette.raised)
            .hover_background(lighter(self.palette.raised, 5))
            .border(self.palette.border, 1.0)
            .focus_border(lighter(self.palette.border, 70), 1.5)
            .foreground(self.palette.text)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .padding(8.0)
            .radius(8.0)
            .min_size(80.0, self.height)
            .size(self.width, Some(self.height))
    }
}

fn input_radius(size: ControlSize) -> f32 {
    if size.height() >= 36.0 {
        10.0
    } else {
        8.0
    }
}

fn input_surface(
    input: Element,
    palette: ComponentPalette,
    size: ControlSize,
    width: Option<f32>,
) -> Element {
    let height = size.height().max(28.0);
    input
        .background(palette.raised)
        .hover_background(lighter(palette.raised, 5))
        .border(palette.border, 1.0)
        .focus_border(lighter(palette.border, 70), 1.5)
        .foreground(palette.text)
        .font_family(FontFamily::Sans)
        .font_size(13.0)
        .padding(8.0)
        .radius(input_radius(size))
        .min_size(80.0, height)
        .size(width, Some(height))
}

/// An editable search control. Its magnifier, text, caret, selection and
/// focus border all live in the same hit box, so pointer and keyboard focus
/// cannot disagree about the field's bounds. The host owns the query and
/// handles `UiAction::InputChanged` for `id`.
pub struct SearchField {
    id: String,
    value: String,
    placeholder: String,
    palette: ComponentPalette,
    size: ControlSize,
    width: Option<f32>,
    state: ControlState,
    shortcut: Option<String>,
}

/// The application's shared search input; the existing `SearchField` name
/// remains available to current callers.
pub type Search = SearchField;

impl SearchField {
    pub fn new(
        id: impl Into<String>,
        value: impl Into<String>,
        placeholder: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            id: id.into(),
            value: value.into(),
            placeholder: placeholder.into(),
            palette,
            size: ControlSize::Spacious,
            width: None,
            state: ControlState::Enabled,
            shortcut: None,
        }
    }

    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = Some(width.max(0.0));
        }
        self
    }

    pub fn shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub fn disabled(mut self) -> Self {
        self.state = ControlState::Disabled;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.state = if enabled {
            ControlState::Enabled
        } else {
            ControlState::Disabled
        };
        self
    }
}

impl Component for SearchField {
    fn render(self) -> Element {
        let height = self.size.height().max(28.0);
        let radius = input_radius(self.size);
        let surface = self.palette.raised;
        if self.state == ControlState::Disabled {
            let (value, ink) = if self.value.is_empty() {
                (self.placeholder, self.palette.muted)
            } else {
                (self.value, self.palette.muted)
            };
            let mut children = vec![
                Element::icon(Icon::Search)
                    .foreground(self.palette.muted)
                    .size(Some(ICON_SIZE), Some(ICON_SIZE)),
                Element::truncated_text(value)
                    .foreground(ink)
                    .font_family(FontFamily::Sans)
                    .font_size(13.0)
                    .flex(1.0),
            ];
            if let Some(shortcut) = self.shortcut {
                children.push(Kbd::new(shortcut, self.palette).render());
            }
            return Element::panel(Element::row(children).gap(8.0))
                .background(surface)
                .border(self.palette.border, 1.0)
                .radius(radius)
                .padding(8.0)
                .size(self.width, Some(height));
        }
        let mut input = input_surface(
            Element::text_input(self.id, self.value, self.placeholder)
                .leading_icon(Icon::Search)
                .leading_icon_tint(self.palette.muted),
            self.palette,
            self.size,
            self.width,
        );
        if let Some(shortcut) = self.shortcut {
            input = input
                .input_shortcut(shortcut)
                .input_shortcut_fill(crate::shortcut::cap_fill(self.palette.selected));
        }
        input
    }
}

/// A caller-controlled switch with a single focusable target. The thumb is
/// painted inside its track and moves only when the caller passes a new
/// `checked` value after receiving `UiAction::Clicked`.
pub struct Switch {
    action_id: String,
    checked: bool,
    state: ControlState,
    palette: ComponentPalette,
}

impl Switch {
    pub fn new(action_id: impl Into<String>, checked: bool, palette: ComponentPalette) -> Self {
        Self {
            action_id: action_id.into(),
            checked,
            state: ControlState::Enabled,
            palette,
        }
    }

    pub fn disabled(mut self) -> Self {
        self.state = ControlState::Disabled;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.state = if enabled {
            ControlState::Enabled
        } else {
            ControlState::Disabled
        };
        self
    }
}

impl Component for Switch {
    fn render(self) -> Element {
        const TRACK_WIDTH: f32 = 36.0;
        const TRACK_HEIGHT: f32 = 20.0;
        const THUMB: f32 = 14.0;
        let thumb_color = if self.checked {
            self.palette.surface
        } else {
            self.palette.text
        };
        let track_color = if self.checked {
            self.palette.text
        } else {
            self.palette.selected
        };
        let thumb = Element::spacer()
            .size(Some(THUMB), Some(THUMB))
            .background(thumb_color)
            .radius(THUMB / 2.0);
        let offset = Element::spacer().size(Some(TRACK_WIDTH - THUMB - 6.0), Some(THUMB));
        let contents = if self.checked {
            Element::row(vec![offset, thumb])
        } else {
            Element::row(vec![thumb, offset])
        };
        let enabled = self.state == ControlState::Enabled;
        let track = if enabled {
            Element::clickable(self.action_id, contents)
                .hover_background(lighter(track_color, 10))
                .pressed_background(lighter(track_color, 16))
                .transition_ms(120)
        } else {
            Element::panel(contents)
        };
        track
            .background(track_color)
            .padding(3.0)
            .radius(TRACK_HEIGHT / 2.0)
            .size(Some(TRACK_WIDTH), Some(TRACK_HEIGHT))
    }
}

pub(crate) fn lighter(color: Color, amount: u8) -> Color {
    Color::rgba(
        color.r.saturating_add(amount),
        color.g.saturating_add(amount),
        color.b.saturating_add(amount),
        color.a,
    )
}

fn darker(color: Color, amount: u8) -> Color {
    Color::rgba(
        color.r.saturating_sub(amount),
        color.g.saturating_sub(amount),
        color.b.saturating_sub(amount),
        color.a,
    )
}

/// One selectable tab. `action_id` is a stable ID handled by the host's
/// `UiAction::Clicked`; it can represent any domain value (file, view, task).
pub struct Tab {
    pub action_id: String,
    pub label: String,
    pub icon: Option<Icon>,
    pub active: bool,
    pub width: f32,
    pub state: ControlState,
}

impl Tab {
    pub fn new(action_id: impl Into<String>, label: impl Into<String>, width: f32) -> Self {
        Self {
            action_id: action_id.into(),
            label: label.into(),
            icon: None,
            active: false,
            width: width.max(40.0),
            state: ControlState::Enabled,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.state = ControlState::Disabled;
        self
    }
}

/// A compact tab strip. It owns appearance, while the host owns navigation.
const TAB_GAP: f32 = 2.0;
const TAB_TRACK_PADDING: f32 = 2.0;
const TAB_ITEM_PADDING: f32 = 4.0;
const TAB_ICON_GAP: f32 = 4.0;
const TAB_FONT_SIZE: f32 = 11.5;

pub struct Tabs {
    pub items: Vec<Tab>,
    pub palette: ComponentPalette,
    pub height: f32,
    pub gap: f32,
    pub id: Option<String>,
    pub animation_ms: u16,
}

impl Tabs {
    pub fn new(items: Vec<Tab>, palette: ComponentPalette) -> Self {
        Self {
            items,
            palette,
            height: ControlSize::Dense.height(),
            gap: TAB_GAP,
            id: None,
            animation_ms: 170,
        }
    }

    /// A stable ID for the strip's selection animation. Useful when tabs are
    /// dynamically inserted, removed, or reordered.
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn animation_ms(mut self, duration: u16) -> Self {
        self.animation_ms = duration;
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = height.max(24.0);
        self
    }

    pub fn size(mut self, size: ControlSize) -> Self {
        self.height = size.height();
        self
    }
}

impl Component for Tabs {
    fn render(self) -> Element {
        let selected_id = self
            .items
            .iter()
            .find(|item| item.active && item.state == ControlState::Enabled)
            .map(|item| item.action_id.clone());
        let enabled_ids = self
            .items
            .iter()
            .filter(|item| item.state == ControlState::Enabled)
            .map(|item| item.action_id.clone())
            .collect();
        let group_id = self.id.unwrap_or_else(|| {
            self.items
                .first()
                .map(|item| format!("tabs:{}", item.action_id))
                .unwrap_or_else(|| "tabs:empty".to_owned())
        });
        let items = self
            .items
            .into_iter()
            .map(|item| {
                let ink = if item.active {
                    self.palette.text
                } else {
                    self.palette.muted
                };
                // The shared indicator paints below the controls and moves
                // between them. Individual surfaces stay transparent so they
                // cannot hide its intermediate position.
                let fill = Color::TRANSPARENT;
                // A tab is a text treatment above the shared selection
                // indicator. Hover changes ink only; another fill would
                // obscure the sliding indicator and make the track look
                // uneven. The active tab retains its selected surface.
                let hover_ink = if item.active {
                    lighter(self.palette.text, 12)
                } else {
                    self.palette.text
                };
                if item.state == ControlState::Disabled {
                    let label = Element::text(item.label)
                        .foreground(self.palette.muted)
                        .font_family(FontFamily::Sans)
                        .font_size(TAB_FONT_SIZE);
                    let visual = if let Some(icon) = item.icon {
                        Element::row(vec![
                            Element::icon(icon).foreground(self.palette.muted),
                            label,
                        ])
                        .gap(TAB_ICON_GAP)
                    } else {
                        label
                    };
                    return Element::row(vec![
                        Element::spacer().flex(1.0),
                        visual,
                        Element::spacer().flex(1.0),
                    ])
                    .padding(TAB_ITEM_PADDING)
                    .background(Color::TRANSPARENT)
                    .radius(7.0)
                    .size(Some(item.width), Some(self.height));
                }
                let button = if let Some(icon) = item.icon {
                    Element::icon_button(item.action_id, icon, item.label)
                } else {
                    Element::button(item.action_id, item.label)
                };
                button
                    .background(fill)
                    .hover_foreground(hover_ink)
                    .pressed_foreground(hover_ink)
                    .foreground(ink)
                    .font_family(FontFamily::Sans)
                    .font_size(TAB_FONT_SIZE)
                    .padding(TAB_ITEM_PADDING)
                    .gap(TAB_ICON_GAP)
                    .radius(7.0)
                    .center_content()
                    .size(Some(item.width), Some(self.height))
            })
            .collect();
        let strip = Element::tab_strip(items, enabled_ids)
            .gap(self.gap)
            .padding(TAB_TRACK_PADDING)
            .background(self.palette.surface)
            .radius(9.0)
            .size(None, Some(self.height + TAB_TRACK_PADDING * 2.0));
        if let Some(selected_id) = selected_id {
            strip.tab_selection(
                group_id,
                selected_id,
                self.palette.selected,
                7.0,
                self.animation_ms,
            )
        } else {
            strip
        }
    }
}

/// Tabs in one rounded track. Each item remains a normal tab action, so the
/// existing keyboard navigation and focus model also applies to segmented
/// selectors. The caller controls the selected item and every visual color.
pub struct SegmentedControl {
    items: Vec<Tab>,
    palette: ComponentPalette,
    height: f32,
}

impl SegmentedControl {
    pub fn new(items: Vec<Tab>, palette: ComponentPalette) -> Self {
        Self {
            items,
            palette,
            height: 28.0,
        }
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = height.max(28.0);
        self
    }
}

impl Component for SegmentedControl {
    fn render(self) -> Element {
        let inner_width = self.items.iter().map(|item| item.width).sum::<f32>()
            + TAB_GAP * self.items.len().saturating_sub(1) as f32;
        Tabs::new(self.items, self.palette)
            .height(self.height - TAB_TRACK_PADDING * 2.0)
            .render()
            .size(
                Some(inner_width + TAB_TRACK_PADDING * 2.0),
                Some(self.height),
            )
    }
}

/// A floating slot anchored to any element. The child may itself be a
/// dropdown, command list, form, or another application-specific component.
/// While open, Tab stays among visible controls in the floating surface;
/// Escape or backdrop dismissal returns focus to the first focusable anchor.
pub struct Popover {
    pub anchor: Element,
    pub content: Element,
    pub open: bool,
    pub dismiss_id: String,
    pub placement: PopoverPlacement,
    pub width: f32,
    pub palette: ComponentPalette,
    pub animation_ms: u16,
    pub close_animation_ms: Option<u16>,
    pub open_scale: f32,
    pub view_key: Option<String>,
}

impl Popover {
    pub fn new(
        anchor: Element,
        content: Element,
        open: bool,
        dismiss_id: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            anchor,
            content,
            open,
            dismiss_id: dismiss_id.into(),
            placement: PopoverPlacement::BottomStart,
            width: 240.0,
            palette,
            animation_ms: 140,
            close_animation_ms: None,
            open_scale: 1.0,
            view_key: None,
        }
    }

    pub fn placement(mut self, placement: PopoverPlacement) -> Self {
        self.placement = placement;
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = width.max(96.0);
        }
        self
    }

    pub fn animation_ms(mut self, duration: u16) -> Self {
        self.animation_ms = duration;
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
}

impl Component for Popover {
    fn render(self) -> Element {
        if !self.open {
            return self.anchor;
        }
        let floating = Element::panel(self.content)
            .padding(6.0)
            .size(Some(self.width), None)
            .background(self.palette.raised)
            .border(self.palette.border, 1.0)
            .radius(10.0)
            .shadow(self.palette.shadow, Point::new(0.0, 9.0), 22.0, 0.0);
        let mut popover = Element::popover(self.anchor, floating, self.dismiss_id, self.placement)
            .popover_animation_ms(self.animation_ms)
            .popover_open_scale(self.open_scale);
        if let Some(duration) = self.close_animation_ms {
            popover = popover.popover_close_animation_ms(duration);
        }
        if let Some(key) = self.view_key {
            popover = popover.popover_view_key(key);
        }
        popover
    }
}

/// One option in a dropdown list; its ID is the host action to execute.
pub struct DropdownOption {
    pub action_id: String,
    pub label: String,
    pub icon: Option<Icon>,
    pub selected: bool,
    pub state: ControlState,
}

impl DropdownOption {
    pub fn new(action_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            action_id: action_id.into(),
            label: label.into(),
            icon: None,
            selected: false,
            state: ControlState::Enabled,
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

    pub fn disabled(mut self) -> Self {
        self.state = ControlState::Disabled;
        self
    }
}

/// A dropdown with caller-owned selection/open state and stable action IDs.
/// The anchor is a slot, so callers can supply an icon button, field, or any
/// other visual trigger without changing the list behavior. Arrow keys,
/// Home/End and Tab move through enabled options and reveal offscreen rows;
/// Enter activates the focused option. The host still owns the open state and
/// handles the resulting ID.
pub struct DropdownList {
    pub anchor: Element,
    pub options: Vec<DropdownOption>,
    pub open: bool,
    pub dismiss_id: String,
    pub width: f32,
    pub placement: PopoverPlacement,
    pub palette: ComponentPalette,
    pub motion: MenuMotion,
}

impl DropdownList {
    pub fn new(
        anchor: Element,
        options: Vec<DropdownOption>,
        open: bool,
        dismiss_id: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            anchor,
            options,
            open,
            dismiss_id: dismiss_id.into(),
            width: 240.0,
            placement: PopoverPlacement::BottomStart,
            palette,
            motion: MenuMotion::default(),
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
}

impl Component for DropdownList {
    fn render(self) -> Element {
        let entries = self
            .options
            .into_iter()
            .map(|option| {
                let mut item =
                    DropdownMenuItem::new(option.action_id, option.label).selected(option.selected);
                if let Some(icon) = option.icon {
                    item = item.icon(icon);
                }
                if option.state == ControlState::Disabled {
                    item = item.disabled();
                }
                DropdownMenuEntry::Item(item)
            })
            .collect();
        DropdownMenu::new(
            self.anchor,
            entries,
            self.open,
            self.dismiss_id,
            self.palette,
        )
        .width(self.width)
        .placement(self.placement)
        .motion(self.motion)
        .render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DrawCommand, ElementKind, Point, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn reference_variants_have_distinct_semantic_treatments() {
        let palette = ComponentPalette::neutral_dark();
        for variant in [
            ButtonVariant::Default,
            ButtonVariant::Secondary,
            ButtonVariant::Outline,
            ButtonVariant::Ghost,
            ButtonVariant::Destructive,
            ButtonVariant::Link,
            ButtonVariant::Warning,
            ButtonVariant::Success,
        ] {
            let element = Button::new("action", "Action", palette)
                .variant(variant)
                .render();
            assert!(element.style.center_content);
            assert_eq!(
                element.style.underline_on_hover,
                variant == ButtonVariant::Link
            );
            assert_eq!(
                element.style.border.is_some(),
                variant == ButtonVariant::Outline
            );
            assert_eq!(
                element.style.background,
                Some(match variant {
                    ButtonVariant::Default => palette.text,
                    ButtonVariant::Secondary => palette.raised,
                    ButtonVariant::Outline => palette.surface,
                    ButtonVariant::Ghost | ButtonVariant::Link => Color::TRANSPARENT,
                    ButtonVariant::Destructive => Color::rgb(161, 67, 71),
                    ButtonVariant::Warning => Color::rgb(209, 154, 55),
                    ButtonVariant::Success => Color::rgb(47, 140, 94),
                    _ => unreachable!(),
                })
            );
        }
    }

    #[test]
    fn reference_sizes_define_button_and_icon_hit_bounds() {
        let palette = ComponentPalette::neutral_dark();
        for (size, expected) in [
            (ButtonSize::Default, 36.0),
            (ButtonSize::Small, 32.0),
            (ButtonSize::Large, 40.0),
            (ButtonSize::Icon, 36.0),
            (ButtonSize::IconSmall, 32.0),
            (ButtonSize::IconExtraSmall, 24.0),
        ] {
            let element = IconButton::new("action", Icon::Add, palette)
                .button_size(size)
                .render();
            assert_eq!(element.style.height, Some(expected));
            assert_eq!(element.style.width, Some(expected));
        }
    }

    #[test]
    fn search_hint_stays_inside_the_editors_single_hit_target() {
        let palette = ComponentPalette::neutral_dark();
        let mut tree = UiTree::new(
            Search::new("query", "", "Buscar", palette)
                .shortcut("Ctrl+K")
                .width(280.0)
                .render(),
        );
        let frame = tree.layout(Size::new(280.0, 36.0));
        assert_eq!(frame.regions.len(), 1);
        assert_eq!(frame.regions[0].id, "query");
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Ctrl+K")));
        assert!(tree.focus("query"));
    }

    #[test]
    fn search_shortcut_yields_space_to_focus_and_typed_text() {
        let palette = ComponentPalette::neutral_dark();
        let mut tree = UiTree::new(Element::column(vec![
            Search::new("query", "", "Buscar", palette)
                .shortcut("Ctrl+K")
                .width(280.0)
                .render(),
            Element::button("outside", "Otro"),
        ]));
        let has_hint = |frame: &crate::Frame| {
            frame.commands.iter().any(|command| {
                matches!(
                    command, DrawCommand::Text { text, .. } if text == "Ctrl+K"
                )
            })
        };
        assert!(has_hint(&tree.layout(Size::new(280.0, 72.0))));
        assert!(tree.focus("query"));
        assert!(!has_hint(&tree.layout(Size::new(280.0, 72.0))));
        tree.handle_event(UiEvent::Text("Rust".to_owned()));
        assert!(tree.focus("outside"));
        assert!(!has_hint(&tree.layout(Size::new(280.0, 72.0))));
        tree.set_input_value("query", "");
        assert!(has_hint(&tree.layout(Size::new(280.0, 72.0))));
    }

    #[test]
    fn shared_text_editors_disable_without_leaving_input_targets() {
        let palette = ComponentPalette::neutral_dark();
        let enabled = UiTree::new(Element::column(vec![
            TextField::new("name", "", "Nombre", palette).render(),
            TextArea::new("description", "", "Descripción", palette)
                .height(72.0)
                .render(),
        ]))
        .layout(Size::new(320.0, 120.0));
        assert!(enabled.regions.iter().any(|region| region.id == "name"));
        assert!(enabled
            .regions
            .iter()
            .any(|region| region.id == "description"));

        let disabled = UiTree::new(Element::column(vec![
            TextField::new("name", "", "Nombre", palette)
                .disabled()
                .render(),
            TextArea::new("description", "", "Descripción", palette)
                .disabled()
                .render(),
        ]))
        .layout(Size::new(320.0, 140.0));
        assert!(disabled.regions.is_empty());
    }

    #[test]
    fn keyboard_shortcuts_keep_the_full_sequence_in_one_capsule() {
        let palette = ComponentPalette::neutral_dark();
        for keys in ["Ctrl++", "Ctrl+Shift+M", "Esc", "Alt+←"] {
            let kbd = Kbd::new(keys, palette).render();
            assert!(matches!(&kbd.kind, ElementKind::TruncatedText(value) if value == keys));
            let frame = UiTree::new(Element::row(vec![kbd])).layout(Size::new(180.0, 28.0));
            assert_eq!(
                frame
                    .commands
                    .iter()
                    .filter(|command| matches!(
                        command, DrawCommand::Text { text, .. } if text == keys
                    ))
                    .count(),
                1,
            );
        }
    }

    #[test]
    fn keyboard_shortcuts_share_the_tooltip_capsule_geometry() {
        let palette = ComponentPalette::neutral_dark();
        let kbd = Kbd::new("Ctrl+Shift+M", palette).render();
        assert_eq!(kbd.style.padding, crate::shortcut::HORIZONTAL_PADDING);
        assert_eq!(kbd.style.height, Some(crate::shortcut::HEIGHT));
        assert_eq!(
            kbd.style.background,
            Some(crate::shortcut::cap_fill(palette.selected))
        );
        assert_eq!(kbd.style.foreground, palette.muted);
        assert_eq!(kbd.style.radius, crate::shortcut::RADIUS);
        assert_eq!(kbd.style.font_size, crate::shortcut::FONT_SIZE);
        assert_eq!(kbd.style.font_family, FontFamily::Monospace);
        assert_eq!(kbd.style.border, None);
    }

    #[test]
    fn all_tabs_share_the_segmented_track() {
        let palette = ComponentPalette::neutral_dark();
        let tabs = Tabs::new(vec![Tab::new("files", "Archivos", 88.0)], palette).render();
        assert_eq!(tabs.style.background, Some(palette.surface));
        assert_eq!(tabs.style.radius, 9.0);
        assert_eq!(tabs.style.padding, 2.0);
        assert_eq!(tabs.style.height, Some(28.0));
        assert_eq!(tabs.style.gap, 2.0);
        let ElementKind::TabStrip { children, .. } = tabs.kind else {
            panic!("Tabs must render one strip");
        };
        assert_eq!(children[0].style.height, Some(24.0));
        assert_eq!(children[0].style.padding, 4.0);
        assert_eq!(children[0].style.font_size, 11.5);
    }

    #[test]
    fn every_tab_centers_its_label_or_icon_label_group() {
        let palette = ComponentPalette::neutral_dark();
        let tabs = Tabs::new(
            vec![
                Tab::new("all", "Todo", 120.0).active(true),
                Tab::new("actions", "Acciones", 120.0).icon(Icon::Code),
                Tab::new("disabled", "Archivos", 120.0).disabled(),
            ],
            palette,
        )
        .render();
        let mut tree = UiTree::new(tabs);
        tree.set_text_measurer(|text, _, _| text.chars().count() as f32 * 10.0);
        let frame = tree.layout(Size::new(368.0, 28.0));
        let text_x = |name: &str| {
            frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Text { text, position, .. } if text == name => Some(position.x),
                    _ => None,
                })
                .unwrap()
        };
        let icon_x = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Icon {
                    icon: Icon::Code,
                    rect,
                    ..
                } => Some(rect.x),
                _ => None,
            })
            .unwrap();
        let center = |left: f32, right: f32| (left + right) / 2.0;
        assert!((center(text_x("Todo"), text_x("Todo") + 40.0) - 62.0).abs() < 0.1);
        assert!((center(icon_x, text_x("Acciones") + 80.0) - 184.0).abs() < 0.1);
        assert!((center(text_x("Archivos"), text_x("Archivos") + 80.0) - 306.0).abs() < 0.1);
    }

    #[test]
    fn explicit_tab_height_preserves_files_and_project_switchers() {
        let palette = ComponentPalette::neutral_dark();
        let tabs = Tabs::new(
            vec![
                Tab::new("files", "Archivos", 88.0).active(true),
                Tab::new("projects", "Proyectos", 92.0),
            ],
            palette,
        )
        .height(32.0)
        .render();
        assert_eq!(tabs.style.height, Some(36.0));
        let ElementKind::TabStrip {
            children,
            selection,
            ..
        } = tabs.kind
        else {
            panic!("Tabs must render one strip");
        };
        assert_eq!(children[0].style.height, Some(32.0));
        assert_eq!(children[1].style.height, Some(32.0));
        assert_eq!(selection.unwrap().selected_id, "files");
    }

    #[test]
    fn button_content_and_tooltip_share_one_action_target() {
        let palette = ComponentPalette::neutral_dark();
        let button = Button::with_content(
            "inspect",
            ButtonContent::IconAndLabel(Icon::Code, "Inspeccionar".into()),
            palette,
        )
        .tooltip("Ver detalles")
        .tooltip_shortcut("Ctrl+I")
        .render();
        let ElementKind::Tooltip {
            anchor,
            anchor_id,
            title,
            shortcut,
            ..
        } = &button.kind
        else {
            panic!("button should own its tooltip");
        };
        assert_eq!(anchor_id, "inspect");
        assert_eq!(title, "Ver detalles");
        assert_eq!(shortcut.as_deref(), Some("Ctrl+I"));
        assert!(matches!(anchor.kind, ElementKind::IconButton { .. }));

        let mut tree = UiTree::new(button);
        let frame = tree.layout(Size::new(200.0, 40.0));
        assert_eq!(frame.regions.len(), 1);
        let center = Point::new(
            frame.regions[0].rect.x + frame.regions[0].rect.width / 2.0,
            frame.regions[0].rect.y + frame.regions[0].rect.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(center));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(center)),
            vec![UiAction::Clicked {
                id: "inspect".into()
            }]
        );
    }

    #[test]
    fn dark_tooltips_share_one_surface_across_manual_and_button_actions() {
        let palette = ComponentPalette {
            surface: Color::rgb(22, 22, 22),
            raised: Color::rgb(43, 43, 43),
            hover: Color::rgb(52, 52, 52),
            selected: Color::rgb(54, 54, 54),
            border: Color::rgb(46, 46, 46),
            text: Color::rgb(212, 212, 212),
            muted: Color::rgb(136, 136, 136),
            shadow: Color::rgba(0, 0, 0, 125),
        };
        let action_palette = ComponentPalette {
            surface: Color::WHITE,
            raised: Color::WHITE,
            selected: Color::WHITE,
            text: Color::rgb(0, 0, 0),
            ..palette
        };
        let manual = Tooltip::new(
            Element::button("manual", "Abrir"),
            "manual",
            "Abrir",
            palette,
        )
        .render();
        let button = Button::new("button", "Abrir", action_palette)
            .tooltip("Abrir")
            .render();
        let icon_button = IconButton::new("icon", Icon::Add, action_palette)
            .tooltip("Agregar")
            .render();
        let tooltip_style = |element: Element| {
            let ElementKind::Tooltip { colors, .. } = element.kind else {
                panic!("expected the shared tooltip");
            };
            colors
        };
        let expected = tooltip_style(manual);
        assert_eq!(expected, tooltip_style(button));
        assert_eq!(expected, tooltip_style(icon_button));
        assert_eq!(expected.background, Color::rgb(27, 27, 27));
        assert_eq!(expected.border, Color::rgb(64, 64, 64));
        assert_eq!(expected.keycap, Color::rgb(56, 56, 56));
        assert_eq!(expected.text, Color::rgb(212, 212, 212));
    }

    #[test]
    fn light_tooltip_keeps_palette_colors() {
        let palette = ComponentPalette {
            surface: Color::rgb(250, 250, 250),
            raised: Color::rgb(246, 246, 246),
            hover: Color::rgb(235, 235, 235),
            selected: Color::rgb(225, 225, 225),
            border: Color::rgb(205, 205, 205),
            text: Color::rgb(25, 25, 25),
            muted: Color::rgb(95, 95, 95),
            shadow: Color::rgba(0, 0, 0, 40),
        };
        let tooltip =
            Tooltip::new(Element::button("help", "Ayuda"), "help", "Ayuda", palette).render();
        let ElementKind::Tooltip { colors, .. } = tooltip.kind else {
            panic!("expected the shared tooltip");
        };
        assert_eq!(colors.background, palette.raised);
        assert_eq!(colors.border, palette.border);
        assert_eq!(colors.text, palette.text);
    }

    #[test]
    fn disabled_button_with_tooltip_has_no_action() {
        let button = Button::new("save", "Guardar", ComponentPalette::neutral_dark())
            .tooltip("Guardar cambios")
            .disabled()
            .render();
        assert!(!matches!(button.kind, ElementKind::Tooltip { .. }));
        assert!(UiTree::new(button)
            .layout(Size::new(200.0, 40.0))
            .regions
            .is_empty());
    }

    #[test]
    fn disabled_bright_variants_keep_muted_ink_readable() {
        let palette = ComponentPalette::neutral_dark();
        for variant in [
            ButtonVariant::Default,
            ButtonVariant::Destructive,
            ButtonVariant::Warning,
            ButtonVariant::Success,
        ] {
            let element = Button::new("save", "Guardar", palette)
                .variant(variant)
                .disabled()
                .render();
            assert_eq!(element.style.background, Some(palette.raised));
            let frame = UiTree::new(element).layout(Size::new(180.0, 36.0));
            assert!(frame.regions.is_empty());
            assert!(frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::Text { color, .. } if *color == palette.muted
            )));
        }
    }

    #[test]
    fn label_button_trailing_icon_preserves_width_when_disabled() {
        let palette = ComponentPalette::neutral_dark();
        let enabled = Button::new("more", "Más opciones", palette)
            .trailing_icon(Icon::ChevronDown)
            .variant(ButtonVariant::Raised)
            .render();
        let disabled = Button::new("more", "Más opciones", palette)
            .trailing_icon(Icon::ChevronDown)
            .variant(ButtonVariant::Raised)
            .disabled()
            .render();
        let enabled_frame = UiTree::new(enabled).layout(Size::new(300.0, 32.0));
        let disabled_frame = UiTree::new(disabled).layout(Size::new(300.0, 32.0));
        let enabled_width = enabled_frame.regions[0].rect.width;
        let disabled_width = disabled_frame.commands.iter().find_map(|command| {
            if let DrawCommand::FillRect { rect, color, .. } = command {
                (*color == palette.raised).then_some(rect.width)
            } else {
                None
            }
        });
        assert_eq!(disabled_width, Some(enabled_width));
        assert!(enabled_frame
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::Icon { .. })));
        assert!(disabled_frame.regions.is_empty());
    }

    #[test]
    fn semantic_button_preserves_one_action_for_both_icons() {
        let palette = ComponentPalette::neutral_dark();
        let button = Button::new("backend:codex", "Codex", palette)
            .icon(Icon::Code)
            .trailing_icon(Icon::ArrowDown)
            .variant(ButtonVariant::Raised)
            .width(180.0)
            .render();
        let mut tree = UiTree::new(button);
        let frame = tree.layout(Size::new(180.0, 32.0));
        assert_eq!(frame.regions.len(), 1);
        assert_eq!(frame.regions[0].id, "backend:codex");
        assert_eq!(frame.regions[0].rect.width, 180.0);
        assert_eq!(
            frame.hit_test(Point::new(172.0, 16.0)),
            Some("backend:codex")
        );
        tree.handle_event(UiEvent::PointerDown(Point::new(172.0, 16.0)));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(Point::new(172.0, 16.0))),
            vec![UiAction::Clicked {
                id: "backend:codex".into()
            }]
        );
    }

    #[test]
    fn disabled_button_keeps_its_box_but_has_no_action() {
        let palette = ComponentPalette::neutral_dark();
        let enabled = IconButton::new("new", Icon::Add, palette)
            .label("Nueva tarea")
            .variant(ButtonVariant::Raised)
            .width(168.0)
            .render();
        let disabled = IconButton::new("new", Icon::Add, palette)
            .label("Nueva tarea")
            .variant(ButtonVariant::Raised)
            .width(168.0)
            .disabled()
            .render();
        let enabled_frame = UiTree::new(enabled).layout(Size::new(168.0, 32.0));
        let disabled_frame = UiTree::new(disabled).layout(Size::new(168.0, 32.0));
        assert_eq!(enabled_frame.regions[0].rect.width, 168.0);
        assert_eq!(enabled_frame.regions[0].rect.height, 32.0);
        assert!(disabled_frame.regions.is_empty());
        assert!(disabled_frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, .. }
                if rect.width == 168.0 && rect.height == 32.0 && *color == palette.raised
        )));
        assert!(disabled_frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon { tint, .. } if *tint == palette.muted
        )));
    }

    #[test]
    fn disabled_intrinsic_width_matches_enabled_width() {
        let palette = ComponentPalette::neutral_dark();
        let enabled = Button::new("save", "Guardar", palette)
            .variant(ButtonVariant::Raised)
            .render();
        let disabled = Button::new("save", "Guardar", palette)
            .variant(ButtonVariant::Raised)
            .disabled()
            .render();
        let enabled_frame = UiTree::new(enabled).layout(Size::new(300.0, 32.0));
        let disabled_frame = UiTree::new(disabled).layout(Size::new(300.0, 32.0));
        let enabled_width = enabled_frame.regions[0].rect.width;
        let painted_width = disabled_frame.commands.iter().find_map(|command| {
            if let DrawCommand::FillRect { rect, color, .. } = command {
                (*color == palette.raised).then_some(rect.width)
            } else {
                None
            }
        });
        assert_eq!(painted_width, Some(enabled_width));
        assert!(disabled_frame.regions.is_empty());
    }

    #[test]
    fn disabled_button_truncates_long_label_inside_fixed_width() {
        let palette = ComponentPalette::neutral_dark();
        let button = Button::new(
            "project",
            "Proyecto con un nombre demasiado largo para el control",
            palette,
        )
        .icon(Icon::Folder)
        .width(140.0)
        .disabled()
        .render();
        let mut tree = UiTree::new(button);
        tree.set_text_measurer(|value, _, _| value.chars().count() as f32 * 7.0);
        let frame = tree.layout(Size::new(140.0, 32.0));
        let label = frame.commands.iter().find_map(|command| match command {
            DrawCommand::Text { text, position, .. } => Some((text, position.x)),
            _ => None,
        });
        let (text, x) = label.expect("button label");
        assert!(text.ends_with('…'));
        assert!(x + text.chars().count() as f32 * 7.0 <= 140.0);
    }

    #[test]
    fn icon_button_sizes_keep_the_same_glyph_and_hit_bounds_during_press() {
        let palette = ComponentPalette::neutral_dark();
        for size in [
            ControlSize::Compact,
            ControlSize::Regular,
            ControlSize::Spacious,
        ] {
            let button = IconButton::new("settings", Icon::Settings, palette)
                .variant(ButtonVariant::Subtle)
                .size(size)
                .render();
            let extent = size.height();
            let mut tree = UiTree::new(button);
            tree.set_reduced_motion(true);
            let base = tree.layout(Size::new(extent, extent));
            let hit = base.regions[0].rect;
            assert_eq!(hit.width, extent);
            assert_eq!(hit.height, extent);
            let glyph = base.commands.iter().find_map(|command| match command {
                DrawCommand::Icon { rect, .. } => Some(*rect),
                _ => None,
            });
            assert_eq!(glyph.unwrap().width, ICON_SIZE);
            tree.handle_event(UiEvent::PointerMove(Point::new(4.0, 4.0)));
            let hover = tree.layout(Size::new(extent, extent));
            assert!(hover.commands.iter().any(|command| matches!(
                command,
                DrawCommand::FillRect { color, .. } if *color == palette.hover
            )));
            tree.handle_event(UiEvent::PointerDown(Point::new(4.0, 4.0)));
            let pressed = tree.layout(Size::new(extent, extent));
            assert!(pressed.commands.iter().any(|command| matches!(
                command,
                DrawCommand::FillRect { color, .. } if *color == palette.selected
            )));
            assert_eq!(base.regions, hover.regions);
            assert_eq!(hover.regions, pressed.regions);
        }
    }

    #[test]
    fn active_state_changes_only_paint() {
        let palette = ComponentPalette::neutral_dark();
        let regular = Button::new("session", "Sesión", palette)
            .variant(ButtonVariant::Subtle)
            .width(140.0)
            .render();
        let active = Button::new("session", "Sesión", palette)
            .variant(ButtonVariant::Subtle)
            .width(140.0)
            .active(true)
            .render();
        let regular_frame = UiTree::new(regular).layout(Size::new(140.0, 32.0));
        let active_frame = UiTree::new(active).layout(Size::new(140.0, 32.0));
        assert_eq!(regular_frame.regions, active_frame.regions);
        assert!(regular_frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == palette.surface
        )));
        assert!(active_frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == palette.selected
        )));
    }

    #[test]
    fn tabs_preserve_domain_action_ids() {
        let palette = ComponentPalette::neutral_dark();
        let tabs = Tabs::new(
            vec![
                Tab::new("files", "Archivos", 100.0).icon(Icon::Folder),
                Tab::new("sessions", "Sesiones", 100.0).active(true),
            ],
            palette,
        )
        .render();
        let mut tree = UiTree::new(tabs);
        let frame = tree.layout(Size::new(240.0, 40.0));
        assert!(frame.regions.iter().any(|region| region.id == "files"));
        assert!(frame.regions.iter().any(|region| region.id == "sessions"));
    }

    #[test]
    fn segmented_control_keeps_the_tab_keyboard_contract_and_fixed_track() {
        let palette = ComponentPalette::neutral_dark();
        let segment = SegmentedControl::new(
            vec![
                Tab::new("group", "Grupo", 70.0).icon(Icon::Hash),
                Tab::new("project", "Proyecto", 78.0)
                    .icon(Icon::Folder)
                    .active(true),
            ],
            palette,
        )
        .render();
        assert_eq!(segment.style.radius, 9.0);
        assert_eq!(segment.style.width, Some(154.0));
        let mut tree = UiTree::new(segment);
        let frame = tree.layout(Size::new(154.0, 28.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, radius }
                if *color == palette.surface && rect.width == 154.0 && rect.height == 28.0 && *radius == 9.0
        )));
        assert!(tree.focus("group"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: crate::Key::ArrowRight,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked {
                id: "project".into()
            }]
        );
    }

    #[test]
    fn active_tab_hover_preserves_selected_treatment() {
        let palette = ComponentPalette::neutral_dark();
        let tabs = Tabs::new(
            vec![
                Tab::new("group", "Grupo", 80.0),
                Tab::new("project", "Proyecto", 80.0).active(true),
            ],
            palette,
        )
        .render();
        let ElementKind::TabStrip { children, .. } = tabs.kind else {
            panic!("Tabs must render a tab strip");
        };
        assert_eq!(children[0].style.hover_background, None);
        assert_eq!(children[1].style.hover_background, None);
        assert_eq!(children[0].style.hover_foreground, Some(palette.text));
        assert_eq!(
            children[1].style.hover_foreground,
            Some(lighter(palette.text, 12))
        );
    }

    #[test]
    fn search_field_keeps_icon_editor_and_focus_ring_in_one_hit_box() {
        let palette = ComponentPalette::neutral_dark();
        let mut tree = UiTree::new(
            SearchField::new("query", "", "Buscar", palette)
                .width(280.0)
                .render(),
        );
        let frame = tree.layout(Size::new(280.0, 40.0));
        let region = frame
            .regions
            .iter()
            .find(|region| region.id == "query")
            .unwrap();
        assert_eq!(region.rect.width, 280.0);
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon { icon: Icon::Search, rect, .. }
                if rect.x >= region.rect.x && rect.x + rect.width <= region.rect.x + region.rect.width
        )));
        let hit_box = region.rect;
        assert!(tree.focus("query"));
        let focused = tree.layout(Size::new(280.0, 40.0));
        assert_eq!(
            focused
                .regions
                .iter()
                .find(|region| region.id == "query")
                .unwrap()
                .rect,
            hit_box
        );
        assert!(focused.commands.iter().any(|command| matches!(
            command,
            DrawCommand::StrokeRect { rect, .. } if *rect == hit_box
        )));
    }

    #[test]
    fn switch_is_one_keyboard_and_pointer_target() {
        let palette = ComponentPalette::neutral_dark();
        let mut tree = UiTree::new(Switch::new("keep_awake", false, palette).render());
        let frame = tree.layout(Size::new(36.0, 20.0));
        assert_eq!(frame.regions.len(), 1);
        assert_eq!(frame.regions[0].id, "keep_awake");
        assert_eq!(frame.regions[0].rect.width, 36.0);
        assert_eq!(frame.regions[0].rect.height, 20.0);
        assert!(tree.focus("keep_awake"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: crate::Key::Enter,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked {
                id: "keep_awake".into()
            }]
        );
        tree.set_root(
            Switch::new("keep_awake", false, palette)
                .disabled()
                .render(),
        );
        let disabled = tree.layout(Size::new(36.0, 20.0));
        assert!(disabled.regions.is_empty());
    }

    #[test]
    fn tab_arrows_activate_visible_enabled_tabs_and_wrap_within_their_strip() {
        let palette = ComponentPalette::neutral_dark();
        let tabs = Tabs::new(
            vec![
                Tab::new("sessions", "Sesiones", 100.0),
                Tab::new("unavailable", "Pronto", 100.0).disabled(),
                Tab::new("files", "Archivos", 100.0),
            ],
            palette,
        )
        .render();
        let mut tree = UiTree::new(Element::column(vec![
            tabs,
            Element::button("outside", "Otro"),
        ]));
        tree.layout(Size::new(340.0, 100.0));
        let key = |key| UiEvent::Key {
            key,
            shift: false,
            ctrl: false,
        };
        assert!(tree.focus("sessions"));
        assert!(tree
            .handle_event(UiEvent::Key {
                key: crate::Key::ArrowRight,
                shift: true,
                ctrl: false,
            })
            .is_empty());
        assert_eq!(tree.focused_id(), Some("sessions"));
        assert_eq!(
            tree.handle_event(key(crate::Key::ArrowRight)),
            vec![UiAction::Clicked { id: "files".into() }]
        );
        assert_eq!(tree.focused_id(), Some("files"));
        assert_eq!(
            tree.handle_event(key(crate::Key::ArrowRight)),
            vec![UiAction::Clicked {
                id: "sessions".into()
            }]
        );
        assert_eq!(
            tree.handle_event(key(crate::Key::ArrowLeft)),
            vec![UiAction::Clicked { id: "files".into() }]
        );
        assert_eq!(
            tree.handle_event(key(crate::Key::Home)),
            vec![UiAction::Clicked {
                id: "sessions".into()
            }]
        );
        assert!(tree.handle_event(key(crate::Key::Home)).is_empty());
        assert_eq!(
            tree.handle_event(key(crate::Key::End)),
            vec![UiAction::Clicked { id: "files".into() }]
        );
        assert!(!tree.focus("unavailable"));
        assert!(tree.focus("outside"));
        assert!(tree.handle_event(key(crate::Key::ArrowLeft)).is_empty());
        assert_eq!(tree.focused_id(), Some("outside"));
    }

    #[test]
    fn dropdown_uses_shared_popover_and_keeps_item_ids() {
        let palette = ComponentPalette::neutral_dark();
        let dropdown = DropdownList::new(
            Element::button("trigger", "Agente").size(Some(100.0), Some(30.0)),
            vec![
                DropdownOption::new("backend:codex", "Codex").selected(true),
                DropdownOption::new("backend:claude", "Claude"),
            ],
            true,
            "dismiss",
            palette,
        )
        .render();
        let mut tree = UiTree::new(dropdown);
        let frame = tree.layout(Size::new(400.0, 300.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "backend:codex"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "backend:claude"));
        assert_eq!(frame.hit_test(Point::new(390.0, 290.0)), Some("dismiss"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: crate::Key::Escape,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked {
                id: "dismiss".into()
            }]
        );
        assert_eq!(tree.focused_id(), Some("trigger"));
    }

    #[test]
    fn selected_dropdown_option_keeps_selected_hover_color() {
        let palette = ComponentPalette::neutral_dark();
        let dropdown = DropdownList::new(
            Element::button("trigger", "Agent"),
            vec![
                DropdownOption::new("codex", "Codex").selected(true),
                DropdownOption::new("claude", "Claude"),
            ],
            true,
            "dismiss",
            palette,
        )
        .render();
        let ElementKind::Popover { floating, .. } = dropdown.kind else {
            panic!("open dropdown must have a popover");
        };
        let ElementKind::Panel(content) = floating.kind else {
            panic!("popover must contain a panel");
        };
        let ElementKind::Column(options) = content.kind else {
            panic!("short dropdown must contain an option column");
        };
        assert_eq!(
            options[0].style.hover_background,
            Some(lighter(palette.selected, 8))
        );
        assert_eq!(options[1].style.hover_background, Some(palette.hover));
    }

    #[test]
    fn dropdown_rows_keep_one_hit_target_and_stable_icon_columns() {
        let palette = ComponentPalette::neutral_dark();
        let dropdown = DropdownList::new(
            Element::button("trigger", "Agente").size(Some(100.0), Some(30.0)),
            vec![
                DropdownOption::new("codex", "Codex")
                    .icon(Icon::Chat)
                    .selected(true),
                DropdownOption::new("native", "Nativo"),
                DropdownOption::new("offline", "No instalado").disabled(),
            ],
            true,
            "dismiss",
            palette,
        )
        .width(180.0)
        .render();
        let mut tree = UiTree::new(dropdown);
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(400.0, 300.0));
        let rows: Vec<_> = frame
            .regions
            .iter()
            .filter(|region| region.id == "codex" || region.id == "native")
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].rect.width, rows[1].rect.width);
        assert_eq!(rows[0].rect.height, 36.0);
        assert_eq!(rows[1].rect.height, 36.0);
        assert!(!frame.regions.iter().any(|region| region.id == "offline"));
        assert_eq!(
            frame
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
            1
        );
        tree.handle_event(UiEvent::PointerMove(Point::new(
            rows[1].rect.x + 4.0,
            rows[1].rect.y + 4.0,
        )));
        let hovered = tree.layout(Size::new(400.0, 300.0));
        assert!(hovered.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, .. }
                if *rect == rows[1].rect && *color == palette.hover
        )));
    }

    #[test]
    fn dropdown_keyboard_stays_in_menu_skips_disabled_and_returns_to_trigger() {
        let palette = ComponentPalette::neutral_dark();
        let make_dropdown = || {
            DropdownList::new(
                Element::button("trigger", "Agente").size(Some(100.0), Some(30.0)),
                vec![
                    DropdownOption::new("codex", "Codex"),
                    DropdownOption::new("offline", "No instalado").disabled(),
                    DropdownOption::new("claude", "Claude"),
                ],
                true,
                "dismiss",
                palette,
            )
            .width(170.0)
            .render()
        };
        let key = |key, shift| UiEvent::Key {
            key,
            shift,
            ctrl: false,
        };
        let mut tree = UiTree::new(make_dropdown());
        tree.layout(Size::new(400.0, 300.0));
        assert!(tree.focus("trigger"));

        tree.handle_event(key(crate::Key::ArrowDown, false));
        assert_eq!(tree.focused_id(), Some("codex"));
        tree.handle_event(key(crate::Key::ArrowDown, false));
        assert_eq!(tree.focused_id(), Some("claude"));
        tree.handle_event(key(crate::Key::ArrowDown, false));
        assert_eq!(tree.focused_id(), Some("codex"));
        tree.handle_event(key(crate::Key::End, false));
        assert_eq!(tree.focused_id(), Some("claude"));
        tree.handle_event(key(crate::Key::Home, false));
        assert_eq!(tree.focused_id(), Some("codex"));
        tree.handle_event(key(crate::Key::Tab, true));
        assert_eq!(tree.focused_id(), Some("claude"));
        tree.handle_event(key(crate::Key::Tab, false));
        assert_eq!(tree.focused_id(), Some("codex"));
        assert_eq!(
            tree.handle_event(key(crate::Key::Enter, false)),
            vec![UiAction::Clicked { id: "codex".into() }]
        );

        tree.set_root(Element::button("trigger", "Agente"));
        tree.layout(Size::new(400.0, 300.0));
        assert_eq!(tree.focused_id(), Some("trigger"));

        tree.set_root(make_dropdown());
        tree.layout(Size::new(400.0, 300.0));
        tree.handle_event(key(crate::Key::Tab, false));
        assert_eq!(tree.focused_id(), Some("codex"));
        assert_eq!(
            tree.handle_event(key(crate::Key::Escape, false)),
            vec![UiAction::Clicked {
                id: "dismiss".into()
            }]
        );
        assert_eq!(tree.focused_id(), Some("trigger"));
    }

    #[test]
    fn long_dropdown_reveals_options_reached_by_keyboard() {
        let palette = ComponentPalette::neutral_dark();
        let options = (0..12)
            .map(|index| {
                let option =
                    DropdownOption::new(format!("option:{index}"), format!("Opción {index}"));
                if index == 5 {
                    option.disabled()
                } else {
                    option
                }
            })
            .collect();
        let dropdown = DropdownList::new(
            Element::button("trigger", "Agente").size(Some(100.0), Some(30.0)),
            options,
            true,
            "dismiss",
            palette,
        )
        .width(180.0)
        .render();
        let mut tree = UiTree::new(dropdown);
        let viewport = Size::new(400.0, 400.0);
        tree.layout(viewport);
        assert!(tree.focus("trigger"));
        let key = |key| UiEvent::Key {
            key,
            shift: false,
            ctrl: false,
        };
        for _ in 0..9 {
            tree.handle_event(key(crate::Key::ArrowDown));
        }
        assert_eq!(tree.focused_id(), Some("option:9"));
        assert!(tree.scroll_offset("dismiss:options") > 0.0);
        let scrolled = tree.layout(viewport);
        assert!(scrolled
            .regions
            .iter()
            .any(|region| region.id == "option:9"));
        tree.handle_event(key(crate::Key::End));
        assert_eq!(tree.focused_id(), Some("option:11"));
        let end = tree.layout(viewport);
        assert!(end.regions.iter().any(|region| region.id == "option:11"));
        tree.handle_event(key(crate::Key::Home));
        assert_eq!(tree.focused_id(), Some("option:0"));
        assert_eq!(tree.scroll_offset("dismiss:options"), 0.0);
        let home = tree.layout(viewport);
        assert!(home.regions.iter().any(|region| region.id == "option:0"));
        tree.handle_event(UiEvent::Key {
            key: crate::Key::Tab,
            shift: true,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("option:11"));
        let wrapped = tree.layout(viewport);
        assert!(wrapped
            .regions
            .iter()
            .any(|region| region.id == "option:11"));
    }

    #[test]
    fn clicking_outside_dropdown_returns_focus_before_dismiss_action() {
        let palette = ComponentPalette::neutral_dark();
        let dropdown = DropdownList::new(
            Element::button("trigger", "Agente").size(Some(100.0), Some(30.0)),
            vec![DropdownOption::new("codex", "Codex")],
            true,
            "dismiss",
            palette,
        )
        .render();
        let mut tree = UiTree::new(dropdown);
        tree.layout(Size::new(400.0, 300.0));
        assert!(tree.focus("codex"));
        let outside = Point::new(390.0, 290.0);
        tree.handle_event(UiEvent::PointerDown(outside));
        assert_eq!(tree.focused_id(), Some("trigger"));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(outside)),
            vec![UiAction::Clicked {
                id: "dismiss".into()
            }]
        );
    }

    #[test]
    fn disabled_options_are_visible_without_click_regions() {
        let palette = ComponentPalette::neutral_dark();
        let tabs = Tabs::new(
            vec![Tab::new("soon", "Próximamente", 140.0).disabled()],
            palette,
        )
        .render();
        let frame = UiTree::new(tabs).layout(Size::new(180.0, 40.0));
        assert!(!frame.regions.iter().any(|region| region.id == "soon"));
        let dropdown = DropdownList::new(
            Element::button("trigger", "Agente"),
            vec![DropdownOption::new("offline", "No instalado").disabled()],
            true,
            "dismiss",
            palette,
        )
        .render();
        let frame = UiTree::new(dropdown).layout(Size::new(300.0, 200.0));
        assert!(!frame.regions.iter().any(|region| region.id == "offline"));
    }
}
