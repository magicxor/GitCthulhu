//! Tests of the executor (`cli`, spec §2.3) and the classifier (§1.9).

mod support;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gitcthulhu_git::{
    CancelToken, GitError, GitErrorKind, InProgress, OpClass, OpContext, OpWarning, PostState,
    Progress, Repo,
    cli::{self, CommandKind, GitCommand, GitOutput},
    error::classify,
};
use support::{TestEnv, TestRepo};

fn silent() -> OpContext {
    OpContext::silent(CancelToken::new())
}

/// A repository with one commit of `f.txt`.
fn repo_with_commit(env: &TestEnv) -> TestRepo {
    let fixture = env.repo("repo");
    fixture.write("f.txt", "one\n");
    fixture.commit_all("one");
    fixture
}

/// `git <args>` through the executor, whatever the exit code.
fn run_raw(repo: &Repo, kind: CommandKind, args: &[&str]) -> GitOutput {
    cli::run(
        repo.runtime(),
        GitCommand::new(repo, kind).args(args.iter().copied()),
        &silent(),
    )
    .expect("git can be run")
}

/// The environment a child git passes to a `!`-alias, one `KEY=value` per line.
fn env_dump(repo: &Repo) -> String {
    let out = run_raw(
        repo,
        CommandKind::Mutation,
        &["-c", "alias.envdump=!env", "envdump"],
    );
    assert_eq!(out.exit_code, Some(0), "env can be run: {}", out.stderr);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn has_var(dump: &str, key: &str) -> bool {
    dump.lines()
        .any(|line| line.split_once('=').is_some_and(|(k, _)| k == key))
}

fn lines_with<'a>(dump: &'a str, needle: &str) -> Vec<&'a str> {
    dump.lines().filter(|line| line.contains(needle)).collect()
}

// ───────────────────────────── progress ─────────────────────────────

#[test]
fn progress_lines_are_parsed() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("progress");
    let mut fixtures: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the progress fixtures exist")
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "txt"))
        .collect();
    fixtures.sort();
    assert!(
        fixtures.len() >= 2,
        "a captured and a hand-written golden file: {fixtures:?}"
    );
    for fixture in fixtures {
        let raw = std::fs::read(&fixture).expect("fixture is readable");
        let events = cli::testing::progress_events(&raw);
        assert!(
            events.iter().any(|event| matches!(
                event,
                Progress::Phase {
                    remote: true,
                    done: true,
                    ..
                }
            )),
            "{}: padded `remote:` lines finish as phases",
            fixture.display()
        );
        let rendered: String = events.iter().map(|event| format!("{event:?}\n")).collect();
        // The golden events of each raw stderr capture (padding and `\r` kept in the input).
        snapbox::assert_data_eq!(
            rendered,
            snapbox::Data::read_from(&fixture.with_extension("events"), None).raw()
        );
    }
}

#[test]
fn progress_events_are_streamed() {
    let env = TestEnv::new();
    let origin = repo_with_commit(&env);
    for idx in 0..50 {
        origin.write(&format!("dir/file-{idx}.txt"), format!("content {idx}\n"));
    }
    origin.commit_all("many files");

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let events = events.clone();
        move |event: Progress| events.lock().expect("not poisoned").push(event)
    };
    let ctx = OpContext {
        progress: Arc::new(sink),
        ..silent()
    };
    let url = format!("file://{}", origin.path.display()).replace('\\', "/");
    let target = env.path("clone");
    let cmd = GitCommand::global(env.tmp.path().to_owned(), CommandKind::Network)
        .args(["clone", "--progress", url.as_str()])
        .arg(target.as_os_str())
        .with_progress();
    let out = cli::run_checked_global(&env.rt, cmd, &ctx, OpClass::Clone).expect("clone works");

    let events = events.lock().expect("not poisoned");
    assert!(
        matches!(events.first(), Some(Progress::Command(args)) if args.starts_with("clone --progress")),
        "the sanitised command comes first: {:?}",
        events.first()
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            Progress::Phase { phase, done: true, .. } if phase == "Receiving objects"
        )),
        "receiving objects is reported as a phase: {events:?}"
    );
    assert!(
        !out.stderr.contains("Receiving objects"),
        "progress lines are not kept in stderr: {}",
        out.stderr
    );
}

// ───────────────────────────── pathspecs ─────────────────────────────

#[test]
fn pathspec_glob_chars_are_literal() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    for name in ["1.txt", "2.txt", "[12].txt"] {
        fixture.write(name, "original\n");
    }
    fixture.commit_all("three files");
    for name in ["1.txt", "2.txt", "[12].txt"] {
        fixture.write(name, "modified\n");
    }
    let repo = fixture.open();

    let cmd = GitCommand::new(&repo, CommandKind::Mutation)
        .arg("restore")
        .pathspecs(&["[12].txt".into()]);
    cli::run_checked(&repo, cmd, &silent(), OpClass::Restore).expect("restore works");

    assert_eq!(
        fixture.read("[12].txt"),
        b"original\n",
        "the named file is restored"
    );
    assert_eq!(
        fixture.read("1.txt"),
        b"modified\n",
        "`[12]` is not a character class matching 1.txt"
    );
    assert_eq!(fixture.read("2.txt"), b"modified\n", "nor 2.txt");
}

/// Index entries named `name` and `others`, then `git rm --cached` of `name` through a literal pathspec. Index-only,
/// so names that Windows forbids in the work tree (`:`, `*`) work everywhere.
fn rm_cached_only(name: &str, others: &[&str]) {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("seed.txt", "x\n");
    fixture.commit_all("seed");
    let blob = fixture.git(&["rev-parse", "HEAD:seed.txt"]);
    for path in std::iter::once(&name).chain(others) {
        fixture.git(&[
            // Git for Windows refuses `:` and `*` in index paths unless told otherwise.
            "-c",
            "core.protectNTFS=false",
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},{path}"),
        ]);
    }
    let repo = fixture.open();

    let cmd = GitCommand::new(&repo, CommandKind::Mutation)
        .args(["rm", "--cached", "-q"])
        .pathspecs(&[name.into()]);
    cli::run_checked(&repo, cmd, &silent(), OpClass::Generic).expect("rm --cached works");

    let listed = fixture.git(&["ls-files"]);
    let mut expected: Vec<&str> = others.iter().copied().chain(["seed.txt"]).collect();
    expected.sort_unstable();
    let mut actual: Vec<&str> = listed.lines().collect();
    actual.sort_unstable();
    assert_eq!(actual, expected, "only {name:?} left the index");
}

#[test]
fn colon_prefixed_name_is_literal() {
    // As magic, `:!x` would mean "everything except x".
    rm_cached_only(":!x", &["x", "y"]);
}

#[test]
fn star_in_name_is_literal() {
    rm_cached_only("*.txt", &["a.txt", "b.txt"]);
}

#[test]
fn stdin_pathspec_handles_10000_paths() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let paths: Vec<gitcthulhu_git::RelaPath> = (0..10_000)
        .map(|idx| format!("many/directory-name-to-make-it-long/file-{idx:05}.txt").into())
        .collect();
    for path in &paths {
        fixture.write(&path.to_string(), "x");
    }
    let repo = fixture.open();

    let cmd = GitCommand::new(&repo, CommandKind::Mutation)
        .arg("add")
        .pathspecs(&paths);
    cli::run_checked(&repo, cmd, &silent(), OpClass::Generic)
        .expect("far more than 32 KiB of paths go through stdin");

    let staged = fixture.git(&["ls-files"]);
    assert_eq!(staged.lines().count(), 10_000, "every path was staged");
}

// ───────────────────────────── environment ─────────────────────────────

#[test]
fn env_has_language_and_no_git_dir() {
    let env = TestEnv::new();
    let repo = repo_with_commit(&env).open();
    let dump = env_dump(&repo);
    for expected in [
        "LANGUAGE=en",
        "LC_MESSAGES=C",
        "GIT_TERMINAL_PROMPT=0",
        "GIT_EDITOR=:",
        "GIT_SEQUENCE_EDITOR=:",
        "GIT_MERGE_AUTOEDIT=no",
        "GIT_PAGER=cat",
    ] {
        assert!(
            dump.lines().any(|line| line == expected),
            "{expected} is set:\n{dump}"
        );
    }
    for absent in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        assert!(!has_var(&dump, absent), "{absent} is not set:\n{dump}");
    }
    // `LC_ALL` is neither forced (hooks keep the user's UTF-8 locale) nor removed: the child sees the inherited value,
    // and `LANGUAGE=en` keeps the messages English under it (the CI's git-min job runs with `LC_ALL=de_DE.UTF-8`).
    let inherited = std::env::var("LC_ALL").ok().filter(|v| !v.is_empty());
    let child = dump
        .lines()
        .find_map(|line| line.strip_prefix("LC_ALL="))
        .filter(|v| !v.is_empty());
    assert_eq!(
        child,
        inherited.as_deref(),
        "LC_ALL is passed through unchanged:\n{dump}"
    );
}

#[test]
fn env_and_arguments_depend_on_command_kind() {
    let env = TestEnv::new();
    let fixture = repo_with_commit(&env);
    let rt = env
        .runtime_with(|config| {
            config.network_config = vec![("http.proxy".into(), "http://proxy.invalid:3128".into())]
        })
        .expect("runtime with network settings");
    let repo = Repo::open(&rt, &fixture.path).expect("repo opens");
    let dump = |kind: CommandKind, editor: cli::EditorMode| {
        let cmd = GitCommand::new(&repo, kind)
            .args(["-c", "alias.envdump=!env", "envdump"])
            .editor(editor);
        let out = cli::run(&rt, cmd, &silent()).expect("git can be run");
        assert_eq!(out.exit_code, Some(0), "env can be run: {}", out.stderr);
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    let read = dump(CommandKind::Read, cli::EditorMode::Noop);
    assert!(
        read.lines().any(|line| line == "GIT_OPTIONAL_LOCKS=0"),
        "reads never refresh the index opportunistically:\n{read}"
    );
    assert!(
        lines_with(&read, "proxy.invalid").is_empty(),
        "reads get no network settings:\n{read}"
    );

    let plan_dir = env.path("it's a plan");
    let mutation = dump(
        CommandKind::Mutation,
        cli::EditorMode::RebaseHelper {
            plan_dir: plan_dir.clone(),
        },
    );
    assert!(
        !has_var(&mutation, "GIT_OPTIONAL_LOCKS"),
        "mutations may take optional locks:\n{mutation}"
    );
    assert_eq!(
        lines_with(&mutation, "proxy.invalid").len(),
        1,
        "the GUI's network settings reach mutations as `-c` (LFS can download):\n{mutation}"
    );
    let quoted_plan = format!(
        "'{}'",
        plan_dir
            .display()
            .to_string()
            .replace('\\', "/")
            .replace('\'', r"'\''")
    );
    for (var, mode) in [
        ("GIT_SEQUENCE_EDITOR", "sequence"),
        ("GIT_EDITOR", "message"),
    ] {
        let line = mutation
            .lines()
            .find(|line| line.starts_with(&format!("{var}=")))
            .unwrap_or_else(|| panic!("{var} is set:\n{mutation}"));
        assert!(
            line.ends_with(&format!(" {mode} {quoted_plan}")),
            "{var} runs the helper in {mode} mode with the sh-quoted plan dir: {line}"
        );
        let helper = line
            .split_once(&format!(" {mode} "))
            .map(|(helper, _)| helper)
            .unwrap_or_default();
        assert!(
            helper.contains("gitcthulhu-git-editor") && !helper.contains('\\'),
            "{var} names the helper with forward slashes: {line}"
        );
    }
}

#[test]
fn literal_pathspecs_env_never_set() {
    let env = TestEnv::new();
    let repo = repo_with_commit(&env).open();
    let dump = env_dump(&repo);
    assert!(
        !has_var(&dump, "GIT_LITERAL_PATHSPECS"),
        "literal pathspecs come from `:(literal)`, not the environment:\n{dump}"
    );
    run_in_child_process(
        "child_inherited_pathspec_env_is_removed",
        &[("GIT_LITERAL_PATHSPECS", "1"), ("GIT_GLOB_PATHSPECS", "1")],
    );
}

#[test]
fn inherited_tooling_env_removed() {
    run_in_child_process(
        "child_inherited_tooling_env_is_removed",
        &[
            ("GIT_ASKPASS", "/bad"),
            ("VSCODE_GIT_IPC_HANDLE", "x"),
            ("GIT_CONFIG_PARAMETERS", "'x.y'='z'"),
            ("GIT_DIR", "/bogus-git-dir"),
        ],
    );
}

/// Marks a test process started by [`run_in_child_process`].
const CHILD_MARKER: &str = "GITCTHULHU_CLI_TEST_CHILD";

/// Run the ignored test `name` of this binary in a child process with `vars` in its environment, so the runtime
/// captures them as inherited environment without mutating this process's environment (tests run in parallel).
fn run_in_child_process(name: &str, vars: &[(&str, &str)]) {
    let exe = std::env::current_exe().expect("the test binary knows its path");
    let output = std::process::Command::new(exe)
        .args([
            name,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_MARKER, "1")
        .envs(vars.iter().copied())
        .output()
        .expect("the test binary can be re-run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "child test {name} failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn is_child_process() -> bool {
    std::env::var_os(CHILD_MARKER).is_some()
}

#[test]
#[ignore = "runs in a child process of `inherited_tooling_env_removed`"]
fn child_inherited_tooling_env_is_removed() {
    if !is_child_process() {
        return;
    }
    let env = TestEnv::new();
    let repo = repo_with_commit(&env).open();
    let dump = env_dump(&repo);
    assert!(
        lines_with(&dump, "/bad").is_empty(),
        "GIT_ASKPASS=/bad is gone:\n{dump}"
    );
    assert!(
        !has_var(&dump, "VSCODE_GIT_IPC_HANDLE"),
        "VS Code's askpass handle is gone:\n{dump}"
    );
    assert!(
        lines_with(&dump, "x.y").is_empty(),
        "the inherited `-c x.y=z` is gone (git sets GIT_CONFIG_PARAMETERS itself for our own -c):\n{dump}"
    );
    assert!(
        lines_with(&dump, "bogus-git-dir").is_empty(),
        "an inherited GIT_DIR does not redirect the child:\n{dump}"
    );
}

#[test]
#[ignore = "runs in a child process of `literal_pathspecs_env_never_set`"]
fn child_inherited_pathspec_env_is_removed() {
    if !is_child_process() {
        return;
    }
    let env = TestEnv::new();
    let repo = repo_with_commit(&env).open();
    let dump = env_dump(&repo);
    for key in ["GIT_LITERAL_PATHSPECS", "GIT_GLOB_PATHSPECS"] {
        assert!(!has_var(&dump, key), "inherited {key} is removed:\n{dump}");
    }
}

// ───────────────────────────── hooks ─────────────────────────────

#[test]
fn trace2_reports_failing_pre_commit_hook() {
    let env = TestEnv::new();
    let fixture = repo_with_commit(&env);
    fixture.hook("pre-commit", "echo 'lint failed: f.txt' >&2\nexit 1");
    fixture.write("f.txt", "two\n");
    fixture.git(&["add", "f.txt"]);
    let repo = fixture.open();

    let cmd = GitCommand::new(&repo, CommandKind::Mutation)
        .args(["commit", "-m", "two"])
        .with_hooks();
    match cli::run_checked(&repo, cmd, &silent(), OpClass::Commit) {
        Err(GitError::HookFailed {
            hook,
            exit_code,
            output,
        }) => {
            assert_eq!(hook, "pre-commit", "the failing hook is named");
            assert_eq!(exit_code, Some(1), "with its exit code");
            assert!(
                output.contains("lint failed: f.txt"),
                "and its output: {output}"
            );
        }
        other => panic!("expected HookFailed, got {other:?}"),
    }
}

#[test]
fn post_hook_failure_is_warning_not_error() {
    let env = TestEnv::new();
    let fixture = repo_with_commit(&env);
    fixture.git(&["branch", "f"]);
    fixture.hook("post-checkout", "echo 'post-checkout broke' >&2\nexit 3");
    let repo = fixture.open();

    let cmd = GitCommand::new(&repo, CommandKind::Mutation)
        .args(["switch", "f"])
        .with_hooks();
    let out = cli::run_checked(&repo, cmd, &silent(), OpClass::Checkout)
        .expect("a failing post hook does not fail the operation");
    assert_ne!(
        out.exit_code,
        Some(0),
        "git itself reports the hook's failure"
    );
    assert!(out.success(), "but the command counts as successful");
    match out.warnings.as_slice() {
        [
            OpWarning::PostHookFailed {
                hook,
                exit_code,
                output,
            },
        ] => {
            assert_eq!(hook, "post-checkout", "the hook is named");
            assert_eq!(*exit_code, Some(3), "with its exit code");
            assert!(
                output.contains("post-checkout broke"),
                "and its output: {output}"
            );
        }
        other => panic!("expected exactly one PostHookFailed, got {other:?}"),
    }
    assert_eq!(
        fixture.git(&["branch", "--show-current"]),
        "f",
        "the switch happened"
    );
}

#[test]
fn nested_git_in_hook_not_attributed() {
    let env = TestEnv::new();
    let fixture = repo_with_commit(&env);
    let remote = env.bare_remote("remote");
    let nested_hooks = env.path("nested-hooks");
    std::fs::create_dir_all(&nested_hooks).expect("hooks dir");
    std::fs::write(nested_hooks.join("pre-push"), "#!/bin/sh\nexit 9\n").expect("nested hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            nested_hooks.join("pre-push"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("nested hook is executable");
    }
    let slash = |p: &Path| p.display().to_string().replace('\\', "/");
    // The hook runs two nested gits: a plain one, and a push whose own pre-push hook fails.
    fixture.hook(
        "pre-commit",
        &format!(
            "git -c x.y=z status >/dev/null\ngit -c core.hooksPath='{}' push -q '{}' HEAD:refs/heads/nested 2>/dev/null || true\nexit 0",
            slash(&nested_hooks),
            slash(&remote)
        ),
    );
    fixture.write("f.txt", "two\n");
    fixture.git(&["add", "f.txt"]);
    let repo = fixture.open();

    let cmd = GitCommand::new(&repo, CommandKind::Mutation)
        .args(["commit", "-q", "-m", "two"])
        .with_hooks();
    let out = cli::run_checked(&repo, cmd, &silent(), OpClass::Commit).expect("the commit works");
    let hooks: Vec<_> = out
        .hooks
        .iter()
        .map(|hook| (hook.name.as_str(), hook.exit_code))
        .collect();
    assert_eq!(
        hooks,
        [("pre-commit", Some(0))],
        "the nested push's failing pre-push hook is not the commit's"
    );
    assert!(out.warnings.is_empty(), "no warnings either");
}

// ───────────────────────────── cancellation ─────────────────────────────

/// Processes that descend from this test process whose name starts with one of `names`.
///
/// On Windows an MSYS `exec` (a shell running its last command) starts the new program from a helper process, so
/// such programs are not found through the parent chain; look for the shell instead.
fn descendants_named(sys: &mut sysinfo::System, names: &[&str]) -> Vec<sysinfo::Pid> {
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let me = sysinfo::get_current_pid().expect("own pid");
    sys.processes()
        .iter()
        .filter(|(_, process)| {
            let name = process.name().to_string_lossy().to_ascii_lowercase();
            names.iter().any(|wanted| name.starts_with(wanted))
        })
        .filter(|(pid, _)| {
            let mut current = **pid;
            for _ in 0..16 {
                match sys.process(current).and_then(|p| p.parent()) {
                    Some(parent) if parent == me => return true,
                    Some(parent) => current = parent,
                    None => return false,
                }
            }
            false
        })
        .map(|(pid, _)| *pid)
        .collect()
}

fn wait_until(what: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn cancel_kills_process_tree() {
    let env = TestEnv::new();
    let repo = repo_with_commit(&env).open();
    let (alias, sleeper) = if cfg!(windows) {
        ("alias.slow=!ping -n 30 127.0.0.1 >/dev/null", "ping")
    } else {
        ("alias.slow=!sleep 30", "sleep")
    };
    let cancel = CancelToken::new();
    let worker = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let cmd = GitCommand::new(&repo, CommandKind::Mutation).args(["-c", alias, "slow"]);
            cli::run(repo.runtime(), cmd, &OpContext::silent(cancel))
        })
    };

    let mut sys = sysinfo::System::new();
    let mut victims = Vec::new();
    wait_until("the slow alias to start", Duration::from_secs(20), || {
        victims = descendants_named(&mut sys, &[sleeper]);
        !victims.is_empty()
    });
    let cancelled_at = Instant::now();
    cancel.cancel();
    let result = worker.join().expect("the worker does not panic");
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(5),
        "cancel returns quickly: {:?}",
        cancelled_at.elapsed()
    );
    assert!(
        matches!(result, Err(GitError::Cancelled)),
        "a cancelled command reports Cancelled: {result:?}"
    );
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    for pid in victims {
        assert!(
            sys.process(pid)
                .is_none_or(|p| p.status() == sysinfo::ProcessStatus::Zombie),
            "the grandchild {pid} (`{sleeper}` under sh under git) was killed with the tree"
        );
    }
}

/// Start `git commit -a` with a pre-commit hook that sleeps, wait until `index.lock` exists and the hook runs,
/// cancel. (`commit -a` keeps the index locked while its hooks run; a plain commit of a staged index does not.)
fn cancel_commit_while_index_is_locked(
    fixture: &TestRepo,
    before_cleanup: Option<Box<dyn Fn() + Send + Sync>>,
) -> PathBuf {
    fixture.hook("pre-commit", "sleep 30");
    fixture.write("f.txt", "two\n");
    let repo = fixture.open();
    let lock = repo.git_dir().join("index.lock");
    let _hook = before_cleanup.map(|hook| cli::testing::after_kill(fixture.path.clone(), hook));

    let cancel = CancelToken::new();
    let worker = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let cmd = GitCommand::new(&repo, CommandKind::Mutation)
                .args(["commit", "-a", "-m", "two"])
                .with_hooks();
            cli::run_checked(&repo, cmd, &OpContext::silent(cancel), OpClass::Commit)
        })
    };
    let mut sys = sysinfo::System::new();
    wait_until(
        "the hook to run with the index locked",
        Duration::from_secs(20),
        || lock.exists() && !descendants_named(&mut sys, &["sh", "sleep", "pre-commit"]).is_empty(),
    );
    cancel.cancel();
    let result = worker.join().expect("the worker does not panic");
    assert!(
        matches!(result, Err(GitError::Cancelled)),
        "the commit was cancelled: {result:?}"
    );
    lock
}

#[test]
fn stale_index_lock_removed_after_cancel() {
    let env = TestEnv::new();
    let fixture = repo_with_commit(&env);
    let lock = cancel_commit_while_index_is_locked(&fixture, None);
    assert!(
        !lock.exists(),
        "the lock of the killed commit is gone, so the next operation works"
    );
    std::fs::remove_file(fixture.path.join(".git").join("hooks").join("pre-commit"))
        .expect("the sleeping hook can be removed");
    fixture.git(&["commit", "-q", "-a", "-m", "after cancel"]);
}

#[test]
fn foreign_lock_recreated_after_kill_is_kept() {
    let env = TestEnv::new();
    let fixture = repo_with_commit(&env);
    let lock_path = fixture.path.join(".git").join("index.lock");
    let replace = move || {
        // Another git takes the lock between the kill and the cleanup.
        std::fs::remove_file(&lock_path).ok();
        std::fs::write(&lock_path, b"another git's index")
            .expect("the foreign lock can be written");
    };
    let lock = cancel_commit_while_index_is_locked(&fixture, Some(Box::new(replace)));
    assert_eq!(
        std::fs::read(&lock).expect("the foreign lock survives"),
        b"another git's index",
        "a lock with a different file id is never deleted"
    );
    std::fs::remove_file(&lock).expect("the test cleans up the foreign lock");
}

// ───────────────────────────── misc ─────────────────────────────

#[test]
fn run_simple_returns_stdout() {
    let env = TestEnv::new();
    let fixture = repo_with_commit(&env);
    let repo = fixture.open();
    let head = cli::run_simple(&repo, &["rev-parse", "HEAD"], &CancelToken::new())
        .expect("rev-parse works");
    assert_eq!(
        String::from_utf8_lossy(&head).trim(),
        fixture.git(&["rev-parse", "HEAD"]),
        "stdout is returned as is"
    );
    match cli::run_simple(
        &repo,
        &["rev-parse", "--verify", "nosuch"],
        &CancelToken::new(),
    ) {
        Err(GitError::CommandFailed {
            args, exit_code, ..
        }) => {
            assert_eq!(
                args, "rev-parse --verify nosuch",
                "the arguments are recorded"
            );
            assert_eq!(exit_code, Some(128), "git's exit code");
        }
        other => panic!("a failure is a plain CommandFailed: {other:?}"),
    }
}

#[test]
fn stdout_cap_exceeded_is_command_failed() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("big.txt", "x".repeat(200_000));
    fixture.commit_all("big");
    let repo = fixture.open();
    let mut cmd =
        GitCommand::new(&repo, CommandKind::Read).args(["cat-file", "-p", "HEAD:big.txt"]);
    cmd.stdout_cap = 1024;
    match cli::run(repo.runtime(), cmd, &silent()) {
        Err(GitError::CommandFailed { stderr, .. }) => {
            assert!(stderr.contains("output too large"), "explained: {stderr}")
        }
        other => panic!("expected CommandFailed, got {other:?}"),
    }
}

#[test]
fn stdout_cap_exceeded_by_a_quick_exit_is_never_success() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    // Small enough to fit into the pipe buffer: git writes it all and exits 0 at once, racing the reader that
    // notices the overflow.
    fixture.write("small.txt", "y".repeat(2000));
    fixture.commit_all("small");
    let repo = fixture.open();
    for attempt in 0..10 {
        let mut cmd =
            GitCommand::new(&repo, CommandKind::Read).args(["cat-file", "-p", "HEAD:small.txt"]);
        cmd.stdout_cap = 1024;
        match cli::run(repo.runtime(), cmd, &silent()) {
            Err(GitError::CommandFailed { stderr, .. }) => assert!(
                stderr.contains("output too large"),
                "attempt {attempt}: explained: {stderr}"
            ),
            other => panic!("attempt {attempt}: truncated output must be an error, got {other:?}"),
        }
    }
}

#[test]
fn credentials_in_urls_are_not_recorded() {
    let env = TestEnv::new();
    let repo = repo_with_commit(&env).open();
    let out = run_raw(
        &repo,
        CommandKind::Read,
        &[
            "ls-remote",
            "--get-url",
            "https://alice:s3cret@example.invalid/r.git",
        ],
    );
    assert_eq!(
        out.record.args, "ls-remote --get-url https://***@example.invalid/r.git",
        "user and password are masked"
    );
}

#[test]
fn missing_working_directory_is_io_error() {
    let env = TestEnv::new();
    let cmd = GitCommand::global(env.path("missing"), CommandKind::Read).arg("version");
    match cli::run(&env.rt, cmd, &silent()) {
        Err(err) => assert_eq!(err.kind(), GitErrorKind::Io, "{err:?}"),
        Ok(out) => panic!("expected an error, got {out:?}"),
    }
}

#[test]
fn already_cancelled_token_spawns_nothing() {
    let env = TestEnv::new();
    let cancel = CancelToken::new();
    cancel.cancel();
    let cmd = GitCommand::global(env.tmp.path().to_owned(), CommandKind::Read).arg("version");
    assert!(
        matches!(
            cli::run(&env.rt, cmd, &OpContext::silent(cancel)),
            Err(GitError::Cancelled)
        ),
        "a cancelled token fails fast"
    );
}

#[test]
fn run_checked_classifies_with_post_state() {
    let env = TestEnv::new();
    let fixture = repo_with_commit(&env);
    fixture.git(&["switch", "-q", "-c", "other"]);
    fixture.write("f.txt", "other\n");
    fixture.commit_all("other");
    fixture.git(&["switch", "-q", "main"]);
    fixture.write("f.txt", "main\n");
    fixture.commit_all("main");
    let repo = fixture.open();

    let cmd = GitCommand::new(&repo, CommandKind::Mutation)
        .args(["merge", "other"])
        .with_hooks();
    match cli::run_checked(&repo, cmd, &silent(), OpClass::Merge) {
        Err(GitError::Conflicts {
            operation, paths, ..
        }) => {
            assert_eq!(
                operation,
                Some(InProgress::Merge),
                "the merge is in progress"
            );
            assert_eq!(paths, ["f.txt"], "the conflicted file comes from the index");
        }
        other => panic!("expected Conflicts, got {other:?}"),
    }
}

// ───────────────────────────── classification table ─────────────────────────────

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("stderr")
}

/// What a fixture of `row` (§1.9) must classify as, and the operation class and post state it is classified with.
fn expectation(row: &str) -> (OpClass, PostState, GitErrorKind) {
    use GitErrorKind as K;
    let conflicted = PostState {
        in_progress: Some(InProgress::Merge),
        conflicted: vec!["f.txt".into()],
        head_branch: Some("main".into()),
        pick_head: None,
    };
    let generic = |kind| (OpClass::Generic, PostState::default(), kind);
    match row {
        "04" => generic(K::UnsafeRepository),
        "05" => generic(K::LockExists),
        "06" => generic(K::HostKeyVerificationFailed),
        "07" => generic(K::AuthFailed),
        "08" => generic(K::RemoteRepositoryNotFound),
        "09" => generic(K::NoSuchRemote),
        "10" => generic(K::Network),
        "14" => generic(K::RefNotFound),
        "15" => generic(K::BranchCheckedOutElsewhere),
        "15a" => (OpClass::Pull, PostState::default(), K::PullStrategyRequired),
        "15b" => generic(K::NoUpstream),
        "15c" => (
            OpClass::CherryPick,
            PostState {
                in_progress: Some(InProgress::CherryPick),
                ..PostState::default()
            },
            K::EmptyPick,
        ),
        "16" | "17" => generic(K::WouldOverwrite),
        "18" => generic(K::UncommittedChanges),
        "19" => generic(K::OperationInProgress),
        "20" => (OpClass::Merge, conflicted, K::Conflicts),
        "21" => generic(K::UnrelatedHistories),
        "22" => generic(K::NotFastForwardable),
        "23" => generic(K::AlreadyExists),
        "24" => generic(K::InvalidName),
        "25" => generic(K::NotFullyMerged),
        "26" => (OpClass::Commit, PostState::default(), K::SigningFailed),
        "27" => (OpClass::Commit, PostState::default(), K::IdentityUnknown),
        "28" => (OpClass::Commit, PostState::default(), K::NothingToCommit),
        "29" => (OpClass::Commit, PostState::default(), K::EmptyMessage),
        "30" => generic(K::NotARepository),
        "31" => generic(K::CommandFailed),
        other => panic!("fixture for unknown row {other}"),
    }
}

/// Text rows of §1.9. Rows 1-3 are not text based (cancel, refused prompt, trace2); 11-13 belong to `network.rs`.
const TEXT_ROWS: &[&str] = &[
    "04", "05", "06", "07", "08", "09", "10", "14", "15", "15a", "15b", "15c", "16", "17", "18",
    "19", "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "30", "31",
];

#[test]
fn classify_table() {
    let mut seen = std::collections::BTreeSet::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .expect("the stderr fixtures exist")
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "stderr"))
        .collect();
    entries.sort();
    for stderr_path in entries {
        let name = stderr_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("utf-8 fixture name")
            .to_owned();
        // <row>[.<variant>]-<git version or "transcribed">
        let row = name
            .split(['.', '-'])
            .next()
            .expect("the name starts with the row")
            .to_owned();
        let stdout = std::fs::read(stderr_path.with_extension("stdout")).unwrap_or_default();
        let stderr = std::fs::read_to_string(&stderr_path).expect("fixture is utf-8");
        let out = GitOutput {
            exit_code: Some(1),
            stdout,
            stderr,
            hooks: Vec::new(),
            record: gitcthulhu_git::CommandRecord {
                args: name.clone(),
                exit_code: Some(1),
                duration: Duration::ZERO,
                stderr: String::new(),
            },
            prompt_refused: None,
            warnings: Vec::new(),
        };
        let (op, post, expected) = expectation(&row);
        let err = classify(op, &out, &post);
        assert_eq!(err.kind(), expected, "{name}: {err:?}");
        assert_extracted_fields(&name, &err);
        seen.insert(row);
    }
    let missing: Vec<_> = TEXT_ROWS
        .iter()
        .filter(|row| !seen.contains(**row))
        .collect();
    assert!(missing.is_empty(), "rows without a fixture: {missing:?}");
}

/// The names and paths the classifier pulls out of the messages are what the scenarios used (see `capture`).
fn assert_extracted_fields(name: &str, err: &GitError) {
    match err {
        GitError::UnsafeRepository { path } => {
            assert_eq!(path, Path::new("/srv/shared/repo"), "{name}: owned path")
        }
        GitError::LockExists { path } => assert_eq!(
            path,
            Path::new("/tmp/gc/repo/.git/index.lock"),
            "{name}: the lock file"
        ),
        GitError::HostKeyVerificationFailed { host, changed, .. } => {
            assert_eq!(host.as_deref(), Some("github.com"), "{name}: host");
            assert_eq!(*changed, name.contains("changed"), "{name}: changed key");
        }
        GitError::NoSuchRemote { name: remote } => {
            assert_eq!(remote, "nosuch", "{name}: remote name")
        }
        GitError::Network { reason, .. } => assert!(!reason.is_empty(), "{name}: a reason"),
        GitError::RefNotFound { name: refname, .. } => {
            assert!(refname.contains("nosuch"), "{name}: the ref: {refname}")
        }
        GitError::BranchCheckedOutElsewhere { branch, worktree } => {
            assert!(
                ["main", "side"].contains(&branch.as_str()),
                "{name}: branch {branch}"
            );
            assert!(
                worktree.starts_with("/tmp/gc"),
                "{name}: worktree {worktree:?}"
            );
        }
        GitError::NoUpstream { .. } | GitError::Conflicts { .. } => {}
        GitError::EmptyPick { .. } => {}
        GitError::WouldOverwrite { kind, paths, .. } => {
            let expected = if name.starts_with("16") {
                (gitcthulhu_git::OverwriteKind::LocalChanges, "f.txt")
            } else {
                (gitcthulhu_git::OverwriteKind::Untracked, "u.txt")
            };
            assert_eq!(
                (*kind, paths.as_slice()),
                (expected.0, &[expected.1.into()][..]),
                "{name}: kind and paths"
            );
        }
        GitError::AlreadyExists {
            what,
            name: existing,
        } => assert!(
            matches!(
                (*what, existing.as_str()),
                ("branch", "other") | ("tag", "v1") | ("remote", "up") | ("directory", _)
            ),
            "{name}: {what} {existing}"
        ),
        GitError::InvalidName { name: invalid, .. } => {
            assert_eq!(invalid, "a..b", "{name}: invalid name")
        }
        GitError::NotFullyMerged { branch } => assert_eq!(branch, "other", "{name}: branch"),
        _ => {}
    }
}

/// Re-create the captured fixtures of `tests/fixtures/{stderr,progress}` with the installed Git:
/// `GITCTHULHU_CAPTURE_FIXTURES=1 cargo test -p gitcthulhu-git --test cli -- --ignored capture_fixtures`.
/// Files are named after the Git version, so running this on Git 2.34.1 adds that version's variants.
#[test]
#[ignore = "writes fixture files; run explicitly with GITCTHULHU_CAPTURE_FIXTURES=1"]
fn capture_fixtures() {
    if std::env::var_os("GITCTHULHU_CAPTURE_FIXTURES").is_none() {
        return;
    }
    capture::stderr_fixtures();
    capture::progress_fixture();
}

mod capture {
    use super::*;

    struct Writer {
        env: TestEnv,
        version: String,
    }

    impl Writer {
        fn save(&self, row: &str, out: &GitOutput) {
            assert_ne!(
                out.exit_code,
                Some(0),
                "{row}: the scenario must fail: {}",
                out.stderr
            );
            let tmp = self.env.tmp.path().display().to_string();
            let anonymise = |text: &str| {
                text.replace(&tmp, "/tmp/gc")
                    .replace(&tmp.replace('\\', "/"), "/tmp/gc")
            };
            let base = format!("{row}-{}", self.version);
            std::fs::write(
                fixtures_dir().join(format!("{base}.stderr")),
                anonymise(&out.stderr),
            )
            .expect("fixture can be written");
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stdout_path = fixtures_dir().join(format!("{base}.stdout"));
            if stdout.trim().is_empty() {
                std::fs::remove_file(&stdout_path).ok();
            } else {
                std::fs::write(stdout_path, anonymise(&stdout)).expect("fixture can be written");
            }
        }

        fn global(
            &self,
            cwd: &Path,
            kind: CommandKind,
            args: &[&str],
            extra_env: &[(&str, &Path)],
        ) -> GitOutput {
            let mut cmd = GitCommand::global(cwd.to_owned(), kind).args(args.iter().copied());
            for (key, value) in extra_env {
                cmd.extra_env
                    .push(((*key).into(), value.as_os_str().to_owned()));
            }
            cli::run(&self.env.rt, cmd, &silent()).expect("git runs")
        }
    }

    /// `main` with f.txt, and `other` changing f.txt differently: merges conflict.
    fn diverged(env: &TestEnv, name: &str) -> TestRepo {
        let fixture = env.repo(name);
        fixture.write("f.txt", "base\n");
        fixture.commit_all("base");
        fixture.git(&["switch", "-q", "-c", "other"]);
        fixture.write("f.txt", "other\n");
        fixture.write("u.txt", "tracked on other\n");
        fixture.commit_all("other");
        fixture.git(&["switch", "-q", "main"]);
        fixture.write("f.txt", "main\n");
        fixture.commit_all("main");
        fixture
    }

    pub(super) fn stderr_fixtures() {
        let env = TestEnv::new();
        let version = env.rt.version().to_string();
        let w = Writer { env, version };
        let env = &w.env;
        use CommandKind::{Mutation as M, Network as N, Read as R};

        // 05
        let fixture = repo_with_commit(env);
        let repo = fixture.open();
        std::fs::write(repo.git_dir().join("index.lock"), b"").expect("lock");
        w.save("05", &run_raw(&repo, M, &["add", "f.txt"]));
        std::fs::remove_file(repo.git_dir().join("index.lock")).expect("lock removed");

        // 08, 09
        let missing = env.path("no-such-remote-dir").display().to_string();
        w.save("08.path", &run_raw(&repo, N, &["fetch", &missing]));
        w.save("09.fetch", &run_raw(&repo, N, &["fetch", "nosuch"]));
        w.save(
            "09.remove",
            &run_raw(&repo, M, &["remote", "remove", "nosuch"]),
        );

        // 10
        w.save(
            "10.refused",
            &run_raw(&repo, N, &["ls-remote", "http://127.0.0.1:9/r.git"]),
        );
        w.save(
            "10.dns",
            &run_raw(&repo, N, &["ls-remote", "https://gitcthulhu.invalid/r.git"]),
        );

        // 14
        let remote = env.bare_remote("origin");
        let remote_url = remote.display().to_string();
        fixture.git(&["remote", "add", "origin", &remote_url]);
        fixture.git(&["push", "-q", "origin", "main"]);
        w.save(
            "14.fetch",
            &run_raw(&repo, N, &["fetch", "origin", "nosuchbranch"]),
        );
        w.save("14.push", &run_raw(&repo, N, &["push", "origin", "nosuch"]));

        // 15
        let linked = env.path("linked");
        fixture.git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            &linked.display().to_string(),
        ]);
        w.save("15.switch", &run_raw(&repo, M, &["switch", "side"]));
        w.save("15.branch-d", &run_raw(&repo, M, &["branch", "-d", "main"]));
        // 18, `worktree remove` of a worktree with an untracked file
        std::fs::write(linked.join("untracked.txt"), "x").expect("untracked file in the worktree");
        w.save(
            "18.worktree-remove",
            &run_raw(
                &repo,
                M,
                &["worktree", "remove", &linked.display().to_string()],
            ),
        );

        // 15b
        fixture.git(&["switch", "-q", "-c", "lonely-branch"]);
        w.save("15b.pull", &run_raw(&repo, N, &["pull"]));
        w.save("15b.push", &run_raw(&repo, N, &["push"]));
        fixture.git(&["switch", "-q", "main"]);

        // 15a
        let clone_path = env.path("clone");
        env.git_global(&[
            "clone",
            "-q",
            &remote_url,
            &clone_path.display().to_string(),
        ]);
        let clone = Repo::open(&env.rt, &clone_path).expect("clone opens");
        std::fs::write(clone_path.join("c.txt"), "clone\n").expect("write");
        run_ok(&clone, &["add", "c.txt"]);
        run_ok(&clone, &["commit", "-q", "-m", "clone side"]);
        fixture.write("o.txt", "origin\n");
        fixture.commit_all("origin side");
        fixture.git(&["push", "-q", "origin", "main"]);
        w.save("15a", &run_raw(&clone, N, &["pull"]));

        // 15c
        let picks = env.repo("picks");
        picks.write("base.txt", "base\n");
        picks.commit_all("base");
        picks.git(&["switch", "-q", "-c", "side"]);
        picks.write("a.txt", "same\n");
        let side = picks.commit_all("side").to_string();
        picks.git(&["switch", "-q", "main"]);
        picks.write("a.txt", "same\n");
        picks.commit_all("main");
        let picks_repo = picks.open();
        w.save("15c", &run_raw(&picks_repo, M, &["cherry-pick", &side]));
        picks.git(&["cherry-pick", "--abort"]);

        // 16, 17, 18
        let d = diverged(env, "diverged");
        let d_repo = d.open();
        d.write("f.txt", "dirty\n");
        w.save("16", &run_raw(&d_repo, M, &["switch", "other"]));
        w.save(
            "16.reset-keep",
            &run_raw(&d_repo, M, &["reset", "--keep", "other"]),
        );
        w.save("18", &run_raw(&d_repo, M, &["rebase", "other"]));
        d.git(&["checkout", "--", "f.txt"]);
        d.write("u.txt", "untracked here\n");
        w.save("17", &run_raw(&d_repo, M, &["switch", "other"]));
        w.save(
            "17.reset-keep",
            &run_raw(&d_repo, M, &["reset", "--keep", "other"]),
        );
        d.remove("u.txt");

        // 20, 19
        w.save("20.merge", &run_raw(&d_repo, M, &["merge", "other"]));
        d.write("f.txt", "resolved\n");
        d.git(&["add", "f.txt"]);
        w.save("19.merge", &run_raw(&d_repo, M, &["merge", "other"]));
        d.git(&["merge", "--abort"]);
        let other = d.git(&["rev-parse", "other"]);
        w.save(
            "20.cherry-pick",
            &run_raw(&d_repo, M, &["cherry-pick", &other]),
        );
        w.save(
            "20.unmerged",
            &run_raw(&d_repo, M, &["cherry-pick", &other]),
        );
        d.git(&["cherry-pick", "--abort"]);
        // 20, `rebase --continue` while the conflicts are not resolved
        let started = run_raw(&d_repo, M, &["rebase", "other"]);
        assert_ne!(started.exit_code, Some(0), "the rebase stops at a conflict");
        w.save(
            "20.rebase-continue",
            &run_raw(&d_repo, M, &["rebase", "--continue"]),
        );
        d.git(&["rebase", "--abort"]);

        // 21, 22
        d.git(&["switch", "-q", "--orphan", "unrelated"]);
        d.write("lonely.txt", "x\n");
        d.git(&["add", "lonely.txt"]);
        d.git(&["commit", "-q", "-m", "unrelated"]);
        d.git(&["switch", "-q", "-f", "main"]);
        w.save("21", &run_raw(&d_repo, M, &["merge", "unrelated"]));
        w.save("22", &run_raw(&d_repo, M, &["merge", "--ff-only", "other"]));

        // 23, 24, 25
        w.save("23.branch", &run_raw(&d_repo, M, &["branch", "other"]));
        d.git(&["tag", "v1"]);
        w.save("23.tag", &run_raw(&d_repo, M, &["tag", "v1"]));
        d.git(&["remote", "add", "up", "https://example.invalid/r.git"]);
        w.save(
            "23.remote",
            &run_raw(
                &d_repo,
                M,
                &["remote", "add", "up", "https://example.invalid/r.git"],
            ),
        );
        let occupied = env.path("occupied");
        std::fs::create_dir_all(&occupied).expect("dir");
        std::fs::write(occupied.join("file"), "x").expect("file");
        w.save(
            "23.clone",
            &w.global(
                env.tmp.path(),
                N,
                &["clone", &remote_url, &occupied.display().to_string()],
                &[],
            ),
        );
        w.save("24.branch", &run_raw(&d_repo, M, &["branch", "a..b"]));
        w.save("24.tag", &run_raw(&d_repo, M, &["tag", "a..b"]));
        w.save("25", &run_raw(&d_repo, M, &["branch", "-d", "other"]));

        // 26, 27, 28, 29
        w.save(
            "26",
            &run_raw(
                &d_repo,
                M,
                &[
                    "-c",
                    "commit.gpgsign=true",
                    "-c",
                    "gpg.program=gitcthulhu-no-such-gpg",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "signed",
                ],
            ),
        );
        let empty_config = env.path("empty-gitconfig");
        std::fs::write(&empty_config, "").expect("empty config");
        w.save(
            "27",
            &w.global(
                &d.path,
                M,
                &[
                    "-c",
                    "user.useConfigOnly=true",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "x",
                ],
                &[("GIT_CONFIG_GLOBAL", &empty_config)],
            ),
        );
        w.save("28", &run_raw(&d_repo, M, &["commit", "-m", "nothing"]));
        w.save(
            "29",
            &run_raw(&d_repo, M, &["commit", "--allow-empty", "-m", ""]),
        );

        // 30, 31
        let plain = env.path("plain");
        std::fs::create_dir_all(&plain).expect("dir");
        w.save(
            "30",
            &w.global(
                &plain,
                R,
                &["status"],
                &[("GIT_CEILING_DIRECTORIES", env.tmp.path())],
            ),
        );
        w.save(
            "31",
            &run_raw(&d_repo, R, &["rev-parse", "--verify", "nosuch"]),
        );
    }

    fn run_ok(repo: &Repo, args: &[&str]) {
        let out = run_raw(repo, CommandKind::Mutation, args);
        assert_eq!(out.exit_code, Some(0), "git {args:?}: {}", out.stderr);
    }

    /// Raw stderr (padding and `\r` kept) of `git clone --progress` from a `file://` remote.
    pub(super) fn progress_fixture() {
        let env = TestEnv::new();
        let origin = env.repo("origin");
        for idx in 0..3000 {
            origin.write(
                &format!("d{}/f{idx}.txt", idx % 30),
                format!("{idx}\n").repeat(idx % 50 + 1),
            );
        }
        origin.commit_all("many files");
        let url = format!("file://{}", origin.path.display()).replace('\\', "/");
        let output = std::process::Command::new(env.rt.git_path())
            .args([
                "clone",
                "--progress",
                &url,
                &env.path("clone").display().to_string(),
            ])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &env.global_config)
            .env("HOME", &env.home)
            .env("LANGUAGE", "en")
            .env("LC_MESSAGES", "C")
            .output()
            .expect("git clone runs");
        assert!(output.status.success(), "clone works");
        let version = env.rt.version();
        let raw = String::from_utf8_lossy(&output.stderr)
            .replace(&env.path("clone").display().to_string(), "/tmp/gc/clone");
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("progress")
            .join(format!("clone-{version}.txt"));
        std::fs::write(path, raw).expect("fixture can be written");
    }
}
