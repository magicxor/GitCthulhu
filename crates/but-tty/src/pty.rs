//! The user's shell on a pseudo-terminal, with what it writes read on its own thread.

use std::ffi::OsString;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{self, Poll};
use std::{io, mem};

use futures::channel::mpsc;
use futures::{Stream, StreamExt as _};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use self::windows::Pty;
#[cfg(unix)]
pub use unix::{Kept, Pty};

/// How much of what the shell wrote waits to be shown before the reader stops reading it,
/// leaving the shell waiting to write the rest.
const MOST_QUEUED: usize = 1024 * 1024;

// ponytail: a full queue is checked on a timer, have Output wake the reader if that shows.
const PAUSE_MS: u16 = 1;

/// What the shell writes, all that's waiting at a time, ending once it exits.
pub struct Output {
    chunks: mpsc::UnboundedReceiver<Vec<u8>>,
    queued: Arc<AtomicUsize>,
    /// What was last taken, which stays queued until the next is asked for, so the reader
    /// doesn't refill the queue while it's being shown.
    shown: usize,
}

impl Output {
    /// All that's waiting now, without waiting for more.
    #[cfg(all(unix, test))]
    pub(crate) fn waiting(&mut self) -> Vec<u8> {
        let waiting: Vec<u8> = std::iter::from_fn(|| self.chunks.try_recv().ok())
            .flatten()
            .collect();
        self.queued.fetch_sub(waiting.len(), Ordering::SeqCst);
        waiting
    }
}

impl Stream for Output {
    type Item = Vec<u8>;

    fn poll_next(self: Pin<&mut Self>, task: &mut task::Context) -> Poll<Option<Vec<u8>>> {
        let this = self.get_mut();
        (this.queued).fetch_sub(mem::take(&mut this.shown), Ordering::SeqCst);
        let Some(mut all) = std::task::ready!(this.chunks.poll_next_unpin(task)) else {
            return Poll::Ready(None);
        };
        while let Ok(more) = this.chunks.try_recv() {
            all.extend(more);
        }
        this.shown = all.len();
        Poll::Ready(Some(all))
    }
}

/// The reader's end of an [`Output`].
#[derive(Clone)]
struct Queue {
    chunks: mpsc::UnboundedSender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
}

impl Queue {
    /// Whether to stop reading until some of it is shown. Never once no one's listening.
    fn full(&self) -> bool {
        !self.chunks.is_closed() && self.queued.load(Ordering::SeqCst) >= MOST_QUEUED
    }

    fn send(&self, chunk: Vec<u8>) -> Result<(), mpsc::TrySendError<Vec<u8>>> {
        self.queued.fetch_add(chunk.len(), Ordering::SeqCst);
        self.chunks.unbounded_send(chunk)
    }

    fn close(&self) {
        self.chunks.close_channel();
    }
}

fn queue() -> (Queue, Output) {
    let (sender, chunks) = mpsc::unbounded();
    let queued = Arc::new(AtomicUsize::new(0));
    let queue = Queue {
        chunks: sender,
        queued: queued.clone(),
    };
    let output = Output {
        chunks,
        queued,
        shown: 0,
    };
    (queue, output)
}

/// How a shell is told to run a command in place of its prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// cmd.exe: `/s /c "<command>"`, the command as written.
    Cmd,
    /// PowerShell (pwsh.exe, powershell.exe): `-Command <command>`.
    PowerShell,
    /// A POSIX shell (sh, bash, zsh, fish): `-i -c <command>`, still interactive so it reads the
    /// same config as at a prompt.
    Posix,
    /// A program that can't be given a command, such as `wsl.exe` or a user's own command line:
    /// it only ever starts at its prompt.
    Other,
}

/// The shell a terminal starts: the program, the arguments it always gets, how it's told to run
/// a command, and variables set for it on top of this process's environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSpec {
    /// The program, a path or a name looked up on `PATH`.
    pub program: OsString,
    /// Arguments before the command's, if any.
    pub args: Vec<OsString>,
    pub kind: ShellKind,
    /// Set for the shell, after the terminal's own (`TERM` and friends).
    pub env: Vec<(OsString, OsString)>,
}

impl ShellSpec {
    /// The platform's default: `%COMSPEC%`, else cmd.exe, on Windows; elsewhere `$SHELL`, else
    /// /bin/zsh on macOS and /bin/sh, as a login shell.
    pub fn default_shell() -> Self {
        #[cfg(windows)]
        {
            Self {
                program: std::env::var_os("COMSPEC")
                    .filter(|shell| !shell.is_empty())
                    .unwrap_or_else(|| "cmd.exe".into()),
                args: Vec::new(),
                kind: ShellKind::Cmd,
                env: Vec::new(),
            }
        }
        #[cfg(unix)]
        {
            let program = std::env::var_os("SHELL")
                .filter(|shell| !shell.is_empty())
                .unwrap_or_else(|| {
                    if cfg!(target_os = "macos") {
                        "/bin/zsh"
                    } else {
                        "/bin/sh"
                    }
                    .into()
                });
            Self {
                program,
                args: vec!["-l".into()],
                kind: ShellKind::Posix,
                env: Vec::new(),
            }
        }
    }

    /// The arguments that run `command`, after [`ShellSpec::args`], as the shell's kind takes them,
    /// or an error for a shell that can't run one.
    pub fn command_args(&self, command: &str) -> io::Result<Vec<OsString>> {
        Ok(match self.kind {
            ShellKind::Cmd => vec!["/s".into(), "/c".into(), command.into()],
            ShellKind::PowerShell => vec!["-Command".into(), command.into()],
            ShellKind::Posix => vec!["-i".into(), "-c".into(), command.into()],
            ShellKind::Other => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "this shell can't be given a command to run",
                ));
            }
        })
    }
}

/// A grid size, with the pixel size of one cell for programs that ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

#[cfg(test)]
mod tests {
    use futures::FutureExt as _;

    use super::*;

    #[test]
    fn what_is_shown_stays_queued_until_the_next_is_asked_for() {
        let (queue, mut output) = queue();
        for _ in 0..16 {
            queue.send(vec![0; 64 * 1024]).unwrap();
        }
        assert!(queue.full(), "16 chunks of 64KiB are all it holds");
        let shown = output.next().now_or_never().flatten().unwrap();
        assert_eq!(shown.len(), MOST_QUEUED, "all that waits comes at once");
        assert!(queue.full(), "the reader stays paused while it's shown");
        assert!(output.next().now_or_never().is_none(), "nothing more waits");
        assert!(!queue.full(), "asking again means it was shown");
    }

    #[test]
    fn a_queue_no_one_reads_is_never_full() {
        let (queue, output) = queue();
        queue.send(vec![0; MOST_QUEUED]).unwrap();
        assert!(queue.full(), "it fills while someone could read it");
        drop(output);
        assert!(
            !queue.full(),
            "a reader stopped now would never start again"
        );
    }
}

#[cfg(test)]
mod spec_tests {
    use super::*;

    fn spec(kind: ShellKind) -> ShellSpec {
        ShellSpec {
            program: "sh".into(),
            args: Vec::new(),
            kind,
            env: Vec::new(),
        }
    }

    #[test]
    fn each_kind_of_shell_is_told_a_command_its_own_way() {
        let args = |kind| -> Vec<String> {
            (spec(kind)
                .command_args("ls")
                .expect("this kind takes commands"))
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
        };
        assert_eq!(args(ShellKind::Cmd), ["/s", "/c", "ls"], "cmd.exe");
        assert_eq!(
            args(ShellKind::PowerShell),
            ["-Command", "ls"],
            "PowerShell"
        );
        assert_eq!(
            args(ShellKind::Posix),
            ["-i", "-c", "ls"],
            "interactive POSIX shells"
        );
        assert!(
            spec(ShellKind::Other).command_args("ls").is_err(),
            "a shell of no known kind is never given a command"
        );
    }
}
