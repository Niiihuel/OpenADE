//! One windowless page in a browser of its own.
//!
//! CEF calls the handlers below on its UI thread. They only move data out:
//! pixels to the embedder's painter, DevTools messages to a bounded channel.
//! Nothing here ever shows a window: popups, dialogs, file choosers, context
//! menus and downloads are refused or left to the driver.

use crate::{Options, OPAQUE_WHITE};
use cef::*;
use openide_browser::transport::{Incoming, MessageSink, MessageSource, Transport, MAX_MESSAGE_BYTES};
use std::io;
use std::os::raw::c_int;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

const CREATE_TIMEOUT: Duration = Duration::from_secs(20);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);
/// DevTools messages CEF may be ahead of the driver's reader thread. Past
/// that CEF's UI thread waits, as a browser on a pipe would.
const MESSAGE_BACKLOG: usize = 256;
/// Leading bytes kept of a message over the transport's limit: its `id`.
const OVERSIZED_PREFIX: usize = 256;

/// One paint by Chromium's software compositor. The pixels are valid only
/// during the call.
pub struct Paint<'a> {
    pub width: u32,
    pub height: u32,
    /// Rows of premultiplied BGRA, top to bottom.
    pub bgra: &'a [u8],
    /// `None` when the pixels are the whole view. A widget Chromium paints
    /// apart, such as the list of a `<select>`, says where its top-left
    /// corner goes in the view, in device pixels; without pixels, it closed.
    pub popup: Option<(i32, i32)>,
}

/// The pointer a page asks for, as CEF names it.
pub use cef::sys::cef_cursor_type_t as Cursor;

/// CEF's objects are reference counted across threads and say which thread
/// each call needs; cef-rs leaves stating that to the embedder.
struct Threadsafe<T>(T);
unsafe impl<T> Send for Threadsafe<T> {}
unsafe impl<T> Sync for Threadsafe<T> {}

struct Core {
    /// The view in CSS pixels, and the device pixels each one takes: what
    /// the render handler answers with.
    width: AtomicI32,
    height: AtomicI32,
    scale: f32,
    paint: Box<dyn Fn(&Paint) + Send + Sync>,
    /// Where the widget painted apart goes in the view, in device pixels.
    popup: Mutex<(i32, i32)>,
    cursor: Mutex<CursorType>,
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    host: Option<Threadsafe<BrowserHost>>,
    /// Keeps the DevTools observer registered.
    registration: Option<Threadsafe<Registration>>,
    /// Where DevTools messages go. Gone with the page, which ends the transport.
    messages: Option<SyncSender<Incoming>>,
    /// A JavaScript dialog the driver has yet to answer. CEF dismisses a
    /// dialog whose callback nobody holds.
    dialog: Option<Threadsafe<JsdialogCallback>>,
    closed: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Releasing the last reference to a CEF object may call back into a handler
/// of this file, which locks the state again: what leaves the state is
/// dropped only after the lock is.
fn outside_the_lock<T>(taken: T) {
    drop(taken);
}

impl Core {
    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn view(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: self.width.load(Ordering::Relaxed),
            height: self.height.load(Ordering::Relaxed),
        }
    }

    fn hold_dialog(&self, callback: Option<&mut JsdialogCallback>) {
        let held = callback.cloned().map(Threadsafe);
        let answered = std::mem::replace(&mut self.state().dialog, held);
        outside_the_lock(answered);
    }

    /// Asks CEF to close the browser. A page that never appeared is closed
    /// when it does.
    fn ask_to_close(&self) {
        let mut state = self.state();
        let asked = state.host.as_ref().map(|host| host.0.close_browser(1)).is_some();
        if !asked {
            state.closed = true;
            state.messages = None;
        }
    }

    /// Closes the browser and waits for CEF to say it is gone.
    fn close(&self) {
        self.ask_to_close();
        let mut state = self.state();
        let deadline = Instant::now() + CLOSE_TIMEOUT;
        while !state.closed {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|poison| poison.into_inner())
                .0;
        }
    }
}

/// Every page that may still be open, for the shutdown of CEF.
static PAGES: Mutex<Vec<Weak<Core>>> = Mutex::new(Vec::new());

pub(crate) fn close_all() {
    let pages = std::mem::take(&mut *PAGES.lock().unwrap_or_else(|poison| poison.into_inner()));
    for core in pages.iter().filter_map(Weak::upgrade) {
        core.close();
    }
}

wrap_render_handler! {
    struct Render {
        core: Arc<Core>,
    }
    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                *rect = self.core.view();
            }
        }

        fn screen_info(&self, _browser: Option<&mut Browser>, screen_info: Option<&mut ScreenInfo>) -> c_int {
            let Some(info) = screen_info else { return 0 };
            // Honoured on the headless platform CEF runs on here. Under
            // Wayland it is not: a windowless view there is always at 1.
            info.device_scale_factor = self.core.scale;
            info.depth = 24;
            info.depth_per_component = 8;
            info.rect = self.core.view();
            info.available_rect = self.core.view();
            1
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: c_int,
            height: c_int,
        ) {
            if buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }
            let (width, height) = (width as u32, height as u32);
            // The page, or the list of a <select> that goes over it.
            let popup = (type_ != PaintElementType::default()).then(|| *lock(&self.core.popup));
            // Always the whole of it. The dirty rectangles are not passed on:
            // the embedder shows whole frames.
            let bgra = unsafe { std::slice::from_raw_parts(buffer, width as usize * height as usize * 4) };
            (self.core.paint)(&Paint { width, height, bgra, popup });
        }

        fn on_popup_show(&self, _browser: Option<&mut Browser>, show: c_int) {
            if show == 0 {
                (self.core.paint)(&Paint { width: 0, height: 0, bgra: &[], popup: Some((0, 0)) });
            }
        }

        fn on_popup_size(&self, _browser: Option<&mut Browser>, rect: Option<&Rect>) {
            // In CSS pixels of the view; its paint follows.
            if let Some(rect) = rect {
                let device = |css: c_int| (css as f32 * self.core.scale).round() as i32;
                *lock(&self.core.popup) = (device(rect.x), device(rect.y));
            }
        }
    }
}

wrap_display_handler! {
    struct Display {
        core: Arc<Core>,
    }
    impl DisplayHandler {
        fn on_cursor_change(
            &self,
            _browser: Option<&mut Browser>,
            _cursor: std::os::raw::c_ulong,
            type_: CursorType,
            _custom_cursor_info: Option<&CursorInfo>,
        ) -> c_int {
            // There is no window to set it on: whoever shows the page does.
            *lock(&self.core.cursor) = type_;
            1
        }
    }
}

wrap_life_span_handler! {
    struct Life {
        core: Arc<Core>,
    }
    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: c_int,
            _target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut c_int>,
        ) -> c_int {
            // One panel has one page: no other window is ever created.
            1
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let Some(host) = browser.and_then(|browser| browser.host()) else { return };
            let mut state = self.core.state();
            if state.closed {
                // Whoever asked for this page stopped waiting for it.
                host.close_browser(1);
                return;
            }
            // Registered here, on CEF's UI thread, before any message can come.
            let mut observer = DevTools::new(self.core.clone());
            state.registration = host.add_dev_tools_message_observer(Some(&mut observer)).map(Threadsafe);
            state.host = Some(Threadsafe(host));
            drop(state);
            self.core.changed.notify_all();
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            let closed = State { closed: true, ..State::default() };
            let ended = std::mem::replace(&mut *self.core.state(), closed);
            outside_the_lock(ended);
            self.core.changed.notify_all();
        }
    }
}

wrap_dev_tools_message_observer! {
    struct DevTools {
        core: Arc<Core>,
    }
    impl DevToolsMessageObserver {
        fn on_dev_tools_message(&self, _browser: Option<&mut Browser>, message: Option<&[u8]>) -> c_int {
            let sender = self.core.state().messages.clone();
            if let (Some(sender), Some(message)) = (sender, message) {
                let oversized = message.len() > MAX_MESSAGE_BYTES;
                let kept = if oversized { &message[..OVERSIZED_PREFIX] } else { message };
                let _ = sender.send(Incoming { bytes: kept.to_vec(), oversized });
            }
            // Taken whole: CEF need not parse it for the other callbacks.
            1
        }

        fn on_dev_tools_agent_detached(&self, _browser: Option<&mut Browser>) {
            // Sessions do not outlive the agent: the driver is told it ended.
            self.core.state().messages = None;
        }
    }
}

wrap_jsdialog_handler! {
    struct Dialogs {
        core: Arc<Core>,
    }
    impl JsdialogHandler {
        fn on_jsdialog(
            &self,
            _browser: Option<&mut Browser>,
            _origin_url: Option<&CefString>,
            _dialog_type: JsdialogType,
            _message_text: Option<&CefString>,
            _default_prompt_text: Option<&CefString>,
            callback: Option<&mut JsdialogCallback>,
            _suppress_message: Option<&mut c_int>,
        ) -> c_int {
            // Nothing is shown. DevTools announces every dialog too, and the
            // driver answers it there; until then the dialog stays open.
            self.core.hold_dialog(callback);
            1
        }

        fn on_before_unload_dialog(
            &self,
            _browser: Option<&mut Browser>,
            _message_text: Option<&CefString>,
            _is_reload: c_int,
            callback: Option<&mut JsdialogCallback>,
        ) -> c_int {
            self.core.hold_dialog(callback);
            1
        }

        fn on_reset_dialog_state(&self, _browser: Option<&mut Browser>) {
            self.core.hold_dialog(None);
        }
    }
}

wrap_dialog_handler! {
    struct Files;
    impl DialogHandler {
        fn on_file_dialog(
            &self,
            _browser: Option<&mut Browser>,
            _mode: FileDialogMode,
            _title: Option<&CefString>,
            _default_file_path: Option<&CefString>,
            _accept_filters: Option<&mut CefStringList>,
            _accept_extensions: Option<&mut CefStringList>,
            _accept_descriptions: Option<&mut CefStringList>,
            callback: Option<&mut FileDialogCallback>,
        ) -> c_int {
            // No file chooser opens on the user's desktop for a page.
            if let Some(callback) = callback {
                callback.cancel();
            }
            1
        }
    }
}

wrap_context_menu_handler! {
    struct Menus;
    impl ContextMenuHandler {
        fn on_before_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
        ) {
            // An empty menu is not shown; a page's own menu is the page's to draw.
            if let Some(model) = model {
                model.clear();
            }
        }
    }
}

wrap_download_handler! {
    struct Downloads;
    impl DownloadHandler {
        fn can_download(
            &self,
            _browser: Option<&mut Browser>,
            _url: Option<&CefString>,
            _request_method: Option<&CefString>,
        ) -> c_int {
            // Nothing is ever written to the user's disk on a page's behalf.
            0
        }
    }
}

wrap_client! {
    struct PageClient {
        render: RenderHandler,
        display: DisplayHandler,
        life: LifeSpanHandler,
        dialogs: JsdialogHandler,
        files: DialogHandler,
        menus: ContextMenuHandler,
        downloads: DownloadHandler,
    }
    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life.clone())
        }
        fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
            Some(self.dialogs.clone())
        }
        fn dialog_handler(&self) -> Option<DialogHandler> {
            Some(self.files.clone())
        }
        fn context_menu_handler(&self) -> Option<ContextMenuHandler> {
            Some(self.menus.clone())
        }
        fn download_handler(&self) -> Option<DownloadHandler> {
            Some(self.downloads.clone())
        }
    }
}

struct Sink(Arc<Core>);

impl MessageSink for Sink {
    fn send(&mut self, message: &[u8]) -> io::Result<()> {
        match &self.0.state().host {
            // Off CEF's UI thread the message is copied and posted to it, so
            // the call's own result says nothing.
            Some(host) => {
                host.0.send_dev_tools_message(Some(message));
                Ok(())
            }
            None => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }
}

struct Source(Receiver<Incoming>);

impl MessageSource for Source {
    fn receive(&mut self) -> io::Result<Option<Incoming>> {
        Ok(self.0.recv().ok())
    }
}

/// A page nobody else shares: its cookies, cache and local storage live in
/// memory, belong to this page alone and are gone when it closes.
pub struct Page {
    core: Arc<Core>,
    source: Mutex<Option<Receiver<Incoming>>>,
}

impl Page {
    /// Opens `about:blank`, starting CEF if this is the first page of the
    /// process. `size` is the view in CSS pixels and `scale` the device
    /// pixels each of them takes, for the life of the page. `paint` runs on
    /// CEF's UI thread for each frame Chromium composites, at most
    /// `frame_rate` times a second: it must take what it needs and return.
    pub fn open(
        options: &Options,
        size: (u32, u32),
        scale: f32,
        frame_rate: u32,
        paint: impl Fn(&Paint) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        crate::ensure_started(options)?;
        let (messages, source) = mpsc::sync_channel(MESSAGE_BACKLOG);
        let core = Arc::new(Core {
            width: AtomicI32::new(size.0 as i32),
            height: AtomicI32::new(size.1 as i32),
            scale,
            paint: Box::new(paint),
            popup: Mutex::default(),
            cursor: Mutex::default(),
            state: Mutex::new(State {
                messages: Some(messages),
                ..State::default()
            }),
            changed: Condvar::new(),
        });
        // Without a cache path the context is an off-the-record profile of
        // its own. The browser keeps it alive and releases it when it closes.
        let mut context = request_context_create_context(Some(&RequestContextSettings::default()), None)
            .ok_or("CEF no creó el contexto de la página.")?;
        let window = WindowInfo {
            windowless_rendering_enabled: 1,
            ..Default::default()
        };
        let settings = BrowserSettings {
            windowless_frame_rate: frame_rate as c_int,
            background_color: OPAQUE_WHITE,
            ..Default::default()
        };
        let mut client = PageClient::new(
            Render::new(core.clone()),
            Display::new(core.clone()),
            Life::new(core.clone()),
            Dialogs::new(core.clone()),
            Files::new(),
            Menus::new(),
            Downloads::new(),
        );
        let page = Self {
            core,
            source: Mutex::new(Some(source)),
        };
        let asked = browser_host_create_browser(
            Some(&window),
            Some(&mut client),
            Some(&"about:blank".into()),
            Some(&settings),
            None,
            Some(&mut context),
        );
        if asked != 1 {
            return Err("CEF no creó la página.".into());
        }
        let deadline = Instant::now() + CREATE_TIMEOUT;
        let mut state = page.core.state();
        while state.host.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || state.closed {
                // Dropping `page` closes the browser if it still appears.
                return Err("CEF no creó la página a tiempo.".into());
            }
            state = page
                .core
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|poison| poison.into_inner())
                .0;
        }
        // Keys and the caret go to a view that has the focus.
        if let Some(host) = &state.host {
            host.0.set_focus(1);
        }
        drop(state);
        let mut pages = PAGES.lock().unwrap_or_else(|poison| poison.into_inner());
        pages.retain(|open| open.strong_count() > 0);
        pages.push(Arc::downgrade(&page.core));
        drop(pages);
        Ok(page)
    }

    /// The DevTools endpoint of this page, for `Driver::connect_to_page`.
    /// It ends when the page closes. There is one: later calls get `None`.
    pub fn transport(&self) -> Option<Transport> {
        let source = self.source.lock().unwrap_or_else(|poison| poison.into_inner()).take()?;
        Some(Transport {
            sink: Box::new(Sink(self.core.clone())),
            source: Box::new(Source(source)),
        })
    }

    fn host(&self, call: impl FnOnce(&BrowserHost)) {
        if let Some(host) = &self.core.state().host {
            call(&host.0);
        }
    }

    /// Gives the view a new size in CSS pixels.
    pub fn resize(&self, width: u32, height: u32) {
        self.core.width.store(width as i32, Ordering::Relaxed);
        self.core.height.store(height as i32, Ordering::Relaxed);
        self.host(|host| host.was_resized());
    }

    /// How often Chromium may composite and paint the page, in frames a second.
    pub fn set_frame_rate(&self, frame_rate: u32) {
        self.host(|host| host.set_windowless_frame_rate(frame_rate as c_int));
    }

    /// Asks for a paint of the whole view, and of a widget open over it,
    /// also when nothing changed.
    pub fn invalidate(&self) {
        self.host(|host| {
            host.invalidate(PaintElementType::default());
            host.invalidate(sys::cef_paint_element_type_t::PET_POPUP.into());
        });
    }

    /// The pointer the page asks for where the pointer last was.
    pub fn cursor(&self) -> Cursor {
        *lock(&self.core.cursor).as_ref()
    }

    // CEF's own pointer events at a point of the view, in CSS pixels. The
    // browser's input goes through the driver, whose events only ever reach
    // the page: these also reach a widget Chromium paints apart, when the
    // point is on it.

    pub fn pointer_move(&self, x: i32, y: i32) {
        let event = MouseEvent { x, y, modifiers: 0 };
        self.host(|host| host.send_mouse_move_event(Some(&event), 0));
    }

    /// The left button going down, or coming `up`. `clicks` is 2 for the
    /// second press of a double click.
    pub fn pointer_button(&self, x: i32, y: i32, up: bool, clicks: u8) {
        let event = MouseEvent { x, y, modifiers: 0 };
        self.host(|host| host.send_mouse_click_event(Some(&event), MouseButtonType::default(), up.into(), clicks.into()));
    }

    /// A turn of the wheel. CEF counts one towards the user as negative.
    pub fn wheel(&self, x: i32, y: i32, dx: i32, dy: i32) {
        let event = MouseEvent { x, y, modifiers: 0 };
        self.host(|host| host.send_mouse_wheel_event(Some(&event), dx, dy));
    }

    /// Starts closing the page, without waiting: what a driver is asking of
    /// it ends at once. Dropping the page closes it and waits until it is gone.
    pub fn close(&self) {
        self.core.ask_to_close();
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        self.core.close();
    }
}
