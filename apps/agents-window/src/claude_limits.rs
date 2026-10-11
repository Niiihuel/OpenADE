//! Claude Code keeps no plan limits on disk: it only hands them, live, to its
//! status line command. With the user's consent that command becomes a small
//! script in the application's data directory, which keeps the latest payload
//! for the usage panel. Nothing here reads a credential or uses the network.

use std::path::{Path, PathBuf};

use serde_json::Value;

pub(crate) const PROVIDER_ID: &str = "claude-code";
pub(crate) const PROVIDER_NAME: &str = "Claude Code";
pub(crate) const STATE_FILE: &str = "claude-statusline.json";
const SCRIPT_FILE: &str = "claude-statusline.sh";
const SCRIPT: &str = include_str!("claude-statusline.sh");

pub(crate) const FOREIGN: &str = "Claude Code ya tiene una línea de estado configurada";
const MISSING: &str = "No se encontró settings.json de Claude Code.";
const INVALID: &str = "El settings.json de Claude Code no es un JSON válido; no se modificó.";
const LINKED: &str =
    "El settings.json de Claude Code es un enlace simbólico que administra otra herramienta; no se modificó.";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Status {
    /// Claude Code has no status line: reading can be offered.
    #[default]
    Off,
    On,
    /// A status line that is not ours; it is never replaced.
    Foreign,
    /// Its settings cannot be read safely, with the reason.
    Refused(String),
}

/// `CLAUDE_CONFIG_DIR`, or `.claude` in the home directory.
pub(crate) fn config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
}

fn command(data: &Path) -> String {
    let script = data.join(SCRIPT_FILE).display().to_string();
    format!("/bin/sh '{}'", script.replace('\'', r"'\''"))
}

/// The text added to settings.json, so that removing it restores every byte.
fn entry(data: &Path) -> String {
    format!(
        "\"statusLine\": {}",
        serde_json::json!({ "type": "command", "command": command(data) })
    )
}

fn settings(claude: &Path) -> Result<(PathBuf, String, Value), String> {
    let path = claude.join("settings.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(MISSING.into()),
        Err(_) => return Err(INVALID.into()),
    };
    let value = serde_json::from_str::<Value>(&text)
        .ok()
        .filter(Value::is_object)
        .ok_or(INVALID)?;
    Ok((path, text, value))
}

fn status_of(value: &Value, data: &Path) -> Status {
    match &value["statusLine"] {
        Value::Null => Status::Off,
        line if line["command"] == command(data).as_str() => Status::On,
        _ => Status::Foreign,
    }
}

pub(crate) fn status(claude: &Path, data: &Path) -> Status {
    match settings(claude) {
        Ok((_, _, value)) => status_of(&value, data),
        Err(reason) => Status::Refused(reason),
    }
}

fn two_space(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default() + "\n"
}

/// Replaces settings.json through a sibling file, keeping its permissions.
fn replace(path: &Path, text: &str) -> Result<(), String> {
    let failed = |_| "No se pudo escribir el settings.json de Claude Code.".to_owned();
    let metadata = std::fs::symlink_metadata(path).map_err(failed)?;
    // Renaming over a link would replace it with a file its owner no longer manages.
    if metadata.file_type().is_symlink() {
        return Err(LINKED.into());
    }
    let temporary = path.with_extension(format!("json.openade-{}", std::process::id()));
    std::fs::write(&temporary, text)
        .and_then(|()| std::fs::set_permissions(&temporary, metadata.permissions()))
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            failed(error)
        })
}

/// Makes the script Claude Code's status line. Only a settings file that
/// parses and has no status line of its own is changed, after a backup.
pub(crate) fn enable(claude: &Path, data: &Path, now_ms: u64) -> Result<(), String> {
    let (path, text, mut value) = settings(claude)?;
    match status_of(&value, data) {
        Status::Foreign => return Err(FOREIGN.into()),
        Status::Off if std::fs::symlink_metadata(&path).is_ok_and(|file| file.is_symlink()) => {
            return Err(LINKED.into())
        }
        _ => {}
    }
    let failed = |_| "No se pudo instalar el script de línea de estado.".to_owned();
    let script = data.join(SCRIPT_FILE);
    std::fs::create_dir_all(data).map_err(failed)?;
    std::fs::write(&script, SCRIPT).map_err(failed)?;
    if status_of(&value, data) == Status::On {
        return Ok(());
    }
    let mut backup = path.clone().into_os_string();
    backup.push(format!(".openade-backup-{}", now_ms / 1000));
    std::fs::copy(&path, &backup)
        .map_err(|_| "No se pudo respaldar el settings.json de Claude Code.".to_owned())?;
    let line = serde_json::json!({ "type": "command", "command": command(data) });
    // A null statusLine is replaced by the rewrite; the insertion would repeat the key.
    let had_key = value.get("statusLine").is_some();
    let empty = value.as_object().is_some_and(|object| object.is_empty());
    value["statusLine"] = line;
    // Added before the closing brace so every other byte stays as the user
    // wrote it; a result that does not parse to the same settings is rewritten.
    let inserted = text.rfind('}').filter(|_| !had_key).map(|end| {
        let body = text[..end].trim_end();
        let comma = if empty { "" } else { "," };
        format!("{body}{comma}\n  {}{}", entry(data), &text[body.len()..])
    });
    let next = inserted
        .filter(|next| serde_json::from_str::<Value>(next).is_ok_and(|parsed| parsed == value))
        .unwrap_or_else(|| two_space(&value));
    replace(&path, &next)
}

/// Removes our status line, and only ours, then the script and its state.
pub(crate) fn disable(claude: &Path, data: &Path) -> Result<(), String> {
    let (path, text, mut value) = settings(claude)?;
    if status_of(&value, data) == Status::On {
        if let Some(object) = value.as_object_mut() {
            object.remove("statusLine");
        }
        let entry = entry(data);
        let removed = [format!(",\n  {entry}"), format!("\n  {entry}")]
            .iter()
            .find(|added| text.contains(added.as_str()))
            .map(|added| text.replacen(added.as_str(), "", 1));
        let next = removed
            .filter(|next| serde_json::from_str::<Value>(next).is_ok_and(|parsed| parsed == value))
            .unwrap_or_else(|| two_space(&value));
        replace(&path, &next)?;
    }
    for file in [SCRIPT_FILE, STATE_FILE] {
        let _ = std::fs::remove_file(data.join(file));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    const PAYLOAD: &str = r#"{"model":{"id":"claude-opus","display_name":"Opus"},"rate_limits":{"five_hour":{"used_percentage":42.5,"resets_at":1790000000},"seven_day":{"used_percentage":63,"resets_at":1790400000}}}"#;

    fn run(data: &Path, stdin: &str) -> (String, bool) {
        use std::io::Write;
        std::fs::write(data.join(SCRIPT_FILE), SCRIPT).unwrap();
        let mut child = Command::new("sh")
            .arg(data.join(SCRIPT_FILE))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        (String::from_utf8(output.stdout).unwrap(), output.status.success())
    }

    #[test]
    fn script_keeps_the_payload_throttles_it_and_prints_the_tightest_window() {
        let data = tempfile::tempdir().unwrap();
        let state = data.path().join(STATE_FILE);
        assert_eq!(run(data.path(), PAYLOAD), ("Opus · 7 d 63%\n".into(), true));
        assert_eq!(std::fs::read_to_string(&state).unwrap(), PAYLOAD);
        // A second payload inside 15 s is shown but not written.
        let later = PAYLOAD.replace("42.5", "90");
        assert_eq!(run(data.path(), &later), ("Opus · 5 h 90%\n".into(), true));
        assert_eq!(std::fs::read_to_string(&state).unwrap(), PAYLOAD);
        // Once the state is older than that, it is replaced.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(&state).unwrap().set_modified(old).unwrap();
        run(data.path(), &later);
        assert_eq!(std::fs::read_to_string(&state).unwrap(), later);
        let left: Vec<_> = std::fs::read_dir(data.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name != SCRIPT_FILE && name != STATE_FILE)
            .collect();
        assert!(left.is_empty(), "temporary files left: {left:?}");
    }

    #[test]
    fn script_never_fails_and_writes_nothing_without_rate_limits() {
        for (stdin, line) in [
            (r#"{"model":{"display_name":"Sonnet"}}"#, "Sonnet\n"),
            ("", "openADE\n"),
            ("\u{0}\u{1} not json }{ \"display_name\": 7", "openADE\n"),
            (r#"{"rate_limits":{"five_hour":{"used_percentage":7}}}"#, "5 h 7%\n"),
            (r#"{"rate_limits":null}"#, "openADE\n"),
        ] {
            let data = tempfile::tempdir().unwrap();
            assert_eq!(run(data.path(), stdin), (line.into(), true), "{stdin}");
            assert_eq!(
                data.path().join(STATE_FILE).exists(),
                stdin.contains("\"rate_limits\""),
                "{stdin}"
            );
        }
    }

    fn home(settings: Option<&str>) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let (claude, data) = (root.path().join("claude"), root.path().join("data"));
        std::fs::create_dir_all(&claude).unwrap();
        if let Some(settings) = settings {
            std::fs::write(claude.join("settings.json"), settings).unwrap();
        }
        (root, claude, data)
    }

    fn files(directory: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn enabling_adds_only_the_status_line_after_a_backup_and_disabling_restores_every_byte() {
        // Four-space indent, an odd key order and no trailing newline survive.
        for original in [
            "{\n    \"zeta\": [1,2],\n    \"model\": \"opus\",\n    \"env\": {\"A\": \"}\"}\n}",
            "{}\n",
            "{\"model\":\"opus\"}",
            "{\n  \"statusLine\": null,\n  \"model\": \"opus\"\n}\n",
        ] {
            let (_root, claude, data) = home(Some(original));
            let path = claude.join("settings.json");
            assert_eq!(status(&claude, &data), Status::Off);
            enable(&claude, &data, 1_700_000_000_000).unwrap();
            assert_eq!(status(&claude, &data), Status::On, "{original}");
            let enabled = std::fs::read_to_string(&path).unwrap();
            let mut expected: Value = serde_json::from_str(original).unwrap();
            expected["statusLine"] =
                serde_json::json!({ "type": "command", "command": command(&data) });
            assert_eq!(serde_json::from_str::<Value>(&enabled).unwrap(), expected);
            let null_line = original.contains("statusLine");
            if !null_line {
                assert!(enabled.starts_with(original.trim_end().trim_end_matches('}').trim_end()));
            }
            assert_eq!(
                files(&claude),
                ["settings.json", "settings.json.openade-backup-1700000000"]
            );
            assert_eq!(
                std::fs::read_to_string(claude.join("settings.json.openade-backup-1700000000"))
                    .unwrap(),
                original
            );
            assert_eq!(std::fs::read_to_string(data.join(SCRIPT_FILE)).unwrap(), SCRIPT);
            // Enabling again changes nothing and makes no second backup.
            enable(&claude, &data, 1_800_000_000_000).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), enabled);
            assert_eq!(files(&claude).len(), 2);

            std::fs::write(data.join(STATE_FILE), PAYLOAD).unwrap();
            disable(&claude, &data).unwrap();
            assert_eq!(status(&claude, &data), Status::Off);
            assert!(files(&data).is_empty());
            let disabled = std::fs::read_to_string(&path).unwrap();
            if null_line {
                assert_eq!(disabled, "{\n  \"model\": \"opus\"\n}\n");
            } else {
                assert_eq!(disabled, original);
            }
        }
    }

    #[test]
    fn a_foreign_status_line_and_unreadable_settings_are_never_touched() {
        let foreign = "{\"statusLine\":{\"type\":\"command\",\"command\":\"my-line\"},\"a\":1}";
        for (settings, enabled, state) in [
            (Some(foreign), FOREIGN, Status::Foreign),
            (Some("{ not json"), INVALID, Status::Refused(INVALID.into())),
            (Some("[1]"), INVALID, Status::Refused(INVALID.into())),
            (None, MISSING, Status::Refused(MISSING.into())),
        ] {
            let (_root, claude, data) = home(settings);
            let before = files(&claude);
            assert_eq!(status(&claude, &data), state);
            assert_eq!(enable(&claude, &data, 1).unwrap_err(), enabled);
            assert_eq!(files(&claude), before, "no backup, nothing created");
            assert!(!data.exists(), "no script without a status line of ours");
            std::fs::create_dir_all(&data).unwrap();
            std::fs::write(data.join(STATE_FILE), PAYLOAD).unwrap();
            // Disabling leaves a foreign line alone; unreadable settings stop it.
            assert_eq!(disable(&claude, &data).is_ok(), settings == Some(foreign));
            assert_eq!(files(&claude), before);
            assert_eq!(
                settings.map(str::to_owned),
                std::fs::read_to_string(claude.join("settings.json")).ok()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn linked_settings_are_refused_and_permissions_are_kept() {
        use std::os::unix::fs::PermissionsExt;
        let (root, claude, data) = home(None);
        let target = root.path().join("managed.json");
        std::fs::write(&target, "{}\n").unwrap();
        std::os::unix::fs::symlink(&target, claude.join("settings.json")).unwrap();
        assert_eq!(enable(&claude, &data, 1).unwrap_err(), LINKED);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{}\n");
        assert_eq!(files(&claude), ["settings.json"]);
        assert!(!data.exists());

        let (_root, claude, data) = home(Some("{}\n"));
        let path = claude.join("settings.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        enable(&claude, &data, 1).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn the_command_quotes_a_data_directory_with_spaces_or_quotes() {
        assert_eq!(
            command(Path::new("/tmp/o'ne two")),
            r"/bin/sh '/tmp/o'\''ne two/claude-statusline.sh'"
        );
    }
}
