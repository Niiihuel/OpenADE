use std::io::{BufRead, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next().expect("mode");
    let workspace = PathBuf::from(args.next().expect("workspace"));
    if !workspace.join(".openide-mcp-tree-test-armed").exists() {
        return;
    }
    if mode == "child" {
        std::fs::write(workspace.join("child-started"), b"started").unwrap();
        std::thread::sleep(Duration::from_secs(3));
        std::fs::write(workspace.join("child-survived"), b"survived").unwrap();
        return;
    }
    assert_eq!(mode, "parent");
    let mut input = std::io::BufReader::new(std::io::stdin());
    let mut output = std::io::stdout().lock();
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).unwrap() == 0 {
            return;
        }
        if line.contains("\"method\":\"initialize\"") {
            writeln!(output, "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{{\"tools\":{{}}}},\"serverInfo\":{{\"name\":\"tree fixture\",\"version\":\"1\"}}}}}}").unwrap();
            output.flush().unwrap();
        } else if line.contains("\"method\":\"notifications/initialized\"") {
            Command::new(std::env::current_exe().unwrap())
                .arg("child")
                .arg(&workspace)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .unwrap();
        } else if line.contains("\"method\":\"tools/list\"") {
            writeln!(output, "{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"tools\":[{{\"name\":\"echo\",\"inputSchema\":{{\"type\":\"object\"}}}}]}}}}").unwrap();
            output.flush().unwrap();
        } else if line.contains("\"method\":\"tools/call\"") {
            std::fs::write(workspace.join("request-started"), b"started").unwrap();
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}
