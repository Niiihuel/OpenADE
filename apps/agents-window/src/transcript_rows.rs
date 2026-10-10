//! Mutation tracking for the projection's single retained UI snapshot.
//!
//! All mutable access is explicit (there is deliberately no DerefMut). This
//! makes new projection branches participate automatically without maintaining
//! a second event-to-row dependency map in the desktop host.

use super::TranscriptItem;
use std::collections::BTreeSet;
use std::ops::{Deref, Index, IndexMut};

#[derive(Debug, Default)]
pub(super) struct TranscriptRows {
    values: Vec<TranscriptItem>,
    dirty: BTreeSet<usize>,
    shifted_from: Option<usize>,
    published_len: Option<usize>,
}

impl TranscriptRows {
    pub(super) fn push(&mut self, item: TranscriptItem) {
        self.dirty.insert(self.values.len());
        self.values.push(item);
    }

    pub(super) fn remove(&mut self, index: usize) -> TranscriptItem {
        self.shifted_from = Some(self.shifted_from.map_or(index, |start| start.min(index)));
        self.values.remove(index)
    }

    pub(super) fn insert(&mut self, index: usize, item: TranscriptItem) {
        self.shifted_from = Some(self.shifted_from.map_or(index, |start| start.min(index)));
        self.values.insert(index, item);
    }

    pub(super) fn get_mut(&mut self, index: usize) -> Option<&mut TranscriptItem> {
        if index < self.values.len() {
            self.dirty.insert(index);
        }
        self.values.get_mut(index)
    }

    pub(super) fn last_mut(&mut self) -> Option<&mut TranscriptItem> {
        let index = self.values.len().checked_sub(1)?;
        self.get_mut(index)
    }

    pub(super) fn sync_into(&mut self, target: &mut Vec<TranscriptItem>) {
        if self.published_len != Some(target.len()) {
            target.clone_from(&self.values);
        } else {
            // Reconciliation can remove earlier text segments and shift every
            // later row. Publish that suffix, plus independently changed rows
            // before it. A normal delta copies only its model marker/text row.
            let suffix = self
                .shifted_from
                .unwrap_or(self.values.len())
                .min(self.values.len());
            let shared = target.len().min(self.values.len());
            for index in self
                .dirty
                .range(..suffix)
                .copied()
                .filter(|index| *index < shared)
            {
                target[index].clone_from(&self.values[index]);
            }
            if suffix < shared {
                target[suffix..shared].clone_from_slice(&self.values[suffix..shared]);
            }
            target.truncate(self.values.len());
            target.extend_from_slice(&self.values[target.len()..]);
        }
        self.published_len = Some(self.values.len());
        self.dirty.clear();
        self.shifted_from = None;
    }
}

impl Deref for TranscriptRows {
    type Target = [TranscriptItem];

    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl Index<usize> for TranscriptRows {
    type Output = TranscriptItem;

    fn index(&self, index: usize) -> &Self::Output {
        &self.values[index]
    }
}

impl IndexMut<usize> for TranscriptRows {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.dirty.insert(index);
        &mut self.values[index]
    }
}

impl IntoIterator for TranscriptRows {
    type Item = TranscriptItem;
    type IntoIter = std::vec::IntoIter<TranscriptItem>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.into_iter()
    }
}
