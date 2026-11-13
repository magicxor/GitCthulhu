//! Credential helper started by `git` as `SSH_ASKPASS`. On Windows it is a GUI-subsystem program, so no console
//! window flashes up for every prompt; it talks to Git through the inherited stdout and to the application through
//! the askpass pipe (the test copy in `gitcthulhu-git` is built the same way and exercised by its askpass tests).
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    gitcthulhu_git::helper_main::askpass_main()
}
