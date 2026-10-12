//! A browser context on the embedded Chromium (`--features cef`): a CEF page
//! Chromium composites on the CPU, driven by the same `Driver` through the
//! DevTools protocol inside this process.
//!
//! Chromium hands over each frame uncompressed. The paint callback, on CEF's
//! UI thread, copies it into the one canvas below, and the runtime's frame
//! thread turns that canvas into the frame the view takes. Nothing queues: a
//! paint the view has not taken is painted over by the next.

use super::*;
use eframe::egui;
use std::hash::Hasher;
use std::sync::atomic::AtomicU32;

/// How often Chromium composites a page that is on screen, or that a command
/// is waiting on: as often as the window itself is presented, which under
/// vsync is the display's refresh rate. Sixty until the window has said.
static DISPLAY_RATE: AtomicU32 = AtomicU32::new(60);
/// The window's first frame intervals with a browser open.
static WINDOW_FRAMES: Mutex<Vec<f32>> = Mutex::new(Vec::new());
const WINDOW_FRAMES_WANTED: usize = 48;

/// Takes the time between two frames of the window while its rate is being
/// measured, and says whether the next frame is wanted at once.
// shortcut: measured once per run; a window moved to a display with another
// refresh rate keeps the first one until the application restarts.
pub(crate) fn observe_window_frame(interval: f32) -> bool {
    let mut frames = WINDOW_FRAMES.lock().unwrap_or_else(|poison| poison.into_inner());
    if frames.len() >= WINDOW_FRAMES_WANTED {
        return false;
    }
    frames.push(interval);
    if frames.len() < WINDOW_FRAMES_WANTED {
        return true;
    }
    if let Some(rate) = display_rate(&frames) {
        DISPLAY_RATE.store(rate, Ordering::Relaxed);
    }
    false
}

/// The refresh rate a run of frame intervals shows. The first follow a
/// wake-up, and a slow frame says nothing about the display: the quick ones
/// do. A rate close to a usual one is that one.
fn display_rate(intervals: &[f32]) -> Option<u32> {
    let mut steady: Vec<f32> = intervals
        .iter()
        .skip(8)
        .copied()
        .filter(|interval| interval.is_finite() && *interval > 0.0)
        .collect();
    if steady.len() < 16 {
        return None;
    }
    steady.sort_by(f32::total_cmp);
    let measured = 1.0 / steady[steady.len() / 4];
    let off = |rate: &u32| (*rate as f32 - measured).abs();
    let usual = [60, 72, 75, 90, 100, 120, 144, 165, 240]
        .into_iter()
        .min_by(|a, b| off(a).total_cmp(&off(b)))?;
    Some(if off(&usual) <= usual as f32 * 0.06 {
        usual
    } else {
        (measured.round() as u32).clamp(30, 240)
    })
}
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
    /// A widget Chromium paints apart, over the page: the list of a `<select>`.
    popup: Option<Popup>,
    /// Where the window's pointer last was on the page, in CSS pixels.
    pointer: (f64, f64),
}

struct Popup {
    bgra: Vec<u8>,
    size: [usize; 2],
    /// Where its top-left corner goes in the view. All in device pixels.
    at: (i32, i32),
}

/// Draws the rows of `popup` that fall inside a view of `size` over it.
fn overlay(rgb: &mut [u8], size: [usize; 2], popup: &Popup) {
    let (left, top) = (popup.at.0.max(0) as usize, popup.at.1.max(0) as usize);
    // What of the popup lies left of or above the view is not drawn.
    let (skip_x, skip_y) = ((-popup.at.0).max(0) as usize, (-popup.at.1).max(0) as usize);
    let width = popup.size[0].saturating_sub(skip_x).min(size[0].saturating_sub(left));
    for row in skip_y..popup.size[1].min(skip_y + size[1].saturating_sub(top)) {
        let from = (row * popup.size[0] + skip_x) * 4;
        let to = ((top + row - skip_y) * size[0] + left) * 3;
        let pixels = popup.bgra[from..from + width * 4].chunks_exact(4);
        for (rgb, bgra) in rgb[to..to + width * 3].chunks_exact_mut(3).zip(pixels) {
            // Premultiplied: what shows through is the page under it.
            let under = 255 - u16::from(bgra[3]);
            for (channel, colour) in rgb.iter_mut().zip([bgra[2], bgra[1], bgra[0]]) {
                *channel = colour.saturating_add((u16::from(*channel) * under / 255) as u8);
            }
        }
    }
}

/// The embedded page of one context, when it runs on one.
#[derive(Default)]
pub(super) struct Embedded {
    page: Mutex<Option<openide_cef::Page>>,
    canvas: Mutex<Canvas>,
    /// The frame thread's own: the canvas in the channel order a frame is
    /// built from, kept between frames.
    rgb: Mutex<Vec<u8>>,
    /// Why this context is not on the embedded Chromium, when it was meant to be.
    fallback: Mutex<Option<Arc<str>>>,
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
        let size = [paint.width as usize, paint.height as usize];
        if let Some(at) = paint.popup {
            let bgra = paint.bgra.to_vec();
            canvas.popup = (!bgra.is_empty()).then_some(Popup { bgra, size, at });
            // Drawn over a page: one that was never painted has none to show.
            if canvas.bgra.is_empty() {
                return;
            }
        } else {
            canvas.size = size;
            canvas.bgra.clear();
            canvas.bgra.extend_from_slice(paint.bgra);
        }
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
        // The window's rate may have been measured since the page was shown.
        self.pace();
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
        if let Some(popup) = &canvas.popup {
            overlay(&mut rgb, size, popup);
        }
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
        if !visible {
            // A hidden page takes no paint, and keeps none: shown again it
            // is painted whole.
            canvas.bgra = Vec::new();
            canvas.popup = None;
            canvas.dirty = false;
        }
        drop(canvas);
        if !visible {
            *self.rgb.lock().unwrap_or_else(|poison| poison.into_inner()) = Vec::new();
        }
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
        let rate = if canvas.visible || canvas.busy {
            DISPLAY_RATE.load(Ordering::Relaxed)
        } else {
            IDLE_FRAME_RATE
        };
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

    /// The window's pointer on a widget Chromium paints apart: the driver's
    /// events only ever reach the page under it, which closes the widget, so
    /// these go to Chromium itself. True when `command` was one of them.
    pub(super) fn takes_pointer(&self, command: &BrowserCommand) -> bool {
        let mut canvas = self.canvas();
        if let BrowserCommand::PointerMove { x, y, .. }
        | BrowserCommand::PointerDown { x, y, .. }
        | BrowserCommand::PointerUp { x, y, .. } = command
        {
            canvas.pointer = (*x, *y);
        }
        let (x, y) = canvas.pointer;
        let device = |css: f64| (css * f64::from(canvas.scale)) as i32;
        let over = canvas.popup.as_ref().is_some_and(|popup| {
            let inside = |point: i32, from: i32, length: usize| (from..from + length as i32).contains(&point);
            inside(device(x), popup.at.0, popup.size[0]) && inside(device(y), popup.at.1, popup.size[1])
        });
        drop(canvas);
        let page = self.page();
        let (Some(page), true) = (&*page, over) else {
            return false;
        };
        let (x, y) = (x as i32, y as i32);
        match command {
            BrowserCommand::PointerMove { .. } => page.pointer_move(x, y),
            // Only the button that chooses: the others mean nothing there.
            BrowserCommand::PointerDown { button: MouseButton::Left, clicks, .. } => page.pointer_button(x, y, false, *clicks),
            BrowserCommand::PointerUp { button: MouseButton::Left, clicks, .. } => page.pointer_button(x, y, true, *clicks),
            BrowserCommand::PointerDown { .. } | BrowserCommand::PointerUp { .. } => {}
            // CEF counts a turn towards the user as negative.
            BrowserCommand::Scroll { dx, dy, .. } => page.wheel(x, y, -dx.round() as i32, -dy.round() as i32),
            _ => return false,
        }
        true
    }

    /// Where a widget painted apart is over the page and how large, in device pixels.
    #[cfg(test)]
    pub(super) fn popup_box(&self) -> Option<((i32, i32), [usize; 2])> {
        self.canvas().popup.as_ref().map(|popup| (popup.at, popup.size))
    }

    /// The pointer the page asks for under the window's, as the window shows it.
    pub(super) fn cursor(&self) -> Option<egui::CursorIcon> {
        self.page().as_ref().map(|page| cursor_icon(page.cursor()))
    }

    pub(super) fn fallback(&self) -> Option<Arc<str>> {
        self.fallback.lock().unwrap_or_else(|poison| poison.into_inner()).clone()
    }
}

fn cursor_icon(cursor: openide_cef::Cursor) -> egui::CursorIcon {
    use egui::CursorIcon as Icon;
    use openide_cef::Cursor::*;
    match cursor {
        CT_CROSS => Icon::Crosshair,
        CT_HAND => Icon::PointingHand,
        CT_IBEAM => Icon::Text,
        CT_WAIT => Icon::Wait,
        CT_HELP => Icon::Help,
        CT_EASTRESIZE => Icon::ResizeEast,
        CT_NORTHRESIZE => Icon::ResizeNorth,
        CT_NORTHEASTRESIZE => Icon::ResizeNorthEast,
        CT_NORTHWESTRESIZE => Icon::ResizeNorthWest,
        CT_SOUTHRESIZE => Icon::ResizeSouth,
        CT_SOUTHEASTRESIZE => Icon::ResizeSouthEast,
        CT_SOUTHWESTRESIZE => Icon::ResizeSouthWest,
        CT_WESTRESIZE => Icon::ResizeWest,
        CT_NORTHSOUTHRESIZE => Icon::ResizeVertical,
        CT_EASTWESTRESIZE => Icon::ResizeHorizontal,
        CT_NORTHEASTSOUTHWESTRESIZE => Icon::ResizeNeSw,
        CT_NORTHWESTSOUTHEASTRESIZE => Icon::ResizeNwSe,
        CT_COLUMNRESIZE => Icon::ResizeColumn,
        CT_ROWRESIZE => Icon::ResizeRow,
        CT_MIDDLEPANNING | CT_MIDDLE_PANNING_VERTICAL | CT_MIDDLE_PANNING_HORIZONTAL | CT_EASTPANNING
        | CT_NORTHPANNING | CT_NORTHEASTPANNING | CT_NORTHWESTPANNING | CT_SOUTHPANNING
        | CT_SOUTHEASTPANNING | CT_SOUTHWESTPANNING | CT_WESTPANNING => Icon::AllScroll,
        CT_MOVE => Icon::Move,
        CT_VERTICALTEXT => Icon::VerticalText,
        CT_CELL => Icon::Cell,
        CT_CONTEXTMENU => Icon::ContextMenu,
        CT_ALIAS => Icon::Alias,
        CT_PROGRESS => Icon::Progress,
        CT_NODROP => Icon::NoDrop,
        CT_COPY => Icon::Copy,
        CT_NONE => Icon::None,
        CT_NOTALLOWED => Icon::NotAllowed,
        CT_ZOOMIN => Icon::ZoomIn,
        CT_ZOOMOUT => Icon::ZoomOut,
        CT_GRAB => Icon::Grab,
        CT_GRABBING => Icon::Grabbing,
        // The arrow, and a picture of the page's own, which is not shown.
        _ => Icon::Default,
    }
}

/// Said once on the error output: CEF starts once per process, so a start
/// that failed fails the same way for every later context. The context keeps
/// the reason for its panel to say.
pub(super) fn report_fallback(view: &LiveView, reason: &str) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!("OpenIDE: el navegador embebido (CEF) no pudo iniciarse y se usa el navegador del sistema. {reason}");
    });
    *view.embedded.fallback.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(reason.into());
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
                return Err(format!(
                    "CEF no se inició; la aplicación usa el navegador del sistema. El panel avisa: {}",
                    state.engine_notice.as_deref().unwrap_or("nada")
                ));
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
    use super::{accept_languages, display_rate, overlay, Popup};

    #[test]
    fn a_widget_painted_apart_goes_over_the_page_where_it_says_and_only_inside_the_view() {
        // A view of 4 × 3 grey pixels and a list of 2 × 2: one row opaque
        // red, one half-transparent blue, premultiplied as Chromium paints.
        let page = || vec![100u8; 4 * 3 * 3];
        let popup = |at| Popup {
            bgra: vec![0, 0, 255, 255, 0, 0, 255, 255, 128, 0, 0, 128, 128, 0, 0, 128],
            size: [2, 2],
            at,
        };
        let pixel = |rgb: &[u8], x: usize, y: usize| [rgb[(y * 4 + x) * 3], rgb[(y * 4 + x) * 3 + 1], rgb[(y * 4 + x) * 3 + 2]];
        let mut rgb = page();
        overlay(&mut rgb, [4, 3], &popup((1, 1)));
        assert_eq!([pixel(&rgb, 1, 1), pixel(&rgb, 2, 1)], [[255, 0, 0]; 2]);
        // The page shows through what is not opaque.
        assert_eq!(pixel(&rgb, 1, 2), [49, 49, 177]);
        for (x, y) in [(0, 0), (0, 1), (3, 1), (3, 2), (1, 0)] {
            assert_eq!(pixel(&rgb, x, y), [100; 3], "({x}, {y}) is the page's");
        }
        // Past an edge only what is inside is drawn, and nothing wraps.
        let mut rgb = page();
        overlay(&mut rgb, [4, 3], &popup((3, 2)));
        assert_eq!(pixel(&rgb, 3, 2), [255, 0, 0]);
        assert_eq!(rgb.iter().filter(|channel| **channel != 100).count(), 3);
        let mut rgb = page();
        overlay(&mut rgb, [4, 3], &popup((-1, -1)));
        assert_eq!(pixel(&rgb, 0, 0), [49, 49, 177]);
        assert_eq!(rgb.iter().filter(|channel| **channel != 100).count(), 3);
        for outside in [(4, 0), (0, 3), (-2, 0), (0, -2), (-9, -9), (9, 9)] {
            let mut rgb = page();
            overlay(&mut rgb, [4, 3], &popup(outside));
            assert_eq!(rgb, page(), "{outside:?}");
        }
    }

    #[test]
    fn the_windows_quick_frames_say_the_displays_refresh_rate() {
        let run = |interval: f32| -> Vec<f32> {
            // A wake-up, then steady frames with a slow one now and then.
            let mut frames = vec![2.5, 0.2, 0.05];
            frames.extend((0..45).map(|index| if index % 7 == 0 { interval * 3.0 } else { interval }));
            frames
        };
        assert_eq!(display_rate(&run(1.0 / 119.6)), Some(120));
        assert_eq!(display_rate(&run(1.0 / 60.3)), Some(60));
        assert_eq!(display_rate(&run(1.0 / 143.2)), Some(144));
        // Not a usual rate: taken as measured, within what a display has.
        assert_eq!(display_rate(&run(1.0 / 50.0)), Some(50));
        assert_eq!(display_rate(&run(1.0 / 2000.0)), Some(240));
        assert_eq!(display_rate(&run(1.0)), Some(30));
        // Too few frames, or none that mean anything, say nothing.
        assert_eq!(display_rate(&[0.008; 20]), None);
        assert_eq!(display_rate(&[f32::NAN; 48]), None);
        assert_eq!(display_rate(&[0.0; 48]), None);
    }

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
