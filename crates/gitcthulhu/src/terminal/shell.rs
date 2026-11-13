//! Which shell a terminal starts (18 §5.5): the shells found on this machine, the default order, the setting
//! (Settings > Terminal > Shell) and quoting dropped paths for the shell.
//!
//! Windows: PowerShell 7 (`pwsh.exe` on `PATH`, else `%ProgramFiles%\PowerShell\7`), Windows PowerShell, Command
//! Prompt (`%COMSPEC%`), Git Bash (next to the `git.exe` on `PATH`, else `%ProgramFiles%\Git`) and WSL; "Automatic"
//! takes the first of the first three. Elsewhere: the login shell (`$SHELL`). A custom command line works everywhere.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use but_tty::{ShellKind, ShellSpec};

use crate::settings::TerminalShell;

/// A shell found on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedShell {
    /// The setting that picks it.
    pub choice: TerminalShell,
    pub spec: ShellSpec,
}

/// What detection looks at; [`Probe::system`] reads this process's environment and the file system.
pub struct Probe<'a> {
    /// `PATH`.
    pub path: Option<OsString>,
    /// `%ProgramFiles%`.
    pub program_files: Option<PathBuf>,
    /// `%SystemRoot%`.
    pub system_root: Option<PathBuf>,
    /// `%COMSPEC%`.
    pub comspec: Option<PathBuf>,
    /// `$SHELL`.
    pub login_shell: Option<OsString>,
    /// Whether a file exists.
    pub exists: &'a dyn Fn(&Path) -> bool,
}

impl Probe<'static> {
    /// This machine.
    pub fn system() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        Probe {
            path: var("PATH"),
            program_files: var("ProgramFiles").map(PathBuf::from),
            system_root: var("SystemRoot").map(PathBuf::from),
            comspec: var("COMSPEC").map(PathBuf::from),
            login_shell: var("SHELL"),
            exists: &|path: &Path| path.is_file(),
        }
    }
}

impl Probe<'_> {
    /// The first `name` in a `PATH` directory.
    fn on_path(&self, name: &str) -> Option<PathBuf> {
        let path = self.path.as_ref()?;
        std::env::split_paths(path)
            .map(|dir| dir.join(name))
            .find(|candidate| (self.exists)(candidate))
    }

    fn existing(&self, path: PathBuf) -> Option<PathBuf> {
        (self.exists)(&path).then_some(path)
    }
}

/// The shells `probe` finds, in the order the pickers list them.
pub fn detect(probe: &Probe<'_>) -> Vec<DetectedShell> {
    if cfg!(windows) {
        detect_windows(probe)
    } else {
        detect_unix(probe)
    }
}

/// [`detect`] on this machine, once per run (the pickers ask on every render).
pub fn detected() -> &'static [DetectedShell] {
    static DETECTED: OnceLock<Vec<DetectedShell>> = OnceLock::new();
    DETECTED.get_or_init(|| detect(&Probe::system()))
}

fn detect_windows(probe: &Probe<'_>) -> Vec<DetectedShell> {
    let system32 = probe.system_root.as_ref().map(|root| root.join("System32"));
    let mut shells = Vec::new();
    let pwsh = probe.on_path("pwsh.exe").or_else(|| {
        let path = probe.program_files.as_ref()?.join(r"PowerShell\7\pwsh.exe");
        probe.existing(path)
    });
    if let Some(program) = pwsh {
        shells.push(DetectedShell {
            choice: TerminalShell::Pwsh,
            spec: spec(program, &["-NoLogo"], ShellKind::PowerShell),
        });
    }
    let powershell = system32
        .as_ref()
        .and_then(|dir| probe.existing(dir.join(r"WindowsPowerShell\v1.0\powershell.exe")))
        .or_else(|| probe.on_path("powershell.exe"));
    if let Some(program) = powershell {
        shells.push(DetectedShell {
            choice: TerminalShell::WindowsPowerShell,
            spec: spec(program, &["-NoLogo"], ShellKind::PowerShell),
        });
    }
    let cmd = probe
        .comspec
        .clone()
        .and_then(|path| probe.existing(path))
        .or_else(|| {
            system32
                .as_ref()
                .and_then(|dir| probe.existing(dir.join("cmd.exe")))
        });
    if let Some(program) = cmd {
        shells.push(DetectedShell {
            choice: TerminalShell::Cmd,
            spec: spec(program, &[], ShellKind::Cmd),
        });
    }
    if let Some(program) = git_bash(probe) {
        let mut bash = spec(program, &["--login", "-i"], ShellKind::Posix);
        bash.env = git_bash_env();
        shells.push(DetectedShell {
            choice: TerminalShell::GitBash,
            spec: bash,
        });
    }
    if let Some(program) = system32.and_then(|dir| probe.existing(dir.join("wsl.exe"))) {
        shells.push(DetectedShell {
            choice: TerminalShell::Wsl,
            spec: spec(program, &[], ShellKind::Other),
        });
    }
    shells
}

/// Git for Windows' `bin\bash.exe`: next to the `git.exe` on `PATH` (in `cmd`, `bin` or `mingw64\bin` of the
/// installation), else in `%ProgramFiles%\Git`. Never `System32\bash.exe`, which is WSL's.
fn git_bash(probe: &Probe<'_>) -> Option<PathBuf> {
    let from_git = probe.on_path("git.exe").and_then(|git| {
        // `<install>\cmd\git.exe`, `<install>\bin\git.exe` or `<install>\mingw64\bin\git.exe`.
        git.ancestors()
            .skip(2)
            .take(2)
            .map(|install| install.join("bin").join("bash.exe"))
            .find(|candidate| {
                !candidate
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .contains(r"\system32\")
                    && (probe.exists)(candidate)
            })
    });
    from_git.or_else(|| {
        let path = probe.program_files.as_ref()?.join(r"Git\bin\bash.exe");
        probe.existing(path)
    })
}

/// Git Bash's `--login` changes to `$HOME` unless told to stay where it starts.
fn git_bash_env() -> Vec<(OsString, OsString)> {
    vec![("CHERE_INVOKING".into(), "1".into())]
}

fn detect_unix(probe: &Probe<'_>) -> Vec<DetectedShell> {
    let program = probe.login_shell.clone().unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            "/bin/zsh"
        } else {
            "/bin/sh"
        }
        .into()
    });
    vec![DetectedShell {
        choice: TerminalShell::Auto,
        spec: ShellSpec {
            program,
            args: vec!["-l".into()],
            kind: ShellKind::Posix,
            env: Vec::new(),
        },
    }]
}

fn spec(program: PathBuf, args: &[&str], kind: ShellKind) -> ShellSpec {
    ShellSpec {
        program: program.into_os_string(),
        args: args.iter().map(OsString::from).collect(),
        kind,
        env: Vec::new(),
    }
}

/// The shell "Automatic" starts: PowerShell 7, else Windows PowerShell, else Command Prompt on Windows; the login
/// shell elsewhere; but-tty's default when nothing was found.
pub fn automatic(detected: &[DetectedShell]) -> ShellSpec {
    let preferred = [
        TerminalShell::Pwsh,
        TerminalShell::WindowsPowerShell,
        TerminalShell::Cmd,
        TerminalShell::Auto,
    ];
    preferred
        .iter()
        .find_map(|choice| detected.iter().find(|shell| shell.choice == *choice))
        .map(|shell| shell.spec.clone())
        .unwrap_or_else(ShellSpec::default_shell)
}

/// The shell for `choice`: a detected one, the automatic one, or a custom command line. An error names a shell
/// that was not found or a custom command line that names no program.
pub fn resolve(choice: &TerminalShell, detected: &[DetectedShell]) -> Result<ShellSpec, String> {
    match choice {
        TerminalShell::Auto => Ok(automatic(detected)),
        TerminalShell::Custom(line) => custom(line),
        other => detected
            .iter()
            .find(|shell| shell.choice == *other)
            .map(|shell| shell.spec.clone())
            .ok_or_else(|| format!("{} was not found on this computer", label(other))),
    }
}

/// A custom command line as a shell: the first word is the program, the rest its arguments (Windows splits as the C
/// runtime does, elsewhere as a POSIX shell does). Its kind follows the program's name, so commands and Git Bash's
/// `CHERE_INVOKING` work for the usual shells.
pub fn custom(line: &str) -> Result<ShellSpec, String> {
    let words = if cfg!(windows) {
        split_windows(line)
    } else {
        shell_words::split(line)
            .map_err(|err| format!("The custom shell command is invalid: {err}"))?
    };
    let mut words = words.into_iter();
    let program = words
        .next()
        .filter(|program| !program.trim().is_empty())
        .ok_or_else(|| "The custom shell command is empty".to_owned())?;
    let name = Path::new(&program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let kind = match name.as_str() {
        "cmd" => ShellKind::Cmd,
        "pwsh" | "powershell" => ShellKind::PowerShell,
        "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" => ShellKind::Posix,
        _ => ShellKind::Other,
    };
    let env = if cfg!(windows) && name == "bash" {
        git_bash_env()
    } else {
        Vec::new()
    };
    Ok(ShellSpec {
        program: program.into(),
        args: words.map(OsString::from).collect(),
        kind,
        env,
    })
}

/// `line` split into words as programs built on the C runtime split their command line: blanks separate words,
/// quotes group, `\"` is a quote, and backslashes are literal unless they precede a quote.
pub fn split_windows(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let mut backslashes = 1;
                while chars.next_if_eq(&'\\').is_some() {
                    backslashes += 1;
                }
                in_word = true;
                if chars.peek() == Some(&'"') {
                    word.extend(std::iter::repeat_n('\\', backslashes / 2));
                    if backslashes % 2 == 1 {
                        chars.next();
                        word.push('"');
                    }
                } else {
                    word.extend(std::iter::repeat_n('\\', backslashes));
                }
            }
            '"' => {
                in_word = true;
                if quoted && chars.peek() == Some(&'"') {
                    chars.next();
                    word.push('"');
                } else {
                    quoted = !quoted;
                }
            }
            ' ' | '\t' if !quoted => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// The name of `choice` in the pickers (21 §5.4).
pub fn label(choice: &TerminalShell) -> &'static str {
    match choice {
        TerminalShell::Auto => "Automatic",
        TerminalShell::Pwsh => "PowerShell 7",
        TerminalShell::WindowsPowerShell => "Windows PowerShell",
        TerminalShell::Cmd => "Command Prompt",
        TerminalShell::GitBash => "Git Bash",
        TerminalShell::Wsl => "WSL",
        TerminalShell::Custom(_) => "Custom",
    }
}

/// `words` side by side, each quoted where it needs to be for a shell of `kind`, as dropped paths are pasted.
pub fn quoted(kind: ShellKind, words: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    let quote = |word: &str| -> String {
        match kind {
            ShellKind::Cmd => {
                let plain = !word.is_empty()
                    && !word
                        .chars()
                        .any(|c| c.is_whitespace() || "&|<>^()%!\"".contains(c));
                if plain {
                    word.to_owned()
                } else {
                    format!("\"{}\"", word.replace('"', "\"\""))
                }
            }
            ShellKind::PowerShell => {
                let plain = !word.is_empty()
                    && word
                        .chars()
                        .all(|c| c.is_alphanumeric() || "\\/:._-".contains(c));
                if plain {
                    word.to_owned()
                } else {
                    // PowerShell ends a single-quoted string at any of its single quotes, the typographic ones
                    // included; each is escaped by doubling it.
                    let mut quoted = String::with_capacity(word.len() + 2);
                    quoted.push('\'');
                    for c in word.chars() {
                        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
                            quoted.push(c);
                        }
                        quoted.push(c);
                    }
                    quoted.push('\'');
                    quoted
                }
            }
            ShellKind::Posix | ShellKind::Other => shell_words::quote(word).into_owned(),
        }
    };
    words
        .into_iter()
        .map(|word| quote(word.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}
