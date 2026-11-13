//! Unix: connect to the askpass server's domain socket.

use std::{io, os::unix::net::UnixStream};

/// Connect to the socket at `path`; returns independent read and write halves.
pub(super) fn connect(path: &str) -> io::Result<(UnixStream, UnixStream)> {
    let stream = UnixStream::connect(path)?;
    let reader = stream.try_clone()?;
    Ok((reader, stream))
}
