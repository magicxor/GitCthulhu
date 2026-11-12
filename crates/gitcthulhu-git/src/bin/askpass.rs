//! Test copy of the askpass helper (the app ships `gitcthulhu-askpass` from the GUI package).
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    gitcthulhu_git::helper_main::askpass_main()
}
