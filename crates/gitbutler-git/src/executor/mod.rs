#[cfg(any(test, feature = "tokio"))]
pub mod tokio;

/// Stats for a file on the filesystem.
///
/// This is returned by `tokio::stat`,
/// and is just a small subset of the information
/// typically returned by `stat(2)` and the like,
/// as we only need a small subset of the information
/// to perform some baseline security checks during
/// the authentication process.
#[derive(Debug, Clone)]
pub struct FileStat {
    /// The device number of the filesystem containing the file.
    ///
    /// On Windows, this is (probably) always 0.
    pub dev: u64,
    /// The inode number of the file.
    pub ino: u64,
    /// If the file is a regular file (not a symlink).
    pub is_regular_file: bool,
}

/// An askpass server compatible with the askpass helper (`gitcthulhu_git::helper_main::askpass_main`).
///
/// When formatted as a string, the result is the connection string the askpass helper gets as
/// `GITCTHULHU_ASKPASS_PIPE`:
///
/// - Unix: the path of a Unix domain socket, readable and writable by the user only (`0600`); it is closed and
///   best-effort unlinked when the handle is dropped.
/// - Windows: the name of a named pipe, starting with `\\.\pipe\LOCAL\`; it is closed when the handle is dropped.
pub trait AskpassServer: core::fmt::Display {
    /// The type of error that is returned by [`AskpassServer::accept`].
    type Error: std::error::Error + core::fmt::Debug + Send + Sync + 'static;

    /// The type of the socket yielded by the incoming iterator.
    type SocketHandle: Socket + Send + Sync + 'static;

    /// Waits for a connection to the server to be established.
    ///
    /// Waits indefinitely; callers that need a deadline should wrap the
    /// future themselves (e.g. with `tokio::time::timeout`).
    async fn accept(&self) -> Result<Self::SocketHandle, Self::Error>;
}

#[cfg(unix)]
type PidInner = i32;

#[cfg(windows)]
type PidInner = u32;

/// The type of a process ID (platforms-specific)
pub type Pid = PidInner;

/// The type of a user ID (unix-specific).
#[cfg(unix)]
pub type Uid = u32;

/// A connection accepted by an [`AskpassServer`], with the platform-specific credentials of its peer.
pub trait Socket {
    /// The error type returned by I/O operations on this socket.
    type Error: std::error::Error + core::fmt::Debug + Send + Sync + 'static;

    /// The process ID of the connecting client.
    fn pid(&self) -> Result<Pid, Self::Error>;

    /// The user ID of the connecting client.
    #[cfg(unix)]
    fn uid(&self) -> Result<Uid, Self::Error>;

    /// Reads a line from the socket. Must not include the newline.
    ///
    /// The returned line must not include a newline, and any
    /// trailing carriage return (`\r`) must be stripped.
    ///
    /// Implementations are allowed to simply call `.trim()` on the
    /// line, as whitespace is not significant in the protocol.
    async fn read_line(&mut self) -> Result<String, Self::Error>;

    /// Writes a line to the socket. The write must
    /// complete fully before returning (i.e. implementations
    /// should use something akin to `write_all`).
    ///
    /// The input line will not include a newline; one must be
    /// added. Newlines should never include a carriage return (`\r`).
    ///
    /// Unlike `read_line`, implementations are not allowed to
    /// modify the line prior to sending aside from appending a newline.
    async fn write_line(&mut self, line: &str) -> Result<(), Self::Error>;
}
