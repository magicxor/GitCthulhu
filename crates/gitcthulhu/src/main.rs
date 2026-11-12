// A qa build keeps its console: the gate's refusal and panics go to stderr.
#![cfg_attr(
    all(windows, not(debug_assertions), not(feature = "qa")),
    windows_subsystem = "windows"
)]
//! GitCthulhu GUI entry point.

fn main() {
    gitcthulhu::run();
    // A qa run reports its result (pass, failed step, watchdog) through the exit code.
    #[cfg(feature = "qa")]
    std::process::exit(gitcthulhu::qa::exit_code());
}
