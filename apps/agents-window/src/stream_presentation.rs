//! Display-only reveal of the live transcript frontier. The transcript stays
//! authoritative and intact: this state holds identities and UTF-8 cursors,
//! never a second text buffer or a partially reconstructed model message.

use crate::transcript::{ModelStreamStatus, TranscriptItem};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};
use unicode_segmentation::GraphemeCursor;

const REVEAL_TICK: Duration = Duration::from_nanos(8_333_333);
const DRAIN_HORIZON: Duration = Duration::from_millis(200);
const FINGERPRINT_EDGE_BYTES: usize = 96;

#[derive(Debug, Default)]
pub(crate) struct StreamingPresentation {
    session: Option<String>,
    initialized: bool,
    active: Option<RevealCursor>,
}

#[derive(Debug, PartialEq, Eq)]
enum RowIdentity {
    Assistant,
    Reasoning { id: String, step: usize },
}

#[derive(Debug)]
struct RevealCursor {
    index: usize,
    identity: RowIdentity,
    visible: usize,
    source_bytes: usize,
    source_fingerprint: u64,
    last_advance: Instant,
    drain_by: Option<Instant>,
}

impl StreamingPresentation {
    /// Synchronize before layout, using the complete source transcript.
    /// Only the live frontier can be animated. Passive trailing plan events
    /// preserve its cursor; a tool, terminal row or different live row makes
    /// the preceding row fully visible.
    /// Switching sessions establishes a complete baseline for existing text,
    /// so reopening history does not replay its appearance.
    pub(crate) fn sync(
        &mut self,
        session: Option<&str>,
        items: &[TranscriptItem],
        now: Instant,
    ) -> bool {
        let session_changed = !self.initialized || self.session.as_deref() != session;
        self.initialized = true;
        if session_changed {
            self.session = session.map(str::to_owned);
        }

        let frontier = session.and_then(|_| {
            let (index, item) = items
                .iter()
                .enumerate()
                .rev()
                .find(|(_, item)| !matches!(item, TranscriptItem::PlanSnapshot { .. }))?;
            let (identity, content) = match item {
                TranscriptItem::Assistant {
                    content,
                    streaming: true,
                } => (RowIdentity::Assistant, content.as_str()),
                TranscriptItem::Reasoning {
                    id,
                    step,
                    content,
                    status: ModelStreamStatus::Receiving,
                    ..
                } => (
                    RowIdentity::Reasoning {
                        id: id.clone(),
                        step: *step,
                    },
                    content.as_str(),
                ),
                _ => return None,
            };
            Some((index, identity, content))
        });

        let Some((index, identity, content)) = frontier else {
            let changed = self.has_pending();
            self.active = None;
            return changed;
        };

        if session_changed {
            let changed = self.has_pending();
            self.active = Some(RevealCursor::baseline(index, identity, content, now));
            return changed;
        }

        let Some(active) = self.active.as_mut() else {
            let mut cursor = RevealCursor::baseline(index, identity, content, now);
            cursor.visible = 0;
            cursor.begin_burst(content, now);
            let changed = !content.is_empty();
            self.active = Some(cursor);
            return changed;
        };

        if active.index != index {
            let mut cursor = RevealCursor::baseline(index, identity, content, now);
            cursor.visible = 0;
            cursor.begin_burst(content, now);
            self.active = Some(cursor);
            return true;
        }

        // TranscriptProjection appends live text. This bounded fingerprint is
        // a defensive presentation check for a replacement at the same index,
        // not validation of content or evidence for any model/tool operation.
        if active.identity != identity
            || content.len() < active.source_bytes
            || prefix_fingerprint(content, active.source_bytes) != active.source_fingerprint
        {
            let changed = active.visible < content.len();
            self.active = Some(RevealCursor::baseline(index, identity, content, now));
            return changed;
        }

        let previous_visible = active.visible;
        if content.len() > active.source_bytes {
            // Do not keep an older burst hidden after its original deadline
            // while extending the reveal window for freshly received text.
            if active.drain_by.is_some_and(|deadline| now >= deadline) {
                active.visible = active.source_bytes;
            }
            let was_caught_up = active.visible == active.source_bytes;
            active.source_bytes = content.len();
            // An append may extend the preceding cluster (combining marks,
            // skin-tone modifiers, flags or a ZWJ emoji). Keep that already
            // visible cluster whole before budgeting newly arriving text.
            active.visible = next_grapheme_boundary(content, active.visible);
            active.source_fingerprint = prefix_fingerprint(content, content.len());
            if was_caught_up {
                active.begin_burst(content, now);
            }
        }
        active.advance(content, now);
        active.visible != previous_visible
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.visible < active.source_bytes)
    }

    /// Return a safe byte prefix for the caller's source string. Every row
    /// outside the active frontier is rendered in full, including all history.
    pub(crate) fn visible_bytes(&self, index: usize, full: &str) -> usize {
        let mut boundary = self
            .active
            .as_ref()
            .filter(|active| active.index == index)
            .map_or(full.len(), |active| active.visible.min(full.len()));
        while !full.is_char_boundary(boundary) {
            boundary -= 1;
        }
        boundary
    }

    /// Snap the current baseline to full visibility. Call after sync when
    /// reduced motion is enabled; future source appends remain distinguishable
    /// if the preference is later disabled.
    pub(crate) fn flush(&mut self) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        let changed = active.visible < active.source_bytes;
        active.visible = active.source_bytes;
        active.drain_by = None;
        changed
    }
}

impl RevealCursor {
    fn baseline(index: usize, identity: RowIdentity, content: &str, now: Instant) -> Self {
        Self {
            index,
            identity,
            visible: content.len(),
            source_bytes: content.len(),
            source_fingerprint: prefix_fingerprint(content, content.len()),
            last_advance: now,
            drain_by: None,
        }
    }

    fn begin_burst(&mut self, content: &str, now: Instant) {
        if self.visible == self.source_bytes {
            self.drain_by = None;
            return;
        }
        self.drain_by = Some(drain_deadline(now));
        self.last_advance = now;
        // Reveal a first slice immediately, so the first received text never
        // waits behind an empty animated row or an additional frame timer.
        self.reveal(
            content,
            proportional_bytes(self.source_bytes - self.visible, REVEAL_TICK, DRAIN_HORIZON),
        );
    }

    fn advance(&mut self, content: &str, now: Instant) {
        let Some(deadline) = self.drain_by else {
            return;
        };
        if now >= deadline {
            self.visible = self.source_bytes;
            self.drain_by = None;
            self.last_advance = now;
            return;
        }
        let elapsed = now.saturating_duration_since(self.last_advance);
        if elapsed < REVEAL_TICK {
            return;
        }
        let horizon = deadline.saturating_duration_since(self.last_advance);
        let amount = proportional_bytes(self.source_bytes - self.visible, elapsed, horizon);
        self.reveal(content, amount);
        self.last_advance = now;
    }

    fn reveal(&mut self, content: &str, amount: usize) {
        let mut boundary = self
            .visible
            .saturating_add(amount.max(1))
            .min(self.source_bytes);
        // Reveal complete extended grapheme clusters. UTF-8 codepoint
        // boundaries alone can expose half an emoji or detach its accent.
        while !content.is_char_boundary(boundary) {
            boundary += 1;
        }
        self.visible = next_grapheme_boundary(content, boundary);
        if self.visible == self.source_bytes {
            self.drain_by = None;
        }
    }
}

fn next_grapheme_boundary(content: &str, boundary: usize) -> usize {
    let mut cursor = GraphemeCursor::new(boundary, content.len(), true);
    if cursor.is_boundary(content, 0).unwrap_or(true) {
        boundary
    } else {
        cursor
            .next_boundary(content, 0)
            .ok()
            .flatten()
            .unwrap_or(content.len())
    }
}

fn proportional_bytes(backlog: usize, elapsed: Duration, horizon: Duration) -> usize {
    let amount = (backlog as u128)
        .saturating_mul(elapsed.as_nanos())
        .div_ceil(horizon.as_nanos().max(1));
    usize::try_from(amount).unwrap_or(backlog).min(backlog)
}

fn drain_deadline(now: Instant) -> Instant {
    now.checked_add(DRAIN_HORIZON).unwrap_or(now)
}

fn prefix_fingerprint(content: &str, bytes: usize) -> u64 {
    let prefix = &content.as_bytes()[..bytes.min(content.len())];
    let mut hasher = DefaultHasher::new();
    prefix.len().hash(&mut hasher);
    prefix[..prefix.len().min(FINGERPRINT_EDGE_BYTES)].hash(&mut hasher);
    if prefix.len() > FINGERPRINT_EDGE_BYTES {
        prefix[prefix.len() - FINGERPRINT_EDGE_BYTES..].hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_segmentation::UnicodeSegmentation;

    fn assistant(text: &str) -> TranscriptItem {
        TranscriptItem::Assistant {
            content: text.into(),
            streaming: true,
        }
    }

    #[test]
    fn passive_plan_snapshots_preserve_live_reveal_until_the_next_segment_or_terminal() {
        let now = Instant::now();
        let text = "Texto incremental con 🦀. ".repeat(80);
        let mut presentation = StreamingPresentation::default();
        presentation.sync(Some("session"), &[], now);
        let mut items = vec![assistant(&text)];
        presentation.sync(Some("session"), &items, now);
        presentation.sync(Some("session"), &items, now + REVEAL_TICK * 2);
        let visible = presentation.visible_bytes(0, &text);
        assert!(visible > 0 && visible < text.len());
        for sequence in [1, 2] {
            items.push(TranscriptItem::PlanSnapshot {
                id: format!("plan:session:{sequence}"),
                sequence: Some(sequence),
                plan: openide_core::PlanSnapshot::from_plan_response("1. Revisar"),
                is_proposal: false,
                tool_call_id: Some(format!("update-{sequence}")),
            });
        }
        assert!(!presentation.sync(Some("session"), &items, now + REVEAL_TICK * 2));
        assert_eq!(presentation.visible_bytes(0, &text), visible);
        assert!(presentation.has_pending());
        presentation.sync(Some("session"), &items, now + REVEAL_TICK * 3);
        assert!(presentation.visible_bytes(0, &text) > visible);
        items.push(assistant(&text));
        presentation.sync(Some("session"), &items, now + REVEAL_TICK * 4);
        assert_eq!(presentation.visible_bytes(0, &text), text.len());
        assert!(presentation.visible_bytes(3, &text) < text.len());
        let TranscriptItem::Assistant { streaming, .. } = &mut items[3] else {
            unreachable!()
        };
        *streaming = false;
        presentation.sync(Some("session"), &items, now + REVEAL_TICK * 5);
        assert_eq!(presentation.visible_bytes(3, &text), text.len());
        assert!(!presentation.has_pending());
    }

    #[test]
    fn burst_frames_reveal_whole_emoji_flags_accents_and_indic_clusters() {
        let now = Instant::now();
        for cluster in ["👩🏽‍💻", "🇦🇷", "e\u{301}", "क्‍ष", "🏳️‍🌈"]
        {
            let text = cluster.repeat(80);
            let boundaries: Vec<_> = text
                .grapheme_indices(true)
                .map(|(offset, _)| offset)
                .chain(std::iter::once(text.len()))
                .collect();
            let mut presentation = StreamingPresentation::default();
            presentation.sync(Some("session"), &[], now);
            let items = [assistant(&text)];
            let ticks = DRAIN_HORIZON.as_nanos().div_ceil(REVEAL_TICK.as_nanos()) as u32;
            for tick in 0..=ticks {
                presentation.sync(Some("session"), &items, now + REVEAL_TICK * tick);
                let visible = presentation.visible_bytes(0, &text);
                assert!(
                    boundaries.contains(&visible),
                    "{cluster}: partial cluster at {visible}"
                );
            }
            assert!(!presentation.has_pending());
            assert_eq!(presentation.visible_bytes(0, &text), text.len());
        }
    }

    #[test]
    fn append_can_extend_a_visible_cluster_without_splitting_it() {
        let now = Instant::now();
        for (first, extended) in [("e", "e\u{301}"), ("👩", "👩🏽‍💻"), ("🇦", "🇦🇷")]
        {
            let mut presentation = StreamingPresentation::default();
            presentation.sync(Some("session"), &[assistant(first)], now);
            let text = format!("{extended}{}", " continuation".repeat(30));
            presentation.sync(Some("session"), &[assistant(&text)], now + REVEAL_TICK);
            let visible = presentation.visible_bytes(0, &text);
            assert!(visible >= extended.len());
            assert!(text
                .grapheme_indices(true)
                .any(|(offset, _)| offset == visible));
        }
    }

    #[test]
    fn terminal_and_reopened_rows_do_not_replay_an_incomplete_grapheme() {
        let now = Instant::now();
        let text = "👩🏽‍💻".repeat(80);
        let mut presentation = StreamingPresentation::default();
        presentation.sync(Some("session"), &[], now);
        presentation.sync(Some("session"), &[assistant(&text)], now);
        assert!(presentation.has_pending());
        let terminal = TranscriptItem::Assistant {
            content: text.clone(),
            streaming: false,
        };
        presentation.sync(Some("session"), &[terminal], now);
        assert!(!presentation.has_pending());
        assert_eq!(presentation.visible_bytes(0, &text), text.len());
        presentation.sync(Some("reopened"), &[assistant(&text)], now);
        assert!(!presentation.has_pending());
        assert_eq!(presentation.visible_bytes(0, &text), text.len());
    }

    #[test]
    fn one_frame_uses_one_reveal_time_and_terminal_still_flushes_immediately() {
        let now = Instant::now();
        let text = "Respuesta completa con ñ y 👩🏽‍💻. ".repeat(60);
        let mut presentation = StreamingPresentation::default();
        presentation.sync(Some("session"), &[], now);
        let mut items = [assistant(&text)];
        assert!(presentation.sync(Some("session"), &items, now));
        let first = presentation.visible_bytes(0, &text);
        assert!(first > 0 && first < text.len());
        for _ in 0..20 {
            assert!(!presentation.sync(Some("session"), &items, now));
            assert_eq!(presentation.visible_bytes(0, &text), first);
        }
        assert!(presentation.sync(Some("session"), &items, now + REVEAL_TICK));
        assert!(presentation.visible_bytes(0, &text) > first);
        let TranscriptItem::Assistant { streaming, .. } = &mut items[0] else {
            unreachable!()
        };
        *streaming = false;
        assert!(presentation.sync(Some("session"), &items, now + REVEAL_TICK));
        assert_eq!(presentation.visible_bytes(0, &text), text.len());
        assert!(!presentation.has_pending());
        for tick in 2..20 {
            assert!(!presentation.sync(Some("session"), &items, now + REVEAL_TICK * tick));
        }
        assert!(matches!(&items[0], TranscriptItem::Assistant { content, .. } if content == &text));
    }
}
