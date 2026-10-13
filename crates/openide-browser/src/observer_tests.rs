use super::*;
use crate::testing::client;
use serde_json::json;
use std::sync::mpsc::Receiver;

/// An observer already told which page is its own.
fn observer() -> (Observer, Client, Receiver<Value>) {
    let (client, written, source) = client();
    // The transport stays open for the whole test.
    std::mem::forget(source);
    let shared = Shared::new();
    {
        let mut state = shared.state();
        state.page_session = "PAGE".into();
        state.main_frame = "MAIN".into();
    }
    (Observer { shared }, client, written)
}

fn posted(written: &Receiver<Value>) -> Vec<(String, String, Value)> {
    written
        .try_iter()
        .map(|message| {
            (
                message["sessionId"].as_str().unwrap_or("").to_owned(),
                message["method"].as_str().unwrap().to_owned(),
                message["params"].clone(),
            )
        })
        .collect()
}

fn log(observer: &mut Observer, client: &Client, text: &str) {
    observer.event(
        client,
        "PAGE",
        "Runtime.consoleAPICalled",
        json!({"type": "log", "args": [{"type": "string", "value": text}]}),
    );
}

fn request(observer: &mut Observer, client: &Client, id: &str, url: &str, kind: &str) {
    observer.event(
        client,
        "PAGE",
        "Network.requestWillBeSent",
        json!({"requestId": id, "loaderId": "L", "frameId": "MAIN", "type": kind, "request": {"url": url, "method": "GET"}}),
    );
}

#[test]
fn the_console_keeps_the_newest_two_hundred_entries_and_counts_the_rest() {
    let (mut observer, client, _written) = observer();
    for number in 0..MAX_EVENTS + 50 {
        log(&mut observer, &client, &format!("línea {number}"));
    }
    let state = observer.shared.state();
    let report = state.console.report(|_| true);
    assert_eq!(report.omitted, 50);
    assert_eq!(report.entries.len(), MAX_EVENTS);
    assert_eq!(report.entries[0].text, "línea 50");
    assert_eq!(report.entries.last().unwrap().text, format!("línea {}", MAX_EVENTS + 49));
}

#[test]
fn console_text_is_cut_at_one_kibibyte_without_splitting_a_character() {
    let (mut observer, client, _written) = observer();
    log(&mut observer, &client, &"ñ".repeat(4000));
    log(&mut observer, &client, &format!("a{}", "🧠".repeat(1000)));
    observer.event(
        &client,
        "PAGE",
        "Runtime.consoleAPICalled",
        json!({"type": "log", "args": (0..600).map(|_| json!({"type": "string", "value": "palabra"})).collect::<Vec<_>>()}),
    );
    let state = observer.shared.state();
    let entries = state.console.report(|_| true).entries;
    assert_eq!(entries[0].text.len(), MAX_CONSOLE_TEXT_BYTES);
    assert!(entries[0].text.chars().all(|character| character == 'ñ'));
    // 1 + 4n bytes: the last whole character ends one byte short of the bound.
    assert_eq!(entries[1].text.len(), MAX_CONSOLE_TEXT_BYTES - 3);
    assert!(entries[1].text.ends_with('🧠'));
    assert!(entries[2].text.len() <= MAX_CONSOLE_TEXT_BYTES, "many arguments are bounded together");
}

#[test]
fn a_report_returns_the_newest_entries_that_fit_twelve_kibibytes() {
    let (mut observer, client, _written) = observer();
    for number in 0..150 {
        log(&mut observer, &client, &format!("{number:03} {}", "y".repeat(200)));
    }
    let state = observer.shared.state();
    let report = state.console.report(|_| true);
    let bytes = serde_json::to_vec(&report.entries).unwrap().len();
    assert!(bytes <= MAX_REPORT_BYTES + 2 && bytes > MAX_REPORT_BYTES - 300, "{bytes}");
    assert_eq!(report.omitted + report.entries.len(), 150);
    assert!(report.entries.last().unwrap().text.starts_with("149 "), "the newest is always kept");
    let first: usize = report.entries[0].text[..3].parse().unwrap();
    assert_eq!(first, report.omitted, "what is left out is the oldest");
    // A filter is applied before the budget, and what it removes is not "omitted".
    let odd = state.console.report(|entry| entry.text.as_bytes()[2] % 2 == 1);
    assert!(odd.entries.iter().all(|entry| entry.text.as_bytes()[2] % 2 == 1));
    assert_eq!(odd.omitted + odd.entries.len(), 75);
    // One entry larger than the whole budget is still not returned in part.
    let empty = Log::<ConsoleEntry>::default().report(|_| true);
    assert_eq!((empty.omitted, empty.entries.len()), (0, 0));
}

#[test]
fn console_levels_types_locations_and_values_follow_the_console() {
    let (mut observer, client, _written) = observer();
    let frame = json!({"callFrames": [{"url": "http://fixture/app.js", "lineNumber": 19, "columnNumber": 18}]});
    for (kind, level) in [
        ("log", ConsoleLevel::Info),
        ("info", ConsoleLevel::Info),
        ("table", ConsoleLevel::Info),
        ("debug", ConsoleLevel::Debug),
        ("warning", ConsoleLevel::Warning),
        ("error", ConsoleLevel::Error),
        ("assert", ConsoleLevel::Error),
    ] {
        observer.event(
            &client,
            "PAGE",
            "Runtime.consoleAPICalled",
            json!({"type": kind, "args": [{"type": "string", "value": "x"}], "stackTrace": frame}),
        );
        let entry = observer.shared.state().console.report(|_| true).entries.pop().unwrap();
        assert_eq!((entry.level, entry.kind.as_str()), (level, kind));
        assert_eq!(entry.location.as_deref(), Some("http://fixture/app.js:20:19"), "counted from one");
    }
    let arguments = json!([
        {"type": "string", "value": "carga"},
        {"type": "number", "value": 1.5},
        {"type": "boolean", "value": true},
        {"type": "undefined"},
        {"type": "object", "subtype": "null", "value": null},
        {"type": "bigint", "unserializableValue": "10n", "description": "10n"},
        {"type": "object", "description": "Object", "preview": {"overflow": false, "properties": [
            {"name": "n", "type": "number", "value": "1"}, {"name": "s", "type": "string", "value": "dos"}]}},
        {"type": "object", "subtype": "array", "description": "Array(2)", "preview": {"overflow": true, "properties": [
            {"name": "0", "type": "number", "value": "1"}, {"name": "1", "type": "string", "value": "b"}]}},
        {"type": "object", "subtype": "error", "description": "Error: falla\n    at http://fixture/a.js:1:1", "preview": {"properties": []}},
        {"type": "function", "description": "() => 1"},
    ]);
    observer.event(&client, "PAGE", "Runtime.consoleAPICalled", json!({"type": "log", "args": arguments}));
    let entry = observer.shared.state().console.report(|_| true).entries.pop().unwrap();
    assert_eq!(
        entry.text,
        "carga 1.5 true undefined null 10n {n: 1, s: \"dos\"} [1, \"b\", …] Error: falla\n    at http://fixture/a.js:1:1 () => 1"
    );
    assert_eq!(entry.location, None);

    // Uncaught errors and the messages Chrome itself logs for the page.
    observer.event(
        &client,
        "PAGE",
        "Runtime.exceptionThrown",
        json!({"exceptionDetails": {"text": "Uncaught", "exception": {"type": "object", "subtype": "error", "description": "Error: sin capturar\n    at http://fixture/:3:9"}}}),
    );
    observer.event(
        &client,
        "PAGE",
        "Runtime.exceptionThrown",
        json!({"exceptionDetails": {"text": "Uncaught", "exception": {"type": "string", "value": "texto suelto"}}}),
    );
    observer.event(
        &client,
        "PAGE",
        "Log.entryAdded",
        json!({"entry": {"level": "error", "source": "network", "text": "Failed to load resource: 404", "url": "http://fixture/missing.js"}}),
    );
    observer.event(&client, "PAGE", "Log.entryAdded", json!({"entry": {"level": "verbose", "text": "[DOM] aviso"}}));
    let state = observer.shared.state();
    let entries = state.console.report(|_| true).entries;
    let last: Vec<_> = entries[entries.len() - 4..]
        .iter()
        .map(|entry| (entry.level, entry.kind.as_str(), entry.text.as_str(), entry.location.as_deref()))
        .collect();
    assert_eq!(
        last,
        [
            (ConsoleLevel::Error, "pageerror", "Error: sin capturar\n    at http://fixture/:3:9", None),
            (ConsoleLevel::Error, "pageerror", "Uncaught texto suelto", None),
            (ConsoleLevel::Error, "error", "Failed to load resource: 404", Some("http://fixture/missing.js:1:1")),
            (ConsoleLevel::Info, "verbose", "[DOM] aviso", None),
        ]
    );
    // The level filter keeps a level and everything more severe.
    let errors = state.console.report(|entry| entry.level <= ConsoleLevel::Error);
    assert!(errors.entries.iter().all(|entry| entry.level == ConsoleLevel::Error) && errors.entries.len() == 5);
    assert!(ConsoleLevel::Error < ConsoleLevel::Warning && ConsoleLevel::Info < ConsoleLevel::Debug);
}

#[test]
fn requests_carry_method_url_status_type_and_failure_and_nothing_else() {
    let (mut observer, client, _written) = observer();
    request(&mut observer, &client, "L", "http://fixture/tools", "Document");
    observer.event(
        &client,
        "PAGE",
        "Network.requestWillBeSent",
        json!({"requestId": "2", "loaderId": "L", "frameId": "MAIN", "type": "Fetch",
            "request": {"url": format!("http://fixture/api?q={}", "z".repeat(2000)), "urlFragment": "#f", "method": "POST",
                "headers": {"X-Secret": "cabecera secreta"}, "postData": "cuerpo secreto"}}),
    );
    request(&mut observer, &client, "3", "http://127.0.0.1:1/caido", "Fetch");
    request(&mut observer, &client, "4", "data:image/png;base64,AAAA", "Image");
    let response = |id: &str, status: u64| json!({"requestId": id, "response": {"status": status, "headers": {"Set-Cookie": "secreta"}}});
    observer.event(&client, "PAGE", "Network.responseReceived", response("L", 200));
    observer.event(&client, "PAGE", "Network.responseReceived", response("2", 404));
    observer.event(&client, "PAGE", "Network.responseReceived", response("desconocido", 500));
    observer.event(
        &client,
        "PAGE",
        "Network.loadingFailed",
        json!({"requestId": "3", "errorText": format!("net::ERR_CONNECTION_REFUSED{}", "!".repeat(600))}),
    );
    let state = observer.shared.state();
    let report = state.network.report(|_| true);
    assert_eq!(report.entries.len(), 3, "an inline data: resource is not a request");
    assert_eq!(
        serde_json::to_value(&report.entries[0]).unwrap(),
        json!({"method": "GET", "url": "http://fixture/tools", "resource_type": "document", "status": 200})
    );
    assert_eq!(report.entries[1].url.len(), MAX_REQUEST_URL_BYTES);
    assert_eq!((report.entries[1].method.as_str(), report.entries[1].status), ("POST", Some(404)));
    assert_eq!(report.entries[1].resource_type, "fetch");
    assert_eq!(report.entries[2].status, None);
    assert_eq!(report.entries[2].failure.as_ref().unwrap().len(), 256, "a failure text is bounded too");
    let serialised = serde_json::to_string(&report).unwrap();
    for secret in ["secreta", "secreto", "X-Secret", "request_id", "Set-Cookie"] {
        assert!(!serialised.contains(secret), "{secret}");
    }
}

#[test]
fn a_redirect_is_the_same_request_and_a_new_main_document_starts_both_logs_over() {
    let (mut observer, client, _written) = observer();
    observer.shared.state().dialog_plan = Some(DialogPlan {
        accept: true,
        prompt_text: None,
    });
    log(&mut observer, &client, "de la página anterior");
    request(&mut observer, &client, "viejo", "http://fixture/old.js", "Script");
    // A navigation request of the main frame: its id is the new loader's.
    request(&mut observer, &client, "L", "http://fixture/redirect", "Document");
    {
        let state = observer.shared.state();
        assert_eq!(state.console.report(|_| true).entries.len(), 0);
        assert_eq!(state.network.report(|_| true).entries.len(), 1);
        assert_eq!(state.dialog_plan, None, "an answer armed for the previous page is forgotten");
    }
    log(&mut observer, &client, "de la nueva");
    // The redirect hop: same request id, the response of the hop before.
    observer.event(
        &client,
        "PAGE",
        "Network.requestWillBeSent",
        json!({"requestId": "L", "loaderId": "L", "frameId": "MAIN", "type": "Document",
            "redirectResponse": {"status": 302}, "request": {"url": "http://fixture/tools", "method": "GET"}}),
    );
    observer.event(&client, "PAGE", "Network.responseReceived", json!({"requestId": "L", "response": {"status": 200}}));
    // A document of a child frame, and one whose id is not the loader, reset nothing.
    observer.event(
        &client,
        "PAGE",
        "Network.requestWillBeSent",
        json!({"requestId": "F", "loaderId": "F", "frameId": "CHILD", "type": "Document", "request": {"url": "http://fixture/frame", "method": "GET"}}),
    );
    observer.event(
        &client,
        "PAGE",
        "Network.requestWillBeSent",
        json!({"requestId": "9", "loaderId": "L", "frameId": "MAIN", "type": "Document", "request": {"url": "http://fixture/x", "method": "GET"}}),
    );
    let state = observer.shared.state();
    let pairs: Vec<_> = state.network.report(|_| true).entries.iter().map(|entry| (entry.url.clone(), entry.status)).collect();
    assert_eq!(
        pairs,
        [
            ("http://fixture/redirect".to_owned(), Some(302)),
            ("http://fixture/tools".to_owned(), Some(200)),
            ("http://fixture/frame".to_owned(), None),
            ("http://fixture/x".to_owned(), None),
        ]
    );
    assert_eq!(state.console.report(|_| true).entries[0].text, "de la nueva");
}

#[test]
fn the_request_log_is_bounded_and_late_news_of_a_dropped_request_is_ignored() {
    let (mut observer, client, _written) = observer();
    for number in 0..MAX_EVENTS + 60 {
        request(&mut observer, &client, &number.to_string(), &format!("http://fixture/api?n={number}"), "Fetch");
    }
    observer.event(&client, "PAGE", "Network.responseReceived", json!({"requestId": "0", "response": {"status": 200}}));
    observer.event(&client, "PAGE", "Network.responseReceived", json!({"requestId": "259", "response": {"status": 201}}));
    let state = observer.shared.state();
    let report = state.network.report(|_| true);
    assert!(report.omitted >= 60 && report.omitted + report.entries.len() == MAX_EVENTS + 60);
    assert!(serde_json::to_vec(&report.entries).unwrap().len() <= MAX_REPORT_BYTES + 2);
    assert_eq!(report.entries.last().unwrap().status, Some(201));
    assert!(state.network.report(|entry| entry.status == Some(200)).entries.is_empty());
}

#[test]
fn a_dialog_is_dismissed_and_recorded_unless_an_answer_was_armed_for_it() {
    let (mut observer, client, written) = observer();
    let open = |observer: &mut Observer, session: &str, kind: &str, message: &str| {
        observer.event(&client, session, "Page.javascriptDialogOpening", json!({"type": kind, "message": message}));
    };
    open(&mut observer, "PAGE", "confirm", "¿Borrar todo?");
    assert_eq!(posted(&written), [("PAGE".to_owned(), "Page.handleJavaScriptDialog".to_owned(), json!({"accept": false}))]);

    observer.shared.state().dialog_plan = Some(DialogPlan {
        accept: true,
        prompt_text: Some("Ada 🧠".into()),
    });
    // The dialog of a frame in another process is answered on its own session.
    open(&mut observer, "CHILD", "prompt", &"¿Nombre?".repeat(400));
    assert_eq!(
        posted(&written),
        [("CHILD".to_owned(), "Page.handleJavaScriptDialog".to_owned(), json!({"accept": true, "promptText": "Ada 🧠"}))]
    );
    open(&mut observer, "PAGE", "alert", "otra");
    assert_eq!(posted(&written)[0].2, json!({"accept": false}), "the answer was used once");

    // Text for a prompt is only sent when accepting.
    observer.shared.state().dialog_plan = Some(DialogPlan {
        accept: false,
        prompt_text: Some("no".into()),
    });
    open(&mut observer, "PAGE", "prompt", "¿?");
    assert_eq!(posted(&written)[0].2, json!({"accept": false}));

    let state = observer.shared.state();
    let entries = state.console.report(|entry| entry.level <= ConsoleLevel::Warning).entries;
    assert_eq!(
        serde_json::to_value(&entries[0]).unwrap(),
        json!({"level": "warning", "type": "dialog", "dialog": "confirm", "action": "dismissed", "text": "¿Borrar todo?"})
    );
    assert_eq!((entries[1].action, entries[1].dialog.as_deref()), (Some("accepted"), Some("prompt")));
    assert!(entries[1].text.len() <= MAX_CONSOLE_TEXT_BYTES && entries[1].text.is_char_boundary(entries[1].text.len()));
    assert_eq!(entries.len(), 4);
}

#[test]
fn the_main_frame_reports_where_it_is_and_which_document_it_holds() {
    let (mut observer, client, _written) = observer();
    let navigated = |observer: &mut Observer, session: &str, frame: Value| {
        observer.event(&client, session, "Page.frameNavigated", json!({"frame": frame}));
    };
    navigated(&mut observer, "PAGE", json!({"id": "MAIN", "loaderId": "L1", "url": "http://fixture/a", "urlFragment": "#top"}));
    // A child frame, and the root of a frame in another process, are not the page.
    navigated(&mut observer, "PAGE", json!({"id": "CHILD", "parentId": "MAIN", "loaderId": "L9", "url": "http://fixture/frame"}));
    navigated(&mut observer, "OTHER", json!({"id": "REMOTE", "loaderId": "L8", "url": "http://other/"}));
    observer.event(&client, "PAGE", "Page.lifecycleEvent", json!({"frameId": "CHILD", "loaderId": "L9", "name": "DOMContentLoaded"}));
    observer.event(&client, "PAGE", "Page.lifecycleEvent", json!({"frameId": "MAIN", "loaderId": "L1", "name": "load"}));
    {
        let state = observer.shared.state();
        assert_eq!((state.url.as_str(), state.loader.as_str(), state.commits), ("http://fixture/a#top", "L1", 1));
        assert_eq!(state.dom_content_loader, "", "only the main frame's own event counts");
    }
    observer.event(&client, "PAGE", "Page.lifecycleEvent", json!({"frameId": "MAIN", "loaderId": "L1", "name": "DOMContentLoaded"}));
    observer.event(&client, "PAGE", "Page.navigatedWithinDocument", json!({"frameId": "MAIN", "url": "http://fixture/a#next"}));
    observer.event(&client, "PAGE", "Page.frameRequestedNavigation", json!({"frameId": "MAIN", "url": "http://fixture/b"}));
    observer.event(&client, "PAGE", "Page.frameRequestedNavigation", json!({"frameId": "CHILD", "url": "http://fixture/c"}));
    observer.event(&client, "PAGE", "Page.frameStoppedLoading", json!({"frameId": "MAIN"}));
    navigated(&mut observer, "PAGE", json!({"id": "MAIN", "loaderId": "L2", "url": "chrome-error://chromewebdata/", "unreachableUrl": "http://fixture/b"}));
    let state = observer.shared.state();
    assert_eq!((state.url.as_str(), state.loader.as_str(), state.commits), ("chrome-error://chromewebdata/", "L2", 3));
    assert_eq!((state.dom_content_loader.as_str(), state.requested, state.stopped), ("L1", 1, 1));
    assert_eq!(state.unreachable.as_deref(), Some("http://fixture/b"));
}

#[test]
fn what_a_click_asked_to_see_in_another_tab_opens_in_the_page_itself() {
    let (mut observer, client, written) = observer();
    let here = |url: &str| ("PAGE".to_owned(), "Page.navigate".to_owned(), json!({"url": url}));
    let opened = |observer: &mut Observer, session: &str, url: &str, gesture: bool| {
        observer.event(&client, session, "Page.windowOpen", json!({"url": url, "windowName": "_blank", "windowFeatures": [], "userGesture": gesture}));
    };
    // A link for another tab or a script's window.open, behind a click: the
    // page navigates there, and whoever clicked waits for that document.
    opened(&mut observer, "PAGE", "https://fixture/next?a=1#top", true);
    assert_eq!(posted(&written), [here("https://fixture/next?a=1#top")]);
    assert_eq!(observer.shared.state().requested, 1);
    // From a frame of another site, the page is still the one that opens it.
    opened(&mut observer, "CHILD", "http://other/", true);
    assert_eq!(posted(&written), [here("http://other/")]);
    // With no click behind it, a popup opens nothing.
    opened(&mut observer, "PAGE", "https://fixture/advert", false);
    // Nor does anything a link could not make the browser itself load.
    for url in ["", "about:blank", "file:///etc/passwd", "javascript:alert(1)", "data:text/html,x", "chrome://settings", "HTTPS://fixture/"] {
        opened(&mut observer, "PAGE", url, true);
    }
    assert!(posted(&written).is_empty());
    assert_eq!(observer.shared.state().requested, 2);
}

#[test]
fn frames_of_other_processes_are_set_up_and_everything_else_attached_is_let_go() {
    let (mut observer, client, written) = observer();
    let attached = |observer: &mut Observer, on: &str, session: &str, target: &str, kind: &str| {
        observer.event(
            &client,
            on,
            "Target.attachedToTarget",
            json!({"sessionId": session, "targetInfo": {"targetId": target, "type": kind}, "waitingForDebugger": true}),
        );
    };
    attached(&mut observer, "PAGE", "S1", "FRAME1", "iframe");
    let setup = posted(&written);
    let methods: Vec<&str> = setup.iter().map(|(_, method, _)| method.as_str()).collect();
    assert!(setup.iter().all(|(session, _, _)| session == "S1"));
    assert_eq!(methods.last(), Some(&"Runtime.runIfWaitingForDebugger"), "it is resumed only after it is observed");
    for needed in ["Page.enable", "Runtime.enable", "Network.enable", "Log.enable", "Target.setAutoAttach"] {
        assert!(methods.contains(&needed), "{needed}");
    }
    // A frame nested in that one is announced on its session.
    attached(&mut observer, "S1", "S2", "FRAME2", "iframe");
    posted(&written);
    attached(&mut observer, "PAGE", "S3", "W", "worker");
    assert_eq!(
        posted(&written),
        [
            ("S3".to_owned(), "Runtime.runIfWaitingForDebugger".to_owned(), json!({})),
            ("PAGE".to_owned(), "Target.detachFromTarget".to_owned(), json!({"sessionId": "S3"})),
        ]
    );
    // The driver's own attachment to its page is announced on the browser session.
    attached(&mut observer, "", "PAGE", "MAIN", "page");
    assert!(posted(&written).is_empty(), "it is never detached from its own page");
    {
        let state = observer.shared.state();
        assert_eq!(state.children.len(), 2);
        assert_eq!(state.children["FRAME2"], ChildSession { session: "S2".into(), parent: "S1".into() });
    }

    let context = |observer: &mut Observer, session: &str, id: i64, frame: &str, default: bool| {
        observer.event(
            &client,
            session,
            "Runtime.executionContextCreated",
            json!({"context": {"id": id, "auxData": {"isDefault": default, "frameId": frame}}}),
        );
    };
    context(&mut observer, "PAGE", 1, "MAIN", true);
    context(&mut observer, "PAGE", 2, "MAIN", false);
    context(&mut observer, "S1", 1, "FRAME1", true);
    context(&mut observer, "PAGE", 3, "CHILD", true);
    observer.event(&client, "PAGE", "Runtime.executionContextDestroyed", json!({"executionContextId": 3}));
    {
        let state = observer.shared.state();
        assert_eq!(state.contexts.len(), 2, "only each frame's own world, and not the destroyed one");
        assert_eq!(state.contexts[&("PAGE".to_owned(), "MAIN".to_owned())], 1);
        assert_eq!(state.contexts[&("S1".to_owned(), "FRAME1".to_owned())], 1);
    }
    observer.event(&client, "PAGE", "Target.detachedFromTarget", json!({"sessionId": "S1"}));
    {
        let state = observer.shared.state();
        assert_eq!(state.children.len(), 1);
        assert!(!state.contexts.contains_key(&("S1".to_owned(), "FRAME1".to_owned())));
        assert_eq!(state.gone, None);
    }
    observer.event(&client, "PAGE", "Runtime.executionContextsCleared", json!({}));
    assert!(observer.shared.state().contexts.is_empty());

    // More frames than are tracked: the page keeps running, the extra ones are not addressable.
    for number in 0..MAX_FRAMES + 10 {
        attached(&mut observer, "PAGE", &format!("X{number}"), &format!("T{number}"), "iframe");
        context(&mut observer, &format!("X{number}"), 1, &format!("T{number}"), true);
    }
    let state = observer.shared.state();
    assert_eq!(state.children.len(), MAX_FRAMES);
    assert_eq!(state.contexts.len(), MAX_FRAMES);
    let resumed = posted(&written).iter().filter(|(_, method, _)| method == "Runtime.runIfWaitingForDebugger").count();
    assert_eq!(resumed, MAX_FRAMES + 10, "a frame beyond the limit is still resumed");
}

#[test]
fn a_page_that_crashes_or_goes_away_is_reported_and_wakes_who_waits() {
    for (session, method, params) in [
        ("PAGE", "Inspector.targetCrashed", json!({})),
        ("", "Target.detachedFromTarget", json!({"sessionId": "PAGE"})),
    ] {
        let (mut observer, client, _written) = observer();
        observer.event(&client, "OTHER", "Inspector.targetCrashed", json!({}));
        assert_eq!(observer.shared.state().gone, None, "a crashed frame of another process is not the page");
        let shared = observer.shared.clone();
        let waiting = std::thread::spawn(move || {
            let started = Instant::now();
            let found = shared.wait(Instant::now() + Duration::from_secs(20), |state| (state.commits > 0).then_some(()));
            (found, started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(30));
        observer.event(&client, session, method, params);
        let (found, waited) = waiting.join().unwrap();
        assert_eq!(found, None);
        assert!(waited < Duration::from_secs(5), "{waited:?}");
        assert!(observer.shared.state().gone.is_some());
    }
    // The end of the transport says so too, and does not replace an earlier reason.
    let (mut observer, _client, _written) = observer();
    observer.closed();
    assert_eq!(observer.shared.state().gone.as_deref(), Some("El navegador se cerró."));
}

#[test]
fn waiting_returns_what_the_condition_yields_or_nothing_at_the_deadline() {
    let (mut observer, client, _written) = observer();
    let shared = observer.shared.clone();
    let waiting = std::thread::spawn(move || shared.wait(Instant::now() + Duration::from_secs(20), |state| state.drag.take()));
    std::thread::sleep(Duration::from_millis(30));
    observer.event(&client, "PAGE", "Input.dragIntercepted", json!({"data": {"items": [1]}}));
    assert_eq!(waiting.join().unwrap(), Some(json!({"items": [1]})));
    let started = Instant::now();
    let nothing: Option<()> = observer.shared.wait(Instant::now() + Duration::from_millis(40), |_| None);
    assert_eq!(nothing, None);
    assert!(started.elapsed() >= Duration::from_millis(40));
    // A deadline already passed still checks once.
    assert_eq!(observer.shared.wait(Instant::now(), |_| Some(7)), Some(7));
}

#[test]
fn text_is_limited_on_a_character_boundary() {
    assert_eq!(limited("hola", 10), "hola");
    assert_eq!(limited("hola", 2), "ho");
    assert_eq!(limited("añ", 2), "a");
    assert_eq!(limited("🧠🧠", 7), "🧠");
    assert_eq!(limited("🧠", 3), "");
    assert_eq!(limited("", 0), "");
}
