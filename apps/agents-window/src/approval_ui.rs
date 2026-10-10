//! Approval presentation contains no execution or permission-policy decisions.
//! Each action carries its owner, workspace and request. The host revalidates
//! that scope against the live pending request before resolving it.

use crate::{
    theme,
    view::{PermissionView, SubagentPermissionView, ViewState},
};
use openide_ui::{
    ApprovalRequest, Button, ButtonVariant, Card, Collapsible, Component, ControlSize, Element,
    FontFamily, Icon, Modal, RadiusRole, ScrollArea, TextSpan,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_SUMMARY_INPUT: usize = 256 * 1024;
const MAX_TARGET_BYTES: usize = 4096;
const MAX_DETAIL_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ApprovalTarget {
    pub owner_session_id: String,
    pub workspace: String,
    pub child_session_id: Option<String>,
    pub request_id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ApprovalUiState {
    pub selected: Option<ApprovalTarget>,
    pub details_open: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ApprovalSummary {
    pub title: String,
    pub target: String,
    pub description: String,
    pub icon: Icon,
}

impl ApprovalTarget {
    pub(crate) fn review_action(&self) -> String {
        format!(
            "approval_review:{}",
            serde_json::to_string(self).expect("approval scope serializes")
        )
    }

    pub(crate) fn decision_action(&self, choice: &str) -> String {
        format!(
            "approval_decide:{}",
            serde_json::to_string(&(
                &self.owner_session_id,
                &self.workspace,
                &self.child_session_id,
                &self.request_id,
                choice,
            ))
            .expect("approval decision serializes")
        )
    }
}

pub(crate) fn target_is_pending(state: &ViewState, target: &ApprovalTarget) -> bool {
    if state.selected_session.as_deref() != Some(target.owner_session_id.as_str())
        || state.workspace != target.workspace
        || target.owner_session_id.is_empty()
        || target.request_id.is_empty()
    {
        return false;
    }
    match target.child_session_id.as_deref() {
        None => state
            .pending_permissions
            .iter()
            .any(|permission| permission.request_id == target.request_id),
        Some(child) => state.pending_subagent_permissions.iter().any(|permission| {
            permission.child_session_id == child
                && (permission.parent_session_id == target.owner_session_id
                    || permission.child_session_id == target.owner_session_id)
                && permission.request_id == target.request_id
        }),
    }
}

fn root_target(permission: &PermissionView, state: &ViewState) -> ApprovalTarget {
    ApprovalTarget {
        owner_session_id: state.selected_session.clone().unwrap_or_default(),
        workspace: state.workspace.clone(),
        child_session_id: None,
        request_id: permission.request_id.clone(),
    }
}

fn child_target(permission: &SubagentPermissionView, state: &ViewState) -> ApprovalTarget {
    ApprovalTarget {
        owner_session_id: state
            .selected_session
            .as_ref()
            .filter(|owner| {
                *owner == &permission.parent_session_id || *owner == &permission.child_session_id
            })
            .cloned()
            .unwrap_or_else(|| permission.parent_session_id.clone()),
        workspace: state.workspace.clone(),
        child_session_id: Some(permission.child_session_id.clone()),
        request_id: permission.request_id.clone(),
    }
}

fn clip(value: &str, maximum: usize) -> (String, bool) {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), end != value.len())
}

fn identifying_value(arguments: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| arguments.get(*key)?.as_str())
        .map(|value| {
            let (value, truncated) = clip(value, MAX_TARGET_BYTES);
            let mut value = value.replace(['\r', '\n', '\t'], " ");
            if truncated {
                value.push('…');
            }
            value
        })
        .unwrap_or_default()
}

/// Describe only known top-level operation identifiers. Authored file content,
/// stdin and arbitrary nested MCP arguments never become the default summary.
pub(crate) fn summary(tool_name: &str, arguments: &str) -> ApprovalSummary {
    let parsed = if arguments.len() <= MAX_SUMMARY_INPUT {
        serde_json::from_str::<Value>(arguments)
            .ok()
            .filter(Value::is_object)
    } else {
        None
    };
    let arguments = parsed.as_ref().unwrap_or(&Value::Null);
    let canonical = tool_name
        .strip_prefix("mcp__openide_browser__")
        .unwrap_or(tool_name);
    let (mut title, mut description, mut icon, keys): (&str, &str, Icon, &[&str]) = match canonical
    {
        "delete_file" => (
            "Eliminar archivo",
            "El agente solicita eliminar este archivo.",
            Icon::Trash,
            &["path", "file_path"],
        ),
        "write_file" => (
            "Escribir archivo",
            "El agente solicita escribir el contenido propuesto en este archivo.",
            Icon::File,
            &["path", "file_path"],
        ),
        "edit_file" => (
            "Editar archivo",
            "El agente solicita modificar el contenido de este archivo.",
            Icon::Edit,
            &["path", "file_path"],
        ),
        "apply_patch" => (
            "Aplicar cambios en archivos",
            "El agente solicita aplicar un parche que puede agregar, modificar o eliminar archivos.",
            Icon::Edit,
            &[],
        ),
        "read_file" | "batch_read" => (
            "Leer archivo",
            "El agente solicita leer el contenido del archivo.",
            Icon::File,
            &["path", "file_path"],
        ),
        "rename_file" => (
            "Mover archivo",
            "El agente solicita mover o renombrar este archivo.",
            Icon::File,
            &["from", "source", "path"],
        ),
        "run_command" | "exec_command" | "start_command" => (
            "Ejecutar comando",
            "El agente solicita ejecutar este comando en el proyecto.",
            Icon::Terminal,
            &["command", "cmd"],
        ),
        "write_stdin" | "terminal_send" => (
            "Enviar entrada a un comando",
            "El agente solicita enviar datos a un comando en ejecución.",
            Icon::Terminal,
            &["command_id", "session_id"],
        ),
        "terminate_command" => (
            "Detener comando",
            "El agente solicita detener este comando.",
            Icon::Stop,
            &["command_id", "session_id"],
        ),
        "web_fetch" | "browser_open" | "browser_navigate" => (
            "Abrir página web",
            "El agente solicita consultar esta dirección.",
            Icon::Globe,
            &["url"],
        ),
        "web_search" => (
            "Buscar en la web",
            "El agente solicita realizar esta búsqueda.",
            Icon::Search,
            &["query", "q"],
        ),
        "browser_click" => (
            "Interactuar con una página",
            "El agente solicita activar un control de la página abierta.",
            Icon::Globe,
            &["selector", "name", "role"],
        ),
        "browser_fill" | "browser_type" => (
            "Escribir en una página",
            "El agente solicita introducir datos en este control del navegador.",
            Icon::Edit,
            &["selector", "name", "role"],
        ),
        _ if tool_name.starts_with("mcp__") => (
            "Usar conexión MCP",
            "El agente solicita ejecutar una operación de esta conexión.",
            Icon::Grid,
            &[],
        ),
        _ => (
            "Usar herramienta",
            "El agente está esperando tu aprobación para esta operación.",
            Icon::InfoCircle,
            &[],
        ),
    };
    let mut target = identifying_value(arguments, keys);
    if canonical == "rename_file" {
        let destination = identifying_value(arguments, &["to", "destination"]);
        if !destination.is_empty() {
            if !target.is_empty() {
                target.push_str(" → ");
            }
            target.push_str(&destination);
        }
    } else if canonical == "apply_patch" {
        let patch = arguments
            .get("patch")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut operations = [0usize; 3];
        let mut paths = Vec::new();
        for line in patch.lines() {
            for (kind, prefix) in ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
                .iter()
                .enumerate()
            {
                if let Some(path) = line.strip_prefix(prefix) {
                    operations[kind] += 1;
                    if paths.len() < 4 {
                        paths.push(path);
                    }
                }
            }
        }
        match operations {
            [0, 0, 1] => {
                title = "Eliminar archivo";
                description = "El agente solicita eliminar este archivo.";
                icon = Icon::Trash;
            }
            [0, 0, count] if count > 1 => {
                title = "Eliminar archivos";
                description = "El agente solicita eliminar estos archivos.";
                icon = Icon::Trash;
            }
            [1, 0, 0] => {
                title = "Crear archivo";
                description = "El agente solicita crear este archivo con el contenido propuesto.";
                icon = Icon::File;
            }
            [count, 0, 0] if count > 1 => {
                title = "Crear archivos";
                description = "El agente solicita crear estos archivos con el contenido propuesto.";
                icon = Icon::File;
            }
            _ => {}
        }
        target = paths.iter().take(3).copied().collect::<Vec<_>>().join(", ");
        if paths.len() > 3 {
            target.push_str(" y más archivos");
        }
        target = clip(&target, MAX_TARGET_BYTES).0;
    }
    if target.is_empty() {
        target = clip(tool_name, 256).0;
        if let Some(mcp) = tool_name.strip_prefix("mcp__") {
            if let Some((server, operation)) = mcp.split_once("__") {
                target = clip(&format!("{server} · {operation}"), 256).0;
            }
        }
    }
    ApprovalSummary {
        title: title.into(),
        target,
        description: description.into(),
        icon,
    }
}

fn notice(
    target: &ApprovalTarget,
    summary: ApprovalSummary,
    status: String,
    location: &str,
) -> Element {
    let action = target.review_action();
    ApprovalRequest::new(&action, summary.title, theme::component_palette())
        .identity(format!("{action}:{location}"))
        .compact(location == "banner")
        .target(summary.target)
        .status(status)
        .icon(summary.icon)
        .render()
}

pub(crate) fn inline_root(permission: &PermissionView, state: &ViewState) -> Element {
    notice(
        &root_target(permission, state),
        summary(&permission.tool_name, &permission.arguments),
        "Aprobación pendiente".into(),
        "inline",
    )
}

pub(crate) fn inline_child(
    permission: &SubagentPermissionView,
    state: &ViewState,
    _width: f32,
) -> Element {
    let requester = state
        .active_subagents
        .iter()
        .find(|child| {
            child.child_session_id == permission.child_session_id
                && child.parent_session_id == permission.parent_session_id
        })
        .map(|child| child.title.as_str())
        .unwrap_or("Subagente");
    notice(
        &child_target(permission, state),
        summary(&permission.tool_name, &permission.arguments),
        format!("Aprobación pendiente · {requester}"),
        "inline",
    )
}

pub(crate) fn pending_banner(state: &ViewState) -> Option<Element> {
    let root = state.pending_permissions.first();
    let children = state
        .pending_subagent_permissions
        .iter()
        .filter(|permission| {
            Some(permission.parent_session_id.as_str()) == state.selected_session.as_deref()
                || Some(permission.child_session_id.as_str()) == state.selected_session.as_deref()
        })
        .collect::<Vec<_>>();
    let count = state.pending_permissions.len() + children.len();
    let status = if count > 1 {
        format!("{count} aprobaciones pendientes")
    } else {
        "Aprobación pendiente".into()
    };
    if let Some(permission) = root {
        let target = root_target(permission, state);
        return target_is_pending(state, &target).then(|| {
            notice(
                &target,
                summary(&permission.tool_name, &permission.arguments),
                status,
                "banner",
            )
        });
    }
    children.first().and_then(|permission| {
        let target = child_target(permission, state);
        target_is_pending(state, &target).then(|| {
            notice(
                &target,
                summary(&permission.tool_name, &permission.arguments),
                status,
                "banner",
            )
        })
    })
}

fn paragraph(value: impl Into<String>, muted: bool, mono: bool) -> Element {
    Element::formatted_text(vec![TextSpan::new(
        value.into(),
        if muted {
            theme::FOREGROUND_SUBTLE
        } else {
            theme::FOREGROUND
        },
        if muted && !mono { 11.0 } else { 12.0 },
        if mono {
            FontFamily::Monospace
        } else {
            FontFamily::Sans
        },
    )])
}

fn selected_permission<'a>(
    state: &'a ViewState,
    target: &ApprovalTarget,
) -> Option<(&'a str, &'a str)> {
    if !target_is_pending(state, target) {
        return None;
    }
    match target.child_session_id.as_deref() {
        None => state
            .pending_permissions
            .iter()
            .find(|permission| permission.request_id == target.request_id)
            .map(|permission| (permission.tool_name.as_str(), permission.arguments.as_str())),
        Some(child) => state
            .pending_subagent_permissions
            .iter()
            .find(|permission| {
                permission.child_session_id == child
                    && (permission.parent_session_id == target.owner_session_id
                        || permission.child_session_id == target.owner_session_id)
                    && permission.request_id == target.request_id
            })
            .map(|permission| (permission.tool_name.as_str(), permission.arguments.as_str())),
    }
}

pub(crate) fn modal(state: &ViewState, base: Element, width: f32, height: f32) -> Element {
    let Some(target) = state.approval_ui.selected.as_ref() else {
        return base;
    };
    let Some((name, arguments)) = selected_permission(state, target) else {
        return base;
    };
    let summary = summary(name, arguments);
    let palette = theme::component_palette();
    // What will happen, in the user's terms, and the exact target.
    let mut sections = vec![
        paragraph(summary.description, false, false),
        Card::new(paragraph(&summary.target, false, true), theme::panel_card_palette())
            .border(false)
            .radius(RadiusRole::Surface)
            .render(),
    ];
    if let Some(child) = target.child_session_id.as_deref() {
        let requester = state
            .active_subagents
            .iter()
            .find(|entry| entry.child_session_id == child)
            .map(|entry| entry.title.as_str())
            .unwrap_or("Subagente");
        sections.push(paragraph(format!("Solicita: {requester}"), true, false));
    }
    let details = if state.approval_ui.details_open {
        let (raw, truncated) = clip(arguments, MAX_DETAIL_BYTES);
        let mut rows = vec![
            paragraph(format!("Herramienta: {name}"), true, false),
            paragraph(raw, true, true),
        ];
        if truncated {
            rows.push(paragraph("Vista limitada a 16 KiB. La decisión se aplica a los argumentos completos de esta solicitud.", true, false));
        }
        ScrollArea::new("approval_protocol_scroll", Element::column(rows).gap(8.0))
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .size(None, Some(200.0))
    } else {
        Element::spacer()
    };
    sections.push(
        Collapsible::new("approval_details", "Detalles técnicos", details, palette)
            .expanded(state.approval_ui.details_open)
            .icon(Icon::Code)
            .render(),
    );
    // The lasting choice sits apart from the two answers to this request.
    let footer = Element::row(vec![
        Button::new(
            target.decision_action("allow_session"),
            "Permitir en esta sesión",
            palette,
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .tooltip("Autoriza esta misma operación, con los mismos argumentos y contexto, durante esta sesión")
        .render(),
        Element::spacer().flex(1.0),
        Button::new(target.decision_action("deny"), "Denegar", palette)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .render(),
        Button::new(
            target.decision_action("allow_once"),
            "Permitir una vez",
            palette,
        )
        .size(ControlSize::Compact)
        .render(),
    ])
    .gap(6.0);
    let requested_by_child = target.child_session_id.is_some();
    Modal::new(base, true, summary.title, "", "approval_close", palette)
        .width((width - 24.0).clamp(240.0, 480.0))
        .height((height - 32.0).clamp(
            200.0,
            if state.approval_ui.details_open {
                520.0
            } else if width < 500.0 {
                300.0
            } else if requested_by_child {
                246.0
            } else {
                220.0
            },
        ))
        .body(Element::column(sections).gap(10.0))
        .footer(footer)
        .initial_focus("approval_close:close")
        .backdrop_dismiss(false)
        .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, Frame, HitKind, Key, Point, Size, UiAction, UiEvent, UiTree};
    use serde_json::json;

    fn fixture() -> ViewState {
        let mut state = crate::view::tests::empty_state();
        state.selected_session = Some("parent:α".into());
        state.workspace = "/project/con espacios".into();
        state.pending_permissions = vec![PermissionView {
            request_id: "request:1".into(),
            tool_name: "delete_file".into(),
            arguments: json!({"path":"src/borrador.md", "reason":"PRIVATE_PROTOCOL_FIELD"})
                .to_string(),
        }];
        state
    }

    fn texts(frame: &Frame) -> String {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn key(key: Key) -> UiEvent {
        UiEvent::Key {
            key,
            shift: false,
            ctrl: false,
        }
    }

    fn click(tree: &mut UiTree, frame: &Frame, id: &str) -> Vec<UiAction> {
        let rect = frame
            .regions
            .iter()
            .find(|region| region.id == id && region.kind == HitKind::Button)
            .unwrap_or_else(|| panic!("missing actionable control {id}"))
            .rect;
        let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(point));
        tree.handle_event(UiEvent::PointerUp(point))
    }

    #[test]
    fn operation_summary_uses_known_identifiers_and_never_infers_nested_mcp_content() {
        for (tool, arguments, title, target) in [
            (
                "delete_file",
                json!({"path":"src/obsoleto.rs"}),
                "Eliminar archivo",
                "src/obsoleto.rs",
            ),
            (
                "write_file",
                json!({"path":"src/lib.rs", "content":"PRIVATE_FILE_CONTENT"}),
                "Escribir archivo",
                "src/lib.rs",
            ),
            (
                "run_command",
                json!({"command":"cargo test --workspace"}),
                "Ejecutar comando",
                "cargo test --workspace",
            ),
            (
                "web_fetch",
                json!({"url":"https://example.org/guide"}),
                "Abrir página web",
                "https://example.org/guide",
            ),
            (
                "mcp__docs__publish",
                json!({"payload":{"path":"PRIVATE_FILE_CONTENT"}}),
                "Usar conexión MCP",
                "docs · publish",
            ),
            (
                "write_stdin",
                json!({"session_id":"terminal-1", "chars":"PRIVATE_FILE_CONTENT"}),
                "Enviar entrada a un comando",
                "terminal-1",
            ),
        ] {
            let result = summary(tool, &arguments.to_string());
            assert_eq!(result.title, title);
            assert_eq!(result.target, target);
            assert!(
                !format!("{}{}{}", result.title, result.target, result.description)
                    .contains("PRIVATE_FILE_CONTENT")
            );
        }
        assert_eq!(
            summary("write_file", r#"{"nested":{"path":"not-the-target"}}"#).target,
            "write_file"
        );
        assert_eq!(
            summary("delete_file", r#"{"path":"unfinished"#).target,
            "delete_file"
        );
        for (patch, title, icon) in [
            (
                "*** Begin Patch\n*** Delete File: notes.md\n*** End Patch",
                "Eliminar archivo",
                Icon::Trash,
            ),
            (
                "*** Begin Patch\n*** Delete File: notes.md\n*** Delete File: old.md\n*** End Patch",
                "Eliminar archivos",
                Icon::Trash,
            ),
            (
                "*** Begin Patch\n*** Add File: notes.md\n+Hola\n*** End Patch",
                "Crear archivo",
                Icon::File,
            ),
            (
                "*** Begin Patch\n*** Delete File: old.md\n*** Add File: notes.md\n+Hola\n*** End Patch",
                "Aplicar cambios en archivos",
                Icon::Edit,
            ),
        ] {
            let summary = summary("apply_patch", &json!({"patch":patch}).to_string());
            assert_eq!(summary.title, title);
            assert_eq!(summary.icon, icon);
            assert!(summary.target.contains("notes.md"));
        }
    }

    #[test]
    fn pending_scope_requires_owner_workspace_and_matching_child_parent_binding() {
        let mut state = fixture();
        let mut target = root_target(&state.pending_permissions[0], &state);
        assert!(target_is_pending(&state, &target));
        let encoded = target.review_action();
        assert_eq!(
            serde_json::from_str::<ApprovalTarget>(
                encoded.strip_prefix("approval_review:").unwrap()
            )
            .unwrap(),
            target
        );
        target.workspace.push_str("/other");
        assert!(!target_is_pending(&state, &target));
        target.workspace = state.workspace.clone();
        target.owner_session_id = "other-owner".into();
        assert!(!target_is_pending(&state, &target));
        let child = SubagentPermissionView {
            child_session_id: "child:β".into(),
            parent_session_id: "parent:α".into(),
            request_id: "request:1".into(),
            tool_name: "write_file".into(),
            arguments: "{}".into(),
        };
        state.pending_subagent_permissions.push(child.clone());
        let target = child_target(&child, &state);
        assert!(target_is_pending(&state, &target));
        state.pending_subagent_permissions[0].parent_session_id = "different-parent".into();
        assert!(!target_is_pending(&state, &target));
        state.pending_permissions.clear();
        assert!(pending_banner(&state).is_none());

        state.pending_subagent_permissions[0] = child.clone();
        state.selected_session = Some(child.child_session_id.clone());
        let from_child = child_target(&child, &state);
        assert_eq!(from_child.owner_session_id, child.child_session_id);
        assert!(target_is_pending(&state, &from_child));
        assert!(pending_banner(&state).is_some());
        state.selected_session = Some("unrelated-session".into());
        assert!(!target_is_pending(&state, &from_child));
        assert!(pending_banner(&state).is_none());
    }

    #[test]
    fn inline_and_persistent_notice_share_scoped_review_without_focus_or_decisions() {
        let state = fixture();
        for width in [350.0, 900.0] {
            let target = root_target(&state.pending_permissions[0], &state);
            let action = target.review_action();
            let mut tree = UiTree::new(
                Element::column(vec![
                    inline_root(&state.pending_permissions[0], &state),
                    pending_banner(&state).unwrap(),
                ])
                .gap(16.0),
            );
            let frame = tree.layout(Size::new(width, 400.0));
            assert!(tree.focused_id().is_none());
            assert!(!texts(&frame).contains("PRIVATE_PROTOCOL_FIELD"));
            assert!(!texts(&frame).contains("Denegar"));
            let buttons: Vec<_> = frame
                .regions
                .iter()
                .filter(|region| region.kind == HitKind::Button)
                .collect();
            assert_eq!(buttons.len(), 2);
            assert_ne!(buttons[0].id, buttons[1].id);
            for location in ["inline", "banner"] {
                assert_eq!(
                    click(&mut tree, &frame, &format!("{action}:{location}")),
                    [UiAction::Clicked { id: action.clone() }]
                );
            }
        }
    }

    #[test]
    fn decision_dialog_keeps_dismiss_separate_and_details_opt_in_at_narrow_widths() {
        for viewport in [Size::new(350.0, 500.0), Size::new(900.0, 800.0)] {
            let mut state = fixture();
            let target = root_target(&state.pending_permissions[0], &state);
            state.approval_ui.selected = Some(target.clone());
            let base = || Element::button("prompt", "Continuar escribiendo");
            let mut tree = UiTree::new(base());
            tree.set_reduced_motion(true);
            tree.layout(viewport);
            assert!(tree.focus("prompt"));
            tree.set_root(modal(&state, base(), viewport.width, viewport.height));
            let frame = tree.layout(viewport);
            assert_eq!(tree.focused_id(), Some("approval_close:close"));
            assert!(!texts(&frame).contains("PRIVATE_PROTOCOL_FIELD"));
            assert!(texts(&frame).contains("src/borrador.md"));
            // Pressing Enter on arrival can only postpone the request.
            assert_eq!(
                tree.handle_event(key(Key::Enter)),
                [UiAction::Clicked {
                    id: "approval_close".into()
                }]
            );
            assert_eq!(
                tree.handle_event(key(Key::Escape)),
                [UiAction::Clicked {
                    id: "approval_close".into()
                }]
            );
            for choice in ["deny", "allow_once"] {
                let id = target.decision_action(choice);
                let region = frame.regions.iter().find(|region| region.id == id).unwrap();
                assert!(
                    region.rect.x >= 0.0 && region.rect.x + region.rect.width <= viewport.width
                );
                assert!(
                    region.rect.y >= 0.0 && region.rect.y + region.rect.height <= viewport.height
                );
                assert_eq!(region.rect.height, 28.0);
                assert_eq!(click(&mut tree, &frame, &id), [UiAction::Clicked { id }]);
            }
            assert_eq!(
                click(&mut tree, &frame, "approval_details"),
                [UiAction::Clicked {
                    id: "approval_details".into()
                }]
            );
            state.approval_ui.details_open = true;
            tree.set_root(modal(&state, base(), viewport.width, viewport.height));
            let expanded = tree.layout(viewport);
            assert!(expanded.regions.iter().any(
                |region| region.id == "approval_protocol_scroll" && region.rect.height <= 200.0
            ));
            if viewport.width > 400.0 {
                assert!(texts(&expanded).contains("PRIVATE_PROTOCOL_FIELD"));
            }
            state.pending_permissions.clear();
            tree.set_root(modal(&state, base(), viewport.width, viewport.height));
            let resolved = tree.layout(viewport);
            assert!(
                !resolved
                    .regions
                    .iter()
                    .any(|region| region.id.starts_with("approval_decide:"))
            );
        }
    }

    #[test]
    #[ignore = "exports native-font approval dialog and chat PNG/SVG fixtures under /tmp"]
    fn export_approval_visual_review() {
        use crate::transcript::{ActivityStatus, TranscriptItem};
        use eframe::egui;
        use openide_ui::FontMetrics;
        use std::path::Path;

        let directory = Path::new("/tmp/openade-approval-review");
        std::fs::create_dir_all(directory).unwrap();
        for width in [350.0, 900.0] {
            let viewport = Size::new(width, 850.0);
            for (scenario, name, arguments) in [
                (
                    "delete-patch",
                    "apply_patch",
                    json!({"patch":"*** Begin Patch\n*** Delete File: docs/borrador.md\n*** End Patch\n"}),
                ),
                (
                    "command",
                    "run_command",
                    json!({"command":"cargo test --workspace", "timeout_ms":120000}),
                ),
                (
                    "mcp",
                    "mcp__project_docs__publish",
                    json!({"document_id":"introduction", "visibility":"team"}),
                ),
            ] {
                let mut state = fixture();
                state.selected_session_title = Some("Revisar los cambios del proyecto".into());
                state.prompt = "Conservar el resto de los archivos.".into();
                state.native_key_available = true;
                let mut route = crate::composer_ui::tests::configured_effort_route();
                route.execution = crate::composer_selection::ExecutionSupport::Verified;
                route.effort_options.clear();
                state.native_provider_id = route.provider_id.clone();
                state.model = route.model_id.clone();
                state.composer_model = route.model_id.clone();
                state.composer_routes = vec![route];
                state.pending_permissions[0].tool_name = name.into();
                state.pending_permissions[0].arguments =
                    serde_json::to_string_pretty(&arguments).unwrap();
                state.transcript = vec![
                    TranscriptItem::User {
                        content: "Aplicá el cambio y comprobá el resultado.".into(),
                        images: Vec::new(),
                    },
                    TranscriptItem::Assistant {
                        content:
                            "Preparé la operación. Está pendiente de tu aprobación para continuar."
                                .into(),
                        streaming: false,
                    },
                    TranscriptItem::Tool {
                        id: "approval-fixture-tool".into(),
                        name: name.into(),
                        arguments: arguments.clone(),
                        output: None,
                        is_error: false,
                        activity: ActivityStatus::WaitingPermission,
                    },
                ];
                for stage in ["notice", "dialog", "details"] {
                    state.approval_ui.selected = (stage != "notice")
                        .then(|| root_target(&state.pending_permissions[0], &state));
                    state.approval_ui.details_open = stage == "details";
                    let base = Element::panel(crate::session_surface::build_session_surface(
                        &state, width, false, false,
                    ))
                    .background(theme::BACKGROUND);
                    let element = modal(&state, base, width, viewport.height);
                    let context = egui::Context::default();
                    crate::fonts::install(&context);
                    context.begin_pass(egui::RawInput::default());
                    let mut tree = UiTree::new(element);
                    tree.set_reduced_motion(true);
                    for family in [FontFamily::Monospace, FontFamily::Sans] {
                        let font = match family {
                            FontFamily::Monospace => egui::FontId::monospace(14.0),
                            FontFamily::Sans => egui::FontId::proportional(14.0),
                        };
                        let metrics = context.fonts_mut(|fonts| FontMetrics {
                            char_width: fonts.glyph_width(
                                &font,
                                if family == FontFamily::Sans { 'a' } else { 'M' },
                            ),
                            line_height: fonts.row_height(&font),
                        });
                        match family {
                            FontFamily::Monospace => tree.set_metrics(metrics),
                            FontFamily::Sans => tree.set_sans_metrics(metrics),
                        }
                    }
                    let measure = context.clone();
                    tree.set_text_measurer(move |text, size, family| {
                        measure.fonts_mut(|fonts| {
                            fonts
                                .layout_no_wrap(
                                    text.into(),
                                    egui::FontId::new(
                                        size,
                                        match family {
                                            FontFamily::Monospace => egui::FontFamily::Monospace,
                                            FontFamily::Sans => egui::FontFamily::Proportional,
                                        },
                                    ),
                                    egui::Color32::WHITE,
                                )
                                .size()
                                .x
                        })
                    });
                    let frame = tree.layout(viewport);
                    let path = directory.join(format!("{scenario}-{stage}-{width:.0}.svg"));
                    crate::settings_ui::visual_qa::write_native_frame_svg(
                        &frame,
                        &path,
                        width,
                        viewport.height,
                    )
                    .unwrap();
                    let svg = std::fs::read_to_string(&path).unwrap();
                    let parsed = resvg::usvg::Tree::from_str(
                        &svg,
                        &crate::settings_ui::visual_qa::native_svg_options(),
                    )
                    .unwrap();
                    let mut pixmap =
                        resvg::tiny_skia::Pixmap::new(width as u32, viewport.height as u32)
                            .unwrap();
                    resvg::render(
                        &parsed,
                        resvg::tiny_skia::Transform::identity(),
                        &mut pixmap.as_mut(),
                    );
                    pixmap.save_png(path.with_extension("png")).unwrap();
                    context.end_pass().textures_delta.clear();
                }
            }
        }
    }
}
