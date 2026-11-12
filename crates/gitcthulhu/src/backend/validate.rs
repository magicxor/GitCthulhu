//! Validation and app-level backend calls of the dialogs (owned by G10 after G0b; spec 21 §2.4).

use std::sync::Arc;

use super::{CloneOptions, FullName, GitError, GitRuntime, HelperStatus, ObjectId, OpContext};
use super::{OpOutcome, Repo};

/// The full name a new branch called `name` would get, or `InvalidName` / `AlreadyExists`.
pub fn validate_branch_name(repo: &Repo, name: &str) -> Result<FullName, GitError> {
    gitcthulhu_git::branch::validate_branch_name(repo, name)
}

/// The full name a new tag called `name` would get, or `InvalidName` / `AlreadyExists`.
pub fn validate_tag_name(repo: &Repo, name: &str) -> Result<FullName, GitError> {
    gitcthulhu_git::tag::validate_tag_name(repo, name)
}

/// Resolves a revision (`main~2`, a hash prefix) to a commit.
pub fn resolve_commit(repo: &Repo, spec: &str) -> Result<ObjectId, GitError> {
    gitcthulhu_git::refs::resolve_commit(repo, spec)
}

/// Where the helper programs are and whether they exist (About, `AuthFailed` dialogs).
pub fn helper_status(rt: &GitRuntime) -> HelperStatus {
    rt.helper_status()
}

/// `network::clone` (app-level, with its own progress dialog; not a tab operation).
pub fn clone_repository(
    rt: &Arc<GitRuntime>,
    ctx: &OpContext,
    opts: &CloneOptions,
) -> Result<OpOutcome<Repo>, GitError> {
    gitcthulhu_git::network::clone(rt, ctx, opts)
}

// ---------------------------------------------------------------------------------------------------------------
// Pure helpers of the clone and submodule dialogs (09 §3.1, §3.9). No backend call: they only look at strings and
// the file system, so the dialogs can use them on the UI thread and tests need no repository.
// ---------------------------------------------------------------------------------------------------------------

/// The repository name a clone or submodule URL suggests: the last non-empty path segment (separators `/`, `\` and
/// the scp-style `:`), without a trailing `.git`. Empty when nothing usable is left.
pub fn repo_name_from_url(url: &str) -> String {
    let url = url.trim();
    // `https://` alone has no name yet (the scheme's `:` is a separator below).
    let url = url.split_once("://").map_or(url, |(_, rest)| rest);
    let last = url
        .rsplit(['/', '\\', ':'])
        .find(|segment| !segment.trim().is_empty())
        .unwrap_or("")
        .trim();
    match last.strip_suffix(".git") {
        Some(stripped) => stripped.to_owned(),
        None => last.to_owned(),
    }
}

/// The first problem of the clone form, in the order 09 §3.1 validates: directory, URL, folder.
pub fn clone_form_error(url: &str, directory: &str, folder: &str) -> Option<&'static str> {
    if directory.trim().is_empty() {
        Some("Invalid empty directory")
    } else if url.trim().is_empty() {
        Some("Invalid empty URL")
    } else if folder.trim().is_empty() {
        Some("Invalid empty folder name")
    } else {
        None
    }
}

/// The first problem of the new-submodule form (09 §3.9), or `None`. `workdir` is the repository's worktree
/// (`None` for a bare repository: submodules need one).
pub fn submodule_form_error(
    workdir: Option<&std::path::Path>,
    url: &str,
    directory: &str,
) -> Option<String> {
    let Some(workdir) = workdir else {
        return Some("Worktree not found".to_owned());
    };
    let directory = directory.trim();
    if directory.is_empty() {
        return Some("Invalid empty directory".to_owned());
    }
    let url = url.trim();
    if url.is_empty() {
        return Some("Invalid empty URL".to_owned());
    }
    if url.starts_with('-') || url.contains(char::is_whitespace) {
        return Some(format!(
            "Invalid repository URL \"{url}\". Check your repository URL and try again."
        ));
    }
    let target = workdir.join(directory);
    let occupied = match std::fs::read_dir(&target) {
        Ok(mut entries) => entries.next().is_some(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        // Exists but is not a directory (or cannot be read): Git would refuse it as well.
        Err(_) => target.exists(),
    };
    occupied
        .then(|| format!("Directory {directory} contains files. Try again with a different name."))
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn repo_names_come_from_the_last_url_segment() {
        for (url, name) in [
            ("https://github.com/team/project.git", "project"),
            ("https://github.com/team/project", "project"),
            ("https://github.com/team/project.git/", "project"),
            ("git@github.com:team/project.git", "project"),
            ("ssh://git@host:2222/srv/git/tool.git", "tool"),
            (r"C:\work\repos\app", "app"),
            ("/srv/git/app.git", "app"),
            ("  https://h/x/y.git  ", "y"),
            ("", ""),
            ("https://", ""),
        ] {
            assert_eq!(repo_name_from_url(url), name, "{url}");
        }
    }

    #[test]
    fn clone_form_errors_are_reported_in_gitnuros_order() {
        assert_eq!(
            clone_form_error("", "", ""),
            Some("Invalid empty directory")
        );
        assert_eq!(clone_form_error("", "C:/x", ""), Some("Invalid empty URL"));
        assert_eq!(
            clone_form_error("u", "C:/x", " "),
            Some("Invalid empty folder name")
        );
        assert_eq!(clone_form_error("u", "C:/x", "f"), None);
    }

    #[test]
    fn submodule_form_errors() {
        let dir = std::env::temp_dir().join(format!("gitcthulhu-g10-sub-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("taken")).expect("test directory");
        std::fs::write(dir.join("taken").join("f.txt"), "x").expect("test file");
        std::fs::create_dir_all(dir.join("empty")).expect("test directory");
        let check = |url: &str, directory: &str| submodule_form_error(Some(&dir), url, directory);
        assert_eq!(
            submodule_form_error(None, "u", "d").as_deref(),
            Some("Worktree not found")
        );
        assert_eq!(check("u", " ").as_deref(), Some("Invalid empty directory"));
        assert_eq!(check(" ", "d").as_deref(), Some("Invalid empty URL"));
        assert!(
            check("--upload-pack=x", "d")
                .expect("an option-like URL is rejected")
                .ends_with("Check your repository URL and try again.")
        );
        assert_eq!(
            check("https://h/x.git", "taken").as_deref(),
            Some("Directory taken contains files. Try again with a different name.")
        );
        assert_eq!(
            check("https://h/x.git", "empty"),
            None,
            "an empty directory is fine"
        );
        assert_eq!(
            check("https://h/x.git", "new"),
            None,
            "a missing directory is fine"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
