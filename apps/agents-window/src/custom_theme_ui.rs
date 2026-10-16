//! Local theme library and editor, rendered with the existing settings controls.
use crate::{
    custom_theme::{CustomTheme, EDITABLE_TOKENS},
    settings_parts, settings_ui, theme,
    view::ViewState,
};
use openide_ui::{
    AlertTone as FeedbackTone, ButtonGroup, ColorPicker, Component, DropdownOption, Element, Hsv,
    Slider, TextField,
};

#[derive(Default)]
pub(crate) struct ThemeUiState {
    pub editor: Option<ThemeEditor>,
    pub pending_delete: Option<String>,
    pub file_busy: bool,
    pub error: Option<String>,
}

pub(crate) struct ThemeEditor {
    pub theme: CustomTheme,
    pub editing_id: Option<String>,
    pub token: String,
    pub hsv: Hsv,
    pub hex_valid: bool,
}

impl ThemeEditor {
    pub fn new(theme: CustomTheme, editing_id: Option<String>) -> Self {
        let token = EDITABLE_TOKENS[0].0.to_owned();
        let hsv = Hsv::from(theme.normalized(&token));
        Self {
            theme,
            editing_id,
            token,
            hsv,
            hex_valid: true,
        }
    }
}

pub(crate) fn groups(state: &ViewState, compact: bool) -> Vec<Element> {
    let ui = &state.custom_theme_ui;
    if let Some(editor) = &ui.editor {
        let mut groups = vec![editor_group(state, editor, compact)];
        if let Some(error) = &ui.error {
            groups.push(settings_parts::feedback_banner(
                error,
                FeedbackTone::Error,
                None,
                compact,
            ));
        }
        return groups;
    }
    let available = !ui.file_busy && ui.editor.is_none();
    let mut rows = vec![settings_parts::settings_row(
        "Biblioteca de temas",
        "Crea un tema propio o importa uno desde un archivo.",
        ButtonGroup::new(vec![
            settings_ui::outline("appearance_theme_new", "Crear tema")
                .enabled(available && state.appearance.custom_themes.len() < 30)
                .render(),
            settings_ui::outline(
                "appearance_theme_import",
                if ui.file_busy {
                    "Seleccionando archivo"
                } else {
                    "Importar"
                },
            )
            .enabled(available && state.appearance.custom_themes.len() < 30)
            .render(),
        ])
        .stacked(compact)
        .render(),
        compact,
    )];
    for item in &state.appearance.custom_themes {
        let active = crate::theme_mode::choice() == crate::theme_mode::ThemeChoice::Custom
            && state.appearance.active_custom_theme.as_deref() == Some(&item.id);
        let id = &item.id;
        let actions = [
            ("edit", "Editar"),
            ("duplicate", "Duplicar"),
            ("export", "Exportar"),
            ("delete", "Eliminar"),
        ]
        .into_iter()
        .map(|(action, title)| {
            settings_ui::outline(format!("appearance_theme_{action}:{id}"), title)
                .enabled(
                    available
                        && !(action == "duplicate" && state.appearance.custom_themes.len() >= 30),
                )
                .render()
        })
        .collect();
        rows.push(settings_parts::settings_row(
            &item.name,
            if active {
                "En uso"
            } else if item.high_contrast {
                "Tema local de alto contraste"
            } else {
                "Tema local"
            },
            ButtonGroup::new(actions).stacked(compact).render(),
            compact,
        ));
        if ui.pending_delete.as_deref() == Some(id) {
            rows.push(settings_parts::settings_row(
                "Eliminar este tema",
                "Si está activo, se volverá al tema oscuro de openADE.",
                ButtonGroup::new(vec![
                    settings_ui::outline("appearance_theme_delete_cancel", "Cancelar").render(),
                    settings_ui::outline(
                        format!("appearance_theme_delete_confirm:{id}"),
                        "Eliminar tema",
                    )
                    .render(),
                ])
                .stacked(compact)
                .render(),
                compact,
            ));
        }
    }
    let mut groups = vec![settings_parts::settings_group("Temas personalizados", rows)];
    if let Some(error) = &ui.error {
        groups.push(settings_parts::feedback_banner(
            error,
            FeedbackTone::Error,
            None,
            compact,
        ));
    }
    groups
}

fn editor_group(state: &ViewState, editor: &ThemeEditor, compact: bool) -> Element {
    use crate::custom_theme::ThemeAppearance;
    let color = editor.theme.normalized(&editor.token);
    let label = EDITABLE_TOKENS
        .iter()
        .find(|(token, _)| *token == editor.token)
        .map_or(editor.token.as_str(), |(_, label)| *label);
    let mut rows = vec![
        settings_parts::settings_row(
            "Nombre",
            "Se conserva en este equipo y al exportar.",
            TextField::new(
                "appearance_theme_name",
                &editor.theme.name,
                "Mi tema",
                theme::control_palette(),
            )
            .accessibility_label("Nombre del tema")
            .width(220.0)
            .render(),
            compact,
        ),
        settings_parts::settings_row(
            "Base",
            "Define los colores predeterminados que no hayas modificado.",
            settings_ui::settings_choice_selector(
                state,
                "appearance_theme_base",
                if editor.theme.appearance == ThemeAppearance::Light {
                    "Claro"
                } else {
                    "Oscuro"
                },
                [
                    ("light", "Claro", ThemeAppearance::Light),
                    ("dark", "Oscuro", ThemeAppearance::Dark),
                ]
                .into_iter()
                .map(|(id, label, value)| {
                    DropdownOption::new(format!("appearance_theme_base:{id}"), label)
                        .selected(value == editor.theme.appearance)
                })
                .collect(),
            ),
            compact,
        ),
        settings_parts::switch_row(
            "appearance_theme_contrast",
            "Alto contraste",
            "Usa valores de alto contraste para los colores sin personalizar.",
            editor.theme.high_contrast,
            true,
            compact,
        ),
        settings_parts::settings_row(
            "Color a editar",
            "Los grupos compartidos se aplican juntos en la interfaz nativa.",
            settings_ui::settings_choice_selector(
                state,
                "appearance_theme_token",
                label,
                EDITABLE_TOKENS
                    .iter()
                    .map(|(token, label)| {
                        DropdownOption::new(format!("appearance_theme_token:{token}"), *label)
                            .selected(*token == editor.token)
                    })
                    .collect(),
            ),
            compact,
        ),
        settings_parts::settings_row(
            "Color",
            "Selecciona el color o escribe su valor hexadecimal RGB.",
            ColorPicker::new(
                "appearance_theme_color",
                editor.hsv,
                theme::control_palette(),
            )
            .part_labels("Saturación y brillo", "Tono del color", "Color hexadecimal")
            .width(220.0)
            .render(),
            compact,
        ),
        settings_parts::settings_row(
            "Opacidad",
            "La transparencia se conserva al guardar y exportar.",
            Element::row(vec![
                Slider::new(
                    "appearance_theme_alpha",
                    f32::from(color.a) / 255.0,
                    theme::control_palette(),
                )
                .accessibility_label("Opacidad del color")
                .steps(256)
                .width(160.0)
                .render(),
                openide_ui::Text::new(
                    format!("{}%", (f32::from(color.a) * 100.0 / 255.0).round()),
                    theme::card_palette(),
                )
                .render(),
            ])
            .gap(8.0)
            .centered_y(),
            compact,
        ),
        settings_parts::settings_row(
            "Color predeterminado",
            "Quita la personalización de este grupo.",
            settings_ui::outline("appearance_theme_reset_color", "Restablecer color").render(),
            compact,
        ),
    ];
    if !editor.theme.report().is_empty() {
        rows.push(settings_parts::feedback_banner("Los colores del terminal y otros tokens sin equivalente independiente se conservan para exportar. Este editor muestra los grupos que modifican la interfaz nativa.", FeedbackTone::Info, None, compact));
    }
    if !editor.hex_valid {
        rows.push(settings_parts::feedback_banner(
            "Escribe un color hexadecimal RGB válido antes de guardar.",
            FeedbackTone::Error,
            None,
            compact,
        ));
    }
    rows.push(settings_parts::settings_row(
        "Guardar tema",
        "Guardar aplica los cambios; cancelar conserva el tema anterior.",
        ButtonGroup::new(vec![
            settings_ui::outline("appearance_theme_cancel", "Cancelar").render(),
            settings_ui::outline("appearance_theme_save", "Guardar y aplicar")
                .enabled(editor.hex_valid)
                .render(),
        ])
        .stacked(compact)
        .render(),
        compact,
    ));
    settings_parts::settings_group(
        if editor.editing_id.is_some() {
            "Editar tema"
        } else {
            "Nuevo tema"
        },
        rows,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom_theme::ThemeAppearance;
    use openide_ui::{Size, UiTree};
    use std::path::PathBuf;

    #[test]
    fn theme_library_and_editor_fit_and_keep_keyboard_access_at_360_and_900() {
        let mut state = crate::view::tests::empty_state();
        state.show_settings = true;
        state.settings_page = settings_ui::SettingsPage::Appearance;
        let theme = CustomTheme::new("Tema de prueba", ThemeAppearance::Dark);
        state.appearance.custom_themes.push(theme.clone());
        for editing in [false, true] {
            state.custom_theme_ui.editor =
                editing.then(|| ThemeEditor::new(theme.clone(), Some(theme.id.clone())));
            for width in [360.0, 900.0] {
                let mut tree = UiTree::new(settings_ui::build_settings_page(
                    &state,
                    state.settings_page,
                    width,
                    true,
                ));
                tree.set_reduced_motion(true);
                let frame = tree.layout(Size::new(width, 8000.0));
                let ids = if editing {
                    vec![
                        "appearance_theme_name".to_owned(),
                        "appearance_theme_save".into(),
                        "appearance_theme_cancel".into(),
                        "settings_choice_toggle:appearance_theme_token".into(),
                        "appearance_theme_color:hex".into(),
                        "appearance_theme_alpha".into(),
                    ]
                } else {
                    vec![
                        "appearance_theme_new".into(),
                        "appearance_theme_import".into(),
                        format!("appearance_theme_edit:{}", theme.id),
                    ]
                };
                for id in ids {
                    let region = frame
                        .regions
                        .iter()
                        .find(|region| region.id == id)
                        .unwrap_or_else(|| panic!("missing {id}"));
                    assert!(
                        region.rect.x >= -0.5 && region.rect.x + region.rect.width <= width + 0.5,
                        "{id} at {width}: {:?}",
                        region.rect
                    );
                    assert!(tree.focus(&id), "{id} must be keyboard accessible");
                    if editing && id == "appearance_theme_name" {
                        assert!(
                            region.rect.y + region.rect.height < 600.0,
                            "the editor must open in the first viewport"
                        );
                    }
                }
                if let Some(directory) = std::env::var_os("OPENIDE_APPEARANCE_VISUAL_DIR") {
                    let path = PathBuf::from(directory).join(format!(
                        "custom-theme-{}-{width:.0}.svg",
                        if editing { "editor" } else { "library" }
                    ));
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent).unwrap();
                    }
                    crate::settings_ui::visual_qa::write_native_frame_svg(
                        &frame, &path, width, 8000.0,
                    )
                    .unwrap();
                }
            }
        }
    }
}
