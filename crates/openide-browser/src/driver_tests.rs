use super::*;
use crate::testing::{message, serve, transport};
use crate::ConsoleLevel;
use std::sync::mpsc::Sender;

/// A driver attached to an endpoint the test scripts. Everything the driver
/// needs to start is answered; `answer` gets the rest first.
fn driver(
    mut answer: impl FnMut(&str, &Value) -> Option<Result<Value, &'static str>> + Send + 'static,
) -> (Driver, Sender<crate::transport::Incoming>) {
    let (transport, written, incoming) = transport();
    let events = incoming.clone();
    serve(written, incoming, move |method, params| {
        if let Some(reply) = answer(method, params) {
            return Some(reply);
        }
        Some(Ok(match method {
            "Target.createBrowserContext" => json!({"browserContextId": "CONTEXT"}),
            "Target.createTarget" => json!({"targetId": "MAIN"}),
            "Target.attachToTarget" => json!({"sessionId": "PAGE"}),
            "Page.createIsolatedWorld" => json!({"executionContextId": 7}),
            "Page.getFrameTree" => json!({"frameTree": {"frame": {"id": "MAIN", "loaderId": "L1"}}}),
            _ => json!({}),
        }))
    });
    (Driver::connect(transport, 640, 480, 2.0).unwrap(), events)
}

fn event(events: &Sender<crate::transport::Incoming>, method: &str, params: Value) {
    events.send(message(json!({"method": method, "params": params, "sessionId": "PAGE"}))).unwrap();
}

#[test]
fn only_plain_http_addresses_without_credentials_are_navigable() {
    for accepted in [
        "about:blank",
        "http://localhost:3000/",
        "HTTPS://Example.test/a b?q=1#frag",
        "http://127.0.0.1",
        "http://[::1]:8080/x",
        "https://example.test/@handle",
        "http://example.test/?next=http://user:pw@other/",
    ] {
        assert_eq!(validate_url(accepted), Ok(()), "{accepted}");
    }
    for refused in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/html,<p>",
        "chrome://version",
        "about:config",
        "view-source:http://a/",
        "ftp://example.test/",
        "http://user@example.test/",
        "http://user:secret@example.test/",
        "https://:secret@example.test",
        "http://example.test:80@evil.test/",
        "http:///user@host/",
        "http://\\user@host/",
        "http://",
        "http:///",
        "http://:8080/",
        "http:example.test",
        "http:/example.test",
        "http:\\\\example.test",
        "//example.test",
        "example.test",
        "http://exa mple.test/",
        " http://example.test/",
    ] {
        let error = validate_url(refused).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Invalid, "{refused}");
        assert!(error.message.contains("HTTP"), "{refused}: {error}");
    }
    for malformed in [String::new(), format!("http://a/{}", "x".repeat(MAX_URL_BYTES)), "http://a/\n".into(), "http://a/\0".into()] {
        assert_eq!(validate_url(&malformed).unwrap_err().message, "URL vacía o demasiado extensa");
    }
    assert!(validate_url(&format!("http://a/{}", "x".repeat(MAX_URL_BYTES - 9))).is_ok(), "exactly at the limit");
}

#[test]
fn a_target_is_exactly_one_way_of_naming_an_element() {
    assert_eq!(Target::from_parts(Some("e12"), None, None, None), Ok(Target::Ref("e12")));
    assert_eq!(Target::from_parts(None, Some("#a"), None, None), Ok(Target::Selector("#a")));
    assert_eq!(
        Target::from_parts(None, None, Some("button"), Some("Guardar")),
        Ok(Target::Role { role: "button", name: "Guardar" })
    );
    for (reference, selector, role, name) in [
        (None, None, None, None),
        (Some("e1"), Some("#a"), None, None),
        (Some("e1"), None, Some("button"), Some("x")),
        (None, Some("#a"), Some("button"), Some("x")),
        (None, None, Some("button"), None),
        (None, None, None, Some("x")),
        (Some(""), None, None, None),
        (None, Some(""), None, None),
        (None, None, Some(""), Some("x")),
        (None, None, Some("button"), Some("")),
        (None, Some("a\nb"), None, None),
    ] {
        let error = Target::from_parts(reference, selector, role, name).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Invalid);
        assert!(error.message.starts_with("Elegí una referencia"));
    }
    let long = "x".repeat(1025);
    assert!(Target::Selector(&long).validate().is_err());
    assert!(Target::Selector(&long[..1024]).validate().is_ok());
    assert!(Target::Role { role: &long[..65], name: "x" }.validate().is_err());
    assert!(Target::Role { role: "button", name: &long }.validate().is_err());
    for valid in ["e1", "e12", "f1e12", "f10e3", "e0"] {
        assert!(valid_ref(valid), "{valid}");
    }
    let oversized = format!("e{}", "1".repeat(32));
    for invalid in ["", "e", "12", "f1", "fe1", "f1e", "e1 ", "E1", "e-1", "e1.5", "f1f2e3", "xe1", "e1; drop", oversized.as_str()] {
        assert!(!valid_ref(invalid), "{invalid}");
    }
}

#[test]
fn the_viewport_stays_within_what_the_panel_can_show() {
    for (width, height) in [(320, 240), (1600, 1200), (900, 700)] {
        assert_eq!(validate_viewport(width, height), Ok(()));
    }
    for (width, height) in [(319, 240), (320, 239), (1601, 1200), (1600, 1201), (0, 0)] {
        assert_eq!(validate_viewport(width, height).unwrap_err().kind, ErrorKind::Invalid);
    }
    let (transport, _written, _incoming) = transport();
    assert_eq!(Driver::connect(transport, 100, 100, 1.0).unwrap_err().kind, ErrorKind::Invalid);
}

#[test]
fn values_selections_and_images_from_the_page_are_bounded_again_here() {
    let exact = bounded_result(&"é".repeat(MAX_EVALUATE_BYTES / 2), false);
    assert_eq!((exact.text.len(), exact.truncated), (MAX_EVALUATE_BYTES, false));
    let cut = bounded_result(&format!("a{}", "é".repeat(MAX_EVALUATE_BYTES)), false);
    assert_eq!((cut.text.len(), cut.truncated), (MAX_EVALUATE_BYTES - 1, true), "never half a character");
    assert!(bounded_result("corto", true).truncated, "the page already cut it");

    assert_eq!(selected(&json!({"text": "hola"})), Ok("hola".to_owned()));
    assert_eq!(selected(&json!({"text": "x".repeat(MAX_SELECTION_BYTES)})).unwrap().len(), MAX_SELECTION_BYTES);
    for refused in [
        json!({"text": "x".repeat(MAX_SELECTION_BYTES + 1)}),
        json!({"text": ""}),
        json!({"text": "a\u{0}b"}),
        json!({"text": 7}),
        json!({}),
        json!(null),
    ] {
        assert_eq!(selected(&refused).unwrap_err().kind, ErrorKind::Failed, "{refused}");
    }
    assert_eq!(selected(&json!({"error": "Seleccioná el texto que querés copiar.", "text": "x"})).unwrap_err().message, "Seleccioná el texto que querés copiar.");
    assert!(selected(&json!({"error": "e".repeat(5000)})).unwrap_err().message.len() <= 512);

    let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut image = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    image.extend_from_slice(&[0, 0, 2, 128, 0, 0, 1, 224]);
    let decoded = png(&encode(&image)).unwrap();
    assert_eq!((decoded.width, decoded.height, decoded.data.len()), (640, 480, image.len()));
    assert!(png("no es base64 !").is_err());
    assert!(png(&encode(b"GIF89a................")).is_err());
    assert!(png(&encode(&[0xff, 0xd8, 0xff, 0xc0, 0, 17, 8, 1, 224, 2, 128])).is_err(), "a JPEG is not a PNG");

    let details = |exception: Value| json!({"text": "Uncaught", "exception": exception});
    assert_eq!(thrown(&details(json!({"type": "object", "subtype": "error", "description": "Error: explota\n    at <anonymous>:1:7"}))), "Error: explota");
    assert_eq!(thrown(&details(json!({"type": "string", "value": "texto"}))), "texto");
    assert_eq!(thrown(&details(json!({"type": "number", "value": 7, "description": "7"}))), "7");
    assert_eq!(thrown(&details(json!({"type": "undefined"}))), "Uncaught");
    assert_eq!(thrown(&json!({})), "Uncaught");
    assert!(thrown(&details(json!({"description": "x".repeat(9000)}))).len() <= 2048);

    let tokens: std::collections::HashSet<String> = (0..64).map(|_| token()).collect();
    assert_eq!(tokens.len(), 64);
    assert!(tokens.iter().all(|token| token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())));
}

#[test]
fn a_request_is_refused_before_the_browser_hears_of_it() {
    let (mut driver, _events) = driver(|method, _| match method {
        // Anything that would act on the page fails the test.
        "Page.navigate" | "Input.dispatchMouseEvent" | "Input.dispatchKeyEvent" | "Input.insertText"
        | "Runtime.evaluate" | "Runtime.callFunctionOn" | "DOM.resolveNode" | "Page.startScreencast" => {
            panic!("{method} reached the browser")
        }
        _ => None,
    });
    let long = "x".repeat(MAX_INPUT_TEXT_BYTES + 1);
    let invalid = [
        driver.navigate("file:///etc/passwd").unwrap_err(),
        driver.resize(10, 10).unwrap_err(),
        driver.click_at(-1.0, 0.0).unwrap_err(),
        driver.click_at(0.0, 1200.5).unwrap_err(),
        driver.click(&Target::Ref("no")).unwrap_err(),
        driver.hover(&Target::Selector("")).unwrap_err(),
        driver.fill(&Target::Selector("#a"), &long).unwrap_err(),
        driver.type_text(&long).unwrap_err(),
        driver.type_text("a\0").unwrap_err(),
        driver.press("").unwrap_err(),
        driver.press("Control+NoSuchKey").unwrap_err(),
        driver.press(&"a".repeat(129)).unwrap_err(),
        driver.scroll(f64::NAN, 0.0).unwrap_err(),
        driver.scroll(0.0, 100_000.5).unwrap_err(),
        driver.wheel(1.0, 1.0, f64::INFINITY, 0.0).unwrap_err(),
        driver.select_options(&Target::Selector("select"), &[]).unwrap_err(),
        driver.select_options(&Target::Selector("select"), &["x"; 33]).unwrap_err(),
        driver.select_options(&Target::Selector("select"), &[&long[..1025]]).unwrap_err(),
        driver.wait_seconds(-0.1).unwrap_err(),
        driver.wait_seconds(10.01).unwrap_err(),
        driver.wait_seconds(f64::NAN).unwrap_err(),
        driver.wait_for_text("").unwrap_err(),
        driver.wait_for_text_gone(&long[..1025]).unwrap_err(),
        driver.evaluate("   ", None).unwrap_err(),
        driver.evaluate(&long, None).unwrap_err(),
        driver.evaluate("1", Some(&Target::Ref("bad ref"))).unwrap_err(),
        driver.handle_dialog(true, Some(&"x".repeat(4097))).unwrap_err(),
        driver.confirm_cut("").unwrap_err(),
        driver.confirm_cut(&"t".repeat(65)).unwrap_err(),
        driver.screenshot(Capture::Element(Target::Selector(""))).unwrap_err(),
        driver.snapshot(&SnapshotOptions { selector: Some(String::new()), ..SnapshotOptions::default() }).unwrap_err(),
        driver.pointer_move(1601.0, 0.0).unwrap_err(),
        driver.pointer_down(0.0, -5.0, crate::MouseButton::Left, 1).unwrap_err(),
        driver.key_down("NoSuchKey").unwrap_err(),
    ];
    for error in &invalid {
        assert_eq!(error.kind, ErrorKind::Invalid, "{error}");
        assert!(!error.message.is_empty());
    }
    // A ref that was never issued is a tool answer, also without touching the page.
    assert_eq!(driver.click(&Target::Ref("e5")).unwrap_err().kind, ErrorKind::Tool);
    assert_eq!(driver.handle_dialog(true, Some("Ada")), Ok(()));
    assert_eq!(driver.wait_seconds(0.0), Ok(()));
}

#[test]
fn what_the_page_reports_about_itself_is_bounded() {
    let (mut driver, events) = driver(|method, _| match method {
        "Page.getNavigationHistory" => Some(Ok(json!({
            "currentIndex": 1,
            "entries": [{"id": 1, "title": "antes"}, {"id": 2, "title": "t".repeat(10_000)}, {"id": 3, "title": "después"}],
        }))),
        _ => None,
    });
    event(&events, "Page.frameNavigated", json!({"frame": {"id": "MAIN", "loaderId": "L1", "url": format!("http://a/{}", "u".repeat(10_000))}}));
    let deadline = Instant::now() + Duration::from_secs(5);
    assert!(driver.shared.wait(deadline, |state| (state.commits == 1).then_some(())).is_some());
    let info = driver.info().unwrap();
    assert_eq!((info.url.len(), info.title.len()), (MAX_URL_BYTES, 2048));
    assert_eq!((info.can_go_back, info.can_go_forward), (true, true));
    assert_eq!((info.width, info.height, info.scale), (640, 480, 2.0));
    // The console and the network are read without asking the browser.
    for number in 0..300 {
        event(&events, "Runtime.consoleAPICalled", json!({"type": "debug", "args": [{"type": "string", "value": format!("d{number}")}]}));
    }
    event(&events, "Runtime.consoleAPICalled", json!({"type": "error", "args": [{"type": "string", "value": "falla"}]}));
    assert!(driver.shared.wait(deadline, |state| (state.console.report(|_| true).omitted == 101).then_some(())).is_some());
    assert_eq!(driver.console_messages(ConsoleLevel::Info).entries.len(), 1);
    assert_eq!(driver.console_messages(ConsoleLevel::Debug).entries.len(), 200);
    assert_eq!(driver.network_requests().entries.len(), 0);
}

#[test]
fn an_oversized_snapshot_evaluation_or_capture_is_cut_or_refused_not_passed_on() {
    let node = |id: usize, parent: Option<usize>, role: &str, name: String| {
        json!({"nodeId": id.to_string(), "parentId": parent.map(|parent| parent.to_string()), "role": {"value": role},
            "name": {"value": name}, "backendDOMNodeId": id, "childIds": if id == 1 { (2..3002).map(|child| child.to_string()).collect::<Vec<_>>() } else { vec![] }})
    };
    let (mut driver, _events) = driver(move |method, params| match method {
        "Accessibility.getFullAXTree" => {
            let mut nodes = vec![node(1, None, "RootWebArea", String::new())];
            nodes.extend((2..3002).map(|id| node(id, Some(1), "button", format!("Botón número {id} con un nombre largo"))));
            Some(Ok(json!({"nodes": nodes})))
        }
        "Runtime.evaluate" => Some(Ok(json!({"result": {"type": "string", "value": "ñ".repeat(40_000)}}))),
        "Page.getLayoutMetrics" => Some(Ok(json!({"cssVisualViewport": {"pageX": 0, "pageY": 0}, "cssContentSize": {"width": 640, "height": 9000}}))),
        "Page.captureScreenshot" => {
            let whole_page = params["captureBeyondViewport"] == true;
            let mut image = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\x02\x80\0\0\x01\xe0".to_vec();
            image.resize(if whole_page { MAX_SCREENSHOT_BYTES + 1 } else { MAX_SCREENSHOT_BYTES }, 0);
            Some(Ok(json!({"data": base64::engine::general_purpose::STANDARD.encode(image)})))
        }
        _ => None,
    });
    let snapshot = driver.snapshot(&SnapshotOptions::default()).unwrap();
    assert!(snapshot.truncated);
    assert!(snapshot.text.len() <= MAX_ARIA_BYTES && snapshot.text.len() > MAX_ARIA_BYTES - 100);
    assert!(snapshot.text.ends_with(']') && snapshot.text.lines().all(|line| line.starts_with("- button \"Botón número ")));
    // Every line that was returned names an element that resolves; the cut ones were never promised.
    let last = snapshot.text.lines().count();
    assert!(driver.refs.resolve(&format!("e{last}")).is_some());

    let value = driver.evaluate("document.body.innerText", None).unwrap();
    assert!(value.truncated && value.text.len() <= MAX_EVALUATE_BYTES && value.text.len() > MAX_EVALUATE_BYTES - 4);
    assert!(value.text.starts_with("\"ññ") && !value.text.contains('\u{fffd}'));

    let viewport = driver.screenshot(Capture::Viewport).unwrap();
    assert_eq!((viewport.data.len(), viewport.width, viewport.height), (MAX_SCREENSHOT_BYTES, 640, 480));
    let refused = driver.screenshot(Capture::FullPage).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Tool);
    assert_eq!(refused.message, "La captura pesa 2049 KiB y supera el límite de 2 MiB; capturá el viewport o un elemento.");
}

#[test]
fn a_ref_is_stale_once_its_frame_holds_another_document() {
    let loader = std::sync::Arc::new(std::sync::Mutex::new("L1"));
    let current = loader.clone();
    let (mut driver, _events) = driver(move |method, _| match method {
        "Accessibility.getFullAXTree" => Some(Ok(json!({"nodes": [
            {"nodeId": "1", "role": {"value": "RootWebArea"}, "childIds": ["2"]},
            {"nodeId": "2", "parentId": "1", "role": {"value": "button"}, "name": {"value": "Guardar"}, "backendDOMNodeId": 10},
        ]}))),
        "Page.getFrameTree" => Some(Ok(json!({"frameTree": {"frame": {"id": "MAIN", "loaderId": *current.lock().unwrap()}}}))),
        "DOM.resolveNode" => Some(Ok(json!({"object": {"objectId": "OBJECT"}}))),
        "Runtime.callFunctionOn" => Some(Ok(json!({"result": {"type": "boolean", "value": true}}))),
        _ => None,
    });
    assert_eq!(driver.snapshot(&SnapshotOptions::default()).unwrap().text, "- button \"Guardar\" [ref=e1]");
    let resolved = driver.run(TIMEOUT, |driver| driver.resolve(&Target::Ref("e1"))).unwrap();
    assert_eq!((resolved.session.as_str(), resolved.frame.as_str(), resolved.backend), ("PAGE", "MAIN", 10));
    // The same frame now holds another document: node 10 there is something else.
    *loader.lock().unwrap() = "L2";
    let stale = driver.run(TIMEOUT, |driver| driver.resolve(&Target::Ref("e1"))).unwrap_err();
    assert_eq!(stale.kind, ErrorKind::Tool);
    assert_eq!(stale.message, "La referencia e1 no corresponde a un elemento de la página actual. Tomá una captura nueva con browser_snapshot y usá sus referencias.");
    // A snapshot of the new document issues refs that the old ones cannot collide with.
    assert_eq!(driver.snapshot(&SnapshotOptions::default()).unwrap().text, "- button \"Guardar\" [ref=f1e1]");
    assert!(driver.run(TIMEOUT, |driver| driver.resolve(&Target::Ref("f1e1"))).is_ok());
    assert_eq!(driver.run(TIMEOUT, |driver| driver.resolve(&Target::Ref("e1"))).unwrap_err().kind, ErrorKind::Tool);
    // By role and name the element is found only when it is the only one.
    assert!(driver.run(TIMEOUT, |driver| driver.resolve(&Target::Role { role: "button", name: " Guardar " })).is_ok());
    let none = driver.run(TIMEOUT, |driver| driver.resolve(&Target::Role { role: "link", name: "Guardar" })).unwrap_err();
    assert!(none.kind == ErrorKind::Tool && none.message.starts_with("El destino coincide con 0 elementos"));
}

#[test]
fn a_request_the_browser_never_answers_times_out_and_the_driver_goes_on() {
    let (mut driver, _events) = driver(|method, _| match method {
        "Runtime.evaluate" => Some(Err("Execution context was destroyed.")),
        _ => None,
    });
    // A protocol error from the page is this request's failure only.
    let error = driver.evaluate("1", None).unwrap_err();
    assert_eq!((error.kind, error.message.as_str()), (ErrorKind::Tool, "Execution context was destroyed."));
    let started = Instant::now();
    let slow = driver.run(Duration::from_millis(60), |driver| driver.call::<Value>("PAGE", "Slow.method", json!({})));
    assert!(slow.is_ok(), "the scripted endpoint answers what it does not know");
    assert!(started.elapsed() < Duration::from_secs(5));
    let (mut driver, _events) = self::driver(|method, _| match method {
        "Page.getNavigationHistory" => Some(Ok(json!({"currentIndex": 0, "entries": [{"id": 1, "title": "x"}]}))),
        _ => None,
    });
    driver.deadline = Instant::now() - Duration::from_secs(1);
    let expired = driver.call::<Value>("PAGE", "Any.method", json!({})).unwrap_err();
    assert_eq!((expired.kind, expired.message.as_str()), (ErrorKind::Failed, "Timeout 10000ms exceeded."));
    // The next request gets a deadline of its own.
    assert_eq!(driver.info().unwrap().title, "x");
}

#[test]
fn a_browser_that_goes_away_is_reported_as_closed_from_then_on() {
    let (mut driver, events) = driver(|method, _| match method {
        // The browser dies instead of answering.
        "Page.getNavigationHistory" => Some(Err(crate::testing::HANG_UP)),
        _ => None,
    });
    // Nothing else keeps the endpoint's side of the transport open.
    drop(events);
    let _ = driver.info();
    let deadline = Instant::now() + Duration::from_secs(5);
    while driver.shared.state().gone.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    for error in [
        driver.navigate("http://localhost/").unwrap_err(),
        driver.snapshot(&SnapshotOptions::default()).unwrap_err(),
        driver.click_at(1.0, 1.0).unwrap_err(),
        driver.info().unwrap_err(),
    ] {
        assert_eq!((error.kind, error.message.as_str()), (ErrorKind::Closed, "El navegador se cerró."));
    }
    // Reading what was already observed still works, and dropping it is quiet.
    assert_eq!(driver.console_messages(ConsoleLevel::Debug).entries.len(), 0);
}

#[test]
fn a_stream_asks_chrome_to_fit_its_frames_only_when_given_a_limit() {
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = asked.clone();
    let (mut driver, _events) = driver(move |method, params| {
        if method == "Page.startScreencast" {
            seen.lock().unwrap().push(params.clone());
        }
        None
    });
    driver.start_stream(FrameFormat::Jpeg { quality: 80 }, None).unwrap();
    driver.start_stream_within(FrameFormat::Jpeg { quality: 200 }, Some((3840, 2160)), None).unwrap();
    assert_eq!(
        *asked.lock().unwrap(),
        [
            json!({"format": "jpeg", "quality": 80, "everyNthFrame": 1}),
            json!({"format": "jpeg", "quality": 100, "everyNthFrame": 1, "maxWidth": 3840, "maxHeight": 2160}),
        ]
    );
    let refused = driver.start_stream_within(FrameFormat::Png, Some((0, 2160)), None).err().map(|error| error.kind);
    assert_eq!(refused, Some(ErrorKind::Invalid));
    assert_eq!(asked.lock().unwrap().len(), 2, "a limit of nothing never reaches the browser");
    // A host names a pointer button the way the protocol does.
    use crate::MouseButton::{Left, Middle, Right};
    assert_eq!(serde_json::to_value([Left, Middle, Right]).unwrap(), json!(["left", "middle", "right"]));
    assert_eq!(serde_json::from_value::<crate::MouseButton>(json!("right")).unwrap(), Right);
    assert_eq!(crate::MouseButton::default(), Left);
}

#[test]
fn a_driver_moves_to_a_worker_thread_and_its_stream_is_shared_with_the_view() {
    fn sendable<T: Send>() {}
    fn shareable<T: Send + Sync + Clone>() {}
    sendable::<Driver>();
    shareable::<crate::FrameStream>();
    shareable::<Client>();
}
