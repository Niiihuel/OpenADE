//! Serial background Git projection for the code review dock. A diff can be
//! large and launching Git processes from the UI event loop stalls painting.

use crate::git_review::{load_git_review, GitReview};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SendError, Sender, TryRecvError};
use std::thread;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewRequest {
    pub workspace: PathBuf,
    /// Monotonic revision owned by the window. Increase on tab activation,
    /// project change, and explicit refresh.
    pub generation: u64,
}

#[derive(Debug)]
pub struct ReviewResponse {
    pub workspace: PathBuf,
    pub generation: u64,
    pub result: Result<GitReview, String>,
}

impl ReviewResponse {
    pub fn matches(&self, generation: u64, workspace: &Path) -> bool {
        self.generation == generation && self.workspace == workspace
    }
}

/// A single worker serializes Git invocations. If the user changes projects
/// during a scan, queued requests are collapsed to the newest generation.
/// The scan already running finishes, then the UI discards its stale result.
pub struct ReviewWorker {
    request_tx: Sender<ReviewRequest>,
    result_rx: Receiver<ReviewResponse>,
}

impl ReviewWorker {
    pub fn spawn() -> io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        thread::Builder::new()
            .name("openide-git-review".to_owned())
            .spawn(move || run_worker(request_rx, result_tx))?;
        Ok(Self {
            request_tx,
            result_rx,
        })
    }

    pub fn submit(&self, request: ReviewRequest) -> Result<(), SendError<ReviewRequest>> {
        self.request_tx.send(request)
    }

    /// Drain all completed scans without blocking. Only the response for the
    /// exact current request can update the dock.
    pub fn poll_latest(&self, generation: u64, workspace: &Path) -> Option<ReviewResponse> {
        let mut latest = None;
        while let Ok(response) = self.result_rx.try_recv() {
            if response.matches(generation, workspace) {
                latest = Some(response);
            }
        }
        latest
    }
}

fn run_worker(request_rx: Receiver<ReviewRequest>, result_tx: Sender<ReviewResponse>) {
    while let Ok(first) = request_rx.recv() {
        let request = latest_pending(first, &request_rx);
        let result = load_git_review(&request.workspace).map_err(|error| error.to_string());
        if result_tx
            .send(ReviewResponse {
                workspace: request.workspace,
                generation: request.generation,
                result,
            })
            .is_err()
        {
            return;
        }
    }
}

fn latest_pending(
    mut request: ReviewRequest,
    request_rx: &Receiver<ReviewRequest>,
) -> ReviewRequest {
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
    use std::time::Duration;

    fn request(workspace: &str, generation: u64) -> ReviewRequest {
        ReviewRequest {
            workspace: PathBuf::from(workspace),
            generation,
        }
    }

    #[test]
    fn coalesces_pending_workspaces_and_refreshes_to_newest_generation() {
        let (tx, rx) = mpsc::channel();
        tx.send(request("old-project", 2)).unwrap();
        tx.send(request("new-project", 4)).unwrap();
        tx.send(request("old-project", 3)).unwrap();
        let latest = latest_pending(request("first-project", 1), &rx);
        assert_eq!(latest, request("new-project", 4));
    }

    #[test]
    fn polling_discards_previous_refresh_and_other_workspace() {
        let (request_tx, _) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = ReviewWorker {
            request_tx,
            result_rx,
        };
        for request in [
            request("project", 1),
            request("other", 2),
            request("project", 2),
        ] {
            result_tx
                .send(ReviewResponse {
                    workspace: request.workspace,
                    generation: request.generation,
                    result: Ok(GitReview::default()),
                })
                .unwrap();
        }
        assert_eq!(
            worker
                .poll_latest(2, Path::new("project"))
                .unwrap()
                .generation,
            2
        );
        assert!(worker.poll_latest(2, Path::new("project")).is_none());
    }

    #[test]
    fn git_errors_are_returned_off_thread() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("missing");
        let worker = ReviewWorker::spawn().unwrap();
        worker
            .submit(ReviewRequest {
                workspace: workspace.clone(),
                generation: 7,
            })
            .unwrap();
        let response = worker
            .result_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(response.matches(7, &workspace));
        assert!(response.result.is_err());
    }
}
