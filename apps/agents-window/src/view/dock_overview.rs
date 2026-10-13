//! What a tab can be opened on, as the empty dock offers it.
use super::*;

/// What this conversation has right now that a tab can show, most telling
/// first: a server the agent started, what the chat changed, the
/// repository's changes, the plan. Nothing when there is none of it.
pub(super) fn suggestions(state: &ViewState, width: f32) -> Option<Element> {
    let counted = |count: usize, one: &str, many: &str| {
        if count == 1 { format!("1 {one}") } else { format!("{count} {many}") }
    };
    let mut rows: Vec<(String, Icon, &str, String)> = Vec::new();
    if let Some((agent, url)) = crate::environment_ui::preview(state).and_then(|agent| Some((agent, agent.preview.as_deref()?))) {
        rows.push((format!("agent_preview_open:{}", agent.command_id), Icon::Globe, "Vista previa", url.to_owned()));
    }
    let changes = &state.environment.changes;
    if changes.files > 0 {
        rows.push(("dock_open:changes".into(), Icon::Code, "Cambios del chat", counted(changes.files, "archivo", "archivos")));
    }
    if let Some(Ok(review)) = &state.git_review {
        if review.files > 0 {
            rows.push(("dock_open:review".into(), Icon::Code, "Cambios de Git", counted(review.files, "archivo", "archivos")));
        }
    }
    if let Some(plan) = state.selected_session.as_ref().and_then(|session| state.session_plans.get(session)) {
        let done = plan.snapshot.plan.iter().filter(|step| step.status == openide_core::PlanStepStatus::Completed).count();
        if !plan.snapshot.plan.is_empty() {
            rows.push(("environment_open".into(), Icon::Plan, "Plan", format!("{done} de {}", plan.snapshot.plan.len())));
        }
    }
    if rows.is_empty() {
        return None;
    }
    let action_width = width.max(150.0);
    let palette = theme::action_palette(theme::CARD, theme::FOREGROUND);
    let mut column = vec![theme::text_px("Sugerencias", 11.0, theme::FOREGROUND_SUBTLE)];
    column.extend(rows.into_iter().map(|(id, icon, title, detail)| {
        ListRow::new(id, title, palette)
            .icon(icon)
            .icon_ink(theme::FOREGROUND)
            .subtitle(detail)
            .filled()
            .sizing(crate::ui_sizing::DOCK_OPEN)
            .render()
            .size(Some(action_width), None)
    }));
    Some(Element::column(column).gap(6.0))
}

pub(super) fn open_actions(width: f32) -> Element {
    let action_width = width.max(150.0);
    let palette = theme::action_palette(theme::CARD, theme::FOREGROUND);
    Element::column(
        [
            ("dock_open:files", Icon::Folder, "Archivos"),
            ("dock_open:activity", Icon::History, "Actividad"),
            ("dock_open:review", Icon::Code, "Cambios de Git"),
            ("dock_open:terminal", Icon::Terminal, "Terminal"),
            ("dock_open:browser", Icon::Globe, "Navegador"),
            ("dock_open:changes", Icon::Code, "Cambios del chat"),
        ]
        .into_iter()
        .map(|(id, icon, title)| {
            ListRow::new(id, title, palette)
                .icon(icon)
                .icon_ink(theme::FOREGROUND)
                .filled()
                .sizing(crate::ui_sizing::DOCK_OPEN)
                .render()
                .size(Some(action_width), crate::ui_sizing::DOCK_OPEN.height)
        })
        .collect(),
    )
    .gap(6.0)
}
