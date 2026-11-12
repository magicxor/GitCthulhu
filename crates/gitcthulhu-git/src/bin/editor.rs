//! Test copy of the rebase editor helper (the app ships `gitcthulhu-editor` from the GUI package). Built as a
//! GUI-subsystem program on Windows like the shipped one, so the rebase tests exercise that configuration.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    gitcthulhu_git::helper_main::editor_main()
}
