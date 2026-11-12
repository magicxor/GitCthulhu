//! GitCthulhu: a plain Git GUI client on gpui-kit.
//!
//! The crate layout, state architecture and visual conventions are specified in
//! `GitCthulhu.plan/21-gui-architecture.md`. This file is the only place where `use gpui_kit::*` is allowed
//! (with the `test-support` feature the glob re-exports GPUI's `test` attribute, which breaks `#[gpui_kit::test]`
//! elsewhere); every other module imports explicitly.
// UI callbacks are `Rc<dyn Fn(&mut Window, &mut App)>` by design; aliasing each shape would only add noise.
#![allow(clippy::type_complexity)]

// GPUI's derive macros resolve through a crate named `gpui`; the kit re-exports itself under that name.
extern crate gpui_kit as gpui;

// Allowed here only (see the crate docs); nothing in this file needs it yet, so the lint is silenced for this one
// import instead of crate-wide.
#[allow(unused_imports)]
use gpui_kit::*;

pub mod actions;
pub mod ai;
pub mod app;
pub mod app_view;
pub mod assets;
pub mod backend;
pub mod blame;
pub mod commit_changes;
pub mod dialogs;
pub mod diff;
pub mod file_history;
pub mod fonts;
pub mod forge;
pub mod icons;
pub mod ids;
pub mod log_view;
pub mod logging;
pub mod menus;
pub mod metrics;
pub mod notifications;
pub mod paths;
#[cfg(feature = "qa")]
pub mod qa;
pub mod rebase_interactive;
pub mod registry;
pub mod settings;
pub mod shell;
pub mod side_panel;
pub mod status_pane;
pub mod tab;
pub mod terminal;
pub mod theme;
pub mod toolbar;
pub mod undo;
pub mod update;
pub mod widgets;
pub mod worktrees;

#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod integration_tests_m2;
#[cfg(test)]
mod test_support;

/// Starts the application and blocks until the last window is closed.
pub fn run() {
    app::run()
}

#[cfg(test)]
mod tests;
