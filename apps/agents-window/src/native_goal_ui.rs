//! Explicit native goal controls; text-only composer goals are not migrated.
use openide_core::{NativeGoal, NativeGoalStatus};
#[derive(Clone, Debug, Default)]
pub(crate) struct NativeGoalUiState {
    pub session_id: Option<String>,
    pub goal: Option<NativeGoal>,
    pub criteria_draft: String,
    pub error: Option<String>,
    pub recovery: openide_core::NativeGoalRecoveryReport,
}
impl NativeGoalUiState {
    pub fn recovery_description(&self) -> String {
        let mut lines = vec![format!(
            "Última revisión: {} temporales eliminados · {} sesiones ocupadas · {} errores",
            self.recovery.removed_temporaries,
            self.recovery.busy_session_ids.len(),
            self.recovery.diagnostics.len()
        )];
        for id in &self.recovery.busy_session_ids {
            lines.push(format!("Sesión ocupada: {id}"));
        }
        for diagnostic in &self.recovery.diagnostics {
            let id = if diagnostic.session_id.is_empty() {
                "historial"
            } else {
                &diagnostic.session_id
            };
            lines.push(format!("Sesión {id}: {}", diagnostic.error));
        }
        lines.join("\n")
    }

    pub fn control_id(&self) -> Option<String> {
        let goal = self.goal.as_ref()?;
        let pause = match goal.status {
            NativeGoalStatus::Active => true,
            NativeGoalStatus::Paused => false,
            NativeGoalStatus::Complete => return None,
        };
        Some(format!(
            "native_goal_control:{}",
            serde_json::to_string(&(self.session_id.as_ref()?, &goal.id, goal.revision, pause))
                .ok()?
        ))
    }
}
