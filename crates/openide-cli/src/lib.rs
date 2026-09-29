//! Configurable, local CLI backends for the agent window.
//!
//! Profiles launch an executable directly in the selected workspace. They never
//! invoke a shell to interpolate a prompt. Output is forwarded as it arrives,
//! and each run emits the same session events used by the native harness.

use openide_core::{AgentEvent, AgentEventKind, AgentRunRequest, OutputStream};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod codex_discovery;
pub use codex_discovery::{
    discover_codex_models, discover_codex_models_with_executable, DiscoveredCodexModel,
};

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(30);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CODEX_EFFORTS_WITH_ULTRA: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];
const CODEX_EFFORTS_THROUGH_MAX: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const CODEX_EFFORTS_THROUGH_XHIGH: &[&str] = &["low", "medium", "high", "xhigh"];
const CODEX_KNOWN_MODELS: &[(&str, &str)] = &[
    ("gpt-6-astra", "GPT-6 Astra"),
    ("gpt-6-sol", "GPT-6 Sol"),
    ("gpt-6-luna", "GPT-6 Luna"),
    ("gpt-5.6-sol", "GPT-5.6 Sol"),
    ("gpt-5.6-terra", "GPT-5.6 Terra"),
    ("gpt-5.6-luna", "GPT-5.6 Luna"),
];

/// Public model choices observed in the installed Codex CLI bundled catalog.
/// Codex also supports a user's configured default and custom providers.
pub fn codex_known_models() -> &'static [(&'static str, &'static str)] {
    CODEX_KNOWN_MODELS
}

/// Levels advertised by the installed Codex CLI's bundled catalog for its
/// public GPT-6 and GPT-5.6 models. Unknown/custom models return `None` and
/// remain subject to validation by Codex itself.
pub fn codex_supported_efforts(model: &str) -> Option<&'static [&'static str]> {
    match model {
        "gpt-6-astra" | "gpt-6-sol" | "gpt-5.6-sol" | "gpt-5.6-terra" => {
            Some(CODEX_EFFORTS_WITH_ULTRA)
        }
        "gpt-6-luna" | "gpt-5.6-luna" => Some(CODEX_EFFORTS_THROUGH_MAX),
        "gpt-5.5" => Some(CODEX_EFFORTS_THROUGH_XHIGH),
        _ => None,
    }
}

/// How a CLI receives the prompt. `Argument` replaces one `{prompt}` token in
/// `args`; `Stdin` writes the prompt to the child's standard input and closes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptTransport {
    Stdin,
    Argument,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliProfile {
    pub id: String,
    pub label: String,
    pub executable: PathBuf,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub prompt_transport: PromptTransport,
    /// Optional per-run model override. Currently only Codex CLI has a
    /// verified model flag; other profiles reject this setting explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional per-run reasoning effort. The Codex config key is passed as a
    /// separate process argument, never through a shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

impl CliProfile {
    /// Built-in headless profiles. Users can change their arguments or add
    /// profiles without rebuilding OpenIDE.
    pub fn builtins() -> Vec<Self> {
        vec![
            Self::preset(
                "codex",
                "Codex CLI",
                "codex",
                &["exec", "--skip-git-repo-check", "-"],
                PromptTransport::Stdin,
            ),
            Self::preset(
                "claude",
                "Claude Code",
                "claude",
                &["-p"],
                PromptTransport::Stdin,
            ),
            Self::preset(
                "gemini",
                "Gemini CLI",
                "gemini",
                &[],
                PromptTransport::Stdin,
            ),
            Self::preset(
                "zcode",
                "ZCode CLI",
                "zcode",
                &[
                    "--prompt",
                    "{prompt}",
                    "--mode",
                    "build",
                    "--output-format",
                    "text",
                ],
                PromptTransport::Argument,
            ),
        ]
    }

    pub fn preset(
        id: impl Into<String>,
        label: impl Into<String>,
        executable: impl Into<PathBuf>,
        args: &[&str],
        prompt_transport: PromptTransport,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            executable: executable.into(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            env: BTreeMap::new(),
            prompt_transport,
            model: None,
            reasoning_effort: None,
        }
    }

    /// Select a model for a built-in Codex CLI run. A model is passed as one
    /// `--model=<slug>` argument, so it cannot be interpreted as a CLI switch.
    pub fn with_model(mut self, model: impl Into<String>) -> Result<Self, String> {
        let model = model.into();
        validate_codex_model(&self, &model)?;
        self.model = Some(model);
        Ok(self)
    }

    /// Select a Codex reasoning effort for this run. Availability still
    /// depends on the selected model and installed Codex client.
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Result<Self, String> {
        let effort = effort.into();
        validate_codex_effort(&self, &effort)?;
        self.reasoning_effort = Some(effort);
        Ok(self)
    }

    /// Resolve an installed executable from an absolute/relative path or PATH.
    pub fn detect_executable(&self) -> Option<PathBuf> {
        detect_executable(&self.executable)
    }
}

/// Detect an executable without invoking a shell. On Windows, PATHEXT is used
/// so npm's `.cmd` shims and native `.exe` installations can both be found.
pub fn detect_executable(executable: impl AsRef<Path>) -> Option<PathBuf> {
    let executable = executable.as_ref();
    if executable.as_os_str().is_empty() {
        return None;
    }

    let has_directory = executable
        .parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty());
    if has_directory || executable.is_absolute() {
        return executable_candidates(executable).find(|candidate| executable_file(candidate));
    }

    if let Some(path) = env::var_os("PATH") {
        if let Some(found) = env::split_paths(&path)
            .flat_map(|directory| executable_candidates(&directory.join(executable)))
            .find(|candidate| executable_file(candidate))
        {
            return Some(found);
        }
    }
    #[cfg(windows)]
    if matches!(executable.to_str(), Some("codex" | "codex.exe")) {
        return env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .and_then(|root| codex_windows_install(&root));
    }
    None
}

#[cfg(windows)]
fn codex_windows_install(local_app_data: &Path) -> Option<PathBuf> {
    let bin = local_app_data.join("OpenAI").join("Codex").join("bin");
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(bin).ok()?.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let candidate = entry.path().join("codex.exe");
        if executable_file(&candidate) {
            let modified = candidate
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok());
            candidates.push((modified, candidate));
        }
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    candidates.pop().map(|(_, path)| path)
}

fn executable_candidates(path: &Path) -> impl Iterator<Item = PathBuf> {
    let mut candidates = vec![path.to_path_buf()];
    #[cfg(windows)]
    if path.extension().is_none() {
        let extensions = env::var_os("PATHEXT")
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_owned());
        for extension in extensions.split(';').filter(|part| !part.is_empty()) {
            candidates.push(path.with_extension(extension.trim_start_matches('.')));
            candidates
                .push(path.with_extension(extension.trim_start_matches('.').to_ascii_lowercase()));
        }
    }
    candidates.into_iter()
}

fn executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return path
            .metadata()
            .is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0);
    }
    #[cfg(not(unix))]
    true
}

pub struct CliRunHandle {
    pub session_id: String,
    pub events: mpsc::Receiver<AgentEvent>,
    cancelled: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl CliRunHandle {
    /// Request cancellation and terminate the active process. This is safe to
    /// call before the executable has been spawned or after it has exited.
    pub fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(mut child) = self.child.lock() {
            if let Some(child) = child.as_mut() {
                terminate_child(child);
            }
        }
    }
}

impl Drop for CliRunHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Default)]
pub struct CliRunner;

impl CliRunner {
    pub fn start(profile: CliProfile, request: AgentRunRequest) -> CliRunHandle {
        let session_id = request.session_id.clone().unwrap_or_else(new_session_id);
        let (sender, events) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_child = Arc::clone(&child);
        let worker_session_id = session_id.clone();
        thread::spawn(move || {
            run_profile(
                profile,
                request,
                worker_session_id,
                sender,
                worker_cancelled,
                worker_child,
            );
        });
        CliRunHandle {
            session_id,
            events,
            cancelled,
            child,
        }
    }
}

fn new_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let ordinal = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    format!("cli-{nanos:x}-{:x}-{ordinal:x}", std::process::id())
}

struct EventEmitter {
    sender: mpsc::Sender<AgentEvent>,
    session_id: String,
    sequence: u64,
}

impl EventEmitter {
    fn emit(&mut self, kind: AgentEventKind) {
        self.sequence = self.sequence.saturating_add(1);
        let _ = self.sender.send(AgentEvent::new(
            self.session_id.clone(),
            self.sequence,
            kind,
        ));
    }
}

fn run_profile(
    profile: CliProfile,
    request: AgentRunRequest,
    session_id: String,
    sender: mpsc::Sender<AgentEvent>,
    cancelled: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<Child>>>,
) {
    let mut emitter = EventEmitter {
        sender,
        session_id,
        sequence: request.sequence_start,
    };
    emitter.emit(AgentEventKind::SessionStarted {
        workspace: request.workspace.clone(),
    });
    emitter.emit(AgentEventKind::TurnStarted {
        prompt: request.prompt.clone(),
    });

    if cancelled.load(Ordering::Acquire) {
        emitter.emit(AgentEventKind::TurnCancelled);
        return;
    }
    if !request.workspace.is_dir() {
        emitter.emit(AgentEventKind::TurnFailed {
            error: format!(
                "Workspace is not a directory: {}",
                request.workspace.display()
            ),
        });
        return;
    }

    let Some(executable) = profile.detect_executable() else {
        emitter.emit(AgentEventKind::TurnFailed {
            error: format!(
                "{} executable was not found: {}",
                profile.label,
                profile.executable.display()
            ),
        });
        return;
    };
    let prompt = render_prompt(&request);
    let args = match render_args(&profile, &prompt) {
        Ok(args) => args,
        Err(error) => {
            emitter.emit(AgentEventKind::TurnFailed { error });
            return;
        }
    };

    if matches!(profile.prompt_transport, PromptTransport::Argument)
        && is_shell_executable(&executable)
    {
        emitter.emit(AgentEventKind::TurnFailed {
            error: format!(
                "{} resolves to a shell or batch shim. Prompts as shell arguments are unsafe; select a native executable or a stdin profile.",
                profile.label
            ),
        });
        return;
    }

    if cancelled.load(Ordering::Acquire) {
        emitter.emit(AgentEventKind::TurnCancelled);
        return;
    }
    let mut command = Command::new(&executable);
    command
        .args(args)
        .current_dir(&request.workspace)
        .stdin(
            if matches!(profile.prompt_transport, PromptTransport::Stdin) {
                Stdio::piped()
            } else {
                Stdio::null()
            },
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(&profile.env);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut process = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            emitter.emit(AgentEventKind::TurnFailed {
                error: format!("Could not start {}: {error}", profile.label),
            });
            return;
        }
    };
    let stdout = process.stdout.take().expect("piped stdout");
    let stderr = process.stderr.take().expect("piped stderr");
    let stdin = process.stdin.take();
    let mut termination_requested = false;
    {
        let mut slot = child_slot.lock().expect("child mutex poisoned");
        *slot = Some(process);
        if cancelled.load(Ordering::Acquire) {
            if let Some(child) = slot.as_mut() {
                terminate_child(child);
                termination_requested = true;
            }
        }
    }

    let (output_tx, output_rx) = mpsc::channel();
    spawn_reader(stdout, OutputStream::Stdout, output_tx.clone());
    spawn_reader(stderr, OutputStream::Stderr, output_tx.clone());
    drop(output_tx);

    let input_error = if let Some(mut stdin) = stdin {
        let result = stdin.write_all(prompt.as_bytes());
        drop(stdin);
        result.err()
    } else {
        None
    };

    let mut stdout_capture = String::new();
    let mut stderr_capture = String::new();
    let mut exit_status = None;
    let mut wait_finished = false;
    let mut exit_seen_at = None;
    let mut reader_count = 2;
    let mut read_error = None;

    while reader_count > 0 || !wait_finished {
        match output_rx.recv_timeout(POLL_INTERVAL) {
            Ok(ReaderMessage::Chunk(stream, text)) => {
                let visible = match stream {
                    OutputStream::Stdout => append_limited(&mut stdout_capture, &text),
                    OutputStream::Stderr => append_limited(&mut stderr_capture, &text),
                };
                if !visible.is_empty() {
                    emitter.emit(AgentEventKind::ProcessOutput {
                        stream,
                        text: visible,
                    });
                }
            }
            Ok(ReaderMessage::Finished) => reader_count -= 1,
            Ok(ReaderMessage::Error(stream, error)) => {
                read_error = Some(format!("Could not read {stream:?}: {error}"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => reader_count = 0,
        }

        if !wait_finished {
            let mut slot = child_slot.lock().expect("child mutex poisoned");
            if let Some(child) = slot.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        exit_status = Some(status);
                        wait_finished = true;
                        exit_seen_at = Some(Instant::now());
                    }
                    Ok(None) => {
                        if cancelled.load(Ordering::Acquire) && !termination_requested {
                            terminate_child(child);
                            termination_requested = true;
                        }
                    }
                    Err(error) => {
                        read_error = Some(format!("Could not wait for {}: {error}", profile.label));
                        terminate_child(child);
                        wait_finished = true;
                        exit_seen_at = Some(Instant::now());
                    }
                }
            }
        }
        // A descendant may inherit a pipe after the main CLI exits. Let the
        // readers drain briefly, then finish the turn instead of hanging UI.
        if exit_seen_at.is_some_and(|at| at.elapsed() > Duration::from_secs(2)) {
            break;
        }
    }
    *child_slot.lock().expect("child mutex poisoned") = None;

    if cancelled.load(Ordering::Acquire) {
        emitter.emit(AgentEventKind::TurnCancelled);
    } else if let Some(error) = read_error.or_else(|| input_error.map(|error| error.to_string())) {
        emitter.emit(AgentEventKind::TurnFailed { error });
    } else if exit_status.is_some_and(|status| status.success()) {
        emitter.emit(AgentEventKind::TurnCompleted {
            response: stdout_capture,
            steps: 1,
        });
    } else {
        let status = exit_status.and_then(|status| status.code()).map_or_else(
            || "unknown status".to_owned(),
            |code| format!("exit code {code}"),
        );
        let detail = stderr_capture.trim();
        emitter.emit(AgentEventKind::TurnFailed {
            error: if detail.is_empty() {
                format!("{} failed ({status})", profile.label)
            } else {
                format!("{} failed ({status}): {detail}", profile.label)
            },
        });
    }
}

fn render_prompt(request: &AgentRunRequest) -> String {
    if request.history.is_empty() {
        return request.prompt.clone();
    }
    let mut prompt = String::from("Previous conversation:\n");
    for message in &request.history {
        prompt.push_str(&format!("{:?}: {}\n", message.role, message.content));
    }
    prompt.push_str("\nCurrent request:\n");
    prompt.push_str(&request.prompt);
    prompt
}

fn render_args(profile: &CliProfile, prompt: &str) -> Result<Vec<String>, String> {
    let slots = profile
        .args
        .iter()
        .filter(|arg| arg.contains("{prompt}"))
        .count();
    match profile.prompt_transport {
        PromptTransport::Stdin if slots > 0 => {
            Err("Stdin profiles cannot contain {prompt} in arguments".to_owned())
        }
        PromptTransport::Argument if slots != 1 => {
            Err("Argument profiles need exactly one {prompt} token".to_owned())
        }
        PromptTransport::Stdin => render_model_options(profile, profile.args.clone()),
        PromptTransport::Argument => render_model_options(
            profile,
            profile
                .args
                .iter()
                .map(|arg| arg.replace("{prompt}", prompt))
                .collect(),
        ),
    }
}

fn validate_codex_profile(profile: &CliProfile) -> Result<(), String> {
    if profile.id != "codex" || profile.args.first().is_none_or(|arg| arg != "exec") {
        return Err(format!(
            "Model and reasoning settings are supported only for the Codex CLI exec profile, not {}",
            profile.label
        ));
    }
    Ok(())
}

fn validate_codex_model(profile: &CliProfile, model: &str) -> Result<(), String> {
    validate_codex_profile(profile)?;
    if model.is_empty()
        || model.starts_with('-')
        || model.len() > 128
        || !model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/+-".contains(&byte))
    {
        return Err("Codex model must be a model slug of at most 128 ASCII characters".into());
    }
    Ok(())
}

fn validate_codex_effort(profile: &CliProfile, effort: &str) -> Result<(), String> {
    validate_codex_profile(profile)?;
    if !codex_discovery::effort_is_safe(effort) {
        return Err(format!("Unsupported Codex reasoning effort: {effort}"));
    }
    Ok(())
}

fn render_model_options(
    profile: &CliProfile,
    mut args: Vec<String>,
) -> Result<Vec<String>, String> {
    if let Some(model) = &profile.model {
        validate_codex_model(profile, model)?;
        args.insert(1, format!("--model={model}"));
    }
    if let Some(effort) = &profile.reasoning_effort {
        validate_codex_effort(profile, effort)?;
        // The quoted value is TOML syntax for Codex's `--config key=value`.
        // Effort is an allowlisted token, and both pieces are passed directly
        // to Command::args, without shell expansion or prompt substitution.
        args.splice(
            1..1,
            [
                "--config".to_owned(),
                format!("model_reasoning_effort=\"{effort}\""),
            ],
        );
    }
    Ok(args)
}

fn append_limited(target: &mut String, chunk: &str) -> String {
    if target.len() >= OUTPUT_LIMIT {
        return String::new();
    }
    let before = target.len();
    let remaining = OUTPUT_LIMIT - target.len();
    if chunk.len() <= remaining {
        target.push_str(chunk);
    } else {
        let boundary = chunk.floor_char_boundary(remaining);
        target.push_str(&chunk[..boundary]);
        target.push_str("\n[output truncated]");
    }
    target[before..].to_owned()
}

enum ReaderMessage {
    Chunk(OutputStream, String),
    Finished,
    Error(OutputStream, io::Error),
}

fn spawn_reader(
    mut reader: impl Read + Send + 'static,
    stream: OutputStream,
    sender: mpsc::Sender<ReaderMessage>,
) {
    thread::spawn(move || {
        let mut bytes = [0_u8; 4096];
        let mut decoder = Utf8Decoder::default();
        loop {
            match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => {
                    let text = decoder.decode(&bytes[..count], false);
                    if !text.is_empty() && sender.send(ReaderMessage::Chunk(stream, text)).is_err()
                    {
                        return;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let _ = sender.send(ReaderMessage::Error(stream, error));
                    break;
                }
            }
        }
        let remainder = decoder.decode(&[], true);
        if !remainder.is_empty() {
            let _ = sender.send(ReaderMessage::Chunk(stream, remainder));
        }
        let _ = sender.send(ReaderMessage::Finished);
    });
}

#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn decode(&mut self, bytes: &[u8], end: bool) -> String {
        self.pending.extend_from_slice(bytes);
        let mut output = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    output.push_str(valid);
                    self.pending.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    output.push_str(
                        std::str::from_utf8(&self.pending[..valid]).expect("valid prefix"),
                    );
                    self.pending.drain(..valid);
                    if let Some(invalid) = error.error_len() {
                        output.push('\u{FFFD}');
                        self.pending.drain(..invalid);
                    } else {
                        if end {
                            output.push('\u{FFFD}');
                            self.pending.clear();
                        }
                        break;
                    }
                }
            }
        }
        output
    }
}

fn is_shell_executable(path: &Path) -> bool {
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    [
        "cmd",
        "cmd.exe",
        "powershell",
        "powershell.exe",
        "pwsh",
        "pwsh.exe",
        "sh",
        "bash",
        "zsh",
    ]
    .iter()
    .any(|shell| name.eq_ignore_ascii_case(shell))
        || (cfg!(windows)
            && path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
                }))
}

fn terminate_child(child: &mut Child) {
    #[cfg(windows)]
    {
        // npm CLI shims may have spawned a Node child. Kill the whole process
        // tree when taskkill is available; Child::kill remains the fallback.
        let mut command = Command::new("taskkill");
        command
            .args(["/F", "/T", "/PID"])
            .arg(child.id().to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        let _ = command.status();
    }
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::AgentMessage;

    #[test]
    fn stream_events_stop_at_capture_limit() {
        let mut captured = String::new();
        let first = append_limited(&mut captured, &"a".repeat(OUTPUT_LIMIT + 10));
        assert_eq!(first, captured);
        assert!(captured.ends_with("[output truncated]"));
        assert!(append_limited(&mut captured, "later").is_empty());
    }

    fn request(prompt: &str) -> AgentRunRequest {
        AgentRunRequest {
            session_id: Some("existing-session".to_owned()),
            sequence_start: 17,
            workspace: env::current_dir().expect("current directory"),
            history: Vec::new(),
            prompt: prompt.to_owned(),
            mode: openide_core::TurnMode::Build,
        }
    }

    fn mock_profile(mode: &str) -> CliProfile {
        let mut profile = CliProfile::preset(
            "mock",
            "Mock CLI",
            env::current_exe().expect("test executable"),
            &["--exact", "tests::cli_child_mock", "--nocapture"],
            PromptTransport::Stdin,
        );
        profile
            .env
            .insert("OPENIDE_CLI_MOCK_MODE".to_owned(), mode.to_owned());
        profile
    }

    fn receive_until_terminal(handle: &CliRunHandle) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        loop {
            let event = handle
                .events
                .recv_timeout(Duration::from_secs(15))
                .expect("terminal event within timeout");
            let terminal = matches!(
                event.kind,
                AgentEventKind::TurnCompleted { .. }
                    | AgentEventKind::TurnFailed { .. }
                    | AgentEventKind::TurnCancelled
            );
            events.push(event);
            if terminal {
                return events;
            }
        }
    }

    #[test]
    fn cli_child_mock() {
        let Ok(mode) = env::var("OPENIDE_CLI_MOCK_MODE") else {
            return;
        };
        let mut prompt = String::new();
        io::stdin()
            .read_to_string(&mut prompt)
            .expect("mock prompt on stdin");
        match mode.as_str() {
            "echo" => {
                print!("stdout: {prompt}");
                io::stdout().flush().expect("flush stdout");
                eprint!("stderr: diagnostic");
                io::stderr().flush().expect("flush stderr");
            }
            "codex_diagnostics" => {
                eprintln!("WARN codex_skills: icon path is invalid");
                eprintln!("WARN shell_snapshot: shell snapshot unavailable");
                eprintln!("OpenAI Codex CLI");
                io::stderr().flush().expect("flush stderr");
                println!("Respuesta válida a {prompt}");
                io::stdout().flush().expect("flush stdout");
            }
            "failed_after_output" => {
                println!("Partial response");
                io::stdout().flush().expect("flush stdout");
                eprintln!("fatal CLI error");
                io::stderr().flush().expect("flush stderr");
                std::process::exit(17);
            }
            "wait" => {
                println!("ready");
                io::stdout().flush().expect("flush stdout");
                thread::sleep(Duration::from_secs(10));
            }
            _ => panic!("unknown mock mode"),
        }
    }

    #[test]
    fn streams_both_outputs_and_preserves_session_sequence() {
        let mut request = request("¡Hola, mundo! 😀");
        request.history.push(AgentMessage::user("Contexto"));
        let handle = CliRunner::start(mock_profile("echo"), request);
        let events = receive_until_terminal(&handle);
        assert_eq!(handle.session_id, "existing-session");
        assert!(events
            .iter()
            .all(|event| event.session_id == "existing-session"));
        assert!(events
            .iter()
            .enumerate()
            .all(|(index, event)| event.sequence == 18 + index as u64));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ProcessOutput { stream: OutputStream::Stdout, text }
                if text.contains("¡Hola, mundo! 😀")
        )));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ProcessOutput { stream: OutputStream::Stderr, text }
                if text.contains("diagnostic")
        )));
        assert!(matches!(
            &events.last().expect("last event").kind,
            AgentEventKind::TurnCompleted { response, .. } if response.contains("¡Hola, mundo! 😀")
        ));
    }

    #[test]
    fn codex_style_stderr_diagnostics_do_not_fail_a_successful_turn() {
        let handle = CliRunner::start(mock_profile("codex_diagnostics"), request("hola"));
        let events = receive_until_terminal(&handle);
        let diagnostics = events
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::ProcessOutput {
                    stream: OutputStream::Stderr,
                    text,
                } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert!(diagnostics.contains("codex_skills"));
        assert!(diagnostics.contains("shell_snapshot"));
        assert!(diagnostics.contains("OpenAI Codex CLI"));
        assert!(matches!(
            &events.last().expect("terminal event").kind,
            AgentEventKind::TurnCompleted { response, .. }
                if response.contains("Respuesta válida a hola")
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::TurnFailed { .. })));
    }

    #[test]
    fn nonzero_exit_fails_even_when_stdout_contains_a_response() {
        let handle = CliRunner::start(mock_profile("failed_after_output"), request("hola"));
        let events = receive_until_terminal(&handle);
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ProcessOutput {
                stream: OutputStream::Stdout,
                text,
            } if text.contains("Partial response")
        )));
        assert!(matches!(
            &events.last().expect("terminal event").kind,
            AgentEventKind::TurnFailed { error }
                if error.contains("exit code 17") && error.contains("fatal CLI error")
        ));
    }

    #[test]
    fn missing_executable_reports_a_terminal_failure() {
        let profile = CliProfile::preset(
            "missing",
            "Missing CLI",
            "openide-cli-command-that-does-not-exist",
            &[],
            PromptTransport::Stdin,
        );
        let handle = CliRunner::start(profile, request("test"));
        let events = receive_until_terminal(&handle);
        assert!(matches!(
            &events.last().expect("last event").kind,
            AgentEventKind::TurnFailed { error } if error.contains("not found")
        ));
    }

    #[test]
    fn zcode_preset_uses_explicit_build_mode() {
        let zcode = CliProfile::builtins()
            .into_iter()
            .find(|profile| profile.id == "zcode")
            .expect("ZCode preset");
        assert!(zcode
            .args
            .windows(2)
            .any(|args| args == ["--mode", "build"]));
    }

    #[test]
    fn codex_model_and_effort_are_distinct_arguments_before_stdin_marker() {
        let profile = CliProfile::builtins()
            .into_iter()
            .find(|profile| profile.id == "codex")
            .unwrap()
            .with_model("gpt-6-sol")
            .unwrap()
            .with_reasoning_effort("high")
            .unwrap();
        let args = render_args(&profile, "prompt with spaces; $(unsafe)").unwrap();
        assert_eq!(args[0], "exec");
        assert!(args.iter().any(|arg| arg == "--model=gpt-6-sol"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--config", "model_reasoning_effort=\"high\""]));
        assert_eq!(args.last().unwrap(), "-");
        assert!(!args.iter().any(|arg| arg.contains("prompt with spaces")));
    }

    #[test]
    fn cli_options_reject_invalid_or_unsupported_profiles() {
        let codex = CliProfile::builtins()
            .into_iter()
            .find(|profile| profile.id == "codex")
            .unwrap();
        assert!(codex.clone().with_model("--help").is_err());
        assert!(codex.clone().with_model("bad\nmodel").is_err());
        assert!(codex.with_reasoning_effort("high\" --help").is_err());

        let custom =
            CliProfile::preset("custom", "Custom", "custom", &["-"], PromptTransport::Stdin);
        assert!(custom.clone().with_model("gpt-6-sol").is_err());
        assert!(custom.with_reasoning_effort("high").is_err());
    }

    #[test]
    fn deserialized_model_options_are_validated_at_run_time() {
        let mut profile = CliProfile::builtins()
            .into_iter()
            .find(|profile| profile.id == "codex")
            .unwrap();
        profile.model = Some("gpt-6-sol".into());
        profile.reasoning_effort = Some("invalid".into());
        assert!(render_args(&profile, "x")
            .unwrap_err()
            .contains("Unsupported"));
    }

    #[test]
    fn bundled_codex_capabilities_are_fallback_only() {
        assert!(codex_known_models()
            .iter()
            .any(|(slug, label)| *slug == "gpt-6-sol" && *label == "GPT-6 Sol"));
        assert_eq!(
            codex_supported_efforts("gpt-6-luna"),
            Some(CODEX_EFFORTS_THROUGH_MAX)
        );
        let profile = CliProfile::builtins()
            .into_iter()
            .find(|profile| profile.id == "codex")
            .unwrap();
        // The installed Codex process validates the live model/effort pair.
        // A bundled catalog may be stale relative to model/list, so this
        // wrapper only enforces safe argument syntax.
        assert!(profile
            .clone()
            .with_model("gpt-6-luna")
            .unwrap()
            .with_reasoning_effort("ultra")
            .is_ok());
        assert!(profile
            .with_reasoning_effort("ultra")
            .unwrap()
            .with_model("gpt-6-luna")
            .is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn finds_codex_desktop_binary_under_versioned_windows_bin() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("OpenAI").join("Codex").join("bin");
        let versioned = bin.join("installation-hash");
        std::fs::create_dir_all(&versioned).unwrap();
        let executable = versioned.join("codex.exe");
        std::fs::write(&executable, b"mock executable").unwrap();
        assert_eq!(codex_windows_install(root.path()), Some(executable));
    }

    #[test]
    fn cancellation_ends_a_running_cli() {
        let handle = CliRunner::start(mock_profile("wait"), request("wait"));
        loop {
            let event = handle
                .events
                .recv_timeout(Duration::from_secs(15))
                .expect("mock readiness event");
            if matches!(
                event.kind,
                AgentEventKind::ProcessOutput { stream: OutputStream::Stdout, ref text }
                    if text.contains("ready")
            ) {
                break;
            }
        }
        handle.cancel();
        let events = receive_until_terminal(&handle);
        assert!(matches!(
            events.last().expect("last event").kind,
            AgentEventKind::TurnCancelled
        ));
    }

    #[test]
    fn dropping_a_running_handle_cancels_its_child() {
        let handle = CliRunner::start(mock_profile("wait"), request("wait"));
        loop {
            let event = handle
                .events
                .recv_timeout(Duration::from_secs(15))
                .expect("mock readiness event");
            if matches!(
                event.kind,
                AgentEventKind::ProcessOutput { stream: OutputStream::Stdout, ref text }
                    if text.contains("ready")
            ) {
                break;
            }
        }
        let cancelled = Arc::clone(&handle.cancelled);
        let child = Arc::clone(&handle.child);
        drop(handle);
        assert!(cancelled.load(Ordering::Acquire));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let finished = {
                let mut child = child.lock().expect("child mutex poisoned");
                child
                    .as_mut()
                    .is_none_or(|process| process.try_wait().expect("child status").is_some())
            };
            if finished {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the CLI child stayed alive after Drop"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn decoder_preserves_utf8_across_chunks() {
        let mut decoder = Utf8Decoder::default();
        assert_eq!(decoder.decode(&[0xF0, 0x9F], false), "");
        assert_eq!(decoder.decode(&[0x98, 0x80], false), "😀");
        assert_eq!(decoder.decode(&[], true), "");
    }
}
