//! The helper programs Git starts (`gitcthulhu-askpass`, `gitcthulhu-editor`). On Windows both are GUI-subsystem
//! programs (no console window flashes up per prompt or rebase step); these tests check that they still talk through
//! the pipes Git gives them. This target also makes `cargo test -p gitcthulhu` build both binaries, which the
//! end-to-end tests of `src/integration_tests.rs` hand to Git (`Fixture::runtime_with_helpers`).

use std::process::{Command, Stdio};

const ASKPASS: &str = env!("CARGO_BIN_EXE_gitcthulhu-askpass");
const EDITOR: &str = env!("CARGO_BIN_EXE_gitcthulhu-editor");

#[test]
fn editor_reports_usage_errors_through_piped_stderr() {
    let out = Command::new(EDITOR)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the editor helper starts");
    assert!(!out.status.success(), "no arguments is a usage error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("usage"),
        "a GUI-subsystem helper still writes to the stderr pipe it inherits: {stderr:?}"
    );
}

#[test]
fn editor_message_mode_without_plan_keeps_the_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let plan = dir.path().join("plan");
    std::fs::create_dir_all(&plan).expect("plan dir");
    let msg = dir.path().join("COMMIT_EDITMSG");
    std::fs::write(&msg, "original\n").expect("message file");
    let status = Command::new(EDITOR)
        .args(["message".as_ref(), plan.as_os_str(), msg.as_os_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .status()
        .expect("the editor helper starts");
    assert!(status.success(), "no prepared message is not an error");
    assert_eq!(
        std::fs::read_to_string(&msg).expect("message file"),
        "original\n",
        "Git's own message stays when the plan has none"
    );
}

#[test]
fn askpass_refuses_without_a_session_through_piped_stderr() {
    let out = Command::new(ASKPASS)
        .arg("Password for 'https://example.invalid': ")
        .env_remove("GITCTHULHU_ASKPASS_PIPE")
        .env_remove("GITCTHULHU_ASKPASS_SECRET")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the askpass helper starts");
    assert!(
        !out.status.success(),
        "without an askpass session the helper answers nothing"
    );
    assert!(out.stdout.is_empty(), "no answer reaches Git's stdout pipe");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("GITCTHULHU_ASKPASS_PIPE"),
        "the refusal reaches the stderr pipe of a GUI-subsystem helper: {stderr:?}"
    );
}
