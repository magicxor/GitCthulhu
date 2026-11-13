//! The `git` executable side of GitCthulhu's askpass support.
//!
//! [`askpass_harness`] is what the backend (`gitcthulhu-git`) builds on: the askpass server and the transport
//! defaults for SSH and HTTP. The askpass helper program is not here (it is `gitcthulhu_git::helper_main`).
#![deny(missing_docs, unsafe_code)]
#![allow(async_fn_in_trait)]
#![allow(unknown_lints)]

#[cfg(feature = "tokio")]
pub mod askpass_harness;
/// The askpass server: a Unix domain socket or a Windows named pipe, and the peer checks.
pub mod executor;

#[cfg(feature = "tokio")]
pub use self::executor::tokio;
