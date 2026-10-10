//! Bounded Markdown presentation for agent responses.
//!
//! limn's `Markdown` parses, renders and caches; this module gives it the
//! app's colors, strings and link action. HTTP(S) links retain their
//! destinations as scoped actions; editable drafts keep their exact source
//! and use paint-only highlighting.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use openide_ui::{
    Button, ButtonVariant, Component, ControlSize, Element, ElementKind, Markdown,
    MarkdownColors, MarkdownLabels, MarkdownPreview, RadiusRole, TextHighlight, Tooltip,
};

use crate::theme;

#[cfg(test)]
#[path = "rich_text_link_tests.rs"]
mod link_tests;

const LINK_ACTION_PREFIX: &str = "chat_link_open:";

const COLORS: MarkdownColors = MarkdownColors {
    text: theme::FOREGROUND,
    strong: theme::BRAND,
    emphasis: theme::EMPHASIS_INK,
    link: theme::LINK,
    code_ink: theme::USER,
    code_fill: theme::SELECTED,
    muted: theme::FOREGROUND_SUBTLE,
    success: theme::DIFF_ADDED,
    panel: theme::PANEL,
    border: theme::BORDER,
};

const LABELS: MarkdownLabels = MarkdownLabels {
    code_fallback: "CÓDIGO",
    truncated_note: "Respuesta extensa: vista limitada a los primeros bloques.",
};

fn markdown(source: &str) -> Markdown<'_> {
    Markdown::new(source, theme::panel_card_palette())
        .colors(COLORS)
        .labels(LABELS)
        .link_action_prefix(LINK_ACTION_PREFIX)
}

fn source_hash(source: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    source.hash(&mut hash);
    hash.finish()
}

fn link_action(source: &str, offset: usize, destination: &str) -> String {
    link_action_with_hash(source_hash(source), offset, destination)
}

fn link_action_with_hash(scope: u64, offset: usize, destination: &str) -> String {
    format!("{LINK_ACTION_PREFIX}{scope:x}:{offset}:{destination}")
}

/// Only web navigation is carried by this action. It never resolves local
/// files, executes a scheme or infers a destination from a visible label.
pub(crate) fn web_link_url_from_action(action: &str) -> Option<String> {
    let mut parts = action.strip_prefix(LINK_ACTION_PREFIX)?.splitn(3, ':');
    u64::from_str_radix(parts.next()?, 16).ok()?;
    parts.next()?.parse::<usize>().ok()?;
    Markdown::web_link(parts.next()?)
}

/// Cached presentation is independent of a conversation. Give each cloned
/// instance distinct focus/hover targets, including repeated identical blocks.
pub(crate) fn scope_web_links(mut element: Element, scope: &str) -> Element {
    fn visit(element: &mut Element, scope: u64, ordinal: &mut usize) {
        match &mut element.kind {
            ElementKind::Column(children) | ElementKind::Row(children) => {
                for child in children {
                    visit(child, scope, ordinal);
                }
            }
            ElementKind::Panel(child) => visit(child, scope, ordinal),
            ElementKind::FormattedText(spans) => {
                for span in spans {
                    if let Some(url) = span.action.as_deref().and_then(web_link_url_from_action) {
                        span.action = Some(link_action_with_hash(scope, *ordinal, &url));
                        *ordinal += 1;
                    }
                }
            }
            _ => {}
        }
    }
    visit(&mut element, source_hash(scope), &mut 0);
    element
}

/// Compact link affordances are separate from the editor's immutable source
/// geometry: the URL stays editable and paint-only color adds no glyph width.
pub(crate) fn composer_links(source: &str, width: f32) -> (Vec<TextHighlight>, Option<Element>) {
    // limn bounds the address as written; a chip must also open, so the
    // normalized (percent-encoded) form has to pass the same limit.
    let links = Markdown::web_links(source)
        .into_iter()
        .filter(|(_, url)| Markdown::web_link(url).is_some())
        .collect::<Vec<_>>();
    let highlights = links
        .iter()
        .map(|(range, _)| TextHighlight {
            range: range.clone(),
            color: theme::LINK,
        })
        .collect();
    if links.is_empty() || !width.is_finite() || width < 48.0 {
        return (highlights, None);
    }
    let mut destinations = Vec::new();
    for (_, url) in links {
        if !destinations.contains(&url) {
            destinations.push(url);
        }
    }
    let count = destinations.len().min(if width < 240.0 { 1 } else { 2 });
    let item_width = ((width - 4.0 * count.saturating_sub(1) as f32) / count as f32).min(280.0);
    let palette = theme::action_palette(theme::INPUT, theme::LINK);
    let items = destinations
        .iter()
        .take(count)
        .enumerate()
        .map(|(index, url)| {
            let label = url
                .as_str()
                .split_once("://")
                .map_or(url.as_str(), |(_, label)| label);
            let action = link_action("composer", index, url.as_str());
            let button = Button::new(action.clone(), label, palette)
                .icon(Markdown::link_icon(url))
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Dense)
                .radius(RadiusRole::Surface)
                .width(item_width)
                .render();
            Tooltip::new(
                button,
                format!("{action}:tooltip"),
                url.as_str(),
                theme::component_palette(),
            )
            .render()
        })
        .collect();
    (
        highlights,
        Some(Element::row(items).gap(4.0).size(Some(width), Some(24.0))),
    )
}

/// User messages preserve literal Markdown and URLs. Only complete HTTP(S)
/// destinations receive link styling; file paths and code remain unchanged.
pub(crate) fn render_user_message(source: &str, scope: &str) -> Element {
    // 21 against the 22 of a response is the user card's own leading.
    scope_web_links(markdown(source).plain_with_autolinks(21.0).render().element, scope)
}

/// Render with the actual reading-column width in logical pixels. HTTP(S)
/// links retain their URI; the host opens them only after an explicit action.
/// Completed messages reuse a bounded presentation tree until their displayed
/// content, width or presentation changes. The host still performs layout.
pub fn render_markdown_at_width(source: &str, width: f32) -> Element {
    render_markdown_preview_at_width(source, width).element
}

/// Keep at most two live trees in a shared bounded pool, so reasoning and
/// assistant text can coexist without churning the completed-message cache.
/// An unchanged live prefix can also reuse its previous tree.
pub fn render_streaming_markdown_at_width(source: &str, width: f32) -> Element {
    render_streaming_markdown_preview_at_width(source, width).element
}

pub fn render_markdown_preview_at_width(source: &str, width: f32) -> MarkdownPreview {
    markdown(source).width(width).render()
}

pub fn render_streaming_markdown_preview_at_width(source: &str, width: f32) -> MarkdownPreview {
    markdown(source).width(width).streaming(true).render()
}

#[cfg(test)]
mod tests {
    use openide_ui::{DrawCommand, Size, UiTree};

    use super::*;

    #[test]
    fn bounded_unicode_response_lays_out_without_markup_delimiters() {
        let markdown = "# Resumen\n\nSe corrigió **la sesión** en `src/main.rs`.\n- [x] Compila\n- [ ] Revisar [guía](https://example.com/docs)";
        let root = render_markdown_at_width(markdown, 360.0);
        let frame = UiTree::new(root).layout(Size::new(360.0, 400.0));
        let text: String = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.contains("la sesión"));
        assert!(text.contains("src/main.rs"));
        assert!(text.contains("guía"));
        assert!(!text.contains("https://example.com/docs"));
        assert!(frame
            .regions
            .iter()
            .any(|region| web_link_url_from_action(&region.id).as_deref()
                == Some("https://example.com/docs")));
        assert!(!text.contains("**"));
        assert!(!text.contains("[guía]"));
    }
}
