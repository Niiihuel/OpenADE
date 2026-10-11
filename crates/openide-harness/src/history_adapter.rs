//! Makes a conversation that came from any provider acceptable to the one
//! about to receive it. Works on a copy: the journal is never rewritten.
//!
//! Provider-only items (signed thinking, encrypted reasoning, hosted search)
//! are not touched here: `reconcile_model_history_route` already dropped them
//! for any route but their own, and a message that still carries them is
//! native to the target and passes through verbatim.

use std::collections::{BTreeMap, BTreeSet};

use openide_core::{AgentMessage, MessageRole, UserImage, UserImageMime};
use sha2::{Digest, Sha256};

use crate::{ModelCompletion, ToolDefinition};

pub const INTERRUPTED_TOOL_RESULT: &str =
    "[Herramienta interrumpida antes de completarse: no hay resultado]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolIdRule {
    /// The transport accepts any identifier.
    Any,
    /// `[A-Za-z0-9_-]`, at most this many characters.
    Token { max_len: usize },
    /// Exactly nine ASCII alphanumerics (Mistral).
    Alnum9,
}

impl ToolIdRule {
    fn accepts(self, id: &str) -> bool {
        match self {
            Self::Any => !id.is_empty(),
            Self::Token { max_len } => {
                !id.is_empty()
                    && id.len() <= max_len
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            }
            Self::Alnum9 => id.len() == 9 && id.bytes().all(|byte| byte.is_ascii_alphanumeric()),
        }
    }

    /// A valid identifier derived only from `id` and `salt`.
    fn derive(self, id: &str, salt: u32) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"openide-tool-call-id\0");
        hasher.update(salt.to_be_bytes());
        hasher.update(id.as_bytes());
        let hex: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        match self {
            Self::Alnum9 => hex[..9].to_owned(),
            Self::Any | Self::Token { .. } => format!("call_{}", &hex[..24]),
        }
    }
}

/// What the target of a request accepts. Comes from the backend of the route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetCaps {
    pub vision: bool,
    pub tools: bool,
    pub tool_call_id: ToolIdRule,
    /// Longest tool name over `[A-Za-z0-9_-]`; None accepts any name.
    pub tool_name_max: Option<usize>,
}

impl TargetCaps {
    /// Accepts everything: only pairing is repaired.
    pub const PERMISSIVE: Self = Self {
        vision: true,
        tools: true,
        tool_call_id: ToolIdRule::Any,
        tool_name_max: None,
    };
}

/// Why a request must not be sent as it is. Shown before anything is sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocker {
    /// The turn offers tools and the target cannot call any.
    ToolsRequired,
    /// The message being sent carries images the target cannot see.
    PendingImages { count: usize },
    /// Not a block: the target lacks the chosen effort and uses `fallback`.
    EffortUnavailable { requested: String, fallback: String },
}

impl Blocker {
    pub fn blocks(&self) -> bool {
        !matches!(self, Self::EffortUnavailable { .. })
    }
}

impl std::fmt::Display for Blocker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ToolsRequired => {
                formatter.write_str("this turn needs tools and the target model has none")
            }
            Self::PendingImages { count } => write!(
                formatter,
                "the message carries {count} image(s) and the target model cannot see images"
            ),
            Self::EffortUnavailable { requested, fallback } => write!(
                formatter,
                "effort '{requested}' is not offered by the target model; using '{fallback}'"
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AdaptedRequest {
    pub messages: Vec<AgentMessage>,
    pub tools: Vec<ToolDefinition>,
    /// Name sent to the target -> the real tool name.
    names: BTreeMap<String, String>,
}

impl AdaptedRequest {
    /// Give the model's calls their real tool names back.
    pub fn restore(&self, completion: &mut ModelCompletion) {
        for call in &mut completion.output.tool_calls {
            if let Some(real) = self.names.get(&call.name) {
                call.name = real.clone();
            }
        }
    }

    pub fn real_tool_name<'a>(&'a self, sent: &'a str) -> &'a str {
        self.names.get(sent).map_or(sent, String::as_str)
    }
}

/// The history as the target accepts it, with no tools offered and no message
/// treated as the one being sent.
pub fn adapt_history(
    messages: &[AgentMessage],
    caps: &TargetCaps,
) -> Result<Vec<AgentMessage>, Vec<Blocker>> {
    adapt_request(messages, &[], None, caps).map(|request| request.messages)
}

/// `pending_prompt` is the index of the user message this request sends for
/// the first time; images there are a blocker instead of a placeholder.
pub fn adapt_request(
    messages: &[AgentMessage],
    tools: &[ToolDefinition],
    pending_prompt: Option<usize>,
    caps: &TargetCaps,
) -> Result<AdaptedRequest, Vec<Blocker>> {
    let mut blockers = Vec::new();
    if !caps.tools && !tools.is_empty() {
        blockers.push(Blocker::ToolsRequired);
    }
    if !caps.vision {
        if let Some(count) = pending_prompt
            .and_then(|index| messages.get(index))
            .map(|message| message.images.len())
            .filter(|count| *count > 0)
        {
            blockers.push(Blocker::PendingImages { count });
        }
    }
    if !blockers.is_empty() {
        return Err(blockers);
    }

    let mut names = NameMap::new(caps.tool_name_max);
    let tools = tools
        .iter()
        .map(|tool| ToolDefinition {
            name: names.sent(&tool.name),
            ..tool.clone()
        })
        .collect();

    // Identifiers the target already accepts keep their value, so a derived
    // one must avoid all of them, not only the other derived ones.
    let mut taken: BTreeSet<String> = messages
        .iter()
        .flat_map(|message| &message.tool_calls)
        .map(|call| call.id.clone())
        .filter(|id| caps.tool_call_id.accepts(id))
        .collect();
    let mut ids = BTreeMap::<String, String>::new();

    let mut adapted = Vec::with_capacity(messages.len());
    let mut index = 0;
    while index < messages.len() {
        let mut message = messages[index].clone();
        index += 1;
        if !caps.vision && !message.images.is_empty() {
            for image in std::mem::take(&mut message.images) {
                if !message.content.is_empty() {
                    message.content.push('\n');
                }
                message.content.push_str(&image_placeholder(&image));
            }
        }
        match message.role {
            // A result is only kept next to the call it answers, below.
            MessageRole::Tool => continue,
            MessageRole::Assistant if !message.tool_calls.is_empty() => {
                // Provider items repeat these identifiers: such a message is
                // native to the target and its identifiers already valid.
                let native = !message.provider_output_items.is_empty();
                let mut results = BTreeMap::<String, AgentMessage>::new();
                while let Some(result) = messages
                    .get(index)
                    .filter(|next| next.role == MessageRole::Tool)
                {
                    index += 1;
                    if let Some(id) = &result.tool_call_id {
                        results.entry(id.clone()).or_insert_with(|| result.clone());
                    }
                }
                let mut paired = Vec::with_capacity(message.tool_calls.len());
                for call in &mut message.tool_calls {
                    let mut result = results.remove(&call.id).unwrap_or_else(|| {
                        AgentMessage::tool(call.id.clone(), INTERRUPTED_TOOL_RESULT)
                    });
                    if !native && !caps.tool_call_id.accepts(&call.id) {
                        let sent = ids.entry(call.id.clone()).or_insert_with(|| {
                            let mut salt = 0;
                            loop {
                                let candidate = caps.tool_call_id.derive(&call.id, salt);
                                if taken.insert(candidate.clone()) {
                                    break candidate;
                                }
                                salt += 1;
                            }
                        });
                        call.id = sent.clone();
                    }
                    result.tool_call_id = Some(call.id.clone());
                    call.name = names.sent(&call.name);
                    paired.push(result);
                }
                if caps.tools {
                    adapted.push(message);
                    adapted.extend(paired);
                } else {
                    adapted.extend(flatten(message, paired));
                }
            }
            MessageRole::Assistant
                if message.content.is_empty() && message.provider_output_items.is_empty() =>
            {
                // Only provider items another route dropped: nothing to say.
            }
            _ => adapted.push(message),
        }
    }
    Ok(AdaptedRequest {
        messages: adapted,
        tools,
        names: names.real,
    })
}

/// The request `model` accepts for this conversation. The newest user message
/// is the one being sent: a blocker there fails the request instead of
/// degrading it silently.
pub(crate) fn adapt_for(
    model: &dyn crate::ModelBackend,
    messages: &[AgentMessage],
    tools: &[ToolDefinition],
) -> Result<AdaptedRequest, crate::AgentError> {
    let pending = messages
        .iter()
        .rposition(|message| message.role == MessageRole::User);
    adapt_request(messages, tools, pending, &model.target_caps()).map_err(|blockers| {
        crate::AgentError::InvalidRequest(
            blockers
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    })
}

/// Tool traffic as plain text for a target that has no tools.
fn flatten(mut assistant: AgentMessage, results: Vec<AgentMessage>) -> Vec<AgentMessage> {
    let calls = std::mem::take(&mut assistant.tool_calls);
    assistant.provider_output_items.clear();
    let mut output = String::new();
    for (call, result) in calls.iter().zip(results) {
        if !assistant.content.is_empty() {
            assistant.content.push('\n');
        }
        assistant.content.push_str(&format!(
            "[Llamada a herramienta {}: {}]",
            call.name, call.arguments
        ));
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&format!("[Resultado de {}]\n{}", call.name, result.content));
    }
    vec![assistant, AgentMessage::user(output)]
}

struct NameMap {
    max_len: Option<usize>,
    sent: BTreeMap<String, String>,
    real: BTreeMap<String, String>,
}

impl NameMap {
    fn new(max_len: Option<usize>) -> Self {
        Self {
            max_len,
            sent: BTreeMap::new(),
            real: BTreeMap::new(),
        }
    }

    fn sent(&mut self, real: &str) -> String {
        if let Some(sent) = self.sent.get(real) {
            return sent.clone();
        }
        let Some(max_len) = self.max_len else {
            return real.to_owned();
        };
        let clean: String = real
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        let mut sent = clean.clone();
        let mut salt = 0u32;
        // A valid name nobody else claimed is sent as it is. Anything else
        // gets a digest of the real name, so two names never meet.
        let mut settled = clean == real && !clean.is_empty() && clean.len() <= max_len;
        while !settled || self.real.get(&sent).is_some_and(|owner| owner != real) {
            let mut hasher = Sha256::new();
            hasher.update(b"openide-tool-name\0");
            hasher.update(salt.to_be_bytes());
            hasher.update(real.as_bytes());
            let digest: String = hasher
                .finalize()
                .iter()
                .take(4)
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let keep = max_len.saturating_sub(digest.len() + 1).min(clean.len());
            sent = format!("{}_{digest}", &clean[..keep]);
            salt += 1;
            settled = true;
        }
        self.sent.insert(real.to_owned(), sent.clone());
        self.real.insert(sent.clone(), real.to_owned());
        sent
    }
}

pub fn image_placeholder(image: &UserImage) -> String {
    let size = image_dimensions(image)
        .map(|(width, height)| format!(", {width}x{height}"))
        .unwrap_or_default();
    format!(
        "[Imagen adjunta: {}{size} — este modelo no puede verla]",
        image.mime_type.as_str()
    )
}

// shortcut: only PNG and GIF state their size in a fixed header; add JPEG and
// WebP parsing if the placeholder needs their dimensions.
fn image_dimensions(image: &UserImage) -> Option<(u32, u32)> {
    let bytes = image.verified_bytes().ok()?;
    match image.mime_type {
        UserImageMime::Png if bytes.len() >= 24 => Some((
            u32::from_be_bytes(bytes[16..20].try_into().ok()?),
            u32::from_be_bytes(bytes[20..24].try_into().ok()?),
        )),
        UserImageMime::Gif if bytes.len() >= 10 => Some((
            u32::from(u16::from_le_bytes(bytes[6..8].try_into().ok()?)),
            u32::from(u16::from_le_bytes(bytes[8..10].try_into().ok()?)),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::ToolCall;
    use serde_json::json;

    const ANTHROPIC: TargetCaps = TargetCaps {
        vision: true,
        tools: true,
        tool_call_id: ToolIdRule::Token { max_len: 64 },
        tool_name_max: Some(64),
    };

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: json!({"path": "a"}),
        }
    }

    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.into(),
            description: "d".into(),
            parameters: json!({"type": "object"}),
        }
    }

    #[test]
    fn invalid_ids_change_in_call_and_result_and_never_collide() {
        let rule = ToolIdRule::Alnum9;
        // The second id is already what the first one derives to.
        let squatter = rule.derive("call:one", 0);
        let messages = vec![
            AgentMessage::user("go"),
            AgentMessage::assistant(
                "",
                vec![call("call:one", "read"), call(&squatter, "read"), call("call|two", "read")],
            ),
            AgentMessage::tool("call:one", "1"),
            AgentMessage::tool(&squatter, "2"),
            AgentMessage::tool("call|two", "3"),
        ];
        let caps = TargetCaps { tool_call_id: rule, ..ANTHROPIC };
        let adapted = adapt_history(&messages, &caps).unwrap();
        let ids: Vec<&str> = adapted[1].tool_calls.iter().map(|call| call.id.as_str()).collect();
        assert_eq!(ids[1], squatter, "a valid id is kept");
        assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), 3);
        assert!(ids.iter().all(|id| rule.accepts(id)));
        for (index, id) in ids.iter().enumerate() {
            assert_eq!(adapted[2 + index].tool_call_id.as_deref(), Some(*id));
            assert_eq!(adapted[2 + index].content, (index + 1).to_string());
        }
        assert_eq!(adapted, adapt_history(&messages, &caps).unwrap(), "stateless");
        assert_eq!(messages[1].tool_calls[0].id, "call:one", "the source is untouched");
    }

    #[test]
    fn pairs_are_repaired_without_reordering() {
        let messages = vec![
            AgentMessage::user("go"),
            AgentMessage::tool("ghost", "orphan"),
            AgentMessage::assistant("", vec![call("a", "read"), call("b", "read")]),
            AgentMessage::tool("b", "B"),
            AgentMessage::tool("ghost", "orphan"),
            AgentMessage::user("next"),
        ];
        let adapted = adapt_history(&messages, &ANTHROPIC).unwrap();
        let shape: Vec<(MessageRole, &str)> = adapted
            .iter()
            .map(|message| (message.role, message.content.as_str()))
            .collect();
        assert_eq!(
            shape,
            vec![
                (MessageRole::User, "go"),
                (MessageRole::Assistant, ""),
                (MessageRole::Tool, INTERRUPTED_TOOL_RESULT),
                (MessageRole::Tool, "B"),
                (MessageRole::User, "next"),
            ]
        );
    }

    #[test]
    fn tool_names_round_trip_through_the_target_charset() {
        let long = format!("mcp__server__{}", "x".repeat(80));
        let names = ["read_file", "mcp.server/tool", "mcp_server_tool", long.as_str(), "ñ"];
        let tools: Vec<ToolDefinition> = names.iter().map(|name| tool(name)).collect();
        let messages = vec![
            AgentMessage::user("go"),
            AgentMessage::assistant("", vec![call("a", "mcp.server/tool")]),
            AgentMessage::tool("a", "ok"),
        ];
        let request = adapt_request(&messages, &tools, None, &ANTHROPIC).unwrap();
        let sent: Vec<&str> = request.tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(sent[0], "read_file");
        assert_eq!(sent[2], "mcp_server_tool");
        assert_eq!(sent.iter().collect::<BTreeSet<_>>().len(), names.len());
        for name in &sent {
            assert!(ToolIdRule::Token { max_len: 64 }.accepts(name), "{name}");
        }
        assert_eq!(request.messages[1].tool_calls[0].name, sent[1]);
        let mut completion = ModelCompletion::from(crate::ModelOutput {
            content: String::new(),
            tool_calls: sent.iter().map(|name| call("x", name)).collect(),
        });
        request.restore(&mut completion);
        let real: Vec<&str> =
            completion.output.tool_calls.iter().map(|call| call.name.as_str()).collect();
        assert_eq!(real, names);
    }

    #[test]
    fn images_become_text_for_a_blind_target_and_block_only_when_pending() {
        let png = [
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".as_slice(),
            &640u32.to_be_bytes(),
            &480u32.to_be_bytes(),
        ]
        .concat();
        let image = UserImage::from_bytes(UserImageMime::Png, &png).unwrap();
        let messages = vec![
            AgentMessage::user_with_images("mira", vec![image]).unwrap(),
            AgentMessage::assistant("visto", vec![]),
        ];
        let blind = TargetCaps { vision: false, ..ANTHROPIC };
        let adapted = adapt_history(&messages, &blind).unwrap();
        assert!(adapted[0].images.is_empty());
        assert_eq!(
            adapted[0].content,
            "mira\n[Imagen adjunta: image/png, 640x480 — este modelo no puede verla]"
        );
        assert_eq!(
            adapt_request(&messages, &[], Some(0), &blind),
            Err(vec![Blocker::PendingImages { count: 1 }])
        );
        assert_eq!(adapt_history(&messages, &ANTHROPIC).unwrap(), messages);
    }

    #[test]
    fn a_target_without_tools_reads_the_tool_traffic_as_text() {
        let messages = vec![
            AgentMessage::user("go"),
            AgentMessage::assistant("leo", vec![call("a", "read")]),
            AgentMessage::tool("a", "contenido"),
        ];
        let plain = TargetCaps { tools: false, ..ANTHROPIC };
        let adapted = adapt_history(&messages, &plain).unwrap();
        assert!(adapted.iter().all(|message| message.tool_calls.is_empty()
            && message.tool_call_id.is_none()
            && message.role != MessageRole::Tool));
        assert_eq!(adapted[1].content, "leo\n[Llamada a herramienta read: {\"path\":\"a\"}]");
        assert_eq!(adapted[2].content, "[Resultado de read]\ncontenido");
        assert_eq!(
            adapt_request(&messages, &[tool("read")], None, &plain),
            Err(vec![Blocker::ToolsRequired])
        );
    }

    #[test]
    fn a_native_message_with_provider_items_passes_verbatim() {
        let mut native = AgentMessage::assistant("", vec![call("toolu_01", "read")]);
        native.provider_output_items = vec![json!({"type": "thinking", "signature": "s"})];
        let messages =
            vec![AgentMessage::user("go"), native.clone(), AgentMessage::tool("toolu_01", "ok")];
        let caps = TargetCaps { tool_call_id: ToolIdRule::Alnum9, ..ANTHROPIC };
        assert_eq!(adapt_history(&messages, &caps).unwrap(), messages);
    }
}

#[cfg(test)]
#[path = "history_adapter_matrix_tests.rs"]
mod matrix_tests;
