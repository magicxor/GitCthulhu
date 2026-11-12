//! The `git` executable side of GitCthulhu's askpass support, plus simple fetch/push/clone helpers over the Git CLI.
//!
//! [`askpass_harness`] is the part the backend (`gitcthulhu-git`) builds on: the askpass server and the transport
//! defaults for SSH and HTTP. The askpass helper program is not here (it is `gitcthulhu_git::helper_main`).
#![deny(missing_docs, unsafe_code)]
#![allow(async_fn_in_trait)]
#![allow(unknown_lints)]

#[cfg(feature = "tokio")]
pub mod askpass_harness;
mod error;
/// utilities to execute a command
pub mod executor;
mod refspec;
mod repository;

#[cfg(feature = "tokio")]
pub use self::executor::tokio;
pub use self::{
    error::Error,
    refspec::{Error as RefSpecError, RefSpec},
    repository::{clone, fetch, prune, push},
};
