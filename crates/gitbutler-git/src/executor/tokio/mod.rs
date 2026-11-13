//! The [Tokio](https://tokio.rs)-based askpass server.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use self::unix::TokioAskpassServer;
#[cfg(unix)]
pub(crate) use self::unix::stat;
#[cfg(windows)]
pub use self::windows::TokioAskpassServer;
#[cfg(windows)]
pub(crate) use self::windows::stat;
