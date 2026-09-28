//! Serial background GitHub requests. Network latency never holds the UI loop.

use crate::github::{load_overview, GithubOverview};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SendError, Sender};
use std::thread;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GithubRequest {
    pub workspace: PathBuf,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GithubResponse {
    pub workspace: PathBuf,
    pub generation: u64,
    pub overview: Option<GithubOverview>,
}

pub struct GithubWorker {
    request_tx: Sender<GithubRequest>,
    result_rx: Receiver<GithubResponse>,
}

impl GithubWorker {
    pub fn spawn() -> io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        thread::Builder::new()
            .name("openide-github".to_owned())
            .spawn(move || run_worker(request_rx, result_tx))?;
        Ok(Self {
            request_tx,
            result_rx,
        })
    }

    pub fn submit(&self, request: GithubRequest) -> Result<(), SendError<GithubRequest>> {
        self.request_tx.send(request)
    }

    pub fn poll_latest(&self, generation: u64, workspace: &Path) -> Option<GithubResponse> {
        let mut latest = None;
        while let Ok(response) = self.result_rx.try_recv() {
            if response.generation == generation && response.workspace == workspace {
                latest = Some(response);
            }
        }
        latest
    }
}

fn run_worker(request_rx: Receiver<GithubRequest>, result_tx: Sender<GithubResponse>) {
    while let Ok(first) = request_rx.recv() {
        let mut request = first;
        while let Ok(next) = request_rx.try_recv() {
            if next.generation >= request.generation {
                request = next;
            }
        }
        let overview = load_overview(&request.workspace);
        if result_tx
            .send(GithubResponse {
                workspace: request.workspace,
                generation: request.generation,
                overview,
            })
            .is_err()
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_response_supersedes_old_or_foreign_workspaces() {
        let (request_tx, _) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = GithubWorker {
            request_tx,
            result_rx,
        };
        for (workspace, generation) in [("repo", 1), ("other", 2), ("repo", 3)] {
            result_tx
                .send(GithubResponse {
                    workspace: workspace.into(),
                    generation,
                    overview: None,
                })
                .unwrap();
        }
        assert_eq!(
            worker.poll_latest(3, Path::new("repo")).unwrap().generation,
            3
        );
    }
}
