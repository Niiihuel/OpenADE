//! Preference actions stay transactional: previews never write settings to disk.
use super::*;
use crate::appearance_preferences::{
    self, CODE_FONT_MAX, CODE_FONT_MIN, MotionPreference, ToolCallDensity, UI_FONT_MAX, UI_FONT_MIN,
};

impl AgentsWindow {
    pub(super) fn sync_appearance(&mut self, ctx: &egui::Context) {
        let sizes_visible =
            self.view.show_settings && self.view.settings_page == SettingsPage::Appearance;
        if !sizes_visible && self.view.appearance != self.settings_preferences.appearance {
            self.view.appearance = self.settings_preferences.appearance.clone();
            appearance_preferences::set_current(&self.view.appearance);
            crate::theme_mode::set_appearance(&self.view.appearance);
        }
        if !sizes_visible {
            self.tree.set_input_value(
                "appearance_size:ui",
                self.view.appearance.ui_font_size.to_string(),
            );
            self.tree.set_input_value(
                "appearance_size:code",
                self.view.appearance.code_font_size.to_string(),
            );
        }
        let p = &self.view.appearance;
        let zoom = p.ui_font_size as f32 / 13.0;
        if (ctx.zoom_factor() - zoom).abs() > 0.001 {
            ctx.set_zoom_factor(zoom);
        }
        if crate::fonts::update(ctx, p) {
            limn_egui::attach_fonts(&mut self.tree, ctx);
            limn_egui::attach_fonts(&mut self.command_tree, ctx);
        }
        let reduced = p.reduce_motion.reduced(system_reduced_motion(ctx));
        if self.reduced_motion != reduced {
            self.reduced_motion = reduced;
            self.view.settings_reduced_motion = reduced;
            self.tree.set_reduced_motion(reduced);
            self.command_tree.set_reduced_motion(reduced);
        }
    }

    pub(super) fn apply_appearance_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        if self.apply_custom_theme_action(action, ctx) {
            return true;
        }
        let id = match action {
            UiAction::Clicked { id } | UiAction::SliderChanged { id, .. }
                if id.starts_with("appearance_") =>
            {
                id
            }
            UiAction::InputChanged { id, .. } | UiAction::Submitted { id, .. }
                if matches!(
                    id.as_str(),
                    "appearance_size:ui"
                        | "appearance_size:code"
                        | "appearance_font_query:ui"
                        | "appearance_font_query:code"
                ) =>
            {
                id
            }
            _ => return false,
        };
        if !self.view.show_settings || self.view.settings_page != SettingsPage::Appearance {
            return true;
        }
        for (index, kind) in ["ui", "code"].into_iter().enumerate() {
            let query_id = format!("appearance_font_query:{kind}");
            if id == &query_id {
                if let UiAction::InputChanged { value, .. } = action {
                    self.view.appearance_font_query[index] = value.clone();
                }
                return true;
            }
            if id == &format!("{query_id}:clear") {
                self.view.appearance_font_query[index].clear();
                self.tree.set_input_value(&query_id, "");
                self.tree.focus(&query_id);
                return true;
            }
        }
        let mut next = self.settings_preferences.clone();
        let p = &mut next.appearance;
        let mut commit = true;
        let mut sync_size_input = None;
        match action {
            UiAction::InputChanged { value, .. } | UiAction::Submitted { value, .. } => {
                let (size, min, max) = match id.as_str() {
                    "appearance_size:ui" => (&mut p.ui_font_size, UI_FONT_MIN, UI_FONT_MAX),
                    "appearance_size:code" => (&mut p.code_font_size, CODE_FONT_MIN, CODE_FONT_MAX),
                    _ => return true,
                };
                let Some(value) = value
                    .trim()
                    .parse::<u8>()
                    .ok()
                    .filter(|value| (min..=max).contains(value))
                else {
                    if matches!(action, UiAction::Submitted { .. }) {
                        self.tree.set_input_value(id, size.to_string());
                    }
                    return true;
                };
                *size = value;
                if matches!(action, UiAction::Submitted { .. }) {
                    sync_size_input = Some(id.clone());
                }
            }
            UiAction::SliderChanged {
                fraction,
                committed,
                ..
            } if fraction.is_finite() => {
                commit = *committed;
                match id.as_str() {
                    "appearance_density" => {
                        p.tool_call_density = ToolCallDensity::from_fraction(*fraction)
                    }
                    "appearance_hue" => {
                        p.hue_degrees = (fraction.clamp(0.0, 1.0) * 360.0).round() as u16
                    }
                    "appearance_intensity" => {
                        p.tint_intensity = (fraction.clamp(0.0, 1.0) * 1000.0).round() as u16
                    }
                    _ => return true,
                }
            }
            UiAction::Clicked { .. } => match id.as_str() {
                "appearance_wrap" => p.code_word_wrap = !p.code_word_wrap,
                "appearance_diff" => p.themed_diff_backgrounds = !p.themed_diff_backgrounds,
                "appearance_transparency" => p.reduce_transparency = !p.reduce_transparency,
                "appearance_email" => p.hide_email = !p.hide_email,
                "appearance_smoothing" => p.font_smoothing = !p.font_smoothing,
                _ if id.starts_with("appearance_motion:") => {
                    p.reduce_motion = match id.trim_start_matches("appearance_motion:") {
                        "system" => MotionPreference::System,
                        "always" => MotionPreference::Always,
                        "never" => MotionPreference::Never,
                        _ => return true,
                    };
                }
                _ if id.starts_with("appearance_size:") => {
                    let Some((kind, direction)) =
                        id.trim_start_matches("appearance_size:").split_once(':')
                    else {
                        return true;
                    };
                    let (value, min, max, default) = match kind {
                        "ui" => (&mut p.ui_font_size, UI_FONT_MIN, UI_FONT_MAX, 13),
                        "code" => (&mut p.code_font_size, CODE_FONT_MIN, CODE_FONT_MAX, 12),
                        _ => return true,
                    };
                    let field = format!("appearance_size:{kind}");
                    *value = if direction == "reset" {
                        default
                    } else {
                        // Bounds and accessible labels also use the last valid size.
                        let current = value.to_string();
                        let Some(adjusted) = openide_ui::NumberInput::adjusted_value(
                            id,
                            &field,
                            &current,
                            Some(f64::from(min)),
                            Some(f64::from(max)),
                            1.0,
                            0,
                        )
                        .and_then(|value| value.parse().ok()) else {
                            return true;
                        };
                        adjusted
                    };
                    sync_size_input = Some(field);
                }
                _ if id.starts_with("appearance_family:") => {
                    let Some((kind, index)) =
                        id.trim_start_matches("appearance_family:").split_once(':')
                    else {
                        return true;
                    };
                    let family = if index == "default" {
                        String::new()
                    } else {
                        let Some(family) = index
                            .parse::<usize>()
                            .ok()
                            .and_then(|index| crate::fonts::families_for(kind).get(index))
                        else {
                            return true;
                        };
                        family.clone()
                    };
                    match kind {
                        "ui" => p.ui_font_family = family,
                        "code" => p.code_font_family = family,
                        _ => return true,
                    }
                }
                _ => return true,
            },
            _ => return true,
        }
        if commit {
            next.reduced_motion = p.reduce_motion.reduced(system_reduced_motion(ctx));
            if !self.commit_settings_preferences(next, "Preferencias de apariencia guardadas") {
                self.view.appearance = self.settings_preferences.appearance.clone();
                if matches!(action, UiAction::InputChanged { .. }) {
                    sync_size_input = Some(id.clone());
                }
            }
            self.view.settings_choice_menu_open = None;
        } else {
            self.view.appearance = p.clone();
        }
        if let Some(id) = sync_size_input {
            let value = if id == "appearance_size:ui" {
                self.view.appearance.ui_font_size
            } else {
                self.view.appearance.code_font_size
            };
            self.tree.set_input_value(&id, value.to_string());
        }
        appearance_preferences::set_current(&self.view.appearance);
        crate::theme_mode::set_appearance(&self.view.appearance);
        self.sync_appearance(ctx);
        true
    }
}

fn system_reduced_motion(_ctx: &egui::Context) -> bool {
    use std::sync::{
        Once,
        atomic::{AtomicBool, Ordering},
    };
    static REDUCED: AtomicBool = AtomicBool::new(false);
    static START: Once = Once::new();
    START.call_once(|| {
        REDUCED.store(
            std::env::var("OPENIDE_REDUCED_MOTION").is_ok_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes"
                )
            }),
            Ordering::Relaxed,
        );
        #[cfg(all(target_os = "linux", not(test)))]
        let context = _ctx.clone();
        #[cfg(all(target_os = "linux", not(test)))]
        let _ = std::thread::Builder::new()
            .name("desktop-motion".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                loop {
                    if let Ok(Ok(motion)) = runtime.block_on(async {
                        tokio::time::timeout(Duration::from_secs(2), async {
                            ashpd::desktop::settings::Settings::new()
                                .await?
                                .reduced_motion()
                                .await
                        })
                        .await
                    }) {
                        let reduced =
                            motion == ashpd::desktop::settings::ReducedMotion::ReducedMotion;
                        if REDUCED.swap(reduced, Ordering::Relaxed) != reduced {
                            context.request_repaint();
                        }
                    }
                    std::thread::sleep(Duration::from_secs(5));
                }
            });
    });
    REDUCED.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliders_preview_without_writing_and_commit_reopens_exact_values() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Appearance;
        let ctx = egui::Context::default();
        app.apply_appearance_action(
            &UiAction::SliderChanged {
                id: "appearance_intensity".into(),
                fraction: 0.7,
                committed: false,
            },
            &ctx,
        );
        assert_eq!(app.view.appearance.tint_intensity, 700);
        assert_eq!(
            SettingsPreferences::load(root.path())
                .appearance
                .tint_intensity,
            0
        );
        app.apply_appearance_action(
            &UiAction::SliderChanged {
                id: "appearance_intensity".into(),
                fraction: 0.7,
                committed: true,
            },
            &ctx,
        );
        assert_eq!(
            SettingsPreferences::load(root.path())
                .appearance
                .tint_intensity,
            700
        );
        app.apply_appearance_action(
            &UiAction::Clicked {
                id: "appearance_size:code:increment".into(),
            },
            &ctx,
        );
        app.apply_appearance_action(
            &UiAction::Clicked {
                id: "appearance_motion:always".into(),
            },
            &ctx,
        );
        let saved = SettingsPreferences::load(root.path());
        assert_eq!(saved.appearance.code_font_size, 13);
        assert_eq!(saved.appearance.reduce_motion, MotionPreference::Always);
        assert!(app.reduced_motion);
        appearance_preferences::set_current(&Default::default());
        crate::theme_mode::set_appearance(&Default::default());
    }

    fn layout_sizes(app: &mut AgentsWindow) -> openide_ui::Frame {
        app.tree
            .set_root(openide_ui::Element::column(crate::appearance_ui::groups(
                &app.view, false,
            )));
        app.tree.layout(openide_ui::Size::new(900.0, 3000.0))
    }

    fn size_event(app: &mut AgentsWindow, ctx: &egui::Context, event: openide_ui::UiEvent) {
        for action in app.tree.handle_event(event) {
            assert!(app.apply_appearance_action(&action, ctx), "{action:?}");
        }
    }

    fn replace_size(app: &mut AgentsWindow, ctx: &egui::Context, id: &str, text: &str) {
        use openide_ui::{Key, UiEvent};
        layout_sizes(app);
        assert!(app.tree.focus(id));
        size_event(
            app,
            ctx,
            UiEvent::Key {
                key: Key::A,
                shift: false,
                ctrl: true,
            },
        );
        size_event(
            app,
            ctx,
            UiEvent::Key {
                key: Key::Backspace,
                shift: false,
                ctrl: false,
            },
        );
        if !text.is_empty() {
            size_event(app, ctx, UiEvent::Text(text.into()));
        }
    }

    fn click_size(app: &mut AgentsWindow, ctx: &egui::Context, id: &str) {
        let frame = layout_sizes(app);
        let rect = frame
            .regions
            .iter()
            .find(|region| region.id == id)
            .unwrap()
            .rect;
        let point = openide_ui::Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        size_event(app, ctx, openide_ui::UiEvent::PointerDown(point));
        size_event(app, ctx, openide_ui::UiEvent::PointerUp(point));
    }

    #[test]
    fn searching_font_families_and_clearing_the_query_do_not_save_preferences() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Appearance;
        let ctx = egui::Context::default();
        let original = app.settings_preferences.appearance.clone();
        for (index, kind) in ["ui", "code"].into_iter().enumerate() {
            let id = format!("appearance_font_query:{kind}");
            app.view.settings_choice_menu_open = Some(format!("appearance_{kind}_font"));
            layout_sizes(&mut app);
            assert!(app.tree.focus(&id));
            size_event(
                &mut app,
                &ctx,
                openide_ui::UiEvent::Text("JetBrains".into()),
            );
            assert_eq!(app.view.appearance_font_query[index], "JetBrains");
            assert_eq!(app.tree.input_value(&id), Some("JetBrains"));
            assert_eq!(app.settings_preferences.appearance, original);
            assert_eq!(SettingsPreferences::load(root.path()).appearance, original);
            click_size(&mut app, &ctx, &format!("{id}:clear"));
            assert!(app.view.appearance_font_query[index].is_empty());
            assert_eq!(app.tree.input_value(&id), Some(""));
            assert_eq!(app.tree.focused_id(), Some(id.as_str()));
        }
        app.view.show_settings = false;
        app.apply_appearance_action(
            &UiAction::InputChanged {
                id: "appearance_font_query:ui".into(),
                value: "outside settings".into(),
            },
            &ctx,
        );
        assert!(app.view.appearance_font_query[0].is_empty());
        assert_eq!(SettingsPreferences::load(root.path()).appearance, original);
    }

    #[test]
    fn number_inputs_keep_incomplete_edits_then_step_reset_and_persist_exact_sizes() {
        use openide_ui::{Key, UiEvent};
        for (kind, default, min, max) in [
            ("ui", 13, UI_FONT_MIN, UI_FONT_MAX),
            ("code", 12, CODE_FONT_MIN, CODE_FONT_MAX),
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut app = AgentsWindow::with_data_root(root.path().to_owned());
            app.view.show_settings = true;
            app.view.settings_page = SettingsPage::Appearance;
            let ctx = egui::Context::default();
            let id = format!("appearance_size:{kind}");
            let saved = || {
                let p = SettingsPreferences::load(root.path()).appearance;
                if kind == "ui" {
                    p.ui_font_size
                } else {
                    p.code_font_size
                }
            };

            replace_size(&mut app, &ctx, &id, "");
            assert_eq!(app.tree.input_value(&id), Some(""));
            assert_eq!(saved(), default);
            click_size(&mut app, &ctx, &format!("{id}:increment"));
            assert_eq!(saved(), default + 1);
            assert_eq!(
                app.tree.input_value(&id),
                Some((default + 1).to_string().as_str())
            );
            click_size(&mut app, &ctx, &format!("{id}:reset"));
            replace_size(&mut app, &ctx, &id, "");
            size_event(&mut app, &ctx, UiEvent::Text("1".into()));
            assert_eq!(app.tree.input_value(&id), Some("1"));
            assert_eq!(saved(), default);
            size_event(&mut app, &ctx, UiEvent::Text("8".into()));
            assert_eq!(saved(), 18);
            assert_eq!(app.tree.focused_id(), Some(id.as_str()));
            assert_eq!(app.tree.input_cursor(&id), Some(2));
            click_size(&mut app, &ctx, &format!("{id}:increment"));
            assert_eq!(saved(), 19);
            assert_eq!(app.tree.input_value(&id), Some("19"));
            click_size(&mut app, &ctx, &format!("{id}:decrement"));
            assert_eq!(saved(), 18);

            replace_size(&mut app, &ctx, &id, &max.to_string());
            assert_eq!(saved(), max);
            app.apply_appearance_action(
                &UiAction::Clicked {
                    id: format!("{id}:increment"),
                },
                &ctx,
            );
            assert_eq!(saved(), max);
            replace_size(&mut app, &ctx, &id, &min.to_string());
            assert_eq!(saved(), min);
            app.apply_appearance_action(
                &UiAction::Clicked {
                    id: format!("{id}:decrement"),
                },
                &ctx,
            );
            assert_eq!(saved(), min);
            click_size(&mut app, &ctx, &format!("{id}:reset"));
            assert_eq!(saved(), default);
            assert_eq!(
                app.tree.input_value(&id),
                Some(default.to_string().as_str())
            );

            for invalid in ["999", "12.5", "not-a-size"] {
                replace_size(&mut app, &ctx, &id, invalid);
                assert_eq!(saved(), default);
                assert_eq!(app.tree.input_value(&id), Some(invalid));
                size_event(
                    &mut app,
                    &ctx,
                    UiEvent::Key {
                        key: Key::Enter,
                        shift: false,
                        ctrl: false,
                    },
                );
                assert_eq!(
                    app.tree.input_value(&id),
                    Some(default.to_string().as_str())
                );
            }
            replace_size(&mut app, &ctx, &id, "");
            app.view.show_settings = false;
            app.sync_appearance(&ctx);
            app.view.show_settings = true;
            layout_sizes(&mut app);
            assert_eq!(
                app.tree.input_value(&id),
                Some(default.to_string().as_str())
            );
        }
        appearance_preferences::set_current(&Default::default());
        crate::theme_mode::set_appearance(&Default::default());
    }

    #[tokio::test]
    async fn smoothing_and_motion_persist_across_restart() {
        let root = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let click = |app: &mut AgentsWindow, id: &str| {
            assert!(app.apply_appearance_action(&UiAction::Clicked { id: id.into() }, &ctx));
        };
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Appearance;
        click(&mut app, "appearance_motion:never");
        click(&mut app, "appearance_smoothing");
        let saved = SettingsPreferences::load(root.path());
        assert!(!saved.appearance.font_smoothing);
        assert_eq!(saved.appearance.reduce_motion, MotionPreference::Never);
        drop(app);

        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Appearance;
        app.sync_appearance(&ctx);
        assert!(!app.view.appearance.font_smoothing);
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            assert!(!ctx.style_of(theme).visuals.text_options.subpixel_binning);
        }
        click(&mut app, "appearance_motion:always");
        assert!(app.reduced_motion);
        assert_eq!(
            SettingsPreferences::load(root.path())
                .appearance
                .reduce_motion,
            MotionPreference::Always
        );
        appearance_preferences::set_current(&Default::default());
        crate::theme_mode::set_appearance(&Default::default());
    }
}
