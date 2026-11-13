//! Partial-availability rules for hunk and line operations (§4.2, WP-B0d).
//!
//! One function decides `TextDiff::partial` for `diff.rs` and gates `stage_selection` / `unstage_selection` /
//! `discard_selection` in `stage.rs`, so the GUI never offers what the backend would refuse.
//! `UnsafeRoundTrip` is not decided here: it needs the worktree bytes and is checked by the discard procedure (§4.6).

use bstr::BStr;

use crate::{EntryKind, PartialAvailability, PartialBlock, Result, WhitespaceMode};

/// Which two images a diff compares; decides which rename rule applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sides {
    /// Unstaged changes: index (stage 2 for conflicts, nothing for untracked) → worktree.
    IndexToWorktree,
    /// Staged changes: `HEAD^{tree}` → index.
    HeadToIndex,
    /// A commit against its parent or another tree.
    Commit,
}

/// How the content of the two sides compares, as determined by the diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContentClass {
    /// Text that the line engine diffs.
    Text,
    /// Both sides have the same blob id (only the mode may differ).
    Identical,
    /// Either side is binary.
    Binary,
    /// Either side exceeds the size or line limit.
    TooLarge,
}

/// What the diff learned about the change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Kinds {
    /// The old side's kind; `None` for an added or untracked path (and intent-to-add).
    pub old: Option<EntryKind>,
    /// The new side's kind; `None` for a deleted path.
    pub new: Option<EntryKind>,
    /// The content comparison.
    pub content: ContentClass,
    /// The diff pairs two different paths (`previous_path` is set).
    pub renamed: bool,
    /// The path has unmerged index entries.
    pub conflicted: bool,
    /// The whitespace mode the diff was computed with.
    pub whitespace: WhitespaceMode,
}

/// Decide whether hunks and lines of the change at `path` can be staged, unstaged or discarded on their own.
///
/// `textconv` is `true` if the displayed text came from a diff driver's binary-to-text conversion. `index` is used
/// to read `.gitattributes` for the LFS rule (worktree first, then the index, like Git). The reasons are checked
/// in this order, the first match wins: `Conflict`, `Submodule`, `TypeChange`, `Symlink`, `TooLarge`, `Binary`,
/// `ModeOnly`, `TextConv`, `Rename` (unstaged diffs only), `WhitespaceIgnored`, `Lfs` (the only one that reads
/// attributes, so it comes last).
pub(crate) fn partial_availability(
    repo: &gix::Repository,
    index: &gix::index::State,
    path: &BStr,
    sides: Sides,
    kinds: &Kinds,
    textconv: bool,
) -> Result<PartialAvailability> {
    if let Some(reason) = block_reason(sides, kinds, textconv) {
        return Ok(PartialAvailability::WholeFileOnly(reason));
    }
    if is_lfs(repo, index, path)? {
        return Ok(PartialAvailability::WholeFileOnly(PartialBlock::Lfs));
    }
    Ok(PartialAvailability::Allowed)
}

/// Every rule that needs no repository access.
fn block_reason(sides: Sides, kinds: &Kinds, textconv: bool) -> Option<PartialBlock> {
    let is_kind = |kind: EntryKind| kinds.old == Some(kind) || kinds.new == Some(kind);
    let is_blob = |kind: EntryKind| matches!(kind, EntryKind::Blob | EntryKind::BlobExecutable);
    let type_changed = match (kinds.old, kinds.new) {
        (Some(old), Some(new)) => old != new && !(is_blob(old) && is_blob(new)),
        _ => false,
    };
    let mode_only = kinds.content == ContentClass::Identical
        && matches!((kinds.old, kinds.new), (Some(old), Some(new)) if old != new && is_blob(old) && is_blob(new));

    let reason = if kinds.conflicted && sides != Sides::Commit {
        PartialBlock::Conflict
    } else if is_kind(EntryKind::Submodule) {
        PartialBlock::Submodule
    } else if type_changed || is_kind(EntryKind::Tree) {
        // A directory entry (sparse index) on either side has no lines to select.
        PartialBlock::TypeChange
    } else if is_kind(EntryKind::Link) {
        PartialBlock::Symlink
    } else if kinds.content == ContentClass::TooLarge {
        PartialBlock::TooLarge
    } else if kinds.content == ContentClass::Binary {
        PartialBlock::Binary
    } else if mode_only {
        PartialBlock::ModeOnly
    } else if textconv {
        PartialBlock::TextConv
    } else if kinds.renamed && sides == Sides::IndexToWorktree {
        // Staged renames allow partial unstage at the new path (§4.5 step 8); unstaged ones are whole-file only.
        PartialBlock::Rename
    } else if kinds.whitespace != WhitespaceMode::None {
        PartialBlock::WhitespaceIgnored
    } else {
        return None;
    };
    Some(reason)
}

/// `true` if `.gitattributes` assigns `filter=lfs` to `path` (the content is a pointer, not the file).
fn is_lfs(repo: &gix::Repository, index: &gix::index::State, path: &BStr) -> Result<bool> {
    use gix::worktree::stack::state::attributes::Source;
    let source = if repo.workdir().is_some() {
        Source::WorktreeThenIdMapping
    } else {
        Source::IdMapping
    };
    let mut stack = repo.attributes_only(index, source)?;
    let mut outcome = stack.selected_attribute_matches(["filter"]);
    let platform = stack.at_entry(path, None)?;
    platform.matching_attributes(&mut outcome);
    let lfs = outcome.iter_selected().any(|m| {
        matches!(m.assignment.state, gix::attrs::StateRef::Value(value) if value.as_bstr() == "lfs")
    });
    Ok(lfs)
}

#[cfg(test)]
mod tests {
    use bstr::ByteSlice;

    use super::*;
    use crate::internal::content::fixture;

    fn text_change() -> Kinds {
        Kinds {
            old: Some(EntryKind::Blob),
            new: Some(EntryKind::Blob),
            content: ContentClass::Text,
            renamed: false,
            conflicted: false,
            whitespace: WhitespaceMode::None,
        }
    }

    #[test]
    fn partial_rules_table() {
        use EntryKind::*;
        use PartialBlock as B;
        let unstaged = Sides::IndexToWorktree;
        let cases: Vec<(&str, Sides, Kinds, bool, Option<PartialBlock>)> = vec![
            ("plain text", unstaged, text_change(), false, None),
            (
                "untracked text",
                unstaged,
                Kinds {
                    old: None,
                    ..text_change()
                },
                false,
                None,
            ),
            (
                "deleted text",
                unstaged,
                Kinds {
                    new: None,
                    ..text_change()
                },
                false,
                None,
            ),
            (
                "exec bit plus content",
                unstaged,
                Kinds {
                    new: Some(BlobExecutable),
                    ..text_change()
                },
                false,
                None,
            ),
            (
                "conflict",
                unstaged,
                Kinds {
                    conflicted: true,
                    ..text_change()
                },
                false,
                Some(B::Conflict),
            ),
            (
                "conflict wins over binary",
                Sides::HeadToIndex,
                Kinds {
                    conflicted: true,
                    content: ContentClass::Binary,
                    ..text_change()
                },
                false,
                Some(B::Conflict),
            ),
            (
                "submodule",
                unstaged,
                Kinds {
                    old: Some(Submodule),
                    new: Some(Submodule),
                    ..text_change()
                },
                false,
                Some(B::Submodule),
            ),
            (
                "file to link",
                unstaged,
                Kinds {
                    new: Some(Link),
                    ..text_change()
                },
                false,
                Some(B::TypeChange),
            ),
            (
                "new symlink",
                unstaged,
                Kinds {
                    old: None,
                    new: Some(Link),
                    ..text_change()
                },
                false,
                Some(B::Symlink),
            ),
            (
                "too large",
                unstaged,
                Kinds {
                    content: ContentClass::TooLarge,
                    ..text_change()
                },
                false,
                Some(B::TooLarge),
            ),
            (
                "binary",
                unstaged,
                Kinds {
                    content: ContentClass::Binary,
                    ..text_change()
                },
                false,
                Some(B::Binary),
            ),
            (
                "mode only",
                unstaged,
                Kinds {
                    new: Some(BlobExecutable),
                    content: ContentClass::Identical,
                    ..text_change()
                },
                false,
                Some(B::ModeOnly),
            ),
            ("textconv", unstaged, text_change(), true, Some(B::TextConv)),
            (
                "unstaged rename",
                unstaged,
                Kinds {
                    renamed: true,
                    ..text_change()
                },
                false,
                Some(B::Rename),
            ),
            (
                "staged rename",
                Sides::HeadToIndex,
                Kinds {
                    renamed: true,
                    ..text_change()
                },
                false,
                None,
            ),
            (
                "whitespace ignored",
                unstaged,
                Kinds {
                    whitespace: WhitespaceMode::IgnoreAll,
                    ..text_change()
                },
                false,
                Some(B::WhitespaceIgnored),
            ),
            (
                "commit text",
                Sides::Commit,
                Kinds {
                    renamed: true,
                    ..text_change()
                },
                false,
                None,
            ),
        ];
        for (name, sides, kinds, textconv, expected) in cases {
            assert_eq!(block_reason(sides, &kinds, textconv), expected, "{name}");
        }
    }

    #[test]
    fn partial_lfs_attribute_blocks_partial() {
        let repo = fixture::repo();
        repo.write(
            ".gitattributes",
            "*.bin filter=lfs diff=lfs merge=lfs -text\n",
        );
        repo.write(
            "a.bin",
            "version https://git-lfs.github.com/spec/v1\noid sha256:00\nsize 1\n",
        );
        repo.write("a.txt", "text\n");
        repo.git(&["add", ".gitattributes", "a.txt"]);
        let local = repo.open();
        let index = local.index().expect("index exists");
        let lfs = partial_availability(
            &local,
            &index,
            b"a.bin".as_bstr(),
            Sides::IndexToWorktree,
            &text_change(),
            false,
        )
        .expect("attributes readable");
        assert_eq!(
            lfs,
            PartialAvailability::WholeFileOnly(PartialBlock::Lfs),
            "filter=lfs is a pointer file"
        );
        let text = partial_availability(
            &local,
            &index,
            b"a.txt".as_bstr(),
            Sides::IndexToWorktree,
            &text_change(),
            false,
        )
        .expect("attributes readable");
        assert_eq!(
            text,
            PartialAvailability::Allowed,
            "other files are unaffected"
        );
        let binary = partial_availability(
            &local,
            &index,
            b"a.bin".as_bstr(),
            Sides::IndexToWorktree,
            &Kinds {
                content: ContentClass::Binary,
                ..text_change()
            },
            false,
        )
        .expect("attributes readable");
        assert_eq!(
            binary,
            PartialAvailability::WholeFileOnly(PartialBlock::Binary),
            "cheap rules are reported before attributes are read"
        );
    }
}
