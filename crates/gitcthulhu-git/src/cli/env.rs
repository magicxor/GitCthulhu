//! Environment and global arguments of every child `git` (§1.7).
//!
//! The base is the runtime's captured process environment (plus the login shell's `PATH` on macOS/Linux), minus
//! everything that would make the child operate on another repository or pick up foreign tooling. On top come the
//! fixed variables (no prompts, English messages, no editor, no pager), the test isolation, and the per-kind
//! additions. Askpass, the transport defaults of the harness and `GitCommand::extra_env` are added by the caller, in
//! that order, after this list.

use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

use super::{CommandKind, EditorMode, GitCommand};
use crate::GitRuntime;

/// Removed: they would make the child target another repository (a GUI started from a hook or a terminal).
const REPOSITORY_TARGETING: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// Removed: foreign askpass programs, `-c` overrides, pathspec modes and trace targets inherited from a VS Code
/// terminal or a hook. `GIT_TRACE2_PARENT_SID` would make our top-level git look like a child in trace2 (§1.11).
/// `GIT_CONFIG_GLOBAL/SYSTEM/NOSYSTEM` are kept (user intent; tests set them).
const INHERITED_TOOLING: &[&str] = &[
    "GIT_TRACE2_EVENT",
    "GIT_TRACE2_PARENT_SID",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "SSH_ASKPASS_REQUIRE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_EXEC_PATH",
    "GIT_REFLOG_ACTION",
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// Removed by prefix.
const INHERITED_TOOLING_PREFIXES: &[&str] =
    &["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_", "VSCODE_GIT_"];

/// Whether an inherited variable must not reach a child git. Windows variable names are case-insensitive.
fn is_removed(key: &OsStr) -> bool {
    let Some(key) = key.to_str() else {
        return false;
    };
    let key = if cfg!(windows) {
        std::borrow::Cow::Owned(key.to_ascii_uppercase())
    } else {
        std::borrow::Cow::Borrowed(key)
    };
    REPOSITORY_TARGETING.contains(&key.as_ref())
        || INHERITED_TOOLING.contains(&key.as_ref())
        || INHERITED_TOOLING_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
}

/// The environment of `cmd`'s child, without askpass, transport defaults and `extra_env` (see the module docs).
/// `trace_dir` is the `GIT_TRACE2_EVENT` directory if hooks are traced.
pub(crate) fn child_env(
    rt: &GitRuntime,
    cmd: &GitCommand,
    trace_dir: Option<&Path>,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = rt
        .base_env()
        .iter()
        .filter(|(key, _)| !is_removed(key))
        .cloned()
        .collect();
    let mut set = |key: &str, value: OsString| env.push((key.into(), value));

    set("GIT_TERMINAL_PROMPT", "0".into());
    set("LANGUAGE", "en".into());
    // `LC_ALL=C` is deliberately not forced: hooks keep the user's UTF-8 `LC_CTYPE`.
    set("LC_MESSAGES", "C".into());
    set("GIT_MERGE_AUTOEDIT", "no".into());
    set("GIT_PAGER", "cat".into());
    match &cmd.editor {
        EditorMode::Noop => {
            set("GIT_EDITOR", ":".into());
            set("GIT_SEQUENCE_EDITOR", ":".into());
        }
        EditorMode::RebaseHelper { plan_dir } => {
            let helper = sh_quote(rt.editor_exe());
            let plan_dir = sh_quote(plan_dir);
            set(
                "GIT_SEQUENCE_EDITOR",
                join([&helper, OsStr::new("sequence"), &plan_dir]),
            );
            set(
                "GIT_EDITOR",
                join([&helper, OsStr::new("message"), &plan_dir]),
            );
        }
    }
    if let Some(isolation) = &rt.config().test_isolation {
        set("GIT_CONFIG_NOSYSTEM", "1".into());
        set("GIT_CONFIG_GLOBAL", isolation.global_config.clone().into());
        set("HOME", isolation.home.clone().into());
        set("USERPROFILE", isolation.home.clone().into());
        set("XDG_CONFIG_HOME", isolation.home.join(".config").into());
    }
    if cmd.kind == CommandKind::Read {
        // No opportunistic index refresh by a read.
        set("GIT_OPTIONAL_LOCKS", "0".into());
    }
    if let Some(dir) = trace_dir {
        set("GIT_TRACE2_EVENT", dir.as_os_str().to_owned());
    }
    env
}

/// The arguments before the subcommand: `--no-pager`, raw UTF-8 paths, no colours, and for Mutation/Network commands
/// the GUI's network settings (`RuntimeConfig::network_config`).
pub(crate) fn global_args(rt: &GitRuntime, kind: CommandKind) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--no-pager",
        "-c",
        "core.quotepath=false",
        "-c",
        "color.ui=false",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    if kind != CommandKind::Read {
        for (key, value) in &rt.config().network_config {
            args.push("-c".into());
            args.push(format!("{key}={value}").into());
        }
    }
    args
}

/// `path` as a single-quoted `sh` word, with forward slashes on Windows; `'` becomes `'\''`.
pub(crate) fn sh_quote(path: &Path) -> OsString {
    let bytes = match gix::path::into_bstr(path) {
        Ok(bytes) => bytes.into_owned(),
        Err(_) => path.to_string_lossy().into_owned().into(),
    };
    let bytes = gix::path::to_unix_separators_on_windows(bytes);
    let mut quoted = Vec::with_capacity(bytes.len() + 2);
    quoted.push(b'\'');
    for &b in bytes.iter() {
        if b == b'\'' {
            quoted.extend_from_slice(b"'\\''");
        } else {
            quoted.push(b);
        }
    }
    quoted.push(b'\'');
    match gix::path::from_byte_slice(&quoted) {
        Ok(path) => path.as_os_str().to_owned(),
        Err(_) => String::from_utf8_lossy(&quoted).into_owned().into(),
    }
}

/// Join words with single spaces.
fn join<'a>(words: impl IntoIterator<Item = &'a OsStr>) -> OsString {
    let mut out = OsString::new();
    for (idx, word) in words.into_iter().enumerate() {
        if idx > 0 {
            out.push(" ");
        }
        out.push(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_escapes_single_quotes() {
        assert_eq!(
            sh_quote(Path::new("/tmp/it's here")),
            OsString::from(r"'/tmp/it'\''s here'"),
            "a quote ends the word, is escaped and the word resumes"
        );
    }

    #[cfg(windows)]
    #[test]
    fn quoting_uses_forward_slashes_on_windows() {
        assert_eq!(
            sh_quote(Path::new(r"C:\Program Files\app\gitcthulhu-editor.exe")),
            OsString::from("'C:/Program Files/app/gitcthulhu-editor.exe'"),
            "sh would eat backslashes"
        );
    }

    #[test]
    fn foreign_tooling_and_repository_targeting_is_removed() {
        for key in [
            "GIT_DIR",
            "GIT_INDEX_FILE",
            "GIT_ASKPASS",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_KEY_0",
            "VSCODE_GIT_IPC_HANDLE",
            "GIT_LITERAL_PATHSPECS",
            "GIT_TRACE2_PARENT_SID",
        ] {
            assert!(is_removed(OsStr::new(key)), "{key} must not reach git");
        }
        for key in [
            "PATH",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_SSH_COMMAND",
            "HOME",
        ] {
            assert!(!is_removed(OsStr::new(key)), "{key} is kept");
        }
        if cfg!(windows) {
            assert!(
                is_removed(OsStr::new("Git_Dir")),
                "Windows variable names are case-insensitive"
            );
        }
    }
}
