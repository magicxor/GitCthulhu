//! The user's shell on a Windows pseudoconsole, with what it writes read on its own thread.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use std::{mem, ptr, sync, thread};

use anyhow::Context as _;
use parking_lot::Mutex;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{COORD, CreatePseudoConsole, HPCON, ResizePseudoConsole};
use windows::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    STARTUPINFOW, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{Owned, PCWSTR, PWSTR};

use super::{Output, PAUSE_MS, Queue, ShellKind, ShellSpec, Size, queue};

/// Closed by whichever comes first, the shell exiting or the [`Pty`] dropping, and never while
/// the lock is held, as closing can wait for the reader to drain what the shell wrote last.
type Console = Arc<Mutex<Option<Owned<HPCON>>>>;

/// A shell running on a pseudoconsole, hung up when dropped, and what it started ended with it.
pub struct Pty {
    /// What to send the shell, written on a thread of its own, as a shell that isn't reading
    /// leaves a write waiting.
    input: sync::mpsc::Sender<Vec<u8>>,
    console: Console,
    /// The job the shell and everything it starts belong to, killed as its handle closes: closing
    /// the pseudoconsole alone leaves the shell running on recent Windows builds. `None` where
    /// this process may not make one.
    job: Option<OwnedHandle>,
    /// The shell runs a command and exits rather than coming back to a prompt.
    runs_command: bool,
    /// How the shell exited, once it has.
    exit_code: Arc<sync::OnceLock<u32>>,
    /// The fewest processes seen in the job: the shell and the helpers it keeps at its prompt (Git Bash's launcher
    /// keeps one), against which more mean a program it started is running.
    at_prompt: sync::atomic::AtomicU32,
    /// The shell's process id, for tests to watch it end.
    #[cfg(test)]
    pub(crate) process_id: u32,
}

impl Size {
    fn coord(self) -> COORD {
        COORD {
            X: i16::try_from(self.cols).unwrap_or(i16::MAX),
            Y: i16::try_from(self.rows).unwrap_or(i16::MAX),
        }
    }
}

impl Pty {
    /// Starts the user's shell (%COMSPEC%, else cmd.exe) in `cwd`, sized `size`, running
    /// `command` with `/c` when there is one. The receiver yields what the shell writes, in
    /// chunks, and ends once it exits, which closes the pseudoconsole on the jobs it left.
    pub fn spawn(cwd: &Path, size: Size, command: Option<&str>) -> anyhow::Result<(Pty, Output)> {
        Self::spawn_with(&ShellSpec::default_shell(), cwd, size, command)
    }

    /// Starts `spec`'s shell in `cwd`, sized `size`, running `command` as its kind takes one
    /// when there is one. The receiver yields what the shell writes, in chunks, and ends once
    /// it exits, which closes the pseudoconsole on the jobs it left.
    pub fn spawn_with(
        spec: &ShellSpec,
        cwd: &Path,
        size: Size,
        command: Option<&str>,
    ) -> anyhow::Result<(Pty, Output)> {
        // Before anything is opened, so a shell that can't be told the command starts nothing.
        let mut command_line = command_line(spec, command)
            .with_context(|| format!("Couldn't start {}", spec.program.display()))?;
        let (input_read, input_write) = pipe().context("Couldn't open the shell's input")?;
        let (output_read, output_write) = pipe().context("Couldn't open the shell's output")?;
        let console =
            open(size, &input_read, &output_write).context("Couldn't open a pseudoconsole")?;
        drop((input_read, output_write));
        let (queue, output) = queue();
        let ended = queue.clone();
        thread::Builder::new()
            .name("pty reader".into())
            .spawn(move || drain(output_read, queue))?;

        let job = job()
            .inspect_err(|err| {
                tracing::warn!("no job for the shell, so what it starts may outlive it: {err}")
            })
            .ok();
        let started = start(&mut command_line, &spec.env, cwd, &console, job.as_ref())
            .with_context(|| format!("Couldn't start {}", spec.program.display()))?;
        #[cfg(test)]
        let process_id = started.id;
        let process = started.process;
        let console = Arc::new(Mutex::new(Some(console)));

        let (input, queued) = sync::mpsc::channel::<Vec<u8>>();
        thread::Builder::new()
            .name("pty writer".into())
            .spawn(move || {
                for bytes in queued {
                    if (&input_write).write_all(&bytes).is_err() {
                        return;
                    }
                }
            })?;

        let waited = console.clone();
        let exit_code = Arc::new(sync::OnceLock::new());
        let code = exit_code.clone();
        thread::Builder::new()
            .name("pty waiter".into())
            .spawn(move || {
                // SAFETY: the process handle is open while `process` is.
                unsafe { WaitForSingleObject(raw(&process), INFINITE) };
                let mut exited = 0;
                // SAFETY: as above, and `exited` is a live local.
                if unsafe { GetExitCodeProcess(raw(&process), &raw mut exited) }.is_ok() {
                    code.set(exited).ok();
                }
                let closing = waited.lock().take();
                drop(closing);
                ended.close();
            })?;

        let pty = Pty {
            input,
            console,
            job,
            runs_command: command.is_some(),
            exit_code,
            at_prompt: sync::atomic::AtomicU32::new(u32::MAX),
            #[cfg(test)]
            process_id,
        };
        Ok((pty, output))
    }

    /// Whether the shell has the terminal back at its prompt: a pseudoconsole doesn't say who's
    /// in front, so this is whether the shell is all that runs in its job. Never while it runs a
    /// command, nor for a shell that keeps a helper process (Git Bash's launcher does).
    pub fn shell_in_front(&self) -> bool {
        !self.runs_command && self.job.as_ref().and_then(active_processes) == Some(1)
    }

    /// Whether a program the shell started is running (a build, an editor), so closing the terminal would end
    /// it: more processes run in the job than the fewest seen so far, which is what the shell keeps at its prompt.
    /// Always while the shell runs a command and has not exited.
    pub fn program_running(&self) -> bool {
        if self.exit_code.get().is_some() {
            return false;
        }
        if self.runs_command {
            return true;
        }
        let Some(count) = self.job.as_ref().and_then(active_processes) else {
            return false;
        };
        let at_prompt = self
            .at_prompt
            .fetch_min(count, sync::atomic::Ordering::Relaxed)
            .min(count);
        count > at_prompt
    }

    /// The exit code of the command the shell ran, once it exited with one other than 0. Never
    /// for a shell at a prompt, as the user typed what made it exit.
    pub fn failed(&self) -> Option<i32> {
        let code = *self.exit_code.get()?;
        // Exit codes are a DWORD; NTSTATUS-style codes read as the negative numbers cmd shows.
        (self.runs_command && code != 0).then_some(code.cast_signed())
    }

    /// Sends input to the shell, once it reads what was sent before.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        (self.input.send(bytes.to_vec())).map_err(|_| io::ErrorKind::BrokenPipe.into())
    }

    pub fn resize(&self, size: Size) -> io::Result<()> {
        let console = self.console.lock();
        let console = console.as_ref().ok_or(io::ErrorKind::BrokenPipe)?;
        // SAFETY: the pseudoconsole is open while the lock is held.
        unsafe { ResizePseudoConsole(**console, size.coord()) }?;
        Ok(())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let console = self.console.lock().take();
        let job = self.job.take();
        // Hung up first, as a terminal does, then the shell's whole tree ended with its job,
        // also once the shell exited on its own and left jobs running.
        thread::Builder::new()
            .name("pty closer".into())
            .spawn(move || {
                drop(console);
                drop(job);
            })
            .ok();
    }
}

/// How many processes run in `job`, if it can be asked.
fn active_processes(job: &OwnedHandle) -> Option<u32> {
    let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    // SAFETY: `accounting` is a live local of the size given.
    unsafe {
        QueryInformationJobObject(
            Some(raw(job)),
            JobObjectBasicAccountingInformation,
            (&raw mut accounting).cast(),
            mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            None,
        )
    }
    .ok()?;
    Some(accounting.ActiveProcesses)
}

/// A job whose processes are killed once its last handle closes, the shell's and everything it
/// starts, unless a program breaks away from it on purpose.
fn job() -> io::Result<OwnedHandle> {
    // SAFETY: an unnamed job with default security.
    let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }?;
    // SAFETY: `CreateJobObjectW` succeeded, so it's open and nothing else owns it.
    let job = unsafe { OwnedHandle::from_raw_handle(job.0) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
    // SAFETY: `limits` is a live local of the size given.
    unsafe {
        SetInformationJobObject(
            raw(&job),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    }?;
    Ok(job)
}

fn pipe() -> io::Result<(File, File)> {
    let (mut read, mut write) = (HANDLE(ptr::null_mut()), HANDLE(ptr::null_mut()));
    // SAFETY: both pointers are to live locals.
    unsafe { CreatePipe(&raw mut read, &raw mut write, None, 0) }?;
    // SAFETY: `CreatePipe` succeeded, so both are open handles nothing else owns.
    let (read, write) = unsafe {
        (
            OwnedHandle::from_raw_handle(read.0),
            OwnedHandle::from_raw_handle(write.0),
        )
    };
    Ok((File::from(read), File::from(write)))
}

fn open(size: Size, input: &File, output: &File) -> io::Result<Owned<HPCON>> {
    // SAFETY: both handles are open for the call, and the pseudoconsole keeps its own.
    let console = unsafe { CreatePseudoConsole(size.coord(), raw(input), raw(output), 0) }?;
    // SAFETY: `CreatePseudoConsole` succeeded, so it's open and nothing else owns it.
    Ok(unsafe { Owned::new(console) })
}

/// A process just started.
struct Started {
    process: OwnedHandle,
    #[cfg(test)]
    id: u32,
}

fn start(
    command_line: &mut [u16],
    env: &[(OsString, OsString)],
    cwd: &Path,
    console: &HPCON,
    job: Option<&OwnedHandle>,
) -> io::Result<Started> {
    let cwd = wide(cwd.as_os_str())?;
    let environment = environment(env);
    let mut attributes = Attributes::new(*console, job.map(raw))?;
    let startup = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: mem::size_of::<STARTUPINFOEXW>() as u32,
            // With no handles of its own given, the shell takes the pseudoconsole's rather than
            // this process's.
            dwFlags: STARTF_USESTDHANDLES,
            ..Default::default()
        },
        lpAttributeList: attributes.list(),
    };
    let mut process = PROCESS_INFORMATION::default();
    // SAFETY: each pointer is to a live local; the strings end in a NUL and the environment
    // block in two.
    unsafe {
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            Some(environment.as_ptr().cast()),
            PCWSTR(cwd.as_ptr()),
            (&raw const startup).cast::<STARTUPINFOW>(),
            &raw mut process,
        )
    }?;
    #[cfg(test)]
    let id = process.dwProcessId;
    // SAFETY: `CreateProcessW` succeeded, so both are open handles nothing else owns.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(process.hProcess.0),
            OwnedHandle::from_raw_handle(process.hThread.0),
        )
    };
    drop(thread);
    Ok(Started {
        process,
        #[cfg(test)]
        id,
    })
}

/// `spec`'s command line: the program, quoted as Windows reads a command line's first word (up
/// to the next quote, with no escapes), then its arguments and the command's, each quoted as
/// programs built on the C runtime split them. cmd.exe reads its command otherwise: after
/// `/s /c`, its outer quotes stripped and the rest left as written.
fn command_line(spec: &ShellSpec, run: Option<&str>) -> io::Result<Vec<u16>> {
    if spec
        .program
        .encode_wide()
        .any(|unit| unit == u16::from(b'"'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a program's path can't hold a quote",
        ));
    }
    let mut line = OsString::from("\"");
    line.push(&spec.program);
    line.push("\"");
    for arg in &spec.args {
        line.push(" ");
        line.push(quoted(arg));
    }
    match (run, spec.kind) {
        (None, _) => {}
        (Some(run), ShellKind::Cmd) => line.push(format!(" /s /c \"{run}\"")),
        (Some(run), _) => {
            for arg in spec.command_args(run)? {
                line.push(" ");
                line.push(quoted(&arg));
            }
        }
    }
    wide(&line)
}

/// `arg` as one argument for a program that splits its command line as the C runtime does: as
/// it is when it needs no quotes, else in quotes, with the backslashes before a quote and before
/// the closing one doubled and each quote escaped.
fn quoted(arg: &OsStr) -> OsString {
    let units: Vec<u16> = arg.encode_wide().collect();
    let needs_quotes = units.is_empty()
        || (units.iter()).any(|&unit| matches!(unit, 0x20 | 0x09 | 0x0A | 0x0B | 0x22));
    if !needs_quotes {
        return arg.to_owned();
    }
    let backslash = u16::from(b'\\');
    let quote = u16::from(b'"');
    let mut out = vec![quote];
    let mut backslashes = 0;
    for &unit in &units {
        if unit == backslash {
            backslashes += 1;
            continue;
        }
        let escapes = if unit == quote {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        out.extend(std::iter::repeat_n(backslash, escapes));
        backslashes = 0;
        out.push(unit);
    }
    out.extend(std::iter::repeat_n(backslash, backslashes * 2));
    out.push(quote);
    OsString::from_wide(&out)
}

/// This process's environment as a terminal's, then `extra`, sorted by name without regard to
/// case, as `CreateProcessW` wants it.
fn environment(extra: &[(OsString, OsString)]) -> Vec<u16> {
    let mut variables: BTreeMap<OsString, (OsString, OsString)> = std::env::vars_os()
        .map(|(name, value)| (name.to_ascii_uppercase(), (name, value)))
        .collect();
    variables.remove(OsStr::new("TERMINFO"));
    for (name, value) in [
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        // Ghostty's, the emulator's, as Claude Code reports progress only to terminals it knows.
        ("TERM_PROGRAM", "ghostty"),
        ("TERM_PROGRAM_VERSION", "1.3.2"),
    ] {
        variables.insert(name.into(), (name.into(), value.into()));
    }
    for (name, value) in extra {
        variables.insert(name.to_ascii_uppercase(), (name.clone(), value.clone()));
    }
    let mut block = Vec::new();
    for (name, value) in variables.into_values() {
        block.extend(name.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = text.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a path or argument can't hold a NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

/// A process's attributes: the pseudoconsole it attaches to and the job it starts in, if any,
/// so it never runs outside it.
struct Attributes {
    list: Vec<usize>,
    /// What the job attribute points at, which must outlive the process's creation.
    jobs: Box<[HANDLE; 1]>,
}

impl Attributes {
    fn new(console: HPCON, job: Option<HANDLE>) -> io::Result<Attributes> {
        let count = if job.is_some() { 2 } else { 1 };
        let mut size = 0;
        // SAFETY: with no list, it only writes the size one needs to `size`.
        unsafe { InitializeProcThreadAttributeList(None, count, None, &raw mut size) }.ok();
        let mut list = vec![0; size.div_ceil(mem::size_of::<usize>())];
        // SAFETY: `list` holds at least `size` bytes, aligned as a pointer.
        unsafe {
            InitializeProcThreadAttributeList(
                Some(LPPROC_THREAD_ATTRIBUTE_LIST(list.as_mut_ptr().cast())),
                count,
                None,
                &raw mut size,
            )
        }?;
        // Initialized, so deleted when dropped from here on.
        let mut attributes = Attributes {
            list,
            jobs: Box::new([job.unwrap_or_default()]),
        };
        // SAFETY: the list has room for this attribute, whose value is the pseudoconsole's
        // handle itself rather than a pointer to it.
        unsafe {
            UpdateProcThreadAttribute(
                attributes.list(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                Some(ptr::without_provenance(console.0.cast_unsigned())),
                mem::size_of::<HPCON>(),
                None,
                None,
            )
        }?;
        if job.is_some() {
            // SAFETY: the list has room for this one too; its value points at `jobs`, which lives
            // as long as the list.
            unsafe {
                UpdateProcThreadAttribute(
                    attributes.list(),
                    0,
                    PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                    Some(attributes.jobs.as_ptr().cast()),
                    mem::size_of::<[HANDLE; 1]>(),
                    None,
                    None,
                )
            }?;
        }
        Ok(attributes)
    }

    fn list(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.list.as_mut_ptr().cast())
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: the list was initialized, and is deleted only here.
        unsafe { DeleteProcThreadAttributeList(self.list()) };
    }
}

/// Reads until the pseudoconsole closes, even once no one's listening, as closing it can wait
/// until what the shell wrote last is read.
fn drain(mut output: File, queue: Queue) {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        while queue.full() {
            thread::sleep(Duration::from_millis(PAUSE_MS.into()));
        }
        match output.read(&mut buffer) {
            Ok(0) => return,
            Ok(n) => {
                let read = buffer.get(..n).unwrap_or_default().to_vec();
                queue.send(read).ok();
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

fn raw(handle: &impl AsRawHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::JobObjects::JobObjectBasicProcessIdList;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};

    use super::*;

    const SIZE: Size = Size {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    };

    fn quoted_text(arg: &str) -> String {
        quoted(OsStr::new(arg)).to_string_lossy().into_owned()
    }

    #[test]
    fn arguments_are_quoted_as_the_c_runtime_splits_them() {
        assert_eq!(
            quoted_text("-NoLogo"),
            "-NoLogo",
            "a plain word stays as it is"
        );
        assert_eq!(
            quoted_text(""),
            r#""""#,
            "an empty argument is a pair of quotes"
        );
        assert_eq!(
            quoted_text(r"C:\Program Files\x"),
            r#""C:\Program Files\x""#,
            "blanks need quotes; backslashes before letters stay single"
        );
        assert_eq!(
            quoted_text(r#"say "hi""#),
            r#""say \"hi\"""#,
            "inner quotes are escaped"
        );
        assert_eq!(
            quoted_text(r"dir with space\"),
            r#""dir with space\\""#,
            "a trailing backslash is doubled so it doesn't escape the closing quote"
        );
    }

    #[test]
    fn the_command_line_quotes_the_program_and_takes_the_command_per_kind() {
        let line = |spec: &ShellSpec, run: Option<&str>| {
            let wide = command_line(spec, run).expect("a valid command line");
            String::from_utf16_lossy(wide.split_last().expect("NUL-terminated").1)
        };
        let cmd = ShellSpec {
            program: r"C:\Windows\System32\cmd.exe".into(),
            args: Vec::new(),
            kind: ShellKind::Cmd,
            env: Vec::new(),
        };
        assert_eq!(
            line(&cmd, Some(r#"echo "a" & exit"#)),
            r#""C:\Windows\System32\cmd.exe" /s /c "echo "a" & exit""#,
            "cmd.exe gets the command as written after /s /c"
        );
        let bash = ShellSpec {
            program: r"C:\Program Files\Git\bin\bash.exe".into(),
            args: vec!["--login".into(), "-i".into()],
            kind: ShellKind::Posix,
            env: Vec::new(),
        };
        assert_eq!(
            line(&bash, None),
            r#""C:\Program Files\Git\bin\bash.exe" --login -i"#,
            "a shell at its prompt gets only its own arguments"
        );
        assert_eq!(
            line(&bash, Some("echo 'x y'")),
            r#""C:\Program Files\Git\bin\bash.exe" --login -i -i -c "echo 'x y'""#,
            "a POSIX shell gets the command after -i -c, as one argument"
        );
    }

    /// The ids of the processes in `pty`'s job.
    fn job_processes(pty: &Pty) -> Vec<u32> {
        #[repr(C)]
        struct List {
            assigned: u32,
            listed: u32,
            ids: [usize; 64],
        }
        let job = pty.job.as_ref().expect("the shell runs in a job");
        let mut list = List {
            assigned: 0,
            listed: 0,
            ids: [0; 64],
        };
        // SAFETY: `list` is a live local laid out as JOBOBJECT_BASIC_PROCESS_ID_LIST with room for
        // 64 ids, the size given.
        unsafe {
            QueryInformationJobObject(
                Some(raw(job)),
                JobObjectBasicProcessIdList,
                (&raw mut list).cast(),
                mem::size_of::<List>() as u32,
                None,
            )
        }
        .expect("the job can be asked for its processes");
        (list.ids.iter().take(list.listed as usize))
            .map(|&id| id as u32)
            .collect()
    }

    /// Whether the process `id` ends within 10 seconds (or is gone already).
    fn ends(id: u32) -> bool {
        // SAFETY: a plain query for a process this test started; the handle is closed below.
        let Ok(process) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, id) }) else {
            return true;
        };
        let started = Instant::now();
        let mut ended = false;
        while started.elapsed() < Duration::from_secs(10) {
            // SAFETY: `process` is open until closed below.
            if unsafe { WaitForSingleObject(process, 100) } == WAIT_OBJECT_0 {
                ended = true;
                break;
            }
        }
        // SAFETY: opened above and closed only here.
        unsafe { CloseHandle(process) }.ok();
        ended
    }

    #[test]
    fn dropping_the_pty_ends_the_shell_and_what_it_runs() {
        let (pty, _output) = Pty::spawn(&std::env::temp_dir(), SIZE, None).expect("cmd.exe starts");
        assert!(
            job_processes(&pty).contains(&pty.process_id),
            "the shell starts in its job"
        );
        assert!(
            pty.shell_in_front(),
            "at its prompt the shell is all that runs"
        );
        assert!(
            !pty.program_running(),
            "at its prompt no program of the shell runs"
        );
        pty.write(b"ping -n 120 127.0.0.1\r")
            .expect("the shell takes input");
        let started = Instant::now();
        let running = loop {
            let running = job_processes(&pty);
            if running.len() >= 2 {
                break running;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "the command the shell runs should join its job"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(
            !pty.shell_in_front(),
            "a command the shell runs is in front"
        );
        assert!(
            pty.program_running(),
            "a plain long-running program counts as running, though it reports nothing"
        );
        drop(pty);
        for id in running {
            assert!(
                ends(id),
                "letting go of the pty ends the shell and the command it runs, as closing a \
                 terminal does"
            );
        }
    }
}
