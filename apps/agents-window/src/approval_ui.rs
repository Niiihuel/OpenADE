//! Approval presentation contains no execution or permission-policy decisions.
//! Each action carries its owner, workspace and request. The host revalidates
//! that scope against the live pending request before resolving it.

use crate::{
    theme,
    view::{PermissionView, SubagentPermissionView, ViewState},
};
use openide_ui::{
    ApprovalRequest, ButtonGroup, Component, ControlSize, Element, Icon, Modal, Text, TextRole,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_SUMMARY_INPUT: usize = 256 * 1024;
const MAX_TARGET_BYTES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ApprovalTarget {
    pub owner_session_id: String,
    pub workspace: String,
    pub child_session_id: Option<String>,
    pub request_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ApprovalSummary {
    pub title: String,
    pub target: String,
    pub icon: Icon,
    /// A text action of the card, as (action, label).
    pub link: Option<(&'static str, &'static str)>,
}

impl ApprovalTarget {
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
    let (mut title, mut icon, keys): (&str, Icon, &[&str]) = match canonical
    {
        "delete_file" => (
            "Eliminar archivo",
            Icon::Trash,
            &["path", "file_path"],
        ),
        "write_file" => (
            "Escribir archivo",
            Icon::File,
            &["path", "file_path"],
        ),
        "edit_file" => (
            "Editar archivo",
            Icon::Edit,
            &["path", "file_path"],
        ),
        "apply_patch" => (
            "Aplicar cambios en archivos",
            Icon::Edit,
            &[],
        ),
        "read_file" | "batch_read" => (
            "Leer archivo",
            Icon::File,
            &["path", "file_path"],
        ),
        "rename_file" => (
            "Mover archivo",
            Icon::File,
            &["from", "source", "path"],
        ),
        "run_command" | "exec_command" | "start_command" => (
            "Ejecutar comando",
            Icon::Terminal,
            &["command", "cmd"],
        ),
        "write_stdin" | "terminal_send" => (
            "Enviar entrada a un comando",
            Icon::Terminal,
            &["command_id", "session_id"],
        ),
        "terminate_command" => (
            "Detener comando",
            Icon::Stop,
            &["command_id", "session_id"],
        ),
        "web_fetch" | "browser_open" | "browser_navigate" => (
            "Abrir página web",
            Icon::Globe,
            &["url"],
        ),
        "web_search" => (
            "Buscar en la web",
            Icon::Search,
            &["query", "q"],
        ),
        crate::image_runtime::TOOL => (
            "Generar imagen",
            Icon::Image,
            &["path"],
        ),
        "browser_click" | "browser_hover" | "browser_select_option" | "browser_check"
        | "browser_drag" => (
            "Interactuar con una página",
            Icon::Globe,
            &["ref", "selector", "name", "role"],
        ),
        "browser_fill" | "browser_type" => (
            "Escribir en una página",
            Icon::Edit,
            &["ref", "selector", "name", "role"],
        ),
        // As with a command, the user approves the script itself.
        "browser_evaluate" => (
            "Ejecutar JavaScript en la página",
            Icon::Code,
            &["expression"],
        ),
        "browser_handle_dialog" => (
            "Responder un diálogo de la página",
            Icon::Globe,
            &[],
        ),
        _ if tool_name.starts_with("mcp__") => (
            "Usar conexión MCP",
            Icon::Grid,
            &[],
        ),
        _ => (
            "Usar herramienta",
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
                icon = Icon::Trash;
            }
            [0, 0, count] if count > 1 => {
                title = "Eliminar archivos";
                icon = Icon::Trash;
            }
            [1, 0, 0] => {
                title = "Crear archivo";
                icon = Icon::File;
            }
            [count, 0, 0] if count > 1 => {
                title = "Crear archivos";
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
        icon,
        link: None,
    }
}

/// An image call says what it sends and that it may cost. The model and the
/// quality are the user's own, read when the card is drawn: the call reads
/// the same saved choice when it runs.
fn with_image(mut summary: ApprovalSummary, tool_name: &str, arguments: &str, state: &ViewState) -> ApprovalSummary {
    use crate::image_runtime::{image_choice, quality_label, PROVIDER, TOOL};
    if tool_name != TOOL {
        return summary;
    }
    let arguments = (arguments.len() <= MAX_SUMMARY_INPUT)
        .then(|| serde_json::from_str::<Value>(arguments).ok())
        .flatten()
        .unwrap_or_default();
    let text = |key: &str| arguments.get(key).and_then(Value::as_str);
    let size = text("size")
        .filter(|size| *size != "auto")
        .unwrap_or("tamaño automático");
    let (model, quality) = image_choice(&state.image_model, &state.image_quality);
    let (prompt, clipped) = clip(
        &text("prompt").unwrap_or_default().replace(['\r', '\n', '\t'], " "),
        600,
    );
    // The prompt goes last: it is what a long card cuts.
    let target = format!(
        "{} · {size} · calidad {} · {PROVIDER} {} · puede tener costo — «{prompt}{}»",
        summary.target,
        quality_label(quality).to_lowercase(),
        model.label,
        if clipped { "…" } else { "" },
    );
    summary.target = clip(&target, MAX_TARGET_BYTES).0;
    summary.link = Some(("image_settings_open", "Cambiar modelo o calidad"));
    summary
}

/// A browser call that names its element by ref says what the ref is: the
/// role and name the agent's last snapshot printed for it. A ref that
/// snapshot does not hold stays as it was written.
fn with_element(mut summary: ApprovalSummary, tool_name: &str, arguments: &str, snapshot: &str) -> ApprovalSummary {
    let reference = tool_name
        .starts_with("mcp__openide_browser__")
        .then(|| serde_json::from_str::<Value>(arguments).ok())
        .flatten()
        .and_then(|arguments| arguments.get("ref")?.as_str().map(str::to_owned))
        .filter(|reference| *reference == summary.target);
    let Some((role, name)) = reference
        .as_deref()
        .and_then(|reference| openide_browser::element_of(snapshot, reference))
    else {
        return summary;
    };
    let role = match role.as_str() {
        "button" => "botón",
        "link" => "enlace",
        "textbox" => "campo de texto",
        "searchbox" => "campo de búsqueda",
        "spinbutton" => "campo numérico",
        "checkbox" => "casilla",
        "radio" => "opción",
        "switch" => "interruptor",
        "slider" => "deslizador",
        "combobox" => "lista desplegable",
        "listbox" => "lista",
        "option" => "opción de lista",
        "menuitem" => "elemento de menú",
        "tab" => "pestaña",
        "heading" => "encabezado",
        "paragraph" => "párrafo",
        "img" => "imagen",
        "iframe" => "marco",
        "listitem" => "elemento de lista",
        "row" => "fila",
        "cell" => "celda",
        "dialog" => "diálogo",
        "generic" => "elemento",
        other => other,
    };
    // The name is the page's: one line of it, and never the whole card.
    let (name, clipped) = clip(&name.replace(['\r', '\n', '\t'], " "), 200);
    summary.target = match (name.is_empty(), clipped) {
        (true, _) => format!("{role} ({})", summary.target),
        (false, false) => format!("{role} \"{name}\" ({})", summary.target),
        (false, true) => format!("{role} \"{name}…\" ({})", summary.target),
    };
    summary
}

fn notice(
    target: &ApprovalTarget,
    summary: ApprovalSummary,
    status: String,
    compact: bool,
) -> Element {
    let request = ApprovalRequest::new(
        summary.title,
        target.decision_action("deny"),
        target.decision_action("allow_session"),
        target.decision_action("allow_once"),
        theme::component_palette(),
    )
    .stacked(compact)
    .target(summary.target)
    .status(status)
    .icon(summary.icon);
    match summary.link {
        Some((action, label)) => request.link(action, label),
        None => request,
    }
    .render()
}

pub(crate) fn inline_root(permission: &PermissionView, state: &ViewState, compact: bool) -> Element {
    let summary = with_element(
        summary(&permission.tool_name, &permission.arguments),
        &permission.tool_name,
        &permission.arguments,
        &state.browser_refs,
    );
    notice(
        &root_target(permission, state),
        with_image(summary, &permission.tool_name, &permission.arguments, state),
        "Aprobación pendiente".into(),
        compact,
    )
}

pub(crate) fn inline_child(
    permission: &SubagentPermissionView,
    state: &ViewState,
    compact: bool,
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
        compact,
    )
}

/// Decision dialog for one tool call from an external CLI. Dismissing it
/// denies the call.
pub(crate) fn external_modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    let Some(request) = &state.external_approval else {
        return base;
    };
    let deny = crate::settings_ui::outline("external_approval_deny", "Denegar").render();
    let allow = theme::primary("external_approval_allow", "Permitir una vez")
        .size(ControlSize::Compact)
        .render();
    Modal::new(
        base,
        true,
        &request.title,
        "La llamada espera tu decisión; sin respuesta se deniega.",
        "external_approval_deny",
        theme::component_palette(),
    )
    .width(Modal::fit_width(viewport_width, 480.0))
    .body(
        Text::new(&request.detail, theme::card_palette())
            .role(TextRole::Small)
            .muted()
            .render(),
    )
    .footer(
        ButtonGroup::new(vec![deny, allow])
            .align_end()
            .sizing(crate::ui_sizing::CONFIRM_FOOTER)
            .render(),
    )
    .initial_focus("external_approval_deny")
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, FontFamily, Frame, HitKind, Point, Size, UiAction, UiEvent, UiTree};
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
            assert!(!format!("{}{}", result.title, result.target).contains("PRIVATE_FILE_CONTENT"));
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
    fn browser_tools_ask_with_the_element_or_the_script_they_act_on() {
        let browser = |tool: &str, arguments: Value| {
            let result = summary(&format!("mcp__openide_browser__{tool}"), &arguments.to_string());
            (result.title, result.target)
        };
        // The user approves the script itself, as with a command.
        assert_eq!(
            browser("browser_evaluate", json!({"expression":"() => document.cookie", "ref":"e3"})),
            ("Ejecutar JavaScript en la página".into(), "() => document.cookie".into())
        );
        for tool in ["browser_click", "browser_hover", "browser_select_option", "browser_check"] {
            assert_eq!(
                browser(tool, json!({"ref":"e12", "values":["PRIVATE_FILE_CONTENT"]})),
                ("Interactuar con una página".into(), "e12".into()),
                "{tool}"
            );
        }
        assert_eq!(
            browser("browser_fill", json!({"ref":"e5", "text":"PRIVATE_FILE_CONTENT"})),
            ("Escribir en una página".into(), "e5".into())
        );
        assert_eq!(
            browser("browser_drag", json!({"from":{"ref":"e2"}, "to":{"ref":"e3"}})),
            ("Interactuar con una página".into(), "openide_browser · browser_drag".into())
        );
        assert_eq!(
            browser("browser_handle_dialog", json!({"accept":true, "prompt_text":"PRIVATE_FILE_CONTENT"})),
            (
                "Responder un diálogo de la página".into(),
                "openide_browser · browser_handle_dialog".into()
            )
        );
        // Tools that only read keep the generic connection summary.
        assert_eq!(
            browser("browser_network_requests", json!({})),
            ("Usar conexión MCP".into(), "openide_browser · browser_network_requests".into())
        );
        // Another server's tool with the same suffix is not a browser script.
        assert_eq!(
            summary("mcp__other__browser_evaluate", r#"{"expression":"1"}"#).title,
            "Usar conexión MCP"
        );
    }

    #[test]
    fn an_image_call_asks_with_what_it_will_send_and_the_models_the_user_saved() {
        let name = crate::image_runtime::TOOL;
        let mut state = fixture();
        let card = |state: &ViewState, arguments: Value| {
            let arguments = arguments.to_string();
            with_image(summary(name, &arguments), name, &arguments, state)
        };
        let asked = card(
            &state,
            json!({"prompt":"Un faro\nal amanecer","path":"assets/hero.png","size":"1536x1024"}),
        );
        assert_eq!((asked.title.as_str(), asked.icon), ("Generar imagen", Icon::Image));
        assert_eq!(
            asked.target,
            "assets/hero.png · 1536x1024 · calidad media · OpenAI GPT Image 2 · puede tener costo — «Un faro al amanecer»"
        );
        assert_eq!(asked.link, Some(("image_settings_open", "Cambiar modelo o calidad")));
        // The saved choice is what the card names, as the call will send it:
        // a level the model lacks reads as its highest.
        state.image_model = "gpt-image-2.5-flare".into();
        state.image_quality = "max".into();
        let arguments = json!({"prompt":"x".repeat(4000),"path":"a.png"});
        let asked = card(&state, arguments.clone());
        assert!(asked.target.starts_with("a.png · tamaño automático · calidad máxima · OpenAI GPT Image 2.5 Flare · puede tener costo — «xxx"));
        assert!(asked.target.ends_with("x…»") && asked.target.len() < 800, "{}", asked.target);
        state.image_model = "gpt-image-1-mini".into();
        assert!(card(&state, arguments).target.contains("· calidad alta · OpenAI GPT Image 1 mini ·"));
        // No other tool gets the link, and the card offers it as an action.
        assert_eq!(summary("write_file", r#"{"path":"a.png"}"#).link, None);
        state.pending_permissions[0].tool_name = name.into();
        state.pending_permissions[0].arguments = json!({"prompt":"Un faro","path":"faro.png"}).to_string();
        for width in [350.0, 900.0] {
            let mut tree = UiTree::new(inline_root(&state.pending_permissions[0], &state, width < 470.0));
            let frame = tree.layout(Size::new(width, 600.0));
            // A narrow card wraps the target: its words are all there.
            for word in ["faro.png", "alta", "mini", "costo"] {
                assert!(texts(&frame).contains(word), "{word} at {width}");
            }
            assert_eq!(
                click(&mut tree, &frame, "image_settings_open"),
                [UiAction::Clicked { id: "image_settings_open".into() }]
            );
            for region in frame.regions.iter().filter(|region| region.kind == HitKind::Button) {
                assert!(region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width);
            }
        }
    }

    #[test]
    fn a_browser_ref_is_shown_with_the_role_and_name_the_last_snapshot_gave_it() {
        let snapshot = "- heading \"Cuenta\" [level=1] [ref=e1]\n- button \"Guardar\" [ref=e12] [cursor=pointer]\n- textbox [ref=e5]: ada@example.test\n- button \"Cancelar\\\" [ref=e13]\" [ref=e40]\n- tree \"Archivos\" [ref=e41]";
        let card = |tool: &str, arguments: Value| {
            let name = format!("mcp__openide_browser__{tool}");
            let arguments = arguments.to_string();
            with_element(summary(&name, &arguments), &name, &arguments, snapshot).target
        };
        assert_eq!(card("browser_click", json!({"ref":"e12"})), "botón \"Guardar\" (e12)");
        assert_eq!(card("browser_fill", json!({"ref":"e5", "text":"PRIVATE_FILE_CONTENT"})), "campo de texto (e5)");
        assert_eq!(card("browser_hover", json!({"ref":"e1"})), "encabezado \"Cuenta\" (e1)");
        // A role without a word of its own keeps the snapshot's.
        assert_eq!(card("browser_click", json!({"ref":"e41"})), "tree \"Archivos\" (e41)");
        // Not in the last snapshot, or only inside another element's name: the bare ref.
        assert_eq!(card("browser_click", json!({"ref":"e99"})), "e99");
        assert_eq!(card("browser_click", json!({"ref":"e13"})), "e13");
        // The other ways of naming an element are shown as they were given.
        assert_eq!(card("browser_click", json!({"selector":"#guardar"})), "#guardar");
        assert_eq!(card("browser_evaluate", json!({"expression":"el => el.id", "ref":"e12"})), "el => el.id");
        // A long name is cut, and another server's tool is none of this.
        let long = format!("- button \"{}\" [ref=e2]", "x".repeat(400));
        let name = "mcp__openide_browser__browser_click";
        let cut = with_element(summary(name, r#"{"ref":"e2"}"#), name, r#"{"ref":"e2"}"#, &long).target;
        assert!(cut.starts_with("botón \"xxx") && cut.ends_with("…\" (e2)") && cut.len() < 240, "{cut}");
        let other = "mcp__other__browser_click";
        assert_eq!(with_element(summary(other, r#"{"ref":"e12"}"#), other, r#"{"ref":"e12"}"#, snapshot).target, "other · browser_click");
        // The card itself reads the snapshot the view was given.
        let mut state = fixture();
        state.browser_refs = snapshot.into();
        state.pending_permissions[0].tool_name = "mcp__openide_browser__browser_click".into();
        state.pending_permissions[0].arguments = json!({"ref":"e12"}).to_string();
        let frame = UiTree::new(inline_root(&state.pending_permissions[0], &state, false)).layout(Size::new(900.0, 600.0));
        assert!(texts(&frame).contains("botón \"Guardar\" (e12)"), "{}", texts(&frame));
    }

    #[test]
    fn pending_scope_requires_owner_workspace_and_matching_child_parent_binding() {
        let mut state = fixture();
        let mut target = root_target(&state.pending_permissions[0], &state);
        assert!(target_is_pending(&state, &target));
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

        state.pending_subagent_permissions[0] = child.clone();
        state.selected_session = Some(child.child_session_id.clone());
        let from_child = child_target(&child, &state);
        assert_eq!(from_child.owner_session_id, child.child_session_id);
        assert!(target_is_pending(&state, &from_child));
        state.selected_session = Some("unrelated-session".into());
        assert!(!target_is_pending(&state, &from_child));
    }

    #[test]
    fn inline_card_carries_the_three_scoped_decisions_without_focus_or_protocol_fields() {
        let mut state = fixture();
        state.pending_permissions[0].tool_name = "run_command".into();
        state.pending_permissions[0].arguments = json!({
            "command": format!("cargo test {} ultimo_argumento", "--workspace ".repeat(30)),
            "reason": "PRIVATE_PROTOCOL_FIELD",
        })
        .to_string();
        let target = root_target(&state.pending_permissions[0], &state);
        for width in [350.0, 900.0] {
            let mut tree = UiTree::new(inline_root(&state.pending_permissions[0], &state, width < 470.0));
            let frame = tree.layout(Size::new(width, 600.0));
            assert!(tree.focused_id().is_none());
            assert!(!texts(&frame).contains("PRIVATE_PROTOCOL_FIELD"));
            assert!(!texts(&frame).contains("Revisar"));
            assert!(
                texts(&frame).contains("ultimo_argumento"),
                "the card is the only place that shows the whole target"
            );
            let buttons: Vec<_> = frame
                .regions
                .iter()
                .filter(|region| region.kind == HitKind::Button)
                .collect();
            assert_eq!(buttons.len(), 3);
            for choice in ["allow_session", "deny", "allow_once"] {
                let id = target.decision_action(choice);
                let rect = frame.regions.iter().find(|region| region.id == id).unwrap().rect;
                assert!(rect.x >= 0.0 && rect.x + rect.width <= width);
                assert_eq!(click(&mut tree, &frame, &id), [UiAction::Clicked { id }]);
            }
        }
    }

    #[test]
    #[ignore = "exports native-font approval card and chat PNG/SVG fixtures under /tmp"]
    fn export_approval_visual_review() {
        use crate::transcript::{ActivityStatus, TranscriptItem};
        use eframe::egui;
        use openide_ui::FontMetrics;
        use std::path::Path;

        let directory = Path::new("/tmp/openade-approval-review");
        std::fs::create_dir_all(directory).unwrap();
        for width in [360.0, 900.0] {
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
                    "long-command",
                    "run_command",
                    json!({"command":"find /home/usuario/proyectos/aplicacion/target/debug/incremental -type f -name '*.o' -mtime +30 -print0 | xargs -0 rm -f && cargo build --release --workspace --all-features"}),
                ),
                (
                    "mcp",
                    "mcp__project_docs__publish",
                    json!({"document_id":"introduction", "visibility":"team"}),
                ),
                (
                    "generate-image",
                    crate::image_runtime::TOOL,
                    json!({"prompt":"Ilustración isométrica de un faro sobre un acantilado al amanecer, con niebla baja sobre el mar, paleta cálida y trazo limpio, sin texto", "path":"docs/assets/portada-faro.png", "size":"1536x1024"}),
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
                {
                    let element = Element::panel(crate::session_surface::build_session_surface(
                        &state, width, false, false,
                    ))
                    .background(theme::BACKGROUND);
                    let context = egui::Context::default();
                    crate::fonts::install(&context);
                    context.begin_pass(egui::RawInput::default());
                    let mut tree = UiTree::new(element);
                    tree.set_reduced_motion(true);
                    for family in [FontFamily::Monospace, FontFamily::Sans] {
                        let font = match family {
                            FontFamily::Monospace => egui::FontId::monospace(14.0),
                            FontFamily::Sans | FontFamily::SansItalic => egui::FontId::proportional(14.0),
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
                            FontFamily::Sans | FontFamily::SansItalic => tree.set_sans_metrics(metrics),
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
                                            FontFamily::Sans | FontFamily::SansItalic => egui::FontFamily::Proportional,
                                        },
                                    ),
                                    egui::Color32::WHITE,
                                )
                                .size()
                                .x
                        })
                    });
                    let frame = tree.layout(viewport);
                    let path = directory.join(format!("{scenario}-{width:.0}.svg"));
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
                    if scenario == "generate-image" {
                        crate::view::ide_visual_tests::export_shot(
                            &frame,
                            viewport,
                            "agent-tools",
                            &format!("image-approval-{width:.0}"),
                        );
                    }
                    context.end_pass().textures_delta.clear();
                }
            }
        }
    }
}
