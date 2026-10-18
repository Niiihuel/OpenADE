//! openADE Remote: the HTTPS server the window runs so a paired phone can
//! follow and drive its tasks.

pub mod auth;
pub mod bridge;
mod diff;
pub mod error;
mod git_workspace;
pub mod network;
pub mod pairing;
pub mod projection;
pub mod push;
pub mod server;
pub mod storage;
mod stream;
pub mod tasks;
pub mod tls;
mod workspace;
