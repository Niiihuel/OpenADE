//! The window is designed dark: every color in the code is a dark one. A
//! light theme is that same frame with its colors carried over, each one's
//! lightness put where its role sits on a light surface and its hue kept.
//! It is done to the commands of a frame on their way to being painted, so
//! layout stays shared between themes. Contrast, tint and opaque glass are
//! applied in that same native paint step.
//!
//! Pictures are left as they are: a page of the browser, an image.

use openide_ui::{Color, DrawCommand, Frame};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use crate::appearance_preferences::AppearancePreferences;
use crate::custom_theme::{CustomTheme, ThemeAppearance};

thread_local! {
    static CUSTOM: std::cell::Cell<Option<CustomPalette>> = const { std::cell::Cell::new(None) };
}

const FILE: &str = "theme";

/// What the user asked for; the system's own theme decides the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemeChoice {
    System,
    Light,
    Dark,
    HighContrastLight,
    HighContrastDark,
    Custom,
}

impl ThemeChoice {
    pub const ALL: [Self; 6] = [
        Self::System,
        Self::Light,
        Self::Dark,
        Self::HighContrastLight,
        Self::HighContrastDark,
        Self::Custom,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
            Self::HighContrastLight => "high_contrast_light",
            Self::HighContrastDark => "high_contrast_dark",
            Self::Custom => "custom",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.key() == key)
    }
}

// Dark until told otherwise: what every test of the window draws.
static CHOICE: AtomicU8 = AtomicU8::new(2);
static SYSTEM_LIGHT: AtomicBool = AtomicBool::new(false);
// One snapshot prevents a frame from combining a new hue with an old intensity.
static APPEARANCE: AtomicU32 = AtomicU32::new(210);

pub(crate) fn choice() -> ThemeChoice {
    ThemeChoice::ALL
        .get(usize::from(CHOICE.load(Ordering::Relaxed)))
        .copied()
        .unwrap_or(ThemeChoice::Dark)
}

pub(crate) fn set_choice(choice: ThemeChoice) {
    CHOICE.store(
        ThemeChoice::ALL
            .iter()
            .position(|known| *known == choice)
            .unwrap_or(2) as u8,
        Ordering::Relaxed,
    );
}

/// What the desktop says its theme is, as the window learns it each frame.
pub(crate) fn set_system_light(light: bool) {
    SYSTEM_LIGHT.store(light, Ordering::Relaxed);
}

/// Whether what is painted now is the light theme.
pub(crate) fn light() -> bool {
    match choice() {
        ThemeChoice::Light => true,
        ThemeChoice::Dark => false,
        ThemeChoice::HighContrastLight => true,
        ThemeChoice::HighContrastDark => false,
        ThemeChoice::System => SYSTEM_LIGHT.load(Ordering::Relaxed),
        ThemeChoice::Custom => {
            CUSTOM.with(|palette| palette.get().is_some_and(|palette| palette.light))
        }
    }
}

/// Updated once when a validated preference changes, never while painting.
pub(crate) fn set_appearance(preferences: &AppearancePreferences) {
    let value = u32::from(preferences.hue_degrees.min(360))
        | (u32::from(preferences.tint_intensity.min(1000)) << 9)
        | (u32::from(preferences.reduce_transparency) << 19);
    APPEARANCE.store(value, Ordering::Relaxed);
    let custom = preferences
        .active_theme()
        .filter(|theme| theme.validate().is_ok())
        .map(CustomPalette::new);
    CUSTOM.with(|palette| palette.set(custom));
    set_choice(choice_with_custom(choice(), custom.is_some()));
}

fn choice_with_custom(choice: ThemeChoice, available: bool) -> ThemeChoice {
    if choice == ThemeChoice::Custom && !available {
        ThemeChoice::Dark
    } else {
        choice
    }
}

pub(crate) fn needs_transform() -> bool {
    matches!(choice(), ThemeChoice::Custom)
        || light()
        || matches!(
            choice(),
            ThemeChoice::HighContrastLight | ThemeChoice::HighContrastDark
        )
        || APPEARANCE.load(Ordering::Relaxed) >> 9 != 0
}

/// The app's semantic colors are dark at layout time; transform only native
/// paint commands. Images, browser pixels and hit-test geometry stay intact.
pub(crate) fn transform(frame: &mut Frame) {
    let settings = APPEARANCE.load(Ordering::Relaxed);
    PaintPreferences {
        light: light(),
        high_contrast: matches!(
            choice(),
            ThemeChoice::HighContrastLight | ThemeChoice::HighContrastDark
        ),
        hue: (settings & 511) as f32,
        intensity: ((settings >> 9) & 1023) as f32 / 1000.0,
        reduce_transparency: settings & (1 << 19) != 0,
        custom: if choice() == ThemeChoice::Custom {
            CUSTOM.with(std::cell::Cell::get)
        } else {
            None
        },
    }
    .apply(frame);
}

#[derive(Clone, Copy)]
struct PaintPreferences {
    light: bool,
    high_contrast: bool,
    hue: f32,
    intensity: f32,
    reduce_transparency: bool,
    custom: Option<CustomPalette>,
}

impl PaintPreferences {
    fn color(self, color: Color, surface: bool, border: bool) -> Color {
        if let Some(custom) = self.custom {
            return custom.color(color, surface, border);
        }
        let mut painted = if self.light { lit(color) } else { color };
        if color.a == 0 {
            return painted;
        }
        let neutral = color.r.abs_diff(color.g) <= 3 && color.g.abs_diff(color.b) <= 3;
        if self.high_contrast && neutral {
            let gray = if border {
                if self.light { 0 } else { 255 }
            } else if surface && color.r < 110 {
                let dark = if color.r <= 32 {
                    0
                } else if color.r <= 46 {
                    12
                } else {
                    24
                };
                if self.light { 255 - dark } else { dark }
            } else if color.r >= 100 {
                if self.light { 0 } else { 255 }
            } else {
                painted.r
            };
            painted = Color::rgba(gray, gray, gray, color.a);
        }
        // Tint neutral native surfaces, preserving their lightness and the
        // semantic hues of syntax, errors, links and diff additions/deletions.
        if self.intensity > 0.0 && neutral && surface && color.r <= 110 {
            let lightness = f32::from(painted.r) / 255.0;
            let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * self.intensity * 0.45;
            let hue = (self.hue % 360.0) / 60.0;
            let x = chroma * (1.0 - (hue % 2.0 - 1.0).abs());
            let (r, g, b) = match hue as u8 {
                0 => (chroma, x, 0.0),
                1 => (x, chroma, 0.0),
                2 => (0.0, chroma, x),
                3 => (0.0, x, chroma),
                4 => (x, 0.0, chroma),
                _ => (chroma, 0.0, x),
            };
            let floor = lightness - chroma / 2.0;
            let channel = |value: f32| ((floor + value) * 255.0).round().clamp(0.0, 255.0) as u8;
            painted = Color::rgba(channel(r), channel(g), channel(b), color.a);
        }
        painted
    }

    fn apply(self, frame: &mut Frame) {
        for command in &mut frame.commands {
            match command {
                DrawCommand::FillRect { color, .. }
                | DrawCommand::FillCornerRect { color, .. }
                | DrawCommand::CornerMask { color, .. } => *color = self.color(*color, true, false),
                DrawCommand::StrokeRect { color, .. }
                | DrawCommand::StrokeCornerRect { color, .. }
                | DrawCommand::SelectionContour { color, .. } => {
                    *color = self.color(*color, false, true)
                }
                DrawCommand::FillTriangle { color, .. }
                | DrawCommand::Polyline { color, .. }
                | DrawCommand::Text { color, .. }
                | DrawCommand::Spinner { color, .. } => *color = self.color(*color, false, false),
                DrawCommand::Icon { icon, tint, .. } => {
                    // Vendored brand artwork already selects its light/dark
                    // variant. White is its neutral multiplier, not UI ink.
                    if !crate::brand_icons::is_brand_icon(icon) {
                        *tint = self.color(*tint, false, false);
                    }
                }
                DrawCommand::FillVerticalGradient { top, bottom, .. } => {
                    *top = self.color(*top, true, false);
                    *bottom = self.color(*bottom, true, false);
                }
                DrawCommand::FillHorizontalGradient { left, right, .. } => {
                    *left = self.color(*left, true, false);
                    *right = self.color(*right, true, false);
                }
                DrawCommand::Record { fill, .. } => *fill = self.color(*fill, true, false),
                DrawCommand::ProgressRing { track, fill, .. } => {
                    *track = self.color(*track, false, false);
                    *fill = self.color(*fill, false, false);
                }
                DrawCommand::Glass {
                    rect,
                    radius,
                    material,
                    opacity,
                } => {
                    let mut color = self.color(material.tint, true, false);
                    if self.reduce_transparency {
                        // Only a backdrop-sampling surface becomes opaque.
                        // Keep the transition's opacity; shadows and veils are
                        // not surfaces and must remain translucent.
                        color.a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
                        *command = DrawCommand::FillRect {
                            rect: *rect,
                            radius: *radius,
                            color,
                        };
                    } else {
                        material.tint = color;
                    }
                }
                _ => {}
            }
        }
    }
}

/// Resolved once when preferences change. Painting never parses color strings
/// or clones the imported library. Roles sharing native RGB values intentionally
/// share the corresponding editable group.
#[derive(Clone, Copy)]
struct CustomPalette {
    light: bool,
    high_contrast: bool,
    sidebar: Color,
    chrome: Color,
    editor: Color,
    base: Color,
    accent: Color,
    action_label: Color,
    brand: Color,
    focus: Color,
    warn: Color,
    danger: Color,
    added: Color,
    removed: Color,
    blue: Color,
    purple: Color,
    green: Color,
    orange: Color,
    diff_added: Color,
    diff_removed: Color,
}

impl CustomPalette {
    fn new(theme: &CustomTheme) -> Self {
        Self {
            light: theme.appearance == ThemeAppearance::Light,
            high_contrast: theme.high_contrast,
            sidebar: theme.normalized("sidebar"),
            chrome: theme.normalized("chrome"),
            editor: theme.normalized("editor"),
            base: theme.normalized("base"),
            accent: theme.normalized("accent"),
            action_label: theme.normalized("actionLabel"),
            brand: theme.normalized("brand"),
            focus: theme.normalized("focus"),
            warn: theme.normalized("warn"),
            danger: theme.normalized("danger"),
            added: theme.normalized("added"),
            removed: theme.normalized("removed"),
            blue: theme.normalized("blue"),
            purple: theme.normalized("purple"),
            green: theme.normalized("green"),
            orange: theme.normalized("orange"),
            diff_added: theme.normalized("diffAddedLineBackground"),
            diff_removed: theme.normalized("diffRemovedLineBackground"),
        }
    }

    fn color(self, source: Color, surface: bool, border: bool) -> Color {
        use crate::theme as t;
        if source.a == 0 {
            return source;
        }
        let rgb = |color: Color| (color.r, color.g, color.b);
        let same = |color| rgb(source) == rgb(color);
        let neutral = source.r.abs_diff(source.g) <= 3 && source.g.abs_diff(source.b) <= 3;
        // Native diff views use row tints at alpha 20 (diff) and 28 (review).
        // Keep their relative strength and transition opacity. The configured
        // alpha sets the strongest tint; never infer a role from faded alpha.
        if surface && (same(Color::rgb(72, 142, 92)) || same(Color::rgb(170, 76, 85))) {
            let configured = if same(Color::rgb(72, 142, 92)) {
                self.diff_added
            } else {
                self.diff_removed
            };
            let intrinsic_alpha = 28;
            return Color {
                a: ((u16::from(configured.a) * u16::from(source.a) + intrinsic_alpha / 2)
                    / intrinsic_alpha)
                    .min(255) as u8,
                ..configured
            };
        }
        let painted = if border && neutral {
            self.focus
        } else if same(t::DIFF_ADDED_BG) {
            self.diff_added
        } else if same(t::DIFF_REMOVED_BG) {
            self.diff_removed
        } else if same(t::DESTRUCTIVE) || same(t::CONTEXT_FULL) {
            self.danger
        } else if same(t::WARNING) || same(t::CONTEXT_WARNING) {
            self.warn
        } else if same(t::DIFF_ADDED) || same(t::ACTIVITY_ADDED) || same(Color::rgb(105, 185, 135))
        {
            self.added
        } else if same(t::DIFF_REMOVED)
            || same(t::ACTIVITY_REMOVED)
            || same(Color::rgb(210, 115, 122))
        {
            self.removed
        } else if same(t::LINK)
            || same(t::EFFORT_BLUE)
            || same(t::SEND_BLUE)
            || same(Color::rgb(143, 185, 218))
            || same(Color::rgb(160, 191, 217))
        {
            self.blue
        } else if same(t::CODE_KEYWORD) || same(t::EFFORT_ULTRA) || same(Color::rgb(184, 143, 216))
        {
            self.purple
        } else if same(t::CODE_STRING) || same(Color::rgb(206, 163, 127)) {
            self.green
        } else if same(t::CODE_NUMBER)
            || same(Color::rgb(151, 193, 169))
            || same(Color::rgb(225, 196, 131))
        {
            self.orange
        } else if same(Color::rgb(126, 151, 119)) {
            mix(
                self.editor,
                self.base,
                if self.high_contrast { 0.8 } else { 0.6 },
            )
        } else if neutral && surface {
            match source.r {
                22 | 25 => self.editor,
                30 | 32 => self.chrome,
                43 => self.sidebar,
                255 => self.accent,
                // Black veils are compositor effects, not a native surface.
                0 => return source,
                1..=110 => mix(
                    self.sidebar,
                    self.base,
                    (f32::from(source.r) - 43.0).max(0.0) / 300.0,
                ),
                _ => mix(self.editor, self.base, f32::from(source.r) / 255.0),
            }
        } else if neutral {
            match source.r {
                0 => self.action_label,
                224 | 255 => self.brand,
                212 | 235 | 246 => self.base,
                _ => mix(
                    self.editor,
                    self.base,
                    (f32::from(source.r) / 212.0)
                        .clamp(if self.high_contrast { 0.7 } else { 0.2 }, 1.0),
                ),
            }
        } else {
            return if self.light { lit(source) } else { source };
        };
        Color::rgba(
            painted.r,
            painted.g,
            painted.b,
            ((u16::from(painted.a) * u16::from(source.a) + 127) / 255) as u8,
        )
    }
}

fn mix(background: Color, foreground: Color, amount: f32) -> Color {
    let channel = |background: u8, foreground: u8| {
        (f32::from(background) + (f32::from(foreground) - f32::from(background)) * amount)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::rgba(
        channel(background.r, foreground.r),
        channel(background.g, foreground.g),
        channel(background.b, foreground.b),
        channel(background.a, foreground.a),
    )
}

/// The theme chosen on this machine; dark when none was.
pub(crate) fn load(root: &Path) {
    let saved = std::fs::read_to_string(root.join(FILE)).ok();
    set_choice(
        saved
            .as_deref()
            .map(str::trim)
            .and_then(ThemeChoice::from_key)
            .unwrap_or(ThemeChoice::Dark),
    );
}

pub(crate) fn save(root: &Path, choice: ThemeChoice) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    std::fs::write(root.join(FILE), choice.key())
}

/// Where a lightness of the dark design sits in the light one: the canvas,
/// the panels, the raised surfaces, the borders, the muted and the plain
/// text, each kept apart from its neighbours as it is in the dark.
const CURVE: [(f32, f32); 12] = [
    (0.0, 255.0),
    (22.0, 250.0),
    (32.0, 244.0),
    (43.0, 238.0),
    (54.0, 228.0),
    (78.0, 205.0),
    (110.0, 150.0),
    (136.0, 112.0),
    (160.0, 92.0),
    (212.0, 32.0),
    (235.0, 20.0),
    (255.0, 12.0),
];

fn along(points: impl Iterator<Item = (f32, f32)> + Clone, at: f32) -> f32 {
    let mut before = points.clone().next().unwrap_or((0.0, 0.0));
    for point in points {
        if at <= point.0 {
            let span = point.0 - before.0;
            return if span <= 0.0 {
                point.1
            } else {
                before.1 + (point.1 - before.1) * (at - before.0) / span
            };
        }
        before = point;
    }
    before.1
}

fn lightness(dark: f32) -> f32 {
    along(CURVE.into_iter(), dark.clamp(0.0, 255.0))
}

#[cfg(test)]
/// The gray of the dark design that the light theme paints as `light`: for
/// a picture of a theme that must look the same in either.
pub(crate) fn gray_painted_as(light: u8) -> u8 {
    along(
        CURVE.into_iter().rev().map(|(dark, light)| (light, dark)),
        f32::from(light),
    )
    .round() as u8
}

/// A color of the dark design as the light theme paints it.
pub(crate) fn lit(color: Color) -> Color {
    let (r, g, b) = (f32::from(color.r), f32::from(color.g), f32::from(color.b));
    let (high, low) = (r.max(g).max(b), r.min(g).min(b));
    let was = (high + low) / 2.0;
    let now = lightness(was);
    if high - low < 1.0 {
        let gray = now.round() as u8;
        return Color::rgba(gray, gray, gray, color.a);
    }
    // The hue and how vivid it is stay; only how light it is moves.
    let chroma = |lightness: f32| 255.0 - (2.0 * lightness - 255.0).abs();
    let saturation = (high - low) / chroma(was).max(1.0);
    let spread = saturation * chroma(now);
    let floor = now - spread / 2.0;
    let channel = |value: f32| {
        (floor + (value - low) / (high - low) * spread)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::rgba(channel(r), channel(g), channel(b), color.a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paint_preferences() -> PaintPreferences {
        PaintPreferences {
            light: false,
            high_contrast: false,
            hue: 210.0,
            intensity: 0.0,
            reduce_transparency: false,
            custom: None,
        }
    }

    #[test]
    fn brand_art_keeps_its_neutral_multiplier_across_native_theme_transforms() {
        use openide_ui::{Element, Icon, Size, UiTree};
        let entry = crate::brand_icons::search("Figma", 1)[0];
        let white = Color::rgb(255, 255, 255);
        let custom = CustomTheme::new("Brand review", ThemeAppearance::Light);
        for preferences in [
            PaintPreferences { light: true, ..paint_preferences() },
            PaintPreferences { light: true, high_contrast: true, ..paint_preferences() },
            PaintPreferences { high_contrast: true, intensity: 1.0, ..paint_preferences() },
            PaintPreferences { custom: Some(CustomPalette::new(&custom)), ..paint_preferences() },
        ] {
            let mut frame = UiTree::new(Element::row(vec![
                entry.element(preferences.light),
                Element::icon(Icon::Grid).foreground(white),
            ])).layout(Size::new(80.0, 30.0));
            preferences.apply(&mut frame);
            let mut brands = 0;
            let mut ordinary = 0;
            for command in &frame.commands {
                if let DrawCommand::Icon { icon, tint, .. } = command {
                    if crate::brand_icons::is_brand_icon(icon) {
                        assert_eq!(*tint, white);
                        brands += 1;
                    } else {
                        assert_eq!(*tint, preferences.color(white, false, false));
                        ordinary += 1;
                    }
                }
            }
            assert_eq!((brands, ordinary), (1, 1));
        }
    }

    #[test]
    fn tint_is_disabled_at_zero_and_preserves_semantic_inks_and_alpha() {
        let base = paint_preferences();
        let surface = Color::rgb(43, 43, 43);
        assert_eq!(base.color(surface, true, false), surface);
        let blue = PaintPreferences {
            intensity: 1.0,
            ..base
        };
        let tinted = blue.color(surface, true, false);
        assert!(tinted.b > tinted.g && tinted.g > tinted.r, "{tinted:?}");
        assert_eq!(
            blue.color(crate::theme::DIFF_ADDED_BG, true, false),
            crate::theme::DIFF_ADDED_BG
        );
        assert_eq!(
            blue.color(crate::theme::LINK, false, false),
            crate::theme::LINK
        );
        assert_eq!(
            blue.color(Color::TRANSPARENT, true, false),
            Color::TRANSPARENT
        );
        let veil = Color::rgba(43, 43, 43, 42);
        assert_eq!(blue.color(veil, true, false).a, 42);
        let light = PaintPreferences {
            light: true,
            ..base
        };
        assert_eq!(light.color(surface, true, false), lit(surface));
    }

    #[test]
    fn contrast_separates_roles_and_reduced_transparency_only_replaces_glass() {
        use openide_ui::{Element, GlassMaterial, Size, UiTree};
        for light in [false, true] {
            let contrast = PaintPreferences {
                light,
                high_contrast: true,
                ..paint_preferences()
            };
            let background = contrast.color(crate::theme::BACKGROUND, true, false);
            let text = contrast.color(crate::theme::FOREGROUND, false, false);
            let border = contrast.color(crate::theme::BORDER, false, true);
            assert_eq!(background.r, if light { 255 } else { 0 });
            assert_eq!(text.r, if light { 0 } else { 255 });
            assert_eq!(border, text);
        }
        let mut frame = UiTree::new(
            Element::panel(Element::text("A"))
                .glass(GlassMaterial::dark())
                .radius(8.0)
                .shadow(
                    Color::rgba(0, 0, 0, 125),
                    openide_ui::Point::new(0.0, 2.0),
                    8.0,
                    0.0,
                ),
        )
        .layout(Size::new(200.0, 80.0));
        let before = frame.clone();
        PaintPreferences {
            reduce_transparency: true,
            ..paint_preferences()
        }
        .apply(&mut frame);
        assert_eq!(frame.regions, before.regions);
        assert_eq!(
            frame.commands[0], before.commands[0],
            "shadow alpha stays translucent"
        );
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { color, radius: 8.0, .. } if color.a == 255
        )));
        assert!(
            !frame
                .commands
                .iter()
                .any(|command| matches!(command, DrawCommand::Glass { .. }))
        );
        assert_eq!(
            frame.commands.last(),
            before.commands.last(),
            "foreground stays unchanged"
        );
    }

    #[test]
    fn the_light_theme_keeps_surfaces_apart_text_readable_and_hues_where_they_were() {
        let gray = |value: u8| lit(Color::rgb(value, value, value)).r;
        // Canvas, panel, raised, selected: still four surfaces, now light.
        let surfaces = [22, 32, 43, 54].map(gray);
        assert!(
            surfaces.windows(2).all(|pair| pair[0] > pair[1]) && surfaces[3] > 220,
            "{surfaces:?}"
        );
        // Text and muted text are dark on them, the plain one darker.
        assert!(gray(212) < 40 && gray(136) < 130 && gray(212) < gray(136));
        // A link stays blue and becomes dark enough to read on a light surface.
        let link = lit(Color::rgb(120, 169, 255));
        assert!(
            link.b > link.g && link.g > link.r && link.b < 200,
            "{link:?}"
        );
        // Transparency is the color's own.
        assert_eq!(lit(Color::rgba(0, 0, 0, 125)).a, 125);
        // A picture of a theme asks for the gray that ends as the one it wants.
        for wanted in [250u8, 238, 112, 32] {
            assert!(
                gray(gray_painted_as(wanted)).abs_diff(wanted) <= 2,
                "{wanted}"
            );
        }
    }

    #[test]
    fn a_choice_is_named_and_read_back() {
        for choice in ThemeChoice::ALL {
            assert_eq!(ThemeChoice::from_key(choice.key()), Some(choice));
        }
        assert_eq!(ThemeChoice::from_key("sepia"), None);
    }

    #[test]
    fn startup_custom_selection_falls_back_to_dark_when_saved_library_is_invalid() {
        let value = serde_json::json!({
            "active_custom_theme": "missing",
            "custom_themes": [{"id":"missing","name":"Invalid"}],
        });
        let preferences = AppearancePreferences::load_value(Some(&value), false);
        assert_eq!(
            choice_with_custom(ThemeChoice::Custom, preferences.active_theme().is_some()),
            ThemeChoice::Dark
        );
        assert_eq!(
            choice_with_custom(ThemeChoice::Light, false),
            ThemeChoice::Light
        );
        assert_eq!(
            choice_with_custom(ThemeChoice::Custom, true),
            ThemeChoice::Custom
        );
    }

    #[test]
    fn every_editable_custom_token_has_a_distinct_native_effect_and_preserves_source_alpha() {
        use crate::custom_theme::{CustomTheme, EDITABLE_TOKENS, ThemeAppearance};
        use crate::theme as t;
        let sources = [
            ("sidebar", t::SIDEBAR, true, false),
            ("chrome", t::PANEL, true, false),
            ("editor", t::CODE_SURFACE, true, false),
            ("base", t::FOREGROUND, false, false),
            ("accent", t::BRAND, true, false),
            ("actionLabel", t::PRIMARY_FOREGROUND, false, false),
            ("brand", t::ICON_PRIMARY, false, false),
            ("focus", t::BORDER, false, true),
            ("warn", t::WARNING, false, false),
            ("danger", t::DESTRUCTIVE, false, false),
            ("added", t::DIFF_ADDED, false, false),
            ("removed", t::DIFF_REMOVED, false, false),
            ("blue", t::LINK, false, false),
            ("purple", t::CODE_KEYWORD, false, false),
            ("green", t::CODE_STRING, false, false),
            ("orange", t::CODE_NUMBER, false, false),
            ("diffAddedLineBackground", t::DIFF_ADDED_BG, true, false),
            ("diffRemovedLineBackground", t::DIFF_REMOVED_BG, true, false),
        ];
        assert_eq!(sources.len(), EDITABLE_TOKENS.len());
        for (token, source, surface, border) in sources {
            assert!(
                EDITABLE_TOKENS
                    .iter()
                    .any(|(editable, _)| *editable == token)
            );
            let mut theme = CustomTheme::new("Paint", ThemeAppearance::Dark);
            let configured = Color::rgba(17, 89, 131, 170);
            theme.set_color(token, configured);
            let preferences = PaintPreferences {
                custom: Some(CustomPalette::new(&theme)),
                ..paint_preferences()
            };
            assert_eq!(
                preferences.color(source, surface, border),
                configured,
                "{token}"
            );
            let translucent = Color::rgba(source.r, source.g, source.b, 153);
            assert_eq!(
                preferences.color(translucent, surface, border).a,
                102,
                "{token}"
            );
            assert_eq!(
                preferences.color(Color::TRANSPARENT, surface, border),
                Color::TRANSPARENT
            );
        }
    }

    #[test]
    fn custom_colors_reach_native_diff_rows_and_editor_syntax() {
        use crate::custom_theme::{CustomTheme, ThemeAppearance};
        use openide_ui::{CodeDiff, CodeDiffLine, CodeDiffLineKind, Component, Size, UiTree};
        let mut theme = CustomTheme::new("Code", ThemeAppearance::Dark);
        let added = Color::rgba(20, 70, 40, 180);
        let removed = Color::rgba(90, 20, 30, 160);
        let keyword = Color::rgb(40, 100, 220);
        theme.set_color("diffAddedLineBackground", added);
        theme.set_color("diffRemovedLineBackground", removed);
        theme.set_color("purple", keyword);
        let preferences = PaintPreferences {
            custom: Some(CustomPalette::new(&theme)),
            ..paint_preferences()
        };
        let lines = vec![
            CodeDiffLine {
                kind: CodeDiffLineKind::Removed,
                old_line: Some(1),
                new_line: None,
                text: "let total = first;".into(),
            },
            CodeDiffLine {
                kind: CodeDiffLineKind::Added,
                old_line: None,
                new_line: Some(1),
                text: "let total = first + second;".into(),
            },
        ];
        let mut frame = UiTree::new(CodeDiff::new("theme-diff", lines).height(80.0).render())
            .layout(Size::new(360.0, 80.0));
        preferences.apply(&mut frame);
        for (configured, expected_alpha) in [(added, 129), (removed, 114)] {
            assert!(
                frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::FillRect { color, rect, .. }
                        if (color.r, color.g, color.b) == (configured.r, configured.g, configured.b)
                            && color.a == expected_alpha && rect.width > 100.0
                )),
                "missing configured diff tint {configured:?}"
            );
        }
        assert_eq!(
            preferences.color(Color::rgb(184, 143, 216), false, false),
            keyword
        );
    }

    #[test]
    fn native_diff_tints_preserve_relative_strength_and_fade_without_an_opacity_jump() {
        use crate::custom_theme::{CustomTheme, ThemeAppearance};
        let mut theme = CustomTheme::new("Fading diff", ThemeAppearance::Dark);
        theme.set_color("diffAddedLineBackground", Color::rgba(20, 70, 40, 180));
        let preferences = PaintPreferences {
            custom: Some(CustomPalette::new(&theme)),
            ..paint_preferences()
        };
        let tint = |alpha| preferences.color(Color::rgba(72, 142, 92, alpha), true, false);
        assert_eq!(tint(28), Color::rgba(20, 70, 40, 180));
        assert_eq!(tint(14), Color::rgba(20, 70, 40, 90));
        assert_eq!(tint(10).a, 64, "half of the native diff's tint strength");
        assert!((0..28).all(|alpha| tint(alpha).a <= tint(alpha + 1).a));
    }

    #[test]
    fn custom_palette_is_used_exactly_and_builtin_modes_do_not_depend_on_library_selection() {
        use crate::custom_theme::{CustomTheme, ThemeAppearance};
        let mut theme = CustomTheme::new("Light", ThemeAppearance::Light);
        theme.high_contrast = true;
        theme.set_color("editor", Color::rgba(231, 222, 213, 211));
        let custom = PaintPreferences {
            light: true,
            high_contrast: true,
            intensity: 1.0,
            custom: Some(CustomPalette::new(&theme)),
            ..paint_preferences()
        };
        assert_eq!(
            custom.color(crate::theme::BACKGROUND, true, false),
            theme.normalized("editor")
        );
        assert_eq!(
            paint_preferences().color(crate::theme::BACKGROUND, true, false),
            crate::theme::BACKGROUND
        );
        let light = PaintPreferences {
            light: true,
            ..paint_preferences()
        };
        assert_eq!(
            light.color(crate::theme::BACKGROUND, true, false),
            lit(crate::theme::BACKGROUND)
        );
    }

    #[test]
    fn custom_frame_changes_native_paint_without_changing_geometry_pixels_or_color_samples() {
        use crate::custom_theme::{CustomTheme, ThemeAppearance};
        use openide_ui::{Element, Point, Rect, Size, UiTree};
        let mut theme = CustomTheme::new("Paint", ThemeAppearance::Dark);
        theme.set_color("editor", Color::rgb(13, 27, 41));
        let mut frame = UiTree::new(
            Element::panel(Element::button("action", "Run")).background(crate::theme::BACKGROUND),
        )
        .layout(Size::new(180.0, 70.0));
        frame.commands.push(DrawCommand::Surface {
            rect: Rect::new(1.0, 2.0, 3.0, 4.0),
            id: "browser".into(),
            label: "Browser pixels".into(),
            opacity: 0.7,
        });
        frame.commands.push(DrawCommand::FillMesh {
            points: vec![
                Point::new(0.0, 0.0),
                Point::new(1.0, 0.0),
                Point::new(1.0, 1.0),
            ]
            .into(),
            colors: vec![
                crate::theme::BACKGROUND,
                crate::theme::FOREGROUND,
                crate::theme::BRAND,
            ]
            .into(),
            indices: vec![0, 1, 2].into(),
        });
        let before = frame.clone();
        PaintPreferences {
            custom: Some(CustomPalette::new(&theme)),
            ..paint_preferences()
        }
        .apply(&mut frame);
        assert_eq!(frame.regions, before.regions);
        assert_eq!(frame.access, before.access);
        assert_eq!(frame.commands.len(), before.commands.len());
        assert_eq!(
            &frame.commands[frame.commands.len() - 2..],
            &before.commands[before.commands.len() - 2..]
        );
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { color, .. } if *color == theme.normalized("editor")
        )));
    }
}
