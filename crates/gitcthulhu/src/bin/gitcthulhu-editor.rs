//! Editor helper started by `git` for commit messages and rebase todo lists. On Windows it is a GUI-subsystem
//! program, so no console window flashes up for every rebase step; it works on the files Git names on its command
//! line and reports errors through the inherited stderr (`tests/helpers.rs`).
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    gitcthulhu_git::helper_main::editor_main()
}
