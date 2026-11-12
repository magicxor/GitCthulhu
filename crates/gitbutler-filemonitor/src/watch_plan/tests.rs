mod compute_watch_plan {
    use std::path::{Component, Path, PathBuf};

    use notify::RecursiveMode;
    use snapbox::prelude::*;

    use crate::watch_plan::compute_watch_plan_for_repo;

    type CanonicalWatch = (RecursiveMode, PathBuf);

    #[test]
    fn respects_gitignore_for_ignored_dirs() {
        let (repo, _tmpdir) = but_testsupport::writable_scenario("watch-plan-ignores-node-modules");
        let actual = compute_canonicalized_plan(&repo);
        // There is no node-modules here, and submodules are tracked just by their directories and special directories
        // It's OK for a directory to not exist, as `.git/logs` doesn't exist yet despite being a directory we want to add.
        snapbox::assert_data_eq!(
            actual.to_debug(),
            snapbox::str![[r#"
[
    (
        NonRecursive,
        "",
    ),
    (
        NonRecursive,
        ".git",
    ),
    (
        Recursive,
        ".git/refs",
    ),
    (
        NonRecursive,
        ".git/modules",
    ),
    (
        NonRecursive,
        ".git/modules/submodule-worktree",
    ),
    (
        Recursive,
        ".git/modules/submodule-worktree/refs",
    ),
    (
        NonRecursive,
        "submodule-worktree",
    ),
    (
        NonRecursive,
        "submodule-worktree/dir",
    ),
    (
        NonRecursive,
        "submodule-repo",
    ),
    (
        NonRecursive,
        "submodule-repo/dir",
    ),
    (
        NonRecursive,
        "submodule-repo/.git",
    ),
    (
        NonRecursive,
        "submodule-repo/.git/logs",
    ),
    (
        NonRecursive,
        "submodule-repo/.git/logs/refs",
    ),
    (
        Recursive,
        "submodule-repo/.git/refs",
    ),
    (
        NonRecursive,
        "src",
    ),
]

"#]]
        );
    }

    #[test]
    fn includes_ignored_but_tracked_dirs() -> anyhow::Result<()> {
        let (repo, _tmpdir) = but_testsupport::writable_scenario("watch-plan-ignored-but-tracked");
        let actual = compute_canonicalized_plan(&repo);

        // We want to see ignored directories that are tracked, `ignored_but_tracked` and `submodule-worktree`
        // as these are always tracked.
        snapbox::assert_data_eq!(
            std::fs::read(repo.workdir_path(".gitignore").expect("worktree"))?,
            snapbox::str![[r#"
ignored_but_tracked_dir/
submodule-repo/
submodule-worktree/

"#]]
        );
        snapbox::assert_data_eq!(
            actual.to_debug(),
            snapbox::str![[r#"
[
    (
        NonRecursive,
        "",
    ),
    (
        NonRecursive,
        ".git",
    ),
    (
        NonRecursive,
        ".git/logs",
    ),
    (
        NonRecursive,
        ".git/logs/refs",
    ),
    (
        Recursive,
        ".git/refs",
    ),
    (
        NonRecursive,
        ".git/modules",
    ),
    (
        NonRecursive,
        ".git/modules/submodule-worktree",
    ),
    (
        Recursive,
        ".git/modules/submodule-worktree/refs",
    ),
    (
        NonRecursive,
        "submodule-worktree",
    ),
    (
        NonRecursive,
        "normal_dir",
    ),
    (
        NonRecursive,
        "ignored_but_tracked_dir",
    ),
]

"#]]
        );

        Ok(())
    }

    #[test]
    fn prioritizes_git_watches() {
        let (repo, _tmpdir) = but_testsupport::writable_scenario("watch-plan-ignores-node-modules");
        let worktree = repo.workdir().expect("non-bare");

        let plan = compute_watch_plan(worktree).expect("plan computed");
        let worktree_real = gix::path::realpath(worktree).expect("realpath worktree");

        let relative_paths: Vec<_> = plan
            .iter()
            .map(|(path, _mode)| canonicalize_one_path(path, &worktree_real))
            .collect();

        let first_non_git = relative_paths
            .iter()
            .position(|p| p != "." && !p.starts_with(".git"))
            .expect("fixture has at least one non-git directory watch");
        let last_git = relative_paths
            .iter()
            .rposition(|p| p.starts_with(".git"))
            .expect("plan watches git paths");

        assert!(
            last_git < first_non_git,
            "git watches should be added before worktree directory watches to preserve critical git monitoring when OS watch limits are hit; plan order was: {relative_paths:?}"
        );
    }

    fn canonicalize_plan(
        plan: Vec<(PathBuf, RecursiveMode)>,
        worktree: &Path,
    ) -> Vec<CanonicalWatch> {
        plan.into_iter()
            .map(|(path, mode)| {
                let relative = path
                    .strip_prefix(worktree)
                    .expect("fixture repositories keep all watched paths in the worktree");
                (
                    mode,
                    PathBuf::from(
                        relative
                            .to_string_lossy()
                            .replace(std::path::MAIN_SEPARATOR, "/"),
                    ),
                )
            })
            .collect()
    }

    fn canonicalize_one_path(path: &Path, worktree: &Path) -> String {
        let relative = path
            .strip_prefix(worktree)
            .expect("fixture repositories keep all watched paths in the worktree");
        let mut components = Vec::new();
        for comp in relative.components() {
            let Component::Normal(part) = comp else {
                continue;
            };
            components.push(part.to_string_lossy());
        }
        if components.is_empty() {
            ".".to_string()
        } else {
            components.join("/")
        }
    }

    /// Compute the paths that should be watched for `worktree_path`.
    fn compute_watch_plan(worktree_path: &Path) -> anyhow::Result<Vec<(PathBuf, RecursiveMode)>> {
        let worktree_path = gix::path::realpath(worktree_path)?;
        let repo = gix::open_opts(&worktree_path, gix::open::Options::isolated())?;
        let git_dir = repo.path().to_owned();
        let mut plan = Vec::new();
        compute_watch_plan_for_repo(&repo, &worktree_path, &git_dir, &[], |path, mode| {
            plan.push((path.to_owned(), mode));
            Ok(std::ops::ControlFlow::Continue(()))
        })?;
        Ok(plan)
    }

    fn compute_canonicalized_plan(repo: &gix::Repository) -> Vec<CanonicalWatch> {
        let worktree = repo.workdir().expect("non-bare");
        let plan = compute_watch_plan(worktree).expect("plan computed");
        let worktree_real = gix::path::realpath(worktree).expect("realpath worktree");
        canonicalize_plan(plan, &worktree_real)
    }
}

mod git_dir_watches {
    use std::path::{Path, PathBuf};

    use notify::RecursiveMode;

    use crate::watch_plan::emit_git_dir_watches;

    /// A scratch directory removed on drop.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn scratch() -> Scratch {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "filemonitor-git-dir-watches-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch directory can be created");
        Scratch(dir)
    }

    fn module_repo(dir: &Path) {
        std::fs::create_dir_all(dir.join("refs/heads")).expect("refs can be created");
        std::fs::create_dir_all(dir.join("objects/pack")).expect("objects can be created");
        std::fs::create_dir_all(dir.join("logs")).expect("logs can be created");
        std::fs::write(dir.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD can be written");
    }

    #[test]
    fn submodules_with_slashes_in_their_name_and_nested_submodules_are_watched() {
        let tmp = scratch();
        let git_dir = tmp.0.join("git");
        module_repo(&git_dir);
        module_repo(&git_dir.join("modules/plain"));
        module_repo(&git_dir.join("modules/libs/foo"));
        module_repo(&git_dir.join("modules/plain/modules/inner"));

        let mut watches = Vec::new();
        let _flow = emit_git_dir_watches(&git_dir, &mut |path, mode| {
            let relative = path
                .strip_prefix(&git_dir)
                .expect("all watches are in the git dir")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            watches.push((mode, relative));
            Ok(std::ops::ControlFlow::Continue(()))
        })
        .expect("the watches can be listed");

        let modules: Vec<_> = watches
            .iter()
            .filter(|(_, path)| path.starts_with("modules"))
            .map(|(mode, path)| (*mode, path.as_str()))
            .collect();
        assert_eq!(
            modules,
            [
                (RecursiveMode::NonRecursive, "modules"),
                (RecursiveMode::NonRecursive, "modules/libs"),
                (RecursiveMode::NonRecursive, "modules/libs/foo"),
                (RecursiveMode::Recursive, "modules/libs/foo/refs"),
                (RecursiveMode::NonRecursive, "modules/plain"),
                (RecursiveMode::Recursive, "modules/plain/refs"),
                (RecursiveMode::NonRecursive, "modules/plain/modules"),
                (RecursiveMode::NonRecursive, "modules/plain/modules/inner"),
                (RecursiveMode::Recursive, "modules/plain/modules/inner/refs"),
            ],
            "every submodule repository is found, whatever its name; never its objects or logs"
        );
        assert!(
            !watches.iter().any(|(_, path)| path.contains("objects")),
            "no object database is watched: {watches:?}"
        );
    }
}
