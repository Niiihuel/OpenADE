//! Image generation as one in-process MCP tool, bound like the browser's.
//!
//! A call spends the user's money and writes a file. The environment lists
//! the tool among those that always ask, and before that approval nothing
//! here reads a credential or sends a request.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use openide_core::{image_dimensions, UserImageMime, MAX_TOOL_MEDIA_BYTES};
use openide_harness::{
    atomic_write, writable_workspace_path, AgentError, McpCallResult, McpError, McpTool,
    McpToolClient,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{
    native_provider::ProviderRegistry, provider_connections,
    settings_preferences::SettingsPreferences,
};

pub(crate) const SERVER: &str = "openide_image";
/// The name the model calls, and the one every view of the call matches.
pub(crate) const TOOL: &str = "mcp__openide_image__generate_image";
pub(crate) const PROVIDER: &str = "OpenAI";
pub(crate) const DEFAULT_QUALITY: &str = "medium";
const SIZES: [&str; 4] = ["auto", "1024x1024", "1536x1024", "1024x1536"];
const MAX_PROMPT_CHARS: usize = 4000;
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;

pub(crate) struct ImageModel {
    pub id: &'static str,
    pub label: &'static str,
    /// The quality levels the model takes, lowest first.
    pub qualities: &'static [&'static str],
}

const QUALITIES: &[&str] = &["low", "medium", "high"];
const EXTENDED_QUALITIES: &[&str] = &["low", "medium", "high", "xhigh", "max"];

// shortcut: a static list taken from OpenAI's SDK types on 2026-10-12, read it
// from provider discovery once a provider lists its image models.
/// The first one is the default.
pub(crate) const IMAGE_MODELS: [ImageModel; 5] = [
    ImageModel { id: "gpt-image-2", label: "GPT Image 2", qualities: QUALITIES },
    ImageModel {
        id: "gpt-image-2.5-sunburst",
        label: "GPT Image 2.5 Sunburst",
        qualities: EXTENDED_QUALITIES,
    },
    ImageModel {
        id: "gpt-image-2.5-flare",
        label: "GPT Image 2.5 Flare",
        qualities: EXTENDED_QUALITIES,
    },
    ImageModel { id: "gpt-image-1.5", label: "GPT Image 1.5", qualities: QUALITIES },
    ImageModel { id: "gpt-image-1-mini", label: "GPT Image 1 mini", qualities: QUALITIES },
];

/// The effort selector's scale, worded for "calidad".
pub(crate) fn quality_label(quality: &str) -> &str {
    match quality {
        "low" => "Baja",
        "medium" => "Media",
        "high" => "Alta",
        "xhigh" => "Muy alta",
        "max" => "Máxima",
        other => other,
    }
}

/// What a call sends for the stored preference: a model this list lacks is
/// the default one, and a quality the model lacks is its highest.
pub(crate) fn image_choice(model: &str, quality: &str) -> (&'static ImageModel, &'static str) {
    let model = IMAGE_MODELS
        .iter()
        .find(|known| known.id == model)
        .unwrap_or(&IMAGE_MODELS[0]);
    let quality = model
        .qualities
        .iter()
        .copied()
        .find(|level| *level == quality)
        .unwrap_or(model.qualities[model.qualities.len() - 1]);
    (model, quality)
}

#[derive(Clone)]
pub(crate) struct ImageMcpClient {
    workspace: PathBuf,
    /// Where the user's model and quality are read when a call runs.
    data_root: PathBuf,
    endpoint: String,
    /// The ChatGPT plan route, whose refusal reads differently.
    plan: bool,
    /// Called once per approved call, off the async runtime.
    token: Arc<dyn Fn() -> Result<String, String> + Send + Sync>,
}

impl ImageMcpClient {
    pub(crate) fn new(
        workspace: &Path,
        data_root: &Path,
        endpoint: &str,
        plan: bool,
        token: impl Fn() -> Result<String, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
            data_root: data_root.to_path_buf(),
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            plan,
            token: Arc::new(token),
        }
    }

    /// The route that generates images for this user: an OpenAI API key when
    /// one is present, else the ChatGPT plan. Only presence is checked here.
    pub(crate) fn connected(data_root: &Path, workspace: &Path) -> Option<Self> {
        Self::connected_with(data_root, workspace, |name| {
            provider_connections::read_key(data_root, name).is_some()
        })
    }

    fn connected_with(
        data_root: &Path,
        workspace: &Path,
        key_present: impl Fn(&str) -> bool,
    ) -> Option<Self> {
        let registry = ProviderRegistry::load(data_root).ok()?;
        if let Some(openai) = registry
            .adapter("openai")
            .filter(|adapter| key_present(&adapter.key_env))
        {
            let (root, name) = (data_root.to_path_buf(), openai.key_env.clone());
            return Some(Self::new(workspace, data_root, &openai.endpoint, false, move || {
                provider_connections::read_key(&root, &name)
                    .ok_or_else(|| "Falta la clave de API de OpenAI".to_owned())
            }));
        }
        // Unverified: no real account confirmed that the images endpoint
        // accepts the token of a ChatGPT sign-in. Its refusal names the plan.
        let codex = registry.adapter("codex")?;
        let token = provider_connections::codex_token_source(data_root.to_path_buf()).ok()?;
        Some(Self::new(workspace, data_root, &codex.endpoint, true, token))
    }

    /// Names the route and never a credential: a pending call or a session
    /// approval does not carry over to another route.
    pub(crate) fn identity_digest(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"openide-image-generation-v1\0");
        hash.update([u8::from(self.plan)]);
        hash.update(self.endpoint.as_bytes());
        format!("{:x}", hash.finalize())
    }

    fn target(&self, path: &str) -> Result<PathBuf, String> {
        writable_workspace_path(&self.workspace, path).map_err(|error| {
            let reason = match error {
                AgentError::Tool(reason) => reason,
                other => other.to_string(),
            };
            format!("No se puede guardar en {path}: {reason}")
        })
    }

    async fn generate(&self, arguments: &Value) -> Result<McpCallResult, String> {
        let text = |key: &str| arguments.get(key).and_then(Value::as_str);
        let prompt = text("prompt")
            .filter(|prompt| {
                !prompt.trim().is_empty() && prompt.chars().count() <= MAX_PROMPT_CHARS
            })
            .ok_or("prompt debe tener entre 1 y 4000 caracteres")?;
        let requested = text("path")
            .filter(|path| path.to_ascii_lowercase().ends_with(".png"))
            .ok_or("path debe ser una ruta que termine en .png")?;
        let size = match arguments.get("size") {
            None => SIZES[0],
            Some(size) => size
                .as_str()
                .filter(|size| SIZES.contains(size))
                .ok_or_else(|| format!("size debe ser uno de: {}", SIZES.join(", ")))?,
        };
        // Whatever can refuse the call does so before anything is spent.
        let target = self.target(requested)?;
        if fs::symlink_metadata(&target).is_ok() {
            return Err(format!(
                "{requested} ya existe y no se reemplaza; elegí otro nombre. No se envió ninguna solicitud."
            ));
        }
        let preferences = SettingsPreferences::load(&self.data_root);
        let (model, quality) = image_choice(&preferences.image_model, &preferences.image_quality);
        let token = self.token.clone();
        let token = tokio::task::spawn_blocking(move || token())
            .await
            .map_err(|_| "No se pudo leer la credencial".to_owned())??;
        let response = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(180))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "No se pudo preparar la solicitud".to_owned())?
            .post(format!("{}/images/generations", self.endpoint))
            .bearer_auth(&token)
            .json(&json!({
                "model": model.id, "prompt": prompt, "size": size, "quality": quality, "n": 1,
            }))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    format!("La solicitud a {PROVIDER} no terminó a tiempo; si la generación había empezado, el proveedor puede haberla cobrado.")
                } else {
                    format!("No se pudo contactar a {PROVIDER}. Revisá la conexión a Internet.")
                }
            })?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            // The provider's own sentence, bounded; never its whole answer.
            let detail = read_body(response, MAX_ERROR_BYTES)
                .await
                .ok()
                .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
                .and_then(|body| Some(body.pointer("/error/message")?.as_str()?.replace(&token, "…")))
                .map(|message| format!(": {}", message.chars().take(300).collect::<String>()))
                .unwrap_or_default();
            return Err(if self.plan && matches!(status, 401 | 403) {
                format!("El plan de ChatGPT no permite generar imágenes con esta conexión (HTTP {status}{detail})")
            } else {
                format!("{PROVIDER} rechazó la generación (HTTP {status}{detail})")
            });
        }
        let image = read_body(response, MAX_RESPONSE_BYTES)
            .await
            .ok()
            .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
            .and_then(|body| {
                STANDARD
                    .decode(body.pointer("/data/0/b64_json")?.as_str()?.trim())
                    .ok()
            })
            .filter(|image| image.starts_with(b"\x89PNG\r\n\x1a\n"))
            .ok_or_else(|| {
                format!("{PROVIDER} no devolvió una imagen PNG válida de hasta 32 MiB; no se guardó nada y el proveedor puede haber cobrado la generación.")
            })?;
        // Nothing awaits from here on: a cancelled turn cannot drop an image
        // that was already received.
        let made_with = format!("{} · calidad {}", model.label, quality_label(quality).to_lowercase());
        Ok(self.save(requested, &image, &made_with))
    }

    /// The image is paid for. A file that appeared at the path while it was
    /// generated moves it to `-2`, `-3`… beside it, and a write that fails
    /// still returns the picture. The text keeps what it was made with.
    fn save(&self, requested: &str, image: &[u8], made_with: &str) -> McpCallResult {
        let path = Path::new(requested);
        let name = |part: Option<&std::ffi::OsStr>| {
            part.map_or_else(|| "png".into(), |part| part.to_string_lossy().into_owned())
        };
        let (stem, extension) = (name(path.file_stem()), name(path.extension()));
        let mut saved = Err(String::new());
        for attempt in 1..100 {
            let candidate = if attempt == 1 {
                requested.to_owned()
            } else {
                path.with_file_name(format!("{stem}-{attempt}.{extension}"))
                    .to_string_lossy()
                    .into_owned()
            };
            let outcome = self.target(&candidate).and_then(|target| {
                // Claiming the name first means the rename below can only
                // replace this empty placeholder, never somebody's file.
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                    .map_err(|error| error.to_string())?;
                atomic_write(&target, image).map_err(|error| {
                    let _ = fs::remove_file(&target);
                    error.to_string()
                })?;
                Ok(candidate)
            });
            // Of the failures, the one at the path that was asked for is told.
            if outcome.is_ok() || attempt == 1 {
                saved = outcome;
            }
            if saved.is_ok() {
                break;
            }
        }
        let size = image_dimensions(UserImageMime::Png, image)
            .map(|(width, height)| format!("{width}x{height}, "))
            .unwrap_or_default();
        let detail = format!("({size}{:.1} MB, {made_with})", image.len() as f64 / 1e6);
        let mut text = match &saved {
            Ok(path) if path == requested => format!("Imagen guardada en {path} {detail}"),
            Ok(path) => format!(
                "Imagen guardada en {path} {detail}: {requested} apareció mientras se generaba y no se reemplazó"
            ),
            Err(error) => format!("La imagen {detail} se generó pero no quedó en disco: {error}"),
        };
        let mut content = Vec::new();
        if image.len() <= MAX_TOOL_MEDIA_BYTES {
            content.push(json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(image)}));
        } else {
            text.push_str(". La vista previa no se muestra: la imagen supera los 8 MiB");
        }
        content.insert(0, json!({"type":"text","text":text}));
        result(content, saved.is_err())
    }
}

fn result(content: Vec<Value>, is_error: bool) -> McpCallResult {
    McpCallResult {
        content,
        is_error,
        structured_content: None,
        extra: BTreeMap::new(),
    }
}

async fn read_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, ()> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if body.len() + chunk.len() > limit {
            return Err(());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(crate) fn image_mcp_tools() -> Vec<McpTool> {
    vec![McpTool {
        name: "generate_image".into(),
        description: Some("Generate one image from a text prompt and save it as a PNG file inside the workspace, with the image model and quality the user chose in Settings. It costs the user money, so the user is asked to approve every call. Returns the path the image was saved at; an existing file is never replaced.".into()),
        input_schema: json!({
            "type": "object",
            "properties": {
                "prompt": {"type": "string", "minLength": 1, "maxLength": MAX_PROMPT_CHARS, "description": "What the image shows."},
                "path": {"type": "string", "minLength": 5, "maxLength": 4096, "description": "Where to save it: a path ending in .png, relative to the workspace or absolute inside it. Its directory must already exist and the file must not."},
                "size": {"type": "string", "enum": SIZES, "description": "Defaults to auto."},
            },
            "required": ["prompt", "path"],
            "additionalProperties": false,
        }),
        extra: BTreeMap::new(),
    }]
}

#[async_trait]
impl McpToolClient for ImageMcpClient {
    async fn list_tools(&self, cancellation: &CancellationToken) -> Result<Vec<McpTool>, McpError> {
        if cancellation.is_cancelled() {
            return Err(McpError::Cancelled);
        }
        Ok(image_mcp_tools())
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        cancellation: &CancellationToken,
    ) -> Result<McpCallResult, McpError> {
        if name != "generate_image" {
            return Err(McpError::UnadvertisedTool(name.to_owned()));
        }
        // Dropping the request is all a cancellation can do: the provider
        // may still finish and charge an image nobody receives.
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(McpError::Cancelled),
            outcome = self.generate(&arguments) => Ok(outcome.unwrap_or_else(|error| {
                result(vec![json!({"type":"text","text":error})], true)
            })),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::Mutex,
    };

    const TOKEN: &str = "fixture-token";

    /// A ChatGPT sign-in saved under `root`, valid for the next hour.
    pub(crate) fn connect_plan(root: &Path) {
        openide_connections::save(
            root,
            &openide_connections::Credential {
                version: 1,
                provider_id: "codex".into(),
                access_token: TOKEN.into(),
                refresh_token: Some("fixture-refresh".into()),
                client_id: Some("fixture-client".into()),
                subject: Some("fixture-subject".into()),
                expires_at: Some(openide_connections::now() + 3600),
                scopes: vec!["chatgpt.tokens.use.direct".into()],
                oauth: true,
                ..Default::default()
            },
        )
        .unwrap();
    }

    /// A provider that gives every request the same answer and keeps what it
    /// was sent.
    fn provider(status: u16, body: String) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for socket in listener.incoming() {
                let mut socket = socket.unwrap();
                let (mut request, mut buffer) = (Vec::new(), [0; 8192]);
                loop {
                    let count = socket.read(&mut buffer).unwrap();
                    request.extend_from_slice(&buffer[..count]);
                    let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                    let complete = text.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .map_or(0, |length| length.parse().unwrap());
                        body.len() >= length
                    });
                    if complete || count == 0 {
                        break;
                    }
                }
                seen.lock().unwrap().push(String::from_utf8_lossy(&request).into_owned());
                write!(socket, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        (endpoint, requests)
    }

    fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::new(3, 2)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    fn generated(image: &[u8]) -> String {
        json!({"data":[{"b64_json":STANDARD.encode(image)}]}).to_string()
    }

    struct Fixture {
        workspace: tempfile::TempDir,
        root: tempfile::TempDir,
        requests: Arc<Mutex<Vec<String>>>,
        client: ImageMcpClient,
    }

    fn fixture(status: u16, body: String, plan: bool) -> Fixture {
        let (workspace, root) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (endpoint, requests) = provider(status, body);
        let client =
            ImageMcpClient::new(workspace.path(), root.path(), &endpoint, plan, || Ok(TOKEN.into()));
        Fixture { workspace, root, requests, client }
    }

    impl Fixture {
        /// Whether the tool failed, and its text block.
        async fn call(&self, arguments: Value) -> (bool, String, usize) {
            let result = self
                .client
                .call_tool("generate_image", arguments, &CancellationToken::new())
                .await
                .unwrap();
            let text = result.content[0]["text"].as_str().unwrap().to_owned();
            assert!(!text.contains(TOKEN), "{text}");
            (result.is_error, text, result.content.len())
        }

        fn sent(&self) -> Vec<Value> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .map(|request| serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap())
                .collect()
        }
    }

    #[tokio::test]
    async fn an_approved_call_saves_the_png_and_returns_it_with_its_path() {
        let image = png();
        let fixture = fixture(200, generated(&image), false);
        std::fs::create_dir(fixture.workspace.path().join("assets")).unwrap();
        let result = fixture
            .client
            .call_tool(
                "generate_image",
                json!({"prompt":"Un faro al amanecer","path":"assets/hero.png","size":"1536x1024"}),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(
            result.content,
            [
                json!({"type":"text","text":"Imagen guardada en assets/hero.png (3x2, 0.0 MB, GPT Image 2 · calidad media)"}),
                json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(&image)}),
            ]
        );
        assert_eq!(std::fs::read(fixture.workspace.path().join("assets/hero.png")).unwrap(), image);
        // As the executor hands it on, the card shows the picture and its line.
        let shown = crate::tool_result_media::preview(TOOL, &serde_json::to_string(&result).unwrap()).unwrap();
        assert_eq!(
            (shown.images.len(), shown.text.as_str()),
            (1, "Imagen guardada en assets/hero.png (3x2, 0.0 MB, GPT Image 2 · calidad media)")
        );
        assert!(shown.notices.is_empty());
        // Nothing but the image is left beside it.
        assert_eq!(std::fs::read_dir(fixture.workspace.path().join("assets")).unwrap().count(), 1);
        let requests = fixture.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        let head = requests[0].to_ascii_lowercase();
        assert!(head.starts_with("post /v1/images/generations "), "{head}");
        assert!(head.contains(&format!("authorization: bearer {TOKEN}\r\n")));
        assert_eq!(
            fixture.sent(),
            [json!({
                "model":"gpt-image-2","prompt":"Un faro al amanecer","size":"1536x1024",
                "quality":"medium","n":1,
            })]
        );
    }

    #[tokio::test]
    async fn the_request_carries_the_model_and_quality_the_user_saved() {
        let fixture = fixture(200, generated(&png()), false);
        let save = |model: &str, quality: &str| {
            SettingsPreferences {
                image_model: model.into(),
                image_quality: quality.into(),
                ..SettingsPreferences::default()
            }
            .save(fixture.root.path())
            .unwrap()
        };
        save("gpt-image-2.5-flare", "max");
        assert!(!fixture.call(json!({"prompt":"a","path":"a.png"})).await.0);
        // A level the chosen model lacks is sent as its highest one, and the
        // preference is read again for every call.
        save("gpt-image-1-mini", "max");
        assert!(!fixture.call(json!({"prompt":"b","path":"b.png"})).await.0);
        let sent = fixture.sent();
        assert_eq!(
            (&sent[0]["model"], &sent[0]["quality"], &sent[0]["size"]),
            (&json!("gpt-image-2.5-flare"), &json!("max"), &json!("auto"))
        );
        assert_eq!((&sent[1]["model"], &sent[1]["quality"]), (&json!("gpt-image-1-mini"), &json!("high")));
    }

    #[test]
    fn a_quality_the_model_lacks_is_its_highest_and_an_unknown_model_is_the_default() {
        let choice = |model, quality| {
            let (model, quality) = image_choice(model, quality);
            (model.id, quality)
        };
        assert_eq!(choice("gpt-image-2", "xhigh"), ("gpt-image-2", "high"));
        assert_eq!(choice("gpt-image-2.5-sunburst", "xhigh"), ("gpt-image-2.5-sunburst", "xhigh"));
        assert_eq!(choice("gpt-image-1.5", "low"), ("gpt-image-1.5", "low"));
        assert_eq!(choice("retired-model", "medium"), ("gpt-image-2", "medium"));
    }

    #[tokio::test]
    async fn a_call_that_cannot_be_saved_is_refused_before_any_request() {
        let fixture = fixture(200, generated(&png()), false);
        let workspace = fixture.workspace.path();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(workspace.join("taken.png"), b"mine").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), workspace.join("link")).unwrap();
        let escaped = outside.path().join("absolute.png");
        for (arguments, reason) in [
            (json!({"prompt":"a","path":"taken.png"}), "ya existe"),
            (json!({"prompt":"a","path":"../escaped.png"}), "parent traversal"),
            (json!({"prompt":"a","path":escaped}), "outside the workspace"),
            #[cfg(unix)]
            (json!({"prompt":"a","path":"link/through.png"}), "link"),
            (json!({"prompt":"a","path":"missing/dir.png"}), "No se puede guardar"),
            (json!({"prompt":"a","path":"notes.txt"}), ".png"),
            (json!({"prompt":"  ","path":"a.png"}), "prompt"),
            (json!({"prompt":"a".repeat(4001),"path":"a.png"}), "prompt"),
            (json!({"prompt":"a","path":"a.png","size":"4096x4096"}), "size"),
        ] {
            let (failed, text, blocks) = fixture.call(arguments.clone()).await;
            assert!(failed && blocks == 1 && text.contains(reason), "{arguments}: {text}");
        }
        assert!(fixture.requests.lock().unwrap().is_empty());
        assert_eq!(std::fs::read(workspace.join("taken.png")).unwrap(), b"mine");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_refusal_gives_the_providers_message_and_never_the_credential() {
        let refusal = json!({"error":{"message":format!("Token {TOKEN} lacks the image scope. {}", "x".repeat(600)),"code":"forbidden"}}).to_string();
        for (plan, expected) in [
            (true, "El plan de ChatGPT no permite generar imágenes con esta conexión (HTTP 403: Token … lacks the image scope."),
            (false, "OpenAI rechazó la generación (HTTP 403: Token … lacks the image scope."),
        ] {
            let fixture = fixture(403, refusal.clone(), plan);
            let (failed, text, _) = fixture.call(json!({"prompt":"a","path":"a.png"})).await;
            assert!(failed && text.starts_with(expected), "{text}");
            assert!(text.chars().count() < 420 && !text.contains("forbidden"), "{text}");
            assert_eq!(std::fs::read_dir(fixture.workspace.path()).unwrap().count(), 0);
        }
        // An answer without a message still says which status it was.
        let fixture = fixture(500, "<html>upstream</html>".into(), true);
        let (failed, text, _) = fixture.call(json!({"prompt":"a","path":"a.png"})).await;
        assert!(failed && text == "OpenAI rechazó la generación (HTTP 500)", "{text}");
    }

    #[tokio::test]
    async fn an_answer_that_is_not_a_png_saves_nothing() {
        for body in [
            json!({"data":[{"b64_json":"not base64!"}]}).to_string(),
            generated(b"GIF89a not a png"),
            json!({"data":[]}).to_string(),
            "{".to_owned(),
        ] {
            let fixture = fixture(200, body, false);
            let (failed, text, _) = fixture.call(json!({"prompt":"a","path":"a.png"})).await;
            assert!(failed && text.contains("no devolvió una imagen PNG válida"), "{text}");
            assert_eq!(std::fs::read_dir(fixture.workspace.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn a_file_that_appeared_meanwhile_is_kept_and_the_image_goes_beside_it() {
        let fixture = fixture(200, String::new(), false);
        let workspace = fixture.workspace.path();
        let image = png();
        std::fs::write(workspace.join("hero.png"), b"mine").unwrap();
        std::fs::create_dir(workspace.join("hero-2.png")).unwrap();
        let result = fixture.client.save("hero.png", &image, "GPT Image 2 · calidad alta");
        assert!(!result.is_error && result.content.len() == 2);
        let text = result.content[0]["text"].as_str().unwrap();
        assert_eq!(
            text,
            "Imagen guardada en hero-3.png (3x2, 0.0 MB, GPT Image 2 · calidad alta): hero.png apareció mientras se generaba y no se reemplazó"
        );
        assert_eq!(std::fs::read(workspace.join("hero.png")).unwrap(), b"mine");
        assert_eq!(std::fs::read(workspace.join("hero-3.png")).unwrap(), image);
        // A directory that is gone leaves no file: the picture is still returned.
        let lost = fixture.client.save("gone/hero.png", &image, "GPT Image 2 · calidad alta");
        assert!(lost.is_error && lost.content.len() == 2);
        assert!(lost.content[0]["text"].as_str().unwrap().contains("se generó pero no quedó en disco: No se puede guardar en gone/hero.png:"));
        // Too large for the inline preview: saved, and the text says so.
        let mut large = image.clone();
        large.resize(MAX_TOOL_MEDIA_BYTES + 1, 0);
        let result = fixture.client.save("large.png", &large, "GPT Image 2 · calidad alta");
        assert!(!result.is_error && result.content.len() == 1);
        assert!(result.content[0]["text"].as_str().unwrap().ends_with("La vista previa no se muestra: la imagen supera los 8 MiB"));
        assert_eq!(std::fs::metadata(workspace.join("large.png")).unwrap().len() as usize, large.len());
    }

    #[tokio::test]
    async fn a_cancelled_turn_stops_waiting_for_the_provider() {
        // Accepted by the system and never answered.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (workspace, root) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let client = ImageMcpClient::new(
            workspace.path(),
            root.path(),
            &format!("http://{}/v1", listener.local_addr().unwrap()),
            true,
            || Ok(TOKEN.into()),
        );
        let cancellation = CancellationToken::new();
        let stop = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            stop.cancel();
        });
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            client.call_tool("generate_image", json!({"prompt":"a","path":"a.png"}), &cancellation),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Err(McpError::Cancelled)));
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_route_is_the_api_key_when_there_is_one_and_else_the_chatgpt_plan() {
        let (root, workspace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let route = |key: bool| {
            ImageMcpClient::connected_with(root.path(), workspace.path(), |name| {
                key && name == "OPENAI_API_KEY"
            })
            .map(|client| (client.plan, client.endpoint.clone(), client.identity_digest()))
        };
        assert!(route(false).is_none());
        let (plan, endpoint, key_identity) = route(true).unwrap();
        assert!(!plan && endpoint == "https://api.openai.com/v1");
        connect_plan(root.path());
        let (plan, endpoint, plan_identity) = route(false).unwrap();
        assert!(plan && endpoint == "https://api.openai.com/v1");
        assert!(key_identity != plan_identity && !plan_identity.contains(TOKEN));
        // The key wins while both are connected.
        assert!(!route(true).unwrap().0);
    }
}
