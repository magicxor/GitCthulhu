//! A minimal blocking client for a Windows named pipe (the std library has no named pipe client).
#![allow(unsafe_code)] // Win32 pipe calls.

use std::{
    io::{self, Read, Write},
    os::windows::ffi::OsStrExt,
    path::Path,
};

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_BROKEN_PIPE,
            ERROR_PIPE_BUSY, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
            WIN32_ERROR,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
            FlushFileBuffers, OPEN_EXISTING, ReadFile, SECURITY_IDENTIFICATION,
            SECURITY_SQOS_PRESENT, WriteFile,
        },
        System::{
            Pipes::{NMPWAIT_USE_DEFAULT_WAIT, WaitNamedPipeW},
            Threading::GetCurrentProcess,
        },
    },
    core::PCWSTR,
};

/// How often a busy pipe (`ERROR_PIPE_BUSY`: every instance is connected to another client) is retried; each try
/// waits for the default timeout of the pipe.
const CONNECT_ATTEMPTS: usize = 10;

/// A `windows` error as `std::io::Error` with the Win32 error code.
///
/// `windows::core::Error::code()` is an `HRESULT` (`0x8007_xxxx` for Win32 errors), not the Win32 error code that
/// `io::Error::from_raw_os_error` expects.
fn io_error(err: &windows::core::Error) -> io::Error {
    match WIN32_ERROR::from_error(err) {
        Some(code) => io::Error::from_raw_os_error(code.0 as i32),
        None => io::Error::other(err.message()),
    }
}

#[derive(Debug)]
struct Handle(HANDLE);

impl Handle {
    fn try_clone(&self) -> io::Result<Handle> {
        let mut new_handle = HANDLE::default();
        // SAFETY: `self.0` is a valid open handle, the target is a valid out pointer.
        let res = unsafe {
            let process = GetCurrentProcess();
            DuplicateHandle(
                process,
                self.0,
                process,
                &mut new_handle,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        };
        match res {
            Ok(()) => Ok(Handle(new_handle)),
            Err(err) => Err(io_error(&err)),
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// A connected client end of a named pipe.
#[derive(Debug)]
pub(super) struct Pipe {
    handle: Handle,
}

impl Pipe {
    /// Connect to the pipe at `path` (`//./pipe/<name>` or `\\.\pipe\<name>`).
    pub(super) fn connect(path: &Path) -> io::Result<Pipe> {
        let wide_path: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let name = PCWSTR(wide_path.as_ptr());

        let mut last_err = None;
        for _ in 0..CONNECT_ATTEMPTS {
            // SAFETY: `name` points to a NUL-terminated wide string that outlives the call.
            let res = unsafe {
                CreateFileW(
                    name,
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    // The server may only identify the helper, never act as its user.
                    FILE_ATTRIBUTE_NORMAL | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    None,
                )
            };
            match res {
                Ok(handle) => {
                    return Ok(Pipe {
                        handle: Handle(handle),
                    });
                }
                Err(err) => {
                    if WIN32_ERROR::from_error(&err) != Some(ERROR_PIPE_BUSY) {
                        return Err(io_error(&err));
                    }
                    // SAFETY: as above.
                    let _ = unsafe { WaitNamedPipeW(name, NMPWAIT_USE_DEFAULT_WAIT) };
                    last_err = Some(io_error(&err));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| io::Error::other("the askpass pipe stayed busy")))
    }

    /// A second handle to the same pipe, for independent reading and writing.
    pub(super) fn try_clone(&self) -> io::Result<Pipe> {
        Ok(Pipe {
            handle: self.handle.try_clone()?,
        })
    }
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes_read = 0u32;
        // SAFETY: the handle is open; `buf` and `bytes_read` are valid for the call.
        let res = unsafe { ReadFile(self.handle.0, Some(buf), Some(&mut bytes_read), None) };
        match res {
            Ok(()) => Ok(bytes_read as usize),
            Err(err) => match WIN32_ERROR::from_error(&err) {
                // The server closed its end: end of file.
                Some(ERROR_PIPE_NOT_CONNECTED | ERROR_BROKEN_PIPE) => Ok(0),
                _ => Err(io_error(&err)),
            },
        }
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut bytes_written = 0u32;
        // SAFETY: the handle is open; `buf` and `bytes_written` are valid for the call.
        let res = unsafe { WriteFile(self.handle.0, Some(buf), Some(&mut bytes_written), None) };
        match res {
            Ok(()) => Ok(bytes_written as usize),
            Err(err) => Err(io_error(&err)),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // SAFETY: the handle is open.
        match unsafe { FlushFileBuffers(self.handle.0) } {
            Ok(()) => Ok(()),
            Err(err) => Err(io_error(&err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

    use super::*;

    fn pipe_name() -> String {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        format!(
            "//./pipe/gitcthulhu-askpass-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_pipe_is_reported_with_its_win32_code() {
        let err = Pipe::connect(Path::new(&pipe_name())).expect_err("nobody listens");
        assert_eq!(
            err.kind(),
            io::ErrorKind::NotFound,
            "ERROR_FILE_NOT_FOUND, not an HRESULT: {err:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn busy_pipe_is_retried_until_an_instance_is_free() {
        let name = pipe_name();
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(1)
            .create(&name)
            .expect("pipe can be created");
        let occupant = ClientOptions::new()
            .open(&name)
            .expect("first client connects");
        server
            .connect()
            .await
            .expect("server accepts the first client");

        let waiting = std::thread::spawn({
            let name = name.clone();
            move || Pipe::connect(Path::new(&name)).map(|mut pipe| pipe.write_all(b"hi\n"))
        });
        // The only instance is taken: the helper must wait instead of failing with ERROR_PIPE_BUSY.
        tokio::time::sleep(Duration::from_millis(100)).await;
        server
            .disconnect()
            .expect("server lets the first client go");
        drop(occupant);
        let accepted = tokio::time::timeout(Duration::from_secs(5), server.connect()).await;

        waiting
            .join()
            .expect("thread joins")
            .expect("the helper connected after waiting")
            .expect("and could write");
        accepted
            .expect("the waiting client connects in time")
            .expect("server accepts it");
    }
}
