//! `openade-graph`: the workspace code graph as a CLI and as an MCP stdio
//! server, so OpenADE's harness and external agent CLIs share one index.

use std::fmt::Write as _;
use std::io::{BufRead, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use openide_graph::{Explanation, Graph, Outline, Symbol};
use serde_json::{json, Value};

const USAGE: &str = "usage: openade-graph [--workspace DIR] [--db FILE] COMMAND
commands:
  index              update the graph and print what changed
  outline PATH       symbols, imports and importers of one file
  find NAME          definitions by name, or containing NAME
  explain NAME       a definition with its callers, callees and members
  path FROM TO       shortest call chain between two definitions
  stats              size of the graph
  mcp                serve the graph over MCP stdio
  hook               answer a PreToolUse hook event read from stdin";

/// Exit code when a query matched nothing.
const NO_MATCH: u8 = 4;
const LIMIT: usize = 20;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// File name of the empty entrypoint the OpenADE plugin manifest requires.
const PLUGIN_ENTRY: &str = "openade-graph.mcp";
/// Above this many lines, a whole-file Read is answered with the outline.
const LARGE_FILE_LINES: usize = 400;
/// Outline lines quoted back in a refused Read.
const OUTLINE_LINES: usize = 150;

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // A plugin with a PATH program runs `program <entrypoint> <args...>`.
    if args.len() == 2 && args[0].ends_with(PLUGIN_ENTRY) {
        args = vec!["--workspace".into(), args[1].clone(), "mcp".into()];
    }
    let mut workspace = PathBuf::from(".");
    let mut db = None;
    while args.first().is_some_and(|a| a.starts_with("--")) {
        let flag = args.remove(0);
        let Some(value) = (!args.is_empty()).then(|| args.remove(0)) else {
            return usage();
        };
        match flag.as_str() {
            "--workspace" => workspace = value.into(),
            "--db" => db = Some(PathBuf::from(value)),
            _ => return usage(),
        }
    }
    let Some(command) = (!args.is_empty()).then(|| args.remove(0)) else {
        return usage();
    };
    if command == "hook" && args.is_empty() {
        return hook();
    }
    let graph = match &db {
        Some(db) => Graph::open_at(&workspace, db),
        None => Graph::open(&workspace),
    };
    let mut graph = match graph {
        Ok(graph) => graph,
        Err(error) => {
            eprintln!("openade-graph: {}: {error}", workspace.display());
            return ExitCode::FAILURE;
        }
    };
    if command == "mcp" {
        return match serve(&mut graph) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("openade-graph: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let arguments = match (command.as_str(), args.as_slice()) {
        ("index" | "stats", []) => json!({}),
        ("outline", [path]) => json!({ "path": path }),
        ("find" | "explain", [name]) => json!({ "name": name }),
        ("path", [from, to]) => json!({ "from": from, "to": to }),
        _ => return usage(),
    };
    match run(&mut graph, &command, &arguments) {
        Ok((text, matched)) => {
            println!("{text}");
            if matched {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(NO_MATCH)
            }
        }
        Err(error) => {
            eprintln!("openade-graph: {error}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

/// Refreshes, then answers one command; the flag is false when nothing matched.
fn run(graph: &mut Graph, command: &str, args: &Value) -> Result<(String, bool), String> {
    let refreshed = graph.refresh().map_err(|e| e.to_string())?;
    let text = |key: &str| -> Result<&str, String> {
        args.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("`{key}` must be a non-empty string"))
    };
    let e = |e: openide_graph::Error| e.to_string();
    match command {
        "index" => Ok((
            format!(
                "{} source files; reparsed {}, removed {}",
                refreshed.files, refreshed.parsed, refreshed.removed
            ),
            true,
        )),
        "stats" => {
            let s = graph.stats().map_err(e)?;
            let langs: Vec<String> = s.by_lang.iter().map(|(l, n)| format!("{l} {n}")).collect();
            Ok((
                format!(
                    "{} files ({}), {} symbols, {} references, {} imports",
                    s.files,
                    langs.join(", "),
                    s.symbols,
                    s.refs,
                    s.imports
                ),
                true,
            ))
        }
        "outline" => {
            let path = text("path")?;
            Ok(match graph.outline(path).map_err(e)? {
                Some(outline) => (render_outline(&outline), true),
                None => (format!("{path} is not an indexed source file"), false),
            })
        }
        "find" => {
            let name = text("name")?;
            let (hits, exact) = graph.find(name, LIMIT).map_err(e)?;
            if hits.is_empty() {
                return Ok((format!("no definition matches `{name}`"), false));
            }
            let mut out = String::new();
            if !exact {
                let _ = writeln!(out, "no exact match; names containing `{name}`:");
            }
            for hit in &hits {
                let _ = writeln!(out, "{}", render_symbol(hit));
            }
            Ok((out.trim_end().to_string(), true))
        }
        "explain" => {
            let name = text("name")?;
            let found = graph.explain(name, 5).map_err(e)?;
            if found.is_empty() {
                return Ok((format!("no definition named `{name}`; try find"), false));
            }
            let parts: Vec<String> = found.iter().map(render_explanation).collect();
            Ok((parts.join("\n\n"), true))
        }
        "path" => {
            let (from, to) = (text("from")?, text("to")?);
            Ok(match graph.path(from, to, 8).map_err(e)? {
                Some(chain) => {
                    let lines: Vec<String> = chain
                        .iter()
                        .enumerate()
                        .map(|(i, (symbol, same))| match same {
                            n if *n > 1 && i > 0 => format!("{}  [guess: {n} defs share this name]", render_symbol(symbol)),
                            _ => render_symbol(symbol),
                        })
                        .collect();
                    (lines.join("\n  → "), true)
                }
                None => (format!("no call chain from `{from}` to `{to}` within 8 calls"), false),
            })
        }
        _ => Err(format!("unknown command `{command}`")),
    }
}

fn render_symbol(s: &Symbol) -> String {
    format!("{} {}  {}:{}-{}  {}", s.kind, s.qualname, s.path, s.line, s.end_line, s.signature)
}

fn render_outline(o: &Outline) -> String {
    let mut out = format!("{} ({}, {} symbols)\n", o.path, o.lang, o.symbols.len());
    if !o.imports.is_empty() {
        let imports: Vec<String> = o
            .imports
            .iter()
            .map(|(target, path)| match path {
                Some(path) if path != target => format!("{target} → {path}"),
                _ => target.clone(),
            })
            .collect();
        let _ = writeln!(out, "imports: {}", imports.join("; "));
    }
    if !o.imported_by.is_empty() {
        let _ = writeln!(out, "imported by: {}", o.imported_by.join(", "));
    }
    if !o.identical.is_empty() {
        let _ = writeln!(out, "identical content: {}", o.identical.join(", "));
    }
    for entry in &o.symbols {
        let s = &entry.symbol;
        let _ = writeln!(
            out,
            "{}{} {} {}-{}  {}",
            "  ".repeat(entry.depth),
            s.kind,
            s.name,
            s.line,
            s.end_line,
            s.signature
        );
    }
    out.trim_end().to_string()
}

fn render_explanation(x: &Explanation) -> String {
    let mut out = render_symbol(&x.symbol);
    if let Some(parent) = &x.parent {
        let _ = write!(out, "\nin: {parent}");
    }
    if x.same_name > 1 {
        let _ = write!(
            out,
            "\nnote: {} definitions share this name; callers below are matched by name",
            x.same_name
        );
    }
    let sites = |sites: &[openide_graph::Site]| -> String {
        sites
            .iter()
            .map(|s| match &s.within {
                Some(within) => format!("{within} ({}:{})", s.path, s.line),
                None => format!("{}:{}", s.path, s.line),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !x.members.is_empty() {
        let members: Vec<String> = x.members.iter().map(|m| format!("{} {}:{}", m.kind, m.qualname, m.line)).collect();
        let _ = write!(out, "\nmembers: {}", members.join(", "));
    }
    if !x.callers.is_empty() {
        let _ = write!(out, "\ncalled by: {}", sites(&x.callers));
    } else if x.used_by.is_empty() && x.implementations.is_empty() {
        out.push_str("\ncalled by: nothing indexed");
    }
    if !x.callees.is_empty() {
        let callees: Vec<String> = x
            .callees
            .iter()
            .map(|(name, defs)| match defs {
                0 => format!("{name} (external)"),
                1 => name.clone(),
                n => format!("{name} ({n} defs)"),
            })
            .collect();
        let _ = write!(out, "\ncalls: {}", callees.join(", "));
    }
    if !x.implementations.is_empty() {
        let _ = write!(out, "\nimplemented by: {}", sites(&x.implementations));
    }
    if !x.used_by.is_empty() {
        let _ = write!(out, "\nused by: {}", sites(&x.used_by));
    }
    out
}

/// A PreToolUse hook for Claude Code, Codex and OpenADE (which reads the same
/// hooks from ~/.claude/settings.json). It only speaks where a graph already
/// exists, and never gets in the tool's way: on any failure it says nothing.
fn hook() -> ExitCode {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return ExitCode::SUCCESS;
    }
    let reply = serde_json::from_str::<Value>(&input).ok().and_then(|event| {
        let cwd = match event.get("cwd").and_then(Value::as_str) {
            Some(cwd) => PathBuf::from(cwd),
            None => std::env::current_dir().ok()?,
        };
        // The file or folder the tool touches decides the project, not the session.
        let target = event["tool_input"]
            .get("file_path")
            .or_else(|| event["tool_input"].get("path"))
            .and_then(Value::as_str)
            .map(|p| cwd.join(p));
        let (root, db) = target
            .and_then(|t| openide_graph::find_existing(&t))
            .or_else(|| openide_graph::find_existing(&cwd))?;
        let mut graph = Graph::open_at(&root, &db).ok()?;
        graph.refresh().ok()?;
        hook_reply(&graph, &cwd, &event)
    });
    if let Some(reply) = reply {
        let _ = writeln!(std::io::stdout(), "{reply}");
    }
    ExitCode::SUCCESS
}

fn hook_reply(graph: &Graph, cwd: &Path, event: &Value) -> Option<Value> {
    let input = event.get("tool_input")?;
    match event.get("tool_name")?.as_str()? {
        "Read" => read_hint(graph, cwd, input),
        "Grep" => symbol_hint(graph, input.get("pattern").or_else(|| input.get("query"))?.as_str()?),
        "Bash" | "shell" => {
            let command = match input.get("command")? {
                Value::String(command) => shell_words::split(command).ok()?,
                Value::Array(words) => words.iter().filter_map(Value::as_str).map(String::from).collect(),
                _ => return None,
            };
            symbol_hint(graph, &searched_pattern(&command)?)
        }
        _ => None,
    }
}

/// Refuses a whole-file Read of a large indexed file, handing back its outline.
fn read_hint(graph: &Graph, cwd: &Path, input: &Value) -> Option<Value> {
    // OpenADE's read_file keeps its own `path` and takes no range: never refuse it.
    if ["path", "offset", "limit"].iter().any(|key| input.get(key).is_some()) {
        return None;
    }
    let file = Path::new(input.get("file_path")?.as_str()?);
    let file = cwd.join(file).canonicalize().ok()?;
    let rel = file.strip_prefix(graph.root()).ok()?.to_str()?.replace('\\', "/");
    let outline = graph.outline(&rel).ok()??;
    let lines = std::fs::read(&file).ok()?.iter().filter(|&&b| b == b'\n').count();
    if lines <= LARGE_FILE_LINES || outline.symbols.len() < 3 {
        return None;
    }
    let text = render_outline(&outline);
    let total = text.lines().count();
    let mut reason = format!(
        "{rel} has {lines} lines. Its outline from the code graph follows: Read again with offset and limit \
         for only the lines you need (offset 1 with limit {lines} reads it whole on purpose).\n\n{}",
        text.lines().take(OUTLINE_LINES).collect::<Vec<_>>().join("\n")
    );
    if total > OUTLINE_LINES {
        let _ = write!(reason, "\n… {} more lines: graph_outline \"{rel}\"", total - OUTLINE_LINES);
    }
    Some(json!({ "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": reason,
    }}))
}

/// Lets a text search go ahead, telling the agent where the graph already
/// places the identifier it is searching for.
fn symbol_hint(graph: &Graph, pattern: &str) -> Option<Value> {
    let name = identifier_in(pattern)?;
    let (hits, exact) = graph.find(&name, 9).ok()?;
    if !exact || hits.is_empty() {
        return None;
    }
    let mut text = format!("The code graph already knows `{name}`:\n");
    for hit in hits.iter().take(8) {
        let _ = writeln!(text, "{}", render_symbol(hit));
    }
    if hits.len() > 8 {
        text.push_str("… more with graph_find\n");
    }
    let _ = write!(
        text,
        "For its callers, callees and members use graph_explain (or `openade-graph explain {name}`) instead of searching text."
    );
    Some(json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "additionalContext": text } }))
}

/// The pattern of a plain `rg`, `grep`, `git grep`, `ag` or `ack` search: the
/// first command of the line only.
fn searched_pattern(words: &[String]) -> Option<String> {
    let end = words.iter().position(|w| matches!(w.as_str(), "|" | "||" | "&&" | ";")).unwrap_or(words.len());
    let mut words = words[..end].iter().map(String::as_str);
    let mut tool = words.next()?;
    if tool == "git" {
        tool = words.next()?;
    }
    if !matches!(tool, "rg" | "grep" | "egrep" | "ag" | "ack") {
        return None;
    }
    // Flags whose value is not the pattern.
    const VALUED: [&str; 15] = [
        "-g", "--glob", "-t", "--type", "-T", "--type-not", "-A", "-B", "-C", "-m", "--max-count", "-f", "--include",
        "--exclude", "--max-depth",
    ];
    while let Some(word) = words.next() {
        if word == "-e" || word == "--regexp" {
            return words.next().map(String::from);
        }
        if VALUED.contains(&word) {
            words.next();
        } else if !word.starts_with('-') {
            return Some(word.to_string());
        }
    }
    None
}

/// The identifier a search pattern looks for, if it looks for exactly one:
/// `\bfoo\b`, `fn foo(`, `class Foo` and `Foo::bar` all qualify.
fn identifier_in(pattern: &str) -> Option<String> {
    let mut text = pattern.replace("\\b", "").replace("\\s+", " ").replace("\\(", "");
    text = text.trim().trim_start_matches('^').trim_end_matches(['$', '(']).trim().to_string();
    for keyword in [
        "pub fn ", "fn ", "struct ", "enum ", "trait ", "impl ", "class ", "def ", "function ", "interface ", "type ",
        "func ", "const ", "let ", "mod ",
    ] {
        if let Some(rest) = text.strip_prefix(keyword) {
            text = rest.trim().to_string();
            break;
        }
    }
    let part_ok = |part: &str| {
        part.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
            && part.chars().all(|c| c.is_alphanumeric() || c == '_')
    };
    (text.len() >= 3 && text.split("::").flat_map(|p| p.split('.')).all(part_ok)).then_some(text)
}

fn tools() -> Value {
    let string = |description: &str| json!({ "type": "string", "description": description });
    let tool = |name: &str, description: &str, properties: Value, required: Vec<&str>| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
            },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "openWorldHint": false },
        })
    };
    json!([
        tool(
            "graph_outline",
            "Outline one source file from the workspace code graph: every definition with its line range and signature, its imports, the files importing it and byte-identical copies. Call this before reading a file, then read only the lines you need.",
            json!({ "path": string("Path relative to the workspace root") }),
            vec!["path"],
        ),
        tool(
            "graph_find",
            "Find definitions (functions, methods, types, modules) by exact or qualified name, falling back to names that contain the text. Faster and cheaper than grep for locating code.",
            json!({ "name": string("Name such as `refresh`, `Graph::refresh` or `Graph.refresh`") }),
            vec!["name"],
        ),
        tool(
            "graph_explain",
            "Explain a definition: where it is, what contains it, its members, who calls it, what it calls and what implements it.",
            json!({ "name": string("Exact or qualified definition name") }),
            vec!["name"],
        ),
        tool(
            "graph_path",
            "Shortest call chain from one definition to another, following calls by name.",
            json!({ "from": string("Starting definition"), "to": string("Target definition") }),
            vec!["from", "to"],
        ),
        tool("graph_stats", "Size of the workspace code graph by language.", json!({}), vec![]),
    ])
}

/// Line-delimited JSON-RPC over stdio, as the MCP stdio transport defines it.
fn serve(graph: &mut Graph) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut initialized = false;
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle(graph, &mut initialized, &line) {
            serde_json::to_writer(&mut stdout, &reply)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn handle(graph: &mut Graph, initialized: &mut bool, line: &str) -> Option<Value> {
    let fail = |id: Value, code: i64, message: &str| {
        Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
    };
    if line.len() > MAX_MESSAGE_BYTES {
        return fail(Value::Null, -32700, "Message exceeds 1 MiB");
    }
    let Ok(request) = serde_json::from_str::<Value>(line) else {
        return fail(Value::Null, -32700, "Invalid JSON");
    };
    if request.get("jsonrpc") != Some(&json!("2.0")) {
        return fail(Value::Null, -32600, "Invalid JSON-RPC request");
    }
    // Notifications, including notifications/initialized, have no reply.
    let id = request.get("id")?.clone();
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let ok = |result: Value| Some(json!({ "jsonrpc": "2.0", "id": id.clone(), "result": result }));
    match request.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            *initialized = true;
            let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18");
            ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "openade-graph", "version": env!("CARGO_PKG_VERSION") },
            }))
        }
        Some("ping") => ok(json!({})),
        _ if !*initialized => fail(id, -32000, "Initialize the server first"),
        Some("tools/list") => ok(json!({ "tools": tools() })),
        Some("tools/call") => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let Some(command) = name.strip_prefix("graph_") else {
                return fail(id, -32602, "Unknown tool");
            };
            if !["outline", "find", "explain", "path", "stats"].contains(&command) {
                return fail(id, -32602, "Unknown tool");
            }
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let (text, is_error) = match run(graph, command, &args) {
                Ok((text, _)) => (text, false),
                Err(error) => (error, true),
            };
            ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
        }
        _ => fail(id, -32601, "Method not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(command: &str) -> Vec<String> {
        shell_words::split(command).unwrap()
    }

    #[test]
    fn search_commands_yield_their_pattern() {
        assert_eq!(searched_pattern(&words("rg -n 'fn helper' src | head")).as_deref(), Some("fn helper"));
        assert_eq!(searched_pattern(&words("grep -rn -A 3 refresh .")).as_deref(), Some("refresh"));
        assert_eq!(searched_pattern(&words("git grep -e Graph::open")).as_deref(), Some("Graph::open"));
        assert_eq!(searched_pattern(&words("rg -g '*.rs' --type rust needle")).as_deref(), Some("needle"));
        assert_eq!(searched_pattern(&words("ls -la && rg x")), None);
        assert_eq!(identifier_in(r"\bhelper\b").as_deref(), Some("helper"));
        assert_eq!(identifier_in(r"fn helper\(").as_deref(), Some("helper"));
        assert_eq!(identifier_in("Graph::refresh").as_deref(), Some("Graph::refresh"));
        assert_eq!(identifier_in("foo.*bar"), None);
        assert_eq!(identifier_in("TODO: fix"), None);
        assert_eq!(identifier_in("ab"), None);
    }

    #[test]
    fn hook_refuses_whole_reads_of_large_files_and_hints_searches() {
        let dir = tempfile::tempdir().unwrap();
        let mut big = String::new();
        for i in 0..150 {
            let _ = write!(big, "fn f{i}() {{\n    helper();\n}}\n");
        }
        std::fs::write(dir.path().join("big.rs"), &big).unwrap();
        std::fs::write(dir.path().join("small.rs"), "fn helper() {}\nfn a() {}\nfn b() {}\n").unwrap();
        let mut graph = Graph::open_at(dir.path(), &dir.path().join(".db/g.db")).unwrap();
        graph.refresh().unwrap();
        let cwd = graph.root().to_path_buf();
        let event = |tool: &str, input: Value| json!({ "tool_name": tool, "tool_input": input });

        let refused = hook_reply(&graph, &cwd, &event("Read", json!({ "file_path": cwd.join("big.rs") }))).unwrap();
        let output = &refused["hookSpecificOutput"];
        assert_eq!(output["permissionDecision"], "deny");
        let reason = output["permissionDecisionReason"].as_str().unwrap();
        assert!(reason.contains("big.rs has 450 lines") && reason.contains("function f0 1-3"), "{reason}");
        assert!(reason.contains("more lines: graph_outline"), "outline is capped");

        for input in [
            json!({ "file_path": "big.rs", "offset": 1, "limit": 450 }),
            json!({ "file_path": "big.rs", "path": "big.rs" }),
            json!({ "file_path": "small.rs" }),
            json!({ "file_path": "/etc/hostname" }),
        ] {
            assert!(hook_reply(&graph, &cwd, &event("Read", input.clone())).is_none(), "{input}");
        }

        let hint = hook_reply(&graph, &cwd, &event("Grep", json!({ "pattern": r"\bhelper\b" }))).unwrap();
        let context = hint["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(context.contains("function helper  small.rs:1-1"), "{context}");
        assert!(hint["hookSpecificOutput"].get("permissionDecision").is_none());
        let bash = event("Bash", json!({ "command": "rg -n 'fn helper' . | head" }));
        assert!(hook_reply(&graph, &cwd, &bash).is_some());
        assert!(hook_reply(&graph, &cwd, &event("Grep", json!({ "pattern": "nowhere_defined" }))).is_none());
        assert!(hook_reply(&graph, &cwd, &event("Bash", json!({ "command": "cargo test" }))).is_none());
    }

    #[test]
    fn mcp_lists_and_calls_tools_after_initialize() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn a() { b(); }\nfn b() {}\n").unwrap();
        let mut graph = Graph::open_at(dir.path(), &dir.path().join(".db/g.db")).unwrap();
        let mut init = false;
        let call = |graph: &mut Graph, init: &mut bool, body: &str| handle(graph, init, body).unwrap();

        let early = call(&mut graph, &mut init, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        assert_eq!(early["error"]["code"], -32000);
        call(&mut graph, &mut init, r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#);
        assert!(handle(&mut graph, &mut init, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());

        let list = call(&mut graph, &mut init, r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#);
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 5);

        let explain = call(
            &mut graph,
            &mut init,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"graph_explain","arguments":{"name":"b"}}}"#,
        );
        let text = explain["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("called by: a (lib.rs:1)"), "{text}");

        let bad = call(
            &mut graph,
            &mut init,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"graph_find","arguments":{}}}"#,
        );
        assert_eq!(bad["result"]["isError"], true);
        assert_eq!(call(&mut graph, &mut init, "not json")["error"]["code"], -32700);
    }
}
