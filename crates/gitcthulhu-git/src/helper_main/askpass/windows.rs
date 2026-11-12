//! Windows: connect to the askpass server's named pipe.

use std::{io, path::Path};

use super::windows_pipe::Pipe;

/// Connect to the pipe `name`; returns independent read and write halves.
pub(super) fn connect(name: &str) -> io::Result<(Pipe, Pipe)> {
    let pipe = Pipe::connect(Path::new(name))?;
    let reader = pipe.try_clone()?;
    Ok((reader, pipe))
}
