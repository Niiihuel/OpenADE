//! Loading skeletons shared by every tool card. While a call's arguments are
//! still streaming the card is drawn in its final shape, and these bars hold
//! the place of the text that has not arrived, so the first real frame
//! replaces them without the card changing shape.
//!
//! Ids are `{scope}:{index}` with a scope built from the session and the call,
//! so the pulse keeps its phase through token-driven rebuilds. The shared
//! painter suspends the animation offscreen and under reduced motion.

use crate::{
    theme,
    transcript::{ModelStreamStatus, TranscriptItem},
};
use openide_ui::{Component, ComponentPalette, Element, Skeleton};

/// Bar thickness for one line of 13 px copy.
pub(crate) const BAR: f32 = 8.0;

/// Whether the model is still writing this call, or has just confirmed it and
/// the host has not started it. Both draw the card the call will become.
pub(crate) fn pending_call(item: &TranscriptItem) -> bool {
    matches!(
        item,
        TranscriptItem::ToolPreview {
            status: ModelStreamStatus::Receiving | ModelStreamStatus::Committed,
            ..
        }
    )
}

/// `{session}:{call}`: the same for a preview and for the tool it becomes
/// once the call id is known.
pub(crate) fn scope(owner: &str, id: &str, call_id: Option<&str>) -> String {
    format!("{owner}:{}", call_id.unwrap_or(id))
}

/// One bar, `fraction` of the row wide, centred in a row `row_height` tall.
pub(crate) fn line(id: String, fraction: f32, bar: f32, row_height: f32) -> Element {
    let fraction = fraction.clamp(0.05, 1.0);
    Element::row(vec![
        Skeleton::new(id, theme::component_palette())
            .height(bar)
            .radius(bar / 2.0)
            .render()
            .centered_y()
            .flex(fraction),
        Element::spacer().flex(1.0 - fraction),
    ])
    .size(None, Some(row_height))
}

/// Body lines: one bar per fraction, each in a row as tall as the line of
/// content it stands for.
pub(crate) fn lines(scope: &str, fractions: &[f32], bar: f32, row_height: f32, gap: f32) -> Element {
    Element::column(
        fractions
            .iter()
            .enumerate()
            .map(|(index, fraction)| line(format!("{scope}:{index}"), *fraction, bar, row_height))
            .collect(),
    )
    .gap(gap)
}

/// Bars drawn on a raised card: a card is filled with the colour a bar on the
/// canvas rests at, so these rest one step lighter and pulse towards the text.
pub(crate) fn on_card() -> ComponentPalette {
    let card = theme::card_palette();
    let toward_text = |parts: u16| {
        let mix = |channel: u8, text: u8| {
            ((u16::from(channel) * (8 - parts) + u16::from(text) * parts) / 8) as u8
        };
        openide_ui::Color::rgba(
            mix(card.raised.r, card.text.r),
            mix(card.raised.g, card.text.g),
            mix(card.raised.b, card.text.b),
            card.raised.a,
        )
    };
    ComponentPalette {
        raised: toward_text(1),
        hover: toward_text(2),
        ..card
    }
}

/// One bar that takes its row's height from the real text beside it.
pub(crate) fn bar(id: String, fraction: f32, palette: ComponentPalette) -> Element {
    let fraction = fraction.clamp(0.05, 1.0);
    Element::row(vec![
        Skeleton::new(id, palette)
            .height(BAR)
            .radius(BAR / 2.0)
            .render()
            .centered_y()
            .flex(fraction),
        Element::spacer().flex(1.0 - fraction),
    ])
}
