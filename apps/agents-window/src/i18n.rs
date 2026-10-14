//! The application writes itself in Spanish. In English, what it is about
//! to draw is put into English on its way to the window: the tree of one
//! frame is walked and each text of the interface replaced by its entry in
//! the catalog. Nothing else knows there are two languages, so a text that
//! has no entry yet is drawn as it was written.
//!
//! What is not the interface's is left alone: what an editor holds, a diff,
//! the value of an input. A text that is someone's content and happens to
//! read exactly like a label is translated too; the catalog cannot tell.

use openide_ui::{Element, ElementKind, TooltipSection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Spanish, a tab, English; `\n`, `\t` and `\\` written out.
const CATALOG: &str = include_str!("../assets/i18n/en.tsv");
const FILE: &str = "language";
/// Texts longer than this are content, not labels.
const MAX_TEXT_BYTES: usize = 600;
/// Texts remembered with what they turned into, so one that changes every
/// frame cannot grow the memory without end.
const MAX_REMEMBERED: usize = 20_000;

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub(crate) fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// Sets the language of everything drawn from now on, limn's own words
/// included.
pub(crate) fn set_english(english: bool) {
    ENGLISH.store(english, Ordering::Relaxed);
    openide_ui::set_labels(if english { openide_ui::Labels::ENGLISH } else { openide_ui::Labels::SPANISH });
}

/// The language chosen on this machine; Spanish when none was.
pub(crate) fn load(root: &Path) {
    set_english(std::fs::read_to_string(root.join(FILE)).is_ok_and(|saved| saved.trim() == "en"));
}

pub(crate) fn save(root: &Path, english: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    std::fs::write(root.join(FILE), if english { "en" } else { "es" })
}

/// A text with values in it: the literal stretches between them, and the
/// English it becomes with the same values.
struct Template {
    parts: Vec<&'static str>,
    /// The name of each value, empty for one that goes by its place.
    names: Vec<&'static str>,
    english: Vec<Piece>,
}

enum Piece {
    Text(&'static str),
    /// The value that was captured at this place of the Spanish text.
    Value(usize),
}

struct Catalog {
    exact: HashMap<&'static str, &'static str>,
    /// The most literal first, so the closest reading of a text wins.
    templates: Vec<Template>,
}

fn unescape(text: &str) -> &'static str {
    if !text.contains('\\') {
        return Box::leak(text.to_owned().into_boxed_str());
    }
    let mut out = String::with_capacity(text.len());
    let mut letters = text.chars();
    while let Some(letter) = letters.next() {
        match (letter, letter == '\\').1.then(|| letters.next()).flatten() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push(letter),
        }
    }
    Box::leak(out.into_boxed_str())
}

/// The literal stretches of a format text and the name of each value
/// between them. `{{` and `}}` are braces, as in `format!`.
fn pieces(text: &'static str) -> (Vec<&'static str>, Vec<&'static str>) {
    let (mut parts, mut names, mut start, mut at) = (Vec::new(), Vec::new(), 0, 0);
    let bytes = text.as_bytes();
    while at < bytes.len() {
        if bytes[at] == b'{' && bytes.get(at + 1) != Some(&b'{') {
            if let Some(end) = text[at..].find('}') {
                parts.push(&text[start..at]);
                let inside = &text[at + 1..at + end];
                names.push(inside.split(':').next().unwrap_or("").trim());
                at += end + 1;
                start = at;
                continue;
            }
        }
        at += if matches!(&bytes[at..], [b'{', b'{', ..] | [b'}', b'}', ..]) { 2 } else { 1 };
    }
    parts.push(&text[start..]);
    (parts, names)
}

fn catalog() -> &'static Catalog {
    static LOADED: OnceLock<Catalog> = OnceLock::new();
    LOADED.get_or_init(|| {
        let mut exact = HashMap::new();
        let mut templates = Vec::new();
        for line in CATALOG.lines() {
            let Some((spanish, english)) = line.split_once('\t') else { continue };
            let (spanish, english) = (unescape(spanish), unescape(english));
            if spanish == english {
                continue;
            }
            let (parts, names) = pieces(spanish);
            if names.is_empty() {
                exact.insert(spanish, english);
                continue;
            }
            // A text that is nearly all values would read anything as itself,
            // and two values side by side cannot be told apart.
            let letters = parts.iter().flat_map(|part| part.chars()).filter(|letter| letter.is_alphabetic()).count();
            if letters < 4 || parts[1..parts.len() - 1].iter().any(|part| part.is_empty()) {
                continue;
            }
            let (english_parts, english_names) = pieces(english);
            let mut unnamed = names.iter().enumerate().filter(|(_, name)| name.is_empty()).map(|(index, _)| index);
            let mut said = Vec::new();
            for (index, part) in english_parts.iter().enumerate() {
                said.push(Piece::Text(part));
                let Some(name) = english_names.get(index) else { continue };
                let value = if name.is_empty() { unnamed.next() } else { names.iter().position(|known| known == name) };
                match value {
                    Some(value) => said.push(Piece::Value(value)),
                    None => {
                        said.clear();
                        break;
                    }
                }
            }
            if !said.is_empty() {
                templates.push(Template { parts, names, english: said });
            }
        }
        templates.sort_by_key(|template| std::cmp::Reverse(template.parts.iter().map(|part| part.len()).sum::<usize>()));
        Catalog { exact, templates }
    })
}

/// The values of `text` read as `template`, when it reads as it at all.
fn captures<'a>(template: &Template, text: &'a str) -> Option<Vec<&'a str>> {
    let (first, last) = (template.parts[0], template.parts[template.parts.len() - 1]);
    if text.len() < first.len() + last.len() || !text.starts_with(first) || !text.ends_with(last) {
        return None;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    let mut found = Vec::with_capacity(template.names.len());
    for part in &template.parts[1..template.parts.len() - 1] {
        let at = rest.find(part)?;
        found.push(&rest[..at]);
        rest = &rest[at + part.len()..];
    }
    found.push(rest);
    // A value is never nothing, and a line break ends a label.
    found.iter().all(|value| !value.is_empty()).then_some(found)
}

fn lookup(text: &str) -> Option<String> {
    let catalog = catalog();
    if let Some(english) = catalog.exact.get(text) {
        return Some((*english).to_owned());
    }
    // The same words inside spaces a layout added around them.
    let inner = text.trim();
    if inner.len() != text.len() {
        let english = lookup(inner)?;
        let start = text.len() - text.trim_start().len();
        return Some(format!("{}{english}{}", &text[..start], &text[start + inner.len()..]));
    }
    let english = catalog.templates.iter().find_map(|template| {
        let values = captures(template, text)?;
        Some(
            template
                .english
                .iter()
                .map(|piece| match piece {
                    Piece::Text(text) => (*text).to_owned(),
                    // A value may be a text of the interface itself: a day, a state, a count.
                    Piece::Value(index) => lookup(values[*index]).unwrap_or_else(|| values[*index].to_owned()),
                })
                .collect(),
        )
    });
    english.or_else(|| said_piece_by_piece(text))
}

/// A text the application joined from labels, such as "Permiso pendiente ·
/// Navegación web" or "leyó 2 archivos, buscó 1 vez", has no entry of its
/// own: each piece is said, and the text is changed when one of them was.
fn said_piece_by_piece(text: &str) -> Option<String> {
    [" · ", ", "].into_iter().filter(|separator| text.contains(separator)).find_map(|separator| {
        let mut changed = false;
        let english = text
            .split(separator)
            .map(|piece| match lookup(piece) {
                Some(english) => {
                    changed = true;
                    english
                }
                None => piece.to_owned(),
            })
            .collect::<Vec<_>>()
            .join(separator);
        changed.then_some(english)
    })
}

/// `text` in English, when the interface is in English and the catalog
/// knows it: for what is shown outside the window's tree.
pub(crate) fn text(text: &str) -> Option<String> {
    english().then(|| said(text)).flatten()
}

/// A piece of a text the application joins itself, such as one phrase of a
/// list, in English when the interface is: the joined text would read as no
/// entry of the catalog.
pub(crate) fn piece(text: String) -> String {
    self::text(&text).unwrap_or(text)
}

/// `text` in English, when the catalog knows it.
fn said(text: &str) -> Option<String> {
    if text.len() > MAX_TEXT_BYTES || !text.chars().any(char::is_alphabetic) {
        return None;
    }
    static REMEMBERED: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let mut remembered = REMEMBERED.get_or_init(Default::default).lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(known) = remembered.get(text) {
        return known.clone();
    }
    if remembered.len() >= MAX_REMEMBERED {
        remembered.clear();
    }
    let english = lookup(text);
    remembered.insert(text.to_owned(), english.clone());
    english
}

fn say(words: &mut String) {
    if let Some(english) = said(words) {
        *words = english;
    }
}

fn walk(element: &mut Element) {
    match &mut element.kind {
        ElementKind::Column(children) | ElementKind::Row(children) | ElementKind::Stack(children) => {
            children.iter_mut().for_each(walk)
        }
        ElementKind::TabStrip { children, .. } => children.iter_mut().for_each(walk),
        ElementKind::Panel(child)
        | ElementKind::Clickable { child, .. }
        | ElementKind::Scroll { child, .. }
        | ElementKind::HorizontalScroll { child, .. }
        | ElementKind::ShortcutHint { child, .. } => walk(child),
        ElementKind::Overlay { base, floating, .. } => {
            walk(base);
            walk(floating);
        }
        ElementKind::Popover { anchor, floating, .. } => {
            walk(anchor);
            walk(floating);
        }
        ElementKind::Tooltip { anchor, title, sections, .. } => {
            walk(anchor);
            say(title);
            for section in sections {
                match section {
                    TooltipSection::Description(words) => say(words),
                    TooltipSection::IconRow { label, .. } => say(label),
                    _ => {}
                }
            }
        }
        // Rows are built as they come into view: each is put into English then.
        ElementKind::UniformList(list) => {
            let build = Arc::clone(&list.build_row);
            list.build_row = Arc::new(move |index| {
                let mut row = build(index);
                walk(&mut row);
                row
            });
            if let Some((_, open)) = &mut list.expansion {
                walk(Arc::make_mut(open));
            }
        }
        ElementKind::Text(words) | ElementKind::TruncatedText(words) => say(words),
        ElementKind::FadedText { text, .. } | ElementKind::ShimmerText { text, .. } => say(text),
        ElementKind::FormattedText(spans) => spans.iter_mut().for_each(|span| say(&mut span.text)),
        ElementKind::Button { label, .. } | ElementKind::IconButton { label, .. } | ElementKind::Surface { label, .. } => {
            say(label)
        }
        ElementKind::TextInput { placeholder, .. } | ElementKind::TextArea { placeholder, .. } => say(placeholder),
        _ => {}
    }
}

/// The frame's tree as the window is to show it: in English when that is
/// the language, and untouched otherwise.
pub(crate) fn localized(mut root: Element) -> Element {
    if english() {
        walk(&mut root);
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_format_text_is_read_back_into_its_values_and_said_in_english() {
        let (parts, names) = pieces("Cada {day} a las {hour:02}:{minute:02}");
        assert_eq!((parts.as_slice(), names.as_slice()), (&["Cada ", " a las ", ":", ""][..], &["day", "hour", "minute"][..]));
        let template = Template { parts, names, english: Vec::new() };
        assert_eq!(captures(&template, "Cada lunes a las 09:30").unwrap(), ["lunes", "09", "30"]);
        assert!(captures(&template, "Diariamente a las 09:30").is_none());
        // Braces written twice are braces, not a value.
        assert_eq!(pieces("un {{literal}} y {}").1, [""]);
    }

    #[test]
    fn every_entry_of_the_catalog_keeps_the_values_of_its_text() {
        let mut entries = 0;
        for line in CATALOG.lines() {
            let (spanish, english) = line.split_once('\t').unwrap_or_else(|| panic!("no tab in {line:?}"));
            let (mut wanted, mut said) = (pieces(unescape(spanish)).1, pieces(unescape(english)).1);
            wanted.sort_unstable();
            said.sort_unstable();
            assert_eq!(wanted, said, "{spanish:?}");
            entries += 1;
        }
        assert!(entries > 1000, "{entries}");
    }

    #[test]
    fn a_text_joined_from_labels_is_said_label_by_label() {
        assert_eq!(lookup("Permiso pendiente · Navegación web").as_deref(), Some("Pending permission · Web navigation"));
        // A caption whose list was said phrase by phrase where it was joined.
        assert_eq!(
            lookup("hace 3 min · 21 s · gpt-6.1-sol · read 8 files, searched 2 times").as_deref(),
            Some("3 min ago · 21 s · gpt-6.1-sol · read 8 files, searched 2 times")
        );
        assert_eq!(lookup("9 archivos, 6 búsquedas, 5 listados").as_deref(), Some("9 files, 6 searches, 5 listings"));
        // Nothing to say in any piece: the text is left to be drawn as it was.
        assert_eq!(lookup("main.rs, lib.rs · 3 KB"), None);
    }

    #[test]
    fn the_tree_is_said_in_english_only_when_that_is_the_language() {
        let tree = || {
            Element::column(vec![
                Element::text("Guardar cambios"),
                Element::uniform_list("rows", 1, 20.0, |_| Element::text("Cancelar")),
                Element::text("un texto que nadie tradujo"),
            ])
        };
        let words = |element: &Element| -> Vec<String> {
            let ElementKind::Column(children) = &element.kind else { panic!() };
            children
                .iter()
                .map(|child| match &child.kind {
                    ElementKind::Text(words) => words.clone(),
                    ElementKind::UniformList(list) => match (list.build_row)(0).kind {
                        ElementKind::Text(words) => words,
                        _ => panic!(),
                    },
                    _ => panic!(),
                })
                .collect()
        };
        // Spanish, as every other test of the window finds it.
        assert_eq!(words(&localized(tree())), ["Guardar cambios", "Cancelar", "un texto que nadie tradujo"]);
        // The walk itself asks nothing of the setting, which other tests share.
        let mut english = tree();
        walk(&mut english);
        assert_eq!(text("Guardar cambios"), None);
        assert_eq!(words(&english), ["Save changes", "Cancel", "un texto que nadie tradujo"]);
    }
}
