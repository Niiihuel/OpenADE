//! Chromium embedded through CEF for the project browser: pages without a
//! window, painted by Chromium's software compositor and driven through the
//! DevTools protocol inside this process.
//!
//! CEF starts once per process, the first time a page is opened, on a thread
//! of its own that also shuts it down when the process exits. Its message
//! loop runs on CEF's own UI thread, never on the application's. Chromium
//! never touches the GPU here: no hardware acceleration, no software GL in
//! its place, and compositing on the CPU.

mod page;

pub use page::{Page, Paint};

use cef::*;
use openide_browser::PrivateDirectory;
use std::ffi::{c_char, CStr, CString, OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// What the browser process runs with; Chromium hands its helpers what they
/// need of it. The first three keep it off the GPU: no hardware acceleration,
/// no compositing on it and no software GL instead. The rest make it the one
/// quiet page of a panel, as the switches of the launched Chrome do.
const SWITCHES: &[&str] = &[
    "disable-gpu",
    "disable-gpu-compositing",
    "disable-software-rasterizer",
    "no-startup-window",
    "noerrdialogs",
    "no-first-run",
    "no-default-browser-check",
    "disable-background-networking",
    "disable-component-update",
    "disable-sync",
    "disable-extensions",
    "disable-component-extensions-with-background-pages",
    "disable-hang-monitor",
    "disable-prompt-on-repost",
    "mute-audio",
    "hide-scrollbars",
    // No keyring prompt on the user's desktop.
    "use-mock-keychain",
    // A page nobody is looking at still has to run for the agent.
    "disable-background-timer-throttling",
    "disable-backgrounding-occluded-windows",
    "disable-renderer-backgrounding",
    // History navigation always loads a document, so its events are uniform.
    "disable-back-forward-cache",
];
const VALUED_SWITCHES: &[(&str, &str)] = &[
    // No connection to the display server and no DRM device opened: nothing
    // of Chromium's is ever on screen, and software frames need neither.
    // Frames shared through the GPU will need the session's own platform.
    ("ozone-platform", "headless"),
    ("password-store", "basic"),
    ("force-color-profile", "srgb"),
    (
        "blink-settings",
        "primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4",
    ),
    (
        "disable-features",
        "Translate,MediaRouter,OptimizationHints,AcceptCHFrame,DialMediaRouteProvider",
    ),
    // Without it a viewport capture waits three frames instead of one.
    ("enable-features", "CDPScreenshotNewSurface"),
];
const OPAQUE_WHITE: u32 = 0xFFFF_FFFF;
/// A CEF that hangs while shutting down must not keep the process from exiting.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// How CEF starts in this process. Only the options of the first page count.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The executable CEF starts its helper processes as: one whose `main`
    /// calls `run_child_process` first. `None` is this executable.
    pub subprocess: Option<PathBuf>,
    /// Where CEF's private directory is created; see `PrivateDirectory::create`.
    pub directory_root: Option<PathBuf>,
    /// The languages pages are told the user reads, most wanted first, as in
    /// `es-AR,es,en`. Empty leaves CEF's own, which is English.
    pub accept_languages: String,
}

/// Runs this process as one of CEF's helpers when it was started as one, and
/// returns its exit code. Any other start gets `None` without CEF being
/// called at all. First thing in `main`.
pub fn run_child_process() -> Option<i32> {
    let helper = std::env::args_os()
        .skip(1)
        .any(|argument| argument.as_encoded_bytes().starts_with(b"--type="));
    if !helper {
        return None;
    }
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let arguments = Arguments::new(std::env::args_os());
    Some(execute_process(Some(&arguments.main()), None, std::ptr::null_mut()))
}

/// An `argv` that outlives the call it is passed to.
struct Arguments {
    _strings: Vec<CString>,
    pointers: Vec<*mut c_char>,
}

impl Arguments {
    fn new(arguments: impl Iterator<Item = OsString>) -> Self {
        let strings: Vec<CString> = arguments
            .filter_map(|argument| CString::new(argument.into_vec()).ok())
            .collect();
        let pointers = strings.iter().map(|string| string.as_ptr().cast_mut()).collect();
        Self {
            _strings: strings,
            pointers,
        }
    }

    fn main(&self) -> MainArgs {
        MainArgs {
            argc: self.pointers.len() as i32,
            argv: self.pointers.as_ptr().cast_mut(),
        }
    }
}

wrap_app! {
    struct Application;
    impl App {
        fn on_before_command_line_processing(
            &self,
            process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else { return };
            // Only the browser process: its type is empty.
            if process_type.is_some_and(|kind| !kind.to_string().is_empty()) {
                return;
            }
            for switch in SWITCHES {
                command_line.append_switch(Some(&(*switch).into()));
            }
            for (name, value) in VALUED_SWITCHES {
                command_line.append_switch_with_value(Some(&(*name).into()), Some(&(*value).into()));
            }
        }
    }
}

struct Host {
    /// Asks the CEF thread to shut down; it answers when it has. Gone once asked.
    stop: Mutex<Option<Sender<Sender<()>>>>,
    directory: PathBuf,
}

static HOST: OnceLock<Result<Host, String>> = OnceLock::new();

/// Starts CEF if this is the first page of the process. A start that failed
/// is not tried again: CEF initialises once per process.
pub(crate) fn ensure_started(options: &Options) -> Result<(), String> {
    let host = HOST.get_or_init(|| start(options.clone())).as_ref().map_err(Clone::clone)?;
    if host.stop.lock().unwrap_or_else(|poison| poison.into_inner()).is_none() {
        return Err("CEF ya se cerró en este proceso.".into());
    }
    Ok(())
}

fn start(options: Options) -> Result<Host, String> {
    let (ready, started) = mpsc::channel();
    let (stop, stopping) = mpsc::channel::<Sender<()>>();
    std::thread::Builder::new()
        .name("openide-cef".into())
        .spawn(move || {
            let prepared = PrivateDirectory::create(options.directory_root.as_deref())
                .map_err(|_| "No se pudo crear el directorio temporal de CEF.".to_owned())
                .and_then(|directory| initialise(&options, directory.path()).map(|()| directory));
            let directory = match prepared {
                Ok(directory) => directory,
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
            let _ = ready.send(Ok(directory.path().to_owned()));
            // CEF is shut down by the thread that initialised it.
            let done = stopping.recv();
            page::close_all();
            cef::shutdown();
            drop(directory);
            if let Ok(done) = done {
                let _ = done.send(());
            }
        })
        .map_err(|error| format!("Hilo de CEF: {error}"))?;
    let directory = started
        .recv()
        .map_err(|_| "CEF terminó mientras iniciaba.".to_owned())??;
    // Every normal exit of the process, a test harness's included.
    unsafe {
        libc::atexit(at_exit);
    }
    Ok(Host {
        stop: Mutex::new(Some(stop)),
        directory,
    })
}

/// CEF's private directory in this process, once it has started: where its
/// helpers' command lines point, and what a clean exit removes.
pub fn directory() -> Option<PathBuf> {
    HOST.get()?.as_ref().ok().map(|host| host.directory.clone())
}

extern "C" fn at_exit() {
    shutdown();
}

/// Closes every page, shuts CEF down and removes its directory. Runs by
/// itself when the process exits normally. CEF cannot start again afterwards.
pub fn shutdown() {
    let Some(Ok(host)) = HOST.get() else { return };
    let stop = host.stop.lock().unwrap_or_else(|poison| poison.into_inner()).take();
    let Some(stop) = stop else { return };
    let (done, finished) = mpsc::channel();
    if stop.send(done).is_ok() {
        let _ = finished.recv_timeout(SHUTDOWN_TIMEOUT);
    }
}

/// CEF reads its data files from the directory `libcef.so` was loaded from,
/// and takes the whole process down when one is missing. Looked for first, a
/// broken installation is an error the caller can fall back from.
fn resources() -> Result<(), String> {
    let mut found: libc::Dl_info = unsafe { std::mem::zeroed() };
    let located = unsafe { libc::dladdr(sys::cef_initialize as *const () as *const libc::c_void, &mut found) };
    if located == 0 || found.dli_fname.is_null() {
        return Ok(());
    }
    let library = Path::new(OsStr::from_bytes(unsafe { CStr::from_ptr(found.dli_fname) }.to_bytes()));
    // A relative name is relative to wherever the loader was: CEF decides.
    let Some(directory) = library.parent().filter(|_| library.is_absolute()) else {
        return Ok(());
    };
    for name in ["icudtl.dat", "v8_context_snapshot.bin", "resources.pak", "locales/en-US.pak"] {
        if !directory.join(name).is_file() {
            return Err(format!("Falta {name} junto a {}.", library.display()));
        }
    }
    Ok(())
}

fn initialise(options: &Options, directory: &Path) -> Result<(), String> {
    resources()?;
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let text = |path: &Path| CefString::from(path.to_string_lossy().as_ref());
    let log = directory.join("cef.log");
    let mut settings = Settings {
        multi_threaded_message_loop: 1,
        windowless_rendering_enabled: 1,
        // The application's own arguments mean nothing to Chromium.
        command_line_args_disabled: 1,
        // A profile nobody else uses, removed with the directory. Pages do
        // not use it either: each has storage of its own, in memory.
        root_cache_path: text(&directory.join("profile")),
        log_file: text(&log),
        log_severity: LogSeverity::WARNING,
        background_color: OPAQUE_WHITE,
        accept_language_list: options.accept_languages.as_str().into(),
        // Leaves the application's handlers of crashes as they are.
        disable_signal_handlers: 1,
        ..Default::default()
    };
    if let Some(subprocess) = &options.subprocess {
        settings.browser_subprocess_path = text(subprocess);
    }
    // Chromium reads the program's name and nothing else of the command line.
    let arguments = Arguments::new(std::env::args_os().take(1));
    let mut application = Application::new();
    // Chromium takes these three for a browser's own orderly exit, whatever
    // the setting above says: a signal would then end the application with
    // status 0. What they did before is put back.
    let signals = [libc::SIGHUP, libc::SIGINT, libc::SIGTERM].map(|signal| {
        let mut before: libc::sigaction = unsafe { std::mem::zeroed() };
        unsafe { libc::sigaction(signal, std::ptr::null(), &mut before) };
        (signal, before)
    });
    let started = initialize(Some(&arguments.main()), Some(&settings), Some(&mut application), std::ptr::null_mut());
    for (signal, before) in &signals {
        unsafe { libc::sigaction(*signal, before, std::ptr::null_mut()) };
    }
    if started != 1 {
        // Only what CEF wrote while starting: no page has run yet. Its first
        // error is the cause; the ones after it are what followed.
        let written = std::fs::read_to_string(&log).unwrap_or_default();
        let reason = written
            .lines()
            .find_map(|line| line.split_once(":ERROR:").or(line.split_once(":FATAL:")))
            .map_or("", |(_, said)| said.split_once("] ").map_or(said, |(_, text)| text));
        let reason: String = reason.chars().take(300).collect();
        return Err(format!("CEF no pudo iniciarse (código {}). {reason}", get_exit_code()));
    }
    Ok(())
}
