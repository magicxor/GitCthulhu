#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! GitCthulhu GUI entry point.

fn main() {
    gitcthulhu::run()
}
