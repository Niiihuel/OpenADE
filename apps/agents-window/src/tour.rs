//! Guided tours of the two window modes, as data.
//!
//! A step points at controls that already exist, by their element IDs, so no
//! control is built differently because a tour mentions it. The view renders
//! the current step; the application owns which one that is.

use openide_ui::{CoachAlign, CoachSide};

use crate::ide_workspace::WindowMode;

/// The coach mark's ID: its close button and Escape report it, and its other
/// actions are `tour:next`, `tour:back` and `tour:skip`.
pub(crate) const ID: &str = "tour";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TourId {
    Agents,
    Ide,
}

/// A panel a step needs open before it can point at its target. Opening it
/// is the only thing a tour does to the window besides showing itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Ensure {
    Nothing,
    /// The task sidebar of the agents window.
    Sidebar,
    /// The IDE's left sidebar on the file explorer.
    IdeExplorer,
    /// The IDE's bottom panel.
    IdeBottom,
    /// The IDE's agent panel.
    IdeAgent,
}

pub(crate) struct Step {
    /// Names the step in tests and captures.
    pub id: &'static str,
    /// The step rings the area these controls cover together. Those that are
    /// not laid out are left out; with none, the step is skipped.
    pub targets: &'static [&'static str],
    pub title: &'static str,
    pub body: &'static str,
    pub side: CoachSide,
    pub align: CoachAlign,
    pub ensure: Ensure,
}

const AGENTS: &[Step] = &[
    Step {
        id: "new-task",
        targets: &["new_session_sidebar"],
        title: "Empieza una tarea",
        body: "Cada tarea es una conversación con un agente sobre tu proyecto. Puedes tener varias a la vez.",
        side: CoachSide::Right,
        align: CoachAlign::Start,
        ensure: Ensure::Sidebar,
    },
    Step {
        id: "tasks",
        targets: &[
            "session_organize:group",
            "session_organize:project",
            "session_sort_toggle",
            "toggle_archived_view",
            "sessions_list",
        ],
        title: "Tus tareas, ordenadas",
        body: "Agrúpalas por grupo o por proyecto, cambia el orden y consulta las que archivaste.",
        side: CoachSide::Right,
        align: CoachAlign::Start,
        ensure: Ensure::Sidebar,
    },
    Step {
        id: "composer",
        targets: &[
            "prompt",
            "composer_add_context",
            "composer:permissions",
            "composer_model_toggle",
            "composer_microphone",
            "run_toggle",
        ],
        title: "Pídele algo al agente",
        body: "Escribe qué necesitas. Abajo: adjunta archivos, elige permisos, modelo y esfuerzo, dicta por voz, y envía o detén.",
        side: CoachSide::Above,
        align: CoachAlign::Center,
        ensure: Ensure::Nothing,
    },
    Step {
        id: "context",
        targets: &["composer_context_toggle"],
        title: "Cuánto entra todavía",
        body: "El anillo se llena a medida que la conversación crece. Tócalo para ver qué ocupa lugar y compactarla.",
        side: CoachSide::Above,
        align: CoachAlign::End,
        ensure: Ensure::Nothing,
    },
    Step {
        id: "environment",
        targets: &[
            "home_workspace_settings",
            "composer_project_toggle",
            "composer_environment_toggle",
            "composer_branch_toggle",
        ],
        title: "Dónde trabaja el agente",
        body: "Elige el proyecto, en qué copia local trabaja (tu carpeta o un worktree) y la rama de Git.",
        side: CoachSide::Above,
        align: CoachAlign::Start,
        ensure: Ensure::Nothing,
    },
    Step {
        id: "panels",
        targets: &["environment_open", "toggle_side_pane_header"],
        title: "Lo que pasa en el proyecto",
        body: "Entorno resume el proyecto y sus cambios. El panel lateral abre Archivos, Actividad, Navegador y terminal.",
        side: CoachSide::Below,
        align: CoachAlign::End,
        ensure: Ensure::Nothing,
    },
    Step {
        id: "usage",
        targets: &["usage_toggle"],
        title: "Cuenta y uso",
        body: "Mira cuánto usaste de cada proveedor y cuándo se renueva el límite, sin salir de la tarea.",
        side: CoachSide::Right,
        align: CoachAlign::End,
        ensure: Ensure::Sidebar,
    },
    Step {
        id: "notifications",
        targets: &["notifications_toggle"],
        title: "Notificaciones",
        body: "Aquí queda lo que pasó fuera de tu vista: un turno que terminó o un permiso que espera tu aprobación.",
        side: CoachSide::Below,
        align: CoachAlign::Start,
        ensure: Ensure::Sidebar,
    },
    Step {
        id: "ide",
        targets: &["window_mode_toggle"],
        title: "Pasa al IDE",
        body: "Este botón abre la vista IDE: editor, archivos y terminal, con el agente al lado.",
        side: CoachSide::Below,
        align: CoachAlign::End,
        ensure: Ensure::Nothing,
    },
];

const IDE: &[Step] = &[
    Step {
        id: "activity",
        targets: &[
            "ide_activity:files",
            "ide_activity:search",
            "ide_activity:git",
            "ide_activity:debug",
            "ide_activity:extensions",
        ],
        title: "Cinco vistas del proyecto",
        body: "Explorador de archivos, búsqueda en todo el proyecto, control de versiones, depuración y complementos.",
        side: CoachSide::Right,
        align: CoachAlign::Start,
        ensure: Ensure::Nothing,
    },
    Step {
        id: "files",
        targets: &["file_tree"],
        title: "Tus archivos",
        body: "Un clic abre el archivo. Con clic derecho puedes crear, renombrar, copiar la ruta o mencionarlo al agente.",
        side: CoachSide::Right,
        align: CoachAlign::Start,
        ensure: Ensure::IdeExplorer,
    },
    Step {
        id: "quick-open",
        targets: &["ide_titlebar_quick_open"],
        title: "Abre cualquier archivo",
        body: "Escribe parte del nombre y ábrelo al instante, sin recorrer carpetas.",
        side: CoachSide::Below,
        align: CoachAlign::Center,
        ensure: Ensure::Nothing,
    },
    Step {
        id: "tabs",
        targets: &["right_dock_tabs"],
        title: "Archivos abiertos",
        body: "Cada archivo que abres queda en una pestaña. Cambia de uno a otro o ciérralos desde aquí.",
        side: CoachSide::Below,
        align: CoachAlign::Start,
        ensure: Ensure::Nothing,
    },
    Step {
        id: "bottom",
        targets: &[
            "ide_bottom:terminal",
            "ide_bottom:problems",
            "ide_bottom:output",
            "ide_bottom:console",
        ],
        title: "Panel inferior",
        body: "Terminal para correr comandos, Problemas del código, Salida de las herramientas y Consola de depuración.",
        side: CoachSide::Above,
        align: CoachAlign::Start,
        ensure: Ensure::IdeBottom,
    },
    Step {
        id: "agent",
        targets: &["ide_agent_tabs", "ide_agent_new_toggle", "ide_agent_list_toggle"],
        title: "El agente, al lado del código",
        body: "Cada pestaña es una tarea. Con + empiezas otra, y el botón de lista las muestra todas.",
        side: CoachSide::Below,
        align: CoachAlign::End,
        ensure: Ensure::IdeAgent,
    },
    Step {
        id: "status",
        targets: &[
            "ide_git_review",
            "ide_editor_diagnostics",
            "notifications_toggle",
        ],
        title: "De un vistazo",
        body: "La rama de Git, los problemas del archivo abierto y las notificaciones. Cada dato se puede tocar.",
        side: CoachSide::Above,
        align: CoachAlign::Start,
        ensure: Ensure::Nothing,
    },
    Step {
        id: "agents",
        targets: &["window_mode_toggle"],
        title: "Vuelve a los agentes",
        body: "Este botón cambia entre el IDE y la ventana de agentes. Tus tareas siguen ahí.",
        side: CoachSide::Below,
        align: CoachAlign::End,
        ensure: Ensure::Nothing,
    },
];

impl TourId {
    pub const ALL: [Self; 2] = [Self::Agents, Self::Ide];

    /// The key of its record in the preferences and the end of its start
    /// action, `tour_start:<key>`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Ide => "ide",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tour| tour.key() == key)
    }

    /// Raise it when the steps change enough to be worth offering again.
    pub fn version(self) -> u32 {
        match self {
            Self::Agents | Self::Ide => 1,
        }
    }

    pub fn mode(self) -> WindowMode {
        match self {
            Self::Agents => WindowMode::Agents,
            Self::Ide => WindowMode::Ide,
        }
    }

    pub fn for_mode(mode: WindowMode) -> Self {
        match mode {
            WindowMode::Agents => Self::Agents,
            WindowMode::Ide => Self::Ide,
        }
    }

    pub fn steps(self) -> &'static [Step] {
        match self {
            Self::Agents => AGENTS,
            Self::Ide => IDE,
        }
    }

    /// The window it explains, as the invitation names it.
    pub fn subject(self) -> &'static str {
        match self {
            Self::Agents => "la ventana de agentes",
            Self::Ide => "el IDE",
        }
    }

    pub fn start_action(self) -> String {
        format!("tour_start:{}", self.key())
    }
}

/// The tour on screen. `backwards` is the direction of travel, so a step
/// whose target is missing is skipped the way the user was going.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ActiveTour {
    pub tour: TourId,
    pub step: usize,
    pub backwards: bool,
    /// The panel this step points into has been asked for.
    pub ensured: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TourView {
    pub active: Option<ActiveTour>,
    /// The tours the corner card offers, each while its window mode is shown.
    pub invites: Vec<TourId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_step_is_short_plain_and_uniquely_named() {
        for tour in TourId::ALL {
            let steps = tour.steps();
            assert!((7..=9).contains(&steps.len()), "{tour:?}");
            assert_eq!(TourId::from_key(tour.key()), Some(tour));
            assert_eq!(TourId::for_mode(tour.mode()), tour);
            for (index, step) in steps.iter().enumerate() {
                assert!(!step.targets.is_empty(), "{}", step.id);
                assert!(step.body.chars().count() <= 140, "{}: {}", step.id, step.body.chars().count());
                assert!(step.title.chars().count() <= 32, "{}", step.id);
                assert!(step.body.ends_with('.'), "{}", step.id);
                assert!(
                    !steps[..index].iter().any(|other| other.id == step.id),
                    "{} repeats",
                    step.id
                );
            }
        }
    }
}
