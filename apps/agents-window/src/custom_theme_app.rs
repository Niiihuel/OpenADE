//! Theme drafts and file pickers never mutate the saved library before validation.
use super::*;
use crate::{
    custom_theme::{CustomTheme, ThemeAppearance, EDITABLE_TOKENS},
    custom_theme_ui::ThemeEditor,
    theme_mode::ThemeChoice,
};
use std::io::Read;

pub(super) struct ThemeFileTask {
    response: mpsc::Receiver<Result<Option<ThemeFileResult>, String>>,
}

enum ThemeFileResult {
    Imported(CustomTheme),
    Exported(PathBuf),
}

impl AgentsWindow {
    pub(super) fn apply_custom_theme_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        let id = match action {
            UiAction::Clicked { id }
            | UiAction::InputChanged { id, .. }
            | UiAction::SliderChanged { id, .. }
            | UiAction::AreaSliderChanged { id, .. }
                if id.starts_with("appearance_theme_") =>
            {
                id
            }
            _ => return false,
        };
        if !self.view.shows(SettingsPage::Appearance) {
            return true;
        }
        if matches!(
            action,
            UiAction::InputChanged { .. }
                | UiAction::SliderChanged { .. }
                | UiAction::AreaSliderChanged { .. }
        ) {
            self.view.custom_theme_ui.error = None;
        }
        let mut color_changed = false;
        match action {
            UiAction::InputChanged { value, .. } if id == "appearance_theme_name" => {
                if let Some(editor) = &mut self.view.custom_theme_ui.editor {
                    if value.len() <= 4096 {
                        editor.theme.name.clone_from(value);
                    }
                }
            }
            UiAction::Clicked { .. } if id == "appearance_theme_cancel" => {
                self.view.custom_theme_ui.editor = None;
                self.view.custom_theme_ui.error = None;
                self.view.settings_choice_menu_open = None;
            }
            UiAction::Clicked { .. } if id == "appearance_theme_new" => {
                if !self.theme_library_editable()
                    || self.settings_preferences.appearance.custom_themes.len() >= 30
                {
                    return true;
                }
                let base = if crate::theme_mode::light() {
                    ThemeAppearance::Light
                } else {
                    ThemeAppearance::Dark
                };
                self.open_theme_draft(CustomTheme::new("Mi tema", base), None);
            }
            UiAction::Clicked { .. } if id == "appearance_theme_import" => {
                if self.theme_library_editable()
                    && self.settings_preferences.appearance.custom_themes.len() < 30
                {
                    self.start_theme_file_task(None, ctx);
                }
            }
            UiAction::Clicked { .. } if id == "appearance_theme_save" => {
                self.save_theme_draft(ctx);
            }
            UiAction::Clicked { .. } if id == "appearance_theme_delete_cancel" => {
                self.view.custom_theme_ui.pending_delete = None;
            }
            UiAction::Clicked { .. } if id == "appearance_theme_contrast" => {
                if let Some(editor) = &mut self.view.custom_theme_ui.editor {
                    remove_inherited_colors(&mut editor.theme);
                    editor.theme.high_contrast = !editor.theme.high_contrast;
                    editor.hsv = editor.theme.normalized(&editor.token).into();
                    color_changed = true;
                }
            }
            UiAction::Clicked { .. } if id == "appearance_theme_reset_color" => {
                if let Some(editor) = &mut self.view.custom_theme_ui.editor {
                    editor.theme.colors.remove(&editor.token);
                    editor.hsv = editor.theme.normalized(&editor.token).into();
                    color_changed = true;
                }
            }
            UiAction::Clicked { .. } if id.starts_with("appearance_theme_token:") => {
                let token = id.trim_start_matches("appearance_theme_token:");
                if EDITABLE_TOKENS.iter().any(|(known, _)| *known == token) {
                    if let Some(editor) = &mut self.view.custom_theme_ui.editor {
                        editor.token = token.into();
                        editor.hsv = editor.theme.normalized(token).into();
                        color_changed = true;
                    }
                    self.view.settings_choice_menu_open = None;
                }
            }
            UiAction::Clicked { .. } if id.starts_with("appearance_theme_base:") => {
                let base = match id.trim_start_matches("appearance_theme_base:") {
                    "light" => ThemeAppearance::Light,
                    "dark" => ThemeAppearance::Dark,
                    _ => return true,
                };
                if let Some(editor) = &mut self.view.custom_theme_ui.editor {
                    remove_inherited_colors(&mut editor.theme);
                    editor.theme.appearance = base;
                    editor.hsv = editor.theme.normalized(&editor.token).into();
                    color_changed = true;
                }
                self.view.settings_choice_menu_open = None;
            }
            UiAction::Clicked { .. } if id.contains(':') => {
                let (operation, theme_id) = id.split_once(':').unwrap();
                if !self.theme_library_editable() {
                    return true;
                }
                let Some(item) = self
                    .settings_preferences
                    .appearance
                    .custom_themes
                    .iter()
                    .find(|item| item.id == theme_id)
                    .cloned()
                else {
                    return true;
                };
                match operation {
                    "appearance_theme_apply" => {
                        let mut next = self.settings_preferences.clone();
                        next.appearance.active_custom_theme = Some(item.id);
                        self.commit_theme_library(next, ThemeChoice::Custom, ctx, "Tema aplicado");
                    }
                    "appearance_theme_edit" => {
                        self.open_theme_draft(item, Some(theme_id.to_owned()))
                    }
                    "appearance_theme_duplicate"
                        if self.settings_preferences.appearance.custom_themes.len() < 30 =>
                    {
                        let mut copy = item;
                        copy.id = Uuid::new_v4().to_string();
                        copy.name =
                            format!("{} (copia)", copy.name.chars().take(80).collect::<String>());
                        self.open_theme_draft(copy, None);
                    }
                    "appearance_theme_export" => self.start_theme_file_task(Some(item), ctx),
                    "appearance_theme_delete" => {
                        self.view.custom_theme_ui.pending_delete = Some(item.id)
                    }
                    "appearance_theme_delete_confirm"
                        if self.view.custom_theme_ui.pending_delete.as_deref()
                            == Some(theme_id) =>
                    {
                        let mut next = self.settings_preferences.clone();
                        next.appearance
                            .custom_themes
                            .retain(|item| item.id != theme_id);
                        let active =
                            next.appearance.active_custom_theme.as_deref() == Some(theme_id);
                        if active {
                            next.appearance.active_custom_theme = None;
                        }
                        let choice = if active && crate::theme_mode::choice() == ThemeChoice::Custom
                        {
                            ThemeChoice::Dark
                        } else {
                            crate::theme_mode::choice()
                        };
                        if self.commit_theme_library(next, choice, ctx, "Tema eliminado") {
                            self.view.custom_theme_ui.pending_delete = None;
                        }
                    }
                    _ => {}
                }
            }
            _ => {
                if let Some(editor) = &mut self.view.custom_theme_ui.editor {
                    if let UiAction::InputChanged { id, value } = action {
                        if id == "appearance_theme_color:hex" {
                            editor.hex_valid = openide_ui::ColorPicker::parse_hex(value).is_some();
                        }
                    }
                    if let UiAction::SliderChanged { fraction, .. } = action {
                        if id == "appearance_theme_alpha" && fraction.is_finite() {
                            let mut color = editor.theme.normalized(&editor.token);
                            color.a = (fraction.clamp(0.0, 1.0) * 255.0).round() as u8;
                            editor.theme.set_color(&editor.token.clone(), color);
                            color_changed = true;
                        }
                    }
                    if let Some(hsv) = openide_ui::ColorPicker::color_after(
                        "appearance_theme_color",
                        editor.hsv,
                        action,
                    ) {
                        let alpha = editor.theme.normalized(&editor.token).a;
                        let mut color = openide_ui::Color::from(hsv);
                        color.a = alpha;
                        editor.theme.set_color(&editor.token.clone(), color);
                        editor.hsv = hsv;
                        color_changed = !matches!(action, UiAction::InputChanged { .. });
                    }
                }
            }
        }
        if color_changed {
            self.sync_theme_color_input();
        }
        true
    }

    fn theme_library_editable(&self) -> bool {
        !self.view.custom_theme_ui.file_busy && self.view.custom_theme_ui.editor.is_none()
    }

    fn open_theme_draft(&mut self, theme: CustomTheme, editing_id: Option<String>) {
        self.tree.scroll_to_start("settings_page:Appearance");
        self.tree
            .set_input_value("appearance_theme_name", &theme.name);
        self.view.custom_theme_ui.editor = Some(ThemeEditor::new(theme, editing_id));
        self.view.custom_theme_ui.pending_delete = None;
        self.view.custom_theme_ui.error = None;
        self.view.settings_choice_menu_open = None;
        self.sync_theme_color_input();
    }

    fn sync_theme_color_input(&mut self) {
        if let Some(editor) = &mut self.view.custom_theme_ui.editor {
            editor.hex_valid = true;
            self.tree.set_input_value(
                "appearance_theme_color:hex",
                openide_ui::ColorPicker::hex(editor.theme.normalized(&editor.token)),
            );
        }
    }

    fn save_theme_draft(&mut self, ctx: &egui::Context) {
        let Some(editor) = &self.view.custom_theme_ui.editor else {
            return;
        };
        if !editor.hex_valid {
            self.view.custom_theme_ui.error =
                Some("Escribe un color hexadecimal RGB válido antes de guardar.".into());
            return;
        }
        let mut theme = editor.theme.clone();
        theme.name = theme.name.trim().to_owned();
        if let Err(error) = theme.validate() {
            self.view.custom_theme_ui.error = Some(error);
            return;
        }
        let mut next = self.settings_preferences.clone();
        if let Some(id) = &editor.editing_id {
            let Some(slot) = next
                .appearance
                .custom_themes
                .iter_mut()
                .find(|item| &item.id == id)
            else {
                self.view.custom_theme_ui.error =
                    Some("El tema original ya no está disponible.".into());
                return;
            };
            theme.id.clone_from(id);
            *slot = theme.clone();
        } else if next.appearance.custom_themes.len() < 30 {
            next.appearance.custom_themes.push(theme.clone());
        } else {
            self.view.custom_theme_ui.error = Some("La biblioteca admite hasta 30 temas.".into());
            return;
        }
        next.appearance.active_custom_theme = Some(theme.id);
        if self.commit_theme_library(next, ThemeChoice::Custom, ctx, "Tema guardado y aplicado") {
            self.view.custom_theme_ui.editor = None;
        }
    }

    fn commit_theme_library(
        &mut self,
        next: SettingsPreferences,
        choice: ThemeChoice,
        ctx: &egui::Context,
        message: &str,
    ) -> bool {
        let previous = crate::theme_mode::choice();
        if let Err(error) = crate::theme_mode::save(&self.data_root, choice) {
            self.view.custom_theme_ui.error = Some(format!("No se pudo guardar el tema: {error}"));
            return false;
        }
        if !self.commit_settings_preferences(next, message) {
            let rollback = crate::theme_mode::save(&self.data_root, previous);
            self.view.custom_theme_ui.error = Some(match rollback {
                Ok(()) => "No se pudo guardar la biblioteca. Se conservó el tema anterior.".into(),
                Err(error) => {
                    format!("No se pudo guardar la biblioteca ni restaurar la selección: {error}")
                }
            });
            return false;
        }
        crate::theme_mode::set_choice(choice);
        self.view.custom_theme_ui.error = None;
        self.view.settings_choice_menu_open = None;
        self.sync_appearance(ctx);
        true
    }

    fn start_theme_file_task(&mut self, export: Option<CustomTheme>, ctx: &egui::Context) {
        if self.theme_file_task.is_some() {
            return;
        }
        let initial = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.data_root.clone());
        let (sender, response) = mpsc::channel();
        let context = ctx.clone();
        let result = std::thread::Builder::new()
            .name("theme-files".into())
            .spawn(move || {
                let result = (|| {
                    if let Some(theme) = export {
                        let filename = format!("{}.yaml", export_slug(&theme.name));
                        let Some(path) =
                            crate::theme_file_dialog::choose_export(&initial, &filename)?
                        else {
                            return Ok(None);
                        };
                        let yaml = theme.export_yaml()?;
                        openide_harness::atomic_write(&path, yaml.as_bytes())
                            .map_err(|error| error.to_string())?;
                        Ok(Some(ThemeFileResult::Exported(path)))
                    } else {
                        let Some(path) = crate::theme_file_dialog::choose_import(&initial)? else {
                            return Ok(None);
                        };
                        read_theme_file(&path).map(|theme| Some(ThemeFileResult::Imported(theme)))
                    }
                })();
                let _ = sender.send(result);
                context.request_repaint();
            });
        match result {
            Ok(_) => {
                self.theme_file_task = Some(ThemeFileTask { response });
                self.view.custom_theme_ui.file_busy = true;
                self.view.custom_theme_ui.error = None;
            }
            Err(error) => {
                self.view.custom_theme_ui.error =
                    Some(format!("No se pudo abrir el selector: {error}"))
            }
        }
    }

    pub(super) fn poll_theme_file_task(&mut self) -> bool {
        let Some(task) = &self.theme_file_task else {
            return false;
        };
        let result = match task.response.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("El selector de temas terminó sin respuesta.".into())
            }
        };
        self.theme_file_task = None;
        self.view.custom_theme_ui.file_busy = false;
        match result {
            Ok(Some(ThemeFileResult::Imported(theme))) => self.open_theme_draft(theme, None),
            Ok(Some(ThemeFileResult::Exported(path))) => {
                self.set_settings_notice(format!("Tema exportado: {}", path.display()), false)
            }
            Ok(None) => {}
            Err(error) => self.view.custom_theme_ui.error = Some(error),
        }
        true
    }
}

fn read_theme_file(path: &Path) -> Result<CustomTheme, String> {
    if !std::fs::metadata(path)
        .map_err(|error| format!("No se pudo leer el tema: {error}"))?
        .is_file()
    {
        return Err("Selecciona un archivo de tema local.".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("No se pudo leer el tema: {error}"))?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("Selecciona un archivo de tema local.".into());
    }
    let mut bytes = Vec::new();
    file.take(crate::custom_theme::MAX_IMPORT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > crate::custom_theme::MAX_IMPORT_BYTES {
        return Err("El tema supera el tamaño máximo permitido.".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "El tema debe usar UTF-8.".to_owned())?;
    CustomTheme::import(text)
}

fn export_slug(name: &str) -> String {
    let slug = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(32)
        .collect::<String>();
    if slug.is_empty() {
        "tema".into()
    } else {
        slug
    }
}

fn remove_inherited_colors(theme: &mut CustomTheme) {
    let appearance = theme.appearance;
    let contrast = theme.high_contrast;
    theme.colors.retain(|token, value| {
        *value
            != crate::custom_theme::format_color(crate::custom_theme::default_color(
                token, appearance, contrast,
            ))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(app: &mut AgentsWindow, id: impl Into<String>, ctx: &egui::Context) {
        assert!(app.apply_custom_theme_action(&UiAction::Clicked { id: id.into() }, ctx));
    }

    #[test]
    fn drafts_cancel_without_writes_and_saved_themes_reopen_duplicate_and_delete() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Appearance;
        let ctx = egui::Context::default();
        let previous = crate::theme_mode::choice();
        click(&mut app, "appearance_theme_new", &ctx);
        click(&mut app, "appearance_theme_cancel", &ctx);
        assert!(SettingsPreferences::load(root.path())
            .appearance
            .custom_themes
            .is_empty());
        click(&mut app, "appearance_theme_new", &ctx);
        app.view.custom_theme_ui.editor.as_mut().unwrap().theme.name = "Aurora".into();
        click(&mut app, "appearance_theme_save", &ctx);
        let saved = SettingsPreferences::load(root.path()).appearance;
        assert_eq!(saved.custom_themes.len(), 1);
        assert_eq!(saved.custom_themes[0].name, "Aurora");
        assert_eq!(
            saved.active_custom_theme.as_deref(),
            Some(saved.custom_themes[0].id.as_str())
        );
        let first = saved.custom_themes[0].id.clone();
        click(
            &mut app,
            format!("appearance_theme_duplicate:{first}"),
            &ctx,
        );
        let copy_id = app
            .view
            .custom_theme_ui
            .editor
            .as_ref()
            .unwrap()
            .theme
            .id
            .clone();
        assert_ne!(copy_id, first);
        click(&mut app, "appearance_theme_save", &ctx);
        assert_eq!(
            SettingsPreferences::load(root.path())
                .appearance
                .custom_themes
                .len(),
            2
        );
        click(
            &mut app,
            format!("appearance_theme_delete_confirm:{copy_id}"),
            &ctx,
        );
        assert_eq!(
            app.settings_preferences.appearance.custom_themes.len(),
            2,
            "deletion needs its matching confirmation"
        );
        click(&mut app, format!("appearance_theme_delete:{copy_id}"), &ctx);
        click(
            &mut app,
            format!("appearance_theme_delete_confirm:{copy_id}"),
            &ctx,
        );
        let saved = SettingsPreferences::load(root.path()).appearance;
        assert_eq!(saved.custom_themes.len(), 1);
        assert!(saved.active_custom_theme.is_none());
        crate::theme_mode::set_choice(previous);
        crate::theme_mode::set_appearance(&Default::default());
        crate::appearance_preferences::set_current(&Default::default());
    }

    #[test]
    fn invalid_drafts_and_selection_write_failures_leave_saved_preferences_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Appearance;
        let ctx = egui::Context::default();
        click(&mut app, "appearance_theme_new", &ctx);
        app.view.custom_theme_ui.editor.as_mut().unwrap().theme.name = " ".into();
        click(&mut app, "appearance_theme_save", &ctx);
        assert!(app.view.custom_theme_ui.error.is_some());
        assert!(app.settings_preferences.appearance.custom_themes.is_empty());
        app.view.custom_theme_ui.editor.as_mut().unwrap().theme.name = "Valid".into();
        app.apply_custom_theme_action(
            &UiAction::InputChanged {
                id: "appearance_theme_color:hex".into(),
                value: "#12".into(),
            },
            &ctx,
        );
        click(&mut app, "appearance_theme_save", &ctx);
        assert!(app.view.custom_theme_ui.editor.is_some());
        assert!(app.settings_preferences.appearance.custom_themes.is_empty());
        app.apply_custom_theme_action(
            &UiAction::InputChanged {
                id: "appearance_theme_color:hex".into(),
                value: "#123456".into(),
            },
            &ctx,
        );
        std::fs::create_dir(root.path().join("theme")).unwrap();
        click(&mut app, "appearance_theme_save", &ctx);
        assert!(app.view.custom_theme_ui.editor.is_some());
        assert!(app.settings_preferences.appearance.custom_themes.is_empty());
        assert!(SettingsPreferences::load(root.path())
            .appearance
            .custom_themes
            .is_empty());
    }

    #[test]
    fn changing_base_rebases_inherited_colors_and_preserves_explicit_overrides() {
        let mut theme = CustomTheme::new("Theme", ThemeAppearance::Dark);
        let override_color = openide_ui::Color::rgba(12, 34, 56, 78);
        theme.set_color("accent", override_color);
        remove_inherited_colors(&mut theme);
        theme.appearance = ThemeAppearance::Light;
        assert_eq!(theme.normalized("accent"), override_color);
        assert_eq!(
            theme.normalized("editor"),
            crate::custom_theme::default_color("editor", ThemeAppearance::Light, false)
        );
        assert_ne!(
            theme.normalized("editor"),
            crate::custom_theme::default_color("editor", ThemeAppearance::Dark, false)
        );
    }

    fn layout_theme_editor(app: &mut AgentsWindow) {
        app.tree
            .set_root(Element::column(crate::custom_theme_ui::groups(
                &app.view, false,
            )));
        app.tree.layout(Size::new(900.0, 3000.0));
    }

    #[test]
    fn editing_another_theme_replaces_retained_name_and_hex_field_values() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Appearance;
        let ctx = egui::Context::default();
        let mut first = CustomTheme::new("Primer tema", ThemeAppearance::Dark);
        first.set_color("sidebar", openide_ui::Color::rgb(18, 52, 86));
        let mut second = CustomTheme::new("Segundo tema", ThemeAppearance::Light);
        second.set_color("sidebar", openide_ui::Color::rgb(171, 205, 239));
        app.settings_preferences.appearance.custom_themes = vec![first.clone(), second.clone()];
        app.view.appearance = app.settings_preferences.appearance.clone();

        click(
            &mut app,
            format!("appearance_theme_edit:{}", first.id),
            &ctx,
        );
        layout_theme_editor(&mut app);
        assert_eq!(
            app.tree.input_value("appearance_theme_name"),
            Some("Primer tema")
        );
        assert_eq!(
            app.tree.input_value("appearance_theme_color:hex"),
            Some("#123456")
        );
        app.tree
            .set_input_value("appearance_theme_name", "Borrador sin guardar");
        app.tree
            .set_input_value("appearance_theme_color:hex", "#111111");
        click(&mut app, "appearance_theme_cancel", &ctx);
        click(
            &mut app,
            format!("appearance_theme_edit:{}", second.id),
            &ctx,
        );
        layout_theme_editor(&mut app);
        assert_eq!(
            app.tree.input_value("appearance_theme_name"),
            Some("Segundo tema")
        );
        assert_eq!(
            app.tree.input_value("appearance_theme_color:hex"),
            Some("#abcdef")
        );
        assert_eq!(
            app.view.custom_theme_ui.editor.as_ref().unwrap().theme,
            second
        );
        assert_eq!(
            app.settings_preferences.appearance.custom_themes,
            vec![first, second]
        );
    }

    #[test]
    fn token_and_color_drags_refresh_hex_but_typing_keeps_its_incomplete_value() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Appearance;
        let ctx = egui::Context::default();
        let mut theme = CustomTheme::new("Colores", ThemeAppearance::Dark);
        theme.set_color("sidebar", openide_ui::Color::rgb(255, 0, 0));
        theme.set_color("accent", openide_ui::Color::rgb(0, 255, 0));
        app.open_theme_draft(theme, None);
        layout_theme_editor(&mut app);
        click(&mut app, "appearance_theme_token:accent", &ctx);
        assert_eq!(
            app.tree.input_value("appearance_theme_color:hex"),
            Some("#00ff00")
        );

        app.tree
            .set_input_value("appearance_theme_color:hex", "#12");
        assert!(app.apply_custom_theme_action(
            &UiAction::InputChanged {
                id: "appearance_theme_color:hex".into(),
                value: "#12".into(),
            },
            &ctx
        ));
        assert_eq!(
            app.tree.input_value("appearance_theme_color:hex"),
            Some("#12")
        );
        assert_eq!(
            app.view
                .custom_theme_ui
                .editor
                .as_ref()
                .unwrap()
                .theme
                .normalized("accent"),
            openide_ui::Color::rgb(0, 255, 0)
        );

        assert!(app.apply_custom_theme_action(
            &UiAction::SliderChanged {
                id: "appearance_theme_color:hue".into(),
                fraction: 2.0 / 3.0,
                committed: false,
            },
            &ctx
        ));
        assert_eq!(
            app.tree.input_value("appearance_theme_color:hex"),
            Some("#0000ff")
        );
        assert!(app.apply_custom_theme_action(
            &UiAction::AreaSliderChanged {
                id: "appearance_theme_color:area".into(),
                x: 0.0,
                y: 0.0,
                committed: false,
            },
            &ctx
        ));
        assert_eq!(
            app.tree.input_value("appearance_theme_color:hex"),
            Some("#ffffff")
        );
        click(&mut app, "appearance_theme_token:sidebar", &ctx);
        assert_eq!(
            app.tree.input_value("appearance_theme_color:hex"),
            Some("#ff0000")
        );
        click(&mut app, "appearance_theme_reset_color", &ctx);
        click(&mut app, "appearance_theme_base:light", &ctx);
        let expected = openide_ui::ColorPicker::hex(crate::custom_theme::default_color(
            "sidebar",
            ThemeAppearance::Light,
            false,
        ));
        assert_eq!(
            app.tree.input_value("appearance_theme_color:hex"),
            Some(expected.as_str())
        );
        click(&mut app, "appearance_theme_contrast", &ctx);
        let expected = openide_ui::ColorPicker::hex(crate::custom_theme::default_color(
            "sidebar",
            ThemeAppearance::Light,
            true,
        ));
        assert_eq!(
            app.tree.input_value("appearance_theme_color:hex"),
            Some(expected.as_str())
        );
        assert!(app.settings_preferences.appearance.custom_themes.is_empty());
    }

    #[test]
    fn theme_file_reader_is_bounded_and_slug_cannot_escape_selected_directory() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("theme.yaml");
        std::fs::write(&path, "name: Imported\naccent: '#123456'\n").unwrap();
        assert_eq!(read_theme_file(&path).unwrap().name, "Imported");
        std::fs::write(&path, vec![b'x'; crate::custom_theme::MAX_IMPORT_BYTES + 1]).unwrap();
        assert!(read_theme_file(&path).is_err());
        assert_eq!(export_slug("../../téma$(secret)"), "tmasecret");
        assert_eq!(export_slug("../"), "tema");
    }
}
