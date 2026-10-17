//! The workspace graph tab: files as nodes, their imports, calls and uses as
//! links. `graph_layout` places them on its own thread; this module filters
//! them, moves the camera, follows the pointer and draws the graph with
//! limn's primitives in place of the tab's surface, under its controls.

use crate::graph_layout::{self, Command, Filtered, Forces, LayoutWorker};
use openide_graph::LinkKind;
use openide_ui::{Color, DrawCommand, FontFamily, Key, Point, Rect, UiEvent};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// The canvas's pointer region, and the surface whose place the drawing takes.
pub const CANVAS_ID: &str = "graph_canvas";
pub const SURFACE_ID: &str = "graph_surface";
const FILE_NAME: &str = "graph-view.json";
const MAX_BYTES: u64 = 256 * 1024;
/// How stale the index may be while the tab is shown; checking costs ~10 ms.
const REFRESH: Duration = Duration::from_secs(5);
/// The drawing loop stops this many frames after the last change.
const IDLE_FRAMES: u32 = 60;
/// Every transition covers this share of what is left each frame.
const EASE: f32 = 0.1;
const ZOOM_EASE: f32 = 0.15;
const FADED: f32 = 0.2;
/// A label's room on either side and below a node, for culling.
const LABEL_REACH: (f32, f32) = (300.0, 200.0);

/// Label sizes come from this ladder. A size the zoom only passes through
/// would put a new set of glyphs in the font atlas, and upload it, every frame.
const LABEL_SIZES: [f32; 16] = [8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 16.0, 18.0, 20.0, 23.0, 26.0, 30.0, 34.0, 39.0, 45.0];
const FOLDER_INKS: [Color; crate::theme::GRAPH_FOLDER_INKS.len()] = crate::theme::GRAPH_FOLDER_INKS;

/// Colors offered to a new group, in turn: the theme's folder inks.
pub const GROUP_COLORS: [[u8; 3]; FOLDER_INKS.len()] = {
    let mut colors = [[0; 3]; FOLDER_INKS.len()];
    let mut index = 0;
    while index < colors.len() {
        let ink = FOLDER_INKS[index];
        colors[index] = [ink.r, ink.g, ink.b];
        index += 1;
    }
    colors
};

fn label_size(size: f32) -> f32 {
    LABEL_SIZES
        .into_iter()
        .min_by(|a, b| (a - size).abs().total_cmp(&(b - size).abs()))
        .unwrap_or(size)
}

/// The folder a file is colored by: its first two directories, so that
/// `crates/core/src/lib.rs` goes with `crates/core`. Files at the root have none.
fn folder_of(path: &str) -> Option<&str> {
    let mut slashes = path.match_indices('/').map(|(at, _)| at);
    let first = slashes.next()?;
    Some(&path[..slashes.next().unwrap_or(first)])
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorGroup {
    pub query: String,
    pub color: [u8; 3],
}

/// What the controls set. Sliders keep the value they show; `forces` turns
/// it into what the layout takes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphOptions {
    pub search: String,
    pub imports: bool,
    pub calls: bool,
    pub uses: bool,
    pub orphans: bool,
    /// Only the open file and what lies within `depth` links of it.
    pub local: bool,
    pub depth: u8,
    pub incoming: bool,
    pub outgoing: bool,
    pub neighbours: bool,
    pub groups: Vec<ColorGroup>,
    pub arrows: bool,
    /// Each folder in its own color.
    pub folder_colors: bool,
    pub text_fade: f32,
    pub node_size: f32,
    pub line_size: f32,
    pub center: f64,
    pub repel: f64,
    pub link: f64,
    pub distance: f64,
    pub open: bool,
    pub filters_open: bool,
    pub groups_open: bool,
    pub display_open: bool,
    pub forces_open: bool,
}

impl Default for GraphOptions {
    fn default() -> Self {
        Self {
            search: String::new(),
            imports: true,
            calls: true,
            uses: true,
            orphans: true,
            local: false,
            depth: 1,
            incoming: true,
            outgoing: true,
            neighbours: false,
            groups: Vec::new(),
            arrows: false,
            folder_colors: true,
            text_fade: 0.0,
            node_size: 1.0,
            line_size: 1.0,
            center: graph_layout::force_to_slider(Forces::default().center),
            repel: 10.0,
            link: graph_layout::force_to_slider(Forces::default().link),
            distance: Forces::default().distance,
            open: true,
            filters_open: true,
            groups_open: false,
            display_open: false,
            forces_open: false,
        }
    }
}

impl GraphOptions {
    pub fn forces(&self) -> Forces {
        Forces {
            center: graph_layout::slider_to_force(self.center),
            repel: graph_layout::slider_to_repel(self.repel),
            link: graph_layout::slider_to_force(self.link),
            distance: self.distance,
        }
    }

    /// Restores every value but whether the panel and its sections are open.
    pub fn reset(&mut self) {
        *self = Self {
            open: self.open,
            filters_open: self.filters_open,
            groups_open: self.groups_open,
            display_open: self.display_open,
            forces_open: self.forces_open,
            ..Self::default()
        };
    }

    /// A missing or unreadable file gives the defaults: these are preferences.
    pub fn load(root: &Path) -> Self {
        let path = root.join(FILE_NAME);
        let fits = std::fs::metadata(&path).is_ok_and(|meta| meta.is_file() && meta.len() <= MAX_BYTES);
        fits.then(|| std::fs::read(&path).ok())
            .flatten()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        openide_harness::atomic_write(&root.join(FILE_NAME), &bytes)
            .map_err(|error| format!("No se pudieron guardar los ajustes del grafo: {error}"))
    }
}

/// A search of the filter or of a group: words that must all match, each in
/// the path, or with `path:`, `file:` or `lang:` in that part only. A leading
/// `-` turns a word down. Matching ignores case.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    terms: Vec<Term>,
}

#[derive(Clone, Debug, PartialEq)]
struct Term {
    field: Field,
    text: String,
    negated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Field {
    Path,
    File,
    Lang,
}

impl Query {
    /// Nothing for a search with no words.
    pub fn parse(text: &str) -> Option<Self> {
        let terms: Vec<Term> = text
            .split_whitespace()
            .filter_map(|word| {
                let (negated, word) = word.strip_prefix('-').map_or((false, word), |rest| (true, rest));
                let (field, text) = match word.split_once(':') {
                    Some(("path", rest)) => (Field::Path, rest),
                    Some(("file", rest)) => (Field::File, rest),
                    Some(("lang", rest)) => (Field::Lang, rest),
                    _ => (Field::Path, word),
                };
                (!text.is_empty()).then(|| Term { field, text: text.to_lowercase(), negated })
            })
            .collect();
        (!terms.is_empty()).then_some(Self { terms })
    }

    pub fn matches(&self, path: &str, lang: &str) -> bool {
        let path = path.to_lowercase();
        let file = path.rsplit('/').next().unwrap_or(&path);
        let lang = lang.to_lowercase();
        self.terms.iter().all(|term| {
            let found = match term.field {
                Field::Path => path.contains(&term.text),
                Field::File => file.contains(&term.text),
                Field::Lang => lang == term.text,
            };
            found != term.negated
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileInfo {
    pub path: String,
    pub lang: String,
    /// The earlier of its creation and last change, in seconds: its place in
    /// the timelapse.
    pub born: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphData {
    pub files: Vec<FileInfo>,
    pub links: Vec<(usize, usize, LinkKind)>,
}

/// Brings the workspace's index up to date and reads its file graph.
fn load(workspace: &Path) -> Result<GraphData, String> {
    let mut graph = openide_graph::Graph::open(workspace).map_err(|error| error.to_string())?;
    graph.refresh().map_err(|error| error.to_string())?;
    let (nodes, links) = graph.file_graph().map_err(|error| error.to_string())?;
    let root = graph.root().to_path_buf();
    let index: HashMap<i64, usize> = nodes.iter().enumerate().map(|(at, node)| (node.id, at)).collect();
    let files = nodes
        .into_iter()
        .map(|node| {
            let born = std::fs::metadata(root.join(&node.path))
                .ok()
                .and_then(|meta| {
                    let modified = meta.modified().ok();
                    let created = meta.created().ok();
                    created.into_iter().chain(modified).min()
                })
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_secs());
            FileInfo { path: node.path, lang: node.lang, born }
        })
        .collect();
    let links = links
        .into_iter()
        .filter_map(|(from, to, kind)| Some((*index.get(&from)?, *index.get(&to)?, kind)))
        .filter(|(from, to, _)| from != to)
        .collect();
    Ok(GraphData { files, links })
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShownNode {
    pub file: usize,
    /// A group's color, which goes before the folder's.
    pub color: Option<[u8; 3]>,
    /// Its folder's place in the theme's inks, when folders are colored.
    pub folder: Option<usize>,
    pub weight: f32,
    pub focused: bool,
}

/// The part of the graph the options let through, in its own indices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shown {
    pub nodes: Vec<ShownNode>,
    /// Directed, without repeats.
    pub links: Vec<(usize, usize)>,
    /// Steps the whole timelapse of this graph takes.
    pub steps: usize,
}

/// Applies link kinds, filter and groups, the local view, orphans and the
/// timelapse's `step` to `data`.
pub fn shown(data: &GraphData, options: &GraphOptions, focus: Option<&str>, step: Option<usize>) -> Shown {
    let mut queries: Vec<(Option<[u8; 3]>, Query)> = Vec::new();
    if let Some(query) = Query::parse(&options.search) {
        queries.push((None, query));
    }
    queries.extend(
        options
            .groups
            .iter()
            .filter_map(|group| Some((Some(group.color), Query::parse(&group.query)?))),
    );
    let focus = focus.and_then(|path| data.files.iter().position(|file| file.path == path));
    let local = focus.filter(|_| options.local);
    let mut colors: Vec<Option<Option<[u8; 3]>>> = data
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            match graph_layout::resolve_filter(queries.iter().map(|(color, query)| (*color, query.matches(&file.path, &file.lang)))) {
                Filtered::Hidden if Some(index) != local => None,
                Filtered::Hidden | Filtered::Plain => Some(None),
                Filtered::Colored(color) => Some(Some(color)),
            }
        })
        .collect();
    let kind_shown = |kind: LinkKind| match kind {
        LinkKind::Import => options.imports,
        LinkKind::Call => options.calls,
        LinkKind::Uses => options.uses,
    };
    let mut seen = HashSet::new();
    let mut links: Vec<(usize, usize)> = data
        .links
        .iter()
        .filter(|(from, to, kind)| kind_shown(*kind) && colors[*from].is_some() && colors[*to].is_some())
        .map(|&(from, to, _)| (from, to))
        .filter(|link| seen.insert(*link))
        .collect();
    let mut weights: HashMap<usize, f32> = HashMap::new();
    if options.local {
        let Some(center) = local else {
            return Shown::default();
        };
        let depth = usize::from(options.depth.clamp(1, 5));
        let mut level: HashMap<usize, usize> = HashMap::from([(center, 0)]);
        let mut queue = VecDeque::from([center]);
        while let Some(file) = queue.pop_front() {
            let at = level[&file];
            if at == depth {
                continue;
            }
            let next = links.iter().filter_map(|&(from, to)| {
                if options.outgoing && from == file {
                    Some(to)
                } else if options.incoming && to == file {
                    Some(from)
                } else {
                    None
                }
            });
            for other in next.collect::<Vec<_>>() {
                if let std::collections::hash_map::Entry::Vacant(entry) = level.entry(other) {
                    entry.insert(at + 1);
                    queue.push_back(other);
                }
            }
        }
        // Between neighbours only the links that reach out, unless asked.
        links.retain(|(from, to)| match (level.get(from), level.get(to)) {
            (Some(a), Some(b)) => options.neighbours || a.abs_diff(*b) == 1,
            _ => false,
        });
        for (index, color) in colors.iter_mut().enumerate() {
            if !level.contains_key(&index) {
                *color = None;
            }
        }
        weights = level
            .iter()
            .map(|(&file, &at)| (file, 30.0 - 30.0 * at as f32 / depth as f32))
            .collect();
    }
    if !options.orphans {
        let linked: HashSet<usize> = links.iter().flat_map(|&(from, to)| [from, to]).collect();
        for (index, color) in colors.iter_mut().enumerate() {
            if !linked.contains(&index) && Some(index) != local {
                *color = None;
            }
        }
    }
    // The timelapse walks the files from the oldest: each file is a step when
    // orphans are shown, and each of its links another.
    let mut order: Vec<usize> = (0..data.files.len()).filter(|&index| colors[index].is_some()).collect();
    order.sort_by_key(|&index| (data.files[index].born, index));
    let mut outgoing: HashMap<usize, Vec<usize>> = HashMap::new();
    for (position, &(from, _)) in links.iter().enumerate() {
        outgoing.entry(from).or_default().push(position);
    }
    let mut steps = 0;
    let mut kept_links: Option<HashSet<usize>> = step.map(|_| HashSet::new());
    let mut last_file = None;
    for &file in &order {
        if options.orphans {
            if step == Some(steps) && last_file.is_none() {
                last_file = Some(file);
            }
            steps += 1;
        }
        for &position in outgoing.get(&file).into_iter().flatten() {
            if let (Some(step), Some(kept)) = (step, kept_links.as_mut()) {
                if steps < step {
                    kept.insert(position);
                } else if steps == step && last_file.is_none() {
                    last_file = Some(file);
                }
            }
            steps += 1;
        }
    }
    if let Some(kept) = &kept_links {
        if let Some(last) = last_file {
            let cut = order.iter().position(|&file| file == last).map_or(order.len(), |at| at + 1);
            for &file in &order[cut..] {
                colors[file] = None;
            }
        }
        let mut position = 0;
        links.retain(|(from, to)| {
            let keep = kept.contains(&position) && colors[*from].is_some() && colors[*to].is_some();
            position += 1;
            keep
        });
    }
    // The largest folders take the first inks; the same folder keeps its ink
    // whatever the filters show.
    let mut sizes: HashMap<&str, usize> = HashMap::new();
    for file in &data.files {
        if let Some(folder) = folder_of(&file.path) {
            *sizes.entry(folder).or_default() += 1;
        }
    }
    let mut ranked: Vec<(&str, usize)> = sizes.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let rank: HashMap<&str, usize> = ranked.into_iter().enumerate().map(|(at, (folder, _))| (folder, at)).collect();
    let mut index_of = HashMap::new();
    let mut nodes = Vec::new();
    for &file in order.iter().filter(|&&file| colors[file].is_some()) {
        index_of.insert(file, nodes.len());
        nodes.push(ShownNode {
            file,
            color: colors[file].flatten(),
            folder: folder_of(&data.files[file].path)
                .filter(|_| options.folder_colors)
                .and_then(|folder| rank.get(folder).copied()),
            weight: 0.0,
            focused: Some(file) == focus,
        });
    }
    let links: Vec<(usize, usize)> = links
        .into_iter()
        .filter_map(|(from, to)| Some((*index_of.get(&from)?, *index_of.get(&to)?)))
        .collect();
    for node in &mut nodes {
        node.weight = weights.get(&node.file).copied().unwrap_or(0.0);
    }
    if !options.local {
        for &(from, to) in &links {
            nodes[from].weight += 1.0;
            nodes[to].weight += 1.0;
        }
    }
    Shown { nodes, links, steps }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum GraphStatus {
    #[default]
    Idle,
    Loading,
    Ready { files: usize, links: usize },
    Failed(String),
}

/// What a pointer or key event on the graph asks of the window.
#[derive(Debug, Default, PartialEq)]
pub struct EventOutcome {
    pub consumed: bool,
    /// A click on a node: open this workspace-relative file.
    pub open: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct Press {
    at: Point,
    node: Option<usize>,
    moved: f32,
}

#[derive(Clone, Copy, Debug)]
struct Look {
    alpha: f32,
    color: [f32; 3],
    /// How far its label has dropped for the hover, 0 to 1.
    drop: f32,
}

pub struct GraphView {
    workspace: Option<PathBuf>,
    loading: Option<Receiver<Result<GraphData, String>>>,
    loaded_at: Option<Instant>,
    pub status: GraphStatus,
    data: GraphData,
    shown: Shown,
    shown_key: Option<(GraphOptions, Option<String>, Option<usize>)>,
    worker: Option<LayoutWorker>,
    forces: Option<Forces>,
    generation: u64,
    sequence: u64,
    positions: Vec<[f32; 2]>,
    moving: bool,
    looks: Vec<Look>,
    link_alpha: Vec<f32>,
    scale: f32,
    target_scale: f32,
    /// Where the origin of the layout is, from the canvas's top-left corner.
    pan: (f32, f32),
    /// Pan speed in pixels per millisecond, carried on after a quick release.
    pan_velocity: (f32, f32),
    zoom_anchor: Option<Point>,
    hover: Option<usize>,
    press: Option<Press>,
    drag: Option<usize>,
    last_move: Option<(Point, Instant, (f32, f32))>,
    pointer: Option<Point>,
    idle_frames: u32,
    rect: Option<Rect>,
    timelapse: Option<(Instant, f64)>,
    step: Option<usize>,
    widths: HashMap<String, f32>,
    /// Of the shown graph, worked out once: each node's neighbours, its label,
    /// and whether a link draws its line (one line for a pair linked both ways).
    neighbours: Vec<Vec<usize>>,
    names: Vec<String>,
    lines: Vec<bool>,
}

impl Default for GraphView {
    fn default() -> Self {
        Self {
            workspace: None,
            loading: None,
            loaded_at: None,
            status: GraphStatus::Idle,
            data: GraphData::default(),
            shown: Shown::default(),
            shown_key: None,
            worker: None,
            forces: None,
            generation: 0,
            sequence: 0,
            positions: Vec::new(),
            moving: false,
            looks: Vec::new(),
            link_alpha: Vec::new(),
            scale: 1.0,
            target_scale: 1.0,
            pan: (0.0, 0.0),
            pan_velocity: (0.0, 0.0),
            zoom_anchor: None,
            hover: None,
            press: None,
            drag: None,
            last_move: None,
            pointer: None,
            idle_frames: 0,
            rect: None,
            timelapse: None,
            step: None,
            widths: HashMap::new(),
            neighbours: Vec::new(),
            names: Vec::new(),
            lines: Vec::new(),
        }
    }
}

impl GraphView {
    /// Keeps the index of `workspace` loading or fresh while the tab is shown.
    /// `wake` asks the window for a frame from another thread.
    pub fn keep_loaded(&mut self, workspace: &Path, wake: impl Fn() + Send + Clone + 'static) {
        if self.workspace.as_deref() != Some(workspace) {
            *self = Self { workspace: Some(workspace.to_path_buf()), widths: std::mem::take(&mut self.widths), ..Self::default() };
        }
        let due = self.loaded_at.is_none_or(|at| at.elapsed() >= REFRESH);
        if self.loading.is_none() && due {
            let (sender, receiver) = mpsc::channel();
            let root = workspace.to_path_buf();
            let done = wake.clone();
            std::thread::Builder::new()
                .name("graph-index".into())
                .spawn(move || {
                    let _ = sender.send(load(&root));
                    done();
                })
                .expect("the graph index thread starts");
            self.loading = Some(receiver);
            if self.loaded_at.is_none() {
                self.status = GraphStatus::Loading;
            }
        }
        if self.worker.is_none() {
            self.worker = Some(LayoutWorker::spawn(wake));
        }
    }

    /// Takes what the threads have produced and applies the options. True
    /// when the controls' text (counts, errors) changed.
    pub fn poll(&mut self, options: &GraphOptions, focus: Option<&str>) -> bool {
        let mut status_changed = false;
        if let Some(result) = self.loading.as_ref().and_then(|receiver| receiver.try_recv().ok()) {
            self.loading = None;
            self.loaded_at = Some(Instant::now());
            match result {
                Ok(data) => {
                    if data != self.data || matches!(self.status, GraphStatus::Failed(_) | GraphStatus::Loading) {
                        self.data = data;
                        self.status = GraphStatus::Loading;
                        self.shown_key = None;
                    }
                }
                Err(error) => {
                    status_changed = true;
                    self.status = GraphStatus::Failed(error);
                }
            }
        }
        if let Some((started, speed)) = self.timelapse {
            let step = graph_layout::timelapse_step(speed, started.elapsed().as_secs_f64() * 1000.0);
            // It ends on its last step instead of redrawing the whole graph forever.
            if step > self.shown.steps {
                self.timelapse = None;
                self.step = None;
            } else {
                self.step = Some(step);
            }
        }
        let key = (options.clone(), focus.map(str::to_owned), self.step);
        if self.loaded_at.is_some() && self.shown_key.as_ref() != Some(&key) {
            let next = shown(&self.data, options, focus, self.step);
            if next != self.shown {
                self.replace_shown(next);
            }
            let ready = GraphStatus::Ready { files: self.shown.nodes.len(), links: self.shown.links.len() };
            if !matches!(self.status, GraphStatus::Failed(_)) && self.status != ready {
                self.status = ready;
                status_changed = true;
            }
            self.shown_key = Some(key);
            self.idle_frames = 0;
        }
        let forces = options.forces();
        if self.forces != Some(forces) {
            if let Some(worker) = &self.worker {
                worker.send(Command::Forces(forces));
                self.forces = Some(forces);
            }
        }
        if let Some(published) = self.worker.as_ref().and_then(|worker| worker.newer_than(self.sequence)) {
            self.sequence = published.sequence;
            if published.generation == self.generation {
                self.positions = published.positions.to_vec();
                self.moving = published.moving;
                self.idle_frames = 0;
            }
        }
        status_changed
    }

    fn replace_shown(&mut self, next: Shown) {
        let old: HashMap<usize, Look> = self
            .shown
            .nodes
            .iter()
            .zip(self.looks.iter())
            .map(|(node, look)| (node.file, *look))
            .collect();
        let old_positions: HashMap<usize, [f32; 2]> = self
            .shown
            .nodes
            .iter()
            .zip(self.positions.iter())
            .map(|(node, position)| (node.file, *position))
            .collect();
        self.looks = next
            .nodes
            .iter()
            .map(|node| old.get(&node.file).copied().unwrap_or(Look { alpha: 1.0, color: rgb(self.fill(node)), drop: 0.0 }))
            .collect();
        // Until the layout answers, known nodes stay put and new ones wait.
        self.positions = next
            .nodes
            .iter()
            .map(|node| old_positions.get(&node.file).copied().unwrap_or([f32::NAN, f32::NAN]))
            .collect();
        self.link_alpha = vec![1.0; next.links.len()];
        self.neighbours = vec![Vec::new(); next.nodes.len()];
        for &(from, to) in &next.links {
            self.neighbours[from].push(to);
            self.neighbours[to].push(from);
        }
        self.names = next
            .nodes
            .iter()
            .map(|node| {
                let path = &self.data.files[node.file].path;
                let name = path.rsplit('/').next().unwrap_or(path);
                name.rsplit_once('.').map_or(name, |(stem, _)| if stem.is_empty() { name } else { stem }).to_owned()
            })
            .collect();
        let pairs: HashSet<(usize, usize)> = next.links.iter().copied().collect();
        self.lines = next.links.iter().map(|&(from, to)| !(from > to && pairs.contains(&(to, from)))).collect();
        self.hover = None;
        self.drag = None;
        self.generation += 1;
        if let Some(worker) = &self.worker {
            worker.send(Command::Data {
                ids: next.nodes.iter().map(|node| self.data.files[node.file].path.clone()).collect(),
                links: next.links.clone(),
                generation: self.generation,
            });
        }
        self.shown = next;
    }

    /// Starts the timelapse from the first step.
    pub fn start_timelapse(&mut self) {
        let steps = shown(&self.data, &GraphOptions { local: false, ..GraphOptions::default() }, None, None).steps.max(self.shown.steps);
        self.timelapse = Some((Instant::now(), graph_layout::timelapse_speed(steps)));
        self.step = Some(1);
        self.idle_frames = 0;
    }

    fn fill(&self, node: &ShownNode) -> Color {
        match (node.color, node.folder) {
            (Some([r, g, b]), _) => Color::rgb(r, g, b),
            (None, Some(folder)) => FOLDER_INKS[folder % FOLDER_INKS.len()],
            (None, None) => crate::theme::GRAPH_NODE,
        }
    }

    fn radius(&self, index: usize, options: &GraphOptions) -> f32 {
        graph_layout::node_radius(self.shown.nodes[index].weight, options.node_size)
    }

    /// A node's radius on screen: it grows with the zoom's square root.
    fn screen_radius(&self, index: usize, options: &GraphOptions) -> f32 {
        self.radius(index, options) * self.scale * graph_layout::node_scale(self.scale)
    }

    fn to_screen(&self, rect: Rect, [x, y]: [f32; 2]) -> Point {
        Point::new(rect.x + self.pan.0 + x * self.scale, rect.y + self.pan.1 + y * self.scale)
    }

    fn to_layout(&self, rect: Rect, point: Point) -> (f32, f32) {
        ((point.x - rect.x - self.pan.0) / self.scale, (point.y - rect.y - self.pan.1) / self.scale)
    }

    fn node_at(&self, rect: Rect, point: Point, options: &GraphOptions) -> Option<usize> {
        self.positions
            .iter()
            .enumerate()
            .filter(|(_, position)| position[0].is_finite())
            .map(|(index, &position)| {
                let at = self.to_screen(rect, position);
                (index, ((at.x - point.x).powi(2) + (at.y - point.y).powi(2)).sqrt())
            })
            .filter(|&(index, distance)| distance <= self.screen_radius(index, options) + 2.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }

    /// The canvas moved or resized: the view keeps its center where it was.
    pub fn place(&mut self, rect: Rect) {
        match self.rect {
            None => self.pan = (rect.width / 2.0, rect.height / 2.0),
            Some(old) if old.width != rect.width || old.height != rect.height => {
                self.pan.0 += (rect.width - old.width) / 2.0;
                self.pan.1 += (rect.height - old.height) / 2.0;
            }
            _ => {}
        }
        self.rect = Some(rect);
    }

    fn zoom_by(&mut self, factor: f32, anchor: Option<Point>) {
        self.target_scale = graph_layout::clamp_zoom(self.target_scale * factor);
        // Zooming in keeps the point under the pointer; out, the view's center.
        self.zoom_anchor = anchor.filter(|_| factor > 1.0);
        self.idle_frames = 0;
    }

    /// `over_canvas`: the event's position is on the canvas and not under the
    /// controls or a menu.
    pub fn handle_event(&mut self, event: &UiEvent, options: &GraphOptions, over_canvas: bool) -> EventOutcome {
        let Some(rect) = self.rect else {
            return EventOutcome::default();
        };
        let consumed = EventOutcome { consumed: true, open: None };
        match event {
            UiEvent::Scroll { position, delta_y } if over_canvas => {
                // One wheel notch is 30 here and 120 pixels in a browser.
                self.zoom_by(graph_layout::wheel_factor(-delta_y * 4.0, 0), Some(*position));
                consumed
            }
            UiEvent::ScrollDirect { position, delta_y } if over_canvas => {
                self.zoom_by(graph_layout::wheel_factor(-delta_y, 0), Some(*position));
                consumed
            }
            UiEvent::Scroll2D { position, delta_x, delta_y, .. } if over_canvas => {
                self.pan.0 += delta_x;
                self.pan.1 += delta_y;
                self.idle_frames = 0;
                consumed
            }
            UiEvent::PointerMove(point) => {
                self.pointer = Some(*point);
                if let Some(press) = &mut self.press {
                    let moved = ((point.x - press.at.x).powi(2) + (point.y - press.at.y).powi(2)).sqrt();
                    press.moved = press.moved.max(moved);
                    let node = press.node;
                    if let Some(node) = node.filter(|_| !graph_layout::is_click(moved) || self.drag.is_some()) {
                        self.drag = Some(node);
                        let (x, y) = self.to_layout(rect, *point);
                        if let Some(worker) = &self.worker {
                            worker.send(Command::Hold { index: node, x: f64::from(x), y: f64::from(y) });
                        }
                        if let Some(position) = self.positions.get_mut(node) {
                            *position = [x, y];
                        }
                    } else if node.is_none() {
                        let now = Instant::now();
                        if let Some((last, at, _)) = self.last_move {
                            let (dx, dy) = (point.x - last.x, point.y - last.y);
                            self.pan.0 += dx;
                            self.pan.1 += dy;
                            let elapsed = now.duration_since(at).as_secs_f32() * 1000.0;
                            let speed = (dx / elapsed.max(1.0), dy / elapsed.max(1.0));
                            let previous = self.last_move.map_or((0.0, 0.0), |(_, _, speed)| speed);
                            self.last_move = Some((*point, now, (previous.0 * 0.8 + speed.0 * 0.2, previous.1 * 0.8 + speed.1 * 0.2)));
                        }
                    }
                    self.idle_frames = 0;
                    return consumed;
                }
                let hover = if over_canvas { self.node_at(rect, *point, options) } else { None };
                if hover != self.hover {
                    self.hover = hover;
                    self.idle_frames = 0;
                }
                // The controls keep hearing plain moves for their own hover.
                EventOutcome::default()
            }
            UiEvent::PointerLeave => {
                if self.press.is_none() && self.hover.take().is_some() {
                    self.idle_frames = 0;
                }
                self.pointer = None;
                EventOutcome::default()
            }
            UiEvent::PointerDown(point)
            | UiEvent::PointerDownModified { position: point, .. }
            | UiEvent::PointerDownEditor { position: point, .. }
                if over_canvas =>
            {
                let node = self.node_at(rect, *point, options);
                self.press = Some(Press { at: *point, node, moved: 0.0 });
                self.pan_velocity = (0.0, 0.0);
                if node.is_none() {
                    self.last_move = Some((*point, Instant::now(), (0.0, 0.0)));
                }
                consumed
            }
            UiEvent::PointerUp(point) => {
                let Some(press) = self.press.take() else {
                    return EventOutcome::default();
                };
                let mut outcome = consumed;
                if let Some(node) = self.drag.take() {
                    if let Some(worker) = &self.worker {
                        worker.send(Command::Release { index: node });
                    }
                } else if let Some(node) = press.node.filter(|_| graph_layout::is_click(press.moved)) {
                    outcome.open = Some(self.data.files[self.shown.nodes[node].file].path.clone());
                }
                if press.node.is_none() {
                    // A release soon after the last move lets the view glide on.
                    if let Some((_, at, speed)) = self.last_move.take() {
                        if at.elapsed() < Duration::from_millis(100) {
                            self.pan_velocity = speed;
                        }
                    }
                }
                self.hover = if over_canvas { self.node_at(rect, *point, options) } else { None };
                self.idle_frames = 0;
                outcome
            }
            _ => EventOutcome::default(),
        }
    }

    /// While the canvas has the keyboard: arrows pan, three times faster with
    /// Shift, and `+` or `-` zoom about the center.
    pub fn handle_key(&mut self, event: &UiEvent) -> bool {
        let (dx, dy, shift) = match event {
            UiEvent::Key { key: Key::ArrowLeft, shift, ctrl: false } => (1.0, 0.0, *shift),
            UiEvent::Key { key: Key::ArrowRight, shift, ctrl: false } => (-1.0, 0.0, *shift),
            UiEvent::Key { key: Key::ArrowUp, shift, ctrl: false } => (0.0, 1.0, *shift),
            UiEvent::Key { key: Key::ArrowDown, shift, ctrl: false } => (0.0, -1.0, *shift),
            UiEvent::Text(text) if text == "+" || text == "=" => {
                self.zoom_by(1.1, None);
                return true;
            }
            UiEvent::Text(text) if text == "-" => {
                self.zoom_by(1.0 / 1.1, None);
                return true;
            }
            _ => return false,
        };
        // The pan's speed eases towards the arrow's, and glides on after it.
        let speed = if shift { 3.0 } else { 1.0 };
        self.pan_velocity.0 = self.pan_velocity.0 * 0.9 + dx * speed * 0.1;
        self.pan_velocity.1 = self.pan_velocity.1 * 0.9 + dy * speed * 0.1;
        self.idle_frames = 0;
        true
    }

    /// Advances the camera and the fades by one frame. True while anything
    /// still moves, so the window asks for another.
    pub fn animate(&mut self) -> bool {
        let Some(rect) = self.rect else {
            return false;
        };
        let mut changed = false;
        if (self.scale - self.target_scale).abs() > 1e-4 {
            let next = self.scale + (self.target_scale - self.scale) * ZOOM_EASE;
            let anchor = self.zoom_anchor.unwrap_or(Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0));
            let (lx, ly) = self.to_layout(rect, anchor);
            self.scale = next;
            self.pan = (anchor.x - rect.x - lx * next, anchor.y - rect.y - ly * next);
            changed = true;
        } else {
            self.scale = self.target_scale;
        }
        if self.press.is_none() && (self.pan_velocity.0.abs() > 0.001 || self.pan_velocity.1.abs() > 0.001) {
            // Pixels per millisecond over a 60 Hz frame, losing a tenth each frame.
            self.pan.0 += self.pan_velocity.0 * 1000.0 / 60.0;
            self.pan.1 += self.pan_velocity.1 * 1000.0 / 60.0;
            self.pan_velocity = (self.pan_velocity.0 * 0.9, self.pan_velocity.1 * 0.9);
            changed = true;
        }
        let focus = self.drag.or(self.hover);
        let neighbours: HashSet<usize> = focus
            .map(|node| self.neighbours[node].iter().copied().chain([node]).collect())
            .unwrap_or_default();
        for index in 0..self.looks.len() {
            let alpha = if focus.is_none() || neighbours.contains(&index) { 1.0 } else { FADED };
            let color = rgb(if Some(index) == focus { crate::theme::FOREGROUND } else { self.fill(&self.shown.nodes[index]) });
            let drop = if Some(index) == focus { 1.0 } else { 0.0 };
            let look = &mut self.looks[index];
            changed |= ease(&mut look.alpha, alpha);
            for (value, target) in look.color.iter_mut().zip(color) {
                changed |= ease(value, target);
            }
            changed |= ease(&mut look.drop, drop);
        }
        for (index, &(from, to)) in self.shown.links.iter().enumerate() {
            let alpha = if focus.is_none() || Some(from) == focus || Some(to) == focus { 1.0 } else { FADED };
            changed |= ease(&mut self.link_alpha[index], alpha);
        }
        if changed || self.moving || self.timelapse.is_some() || self.press.is_some() {
            self.idle_frames = 0;
        } else {
            self.idle_frames += 1;
        }
        self.idle_frames < IDLE_FRAMES
    }

    /// The pointer's look over the canvas.
    pub fn grabbing(&self) -> bool {
        self.press.is_some_and(|press| press.node.is_none()) && self.last_move.is_some()
    }

    pub fn over_node(&self) -> bool {
        self.hover.is_some() || self.drag.is_some()
    }

    /// The graph's paint instructions for the canvas at `rect`: lines, then
    /// nodes and the hover ring, then labels, all clipped to the canvas.
    /// `measure` gives a text's width at a font size.
    pub fn paint(&mut self, rect: Rect, options: &GraphOptions, measure: &dyn Fn(&str, f32) -> f32) -> Vec<DrawCommand> {
        let mut commands = vec![DrawCommand::PushClip(rect)];
        let visible = |point: Point, margin_x: f32, margin_y: f32| {
            point.x >= rect.x - margin_x
                && point.x <= rect.x + rect.width + margin_x
                && point.y >= rect.y - margin_y
                && point.y <= rect.y + rect.height + margin_y
        };
        let focus = self.drag.or(self.hover);
        let line_width = options.line_size.max(0.1);
        let arrow = graph_layout::arrow_opacity(self.scale);
        let mut arrows = Vec::new();
        for (index, &(from, to)) in self.shown.links.iter().enumerate() {
            let (Some(&a), Some(&b)) = (self.positions.get(from), self.positions.get(to)) else {
                continue;
            };
            if !a[0].is_finite() || !b[0].is_finite() {
                continue;
            }
            let (a, b) = (self.to_screen(rect, a), self.to_screen(rect, b));
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let length = (dx * dx + dy * dy).sqrt();
            let (ra, rb) = (self.screen_radius(from, options), self.screen_radius(to, options));
            if length <= ra + rb || (!visible(a, 0.0, 0.0) && !visible(b, 0.0, 0.0) && !crosses(rect, a, b)) {
                continue;
            }
            let (ux, uy) = (dx / length, dy / length);
            let start = Point::new(a.x + ux * ra, a.y + uy * ra);
            let end = Point::new(b.x - ux * rb, b.y - uy * rb);
            let highlighted = focus.is_some() && (Some(from) == focus || Some(to) == focus);
            let alpha = self.link_alpha.get(index).copied().unwrap_or(1.0);
            // A pair linked both ways is one line; its other arrow still shows.
            if self.lines.get(index).copied().unwrap_or(true) {
                commands.push(DrawCommand::Polyline {
                    points: Arc::from([start, end]),
                    color: with_alpha(if highlighted { crate::theme::GRAPH_LINK_LIT } else { crate::theme::GRAPH_LINK }, alpha),
                    width: line_width,
                    dashed: false,
                });
            }
            let span = length - ra - rb;
            if options.arrows && arrow > 0.0 && span > line_width {
                let size = 4.0 * line_width.sqrt();
                let tip = end;
                let back = Point::new(tip.x - ux * size * 2.0, tip.y - uy * size * 2.0);
                let (nx, ny) = (-uy * size, ux * size);
                arrows.push(DrawCommand::FillTriangle {
                    points: [tip, Point::new(back.x + nx, back.y + ny), Point::new(back.x - nx, back.y - ny)],
                    color: with_alpha(crate::theme::FOREGROUND, 0.5 * arrow * alpha),
                });
            }
        }
        commands.extend(arrows);
        let label_alpha = graph_layout::label_opacity(self.scale, options.text_fade);
        let mut labels = Vec::new();
        for (index, node) in self.shown.nodes.iter().enumerate() {
            let Some(&position) = self.positions.get(index) else {
                continue;
            };
            if !position[0].is_finite() {
                continue;
            }
            let center = self.to_screen(rect, position);
            let radius = self.screen_radius(index, options);
            let look = self.looks.get(index).copied().unwrap_or(Look { alpha: 1.0, color: rgb(crate::theme::GRAPH_NODE), drop: 0.0 });
            if visible(center, radius, radius) {
                let fill = Color::rgba(look.color[0] as u8, look.color[1] as u8, look.color[2] as u8, (255.0 * look.alpha) as u8);
                let bounds = Rect::new(center.x - radius, center.y - radius, radius * 2.0, radius * 2.0);
                commands.push(DrawCommand::FillRect { rect: bounds, color: fill, radius });
                // The open file keeps a ring, and so does the node under the pointer.
                if Some(index) == focus || node.focused {
                    commands.push(DrawCommand::StrokeRect {
                        rect: Rect::new(bounds.x - 2.0, bounds.y - 2.0, bounds.width + 4.0, bounds.height + 4.0),
                        color: with_alpha(crate::theme::FOREGROUND, look.alpha),
                        width: 1.5,
                        radius: radius + 2.0,
                    });
                }
            }
            let hovered = Some(index) == focus;
            // In steps of 1/32: a label is laid out again for each new alpha.
            let alpha = if hovered { 1.0 } else { (label_alpha * look.alpha * 32.0).round() / 32.0 };
            if alpha <= 0.0 || !visible(center, LABEL_REACH.0, LABEL_REACH.1) {
                continue;
            }
            let name = self.names[index].as_str();
            let world = self.radius(index, options);
            // Under 1x the hovered label keeps its size so it stays legible.
            let grow = if hovered && self.scale < 1.0 { 1.0 } else { self.scale * graph_layout::node_scale(self.scale) };
            let size = label_size((14.0 + world / 4.0) * grow);
            let width = match self.widths.get(name) {
                Some(width) => *width,
                None => *self.widths.entry(name.to_owned()).or_insert(measure(name, 16.0)),
            } * size / 16.0;
            let top = center.y + (world + 5.0) * self.scale * graph_layout::node_scale(self.scale) + 15.0 * look.drop;
            labels.push(DrawCommand::Text {
                position: Point::new(center.x - width / 2.0, top),
                text: name.to_owned(),
                color: with_alpha(crate::theme::FOREGROUND, alpha),
                font_size: size,
                font_family: FontFamily::Sans,
            });
        }
        commands.extend(labels);
        commands.push(DrawCommand::PopClip);
        commands
    }
}

/// Moves `value` a step towards `target`; true while it is still on its way.
fn ease(value: &mut f32, target: f32) -> bool {
    let gap = target - *value;
    if gap.abs() < 0.002 * target.abs().max(1.0) {
        *value = target;
        return false;
    }
    *value += gap * EASE;
    true
}

fn rgb(color: Color) -> [f32; 3] {
    [f32::from(color.r), f32::from(color.g), f32::from(color.b)]
}

fn with_alpha(color: Color, alpha: f32) -> Color {
    Color::rgba(color.r, color.g, color.b, (f32::from(color.a) * alpha.clamp(0.0, 1.0)) as u8)
}

/// Whether the segment from `a` to `b` may cross `rect` though both ends
/// are outside it: its bounding box overlaps the rectangle.
fn crosses(rect: Rect, a: Point, b: Point) -> bool {
    a.x.min(b.x) <= rect.x + rect.width && a.x.max(b.x) >= rect.x && a.y.min(b.y) <= rect.y + rect.height && a.y.max(b.y) >= rect.y
}

/// Replaces the graph's surface in `commands` with `graph`, so the controls
/// and menus painted after it stay on top.
pub fn splice(commands: &mut Vec<DrawCommand>, graph: Vec<DrawCommand>) -> bool {
    let Some(at) = commands
        .iter()
        .position(|command| matches!(command, DrawCommand::Surface { id, .. } if id == SURFACE_ID))
    else {
        return false;
    };
    commands.splice(at..=at, graph);
    true
}

/// The canvas's box in this frame, if the tab shows it.
pub fn canvas_rect(commands: &[DrawCommand]) -> Option<Rect> {
    commands.iter().rev().find_map(|command| match command {
        DrawCommand::Surface { rect, id, .. } if id == SURFACE_ID => Some(*rect),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(paths: &[&str], links: &[(usize, usize, LinkKind)]) -> GraphData {
        GraphData {
            files: paths
                .iter()
                .enumerate()
                .map(|(index, path)| FileInfo { path: (*path).into(), lang: path.rsplit('.').next().unwrap().into(), born: index as u64 })
                .collect(),
            links: links.to_vec(),
        }
    }

    fn paths(data: &GraphData, shown: &Shown) -> Vec<String> {
        shown.nodes.iter().map(|node| data.files[node.file].path.clone()).collect()
    }

    #[test]
    fn queries_match_words_in_the_path_or_one_part_of_it() {
        let query = Query::parse("path:src -test lang:rs").unwrap();
        assert!(query.matches("crates/core/src/lib.rs", "rs"));
        assert!(!query.matches("crates/core/src/test.rs", "rs"));
        assert!(!query.matches("crates/core/src/lib.py", "py"));
        assert!(Query::parse("file:Lib").unwrap().matches("src/lib.rs", "rs"));
        assert!(!Query::parse("file:src").unwrap().matches("src/lib.rs", "rs"));
        assert!(Query::parse("   ").is_none());
    }

    #[test]
    fn the_filter_hides_files_and_the_first_matching_group_colors_them() {
        let graph = data(&["a/one.rs", "a/two.rs", "b/three.py"], &[(0, 1, LinkKind::Import), (1, 2, LinkKind::Call)]);
        let options = GraphOptions {
            search: "-three".into(),
            groups: vec![
                ColorGroup { query: "one".into(), color: [1, 1, 1] },
                ColorGroup { query: "path:a".into(), color: [2, 2, 2] },
            ],
            ..GraphOptions::default()
        };
        let result = shown(&graph, &options, None, None);
        assert_eq!(paths(&graph, &result), ["a/one.rs", "a/two.rs"]);
        assert_eq!(result.nodes[0].color, Some([1, 1, 1]));
        assert_eq!(result.nodes[1].color, Some([2, 2, 2]));
        assert_eq!(result.links, [(0, 1)]);
        assert_eq!(result.nodes.iter().map(|node| node.weight).collect::<Vec<_>>(), [1.0, 1.0]);
    }

    #[test]
    fn link_kinds_and_orphans_follow_their_switches() {
        let graph = data(&["a.rs", "b.rs", "c.rs"], &[(0, 1, LinkKind::Import), (1, 0, LinkKind::Call)]);
        let options = GraphOptions { calls: false, orphans: false, ..GraphOptions::default() };
        let result = shown(&graph, &options, None, None);
        assert_eq!(paths(&graph, &result), ["a.rs", "b.rs"]);
        assert_eq!(result.links, [(0, 1)]);
    }

    #[test]
    fn the_local_graph_weighs_nodes_by_their_distance_from_the_open_file() {
        let graph = data(
            &["a.rs", "b.rs", "c.rs", "d.rs"],
            &[(0, 1, LinkKind::Import), (1, 2, LinkKind::Import), (2, 3, LinkKind::Import), (0, 2, LinkKind::Import)],
        );
        let one = GraphOptions { local: true, depth: 1, ..GraphOptions::default() };
        let result = shown(&graph, &one, Some("b.rs"), None);
        let weights: HashMap<String, f32> = paths(&graph, &result).into_iter().zip(result.nodes.iter().map(|node| node.weight)).collect();
        assert_eq!(weights, HashMap::from([("a.rs".into(), 0.0), ("b.rs".into(), 30.0), ("c.rs".into(), 0.0)]));
        // a and c are both neighbours of b: their own link waits for the switch.
        assert_eq!(result.links.len(), 2);
        let with_neighbours = shown(&graph, &GraphOptions { neighbours: true, ..one.clone() }, Some("b.rs"), None);
        assert_eq!(with_neighbours.links.len(), 3);
        let two = shown(&graph, &GraphOptions { depth: 2, outgoing: true, incoming: false, ..one }, Some("b.rs"), None);
        let weights: HashMap<String, f32> = paths(&graph, &two).into_iter().zip(two.nodes.iter().map(|node| node.weight)).collect();
        assert_eq!(weights, HashMap::from([("b.rs".into(), 30.0), ("c.rs".into(), 15.0), ("d.rs".into(), 0.0)]));
    }

    #[test]
    fn the_timelapse_shows_files_from_the_oldest_one_step_at_a_time() {
        // Born in order: a, b, c. Steps: a, a→b, b, b→c, c.
        let graph = data(&["a.rs", "b.rs", "c.rs"], &[(0, 1, LinkKind::Import), (1, 2, LinkKind::Import)]);
        let options = GraphOptions::default();
        assert_eq!(shown(&graph, &options, None, None).steps, 5);
        let at = |step| {
            let result = shown(&graph, &options, None, Some(step));
            (paths(&graph, &result), result.links.len())
        };
        assert_eq!(at(1), (vec!["a.rs".to_owned()], 0));
        assert_eq!(at(2), (vec!["a.rs".to_owned(), "b.rs".to_owned()], 1));
        assert_eq!(at(5), (vec!["a.rs".to_owned(), "b.rs".to_owned(), "c.rs".to_owned()], 2));
    }

    #[test]
    fn the_graph_takes_the_place_of_its_surface_among_the_paint_instructions() {
        let surface = DrawCommand::Surface { rect: Rect::new(0.0, 0.0, 10.0, 10.0), id: SURFACE_ID.into(), label: "Grafo".into(), opacity: 1.0 };
        let mut commands = vec![DrawCommand::PopClip, surface, DrawCommand::PopClip];
        assert_eq!(canvas_rect(&commands), Some(Rect::new(0.0, 0.0, 10.0, 10.0)));
        assert!(splice(&mut commands, vec![DrawCommand::PushClip(Rect::new(0.0, 0.0, 1.0, 1.0))]));
        assert!(matches!(commands[1], DrawCommand::PushClip(_)));
        assert_eq!(commands.len(), 3);
        assert!(!splice(&mut commands, Vec::new()));
    }

    #[test]
    fn a_short_press_on_a_node_opens_it_and_a_longer_one_drags_it() {
        let mut view = GraphView { loaded_at: Some(Instant::now()), ..GraphView::default() };
        view.data = data(&["a.rs", "b.rs"], &[(0, 1, LinkKind::Import)]);
        let options = GraphOptions::default();
        view.poll(&options, None);
        view.place(Rect::new(0.0, 0.0, 400.0, 400.0));
        view.positions = vec![[0.0, 0.0], [100.0, 0.0]];
        let center = Point::new(200.0, 200.0);
        view.handle_event(&UiEvent::PointerDown(center), &options, true);
        let outcome = view.handle_event(&UiEvent::PointerUp(Point::new(203.0, 202.0)), &options, true);
        assert_eq!(outcome.open.as_deref(), Some("a.rs"));
        view.handle_event(&UiEvent::PointerDown(center), &options, true);
        view.handle_event(&UiEvent::PointerMove(Point::new(260.0, 230.0)), &options, true);
        assert_eq!(view.positions[0], [60.0, 30.0]);
        let outcome = view.handle_event(&UiEvent::PointerUp(Point::new(260.0, 230.0)), &options, true);
        assert_eq!(outcome.open, None);
    }

    #[test]
    fn the_wheel_zooms_towards_the_pointer_within_its_limits() {
        let mut view = GraphView::default();
        view.place(Rect::new(0.0, 0.0, 400.0, 400.0));
        let options = GraphOptions::default();
        let pointer = Point::new(300.0, 100.0);
        let before = view.to_layout(Rect::new(0.0, 0.0, 400.0, 400.0), pointer);
        view.handle_event(&UiEvent::Scroll { position: pointer, delta_y: 30.0 }, &options, true);
        assert!((view.target_scale - 1.5).abs() < 1e-5);
        while view.animate() && (view.scale - view.target_scale).abs() > 1e-4 {}
        let after = view.to_layout(Rect::new(0.0, 0.0, 400.0, 400.0), pointer);
        assert!((before.0 - after.0).abs() < 0.01 && (before.1 - after.1).abs() < 0.01);
        for _ in 0..100 {
            view.handle_event(&UiEvent::Scroll { position: pointer, delta_y: 30.0 }, &options, true);
        }
        assert_eq!(view.target_scale, 8.0);
    }

    #[test]
    fn arrows_pan_and_plus_or_minus_zoom_about_the_center() {
        let mut view = GraphView::default();
        view.place(Rect::new(0.0, 0.0, 400.0, 400.0));
        assert!(view.handle_key(&UiEvent::Key { key: Key::ArrowLeft, shift: true, ctrl: false }));
        let before = view.pan;
        view.animate();
        assert!(view.pan.0 > before.0, "the view moves left, so the graph right");
        assert!(view.handle_key(&UiEvent::Text("+".into())));
        assert!((view.target_scale - 1.1).abs() < 1e-5);
        assert!(!view.handle_key(&UiEvent::Text("a".into())));
    }

    #[test]
    fn drawing_stops_once_nothing_has_moved_for_sixty_frames() {
        let mut view = GraphView::default();
        view.place(Rect::new(0.0, 0.0, 100.0, 100.0));
        let frames = (0..200).take_while(|_| view.animate()).count();
        assert_eq!(frames, IDLE_FRAMES as usize - 1);
    }

    /// The tab laid out at `size`, its texts measured with `ctx`'s fonts.
    fn tab_frame(ctx: &eframe::egui::Context, view: &GraphView, options: &GraphOptions, size: openide_ui::Size) -> openide_ui::Frame {
        use eframe::egui;
        let mut tree = openide_ui::UiTree::new(crate::graph_ui::build(options, &view.status, size.width, size.height));
        tree.set_reduced_motion(true);
        let fonts = ctx.clone();
        tree.set_text_measurer(move |text, size, _| {
            fonts.fonts_mut(|fonts| fonts.layout_no_wrap(text.into(), egui::FontId::proportional(size), egui::Color32::WHITE).size().x)
        });
        let fonts = ctx.clone();
        tree.set_text_height_measurer(move |size, _| fonts.fonts_mut(|fonts| fonts.row_height(&egui::FontId::proportional(size))));
        tree.layout(size)
    }

    fn text_width(ctx: &eframe::egui::Context, text: &str, size: f32) -> f32 {
        use eframe::egui;
        ctx.fonts_mut(|fonts| fonts.layout_no_wrap(text.to_owned(), egui::FontId::proportional(size), egui::Color32::WHITE).size().x)
    }

    /// This workspace's own graph, laid out to rest and fitted to `size`.
    fn review_graph(options: &GraphOptions, size: openide_ui::Size) -> GraphView {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut view = GraphView { loaded_at: Some(Instant::now()), ..GraphView::default() };
        view.data = load(&workspace).expect("the workspace's index");
        view.poll(options, Some("apps/agents-window/src/app.rs"));
        let mut simulation = graph_layout::Simulation::default();
        let ids = view.shown.nodes.iter().map(|node| view.data.files[node.file].path.clone()).collect();
        simulation.set_data(ids, &view.shown.links);
        while simulation.running() {
            simulation.tick();
        }
        view.positions = simulation.positions();
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for [x, y] in &view.positions {
            (x0, y0, x1, y1) = (x0.min(*x), y0.min(*y), x1.max(*x), y1.max(*y));
        }
        let fit = graph_layout::clamp_zoom((size.width / (x1 - x0)).min(size.height / (y1 - y0)) * 0.9);
        (view.scale, view.target_scale) = (fit, fit);
        view.pan = (size.width / 2.0 - (x0 + x1) / 2.0 * fit, size.height / 2.0 - (y0 + y1) / 2.0 * fit);
        view.rect = Some(Rect::new(0.0, 0.0, size.width, size.height));
        view
    }

    /// Lays out the tab at `size`, draws the graph in it and writes a PNG.
    fn export(view: &mut GraphView, options: &GraphOptions, size: openide_ui::Size, path: &Path) {
        use eframe::egui;
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        ctx.begin_pass(egui::RawInput::default());
        let mut frame = tab_frame(&ctx, view, options, size);
        let rect = canvas_rect(&frame.commands).expect("the tab shows its canvas");
        view.place(rect);
        let mut commands = vec![DrawCommand::FillRect { rect, color: crate::theme::BACKGROUND, radius: 0.0 }];
        commands.extend(view.paint(rect, options, &|text, size| text_width(&ctx, text, size)));
        assert!(splice(&mut frame.commands, commands));
        let svg = path.with_extension("svg");
        crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &svg, size.width, size.height).unwrap();
        let document = resvg::usvg::Tree::from_str(&std::fs::read_to_string(&svg).unwrap(), &crate::settings_ui::visual_qa::native_svg_options()).unwrap();
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
        resvg::render(&document, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
        pixmap.save_png(path).unwrap();
    }

    #[test]
    #[ignore = "opt-in review: the graph tab drawn from this workspace's own index"]
    fn export_graph_visual_review() {
        let directory = PathBuf::from(std::env::var("GRAPH_REVIEW_DIR").unwrap_or_else(|_| "/tmp/openade-graph-review".into()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut options = GraphOptions { arrows: true, ..GraphOptions::default() };
        let size = openide_ui::Size::new(1180.0, 820.0);
        let mut view = review_graph(&options, size);
        export(&mut view, &options, size, &directory.join("01-whole.png"));
        let hub = (0..view.shown.nodes.len()).max_by(|a, b| view.shown.nodes[*a].weight.total_cmp(&view.shown.nodes[*b].weight)).unwrap();
        view.hover = Some(hub);
        for _ in 0..80 {
            view.animate();
        }
        export(&mut view, &options, size, &directory.join("02-hover.png"));
        view.hover = None;
        let [hx, hy] = view.positions[hub];
        (view.scale, view.target_scale) = (1.0, 1.0);
        view.pan = (size.width / 2.0 - hx, size.height / 2.0 - hy);
        for _ in 0..80 {
            view.animate();
        }
        options.forces_open = true;
        options.display_open = true;
        export(&mut view, &options, size, &directory.join("03-zoom.png"));
        options.open = false;
        export(&mut view, &options, size, &directory.join("04-closed.png"));
        eprintln!("{} nodes, {} links -> {}", view.shown.nodes.len(), view.shown.links.len(), directory.display());
    }

    /// Frame times (the graph's paint instructions, egui's painting and its
    /// tessellation) and the font atlas, at rest and while zooming in to read
    /// the names. Debug builds are several times slower than the desktop's.
    #[test]
    #[ignore = "opt-in timing of the graph's frames"]
    fn measure_graph_frames() {
        use eframe::egui;
        let options = GraphOptions::default();
        let size = openide_ui::Size::new(1180.0, 820.0);
        let mut view = review_graph(&options, size);
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size.width, size.height))),
            ..Default::default()
        };
        let mut base = None;
        ctx.run_ui(input(), |ui| base = Some(tab_frame(ui.ctx(), &view, &options, size))).textures_delta.clear();
        let base = base.unwrap();
        let rect = canvas_rect(&base.commands).unwrap();
        view.place(rect);
        let fit = view.scale;
        for (name, target) in [("whole", fit), ("zoom to 3x", 3.0), ("back to whole", fit)] {
            view.target_scale = target;
            let mut times = Vec::new();
            let mut uploaded = 0;
            for _ in 0..90 {
                view.animate();
                let started = Instant::now();
                let mut output = ctx.run_ui(input(), |ui| {
                    let ctx = ui.ctx().clone();
                    let mut frame = base.clone();
                    splice(&mut frame.commands, view.paint(rect, &options, &|text, size| text_width(&ctx, text, size)));
                    crate::render::paint(ui, &frame);
                });
                let primitives = ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
                times.push(started.elapsed());
                drop(primitives);
                uploaded += output.textures_delta.set.iter().flat_map(|(_, deltas)| deltas.iter()).map(|delta| delta.image.width() * delta.image.height()).sum::<usize>();
                output.textures_delta.clear();
            }
            times.sort();
            let atlas = ctx.fonts(|fonts| fonts.font_image_size());
            eprintln!("{name}: median {:?}, worst {:?}, font atlas {atlas:?}, {uploaded} texture pixels uploaded", times[45], times[89]);
        }
    }

    #[test]
    fn options_survive_a_save_and_bad_files_give_the_defaults() {
        let root = tempfile::tempdir().unwrap();
        let options = GraphOptions { arrows: true, groups: vec![ColorGroup { query: "lang:rs".into(), color: [1, 2, 3] }], ..GraphOptions::default() };
        options.save(root.path()).unwrap();
        assert_eq!(GraphOptions::load(root.path()), options);
        std::fs::write(root.path().join(FILE_NAME), "not json").unwrap();
        assert_eq!(GraphOptions::load(root.path()), GraphOptions::default());
    }
}
