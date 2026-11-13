//! Tests of `config` (spec §2.5, WP-B5).

mod support;

use std::sync::Mutex;

use gitcthulhu_git::{
    CancelToken, ConfigScope, GitErrorKind, Identity, PullMode, SigningFormat, config,
};
use support::{TestEnv, TestRepo};

/// `identity_env_override` sets `GIT_AUTHOR_*` in the process environment, which `identity` reads and every spawned
/// `git` inherits: every test of this binary holds this lock.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn commit(repo: &TestRepo, msg: &str) {
    repo.write("a.txt", msg);
    repo.commit_all(msg);
}

fn who(name: &str, email: &str) -> Identity {
    Identity {
        name: name.into(),
        email: email.into(),
    }
}

#[test]
fn identity_local_overrides_global() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "one");
    fixture.git(&["config", "user.name", "Local Name"]);

    let info = config::identity(Some(&fixture.open())).expect("identity");
    assert_eq!(
        info.global.name.as_deref(),
        Some("Test User"),
        "global name"
    );
    assert_eq!(
        info.global.email.as_deref(),
        Some("test@example.com"),
        "global email"
    );
    assert_eq!(info.local.name.as_deref(), Some("Local Name"), "local name");
    assert_eq!(info.local.email, None, "local has no email");
    assert_eq!(
        info.effective,
        Some(who("Local Name", "test@example.com")),
        "local name wins, the email falls back to global"
    );
}

#[test]
fn identity_effective_follows_includes() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "one");
    fixture.git(&["config", "user.name", "Local Name"]);
    let included = env.path("work-identity.cfg");
    std::fs::write(&included, "[user]\n\temail = work@example.com\n").expect("included file");
    let slashed = |path: &std::path::Path| path.to_string_lossy().replace('\\', "/");
    // The usual "work identity" setup: a conditional include keyed on the repository location.
    let local_config = fixture.path.join(".git").join("config");
    let mut text = std::fs::read_to_string(&local_config).expect("local config");
    text.push_str(&format!(
        "[includeIf \"gitdir/i:{}/\"]\n\tpath = {}\n",
        slashed(&fixture.path),
        slashed(&included)
    ));
    std::fs::write(&local_config, text).expect("local config written");
    assert_eq!(
        fixture.git(&["config", "user.email"]),
        "work@example.com",
        "oracle: git resolves the conditional include"
    );

    // An isolated gix open follows no includes at all, so this needs a production-like runtime. The local file sets
    // both values, so the developer's own global configuration cannot change the outcome.
    let rt = env
        .runtime_with(|config| config.test_isolation = None)
        .expect("runtime");
    let repo = gitcthulhu_git::Repo::open(&rt, &fixture.path).expect("open");
    let info = config::identity(Some(&repo)).expect("identity");
    assert_eq!(info.local.email, None, "the local file itself has no email");
    if std::env::var_os("GIT_AUTHOR_NAME").is_none()
        && std::env::var_os("GIT_AUTHOR_EMAIL").is_none()
    {
        assert_eq!(
            info.effective,
            Some(who("Local Name", "work@example.com")),
            "the effective identity is what git commit uses, includes followed"
        );
    }
}

#[test]
#[allow(unsafe_code)]
fn identity_env_override() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "one");
    // SAFETY: serialised by ENV_LOCK; no other test in this binary touches these variables.
    unsafe {
        std::env::set_var("GIT_AUTHOR_NAME", "Env Author");
        std::env::set_var("GIT_AUTHOR_EMAIL", "env@example.com");
    }
    let info = config::identity(Some(&fixture.open()));
    unsafe {
        std::env::remove_var("GIT_AUTHOR_NAME");
        std::env::remove_var("GIT_AUTHOR_EMAIL");
    }
    let info = info.expect("identity");
    assert_eq!(
        info.effective,
        Some(who("Env Author", "env@example.com")),
        "the environment beats every file"
    );
    assert_eq!(
        info.global.name.as_deref(),
        Some("Test User"),
        "files are reported unchanged"
    );
}

#[test]
fn set_identity_global_and_local() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "one");
    let repo = fixture.open();

    let refresh = config::set_identity(
        Some(&repo),
        ConfigScope::Local,
        Some(&who("Loc", "loc@x.org")),
    )
    .expect("set local");
    assert!(
        refresh.contains(gitcthulhu_git::RefreshSet::CONFIG),
        "CONFIG refresh"
    );
    assert_eq!(
        fixture.git(&["config", "--local", "user.name"]),
        "Loc",
        "git sees the local name"
    );
    assert_eq!(
        fixture.git(&["config", "--local", "user.email"]),
        "loc@x.org",
        "and email"
    );

    config::set_identity(
        Some(&repo),
        ConfigScope::Global,
        Some(&who("Glob", "glob@x.org")),
    )
    .expect("set global");
    assert_eq!(
        env.git_global(&["config", "--global", "user.name"]),
        "Glob",
        "global written"
    );
    let info = config::identity(Some(&repo)).expect("identity");
    assert_eq!(info.global.name.as_deref(), Some("Glob"), "global reread");
    assert_eq!(
        info.effective,
        Some(who("Loc", "loc@x.org")),
        "local still wins"
    );

    config::set_identity(Some(&repo), ConfigScope::Local, None).expect("unset local");
    let info = config::identity(Some(&repo)).expect("identity");
    assert_eq!(info.local.name, None, "local cleared");
    assert_eq!(
        info.effective,
        Some(who("Glob", "glob@x.org")),
        "global shows through"
    );

    let err = config::set_identity(None, ConfigScope::Local, Some(&who("a", "b")))
        .expect_err("local without repository");
    assert_eq!(
        err.kind(),
        GitErrorKind::Unsupported,
        "no repository, no local scope"
    );
}

#[test]
fn unset_value() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "one");
    let repo = fixture.open();

    config::set(
        Some(&repo),
        ConfigScope::Local,
        "gitcthulhu.test",
        Some("yes"),
    )
    .expect("set");
    assert_eq!(
        config::get(&repo, "gitcthulhu.test")
            .expect("get")
            .as_deref(),
        Some("yes"),
        "reloaded repo sees the value"
    );
    assert_eq!(
        config::get_bool(&repo, "gitcthulhu.test").expect("bool"),
        Some(true),
        "bool"
    );
    config::set(
        Some(&repo),
        ConfigScope::Local,
        "gitcthulhu.sub.key",
        Some("v"),
    )
    .expect("subsection");
    assert_eq!(
        fixture.git(&["config", "gitcthulhu.sub.key"]),
        "v",
        "subsection key written"
    );

    config::set(Some(&repo), ConfigScope::Local, "gitcthulhu.test", None).expect("unset");
    assert_eq!(
        config::get(&repo, "gitcthulhu.test").expect("get"),
        None,
        "value gone"
    );
    config::set(Some(&repo), ConfigScope::Local, "gitcthulhu.test", None).expect("unset again");

    let err =
        config::set(Some(&repo), ConfigScope::Local, "nodots", Some("x")).expect_err("bad key");
    assert_eq!(err.kind(), GitErrorKind::InvalidName, "keys need a section");

    config::set(
        Some(&repo),
        ConfigScope::Worktree,
        "gitcthulhu.wt",
        Some("1"),
    )
    .expect("worktree scope");
    assert_eq!(
        fixture.git(&["config", "--worktree", "gitcthulhu.wt"]),
        "1",
        "git reads config.worktree"
    );
}

#[test]
fn signing_ssh_format() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "one");
    let repo = fixture.open();
    let plain = config::signing(&repo).expect("signing");
    assert!(
        !plain.commit_sign && !plain.tag_sign,
        "signing is off by default"
    );
    assert_eq!(plain.format, SigningFormat::OpenPgp, "default format");

    fixture.git(&["config", "gpg.format", "ssh"]);
    fixture.git(&["config", "user.signingkey", "~/.ssh/id_ed25519.pub"]);
    fixture.git(&["config", "commit.gpgsign", "true"]);
    fixture.git(&["config", "gpg.program", "gpg2"]);
    fixture.git(&["config", "gpg.ssh.program", "ssh-keygen"]);
    // The fixture pins `commit.gpgsign=false` on every gix open; drop that override to see the file's value.
    let rt = env
        .runtime_with(|config| {
            if let Some(isolation) = config.test_isolation.as_mut() {
                isolation
                    .overrides
                    .retain(|(key, _)| key != "commit.gpgsign");
            }
        })
        .expect("runtime");
    let repo = gitcthulhu_git::Repo::open(&rt, &fixture.path).expect("open");
    let signing = config::signing(&repo).expect("signing");
    assert!(signing.commit_sign, "commit.gpgsign");
    assert!(!signing.tag_sign, "tag.gpgsign stays off");
    assert_eq!(signing.format, SigningFormat::Ssh, "gpg.format");
    assert_eq!(signing.key.as_deref(), Some("~/.ssh/id_ed25519.pub"), "key");
    assert_eq!(
        signing.program.as_deref(),
        Some("ssh-keygen"),
        "the format-specific program wins"
    );

    fixture.git(&["config", "gpg.format", "x509"]);
    let signing = config::signing(&fixture.open()).expect("signing");
    assert_eq!(signing.format, SigningFormat::X509, "x509");
    assert_eq!(
        signing.program.as_deref(),
        Some("gpg2"),
        "falls back to gpg.program"
    );
}

#[test]
fn pull_mode_from_config() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "one");
    let mode = |fixture: &TestRepo| config::pull_mode_default(&fixture.open()).expect("pull mode");

    assert_eq!(mode(&fixture), PullMode::FromConfig, "nothing configured");
    fixture.git(&["config", "pull.rebase", "true"]);
    assert_eq!(mode(&fixture), PullMode::Rebase, "pull.rebase=true");
    fixture.git(&["config", "pull.rebase", "false"]);
    assert_eq!(mode(&fixture), PullMode::Merge, "pull.rebase=false");
    fixture.git(&["config", "--unset", "pull.rebase"]);
    fixture.git(&["config", "pull.ff", "only"]);
    assert_eq!(mode(&fixture), PullMode::FfOnly, "pull.ff=only");
    fixture.git(&["config", "branch.main.rebase", "merges"]);
    assert_eq!(mode(&fixture), PullMode::Rebase, "branch.<b>.rebase wins");
}

#[test]
fn commit_graph_present_detects_file() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "one");
    let repo = fixture.open();
    assert!(
        !config::commit_graph_present(&repo),
        "fresh repository has none"
    );
    fixture.git(&["commit-graph", "write", "--reachable"]);
    assert!(config::commit_graph_present(&repo), "written by git");
}

#[test]
fn write_commit_graph_creates_file() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "one");
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let ctx = fixture.op_ctx(
        CancelToken::new(),
        std::sync::Arc::new(gitcthulhu_git::NoPrompter),
    );
    let out =
        config::write_commit_graph(&repo, guard.write_permission(), &ctx).expect("commit-graph");
    assert!(
        out.refresh.contains(gitcthulhu_git::RefreshSet::LOG),
        "LOG refresh"
    );
    assert!(config::commit_graph_present(&repo), "the file exists");
}

#[test]
fn commit_graph_is_written_in_the_background_only_for_large_repositories() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    for n in 0..5 {
        commit(&fixture, &format!("c{n}"));
    }
    let repo = fixture.open();
    let ctx = fixture.op_ctx(
        CancelToken::new(),
        std::sync::Arc::new(gitcthulhu_git::NoPrompter),
    );
    assert!(
        !config::write_commit_graph_if_large(&repo, 5, &ctx).expect("probe"),
        "5 commits are not more than 5"
    );
    assert!(!config::commit_graph_present(&repo), "nothing was written");
    assert!(
        config::write_commit_graph_if_large(&repo, 4, &ctx).expect("write"),
        "5 commits are more than 4"
    );
    assert!(config::commit_graph_present(&repo), "the file exists");
    assert!(
        !config::write_commit_graph_if_large(&repo, 4, &ctx).expect("again"),
        "an existing commit-graph is left alone"
    );
}

#[test]
fn add_safe_directory_writes_global_config() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnv::new();
    let dir = env.path("some repo");
    config::add_safe_directory(&env.rt, &dir).expect("safe.directory");
    let value = env.git_global(&["config", "--global", "--get-all", "safe.directory"]);
    assert!(
        value.ends_with("some repo"),
        "forward slashes, added once: {value}"
    );
    assert!(!value.contains('\\'), "no backslashes: {value}");
}
