//! A code graph of one workspace, kept in SQLite and refreshed incrementally,
//! so agents ask for outlines, callers and paths instead of rereading files.
//!
//! Every query is meant to follow a [`Graph::refresh`]: it stats each source
//! file and reparses only those whose content hash changed.

mod extract;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

pub use extract::Lang;
use extract::{extract, Extraction};

/// Larger files are generated or minified bundles, not code worth a node;
/// hand-written files reach a megabyte or more (agents-window's app.rs).
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Bump it whenever extraction changes too: unchanged files are never reparsed.
const SCHEMA_VERSION: i64 = 2;
const SCHEMA: &str = "
CREATE TABLE files(
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    lang TEXT NOT NULL,
    hash TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL);
CREATE INDEX files_hash ON files(hash);
CREATE TABLE symbols(
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    parent_id INTEGER,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    qualname TEXT NOT NULL,
    line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    signature TEXT NOT NULL);
CREATE INDEX symbols_file ON symbols(file_id);
CREATE INDEX symbols_name ON symbols(name);
CREATE INDEX symbols_qualname ON symbols(qualname);
CREATE INDEX symbols_parent ON symbols(parent_id);
CREATE TABLE refs(
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    from_id INTEGER,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    line INTEGER NOT NULL);
CREATE INDEX refs_name ON refs(name);
CREATE INDEX refs_from ON refs(from_id);
CREATE TABLE imports(
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    target TEXT NOT NULL,
    target_path TEXT,
    line INTEGER NOT NULL);
CREATE INDEX imports_file ON imports(file_id);
CREATE INDEX imports_target_path ON imports(target_path);
";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("graph database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct Graph {
    conn: Connection,
    root: PathBuf,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RefreshStats {
    pub files: usize,
    pub parsed: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub id: i64,
    pub kind: String,
    pub name: String,
    pub qualname: String,
    pub path: String,
    pub line: u32,
    pub end_line: u32,
    pub signature: String,
}

#[derive(Debug, Clone)]
pub struct OutlineEntry {
    pub depth: usize,
    pub symbol: Symbol,
}

#[derive(Debug, Clone)]
pub struct Outline {
    pub path: String,
    pub lang: String,
    pub symbols: Vec<OutlineEntry>,
    /// Import text and the workspace file it resolves to, when it does.
    pub imports: Vec<(String, Option<String>)>,
    pub imported_by: Vec<String>,
    /// Other files with byte-identical content.
    pub identical: Vec<String>,
}

/// A use of a symbol: where it happens and inside which definition.
#[derive(Debug, Clone)]
pub struct Site {
    pub within: Option<String>,
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct Explanation {
    pub symbol: Symbol,
    pub parent: Option<String>,
    pub members: Vec<Symbol>,
    /// Matched by name, so they are shared by every definition of that name.
    pub callers: Vec<Site>,
    /// Called names and how many definitions each resolves to.
    pub callees: Vec<(String, usize)>,
    pub implementations: Vec<Site>,
    /// Uses as a type, class, table or QML component, matched by name.
    pub used_by: Vec<Site>,
    pub same_name: usize,
}

#[derive(Debug, Clone)]
pub struct Stats {
    pub files: i64,
    pub symbols: i64,
    pub refs: i64,
    pub imports: i64,
    pub by_lang: Vec<(String, i64)>,
}

/// One file of the workspace as a node of the file graph.
#[derive(Debug, Clone)]
pub struct FileNode {
    pub id: i64,
    pub path: String,
    pub lang: String,
    pub symbols: i64,
}

/// Source file id, target file id and why they are linked.
pub type FileLink = (i64, i64, LinkKind);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Import,
    /// A call whose name has exactly one definition, in another file.
    Call,
    /// A type, class, table or QML component used by its unique name.
    Uses,
}

impl Graph {
    /// Opens the workspace's graph in the user's cache directory, shared by
    /// every process that opens the same workspace.
    pub fn open(workspace: &Path) -> Result<Graph> {
        let root = workspace.canonicalize()?;
        let db = default_db_path(&root);
        Graph::open_at(&root, &db)
    }

    pub fn open_at(workspace: &Path, db: &Path) -> Result<Graph> {
        let root = workspace.canonicalize()?;
        if let Some(dir) = db.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(db)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            // The graph is a cache of the sources: rebuild rather than migrate.
            conn.execute_batch(
                "DROP TABLE IF EXISTS imports; DROP TABLE IF EXISTS refs;
                 DROP TABLE IF EXISTS symbols; DROP TABLE IF EXISTS files;",
            )?;
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Graph { conn, root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Brings the graph up to date with the workspace, honoring `.gitignore`.
    pub fn refresh(&mut self) -> Result<RefreshStats> {
        let mut stats = RefreshStats::default();
        let mut seen = HashSet::new();
        let tx = self.conn.transaction()?;
        let walk = ignore::WalkBuilder::new(&self.root).require_git(false).build();
        for entry in walk.flatten() {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = entry.path();
            let Some(lang) = Lang::from_path(path) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let Some(rel) = relative(&self.root, path) else {
                continue;
            };
            stats.files += 1;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos() as i64);
            let size = meta.len() as i64;
            seen.insert(rel.clone());

            let known: Option<(i64, i64, i64, String)> = tx
                .query_row(
                    "SELECT id, size, mtime, hash FROM files WHERE path = ?1",
                    [&rel],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            if known.as_ref().is_some_and(|k| k.1 == size && k.2 == mtime) {
                continue;
            }
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let hash = format!("{:x}", Sha256::digest(&bytes));
            if let Some((id, _, _, old)) = &known {
                if *old == hash {
                    tx.execute("UPDATE files SET size = ?1, mtime = ?2 WHERE id = ?3", params![size, mtime, id])?;
                    continue;
                }
                tx.execute("DELETE FROM files WHERE id = ?1", [id])?;
            }
            tx.execute(
                "INSERT INTO files(path, lang, hash, size, mtime) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![rel, lang.name(), hash, size, mtime],
            )?;
            let file_id = tx.last_insert_rowid();
            store(&tx, &self.root, file_id, &rel, &extract(lang, path, &bytes))?;
            stats.parsed += 1;
        }

        let known: Vec<(i64, String)> = tx
            .prepare("SELECT id, path FROM files")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, path) in known {
            if !seen.contains(&path) {
                tx.execute("DELETE FROM files WHERE id = ?1", [id])?;
                stats.removed += 1;
            }
        }
        if stats.parsed + stats.removed > 0 {
            resolve_wikilinks(&tx)?;
        }
        tx.commit()?;
        Ok(stats)
    }

    pub fn outline(&self, path: &str) -> Result<Option<Outline>> {
        let path = path.trim_start_matches("./");
        let Some((file_id, lang, hash)): Option<(i64, String, String)> = self
            .conn
            .query_row("SELECT id, lang, hash FROM files WHERE path = ?1", [path], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?
        else {
            return Ok(None);
        };

        let mut stmt = self.conn.prepare(&format!(
            "{SYMBOL_SELECT} WHERE s.file_id = ?1 ORDER BY s.line, s.end_line DESC"
        ))?;
        let rows: Vec<(Symbol, Option<i64>)> = stmt
            .query_map([file_id], |row| Ok((symbol(row)?, row.get(7)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut depth_of: HashMap<i64, usize> = HashMap::new();
        let mut symbols = Vec::with_capacity(rows.len());
        for (symbol, parent) in rows {
            let depth = parent.and_then(|p| depth_of.get(&p)).map_or(0, |d| d + 1);
            depth_of.insert(symbol.id, depth);
            symbols.push(OutlineEntry { depth, symbol });
        }

        let imports = self
            .conn
            .prepare("SELECT target, target_path FROM imports WHERE file_id = ?1 ORDER BY line")?
            .query_map([file_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let imported_by = self.strings(
            "SELECT DISTINCT f.path FROM imports i JOIN files f ON f.id = i.file_id
             WHERE i.target_path = ?1 ORDER BY f.path",
            path,
        )?;
        let identical = self
            .conn
            .prepare("SELECT path FROM files WHERE hash = ?1 AND id != ?2 ORDER BY path")?
            .query_map(params![hash, file_id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(Outline {
            path: path.to_string(),
            lang,
            symbols,
            imports,
            imported_by,
            identical,
        }))
    }

    /// Definitions named exactly `name` (or its qualified name); failing that,
    /// those whose name contains it.
    pub fn find(&self, name: &str, limit: usize) -> Result<(Vec<Symbol>, bool)> {
        let exact = self.exact(name, limit)?;
        if !exact.is_empty() {
            return Ok((exact, true));
        }
        let pattern = format!("%{}%", escape_like(name));
        let found = self
            .conn
            .prepare(&format!(
                "{SYMBOL_SELECT} WHERE s.name LIKE ?1 ESCAPE '\\' ORDER BY length(s.name), f.path, s.line LIMIT ?2"
            ))?
            .query_map(params![pattern, limit as i64], symbol)?
            .collect::<rusqlite::Result<_>>()?;
        Ok((found, false))
    }

    pub fn explain(&self, name: &str, limit: usize) -> Result<Vec<Explanation>> {
        let defs = self.exact(name, limit)?;
        let mut out = Vec::with_capacity(defs.len());
        let same_name = defs.len();
        for symbol in defs {
            let short = symbol.name.clone();
            let parent = self
                .conn
                .query_row(
                    "SELECT p.qualname FROM symbols s JOIN symbols p ON p.id = s.parent_id WHERE s.id = ?1",
                    [symbol.id],
                    |row| row.get(0),
                )
                .optional()?;
            let members = self
                .conn
                .prepare(&format!("{SYMBOL_SELECT} WHERE s.parent_id = ?1 ORDER BY s.line LIMIT 60"))?
                .query_map([symbol.id], self::symbol)?
                .collect::<rusqlite::Result<_>>()?;
            let callers = self.sites("call", &short)?;
            let implementations = self.sites("implementation", &short)?;
            let mut used_by = self.sites("class", &short)?;
            used_by.extend(self.sites("type", &short)?);
            let callees = self
                .conn
                .prepare(
                    "SELECT r.name, (SELECT count(*) FROM symbols s WHERE s.name = r.name)
                     FROM refs r WHERE r.from_id = ?1 AND r.kind = 'call'
                     GROUP BY r.name ORDER BY min(r.line) LIMIT 60",
                )?
                .query_map([symbol.id], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as usize)))?
                .collect::<rusqlite::Result<_>>()?;
            out.push(Explanation {
                symbol,
                parent,
                members,
                callers,
                callees,
                implementations,
                used_by,
                same_name,
            });
        }
        Ok(out)
    }

    /// The shortest call chain from a definition named `from` to one named
    /// `to`, following calls by name. Each step carries how many definitions
    /// share its name: above 1, that hop is a guess.
    pub fn path(&self, from: &str, to: &str, max_depth: usize) -> Result<Option<Vec<(Symbol, usize)>>> {
        let starts = self.exact(from, 50)?;
        let goals: HashSet<i64> = self.exact(to, 50)?.into_iter().map(|s| s.id).collect();
        if starts.is_empty() || goals.is_empty() {
            return Ok(None);
        }
        let mut callees = self.conn.prepare(
            "SELECT DISTINCT s.id FROM refs r JOIN symbols s ON s.name = r.name
             WHERE r.from_id = ?1 AND r.kind = 'call'",
        )?;
        let mut came_from: HashMap<i64, Option<i64>> = HashMap::new();
        let mut queue = VecDeque::new();
        for start in &starts {
            came_from.insert(start.id, None);
            queue.push_back((start.id, 0));
        }
        let mut found = None;
        while let Some((id, depth)) = queue.pop_front() {
            if goals.contains(&id) {
                found = Some(id);
                break;
            }
            if depth == max_depth || came_from.len() > 50_000 {
                continue;
            }
            let next: Vec<i64> = callees.query_map([id], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
            for n in next {
                if let std::collections::hash_map::Entry::Vacant(slot) = came_from.entry(n) {
                    slot.insert(Some(id));
                    queue.push_back((n, depth + 1));
                }
            }
        }
        let Some(mut id) = found else {
            return Ok(None);
        };
        let mut chain = vec![id];
        while let Some(Some(prev)) = came_from.get(&id) {
            chain.push(*prev);
            id = *prev;
        }
        chain.reverse();
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.kind, s.qualname, f.path, s.line, s.end_line, s.signature, s.parent_id, s.name,
                    (SELECT count(*) FROM symbols o WHERE o.name = s.name)
             FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1",
        )?;
        let steps = chain
            .into_iter()
            .map(|id| stmt.query_row([id], |row| Ok((symbol(row)?, row.get::<_, i64>(9)? as usize))))
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(steps))
    }

    pub fn stats(&self) -> Result<Stats> {
        let count = |table: &str| -> Result<i64> {
            Ok(self.conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get(0))?)
        };
        let by_lang = self
            .conn
            .prepare("SELECT lang, count(*) FROM files GROUP BY lang ORDER BY count(*) DESC")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Stats {
            files: count("files")?,
            symbols: count("symbols")?,
            refs: count("refs")?,
            imports: count("imports")?,
            by_lang,
        })
    }

    /// Files as nodes and the links between them, for drawing the graph.
    pub fn file_graph(&self) -> Result<(Vec<FileNode>, Vec<FileLink>)> {
        let nodes = self
            .conn
            .prepare(
                "SELECT f.id, f.path, f.lang, (SELECT count(*) FROM symbols s WHERE s.file_id = f.id)
                 FROM files f ORDER BY f.path",
            )?
            .query_map([], |row| {
                Ok(FileNode {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    lang: row.get(2)?,
                    symbols: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut links: Vec<(i64, i64, LinkKind)> = self
            .conn
            .prepare(
                "SELECT DISTINCT i.file_id, f.id FROM imports i JOIN files f ON f.path = i.target_path
                 WHERE i.file_id != f.id",
            )?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, LinkKind::Import)))?
            .collect::<rusqlite::Result<_>>()?;
        let used = self
            .conn
            .prepare(
                "SELECT DISTINCT r.file_id, u.file_id, r.kind = 'call' FROM refs r
                 JOIN (SELECT name, min(file_id) AS file_id FROM symbols GROUP BY name HAVING count(*) = 1) u
                   ON u.name = r.name
                 WHERE r.kind IN ('call', 'class', 'type') AND r.file_id != u.file_id
                 ORDER BY 3 DESC",
            )?
            .query_map([], |row| {
                let kind = if row.get(2)? { LinkKind::Call } else { LinkKind::Uses };
                Ok((row.get(0)?, row.get(1)?, kind))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // One link per pair of files: an import explains the rest, a call a use.
        let mut linked: HashSet<(i64, i64)> = links.iter().map(|l| (l.0, l.1)).collect();
        links.extend(used.into_iter().filter(|l| linked.insert((l.0, l.1))));
        Ok((nodes, links))
    }

    fn exact(&self, name: &str, limit: usize) -> Result<Vec<Symbol>> {
        Ok(self
            .conn
            .prepare(&format!(
                "{SYMBOL_SELECT} WHERE s.name = ?1 OR s.qualname = ?1 ORDER BY f.path, s.line LIMIT ?2"
            ))?
            .query_map(params![name, limit as i64], symbol)?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn sites(&self, kind: &str, name: &str) -> Result<Vec<Site>> {
        Ok(self
            .conn
            .prepare(
                "SELECT s.qualname, f.path, r.line FROM refs r JOIN files f ON f.id = r.file_id
                 LEFT JOIN symbols s ON s.id = r.from_id
                 WHERE r.kind = ?1 AND r.name = ?2 ORDER BY f.path, r.line LIMIT 60",
            )?
            .query_map(params![kind, name], |row| {
                Ok(Site {
                    within: row.get(0)?,
                    path: row.get(1)?,
                    line: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn strings(&self, sql: &str, arg: &str) -> Result<Vec<String>> {
        Ok(self
            .conn
            .prepare(sql)?
            .query_map([arg], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
}

const SYMBOL_SELECT: &str = "SELECT s.id, s.kind, s.qualname, f.path, s.line, s.end_line, s.signature, s.parent_id, s.name
     FROM symbols s JOIN files f ON f.id = s.file_id";

fn symbol(row: &rusqlite::Row) -> rusqlite::Result<Symbol> {
    Ok(Symbol {
        id: row.get(0)?,
        kind: row.get(1)?,
        qualname: row.get(2)?,
        path: row.get(3)?,
        line: row.get(4)?,
        end_line: row.get(5)?,
        signature: row.get(6)?,
        name: row.get(8)?,
    })
}

fn store(tx: &rusqlite::Transaction, root: &Path, file_id: i64, rel: &str, out: &Extraction) -> Result<()> {
    let mut ids = Vec::with_capacity(out.defs.len());
    let mut insert = tx.prepare_cached(
        "INSERT INTO symbols(file_id, parent_id, kind, name, qualname, line, end_line, signature)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for def in &out.defs {
        let parent: Option<i64> = def.parent.map(|p| ids[p]);
        insert.execute(params![
            file_id,
            parent,
            def.kind,
            def.name,
            def.qualname,
            def.line,
            def.end_line,
            def.signature
        ])?;
        ids.push(tx.last_insert_rowid());
    }
    let mut insert = tx.prepare_cached("INSERT INTO refs(file_id, from_id, kind, name, line) VALUES (?1, ?2, ?3, ?4, ?5)")?;
    for r in &out.refs {
        insert.execute(params![file_id, r.from.map(|i| ids[i]), r.kind, r.name, r.line])?;
    }
    let mut insert =
        tx.prepare_cached("INSERT INTO imports(file_id, target, target_path, line) VALUES (?1, ?2, ?3, ?4)")?;
    for import in &out.imports {
        let resolved = resolve_import(root, rel, &import.target, import.is_mod);
        insert.execute(params![file_id, import.target, resolved, import.line])?;
    }
    Ok(())
}

/// The workspace file an import names, when it names one by path.
// shortcut: resolved against the disk when the importer is parsed; a target
// created later links once the importer changes again.
fn resolve_import(root: &Path, importer: &str, target: &str, is_mod: bool) -> Option<String> {
    let importer = Path::new(importer);
    let dir = importer.parent().unwrap_or(Path::new(""));
    // Interpolated (`$DIR/x.sh`, `${./x}`) or absolute targets are not ours to follow.
    if target.contains('$') || target.starts_with('~') {
        return None;
    }
    let candidates: Vec<PathBuf> = match Lang::from_path(importer)? {
        Lang::Rust if is_mod => {
            let stem = importer.file_stem()?.to_str()?;
            let base = if matches!(stem, "main" | "lib" | "mod") { dir.to_path_buf() } else { dir.join(stem) };
            vec![base.join(format!("{target}.rs")), base.join(target).join("mod.rs")]
        }
        // Module names, not paths: these link through the symbols they name.
        Lang::Rust | Lang::Go | Lang::Java | Lang::Swift | Lang::Sql | Lang::Yaml => return None,
        Lang::C | Lang::Cpp | Lang::Bash => vec![dir.join(target), PathBuf::from(target)],
        Lang::Lua => {
            let module = target.replace('.', "/");
            [PathBuf::new(), dir.to_path_buf()]
                .into_iter()
                .flat_map(|b| [b.join(format!("{module}.lua")), b.join(&module).join("init.lua")])
                .collect()
        }
        Lang::Nix => {
            let path = dir.join(target);
            vec![path.join("default.nix"), path]
        }
        // A QML string import names a directory of components or a script.
        Lang::Qml => vec![dir.join(target)],
        Lang::Toml => {
            let path = dir.join(target);
            vec![path.join("Cargo.toml"), path.join("pyproject.toml"), path]
        }
        Lang::Json => {
            let path = dir.join(target);
            vec![PathBuf::from(format!("{}.json", path.display())), path]
        }
        // Wikilinks resolve by name across the workspace, after every refresh.
        Lang::Markdown if target.starts_with("[[") => return None,
        Lang::Markdown => match target.strip_prefix('/') {
            Some(from_root) => vec![PathBuf::from(from_root)],
            None => vec![dir.join(target)],
        },
        Lang::Python => {
            let dots = target.chars().take_while(|&c| c == '.').count();
            let module = target[dots..].replace('.', "/");
            let bases: Vec<PathBuf> = if dots > 0 {
                let mut base = dir.to_path_buf();
                for _ in 1..dots {
                    base = base.parent()?.to_path_buf();
                }
                vec![base]
            } else {
                vec![PathBuf::new(), dir.to_path_buf()]
            };
            bases
                .into_iter()
                .flat_map(|b| [b.join(format!("{module}.py")), b.join(&module).join("__init__.py")])
                .collect()
        }
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => {
            if !target.starts_with('.') {
                return None;
            }
            let base = dir.join(target);
            let mut c = vec![base.clone()];
            for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs"] {
                c.push(PathBuf::from(format!("{}.{ext}", base.display())));
            }
            for ext in ["ts", "tsx", "js", "jsx"] {
                c.push(base.join(format!("index.{ext}")));
            }
            c
        }
    };
    candidates
        .into_iter()
        .filter_map(|c| normalize(&c))
        .find(|c| root.join(c).is_file())
}

/// Points each `[[wikilink]]` at the shortest indexed path ending in its name,
/// as Obsidian does. Runs whenever files come or go, so links to notes created
/// later resolve too.
fn resolve_wikilinks(tx: &rusqlite::Transaction) -> Result<()> {
    let links: Vec<(i64, String)> = tx
        .prepare("SELECT rowid, target FROM imports WHERE target LIKE '[[%'")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut find = tx.prepare(
        "SELECT path FROM files WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\' ORDER BY length(path) LIMIT 1",
    )?;
    let mut update = tx.prepare("UPDATE imports SET target_path = ?1 WHERE rowid = ?2")?;
    for (rowid, target) in links {
        let name = target.trim_start_matches("[[").trim_end_matches("]]");
        let path: Option<String> = find
            .query_row(params![name, format!("%/{}", escape_like(name))], |row| row.get(0))
            .optional()?;
        update.execute(params![path, rowid])?;
    }
    Ok(())
}

fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// The innermost directory holding `start` whose graph already exists, and
/// that graph's database. Nothing is created: callers that must not index on
/// their own (hooks) only act where the user opted in.
pub fn find_existing(start: &Path) -> Option<(PathBuf, PathBuf)> {
    let start = start.canonicalize().ok()?;
    start
        .ancestors()
        .map(|dir| (dir.to_path_buf(), default_db_path(dir)))
        .find(|(_, db)| db.is_file())
}

/// Lexically resolves `.` and `..`; `None` when the path climbs out of the root.
fn normalize(path: &Path) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    normalize(path.strip_prefix(root).ok()?)
}

/// `$XDG_CACHE_HOME/openade/graph/<hash of the workspace path>.db`.
pub fn default_db_path(root: &Path) -> PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    let key = format!("{:x}", Sha256::digest(root.to_string_lossy().as_bytes()));
    cache.join("openade").join("graph").join(format!("{}.db", &key[..16]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn workspace(files: &[(&str, &str)]) -> (tempfile::TempDir, Graph) {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let db = dir.path().join(".cache/graph.db");
        fs::write(dir.path().join(".gitignore"), ".cache/\nignored/\n").unwrap();
        let graph = Graph::open_at(dir.path(), &db).unwrap();
        (dir, graph)
    }

    const MAIN: &str = "mod util;\nfn main() {\n    util::helper();\n    run();\n}\nfn run() {}\n";
    const UTIL: &str = "pub fn helper() {\n    inner();\n}\nfn inner() {}\npub struct Foo;\nimpl Foo {\n    pub fn bar(&self) {}\n}\n";

    #[test]
    fn refresh_parses_only_what_changed() {
        let (dir, mut graph) = workspace(&[("src/main.rs", MAIN), ("src/util.rs", UTIL), ("ignored/x.rs", "fn x() {}")]);
        assert_eq!(graph.refresh().unwrap(), RefreshStats { files: 2, parsed: 2, removed: 0 });
        assert_eq!(graph.refresh().unwrap().parsed, 0);

        fs::write(dir.path().join("src/util.rs"), format!("{UTIL}fn added() {{}}\n")).unwrap();
        assert_eq!(graph.refresh().unwrap().parsed, 1);
        assert_eq!(graph.find("added", 5).unwrap().0.len(), 1);

        fs::remove_file(dir.path().join("src/main.rs")).unwrap();
        let stats = graph.refresh().unwrap();
        assert_eq!((stats.files, stats.removed), (1, 1));
        assert!(graph.find("run", 5).unwrap().0.is_empty());
    }

    #[test]
    fn outline_links_imports_both_ways_and_flags_copies() {
        let (_dir, mut graph) = workspace(&[("src/main.rs", MAIN), ("src/util.rs", UTIL), ("copy/util.rs", UTIL)]);
        graph.refresh().unwrap();
        let main = graph.outline("src/main.rs").unwrap().unwrap();
        assert_eq!(main.imports, vec![("util".to_string(), Some("src/util.rs".to_string()))]);
        let util = graph.outline("./src/util.rs").unwrap().unwrap();
        assert_eq!(util.imported_by, vec!["src/main.rs"]);
        assert_eq!(util.identical, vec!["copy/util.rs"]);
        let bar = util.symbols.iter().find(|e| e.symbol.qualname == "Foo::bar").unwrap();
        assert_eq!(bar.depth, 1);
        assert!(graph.outline("missing.rs").unwrap().is_none());
    }

    #[test]
    fn explain_and_path_follow_calls() {
        let (_dir, mut graph) = workspace(&[("src/main.rs", MAIN), ("src/util.rs", UTIL)]);
        graph.refresh().unwrap();
        let helper = &graph.explain("helper", 5).unwrap()[0];
        assert_eq!(helper.callers.len(), 1);
        assert_eq!(helper.callers[0].within.as_deref(), Some("main"));
        assert_eq!(helper.callees, vec![("inner".to_string(), 1)]);

        let chain = graph.path("main", "inner", 6).unwrap().unwrap();
        let names: Vec<_> = chain.iter().map(|(s, same)| (s.qualname.as_str(), *same)).collect();
        assert_eq!(names, [("main", 1), ("helper", 1), ("inner", 1)]);
        assert!(graph.path("inner", "main", 6).unwrap().is_none());

        let foo = &graph.explain("Foo", 5).unwrap()[0];
        assert_eq!(foo.implementations.len(), 1);
    }

    #[test]
    fn find_falls_back_to_substrings() {
        let (_dir, mut graph) = workspace(&[("src/util.rs", UTIL)]);
        graph.refresh().unwrap();
        let (hits, exact) = graph.find("help", 5).unwrap();
        assert!(!exact);
        assert_eq!(hits[0].qualname, "helper");
        assert!(graph.find("Foo::bar", 5).unwrap().1);
    }

    #[test]
    fn docs_configs_and_components_link_across_files() {
        let (dir, mut graph) = workspace(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/a\"]\n"),
            ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("nix/home.nix", "{ imports = [ ./kitty ]; }\n"),
            ("nix/kitty/default.nix", "{ }\n"),
            ("ui/Bar.qml", "import QtQuick\nItem { Pill { } }\n"),
            ("ui/Pill.qml", "import QtQuick\nRectangle { }\n"),
            ("notes/index.md", "# Index\nSee [[Later]] and [readme](../README.md).\n"),
            ("README.md", "# Readme\n"),
        ]);
        graph.refresh().unwrap();
        let (nodes, links) = graph.file_graph().unwrap();
        let id = |p: &str| nodes.iter().find(|n| n.path == p).unwrap_or_else(|| panic!("{p}")).id;
        assert!(links.contains(&(id("Cargo.toml"), id("crates/a/Cargo.toml"), LinkKind::Import)));
        assert!(links.contains(&(id("nix/home.nix"), id("nix/kitty/default.nix"), LinkKind::Import)));
        assert!(links.contains(&(id("ui/Bar.qml"), id("ui/Pill.qml"), LinkKind::Uses)));
        assert!(links.contains(&(id("notes/index.md"), id("README.md"), LinkKind::Import)));

        // The wikilink resolves once its note exists, without touching index.md.
        let index = graph.outline("notes/index.md").unwrap().unwrap();
        assert!(index.imports.contains(&("[[Later.md]]".to_string(), None)));
        fs::create_dir_all(dir.path().join("notes/sub")).unwrap();
        fs::write(dir.path().join("notes/sub/Later.md"), "# Later\n").unwrap();
        assert_eq!(graph.refresh().unwrap().parsed, 1);
        let later = graph.outline("notes/sub/Later.md").unwrap().unwrap();
        assert_eq!(later.imported_by, vec!["notes/index.md"]);
    }

    #[test]
    fn file_graph_has_import_and_unique_call_links() {
        let (_dir, mut graph) = workspace(&[
            ("web/app.ts", "import { a } from './lib/a';\nexport function go() { a(); unique(); }\n"),
            ("web/lib/a.ts", "export function a() {}\n"),
            ("web/other.js", "export function unique() {}\n"),
            ("py/pkg/__init__.py", ""),
            ("py/main.py", "from pkg import thing\n"),
        ]);
        graph.refresh().unwrap();
        let (nodes, links) = graph.file_graph().unwrap();
        let id = |p: &str| nodes.iter().find(|n| n.path == p).unwrap().id;
        assert!(links.contains(&(id("web/app.ts"), id("web/lib/a.ts"), LinkKind::Import)));
        assert!(links.contains(&(id("web/app.ts"), id("web/other.js"), LinkKind::Call)));
        // The call to a() is covered by the import link already.
        assert_eq!(links.iter().filter(|l| l.0 == id("web/app.ts") && l.1 == id("web/lib/a.ts")).count(), 1);
        assert!(links.contains(&(id("py/main.py"), id("py/pkg/__init__.py"), LinkKind::Import)));
    }
}
