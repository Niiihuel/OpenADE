//! Validated presentation preferences, independent of the UI tree and renderer.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::RefCell;

use crate::custom_theme::{CustomTheme, LIBRARY_VERSION, MAX_CUSTOM_THEMES};

pub(crate) const UI_FONT_MIN: u8 = 7;
pub(crate) const UI_FONT_MAX: u8 = 38;
pub(crate) const CODE_FONT_MIN: u8 = 10;
pub(crate) const CODE_FONT_MAX: u8 = 22;

thread_local! {
    static CURRENT: RefCell<AppearancePreferences> = RefCell::new(AppearancePreferences::default());
}

#[cfg(test)]
pub(crate) fn current() -> AppearancePreferences {
    CURRENT.with_borrow(Clone::clone)
}

pub(crate) fn set_current(preferences: &AppearancePreferences) {
    CURRENT.with_borrow_mut(|current| current.clone_from(preferences));
}

// Rendering reads individual switches without cloning the imported library.
pub(crate) fn code_word_wrap() -> bool {
    CURRENT.with_borrow(|preferences| preferences.code_word_wrap)
}

pub(crate) fn themed_diff_backgrounds() -> bool {
    CURRENT.with_borrow(|preferences| preferences.themed_diff_backgrounds)
}

pub(crate) fn hide_email() -> bool {
    CURRENT.with_borrow(|preferences| preferences.hide_email)
}

/// UI zoom scales the whole native frame. Cancel it for code so the two
/// font size preferences remain independent at the final physical size.
pub(crate) fn code_font_size() -> f32 {
    CURRENT.with_borrow(|preferences| {
        f32::from(preferences.code_font_size) * 13.0 / f32::from(preferences.ui_font_size)
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolCallDensity {
    #[default]
    Grouped,
    Compact,
    Detailed,
}

impl ToolCallDensity {
    #[cfg(test)]
    pub const ALL: [Self; 3] = [Self::Grouped, Self::Compact, Self::Detailed];

    pub fn fraction(self) -> f32 {
        match self {
            Self::Grouped => 0.0,
            Self::Compact => 0.5,
            Self::Detailed => 1.0,
        }
    }

    pub fn from_fraction(value: f32) -> Self {
        if value >= 0.75 {
            Self::Detailed
        } else if value >= 0.25 {
            Self::Compact
        } else {
            Self::Grouped
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MotionPreference {
    #[default]
    System,
    Always,
    Never,
}

impl MotionPreference {
    pub fn reduced(self, system_reduced: bool) -> bool {
        match self {
            Self::System => system_reduced,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct AppearancePreferences {
    pub tool_call_density: ToolCallDensity,
    pub code_word_wrap: bool,
    pub themed_diff_backgrounds: bool,
    pub hue_degrees: u16,
    /// Thousandths of the tint strength; integer storage keeps equality exact.
    pub tint_intensity: u16,
    pub reduce_transparency: bool,
    pub ui_font_size: u8,
    pub code_font_size: u8,
    /// Empty means the bundled system default, never a font named "Default".
    pub ui_font_family: String,
    pub code_font_family: String,
    /// Native subpixel glyph placement; antialiasing and hinting remain active.
    pub font_smoothing: bool,
    pub reduce_motion: MotionPreference,
    pub hide_email: bool,
    pub custom_themes_version: u8,
    pub custom_themes: Vec<CustomTheme>,
    /// Remembered while a built-in is selected; only ThemeChoice::Custom applies it.
    pub active_custom_theme: Option<String>,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            tool_call_density: ToolCallDensity::Grouped,
            code_word_wrap: false,
            themed_diff_backgrounds: true,
            hue_degrees: 210,
            tint_intensity: 0,
            reduce_transparency: false,
            ui_font_size: 13,
            code_font_size: 12,
            ui_font_family: String::new(),
            code_font_family: String::new(),
            font_smoothing: true,
            reduce_motion: MotionPreference::System,
            hide_email: false,
            custom_themes_version: LIBRARY_VERSION,
            custom_themes: Vec::new(),
            active_custom_theme: None,
        }
    }
}

impl AppearancePreferences {
    /// Recover each known field separately so one invalid value cannot reset
    /// the user's other choices. The legacy switch only migrates when absent.
    pub fn load_value(value: Option<&Value>, legacy_reduced_motion: bool) -> Self {
        let mut prefs = Self::default();
        let Some(value) = value.filter(|value| value.is_object()) else {
            if legacy_reduced_motion {
                prefs.reduce_motion = MotionPreference::Always;
            }
            return prefs;
        };
        macro_rules! recover {
            ($field:ident, $valid:expr) => {
                if let Some(candidate) = value
                    .get(stringify!($field))
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
                    .filter($valid)
                {
                    prefs.$field = candidate;
                }
            };
        }
        recover!(tool_call_density, |_| true);
        recover!(code_word_wrap, |_| true);
        recover!(themed_diff_backgrounds, |_| true);
        recover!(hue_degrees, |value: &u16| *value <= 360);
        recover!(tint_intensity, |value: &u16| *value <= 1000);
        recover!(reduce_transparency, |_| true);
        recover!(ui_font_size, |value: &u8| (UI_FONT_MIN..=UI_FONT_MAX)
            .contains(value));
        recover!(code_font_size, |value: &u8| (CODE_FONT_MIN..=CODE_FONT_MAX)
            .contains(value));
        recover!(ui_font_family, |value: &String| valid_family(value));
        recover!(code_font_family, |value: &String| valid_family(value));
        recover!(font_smoothing, |_| true);
        recover!(reduce_motion, |_| true);
        recover!(hide_email, |_| true);
        // A malformed record must not discard other imported themes. Unknown
        // library versions are left unused rather than guessed at.
        if value
            .get("custom_themes_version")
            .is_none_or(|version| version.as_u64() == Some(u64::from(LIBRARY_VERSION)))
        {
            if let Some(themes) = value.get("custom_themes").and_then(Value::as_array) {
                for value in themes {
                    let Ok(theme) = serde_json::from_value::<CustomTheme>(value.clone()) else {
                        continue;
                    };
                    if theme.validate().is_ok()
                        && !prefs.custom_themes.iter().any(|known| known.id == theme.id)
                    {
                        prefs.custom_themes.push(theme);
                        if prefs.custom_themes.len() == MAX_CUSTOM_THEMES {
                            break;
                        }
                    }
                }
            }
            prefs.active_custom_theme = value
                .get("active_custom_theme")
                .and_then(Value::as_str)
                .filter(|id| prefs.custom_themes.iter().any(|theme| theme.id == *id))
                .map(str::to_owned);
        }
        prefs
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.hue_degrees > 360 || self.tint_intensity > 1000 {
            return Err("El tinte de la interfaz está fuera de rango.".into());
        }
        if !(UI_FONT_MIN..=UI_FONT_MAX).contains(&self.ui_font_size)
            || !(CODE_FONT_MIN..=CODE_FONT_MAX).contains(&self.code_font_size)
        {
            return Err("El tamaño de fuente está fuera de rango.".into());
        }
        if !valid_family(&self.ui_font_family) || !valid_family(&self.code_font_family) {
            return Err("El nombre de la fuente no es válido.".into());
        }
        if self.custom_themes_version != LIBRARY_VERSION
            || self.custom_themes.len() > MAX_CUSTOM_THEMES
        {
            return Err(
                "La biblioteca de temas no es compatible o supera el límite de 30 temas.".into(),
            );
        }
        let mut ids = std::collections::HashSet::new();
        for theme in &self.custom_themes {
            theme.validate()?;
            if !ids.insert(&theme.id) {
                return Err("La biblioteca contiene identificadores de tema duplicados.".into());
            }
        }
        if self
            .active_custom_theme
            .as_ref()
            .is_some_and(|id| !ids.contains(id))
        {
            return Err("El tema personalizado seleccionado ya no existe.".into());
        }
        Ok(())
    }

    pub fn active_theme(&self) -> Option<&CustomTheme> {
        let id = self.active_custom_theme.as_deref()?;
        self.custom_themes.iter().find(|theme| theme.id == id)
    }
}

fn valid_family(family: &str) -> bool {
    family.len() <= 256 && !family.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_appearance_migrates_motion_and_untrusted_numbers_stay_bounded() {
        assert_eq!(
            AppearancePreferences::load_value(None, true).reduce_motion,
            MotionPreference::Always
        );
        assert_eq!(
            AppearancePreferences::load_value(None, false).reduce_motion,
            MotionPreference::System
        );
        let value = serde_json::json!({
            "hue_degrees": -1, "tint_intensity": 1001,
            "ui_font_size": 6, "code_font_size": 23,
            "reduce_motion": "never", "hide_email": true,
        });
        let prefs = AppearancePreferences::load_value(Some(&value), true);
        assert_eq!(prefs.hue_degrees, 210);
        assert_eq!(prefs.tint_intensity, 0);
        assert_eq!((prefs.ui_font_size, prefs.code_font_size), (13, 12));
        assert_eq!(prefs.reduce_motion, MotionPreference::Never);
        assert!(prefs.hide_email);
        assert!(prefs.validate().is_ok());
    }

    #[test]
    fn density_stops_and_motion_overrides_have_explicit_meaning() {
        for density in ToolCallDensity::ALL {
            assert_eq!(ToolCallDensity::from_fraction(density.fraction()), density);
        }
        assert_eq!(
            ToolCallDensity::from_fraction(f32::NAN),
            ToolCallDensity::Grouped
        );
        for system in [false, true] {
            assert_eq!(MotionPreference::System.reduced(system), system);
            assert!(MotionPreference::Always.reduced(system));
            assert!(!MotionPreference::Never.reduced(system));
        }
    }

    #[test]
    fn current_font_snapshot_keeps_code_size_independent_from_ui_zoom() {
        let old = current();
        let preferences = AppearancePreferences {
            ui_font_size: 26,
            code_font_size: 18,
            code_word_wrap: true,
            themed_diff_backgrounds: false,
            hide_email: true,
            ..Default::default()
        };
        set_current(&preferences);
        assert_eq!(current(), preferences);
        assert_eq!(code_font_size(), 9.0);
        assert!(code_word_wrap());
        assert!(!themed_diff_backgrounds());
        assert!(hide_email());
        set_current(&old);
    }

    #[test]
    fn custom_library_recovers_valid_siblings_and_rejects_duplicate_ids_and_unknown_versions() {
        use crate::custom_theme::{
            CustomTheme, LIBRARY_VERSION, MAX_CUSTOM_THEMES, ThemeAppearance,
        };
        let theme = CustomTheme::new("Aurora", ThemeAppearance::Dark);
        let mut invalid = theme.clone();
        invalid.id = "invalid".into();
        invalid.colors.insert("base".into(), "broken".into());
        let value = serde_json::json!({
            "custom_themes_version": LIBRARY_VERSION,
            "custom_themes": [invalid, theme.clone(), theme.clone()],
            "active_custom_theme": theme.id,
            "confetti_cannon_enabled": true,
        });
        let recovered = AppearancePreferences::load_value(Some(&value), false);
        assert_eq!(recovered.custom_themes, vec![theme.clone()]);
        assert_eq!(
            recovered.active_custom_theme.as_deref(),
            Some(theme.id.as_str())
        );
        assert!(
            serde_json::to_value(&recovered)
                .unwrap()
                .get("confetti_cannon_enabled")
                .is_none()
        );
        assert!(recovered.validate().is_ok());
        let mut duplicate = recovered.clone();
        duplicate.custom_themes.push(theme);
        assert!(duplicate.validate().is_err());
        let mut value = value;
        value["custom_themes_version"] = serde_json::json!(LIBRARY_VERSION + 1);
        let unknown = AppearancePreferences::load_value(Some(&value), false);
        assert!(unknown.custom_themes.is_empty());
        assert_eq!(unknown.active_custom_theme, None);
        assert!(
            serde_json::to_value(&unknown)
                .unwrap()
                .get("confetti_cannon_enabled")
                .is_none()
        );
        let many = (0..MAX_CUSTOM_THEMES + 1)
            .map(|_| CustomTheme::new("Theme", ThemeAppearance::Dark))
            .collect::<Vec<_>>();
        let mut oversized = recovered;
        oversized.custom_themes = many;
        assert!(oversized.validate().is_err());
    }
}
