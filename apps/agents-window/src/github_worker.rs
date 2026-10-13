//! Serial background GitHub requests. Network latency never holds the UI loop.

use crate::github::{create_pull, load_overview, GithubOverview, PullDraft};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SendError, Sender};
use std::thread;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GithubRequest {
    pub workspace: PathBuf,
    pub generation: u64,
    /// None means public requests with no credential lookup.
    pub account: Option<String>,
    /// Open this pull request for the current branch first.
    pub create_pull: Option<PullDraft>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GithubResponse {
    pub workspace: PathBuf,
    pub generation: u64,
    pub account: Option<String>,
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

    pub fn poll_latest(
        &self,
        generation: u64,
        workspace: &Path,
        account: Option<&str>,
    ) -> Option<GithubResponse> {
        let mut latest = None;
        while let Ok(response) = self.result_rx.try_recv() {
            if response.generation == generation
                && response.workspace == workspace
                && response.account.as_deref() == account
            {
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
                // A read may replace a read; a pull request asked for is still opened.
                let create_pull = next.create_pull.clone().or(request.create_pull);
                request = GithubRequest { create_pull, ..next };
            }
        }
        let created = request.create_pull.as_ref().map(|draft| match request.account.as_deref() {
            Some(account) => create_pull(&request.workspace, account, draft),
            None => Err("Conectá una cuenta de GitHub para crear el pull request".to_owned()),
        });
        let mut overview = load_overview(&request.workspace, request.account.as_deref());
        if let Some(overview) = &mut overview {
            match created {
                // The list may not show a pull request opened a moment ago.
                Some(Ok(pull)) if !overview.pulls.iter().any(|known| known.number == pull.number) => {
                    overview.pulls.insert(0, pull)
                }
                Some(Err(error)) => overview.notice = Some(error),
                _ => {}
            }
        }
        if result_tx
            .send(GithubResponse {
                workspace: request.workspace,
                generation: request.generation,
                account: request.account,
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
    fn same_repository_and_revision_do_not_allow_another_accounts_response() {
        let (request_tx, _) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = GithubWorker {
            request_tx,
            result_rx,
        };
        let response = |account: Option<&str>| GithubResponse {
            workspace: PathBuf::from("repo"),
            generation: 7,
            account: account.map(str::to_owned),
            overview: None,
        };
        result_tx.send(response(Some("alice"))).unwrap();
        assert!(worker
            .poll_latest(7, Path::new("repo"), Some("bob"))
            .is_none());
        result_tx.send(response(Some("alice"))).unwrap();
        assert!(worker.poll_latest(7, Path::new("repo"), None).is_none());
        result_tx.send(response(None)).unwrap();
        assert!(worker.poll_latest(7, Path::new("repo"), None).is_some());
        result_tx.send(response(Some("bob"))).unwrap();
        assert!(worker
            .poll_latest(7, Path::new("repo"), Some("bob"))
            .is_some());
    }

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
                    account: None,
                    overview: None,
                })
                .unwrap();
        }
        assert_eq!(
            worker
                .poll_latest(3, Path::new("repo"), None)
                .unwrap()
                .generation,
            3
        );
    }
}
