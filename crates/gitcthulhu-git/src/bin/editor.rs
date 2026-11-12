//! Test copy of the rebase editor helper (the app ships `gitcthulhu-editor` from the GUI package).

fn main() -> std::process::ExitCode {
    gitcthulhu_git::helper_main::editor_main()
}
