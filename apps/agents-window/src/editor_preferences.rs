//! Bounded editor preferences. The window submits snapshots; one worker writes
//! only the latest pending snapshot, without filesystem work on a UI frame.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
};

const MAX_FILE_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct Prefs {
    pub soft_wrap: bool,
    pub minimap: bool,
    pub tab_width: usize,
    pub use_tabs: bool,
    pub auto_pairs: bool,
    /// Exact workspace identity → language name → executable, never shell code.
    /// The language host validates/launches an override only after an explicit
    /// user request; loading preferences does not execute anything.
    pub server_executables: BTreeMap<String, BTreeMap<String, String>>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            soft_wrap: false,
            minimap: true,
            tab_width: 4,
            use_tabs: false,
            auto_pairs: true,
            server_executables: BTreeMap::new(),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct Stored {
    version: u32,
    #[serde(flatten)]
    prefs: Prefs,
}

impl Prefs {
    /// `path` is the complete preferences filename. Loading is intended once
    /// during startup. Missing, future, invalid or oversized files use defaults.
    pub fn load(path: &Path) -> Self {
        let Ok(file) = fs::File::open(path) else {
            return Self::default();
        };
        let mut bytes = Vec::new();
        if file
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > MAX_FILE_BYTES
        {
            return Self::default();
        }
        let Ok(stored) = serde_json::from_slice::<Stored>(&bytes) else {
            return Self::default();
        };
        if stored.version != 1 || stored.prefs.validate().is_err() {
            return Self::default();
        }
        stored.prefs
    }

    fn validate(&self) -> Result<(), String> {
        if !(1..=16).contains(&self.tab_width) {
            return Err("El ancho de tabulación debe estar entre 1 y 16.".into());
        }
        if self.server_executables.len() > 128 {
            return Err("Hay demasiadas configuraciones de servidores de lenguaje.".into());
        }
        let mut total = 128usize;
        for (workspace, languages) in &self.server_executables {
            if workspace.is_empty()
                || workspace.len() > 4096
                || workspace.contains('\0')
                || !Path::new(workspace).is_absolute()
                || languages.len() > 32
            {
                return Err("La configuración de proyecto del editor no es válida.".into());
            }
            total = total.saturating_add(workspace.len());
            for (language, executable) in languages {
                if language.is_empty()
                    || language.len() > 64
                    || !language
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-+".contains(&c))
                    || executable.trim().is_empty()
                    || executable.len() > 4096
                    || executable.chars().any(char::is_control)
                {
                    return Err("La configuración del servidor de lenguaje no es válida.".into());
                }
                total = total.saturating_add(language.len() + executable.len() + 16);
            }
        }
        if total > MAX_FILE_BYTES as usize {
            return Err("Las preferencias del editor superan el tamaño permitido.".into());
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct WriteOutcome {
    pub sequence: u64,
    pub result: Result<(), String>,
}

#[derive(Default)]
struct Pending {
    next_sequence: u64,
    snapshot: Option<(u64, Prefs)>,
    outcome: Option<WriteOutcome>,
    closed: bool,
}

pub(crate) struct PreferencesWriter {
    shared: Arc<(Mutex<Pending>, Condvar)>,
}

impl PreferencesWriter {
    /// Starts a worker; directory creation and serialization happen there.
    pub fn new(path: PathBuf) -> io::Result<Self> {
        let shared = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("editor-preferences".into())
            .spawn(move || {
                loop {
                    let (sequence, prefs) = {
                        let (mutex, ready) = &*worker;
                        let Ok(mut pending) = mutex.lock() else {
                            return;
                        };
                        while pending.snapshot.is_none() && !pending.closed {
                            let Ok(next) = ready.wait(pending) else {
                                return;
                            };
                            pending = next;
                        }
                        match pending.snapshot.take() {
                            Some(snapshot) => snapshot,
                            None => return,
                        }
                    };
                    let result = save(&path, &prefs).map_err(|error| error.to_string());
                    let Ok(mut pending) = worker.0.lock() else {
                        return;
                    };
                    pending.outcome = Some(WriteOutcome { sequence, result });
                }
            })?;
        Ok(Self { shared })
    }

    /// Replaces at most one queued snapshot. No disk access or unbounded queue.
    pub fn submit(&self, prefs: Prefs) -> Result<u64, String> {
        prefs.validate()?;
        let mut pending = self
            .shared
            .0
            .lock()
            .map_err(|_| "No se pudieron guardar las preferencias del editor.".to_owned())?;
        pending.next_sequence = pending
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| "Se agotó la secuencia de preferencias del editor.".to_owned())?;
        let sequence = pending.next_sequence;
        pending.snapshot = Some((sequence, prefs));
        self.shared.1.notify_one();
        Ok(sequence)
    }

    pub fn poll(&self) -> Option<WriteOutcome> {
        self.shared.0.lock().ok()?.outcome.take()
    }
}

impl Drop for PreferencesWriter {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.shared.0.lock() {
            pending.closed = true;
            self.shared.1.notify_one();
        }
    }
}

fn save(path: &Path, prefs: &Prefs) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(&Stored {
        version: 1,
        prefs: prefs.clone(),
    })?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Las preferencias del editor superan el tamaño permitido.",
        ));
    }
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(".editor-preferences-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        replace_atomically(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(windows))]
fn replace_atomically(temporary: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_atomically(temporary: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    let temporary = wide(temporary);
    let destination = wide(destination);
    if unsafe {
        MoveFileExW(
            temporary.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait(writer: &PreferencesWriter, sequence: u64) -> WriteOutcome {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(outcome) = writer.poll() {
                if outcome.sequence == sequence {
                    return outcome;
                }
            }
            assert!(
                Instant::now() < deadline,
                "preferences worker did not finish"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn round_trip_replacement_and_last_submitted_snapshot_win() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state/editor.json");
        let writer = PreferencesWriter::new(path.clone()).unwrap();
        let mut prefs = Prefs::default();
        prefs.soft_wrap = true;
        prefs.minimap = false;
        prefs.server_executables.insert(
            root.path().to_string_lossy().into(),
            BTreeMap::from([("rust".into(), "rust-analyzer".into())]),
        );
        wait(&writer, writer.submit(prefs.clone()).unwrap())
            .result
            .unwrap();
        assert_eq!(Prefs::load(&path), prefs);
        let mut last = 0;
        for index in 0..200 {
            prefs.tab_width = index % 16 + 1;
            prefs.use_tabs = index % 2 == 0;
            last = writer.submit(prefs.clone()).unwrap();
        }
        wait(&writer, last).result.unwrap();
        assert_eq!(Prefs::load(&path), prefs);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn bounded_invalid_and_future_files_use_defaults_and_invalid_submit_keeps_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("editor.json");
        for contents in [
            b"not json".to_vec(),
            br#"{"version":2,"soft_wrap":true}"#.to_vec(),
            br#"{"version":1,"tab_width":0}"#.to_vec(),
            vec![b' '; MAX_FILE_BYTES as usize + 1],
        ] {
            fs::write(&path, contents).unwrap();
            assert_eq!(Prefs::load(&path), Prefs::default());
        }
        save(&path, &Prefs::default()).unwrap();
        let before = fs::read(&path).unwrap();
        let writer = PreferencesWriter::new(path.clone()).unwrap();
        let invalid = Prefs {
            tab_width: 17,
            ..Prefs::default()
        };
        assert!(writer.submit(invalid).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn persistence_failure_is_reported_and_retry_can_succeed() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("blocked");
        fs::write(&parent, "file").unwrap();
        let path = parent.join("editor.json");
        let writer = PreferencesWriter::new(path.clone()).unwrap();
        assert!(
            wait(&writer, writer.submit(Prefs::default()).unwrap())
                .result
                .is_err()
        );
        fs::remove_file(parent).unwrap();
        let prefs = Prefs {
            soft_wrap: true,
            ..Prefs::default()
        };
        wait(&writer, writer.submit(prefs.clone()).unwrap())
            .result
            .unwrap();
        assert_eq!(Prefs::load(&path), prefs);
    }
}
