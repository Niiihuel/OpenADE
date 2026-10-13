//! A commit message for the staged changes, written by a model on request.
//! Nothing is committed here: the message goes to the form for the user to
//! read, change and commit.

use crate::git_branches::run_git;
use openide_core::AgentMessage;
use openide_harness::ModelBackend;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// What of the staged diff the model is shown. A larger one is cut, and the
/// model is told; the summary of files before it is always whole.
const MAX_DIFF_BYTES: usize = 48 * 1024;
const MAX_MESSAGE_BYTES: usize = 4 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);

const INSTRUCTIONS: &str = "You write the commit message for the staged changes of a Git repository. Answer with the message alone: no preface, no quotes, no code fence. A subject line of at most 72 characters; then, only if the change needs it, a blank line and a short body that says what changed and why. Write it in the language and style of the repository's recent subjects, which are given. Describe only what the diff shows.";

fn git_output(workspace: &Path, args: &[&str]) -> Result<String, String> {
    let output = run_git(workspace, args)?;
    if !output.status.success() {
        return Err("Git no pudo leer los cambios preparados".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn cut(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The request for the model: the files changed, the diff and the subjects
/// the repository's last commits were given.
fn request(workspace: &Path) -> Result<Vec<AgentMessage>, String> {
    let summary = git_output(workspace, &["diff", "--cached", "--stat", "--no-color", "--no-ext-diff"])?;
    if summary.trim().is_empty() {
        return Err("No hay cambios preparados para describir".into());
    }
    let diff = git_output(workspace, &["diff", "--cached", "--no-color", "--no-ext-diff"])?;
    // An unborn branch has no subjects to follow.
    let recent = git_output(workspace, &["log", "-8", "--format=%s"]).unwrap_or_default();
    let shown = cut(&diff, MAX_DIFF_BYTES);
    let note = if shown.len() < diff.len() {
        "\n[The diff is longer than this: the rest was cut.]"
    } else {
        ""
    };
    Ok(vec![
        AgentMessage::system(INSTRUCTIONS),
        AgentMessage::user(format!(
            "Recent subjects:\n{}\n\nFiles changed:\n{}\nDiff:\n{shown}{note}",
            cut(&recent, 2048),
            cut(&summary, 8 * 1024),
        )),
    ])
}

/// The model's answer as a commit message: without a fence or quotes a
/// model may put around it anyway, and within the size a form takes.
pub(crate) fn clean(answer: &str) -> Option<String> {
    let mut text = answer.trim();
    if let Some(fenced) = text.strip_prefix("```") {
        // The fence's language tag, if any, ends at the first line break.
        text = fenced.split_once('\n').map_or("", |(_, rest)| rest);
        text = text.trim_end().strip_suffix("```").unwrap_or(text).trim();
    }
    for quote in ['"', '\'', '`'] {
        if let Some(inner) = text.strip_prefix(quote).and_then(|rest| rest.strip_suffix(quote)) {
            text = inner.trim();
        }
    }
    let text = cut(text, MAX_MESSAGE_BYTES).trim_end();
    (!text.is_empty()).then(|| text.to_owned())
}

/// One request in flight. Dropping it cancels the model's answer.
pub(crate) struct Generation {
    pub workspace: PathBuf,
    receiver: Receiver<Result<String, String>>,
    cancel: CancellationToken,
}

impl Generation {
    /// Reads the staged changes and asks `model`, off the UI thread.
    pub(crate) fn start(model: Arc<dyn ModelBackend>, workspace: PathBuf) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel();
        let cancel = CancellationToken::new();
        let (root, stop) = (workspace.clone(), cancel.clone());
        std::thread::Builder::new()
            .name("openide-commit-message".into())
            .spawn(move || {
                let answer = request(&root).and_then(|messages| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| error.to_string())?;
                    runtime.block_on(async {
                        match tokio::time::timeout(TIMEOUT, model.complete(&messages, &[], stop)).await {
                            Ok(Ok(completion)) => clean(&completion.content)
                                .ok_or_else(|| "El modelo no devolvió un mensaje".to_owned()),
                            Ok(Err(error)) => Err(format!("No se pudo generar el mensaje: {error}")),
                            Err(_) => Err("El modelo tardó demasiado en generar el mensaje".to_owned()),
                        }
                    })
                });
                let _ = sender.send(answer);
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { workspace, receiver, cancel })
    }

    /// The message, or why there is none, once: `None` while it is awaited.
    pub(crate) fn poll(&self) -> Option<Result<String, String>> {
        match self.receiver.try_recv() {
            Ok(answer) => Some(answer),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("La generación del mensaje se interrumpió".into())),
        }
    }
}

impl Drop for Generation {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_models_answer_is_taken_as_the_message_without_its_wrapping() {
        assert_eq!(clean("  Arregla el foco\n\nEl campo lo perdía al abrir.  ").as_deref(), Some("Arregla el foco\n\nEl campo lo perdía al abrir."));
        assert_eq!(clean("```\nArregla el foco\n```").as_deref(), Some("Arregla el foco"));
        assert_eq!(clean("```text\nArregla el foco\n\nCuerpo\n```\n").as_deref(), Some("Arregla el foco\n\nCuerpo"));
        assert_eq!(clean("\"Arregla el foco\"").as_deref(), Some("Arregla el foco"));
        assert_eq!(clean("   \n"), None);
        assert_eq!(clean("```\n```"), None);
        // Bounded, at a character boundary.
        let long = clean(&"ñ".repeat(MAX_MESSAGE_BYTES)).unwrap();
        assert!(long.len() <= MAX_MESSAGE_BYTES && long.chars().all(|letter| letter == 'ñ'));
    }

    struct Fixed(&'static str);

    #[async_trait::async_trait]
    impl ModelBackend for Fixed {
        fn model_id(&self) -> &str {
            "fixed"
        }

        async fn complete(
            &self,
            messages: &[AgentMessage],
            tools: &[openide_harness::ToolDefinition],
            _: CancellationToken,
        ) -> Result<openide_harness::ModelOutput, openide_harness::AgentError> {
            // The model is given the diff and no tool to act with.
            assert!(tools.is_empty() && messages[1].content.contains("+hola"));
            Ok(openide_harness::ModelOutput { content: self.0.into(), tool_calls: Vec::new() })
        }
    }

    #[test]
    fn a_generation_answers_once_with_the_message_for_the_staged_changes() {
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| assert!(run_git(root.path(), args).unwrap().status.success(), "{args:?}");
        git(&["init", "--quiet"]);
        std::fs::write(root.path().join("nota.txt"), "hola\n").unwrap();
        git(&["add", "nota.txt"]);
        let wait = |generation: &Generation| {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                if let Some(answer) = generation.poll() {
                    return answer;
                }
                assert!(std::time::Instant::now() < deadline, "no answer");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let generation = Generation::start(Arc::new(Fixed("```\nAgrega la nota\n```")), root.path().to_owned()).unwrap();
        assert_eq!(wait(&generation), Ok("Agrega la nota".to_owned()));
        // A model that answers nothing is an error, not an empty message.
        let empty = Generation::start(Arc::new(Fixed("  ")), root.path().to_owned()).unwrap();
        assert!(wait(&empty).is_err());
    }

    #[test]
    fn the_request_shows_the_staged_diff_and_refuses_when_nothing_is_staged() {
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| assert!(run_git(root.path(), args).unwrap().status.success(), "{args:?}");
        git(&["init", "--quiet"]);
        assert!(request(root.path()).is_err());
        std::fs::write(root.path().join("nota.txt"), "hola\n").unwrap();
        // Not staged yet: there is nothing to describe.
        assert!(request(root.path()).is_err());
        git(&["add", "nota.txt"]);
        let messages = request(root.path()).unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages[1].content.contains("nota.txt") && messages[1].content.contains("+hola"), "{}", messages[1].content);
    }
}
