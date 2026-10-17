//! The workspace graph's layout: a force model with alpha cooling and
//! velocities, as in d3-force, with the forces, constants and control
//! mappings of the graph view's behaviour spec. It runs on its own thread and
//! publishes positions; the window only reads the latest ones.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Below this the layout has settled and its thread sleeps until news.
pub const ALPHA_MIN: f64 = 0.001;
/// Any change brings alpha back at least this high, and a drag holds it there.
pub const REHEAT: f64 = 0.3;
/// Of a body's velocity, the share each tick keeps.
const VELOCITY_KEEP: f64 = 0.6;
const COLLIDE_RADIUS: f64 = 60.0;
const COLLIDE_STRENGTH: f64 = 0.5;
/// Repulsion never grows as if two bodies were closer than 30.
const DISTANCE_MIN2: f64 = 30.0 * 30.0;
/// Barnes-Hut accuracy: a cell this small for its distance counts as one body.
const THETA2: f64 = 0.81;
/// About 60 ticks a second.
const TICK: Duration = Duration::from_micros(16_667);

/// Each tick takes alpha this share of the way to its target, so from 1 to a
/// target of 0 it reaches `ALPHA_MIN` in 300 ticks.
fn alpha_decay() -> f64 {
    1.0 - ALPHA_MIN.powf(1.0 / 300.0)
}

/// A node's radius from its number of links: the more connected, the larger,
/// between 8 and 30 before the user's size multiplier.
pub fn node_radius(weight: f32, multiplier: f32) -> f32 {
    multiplier * (3.0 * (weight + 1.0).sqrt()).clamp(8.0, 30.0)
}

/// Labels appear as the view zooms in; `fade` (-3 to 3) moves that threshold.
pub fn label_opacity(zoom: f32, fade: f32) -> f32 {
    (zoom.log2() + 1.0 - fade).clamp(0.0, 1.0)
}

/// Nodes grow on screen with the zoom, but only as its square root.
pub fn node_scale(zoom: f32) -> f32 {
    1.0 / zoom.sqrt()
}

/// The center and link sliders are exponential: half way is about 0.09, which
/// gives fine control over the small values that matter.
pub fn slider_to_force(slider: f64) -> f64 {
    (100f64.powf(slider) - 1.0) / 99.0
}

pub fn force_to_slider(force: f64) -> f64 {
    (1.0 + 99.0 * force).ln() / 100f64.ln()
}

/// The repulsion slider runs 0 to 20; its cube is the force (10 is 1000).
pub fn slider_to_repel(slider: f64) -> f64 {
    slider.powi(3)
}

pub fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(1.0 / 128.0, 8.0)
}

/// The zoom factor of one wheel event. `mode` is the DOM's: 0 pixels, 1 lines
/// (40 pixels each), 2 pages (800).
pub fn wheel_factor(delta_y: f32, mode: u8) -> f32 {
    let pixels = match mode {
        1 => delta_y * 40.0,
        2 => delta_y * 800.0,
        _ => delta_y,
    };
    1.5f32.powf(-pixels / 120.0)
}

pub fn arrow_opacity(zoom: f32) -> f32 {
    (2.0 * (zoom - 0.3)).clamp(0.0, 1.0)
}

/// A press that moved this little before its release opened the node; more
/// was a drag.
pub fn is_click(moved_px: f32) -> bool {
    moved_px <= 5.0
}

/// Timelapse steps per second for a graph of `steps` in all.
pub fn timelapse_speed(steps: usize) -> f64 {
    (0.5 * (steps as f64).sqrt()).clamp(5.0, 100.0)
}

/// The step the timelapse shows `elapsed_ms` after it started.
pub fn timelapse_step(speed: f64, elapsed_ms: f64) -> usize {
    1 + (speed * elapsed_ms / 1000.0).floor() as usize
}

/// What the filter and the color groups make of one file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Filtered<C> {
    Hidden,
    Colored(C),
    Plain,
}

/// `queries` in order, each with its color (none for the search filter) and
/// whether it matches: a failed filter hides the file, and the first group
/// that matches gives it its color.
pub fn resolve_filter<C>(queries: impl IntoIterator<Item = (Option<C>, bool)>) -> Filtered<C> {
    for (color, matches) in queries {
        match (color, matches) {
            (None, false) => return Filtered::Hidden,
            (Some(color), true) => return Filtered::Colored(color),
            _ => {}
        }
    }
    Filtered::Plain
}

/// The forces as the simulation takes them, not as the sliders show them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Forces {
    pub center: f64,
    pub repel: f64,
    pub link: f64,
    pub distance: f64,
}

impl Default for Forces {
    fn default() -> Self {
        Self {
            center: 0.1,
            repel: 1000.0,
            link: 1.0,
            distance: 250.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Body {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    held: Option<(f64, f64)>,
}

#[derive(Clone, Copy, Debug)]
struct Link {
    source: usize,
    target: usize,
    /// Inverse of the lesser degree of its ends, so hubs are not torn apart.
    strength: f64,
    /// The share of the correction the target takes: the lighter end moves more.
    bias: f64,
}

pub struct Simulation {
    ids: Vec<String>,
    bodies: Vec<Body>,
    links: Vec<Link>,
    forces: Forces,
    alpha: f64,
    alpha_target: f64,
    random: u64,
}

impl Default for Simulation {
    fn default() -> Self {
        Self::new(0x9e37_79b9_7f4a_7c15)
    }
}

impl Simulation {
    pub fn new(seed: u64) -> Self {
        Self {
            ids: Vec::new(),
            bodies: Vec::new(),
            links: Vec::new(),
            forces: Forces::default(),
            alpha: 1.0,
            alpha_target: 0.0,
            random: seed.max(1),
        }
    }

    pub fn running(&self) -> bool {
        // Repeated decay lands a hair above ALPHA_MIN on its 300th tick.
        self.alpha > ALPHA_MIN * (1.0 + 1e-9)
    }

    pub fn positions(&self) -> Vec<[f32; 2]> {
        self.bodies.iter().map(|body| [body.x as f32, body.y as f32]).collect()
    }

    fn random(&mut self) -> f64 {
        // xorshift64*: deterministic, so tests see the same layout every run.
        self.random ^= self.random >> 12;
        self.random ^= self.random << 25;
        self.random ^= self.random >> 27;
        (self.random.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn jiggle(&mut self) -> f64 {
        (self.random() - 0.5) * 1e-6
    }

    /// Replaces the graph. Nodes it already had keep their place; a new one
    /// starts among its placed neighbours, or else in a ring around the graph
    /// whose area grows with the number of newcomers, so density holds.
    pub fn set_data(&mut self, ids: Vec<String>, links: &[(usize, usize)]) {
        let old: HashMap<&str, Body> = self
            .ids
            .iter()
            .map(String::as_str)
            .zip(self.bodies.iter().copied())
            .collect();
        let mut bodies: Vec<Option<Body>> = ids.iter().map(|id| old.get(id.as_str()).copied()).collect();
        drop(old);
        let mut neighbours = vec![Vec::new(); ids.len()];
        for &(source, target) in links {
            if source != target && source < ids.len() && target < ids.len() {
                neighbours[source].push(target);
                neighbours[target].push(source);
            }
        }
        let newcomers = bodies.iter().filter(|body| body.is_none()).count();
        if newcomers > 0 {
            let radius = bodies
                .iter()
                .flatten()
                .map(|body| body.x * body.x + body.y * body.y)
                .fold(0.0, f64::max)
                .sqrt();
            let area = 60.0 * 60.0 * newcomers as f64;
            let ring = (area / std::f64::consts::PI + radius * radius).sqrt() - radius;
            let spread = area.sqrt();
            for index in 0..bodies.len() {
                if bodies[index].is_some() {
                    continue;
                }
                let placed: Vec<(f64, f64)> = neighbours[index]
                    .iter()
                    .filter_map(|&other| bodies[other].map(|body| (body.x, body.y)))
                    .collect();
                let (x, y) = if placed.is_empty() {
                    let angle = 2.0 * std::f64::consts::PI * self.random();
                    let distance = radius + self.random().sqrt() * ring;
                    (distance * angle.cos(), distance * angle.sin())
                } else {
                    let count = placed.len() as f64;
                    let (sx, sy) = placed.iter().fold((0.0, 0.0), |(ax, ay), (x, y)| (ax + x, ay + y));
                    (
                        sx / count + (self.random() - 0.5) * spread,
                        sy / count + (self.random() - 0.5) * spread,
                    )
                };
                bodies[index] = Some(Body { x, y, ..Body::default() });
            }
        }
        self.bodies = bodies.into_iter().map(Option::unwrap_or_default).collect();
        self.ids = ids;
        let mut degree = vec![0usize; self.ids.len()];
        let links: Vec<(usize, usize)> = links
            .iter()
            .copied()
            .filter(|&(source, target)| source != target && source < degree.len() && target < degree.len())
            .collect();
        for &(source, target) in &links {
            degree[source] += 1;
            degree[target] += 1;
        }
        self.links = links
            .into_iter()
            .map(|(source, target)| Link {
                source,
                target,
                strength: 1.0 / degree[source].min(degree[target]) as f64,
                bias: degree[source] as f64 / (degree[source] + degree[target]) as f64,
            })
            .collect();
        self.reheat();
    }

    pub fn set_forces(&mut self, forces: Forces) {
        self.forces = forces;
        self.reheat();
    }

    pub fn reheat(&mut self) {
        self.alpha = self.alpha.max(REHEAT);
    }

    /// Keeps a node under the pointer while the rest of the graph follows live.
    pub fn hold(&mut self, index: usize, x: f64, y: f64) {
        if let Some(body) = self.bodies.get_mut(index) {
            body.held = Some((x, y));
            self.alpha_target = REHEAT;
            self.reheat();
        }
    }

    pub fn release(&mut self, index: usize) {
        if let Some(body) = self.bodies.get_mut(index) {
            body.held = None;
        }
        self.alpha_target = 0.0;
    }

    pub fn tick(&mut self) {
        self.alpha += (self.alpha_target - self.alpha) * alpha_decay();
        let alpha = self.alpha;
        self.center(alpha);
        self.link(alpha);
        self.repel(alpha);
        self.collide();
        for body in &mut self.bodies {
            match body.held {
                Some((x, y)) => {
                    (body.x, body.y, body.vx, body.vy) = (x, y, 0.0, 0.0);
                }
                None => {
                    body.vx *= VELOCITY_KEEP;
                    body.vy *= VELOCITY_KEEP;
                    body.x += body.vx;
                    body.y += body.vy;
                }
            }
        }
    }

    /// Pulls each node towards the origin, on each axis apart.
    fn center(&mut self, alpha: f64) {
        let strength = self.forces.center * alpha;
        for body in &mut self.bodies {
            body.vx -= body.x * strength;
            body.vy -= body.y * strength;
        }
    }

    fn link(&mut self, alpha: f64) {
        for index in 0..self.links.len() {
            let link = self.links[index];
            let (source, target) = (self.bodies[link.source], self.bodies[link.target]);
            let mut x = target.x + target.vx - source.x - source.vx;
            let mut y = target.y + target.vy - source.y - source.vy;
            if x == 0.0 {
                x = self.jiggle();
            }
            if y == 0.0 {
                y = self.jiggle();
            }
            let length = (x * x + y * y).sqrt();
            let pull = (length - self.forces.distance) / length * alpha * self.forces.link * link.strength;
            let (x, y) = (x * pull, y * pull);
            let target = &mut self.bodies[link.target];
            target.vx -= x * link.bias;
            target.vy -= y * link.bias;
            let source = &mut self.bodies[link.source];
            source.vx += x * (1.0 - link.bias);
            source.vy += y * (1.0 - link.bias);
        }
    }

    /// Every node pushes every other, far groups taken as one body (Barnes-Hut).
    fn repel(&mut self, alpha: f64) {
        if self.bodies.len() < 2 {
            return;
        }
        // A weaker repulsion than 1 is not offered.
        let strength = -self.forces.repel.abs().max(1.0);
        let points: Vec<(f64, f64)> = self.bodies.iter().map(|body| (body.x, body.y)).collect();
        let tree = QuadTree::new(&points);
        let mut stack = Vec::new();
        for index in 0..points.len() {
            let (xi, yi) = points[index];
            let (mut vx, mut vy) = (0.0, 0.0);
            stack.push(0);
            while let Some(cell) = stack.pop() {
                let quad = &tree.cells[cell];
                let dx = quad.cx - xi;
                let dy = quad.cy - yi;
                let far = dx * dx + dy * dy;
                if quad.size * quad.size / THETA2 < far {
                    let distance = if far < DISTANCE_MIN2 { (DISTANCE_MIN2 * far).sqrt() } else { far };
                    let push = strength * quad.count as f64 * alpha / distance;
                    vx += dx * push;
                    vy += dy * push;
                    continue;
                }
                if let Some(children) = quad.children {
                    stack.extend(children.into_iter().filter(|&child| child != 0));
                    continue;
                }
                for &other in &tree.items[quad.items.clone()] {
                    if other == index {
                        continue;
                    }
                    let mut dx = points[other].0 - xi;
                    let mut dy = points[other].1 - yi;
                    if dx == 0.0 {
                        dx = self.jiggle();
                    }
                    if dy == 0.0 {
                        dy = self.jiggle();
                    }
                    let mut distance = dx * dx + dy * dy;
                    if distance < DISTANCE_MIN2 {
                        distance = (DISTANCE_MIN2 * distance).sqrt();
                    }
                    let push = strength * alpha / distance;
                    vx += dx * push;
                    vy += dy * push;
                }
            }
            self.bodies[index].vx += vx;
            self.bodies[index].vy += vy;
        }
    }

    /// Nodes do not overlap: each pair closer than two radii is pushed apart
    /// by half the overlap, judged where their velocities take them.
    fn collide(&mut self) {
        let reach = 2.0 * COLLIDE_RADIUS;
        let cell = |x: f64, y: f64| ((x / reach).floor() as i64, (y / reach).floor() as i64);
        let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (index, body) in self.bodies.iter().enumerate() {
            grid.entry(cell(body.x + body.vx, body.y + body.vy)).or_default().push(index);
        }
        for index in 0..self.bodies.len() {
            let xi = self.bodies[index].x + self.bodies[index].vx;
            let yi = self.bodies[index].y + self.bodies[index].vy;
            let (cx, cy) = cell(xi, yi);
            for gx in cx - 1..=cx + 1 {
                for gy in cy - 1..=cy + 1 {
                    let Some(others) = grid.get(&(gx, gy)) else {
                        continue;
                    };
                    for &other in others {
                        if other <= index {
                            continue;
                        }
                        let body = self.bodies[other];
                        let mut x = xi - body.x - body.vx;
                        let mut y = yi - body.y - body.vy;
                        let distance = x * x + y * y;
                        if distance >= reach * reach {
                            continue;
                        }
                        if x == 0.0 {
                            x = self.jiggle();
                        }
                        if y == 0.0 {
                            y = self.jiggle();
                        }
                        let distance = (x * x + y * y).sqrt();
                        let push = (reach - distance) / distance * COLLIDE_STRENGTH;
                        let (x, y) = (x * push * 0.5, y * push * 0.5);
                        self.bodies[index].vx += x;
                        self.bodies[index].vy += y;
                        self.bodies[other].vx -= x;
                        self.bodies[other].vy -= y;
                    }
                }
            }
        }
    }
}

/// A square quadtree over the bodies' positions, built in one pass. Each cell
/// knows how many bodies it holds and their centroid.
struct QuadTree {
    cells: Vec<Quad>,
    items: Vec<usize>,
}

struct Quad {
    size: f64,
    cx: f64,
    cy: f64,
    count: usize,
    /// Indices into `cells`; 0 (the root, never a child) marks an empty quarter.
    children: Option<[usize; 4]>,
    items: std::ops::Range<usize>,
}

impl QuadTree {
    /// Deeper than this, bodies left together share a leaf.
    const MAX_DEPTH: usize = 32;

    fn new(points: &[(f64, f64)]) -> Self {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for &(x, y) in points {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let size = (x1 - x0).max(y1 - y0).max(1.0);
        let mut tree = Self {
            cells: Vec::new(),
            items: (0..points.len()).collect(),
        };
        let all = tree.items.len();
        tree.build(points, 0..all, x0, y0, size, 0);
        tree
    }

    fn build(&mut self, points: &[(f64, f64)], range: std::ops::Range<usize>, x0: f64, y0: f64, size: f64, depth: usize) -> usize {
        let at = self.cells.len();
        let count = range.len();
        let (sx, sy) = self.items[range.clone()]
            .iter()
            .fold((0.0, 0.0), |(ax, ay), &index| (ax + points[index].0, ay + points[index].1));
        self.cells.push(Quad {
            size,
            cx: sx / count as f64,
            cy: sy / count as f64,
            count,
            children: None,
            items: range.clone(),
        });
        if count <= 1 || depth >= Self::MAX_DEPTH {
            return at;
        }
        let (mx, my) = (x0 + size / 2.0, y0 + size / 2.0);
        let quarter = |index: usize| usize::from(points[index].0 >= mx) | usize::from(points[index].1 >= my) << 1;
        self.items[range.clone()].sort_by_key(|&index| quarter(index));
        let mut children = [0; 4];
        let mut start = range.start;
        for (slot, child) in children.iter_mut().enumerate() {
            let end = start + self.items[start..range.end].iter().take_while(|&&index| quarter(index) == slot).count();
            if end > start {
                let half = size / 2.0;
                let cx0 = if slot & 1 == 1 { mx } else { x0 };
                let cy0 = if slot & 2 == 2 { my } else { y0 };
                *child = self.build(points, start..end, cx0, cy0, half, depth + 1);
            }
            start = end;
        }
        self.cells[at].children = Some(children);
        at
    }
}

/// What the layout thread is told.
pub enum Command {
    Data { ids: Vec<String>, links: Vec<(usize, usize)>, generation: u64 },
    Forces(Forces),
    Hold { index: usize, x: f64, y: f64 },
    Release { index: usize },
}

/// The latest positions, in the order of the data of `generation`.
#[derive(Clone, Debug, Default)]
pub struct Published {
    pub generation: u64,
    pub sequence: u64,
    pub positions: Arc<[[f32; 2]]>,
    pub moving: bool,
}

/// The layout thread. Dropping it ends the thread at its next tick or message.
pub struct LayoutWorker {
    commands: Sender<Command>,
    published: Arc<Mutex<Published>>,
}

impl LayoutWorker {
    /// `wake` is called after each tick's positions are published.
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Self {
        let (commands, inbox) = mpsc::channel();
        let published = Arc::new(Mutex::new(Published::default()));
        let shared = published.clone();
        std::thread::Builder::new()
            .name("graph-layout".into())
            .spawn(move || run(inbox, shared, wake))
            .expect("the graph layout thread starts");
        Self { commands, published }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// The positions published after `sequence`, if any.
    pub fn newer_than(&self, sequence: u64) -> Option<Published> {
        let published = self.published.lock().ok()?;
        (published.sequence > sequence).then(|| published.clone())
    }
}

fn run(inbox: Receiver<Command>, published: Arc<Mutex<Published>>, wake: impl Fn()) {
    let mut simulation = Simulation::default();
    let mut generation = 0;
    let mut sequence = 0;
    let mut next = Instant::now();
    loop {
        // Settled, it waits for news; moving, it takes what arrived and ticks on.
        let first = if simulation.running() && !simulation.bodies.is_empty() {
            match inbox.try_recv() {
                Ok(command) => Some(command),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        } else {
            match inbox.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            }
        };
        let mut pending = first;
        while let Some(command) = pending {
            match command {
                Command::Data { ids, links, generation: next_generation } => {
                    simulation.set_data(ids, &links);
                    generation = next_generation;
                }
                Command::Forces(forces) => simulation.set_forces(forces),
                Command::Hold { index, x, y } => simulation.hold(index, x, y),
                Command::Release { index } => simulation.release(index),
            }
            pending = match inbox.try_recv() {
                Ok(command) => Some(command),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            };
        }
        if !simulation.running() {
            continue;
        }
        simulation.tick();
        sequence += 1;
        if let Ok(mut shared) = published.lock() {
            *shared = Published {
                generation,
                sequence,
                positions: simulation.positions().into(),
                moving: simulation.running(),
            };
        }
        wake();
        next += TICK;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn cases(name: &str) -> Vec<Value> {
        let all: Value = serde_json::from_str(include_str!("graph_cases.json")).expect("valid cases");
        all[name].as_array().expect(name).clone()
    }

    fn number(case: &Value, key: &str) -> f64 {
        case[key].as_f64().unwrap_or_else(|| panic!("{key} in {case}"))
    }

    fn close(actual: f64, expected: f64, case: &Value) {
        assert!((actual - expected).abs() < 1e-6, "{actual} != {expected} for {case}");
    }

    #[test]
    fn the_pure_functions_give_every_spec_case() {
        for case in cases("radioNodo") {
            close(node_radius(number(&case, "peso") as f32, number(&case, "mult") as f32) as f64, number(&case, "esperado"), &case);
        }
        for case in cases("opacidadEtiquetas") {
            close(label_opacity(number(&case, "zoom") as f32, number(&case, "fundido") as f32) as f64, number(&case, "esperado"), &case);
        }
        for case in cases("escalaNodo") {
            close(node_scale(number(&case, "zoom") as f32) as f64, number(&case, "esperado"), &case);
        }
        for case in cases("sliderAFuerza") {
            close(slider_to_force(number(&case, "slider")), number(&case, "esperado"), &case);
        }
        for case in cases("fuerzaAslider") {
            close(force_to_slider(number(&case, "fuerza")), number(&case, "esperado"), &case);
        }
        for case in cases("sliderARepulsion") {
            close(slider_to_repel(number(&case, "slider")), number(&case, "esperado"), &case);
        }
        for case in cases("limitarZoom") {
            close(clamp_zoom(number(&case, "zoom") as f32) as f64, number(&case, "esperado"), &case);
        }
        for case in cases("factorRueda") {
            let factor = wheel_factor(number(&case, "deltaY") as f32, number(&case, "deltaMode") as u8);
            close(factor as f64, number(&case, "esperado"), &case);
        }
        for case in cases("opacidadFlecha") {
            close(arrow_opacity(number(&case, "zoom") as f32) as f64, number(&case, "esperado"), &case);
        }
        for case in cases("esClic") {
            assert_eq!(is_click(number(&case, "desplazamientoPx") as f32), case["esperado"].as_bool().unwrap(), "{case}");
        }
        for case in cases("velocidadTimelapse") {
            close(timelapse_speed(number(&case, "totalPasos") as usize), number(&case, "esperado"), &case);
        }
        for case in cases("pasoTimelapse") {
            let step = timelapse_step(number(&case, "velocidad"), number(&case, "ms"));
            assert_eq!(step as f64, number(&case, "esperado"), "{case}");
        }
        for case in cases("resolverFiltro") {
            let queries = case["consultas"].as_array().unwrap().iter().map(|query| {
                (query["color"].as_str().map(str::to_owned), query["coincide"].as_bool().unwrap())
            });
            let expected = match case["esperado"].as_str().unwrap() {
                "oculta" => Filtered::Hidden,
                "normal" => Filtered::Plain,
                color => Filtered::Colored(color.to_owned()),
            };
            assert_eq!(resolve_filter(queries), expected, "{case}");
        }
    }

    fn ring(count: usize) -> (Vec<String>, Vec<(usize, usize)>) {
        let ids = (0..count).map(|index| format!("n{index}")).collect();
        let links = (0..count).map(|index| (index, (index + 1) % count)).collect();
        (ids, links)
    }

    #[test]
    fn a_fresh_layout_cools_to_rest_in_exactly_300_ticks() {
        let mut simulation = Simulation::default();
        let (ids, links) = ring(12);
        simulation.set_data(ids, &links);
        simulation.alpha = 1.0;
        let mut ticks = 0;
        while simulation.running() {
            simulation.tick();
            ticks += 1;
        }
        assert_eq!(ticks, 300);
        assert!(simulation.positions().iter().flatten().all(|value| value.is_finite()));
    }

    #[test]
    fn changing_a_force_warms_a_settled_layout_again() {
        let mut simulation = Simulation::default();
        let (ids, links) = ring(5);
        simulation.set_data(ids, &links);
        while simulation.running() {
            simulation.tick();
        }
        simulation.set_forces(Forces { repel: 2000.0, ..Forces::default() });
        assert!(simulation.alpha >= REHEAT);
        assert!(simulation.running());
    }

    #[test]
    fn a_held_node_stays_under_the_pointer_until_released() {
        let mut simulation = Simulation::default();
        let (ids, links) = ring(6);
        simulation.set_data(ids, &links);
        simulation.hold(2, 400.0, -300.0);
        for _ in 0..30 {
            simulation.tick();
            assert_eq!(simulation.positions()[2], [400.0, -300.0]);
        }
        assert!(simulation.alpha >= REHEAT * 0.999, "a drag keeps the graph moving");
        simulation.release(2);
        for _ in 0..30 {
            simulation.tick();
        }
        assert_ne!(simulation.positions()[2], [400.0, -300.0]);
        while simulation.running() {
            simulation.tick();
        }
    }

    #[test]
    fn known_nodes_keep_their_place_and_newcomers_start_by_their_neighbours() {
        let mut simulation = Simulation::default();
        let (ids, links) = ring(8);
        simulation.set_data(ids.clone(), &links);
        for _ in 0..200 {
            simulation.tick();
        }
        let before = simulation.positions();
        let mut grown = ids.clone();
        grown.push("leaf".into());
        let mut grown_links = links.clone();
        grown_links.push((8, 3));
        simulation.set_data(grown, &grown_links);
        let after = simulation.positions();
        assert_eq!(&after[..8], &before[..]);
        let [x, y] = after[8];
        let [nx, ny] = after[3];
        // Within half the spread (60 for one newcomer) on each axis.
        assert!((x - nx).abs() <= 30.0 && (y - ny).abs() <= 30.0, "{:?} by {:?}", after[8], after[3]);
    }

    #[test]
    fn nodes_without_placed_neighbours_start_in_a_ring_around_the_graph() {
        let mut simulation = Simulation::default();
        simulation.set_data(vec!["a".into(), "b".into()], &[]);
        let radius = simulation
            .positions()
            .iter()
            .map(|[x, y]| (x * x + y * y).sqrt())
            .fold(0.0, f32::max);
        simulation.set_data(vec!["a".into(), "b".into(), "c".into()], &[]);
        let [x, y] = simulation.positions()[2];
        assert!((x * x + y * y).sqrt() >= radius - 1e-3);
    }

    #[test]
    fn repulsion_and_collision_spread_unlinked_nodes_apart() {
        let mut simulation = Simulation::default();
        let ids: Vec<String> = (0..30).map(|index| format!("n{index}")).collect();
        simulation.set_data(ids, &[]);
        while simulation.running() {
            simulation.tick();
        }
        let positions = simulation.positions();
        let closest = positions
            .iter()
            .enumerate()
            .flat_map(|(i, a)| positions[i + 1..].iter().map(move |b| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()))
            .fold(f32::MAX, f32::min);
        assert!(closest > 60.0, "closest pair {closest}");
    }

    #[test]
    fn the_worker_publishes_the_positions_of_the_latest_data() {
        let (woken, wake) = mpsc::channel();
        let worker = LayoutWorker::spawn(move || {
            let _ = woken.send(());
        });
        let (ids, links) = ring(4);
        worker.send(Command::Data { ids, links, generation: 7 });
        wake.recv_timeout(Duration::from_secs(5)).expect("a tick");
        let published = worker.newer_than(0).expect("positions");
        assert_eq!(published.generation, 7);
        assert_eq!(published.positions.len(), 4);
        assert!(worker.newer_than(u64::MAX).is_none());
    }
}
