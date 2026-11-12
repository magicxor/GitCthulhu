//! Ending the sequencer operation an undo reverses (spec 17 §1.4 R5, §3.3 C9): `git <op> --quit`, which keeps
//! HEAD, index and worktree as they are and saves an autostash into the stash list instead of dropping it.

use std::path::Path;

use bstr::ByteSlice as _;

use crate::{
    Oplog,
    fail_point::{self, FailPoint},
    types::{InProgress, InProgressFamily, RestoreWarning},
};

/// The files of a merge, cherry-pick or revert in progress that the fallback deletes when `git <op> --quit`
/// failed (`MERGE_AUTOSTASH` only after it was stored in the stash list).
const SEQUENCER_FILES: [&str; 6] = [
    "MERGE_HEAD",
    "MERGE_MSG",
    "MERGE_MODE",
    "AUTO_MERGE",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
];

/// Step C9: end the in-progress operation `kind` with `git <op> --quit` and return the warnings to show.
///
/// If git fails, a merge, cherry-pick or revert is cleaned up by deleting its state files (its autostash is first
/// stored with `git stash store`, never dropped); a rebase or `am` is left in place because its state may hold
/// an autostash, and the warning tells the user to run the quit command.
pub(super) fn quit(oplog: &Oplog, repo: &gix::Repository, kind: InProgress) -> Vec<RestoreWarning> {
    let mut warnings = Vec::new();
    let Some(workdir) = repo.workdir() else {
        return warnings;
    };
    let git_dir = repo.git_dir();
    let args: &[&str] = match kind.family() {
        InProgressFamily::Merge => &["merge", "--quit"],
        InProgressFamily::Rebase => &["rebase", "--quit"],
        InProgressFamily::CherryPick => &["cherry-pick", "--quit"],
        InProgressFamily::Revert => &["revert", "--quit"],
        InProgressFamily::Am => &["am", "--quit"],
        InProgressFamily::Bisect => {
            // The app never starts a bisect, so this is not reached by recorded operations; ending one would move
            // HEAD, which a restore must not do behind the user's back.
            warnings.push(RestoreWarning::QuitFailed {
                kind,
                stderr: "a bisect in progress is not ended by undo; run 'git bisect reset'".into(),
            });
            return warnings;
        }
    };
    let had_autostash = [
        "MERGE_AUTOSTASH",
        "rebase-merge/autostash",
        "rebase-apply/autostash",
    ]
    .iter()
    .any(|p| git_dir.join(p).exists());
    let stashes_before = stash_count(repo);

    let result = fail_point::hit(repo, FailPoint::QuitFails)
        .and_then(|()| oplog.options().git.run(workdir, args));
    if let Err(err) = result {
        let mut stderr = match err {
            crate::OplogError::GitCommand { stderr, .. } => stderr,
            other => other.to_string(),
        };
        match kind.family() {
            InProgressFamily::Merge | InProgressFamily::CherryPick | InProgressFamily::Revert => {
                if let Err(message) = remove_merge_state(oplog, repo, workdir, git_dir) {
                    stderr.push('\n');
                    stderr.push_str(&message);
                }
            }
            InProgressFamily::Rebase | InProgressFamily::Am | InProgressFamily::Bisect => {
                stderr.push_str(&format!("\nRun 'git {}' to end it.", args.join(" ")));
            }
        }
        warnings.push(RestoreWarning::QuitFailed { kind, stderr });
    }
    if had_autostash && stash_count(repo) > stashes_before {
        warnings.push(RestoreWarning::AutostashSaved { kind });
    }
    warnings
}

/// The fallback cleanup of a merge, cherry-pick or revert: store `MERGE_AUTOSTASH` in the stash list, then delete
/// the state files. Returns a message for the warning if something could not be done.
fn remove_merge_state(
    oplog: &Oplog,
    repo: &gix::Repository,
    workdir: &Path,
    git_dir: &Path,
) -> Result<(), String> {
    let mut problems = Vec::new();
    let autostash = git_dir.join("MERGE_AUTOSTASH");
    let mut remove_autostash = false;
    match std::fs::read(&autostash) {
        Ok(content) => {
            let oid = content.trim().to_str_lossy().into_owned();
            match oplog
                .options()
                .git
                .run(workdir, &["stash", "store", "-m", "autostash", &oid])
            {
                Ok(_) => remove_autostash = true,
                Err(err) => problems.push(format!(
                    "the autostash {oid} could not be saved to the stash list ({err}); it was kept in MERGE_AUTOSTASH"
                )),
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => problems.push(format!("reading '{}' failed: {err}", autostash.display())),
    }
    let mut files: Vec<std::path::PathBuf> =
        SEQUENCER_FILES.iter().map(|f| git_dir.join(f)).collect();
    if remove_autostash {
        files.push(autostash);
    }
    for file in files {
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => problems.push(format!("removing '{}' failed: {err}", file.display())),
        }
    }
    let sequencer = git_dir.join("sequencer");
    match std::fs::remove_dir_all(&sequencer) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => problems.push(format!("removing '{}' failed: {err}", sequencer.display())),
    }
    if repo.state().is_some() {
        problems.push("the operation still appears to be in progress".into());
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// The number of entries in the stash list (lines of the `refs/stash` reflog).
fn stash_count(repo: &gix::Repository) -> usize {
    std::fs::read(repo.common_dir().join("logs").join("refs").join("stash"))
        .map(|log| log.lines().filter(|l| !l.is_empty()).count())
        .unwrap_or(0)
}
