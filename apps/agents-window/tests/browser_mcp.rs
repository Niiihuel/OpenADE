//! The built executable in its proxy mode, as an external MCP client runs
//! it: `agents-window --browser-mcp` with a cleared environment that holds
//! the broker's address and token and where the loader finds the
//! executable's libraries. The broker is this test's own, on a free loopback
//! port; no window, browser or user configuration is involved.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};

const SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn proxy(home: &std::path::Path, environment: &[(&str, String)]) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agents-window"));
    command
        .arg("--browser-mcp")
        .current_dir(home)
        .env_clear()
        // Anything the application would read or write lives under these.
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_STATE_HOME", home.join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // As the window's own configuration does: only what the loader needs.
    for name in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for (name, value) in environment {
        command.env(name, value);
    }
    command.spawn().expect("the application's executable starts")
}

#[test]
fn the_executable_serves_mcp_over_its_standard_streams_and_touches_nothing() {
    let home = tempfile::tempdir().unwrap();
    let broker = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = format!("127.0.0.1:{}", broker.local_addr().unwrap().port());
    let mut child = proxy(
        home.path(),
        &[("OPENIDE_BROWSER_BROKER", address), ("OPENIDE_BROWSER_TOKEN", SECRET.into())],
    );
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut request = |message: Value| {
        writeln!(input, "{message}").unwrap();
    };
    let mut reply = || -> Value {
        let mut line = String::new();
        assert!(output.read_line(&mut line).unwrap() > 0, "the proxy stopped answering");
        serde_json::from_str(&line).unwrap()
    };
    request(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25"}}));
    let initialized = reply();
    assert_eq!(initialized["result"]["serverInfo"]["name"], "openide-browser");
    assert!(initialized["result"]["instructions"].as_str().unwrap().contains("browser_snapshot"));

    request(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "browser_navigate", "arguments": {"url": "http://localhost:3000/"}}}));
    let (mut socket, peer) = broker.accept().unwrap();
    assert!(peer.ip().is_loopback());
    let mut asked = String::new();
    BufReader::new(socket.try_clone().unwrap()).read_line(&mut asked).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&asked).unwrap(),
        json!({"op": "call_tool", "name": "browser_navigate", "arguments": {"url": "http://localhost:3000/"}, "token": SECRET})
    );
    writeln!(socket, "{}", json!({"ok": true, "result": {"content": [{"type": "text", "text": "abierta"}], "isError": false}})).unwrap();
    let called = reply();
    assert_eq!((&called["id"], &called["result"]["content"][0]["text"]), (&json!(2), &json!("abierta")));

    // The client closes its end: the proxy ends, without an error.
    drop(input);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status:?}");
    let mut complaints = String::new();
    child.stderr.take().unwrap().read_to_string(&mut complaints).unwrap();
    assert_eq!(complaints, "");
    assert_eq!(
        std::fs::read_dir(home.path()).unwrap().count(),
        0,
        "the proxy created no file or directory"
    );
}

#[test]
fn without_its_broker_the_executable_refuses_to_start_and_opens_no_window() {
    let home = tempfile::tempdir().unwrap();
    for environment in [
        vec![],
        vec![("OPENIDE_BROWSER_BROKER", "127.0.0.1:40123".to_owned())],
        vec![("OPENIDE_BROWSER_BROKER", "example.test:443".to_owned()), ("OPENIDE_BROWSER_TOKEN", SECRET.to_owned())],
        vec![("OPENIDE_BROWSER_BROKER", "127.0.0.1:40123".to_owned()), ("OPENIDE_BROWSER_TOKEN", "corto".to_owned())],
    ] {
        let output = proxy(home.path(), &environment).wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{environment:?}");
        assert_eq!(output.stdout, b"");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "Missing scoped openADE browser broker configuration.\n"
        );
    }
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}
