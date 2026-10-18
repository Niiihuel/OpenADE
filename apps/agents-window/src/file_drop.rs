//! Files dropped on the window from another application.
//!
//! winit reports drops on X11, Windows and macOS, and limn turns them into
//! `UiAction::FilesDropped`. winit does not speak Wayland's drag and drop, so
//! there limn-dnd's receiver reads the offer on its own event queue. Either
//! way the host only receives paths: the composer's attachment worker reads
//! and copies the files.

use std::path::PathBuf;

#[cfg(target_os = "linux")]
use limn_dnd::{DropDecision, FileDropReceiver, OfferId, ReceiveEpoch, ReceiveEvent, ReceiveLimits, ReceiveStatus};

#[derive(Default)]
pub(crate) struct FileDrops {
    #[cfg(target_os = "linux")]
    receiver: Option<FileDropReceiver>,
    #[cfg(target_os = "linux")]
    epoch: Option<ReceiveEpoch>,
    #[cfg(target_os = "linux")]
    pending: Option<OfferId>,
    /// An offer is over the window.
    pub hovering: bool,
}

impl FileDrops {
    /// Receive Wayland drops on the app's root window; elsewhere, or without
    /// one, winit's own events are the only source.
    pub(crate) fn attach(creation: &eframe::CreationContext<'_>) -> Self {
        #[cfg(target_os = "linux")]
        if let Some(window) = creation.winit_window().cloned() {
            let repaint = creation.egui_ctx.clone();
            let limits = ReceiveLimits {
                max_files: crate::app::MAX_COMPOSER_ATTACHMENTS,
                ..ReceiveLimits::default()
            };
            // SAFETY: eframe owns the display and the surface until the event
            // loop ends, and `close_and_wait` runs from `on_exit`, inside it.
            // Nothing else reads the display socket outside libwayland.
            let receiver = unsafe {
                FileDropReceiver::attach_owned(window, limits, move || repaint.request_repaint())
            };
            if let Ok(Some(receiver)) = receiver {
                let epoch = receiver.invalidate_pending().ok();
                return Self { receiver: Some(receiver), epoch, ..Self::default() };
            }
        }
        let _ = creation;
        Self::default()
    }

    /// Drain the receiver. A complete drop is accepted and its paths returned
    /// only while `accepting`; otherwise the source is told no.
    pub(crate) fn poll(&mut self, accepting: bool) -> Vec<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            let Some(receiver) = &self.receiver else {
                return Vec::new();
            };
            let mut dropped = Vec::new();
            for event in receiver.poll() {
                match event {
                    ReceiveEvent::Hovered { .. } => self.hovering = true,
                    ReceiveEvent::Left { .. } => self.hovering = false,
                    ReceiveEvent::DropStarted { id, epoch, .. } => {
                        self.hovering = false;
                        if accepting && self.epoch == Some(epoch) {
                            self.pending = Some(id);
                        } else {
                            receiver.cancel(id);
                        }
                    }
                    ReceiveEvent::DropReady { id, paths } => {
                        if accepting
                            && self.pending == Some(id)
                            && receiver.decide(id, DropDecision::Accept).is_ok()
                        {
                            dropped.extend(paths);
                        } else {
                            receiver.cancel(id);
                        }
                    }
                    ReceiveEvent::DropEnded { id, .. } => {
                        if self.pending == Some(id) {
                            self.pending = None;
                        }
                    }
                    _ => {}
                }
            }
            dropped
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = accepting;
            Vec::new()
        }
    }

    /// The receiver's worker shares the window's display connection: let it
    /// clean up before eframe disconnects. Bounded, so a stuck worker cannot
    /// hold the exit.
    pub(crate) fn close_and_wait(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(receiver) = self.receiver.take() {
            receiver.close();
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
            while !matches!(receiver.status(), ReceiveStatus::Closed(_))
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
}

/// The text a file paste carries, so a drop goes through the same worker.
pub(crate) fn file_url_list(paths: &[PathBuf]) -> Option<String> {
    let urls = paths
        .iter()
        .filter_map(|path| reqwest::Url::from_file_path(path).ok())
        .map(String::from)
        .collect::<Vec<_>>();
    (!urls.is_empty()).then(|| urls.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_paths_become_the_file_list_a_paste_carries() {
        let list = file_url_list(&[PathBuf::from("/tmp/a b.png"), PathBuf::from("relative.txt")]);
        assert_eq!(list.as_deref(), Some("file:///tmp/a%20b.png"));
        assert!(crate::composer_attachments::is_file_paste(&list.unwrap()));
        assert_eq!(file_url_list(&[]), None);
    }
}
