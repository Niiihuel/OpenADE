//! The page as text the model reads: Chrome's accessibility tree, one line
//! per element, with a `[ref=eN]` on everything that can be pointed at.
//!
//! Roles, names and states are Chrome's own (`Accessibility.getFullAXTree`).
//! The DOM (`DOMSnapshot.captureSnapshot`) only adds what accessibility does
//! not carry: which elements are laid out and take pointer events, which show
//! a pointer cursor, link targets and placeholders. The text keeps the shape
//! of Playwright's `ariaSnapshot({ mode: 'ai' })`, which the tool descriptions
//! and the documentation describe.
//!
//! Everything here is pure: recorded protocol replies go in, text comes out.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

/// The accessibility tree is page-controlled; neither its depth nor a cycle
/// in it may exhaust the stack.
const MAX_TREE_DEPTH: usize = 400;
/// A name longer than this is left out of the line, as Playwright does.
const MAX_NAME_CHARS: usize = 900;

#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct AxTree {
    pub nodes: Vec<AxNode>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AxNode {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignored: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_reasons: Vec<AxProperty>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<AxValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<AxValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<AxValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<AxProperty>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_ids: Vec<String>,
    #[serde(rename = "backendDOMNodeId", skip_serializing_if = "Option::is_none")]
    pub backend: Option<i64>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct AxValue {
    #[serde(default)]
    pub value: Value,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct AxProperty {
    pub name: String,
    #[serde(default)]
    pub value: AxValue,
}

impl AxNode {
    fn role(&self) -> &str {
        self.role.as_ref().and_then(|role| role.value.as_str()).unwrap_or("")
    }

    fn name(&self) -> &str {
        self.name.as_ref().and_then(|name| name.value.as_str()).unwrap_or("")
    }

    fn property(&self, name: &str) -> Option<&Value> {
        self.properties
            .iter()
            .find(|property| property.name == name)
            .map(|property| &property.value.value)
    }

    fn flag(&self, name: &str) -> bool {
        self.property(name).is_some_and(|value| value == true)
    }

    fn token(&self, name: &str) -> &str {
        self.property(name).and_then(Value::as_str).unwrap_or("")
    }
}

/// The reply of `DOMSnapshot.captureSnapshot`: parallel arrays indexed by
/// node, with every string replaced by its index in `strings`.
#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct DomSnapshot {
    #[serde(default)]
    documents: Vec<DomDocument>,
    #[serde(default)]
    strings: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DomDocument {
    #[serde(default)]
    frame_id: i64,
    #[serde(default)]
    nodes: DomNodes,
    #[serde(default)]
    layout: DomLayout,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DomNodes {
    #[serde(default)]
    parent_index: Vec<i64>,
    #[serde(default)]
    node_type: Vec<i64>,
    #[serde(default)]
    node_name: Vec<i64>,
    #[serde(default)]
    node_value: Vec<i64>,
    #[serde(default)]
    backend_node_id: Vec<i64>,
    #[serde(default)]
    attributes: Vec<Vec<i64>>,
    #[serde(default)]
    content_document_index: Rare,
    #[serde(default)]
    pseudo_type: Rare,
    #[serde(default)]
    is_clickable: Rare,
}

/// A sparse column: `value[n]` belongs to node `index[n]`.
#[derive(Debug, Default, Deserialize, Serialize)]
struct Rare {
    #[serde(default)]
    index: Vec<i64>,
    #[serde(default)]
    value: Vec<i64>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DomLayout {
    #[serde(default)]
    node_index: Vec<i64>,
    #[serde(default)]
    styles: Vec<Vec<i64>>,
    #[serde(default)]
    bounds: Vec<Vec<f64>>,
}

/// The computed styles requested with the DOM snapshot, in this order.
pub(crate) const COMPUTED_STYLES: [&str; 4] = ["display", "visibility", "cursor", "pointer-events"];

/// What the DOM knows about the node behind an accessibility node.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DomNode {
    /// Upper case for HTML elements, `#text` for text.
    pub tag: String,
    /// The characters of a text node.
    pub text: Option<String>,
    /// Laid out with an area and `visibility: visible`.
    pub visible: bool,
    pub inline: bool,
    pub pointer_cursor: bool,
    /// `pointer-events: none`, its own or inherited.
    pub no_pointer_events: bool,
    /// A click listener is attached to the node itself.
    pub clickable: bool,
    /// Its one child node is text: a span around a few words.
    pub only_text: bool,
    /// A list or disclosure marker, which Playwright does not report.
    pub marker: bool,
    pub href: Option<String>,
    pub placeholder: Option<String>,
    /// The author wrote a `role` attribute.
    pub explicit_role: bool,
    /// For an `<iframe>` rendered in the same process: the frame it holds.
    pub content_frame: Option<String>,
}

pub(crate) type DomIndex = HashMap<i64, DomNode>;

impl DomSnapshot {
    /// One index per document, keyed by frame id.
    pub(crate) fn into_frames(self) -> HashMap<String, DomIndex> {
        let text = |index: i64| -> &str {
            usize::try_from(index)
                .ok()
                .and_then(|index| self.strings.get(index))
                .map_or("", String::as_str)
        };
        let frame_ids: Vec<&str> = self.documents.iter().map(|document| text(document.frame_id)).collect();
        let mut frames = HashMap::new();
        for document in &self.documents {
            let nodes = &document.nodes;
            let mut entries: Vec<DomNode> = (0..nodes.backend_node_id.len())
                .map(|at| {
                    let mut entry = DomNode {
                        tag: text(nodes.node_name.get(at).copied().unwrap_or(-1)).to_owned(),
                        ..DomNode::default()
                    };
                    if nodes.node_type.get(at) == Some(&3) {
                        entry.text = Some(text(nodes.node_value.get(at).copied().unwrap_or(-1)).to_owned());
                    }
                    for pair in nodes.attributes.get(at).map_or(&[][..], Vec::as_slice).chunks_exact(2) {
                        match text(pair[0]) {
                            "href" => entry.href = Some(text(pair[1]).to_owned()),
                            "placeholder" => entry.placeholder = Some(text(pair[1]).to_owned()),
                            "role" => entry.explicit_role = true,
                            _ => {}
                        }
                    }
                    entry
                })
                .collect();
            let count = entries.len();
            let at = |index: &i64| usize::try_from(*index).ok().filter(|at| *at < count);
            let mut children = vec![(0u32, 0u32); count];
            for (child, parent) in nodes.parent_index.iter().enumerate() {
                if let Some(parent) = at(parent) {
                    children[parent].0 += 1;
                    children[parent].1 += u32::from(nodes.node_type.get(child) == Some(&3));
                }
            }
            for (entry, counts) in entries.iter_mut().zip(children) {
                entry.only_text = counts == (1, 1);
            }
            for at in nodes.is_clickable.index.iter().filter_map(at) {
                entries[at].clickable = true;
            }
            for (index, kind) in nodes.pseudo_type.index.iter().zip(&nodes.pseudo_type.value) {
                if let Some(at) = at(index) {
                    entries[at].marker = text(*kind) == "marker";
                }
            }
            let content = &nodes.content_document_index;
            for (index, document) in content.index.iter().zip(&content.value) {
                let frame = usize::try_from(*document).ok().and_then(|at| frame_ids.get(at));
                if let (Some(at), Some(frame)) = (at(index), frame) {
                    entries[at].content_frame = Some((*frame).to_owned());
                }
            }
            let layout = &document.layout;
            for (row, index) in layout.node_index.iter().enumerate() {
                let Some(at) = at(index) else { continue };
                let style = |column: usize| {
                    layout
                        .styles
                        .get(row)
                        .and_then(|styles| styles.get(column))
                        .map_or("", |index| text(*index))
                };
                let area = layout
                    .bounds
                    .get(row)
                    .is_some_and(|bounds| bounds.len() == 4 && bounds[2] > 0.0 && bounds[3] > 0.0);
                let entry = &mut entries[at];
                entry.visible |= area && style(1) == "visible";
                entry.inline = style(0) == "inline";
                entry.pointer_cursor = style(2) == "pointer";
                entry.no_pointer_events = style(3) == "none";
            }
            let index: DomIndex = nodes.backend_node_id.iter().copied().zip(entries).collect();
            frames.insert(text(document.frame_id).to_owned(), index);
        }
        frames
    }
}

/// One document of the page: its accessibility tree, what the DOM adds, and
/// which of its `<iframe>` elements hold which other document.
#[derive(Debug, Default)]
pub(crate) struct Frame {
    pub ax: Vec<AxNode>,
    pub dom: DomIndex,
    /// The protocol session and frame id the document was read from.
    pub session: String,
    pub frame: String,
    /// Its slot in the `RefTable`.
    pub document: usize,
    /// Backend node id of an `<iframe>` to the index of the frame inside it.
    pub children: HashMap<i64, usize>,
}

/// The protocol replies one snapshot was built from, kept so that the text
/// can be reproduced without a browser.
#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct Recorded {
    /// One `DOMSnapshot.captureSnapshot` reply per session.
    doms: Vec<DomSnapshot>,
    frames: Vec<RecordedFrame>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct RecordedFrame {
    frame: String,
    /// Which of `doms` covers this frame.
    dom: usize,
    ax: Vec<AxNode>,
    children: HashMap<i64, usize>,
}

impl Recorded {
    pub(crate) fn push_dom(&mut self, dom: DomSnapshot) -> usize {
        self.doms.push(dom);
        self.doms.len() - 1
    }

    pub(crate) fn push_frame(&mut self, frame: &Frame, dom: usize, ax: Vec<AxNode>) {
        self.frames.push(RecordedFrame {
            frame: frame.frame.clone(),
            dom,
            ax,
            children: frame.children.clone(),
        });
    }

    /// The frames as `render` takes them, each a document of its own.
    #[cfg(test)]
    pub(crate) fn into_frames(self, refs: &mut RefTable) -> Vec<Frame> {
        let mut doms: Vec<HashMap<String, DomIndex>> = self.doms.into_iter().map(DomSnapshot::into_frames).collect();
        self.frames
            .into_iter()
            .map(|recorded| Frame {
                dom: doms.get_mut(recorded.dom).and_then(|dom| dom.remove(&recorded.frame)).unwrap_or_default(),
                document: refs.document(&DocumentKey {
                    frame: recorded.frame.clone(),
                    ..DocumentKey::default()
                }),
                ax: recorded.ax,
                session: String::new(),
                frame: recorded.frame,
                children: recorded.children,
            })
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapshotOptions {
    /// Keep only what can be acted on, and the named containers around it.
    pub interactive_only: bool,
    /// Elements nested deeper than this are left out. The outermost are level 0.
    pub max_depth: Option<u32>,
    /// A CSS selector: only the subtree of the one element it matches.
    pub selector: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tristate {
    #[default]
    Off,
    On,
    Mixed,
}

#[derive(Debug)]
enum Item {
    Text(String),
    Node(Box<Node>),
}

#[derive(Debug, Default)]
struct Node {
    role: String,
    name: String,
    /// The name Chrome reported, which identifies the element across snapshots
    /// even when the line leaves it out as redundant.
    identity: String,
    document: usize,
    backend: Option<i64>,
    checked: Tristate,
    pressed: Tristate,
    disabled: bool,
    expanded: bool,
    active: bool,
    selected: bool,
    invalid: &'static str,
    level: u64,
    url: Option<String>,
    placeholder: Option<String>,
    /// Laid out and reachable by the pointer: it gets a ref.
    target: bool,
    pointer_cursor: bool,
    /// Acted on rather than read: a control, or something that takes clicks.
    interactive: bool,
    /// All the text below, as an accessible name computed from content would read it.
    text: String,
    children: Vec<Item>,
}

impl Node {
    fn has_props(&self) -> bool {
        self.url.is_some() || self.placeholder.is_some()
    }

    fn single_text(&self) -> Option<&str> {
        match self.children.as_slice() {
            [Item::Text(text)] => Some(text),
            _ => None,
        }
    }

    /// Playwright's "click target root": the outermost element that both has
    /// a ref and shows the pointer cursor.
    fn click_root(&self, inside_pointer: bool) -> bool {
        self.target && self.pointer_cursor && !inside_pointer
    }
}

/// Collapses whitespace the way an accessible name is compared.
pub(crate) fn normalize(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    for word in text
        .split(|character: char| character.is_whitespace())
        .map(|word| word.replace(['\u{200b}', '\u{ad}'], ""))
        .filter(|word| !word.is_empty())
    {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(&word);
    }
    normalized
}

/// Chrome's role names are ARIA's where ARIA has one. Its internal roles are
/// capitalised; the ones Playwright reports under another name are mapped
/// and the rest are plain containers.
pub(crate) fn aria_role(role: &str) -> &str {
    match role {
        "image" | "SvgRoot" => "img",
        "Iframe" | "IframePresentational" => "iframe",
        "DisclosureTriangle" | "DisclosureTriangleGrouped" => "button",
        "Date" | "DateTime" | "InputTime" | "ColorWell" => "textbox",
        "LayoutTable" => "table",
        "LayoutTableRow" => "row",
        "LayoutTableCell" => "cell",
        "Details" => "group",
        "MenuListOption" => "option",
        _ if role.starts_with(|first: char| first.is_ascii_uppercase()) => "generic",
        _ => role,
    }
}

/// Roles whose accessible name ARIA computes from their content. Chrome also
/// names a few others that way (a term, a caption); for those the content is
/// shown as content.
const NAMED_FROM_CONTENT: &[&str] = &[
    "button",
    "cell",
    "checkbox",
    "columnheader",
    "gridcell",
    "heading",
    "link",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "row",
    "rowheader",
    "switch",
    "tab",
    "tooltip",
    "treeitem",
];

const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "checkbox",
    "combobox",
    "link",
    "listbox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "textbox",
    "treeitem",
];

/// Reasons Chrome gives for leaving a node out that mean the user cannot
/// perceive or reach it either.
const HIDDEN_REASONS: &[&str] = &[
    "activeFullscreenElement",
    "activeModalDialog",
    "ancestorIsLeafNode",
    "ariaHiddenElement",
    "ariaHiddenSubtree",
    "emptyAlt",
    "inertElement",
    "inertSubtree",
    "notRendered",
    "notVisible",
];

/// Reasons that remove the element's own semantics but keep its content.
const TRANSPARENT_REASONS: &[&str] = &["inheritsPresentation", "presentationalRole", "probablyPresentational"];

struct Builder<'a> {
    frames: &'a [Frame],
    /// Frames already placed, so two `<iframe>` entries cannot claim one.
    placed: Vec<bool>,
}

struct FrameBuilder<'a, 'b> {
    builder: &'b mut Builder<'a>,
    frame: &'a Frame,
    by_id: HashMap<&'a str, usize>,
    visited: Vec<bool>,
    root_focused: bool,
}

impl<'a> Builder<'a> {
    /// The items of one frame, from `root` (a backend node id) or its top.
    fn frame(&mut self, at: usize, root: Option<i64>, depth: usize, inside_pointer: bool) -> Option<Vec<Item>> {
        let frame = self.frames.get(at)?;
        if std::mem::replace(self.placed.get_mut(at)?, true) {
            return Some(Vec::new());
        }
        let by_id = frame.ax.iter().enumerate().map(|(at, node)| (node.node_id.as_str(), at)).collect();
        let start = match root {
            Some(backend) => frame.ax.iter().position(|node| node.backend == Some(backend))?,
            None => frame.ax.iter().position(|node| node.parent_id.is_none())?,
        };
        let root_focused = frame.ax.iter().any(|node| node.role() == "RootWebArea" && node.flag("focused"));
        let mut items = Vec::new();
        FrameBuilder {
            builder: self,
            frame,
            by_id,
            visited: vec![false; frame.ax.len()],
            root_focused,
        }
        .convert(start, depth, inside_pointer, &mut items);
        merge_text(&mut items);
        Some(items)
    }
}

impl FrameBuilder<'_, '_> {
    fn convert(&mut self, at: usize, depth: usize, inside_pointer: bool, out: &mut Vec<Item>) {
        if depth > MAX_TREE_DEPTH || std::mem::replace(&mut self.visited[at], true) {
            return;
        }
        let node = &self.frame.ax[at];
        let dom = node.backend.and_then(|backend| self.frame.dom.get(&backend));
        if dom.is_some_and(|dom| dom.marker) {
            return;
        }
        if node.ignored {
            let reason = |list: &[&str]| node.ignored_reasons.iter().any(|reason| list.contains(&reason.name.as_str()));
            if reason(HIDDEN_REASONS) {
                return;
            }
            // Text that only names its control (a label's) is ignored by
            // accessibility and still read on the page.
            if let Some(text) = dom.and_then(|dom| dom.text.as_deref()) {
                out.push(Item::Text(text.to_owned()));
                return;
            }
            let element = dom.is_some_and(|dom| !dom.tag.starts_with('#'));
            if !element || reason(TRANSPARENT_REASONS) {
                return self.hoist(at, depth, inside_pointer, out);
            }
            return match dom.map_or("", |dom| dom.tag.as_str()) {
                // Chrome leaves a table body out of a real table, which
                // Playwright reports; a body without rows is only layout.
                "TBODY" | "THEAD" | "TFOOT" if self.has_rows(at) => {
                    self.element(at, "rowgroup", depth, inside_pointer, out)
                }
                "TBODY" | "THEAD" | "TFOOT" | "TR" => self.hoist(at, depth, inside_pointer, out),
                _ => self.element(at, "generic", depth, inside_pointer, out),
            };
        }
        let tag = dom.map_or("", |dom| dom.tag.as_str());
        let explicit = dom.is_some_and(|dom| dom.explicit_role);
        match node.role() {
            "StaticText" => out.push(Item::Text(node.name().to_owned())),
            "InlineTextBox" | "ListMarker" => {}
            "LineBreak" => out.push(Item::Text(" ".into())),
            "RootWebArea" | "MenuListPopup" | "none" | "presentation" | "" => {
                self.hoist(at, depth, inside_pointer, out)
            }
            // A cell that is not a cell belongs to a table used for layout.
            "generic" if matches!(tag, "TD" | "TH") => self.hoist(at, depth, inside_pointer, out),
            // Chrome groups anything draggable; only an authored or native group is one.
            "group" if !explicit && node.name().is_empty() && !matches!(tag, "" | "FIELDSET" | "DETAILS" | "OPTGROUP" | "ADDRESS" | "HGROUP") => {
                self.element(at, "generic", depth, inside_pointer, out)
            }
            role => self.element(at, aria_role(role), depth, inside_pointer, out),
        }
    }

    fn has_rows(&self, at: usize) -> bool {
        self.frame.ax[at].child_ids.iter().any(|child| {
            self.by_id
                .get(child.as_str())
                .is_some_and(|child| !self.frame.ax[*child].ignored && self.frame.ax[*child].role() == "row")
        })
    }

    /// The children of a node that is not reported itself.
    fn hoist(&mut self, at: usize, depth: usize, inside_pointer: bool, out: &mut Vec<Item>) {
        let node = &self.frame.ax[at];
        // A block keeps its text apart from its neighbours'.
        let block = node
            .backend
            .and_then(|backend| self.frame.dom.get(&backend))
            .is_some_and(|dom| !dom.inline && !dom.tag.starts_with('#'));
        if block {
            out.push(Item::Text(" ".into()));
        }
        self.children(at, depth, inside_pointer, out);
        if block {
            out.push(Item::Text(" ".into()));
        }
    }

    fn children(&mut self, at: usize, depth: usize, inside_pointer: bool, out: &mut Vec<Item>) {
        for child in &self.frame.ax[at].child_ids {
            if let Some(child) = self.by_id.get(child.as_str()).copied() {
                self.convert(child, depth + 1, inside_pointer, out);
            }
        }
    }

    fn element(&mut self, at: usize, role: &str, depth: usize, inside_pointer: bool, out: &mut Vec<Item>) {
        let ax = &self.frame.ax[at];
        let dom = ax.backend.and_then(|backend| self.frame.dom.get(&backend));
        let tristate = |name: &str| match ax.token(name) {
            "true" => Tristate::On,
            "mixed" => Tristate::Mixed,
            _ => Tristate::Off,
        };
        let identity = normalize(ax.name());
        let editable = ax.token("editable");
        let mut node = Node {
            role: role.to_owned(),
            name: identity.clone(),
            identity,
            document: self.frame.document,
            backend: ax.backend,
            checked: tristate("checked"),
            pressed: tristate("pressed"),
            disabled: ax.flag("disabled"),
            expanded: ax.flag("expanded"),
            selected: ax.flag("selected"),
            active: ax.flag("focused") || (self.root_focused && dom.is_some_and(|dom| dom.tag == "BODY")),
            invalid: match ax.token("invalid") {
                "true" => "true",
                "grammar" => "grammar",
                "spelling" => "spelling",
                _ => "",
            },
            level: if role == "heading" { ax.property("level").and_then(Value::as_u64).unwrap_or(0) } else { 0 },
            // Without the DOM every reported element is assumed reachable.
            target: ax.backend.is_some()
                && (self.frame.dom.is_empty() || dom.is_some_and(|dom| dom.visible && !dom.no_pointer_events)),
            pointer_cursor: dom.is_some_and(|dom| dom.pointer_cursor),
            ..Node::default()
        };
        // A label counts as clickable for Chrome only because of the field it names.
        let listens = dom.is_some_and(|dom| dom.clickable && dom.tag != "LABEL");
        node.interactive = INTERACTIVE_ROLES.contains(&role)
            || editable == "richtext"
            || (node.target && (node.pointer_cursor || listens));
        if role == "link" {
            let href = dom.and_then(|dom| dom.href.clone());
            node.url = href.or_else(|| ax.property("url").and_then(Value::as_str).map(str::to_owned)).map(short_url);
        }
        // A native field shows its value, not the editor Chrome nests in it.
        // Chrome already masks a password here.
        let field = matches!(role, "spinbutton" | "slider")
            || (matches!(role, "textbox" | "searchbox" | "combobox") && editable == "plaintext");
        if field {
            node.placeholder = dom.and_then(|dom| dom.placeholder.clone()).filter(|text| *text != node.name);
            match ax.value.as_ref().map(|value| &value.value) {
                Some(Value::String(value)) => node.children.push(Item::Text(value.clone())),
                Some(Value::Number(value)) => node.children.push(Item::Text(value.to_string())),
                _ => {}
            }
        } else if role == "iframe" {
            let inside = inside_pointer || node.click_root(inside_pointer);
            let child = ax.backend.and_then(|backend| self.frame.children.get(&backend)).copied();
            if let Some(items) = child.and_then(|child| self.builder.frame(child, None, depth + 1, inside)) {
                node.children = items;
            }
        } else {
            let inside = inside_pointer || node.click_root(inside_pointer);
            self.children(at, depth, inside, &mut node.children);
        }
        // A span around a few words is its words, unless it is what takes the click.
        let wraps_text = role == "generic"
            && dom.is_some_and(|dom| dom.inline && dom.only_text)
            && !node.interactive
            && node.children.iter().all(|child| matches!(child, Item::Text(_)));
        if wraps_text {
            out.append(&mut node.children);
            return;
        }
        finish(node, field, inside_pointer, out);
    }
}

/// A `data:` URL is shown up to its comma.
fn short_url(url: String) -> String {
    match url.find(',') {
        Some(comma) if url.starts_with("data:") => format!("{}…", &url[..=comma]),
        _ => url,
    }
}

fn merge_text(items: &mut Vec<Item>) {
    let mut merged: Vec<Item> = Vec::with_capacity(items.len());
    let mut buffer = String::new();
    let flush = |buffer: &mut String, merged: &mut Vec<Item>| {
        let text = normalize(buffer);
        buffer.clear();
        if !text.is_empty() {
            merged.push(Item::Text(text));
        }
    };
    for item in items.drain(..) {
        match item {
            Item::Text(text) => buffer.push_str(&text),
            node => {
                flush(&mut buffer, &mut merged);
                merged.push(node);
            }
        }
    }
    flush(&mut buffer, &mut merged);
    *items = merged;
}

/// Applies, bottom-up, what Playwright's AI mode does to a finished element:
/// drop what only repeats a name, unwrap containers that add nothing.
fn finish(mut node: Node, field: bool, inside_pointer: bool, out: &mut Vec<Item>) {
    // A plain container whose only words are this element's name repeats it.
    let repeats = |child: &Node| {
        let text = match (child.name.as_str(), child.children.as_slice()) {
            ("", [Item::Text(text)]) => text.as_str(),
            (name, []) => name,
            _ => "",
        };
        child.role == "generic" && !child.active && !child.has_props() && !text.is_empty() && text == node.name
    };
    if !node.name.is_empty() {
        node.children.retain(|child| !matches!(child, Item::Node(child) if repeats(child)));
    }
    merge_text(&mut node.children);
    if !field {
        let parts: Vec<&str> = node
            .children
            .iter()
            .map(|child| match child {
                Item::Text(text) => text.as_str(),
                Item::Node(child) if child.text.is_empty() => child.name.as_str(),
                Item::Node(child) => child.text.as_str(),
            })
            .collect();
        node.text = normalize(&parts.join(" "));
    }
    let elements = node.children.iter().any(|child| matches!(child, Item::Node(_)));
    if !node.name.is_empty() && node.name == node.text && !NAMED_FROM_CONTENT.contains(&node.role.as_str()) {
        node.name.clear();
    } else if node.single_text() == Some(node.name.as_str()) {
        node.children.clear();
    } else if !node.name.is_empty() && elements {
        // The name was computed from content that is listed right below.
        let below = |child: &Item| matches!(child, Item::Node(child) if child.role != "generic" && child.text == node.name);
        if node.text == node.name || node.children.iter().any(below) {
            node.name.clear();
        }
    }
    if node.role == "img" && node.name.is_empty() && node.children.is_empty() && !node.click_root(inside_pointer) {
        return;
    }
    if node.role == "generic" && !node.has_props() {
        // A container around one plain container around some words is those words.
        let inner = match node.children.as_mut_slice() {
            [Item::Node(child)]
                if child.role == "generic"
                    && child.name.is_empty()
                    && !child.active
                    && !child.has_props()
                    && child.single_text().is_some() =>
            {
                Some(std::mem::take(&mut child.children))
            }
            _ => None,
        };
        if let Some(inner) = inner {
            node.children = inner;
        }
    }
    let unwrap = node.role == "generic"
        && node.name.is_empty()
        && node.children.len() <= 1
        && node.children.iter().all(|child| matches!(child, Item::Node(child) if child.target))
        && !(node.children.is_empty() && node.click_root(inside_pointer));
    if unwrap {
        out.append(&mut node.children);
    } else {
        out.push(Item::Node(Box::new(node)));
    }
}

/// Keeps what can be acted on. A container survives only as far as it names
/// the group its controls belong to, or is the frame they live in.
fn interactive_only(items: Vec<Item>, inside: bool) -> Vec<Item> {
    let mut kept = Vec::new();
    for item in items {
        match item {
            Item::Text(text) if inside => kept.push(Item::Text(text)),
            Item::Text(_) => {}
            Item::Node(mut node) => {
                let children = std::mem::take(&mut node.children);
                node.children = interactive_only(children, inside || node.interactive);
                let container = !node.children.is_empty() && (!node.name.is_empty() || node.role == "iframe");
                if node.interactive || inside || container {
                    kept.push(Item::Node(node));
                } else {
                    kept.append(&mut node.children);
                }
            }
        }
    }
    kept
}

/// The refs of every snapshot so far that still name an element.
///
/// A ref belongs to one document. An element keeps its ref for as long as it
/// stays in that document with the same role and name; a new document never
/// reuses the prefix of an old one, so a ref that outlived its page cannot
/// point at something else.
#[derive(Debug, Default)]
pub(crate) struct RefTable {
    documents: Vec<DocumentRefs>,
    created: u32,
}

/// What identifies a document: where it lives and which load produced it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DocumentKey {
    pub session: String,
    pub frame: String,
    pub loader: String,
}

#[derive(Debug, Default)]
struct DocumentRefs {
    key: DocumentKey,
    /// Empty for the first document, `f<N>` afterwards.
    prefix: String,
    next: u32,
    by_node: HashMap<i64, Assigned>,
    by_number: HashMap<u32, i64>,
    seen: HashSet<i64>,
    visited: bool,
}

#[derive(Debug)]
struct Assigned {
    number: u32,
    role: String,
    name: String,
}

impl RefTable {
    /// The slot of `key`, created on first sight.
    pub(crate) fn document(&mut self, key: &DocumentKey) -> usize {
        let at = self.documents.iter().position(|document| document.key == *key).unwrap_or_else(|| {
            let prefix = if self.created == 0 { String::new() } else { format!("f{}", self.created) };
            self.created += 1;
            self.documents.push(DocumentRefs {
                key: key.clone(),
                prefix,
                ..DocumentRefs::default()
            });
            self.documents.len() - 1
        });
        self.documents[at].visited = true;
        at
    }

    fn assign(&mut self, document: usize, backend: i64, role: &str, name: &str) -> Option<String> {
        let refs = self.documents.get_mut(document)?;
        refs.seen.insert(backend);
        let reusable = refs.by_node.get(&backend).filter(|assigned| assigned.role == role && assigned.name == name);
        let number = match reusable {
            Some(assigned) => assigned.number,
            None => {
                refs.next += 1;
                if let Some(old) = refs.by_node.insert(
                    backend,
                    Assigned {
                        number: refs.next,
                        role: role.to_owned(),
                        name: name.to_owned(),
                    },
                ) {
                    refs.by_number.remove(&old.number);
                }
                refs.by_number.insert(refs.next, backend);
                refs.next
            }
        };
        Some(format!("{}e{number}", refs.prefix))
    }

    /// Ends a snapshot. A whole-page one replaces the set of live refs, so
    /// what it did not list is stale; a partial one only adds to the set.
    pub(crate) fn settle(&mut self, whole_page: bool) {
        if whole_page {
            self.documents.retain(|document| document.visited);
            for document in &mut self.documents {
                let seen = std::mem::take(&mut document.seen);
                document.by_node.retain(|backend, _| seen.contains(backend));
                document.by_number.retain(|_, backend| seen.contains(backend));
            }
        }
        for document in &mut self.documents {
            document.seen.clear();
            document.visited = false;
        }
    }

    /// The document and backend node a ref names, if it is still listed.
    pub(crate) fn resolve(&self, reference: &str) -> Option<(&DocumentKey, i64)> {
        let (prefix, number) = reference.rsplit_once('e')?;
        let number: u32 = number.parse().ok()?;
        let document = self.documents.iter().find(|document| document.prefix == prefix)?;
        Some((&document.key, *document.by_number.get(&number)?))
    }

    /// Forgets the refs of documents that no longer exist.
    pub(crate) fn retain(&mut self, mut alive: impl FnMut(&DocumentKey) -> bool) {
        self.documents.retain(|document| alive(&document.key));
    }
}

/// Renders the page. `root` limits it to the subtree of one backend node of
/// the first frame; `None` is returned when that node is not in the tree.
pub(crate) fn render(
    frames: &[Frame],
    root: Option<i64>,
    options: &SnapshotOptions,
    refs: &mut RefTable,
) -> Option<String> {
    let mut builder = Builder {
        frames,
        placed: vec![false; frames.len()],
    };
    let mut items = builder.frame(0, root, 0, false)?;
    if options.interactive_only {
        items = interactive_only(items, false);
    }
    let mut writer = Writer {
        lines: String::new(),
        refs,
        max_depth: options.max_depth,
    };
    for item in &items {
        writer.item(item, 0, false);
    }
    let whole_page = root.is_none() && !options.interactive_only && options.max_depth.is_none();
    writer.refs.settle(whole_page);
    let mut text = writer.lines;
    text.truncate(text.trim_end_matches('\n').len());
    Some(text)
}

/// The backend nodes of the first frame whose role and accessible name are
/// exactly these: the same words the snapshot shows.
pub(crate) fn matching(frame: &Frame, role: &str, name: &str) -> Vec<i64> {
    frame
        .ax
        .iter()
        .filter(|node| !node.ignored && aria_role(node.role()) == role && normalize(node.name()) == name)
        .filter_map(|node| node.backend)
        .collect()
}

struct Writer<'a> {
    lines: String,
    refs: &'a mut RefTable,
    max_depth: Option<u32>,
}

impl Writer<'_> {
    fn indent(&mut self, depth: u32) {
        for _ in 0..depth {
            self.lines.push_str("  ");
        }
    }

    fn text(&mut self, text: &str, depth: u32) {
        if text.is_empty() {
            return;
        }
        self.indent(depth);
        let _ = writeln!(self.lines, "- text: {}", yaml_value(text));
    }

    fn item(&mut self, item: &Item, depth: u32, inside_pointer: bool) {
        let node = match item {
            Item::Text(text) => return self.text(text, depth),
            Item::Node(node) => node,
        };
        let mut key = node.role.clone();
        if !node.name.is_empty() && node.name.chars().count() <= MAX_NAME_CHARS {
            key.push(' ');
            key.push_str(&Value::from(node.name.as_str()).to_string());
        }
        for (present, flag) in [
            (node.checked == Tristate::Mixed, " [checked=mixed]"),
            (node.checked == Tristate::On, " [checked]"),
            (node.disabled, " [disabled]"),
            (node.expanded, " [expanded]"),
            (node.active, " [active]"),
            (node.invalid == "grammar", " [invalid=grammar]"),
            (node.invalid == "spelling", " [invalid=spelling]"),
            (node.invalid == "true", " [invalid]"),
        ] {
            if present {
                key.push_str(flag);
            }
        }
        if node.level > 0 {
            let _ = write!(key, " [level={}]", node.level);
        }
        for (present, flag) in [
            (node.pressed == Tristate::Mixed, " [pressed=mixed]"),
            (node.pressed == Tristate::On, " [pressed]"),
            (node.selected, " [selected]"),
        ] {
            if present {
                key.push_str(flag);
            }
        }
        let pointer = node.target && node.pointer_cursor;
        let reference = match (node.target, node.backend) {
            (true, Some(backend)) => self.refs.assign(node.document, backend, &node.role, &node.identity),
            _ => None,
        };
        if let Some(reference) = reference {
            let _ = write!(key, " [ref={reference}]");
            if pointer && !inside_pointer {
                key.push_str(" [cursor=pointer]");
            }
        }
        self.indent(depth);
        self.lines.push_str("- ");
        self.lines.push_str(&yaml_key(&key));
        let limit = self.max_depth.is_some_and(|limit| depth >= limit);
        let props = [("url", &node.url), ("placeholder", &node.placeholder)];
        match node.single_text() {
            Some(text) if !node.has_props() => {
                let _ = writeln!(self.lines, ": {}", yaml_value(text));
                return;
            }
            None if !node.has_props() && (node.children.is_empty() || limit) => {
                self.lines.push('\n');
                return;
            }
            _ => self.lines.push_str(":\n"),
        }
        for (name, value) in props {
            if let Some(value) = value {
                self.indent(depth + 1);
                let _ = writeln!(self.lines, "- /{name}: {}", yaml_value(value));
            }
        }
        if let Some(text) = node.single_text() {
            self.text(text, depth + 1);
        } else if !limit {
            for child in &node.children {
                self.item(child, depth + 1, inside_pointer || pointer);
            }
        }
    }
}

/// Whether a YAML reader would take the plain scalar for something else.
fn needs_quotes(text: &str) -> bool {
    let Some(first) = text.chars().next() else {
        return true;
    };
    let last = text.chars().next_back().unwrap_or(first);
    let control = |character: char| {
        matches!(character, '\0'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
    };
    let mut pairs = text.chars().zip(text.chars().skip(1).map(Some).chain([None]));
    first.is_whitespace()
        || last.is_whitespace()
        || text.chars().any(|character| control(character) || matches!(character, '\n' | '\r' | '{' | '}' | '`'))
        || "-&*],?!>|@\"'#%[".contains(first)
        || pairs.any(|(character, next)| {
            (character == ':' && next.is_none_or(char::is_whitespace)) || (character.is_whitespace() && next == Some('#'))
        })
        || looks_numeric(text)
        || ["y", "n", "yes", "no", "true", "false", "on", "off", "null"].contains(&text.to_lowercase().as_str())
}

/// Whether JavaScript's `Number()` accepts the text, which is what makes a
/// YAML reader see a number.
fn looks_numeric(text: &str) -> bool {
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    if unsigned == "Infinity" {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(digits) = lower.strip_prefix(prefix) {
            return !digits.is_empty() && digits.chars().all(|digit| digit.is_digit(radix));
        }
    }
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (unsigned, None),
    };
    let digits = |text: &str| text.bytes().all(|byte| byte.is_ascii_digit());
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mantissa_ok = digits(whole) && digits(fraction) && !(whole.is_empty() && fraction.is_empty());
    let exponent_ok = exponent.is_none_or(|exponent| {
        let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        !exponent.is_empty() && digits(exponent)
    });
    mantissa_ok && exponent_ok
}

fn yaml_key(key: &str) -> String {
    if needs_quotes(key) {
        format!("'{}'", key.replace('\'', "''"))
    } else {
        key.to_owned()
    }
}

fn yaml_value(text: &str) -> String {
    if !needs_quotes(text) {
        return text.to_owned();
    }
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\u{8}' => quoted.push_str("\\b"),
            '\u{c}' => quoted.push_str("\\f"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '\0'..='\u{1f}' | '\u{7f}'..='\u{9f}' => {
                let _ = write!(quoted, "\\x{:02x}", character as u32);
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
