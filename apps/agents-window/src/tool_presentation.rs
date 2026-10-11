//! Tool presentation shared by provisional input, transcript rows and inspector.
//!
//! Adapted to the Rust executor's names and states from OpenIDE's MIT-licensed
//! openideChatToolMeta/openideChatToolPresentation. Only complete, top-level
//! identifying fields become targets; authored content and stdin stay in detail.

use crate::transcript::ActivityStatus;
use openide_ui::Icon;
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolKind {
    File,
    Search,
    Edit,
    Command,
    Web,
    Agent,
    Mcp,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolNavigationTarget<'a> {
    File(&'a str),
    Url(&'a str),
}

/// Read the original argument, never the shortened visual target or a path
/// extracted from arbitrary prose. Only known tools expose navigation actions.
pub(crate) fn navigation_target<'a>(
    name: &str,
    arguments: &'a Value,
) -> Option<ToolNavigationTarget<'a>> {
    let name = name.strip_prefix("mcp__openide_browser__").unwrap_or(name);
    let (keys, file): (&[&str], bool) = match name {
        "read_file" | "write_file" | "edit_file" | "get_diagnostics" => {
            (&["path", "file_path"], true)
        }
        "rename_file" => (&["to", "destination"], true),
        "web_fetch" | "browser_open" | "browser_navigate" => (&["url"], false),
        _ => return None,
    };
    let target = keys.iter().find_map(|key| arguments.get(*key)?.as_str())?;
    if target.is_empty() || target.len() > 4096 || target.chars().any(char::is_control) {
        return None;
    }
    if file {
        Some(ToolNavigationTarget::File(target))
    } else {
        let url = reqwest::Url::parse(target).ok()?;
        (matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && !target.chars().any(char::is_whitespace))
        .then_some(ToolNavigationTarget::Url(target))
    }
}

pub(crate) fn navigation_action(owner: &str, call_id: &str) -> String {
    format!(
        "tool_navigate:{}",
        serde_json::to_string(&(owner, call_id)).expect("tool scope serializes")
    )
}

pub(crate) fn image_action(owner: &str, call_id: &str, index: usize, digest: &str) -> String {
    format!(
        "tool_image:{}",
        serde_json::to_string(&(owner, call_id, index, digest)).expect("image scope serializes")
    )
}

pub(crate) struct ToolPresentation {
    pub title: String,
    pub target: String,
    pub icon: Icon,
    pub kind: ToolKind,
    pub note: Option<String>,
}

struct Meta<'a> {
    operation: &'a str,
    running: &'a str,
    completed: &'a str,
    keys: &'static [&'static str],
    kind: ToolKind,
    icon: Icon,
}

fn meta(name: &str) -> Meta<'_> {
    use ToolKind::*;
    // The bundled browser is exposed through this exact native MCP namespace.
    // Other servers keep their complete tool name, even if a suffix coincides.
    let canonical = name.strip_prefix("mcp__openide_browser__").unwrap_or(name);
    let (operation, running, completed, keys, kind, icon): (_, _, _, &[_], _, _) = match canonical {
        "read_file" | "batch_read" => (
            "Lectura de archivo",
            "Leyendo",
            "Leído",
            &["path", "file_path", "operations"][..],
            File,
            Icon::File,
        ),
        "list_files" | "find_files" => (
            "Exploración de archivos",
            "Explorando",
            "Explorado",
            &["path", "pattern"],
            Search,
            Icon::Folder,
        ),
        "search_files" | "search_text" | "codebase_search" | "codebase_explore" => (
            "Búsqueda de contenido",
            "Buscando",
            "Buscado",
            &["query", "pattern"],
            Search,
            Icon::Search,
        ),
        "get_diagnostics" => (
            "Diagnósticos",
            "Comprobando",
            "Comprobado",
            &["path"],
            File,
            Icon::WarningTriangle,
        ),
        "write_file" => (
            "Escritura de archivo",
            "Escribiendo",
            "Escrito",
            &["path", "file_path"],
            Edit,
            Icon::File,
        ),
        "edit_file" => (
            "Edición de archivo",
            "Editando",
            "Editado",
            &["path", "file_path"],
            Edit,
            Icon::Edit,
        ),
        "apply_patch" => (
            "Aplicación de cambios",
            "Aplicando cambios",
            "Cambios aplicados",
            &[],
            Edit,
            Icon::Edit,
        ),
        "delete_file" => (
            "Eliminación de archivo",
            "Eliminando",
            "Eliminado",
            &["path"],
            Edit,
            Icon::Trash,
        ),
        "rename_file" => (
            "Movimiento de archivo",
            "Moviendo",
            "Movido",
            &["to", "destination"],
            Edit,
            Icon::File,
        ),
        "run_command" | "exec_command" => (
            "Ejecución de comando",
            "Ejecutando",
            "Ejecutado",
            &["command", "cmd"],
            Command,
            Icon::Terminal,
        ),
        "start_command" => (
            "Inicio de comando",
            "Iniciando comando",
            "Comando iniciado",
            &["command", "cmd"],
            Command,
            Icon::Terminal,
        ),
        "poll_command" => (
            "Consulta de comando",
            "Consultando comando",
            "Comando consultado",
            &["command_id", "session_id"],
            Command,
            Icon::Terminal,
        ),
        // Stdin may contain passwords; never use text/chars as a row target.
        "write_stdin" | "terminal_send" => (
            "Entrada al comando",
            "Enviando entrada",
            "Entrada enviada",
            &["command_id", "session_id"],
            Command,
            Icon::Terminal,
        ),
        "terminate_command" => (
            "Detención de comando",
            "Deteniendo comando",
            "Solicitud de detención enviada",
            &["command_id", "session_id"],
            Command,
            Icon::Stop,
        ),
        "resize_command" => (
            "Tamaño del terminal",
            "Ajustando terminal",
            "Terminal ajustado",
            &["command_id", "session_id"],
            Command,
            Icon::Terminal,
        ),
        "spawn_agent" | "delegate_task" => (
            "Inicio de subagente",
            "Delegando",
            "Subagente iniciado",
            &["title", "task_name", "agent_type"],
            Agent,
            Icon::Grid,
        ),
        "wait_agent" | "await_subagent" => (
            "Espera de subagente",
            "Esperando subagente",
            "Espera finalizada",
            &["child_session_id", "agent_id", "target"],
            Agent,
            Icon::Clock,
        ),
        "send_input" | "send_message" | "message_conversation" => (
            "Mensaje al subagente",
            "Enviando mensaje",
            "Mensaje enviado",
            &["child_session_id", "agent_id", "target", "to"],
            Agent,
            Icon::Chat,
        ),
        "cancel_agent" | "cancel_subagent" => (
            "Cancelación de subagente",
            "Cancelando subagente",
            "Cancelación solicitada",
            &["child_session_id", "agent_id", "target"],
            Agent,
            Icon::Stop,
        ),
        "list_agents" => (
            "Consulta de subagentes",
            "Consultando subagentes",
            "Subagentes consultados",
            &[],
            Agent,
            Icon::Grid,
        ),
        "web_search" => (
            "Búsqueda web",
            "Buscando en la web",
            "Búsqueda web realizada",
            &["query", "q"],
            Web,
            Icon::Search,
        ),
        "web_fetch" | "browser_open" | "browser_navigate" => (
            "Navegación web",
            "Abriendo página",
            "Página consultada",
            &["url"],
            Web,
            Icon::Globe,
        ),
        "browser_snapshot" | "browser_read_dom" => (
            "Lectura del navegador",
            "Leyendo página",
            "Página leída",
            &["selector"],
            Web,
            Icon::Globe,
        ),
        "browser_screenshot" => (
            "Captura del navegador",
            "Capturando página",
            "Página capturada",
            &["selector"],
            Web,
            Icon::Image,
        ),
        "browser_back" => (
            "Navegación anterior",
            "Volviendo atrás",
            "Página anterior",
            &[],
            Web,
            Icon::ArrowLeft,
        ),
        "browser_forward" => (
            "Navegación siguiente",
            "Avanzando",
            "Página siguiente",
            &[],
            Web,
            Icon::ArrowRight,
        ),
        "browser_reload" => (
            "Recarga del navegador",
            "Recargando página",
            "Página recargada",
            &[],
            Web,
            Icon::Refresh,
        ),
        "browser_click" => (
            "Interacción con página",
            "Pulsando",
            "Pulsado",
            &["selector", "name", "role"],
            Web,
            Icon::Globe,
        ),
        "browser_fill" | "browser_type" => (
            "Entrada en página",
            "Escribiendo en página",
            "Entrada enviada",
            &["selector", "name", "role"],
            Web,
            Icon::Edit,
        ),
        "browser_press" => (
            "Teclado del navegador",
            "Pulsando tecla",
            "Tecla pulsada",
            &["key"],
            Web,
            Icon::Globe,
        ),
        "browser_scroll" => (
            "Desplazamiento de página",
            "Desplazando página",
            "Página desplazada",
            &[],
            Web,
            Icon::Globe,
        ),
        "skill_view" | "load_skill" => (
            "Lectura de skill",
            "Cargando skill",
            "Skill cargada",
            &["name", "path"],
            Other,
            Icon::File,
        ),
        "skill_save" => (
            "Guardado de skill",
            "Guardando skill",
            "Skill guardada",
            &["name"],
            Other,
            Icon::File,
        ),
        "rule_manage" => (
            "Gestión de regla",
            "Actualizando regla",
            "Regla actualizada",
            &["name"],
            Other,
            Icon::File,
        ),
        "update_todos" | "update_plan" | "plan_save" => (
            "Actualización de plan",
            "Actualizando plan",
            "Plan actualizado",
            &["title"],
            Other,
            Icon::Plan,
        ),
        "get_goal" => (
            "Objetivo",
            "Consultando objetivo",
            "Objetivo consultado",
            &[],
            Other,
            Icon::Target,
        ),
        "create_goal" => (
            "Creación de objetivo",
            "Creando objetivo",
            "Objetivo creado",
            &["objective"],
            Other,
            Icon::Target,
        ),
        "update_goal" => (
            "Actualización de objetivo",
            "Actualizando objetivo",
            "Objetivo actualizado",
            &["status"],
            Other,
            Icon::Target,
        ),
        "record_goal_evidence" => (
            "Evidencia de objetivo",
            "Registrando evidencia",
            "Evidencia registrada",
            &["criterion_id"],
            Other,
            Icon::Target,
        ),
        "complete_goal" => (
            "Finalización de objetivo",
            "Verificando objetivo",
            "Objetivo completado",
            &["goal_id"],
            Other,
            Icon::Target,
        ),
        "git_status" | "git_preflight" => (
            "Revisión de Git",
            "Revisando Git",
            "Git revisado",
            &[],
            Other,
            Icon::GitBranch,
        ),
        "git_commit" | "git_checkpoint" => (
            "Commit",
            "Creando commit",
            "Commit creado",
            &["message"],
            Other,
            Icon::GitBranch,
        ),
        "mcp_call" => (
            "Herramienta MCP",
            "Ejecutando MCP",
            "MCP ejecutado",
            &["tool", "tool_name"],
            Mcp,
            Icon::Code,
        ),
        value
            if value.starts_with("mcp_")
                || value.starts_with("mcp__")
                || value.starts_with("mcp:") =>
        {
            (value, value, value, &[], Mcp, Icon::Code)
        }
        "" => (
            "Herramienta",
            "Herramienta",
            "Herramienta",
            &[],
            Other,
            Icon::Code,
        ),
        _ => (name, name, name, &[], Other, Icon::Code),
    };
    Meta {
        operation,
        running,
        completed,
        keys,
        kind,
        icon,
    }
}

pub(crate) fn operation(name: &str) -> &str {
    meta(name).operation
}
/// What a tool does, from its name alone.
pub(crate) fn kind(name: &str) -> ToolKind {
    meta(name).kind
}
#[cfg(test)]
pub(crate) fn icon(name: &str) -> Icon {
    meta(name).icon
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(clip_text(value, 512).0),
        Value::Number(value) => Some(value.to_string()),
        Value::Array(values) if !values.is_empty() => {
            // Identifying arrays may be very large. Their count is enough once
            // they exceed the compact row budget, without cloning every item.
            if values.len() > 6 {
                return Some(format!("{} elementos", values.len()));
            }
            let names: Option<Vec<_>> = values.iter().map(scalar).collect();
            Some(names.map_or_else(
                || format!("{} archivos", values.len()),
                |names| clip_text(&names.join(", "), 512).0,
            ))
        }
        _ => None,
    }
}

fn target(meta: &Meta<'_>, arguments: &Value) -> String {
    let Some((key, mut value)) = meta.keys.iter().find_map(|key| {
        scalar(arguments.get(*key)?)
            .filter(|text| !text.is_empty())
            .map(|value| (*key, value))
    }) else {
        return String::new();
    };
    if key == "path" || key == "file_path" {
        let start = arguments
            .get("start_line")
            .or_else(|| arguments.get("offset"))
            .and_then(Value::as_u64);
        if let Some(start) = start.filter(|value| *value > 0) {
            value.push_str(&format!(" L{start}"));
            if let Some(end) = arguments
                .get("end_line")
                .and_then(Value::as_u64)
                .filter(|end| *end > start)
            {
                value.push_str(&format!("-{end}"));
            }
        }
    }
    value.replace(['\n', '\r', '\t'], " ")
}

fn authoring_noun(arguments: &Value) -> Option<&'static str> {
    let raw_path = arguments.get("path")?.as_str()?;
    if raw_path.len() > 4096 {
        return None;
    }
    let path = raw_path.replace('\\', "/").to_ascii_lowercase();
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() >= 3 && parts[parts.len() - 3] == "skills" && parts.last() == Some(&"skill.md") {
        Some("skill")
    } else if parts
        .windows(2)
        .any(|parts| matches!(parts[0], ".openide" | ".agents" | ".cursor") && parts[1] == "rules")
    {
        Some("regla")
    } else if parts.last() == Some(&"hooks.json")
        && parts
            .iter()
            .any(|part| matches!(*part, ".openide" | ".agents" | ".cursor" | ".claude"))
    {
        Some("hooks")
    } else if path.ends_with("/.mcp.json")
        || path == ".mcp.json"
        || (parts.last() == Some(&"mcp.json")
            && parts
                .iter()
                .any(|part| matches!(*part, ".openide" | ".vscode" | ".cursor")))
    {
        Some("configuración MCP")
    } else {
        None
    }
}

pub(crate) fn present(
    name: &str,
    arguments: &Value,
    activity: ActivityStatus,
    is_error: bool,
    output: Option<&str>,
) -> ToolPresentation {
    let meta = meta(name);
    let failed = is_error || activity == ActivityStatus::Failed;
    let mut title = if failed {
        format!("Falló · {}", meta.operation)
    } else {
        match activity {
            ActivityStatus::Running => meta.running.into(),
            ActivityStatus::Completed => meta.completed.into(),
            ActivityStatus::WaitingPermission => format!("Permiso pendiente · {}", meta.operation),
            ActivityStatus::PermissionInterrupted => {
                format!("Permiso pendiente · {}", meta.operation)
            }
            ActivityStatus::Cancelled => format!("Cancelada · {}", meta.operation),
            ActivityStatus::Discarded => format!("Descartada · {}", meta.operation),
            ActivityStatus::Interrupted => format!("Sin confirmar · {}", meta.operation),
            ActivityStatus::Failed => unreachable!(),
        }
    };
    if !failed
        && matches!(
            activity,
            ActivityStatus::Running | ActivityStatus::Completed
        )
        && matches!(name, "read_file" | "write_file" | "edit_file")
    {
        if let Some(noun) = authoring_noun(arguments) {
            title.push_str(&format!(" · {noun}"));
        }
    }
    let note = if failed {
        output
            .and_then(|output| {
                if let Some((_, status)) = terminal_result(name, output) {
                    return Some(status);
                }
                if name.starts_with("mcp_") && output.trim_start().starts_with('{') {
                    // Structured binary results are not error prose. Prefer a
                    // bounded text block; never put base64 on the closed row.
                    let result = (output.len() <= 16_384)
                        .then(|| serde_json::from_str::<Value>(output).ok())
                        .flatten();
                    let message = result
                        .as_ref()
                        .and_then(|result| result.get("content")?.as_array())
                        .and_then(|blocks| {
                            blocks.iter().find_map(|block| {
                                (block.get("type")?.as_str()? == "text")
                                    .then(|| block.get("text")?.as_str())
                                    .flatten()
                            })
                        });
                    return Some(message.map_or_else(
                        || "La herramienta MCP informó un error".into(),
                        |message| clip_text(message, 240).0,
                    ));
                }
                clip_text(output, 1024)
                    .0
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(|line| clip_text(line, 240).0)
            })
            .or_else(|| Some("La operación no se completó".into()))
    } else {
        match activity {
            ActivityStatus::WaitingPermission => {
                Some("Esperando autorización para ejecutar".into())
            }
            ActivityStatus::PermissionInterrupted => {
                Some("El turno se interrumpió con un permiso pendiente".into())
            }
            ActivityStatus::Interrupted => {
                Some("Resultado incierto · no se recibió confirmación".into())
            }
            ActivityStatus::Cancelled => Some("Operación cancelada".into()),
            ActivityStatus::Discarded => Some("Operación descartada · no se ejecutó".into()),
            _ => None,
        }
    };
    ToolPresentation {
        title,
        target: target(&meta, arguments),
        kind: meta.kind,
        note,
        icon: if failed {
            Icon::ErrorCircle
        } else if activity == ActivityStatus::Cancelled {
            Icon::CloseCircle
        } else {
            meta.icon
        },
    }
}

/// Parse only complete top-level values from an unfinished JSON object. No
/// regex can accidentally pick a nested `path` or authored body as the target.
pub(crate) fn preview_arguments(raw: &str) -> Value {
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return value;
    }
    let mut rest = raw.trim_start();
    let Some(start) = rest.strip_prefix('{') else {
        return Value::Null;
    };
    rest = start;
    let mut fields = Map::new();
    loop {
        rest = rest.trim_start();
        let mut key_stream = serde_json::Deserializer::from_str(rest).into_iter::<String>();
        let Some(Ok(key)) = key_stream.next() else {
            break;
        };
        rest = rest[key_stream.byte_offset()..].trim_start();
        let Some(after_colon) = rest.strip_prefix(':') else {
            break;
        };
        rest = after_colon.trim_start();
        let mut value_stream = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
        let Some(Ok(value)) = value_stream.next() else {
            break;
        };
        let after = &rest[value_stream.byte_offset()..];
        // Numbers/literals need a delimiter: `12` at the current stream edge
        // might still become `123`; strings and closed containers are complete.
        if !matches!(value, Value::String(_) | Value::Array(_) | Value::Object(_))
            && after.trim_start().is_empty()
        {
            break;
        }
        fields.insert(key, value);
        rest = after.trim_start();
        let Some(after_comma) = rest.strip_prefix(',') else {
            break;
        };
        rest = after_comma;
    }
    Value::Object(fields)
}

/// Only the native persistent-command result schema carries process status.
/// Tool completion alone does not imply that a background process exited.
pub(crate) fn terminal_result(name: &str, output: &str) -> Option<(String, String)> {
    if !matches!(
        name,
        "start_command" | "poll_command" | "write_stdin" | "terminate_command" | "resize_command"
    ) {
        return None;
    }
    let result: Value = serde_json::from_str(output).ok()?;
    result.get("session_id")?.as_str()?;
    if name == "start_command" {
        return Some((
            String::new(),
            "Comando iniciado · aún sin resultado de salida".into(),
        ));
    }
    if name == "write_stdin" {
        return Some((
            String::new(),
            if result.get("stdin_closed").and_then(Value::as_bool) == Some(true) {
                "Entrada enviada · stdin cerrado".into()
            } else {
                "Entrada enviada".into()
            },
        ));
    }
    if name == "terminate_command" {
        return Some((
            String::new(),
            "Detención solicitada · queda salida por consultar".into(),
        ));
    }
    if name == "resize_command" {
        return Some((String::new(), "Tamaño del terminal actualizado".into()));
    }
    let events = result.get("events")?.as_array()?;
    let finished = result.get("finished")?.as_bool()?;
    let mut text = String::new();
    for event in events {
        if let Some(omitted) = event.get("omitted_bytes").and_then(Value::as_u64) {
            text.push_str(&format!("\n[{omitted} bytes de salida omitidos]\n"));
        } else if let Some(chunk) = event.get("text").and_then(Value::as_str) {
            if event.get("stream").and_then(Value::as_str) == Some("stderr") {
                text.push_str("\n[stderr]\n");
            }
            text.push_str(chunk);
        } else if event.get("encoding").and_then(Value::as_str) == Some("base64") {
            text.push_str("\n[Salida binaria · disponible en el inspector]\n");
        }
    }
    let mut status = match result.pointer("/exit/kind").and_then(Value::as_str) {
        Some("exited") => result
            .pointer("/exit/code")
            .and_then(Value::as_i64)
            .map_or_else(
                || "Proceso finalizado · código no disponible".into(),
                |code| format!("Proceso finalizado · código {code}"),
            ),
        Some("terminated") => "Proceso detenido".into(),
        Some("timed_out") => "Tiempo de ejecución agotado".into(),
        Some("wait_failed") => "No se pudo confirmar la salida del proceso".into(),
        _ if finished => "Consulta finalizada".into(),
        _ => "Proceso en ejecución al consultar".into(),
    };
    if result.get("has_more_output").and_then(Value::as_bool) == Some(true) {
        status.push_str(" · hay más salida");
    }
    if let Some(error) = result.get("reader_error").and_then(Value::as_str) {
        status.push_str(" · error de lectura");
        text.push_str(&format!("\n{error}"));
    }
    Some((text, status))
}

pub(crate) fn clip_text(text: &str, max_chars: usize) -> (String, bool) {
    let mut chars = text.chars();
    let visible: String = chars.by_ref().take(max_chars).collect();
    let limited = chars.next().is_some();
    (
        if limited {
            format!("{visible}…")
        } else {
            visible
        },
        limited,
    )
}

/// Stop serialization once the visible input budget is filled. Tool arguments
/// can contain entire authored files; a compact disclosure should not allocate
/// a second copy of those files every time streamed output updates.
pub(crate) fn json_preview(value: &Value, max_bytes: usize) -> (String, bool) {
    struct BoundedJson {
        bytes: Vec<u8>,
        limit: usize,
        limited: bool,
    }
    impl std::io::Write for BoundedJson {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let remaining = self.limit.saturating_sub(self.bytes.len());
            if remaining == 0 && !bytes.is_empty() {
                self.limited = true;
                return Err(std::io::Error::other("visible argument limit"));
            }
            let count = bytes.len().min(remaining);
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = BoundedJson {
        bytes: Vec::with_capacity(max_bytes.min(4096)),
        limit: max_bytes,
        limited: false,
    };
    let _ = serde_json::to_writer_pretty(&mut writer, value);
    // A byte budget can stop in the middle of a Unicode scalar. Retain only
    // its valid prefix; never display a fabricated replacement character.
    let valid = std::str::from_utf8(&writer.bytes)
        .map_or_else(|error| error.valid_up_to(), |text| text.len());
    writer.bytes.truncate(valid);
    let mut text = String::from_utf8(writer.bytes).expect("validated prefix");
    if writer.limited {
        text.push('…');
    }
    (text, writer.limited)
}

/// Read only a bounded suffix. A completed command can return one enormous
/// line; clipping after splitting/joining it still allocates that whole line.
pub(crate) fn terminal_tail(text: &str, max_lines: usize, max_chars: usize) -> (String, bool) {
    let mut start = text.len();
    let mut lines = 1;
    for (count, (index, character)) in text.char_indices().rev().enumerate() {
        if count >= max_chars || (character == '\n' && lines >= max_lines) {
            break;
        }
        if character == '\n' {
            lines += 1;
        }
        start = index;
    }
    (text[start..].to_owned(), start > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn streamed_targets_require_complete_top_level_fields_and_decode_escapes() {
        for partial in [
            r#"{"path":"src/partial"#,
            r#"{"nested":{"path":"secret"},"body":""#,
        ] {
            assert_eq!(target(&meta("write_file"), &preview_arguments(partial)), "");
        }
        let parsed = preview_arguments(r#"{"path":"src/\u00f1.rs","content":"unfinished"#);
        assert_eq!(target(&meta("write_file"), &parsed), "src/ñ.rs");
        assert_eq!(
            preview_arguments(r#"{"session_id":12"#)["session_id"],
            Value::Null
        );
        assert_eq!(
            preview_arguments(r#"{"session_id":12,"chars":""#)["session_id"],
            12
        );
    }

    #[test]
    fn terminal_input_and_authored_content_are_never_row_targets() {
        for name in ["write_stdin", "terminal_send"] {
            let p = present(
                name,
                &json!({"chars":"password", "text":"password", "session_id":"command-1"}),
                ActivityStatus::Completed,
                false,
                None,
            );
            assert_eq!(p.target, "command-1");
        }
        assert_eq!(
            present(
                "write_file",
                &json!({"content":"sensitive body"}),
                ActivityStatus::Running,
                false,
                None
            )
            .target,
            ""
        );
        let p = present(
            "start_command",
            &json!({"command":"cargo test"}),
            ActivityStatus::Completed,
            false,
            Some("started"),
        );
        assert_eq!(p.title, "Comando iniciado");
    }

    #[test]
    fn outcomes_do_not_claim_success_and_unknown_integrations_keep_their_names() {
        for state in [
            ActivityStatus::Failed,
            ActivityStatus::Cancelled,
            ActivityStatus::Interrupted,
            ActivityStatus::WaitingPermission,
        ] {
            let p = present(
                "write_file",
                &json!({"path":"src/a.rs"}),
                state,
                false,
                Some("Permission denied\nmore"),
            );
            assert!(!p.title.contains("Escrito"));
            assert!(p.note.is_some());
        }
        let p = present(
            "mcp__github__create_issue",
            &Value::Null,
            ActivityStatus::Completed,
            false,
            None,
        );
        assert!(p.title.contains("mcp__github__create_issue"));
        assert_eq!(p.kind, ToolKind::Mcp);
        let p = present(
            "read_file",
            &json!({"path":".agents/skills/demo/SKILL.md", "start_line":4,"end_line":8}),
            ActivityStatus::Completed,
            false,
            None,
        );
        assert!(p.title.contains("skill"));
        assert_eq!(p.target, ".agents/skills/demo/SKILL.md L4-8");
    }

    #[test]
    fn process_output_uses_recorded_exit_and_retains_stderr_and_omissions() {
        let snapshot = json!({"session_id":"process-1", "events":[
            {"stream":"stdout", "text":"first\n"}, {"stream":"stderr", "text":"warning\n"}, {"omitted_bytes":200}
        ], "exit":{"kind":"exited", "code":7}, "finished":true, "has_more_output":false});
        let (output, status) = terminal_result("poll_command", &snapshot.to_string()).unwrap();
        assert!(status.contains("código 7"));
        assert!(output.contains("first\n\n[stderr]\nwarning"));
        assert!(output.contains("200 bytes"));
        assert!(terminal_result("mcp__terminal", &snapshot.to_string()).is_none());
        assert!(terminal_result("poll_command", "arbitrary output").is_none());
        let (_, status) = terminal_result("start_command", r#"{"session_id":"p"}"#).unwrap();
        assert!(status.contains("aún sin resultado"));
    }

    #[test]
    fn command_tail_and_identifying_targets_are_bounded_for_huge_unicode_results() {
        let result = format!("{}END", "漢".repeat(1_000_000));
        let (tail, limited) = terminal_tail(&result, 6, 1400);
        assert!(limited);
        assert_eq!(tail.chars().count(), 1400);
        assert!(tail.ends_with("END"));
        assert_eq!(
            terminal_tail("one\ntwo\nthree", 2, 1400),
            ("two\nthree".into(), true)
        );
        assert_eq!(terminal_tail("one", 6, 1400), ("one".into(), false));
        assert!(
            present(
                "run_command",
                &json!({"command":result}),
                ActivityStatus::Completed,
                false,
                None
            )
            .target
            .chars()
            .count()
                <= 513
        );
    }
    #[test]
    fn input_preview_stops_at_its_byte_budget_without_splitting_unicode() {
        let value = json!({"path":"src/main.rs", "content":"漢🙂".repeat(100_000)});
        let (text, limited) = json_preview(&value, 3000);
        assert!(limited);
        assert!(text.len() <= 3003);
        assert!(text.ends_with('…'));
        assert!(!text.contains('�'));
        let (text, limited) = json_preview(&json!({"x":1}), 3000);
        assert!(!limited);
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({"x":1})
        );
    }

    #[test]
    fn native_browser_catalog_uses_browser_actions_but_preserves_other_mcp_names() {
        for tool in crate::browser_runtime::browser_mcp_tools() {
            let name = format!("mcp__openide_browser__{}", tool.name);
            let presentation = present(
                &name,
                &json!({"text":"private input", "url":"https://example.test"}),
                ActivityStatus::Completed,
                false,
                None,
            );
            assert_eq!(presentation.kind, ToolKind::Web, "{name}");
            assert!(!presentation.target.contains("private input"));
        }
        let name = "mcp__other_server__browser_fill";
        assert_eq!(
            present(name, &Value::Null, ActivityStatus::Completed, false, None).title,
            name
        );
    }
}
