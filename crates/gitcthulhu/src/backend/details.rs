//! Commit-details reads (owned by G6 after G0b; spec 21 §2.4).

use super::{CommitDetails, CommitFilesOptions, GitError, ObjectId, Repo};

/// A commit with its message and changed files (no line stats); `stash` appends the untracked files of a stash.
pub fn commit_details(repo: &Repo, id: ObjectId, stash: bool) -> Result<CommitDetails, GitError> {
    gitcthulhu_git::diff::commit_details(
        repo,
        id,
        CommitFilesOptions {
            line_stats: false,
            stash,
        },
    )
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;
    use crate::backend::ChangeKind;
    use crate::backend::handle::{UndoSettings, open_repo};
    use crate::test_support::Fixture;

    fn id_of(fixture: &Fixture, rev: &str) -> ObjectId {
        ObjectId::from_hex(fixture.git(&["rev-parse", rev]).trim().as_bytes()).expect("an id")
    }

    #[test]
    fn details_carry_message_author_and_files() {
        let fixture = Fixture::new();
        fixture.write("a.txt", "one\n");
        fixture.commit_all("First\n\nBody of the first commit.");
        fixture.write("a.txt", "one\ntwo\n");
        fixture.write("dir/b.txt", "b\n");
        fixture.commit_all("Second");
        let rt = fixture.runtime();
        let handle =
            open_repo(&rt, fixture.path(), &UndoSettings::default()).expect("the repo opens");

        let first =
            commit_details(&handle.repo, id_of(&fixture, "HEAD~1"), false).expect("details");
        assert_eq!(
            first.message.trim_end(),
            "First\n\nBody of the first commit."
        );
        assert_eq!(first.info.author.person.name.as_slice(), b"Test User");
        assert!(first.info.parents.is_empty());
        assert_eq!(first.files.len(), 1, "a root commit lists its files");
        assert_eq!(first.files[0].kind, ChangeKind::Added);

        let second = commit_details(&handle.repo, id_of(&fixture, "HEAD"), false).expect("details");
        let files: Vec<(String, ChangeKind)> = second
            .files
            .iter()
            .map(|f| (f.path.to_string(), f.kind))
            .collect();
        assert_eq!(
            files,
            vec![
                ("a.txt".to_owned(), ChangeKind::Modified),
                ("dir/b.txt".to_owned(), ChangeKind::Added),
            ]
        );
        assert_eq!(second.info.parents.len(), 1);
    }

    #[test]
    fn stash_flag_appends_untracked_files() {
        let fixture = Fixture::new();
        fixture.write("tracked.txt", "x\n");
        fixture.commit_all("Base");
        fixture.write("tracked.txt", "changed\n");
        fixture.write("untracked.txt", "u\n");
        fixture.git(&["stash", "push", "-u", "-q"]);
        let stash = id_of(&fixture, "stash@{0}");
        let rt = fixture.runtime();
        let handle =
            open_repo(&rt, fixture.path(), &UndoSettings::default()).expect("the repo opens");
        let plain = commit_details(&handle.repo, stash, false).expect("details");
        assert_eq!(plain.files.len(), 1);
        let with_untracked = commit_details(&handle.repo, stash, true).expect("details");
        let paths: Vec<String> = with_untracked
            .files
            .iter()
            .map(|f| f.path.to_string())
            .collect();
        assert_eq!(paths, vec!["tracked.txt", "untracked.txt"]);
    }

    #[test]
    fn unknown_commit_is_an_error() {
        let fixture = Fixture::new();
        fixture.write("a.txt", "x\n");
        fixture.commit_all("Base");
        let rt = fixture.runtime();
        let handle =
            open_repo(&rt, fixture.path(), &UndoSettings::default()).expect("the repo opens");
        let missing = ObjectId::from_hex(b"1234567890123456789012345678901234567890").expect("hex");
        assert!(commit_details(&handle.repo, missing, false).is_err());
    }
}
