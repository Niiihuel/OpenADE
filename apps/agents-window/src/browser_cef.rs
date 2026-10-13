//! A browser context on the embedded Chromium: a CEF page Chromium composites
//! on the CPU, driven by `openide_browser::Driver` through the DevTools
//! protocol inside this process.
//!
//! Chromium hands over each frame uncompressed and says which rectangle of
//! it changed. The paint callback, on CEF's UI thread, copies that rectangle
//! into the one canvas below, and the runtime's frame thread turns what was
//! painted since its last turn into the part of a frame the view takes: a
//! change costs what it covers. Nothing queues: a paint the view has not
//! taken is painted over by the next, which then carries its rectangle too.

use super::*;
use eframe::egui;
use std::hash::Hasher;
use std::sync::atomic::{AtomicU32, AtomicU64};

/// Counts the paints after which the canvas was not, pixel for pixel, what
/// Chromium had just painted: that the rectangle it names is all that
/// changed is what every part of a frame rests on. A measurement turns the
/// comparison off.
#[cfg(test)]
pub(super) static STRAY_PAINTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
pub(super) static COMPARE_PAINTS: AtomicBool = AtomicBool::new(true);

/// How often Chromium composites a page that is on screen, or that a command
/// is waiting on: as often as the window itself is presented, which under
/// vsync is the display's refresh rate. Sixty until the window has said.
static DISPLAY_RATE: AtomicU32 = AtomicU32::new(60);
/// The window's first frame intervals with a browser open.
static WINDOW_FRAMES: Mutex<Vec<f32>> = Mutex::new(Vec::new());
const WINDOW_FRAMES_WANTED: usize = 48;
/// The most pixels one frame of the view may hold: the largest viewport,
/// 1600 × 1200, fits whole at scale 2.
const MAX_FRAME_PIXELS: f32 = 3840.0 * 2160.0;

/// The scale a page is created at: the window's, unless the largest viewport
/// would then paint more than a frame may hold.
// shortcut: capped once, for the largest viewport. A page's scale is fixed
// when CEF creates it; a display denser than 2x that must keep its own on a
// small panel needs the page created again on resize.
fn page_scale(scale: f32) -> f32 {
    let fit = (MAX_FRAME_PIXELS / ((1600.0 * scale).ceil() * (1200.0 * scale).ceil())).sqrt();
    if fit >= 1.0 {
        scale
    } else {
        (scale * fit * 100.0).floor() / 100.0
    }
}

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

/// A measurement asks for the rate of another display. The window alone
/// decides it in the application.
#[cfg(test)]
pub(super) fn set_display_rate(rate: u32) {
    DISPLAY_RATE.store(rate, Ordering::Relaxed);
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
/// The tab a context starts with.
pub(super) const FIRST_TAB: u64 = 1;

#[derive(Default)]
struct Canvas {
    /// The page as Chromium last painted it: rows of BGRA.
    bgra: Vec<u8>,
    size: [usize; 2],
    /// Device pixels per CSS pixel of those rows.
    scale: f32,
    /// What was painted since the view was last given a frame, as one
    /// rectangle: the columns `[0]..[2]` of the rows `[1]..[3]`.
    // shortcut: one rectangle, as Chromium itself reports a paint; two small
    // changes far apart cost the box around both. A list of rectangles is the
    // upgrade, if a page of that kind ever shows in a measurement.
    damage: Option<[usize; 4]>,
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
    /// What a turn of the wheel left unsent: less than a pixel per axis.
    wheel_rest: (f64, f64),
}

struct Popup {
    bgra: Vec<u8>,
    size: [usize; 2],
    /// Where its top-left corner goes in the view. All in device pixels.
    at: (i32, i32),
}

impl Popup {
    /// The rectangle of the view it is drawn over, which may reach past it.
    fn covers(&self) -> [usize; 4] {
        let edge = |from: i32, length: usize| (i64::from(from) + length as i64).max(0) as usize;
        [edge(self.at.0, 0), edge(self.at.1, 0), edge(self.at.0, self.size[0]), edge(self.at.1, self.size[1])]
    }
}

impl Canvas {
    /// What of `rect` lies on the page has been painted.
    fn damaged(&mut self, rect: [usize; 4]) {
        let rect = [rect[0], rect[1], rect[2].min(self.size[0]), rect[3].min(self.size[1])];
        if rect[0] < rect[2] && rect[1] < rect[3] {
            self.damage = Some(self.damage.map_or(rect, |damage| around(damage, rect)));
        }
    }
}

/// The smallest rectangle that holds both.
fn around(one: [usize; 4], other: [usize; 4]) -> [usize; 4] {
    [one[0].min(other[0]), one[1].min(other[1]), one[2].max(other[2]), one[3].max(other[3])]
}

/// Draws the rows of `popup` that fall inside the rectangle of the view that
/// starts at `corner` and measures `size`, over the pixels of that rectangle.
fn overlay(rgb: &mut [u8], size: [usize; 2], corner: [usize; 2], popup: &Popup) {
    let at = (popup.at.0 - corner[0] as i32, popup.at.1 - corner[1] as i32);
    let (left, top) = (at.0.max(0) as usize, at.1.max(0) as usize);
    // What of the popup lies left of or above the rectangle is not drawn.
    let (skip_x, skip_y) = ((-at.0).max(0) as usize, (-at.1).max(0) as usize);
    let width = popup.size[0].saturating_sub(skip_x).min(size[0].saturating_sub(left));
    // Beside the rectangle there is no row of it to draw on.
    if width == 0 {
        return;
    }
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

/// The embedded page of one context.
#[derive(Default)]
pub(super) struct Embedded {
    /// The page of the tab the view shows.
    page: Mutex<Option<Arc<openide_cef::Page>>>,
    /// The pages of the other tabs, by tab. They share the first one's
    /// cookies, cache and storage, and their paints go nowhere.
    parked: Mutex<Vec<(u64, Arc<openide_cef::Page>)>>,
    /// The tab whose page is `page`.
    active: AtomicU64,
    canvas: Mutex<Canvas>,
    /// The frame thread's own: the canvas in the channel order a frame is
    /// built from, kept between frames.
    rgb: Mutex<Vec<u8>>,
}

/// Where a page's downloads go: the user's downloads folder, as the desktop
/// names it, or `Downloads` in their home.
pub(super) fn downloads_directory() -> PathBuf {
    // A test's downloads are not the user's.
    #[cfg(test)]
    return std::env::temp_dir().join(format!("openide-test-downloads-{}", std::process::id()));
    #[cfg(not(test))]
    {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let config = std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
        std::fs::read_to_string(config.join("user-dirs.dirs"))
            .ok()
            .and_then(|dirs| user_download_dir(&dirs, &home))
            .unwrap_or_else(|| home.join("Downloads"))
    }
}

/// `XDG_DOWNLOAD_DIR` as `user-dirs.dirs` gives it: an absolute path, or one under `$HOME`.
fn user_download_dir(dirs: &str, home: &Path) -> Option<PathBuf> {
    let value = dirs.lines().find_map(|line| line.trim().strip_prefix("XDG_DOWNLOAD_DIR="))?.trim().trim_matches('"');
    let path = match value.strip_prefix("$HOME/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(value),
    };
    (path.is_absolute() && path != home).then_some(path)
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
/// `es_AR.UTF-8` is `es-AR,es,en`.
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
    fn page(&self) -> MutexGuard<'_, Option<Arc<openide_cef::Page>>> {
        self.page.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn parked(&self) -> MutexGuard<'_, Vec<(u64, Arc<openide_cef::Page>)>> {
        self.parked.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// Opens the page of another tab beside the context's first, with a
    /// driver of its own. The view goes on showing the tab it shows.
    pub(super) fn open_tab(view: &Arc<LiveView>, tab: u64) -> Result<Driver, String> {
        // Not under the lock: the window asks this page for its cursor every frame.
        let first = view.embedded.page().clone().ok_or("El navegador no tiene una página abierta.")?;
        let scale = view.embedded.canvas().scale;
        let painted = Arc::downgrade(view);
        let page = first.open_beside(scale, IDLE_FRAME_RATE, move |paint| {
            if let Some(view) = painted.upgrade().filter(|view| view.embedded.active.load(Ordering::Acquire) == tab) {
                view.embedded.paint(&view, paint);
            }
        })?;
        let transport = page.transport().ok_or("La pestaña no tiene conexión DevTools.")?;
        let (width, height) = page.size();
        let driver = Driver::connect_to_page(transport, width, height, f64::from(scale)).map_err(|error| error.message)?;
        view.embedded.parked().push((tab, Arc::new(page)));
        Ok(driver)
    }

    /// Shows another tab's page in the view, at the size the view has now.
    /// The one it showed is kept beside. False when there is no such tab.
    pub(super) fn select_tab(&self, tab: u64) -> bool {
        let mut parked = self.parked();
        let Some(index) = parked.iter().position(|(id, _)| *id == tab) else {
            return false;
        };
        let (_, page) = parked.swap_remove(index);
        let previous = self.page().replace(page.clone());
        let was = self.active.swap(tab, Ordering::AcqRel);
        if let Some(previous) = previous {
            page.resize(previous.size().0, previous.size().1);
            previous.set_frame_rate(IDLE_FRAME_RATE);
            parked.push((was, previous));
        }
        drop(parked);
        // The view holds the other tab's picture: this one is painted whole.
        let mut canvas = self.canvas();
        canvas.bgra = Vec::new();
        canvas.size = [0, 0];
        canvas.popup = None;
        canvas.damage = None;
        canvas.before_capture = None;
        canvas.wheel_rest = (0.0, 0.0);
        let (visible, rate) = (canvas.visible, canvas.rate);
        drop(canvas);
        page.set_frame_rate(rate);
        if visible {
            page.invalidate();
        }
        true
    }

    /// The host opens tabs: a link clicked for another tab asks for one
    /// instead of opening in the page.
    pub(super) fn open_tabs_apart(&self) {
        if let Some(page) = &*self.page() {
            page.open_tabs_apart();
        }
    }

    /// The address a click on the page shown last asked to see in another tab.
    pub(super) fn take_tab_request(&self) -> Option<String> {
        self.page().as_ref().and_then(|page| page.take_tab_request())
    }

    /// What the pages of every tab asked to save, oldest first.
    pub(super) fn downloads(&self) -> Vec<BrowserDownloadView> {
        let mut all: Vec<openide_cef::Download> = self.page().iter().flat_map(|page| page.downloads()).collect();
        for (_, page) in &*self.parked() {
            all.extend(page.downloads());
        }
        all.sort_by_key(|download| download.id);
        all.into_iter()
            .map(|download| BrowserDownloadView {
                name: download.name,
                path: download.path.to_string_lossy().into_owned(),
                received_bytes: download.received,
                total_bytes: download.total,
                state: match download.state {
                    openide_cef::DownloadState::InProgress => "in_progress",
                    openide_cef::DownloadState::Complete => "complete",
                    openide_cef::DownloadState::Failed => "failed",
                },
            })
            .collect()
    }

    /// Closes the page of a tab the view is not showing.
    pub(super) fn close_tab(&self, tab: u64) -> bool {
        let mut parked = self.parked();
        let before = parked.len();
        parked.retain(|(id, _)| *id != tab);
        parked.len() != before
    }

    fn canvas(&self) -> MutexGuard<'_, Canvas> {
        self.canvas.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// Opens this context's page in the embedded Chromium, starting CEF if it
    /// is the first of the process, and connects a driver to it. The failure
    /// says why the context could not start, and whether any other could.
    pub(super) fn start(
        view: &Arc<LiveView>,
        data: &Path,
        viewport: (u32, u32),
        scale: f32,
    ) -> Result<Driver, StartFailure> {
        Self::open(view, data, viewport, page_scale(scale)).map_err(|reason| {
            // CEF starts once per process: a start that failed fails the
            // same way for every later context, and is said once here.
            if openide_cef::start_failure().is_none() {
                return StartFailure {
                    code: "launch_failed",
                    message: reason,
                };
            }
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| eprintln!("OpenIDE: el navegador embebido (CEF) no pudo iniciarse. {reason}"));
            StartFailure {
                code: "engine_failed",
                message: format!("{reason} El navegador embebido se inicia una sola vez por ejecución: cerrá OpenIDE y volvé a abrirlo para reintentar."),
            }
        })
    }

    fn open(view: &Arc<LiveView>, data: &Path, viewport: (u32, u32), scale: f32) -> Result<Driver, String> {
        let painted = Arc::downgrade(view);
        let page = openide_cef::Page::open(
            &host_options(data),
            viewport,
            scale,
            IDLE_FRAME_RATE,
            move |paint| {
                if let Some(view) = painted.upgrade().filter(|view| view.embedded.active.load(Ordering::Acquire) == FIRST_TAB) {
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
        page.save_downloads_to(downloads_directory());
        view.embedded.active.store(FIRST_TAB, Ordering::Release);
        *view.embedded.page() = Some(Arc::new(page));
        Ok(driver)
    }

    /// One paint of the page, on CEF's UI thread, which also carries the
    /// driver's requests: a copy of what Chromium painted again, and a signal.
    fn paint(&self, view: &LiveView, paint: &openide_cef::Paint) {
        let mut canvas = self.canvas();
        if !canvas.visible {
            return;
        }
        let size = [paint.width as usize, paint.height as usize];
        if let Some(at) = paint.popup {
            // Where it was, the page shows again; where it is, it is drawn.
            if let Some(gone) = canvas.popup.take() {
                canvas.damaged(gone.covers());
            }
            let bgra = paint.bgra.to_vec();
            if !bgra.is_empty() {
                let shown = Popup { bgra, size, at };
                canvas.damaged(shown.covers());
                canvas.popup = Some(shown);
            }
            // News whatever the page under it looks like: an agent capture
            // that left the page as it was does not hold this back.
            canvas.before_capture = None;
        } else if size != canvas.size {
            // A page of another size, or the first paint since the view let
            // go of the last: all of it.
            canvas.size = size;
            canvas.bgra.clear();
            canvas.bgra.extend_from_slice(paint.bgra);
            canvas.damage = Some([0, 0, size[0], size[1]]);
        } else {
            // The rest of the canvas already is what Chromium holds. Row by
            // row also when it is all of them: measured, with wide changes
            // copied in one stretch a change of the whole view costs this
            // process a tenth more.
            let [left, top, right, bottom] = paint.dirty;
            let (from, to) = (left.min(size[0]) * 4, right.min(size[0]) * 4);
            for row in (top..bottom.min(size[1])).filter(|_| from < to) {
                let start = row * size[0] * 4;
                canvas.bgra[start + from..start + to].copy_from_slice(&paint.bgra[start + from..start + to]);
            }
            canvas.damaged(paint.dirty);
        }
        // Only a paint of a part rests on the rest being as it was. One of
        // the whole view may also be read while Chromium composites the next
        // frame into the same pixels, whose own paint then follows.
        #[cfg(test)]
        if paint.popup.is_none()
            && paint.dirty != [0, 0, size[0], size[1]]
            && COMPARE_PAINTS.load(Ordering::Relaxed)
            && canvas.bgra != paint.bgra
        {
            STRAY_PAINTS.fetch_add(1, Ordering::Relaxed);
        }
        // A paint of nothing, or of a list over a page never painted, is
        // nothing to show.
        if canvas.damage.is_none() {
            return;
        }
        drop(canvas);
        view.feed().woken = true;
        view.arrived.notify_one();
    }

    /// What was painted since the last call, once: the frame thread asks, and
    /// hands what it gets to the view. It is the whole page when all of it
    /// was painted, and otherwise the one rectangle that was, as a part of
    /// the frame the view already holds. An agent capture makes Chromium draw
    /// the page again; the paints of that moment are held back, and what is
    /// left after it is news only if it differs from what the view already
    /// shows.
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
        let Some(mut damage) = canvas.damage.take().filter(|_| canvas.visible) else {
            // The capture came and went without a paint: a later one is
            // news whatever it shows.
            canvas.before_capture = None;
            return None;
        };
        if canvas
            .before_capture
            .take()
            .is_some_and(|before| before == fingerprint(&canvas.bgra))
        {
            return None;
        }
        let (size, scale) = (canvas.size, canvas.scale);
        let whole = [0, 0, size[0], size[1]];
        // A part the view has yet to take is replaced by this one, which
        // therefore carries its pixels too: the view may skip a frame, and
        // loses nothing that was painted.
        let untaken = view.painted.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some((frame, [left, top])) = untaken.last().and_then(|waiting| waiting.part()) {
            let [width, height] = untaken.last().map_or([0; 2], |waiting| waiting.size());
            damage = if frame == size {
                around(damage, [left, top, left + width, top + height])
            } else {
                whole
            };
        }
        drop(untaken);
        let [left, top, right, bottom] = damage;
        let part = [right - left, bottom - top];
        let mut rgb = self.rgb.lock().unwrap_or_else(|poison| poison.into_inner());
        rgb.resize(part[0] * part[1] * 3, 0);
        for (row, rgb) in (top..bottom).zip(rgb.chunks_exact_mut(part[0] * 3)) {
            let start = (row * size[0] + left) * 4;
            bgra_to_rgb(&canvas.bgra[start..start + part[0] * 4], rgb);
        }
        if let Some(popup) = &canvas.popup {
            overlay(&mut rgb, part, [left, top], popup);
        }
        // CEF may paint again while the frame is built.
        drop(canvas);
        let frame = limn_egui::SurfaceFrame::from_rgb(part, &rgb, scale)?;
        if damage == whole {
            Some(frame)
        } else {
            frame.within(size, [left, top])
        }
    }

    /// The page with what is painted over it, all of it, as a whole frame
    /// would carry it now: its size and its rows of RGB.
    #[cfg(test)]
    pub(super) fn whole(&self) -> ([usize; 2], Vec<u8>) {
        let canvas = self.canvas();
        let mut rgb = vec![0; canvas.bgra.len() / 4 * 3];
        bgra_to_rgb(&canvas.bgra, &mut rgb);
        if let Some(popup) = &canvas.popup {
            overlay(&mut rgb, canvas.size, [0, 0], popup);
        }
        (canvas.size, rgb)
    }

    /// An agent capture is about to redraw the page, and the view already
    /// holds its paints back: remembers what the page looks like now.
    pub(super) fn before_capture(&self) {
        let mut canvas = self.canvas();
        // Kept from a capture just before this one, whose paints are still
        // held back. A paint the view was never given is owed to it.
        if canvas.before_capture.is_none() && canvas.damage.is_none() && canvas.visible {
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
            canvas.size = [0, 0];
            canvas.popup = None;
            canvas.damage = None;
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
        for (_, page) in &*self.parked() {
            page.close();
        }
    }

    /// Closes the page; its cookies, cache and storage go with it.
    pub(super) fn close(&self) {
        let page = self.page().take();
        drop(page);
        self.parked().clear();
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

    /// The window's wheel, turned by CSS pixel deltas at its pointer with
    /// `modifiers` held (`openide_browser::modifiers`). It goes to Chromium
    /// itself: the driver's event returns only once the page has answered
    /// it, a frame later, and the next turn waits for that. Chromium takes
    /// whole pixels; the rest waits for the next turn. False with no page.
    pub(super) fn wheel(&self, dx: f64, dy: f64, modifiers: u8) -> bool {
        use openide_browser::modifiers::{ALT, CONTROL, META, SHIFT};
        let mut canvas = self.canvas();
        let (x, y) = canvas.pointer;
        let total = (canvas.wheel_rest.0 + dx, canvas.wheel_rest.1 + dy);
        let whole = (total.0.trunc(), total.1.trunc());
        canvas.wheel_rest = (total.0 - whole.0, total.1 - whole.1);
        drop(canvas);
        let page = self.page();
        let Some(page) = &*page else {
            return false;
        };
        if whole != (0.0, 0.0) {
            let held = [
                (SHIFT, openide_cef::SHIFT_DOWN),
                (CONTROL, openide_cef::CONTROL_DOWN),
                (ALT, openide_cef::ALT_DOWN),
                (META, openide_cef::COMMAND_DOWN),
            ]
            .into_iter()
            .filter(|(key, _)| modifiers & key != 0)
            .fold(0, |flags, (_, flag)| flags | flag);
            // CEF counts a turn towards the user as negative.
            page.wheel_held(x as i32, y as i32, -whole.0 as i32, -whole.1 as i32, held);
        }
        true
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
        },
    )
    .and_then(|runtime| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let state = runtime.snapshot();
            // What the browser tab would say.
            if let Some(error) = state.error.filter(|_| !state.loading) {
                return Err(error);
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
    use super::{accept_languages, display_rate, overlay, page_scale, LiveView, Popup};
    use openide_cef::Paint;

    #[test]
    fn a_list_that_opens_changes_or_closes_reaches_the_view_also_right_after_an_agent_capture() {
        let view = LiveView::default();
        let embedded = &view.embedded;
        embedded.canvas().scale = 1.0;
        embedded.show(true);
        let white = [255u8; 2 * 2 * 4];
        let page = Paint { width: 2, height: 2, bgra: &white, dirty: [0, 0, 2, 2], popup: None };
        let list = |bgra: &'static [u8]| Paint { width: 1, height: 1, bgra, dirty: [0, 0, 1, 1], popup: Some((1, 1)) };
        // A list alone, before the page was ever painted, is nothing to show.
        embedded.paint(&view, &list(&[0, 0, 255, 255]));
        assert!(embedded.frame(&view).is_none());
        embedded.paint(&view, &page);
        assert!(embedded.frame(&view).is_some(), "the page, with the list over it");
        assert_eq!(embedded.rgb.lock().unwrap()[9..], [255, 0, 0]);
        assert!(embedded.frame(&view).is_none(), "once per paint");
        // An agent capture makes Chromium paint the page again, unchanged:
        // that is not news. What the list did meanwhile is.
        embedded.before_capture();
        embedded.paint(&view, &page);
        assert!(embedded.frame(&view).is_none());
        for (painted, shown) in [(list(&[255, 0, 0, 255]), [0, 0, 255]), (list(&[]), [255, 255, 255])] {
            embedded.before_capture();
            embedded.paint(&view, &page);
            embedded.paint(&view, &painted);
            assert!(embedded.frame(&view).is_some());
            assert_eq!(embedded.rgb.lock().unwrap()[9..], shown);
        }
        // Hidden, the view lets go of the list with the page.
        embedded.paint(&view, &list(&[0, 255, 0, 255]));
        embedded.show(false);
        assert!(embedded.popup_box().is_none() && embedded.frame(&view).is_none());
    }

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
        overlay(&mut rgb, [4, 3], [0, 0], &popup((1, 1)));
        assert_eq!([pixel(&rgb, 1, 1), pixel(&rgb, 2, 1)], [[255, 0, 0]; 2]);
        // The page shows through what is not opaque.
        assert_eq!(pixel(&rgb, 1, 2), [49, 49, 177]);
        for (x, y) in [(0, 0), (0, 1), (3, 1), (3, 2), (1, 0)] {
            assert_eq!(pixel(&rgb, x, y), [100; 3], "({x}, {y}) is the page's");
        }
        // Past an edge only what is inside is drawn, and nothing wraps.
        let mut rgb = page();
        overlay(&mut rgb, [4, 3], [0, 0], &popup((3, 2)));
        assert_eq!(pixel(&rgb, 3, 2), [255, 0, 0]);
        assert_eq!(rgb.iter().filter(|channel| **channel != 100).count(), 3);
        let mut rgb = page();
        overlay(&mut rgb, [4, 3], [0, 0], &popup((-1, -1)));
        assert_eq!(pixel(&rgb, 0, 0), [49, 49, 177]);
        assert_eq!(rgb.iter().filter(|channel| **channel != 100).count(), 3);
        for outside in [(4, 0), (0, 3), (-2, 0), (0, -2), (-9, -9), (9, 9), (9, 0), (5, 2), (-7, 1)] {
            let mut rgb = page();
            overlay(&mut rgb, [4, 3], [0, 0], &popup(outside));
            assert_eq!(rgb, page(), "{outside:?}");
        }
        // Over one rectangle of the view, it is drawn where it falls in it:
        // the columns 2..4 of the rows 1..3 hold its right column alone.
        let mut part = vec![100u8; 2 * 2 * 3];
        overlay(&mut part, [2, 2], [2, 1], &popup((1, 1)));
        assert_eq!(part, [255, 0, 0, 100, 100, 100, 49, 49, 177, 100, 100, 100]);
    }

    #[test]
    fn what_was_painted_reaches_the_view_as_one_part_of_the_frame_and_none_of_it_is_lost() {
        let view = LiveView::default();
        let embedded = &view.embedded;
        embedded.canvas().scale = 1.0;
        embedded.show(true);
        // A page of 4 × 3 pixels, each of them its own grey.
        let mut page: Vec<u8> = (0..12u8).flat_map(|pixel| [pixel, pixel, pixel, 255]).collect();
        let paint = |page: &[u8], width: u32, dirty: [usize; 4]| {
            embedded.paint(&view, &Paint { width, height: page.len() as u32 / 4 / width, bgra: page, dirty, popup: None });
        };
        // The greys the frame thread last built a frame or a part from.
        let built = || embedded.rgb.lock().unwrap().iter().step_by(3).copied().collect::<Vec<u8>>();
        let change = |page: &mut Vec<u8>, x: usize, y: usize| {
            let pixel = (y * 4 + x) * 4;
            let lighter = page[pixel] + 100;
            page[pixel..pixel + 3].fill(lighter);
            paint(page, 4, [x, y, x + 1, y + 1]);
        };
        let strays = super::STRAY_PAINTS.load(std::sync::atomic::Ordering::Relaxed);

        // The first paint is the whole page, whatever Chromium says changed.
        paint(&page, 4, [1, 1, 2, 2]);
        let first = embedded.frame(&view).unwrap();
        assert_eq!((first.size(), first.part()), ([4, 3], None));
        assert_eq!(built(), (0..12).collect::<Vec<u8>>());
        assert!(embedded.frame(&view).is_none(), "once per paint");
        // One pixel changes: that pixel is all that is built and handed over.
        change(&mut page, 2, 1);
        let part = embedded.frame(&view).unwrap();
        assert_eq!((part.size(), part.part()), ([1, 1], Some(([4, 3], [2, 1]))));
        assert_eq!(built(), [106]);
        // Paints the frame thread did not get to in between come as one part.
        change(&mut page, 0, 0);
        change(&mut page, 1, 1);
        let both = embedded.frame(&view).unwrap();
        assert_eq!((both.size(), both.part()), ([2, 2], Some(([4, 3], [0, 0]))));
        assert_eq!(built(), [100, 1, 4, 105]);
        // A part the view never took is replaced by the next, which carries
        // what it carried: the view skips a frame and loses no pixel of it.
        view.publish(both);
        change(&mut page, 2, 0);
        let carried = embedded.frame(&view).unwrap();
        assert_eq!((carried.size(), carried.part()), ([3, 2], Some(([4, 3], [0, 0]))));
        assert_eq!(built(), [100, 1, 102, 4, 105, 106]);
        view.publish(carried);
        assert_eq!(view.painted.lock().unwrap().len(), 1, "the part it never took was replaced");
        // After a whole frame it has yet to take, what changes is still a
        // part: both wait, the frame first.
        let whole = limn_egui::SurfaceFrame::from_rgb([4, 3], &[0; 36], 1.0).unwrap();
        view.publish(whole);
        change(&mut page, 3, 0);
        let after = embedded.frame(&view).unwrap();
        assert_eq!((after.size(), after.part()), ([1, 1], Some(([4, 3], [3, 0]))));
        view.publish(after);
        change(&mut page, 3, 1);
        let again = embedded.frame(&view).unwrap();
        assert_eq!((again.size(), again.part()), ([1, 2], Some(([4, 3], [3, 0]))));
        view.publish(again);
        let waiting: Vec<_> = view.painted.lock().unwrap().drain(..).map(|frame| (frame.size(), frame.part().is_some())).collect();
        assert_eq!(waiting, [([4, 3], false), ([1, 2], true)]);
        // What Chromium says it did not paint is not read again.
        let mut elsewhere = page.clone();
        elsewhere[..4].fill(77);
        paint(&elsewhere, 4, [2, 2, 4, 3]);
        assert_eq!(embedded.frame(&view).unwrap().part(), Some(([4, 3], [2, 2])));
        assert_eq!(embedded.whole().1[..3], [100, 100, 100]);
        // A paint of nothing is no frame.
        paint(&page, 4, [2, 2, 2, 2]);
        paint(&page, 4, [9, 9, 12, 12]);
        assert!(embedded.frame(&view).is_none());

        // While an agent capture redraws the page its paints are held back,
        // and afterwards the view gets what they left changed, in one part.
        embedded.before_capture();
        view.feed().rerendering = true;
        change(&mut page, 1, 2);
        change(&mut page, 2, 2);
        assert!(embedded.frame(&view).is_none());
        view.feed().rerendering = false;
        let after = embedded.frame(&view).unwrap();
        assert_eq!((after.size(), after.part()), ([2, 1], Some(([4, 3], [1, 2]))));
        // A capture that left the page as it was is no news, part or whole.
        embedded.before_capture();
        paint(&page, 4, [0, 0, 4, 3]);
        paint(&page, 4, [1, 1, 3, 2]);
        assert!(embedded.frame(&view).is_none());

        // A list over the page is drawn into the part it falls in, and the
        // page shows again where it was once it moves or closes.
        let list = |at| Paint { width: 1, height: 1, bgra: &[200, 200, 200, 255], dirty: [0, 0, 1, 1], popup: Some(at) };
        embedded.paint(&view, &list((3, 0)));
        let opened = embedded.frame(&view).unwrap();
        assert_eq!((opened.size(), opened.part(), built()), ([1, 1], Some(([4, 3], [3, 0])), vec![200]));
        embedded.paint(&view, &list((1, 0)));
        assert_eq!(embedded.frame(&view).unwrap().part(), Some(([4, 3], [1, 0])));
        assert_eq!(built(), [200, 102, 103]);
        change(&mut page, 2, 0);
        assert_eq!(built(), [200, 102, 103], "nothing is built before the frame thread asks");
        embedded.paint(&view, &Paint { width: 0, height: 0, bgra: &[], dirty: [0; 4], popup: Some((0, 0)) });
        assert_eq!(embedded.frame(&view).unwrap().part(), Some(([4, 3], [1, 0])));
        assert_eq!(built(), [1, 202]);

        // A page of another size is a whole frame, and so is the first paint
        // of a view shown again: its last frame may be gone with it.
        let wider: Vec<u8> = (0..15u8).flat_map(|pixel| [pixel, pixel, pixel, 255]).collect();
        paint(&wider, 5, [0, 0, 1, 1]);
        let resized = embedded.frame(&view).unwrap();
        assert_eq!((resized.size(), resized.part()), ([5, 3], None));
        embedded.show(false);
        paint(&wider, 5, [0, 0, 5, 3]);
        assert!(embedded.frame(&view).is_none(), "a hidden page takes no paint");
        embedded.show(true);
        embedded.paint(&view, &list((0, 0)));
        assert!(embedded.frame(&view).is_none(), "a list has no page to go over yet");
        paint(&wider, 5, [4, 2, 5, 3]);
        let shown = embedded.frame(&view).unwrap();
        assert_eq!((shown.size(), shown.part(), built()[0]), ([5, 3], None, 200));
        // Only the paint that did not say all it painted; it is not left
        // for a scenario on the real engine to find.
        assert_eq!(super::STRAY_PAINTS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed) - strays, 1);
    }

    #[test]
    fn a_page_keeps_the_windows_scale_until_its_largest_frame_would_be_over_the_pixel_budget() {
        for scale in [1.0, 1.5, 2.0] {
            assert_eq!(page_scale(scale), scale);
        }
        let capped = page_scale(3.0);
        assert!((2.07..2.08).contains(&capped), "{capped}");
        assert!((1600.0 * capped).ceil() * (1200.0 * capped).ceil() <= 3840.0 * 2160.0);
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
    fn downloads_go_where_the_desktop_names_its_downloads_folder() {
        use super::user_download_dir;
        use std::path::{Path, PathBuf};
        let home = Path::new("/home/ana");
        let dirs = "# comment\nXDG_DESKTOP_DIR=\"$HOME/Escritorio\"\nXDG_DOWNLOAD_DIR=\"$HOME/Descargas\"\n";
        assert_eq!(user_download_dir(dirs, home), Some(PathBuf::from("/home/ana/Descargas")));
        assert_eq!(user_download_dir("XDG_DOWNLOAD_DIR=\"/datos/bajadas\"", home), Some(PathBuf::from("/datos/bajadas")));
        // Not the home itself, which is how the desktop says there is none, nor a relative path.
        assert_eq!(user_download_dir("XDG_DOWNLOAD_DIR=\"$HOME/\"", home), None);
        assert_eq!(user_download_dir("XDG_DOWNLOAD_DIR=\"bajadas\"", home), None);
        assert_eq!(user_download_dir("XDG_MUSIC_DIR=\"$HOME/Música\"", home), None);
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
