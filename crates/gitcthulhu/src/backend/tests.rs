use core::prelude::v1::test;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::{TestAppContext, WeakEntity};

use super::oplog::push_marker;
use super::present::{ErrorPresentation, OpFailure, RecoveryFlow, post_hook_warning, present};
use super::*;
use crate::notifications::ToastKind;
use crate::settings::Settings;
use crate::tab::ops::OpKind;
use crate::test_support::seed::{full, oid};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Spec 21 §0/§2.4: only `src/backend/**` (plus the helper bins, the terminal and the test harness) names the
/// backend crates. Comment lines are not code and are skipped.
#[test]
fn backend_seam_is_respected() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let exempt = ["backend", "bin", "terminal", "test_support"];
    let forbidden = [
        "gitcthulhu_git",
        "gitcthulhu_oplog",
        "gitcthulhu_services",
        "gix::",
        "but_secret",
    ];
    let mut checked = 0;
    for file in files {
        let relative = file.strip_prefix(&root).expect("inside src");
        let first = relative
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        if exempt.contains(&first.as_str()) {
            continue;
        }
        checked += 1;
        let text = fs::read_to_string(&file).expect("read source");
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for token in forbidden {
                assert!(
                    !line.contains(token),
                    "{}:{} names `{token}` outside src/backend (use the crate::backend re-exports): {line}",
                    relative.display(),
                    n + 1
                );
            }
        }
    }
    assert!(
        checked > 50,
        "the seam test saw the source tree ({checked} files)"
    );
}

#[test]
fn runtime_config_maps_proxy_ssl_and_host_keys() {
    let defaults = runtime_config(&Settings::default());
    assert!(
        defaults.network_config.is_empty(),
        "no proxy and SSL verification on by default"
    );
    assert_eq!(defaults.host_key_policy, HostKeyPolicy::AcceptNew);

    let settings = Settings {
        use_proxy: true,
        proxy_host: "proxy.local".into(),
        proxy_port: 3128,
        proxy_auth: true,
        proxy_user: "alice".into(),
        verify_ssl: false,
        ssh_ask_new_hosts: true,
        ..Settings::default()
    };
    let config = runtime_config(&settings);
    assert_eq!(
        config.network_config,
        vec![
            (
                "http.proxy".to_owned(),
                "http://alice@proxy.local:3128".to_owned()
            ),
            ("http.sslVerify".to_owned(), "false".to_owned()),
        ],
        "proxy and TLS settings become -c pairs"
    );
    assert_eq!(config.host_key_policy, HostKeyPolicy::Ask);
}

fn report(statuses: &[PushRefStatus]) -> PushReport {
    PushReport {
        remote: "origin".into(),
        url: None,
        updates: statuses
            .iter()
            .enumerate()
            .map(|(i, status)| PushedRefUpdate {
                local: Some(full(&format!("refs/heads/b{i}"))),
                remote_ref: full(&format!("refs/heads/b{i}")),
                old: None,
                new: Some(oid(i as u64 + 1)),
                status: *status,
                summary: String::new(),
            })
            .collect(),
    }
}

#[test]
fn push_marker_keeps_every_update_but_rejected_ones() {
    let marker = push_marker(
        OperationKind::Push,
        "Push".into(),
        &report(&[
            PushRefStatus::Rejected,
            PushRefStatus::Forced,
            PushRefStatus::Deleted,
        ]),
    )
    .expect("two updates went through");
    assert_eq!(marker.updates.len(), 2, "the rejected ref is not recorded");
    assert!(marker.updates[0].forced, "a forced update is marked forced");
    assert_eq!(
        marker.updates[1].new_target, None,
        "a deletion has no new target"
    );
    assert!(
        push_marker(
            OperationKind::Push,
            "Push".into(),
            &report(&[PushRefStatus::Rejected])
        )
        .is_none(),
        "nothing to record when every ref was rejected"
    );
}

fn git(err: GitError) -> OpFailure {
    OpFailure::Git(Arc::new(err))
}

fn dialog(p: ErrorPresentation) -> super::present::ErrorDialogData {
    match p {
        ErrorPresentation::Dialog(data) => data,
        _ => panic!("expected the error dialog"),
    }
}

/// Spec 21 §2.9: one assertion per row of the `present` table.
#[gpui_kit::test]
fn present_table_rows(cx: &mut TestAppContext) {
    crate::test_support::init_app(cx);
    cx.update(|cx| {
        let tab = WeakEntity::new_invalid();
        let p =
            |f: &OpFailure, cx: &gpui_kit::App| present(OpKind::Commit, f, None, tab.clone(), cx);

        assert!(matches!(
            p(&git(GitError::Cancelled), cx),
            ErrorPresentation::None
        ));
        assert!(matches!(
            p(
                &git(GitError::AuthCancelled {
                    prompt: "Password".into()
                }),
                cx
            ),
            ErrorPresentation::None
        ));
        assert!(matches!(
            p(&OpFailure::Cancelled, cx),
            ErrorPresentation::None
        ));
        match p(&OpFailure::RepoBusy, cx) {
            ErrorPresentation::Toast { kind, text } => {
                assert_eq!(kind, ToastKind::Warning);
                assert_eq!(text.as_ref(), present::REPO_BUSY);
            }
            _ => panic!("RepoBusy is a warning toast"),
        }
        assert!(matches!(
            p(&OpFailure::InterruptedRestore, cx),
            ErrorPresentation::Recovery(RecoveryFlow::OplogInterrupted)
        ));

        let hook = || {
            git(GitError::HookFailed {
                hook: "pre-commit".into(),
                exit_code: Some(1),
                output: "lint failed".into(),
            })
        };
        let data = dialog(p(&hook(), cx));
        assert_eq!(data.title.as_ref(), "Commit failed");
        assert_eq!(
            data.message.as_ref(),
            "The pre-commit hook failed (exit code 1)"
        );
        assert_eq!(
            data.hook.as_ref().map(|h| h.output.as_ref()),
            Some("lint failed"),
            "the hook output is shown"
        );
        assert!(
            data.retry.is_none(),
            "no retry without a retry_without_hooks"
        );
        let with_retry = dialog(present(
            OpKind::Commit,
            &hook(),
            Some(Box::new(|_, _, _| {})),
            tab.clone(),
            cx,
        ));
        assert_eq!(
            with_retry.retry.as_ref().map(|r| r.label.as_ref()),
            Some("Retry without hooks")
        );

        assert!(matches!(
            p(&git(GitError::UnsafeRepository { path: "/r".into() }), cx),
            ErrorPresentation::Recovery(RecoveryFlow::UnsafeRepository { .. })
        ));
        match present(
            OpKind::Merge,
            &git(GitError::Conflicts {
                operation: Some(InProgress::Merge),
                paths: vec![],
                stderr: String::new(),
            }),
            None,
            tab.clone(),
            cx,
        ) {
            ErrorPresentation::Toast { kind, text } => {
                assert_eq!(kind, ToastKind::Warning);
                assert_eq!(text.as_ref(), "Merge stopped with conflicts");
            }
            _ => panic!("conflicts are a warning toast"),
        }
        assert!(matches!(
            p(&git(GitError::StaleDiff { path: "a".into() }), cx),
            ErrorPresentation::Toast {
                kind: ToastKind::Warning,
                ..
            }
        ));

        let paths: Vec<RelaPath> = (0..25)
            .map(|i| RelaPath::from(format!("f{i}.txt")))
            .collect();
        let data = dialog(p(
            &git(GitError::WouldOverwrite {
                kind: OverwriteKind::LocalChanges,
                paths,
                stderr: "error".into(),
            }),
            cx,
        ));
        assert!(
            data.message.contains("f19.txt") && !data.message.contains("f20.txt"),
            "the first 20 paths are listed: {}",
            data.message
        );
        assert!(data.message.ends_with("and 5 more"), "{}", data.message);

        let data = dialog(p(
            &git(GitError::BranchCheckedOutElsewhere {
                branch: "feature".into(),
                worktree: "/wt".into(),
            }),
            cx,
        ));
        assert!(
            data.message
                .contains("'feature' is checked out in another worktree")
        );
        assert!(
            data.extra.is_none(),
            "milestone 1: no open-in-new-tab action"
        );

        let data = dialog(p(
            &git(GitError::IdentityUnknown {
                stderr: String::new(),
            }),
            cx,
        ));
        assert_eq!(
            data.extra.as_ref().map(|e| e.label.as_ref()),
            Some("Set author…")
        );

        let data = dialog(p(
            &git(GitError::AuthFailed {
                url: Some("https://example.com".into()),
                stderr: "denied".into(),
            }),
            cx,
        ));
        assert!(data.message.contains("authentication failed"));

        let data = dialog(p(
            &git(GitError::GitNotFound {
                searched: "git".into(),
            }),
            cx,
        ));
        assert_eq!(
            data.extra.as_ref().map(|e| e.label.as_ref()),
            Some("Open Settings")
        );

        let data = dialog(p(&OpFailure::Other("broken".into()), cx));
        assert_eq!(
            (data.title.as_ref(), data.message.as_ref()),
            ("Commit failed", "broken")
        );

        let data = dialog(p(
            &git(GitError::CommandFailed {
                args: "commit".into(),
                exit_code: Some(1),
                stderr: "fatal: x".into(),
                stdout_tail: String::new(),
            }),
            cx,
        ));
        assert_eq!(data.title.as_ref(), "Commit failed");
        assert_eq!(data.details.as_ref().map(|d| d.as_ref()), Some("fatal: x"));
    });
}

#[test]
fn post_hook_warning_text_and_dialog() {
    let warnings = vec![
        OpWarning::PostHookFailed {
            hook: "post-checkout".into(),
            exit_code: Some(3),
            output: "boom".into(),
        },
        OpWarning::AutostashConflict {
            output: "conflict".into(),
        },
    ];
    let (text, data) =
        post_hook_warning(OpKind::CheckoutBranch, &warnings).expect("a known warning");
    assert_eq!(
        text.as_ref(),
        "Branch checked out - the post-checkout hook failed (exit code 3) (+1 more)"
    );
    assert!(data.warning, "warning mode");
    assert_eq!(data.title.as_ref(), "Hook failed");
    assert_eq!(data.hook.map(|h| h.output.to_string()), Some("boom".into()));
    let (text, _) = post_hook_warning(OpKind::Pull, &warnings[1..]).expect("autostash");
    assert_eq!(
        text.as_ref(),
        "Your local changes conflicted when re-applied"
    );
    assert!(post_hook_warning(OpKind::Pull, &[]).is_none());
}
