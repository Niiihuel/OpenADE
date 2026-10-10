//! Named navigation presets. One binding supplies dispatch and visible hints.
//! Editor modes (such as Vim) are independent of the workbench keymap.
use eframe::egui;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum KeymapProfile {
    #[default]
    VsCode,
    Zed,
    OpenIde,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShortcutAction {
    CommandPalette,
    QuickOpen,
    ToggleSidebar,
    ToggleBottomPanel,
    ToggleTerminal,
    NewFile,
    NewTask,
    Settings,
    Problems,
    Output,
    DebugConsole,
    Files,
    Search,
    Git,
    Extensions,
    Debug,
    ModelPicker,
    DuplicateLines,
}

impl ShortcutAction {
    pub const ALL: [Self; 18] = [
        Self::CommandPalette,
        Self::QuickOpen,
        Self::NewFile,
        Self::Settings,
        Self::DuplicateLines,
        Self::ToggleSidebar,
        Self::ToggleBottomPanel,
        Self::ToggleTerminal,
        Self::Files,
        Self::Search,
        Self::Git,
        Self::Extensions,
        Self::Debug,
        Self::Problems,
        Self::Output,
        Self::DebugConsole,
        Self::NewTask,
        Self::ModelPicker,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::CommandPalette => "Paleta de comandos",
            Self::QuickOpen => "Abrir archivo del proyecto",
            Self::ToggleSidebar => "Mostrar u ocultar la barra lateral",
            Self::ToggleBottomPanel => "Mostrar u ocultar el panel inferior",
            Self::ToggleTerminal => "Abrir o enfocar la terminal",
            Self::NewFile => "Nuevo archivo en el IDE",
            Self::NewTask => "Nueva tarea",
            Self::Settings => "Abrir configuración",
            Self::Problems => "Problemas",
            Self::Output => "Salida",
            Self::DebugConsole => "Consola de depuración",
            Self::Files => "Explorador de archivos",
            Self::Search => "Buscar en el proyecto",
            Self::Git => "Control de versiones",
            Self::Extensions => "Complementos",
            Self::Debug => "Depuración",
            Self::ModelPicker => "Elegir modelo y esfuerzo",
            Self::DuplicateLines => "Duplicar líneas del editor",
        }
    }

    pub fn group(self) -> &'static str {
        match self {
            Self::CommandPalette
            | Self::QuickOpen
            | Self::NewFile
            | Self::Settings
            | Self::DuplicateLines => "Navegación",
            Self::NewTask | Self::ModelPicker => "Conversación",
            _ => "Paneles",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Binding {
    key: egui::Key,
    shift: bool,
    alt: bool,
    physical_ctrl: bool,
    no_primary: bool,
    label: &'static str,
}

impl Binding {
    fn matches(self, event: &egui::Event) -> bool {
        let egui::Event::Key {
            key,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } = event
        else {
            return false;
        };
        let primary = if self.no_primary {
            !modifiers.ctrl && !modifiers.mac_cmd
        } else if self.physical_ctrl || !cfg!(target_os = "macos") {
            modifiers.ctrl && !modifiers.mac_cmd
        } else {
            modifiers.mac_cmd && !modifiers.ctrl
        };
        *key == self.key && primary && modifiers.shift == self.shift && modifiers.alt == self.alt
    }
}

impl KeymapProfile {
    pub const ALL: [Self; 3] = [Self::VsCode, Self::Zed, Self::OpenIde];
    pub fn id(self) -> &'static str {
        match self {
            Self::VsCode => "vscode",
            Self::Zed => "zed",
            Self::OpenIde => "openide",
        }
    }
    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|profile| profile.id() == value)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::VsCode => "VS Code",
            Self::Zed => "Zed",
            Self::OpenIde => "OpenIDE clásico",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::VsCode => "Navegación y paneles con las combinaciones habituales de VS Code.",
            Self::Zed => "Navegación y paneles basados en el mapa predeterminado de Zed.",
            Self::OpenIde => "Conserva Ctrl+K para buscar y Ctrl+N para crear una tarea.",
        }
    }
    pub fn shortcut(self, action: ShortcutAction) -> &'static str {
        self.binding(action).map_or("", |binding| binding.label)
    }
    fn binding(self, action: ShortcutAction) -> Option<Binding> {
        use egui::Key;
        use ShortcutAction::*;
        let classic = self == Self::OpenIde;
        if action == DuplicateLines && !classic {
            // VS Code and Zed reserve Shift+Alt+Down for an additional cursor
            // on Linux; duplicating a line also requires Ctrl there.
            let linux = cfg!(target_os = "linux");
            return Some(Binding {
                key: Key::ArrowDown,
                shift: true,
                alt: true,
                physical_ctrl: linux,
                no_primary: !linux,
                label: if linux {
                    "Ctrl+Shift+Alt+Down"
                } else {
                    "Shift+Alt+Down"
                },
            });
        }
        let (key, shift, alt, ctrl_label, cmd_label) = match action {
            CommandPalette if classic => (Key::K, false, false, "Ctrl+K", "Cmd+K"),
            CommandPalette => (Key::P, true, false, "Ctrl+Shift+P", "Cmd+Shift+P"),
            QuickOpen => (Key::P, false, false, "Ctrl+P", "Cmd+P"),
            ToggleSidebar => (Key::B, false, false, "Ctrl+B", "Cmd+B"),
            ToggleBottomPanel => (Key::J, false, false, "Ctrl+J", "Cmd+J"),
            ToggleTerminal => (Key::Backtick, false, false, "Ctrl+`", "Ctrl+`"),
            NewFile if classic => return None,
            NewFile => (Key::N, false, false, "Ctrl+N", "Cmd+N"),
            NewTask if classic => (Key::N, false, false, "Ctrl+N", "Cmd+N"),
            NewTask => (Key::N, false, true, "Ctrl+Alt+N", "Cmd+Alt+N"),
            Settings => (Key::Comma, false, false, "Ctrl+,", "Cmd+,"),
            Problems if classic => return None,
            Problems => (Key::M, true, false, "Ctrl+Shift+M", "Cmd+Shift+M"),
            Output | DebugConsole if self == Self::Zed => return None,
            Output => (Key::U, true, false, "Ctrl+Shift+U", "Cmd+Shift+U"),
            DebugConsole => (Key::Y, true, false, "Ctrl+Shift+Y", "Cmd+Shift+Y"),
            Files => (Key::E, true, false, "Ctrl+Shift+E", "Cmd+Shift+E"),
            Search => (Key::F, true, false, "Ctrl+Shift+F", "Cmd+Shift+F"),
            Git => (Key::G, true, false, "Ctrl+Shift+G", "Ctrl+Shift+G"),
            Extensions => (Key::X, true, false, "Ctrl+Shift+X", "Cmd+Shift+X"),
            Debug if classic => return None,
            Debug => (Key::D, true, false, "Ctrl+Shift+D", "Cmd+Shift+D"),
            ModelPicker if classic => (Key::M, true, false, "Ctrl+Shift+M", "Cmd+Shift+M"),
            ModelPicker => (Key::M, false, true, "Ctrl+Alt+M", "Cmd+Alt+M"),
            DuplicateLines => (Key::D, true, false, "Ctrl+Shift+D", "Cmd+Shift+D"),
        };
        Some(Binding {
            key,
            shift,
            alt,
            physical_ctrl: action == ToggleTerminal || action == Git,
            no_primary: false,
            label: if cfg!(target_os = "macos") {
                cmd_label
            } else {
                ctrl_label
            },
        })
    }

    /// Inspect native events before the lossy UI-key translation (which omits
    /// Alt and combines Ctrl/Cmd). Only a dispatched binding is consumed.
    pub(crate) fn matches(self, action: ShortcutAction, event: &egui::Event) -> bool {
        if action == ShortcutAction::CommandPalette
            && matches!(event,
            egui::Event::Key { key: egui::Key::F1, pressed: true, repeat: false, modifiers, .. }
                if modifiers.is_none())
        {
            return true;
        }
        self.binding(action)
            .is_some_and(|binding| binding.matches(event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(key: egui::Key, shift: bool, alt: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: !cfg!(target_os = "macos"),
                mac_cmd: cfg!(target_os = "macos"),
                command: true,
                shift,
                alt,
            },
        }
    }
    #[test]
    fn presets_have_unique_exact_native_chords_and_matching_hints() {
        for profile in KeymapProfile::ALL {
            assert_eq!(KeymapProfile::from_id(profile.id()), Some(profile));
            for action in ShortcutAction::ALL {
                let Some(binding) = profile.binding(action) else {
                    continue;
                };
                let mut key = event(binding.key, binding.shift, binding.alt);
                if binding.no_primary {
                    if let egui::Event::Key { modifiers, .. } = &mut key {
                        modifiers.ctrl = false;
                        modifiers.mac_cmd = false;
                        modifiers.command = false;
                    }
                }
                if binding.physical_ctrl {
                    if let egui::Event::Key { modifiers, .. } = &mut key {
                        modifiers.ctrl = true;
                        modifiers.mac_cmd = false;
                    }
                }
                assert!(!profile.shortcut(action).is_empty());
                let matches: Vec<_> = ShortcutAction::ALL
                    .into_iter()
                    .filter(|candidate| profile.matches(*candidate, &key))
                    .collect();
                assert_eq!(matches, vec![action], "{:?}: {:?}", profile, key);
            }
        }
    }
    #[test]
    fn vscode_palette_does_not_capture_prefix_delete_line_or_altgr() {
        use egui::Key;
        use ShortcutAction::*;
        assert!(!KeymapProfile::VsCode.matches(CommandPalette, &event(Key::K, false, false)));
        assert!(!KeymapProfile::OpenIde.matches(CommandPalette, &event(Key::K, true, false)));
        assert!(!KeymapProfile::VsCode.matches(ToggleBottomPanel, &event(Key::J, false, true)));
        assert!(KeymapProfile::VsCode.matches(Problems, &event(Key::M, true, false)));
        assert!(KeymapProfile::OpenIde.matches(ModelPicker, &event(Key::M, true, false)));
        assert!(!KeymapProfile::VsCode.matches(ModelPicker, &event(Key::M, true, false)));
    }

    #[test]
    fn duplicate_line_chord_follows_the_platform_without_capturing_the_other_chord() {
        use ShortcutAction::DuplicateLines;
        let native = |ctrl: bool| egui::Event::Key {
            key: egui::Key::ArrowDown,
            physical_key: Some(egui::Key::ArrowDown),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl,
                command: ctrl && !cfg!(target_os = "macos"),
                shift: true,
                alt: true,
                ..Default::default()
            },
        };
        let linux = cfg!(target_os = "linux");
        for profile in [KeymapProfile::VsCode, KeymapProfile::Zed] {
            assert!(profile.matches(DuplicateLines, &native(linux)));
            assert!(!profile.matches(DuplicateLines, &native(!linux)));
            assert_eq!(
                profile.shortcut(DuplicateLines),
                if linux {
                    "Ctrl+Shift+Alt+Down"
                } else {
                    "Shift+Alt+Down"
                },
            );
        }
        assert!(KeymapProfile::OpenIde.matches(DuplicateLines, &event(egui::Key::D, true, false),));
        assert!(!KeymapProfile::OpenIde.matches(DuplicateLines, &native(linux)));
    }
}
