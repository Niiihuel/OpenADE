use super::*;

/// A real bounded actor queue with no process, socket, environment or worker.
/// Host tests explicitly advance replies so startup/navigation races are stable.
pub(crate) struct BrowserCommands {
    pending: Receiver<PendingCommand>,
    handle: BrowserHandle,
}
impl BrowserCommands {
    pub(crate) fn finish_next(&self) -> Option<BrowserCommand> {
        self.finish_next_with_clipboard(None)
    }
    pub(crate) fn finish_next_with_clipboard(
        &self,
        clipboard: Option<BrowserClipboard>,
    ) -> Option<BrowserCommand> {
        let pending = self.pending.try_recv().ok()?;
        pending.lifecycle.store(FINISHED, Ordering::Release);
        let mut snapshot = self.handle.snapshot();
        snapshot.clipboard = clipboard;
        let _ = pending.reply.try_send(Ok(snapshot));
        Some(pending.command)
    }
    pub(crate) fn set_snapshot(&self, snapshot: BrowserSnapshot) {
        *self.handle.state.lock().unwrap() = snapshot;
    }
}

pub(crate) fn fixture(
    scope: BrowserScope,
    snapshot: BrowserSnapshot,
) -> (BrowserRuntime, BrowserCommands) {
    let (queue, pending) = mpsc::sync_channel(COMMAND_CAPACITY);
    let handle = BrowserHandle {
        queue,
        state: Arc::new(Mutex::new(snapshot)),
        closed: Arc::new(AtomicBool::new(false)),
        context_lost: Arc::new(AtomicBool::new(false)),
        visible: Arc::new(AtomicBool::new(false)),
        view: Arc::default(),
        scope,
        identity_digest: "fixture".into(),
    };
    (
        BrowserRuntime {
            handle: handle.clone(),
            child: Arc::new(Mutex::new(None)),
            node_path: PathBuf::new(),
            proxy_path: PathBuf::new(),
            broker_address: String::new(),
            broker_token: String::new(),
        },
        BrowserCommands { pending, handle },
    )
}

fn handle() -> BrowserHandle {
    let (queue, _) = mpsc::sync_channel(1);
    BrowserHandle {
        queue,
        state: Arc::new(Mutex::new(BrowserSnapshot::default())),
        closed: Arc::new(AtomicBool::new(false)),
        context_lost: Arc::new(AtomicBool::new(false)),
        visible: Arc::new(AtomicBool::new(true)),
        view: Arc::default(),
        scope: BrowserScope {
            workspace: PathBuf::from("/fixture"),
            session_id: "browser-test".into(),
        },
        identity_digest: "fixture".into(),
    }
}

#[test]
fn temporary_environment_replaces_every_stale_shell_alias_without_global_mutation() {
    let mut command = Command::new("node");
    for name in ["TMPDIR", "TMP", "TEMP"] {
        command.env(name, "/tmp/deleted-nix-shell");
    }
    let directory = Path::new("/private/browser/tmp");
    browser_temporary_environment(&mut command, directory);
    for name in ["TMPDIR", "TMP", "TEMP", "OPENIDE_BROWSER_TEMP_DIR"] {
        assert_eq!(
            command.get_envs().find(|(key, _)| *key == name).unwrap().1,
            Some(directory.as_os_str())
        );
    }
}

#[test]
fn private_temporary_directory_is_scoped_and_installer_cleanup_is_owned() {
    let data = tempfile::tempdir().unwrap();
    let unrelated = data.path().join("unrelated");
    fs::write(&unrelated, "keep").unwrap();
    let path = data.path().join("install-tmp");
    {
        let temporary = InstallerTemporaryDirectory(path.clone());
        private_directory(&temporary.0).unwrap();
        fs::write(temporary.0.join("artifact"), "test").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
    assert!(!path.exists());
    assert_eq!(fs::read_to_string(unrelated).unwrap(), "keep");
}

#[test]
fn passive_capture_does_not_clear_failed_navigation_and_explicit_retry_does() {
    let handle = handle();
    assert!(apply_response(&handle, &json!({"ok":false,"code":"command_failed","error":"net::ERR_CONNECTION_REFUSED","snapshot":{"url":"http://localhost:1/"}})).is_err());
    let refresh =
        json!({"ok":true,"snapshot":{"url":"http://localhost:1/","title":"No disponible"}});
    assert_eq!(
        apply_refresh_response(&handle, &refresh)
            .unwrap()
            .error
            .as_deref(),
        Some("net::ERR_CONNECTION_REFUSED")
    );
    assert!(apply_response(
        &handle,
        &json!({"ok":true,"snapshot":{"url":"http://localhost:3000/","title":"Proyecto"}})
    )
    .unwrap()
    .error
    .is_none());
}

#[test]
fn command_validation_preserves_viewport_protocol_and_input_bounds() {
    assert!(BrowserCommand::Navigate {
        url: "file:///etc/passwd".into()
    }
    .validate()
    .is_err());
    assert!(BrowserCommand::Navigate {
        url: "https://name:secret@example.test".into()
    }
    .validate()
    .is_err());
    assert!(BrowserCommand::Navigate {
        url: "http://localhost:3000".into()
    }
    .validate()
    .is_ok());
    assert!(BrowserCommand::Scroll {
        dx: f64::NAN,
        dy: 1.0
    }
    .validate()
    .is_err());
    assert!(BrowserCommand::Type {
        text: "á".repeat(8193)
    }
    .validate()
    .is_err());
    assert!(BrowserCommand::Resize {
        width: 800,
        height: 600
    }
    .validate()
    .is_ok());
}

#[test]
fn clipboard_reply_is_bounded_and_never_retained_in_shared_browser_state() {
    let handle = handle();
    let reply = apply_response(&handle, &json!({"ok":true,"snapshot":{"url":"about:blank","clipboard":{"text":"Niño 🧠","cut_token":"token"}}})).unwrap();
    assert_eq!(reply.clipboard.unwrap().text, "Niño 🧠");
    assert!(handle.snapshot().clipboard.is_none());
    assert!(apply_response(
        &handle,
        &json!({"ok":true,"snapshot":{"clipboard":{"text":"x".repeat(16*1024+1)}}})
    )
    .is_err());
}

fn frame_line(format: &str, scale: f64, data: &str) -> Value {
    json!({"event":"frame","seq":1,"format":format,"scale":scale,"data":data})
}

fn encoded(format: FrameFormat, width: u32, height: u32) -> EncodedFrame {
    let mut bytes = Vec::new();
    let pixels = image::RgbImage::from_fn(width, height, |x, y| image::Rgb([x as u8, y as u8, 200]));
    pixels
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            match format {
                FrameFormat::Jpeg => image::ImageFormat::Jpeg,
                FrameFormat::Png => image::ImageFormat::Png,
            },
        )
        .unwrap();
    EncodedFrame {
        format,
        scale: 2.0,
        base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

#[test]
fn frames_bypass_the_reply_channel_and_only_the_newest_undecoded_one_survives() {
    let view = LiveView::default();
    let closed = AtomicBool::new(false);
    // A reply is the command's; an event never takes its place in the queue.
    let reply = json!({"id":7,"ok":true,"snapshot":{"url":"about:blank"}});
    assert_eq!(route_line(reply.clone(), &view), Some(reply));
    for data in ["primero", "segundo", "tercero"] {
        assert!(route_line(frame_line("jpeg", 2.0, data), &view).is_none());
    }
    // The decoder was slower than the stream: it gets the newest frame, and
    // the two it never saw were replaced, not queued.
    let frame = view.next(&closed).unwrap();
    assert_eq!((frame.base64.as_str(), frame.format, frame.scale), ("tercero", FrameFormat::Jpeg, 2.0));
    assert_eq!(view.skipped.load(Ordering::Relaxed), 2);
    assert!(view.encoded.lock().unwrap().is_none());

    // What the bridge may not send never reaches the decoder.
    for line in [
        frame_line("webp", 2.0, "x"),
        frame_line("jpeg", 0.5, "x"),
        frame_line("jpeg", 3.5, "x"),
        frame_line("png", 2.0, ""),
        json!({"event":"frame","format":"png","scale":2.0}),
        json!({"event":"frame","format":"png","scale":"2","data":"x"}),
        json!({"event":"otro"}),
    ] {
        assert!(route_line(line, &view).is_none());
    }
    assert!(view.encoded.lock().unwrap().is_none());
    assert!(route_line(frame_line("png", 1.5, "x"), &view).is_none());
    assert_eq!(view.next(&closed).unwrap().format, FrameFormat::Png);

    assert!(route_line(json!({"event":"stream_lost","error":"Session closed"}), &view).is_none());
    assert_eq!(view.lost.lock().unwrap().as_deref(), Some("Session closed"));
    closed.store(true, Ordering::Release);
    assert!(view.next(&closed).is_none(), "a closed browser ends its decoder");
}

#[test]
fn decoded_frames_keep_their_pixels_and_respect_the_format_and_the_pixel_budget() {
    for format in [FrameFormat::Jpeg, FrameFormat::Png] {
        assert_eq!(decode_frame(&encoded(format, 40, 30)).unwrap().size(), [40, 30]);
    }
    // The declared format is the one decoded, whatever the bytes claim.
    let mut wrong = encoded(FrameFormat::Png, 4, 4);
    wrong.format = FrameFormat::Jpeg;
    assert!(decode_frame(&wrong).is_err());
    let mut broken = encoded(FrameFormat::Jpeg, 4, 4);
    broken.base64 = "no es base64".into();
    assert!(decode_frame(&broken).is_err());

    // One row over the budget is refused from its header, before allocating.
    let mut bytes = Vec::new();
    image::GrayImage::new(3840, 2161)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    let oversized = EncodedFrame {
        format: FrameFormat::Png,
        scale: 2.0,
        base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    };
    assert!(decode_frame(&oversized).unwrap_err().contains("límite de píxeles"));
}

#[test]
fn a_published_frame_wakes_the_window_and_is_taken_once() {
    let (runtime, _commands) = fixture(
        BrowserScope {
            workspace: PathBuf::from("/fixture"),
            session_id: "browser-test".into(),
        },
        BrowserSnapshot::default(),
    );
    let woken = Arc::new(AtomicUsize::new(0));
    let counter = woken.clone();
    runtime.on_frame(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    });
    for width in [8, 16] {
        let frame = decode_frame(&encoded(FrameFormat::Png, width, 4)).unwrap();
        runtime.handle.view.publish(frame);
    }
    // The window was slower than the decoder: it uploads the newest only.
    assert_eq!(woken.load(Ordering::Relaxed), 2);
    assert_eq!(runtime.take_frame().unwrap().size(), [16, 4]);
    assert!(runtime.take_frame().is_none());
}

#[test]
fn pointer_input_is_bounded_like_a_click_and_the_launch_scale_like_a_display() {
    for (x, y, valid) in [
        (0.0, 0.0, true),
        (1600.0, 1200.0, true),
        (-0.5, 10.0, false),
        (10.0, 1200.5, false),
        (f64::NAN, 10.0, false),
        (f64::INFINITY, 10.0, false),
    ] {
        for command in [
            BrowserCommand::PointerMove { x, y },
            BrowserCommand::PointerDown { x, y },
            BrowserCommand::PointerUp { x, y },
        ] {
            assert_eq!(command.validate().is_ok(), valid, "{command:?}");
        }
    }
    // Only input the page answers with frames alone skips the footer.
    assert!(BrowserCommand::PointerMove { x: 1.0, y: 1.0 }.quiet());
    assert!(BrowserCommand::Scroll { dx: 0.0, dy: 1.0 }.quiet());
    assert!(!BrowserCommand::PointerDown { x: 1.0, y: 1.0 }.quiet());
    assert!(!BrowserCommand::Click { x: 1.0, y: 1.0 }.quiet());
    // The agent's catalog does not reach the window's pointer.
    assert!(browser_tool_command("browser_pointer_move", json!({"x":1,"y":1})).is_err());

    for (window, launched) in [
        (1.0, 1.0),
        (1.5, 1.5),
        (2.0, 2.0),
        (1.0999999, 1.1),
        (0.5, 1.0),
        (4.0, 3.0),
        (f32::NAN, 1.0),
        (f32::INFINITY, 1.0),
    ] {
        assert_eq!(device_scale(window), launched, "{window}");
    }
}

#[test]
fn a_lost_stream_says_so_until_it_is_back_and_a_poll_does_not_hide_it() {
    let handle = handle();
    apply_response(&handle, &json!({"ok":true,"snapshot":{"url":"http://localhost:3000/"}})).unwrap();
    mark_stream_lost(&handle, "Session closed");
    let state = handle.snapshot();
    assert_eq!(state.error_code.as_deref(), Some("stream_lost"));
    assert!(state.error.unwrap().contains("La vista en vivo se detuvo"));
    assert!(state.started, "the page and its context are still there");
    // The periodic state poll succeeds and must not make the view look live.
    let poll = json!({"ok":true,"snapshot":{"url":"http://localhost:3000/"}});
    assert!(apply_refresh_response(&handle, &poll).unwrap().error.is_some());
}

/// What one frame costs the host, on frames the real bridge produced:
/// `OPENIDE_BROWSER_E2E_ARTIFACTS=<dir> node --test scripts/browser-e2e.mjs`
/// saves them, and this reads them back from `OPENIDE_BROWSER_FRAME_SAMPLES`.
/// Run it with `--profile desktop`; the development profile is unoptimized.
#[test]
#[ignore = "opt-in timing; needs frames saved by scripts/browser-e2e.mjs"]
fn frame_cost_on_the_decoder_and_ui_threads() {
    let directory = PathBuf::from(
        std::env::var_os("OPENIDE_BROWSER_FRAME_SAMPLES").expect("OPENIDE_BROWSER_FRAME_SAMPLES"),
    );
    let ctx = eframe::egui::Context::default();
    // A window's renderer reports what its GPU takes; a bare context assumes
    // less than a frame at this scale.
    let input = eframe::egui::RawInput {
        max_texture_side: Some(8192),
        ..Default::default()
    };
    ctx.run_ui(input, |_| {}).textures_delta.clear();
    for (name, format) in [
        ("frame-2560x1600.jpg", FrameFormat::Jpeg),
        ("frame-2560x1600.png", FrameFormat::Png),
    ] {
        let bytes = fs::read(directory.join(name)).unwrap();
        let frame = EncodedFrame {
            format,
            scale: 2.0,
            base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        };
        let (mut decode, mut present) = (Vec::new(), Vec::new());
        for _ in 0..40 {
            let started = Instant::now();
            let decoded = decode_frame(&frame).unwrap();
            decode.push(started.elapsed());
            assert_eq!(decoded.size(), [2560, 1600]);
            // The UI thread's own share. The copy to the GPU that follows it
            // inside the renderer is not part of this number.
            let started = Instant::now();
            assert!(limn_egui::present_surface(&ctx, "timing", decoded));
            present.push(started.elapsed());
            ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        }
        decode.sort();
        present.sort();
        eprintln!(
            "{name} ({} KiB): decoder thread median {:.1?}, p90 {:.1?}; UI thread median {:.1?}, p90 {:.1?}",
            bytes.len() / 1024,
            decode[20],
            decode[36],
            present[20],
            present[36],
        );
    }
    limn_egui::forget_surface("timing");
}

/// The production path short of the GPU: the real bridge and Chromium, this
/// actor, its reader and its decoder. It needs Node, a Chromium and the
/// Playwright installed at `OPENIDE_PLAYWRIGHT_MODULE_DIR`, uses a throwaway
/// profile and data root, and only visits a page it serves on localhost.
#[test]
#[ignore = "opt-in: launches the real bridge and Chromium against a local fixture"]
fn the_actor_streams_decoded_frames_of_a_real_page_only_while_visible() {
    let module_dir = PathBuf::from(
        std::env::var_os("OPENIDE_PLAYWRIGHT_MODULE_DIR").expect("OPENIDE_PLAYWRIGHT_MODULE_DIR"),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let body = "<!doctype html><title>Fixture</title><style>@keyframes turn{to{transform:rotate(360deg)}}div{width:80px;height:80px;margin:40px;background:linear-gradient(45deg,#f06,#48f);animation:turn 1s linear infinite}</style><div></div><p>Texto de la página</p>";
        for mut stream in listener.incoming().flatten() {
            let _ = stream.read(&mut [0u8; 2048]);
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    let (data, workspace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let runtime = BrowserRuntime::start(
        BrowserScope {
            workspace: workspace.path().into(),
            session_id: "stream-test".into(),
        },
        data.path(),
        BrowserLaunchOptions {
            playwright_module_dir: Some(module_dir),
            viewport_width: 1280,
            viewport_height: 800,
            device_scale: 2.0,
            ..Default::default()
        },
    )
    .unwrap();
    let wait = |what: &str, ready: &mut dyn FnMut() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(40);
        while !ready() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}: {:?}", runtime.snapshot().error);
            thread::sleep(Duration::from_millis(5));
        }
    };
    wait("the context to start", &mut || runtime.snapshot().started);
    let mut navigation = runtime
        .submit_ui(BrowserCommand::Navigate {
            url: format!("http://{address}/"),
        })
        .unwrap();
    let mut reply = None;
    wait("the page to open", &mut || {
        reply = navigation.poll();
        reply.is_some()
    });
    assert_eq!(reply.unwrap().unwrap().title, "Fixture");

    // Visible and animating: frames arrive decoded, at device pixels.
    let woken = Arc::new(AtomicUsize::new(0));
    let counter = woken.clone();
    runtime.on_frame(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    });
    wait("a first frame", &mut || runtime.take_frame().is_some());
    let skipped = runtime.handle.view.skipped.load(Ordering::Relaxed);
    let (started, mut frames) = (Instant::now(), 0u32);
    while started.elapsed() < Duration::from_secs(3) {
        if let Some(frame) = runtime.take_frame() {
            assert_eq!(frame.size(), [2560, 1600]);
            frames += 1;
        }
        thread::sleep(Duration::from_millis(1));
    }
    eprintln!(
        "decoded 2560 × 1600 frames taken: {:.1} per second; {} replaced before decoding",
        f64::from(frames) / started.elapsed().as_secs_f64(),
        runtime.handle.view.skipped.load(Ordering::Relaxed) - skipped,
    );
    assert!(frames >= 10, "only {frames} frames in three seconds");
    assert!(woken.load(Ordering::Relaxed) >= frames as usize);
    assert!(runtime.snapshot().error.is_none());

    // Hidden: the screencast stops, and with it the decoder.
    runtime.set_visible(false);
    thread::sleep(Duration::from_millis(600));
    let _ = runtime.take_frame();
    thread::sleep(Duration::from_millis(600));
    assert!(runtime.take_frame().is_none(), "a hidden page pushes no frame");
    // The agent still gets its 1x capture and the page's structure.
    let mut snapshot = runtime.submit(BrowserCommand::Snapshot).unwrap();
    let mut reply = None;
    wait("the agent's snapshot", &mut || {
        reply = snapshot.poll();
        reply.is_some()
    });
    let reply = reply.unwrap().unwrap();
    let image = reply.screenshot.expect("a capture for the agent");
    assert_eq!((image.width, image.height), (1280, 800));
    assert!(reply.aria_snapshot.contains("Texto de la página"));
    runtime.shutdown();
}
