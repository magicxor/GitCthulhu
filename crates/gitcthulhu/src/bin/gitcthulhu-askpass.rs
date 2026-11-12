//! Credential helper started by `git` (console subsystem; spawned with `CREATE_NO_WINDOW`).

fn main() -> std::process::ExitCode {
    gitcthulhu_git::helper_main::askpass_main()
}
