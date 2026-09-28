//! Serial, on-demand file and diff loading for the right dock. The UI never
//! scans a large file or starts Git while processing a click or paint event.

use std::{
    io,
    path::PathBuf,
    sync::mpsc::{self, Receiver, SendError, Sender, TryRecvError},
    thread,
};

use crate::{
    files::{read_text_preview_page, PreviewPage},
    git_review::{load_file_diff, DiffStage, FileDiffDetail},
};

pub const PREVIEW_PAGE_LINES: usize = 120;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DetailKind {
    Preview { path: PathBuf, page: usize },
    Diff { path: PathBuf, stage: DiffStage },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetailRequest {
    pub workspace: PathBuf,
    pub generation: u64,
    pub kind: DetailKind,
}

#[derive(Debug)]
pub enum DetailResult {
    Preview(Result<PreviewPage, String>),
    Diff(Result<Option<FileDiffDetail>, String>),
}

#[derive(Debug)]
pub struct DetailResponse {
    pub workspace: PathBuf,
    pub generation: u64,
    pub kind: DetailKind,
    pub result: DetailResult,
}

impl DetailResponse {
    pub fn matches(&self, request: &DetailRequest) -> bool {
        self.workspace == request.workspace
            && self.generation == request.generation
            && self.kind == request.kind
    }
}

pub struct DetailWorker {
    request_tx: Sender<DetailRequest>,
    result_rx: Receiver<DetailResponse>,
}

impl DetailWorker {
    pub fn spawn() -> io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        thread::Builder::new()
            .name("openide-file-detail".to_owned())
            .spawn(move || run_worker(request_rx, result_tx))?;
        Ok(Self {
            request_tx,
            result_rx,
        })
    }

    pub fn submit(&self, request: DetailRequest) -> Result<(), SendError<DetailRequest>> {
        self.request_tx.send(request)
    }

    /// Ignore stale responses after a tab, project, or page change.
    pub fn poll_latest(&self, request: &DetailRequest) -> Option<DetailResponse> {
        let mut latest = None;
        while let Ok(response) = self.result_rx.try_recv() {
            if response.matches(request) {
                latest = Some(response);
            }
        }
        latest
    }
}

fn run_worker(request_rx: Receiver<DetailRequest>, result_tx: Sender<DetailResponse>) {
    while let Ok(first) = request_rx.recv() {
        let request = latest_pending(first, &request_rx);
        let result = match &request.kind {
            DetailKind::Preview { path, page } => DetailResult::Preview(
                read_text_preview_page(&request.workspace, path, *page, PREVIEW_PAGE_LINES)
                    .map_err(|error| error.to_string()),
            ),
            DetailKind::Diff { path, stage } => DetailResult::Diff(
                load_file_diff(&request.workspace, path, *stage).map_err(|error| error.to_string()),
            ),
        };
        if result_tx
            .send(DetailResponse {
                workspace: request.workspace,
                generation: request.generation,
                kind: request.kind,
                result,
            })
            .is_err()
        {
            return;
        }
    }
}

fn latest_pending(
    mut request: DetailRequest,
    request_rx: &Receiver<DetailRequest>,
) -> DetailRequest {
    loop {
        match request_rx.try_recv() {
            Ok(next) if next.generation >= request.generation => request = next,
            Ok(_) => {}
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return request,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_page_replaces_a_pending_scan_and_old_reply_is_ignored() {
        let (tx, rx) = mpsc::channel();
        let make_request = |generation, page| DetailRequest {
            workspace: PathBuf::from("project"),
            generation,
            kind: DetailKind::Preview {
                path: PathBuf::from("main.rs"),
                page,
            },
        };
        tx.send(make_request(2, 1)).unwrap();
        tx.send(make_request(3, 2)).unwrap();
        assert_eq!(latest_pending(make_request(1, 0), &rx), make_request(3, 2));

        let stale = DetailResponse {
            workspace: PathBuf::from("project"),
            generation: 2,
            kind: DetailKind::Preview {
                path: PathBuf::from("main.rs"),
                page: 1,
            },
            result: DetailResult::Preview(Ok(PreviewPage::default())),
        };
        assert!(!stale.matches(&make_request(3, 2)));
    }
}
