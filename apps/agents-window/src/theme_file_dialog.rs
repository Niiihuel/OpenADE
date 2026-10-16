//! Native theme file choosers, run on the app's background picker threads.
//! Save dialogs own overwrite confirmation; this module never writes files.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
enum Mode<'a> {
    Import,
    Export(&'a str),
}

impl Mode<'_> {
    fn title(self) -> &'static str {
        match self {
            Self::Import => "Importar tema",
            Self::Export(_) => "Exportar tema YAML",
        }
    }
}

pub(crate) fn choose_import(initial: &Path) -> Result<Option<PathBuf>, String> {
    choose(initial, Mode::Import)
}

pub(crate) fn choose_export(initial: &Path, filename: &str) -> Result<Option<PathBuf>, String> {
    if filename.is_empty()
        || Path::new(filename).file_name() != Some(filename.as_ref())
        || filename.as_bytes().contains(&0)
        || !yaml_extension(Path::new(filename))
    {
        return Err("El nombre de exportación debe ser un archivo YAML".into());
    }
    choose(initial, Mode::Export(filename))
}

fn choose(initial: &Path, mode: Mode<'_>) -> Result<Option<PathBuf>, String> {
    let directory = initial
        .ancestors()
        .find(|path| path.is_dir())
        .unwrap_or(Path::new("/"));
    validate_selection(choose_native(directory, mode)?, mode)
}

fn yaml_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml")
        })
}

fn validate_selection(
    selection: Option<PathBuf>,
    mode: Mode<'_>,
) -> Result<Option<PathBuf>, String> {
    let Some(path) = selection else {
        return Ok(None);
    };
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err("Selecciona un archivo local con ruta absoluta".into());
    }
    match mode {
        Mode::Import => {
            let json = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
            if !path.is_file() || (!yaml_extension(&path) && !json) {
                return Err("Selecciona un archivo YAML, YML o JSON existente".into());
            }
        }
        Mode::Export(_) => {
            // Never append an extension after the dialog: it could name an
            // existing file the user has not confirmed replacing.
            if !yaml_extension(&path)
                || !path.parent().is_some_and(Path::is_dir)
                || (path.exists() && !path.is_file())
            {
                return Err("Selecciona un destino YAML en una carpeta existente".into());
            }
        }
    }
    Ok(Some(path))
}

#[cfg(target_os = "linux")]
enum PortalOutcome {
    Selected(Option<PathBuf>),
    Unavailable(String),
}

#[cfg(target_os = "linux")]
fn choose_native(initial: &Path, mode: Mode<'_>) -> Result<Option<PathBuf>, String> {
    let portal_error = match choose_portal(initial, mode)? {
        PortalOutcome::Selected(selection) => return Ok(selection),
        PortalOutcome::Unavailable(error) => error,
    };
    let kde = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .any(|desktop| desktop.eq_ignore_ascii_case("kde"));
    let backends = if kde {
        ["kdialog", "zenity"]
    } else {
        ["zenity", "kdialog"]
    };
    let mut errors = vec![format!("Portal: {portal_error}")];
    for backend in backends {
        match choose_command(backend, initial, mode) {
            Ok(selection) => return Ok(selection), // Cancellation never opens another chooser.
            Err(error) => errors.push(error),
        }
    }
    Err(format!("No se pudo abrir el selector de archivos. Configura el portal de tu escritorio o instala KDialog/Zenity. {}", errors.join(" · ")))
}

#[cfg(target_os = "linux")]
fn local_uri(value: &str) -> Result<PathBuf, String> {
    let uri = reqwest::Url::parse(value).map_err(|_| "El portal devolvió una URI inválida")?;
    if uri.scheme() != "file"
        || uri.query().is_some()
        || uri.fragment().is_some()
        || uri
            .host_str()
            .is_some_and(|host| !host.eq_ignore_ascii_case("localhost") && !host.is_empty())
    {
        return Err("Selecciona un archivo local de este equipo".into());
    }
    let path = uri
        .to_file_path()
        .map_err(|_| "Selecciona un archivo local de este equipo")?;
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err("El portal devolvió una ruta inválida".into());
    }
    Ok(path)
}

#[cfg(target_os = "linux")]
fn choose_portal(initial: &Path, mode: Mode<'_>) -> Result<PortalOutcome, String> {
    use ashpd::desktop::{
        file_chooser::{FileChooserProxy, FileFilter, SelectedFiles},
        ResponseError,
    };
    use std::time::Duration;

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return Ok(PortalOutcome::Unavailable(error.to_string())),
    };
    runtime.block_on(async {
        // A fresh connection avoids retaining IO tasks from a previous picker runtime.
        let connection =
            match tokio::time::timeout(Duration::from_secs(5), ashpd::zbus::Connection::session())
                .await
            {
                Ok(Ok(connection)) => connection,
                Ok(Err(error)) => return Ok(PortalOutcome::Unavailable(error.to_string())),
                Err(_) => {
                    return Ok(PortalOutcome::Unavailable(
                        "El bus de escritorio no respondió".into(),
                    ))
                }
            };
        match tokio::time::timeout(
            Duration::from_secs(5),
            FileChooserProxy::with_connection(connection.clone()),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Ok(PortalOutcome::Unavailable(error.to_string())),
            Err(_) => {
                return Ok(PortalOutcome::Unavailable(
                    "El portal de archivos no respondió".into(),
                ))
            }
        }
        let yaml = FileFilter::new("Temas YAML")
            .glob("*.[yY][aA][mM][lL]")
            .glob("*.[yY][mM][lL]");
        let response = match mode {
            Mode::Import => SelectedFiles::open_file()
                .connection(Some(connection))
                .title(mode.title())
                .accept_label("Importar")
                .modal(true)
                .multiple(false)
                .filter(yaml)
                .filter(FileFilter::new("Temas JSON").glob("*.[jJ][sS][oO][nN]"))
                .current_folder(initial)
                .map_err(|error| error.to_string())?
                .send()
                .await
                .and_then(|request| request.response()),
            Mode::Export(filename) => SelectedFiles::save_file()
                .connection(Some(connection))
                .title(mode.title())
                .accept_label("Exportar")
                .modal(true)
                .current_name(filename)
                .filter(yaml)
                .current_folder(initial)
                .map_err(|error| error.to_string())?
                .send()
                .await
                .and_then(|request| request.response()),
        };
        match response {
            Ok(response) => {
                if response.uris().len() != 1 {
                    return Err("El portal debe devolver un único archivo".into());
                }
                local_uri(response.uris()[0].as_str())
                    .map(|path| PortalOutcome::Selected(Some(path)))
            }
            Err(ashpd::Error::Response(ResponseError::Cancelled)) => {
                Ok(PortalOutcome::Selected(None))
            }
            Err(error) => Ok(PortalOutcome::Unavailable(error.to_string())),
        }
    })
}

#[cfg(target_os = "linux")]
fn command(backend: &str, initial: &Path, mode: Mode<'_>) -> std::process::Command {
    let mut command = crate::child_process::command_no_window(backend);
    let filter = match mode {
        Mode::Import => "*.yaml *.yml *.json",
        Mode::Export(_) => "*.yaml *.yml",
    };
    let mut start = initial.as_os_str().to_owned();
    start.push("/");
    if let Mode::Export(filename) = mode {
        start.push(filename);
    }
    if backend == "kdialog" {
        command
            .arg("--title")
            .arg(mode.title())
            .arg(match mode {
                Mode::Import => "--getopenfilename",
                Mode::Export(_) => "--getsavefilename",
            })
            .arg(start)
            .arg(format!("{filter}|Temas"));
    } else {
        command
            .arg("--file-selection")
            .arg("--title")
            .arg(mode.title())
            .arg("--filename")
            .arg(start)
            .arg(format!("--file-filter=Temas | {filter}"));
        if matches!(mode, Mode::Export(_)) {
            command.args(["--save", "--confirm-overwrite"]);
        }
    }
    command
}

#[cfg(target_os = "linux")]
fn choose_command(
    backend: &str,
    initial: &Path,
    mode: Mode<'_>,
) -> Result<Option<PathBuf>, String> {
    use std::os::unix::ffi::OsStringExt;
    let output = command(backend, initial, mode)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("No se pudo abrir {backend}: {error}"))?;
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err(format!(
            "{backend}: {}",
            String::from_utf8_lossy(&output.stderr)
                .trim()
                .chars()
                .take(512)
                .collect::<String>()
        ));
    }
    let mut bytes = output.stdout;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    Ok((!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsString::from_vec(bytes))))
}

#[cfg(windows)]
fn choose_native(initial: &Path, mode: Mode<'_>) -> Result<Option<PathBuf>, String> {
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName System.Windows.Forms
$save = $env:OPENADE_THEME_DIALOG_MODE -eq 'save'
if ($save) {
    $dialog = New-Object System.Windows.Forms.SaveFileDialog
    $dialog.Filter = 'Temas YAML (*.yaml;*.yml)|*.yaml;*.yml'
    $dialog.DefaultExt = 'yaml'
    $dialog.AddExtension = $true
    $dialog.OverwritePrompt = $true
    $dialog.FileName = $env:OPENADE_THEME_DIALOG_NAME
} else {
    $dialog = New-Object System.Windows.Forms.OpenFileDialog
    $dialog.Filter = 'Temas (*.yaml;*.yml;*.json)|*.yaml;*.yml;*.json'
    $dialog.Multiselect = $false
    $dialog.CheckFileExists = $true
}
$dialog.InitialDirectory = $env:OPENADE_THEME_DIALOG_DIRECTORY
$dialog.Title = $env:OPENADE_THEME_DIALOG_TITLE
try {
    if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
        [Console]::Out.Write($dialog.FileName)
    }
} finally { $dialog.Dispose() }
"#;
    let (action, filename) = match mode {
        Mode::Import => ("open", ""),
        Mode::Export(name) => ("save", name),
    };
    let output = crate::child_process::command_no_window("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Sta",
            "-WindowStyle",
            "Hidden",
            "-Command",
            SCRIPT,
        ])
        .env("OPENADE_THEME_DIALOG_MODE", action)
        .env("OPENADE_THEME_DIALOG_NAME", filename)
        .env("OPENADE_THEME_DIALOG_DIRECTORY", initial)
        .env("OPENADE_THEME_DIALOG_TITLE", mode.title())
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("No se pudo abrir el selector: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "No se pudo abrir el selector: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value = String::from_utf8(output.stdout)
        .map_err(|_| "El selector devolvió una ruta con codificación inválida")?;
    Ok((!value.is_empty()).then(|| PathBuf::from(value)))
}

#[cfg(target_os = "macos")]
fn choose_native(initial: &Path, mode: Mode<'_>) -> Result<Option<PathBuf>, String> {
    const SCRIPT: &str = r#"
on run argv
    try
        set initialFolder to POSIX file (item 1 of argv) as alias
        if item 2 of argv is "save" then
            set selected to choose file name with prompt "Exportar tema YAML" default location initialFolder default name (item 3 of argv)
        else
            set selected to choose file with prompt "Importar tema" of type {"yaml", "yml", "json"} default location initialFolder
        end if
        return POSIX path of selected
    on error number -128
        return ""
    end try
end run
"#;
    let (action, filename) = match mode {
        Mode::Import => ("open", ""),
        Mode::Export(name) => ("save", name),
    };
    let output = crate::child_process::command_no_window("osascript")
        .args(["-e", SCRIPT, "--"])
        .arg(initial)
        .args([action, filename])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("No se pudo abrir el selector: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "No se pudo abrir el selector: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut value = String::from_utf8(output.stdout)
        .map_err(|_| "El selector devolvió una ruta con codificación inválida")?;
    if value.ends_with('\n') {
        value.pop();
    }
    Ok((!value.is_empty()).then(|| PathBuf::from(value)))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn choose_native(_initial: &Path, _mode: Mode<'_>) -> Result<Option<PathBuf>, String> {
    Err("El selector de temas del sistema no está disponible aquí".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_accepts_only_existing_imports_and_explicit_yaml_export_paths() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["tema.yaml", "Tema.YML", "import.json"] {
            let path = directory.path().join(name);
            std::fs::write(&path, "{}").unwrap();
            assert_eq!(
                validate_selection(Some(path.clone()), Mode::Import).unwrap(),
                Some(path)
            );
        }
        for path in [
            directory.path().join("missing.yaml"),
            directory.path().to_path_buf(),
            PathBuf::from("relative.yaml"),
        ] {
            assert!(validate_selection(Some(path), Mode::Import).is_err());
        }
        let path = directory.path().join("new.yaml");
        assert_eq!(
            validate_selection(Some(path.clone()), Mode::Export("theme.yaml")).unwrap(),
            Some(path)
        );
        for name in ["without-extension", "output.json"] {
            assert!(validate_selection(
                Some(directory.path().join(name)),
                Mode::Export("theme.yaml")
            )
            .is_err());
        }
        assert!(validate_selection(None, Mode::Import).unwrap().is_none());
        assert!(choose_export(directory.path(), "../outside.yaml").is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn portal_uris_are_local_paths_not_remote_urls_or_extra_parameters() {
        assert_eq!(
            local_uri("file:///tmp/Mi%20tema.yaml").unwrap(),
            Path::new("/tmp/Mi tema.yaml")
        );
        assert_eq!(
            local_uri("file://localhost/tmp/tema.yaml").unwrap(),
            Path::new("/tmp/tema.yaml")
        );
        for value in [
            "https://example.com/theme.yaml",
            "file://remote/tmp/theme.yaml",
            "file:///tmp/theme.yaml?q=1",
            "file:///tmp/theme.yaml#fragment",
            "file:///tmp/a%00.yaml",
        ] {
            assert!(local_uri(value).is_err(), "refuse {value}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fallback_commands_use_arguments_and_native_save_confirmation() {
        let zenity = command(
            "zenity",
            Path::new("/tmp/Un lugar"),
            Mode::Export("mi tema.yaml"),
        );
        let args = zenity
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.iter().any(|arg| arg == "--confirm-overwrite"));
        assert!(args.iter().any(|arg| arg == "/tmp/Un lugar/mi tema.yaml"));
        let kde = command("kdialog", Path::new("/tmp"), Mode::Import);
        let args = kde
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.iter().any(|arg| arg == "--getopenfilename"));
        assert!(args.iter().any(|arg| arg == "*.yaml *.yml *.json|Temas"));
    }
}
