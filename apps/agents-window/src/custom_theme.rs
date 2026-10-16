//! Portable core themes. Only documented scalar colors are accepted: imports
//! never read `include`, syntax rules, URLs or files referenced by a theme.

use openide_ui::Color;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) const MAX_CUSTOM_THEMES: usize = 30;
pub(crate) const MAX_IMPORT_BYTES: usize = 1024 * 1024;
pub(crate) const LIBRARY_VERSION: u8 = 1;

pub(crate) const CORE_TOKENS: &[&str] = &[
    "sidebar",
    "chrome",
    "editor",
    "base",
    "accent",
    "actionLabel",
    "brand",
    "focus",
    "success",
    "warn",
    "danger",
    "added",
    "modified",
    "removed",
    "untracked",
    "terminalAnsiBlack",
    "terminalAnsiRed",
    "terminalAnsiYellow",
    "terminalAnsiGreen",
    "terminalAnsiCyan",
    "terminalAnsiBlue",
    "terminalAnsiMagenta",
    "terminalAnsiWhite",
    "terminalAnsiBrightBlack",
    "terminalAnsiBrightRed",
    "terminalAnsiBrightYellow",
    "terminalAnsiBrightGreen",
    "terminalAnsiBrightCyan",
    "terminalAnsiBrightBlue",
    "terminalAnsiBrightMagenta",
    "terminalAnsiBrightWhite",
    "red",
    "orange",
    "yellow",
    "green",
    "cyan",
    "blue",
    "magenta",
    "purple",
    "diffAddedLineBackground",
    "diffAddedTextBackground",
    "diffRemovedLineBackground",
    "diffRemovedTextBackground",
];

/// Paint commands currently share several source colors. Expose only groups
/// with a distinct native effect; keep the remaining portable tokens intact.
pub(crate) const EDITABLE_TOKENS: &[(&str, &str)] = &[
    (
        "sidebar",
        "Barras laterales, tarjetas y campos (comparten color)",
    ),
    ("chrome", "Paneles y menús"),
    ("editor", "Fondo de editores y espacio de trabajo"),
    ("base", "Texto y texto secundario derivado"),
    ("accent", "Botones principales"),
    ("actionLabel", "Texto de botones principales"),
    ("brand", "Iconos destacados"),
    ("focus", "Bordes y foco"),
    ("warn", "Advertencias"),
    ("danger", "Errores y acciones destructivas"),
    ("added", "Texto y marcas añadidas"),
    ("removed", "Texto y marcas eliminadas"),
    ("blue", "Enlaces y acciones azules"),
    ("purple", "Sintaxis de palabras clave y esfuerzo alto"),
    ("green", "Cadenas de código"),
    ("orange", "Números de código"),
    ("diffAddedLineBackground", "Fondo de líneas añadidas"),
    ("diffRemovedLineBackground", "Fondo de líneas eliminadas"),
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ThemeAppearance {
    Dark,
    Light,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CustomTheme {
    pub id: String,
    pub name: String,
    pub appearance: ThemeAppearance,
    pub high_contrast: bool,
    pub colors: BTreeMap<String, String>,
}

impl CustomTheme {
    pub fn new(name: &str, appearance: ThemeAppearance) -> Self {
        let mut theme = Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.trim().to_owned(),
            appearance,
            high_contrast: false,
            colors: BTreeMap::new(),
        };
        theme.fill_defaults();
        theme
    }

    fn fill_defaults(&mut self) {
        for token in CORE_TOKENS {
            self.colors.entry((*token).into()).or_insert_with(|| {
                format_color(default_color(token, self.appearance, self.high_contrast))
            });
        }
    }

    pub fn normalized(&self, token: &str) -> Color {
        self.colors
            .get(token)
            .and_then(|value| parse_color(value).ok())
            .unwrap_or_else(|| default_color(token, self.appearance, self.high_contrast))
    }

    pub fn set_color(&mut self, token: &str, color: Color) {
        if CORE_TOKENS.contains(&token) {
            self.colors.insert(token.into(), format_color(color));
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || self.id.len() > 80
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        {
            return Err("El identificador del tema no es válido.".into());
        }
        if self.name.trim().is_empty()
            || self.name.len() > 120
            || self.name.chars().any(char::is_control)
        {
            return Err(
                "El nombre del tema debe tener entre 1 y 120 bytes y no incluir controles.".into(),
            );
        }
        if self.colors.len() > CORE_TOKENS.len() {
            return Err("El tema contiene colores desconocidos.".into());
        }
        for (token, value) in &self.colors {
            if !CORE_TOKENS.contains(&token.as_str()) {
                return Err(format!(
                    "El color «{token}» no es un token de tema compatible."
                ));
            }
            if format_color(parse_color(value)?) != *value {
                return Err(format!("El color «{token}» no está normalizado."));
            }
        }
        Ok(())
    }

    /// YAML core themes and strict VS Code JSON are deliberately separate.
    /// A JSON-looking input cannot fall back to YAML and accidentally accept JSONC.
    pub fn import(text: &str) -> Result<Self, String> {
        if text.len() > MAX_IMPORT_BYTES {
            return Err("El archivo de tema supera el límite de 1 MiB.".into());
        }
        let text = text.trim_start_matches('\u{feff}').trim();
        if text.starts_with('{') || text.starts_with('[') {
            Self::import_vscode_json(text)
        } else {
            Self::import_yaml(text)
        }
    }

    fn import_yaml(text: &str) -> Result<Self, String> {
        let value: serde_yaml::Value = serde_yaml::from_str(text)
            .map_err(|error| format!("No se pudo leer el tema YAML: {error}"))?;
        let mapping = value.as_mapping().ok_or("El tema YAML debe ser un mapa.")?;
        let get = |key: &str| mapping.get(serde_yaml::Value::String(key.into()));
        let name = get("name")
            .and_then(serde_yaml::Value::as_str)
            .ok_or("El tema debe incluir un nombre.")?;
        let appearance_name = match get("appearance") {
            None => "dark",
            Some(value) => value
                .as_str()
                .ok_or("La apariencia del tema debe ser texto.")?,
        };
        let (appearance, inferred_contrast) = parse_appearance(appearance_name)?;
        let high_contrast = match get("highContrast") {
            None => inferred_contrast,
            Some(value) => value
                .as_bool()
                .ok_or("highContrast debe ser verdadero o falso.")?,
        };
        let mut theme = Self::new(name, appearance);
        theme.high_contrast = high_contrast;
        theme.colors.clear();
        for token in CORE_TOKENS {
            if let Some(value) = get(token) {
                let color = value
                    .as_str()
                    .ok_or_else(|| format!("El color «{token}» debe ser texto."))?;
                if !color.trim().is_empty() {
                    theme
                        .colors
                        .insert((*token).into(), format_color(parse_color(color)?));
                }
            }
        }
        theme.fill_defaults();
        theme.validate()?;
        Ok(theme)
    }

    fn import_vscode_json(text: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|error| format!("No se pudo leer el tema JSON: {error}"))?;
        let object = value
            .as_object()
            .ok_or("El tema JSON debe ser un objeto.")?;
        let name = match object.get("name") {
            None => "Tema importado",
            Some(value) => value.as_str().ok_or("El nombre del tema debe ser texto.")?,
        };
        let appearance_name = match object.get("type") {
            None => "dark",
            Some(value) => value.as_str().ok_or("El tipo de tema debe ser texto.")?,
        };
        let (appearance, high_contrast) = parse_appearance(appearance_name)?;
        let mut theme = Self::new(name, appearance);
        theme.high_contrast = high_contrast;
        theme.colors.clear();
        let colors = match object.get("colors") {
            None => None,
            Some(value) => Some(
                value
                    .as_object()
                    .ok_or("Los colores del tema JSON deben ser un objeto.")?,
            ),
        };
        for (token, key) in VSCODE_TOKENS {
            if let Some(value) = colors.and_then(|colors| colors.get(*key)) {
                let color = value
                    .as_str()
                    .ok_or_else(|| format!("El color «{key}» debe ser texto."))?;
                if !color.trim().is_empty() {
                    theme
                        .colors
                        .insert((*token).into(), format_color(parse_color(color)?));
                }
            }
        }
        theme.fill_defaults();
        theme.validate()?;
        Ok(theme)
    }

    pub fn export_yaml(&self) -> Result<String, String> {
        self.validate()?;
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert("name".into(), self.name.clone().into());
        mapping.insert(
            "appearance".into(),
            match self.appearance {
                ThemeAppearance::Dark => "dark",
                ThemeAppearance::Light => "light",
            }
            .into(),
        );
        mapping.insert("highContrast".into(), self.high_contrast.into());
        for token in CORE_TOKENS {
            mapping.insert((*token).into(), format_color(self.normalized(token)).into());
        }
        serde_yaml::to_string(&mapping)
            .map_err(|error| format!("No se pudo exportar el tema: {error}"))
    }

    /// Portable fields retained for export which currently have no independent
    /// native paint role. The UI displays this report when importing.
    pub fn report(&self) -> Vec<String> {
        CORE_TOKENS
            .iter()
            .filter(|token| {
                !EDITABLE_TOKENS
                    .iter()
                    .any(|(editable, _)| editable == *token)
            })
            .map(|token| (*token).to_owned())
            .collect()
    }
}

pub(crate) fn parse_color(value: &str) -> Result<Color, String> {
    if value.len() > 256 {
        return Err("El valor de color es demasiado largo.".into());
    }
    let parsed = color::parse_color(value.trim())
        .map_err(|_| format!("El color «{value}» no es válido."))?;
    if !parsed
        .components
        .iter()
        .all(|component| component.is_finite())
    {
        return Err("El color debe contener valores finitos.".into());
    }
    let rgba = parsed.to_alpha_color::<color::Srgb>().to_rgba8();
    Ok(Color::rgba(rgba.r, rgba.g, rgba.b, rgba.a))
}

pub(crate) fn format_color(color: Color) -> String {
    if color.a == 255 {
        format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
    } else {
        format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            color.r, color.g, color.b, color.a
        )
    }
}

pub(crate) fn default_color(
    token: &str,
    appearance: ThemeAppearance,
    high_contrast: bool,
) -> Color {
    // Cursor ships one dark high-contrast bucket. openADE additionally offers a
    // light high-contrast base, so derive readable native roles from its light
    // bucket rather than turning a user's explicit light selection dark.
    if high_contrast && appearance == ThemeAppearance::Light {
        let override_color = match token {
            "sidebar" | "chrome" | "editor" | "actionLabel" => Some("#ffffff"),
            "base" | "focus" => Some("#000000"),
            "accent" | "blue" | "cyan" => Some("#005a9c"),
            "brand" | "orange" => Some("#8a3000"),
            "success" | "added" | "green" => Some("#00682e"),
            "warn" | "modified" | "yellow" => Some("#6b4700"),
            "danger" | "removed" | "red" => Some("#a00000"),
            "purple" => Some("#5c258f"),
            "magenta" => Some("#8c246c"),
            _ => None,
        };
        if let Some(value) = override_color {
            return parse_color(value).expect("light high-contrast overrides are valid CSS colors");
        }
    }
    let bucket = if high_contrast && appearance == ThemeAppearance::Dark {
        HIGH_CONTRAST_DEFAULTS
    } else {
        match appearance {
            ThemeAppearance::Dark => DARK_DEFAULTS,
            ThemeAppearance::Light => LIGHT_DEFAULTS,
        }
    };
    let value = CORE_TOKENS
        .iter()
        .position(|known| *known == token)
        .and_then(|index| bucket.get(index))
        .copied()
        .unwrap_or("#000000");
    parse_color(value).expect("the bundled theme defaults are valid CSS colors")
}

fn parse_appearance(value: &str) -> Result<(ThemeAppearance, bool), String> {
    match value {
        "dark" | "vs-dark" => Ok((ThemeAppearance::Dark, false)),
        "light" | "vs" => Ok((ThemeAppearance::Light, false)),
        "hc" | "hc-black" => Ok((ThemeAppearance::Dark, true)),
        "hc-light" => Ok((ThemeAppearance::Light, true)),
        _ => Err("La apariencia del tema debe ser clara u oscura.".into()),
    }
}

const VSCODE_TOKENS: &[(&str, &str)] = &[
    ("sidebar", "sideBar.background"),
    ("chrome", "panel.background"),
    ("editor", "editor.background"),
    ("base", "editor.foreground"),
    ("accent", "button.background"),
    ("actionLabel", "button.foreground"),
    ("brand", "charts.blue"),
    ("focus", "focusBorder"),
    ("success", "terminal.ansiGreen"),
    ("warn", "list.warningForeground"),
    ("danger", "errorForeground"),
    ("added", "gitDecoration.addedResourceForeground"),
    ("modified", "gitDecoration.modifiedResourceForeground"),
    ("removed", "gitDecoration.deletedResourceForeground"),
    ("untracked", "gitDecoration.untrackedResourceForeground"),
    ("terminalAnsiBlack", "terminal.ansiBlack"),
    ("terminalAnsiRed", "terminal.ansiRed"),
    ("terminalAnsiYellow", "terminal.ansiYellow"),
    ("terminalAnsiGreen", "terminal.ansiGreen"),
    ("terminalAnsiCyan", "terminal.ansiCyan"),
    ("terminalAnsiBlue", "terminal.ansiBlue"),
    ("terminalAnsiMagenta", "terminal.ansiMagenta"),
    ("terminalAnsiWhite", "terminal.ansiWhite"),
    ("terminalAnsiBrightBlack", "terminal.ansiBrightBlack"),
    ("terminalAnsiBrightRed", "terminal.ansiBrightRed"),
    ("terminalAnsiBrightYellow", "terminal.ansiBrightYellow"),
    ("terminalAnsiBrightGreen", "terminal.ansiBrightGreen"),
    ("terminalAnsiBrightCyan", "terminal.ansiBrightCyan"),
    ("terminalAnsiBrightBlue", "terminal.ansiBrightBlue"),
    ("terminalAnsiBrightMagenta", "terminal.ansiBrightMagenta"),
    ("terminalAnsiBrightWhite", "terminal.ansiBrightWhite"),
    ("red", "charts.red"),
    ("orange", "charts.orange"),
    ("yellow", "charts.yellow"),
    ("green", "charts.green"),
    ("purple", "charts.purple"),
    ("cyan", "terminal.ansiCyan"),
    ("blue", "terminal.ansiBlue"),
    ("magenta", "terminal.ansiMagenta"),
    (
        "diffAddedLineBackground",
        "diffEditor.insertedLineBackground",
    ),
    (
        "diffAddedTextBackground",
        "diffEditor.insertedTextBackground",
    ),
    (
        "diffRemovedLineBackground",
        "diffEditor.removedLineBackground",
    ),
    (
        "diffRemovedTextBackground",
        "diffEditor.removedTextBackground",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_yaml_normalizes_css_colors_and_round_trips_alpha_and_all_tokens() {
        let theme = CustomTheme::import(
            "name: Aurora\nappearance: dark\naccent: 'rgb(255 0 0 / 50%)'\nbase: 'oklch(1 0 0)'\n",
        )
        .unwrap();
        assert_eq!(theme.name, "Aurora");
        assert_eq!(theme.colors.len(), CORE_TOKENS.len());
        assert_eq!(theme.colors["accent"], "#ff000080");
        assert_eq!(theme.colors["base"], "#ffffff");
        let restored = CustomTheme::import(&theme.export_yaml().unwrap()).unwrap();
        assert_ne!(
            restored.id, theme.id,
            "an import creates a separate library entry"
        );
        assert_eq!(restored.colors, theme.colors);
        assert_eq!(restored.appearance, theme.appearance);
        assert_eq!(restored.high_contrast, theme.high_contrast);
    }

    #[test]
    fn vscode_json_maps_only_documented_colors_and_never_resolves_include() {
        let theme = CustomTheme::import(r##"{"name":"VS Theme","type":"hc-light","include":"https://invalid.example/theme.json","colors":{"editor.background":"#12345678","button.background":"rebeccapurple","terminal.ansiRed":"#f00"},"tokenColors":[{"settings":{"foreground":"#bad"}}]}"##).unwrap();
        assert_eq!(theme.appearance, ThemeAppearance::Light);
        assert!(theme.high_contrast);
        assert_eq!(theme.colors["editor"], "#12345678");
        assert_eq!(theme.colors["accent"], "#663399");
        assert_eq!(theme.colors["terminalAnsiRed"], "#ff0000");
        assert!(
            theme
                .report()
                .iter()
                .any(|token| token == "terminalAnsiRed")
        );
        assert!(CustomTheme::import("{ // JSONC\n\"name\":\"Bad\" }").is_err());
        let empty = CustomTheme::import(r#"{"name":"Defaults"}"#).unwrap();
        assert_eq!(empty.colors.len(), CORE_TOKENS.len());
    }

    #[test]
    fn invalid_names_colors_shapes_and_oversized_inputs_are_rejected() {
        for input in [
            "name: '  '\n",
            "[]",
            "name: Bad\nbase: definitely-not-a-color\n",
            "name: Bad\nappearance: sepia\n",
            "name: Bad\nhighContrast: yes\n",
            "name: Bad\nbase: 123\n",
            "name: Bad\nappearance: 123\n",
            r#"{"name":"Bad","colors":[]}"#,
            r#"{"name":"Bad","colors":{"editor.foreground":123}}"#,
        ] {
            assert!(CustomTheme::import(input).is_err(), "{input}");
        }
        assert!(CustomTheme::import(&" ".repeat(MAX_IMPORT_BYTES + 1)).is_err());
        let mut theme = CustomTheme::new("Valid", ThemeAppearance::Dark);
        theme.colors.insert("unexpected".into(), "#ffffff".into());
        assert!(theme.validate().is_err());
    }

    #[test]
    fn editable_tokens_are_known_and_color_helpers_preserve_alpha() {
        let mut theme = CustomTheme::new("Native", ThemeAppearance::Light);
        for (token, _) in EDITABLE_TOKENS {
            assert!(CORE_TOKENS.contains(token), "{token}");
            let color = Color::rgba(23, 45, 67, 89);
            theme.set_color(token, color);
            assert_eq!(theme.normalized(token), color);
            assert_eq!(parse_color(&format_color(color)).unwrap(), color);
        }
        assert!(theme.validate().is_ok());
    }

    #[test]
    fn light_high_contrast_stays_light_and_default_text_pairs_meet_wcag_aa() {
        fn luminance(color: Color) -> f32 {
            let channel = |value: u8| {
                let value = f32::from(value) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
        }
        let color = |token| default_color(token, ThemeAppearance::Light, true);
        let contrast = |first, second| {
            let first = luminance(first);
            let second = luminance(second);
            (first.max(second) + 0.05) / (first.min(second) + 0.05)
        };
        for surface in ["editor", "sidebar", "chrome"] {
            assert_eq!(color(surface), Color::rgb(255, 255, 255));
            for ink in [
                "base", "focus", "brand", "warn", "danger", "added", "removed", "blue", "purple",
                "green", "orange",
            ] {
                assert!(
                    contrast(color(ink), color(surface)) >= 4.5,
                    "{ink} on {surface}"
                );
            }
        }
        assert!(contrast(color("actionLabel"), color("accent")) >= 4.5);
    }
}

// Core fallback buckets recovered from Cursor 3.17.21 shipped Glass themes.

const DARK_DEFAULTS: &[&str] = &[
    "oklch(0.209 0 0)",                   // sidebar
    "oklch(0.191 0 0)",                   // chrome
    "oklch(0.209 0 0)",                   // editor
    "oklch(0.955 0 0)",                   // base
    "oklch(0.681 0.131 252.629)",         // accent
    "oklch(0.226 0.013 264.284)",         // actionLabel
    "oklch(0.652 0.214 37.99)",           // brand
    "oklch(0.955 0 0)",                   // focus
    "oklch(0.638 0.129 153.739)",         // success
    "oklch(0.811 0.118 71.255)",          // warn
    "oklch(0.628 0.194 8.387)",           // danger
    "oklch(0.713 0.094 155.438)",         // added
    "oklch(0.811 0.118 71.255)",          // modified
    "oklch(0.712 0.177 12.651)",          // removed
    "oklch(0.775 0.062 217.469)",         // untracked
    "oklch(0.26 0 0)",                    // terminalAnsiBlack
    "oklch(0.712 0.177 12.651)",          // terminalAnsiRed
    "oklch(0.712 0.125 71.923)",          // terminalAnsiYellow
    "oklch(0.638 0.129 153.739)",         // terminalAnsiGreen
    "oklch(0.775 0.062 217.469)",         // terminalAnsiCyan
    "oklch(0.697 0.059 248.687)",         // terminalAnsiBlue
    "oklch(0.692 0.062 332.665)",         // terminalAnsiMagenta
    "oklch(0.955 0 0)",                   // terminalAnsiWhite
    "oklch(0.955 0 0 / 0.6)",             // terminalAnsiBrightBlack
    "oklch(0.712 0.177 12.651)",          // terminalAnsiBrightRed
    "oklch(0.811 0.118 71.255)",          // terminalAnsiBrightYellow
    "oklch(0.713 0.094 155.438)",         // terminalAnsiBrightGreen
    "oklch(0.775 0.062 217.469)",         // terminalAnsiBrightCyan
    "oklch(0.713 0.056 247.764)",         // terminalAnsiBrightBlue
    "oklch(0.692 0.062 332.665)",         // terminalAnsiBrightMagenta
    "oklch(0.955 0 0)",                   // terminalAnsiBrightWhite
    "oklch(0.712 0.177 12.651)",          // red
    "oklch(0.693 0.096 38.241)",          // orange
    "oklch(0.811 0.118 71.255)",          // yellow
    "oklch(0.638 0.129 153.739)",         // green
    "oklch(0.697 0.059 248.687)",         // cyan
    "oklch(0.74 0.101 251.884)",          // blue
    "oklch(0.692 0.062 332.665)",         // magenta
    "oklch(0.679 0.156 286.998)",         // purple
    "oklch(0.638 0.129 153.739 / 0.2)",   // diffAddedLineBackground
    "oklch(0.638 0.129 153.739 / 0.133)", // diffAddedTextBackground
    "oklch(0.5 0.2 9.842 / 0.2)",         // diffRemovedLineBackground
    "oklch(0.5 0.2 9.842 / 0.133)",       // diffRemovedTextBackground
];

const HIGH_CONTRAST_DEFAULTS: &[&str] = &[
    "oklch(0.145 0 0)",                   // sidebar
    "oklch(0.145 0 0)",                   // chrome
    "oklch(0.145 0 0)",                   // editor
    "oklch(0.955 0 0)",                   // base
    "oklch(0.758 0.129 249.583)",         // accent
    "oklch(0.226 0.013 264.284)",         // actionLabel
    "oklch(0.652 0.214 37.99)",           // brand
    "oklch(0.865 0.177 90.382)",          // focus
    "oklch(0.767 0.161 152.813)",         // success
    "oklch(0.894 0.139 90.467)",          // warn
    "oklch(0.712 0.181 22.839)",          // danger
    "oklch(0.767 0.161 152.813)",         // added
    "oklch(0.894 0.139 90.467)",          // modified
    "oklch(0.712 0.181 22.839)",          // removed
    "oklch(0.797 0.112 186.504)",         // untracked
    "oklch(0.348 0 0)",                   // terminalAnsiBlack
    "oklch(0.712 0.181 22.839)",          // terminalAnsiRed
    "oklch(0.894 0.139 90.467)",          // terminalAnsiYellow
    "oklch(0.767 0.161 152.813)",         // terminalAnsiGreen
    "oklch(0.797 0.112 186.504)",         // terminalAnsiCyan
    "oklch(0.758 0.129 249.583)",         // terminalAnsiBlue
    "oklch(0.778 0.101 326.494)",         // terminalAnsiMagenta
    "oklch(0.955 0 0)",                   // terminalAnsiWhite
    "oklch(0.545 0 0)",                   // terminalAnsiBrightBlack
    "oklch(0.761 0.142 20.941)",          // terminalAnsiBrightRed
    "oklch(0.894 0.139 90.467)",          // terminalAnsiBrightYellow
    "oklch(0.767 0.161 152.813)",         // terminalAnsiBrightGreen
    "oklch(0.848 0.102 186.84)",          // terminalAnsiBrightCyan
    "oklch(0.821 0.097 243.759)",         // terminalAnsiBrightBlue
    "oklch(0.778 0.101 326.494)",         // terminalAnsiBrightMagenta
    "oklch(0.955 0 0)",                   // terminalAnsiBrightWhite
    "oklch(0.712 0.181 22.839)",          // red
    "oklch(0.82 0.123 58.928)",           // orange
    "oklch(0.894 0.139 90.467)",          // yellow
    "oklch(0.767 0.161 152.813)",         // green
    "oklch(0.758 0.129 249.583)",         // cyan
    "oklch(0.74 0.101 251.884)",          // blue
    "oklch(0.778 0.101 326.494)",         // magenta
    "oklch(0.679 0.156 286.998)",         // purple
    "oklch(0.768 0.075 131.063 / 0.2)",   // diffAddedLineBackground
    "oklch(0.768 0.075 131.063 / 0.133)", // diffAddedTextBackground
    "oklch(0.606 0.121 15.342 / 0.2)",    // diffRemovedLineBackground
    "oklch(0.606 0.121 15.342 / 0.133)",  // diffRemovedTextBackground
];

const LIGHT_DEFAULTS: &[&str] = &[
    "oklch(0.964 0 0)",                   // sidebar
    "oklch(0.979 0 0)",                   // chrome
    "oklch(0.991 0 0)",                   // editor
    "oklch(0.191 0 0)",                   // base
    "oklch(0.561 0.137 249.943)",         // accent
    "oklch(0.991 0 0)",                   // actionLabel
    "oklch(0.652 0.214 37.99)",           // brand
    "oklch(0.561 0.137 249.943)",         // focus
    "oklch(0.479 0.116 156.327)",         // success
    "oklch(0.567 0.123 69.543)",          // warn
    "oklch(0.518 0.196 14.313)",          // danger
    "oklch(0.479 0.116 156.327)",         // added
    "oklch(0.567 0.123 69.543)",          // modified
    "oklch(0.518 0.196 14.313)",          // removed
    "oklch(0.487 0.077 204.607)",         // untracked
    "oklch(0.191 0 0)",                   // terminalAnsiBlack
    "oklch(0.518 0.196 14.313)",          // terminalAnsiRed
    "oklch(0.503 0.109 69.936)",          // terminalAnsiYellow
    "oklch(0.479 0.116 156.327)",         // terminalAnsiGreen
    "oklch(0.487 0.077 204.607)",         // terminalAnsiCyan
    "oklch(0.497 0.143 250.703)",         // terminalAnsiBlue
    "oklch(0.449 0.176 345.345)",         // terminalAnsiMagenta
    "oklch(0.991 0 0)",                   // terminalAnsiWhite
    "oklch(0.191 0 0 / 0.741)",           // terminalAnsiBrightBlack
    "oklch(0.582 0.178 13.604)",          // terminalAnsiBrightRed
    "oklch(0.567 0.123 69.543)",          // terminalAnsiBrightYellow
    "oklch(0.542 0.133 155.45)",          // terminalAnsiBrightGreen
    "oklch(0.552 0.068 203.209)",         // terminalAnsiBrightCyan
    "oklch(0.561 0.137 249.943)",         // terminalAnsiBrightBlue
    "oklch(0.577 0.154 343.697)",         // terminalAnsiBrightMagenta
    "oklch(1 0 0)",                       // terminalAnsiBrightWhite
    "oklch(0.518 0.196 14.313)",          // red
    "oklch(0.576 0.183 39.534)",          // orange
    "oklch(0.567 0.123 69.543)",          // yellow
    "oklch(0.479 0.116 156.327)",         // green
    "oklch(0.487 0.077 204.607)",         // cyan
    "oklch(0.561 0.137 249.943)",         // blue
    "oklch(0.449 0.176 345.345)",         // magenta
    "oklch(0.572 0.154 287.656)",         // purple
    "oklch(0.663 0.162 155.537 / 0.141)", // diffAddedLineBackground
    "oklch(0.666 0.162 155.968 / 0.22)",  // diffAddedTextBackground
    "oklch(0.703 0.192 13.697 / 0.22)",   // diffRemovedLineBackground
    "oklch(0.703 0.192 13.697 / 0.341)",  // diffRemovedTextBackground
];
