//! System folder dialogs, called from the app's background picker threads.
//! The desktop owns the chooser's UI; cancelling never tries another backend.

use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
pub(crate) fn choose_folder(initial: &Path, title: &str) -> Result<Option<PathBuf>, String> {
    let initial = initial
        .ancestors()
        .find(|path| path.is_dir())
        .unwrap_or(Path::new("/"));
    let portal_error = match choose_portal_folder(initial, title) {
        Ok(PortalOutcome::Selected(selection)) => return Ok(selection),
        Ok(PortalOutcome::Unavailable(error)) => error,
        Err(error) => return Err(error),
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
        match choose_command_folder(backend, initial, title) {
            Ok(selection) => return validate_selection(selection),
            Err(error) => errors.push(error),
        }
    }
    Err(format!(
        "No se pudo abrir un selector de carpetas del sistema. Configura el portal de archivos de tu escritorio o instala KDialog/Zenity. {}",
        errors.join(" · ")
    ))
}

#[cfg(target_os = "linux")]
enum PortalOutcome {
    Selected(Option<PathBuf>),
    Unavailable(String),
}

#[cfg(target_os = "linux")]
fn choose_portal_folder(initial: &Path, title: &str) -> Result<PortalOutcome, String> {
    use ashpd::desktop::{
        file_chooser::{FileChooserProxy, SelectedFiles},
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
        // ashpd caches its default connection globally. Use a fresh connection
        // so it never retains IO tasks belonging to a previous picker runtime.
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
        let proxy = match tokio::time::timeout(
            Duration::from_secs(5),
            FileChooserProxy::with_connection(connection.clone()),
        )
        .await
        {
            Ok(Ok(proxy)) => proxy,
            Ok(Err(error)) => return Ok(PortalOutcome::Unavailable(error.to_string())),
            Err(_) => {
                return Ok(PortalOutcome::Unavailable(
                    "El portal de archivos no respondió".into(),
                ))
            }
        };
        if proxy.version() < 3 {
            return Ok(PortalOutcome::Unavailable(
                "Este portal no permite seleccionar carpetas".into(),
            ));
        }
        let request = SelectedFiles::open_file()
            .connection(Some(connection))
            .title(title)
            .accept_label("Seleccionar carpeta")
            .modal(true)
            .multiple(false)
            .directory(true)
            .current_folder(initial)
            .map_err(|error| format!("No se pudo preparar la carpeta inicial: {error}"))?;
        let response = match request.send().await.and_then(|request| request.response()) {
            Ok(response) => response,
            Err(ashpd::Error::Response(ResponseError::Cancelled)) => {
                return Ok(PortalOutcome::Selected(None));
            }
            Err(error) => return Ok(PortalOutcome::Unavailable(error.to_string())),
        };
        let selection = response
            .uris()
            .first()
            .map(|uri| {
                reqwest::Url::parse(uri.as_str())
                    .ok()
                    .and_then(|uri| uri.to_file_path().ok())
                    .ok_or_else(|| "Selecciona una carpeta local de este equipo".to_owned())
            })
            .transpose()?;
        validate_selection(selection).map(PortalOutcome::Selected)
    })
}

#[cfg(target_os = "linux")]
fn choose_command_folder(
    backend: &str,
    initial: &Path,
    title: &str,
) -> Result<Option<PathBuf>, String> {
    use std::os::unix::ffi::OsStringExt;
    use std::process::Stdio;

    let mut command = crate::child_process::command_no_window(backend);
    if backend == "kdialog" {
        command
            .arg("--title")
            .arg(title)
            .arg("--getexistingdirectory")
            .arg(initial);
    } else {
        // GTK treats a trailing slash as an initial directory, not a filename.
        let mut folder = initial.as_os_str().to_owned();
        folder.push("/");
        command
            .args(["--file-selection", "--directory", "--title"])
            .arg(title)
            .arg("--filename")
            .arg(folder);
    }
    let output = command.stdin(Stdio::null()).output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("{backend} no está instalado")
        } else {
            format!("No se pudo abrir {backend}: {error}")
        }
    })?;
    if output.status.code() == Some(1) {
        return Ok(None); // User cancellation; do not open a second dialog.
    }
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{backend}: {}",
            detail.trim().chars().take(512).collect::<String>()
        ));
    }
    let mut bytes = output.stdout;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.is_empty() {
        Ok(None)
    } else {
        // Preserve spaces, Unicode and arbitrary filesystem bytes. The app
        // reports an unrepresentable workspace path at its existing boundary.
        Ok(Some(PathBuf::from(std::ffi::OsString::from_vec(bytes))))
    }
}

#[cfg(target_os = "linux")]
fn validate_selection(selection: Option<PathBuf>) -> Result<Option<PathBuf>, String> {
    if selection
        .as_ref()
        .is_some_and(|path| !path.is_absolute() || !path.is_dir())
    {
        return Err("Selecciona una carpeta local existente".to_owned());
    }
    Ok(selection)
}

#[cfg(windows)]
pub(crate) fn choose_folder(initial: &Path, title: &str) -> Result<Option<PathBuf>, String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName System.Windows.Forms
$dialog = New-Object System.Windows.Forms.FolderBrowserDialog
$dialog.Description = $env:OPENIDE_PICKER_DESCRIPTION
$dialog.ShowNewFolderButton = $true
if ([System.IO.Directory]::Exists($env:OPENIDE_INITIAL_FOLDER)) {
    $dialog.SelectedPath = $env:OPENIDE_INITIAL_FOLDER
}
try {
    if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
        [Console]::Out.Write($dialog.SelectedPath)
    }
} finally {
    $dialog.Dispose()
}
"#;

    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Sta",
            "-WindowStyle",
            "Hidden",
            "-Command",
            SCRIPT,
        ])
        .env("OPENIDE_INITIAL_FOLDER", initial)
        .env("OPENIDE_PICKER_DESCRIPTION", title)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("No se pudo abrir el selector de carpetas: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "No se pudo abrir el selector de carpetas: {}",
            detail.trim()
        ));
    }
    let selected = String::from_utf8(output.stdout)
        .map_err(|_| "El selector devolvió una ruta con codificación inválida".to_owned())?;
    if selected.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(selected)))
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) fn choose_folder(_initial: &Path, _title: &str) -> Result<Option<PathBuf>, String> {
    Err("El selector de carpetas del sistema no está disponible aquí".to_owned())
}
