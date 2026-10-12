//! Loopback only: the listener and the closed port are this machine's.
use super::*;
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn the_address_of_a_dev_server_is_found_in_what_it_prints() {
    for (output, expected) in [
        // Vite, Next.js, Astro.
        ("  VITE v7.1.2  ready in 312 ms\n\n  ➜  Local:   http://localhost:5173/\n  ➜  Network: use --host to expose\n", "http://localhost:5173/"),
        ("   ▲ Next.js 15.4.1\n   - Local:        http://localhost:3000\n   - Network:      http://192.168.1.10:3000\n", "http://localhost:3000/"),
        ("┃ Local    http://localhost:4321/\n", "http://localhost:4321/"),
        // Where a server listens becomes where its page is.
        ("Serving HTTP on 0.0.0.0 port 8000 (http://0.0.0.0:8000/) ...\n", "http://localhost:8000/"),
        ("Network: http://192.168.1.10:5173/\nLocal: http://0.0.0.0:5173/app\n", "http://localhost:5173/app"),
        (" * Running on http://127.0.0.1:5000\n", "http://127.0.0.1:5000/"),
        ("Listening on http://[::1]:4000/admin?tab=1 (Ctrl+C to quit)\n", "http://[::1]:4000/admin?tab=1"),
        ("Server ready at https://localhost:8443.\n", "https://localhost:8443/"),
        ("Abrí \"http://localhost:8080/\", o <http://localhost:9000/>\n", "http://localhost:8080/"),
        ("see https://vite.dev/config/ then http://localhost:5173/\n", "http://localhost:5173/"),
    ] {
        assert_eq!(first_loopback_url(output).as_deref(), Some(expected), "{output:?}");
    }
    for output in [
        "",
        "ready in 312 ms\n",
        // Another machine, a look-alike name, credentials, another scheme.
        "Network: http://192.168.1.10:5173/\n",
        "docs at https://example.test/guide\n",
        "http://localhost.evil.test:5173/\n",
        "http://127.0.0.1.evil.test/\n",
        "http://usuario:clave@localhost:3000/\n",
        "ws://localhost:5173/ ftp://localhost/ file:///tmp/x\n",
        "http://[::]:8000/\n",
        // Still arriving: the port may be the next thing printed.
        "  ➜  Local:   http://localhost",
        "  ➜  Local:   http://localhost:51",
    ] {
        assert_eq!(first_loopback_url(output), None, "{output:?}");
    }
}

#[test]
fn only_addresses_of_this_machine_are_loopback_urls() {
    for (url, expected) in [
        ("http://localhost:5173", Some("http://localhost:5173/")),
        ("  http://LOCALHOST:5173/a  ", Some("http://localhost:5173/a")),
        ("http://127.0.0.1:1/", Some("http://127.0.0.1:1/")),
        ("http://127.8.9.10/", Some("http://127.8.9.10/")),
        ("http://0.0.0.0:3000/x", Some("http://localhost:3000/x")),
        ("https://[::1]/", Some("https://[::1]/")),
        ("http://example.test/", None),
        ("http://10.0.0.1/", None),
        ("http://[::ffff:127.0.0.1]/", None),
        ("http://app.localhost:3000/", None),
        ("http://localhost.:3000/", None),
        ("http://a@localhost/", None),
        ("localhost:3000", None),
        ("about:blank", None),
        ("javascript:alert(1)", None),
    ] {
        assert_eq!(loopback_url(url).as_deref(), expected, "{url}");
    }
}

#[test]
fn the_check_is_a_connection_to_a_listener_and_gives_up_on_a_closed_port() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let open = format!("http://127.0.0.1:{port}/");
    assert!(answers(&open, 1, Duration::ZERO));
    // `localhost` is tried on both families, whichever the server chose.
    assert!(answers(&format!("http://localhost:{port}/"), 1, Duration::ZERO));
    // Nothing was sent: the listener sees a connection that says nothing.
    let (mut accepted, _) = listener.accept().unwrap();
    accepted.set_nonblocking(true).unwrap();
    let mut scratch = [0u8; 16];
    assert!(!matches!(std::io::Read::read(&mut accepted, &mut scratch), Ok(read) if read > 0));
    drop((accepted, listener));
    let started = Instant::now();
    assert!(!answers(&open, 3, Duration::from_millis(10)));
    assert!(started.elapsed() < Duration::from_secs(3), "bounded retries");
    // Never asked of another machine, however it is written.
    assert!(!answers("http://192.0.2.1:80/", 1, Duration::ZERO));
    assert!(started.elapsed() < Duration::from_secs(3));
}

fn watch() -> (OutputWatch, Arc<Mutex<Option<String>>>, Arc<AtomicUsize>) {
    let found = Arc::new(Mutex::new(None));
    let woken = Arc::new(AtomicUsize::new(0));
    let count = woken.clone();
    let watch = OutputWatch::new(
        found.clone(),
        Arc::new(move || {
            count.fetch_add(1, Ordering::AcqRel);
        }),
    );
    (watch, found, woken)
}

fn wait_for(found: &Mutex<Option<String>>) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while found.lock().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    found.lock().unwrap().clone()
}

#[test]
fn a_watch_publishes_a_coloured_split_address_once_it_answers() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (mut watch, found, woken) = watch();
    // As Vite prints it: colour around the address and bold around the port,
    // here arriving in three reads.
    assert!(watch.feed(b"\x1b[32m  VITE\x1b[39m ready\r\n  \x1b[36mhttp://127.0."));
    assert!(watch.feed(format!("0.1:\x1b[1m{port}").as_bytes()));
    assert!(found.lock().unwrap().is_none());
    assert!(!watch.feed(b"\x1b[22m/\x1b[39m\r\n"), "nothing left to look for");
    assert_eq!(wait_for(&found), Some(format!("http://127.0.0.1:{port}/")));
    assert_eq!(woken.load(Ordering::Acquire), 1);
}

#[test]
fn a_watch_looks_only_at_the_start_of_the_output_and_never_reports_a_dead_address() {
    // A printed address nobody listens on is never published.
    let closed = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    };
    let (mut watch, found, woken) = watch();
    assert!(!watch.feed(format!("Local: http://127.0.0.1:{closed}/\n").as_bytes()));
    // Past the first few KiB an address is no longer a server announcing itself.
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (mut late, late_found, _) = self::watch();
    assert!(late.feed(&vec![b'.'; WATCH_BYTES - 1]));
    assert!(!late.feed(format!("\nLocal: http://127.0.0.1:{port}/\n").as_bytes()));
    assert!(!late.feed(format!("\nLocal: http://127.0.0.1:{port}/\n").as_bytes()));
    std::thread::sleep(CHECK_PAUSE * 2);
    assert!(late_found.lock().unwrap().is_none());
    // The dead one's bounded retries end without publishing anything.
    std::thread::sleep(CHECK_PAUSE * (CHECK_ATTEMPTS + 2));
    assert!(found.lock().unwrap().is_none());
    assert_eq!(woken.load(Ordering::Acquire), 0);
}

#[test]
fn the_setting_defaults_to_automatic_and_names_its_three_choices() {
    assert_eq!(PreviewOpening::default(), PreviewOpening::Auto);
    let options = PreviewOpening::Ask.options();
    assert_eq!(
        options.iter().map(|option| option.label.as_str()).collect::<Vec<_>>(),
        ["Automáticamente", "Preguntar", "Nunca"]
    );
    for choice in PreviewOpening::ALL {
        let action = format!("{SETTING_ACTION_PREFIX}{}", choice.id());
        assert_eq!(PreviewOpening::from_action_id(&action), Some(choice));
        let saved = serde_json::to_value(choice).unwrap();
        assert_eq!(serde_json::from_value::<PreviewOpening>(saved).unwrap(), choice);
    }
    assert_eq!(PreviewOpening::from_action_id("preview_opening:siempre"), None);
    assert_eq!(serde_json::to_value(PreviewOpening::Ask).unwrap(), "ask");
}
