//! `tracing` subscriber with a size-rotated log file in [`crate::paths::log_dir`] (10 MB x 5, spec 21 §1.1).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// Maximum size of one log file before it is rotated.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Number of log files kept (the active one plus rotated ones).
pub const MAX_FILES: usize = 5;
const FILE_NAME: &str = "gitcthulhu.log";

/// A log file that rotates to `gitcthulhu.log.1` .. `gitcthulhu.log.{max_files-1}` when it grows past its limit.
pub struct RollingFile {
    dir: PathBuf,
    max_bytes: u64,
    max_files: usize,
    file: Option<File>,
    written: u64,
}

impl RollingFile {
    /// Opens (or creates) the active log file in `dir`.
    pub fn open(dir: &Path, max_bytes: u64, max_files: usize) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(FILE_NAME);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            dir: dir.to_owned(),
            max_bytes,
            max_files: max_files.max(1),
            file: Some(file),
            written,
        })
    }

    fn path_of(&self, index: usize) -> PathBuf {
        if index == 0 {
            self.dir.join(FILE_NAME)
        } else {
            self.dir.join(format!("{FILE_NAME}.{index}"))
        }
    }

    fn rotate(&mut self) -> io::Result<()> {
        // Close the active file first: Windows cannot rename an open file.
        self.file = None;
        let oldest = self.path_of(self.max_files - 1);
        if oldest.exists() {
            fs::remove_file(&oldest)?;
        }
        for index in (0..self.max_files - 1).rev() {
            let from = self.path_of(index);
            if from.exists() {
                fs::rename(&from, self.path_of(index + 1))?;
            }
        }
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.path_of(0))?,
        );
        self.written = 0;
        Ok(())
    }
}

impl Write for RollingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written >= self.max_bytes {
            self.rotate()?;
        }
        let Some(file) = self.file.as_mut() else {
            return Err(io::Error::other("log file is closed"));
        };
        let n = file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

#[derive(Clone)]
struct SharedWriter(Arc<Mutex<RollingFile>>);

struct SharedGuard(Arc<Mutex<RollingFile>>);

impl Write for SharedGuard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().flush()
    }
}

impl<'a> MakeWriter<'a> for SharedWriter {
    type Writer = SharedGuard;

    fn make_writer(&'a self) -> Self::Writer {
        SharedGuard(self.0.clone())
    }
}

/// Installs the global subscriber: file output (when the log directory is writable) and `RUST_LOG` filtering
/// (default `info`). Safe to call more than once; later calls are ignored.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false);
    let result = match RollingFile::open(&crate::paths::log_dir(), MAX_FILE_BYTES, MAX_FILES) {
        Ok(file) => builder
            .with_writer(SharedWriter(Arc::new(Mutex::new(file))))
            .try_init(),
        // No writable log directory: still log to stderr (visible when started from a terminal).
        Err(_) => builder.with_writer(io::stderr).try_init(),
    };
    // An already installed subscriber (tests, embedding) is not an error.
    drop(result);
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn rotation_keeps_the_configured_number_of_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut log = RollingFile::open(dir.path(), 10, 3).expect("open log");
        for _ in 0..10 {
            log.write_all(b"0123456789").expect("write");
        }
        let mut names: Vec<_> = fs::read_dir(dir.path())
            .expect("read dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["gitcthulhu.log", "gitcthulhu.log.1", "gitcthulhu.log.2"],
            "only the active file and two rotated files are kept"
        );
    }
}
