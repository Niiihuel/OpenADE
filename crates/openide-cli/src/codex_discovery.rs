//! Read-only model discovery through the installed Codex app-server.
//!
//! This deliberately uses Codex's own authentication state. It never opens
//! auth files, starts login, or sends a turn. Call it from a worker thread.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 10;
const MAX_MODELS: usize = 1_000;
const MAX_MESSAGES: usize = 512;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 4 * 1024 * 1024;

/// `id` is the executable `model` slug returned by `model/list`, suitable for
/// `codex exec --model`. The catalog's separate `id` field is not used here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredCodexModel {
    pub id: String,
    pub display_name: String,
    /// Informational metadata from model/list, never an execution policy.
    pub description: Option<String>,
    pub input_modalities: Vec<String>,
    pub supported_efforts: Vec<String>,
    pub is_default: bool,
}

/// Discover models for the existing ChatGPT login in the installed Codex CLI.
/// A missing executable, absent ChatGPT login, or protocol error returns an
/// error so the caller can show a clearly labeled static fallback.
pub fn discover_codex_models() -> Result<Vec<DiscoveredCodexModel>, String> {
    discover_codex_models_with_executable(Path::new("codex"))
}

/// Use the executable from a configured Codex profile, if one is available.
/// This function is synchronous and must run off the UI thread.
pub fn discover_codex_models_with_executable(
    executable: &Path,
) -> Result<Vec<DiscoveredCodexModel>, String> {
    let executable = super::detect_executable(executable)
        .ok_or_else(|| "No se encontró el ejecutable de Codex".to_owned())?;
    let mut command = Command::new(executable);
    command
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(super::CREATE_NO_WINDOW);
    let mut child = ChildCleanup(
        command
            .spawn()
            .map_err(|error| format!("No se pudo iniciar Codex app-server: {error}"))?,
    );
    let stdin = child
        .0
        .stdin
        .take()
        .ok_or("Codex app-server no abrió stdin")?;
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or("Codex app-server no abrió stdout")?;
    let (sender, receiver) = mpsc::sync_channel(8);
    let reader = thread::spawn(move || read_stdout(stdout, sender));
    let mut transport = ProcessTransport { stdin, receiver };
    let result = query_models(&mut transport, Instant::now() + DISCOVERY_TIMEOUT);
    drop(transport);
    drop(child);
    // A descendant outside our process tree could still hold stdout open.
    // Do not turn a bounded RPC into an unbounded join after killing the CLI.
    if reader.is_finished() {
        let _ = reader.join();
    }
    result
}

struct ChildCleanup(Child);

impl Drop for ChildCleanup {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            super::terminate_child(&mut self.0);
        }
        let _ = self.0.wait();
    }
}

fn read_stdout(stdout: ChildStdout, sender: SyncSender<Result<Vec<u8>, String>>) {
    let mut reader = BufReader::new(stdout);
    let mut total = 0usize;
    for _ in 0..MAX_MESSAGES {
        match read_bounded_line(&mut reader) {
            Ok(Some(line)) => {
                total = total.saturating_add(line.len());
                if total > MAX_TOTAL_BYTES {
                    let _ = sender.send(Err("Codex app-server excedió el límite de salida".into()));
                    return;
                }
                if sender.send(Ok(line)).is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = sender.send(Err("Codex app-server cerró stdout".into()));
                return;
            }
            Err(error) => {
                let _ = sender.send(Err(format!("Salida inválida de Codex app-server: {error}")));
                return;
            }
        }
    }
    let _ = sender.send(Err("Codex app-server envió demasiados mensajes".into()));
}

fn read_bounded_line(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "mensaje JSON sin salto de línea",
                ))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(count) > MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "mensaje JSON demasiado grande",
            ));
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

trait RpcTransport {
    fn send(&mut self, message: Value) -> Result<(), String>;
    fn receive(&mut self, deadline: Instant) -> Result<Value, String>;
}

struct ProcessTransport {
    stdin: ChildStdin,
    receiver: Receiver<Result<Vec<u8>, String>>,
}

impl RpcTransport for ProcessTransport {
    fn send(&mut self, message: Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(&message)
            .map_err(|error| format!("No se pudo serializar la petición Codex: {error}"))?;
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err("Petición Codex demasiado grande".into());
        }
        self.stdin
            .write_all(&bytes)
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .map_err(|error| format!("No se pudo escribir a Codex app-server: {error}"))
    }

    fn receive(&mut self, deadline: Instant) -> Result<Value, String> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("Caducó la consulta a Codex app-server".into());
        }
        let bytes = self
            .receiver
            .recv_timeout(remaining)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    "Caducó la consulta a Codex app-server".to_owned()
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    "Se interrumpió la salida de Codex app-server".to_owned()
                }
            })??;
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("Codex app-server envió JSON inválido: {error}"))
    }
}

fn request(
    transport: &mut impl RpcTransport,
    deadline: Instant,
    id: i64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    transport.send(json!({"id":id,"method":method,"params":params}))?;
    for _ in 0..MAX_MESSAGES {
        let response = transport.receive(deadline)?;
        if response.get("id").is_none() && response.get("method").and_then(Value::as_str).is_some()
        {
            // Startup notifications may arrive before a requested response.
            continue;
        }
        if response.get("id").and_then(Value::as_i64) != Some(id) {
            return Err(format!(
                "Codex app-server respondió con un ID inesperado a {method}"
            ));
        }
        if let Some(error) = response.get("error") {
            let code = error
                .get("code")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("error desconocido");
            return Err(format!(
                "Codex app-server rechazó {method} ({code}): {message}"
            ));
        }
        return response
            .get("result")
            .cloned()
            .ok_or_else(|| format!("Codex app-server no devolvió resultado para {method}"));
    }
    Err("Codex app-server envió demasiadas notificaciones".into())
}

fn query_models(
    transport: &mut impl RpcTransport,
    deadline: Instant,
) -> Result<Vec<DiscoveredCodexModel>, String> {
    let version = env!("CARGO_PKG_VERSION");
    request(
        transport,
        deadline,
        1,
        "initialize",
        json!({"clientInfo":{"name":"openIDE","title":"openIDE","version":version}}),
    )?;
    transport.send(json!({"method":"initialized"}))?;
    let account = request(transport, deadline, 2, "account/read", json!({}))?;
    let account_type = account
        .get("account")
        .and_then(|account| account.get("type"))
        .and_then(Value::as_str);
    if account_type != Some("chatgpt") {
        return Err("Codex CLI no tiene una sesión ChatGPT activa".into());
    }

    let mut models = Vec::new();
    let mut seen_models = HashSet::new();
    let mut seen_cursors = HashSet::new();
    let mut cursor: Option<String> = None;
    for page in 0..MAX_PAGES {
        let mut params = json!({"limit":PAGE_SIZE,"includeHidden":false});
        if let Some(cursor) = &cursor {
            params["cursor"] = Value::String(cursor.clone());
        }
        let response = request(transport, deadline, page as i64 + 3, "model/list", params)?;
        let (page_models, next_cursor) = parse_model_page(&response)?;
        for model in page_models {
            if seen_models.insert(model.id.clone()) {
                models.push(model);
            } else if model.is_default {
                if let Some(existing) = models.iter_mut().find(|existing| existing.id == model.id) {
                    *existing = model;
                }
            }
        }
        if models.len() > MAX_MODELS {
            return Err("Codex app-server anunció demasiados modelos".into());
        }
        let Some(next_cursor) = next_cursor else {
            if models.is_empty() {
                return Err("Codex app-server no anunció modelos disponibles".into());
            }
            return Ok(models);
        };
        if !seen_cursors.insert(next_cursor.clone()) {
            return Err("Codex app-server repitió un cursor de modelos".into());
        }
        cursor = Some(next_cursor);
    }
    Err("Codex app-server excedió el límite de páginas de modelos".into())
}

fn parse_model_page(
    response: &Value,
) -> Result<(Vec<DiscoveredCodexModel>, Option<String>), String> {
    let entries = response
        .get("data")
        .and_then(Value::as_array)
        .ok_or("Codex app-server no devolvió una lista de modelos")?;
    if entries.len() > PAGE_SIZE {
        return Err("Codex app-server devolvió una página de modelos demasiado grande".into());
    }
    let mut models = Vec::new();
    for entry in entries {
        if entry.get("hidden").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let slug = entry
            .get("model")
            .and_then(Value::as_str)
            .filter(|slug| valid_model_slug(slug))
            .ok_or("Codex app-server devolvió un slug de modelo inválido")?;
        let display_name = entry
            .get("displayName")
            .and_then(Value::as_str)
            .filter(|name| {
                !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
            })
            .unwrap_or(slug);
        let options = entry
            .get("supportedReasoningEfforts")
            .and_then(Value::as_array)
            .ok_or("Codex app-server no informó niveles de esfuerzo")?;
        let mut supported_efforts = Vec::new();
        for option in options {
            if let Some(effort) = option
                .get("reasoningEffort")
                .and_then(Value::as_str)
                .filter(|effort| effort_is_safe(effort))
            {
                if !supported_efforts.iter().any(|existing| existing == effort) {
                    supported_efforts.push(effort.to_owned());
                }
            }
        }
        let is_default = entry
            .get("isDefault")
            .and_then(Value::as_bool)
            .ok_or("Codex app-server no informó el modelo predeterminado")?;
        models.push(DiscoveredCodexModel {
            id: slug.to_owned(),
            display_name: display_name.to_owned(),
            description: entry
                .get("description")
                .and_then(Value::as_str)
                .filter(|value| {
                    !value.trim().is_empty()
                        && value.len() <= 4_096
                        && value.chars().all(|character| {
                            !character.is_control() || matches!(character, '\n' | '\t')
                        })
                })
                .map(str::to_owned),
            input_modalities: entry
                .get("inputModalities")
                .and_then(Value::as_array)
                .filter(|values| values.len() <= 16)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|value| {
                    !value.is_empty() && value.len() <= 64 && !value.chars().any(char::is_control)
                })
                .map(str::to_owned)
                .collect(),
            supported_efforts,
            is_default,
        });
    }
    let next_cursor = match response.get("nextCursor") {
        Some(Value::String(cursor))
            if !cursor.is_empty()
                && cursor.len() <= 256
                && !cursor.chars().any(char::is_control) =>
        {
            Some(cursor.to_owned())
        }
        None | Some(Value::Null) => None,
        _ => return Err("Codex app-server devolvió un cursor inválido".into()),
    };
    Ok((models, next_cursor))
}

fn valid_model_slug(model: &str) -> bool {
    !model.is_empty()
        && !model.starts_with('-')
        && model.len() <= 128
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/+-".contains(&byte))
}

pub(super) fn effort_is_safe(effort: &str) -> bool {
    matches!(
        effort,
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakeTransport {
        sent: Vec<Value>,
        received: VecDeque<Value>,
    }

    impl FakeTransport {
        fn new(received: Vec<Value>) -> Self {
            Self {
                sent: Vec::new(),
                received: received.into(),
            }
        }
    }

    impl RpcTransport for FakeTransport {
        fn send(&mut self, message: Value) -> Result<(), String> {
            self.sent.push(message);
            Ok(())
        }

        fn receive(&mut self, _: Instant) -> Result<Value, String> {
            self.received
                .pop_front()
                .ok_or_else(|| "sin mensajes simulados".into())
        }
    }

    fn row(catalog_id: &str, model: &str, default: bool) -> Value {
        json!({
            "id": catalog_id,
            "model": model,
            "displayName": "Modelo de prueba",
            "hidden": false,
            "supportedReasoningEfforts": [
                {"reasoningEffort":"low","description":"rápido"},
                {"reasoningEffort":"high","description":"profundo"}
            ],
            "isDefault": default
        })
    }

    #[test]
    fn model_page_uses_executable_slug_and_preserves_effort_order() {
        let response = json!({"data":[row("opaque-id", "gpt-6-sol", true)],"nextCursor":null});
        let (models, cursor) = parse_model_page(&response).unwrap();
        assert_eq!(cursor, None);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-6-sol");
        assert_eq!(models[0].supported_efforts, ["low", "high"]);
        assert!(models[0].is_default);
    }

    #[test]
    fn protocol_initializes_checks_chatgpt_and_pages_without_turns() {
        let mut transport = FakeTransport::new(vec![
            json!({"id":1,"result":{"userAgent":"codex"}}),
            json!({"method":"account/updated","params":{}}),
            json!({"id":2,"result":{"account":{"type":"chatgpt"},"requiresOpenaiAuth":true}}),
            json!({"id":3,"result":{"data":[row("catalog-a","gpt-6-sol",true)],"nextCursor":"page-2"}}),
            json!({"id":4,"result":{"data":[row("catalog-b","gpt-6-luna",false)],"nextCursor":null}}),
        ]);
        let models = query_models(&mut transport, Instant::now() + Duration::from_secs(1)).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(transport.sent[0]["method"], "initialize");
        assert_eq!(transport.sent[1]["method"], "initialized");
        assert_eq!(transport.sent[2]["method"], "account/read");
        assert_eq!(transport.sent[3]["method"], "model/list");
        assert_eq!(transport.sent[4]["params"]["cursor"], "page-2");
        assert!(transport
            .sent
            .iter()
            .all(|message| message["method"] != "turn/start"
                && message["method"] != "account/login/start"));
    }

    #[test]
    fn protocol_rejects_non_chatgpt_auth_and_rpc_errors() {
        let mut key = FakeTransport::new(vec![
            json!({"id":1,"result":{}}),
            json!({"id":2,"result":{"account":{"type":"apiKey"}}}),
        ]);
        assert!(
            query_models(&mut key, Instant::now() + Duration::from_secs(1))
                .unwrap_err()
                .contains("ChatGPT")
        );
        assert!(!key
            .sent
            .iter()
            .any(|message| message["method"] == "model/list"));

        let mut error = FakeTransport::new(vec![
            json!({"id":1,"result":{}}),
            json!({"id":2,"result":{"account":{"type":"chatgpt"}}}),
            json!({"id":3,"error":{"code":-32600,"message":"restart required"}}),
        ]);
        assert!(
            query_models(&mut error, Instant::now() + Duration::from_secs(1))
                .unwrap_err()
                .contains("restart required")
        );
    }

    #[test]
    fn bounded_line_reader_rejects_oversized_messages() {
        let long = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        let mut reader = BufReader::new(long.as_slice());
        assert_eq!(
            read_bounded_line(&mut reader).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
