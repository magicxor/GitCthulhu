//! The rebase editor helper program (WP-B7, spec §5.4).
//!
//! Git starts it as `GIT_SEQUENCE_EDITOR` and as `GIT_EDITOR` during an interactive rebase that this application
//! started, with `<mode> <plan_dir> <file>` as its arguments:
//!
//! - `sequence`: swap the todo list Git generated for the plan's, after checking that Git would rebase exactly the
//!   commits the plan was made for;
//! - `message`: write the message the user entered in the GUI for the step Git is currently executing, if any.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitCode,
};

pub(super) fn run() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let [mode, plan_dir, file, ..] = args.as_slice() else {
        eprintln!("usage: <editor> sequence|message <plan_dir> <file>");
        return ExitCode::FAILURE;
    };
    let (plan_dir, file) = (Path::new(plan_dir), Path::new(file));
    let result = match mode.to_str() {
        Some("sequence") => sequence(plan_dir, file),
        Some("message") => message(plan_dir, file),
        _ => Err("unknown mode".to_owned()),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("gitcthulhu editor helper: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Commands whose second token is a commit (`fixup` may carry `-C`/`-c` first), long and abbreviated form.
const COMMIT_COMMANDS: &[&str] = &[
    "pick", "p", "reword", "r", "edit", "e", "squash", "s", "fixup", "f", "drop", "d",
];

/// The (abbreviated) commit id a todo or `done` line refers to, if it is a commit command.
fn commit_of_line(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut tokens = line.split_whitespace();
    let command = tokens.next()?;
    if !COMMIT_COMMANDS.contains(&command) {
        return None;
    }
    let mut next = tokens.next()?;
    if matches!(next, "-C" | "-c" | "-cC" | "-Cc") {
        next = tokens.next()?;
    }
    (!next.is_empty() && next.bytes().all(|b| b.is_ascii_hexdigit())).then_some(next)
}

/// The ids of all commit lines of a todo list.
fn todo_commits(todo: &str) -> Vec<&str> {
    todo.lines().filter_map(commit_of_line).collect()
}

/// Whether `generated` (abbreviated ids, as Git wrote them) and `expected` (full ids) name the same set of
/// commits: every expected id has exactly one generated id that is its prefix, and the counts are equal.
fn same_commit_set(expected: &[&str], generated: &[&str]) -> bool {
    expected.len() == generated.len()
        && expected.iter().all(|full| {
            generated
                .iter()
                .filter(|short| {
                    full.to_ascii_lowercase()
                        .starts_with(&short.to_ascii_lowercase())
                })
                .count()
                == 1
        })
        && generated.iter().all(|short| {
            expected
                .iter()
                .filter(|full| {
                    full.to_ascii_lowercase()
                        .starts_with(&short.to_ascii_lowercase())
                })
                .count()
                == 1
        })
}

fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|err| format!("cannot read '{}': {err}", path.display()))
}

fn sequence(plan_dir: &Path, todo_path: &Path) -> Result<ExitCode, String> {
    let generated_todo = read_text(todo_path)?;
    let expected_text = read_text(&plan_dir.join("expected"))?;
    let expected: Vec<&str> = expected_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let generated = todo_commits(&generated_todo);
    if !same_commit_set(&expected, &generated) {
        std::fs::write(plan_dir.join("mismatch"), generated_todo.as_bytes())
            .map_err(|err| format!("cannot write the mismatch marker: {err}"))?;
        return Ok(ExitCode::FAILURE);
    }
    std::fs::copy(plan_dir.join("todo"), todo_path)
        .map_err(|err| format!("cannot replace the todo list: {err}"))?;
    Ok(ExitCode::SUCCESS)
}

/// The message file for the step Git is executing, found through the last line of `rebase-merge/done`.
///
/// Only that line counts: if it is not a commit command (e.g. `merge -c` of a terminal-started `--rebase-merges`),
/// an earlier step's message must not be served.
fn message_file(plan_dir: &Path) -> Option<PathBuf> {
    let git_dir = plan_dir.parent()?.parent()?;
    let done = std::fs::read(git_dir.join("rebase-merge").join("done")).ok()?;
    let done = String::from_utf8_lossy(&done);
    let last = done
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))?;
    let sha = commit_of_line(last)?.to_ascii_lowercase();
    let entries = std::fs::read_dir(plan_dir.join("messages")).ok()?;
    entries
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with(&sha)
        })
        .map(|entry| entry.path())
}

fn message(plan_dir: &Path, msg_path: &Path) -> Result<ExitCode, String> {
    // Without a prepared message Git's own one stays in place.
    if let Some(source) = message_file(plan_dir) {
        let text = std::fs::read(&source)
            .map_err(|err| format!("cannot read '{}': {err}", source.display()))?;
        std::fs::write(msg_path, text)
            .map_err(|err| format!("cannot write '{}': {err}", msg_path.display()))?;
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn editor_helper_sequence_prefix_matching() {
        let todo =
            "pick aaaaaaa first\np bbbbbbb second\n\n# comment\nexec echo hi\nfixup -c bbbbbbb x\n";
        assert_eq!(
            todo_commits(todo),
            ["aaaaaaa", "bbbbbbb", "bbbbbbb"],
            "commit lines in either spelling, flags skipped, comments and exec ignored"
        );
        assert!(
            same_commit_set(&[A, B], &["aaaaaaa", "bbbbbbb"]),
            "abbreviations match their full ids"
        );
        assert!(
            !same_commit_set(&[A, B], &["aaaaaaa"]),
            "a missing commit is a mismatch"
        );
        assert!(
            !same_commit_set(&[A], &["aaaaaaa", "bbbbbbb"]),
            "an extra commit is a mismatch"
        );
        assert!(
            !same_commit_set(&[A, B], &["aaaaaaa", "ccccccc"]),
            "a different commit is a mismatch"
        );
        assert!(
            !same_commit_set(&[A, A], &["aaaaaaa", "aaaaaaa"]),
            "an ambiguous prefix is not exactly one match"
        );
    }

    #[test]
    fn editor_helper_message_lookup() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let git_dir = tmp.path();
        let plan_dir = git_dir.join("gitcthulhu").join("rebase");
        std::fs::create_dir_all(plan_dir.join("messages")).expect("plan dir");
        std::fs::create_dir_all(git_dir.join("rebase-merge")).expect("rebase dir");
        std::fs::write(plan_dir.join("messages").join(B), "new message\n").expect("message");

        assert!(
            message_file(&plan_dir).is_none(),
            "no `done` file: not in a rebase"
        );
        std::fs::write(
            git_dir.join("rebase-merge").join("done"),
            "pick aaaaaaa one\nreword bbbbbbb two\n",
        )
        .expect("done");
        let found = message_file(&plan_dir).expect("the reword step has a message");
        assert_eq!(
            found.file_name().and_then(|n| n.to_str()),
            Some(B),
            "the last line of `done` selects the file by prefix"
        );
        std::fs::write(
            git_dir.join("rebase-merge").join("done"),
            "pick aaaaaaa one\n",
        )
        .expect("done");
        assert!(
            message_file(&plan_dir).is_none(),
            "a step without a prepared message keeps Git's default"
        );
        std::fs::write(
            git_dir.join("rebase-merge").join("done"),
            "reword bbbbbbb two\nmerge -c ccccccc topic # Merge topic\n",
        )
        .expect("done");
        assert!(
            message_file(&plan_dir).is_none(),
            "only the last done line selects a message, never an earlier step's"
        );
    }
}
