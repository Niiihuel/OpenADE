//! Explicit UI font weight, shared by egui's measurement and painting paths.

use eframe::egui;
use std::sync::OnceLock;

fn database() -> &'static fontdb::Database {
    static DATABASE: OnceLock<fontdb::Database> = OnceLock::new();
    DATABASE.get_or_init(|| {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        database
    })
}

pub(crate) fn families() -> &'static [String] {
    static FAMILIES: OnceLock<Vec<String>> = OnceLock::new();
    FAMILIES.get_or_init(|| {
        let mut names: Vec<_> = database()
            .faces()
            .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    })
}

pub(crate) fn families_for(kind: &str) -> &'static [String] {
    if kind != "code" {
        return families();
    }
    static MONOSPACE: OnceLock<Vec<String>> = OnceLock::new();
    MONOSPACE.get_or_init(|| {
        let mut names: Vec<_> = database()
            .faces()
            .filter(|face| face.monospaced)
            .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    })
}

/// Share the same font definitions with limn's measurement and paint paths.
/// Missing saved families retain the bundled fallback when moving machines.
/// Returns true once a requested font or rasterization change is active in a
/// later pass, so both limn trees can refresh their measurements together.
pub(crate) fn update(
    context: &egui::Context,
    preferences: &crate::appearance_preferences::AppearancePreferences,
) -> bool {
    let key = egui::Id::new("openade.appearance.fonts");
    #[derive(Clone)]
    struct Selection {
        families: (String, String),
        requested_pass: u64,
        attached: bool,
    }
    let selected = (
        preferences.ui_font_family.clone(),
        preferences.code_font_family.clone(),
    );
    let smoothing_changed = [egui::Theme::Dark, egui::Theme::Light]
        .into_iter()
        .any(|theme| {
            context
                .style_of(theme)
                .visuals
                .text_options
                .subpixel_binning
                != preferences.font_smoothing
        });
    let mut families_changed = true;
    if let Some(mut installed) = context.data(|data| data.get_temp::<Selection>(key)) {
        families_changed = installed.families != selected;
        if !families_changed && !smoothing_changed {
            if !installed.attached && context.cumulative_pass_nr() > installed.requested_pass {
                installed.attached = true;
                context.data_mut(|data| data.insert_temp(key, installed));
                return true;
            }
            return false;
        }
    }
    if families_changed {
        context.set_fonts(selected_definitions(&selected));
    }
    if smoothing_changed {
        // Fractional glyph placement is smoother; retain hinting and theme gamma
        // so this preference never changes the font face, weight or advance widths.
        context.all_styles_mut(|style| {
            style.visuals.text_options.subpixel_binning = preferences.font_smoothing;
        });
    }
    let requested_pass = context.cumulative_pass_nr();
    context.data_mut(|data| {
        data.insert_temp(
            key,
            Selection {
                families: selected,
                requested_pass,
                attached: false,
            },
        )
    });
    context.request_repaint();
    false
}

fn selected_definitions(selected: &(String, String)) -> egui::FontDefinitions {
    let mut definitions = limn_egui::font_definitions();
    for (name, family, weight) in [
        (&selected.0, egui::FontFamily::Proportional, 400),
        (
            &selected.0,
            egui::FontFamily::Name("limn-sans-medium".into()),
            500,
        ),
        (
            &selected.0,
            egui::FontFamily::Name("limn-sans-bold".into()),
            700,
        ),
        (&selected.1, egui::FontFamily::Monospace, 400),
    ] {
        if name.is_empty() {
            continue;
        }
        let Some(id) = database().query(&fontdb::Query {
            families: &[fontdb::Family::Name(name)],
            weight: fontdb::Weight(weight),
            ..Default::default()
        }) else {
            continue;
        };
        // Saved preferences can bypass the monospace-only code font picker.
        if family == egui::FontFamily::Monospace
            && !database().face(id).is_some_and(|face| face.monospaced)
        {
            continue;
        }
        if let Some((bytes, index)) =
            database().with_face_data(id, |bytes, index| (bytes.to_vec(), index))
        {
            let key = format!("openade:{name}:{weight}");
            let mut font = egui::FontData::from_owned(bytes);
            font.index = index;
            definitions.font_data.insert(key.clone(), font.into());
            definitions
                .families
                .entry(family)
                .or_default()
                .insert(0, key);
        }
    }
    definitions
}

/// Mask only presentation. Account identifiers and authentication stay exact.
pub(crate) fn account_label(value: &str) -> String {
    if !crate::appearance_preferences::hide_email() {
        return value.to_owned();
    }
    let Some((local, domain)) = value
        .split_once('@')
        .filter(|(local, domain)| !local.is_empty() && !domain.is_empty())
    else {
        return value.to_owned();
    };
    format!("{}***@{}", local.chars().next().unwrap(), domain)
}

pub fn install(context: &egui::Context) {
    limn_egui::install_fonts(context);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed_family(monospaced: bool) -> Option<String> {
        families()
            .iter()
            .find(|name| {
                database()
                    .query(&fontdb::Query {
                        families: &[fontdb::Family::Name(name)],
                        weight: fontdb::Weight(400),
                        ..Default::default()
                    })
                    .and_then(|id| database().face(id))
                    .is_some_and(|face| face.monospaced == monospaced)
            })
            .cloned()
    }

    #[test]
    fn saved_proportional_code_family_keeps_mono_fallback_but_still_works_as_ui_font() {
        let fallback = limn_egui::font_definitions();
        let missing = selected_definitions(&(String::new(), "openADE missing font fixture".into()));
        assert_eq!(
            missing.families[&egui::FontFamily::Monospace],
            fallback.families[&egui::FontFamily::Monospace]
        );
        if let Some(name) = installed_family(false) {
            let selected = (name.clone(), name.clone());
            let definitions = selected_definitions(&selected);
            assert_eq!(
                definitions.families[&egui::FontFamily::Monospace],
                fallback.families[&egui::FontFamily::Monospace]
            );
            assert_eq!(
                definitions.families[&egui::FontFamily::Proportional][0],
                format!("openade:{name}:400")
            );
        }
    }

    #[test]
    fn installed_monospace_code_family_is_first_and_retains_bundled_fallbacks() {
        let fallback = limn_egui::font_definitions();
        let name = installed_family(true).unwrap_or_default();
        let definitions = selected_definitions(&(String::new(), name.clone()));
        assert_eq!(
            definitions.families[&egui::FontFamily::Proportional],
            fallback.families[&egui::FontFamily::Proportional]
        );
        let mono = &definitions.families[&egui::FontFamily::Monospace];
        if name.is_empty() {
            assert_eq!(mono, &fallback.families[&egui::FontFamily::Monospace]);
        } else {
            assert_eq!(mono[0], format!("openade:{name}:400"));
            assert_eq!(
                &mono[1..],
                fallback.families[&egui::FontFamily::Monospace].as_slice()
            );
        }
    }

    #[test]
    fn font_metrics_refresh_after_the_pass_that_installs_the_fonts() {
        let ctx = egui::Context::default();
        let preferences = crate::appearance_preferences::AppearancePreferences::default();
        ctx.begin_pass(Default::default());
        assert!(!update(&ctx, &preferences));
        assert!(!update(&ctx, &preferences));
        ctx.end_pass().textures_delta.clear();
        ctx.begin_pass(Default::default());
        assert!(update(&ctx, &preferences));
        assert!(!update(&ctx, &preferences));
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn smoothing_changes_the_active_rasterizer_on_the_next_pass_and_refreshes_once() {
        let ctx = egui::Context::default();
        let mut preferences = crate::appearance_preferences::AppearancePreferences::default();
        ctx.begin_pass(Default::default());
        assert!(!update(&ctx, &preferences));
        ctx.end_pass().textures_delta.clear();
        ctx.begin_pass(Default::default());
        assert!(update(&ctx, &preferences));
        let definitions = ctx.fonts(|fonts| fonts.definitions().clone());
        let options = ctx.fonts(|fonts| *fonts.options());
        let metrics = |ctx: &egui::Context| {
            ctx.fonts_mut(|fonts| {
                [
                    egui::FontId::proportional(14.0),
                    egui::FontId::monospace(14.0),
                ]
                .map(|font| (fonts.row_height(&font), fonts.glyph_width(&font, 'M')))
            })
        };
        let original_metrics = metrics(&ctx);
        assert!(options.subpixel_binning);
        for enabled in [false, true] {
            preferences.font_smoothing = enabled;
            assert!(!update(&ctx, &preferences));
            assert!(!update(&ctx, &preferences));
            assert_eq!(
                ctx.fonts(|fonts| fonts.options().subpixel_binning),
                !enabled,
                "the current pass keeps the active atlas until the next begin_pass"
            );
            ctx.end_pass().textures_delta.clear();
            ctx.begin_pass(Default::default());
            assert_eq!(ctx.fonts(|fonts| fonts.options().subpixel_binning), enabled);
            assert!(
                update(&ctx, &preferences),
                "reattach measurements once the new atlas is active"
            );
            assert!(!update(&ctx, &preferences));
            ctx.fonts(|fonts| {
                assert_eq!(fonts.options().font_hinting, options.font_hinting);
                assert_eq!(
                    fonts.options().color_transfer_function,
                    options.color_transfer_function
                );
                assert_eq!(fonts.definitions().families, definitions.families);
                for (name, face) in &definitions.font_data {
                    assert!(
                        std::sync::Arc::ptr_eq(face, &fonts.definitions().font_data[name]),
                        "smoothing must not reinstall or replace font face {name}"
                    );
                }
            });
            assert_eq!(metrics(&ctx), original_metrics);
        }
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn smoothing_preserves_theme_gamma_hinting_and_requests_refresh_after_external_style_changes() {
        let ctx = egui::Context::default();
        let preferences = crate::appearance_preferences::AppearancePreferences {
            font_smoothing: false,
            ..Default::default()
        };
        let baseline = [egui::Theme::Dark, egui::Theme::Light]
            .map(|theme| (theme, ctx.style_of(theme).visuals.text_options));
        ctx.begin_pass(Default::default());
        assert!(!update(&ctx, &preferences));
        for (theme, original) in baseline {
            let options = ctx.style_of(theme).visuals.text_options;
            assert!(!options.subpixel_binning);
            assert_eq!(options.font_hinting, original.font_hinting);
            assert_eq!(
                options.color_transfer_function,
                original.color_transfer_function
            );
        }
        ctx.end_pass().textures_delta.clear();
        ctx.begin_pass(Default::default());
        assert!(update(&ctx, &preferences));
        ctx.set_visuals(egui::Visuals::default());
        assert!(!update(&ctx, &preferences));
        assert!(!update(&ctx, &preferences));
        ctx.end_pass().textures_delta.clear();
        ctx.begin_pass(Default::default());
        assert!(!ctx.fonts(|fonts| fonts.options().subpixel_binning));
        assert!(update(&ctx, &preferences));
        assert!(!update(&ctx, &preferences));
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn privacy_masks_displayed_emails_without_changing_account_ids() {
        let mut preferences = crate::appearance_preferences::AppearancePreferences::default();
        preferences.hide_email = true;
        crate::appearance_preferences::set_current(&preferences);
        assert_eq!(account_label("álice@example.com"), "á***@example.com");
        assert_eq!(account_label("github-user"), "github-user");
        assert_eq!(account_label("@invalid"), "@invalid");
        crate::appearance_preferences::set_current(&Default::default());
        assert_eq!(account_label("alice@example.com"), "alice@example.com");
    }
}
