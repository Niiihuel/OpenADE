//! Makes a conversation that came from any provider acceptable to the one
//! about to receive it. Works on a copy: the journal is never rewritten.
//!
//! Provider-only items (signed thinking, encrypted reasoning, hosted search)
//! are not touched here: `reconcile_model_history_route` already dropped them
//! for any route but their own, and a message that still carries them is
//! native to the target and passes through verbatim.

use std::collections::{BTreeMap, BTreeSet};

use openide_core::{AgentMessage, MessageRole, UserImage};
use sha2::{Digest, Sha256};

use crate::{ModelCompletion, ToolDefinition};

pub const INTERRUPTED_TOOL_RESULT: &str =
    "[Herramienta interrumpida antes de completarse: no hay resultado]";

/// Anthropic holds every image of a request with more than 20 images to
/// 2000 px a side and rejects the request otherwise. Staying at 20 keeps a
/// tall screenshot sendable.
const MAX_REQUEST_IMAGES: usize = 20;
/// The newest tool images a request carries, whatever else it holds.
const MAX_REQUEST_TOOL_IMAGES: usize = 16;
/// The images of one request, before base64: 20 MB encoded, which leaves
/// room for the text under the 32 MB the strictest provider accepts.
const MAX_REQUEST_IMAGE_BYTES: usize = 15_000_000;
/// Older tool images leave a request this many at a time: what a provider
/// cached of the conversation then changes once per group, not per image.
const TOOL_IMAGE_GROUP: usize = 4;

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

    let mut without_images = without_unsent_tool_images(messages, caps);
    let mut adapted = Vec::with_capacity(messages.len());
    let mut index = 0;
    while index < messages.len() {
        // A result is cloned where it is paired with its call, below.
        if messages[index].role == MessageRole::Tool {
            index += 1;
            continue;
        }
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
                        results.entry(id.clone()).or_insert_with(|| {
                            without_images.remove(&(index - 1)).unwrap_or_else(|| result.clone())
                        });
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

/// The tool results whose images this request does not carry, by position,
/// each with a line of text where an image was. A target that cannot take
/// images in a tool exchange carries none; any other carries the newest that
/// fit the request, so the model always sees the latest capture.
fn without_unsent_tool_images(
    messages: &[AgentMessage],
    caps: &TargetCaps,
) -> BTreeMap<usize, AgentMessage> {
    let tool_images: Vec<(usize, usize)> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == MessageRole::Tool)
        .flat_map(|(at, message)| message.images.iter().map(move |image| (at, image.size_bytes as usize)))
        .collect();
    let (kept, why) = if caps.vision && caps.tools {
        let (mut count, mut bytes) = (0, 0);
        for message in messages.iter().filter(|message| message.role != MessageRole::Tool) {
            count += message.images.len();
            bytes += message.images.iter().map(|image| image.size_bytes as usize).sum::<usize>();
        }
        let max = MAX_REQUEST_IMAGES.saturating_sub(count).min(MAX_REQUEST_TOOL_IMAGES);
        let mut kept = 0;
        for (_, size) in tool_images.iter().rev().take(max) {
            bytes += size;
            if bytes > MAX_REQUEST_IMAGE_BYTES {
                break;
            }
            kept += 1;
        }
        let left_out = (tool_images.len() - kept).next_multiple_of(TOOL_IMAGE_GROUP);
        // Never the newest images for the sake of a whole group.
        if kept < tool_images.len() && left_out < tool_images.len() {
            kept = tool_images.len() - left_out;
        }
        (kept, "an older tool image, no longer sent to the model; the user was shown it")
    } else {
        (0, "it is shown to the user, this model cannot receive images")
    };
    // How many of its images each result loses, oldest first.
    let mut unsent = BTreeMap::<usize, usize>::new();
    for (at, _) in &tool_images[..tool_images.len() - kept] {
        *unsent.entry(*at).or_default() += 1;
    }
    unsent
        .into_iter()
        .map(|(at, count)| {
            let source = &messages[at];
            // Built rather than cloned: the images left out are not copied.
            let mut message = AgentMessage::tool("", source.content.as_str());
            message.tool_call_id = source.tool_call_id.clone();
            // The numbers the result's own text gave its images, in order.
            for (index, image) in source.images[..count].iter().enumerate() {
                message.content.push_str(&format!(
                    "\n[image {} omitted ({}): {why}]",
                    index + 1,
                    image.mime_type.as_str()
                ));
            }
            message.images = source.images[count..].to_vec();
            (at, message)
        })
        .collect()
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
    let size = image
        .verified_bytes()
        .ok()
        .and_then(|bytes| openide_core::image_dimensions(image.mime_type, &bytes))
        .map(|(width, height)| format!(", {width}x{height}"))
        .unwrap_or_default();
    format!(
        "[Imagen adjunta: {}{size} — este modelo no puede verla]",
        image.mime_type.as_str()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::{ToolCall, UserImageMime};
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

    /// One call and its result with `sizes.len()` images of those byte sizes.
    fn capture(id: &str, sizes: &[usize]) -> [AgentMessage; 2] {
        let mut result = AgentMessage::tool(id, format!("shot {id}"));
        result.images = sizes
            .iter()
            .map(|size| {
                let png = crate::tool_media::test_png(800, 600, *size);
                UserImage::from_bytes(UserImageMime::Png, &png).unwrap()
            })
            .collect();
        [AgentMessage::assistant("", vec![call(id, "mcp__browser__shot")]), result]
    }

    fn tool_images(messages: &[AgentMessage]) -> Vec<usize> {
        messages
            .iter()
            .filter(|message| message.role == MessageRole::Tool)
            .map(|message| message.images.len())
            .collect()
    }

    #[test]
    fn a_request_carries_the_newest_tool_images_that_fit_and_a_line_for_the_rest() {
        let mut messages = vec![AgentMessage::user("go")];
        for index in 0..16 {
            messages.extend(capture(&format!("c{index}"), &[100]));
        }
        // Sixteen fit: the request is the history.
        assert_eq!(adapt_history(&messages, &ANTHROPIC).unwrap(), messages);
        // One more, and the oldest group of four leaves together.
        messages.extend(capture("c16", &[100]));
        let adapted = adapt_history(&messages, &ANTHROPIC).unwrap();
        assert_eq!(tool_images(&adapted), [vec![0; 4], vec![1; 13]].concat());
        assert_eq!(
            adapted[2].content,
            "shot c0\n[image 1 omitted (image/png): an older tool image, no longer sent to the model; the user was shown it]"
        );
        assert_eq!(adapted[10], messages[10], "a result that keeps its images is untouched");
        assert_eq!(messages[2].images.len(), 1, "the source is untouched");
        // The same four stay out until a whole new group has to leave.
        for index in 17..20 {
            messages.extend(capture(&format!("c{index}"), &[100]));
            assert_eq!(tool_images(&adapt_history(&messages, &ANTHROPIC).unwrap())[..5], [0, 0, 0, 0, 1]);
        }
        messages.extend(capture("c20", &[100]));
        assert_eq!(tool_images(&adapt_history(&messages, &ANTHROPIC).unwrap())[..9], [0, 0, 0, 0, 0, 0, 0, 0, 1]);

        // The user's own images count toward the 20 of a request and are never the ones left out.
        let png = crate::tool_media::test_png(8, 6, 0);
        let mine = UserImage::from_bytes(UserImageMime::Png, &png).unwrap();
        let mut messages = vec![AgentMessage::user_with_images("mira", vec![mine; 6]).unwrap()];
        for index in 0..16 {
            messages.extend(capture(&format!("c{index}"), &[100]));
        }
        let adapted = adapt_history(&messages, &ANTHROPIC).unwrap();
        assert_eq!(adapted[0].images.len(), 6);
        assert_eq!(tool_images(&adapted), [vec![0; 4], vec![1; 12]].concat());

        // Bytes bound the request too, and the newest capture is always sent.
        let big = MAX_REQUEST_IMAGE_BYTES / 4;
        let mut messages = vec![AgentMessage::user("go")];
        messages.extend(capture("a", &[big, big]));
        messages.extend(capture("b", &[big, big]));
        messages.extend(capture("c", &[big]));
        let adapted = adapt_history(&messages, &ANTHROPIC).unwrap();
        // Four of the five fit; the one that does not leaves with its group.
        assert_eq!(tool_images(&adapted), [0, 0, 1]);
        assert!(adapted[2].content.contains("[image 1 omitted") && adapted[2].content.contains("[image 2 omitted"));
        // Exactly the limit is sent whole.
        assert_eq!(tool_images(&adapt_history(&messages[..5], &ANTHROPIC).unwrap()), [2, 2]);
        let sent = |messages: &[AgentMessage]| -> usize {
            messages.iter().flat_map(|message| &message.images).map(|image| image.size_bytes as usize).sum()
        };
        assert_eq!(sent(&messages[..5]), MAX_REQUEST_IMAGE_BYTES);
        assert!(sent(&adapted) <= MAX_REQUEST_IMAGE_BYTES);

        // A group may end inside a result: it keeps its newer images, in order.
        let mut messages = vec![AgentMessage::user("go")];
        messages.extend(capture("a", &[100, 101]));
        messages.extend(capture("b", &[102, 103, 104, 105]));
        for index in 0..12 {
            messages.extend(capture(&format!("c{index}"), &[100]));
        }
        let adapted = adapt_history(&messages, &ANTHROPIC).unwrap();
        assert_eq!(tool_images(&adapted)[..3], [0, 2, 1]);
        // The model can tell which of the result's numbered images it still has.
        assert_eq!(adapted[4].images, messages[4].images[2..]);
        assert!(adapted[4].content.ends_with(
            "shot b\n[image 1 omitted (image/png): an older tool image, no longer sent to the model; the user was shown it]\n[image 2 omitted (image/png): an older tool image, no longer sent to the model; the user was shown it]"
        ), "{}", adapted[4].content);

        // A target without tools reads results as text, so their images are lines too.
        let plain = TargetCaps { tools: false, ..ANTHROPIC };
        let adapted = adapt_history(&messages, &plain).unwrap();
        assert!(adapted.iter().all(|message| message.images.is_empty()));
        assert!(adapted[2].content.contains("shot a\n[image 1 omitted (image/png): it is shown to the user, this model cannot receive images]"), "{}", adapted[2].content);
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
