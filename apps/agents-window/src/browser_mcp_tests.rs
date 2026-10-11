use super::*;
use std::io::{BufRead, BufReader, PipeReader, PipeWriter};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

const SECRET: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// The proxy on pipes instead of the process's streams, with a broker the
/// test plays on a free loopback port. Nothing leaves this machine.
struct Session {
    input: Option<PipeWriter>,
    output: BufReader<PipeReader>,
    broker: TcpListener,
    served: std::thread::JoinHandle<i32>,
}

fn session() -> Session {
    let broker = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = broker.local_addr().unwrap().port();
    let (requests, input) = std::io::pipe().unwrap();
    let (output, replies) = std::io::pipe().unwrap();
    Session {
        input: Some(input),
        output: BufReader::new(output),
        broker,
        served: std::thread::spawn(move || serve(requests, replies, port, SECRET.into(), |_| {})),
    }
}

impl Session {
    fn send(&mut self, message: Value) {
        let mut input = self.input.as_ref().unwrap();
        writeln!(input, "{message}").unwrap();
    }

    fn raw(&mut self, line: &[u8]) {
        let mut input = self.input.as_ref().unwrap();
        input.write_all(line).unwrap();
        input.write_all(b"\n").unwrap();
    }

    /// The next reply line. Replies are the only thing ever written.
    fn reply(&mut self) -> Value {
        let mut line = String::new();
        assert!(self.output.read_line(&mut line).unwrap() > 0, "the proxy stopped answering");
        serde_json::from_str(&line).unwrap()
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.reply()
    }

    fn initialize(&mut self) {
        let reply = self.request(1, "initialize", json!({"protocolVersion": "2025-06-18"}));
        assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    }

    /// One connection as the broker sees it: the request line and the socket
    /// to answer on.
    fn accept(&self) -> (Value, TcpStream) {
        let (socket, peer) = self.broker.accept().unwrap();
        assert!(peer.ip().is_loopback());
        let mut line = String::new();
        BufReader::new(socket.try_clone().unwrap()).read_line(&mut line).unwrap();
        (serde_json::from_str(&line).unwrap(), socket)
    }

    /// Closes the proxy's input, as a client that went away, and returns its
    /// exit code.
    fn finish(mut self) -> i32 {
        self.input.take();
        self.served.join().unwrap()
    }
}

fn answer(mut socket: TcpStream, result: Value) {
    writeln!(socket, "{}", json!({"ok": true, "result": result})).unwrap();
}

/// True once the peer closed its end, which is how the broker hears of a
/// cancellation.
fn closed_by_peer(socket: &mut TcpStream) -> bool {
    socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    matches!(socket.read(&mut [0u8; 16]), Ok(0))
}

#[test]
fn only_the_windows_own_loopback_broker_and_token_are_accepted() {
    assert_eq!(configuration(Some("127.0.0.1:40123"), Some(SECRET)), Some((40123, SECRET.to_owned())));
    assert_eq!(configuration(Some("127.0.0.1:65535"), Some(SECRET)).map(|(port, _)| port), Some(65535));
    for broker in [
        None,
        Some(""),
        Some("127.0.0.1"),
        Some("127.0.0.1:"),
        Some("127.0.0.1:0"),
        Some("127.0.0.1:65536"),
        Some("127.0.0.1:123456"),
        Some("127.0.0.1:+80"),
        Some("127.0.0.1:80 "),
        Some("localhost:40123"),
        Some("127.0.0.2:40123"),
        Some("10.0.0.7:40123"),
        Some("[::1]:40123"),
        Some("example.test:443"),
    ] {
        assert_eq!(configuration(broker, Some(SECRET)), None, "{broker:?}");
    }
    let upper = SECRET.to_uppercase();
    for token in [None, Some(""), Some(&SECRET[1..]), Some(upper.as_str()), Some("g".repeat(64).as_str()), Some(format!("{SECRET}a").as_str())] {
        assert_eq!(configuration(Some("127.0.0.1:40123"), token), None, "{token:?}");
    }
}

#[test]
fn the_proxy_transports_scoped_requests_to_its_broker_and_starts_no_browser() {
    let mut session = session();
    let early = session.request(1, "tools/list", json!({}));
    assert!(early["error"]["message"].as_str().unwrap().contains("Initialize"));
    assert_eq!(early["error"]["code"], -32000);

    let initialized = session.request(2, "initialize", json!({"protocolVersion": "2025-11-25"}));
    assert_eq!(initialized["jsonrpc"], "2.0");
    assert_eq!(initialized["id"], 2);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(initialized["result"]["serverInfo"]["name"], "openide-browser");
    assert_eq!(initialized["result"]["capabilities"], json!({"tools": {}}));
    let instructions = initialized["result"]["instructions"].as_str().unwrap();
    for phrase in ["browser_snapshot", "[ref=e12]", "snapshot again after a navigation", "over browser_evaluate", "untrusted data"] {
        assert!(instructions.contains(phrase), "server instructions carry: {phrase}");
    }
    // A version this server does not know gets the newest it does.
    assert_eq!(session.request(3, "initialize", json!({"protocolVersion": "1999-01-01"}))["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(session.request(4, "ping", json!({}))["result"], json!({}));

    session.send(json!({"jsonrpc": "2.0", "id": "lista", "method": "tools/list"}));
    let (asked, socket) = session.accept();
    assert_eq!(asked, json!({"op": "list_tools", "token": SECRET}));
    answer(socket, json!({"tools": [{"name": "browser_snapshot", "inputSchema": {"type": "object"}}]}));
    let listed = session.reply();
    assert_eq!(listed["id"], "lista");
    assert_eq!(listed["result"]["tools"][0]["name"], "browser_snapshot");

    session.send(json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "browser_click", "arguments": {"ref": "e12"}}}));
    let (asked, socket) = session.accept();
    assert_eq!(asked, json!({"op": "call_tool", "name": "browser_click", "arguments": {"ref": "e12"}, "token": SECRET}));
    answer(socket, json!({"content": [{"type": "text", "text": "localhost fixture"}]}));
    assert_eq!(session.reply()["result"]["content"][0]["text"], "localhost fixture");
    // Without arguments the broker gets an empty object.
    session.send(json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "browser_snapshot"}}));
    let (asked, socket) = session.accept();
    assert_eq!(asked["arguments"], json!({}));
    answer(socket, json!({"content": []}));
    assert_eq!(session.reply()["id"], 7);

    session.broker.set_nonblocking(true).unwrap();
    assert!(session.broker.accept().is_err(), "the broker heard of the three requests that needed it, and no other");
    assert_eq!(session.finish(), 0);
}

#[test]
fn malformed_requests_are_answered_without_reaching_the_broker() {
    let mut session = session();
    session.initialize();
    session.raw(b"{not json");
    assert_eq!(session.reply(), json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Invalid JSON."}}));
    session.raw(b"");
    assert_eq!(session.reply()["error"]["code"], -32700);
    for invalid in [json!(7), json!([1]), json!({"id": 1, "method": "ping"}), json!({"jsonrpc": "1.0", "id": 1, "method": "ping"}), json!({"jsonrpc": "2.0", "id": 1, "method": 5})] {
        session.send(invalid);
        let reply = session.reply();
        assert_eq!((&reply["id"], &reply["error"]["code"], &reply["error"]["message"]), (&Value::Null, &json!(-32600), &json!("Invalid JSON-RPC request.")));
    }
    for id in [json!(null), json!({}), json!([1]), json!(true), json!(1.5), json!(9007199254740993u64), json!("x".repeat(257))] {
        session.send(json!({"jsonrpc": "2.0", "id": id, "method": "ping"}));
        let reply = session.reply();
        assert_eq!((&reply["id"], &reply["error"]["message"]), (&Value::Null, &json!("Invalid request id.")), "{id}");
    }
    for id in [json!(0), json!(-3), json!(9007199254740991u64), json!(""), json!("x".repeat(256))] {
        session.send(json!({"jsonrpc": "2.0", "id": id, "method": "ping"}));
        assert_eq!(session.reply(), json!({"jsonrpc": "2.0", "id": id, "result": {}}));
    }
    assert_eq!(session.request(9, "resources/list", json!({}))["error"], json!({"code": -32601, "message": "Method not found."}));
    for params in [json!({}), json!({"name": 7}), json!({"name": "x".repeat(129)})] {
        assert_eq!(session.request(10, "tools/call", params)["error"]["message"], "Tool name is missing or invalid.");
    }
    // A notification is never answered, whatever it says.
    session.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    session.send(json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 404}}));
    assert_eq!(session.request(11, "ping", json!({}))["id"], 11);
    session.broker.set_nonblocking(true).unwrap();
    assert!(session.broker.accept().is_err(), "none of this reached the broker");
    assert_eq!(session.finish(), 0);
}

#[test]
fn four_requests_wait_at_most_and_a_cancelled_one_closes_its_connection_unanswered() {
    let mut session = session();
    session.initialize();
    let call = |id: u64| json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "browser_wait_for", "arguments": {"seconds": 10}}});
    let mut held = Vec::new();
    for id in 10..14 {
        session.send(call(id));
        held.push(session.accept().1);
    }
    // The fifth is refused, and so is anything else while four wait.
    session.send(call(14));
    assert_eq!(session.reply(), json!({"jsonrpc": "2.0", "id": 14, "error": {"code": -32000, "message": "Browser request queue is full."}}));
    assert_eq!(session.request(15, "ping", json!({}))["error"]["message"], "Browser request queue is full.");
    assert_eq!(session.request(11, "ping", json!({}))["error"]["message"], "Duplicate active request id.");

    // Cancelling one closes its broker connection: that is how the window
    // keeps a queued action from running. It gets no answer.
    session.send(json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 11}}));
    assert!(closed_by_peer(&mut held[1]));
    // Its place is free again.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let reply = session.request(20, "ping", json!({}));
        if reply.get("result").is_some() {
            break;
        }
        assert_eq!(reply["error"]["message"], "Browser request queue is full.");
        assert!(Instant::now() < deadline, "the cancelled request kept its place");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Four wait again. The others are still answered, in whatever order the
    // broker replies, and a client that has read a reply finds that id and
    // its place free at once.
    session.send(call(21));
    held.push(session.accept().1);
    answer(held.remove(3), json!({"content": [{"type": "text", "text": "trece"}]}));
    let reply = session.reply();
    assert_eq!((&reply["id"], &reply["result"]["content"][0]["text"]), (&json!(13), &json!("trece")));
    assert_eq!(session.request(13, "ping", json!({})), json!({"jsonrpc": "2.0", "id": 13, "result": {}}));
    answer(held.remove(0), json!({"content": []}));
    assert_eq!(session.reply()["id"], 10);

    // The client goes away with requests waiting: they are cancelled too.
    let mut waiting = held.remove(1);
    drop(held);
    let (served, input) = (session.served, session.input.take());
    drop(input);
    assert_eq!(served.join().unwrap(), 0);
    assert!(closed_by_peer(&mut waiting));
}

#[test]
fn a_broker_that_fails_is_reported_and_the_proxy_goes_on() {
    let mut session = session();
    session.initialize();
    let list = |id: u64| json!({"jsonrpc": "2.0", "id": id, "method": "tools/list"});
    // It hangs up, answers something else, or answers more than a reply may hold.
    session.send(list(2));
    drop(session.accept());
    assert_eq!(session.reply()["error"], json!({"code": -32000, "message": "Scoped browser broker closed the connection."}));
    for refused in [json!({"ok": false, "result": {}}), json!({"ok": true}), json!({"result": {}}), json!("ok")] {
        session.send(list(3));
        let (_, mut socket) = session.accept();
        writeln!(socket, "{refused}").unwrap();
        assert_eq!(session.reply()["error"]["message"], "Invalid scoped browser response.", "{refused}");
    }
    session.send(list(4));
    let (_, mut socket) = session.accept();
    writeln!(socket, "esto no es JSON").unwrap();
    assert_eq!(session.reply()["error"]["message"], "Invalid scoped browser response.");
    session.send(list(5));
    let (_, mut socket) = session.accept();
    // Without a line end, so the bound is what stops the read.
    let _ = socket.write_all(&vec![b'x'; MAX_OUTPUT + 1]);
    assert_eq!(session.reply()["error"]["message"], "Browser response exceeds 4 MiB.");
    // A request that fits a line but, with the token, not the broker's own
    // bound is never sent.
    let call = |length: usize| json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "browser_evaluate", "arguments": {"expression": "x".repeat(length)}}});
    let overhead = call(0).to_string().len();
    session.send(call(MAX_INPUT - overhead));
    assert_eq!(session.reply()["error"]["message"], "Browser request exceeds 64 KiB.");
    // A reply of exactly the size the broker may send is passed on whole.
    session.send(list(7));
    let (_, socket) = session.accept();
    let large = "y".repeat(MAX_OUTPUT - 4096);
    answer(socket, json!({"tools": [], "padding": large}));
    assert_eq!(session.reply()["result"]["padding"].as_str().map(str::len), Some(MAX_OUTPUT - 4096));
    assert_eq!(session.request(8, "ping", json!({}))["result"], json!({}));
    assert_eq!(session.finish(), 0);
}

#[test]
fn a_line_over_the_input_bound_or_a_reader_that_left_ends_the_proxy_with_an_error() {
    // A request line may hold 64 KiB; one byte more ends the service.
    let mut session = session();
    session.initialize();
    let padding = MAX_INPUT - r#"{"jsonrpc":"2.0","id":2,"method":"ping","x":""}"#.len();
    session.raw(format!(r#"{{"jsonrpc":"2.0","id":2,"method":"ping","x":"{}"}}"#, "x".repeat(padding)).as_bytes());
    assert_eq!(session.reply()["id"], 2);
    session.send(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}));
    let (_, mut waiting) = session.accept();
    // The proxy may be gone before the whole line is written.
    let long = format!(r#"{{"jsonrpc":"2.0","id":4,"method":"ping","x":"{}"}}"#, "x".repeat(padding + 1));
    let _ = writeln!(session.input.as_ref().unwrap(), "{long}");
    assert!(closed_by_peer(&mut waiting), "what was waiting is cancelled");
    assert_eq!(session.served.join().unwrap(), 1);

    // A last line without its terminator is not a request.
    let (requests, mut input) = std::io::pipe().unwrap();
    input.write_all(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
    drop(input);
    let (mut replies, output) = std::io::pipe().unwrap();
    assert_eq!(serve(requests, output, 9, SECRET.into(), |_| {}), 0);
    let mut written = String::new();
    replies.read_to_string(&mut written).unwrap();
    assert_eq!(written, "");

    // Nobody reads the replies any more: the first one that cannot be
    // written ends the service.
    let (ended, exit): (mpsc::Sender<i32>, Receiver<i32>) = mpsc::channel();
    static ENDED: Mutex<Option<mpsc::Sender<i32>>> = Mutex::new(None);
    *ENDED.lock().unwrap() = Some(ended);
    let (requests, mut input) = std::io::pipe().unwrap();
    let (replies, output) = std::io::pipe().unwrap();
    drop(replies);
    let served = std::thread::spawn(move || {
        serve(requests, output, 9, SECRET.into(), |code| {
            if let Some(ended) = ENDED.lock().unwrap().as_ref() {
                let _ = ended.send(code);
            }
        })
    });
    writeln!(input, r#"{{"jsonrpc":"2.0","id":1,"method":"ping"}}"#).unwrap();
    assert_eq!(exit.recv_timeout(Duration::from_secs(10)), Ok(1), "the process is told to end at once");
    assert_eq!(served.join().unwrap(), 1);
}
