//! Symbol extraction from one source file. Code goes through tree-sitter with
//! each grammar's bundled tags query (or our own where a grammar ships none)
//! plus patterns for containers and imports; Markdown is read line by line.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    C,
    Cpp,
    Go,
    Java,
    Lua,
    Nix,
    Qml,
    Sql,
    Swift,
    Bash,
    Toml,
    Yaml,
    Json,
    Markdown,
}

pub const ALL_LANGS: [Lang; 19] = [
    Lang::Rust,
    Lang::Python,
    Lang::JavaScript,
    Lang::TypeScript,
    Lang::Tsx,
    Lang::C,
    Lang::Cpp,
    Lang::Go,
    Lang::Java,
    Lang::Lua,
    Lang::Nix,
    Lang::Qml,
    Lang::Sql,
    Lang::Swift,
    Lang::Bash,
    Lang::Toml,
    Lang::Yaml,
    Lang::Json,
    Lang::Markdown,
];

impl Lang {
    pub fn from_path(path: &Path) -> Option<Lang> {
        // Lockfiles are generated and huge; their keys are not code to find.
        if path.file_name()?.to_str()?.ends_with("lock.json") {
            return None;
        }
        Some(match path.extension()?.to_str()? {
            "rs" => Lang::Rust,
            "py" | "pyi" => Lang::Python,
            "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "c" | "h" => Lang::C,
            "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" => Lang::Cpp,
            "go" => Lang::Go,
            "java" => Lang::Java,
            "lua" => Lang::Lua,
            "nix" => Lang::Nix,
            "qml" => Lang::Qml,
            "sql" => Lang::Sql,
            "swift" => Lang::Swift,
            "sh" | "bash" | "zsh" => Lang::Bash,
            "toml" => Lang::Toml,
            "yaml" | "yml" => Lang::Yaml,
            "json" => Lang::Json,
            "md" | "mdx" | "markdown" => Lang::Markdown,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::Lua => "lua",
            Lang::Nix => "nix",
            Lang::Qml => "qml",
            Lang::Sql => "sql",
            Lang::Swift => "swift",
            Lang::Bash => "bash",
            Lang::Toml => "toml",
            Lang::Yaml => "yaml",
            Lang::Json => "json",
            Lang::Markdown => "markdown",
        }
    }

    fn separator(self) -> &'static str {
        match self {
            Lang::Rust | Lang::Cpp => "::",
            Lang::Markdown => " > ",
            _ => ".",
        }
    }

    fn language(self) -> Option<Language> {
        Some(match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::C => tree_sitter_c::LANGUAGE.into(),
            Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
            Lang::Lua => tree_sitter_lua::LANGUAGE.into(),
            Lang::Nix => tree_sitter_nix::LANGUAGE.into(),
            Lang::Qml => tree_sitter_qmljs::LANGUAGE.into(),
            Lang::Sql => tree_sitter_sequel::LANGUAGE.into(),
            Lang::Swift => tree_sitter_swift::LANGUAGE.into(),
            Lang::Bash => tree_sitter_bash::LANGUAGE.into(),
            Lang::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Lang::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Lang::Json => tree_sitter_json::LANGUAGE.into(),
            Lang::Markdown => return None,
        })
    }

    fn query(self) -> Option<&'static Query> {
        static QUERIES: [OnceLock<Query>; ALL_LANGS.len()] = [const { OnceLock::new() }; ALL_LANGS.len()];
        let language = self.language()?;
        Some(QUERIES[self as usize].get_or_init(|| {
            let source = match self {
                Lang::Rust => format!("{}\n{RUST_EXTRA}", tree_sitter_rust::TAGS_QUERY),
                Lang::Python => format!("{}\n{PYTHON_EXTRA}", tree_sitter_python::TAGS_QUERY),
                Lang::JavaScript => format!("{}\n{JS_EXTRA}", tree_sitter_javascript::TAGS_QUERY),
                // The TypeScript tags only add to the JavaScript ones.
                Lang::TypeScript | Lang::Tsx => format!(
                    "{}\n{}\n{JS_EXTRA}",
                    tree_sitter_javascript::TAGS_QUERY,
                    tree_sitter_typescript::TAGS_QUERY
                ),
                Lang::C => format!("{}\n{C_EXTRA}", tree_sitter_c::TAGS_QUERY),
                Lang::Cpp => format!("{}\n{C_EXTRA}\n{CPP_EXTRA}", tree_sitter_cpp::TAGS_QUERY),
                Lang::Go => format!("{}\n{GO_EXTRA}", tree_sitter_go::TAGS_QUERY),
                Lang::Java => format!("{}\n{JAVA_EXTRA}", tree_sitter_java::TAGS_QUERY),
                Lang::Lua => format!("{}\n{LUA_EXTRA}", tree_sitter_lua::TAGS_QUERY),
                Lang::Nix => NIX.to_string(),
                Lang::Swift => format!("{}\n{SWIFT_EXTRA}", tree_sitter_swift::TAGS_QUERY),
                Lang::Qml => QML.to_string(),
                Lang::Sql => SQL.to_string(),
                Lang::Bash => BASH.to_string(),
                Lang::Toml => TOML.to_string(),
                Lang::Yaml => YAML.to_string(),
                Lang::Json => JSON.to_string(),
                Lang::Markdown => unreachable!("Markdown has no grammar"),
            };
            Query::new(&language, &source).unwrap_or_else(|e| panic!("{} query: {e}", self.name()))
        }))
    }
}

const RUST_EXTRA: &str = r#"
(impl_item type: (_) @name) @definition.impl
(call_expression function: (scoped_identifier name: (identifier) @name)) @reference.call
(use_declaration argument: (_) @import)
(mod_item name: (identifier) @import.mod !body)
"#;

const PYTHON_EXTRA: &str = r#"
(import_statement name: (dotted_name) @import)
(import_statement name: (aliased_import name: (dotted_name) @import))
(import_from_statement module_name: (_) @import)
"#;

const JS_EXTRA: &str = r#"
(import_statement source: (string (string_fragment) @import))
(export_statement source: (string (string_fragment) @import))
"#;

// The C and C++ tags define but never reference.
const C_EXTRA: &str = r#"
(preproc_include path: (_) @import)
(call_expression function: (identifier) @name) @reference.call
(call_expression function: (field_expression field: (field_identifier) @name)) @reference.call
"#;

const CPP_EXTRA: &str = r#"
(call_expression function: (qualified_identifier name: (identifier) @name)) @reference.call
"#;

const GO_EXTRA: &str = r#"
(import_spec path: (_) @import)
"#;

const JAVA_EXTRA: &str = r#"
(import_declaration [(scoped_identifier) (identifier)] @import)
"#;

const LUA_EXTRA: &str = r#"
(function_call
  name: (_) @_f
  arguments: (arguments (string content: (string_content) @import))
  (#eq? @_f "require"))
"#;

// The Nix crate ships no tags constant. Attribute sets are the containers a
// module is read by (`programs.kitty = { .. }`).
const NIX: &str = r#"
(binding attrpath: (attrpath) @name expression: (function_expression)) @definition.function
(binding attrpath: (attrpath) @name expression: [(attrset_expression) (rec_attrset_expression)]) @definition.module
(apply_expression function: (variable_expression name: (identifier) @name)) @reference.call
(path_expression) @import
"#;

const SWIFT_EXTRA: &str = r#"
(import_declaration (identifier) @import)
(call_expression (simple_identifier) @name) @reference.call
"#;

const QML: &str = r#"
(ui_inline_component name: (identifier) @name) @definition.component
(ui_property name: (identifier) @name) @definition.property
(ui_signal name: (identifier) @name) @definition.signal
(function_declaration name: (identifier) @name) @definition.function
(ui_object_definition type_name: (_) @name) @reference.class
(call_expression function: (identifier) @name) @reference.call
(call_expression function: (member_expression property: (property_identifier) @name)) @reference.call
(ui_import source: (_) @import)
"#;

const SQL: &str = r#"
(create_table (object_reference name: (identifier) @name)) @definition.table
(create_view (object_reference name: (identifier) @name)) @definition.view
(create_function (object_reference name: (identifier) @name)) @definition.function
(create_type name: (identifier) @name) @definition.type
(relation (object_reference name: (identifier) @name)) @reference.class
"#;

const BASH: &str = r#"
(function_definition name: (word) @name) @definition.function
(command name: (command_name (word) @name)) @reference.call
(command
  name: (command_name (word) @_c)
  argument: [(word) (string) (raw_string)] @import
  (#match? @_c "^(source|\\.)$"))
"#;

// Only tables and the keys directly under them or the document: keys inside
// inline tables (`{ path = "..", version = ".." }`) would drown every search.
const TOML: &str = r#"
(table [(bare_key) (dotted_key) (quoted_key)] @name) @definition.table
(table_array_element [(bare_key) (dotted_key) (quoted_key)] @name) @definition.table
(table (pair [(bare_key) (dotted_key) (quoted_key)] @name) @definition.key)
(document (pair [(bare_key) (dotted_key) (quoted_key)] @name) @definition.key)
(pair (bare_key) @_k (string) @import (#eq? @_k "path"))
(pair (bare_key) @_k (array (string) @import) (#eq? @_k "members"))
"#;

const YAML: &str = r#"
(block_mapping_pair key: (flow_node) @name) @definition.key
"#;

// Two levels of keys, enough for package.json scripts and dependencies.
const JSON: &str = r#"
(document (object (pair key: (string (string_content) @name)) @definition.key))
(document (object (pair value: (object (pair key: (string (string_content) @name)) @definition.key))))
(pair
  key: (string (string_content) @_k)
  value: (string (string_content) @import)
  (#eq? @_k "extends"))
"#;

#[derive(Debug, Clone)]
pub struct Def {
    pub kind: String,
    pub name: String,
    pub qualname: String,
    pub line: u32,
    pub end_line: u32,
    pub parent: Option<usize>,
    pub signature: String,
}

#[derive(Debug, Clone)]
pub struct Ref {
    /// Index into `defs` of the innermost enclosing definition.
    pub from: Option<usize>,
    pub kind: String,
    pub name: String,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct Import {
    /// Unquoted import text; a Markdown wikilink keeps its `[[..]]`.
    pub target: String,
    /// A Rust `mod name;` declaration, which names a file rather than a path.
    pub is_mod: bool,
    pub line: u32,
}

#[derive(Debug, Default)]
pub struct Extraction {
    pub defs: Vec<Def>,
    pub refs: Vec<Ref>,
    pub imports: Vec<Import>,
}

struct RawDef {
    start: usize,
    end: usize,
    kind: String,
    name: String,
    line: u32,
    end_line: u32,
    signature: String,
}

pub fn extract(lang: Lang, path: &Path, source: &[u8]) -> Extraction {
    let mut out = match lang {
        Lang::Markdown => markdown(source),
        _ => tree(lang, source),
    };
    // A QML file is a component named after the file, used as `Name { }`.
    if lang == Lang::Qml {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            out.defs.push(Def {
                kind: "component".into(),
                name: stem.into(),
                qualname: stem.into(),
                line: 1,
                end_line: String::from_utf8_lossy(source).lines().count().max(1) as u32,
                parent: None,
                signature: stem.into(),
            });
        }
    }
    out
}

fn tree(lang: Lang, source: &[u8]) -> Extraction {
    let (Some(language), Some(query)) = (lang.language(), lang.query()) else {
        return Extraction::default();
    };
    let mut parser = Parser::new();
    parser.set_language(&language).expect("grammar matches the tree-sitter version");
    let Some(tree) = parser.parse(source, None) else {
        return Extraction::default();
    };
    let names = query.capture_names();
    let text = |node: tree_sitter::Node| String::from_utf8_lossy(&source[node.byte_range()]).into_owned();

    let mut defs: Vec<RawDef> = Vec::new();
    let mut refs: Vec<(usize, usize, String, String, u32)> = Vec::new();
    let mut imports = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    while let Some(m) = matches.next() {
        let mut name = None;
        let mut tagged = None;
        for capture in m.captures {
            let label = names[capture.index as usize];
            if label == "name" {
                name = Some(capture.node);
            } else if label == "import" || label == "import.mod" {
                imports.push(Import {
                    target: unquote(&text(capture.node)),
                    is_mod: label == "import.mod",
                    line: capture.node.start_position().row as u32 + 1,
                });
            } else if let Some(kind) = label.strip_prefix("definition.") {
                tagged = Some((true, kind, capture.node));
            } else if let Some(kind) = label.strip_prefix("reference.") {
                tagged = Some((false, kind, capture.node));
            }
        }
        let (Some(name), Some((is_def, kind, node))) = (name, tagged) else {
            continue;
        };
        let name = clean_name(&text(name));
        if name.is_empty() {
            continue;
        }
        if is_def {
            let start = node.start_byte();
            let end = node.end_byte();
            // Rust methods match both the method and the plain function pattern.
            if let Some(existing) = defs.iter_mut().find(|d| d.start == start && d.end == end) {
                if kind == "method" {
                    existing.kind = kind.to_string();
                }
                continue;
            }
            defs.push(RawDef {
                start,
                end,
                kind: kind.to_string(),
                name,
                line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                signature: signature(&text(node)),
            });
        } else {
            refs.push((
                node.start_byte(),
                node.end_byte(),
                kind.to_string(),
                name,
                node.start_position().row as u32 + 1,
            ));
        }
    }

    // Outer definitions first, so a parent always precedes its children.
    defs.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut out = Extraction {
        imports,
        ..Extraction::default()
    };
    let mut stack: Vec<usize> = Vec::new();
    for (index, raw) in defs.iter().enumerate() {
        while stack.last().is_some_and(|&top| defs[top].end <= raw.start) {
            stack.pop();
        }
        let parent = stack.last().copied();
        // Rust's tags call any function in a `{}` body a method, `mod` bodies included.
        let in_type = parent.is_some_and(|p| matches!(out.defs[p].kind.as_str(), "impl" | "interface" | "class"));
        let kind = if raw.kind == "method" && !in_type && lang == Lang::Rust {
            "function"
        } else {
            raw.kind.as_str()
        };
        let qualname = match parent {
            Some(p) => format!("{}{}{}", out.defs[p].qualname, lang.separator(), raw.name),
            None => raw.name.clone(),
        };
        out.defs.push(Def {
            kind: kind.to_string(),
            name: raw.name.clone(),
            qualname,
            line: raw.line,
            end_line: raw.end_line,
            parent,
            signature: raw.signature.clone(),
        });
        stack.push(index);
    }

    for (start, end, kind, name, line) in refs {
        // The last definition starting at or before the reference, walked up
        // to the first one that also ends after it.
        let mut from = defs.partition_point(|d| d.start <= start).checked_sub(1);
        while let Some(i) = from {
            if defs[i].end >= end {
                break;
            }
            from = out.defs[i].parent;
        }
        out.refs.push(Ref { from, kind, name, line });
    }
    out
}

/// Headings as nested definitions; relative links and `[[wikilinks]]` as
/// imports. Fenced code blocks are skipped.
fn markdown(source: &[u8]) -> Extraction {
    static LINK: OnceLock<Regex> = OnceLock::new();
    static WIKI: OnceLock<Regex> = OnceLock::new();
    let link = LINK.get_or_init(|| Regex::new(r#"\]\(\s*<?([^)\s>]+)>?(?:\s+"[^"]*")?\s*\)"#).unwrap());
    let wiki = WIKI.get_or_init(|| Regex::new(r"\[\[([^\]|#^]+)[^\]]*\]\]").unwrap());

    let text = String::from_utf8_lossy(source);
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Extraction::default();
    // Open headings as (level, index into defs), and every heading's level.
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut levels: Vec<usize> = Vec::new();
    let mut fence: Option<&str> = None;
    for (index, line) in lines.iter().enumerate() {
        let number = index as u32 + 1;
        let trimmed = line.trim_start();
        let marker = ["```", "~~~"].into_iter().find(|m| trimmed.starts_with(m));
        match (fence, marker) {
            (Some(open), Some(m)) if m == open => {
                fence = None;
                continue;
            }
            (None, Some(m)) => {
                fence = Some(m);
                continue;
            }
            (Some(_), _) => continue,
            (None, None) => {}
        }

        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        let rest = &trimmed[hashes..];
        if (1..=6).contains(&hashes) && line.len() - trimmed.len() < 4 && (rest.is_empty() || rest.starts_with(' ')) {
            let title = rest.trim().trim_end_matches('#').trim().to_string();
            if !title.is_empty() {
                while open.last().is_some_and(|&(level, _)| level >= hashes) {
                    open.pop();
                }
                let parent = open.last().map(|&(_, index)| index);
                let qualname = match parent {
                    Some(p) => format!("{}{}{title}", out.defs[p].qualname, Lang::Markdown.separator()),
                    None => title.clone(),
                };
                out.defs.push(Def {
                    kind: "heading".into(),
                    name: title,
                    qualname,
                    line: number,
                    end_line: number,
                    parent,
                    signature: trimmed.trim_end().to_string(),
                });
                open.push((hashes, out.defs.len() - 1));
                levels.push(hashes);
            }
        }

        for found in link.captures_iter(line) {
            let target = found[1].split(['#', '?']).next().unwrap_or("");
            if target.is_empty() || target.contains("://") || target.starts_with("mailto:") {
                continue;
            }
            out.imports.push(Import {
                target: target.replace("%20", " "),
                is_mod: false,
                line: number,
            });
        }
        for found in wiki.captures_iter(line) {
            let name = found[1].trim();
            let has_ext = Path::new(name).extension().is_some_and(|e| e.len() <= 5 && !e.to_string_lossy().contains(' '));
            let file = if has_ext { name.to_string() } else { format!("{name}.md") };
            out.imports.push(Import {
                target: format!("[[{file}]]"),
                is_mod: false,
                line: number,
            });
        }
    }

    // A heading's section runs until the next heading of its level or above.
    let total = lines.len() as u32;
    for i in 0..out.defs.len() {
        let next = (i + 1..out.defs.len()).find(|&j| levels[j] <= levels[i]);
        out.defs[i].end_line = next.map_or(total.max(out.defs[i].line), |j| out.defs[j].line - 1);
    }
    out
}

/// `Foo<T>`, `crate::Foo` and `"Foo"` name `Foo`.
fn clean_name(raw: &str) -> String {
    let base = raw.split('<').next().unwrap_or(raw);
    let base = base.rsplit("::").next().unwrap_or(base);
    unquote(base.trim())
}

/// Strips quotes or angle brackets wrapping the whole text, and only those:
/// Nix's `fileSystems."/"` keeps its own.
fn unquote(raw: &str) -> String {
    let raw = raw.trim();
    let mut chars = raw.chars();
    let wrapped = match (chars.next(), chars.next_back()) {
        (Some(open), Some(close)) => matches!((open, close), ('"', '"') | ('\'', '\'') | ('`', '`') | ('<', '>')),
        _ => false,
    };
    if wrapped {
        raw[1..raw.len() - 1].to_string()
    } else {
        raw.to_string()
    }
}

fn signature(definition: &str) -> String {
    let line = definition.lines().next().unwrap_or("").trim();
    let line = line.trim_end_matches('{').trim_end();
    match line.char_indices().nth(160) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lang: Lang, file: &str, source: &str) -> Extraction {
        extract(lang, Path::new(file), source.as_bytes())
    }

    fn def<'a>(out: &'a Extraction, name: &str) -> &'a Def {
        out.defs.iter().find(|d| d.name == name).unwrap_or_else(|| panic!("no def {name}: {:?}", out.defs))
    }

    fn targets(out: &Extraction) -> Vec<&str> {
        out.imports.iter().map(|i| i.target.as_str()).collect()
    }

    fn calls(out: &Extraction) -> Vec<&str> {
        out.refs.iter().filter(|r| r.kind == "call").map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn every_query_compiles() {
        for lang in ALL_LANGS {
            assert_eq!(lang.query().is_some(), lang != Lang::Markdown, "{}", lang.name());
        }
    }

    #[test]
    fn rust_methods_nest_in_their_impl_and_calls_know_their_caller() {
        let source = "mod util;\nuse std::path::Path;\nstruct Foo;\nimpl Foo {\n    fn bar(&self) { helper(); util::deep(); }\n}\nfn helper() {}\nmod tests {\n    fn case() {}\n}\n";
        let out = run(Lang::Rust, "lib.rs", source);
        let case = def(&out, "case");
        assert_eq!((case.kind.as_str(), case.qualname.as_str()), ("function", "tests::case"));
        let bar = out.defs.iter().position(|d| d.name == "bar").unwrap();
        assert_eq!(out.defs[bar].kind, "method");
        assert_eq!(out.defs[bar].qualname, "Foo::bar");
        let from: Vec<_> = out.refs.iter().filter(|r| r.kind == "call").map(|r| (r.name.as_str(), r.from)).collect();
        assert!(from.contains(&("helper", Some(bar))) && from.contains(&("deep", Some(bar))));
        assert!(out.imports.iter().any(|i| i.is_mod && i.target == "util"));
        assert!(targets(&out).contains(&"std::path::Path"));
    }

    #[test]
    fn typescript_sees_javascript_definitions_and_imports() {
        let out = run(
            Lang::TypeScript,
            "a.ts",
            "import { a } from './a';\nexport function run(x: number) { return a(x); }\ninterface Shape {}\n",
        );
        assert_eq!(def(&out, "run").kind, "function");
        assert_eq!(def(&out, "Shape").kind, "interface");
        assert!(calls(&out).contains(&"a"));
        assert_eq!(targets(&out), ["./a"]);
    }

    #[test]
    fn python_classes_methods_and_imports() {
        let out = run(
            Lang::Python,
            "m.py",
            "from pkg import mod\nimport os.path\nclass A:\n    def f(self):\n        g()\ndef g():\n    pass\n",
        );
        let f = out.defs.iter().position(|d| d.name == "f").unwrap();
        assert_eq!(out.defs[f].qualname, "A.f");
        assert!(out.refs.iter().any(|r| r.name == "g" && r.from == Some(f)));
        assert!(targets(&out).contains(&"pkg") && targets(&out).contains(&"os.path"));
    }

    #[test]
    fn c_family_includes_and_calls() {
        let out = run(Lang::C, "a.c", "#include \"util.h\"\n#include <stdio.h>\nint main(void) { helper(1); return 0; }\n");
        assert_eq!(def(&out, "main").kind, "function");
        assert_eq!(targets(&out), ["util.h", "stdio.h"]);
        assert_eq!(calls(&out), ["helper"]);
        let cpp = run(Lang::Cpp, "a.cpp", "namespace n { int f() { return ns::g(); } }\nclass K { void m(); };\n");
        assert!(calls(&cpp).contains(&"g"));
        assert_eq!(def(&cpp, "K").kind, "class");
    }

    #[test]
    fn go_java_swift_imports_and_definitions() {
        let go = run(Lang::Go, "a.go", "package a\nimport \"fmt\"\nfunc Run() { fmt.Println(x()) }\nfunc x() int { return 1 }\n");
        assert_eq!(targets(&go), ["fmt"]);
        assert!(calls(&go).contains(&"x"));
        assert_eq!(def(&go, "Run").kind, "function");
        let java = run(Lang::Java, "A.java", "import java.util.List;\nclass A { void m() { n(); } void n() {} }\n");
        assert_eq!(targets(&java), ["java.util.List"]);
        assert_eq!(def(&java, "m").qualname, "A.m");
        let swift = run(Lang::Swift, "a.swift", "import Foundation\nfunc go() { other() }\nfunc other() {}\n");
        assert_eq!(targets(&swift), ["Foundation"]);
        assert!(calls(&swift).contains(&"other"));
    }

    #[test]
    fn scripting_languages_link_their_files() {
        let lua = run(Lang::Lua, "init.lua", "local m = require('core.keys')\nlocal function setup() m.go() end\n");
        assert_eq!(targets(&lua), ["core.keys"]);
        assert_eq!(def(&lua, "setup").kind, "function");
        let nix = run(
            Lang::Nix,
            "home.nix",
            "{ imports = [ ./hardware.nix ../common ];\n  programs.kitty = { enable = true; };\n  mk = x: wrap x; }\n",
        );
        assert_eq!(targets(&nix), ["./hardware.nix", "../common"]);
        let mounts = run(Lang::Nix, "hw.nix", "{ fileSystems.\"/\" = { device = \"x\"; }; }\n");
        assert_eq!(mounts.defs[0].name, "fileSystems.\"/\"");
        assert_eq!(def(&nix, "programs.kitty").kind, "module");
        assert_eq!(def(&nix, "mk").kind, "function");
        assert!(calls(&nix).contains(&"wrap"));
        let sh = run(Lang::Bash, "a.sh", "source ./lib.sh\n. \"$DIR/x.sh\"\nbuild() { make all; }\nbuild\n");
        assert!(targets(&sh).contains(&"./lib.sh"));
        assert_eq!(def(&sh, "build").kind, "function");
        assert!(calls(&sh).contains(&"build") && calls(&sh).contains(&"make"));
    }

    #[test]
    fn qml_files_are_components_used_by_type_name() {
        let out = run(
            Lang::Qml,
            "Bar.qml",
            "import QtQuick\nimport \"../widgets\"\nItem {\n    property int size: 3\n    signal clicked()\n    function go() { helper() }\n    Pill { }\n}\n",
        );
        assert_eq!(def(&out, "Bar").kind, "component");
        assert_eq!(def(&out, "size").kind, "property");
        assert_eq!(def(&out, "clicked").kind, "signal");
        assert!(out.refs.iter().any(|r| r.kind == "class" && r.name == "Pill"));
        assert!(calls(&out).contains(&"helper"));
        assert_eq!(targets(&out), ["QtQuick", "../widgets"]);
    }

    #[test]
    fn sql_tables_and_their_uses() {
        let out = run(Lang::Sql, "s.sql", "CREATE TABLE users (id int);\nSELECT * FROM users;\n");
        assert_eq!(def(&out, "users").kind, "table");
        assert!(out.refs.iter().any(|r| r.kind == "class" && r.name == "users"));
    }

    #[test]
    fn config_keys_nest_and_paths_link() {
        let toml = run(
            Lang::Toml,
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\"]\n[dependencies]\nfoo = { path = \"../foo\", version = \"1\" }\n",
        );
        assert_eq!(def(&toml, "members").qualname, "workspace.members");
        assert_eq!(def(&toml, "foo").qualname, "dependencies.foo");
        assert!(!toml.defs.iter().any(|d| d.name == "version"));
        assert_eq!(targets(&toml), ["crates/a", "../foo"]);

        let yaml = run(Lang::Yaml, "ci.yml", "jobs:\n  build:\n    runs-on: ubuntu\n");
        assert_eq!(def(&yaml, "runs-on").qualname, "jobs.build.runs-on");

        let json = run(Lang::Json, "package.json", "{\"scripts\": {\"dev\": \"vite\"}, \"extends\": \"./base.json\", \"a\": {\"b\": {\"deep\": 1}}}");
        assert_eq!(def(&json, "dev").qualname, "scripts.dev");
        assert!(!json.defs.iter().any(|d| d.name == "deep"));
        assert_eq!(targets(&json), ["./base.json"]);
        assert!(Lang::from_path(Path::new("web/package-lock.json")).is_none());
    }

    #[test]
    fn markdown_headings_nest_and_links_skip_code() {
        let source = "# Guide\nSee [setup](./setup.md#run) and [[Ideas|my ideas]] and ![[diagram.png]].\n## Install\ntext [site](https://x.dev)\n```sh\n# not a heading [x](./code.md)\n```\n### Linux\n## Use\n# Other\n";
        let out = run(Lang::Markdown, "README.md", source);
        let heads: Vec<_> = out.defs.iter().map(|d| (d.qualname.as_str(), d.line, d.end_line)).collect();
        assert_eq!(
            heads,
            [
                ("Guide", 1, 9),
                ("Guide > Install", 3, 8),
                ("Guide > Install > Linux", 8, 8),
                ("Guide > Use", 9, 9),
                ("Other", 10, 10)
            ]
        );
        assert_eq!(targets(&out), ["./setup.md", "[[Ideas.md]]", "[[diagram.png]]"]);
    }
}
