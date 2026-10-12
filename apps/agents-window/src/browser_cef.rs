//! A browser context on the embedded Chromium (`--features cef`): a CEF page
//! Chromium composites on the CPU, driven by the same `Driver` through the
//! DevTools protocol inside this process.
//!
//! Chromium hands over each frame uncompressed. The paint callback, on CEF's
//! UI thread, copies it into the one canvas below, and the runtime's frame
//! thread turns that canvas into the frame the view takes. Nothing queues: a
//! paint the view has not taken is painted over by the next.

use super::*;
use std::hash::Hasher;

/// How often Chromium composites a page that is on screen, or that a command
/// is waiting on: a click and a capture each take a frame or two.
const FRAME_RATE: u32 = 60;
/// And one that is neither. Its paints go nowhere, but it stays a visible
/// page: one CEF hides stops animating and timing for the agent's next step.
const IDLE_FRAME_RATE: u32 = 10;

#[derive(Default)]
struct Canvas {
    /// The page as Chromium last painted it: rows of BGRA.
    bgra: Vec<u8>,
    size: [usize; 2],
    /// Device pixels per CSS pixel of those rows.
    scale: f32,
    /// Painted since the view was last given a frame.
    dirty: bool,
    /// The surface is on screen. A paint of a hidden one is not taken.
    visible: bool,
    /// The worker is running a command on the page.
    busy: bool,
    /// The frame rate the page was last given.
    rate: u32,
    /// What the canvas held, all of it shown, when an agent capture began.
    before_capture: Option<u64>,
}

/// The embedded page of one context, when it runs on one.
#[derive(Default)]
pub(super) struct Embedded {
    page: Mutex<Option<openide_cef::Page>>,
    canvas: Mutex<Canvas>,
    /// The frame thread's own: the canvas in the channel order a frame is
    /// built from, kept between frames.
    rgb: Mutex<Vec<u8>>,
}

/// How CEF starts in this process. Its helper processes are this executable,
/// whose `main` runs them, and its directory goes under the data root.
fn host_options(data: &Path) -> openide_cef::Options {
    let options = openide_cef::Options {
        subprocess: None,
        directory_root: Some(data.to_owned()),
        accept_languages: accept_languages(&system_locale()),
    };
    // A test harness has no `main` to run CEF's helpers; the application
    // built beside it does. Nor does a test's data root outlive the test.
    #[cfg(test)]
    let options = openide_cef::Options {
        subprocess: Some(std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("agents-window")),
        directory_root: None,
        ..options
    };
    options
}

pub(super) fn system_locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_default()
}

/// The languages pages are told the user reads, from the system's locale:
/// `es_AR.UTF-8` is `es-AR,es,en`. A launched Chrome reads the same variables.
pub(super) fn accept_languages(locale: &str) -> String {
    let tag = locale.split(['.', '@']).next().unwrap_or("").replace('_', "-");
    let language = tag.split('-').next().unwrap_or("");
    let named = language.len() >= 2
        && language.bytes().all(|byte| byte.is_ascii_lowercase())
        && tag.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if !named {
        return "en-US,en".into();
    }
    let mut languages = vec![tag.as_str(), language, "en"];
    languages.dedup();
    languages.join(",")
}

/// Chromium's pixels as the view's frames are built from: the page has an
/// opaque background, so its alpha carries nothing.
pub(super) fn bgra_to_rgb(bgra: &[u8], rgb: &mut [u8]) {
    for (rgb, bgra) in rgb.chunks_exact_mut(3).zip(bgra.chunks_exact(4)) {
        rgb[0] = bgra[2];
        rgb[1] = bgra[1];
        rgb[2] = bgra[0];
    }
}

fn fingerprint(pixels: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(pixels);
    hasher.finish()
}

impl Embedded {
    fn page(&self) -> MutexGuard<'_, Option<openide_cef::Page>> {
        self.page.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn canvas(&self) -> MutexGuard<'_, Canvas> {
        self.canvas.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// Opens this context's page in the embedded Chromium, starting CEF if it
    /// is the first of the process, and connects a driver to it. The error
    /// says why the context cannot run there.
    pub(super) fn start(
        view: &Arc<LiveView>,
        data: &Path,
        viewport: (u32, u32),
        scale: f32,
    ) -> Result<Driver, String> {
        // shortcut: the scale is capped once, for the largest viewport, so a
        // frame always fits the pixel budget. A page's scale is fixed when
        // CEF creates it; a display denser than 2x that must keep its own on
        // a small panel needs the page created again on resize.
        let scale = match frame_bounds((1600, 1200), scale) {
            (fitted, Some(_)) => (fitted * 100.0).floor() / 100.0,
            _ => scale,
        };
        let painted = Arc::downgrade(view);
        let page = openide_cef::Page::open(
            &host_options(data),
            viewport,
            scale,
            IDLE_FRAME_RATE,
            move |paint| {
                if let Some(view) = painted.upgrade() {
                    view.embedded.paint(&view, paint);
                }
            },
        )?;
        let transport = page
            .transport()
            .ok_or("La página embebida no tiene conexión DevTools.")?;
        let driver = Driver::connect_to_page(transport, viewport.0, viewport.1, f64::from(scale))
            .map_err(|error| error.message)?;
        // Not on screen until the worker says so.
        *view.embedded.canvas() = Canvas {
            scale,
            rate: IDLE_FRAME_RATE,
            ..Canvas::default()
        };
        *view.embedded.page() = Some(page);
        Ok(driver)
    }

    pub(super) fn is_open(&self) -> bool {
        self.page().is_some()
    }

    /// One paint of the page, on CEF's UI thread, which also carries the
    /// driver's requests: one copy, and a signal.
    fn paint(&self, view: &LiveView, paint: &openide_cef::Paint) {
        let mut canvas = self.canvas();
        if !canvas.visible {
            return;
        }
        canvas.size = [paint.width as usize, paint.height as usize];
        canvas.bgra.clear();
        canvas.bgra.extend_from_slice(paint.bgra);
        canvas.dirty = true;
        drop(canvas);
        view.feed().woken = true;
        view.arrived.notify_one();
    }

    /// The page as last painted, once per paint: the frame thread asks, and
    /// hands what it gets to the view. An agent capture makes Chromium draw
    /// the page another way for a moment, as with the stream; the paints of
    /// that moment are held back, and what is left after it is news only if
    /// it differs from what the view already shows.
    pub(super) fn frame(&self, view: &LiveView) -> Option<limn_egui::SurfaceFrame> {
        let mut canvas = self.canvas();
        let feed = view.feed();
        let redrawing = feed.rerendering
            || feed
                .settling_until
                .is_some_and(|until| Instant::now() < until);
        drop(feed);
        if redrawing {
            return None;
        }
        if !canvas.dirty || !canvas.visible {
            // The capture came and went without a paint: a later one is
            // news whatever it shows.
            canvas.before_capture = None;
            return None;
        }
        canvas.dirty = false;
        if canvas
            .before_capture
            .take()
            .is_some_and(|before| before == fingerprint(&canvas.bgra))
        {
            return None;
        }
        let mut rgb = self.rgb.lock().unwrap_or_else(|poison| poison.into_inner());
        rgb.resize(canvas.bgra.len() / 4 * 3, 0);
        bgra_to_rgb(&canvas.bgra, &mut rgb);
        let (size, scale) = (canvas.size, canvas.scale);
        // CEF may paint again while the frame is built.
        drop(canvas);
        limn_egui::SurfaceFrame::from_rgb(size, &rgb, scale)
    }

    /// An agent capture is about to redraw the page, and the view already
    /// holds its paints back: remembers what the page looks like now.
    pub(super) fn before_capture(&self) {
        let mut canvas = self.canvas();
        // Kept from a capture just before this one, whose paints are still
        // held back. A paint the view was never given is owed to it.
        if canvas.before_capture.is_none() && !canvas.dirty && canvas.visible {
            canvas.before_capture = Some(fingerprint(&canvas.bgra));
        }
    }

    /// Lets the page's paints through to the view, or stops them. Shown, the
    /// page is painted whole once more: the view is owed a frame even of a
    /// page that holds still.
    pub(super) fn show(&self, visible: bool) {
        let mut canvas = self.canvas();
        canvas.visible = visible;
        canvas.before_capture = None;
        drop(canvas);
        self.pace();
        if let (true, Some(page)) = (visible, &*self.page()) {
            page.invalidate();
        }
    }

    /// A command starts or ends on the page.
    pub(super) fn busy(&self, busy: bool) {
        self.canvas().busy = busy;
        self.pace();
    }

    fn pace(&self) {
        let mut canvas = self.canvas();
        let rate = if canvas.visible || canvas.busy { FRAME_RATE } else { IDLE_FRAME_RATE };
        // Only a change: setting the rate it has restarts Chromium's frame timer.
        let changed = std::mem::replace(&mut canvas.rate, rate) != rate;
        drop(canvas);
        if let (true, Some(page)) = (changed, &*self.page()) {
            page.set_frame_rate(rate);
        }
    }

    /// The view takes the size of the new viewport; the driver gives it to
    /// the page next.
    pub(super) fn resize(&self, viewport: (u32, u32)) {
        if let Some(page) = &*self.page() {
            page.resize(viewport.0, viewport.1);
        }
    }

    /// Starts closing the page without waiting, so a request in flight ends.
    pub(super) fn interrupt(&self) {
        if let Some(page) = &*self.page() {
            page.close();
        }
    }

    /// Closes the page; its cookies, cache and storage go with it.
    pub(super) fn close(&self) {
        let page = self.page().take();
        drop(page);
        *self.canvas() = Canvas::default();
        *self.rgb.lock().unwrap_or_else(|poison| poison.into_inner()) = Vec::new();
    }

    /// CEF's own wheel event, in CSS pixels of the view, to measure it
    /// against the driver's.
    #[cfg(test)]
    pub(super) fn native_wheel(&self, x: i32, y: i32, dy: i32) {
        if let Some(page) = &*self.page() {
            page.wheel(x, y, 0, dy);
        }
    }
}

/// Said once: CEF starts once per process, so a start that failed fails the
/// same way for every later context.
pub(super) fn report_fallback(reason: &str) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!("OpenIDE: el navegador embebido (CEF) no pudo iniciarse y se usa el navegador del sistema. {reason}");
    });
}

/// `agents-window --cef-probe`: starts a browser context the way the window
/// does, without a window, waits for its first frame and says what it got.
/// For checking that an installation can run the embedded Chromium.
pub(crate) fn probe() -> i32 {
    let root = std::env::temp_dir().join(format!("openide-cef-probe-{}", std::process::id()));
    if let Err(error) = private_directory(&root) {
        eprintln!("No se pudo crear {}: {error}", root.display());
        return 1;
    }
    let started = Instant::now();
    let outcome = BrowserRuntime::start(
        BrowserScope {
            workspace: root.clone(),
            session_id: "cef-probe".into(),
        },
        &root,
        BrowserLaunchOptions {
            viewport_width: 1280,
            viewport_height: 800,
            device_scale: 2.0,
            ..Default::default()
        },
    )
    .and_then(|runtime| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let state = runtime.snapshot();
            if let Some(error) = state.error.filter(|_| !state.loading) {
                return Err(error);
            }
            if state.started && !runtime.handle.view.embedded.is_open() {
                return Err("CEF no se inició; la aplicación usa el navegador del sistema.".into());
            }
            if let Some(frame) = runtime.take_frame() {
                return Ok((frame.size(), started.elapsed()));
            }
            if Instant::now() >= deadline {
                return Err("CEF no entregó ningún cuadro en 30 s.".into());
            }
            thread::sleep(Duration::from_millis(2));
        }
    });
    let _ = fs::remove_dir_all(&root);
    match outcome {
        Ok((size, elapsed)) => {
            println!(
                "CEF funciona: primer cuadro de {} × {} a los {} ms, sin ventana y sin GPU.",
                size[0],
                size[1],
                elapsed.as_millis()
            );
            if let Some(directory) = openide_cef::directory() {
                println!("Directorio temporal de CEF: {}", directory.display());
            }
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::accept_languages;

    #[test]
    fn languages_follow_the_locale_and_fall_back_to_english() {
        assert_eq!(accept_languages("es_AR.UTF-8"), "es-AR,es,en");
        assert_eq!(accept_languages("sr_RS@latin"), "sr-RS,sr,en");
        assert_eq!(accept_languages("de"), "de,en");
        assert_eq!(accept_languages("en_US.UTF-8"), "en-US,en");
        for unnamed in ["", "C", "C.UTF-8", "POSIX", "es AR", "../x"] {
            assert_eq!(accept_languages(unnamed), "en-US,en", "{unnamed:?}");
        }
    }
}
